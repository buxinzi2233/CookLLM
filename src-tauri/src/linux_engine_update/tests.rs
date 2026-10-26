use super::*;
use flate2::{write::GzEncoder, Compression};
use serde_json::json;
use std::cell::Cell;

pub(super) fn release(tag: &str, suffixes: &[&str]) -> serde_json::Value {
    let assets: Vec<_> = suffixes.iter().map(|suffix| {
        let name = format!("llama-{tag}-bin-ubuntu-{suffix}.tar.gz");
        json!({"name": name, "size": 12, "digest": format!("sha256:{}", "a".repeat(64)),
            "browser_download_url": format!("https://github.com/{LLAMA_CPP_REPO}/releases/download/{tag}/{name}")})
    }).collect();
    json!({"tag_name": tag, "draft": false, "prerelease": true, "assets": assets})
}

#[test]
fn routes_cpu_vulkan_and_excludes_other_architectures_backends() {
    let value = release(
        "b12345",
        &[
            "x64",
            "vulkan-x64",
            "arm64",
            "vulkan-arm64",
            "s390x",
            "cuda-12.8-x64",
            "cuda-13.4-x64",
            "sycl-fp16-x64",
            "openvino-2026.4.1-x64",
            "rocm-10.0-x64",
        ],
    );
    let assets = parse_assets(&value);
    assert_eq!(
        assets
            .iter()
            .map(|a| a.backend.as_str())
            .collect::<Vec<_>>(),
        ["cpu", "vulkan", "cuda", "cuda"]
    );
    assert_eq!(assets[0].sha256.as_deref(), Some("a".repeat(64).as_str()));
    assert!(backend_gate("cuda").is_ok());
}

#[test]
fn latest_selects_highest_numeric_nightly_with_assets_not_stable_marker() {
    let values = vec![
        release("v0.6.0", &["x64"]),
        release("b9", &["x64"]),
        release("b11", &["arm64"]),
        release("b10", &["vulkan-x64"]),
        release("b12/escape", &["x64"]),
    ];
    assert_eq!(
        latest_nightly(&values, "vulkan", &cuda::Selection::default()).unwrap()["tag_name"],
        "b10"
    );
    assert_eq!(
        latest_nightly(&values, "cpu", &cuda::Selection::default()).unwrap()["tag_name"],
        "b9"
    );
    assert!(nightly_number("b").is_none());
}

#[test]
fn newer_cpu_only_nightly_does_not_hide_older_vulkan() {
    let releases = json!([
        release("b12", &["x64"]),
        release("b11", &["x64", "vulkan-x64"])
    ]);
    for (backend, tag) in [("cpu", "b12"), ("vulkan", "b11")] {
        let value = fetch_release_with(
            None,
            backend,
            &cuda::Selection::default(),
            &|| false,
            |url| {
                assert!(url.ends_with("/releases?per_page=30&page=1"));
                Ok(releases.clone())
            },
        )
        .unwrap();
        assert_eq!(value["tag_name"], tag);
    }
}

#[test]
fn explicit_nightly_missing_backend_errors_without_downgrade() {
    let mut requests = 0;
    let result = fetch_release_with(
        Some("b12"),
        "vulkan",
        &cuda::Selection::default(),
        &|| false,
        |url| {
            requests += 1;
            assert!(url.ends_with("/releases/tags/b12"));
            Ok(release("b12", &["x64"]))
        },
    );
    assert!(result.unwrap_err().contains("请求后端"));
    assert_eq!(requests, 1);
}

#[test]
fn metadata_requires_official_url_digest_and_bounded_size() {
    let mut value = release("b1", &["x64"]);
    value["assets"][0]["browser_download_url"] = json!("https://evil.example/llama.tar.gz");
    assert!(parse_assets(&value).is_empty());
    value = release("b1", &["x64"]);
    value["assets"][0]["digest"] = json!("sha256:123");
    assert!(parse_assets(&value).is_empty());
    value = release("b1", &["x64"]);
    value["assets"][0]["size"] = json!(MAX_ARCHIVE + 1);
    assert!(parse_assets(&value).is_empty());
    value = release("b1", &["x64"]);
    value["draft"] = json!(true);
    assert!(parse_assets(&value).is_empty());
}

