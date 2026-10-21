#!/bin/sh
# 打 macOS .app 包：Dock 名/访达名/图标走 bundle（CFBundleDisplayName +
# en/zh-Hans 本地化，英文 UEFI Player / 中文 UEFI播放器）。
# cargo run 是裸二进制，Dock 提示只能是可执行文件名（macOS 行为），
# 看不到图标和包装名；要正确 Dock 名 + 图标就双击这里产出的 .app。
# 注意：Dock 名是系统级的（跟系统语言走），不跟应用内中英切换联动；
# 窗口标题栏才跟应用内切换（UEFI Player / UEFI播放器，见 app.rs）。
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

APP="$HERE/target/bundle/UEFI Player.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" \
    "$APP/Contents/Resources/en.lproj" "$APP/Contents/Resources/zh-Hans.lproj"
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
    <string>UEFI Player</string>
    <key>CFBundleDisplayName</key>
    <string>UEFI Player</string>
    <key>LSHasLocalizedDisplayName</key>
    <true/>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

# 本地化 Dock/访达名：英文 UEFI Player，简中 UEFI播放器（跟系统语言走）。
cat > "$APP/Contents/Resources/en.lproj/InfoPlist.strings" <<'STRINGS'
"CFBundleName" = "UEFI Player";
"CFBundleDisplayName" = "UEFI Player";
STRINGS
cat > "$APP/Contents/Resources/zh-Hans.lproj/InfoPlist.strings" <<'STRINGS'
"CFBundleName" = "UEFI播放器";
"CFBundleDisplayName" = "UEFI播放器";
STRINGS

echo "bundled: $APP"
