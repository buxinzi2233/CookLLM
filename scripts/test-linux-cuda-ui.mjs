// Imported by test-linux-ui.mjs. Real helpers, IPC payloads and Settings handlers;
// hooks/native calls are isolated fixtures, with no network or user configuration I/O.
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as icons from "lucide-react";

export async function cudaUiFixtures({ test, p, bridge, i18n, expression, platform, calls, globalValue, native }) {
  const t = (key, vars) => i18n.formatMessage("en", key, vars);
  const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
  const tick = () => new Promise(resolve => setImmediate(resolve));
  const release = (full = "12.8", tag = "b99999", engineSize = 172, runtimeSize = 594) => {
    const fileName = `llama-${tag}-bin-ubuntu-cuda-${full}-x64.tar.gz`;
    const engine = { backend: "cuda", cudaVersion: full.split(".")[0], cudaFullVersion: full, fileName, url: `https://example.test/${fileName}`, size: engineSize, sha256: "a".repeat(64) };
    const runtime = { ...engine, fileName: `cudart-${fileName}`, url: `https://example.test/cudart-${fileName}`, size: runtimeSize };
    return { tag, matchBackend: "cuda", assets: [engine], matchAsset: engine, cudartAssets: [runtime] };
  };
  const selection = (backend = "cuda", cudaVersion = "auto") => ({ current: { backend, cudaVersion, generation: 0, userSelected: false, initialized: false } });
  const operation = () => ({ current: { id: 0, active: false, mounted: true } });
  const local = (full = "12.8") => ({ serverAvailable: true, localVersion: "b99999", localBackend: "cuda", cudaVersion: full });
  async function ensureFixture(overrides = {}) {
    const state = { remote: null, error: null, checking: false, seen: [] };
    const engineSelection = selection(), engineOperation = operation();
    const ensure = await expression("components/SettingsPage.tsx", "ensureRemote", {
      ...p, t, engineSelection, engineOperation, engineStatus: local("12.7"),
      setRemote: value => state.remote = value, setEngineError: value => state.error = value,
      setChecking: value => state.checking = value, setCheckResult() {},
      checkLlamaCppUpdate: async (...args) => { state.seen.push(args); return release(); }, ...overrides,
    });
    return { ensure, state, engineSelection, engineOperation };
  }

  await test("Linux x64 CUDA bridge preserves auto, legacy majors and full versions without runtime override", async () => {
    for (const cudaVersion of ["auto", "12", "13", "12.8", "13.0"]) {
      const asset = release(cudaVersion.includes(".") ? cudaVersion : "12.8").matchAsset;
      await bridge.checkLlamaCppUpdate("cuda", cudaVersion);
      await bridge.downloadLlamaCpp({ backend: "cuda", cudaVersion, tag: "b99999", assetUrl: asset.url, assetName: asset.fileName });
      assert.deepEqual(calls.at(-2).args, { backend: "cuda", cudaVersion });
      assert.deepEqual(calls.at(-1).args, { backend: "cuda", cudaVersion, tag: "b99999", assetUrl: asset.url, assetName: asset.fileName });
    }
    for (const arch of ["Linux i686", "Linux aarch64", "Linux arm64", "Linux", "Unknown"]) {
      platform(arch); const count = calls.length;
      await assert.rejects(() => bridge.checkLlamaCppUpdate("cuda", "auto"), /unknown architectures/);
      await assert.rejects(() => bridge.downloadLlamaCpp({ backend: "cuda", cudaVersion: "12.8" }), /unknown architectures/);
      assert.equal(calls.length, count);
    }
  });
  await test("CUDA capability is not driver evidence and native driver errors reach the caller", async () => {
    assert.equal(p.canAutoInstallEngine("linux", "cuda", true), true);
    globalValue("window", { __TAURI_INTERNALS__: { invoke: async () => { throw new Error("driver CUDA ceiling unknown fixture"); } } });
    try { await assert.rejects(() => bridge.checkLlamaCppUpdate("cuda", "auto"), /driver CUDA ceiling/); }
    finally { globalValue("window", { __TAURI_INTERNALS__: native }); }
  });
  await test("CUDA pair helper sums exact metadata sizes, not fixed sample amounts", () => {
    for (const [engineSize, runtimeSize] of [[172, 594], [301, 809], [1, 2]]) {
      const value = release("12.8", "b99999", engineSize, runtimeSize);
      const info = bridge.engineDownloadInfo(value);
      assert.equal(info.engine, value.matchAsset); assert.equal(info.runtime, value.cudartAssets[0]);
      assert.equal(info.total, engineSize + runtimeSize);
    }
  });
  await test("CUDA pair helper blocks missing, ambiguous, wrong tag/version/arch and incomplete metadata", () => {
    const value = release(), runtime = value.cudartAssets[0];
    const patches = [undefined, [], [runtime, runtime], [{ ...runtime, cudaFullVersion: "12.9" }],
      [{ ...runtime, fileName: runtime.fileName.replace("b99999", "b99998") }],
      [{ ...runtime, fileName: runtime.fileName.replace("x64", "arm64") }],
      [{ ...runtime, cudaVersion: "13" }], [{ ...runtime, sha256: "" }], [{ ...runtime, size: 0 }]];
    for (const cudartAssets of patches) assert.throws(() => bridge.engineDownloadInfo({ ...value, cudartAssets }), /no unique runtime/);
    for (const patch of [{ cudaFullVersion: "12" }, { fileName: "wrong" }, { sha256: null }]) {
      assert.throws(() => bridge.engineDownloadInfo({ ...value, matchAsset: { ...value.matchAsset, ...patch } }), /no unique runtime/);
    }
    const cpu = { ...value, matchAsset: { backend: "cpu", size: 77 }, cudartAssets: [] };
    assert.equal(bridge.engineDownloadInfo(cpu).total, 77);
  });
  await test("same tag different CUDA full version is not up to date; legacy major is not full-version proof", async () => {
    for (const full of ["12.7", "12", "13.0", undefined]) assert.equal(bridge.engineReleaseUpToDate(release(), { ...local(), cudaVersion: full }, "cuda"), false);
    assert.equal(bridge.engineReleaseUpToDate(release(), local(), "cuda"), true);
    assert.equal(bridge.engineReleaseUpToDate(release(), { ...local(), serverAvailable: false }, "cuda"), false);
    assert.equal(bridge.engineReleaseUpToDate(release(), { ...local(), localVersion: "b9999" }, "cuda"), false);
    const fixture = await ensureFixture(); await fixture.ensure(); assert.equal(fixture.state.remote.upToDate, false);
  });
  await test("actual ensureRemote blocks unpaired CUDA and exposes backend driver errors", async () => {
    let result = { ...release(), cudartAssets: [] }, error = null;
    const fixture = await ensureFixture({ checkLlamaCppUpdate: async () => { if (error) throw new Error(error); return result; } });
    assert.equal(await fixture.ensure(), null); assert.match(fixture.state.error, /no unique runtime/);
    assert.equal(fixture.state.checking, false); assert.equal(fixture.state.remote, null);
    error = "driver maximum fixture"; await fixture.ensure(); assert.equal(fixture.state.error, error);
    error = null; result = release(); assert.ok(await fixture.ensure()); assert.equal(fixture.state.error, null);
  });
  await test("late backend/version responses and failures never overwrite current remote or checking state", async () => {
    for (const kind of ["version", "backend", "failure"]) {
      const old = deferred(), fresh = deferred(); let count = 0;
      const fixture = await ensureFixture({ checkLlamaCppUpdate: () => ++count === 1 ? old.promise : fresh.promise });
      const first = fixture.ensure("cuda", "12.8");
      const second = fixture.ensure(kind === "backend" ? "vulkan" : "cuda", "13.0");
      if (kind === "failure") old.reject(new Error("old error")); else old.resolve(release());
      assert.equal(await first, null); assert.equal(fixture.state.checking, true); assert.equal(fixture.state.error, null);
      const next = kind === "backend" ? { ...release(), matchAsset: { backend: "vulkan", size: 1 } } : release("13.0");
      fresh.resolve(next); assert.ok(await second); assert.equal(fixture.state.remote.matchAsset, next.matchAsset);
      assert.equal(fixture.state.checking, false);
    }
  });
  await test("hardware recommendation and stored preference select CUDA only on supported Linux x64", async () => {
    for (const preferred of [undefined, "cuda", "vulkan"]) {
      const engineSelection = selection("cpu"), choices = [], checks = [];
      const refresh = await expression("components/SettingsPage.tsx", "refreshEngine", {
        ...p, config: { llamaBackend: preferred }, engineSelection, engineOperation: operation(),
        detectHardware: async () => ({ recommendedBackend: "cuda" }), getLlamaCppStatus: async () => local(), checkOrphanServer: async () => null,
        setHardware() {}, setEngineStatus() {}, setDetectedServer() {}, setBackend: v => choices.push(v), setCudaVersion() {},
        ensureRemote: async (...args) => checks.push(args),
      });
      await refresh(); assert.deepEqual(choices, [preferred || "cuda"]); assert.equal(checks[0][0], preferred || "cuda");
      await refresh(); assert.equal(choices.length, 1);
    }
    platform("Linux aarch64");
    const choices = [];
    const refresh = await expression("components/SettingsPage.tsx", "refreshEngine", { ...p, config: {}, engineSelection: selection("cpu"), engineOperation: operation(), detectHardware: async () => ({ recommendedBackend: "cuda" }), getLlamaCppStatus: async () => local(), checkOrphanServer: async () => null, setHardware() {}, setEngineStatus() {}, setDetectedServer() {}, setBackend: v => choices.push(v), setCudaVersion() {}, ensureRemote: async () => null });
    await refresh(); assert.deepEqual(choices, ["cpu"]);
  });

  async function updateFixture(overrides = {}) {
    const state = { inputs: [], persisted: [], progress: [], errors: [], logs: [], timers: [] };
    const saved = { serverPath: "/fixture/managed/new/llama-server", llamacppDir: "/fixture/managed/new", llamaBackend: "cuda", llamaCudaVersion: "auto", activeEngineId: "old", engines: [{ id: "old", path: "/fixture/managed/new/llama-server", backend: "cuda", cudaVersion: "12.8", version: "b99999", name: "retained name" }], models: [{ id: "new-model" }], profiles: [{ id: "retained-profile" }], network: { proxyMode: "direct" } };
    const engineSelection = selection(), engineOperation = operation();
    const handler = await expression("components/SettingsPage.tsx", "runUpdate", {
      ...p, t, engineSelection, engineOperation, updating: false, cudaVersion: "auto", remote: release("13.0", "b99998"), config: { serverPath: "/fixture/old" },
      ensureRemote: async (...args) => { state.check = args; return release(); },
      engineDownloadInfo: bridge.engineDownloadInfo, setUpdating: value => state.updating = value, setEngineError: value => state.errors.push(value), setProgress: value => state.progress.push(value),
      downloadLlamaCpp: async input => { state.inputs.push(input); return saved.serverPath; }, loadConfig: async () => saved,
      onPersist: async value => state.persisted.push(value), setServerPath: value => state.path = value, setBackend: value => state.backend = value, setEngineStatus() {},
      getLlamaCppStatus: async () => local(), setRemote: value => state.remote = value, onLog: (...args) => state.logs.push(args), refreshEngine: assert.fail,
      window: { setTimeout: fn => state.timers.push(fn) }, ...overrides,
    });
    return { handler, state, saved, engineSelection, engineOperation };
  }
  await test("actual CUDA install downloads fresh exact asset and adopts full backend-saved config", async () => {
    const fixture = await updateFixture(); await fixture.handler("cuda");
    const { state, saved } = fixture, value = release();
    assert.deepEqual(state.check, ["cuda", "auto"]);
    assert.deepEqual(state.inputs, [{ backend: "cuda", cudaVersion: "auto", tag: value.tag, assetUrl: value.matchAsset.url, assetName: value.matchAsset.fileName }]);
    assert.equal(state.progress[0].total, 766); assert.equal(state.persisted[0], saved);
    assert.equal(state.path, saved.serverPath); assert.equal(state.backend, "cuda"); assert.equal(state.remote.matchAsset.cudaFullVersion, "12.8");
    assert.equal(state.remote.upToDate, true); assert.equal(state.updating, false);
  });
  await test("Linux CUDA install sends original preference while fresh asset fields lock the installed version", async () => {
    for (const preference of ["auto", "12", "13", "12.8"]) {
      const full = preference === "13" ? "13.4" : "12.8", fresh = release(full);
      const fixture = await updateFixture({ engineSelection: selection("cuda", preference), cudaVersion: preference,
        ensureRemote: async (...args) => { fixture.state.check = args; return fresh; }, getLlamaCppStatus: async () => local(full) });
      fixture.saved.llamaCudaVersion = preference; fixture.saved.engines[0].cudaVersion = full;
      await fixture.handler("cuda");
      assert.deepEqual(fixture.state.check, ["cuda", preference]);
      assert.deepEqual(fixture.state.inputs, [{ backend: "cuda", cudaVersion: preference, tag: fresh.tag, assetUrl: fresh.matchAsset.url, assetName: fresh.matchAsset.fileName }]);
      const saved = fixture.state.persisted[0];
      assert.equal(saved.llamaCudaVersion, preference); assert.equal(saved.engines[0].cudaVersion, full);
      const checks = [], nextFull = preference === "12.8" ? "12.8" : preference === "13" ? "13.5" : "12.9";
      const restarted = await ensureFixture({ engineSelection: selection("cuda", saved.llamaCudaVersion), engineStatus: local(full),
        checkLlamaCppUpdate: async (...args) => { checks.push(args); return release(nextFull, "b100000"); } });
      const next = await restarted.ensure();
      assert.deepEqual(checks, [["cuda", preference]]); assert.equal(next.matchAsset.cudaFullVersion, nextFull);
    }
  });
  await test("Linux CPU and Vulkan installs adopt config without erasing stored CUDA preference", async () => {
    for (const backend of ["cpu", "vulkan"]) {
      const fresh = { tag: "b100000", matchBackend: backend, assets: [], matchAsset: { backend, size: 42, url: `https://example.test/${backend}`, fileName: `${backend}.tar.gz` }, cudartAssets: [] };
      const fixture = await updateFixture({ engineSelection: selection(backend, "13"), ensureRemote: async () => fresh });
      fixture.saved.llamaBackend = backend; fixture.saved.llamaCudaVersion = "13";
      fixture.saved.engines[0].backend = backend; delete fixture.saved.engines[0].cudaVersion;
      await fixture.handler(backend);
      assert.equal(fixture.state.inputs[0].cudaVersion, undefined);
      assert.equal(fixture.state.persisted[0].llamaCudaVersion, "13");
      assert.equal(fixture.state.persisted[0].engines[0].cudaVersion, undefined);
    }
  });
  await test("Windows Settings preserves backend-owned cudart selection and legacy CUDA preference", async () => {
    platform("Win32");
    const engineSelection = selection("cuda", "12");
    const fixture = await updateFixture({
      engineSelection, cudaVersion: "12", refreshEngine: async () => {},
      getLlamaCppStatus: async () => local(), loadConfig: assert.fail,
      config: { serverPath: "C:\\old\\llama-server.exe", activeEngineId: "old", engines: [{ id: "old", name: "keep", path: "C:\\old\\llama-server.exe" }] },
    });
    await fixture.handler("cuda");
    assert.deepEqual(fixture.state.inputs, [{ backend: "cuda", cudaVersion: "12", tag: "b99999" }]);
    assert.equal(fixture.state.persisted[0].llamaCudaVersion, "12");
    assert.equal(fixture.state.persisted[0].engines[0].name, "keep");
    assert.equal(fixture.state.persisted[0].engines[0].cudaVersion, "12.8");
  });
  await test("fresh up-to-date install skips download, but force reinstall still uses precise current asset", async () => {
    const fixture = await updateFixture({ ensureRemote: async () => ({ ...release(), upToDate: true }) });
    await fixture.handler("cuda"); assert.equal(fixture.state.inputs.length, 0);
    await fixture.handler("cuda", true); assert.equal(fixture.state.inputs.length, 1); assert.equal(fixture.state.inputs[0].cudaVersion, "auto");
    assert.equal(fixture.state.inputs[0].assetName, release().matchAsset.fileName);
  });
  await test("missing runtime and cancellation never persist stale configuration and leave retry-ready state", async () => {
    const missing = await updateFixture({ ensureRemote: async () => ({ ...release(), cudartAssets: [] }) });
    await missing.handler("cuda"); assert.equal(missing.state.inputs.length, 0); assert.equal(missing.state.persisted.length, 0); assert.match(missing.state.errors.at(-1), /no unique runtime/);
    const cancelled = await updateFixture({ downloadLlamaCpp: async () => { throw new Error("cancelled fixture"); }, loadConfig: assert.fail });
    await cancelled.handler("cuda"); assert.equal(cancelled.state.persisted.length, 0); assert.equal(cancelled.state.updating, false); assert.match(cancelled.state.errors.at(-1), /cancel/i);
  });
  await test("late CUDA install callbacks and previous progress timers do not mutate another operation", async () => {
    const download = deferred();
    const fixture = await updateFixture({ downloadLlamaCpp: () => download.promise, loadConfig: assert.fail });
    const running = fixture.handler("cuda"); await tick();
    fixture.engineOperation.current.mounted = false; fixture.engineOperation.current.id++;
    download.resolve("/fixture/late"); await running; assert.equal(fixture.state.persisted.length, 0); assert.equal(fixture.state.remote, undefined);
    const timer = await updateFixture(); await timer.handler("cuda");
    timer.engineOperation.current.id++; timer.engineOperation.current.active = true;
    const count = timer.state.progress.length; timer.state.timers[0](); assert.equal(timer.state.progress.length, count);
  });

  async function settingsSession(config = {}, overrides = {}) {
    const slots = [], effects = [], pending = [], saved = [], checks = [], timers = [];
    let cursor = 0, tree, component, progressHandler;
    const props = { visible: true, config: { serverPath: "", ...config }, appUpdate: null, checkingUpdate: false, onPersist: async value => saved.push(value), onLog() {}, onCheckUpdate: async () => null, onExportBackup: async () => {}, onImportBackup: async () => {} };
    const deps = {
      ...p, ...bridge, ...Object.fromEntries("Activity AlertTriangle ArrowRight Check ChevronDown ChevronRight Cpu Download Eye EyeOff FolderOpen Github KeyRound Languages Loader2 Moon Pencil Plus RefreshCw RotateCw Search SlidersHorizontal Sparkles Star Sun Trash2 Upload Wifi Wrench X Zap".split(" ").map(name => [name, icons[name]])), React, APP_REPO: "fixture/repo", PROJECT_URL: "https://example.test", EngineModal: () => null, ConfirmModal: () => null,
      useI18n: () => ({ t }), cn: (...values) => values.filter(Boolean).join(" "), formatBytes: value => `${value} B`, formatMB: String, formatEngineBackend: value => value || "CPU",
      useState(initial) { const index = cursor++; if (!(index in slots)) slots[index] = initial; return [slots[index], value => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }]; },
      useRef(initial) { const index = cursor++; return slots[index] ??= { current: initial }; },
      useEffect(fn, dependencies) { const index = cursor++; const old = effects[index]; if (!old || dependencies.some((value, i) => value !== old.dependencies[i])) pending.push(() => { old?.cleanup?.(); effects[index] = { dependencies, cleanup: fn() }; }); },
      getGpuInfo: async () => null, getModelsDir: async () => null, getAppVersion: async () => "fixture", getSystemProxy: async () => null,
      detectHardware: async () => ({ recommendedBackend: "cpu" }), getLlamaCppStatus: async () => null, checkOrphanServer: async () => null,
      checkLlamaCppUpdate: async (backend, cuda) => { checks.push([backend, cuda]); return backend === "cuda" ? release(cuda.includes(".") ? cuda : "12.8") : { tag: "b99999", assets: [], matchBackend: backend, matchAsset: { backend, size: 42 } }; },
      onDownloadProgress: async fn => { progressHandler = fn; return () => {}; },
      window: { setTimeout: fn => timers.push(fn), clearTimeout() {}, addEventListener() {}, removeEventListener() {} }, ...overrides,
    };
    component = await expression("components/SettingsPage.tsx", "SettingsPage", deps);
    const render = () => { cursor = 0; tree = component(props); pending.splice(0).forEach(fn => fn()); return tree; };
    const buttons = node => !node || typeof node !== "object" ? [] : [...(node.type === "button" ? [node] : []), ...React.Children.toArray(node.props?.children).flatMap(buttons)];
    const click = predicate => { const button = buttons(render()).find(predicate); assert.ok(button, "Settings button found"); assert.ok(!button.props.disabled, "Settings button enabled"); button.props.onClick({ stopPropagation() {} }); render(); };
    render(); await tick(); render(); await tick(); render();
    return { checks, saved, render, html: () => renderToStaticMarkup(render()), clickClass: className => click(node => node.props.className?.split(" ").includes(className)), clickText: label => click(node => renderToStaticMarkup(node).includes(label)), updateConfig: value => { props.config = value; render(); }, progress: value => progressHandler(value), unmount: () => effects.forEach(effect => effect?.cleanup?.()) };
  }
  await test("real Settings CPU metadata rechecks on CUDA click and shows exact versions, runtime total and instructions", async () => {
    const session = await settingsSession(); assert.equal(session.checks.at(-1)[0], "cpu");
    session.clickClass("cuda-dropdown-trigger"); await tick();
    assert.equal(session.checks.at(-1)[0], "cuda");
    const html = session.html(); assert.match(html, /CUDA 12\.8/); assert.match(html, /Auto \(driver-compatible\)/);
    assert.match(html, /Engine 172 B \+ paired runtime 594 B · Total download 766 B/); assert.match(html, /private cuda-runtime/); assert.match(html, /validation covers 12\.8/);
    session.clickText("CUDA 12.8"); await tick(); assert.deepEqual(session.checks.at(-1), ["cuda", "12.8"]);
    session.clickClass("cuda-dropdown-trigger"); await tick();
    assert.match(session.html(), /cuda-dropdown-item selected[^]*?CUDA 12\.8/);
    session.unmount();
  });
  await test("real Settings switching during pending metadata leaves newest version visible after old response", async () => {
    const old = deferred(), fresh = deferred(); let number = 0;
    const session = await settingsSession({}, { checkLlamaCppUpdate: async backend => backend === "cuda" ? (++number === 1 ? old.promise : fresh.promise) : { tag: "b1", assets: [], matchAsset: { backend, size: 1 } } });
    session.clickClass("cuda-dropdown-trigger"); session.clickText("CUDA 13");
    fresh.resolve(release("13.0", "b99999")); await tick(); assert.match(session.html(), /Total download 766 B/);
    old.resolve(release("12.8", "b88888")); await tick(); assert.match(session.html(), /b99999/); assert.doesNotMatch(session.html(), /b88888/);
    session.unmount();
  });
  await test("real Settings delayed hardware refresh and old persisted preference do not replace user selection", async () => {
    const hardware = deferred();
    const session = await settingsSession({ llamaBackend: "cpu" }, { detectHardware: () => hardware.promise });
    session.clickClass("cuda-text-btn"); await tick(); hardware.resolve({ recommendedBackend: "vulkan" }); await tick();
    session.updateConfig({ serverPath: "", llamaBackend: "vulkan", llamaCudaVersion: "13" });
    const html = session.html(); assert.match(html, /cuda-compound-pill active/); assert.doesNotMatch(html, /mini-seg-btn active/);
    session.unmount();
  });
  await test("Settings engine progress ignores app source, idle events and callbacks after cleanup", async () => {
    const seen = [], engineOperation = operation(); engineOperation.current.active = true;
    const handler = await expression("components/SettingsPage.tsx", "handleEngineProgress", { ...p, t, active: true, engineOperation, setProgress: value => seen.push(value), onLog() {} });
    handler({ source: "app", phase: "download" }); assert.equal(seen.length, 0);
    handler({ source: "engine", phase: "download" }); assert.equal(seen.length, 1);
    engineOperation.current.active = false; handler({ source: "engine", phase: "done" }); assert.equal(seen.length, 1);
    const session = await settingsSession(); session.unmount(); session.progress({ source: "engine", phase: "done" });
  });
  await test("CUDA help is symmetric, distinguishes 13 metadata from 12.8 validation and leaves NInfer manual", () => {
    for (const key of ["linux.cudaPairMissing", "linux.cudaDownloadSize", "linux.cudaRuntimeHint", "linux.cudaCompatibility", "llama.cudaAuto"]) {
      const zh = i18n.formatMessage("zh", key), en = i18n.formatMessage("en", key);
      assert.notEqual(zh, key); assert.notEqual(en, key); assert.notEqual(zh, en);
    }
    for (const locale of ["zh", "en"]) {
      const compatibility = i18n.formatMessage(locale, "linux.cudaCompatibility"); assert.match(compatibility, /13/); assert.match(compatibility, /12\.8/);
      assert.match(i18n.formatMessage(locale, "linux.manualEngine"), /NInfer/);
      assert.doesNotMatch(i18n.formatMessage(locale, "linux.autoEngine"), /CUDA requires manual|CUDA 请手动/);
    }
  });
}