#[test]
fn download_selection_matches_each_supplied_field_and_backend() {
    let value = release("b1", &["x64", "vulkan-x64"]);
    let asset = select_asset(&value, "cpu", None, None).unwrap();
    assert!(select_asset(&value, "cpu", Some(&asset.url), Some(&asset.file_name)).is_ok());
    assert!(select_asset(&value, "cpu", Some("file:///tmp/fake"), None).is_err());
    assert!(select_asset(&value, "cpu", None, Some("../../fake")).is_err());
    assert!(select_asset(&value, "vulkan", Some(&asset.url), Some(&asset.file_name)).is_err());
    assert!(select_asset(&value, "cuda", None, None).is_err());
}

#[test]
fn redirected_responses_require_https_and_allowlisted_host() {
    for value in [
        "http://github.com/a",
        "https://github.com.evil.test/a",
        "https://user@github.com/a",
        "https://github.com:444/a",
    ] {
        assert!(!trusted_response(
            &reqwest::Url::parse(value).unwrap(),
            false
        ));
    }
    assert!(trusted_response(
        &reqwest::Url::parse("https://release-assets.githubusercontent.com/a?token=1").unwrap(),
        false
    ));
    assert!(!trusted_response(
        &reqwest::Url::parse("https://release-assets.githubusercontent.com/a").unwrap(),
        true
    ));
}

