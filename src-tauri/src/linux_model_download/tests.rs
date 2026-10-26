use super::*;
use std::{
    io::Read,
    net::{TcpListener, TcpStream},
    thread::{self, JoinHandle},
};

struct Server {
    url: String,
    thread: Option<JoinHandle<String>>,
}
impl Server {
    fn new(handle: impl FnOnce(&mut TcpStream) + Send + 'static) -> Self {
        let mut handle = Some(handle);
        Self::many(1, move |stream, _| handle.take().unwrap()(stream))
    }
    fn many(count: usize, mut handle: impl FnMut(&mut TcpStream, usize) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/model.gguf", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let mut requests = String::new();
            for index in 0..count {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "fixture not contacted");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 16384);
                }
                handle(&mut stream, index);
                requests.push_str(&String::from_utf8(request).unwrap());
            }
            requests
        });
        Self {
            url,
            thread: Some(task),
        }
    }
    fn finish(mut self) -> String {
        self.thread.take().unwrap().join().unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.thread.take() {
            let result = task.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn network() -> NetworkConfig {
    NetworkConfig {
        proxy_mode: "direct".into(),
        proxy_url: String::new(),
    }
}
fn timing() -> Timing {
    Timing {
        overall: Duration::from_secs(4),
        idle: Duration::from_millis(600),
        poll: Duration::from_millis(10),
    }
}
fn saved(dest: &Path, url: &str, bytes: &[u8], total: u64) {
    fs::write(partial_path(dest), bytes).unwrap();
    let meta = ResumeMetadata {
        format: METADATA_FORMAT.into(),
        source: url.into(),
        validator: "\"v1\"".into(),
        total,
    };
    fs::write(metadata_path(dest), serde_json::to_vec(&meta).unwrap()).unwrap();
}
fn download(
    url: &str,
    dest: &Path,
    registry: &DownloadRegistry,
    task: &Arc<DownloadTaskControl>,
    events: &Mutex<Vec<String>>,
) -> Result<u64, String> {
    tauri::async_runtime::block_on(transfer(
        &network(),
        url,
        "SENTINEL_TOKEN",
        dest,
        &Session {
            registry,
            task,
            id: "test",
        },
        &|phase, _, _, _| events.lock().unwrap().push(phase.into()),
        timing(),
    ))
}
fn setup() -> (
    tempfile::TempDir,
    PathBuf,
    DownloadRegistry,
    Arc<DownloadTaskControl>,
    Mutex<Vec<String>>,
) {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("model.gguf");
    let registry = DownloadRegistry::default();
    let task = registry.register("test");
    (temp, dest, registry, task, Mutex::new(Vec::new()))
}
fn assert_closed(stream: &mut TcpStream) {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("client remained active: {other:?}"),
    }
}

#[test]
fn partial_range_resumes_with_validator_and_atomic_publish() {
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\ndef").unwrap();
    });
    let (_temp, dest, registry, task, events) = setup();
    saved(&dest, &server.url, b"abc", 6);
    fs::write(&dest, b"original").unwrap();
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap(),
        6
    );
    assert_eq!(fs::read(&dest).unwrap(), b"abcdef");
    assert!(!partial_path(&dest).exists());
    assert!(!metadata_path(&dest).exists());
    let request = server.finish().to_lowercase();
    assert!(request.contains("range: bytes=3-"));
    assert!(request.contains("if-range: \"v1\""));
    assert!(!request.contains("authorization"));
    assert_eq!(events.lock().unwrap().last().unwrap(), "done");
}

#[test]
fn ignored_range_200_restarts_partial_not_completed_dest() {
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nUVWXYZ").unwrap();
    });
    let (_temp, dest, registry, task, events) = setup();
    saved(&dest, &server.url, b"abc", 6);
    fs::write(&dest, b"old").unwrap();
    download(&server.url, &dest, &registry, &task, &events).unwrap();
    assert_eq!(fs::read(dest).unwrap(), b"UVWXYZ");
    assert!(server.finish().to_lowercase().contains("range: bytes=3-"));
}

