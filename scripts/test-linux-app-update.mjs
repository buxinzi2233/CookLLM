// Shared by test-linux-ui.mjs: execute real component handlers with isolated hooks/IPC.
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as icons from "lucide-react";

export async function appUpdateFixtures({ test, p, bridge, i18n, expression, UpdateDialog, ChangelogViewer, update, platform, calls, globalValue, native }) {
  const t = (key) => i18n.formatMessage("en", key);
  const capable = (kind = "appimage") => ({ ...update, installKind: kind, installSupported: true, assetName: `CookLLM.${kind}`, assetUrl: `https://example.test/CookLLM.${kind}`, assetSha256: "a".repeat(64) });
  const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
  const tick = () => new Promise(resolve => setImmediate(resolve));
  await test("Linux app capability requires backend permission and a matching complete asset", async () => {
    for (const kind of ["appimage", "deb", "rpm"]) assert.equal(p.canAutoInstallApp("linux", capable(kind)), true);
    for (const patch of [{ installSupported: false }, { installSupported: undefined }, { installKind: "manual" }, { installKind: undefined }, { assetName: null }, { assetUrl: null }, { assetSha256: null }, { assetSha256: "a" }, { assetName: "wrong.rpm" }, { assetUrl: "https://example.test/other.appimage" }]) {
      const value = { ...capable(), ...patch };
      assert.equal(p.canAutoInstallApp("linux", value), false);
      await assert.rejects(() => bridge.downloadAppUpdate(value), /manual/i);
      await assert.rejects(() => bridge.installAppUpdate("/tmp/app", value), /manual/i);
    }
    assert.equal(calls.length, 0);
    // No navigator architecture or filename alone establishes the installed format.
    platform("Linux aarch64"); assert.equal(p.canAutoInstallApp(undefined, capable()), true);
  });
  await test("Linux app bridge preserves download payload and installer result/path contract", async () => {
    const value = capable();
    assert.equal(await bridge.downloadAppUpdate(value), "/fixture/installer");
    assert.deepEqual(calls[0].args, { url: value.assetUrl, fileName: value.assetName, size: null, sha256: value.assetSha256 });
    for (const kind of ["deb", "rpm", "appimage"]) {
      const result = { status: kind === "appimage" ? "restarting" : "installer-opened", message: "fixture", backupPath: "/fixture/backup" };
      globalValue("window", { __TAURI_INTERNALS__: { invoke: async (command, args) => { assert.equal(command, "install_app_update"); assert.deepEqual(args, { path: "/fixture/installer" }); return result; } } });
      assert.deepEqual(await bridge.installAppUpdate("/fixture/installer", capable(kind)), result);
    }
    platform("Win32"); globalValue("window", { __TAURI_INTERNALS__: { invoke: async () => null } });
    assert.equal(await bridge.installAppUpdate("C:\\installer.exe"), null);
    globalValue("window", { __TAURI_INTERNALS__: native });
  });
  await test("app and engine progress sources are mutually exclusive with legacy compatibility", () => {
    for (const source of ["app", "engine", undefined]) for (const target of ["app", "engine"]) assert.equal(p.acceptsDownloadProgress({ source }, target), source === undefined || source === target);
  });

  async function session(value = capable(), overrides = {}) {
    const slots = [], effects = [], pending = [];
    let cursor = 0, tree, component, progressHandler, closed = false;
    const deps = {
      ...p, ...Object.fromEntries(["AlertTriangle", "ArrowRight", "CheckCircle2", "Download", "FileText", "Loader2", "PackageCheck", "RefreshCw", "Rocket", "Sparkles", "X"].map(name => [name, icons[name]])), React, APP_VERSION: "fixture", useI18n: () => ({ t }), formatBytes: String, formatMB: String, ChangelogViewer,
      useState(initial) { const index = cursor++; if (!(index in slots)) slots[index] = initial; return [slots[index], next => { slots[index] = typeof next === "function" ? next(slots[index]) : next; }]; },
      useRef(initial) { const index = cursor++; return slots[index] ??= { current: initial }; },
      useEffect(fn, dependencies) { const index = cursor++; const old = effects[index]; if (!old || dependencies.some((value, i) => value !== old.dependencies[i])) { pending.push(() => { old?.cleanup?.(); effects[index] = { dependencies, cleanup: fn() }; }); } },
      onDownloadProgress: async fn => { progressHandler = fn; return () => {}; },
      downloadAppUpdate: async () => "/fixture/app", installAppUpdate: async () => ({ status: "restarting", message: "Restarting fixture" }), cancelAppUpdate: async () => {}, openExternal: async () => {}, ...overrides,
    };
    component = await expression("components/AppUpdateDialog.tsx", "AppUpdateSession", deps);
    const render = () => { cursor = 0; tree = component({ update: value, onClose: () => { closed = true; } }); pending.splice(0).forEach(fn => fn()); return tree; };
    const buttons = node => { if (!node || typeof node !== "object") return []; const children = React.Children.toArray(node.props?.children); return [...(node.type === "button" ? [node] : []), ...children.flatMap(buttons)]; };
    render();
    return {
      slots, render,
      html: () => renderToStaticMarkup(render()),
      click(label) { const button = buttons(render()).find(node => renderToStaticMarkup(node).includes(label)); assert.ok(button, `button ${label}`); button.props.onClick(); render(); },
      progress(value) { progressHandler?.(value); },
      unmount() { effects.forEach(effect => effect?.cleanup?.()); },
      get closed() { return closed; },
    };
  }
  await test("actual AppImage dialog downloads, permits ready close, and waits on restarting", async () => {
    const s = await session(); assert.match(s.html(), /backed up before restarting/);
    s.click("Download &amp; Install"); await tick(); assert.match(s.html(), /Close &amp; Install/);
    assert.equal(await expression("components/AppUpdateDialog.tsx", "canClose", { phase: "ready" }), true);
    s.click("Close &amp; Install"); await tick(); assert.match(s.html(), /Restarting fixture/); assert.equal(s.slots[0], "launch"); s.unmount();
  });
  await test("actual deb/rpm handlers open system installer without claiming updated or blocking close", async () => {
    for (const kind of ["deb", "rpm"]) {
      const s = await session(capable(kind), { installAppUpdate: async () => ({ status: "installer-opened", message: "fixture installer" }) });
      s.click("Download &amp; Install"); await tick(); s.click("Open system installer"); await tick();
      assert.equal(s.slots[0], "opened"); assert.match(s.html(), /System installer opened/); assert.match(s.html(), /keeps running/); assert.doesNotMatch(s.html(), /class="download-done-icon spin"/);
      s.click("aria-label"); assert.equal(s.closed, true); s.unmount();
    }
  });
  await test("actual install errors and unknown/null Linux results restore retry without redownload", async () => {
    for (const result of [null, { status: "unexpected" }, new Error("installer failed")]) {
      let attempts = 0;
      const s = await session(capable(), { installAppUpdate: async () => { attempts++; if (attempts === 1) { if (result instanceof Error) throw result; return result; } return { status: "restarting", message: "retried" }; } });
      s.click("Download &amp; Install"); await tick(); s.click("Close &amp; Install"); await tick();
      assert.equal(s.slots[0], "error"); assert.match(s.html(), /Retry/); s.click("Retry"); await tick(); assert.equal(s.slots[0], "launch"); assert.equal(attempts, 2); s.unmount();
    }
  });
  await test("actual Windows null result preserves restart waiting without capability fields", async () => {
    platform("Win32"); const s = await session(update, { installAppUpdate: async () => null });
    s.click("Download &amp; Install"); await tick(); s.click("Close &amp; Install"); await tick(); assert.equal(s.slots[0], "launch"); s.unmount();
  });
  await test("actual cancel invalidates download success/error before cancellation IPC finishes", async () => {
    for (const rejectDownload of [false, true]) {
      const download = deferred(), cancel = deferred();
      const s = await session(capable(), { downloadAppUpdate: () => download.promise, cancelAppUpdate: () => cancel.promise });
      s.click("Download &amp; Install"); s.click("Cancel");
      if (rejectDownload) download.reject(new Error("late download")); else download.resolve("/late");
      await tick(); assert.equal(s.slots[0], "cancel"); assert.equal(s.slots[2], "");
      cancel.resolve(); await tick(); assert.equal(s.slots[0], "info"); s.unmount();
    }
  });
  await test("actual cancellation failure is visible and retryable; late download stays ignored", async () => {
    const download = deferred();
    const s = await session(capable(), { downloadAppUpdate: () => download.promise, cancelAppUpdate: async () => { throw new Error("cancel fixture failed"); } });
    s.click("Download &amp; Install"); s.click("Cancel"); await tick(); assert.match(s.html(), /cancel fixture failed/);
    download.resolve("/late"); await tick(); assert.equal(s.slots[0], "error"); assert.match(s.html(), /Retry/); s.unmount();
  });
  await test("actual dialog progress handler ignores engine events and all events after cancellation", async () => {
    const download = deferred(), cancel = deferred();
    const s = await session(capable(), { downloadAppUpdate: () => download.promise, cancelAppUpdate: () => cancel.promise });
    s.click("Download &amp; Install"); s.progress({ source: "engine", percent: 50 }); assert.equal(s.slots[1], null);
    s.progress({ source: "app", percent: 25 }); assert.equal(s.slots[1].percent, 25);
    s.progress({ percent: 30 }); assert.equal(s.slots[1].percent, 30);
    s.click("Cancel"); s.progress({ source: "app", percent: 99 }); assert.equal(s.slots[1], null);
    cancel.resolve(); download.resolve("/late"); await tick(); s.unmount();
  });
  await test("actual dialog close/reopen and same-tag asset changes isolate late completion", async () => {
    const value = capable(), element = UpdateDialog({ open: true, update: value, onClose() {} });
    assert.equal(UpdateDialog({ open: false, update: value, onClose() {} }), null);
    for (const patch of [{ latestTag: "v100" }, { assetSha256: "b".repeat(64) }, { assetUrl: value.assetUrl + "?new" }, { installSupported: false }]) assert.notEqual(UpdateDialog({ open: true, update: { ...value, ...patch }, onClose() {} }).key, element.key);
    const download = deferred(); const old = await session(value, { downloadAppUpdate: () => download.promise }); old.click("Download &amp; Install"); old.unmount();
    const fresh = await session(value); download.resolve("/old/session"); await tick(); assert.equal(old.slots[0], "download"); assert.equal(fresh.slots[0], "info"); assert.equal(fresh.slots[2], ""); fresh.unmount();
  });
  await test("actual Settings progress callback excludes app completion and accepts engine/legacy events", async () => {
    const seen = [], logs = [];
    const handler = await expression("components/settings/EngineCard.tsx", "handleEngineProgress", { ...p, active: true, t, setProgress: value => seen.push(value), onLog: value => logs.push(value) });
    handler({ source: "app", phase: "done" }); assert.equal(seen.length, 0); assert.equal(logs.length, 0);
    handler({ source: "engine", phase: "done" }); handler({ phase: "done" }); assert.equal(seen.length, 2); assert.equal(logs.length, 2);
  });
  await test("actual late install and cancel results after unmount cannot alter reopened session", async () => {
    for (const kind of ["install", "cancel"]) {
      const late = deferred(), download = deferred();
      const old = await session(capable(), kind === "install" ? { installAppUpdate: () => late.promise } : { downloadAppUpdate: () => download.promise, cancelAppUpdate: () => late.promise });
      old.click("Download &amp; Install");
      if (kind === "install") { await tick(); old.click("Close &amp; Install"); } else old.click("Cancel");
      old.unmount(); const fresh = await session(capable()); const previous = old.slots[0];
      late.reject(new Error("late task error")); download.resolve("/old"); await tick();
      assert.equal(old.slots[0], previous); assert.equal(fresh.slots[0], "info"); assert.equal(fresh.slots[3], ""); fresh.unmount();
    }
  });
  await test("new app-update messages exist in Chinese and English", () => {
    for (const key of ["update.assetMissing", "update.appImageHint", "update.packageHint", "update.installerOpened", "update.openInstaller", "update.unknownResult", "update.cancelling"]) assert.notEqual(i18n.formatMessage("zh", key), i18n.formatMessage("en", key));
  });
}
