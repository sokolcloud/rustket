#!/usr/bin/env bash
# Builds packaging/out/Rustket-<version>-macos-universal.dmg on macOS: compiles
# the `dist` profile for aarch64 and x86_64 (MACOSX_DEPLOYMENT_TARGET 11.0),
# merges them with lipo, assembles Rustket.app, signs it and wraps it in a
# compressed disk image with an /Applications link.
#
#   packaging/macos/build.sh
#   SKIP_BUILD=1 packaging/macos/build.sh   # reuse target/<triple>/dist/rustket
#
# Signing: ad hoc unless MACOS_SIGN_IDENTITY names a "Developer ID
# Application" identity in the keychain (then hardened runtime and a secure
# timestamp). Notarization, optional and only with an identity: set
# MACOS_NOTARY_PROFILE to a `xcrun notarytool store-credentials` profile.
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

slices=()
for t in $TRIPLES; do
    if [ "${SKIP_BUILD:-0}" != 1 ]; then
        (cd "$ROOT" && cargo build --locked --profile dist -p rustket --bin rustket --target "$t" \
            ${RUSTKET_CARGO_FEATURES:+--features "$RUSTKET_CARGO_FEATURES"}) ||
            die "cargo build for $t failed"
    fi
    [ -x "$TARGET_DIR/$t/dist/rustket" ] || die "$TARGET_DIR/$t/dist/rustket not found"
    slices+=("$TARGET_DIR/$t/dist/rustket")
done

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

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$APP"
else
    codesign --force --deep --sign - "$APP"
fi
codesign --verify --deep --strict "$APP" || die "code signature does not verify"
echo "$APP"

DMGSRC="$STAGE/dmg"
mkdir -p "$DMGSRC"
cp -R "$APP" "$DMGSRC/"
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
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$DMG"
    if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$DMG" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG"
    fi
fi
echo "$DMG"