// Raw headers deliberately allow malformed traversal paths that tar::Builder rejects.
pub(super) struct Entry<'a> {
    pub(super) path: &'a str,
    pub(super) kind: u8,
    pub(super) mode: u32,
    pub(super) data: &'a [u8],
    pub(super) link: &'a str,
}
pub(super) fn regular(path: &str) -> Entry<'_> {
    Entry {
        path,
        kind: b'0',
        mode: 0o755,
        data: b"test",
        link: "",
    }
}
fn link<'a>(path: &'a str, target: &'a str) -> Entry<'a> {
    Entry {
        path,
        kind: b'2',
        mode: 0o777,
        data: b"",
        link: target,
    }
}
pub(super) fn archive(dir: &Path, entries: &[Entry<'_>]) -> PathBuf {
    let path = dir.join("fixture.tar.gz");
    let encoder = GzEncoder::new(fs::File::create(&path).unwrap(), Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..entry.path.len()].copy_from_slice(entry.path.as_bytes());
        header.set_entry_type(tar::EntryType::new(entry.kind));
        header.set_mode(entry.mode);
        header.set_size(entry.data.len() as u64);
        if !entry.link.is_empty() {
            header.set_link_name(entry.link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, entry.data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    path
}
fn extract(dir: &Path, entries: &[Entry<'_>]) -> Result<PathBuf, String> {
    extract_archive(
        &archive(dir, entries),
        &dir.join("extract"),
        "llama-b1",
        &|| false,
        MAX_ENTRIES,
        MAX_EXPANDED,
    )
}
pub(super) fn signed_asset(path: &Path) -> LlamaCppAsset {
    let data = fs::read(path).unwrap();
    LlamaCppAsset {
        backend: "cpu".into(),
        cuda_version: String::new(),
        cuda_full_version: String::new(),
        file_name: "fixture.tar.gz".into(),
        url: String::new(),
        size: data.len() as u64,
        sha256: Some(format!("{:x}", Sha256::digest(&data))),
    }
}

#[test]
fn checksum_mismatch_and_truncation_fail_before_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let path = archive(temp.path(), &[regular("llama-b1/llama-server")]);
    let mut asset = signed_asset(&path);
    verify_archive(&path, &asset, &|| false).unwrap();
    asset.sha256 = Some("0".repeat(64));
    assert!(verify_archive(&path, &asset, &|| false)
        .unwrap_err()
        .contains("SHA256"));
    asset = signed_asset(&path);
    fs::write(&path, b"truncated").unwrap();
    assert!(verify_archive(&path, &asset, &|| false).is_err());
}

#[test]
fn permissions_and_forward_multilevel_relative_symlinks_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let mut server = regular("llama-b1/llama-server");
    server.mode = 0o6755;
    let root = extract(
        temp.path(),
        &[
            server,
            link("llama-b1/libllama.so", "libllama.so.0"),
            link("llama-b1/libllama.so.0", "libllama.so.0.6.0"),
            regular("llama-b1/libllama.so.0.6.0"),
        ],
    )
    .unwrap();
    assert_eq!(
        fs::metadata(root.join("llama-server"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    assert_eq!(
        fs::read_link(root.join("libllama.so")).unwrap(),
        PathBuf::from("libllama.so.0")
    );
    assert_eq!(fs::read(root.join("libllama.so")).unwrap(), b"test");
}

#[test]
fn absolute_traversal_and_wrong_root_paths_are_rejected() {
    for path in [
        "/tmp/escape",
        "llama-b1/../../escape",
        "../escape",
        "other/llama-server",
    ] {
        let temp = tempfile::tempdir().unwrap();
        assert!(extract(temp.path(), &[regular(path)]).is_err(), "{path}");
    }
}

#[test]
fn links_reject_escape_absolute_dangling_cycles_and_directory_targets() {
    for target in ["../../escape", "/tmp/escape", "missing", "lib.so", "."] {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            extract(
                temp.path(),
                &[
                    regular("llama-b1/llama-server"),
                    link("llama-b1/lib.so", target)
                ]
            )
            .is_err(),
            "{target}"
        );
        assert!(fs::symlink_metadata(temp.path().join("extract/llama-b1/lib.so")).is_err());
    }
    let temp = tempfile::tempdir().unwrap();
    assert!(extract(
        temp.path(),
        &[
            regular("llama-b1/llama-server"),
            link("llama-b1/a", "b"),
            link("llama-b1/b", "a")
        ]
    )
    .is_err());
}

#[test]
fn duplicate_files_symlink_overwrite_and_writes_under_links_are_rejected() {
    let cases = vec![
        vec![
            regular("llama-b1/llama-server"),
            regular("llama-b1/llama-server"),
        ],
        vec![
            regular("llama-b1/llama-server"),
            link("llama-b1/llama-server", "target"),
        ],
        vec![
            link("llama-b1/alias", "target"),
            regular("llama-b1/alias/llama-server"),
        ],
        vec![
            regular("llama-b1/alias/llama-server"),
            link("llama-b1/alias", "target"),
        ],
        vec![
            link("llama-b1/llama-server", "target"),
            regular("llama-b1/llama-server"),
        ],
    ];
    for entries in cases {
        let temp = tempfile::tempdir().unwrap();
        assert!(extract(temp.path(), &entries).is_err());
    }
}

#[test]
fn hardlinks_devices_fifo_and_extended_headers_are_rejected() {
    for kind in [b'1', b'3', b'4', b'6', b'x', b'L'] {
        let temp = tempfile::tempdir().unwrap();
        let entry = Entry {
            path: "llama-b1/special",
            kind,
            mode: 0o644,
            data: b"",
            link: "",
        };
        assert!(extract(temp.path(), &[regular("llama-b1/llama-server"), entry]).is_err());
    }
}

#[test]
fn entry_count_and_unpacked_size_limits_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let path = archive(
        temp.path(),
        &[regular("llama-b1/llama-server"), regular("llama-b1/lib.so")],
    );
    assert!(extract_archive(
        &path,
        &temp.path().join("count"),
        "llama-b1",
        &|| false,
        1,
        100
    )
    .unwrap_err()
    .contains("数量"));
    assert!(extract_archive(
        &path,
        &temp.path().join("size"),
        "llama-b1",
        &|| false,
        10,
        3
    )
    .unwrap_err()
    .contains("大小"));
}

#[test]
fn cancellation_interrupts_hash_and_mid_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let path = archive(
        temp.path(),
        &[regular("llama-b1/llama-server"), regular("llama-b1/lib.so")],
    );
    assert!(verify_archive(&path, &signed_asset(&path), &|| true)
        .unwrap_err()
        .contains("取消"));
    let calls = Cell::new(0);
    let cancelled = || {
        calls.set(calls.get() + 1);
        calls.get() > 3
    };
    assert!(extract_archive(
        &path,
        &temp.path().join("extract"),
        "llama-b1",
        &cancelled,
        MAX_ENTRIES,
        MAX_EXPANDED
    )
    .unwrap_err()
    .contains("取消"));
    assert!(!temp.path().join("extract/llama-b1/lib.so").exists());
}

pub(super) fn old_install(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let old = root.join("user-selected");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("llama-server"), b"old server").unwrap();
    let config = root.join("config.json");
    fs::write(&config, b"old config").unwrap();
    let managed = root.join("managed");
    fs::create_dir(&managed).unwrap();
    (old, config, managed)
}
pub(super) fn assert_old(old: &Path, config: &Path, managed: &Path) {
    assert_eq!(fs::read(old.join("llama-server")).unwrap(), b"old server");
    assert_eq!(fs::read(config).unwrap(), b"old config");
    assert_eq!(fs::read_dir(managed).unwrap().count(), 0);
}

#[test]
fn install_rename_failure_preserves_old_engine_and_config() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    assert!(install_checked(
        &temp.path().join("missing-source"),
        &managed,
        &|| false,
        |_| panic!("health should not run"),
        |_| panic!("commit should not run")
    )
    .is_err());
    assert_old(&old, &config, &managed);
}

