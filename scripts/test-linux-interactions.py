#!/usr/bin/env python3
"""Real W3C desktop interactions on a private nested niri seat (stdlib only).

No build/download or host clipboard access. Requires niri, WebKitWebDriver,
wl-copy, wl-paste and dbus-run-session. Output must be a NEW empty /tmp directory.
Native file picking is not covered: import candidates use a real Tauri drag event,
then W3C clicks exercise the actual import/inspect/save UI. Completed download
records are fixture-seeded; deletion itself uses the real confirmation UI/backend.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import subprocess
import sys
import time
import traceback

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("desktop", Path(__file__).with_name("test-linux-desktop.py"))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)
wait, port, request = desktop.wait, desktop.port, desktop.request
members, kill_group, release_check = desktop.members, desktop.kill_group, desktop.release_check
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


class UI(desktop.Driver):
    def element(self, selector):
        return self.call("/element", {"using": "css selector", "value": selector})

    def click(self, element):
        self.call("/element/" + element[ELEMENT] + "/click", {})

    def css_click(self, selector):
        self.click(wait(lambda: self.element(selector), 10))

    def button(self, text, selector="button", exact=True):
        value = wait(lambda: self.js("return Array.from(document.querySelectorAll(arguments[0])).find(b=> !b.disabled && (arguments[2] ? b.innerText.trim()===arguments[1] : b.innerText.includes(arguments[1]))) || null", selector, text, exact), 10)
        self.click(value)

    def type(self, element, text):
        self.call("/element/" + element[ELEMENT] + "/value", {"text": text, "value": list(text)})

    def text(self):
        return self.js("return document.body.innerText")

    def nav(self, text):
        self.button(text, ".side-nav button")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def fixture_pids(root):
    """Only processes explicitly carrying this fixture's private runtime."""
    expected = ("XDG_RUNTIME_DIR=" + str(root / "runtime")).encode()
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            if expected in (entry / "environ").read_bytes().split(b"\0"):
                found.append(int(entry.name))
        except (OSError, ValueError):
            pass
    return found


def clipboard_failure(driver, root, paste, results):
    """Explicit UI fault injection, including the older execCommand fallback."""
    driver.js("window.__copyFixture={calls:[],exec:document.execCommand,descriptor:Object.getOwnPropertyDescriptor(navigator,'clipboard'),invoke:window.__TAURI_INTERNALS__.invoke};Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:()=>{window.__copyFixture.calls.push('navigator.clipboard');return Promise.reject(new Error('fixture clipboard failure'))}}});document.execCommand=function(cmd,...args){if(cmd==='copy'){window.__copyFixture.calls.push('execCommand.copy');return false}return window.__copyFixture.exec.call(this,cmd,...args)};window.__TAURI_INTERNALS__.invoke=function(cmd,args){if(cmd==='clipboard_write'){window.__copyFixture.calls.push(cmd);return Promise.reject(new Error('fixture clipboard failure'))}return window.__copyFixture.invoke.call(this,cmd,args)};return true")
    before = paste()
    private_socket = Path(results["isolation"]["child_socket"])
    assert private_socket.resolve().is_relative_to(root / "runtime")
    socket_mode = stat.S_IMODE(private_socket.stat().st_mode)
    # Existing GUI connections stay alive; new wl-copy connections get a real error.
    private_socket.chmod(0)
    try:
        driver.button("Copy logs", ".logs-page button")
        wait(lambda: driver.js("return window.__copyFixture.calls.length>0"), 5)
        time.sleep(0.5)
        private_socket.chmod(socket_mode)
        results["clipboard_fault"] = {"rejected_routes": driver.js("return window.__copyFixture.calls"), "native_helper_fault": "private socket denied new clients only", "private_selection_unchanged": paste() == before}
        assert results["clipboard_fault"]["private_selection_unchanged"], "copy fallback succeeded; error path not exercised"
        alert = wait(lambda: driver.js("return document.querySelector('.logs-page [role=alert]')?.innerText || null"), 5)
        assert "Log copy failed" in alert, alert
        driver.screenshot("logs-copy-error.png")
        return {"method": "W3C Copy logs click; explicit fault-injected clipboard boundary", "visible_alert": alert}
    finally:
        private_socket.chmod(socket_mode)
        driver.js("const f=window.__copyFixture;document.execCommand=f.exec;window.__TAURI_INTERNALS__.invoke=f.invoke;if(f.descriptor)Object.defineProperty(navigator,'clipboard',f.descriptor);else delete navigator.clipboard;delete window.__copyFixture;return true")


