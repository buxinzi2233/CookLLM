use super::tests::{archive, assert_old, old_install, regular, release, signed_asset, Entry};
use super::*;
use serde_json::json;
use std::{cell::Cell, net::TcpListener, os::unix::process::CommandExt};

fn paired(tag: &str, versions: &[&str]) -> serde_json::Value {
    let suffixes: Vec<_> = versions.iter().map(|v| format!("cuda-{v}-x64")).collect();
    let refs: Vec<_> = suffixes.iter().map(String::as_str).collect();
    let mut value = release(tag, &refs);
    let engines = value["assets"].as_array().unwrap().clone();
    for mut runtime in engines {
        let name = format!("cudart-{}", runtime["name"].as_str().unwrap());
        runtime["name"] = json!(name);
        runtime["browser_download_url"] = json!(format!(
            "https://github.com/{LLAMA_CPP_REPO}/releases/download/{tag}/{name}"
        ));
        value["assets"].as_array_mut().unwrap().push(runtime);
    }
    value
}
fn selection(request: &str, driver: Option<(u32, u32)>) -> cuda::Selection {
    cuda::Selection {
        request: request.into(),
        driver,
    }
}

#[test]
fn cuda_pair_tag_arch_version_digest_url_and_size_are_strict() {
    let valid = paired("b1", &["12.8", "13.4"]);
    let select = selection("auto", Some((13, 4)));
    assert_eq!(
        select
            .choose(&valid, None, None)
            .unwrap()
            .0
            .cuda_full_version,
        "12.8"
    );
    for (field, invalid) in [
        (
            "name",
            json!("cudart-llama-b2-bin-ubuntu-cuda-12.8-x64.tar.gz"),
        ),
        (
            "name",
            json!("cudart-llama-b1-bin-ubuntu-cuda-12.9-x64.tar.gz"),
        ),
        (
            "name",
            json!("cudart-llama-b1-bin-ubuntu-cuda-12.8-arm64.tar.gz"),
        ),
        ("digest", json!("sha256:bad")),
        (
            "browser_download_url",
            json!("https://github.com/evil/runtime.tar.gz"),
        ),
        ("size", json!(cuda::MAX_RUNTIME_ARCHIVE + 1)),
        ("size", json!(0)),
    ] {
        let mut value = paired("b1", &["12.8"]);
        value["assets"][1][field] = invalid;
        assert!(select.choose(&value, None, None).is_err(), "{field}");
    }
    let mut value = paired("b1", &["12.8"]);
    value["assets"][1]["size"] = json!(MAX_ARCHIVE + 1);
    assert!(select.choose(&value, None, None).is_ok());
    value["assets"][0]["size"] = json!(MAX_ARCHIVE + 1);
    assert!(select.choose(&value, None, None).is_err());
    let mut value = paired("b1", &["12.8"]);
    let duplicate = value["assets"][1].clone();
    value["assets"].as_array_mut().unwrap().push(duplicate);
    assert!(select.choose(&value, None, None).is_err());
}

#[test]
fn explicit_cuda_is_exact_or_major_never_crosses_major_and_unknown_driver_fails() {
    let value = paired("b1", &["12.8", "12.9", "13.4"]);
    for (request, expected) in [
        ("auto", "12.9"),
        ("12", "12.9"),
        ("12.8", "12.8"),
        ("13", "13.4"),
    ] {
        let (engine, runtime) = selection(request, Some((13, 4)))
            .choose(&value, None, None)
            .unwrap();
        assert_eq!(engine.cuda_full_version, expected);
        assert_eq!(runtime.file_name, format!("cudart-{}", engine.file_name));
    }
    assert!(selection("12.7", Some((13, 4)))
        .choose(&value, None, None)
        .is_err());
    assert!(selection("13", Some((12, 9)))
        .choose(&value, None, None)
        .is_err());
    assert!(selection("auto", None).choose(&value, None, None).is_err());
    assert_eq!(
        selection("auto", Some((12, 8)))
            .choose(&value, None, None)
            .unwrap()
            .0
            .cuda_full_version,
        "12.8"
    );
    let (engine, _) = selection("12.8", Some((13, 4)))
        .choose(&value, None, None)
        .unwrap();
    assert!(selection("12", Some((13, 4)))
        .choose(&value, Some("https://evil.test/x"), None)
        .is_err());
    assert!(selection("13", Some((13, 4)))
        .choose(&value, Some(&engine.url), Some(&engine.file_name))
        .is_err());
    assert_eq!(
        cuda::driver_xml("<cuda_version>13.4</cuda_version>"),
        Some((13, 4))
    );
    assert_eq!(cuda::driver_xml("<cuda_version>N/A</cuda_version>"), None);
    assert!(cuda::Selection::new("cuda", Some("12.8junk")).is_err());
    assert!(!cuda::cuda_device("Available devices:\nCPU: host\nCUDA0:"));
    assert!(cuda::cuda_device(
        "Available devices:\n  CUDA0: NVIDIA fixture (8192 MiB)"
    ));
}

