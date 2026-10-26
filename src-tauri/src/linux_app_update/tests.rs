use super::*;
use serde_json::{json, Value};

fn release(name: &str) -> Value {
    json!({"tag_name":"v9.8.7", "assets":[{"name":name, "browser_download_url":format!("https://github.com/{APP_GITHUB_REPO}/releases/download/v9.8.7/{name}"),"size":64,"digest":format!("sha256:{}","a".repeat(64))}]})
}
#[test]
fn running_executable_checks_permissions_without_opening_for_write() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("sleep-fixture");
    fs::copy("/bin/sleep", &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new(&executable).arg("10").spawn().unwrap();
    let write_error = fs::OpenOptions::new().write(true).open(&executable).err();
    let readable_and_writable = regular_owned(&executable, true).is_ok();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        write_error.and_then(|e| e.raw_os_error()),
        Some(libc::ETXTBSY)
    );
    assert!(readable_and_writable);
}

#[test]
fn kernel_mount_binding_requires_unique_owned_readonly_fuse() {
    let image = Path::new("/tmp/CookLLM isolated.AppImage");
    let appdir = Path::new("/tmp/.mount_CookLLfixture");
    let mount = "361 64 0:92 / /tmp/.mount_CookLLfixture ro,nosuid,nodev - fuse.CookLLM\\040isolated.AppImage CookLLM\\040isolated.AppImage ro,user_id=1000,group_id=1000";
    assert!(image_mount_binding(mount, image, appdir, 1000));
    assert!(!image_mount_binding(mount, image, appdir, 1001));
    assert!(!image_mount_binding(
        mount,
        image,
        Path::new("/tmp/.mount_wrong"),
        1000
    ));
    assert!(!image_mount_binding(
        &mount.replace("ro,nosuid", "rw,nosuid"),
        image,
        appdir,
        1000
    ));
    assert!(!image_mount_binding(
        &format!("{mount}\n{mount}"),
        image,
        appdir,
        1000
    ));
    assert!(!image_mount_binding(
        &mount.replace("fuse.CookLLM", "ext4.CookLLM"),
        image,
        appdir,
        1000
    ));
    assert!(!active_image_runtime(
        Path::new("/tmp/nonexistent.AppImage"),
        appdir
    ));
}

#[test]
fn kernel_mount_binding_allows_old_image_at_another_mountpoint() {
    let image = Path::new("/tmp/CookLLM.AppImage");
    let appdir = Path::new("/tmp/.mount_new");
    let current = "362 64 0:93 / /tmp/.mount_new ro,nosuid,nodev - fuse.CookLLM.AppImage CookLLM.AppImage ro,user_id=1000,group_id=1000";
    let previous = "361 64 0:92 / /tmp/.mount_old ro,nosuid,nodev - fuse.CookLLM.AppImage CookLLM.AppImage ro,user_id=1000,group_id=1000";
    for mounts in [format!("{previous}\n{current}"), format!("{current}\n{previous}")] {
        assert!(image_mount_binding(&mounts, image, appdir, 1000));
    }
    assert!(!image_mount_binding(previous, image, appdir, 1000));
    assert!(!image_mount_binding(&format!("{current}\n{current}"), image, appdir, 1000));
    let covered = current.replace("fuse.CookLLM.AppImage CookLLM.AppImage", "fuse.Other.AppImage Other.AppImage");
    assert!(!image_mount_binding(&format!("{current}\n{covered}"), image, appdir, 1000));
    assert!(!image_mount_binding(&current.replace("user_id=1000", "user_id=1001"), image, appdir, 1000));
    assert!(!image_mount_binding(&current.replace("ro,nosuid", "rw,nosuid"), image, appdir, 1000));
}

