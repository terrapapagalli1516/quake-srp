"""Turning a diagram's `frame(c, t)` into PNG frames, a video, stills and a contact sheet.

A diagram script ends with `run(frame, duration=..., name=..., cues=CUES)`;
its command line is then:

    uv run diagram.py                 # every frame as PNG, then the mp4 and a contact sheet
    uv run diagram.py --still 2.5 7   # PNG stills at those seconds (quick looks)
    uv run diagram.py --contact       # the contact sheet alone
    uv run diagram.py --from 4 --to 6 # part of it (frames and an mp4 of that part)
    uv run diagram.py --cues cues.json  # retimed: named cues from a JSON object
    uv run diagram.py --duration 4.35 --cues cues.json  # to an edit's slot
    uv run diagram.py --print-cues    # the cue table, as JSON to edit
    uv run diagram.py --alpha         # transparent: RGBA PNGs and a ProRes 4444 .mov, to lay over footage

Frames go to a fresh directory under filmroot's scratch (FILM_SCRATCH), `diagrams/frames/NAME/`,
and are removed once the video is made unless `--keep-frames` (they are the master if another
codec is wanted). The video and its contact sheet go to `--out`, else to FILM_ROOT's
`diagrams/`; stills to `stills/` beside it. A full render takes minutes.
"""

from __future__ import annotations

import argparse
import math
import multiprocessing as mp
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import cairo
import numpy as np
from PIL import Image

from . import look, text
from .canvas import Canvas, new_canvas
from .quake import filmroot


def frames_root() -> Path:
    """Where a render's PNG frames go: FILM_SCRATCH's diagrams/frames/."""
    return filmroot.scratch("diagrams") / "frames"


def default_out_dir() -> Path:
    """Where a video goes without --out: FILM_ROOT's diagrams/."""
    return filmroot.FILM / "diagrams"

_FRAME = None  # the diagram's frame function, inherited by the workers through fork
_SIZE = (look.W, look.H)
_ALPHA = False


def duration_arg(default: float) -> float:
    """The diagram's length: `--duration S` from the command line, else `default`. For a script
    that computes something from its length at import: `DURATION = duration_arg(7.2)`."""
    if "--duration" in sys.argv[:-1]:
        return float(sys.argv[sys.argv.index("--duration") + 1])
    return default


def render_frame(t: float) -> Image.Image:
    """One frame at time t as a Pillow image: RGB, or RGBA with --alpha."""
    w, h = _SIZE
    if _ALPHA:
        c = Canvas(cairo.ImageSurface(cairo.FORMAT_ARGB32, w, h), transparent=True)
    else:
        c = new_canvas(w, h)
    _FRAME(c, t)
    c.surface.flush()
    data, stride = bytes(c.surface.get_data()), c.surface.get_stride()
    if _ALPHA:
        return Image.frombuffer("RGBA", (w, h), data, "raw", "BGRa", stride, 1)
    return Image.frombuffer("RGB", (w, h), data, "raw", "BGRX", stride, 1)


def _worker_init():
    try:
        os.nice(10)
    except OSError:
        pass


def _render_to(job):
    i, t, path = job
    render_frame(t).save(path, compress_level=1)
    return i


def _pool(jobs: int):
    return mp.get_context("fork").Pool(jobs, initializer=_worker_init)


def encode(frames_dir: Path, out: Path, fps: int = look.FPS, crf: int = 16, start_number: int = 0,
           alpha: bool = False) -> None:
    """PNG frames -> H.264 mp4 (yuv420p, BT.709 tagged, CRF ~16), or with alpha a ProRes 4444 .mov."""
    src = ["-framerate", str(fps), "-start_number", str(start_number), "-i", str(frames_dir / "%05d.png")]
    if alpha:
        codec = ["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le", "-vendor", "apl0"]
        vf = "scale=out_color_matrix=bt709:out_range=tv,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
    else:
        codec = ["-c:v", "libx264", "-preset", "slow", "-crf", str(crf), "-pix_fmt", "yuv420p", "-movflags", "+faststart"]
        vf = ("scale=out_color_matrix=bt709:out_range=tv:flags=accurate_rnd+full_chroma_int,"
              "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv")
    cmd = ["ffmpeg", "-y", "-loglevel", "error", *src, "-vf", vf, *codec, str(out)]
    subprocess.run(["nice", "-n", "10", *cmd], check=True)


def _to_rgb(im: Image.Image) -> Image.Image:
    if im.mode == "RGBA":  # transparent frames are shown on the film's background
        bg = Image.new("RGB", im.size, tuple(int(v * 255) for v in look.BG))
        bg.paste(im, (0, 0), im)
        return bg
    return im


