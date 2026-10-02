const NATIVE_PLATFORMS = new Set(["macos", "windows", "linux"]);

/** 运行在 Tauri 桌面壳内（而不是浏览器预览）。 */
export function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

/**
 * 桌面包的编译期平台是唯一可信来源。user-agent 只服务浏览器预览，不能覆盖
 * 原生平台，否则 macOS/Windows 的窗口形态会互相串线。
 */
export function detectRuntimePlatform(nativePlatform, userAgent = "") {
  if (NATIVE_PLATFORMS.has(nativePlatform)) return nativePlatform;
  if (/Windows/i.test(userAgent)) return "windows";
  if (/(Macintosh|MacIntel|MacPPC|Mac OS X)/i.test(userAgent)) return "macos";
  if (/Linux/i.test(userAgent)) return "linux";
  return "unknown";
}
