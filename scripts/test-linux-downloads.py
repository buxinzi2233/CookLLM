#!/usr/bin/env python3
"""Real Linux download IPC acceptance; stdlib, local GGUF, existing release only.

Usage: python3 scripts/test-linux-downloads.py --app /path/to/cookllm
       --model /path/to/model.gguf --output /tmp/new-empty-private-directory
No builds, external downloads, tokens, clipboard operations or product edits.
Results/controller logs are also persisted in .tmp/linux-validation/downloads-*.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("download_desktop", Path(__file__).with_name("test-linux-desktop.py"))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)
wait, request, kill_group = desktop.wait, desktop.request, desktop.kill_group
CASES = ("pause_resume", "cancel_stream", "cancel_stall", "ignore_range_200", "truncated_body", "invalid_206", "parallel_delete", "target_conflict")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def fixture_pids(root):
    marker = ("XDG_RUNTIME_DIR=" + str(root / "runtime")).encode()
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if fields[0] != "Z" and marker in (entry / "environ").read_bytes().split(b"\0"):
                found.append(int(entry.name))
        except (OSError, ValueError, IndexError):
            pass
    return found


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = False
    block_on_close = True

    def __init__(self, root, model):
        self.root, self.model = root, model
        self.routes, self.records = {}, []
        self.lock = threading.Lock()
        self.stop = threading.Event()
        super().__init__(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": 0.1}, name="download-http")
        self.thread.start()

    def route(self, name, mode="slow"):
        folder = self.root / name
        folder.mkdir(parents=True)
        source = folder / (name + ".gguf")
        shutil.copyfile(self.model, source)
        path = "/" + name + "/" + source.name
        self.routes[path] = {"source": source, "mode": mode, "release": threading.Event()}
        return f"http://127.0.0.1:{self.server_port}{path}", self.routes[path]

    def requests(self, url):
        path = urlsplit(url).path
        with self.lock:
            return [dict(row) for row in self.records if row["path"] == path]

    def close(self):
        self.stop.set()
        for route in self.routes.values():
            route["release"].set()
        self.shutdown()
        self.thread.join(timeout=5)
        self.server_close()  # joins every non-daemon request thread
        assert not self.thread.is_alive(), "HTTP controller thread still alive"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        route = self.server.routes.get(urlsplit(self.path).path)
        if route is None or urlsplit(self.path).query:
            self.send_error(404)
            return
        data = route["source"].read_bytes()
        etag = '"' + hashlib.sha256(data).hexdigest() + '"'
        range_value, if_range = self.headers.get("Range"), self.headers.get("If-Range")
        mode, offset, code = route["mode"], 0, 200
        if range_value and mode != "ignore" and if_range == etag:
            assert range_value.startswith("bytes=") and range_value.endswith("-")
            offset = int(range_value[6:-1])
            code = 206
        if offset >= len(data):
            self.send_error(416)
            return
        row = {"path": urlsplit(self.path).path, "range": range_value, "if_range": if_range, "etag": etag, "mode": mode, "status": code, "offset": offset, "sent": 0, "end": "sending", "authorization_present": bool(self.headers.get("Authorization"))}
        with self.server.lock:
            self.server.records.append(row)
        self.connection.settimeout(3)
        self.close_connection = True
        try:
            self.send_response(code)
            self.send_header("ETag", etag)
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(len(data) - offset))
            self.send_header("Connection", "close")
            if code == 206:
                start = offset + 1 if mode == "bad-range" else offset
                self.send_header("Content-Range", f"bytes {start}-{len(data)-1}/{len(data)}")
            self.end_headers()
            end = min(len(data), offset + 65536) if mode == "truncate" else len(data)
            for start in range(offset, end, 16384):
                if self.server.stop.is_set():
                    break
                chunk = data[start:min(start + 16384, end)]
                self.wfile.write(chunk)
                self.wfile.flush()
                with self.server.lock:
                    row["sent"] += len(chunk)
                if mode == "stall":
                    route["release"].wait(timeout=20)
                    break
                delay = 0.08 if route["mode"] == "slow" else 0.003
                if self.server.stop.wait(delay):
                    break
            with self.server.lock:
                row["end"] = "fixture-truncated" if mode in ("truncate", "stall") else "finished"
        except (BrokenPipeError, ConnectionResetError) as exc:
            with self.server.lock:
                row["end"] = "peer-closed (expected for pause/cancel/rejected response)"
                row["transport_note"] = type(exc).__name__
        except OSError as exc:
            with self.server.lock:
                row["end"], row["transport_error"] = "server-error", str(exc)


class Acceptance:
    def __init__(self, driver, server, models, results):
        self.driver, self.server, self.models, self.results = driver, server, models, results
        self.active = set()
        self.sequence = 0
        self.current = None
        self.expected_hash = digest(server.model)
        self.total = server.model.stat().st_size
        self.event_id = None

    def listen(self):
        self.event_id = self.driver.async_js("""
