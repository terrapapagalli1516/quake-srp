#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Where a pixel of the port's frame parts from id's: one view rendered by id's
C and by the port, each writing the stages of its frame (id's `oracle_stages`,
`quaketool view --stages`), and the two put side by side, stage by stage, to
the first one whose values differ.

    uv run oracle/pixel_trace.py --maps e1m1                      # the first frame, x87 build
    uv run oracle/pixel_trace.py --maps e1m3 --sse --view=-735.96875,-1591.96875,98.03125,30,210,0
    uv run oracle/pixel_trace.py --maps e1m1 --pixel 145,91 --pixel 146,91
    uv run oracle/pixel_trace.py --maps e1m2 --mode ents --res 640x400 --aspect 0.8333333

The stages, each record compared to the bit (floats are their IEEE bits):

  frame      vpn/vright/vup (AngleVectors), R_ViewChanged's xcenter, xscale,
             xscaleinv ..., horizontalFieldOfView, the screenedge and clip
             planes, skytime
  edges      every edge R_ScanEdges starts with, row by row: its 20.20 u and
             u_step, its surfaces, its nearest 1/z, its last row
  spans      every span R_ScanEdges cut: row, first pixel, count, surface
  surface    each surface's sort key, flags, nearest 1/z (the mip level's
             input) and its 1/z plane (R_RenderFace)
  gradients  what D_CalcGradients left the span routine: the eye along the
             view axes, the s/z and t/z planes, sadjust, tadjust, the
             extents, the mip level, and a hash of the surface block (or
             liquid) it reads — the lighting and the texture
  pixel      the palette index

For every differing pixel (or each --pixel) it lists the stages that differ
along the pixel's own path — the frame, its row's edges, the span that
covers it, that span's surface and gradients and block — with the values;
the last is the nearest cause. If the span, its surface, gradients and
block are the same, the difference is in the span routine's per-pixel
arithmetic (or in what is drawn after the world: the entities, particles).
Then a count of the pixels by their nearest differing stage.