#[test]
fn health_failure_preserves_old_engine_and_config_and_removes_new_version() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    let source = extract(temp.path(), &[regular("llama-b1/llama-server")]).unwrap();
    assert!(install_checked(
        &source,
        &managed,
        &|| false,
        |server| health_check(server, &|| false),
        |_| panic!("commit should not run")
    )
    .is_err());
    assert_old(&old, &config, &managed);
}

#[test]
fn config_save_failure_rolls_back_new_version_and_preserves_old() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    let source = extract(temp.path(), &[regular("llama-b1/llama-server")]).unwrap();
    assert!(install_checked(
        &source,
        &managed,
        &|| false,
        |_| Ok(()),
        |_| atomic_write(&config.join("invalid-parent"), b"new config")
    )
    .is_err());
    assert_old(&old, &config, &managed);
}

#[test]
fn cancelled_after_health_leaves_old_config_and_no_new_install() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    let source = extract(temp.path(), &[regular("llama-b1/llama-server")]).unwrap();
    let cancelled = Cell::new(false);
    assert!(install_checked(
        &source,
        &managed,
        &|| cancelled.get(),
        |_| {
            cancelled.set(true);
            Ok(())
        },
        |_| panic!("commit should not run")
    )
    .is_err());
    assert_old(&old, &config, &managed);
}

#[test]
fn successful_install_is_in_new_managed_directory_and_keeps_old_engine() {
    let temp = tempfile::tempdir().unwrap();
    let (old, config, managed) = old_install(temp.path());
    let source = extract(temp.path(), &[regular("llama-b1/llama-server")]).unwrap();
    let server = install_checked(
        &source,
        &managed,
        &|| false,
        |_| Ok(()),
        |p| atomic_write(&config, p.to_string_lossy().as_bytes()),
    )
    .unwrap();
    assert!(server.starts_with(&managed));
    assert_eq!(fs::read(&server).unwrap(), b"test");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        server.to_string_lossy()
    );
    assert_eq!(fs::read(old.join("llama-server")).unwrap(), b"old server");
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 1);
}

#[test]
fn verified_fixture_runs_version_health_check() {
    let temp = tempfile::tempdir().unwrap();
    let server = Entry {
        path: "llama-b1/llama-server",
        kind: b'0',
        mode: 0o755,
        data: b"#!/bin/sh\n[ \"$1\" = --version ] || exit 2\necho 'version: fixture'\n",
        link: "",
    };
    let archive = archive(temp.path(), &[server]);
    verify_archive(&archive, &signed_asset(&archive), &|| false).unwrap();
    let source = extract_archive(
        &archive,
        &temp.path().join("extract"),
        "llama-b1",
        &|| false,
        MAX_ENTRIES,
        MAX_EXPANDED,
    )
    .unwrap();
    health_check(&source.join("llama-server"), &|| false).unwrap();
}

#[test]
#[ignore = "requires COOKLLM_LINUX_RELEASE_FIXTURE directory with release.json and downloaded CPU asset"]
fn real_upstream_cpu_digest_extract_and_version() {
    let dir = PathBuf::from(
        std::env::var_os("COOKLLM_LINUX_RELEASE_FIXTURE").expect("fixture directory"),
    );
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("release.json")).unwrap()).unwrap();
    let asset = select_asset(&value, "cpu", None, None).unwrap();
    let path = dir.join(&asset.file_name);
    verify_archive(&path, &asset, &|| false).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = extract_archive(
        &path,
        &temp.path().join("extract"),
        &format!("llama-{}", value["tag_name"].as_str().unwrap()),
        &|| false,
        MAX_ENTRIES,
        MAX_EXPANDED,
    )
    .unwrap();
    health_check(&source.join("llama-server"), &|| false).unwrap();
    let output = platform::checked_tool(
        Command::new(source.join("llama-server")).arg("--version"),
        None,
        Duration::from_secs(15),
    )
    .unwrap();
    println!(
        "asset={} sha256={}\n{}{}",
        asset.file_name,
        asset.sha256.unwrap(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
