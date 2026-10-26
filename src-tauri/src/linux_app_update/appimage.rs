//! One bounded, constant-script helper. It receives backend-generated positional arguments;
//! there is no public binary flag / IPC command accepting arbitrary replacement paths.
use super::*;

const SCRIPT: &str = r#"
PATH=/usr/bin:/bin
export PATH
umask 077
target=$1
work=$2
parent=$3
identity=$4
oldhash=$5
newhash=$6
limit=$7
# Wait until the old process is gone (including its single-instance lock). PID reuse is
# distinguished by Linux starttime. Timeout leaves the original untouched.
i=0
while [ -r "/proc/$parent/stat" ]; do
  current=$(awk '{ sub(/^.*\) /, ""); print $20 }' "/proc/$parent/stat")
  [ "$current" = "$identity" ] || break
  [ "$i" -lt "$limit" ] || { echo parent-timeout > "$work/result"; exit 1; }
  sleep 0.1
  i=$((i + 1))
done
# A helper launch alone is not consent to modify before the parent commits.
[ -f "$work/commit" ] || { echo not-committed > "$work/result"; exit 1; }
[ ! -L "$target" ] && [ -f "$target" ] || { echo target-changed > "$work/result"; exit 1; }
actual=$(sha256sum < "$target"); actual=${actual%% *}
[ "$actual" = "$oldhash" ] || { echo target-changed > "$work/result"; exit 1; }
actual=$(sha256sum < "$work/new.AppImage"); actual=${actual%% *}
[ "$actual" = "$newhash" ] || { echo staging-changed > "$work/result"; exit 1; }
# backup and new were copied, fsynced and validated before spawning this helper.
if ! mv -fT -- "$work/new.AppImage" "$target"; then
  echo replace-failed > "$work/result"
  env -u APPIMAGE -u APPDIR -u COOKLLM_UPDATE_READY_DIR "$target" &
  exit 1
fi
sync -f "$target"
sync -f "$work/.."
env -u APPIMAGE -u APPDIR COOKLLM_UPDATE_READY_DIR="$work" setsid "$target" &
child=$!
childid=$(awk '{ sub(/^.*\) /, ""); print $20 }' "/proc/$child/stat" 2>/dev/null)
same_child() {
  [ -n "$childid" ] && [ "$(awk '{ sub(/^.*\) /, ""); print $20 }' "/proc/$child/stat" 2>/dev/null)" = "$childid" ] && kill -0 "$child" 2>/dev/null
}
i=0
while [ "$i" -lt "$limit" ]; do
  if [ -f "$work/ready" ] && same_child; then
    echo startup-confirmed > "$work/result"
    exit 0
  fi
  same_child || break
  sleep 0.1
  i=$((i + 1))
done
# Never replace beneath a still-running unconfirmed image. Stop its private process group.
if same_child; then
  /bin/kill -TERM -- "-$child" 2>/dev/null || true
  sleep 0.5
  if same_child; then /bin/kill -KILL -- "-$child" 2>/dev/null || true; fi
fi
wait "$child" 2>/dev/null || true
# Retain old.AppImage even after rollback; model/config directories are never involved.
if cp -- "$work/old.AppImage" "$work/rollback.AppImage" && chmod 700 "$work/rollback.AppImage" && sync -f "$work/rollback.AppImage" && mv -fT -- "$work/rollback.AppImage" "$target"; then
  sync -f "$work/.."
  echo rolled-back-startup-unconfirmed > "$work/result"
  env -u APPIMAGE -u APPDIR -u COOKLLM_UPDATE_READY_DIR "$target" &
else
  echo rollback-failed-backup-retained > "$work/result"
fi
exit 1
"#;

