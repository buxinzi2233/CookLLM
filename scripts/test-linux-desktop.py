#!/usr/bin/env python3
"""Real Linux acceptance, stdlib only; no download, build, clipboard or user config.

Run under dbus-run-session. All four arguments must name existing local files or
an empty /tmp output directory. This exercises real HTTP and Tauri IPC, not mocks.
"""
import argparse
import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import traceback
import urllib.request

HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def request(url, method="GET", data=None, timeout=30):
    body = None if data is None else json.dumps(data).encode()
    req = urllib.request.Request(url, body, {"Content-Type": "application/json"}, method=method)
    try:
        response = HTTP.open(req, timeout=timeout)
    except urllib.error.HTTPError as exc:
        raise RuntimeError(f"HTTP {exc.code}: {exc.read().decode()}") from exc
    with response:
        raw = response.read()
        if "json" in response.headers.get("Content-Type", ""):
            return json.loads(raw)
        return raw.decode()


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait(fn, seconds=30):
    deadline = time.monotonic() + seconds
    error = None
    while time.monotonic() < deadline:
        try:
            value = fn()
            if value:
                return value
        except Exception as exc:
            error = exc
        time.sleep(0.3)
    raise TimeoutError(str(error or "condition not reached"))


def members(group):
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            if int(fields[2]) == group and fields[0] != "Z":
                found.append(int(entry.name))
        except (OSError, ValueError, IndexError):
            pass
    return found


def release_check(number, group):
    def released():
        if members(group):
            return False
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("127.0.0.1", number))
        return True
    wait(released, 10)
    return {"port": number, "group": group, "live_members": members(group), "bind_released": True}


def kill_group(group):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if not members(group):
            return
        try:
            os.killpg(group, sig)
        except ProcessLookupError:
            return
        deadline = time.monotonic() + 2
        while members(group) and time.monotonic() < deadline:
            time.sleep(0.1)