def worker(args, root):
    def interrupted(signum, frame):
        raise KeyboardInterrupt("fixture interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    env = os.environ.copy()
    env.pop("NIRI_SOCKET", None)
    env.pop("NIRI_CONFIG", None)
    groups, processes, listeners = [], [], []
    results = {"status": "running", "cases": {}, "unverified": ["native file chooser / physical drag", "file manager launch", "native tray menu", "GPU / other distributions"], "inputs": {name: {"path": str(getattr(args, name)), "sha256": digest(getattr(args, name))} for name in ("app", "engine", "model")}}
    driver = None
    app = root / "cookllm"
    shutil.copy2(args.app, app)
    models = root / "模型 fixture"
    models.mkdir()
    paths = [models / "Tiny.gguf", models / "tiny.gguf"]
    for target in paths:
        shutil.copy2(args.model, target)
    config_path = root / "niri.kdl"
    config_path.write_text('prefer-no-csd\nlayout { default-column-width { proportion 1.0; }; }\n')

    def save():
        (root / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2))

    def case(name, fn):
        print("CASE " + name, flush=True)
        try:
            results["cases"][name] = {"status": "passed", "evidence": fn()}
        except Exception:
            results["cases"][name] = {"status": "failed", "error": traceback.format_exc()}
            if driver and driver.session:
                try:
                    driver.call("/frame", {"id": None})
                    results["cases"][name]["ui"] = driver.text()
                    driver.screenshot(name + "-failure.png")
                except Exception as exc:
                    results["cases"][name]["capture_error"] = str(exc)
        save()
        return results["cases"][name]["status"] == "passed"

    def spawn(command, name, child_env, stdin=None):
        with (root / name).open("wb") as log:
            process = subprocess.Popen(command, env=child_env, cwd=root, stdin=stdin, stdout=log, stderr=log, start_new_session=True)
        groups.append(process.pid)
        processes.append(process)
        return process

    def paste():
        result = subprocess.run(["wl-paste", "--no-newline"], env=env, capture_output=True, timeout=5)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors="replace"))
        return result.stdout.decode()

    try:
        attempts = []
        for backend in ("wayland", "x11"):
            parent_env = env.copy()
            parent_env["WINIT_UNIX_BACKEND"] = backend
            if backend == "wayland":
                parent_env["WAYLAND_DISPLAY"] = env["COOKLLM_PARENT_WAYLAND"]
                parent_env.pop("DISPLAY", None)
            else:
                if not env.get("DISPLAY"):
                    continue
                parent_env.pop("WAYLAND_DISPLAY", None)
            nested = spawn(["niri", "--config", str(config_path)], "nested-" + backend + ".log", parent_env)
            try:
                socket_path = wait(lambda: next((p for p in (root / "runtime").glob("wayland-*") if stat.S_ISSOCK(p.stat().st_mode)), None) if nested.poll() is None else None, 12)
                break
            except Exception as exc:
                attempts.append({"backend": backend, "error": str(exc), "exit": nested.poll()})
                kill_group(nested.pid)
                nested.wait(timeout=5)
        else:
            raise RuntimeError("nested compositor attempts exhausted: " + json.dumps(attempts))
        host_socket = Path(env["COOKLLM_PARENT_WAYLAND"]).resolve(strict=True)
        child_socket = socket_path.resolve(strict=True)
        assert child_socket.is_relative_to(root / "runtime") and not child_socket.samefile(host_socket)
        info = child_socket.stat()
        assert stat.S_ISSOCK(info.st_mode) and info.st_uid == os.getuid()
        assert stat.S_IMODE((root / "runtime").stat().st_mode) == 0o700
        env.update(WAYLAND_DISPLAY=str(child_socket), GDK_BACKEND="wayland")
        env.pop("DISPLAY", None)
        env.pop("COOKLLM_PARENT_WAYLAND", None)
        results["isolation"] = {"parent_socket": str(host_socket), "child_socket": str(child_socket), "distinct_inode": [host_socket.stat().st_ino, info.st_ino], "runtime_mode": "0700", "backend": backend, "attempts": attempts, "dbus": env["DBUS_SESSION_BUS_ADDRESS"], "service_activation": "no service directories", "host_clipboard_accessed": False}
        sentinel = "CookLLM 独立座席\n中文多行 fixture\n"
        owner = spawn(["wl-copy", "--foreground", "--type", "text/plain;charset=utf-8"], "clipboard-owner.log", env, subprocess.PIPE)
        owner.stdin.write(sentinel.encode())
        owner.stdin.close()
        wait(lambda: paste() == sentinel, 8)
        results["cases"]["isolated_seat_clipboard"] = {"status": "passed", "evidence": {"roundtrip": sentinel}}
        save()
        driver = UI(root, env, app)
        results["driver_session"] = driver.start()
        listeners.append(int(driver.base.rsplit(":", 1)[1]))
        if args.clipboard_error_only:
            isolated_config = driver.invoke("load_config")
            isolated_config.update(language="en", autoUpdateEnabled=False, gpuMonitorEnabled=False, minimizeToTrayOnClose=False)
            driver.invoke("save_config", {"config": isolated_config})
            driver.call("/refresh", {})
            driver.nav("Logs")
            case("logs_copy_error_visible", lambda: clipboard_failure(driver, root, paste, results))
            results["status"] = "passed" if all(v["status"] == "passed" for v in results["cases"].values()) else "failed"
            return int(results["status"] != "passed")
        config = driver.invoke("load_config")
        profile = copy.deepcopy(config["profiles"][0])
        numbers = [port(), port()]
        listeners.extend(numbers)
        extra_args = "--metrics --chat-template chatml -n 16"
        profile.update(id="fixture-profile-a", name="CPU A 512", host="127.0.0.1", port=numbers[0], gpuLayers=0, contextSize=512, threads=2, parallel=1, batchSize=128, ubatchSize=128, flashAttention=False, jinja=True, reasoning="off", reasoningEffort="none", loadMode="auto", cacheTypeK="f16", cacheTypeV="f16", extraArgs=extra_args, engineId="fixture-engine")
        other = copy.deepcopy(profile)
        other.update(id="fixture-profile-b", name="CPU B 512", port=numbers[1])
        config.update(serverPath=str(args.engine), activeEngineId="fixture-engine", engines=[{"id": "fixture-engine", "name": "Official CPU fixture", "path": str(args.engine), "backend": "cpu", "engineType": "llamacpp"}], models=[], profiles=[profile], modelsDir=str(models), autoUpdateEnabled=False, gpuMonitorEnabled=False, minimizeToTrayOnClose=False, language="en", network={"proxyMode": "direct"})
        driver.invoke("save_config", {"config": config})
        driver.call("/refresh", {})
        wait(lambda: "Add model" in driver.text())

        def import_ui():
            driver.button("Add model", ".models-toolbar button")
            wait(lambda: driver.element(".import-modal"))
            time.sleep(1)
            driver.invoke("plugin:event|emit", {"event": "tauri://drag-drop", "payload": {"paths": [str(p) for p in paths], "position": {"x": 400, "y": 300}}})
            wait(lambda: driver.js("return document.querySelectorAll('.import-row').length===2"))
            before = driver.text()
            driver.screenshot("import-candidates.png")
            driver.button("Import selected", ".import-modal footer button", exact=False)
            wait(lambda: len(driver.invoke("load_config")["models"]) == 2)
            imported = driver.invoke("load_config")["models"]
            assert {m["path"] for m in imported} == {str(p) for p in paths}
            driver.screenshot("imported-models.png")
            return {"candidate_input": "real Tauri drag event fixture (not physical drag)", "confirmation": "W3C element click", "candidates": before, "models": imported}

        if not case("ui_import", import_ui):
            raise RuntimeError("import prerequisite failed; see case evidence")
        config = driver.invoke("load_config")
        for model in config["models"]:
            model.update(profiles=[copy.deepcopy(profile), copy.deepcopy(other)], defaultProfileId=profile["id"])
        config.update(preferredModelId=config["models"][0]["id"], preferredProfileId=profile["id"])
        driver.invoke("save_config", {"config": config})
        driver.call("/refresh", {})
        wait(lambda: driver.element(".model-card select"))

        def switch_profile():
            option = driver.js("return document.querySelector('.model-card select option[value=\"fixture-profile-b\"]')")
            driver.click(option)
            assert driver.js("return document.querySelector('.model-card select').value") == other["id"]
            driver.screenshot("preset-b.png")
            return {"selected": other["id"], "port": numbers[1], "method": "W3C option click"}

        case("preset_switch", switch_profile)
        running = {}

        def start_ui():
            driver.css_click(".model-card .launch-button")
            status = wait(lambda: (s if s["running"] else None) if (s := driver.invoke("get_server_status")) else None, 30)
            running.update(status)
            group = os.getpgid(status["pid"])
            groups.append(group)
            assert status["profileId"] == other["id"], status
            wait(lambda: request(f"http://127.0.0.1:{numbers[1]}/health"), 30)
            driver.screenshot("running-model.png")
            return {"status": status, "process_group": group, "cmdline": Path(f'/proc/{status["pid"]}/cmdline').read_bytes().decode().replace("\0", " ")}

        if not case("ui_start", start_ui):
            raise RuntimeError("start prerequisite failed; see case evidence")

        def send_ui():
            driver.nav("Chat")
            frame = wait(lambda: driver.element("iframe"), 15)
            driver.call("/frame", {"id": frame})
            dom = wait(lambda: driver.js("return document.body.innerText.length > 20 && {url:location.href,text:document.body.innerText,inputs:Array.from(document.querySelectorAll('textarea,input,[contenteditable]')).map(e=>({tag:e.tagName,type:e.type,editable:e.contentEditable,placeholder:e.getAttribute('placeholder'),outer:e.outerHTML.slice(0,1200)})),buttons:Array.from(document.querySelectorAll('button')).map(e=>({text:e.innerText,title:e.title,aria:e.getAttribute('aria-label')}))}"), 20)
            (root / "webui-dom.json").write_text(json.dumps(dom, ensure_ascii=False, indent=2))
            input_element = wait(lambda: driver.js("return Array.from(document.querySelectorAll('textarea,[contenteditable=true],input[type=text]')).find(e=>e.getBoundingClientRect().height>0) || null"), 10)
            driver.click(input_element)
            driver.type(input_element, "Once upon a time")
            typed = driver.js("return document.activeElement.value || document.activeElement.innerText")
            assert "Once upon a time" in typed
            send = wait(lambda: driver.js("return Array.from(document.querySelectorAll('button')).find(e=>!e.disabled && /send/i.test([e.innerText,e.title,e.getAttribute('aria-label')].join(' '))) || null"), 10)
            driver.click(send)
            def completed():
                metrics = request(f"http://127.0.0.1:{numbers[1]}/metrics")
                return metrics if any("tokens_predicted_total" in line and float(line.split()[-1]) > 0 for line in metrics.splitlines() if not line.startswith("#")) else None
            try:
                metrics = wait(completed, 20)
                text = driver.text()
                assert "Once upon a time" in text
                rendered = wait(lambda: driver.js("return Array.from(document.querySelectorAll('.chat-message-assistant .markdown-content, .chat-message-assistant [data-block-id], [data-role=assistant]')).map(e=>e.innerText).filter(t=>t.trim())"), 5)
                assert any(value.strip() and value.strip() != "Once upon a time" for value in rendered), rendered
                driver.screenshot("webui-send.png")
            finally:
                (root / "webui-after-send.json").write_text(json.dumps({"text": driver.text(), "html": driver.js("return document.body.innerHTML").strip()}, ensure_ascii=False, indent=2))
                driver.call("/frame", {"id": None})
            return {"method": "W3C input/value and Send click (no fetch/chat invoke)", "typed": typed, "rendered_text": text, "rendered_messages": rendered, "metrics": metrics}

        case("webui_send", send_ui)

        def clipboard_ui():
            driver.invoke("clipboard_write", {"text": sentinel})
            assert wait(lambda: paste() == sentinel, 8)
            driver.nav("Logs")
            wait(lambda: driver.js("return document.querySelector('.logs-page .log-line') || document.querySelector('.logs-page').innerText.includes('llama')"), 10)
            driver.button("Copy logs", ".logs-page button")
            copied = wait(lambda: (text if text != sentinel and "llama" in text else None) if (text := paste()) else None, 8)
            driver.screenshot("logs-copy.png")
            return {"ipc_roundtrip": sentinel, "ui_copy": copied, "copied_bytes": len(copied.encode())}

        case("clipboard_and_log_copy", clipboard_ui)

        def log_copy_error():
            return clipboard_failure(driver, root, paste, results)

        case("logs_copy_error_visible", log_copy_error)

        def stop_ui():
            driver.css_click(".capsule-btn")
            wait(lambda: not driver.invoke("get_server_status")["running"], 10)
            return release_check(numbers[1], os.getpgid(running["pid"]) if Path(f'/proc/{running["pid"]}').exists() else running["pid"])

        case("ui_stop", stop_ui)

        def disk_delete():
            tasks = [{"taskId": f"fixture-download-{i}", "repo": "direct-url", "file": p.name, "sizeBytes": p.stat().st_size, "startedAt": 1, "finishedAt": 2, "status": "done", "path": str(p), "percent": 100} for i, p in enumerate(paths)]
            driver.js("localStorage.setItem('cookllm.downloads',JSON.stringify(arguments[0]));return true", tasks)
            driver.call("/refresh", {})
            driver.nav("Explore")
            driver.button("Tasks", ".explore-view-tabs button")
            card = wait(lambda: driver.js("return Array.from(document.querySelectorAll('.task-card-done')).find(e=>e.querySelector('strong')?.innerText==='Tiny.gguf') || null"), 10)
            delete = driver.js("return arguments[0].querySelector('button.danger')", card)
            driver.click(delete)
            driver.css_click(".delete-file-checkbox input")
            driver.screenshot("delete-confirm.png")
            driver.button("Delete file & task")
            wait(lambda: not paths[0].exists(), 10)
            # Disk unlink precedes the async partial cleanup and config commit.
            # Observe the completed business state, not that intermediate filesystem state.
            def committed_models():
                models = driver.invoke("load_config")["models"]
                return models if [m["path"] for m in models] == [str(paths[1])] else None
            retained = wait(committed_models, 10)
            assert [m["path"] for m in retained] == [str(paths[1])], retained
            assert paths[1].is_file() and digest(paths[1]) == digest(args.model)
            driver.nav("Models")
            driver.screenshot("delete-retained-model.png")
            return {"fixture_record": tasks[0], "deleted": str(paths[0]), "remaining": retained, "remaining_sha256": digest(paths[1]), "source_untouched": digest(args.model)}

        case("case_sensitive_disk_delete", disk_delete)

        def close_window():
            driver.nav("Models")
            driver.css_click(".model-card .launch-button")
            status = wait(lambda: (s if s["running"] else None) if (s := driver.invoke("get_server_status")) else None, 20)
            group = os.getpgid(status["pid"])
            groups.append(group)
            driver.screenshot("before-window-close.png")
            try:
                driver.css_click(".window-controls .close")
            except RuntimeError as exc:
                if not any(message in str(exc) for message in ("no such window", "invalid session", "Session terminated without a reply")):
                    raise
            cleanup = release_check(status["port"], group)
            wait(lambda: not any(Path(f'/proc/{pid}/comm').read_text().strip() == "cookllm" for pid in fixture_pids(root) if Path(f'/proc/{pid}/comm').exists()), 10)
            driver.session = None
            return {"method": "W3C titlebar Close click while engine running", "cleanup": cleanup}

        case("window_close_cleanup", close_window)
        results["status"] = "passed" if all(v["status"] == "passed" for v in results["cases"].values()) else "failed"
    except (Exception, KeyboardInterrupt):
        results["status"] = "failed"
        results["error"] = traceback.format_exc()
    finally:
        if driver:
            try:
                driver.close()
            except Exception:
                results["driver_cleanup_error"] = traceback.format_exc()
                results["status"] = "failed"
            finally:
                groups.extend(driver.groups)
        for group in reversed(groups):
            kill_group(group)
        # wl-copy may daemonize into its own session; only the proven private runtime is eligible.
        for pid in fixture_pids(root):
            try:
                if pid == os.getppid() or Path(f"/proc/{pid}/comm").read_text().strip().startswith("dbus-"):
                    continue
                os.kill(pid, signal.SIGTERM)
            except (ProcessLookupError, FileNotFoundError):
                pass
        for process in processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                kill_group(process.pid)
        results["cleanup"] = {"groups": {str(g): members(g) for g in groups}, "remaining_fixture_pids": fixture_pids(root), "ports": {}}
        if driver and hasattr(driver, "base"):
            listeners.append(int(driver.base.rsplit(":", 1)[1]))
        for number in set(listeners):
            try:
                with socket.socket() as listener:
                    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    listener.bind(("127.0.0.1", number))
                results["cleanup"]["ports"][str(number)] = "released"
            except OSError as exc:
                results["cleanup"]["ports"][str(number)] = str(exc)
                results["status"] = "failed"
        if any(results["cleanup"]["groups"].values()):
            results["status"] = "failed"
        # The private dbus daemon is owned/closed by the outer dbus-run-session wrapper.
        results["cleanup"]["private_bus_wrapper_pending_exit"] = True
        for log in root.glob("*.log"):
            results.setdefault("stderr_tails", {})[log.name] = log.read_text(errors="replace")[-2400:]
        save()
    print(json.dumps({"status": results["status"], "cases": {k: v["status"] for k, v in results["cases"].items()}, "results": str(root / "results.json")}, ensure_ascii=False), flush=True)
    return int(results["status"] != "passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("app", "engine", "model", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--clipboard-error-only", action="store_true", help="Only verify the fault-injected main Logs copy error; skip model/server cases")
    args = parser.parse_args()
    for name in ("app", "engine", "model"):
        value = getattr(args, name).resolve(strict=True)
        if not value.is_file():
            parser.error(name + " must be a local file")
        setattr(args, name, value)
    root = args.output.resolve()
    if not root.is_relative_to(Path("/tmp")) or root == Path("/tmp"):
        parser.error("output must be a dedicated /tmp directory")
    if args.worker:
        if os.environ.get("COOKLLM_INTERACTIONS_WORKER") != str(root):
            parser.error("worker is private to this script")
        return worker(args, root)
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        parser.error("output must be empty")
    env = os.environ.copy()
    parent_wayland = env.get("WAYLAND_DISPLAY")
    runtime = env.get("XDG_RUNTIME_DIR")
    if not parent_wayland or not runtime:
        parser.error("a parent Wayland socket is required")
    env["COOKLLM_PARENT_WAYLAND"] = str((Path(runtime) / parent_wayland).resolve(strict=True))
    for key in list(env):
        if key in ("APPIMAGE", "APPDIR", "ARGV0", "LD_LIBRARY_PATH", "LD_PRELOAD", "NIRI_SOCKET", "NIRI_CONFIG", "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE") or key.startswith("LLAMA_ARG_"):
            env.pop(key)
    env.update(PATH="/usr/local/bin:/usr/bin:/bin", TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_DMABUF_RENDERER="1", OMP_NUM_THREADS="2", COOKLLM_INTERACTIONS_WORKER=str(root), NO_AT_BRIDGE="1", GTK_USE_PORTAL="0")
    for key, folder in {"HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data", "XDG_CACHE_HOME": "cache", "XDG_STATE_HOME": "state", "XDG_RUNTIME_DIR": "runtime"}.items():
        target = root / folder
        target.mkdir(mode=0o700)
        env[key] = str(target)
    bus = root / "dbus.conf"
    bus.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(root / "runtime") + '</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    # Some AppImage launchers overwrite argv[0], making sys.executable name the app.
    command = ["dbus-run-session", "--config-file=" + str(bus), "--", "/usr/bin/python3", str(Path(__file__).resolve()), "--worker"]
    for name in ("app", "engine", "model", "output"):
        command += ["--" + name, str(getattr(args, name))]
    if args.clipboard_error_only:
        command.append("--clipboard-error-only")
    process = subprocess.Popen(command, env=env, start_new_session=True)
    try:
        code = process.wait(timeout=240)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        code = 1
    finally:
        kill_group(process.pid)
        process.wait(timeout=5)
        leftovers = fixture_pids(root)
        for pid in leftovers:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        time.sleep(0.3)
        result_path = root / "results.json"
        if result_path.exists():
            results = json.loads(result_path.read_text())
            results["outer_cleanup"] = {"dbus_group": process.pid, "members": members(process.pid), "fixture_pids": fixture_pids(root)}
            if results["outer_cleanup"]["members"] or results["outer_cleanup"]["fixture_pids"]:
                results["status"] = "failed"
                code = 1
            result_path.write_text(json.dumps(results, ensure_ascii=False, indent=2))
        else:
            code = 1
            result_path.write_text(json.dumps({"status": "failed", "error": "private worker exited before producing results", "outer_cleanup": {"members": members(process.pid), "fixture_pids": fixture_pids(root)}}, indent=2))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