The view options are compare.py's; the frames and the stage files are kept
in --out (default: a fresh directory in $QUAKE_SCRATCH or the system's).
"""

from __future__ import annotations

import argparse
import collections
import os
import struct
import sys
import tempfile
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compare  # noqa: E402


def f32(bits: str) -> float:
    return struct.unpack("<f", struct.pack("<I", int(bits, 16)))[0]


def parse_stages(path: Path) -> dict:
    """The stage records: frame fields, edges by row, surfaces and gradients
    by id, and the spans (a frame drawn in several batches repeats a
    surface's records: the union is kept)."""
    st = {"F": {}, "E": collections.defaultdict(list), "S": {}, "G": {}, "P": set()}
    for line in path.read_text().splitlines():
        f = line.split()
        if not f:
            continue
        kind = f[0]
        if kind == "F":
            st["F"] = dict(kv.split("=", 1) for kv in f[1:])
        elif kind == "E":
            # E v u u_step s0 s1 nearzi=.. last=..
            st["E"][int(f[1])].append(tuple(f[2:]))
        elif kind == "S":
            st["S"][f[1]] = dict(kv.split("=", 1) for kv in f[2:])
        elif kind == "G":
            st["G"][f[1]] = {"kind": f[2], **dict(kv.split("=", 1) for kv in f[3:])}
        elif kind == "P":
            st["P"].add((int(f[1]), int(f[2]), int(f[3]), f[4]))
    for v in st["E"]:
        st["E"][v].sort()
    # id's flags carry SURF_DRAWTILED (0x20) on sky and liquids; the port's do not
    for rec in st["S"].values():
        rec["flags"] = str(int(rec["flags"]) & ~0x20)
    return st


def show(v: str) -> str:
    """A value as read: a float's bits with the float, anything else as is."""
    if len(v) == 8 and all(c in "0123456789abcdef" for c in v):
        return f"{v} ({f32(v):.9g})"
    return v


def field_diffs(c: dict | None, p: dict | None) -> list[str]:
    if c is None or p is None:
        return [f"only in {'the port' if c is None else 'C'}"]
    keys = sorted(set(c) | set(p), key=lambda k: list(c).index(k) if k in c else 999)
    return [f"{k}: C {show(c.get(k, '-'))}  port {show(p.get(k, '-'))}" for k in keys if c.get(k) != p.get(k)]


def span_at(spans: set, x: int, y: int):
    for v, u, n, sid in spans:
        if v == y and u <= x < u + n:
            return (v, u, n, sid)
    return None


def trace_pixel(c: dict, p: dict, x: int, y: int) -> list[tuple[str, list[str]]]:
    """The stages along pixel (x, y)'s path whose records differ, from the
    frame down to the gradients and the block of the surface it shows, each
    with what differs: the last one is the nearest cause."""
    found = []
    fd = field_diffs(c["F"], p["F"])
    if fd:
        found.append(("frame", fd))
    if c["E"].get(y, []) != p["E"].get(y, []):
        ce, pe = set(c["E"].get(y, [])), set(p["E"].get(y, []))
        why = [f"C only: {' '.join(e)}" for e in sorted(ce - pe)[:3]]
        why += [f"port only: {' '.join(e)}" for e in sorted(pe - ce)[:3]]
        found.append((f"edges (row {y})", why))
    cs, ps = span_at(c["P"], x, y), span_at(p["P"], x, y)
    if cs != ps:
        found.append(("span", [f"C {cs}", f"port {ps}"]))
        return found
    if cs is None:
        return found + [("none", ["no span covers it on either side"])]
    sid = cs[3]
    sd = field_diffs(c["S"].get(sid), p["S"].get(sid))
    if sd:
        found.append((f"surface {sid}", sd))
    if sid in c["G"] or sid in p["G"]:
        diffs = field_diffs(c["G"].get(sid), p["G"].get(sid))
        gd = [d for d in diffs if not d.startswith("hash")]
        if gd:
            found.append((f"gradients {sid}", gd))
        elif diffs:
            found.append((f"block {sid}", diffs))
    if not found or not (found[-1][0].startswith(("gradients", "block", "surface"))):
        found.append((f"pixel {sid}", [f"span {cs}: its surface, gradients and block are the same; the span "
                                       f"routine's per-pixel arithmetic (or what is drawn over the world) parts"]))
    return found


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--maps", default="e1m1", help="one map")
    ap.add_argument("--mode", choices=("world", "ents"), default="world")
    ap.add_argument("--res", type=compare.parse_res, default=(320, 200))
    ap.add_argument("--view", type=lambda s: [float(v) for v in s.split(",")], help="x,y,z,pitch,yaw,roll")
    ap.add_argument("--time", type=float)
    ap.add_argument("--settle", type=int, default=0)
    ap.add_argument("--aspect", type=float)
    ap.add_argument("--spans", type=int, choices=(16, 8, 1), default=16)
    ap.add_argument("--c-cmd", action="append", default=[], help="as compare.py's (d_mipscale 0 ...)")
    ap.add_argument("--sse", action="store_true", help="id's C built with SSE2 floats (the port's target)")
    ap.add_argument("--oracle", help="another build of id's C (default: the tree's x87 build, or --sse's)")
    ap.add_argument("--pixel", action="append", default=[], help="x,y to trace (default: every differing pixel)")
    ap.add_argument("--show", type=int, default=8, help="pixels to trace in full (the rest are counted)")
    ap.add_argument("--pak", type=Path, default=compare.DEFAULT_PAK)
    ap.add_argument("--quaketool")
    ap.add_argument("--out", type=Path)
    ns = ap.parse_args()

    out = ns.out or Path(tempfile.mkdtemp(prefix="quake-trace-", dir=os.environ.get("QUAKE_SCRATCH")))
    out.mkdir(parents=True, exist_ok=True)
    out = out.resolve()
    mapname, ents = ns.maps, ns.mode == "ents"
    case = f"{mapname}_{ns.mode}_{ns.res[0]}x{ns.res[1]}"
    args = argparse.Namespace(
        res=ns.res, view=ns.view, time=ns.time, settle=ns.settle, viewmodel=False, bench=0, viewsize=120,
        spans=ns.spans, exactpersp=ns.spans == 1, perspspan=8 if ns.spans == 8 else None,
        c_cmd=ns.c_cmd + [f'oracle_stages "{out / case}.c.stages"'], c_post=[], demo=None, aspect=ns.aspect,
        oracle_dt=None, id_lightstyles=False, dlights="id", pak=ns.pak, pak1=None, game_dir=[], oracle=ns.oracle,
        sse=ns.sse, full=False, stages=True,
    )
    qt = compare.ensure_quaketool(ns.quaketool)
    meta = compare.run_c(args, case, mapname, ents, out)
    compare.run_port(args, qt, case, mapname, meta, ents, out)
    c, p = parse_stages(out / f"{case}.c.stages"), parse_stages(out / f"{case}.port.stages")

    pal = np.frombuffer(compare.read_pak_file(ns.pak, "gfx/palette.lmp")[:768], dtype=np.uint8).reshape(256, 3)
    c_idx = compare.read_pnm(out / f"{case}.c.pgm")
    p_idx, _ = compare.recover_indices(compare.read_pnm(out / f"{case}.port.ppm"), c_idx, pal)
    build = ns.oracle or ("SSE" if ns.sse else "x87")
    v = meta["vieworg"] + meta["viewangles"]
    print(f"{case} against id's {build} build: view {','.join(f'{x:.9g}' for x in v)} t={meta['time']!r}")

    # Every stage, whole.
    fd = field_diffs(c["F"], p["F"])
    rows = sorted(set(c["E"]) | set(p["E"]))
    erows = [r for r in rows if c["E"].get(r, []) != p["E"].get(r, [])]
    sids = sorted(set(c["S"]) | set(p["S"]))
    sdiff = [s for s in sids if field_diffs(c["S"].get(s), p["S"].get(s))]
    gids = sorted(set(c["G"]) | set(p["G"]))
    gdiff = [g for g in gids if [d for d in field_diffs(c["G"].get(g), p["G"].get(g)) if not d.startswith("hash")]]
    bdiff = [g for g in gids if g not in gdiff and field_diffs(c["G"].get(g), p["G"].get(g))]
    pdiff = c["P"] ^ p["P"]
    ys, xs = np.nonzero(p_idx != c_idx)
    print(f"  frame      {len(fd)} of {len(c['F'])} fields differ" + (": " + "; ".join(fd[:4]) if fd else ""))
    print(f"  edges      {len(erows)} of {len(rows)} rows differ")
    print(f"  spans      {len(pdiff)} spans in one and not the other, of {len(c['P'])}")
    print(f"  surface    {len(sdiff)} of {len(sids)} surfaces differ")
    print(f"  gradients  {len(gdiff)} of {len(gids)} differ, and {len(bdiff)} blocks")
    print(f"  pixel      {len(xs)} of {c_idx.size} differ")

    pixels = [tuple(int(t) for t in s.split(",")) for s in ns.pixel] or list(zip(xs.tolist(), ys.tolist()))
    tally = collections.Counter()
    for k, (x, y) in enumerate(pixels):
        found = trace_pixel(c, p, x, y)
        tally[found[-1][0].split()[0] if found else "nothing"] += 1
        if k < ns.show:
            print(f"\npixel ({x},{y}): id's index {c_idx[y, x]}, the port's {p_idx[y, x]}")
            for stage, why in found:
                print(f"  {stage}")
                for w in why[:6]:
                    print(f"      {w}")
    if pixels:
        print("\npixels by the nearest stage that differs on their path: "
              + ", ".join(f"{k} {n}" for k, n in tally.most_common()))
    print(f"\nframes and stages in {out}")


if __name__ == "__main__":
    main()
