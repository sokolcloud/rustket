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

# third_party_licenses: "name version license" of every crate linked into the
# binary (normal dependencies only).
third_party_licenses() {
    local list
    if ! list="$(cd "$ROOT" && cargo tree --locked -p rustket -e normal --prefix none \
        --format '{p} | {l}' 2>/dev/null)"; then
        echo "(cargo not available when packaging; see Cargo.lock)"
        return
    fi
    sed -e 's/ (\*)//' -e 's/ (proc-macro)//' <<<"$list" | grep -v '(/' | sort -u
}
