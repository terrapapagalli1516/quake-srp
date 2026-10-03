#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy", "pillow"]
# ///
"""A recorded demo's dynamic lights, in pixels: id's frame of a demo against the
port's, with no lights (what demo playback drew before), with the lights the
port's own playback makes, and with id's.

    uv run oracle/demo_lights.py                       # the standard frames, a table and images
    uv run oracle/demo_lights.py --frames demo1:323,demo3:371
    uv run oracle/demo_lights.py --sample 40           # + 40 lit frames spread over the loop
    uv run oracle/demo_lights.py --out DIR

`demo_lerp.py` shows that the port's playback makes id's lights, slot for
slot (the radius of a muzzle flash, bright or dim light only up to
`rand()&31`, the one thing two generators cannot share). This shows what that
is in pixels. id's side plays the demo at the port's 1/72 s step and shoots
frame K of it (the `demoN:K` of --frames is `quaketool play`'s frame index
too: the shot is the 3-D view alone, at 320x200 and the page's 4:3 aspect,
with the weapon); the port draws the same view from id's camera, clock,
entity list and particles (`compare.py --demo`, whose `--dlights` picks the
lights):

  * `none`   no lights — playback before this change;
  * `port`   the lights the port's playback made at that clock (its own
             `rand()&31` for a flash's radius, `quaketool play --trace`);
  * `id`     id's own `cl_dlights` of the frame: the renderer alone, exact
             lights, so what is left is the renderer's.

Per frame: the match with id's frame (exact%: the palette index equal, over
the 3-D view), and the lights as id's frame and the port's playback had them
(their kinds, and a jittered light's radius in each).
"""

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
QUAKETOOL = PROJECT / "quake-rs" / "target" / "release" / "quaketool"
PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
# The port's host step as the f32 `quaketool play` runs at: id's side plays at it.
DT = "0.01388888899236917"
MAPS = {"demo1": "e1m3", "demo2": "e1m4", "demo3": "e1m6"}
# Frames of the attract loop (frame K of `demoN`, 1/72 s apart): a view entity's
# muzzle flash, a grenade's explosion on the frame it is made and as it fades,
# explosions with a flash, two explosions at once, a rocket in flight (alone,
# and with a flash and an explosion), a monster's flash.
STANDARD = ["demo1:323", "demo1:358", "demo1:376", "demo1:557", "demo1:601", "demo1:982", "demo2:253",
            "demo2:4708", "demo2:4023", "demo3:204", "demo3:359", "demo3:371"]


def trace_lights(trace: Path) -> list[dict]:
    """[{kind-less light: radius...}] per frame of a `quaketool play --trace`."""
    frames = []
    for line in trace.read_text().splitlines():
        if line.startswith("F "):
            frames.append([])
        elif line.startswith("D ") and frames:
            p = line.split()
            frames[-1].append({"slot": int(p[1]), "key": int(p[2]), "radius": float(p[6]),
                               "decay": float(p[8]), "minlight": float(p[9])})
    return frames


def kinds(lights: list[dict]) -> str:
    """The port's lights of a frame: what each is (an explosion decays; a flash has minlight 32; a
    rocket's or a dim light's radius is 200+; a bright light's 400+) and its entity number."""
    out = []
    for l in lights:
        kind = "explosion" if l["decay"] > 0 else "flash" if l["minlight"] == 32 else "rocket/dim" if l["radius"] < 400 else "bright"
        out.append(kind if kind == "explosion" else f"{kind}#{l['key']}")
    return ", ".join(out) or "none"


def radii(lights: list[float]) -> str:
    return " ".join(f"{r:.0f}" for r in lights) or "-"


def jittered(light: dict) -> bool:
    """The radius is `base + (rand()&31)`: a flash, a bright light, a dim light — not an explosion's
    (350, decaying) or a rocket's (exactly 200)."""
    return light["decay"] == 0 and light["radius"] != 200.0


def run_compare(demo: str, frame: int, mode: str, out: Path, args) -> tuple[dict, Path]:
    cmd = ["uv", "run", str(HERE / "compare.py"), "--maps", MAPS[demo], "--modes", "ents", "--demo", demo,
           "--settle", str(frame), "--oracle-dt", DT, "--aspect", "0.8333333", "--spans", "16", "--viewmodel",
           "--id-lightstyles",
           "--dlights", demo if mode == "port" else mode, "--quaketool", str(args.quaketool), "--out", str(out)]
    res = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    if res.returncode != 0:
        sys.exit(f"compare.py failed for {demo}:{frame} {mode}:\n{res.stdout[-2000:]}{res.stderr[-2000:]}")
    summary = json.loads((out / "summary.json").read_text())
    return next(iter(summary.values())), out / f"{MAPS[demo]}_ents_320x200"


