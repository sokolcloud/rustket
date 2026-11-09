#!/bin/sh
# Type-checks the workspace for Windows and macOS targets from Linux.
#
#   dev/cross-check.sh                    # clippy, all four targets
#   dev/cross-check.sh x86_64-pc-windows-msvc -- --lib   # one target, extra args
#
# Needs the targets' std (`rustup target add <triple>`). C sources (sqlite,
# aws-lc, ring) are not compiled: dev/cross/fakecc and fakear stand in for the
# C toolchain and write empty outputs, which is enough because nothing is
# linked. This catches Rust errors and clippy lints under foreign `cfg`s; real
# builds and tests run in CI on the native runners.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
targets=""
while [ $# -gt 0 ] && [ "$1" != "--" ]; do targets="$targets $1"; shift; done
[ "${1:-}" = "--" ] && shift
[ -n "$targets" ] || targets="x86_64-pc-windows-msvc aarch64-pc-windows-msvc x86_64-apple-darwin aarch64-apple-darwin"
status=0
for t in $targets; do
  tu=$(echo "$t" | tr '-' '_')
  echo "== $t"
  env "CC_$tu=$here/cross/fakecc" "CXX_$tu=$here/cross/fakecc" "AR_$tu=$here/cross/fakear" \
    cargo clippy --workspace --all-targets --target "$t" "$@" -- -D warnings || status=1
done
exit $status
