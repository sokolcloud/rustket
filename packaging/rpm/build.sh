#!/usr/bin/env bash
# Builds packaging/out/rustket-<version>-<release>.<arch>.rpm: compiles with
# the `dist` profile, stages the same file layout as the .deb and wraps it with
# rpmbuild (packaging/rpm/rustket.spec). Also works on Debian/Ubuntu
# (`apt-get install rpm`).
#
#   packaging/rpm/build.sh
#   SKIP_BUILD=1 packaging/rpm/build.sh   # package an existing target/dist/rustket
#
# Environment: CARGO_TARGET_DIR, RUSTKET_PACKAGE_OUT, RPM_RELEASE (default 1).
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

command -v rpmbuild >/dev/null || die "rpmbuild not found (Debian/Ubuntu: apt-get install rpm)"
build_binary

VERSION="$(binary_version)"
# RPM versions cannot contain '-'; '~' sorts a pre-release before the release.
RPM_VERSION="${VERSION//-/\~}"
RELEASE="${RPM_RELEASE:-1}"
ARCH="$(uname -m)"
TOP="$OUT/rpm"
STAGE="$TOP/stage"
RPM="$OUT/rustket-${RPM_VERSION}-${RELEASE}.${ARCH}.rpm"

rm -rf "$TOP"
mkdir -p "$STAGE"
install -Dm755 "$BIN" "$STAGE/usr/bin/rustket"
install -Dm644 "$LINUX/rustket.desktop" "$STAGE/usr/share/applications/rustket.desktop"
install -Dm644 "$LINUX/rustket.metainfo.xml" "$STAGE/usr/share/metainfo/rustket.metainfo.xml"
install_icons "$STAGE/usr/share"
mkdir -p "$STAGE/usr/share/man/man1"
gzip -9n <"$LINUX/rustket.1" >"$STAGE/usr/share/man/man1/rustket.1.gz"
chmod 644 "$STAGE/usr/share/man/man1/rustket.1.gz"
install -Dm644 "$ROOT/LICENSE" "$STAGE/usr/share/licenses/rustket/LICENSE"
mkdir -p "$STAGE/usr/share/doc/rustket"
third_party_notices "$STAGE/usr/share/doc/rustket/THIRD-PARTY-NOTICES"

rm -f "$RPM"
rpmbuild -bb --quiet --target "$ARCH" \
    --define "_topdir $TOP" \
    --define "_rpmdir $OUT" \
    --define "_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" \
    --define "rustket_version $RPM_VERSION" \
    --define "rustket_release $RELEASE" \
    --define "rustket_stage $STAGE" \
    "$ROOT/packaging/rpm/rustket.spec"
[ -f "$RPM" ] || die "rpmbuild did not produce $RPM"
echo "$RPM"
