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
{
    echo "Rust crates compiled into /usr/bin/rustket (name version | license):"
    echo
    third_party_licenses
} >"$DOC/THIRD-PARTY"
chmod 644 "$DOC/THIRD-PARTY"
DATE="$(date -R -d "@${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)}")"
cat >"$DOC/changelog.Debian" <<EOF
rustket ($VERSION-$REVISION) unstable; urgency=medium

  * Package built from the Rustket sources (packaging/deb/build.sh).

 -- $MAINTAINER  $DATE
EOF
gzip -9n "$DOC/changelog.Debian"

mkdir -p "$STAGE/usr/share/man/man1"
sed "s/@VERSION@/$VERSION/" "$LINUX/rustket.1" | gzip -9n >"$STAGE/usr/share/man/man1/rustket.1.gz"
chmod 644 "$STAGE/usr/share/man/man1/rustket.1.gz"

# The symbol table is kept on purpose (profile.dist strip = "debuginfo") so
# RUST_BACKTRACE=1 crash reports name functions.
mkdir -p "$STAGE/usr/share/lintian/overrides"
cat >"$STAGE/usr/share/lintian/overrides/rustket" <<'EOF'
# Symbols are kept for readable crash backtraces (RUST_BACKTRACE=1).
rustket: unstripped-binary-or-object [usr/bin/rustket]
EOF
chmod 644 "$STAGE/usr/share/lintian/overrides/rustket"

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

# Libraries the toolkit loads with dlopen (not in NEEDED): xkbcommon for the
# keyboard, X11 or Wayland for windows, Vulkan/EGL for the GPU renderer (the CPU
# renderer works without them).
DEPENDS="$(shlib_depends), libxkbcommon0, libx11-6 | libwayland-client0"
RECOMMENDS="libxkbcommon-x11-0, libx11-xcb1, libxcursor1, libxi6, libwayland-client0, libwayland-egl1, libegl1, libvulkan1, xdg-utils"
SUGGESTS="gnome-keyring | kwalletmanager | keepassxc"
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
