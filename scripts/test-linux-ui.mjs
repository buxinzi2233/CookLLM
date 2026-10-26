// Run: node scripts/test-linux-ui.mjs. Existing TypeScript/React dependencies only.
// All compiled files, native IPC and browser interactions are isolated fixtures.
import assert from "node:assert/strict";
import { appUpdateFixtures } from "./test-linux-app-update.mjs";
import { cudaUiFixtures } from "./test-linux-cuda-ui.mjs";
import { mkdtemp, mkdir, readFile, writeFile, symlink, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const temp = await mkdtemp(join(tmpdir(), "cookllm-ui-tests-"));
const descriptors = Object.fromEntries(["navigator", "window", "localStorage"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
function globalValue(key, value) { Object.defineProperty(globalThis, key, { configurable: true, value }); }
function platform(value) { globalValue("navigator", { platform: value }); }
globalValue("localStorage", { getItem: () => null, setItem() {} });
const calls = [];
const native = { invoke: async (command, args) => { calls.push({ command, args }); return "/fixture/installer"; } };
globalValue("window", { __TAURI_INTERNALS__: native });
const compilerOptions = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX };
let passed = 0;
async function test(name, run) { calls.length = 0; platform("Linux x86_64"); await run(); console.log(`ok ${++passed} - ${name}`); }
const asts = new Map();
let engineHelpers = {};
const normalize = (file) => file.split("\\").join("/");
/** 只把 src 内的 .ts/.tsx（含 index）计入闭包；css 与 node_modules 依赖保持原样。 */
async function resolveModule(from, specifier) {
  const base = normalize(join(dirname(from), specifier));
  const candidates = /\.[cm]?[jt]sx?$/.test(specifier) ? [base] : [`${base}.ts`, `${base}.tsx`, `${base}/index.ts`, `${base}/index.tsx`];
  for (const candidate of candidates) {
    try { if ((await stat(join(root, "src", candidate))).isFile()) return candidate; } catch { /* 非 src 模块 */ }
  }
  return null;
}
/** 按 import 关系递归收集入口模块的真实依赖闭包，重构上游文件后清单不会过期。 */
async function moduleClosure(entries) {
  const closure = new Set();
  const queue = entries.map(normalize);
  while (queue.length) {
    const file = queue.shift();
    if (closure.has(file)) continue;
    closure.add(file);
    const source = await readFile(join(root, "src", file), "utf8");
    for (const match of source.matchAll(/(?:from|import)\s*\(?\s*["'](\.[^"']+)["']/g)) {
      const target = await resolveModule(file, match[1]);
      if (target && !closure.has(target)) queue.push(target);
    }
  }
  return [...closure].sort();
}
async function expression(file, name, deps) {
  // 引擎卡片是 Settings 页里引擎选择/更新状态的唯一实现（v0.4.1 起从 SettingsPage 迁出）。
  if (file === "components/settings/EngineCard.tsx") deps = {
    ...engineHelpers,
    engineSelection: { current: { backend: deps.backend ?? "vulkan", cudaVersion: deps.cudaVersion ?? "auto", generation: 0 } },
    engineOperation: { current: { id: 0, active: name === "handleEngineProgress", mounted: true } }, setCheckResult() {}, ...deps,
  };
  let ast = asts.get(file);
  if (!ast) { ast = ts.createSourceFile(file, await readFile(join(root, "src", file), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX); asts.set(file, ast); }
  let expression;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(ast) === name) expression = node.initializer?.getText(ast);
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) expression = node.getText(ast).replace(/^export\s+(?:default\s+)?/, "");
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.ok(expression, `${file}: ${name} exists`);
  const { outputText } = ts.transpileModule(`const implementation = ${expression};`, { compilerOptions: { ...compilerOptions, jsx: ts.JsxEmit.React } });
  return new Function(...Object.keys(deps), `${outputText}\nreturn implementation;`)(...Object.values(deps));
}
try {
  await symlink(join(root, "node_modules"), join(temp, "node_modules"), process.platform === "win32" ? "junction" : "dir");
  await mkdir(join(temp, "components"));
  const closure = await moduleClosure(["platform.ts", "data.ts", "utils.ts", "tauri.ts", "i18n.ts", "components/AppUpdateDialog.tsx", "components/EngineModal.tsx", "components/Playground.tsx", "components/SettingsPage.tsx", "components/ConfirmModal.tsx"]);
  for (const file of closure) {
    let source = await readFile(join(root, "src", file), "utf8");
    // Desktop app has no SSR entry. Supply only the server snapshot in this
    // temporary test module; translations, component state and handlers stay real.
    if (file === "i18n.ts") source = source.replace("useSyncExternalStore(subscribe, getLocale)", "useSyncExternalStore(subscribe, getLocale, getLocale)");
    const { outputText } = ts.transpileModule(source, { compilerOptions });
    const target = join(temp, file.replace(/\.tsx?$/, ".mjs"));
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, outputText.replace(/(from\s+["'])(\.{1,2}\/[^"']+)(["'])/g, "$1$2.mjs$3"));
  }
  const p = await import(pathToFileURL(join(temp, "platform.mjs")));
  const bridge = await import(pathToFileURL(join(temp, "tauri.mjs")));
  engineHelpers = { engineDownloadInfo: bridge.engineDownloadInfo, engineReleaseUpToDate: bridge.engineReleaseUpToDate };
  const i18n = await import(pathToFileURL(join(temp, "i18n.mjs")));
  const utils = await import(pathToFileURL(join(temp, "utils.mjs")));
  const { default: UpdateDialog } = await import(pathToFileURL(join(temp, "components/AppUpdateDialog.mjs")));
  const { default: ChangelogViewer } = await import(pathToFileURL(join(temp, "components/ChangelogViewer.mjs")));
  const { default: EngineModal } = await import(pathToFileURL(join(temp, "components/EngineModal.mjs")));
  const { default: Playground } = await import(pathToFileURL(join(temp, "components/Playground.mjs")));
  const { default: Settings } = await import(pathToFileURL(join(temp, "components/SettingsPage.mjs")));
  i18n.setLocale("en");
  const t = (key, vars) => i18n.formatMessage("en", key, vars);
  const update = { status: "available", latestTag: "v99", releaseUrl: "https://example.test/releases/v99", assetName: "CookLLM.deb", assetUrl: "https://example.test/CookLLM.deb" };

  await test("platform gates, CPU default and explicit tray preferences", () => {
    for (const platform of ["linux", "macos", "unknown", "windows"]) {
      assert.equal(p.canAutoInstallApp(platform), platform === "windows");
      assert.equal(p.canAutoInstallEngine(platform), platform === "windows");
      assert.equal(p.defaultEngineBackend(platform), platform === "windows" ? "cuda" : "cpu");
      assert.equal(p.minimizeToTrayOnClose(undefined, platform), platform !== "linux");
      assert.equal(p.minimizeToTrayOnClose(true, platform), true);
      assert.equal(p.minimizeToTrayOnClose(false, platform), false);
    }
  });
  await test("Linux inputs preserve spaces, case and literal backslash", () => {
    const value = " /引擎/A\\B ";
    assert.equal(p.engineInputPath(value), value);
    assert.notEqual(p.pathIdentity(value), p.pathIdentity(value.toLowerCase()));
    assert.equal(p.engineInputPath(value, "windows"), value.trim());
    assert.equal(p.enginePathPlaceholder("linux"), "/path/to/llama.cpp/llama-server");
    assert.match(p.enginePathPlaceholder("windows"), /\.exe$/);
  });
  await test("actual package extensions, including unknown and absent", () => {
    for (const [file, type] of [["a.MSI", "MSI"], ["a.exe", "EXE"], ["a.AppImage", "AppImage"], ["a.deb", "DEB"], ["a.rpm", "RPM"], ["a.zip", "—"], [null, "—"]]) assert.equal(p.installerPackageType(file), type);
  });
  await test("Linux native bridge gates every auto-install path before IPC", async () => {
    for (const run of [() => bridge.downloadAppUpdate(update), () => bridge.installAppUpdate("/tmp/a.exe"), () => bridge.downloadLlamaCpp({ backend: "other" }), () => bridge.checkLlamaCppUpdate("other")]) await assert.rejects(run, /manual/i);
    assert.equal(calls.length, 0);
  });
  await test("Windows install bridge retains native commands and payload", async () => {
    platform("Win32");
    await bridge.downloadAppUpdate({ ...update, assetName: "a.msi" });
    await bridge.installAppUpdate("C:\\a.msi");
    await bridge.downloadLlamaCpp({ backend: "cuda", cudaVersion: "12" });
    await bridge.checkLlamaCppUpdate("cpu");
    assert.deepEqual(calls.map((c) => c.command), ["download_app_update", "install_app_update", "download_llamacpp", "check_llamacpp_update"]);
    assert.equal(calls[0].args.fileName, "a.msi");
    assert.equal(calls[2].args.cudaVersion, "12");
  });
  await test("Linux local selection, scan, probe and update check remain available", async () => {
    await bridge.pickServerFile(); await bridge.pickServerDir(); await bridge.getLlamaCppStatus(" /A\\B "); await bridge.checkForUpdate("1");
    assert.deepEqual(calls.map((c) => c.command), ["pick_server_file", "pick_server_dir", "get_llamacpp_status", "check_app_update"]);
    assert.equal(calls[2].args.customPath, " /A\\B ");
  });
  await test("external bridge rejects non-HTTP schemes before native IPC", async () => {
    for (const url of ["javascript:alert(1)", "file:///tmp/a", "data:text/html,test", "mailto:x@y", "/relative", "garbage"]) await assert.rejects(() => bridge.openExternal(url), /HTTP/);
    assert.equal(calls.length, 0);
    await bridge.openExternal("https://example.test");
    assert.equal(calls[0].command, "open_url");
  });
  await test("browser popup and native clipboard failures propagate", async () => {
    globalValue("window", { open: () => null });
    await assert.rejects(() => bridge.openExternal("https://example.test"), /blocked/);
    globalValue("window", { __TAURI_INTERNALS__: { invoke: async () => { throw new Error("clipboard fixture"); } } });
    await assert.rejects(() => bridge.writeClipboard("x"), /clipboard fixture/);
    globalValue("window", { __TAURI_INTERNALS__: native });
  });
  const frame = {}, origin = "http://127.0.0.1:8080", webUiUrl = origin + "/chat";
  const message = (data, extra = {}) => ({ data, source: frame, origin, ...extra });
  await test("bridge requires matching source AND exact origin", async () => {
    let count = 0;
    const actions = { copy: async () => count++, open: async () => count++, toggleZen: () => count++ };
    for (const extra of [{ source: {} }, { source: null }, { origin: "http://127.0.0.1:8081" }, { origin: "https://127.0.0.1:8080" }, { origin: "null" }]) {
      assert.equal(await p.dispatchWebUiMessage(message({ type: "cookllm:copy", text: "x" }, extra), frame, webUiUrl, actions), null);
    }
    assert.equal(p.isTrustedWebUiMessage(message({}), frame, "invalid"), false);
    assert.equal(count, 0);
    assert.equal(await p.dispatchWebUiMessage(message({ type: "cookllm:copy", text: "" }), frame, webUiUrl, actions), "copy");
    assert.equal(await p.dispatchWebUiMessage(message({ type: "cookllm:zen-toggle" }), frame, webUiUrl, actions), "zen");
    assert.equal(await p.dispatchWebUiMessage(message({ type: "cookllm:open", url: "https://example.test" }), frame, webUiUrl, actions), "open");
    assert.equal(count, 3);
  });
  await test("bridge validates message shapes and propagates action failures", async () => {
    let calls = 0;
    const actions = { copy: async () => { throw new Error("copy failed"); }, open: async () => { calls++; throw new Error("open failed"); }, toggleZen() {} };
    for (const data of [null, 1, "copy", {}, { type: "cookllm:copy", text: 42 }, { type: "invoke", command: "install_app_update" }]) assert.equal(await p.dispatchWebUiMessage(message(data), frame, webUiUrl, actions), null);
    await assert.rejects(() => p.dispatchWebUiMessage(message({ type: "cookllm:open", url: "file:///etc/passwd" }), frame, webUiUrl, actions), /http-url-required/);
    assert.equal(calls, 0);
    await assert.rejects(() => p.dispatchWebUiMessage(message({ type: "cookllm:copy", text: "x" }), frame, webUiUrl, actions), /copy failed/);
    await assert.rejects(() => p.dispatchWebUiMessage(message({ type: "cookllm:open", url: "https://example.test" }), frame, webUiUrl, actions), /open failed/);
  });
  await test("actual EngineModal save preserves Linux path and CPU fallback", async () => {
    let saved;
    const save = await expression("components/EngineModal.tsx", "handleSave", { ...p, path: " /引擎/A\\B ", name: "manual", engine: null, backend: "", cudaVersion: "", version: "", detectionStatus: null, engineType: "llamacpp", setAsActive: true, uid: () => "fixture", t, setError: assert.fail, onSave: (engine) => { saved = engine; } });
    save();
    assert.equal(saved.path, " /引擎/A\\B "); assert.equal(saved.backend, "cpu");
    platform("Win32"); save(); assert.equal(saved.path, "/引擎/A\\B"); assert.equal(saved.backend, "cuda");
  });
  await test("actual candidate selection and settings active state preserve Linux case", async () => {
    // v0.4.1：activeEngine 计算已从 SettingsPage 迁到 hooks/useEngines.ts 的 activeAppEngine。
    const engines = [{ id: "upper", path: "/Engine" }, { id: "lower", path: "/engine" }];
    const useEngines = await expression("hooks/useEngines.ts", "useEngines", { ...p, useMemo: (compute) => compute() });
    const derived = useEngines({ config: { engines, serverPath: "/engine" }, persist: assert.fail, setToast: assert.fail, t: assert.fail });
    assert.equal(derived.activeAppEngine.id, "lower");
    assert.equal(derived.appEngines, engines);
    assert.equal(await expression("components/EngineModal.tsx", "isSelected", { ...p, path: "/engine", c: engines[0] }), false);
    platform("Win32");
    assert.equal(await expression("components/EngineModal.tsx", "isSelected", { ...p, path: "/engine", c: engines[0] }), true);
  });
  await test("actual Settings engine and app handlers stop before side effects on unsupported Linux ARM", async () => {
    platform("Linux aarch64");
    const errors = [];
    for (const name of ["startUpdate", "forceReinstall", "runUpdate", "ensureRemote"]) {
      const handler = await expression("components/settings/EngineCard.tsx", name, { ...p, t, backend: "cuda", remote: null, setRemote() {}, setEngineError: (error) => errors.push(error) });
      await handler("cuda");
    }
    for (const name of ["startDownload", "startInstall"]) {
      const handler = await expression("components/AppUpdateDialog.tsx", name, { ...p, t, update, setError: (error) => errors.push(error), setPhase: (phase) => assert.equal(phase, "error") });
      await handler("cuda");
    }
    assert.equal(errors.length, 6);
  });
  await test("actual update UI renders manual Linux action and true package type", () => {
    const html = renderToStaticMarkup(React.createElement(UpdateDialog, { open: true, update, onClose() {} }));
    assert.match(html, /Open release page/); assert.match(html, /DEB/); assert.doesNotMatch(html, /Download &amp; Install/);
    platform("Win32");
    const win = renderToStaticMarkup(React.createElement(UpdateDialog, { open: true, update: { ...update, assetName: "a.exe" }, onClose() {} }));
    assert.match(win, /Download &amp; Install/); assert.match(win, /EXE/); assert.doesNotMatch(win, /Open release page/);
  });
  await test("actual engine UI defaults to CPU and Linux placeholder", () => {
    const html = renderToStaticMarkup(React.createElement(EngineModal, { engine: { id: "fixture", path: "/engine", name: "manual" }, onSave() {}, onClose() {} }));
    assert.match(html, /value="cpu" selected=""/); assert.match(html, /\/path\/to\/llama.cpp\/llama-server/); assert.match(html, /For manual engines/);
  });
  await test("actual Playground starts in loading state, not healthy", () => {
    const html = renderToStaticMarkup(React.createElement(Playground, { visible: true, status: { running: true }, webUiUrl, onOpenWebUi() {} }));
    assert.match(html, /does not mean the model is ready/); assert.match(html, /<iframe/);
  });
  await test("actual Playground message callback exposes clipboard failure", async () => {
    const errors = [];
    const handler = await expression("components/Playground.tsx", "onMessage", { dispatchWebUiMessage: p.dispatchWebUiMessage, status: { running: true }, frameRef: { current: { contentWindow: frame } }, webUiUrl, writeClipboard: async () => { throw new Error("fixture failure"); }, openExternal: async () => {}, onToggleZenMode() {}, setBridgeError: (value) => errors.push(value), t });
    handler(message({ type: "cookllm:copy", text: "test" }));
    await new Promise((resolve) => setImmediate(resolve));
    assert.match(errors[0], /Desktop action failed: fixture failure/);
  });
  await test("new desktop messages exist in both locales", () => {
    for (const key of ["linux.autoEngine", "linux.engineGate", "linux.assetMissing", "linux.configReloadFailed", "linux.installing", "linux.installed", "linux.keepOpen", "linux.manualEngine", "linux.manualUpdate", "linux.trayDesc", "pg.loading", "pg.loaded", "pg.failed", "pg.slow", "pg.bridgeError"]) {
      assert.ok(i18n.formatMessage("zh", key)); assert.ok(i18n.formatMessage("en", key)); assert.notEqual(i18n.formatMessage("zh", key), i18n.formatMessage("en", key));
    }
  });

  await test("engine capabilities depend on platform, backend and detected x64 architecture", async () => {
    for (const env of [{ platform: "Linux x86_64" }, { platform: "Linux", userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }]) assert.equal(p.isLinuxX64(env), true);
    for (const env of [{}, { platform: "Linux" }, { platform: "Linux aarch64" }, { platform: "Linux i686" }, { platform: "Win32", userAgent: "x86_64" }, { platform: "Linux arm64", userAgent: "x86_64" }]) assert.equal(p.isLinuxX64(env), false);
    for (const os of ["windows", "linux", "macos", "unknown"]) for (const arch of [true, false]) for (const backend of ["cpu", "vulkan", "cuda", "other", undefined]) {
      assert.equal(p.canAutoInstallEngine(os, backend, arch), os === "windows" || os === "linux" && arch && ["cpu", "vulkan", "cuda"].includes(backend));
    }
    for (const value of ["Linux aarch64", "Linux", "MacIntel"]) {
      platform(value);
      await assert.rejects(() => bridge.downloadLlamaCpp({ backend: "cpu" }), /manual/);
      await assert.rejects(() => bridge.checkLlamaCppUpdate("vulkan"), /manual/);
    }
    assert.equal(calls.length, 0);
  });
  await test("Linux CPU/Vulkan IPC keeps payload and missing preference defaults to CPU", async () => {
    for (const backend of ["cpu", "vulkan"]) {
      await bridge.checkLlamaCppUpdate(backend);
      await bridge.downloadLlamaCpp({ backend, tag: "b99999" });
    }
    await bridge.checkLlamaCppUpdate();
    assert.deepEqual(calls.map(c => c.command), ["check_llamacpp_update", "download_llamacpp", "check_llamacpp_update", "download_llamacpp", "check_llamacpp_update"]);
    assert.deepEqual(calls[1].args, { backend: "cpu", cudaVersion: null, assetUrl: null, assetName: null, tag: "b99999" });
    assert.equal(calls[2].args.backend, "vulkan"); assert.equal(calls[4].args.backend, "cpu");
  });
  await test("actual hardware refresh preserves explicit user selection across platforms", async () => {
    assert.equal(p.autoEngineBackend("cuda"), "cuda"); assert.equal(p.autoEngineBackend("vulkan"), "vulkan");
    assert.equal(p.autoEngineBackend(undefined), "cpu"); assert.equal(p.autoEngineBackend("cuda", "windows"), "cuda");
    const selected = [], refreshed = [];
    // activeEngine 现在由 App 作为 prop 传入（useEngines 派生）；单独抽取该函数时需显式提供，否则会被源码的空 catch 吞掉。
    const refresh = await expression("components/settings/EngineCard.tsx", "refreshEngine", { ...p, activeEngine: undefined, engineSelection: { current: { userSelected: true } }, config: { llamaBackend: "vulkan" }, detectHardware: async () => ({ recommendedBackend: "cuda" }), getLlamaCppStatus: async () => ({ localBackend: "cuda" }), checkOrphanServer: async () => null, setHardware() {}, setEngineStatus: value => refreshed.push(value), setDetectedServer() {}, setBackend: value => selected.push(value), setCudaVersion() {} });
    await refresh(); assert.deepEqual(selected, []); assert.equal(refreshed.length, 1);
    platform("Win32"); await refresh(); assert.deepEqual(selected, []); assert.equal(refreshed.length, 2);
  });
  const release = { tag: "b99999", matchBackend: "vulkan", assets: [], matchAsset: { backend: "vulkan", fileName: "llama-b99999-bin-ubuntu-vulkan-x64.tar.gz", url: "https://example.test/llama-b99999-bin-ubuntu-vulkan-x64.tar.gz", size: 170, sha256: "a".repeat(64) } };
  await test("actual Settings check uses selected backend and exposes missing assets/rate limits with retry", async () => {
    let result = release, failure = null, remote = null, error, checking;
    const seen = [];
    const ensure = await expression("components/settings/EngineCard.tsx", "ensureRemote", { ...p, t, backend: "vulkan", cudaVersion: "12", engineStatus: { localVersion: "b1", localBackend: "cpu" }, setChecking: v => checking = v, setEngineError: v => error = v, setRemote: v => remote = v, checkLlamaCppUpdate: async backend => { seen.push(backend); if (failure) throw new Error(failure); return result; } });
    assert.equal((await ensure()).tag, "b99999"); assert.equal(checking, false); assert.equal(error, null); assert.equal(remote.upToDate, false);
    result = { ...release, matchAsset: null }; assert.equal(await ensure(), null); assert.match(error, /no trusted asset/); assert.equal(remote, null);
    failure = "HTTP 403 rate limit"; await ensure(undefined, undefined, true); assert.match(error, /403/); assert.equal(checking, false);
    failure = null; result = release; await ensure(); assert.equal(error, null); assert.deepEqual(seen, ["vulkan", "vulkan", "vulkan", "vulkan"]);
  });
  await test("actual Settings update and reinstall dispatch recheck with explicit force semantics", async () => {
    for (const name of ["startUpdate", "forceReinstall"]) {
      const seen = [];
      const handler = await expression("components/settings/EngineCard.tsx", name, { ...p, t, backend: "vulkan", updating: false, remote: null, ensureRemote: async () => release, runUpdate: async (...args) => seen.push(args), setEngineError: assert.fail });
      await handler(); assert.deepEqual(seen, [name === "forceReinstall" ? ["vulkan", true] : ["vulkan"]]);
    }
  });
  async function updateFixture(overrides = {}) {
    const state = { events: [], logs: [], errors: [], persisted: [], progress: [] };
    const saved = { serverPath: "/managed/new/llama-server", llamacppDir: "/managed/new", llamaBackend: "vulkan", llamaCudaVersion: null, activeEngineId: "manual", engines: [{ id: "manual", path: "/managed/new/llama-server", backend: "vulkan", version: "b99999", cudaVersion: null }], models: [{ id: "concurrent-model" }], network: { proxyMode: "direct" } };
    const handler = await expression("components/settings/EngineCard.tsx", "runUpdate", {
      ...p, t, updating: false, cudaVersion: "12", remote: release, ensureRemote: async () => release,
      config: { serverPath: "/old", llamaCudaVersion: "12", engines: [] },
      window: { setTimeout: fn => fn() },
      setUpdating: v => state.events.push(v), setEngineError: v => state.errors.push(v), setProgress: v => state.progress.push(v),
      downloadLlamaCpp: async args => { state.input = args; return saved.serverPath; },
      loadConfig: async () => saved, onPersist: async v => state.persisted.push(v),
      setBackend: v => state.backend = v, setCudaVersion() {}, setEngineStatus: v => state.status = v,
      getLlamaCppStatus: async () => ({ localBackend: "vulkan", localVersion: "b99999" }),
      onLog: (...args) => state.logs.push(args), refreshEngine: assert.fail,
      setRemote: value => state.remote = value, ...overrides,
    });
    await handler("vulkan");
    return { state, saved };
  }
  await test("actual Settings install adopts complete saved config and updates status", async () => {
    const { state, saved } = await updateFixture();
    assert.deepEqual(state.persisted, [saved]); assert.equal(state.persisted[0], saved);
    // 原断言：SettingsPage 的本地 serverPath 状态被写成安装结果。
    // v0.4.1：EngineCard 不再持有 serverPath 状态，安装结果整体交给 onPersist（App.persist → config → 卡片），
    // 所以路径契约改由持久化配置承载：采用安装器返回的路径，而不是安装前的旧路径。
    assert.equal(state.persisted[0].serverPath, saved.serverPath); assert.notEqual(state.persisted[0].serverPath, "/old"); assert.equal(state.backend, "vulkan"); assert.equal(state.status.localVersion, "b99999");
    assert.deepEqual(state.events, [true, false]); assert.equal(state.input.tag, release.tag); assert.equal(state.remote.upToDate, true); assert.equal(state.progress.at(-1), null); assert.deepEqual(state.errors, [null]);
  });
  await test("actual Settings load failure never persists stale config or reports success", async () => {
    for (const loadConfig of [async () => null, async () => { throw new Error("load fixture failed"); }]) {
      const { state } = await updateFixture({ loadConfig });
      assert.deepEqual(state.persisted, []); assert.equal(state.remote, undefined); assert.equal(state.events.at(-1), false); assert.ok(state.errors.at(-1)); assert.equal(state.logs.at(-1)[1], "stderr");
    }
  });
  await test("actual Settings download failure and cancellation leave retry-ready state", async () => {
    for (const message of ["HTTP 429", "更新已取消"]) {
      const { state } = await updateFixture({ downloadLlamaCpp: async () => { throw new Error(message); }, loadConfig: assert.fail });
      assert.deepEqual(state.persisted, []); assert.equal(state.events.at(-1), false); assert.equal(state.progress.at(-1), null); assert.ok(state.errors.at(-1));
    }
  });
  await test("actual Settings Linux UI preserves manual metadata and enables CUDA alongside CPU/Vulkan", async () => {
    const props = { visible: true, config: { serverPath: "/manual/cuda", llamaBackend: "cuda", activeEngineId: "cuda", engines: [{ id: "cuda", name: "Manual CUDA", path: "/manual/cuda", backend: "cuda" }] }, onPersist: async () => {}, onLog() {}, onCheckUpdate: async () => update, onExportBackup: async () => {}, onImportBackup: async () => {} };
    // v0.4.1：SettingsPage 只透传 activeEngine（App 用 useEngines 计算），这里同样用真实 hook 派生后再渲染。
    const useEngines = await expression("hooks/useEngines.ts", "useEngines", { ...p, useMemo: (compute) => compute() });
    const activeEngine = useEngines({ config: props.config, persist: assert.fail, setToast: assert.fail, t }).activeAppEngine;
    const html = renderToStaticMarkup(React.createElement(Settings, { ...props, activeEngine }));
    assert.match(html, /Manual CUDA/); assert.match(html, /CPU\/Vulkan\/CUDA/); assert.match(html, /class="cuda-text-btn"/); assert.doesNotMatch(html, /class="cuda-text-btn" disabled=""/); assert.match(html, /cuda-compound-pill active/);
    assert.match(html, /class="engine-primary-btn compact"><[^]*?Check/);
    platform("Linux aarch64");
    const arm = renderToStaticMarkup(React.createElement(Settings, { ...props, activeEngine }));
    assert.match(arm, /unknown architectures/); assert.match(arm, /class="engine-primary-btn compact" disabled=""/);
  });
  await appUpdateFixtures({ test, p, bridge, i18n, expression, UpdateDialog, ChangelogViewer, update, platform, calls, globalValue, native });
  await cudaUiFixtures({ test, p, bridge, i18n, expression, utils, Settings, platform, calls, globalValue, native });
  console.log(`Passed ${passed} UI/helper fixtures; simulated platform/IPC and SSR, not desktop runtime validation.`);
} finally {
  for (const [key, descriptor] of Object.entries(descriptors)) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  await rm(temp, { recursive: true, force: true });
}
