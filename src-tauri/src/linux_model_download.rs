// Linux model downloads use one owned async future; no detached IO workers.
use super::{linux_engine_update, platform, DownloadRegistry, DownloadTaskControl, NetworkConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    future::Future,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

pub(super) fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}
fn metadata_path(dest: &Path) -> PathBuf {
    let mut name = partial_path(dest).into_os_string();
    name.push(".cookllm.json");
    PathBuf::from(name)
}

// The map mutex is held only to claim/release one canonical target, never during IO.
struct TargetLease(PathBuf);
fn targets() -> &'static Mutex<HashSet<PathBuf>> {
    static TARGETS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    TARGETS.get_or_init(|| Mutex::new(HashSet::new()))
}
impl TargetLease {
    fn acquire(dest: &Path) -> Result<Self, String> {
        let parent = dest.parent().ok_or("下载目录为空")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let key = parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(dest.file_name().ok_or("下载文件名为空")?);
        if !targets()
            .lock()
            .map_err(|_| "下载目标锁不可用")?
            .insert(key.clone())
        {
            return Err("同一目标已有下载任务，请等待该任务结束后重试".into());
        }
        Ok(Self(key))
    }
}
impl Drop for TargetLease {
    fn drop(&mut self) {
        if let Ok(mut targets) = targets().lock() {
            targets.remove(&self.0);
        }
    }
}

// Explicit file deletion stays separate from download cancellation. Only a .part
// request owns sidecar cleanup, so Promise.all([dest, dest.part]) is idempotent.
// Holding the state mutex prevents a new writer from claiming the target between
// the active check and unlink; cleanup requests serialize without claiming leases.
pub(super) fn remove_local_file(root: &Path, requested: &Path) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("模型库目录校验失败：{e}"))?;
    let parent = requested
        .parent()
        .ok_or("删除目标父目录为空")?
        .canonicalize()
        .map_err(|e| format!("删除目标父目录校验失败：{e}"))?;
    if !parent.starts_with(&root) {
        return Err("删除目标位于模型库目录之外".into());
    }
    let name = requested.file_name().ok_or("删除目标文件名为空")?;
    let target = parent.join(name);
    let partial_dest = name
        .to_str()
        .and_then(|name| name.strip_suffix(".part"))
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(|name| parent.join(name));
    let metadata_dest = name
        .to_str()
        .and_then(|name| name.strip_suffix(".part.cookllm.json"))
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(|name| parent.join(name));
    let key = partial_dest
        .as_deref()
        .or(metadata_dest.as_deref())
        .unwrap_or(&target);
    let active = targets().lock().map_err(|_| "下载目标锁不可用")?;
    if active.contains(key) {
        return Err("目标下载仍在运行，请等待停止后清理".into());
    }
    // Read existence after taking the guard: a writer may have finished, or another
    // cleanup may already have removed this exact file while this call was queued.
    // A symlink target is never followed/deleted, including dangling symlinks.
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("删除目标是符号链接，已保留".into())
        }
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读取删除目标失败：{e}")),
    };
    if meta.as_ref().is_some_and(|m| m.is_dir())
        && active.iter().any(|path| path.starts_with(&target))
    {
        return Err("目标下载仍在运行，请等待停止后清理".into());
    }
    if let Some(meta) = meta {
        if meta.is_dir() {
            fs::remove_dir_all(&target).map_err(|e| format!("删除目录失败：{e}"))?;
        } else {
            fs::remove_file(&target).map_err(|e| format!("删除文件失败：{e}"))?;
        }
    }
    // Root/parent validation and the active guard also apply when .part is absent.
    // load_metadata opens with O_NOFOLLOW and recognizes only our bounded format;
    // unknown JSON, directories and links are preserved, never recursively removed.
    if let Some(dest) = partial_dest {
        let sidecar = metadata_path(&dest);
        if load_metadata(&sidecar).is_some() {
            fs::remove_file(&sidecar).map_err(|e| format!("删除下载元数据失败：{e}"))?;
        }
    }
    drop(active);
    Ok(())
}