def contact_sheet(times: list[float], out: Path, jobs: int, cols: int = 6, title: str = "") -> None:
    """Thumbnails of the given times in a grid, each labelled with its second."""
    with _pool(jobs) as pool:
        imgs = [_to_rgb(im) for im in pool.map(render_frame, times)]
    tw = look.W // cols
    th = tw * look.H // look.W
    rows = math.ceil(len(times) / cols)
    label_h = 28
    head = 40 if title else 0
    sheet = new_canvas(look.W, head + rows * (th + label_h))
    sheet.ctx.set_source_rgb(*look.BG_DEEP)
    sheet.ctx.paint()
    if title:
        sheet.text(title, 12, 8, 3, "gold")
    for k, t in enumerate(times):
        r, col = divmod(k, cols)
        sheet.text(f"{t:.2f} s", col * tw + 6, head + r * (th + label_h) + th + 4, 2, "dim")
    sheet.surface.flush()
    base = Image.frombuffer("RGB", (sheet.w, sheet.h), bytes(sheet.surface.get_data()), "raw", "BGRX",
                            sheet.surface.get_stride(), 1).copy()
    for k, im in enumerate(imgs):
        r, col = divmod(k, cols)
        base.paste(im.resize((tw - 4, th - 4), Image.LANCZOS), (col * tw + 2, head + r * (th + label_h) + 2))
    base.save(out)


def run(frame, duration: float, name: str | None = None, cues=None, fps: int = look.FPS,
        out_dir: str | Path | None = None):
    """The command line of a diagram script (see this module's doc)."""
    global _FRAME, _ALPHA
    script = Path(sys.argv[0]).resolve()
    name = name or script.stem
    doc = (getattr(sys.modules.get("__main__"), "__doc__", None) or "").strip()
    about = f"Renders the diagram {name!r}: {duration:g} s at {fps} fps unless --duration."
    ap = argparse.ArgumentParser(description=f"{doc}\n\n{about}" if doc else about,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--still", type=float, nargs="+", help="render PNG stills at these seconds")
    ap.add_argument("--contact", action="store_true", help="render the contact sheet only")
    ap.add_argument("--from", dest="t0", type=float, default=0.0)
    ap.add_argument("--to", dest="t1", type=float, default=None)
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 4))
    ap.add_argument("--cues", help="a JSON object of cue times that overrides the script's")
    ap.add_argument("--duration", type=float, help="the length in seconds, overriding the script's (an edit's slot)")
    ap.add_argument("--print-cues", action="store_true")
    ap.add_argument("--alpha", action="store_true", help="transparent frames (c.background() draws nothing)")
    ap.add_argument("--out", help="the video (default: FILM_ROOT's diagrams/NAME.mp4, or .mov with --alpha)")
    ap.add_argument("--frames", help="the frame directory (default: a fresh one under FILM_SCRATCH)")
    ap.add_argument("--keep-frames", action="store_true", help="keep the PNG frames after the video is made")
    ap.add_argument("--no-encode", action="store_true")
    ap.add_argument("--crf", type=int, default=16)
    a = ap.parse_args()

    if a.cues:
        cues.load(a.cues)
    if a.duration:
        duration = a.duration
    if a.print_cues:
        print(cues.dump() if cues is not None else "{}")
        return
    _FRAME, _ALPHA = frame, a.alpha
    out_dir = Path(out_dir) if out_dir else default_out_dir()
    stamp = time.strftime("%Y%m%d-%H%M%S")
    suffix = "_alpha" if a.alpha else ""

    if a.still:
        d = (Path(a.out).parent if a.out else out_dir) / "stills"
        d.mkdir(parents=True, exist_ok=True)
        for t in a.still:
            p = d / f"{name}{suffix}_{t:06.2f}.png"
            render_frame(t).save(p)
            print(p)
        return

    sheet_times = [round(x, 2) for x in np.linspace(0.25, duration - 1 / fps, 24)]
    if a.contact:
        p = (Path(a.out).parent if a.out else out_dir) / f"{name}{suffix}_contact.png"
        p.parent.mkdir(parents=True, exist_ok=True)
        contact_sheet(sheet_times, p, a.jobs, title=name.upper())
        print(p)
        return

    t1 = duration if a.t1 is None else min(a.t1, duration)
    whole = a.t0 == 0 and t1 == duration
    first, last = int(round(a.t0 * fps)), int(math.ceil(t1 * fps))
    frames_dir = Path(a.frames) if a.frames else frames_root() / name / (stamp + suffix)
    frames_dir.mkdir(parents=True, exist_ok=False)
    jobs = [(i, i / fps, frames_dir / f"{i:05d}.png") for i in range(first, last)]
    t_start = time.time()
    with _pool(a.jobs) as pool:
        for k, _ in enumerate(pool.imap_unordered(_render_to, jobs, chunksize=4)):
            if k % 120 == 0:
                print(f"  {k}/{len(jobs)} frames", flush=True)
    print(f"{len(jobs)} frames in {time.time() - t_start:.1f}s -> {frames_dir}")
    if a.no_encode:
        return
    if not shutil.which("ffmpeg"):
        raise SystemExit(f"ffmpeg is not on PATH: the frames are in {frames_dir}")
    ext = ".mov" if a.alpha else ".mp4"
    stem = f"{name}{suffix}" if whole else f"{name}{suffix}_{a.t0:g}-{t1:g}"
    out = Path(a.out) if a.out else out_dir / (stem + ext)
    out.parent.mkdir(parents=True, exist_ok=True)
    encode(frames_dir, out, fps, a.crf, start_number=first, alpha=a.alpha)
    print(out)
    if not a.frames and not a.keep_frames:
        shutil.rmtree(frames_dir)
    if whole:
        p = out.parent / f"{name}{suffix}_contact.png"  # beside the video (--out moves both)
        contact_sheet(sheet_times, p, a.jobs, title=name.upper())
        print(p)


__all__ = ["run", "render_frame", "encode", "contact_sheet", "Canvas", "text"]
