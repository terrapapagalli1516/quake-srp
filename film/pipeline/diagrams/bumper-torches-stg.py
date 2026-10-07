#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""TORCHES! over e1m3's gable flame (TQ1, over STG's footage, 2.5 s): the bumper, top left.

footage/game/STG.mp4: e1m3's gable flame; over the TQ1 slot (file 7.25-9.75) its body tops at
y ~370 and its embers rise to y 67 in a column at x 908-976 (measured on the frames). So the
title sits in the Classic box's top-left (the box is x 240-1680), scale 10 (640 px; 12 would
cross the embers), its band x 240-900, y 38-174, clear of the flame, its embers and the
ladder's pillar (x 1680-1920).

The string is world.qc's FLICKER (first variety, light style 1), read from the repository's
torch.rs as checked out; each letter runs it at a light style's rate, ten letters a second,
gliding between letters as slop's torches do (GLIDE_STEP 2, as the D12b strip computes it),
three letters further on per character so they flicker apart. The letters are id's
conchars, scale 10 (`--scale N`), centred on a 55% dark band; typed on in 6 frames, faded
out over the last 0.3 s.
"""

import math
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

BOX_X0, BOX_X1, BAND_Y = 240, 900, 106  # the band's left and right ends, its centre line

import numpy as np

_src = (quake.REPO / "quake-rs/src/render/torch.rs").read_text()
FLICKER = re.search(r'const FLICKER_1: &\[u8\] = b"([a-z]+)";', _src).group(1)
assert FLICKER == "mmnmmommommnonmmonqnmmo"
TEXT = "TORCHES!"
SCALE = 10
if "--scale" in sys.argv:
    i = sys.argv.index("--scale")
    SCALE = int(sys.argv[i + 1])
    del sys.argv[i : i + 2]

CUES = Cues(type_on=0.0, out=2.5).argv()
DURATION = duration_arg(2.5)


def value(letters: float) -> int:
    """The flicker string's value at a (fractional) letter, gliding in steps of 2."""
    k = math.floor(letters)
    here = (ord(FLICKER[k % len(FLICKER)]) - 97) * 22
    nxt = (ord(FLICKER[(k + 1) % len(FLICKER)]) - 97) * 22
    x = (nxt - here) * float(np.float32(letters - k)) / 2
    return here + (math.floor(x + 0.5) if x >= 0 else -math.floor(-x + 0.5)) * 2


def frame(c, t):
    c.background()
    a = 1.0 - ramp(t, CUES.out - 0.3, 0.3)
    if a <= 0:
        return
    n = int(round(len(TEXT) * min(1.0, ((t - CUES.type_on) * 60 + 1) / 6))) if t >= CUES.type_on else 0
    w = len(TEXT) * 8 * SCALE
    x0, y0 = (BOX_X0 + BOX_X1 - w) / 2, BAND_Y - 4 * SCALE
    c.rect(BOX_X0, y0 - 28, BOX_X1 - BOX_X0, 8 * SCALE + 56, fill="bg_deep", alpha=0.55 * a)
    for i, ch in enumerate(TEXT[:n]):
        v = value(t * 10 + 3 * i)  # m = 264 is a torch's own light; q = 352 its brightest here
        u = clamp01((v - 264) / 88)
        col = shade(mix("gold", 238, u), 0.75 + 0.6 * u)
        c.text(ch, x0 + i * 8 * SCALE, y0, SCALE, col, a)


if __name__ == "__main__":
    main(frame, DURATION, "bumper-torches-stg", CUES, "e1m1")