const done=arguments[arguments.length-1], t=window.__TAURI_INTERNALS__;
window.__downloadResult={}; window.__downloadEvents=[];
const handler=t.transformCallback(e=>window.__downloadEvents.push(e.payload));
window.__downloadHandler=handler;
t.invoke('plugin:event|listen',{event:'model-download-progress',target:{kind:'Any'},handler})
 .then(done).catch(e=>done({testError:String(e)}));
""")

    def start(self, url):
        self.sequence += 1
        task = f"fixture-{self.current}-{self.sequence}"
        self.active.add(task)
        self.results["cases"][self.current].setdefault("tasks", []).append(task)
        # execute/sync returns immediately, never the download Promise itself.
        value = self.driver.js("""
const [url,taskId]=arguments;
const result={settled:false,ok:null}; window.__downloadResult[taskId]=result;
window.__TAURI_INTERNALS__.invoke('hf_download_url',{url,taskId})
 .then(value=>Object.assign(result,{settled:true,ok:true,value}))
 .catch(e=>Object.assign(result,{settled:true,ok:false,error:String(e)}));
return {started:taskId};
""", url, task)
        assert value == {"started": task}, value
        return task

    def state(self, task):
        # Driver.call interprets a top-level {error: ...} as a W3C protocol error.
        # Return an array so a legitimate rejected IPC Promise remains observable.
        return self.driver.js("return [window.__downloadResult[arguments[0]]]", task)[0]

    def settle(self, task, seconds=12):
        result = wait(lambda: (state if (state := self.state(task))["settled"] else None), seconds)
        self.results["cases"][self.current].setdefault("promises", {})[task] = result
        self.active.discard(task)
        self.driver.invoke("hf_clear_download", {"taskId": task})
        return result

    def paths(self, url):
        dest = self.models / Path(urlsplit(url).path).name
        return dest, Path(str(dest) + ".part"), Path(str(dest) + ".part.cookllm.json")

    def growing(self, part):
        first = wait(lambda: part.stat().st_size if part.exists() and part.stat().st_size > 0 else None, 8)
        second = wait(lambda: part.stat().st_size if part.exists() and part.stat().st_size > first else None, 5)
        assert 0 < first < second < self.total, (first, second)
        return [first, second]

    def no_done(self, task):
        events = self.driver.js("return window.__downloadEvents.filter(e=>e.taskId===arguments[0])", task)
        assert not any(e["phase"] == "done" for e in events), events
        return events

    def success(self, task, dest):
        result = self.settle(task)
        assert result["ok"], result
        assert result["value"]["path"] == str(dest), result
        assert result["value"]["sizeBytes"] == self.total, result
        assert dest.stat().st_size == self.total and digest(dest) == self.expected_hash
        assert not Path(str(dest) + ".part").exists()
        assert not Path(str(dest) + ".part.cookllm.json").exists()
        events = wait(lambda: self.driver.js("return window.__downloadEvents.filter(e=>e.taskId===arguments[0]&&e.phase==='done')", task), 3)
        return {"sha256": digest(dest), "done_events": events, "result": result}

    def paused(self, name):
        url, route = self.server.route(name)
        dest, part, meta = self.paths(url)
        task = self.start(url)
        growth = self.growing(part)
        started = time.monotonic()
        self.driver.invoke("hf_pause_download", {"taskId": task})
        result = self.settle(task, 3)
        elapsed = time.monotonic() - started
        assert not result["ok"] and "暂停" in result["error"], result
        assert elapsed < 2, elapsed
        assert part.is_file() and meta.is_file() and not dest.exists()
        metadata = json.loads(meta.read_text())
        assert metadata["source"] == url and metadata["validator"] == self.server.requests(url)[0]["etag"]
        assert metadata["format"] and metadata["total"] == self.total, metadata
        sizes = [part.stat().st_size]
        for _ in range(3):
            time.sleep(0.25)
            sizes.append(part.stat().st_size)
        assert len(set(sizes)) == 1 and 0 < sizes[0] < self.total, sizes
        self.no_done(task)
        evidence = {"growth": growth, "pause_seconds": elapsed, "stable_partial_sizes": sizes, "metadata": metadata}
        self.results["cases"][self.current]["pause"] = evidence
        return url, route, dest, part, meta, sizes[0]

    def pause_resume(self):
        url, route, dest, part, meta, offset = self.paused("pause-resume")
        route["mode"] = "fast"
        task = self.start(url)
        evidence = self.success(task, dest)
        rows = self.server.requests(url)
        assert len(rows) == 2 and rows[1]["range"] == f"bytes={offset}-", rows
        assert rows[1]["if_range"] == rows[0]["etag"] and rows[1]["status"] == 206, rows
        return evidence

    def cancel(self, stall=False):
        url, route = self.server.route("cancel-stall" if stall else "cancel-stream", "stall" if stall else "slow")
        dest, part, meta = self.paths(url)
        shutil.copyfile(self.server.model, dest)  # old completed destination must survive
        before = digest(dest)
        task = self.start(url)
        if stall:
            wait(lambda: self.server.requests(url) and self.server.requests(url)[0]["sent"] >= 16384, 8)
            wait(lambda: part.exists() and part.stat().st_size > 0, 5)
            time.sleep(0.3)
            assert self.server.requests(url)[0]["sent"] == 16384, "stall fixture sent extra bytes"
        else:
            self.results["cases"][self.current]["growth"] = self.growing(part)
        started = time.monotonic()
        self.driver.invoke("hf_cancel_download", {"taskId": task})
        result = self.settle(task, 3)
        elapsed = time.monotonic() - started
        assert elapsed < 2 and not result["ok"] and "取消" in result["error"], (elapsed, result)
        assert not part.exists() and not meta.exists()
        time.sleep(0.8)
        assert not part.exists() and not meta.exists(), "cancelled files reappeared"
        assert dest.stat().st_size == self.total and digest(dest) == before
        self.no_done(task)
        route["release"].set()
        return {"cancel_seconds": elapsed, "observation_seconds": 0.8, "old_dest_sha256": before, "remaining_partial": False, "stall_first_block": 16384 if stall else None}

    def ignore_range(self):
        url, route, dest, part, meta, offset = self.paused("ignore-range")
        route["mode"] = "ignore"
        task = self.start(url)
        evidence = self.success(task, dest)
        row = self.server.requests(url)[-1]
        assert row["range"] == f"bytes={offset}-" and row["if_range"] == row["etag"]
        assert row["status"] == 200 and row["sent"] == self.total, row
        return evidence

    def invalid(self, resumed=False):
        if resumed:
            url, route, dest, part, meta, offset = self.paused("invalid-range")
            route["mode"] = "bad-range"
        else:
            url, route = self.server.route("truncated-body", "truncate")
            dest, part, meta = self.paths(url)
            offset = None
        shutil.copyfile(self.server.model, dest)
        before = digest(dest)
        task = self.start(url)
        result = self.settle(task, 8)
        assert not result["ok"], result
        self.no_done(task)
        assert dest.stat().st_size == self.total and digest(dest) == before, "old completed destination replaced on failure"
        row = self.server.requests(url)[-1]
        assert row["status"] == (206 if resumed else 200), row
        if resumed:
            assert row["range"] == f"bytes={offset}-", row
        else:
            assert row["sent"] == 65536, row
        return {"error": result["error"], "old_dest_sha256": before, "partial_size": part.stat().st_size if part.exists() else None}

    def parallel_delete(self):
        url, route, dest, part, meta, offset = self.paused("parallel-delete")
        adjacent = Path(str(dest) + ".unknown.json")
        unknown_dest = self.models / "unknown-json.gguf"
        unknown_part = Path(str(unknown_dest) + ".part")
        unknown_meta = Path(str(unknown_part) + ".cookllm.json")
        unknown = b'{"format":"not-cookllm","keep":true}\n'
        adjacent.write_bytes(unknown)
        unknown_part.write_bytes(b"fixture")
        unknown_meta.write_bytes(unknown)
        result = self.driver.async_js("""
