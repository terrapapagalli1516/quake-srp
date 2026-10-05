#!/usr/bin/env bash
# Build the oracle: id's WinQuake software renderer, headless (null sound/cd/
# input/net drivers; our vid_oracle.c, sys_oracle.c, oracle.c and
# walk_oracle.c), portable C paths only (id386 0: no assembly), as a static
# 32-bit i386 binary that runs directly on the x86_64 host.
#
#   oracle/build.sh            -> oracle/build/quake-oracle      (x87 FPU, like 1996)
#   ORACLE_FPMATH=sse oracle/build.sh -> oracle/build/quake-oracle-sse (SSE2 float math)
#
# id's tree is never touched: it is copied to oracle/build/src and the one
# edit (quakedef.h's id386 switch) is made on the copy.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SRC=${QUAKE_C_SRC:-$HERE/../quake-c/WinQuake}
OUT=$HERE/build
IMAGE=quake-oracle-cc:bookworm
FPMATH=${ORACLE_FPMATH:-x87}

[ -f "$SRC/r_main.c" ] || { echo "id's WinQuake source not found at $SRC (set QUAKE_C_SRC)" >&2; exit 1; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -q -t "$IMAGE" "$HERE/c"

rm -rf "$OUT/src"
mkdir -p "$OUT/src"
cp "$SRC"/*.c "$SRC"/*.h "$SRC"/progdefs.q1 "$OUT/src/"
# the only edit to id's code: take the portable C paths (nonintel.c route)
sed -i 's|^#if defined __i386__ // && !defined __sun__|#if 0 // oracle: id386 0, the portable C paths|' "$OUT/src/quakedef.h"
grep -q 'oracle: id386 0' "$OUT/src/quakedef.h" || { echo "quakedef.h id386 patch did not apply" >&2; exit 1; }
cp "$HERE"/c/*.c "$OUT/src/"

# Makefile.linuxi386's SQUAKE_OBJS C files, with the platform drivers swapped
# for the null ones (net_none = loopback only) and ours.
FILES="cl_demo cl_input cl_main cl_parse cl_tent chase cmd common console crc cvar
draw d_edge d_fill d_init d_modech d_part d_polyse d_scan d_sky d_sprite d_surf
d_vars d_zpoint host host_cmd keys menu mathlib model net_loop net_main net_vcr
net_none nonintel pr_cmds pr_edict pr_exec r_aclip r_alias r_bsp r_light r_draw
r_efrag r_edge r_misc r_main r_sky r_sprite r_surf r_part r_vars screen sbar
sv_main sv_phys sv_move sv_user zone view wad world cd_null in_null snd_null
sys_oracle vid_oracle oracle walk_oracle"
SRCS=$(for f in $FILES; do printf '%s.c ' "$f"; done)
# walk_oracle.c drives CL_SendCmd and logs the sound layer's entry points
# (snd_null's) and the server's SV_StartSound; sys_oracle.c times
# SV_SpawnServer (-oracle_loadtime)
WRAPS=$(for f in S_PrecacheSound S_StartSound S_StopSound S_StopAllSounds S_StaticSound S_LocalSound \
        SV_StartSound CL_SendCmd SV_SpawnServer; do printf -- '-Wl,--wrap=%s ' "$f"; done)

CFLAGS="-O2 -g0 -std=gnu89 -fcommon -fno-strict-aliasing -fwrapv -w"
BIN=quake-oracle
if [ "$FPMATH" = sse ]; then
    CFLAGS="$CFLAGS -msse2 -mfpmath=sse"
    BIN=quake-oracle-sse
fi

docker run --rm -u "$(id -u):$(id -g)" -v "$OUT:/w" -w /w/src "$IMAGE" \
    sh -c "gcc $CFLAGS -o /w/$BIN $SRCS -static -Wl,--wrap=R_RenderView -Wl,--wrap=D_DrawSpans8 -Wl,--wrap=Turbulent8 $WRAPS -lm && gcc --version | head -1"
echo "built $OUT/$BIN"
