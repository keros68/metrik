import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 版本号只有一个事实来源：package.json。硬编码在 UI 里迟早会和实际发布漂移。
const { version } = JSON.parse(readFileSync("./package.json", "utf8"));

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  optimizeDeps: {
    include: ["react", "react-dom/client"],
  },
  server: {
    host: "0.0.0.0",
    port: 1420,
    strictPort: true,
    allowedHosts: ["terminal.local"],
    warmup: {
      clientFiles: ["./src/main.jsx"],
    },
  },
  plugins: [react()],
});
