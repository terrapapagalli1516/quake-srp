# /// script
# requires-python = ">=3.12"
# dependencies = ["playwright==1.63.0", "pillow"]
# ///
"""The film's web footage: the real page, captured in a headless Chromium.

    uv run film/pipeline/web/capture.py --scale 2 --out OUT/footage/web [--only F01,S18] [--hw vaapi|none|auto]

Builds the page's program from this repository (the threads build, `cargo build --release
--target wasm32-wasip1-threads` in quake-wasm/), lays out a deploy dir (web/PLATFORM.md), serves
it with web/isolated.py's handler on the first free port in 9300-9309, and captures every web
shot the cut uses, at `--scale` times v7's pixels (scale 2: 3840x2160):

    F01   F01-page-load.mp4           the page loads in a browser window, the game starts
    F05b  F05b-phone-touch.mp4        touch play on a phone, rockets into e1m1's grunts
    S18   S18-slop-options-menu.mp4   the game's own menu: Options, Slop Options, its rows

Each clip has `sheets/NAME.png` (a contact sheet) and `sheets/NAME.events.json` (what happens
on which frame) beside it, the names the edit reads. A clip whose inputs (the program, the
page, the pak, this stage's code, the scale and the encoder) are unchanged since it was made
is skipped; `--force` makes it again. Every server and browser this starts is stopped before
it returns.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import webcap  # noqa: E402
import shot_f01  # noqa: E402
import shot_f05b  # noqa: E402
import shot_s18  # noqa: E402

SHOTS = {m.ID: m for m in (shot_f01, shot_f05b, shot_s18)}


@dataclass
class Job:
    scale: int
    deploy: Path
    out: Path
    hw: str
    qp: int
    keep: Path | None

    def clip(self, name: str, size: tuple[int, int]) -> webcap.Clip:
        return webcap.Clip(self.out / f"{name}.mp4", size, self.hw, self.qp, self.keep / name if self.keep else None)


def pick(only: str | None) -> list:
    if not only:
        return list(SHOTS.values())
    out = []
    for want in only.replace(",", " ").split():
        m = SHOTS.get(want) or next((m for m in SHOTS.values() if m.NAME == want or m.NAME == Path(want).stem), None)
        if m is None:
            raise SystemExit(f"no web shot {want!r}: the shots are {', '.join(SHOTS)}")
        if m not in out:
            out.append(m)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scale", type=int, default=1, help="v7's pixels times this (2: 3840x2160, devicePixelRatio doubled)")
    ap.add_argument("--out", type=Path, help="the folder for the clips (the edit's footage/web)")
    ap.add_argument("--only", help="these shots, by id or file name (comma or space separated)")
    ap.add_argument("--hw", choices=("auto", "vaapi", "none"), default="auto",
                    help="vaapi: HEVC on the GPU; none: software H.264 (v7's encode); auto: vaapi when it works")
    ap.add_argument("--qp", type=int, default=16, help="the VAAPI encode's constant QP (lower is better)")
    ap.add_argument("--wasm", type=Path, help="a quake.wasm (threads build) to use instead of building one")
    ap.add_argument("--force", action="store_true", help="make the clips even when their inputs are unchanged")
    ap.add_argument("--keep-frames", type=Path, help="also write every composed frame as a PNG under this folder")
    ap.add_argument("--mem-max", default=os.environ.get("FILM_MEM_MAX", "6G"),
                    help="run inside a systemd user scope with this MemoryMax and no swap, browser and "
                         "encoder included (default $FILM_MEM_MAX or 6G; 'none': no scope)")
    ap.add_argument("--list", action="store_true", help="list the shots and exit")
    a = ap.parse_args()
    if not a.list and a.mem_max != "none" and not os.environ.get("FILM_WEB_SCOPED") and shutil.which("systemd-run"):
        # One memory cap over the whole capture: Chromium at a high devicePixelRatio and a 4K
        # encoder are heavy, and a box that runs out of memory kills whatever it picks.
        os.environ["FILM_WEB_SCOPED"] = a.mem_max
        cmd = ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={a.mem_max}", "-p", "MemorySwapMax=0",
               "--", sys.executable, *sys.argv]
        if subprocess.run(["systemd-run", "--user", "--scope", "-q", "--", "true"], capture_output=True).returncode == 0:
            os.execvp(cmd[0], cmd)
        webcap.log("no systemd user scope here: running without a memory cap")

    if a.list:
        for m in SHOTS.values():
            print(f"{m.ID:5} {m.NAME + '.mp4':28} {m.DESC}")
        return 0
    if a.out is None:
        ap.error("--out is required")
    if a.scale < 1:
        raise SystemExit("--scale is a whole number, 1 or more")
    hw = a.hw
    if hw == "auto":
        hw = "vaapi" if webcap.vaapi_works() else "none"
    elif hw == "vaapi" and not webcap.vaapi_works():
        raise SystemExit(f"--hw vaapi: no working VAAPI HEVC encoder on {webcap.VAAPI_DEVICE} (try --hw none)")
    shots = pick(a.only)
    out = a.out.resolve()
    (out / "sheets").mkdir(parents=True, exist_ok=True)
    stamps = out / ".stamps"
    stamps.mkdir(exist_ok=True)

    wasm = a.wasm.resolve() if a.wasm else webcap.build_wasm(None)
    work = webcap.scratch_dir("capture")
    try:
        deploy = webcap.make_deploy(wasm, work / "deploy")
        page = webcap.tree_digest([deploy])
        code = webcap.tree_digest(sorted(Path(__file__).resolve().parent.glob("*.py")))
        job = Job(a.scale, deploy, out, hw, a.qp, a.keep_frames.resolve() if a.keep_frames else None)
        made = []
        for m in shots:
            stamp = {"shot": m.NAME, "scale": a.scale, "hw": hw, "qp": a.qp if hw == "vaapi" else None,
                     "checks": webcap.vaapi.CHECKS if hw == "vaapi" else None, "page": page, "code": code}
            sfile = stamps / f"{m.NAME}.json"
            mp4 = out / f"{m.NAME}.mp4"
            if not a.force and mp4.exists() and sfile.exists() and json.loads(sfile.read_text()) == stamp:
                webcap.log(f"{m.NAME}: up to date")
                continue
            webcap.log(f"{m.NAME}: capturing at scale {a.scale} ({1920 * a.scale}x{1080 * a.scale}), encoder {hw}")
            t = time.time()
            for attempt in (1, 2):
                try:
                    res = m.run(job)
                    break
                except webcap.WrongEncode as e:
                    raise SystemExit(f"{m.NAME}: {e}")
                except webcap.BadEncode as e:
                    # The capture is deterministic, so a second one gives the same frames.
                    webcap.log(f"{m.NAME}: {e}" + ("; capturing it again" if attempt == 1 else ""))
                    if attempt == 2:
                        raise SystemExit(f"{m.NAME}: two encodes in a row do not decode clean; try --hw none")
            clip = res.pop("clip")
            extra = {"scale": a.scale, "encoder": clip.encoder, **res}
            webcap.write_events(clip, out / "sheets" / f"{m.NAME}.events.json", extra)
            webcap.contact_sheet(mp4, out / "sheets" / f"{m.NAME}.png", a.scale)
            sfile.write_text(json.dumps(stamp, indent=1))
            webcap.log(f"{m.NAME}: wrote {mp4} ({clip.n} frames, {clip.n / webcap.FPS:.2f} s) in {time.time() - t:.0f} s")
            made.append(m.NAME)
        webcap.log("made:", ", ".join(made) or "nothing (all up to date)")
    finally:
        shutil.rmtree(work)
    return 0


if __name__ == "__main__":
    sys.exit(main())
