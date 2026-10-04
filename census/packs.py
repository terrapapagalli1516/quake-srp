#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""The edicts comparison (`classic_check.py`'s `edicts`) over the mission packs.

For every map of Scourge of Armagon (`hipnotic`) and Dissolution of Eternity
(`rogue`), id's own server (the C oracle, run with `-hipnotic`/`-rogue` over a
`-basedir` holding `id1/` and the pack's own directory) and the port's
(`quaketool census-edicts` on the same paks, layered the same way) load the
map, connect the player and stand idle; at each server time both dump every
live edict, and `edict_diff.py` lists what differs. A pack's maps are what
reach the engine paths id1 never does (`AUDIT.md`, "The mission packs'
paths"), so a divergence here is a candidate bug of that class.

    uv run census/packs.py --data DIR [--games hipnotic,rogue] [--maps hip1m1,...]
                           [--times 1.7,4.7,10.7] [--out DIR]

`--data` holds `id1/pak0.pak`, `id1/pak1.pak` (id's `-hipnotic`/`-rogue`
refuse to run without the registered game: `COM_CheckRegistered`) and each
pack's `pak0.pak` (`hipnotic/`, `rogue/`), as a deploy of the page does.
Writes, per map, `<game>/<map>.c.txt` / `.port.txt` (the dumps), `.c.log`
(id's console: a `Host_Error`, `PR_RunError` or `Sys_Error` there is id's own
verdict on the map) and `.diff.txt`, and `summary.txt`: one row per map with
the edict counts at the last time, the three `edict_diff` counts at each
time, and id's console errors. The run directory (`--out`, default
`oracle/build/packs`) also holds the oracle's `-basedir`, as symlinks.

