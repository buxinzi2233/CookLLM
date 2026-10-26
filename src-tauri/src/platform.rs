//! Linux engine validation and process identity. Never execute a file to inspect it.
use std::{
    ffi::CString,
    fs,
    io::{self, Read},
    os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::{ffi::OsStrExt, fs::MetadataExt}},
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

pub(crate) fn executable_file(path: &Path) -> bool {
    if !fs::metadata(path).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0) {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else { return false; };
    // Check the effective user's access, including ACLs, without changing file permissions.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

pub(crate) fn validate_engine(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("引擎路径不是普通文件：{}", path.display()));
    }
    if !executable_file(path) {
        return Err(format!("引擎缺少执行权限，请检查文件权限和挂载选项：{}", path.display()));
    }
    let mut header = [0u8; 256];
    let mut file = fs::File::open(path).map_err(|e| format!("读取引擎失败：{e}"))?;
    let n = file.read(&mut header).map_err(|e| format!("读取引擎头失败：{e}"))?;
    let bytes = &header[..n];
    if bytes.starts_with(b"#!") {
        let end = bytes.iter().position(|b| *b == b'\n').unwrap_or(n);
        if end == n && n == header.len() {
            return Err("脚本解释器行过长，请使用简短的绝对解释器路径".into());
        }
        let line = std::str::from_utf8(&bytes[2..end]).map_err(|_| "脚本解释器行不是有效 UTF-8")?;
        if line.contains('\r') {
            return Err("脚本使用 CRLF 换行，请转换为 Linux LF 换行".into());
        }
        let interpreter = line.split_whitespace().next().ok_or("脚本缺少 shebang 解释器")?;
        if !Path::new(interpreter).is_absolute() || !executable_file(Path::new(interpreter)) {
            return Err(format!("脚本解释器路径不存在或缺少执行权限：{interpreter}"));
        }
        return Ok(());
    }
    if !bytes.starts_with(b"\x7fELF") {
        return Err("请选择 Linux ELF 引擎或带 #! 解释器行的可执行脚本（Windows EXE 不适用）".into());
    }
    if n < 64 || bytes[6] != 1 {
        return Err("ELF 文件头截断或版本无效".into());
    }
    let machine = match bytes[5] {
        1 => u16::from_le_bytes([bytes[18], bytes[19]]),
        2 => u16::from_be_bytes([bytes[18], bytes[19]]),
        _ => return Err("ELF 字节序无效".into()),
    };
    let expected = match std::env::consts::ARCH {
        "x86_64" => (2, 1, 62),
        "aarch64" => (2, 1, 183),
        "x86" => (1, 1, 3),
        other => return Err(format!("尚未配置此架构的引擎校验：{other}")),
    };
    if (bytes[4], bytes[5], machine) != expected {
        return Err(format!("ELF 架构不匹配：当前 {}，文件 class={} machine={machine}", std::env::consts::ARCH, bytes[4]));
    }
    let kind = u16::from_le_bytes([bytes[16], bytes[17]]);
    if !matches!(kind, 2 | 3) {
        return Err("ELF 不是可执行文件或 PIE 引擎".into());
    }
    Ok(())
}

pub(crate) fn spawn_error(error: io::Error) -> String {
    let hint = match error.raw_os_error() {
        Some(libc::ENOENT) => "；检查 ELF 动态加载器或脚本解释器是否已安装",
        Some(libc::EACCES) => "；检查执行权限、目录访问权限及 noexec 挂载选项",
        Some(libc::ENOEXEC) => "；检查 Linux ELF 架构或脚本 #! 解释器行",
        _ => "；动态库加载失败的具体信息见引擎 stderr 日志",
    };
    format!("启动失败：{error}{hint}")
}

/// WNOWAIT deliberately keeps an exited group leader as a zombie until cleanup.
/// Its reserved PID prevents a later unrelated process group reusing this PGID.
pub(crate) fn child_exited(child: &Child) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(libc::P_PID, child.id(), &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
    };
    if result == -1 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { info.si_pid() } != 0)
}

fn proc_stat(pid: u32) -> Option<(char, i32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces and ')'; fields after its final ')' start at state (3).
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.first()?.chars().next()?, fields.get(2)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

fn group_has_live_members(pgid: i32) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else { return true; };
    entries.flatten().any(|entry| {
        entry.file_name().to_str().and_then(|s| s.parse().ok()).and_then(proc_stat)
            .is_some_and(|(state, group, _)| group == pgid && state != 'Z' && state != 'X')
    })
}

