//! Strict Linux CUDA asset pairing and private runtime environment.
use super::*;

pub(super) const MAX_RUNTIME_ARCHIVE: u64 = 1024 * 1024 * 1024;
const RUNTIME_DIR: &str = "cuda-runtime";

pub(super) fn version(value: &str) -> Option<(u32, u32)> {
    let (major, minor) = value.split_once('.')?;
    if major.is_empty()
        || minor.is_empty()
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let parsed = (major.parse().ok()?, minor.parse().ok()?);
    (value == format!("{}.{}", parsed.0, parsed.1)).then_some(parsed)
}

pub(super) fn engine_version<'a>(name: &'a str, tag: &str) -> Option<&'a str> {
    let full = name
        .strip_prefix(&format!("llama-{tag}-bin-ubuntu-cuda-"))?
        .strip_suffix("-x64.tar.gz")?;
    let (major, _) = version(full)?;
    matches!(major, 12 | 13).then_some(full)
}

pub(super) fn asset_limit(asset: &LlamaCppAsset) -> u64 {
    if asset.file_name.starts_with("cudart-llama-") {
        MAX_RUNTIME_ARCHIVE
    } else {
        MAX_ARCHIVE
    }
}

pub(super) fn parse_cudart(value: &serde_json::Value) -> Vec<LlamaCppAsset> {
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
            let full = engine_version(name.strip_prefix("cudart-")?, tag)?;
            trusted_asset(item, tag, "cuda", full, MAX_RUNTIME_ARCHIVE)
        })
        .collect()
}

#[derive(Default)]
pub(super) struct Selection {
    pub request: String,
    pub driver: Option<(u32, u32)>,
}
impl Selection {
    pub fn new(backend: &str, request: Option<&str>) -> Result<Self, String> {
        if backend != "cuda" {
            return Ok(Self::default());
        }
        let request = request.unwrap_or("auto").trim();
        if !matches!(request, "" | "auto" | "12" | "13")
            && !version(request).is_some_and(|v| matches!(v.0, 12 | 13))
        {
            return Err("CUDA 版本应为 auto、12、13 或完整 major.minor".into());
        }
        Ok(Self {
            request: request.into(),
            driver: Some(driver_capability()?),
        })
    }
    pub fn accepts(&self, asset: &LlamaCppAsset) -> bool {
        let Some(full) = version(&asset.cuda_full_version) else {
            return false;
        };
        let Some(driver) = self.driver else {
            return false;
        };
        full <= driver
            && match self.request.as_str() {
                "" | "auto" => true,
                "12" | "13" => asset.cuda_version == self.request,
                requested => asset.cuda_full_version == requested,
            }
    }
    pub fn choose(
        &self,
        value: &serde_json::Value,
        url: Option<&str>,
        name: Option<&str>,
    ) -> Result<(LlamaCppAsset, LlamaCppAsset), String> {
        let runtimes = parse_cudart(value);
        let mut pairs: Vec<_> = parse_assets(value)
            .into_iter()
            .filter(|a| a.backend == "cuda" && self.accepts(a))
            .filter(|a| url.is_none_or(|u| a.url == u) && name.is_none_or(|n| a.file_name == n))
            .filter_map(|a| {
                let paired: Vec<_> = runtimes
                    .iter()
                    .filter(|r| r.cuda_full_version == a.cuda_full_version)
                    .collect();
                (paired.len() == 1).then(|| (a, paired[0].clone()))
            })
            .collect();
        // Duplicate metadata is ambiguous even when all fields happen to match.
        let mut names = BTreeSet::new();
        if pairs
            .iter()
            .any(|(a, _)| !names.insert(a.file_name.clone()))
        {
            return Err("CUDA 引擎资产元数据重复".into());
        }
        pairs.sort_by_key(|(a, _)| {
            let v = version(&a.cuda_full_version).unwrap();
            // Auto prefers the compatible CUDA 12 family, then CUDA 13. Within a family use newest minor.
            (v.0 != 12, std::cmp::Reverse(v.1))
        });
        pairs.into_iter().next().ok_or_else(|| {
            "未找到匹配请求版本及驱动上限的完整 Linux CUDA 引擎/运行库配对；请手动选择引擎".into()
        })
    }
}

pub(super) fn driver_xml(text: &str) -> Option<(u32, u32)> {
    for key in ["cuda_umd_version", "cuda_version"] {
        let open = format!("<{key}>");
        let close = format!("</{key}>");
        if let Some(rest) = text.split_once(&open).map(|(_, rest)| rest) {
            if let Some((value, _)) = rest.split_once(&close) {
                if let Some(v) = version(value.trim()) {
                    return Some(v);
                }
            }
        }
    }
    None
}
fn driver_capability() -> Result<(u32, u32), String> {
    // Absolute system paths only; never consult a user-controlled PATH or a bundled tool.
    let tool = ["/usr/bin/nvidia-smi", "/bin/nvidia-smi"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| platform::executable_file(p))
        .ok_or("驱动 CUDA 上限未知：系统 nvidia-smi 缺失；请手动选择引擎")?;
    let output = platform::checked_tool(
        Command::new(tool)
            .args(["-q", "-x"])
            .env("PATH", "/usr/bin:/bin")
            .env_remove("LD_PRELOAD")
            .env_remove("LD_LIBRARY_PATH"),
        None,
        Duration::from_secs(5),
    )
    .map_err(|e| format!("驱动 CUDA 上限探测失败：{e}；请手动选择引擎"))?;
    driver_xml(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "驱动 CUDA 上限未知；请手动选择引擎".into())
}

