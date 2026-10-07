#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D12, a light is a string of letters: the string and its playhead (ST2, 9.6 s), over e1m1's
fluorescent corridor (Classic, real time).

world.qc's style 10, FLUORESCENT FLICKER, at the top on a dark panel, read from the
repository's quake-rs/src/server/lightstyle.rs as checked out (WORLDSPAWN). The playhead steps
a letter every tenth of cl.time, as id's R_AnimateLight does: letter floor(10 cl.time) mod 25.
Cue `cl0` is cl.time at the overlay's first frame: 3.0 is N3b's shot start
(footage/game/N3b.json); if the edit starts the footage s seconds later, use 3.0 + s. The
letter changes fall at cl.time k/10, i.e. overlay times k/10 - cl0.
"""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

import numpy as np  # noqa: E402

# ------------------------------------------------------------ the facts ----

_src = (quake.REPO / "quake-rs/src/server/lightstyle.rs").read_text()
FLUOR = re.search(r'"([a-z]+)",\s*// 10 FLUORESCENT FLICKER', _src).group(1)
assert FLUOR == "mmamammmmammamamaaamammma" and len(FLUOR) == 25

# ----------------------------------------------------------------- time ----

CUES = Cues(string=0.0, play=0.6, cl0=3.0).argv()
DURATION = duration_arg(9.6)

# --------------------------------------------------------------- layout ----

CH = 32  # 4x conchars
STR_X, STR_Y = (W - len(FLUOR) * CH) / 2, 64


def frame(c, t):
    c.background()
    a = fade(t, CUES.string, d_in=0.3)
    panel(c, STR_X - 40, 30, len(FLUOR) * CH + 80, 120, a, dark=0.78)
    c.text(typed(FLUOR, ramp(t, CUES.string, 0.5, "linear")), STR_X, STR_Y, 4, "gold", a)
    c.text("STYLE 10 · FLUORESCENT FLICKER · E1M1", W / 2, STR_Y + 50, 2, "dim", a * ramp(t, CUES.string + 0.3, 0.4),
           "center")
    if t >= CUES.play:
        cur = int(np.floor((CUES.cl0 + t) * 10 + 1e-9)) % len(FLUOR)
        c.rect(STR_X + cur * CH - 4, STR_Y - 8, CH + 8, CH + 14, stroke=111, lw=4, alpha=a)


if __name__ == "__main__":
    main(frame, DURATION, "D12-string", CUES, "e1m1")
