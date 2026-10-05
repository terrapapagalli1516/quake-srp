#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Render the film's game footage from its shot files, in one command.

    uv run film/render.py                     # every shot in film/shots/, into film/out/
    uv run film/render.py S01 ST1a-on         # only these (a name, or a shot file's path)
    uv run film/render.py --png S10           # PNG frames instead of video (no ffmpeg needed)
    uv run film/render.py --list              # the shots and what each one shows

Each shot file goes through `quaketool film` and, unless `--png`, through ffmpeg into
`OUT/NAME.mp4`: H.264, CRF 16, 4:2:0 in BT.709, at the shot's own size and frame rate,
the film's master format. A shot that writes its game sound or its events log gets them
beside it, as `OUT/NAME.wav` and `OUT/NAME.events.json`. With `--png` each shot is a
folder of numbered frames, `OUT/NAME/00000.png` on, with its `sound.wav` and
`events.json`.

Needs cargo (it builds quaketool, as `oracle/classic_check.py` does; `--quaketool BIN`
uses that one instead), id's shareware pak at `quake-data/ID1/PAK0.PAK` (`--pak`), and
ffmpeg with libx264 for video. Shots are rendered one at a time: each uses every core.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

FILM = Path(__file__).resolve().parent
ROOT = FILM.parent
SHOTS = FILM / "shots"
CRATE = ROOT / "quake-rs"


def build_quaketool() -> Path:
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=CRATE, check=True)
    return CRATE / "target" / "release" / "quaketool"


def shot_lines(path: Path) -> list[str]:
    """The shot file's lines, as `quaketool film` reads them: a `#` that starts a
    word starts a comment, and blank lines are skipped."""
    out = []
    for raw in path.read_text().splitlines():
        words = []
        for w in raw.split():
            if w.startswith("#"):
                break
            words.append(w)
        if words:
            out.append(" ".join(words))
    return out


def frame_size_and_rate(path: Path) -> tuple[int, int, str]:
    """The output frames' size and rate: the last `size` and `fps` lines, or the
    tool's defaults (1920x1080, 60)."""
    w, h, fps = 1920, 1080, "60"
    for line in shot_lines(path):
        key, _, value = line.partition(" ")
        if key == "size":
            w, h = (int(v) for v in value.lower().split("x"))
        elif key == "fps":
            fps = value
    return w, h, fps


def summary(path: Path) -> str:
    """The first sentence of the shot file's leading comment, after its name."""
    words = []
    for raw in path.read_text().splitlines():
        text = raw.removeprefix("#").strip()
        if not raw.startswith("#") or not text:
            break
        words += text.split()
    first = " ".join(words).split(". ")[0].rstrip(".")
    return first.split(": ", 1)[-1]


def encoder(out: Path, w: int, h: int, fps: str) -> list[str]:
    """ffmpeg reading raw RGB frames, writing the film's H.264. An odd size gets a
    black row or column (4:2:0 halves both)."""
    vf = "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"
    if w % 2 or h % 2:
        vf = "pad=ceil(iw/2)*2:ceil(ih/2)*2," + vf
    return ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{w}x{h}", "-r", fps, "-i", "-",
            "-vf", vf, "-c:v", "libx264", "-preset", "medium", "-crf", "16", "-pix_fmt", "yuv420p",
            "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
            "-movflags", "+faststart", str(out)]


def render_video(qt: Path, pak: Path, shot: Path, out: Path) -> str:
    """The shot as OUT/NAME.mp4, its sound and events beside it. Returns the tool's report."""
    w, h, fps = frame_size_and_rate(shot)
    name = shot.stem
    with tempfile.TemporaryDirectory(prefix=f"{name}-", dir=out) as tmp:
        log = Path(tmp) / "report.txt"
        with log.open("w") as err:
            film = subprocess.Popen([qt, "film", pak, shot, tmp, "--raw"], stdout=subprocess.PIPE, stderr=err)
            ff = subprocess.run(encoder(out / f"{name}.mp4", w, h, fps), stdin=film.stdout)
            film.stdout.close()
            code = film.wait()
        report = log.read_text(errors="replace").strip()
        if code != 0 or ff.returncode != 0:
            raise RuntimeError(f"{name}: {report or 'ffmpeg failed'}")
        for part, dst in (("sound.wav", f"{name}.wav"), ("events.json", f"{name}.events.json")):
            if (Path(tmp) / part).exists():
                shutil.move(Path(tmp) / part, out / dst)
    return report


def render_png(qt: Path, pak: Path, shot: Path, out: Path) -> str:
    """The shot's frames as OUT/NAME/00000.png on, with its sound.wav and events.json."""
    res = subprocess.run([qt, "film", pak, shot, out / shot.stem], capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f"{shot.stem}: {res.stderr.strip() or res.stdout.strip()}")
    return res.stdout.strip()


def find_shots(names: list[str]) -> list[Path]:
    if not names:
        return sorted(SHOTS.glob("*.shot"))
    shots = []
    for n in names:
        p = Path(n) if n.endswith(".shot") else SHOTS / f"{n}.shot"
        if not p.exists():
            sys.exit(f"render.py: no shot {n} ({p})")
        shots.append(p)
    return shots


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("shots", nargs="*", help="shot names (film/shots/NAME.shot) or paths; default: all")
    ap.add_argument("--out", type=Path, default=FILM / "out")
    ap.add_argument("--pak", type=Path, default=ROOT / "quake-data" / "ID1" / "PAK0.PAK")
    ap.add_argument("--quaketool", type=Path, help="use this quaketool instead of building quake-rs")
    ap.add_argument("--png", action="store_true", help="PNG frames, not video")
    ap.add_argument("--list", action="store_true", help="list the shots and stop")
    args = ap.parse_args()
    shots = find_shots(args.shots)
    if args.list:
        for s in shots:
            print(f"{s.stem:<22} {summary(s)}")
        return 0
    if not args.pak.exists():
        sys.exit(f"render.py: no pak at {args.pak} (README.md, \"Build and run it\", fetches it)")
    if not args.png and not shutil.which("ffmpeg"):
        sys.exit("render.py: no ffmpeg on the PATH (or render PNG frames with --png)")
    qt = args.quaketool.resolve() if args.quaketool else build_quaketool()
    args.out.mkdir(parents=True, exist_ok=True)
    failed = []
    for i, shot in enumerate(shots, 1):
        t0 = time.monotonic()
        try:
            report = (render_png if args.png else render_video)(qt, args.pak.resolve(), shot.resolve(), args.out)
        except RuntimeError as e:
            failed.append(shot.stem)
            print(f"[{i}/{len(shots)}] FAILED {e}", flush=True)
            continue
        first = report.splitlines()[0] if report else ""
        print(f"[{i}/{len(shots)}] {shot.stem}: {first} ({time.monotonic() - t0:.0f} s)", flush=True)
    if failed:
        print(f"failed: {' '.join(failed)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