#[test]
fn malformed_206_preserves_partial_and_completed_target() {
    for (range, length, etag) in [
        ("bytes 2-5/6", 4, "v1"),
        ("bytes 3-4/6", 2, "v1"),
        ("bytes 3-5/6", 2, "v1"),
        ("bytes 3-5/6", 3, "v2"),
        ("bytes 3-6/7", 4, "v1"),
    ] {
        let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {length}\r\nContent-Range: {range}\r\nETag: \"{etag}\"\r\nConnection: close\r\n\r\nBAD!");
        let server = Server::new(move |s| {
            let _ = s.write_all(header.as_bytes());
        });
        let (_temp, dest, registry, task, events) = setup();
        saved(&dest, &server.url, b"abc", 6);
        fs::write(&dest, b"completed").unwrap();
        assert!(download(&server.url, &dest, &registry, &task, &events).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"completed");
        assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
        assert!(!events.lock().unwrap().iter().any(|e| e == "done"));
        server.finish();
    }
}

#[test]
fn truncated_body_keeps_recoverable_partial_and_existing_dest() {
    let server = Server::new(|s| {
        s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nabc",
        )
        .unwrap();
        thread::sleep(Duration::from_millis(30));
    });
    let (_temp, dest, registry, task, events) = setup();
    fs::write(&dest, b"completed").unwrap();
    assert!(download(&server.url, &dest, &registry, &task, &events).is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"completed");
    assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
    assert!(load_metadata(&metadata_path(&dest)).is_some());
    assert!(!events.lock().unwrap().iter().any(|e| e == "done"));
    server.finish();
}

#[test]
fn pause_body_then_resume_uses_full_filename_partial() {
    let (_temp, dest, registry, task, events) = setup();
    let pause = task.clone();
    let server = Server::many(2, move |s, index| {
        if index == 0 {
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
                .unwrap();
            thread::sleep(Duration::from_millis(100));
            pause.pause.store(true, Ordering::Relaxed);
            assert_closed(s);
        } else {
            s.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\ndef").unwrap();
        }
    });
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap_err(),
        "下载已暂停"
    );
    assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
    assert!(!dest.exists());
    assert!(!dest.with_extension("part").exists());
    assert_eq!(events.lock().unwrap().last().unwrap(), "paused");
    let resumed = registry.register("test");
    assert_eq!(
        download(&server.url, &dest, &registry, &resumed, &events).unwrap(),
        6
    );
    assert_eq!(fs::read(&dest).unwrap(), b"abcdef");
    assert!(!partial_path(&dest).exists());
    let requests = server.finish().to_lowercase();
    assert_eq!(requests.matches("get ").count(), 2);
    assert_eq!(requests.matches("range: bytes=3-").count(), 1);
    assert_eq!(requests.matches("if-range: \"v1\"").count(), 1);
}

#[test]
fn cancel_stalled_headers_and_body_drops_io_before_return() {
    for headers in [false, true] {
        let (_temp, dest, registry, task, events) = setup();
        fs::write(&dest, b"completed").unwrap();
        let cancel = task.clone();
        let server = Server::new(move |s| {
            if headers {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(100));
            cancel.cancel.store(true, Ordering::Relaxed);
            assert_closed(s);
        });
        let started = Instant::now();
        assert_eq!(
            download(&server.url, &dest, &registry, &task, &events).unwrap_err(),
            "下载已取消"
        );
        assert!(started.elapsed() < Duration::from_millis(550));
        assert_eq!(fs::read(&dest).unwrap(), b"completed");
        assert!(!partial_path(&dest).exists());
        assert!(!metadata_path(&dest).exists());
        assert_eq!(events.lock().unwrap().last().unwrap(), "cancelled");
        server.finish();
    }
}

#[test]
fn pause_stalled_headers_drops_request_without_fallback() {
    let (_temp, dest, registry, task, events) = setup();
    let pause = task.clone();
    let server = Server::new(move |s| {
        thread::sleep(Duration::from_millis(100));
        pause.pause.store(true, Ordering::Relaxed);
        assert_closed(s);
    });
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap_err(),
        "下载已暂停"
    );
    assert!(!dest.exists());
    assert_eq!(events.lock().unwrap().last().unwrap(), "paused");
    server.finish();
}

#[test]
fn stale_generation_stops_writer_without_old_events() {
    let (_temp, dest, registry, task, events) = setup();
    let replacement = registry.clone();
    let server = Server::new(move |s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        replacement.register("test");
        assert_closed(s);
    });
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap_err(),
        "下载任务已被接管"
    );
    assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|e| ["done", "paused", "cancelled"].contains(&e.as_str())));
    registry.unregister_if_current("test", &task);
    assert!(registry.slot("test").is_some());
    server.finish();
}

