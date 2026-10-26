// Run: node scripts/test-linux-paths.mjs (uses the existing TypeScript dependency).
// Compiled modules and all fixtures live in an OS temporary directory; no user config/model I/O.
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, symlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const temp = await mkdtemp(join(tmpdir(), "cookllm-path-tests-"));
const storageDescriptor = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: { getItem: () => null } });
const compilerOptions = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext };
let passed = 0;
async function test(name, run) {
  await run();
  passed += 1;
  console.log(`ok ${passed} - ${name}`);
}
function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

try {
  await symlink(join(root, "node_modules"), join(temp, "node_modules"), process.platform === "win32" ? "junction" : "dir");
  for (const name of ["platform", "data", "utils", "tauri", "i18n"]) {
    const source = await readFile(join(root, "src", `${name}.ts`), "utf8");
    const { outputText } = ts.transpileModule(source, { compilerOptions });
    const esm = outputText.replace(/(from\s+["'])(\.\/[^"']+)(["'])/g, "$1$2.mjs$3");
    await writeFile(join(temp, `${name}.mjs`), esm);
  }
  const { getPlatform, pathIdentity } = await import(pathToFileURL(join(temp, "platform.mjs")));
  const { EMPTY_CONFIG, DEFAULT_PROFILES, defaultProfilesForModel, mergeImportedConfig, migrateConfig, removeModelsAtPaths } = await import(pathToFileURL(join(temp, "data.mjs")));
  const { fileName, removeDownloadFiles } = await import(pathToFileURL(join(temp, "utils.mjs")));
  const model = (id, path, profiles = [DEFAULT_PROFILES[0]]) => ({ id, path, name: id, profiles, accent: "violet" });
  const config = (models = [], extra = {}) => ({ ...EMPTY_CONFIG, models, ...extra });
  const upper = "/模型 空格/Model.gguf";
  const lower = "/模型 空格/model.gguf";

  await test("platform hints are layered and unknown defaults to case-sensitive", () => {
    assert.equal(getPlatform({ userAgentData: { platform: "Windows" }, platform: "Linux x86_64" }), "windows");
    assert.equal(getPlatform({ userAgentData: { platform: "" }, platform: "Linux x86_64" }), "linux");
    assert.equal(getPlatform({ platform: "Win32" }), "windows");
    assert.equal(getPlatform({ platform: "MacIntel" }), "macos");
    assert.equal(getPlatform({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }), "windows");
    assert.equal(getPlatform({}), "unknown");
    assert.notEqual(pathIdentity(upper, "unknown"), pathIdentity(lower, "unknown"));
  });
  await test("Linux case, Chinese, spaces, colon and literal backslash identity", () => {
    assert.notEqual(pathIdentity(upper, "linux"), pathIdentity(lower, "linux"));
    assert.equal(pathIdentity(" /中文/A:B\\C.gguf ", "linux"), " /中文/A:B\\C.gguf ");
    assert.notEqual(pathIdentity("/m/A\\B.gguf", "linux"), pathIdentity("/m/A/B.gguf", "linux"));
    assert.notEqual(pathIdentity(upper + " ", "linux"), pathIdentity(upper, "linux"));
    assert.equal(fileName("/m/A\\B.gguf", "linux"), "A\\B.gguf");
  });
  await test("Windows identity remains case-insensitive without altering stored paths", () => {
    assert.equal(pathIdentity("C:\\模型 空格\\Model.gguf", "windows"), pathIdentity("c:\\模型 空格\\model.gguf", "windows"));
    assert.equal(fileName("C:\\模型 空格\\Model.gguf", "windows"), "Model.gguf");
    assert.equal(defaultProfilesForModel("/models/Test.NINFER")[0].kvDtype, "rk8v4");
  });
  await test("real config merge keeps Linux case variants and merges Windows profiles", () => {
    const current = config([model("local", upper)]);
    const incoming = config([model("backup", lower, [DEFAULT_PROFILES[1]])]);
    const linux = mergeImportedConfig(current, incoming, new Set(), "linux");
    assert.equal(linux.added, 1);
    assert.deepEqual(linux.config.models.map((m) => m.path), [upper, lower]);
    const windows = mergeImportedConfig(current, incoming, new Set(), "windows");
    assert.equal(windows.added, 0);
    assert.equal(windows.updated, 1);
    assert.equal(windows.config.models[0].id, "local");
    assert.equal(windows.config.models[0].path, upper);
    assert.equal(windows.config.models[0].profiles.length, 2);
    assert.equal(current.models[0].profiles.length, 1);
  });
  await test("real merge deduplicates repeated incoming paths and accumulates profiles", () => {
    const incoming = config([model("a", upper), model("b", upper, [DEFAULT_PROFILES[1]]), model("c", upper, [DEFAULT_PROFILES[2]])]);
    const merged = mergeImportedConfig(config(), incoming, new Set(), "linux");
    assert.equal(merged.added, 1);
    assert.equal(merged.config.models.length, 1);
    assert.equal(merged.config.models[0].profiles.length, 3);
  });
  await test("missing-path sets use the same identity and never activate a missing imported engine", () => {
    const engine = { id: "missing-engine", name: "engine", path: "/Engine" };
    const incoming = config([model("upper", upper), model("lower", lower)], { serverPath: engine.path, engines: [engine], activeEngineId: engine.id });
    const missing = new Set([upper, engine.path]);
    const linux = mergeImportedConfig(config(), incoming, missing, "linux");
    assert.deepEqual(linux.config.models.map((m) => m.path), [lower]);
    assert.equal(linux.config.serverPath, "");
    assert.equal(linux.config.activeEngineId, undefined);
    assert.deepEqual(linux.config.engines, []);
    assert.equal(mergeImportedConfig(config(), incoming, missing, "windows").config.models.length, 0);
  });
  await test("empty desktop defaults and legacy user paths survive migration", () => {
    assert.equal(migrateConfig(structuredClone(EMPTY_CONFIG)).serverPath, "");
    assert.deepEqual(migrateConfig(structuredClone(EMPTY_CONFIG)).engines, []);
    for (const path of ["C:\\old\\llama-server.exe", "/引擎 空格/llama-server "]) {
      const migrated = migrateConfig(config([], { serverPath: path }));
      assert.equal(migrated.serverPath, path);
      assert.equal(migrated.engines[0].path, path);
    }
    const spaced = upper + " ";
    const merged = mergeImportedConfig(config([model("a", upper)]), config([model("b", spaced)]), new Set(), "linux");
    assert.deepEqual(merged.config.models.map((m) => m.path), [upper, spaced]);
  });
  await test("real model record removal respects Linux and Windows identities", () => {
    const models = [model("a", upper), model("b", lower), model("c", upper + " ")];
    assert.deepEqual(removeModelsAtPaths(models, [upper], "linux").map((m) => m.id), ["b", "c"]);
    assert.deepEqual(removeModelsAtPaths(models, [upper], "windows").map((m) => m.id), ["c"]);
    assert.equal(models.length, 3);
  });
  await test("file deletion helper awaits results and reports individual failures", async () => {
    const gate = deferred();
    let settled = false;
    const result = removeDownloadFiles(upper, async (path) => {
      if (path === upper) await gate.promise;
      else throw new Error("part failure");
    }).then((value) => { settled = true; return value; });
    await Promise.resolve();
    assert.equal(settled, false);
    gate.resolve();
    const outcome = await result;
    assert.deepEqual(outcome.removedPaths, [upper]);
    assert.match(outcome.errors[0], /part failure/);
    const failed = await removeDownloadFiles(upper, async (path) => { if (path === upper) throw new Error("denied"); });
    assert.deepEqual(failed.removedPaths, []);
    assert.match(failed.errors[0], /denied/);
  });

  // Execute the actual App closure (AST extraction, no copied algorithm or React renderer).
  // Only UI state and native I/O are injected; data/path/deletion helpers above are real modules.
  const appText = await readFile(join(root, "src/App.tsx"), "utf8");
  const appAst = ts.createSourceFile("App.tsx", appText, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  function appFunction(name, deps) {
    let initializer;
    function visit(node) {
      if (ts.isVariableDeclaration(node) && node.name.getText(appAst) === name) initializer = node.initializer;
      ts.forEachChild(node, visit);
    }
    visit(appAst);
    assert.ok(initializer, `${name} must exist in App.tsx`);
    const { outputText } = ts.transpileModule(`const implementation = ${initializer.getText(appAst)};`, { compilerOptions });
    return new Function(...Object.keys(deps), `${outputText}\nreturn implementation;`)(...Object.values(deps));
  }
  function deletionHarness({ platform = "linux", tasks = [], models = [], remove = async () => {}, cancel = async () => {}, runs = new Map() } = {}) {
    const downloadsRef = { current: tasks };
    const configRef = { current: config(models) };
    const saved = [], errors = [], toasts = [], calls = [];
    const deletingTaskIdsRef = { current: new Set() };
    const deps = {
      downloadsRef, configRef, deletingTaskIdsRef, downloadRunsRef: { current: runs },
      cancelPendingProgress() {}, clearProgressKey() {}, hfCancelDownload: cancel,
      pathIdentity: (path) => pathIdentity(path, platform),
      getTaskFilePath: (task) => task.path || "/fallback/model.gguf",
      removeDownloadFiles: (path) => removeDownloadFiles(path, async (file) => { calls.push(file); await remove(file); }),
      removeModelsAtPaths: (items, paths) => removeModelsAtPaths(items, paths, platform),
      mutateDownloads: (updater) => { downloadsRef.current = updater(downloadsRef.current); },
      patchByTaskId: (id, patch) => { downloadsRef.current = downloadsRef.current.map((task) => task.taskId === id ? { ...task, ...patch } : task); },
      persist: async (next) => { configRef.current = next; saved.push(next); },
      getModelsDir: async () => null, setDiskUsage() {},
      t: (key, vars) => vars?.error || key,
      appendLog: (message) => errors.push(message), setToast: (message) => toasts.push(message),
    };
    return { run: appFunction("deleteTasksImpl", deps), downloadsRef, configRef, saved, errors, toasts, calls, deletingTaskIdsRef };
  }
  const task = (id, path, status = "done") => ({ taskId: id, path, file: id, repo: "fixture", status });
  await test("actual App deletion waits and uses the latest config without deleting case variants", async () => {
    const gate = deferred();
    const h = deletionHarness({ tasks: [task("a", upper)], models: [model("a", upper), model("b", lower)], remove: () => gate.promise });
    const pending = h.run(["a"], true);
    assert.equal(h.saved.length, 0);
    assert.equal(h.downloadsRef.current.length, 1);
    h.configRef.current = { ...h.configRef.current, theme: "light" };
    gate.resolve();
    await pending;
    assert.deepEqual(h.configRef.current.models.map((m) => m.id), ["b"]);
    assert.equal(h.configRef.current.theme, "light");
    assert.equal(h.downloadsRef.current.length, 0);
    assert.equal(h.deletingTaskIdsRef.current.size, 0);
  });
  await test("actual App deletion retains failed file/model records and exposes errors", async () => {
    const h = deletionHarness({ tasks: [task("a", upper), task("b", lower)], models: [model("a", upper), model("b", lower)], remove: async (path) => { if (path === upper) throw new Error("denied fixture"); } });
    await h.run(["a", "b"], true);
    assert.deepEqual(h.configRef.current.models.map((m) => m.id), ["a"]);
    assert.deepEqual(h.downloadsRef.current.map((t) => t.taskId), ["a"]);
    assert.match(h.errors[0], /denied fixture/);
    assert.ok(!h.toasts.includes("toast.tasksAndFilesDeleted"));
  });
  await test("actual App partial cache failure only removes the successfully deleted main record", async () => {
    const h = deletionHarness({ tasks: [task("a", upper)], models: [model("a", upper), model("b", lower)], remove: async (path) => { if (path.endsWith(".part")) throw new Error("cache failure"); } });
    await h.run(["a"], true);
    assert.deepEqual(h.configRef.current.models.map((m) => m.id), ["b"]);
    assert.equal(h.downloadsRef.current.length, 1);
    assert.match(h.errors[0], /cache failure/);
  });
  await test("actual App active deletion waits for download completion and uses its returned path", async () => {
    const gate = deferred();
    const h = deletionHarness({ tasks: [task("a", undefined, "active")], models: [model("a", upper)], runs: new Map([["a", gate.promise]]) });
    const pending = h.run(["a"], true);
    await Promise.resolve();
    assert.equal(h.calls.length, 0);
    gate.resolve(upper);
    await pending;
    assert.deepEqual(h.calls, [upper, upper + ".part"]);
    assert.equal(h.configRef.current.models.length, 0);
  });
  await test("actual App cancel failure performs no file or record deletion", async () => {
    const h = deletionHarness({ tasks: [task("a", upper, "active")], models: [model("a", upper)], cancel: async () => { throw new Error("cancel failed"); } });
    await h.run(["a"], true);
    assert.equal(h.calls.length, 0);
    assert.equal(h.configRef.current.models.length, 1);
    assert.equal(h.downloadsRef.current.length, 1);
    assert.match(h.errors[0], /cancel failed/);
  });
  await test("actual App record-only deletion keeps library files, including a prior failed task", async () => {
    const h = deletionHarness({ tasks: [task("a", upper, "error")], models: [model("a", upper)] });
    await h.run(["a"], false);
    assert.equal(h.calls.length, 0);
    assert.equal(h.configRef.current.models.length, 1);
    assert.equal(h.downloadsRef.current.length, 0);
  });
  await test("actual App Windows comparison removes matching-case aliases only", async () => {
    const h = deletionHarness({ platform: "windows", tasks: [task("a", upper)], models: [model("a", upper), model("b", lower), model("c", "/other.gguf")] });
    await h.run(["a"], true);
    assert.deepEqual(h.configRef.current.models.map((m) => m.id), ["c"]);
  });
  await test("batch cancellation starts together and failed deletion retains the completed path", async () => {
    const first = deferred(), second = deferred();
    const cancelled = [];
    const h = deletionHarness({
      tasks: [task("a", undefined, "active"), task("b", undefined, "active")],
      models: [model("a", upper), model("b", lower)],
      runs: new Map([["a", first.promise], ["b", second.promise]]),
      cancel: async (id) => { cancelled.push(id); },
      remove: async (path) => { if (path === lower) throw new Error("retry path fixture"); },
    });
    const pending = h.run(["a", "b"], true);
    assert.deepEqual(cancelled, ["a", "b"]);
    second.resolve(lower);
    first.resolve(upper);
    await pending;
    assert.equal(h.downloadsRef.current[0].path, lower);
    assert.deepEqual(h.configRef.current.models.map((m) => m.id), ["b"]);
  });
  await test("actual App late download completion during deletion skips model import", async () => {
    const gate = deferred();
    const deletingTaskIdsRef = { current: new Set() };
    const downloadRunsRef = { current: new Map() };
    let imported = 0;
    const launch = appFunction("launchDownload", {
      deletingTaskIdsRef, downloadRunsRef,
      pauseRequestedRef: { current: new Set() }, cancelledTaskIdsRef: { current: new Set() },
      hfClearDownload: async () => {}, hfDownload: () => gate.promise,
      importDownloadedModel: async () => { imported += 1; }, appendLog() {}, t: (key) => key,
    });
    launch(task("a", upper, "active"));
    await Promise.resolve();
    const run = downloadRunsRef.current.get("a");
    deletingTaskIdsRef.current.add("a");
    gate.resolve({ path: upper });
    assert.equal(await run, upper);
    assert.equal(imported, 0);
  });
  console.log(`Passed ${passed} fixture tests (platform branches simulated; no desktop or Windows runtime claim).`);
} finally {
  if (storageDescriptor) Object.defineProperty(globalThis, "localStorage", storageDescriptor);
  else delete globalThis.localStorage;
  await rm(temp, { recursive: true, force: true });
}
