BUNDLE_ID=net.karstx.usbip
HCI_ENTITLEMENT=com.apple.developer.usb.host-controller-interface
IDENTITY=${IDENTITY:--}
PROFILE=${PROFILE:-}
NOTARY_PROFILE=${NOTARY_PROFILE:-}
# Notarize with an App Store Connect API key instead of a keychain profile: the .p8 file, its
# key id and the issuer id. Suits a headless build that has no keychain profile to look up.
NOTARY_KEY=${NOTARY_KEY:-}
NOTARY_KEY_ID=${NOTARY_KEY_ID:-}
NOTARY_ISSUER=${NOTARY_ISSUER:-}
# Look IDENTITY up in this keychain only, e.g. a dedicated signing keychain on a machine whose
# login keychain is locked or has several identities with the same name.
KEYCHAIN=${KEYCHAIN:-}
OUT=${1:-.}

CODESIGN=/usr/bin/codesign
SECURITY=/usr/bin/security
PLISTBUDDY=/usr/libexec/PlistBuddy
HDIUTIL=/usr/bin/hdiutil
DISKUTIL=/usr/sbin/diskutil
DITTO=/usr/bin/ditto
XCRUN=/usr/bin/xcrun

die() {
  echo "error: $*" >&2
  exit 1
}

NOTARY_ARGS=()
if [ -n "$NOTARY_KEY" ]; then
  [ -n "$NOTARY_KEY_ID" ] && [ -n "$NOTARY_ISSUER" ] || die "NOTARY_KEY needs NOTARY_KEY_ID and NOTARY_ISSUER"
  NOTARY_ARGS=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "$NOTARY_PROFILE" ]; then
  NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
fi
if [ ${#NOTARY_ARGS[@]} -gt 0 ] && [ "$IDENTITY" = "-" ]; then
  die "notarizing needs a Developer ID IDENTITY"
fi

NAME=$(basename "$APP" .app)
TARGET=$OUT/$NAME.app
DMG=$OUT/$NAME-$VERSION.dmg
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

rm -rf "$TARGET"
mkdir -p "$OUT"
cp -R "$APP" "$TARGET"
chmod -R u+w "$TARGET"

KEYCHAIN_ARGS=()
if [ -n "$KEYCHAIN" ]; then
  KEYCHAIN_ARGS=(--keychain "$KEYCHAIN")
fi

SIGN_ARGS=(--force --sign "$IDENTITY" "${KEYCHAIN_ARGS[@]}")
if [ "$IDENTITY" != "-" ]; then
  SIGN_ARGS+=(--options runtime --timestamp)
fi

if [ -n "$PROFILE" ]; then
  [ "$IDENTITY" != "-" ] || die "PROFILE needs a real IDENTITY, an ad-hoc signature cannot use it"
  "$SECURITY" cms -D -i "$PROFILE" >"$WORK/profile.plist" || die "cannot decode $PROFILE"

  APP_ID=$("$PLISTBUDDY" -c "Print :Entitlements:com.apple.application-identifier" "$WORK/profile.plist")
  TEAM_ID=$("$PLISTBUDDY" -c "Print :TeamIdentifier:0" "$WORK/profile.plist")
  [ "$APP_ID" = "$TEAM_ID.$BUNDLE_ID" ] || die "$PROFILE is for $APP_ID, not $TEAM_ID.$BUNDLE_ID"
  "$PLISTBUDDY" -c "Print :Entitlements:$HCI_ENTITLEMENT" "$WORK/profile.plist" >/dev/null 2>&1 ||
    die "$PROFILE does not grant $HCI_ENTITLEMENT"

  cp "$PROFILE" "$TARGET/Contents/embedded.provisionprofile"

  cp "$ENTITLEMENTS" "$WORK/entitlements.plist"
  chmod u+w "$WORK/entitlements.plist"
  "$PLISTBUDDY" -c "Add :com.apple.application-identifier string $APP_ID" "$WORK/entitlements.plist"
  "$PLISTBUDDY" -c "Add :com.apple.developer.team-identifier string $TEAM_ID" "$WORK/entitlements.plist"
  SIGN_ARGS+=(--entitlements "$WORK/entitlements.plist")
fi

"$CODESIGN" "${SIGN_ARGS[@]}" "$TARGET"
"$CODESIGN" --verify --strict --verbose=2 "$TARGET"

mkdir "$WORK/dmg"
"$DITTO" "$TARGET" "$WORK/dmg/$NAME.app"
ln -s /Applications "$WORK/dmg/Applications"
rm -f "$DMG"
if "$DISKUTIL" image create from --help >/dev/null 2>&1; then
  "$DISKUTIL" image create from --format UDZO --volumeName "$NAME $VERSION" "$WORK/dmg" "$DMG" \
    >"$WORK/diskutil.log" 2>&1 || {
    cat "$WORK/diskutil.log" >&2
    die "creating $DMG failed"
  }
else
  "$HDIUTIL" create -quiet -volname "$NAME $VERSION" -srcfolder "$WORK/dmg" -format UDZO "$DMG"
fi

if [ "$IDENTITY" != "-" ]; then
  "$CODESIGN" --force --sign "$IDENTITY" "${KEYCHAIN_ARGS[@]}" --timestamp "$DMG"
fi
if [ ${#NOTARY_ARGS[@]} -gt 0 ]; then
  "$XCRUN" notarytool submit "$DMG" "${NOTARY_ARGS[@]}" --wait
  "$XCRUN" stapler staple "$DMG"
fi

echo "$TARGET"
echo "$DMG"
