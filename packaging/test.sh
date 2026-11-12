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
    ./usr/share/doc/rustket/THIRD-PARTY-NOTICES.gz
    ./usr/share/lintian/overrides/rustket
    ./usr/share/man/man1/rustket.1.gz
    ./usr/lib/rustket/install-kind
)
for size in $ICON_SIZES; do
    expected+=("./usr/share/icons/hicolor/${size}x${size}/apps/rustket.png")
done
for path in "${expected[@]}"; do
    check "contains $path" "grep -q ' $path\$' <<<\"\$CONTENTS\""
done
# Negations use here-strings: `! a | grep -q` passes whenever `a` fails
# (pipefail), or when grep stopping early makes it die of SIGPIPE.
OWNERS="$(awk '{ print $2 }' <<<"$CONTENTS")"
PATHS="$(awk '{ print $6 }' <<<"$CONTENTS")"
check "everything owned by root:root" "! grep -qv '^root/root\$' <<<\"\$OWNERS\""
check "binary is executable" "grep -q '^-rwxr-xr-x .* ./usr/bin/rustket\$' <<<\"\$CONTENTS\""
check "nothing outside /usr" "! grep -qv '^\\./\\(usr\\(/.*\\)\\?\\)\\?\$' <<<\"\$PATHS\""

INFO="$(dpkg-deb --field "$DEB")"
check "Package: rustket" "grep -qx 'Package: rustket' <<<\"\$INFO\""
check "Depends on libc6 with a version" "grep -q '^Depends: .*libc6 (>= [0-9.]*)' <<<\"\$INFO\""
for lib in libxkbcommon0 libxkbcommon-x11-0 libx11-6 libx11-xcb1 libxcursor1 libxi6; do
    check "Depends on $lib" "grep -q '^Depends: .*\\b$lib\\b' <<<\"\$INFO\""
done
check "Recommends libdbus (file dialogs)" "grep -q '^Recommends: .*libdbus-1-3' <<<\"\$INFO\""
check "no GTK dependency" "! grep -qi '^Depends:.*gtk' <<<\"\$INFO\""

X="$(mktemp -d)"
trap 'rm -rf "$X"' EXIT
dpkg-deb --extract "$DEB" "$X"
check "deb: install-kind marker" "[ \"\$(cat '$X/usr/lib/rustket/install-kind')\" = deb ]"
D="$X/usr/share/applications/rustket.desktop"
check "desktop: Exec=rustket %u" "grep -qx 'Exec=rustket %u' '$D'"
check "desktop: rocketchat scheme" "grep -qx 'MimeType=x-scheme-handler/rocketchat;' '$D'"
check "desktop: StartupWMClass=rustket" "grep -qx 'StartupWMClass=rustket' '$D'"
check "desktop: no StartupNotify" "! grep -q '^StartupNotify=true' '$D'"
check "desktop: categories" "grep -qx 'Categories=Network;InstantMessaging;Chat;' '$D'"
if command -v desktop-file-validate >/dev/null; then
    check "desktop-file-validate" "desktop-file-validate '$D'"
fi
if command -v appstreamcli >/dev/null; then
    # Two known warnings (non-rDNS id, no homepage); errors fail.
    # It exits non-zero on warnings too, so only the output decides.
    AS="$(appstreamcli validate --no-net "$X/usr/share/metainfo/rustket.metainfo.xml" 2>&1 || true)"
    check "appstreamcli: ran" "grep -q 'Validation' <<<\"\$AS\""
    check "appstreamcli: no errors" "! grep -q '^E:' <<<\"\$AS\""
fi
N="$(zcat "$X/usr/share/doc/rustket/THIRD-PARTY-NOTICES.gz")"
check "notices: Noto Sans OFL" "grep -q 'SIL Open Font License' <<<\"\$N\""
check "notices: Lucide" "grep -q 'Copyright (c) 2013-2022 Cole Bemis' <<<\"\$N\""
check "notices: crate license texts" "grep -q 'Apache License' <<<\"\$N\" && grep -q '^serde [0-9.]* |' <<<\"\$N\""
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
    usr/share/icons/hicolor/256x256/apps/rustket.png \
    usr/share/doc/rustket/THIRD-PARTY-NOTICES usr/share/doc/rustket/copyright \
    usr/lib/rustket/install-kind; do
    check "AppDir: $f" "[ -e '$APPDIR/$f' ]"
done
check "AppDir: install-kind marker" "[ \"\$(cat '$APPDIR/usr/lib/rustket/install-kind')\" = appimage ]"

if command -v lintian >/dev/null; then
    LINT="$(lintian --tag-display-limit 0 "$DEB" 2>&1 || true)"
    [ -z "$LINT" ] || echo "$LINT"
    check "lintian: no errors" "! grep -q '^E:' <<<\"\$LINT\""
fi

[ "$fail" = 0 ] && echo "all packaging checks passed" || { echo "packaging checks failed"; exit 1; }
