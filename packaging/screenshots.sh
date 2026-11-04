DOMAIN=net.karstx.usbip
PROCESS=usbip-macos
APP=${1:?usage: screenshots <usbip.app> <output dir>}
OUT=${2:?usage: screenshots <usbip.app> <output dir>}
# A server already serving the screenshot devices; without one, the script serves them itself.
SERVER=${SERVER:-}

die() {
  echo "error: $*" >&2
  exit 1
}

[ -d "$APP" ] || die "$APP is not an app bundle"
if pgrep -x "$PROCESS" >/dev/null; then
  die "usbip is running, quit it first"
fi
if [ -z "$SERVER" ] && /usr/sbin/lsof -iTCP:3240 -sTCP:LISTEN >/dev/null; then
  die "port 3240 is in use"
fi

mkdir -p "$OUT"
WORK=$(mktemp -d)
SERVER_PID=
APP_PID=

quit_app() {
  [ -n "$APP_PID" ] || return 0
  kill -TERM "$APP_PID" 2>/dev/null || true
  while kill -0 "$APP_PID" 2>/dev/null; do sleep 0.5; done
  APP_PID=
}

restore() {
  quit_app
  dark_mode "$DARK_BEFORE"
  [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
  defaults delete "$DOMAIN" >/dev/null 2>&1 || true
  defaults import "$DOMAIN" "$WORK/prefs.plist"
  rm -rf "$WORK"
}

dark_mode() {
  osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $1"
}

DARK_BEFORE=$(osascript -e 'tell application "System Events" to tell appearance preferences to get dark mode')
defaults export "$DOMAIN" "$WORK/prefs.plist"
trap restore EXIT

if [ -z "$SERVER" ]; then
  SERVER=localhost
  "$SCREENSHOT_SERVER" 127.0.0.1:3240 2>"$WORK/server.log" &
  SERVER_PID=$!
  for _ in $(seq 50); do
    /usr/sbin/lsof -iTCP:3240 -sTCP:LISTEN >/dev/null && break
    sleep 0.1
  done
fi

ui() {
  osascript - "$@" <<EOF
on run argv
  tell application "System Events" to tell process "$PROCESS"
    $(cat)
  end tell
end run
EOF
}

# The window ids screencapture needs come only from the window server, which AppleScript can't reach.
window_id() {
  osascript -l JavaScript - "$APP_PID" <<'EOF'
ObjC.import("CoreGraphics");
function run([pid]) {
  const windows = ObjC.deepUnwrap(ObjC.castRefToObject(
    $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, $.kCGNullWindowID)));
  const w = windows.find(w => w.kCGWindowOwnerPID == pid && w.kCGWindowLayer == 0);
  return w ? w.kCGWindowNumber : "";
}
EOF
}

capture_window() {
  local id=
  for _ in $(seq 20); do
    id=$(window_id)
    [ -n "$id" ] && break
    sleep 0.25
  done
  [ -n "$id" ] || die "no window to capture for $1"
  screencapture -x -l "$id" "$OUT/$1.png"
  echo "$OUT/$1.png"
}

for suffix in light dark; do
  dark_mode "$([ "$suffix" = dark ] && echo true || echo false)"

  defaults delete "$DOMAIN" >/dev/null 2>&1 || true
  defaults write "$DOMAIN" servers -array "$SERVER"
  defaults write "$DOMAIN" attached -array "1-1@$SERVER"
  defaults write "$DOMAIN" names -array \
    "1209:7360=Flash Drive" "1209:7361=Keyboard" "1209:7362=Serial Adapter" "1209:7363=Dev Board"

  open -n "$APP"
  for _ in $(seq 100); do
    APP_PID=$(pgrep -nx "$PROCESS" || true)
    [ -n "$APP_PID" ] && [ -d "/Volumes/FLASH DRIVE" ] && break
    sleep 0.25
  done
  [ -d "/Volumes/FLASH DRIVE" ] || die "the flash drive did not mount"
  sleep 1

  # The menu is drawn outside the app's process, so it has no window of the app's to capture.
  bounds=$(
    ui <<'EOF'
set m to menu bar item 1 of menu bar 2
click m
delay 0.5
set {x, y} to position of menu 1 of m
set {w, h} to size of menu 1 of m
return (x as text) & "," & y & "," & w & "," & h
EOF
  )
  screencapture -x -R "$bounds" "$OUT/menu-$suffix.png"
  echo "$OUT/menu-$suffix.png"
  ui >/dev/null <<'EOF'
set m to menu bar item 1 of menu bar 2
click menu item "Settings…" of menu 1 of m
repeat until exists window 1
  delay 0.2
end repeat
EOF

  for tab in Servers General Log; do
    ui "$tab" >/dev/null <<'EOF'
click radio button (item 1 of argv) of tab group 1 of window 1
EOF
    sleep 0.5
    capture_window "settings-$(echo "$tab" | tr '[:upper:]' '[:lower:]')-$suffix"
  done

  quit_app
done
