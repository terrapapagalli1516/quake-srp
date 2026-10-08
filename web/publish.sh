#!/usr/bin/env bash
# The public demo's files: a directory a static host serves as it is, with the
# page, the threads build of the program and id's shareware pak, plus a
# `_headers` file that Cloudflare Pages (and Netlify, which reads the same
# file) turns into the two cross-origin isolation headers and the caching
# below. A host that ignores `_headers` still works: the service worker adds
# the isolation headers after one reload (PLATFORM.md, "Offline and install").
#
#   web/publish.sh OUTDIR [PAK0.PAK]     # the pak defaults to quake-data/ID1/PAK0.PAK
#
# Shareware only: the pak must be id's unmodified shareware pak0.pak (its
# hash is checked), and no files.json is written, so the page offers nothing
# beyond it. id's shareware licence goes beside the pak when it is found next
# to it (`SLICNSE.TXT`, as ci/fetch_shareware.sh leaves it): the licence's
# section 6 lets the shareware be passed on free of charge with the licence
# accompanying it (README.md, "License"). id's original archive, quake106.zip,
# goes out too, unmodified, beside the page: "the Software as a whole" that
# section 6 speaks of (the one found beside the pak's folder, as
# ci/fetch_shareware.sh keeps it; its hash is checked).
#
# OUTDIR must be new or empty; nothing is deleted. Needs cargo with the
# wasm32-wasip1-threads target, and uv.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:?usage: web/publish.sh OUTDIR [PAK0.PAK]}
PAK=${2:-$ROOT/quake-data/ID1/PAK0.PAK}
PAK_SHA256=35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af   # as ci/fetch_shareware.sh
ZIP_SHA256=ec6c9d34b1ae0252ac0066045b6611a7919c2a0d78a3a66d9387a8f597553239   # as ci/fetch_shareware.sh
ZIP=$(dirname "$(dirname "$PAK")")/quake106.zip
if [ -f "$ZIP" ]; then
    echo "$ZIP_SHA256  $ZIP" | sha256sum -c --quiet >/dev/null 2>&1 \
        || { echo "$ZIP is not id's quake106.zip; the public demo serves only that" >&2; exit 1; }
fi

[ -f "$PAK" ] || { echo "no pak at $PAK (ci/fetch_shareware.sh fetches it)" >&2; exit 1; }
echo "$PAK_SHA256  $PAK" | sha256sum -c --quiet >/dev/null 2>&1 \
    || { echo "$PAK is not id's shareware pak0.pak (1.06); the public demo serves only that" >&2; exit 1; }
if [ -e "$OUT" ] && [ -n "$(ls -A "$OUT")" ]; then
    echo "$OUT is not empty: give a new directory" >&2; exit 1
fi

# The program keeps each source file's path for its panic messages: the
# build machine's directories become `quake-srp/...`, and the standard
# library's source (in the toolchain's own directory) becomes `rust/...`.
# RUSTFLAGS replaces quake-wasm/.cargo/config.toml's flags, so its +simd128
# is repeated here.
(cd "$ROOT/quake-wasm" \
    && STD_SRC="$(rustc --print sysroot)/lib/rustlib/src/rust" \
    && RUSTFLAGS="-C target-feature=+simd128 --remap-path-prefix=$ROOT=quake-srp --remap-path-prefix=$STD_SRC=rust" \
        cargo build --release --target wasm32-wasip1-threads)

mkdir -p "$OUT/id1"
OUT=$(cd "$OUT" && pwd)
(cd "$ROOT/web" && uv run --no-project python -c 'import sys, isolated; isolated.copy_page(sys.argv[1])' "$OUT")
cp "$ROOT/quake-wasm/target/wasm32-wasip1-threads/release/quake.wasm" "$OUT/quake.wasm"
if grep -aq -e "$ROOT" -e "$HOME/" "$OUT/quake.wasm"; then
    echo "quake.wasm still names a directory of this machine" >&2; exit 1
fi
cp "$PAK" "$OUT/id1/pak0.pak"
LICENCE=$(dirname "$(dirname "$PAK")")/SLICNSE.TXT
if [ -f "$LICENCE" ]; then
    cp "$LICENCE" "$OUT/id1/slicnse.txt"
else
    echo "note: no SLICNSE.TXT beside the pak's folder; the demo goes out without id's shareware licence" >&2
fi
if [ -f "$ZIP" ]; then
    cp "$ZIP" "$OUT/quake106.zip"
else
    echo "note: no quake106.zip beside the pak's folder; the demo goes out without id's original archive" >&2
fi

# Every response: cross-origin isolation (SharedArrayBuffer, the threads).
# The pak and id's archive never change: cached for a year. The program and the
# page change with each deploy under the same names: revalidated on every
# load (a 304 when nothing changed), so a deploy takes effect at the next
# load. Everything else keeps the host's default.
cat > "$OUT/_headers" <<'HEADERS'
/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp

/id1/pak0.pak
  Cache-Control: public, max-age=31536000, immutable

/quake106.zip
  Cache-Control: public, max-age=31536000, immutable

/quake.wasm
  Cache-Control: no-cache
HEADERS

echo "$OUT:"
(cd "$OUT" && find . -type f | sort | while read -r f; do printf '  %10s  %s\n' "$(stat -c %s "$f")" "${f#./}"; done)
echo "Upload it as it is (Cloudflare Pages: a direct upload of this directory)."
