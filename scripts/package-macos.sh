#!/bin/sh
set -eu

PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DIST_DIR="$PROJECT_ROOT/dist"
APP_DIR="$DIST_DIR/Codex Usage Monitor.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ICON_WORK_DIR="${TMPDIR:-/tmp}/codex-usage-monitor-icon-$$"

cd "$PROJECT_ROOT"
cargo build --release

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR" "$ICON_WORK_DIR/icon.iconset"
trap 'rm -rf "$ICON_WORK_DIR"' EXIT HUP INT TERM

cp "$PROJECT_ROOT/target/release/codex-usage-monitor" "$MACOS_DIR/codex-usage-monitor"
chmod 755 "$MACOS_DIR/codex-usage-monitor"

qlmanage -t -s 1024 -o "$ICON_WORK_DIR" "$PROJECT_ROOT/assets/icon.svg" >/dev/null 2>&1
ICON_SOURCE="$ICON_WORK_DIR/icon.svg.png"
if [ ! -f "$ICON_SOURCE" ]; then
  echo "Unable to render assets/icon.svg with qlmanage" >&2
  exit 1
fi

for spec in \
  "16 icon_16x16.png" \
  "32 icon_16x16@2x.png" \
  "32 icon_32x32.png" \
  "64 icon_32x32@2x.png" \
  "128 icon_128x128.png" \
  "256 icon_128x128@2x.png" \
  "256 icon_256x256.png" \
  "512 icon_256x256@2x.png" \
  "512 icon_512x512.png" \
  "1024 icon_512x512@2x.png"
do
  set -- $spec
  sips -z "$1" "$1" "$ICON_SOURCE" --out "$ICON_WORK_DIR/icon.iconset/$2" >/dev/null
done
iconutil -c icns "$ICON_WORK_DIR/icon.iconset" -o "$RESOURCES_DIR/AppIcon.icns"

sed \
  -e "s/@VERSION@/$(sed -n 's/^version = \"\\([^\"]*\\)\"/\\1/p' Cargo.toml | head -1)/" \
  "$PROJECT_ROOT/scripts/Info.plist.in" > "$CONTENTS_DIR/Info.plist"

printf 'APPL????' > "$CONTENTS_DIR/PkgInfo"
codesign --verify --deep "$APP_DIR" >/dev/null 2>&1 || true

rm -f "$DIST_DIR/Codex-Usage-Monitor-macOS.zip"
ditto -c -k --sequesterRsrc --keepParent \
  "$APP_DIR" "$DIST_DIR/Codex-Usage-Monitor-macOS.zip"

echo "Created $APP_DIR"
echo "Created $DIST_DIR/Codex-Usage-Monitor-macOS.zip"