pub(super) fn tools_available() -> bool {
    [
        "/bin/sh",
        "/bin/kill",
        "/usr/bin/awk",
        "/usr/bin/sha256sum",
        "/usr/bin/mv",
        "/usr/bin/cp",
        "/usr/bin/chmod",
        "/usr/bin/sync",
        "/usr/bin/env",
        "/usr/bin/setsid",
        "/usr/bin/sleep",
    ]
    .iter()
    .all(|p| platform::executable_file(Path::new(p)))
}
fn copy_sync(source: &mut fs::File, to: &Path, digest: &str) -> Result<(), String> {
    source.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut dest = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o700)
        .open(to)
        .map_err(error)?;
    std::io::copy(source, &mut dest).map_err(error)?;
    dest.sync_all().map_err(error)?;
    if installer_digest(to)? != digest {
        return Err("暂存复制摘要校验失败".into());
    }
    Ok(())
}
pub(super) struct Prepared {
    directory: TempDir,
    target: PathBuf,
    old_hash: String,
    new_hash: String,
}
pub(super) fn prepare(
    target: &Path,
    source: &mut fs::File,
    digest: &str,
) -> Result<Prepared, String> {
    if !tools_available() {
        return Err("AppImage 更新辅助工具缺失，请手动更新".into());
    }
    let mut old = regular_owned(target, true)?;
    image_header(&mut old)?;
    image_header(source)?;
    let parent = target.parent().ok_or("AppImage 父目录缺失")?;
    let meta = fs::symlink_metadata(parent).map_err(error)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o200 == 0
        || meta.mode() & 0o022 != 0
    {
        return Err("AppImage 目录非本用户私有可写目录".into());
    }
    let directory = tempfile::Builder::new()
        .prefix(".cookllm-app-update-")
        .tempdir_in(parent)
        .map_err(error)?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).map_err(error)?;
    let old_hash = installer_digest(target)?;
    copy_sync(&mut old, &directory.path().join("old.AppImage"), &old_hash)?;
    copy_sync(source, &directory.path().join("new.AppImage"), digest)?;
    let mut binding = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.path().join("target"))
        .map_err(error)?;
    binding
        .write_all(target.as_os_str().as_encoded_bytes())
        .map_err(error)?;
    binding.sync_all().map_err(error)?;
    fs::File::open(directory.path())
        .and_then(|f| f.sync_all())
        .map_err(error)?;
    Ok(Prepared {
        directory,
        target: target.into(),
        old_hash,
        new_hash: digest.into(),
    })
}
fn starttime(pid: u32) -> Result<String, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(error)?;
    stat.rsplit_once(") ")
        .and_then(|(_, s)| s.split_whitespace().nth(19))
        .map(str::to_owned)
        .ok_or("进程启动时间缺失".into())
}
impl Prepared {
    fn command(
        &self,
        shell: &Path,
        parent: u32,
        identity: &str,
        ticks: u32,
    ) -> Result<Command, String> {
        let log = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.directory.path().join("helper.log"))
            .map_err(error)?;
        let mut command = Command::new(shell);
        command
            .arg("-c")
            .arg(SCRIPT)
            .arg("cookllm-app-update")
            .arg(&self.target)
            .arg(self.directory.path())
            .arg(parent.to_string())
            .arg(identity)
            .arg(&self.old_hash)
            .arg(&self.new_hash)
            .arg(ticks.to_string())
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(error)?)
            .stderr(log)
            .current_dir(self.target.parent().ok_or("AppImage 父目录缺失")?);
        // Do not pin the old FUSE mount via cwd or load helper/new-app libraries from it.
        if let Some(appdir) = std::env::var_os("APPDIR").filter(|v| !v.is_empty()) {
            let appdir = appdir.to_string_lossy();
            for (key, value) in std::env::vars_os() {
                if value.to_string_lossy().contains(appdir.as_ref()) {
                    command.env_remove(key);
                }
            }
        }
        command
            .env_remove("APPIMAGE")
            .env_remove("APPDIR")
            .env_remove("COOKLLM_UPDATE_READY_DIR");
        // The helper must outlive the old GUI, but has a bounded wait in both phases.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        Ok(command)
    }
    pub(super) fn spawn(&self) -> Result<Child, String> {
        self.command(
            Path::new("/bin/sh"),
            std::process::id(),
            &starttime(std::process::id())?,
            900,
        )?
        .spawn()
        .map_err(|e| format!("启动更新辅助进程失败：{e}"))
    }
    pub(super) fn commit(self) -> Result<PathBuf, String> {
        let marker = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.directory.path().join("commit"))
            .map_err(error)?;
        marker.sync_all().map_err(error)?;
        let directory = self.directory.keep();
        Ok(directory.join("old.AppImage"))
    }
}
pub(super) fn notify_ready() -> Result<(), String> {
    let Some(value) = std::env::var_os("COOKLLM_UPDATE_READY_DIR") else {
        return Ok(());
    };
    let dir = PathBuf::from(value);
    // This environment capability can create only the fixed ready marker in an owned,
    // generated private directory bound to the AppImage actually running now.
    if !dir.is_absolute()
        || fs::canonicalize(&dir).map_err(error)? != dir
        || !dir
            .file_name()
            .is_some_and(|s| s.to_string_lossy().starts_with(".cookllm-app-update-"))
    {
        return Err("更新启动确认目录无效".into());
    }
    let meta = fs::symlink_metadata(&dir).map_err(error)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("更新启动确认目录权限无效".into());
    }
    let mut binding = regular_owned(&dir.join("target"), false)?;
    let mut bytes = Vec::new();
    binding.read_to_end(&mut bytes).map_err(error)?;
    use std::os::unix::ffi::OsStringExt;
    let target = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    let image = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or("更新启动缺少 APPIMAGE 身份")?;
    if target != image
        || target.parent() != dir.parent()
        || detect().target.as_ref() != Some(&target)
    {
        return Err("更新启动应用身份不匹配".into());
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("ready"))
    {
        Ok(f) => f.sync_all().map_err(error),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (TempDir, PathBuf, fs::File, String) {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("CookLLM 空格\\\n.AppImage");
        let new = d.path().join("download");
        let mut bytes = vec![0; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[8..11].copy_from_slice(b"AI\x02");
        bytes[18] = 62;
        fs::write(&old, &bytes).unwrap();
        bytes[30] = 1;
        fs::write(&new, &bytes).unwrap();
        (
            d,
            old,
            fs::File::open(&new).unwrap(),
            installer_digest(&new).unwrap(),
        )
    }
    // Opt-in only: run this single test under dbus-run-session, with the fixture path
    // and COOKLLM_APPIMAGE_ISOLATED_DBUS=1. It never executes the supplied original.
    #[test]
    #[ignore = "requires a real AppImage and an isolated display/DBus session"]
    fn real_appimage_helper_react_handshake() {
        use std::collections::{HashMap, HashSet};
        use std::os::unix::process::CommandExt;
        use std::time::Instant;
        fn processes() -> HashMap<i32, (i32, i32, char)> {
            let mut out = HashMap::new();
            for entry in fs::read_dir("/proc").unwrap().flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<i32>().ok())
                else {
                    continue;
                };
                let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some((_, tail)) = stat.rsplit_once(") ") else {
                    continue;
                };
                let fields: Vec<_> = tail.split_whitespace().collect();
                if fields.len() < 3 {
                    continue;
                }
                if let (Ok(ppid), Ok(group)) = (fields[1].parse(), fields[2].parse()) {
                    out.insert(pid, (ppid, group, fields[0].chars().next().unwrap_or('?')));
                }
            }
            out
        }
        struct Cleanup {
            children: Vec<Child>,
            groups: HashSet<i32>,
            image: PathBuf,
        }
        impl Cleanup {
            fn track(&mut self) {
                let all = processes();
                let mut known: HashSet<i32> = self.children.iter().map(|c| c.id() as i32).collect();
                // Type-2 FUSE runtime daemonizes; identify only this temporary image,
                // then include its dedicated process group in cleanup too.
                for pid in all.keys() {
                    if fs::read_link(format!("/proc/{pid}/exe")).ok().as_deref()
                        == Some(self.image.as_path())
                    {
                        known.insert(*pid);
                    }
                }
                loop {
                    let before = known.len();
                    for (pid, (parent, group, _)) in &all {
                        if known.contains(parent) || self.groups.contains(group) {
                            known.insert(*pid);
                        }
                    }
                    if known.len() == before {
                        break;
                    }
                }
                for pid in known {
                    if let Some((_, group, _)) = all.get(&pid) {
                        if *group > 1 && *group != unsafe { libc::getpgrp() } {
                            self.groups.insert(*group);
                        }
                    }
                }
            }
            fn live(&self) -> usize {
                processes()
                    .values()
                    .filter(|(_, group, state)| self.groups.contains(group) && *state != 'Z')
                    .count()
            }
            fn mounts(&self) -> Vec<PathBuf> {
                let expected = self.image.file_name().unwrap().to_string_lossy();
                fs::read_to_string("/proc/self/mountinfo")
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| {
                        let (left, right) = line.split_once(" - ")?;
                        let fields: Vec<_> = left.split_whitespace().collect();
                        let detail: Vec<_> = right.split_whitespace().collect();
                        if fields.len() >= 5
                            && detail.len() >= 2
                            && detail[0].starts_with("fuse.")
                            && mount_field(detail[1]) == expected
                        {
                            Some(PathBuf::from(mount_field(fields[4])))
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            fn stop(&mut self) {
                self.track();
                for group in &self.groups {
                    unsafe {
                        libc::kill(-*group, libc::SIGTERM);
                    }
                }
                let until = Instant::now() + Duration::from_secs(2);
                while self.live() != 0 && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(50));
                }
                for group in &self.groups {
                    unsafe {
                        libc::kill(-*group, libc::SIGKILL);
                    }
                }
                for child in &mut self.children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                // A daemon killed during fixture cleanup may leave a cached FUSE mount.
                // Only this random fixture's exact mount source is eligible for unmount.
                for mount in self.mounts() {
                    let _ = platform::checked_tool(
                        Command::new("/usr/bin/fusermount3").arg("-u").arg(&mount),
                        None,
                        Duration::from_secs(2),
                    );
                }
            }
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.stop();
            }
        }
        assert_eq!(
            std::env::var("COOKLLM_APPIMAGE_ISOLATED_DBUS").as_deref(),
            Ok("1"),
            "run with dbus-run-session and explicit isolation flag"
        );
        assert!(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some());
        let original = PathBuf::from(
            std::env::var_os("COOKLLM_APPIMAGE_FIXTURE")
                .expect("COOKLLM_APPIMAGE_FIXTURE is required"),
        );
        let original_hash = installer_digest(&original).unwrap();
        let root = tempfile::Builder::new()
            .prefix("cookllm-app-update-real-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.path().join(format!(
            "CookLLM isolated-{}.AppImage",
            root.path().file_name().unwrap().to_string_lossy()
        ));
        let download = root.path().join("download.AppImage");
        fs::copy(&original, &target).unwrap();
        fs::copy(&original, &download).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let mut source = fs::File::open(&download).unwrap();
        let prepared = prepare(&target, &mut source, &original_hash).unwrap();
        let work = prepared.directory.path().to_path_buf();
        let mut cleanup = Cleanup {
            children: Vec::new(),
            groups: HashSet::new(),
            image: target.clone(),
        };
        let parent = Command::new("/bin/sleep")
            .arg("1")
            .process_group(0)
            .spawn()
            .unwrap();
        let parent_id = parent.id();
        cleanup.groups.insert(parent_id as i32);
        cleanup.children.push(parent);
        let mut command = prepared
            .command(
                Path::new("/bin/sh"),
                parent_id,
                &starttime(parent_id).unwrap(),
                250,
            )
            .unwrap();
        for key in [
            "APPDIR",
            "APPIMAGE",
            "ARGV0",
            "OWD",
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "TAURI_WEBVIEW_AUTOMATION",
            "WAYLAND_DISPLAY",
        ] {
            command.env_remove(key);
        }
        for (key, leaf) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            let dir = root.path().join(leaf);
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            command.env(key, dir);
        }
        command
            .env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
            .env("GDK_BACKEND", "x11")
            .env("NO_AT_BRIDGE", "1");
        let helper = command.spawn().unwrap();
        cleanup.groups.insert(helper.id() as i32);
        cleanup.children.push(helper);
        let backup = prepared.commit().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !work.join("result").exists(),
            "helper must wait for old process"
        );
        cleanup.children[0].wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(35);
        let mut context_checked = false;
        let status = loop {
            cleanup.track();
            if !context_checked {
                for (pid, (_, group, _)) in processes() {
                    if !cleanup.groups.contains(&group) {
                        continue;
                    }
                    let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) else {
                        continue;
                    };
                    if exe.file_name().is_none_or(|s| s != "cookllm") {
                        continue;
                    }
                    let appdir = exe.ancestors().nth(3).unwrap();
                    let context = appimage_context(
                        &target,
                        appdir,
                        &exe,
                        active_image_runtime(&target, appdir),
                    );
                    eprintln!("real running package identity: {context:?}");
                    assert!(
                        context.is_ok(),
                        "real package capability was not established"
                    );
                    context_checked = true;
                }
            }
            if let Some(status) = cleanup.children[1].try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let log = fs::read_to_string(work.join("helper.log")).unwrap_or_default();
        let result = fs::read_to_string(work.join("result")).unwrap_or_default();
        eprintln!("real AppImage helper result={result:?}\n{log}");
        cleanup.stop();
        assert!(cleanup.mounts().is_empty(), "fixture FUSE mount remains");
        assert_eq!(
            cleanup.live(),
            0,
            "fixture processes remain after group cleanup"
        );
        assert!(
            status.is_some_and(|s| s.success()),
            "helper failed or timed out: {result}"
        );
        assert_eq!(result.trim(), "startup-confirmed");
        assert!(
            work.join("ready").is_file(),
            "real notify_ready marker missing"
        );
        assert_eq!(installer_digest(&backup).unwrap(), original_hash);
        assert_eq!(installer_digest(&target).unwrap(), original_hash);
        assert_eq!(installer_digest(&original).unwrap(), original_hash);
        eprintln!(
            "verified original/backup/target SHA256={original_hash}; live fixture processes=0"
        );
    }

    #[test]
    fn preparation_preserves_original_and_unrelated_data() {
        let (d, old, mut new, hash) = fixture();
        fs::write(d.path().join("config.json"), b"unchanged").unwrap();
        fs::write(d.path().join("model.gguf"), b"model").unwrap();
        let old_hash = installer_digest(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        assert_eq!(installer_digest(&old).unwrap(), old_hash);
        assert_eq!(
            installer_digest(&p.directory.path().join("old.AppImage")).unwrap(),
            old_hash
        );
        assert_eq!(
            fs::read(d.path().join("config.json")).unwrap(),
            b"unchanged"
        );
        assert_eq!(fs::read(d.path().join("model.gguf")).unwrap(), b"model");
    }
    #[test]
    fn copy_failure_and_readonly_or_symlink_leave_original() {
        let (d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        assert!(prepare(&old, &mut new, &"0".repeat(64)).is_err());
        assert_eq!(fs::read(&old).unwrap(), before);
        fs::set_permissions(&old, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(prepare(&old, &mut new, &hash).is_err());
        fs::set_permissions(&old, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&old, &link).unwrap();
        assert!(prepare(&link, &mut new, &hash).is_err());
    }
    #[test]
    fn helper_spawn_failure_leaves_original() {
        let (_d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        assert!(p
            .command(Path::new("/no-such-cookllm-shell"), 1, "0", 1)
            .unwrap()
            .spawn()
            .is_err());
        assert_eq!(fs::read(&old).unwrap(), before);
    }
    #[test]
    fn helper_waits_for_old_process_and_commit() {
        let (_d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        let status = p
            .command(
                Path::new("/bin/sh"),
                std::process::id(),
                &starttime(std::process::id()).unwrap(),
                2,
            )
            .unwrap()
            .status()
            .unwrap();
        assert!(!status.success());
        assert_eq!(fs::read(&old).unwrap(), before);
        assert_eq!(
            fs::read_to_string(p.directory.path().join("result"))
                .unwrap()
                .trim(),
            "parent-timeout"
        );
    }
    #[test]
    fn restart_failure_rolls_back_and_keeps_backup() {
        let (_d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        // Invalid executable fixture: actual exec fails, exercising rollback without GUI.
        let mut cmd = p.command(Path::new("/bin/sh"), u32::MAX, "0", 2).unwrap();
        let backup = p.commit().unwrap();
        assert!(!cmd.status().unwrap().success());
        assert_eq!(fs::read(&old).unwrap(), before);
        assert_eq!(fs::read(backup).unwrap(), before);
    }
    #[test]
    fn successful_handshake_only_after_old_process_releases() {
        let (_d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let mut p = prepare(&old, &mut new, &hash).unwrap();
        // Only this fixture replaces the prevalidated ELF with a bounded shell dummy.
        // Production always hashes the staged ELF against official release metadata.
        let stage = p.directory.path().join("new.AppImage");
        fs::write(
            &stage,
            b"#!/bin/sh\nprintf ready > \"$COOKLLM_UPDATE_READY_DIR/ready\"\nsleep 1\n",
        )
        .unwrap();
        p.new_hash = installer_digest(&stage).unwrap();
        let mut parent = Command::new("/bin/sleep").arg("0.4").spawn().unwrap();
        let mut helper = p
            .command(
                Path::new("/bin/sh"),
                parent.id(),
                &starttime(parent.id()).unwrap(),
                30,
            )
            .unwrap()
            .spawn()
            .unwrap();
        let backup = p.commit().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fs::read(&old).unwrap(), before);
        parent.wait().unwrap();
        assert!(helper.wait().unwrap().success());
        assert_eq!(
            fs::read_to_string(backup.parent().unwrap().join("result"))
                .unwrap()
                .trim(),
            "startup-confirmed"
        );
        assert_eq!(fs::read(&backup).unwrap(), before);
        std::thread::sleep(Duration::from_millis(1100));
    }
    #[test]
    fn missing_commit_keeps_old_image() {
        let (_d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        assert!(!p
            .command(Path::new("/bin/sh"), u32::MAX, "0", 2)
            .unwrap()
            .status()
            .unwrap()
            .success());
        assert_eq!(fs::read(&old).unwrap(), before);
    }
    #[test]
    fn rename_failure_retains_original_and_backup() {
        let (d, old, mut new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let p = prepare(&old, &mut new, &hash).unwrap();
        let mut cmd = p.command(Path::new("/bin/sh"), u32::MAX, "0", 2).unwrap();
        let backup = p.commit().unwrap();
        // A read-only parent causes the real atomic rename to fail for a normal user.
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let status = cmd.status().unwrap();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!status.success());
        assert_eq!(fs::read(&old).unwrap(), before);
        assert_eq!(fs::read(&backup).unwrap(), before);
        assert_eq!(
            fs::read_to_string(backup.parent().unwrap().join("result"))
                .unwrap()
                .trim(),
            "replace-failed"
        );
    }
    #[test]
    fn copy_io_failure_leaves_source_unchanged() {
        let (d, old, _new, hash) = fixture();
        let before = fs::read(&old).unwrap();
        let mut write_only = fs::OpenOptions::new().write(true).open(&old).unwrap();
        assert!(copy_sync(&mut write_only, &d.path().join("partial"), &hash).is_err());
        assert_eq!(fs::read(&old).unwrap(), before);
    }
    #[test]
    fn changed_target_is_not_replaced() {
        let (_d, old, mut new, hash) = fixture();
        let p = prepare(&old, &mut new, &hash).unwrap();
        let mut cmd = p.command(Path::new("/bin/sh"), u32::MAX, "0", 2).unwrap();
        let backup = p.commit().unwrap();
        fs::write(&old, b"changed externally").unwrap();
        assert!(!cmd.status().unwrap().success());
        assert_eq!(fs::read(&old).unwrap(), b"changed externally");
        assert!(backup.is_file());
    }
}
