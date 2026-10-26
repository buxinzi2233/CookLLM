#!/usr/bin/env python3
"""Real Thunar GUI + native tray DBus protocol test host on a private niri seat.

No product build, system configuration, downloads or host clipboard/DBus access.
Requires local release/CPU engine/model, niri, WebKitWebDriver, Thunar and Python GI.
Output must be an empty dedicated /tmp directory. The GTK consumer is a fixture,
not a distribution panel: menu Event tests are protocol calls, not mouse clicks.
"""
import argparse
import copy
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
spec = importlib.util.spec_from_file_location("interactions", Path(__file__).with_name("test-linux-interactions.py"))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)
wait, port, request = helpers.wait, helpers.port, helpers.request
kill_group, members, release_check = helpers.kill_group, helpers.members, helpers.release_check
fixture_pids, digest, UI = helpers.fixture_pids, helpers.digest, helpers.UI
WATCHER = "org.kde.StatusNotifierWatcher"
ITEM = "org.kde.StatusNotifierItem"
MENU = "com.canonical.dbusmenu"
EXPECTED_APP = "7175353013e8ea7f807b7e2530bf098ba6976d5199cd5da33f4b0a1b79936ac9"


def write_json(path, value):
    temporary = path.with_suffix(".new")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2))
    temporary.replace(path)


