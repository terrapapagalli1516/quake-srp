#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D01, fixed point (S11, 1.6 s): a texel coordinate as a 16.16 integer, and `>> 16`.

The facts: id's span loops keep texel coordinates as 16.16 fixed-point integers
(16 bits of whole texels, 16 of fraction), and the texel is the top half:
`quake-rs/src/render/raster.rs`, `span_exact_reference`:
`let s = ((sz * z) as i64).wrapping_add(fx.sadjust) >> 16;`. PERF_PLAN.md §15
speaks of "fixed point (16.16 with 7 more bits)". The value 37.25 is schematic:
its bits are computed here (37.25 x 65536 = 0x00254000), not typed in.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # qkit/

from qkit import *

# ------------------------------------------------------------ the facts ----

VALUE = 37.25  # schematic
FIXED = int(round(VALUE * 65536))  # 16.16
assert FIXED == 0x00254000
BITS = [(FIXED >> (31 - i)) & 1 for i in range(32)]  # most significant first
TEXEL = FIXED >> 16
assert TEXEL == 37

# ----------------------------------------------------------------- time ----

CUES = Cues(fill=0.0, rshift=0.62, texel=1.2)
DURATION = 1.6

# --------------------------------------------------------------- layout ----

CW, CH = 40, 56  # a bit's cell
NIB, HALF = 6, 14  # extra gap after every 4 bits, and between the halves
ROW_W = 32 * CW + 7 * NIB + HALF
X0 = (W - ROW_W) / 2
Y0 = 540 - CH / 2
BROWN, SLATE = 26, 36  # the halves' colours (palette indices)


def cx(i: float) -> float:
    """Left edge of cell i (fractional i: between cells, for the shift)."""
    def edge(k: int) -> float:
        return X0 + k * CW + (k // 4) * NIB + (k // 16) * HALF
    k = int(i // 1)
    u = i - k
    return edge(k) + (edge(k + 1) - edge(k)) * u if k < 40 else edge(k)


def draw_cell(c, x, bit, rim, a, filled):
    """One bit: a rim in its half's colour, the inside yellow (1) or dark (0)."""
    c.rect(x + 1, Y0, CW - 2, CH, fill=rim, alpha=a)
    if filled <= 0:
        return
    inner = 111 if bit else 16
    c.rect(x + 5, Y0 + 5, CW - 10, CH - 10, fill=inner, alpha=a * filled)
    c.text("1" if bit else "0", x + CW / 2, Y0 + CH / 2, 3, 16 if bit else "rule", a * filled, "center", "middle")


def frame(c, t):
    c.background()
    shift = ramp(t, CUES.rshift, 0.5, "inout")  # 0..1: the >> 16
    out = 1 - ramp(t, CUES.texel - 0.05, 0.3, "smooth")

    # the empty row behind (where the bits sit before and after the shift)
    for i in range(32):
        c.rect(cx(i) + 1, Y0, CW - 2, CH, stroke="rule", lw=2, alpha=0.6)

    # labels above the halves; "integer" becomes "texel" once the row has shifted
    mid_int = (cx(0) + cx(15) + CW) / 2
    mid_frac = (cx(16) + cx(31) + CW) / 2
    lab_y = Y0 - 44
    la = fade(t, 0.0, d_in=0.15)
    x_int = lerp(mid_int, mid_frac, shift)
    c.text("integer", x_int, lab_y, 3, "gold", la * (1 - ramp(t, CUES.texel, 0.15)), "center")
    c.text("texel", mid_frac, lab_y, 3, "yellow", ramp(t, CUES.texel, 0.2), "center")
    c.text("fraction", mid_frac + 16 * (CW + 2) * shift, lab_y, 3, "slate", la * (1 - ramp(t, CUES.rshift, 0.15)), "center")
    c.text("16.16", W / 2, Y0 - 200, 6, "gold", la, "center")
    c.sans(f"{VALUE:g} as a texel coordinate (schematic)", W / 2, Y0 - 112, 26, "dim", la * (1 - shift), "center")

    # the bits fill in, left to right, over the first 0.6 s; then the whole word
    # moves right by 16 cells (>> 16) and the fraction falls off the end
    end = cx(31) + CW
    for i, b in enumerate(BITS):
        filled = ramp(t, CUES.fill + 0.017 * i, 0.06, "out")
        x = cx(i + 16 * shift)
        if i < 16:
            draw_cell(c, x, b, BROWN, 1.0, filled)
        else:
            past = max(0.0, x - (end - CW)) / (8 * CW)  # how far past the row's end
            a = max(0.0, 1 - past) * (1 - ramp(t, CUES.rshift + 0.15, 0.35, "smooth"))
            draw_cell(c, x, b, SLATE, a, filled)
    # zeros shifted in at the top
    zin = ramp(t, CUES.rshift + 0.35, 0.2, "smooth")
    for i in range(16):
        draw_cell(c, cx(i), 0, "rule", zin * 0.7, 1.0)

    # the shift: a rust arrow under the row
    aa = fade(t, CUES.rshift - 0.08, d_in=0.12) * out
    ay = Y0 + CH + 46
    c.arrow(cx(4), ay, cx(28), ay, "rust", 3, 18, aa, draw=ramp(t, CUES.rshift - 0.05, 0.4))
    c.text(">> 16", W / 2, ay + 22, 3, "rust", aa, "center")

    # what is left: 37
    ta = ramp(t, CUES.texel, 0.2, "out")
    rx = cx(31) + CW + 36
    c.text("=", rx, 540, 3, "dim", ta, "left", "middle")
    c.bignum(str(TEXEL), rx + 44, 540, 2, alpha=ta, valign="middle")


if __name__ == "__main__":
    run(frame, duration=DURATION, name="D01-fixed-point", cues=CUES)
