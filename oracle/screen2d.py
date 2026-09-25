#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "pillow"]
# ///
"""The 2-D layer against id's own renderer: the same scenario (player state,
viewsize, menu, console, centerprint, intermission, ...) played through id's
WinQuake (the C oracle) and through the port's live App (the wasm crate's
`oracle_screen` harness, natively), and the two composited screens diffed
pixel for pixel.

    uv run oracle/screen2d.py                          # every scenario, 320x200 + 640x400
    uv run oracle/screen2d.py --res 320x200 --only hud,menu_main
    uv run oracle/screen2d.py --list

Both sides paint the 3-D view one flat palette index (`oracle_blank`, the
harness's `blank`), so what differs is the 2-D layer: the status bar, the
inventory and scoreboards, the menus and their fade, the console, the notify
lines, centerprints, the intermission and finale overlays, the backtile.

Per shot it writes <out>/<res>/<scenario>.<shot>.{c.ppm,c.pgm,c.json,port.ppm,side.png}
(side = C | port | diff at 2x, diff white where the RGB differs) and prints
exact% over the whole screen and over the 2-D pixels (those that are not the
blank colour in either screen). Colours are compared as presented: after the
palette shift (V_UpdatePalette's cshifts), so a powerup tint counts too.
"""

from __future__ import annotations

import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from compare import DEFAULT_PAK, ensure_oracle, read_pak_file, read_pnm  # noqa: E402

PROJECT = HERE.parent
WASM = PROJECT / "quake-wasm"
BLANK = 15  # the 3-D view's palette index in both renderers

# QuakeC items bits (defs.qc)
IT_SHOTGUN, IT_SUPER_SHOTGUN, IT_NAILGUN, IT_SUPER_NAILGUN = 1, 2, 4, 8
IT_GRENADE_LAUNCHER, IT_ROCKET_LAUNCHER, IT_LIGHTNING = 16, 32, 64
IT_SHELLS, IT_NAILS, IT_ROCKETS, IT_CELLS, IT_AXE = 256, 512, 1024, 2048, 4096
IT_ARMOR1, IT_ARMOR2, IT_ARMOR3 = 8192, 16384, 32768
IT_KEY1, IT_KEY2, IT_INVISIBILITY, IT_INVULNERABILITY, IT_SUIT, IT_QUAD = (
    131072, 262144, 524288, 1048576, 2097152, 4194304)
ALL_WEAPONS = (IT_AXE | IT_SHOTGUN | IT_SUPER_SHOTGUN | IT_NAILGUN | IT_SUPER_NAILGUN
               | IT_GRENADE_LAUNCHER | IT_ROCKET_LAUNCHER | IT_LIGHTNING)

# ---------------------------------------------------------------------------
# Scenarios: lists of abstract steps, each played on both sides.
#   ("viewsize", n) ("frames", n) ("field", name, *vals) ("serverflags", n)
#   ("impulse", n) ("console",) ("type", text) ("key", NAME) ("showscores", 0|1)
#   ("centerprint", text) ("print", text) ("intermission", n, t, text)
#   ("faceanim",) ("cmd", line) ("shot", name)
# Every scenario starts on e1m1 after 20 frames (the console has retracted), at viewsize 100.
# ---------------------------------------------------------------------------


def stats(health=100, armor=0, items=IT_SHOTGUN | IT_AXE | IT_SHELLS, weapon=IT_SHOTGUN,
          shells=25, nails=0, rockets=0, cells=0, current=None):
    cur = {IT_AXE: 0, IT_SHOTGUN: shells, IT_SUPER_SHOTGUN: shells, IT_NAILGUN: nails,
           IT_SUPER_NAILGUN: nails, IT_GRENADE_LAUNCHER: rockets, IT_ROCKET_LAUNCHER: rockets,
           IT_LIGHTNING: cells}[weapon] if current is None else current
    return [("field", "health", health), ("field", "armorvalue", armor), ("field", "items", items),
            ("field", "weapon", weapon), ("field", "ammo_shells", shells), ("field", "ammo_nails", nails),
            ("field", "ammo_rockets", rockets), ("field", "ammo_cells", cells),
            ("field", "currentammo", cur)]