#[test]
fn same_path_conflicts_but_other_paths_remain_parallel() {
    let (_temp, dest, registry, task, events) = setup();
    let lease = TargetLease::acquire(&dest).unwrap();
    let error = download("http://127.0.0.1:1", &dest, &registry, &task, &events).unwrap_err();
    assert!(error.contains("同一目标"));
    let alias = dest.parent().unwrap().join(".").join("model.gguf");
    assert!(TargetLease::acquire(&alias).is_err());
    let other = dest.parent().unwrap().join("other.gguf");
    let other_lease = TargetLease::acquire(&other).unwrap();
    drop(other_lease);
    drop(lease);
    assert!(TargetLease::acquire(&dest).is_ok());
}

#[test]
fn hf_token_host_check_rejects_substrings_and_unrelated_redirects() {
    for url in [
        "https://evil.test/hf-mirror.com/model",
        "https://hf-mirror.com.evil.test/model",
        "https://huggingface.co.evil.test/model",
        "http://huggingface.co/model",
    ] {
        assert!(!hf_host(url));
    }
    assert!(hf_host("https://huggingface.co/a"));
    assert!(hf_host("https://hf-mirror.com/a"));
    let target = Server::new(|s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        target.url
    );
    let source = Server::new(move |s| {
        s.write_all(redirect.as_bytes()).unwrap();
    });
    let (_temp, dest, registry, task, events) = setup();
    download(&source.url, &dest, &registry, &task, &events).unwrap();
    for request in [source.finish(), target.finish()] {
        assert!(!request.to_lowercase().contains("authorization"));
    }
}

#[test]
fn unknown_legacy_and_sidecar_files_are_not_deleted() {
    let (_temp, dest, registry, task, events) = setup();
    fs::write(dest.with_extension("part"), b"user legacy file").unwrap();
    fs::write(metadata_path(&dest), b"user metadata file").unwrap();
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    assert!(download(&server.url, &dest, &registry, &task, &events).is_err());
    assert_eq!(
        fs::read(metadata_path(&dest)).unwrap(),
        b"user metadata file"
    );
    assert_eq!(
        fs::read(dest.with_extension("part")).unwrap(),
        b"user legacy file"
    );
    server.finish();
}

#[test]
fn same_length_dest_is_not_assumed_complete_and_missing_validator_restarts() {
    let (_temp, dest, registry, task, events) = setup();
    fs::write(&dest, b"bad").unwrap();
    fs::write(partial_path(&dest), b"old partial").unwrap();
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    download(&server.url, &dest, &registry, &task, &events).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"abc");
    assert!(!server.finish().to_lowercase().contains("range:"));
}

#[test]
fn http_error_and_missing_length_preserve_finished_file() {
    for status in ["404 Not Found\r\nContent-Length: 0", "200 OK"] {
        let response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n\r\n");
        let server = Server::new(move |s| {
            s.write_all(response.as_bytes()).unwrap();
        });
        let (_temp, dest, registry, task, events) = setup();
        fs::write(&dest, b"completed").unwrap();
        assert!(download(&server.url, &dest, &registry, &task, &events).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"completed");
        assert!(!events.lock().unwrap().iter().any(|e| e == "done"));
        server.finish();
    }
}

#[test]
fn idle_header_and_body_timeouts_leave_no_writer() {
    for headers in [false, true] {
        let (_temp, dest, registry, task, events) = setup();
        let server = Server::new(move |s| {
            if headers {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
                    .unwrap();
            }
            assert_closed(s);
        });
        assert!(download(&server.url, &dest, &registry, &task, &events)
            .unwrap_err()
            .contains("超时"));
        assert!(!dest.exists());
        if headers {
            assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
        }
        assert!(!events.lock().unwrap().iter().any(|e| e == "done"));
        server.finish();
    }
}

