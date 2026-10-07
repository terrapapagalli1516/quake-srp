#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D12's graph, the strip under S48 (9.5 s): id's light steps, slop's glides.

S48 plays e1m1's fluorescent corridor rendered at 240 Hz over cl.time 4.95-5.54
at 1/16 speed (one 240 Hz frame per four 60 fps frames), so diagram time t is
cl.time 4.95 + t/16. Style 10, `mmamammmmammamamaaamammma`, and GLIDE_STEP are
read from the repository's quake-rs/src/server/lightstyle.rs as checked out
(WORLDSPAWN): the letter at 5.1 is 'm', at 5.2 'a' (FRAMERATE.md's measured
window, "5.1-5.2 in e1m1's corridor, where style 10 goes from 'm' to 'a'").

- id's (`r_lerplightstyles 0`): `(map[(int)(cl.time*10) % len] - 'a') * 22`,
  held for the tenth.
- slop's glide (`r_lerplightstyles 1`): the port's `lightstyle_value`,
  `LerpLightStyles::Smooth`: from letter k toward letter k+1 by frac(10
  cl.time), rounded to whole steps of GLIDE_STEP = 2 units, so it is id's
  letter at every whole tenth. Computed here the same way, at each 240 Hz frame.

Laid over footage: render with `--alpha` (ProRes 4444 .mov); the mp4 shows it
on the film's background. The strip is 1880x300 at (20, 760).
"""

import math
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # qkit/

import numpy as np

from qkit import *
from qkit import quake

# ------------------------------------------------------------ the facts ----

_src = (quake.REPO / "quake-rs/src/server/lightstyle.rs").read_text()
FLUOR = re.search(r'"([a-z]+)",\s*// 10 FLUORESCENT FLICKER', _src).group(1)
GLIDE_STEP = int(re.search(r"pub const GLIDE_STEP: i32 = (\d+);", _src).group(1))
assert FLUOR == "mmamammmmammamamaaamammma" and GLIDE_STEP == 2
T0, T1, SLOW = 4.95, 5.54, 16.0


def letter(i: int) -> int:
    return (ord(FLUOR[i % len(FLUOR)]) - 97) * 22


def rround(x: float) -> int:  # Rust's f32::round: half away from zero
    return int(math.floor(x + 0.5)) if x >= 0 else -int(math.floor(-x + 0.5))


def id_value(cl: float) -> int:
    return letter(int(math.floor(cl * 10)))


def glide_value(cl: float) -> int:
    p = cl * 10
    i = math.floor(p)
    frac = float(np.float32(p - i))
    here, nxt = letter(i), letter(i + 1)
    return here + rround((nxt - here) * frac / GLIDE_STEP) * GLIDE_STEP


assert letter(51) == 264 and letter(52) == 0  # 5.1: 'm'; 5.2: 'a'
FR = T0 + np.arange(int((T1 - T0) * 240) + 1) / 240  # the 240 Hz frames
ID_V = np.array([id_value(x) for x in FR])
GL_V = np.array([glide_value(x) for x in FR])
TENTHS = [k / 10 for k in range(50, 56)]  # 5.0 .. 5.5
for x in TENTHS:
    assert id_value(x + 1e-9) == glide_value(x + 1e-9)  # they meet at every tenth

# ----------------------------------------------------------------- time ----

CUES = Cues(strip=0.0, play=0.0)
DURATION = 9.5

# --------------------------------------------------------------- layout ----

SX, SY, SW, SH = 20, 760, 1880, 300
AX = (SX + 150, SY + 84, SW - 190, SH - 144)  # the plot inside the strip


def cl_at(t: float) -> float:
    return min(T1, T0 + (t - CUES.play) / SLOW)


def frame(c, t):
    c.background()
    a = ramp(t, CUES.strip, 0.3, "smooth")
    c.rect(SX, SY, SW, SH, fill="bg_deep", alpha=0.82 * a)
    c.rect(SX, SY, SW, SH, stroke="rule", lw=2, alpha=a)
    ax = Axes(c, *AX, xlim=(T0, T1), ylim=(-20, 300))
    ax.frame(xticks=TENTHS, xlabels=[f"{x:.1f}" for x in TENTHS], yticks=[0, 264], ylabels=["a  0", "m 264"],
             alpha=a, label_scale=2, grid=True)
    c.text("cl.time", SX + 24, ax.y + ax.h + 14, 2, "dim", a)
    # the letter of each tenth, over the plot
    for k in range(49, 56):
        x0, x1 = max(T0, k / 10), min(T1, (k + 1) / 10)
        if x1 <= x0:
            continue
        c.text(FLUOR[k % 25], float(ax.px((x0 + x1) / 2)), ax.y - 6, 3, "gold", a * 0.9, "center", "bottom")

    cl = cl_at(t)
    n = int(np.searchsorted(FR, cl, side="right"))  # frames shown so far
    # the curves: the future faint, the past full
    # id's as the step function it is (vertical at each tenth); the glide through its frames
    sx, sy = [T0], [id_value(T0)]
    for k in range(50, 56):
        sx += [k / 10, k / 10]
        sy += [letter(k - 1), letter(k)]
    sx.append(T1)
    sy.append(id_value(T1))
    sx, sy = np.array(sx), np.array(sy)
    ax.plot(sx, sy, 105, 4, a * 0.25)
    m = int(np.searchsorted(sx, cl, side="right"))
    if n >= 1:
        ax.plot(np.r_[sx[:m], cl], np.r_[sy[:m], id_value(cl)], 105, 4, a)
        ax.dots(FR[:n], ID_V[:n], 3, 105, a)
    ax.plot(FR, GL_V, 63, 4, a * 0.25)
    if n >= 2:
        ax.plot(FR[:n], GL_V[:n], 63, 4, a)
    if n >= 1:
        ax.dots(FR[:n], GL_V[:n], 3, 63, a)
    # the playhead and the frame it is on
    if n >= 1:
        px = float(ax.px(FR[n - 1]))
        c.line(px, ax.y - 4, px, ax.y + ax.h, 111, 2, a)
        ax.dots([FR[n - 1]], [ID_V[n - 1]], 7, 105, a, stroke="bg_deep")
        ax.dots([FR[n - 1]], [GL_V[n - 1]], 7, 63, a, stroke="bg_deep")
    # a white tick where they meet, flashing as the playhead passes each tenth
    for x in TENTHS:
        dt = cl - x
        if dt < 0:
            continue
        flash = math.exp(-dt / 0.02)  # fades over about 0.3 s of the film (cl.time 0.02)
        X, Y = ax.pt(x, id_value(x + 1e-9))
        c.line(X, Y - 18, X, Y + 18, "white", 3, a * (0.45 + 0.55 * flash))

    # the legend and the frame count
    c.line(SX + 24, SY + 26, SX + 64, SY + 26, 105, 4, a)
    c.text("id's: each letter held for 0.1 s", SX + 76, SY + 26, 2, "text", a, "left", "middle")
    c.line(SX + 620, SY + 26, SX + 660, SY + 26, 63, 4, a)
    c.text("slop: glides, and meets id's at every tenth", SX + 672, SY + 26, 2, "text", a, "left",
           "middle")
    c.text(f"240 Hz frame {max(0, n - 1):3d}", SX + SW - 20, SY + 26, 2, "dim", a, "right", "middle")


if __name__ == "__main__":
    run(frame, duration=DURATION, name="D12b-glide-strip", cues=CUES)
