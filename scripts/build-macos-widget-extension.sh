#!/bin/zsh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  exit 0
fi

SCRIPT_DIR="${0:A:h}"
PROJECT_DIR="${SCRIPT_DIR:h}"
BUILD_DIR="$PROJECT_DIR/.build/macos-widget-extension"
APPEX_BUNDLE="$BUILD_DIR/MetrikWidget.appex"
APPEX_BINARY="$APPEX_BUNDLE/Contents/MacOS/MetrikWidget"
APPEX_RESOURCES="$APPEX_BUNDLE/Contents/Resources"
HELPERS_DIR="$BUILD_DIR/Helpers"
SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"
APP_VERSION="$(node -p "require('$PROJECT_DIR/package.json').version")"
# WidgetKit archives the extension's bundle stub into every timeline and validates it
# against LaunchServices before accepting a reload. The extension therefore has to use
# the containing app's build version; giving only the extension a per-build timestamp
# leaves upgraded installations with incompatible cached bundle stubs. Release versions
# are already unique and serialized by CI, while the isolated preview app uses its own
# bundle identifier and build sequence for local visual iteration.
APP_BUILD_NUMBER="$APP_VERSION"

# A universal Tauri release needs a universal extension too. Local arm64/x86_64
# builds may override this to one architecture to keep iteration fast.
if [[ -n "${METRIK_WIDGET_ARCHS:-}" ]]; then
  ARCHS=(${=METRIK_WIDGET_ARCHS})
elif [[ "${TAURI_ENV_ARCH:-}" == "universal" ]]; then
  ARCHS=(arm64 x86_64)
else
  ARCHS=("$(uname -m)")
fi

rm -rf "$BUILD_DIR"
mkdir -p "$APPEX_BUNDLE/Contents/MacOS" "$APPEX_RESOURCES" "$HELPERS_DIR" "$BUILD_DIR/arch"

build_universal() {
  local output="$1"
  shift
  local name="${output:t}"
  local binaries=()
  local arch
  for arch in "${ARCHS[@]}"; do
    local arch_output="$BUILD_DIR/arch/$name-$arch"
    xcrun swiftc \
      -parse-as-library \
      -O \
      -target "${arch}-apple-macosx14.0" \
      -sdk "$SDK_PATH" \
      "$@" \
      -o "$arch_output"
    binaries+=("$arch_output")
  done
  if (( ${#binaries[@]} == 1 )); then
    ditto "${binaries[1]}" "$output"
  else
    xcrun lipo -create "${binaries[@]}" -output "$output"
  fi
  chmod +x "$output"
}

echo "Building Metrik WidgetKit extension for ${ARCHS[*]}..."
build_universal "$APPEX_BINARY" \
  -application-extension \
  -framework AppKit \
  -framework SwiftUI \
  -framework WidgetKit \
  -Xlinker -e \
  -Xlinker _NSExtensionMain \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetModels.swift" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetViews.swift" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetBundle.swift"

ditto "$PROJECT_DIR/WidgetExtension/Info.plist" "$APPEX_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $APP_VERSION" "$APPEX_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $APP_BUILD_NUMBER" "$APPEX_BUNDLE/Contents/Info.plist"

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

build_universal "$HELPERS_DIR/metrik-widget-publish" \
  -framework Foundation \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$PROJECT_DIR/WidgetExtension/MetrikWidgetPublisher.Info.plist" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetPublisher.swift"
build_universal "$HELPERS_DIR/metrik-widget-reload" \
  -framework WidgetKit \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$PROJECT_DIR/WidgetExtension/MetrikWidgetReloader.Info.plist" \
  "$PROJECT_DIR/WidgetExtension/Sources/MetrikWidgetReloader.swift"

# This is ad-hoc bundle integrity, not Developer ID distribution signing. The
# nested extension must be sealed before Tauri seals the outer app bundle.
# Both snapshot processes are sandboxed and share the extension bundle identifier, so
# UserDefaults/cfprefsd resolves to the Widget's standard container. This works for ad-hoc
# builds without a TeamIdentifier; App Group access does not on macOS 26.
# A sandboxed standalone executable additionally needs an embedded Info.plist
# (the publisher is built with -sectcreate __TEXT __info_plist above): without a
# bundle identifier, libsecinit crashes with SIGTRAP before main() ever runs.
codesign --force --sign - \
  --entitlements "$PROJECT_DIR/WidgetExtension/MetrikWidget.entitlements" \
  "$APPEX_BUNDLE"
codesign --force --sign - \
  --entitlements "$PROJECT_DIR/WidgetExtension/MetrikWidgetPublisher.entitlements" \
  "$HELPERS_DIR/metrik-widget-publish"
codesign --force --sign - "$HELPERS_DIR/metrik-widget-reload"
codesign --verify --strict --verbose=2 "$APPEX_BUNDLE"

echo "Prepared WidgetKit bundle: $APPEX_BUNDLE"
