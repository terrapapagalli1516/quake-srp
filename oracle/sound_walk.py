#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""The sound oracle's game half: one scripted walk through id's game and the
port's, and every call each makes into the sound layer on the way, compared
(sound.py compares the two mixers on the same calls; this compares the calls).

    uv run oracle/sound_walk.py                       # every case: a timeline each, exit 1 on a difference
    uv run oracle/sound_walk.py --cases telegate --out DIR
    uv run oracle/sound_walk.py --instant-load        # id's loads take no time (the oracle's default clock)

id's side is the C oracle (build.sh): id's whole game, headless, its null
sound driver's entry points and SV_StartSound logged, the walk driven from
CL_SendCmd (c/walk_oracle.c). The port's is `quaketool sndwalk`, the Classic
client. Both run a host frame every 1/72 s (the port's f32 step) from a fresh
`map`, with id's Always Run speed (cl_forwardspeed 400), and take one frame of
the script in each frame that sends a move: in id's game a level change's
signon frames send none, so the walk stays in step across it. id's level
loads take a second of the clock by default (-oracle_loadtime 1), as they took
seconds on id's machines: the frame after one then runs Host_FilterTime's
0.1 s, as it did there.

What is compared, walk frame by walk frame (a call belongs to the walk frame
it follows; id's signon frames to the frame before them, whose move set off
the level change):
  - the player's own sounds and the teleport fog's (`misc/r_tele1..5`, one
    picked by random(): any of the five matches any): entity class, channel,
    sample, volume and attenuation bytes, origin as the client has it;
  - S_StopAllSounds (a run of them with nothing between is one), the level's
    S_StaticSound loops (as a set), S_StopSound, S_LocalSound.
Every other sound is the world's: monsters, and what they set off, on
random(); id's rand() is stirred every host frame and the port draws from its
own streams (quake-rs/src/qrand.rs), so those are listed, not compared. The
player's path is compared too: a walk whose paths part compares nothing.

Each case prints its timeline (walk frame, id's and the port's cl.time, the
call), the level changes host frame by host frame on id's side, the sounds
id's server started that never reached its client (the signon), and a verdict.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from oraclebin import ORACLE_BIN as ORACLE, ensure_oracle  # noqa: E402
import scratch  # noqa: E402

PROJECT = HERE.parent
DEFAULT_PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
# The port's host step, 1/72 s as the f32 the page and quaketool use (demo_lerp.py).
DT = "0.01388888899236917"
# How long a level load takes on id's clock: past 0.1 s, so the frame after it
# is clamped to 0.1 s, as on id's machines.
LOADTIME = "1"

# A walk: "frames yaw forward jump" a line. A yaw crosses the wire as a byte
# (MSG_WriteAngle: (int)yaw*256/360) and the port's server takes it unrounded,
# so the scripts use multiples of 45 in (-180, 180], which both read the same.
CASES = {
    "telegate": (
        "start",
        "start: the NORMAL skill hall's teleporter, then the first episode's slipgate; a second in e1m1",
        """\
10 90 0 0       # stand: the level settles
268 90 1 0      # run north down the NORMAL skill hall, into its teleporter, on across the hub
125 -180 1 0    # west along the first episode's hall
22 -135 1 0     # south-west into the slipgate: the frame it touches is the last of these
72 -135 0 0     # a second in e1m1
""",
    ),
}

TELE = re.compile(r"misc/r_tele[1-5]\.wav$")


@dataclass
class Call:
    frame: int          # the host frame
    cltime: float
    svtime: float
    signon: int
    kind: str
    args: list[str]

    def text(self) -> str:
        return " ".join([self.kind, *self.args])


@dataclass
class Walk:
    """One side's log: the path, and the calls grouped by walk frame (-1: the
    boot, before the first move)."""
    path: dict[int, tuple[float, float, float]] = field(default_factory=dict)
    cltime: dict[int, float] = field(default_factory=dict)
    groups: dict[int, list[Call]] = field(default_factory=dict)
    server: list[Call] = field(default_factory=list)   # id's `sv` lines
    by_frame: dict[int, list[Call]] = field(default_factory=dict)


def parse(path: Path) -> Walk:
    w = Walk()
    current = -1
    for line in path.read_text().splitlines():
        p = line.split()
        if len(p) < 7:
            continue
        c = Call(int(p[0]), float(p[2]), float(p[3]), int(p[4]), p[6], p[7:])
        w.by_frame.setdefault(c.frame, []).append(c)
        if c.kind == "walk":
            current = int(c.args[0])
            w.path[current] = tuple(float(v) for v in c.args[1:4])
            w.cltime[current] = c.cltime
        elif c.kind == "sv":
            w.server.append(c)
        else:
            w.groups.setdefault(current, []).append(c)
    return w


def is_walks(c: Call) -> bool:
    """A call the comparison holds both sides to (see the module note)."""
    if c.kind != "start":
        return True
    return c.args[1] == "player" or bool(TELE.search(c.args[3]))


def key(c: Call) -> tuple:
    """What has to match: a start's class, channel, sample (the fog's any
    r_tele), bytes and origin -- not its entity number, which is one off
    between the two (the port's player is the last edict, AUDIT.md)."""
    if c.kind == "start":
        _, cls, chan, sample, x, y, z, vol, attn = c.args
        return ("start", cls, chan, "misc/r_tele?.wav" if TELE.search(sample) else sample, x, y, z, vol, attn)
    return (c.kind, *c.args)


def walks_calls(calls: list[Call]) -> list[tuple]:
    """A group's compared calls in order: the statics as one sorted entry,
    a run of stopalls as one."""
    out: list[tuple] = []
    statics: list[tuple] = []
    for c in calls:
        if not is_walks(c):
            continue
        if c.kind == "static":
            statics.append(key(c))
            continue
        if statics:
            out.append(("static", len(statics), tuple(sorted(statics))))
            statics = []
        if c.kind == "stopall" and out and out[-1][0] == "stopall":
            continue
        out.append(key(c))
    if statics:
        out.append(("static", len(statics), tuple(sorted(statics))))
    return out


def show(k: tuple) -> str:
    if k[0] == "static":
        return f"static x{k[1]} ({len(set(s[1] for s in k[2]))} samples)"
    return " ".join(str(v) for v in k)


def ensure_quaketool(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    crate = PROJECT / "quake-rs"
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=crate, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", crate / "target"))
    if not target.is_absolute():
        target = crate / target
    return target / "release" / "quaketool"


def run_c(pak: Path, m: str, script: Path, log: Path, loadtime: str) -> None:
    # id's -basedir in the system's temp dir: MAX_OSPATH is 128 (demo_lerp.py).
    with scratch.tempdir("sound-walk-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(pak.resolve())
        (base / "id1" / "walk.cfg").write_text(
            f"cl_forwardspeed 400\noracle_sndlog {log}\noracle_walk {script}\nmap {m}\n")
        res = subprocess.run([str(ORACLE), "-basedir", str(base), "-width", "320", "-height", "200",
                              "-oracle_dt", DT, "-oracle_loadtime", loadtime, "+exec", "walk.cfg"],
                             cwd=base, capture_output=True, text=True, timeout=600)
        if res.returncode != 0 or not log.exists():
            sys.exit(f"the C oracle failed (rc {res.returncode}):\n{res.stdout[-2000:]}{res.stderr[-2000:]}")


def run_port(qt: Path, pak: Path, m: str, script: Path, log: Path) -> None:
    res = subprocess.run([str(qt), "sndwalk", str(pak), m, str(script), str(log)],
                         capture_output=True, text=True, timeout=600)
    if res.returncode != 0:
        sys.exit(f"quaketool sndwalk failed:\n{res.stdout}{res.stderr}")


def compare_case(name: str, c: Walk, p: Walk) -> bool:
    ok = True
    # The path: identical but for the frames after a level starts, while
    # id's player still falls from its spawn spot (PutClientInServer puts it
    # 1 unit up; the port's is settled before its first frame, AUDIT.md).
    frames = sorted(set(c.path) | set(p.path))
    missing = [f for f in frames if f not in c.path or f not in p.path]
    apart = []
    for f in frames:
        if f in c.path and f in p.path:
            d = max(abs(a - b) for a, b in zip(c.path[f], p.path[f]))
            if d > 0.002:
                apart.append((f, d))
    starts = {0} | {f + 1 for f in c.groups if f >= 0 and any(x.kind == "stopall" for x in c.groups[f])}
    settling = [f for f, _ in apart if any(0 <= f - s < 8 for s in starts)]
    parted = [(f, d) for f, d in apart if f not in settling]
    print(f"  path: {len(frames)} walk frames", end="")
    if settling:
        print(f"; {len(settling)} just after a level starts differ by up to "
              f"{max(d for f, d in apart if f in settling):.3f} (id's player still falling from its spawn spot)", end="")
    if parted or missing:
        ok = False
        first = parted[0] if parted else (missing[0], float("nan"))
        print(f"; APART at {len(parted) + len(missing)} frames, first {first[0]} by {first[1]:.3f}")
    else:
        print("; otherwise identical")

    print(f"  {'walk':>5} {'id cl.time':>11} {'port cl.time':>12}  call")
    groups = sorted(set(c.groups) | set(p.groups))
    compared = differ = 0
    for g in groups:
        cg, pg = walks_calls(c.groups.get(g, [])), walks_calls(p.groups.get(g, []))
        n = max(len(cg), len(pg))
        for i in range(n):
            a = cg[i] if i < len(cg) else None
            b = pg[i] if i < len(pg) else None
            same = a == b
            compared += 1
            differ += not same
            label = "boot" if g < 0 else str(g)
            ct = f"{c.cltime[g]:.4f}" if g in c.cltime else "-"
            pt = f"{p.cltime[g]:.4f}" if g in p.cltime else "-"
            if same:
                print(f"  {label:>5} {ct:>11} {pt:>12}  {show(a)}")
            else:
                print(f"  {label:>5} {ct:>11} {pt:>12}  DIFFERENT: id's {show(a) if a else '(none)'}")
                print(f"  {'':>5} {'':>11} {'':>12}             port's {show(b) if b else '(none)'}")
    ok &= differ == 0

    # The level changes on id's side, host frame by host frame.
    for g in groups:
        if g < 0 or not any(x.kind == "stopall" for x in c.groups.get(g, [])):
            continue
        first = c.groups[g][0].frame
        print(f"  the level change after walk frame {g}, id's host frames (the port's is all in its frame {g}):")
        for f in range(first - 1, first + 8):
            calls = c.by_frame.get(f, [])
            if not calls:
                continue
            kinds: dict[str, int] = {}
            for x in calls:
                kinds[x.kind] = kinds.get(x.kind, 0) + 1
            h = calls[0]
            print(f"    host {f:>4}: cl.time {h.cltime:.4f} sv.time {h.svtime:.4f} signon {h.signon}  "
                  + ", ".join(f"{k} x{v}" if v > 1 else k for k, v in kinds.items()))

    # The world's sounds, and the server's that never reached the client.
    for side, w in (("id's", c), ("the port's", p)):
        world: dict[str, int] = {}
        for calls in w.groups.values():
            for x in calls:
                if not is_walks(x):
                    world[x.args[3]] = world.get(x.args[3], 0) + 1
        print(f"  the world's sounds (random, not compared), {side}: "
              + (", ".join(f"{s} x{n}" for s, n in sorted(world.items())) or "none"))
    lost = []
    for s in c.server:
        delivered = any(x.kind == "start" and x.args[3] == s.args[2] and x.args[0] == s.args[0]
                        for x in c.by_frame.get(s.frame, []))
        if not delivered:
            lost.append(s)
    print("  id's server sounds its client never got: "
          + (", ".join(f"{s.args[2]} (ent {s.args[0]}, sv.time {s.svtime:.3f}, signon {s.signon})" for s in lost)
             or "none"))
    t0c, t0p = c.cltime.get(0), p.cltime.get(0)
    print(f"  {name}: {compared} calls compared, "
          + ("IDENTICAL" if differ == 0 else f"{differ} DIFFERENT")
          + (f"; walk frame 0 at cl.time {t0c:.4f} in id's, {t0p:.4f} in the port's" if t0c and t0p else ""))
    return ok


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pak", type=Path, default=DEFAULT_PAK)
    ap.add_argument("--cases", default=",".join(CASES), help="comma list of: " + ", ".join(CASES))
    ap.add_argument("--instant-load", action="store_true", help="id's level loads take no time")
    ap.add_argument("--quaketool", help="use this quaketool binary instead of building quake-rs")
    ap.add_argument("--out", type=Path, default=HERE / "build" / "sound-walk")
    args = ap.parse_args()
    ensure_oracle()
    qt = ensure_quaketool(args.quaketool)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    all_ok = True
    for name in [n for n in args.cases.split(",") if n]:
        if name not in CASES:
            sys.exit(f"unknown case {name!r}; have {', '.join(CASES)}")
        m, about, script = CASES[name]
        sfile = out / f"{name}.walk"
        sfile.write_text(script)
        clog, plog = out / f"{name}.c.log", out / f"{name}.port.log"
        run_c(args.pak, m, sfile, clog, "0" if args.instant_load else LOADTIME)
        run_port(qt, args.pak, m, sfile, plog)
        print(f"{name}: {about}")
        all_ok &= compare_case(name, parse(clog), parse(plog))
    print("\nthe walks' sound calls: every case identical" if all_ok else "\nthe walks' sound calls: SOME CASES DIFFER")
    sys.exit(0 if all_ok else 1)


if __name__ == "__main__":
    main()
