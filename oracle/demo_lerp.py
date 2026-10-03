#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Demo playback against id's client, frame by frame: what CL_RelinkEntities
draws between two recorded messages.

id's WinQuake (the oracle, oracle/build/quake-oracle) plays the attract loop
from boot at a fixed 72 Hz (-oracle_dt: every host frame is exactly the
port's 1/72 s step) and writes one record per rendered frame (oracle_trace):
cl.time, cl.oldtime, cl.mtime[0..1], cl.viewangles, the view entity's
relinked origin, cl.velocity, every entity on cl_visedicts and every dynamic
light R_PushDlights marks (its slot in cl_dlights, key, origin, radius, die,
decay, minlight). The port's `quaketool play demo1 --trace` writes the same
records from its client. This script runs both, splits the traces into demos
(a demo change restarts the clock) and compares them frame by frame.

    uv run oracle/demo_lerp.py                 # the whole loop, into demo1 again (17,500 frames)
    uv run oracle/demo_lerp.py --frames 2000   # the first 2000 frames
    uv run oracle/demo_lerp.py --keep DIR      # keep both traces in DIR

Clocks must agree to 1e-9 s, positions and angles to --tol (x87 float
excess precision in the oracle leaves ulp-level differences), entity sets
exactly. The first differing frame of each kind is printed.

The lights must be the same set in every frame, slot for slot (the slot is
the light's bit in the surfaces' dlightbits, so it is part of id's state): the
same key, origin (--tol), decay and minlight, `die` to 1e-4 (a float, from
a double clock). The radius is the same except where id's draws `rand()&31`
for it (a muzzle flash 200+, a bright light 400+, a dim light 200+): there
the port's own draw must be in the same 0..31 window, which is all that two
different generators can share. An explosion's (decaying 350) and a
rocket's (200) are exact.
"""

import argparse
import math
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
ORACLE = HERE / "build" / "quake-oracle"
QUAKETOOL = PROJECT / "quake-rs" / "target" / "release" / "quaketool"
PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
# The port's host step, 1/72 s as the f32 the page and quaketool play use.
DT = "0.01388888899236917"


def parse(path: Path):
    """[(frame dict, {entnum: (model, origin, angles, frame)}, {slot: light})] per
    rendered frame; a light is {key, org, radius, die, decay, minlight}."""
    frames = []
    for line in path.read_text().splitlines():
        p = line.split()
        if not p:
            continue
        if p[0] == "F":
            kv = {}
            key = None
            for tok in p[2:]:
                if "=" in tok:
                    key, val = tok.split("=", 1)
                    kv[key] = [float(val)]
                else:
                    kv[key].append(float(tok))
            frames.append((kv, {}, {}))
        elif p[0] == "E" and frames:
            num = int(p[1])
            if num >= 0:
                vals = [float(x) for x in p[3:9]]
                frames[-1][1][num] = (p[2], vals[:3], vals[3:6], int(p[9]))
        elif p[0] == "D" and frames:
            v = [float(x) for x in p[3:]]
            frames[-1][2][int(p[1])] = {"key": int(p[2]), "org": v[:3], "radius": v[3], "die": v[4],
                                       "decay": v[5], "minlight": v[6]}
    return frames


def split_demos(frames):
    """Cut where the clock restarts (a new demo's first frame has old == 0)."""
    demos, cur = [], []
    for fr in frames:
        if cur and fr[0]["old"][0] == 0.0:
            demos.append(cur)
            cur = []
        cur.append(fr)
    if cur:
        demos.append(cur)
    return demos


def angle_diff(a, b):
    d = (a - b) % 360.0
    return min(d, 360.0 - d)


LIGHT_DIE_TOL = 1e-4
# Entities whose models carry id's EF_ROCKET: their light is exactly 200.
ROCKET_MODELS = ("progs/missile.mdl", "progs/lavaball.mdl")


def light_kind(light, ents):
    """What made id's light: explosion (decaying), muzzle flash (minlight 32),
    bright light (400 + rand()&31), rocket (an EF_ROCKET model's entity), else
    a dim light (200 + rand()&31)."""
    if light["decay"] > 0:
        return "explosion"
    if light["minlight"] == 32:
        return "muzzle flash"
    if light["radius"] >= 400:
        return "bright light"
    if ents.get(light["key"], ("",))[0] in ROCKET_MODELS:
        return "rocket"
    return "dim light"


def compare_lights(cl, pl, ce, tol):
    """The differences between id's lights `cl` and the port's `pl` of one frame
    ({slot: light}): [(kind of difference, detail)]."""
    diffs = []
    if set(cl) != set(pl):
        diffs.append(("light set", f"slots id {sorted(cl)} port {sorted(pl)}"))
    for slot in sorted(set(cl) & set(pl)):
        c, p = cl[slot], pl[slot]
        where = f"slot {slot}"
        if c["key"] != p["key"]:
            diffs.append(("light key", f"{where}: id {c['key']} port {p['key']}"))
        if max(abs(a - b) for a, b in zip(c["org"], p["org"])) > tol:
            diffs.append(("light origin", f"{where}: id {c['org']} port {p['org']}"))
        if c["decay"] != p["decay"] or c["minlight"] != p["minlight"]:
            diffs.append(("light params", f"{where}: id decay {c['decay']} min {c['minlight']}, "
                                          f"port decay {p['decay']} min {p['minlight']}"))
        if abs(c["die"] - p["die"]) > LIGHT_DIE_TOL:
            diffs.append(("light die", f"{where}: id {c['die']} port {p['die']}"))
        kind = light_kind(c, ce)
        if kind in ("explosion", "rocket"):
            if abs(c["radius"] - p["radius"]) > 0.01:
                diffs.append(("light radius", f"{where} {kind}: id {c['radius']} port {p['radius']}"))
        else:
            base = 400.0 if kind == "bright light" else 200.0
            if not (0 <= c["radius"] - base < 32 and 0 <= p["radius"] - base < 32):
                diffs.append(("light radius", f"{where} {kind}: id {c['radius']} port {p['radius']} "
                                              f"outside {base}..{base + 31}"))
    return diffs


def compare(cdemo, pdemo, tol, label):
    worst = {"clock": 0.0, "angles": 0.0, "vorg": 0.0, "vel": 0.0, "ent_origin": 0.0, "ent_angles": 0.0}
    first = {}
    set_mismatch = 0
    kinds, lit_frames, light_diffs = {}, [0, 0], 0
    n = min(len(cdemo), len(pdemo))
    for i in range(n):
        (cf, ce, cl), (pf, pe, pl) = cdemo[i], pdemo[i]
        lit_frames[0] += bool(cl)
        lit_frames[1] += bool(pl)
        for light in cl.values():
            kinds[light_kind(light, ce)] = kinds.get(light_kind(light, ce), 0) + 1
        ld = compare_lights(cl, pl, ce, tol)
        light_diffs += bool(ld)
        for kind, detail in ld:
            first.setdefault(kind, (i, detail))
        diffs = {
            "clock": max(abs(cf["t"][0] - pf["t"][0]), abs(cf["old"][0] - pf["old"][0])),
            "angles": max(angle_diff(a, b) for a, b in zip(cf["ang"], pf["ang"])),
            "vorg": max(abs(a - b) for a, b in zip(cf["vorg"], pf["vorg"])),
            "vel": max(abs(a - b) for a, b in zip(cf["vel"], pf["vel"])),
        }
        eo = ea = 0.0
        if set(ce) != set(pe):
            set_mismatch += 1
            first.setdefault("entity set", (i, sorted(set(ce) ^ set(pe))))
        for num in set(ce) & set(pe):
            (cm, co, ca, _), (_, po, pa, _) = ce[num], pe[num]
            eo = max(eo, max(abs(a - b) for a, b in zip(co, po)))
            ea = max(ea, max(angle_diff(a, b) for a, b in zip(ca, pa)))
        diffs["ent_origin"], diffs["ent_angles"] = eo, ea
        for k, v in diffs.items():
            worst[k] = max(worst[k], v)
            limit = 1e-9 if k == "clock" else tol
            if v > limit and k not in first:
                first[k] = (i, v)
    ok = not first and len(cdemo) == len(pdemo)
    print(f"{label}: {len(cdemo)} frames in id's, {len(pdemo)} in the port's; compared {n}: "
          f"{'MATCH' if ok else 'DIFFER'}")
    print("  worst |diff|: " + ", ".join(f"{k} {v:.3g}" for k, v in worst.items())
          + f"; entity-set mismatches {set_mismatch}")
    made = ", ".join(f"{k} {v}" for k, v in sorted(kinds.items())) or "none"
    print(f"  dynamic lights: frames lit {lit_frames[0]} in id's, {lit_frames[1]} in the port's; "
          f"frames whose lights differ {light_diffs}; id's lights drawn (light-frames): {made}")
    for k, v in first.items():
        print(f"  first {k} beyond tolerance at frame {v[0]}: {v[1]}")
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--frames", type=int, default=17500, help="frames to trace (default: the loop and into demo1 again)")
    ap.add_argument("--tol", type=float, default=0.01, help="position/angle tolerance (units, degrees)")
    ap.add_argument("--keep", type=Path, help="write the traces here")
    ap.add_argument("--oracle", type=Path, default=ORACLE)
    ap.add_argument("--quaketool", type=Path, default=QUAKETOOL)
    a = ap.parse_args()
    for p in (a.oracle, a.quaketool, PAK):
        if not p.exists():
            sys.exit(f"missing {p} (oracle/build.sh; cargo build --release in quake-rs; the pak)")
    # id's -basedir: in the system's temp dir, never under --keep, because id's
    # MAX_OSPATH is 128 and "<basedir>/id1/pak0.pak" overflows it in a deep checkout.
    with tempfile.TemporaryDirectory(prefix="demo-lerp-") as tmp:
        out = a.keep.resolve() if a.keep else Path(tmp)
        out.mkdir(parents=True, exist_ok=True)
        base = Path(tmp) / "base"
        (base / "id1").mkdir(parents=True)
        (base / "id1" / "pak0.pak").symlink_to(PAK.resolve())
        ctrace, ptrace = out / "id.trace", out / "port.trace"
        (base / "id1" / "trace.cfg").write_text(f"oracle_trace {ctrace} {a.frames}\n")
        subprocess.run([str(a.oracle), "-basedir", str(base), "-width", "320", "-height", "200",
                        "-oracle_dt", DT, "+exec", "trace.cfg"], cwd=base, check=True,
                       capture_output=True, timeout=1800)
        subprocess.run([str(a.quaketool), "play", str(PAK), "demo1", str(a.frames), "--hash-every", "0",
                        "--trace", str(ptrace)], check=True, capture_output=True, timeout=1800)
        cdemos, pdemos = split_demos(parse(ctrace)), split_demos(parse(ptrace))
        ok = len(cdemos) == len(pdemos)
        for k, (cd, pd) in enumerate(zip(cdemos, pdemos)):
            ok &= compare(cd, pd, a.tol, f"demo{k % 3 + 1} (run {k + 1})")
        if len(cdemos) != len(pdemos):
            print(f"demos traced: {len(cdemos)} in id's, {len(pdemos)} in the port's")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