class Driver:
    def __init__(self, root, env, app):
        self.root, self.env, self.app = root, env, app
        self.process = None
        self.session = None
        self.groups = []
        self.count = 0

    def start(self):
        self.count += 1
        number = port()
        self.base = f"http://127.0.0.1:{number}"
        with (self.root / f"driver-{self.count}.log").open("wb") as log:
            self.process = subprocess.Popen(["/usr/bin/WebKitWebDriver", f"--port={number}", "--host=127.0.0.1"], env=self.env, stdout=log, stderr=log, start_new_session=True)
        self.groups.append(self.process.pid)
        wait(lambda: request(self.base + "/status"), 10)
        value = request(self.base + "/session", "POST", {"capabilities": {"alwaysMatch": {"webkitgtk:browserOptions": {"binary": str(self.app), "args": []}}}}, timeout=45)
        self.session = value["value"]["sessionId"]
        self.call("/timeouts", {"script": 30000})
        wait(lambda: self.js("return !!window.__TAURI_INTERNALS__ && document.body.innerText.length > 30"))
        return value

    def call(self, path, data=None, method="POST"):
        result = request(self.base + "/session/" + self.session + path, method, data)
        value = result.get("value")
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(value)
        return value

    def js(self, script, *args):
        return self.call("/execute/sync", {"script": script, "args": list(args)})

    def async_js(self, script, *args):
        value = self.call("/execute/async", {"script": script, "args": list(args)})
        if isinstance(value, dict) and "testError" in value:
            raise RuntimeError(value["testError"])
        return value

    def invoke(self, name, args=None):
        return self.async_js("const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done).catch(e=>done({testError:String(e)}));", name, args or {})

    def screenshot(self, name):
        raw = self.call("/screenshot", method="GET")
        (self.root / name).write_bytes(base64.b64decode(raw))

    def close(self):
        if self.session:
            try:
                self.call("", method="DELETE")
            except Exception:
                pass
            self.session = None
        if self.process:
            kill_group(self.process.pid)
            self.process.wait(timeout=5)
            self.process = None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("app", "engine", "model", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args()
    for name in ("app", "engine", "model"):
        value = getattr(args, name).resolve(strict=True)
        if not value.is_file():
            parser.error(f"{name} must be a file")
        setattr(args, name, value)
    root = args.output.resolve()
    if not root.is_relative_to(Path("/tmp")) or root == Path("/tmp"):
        parser.error("output must be a dedicated /tmp directory")
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        parser.error("output must be empty; choose a new directory")
    env = os.environ.copy()
    removed = {}
    for key in list(env):
        if key in ("APPIMAGE", "APPDIR", "ARGV0", "LD_LIBRARY_PATH", "LD_PRELOAD") or key.startswith("LLAMA_ARG_"):
            removed[key] = "removed"
            env.pop(key)
    overrides = {"PATH": "/usr/local/bin:/usr/bin:/bin", "TAURI_WEBVIEW_AUTOMATION": "true", "WEBKIT_DISABLE_DMABUF_RENDERER": "1", "GDK_BACKEND": "x11", "OMP_NUM_THREADS": "2"}
    for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        folder = root / key.lower()
        folder.mkdir()
        overrides[key] = str(folder)
    env.update(overrides)
    app = root / "cookllm"
    shutil.copy2(args.app, app)
    models = root / "模型 fixture"
    models.mkdir()
    model_paths = [models / "Tiny.gguf", models / "tiny.gguf"]
    for target in model_paths:
        shutil.copy2(args.model, target)
    results = {"environment": overrides, "removed_environment": removed, "inputs": {name: {"path": str(getattr(args, name)), "sha256": hashlib.sha256(getattr(args, name).read_bytes()).hexdigest()} for name in ("app", "engine", "model")}, "unverified": ["clipboard (preserve user clipboard)", "file manager launch", "tray menu", "Windows and other distributions", "GPU inference"]}
    groups = []
    driver = Driver(root, env, app)
    direct = None

    def save():
        (root / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2))

    def chat(number):
        base = f"http://127.0.0.1:{number}"
        health = wait(lambda: request(base + "/health"))
        result = request(base + "/v1/chat/completions", "POST", {"messages": [{"role": "user", "content": "Once upon a time"}], "max_tokens": 16, "temperature": 0, "seed": 1})
        assert result["usage"]["completion_tokens"] > 0, result
        assert result["choices"][0]["message"]["content"], result
        metrics = request(base + "/metrics")
        assert "llamacpp:" in metrics, metrics
        return {"health": health, "chat": result, "metrics": metrics}

    try:
        number = port()
        command = [str(args.engine), "-m", str(model_paths[0]), "--host", "127.0.0.1", "--port", str(number), "-ngl", "0", "-c", "512", "-t", "2", "-np", "1", "-n", "16", "--metrics", "--no-jinja", "--chat-template", "chatml"]
        with (root / "direct-server.log").open("wb") as log:
            direct = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
        groups.append(direct.pid)
        results["direct"] = {"command": command, **chat(number)}
        kill_group(direct.pid)
        direct.wait(timeout=5)
        results["direct"]["cleanup"] = release_check(number, direct.pid)
        save()
        results["driver_session"] = driver.start()
        config = driver.invoke("load_config")
        (root / "initial-config.json").write_text(json.dumps(config, ensure_ascii=False, indent=2))
        expanded = driver.invoke("expand_paths", {"paths": [str(p) for p in model_paths]})
        assert len(expanded) == 2 and len({p["path"] for p in expanded}) == 2, expanded
        number = port()
        profile = copy.deepcopy(config["profiles"][0])
        profile.update(id="fixture-profile", name="CPU 512 / 2 threads", host="127.0.0.1", port=number, gpuLayers=0, contextSize=512, threads=2, parallel=1, batchSize=128, ubatchSize=128, flashAttention=False, jinja=False, reasoning="off", reasoningEffort="none", loadMode="auto", cacheTypeK="f16", cacheTypeV="f16", extraArgs="--metrics --no-jinja --chat-template chatml -n 16", engineId="fixture-engine")
        config.update(serverPath=str(args.engine), activeEngineId="fixture-engine", engines=[{"id": "fixture-engine", "name": "Official CPU fixture", "path": str(args.engine), "backend": "cpu", "engineType": "llamacpp"}], models=[{"id": f"fixture-model-{i}", "name": p.name, "path": str(p), "sizeBytes": p.stat().st_size, "architecture": "llama", "quantization": "F32", "parameters": "260K", "profiles": [copy.deepcopy(profile)], "defaultProfileId": profile["id"], "accent": "cyan"} for i, p in enumerate(model_paths)], profiles=[], preferredModelId="fixture-model-0", preferredProfileId=profile["id"], modelsDir=str(models), autoUpdateEnabled=False, gpuMonitorEnabled=False, minimizeToTrayOnClose=False, language="en")
        driver.invoke("save_config", {"config": config})
        loaded = driver.invoke("load_config")
        assert len(loaded["models"]) == 2
        results["config_import"] = {"expanded": expanded, "model_paths": [m["path"] for m in loaded["models"]]}
        driver.call("/refresh", {})
        wait(lambda: driver.js("return document.body.innerText.includes('Tiny.gguf')"))
        driver.screenshot("models.png")
        results["cycles"] = []
        for i in range(2):
            status = driver.invoke("start_server", {"modelId": f"fixture-model-{i}", "profileId": profile["id"]})
            group = os.getpgid(status["pid"])
            groups.append(group)
            cycle = {"start": status, "process_group": group}
            results["cycles"].append(cycle)
            cycle.update(chat(number))
            if i == 0:
                time.sleep(3)
                cycle["ui_text"] = driver.js("return document.body.innerText")
                cycle["iframe_sources"] = driver.js("return Array.from(document.querySelectorAll('iframe')).map(x=>x.src)")
                driver.js("const b=Array.from(document.querySelectorAll('button')).find(x=>/^(Playground|Chat|会话)$/.test(x.innerText.trim())); if(b)b.click(); return !!b")
                frame = wait(lambda: driver.js("return document.querySelector('iframe')"), 10)
                driver.call("/frame", {"id": frame})
                cycle["iframe_dom"] = wait(lambda: driver.js("return document.body.innerText.length > 20 && {url:location.href,title:document.title,text:document.body.innerText}"), 20)
                cycle["iframe_health"] = driver.async_js("const done=arguments[arguments.length-1]; fetch('/health').then(r=>r.json()).then(done).catch(e=>done({testError:String(e)}));")
                cycle["iframe_chat"] = driver.async_js("const done=arguments[arguments.length-1]; fetch('/v1/chat/completions',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({messages:[{role:'user',content:'Once upon a time'}],max_tokens:16,temperature:0,seed:1})}).then(async r=>({status:r.status,body:await r.json()})).then(done).catch(e=>done({testError:String(e)}));")
                assert cycle["iframe_chat"]["status"] == 200
                assert cycle["iframe_chat"]["body"]["choices"][0]["message"]["content"]
                driver.call("/frame", {"id": None})
                driver.screenshot("running.png")
                assert driver.js("const b=Array.from(document.querySelectorAll('button')).find(x=>x.innerText.trim()==='Logs'); if(b)b.click(); return !!b")
                cycle["logs_ui"] = wait(lambda: driver.js("return document.body.innerText.includes('llama') && document.body.innerText"), 10)
                driver.screenshot("logs.png")
            cycle["stop"] = driver.invoke("stop_server")
            cycle["cleanup"] = release_check(number, group)
            save()
        driver.close()
        driver.start()
        restored = driver.invoke("load_config")
        assert [m["path"] for m in restored["models"]] == [str(p) for p in model_paths]
        assert restored["engines"] == loaded["engines"]
        results["restart_persistence"] = {"model_paths": [m["path"] for m in restored["models"]], "engine": restored["engines"]}
        results["status"] = "passed"
    except Exception:
        results["status"] = "failed"
        results["error"] = traceback.format_exc()
        if driver.session:
            try:
                results["failure_ui"] = driver.js("return document.body.innerText")
                driver.screenshot("failure.png")
            except Exception as exc:
                results["failure_capture_error"] = str(exc)
    finally:
        if driver.session:
            try:
                driver.invoke("stop_server")
            except Exception:
                pass
        for group in groups:
            kill_group(group)
        if direct:
            direct.wait(timeout=5)
        driver.close()
        results["cleanup_groups"] = {str(g): members(g) for g in groups + driver.groups}
        save()
    print(json.dumps({"status": results["status"], "results": str(root / "results.json"), "error": results.get("error")}, ensure_ascii=False))
    return 0 if results["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
