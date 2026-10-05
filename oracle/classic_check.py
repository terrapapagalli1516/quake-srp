#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""The proof of Classic: every check that the port, with every departure off
(the Classic preset), is still id's WinQuake — in one command.

    uv run oracle/classic_check.py                 # everything, a report
    uv run oracle/classic_check.py --only play,goldens
    uv run oracle/classic_check.py --record        # rewrite oracle/classic_expected.txt

Ten checks, of two kinds:

- **Identity** (four: the port against itself, recorded in `oracle/classic_expected.txt`
  on a tree known to be Classic, `a50d8d7` for the first recording):
  - `goldens`: `quaketool scene` of e1m1/e1m2/e1m3, sha256 prefixes;
  - `play`: `quaketool play`'s frame hashes and sound-call tallies for id's
    three demos and four scripted walks at 320x200, 640x400 and 960x600 —
    the browser's own client frames, natively;
  - `timedemo`: id's `timedemo` frame counts for demo1..3 (id's C draws 969
    for demo1);
  - `census`: the `quaketool census` playthrough of all nine maps through
    the real QuakeC (its whole report, by hash).
- **Against id's C** (six: the C oracle built from id's source runs in the
  same check):
  - `edicts`: id's own server edicts diffed against the port's for all nine
    maps at t = 1.7, 4.7 and 10.7 s (`census/`). The diff is not empty (the
    player's edict number, random numbers: oracle/README.md, "Classic"), so
    it is judged by hash against the recorded one: a change means the port's
    game state moved;
  - `oracle`: `compare.py --aspect 0.8333333 --spans 16 --sse`, the eight
    standard 3-D rows (e1m1/2/3/7, world and entities) at the page's aspect,
    against id's C built with SSE floats: every row 100.0000%;
  - `exact`: `exact_sweep.py` against the same build — every map's first
    frame, 18 yaws and pitches and 4 rolled views, the entities drawn, at
    320x200 and at the page's 640x400 and aspect: not one pixel differs
    (the x87 build's count is in its report, `exact.txt`, not judged);
  - `screen2d`: `screen2d.py`, id's composited 2-D layer at 320x200 and
    640x400, the port in its Classic preset: no shot below its recorded
    `2d exact%` (the known residues, oracle/README.md, are recorded);
  - `demolerp`: `demo_lerp.py`, id's client playing the attract loop (17,500
    frames: demo1, demo2, demo3, demo1 again) against the port's, frame by
    frame — the clock identical, the camera and every entity within 0.01;
  - `sound`: `sound.py`, id's mixer against the engine's `Fixes::NONE`
    mixer: every case sample-identical; and `sound_walk.py`, a walk through
    id's game and the port's (start, a teleporter, the e1m1 slipgate): every
    sound call the walk makes identical.

"id's C" is id's WinQuake source built headless with gcc, in two builds
(oracle/README.md, "Bit for bit"). `oracle` and `exact` run the SSE2 one,
every float operation in the type the C declares, which any conforming
compiler gives. The default x87 build is not their target: gcc keeps some
float variables in 80-bit registers, by its own register allocation, not by
anything the C says. `edicts`, `screen2d`, `demolerp` and `sound` run the x87
build: where the two part there, the port follows the x87's registers
(`world.rs`'s plane distances, the centerprint's row, the mixer's float steps).

Needs cargo, uv, the shareware pak at `quake-data/ID1/PAK0.PAK`, and for
`edicts`/`oracle`/`exact`/`screen2d`/`demolerp`/`sound` the C oracles
(`oracle/build.sh`, its SSE build `ORACLE_FPMATH=sse oracle/build.sh`,
`oracle/build_sound.sh`: docker; each tool builds its oracle when it is
missing or older than its sources, `oraclebin.py`). The report (and every tool's own
output) goes to `--out` (default `oracle/build/classic-check`); the exit
status is 0 only when every check passed.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
CRATE = PROJECT / "quake-rs"
PAK = PROJECT / "quake-data" / "ID1" / "PAK0.PAK"
EXPECTED = HERE / "classic_expected.txt"
MAPS = ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"]
PLAY_WORKLOADS = "demo1,demo2,demo3,walk_e1m1,fire_e1m1,quad_e1m1,walk_e1m3"
PLAY_RES = "320x200,640x400,960x600"
TIMEDEMO_RES = "320x200,640x400"
# The server times of the edict dumps, and the 0.1 s frames id's oracle
# waits for each after the last (the player connects at sv.time 1.2).
EDICT_TIMES = [(1.7, 5), (4.7, 30), (10.7, 60)]
CHECKS = ["goldens", "play", "timedemo", "census", "edicts", "oracle", "exact", "screen2d", "demolerp", "sound"]


def run(cmd, cwd=PROJECT, timeout=1800) -> str:
    res = subprocess.run([str(c) for c in cmd], cwd=cwd, capture_output=True, text=True, timeout=timeout)
    if res.returncode != 0:
        raise RuntimeError(f"{' '.join(map(str, cmd))} failed ({res.returncode}):\n{res.stdout[-2000:]}{res.stderr[-2000:]}")
    return res.stdout


def sha(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:16]


# --- the expected list: `key value` lines, `#` comments ------------------------

def load_expected() -> dict:
    if not EXPECTED.exists():
        return {}
    out = {}
    for line in EXPECTED.read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            k, _, v = line.partition(" ")
            out[k] = v
    return out


def save_expected(values: dict, note: str) -> None:
    """Write the list, keeping the notes of earlier recordings and adding
    `note` (what was recorded, on which tree, and why)."""
    head = ["# oracle/classic_check.py --record: the Classic identity list (see its doc)."]
    if EXPECTED.exists():
        head = [l for l in EXPECTED.read_text().splitlines() if l.startswith("#")] or head
    lines = head + [f"# {note}"] + [f"{k} {v}" for k, v in sorted(values.items())]
    EXPECTED.write_text("\n".join(lines) + "\n")


# --- the checks: each returns {key: value} (identity) or raises on a failure ----

def build_quaketool() -> Path:
    run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=CRATE)
    return CRATE / "target" / "release" / "quaketool"


def check_goldens(qt: Path, out: Path) -> dict:
    got = {}
    for m in ["e1m1", "e1m2", "e1m3"]:
        ppm = out / f"golden-{m}.ppm"
        run([qt, "scene", PAK, f"maps/{m}.bsp", ppm])
        got[f"goldens.{m}"] = hashlib.sha256(ppm.read_bytes()).hexdigest()[:8]
    return got


def check_play(qt: Path, out: Path) -> dict:
    text = run([qt, "play", PAK, PLAY_WORKLOADS, "--res", PLAY_RES, "--hash-every", "30"])
    (out / "play.txt").write_text(text)
    got, workload = {}, None
    for line in text.splitlines():
        if not line.startswith(" "):
            workload = line.strip()
            continue
        line = line.strip()
        if line.startswith("hashes "):
            res, hashes = line[len("hashes "):].split(": ", 1)
            got[f"play.{workload}.{res}.hashes"] = sha(hashes)
        elif "sound calls:" in line:
            res = line.split(":", 1)[0]
            got[f"play.{workload}.{res}.sound"] = sha(line)
    return got


def check_timedemo(qt: Path, out: Path) -> dict:
    got = {}
    for d in ["demo1", "demo2", "demo3"]:
        text = run([qt, "timedemo", PAK, d, "--res", TIMEDEMO_RES])
        (out / f"timedemo-{d}.txt").write_text(text)
        counts = re.findall(r"(\d+) frames", text)
        got[f"timedemo.{d}.frames"] = ",".join(counts)
    return got


def check_census(qt: Path, out: Path) -> dict:
    text = run([qt, "census", PAK])
    (out / "census.txt").write_text(text)
    return {"census.report": sha(text)}


def check_edicts(qt: Path, out: Path) -> dict:
    got = {}
    times = ",".join(str(t) for t, _ in EDICT_TIMES)
    for m in MAPS:
        c_path, port_path = out / f"edicts-{m}.c.txt", out / f"edicts-{m}.port.txt"
        c_path.unlink(missing_ok=True)
        # id's server, idle from the signon (census/oracle_run.py: 0.1 s
        # frames), a dump at each time.
        script = [f"map {m}"]
        for _, frames in EDICT_TIMES:
            script += [f"waits {frames}", f"oracle_edicts {c_path}"]
        run(["uv", "run", PROJECT / "census" / "oracle_run.py", *script])
        port_path.write_text(run([qt, "census-edicts", PAK, m, times]))
        diffs = []
        for t, _ in EDICT_TIMES:
            diffs.append(run(["uv", "run", PROJECT / "census" / "edict_diff.py", c_path, port_path, "--t", str(t)]))
        text = "\n".join(diffs)
        (out / f"edicts-{m}.diff.txt").write_text(text)
        got[f"edicts.{m}"] = sha(text)
    return got


def check_oracle(qt: Path, out: Path) -> dict:
    d = out / "compare"
    text = run(["uv", "run", HERE / "compare.py", "--aspect", "0.8333333", "--spans", "16", "--sse",
                "--quaketool", qt, "--out", d])
    (out / "compare.txt").write_text(text)
    rows = json.loads((d / "summary.json").read_text())
    if len(rows) != 8:
        raise RuntimeError(f"{len(rows)} rows, not the eight standard ones")
    return {f"oracle.{k}": f"{v['exact_pct']:.4f}" for k, v in rows.items()}


def check_exact(qt: Path, out: Path) -> dict:
    sweep = ["uv", "run", str(HERE / "exact_sweep.py"), "--quaketool", str(qt), "--maps", ",".join(MAPS),
             "--views", "standard,sweep,roll", "--mode", "ents", "--oracles", "sse,x87"]
    text, failed = "", False
    for extra in ([], ["--res", "640x400", "--aspect", "0.8333333"]):
        res = subprocess.run(sweep + extra, cwd=PROJECT, capture_output=True, text=True, timeout=1800)
        text += res.stdout + res.stderr
        failed |= res.returncode != 0
    (out / "exact.txt").write_text(text)
    if failed:
        raise RuntimeError("exact_sweep.py: a view differs from id's SSE build (see exact.txt)")
    return {}


def check_screen2d(qt: Path, out: Path) -> dict:
    d = out / "screen2d"
    text = run(["uv", "run", HERE / "screen2d.py", "--out", d])
    (out / "screen2d.txt").write_text(text)
    shots = json.loads((d / "summary.json").read_text())
    return {f"screen2d.{k}": f"{v['exact2d_pct']:.2f}" for k, v in shots.items()}


def check_demolerp(qt: Path, out: Path) -> dict:
    keep = out / "demo_lerp"
    keep.mkdir(parents=True, exist_ok=True)
    res = subprocess.run(["uv", "run", str(HERE / "demo_lerp.py"), "--quaketool", str(qt), "--keep", str(keep)],
                         cwd=PROJECT, capture_output=True, text=True, timeout=1800)
    (out / "demo_lerp.txt").write_text(res.stdout + res.stderr)
    if res.returncode != 0 or "DIFFER" in res.stdout:
        raise RuntimeError("demo_lerp.py: the port's demo playback left id's (see demo_lerp.txt)")
    return {}


def check_sound(qt: Path, out: Path) -> dict:
    text = run(["uv", "run", HERE / "sound.py", "--quaketool", qt, "--out", out / "sound"])
    (out / "sound.txt").write_text(text)
    if "Classic: every case identical" not in text:
        raise RuntimeError("sound.py: not every Classic case is identical (see sound.txt)")
    walk = ["uv", "run", str(HERE / "sound_walk.py"), "--quaketool", str(qt), "--out", str(out / "sound-walk")]
    res = subprocess.run(walk, cwd=PROJECT, capture_output=True, text=True, timeout=1800)
    (out / "sound_walk.txt").write_text(res.stdout + res.stderr)
    if res.returncode != 0 or "every case identical" not in res.stdout:
        raise RuntimeError("sound_walk.py: the walks' sound calls left id's (see sound_walk.txt)")
    return {}


def compare_values(name: str, got: dict, expected: dict) -> list[str]:
    """The failures of `got` against `expected`: an identity value that moved,
    or (screen2d) a shot that matches id's layer less than it did."""
    fails = []
    for k, v in sorted(got.items()):
        want = expected.get(k)
        if want is None:
            fails.append(f"{k}: {v} (nothing recorded)")
        elif name in ("oracle", "screen2d"):
            if float(v) < float(want):
                fails.append(f"{k}: {v}% < recorded {want}%")
        elif v != want:
            fails.append(f"{k}: {v} != recorded {want}")
    missing = [k for k in expected if k.split(".")[0] == name and k not in got]
    fails += [f"{k}: not produced" for k in missing]
    return fails


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", help="comma list of: " + ", ".join(CHECKS))
    ap.add_argument("--record", action="store_true", help="write the identity values to classic_expected.txt")
    ap.add_argument("--note", default="", help="with --record: the tree it was recorded on")
    ap.add_argument("--quaketool", type=Path, help="use this quaketool instead of building quake-rs")
    ap.add_argument("--out", type=Path, default=HERE / "build" / "classic-check")
    args = ap.parse_args()
    checks = args.only.split(",") if args.only else CHECKS
    unknown = set(checks) - set(CHECKS)
    if unknown:
        ap.error(f"unknown checks {sorted(unknown)}")
    args.out.mkdir(parents=True, exist_ok=True)
    qt = args.quaketool.resolve() if args.quaketool else build_quaketool()
    expected = load_expected()
    recorded = dict(expected)
    report, ok = [], True
    for name in checks:
        t0 = time.monotonic()
        got = {}
        try:
            got = globals()[f"check_{name}"](qt, args.out)
            fails = [] if args.record else compare_values(name, got, expected)
            recorded.update(got)
        except Exception as e:  # a tool failed: the check did
            fails = [str(e)]
        secs = time.monotonic() - t0
        status = "PASS" if not fails else "FAIL"
        ok &= not fails
        detail = f"{len(got)} values match" if not fails else "; ".join(fails[:6])
        report.append(f"{status}  {name:<9} {secs:6.1f} s  {detail}")
        print(report[-1], flush=True)
    if args.record:
        save_expected(recorded, args.note or "recorded")
        print(f"recorded {len(recorded)} values in {EXPECTED}")
    summary = "\n".join(report) + f"\n{'ALL PASS' if ok else 'FAILED'}\n"
    (args.out / "classic_check.txt").write_text(summary)
    print(summary.splitlines()[-1], f"(report: {args.out / 'classic_check.txt'})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