FULL = ALL_WEAPONS | IT_NAILS | IT_KEY1 | IT_KEY2 | IT_ARMOR2
SETTLE = [("frames", 25)]  # past the 2 s new-item flash

SCENARIOS: dict[str, list] = {
    "hud": [("frames", 1), ("shot", "v100"), ("viewsize", 110), ("frames", 2), ("shot", "v110"),
            ("viewsize", 120), ("frames", 2), ("shot", "v120"), ("viewsize", 50), ("frames", 2),
            ("shot", "v50"), ("viewsize", 30), ("frames", 2), ("shot", "v30")],
    "full": stats(health=100, armor=150, items=FULL, weapon=IT_SUPER_NAILGUN, shells=40, nails=173,
                  rockets=12, cells=9) + [("serverflags", 15)] + SETTLE + [("shot", "inv")],
    "weapons": stats(items=FULL, shells=100, nails=200, rockets=100, cells=100) + SETTLE + sum(
        ([("field", "weapon", wb), ("field", "currentammo", amm), ("frames", 1), ("shot", nm)]
         for nm, wb, amm in [("axe", IT_AXE, 0), ("sg", IT_SHOTGUN, 100), ("ssg", IT_SUPER_SHOTGUN, 100),
                             ("ng", IT_NAILGUN, 200), ("sng", IT_SUPER_NAILGUN, 200),
                             ("gl", IT_GRENADE_LAUNCHER, 100), ("rl", IT_ROCKET_LAUNCHER, 100),
                             ("lg", IT_LIGHTNING, 100)]), []),
    "faces": stats(health=100) + SETTLE + sum(
        ([("field", "health", hp), ("frames", 1), ("shot", f"hp{hp}")] for hp in (250, 99, 79, 59, 39, 19, 5)),
        []) + [("faceanim",), ("shot", "pain"), ("field", "health", -5), ("frames", 1),
               ("shot", "dead")],
    "powerups": stats(items=IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_QUAD) + SETTLE + [("shot", "quad")]
    + [("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_INVISIBILITY), ("frames", 25), ("shot", "invis")]
    + [("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_INVULNERABILITY), ("frames", 25), ("shot", "invuln")]
    + [("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_SUIT), ("frames", 25), ("shot", "suit")]
    + [("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_INVISIBILITY | IT_INVULNERABILITY),
       ("frames", 25), ("shot", "invis_invuln")],
    "armor": stats(armor=25, items=IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_ARMOR1) + SETTLE + [("shot", "green")]
    + [("field", "armorvalue", 150), ("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_ARMOR2),
       ("frames", 25), ("shot", "yellow")]
    + [("field", "armorvalue", 200), ("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_ARMOR3),
       ("frames", 25), ("shot", "red")],
    "flash": stats() + SETTLE + [("field", "items", IT_SHOTGUN | IT_AXE | IT_SHELLS | IT_ROCKET_LAUNCHER),
                                 ("frames", 3), ("shot", "rl_new")],
    "scores": [("frames", 10), ("showscores", 1), ("frames", 1), ("shot", "tab")],
    "centerprint": [("frames", 1), ("centerprint", "One line"), ("frames", 1), ("shot", "one"),
                    ("centerprint", "The first line\\nsecond\\nand the third one here"), ("frames", 1),
                    ("shot", "three"), ("frames", 25), ("shot", "gone")],
    "notify": [("frames", 1), ("print", "First notify line"), ("frames", 1),
               ("print", "a second, rather longer line that will wrap at the console width"),
               ("frames", 1), ("shot", "lines")],
    "console": [("frames", 1), ("cmd", "clear"), ("print", "Some console text"), ("print", "a second line"),
                ("console",), ("frames", 2), ("shot", "sliding"), ("frames", 10), ("shot", "down"),
                ("type", "god"), ("frames", 1), ("shot", "typed")],
    "intermission": [("frames", 1), ("intermission", 1, 75, ""), ("frames", 2), ("shot", "stats")],
    "finale": [("frames", 1), ("intermission", 2, 75,
                                "As the corpse of the monstrous entity\\nChthon sinks back into the lava whence\\n"
                                "it rose, you grip the Rune of Earth\\nMagic tightly."),
               ("frames", 20), ("shot", "reveal"), ("frames", 30), ("shot", "later")],
    "menu_main": [("frames", 1), ("key", "ESCAPE"), ("frames", 1), ("shot", "main"),
                  ("key", "DOWNARROW"), ("frames", 1), ("shot", "main_row2")],
    "menu_sp": [("frames", 1), ("key", "ESCAPE"), ("key", "ENTER"), ("frames", 1), ("shot", "sp"),
                ("key", "DOWNARROW"), ("key", "ENTER"), ("frames", 1), ("shot", "load"),
                ("key", "ESCAPE"), ("key", "DOWNARROW"), ("key", "ENTER"), ("frames", 1), ("shot", "save")],
    "menu_mp": [("frames", 1), ("key", "ESCAPE"), ("key", "DOWNARROW"), ("key", "ENTER"), ("frames", 1),
                ("shot", "mp")],
    "menu_options": [("frames", 1), ("key", "ESCAPE"), ("key", "DOWNARROW"), ("key", "DOWNARROW"),
                     ("key", "ENTER"), ("frames", 1), ("shot", "options"),
                     ("key", "ENTER"), ("frames", 1), ("shot", "keys"),
                     ("key", "ESCAPE"), ("key", "UPARROW"), ("key", "ENTER"), ("frames", 1), ("shot", "video")],
    "menu_help": [("frames", 1), ("key", "ESCAPE"), ("key", "DOWNARROW"), ("key", "DOWNARROW"),
                  ("key", "DOWNARROW"), ("key", "ENTER"), ("frames", 1), ("shot", "help1"),
                  ("key", "RIGHTARROW"), ("frames", 1), ("shot", "help2")],
    "menu_quit": [("frames", 1), ("key", "ESCAPE"), ("key", "UPARROW"), ("key", "ENTER"), ("frames", 1),
                  ("shot", "quit")],
}


# ---------------------------------------------------------------------------


def c_lines(steps, out: Path, name: str) -> list[str]:
    # cl_forwardspeed/cl_backspeed 400: Always Run, the port's one default departure
    lines = ["oracle_exit 0", "oracle_stage 1", f"oracle_blank {BLANK}", "crosshair 0", "viewsize 100",
             "cl_forwardspeed 400", "cl_backspeed 400",
             "map e1m1"] + ["wait"] * 20
    for st in steps:
        op, args = st[0], st[1:]
        if op == "viewsize":
            lines.append(f"viewsize {args[0]}")
        elif op == "frames":
            lines += ["wait"] * args[0]
        elif op == "field":
            lines.append(f"oracle_field {args[0]} " + " ".join(str(v) for v in args[1:]))
        elif op == "serverflags":
            lines.append(f"oracle_global serverflags {args[0]}")
        elif op == "impulse":
            lines.append(f"impulse {args[0]}")
        elif op == "console":
            lines.append("toggleconsole")
        elif op == "type":
            lines += [f"oracle_key {'SPACE' if ch == ' ' else ch}" for ch in args[0]] + ["oracle_key ENTER"]
        elif op == "key":
            lines.append(f"oracle_key {args[0]}")
        elif op == "showscores":
            lines.append("+showscores" if args[0] else "-showscores")
        elif op == "centerprint":
            lines.append(f"oracle_centerprint {args[0]}")
        elif op == "print":
            lines.append(f"echo {args[0]}")
        elif op == "intermission":
            lines.append(f"oracle_intermission {args[0]} {args[1]} {args[2]}".rstrip())
        elif op == "faceanim":
            lines.append("oracle_faceanim")
        elif op == "cmd":
            lines.append(args[0])
        elif op == "shot":
            # the shot is taken at the end of this frame: the commands after it
            # must wait for the next one
            lines += [f'oracle_shot "{out / name}.{args[0]}.c"', "wait"]
        else:
            raise SystemExit(f"unknown step {st}")
    return lines + ["wait", "oracle_quit"]


def port_lines(steps, out: Path, name: str, res, metas: dict) -> list[str]:
    w, h = res
    lines = [f"res {w} {h}", f"blank {BLANK}", "map e1m1", "cmd viewsize 100", "frames 20"]
    for st in steps:
        op, args = st[0], st[1:]
        if op == "viewsize":
            lines.append(f"cmd viewsize {args[0]}")
        elif op == "frames":
            lines.append(f"frames {args[0]}")
        elif op == "field":
            lines.append(f"field {args[0]} " + " ".join(str(v) for v in args[1:]))
        elif op in ("serverflags", "impulse", "type", "key", "showscores", "centerprint", "cmd"):
            lines.append(f"{op} {args[0]}")
        elif op == "console":
            lines.append("console")
        elif op == "print":
            # echo prints each argument and a space, then a newline
            lines.append(f"print {' '.join(args[0].split())} \\n")
        elif op == "intermission":
            lines.append(f"intermission {args[0]} {args[1]} {args[2]}".rstrip())
        elif op == "faceanim":
            lines.append("faceanim")
        elif op == "shot":
            m = metas[args[0]]
            lines.append(f"clocks {m['realtime']!r} {m['host_time']!r} {m['time']!r} {m['centertime_start']!r}")
            lines.append(f"shot {out / name}.{args[0]}.port.ppm")
        else:
            raise SystemExit(f"unknown step {st}")
    return lines


def run_c(oracle: Path, pak: Path, res, steps, out: Path, name: str) -> dict:
    w, h = res
    with tempfile.TemporaryDirectory(prefix="quake-screen2d-") as tmp:
        base = Path(tmp)
        (base / "id1").mkdir()
        (base / "id1" / "pak0.pak").symlink_to(pak.resolve())
        (base / "id1" / "screen.cfg").write_text("\n".join(c_lines(steps, out, name)) + "\n")
        res_ = subprocess.run([str(oracle), "-basedir", str(base), "-width", str(w), "-height", str(h),
                               "+exec", "screen.cfg"], cwd=base, capture_output=True, text=True, timeout=300)
    metas = {}
    for st in steps:
        if st[0] == "shot":
            p = out / f"{name}.{st[1]}.c.json"
            if not p.exists():
                sys.exit(f"C oracle wrote no {p} (rc {res_.returncode}):\n{res_.stdout[-3000:]}{res_.stderr[-1000:]}")
            metas[st[1]] = json.loads(p.read_text())
    return metas


def build_harness() -> Path:
    env = dict(os.environ, CARGO_PROFILE_RELEASE_LTO="false", CARGO_PROFILE_RELEASE_CODEGEN_UNITS="16")
    res = subprocess.run(["cargo", "test", "--release", "--lib", "--no-run", "--message-format=json"],
                         cwd=WASM, env=env, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"building the harness failed:\n{res.stderr[-4000:]}")
    for line in res.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("executable") and msg["target"]["name"] == "quake_wasm":
            return Path(msg["executable"])
    sys.exit("no test executable in cargo's output")


def run_port(harness: Path, script: list[str], out: Path, name: str):
    sp = out / f"{name}.port.txt"
    sp.write_text("\n".join(script) + "\n")
    res = subprocess.run([str(harness), "--exact", "oracle_screen::oracle_screen", "--ignored", "--test-threads=1"],
                         env=dict(os.environ, QUAKE_SCREEN_SCRIPT=str(sp)), capture_output=True, text=True,
                         timeout=600)
    if res.returncode != 0:
        sys.exit(f"port harness failed for {name}:\n{res.stdout[-3000:]}{res.stderr[-2000:]}")


def measure(out: Path, name: str, shot: str, pal: np.ndarray, meta: dict) -> dict:
    stem = out / f"{name}.{shot}"
    c_idx = read_pnm(Path(f"{stem}.c.pgm"))
    c_rgb = read_pnm(Path(f"{stem}.c.ppm"))
    p_rgb = read_pnm(Path(f"{stem}.port.ppm"))
    if c_rgb.shape != p_rgb.shape:
        sys.exit(f"{stem}: size mismatch C {c_rgb.shape} vs port {p_rgb.shape}")
    same = (c_rgb == p_rgb).all(axis=2)
    blank = c_idx == BLANK
    blank_rgb = c_rgb[blank][0] if blank.any() else pal[BLANK]
    p_blank = (p_rgb == blank_rgb).all(axis=2)
    mask2d = ~blank | ~p_blank
    diff = (~same).astype(np.uint8) * 255
    gap = np.full((c_rgb.shape[0], 2, 3), (255, 0, 255), dtype=np.uint8)
    side = np.concatenate([c_rgb, gap, p_rgb, gap, np.stack([diff] * 3, axis=2)], axis=1)
    img = Image.fromarray(side)
    img.resize((img.width * 2, img.height * 2), Image.NEAREST).save(f"{stem}.side.png")
    return {
        "exact_pct": float(same.mean() * 100),
        "px2d": int(mask2d.sum()),
        "exact2d_pct": float(same[mask2d].mean() * 100) if mask2d.any() else 100.0,
        "diff_px": int((~same).sum()),
        "vrect": meta.get("scr_vrect"),
    }


def parse_res(s: str):
    w, h = s.lower().split("x")
    return int(w), int(h)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--res", default="320x200,640x400", help="comma-separated WxH list")
    ap.add_argument("--only", help="comma-separated scenario names (default: all)")
    ap.add_argument("--list", action="store_true", help="list the scenarios and exit")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--pak", type=Path, default=DEFAULT_PAK)
    ap.add_argument("--oracle", help="C oracle binary (default oracle/build/quake-oracle)")
    ap.add_argument("--harness", help="port harness test binary (default: build quake-wasm's tests)")
    ap.add_argument("--out", type=Path, help="output dir (default: a new temp dir)")
    args = ap.parse_args()
    if args.list:
        for n, steps in SCENARIOS.items():
            print(f"{n:<14} shots: {', '.join(s[1] for s in steps if s[0] == 'shot')}")
        return
    names = args.only.split(",") if args.only else list(SCENARIOS)
    for n in names:
        if n not in SCENARIOS:
            sys.exit(f"unknown scenario {n!r} (--list)")
    out_root = (args.out or Path(tempfile.mkdtemp(prefix="quake-screen2d-"))).resolve()
    oracle = ensure_oracle(args.oracle)
    harness = Path(args.harness).resolve() if args.harness else build_harness()
    pal = np.frombuffer(read_pak_file(args.pak, "gfx/palette.lmp")[:768], dtype=np.uint8).reshape(256, 3)

    def one(res, name):
        out = out_root / f"{res[0]}x{res[1]}"
        out.mkdir(parents=True, exist_ok=True)
        steps = SCENARIOS[name]
        metas = run_c(oracle, args.pak, res, steps, out, name)
        run_port(harness, port_lines(steps, out, name, res, metas), out, name)
        return [(res, name, s[1], measure(out, name, s[1], pal, metas[s[1]])) for s in steps if s[0] == "shot"]

    t0 = time.time()
    jobs = [(parse_res(r), n) for r in args.res.split(",") for n in names]
    with cf.ThreadPoolExecutor(args.jobs) as ex:
        results = [r for rs in ex.map(lambda j: one(*j), jobs) for r in rs]
    summary = {}
    print(f"{'res':<8} {'scenario.shot':<28} {'exact%':>7} {'2d px':>7} {'2d exact%':>9} {'diff px':>8}")
    for res, name, shot, st in results:
        key = f"{res[0]}x{res[1]}/{name}.{shot}"
        summary[key] = st
        print(f"{res[0]}x{res[1]:<4} {name + '.' + shot:<28} {st['exact_pct']:7.2f} {st['px2d']:7d} "
              f"{st['exact2d_pct']:9.2f} {st['diff_px']:8d}")
    (out_root / "summary.json").write_text(json.dumps(summary, indent=2))
    print(f"\nimages + summary.json in {out_root} ({time.time() - t0:.0f} s)")


if __name__ == "__main__":
    main()
