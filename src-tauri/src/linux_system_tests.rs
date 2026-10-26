//! Local-only system interface fixtures: no desktop clipboard or user configuration.
use super::*;
use std::{io::{Read, Write}, net::{TcpListener, TcpStream}, thread};

#[test]
fn path_lookup_requires_executable_and_preserves_names() {
    let dir = Fixture::new();
    let tool = dir.file("nvidia-smi", b"#!/bin/sh\nexit 0\n", 0o600);
    assert!(platform::find_in_path("nvidia-smi", Some(dir.0.as_os_str())).is_none());
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(platform::find_in_path("nvidia-smi", Some(dir.0.as_os_str())), Some(tool));
    assert!(platform::find_in_path("absent", Some(dir.0.as_os_str())).is_none());
    assert!(platform::find_in_path("nvidia-smi", None).is_none());
    let second = dir.0.join("带 空格");
    fs::create_dir(&second).unwrap();
    let paths = std::env::join_paths([&second, &dir.0]).unwrap();
    assert!(platform::find_in_path("nvidia-smi", Some(&paths)).is_some());
}

#[test]
fn memory_parsing_and_disk_missing_are_not_zero_measurements() {
    assert_eq!(platform::memory_from_meminfo("MemFree: 4 kB\nMemTotal: 8388608 kB\n"), Some(8192.0));
    for value in ["", "MemTotal: 0 kB", "MemTotal: nope kB", "MemTotal: 4 MB", "MemTotal: -1 kB"] {
        assert_eq!(platform::memory_from_meminfo(value), None);
    }
    let dir = Fixture::new();
    let (total, free) = free_disk_space(&dir.0).unwrap();
    assert!(total > 0 && free <= total);
    assert!(free_disk_space(&dir.0.join("missing")).is_err());
}

#[test]
fn vulkan_requires_a_hardware_device_not_loader_or_cpu() {
    assert!(platform::vulkan_device("Vulkan Instance Version: 1.4\nlibvulkan.so\n").is_none());
    assert!(platform::vulkan_device("GPU0:\n deviceType = PHYSICAL_DEVICE_TYPE_CPU\n deviceName = llvmpipe\n").is_none());
    assert_eq!(platform::vulkan_device("GPU0:\n deviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n deviceName = Test GPU\n"), Some("Test GPU".into()));
    assert!(platform::vulkan_device("GPU0:\n deviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\nGPU1:\n deviceName = unknown\n").is_none());
}

#[test]
fn query_gpu_fixture_valid_partial_missing_failure_and_timeout() {
    let _lock = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let dir = Fixture::new();
    let good = dir.file("smi-good", b"#!/bin/sh\nprintf '16384, 123, 8, N/A\n999, 999, 99, 99\n'\n", 0o700);
    let stats = query_gpu_stats_at(&good).unwrap();
    assert_eq!(stats.memory_total_mb, Some(16384.0));
    assert_eq!(stats.memory_used_mb, Some(123.0));
    assert_eq!(stats.util_percent, Some(8.0));
    assert_eq!(stats.power_watts, None);
    let invalid = dir.file("smi-invalid", b"#!/bin/sh\nprintf 'NaN, -1, inf, N/A\n'\n", 0o700);
    let stats = query_gpu_stats_at(&invalid).unwrap();
    assert!(stats.memory_total_mb.is_none() && stats.memory_used_mb.is_none() && stats.util_percent.is_none());
    let failed = dir.file("smi-fail", b"#!/bin/sh\necho 'No devices' >&2\nexit 9\n", 0o700);
    assert!(query_gpu_stats_at(&failed).is_none());
    let empty = dir.file("smi-empty", b"#!/bin/sh\nexit 0\n", 0o700);
    assert!(query_gpu_stats_at(&empty).is_none());
    assert!(query_gpu_stats_at(&dir.0.join("absent")).is_none());
    let hung = dir.file("smi-hung", b"#!/bin/sh\nexec /bin/sleep 30\n", 0o700);
    let start = Instant::now();
    assert!(query_gpu_stats_at(&hung).is_none());
    assert!(start.elapsed() < Duration::from_secs(4));
}

#[test]
fn busy_guard_releases_on_early_return_and_unwind() {
    let busy = AtomicBool::new(false);
    {
        let _guard = BusyGuard::acquire(&busy).unwrap();
        assert!(BusyGuard::acquire(&busy).is_none());
    }
    assert!(!busy.load(Ordering::Acquire));
    let _ = std::panic::catch_unwind(|| {
        let _guard = BusyGuard::acquire(&busy).unwrap();
        panic!("fixture unwind");
    });
    assert!(BusyGuard::acquire(&busy).is_some());
}

