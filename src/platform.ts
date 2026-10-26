/** 平台信息只来自运行环境，不根据待比较的路径猜测平台。 */
export type Platform = "windows" | "linux" | "macos" | "unknown";

interface PlatformNavigator {
  userAgentData?: { platform?: string };
  platform?: string;
  userAgent?: string;
}

export function getPlatform(
  environment: PlatformNavigator = typeof navigator === "undefined" ? {} : navigator,
): Platform {
  // Chromium 优先用 UA Client Hints；WebKitGTK 使用 navigator.platform。
  for (const value of [environment.userAgentData?.platform, environment.platform, environment.userAgent]) {
    if (!value) continue;
    if (/^(win32|win64|windows)(\b|_)/i.test(value) || /Windows NT/i.test(value)) return "windows";
    if (/linux/i.test(value)) return "linux";
    if (/mac/i.test(value)) return "macos";
  }
  return "unknown";
}

/** 仅用于身份比较；不改写原路径，也不折叠空格、分隔符或符号链接。 */
export function pathIdentity(path: string, platform: Platform = getPlatform()): string {
  return platform === "windows" ? path.toLowerCase() : path;
}

/** Linux 安装方式与权限由后端探测；这里仅检查能力与资产是否完整。 */
export interface AppUpdateCapability {
  installKind?: "appimage" | "deb" | "rpm" | "manual";
  installSupported?: boolean;
  installReason?: string;
  assetName?: string | null;
  assetUrl?: string | null;
  assetSha256?: string | null;
}

export function canAutoInstallApp(platform: Platform = getPlatform(), update?: AppUpdateCapability | null): boolean {
  if (platform === "windows") return true;
  if (platform !== "linux" || update?.installSupported !== true || !update.assetName || !update.assetUrl
    || !/^[a-f0-9]{64}$/i.test(update.assetSha256 ?? "")) return false;
  const kind = update.installKind;
  if (!kind || kind === "manual" || !update.assetName.toLowerCase().endsWith(`.${kind}`)) return false;
  try {
    const url = new URL(update.assetUrl);
    return (url.protocol === "https:" || url.protocol === "http:")
      && decodeURIComponent(url.pathname.split("/").pop() ?? "") === update.assetName;
  } catch { return false; }
}

/** 无 source 的旧 Windows 事件仍交给原订阅者。 */
export function acceptsDownloadProgress(payload: { source?: "app" | "engine" }, source: "app" | "engine"): boolean {
  return payload.source === undefined || payload.source === source;
}

export function isLinuxX64(
  environment: PlatformNavigator = typeof navigator === "undefined" ? {} : navigator,
): boolean {
  const arch = `${environment.platform ?? ""} ${environment.userAgent ?? ""}`;
  return getPlatform(environment) === "linux" && /\b(x86_64|amd64|x64)\b/i.test(arch)
    && !/\b(aarch64|arm64|armv\d+|i[3-6]86)\b/i.test(arch);
}

export function canAutoInstallEngine(
  platform: Platform = getPlatform(), backend?: string, linuxX64 = isLinuxX64(),
): boolean {
  return platform === "windows" || (platform === "linux" && linuxX64 && (backend === "cpu" || backend === "vulkan" || backend === "cuda"));
}

/** 保留用户后端偏好；能力门控不代表驱动或设备已经验证。 */
export function autoEngineBackend(
  preferred?: "cuda" | "vulkan" | "cpu", platform: Platform = getPlatform(),
): "cuda" | "vulkan" | "cpu" {
  return preferred ?? defaultEngineBackend(platform);
}

export function minimizeToTrayOnClose(value?: boolean, platform: Platform = getPlatform()): boolean {
  return value ?? platform !== "linux";
}

/** Linux 文件名可以包含首尾空格；Windows 保留原有输入修剪行为。 */
export function engineInputPath(path: string, platform: Platform = getPlatform()): string {
  return platform === "windows" ? path.trim() : path;
}

export function enginePathPlaceholder(platform: Platform = getPlatform()): string {
  return platform === "windows" ? "C:\\llama.cpp\\llama-server.exe" : "/path/to/llama.cpp/llama-server";
}

export function defaultEngineBackend(platform: Platform = getPlatform()): "cuda" | "cpu" {
  return platform === "windows" ? "cuda" : "cpu";
}

/** 只展示实际扩展名，不把未知包或 Linux 包当成 EXE。 */
export function installerPackageType(name?: string | null): string {
  const match = name?.match(/\.(appimage|deb|rpm|msi|exe)$/i)?.[1]?.toLowerCase();
  return match === "appimage" ? "AppImage" : match?.toUpperCase() || "—";
}

export function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

/** A navigated iframe must not retain access to the trusted WebUI bridge. */
export function isTrustedWebUiMessage(
  event: { source: unknown; origin: string }, frameWindow: unknown, webUiUrl: string,
): boolean {
  return !!frameWindow && event.source === frameWindow && isHttpUrl(webUiUrl)
    && event.origin === new URL(webUiUrl).origin;
}

export async function dispatchWebUiMessage(
  event: { source: unknown; origin: string; data: unknown },
  frameWindow: unknown,
  webUiUrl: string,
  actions: { copy: (text: string) => Promise<void>; open: (url: string) => Promise<void>; toggleZen: () => void },
): Promise<"copy" | "open" | "zen" | null> {
  if (!isTrustedWebUiMessage(event, frameWindow, webUiUrl)) return null;
  const data = event.data;
  if (!data || typeof data !== "object") return null;
  const message = data as { type?: unknown; text?: unknown; url?: unknown };
  if (message.type === "cookllm:copy" && typeof message.text === "string") {
    await actions.copy(message.text);
    return "copy";
  }
  if (message.type === "cookllm:open" && typeof message.url === "string") {
    if (!isHttpUrl(message.url)) throw new Error("http-url-required");
    await actions.open(message.url);
    return "open";
  }
  if (message.type === "cookllm:zen-toggle") {
    actions.toggleZen();
    return "zen";
  }
  return null;
}
