#!/usr/bin/env python3
"""Real two-AppImage Linux upgrade acceptance (Python stdlib + WebKitWebDriver).

Uses simulated official release metadata through a loopback-only CONNECT fixture;
never contacts GitHub. Supply existing old/new AppImages and distinct native versions.
No build, release, package installation, user configuration, or certificate-store edit.
Example: python3 scripts/test-linux-upgrade.py --old-app /path/old.AppImage
 --new-app /path/new.AppImage --old-version 0.3.2 --new-version 0.3.3
 --output /tmp/upgrade-new-empty-directory
The optional --engine/--model pair adds a real managed-server shutdown assertion.
"""
import argparse
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import socketserver
import ssl
import subprocess
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('upgrade_desktop', Path(__file__).with_name('test-linux-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)
wait, request, kill_group = desktop.wait, desktop.request, desktop.kill_group


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def write_json(path, data):
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')


def processes(root, images=()):
    """Only this fixture's environment or exact AppImage device/inode, never names."""
    marker = ('XDG_RUNTIME_DIR=' + str(root / 'runtime')).encode()
    inodes = {(p.stat().st_dev, p.stat().st_ino) for p in images if p.exists()}
    found = {}
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            fields = (entry / 'stat').read_text().rsplit(') ', 1)[1].split()
            if fields[0] == 'Z':
                continue
            # The isolated bus belongs to the outer controller, not GUI cleanup.
            executable = os.readlink(entry / 'exe')
            if Path(executable).name in ('dbus-daemon', 'dbus-run-session'):
                continue
            owned = marker in (entry / 'environ').read_bytes().split(b'\0')
            exe = (entry / 'exe').stat()
            if owned or (exe.st_dev, exe.st_ino) in inodes:
                found[int(entry.name)] = {'exe': executable, 'group': int(fields[2]), 'starttime': fields[19], 'exe_device': exe.st_dev, 'exe_inode': exe.st_ino}
        except (OSError, ValueError, IndexError):
            pass
    return found


def gui_pids(root, images=()):
    return {p: row for p, row in processes(root, images).items() if Path(row['exe']).name == 'cookllm'}


def terminate_fixture(root, images):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in processes(root, images):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
        time.sleep(.4)
    return processes(root, images)


def certificates(root):
    cert = root / 'cert'; cert.mkdir()
    commands = [
        ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=CookLLM local upgrade fixture CA', '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign,cRLSign', '-keyout', 'ca.key', '-out', 'ca.pem'],
        ['req', '-new', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=api.github.com', '-keyout', 'server.key', '-out', 'server.csr'],
        ['x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial', '-days', '1', '-extfile', 'server.ext', '-out', 'server.pem'],
    ]
    (cert / 'server.ext').write_text('subjectAltName=DNS:api.github.com,DNS:github.com\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
    with (cert / 'openssl.log').open('wb') as log:
        for command in commands:
            subprocess.run(['/usr/bin/openssl', *command], cwd=cert, stdout=log, stderr=log, check=True, timeout=20)
    (cert / 'empty').mkdir()
    return cert


class TLSResponse(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def do_GET(self):
        allowed = (self.host, self.path) in self.fixture.routes
        row = {'timestamp': time.time(), 'host': self.host, 'path': self.path, 'method': 'GET', 'status': 200 if allowed else 403, 'authorization_present': bool(self.headers.get('Authorization')), 'sent': 0}
        with self.fixture.lock:
            self.fixture.records.append(row)
        if not allowed or row['authorization_present']:
            self.send_error(403); self.close_connection = True; return
        payload = self.fixture.routes[(self.host, self.path)]
        self.send_response(200)
        self.send_header('Content-Type', 'application/json' if isinstance(payload, bytes) else 'application/octet-stream')
        self.send_header('Content-Length', str(len(payload) if isinstance(payload, bytes) else payload.stat().st_size))
        self.send_header('Connection', 'close'); self.end_headers()
        self.close_connection = True
        if isinstance(payload, bytes):
            self.wfile.write(payload); row['sent'] = len(payload)
        else:
            with payload.open('rb') as source:
                while not self.fixture.stop.is_set():
                    chunk = source.read(256 * 1024)
                    if not chunk: break
                    self.wfile.write(chunk); row['sent'] += len(chunk)
        self.wfile.flush()


class Connect(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(10)
        raw = bytearray()
        while not raw.endswith(b'\r\n\r\n') and len(raw) < 8192:
            value = self.request.recv(1)
            if not value: return
            raw.extend(value)
        line = bytes(raw).split(b'\r\n', 1)[0].decode('ascii', 'replace').split()
        host = line[1][:-4] if len(line) == 3 and line[1].endswith(':443') else ''
        allowed = len(line) == 3 and line[0] == 'CONNECT' and host in ('api.github.com', 'github.com') and b'authorization:' not in raw.lower()
        with self.server.lock:
            self.server.records.append({'timestamp': time.time(), 'method': 'CONNECT', 'host': host, 'status': 200 if allowed else 403})
        if not allowed:
            self.request.sendall(b'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'); return
        self.request.sendall(b'HTTP/1.1 200 Connection established\r\n\r\n')
        try:
            with self.server.tls.wrap_socket(self.request, server_side=True) as stream:
                handler = type('BoundTLS', (TLSResponse,), {'host': host, 'fixture': self.server})
                handler(stream, self.client_address, self.server)
        except (OSError, ssl.SSLError) as exc:
            with self.server.lock:
                self.server.records.append({'host': host, 'transport_error': type(exc).__name__})


class Proxy(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = False
    block_on_close = True

    def __init__(self, cert, routes):
        self.routes, self.records = routes, []
        self.lock, self.stop = threading.Lock(), threading.Event()
        self.tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.tls.load_cert_chain(cert / 'server.pem', cert / 'server.key')
        super().__init__(('127.0.0.1', 0), Connect)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={'poll_interval': .1})
        self.thread.start()

    def close(self):
        self.stop.set(); self.shutdown(); self.thread.join(timeout=3); self.server_close()
        assert not self.thread.is_alive()


def inspector_endpoint(pid):
    entries = (Path('/proc') / str(pid) / 'environ').read_bytes().split(b'\0')
    values = [item.decode().split('=', 1)[1] for item in entries if item.startswith(b'WEBKIT_INSPECTOR_SERVER=')]
    assert len(values) == 1 and values[0].startswith('127.0.0.1:'), values
    assert values[0].rsplit(':', 1)[1].isdigit()
    return values[0]


def inspector_binding(pid, endpoint):
    """Kernel proof that the attached loopback listener belongs to this GUI."""
    host, number = endpoint.rsplit(':', 1)
    assert host == '127.0.0.1' and number.isdigit()
    local = '0100007F:' + f'{int(number):04X}'
    rows = [line.split() for line in (Path('/proc') / str(pid) / 'net/tcp').read_text().splitlines()[1:]]
    listeners = [row for row in rows if row[1] == local and row[3] == '0A']
    assert len(listeners) == 1, listeners
    inode = listeners[0][9]
    descriptors = []
    for fd in (Path('/proc') / str(pid) / 'fd').iterdir():
        try:
            if os.readlink(fd) == 'socket:[' + inode + ']': descriptors.append(fd.name)
        except FileNotFoundError:
            pass
    assert descriptors, f'GUI {pid} does not own inspector listener {inode}'
    return {'pid': pid, 'endpoint': endpoint, 'socket_inode': inode, 'owned_fds': descriptors}


class AttachedDriver(desktop.Driver):
    """Attach to the helper-started GUI; never launch or replace an app process."""
    def start_at(self, endpoint):
        self.root.mkdir(mode=0o700)
        number = desktop.port()
        self.base = f'http://127.0.0.1:{number}'
        command = ['/usr/bin/WebKitWebDriver', f'--port={number}', '--host=127.0.0.1', '--target=' + endpoint]
        write_json(self.root / 'command.json', {'command': command, 'cwd': str(self.root)})
        with (self.root / 'driver.log').open('wb') as log:
            self.process = subprocess.Popen(command, cwd=self.root, env=self.env, stdout=log, stderr=log, start_new_session=True)
        self.groups.append(self.process.pid)
        wait(lambda: request(self.base + '/status'), 10)
        value = request(self.base + '/session', 'POST', {'capabilities': {'alwaysMatch': {'webkitgtk:browserOptions': {'binary': str(self.app), 'args': []}}}}, timeout=40)
        self.session = value['value']['sessionId']
        self.call('/timeouts', {'script': 30000})
        wait(lambda: self.js('return !!window.__TAURI_INTERNALS__ && document.body.innerText.length > 30'), 20)
        return value


def worker(args, root):
    target = root / 'install' / 'CookLLM-upgrade.AppImage'
    target.parent.mkdir(mode=0o700)
    shutil.copy2(args.old_app, target); target.chmod(0o700)
    cert = certificates(root)
    name = f'CookLLM_{args.new_version}_amd64.AppImage'
    asset_path = f'/Crvesoft/CookLLM/releases/download/v{args.new_version}/{name}'
    url = 'https://github.com' + asset_path
    newhash = digest(args.new_app)
    metadata = {'tag_name': 'v' + args.new_version, 'html_url': 'https://github.com/Crvesoft/CookLLM/releases/tag/v' + args.new_version, 'body': 'Local synthetic release metadata; real AppImage payload', 'assets': [{'name': name, 'browser_download_url': url, 'size': args.new_app.stat().st_size, 'digest': 'sha256:' + newhash}]}
    write_json(root / 'release-fixture.json', metadata)
    routes = {('api.github.com', '/repos/Crvesoft/CookLLM/releases/' + suffix): json.dumps(metadata).encode() for suffix in ('latest', 'tags/v' + args.new_version)}
    routes[('github.com', asset_path)] = args.new_app
    proxy = Proxy(cert, routes)
    proxy_url = f'http://127.0.0.1:{proxy.server_address[1]}'
    env = os.environ.copy()
    env.update(SSL_CERT_FILE=str(cert / 'ca.pem'), SSL_CERT_DIR=str(cert / 'empty'), HTTPS_PROXY=proxy_url, HTTP_PROXY=proxy_url, ALL_PROXY=proxy_url, NO_PROXY='localhost,127.0.0.1')
    driver = desktop.Driver(root, env, target)
    attached = None
    results = {'status': 'running', 'boundary': 'simulated official release metadata; real two-package integration, not an official online release test', 'inputs': {'old': {'path': str(args.old_app), 'sha256': digest(args.old_app)}, 'new': {'path': str(args.new_app), 'sha256': newhash}}, 'versions': {}, 'timestamps': {}, 'proxy_port': proxy.server_address[1], 'unverified': ['official online releases', 'deb/rpm installation', 'Windows', 'new-image failure rollback (existing unit tests)']}
    models = root / 'models'; models.mkdir()
    model = models / 'Upgrade fixture.gguf'
    if args.model: shutil.copy2(args.model, model)
    else: model.write_bytes(b'GGUF-upgrade-retention-fixture-not-an-inference-model\n')
    cfgdir = Path(env['XDG_CONFIG_HOME']) / 'dev.cookllm.launcher'; cfgdir.mkdir()
    cfgpath = cfgdir / 'config.json'
    seed = {'serverPath': '', 'models': [], 'autoUpdateEnabled': False, 'gpuMonitorEnabled': False, 'minimizeToTrayOnClose': False, 'hfToken': '', 'network': {'proxyMode': 'manual', 'proxyUrl': proxy_url}, 'modelsDir': str(models)}
    write_json(cfgpath, seed)
    backup = None
    monitor_stop = threading.Event()
    observations = []

    def save():
        write_json(root / 'results.json', results)

    def observe():
        seen = set()
        while not monitor_stop.wait(.05):
            guis = gui_pids(root, [target, *target.parent.glob('.cookllm-app-update-*/old.AppImage')])
            observations.append({'timestamp': time.time(), 'guis': guis})
            for pid in set(guis) - seen:
                seen.add(pid)
                try:
                    mounts = (Path('/proc') / str(pid) / 'mountinfo').read_text()
                    write_json(root / f'gui-{pid}-binding.json', {'timestamp': time.time(), 'gui': guis[pid], 'target_sha256': digest(target), 'fixture_mounts': [line for line in mounts.splitlines() if ' - fuse.' + target.name + ' ' in line], 'fixture_processes': processes(root, [target, *target.parent.glob('.cookllm-app-update-*/old.AppImage')])})
                except OSError:
                    pass

    thread = None
    before, config_hash, model_hash = None, None, digest(model)
    try:
        results['session'] = driver.start()
        results['driver_port'] = int(driver.base.rsplit(':', 1)[1])
        results['versions']['old_native'] = driver.invoke('plugin:app|version')
        assert results['versions']['old_native'] == args.old_version
        old = wait(lambda: gui_pids(root, [target]), 10)
        assert len(old) == 1, old
        results['old_gui'] = old
        results['old_inspector_endpoint'] = inspector_endpoint(next(iter(old)))
        config = driver.invoke('load_config')
        profile = {'description': 'Private upgrade acceptance fixture', 'temperature': 0.7, 'topP': 0.9, 'minP': 0.05, 'repeatPenalty': 1.1, 'loadMode': 'auto'}
        profile.update(id='upgrade-profile', name='Upgrade retained context 512', host='127.0.0.1', port=desktop.port(), contextSize=512, gpuLayers=0, threads=2, parallel=1, batchSize=128, ubatchSize=128, cacheTypeK='f16', cacheTypeV='f16', jinja=False, flashAttention=False, reasoning='off', reasoningEffort='none', extraArgs='--no-jinja --chat-template chatml -n 16', engineId='upgrade-engine' if args.engine else None)
        config.update(models=[{'id': 'upgrade-model', 'name': 'Upgrade fixture', 'path': str(model), 'sizeBytes': model.stat().st_size, 'architecture': 'llama', 'quantization': 'F32', 'parameters': 'fixture', 'profiles': [profile], 'defaultProfileId': profile['id'], 'accent': 'cyan'}], profiles=[], preferredModelId='upgrade-model', preferredProfileId=profile['id'], theme='dark', language='en', customTags=['upgrade-retained'], exploreSidebarCollapsed=True)
        if args.engine:
            config.update(serverPath=str(args.engine), activeEngineId='upgrade-engine', engines=[{'id': 'upgrade-engine', 'name': 'CPU fixture', 'path': str(args.engine), 'backend': 'cpu', 'engineType': 'llamacpp'}])
        driver.invoke('save_config', {'config': config})
        driver.call('/refresh', {})
        wait(lambda: driver.js("return document.body.innerText.includes('Upgrade fixture')"), 20)
        before = driver.invoke('load_config')
        write_json(root / 'config-before.json', before)
        config_hash, model_hash = digest(cfgpath), digest(model)
        results['config_before_sha256'] = config_hash
        results['model_before_sha256'] = model_hash
        driver.screenshot('old-gui.png')
        # An actual second launch must exit and retain the original single GUI.
        with (root / 'duplicate-old.log').open('wb') as log:
            duplicate = subprocess.Popen([str(target)], cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
        assert duplicate.wait(timeout=15) == 0
        assert gui_pids(root, [target]) == old
        results['single_instance_old'] = True
        if args.engine:
            status = driver.invoke('start_server', {'modelId': 'upgrade-model', 'profileId': profile['id']})
            group = os.getpgid(status['pid'])
            wait(lambda: request(f"http://127.0.0.1:{profile['port']}/health"), 30)
            results['old_server'] = {'status': status, 'group': group, 'port': profile['port']}
        results['check'] = driver.invoke('check_app_update')
        assert results['check']['status'] == 'available' and results['check']['installKind'] == 'appimage' and results['check']['installSupported'] is True, results['check']
        assert results['check']['assetSha256'] == newhash
        save()
        downloaded = Path(driver.invoke('download_app_update', {'url': url, 'fileName': name, 'size': args.new_app.stat().st_size, 'sha256': newhash}))
        assert digest(downloaded) == newhash
        results['download'] = {'path': str(downloaded), 'sha256': digest(downloaded), 'mode': oct(downloaded.stat().st_mode & 0o777), 'size': downloaded.stat().st_size}
        thread = threading.Thread(target=observe); thread.start()
        results['timestamps']['install_requested'] = time.time()
        result = driver.invoke('install_app_update', {'path': str(downloaded)})
        results['install'] = result
        assert result['status'] == 'restarting', result
        backup = Path(result['backupPath'])
        work = backup.parent
        wait(lambda: not any(p in gui_pids(root, [target, backup]) for p in old), 20)
        results['timestamps']['old_gui_gone_observed'] = time.time()
        save()
        helper_result = wait(lambda: (work / 'result').read_text().strip() if (work / 'result').exists() else None, 100)
        results['helper_result'] = helper_result
        results['timestamps']['helper_result_observed'] = time.time()
        assert helper_result == 'startup-confirmed', helper_result
        results['timestamps']['startup_confirmed_observed'] = time.time()
        assert (work / 'ready').is_file()
        assert digest(backup) == results['inputs']['old']['sha256']
        assert digest(target) == newhash
        new = gui_pids(root, [target, backup]); assert len(new) == 1 and not set(new) & set(old), new
        results['new_gui'] = new
        results['target_sha256'], results['backup_sha256'] = digest(target), digest(backup)
        after = json.loads(cfgpath.read_text()); write_json(root / 'config-after.json', after)
        assert after == before and digest(cfgpath) == config_hash
        assert digest(model) == model_hash
        results['config_preserved'] = True
        results['model_preserved'] = True
        if args.engine:
            results['server_cleanup'] = desktop.release_check(profile['port'], group)
        # A new WebDriver attaches to the exact helper-started process. It does not
        # spawn a second GUI, restart the app, or alter the single-instance handoff.
        try:
            endpoint = inspector_endpoint(next(iter(new)))
            assert endpoint == results['old_inspector_endpoint']
            results['inspector_binding_before_attach'] = inspector_binding(next(iter(new)), endpoint)
            attached = AttachedDriver(root / 'attached', env, target)
            results['new_session'] = attached.start_at(endpoint)
            results['attached_driver_port'] = int(attached.base.rsplit(':', 1)[1])
            # --target does not spawn a browser; WebKit reports processID=0.
            reported_pid = results['new_session']['value']['capabilities']['webkit:processID']
            assert reported_pid in (0, next(iter(new)))
            results['inspector_binding_after_attach'] = inspector_binding(next(iter(new)), endpoint)
            assert results['inspector_binding_after_attach']['socket_inode'] == results['inspector_binding_before_attach']['socket_inode']
            assert gui_pids(root, [target, backup]) == new
            results['versions']['new_native'] = attached.invoke('plugin:app|version')
            assert results['versions']['new_native'] == args.new_version
            assert attached.invoke('load_config') == before
            results['new_check'] = attached.invoke('check_app_update')
            assert results['new_check']['status'] == 'latest' and results['new_check']['installSupported'] is True
            attached.screenshot('new-gui.png')
        except Exception as exc:
            results['new_automation_error'] = str(exc)
            raise RuntimeError('new GUI native-version observation pending; ready alone is insufficient') from exc
        # End handoff sampling before deliberately starting a duplicate executable.
        # A rejected startup briefly executes cookllm but must never create a GUI.
        monitor_stop.set()
        thread.join(timeout=3)
        assert not thread.is_alive()
        results['timestamps']['handoff_sampling_finished'] = time.time()
        handles = attached.call('/window/handles', method='GET')
        assert len(handles) == 1, handles
        results['timestamps']['duplicate_new_requested'] = time.time()
        with (root / 'duplicate-new.log').open('wb') as log:
            duplicate = subprocess.Popen([str(target)], cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
        assert duplicate.wait(timeout=15) == 0
        assert gui_pids(root, [target, backup]) == new
        assert attached.call('/window/handles', method='GET') == handles
        results['duplicate_new'] = {'pid': duplicate.pid, 'exitcode': duplicate.returncode, 'gui_pids_after': list(new), 'window_handles_after': handles}
        results['single_instance_new'] = True
        results['status'] = 'passed'
    except Exception:
        results['status'] = 'failed'; results['error'] = traceback.format_exc()
    finally:
        monitor_stop.set()
        if thread: thread.join(timeout=3)
        write_json(root / 'gui-timeline.json', observations)
        try:
            results['final_target_sha256'] = digest(target)
            if backup and backup.exists(): results['backup_sha256'] = digest(backup)
            if before is not None:
                after = json.loads(cfgpath.read_text())
                write_json(root / 'config-after.json', after)
                results['config_preserved'] = after == before and digest(cfgpath) == config_hash
                results['model_preserved'] = digest(model) == model_hash
                if not results['config_preserved'] or not results['model_preserved']: results['status'] = 'failed'
            if 'old_server' in results:
                state = results['old_server']
                results['server_cleanup'] = desktop.release_check(state['port'], state['group'])
        except Exception as exc:
            results['final_evidence_error'] = str(exc)
            results['status'] = 'failed'
        # Stop private GUI/helper/FUSE before deleting the session; the helper may
        # have daemonized outside the driver's process group.
        images = [target, *target.parent.glob('.cookllm-app-update-*/old.AppImage')]
        results['timestamps']['cleanup_started'] = time.time()
        results['cleanup_before_driver'] = terminate_fixture(root, images)
        if attached: attached.close()
        driver.close()
        proxy.close()
        write_json(root / 'proxy-requests.json', proxy.records)
        results['cleanup'] = {'fixture_pids': processes(root, images), 'proxy_thread_alive': proxy.thread.is_alive()}
        if results['cleanup']['fixture_pids']: results['status'] = 'failed'
        results['gui_nonoverlap'] = bool(observations) and all(len(row['guis']) <= 1 for row in observations)
        if observations and not results['gui_nonoverlap']:
            results['status'] = 'failed'
            results['overlap_error'] = 'overlapping GUI instances observed'
        save()
    return int(results['status'] != 'passed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('old-app', 'new-app', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('old-version', 'new-version'):
        parser.add_argument('--' + name, required=True)
    for name in ('engine', 'model'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    for name in ('old_app', 'new_app', 'engine', 'model'):
        if getattr(args, name):
            value = getattr(args, name).resolve(strict=True)
            if not value.is_file(): parser.error(name + ' must be a file')
            setattr(args, name, value)
    if bool(args.engine) != bool(args.model): parser.error('engine and model must be supplied together')
    if args.old_version == args.new_version: parser.error('distinct versions required')
    root = args.output.resolve()
    allowed = root.is_relative_to(Path('/tmp')) and root != Path('/tmp')
    private = Path(__file__).resolve().parents[1] / '.tmp/linux-validation'
    allowed |= root.parent == private and root.name.startswith('upgrade-')
    if not allowed: parser.error('output must be dedicated /tmp or .tmp/linux-validation/upgrade-*')
    if args.worker:
        if os.environ.get('COOKLLM_UPGRADE_WORKER') != str(root): parser.error('private worker only')
        return worker(args, root)
    os.umask(0o077)
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(root.iterdir()): parser.error('output must be empty')
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(('LD', 'LLAMA_ARG_', 'DBUS_', 'SSL_CERT_', 'COOKLLM_')) or key in ('APPIMAGE', 'APPDIR', 'ARGV0', 'WAYLAND_DISPLAY', 'NIRI_SOCKET', 'NIRI_CONFIG', 'XDG_CONFIG_DIRS', 'XDG_DATA_DIRS') or 'proxy' in key.lower() or 'token' in key.lower():
            env.pop(key)
    env.update(PATH='/usr/local/bin:/usr/bin:/bin', GDK_BACKEND='x11', WEBKIT_DISABLE_DMABUF_RENDERER='1', TAURI_WEBVIEW_AUTOMATION='true', NO_AT_BRIDGE='1', GTK_USE_PORTAL='0', COOKLLM_UPGRADE_WORKER=str(root), PYTHONDONTWRITEBYTECODE='1')
    if 'XAUTHORITY' not in env and (Path.home() / '.Xauthority').is_file(): env['XAUTHORITY'] = str(Path.home() / '.Xauthority')
    for key, name in {'HOME': 'home', 'XDG_CONFIG_HOME': 'config', 'XDG_DATA_HOME': 'data', 'XDG_CACHE_HOME': 'cache', 'XDG_STATE_HOME': 'state', 'XDG_RUNTIME_DIR': 'runtime', 'TMPDIR': 'tmp'}.items():
        folder = root / name; folder.mkdir(mode=0o700); env[key] = str(folder)
    bus = root / 'dbus.conf'
    bus.write_text('<busconfig><type>session</type><listen>unix:abstract=cookllm-upgrade-' + str(os.getpid()) + '</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
    command = ['/usr/bin/dbus-run-session', '--config-file=' + str(bus), '--', '/usr/bin/python3', str(Path(__file__).resolve()), '--worker']
    for name in ('old_app', 'new_app', 'old_version', 'new_version', 'output', 'engine', 'model'):
        if getattr(args, name): command.extend(['--' + name.replace('_', '-'), str(getattr(args, name))])
    write_json(root / 'command.json', {'command': command, 'cwd': str(root), 'timeout': 240, 'versions': [args.old_version, args.new_version]})
    process, code = None, 1
    try:
        with (root / 'controller.log').open('wb') as log:
            process = subprocess.Popen(command, env=env, cwd=root, stdout=log, stderr=log, start_new_session=True)
        code = process.wait(timeout=240)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        code = 1
    finally:
        if process:
            kill_group(process.pid); process.wait(timeout=5)
        images = list((root / 'install').glob('*.AppImage')) + list((root / 'install').glob('.cookllm-app-update-*/old.AppImage'))
        remaining = terminate_fixture(root, images)
        path = root / 'results.json'
        results = json.loads(path.read_text()) if path.exists() else {'status': 'failed', 'error': 'worker ended before evidence'}
        results['outer_cleanup'] = {'controller_code': code, 'fixture_pids': remaining, 'dbus_group_members': desktop.members(process.pid) if process else []}
        if code or remaining: results['status'] = 'failed'
        numbers = {results[key] for key in ('driver_port', 'attached_driver_port', 'proxy_port') if key in results}
        if 'old_server' in results: numbers.add(results['old_server']['port'])
        if 'old_inspector_endpoint' in results: numbers.add(int(results['old_inspector_endpoint'].rsplit(':', 1)[1]))
        results['outer_cleanup']['ports_released'] = {}
        for number in numbers:
            try:
                with socket.socket() as probe:
                    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    probe.bind(('127.0.0.1', number))
                results['outer_cleanup']['ports_released'][str(number)] = True
            except OSError as exc:
                results['outer_cleanup']['ports_released'][str(number)] = str(exc)
                results['status'] = 'failed'
        write_json(path, results)
    print(json.dumps({'status': results['status'], 'results': str(path)}, ensure_ascii=False))
    return int(results['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
