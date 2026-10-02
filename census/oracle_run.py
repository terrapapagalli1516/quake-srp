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
    # A mission pack's own game directory: id's COM_InitFilesystem already
    # knows `-hipnotic`/`-rogue` (stock WinQuake, no oracle.c change needed) —
    # this harness just didn't set up the directory or pass the flag yet
    # (startdoor brief, AUDIT.md "the mission packs"). Point `--hipnotic-pak`/
    # `--rogue-pak` at that pack's own `pak0.pak` (e.g. a deploy's
    # `hipnotic/pak0.pak`, built by `mission_paks.py`); id1's shareware
    # pak0 is always included too, since the search path layers over it.
    ap.add_argument("--hipnotic-pak", default=None, help="path to hipnotic's own pak0.pak; implies -hipnotic")
    ap.add_argument("--rogue-pak", default=None, help="path to rogue's own pak0.pak; implies -rogue")
    # id's COM_CheckRegistered gates -hipnotic/-rogue on gfx/pop.lmp, which
    # only the registered id1/pak1.pak carries: a mission pack run needs it
    # alongside the shareware pak0.
    ap.add_argument("--id1-pak1", default=None, help="path to id1's registered pak1.pak (needed for --hipnotic-pak/--rogue-pak)")
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
    if not ORACLE.exists():
        print("building the C oracle (oracle/build.sh) ...", file=sys.stderr)
        subprocess.run([str(PROJECT / "oracle" / "build.sh")], check=True)
    with tempfile.TemporaryDirectory(prefix="oracle-base-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(PAK.resolve())
        if a.id1_pak1:
            (base / "id1" / "pak1.pak").symlink_to(Path(a.id1_pak1).resolve())
        (base / "id1" / "census.cfg").write_text("\n".join(lines) + "\n")
        argv = [str(ORACLE), "-basedir", str(base), "-width", "320", "-height", "200"]
        if a.hipnotic_pak:
            hdir = base / "hipnotic"
            hdir.mkdir()
            (hdir / "pak0.pak").symlink_to(Path(a.hipnotic_pak).resolve())
            argv.append("-hipnotic")
        if a.rogue_pak:
            rdir = base / "rogue"
            rdir.mkdir()
            (rdir / "pak0.pak").symlink_to(Path(a.rogue_pak).resolve())
            argv.append("-rogue")
        argv += ["+exec", "census.cfg"]
        res = subprocess.run(argv, cwd=base, capture_output=True, text=True, timeout=300)
        sys.stdout.write(res.stdout)
        sys.stderr.write(res.stderr[-2000:])
        print(f"[oracle rc={res.returncode} out={out}]")


if __name__ == "__main__":
    main()
