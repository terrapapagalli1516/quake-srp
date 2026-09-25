#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Run id's own WinQuake (the headless oracle, oracle/build/quake-oracle) through a
console script and collect its console output + census dumps.

The oracle's clock is deterministic: every Host_Frame is exactly 0.1 s, and a
`wait` defers the rest of the command buffer by one frame, so `waits N` below is
"let N frames (N/10 s) of id's game run". Census dumps (oracle/c/oracle.c):

    oracle_edicts PATH   append every live server edict (classname, model,
                         origin, angles, frame, movetype, solid, flags, health,
                         nextthink, effects, targetname, mins, maxs) with a "# t=" header
    oracle_client PATH   append cl.time, cl.viewangles, punch, idealpitch, the
                         four cshifts (contents/damage/bonus/powerup), stats

Usage:
    uv run census/oracle_run.py [--dev] [--out DIR] 'map e1m1' 'waits 30' 'oracle_edicts {out}/e.txt' ...

`{out}` expands to the output directory. The run ends with `oracle_quit` (the stock `quit` opens the confirm menu). Prints the
oracle's console log (stdout) — dprint() output appears with --dev (developer 1).
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
ORACLE = PROJECT / "oracle" / "build" / "quake-oracle"
PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dev", action="store_true", help="developer 1 (shows dprint)")
    ap.add_argument("--out", default=None)
    ap.add_argument("--skill", type=int, default=1)
    ap.add_argument("cmds", nargs="+")
    a = ap.parse_args()
    out = Path(a.out or tempfile.mkdtemp(prefix="oracle-census-")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    lines = [f"skill {a.skill}"]
    if a.dev:
        lines.append("developer 1")
    for c in a.cmds:
        c = c.replace("{out}", str(out))
        if c.startswith("waits "):
            lines += ["wait"] * int(c.split()[1])
        else:
            lines.append(c)
    lines.append("oracle_quit")
    with tempfile.TemporaryDirectory(prefix="oracle-base-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(PAK.resolve())
        (base / "id1" / "census.cfg").write_text("\n".join(lines) + "\n")
        res = subprocess.run([str(ORACLE), "-basedir", str(base), "-width", "320", "-height", "200",
                              "+exec", "census.cfg"], cwd=base, capture_output=True, text=True, timeout=300)
        sys.stdout.write(res.stdout)
        sys.stderr.write(res.stderr[-2000:])
        print(f"[oracle rc={res.returncode} out={out}]")


if __name__ == "__main__":
    main()
