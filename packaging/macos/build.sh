#!/usr/bin/env bash
# Builds packaging/out/Rustket-<version>-macos-universal.dmg on macOS: compiles
# the `dist` profile for aarch64 and x86_64 (MACOSX_DEPLOYMENT_TARGET 11.0),
# merges them with lipo, assembles Rustket.app, signs it and wraps it in a
# compressed disk image with an /Applications link.
#
#   packaging/macos/build.sh
#   SKIP_BUILD=1 packaging/macos/build.sh     # reuse target/<triple>/dist/rustket
#   APP_ONLY=1 packaging/macos/build.sh       # stop after assembling the unsigned app
#   PREBUILT_APP=1 packaging/macos/build.sh   # sign and package an existing
#                                             # packaging/out/macos/Rustket.app; runs
#                                             # no cargo and none of the app's code
#
# Signing: ad hoc unless MACOS_SIGN_IDENTITY names a "Developer ID
# Application" identity in the keychain (name or SHA-1); then hardened runtime
# and a secure timestamp, no entitlements. Notarization, optional and only
# with an identity: set MACOS_NOTARY_PROFILE to a `xcrun notarytool
# store-credentials` profile (MACOS_NOTARY_KEYCHAIN: the keychain holding it,
# default the login keychain). The app is notarized and stapled first, then
# the dmg; both are checked with spctl and stapler. Release CI sets all three
# (.github/workflows/release.yml, docs/release-signing.md).
# Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT, RUSTKET_CARGO_FEATURES.
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

[ "$(uname -s)" = Darwin ] || die "macOS only"
TRIPLES="aarch64-apple-darwin x86_64-apple-darwin"
export MACOSX_DEPLOYMENT_TARGET=11.0
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || die "no version in Cargo.toml"
STAGE="$OUT/macos"
APP="$STAGE/Rustket.app"
DMG="$OUT/Rustket-$VERSION-macos-universal.dmg"

PREBUILT="${PREBUILT_APP:-0}"
[ "$PREBUILT" != 1 ] || [ "${APP_ONLY:-0}" != 1 ] || die "PREBUILT_APP and APP_ONLY exclude each other"

slices=()
if [ "$PREBUILT" != 1 ]; then
    for t in $TRIPLES; do
        if [ "${SKIP_BUILD:-0}" != 1 ]; then
            (cd "$ROOT" && cargo build --locked --profile dist -p rustket --bin rustket --target "$t" \
                ${RUSTKET_CARGO_FEATURES:+--features "$RUSTKET_CARGO_FEATURES"}) ||
                die "cargo build for $t failed"
        fi
        [ -x "$TARGET_DIR/$t/dist/rustket" ] || die "$TARGET_DIR/$t/dist/rustket not found"
        slices+=("$TARGET_DIR/$t/dist/rustket")
    done
fi

