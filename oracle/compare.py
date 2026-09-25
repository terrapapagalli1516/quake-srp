#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""Render the same view in id's software renderer (the C oracle) and in the port,
and measure how far apart they are.

    uv run oracle/compare.py                         # e1m1 e1m2 e1m3 e1m7, world + ents, 320x200
    uv run oracle/compare.py --maps e1m1 --modes world --res 640x400
    uv run oracle/compare.py --maps e1m3 --view 1100,-600,120,10,45,0 --time 3.2
    uv run oracle/compare.py --maps e1m1 --crop torch:120,90,40,30

Per case it writes, into --out (default: a fresh scratch dir it prints):
    <case>.c.ppm / .c.pgm / .c.json / .c.ents   id's frame (RGB, raw palette indices,
                                                the view/clock metadata, the entity list)
    <case>.port.ppm                             the port's frame
    <case>.side.png                             C | port | diff (2x, diff = max-channel
                                                |dRGB| x4, white = far apart)
    <case>.<crop>.png                           the same three for each --crop, at 6x
and prints one table row per case (also saved as summary.json).

The view is the C's own first frame after signon (V_CalcRefdef's eye at the
player's spawn, cl.time at that frame) unless --view/--time pin it; the port is
then handed exactly that vieworg/viewangles/cl.time. In `ents` mode the port
draws the entity list id's frame drew (the .ents file), so both renderers get the
same inputs and the diff measures rendering alone, not the simulation.
"""

from __future__ import annotations

import argparse
import json
import os
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
DEFAULT_PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
ORACLE_BIN = HERE / "build" / "quake-oracle"
BUCKETS = [(0, 0), (1, 4), (5, 8), (9, 16), (17, 32), (33, 64), (65, 255)]


def read_pak_file(pak: Path, name: str) -> bytes:
    """One file out of a PACK archive (id's pakheader_t / packfile_t layout)."""
    data = pak.read_bytes()
    magic, dirofs, dirlen = struct.unpack_from("<4sii", data, 0)
    if magic != b"PACK":
        sys.exit(f"{pak}: not a PACK file")
    for i in range(dirlen // 64):
        fname, pos, length = struct.unpack_from("<56sii", data, dirofs + 64 * i)
        if fname.split(b"\0", 1)[0].decode("latin-1") == name:
            return data[pos : pos + length]
    sys.exit(f"{name} not in {pak}")


def read_pnm(path: Path) -> np.ndarray:
    """P5 (H,W) or P6 (H,W,3) with maxval 255, as written by oracle.c / quaketool."""
    raw = path.read_bytes()
    fields, pos = [], 0
    while len(fields) < 4:
        while raw[pos : pos + 1].isspace():
            pos += 1
        start = pos
        while not raw[pos : pos + 1].isspace():
            pos += 1
        fields.append(raw[start:pos])
    pos += 1
    kind, w, h = fields[0], int(fields[1]), int(fields[2])
    ch = 3 if kind == b"P6" else 1
    arr = np.frombuffer(raw, dtype=np.uint8, count=w * h * ch, offset=pos)
    return arr.reshape((h, w, 3) if ch == 3 else (h, w))


def ensure_oracle() -> Path:
    if not ORACLE_BIN.exists():
        print("building the C oracle (oracle/build.sh) ...", file=sys.stderr)
        subprocess.run([str(HERE / "build.sh")], check=True)
    return ORACLE_BIN


def ensure_quaketool(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    crate = PROJECT / "quake-rs"
    subprocess.run(
        ["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=crate, check=True
    )
    target = Path(os.environ.get("CARGO_TARGET_DIR", crate / "target"))
    if not target.is_absolute():
        target = crate / target
    return target / "release" / "quaketool"


def run_c(args, case: str, mapname: str, ents: bool, out: Path) -> dict:
    """Render one frame in id's renderer; returns the frame's .json metadata."""
    w, h = args.res
    with tempfile.TemporaryDirectory(prefix="quake-oracle-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(args.pak.resolve())
        cmds = [
            f"viewsize {args.viewsize}",
            f"r_drawentities {1 if ents else 0}",
            f"r_drawviewmodel {1 if args.viewmodel else 0}",
            "crosshair 0",
            f"oracle_settle {args.settle}",
            f"oracle_spans {args.spans}",
        ] + args.c_cmd
        if args.view:
            cmds.append("oracle_view " + " ".join(repr(float(v)) for v in args.view))
        if args.time is not None:
            cmds.append(f"oracle_time {args.time!r}")
        cmds += [f'oracle_shot "{out / case}.c"', f"map {mapname}"]
        (base / "id1" / "oracle.cfg").write_text("\n".join(cmds) + "\n")
        cmd = [str(ensure_oracle()), "-basedir", str(base), "-width", str(w), "-height", str(h)]
        if args.aspect is not None:
            cmd += ["-oracle_aspect", str(args.aspect)]
        cmd += ["+exec", "oracle.cfg"]
        res = subprocess.run(cmd, cwd=base, capture_output=True, text=True, timeout=120)
        meta_path = out / f"{case}.c.json"
        if res.returncode != 0 or not meta_path.exists():
            sys.exit(f"C oracle failed for {case} (rc {res.returncode}):\n{res.stdout[-3000:]}{res.stderr[-2000:]}")
    return json.loads(meta_path.read_text())


def run_port(args, qt: Path, case: str, mapname: str, meta: dict, ents: bool, out: Path) -> str:
    w, h = args.res
    org, ang = meta["vieworg"], meta["viewangles"]
    cmd = [
        str(qt), "view", str(args.pak), f"maps/{mapname}.bsp", str(out / f"{case}.port.ppm"),
        "--res", f"{w}x{h}",
        "--origin", ",".join(repr(v) for v in org),
        "--angles", ",".join(repr(v) for v in ang),
        "--time", repr(meta["time"]),
        "--fov", repr(meta["fov_x"]),
    ]
    if ents:
        cmd += ["--ents", str(out / f"{case}.c.ents")]
    if args.viewmodel and meta["viewmodel"]["model"]:
        cmd += ["--viewmodel", f'{meta["viewmodel"]["model"]}:{meta["viewmodel"]["frame"]}']
    res = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    if res.returncode != 0:
        sys.exit(f"quaketool view failed for {case}:\n{res.stdout}{res.stderr}")
    return res.stdout


def recover_indices(rgb: np.ndarray, c_idx: np.ndarray, pal: np.ndarray):
    """Palette index of each port pixel: the C's own index where the colours are
    equal (so duplicate palette entries never count as a miss), else the first
    palette entry of that exact colour, else the nearest one. Also returns the
    mask of port pixels whose colour is in no palette entry at all."""
    key = (rgb[..., 0].astype(np.int32) << 16) | (rgb[..., 1].astype(np.int32) << 8) | rgb[..., 2]
    pkey = (pal[:, 0].astype(np.int32) << 16) | (pal[:, 1].astype(np.int32) << 8) | pal[:, 2]
    first = {}
    for i, k in enumerate(pkey.tolist()):
        first.setdefault(k, i)
    lut_keys = np.array(sorted(first), dtype=np.int32)
    lut_vals = np.array([first[k] for k in sorted(first)], dtype=np.int32)
    pos = np.clip(np.searchsorted(lut_keys, key), 0, len(lut_keys) - 1)
    in_pal = lut_keys[pos] == key
    idx = np.where(in_pal, lut_vals[pos], -1)
    if (~in_pal).any():
        miss = rgb[~in_pal].astype(np.int32)
        d = ((miss[:, None, :] - pal[None, :, :].astype(np.int32)) ** 2).sum(axis=2)
        idx[~in_pal] = d.argmin(axis=1)
    same = pal[c_idx].astype(np.int32)
    idx = np.where((same == rgb.astype(np.int32)).all(axis=2), c_idx, idx)
    return idx, ~in_pal


def stats(c_idx, c_rgb, p_rgb, pal) -> dict:
    p_idx, nonpal = recover_indices(p_rgb, c_idx, pal)
    drgb = np.abs(c_rgb.astype(np.int32) - p_rgb.astype(np.int32))
    dmax = drgb.max(axis=2)
    n = c_idx.size
    hist = {f"{lo}-{hi}" if lo != hi else str(lo): int(((dmax >= lo) & (dmax <= hi)).sum()) for lo, hi in BUCKETS}
    return {
        "pixels": n,
        "exact_pct": float((p_idx == c_idx).mean() * 100),
        "mean_abs_index_delta": float(np.abs(p_idx - c_idx.astype(np.int32)).mean()),
        "mean_abs_rgb_delta": float(drgb.mean()),
        "nonpalette_pct": float(nonpal.mean() * 100),
        "maxchan_hist": hist,
        "dmax": dmax,
    }


def diff_image(dmax: np.ndarray) -> np.ndarray:
    d = np.clip(dmax * 4, 0, 255).astype(np.uint8)
    return np.stack([d, d, d], axis=2)


def side_by_side(c_rgb, p_rgb, dmax, scale: int) -> Image.Image:
    gap = np.full((c_rgb.shape[0], 2, 3), (255, 0, 255), dtype=np.uint8)
    side = np.concatenate([c_rgb, gap, p_rgb, gap, diff_image(dmax)], axis=1)
    img = Image.fromarray(side)
    return img.resize((img.width * scale, img.height * scale), Image.NEAREST)


def parse_res(s: str):
    w, h = s.lower().split("x")
    return int(w), int(h)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--maps", default="e1m1,e1m2,e1m3,e1m7", help="comma-separated map names")
    ap.add_argument("--modes", default="world,ents", help="world (r_drawentities 0) and/or ents")
    ap.add_argument("--res", type=parse_res, default=(320, 200), help="WxH (default 320x200)")
    ap.add_argument("--view", type=lambda s: [float(v) for v in s.split(",")],
                    help="pin the view: x,y,z,pitch,yaw,roll (Quake convention, pitch + = down)")
    ap.add_argument("--time", type=float, help="pin cl.time for the frame")
    ap.add_argument("--settle", type=int, default=0, help="frames after signon before the shot")
    ap.add_argument("--viewmodel", action="store_true", help="draw the weapon too (r_drawviewmodel 1)")
    ap.add_argument("--viewsize", type=int, default=120, help="C scr_viewsize (120 = no status bar)")
    ap.add_argument("--spans", type=int, choices=(8, 16, 1), default=8,
                    help="C span routine: 8 = id's portable C D_DrawSpans8 (default), 16 = the asm's "
                         "16-pixel segments (d_subdiv16), 1 = exact per-pixel perspective (experiment)")
    ap.add_argument("--c-cmd", action="append", default=[],
                    help="extra C console command before the map loads (repeatable), e.g. 'd_mipscale 0'")
    ap.add_argument("--aspect", type=float, help="C vid.aspect (default 1.0, square pixels)")
    ap.add_argument("--crop", action="append", default=[], help="name:x,y,w,h — zoomed crop per case")
    ap.add_argument("--pak", type=Path, default=DEFAULT_PAK)
    ap.add_argument("--quaketool", help="use this quaketool binary instead of building quake-rs")
    ap.add_argument("--out", type=Path, help="output dir (default: a new temp dir)")
    args = ap.parse_args()

    if args.view is not None and len(args.view) != 6:
        sys.exit("--view needs x,y,z,pitch,yaw,roll")
    out = args.out or Path(tempfile.mkdtemp(prefix="quake-oracle-cmp-"))
    out.mkdir(parents=True, exist_ok=True)
    out = out.resolve()
    pal = np.frombuffer(read_pak_file(args.pak, "gfx/palette.lmp")[:768], dtype=np.uint8).reshape(256, 3)
    qt = ensure_quaketool(args.quaketool)

    w, h = args.res
    rows, summary = [], {}
    for mapname in args.maps.split(","):
        for mode in args.modes.split(","):
            if mode not in ("world", "ents"):
                sys.exit(f"unknown mode {mode!r}")
            case = f"{mapname}_{mode}_{w}x{h}"
            t0 = time.time()
            meta = run_c(args, case, mapname, mode == "ents", out)
            run_port(args, qt, case, mapname, meta, mode == "ents", out)
            c_idx = read_pnm(out / f"{case}.c.pgm")
            c_rgb = pal[c_idx]
            p_rgb = read_pnm(out / f"{case}.port.ppm")
            if p_rgb.shape != c_rgb.shape:
                sys.exit(f"{case}: size mismatch C {c_rgb.shape} vs port {p_rgb.shape}")
            st = stats(c_idx, c_rgb, p_rgb, pal)
            dmax = st.pop("dmax")
            side_by_side(c_rgb, p_rgb, dmax, 2).save(out / f"{case}.side.png")
            for spec in args.crop:
                name, box = spec.split(":")
                x, y, cw, ch = (int(v) for v in box.split(","))
                sl = (slice(y, y + ch), slice(x, x + cw))
                side_by_side(c_rgb[sl], p_rgb[sl], dmax[sl], 6).save(out / f"{case}.{name}.png")
            st.update({
                "map": mapname, "mode": mode, "res": f"{w}x{h}", "time": meta["time"],
                "vieworg": meta["vieworg"], "viewangles": meta["viewangles"], "entities": meta["entities"],
            })
            summary[case] = st
            rows.append((case, st, time.time() - t0))

    print(f"{'case':<24} {'exact%':>7} {'|dIdx|':>7} {'|dRGB|':>7} {'nonpal%':>7}  ents  max-channel |dRGB| histogram (% of pixels)")
    for case, st, _ in rows:
        hist = " ".join(f"{k}:{v * 100 / st['pixels']:.1f}" for k, v in st["maxchan_hist"].items())
        print(f"{case:<24} {st['exact_pct']:7.2f} {st['mean_abs_index_delta']:7.2f} {st['mean_abs_rgb_delta']:7.2f} "
              f"{st['nonpalette_pct']:7.2f}  {st['entities']:4d}  {hist}")
    (out / "summary.json").write_text(json.dumps(summary, indent=2))
    print(f"\nimages + summary.json in {out}")


if __name__ == "__main__":
    main()
