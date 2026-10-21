#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""QuakeC's particle() calls as id's client received them, against the port's.

    uv run oracle/particle_wire.py            # every case, both of id's builds; exit 1 on a difference
    uv run oracle/particle_wire.py --out DIR

id's PF_particle hands its arguments to SV_StartParticle, which writes an
svc_particle into the client datagram: the origin as three MSG_WriteCoords
(1/8 unit, toward zero, 16 bits), each axis of the direction as `(int)(dir*16)`
clamped into a signed byte, the count and the colour as the low byte of an
int. R_ParseParticleEffect reads them back (the direction over 16, a count of
255 as 1024) and spawns the particles at `vel = dir*15`. The byte is what
tames LightningDamage's '0 0 100': id's client gets 127/16, and the bolt's
blood rises at 119 u/s. The port has no datagram; its builtin queues the
burst id's client would have read (`ParticleBurst::of_call`), and until
2026-10-20 it queued the arguments as given: the blood rose at 1500 u/s.

id's side is the C oracle (build.sh) on the start map: each case is one
PF_particle call, made after a server frame's physics, and the log is every
svc_particle the client parses (c/particle_oracle.c). The port's is
`quaketool partwire`, the same calls through `ParticleBurst::of_call`. Both
builds of id's C are run (x87 and SSE2 floats). Every line must be identical.

Every `(int)` is x86's, NaN and values past 2^31 too (0x80000000, where Rust's
`as` would saturate: `math::c_ftoi`); no QuakeC computes one, but a mod could.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from oraclebin import ensure_oracle  # noqa: E402
import scratch  # noqa: E402

PROJECT = HERE.parent
DEFAULT_PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
NAN = float("nan")

# (what, org, dir, color, count). The first block is id1's own calls
# (`census/qcsym.py pak0.pak calls particle`), with values their QuakeC
# makes; the rest probe the wire's edges.
CASES = [
    ("LightningDamage: the bolt's blood", (412.5, -352.75, 40.03125), (0, 0, 100), 225, 120),
    ("TraceAttack: shotgun blood, head on", (196.2, 576.4, 31.9), (3.9, -0.6, 0.8), 73, 8),
    ("TraceAttack: shotgun blood, grazing", (-31.1, 40.6, 12.3), (11.6, -3.7, 2.1), 73, 8),
    ("spawn_touchblood: a nail's blood", (100.0, 200.0, 300.0), (-7.99, 7.99, 0.5), 73, 18),
    ("W_FireAxe: SpawnBlood at rest", (1.0, 2.0, 3.0), (0, 0, 0), 73, 40),
    ("barrel_explode: the explosion", (72.0, 2056.0, -176.0), (0, 0, 0), 75, 255),
    ("dir at the byte's ends", (0, 0, 0), (7.9375, -8.0, -8.0625), 73, 1),
    ("dir past them", (0, 0, 0), (8.0, -8.125, 1000.0), 73, 1),
    ("dir toward zero", (0, 0, 0), (0.06, -0.06, -0.07), 73, 1),
    ("dir just under a step", (0, 0, 0), (0.1249, -0.1249, 4.99), 73, 1),
    ("org toward zero", (-0.06, 1.3, -2.2), (0, 0, 0), 73, 1),
    ("org at 16 bits", (4095.9, -4096.0, 4096.0), (0, 0, 0), 73, 1),
    ("org past 16 bits", (-4100.0, 5000.5, -9000.25), (0, 0, 0), 73, 1),
    ("count -1: the explosion too", (0, 0, 0), (0, 0, 1), 73, -1),
    ("count 256: nothing", (0, 0, 0), (0, 0, 1), 73, 256),
    ("count 300 and a fraction", (0, 0, 0), (0, 0, 1), 73, 300.9),
    ("count 0", (0, 0, 0), (0, 0, 1), 73, 0),
    ("color 300", (0, 0, 0), (0, 0, 1), 300, 1),
    ("color -10", (0, 0, 0), (0, 0, 1), -10, 1),
    ("color 99999", (0, 0, 0), (0, 0, 1), 99999, 1),
    ("color 225.7", (0, 0, 0), (0, 0, 1), 225.7, 1),
    ("dir: NaN and past 2^31", (0, 0, 0), (NAN, 1e9, -1e9), 73, 1),
    ("org: NaN and past 2^28", (NAN, 3e8, -3e8), (0, 0, 0), 73, 1),
    ("count past 2^31", (0, 0, 0), (0, 0, 1), 73, 3e9),
    ("count NaN", (0, 0, 0), (0, 0, 1), 73, NAN),
    ("color past 2^31", (0, 0, 0), (0, 0, 1), 3e9, 1),
    ("color NaN", (0, 0, 0), (0, 0, 1), NAN, 1),
]


