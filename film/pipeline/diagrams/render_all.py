#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Render every diagram the cut uses, and the ladder from the clock, at a whole-number scale.

    uv run film/pipeline/diagrams/render_all.py --scale 2 --clock OUT/edit --out OUT/diagrams
    uv run film/pipeline/diagrams/render_all.py --scale 2 --clock OUT/edit --out OUT/diagrams --only D13 ladder
    uv run film/pipeline/diagrams/render_all.py --list          # the commands, without running them

`--scale 1` draws them at 1920x1080 exactly as the v7 cut has them; `--scale 2` at 3840x2160,
every line, letter and glyph at twice its 1080 geometry (the kit's cairo transform: lines and
Inter are drawn sharp at the new resolution, id's 8x8 glyphs at twice their scale, nearest
neighbour). The outputs keep the names the edit reads, under `--out`:
v7/ladder_ST0.mov ... (one per span of the clock's ladder-events.json), v7/D03-oracle-band_alpha.mov,
v6/D11-jump_alpha.mov, final/D01-fixed-point.mp4, and so on (JOBS below), each single diagram
with a contact sheet beside it.

Codecs: the overlays are ProRes 4444 with alpha (ffmpeg's prores_ks, profile 4444,
yuva444p10le in, read back as yuva444p12le): VAAPI encodes no alpha. D01 and D02, whole-frame
shots, are H.264 (libx264, CRF 16, yuv420p): short, so software. At scale 2 the ProRes files
are about four times v7's.

Incremental: each output gets a `.key` beside it, a hash of what made it (this folder's code
and cue files, the arguments, the scale and, for the ladder, the clock's events). An output
whose key matches is skipped; `--force` renders anyway. Inputs outside this folder (id's pak,
the repository's Rust sources, the quaketool binary, the merge history D15 draws) are not in
the key: after changing those, pass `--force`.

The other diagrams' timings are the cue files in cues/, on the v7 cut's words, not the clock's:
the clock gives the ladder its spans and events only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))  # film/pipeline: filmroot

import filmroot  # noqa: E402

CUES = HERE / "cues"

# name, output under --out, script, arguments
JOBS = [
    ("D01", "final/D01-fixed-point.mp4", "D01-fixed-point.py",
     ["--duration", "1.17", "--cues", CUES / "D01-fixed-point.json"]),
    ("D02", "final/D02-palette.mp4", "D02-palette.py",
     ["--duration", "4.12", "--cues", CUES / "D02-palette.json"]),
    ("D03", "v7/D03-oracle-band_alpha.mov", "D03-oracle-band.py",
     ["--duration", "4.717", "--cues", CUES / "D03-oracle-band.json"]),
    ("D06a", "v6/D06a-perspective_alpha.mov", "D06a-perspective.py", []),
    ("D11", "v6/D11-jump_alpha.mov", "D11-jump.py",
     ["--duration", "12.35", "--cues", CUES / "D11-jump.json"]),
    ("D12", "v3/D12-string_alpha.mov", "D12-string.py",
     ["--duration", "9.4167", "--cues", CUES / "D12-string.json"]),
    ("D12b", "v7/D12b-glide-strip_alpha.mov", "D12b-glide-strip.py",
     ["--alpha", "--duration", "9.583", "--cues", CUES / "D12b-glide-strip.json"]),
    ("D13", "v6/D13-luxel_alpha.mov", "D13-luxel.py",
     ["--duration", "4.483", "--cues", CUES / "D13-luxel.json"]),
    ("D14", "v6/D14-mixer-teleporter_alpha.mov", "D14-mixer.py",
     ["--teleporter", "--duration", "10.467", "--cues", CUES / "D14-mixer-teleporter.json"]),
    ("D15", "v4/D15-fleet-gate_alpha.mov", "D15-fleet-gate.py", []),
    ("PER4", "v4/PER4-menu-row_alpha.mov", "PER4-menu-row.py",
     ["--duration", "3.1333", "--cues", CUES / "PER4-menu-row.json"]),
    ("bumper", "v4/bumper-torches-stg_alpha.mov", "bumper-torches-stg.py", []),
    # the ladder: one movie per span of the clock's events, ladder_NAME.mov in this folder
    ("ladder", "v7", "ladder.py", ["--overrides", HERE / "ladder-span-overrides.json"]),
]
NAMES = [j[0] for j in JOBS]


def code_hash() -> str:
    """This folder's code and data: every .py and .json in it and in qkit/ and cues/."""
    h = hashlib.sha256()
    for p in sorted([*HERE.glob("*.py"), *HERE.glob("*.json"), *(HERE / "qkit").glob("*.py"), *CUES.glob("*.json")]):
        h.update(p.relative_to(HERE).as_posix().encode())
        h.update(p.read_bytes())
    return h.hexdigest()


def key_for(name: str, args: list[str], scale: int, events: Path | None, code: str) -> str:
    h = hashlib.sha256()
    h.update(json.dumps([name, [str(a) for a in args], scale, code]).encode())
    if events is not None:
        h.update(events.read_bytes())
    return h.hexdigest()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scale", type=int, default=1, help="a whole multiple of 1920x1080 (2: 3840x2160)")
    ap.add_argument("--clock", help="the clock's folder, with ladder-events.json (default: FILM_ROOT/edit/v7)")
    ap.add_argument("--out", help="the diagrams folder the edit reads (default: FILM_ROOT/diagrams)")
    ap.add_argument("--only", nargs="+", metavar="NAME", help=f"only these: {' '.join(NAMES)}")
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 4), help="frames rendered in parallel")
    ap.add_argument("--force", action="store_true", help="render even where an output's key matches")
    ap.add_argument("--keep-frames", action="store_true", help="keep the PNG frames in FILM_SCRATCH")
    ap.add_argument("--list", action="store_true", help="print the commands and stop")
    a = ap.parse_args()
    if a.scale < 1:
        raise SystemExit("--scale is a whole number, 1 or more")
    unknown = set(a.only or []) - set(NAMES)
    if unknown:
        raise SystemExit(f"unknown diagrams {sorted(unknown)}: they are {' '.join(NAMES)}")
    clock = Path(a.clock).resolve() if a.clock else filmroot.FILM / "edit" / "v7"
    out_root = Path(a.out).resolve() if a.out else filmroot.FILM / "diagrams"
    events = clock / "ladder-events.json"
    code = code_hash()

    for name, out, script, args in JOBS:
        if a.only and name not in a.only:
            continue
        target = out_root / out
        lead = [events] if name == "ladder" else []
        cmd = ["uv", "run", HERE / script, *lead, *args, "--out", target, "--scale", str(a.scale),
               "--jobs", str(a.jobs)]
        if a.keep_frames:
            cmd.append("--keep-frames")
        cmd = [str(x) for x in cmd]
        print(" ".join(cmd), flush=True)
        if a.list:
            continue
        if name == "ladder" and not events.exists():
            raise SystemExit(f"{events} is missing: the clock (film/pipeline/edit/timeline.py) writes it")
        key = key_for(name, args, a.scale, events if name == "ladder" else None, code)
        stamp = (target / ".ladder.key") if name == "ladder" else target.with_name(target.name + ".key")
        done = stamp.exists() and stamp.read_text().strip() == key and (target.is_dir() or target.exists())
        if done and not a.force:
            print(f"  {name}: unchanged, skipped ({stamp.name})", flush=True)
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(cmd, check=True)
        stamp.parent.mkdir(parents=True, exist_ok=True)
        stamp.write_text(key + "\n")


if __name__ == "__main__":
    main()