const done=arguments[arguments.length-1];
Promise.all(arguments[0].map(path=>window.__TAURI_INTERNALS__.invoke('remove_local_file',{path})))
 .then(done).catch(e=>done({testError:String(e)}));
""", [str(dest), str(part)])
        assert not dest.exists() and not part.exists() and not meta.exists()
        self.driver.invoke("remove_local_file", {"path": str(unknown_part)})
        assert not unknown_part.exists() and unknown_meta.read_bytes() == unknown and adjacent.read_bytes() == unknown
        # Repeat parallel missing-file deletion to exercise idempotence.
        self.driver.async_js("const done=arguments[arguments.length-1];Promise.all(arguments[0].map(path=>window.__TAURI_INTERNALS__.invoke('remove_local_file',{path}))).then(done).catch(e=>done({testError:String(e)}));", [str(dest), str(part)])
        return {"parallel_ipc_results": result, "owned_sidecar_removed": True, "unknown_sidecar_preserved": str(unknown_meta), "adjacent_json_preserved": str(adjacent)}

    def target_conflict(self):
        url, route = self.server.route("target-conflict")
        dest, part, meta = self.paths(url)
        first = self.start(url)
        self.growing(part)
        second = self.start(url)
        rejected = self.settle(second, 3)
        assert not rejected["ok"] and "同一目标已有下载任务" in rejected["error"], rejected
        self.no_done(second)
        assert len(self.server.requests(url)) == 1, "conflicting writer issued HTTP request"
        assert not self.state(first)["settled"], "original writer stopped on conflict"
        route["mode"] = "fast"
        return {"conflict": rejected, "winner": self.success(first, dest)}

    def cleanup_active(self):
        errors = []
        for task in list(self.active):
            try:
                self.driver.invoke("hf_cancel_download", {"taskId": task})
                self.settle(task, 4)
            except Exception as exc:
                errors.append({"task": task, "error": str(exc)})
        return errors


def worker(args, root):
    def interrupted(signum, frame):
        raise KeyboardInterrupt("download fixture interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    results = {"status": "running", "cases": {}, "inputs": {name: {"path": str(getattr(args, name)), "sha256": digest(getattr(args, name))} for name in ("app", "model")}, "unverified": ["UI download buttons", "external HTTP/TLS, proxy and real HF tokens", "other distributions and Windows"]}
    driver, server, suite = None, None, None

    def save():
        (root / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2))

    try:
        app = root / "cookllm"
        shutil.copy2(args.app, app)
        models, served = root / "fixturemodels", root / "served"
        models.mkdir()
        served.mkdir()
        # Only startup switches are seeded; the actual schema comes from load_config.
        config_dir = Path(os.environ["XDG_CONFIG_HOME"]) / "dev.cookllm.launcher"
        config_dir.mkdir()
        seed = {"serverPath": "", "models": [], "autoUpdateEnabled": False, "gpuMonitorEnabled": False, "hfToken": "", "network": {"proxyMode": "direct", "proxyUrl": ""}, "modelsDir": str(models)}
        (config_dir / "config.json").write_text(json.dumps(seed))
        server = FixtureServer(served, args.model)
        results["http_port"] = server.server_port
        results["isolation"] = {key: os.environ[key] for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "GDK_BACKEND", "WEBKIT_DISABLE_DMABUF_RENDERER", "TAURI_WEBVIEW_AUTOMATION")}
        driver = desktop.Driver(root, os.environ.copy(), app)
        results["session"] = driver.start()
        results["driver_port"] = int(driver.base.rsplit(":", 1)[1])
        config = driver.invoke("load_config")
        assert config["autoUpdateEnabled"] is False and not config.get("hfToken"), config
        network = dict(config.get("network") or {})
        network.update(proxyMode="direct", proxyUrl="")
        config.update(network=network, modelsDir=str(models), hfToken="", autoUpdateEnabled=False, gpuMonitorEnabled=False, minimizeToTrayOnClose=False)
        driver.invoke("save_config", {"config": config})
        loaded = driver.invoke("load_config")
        assert loaded["network"]["proxyMode"] == "direct" and loaded["modelsDir"] == str(models) and not loaded.get("hfToken")
        results["config"] = loaded
        suite = Acceptance(driver, server, models, results)
        suite.listen()
        results["event_listener"] = "real plugin:event|listen model-download-progress"
        functions = (suite.pause_resume, suite.cancel, lambda: suite.cancel(True), suite.ignore_range, suite.invalid, lambda: suite.invalid(True), suite.parallel_delete, suite.target_conflict)
        for name, fn in zip(CASES, functions):
            suite.current = name
            results["cases"][name] = {"status": "running"}
            print("CASE " + name, flush=True)
            try:
                evidence = fn()
                results["cases"][name].update(status="passed", evidence=evidence)
            except Exception:
                results["cases"][name].update(status="failed", traceback=traceback.format_exc())
                print(traceback.format_exc(), flush=True)
            finally:
                errors = suite.cleanup_active()
                if errors:
                    results["cases"][name].update(status="failed", cleanup_errors=errors)
                results["cases"][name]["http"] = [dict(row) for row in server.records]
                save()
        results["events"] = driver.js("return window.__downloadEvents")
        results["status"] = "passed" if all(row["status"] == "passed" for row in results["cases"].values()) else "failed"
    except BaseException:
        results.update(status="failed", traceback=traceback.format_exc())
        print(results["traceback"], flush=True)
    finally:
        # A repeated controller TERM must not interrupt resource cleanup halfway.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        cleanup_errors = []
        if suite:
            cleanup_errors.extend(suite.cleanup_active())
            if suite.event_id is not None:
                try:
                    driver.invoke("plugin:event|unlisten", {"event": "model-download-progress", "eventId": suite.event_id})
                except Exception as exc:
                    cleanup_errors.append({"event_unlisten": str(exc)})
        if driver:
            try:
                driver.close()
            except Exception as exc:
                cleanup_errors.append({"driver_close": str(exc)})
                for group in driver.groups:
                    kill_group(group)
        if server:
            try:
                server.close()
            except Exception as exc:
                cleanup_errors.append({"server_close": str(exc)})
            results["http_requests"] = server.records
            if any(row.get("transport_error") or row["authorization_present"] for row in server.records):
                cleanup_errors.append({"http": "unexpected transport error or Authorization header"})
        ports = [server.server_port] if server else []
        if driver and hasattr(driver, "base"):
            ports.append(int(driver.base.rsplit(":", 1)[1]))
        released = {}
        for number in ports:
            try:
                with socket.socket() as probe:
                    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    probe.bind(("127.0.0.1", number))
                released[str(number)] = True
            except OSError as exc:
                released[str(number)] = str(exc)
                cleanup_errors.append({"port": number, "error": str(exc)})
        results["cleanup"] = {"errors": cleanup_errors, "ports_released": released, "driver_groups": {str(group): desktop.members(group) for group in driver.groups} if driver else {}, "http_threads": [t.name for t in threading.enumerate() if t.name == "download-http" or "process_request_thread" in t.name]}
        if cleanup_errors or any(results["cleanup"]["driver_groups"].values()) or results["cleanup"]["http_threads"]:
            results["status"] = "failed"
        for name in CASES:
            results["cases"].setdefault(name, {"status": "not-run", "reason": "setup interrupted"})
        save()
    print(json.dumps({"status": results["status"], "cases": {name: row["status"] for name, row in results["cases"].items()}}, ensure_ascii=False), flush=True)
    return int(results["status"] != "passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("app", "model", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    for name in ("app", "model"):
        value = getattr(args, name).resolve(strict=True)
        if not value.is_file():
            parser.error(name + " must name an existing local file")
        setattr(args, name, value)
    if args.model.stat().st_size < 131072 or args.model.read_bytes()[:4] != b"GGUF":
        parser.error("model must be a local GGUF of at least 128 KiB")
    root = args.output.resolve()
    if not root.is_relative_to(Path("/tmp")) or root == Path("/tmp"):
        parser.error("output must be a dedicated empty /tmp directory")
    if args.worker:
        if os.environ.get("COOKLLM_DOWNLOADS_WORKER") != str(root):
            parser.error("worker is private to the controller")
        return worker(args, root)
    os.umask(0o077)
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(root.iterdir()):
        parser.error("output must be empty")
    root.chmod(0o700)
    env = os.environ.copy()
    # AppImage launchers may inject loader paths, argv[0] and a malformed PATH.
    for key in list(env):
        if key.startswith(("LD", "LLAMA_ARG_", "DBUS_")) or key in ("APPIMAGE", "APPDIR", "ARGV0", "WAYLAND_DISPLAY", "NIRI_SOCKET", "NIRI_CONFIG", "XDG_CONFIG_DIRS", "XDG_DATA_DIRS") or "proxy" in key.lower() or "token" in key.lower():
            env.pop(key)
    env.update(PATH="/usr/local/bin:/usr/bin:/bin", GDK_BACKEND="x11", WEBKIT_DISABLE_DMABUF_RENDERER="1", TAURI_WEBVIEW_AUTOMATION="true", NO_AT_BRIDGE="1", GTK_USE_PORTAL="0", COOKLLM_DOWNLOADS_WORKER=str(root), PYTHONDONTWRITEBYTECODE="1")
    if "XAUTHORITY" not in env and (Path.home() / ".Xauthority").is_file():
        env["XAUTHORITY"] = str(Path.home() / ".Xauthority")
    for key, name in {"HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data", "XDG_CACHE_HOME": "cache", "XDG_STATE_HOME": "state", "XDG_RUNTIME_DIR": "runtime"}.items():
        folder = root / name
        folder.mkdir(mode=0o700)
        env[key] = str(folder)
    bus = root / "dbus.conf"
    bus.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + str(root / "runtime") + '</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    command = ["/usr/bin/dbus-run-session", "--config-file=" + str(bus), "--", "/usr/bin/python3", str(Path(__file__).resolve()), "--worker"]
    for name in ("app", "model", "output"):
        command += ["--" + name, str(getattr(args, name))]
    code, process = 1, None
    try:
        with (root / "controller.log").open("wb") as log:
            process = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
        code = process.wait(timeout=180)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        if process:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                kill_group(process.pid)
        code = 1
    finally:
        if process:
            kill_group(process.pid)
            process.wait(timeout=5)
        # Children in independent driver sessions are selected by fixture runtime only.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for pid in fixture_pids(root):
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
            time.sleep(0.3)
        path = root / "results.json"
        results = json.loads(path.read_text()) if path.exists() else {"status": "failed", "error": "worker exited before results", "cases": {name: {"status": "not-run"} for name in CASES}}
        results["outer_cleanup"] = {"dbus_group": process.pid if process else None, "members": desktop.members(process.pid) if process else [], "fixture_pids": fixture_pids(root), "controller_returncode": code}
        if code or results["outer_cleanup"]["members"] or results["outer_cleanup"]["fixture_pids"]:
            results["status"] = "failed"
        for name in CASES:
            results["cases"].setdefault(name, {"status": "not-run", "reason": "controller interrupted"})
        # Recheck listeners after all private DBus/GUI processes have exited.
        numbers = set(results.get("cleanup", {}).get("ports_released", {}))
        numbers.update(str(results[key]) for key in ("http_port", "driver_port") if key in results)
        results["outer_cleanup"]["ports_released"] = {}
        for number in numbers:
            try:
                with socket.socket() as probe:
                    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    probe.bind(("127.0.0.1", int(number)))
                results["outer_cleanup"]["ports_released"][number] = True
            except OSError as exc:
                results["status"] = "failed"
                results["outer_cleanup"].setdefault("port_errors", {})[number] = str(exc)
        persistent = Path(__file__).resolve().parents[1] / ".tmp" / "linux-validation" / ("downloads-" + root.name)
        persistent.mkdir(parents=True, exist_ok=False)
        results["output"] = str(root)
        results["persistent"] = str(persistent)
        path.write_text(json.dumps(results, ensure_ascii=False, indent=2))
        for source in [path, *root.glob("*.log")]:
            shutil.copy2(source, persistent / source.name)
    print(json.dumps({"status": results["status"], "cases": {name: row["status"] for name, row in results["cases"].items()}, "results": str(path), "persistent": str(persistent)}, ensure_ascii=False))
    return int(results["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())