def script_text() -> str:
    lines = ["# oracle/particle_wire.py: one particle() call a line, ox oy oz dx dy dz color count"]
    for what, org, d, color, count in CASES:
        lines.append(" ".join(str(v) for v in (*org, *d, color, count)) + f"  # {what}")
    return "\n".join(lines) + "\n"


def ensure_quaketool(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    crate = PROJECT / "quake-rs"
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=crate, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", crate / "target"))
    if not target.is_absolute():
        target = crate / target
    return target / "release" / "quaketool"


def run_c(oracle: Path, pak: Path, script: Path, log: Path) -> list[str]:
    # id's -basedir in the system's temp dir: MAX_OSPATH is 128 (demo_lerp.py).
    with scratch.tempdir("particle-wire-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(pak.resolve())
        # quoted: COM_Parse splits a bare token at spaces and at : ' ( ) { }
        (base / "id1" / "wire.cfg").write_text(f'oracle_particles "{script}" "{log}"\nmap start\n')
        log.unlink(missing_ok=True)
        res = subprocess.run([str(oracle), "-basedir", str(base), "-width", "320", "-height", "200",
                              "+exec", "wire.cfg"], cwd=base, capture_output=True, text=True, timeout=300)
        if res.returncode != 0 or not log.exists():
            sys.exit(f"the C oracle failed (rc {res.returncode}):\n{res.stdout[-2000:]}{res.stderr[-2000:]}")
    return log.read_text().splitlines()


def run_port(qt: Path, script: Path, log: Path) -> list[str]:
    log.unlink(missing_ok=True)
    res = subprocess.run([str(qt), "partwire", str(script), str(log)], capture_output=True, text=True, timeout=300)
    if res.returncode != 0:
        sys.exit(f"quaketool partwire failed:\n{res.stdout}{res.stderr}")
    return log.read_text().splitlines()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pak", type=Path, default=DEFAULT_PAK)
    ap.add_argument("--quaketool", help="use this quaketool binary instead of building quake-rs")
    ap.add_argument("--out", type=Path, default=HERE / "build" / "particle-wire")
    args = ap.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    script = out / "calls.txt"
    script.write_text(script_text())
    qt = ensure_quaketool(args.quaketool)
    port = run_port(qt, script, out / "port.log")
    ok = len(port) == len(CASES)
    for fpmath in ("x87", "sse"):
        ids = run_c(ensure_oracle(fpmath=fpmath), args.pak, script, out / f"c-{fpmath}.log")
        differ = sum(a != b for a, b in zip(ids, port)) + abs(len(ids) - len(port))
        print(f"id's {fpmath} build against the port: {len(ids)} svc_particles, "
              + ("every one identical" if differ == 0 else f"{differ} DIFFERENT"))
        if differ:
            for i in range(max(len(ids), len(port))):
                a = ids[i] if i < len(ids) else "(none)"
                b = port[i] if i < len(port) else "(none)"
                if a != b:
                    what = CASES[i][0] if i < len(CASES) else "?"
                    print(f"  {what}:\n    id's   {a}\n    port's {b}")
        ok &= differ == 0
    print(f"\n  {'call':<38} {'what id' + chr(39) + 's client read: org, dir, color, count'}")
    for (what, *_), line in zip(CASES, port):
        print(f"  {what:<38} {line.removeprefix('particle ')}")
    print("\nparticle() on the wire: " + ("IDENTICAL" if ok else "DIFFERENT"))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