pub(super) fn libraries(major: &str) -> [String; 3] {
    [
        format!("libcudart.so.{major}"),
        format!("libcublas.so.{major}"),
        format!("libcublasLt.so.{major}"),
    ]
}
fn copyright(name: &str) -> bool {
    matches!(
        name,
        "LICENSE"
            | "LICENSE.txt"
            | "LICENSE.TXT"
            | "COPYING"
            | "NOTICE"
            | "NOTICE.txt"
            | "EULA.txt"
            | "CUDA_EULA.txt"
    )
}
pub(super) fn runtime_member(
    relative: &Path,
    root: &str,
    major: &str,
    kind: tar::EntryType,
) -> Result<(), String> {
    let parts: Vec<_> = relative.components().collect();
    if relative == Path::new(root) && kind.is_dir() {
        return Ok(());
    }
    if parts.len() != 2 || !kind.is_file() {
        return Err("CUDA 运行库归档仅接受严格根下的普通 soname/版权文件".into());
    }
    let name = relative
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("运行库名称格式错误")?;
    if !libraries(major).iter().any(|lib| lib == name) && !copyright(name) {
        return Err(format!("CUDA 运行库含非许可成员：{name}"));
    }
    Ok(())
}

pub(super) fn merge_runtime(
    source: &Path,
    runtime: &Path,
    major: &str,
    cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    let target = source.join(RUNTIME_DIR);
    if fs::symlink_metadata(&target).is_ok() {
        return Err("CUDA 运行库目标已存在".into());
    }
    let libs = libraries(major);
    // Complete preflight before moving anything, including engine sibling collisions.
    for lib in &libs {
        let metadata = fs::symlink_metadata(runtime.join(lib))
            .map_err(|_| format!("CUDA 配套运行库缺少 {lib}"))?;
        if !metadata.is_file()
            || metadata.len() == 0
            || fs::symlink_metadata(source.join(lib)).is_ok()
        {
            return Err(format!("CUDA 运行库成员异常或覆盖引擎文件：{lib}"));
        }
    }
    for entry in fs::read_dir(runtime).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        runtime_member(
            &PathBuf::from("runtime").join(entry.file_name()),
            "runtime",
            major,
            if entry.file_type().map_err(|e| e.to_string())?.is_file() {
                tar::EntryType::Regular
            } else {
                tar::EntryType::Symlink
            },
        )?;
    }
    fs::rename(runtime, &target).map_err(|e| e.to_string())?;
    for lib in libs {
        check_cancel(cancel)?;
        std::os::unix::fs::symlink(Path::new(RUNTIME_DIR).join(&lib), source.join(lib))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn runtime_env(command: &mut Command, engine: &Path) -> Result<(), String> {
    command
        .env_remove("LD_PRELOAD")
        .env_remove("LD_LIBRARY_PATH");
    let full = fs::read_to_string(engine.join("cuda_version.txt")).map_err(|e| e.to_string())?;
    let major = version(full.trim())
        .ok_or("托管 CUDA 版本元数据格式错误")?
        .0
        .to_string();
    let runtime = engine.join(RUNTIME_DIR);
    if !fs::symlink_metadata(&runtime)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("托管 CUDA 运行库目录格式错误".into());
    }
    for lib in libraries(&major) {
        if !fs::symlink_metadata(runtime.join(lib))
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err("托管 CUDA 运行库应为普通文件".into());
        }
    }
    command.env("LD_LIBRARY_PATH", runtime);
    Ok(())
}

pub(super) fn cuda_device(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        let Some((name, description)) = line.split_once(':') else {
            return false;
        };
        name.strip_prefix("CUDA")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && !description.trim().is_empty()
    })
}

pub(super) fn health(server: &Path, cancel: &impl Fn() -> bool) -> Result<(), String> {
    check_cancel(cancel)?;
    let mut command = Command::new(server);
    let engine = server.parent().ok_or("引擎父目录缺失")?;
    runtime_env(&mut command, engine)?;
    let output = platform::checked_tool(
        command.arg("--list-devices").current_dir(engine),
        None,
        Duration::from_secs(30),
    )?;
    check_cancel(cancel)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !cuda_device(&text) {
        return Err(format!(
            "CUDA 健康探测未发现真实 CUDA 设备（CPU fallback）：{text}"
        ));
    }
    Ok(())
}