#[test]
fn concurrent_target_conflict_and_cancel_are_per_task() {
    let (_temp, dest, registry, task, events) = setup();
    fs::write(&dest, b"completed").unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = Server::new(move |s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
            .unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_closed(s);
    });
    thread::scope(|scope| {
        let writer = scope.spawn(|| download(&server.url, &dest, &registry, &task, &events));
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let second = registry.register("second");
        let result = tauri::async_runtime::block_on(transfer(
            &network(),
            &server.url,
            "",
            &dest,
            &Session {
                registry: &registry,
                task: &second,
                id: "second",
            },
            &|_, _, _, _| {},
            timing(),
        ));
        assert!(result.unwrap_err().contains("同一目标"));
        let other_server = Server::new(|s| {
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxyz")
                .unwrap();
        });
        let other_dest = dest.parent().unwrap().join("other.gguf");
        let other = registry.register("other");
        task.cancel.store(true, Ordering::Relaxed);
        let result = tauri::async_runtime::block_on(transfer(
            &network(),
            &other_server.url,
            "",
            &other_dest,
            &Session {
                registry: &registry,
                task: &other,
                id: "other",
            },
            &|_, _, _, _| {},
            timing(),
        ));
        assert_eq!(result.unwrap(), 3);
        assert_eq!(fs::read(other_dest).unwrap(), b"xyz");
        release_tx.send(()).unwrap();
        assert_eq!(writer.join().unwrap().unwrap_err(), "下载已取消");
        other_server.finish();
    });
    assert_eq!(fs::read(&dest).unwrap(), b"completed");
    assert!(!partial_path(&dest).exists());
    server.finish();
}

#[test]
fn configured_connection_failure_falls_back_to_direct_single_get() {
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
            .unwrap();
    });
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", unused.local_addr().unwrap());
    drop(unused);
    let configured = NetworkConfig {
        proxy_mode: "manual".into(),
        proxy_url: proxy,
    };
    let (_temp, dest, registry, task, _) = setup();
    let result = tauri::async_runtime::block_on(transfer(
        &configured,
        &server.url,
        "",
        &dest,
        &Session {
            registry: &registry,
            task: &task,
            id: "test",
        },
        &|_, _, _, _| {},
        timing(),
    ));
    assert_eq!(result.unwrap(), 3);
    let request = server.finish().to_lowercase();
    assert_eq!(request.matches("get ").count(), 1);
}

#[test]
fn full_length_partial_is_revalidated_not_assumed_complete() {
    let server = Server::new(|s| {
        s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nnew",
        )
        .unwrap();
    });
    let (_temp, dest, registry, task, events) = setup();
    saved(&dest, &server.url, b"old", 3);
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap(),
        3
    );
    assert_eq!(fs::read(dest).unwrap(), b"new");
    assert!(!server.finish().to_lowercase().contains("range:"));
}

#[test]
fn last_modified_if_range_and_chunked_206_enforce_final_length() {
    let server = Server::new(|s| {
        s.write_all(b"HTTP/1.1 206 Partial Content\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 3-5/6\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\nConnection: close\r\n\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
    });
    let (_temp, dest, registry, task, events) = setup();
    fs::write(&dest, b"completed").unwrap();
    saved(&dest, &server.url, b"abc", 6);
    let mut metadata = load_metadata(&metadata_path(&dest)).unwrap();
    metadata.validator = "Wed, 21 Oct 2015 07:28:00 GMT".into();
    fs::write(metadata_path(&dest), serde_json::to_vec(&metadata).unwrap()).unwrap();
    let error = download(&server.url, &dest, &registry, &task, &events).unwrap_err();
    assert!(error.contains("长度"));
    assert_eq!(fs::read(&dest).unwrap(), b"completed");
    assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abcde");
    assert!(!events.lock().unwrap().iter().any(|e| e == "done"));
    assert!(server
        .finish()
        .to_lowercase()
        .contains("if-range: wed, 21 oct 2015 07:28:00 gmt"));
}

#[test]
fn symlink_download_paths_do_not_touch_user_files() {
    for suffix in ["", ".part", ".part.cookllm.json"] {
        let (_temp, dest, registry, task, events) = setup();
        let victim = dest.parent().unwrap().join("user-file");
        fs::write(&victim, b"user contents").unwrap();
        let mut path = dest.as_os_str().to_os_string();
        path.push(suffix);
        std::os::unix::fs::symlink(&victim, PathBuf::from(path)).unwrap();
        assert!(download("http://127.0.0.1:1", &dest, &registry, &task, &events).is_err());
        assert_eq!(fs::read(victim).unwrap(), b"user contents");
    }
}

#[test]
fn cleanup_stopped_paused_partial_removes_owned_sidecar_only() {
    let (_temp, dest, registry, task, events) = setup();
    let root = dest.parent().unwrap();
    fs::write(&dest, b"completed").unwrap();
    let pause = task.clone();
    let server = Server::new(move |stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\n\r\nabc")
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        pause.pause.store(true, Ordering::Relaxed);
        assert_closed(stream);
    });
    assert_eq!(
        download(&server.url, &dest, &registry, &task, &events).unwrap_err(),
        "下载已暂停"
    );
    assert!(metadata_path(&dest).exists());
    let unrelated = root.join("user.json");
    fs::write(&unrelated, b"user file").unwrap();
    remove_local_file(root, &partial_path(&dest)).unwrap();
    assert!(!partial_path(&dest).exists());
    assert!(!metadata_path(&dest).exists());
    assert_eq!(fs::read(&dest).unwrap(), b"completed");
    assert_eq!(fs::read(unrelated).unwrap(), b"user file");
    server.finish();
}