#[test]
fn newer_incomplete_or_wrong_requested_cuda_nightly_does_not_hide_old_pair() {
    let values = vec![
        paired("b8", &["12.8"]),
        release("b10", &["cuda-12.8-x64"]),
        paired("b9", &["13.4"]),
    ];
    let select = selection("12", Some((13, 4)));
    assert_eq!(
        latest_nightly(&values, "cuda", &select).unwrap()["tag_name"],
        "b8"
    );
    let fetched =
        fetch_release_with(None, "cuda", &select, &|| false, |_| Ok(json!(values))).unwrap();
    assert_eq!(fetched["tag_name"], "b8");
    assert!(
        fetch_release_with(Some("b9"), "cuda", &select, &|| false, |_| Ok(paired(
            "b9",
            &["13.4"]
        )))
        .is_err()
    );
}

const RUNTIME_ROOT: &str = "cudart-llama-b1-bin-ubuntu-cuda-12.8-x64";
fn fixtures(root: &Path, script: &[u8]) -> (LlamaCppAsset, LlamaCppAsset, PathBuf, PathBuf) {
    let ed = root.join("engine-input");
    let rd = root.join("runtime-input");
    fs::create_dir(&ed).unwrap();
    fs::create_dir(&rd).unwrap();
    let ep = archive(
        &ed,
        &[
            Entry {
                path: "llama-b1/llama-server",
                kind: b'0',
                mode: 0o755,
                data: script,
                link: "",
            },
            regular("llama-b1/libggml-cuda.so"),
        ],
    );
    let paths: Vec<_> = cuda::libraries("12")
        .iter()
        .map(|n| format!("{RUNTIME_ROOT}/{n}"))
        .collect();
    let entries: Vec<_> = paths.iter().map(|p| regular(p)).collect();
    let rp = archive(&rd, &entries);
    let mut engine = signed_asset(&ep);
    let mut runtime = signed_asset(&rp);
    for asset in [&mut engine, &mut runtime] {
        asset.backend = "cuda".into();
        asset.cuda_version = "12".into();
        asset.cuda_full_version = "12.8".into();
    }
    engine.file_name = "llama-b1-bin-ubuntu-cuda-12.8-x64.tar.gz".into();
    runtime.file_name = format!("cudart-{}", engine.file_name);
    (engine, runtime, ep, rp)
}
fn prepare_fixture(
    staging: &Path,
    engine: &LlamaCppAsset,
    runtime: &LlamaCppAsset,
    ep: &Path,
    rp: &Path,
    cancel: &impl Fn() -> bool,
) -> Result<PathBuf, String> {
    prepare_packages(
        staging,
        "b1",
        engine,
        Some(runtime),
        cancel,
        |asset, dest, _, _| {
            fs::copy(
                if asset.file_name.starts_with("cudart-") {
                    rp
                } else {
                    ep
                },
                dest,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )
}

#[test]
fn installed_cuda_preference_survives_restart_and_selects_future_minor() {
    for request in ["auto", "12", "12.8", "", "  "] {
        let temp = tempfile::tempdir().unwrap();
        let managed = temp.path().join("managed");
        fs::create_dir(&managed).unwrap();
        let staging = tempfile::tempdir_in(&managed).unwrap();
        let (engine, runtime, ep, rp) = fixtures(temp.path(), b"#!/bin/sh\necho fixture\n");
        let source =
            prepare_fixture(staging.path(), &engine, &runtime, &ep, &rp, &|| false).unwrap();
        let mut config = AppConfig::default();
        config.engines = vec![serde_json::from_value(
            json!({"id":"selected", "name":"keep name", "path":"old"}),
        )
        .unwrap()];
        config.active_engine_id = Some("selected".into());
        let config_path = temp.path().join("config.json");
        let server = install_checked(
            &source,
            &managed,
            &|| false,
            |_| Ok(()),
            |server| {
                update_installed_config(&mut config, server, "b1", &engine, request);
                save_config_atomic(&config_path, &config)
            },
        )
        .unwrap();
        // Reload the persisted snapshot exactly as a restarted app would.
        let saved: AppConfig = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        let preference = if request.trim().is_empty() {
            "auto"
        } else {
            request
        };
        assert_eq!(saved.llama_cuda_version.as_deref(), Some(preference));
        assert_eq!(saved.engines[0].cuda_version.as_deref(), Some("12.8"));
        assert_eq!(saved.engines[0].name, "keep name");
        assert_eq!(
            fs::read_to_string(server.parent().unwrap().join("cuda_version.txt"))
                .unwrap()
                .trim(),
            "12.8"
        );
        let select = selection(saved.llama_cuda_version.as_deref().unwrap(), Some((13, 4)));
        let releases = vec![paired("b2", &["12.9"]), paired("b1", &["12.8"])];
        let next =
            fetch_release_with(None, "cuda", &select, &|| false, |_| Ok(json!(releases))).unwrap();
        let actual = select
            .choose(&next, None, None)
            .unwrap()
            .0
            .cuda_full_version;
        assert_eq!(actual, if preference == "12.8" { "12.8" } else { "12.9" });
    }
}

#[test]
fn cpu_and_vulkan_install_preserve_cuda_preference_but_clear_engine_actual() {
    for preference in [None, Some("auto"), Some("12"), Some("13"), Some("12.8")] {
        for backend in ["cpu", "vulkan"] {
            let mut config = AppConfig::default();
            config.llama_cuda_version = preference.map(str::to_owned);
            config.engines = vec![serde_json::from_value(
                json!({"id":"selected", "name":"keep", "path":"old", "cudaVersion":"12.8"}),
            )
            .unwrap()];
            let mut asset = parse_assets(&release("b2", &["x64", "vulkan-x64"]))
                .into_iter()
                .find(|a| a.backend == backend)
                .unwrap();
            asset.cuda_full_version.clear();
            update_installed_config(
                &mut config,
                Path::new("/fixture/engine/llama-server"),
                "b2",
                &asset,
                "auto",
            );
            assert_eq!(config.llama_cuda_version.as_deref(), preference);
            assert_eq!(config.engines[0].cuda_version, None);
            assert_eq!(config.engines[0].backend.as_deref(), Some(backend));
        }
    }
}

#[test]
fn preference_based_download_still_locks_fresh_url_name_and_tag() {
    let value = paired("b1", &["12.8", "12.9"]);
    for request in ["auto", "12"] {
        let select = selection(request, Some((13, 4)));
        let exact = parse_assets(&value)
            .into_iter()
            .find(|a| a.cuda_full_version == "12.8")
            .unwrap();
        assert_eq!(
            select
                .choose(&value, Some(&exact.url), Some(&exact.file_name))
                .unwrap()
                .0
                .cuda_full_version,
            "12.8"
        );
        let wrong_name = exact.file_name.replace("12.8", "12.9");
        assert!(select
            .choose(&value, Some(&exact.url), Some(&wrong_name))
            .is_err());
        let wrong_url = exact.url.replace("b1", "b2");
        assert!(select
            .choose(&value, Some(&wrong_url), Some(&exact.file_name))
            .is_err());
        assert!(
            fetch_release_with(Some("b1"), "cuda", &select, &|| false, |_| Ok(paired(
                "b2",
                &["12.8"]
            )))
            .is_err()
        );
    }
}

#[test]
fn cuda_runtime_members_missing_libraries_collisions_and_copyright() {
    for name in [
        "libggml-cuda.so",
        "llama-server",
        "sub/libcudart.so.12",
        "libcudart.so.13",
    ] {
        assert!(cuda::runtime_member(
            &Path::new(RUNTIME_ROOT).join(name),
            RUNTIME_ROOT,
            "12",
            tar::EntryType::Regular
        )
        .is_err());
    }
    assert!(cuda::runtime_member(
        &Path::new(RUNTIME_ROOT).join("libcudart.so.12"),
        RUNTIME_ROOT,
        "12",
        tar::EntryType::Symlink
    )
    .is_err());
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("engine");
    let runtime = temp.path().join("runtime");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&runtime).unwrap();
    fs::write(source.join("libggml-cuda.so"), b"engine").unwrap();
    assert!(cuda::merge_runtime(&source, &runtime, "12", &|| false).is_err());
    for lib in cuda::libraries("12") {
        fs::write(runtime.join(lib), b"runtime").unwrap();
    }
    fs::write(source.join("libcudart.so.12"), b"engine collision").unwrap();
    assert!(cuda::merge_runtime(&source, &runtime, "12", &|| false).is_err());
    assert_eq!(
        fs::read(source.join("libcudart.so.12")).unwrap(),
        b"engine collision"
    );
    fs::remove_file(source.join("libcudart.so.12")).unwrap();
    fs::write(runtime.join("LICENSE"), b"copyright").unwrap();
    cuda::merge_runtime(&source, &runtime, "12", &|| false).unwrap();
    assert_eq!(
        fs::read(source.join("cuda-runtime/LICENSE")).unwrap(),
        b"copyright"
    );
    assert_eq!(fs::read(source.join("libggml-cuda.so")).unwrap(), b"engine");
}

#[test]
fn cuda_pair_cancel_between_packages_and_bad_runtime_digest_preserve_old() {
    for bad_digest in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (old, config, managed) = old_install(temp.path());
        let staging = tempfile::tempdir_in(&managed).unwrap();
        let (engine, mut runtime, ep, rp) = fixtures(temp.path(), b"#!/bin/sh\necho fixture\n");
        let cancel = Cell::new(false);
        let fetched = Cell::new(0);
        if bad_digest {
            runtime.sha256 = Some("0".repeat(64));
        }
        let result = prepare_packages(
            staging.path(),
            "b1",
            &engine,
            Some(&runtime),
            &|| cancel.get(),
            |a, dest, offset, total| {
                assert_eq!(total, engine.size + runtime.size);
                assert_eq!(offset, if fetched.get() == 0 { 0 } else { engine.size });
                if !bad_digest && offset > 0 {
                    // Cancellation arrives after engine hash/extraction, before runtime transport IO.
                    assert!(staging
                        .path()
                        .join("engine-extract/llama-b1/llama-server")
                        .exists());
                    cancel.set(true);
                }
                check_cancel(&|| cancel.get())?;
                fetched.set(fetched.get() + 1);
                fs::copy(
                    if a.file_name.starts_with("cudart-") {
                        &rp
                    } else {
                        &ep
                    },
                    dest,
                )
                .unwrap();
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(fetched.get(), if bad_digest { 2 } else { 1 });
        drop(staging);
        assert_old(&old, &config, &managed);
    }
}

#[test]
fn cuda_health_cpu_fallback_rolls_back_and_launch_environment_is_managed_only() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    let staging = tempfile::tempdir_in(&managed).unwrap();
    let (engine, runtime, ep, rp) =
        fixtures(temp.path(), b"#!/bin/sh\necho 'version ok; CPU: fixture'\n");
    let source = prepare_fixture(staging.path(), &engine, &runtime, &ep, &rp, &|| false).unwrap();
    assert!(install_checked(
        &source,
        &managed,
        &|| false,
        |p| cuda::health(p, &|| false),
        |_| panic!("commit must not run")
    )
    .is_err());
    drop(staging);
    assert_old(&old, &config, &managed);
    let version = managed.join("version-fixture/engine");
    fs::create_dir_all(&version).unwrap();
    fs::write(version.join("llama-server"), b"fixture").unwrap();
    fs::write(version.join("backend.txt"), b"cuda\n").unwrap();
    fs::write(version.join("cuda_version.txt"), b"12.8\n").unwrap();
    fs::create_dir(version.join("cuda-runtime")).unwrap();
    for lib in cuda::libraries("12") {
        fs::write(version.join("cuda-runtime").join(lib), b"runtime").unwrap();
    }
    let mut command = Command::new(version.join("llama-server"));
    command
        .env("LD_PRELOAD", "fixture")
        .env("LD_LIBRARY_PATH", "/opt/resolve");
    managed_launch_env(&mut command, &version.join("llama-server"), &managed).unwrap();
    let env: BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
        env[std::ffi::OsStr::new("LD_LIBRARY_PATH")],
        Some(version.join("cuda-runtime").as_os_str())
    );
    assert_eq!(env[std::ffi::OsStr::new("LD_PRELOAD")], None);
    let mut manual = Command::new(old.join("llama-server"));
    managed_launch_env(&mut manual, &old.join("llama-server"), &managed).unwrap();
    assert_eq!(manual.get_envs().count(), 0);
}

