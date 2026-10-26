use super::*;

#[path = "linux_system_tests.rs"]
mod system;
use std::{
    os::unix::{fs::{PermissionsExt, symlink}, process::CommandExt},
    sync::atomic::AtomicU64,
    time::{Duration, Instant},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
// Serialize executable writes and fork/exec: another concurrent fork can briefly
// inherit a writer FD before CLOEXEC runs, producing ETXTBSY on a fresh fixture.
static PROCESS_FIXTURE_LOCK: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cookllm-linux-{}-{}", std::process::id(), NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }
    fn sleep_engine(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::copy("/bin/sleep", &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
struct TestChild(Child);
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "fixture readiness timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn engine_permissions_scripts_and_no_probe_execution() {
    let dir = Fixture::new();
    let marker = dir.0.join("must-not-exist");
    let script = dir.file("llama-server.v1", format!("#!/bin/sh\ntouch '{}'\n", marker.display()).as_bytes(), 0o600);
    assert!(platform::validate_engine(&script).unwrap_err().contains("执行权限"));
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(platform::validate_engine(&script).is_ok());
    assert!(!marker.exists());
    let link = dir.0.join("带 空格引擎");
    symlink(&script, &link).unwrap();
    assert!(platform::validate_engine(&link).is_ok());
    assert!(platform::validate_engine(&dir.0).is_err());
    for body in [b"echo no-shebang\n".as_slice(), b"#!/missing/cookllm-interpreter\n", b"#!/bin/sh\r\nexit 0\r\n", b"#!relative-shell\n"] {
        let bad = dir.file("invalid", body, 0o700);
        assert!(platform::validate_engine(&bad).is_err());
    }
}

#[test]
fn elf_architecture_format_and_truncation() {
    let dir = Fixture::new();
    let valid = dir.sleep_engine("llama-server");
    assert!(platform::validate_engine(&valid).is_ok());
    let mut bytes = fs::read(&valid).unwrap();
    bytes[18..20].copy_from_slice(&0xffffu16.to_le_bytes());
    let wrong = dir.file("wrong-arch", &bytes, 0o700);
    assert!(platform::validate_engine(&wrong).unwrap_err().contains("架构不匹配"));
    let truncated = dir.file("truncated", b"\x7fELF", 0o700);
    assert!(platform::validate_engine(&truncated).unwrap_err().contains("截断"));
    let pe = dir.file("windows.exe", b"MZ-not-ELF", 0o700);
    assert!(platform::validate_engine(&pe).is_err());
    assert!(platform::spawn_error(std::io::Error::from_raw_os_error(libc::ENOENT)).contains("加载器"));
}

#[test]
fn recursive_candidates_allow_dots_and_filter_permissions_rpc_and_directories() {
    let dir = Fixture::new();
    fs::create_dir(dir.0.join("llama-server")).unwrap();
    dir.file("llama-server.v1", b"#!/bin/sh\nexit 0\n", 0o700);
    dir.file("llama-server.noexec", b"#!/bin/sh\nexit 0\n", 0o600);
    dir.file("llama-server.exe", b"MZ", 0o700);
    dir.file("ggml-rpc-server.v1", b"#!/bin/sh\nexit 0\n", 0o700);
    dir.file("llama-cli", b"#!/bin/sh\nexit 0\n", 0o700);
    dir.file("llama-server.txt", b"not executable content", 0o700);
    let mut candidates = Vec::new();
    collect_server_candidates(&dir.0, &dir.0, 0, &mut candidates);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "llama-server.v1");
    assert_eq!(find_server_executable(&dir.0, 0), Some(dir.0.join("llama-server.v1")));
    assert!(find_executable(&dir.0, "llama-server", 0).is_none());
    assert!(find_executable(&dir.0, "LLAMA-SERVER.V1", 0).is_none());
    assert_ne!(path_dedupe_key("/models/A.gguf"), path_dedupe_key("/models/a.gguf"));
    assert_ne!(path_dedupe_key("/models/a.gguf "), path_dedupe_key("/models/a.gguf"));
}

#[test]
fn orphan_discovery_is_exact_path_not_basename_or_arguments() {
    let _serial = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let first = Fixture::new();
    let second = Fixture::new();
    let exe = first.sleep_engine("llama-server");
    let other = second.sleep_engine("llama-server");
    let mut a = TestChild(Command::new(&exe).arg("30").spawn().unwrap());
    let mut b = TestChild(Command::new(&other).arg("30").spawn().unwrap());
    let paths = vec![exe.clone()];
    let found = find_orphan_servers(None, &paths);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pid, a.0.id());
    assert!(find_orphan_servers(Some(a.0.id()), &paths).is_empty());
    assert!(find_orphan_servers(None, &[]).is_empty());
    let mut config = AppConfig::default();
    let link = first.0.join("alias with spaces ");
    symlink(&exe, &link).unwrap();
    config.server_path = link.to_string_lossy().into_owned();
    assert_eq!(configured_engine_paths(&config), paths);
    let fd = platform::verified_pidfd(&found[0], &paths, None).unwrap();
    platform::stop_external(&fd, Duration::from_millis(50)).unwrap();
    a.0.wait().unwrap();
    assert!(b.0.try_wait().unwrap().is_none(), "same basename in another path must survive");
}

