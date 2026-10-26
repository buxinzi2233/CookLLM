//! Linux-only, managed llama.cpp installs. No user-selected directory is replaced.
use super::*;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Component,
    time::{Duration, Instant},
};

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const RELEASE_PAGES: usize = 3;
const RELEASES_PER_PAGE: usize = 30;
static UPDATE_LOCK: Mutex<()> = Mutex::new(());

fn check_cancel(cancel: &impl Fn() -> bool) -> Result<(), String> {
    if cancel() {
        Err("更新已取消".into())
    } else {
        Ok(())
    }
}

fn nightly_number(tag: &str) -> Option<u64> {
    let digits = tag.strip_prefix('b')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn digest_hex(value: &str) -> Option<String> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

mod cuda;

fn trusted_asset(
    item: &serde_json::Value,
    tag: &str,
    backend: &str,
    full: &str,
    limit: u64,
) -> Option<LlamaCppAsset> {
    let name = item["name"].as_str()?;
    let url = item["browser_download_url"].as_str()?;
    if url != format!("https://github.com/{LLAMA_CPP_REPO}/releases/download/{tag}/{name}") {
        return None;
    }
    let sha256 = digest_hex(item["digest"].as_str()?)?;
    let size = item["size"].as_u64()?;
    if size == 0 || size > limit {
        return None;
    }
    Some(LlamaCppAsset {
        backend: backend.into(),
        cuda_version: full.split('.').next().unwrap_or_default().into(),
        cuda_full_version: full.into(),
        file_name: name.into(),
        url: url.into(),
        size,
        sha256: Some(sha256),
    })
}

fn parse_assets(value: &serde_json::Value) -> Vec<LlamaCppAsset> {
    let tag = value["tag_name"].as_str().unwrap_or_default();
    if nightly_number(tag).is_none() || value["draft"].as_bool() == Some(true) {
        return vec![];
    }
    value["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item["name"].as_str()?;
            let (backend, full) = if name == format!("llama-{tag}-bin-ubuntu-x64.tar.gz") {
                ("cpu", "")
            } else if name == format!("llama-{tag}-bin-ubuntu-vulkan-x64.tar.gz") {
                ("vulkan", "")
            } else {
                ("cuda", cuda::engine_version(name, tag)?)
            };
            trusted_asset(item, tag, backend, full, MAX_ARCHIVE)
        })
        .collect()
}

fn latest_nightly<'a>(
    values: &'a [serde_json::Value],
    backend: &str,
    selection: &cuda::Selection,
) -> Option<&'a serde_json::Value> {
    values
        .iter()
        .filter(|v| release_matches(v, backend, selection))
        .max_by_key(|v| nightly_number(v["tag_name"].as_str().unwrap_or_default()).unwrap_or(0))
}

fn release_matches(value: &serde_json::Value, backend: &str, selection: &cuda::Selection) -> bool {
    if backend == "cuda" {
        selection.choose(value, None, None).is_ok()
    } else {
        parse_assets(value)
            .iter()
            .any(|asset| asset.backend == backend)
    }
}

