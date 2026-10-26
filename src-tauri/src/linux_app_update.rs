//! Linux application updates. The IPC path is only a lookup key for a session receipt.
//! No user configuration/model path participates in the installation transaction.
use super::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::time::Duration;
use tempfile::TempDir;

mod appimage;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    AppImage,
    Deb,
    Rpm,
    Manual,
}
impl Kind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::AppImage => "appimage",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Manual => "manual",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::AppImage => ".appimage",
            Self::Deb => ".deb",
            Self::Rpm => ".rpm",
            Self::Manual => "",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Context {
    pub kind: Kind,
    pub target: Option<PathBuf>,
    pub reason: String,
}
fn manual(reason: &str) -> Context {
    Context {
        kind: Kind::Manual,
        target: None,
        reason: reason.into(),
    }
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn regular_owned(path: &Path, writable: bool) -> Result<fs::File, String> {
    if !path.is_absolute() {
        return Err("更新路径必须是绝对路径".into());
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(error)?;
    let meta = file.metadata().map_err(error)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || (writable && meta.mode() & 0o200 == 0)
    {
        return Err("更新目标需要本用户拥有的普通可写文件".into());
    }
    if writable {
        // The active AppImage runtime holds ETXTBSY on its executable. We replace by
        // atomic rename, never by opening that running inode for writing. Check actual
        // effective-user permissions (including a read-only filesystem) without opening it.
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(error)?;
        if unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::W_OK, libc::AT_EACCESS) }
            != 0
        {
            return Err(error(std::io::Error::last_os_error()));
        }
    }
    Ok(file)
}
fn image_header(file: &mut fs::File) -> Result<(), String> {
    let mut header = [0u8; 20];
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    file.read_exact(&mut header).map_err(error)?;
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || &header[8..11] != b"AI\x02"
        || &header[18..20] != [62, 0]
    {
        return Err("需要 x86_64 ELF AppImage type 2".into());
    }
    Ok(())
}
fn mount_field(text: &str) -> String {
    text.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}