#[test]
fn orphan_identity_rechecks_uid_start_time_inode_self_and_managed_group() {
    let _serial = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let dir = Fixture::new();
    let exe = dir.sleep_engine("llama-server");
    let child = TestChild(Command::new(&exe).arg("30").process_group(0).spawn().unwrap());
    let identity = platform::process_identity(child.0.id()).unwrap();
    let paths = vec![exe];
    assert!(platform::verified_pidfd(&identity, &paths, Some(child.0.id())).is_err());
    for field in 0..4 {
        let mut changed = identity.clone();
        match field { 0 => changed.started += 1, 1 => changed.uid = changed.uid.wrapping_add(1), 2 => changed.inode += 1, _ => changed.exe = dir.0.join("other") }
        assert!(platform::verified_pidfd(&changed, &paths, None).is_err());
    }
    let own = platform::process_identity(std::process::id()).unwrap();
    assert!(!platform::matches_engine(&own, &[own.exe.clone()], None));
    let mut descendant = identity.clone();
    descendant.pid += 1;
    assert!(!platform::matches_engine(&descendant, &paths, Some(child.0.id())));
}

#[test]
fn external_pid_cleanup_does_not_signal_group_peers() {
    let _serial = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let dir = Fixture::new();
    let exe = dir.sleep_engine("llama-server");
    let mut leader = TestChild(Command::new(&exe).arg("30").process_group(0).spawn().unwrap());
    let mut peer = TestChild(Command::new(&exe).arg("30").process_group(leader.0.id() as i32).spawn().unwrap());
    let identity = platform::process_identity(leader.0.id()).unwrap();
    let fd = platform::verified_pidfd(&identity, &[exe], None).unwrap();
    platform::stop_external(&fd, Duration::from_millis(50)).unwrap();
    leader.0.wait().unwrap();
    assert!(peer.0.try_wait().unwrap().is_none());
}

fn run_process_fixture(mode: &str) {
    let _serial = PROCESS_FIXTURE_LOCK.lock().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "linux_port_tests::process_fixture_worker", "--nocapture"])
        .env("COOKLLM_PROCESS_FIXTURE", mode).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
#[test]
fn managed_group_graceful_shutdown() { run_process_fixture("graceful"); }
#[test]
fn managed_group_sigterm_timeout_forces_group_exit() { run_process_fixture("timeout"); }
#[test]
fn managed_group_cleans_descendants_after_parent_exit() { run_process_fixture("parent-exit"); }

// Run subreaper behavior in a separate harness process; never change the main test runner.
#[test]
fn process_fixture_worker() {
    let Ok(mode) = std::env::var("COOKLLM_PROCESS_FIXTURE") else { return; };
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }, 0);
    let dir = Fixture::new();
    let ready = dir.0.join("ready");
    let stopped = dir.0.join("stopped");
    let script = if mode == "graceful" {
        "trap 'echo stopped > \"$2\"; exit 0' TERM; echo ready > \"$1\"; while :; do sleep 0.02; done"
    } else if mode == "parent-exit" {
        "trap '' TERM; sleep 30 & echo $! > \"$1\"; exit 0"
    } else {
        "trap '' TERM; sleep 30 & echo $! > \"$1\"; wait"
    };
    let child = Command::new("/bin/sh").args(["-c", script, "fixture"])
        .arg(&ready).arg(&stopped).process_group(0).spawn().unwrap();
    let mut managed = ManagedProcess { child, status: ServerStatus::default() };
    let pgid = managed.child.id() as i32;
    wait_until(|| ready.exists() && fs::metadata(&ready).unwrap().len() > 0);
    if mode == "parent-exit" {
        wait_until(|| managed_child_exited(&mut managed).unwrap());
        // Repeated status polling leaves the zombie leader pinned for group cleanup.
        assert!(managed_child_exited(&mut managed).unwrap());
        assert!(Path::new(&format!("/proc/{pgid}")).exists());
    }
    let start = Instant::now();
    let result = kill_managed_child(&mut managed);
    if result.is_err() {
        // Fixture-only fallback, group ID still belongs to our unreaped child.
        unsafe { libc::kill(-pgid, libc::SIGKILL); }
        let _ = managed.child.wait();
    }
    result.unwrap();
    assert!(start.elapsed() < Duration::from_secs(4));
    if mode == "timeout" { assert!(start.elapsed() >= Duration::from_millis(750)); }
    if mode == "graceful" { assert!(stopped.exists()); }
    assert!(managed.child.try_wait().unwrap().is_some());
    // Reap only this fixture's adopted group children, not unrelated children.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-pgid, &mut status, libc::WNOHANG) };
        if pid < 0 { break; }
        assert!(Instant::now() < deadline, "fixture descendants not reaped");
        if pid == 0 { std::thread::sleep(Duration::from_millis(10)); }
    }
    assert_eq!(unsafe { libc::kill(-pgid, 0) }, -1);
}
