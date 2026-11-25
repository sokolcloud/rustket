#!/usr/bin/env bash
# Builds packaging/out/Rustket-<version>-macos-universal.dmg on macOS: compiles
# the `dist` profile for aarch64 and x86_64 (MACOSX_DEPLOYMENT_TARGET 11.0),
# merges them with lipo, assembles Rustket.app, signs it and wraps it in a
# compressed disk image with an /Applications link. MAS=1 builds the Mac App
# Store package instead (below).
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
# the dmg; both are checked with spctl and stapler. Each submission waits at
# most MACOS_NOTARY_TIMEOUT (notarytool duration, default 300m). Release CI
# sets all three (.github/workflows/release.yml, docs/release-signing.md).
# Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT, RUSTKET_CARGO_FEATURES.
#
# Mac App Store (docs/app-store.md, .github/workflows/app-store.yml): MAS=1
# needs RUSTKET_CARGO_FEATURES=mas, a numeric x.y.z version, MAS_BUILD_NUMBER
# (CFBundleVersion) and a full Xcode (actool, xcodebuild). Stage directory
# packaging/out/macos-mas, output packaging/out/Rustket-<version>-mas.pkg.
#
#   MAS=1 APP_ONLY=1 packaging/macos/build.sh
#       compile, assemble the unsigned app, run the static gates and the DT*
#       probe (packaging/macos/xcode-probe, when present)
#   MAS=1 PREBUILT_APP=1 MAS_BINARY=<universal rustket> MAS_NOTICES=<file> \
#       packaging/macos/build.sh
#       assemble the app around a prebuilt binary, which is only inspected
#       (lipo, vtool, nm), never run; MAS_NOTICES is the THIRD-PARTY-NOTICES
#       text of the same build. Everything else comes from this checkout.
#       With APP_ONLY=1 it stops before signing.
#
# MAS signing: MAS_SIGN_IDENTITY ("Apple Distribution", name or SHA-1),
# MAS_INSTALLER_IDENTITY ("3rd Party Mac Developer Installer", name),
# MAS_PROVISIONING_PROFILE (path of the Mac App Store Connect profile) and
# APPLE_TEAM_ID (default D6JXBXV9Q5). MAS_SANDBOX_TEST=1 instead signs with
# the Developer ID identity in MACOS_SIGN_IDENTITY and
# rustket-sandbox-test.entitlements (no profile), notarizes with
# MACOS_NOTARY_PROFILE and writes Rustket-<version>-mas-sandbox-test.zip for
# testing the sandbox on a Mac.
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

[ "$(uname -s)" = Darwin ] || die "macOS only"
TRIPLES="aarch64-apple-darwin x86_64-apple-darwin"
export MACOSX_DEPLOYMENT_TARGET=11.0
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || die "no version in Cargo.toml"
MACOS="$ROOT/packaging/macos"
# CFBundleLocalizations and the <lang>.lproj folders (packaging/macos/InfoPlist):
# the bundled UI languages (i18n BUNDLED_COMPLETE, zh as zh-Hans).
LOCALIZATIONS="en ru es th zh-Hans"
BUNDLE_ID=io.github.sokolcloud.rustket
TEAM_ID="${APPLE_TEAM_ID:-D6JXBXV9Q5}"

MAS="${MAS:-0}"
case "$MAS" in 0 | 1) ;; *) die "MAS must be 0 or 1" ;; esac
PREBUILT="${PREBUILT_APP:-0}"
APP_ONLY="${APP_ONLY:-0}"
SANDBOX_TEST="${MAS_SANDBOX_TEST:-0}"
FEATURES=" ${RUSTKET_CARGO_FEATURES:-} "
FEATURES="${FEATURES//,/ }"

