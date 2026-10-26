import { useEffect, useRef, useState } from "react";
import { AlertTriangle, ArrowRight, CheckCircle2, Download, FileText, Loader2, PackageCheck, RefreshCw, Rocket, Sparkles, X } from "lucide-react";
import { APP_VERSION } from "../data";
import { useI18n } from "../i18n";
import { openExternal, cancelAppUpdate, downloadAppUpdate, installAppUpdate, onDownloadProgress, type DownloadProgress, type UpdateCheckResult } from "../tauri";
import { formatBytes, formatMB } from "../utils";
import ChangelogViewer from "./ChangelogViewer";

import { acceptsDownloadProgress, canAutoInstallApp, getPlatform, installerPackageType } from "../platform";

type UpdatePhase = "info" | "download" | "ready" | "launch" | "opened" | "cancel" | "error";

export default function AppUpdateDialog({ open, update, onClose }: { open: boolean; update: UpdateCheckResult | null; onClose: () => void }) {
  if (!open || !update) return null;
  // A new resource or a reopened dialog owns a fresh session, never an old task's state.
  const sessionKey = JSON.stringify([update.latestTag, update.assetUrl, update.assetName, update.assetSha256, update.assetSize, update.installKind, update.installSupported, update.installReason]);
  return <AppUpdateSession key={sessionKey} update={update} onClose={onClose} />;
}

