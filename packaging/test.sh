#!/usr/bin/env bash
# Packaging smoke test: builds the .deb and the AppDir, then checks the package
# contents, control fields, metadata files and `--version` of the packaged
# binary. Never installs anything and never opens a window.
#
#   packaging/test.sh                # full dist build first
#   SKIP_BUILD=1 packaging/test.sh   # reuse an existing target/dist/rustket
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

fail=0
check() {
    if eval "$2"; then
        echo "ok   $1"
    else
        echo "FAIL $1"
        fail=1
    fi
}

DEB="$("$ROOT/packaging/deb/build.sh" | tail -1)"
[ -f "$DEB" ] || die "no package produced"
echo "package: $DEB"
# Later steps reuse the binary just built.
export SKIP_BUILD=1

CONTENTS="$(dpkg-deb --contents "$DEB")"
expected=(
    ./usr/bin/rustket
    ./usr/share/applications/rustket.desktop
    ./usr/share/metainfo/rustket.metainfo.xml
    ./usr/share/doc/rustket/copyright
    ./usr/share/doc/rustket/changelog.Debian.gz
    ./usr/share/man/man1/rustket.1.gz
)
for size in $ICON_SIZES; do
    expected+=("./usr/share/icons/hicolor/${size}x${size}/apps/rustket.png")
done
for path in "${expected[@]}"; do
    check "contains $path" "grep -q ' $path\$' <<<\"\$CONTENTS\""
done
check "everything owned by root:root" "! awk '{ print \$2 }' <<<\"\$CONTENTS\" | grep -qv '^root/root\$'"
check "binary is executable" "grep -q '^-rwxr-xr-x .* ./usr/bin/rustket\$' <<<\"\$CONTENTS\""
check "nothing outside /usr" "! awk '{ print \$6 }' <<<\"\$CONTENTS\" | grep -qv '^\\./\\(usr\\(/.*\\)\\?\\)\\?\$'"

INFO="$(dpkg-deb --field "$DEB")"
check "Package: rustket" "grep -qx 'Package: rustket' <<<\"\$INFO\""
check "Depends on libc6 with a version" "grep -q '^Depends: .*libc6 (>= [0-9.]*)' <<<\"\$INFO\""
check "Depends on xkbcommon" "grep -q '^Depends: .*libxkbcommon0' <<<\"\$INFO\""
check "no GTK dependency" "! grep -qi 'gtk' <<<\"\$INFO\""

X="$(mktemp -d)"
trap 'rm -rf "$X"' EXIT
dpkg-deb --extract "$DEB" "$X"
D="$X/usr/share/applications/rustket.desktop"
check "desktop: Exec=rustket %u" "grep -qx 'Exec=rustket %u' '$D'"
check "desktop: rocketchat scheme" "grep -qx 'MimeType=x-scheme-handler/rocketchat;' '$D'"
check "desktop: StartupWMClass=rustket" "grep -qx 'StartupWMClass=rustket' '$D'"
check "desktop: categories" "grep -qx 'Categories=Network;InstantMessaging;Chat;' '$D'"
if command -v desktop-file-validate >/dev/null; then
    check "desktop-file-validate" "desktop-file-validate '$D'"
fi
if command -v appstreamcli >/dev/null; then
    # Two known warnings (non-rDNS id, no homepage); errors fail.
    check "appstreamcli: no errors" \
        "! appstreamcli validate --no-net '$X/usr/share/metainfo/rustket.metainfo.xml' | grep -q '^E:'"
fi
V="$("$X/usr/bin/rustket" --version)"
check "packaged binary --version ($V)" "[[ '$V' =~ ^rustket\ [0-9]+\.[0-9]+\.[0-9]+ ]]"
HELP="$("$X/usr/bin/rustket" --help)"
check "packaged binary --help" "grep -q -- '--version' <<<\"\$HELP\""

# Captured first: grep stopping early would SIGPIPE the script (pipefail).
APPIMAGE_OUT="$("$ROOT/packaging/appimage/build.sh")"
echo "$APPIMAGE_OUT"
APPDIR="$(grep -m1 'Rustket.AppDir$' <<<"$APPIMAGE_OUT")"
check "AppDir: AppRun --version" "[ \"\$('$APPDIR/AppRun' --version)\" = '$V' ]"
for f in AppRun rustket.desktop rustket.png .DirIcon usr/bin/rustket \
    usr/share/applications/rustket.desktop usr/share/metainfo/rustket.appdata.xml \
    usr/share/icons/hicolor/256x256/apps/rustket.png; do
    check "AppDir: $f" "[ -e '$APPDIR/$f' ]"
done

if command -v lintian >/dev/null; then
    LINT="$(lintian --tag-display-limit 0 "$DEB" 2>&1 || true)"
    [ -z "$LINT" ] || echo "$LINT"
    check "lintian: no errors" "! grep -q '^E:' <<<\"\$LINT\""
fi

[ "$fail" = 0 ] && echo "all packaging checks passed" || { echo "packaging checks failed"; exit 1; }
