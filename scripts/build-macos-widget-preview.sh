#!/bin/zsh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "The native WidgetKit preview can only be built on macOS." >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  RUSTUP_ROOT="${RUSTUP_HOME:-${HOME}/.rustup}"
  CARGO_BINARY="$(find "$RUSTUP_ROOT/toolchains" -path '*/bin/cargo' -type f -print 2>/dev/null | sort | tail -n 1)"
  if [[ -z "$CARGO_BINARY" ]]; then
    echo "Rust is installed but cargo could not be located for the Tauri host build." >&2
    exit 1
  fi
  export PATH="${CARGO_BINARY:h}:$PATH"
fi

SCRIPT_DIR="${0:A:h}"
PROJECT_DIR="${SCRIPT_DIR:h}"
BUILD_DIR="$PROJECT_DIR/.build/macos-widget-preview"
APP_BUNDLE="$PROJECT_DIR/src-tauri/target/debug/bundle/macos/Metrik Widget Preview.app"
APPEX_NAME="MetrikWidget.appex"
APPEX_BUNDLE="$BUILD_DIR/$APPEX_NAME"
APPEX_BINARY="$APPEX_BUNDLE/Contents/MacOS/MetrikWidget"
APPEX_RESOURCES="$APPEX_BUNDLE/Contents/Resources"
INSTALLED_APP="/Applications/Metrik Widget Preview.app"
INSTALLED_APPEX="$INSTALLED_APP/Contents/PlugIns/$APPEX_NAME"
WIDGET_ID="app.metrik.desktop.widget-preview.widget"
PREVIEW_BUILD_SEQUENCE=$(( $(date +%s) % 255 + 1 ))
PREVIEW_BUILD_NUMBER="$(date +%Y).$(date +%m).$(date +%d)d$PREVIEW_BUILD_SEQUENCE"

SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"
ARCH="$(uname -m)"

rm -rf "$BUILD_DIR"
mkdir -p "$APPEX_BUNDLE/Contents/MacOS" "$APPEX_RESOURCES"

echo "Building the Metrik WidgetKit extension with the installed macOS SDK..."
xcrun swiftc \
  -parse-as-library \
  -application-extension \
  -O \
  -target "${ARCH}-apple-macosx14.0" \
  -sdk "$SDK_PATH" \
  -framework AppKit \
  -framework SwiftUI \
  -framework WidgetKit \
  -Xlinker -e \
  -Xlinker _NSExtensionMain \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetModels.swift" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetViews.swift" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetBundle.swift" \
  -o "$APPEX_BINARY"

ditto "$PROJECT_DIR/WidgetExtension/Info.plist" "$APPEX_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleVersion $PREVIEW_BUILD_NUMBER" \
  "$APPEX_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleIdentifier $WIDGET_ID" \
  "$APPEX_BUNDLE/Contents/Info.plist"
for asset in \
  chatgpt-app-icon.png \
  claude-app-icon.jpg \
  zcode-app-icon.png \
  opencode-app-icon.png \
  kimi-app-icon.png \
  antigravity-app-icon.png \
  workbuddy-app-icon.png \
  qoder-app-icon.png \
  deepseek-app-icon.png \
  dsh-app-icon.png \
  grok-app-icon.png \
  grokbot-app-icon.png \
  hermes-app-icon.png \
  pi-app-icon.png \
  qwen-app-icon.png \
  cursor-app-icon.png \
  minimax-app-icon.png
do
  ditto "$PROJECT_DIR/src/assets/$asset" "$APPEX_RESOURCES/$asset"
done

echo "Building the host-app timeline refresh helper..."
mkdir -p "$BUILD_DIR/Helpers"
xcrun swiftc \
  -parse-as-library \
  -O \
  -target "${ARCH}-apple-macosx14.0" \
  -sdk "$SDK_PATH" \
  -framework WidgetKit \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetReloader.swift" \
  -o "$BUILD_DIR/Helpers/metrik-widget-reload"

echo "Building the isolated Metrik preview host..."
cd "$PROJECT_DIR"
npx tauri build \
  --debug \
  --bundles app \
  --config src-tauri/tauri.widget-preview.conf.json

if [[ ! -d "$APP_BUNDLE" ]]; then
  echo "Expected preview host was not produced at $APP_BUNDLE" >&2
  exit 1
fi

mkdir -p "$APP_BUNDLE/Contents/PlugIns" "$APP_BUNDLE/Contents/Helpers"
rm -rf "$APP_BUNDLE/Contents/PlugIns/$APPEX_NAME"
ditto "$APPEX_BUNDLE" "$APP_BUNDLE/Contents/PlugIns/$APPEX_NAME"
ditto "$BUILD_DIR/Helpers/metrik-widget-reload" "$APP_BUNDLE/Contents/Helpers/metrik-widget-reload"

chmod +x \
  "$APP_BUNDLE/Contents/Helpers/metrik-widget-reload" \
  "$APPEX_BINARY"
xattr -cr "$APP_BUNDLE"

echo "Applying local ad-hoc signatures..."
codesign --force --sign - "$APP_BUNDLE/Contents/Helpers/metrik-widget-reload"
codesign --force --sign - \
  --entitlements "$PROJECT_DIR/WidgetExtension/MetrikWidget.preview.entitlements" \
  "$APP_BUNDLE/Contents/PlugIns/$APPEX_NAME/Contents/MacOS/MetrikWidget"
codesign --force --sign - \
  --entitlements "$PROJECT_DIR/WidgetExtension/MetrikWidget.preview.entitlements" \
  "$APP_BUNDLE/Contents/PlugIns/$APPEX_NAME"
codesign --force --sign - \
  --entitlements "$PROJECT_DIR/WidgetExtension/MetrikWidget.preview.entitlements" \
  "$APP_BUNDLE"

codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

echo "Installing the isolated preview app..."
if [[ -d "$INSTALLED_APP" ]]; then
  pluginkit -r "$INSTALLED_APPEX" || true
  rm -rf "$INSTALLED_APP"
fi
ditto "$APP_BUNDLE" "$INSTALLED_APP"

# Launch Services needs a brief moment to index the newly copied host before
# pluginkit can retain the extension registration reliably.
sleep 1
pluginkit -a "$INSTALLED_APPEX"
pluginkit -e use -i "$WIDGET_ID"

echo "Installed: $INSTALLED_APP"
echo "Widget extension: $INSTALLED_APPEX"
