#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""R_RotateBmodel against id's C, on a real mission-pack rotating door.

    uv run oracle/rotate_check.py

Renders Scourge of Armagon's `maps/start.bsp` — the start room's "damndoor"
(`func_rotate_door`, driving `rotate_object *65` and 22 `func_movewall`s the
server moves to match) — at four points of its swing (roll 0, 30, 60, 90
degrees: 0 is closed, 90 is id's own `angles "0 0 90"`, the full open turn)
from the player's own spawn point, which already has the door in view. id's
C renders each pose for real (`oracle_entfield`, oracle.c: poses any edict,
not just the player's, for a shot — a door mid-turn without scripting its
trigger) and the port renders the same entity id's frame drew
(`quaketool view --ents`), exactly as `compare.py`'s `ents` mode does for
id1. Needs docker (the C oracle, built on first use), uv, cargo, and the
*registered* re-release content this round's mission-pack work produced:
`id1/pak0.pak` + `id1/pak1.pak` and `hipnotic/pak0.pak` (`--id1`/`--id1-pak1`/
`--hipnotic`; id's own `-game`/`-hipnotic` refuse the shareware id1, so the
registered pak1 is required here, unlike the rest of this directory).

Reports each pose's match on the door's own pixels (where id's roll-0 and
roll-90 frames differ — isolates the door from the room's own, unrelated,
already-documented baseline mismatch: texel-boundary noise on diagonal
surfaces, oracle/README.md) and writes a side-by-side PNG per pose.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compare as C  # noqa: E402  (the shared pak/pnm/stats/side_by_side helpers)

HERE = Path(__file__).resolve().parent
DEFAULT_ID1 = HERE.parent / "quake-data" / "ID1" / "PAK0.PAK"

# The player's own spawn (start.bsp's info_player_start): *65 is already in
# view from here, no teleport needed (confirmed against a live oracle_edicts
# dump: *29/*65/*77/*94 are on cl_visedicts at this eye).
SPAWN = (816.0, -704.0, 216.0, 0.0, 90.0, 0.0)  # x y z pitch yaw roll
# rotate_object *65's edict number (oracle_edicts: `62  rotate_object  *65
# 816 -256 160  ...  t2`), found once against a live dump; hip1m1 and other
# mission-pack maps would need their own (not stable across maps/builds).
DOOR_EDICT = 62
ROLLS = (0, 30, 60, 90)


def run_c(oracle: Path, base: Path, roll: float, out: Path, case: str) -> dict:
    x, y, z, p, yaw, r = SPAWN
    lines = (
        # oracle_spans 16: the x86 asm's 16-pixel spans in C, what the port draws (compare.py's
        # own default is id's portable 8-pixel C, whose whole-scene match is ~4 points lower:
        # README, "Discrepancy classes").
        ["oracle_exit 1", "viewsize 120", "r_drawviewmodel 0", "r_drawentities 1", "crosshair 0", "oracle_spans 16", "map start"]
        + ["wait"] * 30
        + [f"oracle_entfield {DOOR_EDICT} angles 0 0 {roll}"]
        + ["wait"] * 3
        + [f"oracle_view {x} {y} {z} {p} {yaw} {r}", f'oracle_shot "{out / case}"']
        + ["wait", "wait", "oracle_quit"]
    )
    (base / "id1" / "oracle.cfg").write_text("\n".join(lines) + "\n")
    res = subprocess.run(
        [str(oracle), "-basedir", str(base), "-hipnotic", "-width", "320", "-height", "200", "+exec", "oracle.cfg"],
        cwd=base, capture_output=True, text=True, timeout=60,
    )
    if res.returncode != 0 or not (out / f"{case}.json").exists():
        sys.exit(f"C oracle failed at roll {roll}:\n{res.stdout[-2000:]}{res.stderr[-1000:]}")
    return json.loads((out / f"{case}.json").read_text())


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--id1", type=Path, default=DEFAULT_ID1, help="id1/pak0.pak (shareware is fine for the palette)")
    ap.add_argument("--id1-pak1", type=Path, required=False,
                    help="id1's registered pak1.pak (required: -hipnotic refuses the shareware id1)")
    ap.add_argument("--hipnotic", type=Path, required=False, help="hipnotic/pak0.pak (the 2021 re-release)")
    ap.add_argument("--out", type=Path, help="output dir (default: a new temp dir)")
    ap.add_argument("--oracle", help="C oracle binary (default: build it)")
    ap.add_argument("--quaketool", help="quaketool binary (default: build it)")
    args = ap.parse_args()

    if not args.id1_pak1 or not args.hipnotic:
        sys.exit(
            "needs --id1-pak1 and --hipnotic (id's own -hipnotic refuses the shareware id1): e.g.\n"
            "  uv run oracle/rotate_check.py --id1-pak1 .../id1/pak1.pak --hipnotic .../hipnotic/pak0.pak"
        )

    out = args.out or Path(tempfile.mkdtemp(prefix="quake-rotate-check-"))
    out.mkdir(parents=True, exist_ok=True)
    oracle = C.ensure_oracle(args.oracle)
    qt = C.ensure_quaketool(args.quaketool)
    pal = np.frombuffer(C.read_pak_file(args.id1, "gfx/palette.lmp")[:768], dtype=np.uint8).reshape(256, 3)

    with tempfile.TemporaryDirectory(prefix="quake-rotate-base-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(args.id1.resolve())
        (base / "id1" / "pak1.pak").symlink_to(args.id1_pak1.resolve())
        (base / "hipnotic").mkdir()
        (base / "hipnotic" / "pak0.pak").symlink_to(args.hipnotic.resolve())

        frames: dict[int, np.ndarray] = {}
        rows = []
        for roll in ROLLS:
            case = f"door_roll{roll}"
            meta = run_c(oracle, base, roll, out, case)
            c_idx = C.read_pnm(out / f"{case}.pgm")
            frames[roll] = c_idx
            port_ppm = out / f"{case}.port.ppm"
            pak_arg = f"{args.id1},{args.id1_pak1},{args.hipnotic}"
            cmd = [
                str(qt), "view", pak_arg, "maps/start.bsp", str(port_ppm),
                "--res", "320x200",
                "--origin", ",".join(repr(v) for v in meta["vieworg"]),
                "--angles", ",".join(repr(v) for v in meta["viewangles"]),
                "--time", repr(meta["time"]), "--fov", repr(meta["fov_x"]),
                "--ents", str(out / f"{case}.ents"),
            ]
            pres = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
            if pres.returncode != 0:
                sys.exit(f"quaketool view failed at roll {roll}:\n{pres.stdout}{pres.stderr}")
            p_rgb = C.read_pnm(port_ppm)
            c_rgb = pal[c_idx]
            st = C.stats(c_idx, c_rgb, p_rgb, pal)
            dmax = st.pop("dmax")
            exact = st.pop("exact")
            C.side_by_side(c_rgb, p_rgb, dmax, 3).save(out / f"{case}.side.png")
            rows.append((roll, st["exact_pct"], exact))

        # The door's own pixels: where id's closed (roll 0) and full-open
        # (roll 90) frames differ — isolates the swing from the room's own
        # baseline mismatch (present, unchanged, at roll 0 too).
        mask = frames[0] != frames[ROLLS[-1]]
        print(f"{'roll':>5}  {'scene exact%':>12}  {'door-pixel exact%':>18}  ({int(mask.sum())} door pixels)")
        worst = 100.0
        for roll, scene_pct, exact in rows:
            door_pct = float(exact[mask].mean() * 100) if mask.any() else float("nan")
            worst = min(worst, door_pct)
            print(f"{roll:5d}  {scene_pct:12.2f}  {door_pct:18.2f}")
        print(f"\nimages in {out}")
        if worst < 90.0:
            sys.exit(f"worst door-pixel match {worst:.2f}% is below the 90% sanity floor")


if __name__ == "__main__":
    main()