Monster AI draws from id's `rand()` and the port's own PRNG, so an awake
monster wanders apart (`edict_diff.py`'s note); the times are those of the
Classic check, early enough that most monsters are still asleep.
"""

import argparse
import re
import struct
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
sys.path.insert(0, str(PROJECT / "oracle"))
from oraclebin import ORACLE_BIN as ORACLE, ensure_oracle  # noqa: E402

CRATE = PROJECT / "quake-rs"
QUAKETOOL = CRATE / "target" / "release" / "quaketool"
GAMES = ("hipnotic", "rogue")
# id's console lines that end or poison the run: the map is id's verdict.
ERRORS = re.compile(r"Host_Error|PR_RunError|Sys_Error|SV_Error|ED_Alloc|is not a field|no precache|"
                    r"Bad entity|NULL function|runaway|stack overflow|Mod_.*not found|Illegible")


def pak_maps(pak: Path) -> list[str]:
    """The `maps/*.bsp` in `pak` that are levels (not the `b_*` item boxes)."""
    data = pak.read_bytes()
    _magic, dirofs, dirlen = struct.unpack_from("<4sii", data, 0)
    out = []
    for i in range(dirlen // 64):
        (raw,) = struct.unpack_from("<56s", data, dirofs + 64 * i)
        name = raw.split(b"\0", 1)[0].decode("latin-1")
        if m := re.fullmatch(r"maps/([^/]+)\.bsp", name):
            if not m[1].startswith("b_"):
                out.append(m[1])
    return sorted(out)


def basedir(out: Path, data: Path, game: str) -> Path:
    """The oracle's `-basedir`: id1's two paks and the pack's own, linked."""
    base = out / f"base-{game}"
    for sub, names in (("id1", ("pak0.pak", "pak1.pak")), (game, ("pak0.pak",))):
        (base / sub).mkdir(parents=True, exist_ok=True)
        for n in names:
            link = base / sub / n
            if not link.exists():
                link.symlink_to((data / sub / n).resolve())
    return base


def run_c(base: Path, game: str, mapname: str, times: list[float], dump: Path, skill: int) -> str:
    """id's server: `map`, then idle 0.1 s frames to each time, dumping edicts.
    The player connects at sv.time 1.2 as in census/oracle_run.py; returns
    id's console."""
    dump.unlink(missing_ok=True)
    lines, now = [f"skill {skill}", f"map {mapname}"], 1.2
    for t in times:
        lines += ["wait"] * round((t - now) / 0.1)
        lines.append(f"oracle_edicts {dump}")
        now = t
    lines.append("oracle_quit")
    (base / "id1" / "packs.cfg").write_text("\n".join(lines) + "\n")
    try:
        res = subprocess.run([str(ORACLE), "-basedir", str(base), "-width", "320", "-height", "200",
                              f"-{game}", "+exec", "packs.cfg"], cwd=base, capture_output=True,
                             text=True, errors="replace", timeout=600)
        return res.stdout + res.stderr + f"\n[oracle rc={res.returncode}]\n"
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode("latin-1") if isinstance(e.stdout, bytes) else (e.stdout or "")
        return out + "\n[oracle TIMEOUT after 600 s]\n"


def run_port(data: Path, game: str, mapname: str, times: list[float]) -> str:
    paks = ",".join(str(data / p) for p in ("id1/pak0.pak", "id1/pak1.pak", f"{game}/pak0.pak"))
    res = subprocess.run([str(QUAKETOOL), "census-edicts", paks, mapname, ",".join(map(str, times))],
                         capture_output=True, text=True, errors="replace", timeout=600)
    if res.returncode != 0:
        return f"# port FAILED: {res.stdout[-500:]}{res.stderr[-1500:]}\n"
    return res.stdout


def counts(diff: str) -> list[str]:
    """`edict_diff.py`'s three counts per time block: only-C/only-port/different."""
    out = []
    for block in diff.split("=== t=")[1:]:
        t = block.split(":", 1)[0]
        n = [re.search(rf"{k} \((\d+)", block) for k in ("only in C", "only in port", "matched but different")]
        out.append(f"t={t} " + "/".join(m[1] if m else "?" for m in n))
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", required=True, type=Path)
    ap.add_argument("--games", default=",".join(GAMES))
    ap.add_argument("--maps", default=None, help="comma list (default: every level of each game)")
    ap.add_argument("--times", default="1.7,4.7,10.7")
    ap.add_argument("--skill", type=int, default=1)
    ap.add_argument("--out", type=Path, default=PROJECT / "oracle" / "build" / "packs")
    a = ap.parse_args()
    times = sorted(float(t) for t in a.times.split(","))
    ensure_oracle()
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=CRATE, check=True)
    out = a.out.resolve()
    summary = []
    for game in a.games.split(","):
        base = basedir(out, a.data.resolve(), game)
        (out / game).mkdir(parents=True, exist_ok=True)
        maps = a.maps.split(",") if a.maps else pak_maps(a.data / game / "pak0.pak")
        for m in maps:
            stem = out / game / m
            log = run_c(base, game, m, times, stem.with_suffix(".c.txt"), a.skill)
            stem.with_suffix(".c.log").write_text(log)
            port = run_port(a.data.resolve(), game, m, times)
            stem.with_suffix(".port.txt").write_text(port)
            errs = sorted({l.strip()[:90] for l in log.splitlines() if ERRORS.search(l)})
            campaign = sum(1 for l in log.splitlines() if l.startswith("Cvar_Set: variable campaign"))
            if not stem.with_suffix(".c.txt").exists() or port.startswith("# port FAILED"):
                diff = ""
                row = [f"{game}/{m}", "no comparison:",
                       "C dumped nothing" if not stem.with_suffix(".c.txt").exists() else "",
                       port.strip()[:200] if port.startswith("# port") else ""]
            else:
                diff = subprocess.run(["uv", "run", str(HERE / "edict_diff.py"), str(stem.with_suffix(".c.txt")),
                                       str(stem.with_suffix(".port.txt"))], capture_output=True, text=True).stdout
                last = [l for l in diff.splitlines() if l.startswith("=== t=")]
                row = [f"{game}/{m}", last[-1][4:] if last else "?", " ".join(counts(diff))]
            stem.with_suffix(".diff.txt").write_text(diff)
            row.append(f"C console: {campaign} campaign lines; " + ("; ".join(errs) if errs else "no errors"))
            summary.append("  ".join(r for r in row if r))
            print(summary[-1], flush=True)
    (out / "summary.txt").write_text("\n".join(summary) + "\n")
    print(f"[{out / 'summary.txt'}]")


if __name__ == "__main__":
    sys.exit(main())
