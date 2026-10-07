#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.14"
# dependencies = ["numpy", "scipy", "numba"]
# ///
"""HERO7's kills on the score's own grid: each kill's accent goes on the sixteenth (the gallop's grid,
counted from HERO's downbeat as its patterns are) nearest to it, so the music keeps its metre and the
kills land on its beats where they fall. Reads the score (for its tempo map) and writes kills.inc beside
this file, which the score includes; the accents add no anchor, so one pass is enough.

    uv run film/pipeline/music/v7/kills.py [film/pipeline/music/score-v10.score]
"""
import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "synth"))
from score import Score  # noqa: E402

ACCENT = {   # what each kill gets: the first is inside the downbeat's bar, the rest in the action half
    1: ["stab  D3,A3,D4,F4 0.25 0.8", "crash - 1 0.7 decay=1.0"],
    2: ["stab  D3,A3,D4,F4 0.25 0.75", "crash - 1 0.65 decay=1.0"],
    3: ["stab  D3,A3,D4 0.2 0.65", "crash - 1 0.55 decay=0.9"],
}

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("score", nargs="?", default=str(HERE.parent / "score-v10.score"))
a = ap.parse_args()

out = HERE / "kills.inc"
out.write_text("# (written by v7/kills.py)\n")
s = Score.load(a.score)
lines = ["# HERO7's kills on the score's nearest sixteenth (written by v7/kills.py from the events; do not edit)"]
ev = {k: float(v[0]) for k, v in s.macros.items() if k.startswith("KILL") or k in ("HERO", "ALL_ON_OUT")}
for i in (1, 2, 3, 4):
    t = ev.get(f"KILL{i}")
    if t is None:
        continue
    if t > ev["ALL_ON_OUT"] - 0.3:   # the last fiend dies into the button, which is its hit
        lines.append(f"# KILL{i} {t:.3f}: on the button ({ev['ALL_ON_OUT']:.3f})")
        continue
    b, b0 = s.tmap.beat(t), s.tmap.beat(ev["HERO"])   # HERO's patterns count their beats from HERO
    q = b0 + round((b - b0) * 4) / 4                 # the sixteenth: the gallop's own grid
    tq = s.tmap.sec(q)
    lines.append(f"# KILL{i} {t:.3f} -> the sixteenth at {tq:.3f} ({(tq - t) * 1000:+.0f} ms)")
    for acc in ACCENT[i]:
        lines.append(f"@{tq:.4f} {acc}")
out.write_text("\n".join(lines) + "\n")
print("\n".join(l for l in lines if l.startswith("#")))