fn image_mount_binding(mounts: &str, image: &Path, appdir: &Path, uid: u32) -> bool {
    let Some(name) = image.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let mut matches = 0;
    let mut bound = false;
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = left.split_whitespace().collect();
        let detail: Vec<_> = right.split_whitespace().collect();
        if fields.len() < 6
            || detail.len() < 3
            || Path::new(&mount_field(fields[4])) != appdir
        {
            continue;
        }
        // An old image runtime may keep a different mount alive during replacement.
        // Require one binding at this GUI's exact APPDIR, not one mount per filename.
        // Stacked/ambiguous entries at the same mountpoint still fail closed.
        matches += 1;
        bound = mount_field(detail[1]) == name
            && mount_field(detail[0]) == format!("fuse.{name}")
            && fields[5].split(',').any(|s| s == "ro")
            && detail[2].split(',').any(|s| s == format!("user_id={uid}"));
    }
    matches == 1 && bound
}
fn active_image_runtime(image: &Path, appdir: &Path) -> bool {
    // Current type-2 runtime daemonizes into its own session, so it is not a GUI
    // ancestor. Bind the unique kernel FUSE mount to an owned runtime whose executable
    // AND open backing-image descriptor have the exact image device/inode. An APPIMAGE
    // environment value, a same-named file, or an unrelated mount alone proves nothing.
    let Ok(meta) = fs::symlink_metadata(image) else {
        return false;
    };
    let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != uid || !image_mount_binding(&mounts, image, appdir, uid) {
        return false;
    }
    let Ok(processes) = fs::read_dir("/proc") else {
        return false;
    };
    for process in processes.flatten() {
        if !process
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Ok(exe) = fs::metadata(process.path().join("exe")) else {
            continue;
        };
        if exe.uid() != uid || exe.dev() != meta.dev() || exe.ino() != meta.ino() {
            continue;
        }
        let Ok(fds) = fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let mut image_open = false;
        let mut fuse_open = false;
        for fd in fds.flatten() {
            if fs::read_link(fd.path()).ok().as_deref() == Some(Path::new("/dev/fuse")) {
                fuse_open = true;
            }
            if fs::metadata(fd.path())
                .is_ok_and(|m| m.is_file() && m.dev() == meta.dev() && m.ino() == meta.ino())
            {
                image_open = true;
            }
        }
        if image_open && fuse_open {
            return true;
        }
    }
    false
}
fn appimage_context(
    image: &Path,
    appdir: &Path,
    exe: &Path,
    runtime_verified: bool,
) -> Result<Context, String> {
    let mut file = regular_owned(image, true)?;
    image_header(&mut file)?;
    let canonical = fs::canonicalize(image).map_err(error)?;
    if canonical != image {
        return Err("AppImage 路径含符号链接或非规范组件".into());
    }
    let parent = image.parent().ok_or("AppImage 父目录缺失")?;
    let meta = fs::symlink_metadata(parent).map_err(error)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o200 == 0
        || meta.mode() & 0o022 != 0
    {
        return Err("AppImage 目录需要本用户独占写入权限".into());
    }
    let root = fs::canonicalize(appdir).map_err(error)?;
    if !runtime_verified
        || !root
            .file_name()
            .is_some_and(|s| s.to_string_lossy().starts_with(".mount_"))
        || !exe.starts_with(&root)
        || exe
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_lowercase)
            .as_deref()
            != Some("cookllm")
    {
        return Err("APPIMAGE 与当前挂载应用进程的身份未核实".into());
    }
    Ok(Context {
        kind: Kind::AppImage,
        target: Some(image.into()),
        reason: "替换当前 AppImage，保留旧版；React 启动上报超时将回退（不代表模型健康检查）"
            .into(),
    })
}
fn tool_output(tool: &str, args: &[&std::ffi::OsStr]) -> Option<String> {
    let result =
        platform::checked_tool(Command::new(tool).args(args), None, Duration::from_secs(5)).ok()?;
    String::from_utf8(result.stdout)
        .ok()
        .map(|s| s.trim().into())
}
// Tauri's actual deb control/RPM NAME is cook-llm; the installed executable is cookllm.
// Verified from the existing local bundle headers, not inferred from the asset filename.
const SYSTEM_PACKAGE: &str = "cook-llm";
fn package_identity(kind: Kind, output: &str) -> bool {
    match kind {
        Kind::Deb => output == format!("{SYSTEM_PACKAGE} amd64"),
        Kind::Rpm => output == format!("{SYSTEM_PACKAGE} x86_64"),
        _ => false,
    }
}
fn deb_owner(exe: &Path, output: &str) -> bool {
    output == format!("{SYSTEM_PACKAGE}: {}", exe.display())
        || output == format!("{SYSTEM_PACKAGE}:amd64: {}", exe.display())
}
fn package_kind(exe: &Path) -> Kind {
    // A development binary or an arbitrary APPIMAGE value never selects a system package.
    if exe != Path::new("/usr/bin/cookllm") {
        return Kind::Manual;
    }
    let text = tool_output("/usr/bin/dpkg-query", &["-S".as_ref(), exe.as_os_str()]);
    if text.as_deref().is_some_and(|s| deb_owner(exe, s)) {
        if tool_output(
            "/usr/bin/dpkg-query",
            &[
                "-W".as_ref(),
                "-f=${Package} ${Architecture} ${db:Status-Status}".as_ref(),
                SYSTEM_PACKAGE.as_ref(),
            ],
        )
        .as_deref()
        .and_then(|s| s.strip_suffix(" installed"))
        .is_some_and(|s| package_identity(Kind::Deb, s))
        {
            return Kind::Deb;
        }
    }
    if tool_output(
        "/usr/bin/rpm",
        &[
            "-qf".as_ref(),
            "--qf".as_ref(),
            "%{NAME} %{ARCH}".as_ref(),
            exe.as_os_str(),
        ],
    )
    .is_some_and(|s| package_identity(Kind::Rpm, &s))
    {
        return Kind::Rpm;
    }
    Kind::Manual
}
pub(super) fn detect() -> Context {
    if std::env::consts::ARCH != "x86_64" {
        return manual("自动更新仅验证 x86_64");
    }
    let Ok(exe) = std::env::current_exe() else {
        return manual("当前应用路径未知");
    };
    if let Some(image) = std::env::var_os("APPIMAGE") {
        let image = PathBuf::from(image);
        let appdir = std::env::var_os("APPDIR")
            .map(PathBuf::from)
            .unwrap_or_default();
        return appimage_context(&image, &appdir, &exe, active_image_runtime(&image, &appdir))
            .unwrap_or_else(|e| manual(&e));
    }
    let kind = package_kind(&exe);
    if kind == Kind::Manual {
        return manual("当前进程未核实为 CookLLM 安装包（源码或未知布局），请从发布页手动更新");
    }
    Context {
        kind,
        target: None,
        reason: "交给系统图形安装器；仍需用户在系统窗口确认，交接不表示升级已完成".into(),
    }
}
#[derive(Clone, Debug)]
pub(super) struct Asset {
    pub url: String,
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub kind: Kind,
}
fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}
fn asset_tag(url: &str, name: &str) -> Result<String, String> {
    let u = reqwest::Url::parse(url).map_err(error)?;
    let prefix = format!("/{APP_GITHUB_REPO}/releases/download/");
    if u.scheme() != "https"
        || u.host_str() != Some("github.com")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port_or_known_default() != Some(443)
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err("应用资产地址非官方发布路径".into());
    }
    let rest = u.path().strip_prefix(&prefix).ok_or("应用资产仓库不匹配")?;
    let (tag, filename) = rest.split_once('/').ok_or("应用资产 tag 缺失")?;
    if !valid_component(tag) || !valid_component(name) || filename != name {
        return Err("应用资产 tag 或文件名无效".into());
    }
    Ok(tag.into())
}
pub(super) fn select(value: &serde_json::Value, kind: Kind) -> Result<Asset, String> {
    if kind == Kind::Manual {
        return Err("当前安装类型为手动更新".into());
    }
    let tag = value["tag_name"].as_str().ok_or("发布 tag 缺失")?;
    let assets = value["assets"].as_array().ok_or("发布资产缺失")?;
    let mut found = Vec::new();
    for item in assets {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        let lower = name.to_lowercase();
        if !lower.starts_with("cookllm")
            || !lower.ends_with(kind.extension())
            || lower.contains("arm")
            || lower.contains("aarch")
            || lower.contains("i686")
            || lower.contains("i386")
            || lower.contains("win")
        {
            continue;
        }
        if ![
            "_amd64", "-amd64", ".amd64", "_x86_64", "-x86_64", ".x86_64",
        ]
        .iter()
        .any(|arch| lower.ends_with(&format!("{arch}{}", kind.extension())))
        {
            continue;
        }
        let Some(url) = item["browser_download_url"].as_str() else {
            continue;
        };
        if asset_tag(url, name).ok().as_deref() != Some(tag) {
            continue;
        }
        let Some(size) = item["size"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 4 * 1024 * 1024 * 1024)
        else {
            continue;
        };
        let Some(digest) = item["digest"]
            .as_str()
            .and_then(|s| s.strip_prefix("sha256:"))
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        found.push(Asset {
            name: name.into(),
            url: url.into(),
            size,
            digest: digest.to_lowercase(),
            kind,
        });
    }
    if found.len() != 1 {
        return Err("官方发布缺少唯一匹配 x86_64 包或有效 SHA-256 摘要，请手动更新".into());
    }
    Ok(found.remove(0))
}
fn final_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}
struct Receipt {
    directory: TempDir,
    path: PathBuf,
    asset: Asset,
    context: Context,
    dev: u64,
    ino: u64,
}
static RECEIPTS: OnceLock<Mutex<HashMap<PathBuf, Receipt>>> = OnceLock::new();
static BUSY: AtomicBool = AtomicBool::new(false);
struct Busy;
impl Busy {
    fn acquire() -> Result<Self, String> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "应用更新任务正在进行".to_string())?;
        Ok(Self)
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}
fn receipts() -> &'static Mutex<HashMap<PathBuf, Receipt>> {
    RECEIPTS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn verify_receipt(receipt: &Receipt) -> Result<fs::File, String> {
    let meta = fs::symlink_metadata(receipt.directory.path()).map_err(error)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("下载私有目录身份已变更".into());
    }
    let mut file = regular_owned(&receipt.path, false)?;
    let meta = file.metadata().map_err(error)?;
    if meta.dev() != receipt.dev || meta.ino() != receipt.ino || meta.len() != receipt.asset.size {
        return Err("下载 receipt 文件身份或大小已变更".into());
    }
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(error)?;
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != receipt.asset.digest {
        return Err("下载 receipt SHA-256 校验失败".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    if receipt.asset.kind == Kind::AppImage {
        image_header(&mut file)?;
    }
    Ok(file)
}
fn progress(app: &AppHandle, phase: &str, n: u64, total: u64, message: &str) {
    let _ = app.emit("download-progress", serde_json::json!({ "source": "app", "phase": phase, "percent": if total == 0 {0} else {(n * 100 / total).min(100)}, "downloaded": n, "total": total, "speedBps": 0, "message": message }));
}
pub(super) fn release_metadata(
    network: &NetworkConfig,
    tag: Option<&str>,
    cancel: impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let suffix = match tag {
        Some(tag) if valid_component(tag) => format!("tags/{tag}"),
        Some(_) => return Err("发布 tag 无效".into()),
        None => "latest".into(),
    };
    let url = format!("https://api.github.com/repos/{APP_GITHUB_REPO}/releases/{suffix}");
    let environment = platform::ProxyEnvironment::read(|key| std::env::var(key).ok());
    tauri::async_runtime::block_on(async {
        let client = linux_engine_update::download_client(network, &environment)?;
        let request = async {
            let mut response = client
                .get(&url)
                .header("accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(error)?
                .error_for_status()
                .map_err(error)?;
            if response.url().as_str() != url {
                return Err("官方发布 API 重定向身份不匹配".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(error)? {
                if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                    return Err("发布元数据超过大小上限".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(error)
        };
        let mut request = std::pin::pin!(request);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if cancel() {
                return Err("cancelled".into());
            }
            if std::time::Instant::now() >= deadline {
                return Err("官方发布元数据请求超时".into());
            }
            if let Ok(result) = tokio::time::timeout(Duration::from_millis(50), &mut request).await
            {
                return result;
            }
        }
    })
}
pub(super) fn download(app: AppHandle, url: String, name: String) -> Result<String, String> {
    let _busy = Busy::acquire()?;
    let cancel = CancelFlag(&APP_UPDATE_CANCEL);
    let _cancel_guard = CancelGuard(cancel);
    let context = detect();
    let tag = asset_tag(&url, &name)?;
    let network = read_config(&app)?.network.clone().unwrap_or_default();
    // Construct the API URL ourselves, never accept metadata or a hash supplied by IPC.
    let release = release_metadata(&network, Some(&tag), || cancel.cancelled())?;
    if release["tag_name"].as_str() != Some(&tag) {
        return Err("发布 tag 与请求不一致".into());
    }
    let asset = select(&release, context.kind)?;
    if asset.url != url || asset.name != name {
        return Err("选择与官方发布资产不一致".into());
    }
    if cancel.cancelled() {
        return Err("cancelled".into());
    }
    // Random 0700 directory, owned for the lifetime of the session receipt; each file is 0600.
    let directory = tempfile::Builder::new()
        .prefix("cookllm-app-download-")
        .tempdir()
        .map_err(error)?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).map_err(error)?;
    let path = directory.path().join(&name);
    let environment = platform::ProxyEnvironment::read(|key| std::env::var(key).ok());
    tauri::async_runtime::block_on(async {
        let client = linux_engine_update::download_client(&network, &environment)?;
        linux_engine_update::download_stream(
            client.get(&url),
            asset.size,
            &path,
            &|| cancel.cancelled(),
            |n, t| progress(&app, "download", n, t, "下载应用安装包"),
            linux_engine_update::DownloadTiming::default(),
            final_url,
        )
        .await
    })?;
    let meta = fs::symlink_metadata(&path).map_err(error)?;
    let receipt = Receipt {
        directory,
        path: path.clone(),
        asset,
        context,
        dev: meta.dev(),
        ino: meta.ino(),
    };
    verify_receipt(&receipt)?;
    if cancel.cancelled() {
        return Err("cancelled".into());
    }
    progress(
        &app,
        "done",
        receipt.asset.size,
        receipt.asset.size,
        "安装包已校验，等待确认安装",
    );
    // Bound retained downloads; a replaced receipt's private directory is removed by RAII.
    let mut registry = receipts().lock().map_err(error)?;
    registry.clear();
    registry.insert(path.clone(), receipt);
    Ok(path.to_string_lossy().into())
}
fn package_installer(kind: Kind) -> Option<PathBuf> {
    let candidates: &[&str] = match kind {
        Kind::Deb => &[
            "/usr/bin/gdebi-gtk",
            "/usr/bin/gnome-software",
            "/usr/bin/plasma-discover",
        ],
        Kind::Rpm => &["/usr/bin/gnome-software", "/usr/bin/plasma-discover"],
        _ => &[],
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| platform::executable_file(p))
}
pub(super) fn supported(context: &Context) -> bool {
    match context.kind {
        Kind::AppImage => appimage::tools_available(),
        Kind::Deb | Kind::Rpm => package_installer(context.kind).is_some(),
        Kind::Manual => false,
    }
}
pub(super) fn notify_ready() -> Result<(), String> {
    appimage::notify_ready()
}
pub(super) fn install(app: &AppHandle, path: &str) -> Result<Option<AppInstallResult>, String> {
    let _busy = Busy::acquire()?;
    let mut registry = receipts().lock().map_err(error)?;
    let receipt = registry
        .get(Path::new(path))
        .ok_or("安装需要本会话后端下载 receipt，请重新下载")?;
    if detect() != receipt.context {
        return Err("当前运行安装类型或目标已变更，请重新检查更新".into());
    }
    let mut file = verify_receipt(receipt)?;
    if receipt.context.kind == Kind::AppImage {
        let target = receipt.context.target.as_ref().ok_or("AppImage 目标缺失")?;
        let prepared = appimage::prepare(target, &mut file, &receipt.asset.digest)?;
        // Start the constrained helper first: failed spawn leaves the app and old target alive.
        let mut helper = prepared.spawn()?;
        let stop = (|| {
            let state = app.state::<ProcessState>();
            let mut guard = state.0.lock().map_err(error)?;
            if let Some(managed) = guard.as_mut() {
                kill_managed_child(managed)?;
                *guard = None;
            }
            Ok::<_, String>(())
        })();
        if let Err(e) = stop {
            let _ = helper.kill();
            let _ = helper.wait();
            return Err(e);
        }
        let backup = match prepared.commit() {
            Ok(path) => path,
            Err(error) => {
                let _ = helper.kill();
                let _ = helper.wait();
                return Err(error);
            }
        };
        registry.remove(Path::new(path));
        let result = AppInstallResult { status: "restarting".into(), message: "退出后替换并重启；90 秒内未收到 React 启动确认将回退，旧版备份保留。此确认不验证模型推理。".into(), backup_path: Some(backup.to_string_lossy().into()) };
        // Give the IPC result a chance to flush; cleanup and exit happen on the app's main loop.
        let handle = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            handle.cleanup_before_exit();
            handle.exit(0);
        });
        Ok(Some(result))
    } else {
        let tool = package_installer(receipt.context.kind)
            .ok_or("未找到系统图形安装器，请从发布页手动安装")?;
        // The receipt remains private and alive while the system installer opens it.
        // Validate package identity/architecture using the installed package tool, never execute it.
        validate_package(&receipt.path, receipt.context.kind)?;
        let mut command = Command::new(&tool);
        match tool.file_name().and_then(|s| s.to_str()) {
            Some("gnome-software") => {
                command.arg("--local-filename");
            }
            Some("plasma-discover") => {
                command.arg("--local-filename");
            }
            _ => {}
        }
        command
            .arg(&receipt.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动系统安装器失败：{e}"))?;
        std::thread::sleep(Duration::from_millis(200));
        if child
            .try_wait()
            .map_err(error)?
            .is_some_and(|s| !s.success())
        {
            return Err("系统安装器提前退出，请手动安装".into());
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(Some(AppInstallResult {
            status: "installer-opened".into(),
            message:
                "已交给系统图形安装器；请在系统窗口确认。应用保持运行，升级结果由系统安装器报告。"
                    .into(),
            backup_path: None,
        }))
    }
}
fn validate_package(path: &Path, kind: Kind) -> Result<(), String> {
    let output = match kind {
        Kind::Deb => tool_output(
            "/usr/bin/dpkg-deb",
            &[
                "--show".as_ref(),
                "--showformat=${Package} ${Architecture}".as_ref(),
                path.as_os_str(),
            ],
        ),
        Kind::Rpm => tool_output(
            "/usr/bin/rpm",
            &[
                "-qp".as_ref(),
                "--qf".as_ref(),
                "%{NAME} %{ARCH}".as_ref(),
                path.as_os_str(),
            ],
        ),
        _ => None,
    };
    if output.is_some_and(|s| package_identity(kind, &s)) {
        Ok(())
    } else {
        Err("安装包内容的应用名称或架构未核实".into())
    }
}
