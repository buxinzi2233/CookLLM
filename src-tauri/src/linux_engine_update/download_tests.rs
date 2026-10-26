use super::*;
use std::{
    cell::Cell,
    net::{TcpListener, TcpStream},
    thread::{self, JoinHandle},
};

// Every fixture owns and joins its server thread, including panic paths. No detached IO.
struct Server {
    url: String,
    task: Option<JoinHandle<String>>,
}
impl Server {
    fn new(handle: impl FnOnce(&mut TcpStream) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture was not contacted");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_request(&mut stream);
            handle(&mut stream);
            request
        });
        Self {
            url,
            task: Some(task),
        }
    }
    fn finish(mut self) -> String {
        self.task.take().unwrap().join().unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        assert!(request.len() <= 16384);
    }
    String::from_utf8(request).unwrap()
}
fn closed_by_client(stream: &mut TcpStream) {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("download connection remained active: {other:?}"),
    }
}
fn test_timing() -> DownloadTiming {
    DownloadTiming {
        overall: Duration::from_secs(4),
        idle: Duration::from_millis(400),
        poll: Duration::from_millis(10),
    }
}
fn environment(entries: &[(&str, &str)]) -> platform::ProxyEnvironment {
    platform::ProxyEnvironment::read(|key| {
        entries.iter().find(|e| e.0 == key).map(|e| e.1.to_string())
    })
}
fn client(mode: &str, proxy: &str, env: &platform::ProxyEnvironment) -> reqwest::Client {
    download_client(
        &NetworkConfig {
            proxy_mode: mode.into(),
            proxy_url: proxy.into(),
        },
        env,
    )
    .unwrap()
}
fn direct_client() -> reqwest::Client {
    client("direct", "", &environment(&[]))
}
fn local_url(url: &reqwest::Url) -> bool {
    url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
}
fn stream_to(
    url: &str,
    size: u64,
    path: &Path,
    cancel: &impl Fn() -> bool,
    progress: impl Fn(u64, u64),
    timing: DownloadTiming,
) -> Result<(), String> {
    tauri::async_runtime::block_on(async {
        download_stream(
            direct_client().get(url),
            size,
            path,
            cancel,
            progress,
            timing,
            local_url,
        )
        .await
    })
}

#[test]
fn shared_app_transport_stays_512mib_while_runtime_has_separate_limit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let error = stream_to(
        "http://127.0.0.1:1",
        MAX_ARCHIVE + 1,
        &path,
        &|| true,
        |_, _| {},
        test_timing(),
    )
    .unwrap_err();
    assert!(error.contains("大小超出上限"));
    let error = tauri::async_runtime::block_on(download_stream_limited(
        direct_client().get("http://127.0.0.1:1"),
        MAX_ARCHIVE + 1,
        cuda::MAX_RUNTIME_ARCHIVE,
        &path,
        &|| true,
        |_, _| {},
        test_timing(),
        local_url,
    ))
    .unwrap_err();
    assert!(error.contains("取消"));
    let error = tauri::async_runtime::block_on(download_stream_limited(
        direct_client().get("http://127.0.0.1:1"),
        cuda::MAX_RUNTIME_ARCHIVE + 1,
        cuda::MAX_RUNTIME_ARCHIVE,
        &path,
        &|| true,
        |_, _| {},
        test_timing(),
        local_url,
    ))
    .unwrap_err();
    assert!(error.contains("大小超出上限"));
    assert!(!path.exists());
}