#[test]
fn command_deadline_covers_full_stdin_and_reports_stderr() {
    let _lock = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let error = platform::checked_tool(Command::new("/bin/sh").args(["-c", "echo fixture-error >&2; exit 7"]), None, Duration::from_secs(1)).unwrap_err();
    assert!(error.contains("7") && error.contains("fixture-error"));
    let start = Instant::now();
    let error = platform::checked_tool(Command::new("/bin/sleep").arg("30"), Some(&vec![b'x'; 1024 * 1024]), Duration::from_millis(80)).unwrap_err();
    assert!(error.contains("超时"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn clipboard_fixture_checks_arguments_payload_and_fallback() {
    let _lock = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let dir = Fixture::new();
    // All candidate executables come from this fixture-only PATH.
    dir.file("wl-copy", "#!/bin/sh\n[ \"$1\" = --type ] && [ \"$2\" = 'text/plain;charset=utf-8' ] || exit 9\ntext=$(/bin/cat)\n[ \"$text\" = '中文 $HOME --literal' ] || exit 8\n".as_bytes(), 0o700);
    platform::write_clipboard("中文 $HOME --literal", Some(dir.0.as_os_str()), true, true).unwrap();
    dir.file("wl-copy", b"#!/bin/sh\necho fixture-wl-error >&2\nexit 7\n", 0o700);
    dir.file("xclip", b"#!/bin/sh\n[ \"$1 $2 $3\" = '-selection clipboard -in' ] || exit 9\n/bin/cat >/dev/null\n", 0o700);
    platform::write_clipboard("fallback", Some(dir.0.as_os_str()), true, true).unwrap();
    fs::remove_file(dir.0.join("xclip")).unwrap();
    let error = platform::write_clipboard("test", Some(dir.0.as_os_str()), true, false).unwrap_err();
    assert!(error.contains("fixture-wl-error") || error.contains("接收完整输入"));
    dir.file("xsel", b"#!/bin/sh\n[ \"$1 $2\" = '--clipboard --input' ] || exit 9\n/bin/cat >/dev/null\n", 0o700);
    platform::write_clipboard("xsel", Some(dir.0.as_os_str()), false, true).unwrap();
    assert!(platform::write_clipboard("no desktop", Some(dir.0.as_os_str()), false, false).is_err());
    assert_eq!(platform::clipboard_commands(true, true).iter().map(|c| c.0).collect::<Vec<_>>(), vec!["wl-copy", "xclip", "xsel"]);
}

#[test]
fn reveal_uses_parent_and_preserves_whitespace_with_fixture_openers() {
    let _lock = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let dir = Fixture::new();
    let folder = dir.0.join(" 中文 空格 ");
    fs::create_dir(&folder).unwrap();
    assert_eq!(platform::reveal_directory(&folder).unwrap(), folder);
    assert_eq!(platform::reveal_directory(&folder.join("missing.gguf")).unwrap(), folder);
    dir.file("gio", b"#!/bin/sh\nexit 2\n", 0o700);
    dir.file("xdg-open", b"#!/bin/sh\ncase \"$1\" in file://*%20/) exit 0;; *) exit 9;; esac\n", 0o700);
    platform::reveal_path(&folder.join("missing.gguf"), Some(dir.0.as_os_str())).unwrap();
    assert!(platform::reveal_path(&folder, None).is_err());
}

fn proxy_env(entries: &[(&str, &str)]) -> platform::ProxyEnvironment {
    platform::ProxyEnvironment::read(|key| entries.iter().find(|e| e.0 == key).map(|e| e.1.to_string()))
}

#[test]
fn proxy_environment_precedence_protocols_display_and_empty() {
    let env = proxy_env(&[("http_proxy", "http://low:81"), ("HTTP_PROXY", "http://upper:82"), ("HTTPS_PROXY", "https://tls:83"), ("ALL_PROXY", "socks5h://all:84"), ("NO_PROXY", "localhost,.example.org")]);
    assert_eq!(env.for_scheme("http"), Some("http://low:81"));
    assert_eq!(env.for_scheme("https"), Some("https://tls:83"));
    assert_eq!(env.for_scheme("ftp"), None);
    let all = proxy_env(&[("ALL_PROXY", "socks5h://user:fixture-password@localhost:1080"), ("NO_PROXY", "localhost")]);
    assert_eq!(all.for_scheme("https"), all.for_scheme("http"));
    let display = all.display().unwrap();
    assert!(display.contains("ALL_PROXY=") && display.contains("NO_PROXY=localhost"));
    assert!(!display.contains("fixture-password") && !display.contains("user"));
    assert!(proxy_env(&[]).display().is_none());
    assert_eq!(proxy_env(&[("http_proxy", ""), ("HTTP_PROXY", "http://ignored:80")]).for_scheme("http"), None);
}

fn accept_fixture(listener: TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match listener.accept() {
            Ok((stream, _)) => { stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap(); return stream; }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "fixture was not contacted");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("fixture accept: {e}"),
        }
    }
}
fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if stream.read(&mut byte).unwrap_or(0) == 0 { break; }
        data.push(byte[0]);
        assert!(data.len() < 16384);
    }
    String::from_utf8(data).unwrap()
}
fn http_fixture(delay: Duration) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let mut stream = accept_fixture(listener);
        let request = read_request(&mut stream);
        thread::sleep(delay);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        request
    });
    (address, task)
}
fn client(mode: &str, url: &str, env: &platform::ProxyEnvironment) -> reqwest::blocking::Client {
    build_net_client_with_environment(&NetworkConfig { proxy_mode: mode.into(), proxy_url: url.into() }, env).unwrap()
}

