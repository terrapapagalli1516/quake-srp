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
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compare  # noqa: E402

# oracle -> (oracle/build.sh's ORACLE_FPMATH, compare.py's `fpcw`): x87store
# is the x87 build with no float variable in an 80-bit register
# (-ffloat-store), x87cw the x87 build in the FPU state id's x86 builds
# rendered in (24-bit precision, chop rounding)
ORACLES = {"x87": ("x87", False), "sse": ("sse", False), "x87store": ("x87store", False), "x87cw": ("x87", True)}


def oracle_path(name: str) -> str:
    """`x87`, `sse` (the tree's builds, built when missing or stale), or
    `NAME=PATH` (another build of id's C, e.g. one with other flags)."""
    if "=" in name:
        return str(Path(name.split("=", 1)[1]).resolve())
    return str(compare.ensure_oracle(fpmath=ORACLES[name][0]))


def case_args(ns: argparse.Namespace, res, view, time_, oracle: str) -> argparse.Namespace:
    """The namespace compare.py's run_c / run_port read, for one case."""
    c_cmd = []
    if ns.mip0:
        c_cmd.append("d_mipscale 0")
    return argparse.Namespace(
        res=res, view=view, time=time_, settle=0, viewmodel=False, bench=0, viewsize=120,
        spans=ns.spans, exactpersp=ns.spans == 1, perspspan=8 if ns.spans == 8 else None, c_cmd=c_cmd, c_post=[],
        demo=None, aspect=ns.aspect, oracle_dt=None, id_lightstyles=False, dlights="id", pak=ns.pak,
        pak1=None, game_dir=[], oracle=oracle_path(oracle), full=False,
        fpcw=ORACLES.get(oracle, ("", False))[1],
    )


def start_view(ns, mapname: str, res, scratch: Path) -> tuple[list[float], float]:
    """id's first frame after signon on `mapname`: its eye (x, y, z, pitch, yaw, roll) and clock."""
    a = case_args(ns, res, None, None, "sse" if "sse" in ns.oracles else "x87")
    meta = compare.run_c(a, f"{mapname}_start", mapname, False, scratch)
    return meta["vieworg"] + meta["viewangles"], meta["time"]


def views_for(ns, mapname: str, eye: list[float]) -> list[tuple[str, list[float]]]:
    out = []
    kinds = ns.views.split(",")
    if "standard" in kinds:
        out.append(("std", eye))
    if "sweep" in kinds:
        for dy in range(0, 360, 60):
            for p in (-25.0, 0.0, 30.0):
                out.append((f"y{dy}p{int(p)}", eye[:3] + [p, eye[4] + dy, 0.0]))
    if "roll" in kinds:
        for p in (-10.0, 20.0):
            for r in (-15.0, 12.0):
                out.append((f"p{int(p)}r{int(r)}", eye[:3] + [p, eye[4] + 37.0, r]))
    return out


def run_case(ns, qt: Path, oracle_name: str, mapname: str, res, label: str, view, time_, out: Path, pal):
    ents = ns.mode == "ents"
    case = f"{mapname}_{label}_{res[0]}x{res[1]}_{ns.mode}_{oracle_name.split('=')[0]}"
    a = case_args(ns, res, view, time_, oracle_name)
    meta = compare.run_c(a, case, mapname, ents, out)
    compare.run_port(a, qt, case, mapname, meta, ents, out)
    c_idx = compare.read_pnm(out / f"{case}.c.pgm")
    p_rgb = compare.read_pnm(out / f"{case}.port.ppm")
    p_idx, _ = compare.recover_indices(p_rgb, c_idx, pal)
    ys, xs = np.nonzero(p_idx != c_idx)
    diffs = [[int(x), int(y), int(c_idx[y, x]), int(p_idx[y, x])] for y, x in zip(ys, xs)]
    return {"case": case, "map": mapname, "label": label, "res": f"{res[0]}x{res[1]}", "oracle": oracle_name.split("=")[0],
            "view": view, "time": time_, "pixels": int(c_idx.size), "diff": len(diffs), "diffs": diffs[:4000]}


def sweep(ns: argparse.Namespace, qt: Path, pal: np.ndarray, out: Path) -> bool:
    """Run every case into `out` and print the table; whether every case
    against the SSE build (or, without it, the x87 build) is identical."""
    t0 = time.time()

    jobs = []
    for res_s in ns.res.split(","):
        res = compare.parse_res(res_s)
        for mapname in ns.maps.split(","):
            eye, clock = start_view(ns, mapname, res, out)
            for label, view in views_for(ns, mapname, eye):
                for oracle_name in ns.oracles.split(","):
                    jobs.append((oracle_name, mapname, res, label, view, clock))
    with ThreadPoolExecutor(ns.jobs) as pool:
        results = list(pool.map(lambda j: run_case(ns, qt, j[0], j[1], j[2], j[3], j[4], j[5], out, pal), jobs))

    spans = {16: "16-pixel spans", 8: "8-pixel spans", 1: "exact perspective"}[ns.spans]
    mode = ns.mode + ", " + spans + (", mip 0" if ns.mip0 else "")
    print(f"{len(jobs)} cases ({mode}{f', aspect {ns.aspect}' if ns.aspect else ''}), {time.time() - t0:.0f} s")
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
    ap.add_argument("--spans", type=int, choices=(16, 8, 1), default=16,
                    help="16: id's x86 spans (D_DrawSpans16, Turbulent8), the port's Classic; 8: id's portable C "
                         "D_DrawSpans8 against the port's --perspspan 8; 1: the port's exact perspective extra "
                         "(--exactpersp) against the oracle's transcription of it")
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    ap.add_argument("--pak", type=Path, default=compare.DEFAULT_PAK)
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
        with tempfile.TemporaryDirectory(prefix="quake-exact-", dir=os.environ.get("QUAKE_SCRATCH")) as tmp:
            identical = sweep(ns, qt, pal, Path(tmp))
    sys.exit(0 if identical else 1)



if __name__ == "__main__":
    main()
