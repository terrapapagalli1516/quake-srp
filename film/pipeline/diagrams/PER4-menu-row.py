#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""PER4's menu row (3.0 s), small, top right: the port's Perspective span row as the game
draws it, its value stepping with N2d's flip.

The label and the values are the game's own, read from the repository's quake-rs/src/menu.rs
as checked out: the row "Perspective span" (PICTURE_ROWS), and its value
(RowKind::PerspSpan): "id's 16" at 16, "exact" at every pixel, else the number: 64, 32,
id's 16, 8, 4, exact, and back to 8. Drawn in id's conchars (its lower case is id's small
capitals), label and value bronze as the Slop Options pages draw them (AUDIT.md: "every label and value bronze, as id's Options"), with
id's blinking cursor glyph (char 13). The steps are cues: `s64`..`exact` and `land` (8 again),
in seconds into the overlay; N2d's own flip is 64 at 0, then every 0.267 s, landing on 8 at
1.6. The v7 cut's timing (cues/PER4-menu-row.json, 3.1333 s): the flips at shot 0.727 +
0.267 k, landing on 8 at 2.327.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

import re  # noqa: E402

_src = (quake.REPO / "quake-rs/src/menu.rs").read_text()
assert re.search(r'label: "\s*Perspective span"', _src)
assert 'PerspSpan::Spans16 => "id\'s 16"' in _src and 'PerspSpan::Exact => "exact"' in _src
LABEL = "Perspective span"

CUES = Cues(before=0.0, s64=0.3, s32=0.567, s16=0.833, s8=1.1, s4=1.367, exact=1.633, land=1.9, out=3.0).argv()
DURATION = duration_arg(3.0)
STEPS = [("before", "8"), ("s64", "64"), ("s32", "32"), ("s16", "id's 16"), ("s8", "8"), ("s4", "4"),
         ("exact", "exact"), ("land", "8")]

X, Y = 1196, 84  # the label's left end; the row is 600 px wide, its right end at 1824


def value_at(t: float) -> str:
    v = STEPS[0][1]
    for cue, val in STEPS:
        if t >= CUES[cue] - 1e-6:
            v = val
    return v


def frame(c, t):
    c.background()
    a = fade(t, 0.0, CUES.out, 0.15, 0.25)
    c.rect(X - 48, Y - 22, 600 + 96, 66, fill="bg_deep", alpha=0.8 * a)
    c.rect(X - 48, Y - 22, 600 + 96, 66, stroke="rust_dark", lw=2, alpha=a)
    c.text(LABEL, X, Y, 3, "gold", a)
    c.text(value_at(t), X + 600, Y, 3, "gold", a, "right")
    if int(t * 4) % 2 == 0:  # id's blinking menu cursor
        c.text(chr(13), X - 32, Y, 3, "gold", a)


if __name__ == "__main__":
    main(frame, DURATION, "PER4-menu-row", CUES, "e1m6")
