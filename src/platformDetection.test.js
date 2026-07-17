import assert from "node:assert/strict";
import test from "node:test";

import { detectRuntimePlatform } from "./platformDetection.js";

test("compiled macOS platform wins over a Windows-like webview user-agent", () => {
  assert.equal(detectRuntimePlatform("macos", "Mozilla/5.0 (Windows NT 10.0)"), "macos");
});

test("compiled Windows platform wins over a Mac-like webview user-agent", () => {
  assert.equal(detectRuntimePlatform("windows", "Mozilla/5.0 (Macintosh)"), "windows");
});

test("browser previews fall back to a narrowly matched user-agent", () => {
  assert.equal(detectRuntimePlatform(null, "Mozilla/5.0 (Macintosh; Intel Mac OS X)"), "macos");
  assert.equal(detectRuntimePlatform(null, "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"), "windows");
  assert.equal(detectRuntimePlatform(null, "Mozilla/5.0 (X11; Linux x86_64)"), "linux");
  assert.equal(detectRuntimePlatform(null, "preview"), "unknown");
});
