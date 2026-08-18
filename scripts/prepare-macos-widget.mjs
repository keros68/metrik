import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

// Tauri runs beforeBundleCommand on every release platform. WidgetKit only
// exists on macOS, so Windows and Linux intentionally have nothing to prepare.
if (process.platform === "darwin") {
  const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
  execFileSync("/bin/zsh", [path.join(scriptDirectory, "build-macos-widget-extension.sh")], {
    stdio: "inherit",
  });
}
