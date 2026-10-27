#!/usr/bin/env bash
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$HERE/AppIcon

if [[ ${DEVELOPER_DIR:-} == /nix/store/* ]]; then
  unset DEVELOPER_DIR SDKROOT
fi
DEVELOPER_DIR=${DEVELOPER_DIR:-$(/usr/bin/xcode-select -p)}
export DEVELOPER_DIR
ACTOOL=$(/usr/bin/xcrun --find actool)

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

"$ACTOOL" "$HERE/AppIcon.icon" \
  --compile "$WORK" \
  --app-icon AppIcon \
  --platform macosx \
  --target-device mac \
  --minimum-deployment-target 14.0 \
  --output-partial-info-plist "$WORK/partial.plist" \
  --output-format human-readable-text \
  --notices --warnings --errors

for f in Assets.car AppIcon.icns; do
  [ -f "$WORK/$f" ] || {
    echo "error: actool produced no $f, Xcode 26 or later is needed ($DEVELOPER_DIR)" >&2
    exit 1
  }
done

mkdir -p "$OUT"
cp "$WORK/Assets.car" "$WORK/AppIcon.icns" "$OUT/"
/usr/bin/sips -s format png "$OUT/AppIcon.icns" --out "$HERE/../docs/icon.png" >/dev/null
echo "Info.plist keys actool asks for:"
cat "$WORK/partial.plist"