IDENTITY="${MACOS_SIGN_IDENTITY:-}"
NOTARIZE=0
if [ -n "$IDENTITY" ] && [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    NOTARIZE=1
    NOTARY_AUTH=(--keychain-profile "$MACOS_NOTARY_PROFILE")
    [ -z "${MACOS_NOTARY_KEYCHAIN:-}" ] || NOTARY_AUTH+=(--keychain "$MACOS_NOTARY_KEYCHAIN")
elif [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    die "MACOS_NOTARY_PROFILE needs MACOS_SIGN_IDENTITY"
fi

# json_field <name>: a top-level string of the JSON object on stdin, or nothing.
json_field() {
    python3 -I -c 'import json, sys
try:
    print(json.load(sys.stdin).get(sys.argv[1]) or "")
except Exception:
    pass' "$1"
}

# notarize <file>: submits a zip or dmg and waits; prints the notary log and
# fails unless Apple accepted it.
notarize() {
    local file="$1" json id status
    echo "notarizing ${file##*/}" >&2
    json="$(xcrun notarytool submit "$file" "${NOTARY_AUTH[@]}" --wait --output-format json)" || true
    id="$(json_field id <<<"$json")"
    status="$(json_field status <<<"$json")"
    echo "notarization of ${file##*/}: ${status:-no result}${id:+ (submission $id)}" >&2
    if [ "$status" != Accepted ]; then
        [ -z "$json" ] || echo "$json" >&2
        if [ -n "$id" ]; then
            xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" >&2 || true
        fi
        die "notarization of $file failed"
    fi
}

# staple <file>: the ticket can take a moment to become available; retry.
staple() {
    local attempt
    for attempt in 1 2 3 4; do
        xcrun stapler staple "$1" && return 0
        [ "$attempt" = 4 ] || sleep 15
    done
    die "stapling $1 failed"
}

if [ "$PREBUILT" = 1 ]; then
    [ -x "$APP/Contents/MacOS/rustket" ] || die "$APP not found"
    lipo "$APP/Contents/MacOS/rustket" -verify_arch arm64 x86_64 || die "not a universal binary"
    rm -rf "$STAGE/dmg"
    # Extended attributes picked up in transit would break the signature.
    xattr -cr "$APP"
else
    rm -rf "$STAGE"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    lipo -create -output "$APP/Contents/MacOS/rustket" "${slices[@]}"
    lipo "$APP/Contents/MacOS/rustket" -verify_arch arm64 x86_64 || die "not a universal binary"
    install -m644 "$ICONS/rustket.icns" "$APP/Contents/Resources/rustket.icns"
    install -m644 "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
    install_marker "$APP/Contents/Resources/install-kind" dmg
    # shellcheck disable=SC2086 # one argument per triple
    python3 -I "$ROOT/packaging/notices.py" $TRIPLES -o "$APP/Contents/Resources/THIRD-PARTY-NOTICES" ||
        die "packaging/notices.py failed"
    printf 'APPL????' >"$APP/Contents/PkgInfo"
    cat >"$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>Rustket</string>
    <key>CFBundleExecutable</key>
    <string>rustket</string>
    <key>CFBundleIconFile</key>
    <string>rustket</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.sokolcloud.rustket</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Rustket</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleLocalizations</key>
    <array>
        <string>en</string>
        <string>ru</string>
    </array>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>Rocket.Chat</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>rocketchat</string>
            </array>
        </dict>
    </array>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.social-networking</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 Sergey Sokolov. MIT License.</string>
</dict>
</plist>
EOF
    plutil -lint "$APP/Contents/Info.plist" >/dev/null || die "invalid Info.plist"
    "$APP/Contents/MacOS/rustket" --version >/dev/null || die "the bundled binary does not run"
    if [ "${APP_ONLY:-0}" = 1 ]; then
        echo "$APP"
        exit 0
    fi
fi

if [ -n "$IDENTITY" ]; then
    # Hardened runtime without entitlements: the app needs no JIT, no
    # third-party dylibs, no DYLD_* variables, no camera/microphone and sends
    # no Apple Events (it only receives the URL events).
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
else
    codesign --force --deep --sign - "$APP"
fi
codesign --verify --deep --strict "$APP" || die "code signature does not verify"
if [ "$NOTARIZE" = 1 ]; then
    # The app is notarized on its own so that its ticket is stapled to the
    # bundle inside the dmg (the self-updater copies only the bundle).
    ZIP="$STAGE/Rustket-notarize.zip"
    rm -f "$ZIP"
    ditto -c -k --keepParent "$APP" "$ZIP"
    notarize "$ZIP"
    rm -f "$ZIP"
    staple "$APP"
fi
echo "$APP"

DMGSRC="$STAGE/dmg"
mkdir -p "$DMGSRC"
ditto "$APP" "$DMGSRC/Rustket.app"
ln -s /Applications "$DMGSRC/Applications"
rm -f "$DMG"
# hdiutil occasionally fails with "Resource busy" on CI machines; retry.
for attempt in 1 2 3; do
    if hdiutil create -volname Rustket -srcfolder "$DMGSRC" -fs HFS+ -format UDZO -ov "$DMG"; then
        break
    fi
    [ "$attempt" = 3 ] && die "hdiutil create failed"
    sleep 5
done
if [ -n "$IDENTITY" ]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
    codesign --verify --strict "$DMG" || die "dmg signature does not verify"
fi
if [ "$NOTARIZE" = 1 ]; then
    notarize "$DMG"
    staple "$DMG"
    codesign --verify --deep --strict "$APP" || die "code signature does not verify"
    spctl -a -vv -t exec "$APP" || die "Gatekeeper rejects $APP"
    spctl -a -vv -t open --context context:primary-signature "$DMG" ||
        die "Gatekeeper rejects $DMG"
    xcrun stapler validate "$APP" || die "no stapled ticket on $APP"
    xcrun stapler validate "$DMG" || die "no stapled ticket on $DMG"
elif [ -n "$IDENTITY" ]; then
    echo "warning: signed but not notarized (MACOS_NOTARY_PROFILE unset)" >&2
fi
echo "$DMG"
