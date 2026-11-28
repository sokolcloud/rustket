#!/usr/bin/env bash
# Builds the release asset Rustket-<version>-policy-templates.zip: the
# templates for administrators (packaging/policy/, generated from the key
# registry, docs/policy.md section 14), their reference docs/policies.md and
# the inventory and compliance scripts (packaging/policy-scripts/, static, in
# scripts/).
# Uses only checked-in files: no cargo build. Prints the archive's path.
#
#   packaging/policy-templates.sh [output directory]   # default packaging/out
set -euo pipefail
umask 022
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

command -v zip >/dev/null || die "zip not found"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || die "cannot read the version from Cargo.toml"
DEST="${1:-$OUT}"
mkdir -p "$DEST"
ZIP="$(cd "$DEST" && pwd)/Rustket-$VERSION-policy-templates.zip"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cp -R "$ROOT/packaging/policy/." "$TMP/"
cp "$ROOT/docs/policies.md" "$TMP/policies.md"
mkdir "$TMP/scripts"
cp "$ROOT/packaging/policy-scripts/rustket-policy-check.ps1" "$ROOT/packaging/policy-scripts/rustket-policy-check.sh" "$TMP/scripts/"
chmod 755 "$TMP/scripts/rustket-policy-check.sh"
rm -f "$ZIP"
# Sorted names, no extra attributes and no directory entries.
(cd "$TMP" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | zip -q -X -D "$ZIP" -@)
echo "$ZIP"
