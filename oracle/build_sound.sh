#!/usr/bin/env bash
# Build the sound oracle: id's snd_dma.c + snd_mix.c + snd_mem.c (and mathlib.c's
# VectorNormalize), headless, around our c/snd_oracle.c (a fake DMA driver, the
# engine pieces the mixer calls, and a script runner). Portable C paths (id386 0),
# a static 32-bit i386 binary built in the same container as the renderer oracle.
#
#   oracle/build_sound.sh                 -> oracle/build/snd-oracle      (x87 FPU, like 1996)
#   ORACLE_FPMATH=sse oracle/build_sound.sh -> oracle/build/snd-oracle-sse (SSE2 float math)
#
# id's tree is never touched: the files are copied to oracle/build/snd-src and the
# one edit (quakedef.h's id386 switch, as build.sh makes it) is made on the copy.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SRC=${QUAKE_C_SRC:-quake-c/WinQuake}
OUT=$HERE/build
IMAGE=quake-oracle-cc:bookworm
FPMATH=${ORACLE_FPMATH:-x87}

[ -f "$SRC/snd_mix.c" ] || { echo "id's WinQuake source not found at $SRC (set QUAKE_C_SRC)" >&2; exit 1; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -q -t "$IMAGE" "$HERE/c"

rm -rf "$OUT/snd-src"
mkdir -p "$OUT/snd-src"
cp "$SRC"/*.h "$SRC"/progdefs.q1 "$SRC"/snd_dma.c "$SRC"/snd_mix.c "$SRC"/snd_mem.c "$SRC"/mathlib.c "$OUT/snd-src/"
sed -i 's|^#if defined __i386__ // && !defined __sun__|#if 0 // oracle: id386 0, the portable C paths|' "$OUT/snd-src/quakedef.h"
grep -q 'oracle: id386 0' "$OUT/snd-src/quakedef.h" || { echo "quakedef.h id386 patch did not apply" >&2; exit 1; }
cp "$HERE"/c/snd_oracle.c "$OUT/snd-src/"

CFLAGS="-O2 -g0 -std=gnu89 -fcommon -fno-strict-aliasing -fwrapv -w"
BIN=snd-oracle
if [ "$FPMATH" = sse ]; then
    CFLAGS="$CFLAGS -msse2 -mfpmath=sse"
    BIN=snd-oracle-sse
fi

docker run --rm -u "$(id -u):$(id -g)" -v "$OUT:/w" -w /w/snd-src "$IMAGE" \
    sh -c "gcc $CFLAGS -o /w/$BIN snd_dma.c snd_mix.c snd_mem.c mathlib.c snd_oracle.c -static -Wl,--wrap=rand -lm"
echo "built $OUT/$BIN"