#[test]
fn cleanup_missing_dest_and_partial_still_validates_and_removes_owned_metadata() {
    let (_temp, dest, _, _, _) = setup();
    let root = dest.parent().unwrap();
    saved(&dest, "http://127.0.0.1/model", b"abc", 6);
    fs::remove_file(partial_path(&dest)).unwrap();
    remove_local_file(root, &dest).unwrap();
    assert!(metadata_path(&dest).exists());
    remove_local_file(root, &partial_path(&dest)).unwrap();
    assert!(!metadata_path(&dest).exists());
    remove_local_file(root, &partial_path(&dest)).unwrap();
    assert!(remove_local_file(root, &root.join("missing-parent/model.gguf.part")).is_err());
}

#[test]
fn cleanup_unknown_json_wrong_format_large_files_and_directories_are_preserved() {
    for contents in [
        b"{\"user\":true}".to_vec(),
        b"invalid json".to_vec(),
        vec![b'x'; 16385],
    ] {
        let (_temp, dest, _, _, _) = setup();
        fs::write(partial_path(&dest), b"partial").unwrap();
        fs::write(metadata_path(&dest), &contents).unwrap();
        remove_local_file(dest.parent().unwrap(), &partial_path(&dest)).unwrap();
        assert!(!partial_path(&dest).exists());
        assert_eq!(fs::read(metadata_path(&dest)).unwrap(), contents);
    }
    let (_temp, dest, _, _, _) = setup();
    saved(&dest, "http://127.0.0.1/model", b"abc", 6);
    let mut meta = load_metadata(&metadata_path(&dest)).unwrap();
    meta.format = "other-application-v1".into();
    let contents = serde_json::to_vec(&meta).unwrap();
    fs::write(metadata_path(&dest), &contents).unwrap();
    remove_local_file(dest.parent().unwrap(), &partial_path(&dest)).unwrap();
    assert_eq!(fs::read(metadata_path(&dest)).unwrap(), contents);
    fs::remove_file(metadata_path(&dest)).unwrap();
    fs::create_dir(metadata_path(&dest)).unwrap();
    remove_local_file(dest.parent().unwrap(), &partial_path(&dest)).unwrap();
    assert!(metadata_path(&dest).is_dir());
}