def pnm(path: Path) -> np.ndarray:
    raw = path.read_bytes()
    parts = raw.split(b"\n", 3)
    w, h = (int(v) for v in parts[1].split())
    return np.frombuffer(parts[3], dtype=np.uint8).reshape(h, w, 3)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--frames", help="comma list of demoN:K (default: the standard frames)")
    ap.add_argument("--sample", type=int, default=0, help="also N lit frames spread evenly over the loop's three demos")
    ap.add_argument("--out", type=Path, default=HERE / "build" / "demo-lights")
    ap.add_argument("--quaketool", type=Path, default=QUAKETOOL)
    ap.add_argument("--scale", type=int, default=3, help="scale of the before/after images")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    # The port's playback of the loop, once: which frames have lights, and their radii.
    port_frames = {}
    with tempfile.TemporaryDirectory(prefix="demo-lights-") as tmp:
        for d, n in (("demo1", 5350), ("demo2", 5035), ("demo3", 5928)):
            trace = Path(tmp) / f"{d}.trace"
            subprocess.run([str(args.quaketool), "play", str(PAK), d, str(n), "--hash-every", "0", "--trace", str(trace)],
                           check=True, capture_output=True, timeout=1800)
            port_frames[d] = trace_lights(trace)[:n]
    specs = (args.frames.split(",") if args.frames else STANDARD)
    if args.sample:
        lit = [(d, i) for d in port_frames for i, ls in enumerate(port_frames[d]) if ls]
        step = max(1, len(lit) // args.sample)
        specs += [f"{d}:{i}" for d, i in lit[::step][: args.sample] if f"{d}:{i}" not in specs]

    rows = []
    for spec in specs:
        demo, _, k = spec.partition(":")
        frame = int(k)
        res = {}
        for mode in ("none", "port", "id"):
            st, case = run_compare(demo, frame, mode, args.out / spec.replace(":", "-") / mode, args)
            res[mode] = (st, case)
        # The image: id's frame | the port before | the port after (its own lights) | id's lights.
        c = pnm(res["id"][1].with_name(res["id"][1].name + ".c.ppm"))
        parts = [c] + [pnm(res[m][1].with_name(res[m][1].name + ".port.ppm")) for m in ("none", "port", "id")]
        gap = np.full((c.shape[0], 2, 3), (255, 0, 255), dtype=np.uint8)
        row = np.concatenate([x for p in parts for x in (p, gap)][:-1], axis=1)
        img = Image.fromarray(row)
        img = img.resize((img.width * args.scale, img.height * args.scale), Image.NEAREST)
        img.save(args.out / f"{spec.replace(':', '-')}.png")
        meta = json.loads((res["id"][1].with_name(res["id"][1].name + ".c.json")).read_text())
        port_lights = port_frames[demo][frame] if frame < len(port_frames[demo]) else []
        rows.append((spec, meta["time"], res["none"][0]["exact_pct"], res["port"][0]["exact_pct"],
                     res["id"][0]["exact_pct"], kinds(port_lights), radii([dl[3] for dl in meta["dlights"]]),
                     radii([l["radius"] for l in port_lights]), any(jittered(l) for l in port_lights)))
        print(f"{spec:<11} t={meta['time']:8.4f}  none {rows[-1][2]:7.2f}  port {rows[-1][3]:7.2f}  id {rows[-1][4]:7.2f}"
              f"  {rows[-1][5]}; radius id {rows[-1][6]} / port {rows[-1][7]}", flush=True)

    print()
    print(f"{'frame':<11} {'cl.time':>8} {'no lights':>10} {'port lights':>12} {'id lights':>10}   lights; radii in id's frame / in the port's playback")
    for spec, t, a, b, c_, kd, ri, rp, jit in rows:
        print(f"{spec:<11} {t:8.4f} {a:10.2f} {b:12.2f} {c_:10.2f}   {kd}; {ri} / {rp}{'  (jitter)' if jit else ''}")
    for label, sel in (("frames with only exact radii (explosions, rockets)", [r for r in rows if not r[8]]),
                       ("frames with a flash (a radius of rand()&31's)", [r for r in rows if r[8]])):
        if sel:
            px = np.array([[r[2], r[3], r[4]] for r in sel])
            print(f"\n{label}: {len(sel)} frames; exact% of the 3-D view, min / median:\n"
                  f"  no lights {px[:, 0].min():.2f} / {np.median(px[:, 0]):.2f}   "
                  f"the port's playback lights {px[:, 1].min():.2f} / {np.median(px[:, 1]):.2f}   "
                  f"id's lights {px[:, 2].min():.2f} / {np.median(px[:, 2]):.2f}")
    (args.out / "summary.json").write_text(json.dumps(
        [dict(zip(("frame", "time", "none", "port", "id", "kinds", "radii_id", "radii_port", "jitter"), r)) for r in rows],
        indent=1))
    print(f"images and summary.json in {args.out}")


if __name__ == "__main__":
    main()