function AppUpdateSession({ update, onClose }: { update: UpdateCheckResult; onClose: () => void }) {
  const { t } = useI18n();
  const [phase, setPhase] = useState<UpdatePhase>("info");
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [installerPath, setInstallerPath] = useState("");
  const [error, setError] = useState("");
  const [installMessage, setInstallMessage] = useState("");
  const generation = useRef(0);
  const mounted = useRef(true);
  const busy = useRef<"download" | "cancel" | "install" | null>(null);
  const autoInstall = canAutoInstallApp(getPlatform(), update);
  const packageInstall = getPlatform() === "linux" && (update.installKind === "deb" || update.installKind === "rpm");
  const installHint = getPlatform() === "linux" ? t(packageInstall ? "update.packageHint" : "update.appImageHint") : t("update.readyHint");
  const isCurrent = (id: number) => mounted.current && generation.current === id;

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current++; };
  }, []);

  useEffect(() => {
    if (phase !== "download") return;
    let active = true;
    const id = generation.current;
    const unlisten = onDownloadProgress((payload) => {
      if (active && isCurrent(id) && busy.current === "download" && acceptsDownloadProgress(payload, "app")) setProgress(payload);
    });
    void unlisten.catch((caught) => { if (active && isCurrent(id)) setError(String(caught)); });
    return () => {
      active = false;
      void unlisten.then((listener) => listener?.(), () => undefined);
    };
  }, [phase]);

  const closeDialog = () => {
    generation.current++;
    mounted.current = false;
    onClose();
  };

  const startDownload = async () => {
    if (!canAutoInstallApp(getPlatform(), update)) { setError(update.installReason || t("linux.manualUpdate")); setPhase("error"); return; }
    if (busy.current || !mounted.current) return;
    const id = ++generation.current;
    busy.current = "download";
    setPhase("download");
    setError("");
    setInstallerPath("");
    setProgress(null);
    try {
      const path = await downloadAppUpdate(update);
      if (!isCurrent(id)) return;
      setInstallerPath(path);
      setPhase("ready");
    } catch (caught) {
      if (!isCurrent(id)) return;
      setError(caught instanceof Error ? caught.message : String(caught));
      setPhase("error");
    } finally {
      if (isCurrent(id)) busy.current = null;
    }
  };

  const cancelDownload = async () => {
    if (busy.current !== "download") return;
    // Invalidate download success/error before waiting for cancellation IPC.
    const id = ++generation.current;
    busy.current = "cancel";
    setPhase("cancel");
    setProgress(null);
    setInstallerPath("");
    try {
      await cancelAppUpdate();
      if (isCurrent(id)) setPhase("info");
    } catch (caught) {
      if (!isCurrent(id)) return;
      setError(caught instanceof Error ? caught.message : String(caught));
      setPhase("error");
    } finally {
      if (isCurrent(id)) busy.current = null;
    }
  };

  const startInstall = async () => {
    if (!canAutoInstallApp(getPlatform(), update)) { setError(update.installReason || t("linux.manualUpdate")); setPhase("error"); return; }
    if (busy.current || !mounted.current || !installerPath) return;
    const id = ++generation.current;
    busy.current = "install";
    setError("");
    setPhase("launch");
    try {
      const result = await installAppUpdate(installerPath, update);
      if (!isCurrent(id)) return;
      if (result === null && getPlatform() === "windows") return;
      if (result?.status === "restarting") {
        setInstallMessage(result.message);
      } else if (result?.status === "installer-opened") {
        setInstallMessage(result.message);
        setPhase("opened");
      } else {
        throw new Error(t("update.unknownResult"));
      }
    } catch (caught) {
      if (!isCurrent(id)) return;
      setError(caught instanceof Error ? caught.message : String(caught));
      setPhase("error");
    } finally {
      if (isCurrent(id)) busy.current = null;
    }
  };

  const openRelease = async () => {
    const id = generation.current;
    try { await openExternal(update.releaseUrl); }
    catch (caught) { if (isCurrent(id)) { setError(String(caught)); setPhase("error"); } }
  };

  const speedText = progress?.speedBps ? `${formatBytes(progress.speedBps)}/s` : "";
  const sizeText = progress && (progress.downloaded > 0 || progress.total > 0)
    ? progress.total > 0
      ? `${formatMB(progress.downloaded)} / ${formatMB(progress.total)}`
      : formatMB(progress.downloaded)
    : "";
  const percent = phase === "download" && progress ? Math.min(100, Math.max(0, progress.percent)) : phase === "ready" || phase === "launch" ? 100 : 0;
  const canClose = phase === "info" || phase === "error" || phase === "ready" || phase === "opened";

  return (
    <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && canClose && closeDialog()}>
      <section className="import-modal update-modal" aria-live="polite" role="dialog" aria-modal="true">
        <header className="update-modal-header">
          <div className="update-modal-title">
            <Sparkles size={17} className="update-modal-icon" />
            <h2>{t("update.dialogTitle")}</h2>
            {update.latestTag && <span className="update-modal-tag">{update.latestTag}</span>}
          </div>
          {canClose && <button className="ghost-icon" onClick={closeDialog} aria-label={t("cancel")}><X size={18} /></button>}
        </header>

        {phase === "download" ? (
          <div className="update-body">
            <div className="download-minimal">
              <div className="download-minimal-header">
                <div className="download-minimal-left">
                  <span className="download-status-dot" />
                  <span className="download-status-title">{t("update.downloading")}</span>
                  {speedText && <span className="download-status-speed">{speedText}</span>}
                </div>
                <div className="download-minimal-right">
                  {sizeText && <span className="download-status-size">{sizeText}</span>}
                  <span className="download-status-percent">{percent}%</span>
                </div>
              </div>

              <div className="download-bar">
                <div className="download-bar-inner" style={{ width: `${percent}%` }} />
              </div>

              <div className="download-minimal-sub">
                <span className="download-minimal-hint">{t("update.keepOpen")}</span>
                {update.assetName && (
                  <span className="download-minimal-tag">
                    {installerPackageType(update.assetName)}
                  </span>
                )}
              </div>
            </div>
          </div>
        ) : phase === "ready" ? (
          <div className="update-body">
            <div className="download-done-simple">
              <div className="download-done-icon">
                <CheckCircle2 size={24} strokeWidth={2.4} />
              </div>
              <div className="download-done-info">
                <h3>{t("update.ready")}</h3>
                <p>{installHint}</p>
              </div>
            </div>
          </div>
        ) : phase === "opened" ? (
          <div className="update-body"><h3>{t("update.installerOpened")}</h3><p>{installMessage}</p><p>{t("update.packageHint")}</p></div>
        ) : phase === "cancel" ? (
          <div className="update-body" role="status">{t("update.cancelling")}</div>
        ) : phase === "launch" ? (
          <div className="update-body">
            <div className="download-done-simple">
              <div className="download-done-icon spin">
                <Loader2 size={24} />
              </div>
              <div className="download-done-info">
                <h3>{t("update.launching")}</h3>
                <p>{installMessage || installHint}</p>
              </div>
            </div>
          </div>
        ) : phase === "error" ? (
          <div className="update-body">
            <div className="download-done-simple">
              <div className="download-done-icon error">
                <AlertTriangle size={24} />
              </div>
              <div className="download-done-info">
                <h3>{t("update.failed")}</h3>
                <p>{error}</p>
              </div>
            </div>
          </div>
        ) : (
          <div className="update-body">
            <div className="update-version-card">
              <div className="update-version-col">
                <span className="update-version-tag">{t("update.current")}</span>
                <strong>v{APP_VERSION}</strong>
              </div>
              <div className="update-version-arrow">
                <ArrowRight size={15} />
              </div>
              <div className="update-version-col highlight">
                <span className="update-version-tag new">{t("update.latest")}</span>
                <strong>{update.latestTag}</strong>
              </div>
              {update.assetName && (
                <div className="update-asset-pill" title={update.assetName}>
                  <PackageCheck size={13} />
                  <span>{installerPackageType(update.assetName)}{update.assetSize ? ` · ${formatMB(update.assetSize)}` : ""}</span>
                </div>
              )}
            </div>

            <div className="update-changelog-wrap">
              <div className="update-changelog-header">
                <FileText size={13} />
                <strong>{t("update.changelog")}</strong>
              </div>
              <div className="update-changelog-content">
                <ChangelogViewer content={update.releaseNotes?.trim() || t("update.noChangelog")} />
              </div>
            </div>
          </div>
        )}

        {!autoInstall && <p className="about-desc" role="status">{update.installReason || t("linux.manualUpdate")}</p>}
        {autoInstall && phase === "info" && getPlatform() === "linux" && <p className="about-desc">{installHint}</p>}
        {phase === "download" && error && <p role="alert">{error}</p>}
        <footer>
          {!autoInstall ? (
            <>
              <button className="secondary-button" onClick={closeDialog}>{t("update.later")}</button>
              <button className="primary-button" onClick={() => void openRelease()}>{t("update.openRelease")}</button>
            </>
          ) : phase === "ready" ? (
            <>
              <button className="secondary-button" onClick={closeDialog}>{t("update.later")}</button>
              <button className="primary-button update-button" onClick={() => void startInstall()}><Rocket size={14} />{t(packageInstall ? "update.openInstaller" : "update.installReady")}</button>
            </>
          ) : phase === "download" ? (
            <button className="secondary-button" onClick={() => void cancelDownload()}><X size={14} />{t("cancel")}</button>
          ) : phase === "opened" ? (
            <button className="secondary-button" onClick={closeDialog}>{t("cancel")}</button>
          ) : phase === "cancel" ? (
            <span className="download-footer-hint">{t("update.cancelling")}</span>
          ) : phase === "launch" ? (
            <span className="download-footer-hint">{t(packageInstall ? "update.launching" : "update.restarting")}</span>
          ) : phase === "error" ? (
            <>
              <button className="secondary-button" onClick={closeDialog}>{t("update.later")}</button>
              <button className="primary-button update-button" onClick={() => void (installerPath ? startInstall() : startDownload())}><RefreshCw size={14} />{t("retry")}</button>
            </>
          ) : (
            <>
              <button className="secondary-button" onClick={closeDialog}>{t("update.later")}</button>
              <button className="primary-button update-button" onClick={() => void startDownload()}><Download size={14} />{t("update.installNow")}</button>
            </>
          )}
        </footer>
      </section>
    </div>
  );
}