#[test]
fn cleanup_symlinks_and_outside_parents_preserve_every_owned_or_unknown_file() {
    let (_temp, dest, _, _, _) = setup();
    let root = dest.parent().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("user.gguf");
    fs::write(&victim, b"user contents").unwrap();
    for requested in [&dest, &partial_path(&dest)] {
        std::os::unix::fs::symlink(&victim, requested).unwrap();
        assert!(remove_local_file(root, requested).is_err());
        assert!(fs::symlink_metadata(requested)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&victim).unwrap(), b"user contents");
        fs::remove_file(requested).unwrap();
    }
    saved(&dest, "http://127.0.0.1/model", b"abc", 6);
    let own = fs::read(metadata_path(&dest)).unwrap();
    fs::remove_file(metadata_path(&dest)).unwrap();
    let outside_meta = outside.path().join("owned.json");
    fs::write(&outside_meta, &own).unwrap();
    std::os::unix::fs::symlink(&outside_meta, metadata_path(&dest)).unwrap();
    remove_local_file(root, &partial_path(&dest)).unwrap();
    assert!(fs::symlink_metadata(metadata_path(&dest))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&outside_meta).unwrap(), own);
    fs::remove_file(metadata_path(&dest)).unwrap();
    std::os::unix::fs::symlink(outside.path().join("missing"), metadata_path(&dest)).unwrap();
    remove_local_file(root, &partial_path(&dest)).unwrap();
    assert!(fs::symlink_metadata(metadata_path(&dest))
        .unwrap()
        .file_type()
        .is_symlink());
    let outside_dest = outside.path().join("missing.gguf");
    saved(&outside_dest, "http://127.0.0.1/model", b"abc", 6);
    fs::remove_file(partial_path(&outside_dest)).unwrap();
    assert!(remove_local_file(root, &partial_path(&outside_dest)).is_err());
    assert!(metadata_path(&outside_dest).exists());
    let alias = root.join("outside");
    std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
    assert!(remove_local_file(root, &alias.join("missing.gguf.part")).is_err());
    assert!(metadata_path(&outside_dest).exists());
}

#[test]
fn cleanup_active_target_and_containing_directory_fail_but_other_targets_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let directory = root.join("repo");
    fs::create_dir(&directory).unwrap();
    let dest = directory.join("model.gguf");
    let lease = TargetLease::acquire(&dest).unwrap();
    saved(&dest, "http://127.0.0.1/model", b"abc", 6);
    fs::write(&dest, b"completed").unwrap();
    for target in [
        &dest,
        &partial_path(&dest),
        &metadata_path(&dest),
        &directory,
    ] {
        assert!(remove_local_file(root, target)
            .unwrap_err()
            .contains("仍在运行"));
    }
    assert_eq!(fs::read(&dest).unwrap(), b"completed");
    assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
    assert!(metadata_path(&dest).exists());
    let other = root.join("other.gguf");
    saved(&other, "http://127.0.0.1/other", b"xyz", 6);
    remove_local_file(root, &partial_path(&other)).unwrap();
    assert!(!metadata_path(&other).exists());
    drop(lease);
    remove_local_file(root, &partial_path(&dest)).unwrap();
    remove_local_file(root, &dest).unwrap();
    assert!(!dest.exists());
    assert!(!metadata_path(&dest).exists());
}

#[test]
fn cleanup_parallel_frontend_dest_and_partial_requests_both_succeed() {
    let (_temp, dest, _, _, _) = setup();
    let root = dest.parent().unwrap();
    fs::write(&dest, b"completed").unwrap();
    saved(&dest, "http://127.0.0.1/model", b"abc", 6);
    let barrier = std::sync::Barrier::new(2);
    thread::scope(|scope| {
        let main = scope.spawn(|| {
            barrier.wait();
            remove_local_file(root, &dest)
        });
        let part = scope.spawn(|| {
            barrier.wait();
            remove_local_file(root, &partial_path(&dest))
        });
        main.join().unwrap().unwrap();
        part.join().unwrap().unwrap();
    });
    assert!(!dest.exists());
    assert!(!partial_path(&dest).exists());
    assert!(!metadata_path(&dest).exists());
}

#[test]
fn cleanup_explicit_main_deletion_semantics_after_success_and_failure() {
    for success in [true, false] {
        let server = Server::new(move |stream| {
            let response = if success {
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nabc".as_slice()
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nabc".as_slice()
            };
            stream.write_all(response).unwrap();
            thread::sleep(Duration::from_millis(30));
        });
        let (_temp, dest, registry, task, events) = setup();
        let root = dest.parent().unwrap();
        fs::write(&dest, b"original completed model").unwrap();
        let result = download(&server.url, &dest, &registry, &task, &events);
        assert_eq!(result.is_ok(), success);
        if success {
            assert_eq!(fs::read(&dest).unwrap(), b"abc");
        } else {
            assert_eq!(fs::read(&dest).unwrap(), b"original completed model");
        }
        remove_local_file(root, &dest).unwrap();
        assert!(!dest.exists());
        if !success {
            assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
            assert!(metadata_path(&dest).exists());
        }
        remove_local_file(root, &partial_path(&dest)).unwrap();
        assert!(!partial_path(&dest).exists());
        assert!(!metadata_path(&dest).exists());
        server.finish();
    }
}
