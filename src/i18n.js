import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import en from "./locales/en.js";
import { isDesktop } from "./platformDetection.js";

// 界面语言。中文原文就是键：t("今日") 在中文下原样返回，在英文下查 locales/en.js。
// 桌面端以后端为唯一权威（设置存在 app_setting，托盘与通知也由它构建）；浏览器
// 预览按 ?lang= / localStorage / navigator.language 的顺序决定。规则见
// docs/ARCHITECTURE.md 的「界面语言」一节。

export const LANGUAGE_SETTINGS = ["auto", "zh", "en"];
export const LANGUAGE_CHANGED_EVENT = "metrik://ui-language";
const BROWSER_SETTING_KEY = "metrik:language";

let state = { setting: "auto", language: "zh" };
const listeners = new Set();
const warnedMissing = new Set();
// 与 i18n.test.js 的守卫同一个范围：CJK 标点、汉字、全角符号。
const CJK = /[　-〿㐀-䶿一-鿿豈-﫿＀-￯]/;

function normalizeSetting(value) {
  return LANGUAGE_SETTINGS.includes(value) ? value : "auto";
}

/** 主语言子标签为 zh 时用中文；其余（含未知、C、POSIX、空）一律英文。 */
export function localeIsChinese(locale) {
  if (typeof locale !== "string") return false;
  const primary = locale.trim().split(/[-_.@]/)[0];
  return primary.toLowerCase() === "zh";
}

export function resolveLanguage(setting, systemLocale) {
  if (setting === "zh" || setting === "en") return setting;
  return localeIsChinese(systemLocale) ? "zh" : "en";
}

export function getLanguage() {
  return state.language;
}

export function getLanguageSetting() {
  return state.setting;
}

/** 数字与日期格式化用的 BCP 47 标签，跟随生效语言。 */
export function localeTag(language = state.language) {
  return language === "en" ? "en-US" : "zh-CN";
}

function applyState(next) {
  const setting = normalizeSetting(next?.setting);
  const language = next?.language === "en" || next?.language === "zh"
    ? next.language
    : resolveLanguage(setting, globalThis.navigator?.language);
  if (setting === state.setting && language === state.language) return;
  state = { setting, language };
  if (typeof document !== "undefined") document.documentElement.lang = localeTag(language);
  for (const listener of listeners) listener();
}

function subscribe(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function snapshot() {
  return state;
}

/** 组件里读取 { setting, language }；语言变化时触发重渲染。 */
export function useLanguageState() {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

export function useLanguage() {
  return useLanguageState().language;
}

function interpolate(text, params) {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name) =>
    Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : match);
}

/**
 * 翻译一段界面文字。key 是中文原文；占位符写成 {name}，由 params 填充。
 * 英文条目可以是函数 (params) => string，用于复数或语序变化。
 * 缺英文条目时退回原文；键含中文时开发模式下告警一次。
 */
export function t(key, params) {
  if (state.language === "en") {
    const entry = Object.prototype.hasOwnProperty.call(en, key) ? en[key] : undefined;
    if (typeof entry === "function") return entry(params || {});
    if (typeof entry === "string") return interpolate(entry, params);
    // 只为中文键告警：不含中文的文字（Agent 名、数字等）本来就不需要条目。
    if (import.meta.env?.DEV && CJK.test(key) && !warnedMissing.has(key)) {
      warnedMissing.add(key);
      console.warn(`[i18n] missing English entry: ${key}`);
    }
  }
  return interpolate(key, params);
}

export function formatNumber(value, options) {
  return Number(value).toLocaleString(localeTag(), options);
}

export function formatDateTime(value, options) {
  return new Date(value).toLocaleString(localeTag(), options);
}

export function formatDate(value, options) {
  return new Date(value).toLocaleDateString(localeTag(), options);
}

export function formatTime(value, options) {
  return new Date(value).toLocaleTimeString(localeTag(), options);
}

function readBrowserSetting() {
  try {
    return normalizeSetting(localStorage.getItem(BROWSER_SETTING_KEY));
  } catch {
    return "auto";
  }
}

function browserState() {
  const forced = new URLSearchParams(globalThis.location?.search || "").get("lang");
  if (forced === "zh" || forced === "en") return { setting: forced, language: forced };
  const setting = readBrowserSetting();
  return { setting, language: resolveLanguage(setting, globalThis.navigator?.language) };
}

/**
 * 首帧渲染前调用一次（main.jsx）。桌面端向后端取生效语言并订阅变更事件，
 * 所有窗口因此同时切换；从不 reject，失败时按浏览器规则兜底。
 */
export async function initLanguage() {
  if (!isDesktop()) {
    applyState(browserState());
    return;
  }
  try {
    applyState(await invoke("ui_language"));
  } catch (error) {
    console.warn("ui_language failed:", error);
    applyState(browserState());
  }
  try {
    const { listen } = await import("@tauri-apps/api/event");
    await listen(LANGUAGE_CHANGED_EVENT, (event) => applyState(event.payload));
    // 首次读取与监听建立之间发生的切换没有事件送达；监听建立后再读一次，
    // 窗口加载期间切换语言也不会停在旧语言。
    applyState(await invoke("ui_language"));
  } catch (error) {
    console.warn("language change listener failed:", error);
  }
}

/** 设置页修改语言。桌面端由后端保存并广播给全部窗口和托盘。 */
export async function setLanguageSetting(setting) {
  const value = normalizeSetting(setting);
  if (isDesktop()) {
    applyState(await invoke("set_ui_language", { setting: value }));
    return;
  }
  try {
    localStorage.setItem(BROWSER_SETTING_KEY, value);
  } catch {
    // 浏览器禁用存储时只在本页生效。
  }
  applyState({ setting: value, language: resolveLanguage(value, globalThis.navigator?.language) });
}