pub(super) struct Session<'a> {
    pub registry: &'a DownloadRegistry,
    pub task: &'a Arc<DownloadTaskControl>,
    pub id: &'a str,
}
impl Session<'_> {
    // Hold the generation lock across each write/rename/event. A new register cannot
    // supersede this generation between the check and a filesystem mutation.
    fn active<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let guard = self.registry.0.lock().map_err(|_| "下载注册表不可用")?;
        if !guard
            .get(self.id)
            .is_some_and(|t| Arc::ptr_eq(t, self.task))
        {
            return Err("下载任务已被接管".into());
        }
        if self.task.cancel.load(Ordering::Relaxed) {
            return Err("下载已取消".into());
        }
        if self.task.pause.load(Ordering::Relaxed) {
            return Err("下载已暂停".into());
        }
        action()
    }
    fn check(&self) -> Result<(), String> {
        self.active(|| Ok(()))
    }
    fn event(&self, action: impl FnOnce()) {
        if let Ok(guard) = self.registry.0.lock() {
            if guard
                .get(self.id)
                .is_some_and(|t| Arc::ptr_eq(t, self.task))
            {
                action();
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Timing {
    overall: Duration,
    idle: Duration,
    poll: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            overall: Duration::from_secs(3600),
            idle: Duration::from_secs(60),
            poll: Duration::from_millis(50),
        }
    }
}
struct Watch {
    deadline: Instant,
    last: Instant,
    timing: Timing,
}
impl Watch {
    fn new(timing: Timing) -> Self {
        let now = Instant::now();
        Self {
            deadline: now + timing.overall,
            last: now,
            timing,
        }
    }
    fn check(&self, session: &Session<'_>) -> Result<(), String> {
        session.check()?;
        if Instant::now() >= self.deadline {
            return Err("下载超过总时间上限".into());
        }
        if self.last.elapsed() >= self.timing.idle {
            return Err("下载长时间无进度，已超时".into());
        }
        Ok(())
    }
    async fn wait<T>(
        &self,
        future: impl Future<Output = Result<T, reqwest::Error>>,
        session: &Session<'_>,
    ) -> Result<T, String> {
        let mut future = std::pin::pin!(future);
        loop {
            self.check(session)?;
            let now = Instant::now();
            let tick = self
                .timing
                .poll
                .min(self.deadline.saturating_duration_since(now))
                .min((self.last + self.timing.idle).saturating_duration_since(now));
            if let Ok(result) = tokio::time::timeout(tick, &mut future).await {
                self.check(session)?;
                return result.map_err(|e| format!("下载失败：{e}"));
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ResumeMetadata {
    format: String,
    source: String,
    validator: String,
    total: u64,
}
const METADATA_FORMAT: &str = "cookllm-model-download-v1";
fn load_metadata(path: &Path) -> Option<ResumeMetadata> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > 16384 {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, 16385), &mut bytes).ok()?;
    if bytes.len() > 16384 {
        return None;
    }
    let value: ResumeMetadata = serde_json::from_slice(&bytes).ok()?;
    (value.format == METADATA_FORMAT).then_some(value)
}
fn no_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => Err("下载目标应为普通文件".into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
fn open_file(path: &Path, append: bool) -> Result<fs::File, String> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("打开下载文件失败：{e}"))
}
fn validator(response: &reqwest::Response) -> Option<String> {
    let headers = response.headers();
    headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.starts_with("W/") && v.starts_with('"') && v.ends_with('"'))
        .or_else(|| {
            headers
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
        })
        .map(str::to_owned)
}
fn hf_host(url: &str) -> bool {
    reqwest::Url::parse(url).ok().is_some_and(|u| {
        u.scheme() == "https" && matches!(u.host_str(), Some("huggingface.co" | "hf-mirror.com"))
    })
}
fn range_header(value: &str, offset: u64) -> Result<(u64, u64), String> {
    let parse = || -> Option<(u64, u64)> {
        let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        let start: u64 = start.parse().ok()?;
        let end: u64 = end.parse().ok()?;
        let total: u64 = total.parse().ok()?;
        if start != offset || end < start || end >= total || end + 1 != total {
            return None;
        }
        Some((total, end - start + 1))
    };
    parse().ok_or_else(|| "下载响应 Content-Range 与断点不一致".into())
}
fn http_error(code: u16, token: &str) -> String {
    match code {
        401 | 403 if token.trim().is_empty() => {
            "HF_GATED_NEED_TOKEN::该模型需要授权（gated），请在设置中配置 Hugging Face Token".into()
        }
        401 | 403 => {
            "HF_GATED_NO_PERMISSION::该模型无访问权限，请前往网页端同意模型许可协议后重试".into()
        }
        404 => "文件不存在或仓库未公开（HTTP 404）".into(),
        _ => format!("下载失败：HTTP {code}"),
    }
}

// Retry only connection/header failures. HTTP, malformed ranges and body failures
// never move to another server and never discard the recoverable partial.
async fn response_with_fallback(
    network: &NetworkConfig,
    url: &str,
    token: &str,
    offset: u64,
    resume: Option<&ResumeMetadata>,
    watch: &mut Watch,
    session: &Session<'_>,
) -> Result<reqwest::Response, String> {
    let environment = platform::ProxyEnvironment::read(|key| std::env::var(key).ok());
    let direct = NetworkConfig {
        proxy_mode: "direct".into(),
        proxy_url: String::new(),
    };
    let mut errors = Vec::new();
    // Local proxy connection attempts are asynchronous and control-polled, unlike
    // the legacy blocking probe. There is no extra GET solely to probe a channel.
    let mut channels = vec![
        (network.clone(), url.to_string()),
        (direct.clone(), url.to_string()),
    ];
    for port in [7897, 7890, 7891, 10809, 10808, 1080, 8888, 2080] {
        channels.push((
            NetworkConfig {
                proxy_mode: "manual".into(),
                proxy_url: format!("http://127.0.0.1:{port}"),
            },
            url.to_string(),
        ));
    }
    if hf_host(url) {
        let mut mirror = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        if mirror.host_str() == Some("huggingface.co") {
            mirror
                .set_host(Some("hf-mirror.com"))
                .map_err(|e| e.to_string())?;
            channels.push((direct, mirror.to_string()));
        }
    }
    for (index, (network, target)) in channels.iter().enumerate() {
        watch.check(session)?;
        let client = match linux_engine_update::download_client(network, &environment) {
            Ok(client) => client,
            Err(e) if index == 0 => return Err(e),
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let mut request = client
            .get(target)
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if !token.trim().is_empty() && hf_host(target) {
            request = request.bearer_auth(token.trim());
        }
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            if let Some(meta) = resume {
                request = request.header(reqwest::header::IF_RANGE, &meta.validator);
            }
        }
        // Bound a failed channel's headers independently of the complete download.
        request = request.timeout(Duration::from_secs(3600));
        let attempt =
            tokio::time::timeout(Duration::from_secs(30), watch.wait(request.send(), session))
                .await;
        match attempt {
            Ok(Ok(response)) => {
                watch.last = Instant::now();
                return Ok(response);
            }
            Ok(Err(error)) => {
                watch.check(session)?;
                errors.push(error);
            }
            Err(_) => {
                watch.check(session)?;
                errors.push("下载通道响应头超时".into());
            }
        }
    }
    Err(format!("下载连接失败：{}", errors.join("；")))
}

async fn transfer(
    network: &NetworkConfig,
    url: &str,
    token: &str,
    dest: &Path,
    session: &Session<'_>,
    progress: &impl Fn(&str, u64, u64, u64),
    timing: Timing,
) -> Result<u64, String> {
    let _lease = TargetLease::acquire(dest)?;
    let part = partial_path(dest);
    let sidecar = metadata_path(dest);
    no_symlink(dest)?;
    no_symlink(&part)?;
    no_symlink(&sidecar)?;
    // Unknown legacy model.part is intentionally left untouched. Only our matching
    // metadata proves the provenance of an existing partial. Without it restart.
    let saved = load_metadata(&sidecar)
        .filter(|m| m.source == url && !m.validator.is_empty() && m.total > 0);
    let offset = saved
        .as_ref()
        .and_then(|m| fs::metadata(&part).ok().map(|file| (file.len(), m.total)))
        // A full-size partial still needs a new transfer/identity check, not a
        // length-only completion claim or a permanently failing Range=total-.
        .filter(|(length, total)| *length < *total)
        .map(|(length, _)| length)
        .unwrap_or(0);
    let mut watch = Watch::new(timing);
    let result = async {
        session.check()?;
        session.event(|| {
            progress(
                "download",
                offset,
                saved.as_ref().map(|m| m.total).unwrap_or(0),
                0,
            )
        });
        let mut response = response_with_fallback(
            network,
            url,
            token,
            offset,
            saved.as_ref(),
            &mut watch,
            session,
        )
        .await?;
        let code = response.status().as_u16();
        if code != 200 && code != 206 {
            return Err(http_error(code, token));
        }
        let append = code == 206 && offset > 0;
        let (total, body_size) = if code == 206 {
            let range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .ok_or("下载响应缺少 Content-Range")?;
            let (total, body) = range_header(range, offset)?;
            if response.content_length().is_some_and(|n| n != body) {
                return Err("下载响应 Range 长度不一致".into());
            }
            if offset > 0 {
                let meta = saved.as_ref().ok_or("断点缺少来源校验")?;
                if total != meta.total
                    || validator(&response).as_deref() != Some(meta.validator.as_str())
                {
                    return Err("远端文件已变化，请重新开始下载".into());
                }
            }
            (total, body)
        } else {
            let total = response
                .content_length()
                .filter(|n| *n > 0)
                .ok_or("下载响应缺少有效文件长度")?;
            (total, total)
        };
        let remote_validator = validator(&response);
        let mut file = session.active(|| {
            if sidecar.exists() && load_metadata(&sidecar).is_none() {
                return Err("下载元数据路径已有其他文件".into());
            }
            // Remove the old validator before truncation so an interrupted metadata
            // write can never pair stale identity with newly downloaded bytes.
            if load_metadata(&sidecar).is_some() {
                fs::remove_file(&sidecar).map_err(|e| e.to_string())?;
            }
            let file = open_file(&part, append)?;
            if let Some(validator) = remote_validator {
                let meta = ResumeMetadata {
                    format: METADATA_FORMAT.into(),
                    source: url.into(),
                    validator,
                    total,
                };
                let bytes = serde_json::to_vec(&meta).map_err(|e| e.to_string())?;
                let mut temporary =
                    tempfile::NamedTempFile::new_in(sidecar.parent().ok_or("下载元数据目录为空")?)
                        .map_err(|e| e.to_string())?;
                temporary.write_all(&bytes).map_err(|e| e.to_string())?;
                temporary.as_file().sync_all().map_err(|e| e.to_string())?;
                temporary
                    .persist_noclobber(&sidecar)
                    .map_err(|e| e.to_string())?;
            }
            Ok(file)
        })?;
        let mut downloaded = if append { offset } else { 0 };
        let initial = downloaded;
        let start = Instant::now();
        let mut last_emit = Instant::now();
        while let Some(chunk) = watch.wait(response.chunk(), session).await? {
            let next = downloaded
                .checked_add(chunk.len() as u64)
                .ok_or("下载长度溢出")?;
            if next > total {
                return Err("下载数据超过声明长度".into());
            }
            session.active(|| {
                file.write_all(&chunk)
                    .map_err(|e| format!("写入下载文件失败：{e}"))
            })?;
            downloaded = next;
            watch.last = Instant::now();
            if last_emit.elapsed() >= Duration::from_millis(200) {
                let speed = ((downloaded - initial) as f64
                    / start.elapsed().as_secs_f64().max(0.001)) as u64;
                session.active(|| {
                    progress("download", downloaded, total, speed);
                    Ok(())
                })?;
                last_emit = Instant::now();
            }
        }
        if downloaded != total || downloaded - initial != body_size {
            return Err("下载文件长度与响应不一致".into());
        }
        session.active(|| {
            file.flush().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            no_symlink(dest)?;
            fs::rename(&part, dest).map_err(|e| format!("完成下载文件替换失败：{e}"))?;
            if load_metadata(&sidecar).is_some() {
                let _ = fs::remove_file(&sidecar);
            }
            progress("done", downloaded, total, 0);
            Ok(downloaded)
        })
    }
    .await;
    // IO futures/response/file are already dropped before control completion emits.
    if result.as_ref().err().map(String::as_str) == Some("下载已取消") {
        session.event(|| {
            let _ = fs::remove_file(&part);
            if load_metadata(&sidecar).is_some() {
                let _ = fs::remove_file(&sidecar);
            }
            progress("cancelled", 0, 0, 0);
        });
    } else if result.as_ref().err().map(String::as_str) == Some("下载已暂停") {
        let downloaded = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let total = load_metadata(&sidecar).map(|m| m.total).unwrap_or(0);
        session.event(|| progress("paused", downloaded, total, 0));
    }
    result
}

pub(super) async fn run(
    app: &super::AppHandle,
    registry: &DownloadRegistry,
    network: &NetworkConfig,
    token: &str,
    dest: &Path,
    url: &str,
    id: &str,
    repo: &str,
    file: &str,
) -> Result<super::HfDownloadResult, String> {
    let task = registry.register(id);
    let session = Session {
        registry,
        task: &task,
        id,
    };
    let result = transfer(
        network,
        url,
        token,
        dest,
        &session,
        &|phase, downloaded, total, speed| {
            let percent = if phase == "done" {
                100
            } else if total > 0 {
                (downloaded as f64 / total as f64 * 100.0).min(99.0) as u32
            } else {
                0
            };
            let message = match phase {
                "paused" => "下载已暂停".into(),
                "cancelled" => "下载已取消".into(),
                "done" => "下载完成".into(),
                _ => format!("{downloaded}/{total}"),
            };
            super::emit_model_progress(
                app, id, repo, file, phase, percent, downloaded, total, speed, message,
            );
        },
        Timing::default(),
    )
    .await;
    registry.unregister_if_current(id, &task);
    result.map(|size_bytes| super::HfDownloadResult {
        path: dest.to_string_lossy().into(),
        size_bytes,
    })
}

#[cfg(test)]
mod tests;
