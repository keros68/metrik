import React from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/geist";
import "@fontsource/instrument-serif/400.css";
import "@fontsource/newsreader/400.css";
import "uplot/dist/uPlot.min.css";
import { App } from "./App.jsx";
import { initLanguage, useLanguageState } from "./i18n.js";
import { isDesktop } from "./platformDetection.js";
import "./styles.css";

document.documentElement.dataset.runtime = isDesktop() ? "desktop" : "browser";

// 语言变化时从根部重渲染整棵树（不重新挂载，窗口与表单状态保留）。
function Root() {
  useLanguageState();
  return <App />;
}

// 首帧之前先取到生效语言，任何窗口都不会先闪一下另一种语言。
initLanguage().finally(() => {
  createRoot(document.getElementById("root")).render(
    <React.StrictMode>
      <Root />
    </React.StrictMode>,
  );
});