fn signal_group(pgid: i32, signal: i32) -> io::Result<()> {
    if unsafe { libc::kill(-pgid, signal) } == 0 { return Ok(()); }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
}

pub(crate) fn stop_group(child: &mut Child, grace: Duration) -> io::Result<()> {
    // Also verifies that this is still our unreaped child before using its PGID.
    child_exited(child)?;
    let pgid = i32::try_from(child.id()).map_err(|_| io::Error::other("invalid child PID"))?;
    if pgid <= 1 || unsafe { libc::getpgid(pgid) } != pgid {
        return Err(io::Error::other("托管引擎进程组身份已改变"));
    }
    signal_group(pgid, libc::SIGTERM)?;
    let deadline = Instant::now() + grace;
    while group_has_live_members(pgid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Leader has not been reaped, so even this final group signal cannot hit a reused PGID.
    signal_group(pgid, libc::SIGKILL)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !child_exited(child)? || group_has_live_members(pgid) {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "托管引擎进程组退出超时；保留状态以便重试"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub uid: u32,
    pub pgid: i32,
    pub started: u64,
    pub exe: PathBuf,
    pub device: u64,
    pub inode: u64,
}

pub(crate) fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let uid = fs::metadata(&root).ok()?.uid();
    let (_, pgid, started) = proc_stat(pid)?;
    let exe = fs::read_link(root.join("exe")).ok()?;
    let meta = fs::metadata(root.join("exe")).ok()?;
    let identity = ProcessIdentity { pid, uid, pgid, started, exe, device: meta.dev(), inode: meta.ino() };
    // A PID may exit/reappear while /proc fields are being read.
    if proc_stat(pid)?.2 != started { return None; }
    Some(identity)
}

pub(crate) fn matches_engine(identity: &ProcessIdentity, paths: &[PathBuf], managed: Option<u32>) -> bool {
    identity.pid > 1 && identity.pid != std::process::id()
        && Some(identity.pid) != managed
        && managed != u32::try_from(identity.pgid).ok()
        && identity.uid == unsafe { libc::geteuid() }
        && paths.iter().any(|path| {
            identity.exe == *path && fs::metadata(path).is_ok_and(|meta| {
                meta.dev() == identity.device && meta.ino() == identity.inode
            })
        })
}

pub(crate) fn discover_engines(paths: &[PathBuf], managed: Option<u32>) -> Vec<ProcessIdentity> {
    let Ok(entries) = fs::read_dir("/proc") else { return Vec::new(); };
    entries.flatten().filter_map(|entry| {
        let pid = entry.file_name().to_str()?.parse().ok()?;
        let identity = process_identity(pid)?;
        matches_engine(&identity, paths, managed).then_some(identity)
    }).collect()
}

pub(crate) fn verified_pidfd(expected: &ProcessIdentity, paths: &[PathBuf], managed: Option<u32>) -> Result<OwnedFd, String> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) };
    if fd < 0 { return Err(format!("打开进程身份句柄失败：{}", io::Error::last_os_error())); }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let current = process_identity(expected.pid).ok_or("引擎进程已退出或身份读取失败")?;
    if current != *expected || !matches_engine(&current, paths, managed) {
        return Err("进程身份已变化或未匹配当前配置引擎，请刷新进程列表".into());
    }
    Ok(fd)
}

fn signal_pidfd(fd: &OwnedFd, signal: i32) -> io::Result<()> {
    let result = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), signal, std::ptr::null::<libc::siginfo_t>(), 0) };
    if result == 0 { return Ok(()); }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
}

pub(crate) fn find_in_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?).map(|dir| dir.join(name)).find(|p| executable_file(p))
}

fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain_pipe(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0; 8192];
    // Bound both memory and per-poll work even for a noisy or stuck system tool.
    for _ in 0..8 {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if bytes.len() + n > 1024 * 1024 { return Err(io::Error::other("工具输出超过 1 MiB")); }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Nonblocking stdin/stdout/stderr share one deadline. No reader threads can leak.
/// On success we leave intentional desktop helper daemons (e.g. wl-copy) running;
/// on failure/timeout the still-unreaped leader pins its private group for cleanup.
pub(crate) fn run_tool(command: &mut std::process::Command, input: Option<&[u8]>, timeout: Duration) -> Result<std::process::Output, String> {
    use std::{io::Write, os::unix::process::CommandExt, process::Stdio};
    command.process_group(0).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("启动系统工具失败：{e}"))?;
    let result = (|| -> io::Result<std::process::Output> {
        if let Some(stdin) = child.stdin.as_ref() { nonblocking(stdin)?; }
        nonblocking(child.stdout.as_ref().ok_or_else(|| io::Error::other("缺少 stdout"))?)?;
        nonblocking(child.stderr.as_ref().ok_or_else(|| io::Error::other("缺少 stderr"))?)?;
        let deadline = Instant::now() + timeout;
        let input = input.unwrap_or_default();
        let mut written = 0;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            if let Some(stdin) = child.stdin.as_mut() {
                if written < input.len() {
                    match stdin.write(&input[written..]) {
                        Ok(n) => written += n,
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                        Err(e) => return Err(e),
                    }
                }
                if written == input.len() { child.stdin.take(); }
            }
            drain_pipe(child.stdout.as_mut().unwrap(), &mut stdout)?;
            drain_pipe(child.stderr.as_mut().unwrap(), &mut stderr)?;
            if child_exited(&child)? {
                // Drain currently available bytes; do not await EOF from a daemon's inherited pipe.
                drain_pipe(child.stdout.as_mut().unwrap(), &mut stdout)?;
                drain_pipe(child.stderr.as_mut().unwrap(), &mut stderr)?;
                if written < input.len() { return Err(io::Error::other("系统工具在接收完整输入前退出")); }
                return Ok(std::process::Output { status: child.wait()?, stdout, stderr });
            }
            if Instant::now() >= deadline { return Err(io::Error::new(io::ErrorKind::TimedOut, "系统工具运行超时")); }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    match result {
        Ok(output) => Ok(output),
        Err(error) => {
            let cleanup = stop_group(&mut child, Duration::from_millis(50));
            Err(match cleanup { Ok(()) => error.to_string(), Err(e) => format!("{error}；工具清理：{e}") })
        }
    }
}

pub(crate) fn checked_tool(command: &mut std::process::Command, input: Option<&[u8]>, timeout: Duration) -> Result<std::process::Output, String> {
    let output = run_tool(command, input, timeout)?;
    if output.status.success() { Ok(output) } else {
        Err(format!("系统工具退出状态 {}：{}", output.status, String::from_utf8_lossy(&output.stderr).trim()))
    }
}

pub(crate) fn memory_from_meminfo(text: &str) -> Option<f64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let mut fields = line.split_whitespace();
    fields.next()?;
    let kb = fields.next()?.parse::<u64>().ok()?;
    (fields.next()? == "kB" && kb > 0).then_some(kb as f64 / 1024.0)
}

pub(crate) fn disk_space(path: &Path) -> Result<(u64, u64), String> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "磁盘路径含 NUL")?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(format!("查询磁盘空间失败：{}", io::Error::last_os_error()));
    }
    let block = stat.f_frsize as u64;
    let total = (stat.f_blocks as u64).checked_mul(block).ok_or("磁盘总容量溢出")?;
    let available = (stat.f_bavail as u64).checked_mul(block).ok_or("磁盘可用容量溢出")?;
    Ok((total, available))
}