#[test]
fn proxy_system_http_and_all_fallback_use_local_proxy() {
    for key in ["HTTP_PROXY", "ALL_PROXY"] {
        let (address, task) = http_fixture(Duration::ZERO);
        let env = proxy_env(&[(key, &address)]);
        let response = client("system", "", &env).get("http://fixture.invalid/model").timeout(Duration::from_secs(2)).send().unwrap();
        assert!(response.status().is_success());
        let request = task.join().unwrap();
        assert!(request.starts_with("GET http://fixture.invalid/model HTTP/1.1"));
        assert!(!request.to_lowercase().contains("authorization"));
    }
}

#[test]
fn proxy_system_https_uses_connect_not_http_proxy() {
    let (address, task) = http_fixture(Duration::ZERO);
    let env = proxy_env(&[("HTTPS_PROXY", &address), ("HTTP_PROXY", "http://127.0.0.1:1")]);
    // The fixture only records CONNECT; it intentionally has no TLS endpoint.
    assert!(client("system", "", &env).get("https://fixture.invalid/metadata").timeout(Duration::from_secs(2)).send().is_err());
    assert!(task.join().unwrap().starts_with("CONNECT fixture.invalid:443 HTTP/1.1"));
}

#[test]
fn proxy_direct_and_no_proxy_bypass_environment() {
    for (mode, bypass) in [("direct", ""), ("system", "127.0.0.1"), ("system", "*")] {
        let (address, task) = http_fixture(Duration::ZERO);
        let env = proxy_env(&[("HTTP_PROXY", "http://127.0.0.1:1"), ("ALL_PROXY", "http://127.0.0.1:1"), ("NO_PROXY", bypass)]);
        let response = client(mode, "", &env).get(format!("{address}/direct")).timeout(Duration::from_secs(2)).send()
            .unwrap_or_else(|e| panic!("mode={mode} bypass={bypass}: {e:?}"));
        assert!(response.status().is_success());
        assert!(task.join().unwrap().starts_with("GET /direct HTTP/1.1"));
    }
}

#[test]
fn proxy_manual_overrides_environment_and_timeout_is_bounded() {
    let (address, task) = http_fixture(Duration::from_millis(300));
    let env = proxy_env(&[("HTTP_PROXY", "http://127.0.0.1:1"), ("NO_PROXY", "*")]);
    let client = client("manual", &address, &env);
    let start = Instant::now();
    let result = test_proxy_with_client(&client, "http://fixture.invalid/probe", Duration::from_millis(80)).unwrap();
    assert!(!result.ok && result.status == "ERR");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(task.join().unwrap().starts_with("GET http://fixture.invalid/probe HTTP/1.1"));
}

#[test]
fn socks5h_feature_resolves_hostname_at_fixture_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("socks5h://{}", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let mut stream = accept_fixture(listener);
        let mut header = [0; 2]; stream.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 5);
        let mut methods = vec![0; header[1] as usize]; stream.read_exact(&mut methods).unwrap();
        stream.write_all(&[5, 0]).unwrap();
        let mut request = [0; 5]; stream.read_exact(&mut request).unwrap();
        assert_eq!(&request[..4], &[5, 1, 0, 3]);
        let mut host = vec![0; request[4] as usize]; stream.read_exact(&mut host).unwrap();
        assert_eq!(host, b"fixture.invalid");
        let mut port = [0; 2]; stream.read_exact(&mut port).unwrap();
        assert_eq!(u16::from_be_bytes(port), 80);
        stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80]).unwrap();
        let request = read_request(&mut stream);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        request
    });
    let response = client("manual", &address, &proxy_env(&[])).get("http://fixture.invalid/socks").timeout(Duration::from_secs(2)).send().unwrap();
    assert!(response.status().is_success());
    assert!(task.join().unwrap().starts_with("GET /socks HTTP/1.1"));
}

#[test]
fn tray_default_explicit_preference_and_install_gate() {
    assert!(!should_hide_on_close(None, true));
    assert!(should_hide_on_close(Some(true), true));
    assert!(!should_hide_on_close(Some(false), true));
    assert!(!should_hide_on_close(Some(true), false));
    let config: AppConfig = serde_json::from_value(serde_json::to_value(AppConfig::default()).unwrap()).unwrap();
    assert_eq!(config.minimize_to_tray_on_close, None);
    assert!(automatic_install_gate().unwrap_err().contains("尚未开放"));
}
