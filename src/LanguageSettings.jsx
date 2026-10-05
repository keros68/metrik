import { useState } from "react";
import { setLanguageSetting, t, useLanguageState } from "./i18n.js";

// 标签在渲染时经 t() 翻译；两种语言的名称始终用各自的语言显示，不翻译。
const LANGUAGE_OPTIONS = [
  { id: "auto", label: "跟随系统", translate: true },
  { id: "zh", label: "简体中文", translate: false }, // i18n-ignore: 语言名用其本身的语言显示
  { id: "en", label: "English", translate: false },
];

export function LanguageCard() {
  const { setting } = useLanguageState();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const choose = async (next) => {
    if (next === setting) return;
    setBusy(true);
    setError("");
    try {
      await setLanguageSetting(next);
    } catch {
      setError("语言设置保存失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>{t("语言")}</h2>
      <p className="settings-muted">{t("跟随系统时，系统语言为中文则显示中文，否则显示英文。")}</p>
      <div className="theme-toggle" role="group" aria-label={t("界面语言")}>
        {LANGUAGE_OPTIONS.map((option) => (
          <button
            key={option.id}
            type="button"
            className={setting === option.id ? "is-selected" : ""}
            aria-pressed={setting === option.id}
            disabled={busy}
            onClick={() => choose(option.id)}
          >
            {option.translate ? t(option.label) : option.label}
          </button>
        ))}
      </div>
      {error && <p className="settings-feedback settings-feedback--error" role="alert">{t(error)}</p>}
    </div>
  );
}
