import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const tauriConfig = JSON.parse(fs.readFileSync("src-tauri/tauri.conf.json", "utf8"));
const widgetInfo = fs.readFileSync("WidgetExtension/Info.plist", "utf8");
const widgetViews = fs.readFileSync(
  "WidgetExtension/Sources/MetrikWidgetViews.swift",
  "utf8",
);
const widgetModels = fs.readFileSync(
  "WidgetExtension/Sources/MetrikWidgetModels.swift",
  "utf8",
);
const reloaderInfo = fs.readFileSync(
  "WidgetExtension/MetrikWidgetReloader.Info.plist",
  "utf8",
);
const reloaderSource = fs.readFileSync(
  "WidgetExtension/Sources/MetrikWidgetReloader.swift",
  "utf8",
);
const publisherInfo = fs.readFileSync(
  "WidgetExtension/MetrikWidgetPublisher.Info.plist",
  "utf8",
);
const widgetBuildScript = fs.readFileSync(
  "scripts/build-macos-widget-extension.sh",
  "utf8",
);
const widgetPreviewScript = fs.readFileSync(
  "scripts/build-macos-widget-preview.sh",
  "utf8",
);

/// 提取 `for asset in … do` 列表并校验：名字必须是纯文件名、不重复、
/// 且真实存在于 src/assets。这能抓住 v0.17.0 那种事故——脚本里混入了字面量
/// `\n`，bash -n 语法上过、CI 只编译 macOS 也只在发版时才爆（ditto 找不到
/// src/assets/n）。资产清单从此属于单测管辖。
function assetLoopEntries(rawScript) {
  // 检出于 Windows 时是 CRLF，先归一化；匹配与拆分都按 LF 处理。
  const script = rawScript.replace(/\r\n/g, "\n");
  const loop = script.match(/for asset in \\([\s\S]*?)^do$/m);
  assert.ok(loop, "脚本里应有 for asset 循环");
  return loop[1]
    .split(/\\\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

test("widget 脚本的图标清单是真实存在的纯文件名且无重复", () => {
  for (const [name, script] of [
    ["extension", widgetBuildScript],
    ["preview", widgetPreviewScript],
  ]) {
    const assets = assetLoopEntries(script);
    assert.equal(assets.length, 17, `${name} 清单应覆盖全部 17 个 Agent 图标`);
    for (const asset of assets) {
      assert.match(
        asset,
        /^[\w.-]+\.(png|jpg)$/,
        `${name}: ${asset} 不是纯文件名（混入转义/空白？）`,
      );
      assert.ok(
        fs.existsSync(`src/assets/${asset}`),
        `${name}: src/assets/${asset} 不存在`,
      );
    }
    assert.equal(
      new Set(assets).size,
      assets.length,
      `${name} 清单里有重复项`,
    );
  }
});

test("Swift 的 asset 映射覆盖全部 Agent 且都被打包脚本覆盖", () => {
  const shipped = new Set(assetLoopEntries(widgetBuildScript));
  const cases = [
    ...widgetModels.matchAll(/case "(\w+)": \("([\w.-]+)", "(png|jpg)"\)/g),
  ];
  const mappedAgents = new Set(cases.map(([, id]) => id));
  const domainSource = fs.readFileSync("src-tauri/src/domain.rs", "utf8");
  const agentListBlock = domainSource.match(/pub const AGENT_IDS[\s\S]*?=\s*\[([\s\S]*?)\];/);
  assert.ok(agentListBlock, "应在 domain.rs 中找到 AGENT_IDS");
  const allAgents = [...agentListBlock[1].matchAll(/"(\w+)"/g)].map(([, id]) => id);
  assert.equal(allAgents.length, 17);
  for (const agentId of allAgents) {
    assert.ok(
      mappedAgents.has(agentId),
      `MetrikWidgetModels.swift 缺少 Agent "${agentId}" 的图标映射`,
    );
  }

  const referenced = cases.map(([, , file, ext]) => `${file}.${ext}`);
  assert.equal(referenced.length, 17, "Swift 映射的 case 数量应覆盖全部 17 个 Agent");
  for (const asset of referenced) {
    assert.ok(
      shipped.has(asset),
      `MetrikWidgetModels 引用的 ${asset} 没进 widget 打包清单`,
    );
  }
});

test("macOS release embeds the native WidgetKit extension and helpers", () => {
  assert.equal(
    tauriConfig.build.beforeBundleCommand,
    "node scripts/prepare-macos-widget.mjs",
  );
  assert.deepEqual(tauriConfig.bundle.macOS.files, {
    "PlugIns/MetrikWidget.appex":
      "../.build/macos-widget-extension/MetrikWidget.appex",
    "Helpers/metrik-widget-publish":
      "../.build/macos-widget-extension/Helpers/metrik-widget-publish",
    "Helpers/metrik-widget-reload":
      "../.build/macos-widget-extension/Helpers/metrik-widget-reload",
  });
});

test("production widget uses the host app bundle namespace", () => {
  assert.match(widgetInfo, /<string>app\.metrik\.desktop\.widget<\/string>/);
  assert.doesNotMatch(widgetInfo, /widget-preview/);
  assert.match(publisherInfo, /<string>app\.metrik\.desktop\.widget<\/string>/);
  assert.doesNotMatch(
    publisherInfo,
    /<key>CFBundleIdentifier<\/key>\s*<string>[^<]*widget-publish/,
  );
});

test("production widget never substitutes gallery preview data", () => {
  assert.doesNotMatch(
    widgetViews,
    /MetrikWidgetStore\.load\(\)\s*\?\?\s*MetrikWidgetStore\.preview/,
  );
  assert.match(
    widgetViews,
    /MetrikWidgetStore\.load\(\)\s*\?\?\s*MetrikWidgetStore\.unavailable/,
  );
  assert.doesNotMatch(widgetModels, /forResource:\s*"preview-widget-snapshot"/);
  assert.doesNotMatch(widgetModels, /Data\(contentsOf:/);
  assert.match(widgetModels, /UserDefaults\.standard/);
  assert.match(widgetModels, /Shared snapshot decoded with/);
});

test("timeline reloader identifies as the containing Metrik app", () => {
  assert.match(reloaderInfo, /<string>app\.metrik\.desktop<\/string>/);
  assert.match(reloaderSource, /RunLoop\.current\.run/);
});

test("production widget uses the containing app build version", () => {
  assert.match(widgetBuildScript, /APP_BUILD_NUMBER="\$APP_VERSION"/);
  assert.doesNotMatch(
    widgetBuildScript,
    /APP_BUILD_NUMBER=.*(?:date|GITHUB_RUN_NUMBER)/,
  );
});
