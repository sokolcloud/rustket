#!/usr/bin/env bash
# Assembles packaging/out/Rustket.AppDir from the `dist` build and, when
# `appimagetool` is on PATH, packs it into packaging/out/Rustket-<version>-<arch>.AppImage.
#
#   packaging/appimage/build.sh
#   SKIP_BUILD=1 packaging/appimage/build.sh   # reuse an existing target/dist/rustket
#
# Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT, APPIMAGETOOL (path),
# APPIMAGETOOL_ARGS (default "--no-appstream": the metainfo has two AppStream
# warnings, a non-rDNS id and no homepage, which appimagetool treats as errors).
# RUSTKET_CARGO_FEATURES: Cargo features of the build.
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

build_binary
VERSION="$(binary_version)"
ARCH="$(uname -m)"
APPDIR="$OUT/Rustket.AppDir"
IMAGE="$OUT/Rustket-$VERSION-$ARCH.AppImage"

rm -rf "$APPDIR"
mkdir -p "$APPDIR"
install -Dm755 "$ROOT/packaging/appimage/AppRun" "$APPDIR/AppRun"
install -Dm755 "$BIN" "$APPDIR/usr/bin/rustket"
install -Dm644 "$LINUX/rustket.desktop" "$APPDIR/usr/share/applications/rustket.desktop"
# appimagetool looks for <desktop id>.appdata.xml.
install -Dm644 "$LINUX/rustket.metainfo.xml" "$APPDIR/usr/share/metainfo/rustket.appdata.xml"
install_icons "$APPDIR/usr/share"
install_marker "$APPDIR/usr/lib/rustket/install-kind" appimage
install -Dm644 "$ROOT/packaging/deb/copyright" "$APPDIR/usr/share/doc/rustket/copyright"
third_party_notices "$APPDIR/usr/share/doc/rustket/THIRD-PARTY-NOTICES"
{
    cat "$LINUX/rustket.desktop"
    echo "X-AppImage-Version=$VERSION"
} >"$APPDIR/rustket.desktop"
chmod 644 "$APPDIR/rustket.desktop"
install -m644 "$ICONS/rustket-256.png" "$APPDIR/rustket.png"
ln -sf rustket.png "$APPDIR/.DirIcon"

"$APPDIR/AppRun" --version >/dev/null || die "the AppDir does not run"
echo "$APPDIR"

TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [ -n "$TOOL" ]; then
    # shellcheck disable=SC2086
    ARCH="$ARCH" "$TOOL" ${APPIMAGETOOL_ARGS---no-appstream} "$APPDIR" "$IMAGE"
    echo "$IMAGE"
else
    cat <<EOF
appimagetool is not installed; the AppDir is complete. To produce the AppImage:
  ARCH=$ARCH appimagetool --no-appstream "$APPDIR" "$IMAGE"
(appimagetool: https://github.com/AppImage/appimagetool/releases)
EOF
fi
