#!/usr/bin/env bash
# Re-derive oracle/README.md's numbers: the headline matrix, the attribution
# ladder (id's renderer made to drop one known difference at a time), and one
# crop per discrepancy class. ~20 s. Rerun it after a fidelity fix and watch the
# rows move.
#
#   oracle/characterise.sh [outdir]            # everything lands in outdir (default: a temp dir)
#   oracle/characterise.sh --update-crops      # also refresh the committed oracle/crops/*.png
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
UPDATE=0
if [ "${1:-}" = --update-crops ]; then UPDATE=1; shift; fi
OUT=${1:-$(mktemp -d -t quake-oracle-char-XXXXXX)}
mkdir -p "$OUT"
cmp() { uv run -q "$HERE/compare.py" "$@"; }
rows() { grep -E '^(e1m|case|entity)' || true; }

echo "== headline: id's x86 spans (D_DrawSpans16, its own mip levels) vs the port, first frame after signon"
cmp --spans 16 --out "$OUT/headline" | rows
cmp --spans 16 --res 640x480 --out "$OUT/headline640" | rows
cmp --spans 16 --res 640x400 --aspect 0.8333333 --modes world --out "$OUT/headline_page" | rows
echo "== id's portable C as written (D_DrawSpans8, the default)"
cmp --modes world --out "$OUT/headline8" | rows

echo
echo "== attribution ladder, world only, 320x200: exact% as id's renderer drops one difference at a time"
ladder() { # label, compare args...
    local label=$1; shift
    printf '%-44s' "$label"
    cmp --modes world "$@" --out "$OUT/ladder" | awk '/^e1m/ {printf " %s %6s", substr($1,1,4), $2}'
    echo
}
ladder "id's C as shipped in source (spans 8)"
ladder "16-px segments (the x86 asm, d_subdiv16 1)" --spans 16
ladder "exact per-pixel perspective" --spans 1
ladder "mip 0 forced (d_mipscale 0)" --c-cmd "d_mipscale 0"
ladder "mip 0 + 16-px segments" --spans 16 --c-cmd "d_mipscale 0"
ladder "mip 0 + exact perspective (the port's too)" --spans 1 --exactpersp --c-cmd "d_mipscale 0"

echo
echo "== crops (C | port | diff), each with the classes it does not show removed (both renderers)"
EXACT=(--spans 1 --exactpersp --c-cmd "d_mipscale 0")
crop() { # name, crop box, compare args...
    local name=$1 box=$2; shift 2
    cmp --crop "$name:$box" --out "$OUT/crop_$name" "$@" | awk -v n="$name" '/^e1m/ {printf "%-14s %s exact %s%%\n", n, $1, $2}'
    local png
    png=$(ls "$OUT/crop_$name"/*."$name".png | head -1)
    cp "$png" "$OUT/$name.png"
    if [ $UPDATE = 1 ]; then mkdir -p "$HERE/crops" && cp "$png" "$HERE/crops/$name.png"; fi
}
crop mip        60,5,120,60   --maps e1m3 --modes world
crop spans      0,30,60,120   --maps e1m7 --modes world --c-cmd "d_mipscale 0"
crop lightmap   0,100,70,50   --maps e1m1 --modes world "${EXACT[@]}"
crop sky        125,0,80,40   --maps e1m2 --modes world "${EXACT[@]}"
crop pools      105,125,95,25 --maps e1m2 --modes world "${EXACT[@]}"
crop liquid     100,60,120,60 --maps e1m1 --modes world "${EXACT[@]}" --view 205,1150,-230,40,90,0
crop alias      62,68,45,50   --maps e1m2 --modes ents "${EXACT[@]}" --view 1432.386,1397.978,233.254,9.344,-103.449,0
crop fullbright 30,30,125,25  --maps e1m2 --modes ents "${EXACT[@]}" --view 1432.386,1397.978,233.254,9.344,-103.449,0
crop viewmodel  110,110,100,90 --maps e1m1 --modes world --viewmodel --settle 3
echo
echo "all output in $OUT"