if [ "$MAS" = 1 ]; then
    STAGE="$OUT/macos-mas"
    PKG="$OUT/Rustket-$VERSION-mas.pkg"
    [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
        die "the App Store needs a numeric x.y.z version, not $VERSION"
    BUILD_NUMBER="${MAS_BUILD_NUMBER:-}"
    [[ "$BUILD_NUMBER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] ||
        die "MAS_BUILD_NUMBER must be one to three numbers separated by dots (got '$BUILD_NUMBER')"
    if [ "$PREBUILT" = 1 ]; then
        [ -n "${MAS_BINARY:-}" ] && [ -f "$MAS_BINARY" ] && [ ! -L "$MAS_BINARY" ] ||
            die "PREBUILT_APP=1 MAS=1 needs MAS_BINARY, the universal rustket binary"
        case "$(cd "$(dirname "$MAS_BINARY")" && pwd)/" in
            "$STAGE"/*) die "MAS_BINARY must not live in $STAGE" ;;
        esac
    else
        [ -z "${MAS_BINARY:-}" ] || die "MAS_BINARY needs PREBUILT_APP=1"
        case "$FEATURES" in *" mas "*) ;; *) die "MAS=1 needs RUSTKET_CARGO_FEATURES=mas" ;; esac
        case "$FEATURES" in *" self-update "*) die "the App Store build must not enable self-update" ;; esac
    fi
    if [ "$APP_ONLY" != 1 ]; then
        if [ "$SANDBOX_TEST" = 1 ]; then
            [ -n "${MACOS_SIGN_IDENTITY:-}" ] && [ -n "${MACOS_NOTARY_PROFILE:-}" ] ||
                die "MAS_SANDBOX_TEST=1 needs MACOS_SIGN_IDENTITY and MACOS_NOTARY_PROFILE"
        else
            [ -n "${MAS_SIGN_IDENTITY:-}" ] || die "MAS=1 needs MAS_SIGN_IDENTITY (or APP_ONLY=1)"
            [ -n "${MAS_INSTALLER_IDENTITY:-}" ] || die "MAS=1 needs MAS_INSTALLER_IDENTITY"
            [ -f "${MAS_PROVISIONING_PROFILE:-}" ] ||
                die "MAS=1 needs MAS_PROVISIONING_PROFILE, the path of the provisioning profile"
        fi
    fi
else
    STAGE="$OUT/macos"
    DMG="$OUT/Rustket-$VERSION-macos-universal.dmg"
    [ "$PREBUILT" != 1 ] || [ "$APP_ONLY" != 1 ] || die "PREBUILT_APP and APP_ONLY exclude each other"
    case "$FEATURES" in *" mas "*) die "the mas feature needs MAS=1" ;; esac
    [ -z "${MAS_BINARY:-}" ] || die "MAS_BINARY needs MAS=1"
    [ "$SANDBOX_TEST" != 1 ] || die "MAS_SANDBOX_TEST needs MAS=1"
fi
APP="$STAGE/Rustket.app"

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
[ "$MAS" != 1 ] || [ "$SANDBOX_TEST" = 1 ] || IDENTITY="${MAS_SIGN_IDENTITY:-}"
NOTARIZE=0
if [ -n "$IDENTITY" ] && [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    NOTARIZE=1
    NOTARY_AUTH=(--keychain-profile "$MACOS_NOTARY_PROFILE")
    [ -z "${MACOS_NOTARY_KEYCHAIN:-}" ] || NOTARY_AUTH+=(--keychain "$MACOS_NOTARY_KEYCHAIN")
elif [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    die "MACOS_NOTARY_PROFILE needs MACOS_SIGN_IDENTITY"
fi
# App Store packages are not notarized; Apple processes the upload instead.
[ "$MAS" != 1 ] || [ "$SANDBOX_TEST" = 1 ] || NOTARIZE=0

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
    # A new team's first submissions can stay "In Progress" for hours.
    json="$(xcrun notarytool submit "$file" "${NOTARY_AUTH[@]}" --wait \
        --timeout "${MACOS_NOTARY_TIMEOUT:-300m}" --output-format json)" || true
    id="$(json_field id <<<"$json")"
    status="$(json_field status <<<"$json")"
    echo "notarization of ${file##*/}: ${status:-no result}${id:+ (submission $id)}" >&2
    if [ "$status" != Accepted ]; then
        [ -z "$json" ] || echo "$json" >&2
        if [ "$status" = "In Progress" ]; then
            die "Apple is still processing submission $id after ${MACOS_NOTARY_TIMEOUT:-300m}; re-run the job later (xcrun notarytool info $id shows the status)"
        fi
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

# dt_values: the toolchain keys an Xcode build writes into Info.plist,
# computed from the selected Xcode (DEVELOPER_DIR) and this machine. The key
# set and the meaning of DTPlatformBuild (the Xcode build) follow an
# Xcode-built app; dt_probe checks them against one.
dt_values() {
    local xv major minor patch
    xv="$(xcodebuild -version)" || die "xcodebuild -version failed (a full Xcode is needed; set DEVELOPER_DIR)"
    XCODE_VERSION="$(awk '$1 == "Xcode" { print $2; exit }' <<<"$xv")"
    DT_XCODE_BUILD="$(awk '$1 == "Build" && $2 == "version" { print $3; exit }' <<<"$xv")"
    [[ "$XCODE_VERSION" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || die "unexpected Xcode version '$XCODE_VERSION'"
    [[ "$DT_XCODE_BUILD" =~ ^[0-9]+[A-Z][0-9]+[a-z]?$ ]] || die "unexpected Xcode build '$DT_XCODE_BUILD'"
    # App Store Connect rejects apps built with a beta Xcode. The build
    # number cannot tell (releases such as 15E204a and 16C5032a end in a
    # letter too); beta installs are named Xcode-beta.app or Xcode_*_beta.app.
    if [[ "${DEVELOPER_DIR:-}:$(xcode-select -p)" == *[Bb]eta* ]] || grep -qi beta <<<"$xv"; then
        die "Xcode $XCODE_VERSION ($DT_XCODE_BUILD) is a beta; the App Store needs a release Xcode"
    fi
    IFS=. read -r major minor patch <<<"$XCODE_VERSION"
    DT_XCODE="$(printf '%02d%d%d' "$major" "${minor:-0}" "${patch:-0}")"
    DT_SDK_VERSION="$(xcrun --sdk macosx --show-sdk-version)" || die "no macOS SDK"
    DT_SDK_BUILD="$(xcrun --sdk macosx --show-sdk-build-version)" || die "no macOS SDK build version"
    DT_PLATFORM_VERSION="$(xcrun --sdk macosx --show-sdk-platform-version 2>/dev/null || echo "$DT_SDK_VERSION")"
    DT_BUILD_MACHINE="$(sw_vers -buildVersion)"
    echo "Xcode $XCODE_VERSION ($DT_XCODE_BUILD), macOS SDK $DT_SDK_VERSION ($DT_SDK_BUILD), build machine $DT_BUILD_MACHINE" >&2
}

# check_binary <file>: static gates on the universal binary (App Store
# builds). The binary is data here: it is inspected, never run.
check_binary() {
    local bin="$1" arch info minos sdk syms bad marker
    lipo "$bin" -verify_arch arm64 x86_64 || die "not a universal arm64 + x86_64 binary"
    for arch in arm64 x86_64; do
        info="$(vtool -arch "$arch" -show-build "$bin")" || die "vtool failed for the $arch slice"
        minos="$(awk '$1 == "minos" { print $2; exit }' <<<"$info")"
        sdk="$(awk '$1 == "sdk" { print $2; exit }' <<<"$info")"
        [ "$minos" = "$MACOSX_DEPLOYMENT_TARGET" ] ||
            die "the $arch slice has minos '$minos', LSMinimumSystemVersion is $MACOSX_DEPLOYMENT_TARGET"
        [ -n "$sdk" ] && [ "${sdk%%.*}" = "${DT_SDK_VERSION%%.*}" ] ||
            die "the $arch slice was linked against SDK '$sdk', this Xcode has SDK $DT_SDK_VERSION"
        echo "$arch: minos $minos, sdk $sdk" >&2
        # Private CoreGraphics/SkyLight symbols (guideline 2.5.1, ITMS-90338):
        # CGS/SLS followed by a capital letter, which leaves out public
        # CGSize* and CGShieldingWindowLevel.
        syms="$(nm -u -arch "$arch" "$bin")" || die "nm -u failed for the $arch slice"
        [ -n "$syms" ] || die "nm -u listed no symbols for the $arch slice"
        # The name is the last field, whether nm prints "_sym" or "U _sym".
        bad="$(awk '{ print $NF }' <<<"$syms" | grep -E '^_(CGS|SLS)[A-Z]' || true)"
        [ -z "$bad" ] || die "the $arch slice references private symbols: $(tr '\n' ' ' <<<"$bad")"
    done
    # A private NSUserNotificationCenter delegate selector, and markers of the
    # self-updater (guideline 2.4.5(vii)).
    for marker in didDismissAlert api.github.com install-kind SHA256SUMS; do
        if LC_ALL=C grep -aqF "$marker" "$bin"; then
            die "the binary contains '$marker'"
        fi
    done
}

# write_info_plist <file>: Info.plist of both flavors.
write_info_plist() {
    local bundle_version="$VERSION" langs="" l mas_keys="" icon_name=""
    for l in $LOCALIZATIONS; do
        langs+="        <string>$l</string>"$'\n'
    done
    if [ "$HAVE_ASSETS" = 1 ]; then
        icon_name="    <key>CFBundleIconName</key>
    <string>AppIcon</string>"$'\n'
    fi
    if [ "$MAS" = 1 ]; then
        bundle_version="$BUILD_NUMBER"
        mas_keys="    <key>BuildMachineOSBuild</key>
    <string>$DT_BUILD_MACHINE</string>
    <key>CFBundleSupportedPlatforms</key>
    <array>
        <string>MacOSX</string>
    </array>
    <key>DTCompiler</key>
    <string>com.apple.compilers.llvm.clang.1_0</string>
    <key>DTPlatformBuild</key>
    <string>$DT_XCODE_BUILD</string>
    <key>DTPlatformName</key>
    <string>macosx</string>
    <key>DTPlatformVersion</key>
    <string>$DT_PLATFORM_VERSION</string>
    <key>DTSDKBuild</key>
    <string>$DT_SDK_BUILD</string>
    <key>DTSDKName</key>
    <string>macosx$DT_SDK_VERSION</string>
    <key>DTXcode</key>
    <string>$DT_XCODE</string>
    <key>DTXcodeBuild</key>
    <string>$DT_XCODE_BUILD</string>
    <key>ITSAppUsesNonExemptEncryption</key>
    <false/>"$'\n'
    fi
    cat >"$1" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
${mas_keys}    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>Rustket</string>
    <key>CFBundleExecutable</key>
    <string>rustket</string>
    <key>CFBundleIconFile</key>
    <string>rustket</string>
${icon_name}    <key>CFBundleIdentifier</key>
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
    <string>$bundle_version</string>
    <key>CFBundleLocalizations</key>
    <array>
${langs}    </array>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>io.github.sokolcloud.rustket</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>rocketchat</string>
            </array>
        </dict>
    </array>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.business</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 Sergey Sokolov. MIT License.</string>
    <key>NSLocalNetworkUsageDescription</key>
    <string>Rustket connects to the workspace servers you add, including servers on your local network.</string>
</dict>
</plist>
EOF
    plutil -lint "$1" >/dev/null || die "invalid Info.plist"
}

# install_localizations: <lang>.lproj/InfoPlist.strings for every
# CFBundleLocalizations entry.
install_localizations() {
    local l src
    for l in $LOCALIZATIONS; do
        src="$MACOS/InfoPlist/$l.lproj/InfoPlist.strings"
        [ -f "$src" ] || die "missing $src"
        plutil -lint "$src" >/dev/null || die "invalid $src"
        install -d "$APP/Contents/Resources/$l.lproj"
        install -m644 "$src" "$APP/Contents/Resources/$l.lproj/InfoPlist.strings"
    done
}

# compile_assets: Assets.car with the AppIcon set (CFBundleIconName); the
# icns stays for CFBundleIconFile. Needs Xcode's actool: required for the
# App Store, skipped with a warning for a local dmg without Xcode.
compile_assets() {
    local catalog="$STAGE/Assets.xcassets"
    HAVE_ASSETS=0
    if ! xcrun --find actool >/dev/null 2>&1; then
        [ "$MAS" != 1 ] || die "actool not found: the App Store build needs a full Xcode (DEVELOPER_DIR)"
        echo "warning: actool not found (no full Xcode); the app has no Assets.car" >&2
        return 0
    fi
    rm -rf "$catalog"
    mkdir -p "$catalog"
    cp -R "$MACOS/AppIcon.appiconset" "$catalog/AppIcon.appiconset"
    printf '{\n  "info" : {\n    "author" : "xcode",\n    "version" : 1\n  }\n}\n' >"$catalog/Contents.json"
    xcrun actool --compile "$APP/Contents/Resources" --platform macosx --target-device mac \
        --app-icon AppIcon --minimum-deployment-target "$MACOSX_DEPLOYMENT_TARGET" \
        --output-partial-info-plist "$STAGE/actool-Info.plist" \
        --errors --warnings --notices --output-format human-readable-text "$catalog" ||
        die "actool failed"
    [ -f "$APP/Contents/Resources/Assets.car" ] || die "actool wrote no Assets.car"
    # actool also writes AppIcon.icns; CFBundleIconFile names rustket.icns.
    rm -f "$APP/Contents/Resources/AppIcon.icns" "$STAGE/actool-Info.plist"
    rm -rf "$catalog"
    HAVE_ASSETS=1
}

# dt_probe: builds packaging/macos/xcode-probe (an empty Xcode macOS app)
# with the same Xcode and fails if its toolchain keys differ from ours.
dt_probe() {
    local dir="$MACOS/xcode-probe" project plist
    if [ ! -d "$dir" ]; then
        echo "notice: packaging/macos/xcode-probe does not exist yet; the DT* keys are not compared with an Xcode-built app (docs/app-store.md)" >&2
        return 0
    fi
    project="$(find "$dir" -maxdepth 2 -name '*.xcodeproj' -print -quit)"
    [ -n "$project" ] || die "no .xcodeproj in $dir"
    rm -rf "$STAGE/probe"
    if ! xcodebuild -project "$project" -configuration Release -derivedDataPath "$STAGE/probe" \
        CODE_SIGNING_ALLOWED=NO build >"$STAGE/probe.log" 2>&1; then
        tail -50 "$STAGE/probe.log" >&2
        die "the Xcode probe does not build"
    fi
    plist="$(find "$STAGE/probe/Build/Products/Release" -maxdepth 3 -path '*.app/Contents/Info.plist' -print -quit)"
    [ -n "$plist" ] || die "the Xcode probe built no app"
    python3 -I - "$plist" "$APP/Contents/Info.plist" <<'PY' || die "the DT* keys differ from an Xcode-built app"
import plistlib, sys
probe, ours = (plistlib.load(open(p, "rb")) for p in sys.argv[1:3])
keys = {k for d in (probe, ours) for k in d
        if k.startswith("DT") or k in ("BuildMachineOSBuild", "CFBundleSupportedPlatforms")}
bad = [k for k in sorted(keys) if probe.get(k) != ours.get(k)]
for k in sorted(keys):
    print(f"{'DIFF' if k in bad else 'ok  '} {k}: probe {probe.get(k)!r}, ours {ours.get(k)!r}", file=sys.stderr)
sys.exit(1 if bad else 0)
PY
    rm -rf "$STAGE/probe" "$STAGE/probe.log"
}

# check_notices <file>: THIRD-PARTY-NOTICES from the build job is plain
# UTF-8 text written by packaging/notices.py.
check_notices() {
    python3 -I - "$1" <<'PY' || die "MAS_NOTICES is not a notices file from packaging/notices.py"
import sys
data = open(sys.argv[1], "rb").read()
text = data.decode("utf-8")
ok = (len(data) < 16 << 20 and "\0" not in text
      and text.startswith("Third-party software in Rustket\n"))
sys.exit(0 if ok else 1)
PY
}

HAVE_ASSETS=0
if [ "$MAS" = 1 ]; then
    dt_values
    rm -rf "$STAGE"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    if [ "$PREBUILT" = 1 ]; then
        install -m755 "$MAS_BINARY" "$APP/Contents/MacOS/rustket"
    else
        lipo -create -output "$APP/Contents/MacOS/rustket" "${slices[@]}"
    fi
    check_binary "$APP/Contents/MacOS/rustket"
    install -m644 "$ICONS/rustket.icns" "$APP/Contents/Resources/rustket.icns"
    install -m644 "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
    if [ "$PREBUILT" = 1 ]; then
        if [ -n "${MAS_NOTICES:-}" ]; then
            check_notices "$MAS_NOTICES"
            install -m644 "$MAS_NOTICES" "$APP/Contents/Resources/THIRD-PARTY-NOTICES.txt"
        else
            [ "$APP_ONLY" = 1 ] || die "MAS_NOTICES is needed for a signed package"
            echo "warning: no MAS_NOTICES; the app has no THIRD-PARTY-NOTICES" >&2
        fi
    else
        # shellcheck disable=SC2086 # one argument per triple
        python3 -I "$ROOT/packaging/notices.py" $TRIPLES -o "$APP/Contents/Resources/THIRD-PARTY-NOTICES.txt" ||
            die "packaging/notices.py failed"
    fi
    install_localizations
    compile_assets
    printf 'APPL????' >"$APP/Contents/PkgInfo"
    write_info_plist "$APP/Contents/Info.plist"
    # The App Store updates the app; no install-kind marker for the updater.
    [ ! -e "$APP/Contents/Resources/install-kind" ] || die "install-kind marker in an App Store build"
    if [ "$PREBUILT" != 1 ]; then
        "$APP/Contents/MacOS/rustket" --version >/dev/null || die "the bundled binary does not run"
        dt_probe
    fi
    if [ "$APP_ONLY" = 1 ]; then
        echo "$APP"
        exit 0
    fi
elif [ "$PREBUILT" = 1 ]; then
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
    python3 -I "$ROOT/packaging/notices.py" $TRIPLES -o "$APP/Contents/Resources/THIRD-PARTY-NOTICES.txt" ||
        die "packaging/notices.py failed"
    install_localizations
    compile_assets
    printf 'APPL????' >"$APP/Contents/PkgInfo"
    write_info_plist "$APP/Contents/Info.plist"
    "$APP/Contents/MacOS/rustket" --version >/dev/null || die "the bundled binary does not run"
    if [ "$APP_ONLY" = 1 ]; then
        echo "$APP"
        exit 0
    fi
fi

# check_profile <file>: a Mac App Store Connect distribution profile for this
# app and team that has not expired (a warning under 30 days left).
check_profile() {
    security cms -D -i "$1" >"$STAGE/profile.plist" || die "cannot decode the provisioning profile"
    python3 -I - "$STAGE/profile.plist" "$TEAM_ID.$BUNDLE_ID" "$TEAM_ID" <<'PY' || die "unusable provisioning profile"
import datetime, plistlib, sys
path, app_id, team = sys.argv[1:4]
p = plistlib.load(open(path, "rb"))
ent = p.get("Entitlements") or {}
errors = []
if ent.get("com.apple.application-identifier") != app_id:
    errors.append(f"application-identifier is {ent.get('com.apple.application-identifier')!r}, expected {app_id!r}")
if ent.get("com.apple.developer.team-identifier") != team or team not in (p.get("TeamIdentifier") or []):
    errors.append(f"not a profile of team {team}")
if "ProvisionedDevices" in p or p.get("ProvisionsAllDevices"):
    errors.append("a development or Developer ID profile (it lists devices); use a Mac App Store Connect profile")
if "OSX" not in (p.get("Platform") or ["OSX"]):
    errors.append(f"platform {p.get('Platform')!r}, not macOS")
expires = p.get("ExpirationDate")
now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
days = (expires - now).total_seconds() / 86400 if expires else -1
if days < 0:
    errors.append(f"expired on {expires}; renew it (docs/app-store.md)")
elif days < 30:
    print(f"warning: the provisioning profile expires in {days:.0f} days ({expires})", file=sys.stderr)
print(f"profile {p.get('Name')!r} ({p.get('UUID')}), expires {expires}", file=sys.stderr)
for e in errors:
    print(f"error: provisioning profile: {e}", file=sys.stderr)
sys.exit(1 if errors else 0)
PY
    rm -f "$STAGE/profile.plist"
}

# check_entitlements <expected.entitlements>: the signature carries exactly
# these entitlements.
check_entitlements() {
    codesign -d --entitlements - --xml "$APP" >"$STAGE/signed.entitlements" 2>/dev/null ||
        die "cannot read the entitlements of $APP"
    python3 -I - "$1" "$STAGE/signed.entitlements" <<'PY' || die "the signed entitlements differ from ${1##*/}"
import plistlib, sys
want, got = (plistlib.load(open(p, "rb")) for p in sys.argv[1:3])
for k in sorted(set(want) | set(got)):
    if want.get(k) != got.get(k):
        print(f"entitlement {k}: expected {want.get(k)!r}, signed {got.get(k)!r}", file=sys.stderr)
sys.exit(0 if want == got else 1)
PY
    rm -f "$STAGE/signed.entitlements"
}

if [ "$MAS" = 1 ] && [ "$SANDBOX_TEST" = 1 ]; then
    # A Developer ID build with the App Sandbox, for testing on a Mac before
    # TestFlight: no identity entitlements, no profile.
    ENTITLEMENTS="$MACOS/rustket-sandbox-test.entitlements"
    xattr -cr "$APP"
    codesign --force -o runtime --timestamp --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
    codesign --verify --strict --verbose=2 "$APP" || die "code signature does not verify"
    check_entitlements "$ENTITLEMENTS"
    ZIP="$STAGE/Rustket-notarize.zip"
    ditto -c -k --keepParent "$APP" "$ZIP"
    notarize "$ZIP"
    rm -f "$ZIP"
    staple "$APP"
    spctl -a -vv -t exec "$APP" || die "Gatekeeper rejects $APP"
    OUTZIP="$OUT/Rustket-$VERSION-mas-sandbox-test.zip"
    rm -f "$OUTZIP"
    ditto -c -k --keepParent "$APP" "$OUTZIP"
    echo "$OUTZIP"
    exit 0
fi

if [ "$MAS" = 1 ]; then
    ENTITLEMENTS="$MACOS/rustket-mas.entitlements"
    python3 -I - "$ENTITLEMENTS" "$TEAM_ID.$BUNDLE_ID" "$TEAM_ID" <<'PY' || die "rustket-mas.entitlements does not match team $TEAM_ID"
import plistlib, sys
e = plistlib.load(open(sys.argv[1], "rb"))
sys.exit(0 if e.get("com.apple.application-identifier") == sys.argv[2]
         and e.get("com.apple.developer.team-identifier") == sys.argv[3] else 1)
PY
    check_profile "$MAS_PROVISIONING_PROFILE"
    install -m644 "$MAS_PROVISIONING_PROFILE" "$APP/Contents/embedded.provisionprofile"
    xattr -cr "$APP"
    # One signature over the finished bundle, last, not deep: the bundle
    # has a single Mach-O.
    codesign --force -o runtime --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
    codesign --verify --strict --verbose=2 "$APP" || die "code signature does not verify"
    codesign -d -r- "$APP" || die "cannot read the designated requirement"
    check_entitlements "$ENTITLEMENTS"
    [ -z "$(xattr -r -l "$APP")" ] || die "extended attributes left on $APP"
    rm -f "$PKG"
    productbuild --component "$APP" /Applications --sign "$MAS_INSTALLER_IDENTITY" "$PKG" ||
        die "productbuild failed"
    pkgutil --check-signature "$PKG" || die "the package signature does not verify"
    # The payload is the signed app with its profile.
    rm -rf "$STAGE/expanded"
    pkgutil --expand-full "$PKG" "$STAGE/expanded" || die "cannot expand $PKG"
    payload="$(find "$STAGE/expanded" -type d -path '*/Payload/Rustket.app' -print -quit)"
    [ -n "$payload" ] || die "no Rustket.app in the package payload"
    for f in Contents/embedded.provisionprofile Contents/_CodeSignature/CodeResources \
        Contents/Resources/Assets.car Contents/MacOS/rustket; do
        [ -f "$payload/$f" ] || die "the package payload lacks $f"
    done
    [ ! -e "$payload/Contents/Resources/install-kind" ] || die "install-kind marker in the package"
    rm -rf "$STAGE/expanded"
    echo "$PKG"
    exit 0
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
