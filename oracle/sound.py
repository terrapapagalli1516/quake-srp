#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""The sound oracle: id's mixer (snd_dma.c, snd_mix.c, snd_mem.c, built headless
by build_sound.sh) and the port's (quake_rs::snd::Mixer, through `quaketool
sndscript`) run the same scripted calls, and their PCM is compared sample for
sample.

    uv run oracle/sound.py                          # every scenario at 11025/22050/44100/48000
    uv run oracle/sound.py --scenarios statics,loops --rates 11025
    uv run oracle/sound.py --fixes                  # also: how far the slop mixer departs from id's
    uv run oracle/sound.py --sse                    # the C built with SSE2 floats instead of x87

A script is a list of snd_oracle.c's commands (see its header): sound calls,
the listener, the listener's leaf, cvars, and `update` (S_Update, then S_Update_
mixing ahead of the fake DMA position) and `advance N` (the DMA position moves
N sample pairs). Each scenario writes one script per rate into --out (default
oracle/build/sound-out/), then for each: <case>.c.raw / <case>.port.raw (16-bit
stereo, every pair each mixer painted, in order) and <case>.c.trace /
<case>.port.trace (every sounding channel after each update: volumes,
position, end). The table gives per case the pairs compared, whether the PCM
is identical, the first differing pair and the largest difference, and the
trace lines that differ. The Classic port must be identical everywhere; with
--fixes a second table shows what the fixes change.
"""

from __future__ import annotations

import argparse
import math
import os
import random
import struct
import subprocess
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
DEFAULT_PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
RATES = [11025, 22050, 44100, 48000]

# Samples the scenarios use (all in the shareware pak).
ONESHOTS = [
    "weapons/guncock.wav", "weapons/rocket1i.wav", "weapons/grenade.wav", "weapons/r_exp3.wav",
    "weapons/sgun1.wav", "weapons/spike2.wav", "weapons/tink1.wav", "weapons/ric1.wav",
    "player/pain1.wav", "player/pain2.wav", "player/land.wav", "player/plyrjmp8.wav",
    "items/r_item1.wav", "items/health1.wav", "misc/talk.wav", "misc/menu1.wav",
    "soldier/sight1.wav", "soldier/pain1.wav", "dog/dattack1.wav", "zombie/z_idle.wav",
    "demon/sight2.wav", "knight/sword1.wav", "misc/h2ohit1.wav", "doors/doormv1.wav",
]
SIXTEEN = ["weapons/lhit.wav", "weapons/lstart.wav"]           # 16-bit, 22050 Hz
LOOPED = ["ambience/fire1.wav", "ambience/hum1.wav", "ambience/drip1.wav", "ambience/buzz1.wav",
          "ambience/comp1.wav", "ambience/suck1.wav", "ambience/swamp1.wav", "ambience/drone6.wav",
          "doors/stndr1.wav", "doors/winch2.wav", "plats/train1.wav", "ambience/fl_hum1.wav"]
NOT_LOOPED = "weapons/guncock.wav"


def coord(v: float) -> str:
    """A coordinate as MSG_ReadCoord gives it: a multiple of 1/8."""
    return f"{round(v * 8) / 8:g}"


def listener(x: float, y: float, z: float, yaw_deg: float) -> str:
    """AngleVectors at pitch 0, roll 0: forward (cy, sy, 0), right (sy, -cy, 0)."""
    y_r = math.radians(yaw_deg)
    cy, sy = math.cos(y_r), math.sin(y_r)
    return (f"listener {coord(x)} {coord(y)} {coord(z)} {cy:.9f} {sy:.9f} 0 {sy:.9f} {-cy:.9f} 0")


class Script:
    """A script under construction, for one rate: frames of calls, each ended by
    `update` and the DMA clock's advance."""

    def __init__(self, rate: int, seed: str):
        self.rate = rate
        self.lines: list[str] = []
        self.rng = random.Random(seed)
        self.clock = 0.0     # seconds of device time
        self.pairs = 0       # pairs the DMA position has advanced

    def add(self, line: str) -> None:
        self.lines.append(line)

    def frame(self, dt: float, frametime: str | None = None) -> None:
        """End a host frame: `update`, then the device plays dt seconds."""
        if frametime:
            self.add(f"frametime {frametime}")
        self.add("update")
        self.clock += dt
        target = int(self.clock * self.rate)
        self.add(f"advance {target - self.pairs}")
        self.pairs = target

    def text(self) -> str:
        return "\n".join(self.lines) + "\n"


# Frame times whose ambient step (x ambient_fade 100) is not within a hair of a
# whole number: the C carries host_frametime * ambient_fade in 80 bits, the port
# in 64 (see snd/dma.rs), which could part only on such a tie.
FT72 = "0.013888888888888889"
FT60 = "0.016666666666666666"


def scenario_oneshots(s: Script) -> None:
    """One-shot sounds all around a turning listener: distance, pan, volume and
    attenuation bytes, the view entity's own sounds, channel 0 and the same
    (entity, channel) override, more sounds than the 8 dynamic channels, a
    sound started twice in one frame (the rand() offset), 16-bit samples."""
    r = s.rng
    s.add("viewent 1")
    s.add("leaf 0 0 0 0")
    s.add(f"frametime {FT72}")
    for f in range(400):
        s.add(listener(r.uniform(-200, 200), r.uniform(-200, 200), r.uniform(-50, 50), f * 3.7))
        for _ in range(r.choice([0, 0, 1, 1, 2, 3, 5])):
            ent = r.choice([1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
            chan = r.choice([0, 0, 1, 2, 3, 4, 5, 6, 7])
            snd = r.choice(ONESHOTS + SIXTEEN)
            vol = r.choice([255, 255, 200, 128, 77, 30, 1])
            att = r.choice([64, 64, 128, 192, 0, 32, 255])
            ang = r.uniform(0, 2 * math.pi)
            dist = r.choice([0, 16, 100, 300, 600, 900, 1200, 2500])
            s.add(f"start {ent} {chan} {snd} {coord(dist * math.cos(ang))} {coord(dist * math.sin(ang))} "
                  f"{coord(r.uniform(-64, 64))} {vol} {att}")
        if f % 37 == 5:     # the same sample twice in one frame: rand() offsets the second
            snd = r.choice(ONESHOTS)
            s.add(f"start 20 0 {snd} 100 0 0 255 64")
            s.add(f"start 21 0 {snd} -100 0 0 255 64")
        if f % 53 == 7:
            s.add(f"local {r.choice(['misc/menu1.wav', 'misc/menu2.wav', 'misc/talk.wav'])}")
        s.frame(1 / 72)


def scenario_statics(s: Script) -> None:
    """A level's static sounds: torches and hums placed around a listener who
    walks among them (the combining of one sample's statics, volumes summed past
    255), a sample with no loop point (refused, its slot spent), a missing
    sample, then a level change (stopall) and a second level's statics."""
    r = s.rng
    s.add("viewent 1")
    s.add("leaf 0 0 0 0")
    s.add(f"frametime {FT72}")
    for level in range(2):
        s.add("stopall")
        for i in range(40):
            snd = r.choice(LOOPED[:4]) if i % 3 else "ambience/fire1.wav"
            s.add(f"static {snd} {coord(r.uniform(-800, 800))} {coord(r.uniform(-800, 800))} "
                  f"{coord(r.uniform(-100, 100))} {r.choice([255, 200, 128, 76])} {r.choice([64, 128, 192])}")
        s.add(f"static {NOT_LOOPED} 0 0 0 255 192")
        s.add("static no/such.wav 0 0 0 255 192")
        s.add("static ambience/fire1.wav 8 8 0 255 64")   # right by the start: past 255 when combined
        for f in range(500):
            t = f / 72
            s.add(listener(600 * math.sin(t * 0.7 + level), 600 * math.cos(t * 0.5), 0, f * 1.3))
            if f % 97 == 3:
                s.add(f"start 1 1 player/land.wav 0 0 0 255 64")
            s.frame(1 / 72)


def scenario_ambients(s: Script) -> None:
    """The leaf ambients: the listener's leaf changes levels (water, sky, both,
    under the floor of 8), ambient_level and ambient_fade change, a frame rate
    of 72 and of 60, and a leaf of none (outside the world)."""
    r = s.rng
    s.add("viewent 1")
    s.add(listener(0, 0, 0, 0))
    levels = ["255 0 0 0", "0 255 0 0", "128 200 0 0", "20 20 0 0", "0 0 255 255", "60 255 0 0", "none"]
    for seg in range(24):
        lv = levels[seg % len(levels)] if seg < 14 else r.choice(levels)
        s.add(f"leaf {lv}")
        if seg == 9:
            s.add("cvar ambient_level 0.5")
        if seg == 12:
            s.add("cvar ambient_fade 250")
        if seg == 16:
            s.add("cvar ambient_level 0")
        if seg == 18:
            s.add("cvar ambient_level 0.3")
            s.add("cvar ambient_fade 100")
        ft, dt = (FT60, 1 / 60) if seg % 2 else (FT72, 1 / 72)
        for _ in range(r.randint(60, 200)):
            s.frame(dt, ft)


def scenario_loops(s: Script) -> None:
    """Looping dynamic sounds (a door's or lift's hum, `S_StartSound` with a
    looped sample) running for many laps, some cut by their stop sound on the
    same (entity, channel), ambients looping too: every lap restarts inside a
    paint pass (id's loop-seam bug in Classic). Frames of odd lengths, and a
    frame far longer than the 512-pair paint buffer."""
    r = s.rng
    s.add("viewent 1")
    s.add("leaf 255 128 0 0")
    s.add(listener(0, 0, 0, 90))
    for f in range(900):
        if f % 60 == 1:
            ent = 30 + (f // 60) % 5
            s.add(f"start {ent} 2 {r.choice(LOOPED[8:])} {coord(r.uniform(-300, 300))} {coord(r.uniform(-300, 300))} 0 255 64")
        if f % 60 == 45:
            ent = 30 + (f // 60) % 5
            s.add(f"start {ent} 2 {r.choice(['doors/drclos4.wav', 'misc/null.wav', 'plats/plat2.wav'])} 0 0 0 255 64")
        if f % 150 == 149:
            s.frame(0.19, FT72)      # a stall: one S_Update_ paints several passes
        else:
            s.frame(r.choice([1 / 72, 1 / 72, 1 / 61, 1 / 90]), FT72)


def scenario_stops(s: Script) -> None:
    """S_StopSound on every dynamic channel (id's search covers channels 0..7:
    the ambients and the first four dynamic ones), a stop of an ambient's
    (0, 0) key, stopall mid-sound, and the mixahead cvar changed."""
    r = s.rng
    s.add("viewent 1")
    s.add("leaf 255 0 0 0")
    s.add(f"frametime {FT72}")
    s.add(listener(0, 0, 0, 0))
    for f in range(600):
        if f % 20 == 0:
            for ent in range(40, 48):       # fill all eight dynamic channels
                s.add(f"start {ent} 1 {r.choice(LOOPED[8:] + ONESHOTS[:4])} {coord(r.uniform(-200, 200))} 50 0 255 64")
        if f % 20 in (5, 9, 13):
            s.add(f"stop {r.randint(40, 47)} 1")
        if f == 250:
            s.add("stop 0 0")
        if f == 300:
            s.add("stopall")
        if f == 400:
            s.add("cvar _snd_mixahead 0.05")
        if f == 500:
            s.add("cvar _snd_mixahead 0.2")
        s.frame(1 / 72)


def scenario_cvars(s: Script) -> None:
    """volume (to 1 and past the clamp), loadas8bit on the 16-bit samples,
    nosound on and off."""
    r = s.rng
    s.add("viewent 1")
    s.add("leaf 0 0 0 0")
    s.add(f"frametime {FT72}")
    s.add(listener(0, 0, 0, 45))
    for f in range(500):
        if f == 100:
            s.add("cvar volume 1")
        if f == 200:
            s.add("cvar loadas8bit 1")
        if f == 300:
            s.add("cvar nosound 1")
        if f == 350:
            s.add("cvar nosound 0")
        if f == 400:
            s.add("cvar volume 0.25")
        if f % 4 == 0:
            snd = r.choice(ONESHOTS + SIXTEEN) if f < 200 else r.choice(SIXTEEN + ["weapons/r_exp3.wav"])
            s.add(f"start {r.randint(2, 9)} {r.randint(0, 7)} {snd} {coord(r.uniform(-64, 64))} {coord(r.uniform(-64, 64))} 0 255 {r.choice([0, 64])}")
        s.frame(1 / 72)


def scenario_soak(s: Script) -> None:
    """A minute of everything at random: sounds, statics, stops, level changes,
    leaves, listener moves, frame lengths."""
    r = s.rng
    s.add("viewent 1")
    for f in range(72 * 60):
        if f % 900 == 0:
            s.add("stopall")
            for _ in range(r.randint(5, 60)):
                s.add(f"static {r.choice(LOOPED)} {coord(r.uniform(-1000, 1000))} {coord(r.uniform(-1000, 1000))} "
                      f"{coord(r.uniform(-200, 200))} {r.randint(1, 255)} {r.choice([0, 64, 128, 192, 255])}")
        if f % 50 == 0:
            s.add(f"leaf {r.choice(['0 0 0 0', '255 0 0 0', '0 255 0 0', '90 140 0 0', 'none'])}")
        s.add(listener(r.uniform(-900, 900), r.uniform(-900, 900), r.uniform(-100, 100), r.uniform(0, 360)))
        for _ in range(r.choice([0, 0, 0, 1, 1, 2, 4])):
            s.add(f"start {r.randint(1, 60)} {r.choice([0, 1, 2, 3, 4, 5, 6, 7])} {r.choice(ONESHOTS + SIXTEEN + LOOPED)} "
                  f"{coord(r.uniform(-1200, 1200))} {coord(r.uniform(-1200, 1200))} {coord(r.uniform(-200, 200))} "
                  f"{r.randint(1, 255)} {r.randint(0, 255)}")
        if r.random() < 0.02:
            s.add(f"stop {r.randint(1, 60)} {r.randint(0, 7)}")
        if r.random() < 0.01:
            s.add(f"local {r.choice(['misc/menu1.wav', 'misc/menu3.wav'])}")
        s.frame(r.choice([1 / 72, 1 / 72, 1 / 60, 1 / 50]), r.choice([FT72, FT60]))


SCENARIOS = {
    "oneshots": scenario_oneshots,
    "statics": scenario_statics,
    "ambients": scenario_ambients,
    "loops": scenario_loops,
    "stops": scenario_stops,
    "cvars": scenario_cvars,
    "soak": scenario_soak,
}


def ensure_oracle(sse: bool) -> Path:
    binary = HERE / "build" / ("snd-oracle-sse" if sse else "snd-oracle")
    sources = [HERE / "c" / "snd_oracle.c", HERE / "build_sound.sh"]
    if not binary.exists() or any(p.stat().st_mtime > binary.stat().st_mtime for p in sources):
        env = dict(os.environ, ORACLE_FPMATH="sse" if sse else "x87")
        subprocess.run([str(HERE / "build_sound.sh")], check=True, env=env)
    return binary


def ensure_quaketool(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    crate = PROJECT / "quake-rs"
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=crate, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", crate / "target"))
    if not target.is_absolute():
        target = crate / target
    return target / "release" / "quaketool"


def run(cmd: list[str]) -> None:
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"failed: {' '.join(cmd)}\n{res.stdout}{res.stderr}")


def compare(a: Path, b: Path) -> dict:
    x = np.fromfile(a, dtype="<i2").reshape(-1, 2).astype(np.int32)
    y = np.fromfile(b, dtype="<i2").reshape(-1, 2).astype(np.int32)
    n = min(len(x), len(y))
    diff = np.abs(x[:n] - y[:n]).max(axis=1) if n else np.zeros(0, dtype=np.int32)
    bad = np.nonzero(diff)[0]
    return {
        "pairs": (len(x), len(y)),
        "same": len(x) == len(y) and len(bad) == 0,
        "differing": int(len(bad)) + abs(len(x) - len(y)),
        "first": int(bad[0]) if len(bad) else None,
        "maxdiff": int(diff.max()) if n else 0,
        "peak": int(np.abs(x).max()) if len(x) else 0,
    }


def trace_diff(a: Path, b: Path) -> int:
    la, lb = a.read_text().splitlines(), b.read_text().splitlines()
    return sum(1 for p, q in zip(la, lb) if p != q) + abs(len(la) - len(lb))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pak", type=Path, default=DEFAULT_PAK)
    ap.add_argument("--scenarios", default=",".join(SCENARIOS), help="comma list of: " + ", ".join(SCENARIOS))
    ap.add_argument("--rates", default=",".join(map(str, RATES)))
    ap.add_argument("--seed", type=int, default=1996)
    ap.add_argument("--fixes", action="store_true", help="also run the port with every fix (the slop mixer)")
    ap.add_argument("--sse", action="store_true", help="the C built with SSE2 float math instead of x87")
    ap.add_argument("--quaketool", help="use this quaketool binary instead of building quake-rs")
    ap.add_argument("--out", type=Path, default=HERE / "build" / "sound-out")
    args = ap.parse_args()

    oracle = ensure_oracle(args.sse)
    qt = ensure_quaketool(args.quaketool)
    args.out.mkdir(parents=True, exist_ok=True)
    rates = [int(r) for r in args.rates.split(",")]
    names = [n for n in args.scenarios.split(",") if n]
    for n in names:
        if n not in SCENARIOS:
            sys.exit(f"unknown scenario {n!r}; have {', '.join(SCENARIOS)}")

    print(f"C: {oracle.name}   port: {qt}   out: {args.out}")
    print(f"{'case':<20} {'pairs':>9} {'PCM':>10} {'differing':>9} {'first':>8} {'max|d|':>7} {'peak':>6} {'trace':>6}")
    all_same = True
    fix_rows = []
    for name in names:
        for rate in rates:
            case = f"{name}-{rate}"
            s = Script(rate, f"{args.seed}-{name}")   # the same calls at every rate
            SCENARIOS[name](s)
            script = args.out / f"{case}.txt"
            script.write_text(s.text())
            c_raw, c_tr = args.out / f"{case}.c.raw", args.out / f"{case}.c.trace"
            p_raw, p_tr = args.out / f"{case}.port.raw", args.out / f"{case}.port.trace"
            run([str(oracle), str(args.pak), str(script), str(c_raw), str(rate), str(c_tr)])
            run([str(qt), "sndscript", str(args.pak), str(script), str(p_raw), "--rate", str(rate), "--trace", str(p_tr)])
            r = compare(c_raw, p_raw)
            td = trace_diff(c_tr, p_tr)
            all_same &= r["same"] and td == 0
            first = "-" if r["first"] is None else str(r["first"])
            print(f"{case:<20} {r['pairs'][0]:>9} {'identical' if r['same'] else 'DIFFERENT':>10} "
                  f"{r['differing']:>9} {first:>8} {r['maxdiff']:>7} {r['peak']:>6} {td:>6}")
            if args.fixes:
                f_raw = args.out / f"{case}.fixes.raw"
                run([str(qt), "sndscript", str(args.pak), str(script), str(f_raw), "--rate", str(rate), "--fixes"])
                fix_rows.append((case, compare(c_raw, f_raw)))
    if fix_rows:
        print("\nthe slop mixer (every fix) against id's:")
        print(f"{'case':<20} {'pairs C':>9} {'pairs port':>10} {'differing':>9} {'max|d|':>7}")
        for case, r in fix_rows:
            print(f"{case:<20} {r['pairs'][0]:>9} {r['pairs'][1]:>10} {r['differing']:>9} {r['maxdiff']:>7}")
    print("\nClassic: every case identical" if all_same else "\nClassic: SOME CASES DIFFER")
    sys.exit(0 if all_same else 1)


if __name__ == "__main__":
    main()
