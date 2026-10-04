#!/usr/bin/env bash
# What .github/workflows/check.yml runs, on this checkout: both crates'
# formatting, tests and clippy (warnings are errors), and the browser
# program's two builds.
# With --oracle, also oracle.yml's Classic proof (it needs quake-c/WinQuake
# and docker; oracle/README.md).
#
#   ci/local.sh            # check.yml
#   ci/local.sh --oracle   # check.yml, then oracle.yml
#
# It uses the toolchain that is active here and says so when that is not the
# one the workflows pin. Prints PASS only if every step passed.
set -euo pipefail
cd "$(dirname "$0")/.."

pin=$(sed -n 's/^ *RUST_TOOLCHAIN: *"\([^"]*\)".*/\1/p' .github/workflows/check.yml)
have=$(rustc --version)
echo "== $have (check.yml pins $pin)"
case "$have" in "rustc $pin "*) ;; *) echo "   note: not the pinned toolchain; clippy's lints may differ" ;; esac

step() { echo "== $*"; "$@"; }

(cd quake-rs && step cargo fmt --check)
(cd quake-wasm && step cargo fmt --check)
step ci/fetch_shareware.sh
(cd quake-rs && step cargo test --release)
(cd quake-rs && step cargo clippy --release --all-targets -- -D warnings)
(cd quake-wasm && step cargo test --release)
(cd quake-wasm && step cargo clippy --release --all-targets -- -D warnings)
(cd quake-wasm && step cargo clippy --release --target wasm32-wasip1 -- -D warnings)
(cd quake-wasm && step cargo build --release --target wasm32-wasip1)
(cd quake-wasm && step cargo build --release --target wasm32-wasip1-threads)
if [ "${1:-}" = --oracle ]; then
    step uv run oracle/classic_check.py
fi
echo "== PASS"