/// Only hardware GPU entries count, not loader/layer extensions or llvmpipe CPU.
pub(crate) fn vulkan_device(text: &str) -> Option<String> {
    let mut hardware = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("GPU") && line.ends_with(':') { hardware = false; }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "deviceType" {
                hardware = matches!(value.trim(), "PHYSICAL_DEVICE_TYPE_DISCRETE_GPU" | "PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU" | "PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU");
            }
            if key.trim() == "deviceName" && hardware && !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

pub(crate) fn clipboard_commands(wayland: bool, x11: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut commands = Vec::new();
    if wayland { commands.push(("wl-copy", vec!["--type", "text/plain;charset=utf-8"])); }
    if x11 {
        commands.push(("xclip", vec!["-selection", "clipboard", "-in"]));
        commands.push(("xsel", vec!["--clipboard", "--input"]));
    }
    commands
}

pub(crate) fn write_clipboard(text: &str, path: Option<&std::ffi::OsStr>, wayland: bool, x11: bool) -> Result<(), String> {
    let mut errors = Vec::new();
    for (name, args) in clipboard_commands(wayland, x11) {
        let Some(tool) = find_in_path(name, path) else { continue; };
        match checked_tool(std::process::Command::new(tool).args(args), Some(text.as_bytes()), Duration::from_secs(2)) {
            Ok(_) => return Ok(()),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(format!("剪贴板写入失败，请检查桌面会话与 wl-copy/xclip/xsel：{}", errors.join("；")))
}

pub(crate) fn reveal_directory(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(path)
    };
    let directory = if absolute.is_dir() { absolute.as_path() } else {
        absolute.parent().filter(|p| p.is_dir()).ok_or("文件所在目录不存在")?
    };
    Ok(directory.to_path_buf())
}

pub(crate) fn reveal_path(path: &Path, search_path: Option<&std::ffi::OsStr>) -> Result<(), String> {
    let directory = reveal_directory(path)?;
    let uri = reqwest::Url::from_directory_path(directory).map_err(|_| "目录路径转换失败")?;
    let mut errors = Vec::new();
    for name in ["gio", "xdg-open"] {
        let Some(tool) = find_in_path(name, search_path) else { continue; };
        let mut command = std::process::Command::new(tool);
        if name == "gio" { command.arg("open"); }
        command.arg(uri.as_str());
        match checked_tool(&mut command, None, Duration::from_secs(3)) {
            Ok(_) => return Ok(()),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(format!("打开目录失败，请检查 gio/xdg-open 与桌面会话：{}", errors.join("；")))
}

/// Snapshot injection keeps tests independent of the process-wide environment.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProxyEnvironment {
    pub http: Option<String>,
    pub https: Option<String>,
    pub all: Option<String>,
    pub no_proxy: Option<String>,
}
impl ProxyEnvironment {
    pub fn read(mut get: impl FnMut(&str) -> Option<String>) -> Self {
        let mut value = |lower: &str, upper: &str| {
            get(lower).or_else(|| get(upper)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        Self {
            http: value("http_proxy", "HTTP_PROXY"),
            https: value("https_proxy", "HTTPS_PROXY"),
            all: value("all_proxy", "ALL_PROXY"),
            no_proxy: value("no_proxy", "NO_PROXY"),
        }
    }
    pub fn for_scheme(&self, scheme: &str) -> Option<&str> {
        match scheme {
            "http" => self.http.as_deref().or(self.all.as_deref()),
            "https" => self.https.as_deref().or(self.all.as_deref()),
            _ => None,
        }
    }
    pub fn apply(&self, mut builder: reqwest::blocking::ClientBuilder) -> Result<reqwest::blocking::ClientBuilder, String> {
        builder = builder.no_proxy();
        // hyper-util's wildcard matches domains but not literal IPs; NO_PROXY=*
        // must disable both protocols entirely, including loopback/IP destinations.
        if self.no_proxy.as_deref().is_some_and(|list| list.split(',').any(|entry| entry.trim() == "*")) {
            return Ok(builder);
        }
        for scheme in ["http", "https"] {
            if let Some(url) = self.for_scheme(scheme) {
                let proxy = if scheme == "http" { reqwest::Proxy::http(url) } else { reqwest::Proxy::https(url) }
                    .map_err(|_| format!("{scheme} 环境代理地址无效"))?
                    .no_proxy(self.no_proxy.as_deref().and_then(reqwest::NoProxy::from_string));
                builder = builder.proxy(proxy);
            }
        }
        Ok(builder)
    }
    pub fn display(&self) -> Option<String> {
        // The single IPC string is display-only, never reused as a Proxy::all address.
        let entries: Vec<_> = [("HTTP_PROXY", &self.http), ("HTTPS_PROXY", &self.https), ("ALL_PROXY", &self.all), ("NO_PROXY", &self.no_proxy)]
            .into_iter().filter_map(|(key, value)| value.as_ref().map(|value| {
                let shown = if key == "NO_PROXY" { value.clone() } else {
                    let address = if value.contains("://") { value.clone() } else { format!("http://{value}") };
                    reqwest::Url::parse(&address).map(|mut url| {
                        let _ = url.set_username(""); let _ = url.set_password(None); url.to_string()
                    }).unwrap_or_else(|_| "[地址格式待检查]".into())
                };
                format!("{key}={shown}")
            })).collect();
        (!entries.is_empty()).then(|| entries.join("; "))
    }
}

pub(crate) fn stop_external(fd: &OwnedFd, grace: Duration) -> Result<(), String> {
    signal_pidfd(fd, libc::SIGTERM).map_err(|e| e.to_string())?;
    let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let deadline = Instant::now() + grace;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().min(i32::MAX as u128) as i32) };
        if result > 0 { return Ok(()); }
        if result == 0 { break; }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error.to_string()); }
    }
    signal_pidfd(fd, libc::SIGKILL).map_err(|e| e.to_string())?;
    let result = unsafe { libc::poll(&mut poll, 1, 2000) };
    if result > 0 { Ok(()) } else { Err("外部引擎退出等待超时或被中断；请刷新进程列表".into()) }
}
