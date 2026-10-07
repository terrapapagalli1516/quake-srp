#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Render the diagrams the v7 cut uses, into FILM_ROOT, with the arguments that made each.

    uv run film/pipeline/diagrams/render_v7.py               # all of them: this takes minutes
    uv run film/pipeline/diagrams/render_v7.py D01 ladder    # some, by name
    uv run film/pipeline/diagrams/render_v7.py --list        # the commands, without running them

Each output goes where the edit looks for it (FILM_ROOT/diagrams/v7/ladder_ST0.mov, ...), with
a contact sheet beside each single diagram. The overlays are ProRes 4444 with alpha; D01 and
D02, whole-frame shots, are H.264. Frames go to FILM_SCRATCH and are removed once each movie
is made (`--keep-frames` keeps them, for comparing renders frame by frame).

The ladder's spans and events are the edit's: its timeline writes edit/v7/ladder-events.json
under FILM_ROOT (`--events` names another file); ladder-span-overrides.json, here, moves two
spans. The other diagrams' timings are the cue files in cues/, on the v7 cut's words.

What they read: id's shareware pak (palette, conchars, gfx.wad, maps), and the repository as
checked out: torch.rs (the bumper, D13), lightstyle.rs (D12, D12b), menu.rs (PER4), the
Rust files D03 names, the merge history (D15, `git log --all`); D02 and D06a render frames
with quaketool, and D14 mixes a sound with `quaketool sndscript` (build it first:
`cd quake-rs && cargo build --release --bin quaketool`). They need cairo, the Inter font (D01's
caption) and ffmpeg.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))  # film/pipeline: filmroot

import filmroot  # noqa: E402

CUES = HERE / "cues"

# name, output under FILM_ROOT, script, arguments
JOBS = [
    ("D01", "diagrams/final/D01-fixed-point.mp4", "D01-fixed-point.py",
     ["--duration", "1.17", "--cues", CUES / "D01-fixed-point.json"]),
    ("D02", "diagrams/final/D02-palette.mp4", "D02-palette.py",
     ["--duration", "4.12", "--cues", CUES / "D02-palette.json"]),
    ("D03", "diagrams/v7/D03-oracle-band_alpha.mov", "D03-oracle-band.py",
     ["--duration", "4.717", "--cues", CUES / "D03-oracle-band.json"]),
    ("D06a", "diagrams/v6/D06a-perspective_alpha.mov", "D06a-perspective.py", []),
    ("D11", "diagrams/v6/D11-jump_alpha.mov", "D11-jump.py",
     ["--duration", "12.35", "--cues", CUES / "D11-jump.json"]),
    ("D12", "diagrams/v3/D12-string_alpha.mov", "D12-string.py",
     ["--duration", "9.4167", "--cues", CUES / "D12-string.json"]),
    ("D12b", "diagrams/v7/D12b-glide-strip_alpha.mov", "D12b-glide-strip.py",
     ["--alpha", "--duration", "9.583", "--cues", CUES / "D12b-glide-strip.json"]),
    ("D13", "diagrams/v6/D13-luxel_alpha.mov", "D13-luxel.py",
     ["--duration", "4.483", "--cues", CUES / "D13-luxel.json"]),
    ("D14", "diagrams/v6/D14-mixer-teleporter_alpha.mov", "D14-mixer.py",
     ["--teleporter", "--duration", "10.467", "--cues", CUES / "D14-mixer-teleporter.json"]),
    ("D15", "diagrams/v4/D15-fleet-gate_alpha.mov", "D15-fleet-gate.py", []),
    ("PER4", "diagrams/v4/PER4-menu-row_alpha.mov", "PER4-menu-row.py",
     ["--duration", "3.1333", "--cues", CUES / "PER4-menu-row.json"]),
    ("bumper", "diagrams/v4/bumper-torches-stg_alpha.mov", "bumper-torches-stg.py", []),
    # the ladder: one movie per span of the events, ladder_NAME.mov in this folder
    ("ladder", "diagrams/v7", "ladder.py", ["--overrides", HERE / "ladder-span-overrides.json"]),
]


def main() -> None:
    names = [j[0] for j in JOBS]
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("only", nargs="*", metavar="NAME", help=f"render only these: {' '.join(names)}")
    ap.add_argument("--events", help="the ladder's events (default: FILM_ROOT/edit/v7/ladder-events.json)")
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 4), help="frames rendered in parallel")
    ap.add_argument("--keep-frames", action="store_true", help="keep the PNG frames in FILM_SCRATCH")
    ap.add_argument("--list", action="store_true", help="print the commands and stop")
    a = ap.parse_args()
    unknown = set(a.only) - set(names)
    if unknown:
        raise SystemExit(f"unknown diagrams {sorted(unknown)}: they are {' '.join(names)}")
    events = Path(a.events).resolve() if a.events else filmroot.FILM / "edit" / "v7" / "ladder-events.json"

    cmds = []
    for name, out, script, args in JOBS:
        if a.only and name not in a.only:
            continue
        lead = [events] if name == "ladder" else []
        cmd = ["uv", "run", HERE / script, *lead, *args, "--out", filmroot.FILM / out, "--jobs", str(a.jobs)]
        if a.keep_frames:
            cmd.append("--keep-frames")
        cmds.append((name, [str(x) for x in cmd]))

    for name, cmd in cmds:
        print(" ".join(cmd), flush=True)
        if a.list:
            continue
        if name == "ladder" and not events.exists():
            raise SystemExit(f"{events} is missing: the edit's timeline writes it (or pass --events)")
        subprocess.run(cmd, check=True)


if __name__ == "__main__":
    main()
