import React from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/geist";
import "@fontsource/instrument-serif/400.css";
import "@fontsource/newsreader/400.css";
import "uplot/dist/uPlot.min.css";
import { App } from "./App.jsx";
import "./styles.css";

document.documentElement.dataset.runtime = window.__TAURI_INTERNALS__ ? "desktop" : "browser";

createRoot(document.getElementById("root")).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