def tray_host(root):
    """Only consumes genuine application item properties/layout/actions."""
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gio, GLib, Gtk, GdkPixbuf, Gdk
    conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    registered = []
    state = {"host_kind": "native tray DBus protocol test host (GTK fixture consumer)", "registrations": registered}
    xml = '''<node><interface name="org.kde.StatusNotifierWatcher">
    <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
    <method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
    <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
    <signal name="StatusNotifierHostRegistered"/></interface></node>'''

    def call(dest, path, interface, method, args=None):
        return conn.call_sync(dest, path, interface, method, args, None,
                              Gio.DBusCallFlags.NONE, 5000, None)

    def owner_pid(dest):
        return call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                    "GetConnectionUnixProcessID", GLib.Variant("(s)", (dest,))).unpack()[0]

    def method(connection, sender, path, interface, name, params, invocation):
        value = params.unpack()[0]
        if name == "RegisterStatusNotifierItem":
            dest, item_path = (sender, value) if value.startswith("/") else (value, "/StatusNotifierItem")
            entry = {"destination": dest, "path": item_path, "sender": sender, "pid": owner_pid(sender)}
            if entry not in registered:
                registered.append(entry)
                conn.emit_signal(None, "/StatusNotifierWatcher", WATCHER,
                                 "StatusNotifierItemRegistered", GLib.Variant("(s)", (dest + item_path,)))
            write_json(root / "tray-host.json", state)
        invocation.return_value(None)

    def get_property(connection, sender, path, interface, name):
        if name == "RegisteredStatusNotifierItems":
            return GLib.Variant("as", [r["destination"] + r["path"] for r in registered])
        return GLib.Variant("b", True) if name == "IsStatusNotifierHostRegistered" else GLib.Variant("i", 0)

    node = Gio.DBusNodeInfo.new_for_xml(xml)
    conn.register_object("/StatusNotifierWatcher", node.interfaces[0], method, get_property, None)
    reply = call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                 GLib.Variant("(su)", (WATCHER, 4))).unpack()[0]
    assert reply == 1, reply
    conn.emit_signal(None, "/StatusNotifierWatcher", WATCHER, "StatusNotifierHostRegistered", None)
    window = Gtk.Window(title="Native tray DBus protocol test host — fixture")
    window.set_default_size(480, 230)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
    window.add(box)
    box.pack_start(Gtk.Label(label="GTK fixture consumer — not a desktop panel\nGenuine CookLLM item / DBusMenu"), False, False, 8)
    image = Gtk.Image()
    box.pack_start(image, False, False, 0)
    # Flat Gtk.MenuBar exposes the genuine DBusMenu entries without a Wayland
    # popup input serial; still a fixture renderer, never a distribution panel.
    menu = Gtk.MenuBar()
    box.pack_start(menu, False, False, 0)
    window.show_all()
    state["ready"] = True
    write_json(root / "tray-host.json", state)
    last_command = 0

    def unwrap(value):
        if isinstance(value, GLib.Variant):
            return unwrap(value.unpack())
        if isinstance(value, (tuple, list)):
            return [unwrap(x) for x in value]
        if isinstance(value, dict):
            return {k: unwrap(v) for k, v in value.items()}
        return value

    def dispatch():
        nonlocal last_command
        command_path = root / "tray-command.json"
        if not command_path.exists():
            return True
        command = json.loads(command_path.read_text())
        if command["id"] <= last_command:
            return True
        last_command = command["id"]
        try:
            item = registered[0]
            dest, path = item["destination"], item["path"]
            props = call(dest, path, "org.freedesktop.DBus.Properties", "GetAll",
                         GLib.Variant("(s)", (ITEM,))).unpack()[0]
            menu_path = props["Menu"]
            if command["action"] == "inspect":
                revision, layout = call(dest, menu_path, MENU, "GetLayout",
                                        GLib.Variant("(iias)", (0, -1, []))).unpack()
                layout = unwrap(layout)
                labels = {}

                def visit(node):
                    ident, properties, children = node
                    label = properties.get("label")
                    if label:
                        labels[label] = ident
                        widget = Gtk.MenuItem(label=label)
                        widget.connect("activate", lambda w, i=ident: call(dest, menu_path, MENU, "Event",
                                       GLib.Variant("(isvu)", (i, "clicked", GLib.Variant("i", 0), 0))))
                        menu.append(widget)
                    elif properties.get("type") == "separator":
                        menu.append(Gtk.SeparatorMenuItem())
                    for child in children:
                        visit(child)
                for child in menu.get_children():
                    menu.remove(child)
                visit(layout)
                icon = {"name": props.get("IconName"), "theme_path": props.get("IconThemePath")}
                pixmaps = props.get("IconPixmap", [])
                icon["pixmaps"] = [{"width": w, "height": h, "bytes": len(data)} for w, h, data in pixmaps]
                loaded = False
                if icon["name"]:
                    theme = Gtk.IconTheme.new()
                    theme.set_search_path(Gtk.IconTheme.get_default().get_search_path())
                    if icon["theme_path"]:
                        theme.prepend_search_path(icon["theme_path"])
                    try:
                        if Path(icon["name"]).is_absolute():
                            icon_file = Path(icon["name"]).resolve(strict=True)
                            assert icon_file.is_relative_to(root / "runtime"), icon_file
                            pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(icon_file), 32, 32, True)
                        else:
                            pixbuf = theme.load_icon(icon["name"], 32, 0)
                        image.set_from_pixbuf(pixbuf)
                        pixbuf.savev(str(root / "tray-native-icon.png"), "png", [], [])
                        loaded = True
                    except GLib.Error:
                        pass
                if not loaded and pixmaps:
                    w, h, raw = pixmaps[0]
                    # SNI pixmaps are network-order ARGB; GTK expects RGBA.
                    rgba = bytes(v for i in range(0, len(raw), 4) for v in (raw[i+1], raw[i+2], raw[i+3], raw[i]))
                    pixbuf = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(rgba), GdkPixbuf.Colorspace.RGB, True, 8, w, h, w * 4)
                    image.set_from_pixbuf(pixbuf)
                    pixbuf.savev(str(root / "tray-native-icon.png"), "png", [], [])
                    loaded = True
                state.update(item={**item, "properties": {k: props.get(k) for k in ["Id", "Title", "Status", "Menu", "ItemIsMenu"]},
                                   "icon": {**icon, "rendered": loaded}}, revision=revision, layout=layout, labels=labels)
                menu.show_all()
                state["gtk_rendered_labels"] = [w.get_label() for w in menu.get_children() if isinstance(w, Gtk.MenuItem) and w.get_label()]
                write_json(root / "tray-host.json", state)
                value = state.copy()
            else:
                ident = state["labels"][command["label"]]
                call(dest, menu_path, MENU, "Event", GLib.Variant("(isvu)", (ident, "clicked", GLib.Variant("i", 0), 0)))
                value = {"label": command["label"], "id": ident, "method": "real DBusMenu.Event(clicked), fixture consumer"}
            write_json(root / "tray-response.json", {"id": last_command, "value": value})
        except Exception:
            write_json(root / "tray-response.json", {"id": last_command, "error": traceback.format_exc()})
        return True

    GLib.timeout_add(100, dispatch)
    Gtk.main()
    return 0


