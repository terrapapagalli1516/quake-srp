#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D15, the fleet and the gate (S57 + S58: 11.6 s, the gate from 7.6), over N10.

README.md, "How it was built": "Most of the work ran as fleets of agents, each on its own
git branch with a written brief, and a chair agent that merged a branch only after the full
check passed." The check: `oracle/classic_check.py` (Classic's proof), `cargo test` and
clippy.

The eleven branches are real: each names a section of AUDIT.md, FRAMERATE.md or
PERF_PLAN.md, and each has a "Merge <branch>" commit in the repository's history, read
below with `git log --all` from the repository as checked out (q26/lerp was merged through
q26/settings; a shallow clone lacks them). They peel off and merge in the order of those
merge commits' dates. The graph's shape is schematic.

Each agent's diamond waits at its branch's tip, then goes across to the main line at the
tip's height and up the main line to the gate (an L), under the branch labels.
"""

import math
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

# ------------------------------------------------------------ the facts ----

NAMES = ["quake/edge", "quake/w2b", "q26/hires", "q26/lerp", "q26/framerate", "fleet/stairs", "fleet/lerplight",
         "fleet/torchlight", "fleet/perspspan", "fleet/opt-pixels", "fleet/slopmenu"]


def _merge_date(name: str) -> str:
    out = subprocess.run(["git", "-C", str(quake.REPO), "log", "--all", "--merges", "--format=%cI %s",
                          f"--grep=Merge {name}"], capture_output=True, text=True, check=True).stdout
    dates = [ln.split()[0] for ln in out.splitlines() if f"Merge {name}" in ln]
    assert dates, f"no merge commit for {name}"
    return min(dates)


BRANCHES = sorted(NAMES, key=_merge_date)  # the order they were merged in
COLS = [105, 63, 44, 31, 235]  # rust, green, slate, brown, lava

# ----------------------------------------------------------------- time ----

CUES = Cues(main=0.0, peel=0.6, caption_a=1.2, gate=7.6, flow=8.0, caption_b=8.3).argv()
DURATION = duration_arg(11.6)
PEEL_STEP, GROW = 0.6, 0.7
FLOW_STEP, FLOW = 0.3, 0.4

# --------------------------------------------------------------- layout ----

MX = W / 2
MAIN_BOTTOM, MAIN_TOP = 990, 60
GATE_Y = 300
LEFT_LANES = [180, 310, 440, 570, 700, 830]  # outermost first: early branches go furthest
RIGHT_LANES = [1610, 1480, 1350, 1220, 1090]
SIDE = 0  # the leg runs up the main line itself (beside it would cross the next labels' plates)


def branch_geom(i):
    """Root on main, lane x, tip y."""
    root = 960 - 34 * i
    lane = LEFT_LANES[i // 2] if i % 2 == 0 else RIGHT_LANES[i // 2]
    return root, lane, root - 200


def paper(c, x, y, col, a):
    """A brief: a sheet with a folded corner and three lines."""
    c.poly([(x - 9, y - 12), (x + 4, y - 12), (x + 9, y - 7), (x + 9, y + 12), (x - 9, y + 12)], fill="text",
           stroke=col, lw=2, alpha=a)
    for k in range(3):
        c.line(x - 5, y - 5 + 5 * k, x + 5, y - 5 + 5 * k, "bg", 1.5, a)


def diamond(c, x, y, col, a, r=11):
    c.poly([(x, y - r), (x + r, y), (x, y + r), (x - r, y)], fill=col, stroke="white", lw=2, alpha=a)


def branch_path(i):
    """The branch: out from main on a quarter curve, then straight up to its tip."""
    root, lane, tip = branch_geom(i)
    out = [(MX + (lane - MX) * math.sin(k / 10 * math.pi / 2), root - 40 * (1 - math.cos(k / 10 * math.pi / 2)))
           for k in range(11)]
    return out + [(lane, tip)]


def agent_at(lane: float, tip: float, u: float) -> tuple[float, float]:
    """The L from the tip: across to beside main (first half), then up to the gate (second)."""
    side = MX - SIDE if lane < MX else MX + SIDE
    if u < 0.5:
        k = EASE["inout"](u / 0.5)
        return lerp(lane, side, k), tip
    k = EASE["inout"]((u - 0.5) / 0.5)
    return lerp(side, MX, k), lerp(tip, GATE_Y + 26, k)


def frame(c, t):
    c.background()
    a = fade(t, 0.0, d_in=0.3)
    # plates under the title, the gate's line and the captions, over the footage
    c.rect(56, 28, 568, 74, fill="bg_deep", alpha=a * 0.7, radius=6)
    ga = ramp(t, CUES.gate, 0.35, "out")
    c.rect(MX - 330, GATE_Y - 34, 990, 68, fill="bg_deep", alpha=ga * 0.7, radius=6)
    ca = fade(t, CUES.caption_a, CUES.gate + 0.3, 0.5, 0.4)
    cb = ramp(t, CUES.caption_b, 0.5)
    c.rect(MX - 700, 1008, 1400, 56, fill="bg_deep", alpha=max(ca, cb) * 0.75, radius=6)

    c.text("HOW IT WAS BUILT", 80, 44, 4, "gold", a)
    m = ramp(t, CUES.main, 0.8, "out")
    c.line(MX, MAIN_BOTTOM, MX, MAIN_BOTTOM - (MAIN_BOTTOM - MAIN_TOP) * m, 92, 6, 1.0)
    c.text("main", MX + 16, MAIN_BOTTOM - 10, 2, "gold", m, "left", "middle")

    merged_n, gate_flash, labels, agents = 0, 0.0, [], []
    for i, name in enumerate(BRANCHES):
        col = COLS[i % len(COLS)]
        root, lane, tip = branch_geom(i)
        t0 = CUES.peel + PEEL_STEP * i
        g = ramp(t, t0, GROW, "inout")
        if g <= 0:
            continue
        f0 = CUES.flow + FLOW_STEP * i  # its merge
        fl = ramp(t, f0, FLOW, "linear")
        done = t >= f0 + FLOW
        dim = 0.6 if done else 1.0
        tipx, tipy = c.polyline(branch_path(i), col, 4, dim, draw=g)
        c.circle(MX, root, 6, fill=col, alpha=dim)
        paper(c, (MX + lane) / 2, root - 52, col, min(1, g * 3) * dim)
        labels.append((name, lane, tip, ramp(t, t0 + GROW - 0.15, 0.3), dim))
        if fl <= 0:
            agents.append((tipx, tipy, col))
        elif not done:
            agents.append((*agent_at(lane, tip, fl), col))
        else:
            merged_n += 1
            gate_flash = max(gate_flash, 1 - ramp(t, f0 + FLOW, 0.25, "smooth"))
    for x, y, col in agents:  # the diamonds, under the labels
        diamond(c, x, y, col, 1.0)
    for name, lane, tip, la, dim in labels:
        lb = c.text(name, lane, tip - 34, 2, "text", 0, "center", "middle")
        c.rect(lb[0] - 6, lb[1] - 5, lb[2] + 12, lb[3] + 10, fill="bg_deep", alpha=0.8 * la * dim)
        c.text(name, lane, tip - 34, 2, "text", la * (0.6 + 0.4 * dim), "center", "middle")

    # the gate, the chair, the check
    if ga > 0:
        bar = mix(92, 63, gate_flash)
        c.rect(MX - 210, GATE_Y - 7, 420, 14, fill=bar, alpha=ga)
        if gate_flash > 0:
            c.rect(MX - 222, GATE_Y - 19, 444, 38, stroke=63, lw=3, alpha=ga * gate_flash)
        c.text("chair", MX - 236, GATE_Y, 3, "gold", ga, "right", "middle")
        c.text("classic_check · cargo test · clippy", MX + 236, GATE_Y, 2, "text", ga, "left", "middle")
    # merge dots on main, above the gate
    for k in range(merged_n):
        c.circle(MX, GATE_Y - 40 - 20 * k, 8, fill=COLS[k % len(COLS)], stroke="white", lw=2)
    c.text("Claude · fleets of agents · a branch and a brief each", MX, 1036, 3, "white", ca, "center", "middle")
    c.text("merged only when every check passed", MX, 1036, 3, "white", cb, "center", "middle")


if __name__ == "__main__":
    main(frame, DURATION, "D15-fleet-gate", CUES, "start", dim=0.45)
