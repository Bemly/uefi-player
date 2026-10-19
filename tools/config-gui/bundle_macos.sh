#!/bin/sh
# 打 macOS .app 包：Dock/访达图标走 bundle 里的 icon.icns。
# cargo run 是裸二进制，看不到图标；要图标就双击这里产出的 .app。
# 用法：./bundle_macos.sh [debug|release]   （默认 debug）
set -eu

MODE="${1:-debug}"
HERE="$(cd "$(dirname "$0")" && pwd)"
HOST="$(rustc -vV | sed -n 's/^host: //p')"

CARGO_ARGS="--manifest-path \"$HERE/Cargo.toml\" --target \"$HOST\""
if [ "$MODE" = "release" ]; then
    # shellcheck disable=SC2086
    eval cargo build $CARGO_ARGS --release
    BIN="$HERE/target/$HOST/release/player-config-gui"
else
    # shellcheck disable=SC2086
    eval cargo build $CARGO_ARGS
    BIN="$HERE/target/$HOST/debug/player-config-gui"
fi

APP="$HERE/target/bundle/player-config-gui.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/player-config-gui"
cp "$HERE/assets/icon.icns" "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>player-config-gui</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundleIdentifier</key>
    <string>dev.bemly.player-config-gui</string>
    <key>CFBundleName</key>
    <string>player.toml editor</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

echo "bundled: $APP"