#[test]
fn system_package_identity_uses_actual_bundle_name_and_exact_owner() {
    assert!(package_identity(Kind::Deb, "cook-llm amd64"));
    assert!(package_identity(Kind::Rpm, "cook-llm x86_64"));
    for bad in [
        "cookllm amd64",
        "other amd64",
        "cook-llm arm64",
        "cook-llm amd64 extra",
    ] {
        assert!(!package_identity(Kind::Deb, bad));
    }
    assert!(!package_identity(Kind::Rpm, "cook-llm aarch64"));
    let exe = Path::new("/usr/bin/cookllm");
    assert!(deb_owner(exe, "cook-llm: /usr/bin/cookllm"));
    assert!(deb_owner(exe, "cook-llm:amd64: /usr/bin/cookllm"));
    assert!(!deb_owner(exe, "other: /usr/bin/cookllm"));
    assert!(!deb_owner(exe, "cook-llm: /tmp/cookllm"));
    assert!(!deb_owner(
        exe,
        "cook-llm: /usr/bin/cookllm\nother: /usr/bin/cookllm"
    ));
}

#[test]
fn selects_only_exact_package_and_architecture() {
    for (name, kind) in [
        ("CookLLM_9.8.7_amd64.AppImage", Kind::AppImage),
        ("CookLLM_9.8.7_amd64.deb", Kind::Deb),
        ("CookLLM-9.8.7-1.x86_64.rpm", Kind::Rpm),
    ] {
        let value = release(name);
        assert_eq!(select(&value, kind).unwrap().name, name);
        for other in [Kind::AppImage, Kind::Deb, Kind::Rpm, Kind::Manual] {
            if kind != other {
                assert!(select(&value, other).is_err());
            }
        }
    }
    for name in [
        "CookLLM_9.8.7_arm64.AppImage",
        "CookLLM_9.8.7_i686.AppImage",
        "CookLLM_9.8.7.AppImage",
        "CookLLM_9.8.7_x86_64evil.AppImage",
        "CookLLM_9.8.7_amd64.exe",
        "other_amd64.AppImage",
    ] {
        assert!(select(&release(name), Kind::AppImage).is_err(), "{name}");
    }
}
#[test]
fn missing_digest_size_wrong_repo_and_ambiguity_are_manual() {
    let valid = release("CookLLM_9.8.7_amd64.AppImage");
    for field in ["digest", "size", "browser_download_url"] {
        let mut v = valid.clone();
        v["assets"][0].as_object_mut().unwrap().remove(field);
        assert!(select(&v, Kind::AppImage).is_err());
    }
    for digest in [
        "sha256:abc",
        "md5:abcd",
        &format!("sha256:{}", "z".repeat(64)),
    ] {
        let mut v = valid.clone();
        v["assets"][0]["digest"] = json!(digest);
        assert!(select(&v, Kind::AppImage).is_err());
    }
    let mut v = valid.clone();
    v["assets"][0]["browser_download_url"] = json!(
        "https://github.com/other/CookLLM/releases/download/v9.8.7/CookLLM_9.8.7_amd64.AppImage"
    );
    assert!(select(&v, Kind::AppImage).is_err());
    let mut v = valid.clone();
    v["assets"][0]["size"] = json!(0);
    assert!(select(&v, Kind::AppImage).is_err());
    let mut v = valid.clone();
    let asset = v["assets"][0].clone();
    v["assets"].as_array_mut().unwrap().push(asset);
    assert!(select(&v, Kind::AppImage).is_err());
}
#[test]
fn official_url_tag_boundary() {
    let name = "CookLLM_9.8.7_amd64.deb";
    let url = format!("https://github.com/{APP_GITHUB_REPO}/releases/download/v9.8.7/{name}");
    assert_eq!(asset_tag(&url, name).unwrap(), "v9.8.7");
    for bad in [
        url.replace("https:", "http:"),
        format!("{url}?q=x"),
        format!("{url}#x"),
        url.replace("github.com/", "github.com:444/"),
        url.replace("/v9.8.7/", "/v9%2f8/"),
        url.replace("github.com/", "github.com.evil/"),
        url.replace("github.com/", "name@github.com/"),
    ] {
        assert!(asset_tag(&bad, name).is_err(), "{bad}");
    }
}
fn receipt() -> Receipt {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("CookLLM_9.8.7_amd64.AppImage");
    let mut bytes = vec![0; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[8..11].copy_from_slice(b"AI\x02");
    bytes[18] = 62;
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut asset = select(&release("CookLLM_9.8.7_amd64.AppImage"), Kind::AppImage).unwrap();
    asset.digest = installer_digest(&path).unwrap();
    let meta = fs::metadata(&path).unwrap();
    Receipt {
        directory,
        path,
        asset,
        context: manual("fixture"),
        dev: meta.dev(),
        ino: meta.ino(),
    }
}
#[test]
fn receipt_accepts_original_rejects_modified_and_replaced_inode() {
    let receipt = receipt();
    assert!(verify_receipt(&receipt).is_ok());
    let original = fs::read(&receipt.path).unwrap();
    let mut changed = original.clone();
    changed[30] = 1;
    fs::write(&receipt.path, &changed).unwrap();
    assert!(verify_receipt(&receipt).is_err());
    fs::write(&receipt.path, &original).unwrap();
    assert!(verify_receipt(&receipt).is_ok());
    let other = receipt.directory.path().join("other");
    fs::write(&other, &original).unwrap();
    fs::rename(&other, &receipt.path).unwrap();
    assert!(verify_receipt(&receipt).is_err());
}
#[test]
fn receipt_rejects_symlink_directory_permissions_and_wrong_elf() {
    let mut receipt = receipt();
    let other = receipt.directory.path().join("other");
    fs::rename(&receipt.path, &other).unwrap();
    std::os::unix::fs::symlink(&other, &receipt.path).unwrap();
    assert!(verify_receipt(&receipt).is_err());
    fs::remove_file(&receipt.path).unwrap();
    fs::rename(&other, &receipt.path).unwrap();
    fs::set_permissions(receipt.directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_receipt(&receipt).is_err());
    fs::set_permissions(receipt.directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut bytes = fs::read(&receipt.path).unwrap();
    bytes[18] = 183;
    fs::write(&receipt.path, bytes).unwrap();
    receipt.asset.digest = installer_digest(&receipt.path).unwrap();
    assert!(verify_receipt(&receipt).is_err());
}
#[test]
fn unknown_path_never_creates_receipt() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("manual.AppImage");
    fs::write(&path, b"anything").unwrap();
    assert!(!receipts().lock().unwrap().contains_key(&path));
    assert_eq!(package_kind(&path), Kind::Manual);
    assert_eq!(package_kind(Path::new("/tmp/cookllm")), Kind::Manual);
}
#[test]
fn appimage_environment_requires_running_context() {
    let receipt = receipt();
    let mount = receipt.directory.path().join(".mount_CookLLMfixture");
    fs::create_dir(&mount).unwrap();
    let exe = mount.join("cookllm");
    fs::write(&exe, b"fixture").unwrap();
    assert!(appimage_context(&receipt.path, &mount, &exe, false).is_err());
    assert!(appimage_context(&receipt.path, &mount, &exe, true).is_ok());
    assert!(appimage_context(&receipt.path, &mount, Path::new("/tmp/cookllm"), true).is_err());
    fs::set_permissions(&receipt.path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(appimage_context(&receipt.path, &mount, &exe, true).is_err());
}
#[test]
fn ipc_optional_fields_and_installer_handoff_status() {
    let info = AppUpdateInfo {
        status: "available".into(),
        latest_tag: "v9.8.7".into(),
        release_url: "release".into(),
        release_notes: None,
        asset_url: None,
        asset_name: None,
        asset_size: None,
        asset_sha256: None,
        install_kind: None,
        install_supported: None,
        install_reason: None,
    };
    let v = serde_json::to_value(info).unwrap();
    assert!(v.get("installKind").is_none());
    let result = AppInstallResult {
        status: "installer-opened".into(),
        message: "系统窗口确认".into(),
        backup_path: None,
    };
    let v = serde_json::to_value(result).unwrap();
    assert_eq!(v["status"], "installer-opened");
    assert!(v.get("backupPath").is_none());
}
