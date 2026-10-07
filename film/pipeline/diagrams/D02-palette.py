#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D02, the palette (S12, 3.0 s): every pixel is one of 256 colours.

The frame: e1m1's start at S10's camera, classic-1996 (`quaketool view` of
maps/e1m1.bsp at 320x200, eye 480,-352,110, angles 0,90,0, `--aspect 0.8333333`,
id's 16:10 mode on a 4:3 screen), the world alone, shown 4:3 (1440x1080).
Its 64,000 pixels fly to their entries of id's palette (`gfx/palette.lmp` from
the shareware pak), cell i at column i mod 16, row i div 16.

The PPM carries colours, not indices: each is mapped back to the palette.
Twelve palette colours appear twice; the frame's world is drawn through
`gfx/colormap.lmp`, which only ever holds the lower index of each such pair
(checked below for every such colour in the frame), so the lower index is the one the renderer wrote.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # qkit/

import numpy as np

from qkit import *
from qkit import quake

# ------------------------------------------------------------ the facts ----

FRAME_RGB = quake.game_frame("view", "e1m1", "--res", "320x200", "--origin", "480,-352,110", "--angles", "0,90,0",
                             "--aspect", "0.8333333")
PAL = quake.palette().astype(np.int64)
_key = lambda a: (a[..., 0] << 16) | (a[..., 1] << 8) | a[..., 2]
_lut = {}
for _i in range(255, -1, -1):  # the lower index wins
    _lut[int(_key(PAL[_i]))] = _i
IDX = np.vectorize(lambda v: _lut[int(v)])(_key(FRAME_RGB.astype(np.int64)))  # (200, 320)
_cmap = set(np.unique(np.frombuffer(quake.pak_file("gfx/colormap.lmp"), np.uint8)[: 64 * 256]).tolist())
for _a in np.unique(IDX):  # each colour the frame shows that the palette holds twice
    for _b in range(_a + 1, 256):
        if (PAL[_a] == PAL[_b]).all():
            assert _b not in _cmap, f"colormap uses the upper duplicate {_b} of {_a}"
USED = np.zeros(256, bool)
USED[np.unique(IDX)] = True
FH, FW = IDX.shape

# ----------------------------------------------------------------- time ----

CUES = Cues(fly=0.3, land=1.8, label=1.85, out=3.0).argv()  # T0 below reads them at import
DURATION = 3.0
FLIGHT = 0.85  # each pixel's flight; starts spread over fly .. land - FLIGHT

# --------------------------------------------------------------- layout ----

BOX_X, BOX_W, BOX_H = 240, 1440, 1080  # the 4:3 frame
PX_W, PX_H = BOX_W / FW, BOX_H / FH  # 4.5 x 5.4
CELL = 40
GX, GY = (W - 16 * CELL) / 2, (H - 16 * CELL) / 2 + 30  # the 640x640 grid

rng = np.random.default_rng(1996)
ys, xs = np.mgrid[0:FH, 0:FW]
ys, xs = ys.ravel(), xs.ravel()
IDXF = IDX.ravel()
SX = BOX_X + xs * PX_W  # start: the pixel's place
SY = ys * PX_H
CX = GX + (IDXF % 16) * CELL  # end: a point inside its cell (they pile up)
CY = GY + (IDXF // 16) * CELL
EX = CX + rng.uniform(4, CELL - 8, len(IDXF))
EY = CY + rng.uniform(4, CELL - 8, len(IDXF))
# start times: a swarm, the top of the frame a little first
T0 = CUES.fly + (CUES.land - FLIGHT - CUES.fly) * np.clip(0.75 * rng.random(len(IDXF)) + 0.25 * ys / FH, 0, 1)
# a cell fills when its first pixel lands
FIRST = np.full(256, np.inf)
np.minimum.at(FIRST, IDXF, T0 + FLIGHT)
COLS = PAL / 255.0


def inout(u):
    return np.where(u < 0.5, 4 * u**3, 1 - (-2 * u + 2) ** 3 / 2)


def frame(c, t):
    c.background()
    ctx = c.ctx
    u = np.clip((t - T0) / FLIGHT, 0, 1)
    home = u <= 0
    # the pixels still at home: the frame, with holes where pixels have left
    if home.any():
        rgba = np.dstack([FRAME_RGB, np.where(home.reshape(FH, FW), 255, 0).astype(np.uint8)])
        surf = surface_from_array(rgba)
        ctx.save()
        ctx.translate(BOX_X, 0)
        ctx.scale(PX_W, PX_H)
        ctx.set_source_surface(surf, 0, 0)
        import cairo

        ctx.get_source().set_filter(cairo.FILTER_NEAREST)
        ctx.paint()
        ctx.restore()

    # the grid: outlines, then each cell once its first pixel has landed
    ga = fade(t, CUES.fly, d_in=0.4)
    dim = ramp(t, CUES.land, 0.5, "smooth")
    for i in range(256):
        x, y = GX + (i % 16) * CELL, GY + (i // 16) * CELL
        if t >= FIRST[i]:
            c.rect(x + 1, y + 1, CELL - 2, CELL - 2, fill=tuple(COLS[i]), alpha=1.0)
        elif dim > 0:  # a colour this frame does not use: 40%, and a little smaller
            ins = 1 + 6 * dim
            c.rect(x + ins, y + ins, CELL - 2 * ins, CELL - 2 * ins, fill=tuple(COLS[i]), alpha=0.4 * dim)
        c.rect(x + 0.5, y + 0.5, CELL - 1, CELL - 1, stroke="grid", lw=1, alpha=ga * 0.8)

    # the pixels in flight
    fl = (u > 0) & (u < 1)
    if fl.any():
        e = inout(u[fl])
        x = SX[fl] + (EX[fl] - SX[fl]) * e
        y = SY[fl] + (EY[fl] - SY[fl]) * e
        w = PX_W + (4 - PX_W) * e
        h = PX_H + (4 - PX_H) * e
        idx = IDXF[fl]
        order = np.argsort(idx, kind="stable")
        x, y, w, h, idx = x[order], y[order], w[order], h[order], idx[order]
        cuts = np.flatnonzero(np.diff(idx)) + 1
        for a, b in zip(np.r_[0, cuts], np.r_[cuts, len(idx)]):
            ctx.set_source_rgb(*COLS[idx[a]])
            for k in range(a, b):
                ctx.rectangle(x[k], y[k], w[k], h[k])
            ctx.fill()

    # the labels
    la = ramp(t, CUES.label, 0.4, "out")
    c.bignum("256", W / 2, GY - 30, 3, alpha=la, align="center", valign="bottom")
    c.text("gfx/palette.lmp", W / 2, GY + 16 * CELL + 28, 3, "gold", la, "center")


if __name__ == "__main__":
    run(frame, duration=DURATION, name="D02-palette", cues=CUES)