#[test]
fn sustained_slow_progress_outlives_idle_limit_without_timeout() {
    let server = Server::new(|stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n")
            .unwrap();
        for _ in 0..12 {
            stream.write_all(b"a").unwrap();
            thread::sleep(Duration::from_millis(70));
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let calls = Cell::new(0);
    let start = Instant::now();
    stream_to(
        &server.url,
        12,
        &path,
        &|| false,
        |_, _| calls.set(calls.get() + 1),
        test_timing(),
    )
    .unwrap();
    assert!(start.elapsed() > test_timing().idle);
    assert!(calls.get() > 2);
    assert_eq!(fs::read(&path).unwrap(), vec![b'a'; 12]);
    server.finish();
}

#[test]
fn stalled_body_cancel_returns_promptly_and_never_writes_after_return() {
    let server = Server::new(|stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc")
            .unwrap();
        // This attempts to deliver more data after the caller has already cancelled.
        thread::sleep(Duration::from_millis(400));
        let _ = stream.write_all(b"later");
        closed_by_client(stream);
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let first_progress = Cell::new(None::<Instant>);
    let progress_calls = Cell::new(0);
    let cancel = || {
        first_progress
            .get()
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(80))
    };
    let result = stream_to(
        &server.url,
        8,
        &path,
        &cancel,
        |_, _| {
            progress_calls.set(progress_calls.get() + 1);
            first_progress.set(Some(Instant::now()));
        },
        DownloadTiming {
            idle: Duration::from_secs(2),
            ..test_timing()
        },
    );
    assert!(result.unwrap_err().contains("取消"));
    assert!(first_progress.get().unwrap().elapsed() < Duration::from_millis(600));
    let before = fs::read(&path).unwrap();
    let before_calls = progress_calls.get();
    server.finish();
    assert_eq!(before, b"abc");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(progress_calls.get(), before_calls);
}

#[test]
fn cancelling_while_waiting_headers_drops_the_request() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let server = Server::new(move |stream| {
        thread::sleep(Duration::from_millis(80));
        flag.store(true, Ordering::Relaxed);
        closed_by_client(stream);
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let start = Instant::now();
    let error = stream_to(
        &server.url,
        3,
        &path,
        &|| cancelled.load(Ordering::Relaxed),
        |_, _| {},
        test_timing(),
    )
    .unwrap_err();
    assert!(error.contains("取消"));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!path.exists());
    server.finish();
}

#[test]
fn cancelling_pending_proxy_connection_drops_connect_future() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let server = Server::new(move |stream| {
        // CONNECT has arrived; reqwest is still establishing its HTTPS connection.
        thread::sleep(Duration::from_millis(80));
        flag.store(true, Ordering::Relaxed);
        closed_by_client(stream);
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let start = Instant::now();
    let error = tauri::async_runtime::block_on(async {
        let client = client("manual", &server.url, &environment(&[]));
        download_stream(
            client.get("https://github.com/fixture"),
            3,
            &path,
            &|| cancelled.load(Ordering::Relaxed),
            |_, _| {},
            test_timing(),
            |url| trusted_response(url, false),
        )
        .await
    })
    .unwrap_err();
    assert!(error.contains("取消"));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!path.exists());
    assert!(server
        .finish()
        .starts_with("CONNECT github.com:443 HTTP/1.1"));
}

#[test]
fn idle_timeout_drops_stalled_body_and_keeps_only_received_bytes() {
    let server = Server::new(|stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc")
            .unwrap();
        closed_by_client(stream);
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let start = Instant::now();
    let error = stream_to(&server.url, 8, &path, &|| false, |_, _| {}, test_timing()).unwrap_err();
    assert!(error.contains("无进度"));
    assert!(start.elapsed() >= test_timing().idle && start.elapsed() < Duration::from_secs(2));
    server.finish();
    assert_eq!(fs::read(&path).unwrap(), b"abc");
}

#[test]
fn overall_deadline_applies_even_with_continuous_progress() {
    let server = Server::new(|stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\nConnection: close\r\n\r\n")
            .unwrap();
        for _ in 0..40 {
            if stream.write_all(b"a").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let calls = Cell::new(0);
    let error = stream_to(
        &server.url,
        40,
        &path,
        &|| false,
        |_, _| calls.set(calls.get() + 1),
        DownloadTiming {
            overall: Duration::from_millis(300),
            idle: Duration::from_secs(2),
            ..test_timing()
        },
    )
    .unwrap_err();
    assert!(error.contains("总时间"));
    assert!(calls.get() > 1);
    let before = fs::read(&path).unwrap();
    server.finish();
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn http_status_length_mismatch_truncation_and_excess_fail() {
    let cases: &[(&[u8], &str, bool)] = &[
        (
            b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "503",
            false,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
            "长度",
            false,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
            "下载失败",
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nab",
            "不完整",
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabcd",
            "超过",
            true,
        ),
    ];
    for &(response, message, created) in cases {
        let server = Server::new(move |stream| {
            let _ = stream.write_all(response);
        });
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("download");
        let error =
            stream_to(&server.url, 3, &path, &|| false, |_, _| {}, test_timing()).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert_eq!(path.exists(), created);
        server.finish();
    }
}

#[test]
fn production_url_validation_still_excludes_local_fixture_origin() {
    let server = Server::new(|stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("download");
    let error = tauri::async_runtime::block_on(async {
        download_stream(
            direct_client().get(&server.url),
            3,
            &path,
            &|| false,
            |_, _| {},
            test_timing(),
            |url| trusted_response(url, false),
        )
        .await
    })
    .unwrap_err();
    assert!(error.contains("非受信"));
    assert!(!path.exists());
    server.finish();
}

#[test]
fn async_client_keeps_system_all_manual_direct_and_no_proxy_semantics() {
    for (mode, key) in [
        ("system", "HTTP_PROXY"),
        ("system", "ALL_PROXY"),
        ("manual", "HTTP_PROXY"),
    ] {
        let server = Server::new(|stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let env = if mode == "manual" {
            environment(&[("HTTP_PROXY", "http://127.0.0.1:1"), ("NO_PROXY", "*")])
        } else {
            environment(&[(key, &server.url)])
        };
        tauri::async_runtime::block_on(async {
            client(mode, &server.url, &env)
                .get("http://fixture.invalid/engine")
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        });
        assert!(server
            .finish()
            .starts_with("GET http://fixture.invalid/engine HTTP/1.1"));
    }
    for (mode, no_proxy) in [
        ("direct", ""),
        ("system", "127.0.0.1"),
        ("system", "*"),
        ("system", "localhost"),
    ] {
        let server = Server::new(|stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let env = environment(&[
            ("HTTP_PROXY", "http://127.0.0.1:1"),
            ("ALL_PROXY", "http://127.0.0.1:1"),
            ("NO_PROXY", no_proxy),
        ]);
        let url = if no_proxy == "localhost" {
            server.url.replace("127.0.0.1", "localhost")
        } else {
            server.url.clone()
        };
        tauri::async_runtime::block_on(async {
            client(mode, "", &env)
                .get(format!("{url}/direct"))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        });
        assert!(server.finish().starts_with("GET /direct HTTP/1.1"));
    }
}

#[test]
fn async_client_https_proxy_uses_connect_and_protocol_precedence() {
    let server = Server::new(|stream| {
        stream
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
    });
    let env = environment(&[
        ("HTTPS_PROXY", &server.url),
        ("HTTP_PROXY", "http://127.0.0.1:1"),
        ("ALL_PROXY", "http://127.0.0.1:1"),
    ]);
    tauri::async_runtime::block_on(async {
        assert!(client("system", "", &env)
            .get("https://fixture.invalid/engine")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err());
    });
    assert!(server
        .finish()
        .starts_with("CONNECT fixture.invalid:443 HTTP/1.1"));
}

#[test]
fn async_client_socks5h_keeps_remote_dns_resolution() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("socks5h://{}", listener.local_addr().unwrap());
    // Reuse Server's joined ownership even though SOCKS has a binary preamble.
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut stream = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut header = [0; 2];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 5);
        let mut methods = vec![0; header[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        stream.write_all(&[5, 0]).unwrap();
        let mut request = [0; 5];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request[..4], &[5, 1, 0, 3]);
        let mut host = vec![0; request[4] as usize];
        stream.read_exact(&mut host).unwrap();
        assert_eq!(host, b"fixture.invalid");
        let mut port = [0; 2];
        stream.read_exact(&mut port).unwrap();
        assert_eq!(u16::from_be_bytes(port), 80);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .unwrap();
        let request = read_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        request
    });
    let server = Server {
        url: proxy,
        task: Some(task),
    };
    tauri::async_runtime::block_on(async {
        client("manual", &server.url, &environment(&[]))
            .get("http://fixture.invalid/socks")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    assert!(server.finish().starts_with("GET /socks HTTP/1.1"));
}
