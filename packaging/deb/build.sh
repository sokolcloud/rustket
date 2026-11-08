#!/usr/bin/env bash
# Builds packaging/out/rustket_<version>_<arch>.deb without cargo-deb: compiles
# with the `dist` profile, assembles the package tree and runs
# `dpkg-deb --build --root-owner-group`.
#
#   packaging/deb/build.sh            # build + package
#   SKIP_BUILD=1 packaging/deb/build.sh   # package an existing target/dist/rustket
#   LINT=1 packaging/deb/build.sh     # also run lintian
#
# Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT (output directory),
# DEB_MAINTAINER ("Name <email>"), DEB_REVISION (default 1).
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

command -v dpkg-deb >/dev/null || die "dpkg-deb not found"
build_binary

VERSION="$(binary_version)"
REVISION="${DEB_REVISION:-1}"
ARCH="$(dpkg --print-architecture)"
MAINTAINER="${DEB_MAINTAINER:-Sergey Sokolov <sokol@xi-ix.org>}"
STAGE="$OUT/deb/rustket_${VERSION}-${REVISION}_${ARCH}"
DEB="$OUT/rustket_${VERSION}-${REVISION}_${ARCH}.deb"

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN"
install -Dm755 "$BIN" "$STAGE/usr/bin/rustket"
install -Dm644 "$LINUX/rustket.desktop" "$STAGE/usr/share/applications/rustket.desktop"
install -Dm644 "$LINUX/rustket.metainfo.xml" "$STAGE/usr/share/metainfo/rustket.metainfo.xml"
install_icons "$STAGE/usr/share"

DOC="$STAGE/usr/share/doc/rustket"
install -Dm644 "$ROOT/packaging/deb/copyright" "$DOC/copyright"
third_party_notices "$DOC/THIRD-PARTY-NOTICES"
gzip -9n "$DOC/THIRD-PARTY-NOTICES"
DATE="$(date -R -d "@${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)}")"
cat >"$DOC/changelog.Debian" <<EOF
rustket ($VERSION-$REVISION) unstable; urgency=medium

  * Package built from the Rustket sources (packaging/deb/build.sh).

 -- $MAINTAINER  $DATE
EOF
gzip -9n "$DOC/changelog.Debian"

mkdir -p "$STAGE/usr/share/man/man1"
gzip -9n <"$LINUX/rustket.1" >"$STAGE/usr/share/man/man1/rustket.1.gz"
chmod 644 "$STAGE/usr/share/man/man1/rustket.1.gz"

# The symbol table is kept on purpose (profile.dist strip = "debuginfo") so
# RUST_BACKTRACE=1 crash reports name functions.
install -Dm644 "$ROOT/packaging/deb/lintian-overrides" "$STAGE/usr/share/lintian/overrides/rustket"

# Shared-library dependencies from the binary's NEEDED entries.
shlib_depends() {
    local tmp
    if command -v dpkg-shlibdeps >/dev/null; then
        tmp="$(mktemp -d)"
        mkdir -p "$tmp/debian"
        printf 'Source: rustket\n\nPackage: rustket\nArchitecture: any\n' >"$tmp/debian/control"
        if (cd "$tmp" && dpkg-shlibdeps -O "$STAGE/usr/bin/rustket" 2>/dev/null) |
            sed -n 's/^shlibs:Depends=//p' >"$tmp/out" && [ -s "$tmp/out" ]; then
            cat "$tmp/out"
            rm -rf "$tmp"
            return
        fi
        rm -rf "$tmp"
    fi
    # Fallback: the newest GLIBC symbol version the binary references.
    local glibc
    glibc="$(objdump -T "$STAGE/usr/bin/rustket" | grep -o 'GLIBC_[0-9.]*' |
        sed 's/GLIBC_//' | sort -uV | tail -1)"
    echo "libc6 (>= ${glibc:-2.35}), libgcc-s1"
}

# Libraries loaded with dlopen (not in NEEDED), listed in packaging/deb/depends
# and recommends (keep [package.metadata.deb] in crates/rustket/Cargo.toml in
# sync). winit's X11 backend needs xkbcommon-x11, X11-xcb, Xcursor and Xi to
# open a window at all (it panics without libxkbcommon-x11), and Wayland
# desktops may run it through XWayland. Recommended: Wayland/Vulkan/EGL for the
# GPU renderer (the CPU renderer works without them), libdbus plus a portal
# backend or zenity for the file dialogs (rfd), a sound player and theme for
# notification sounds, xdg-utils for links.
DEPENDS="$(shlib_depends), $(cat "$ROOT/packaging/deb/depends")"
RECOMMENDS="$(cat "$ROOT/packaging/deb/recommends")"
SUGGESTS="$(cat "$ROOT/packaging/deb/suggests")"
INSTALLED_SIZE="$(du -sk --exclude=DEBIAN "$STAGE" | cut -f1)"

cat >"$STAGE/DEBIAN/control" <<EOF
Package: rustket
Version: $VERSION-$REVISION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $INSTALLED_SIZE
Depends: $DEPENDS
Recommends: $RECOMMENDS
Suggests: $SUGGESTS
Section: net
Priority: optional
Description: native Rocket.Chat desktop client
 Rustket is a lightweight Rocket.Chat client written in Rust that renders
 rooms, threads, end-to-end encrypted chats, apps, omnichannel and
 administration natively instead of embedding a web browser.
 .
 Browser sign-in (OAuth, OpenID, SAML, CAS) and video calls use the default
 browser and return through rocketchat:// links, which this package registers.
EOF

(cd "$STAGE" && find usr -type f -print0 | sort -z | xargs -0 md5sum) >"$STAGE/DEBIAN/md5sums"
chmod 644 "$STAGE/DEBIAN/md5sums"

dpkg-deb --build --root-owner-group -Zxz "$STAGE" "$DEB" >/dev/null
echo "$DEB"

if [ "${LINT:-0}" = 1 ]; then
    lintian --info --display-info "$DEB" || true
fi
