#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D03, the oracle as a band across the top (S20, 4.7 s), over S21's two panels.

S21's frame has id's C on the left (x 60-940) and the port on the right (x 980-1860),
from y 84. The band, 1920x360, feeds them: on the left, id's C files -> gcc with the null
drivers and a clock of 0.1 s steps -> "id's C, the oracle" -> down into the left panel; on
the right, the port's Rust files -> cargo build -> "the port, quaketool view" -> down into
the right panel. In the middle, the card both are given, headed THE HARNESS (id's C is the
oracle; the harness gives both the same camera, clock and entities: oracle/README.md,
"renders the same view, clock and entities"); at the end "= pixel by pixel" between the
panels. S21 shows e1m1, so the card names no case's numbers.

The C names are WinQuake files the docs name; the Rust names are files in the
repository's quake-rs/src/render/ and quake-rs/src/server/, checked below as checked out.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

# ------------------------------------------------------------ the facts ----

C_FILES = ["r_main.c", "d_scan.c", "r_light.c", "snd_mix.c", "sv_phys.c"]
RS_FILES = ["raster.rs", "surf.rs", "edge.rs", "lightstyle.rs", "sv_phys.rs"]
SRC = quake.REPO / "quake-rs/src"
for f in RS_FILES:
    assert (SRC / "render" / f).exists() or (SRC / "server" / f).exists(), f

# ----------------------------------------------------------------- time ----

CUES = Cues(card=0.0, left=0.5, right=1.3, feed=2.2, down=2.7, compare=3.3).argv()
DURATION = duration_arg(4.7)

# --------------------------------------------------------------- layout ----

BAND_H = 360
LEFT_PANEL_X, RIGHT_PANEL_X = 500, 1420  # S21's panel centres
BOX_W, BOX_H, BOX_Y = 260, 108, 70
LBOX_X, RBOX_X = 410, 1250
CARD = (736, 26, 448, 214)
CARD_TITLE = "THE HARNESS"


def glyph_camera(c, x, y, a, col="slate"):
    c.rect(x, y - 12, 30, 24, stroke=col, lw=3, alpha=a, radius=3)
    c.poly([(x + 30, y), (x + 44, y - 10), (x + 44, y + 10)], fill=col, alpha=a)
    c.circle(x + 15, y, 6, stroke=col, lw=2, alpha=a)


def glyph_clock(c, x, y, a, col="slate"):
    c.circle(x + 18, y, 15, stroke=col, lw=3, alpha=a)
    c.line(x + 18, y, x + 18, y - 10, col, 3, a)
    c.line(x + 18, y, x + 26, y + 4, col, 3, a)


def glyph_list(c, x, y, a, col="slate"):
    for k in (-10, 0, 10):
        c.circle(x + 6, y + k, 3, fill=col, alpha=a)
        c.line(x + 16, y + k, x + 40, y + k, col, 3, a)


def lane(c, t, t0, files, file_col, arrow_lab, box_x, box_lines, box_col, side):
    """Files -> build arrow -> box; side 'left' flows right, 'right' flows left."""
    files_x = 40 if side == "left" else W - 40
    for i, f in enumerate(files):
        a = stagger(t, t0, i, 0.06, 0.2)
        c.text(f, files_x, 46 + 32 * i, 2, file_col, a, side)
    ax0, ax1 = (220, box_x - 16) if side == "left" else (W - 220, box_x + BOX_W + 16)
    yc = BOX_Y + BOX_H / 2
    d = ramp(t, t0 + 0.25, 0.35, "inout")
    c.arrow(ax0, yc, ax1, yc, "axis", 4, 18, 1.0 if d > 0 else 0, draw=d)
    la = ramp(t, t0 + 0.35, 0.3)
    for i, s in enumerate(arrow_lab):
        c.text(s, (ax0 + ax1) / 2 + (-30 if side == "left" else 30), 206 + 26 * i, 2, "dim", la, "center")
    b = ramp(t, t0 + 0.55, 0.25, "out")
    if b > 0:
        c.rect(box_x, BOX_Y, BOX_W, BOX_H, fill="panel", stroke=box_col, lw=4, alpha=b, radius=6)
        c.text(box_lines[0], box_x + BOX_W / 2, BOX_Y + 34, 3, "text", b, "center", "middle")
        c.text(box_lines[1], box_x + BOX_W / 2, BOX_Y + 76, 2, box_col, b, "center", "middle")


def frame(c, t):
    c.background()
    a = fade(t, 0.0, d_in=0.3)
    # the band, darkest at the top
    c.rect(0, 0, W, BAND_H, fill="bg_deep", alpha=a * 0.8)
    c.line(0, BAND_H, W, BAND_H, "rule", 2, a * 0.8)

    # the card in the middle
    ca = ramp(t, CUES.card, 0.4, "out")
    x, y, w, h = CARD
    c.rect(x, y, w, h, fill="panel", stroke="slate", lw=3, alpha=ca, radius=6)
    c.text(CARD_TITLE, x + w / 2, y + 22, 2, "gold", ca, "center")
    rows = [(glyph_camera, "SAME CAMERA"), (glyph_clock, "SAME CLOCK"), (glyph_list, "SAME ENTITIES")]
    for i, (g, s) in enumerate(rows):
        ra = ca * ramp(t, CUES.card + 0.15 + 0.1 * i, 0.25)
        ry = y + 80 + 44 * i
        g(c, x + 36, ry, ra)
        c.text(s, x + 100, ry, 3, "text", ra, "left", "middle")

    lane(c, t, CUES.left, C_FILES, "rust", ["gcc · null drivers", "a clock of 0.1 s steps"], LBOX_X,
         ["id's C", "the oracle"], "rust", "left")
    lane(c, t, CUES.right, RS_FILES, "slime_light", ["cargo build"], RBOX_X, ["the port", "quaketool view"],
         "slime", "right")

    # the card feeds both boxes
    fa = ramp(t, CUES.feed, 0.4, "inout")
    yc = BOX_Y + BOX_H / 2
    c.arrow(x, yc, LBOX_X + BOX_W + 4, yc, "slate", 4, 16, 1.0 if fa > 0 else 0, draw=fa)
    c.arrow(x + w, yc, RBOX_X - 4, yc, "slate", 4, 16, 1.0 if fa > 0 else 0, draw=fa)

    # down into the panels below
    da = ramp(t, CUES.down, 0.45, "inout")
    for bx, px in ((LBOX_X, LEFT_PANEL_X), (RBOX_X, RIGHT_PANEL_X)):
        x0 = bx + BOX_W / 2
        c.polyline([(x0, BOX_Y + BOX_H), (x0, 250), (px, 250), (px, BAND_H + 6)], "text", 4, 1.0 if da > 0 else 0,
                   draw=da)
        if da >= 1:
            c.arrow(px, 300, px, BAND_H + 30, "text", 4, 20)

    # = pixel by pixel, between the panels
    qa = ramp(t, CUES.compare, 0.35, "back")
    if qa > 0:
        q = min(1.0, qa)
        c.rect(W / 2 - 40, 264, 80, 70, fill="panel", stroke="text", lw=3, alpha=q, radius=6)
        c.text("=", W / 2, 299, 6, "white", q, "center", "middle")
        c.text("PIXEL BY PIXEL", W / 2 - 64, 299, 3, "text", q, "right", "middle")


if __name__ == "__main__":
    main(frame, DURATION, "D03-oracle-band", CUES, "e1m1")