struct Running(std::process::Child);
impl Drop for Running {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = platform::stop_group(&mut self.0, Duration::from_millis(100));
        }
    }
}
#[test]
#[ignore = "requires local official CUDA release fixture, NVIDIA driver and COOKLLM_CUDA_MODEL; no network downloads"]
fn real_upstream_cuda_pair_health_http_chat_and_cleanup() {
    let fixture =
        PathBuf::from(std::env::var_os("COOKLLM_LINUX_RELEASE_FIXTURE").expect("fixture"));
    let model = PathBuf::from(std::env::var_os("COOKLLM_CUDA_MODEL").expect("small model"));
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.join("release.json")).unwrap()).unwrap();
    let select = cuda::Selection::new("cuda", Some("12.8")).unwrap();
    let (engine, runtime) = select.choose(&value, None, None).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let managed = temp.path().join("llamacpp-managed");
    fs::create_dir(&managed).unwrap();
    let staging = tempfile::tempdir_in(&managed).unwrap();
    let source = prepare_packages(
        staging.path(),
        value["tag_name"].as_str().unwrap(),
        &engine,
        Some(&runtime),
        &|| false,
        |a, p, offset, total| {
            println!(
                "package={} bytes={} sha256={} offset={offset} total={total}",
                a.file_name,
                a.size,
                a.sha256.as_deref().unwrap()
            );
            // Read-only fixture reuse; same-filesystem hardlink when possible, copy across filesystems.
            fs::hard_link(fixture.join(&a.file_name), p)
                .or_else(|_| fs::copy(fixture.join(&a.file_name), p).map(|_| ()))
                .map_err(|e| e.to_string())
        },
    )
    .unwrap();
    fs::write(source.join("backend.txt"), b"cuda\n").unwrap();
    let server = install_checked(
        &source,
        &managed,
        &|| false,
        |p| cuda::health(p, &|| false),
        |_| Ok(()),
    )
    .unwrap();
    let origin_only = platform::checked_tool(
        Command::new(&server)
            .arg("--list-devices")
            .env_remove("LD_PRELOAD")
            .env_remove("LD_LIBRARY_PATH"),
        None,
        Duration::from_secs(30),
    );
    match origin_only {
        Ok(output) => println!(
            "origin_only={}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => println!("origin_only_probe={error}"),
    }
    let mut devices = Command::new(&server);
    managed_launch_env(&mut devices, &server, &managed).unwrap();
    let output =
        platform::checked_tool(devices.arg("--list-devices"), None, Duration::from_secs(30))
            .unwrap();
    println!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&model).unwrap();
    assert_eq!(bytes.len(), 1185376);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "270cba1bd5109f42d03350f60406024560464db173c0e387d91f0426d3bd256d"
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let log = temp.path().join("server.log");
    let out = fs::File::create(&log).unwrap();
    let mut command = Command::new(&server);
    managed_launch_env(&mut command, &server, &managed).unwrap();
    command
        .args(["-m"])
        .arg(&model)
        .args([
            "-c",
            "512",
            "-t",
            "2",
            "-ngl",
            "99",
            "-n",
            "16",
            "-lv",
            "4",
            "--host",
            "127.0.0.1",
            "--port",
        ])
        .arg(port.to_string())
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .process_group(0);
    let mut child = Running(command.spawn().unwrap());
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if client
            .get(format!("{base}/health"))
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "server exited: {}",
            fs::read_to_string(&log).unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "health timeout: {}",
            fs::read_to_string(&log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let response=client.post(format!("{base}/v1/chat/completions")).json(&json!({"messages":[{"role":"user","content":"Once upon a time"}],"max_tokens":16,"temperature":0,"stream":false}))
        .send().unwrap().error_for_status().unwrap();
    let body: serde_json::Value = response.json().unwrap();
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "{body}"
    );
    println!("http_health=200 chat={body}");
    platform::stop_group(&mut child.0, Duration::from_millis(500)).unwrap();
    assert!(child.0.try_wait().unwrap().is_some());
    assert!(
        TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "port not released"
    );
    let text = fs::read_to_string(&log).unwrap();
    println!("{text}\nprocess exited; port {port} released");
    if let Some(dir) = std::env::var_os("COOKLLM_CUDA_LOG_DIR") {
        fs::copy(&log, PathBuf::from(dir).join("real-server.log")).unwrap();
    }
    assert!(
        text.contains("offloaded 6/6 layers to GPU"),
        "expected real CUDA layer offload"
    );
}
