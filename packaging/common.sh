# Shared helpers for packaging/deb/build.sh and packaging/appimage/build.sh.
# Sourced, not executed.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LINUX="$ROOT/packaging/linux"
ICONS="$ROOT/crates/rustket/assets/icons"
ICON_SIZES="16 22 24 32 48 64 128 256 512"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="${RUSTKET_PACKAGE_OUT:-$ROOT/packaging/out}"
BIN="$TARGET_DIR/dist/rustket"

die() {
    echo "error: $*" >&2
    exit 1
}

# Builds the binary with the `dist` profile unless SKIP_BUILD=1 (then the
# existing $BIN is used).
build_binary() {
    if [ "${SKIP_BUILD:-0}" != 1 ]; then
        (cd "$ROOT" && cargo build --locked --profile dist -p rustket --bin rustket) ||
            die "cargo build failed"
    fi
    [ -x "$BIN" ] || die "$BIN not found (build with: cargo build --profile dist -p rustket)"
}

# `rustket --version` prints "rustket X.Y.Z".
binary_version() {
    local v
    v="$("$BIN" --version | awk 'NR == 1 { print $2 }')"
    [ -n "$v" ] || die "cannot read the version from $BIN --version"
    echo "$v"
}

# install_icons <dest-share-dir>: hicolor PNGs for every size.
install_icons() {
    local share="$1" size
    for size in $ICON_SIZES; do
        [ -f "$ICONS/rustket-$size.png" ] || die "missing icon $ICONS/rustket-$size.png"
        install -Dm644 "$ICONS/rustket-$size.png" \
            "$share/icons/hicolor/${size}x${size}/apps/rustket.png"
    done
}

# third_party_notices <file>: copyright notices and license texts of every
# crate and asset compiled into the binary (packaging/notices.py).
third_party_notices() {
    command -v python3 >/dev/null || die "python3 is needed for the third-party notices"
    python3 -I "$ROOT/packaging/notices.py" >"$1" || die "packaging/notices.py failed"
    chmod 644 "$1"
}
