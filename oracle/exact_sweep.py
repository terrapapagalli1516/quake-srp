#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Count the pixels where the port's Classic frame differs from id's C, over
many views, against the x87 build of id's C and the SSE2 build — the
bit-exactness check of oracle/README.md, "Bit for bit".

    uv run oracle/exact_sweep.py                          # standard + swept views, 320x200, both oracles
    uv run oracle/exact_sweep.py --views standard --res 640x480,1280x1024
    uv run oracle/exact_sweep.py --oracles sse --maps e1m1 --views sweep --keep DIR
    uv run oracle/exact_sweep.py --mip0 --spans 1        # both renderers at mip 0, exact perspective
    uv run oracle/exact_sweep.py --maps e1m1,e1m2,e1m3,e1m4,e1m5,e1m6,e1m7,e1m8 --views cases,monsters \
        --mode ents --aspect 0.8333333                    # the player among monsters, awake

Every case is one view: id's C renders it in its own process (fresh edge and
surface caches, as the port's `quaketool view` has), and the port renders the
same eye, angles and clock (compare.py's `run_c` / `run_port`). A case's
count is the pixels whose palette index differs (duplicate palette colours
count as equal, as compare.py's). The table prints, per oracle, the cases
with a difference and the total; `--keep DIR` keeps every frame and writes
`DIR/diffs.json` (each differing pixel: x, y, id's index, the port's), the
input of `pixel_trace.py`. The exit status is 0 when every case is identical
to the SSE build's frame (the target: id's C in strict IEEE single precision;
the x87 build's own extended-precision registers are reported, not judged),
or with `--oracles x87` alone, to the x87 build's.

Views (`--views`, comma-separated):
  standard  each map's first frame after signon (V_CalcRefdef's eye, id's clock)
  sweep     that eye, yaw +0/60/../300 x pitch -25/0/+30: 18 a map
  roll      that eye, rolled -15 and +12 at pitches -10 and +20: 4 a map
  monsters  the player among the map's monsters: around its groups of them
            (the entity lump's, normal skill), a camera every 45 degrees, 22
            units over the group's middle monster and 260 units out (or 200,
            150, 340: the first in the open with a clear line to it; a yaw
            with none is left out), looking back at it; `--monster-views` a
            map, taken a yaw at a time across the groups. Once the player is
            in the game id's console moves him there (noclip, god: the
            monsters wake, turn and attack), and the shot is the frame
            `--monster-settle` after signon, at id's clock. id's monsters,
            their missiles, particles and lights are what both renderers draw
            (`--mode ents`). A camera id puts inside solid is skipped (the
            count is printed)
  cases     fixed views of the maps given (CASES): four proof frames with the
            monsters awake (made for an explainer film of the port), and the
            18 views that differed when 1,967 views were searched for them,
            each fixed since, at the settle and setup it was found at

A view of `monsters` or `cases` runs id's game for its settle frames: the
clock is id's at that frame (not pinned), and the port is handed id's entity
list, particles, dynamic lights and light-style strings of it (compare.py's).
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import struct
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compare  # noqa: E402
import scratch  # noqa: E402

# oracle -> (oracle/build.sh's ORACLE_FPMATH, compare.py's `fpcw`): x87store
# is the x87 build with no float variable in an 80-bit register
# (-ffloat-store), x87cw the x87 build in the FPU state id's x86 builds
# rendered in (24-bit precision, chop rounding)
ORACLES = {"x87": ("x87", False), "sse": ("sse", False), "x87store": ("x87store", False), "x87cw": ("x87", True)}


# The views of `--views cases`: (map, label, view x,y,z,pitch,yaw,roll, how the
# player gets there, settle). How: "awake" moves the player 22 units under the
# eye with noclip and god once he is in the game (the monsters wake), "asleep"
# the same with notarget, None leaves him at the start (the view alone moves).
CASES = [
    # Four proof frames with the monsters awake (made for an explainer film of the port).
    ("e1m1", "proof", [596.7, 2808, -34, 4, 180, 0], "awake", 15),  # three grunts down the skylit hall, dogs behind
    ("e1m2", "proof", [1543, 1385, 226, 4, 270, 0], "awake", 19),  # a grunt's muzzle flash, an ogre, blood
    ("e1m3", "proof", [1283.8, 912.2, 582, 4, 315, 0], "awake", 11),  # a fiend in mid-leap, one on the rug
    ("e1m5", "proof", [160.7, 2549.3, 446, 4, 0, 0], "awake", 15),  # two knights at the hall's door
    # The 18 views that differed when 1,967 views with monsters were searched for those
    # four, each fixed since.
    # A trigger_once had put e1m1's style 32 out: id's light-style strings.
    ("e1m1", "lightsoff", [78, 2392, 62, 4, 0, 0], "asleep", 12),
    ("e1m1", "lightsoff2", [78, 2392, 110, 19.4, 0, 0], "asleep", 12),
    # The eye on e1m3's pool plane, z -368: Mod_PointInLeaf's leaf, warped.
    *(("e1m3", f"pool{i}", [x, y, -368, p, yaw, 0], "asleep", 12) for i, (x, y, p, yaw) in enumerate([
        (-1123.8, -424.8, 19.4, 225), (-1427.8, -361.2, 13, 315), (-1427.8, -728.8, 13, 45),
        (-1244, -375, 19.4, 270), (-984, -545, 13, 180), (-1060.2, -361.2, 13, 225), (-975.3, -276.3, 9, 225),
        (-1414, -545, 19.4, 0), (-1364.2, -424.8, 19.4, 315), (-1364.2, -665.2, 19.4, 45), (-1624, -545, 9, 0),
        (-1074, -545, 19.4, 180), (-1123.8, -665.2, 19.4, 135), (-1060.2, -728.8, 13, 135)])),
    # D_DrawZSpans' pair stores: an armour, a key and an ogre at column 1; a fiend's edge pixel.
    ("e1m6", "column1", [-79.3, 1283.3, 110, 1.8, 135, 0], "asleep", 12),
    ("e1m5", "fiendedge", [-868.2, 1728.2, 174, 4, 315, 0], "asleep", 12),
    # The pool view with the player left at the start: the first frame, as found in world mode.
    ("e1m3", "pool", [-1123.8, -424.8, -368, 19.4, 225, 0], None, 0),
]

# `--views monsters`' cameras (monster_views): every 45 degrees around a group,
# 22 units over its middle monster, the first of these distances out whose eye
# is in the open with a clear line to it.
MONSTER_YAWS = range(0, 360, 45)
MONSTER_DISTS, MONSTER_RISE = (260.0, 200.0, 150.0, 340.0), 22.0


class BspTree:
    """A map's render tree, enough for Mod_PointInLeaf: its planes, nodes and
    leaves' contents (BSP29 lumps 1, 5 and 10), and its entities (lump 0)."""

    def __init__(self, pak: Path, mapname: str):
        bsp = compare.read_pak_file(pak, f"maps/{mapname}.bsp")

        def lump(i: int) -> bytes:
            ofs, size = struct.unpack_from("<ii", bsp, 4 + 8 * i)
            return bsp[ofs:ofs + size]

        self.planes = [struct.unpack_from("<4f", lump(1), o) for o in range(0, len(lump(1)), 20)]
        self.nodes = [struct.unpack_from("<i2h", lump(5), o) for o in range(0, len(lump(5)), 24)]
        self.leaves = [struct.unpack_from("<i", lump(10), o)[0] for o in range(0, len(lump(10)), 28)]
        text = lump(0).decode("latin-1")
        self.entities = [dict(re.findall(r'"([^"]*)"\s+"([^"]*)"', b)) for b in re.findall(r"\{([^}]*)\}", text)]

    def contents(self, p) -> int:
        """The contents of the leaf `p` is in: Mod_PointInLeaf's walk (on a
        plane, the back child) from the world's head node, 0."""
        n = 0
        while n >= 0:
            planenum, front, back = self.nodes[n]
            a, b, c, dist = self.planes[planenum]
            n = front if a * p[0] + b * p[1] + c * p[2] - dist > 0 else back
        return self.leaves[-n - 1]

    def clear(self, p, q) -> bool:
        """Whether the line from `p` to `q` stays out of solid (every 4 units)."""
        steps = max(1, int(math.dist(p, q) / 4))
        return all(self.contents([p[k] + (q[k] - p[k]) * i / steps for k in range(3)]) != -2 for i in range(steps + 1))


def monster_groups(tree: BspTree) -> list[list[float]]:
    """The map's groups of monsters at normal skill (not `SPAWNFLAG_NOT_MEDIUM`;
    Chthon, `monster_boss`, rises only on his trigger), the largest first:
    monsters within 360 units across and 96 up of one another, none within 150
    units of a group already taken. Each group is given by its member nearest
    its centre (a centre can be inside a wall between two rooms)."""
    ms = []
    for e in tree.entities:
        cn = e.get("classname", "")
        if cn.startswith("monster_") and cn != "monster_boss" and not int(e.get("spawnflags", "0") or 0) & 512:
            ms.append([float(v) for v in e["origin"].split()])
    groups, seen = [], set()
    for o in ms:
        g = frozenset(j for j, q in enumerate(ms) if math.dist(o[:2], q[:2]) < 360 and abs(o[2] - q[2]) < 96)
        if g not in seen:
            seen.add(g)
            centre = [sum(ms[j][k] for j in g) / len(g) for k in range(3)]
            groups.append((len(g), min((ms[j] for j in sorted(g)), key=lambda q: math.dist(q, centre))))
    groups.sort(key=lambda t: -t[0])  # stable: the lump's order breaks ties
    keep: list[list[float]] = []
    for _, c in groups:
        if all(math.dist(c, k) > 150 for k in keep):
            keep.append(c)
    return keep


def monster_views(tree: BspTree, count: int, settle: int) -> list[tuple[str, list[float], int, list[str]]]:
    """`--views monsters`: up to `count` cameras, taken a yaw at a time across
    the groups (every group's 0 degrees, then every group's 45, ...), so that
    a map's views look at as many groups as they can. A camera is 22 units
    over its monster and the first of MONSTER_DISTS out whose eye is in the
    open with a clear line to the monster; it looks back at a point 10 units
    over it."""
    out = []
    groups = monster_groups(tree)
    for a in MONSTER_YAWS:
        for g, c in enumerate(groups):
            target = [c[0], c[1], c[2] + 10.0]
            for r in MONSTER_DISTS:
                cam = [round(c[0] + r * math.cos(math.radians(a)), 1),
                       round(c[1] + r * math.sin(math.radians(a)), 1), round(c[2] + MONSTER_RISE, 1)]
                if tree.contents(cam) != -2 and tree.clear(cam, target):
                    pitch = round(math.degrees(math.atan2(cam[2] - target[2], r)), 1)
                    view = cam + [pitch, (a + 180) % 360, 0.0]
                    out.append((f"g{g}a{a}", view, settle, player_setup("awake", view)))
                    break
            if len(out) == count:
                return out
    return out


def player_setup(how: str | None, view: list[float]) -> list[str]:
    """id's console commands that put the player under the eye (compare.py's
    `--c-post`): once he is in the game (six frames), noclip, then god (the
    monsters see him) or notarget (they do not), then his origin."""
    if how is None:
        return []
    origin = " ".join(f"{v:g}" for v in (view[0], view[1], view[2] - 22.0))
    return ["wait"] * 6 + ["noclip", "god" if how == "awake" else "notarget", f"oracle_field origin {origin}"]


def oracle_path(name: str) -> str:
    """`x87`, `sse` (the tree's builds, built when missing or stale), or
    `NAME=PATH` (another build of id's C, e.g. one with other flags)."""
    if "=" in name:
        return str(Path(name.split("=", 1)[1]).resolve())
    return str(compare.ensure_oracle(fpmath=ORACLES[name][0]))


def case_args(ns: argparse.Namespace, res, view, time_, oracle: str, settle: int = 0,
              c_post: list[str] | None = None) -> argparse.Namespace:
    """The namespace compare.py's run_c / run_port read, for one case."""
    c_cmd = []
    if ns.mip0:
        c_cmd.append("d_mipscale 0")
    return argparse.Namespace(
        res=res, view=view, time=time_, settle=settle, viewmodel=False, bench=0, viewsize=120,
        spans=ns.spans, exactpersp=ns.spans == 1, perspspan=8 if ns.spans == 8 else None, c_cmd=c_cmd,
        c_post=c_post or [],
        demo=None, aspect=ns.aspect, oracle_dt=None, id_lightstyles=ns.id_lightstyles, dlights="id", pak=ns.pak,
        pak1=ns.pak1, game_dir=ns.game_dir, oracle=oracle_path(oracle), full=False,
        fpcw=ORACLES.get(oracle, ("", False))[1],
    )


def start_view(ns, mapname: str, res, scratch: Path) -> tuple[list[float], float]:
    """id's first frame after signon on `mapname`: its eye (x, y, z, pitch, yaw, roll) and clock."""
    a = case_args(ns, res, None, None, "sse" if "sse" in ns.oracles else "x87")
    meta = compare.run_c(a, f"{mapname}_start", mapname, False, scratch)
    return meta["vieworg"] + meta["viewangles"], meta["time"]


def views_for(ns, mapname: str, eye: list[float] | None) -> list[tuple[str, list[float], int, list[str]]]:
    """The map's views: (label, view, settle, id's console setup); `eye` is
    the map's first frame's, which the standard, swept and rolled views need."""
    out = []
    kinds = ns.views.split(",")
    if "standard" in kinds:
        out.append(("std", eye, 0, []))
    if "sweep" in kinds:
        for dy in range(0, 360, 60):
            for p in (-25.0, 0.0, 30.0):
                out.append((f"y{dy}p{int(p)}", eye[:3] + [p, eye[4] + dy, 0.0], 0, []))
    if "roll" in kinds:
        for p in (-10.0, 20.0):
            for r in (-15.0, 12.0):
                out.append((f"p{int(p)}r{int(r)}", eye[:3] + [p, eye[4] + 37.0, r], 0, []))
    if "monsters" in kinds:
        out += monster_views(BspTree(ns.pak, mapname), ns.monster_views, ns.monster_settle)
    if "cases" in kinds:
        for m, label, view, how, settle in CASES:
            if m == mapname:
                out.append((label, view, settle, player_setup(how, view)))
    return out


def run_case(ns, qt: Path, oracle_name: str, mapname: str, res, label: str, view, time_, settle: int,
             c_post: list[str], out: Path, pal):
    ents = ns.mode == "ents"
    case = f"{mapname}_{label}_{res[0]}x{res[1]}_{ns.mode}_{oracle_name.split('=')[0]}"
    # a settled view runs id's game to that frame: its clock is id's, not pinned
    a = case_args(ns, res, view, None if settle else time_, oracle_name, settle, c_post)
    meta = compare.run_c(a, case, mapname, ents, out)
    if meta["viewleaf_contents"] == -2:  # CONTENTS_SOLID: no view a player can have
        return {"case": case, "map": mapname, "label": label, "oracle": oracle_name.split("=")[0], "solid": True}
    compare.run_port(a, qt, case, mapname, meta, ents, out)
    c_idx = compare.read_pnm(out / f"{case}.c.pgm")
    p_rgb = compare.read_pnm(out / f"{case}.port.ppm")
    p_idx, _ = compare.recover_indices(p_rgb, c_idx, pal)
    ys, xs = np.nonzero(p_idx != c_idx)
    diffs = [[int(x), int(y), int(c_idx[y, x]), int(p_idx[y, x])] for y, x in zip(ys, xs)]
    return {"case": case, "map": mapname, "label": label, "res": f"{res[0]}x{res[1]}", "oracle": oracle_name.split("=")[0],
            "view": view, "time": meta["time"], "settle": settle, "pixels": int(c_idx.size), "diff": len(diffs),
            "entities": meta["entities"], "diffs": diffs[:4000]}


def sweep(ns: argparse.Namespace, qt: Path, pal: np.ndarray, out: Path) -> bool:
    """Run every case into `out` and print the table; whether every case
    against the SSE build (or, without it, the x87 build) is identical."""
    t0 = time.time()

    jobs = []
    for res_s in ns.res.split(","):
        res = compare.parse_res(res_s)
        for mapname in ns.maps.split(","):
            # the first frame's eye, for the views made from it
            needs_eye = {"standard", "sweep", "roll"} & set(ns.views.split(","))
            eye, clock = start_view(ns, mapname, res, out) if needs_eye else (None, None)
            for label, view, settle, c_post in views_for(ns, mapname, eye):
                for oracle_name in ns.oracles.split(","):
                    jobs.append((oracle_name, mapname, res, label, view, clock, settle, c_post))
    with ThreadPoolExecutor(ns.jobs) as pool:
        results = list(pool.map(lambda j: run_case(ns, qt, *j[:8], out, pal), jobs))
    solid = [r for r in results if r.get("solid")]
    results = [r for r in results if not r.get("solid")]

    spans = {16: "16-pixel spans", 8: "8-pixel spans", 1: "exact perspective"}[ns.spans]
    mode = ns.mode + ", " + spans + (", mip 0" if ns.mip0 else "")
    skipped = f", {len(solid)} skipped (the eye in solid)" if solid else ""
    print(f"{len(results)} cases ({mode}{f', aspect {ns.aspect}' if ns.aspect else ''}){skipped}, "
          f"{time.time() - t0:.0f} s")
    for oracle_name in (o.split("=")[0] for o in ns.oracles.split(",")):
        rows = [r for r in results if r["oracle"] == oracle_name]
        bad = sorted((r for r in rows if r["diff"]), key=lambda r: -r["diff"])
        total = sum(r["diff"] for r in rows)
        px = sum(r["pixels"] for r in rows)
        print(f"  {oracle_name}: {len(rows) - len(bad)}/{len(rows)} cases identical, {total} differing pixels "
              f"of {px} ({100.0 * (px - total) / px:.4f}% exact)")
        for r in bad[: ns.show]:
            print(f"    {r['case']:<36} {r['diff']:6d}  view {','.join(f'{v:g}' for v in r['view'])}")
    if ns.keep:
        (out / "diffs.json").write_text(json.dumps(results))
        print(f"frames and diffs.json in {out}")
    judged = "sse" if "sse" in ns.oracles.split(",") else "x87"
    return not any(r["diff"] for r in results if r["oracle"] == judged)



def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--maps", default="e1m1,e1m2,e1m3,e1m7")
    ap.add_argument("--views", default="standard,sweep")
    ap.add_argument("--mode", choices=("world", "ents"), default="world",
                    help="world: r_drawentities 0; ents: the port draws id's entity list (compare.py's modes)")
    ap.add_argument("--res", default="320x200", help="comma-separated WxH list")
    ap.add_argument("--oracles", default="x87,sse",
                    help="x87 and/or sse: id's C with x87 floats and with SSE2 floats (oracle/build.sh, "
                         "ORACLE_FPMATH=sse), each built when missing or stale; x87store: x87 with -ffloat-store; "
                         "x87cw: the x87 build in id's x86 rendering FPU state (compare.py --fpcw); or NAME=PATH")
    ap.add_argument("--aspect", type=float, help="vid.aspect for both renderers (compare.py --aspect)")
    ap.add_argument("--mip0", action="store_true", help="both renderers at mip 0 (d_mipscale 0)")
    ap.add_argument("--id-lightstyles", action="store_true",
                    help="hand the port id's light-style values of each frame (compare.py's), not only id's "
                         "light-style strings (the default), so R_AnimateLight is id's too")
    ap.add_argument("--spans", type=int, choices=(16, 8, 1), default=16,
                    help="16: id's x86 spans (D_DrawSpans16, Turbulent8), the port's Classic; 8: id's portable C "
                         "D_DrawSpans8 against the port's --perspspan 8; 1: the port's exact perspective extra "
                         "(--exactpersp) against the oracle's transcription of it")
    ap.add_argument("--monster-views", type=int, default=12,
                    help="--views monsters: at most this many cameras a map, across its groups of monsters")
    ap.add_argument("--monster-settle", type=int, default=12,
                    help="--views monsters: the frame after signon shot (the player is moved at the sixth)")
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    ap.add_argument("--pak", type=Path, default=compare.DEFAULT_PAK)
    ap.add_argument("--pak1", type=Path, help="id1's registered pak1.pak (compare.py --pak1): its maps, and the packs'")
    ap.add_argument("--game-dir", nargs=2, metavar=("NAME", "DIR"), action="append", default=[],
                    help="a mission pack's game directory, as compare.py's (--game-dir hipnotic DIR)")
    ap.add_argument("--quaketool", help="this quaketool binary instead of building quake-rs")
    ap.add_argument("--keep", type=Path, help="keep every frame here, and diffs.json")
    ap.add_argument("--show", type=int, default=12, help="list at most this many differing cases per oracle")
    ns = ap.parse_args()

    for name in ns.oracles.split(","):
        if name not in ORACLES and "=" not in name:
            sys.exit(f"--oracles: x87, sse or NAME=PATH, not {name!r}")
        oracle_path(name)  # built once here, not by every case
    qt = compare.ensure_quaketool(ns.quaketool)
    pal = np.frombuffer(compare.read_pak_file(ns.pak, "gfx/palette.lmp")[:768], dtype=np.uint8).reshape(256, 3)
    if ns.keep:
        ns.keep.mkdir(parents=True, exist_ok=True)
        identical = sweep(ns, qt, pal, ns.keep.resolve())
    else:
        # The frames are only needed while the cases are counted.
        with scratch.tempdir("quake-exact-") as tmp:
            identical = sweep(ns, qt, pal, Path(tmp))
    sys.exit(0 if identical else 1)



if __name__ == "__main__":
    main()
