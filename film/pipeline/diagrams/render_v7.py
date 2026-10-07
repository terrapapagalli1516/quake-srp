#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Render the diagrams the v7 cut uses, into FILM_ROOT, at 1920x1080: render_all.py's defaults.

    uv run film/pipeline/diagrams/render_v7.py               # all of them: this takes minutes
    uv run film/pipeline/diagrams/render_v7.py D01 ladder    # some, by name
    uv run film/pipeline/diagrams/render_v7.py --list        # the commands, without running them

The same as `render_all.py --scale 1 --clock FILM_ROOT/edit/v7 --out FILM_ROOT/diagrams`
(`--events FILE` names the ladder's events instead: a file called ladder-events.json, whose
folder is then the clock). See render_all.py for the outputs, the codecs and the keys.
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


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("only", nargs="*", metavar="NAME", help="render only these (render_all.py --list names them)")
    ap.add_argument("--events", help="the ladder's events (default: FILM_ROOT/edit/v7/ladder-events.json)")
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 4), help="frames rendered in parallel")
    ap.add_argument("--keep-frames", action="store_true", help="keep the PNG frames in FILM_SCRATCH")
    ap.add_argument("--force", action="store_true", help="render even where an output's key matches")
    ap.add_argument("--list", action="store_true", help="print the commands and stop")
    a = ap.parse_args()
    clock = filmroot.FILM / "edit" / "v7"
    if a.events:
        events = Path(a.events).resolve()
        if events.name != "ladder-events.json":
            raise SystemExit("--events: the file must be called ladder-events.json (its folder is the clock)")
        clock = events.parent
    cmd = ["uv", "run", str(HERE / "render_all.py"), "--scale", "1", "--clock", str(clock),
           "--out", str(filmroot.FILM / "diagrams"), "--jobs", str(a.jobs)]
    if a.only:
        cmd += ["--only", *a.only]
    cmd += [f for f, on in (("--keep-frames", a.keep_frames), ("--force", a.force), ("--list", a.list)) if on]
    raise SystemExit(subprocess.run(cmd).returncode)


if __name__ == "__main__":
    main()