def worker(args, root):
    signal.signal(signal.SIGTERM, lambda sig, frame: (_ for _ in ()).throw(KeyboardInterrupt()))
    env = os.environ.copy()
    env.pop("NIRI_SOCKET", None)
    env.pop("NIRI_CONFIG", None)
    groups, processes, ports = [], [], []
    driver = None
    results = {"status": "running", "cases": {}, "inputs": {n: {"path": str(getattr(args, n)), "sha256": digest(getattr(args, n))} for n in ("app", "engine", "model")},
               "unverified": ["distribution panel / physical tray mouse menu", "file selection (product only opens parent)", "other desktops/distributions", "physical picker / drag", "no-host default-close baseline not repeated"]}
    app = root / "cookllm"
    shutil.copy2(args.app, app)
    config_path = root / "niri.kdl"
    config_path.write_text('prefer-no-csd\nlayout { default-column-width { proportion 0.5; }; }\n')

    def save():
        write_json(root / "results.json", results)

    def spawn(command, name, child_env):
        with (root / name).open("wb") as log:
            process = subprocess.Popen(command, env=child_env, cwd=root, stdout=log, stderr=log, start_new_session=True)
        groups.append(process.pid)
        processes.append(process)
        return process

    def run(command):
        return subprocess.run(command, env=env, cwd=root, capture_output=True, text=True, timeout=8, check=True).stdout

    def windows():
        return json.loads(run(["/usr/bin/niri", "msg", "--json", "windows"]))

    def app_pids():
        return [pid for pid in fixture_pids(root) if Path(f"/proc/{pid}/exe").exists() and Path(f"/proc/{pid}/exe").resolve() == app]

    def app_windows():
        return [w for w in windows() if w.get("pid") in app_pids()]

    def screen(name):
        target = root / (name + ".png")
        run(["/usr/bin/niri", "msg", "action", "screenshot-screen", "--path", str(target), "--show-pointer", "false"])
        wait(lambda: target.exists() and target.stat().st_size, 5)
        write_json(root / (name + "-windows.json"), windows())
        return target.name

    def case(name, fn):
        print("CASE " + name, flush=True)
        try:
            results["cases"][name] = {"status": "passed", "evidence": fn()}
        except Exception:
            results["cases"][name] = {"status": "failed", "error": traceback.format_exc()}
            try:
                screen(name + "-failure")
            except Exception as exc:
                results["cases"][name]["capture_error"] = str(exc)
        save()
        return results["cases"][name]["status"] == "passed"

    command_id = 0

    def host_command(action, label=None):
        nonlocal command_id
        command_id += 1
        write_json(root / "tray-command.json", {"id": command_id, "action": action, "label": label})
        def response():
            p = root / "tray-response.json"
            value = json.loads(p.read_text()) if p.exists() else {}
            return value if value.get("id") == command_id else None
        result = wait(response, 15)
        if "error" in result:
            raise RuntimeError(result["error"])
        return result["value"]

    try:
        assert results["inputs"]["app"]["sha256"] == EXPECTED_APP, "release digest differs from requested baseline"
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
            nested = spawn(["/usr/bin/niri", "--config", str(config_path)], "nested-" + backend + ".log", parent_env)
            try:
                child = wait(lambda: next((p for p in (root / "runtime").glob("wayland-*") if stat.S_ISSOCK(p.stat().st_mode)), None) if nested.poll() is None else None, 12)
                break
            except Exception as exc:
                attempts.append({"backend": backend, "error": str(exc), "exit": nested.poll()})
                kill_group(nested.pid)
                nested.wait(timeout=5)
        else:
            raise RuntimeError("bounded nested compositor attempts exhausted: " + json.dumps(attempts))
        parent = Path(env["COOKLLM_PARENT_WAYLAND"]).resolve(strict=True)
        assert child.resolve().is_relative_to(root / "runtime") and not child.samefile(parent)
        niri_socket = wait(lambda: next((p for p in (root / "runtime").glob("niri*.sock") if stat.S_ISSOCK(p.stat().st_mode)), None), 5)
        env.update(WAYLAND_DISPLAY=str(child), NIRI_SOCKET=str(niri_socket), GDK_BACKEND="wayland")
        env.pop("DISPLAY", None)
        env.pop("COOKLLM_PARENT_WAYLAND", None)
        results["isolation"] = {"parent_socket": str(parent), "child_socket": str(child), "niri_socket": str(niri_socket), "backend": backend, "attempts": attempts,
                                "dbus": env["DBUS_SESSION_BUS_ADDRESS"], "service_activation": "none (gio direct exec)", "home_xdg_runtime_mode": "0700", "host_clipboard_accessed": False}
        # Disable gvfs/portal discovery; all changes are exclusively in fixture XDG.
        mime = root / "config" / "mimeapps.list"
        mime.write_text('[Default Applications]\ninode/directory=thunar.desktop;\n[Added Associations]\ninode/directory=thunar.desktop;\n')
        results["mime"] = run(["/usr/bin/gio", "mime", "inode/directory"])
        assert "thunar.desktop" in results["mime"], results["mime"]
        host = spawn(["/usr/bin/python3", str(Path(__file__).resolve()), "--host", "--output", str(root)], "tray-host.log", env)
        wait(lambda: json.loads((root / "tray-host.json").read_text()).get("ready") if host.poll() is None else None, 10)
        driver = UI(root, env, app)
        results["driver_session"] = driver.start()
        ports.append(int(driver.base.rsplit(":", 1)[1]))
        original_pid = wait(lambda: app_pids()[0] if len(app_pids()) == 1 else None, 5)
        wait(lambda: app_windows(), 5)

        def reveal(name, existing, directory=False):
            parent_dir = root / name
            parent_dir.mkdir()
            target = parent_dir if directory else parent_dir / "中文 空格 fixture.txt"
            if existing and not directory:
                target.write_text("fixture only\n")
            driver.invoke("reveal_in_folder", {"path": str(target)})
            def observed():
                return next((w for w in windows() if parent_dir.name in (w.get("title") or "") and "thunar" in (w.get("app_id") or "").lower()), None)
            window = wait(observed, 12)
            pid = window["pid"]
            assert pid in fixture_pids(root), window
            exe = str(Path(f"/proc/{pid}/exe").resolve(strict=True))
            assert Path(exe).name.lower() == "thunar", exe
            run(["/usr/bin/niri", "msg", "action", "focus-window", "--id", str(window["id"])])
            screenshot = screen(name)
            return {"invoke": "real Tauri reveal_in_folder", "path": str(target), "expected_directory_uri": parent_dir.as_uri() + "/", "window": window, "exe": exe, "screenshot": screenshot, "selection_claimed": False}

        if not args.tray_only:
            case("已有 文件中文", lambda: reveal("已有 文件中文", True))
            case("缺失 文件父目录", lambda: reveal("缺失 文件父目录", False))
            case("直接 目录中文", lambda: reveal("直接 目录中文", True, True))

        def bad_path():
            target = root / "不存在 父目录" / "fixture.txt"
            try:
                driver.invoke("reveal_in_folder", {"path": str(target)})
            except RuntimeError as exc:
                message = str(exc)
                assert "文件所在目录不存在" in message, message
                return {"path": str(target), "error": message}
            raise AssertionError("invalid parent unexpectedly succeeded")
        if not args.tray_only:
            case("invalid_path_error", bad_path)
        else:
            results["unverified"].append("file manager cases skipped by --tray-only (separate first-run evidence)")

        def tray_registration():
            wait(lambda: json.loads((root / "tray-host.json").read_text()).get("registrations"), 12)
            data = host_command("inspect")
            assert data["item"]["pid"] == original_pid, data["item"]
            assert {"显示 CookLLM", "退出 CookLLM"}.issubset(data["labels"]), data
            assert data["item"]["icon"]["rendered"], data["item"]
            run(["/usr/bin/niri", "msg", "action", "focus-window", "--id", str(next(w["id"] for w in windows() if w.get("pid") == host.pid))])
            data["screenshot"] = screen("tray-native-gtk-menu")
            return data
        tray_ready = case("tray_registration_icon_layout", tray_registration)
        if not tray_ready:
            raise RuntimeError("tray registration prerequisite failed; remaining tray cases archived")
        if args.tray_ui_only:
            results["unverified"].append("lifecycle cases skipped by --tray-ui-only (separate second-run evidence)")
            results["status"] = "passed"
            return 0

        config = driver.invoke("load_config")
        profile = copy.deepcopy(config["profiles"][0])
        number = port()
        ports.append(number)
        profile.update(id="fixture-profile", name="CPU tray cleanup", host="127.0.0.1", port=number, gpuLayers=0, contextSize=512, threads=2, parallel=1, batchSize=128, ubatchSize=128, flashAttention=False, jinja=False, reasoning="off", reasoningEffort="none", loadMode="auto", cacheTypeK="f16", cacheTypeV="f16", extraArgs="--metrics --no-jinja --chat-template chatml -n 16", engineId="fixture-engine")
        model = root / "Tiny tray fixture.gguf"
        shutil.copy2(args.model, model)
        config.update(serverPath=str(args.engine), activeEngineId="fixture-engine", engines=[{"id": "fixture-engine", "name": "Official CPU fixture", "path": str(args.engine), "backend": "cpu", "engineType": "llamacpp"}],
                      models=[{"id": "fixture-model", "name": model.name, "path": str(model), "sizeBytes": model.stat().st_size, "architecture": "llama", "quantization": "F32", "parameters": "260K", "profiles": [profile], "defaultProfileId": profile["id"], "accent": "cyan"}],
                      profiles=[], preferredModelId="fixture-model", preferredProfileId=profile["id"], modelsDir=str(root), autoUpdateEnabled=False, gpuMonitorEnabled=False, minimizeToTrayOnClose=True, language="en", network={"proxyMode": "direct"})
        driver.invoke("save_config", {"config": config})
        assert driver.invoke("load_config")["minimizeToTrayOnClose"] is True
        driver.call("/refresh", {})
        wait(lambda: "Tiny tray fixture.gguf" in driver.text(), 10)
        status = driver.invoke("start_server", {"modelId": "fixture-model", "profileId": profile["id"]})
        engine_group = os.getpgid(status["pid"])
        groups.append(engine_group)
        results["engine"] = {"status": status, "group": engine_group, "health": wait(lambda: request(f"http://127.0.0.1:{number}/health"), 25)}
        save()

        def hide():
            run(["/usr/bin/niri", "msg", "action", "focus-window", "--id", str(app_windows()[0]["id"])])
            driver.css_click(".window-controls .close")
            wait(lambda: not app_windows(), 8)
            assert app_pids() == [original_pid], app_pids()
            assert members(engine_group), "engine unexpectedly exited on hide"
            return {"method": "real W3C titlebar Close click", "visible_app_windows": app_windows(), "app_pid": original_pid, "alive": True, "engine_members": members(engine_group), "screenshot": screen("tray-hidden")}
        if not case("close_hides_keeps_process_engine", hide):
            raise RuntimeError("hidden window prerequisite failed")

        def show():
            event = host_command("event", "显示 CookLLM")
            visible = wait(lambda: app_windows(), 8)
            driver.screenshot("tray-restored-webview.png")
            assert app_pids() == [original_pid], app_pids()
            return {"event": event, "visible": visible, "pid": original_pid, "screenshot": screen("tray-restored")}
        if not case("dbusmenu_show_restores", show):
            raise RuntimeError("restore prerequisite failed")

        def reopen():
            hidden = hide()
            second = spawn([str(app)], "single-instance-reopen.log", env)
            code = second.wait(timeout=12)
            visible = wait(lambda: app_windows(), 8)
            assert code == 0 and app_pids() == [original_pid] and len(visible) == 1, (code, app_pids(), visible)
            return {"hidden": hidden, "second_pid": second.pid, "second_exit": code, "primary_pid": original_pid, "visible": visible, "screenshot": screen("single-instance-restored")}
        case("single_instance_reopen_restores", reopen)

        def quit_menu():
            event = host_command("event", "退出 CookLLM")
            cleanup = release_check(number, engine_group)
            wait(lambda: not app_pids() and not app_windows(), 10)
            driver.session = None
            return {"event": event, "engine_cleanup": cleanup, "app_pids": app_pids(), "main_windows": app_windows(), "screenshot": screen("tray-exited")}
        case("dbusmenu_quit_cleans_engine", quit_menu)
        results["status"] = "passed" if all(c["status"] == "passed" for c in results["cases"].values()) else "failed"
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
            groups.extend(driver.groups)
        for group in reversed(groups):
            kill_group(group)
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
        results["cleanup"] = {"groups": {str(g): members(g) for g in groups}, "private_bus_wrapper_pending_exit": True, "ports": {}}
        for number in set(ports):
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
        for log in root.glob("*.log"):
            results.setdefault("log_tails", {})[log.name] = log.read_text(errors="replace")[-2400:]
        save()
    print(json.dumps({"status": results["status"], "cases": {k: v["status"] for k, v in results["cases"].items()}, "results": str(root / "results.json")}, ensure_ascii=False), flush=True)
    return int(results["status"] != "passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("app", "engine", "model"):
        parser.add_argument("--" + name, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--tray-only", action="store_true", help="Skip file manager cases when first-run evidence already exists")
    parser.add_argument("--tray-ui-only", action="store_true", help="Only capture native GTK consumer/icon/layout; implies --tray-only")
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--host", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.tray_only = args.tray_only or args.tray_ui_only
    root = args.output.resolve()
    if not root.is_relative_to(Path("/tmp")) or root == Path("/tmp"):
        parser.error("output must be a dedicated /tmp directory")
    if args.worker or args.host:
        if os.environ.get("COOKLLM_DESKTOP_SERVICES_WORKER") != str(root):
            parser.error("worker/host private to this script")
        return tray_host(root) if args.host else worker(args, root)
    for name in ("app", "engine", "model"):
        value = getattr(args, name)
        if value is None or not value.resolve(strict=True).is_file():
            parser.error(name + " must be an existing local file")
        setattr(args, name, value.resolve(strict=True))
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if any(root.iterdir()):
        parser.error("output must be empty")
    env = os.environ.copy()
    if not env.get("WAYLAND_DISPLAY") or not env.get("XDG_RUNTIME_DIR"):
        parser.error("parent Wayland socket required")
    env["COOKLLM_PARENT_WAYLAND"] = str((Path(env["XDG_RUNTIME_DIR"]) / env["WAYLAND_DISPLAY"]).resolve(strict=True))
    for key in list(env):
        if key in ("APPIMAGE", "APPDIR", "ARGV0", "LD_LIBRARY_PATH", "LD_PRELOAD", "NIRI_SOCKET", "NIRI_CONFIG", "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE", "AT_SPI_BUS_ADDRESS") or key.startswith("LLAMA_ARG_"):
            env.pop(key)
    env.update(PATH="/usr/local/bin:/usr/bin:/bin", TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_DMABUF_RENDERER="1", OMP_NUM_THREADS="2", NO_AT_BRIDGE="1", GTK_USE_PORTAL="0", GIO_USE_VFS="local", GIO_USE_VOLUME_MONITOR="unix", COOKLLM_DESKTOP_SERVICES_WORKER=str(root))
    for key, folder in {"HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data", "XDG_CACHE_HOME": "cache", "XDG_STATE_HOME": "state", "XDG_RUNTIME_DIR": "runtime"}.items():
        target = root / folder
        target.mkdir(mode=0o700)
        env[key] = str(target)
    bus = root / "dbus.conf"
    bus.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(root / "runtime") + '</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    command = ["/usr/bin/dbus-run-session", "--config-file=" + str(bus), "--", "/usr/bin/python3", str(Path(__file__).resolve()), "--worker"]
    for name in ("app", "engine", "model", "output"):
        command += ["--" + name, str(getattr(args, name))]
    if args.tray_only:
        command.append("--tray-only")
    if args.tray_ui_only:
        command.append("--tray-ui-only")
    process = subprocess.Popen(command, env=env, start_new_session=True)
    try:
        code = process.wait(timeout=240)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        code = 1
    finally:
        kill_group(process.pid)
        process.wait(timeout=5)
        # Eligibility is exact fixture runtime, never a global process-name match.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for pid in fixture_pids(root):
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
            time.sleep(0.3)
        result_path = root / "results.json"
        results = json.loads(result_path.read_text()) if result_path.exists() else {"status": "failed", "error": "worker exited before results"}
        results["outer_cleanup"] = {"dbus_group": process.pid, "members": members(process.pid), "fixture_pids": fixture_pids(root)}
        if results["outer_cleanup"]["members"] or results["outer_cleanup"]["fixture_pids"]:
            results["status"] = "failed"
            code = 1
        write_json(result_path, results)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
