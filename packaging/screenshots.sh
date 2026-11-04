DOMAIN=net.karstx.usbip
PROCESS=usbip-macos
APP=${1:?usage: screenshots <usbip.app> <output dir>}
OUT=${2:?usage: screenshots <usbip.app> <output dir>}
# The name the menu shows for the server, which must resolve to this Mac.
SERVER=${SERVER:-localhost}
# Desktop pictures to switch to for each appearance, for a disposable screenshot machine.
WALLPAPER_LIGHT=${WALLPAPER_LIGHT:-}
WALLPAPER_DARK=${WALLPAPER_DARK:-}
DRIVE="/Volumes/FLASH DRIVE"

die() {
  echo "error: $*" >&2
  exit 1
}

[ -d "$APP" ] || die "$APP is not an app bundle"
if pgrep -x "$PROCESS" >/dev/null; then
  die "usbip is running, quit it first"
fi
if /usr/sbin/lsof -iTCP:3240 -sTCP:LISTEN >/dev/null; then
  die "port 3240 is in use"
fi

mkdir -p "$OUT"
WORK=$(mktemp -d)
SERVER_PID=
APP_PID=
HOLDER_PID=

start_server() {
  "$SCREENSHOT_SERVER" 127.0.0.1:3240 2>>"$WORK/server.log" &
  SERVER_PID=$!
  for _ in $(seq 50); do
    /usr/sbin/lsof -iTCP:3240 -sTCP:LISTEN >/dev/null && return
    sleep 0.1
  done
  die "the screenshot server did not start"
}

stop_server() {
  [ -n "$SERVER_PID" ] || return 0
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=
}

quit_app() {
  [ -n "$APP_PID" ] || return 0
  kill -TERM "$APP_PID" 2>/dev/null || true
  while kill -0 "$APP_PID" 2>/dev/null; do sleep 0.5; done
  APP_PID=
}

dark_mode() {
  osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $1"
}

restore() {
  [ -z "$HOLDER_PID" ] || kill "$HOLDER_PID" 2>/dev/null || true
  quit_app
  stop_server
  dark_mode "$DARK_BEFORE"
  defaults delete "$DOMAIN" >/dev/null 2>&1 || true
  defaults import "$DOMAIN" "$WORK/prefs.plist"
  rm -rf "$WORK"
}

DARK_BEFORE=$(osascript -e 'tell application "System Events" to tell appearance preferences to get dark mode')
defaults export "$DOMAIN" "$WORK/prefs.plist"
trap restore EXIT

ui() {
  osascript - "$@" <<EOF
on run argv
  tell application "System Events" to tell process "$PROCESS"
    $(cat)
  end tell
end run
EOF
}

# Starts the app with the given devices attached, as if they had been attached before it quit.
launch() {
  defaults delete "$DOMAIN" >/dev/null 2>&1 || true
  defaults write "$DOMAIN" servers -array "$SERVER"
  defaults write "$DOMAIN" attached -array "$@"
  defaults write "$DOMAIN" names -array \
    "0781:5581=SanDisk Ultra" "046d:c52b=USB Receiver" "10c4:ea60=Sonoff Zigbee 3.0 USB Dongle Plus" \
    "2e8a:000a=Pico" "1050:0407=YubiKey OTP+FIDO+CCID"
  open -n "$APP"
  for _ in $(seq 40); do
    APP_PID=$(pgrep -nx "$PROCESS" || true)
    [ -n "$APP_PID" ] && break
    sleep 0.25
  done
  [ -n "$APP_PID" ] || die "usbip did not start"
}

wait_for_menu_line() {
  for _ in $(seq 60); do
    ui "$1" 2>/dev/null <<'EOF' | grep -q true && return
set m to menu bar item 1 of menu bar 2
click m
delay 0.3
set found to exists (first menu item of menu 1 of m whose name starts with (item 1 of argv))
perform action "AXCancel" of menu 1 of m
return found
EOF
    sleep 0.5
  done
  die "the menu never showed $1"
}

# The menu bar from the status item to the right edge of the screen, and the open menu below it.
# The menu is drawn outside the app's process, so it has no window of the app's to capture.
capture_menu() {
  local bounds
  bounds=$(
    ui <<'EOF'
set m to menu bar item 1 of menu bar 2
click m
delay 0.5
set {itemX, itemY} to position of m
set {menuX, menuY} to position of menu 1 of m
set {menuW, menuH} to size of menu 1 of m
set {screenW, screenH} to size of menu bar 1
set leftX to menuX
if itemX < leftX then set leftX to itemX
set leftX to leftX - 160
return (leftX as text) & ",0," & (screenW - leftX) & "," & (menuY + menuH + 40)
EOF
  )
  screencapture -x -R "$bounds" "$OUT/$1.png"
  echo "$OUT/$1.png"
  ui >/dev/null <<'EOF'
perform action "AXCancel" of menu 1 of menu bar item 1 of menu bar 2
EOF
}

# The window ids screencapture needs come only from the window server, which AppleScript can't reach.
window_id() {
  osascript -l JavaScript - "$APP_PID" <<'EOF'
ObjC.import("CoreGraphics");
function run([pid]) {
  const windows = ObjC.deepUnwrap(ObjC.castRefToObject(
    $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, $.kCGNullWindowID)));
  const w = windows.find(w => w.kCGWindowOwnerPID == pid && w.kCGWindowLayer < 25);
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
  wallpaper=$([ "$suffix" = dark ] && echo "$WALLPAPER_DARK" || echo "$WALLPAPER_LIGHT")
  if [ -n "$wallpaper" ]; then
    osascript -e "tell application \"System Events\" to tell every desktop to set picture to \"$wallpaper\""
  fi
  start_server

  launch "1-1@$SERVER"
  for _ in $(seq 60); do [ -d "$DRIVE" ] && break; sleep 0.5; done
  [ -d "$DRIVE" ] || die "the flash drive did not mount"
  wait_for_menu_line "SanDisk Ultra"
  sleep 1
  capture_menu "menu-$suffix"

  # A file held open keeps the drive from ejecting, so detaching it is refused.
  echo "notes" >"$DRIVE/notes.txt"
  tail -f "$DRIVE/notes.txt" >/dev/null &
  HOLDER_PID=$!
  ui >/dev/null <<'EOF'
set m to menu bar item 1 of menu bar 2
click m
delay 0.3
click (first menu item of menu 1 of m whose name starts with "SanDisk Ultra")
repeat until exists window 1
  delay 0.2
end repeat
EOF
  sleep 0.5
  capture_window "eject-refused-$suffix"
  ui >/dev/null <<'EOF'
click button 1 of window 1
EOF
  kill "$HOLDER_PID"
  wait "$HOLDER_PID" 2>/dev/null || true
  HOLDER_PID=

  ui >/dev/null <<'EOF'
set m to menu bar item 1 of menu bar 2
click m
delay 0.3
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

  # The receiver rather than the drive, so macOS doesn't add its own "not ejected properly" warning.
  launch "1-2@$SERVER"
  wait_for_menu_line "USB Receiver"
  sleep 2
  stop_server
  sleep 1.5
  screencapture -x -R "$(
    ui <<'EOF'
set {screenW, screenH} to size of menu bar 1
return ((screenW - 420) as text) & ",0,420,150"
EOF
  )" "$OUT/disconnected-notification-$suffix.png"
  echo "$OUT/disconnected-notification-$suffix.png"
  wait_for_menu_line "Try Again"
  capture_menu "menu-reconnecting-$suffix"
  quit_app
done
