import { Check, ChevronDown, Cpu, Download, Info, Loader2, RefreshCw, RotateCw, Settings, Wrench, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { cancelLlamaCppUpdate, checkLlamaCppUpdate, detectHardware, downloadLlamaCpp, engineDownloadInfo, engineReleaseUpToDate, getLlamaCppStatus, loadConfig, onDownloadProgress, type DownloadProgress, type LlamaCppLocalStatus, type LlamaCppRelease } from "../../tauri";
import type { AppConfig, LlamaBackend, LlamaEngine, LlamaLogPayload } from "../../types";
import { acceptsDownloadProgress, autoEngineBackend, canAutoInstallEngine, defaultEngineBackend, getPlatform } from "../../platform";
import { cn, formatBytes, formatEngineBackend, formatMB, getEngineTypeDisplay } from "../../utils";

interface Props {
  config: AppConfig;
  activeEngine?: LlamaEngine;
  onPersist: (config: AppConfig, message?: string) => Promise<void>;
  onLog: (line: string, stream?: LlamaLogPayload["stream"]) => void;
  onOpenEngineHub?: () => void;
}

export default function EngineCard({ config, activeEngine, onPersist, onLog, onOpenEngineHub }: Props) {
  const { t } = useI18n();
  const [engineStatus, setEngineStatus] = useState<LlamaCppLocalStatus | null>(null);
  const [remote, setRemote] = useState<LlamaCppRelease | null>(null);
  const [checking, setChecking] = useState(false);
  const [backend, setBackend] = useState<LlamaBackend>(autoEngineBackend(config.llamaBackend));
  const [cudaVersion, setCudaVersion] = useState(config.llamaCudaVersion || (getPlatform() === "linux" ? "auto" : "12"));
  const engineSelection = useRef({ backend, cudaVersion, generation: 0, userSelected: false, initialized: false });
  const engineOperation = useRef({ id: 0, active: false, mounted: true });
  const [cudaMenuOpen, setCudaMenuOpen] = useState(false);
  const cudaMenuRef = useRef<HTMLDivElement>(null);
  const [updating, setUpdating] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [engineError, setEngineError] = useState<string | null>(null);

  const activeEngineType = getEngineTypeDisplay(activeEngine?.engineType, activeEngine?.path);
  const isLlamaCppEngine = activeEngineType.key === "llamacpp";

  useEffect(() => {
    // A persisted snapshot or delayed refresh must not reset an immediate UI choice.
    if (engineSelection.current.userSelected || engineOperation.current.active) return;
    const nextBackend = autoEngineBackend(config.llamaBackend);
    const nextCuda = config.llamaCudaVersion || (getPlatform() === "linux" ? "auto" : "12");
    if (nextBackend === engineSelection.current.backend && nextCuda === engineSelection.current.cudaVersion) return;
    Object.assign(engineSelection.current, { backend: nextBackend, cudaVersion: nextCuda });
    setBackend(nextBackend); setCudaVersion(nextCuda);
    void ensureRemote(nextBackend, nextCuda, true);
  }, [config.llamaBackend, config.llamaCudaVersion]);

  const selectEngine = (nextBackend: LlamaBackend, nextCuda = engineSelection.current.cudaVersion) => {
    if (engineOperation.current.active) return;
    Object.assign(engineSelection.current, { backend: nextBackend, cudaVersion: nextCuda, userSelected: true });
    setBackend(nextBackend); setCudaVersion(nextCuda); setRemote(null);
    void onPersist({ ...config, llamaBackend: nextBackend, llamaCudaVersion: nextCuda }, "");
    void ensureRemote(nextBackend, nextCuda);
  };

  useEffect(() => {
    if (!cudaMenuOpen) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (cudaMenuRef.current && !cudaMenuRef.current.contains(e.target as Node)) {
        setCudaMenuOpen(false);
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCudaMenuOpen(false);
    };
    window.addEventListener("mousedown", handleClickOutside);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("mousedown", handleClickOutside);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [cudaMenuOpen]);

  const refreshEngine = async (targetPath?: string) => {
    try {
      const p = targetPath ?? activeEngine?.path ?? config.serverPath;
      const [hw, st] = await Promise.all([
        detectHardware(),
        getLlamaCppStatus(p || undefined),
      ]);
      if (!engineOperation.current.mounted) return;
      setEngineStatus(st);
      if (!engineSelection.current.initialized && !engineSelection.current.userSelected && !engineOperation.current.active) {
        engineSelection.current.initialized = true;
        const preferred = config.llamaBackend ?? hw?.recommendedBackend ?? st?.localBackend;
        const nextBackend = canAutoInstallEngine(getPlatform(), preferred) ? autoEngineBackend(preferred) : defaultEngineBackend();
        const nextCuda = config.llamaCudaVersion || (getPlatform() === "linux" ? "auto" : st?.cudaVersion?.split(".")[0] || "12");
        const changed = nextBackend !== engineSelection.current.backend || nextCuda !== engineSelection.current.cudaVersion;
        Object.assign(engineSelection.current, { backend: nextBackend, cudaVersion: nextCuda });
        setBackend(nextBackend); setCudaVersion(nextCuda);
        if (changed) void ensureRemote(nextBackend, nextCuda, true);
      }
    } catch { /* 非 Tauri 环境忽略 */ }
  };
  useEffect(() => {
    void refreshEngine(activeEngine?.path);
    void ensureRemote(undefined, undefined, true);
  }, [config.serverPath, config.activeEngineId]);

  /** 检查远程最新版本并与本地版本比对；检查更新 / 一键更新 / 强制重装三处共用的前置步骤 */
  const ensureRemote = async (
    overrideBackend?: LlamaBackend,
    overrideCuda?: string,
    silent = false
  ): Promise<LlamaCppRelease | null> => {
    const useBackend = overrideBackend ?? engineSelection.current.backend;
    const useCuda = overrideCuda ?? engineSelection.current.cudaVersion;
    const generation = ++engineSelection.current.generation;
    const isCurrent = () => engineOperation.current.mounted && generation === engineSelection.current.generation;
    setRemote(null);
    if (!canAutoInstallEngine(getPlatform(), useBackend)) {
      if (!silent) setEngineError(t("linux.engineGate"));
      return null;
    }
    setChecking(true);
    setEngineError(null);
    try {
      const result = await checkLlamaCppUpdate(useBackend, useCuda);
      if (!isCurrent()) return null;
      if (getPlatform() === "linux") {
        if (result.matchAsset?.backend !== useBackend) throw new Error(t("linux.assetMissing"));
        engineDownloadInfo(result);
      }
      const upToDate = engineReleaseUpToDate(result, engineStatus, useBackend);
      const next = { ...result, upToDate };
      setRemote(next);
      return next;
    } catch (error) {
      if (isCurrent()) {
        setEngineError(error instanceof Error ? error.message : String(error));
        setRemote(null);
      }
      return null;
    } finally {
      if (isCurrent()) setChecking(false);
    }
  };

  const runCheck = async () => {
    if (checking) return;
    await ensureRemote();
  };

  // Linux exposes exact metadata versions and keeps legacy major-version requests valid.
  const discoveredCuda = Array.from(new Set((remote?.assets ?? [])
    .filter(asset => asset.backend === "cuda")
    .map(asset => (getPlatform() === "linux" ? asset.cudaFullVersion : asset.cudaVersion) || asset.cudaVersion || "")
    .filter(Boolean))).sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
  const cudaOptions = Array.from(new Set(["auto", ...discoveredCuda, "12", "13", ...(getPlatform() === "linux" ? [] : ["11"]), cudaVersion]));
  const cudaSelectOptions = cudaOptions.map(ver => ({ value: ver, label: ver === "auto" ? t("llama.cudaAuto") : `CUDA ${ver}`, full: ver }));

  const getCudaDisplayShort = () => cudaVersion === "auto"
    ? remote?.matchAsset?.cudaFullVersion || t("llama.cudaAuto")
    : cudaVersion;

  // 强制重装：忽略版本比较，直接重新下载安装（用于修复损坏文件）
  const forceReinstall = async () => {
    if (!canAutoInstallEngine(getPlatform(), backend)) { setEngineError(t("linux.engineGate")); return; }
    if (updating) return;
    await runUpdate(backend, true);
  };

  const cancelUpdate = () => { void cancelLlamaCppUpdate().catch(() => undefined); };

  const runUpdate = async (useBackend: LlamaBackend, force = false) => {
    if (!canAutoInstallEngine(getPlatform(), useBackend)) { setEngineError(t("linux.engineGate")); return; }
    if (updating || engineOperation.current.active) return;
    const operation = ++engineOperation.current.id;
    engineOperation.current.active = true;
    const isActive = () => engineOperation.current.mounted && engineOperation.current.id === operation;
    setUpdating(true); setEngineError(null);
    let cancelled = false;
    try {
      // Always recheck: never download using a cached tag or an asset from a prior selection.
      const selectedCuda = useBackend === "cuda" ? engineSelection.current.cudaVersion : undefined;
      const freshRelease = await ensureRemote(useBackend, selectedCuda);
      if (!freshRelease || (!force && freshRelease.upToDate) || !isActive()) return;
      const { engine, total } = engineDownloadInfo(freshRelease);
      const generation = engineSelection.current.generation;
      setProgress({ phase: "download", percent: 0, downloaded: 0, total, speedBps: 0, message: "" });
      const path = await downloadLlamaCpp({ backend: useBackend, cudaVersion: selectedCuda,
        // Windows resolves its own cudart pair only when explicit asset fields are absent.
        tag: freshRelease.tag, ...(getPlatform() === "linux" ? { assetUrl: engine.url, assetName: engine.fileName } : {}) });
      if (!isActive()) return;
      if (getPlatform() === "linux") {
        // The installer saved directory, backend and engine metadata atomically.
        // Adopt that entire snapshot, never merge the pre-download config over it.
        const installed = await loadConfig();
        if (!isActive()) return;
        if (!installed) throw new Error(t("linux.configReloadFailed"));
        await onPersist(installed, "");
        if (!isActive()) return;
        const installedStatus = await getLlamaCppStatus().catch(() => null);
        if (!isActive()) return;
        setEngineStatus(installedStatus);
        const installedBackend = autoEngineBackend(installed.llamaBackend);
        Object.assign(engineSelection.current, { backend: installedBackend, cudaVersion: installed.llamaCudaVersion || engineSelection.current.cudaVersion });
        setBackend(installedBackend);
        setCudaVersion(installed.llamaCudaVersion || engineSelection.current.cudaVersion);
      } else {
        const parentDir = path.replace(/[/\\][^/\\]+$/, "");
        const newStatus = await getLlamaCppStatus(parentDir).catch(() => null);
        if (!isActive()) return;
        const installedCuda = newStatus?.cudaVersion || (useBackend === "cuda" ? getCudaDisplayShort() : undefined);
        const installedVer = newStatus?.localVersion || freshRelease.tag;

        let nextEngines = config.engines && config.engines.length > 0 ? [...config.engines] : [];
        if (config.activeEngineId && nextEngines.length > 0) {
          const idx = nextEngines.findIndex((e) => e.id === config.activeEngineId);
          if (idx >= 0) {
            nextEngines[idx] = {
              ...nextEngines[idx],
              path,
              backend: useBackend,
              cudaVersion: installedCuda,
              version: installedVer,
            };
          }
        } else if (nextEngines.length > 0) {
          nextEngines[0] = {
            ...nextEngines[0],
            path,
            backend: useBackend,
            cudaVersion: installedCuda,
            version: installedVer,
          };
        }
        await onPersist(
          {
            ...config,
            serverPath: path,
            llamaBackend: useBackend,
            llamaCudaVersion: cudaVersion,
            engines: nextEngines,
          },
          ""
        );
      }
      if (!isActive()) return;
      onLog(t("llama.updated", { path }), "system");
      if (getPlatform() !== "linux") await refreshEngine();
      // 更新已完成：将 remote 标记为已是最新，避免按钮仍显示“立即更新”
      if (isActive() && generation === engineSelection.current.generation) {
        setRemote({ ...freshRelease, upToDate: true });
      }
    } catch (error) {
      if (!isActive()) return;
      const raw = error instanceof Error ? error.message : String(error);
      cancelled = raw.includes("取消") || /cancelled|canceled/i.test(raw);
      setProgress(null);
      setEngineError(cancelled ? t("llama.cancelled") : raw);
      onLog(cancelled ? t("llama.cancelled") : t("llama.failed", { error: raw }), cancelled ? "system" : "stderr");
    } finally {
      if (isActive()) {
        engineOperation.current.active = false;
        setUpdating(false);
        window.setTimeout(() => { if (isActive() && !engineOperation.current.active) setProgress(null); }, cancelled ? 400 : 1200);
      }
    }
  };

  // 一键更新：重新检查当前选择，已是最新则跳过下载（不再弹确认窗）
  const startUpdate = async () => {
    if (!canAutoInstallEngine(getPlatform(), backend)) { setEngineError(t("linux.engineGate")); return; }
    if (updating) return;
    await runUpdate(backend);
  };
  // 订阅下载进度事件（仅 Tauri）
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    engineOperation.current.mounted = true;
    const handleEngineProgress = (payload: DownloadProgress) => {
      if (!active || !engineOperation.current.active || !acceptsDownloadProgress(payload, "engine")) return;
      setProgress(payload);
      if (payload.phase === "done") {
        onLog(t("llama.progress.done"), "system");
      }
    };
    void onDownloadProgress(handleEngineProgress).then((fn) => { if (active) unlisten = fn; else fn?.(); }).catch((error) => { if (active) setEngineError(String(error)); });
    return () => {
      active = false; unlisten?.();
      engineOperation.current.mounted = false;
      engineOperation.current.active = false;
      engineOperation.current.id++;
      engineSelection.current.generation++;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const downloadInfo = remote?.matchAsset ? engineDownloadInfo(remote) : null;
  const isUpToDate = remote !== null && remote.upToDate === true;

  const speedText = progress && progress.speedBps > 0
    ? `${formatBytes(progress.speedBps)}/s`
    : "";

  const sizeText = progress && (progress.downloaded > 0 || progress.total > 0)
    ? progress.total > 0
      ? `${formatMB(progress.downloaded)} / ${formatMB(progress.total)}`
      : formatMB(progress.downloaded)
    : "";

  const progressPercent = progress ? Math.min(100, progress.percent) : 0;

  return (
    <>
      <div className="settings-card engine-management-card">
        <div className="settings-card-header">
          <div className="settings-card-icon"><Settings size={16} /></div>
          <h3>{t("llama.title")}</h3>
          <div className="settings-card-header-actions">
            <button
              type="button"
              className="secondary-button compact engine-header-hub-btn"
              onClick={() => onOpenEngineHub?.()}
              title={t("llama.manageHubBtn")}
            >
              <Wrench size={13} />
              <span>{t("llama.manageHubBtn")}</span>
            </button>
          </div>
        </div>
        <div className="settings-card-body">
          <p className="about-desc">
            {getPlatform() === "linux"
              ? t(canAutoInstallEngine(getPlatform(), backend) ? "linux.autoEngine" : "linux.engineGate")
              : t(canAutoInstallEngine(getPlatform(), backend) ? "llama.desc" : "linux.manualEngine")}
          </p>
          {/* 行 1：当前运行分支 */}
          <div className="engine-card-row">
            <span className="engine-row-label">{t("llama.currentBranchTitle")}</span>
            <div className="engine-row-main">
              <span className="engine-name-text">{activeEngine?.name || "llama.cpp"}</span>
              <span className="engine-pill-tag backend">
                {formatEngineBackend(
                  activeEngine?.backend,
                  (activeEngine?.id === config.activeEngineId || !config.activeEngineId ? engineStatus?.cudaVersion : undefined) || activeEngine?.cudaVersion
                )}
              </span>
              {(activeEngine?.version || engineStatus?.localVersion) && (
                <span className="engine-pill-tag version">
                  {activeEngine?.version || engineStatus?.localVersion}
                </span>
              )}
            </div>
            <div className="engine-row-right">
              {isLlamaCppEngine ? (
                <div className="mini-seg engine-backend-seg" ref={cudaMenuRef}>
                  {/* CUDA 复合胶囊按钮 */}
                  <div className={cn("mini-seg-item cuda-compound-pill", backend === "cuda" && "active")}>
                    <button
                      type="button"
                      className="cuda-text-btn"
                      disabled={!canAutoInstallEngine(getPlatform(), "cuda") || updating}
                      onClick={() => {
                        selectEngine("cuda");
                        if (backend === "cuda") {
                          // 若已处于 CUDA 分支，再次点击 CUDA 文本也可顺畅触发/收起版本菜单
                          setCudaMenuOpen((prev) => !prev);
                        } else {
                          setCudaMenuOpen(false);
                        }
                      }}
                    >
                      CUDA
                    </button>

                    <button
                      type="button"
                      className={cn("cuda-dropdown-trigger", cudaMenuOpen && "open")}
                      disabled={!canAutoInstallEngine(getPlatform(), "cuda") || updating}
                      onClick={(e) => {
                        e.stopPropagation();
                        if (backend !== "cuda") {
                          selectEngine("cuda");
                        } else {
                          // CPU metadata can lack cudart assets: recheck on opening CUDA.
                          void ensureRemote("cuda", cudaVersion);
                        }
                        setCudaMenuOpen((prev) => !prev);
                      }}
                      title={t("llama.cudaVersionLabel")}
                    >
                      <span className="cuda-version-badge">({getCudaDisplayShort()})</span>
                      <ChevronDown size={11} className={cn("cuda-chevron", cudaMenuOpen && "open")} />
                    </button>

                    {/* 浮动下拉菜单 */}
                    {cudaMenuOpen && (
                      <div className="cuda-version-dropdown-menu" onClick={(e) => e.stopPropagation()}>
                        <div className="cuda-dropdown-list">
                          {cudaSelectOptions.map((opt) => {
                            const isSelected = cudaVersion === opt.value;
                            return (
                              <button
                                key={opt.value}
                                type="button"
                                className={cn("cuda-dropdown-item", isSelected && "selected")}
                                onClick={() => {
                                  selectEngine("cuda", opt.value);
                                  setCudaMenuOpen(false);
                                }}
                              >
                                <span className="opt-label">{opt.label}</span>
                                {isSelected && <Check size={13} className="opt-check" />}
                              </button>
                            );
                          })}
                        </div>
                      </div>
                    )}
                  </div>

                  <span className="mini-seg-divider" />

                  {/* Vulkan */}
                  <button
                    type="button"
                    className={cn("mini-seg-btn", backend === "vulkan" && "active")}
                    disabled={updating}
                    onClick={() => {
                      setCudaMenuOpen(false);
                      selectEngine("vulkan");
                    }}
                  >
                    Vulkan
                  </button>

                  <span className="mini-seg-divider" />

                  {/* CPU */}
                  <button
                    type="button"
                    className={cn("mini-seg-btn", backend === "cpu" && "active")}
                    disabled={updating}
                    onClick={() => {
                      setCudaMenuOpen(false);
                      selectEngine("cpu");
                    }}
                  >
                    CPU
                  </button>
                </div>
              ) : null}
            </div>
          </div>

          {/* 行 2：版本状态 */}
          <div className="engine-card-row">
            <span className="engine-row-label">{t("llama.versionStatusTitle")}</span>
            <div className="engine-row-main">
              {isLlamaCppEngine ? (
                <span className="engine-status-text">
                  {t("llama.localVersionLabel")} {activeEngine?.version || engineStatus?.localVersion || t("llama.localVersionNone")}
                  {"  ·  "}
                  {t("llama.remoteVersionLabel")} {remote?.tag || (checking ? t("llama.checking") : "--")}
                </span>
              ) : (
                <span className="engine-status-text">
                  {t("llama.localVersionLabel")} {activeEngine?.version || engineStatus?.localVersion || t("llama.localVersionNone")}
                  {"  ·  "}
                  <span className="engine-nonllama-hint-inline">
                    <Info size={13} className="engine-nonllama-inline-icon" />
                    {t("llama.nonLlamaUpdateHint", { engine: activeEngineType.label })}
                  </span>
                </span>
              )}
            </div>
            <div className="engine-row-right">
              {isLlamaCppEngine && (
                <div className="engine-row-actions">
                  <button
                    type="button"
                    className="secondary-button compact"
                    disabled={!canAutoInstallEngine(getPlatform(), backend) || checking || updating}
                    onClick={() => void runCheck()}
                  >
                    <RefreshCw size={13} className={checking ? "spin" : ""} />
                    <span>{t("llama.checkUpdateBtn")}</span>
                  </button>

                  {isUpToDate ? (
                    <button
                      type="button"
                      className="secondary-button compact"
                      disabled={!canAutoInstallEngine(getPlatform(), backend) || updating || checking}
                      onClick={() => void forceReinstall()}
                    >
                      <RotateCw size={13} className={updating ? "spin" : ""} />
                      <span>{t("llama.forceReinstall")}</span>
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="engine-primary-btn compact"
                      disabled={!canAutoInstallEngine(getPlatform(), backend) || updating || checking}
                      onClick={() => void startUpdate()}
                    >
                      {updating ? <Loader2 size={13} className="spin" /> : <Download size={13} />}
                      <span>{remote?.tag ? t("llama.updateBtnTag", { tag: remote.tag }) : t("llama.checkAndUpdate")}</span>
                    </button>
                  )}
                </div>
              )}
            </div>
          </div>

          {getPlatform() === "linux" && backend === "cuda" && (
            <>
              {downloadInfo?.runtime && <p className="about-desc" role="status">{t("linux.cudaDownloadSize", {
                engine: formatBytes(downloadInfo.engine.size), runtime: formatBytes(downloadInfo.runtime.size), total: formatBytes(downloadInfo.total),
              })}</p>}
              <p className="about-desc">{t("linux.cudaRuntimeHint")}</p>
              <p className="about-desc">{t("linux.cudaCompatibility")}</p>
            </>
          )}

          {engineError && <p className="import-error">{engineError}</p>}
        </div>
      </div>

      {/* 下载进度弹窗（极简设计） */}
      {progress && (
        <div className="modal-backdrop">
          <div className="import-modal download-modal" role="dialog" aria-modal="true">
            <header className="update-modal-header">
              <div className="update-modal-title">
                <Cpu size={17} className="update-modal-icon" />
                <h2>{t("llama.updateModalTitle")}</h2>
                {remote?.tag && <span className="update-modal-tag">{remote.tag}</span>}
              </div>
              {updating && progress.phase !== "done" && (
                <button className="ghost-icon" onClick={() => void cancelUpdate()} title={t("llama.cancelUpdate")}>
                  <X size={18} />
                </button>
              )}
            </header>

            <div className="download-body">
              {progress.phase === "done" ? (
                <div className="download-done-simple">
                  <div className="download-done-icon">
                    <Check size={24} strokeWidth={2.4} />
                  </div>
                  <div className="download-done-info">
                    <h3>{t("llama.progress.done")}</h3>
                    <p>{remote?.tag ? `${remote.tag} · ` : ""}{t(getPlatform() === "linux" ? "linux.installed" : "llama.progress.done")}</p>
                  </div>
                </div>
              ) : (
                <div className="download-minimal">
                  <div className="download-minimal-header">
                    <div className="download-minimal-left">
                      <span className="download-status-dot" />
                      <span className="download-status-title">
                        {progress.phase === "extract"
                          ? t("llama.progress.extract", { percent: progressPercent })
                          : progress.phase === "install"
                            ? t(getPlatform() === "linux" ? "linux.installing" : "llama.progress.install")
                            : t("llama.progress.download", { percent: progressPercent })}
                      </span>
                      {speedText && progress.phase === "download" && (
                        <span className="download-status-speed">{speedText}</span>
                      )}
                    </div>
                    <div className="download-minimal-right">
                      {sizeText && progress.phase === "download" && (
                        <span className="download-status-size">{sizeText}</span>
                      )}
                      <span className="download-status-percent">{progressPercent}%</span>
                    </div>
                  </div>

                  <div className="download-bar">
                    <div className="download-bar-inner" style={{ width: `${progressPercent}%` }} />
                  </div>

                  <div className="download-minimal-sub">
                    <span className="download-minimal-hint">
                      {progress.phase === "extract"
                        ? t("llama.progress.extract", { percent: progressPercent })
                        : progress.phase === "install"
                          ? t(getPlatform() === "linux" ? "linux.installing" : "llama.progress.install")
                          : t("linux.keepOpen")}
                    </span>
                    <span className="download-minimal-tag">
                      {backend.toUpperCase()}{backend === "cuda" && cudaVersion && cudaVersion !== "auto" ? ` · CUDA ${cudaVersion}` : ""}
                    </span>
                  </div>
                </div>
              )}
            </div>

            <footer>
              {progress.phase === "done" ? (
                <button className="primary-button" onClick={() => setProgress(null)}>
                  <Check size={14} />{t("llama.progress.done")}
                </button>
              ) : updating ? (
                <button className="secondary-button" onClick={() => void cancelUpdate()}>
                  <X size={14} />{t("llama.cancelUpdate")}
                </button>
              ) : (
                <button className="secondary-button" onClick={() => setProgress(null)}>
                  <X size={14} />{t("win.close")}
                </button>
              )}
            </footer>
          </div>
        </div>
      )}
    </>
  );
}
