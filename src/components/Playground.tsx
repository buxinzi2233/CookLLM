import { Bot, Globe, RefreshCw, Scan } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { ServerStatus } from "../types";
import { openExternal, writeClipboard } from "../tauri";
import { dispatchWebUiMessage } from "../platform";
import { cn } from "../utils";

export default function Playground({
  visible,
  status,
  webUiUrl,
  modelName,
  zenMode,
  onToggleZenMode,
}: {
  visible: boolean;
  status: ServerStatus;
  webUiUrl: string;
  modelName?: string;
  onOpenWebUi: () => void;
  zenMode?: boolean;
  onToggleZenMode?: () => void;
}) {
  const { t } = useI18n();
  /** 刷新内嵌 WebUI：key 变化时重建 iframe（服务重启后旧页面状态失效时用） */
  const [frameKey, setFrameKey] = useState(0);

  const [frameState, setFrameState] = useState<"loading" | "loaded" | "slow" | "failed">("loading");
  const [bridgeError, setBridgeError] = useState("");
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  useEffect(() => {
    setBridgeError("");
    setFrameState("loading");
    if (!status.running) return;
    const timer = window.setTimeout(() => setFrameState((state) => state === "loading" ? "slow" : state), 15000);
    return () => window.clearTimeout(timer);
  }, [webUiUrl, frameKey, status.running]);

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      if (!status.running) return;
      void dispatchWebUiMessage(event, frameRef.current?.contentWindow, webUiUrl, {
        copy: writeClipboard,
        open: openExternal,
        toggleZen: () => onToggleZenMode?.(),
      }).then((action) => {
        if (action) setBridgeError("");
      }).catch((error) => {
        const detail = error instanceof Error ? error.message : String(error);
        setBridgeError(t("pg.bridgeError", { error: detail === "http-url-required" ? t("pg.httpOnly") : detail }));
      });
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [onToggleZenMode, webUiUrl, status.running, t]);

  return (
    <div className="playground-pane" hidden={!visible}>
      {/* 高度相对面板容器（而非视口）：填满 Dock 下全部剩余高度，Dock 展开时随 flex 布局自动缩小 */}
      <section className="playground-layout">
        <div className="chat-panel">
          <div className="chat-header">
            <div><Bot size={18} /><span>{modelName || "Local model"}</span></div>
            <div className="playground-actions">
              <button
                type="button"
                className="chat-action-btn"
                title={status.running ? t("refreshWebUi") : t("waitingService")}
                aria-label={t("refreshWebUi")}
                disabled={!status.running}
                onClick={() => setFrameKey((key) => key + 1)}
              >
                <RefreshCw size={15} />
              </button>
              {onToggleZenMode && (
                <button
                  type="button"
                  className={cn("chat-action-btn", zenMode && "active")}
                  onClick={onToggleZenMode}
                  title={zenMode ? t("zen.exit") : t("zen.enter")}
                  aria-label={zenMode ? t("zen.exit") : t("zen.enter")}
                >
                  <Scan size={15} />
                </button>
              )}
              <button
                type="button"
                className="chat-action-btn"
                disabled={!status.running}
                onClick={() => { setBridgeError(""); void openExternal(webUiUrl).catch((error) => setBridgeError(t("pg.bridgeError", { error: String(error) }))); }}
                title={t("openInBrowser")}
                aria-label={t("openInBrowser")}
              >
                <Globe size={15} />
              </button>
            </div>
          </div>
          {bridgeError && <p className="import-error" role="alert">{bridgeError}</p>}
          {status.running && <p role="status" style={{ margin: "4px 12px", fontSize: 12 }}>{t(`pg.${frameState}`)}</p>}
          {status.running
            ? (
              <div className="native-frame-wrap">
                <iframe ref={frameRef} key={`${webUiUrl}:${frameKey}`} src={webUiUrl} onLoad={() => setFrameState("loaded")} onError={() => setFrameState("failed")} title="llama.cpp Web UI" allow="clipboard-read; clipboard-write" />
              </div>
            )
            : <div className="messages-empty"><Globe size={30} /><p>{t("pgNotRunning")}</p></div>}
        </div>
      </section>
    </div>
  );
}