fn trusted_response(url: &reqwest::Url, api: bool) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && if api {
            url.host_str() == Some("api.github.com")
        } else {
            matches!(
                url.host_str(),
                Some(
                    "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            )
        }
}

fn release_json(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<serde_json::Value, String> {
    let response = client
        .get(url)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", HF_UA)
        .timeout(Duration::from_secs(30))
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if !trusted_response(response.url(), true) {
        return Err("发布元数据重定向到非受信地址".into());
    }
    let mut data = Vec::new();
    response
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > 8 * 1024 * 1024 {
        return Err("发布元数据超出大小上限".into());
    }
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

fn fetch_release(
    client: &reqwest::blocking::Client,
    tag: Option<&str>,
    backend: &str,
    selection: &cuda::Selection,
    cancel: &impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    fetch_release_with(tag, backend, selection, cancel, |url| {
        release_json(client, url)
    })
}

fn fetch_release_with(
    tag: Option<&str>,
    backend: &str,
    selection: &cuda::Selection,
    cancel: &impl Fn() -> bool,
    mut fetch: impl FnMut(&str) -> Result<serde_json::Value, String>,
) -> Result<serde_json::Value, String> {
    if let Some(tag) = tag {
        if nightly_number(tag).is_none() {
            return Err("Linux 引擎仅接受 b数字 nightly 标签".into());
        }
        check_cancel(cancel)?;
        let value = fetch(&format!("{}/releases/tags/{tag}", llama_api_base()))?;
        if value["tag_name"].as_str() != Some(tag) || !release_matches(&value, backend, selection) {
            return Err("发布标签或请求后端的 Linux 资产元数据不匹配".into());
        }
        return Ok(value);
    }
    // /latest excludes prereleases and may contain only nightly-tag.txt. Bounded scan.
    let mut values = Vec::new();
    for page in 1..=RELEASE_PAGES {
        check_cancel(cancel)?;
        let value = fetch(&format!(
            "{}/releases?per_page={RELEASES_PER_PAGE}&page={page}",
            llama_api_base()
        ))?;
        let items = value.as_array().ok_or("发布列表格式错误")?;
        values.extend(items.iter().cloned());
        if items.len() < RELEASES_PER_PAGE {
            break;
        }
    }
    latest_nightly(&values, backend, selection)
        .cloned()
        .ok_or_else(|| {
            format!(
                "最近 {} 条发布中未找到带 SHA256 的 Linux {backend} nightly 资产",
                RELEASE_PAGES * RELEASES_PER_PAGE
            )
        })
}

fn backend_gate(backend: &str) -> Result<(), String> {
    if !cfg!(target_arch = "x86_64") {
        return Err("Linux 自动引擎安装仅支持 x86_64；请手动选择引擎".into());
    }
    match backend {
        "cpu" | "vulkan" | "cuda" => Ok(()),
        _ => Err("Linux 自动引擎安装仅支持 CPU/Vulkan/CUDA".into()),
    }
}

pub(super) fn check_update(
    app: &AppHandle,
    backend: Option<String>,
    cuda_version: Option<String>,
) -> Result<LlamaCppRelease, String> {
    let requested = backend.unwrap_or_else(|| detect_hardware().recommended_backend);
    backend_gate(&requested)?;
    let config = read_config(app)?;
    let client = build_net_client(&config.network.unwrap_or_default())?;
    let selection = cuda::Selection::new(&requested, cuda_version.as_deref())?;
    let value = fetch_release(&client, None, &requested, &selection, &|| false)?;
    let assets = parse_assets(&value);
    let matched = if requested == "cuda" {
        Some(selection.choose(&value, None, None)?.0)
    } else {
        assets.iter().find(|a| a.backend == requested).cloned()
    };
    Ok(LlamaCppRelease {
        tag: value["tag_name"].as_str().unwrap_or_default().into(),
        assets,
        cudart_assets: cuda::parse_cudart(&value),
        match_backend: requested,
        match_asset: matched,
    })
}

fn select_asset(
    value: &serde_json::Value,
    backend: &str,
    url: Option<&str>,
    name: Option<&str>,
) -> Result<LlamaCppAsset, String> {
    backend_gate(backend)?;
    let candidates = parse_assets(value);
    let matches: Vec<_> = candidates
        .into_iter()
        .filter(|a| {
            a.backend == backend
                && url.is_none_or(|url| url == a.url)
                && name.is_none_or(|name| name == a.file_name)
        })
        .collect();
    if matches.len() != 1 {
        return Err("下载选择与受信发布元数据不匹配（后端、URL 或文件名）".into());
    }
    Ok(matches.into_iter().next().unwrap())
}

// Keep the async client local to the Linux updater. ProxyEnvironment supplies the same
// snapshot/precedence rules as the existing blocking client; no global client changes.
pub(super) fn download_client(
    network: &NetworkConfig,
    environment: &platform::ProxyEnvironment,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent(HF_UA)
        .connect_timeout(Duration::from_secs(10))
        .no_proxy();
    match network.proxy_mode.as_str() {
        "manual" => {
            let url = network.proxy_url.trim();
            if url.is_empty() {
                return Err("手动代理地址为空".into());
            }
            builder =
                builder.proxy(reqwest::Proxy::all(url).map_err(|e| format!("代理地址无效：{e}"))?);
        }
        "direct" => {}
        "system" => {
            // NO_PROXY=* must bypass literal IPs as well as domain names.
            if !environment
                .no_proxy
                .as_deref()
                .is_some_and(|list| list.split(',').any(|s| s.trim() == "*"))
            {
                for scheme in ["http", "https"] {
                    if let Some(url) = environment.for_scheme(scheme) {
                        let proxy = if scheme == "http" {
                            reqwest::Proxy::http(url)
                        } else {
                            reqwest::Proxy::https(url)
                        }
                        .map_err(|_| format!("{scheme} 环境代理地址无效"))?
                        .no_proxy(
                            environment
                                .no_proxy
                                .as_deref()
                                .and_then(reqwest::NoProxy::from_string),
                        );
                        builder = builder.proxy(proxy);
                    }
                }
            }
        }
        _ => return Err("未知代理模式".into()),
    }
    builder
        .build()
        .map_err(|e| format!("初始化下载客户端失败：{e}"))
}

#[derive(Clone, Copy)]
pub(super) struct DownloadTiming {
    overall: Duration,
    idle: Duration,
    poll: Duration,
}

impl Default for DownloadTiming {
    fn default() -> Self {
        Self {
            overall: Duration::from_secs(3600),
            idle: Duration::from_secs(60),
            poll: Duration::from_millis(50),
        }
    }
}

struct DownloadWatch {
    deadline: Instant,
    last_progress: Instant,
    timing: DownloadTiming,
}

impl DownloadWatch {
    fn new(timing: DownloadTiming) -> Self {
        let now = Instant::now();
        Self {
            deadline: now + timing.overall,
            last_progress: now,
            timing,
        }
    }

    fn check(&self, cancel: &impl Fn() -> bool) -> Result<(), String> {
        check_cancel(cancel)?;
        let now = Instant::now();
        if now >= self.deadline {
            return Err("下载超过总时间上限".into());
        }
        if now.duration_since(self.last_progress) >= self.timing.idle {
            return Err("下载长时间无进度，已超时".into());
        }
        Ok(())
    }

    async fn wait<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, reqwest::Error>>,
        cancel: &impl Fn() -> bool,
    ) -> Result<T, String> {
        // A tick only drops the timeout wrapper, not the pinned request/chunk future.
        // Cancellation/expiry drops that future on return; no spawned readers or writers.
        let mut future = std::pin::pin!(future);
        loop {
            self.check(cancel)?;
            let now = Instant::now();
            let tick = self
                .timing
                .poll
                .min(self.deadline.saturating_duration_since(now))
                .min((self.last_progress + self.timing.idle).saturating_duration_since(now));
            if let Ok(result) = tokio::time::timeout(tick, &mut future).await {
                self.check(cancel)?;
                return result.map_err(|e| format!("引擎下载失败：{e}"));
            }
        }
    }
}

// Private transport boundary lets local HTTP fixtures exercise real reqwest streaming
// without weakening the production URL check. Synchronous file writes happen only here,
// on the calling task; once this future returns there is no task that can keep writing.
pub(super) async fn download_stream(
    request: reqwest::RequestBuilder,
    expected_size: u64,
    path: &Path,
    cancel: &impl Fn() -> bool,
    progress: impl Fn(u64, u64),
    timing: DownloadTiming,
    validate_url: impl Fn(&reqwest::Url) -> bool,
) -> Result<(), String> {
    download_stream_limited(
        request,
        expected_size,
        MAX_ARCHIVE,
        path,
        cancel,
        progress,
        timing,
        validate_url,
    )
    .await
}

async fn download_stream_limited(
    request: reqwest::RequestBuilder,
    expected_size: u64,
    limit: u64,
    path: &Path,
    cancel: &impl Fn() -> bool,
    progress: impl Fn(u64, u64),
    timing: DownloadTiming,
    validate_url: impl Fn(&reqwest::Url) -> bool,
) -> Result<(), String> {
    if expected_size == 0 || expected_size > limit || limit > cuda::MAX_RUNTIME_ARCHIVE {
        return Err("下载资产大小超出上限".into());
    }
    let mut watch = DownloadWatch::new(timing);
    let mut response = watch
        .wait(request.send(), cancel)
        .await?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if !validate_url(response.url()) {
        return Err("引擎下载重定向到非受信地址".into());
    }
    if response
        .content_length()
        .is_some_and(|n| n != expected_size)
    {
        return Err("下载长度与发布元数据不匹配".into());
    }
    watch.last_progress = Instant::now();
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut total = 0u64;
    while let Some(chunk) = watch.wait(response.chunk(), cancel).await? {
        if chunk.is_empty() {
            continue;
        }
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or("下载大小溢出")?;
        if total > expected_size || total > limit {
            return Err("下载超过发布资产大小".into());
        }
        out.write_all(&chunk).map_err(|e| e.to_string())?;
        watch.last_progress = Instant::now();
        progress(total, expected_size);
    }
    watch.check(cancel)?;
    if total != expected_size {
        return Err("下载内容不完整".into());
    }
    out.sync_all().map_err(|e| e.to_string())?;
    check_cancel(cancel)
}

fn download_archive(
    network: &NetworkConfig,
    asset: &LlamaCppAsset,
    path: &Path,
    cancel: &impl Fn() -> bool,
    progress: impl Fn(u64, u64),
) -> Result<(), String> {
    check_cancel(cancel)?;
    let url = reqwest::Url::parse(&asset.url).map_err(|e| e.to_string())?;
    if !trusted_response(&url, false) {
        return Err("引擎下载地址非受信上游".into());
    }
    let environment = platform::ProxyEnvironment::read(|key| std::env::var(key).ok());
    tauri::async_runtime::block_on(async {
        let client = download_client(network, &environment)?;
        download_stream_limited(
            client.get(url),
            asset.size,
            cuda::asset_limit(asset),
            path,
            cancel,
            progress,
            DownloadTiming::default(),
            |url| trusted_response(url, false),
        )
        .await
    })
}

fn verify_archive(
    path: &Path,
    asset: &LlamaCppAsset,
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    let expected = asset.sha256.as_deref().ok_or("发布资产缺少 SHA256")?;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != asset.size
        || asset.size == 0
        || asset.size > cuda::asset_limit(asset)
    {
        return Err("归档大小与发布元数据不匹配".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != expected {
        return Err("引擎归档 SHA256 校验失败".into());
    }
    Ok(())
}

#[derive(Clone)]
enum Node {
    Directory,
    File,
    Link(PathBuf),
}

fn archive_path(path: &Path, root: &str) -> Result<PathBuf, String> {
    if path.as_os_str().len() > 4096
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.components().next() != Some(Component::Normal(root.as_ref()))
    {
        return Err(format!("归档路径越界或格式异常：{}", path.display()));
    }
    Ok(path.to_path_buf())
}

fn link_target(path: &Path, target: &Path, root: &str) -> Result<PathBuf, String> {
    let mut joined = path.parent().ok_or("链接缺少父目录")?.to_path_buf();
    for part in target.components() {
        match part {
            Component::Normal(s) => joined.push(s),
            Component::CurDir => {}
            Component::ParentDir if joined.components().count() > 1 => {
                joined.pop();
            }
            _ => return Err("归档链接越界或使用绝对路径".into()),
        }
    }
    archive_path(&joined, root)
}

fn extract_archive(
    path: &Path,
    dest: &Path,
    root: &str,
    cancel: &impl Fn() -> bool,
    max_entries: usize,
    max_size: u64,
) -> Result<PathBuf, String> {
    extract_archive_kind(path, dest, root, cancel, max_entries, max_size, None)
}

fn extract_archive_kind(
    path: &Path,
    dest: &Path,
    root: &str,
    cancel: &impl Fn() -> bool,
    max_entries: usize,
    max_size: u64,
    runtime_major: Option<&str>,
) -> Result<PathBuf, String> {
    check_cancel(cancel)?;
    // The caller owns an empty private TempDir. Never unpack into an existing engine.
    fs::create_dir(dest).map_err(|e| e.to_string())?;
    let decoder = GzDecoder::new(fs::File::open(path).map_err(|e| e.to_string())?);
    let mut archive = tar::Archive::new(decoder.take(max_size + max_entries as u64 * 1024 + 1024));
    let mut nodes = BTreeMap::new();
    let mut total = 0u64;
    for (index, entry) in archive
        .entries()
        .map_err(|e| e.to_string())?
        .raw(true)
        .enumerate()
    {
        check_cancel(cancel)?;
        if index >= max_entries {
            return Err("归档成员数量超出上限".into());
        }
        let mut entry = entry.map_err(|e| e.to_string())?;
        let relative = archive_path(&entry.path().map_err(|e| e.to_string())?, root)?;
        if nodes.contains_key(&relative) {
            return Err("归档包含重复路径".into());
        }
        let kind = entry.header().entry_type();
        if let Some(major) = runtime_major {
            cuda::runtime_member(&relative, root, major, kind)?;
        }
        let node = if kind.is_dir() {
            Node::Directory
        } else if kind.is_file() {
            Node::File
        } else if kind.is_symlink() {
            Node::Link(
                entry
                    .link_name()
                    .map_err(|e| e.to_string())?
                    .ok_or("归档链接缺少目标")?
                    .into_owned(),
            )
        } else {
            return Err("归档包含硬链接或特殊成员类型".into());
        };
        for parent in relative
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            if nodes
                .get(parent)
                .is_some_and(|n| !matches!(n, Node::Directory))
            {
                return Err("归档文件经过链接或非目录父路径".into());
            }
        }
        if !matches!(node, Node::Directory)
            && nodes.keys().any(|p: &PathBuf| p.starts_with(&relative))
        {
            return Err("归档链接或文件覆盖已有目录".into());
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or("归档大小溢出")?;
        if total > max_size {
            return Err("归档展开大小超出上限".into());
        }
        let output = dest.join(&relative);
        match &node {
            Node::Directory => {
                if size != 0 {
                    return Err("归档目录含异常数据".into());
                }
                fs::create_dir_all(&output).map_err(|e| e.to_string())?;
                fs::set_permissions(&output, fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            Node::File => {
                fs::create_dir_all(output.parent().ok_or("归档文件缺少父目录")?)
                    .map_err(|e| e.to_string())?;
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .mode(0o600)
                    .open(&output)
                    .map_err(|e| e.to_string())?;
                let mut written = 0;
                let mut buffer = [0u8; 128 * 1024];
                loop {
                    check_cancel(cancel)?;
                    let n = entry.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                    written += n as u64;
                }
                if written != size {
                    return Err("归档文件数据截断".into());
                }
                // Preserve executable bits; strip setuid/setgid/sticky and group/world writes.
                let mode = entry.header().mode().map_err(|e| e.to_string())? & 0o755;
                out.set_permissions(fs::Permissions::from_mode(mode))
                    .map_err(|e| e.to_string())?;
            }
            Node::Link(target) => {
                if size != 0 || target.as_os_str().is_empty() {
                    return Err("归档链接格式异常".into());
                }
                link_target(&relative, target, root)?;
            }
        }
        nodes.insert(relative, node);
    }
    // Validate the entire link graph before creating any link. Directory links are excluded.
    for (path, node) in &nodes {
        if let Node::Link(target) = node {
            let mut current = link_target(path, target, root)?;
            let mut seen = BTreeSet::from([path.clone()]);
            loop {
                check_cancel(cancel)?;
                if !seen.insert(current.clone()) {
                    return Err("归档包含循环链接".into());
                }
                match nodes.get(&current) {
                    Some(Node::File) => break,
                    Some(Node::Link(next)) => current = link_target(&current, next, root)?,
                    _ => return Err("归档链接目标缺失或不是普通文件".into()),
                }
            }
        }
    }
    for (path, node) in &nodes {
        check_cancel(cancel)?;
        if let Node::Link(target) = node {
            let output = dest.join(path);
            fs::create_dir_all(output.parent().ok_or("链接父目录缺失")?)
                .map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(target, output).map_err(|e| e.to_string())?;
        }
    }
    if runtime_major.is_some() {
        return Ok(dest.join(root));
    }
    let server = dest.join(root).join("llama-server");
    if !matches!(
        nodes.get(&PathBuf::from(root).join("llama-server")),
        Some(Node::File)
    ) || !platform::executable_file(&server)
    {
        return Err("归档根目录缺少可执行的 llama-server 普通文件".into());
    }
    Ok(dest.join(root))
}

// Both production transport and offline fixtures pass through this same preparation boundary.
// The caller owns staging on the managed filesystem; every failure is discarded with staging.
fn prepare_packages(
    staging: &Path,
    tag: &str,
    engine: &LlamaCppAsset,
    runtime: Option<&LlamaCppAsset>,
    cancel: &impl Fn() -> bool,
    mut fetch: impl FnMut(&LlamaCppAsset, &Path, u64, u64) -> Result<(), String>,
) -> Result<PathBuf, String> {
    check_cancel(cancel)?;
    if engine.backend == "cuda" {
        let runtime = runtime.ok_or("CUDA 引擎缺少配套运行库")?;
        if cuda::engine_version(&engine.file_name, tag) != Some(engine.cuda_full_version.as_str())
            || runtime.file_name != format!("cudart-{}", engine.file_name)
            || runtime.cuda_full_version != engine.cuda_full_version
            || runtime.cuda_version != engine.cuda_version
            || runtime.backend != "cuda"
        {
            return Err("CUDA 引擎/运行库 tag、完整版本或架构不匹配".into());
        }
    } else if runtime.is_some() {
        return Err("非 CUDA 引擎附带运行库".into());
    }
    let total = engine
        .size
        .checked_add(runtime.map_or(0, |r| r.size))
        .ok_or("配套下载大小溢出")?;
    let archive = staging.join("engine.tar.gz");
    fetch(engine, &archive, 0, total)?;
    verify_archive(&archive, engine, cancel)?;
    check_cancel(cancel)?;
    let source = extract_archive(
        &archive,
        &staging.join("engine-extract"),
        &format!("llama-{tag}"),
        cancel,
        MAX_ENTRIES,
        MAX_EXPANDED,
    )?;
    if let Some(runtime) = runtime {
        // Same cancellation flag before contacting the second package.
        check_cancel(cancel)?;
        let archive = staging.join("runtime.tar.gz");
        fetch(runtime, &archive, engine.size, total)?;
        verify_archive(&archive, runtime, cancel)?;
        let root = runtime
            .file_name
            .strip_suffix(".tar.gz")
            .ok_or("运行库归档格式错误")?;
        let extracted = extract_archive_kind(
            &archive,
            &staging.join("runtime-extract"),
            root,
            cancel,
            32,
            MAX_EXPANDED,
            Some(&runtime.cuda_version),
        )?;
        cuda::merge_runtime(&source, &extracted, &runtime.cuda_version, cancel)?;
        let mut meta = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(source.join("cuda_version.txt"))
            .map_err(|e| e.to_string())?;
        writeln!(meta, "{}", engine.cuda_full_version).map_err(|e| e.to_string())?;
    }
    check_cancel(cancel)?;
    Ok(source)
}

pub(super) fn managed_launch_env(
    command: &mut Command,
    server: &Path,
    managed: &Path,
) -> Result<(), String> {
    let Ok(server) = server.canonicalize() else {
        return Ok(());
    };
    let Ok(managed) = managed.canonicalize() else {
        return Ok(());
    };
    let Some(engine) = server.parent() else {
        return Ok(());
    };
    let Some(container) = engine.parent() else {
        return Ok(());
    };
    if container.parent() == Some(managed.as_path())
        && engine.file_name().is_some_and(|n| n == "engine")
        && container
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("version-"))
        && fs::read_to_string(engine.join("backend.txt")).is_ok_and(|v| v.trim() == "cuda")
    {
        cuda::runtime_env(command, engine)?;
    }
    Ok(())
}

fn health_check(server: &Path, cancel: &impl Fn() -> bool) -> Result<(), String> {
    check_cancel(cancel)?;
    // Only called after trusted metadata + digest verification, never on a user's path.
    let output = platform::checked_tool(
        Command::new(server)
            .arg("--version")
            .current_dir(server.parent().ok_or("引擎父目录缺失")?)
            .env_remove("LD_PRELOAD")
            .env_remove("LD_LIBRARY_PATH"),
        None,
        Duration::from_secs(15),
    )?;
    check_cancel(cancel)?;
    if output.stdout.is_empty() && output.stderr.is_empty() {
        return Err("引擎健康探测未返回版本信息".into());
    }
    Ok(())
}

// A fresh private version container is removed on every failure, including config persistence.
// The previous engine and config are never moved or deleted by this transaction.
fn install_checked(
    source: &Path,
    managed: &Path,
    cancel: &impl Fn() -> bool,
    health: impl FnOnce(&Path) -> Result<(), String>,
    commit: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    check_cancel(cancel)?;
    let final_dir = tempfile::Builder::new()
        .prefix("version-")
        .tempdir_in(managed)
        .map_err(|e| e.to_string())?;
    let target = final_dir.path().join("engine");
    fs::rename(source, &target).map_err(|e| format!("安装新引擎失败：{e}"))?;
    let server = target.join("llama-server");
    health(&server)?;
    check_cancel(cancel)?;
    commit(&server)?;
    // Commit is the cancellation linearization point: after it, report success, not rollback.
    let _ = final_dir.keep();
    Ok(server)
}

pub(super) fn save_config_atomic(path: &Path, config: &AppConfig) -> Result<(), String> {
    let contents = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    atomic_write(path, &contents)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or("配置父目录缺失")?)
        .map_err(|e| e.to_string())?;
    temporary.write_all(contents).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    // POSIX atomic replacement: an error leaves the old file intact. No fallible work after commit.
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn ensure_stopped(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<ProcessState>();
    let mut guard = state.0.lock().map_err(|_| "进程状态锁已损坏")?;
    if let Some(managed) = guard.as_mut() {
        if !managed_child_exited(managed).map_err(|e| e.to_string())? {
            return Err("llama-server 正在运行，请先停止服务后更新引擎".into());
        }
    }
    Ok(())
}

// Keep the user's selection policy separate from the actual installed engine metadata.
fn update_installed_config(
    updated: &mut AppConfig,
    server: &Path,
    version: &str,
    asset: &LlamaCppAsset,
    cuda_preference: &str,
) {
    let path = server.to_string_lossy().to_string();
    updated.server_path = path.clone();
    updated.llamacpp_dir = server.parent().map(|p| p.to_string_lossy().to_string());
    updated.llama_backend = Some(asset.backend.clone());
    if asset.backend == "cuda" {
        let preference = cuda_preference.trim();
        updated.llama_cuda_version = Some(
            if preference.is_empty() {
                "auto"
            } else {
                preference
            }
            .into(),
        );
    }
    let active = updated.active_engine_id.clone();
    let engine = if let Some(id) = active {
        updated.engines.iter_mut().find(|e| e.id == id)
    } else {
        updated.engines.first_mut()
    };
    if let Some(engine) = engine {
        engine.path = path;
        engine.backend = Some(asset.backend.clone());
        engine.cuda_version = (asset.backend == "cuda").then(|| asset.cuda_full_version.clone());
        engine.version = Some(version.to_string());
    }
}

pub(super) fn download(
    app: AppHandle,
    backend: String,
    cuda_version: Option<String>,
    url: Option<String>,
    name: Option<String>,
    tag: Option<String>,
) -> Result<String, String> {
    backend_gate(&backend)?;
    let _exclusive = UPDATE_LOCK
        .try_lock()
        .map_err(|_| "已有 Linux 引擎更新任务正在运行")?;
    let cancel_flag = CancelFlag(&LLAMACPP_UPDATE_CANCEL);
    let _guard = CancelGuard(cancel_flag);
    let cancel = || cancel_flag.cancelled();
    ensure_stopped(&app)?;
    let config = read_config(&app)?;
    let network = config.network.clone().unwrap_or_default();
    let client = build_net_client(&network)?;
    let selection = cuda::Selection::new(&backend, cuda_version.as_deref())?;
    let value = fetch_release(&client, tag.as_deref(), &backend, &selection, &cancel)?;
    let version = value["tag_name"].as_str().ok_or("发布版本缺失")?;
    let (asset, runtime) = if backend == "cuda" {
        let (engine, runtime) = selection.choose(&value, url.as_deref(), name.as_deref())?;
        (engine, Some(runtime))
    } else {
        (
            select_asset(&value, &backend, url.as_deref(), name.as_deref())?,
            None,
        )
    };
    let managed = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("llamacpp-managed");
    fs::create_dir_all(&managed).map_err(|e| e.to_string())?;
    if fs::symlink_metadata(&managed)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("托管引擎根目录应为普通目录".into());
    }
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&managed)
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let total = asset.size + runtime.as_ref().map_or(0, |r| r.size);
    emit_download_progress(
        &app,
        "download",
        0,
        0,
        total,
        0,
        "下载 Linux 引擎与配套运行库",
    );
    let source = prepare_packages(
        staging.path(),
        version,
        &asset,
        runtime.as_ref(),
        &cancel,
        |item, path, offset, total| {
            download_archive(&network, item, path, &cancel, |done, _| {
                let cumulative = offset + done;
                let speed = (cumulative as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64;
                emit_download_progress(
                    &app,
                    "download",
                    (cumulative * 100 / total) as u32,
                    cumulative,
                    total,
                    speed,
                    "下载 Linux 引擎与配套运行库",
                );
            })?;
            let cumulative = offset + item.size;
            emit_download_progress(
                &app,
                "extract",
                (cumulative * 100 / total) as u32,
                cumulative,
                total,
                0,
                "校验 SHA256 并受限解压 Linux 引擎/运行库",
            );
            Ok(())
        },
    )?;
    for (name, value) in [("version.txt", version), ("backend.txt", backend.as_str())] {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(source.join(name))
            .map_err(|e| e.to_string())?;
        writeln!(file, "{value}").map_err(|e| e.to_string())?;
    }
    emit_download_progress(
        &app,
        "install",
        100,
        total,
        total,
        0,
        "安装至托管新目录并执行健康探测",
    );
    let server = install_checked(
        &source,
        &managed,
        &cancel,
        |p| {
            if backend == "cuda" {
                cuda::health(p, &cancel)
            } else {
                health_check(p, &cancel)
            }
        },
        |p| {
            ensure_stopped(&app)?;
            let mut updated = read_config(&app)?;
            // Preserve concurrent user changes rather than silently switching a different engine.
            if updated.server_path != config.server_path
                || updated.active_engine_id != config.active_engine_id
            {
                return Err("更新期间所选引擎已变化，请重试".into());
            }
            update_installed_config(&mut updated, p, version, &asset, &selection.request);
            check_cancel(&cancel)?;
            save_config(app.clone(), updated)
        },
    )?;
    emit_download_progress(
        &app,
        "done",
        100,
        total,
        total,
        0,
        "Linux 引擎更新完成；旧引擎保留",
    );
    Ok(server.to_string_lossy().into_owned())
}

#[cfg(test)]
mod download_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod cuda_tests;
