#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow", "soundfile", "pyloudnorm", "scipy"]
# ///
"""build.py: the whole film from its parts, in one command.

    uv run film/pipeline/edit/build.py --publish cut        # a new edit/build-NNN/, then edit/cut.*
    uv run film/pipeline/edit/build.py --publish cut --preview --no-burn-in    # the clean master and the phone files
    uv run film/pipeline/edit/build.py --range 113.5-116.5  # a stretch only, as build-NNN/range.mp4
    uv run film/pipeline/edit/build.py --scale 2 --hw vaapi --root OUT --publish cut --publish-to OUT --preview
                                                            # the film at 3840x2160, encoded on the GPU
    uv run film/pipeline/edit/timeline.py --dry             # the clock alone, in a second

film/make.py runs it as the film's last stage but one. A full build takes minutes (the segments, then the
encode); give it a low priority (nice). The config is film/edit.toml; the media are read from FILM_ROOT (or
--root) and the builds written under it (paths.out); caches and intermediates go to FILM_SCRATCH/edit
(film/pipeline/filmroot.py). It needs ffmpeg (libx264; libx265 for --preview; VAAPI for --hw vaapi), cairo,
the Inter font for the cards and a monospace font for the timecode.

--scale N draws the film at N x 1920x1080: edit.toml's positions and sizes stay in 1080's units and are
multiplied by N, the cards and labels are drawn by cairo with an N x transform (Quake's glyphs nearest-
neighbour, so exactly N x), the 4:3 box is N x 1440x1080, and overlay movies drawn at 1080 are scaled up
nearest-neighbour. With N > 1 a 1080p copy (area-averaged from the same segments) is written beside the master,
and the phone files are made from it.

Steps, each from what exists on disk now:
1. **Timeline** (timeline.py): shots.md + the voice's clips + edit.toml -> timeline.json, the edit
   decision list: every shot's frame, length and source (footage, a diagram, an edit card, a
   stand-in, or a placeholder slate), its overlays, and every clip's sample.
2. **Art** (cards.py, in Quake's font through the diagram kit): slates, the edit's cards
   (title, terminal, end card) and overlays (the montage's captions, S28's title, S37's proof
   crawl, S59's rules), as PNG frames. Cached by content.
3. **Segments**: one file per shot, exactly its frame count at 60 fps, the source decoded once
   (in-point, fit to the frame, held on its last frame if short), overlays, fades, a dissolve from
   the shot before if asked. Lossless H.264 at 1080 in software; with --hw vaapi, HEVC at QP 16 on
   the GPU (a 4K film's lossless segments would not fit a disk). Cached by content under the scratch dir.
4. **Audio** (numpy): every narration clip levelled to one loudness and placed on its sample;
   the score (or the theme sketch as a stand-in) placed by section and ducked under the voice,
   phrase by phrase, to sit `balance_lu` below it; each shot's own game sound, if its source
   has one; the master normalised to -14 LUFS and limited under -1 dBTP.
5. **Encode**: the segments concatenated (stream order, frame-exact) and encoded once, H.264
   CRF 17 yuv420p BT.709 in software or HEVC QP 18 on the GPU (the 1080p copy: H.264 QP 18),
   AAC 48 kHz; with --burn-in (default) a timecode, and each real shot's id, are burned in for
   review. The phone files (--preview) are always software x265/x264: under 10 MB, quality per
   bit decides, and the GPU's encoders lose there.
6. **Checks**: the frame count, a contact sheet (a frame every 5 s), and a loudness report
   (integrated, range, true peak; the voice against the score in some voiced passages).

Nothing is deleted: each build is a fresh build-NNN/ and a fresh scratch dir.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import multiprocessing as mp
import os
import re
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
import pyloudnorm as pyln
import soundfile as sf
from scipy import ndimage, signal

EDIT = Path(__file__).resolve().parent
sys.path.insert(0, str(EDIT))
sys.path.insert(0, str(EDIT.parent))
if "--root" in sys.argv[:-1]:  # FILM_ROOT for this build: filmroot reads it once, at import
    os.environ["FILM_ROOT"] = str(Path(sys.argv[sys.argv.index("--root") + 1]).resolve())

import cards  # noqa: E402
import timeline  # noqa: E402
from filmroot import FILM, PIPELINE, scratch  # noqa: E402

SCRATCH = scratch("edit")
CACHE = SCRATCH / "cache"
QKIT = PIPELINE / "diagrams" / "qkit"
FPS, SR, W, H = 60, 48000, 1920, 1080   # W, H: the edit's own units (edit.toml's coordinates are 1080's)
SPF = SR // FPS  # samples per frame: 800
S = 1            # the frame is S x W by S x H (--scale): every size and position below is multiplied by it
HW = "none"      # --hw: "vaapi" encodes on the GPU (the segments, the master, the 1080p copy), "none" in software
VAAPI_DEVICE = os.environ.get("VAAPI_DEVICE", "/dev/dri/renderD128")
SEG_QP = 16      # the segments' constant QP on the GPU: a high-quality intermediate (lossless 4K would not fit the disk)
MASTER_QP = 18   # the master's


def mono_font() -> str:
    """The burned-in timecode's font: JetBrains Mono if fontconfig has it, else its monospace default."""
    for name in ("JetBrains Mono", "monospace"):
        r = subprocess.run(["fc-match", "-f", "%{file}", name], capture_output=True, text=True)
        if r.returncode == 0 and r.stdout and Path(r.stdout).exists():
            return r.stdout
    raise SystemExit("build.py: no monospace font for the timecode (fontconfig's fc-match found none)")
SEG_VERSION = "seg-v4"  # bump when the segment graph changes, so the cache renews


def log(*a) -> None:
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def sh(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode:
        raise RuntimeError(f"{cmd[0]} failed ({r.returncode}):\n{' '.join(cmd)}\n{r.stderr[-3000:]}")
    return r


def digest(*parts) -> str:
    h = hashlib.sha1()
    for p in parts:
        h.update(json.dumps(p, sort_keys=True, default=str).encode())
    return h.hexdigest()[:16]


def file_sig(p: Path) -> list:
    st = p.stat()
    return [str(p), st.st_size, int(st.st_mtime)]


CODE_SIG = digest(*(Path(p).read_text() for p in [
    EDIT / "cards.py", EDIT / "qglyph.py", *sorted(QKIT.glob("*.py"))]))


# =================================================================== art ====

def _render_job(job):
    kind, name, shot, t_or_k, path, extra = job
    alpha = kind in ("overlay", "caption", "tag", "bumper")
    c = cards.new(alpha)
    if kind == "slate":
        cards.slate(c, shot, extra.get("section", ""), quiet=extra.get("quiet", False))
    elif kind == "card":
        cards.CARDS[name](c, t_or_k, shot)
    elif kind == "overlay":
        cards.OVERLAYS[name](c, t_or_k, shot | {"ov": extra.get("ov", {})})
    elif kind == "caption":
        cards.ov_caption(c, t_or_k, extra["text"], extra.get("sub"))
    elif kind == "bumper":
        cards.ov_bumper(c, t_or_k, extra["text"], extra)
    elif kind == "tag":
        cards.tag(c, extra["text"], extra.get("color", "rust"))
    cards.save(c, Path(path))
    return path


def plan_art(tl: dict, burn_in: bool) -> tuple[dict, list]:
    """What each shot needs drawn, cached by content: {shot id: {...paths}}, and the frame jobs to run."""
    secs = {s["id"]: s["title"] for s in tl["sections"]}
    art, jobs = {}, []
    for s in tl["shots"]:
        a = {"overlays": []}
        src = s["source"]
        real = src["status"] in ("footage", "final")
        ovs = [o for o in s["overlays"] if o.get("when", "always") == "always" or real]
        has_card_overlay = any(o["type"] in ("card", "video") for o in ovs)
        if src["type"] == "slate":
            key = digest("slate", CODE_SIG, {k: s[k] for k in ("id", "kind", "extra", "desc", "notice", "over", "len",
                                                                "start", "planned_start", "planned_len", "section")},
                         has_card_overlay)
            p = CACHE / "art" / f"slate-{s['id']}-{key}.png"
            a["slate"] = p
            if not p.exists():
                jobs.append(("slate", None, s, 0.0, str(p), {"section": secs.get(s["section"], ""),
                                                               "quiet": has_card_overlay}))
        elif src["type"] == "card":
            key = digest("card", CODE_SIG, src["name"], s["frames"], s["cues"], s.get("card_opts"), s["start"],
                         file_sig(FILM / cards.S23_LAST[0]) if src["name"] in ("push", "still_rings") else None)
            d = CACHE / "art" / f"card-{s['id']}-{key}"
            a["card"] = d
            if not (d / "done").exists():
                d.mkdir(parents=True, exist_ok=True)
                for f in range(s["frames"]):
                    jobs.append(("card", src["name"], s, f / FPS, str(d / f"{f:05d}.png"), {}))
        for o in ovs:
            if o["type"] == "png":
                a["overlays"].append({"png_at": FILM / o["path"], "at": float(o.get("at", 0.0))})
            elif o["type"] == "pillars":  # the 4:3 box's black edges sliding out (open) or in (close), eased out
                a["overlays"].append({"pillars": {"at": float(o["at"]), "dur": float(o.get("dur", 0.4)),
                                                  "mode": o.get("mode", "open")}})
            elif o["type"] == "diff_tint":
                d = {k: o[k] for k in o if k != "type"}
                if "from_end" in d:
                    d["from"] = max(0.0, s["len"] - float(d["from_end"]))
                a["overlays"].append({"diff_tint": d})
            elif o["type"] == "video":
                a["overlays"].append({"video": FILM / o["path"], "x": int(o.get("x", 0)), "y": int(o.get("y", 0)),
                                      "scale": float(o.get("scale", 1.0)), "in": float(o.get("in", 0.0)),
                                      "fade_in_at": o.get("fade_in_at"), "fade": float(o.get("fade", 0.4)),
                                      "enable": o.get("enable"), "panel": o.get("panel"), "at": o.get("at")})
            elif o["type"] == "card" and o["name"] in cards.STATIC_OVERLAYS:
                key = digest("ovs", CODE_SIG, o["name"], {k: v for k, v in o.items() if k != "at"})
                p = CACHE / "art" / f"ovs-{s['id']}-{o['name']}-{key}.png"
                a["overlays"].append({"png_at": p, "at": float(o.get("at") or 0.0), "until": o.get("until")}
                                     if (o.get("at") or o.get("until")) else {"png": p})
                if not p.exists():
                    jobs.append(("overlay", o["name"], s, 0.0, str(p), {"ov": o}))
            elif o["type"] == "card":
                key = digest("ov", CODE_SIG, o["name"], s["frames"], s["cues"], o)
                d = CACHE / "art" / f"ov-{s['id']}-{o['name']}-{key}"
                a["overlays"].append({"seq": d})
                if not (d / "done").exists():
                    d.mkdir(parents=True, exist_ok=True)
                    for f in range(s["frames"]):
                        jobs.append(("overlay", o["name"], s, f / FPS, str(d / f"{f:05d}.png"), {"ov": o}))
            elif o["type"] == "bumper":
                key = digest("bump", CODE_SIG, o)
                d = CACHE / "art" / f"bump-{s['id']}-{key}"
                a["overlays"].append({"seq": d, "at": float(o.get("at", 0.0))})
                if not (d / "done").exists():
                    d.mkdir(parents=True, exist_ok=True)
                    nf = int(float(o.get("hold", 1.5)) * FPS) + 1  # the last frame is empty, and holds
                    for f in range(nf):
                        jobs.append(("bumper", None, s, f / FPS, str(d / f"{f:05d}.png"), o))
            elif o["type"] == "caption":
                key = digest("cap", CODE_SIG, o["text"], o.get("sub"), o.get("typed", True))
                d = CACHE / "art" / f"cap-{s['id']}-{key}"
                a["overlays"].append({"seq": d, "at": float(o.get("at", 0.0))})
                if not (d / "done").exists():
                    d.mkdir(parents=True, exist_ok=True)
                    for k in range(6):  # typed on in 6 frames; the last one holds (`typed = false`: carried on, whole)
                        jobs.append(("caption", None, s, (k + 1) if o.get("typed", True) else 6, str(d / f"{k:05d}.png"),
                                     {"text": o["text"], "sub": o.get("sub")}))
        if src["status"] == "standin":
            text = f"stand-in · {Path(src['path']).name} · {s['id']}"
            key = digest("tag", CODE_SIG, text)
            p = CACHE / "art" / f"tag-{key}.png"
            a["overlays"].append({"png": p})
            if not p.exists():
                jobs.append(("tag", None, s, 0, str(p), {"text": text, "color": "yellow"}))
        elif burn_in and src["type"] == "video":
            key = digest("tag", CODE_SIG, s["id"])
            p = CACHE / "art" / f"tag-{key}.png"
            a["overlays"].append({"png": p})
            if not p.exists():
                jobs.append(("tag", None, s, 0, str(p), {"text": s["id"], "color": "dim"}))
        art[s["id"]] = a
    return art, jobs


def render_art(jobs: list, procs: int) -> None:
    if not jobs:
        return
    (CACHE / "art").mkdir(parents=True, exist_ok=True)
    log(f"art: {len(jobs)} frames")
    with mp.get_context("fork").Pool(procs) as pool:
        for k, _ in enumerate(pool.imap_unordered(_render_job, jobs, chunksize=8)):
            if k and k % 500 == 0:
                log(f"  art {k}/{len(jobs)}")
    for j in jobs:  # mark finished sequences
        if j[0] in ("card", "overlay", "caption", "bumper") and not j[4].endswith(f"{Path(j[4]).parent.name}.png"):
            if Path(j[4]).parent.name.startswith(("card-", "ov-", "cap-", "bump-")):
                (Path(j[4]).parent / "done").touch()


# ============================================================== segments ====

_OVW: dict = {}


def overlay_upscale(path) -> int:
    """How much a full-frame overlay movie must be scaled to fill this frame: 1 when it was rendered at this
    frame's size (the diagrams stage at --scale), S when it is a 1080 render (media brought in for a test)."""
    if path not in _OVW:
        _OVW[path] = (timeline.probe(Path(path)).get("width") or W * S)
    w = _OVW[path]
    return S if (S > 1 and w == W) else 1

def source_chain(idx: int, src: dict, length: float, start_at: float | None = None) -> str:
    """Filters that turn input idx into the frame (S x 1920x1080) in gbrp at 60 fps, from the in-point, held at
    its end. Classic's 960x600 shows 4:3 in the box (S x 1440x1080, S x 240 px bars), scaled as v7 scaled it."""
    if src["type"] == "video":
        f = "setpts=PTS-STARTPTS"
        sp = float(src.get("speed", 1.0))
        if src.get("reverse"):  # played backwards: take the span it covers, then turn it round
            f += f",trim=end={length * sp + 0.05:.5f},setpts=PTS-STARTPTS,reverse"
        if sp != 1.0:  # faster (> 1) or slower (< 1) than it was rendered
            f += f",setpts=PTS/{sp:.6f}"
        if src.get("box"):
            f += f",fps=60,scale={1440 * S}:{1080 * S}:flags=bicubic,pad={1920 * S}:{1080 * S}:{240 * S}:0:color=black," \
                 "setsar=1,format=gbrp"
        else:
            f += f",fps=60,scale={1920 * S}:{1080 * S}:force_original_aspect_ratio=decrease:flags=bicubic," \
                 f"pad={1920 * S}:{1080 * S}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=gbrp"
        if src.get("lift"):  # a display lift, as id's Brightness slider: pow(x, gamma), alike on every channel
            gm = float(src["lift"])
            f += f",lutrgb=r='255*pow(val/255,{gm})':g='255*pow(val/255,{gm})':b='255*pow(val/255,{gm})'"
        if src.get("mask43"):  # a 16:9 picture shown in the box phase: its sides masked to the 4:3 box (a stopgap)
            f += (f",drawbox=x=0:y=0:w={240 * S}:h={1080 * S}:color=black:t=fill,"
                  f"drawbox=x={1680 * S}:y=0:w={240 * S}:h={1080 * S}:color=black:t=fill")
        span = None
        if src.get("out") is not None:
            span = src["out"] - (start_at if start_at is not None else src["in"])
        if span is not None and span > 0:
            f += f",trim=end={span:.5f}"
        f += f",tpad=stop_mode=clone:stop_duration={length + 1:.3f},settb=1/60"
        return f"[{idx}:v]{f}"
    return f"[{idx}:v]scale={1920 * S}:{1080 * S},format=gbrp,setsar=1,settb=1/60"


def video_input(src: dict, at: float) -> list[str]:
    p = src.get("frozen") or str(FILM / src["path"])
    dur = (src.get("probe") or {}).get("duration") or 0.0
    at = min(at, max(0.0, dur - 2 / FPS)) if dur else at
    return (["-ss", f"{at:.5f}"] if at > 0 else []) + ["-i", p]


def segment_spec(s: dict, prev: dict | None, art: dict, burn_in: bool) -> dict:
    src = s["source"]
    spec = {"v": SEG_VERSION, "scale": S, "codec": segment_codec(),
            "id": s["id"], "frames": s["frames"], "fade_in": s["fade_in"], "fade_out": s["fade_out"],
            "dim": s["dim"], "burn_in": burn_in, "slate_len": s["len"] if src["type"] == "slate" else None}
    if src["type"] == "burst":
        spec["src"] = {k: src.get(k) for k in ("a", "b", "in", "at", "dur")} | {
            "sig": [file_sig(FILM / src["a"]), file_sig(FILM / src["b"])]}
    elif src["type"] == "video":
        spec["src"] = {k: src.get(k) for k in ("path", "in", "out", "box", "speed", "reverse")} | {"sig": file_sig(FILM / src["path"])}
        if src.get("mask43"):
            spec["src"]["mask43"] = True
        if src.get("lift"):
            spec["src"]["lift"] = src["lift"]
    elif src["type"] == "slate":
        spec["src"] = {"slate": str(art["slate"])}
    else:
        spec["src"] = {"card": str(art["card"])}
    spec["overlays"] = [o["pillars"] if "pillars" in o else o["diff_tint"] if "diff_tint" in o else
                        [file_sig(o["video"]), {k: v for k, v in o.items() if k != "video"}] if "video" in o else
                        [file_sig(o["png_at"]), o["at"]] if "png_at" in o else str(o.get("seq") or o.get("png"))
                        for o in art["overlays"]]
    tr = s.get("transition_in")
    if tr and prev is not None:
        spec["transition"] = {"type": tr.get("type", "fade"), "dur": tr.get("dur", 0.5),
                              "prev": prev["source"].get("path") or prev["id"], "prev_in": prev["source"].get("in"),
                              "prev_len": prev["len"]}
    return spec


def segment_cmd(s: dict, prev: dict | None, prev_art: dict | None, art: dict, out: Path) -> list[str]:
    src = s["source"]
    n, length = s["frames"], s["frames"] / FPS
    inputs: list[list[str]] = []
    graph: list[str] = []

    def add(args: list[str]) -> int:
        inputs.append(args)
        return len(inputs) - 1

    if src["type"] == "burst":
        T, D = src["at"], src["dur"]
        ia = add(video_input({"path": src["a"], "probe": src.get("a_probe"), "frozen": src.get("frozen_a")}, src["in"]))
        ib = add(video_input({"path": src["b"], "probe": src.get("probe"), "frozen": src.get("frozen")}, src["in"]))
        ease = f"(1-pow(1-clip((t-{T:.4f})/{D:.4f},0,1),3))"
        graph.append(f"[{ib}:v]setpts=PTS-STARTPTS,fps=60,scale={1920 * S}:{1080 * S}:flags=bicubic,setsar=1,format=gbrp,"
                     f"tpad=stop_mode=clone:stop_duration={length + 1:.3f},settb=1/60[bb]")
        ap_ = src.get("a_probe") or {}
        boxed = ap_.get("width") and ap_.get("height") and abs(ap_["width"] / ap_["height"] - 16 / 9) < 0.01
        crop_a = "crop=ih*4/3:ih:(iw-ih*4/3)/2:0," if boxed else ""  # (a) comes boxed in a 16:9 frame
        graph.append(f"[{ia}:v]setpts=PTS-STARTPTS,fps=60,{crop_a}scale={1440 * S}:{1080 * S}:flags=bicubic,setsar=1,"
                     f"format=gbrap,tpad=stop_mode=clone:stop_duration={length + 1:.3f},settb=1/60,"
                     f"scale=w='2*trunc(({1440 * S}+{480 * S}*{ease})/2)':h={1080 * S}:eval=frame,"
                     f"fade=t=out:st={T:.4f}:d={D:.4f}:alpha=1[ba]")
        k1 = add(["-f", "lavfi", "-i", f"color=c=black:s={240 * S}x{1080 * S}:r=60"])
        k2 = add(["-f", "lavfi", "-i", f"color=c=black:s={240 * S}x{1080 * S}:r=60"])
        graph.append(f"[bb][{k1}:v]overlay=x='-{240 * S}*{ease}':y=0:eval=frame:format=gbrp[bl]")
        graph.append(f"[bl][{k2}:v]overlay=x='{1680 * S}+{240 * S}*{ease}':y=0:eval=frame:format=gbrp[br]")
        graph.append(f"[br][ba]overlay=x='(W-w)/2':y=0:eval=frame:format=gbrp:eof_action=repeat[b0]")
    elif src["type"] == "video":
        i = add(video_input(src, src["in"]))
        graph.append(source_chain(i, src, length) + "[b0]")
    elif src["type"] == "slate":
        i = add(["-loop", "1", "-framerate", "60", "-i", str(art["slate"])])
        graph.append(source_chain(i, src, length) + "[s0]")
        j = add(["-f", "lavfi", "-i", f"color=c=0xAF632F:s={1920 * S}x{6 * S}:r=60"])
        graph.append(f"[{j}:v]format=gbrp[bar]")
        graph.append(f"[s0][bar]overlay=x='-w+w*t/{length:.4f}':y=H-{6 * S}:eval=frame:format=gbrp[b0]")
    else:
        i = add(["-framerate", "60", "-i", str(art["card"] / "%05d.png")])
        graph.append(source_chain(i, src, length) + ",tpad=stop_mode=clone:stop_duration=1[b0]")
    cur = "b0"
    if s["dim"] < 1.0:
        d = s["dim"]
        graph.append(f"[{cur}]colorchannelmixer=rr={d}:gg={d}:bb={d}[dm]")
        cur = "dm"
    tr = s.get("transition_in")
    if tr and prev is not None:
        dur = float(tr.get("dur", 0.5))
        ps = prev["source"]
        if ps["type"] == "video":
            at = ps["in"] + prev["len"]
            k = add(video_input(ps, at))
            graph.append(source_chain(k, ps, dur + 0.2, start_at=at) + "[pa]")
        elif ps["type"] == "slate":
            k = add(["-loop", "1", "-framerate", "60", "-i", str(prev_art["slate"])])
            graph.append(source_chain(k, ps, dur) + "[pa]")
        else:
            last = sorted(prev_art["card"].glob("*.png"))[-1]
            k = add(["-loop", "1", "-framerate", "60", "-i", str(last)])
            graph.append(source_chain(k, ps, dur) + "[pa]")
        if prev.get("dim", 1.0) < 1.0:
            d = prev["dim"]
            graph.append(f"[pa]colorchannelmixer=rr={d}:gg={d}:bb={d}[pad]")
            graph.append(f"[pad]trim=end={dur + 0.1:.4f}[pt]")
        else:
            graph.append(f"[pa]trim=end={dur + 0.1:.4f}[pt]")
        kind = {"fade": "fade", "dissolve": "fade", "wipe": "wipeleft"}.get(tr.get("type", "fade"), tr.get("type"))
        graph.append(f"[pt][{cur}]xfade=transition={kind}:duration={dur:.4f}:offset=0[xf]")
        cur = "xf"
    for m, o in enumerate(art["overlays"]):
        if "pillars" in o:
            pT, pD, mode = o["pillars"]["at"], o["pillars"]["dur"], o["pillars"]["mode"]
            u = f"(1-pow(1-clip((t-{pT:.4f})/{pD:.4f},0,1),3))"
            off = u if mode == "open" else f"(1-{u})"
            en = f"between(t,{pT:.4f},{pT + pD:.4f})" if mode == "open" else f"gte(t,{pT:.4f})"
            k1 = add(["-f", "lavfi", "-i", f"color=c=black:s={240 * S}x{1080 * S}:r=60"])
            k2 = add(["-f", "lavfi", "-i", f"color=c=black:s={240 * S}x{1080 * S}:r=60"])
            graph.append(f"[{cur}][{k1}:v]overlay=x='-{240 * S}*{off}':y=0:eval=frame:format=gbrp:enable='{en}'[pl{m}]")
            graph.append(f"[pl{m}][{k2}:v]overlay=x='{1680 * S}+{240 * S}*{off}':y=0:eval=frame:format=gbrp:"
                         f"enable='{en}'[v{m}]")
            cur = f"v{m}"
            continue
        if "diff_tint" in o:  # the pixels where two panels of one frame differ, tinted on the first (S33)
            d = o["diff_tint"]
            ax, ay, w, h = (int(v) * S for v in d["a"])
            bx, by = (int(v) * S for v in d["b"][:2])
            thr, top = int(d.get("threshold", 24)), int(d.get("skip_top", 0)) * S
            k = add(["-f", "lavfi", "-i", f"color=c={d.get('color', '0xDB7F3B')}:s={w}x{h}:r=60"])
            graph.append(f"[{cur}]split=3[dt0_{m}][dt1_{m}][dt2_{m}]")
            graph.append(f"[dt1_{m}]crop={w}:{h}:{ax}:{ay},format=gray[dga_{m}]")
            graph.append(f"[dt2_{m}]crop={w}:{h}:{bx}:{by},format=gray[dgb_{m}]")
            graph.append(f"[dga_{m}][dgb_{m}]blend=all_mode=difference,geq=lum='if(gt(lum(X,Y),{thr})*gt(Y,{top}),255,0)'[dm_{m}]")
            graph.append(f"[{k}:v]format=gbrap[dc_{m}]")
            graph.append(f"[dc_{m}][dm_{m}]alphamerge,colorchannelmixer=aa={float(d.get('alpha', 0.6))}[dtint_{m}]")
            graph.append(f"[dt0_{m}][dtint_{m}]overlay={ax}:{ay}:format=gbrp:shortest=0:enable='gte(t,{float(d.get('from', 0)):.3f})'[v{m}]")
            cur = f"v{m}"
            continue
        if "png_at" in o:
            k = add(["-loop", "1", "-framerate", "60", "-i", str(o["png_at"])])
            graph.append(f"[{k}:v]format=gbrap,fade=t=in:st={o['at']:.3f}:d=0.15:alpha=1,settb=1/60[o{m}]")
            en = f":enable='lt(t,{float(o['until']):.4f})'" if o.get("until") is not None else ""
            graph.append(f"[{cur}][o{m}]overlay=0:0:format=gbrp{en}[v{m}]")
            cur = f"v{m}"
            continue
        if "video" in o:
            k = add((["-ss", f"{o['in']:.5f}"] if o.get("in") else []) + ["-i", str(o["video"])])
            f = f"[{k}:v]setpts=PTS-STARTPTS,fps=60,format=gbrap"
            if o.get("at"):  # it starts later in the shot
                f += f",setpts=PTS+{int(round(float(o['at']) * FPS))}"
            up = overlay_upscale(o["video"])  # a full-frame render made for a smaller frame (1080 media in a 4K build)
            if up != 1:
                f += f",scale=iw*{up}:ih*{up}:flags=neighbor"
            if o.get("scale", 1.0) != 1.0:
                f += f",scale=iw*{o['scale']}:ih*{o['scale']}:flags=area"
            if o.get("fade_in_at") is not None:  # an opaque clip, faded in over the shot (S62's clean end)
                f += f",fade=t=in:st={o['fade_in_at']:.3f}:d={o['fade']:.3f}:alpha=1"
            en = f":enable='between(t,{o['enable'][0]:.4f},{o['enable'][1] - 0.5 / FPS:.4f})'" if o.get("enable") else ""
            if o.get("panel"):  # move a diagram's inset elsewhere, following its own moves to and from the full screen
                (px, py, ps), (fa, fb, fd) = o["panel"]["to"], o["panel"]["full"]
                ease = "if(lt({u},0.5),4*{u}*{u}*{u},1-pow(-2*{u}+2,3)/2)"
                ui, uo = f"clip((t-{fa})/{fd},0,1)", f"clip((t-{fb - fd})/{fd},0,1)"
                mm = f"({ease.format(u=ui)})*(1-({ease.format(u=uo)}))"
                sc = f"({ps}+(1-{ps})*{mm})"
                f += f",scale=w='2*trunc({960 * S}*{sc})':h='2*trunc({540 * S}*{sc})':eval=frame"
                graph.append(f + f",settb=1/60[o{m}]")
                graph.append(f"[{cur}][o{m}]overlay=x='{px * S}*(1-{mm})':y='{py * S}*(1-{mm})':eval=frame:eof_action=repeat:"
                             f"format=gbrp{en}[v{m}]")
                cur = f"v{m}"
                continue
            graph.append(f + f",settb=1/60[o{m}]")
            graph.append(f"[{cur}][o{m}]overlay={int(o.get('x', 0)) * S}:{int(o.get('y', 0)) * S}:eof_action=repeat:"
                         f"format=gbrp{en}[v{m}]")
            cur = f"v{m}"
            continue
        if "seq" in o:
            k = add(["-framerate", "60", "-i", str(o["seq"] / "%05d.png")])
            if o.get("at"):
                graph.append(f"[{k}:v]format=gbrap,settb=1/60,setpts=PTS+{int(round(o['at'] * FPS))}[o{m}]")
                graph.append(f"[{cur}][o{m}]overlay=0:0:eof_action=repeat:format=gbrp[v{m}]")
                cur = f"v{m}"
                continue
        else:
            k = add(["-loop", "1", "-framerate", "60", "-i", str(o["png"])])
        graph.append(f"[{k}:v]format=gbrap,settb=1/60[o{m}]")
        graph.append(f"[{cur}][o{m}]overlay=0:0:eof_action=repeat:format=gbrp[v{m}]")
        cur = f"v{m}"
    tail = []
    if s["fade_in"] > 0:
        tail.append(f"fade=t=in:st=0:d={s['fade_in']:.3f}")
    if s["fade_out"] > 0:
        tail.append(f"fade=t=out:st={length - s['fade_out']:.4f}:d={s['fade_out']:.3f}")
    tail += [f"trim=end_frame={n}", "setpts=PTS-STARTPTS",
             "scale=out_color_matrix=bt709:out_range=tv:flags=accurate_rnd+full_chroma_int", "format=yuv420p"]
    if HW == "vaapi":
        tail += ["format=nv12", "hwupload"]
    graph.append(f"[{cur}]" + ",".join(tail) + "[vout]")
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-threads", "4"]
    if HW == "vaapi":
        cmd += ["-vaapi_device", VAAPI_DEVICE]
    for a in inputs:
        cmd += a
    cmd += ["-filter_complex", ";".join(graph), "-map", "[vout]", "-frames:v", str(n), "-an", *segment_codec(),
            "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv",
            "-r", "60", "-video_track_timescale", "15360", str(out)]
    return cmd


def segment_codec() -> list[str]:
    """The intermediate segments' codec. At 1080 in software: lossless H.264, as v7 was built. On the GPU (--hw
    vaapi): HEVC at a low constant QP (SEG_QP), visually lossless and a small fraction of a lossless 4K file's size.
    In software above 1080: H.264 at CRF 10, for the same reason."""
    if HW == "vaapi":
        return ["-c:v", "hevc_vaapi", "-rc_mode", "CQP", "-qp", str(SEG_QP), "-profile:v", "main"]
    if S == 1:
        return ["-c:v", "libx264", "-qp", "0", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]
    return ["-c:v", "libx264", "-crf", "10", "-preset", "veryfast", "-pix_fmt", "yuv420p"]


def count_frames(p: Path) -> int:
    r = sh(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
            "stream=nb_read_packets", "-of", "csv=p=0", str(p)])
    return int(r.stdout.strip() or 0)


def segment_paths(tl: dict, art: dict, burn_in: bool) -> list[Path]:
    """Each shot's segment file (cached by its content): the picture's identity, without rendering anything."""
    shots = tl["shots"]
    return [CACHE / "seg" / f"{s['id']}-{digest(segment_spec(s, shots[i - 1] if i else None, art[s['id']], burn_in))}.mp4"
            for i, s in enumerate(shots)]


def render_segments(tl: dict, art: dict, burn_in: bool, jobs: int, only: set | None = None) -> list[Path]:
    (CACHE / "seg").mkdir(parents=True, exist_ok=True)
    shots = tl["shots"]
    todo, paths = [], []
    for i, s in enumerate(shots):
        if only is not None and s["id"] not in only:
            continue
        prev = shots[i - 1] if i else None
        spec = segment_spec(s, prev, art[s["id"]], burn_in)
        p = CACHE / "seg" / f"{s['id']}-{digest(spec)}.mp4"
        paths.append(p)
        if not p.exists():
            todo.append((s, prev, art.get(prev["id"]) if prev else None, art[s["id"]], p))
    log(f"segments: {len(shots)} shots, {len(todo)} to render, {len(shots) - len(todo)} cached")

    def one(job):
        s, prev, prev_art, a, p = job
        tmp = p.with_suffix(".part.mp4")
        sh(segment_cmd(s, prev, prev_art, a, tmp))
        got = count_frames(tmp)
        if got != s["frames"]:
            raise RuntimeError(f"{s['id']}: {got} frames, want {s['frames']}")
        tmp.rename(p)
        return s["id"]

    with ThreadPoolExecutor(jobs) as ex:
        for k, sid in enumerate(ex.map(one, todo)):
            if k % 10 == 0:
                log(f"  segment {sid} ({k + 1}/{len(todo)})")
    return paths


# ================================================================= audio ====

def lufs(meter: pyln.Meter, x: np.ndarray) -> float:
    if len(x) < int(0.45 * SR):
        x = np.concatenate([x, np.zeros((int(0.45 * SR) - len(x),) + x.shape[1:])])
    with np.errstate(divide="ignore"):
        v = meter.integrated_loudness(x)
    return float(v) if np.isfinite(v) else -120.0


def eq_power(n: int, rising: bool) -> np.ndarray:
    u = np.linspace(0, 1, n, endpoint=False) if n else np.zeros(0)
    w = np.sin(u * np.pi / 2)
    return w if rising else w[::-1]


def read_stereo(p: Path) -> np.ndarray:
    x, sr = sf.read(str(p), dtype="float64", always_2d=True)
    if sr != SR:
        x = signal.resample_poly(x, SR, sr, axis=0)
    if x.shape[1] == 1:
        x = np.repeat(x, 2, axis=1)
    return x[:, :2]


def loop_fill(src: np.ndarray, n: int, xf: int, end: bool = False) -> np.ndarray:
    """src repeated to n samples, each restart crossfaded over xf samples; with end=True the last
    sample of the result is src's last (the loop is laid out backwards from the end)."""
    if len(src) >= n:
        return (src[len(src) - n:] if end else src[:n]).copy()
    if end:
        copies = math.ceil((n - xf) / (len(src) - xf))
        full = loop_fill(src, copies * (len(src) - xf) + xf, xf)
        return full[len(full) - n:].copy()
    out = np.zeros((n, src.shape[1]))
    pos = 0
    first = True
    while pos < n:
        piece = src.copy()
        if not first and xf:
            piece[:xf] *= eq_power(xf, True)[:, None]
        piece[-xf:] *= eq_power(xf, False)[:, None] if xf else 1
        m = min(len(piece), n - pos)
        out[pos:pos + m] += piece[:m]
        pos += len(piece) - xf
        first = False
    return out


def conform_score(tl: dict, n: int, m: dict) -> tuple[np.ndarray, str]:
    """The score, cut to the edit as the picture was: each shot takes the score from that shot's start on
    the clock the score was composed on, so a shot trimmed at its tail cuts the score there too. Where two
    shots' music is not contiguous on that clock, the incoming one fades in over `conform_xfade` before the
    cut and the outgoing one fades out over the same after it (equal power): a hit on the cut stays whole."""
    x = read_stereo(FILM / m["path"])
    clock = m.get("clock") or "planned"
    if clock == "planned":
        old = {s["id"]: int(round(s["planned_start"] * SR)) for s in tl["shots"]}
    else:  # by frame: the timeline's seconds are rounded, and a false cut would crossfade the score with itself
        old = {s["id"]: s["frame"] * SPF for s in json.loads((FILM / clock).read_text())["shots"]}
    runs = []  # [new start, new end, old start] in samples, contiguous on the score's clock
    for s in tl["shots"]:
        n0, n1 = s["frame"] * SPF, (s["frame"] + s["frames"]) * SPF
        o0 = old[s["id"]] if s["id"] in old else (runs[-1][2] + runs[-1][1] - runs[-1][0] if runs else 0)
        if runs and abs((runs[-1][2] + (n0 - runs[-1][0])) - o0) <= 1:
            runs[-1][1] = n1
        else:
            runs.append([n0, n1, o0])
    xf = int(m.get("xfade_conform", 0.25) * SR)
    out = np.zeros((n, 2))
    for k, (n0, n1, o0) in enumerate(runs):
        pre = xf if k > 0 else 0
        post = xf if k < len(runs) - 1 else 0
        lo, hi = n0 - pre, min(n, n1 + post)
        a = o0 - pre
        seg = np.zeros((hi - lo, 2))
        src_lo, src_hi = max(0, a), min(len(x), a + (hi - lo))
        if src_hi > src_lo:
            seg[src_lo - a: src_hi - a] = x[src_lo:src_hi]
        if pre:
            seg[:pre] *= eq_power(pre, True)[:, None]
        if post and hi == n1 + post:
            seg[-post:] *= eq_power(post, False)[:, None]
        out[lo:hi] += seg
    cuts = len(runs) - 1
    m["_runs"] = runs
    return out, (f"score {m['path']} ({len(x) / SR:.2f} s, composed on {clock}), conformed to the edit: "
                 f"{cuts} cut{'s' if cuts != 1 else ''} where shots changed length")


def music_track(tl: dict, n: int) -> tuple[np.ndarray, str]:
    m = tl["music"]
    out = np.zeros((n, 2))
    if m["mode"] == "score":
        return conform_score(tl, n, m)
    if m["mode"] == "none":
        return out, "no score yet (the animatic carries the voice, the game sound and the effects stem)"
    x = read_stereo(FILM / m["path"])
    xf = int(m.get("xfade", 1.0) * SR)
    regions = m["regions"]
    for k, r in enumerate(regions):
        prev_exact = k > 0 and regions[k - 1]["exact"]
        a = int(round(r["start"] * SR))
        b = int(round(r["end"] * SR))
        cut_in = r["exact"] or prev_exact  # a hard cut on the downbeat (into or out of the montage)
        lo = a if (cut_in or k == 0) else a - xf // 2
        nxt_exact = k + 1 < len(regions) and regions[k + 1]["exact"]
        hi = b if (r["exact"] or nxt_exact or k == len(regions) - 1) else b + xf // 2
        lo, hi = max(0, lo), min(n, hi)
        si, so = int(r["in"] * SR), int(r["out"] * SR)
        src = x[si:so]
        length = hi - lo
        if r["align"] == "end":
            seg = loop_fill(src, length, int(0.5 * SR), end=True)
        else:
            seg = loop_fill(src, length, int(0.005 * SR) if r["exact"] else int(0.5 * SR))
        fin = int(0.02 * SR) if cut_in else (int(2.0 * SR) if k == 0 else xf)
        fout = int(0.02 * SR) if (r["exact"] or nxt_exact) else (0 if k == len(regions) - 1 else xf)
        if fin:
            seg[:fin] *= eq_power(fin, True)[:, None]
        if fout:
            seg[-fout:] *= eq_power(fout, False)[:, None]
        out[lo:hi] += seg
    return out, f"stand-in: {m['path']} by section (edit.toml, [music.standin_map])"


def phrases(tl: dict, hold: float) -> list[list[float]]:
    sp = sorted(v["speech"] for v in tl["voice"])
    out = []
    for a, b in sp:
        if out and a - out[-1][1] < hold:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def duck_curve(n: int, ph: list[list[float]], depths: list[float], attack: float, release: float) -> np.ndarray:
    """Gain in dB per sample: each phrase's depth, reached 0.02 s before its first word, released after its last."""
    rate = 1000
    m = int(math.ceil(n / SR * rate)) + 2
    g = np.zeros(m)
    t = np.arange(m) / rate
    for (a, b), d in zip(ph, depths):
        a0, a1 = a - 0.02 - attack, a - 0.02
        b1 = b + release
        lo, hi = int(max(0, a0 * rate)), int(min(m, b1 * rate + 1))
        tt = t[lo:hi]
        shape = np.ones_like(tt)
        up = tt < a1
        shape[up] = 0.5 - 0.5 * np.cos(np.pi * np.clip((tt[up] - a0) / attack, 0, 1))
        dn = tt > b
        shape[dn] = 0.5 + 0.5 * np.cos(np.pi * np.clip((tt[dn] - b) / release, 0, 1))
        g[lo:hi] = np.minimum(g[lo:hi], -d * shape)
    return np.interp(np.arange(n) / SR, t, g)


def score_hits(tl: dict) -> list[dict]:
    """The score's own hits (its .json "hits", on the clock it was composed on), in film seconds: each through
    the conform's runs (a hit in a stretch the edit left out has no film time)."""
    m = tl["music"]
    if m.get("mode") != "score":
        return []
    side = (FILM / m["path"]).with_suffix(".json")
    if not side.exists():
        return []
    hits = json.loads(side.read_text()).get("hits", [])
    out = []
    for h in hits:
        o = float(h["t"]) * SR
        for n0, n1, o0 in m.get("_runs", []):
            if o0 <= o < o0 + (n1 - n0):
                out.append({"name": h.get("name", "?"), "t": round((n0 + (o - o0)) / SR, 4)})
                break
    return out


def phrases_v4(tl: dict, bridge: float, hits: list[float]) -> list[list[float]]:
    """The voiced phrases for the duck (v4, #7b): the speech, with gaps up to `bridge` joined, except a gap in
    which the score writes a hit."""
    sp = sorted(v["speech"] for v in tl["voice"])
    out = []
    for a, b in sp:
        if out and a - out[-1][1] < bridge and not any(out[-1][1] - 0.02 <= h < a for h in hits):
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def ridden_duck(n: int, ph: list[list[float]], voice: np.ndarray, music: np.ndarray, lv: dict, meter,
                hits: list[float], speech: list[list[float]]) -> tuple[np.ndarray, list[dict]]:
    """The score's duck (v4, #7): within each phrase the depth is ridden in `duck_ride` windows (half-overlapped)
    toward `balance_lu` under the voice, clipped to [duck_min_db, duck_max_db] and smoothed, instead of one depth
    a phrase (c); in and out with the attack and release; and a hit the score writes in a gap gets `duck_hit_window`
    seconds duck-free from 0.05 s before it, the duck released before it (a)."""
    rate = 1000
    m = int(math.ceil(n / SR * rate)) + 2
    t = np.arange(m) / rate
    depth = np.zeros(m)          # the wanted depth in dB (positive), inside phrases
    inside = np.zeros(m)         # 1 inside a phrase, shaped by attack and release
    win = float(lv.get("duck_ride", 1.0))
    att, rel = lv["duck_attack"], lv["duck_release"]
    rep = []
    for a, b in ph:
        i0, i1 = int(a * SR), int(b * SR)
        Lv_all = lufs(meter, voice[i0:i1])
        Lm_all = lufs(meter, music[i0:i1])
        d_all = float(np.clip(Lm_all - (Lv_all - lv["balance_lu"]), lv["duck_min_db"], lv["duck_max_db"]))
        centres, ds = [], []
        c = a
        while c < b + 1e-6:
            w0, w1 = max(0.0, c - win / 2), c + win / 2
            j0, j1 = int(w0 * SR), min(n, int(w1 * SR))
            Lv = lufs(meter, voice[j0:j1]) if j1 - j0 > int(0.45 * SR) else -99.0
            Lm = lufs(meter, music[j0:j1]) if j1 - j0 > int(0.45 * SR) else -99.0
            if Lv < -45:      # a breath inside the phrase: the phrase's own depth
                d = d_all
            elif Lm < -60:    # no score here: nothing to duck
                d = lv["duck_min_db"]
            else:
                d = float(np.clip(Lm - (Lv - lv["balance_lu"]), lv["duck_min_db"], lv["duck_max_db"]))
            centres.append(c)
            ds.append(d)
            c += win / 2
        lo, hi = int(max(0, (a - 0.02 - att) * rate)), int(min(m, (b + rel) * rate + 1))
        tt = t[lo:hi]
        depth[lo:hi] = np.maximum(depth[lo:hi], np.interp(tt, centres, ds))
        shape = np.ones_like(tt)
        a0, a1 = a - 0.02 - att, a - 0.02
        up = tt < a1
        shape[up] = 0.5 - 0.5 * np.cos(np.pi * np.clip((tt[up] - a0) / att, 0, 1))
        dn = tt > b
        shape[dn] = 0.5 + 0.5 * np.cos(np.pi * np.clip((tt[dn] - b) / rel, 0, 1))
        inside[lo:hi] = np.maximum(inside[lo:hi], shape)
        rep.append({"t": [round(a, 2), round(b, 2)], "duck_db": round(d_all, 1),
                    "ridden": [round(min(ds), 1), round(max(ds), 1)]})
    depth = ndimage.uniform_filter1d(depth, size=int(0.4 * rate))   # no step faster than the ear forgives
    g = -depth * inside
    # (a) the hits written in the gaps: duck-free from 0.05 s before each, for duck_hit_window
    free = np.zeros(m)
    hw = float(lv.get("duck_hit_window", 0.6))
    starts = sorted(a for a, _ in speech)
    for h in hits:
        nxt = next((a for a in starts if a > h + 0.02), 1e9)   # the window yields to the next word: the voice stays on top
        f0, f1 = h - 0.05, max(h + 0.1, min(h + hw, nxt - 0.02 - att))
        r0, r1 = f0 - 0.15, f1 + att               # released over 0.15 s before, back over the attack after
        lo, hi = int(max(0, r0 * rate)), int(min(m, r1 * rate + 1))
        tt = t[lo:hi]
        sh_ = np.ones_like(tt)
        pre = tt < f0
        sh_[pre] = 0.5 - 0.5 * np.cos(np.pi * np.clip((tt[pre] - r0) / 0.15, 0, 1))
        post = tt > f1
        sh_[post] = 0.5 + 0.5 * np.cos(np.pi * np.clip((tt[post] - f1) / att, 0, 1))
        free[lo:hi] = np.maximum(free[lo:hi], sh_)
    g = g * (1 - free)
    return np.interp(np.arange(n) / SR, t, g), rep


def hits_in_gaps(tl: dict, hits: list[dict]) -> list[dict]:
    """The score's hits not under a word: in a silence, or at a line's very end (a cut on its last word)."""
    words = [(w["t"] + s["start"], w["e"] + s["start"]) for s in tl["shots"] for w in s["words"] if "e" in w]
    out = []
    for h in hits:
        if not any(a + 0.02 < h["t"] < b - 0.02 for a, b in words):
            out.append(h)
    return out


def voice_bus(voice: np.ndarray, meter, lv: dict) -> tuple[np.ndarray, dict]:
    """The voice bus (for small speakers): a presence lift (+2 dB, a peak at 3 kHz spanning about 2-4 kHz), then
    a gentle leveller (2:1 toward the clip level over 0.4 s windows, at most 3 dB either way, smoothed over
    0.3 s); the bus is then set back to its own integrated loudness, so the balance with the score is unchanged."""
    L0 = lufs(meter, voice)
    gdb, f0, q = float(lv.get("voice_presence_db", 2.0)), 3000.0, 0.8
    A = 10 ** (gdb / 40)
    w0 = 2 * math.pi * f0 / SR
    al = math.sin(w0) / (2 * q)
    b = np.array([1 + al * A, -2 * math.cos(w0), 1 - al * A])
    a = np.array([1 + al / A, -2 * math.cos(w0), 1 - al / A])
    y = signal.lfilter(b / a[0], a / a[0], voice)
    blk = int(0.1 * SR)
    nb = len(y) // blk
    ms = (y[: nb * blk].reshape(nb, blk) ** 2).mean(axis=1)
    ms4 = ndimage.uniform_filter1d(ms, size=4)          # 0.4 s windows, as the momentary meter
    lev = 10 * np.log10(np.maximum(ms4, 1e-12)) - 0.691  # about LUFS for a centred mono voice
    target = lv["voice_clip_lufs"]
    rng = float(lv.get("voice_level_range_db", 3.0))
    gain = np.where(lev > -45, np.clip((target - lev) * 0.5, -rng, rng), 0.0)
    gain = ndimage.uniform_filter1d(gain, size=3)
    g = np.interp(np.arange(len(y)), np.arange(nb) * blk + blk / 2, gain)
    y = y * 10 ** (g / 20)
    L1 = lufs(meter, y)
    y *= 10 ** ((L0 - L1) / 20)
    return y, {"presence_db": gdb, "leveller_range_db": rng, "lufs_kept": round(L0, 2)}


def section_curve(n: int, tl: dict, cfg_sections: dict, base: float, music: np.ndarray, meter) -> np.ndarray:
    """The score's gain in dB: `music_db` everywhere, plus a section's own `music_db`, or the gain that
    brings that section's score to its `music_lufs` (measured, so it holds whatever the score's own level)."""
    rate = 1000
    m = int(math.ceil(n / SR * rate)) + 2
    g = np.full(m, base)
    for s in tl["sections"]:
        sc = cfg_sections.get(s["id"], {})
        a, b = int(s["start"] * rate), int((s["start"] + s["len"]) * rate)
        if "music_lufs" in sc:
            L = lufs(meter, music[int(s["start"] * SR): int((s["start"] + s["len"]) * SR)])
            g[a:b] = sc["music_lufs"] - L if L > -70 else base
        elif sc.get("music_db"):
            g[a:b] += sc["music_db"]
    # smooth the steps over 20 ms: the montage's level changes on its hard cuts
    g = ndimage.uniform_filter1d(g, size=20)
    return np.interp(np.arange(n) / SR, np.arange(m) / rate, g)


def true_peak(x: np.ndarray, chunk: int = 10 * SR, margin: int = 256) -> np.ndarray:
    """Per-sample true-peak estimate (4x oversampled), max over channels; in chunks, to spare memory."""
    out = np.empty(len(x))
    for i in range(0, len(x), chunk):
        lo, hi = max(0, i - margin), min(len(x), i + chunk + margin)
        up = np.abs(signal.resample_poly(x[lo:hi], 4, 1, axis=0)).max(axis=1)
        p = up[: (hi - lo) * 4].reshape(hi - lo, 4).max(axis=1)
        j = min(len(x), i + chunk)
        out[i:j] = p[i - lo: i - lo + (j - i)]
    return out


def limit(x: np.ndarray, ceiling_db: float, look: float = 0.003, release: float = 0.08) -> tuple[np.ndarray, float]:
    """A look-ahead true-peak limiter with smooth gain: in 0.5 ms control blocks, the reduction each
    peak needs is held over the look-ahead, ramped in linearly across it, and released exponentially;
    the gain curve is interpolated between blocks, so it never steps."""
    c = 10 ** (ceiling_db / 20)
    p = true_peak(x)
    need = np.clip(1 - c / np.maximum(p, 1e-12), 0, 1)  # gain reduction needed, 0..1
    if need.max() <= 0:
        return x, 0.0
    blk = SR // 2000
    nb = int(math.ceil(len(need) / blk))
    hb = np.pad(need, (0, nb * blk - len(need))).reshape(nb, blk).max(axis=1)
    hb = ndimage.maximum_filter1d(hb, size=3)            # a block's peak covers its neighbours too
    la = max(1, int(round(look * 2000)))
    hh = ndimage.maximum_filter1d(hb, size=la + 1, origin=-(la // 2) - (la % 2))  # max over [i, i + la]
    cs = np.concatenate([[0.0], np.cumsum(hh)])
    idx = np.arange(nb)
    lo = np.maximum(0, idx - la)
    s = (cs[idx + 1] - cs[lo]) / (idx + 1 - lo)          # mean over [i - la, i]: a linear ramp in
    a = math.exp(-1.0 / (release * 2000))
    y = np.empty(nb)
    prev = 0.0
    for i, v in enumerate(s.tolist()):
        prev = v if v >= prev else prev * a + v * (1 - a)
        y[i] = prev
    curve = np.interp(np.arange(len(x)), np.arange(nb) * blk + blk / 2, y)
    out = x * (1 - curve)[:, None]
    return out, float(20 * np.log10(max(1e-9, 1 - curve.max())))


def game_audio(s: dict, tmpdir: Path) -> np.ndarray | None:
    x = _game_audio(s, tmpdir)
    if x is None:
        return None
    src = s["source"]
    f = int(0.01 * SR)
    for o in s["overlays"]:  # a switch to another file of the same camera and clock: its sound too
        if o.get("type") == "video" and o.get("enable") and o.get("sound"):
            y, sr = sf.read(str(FILM / o["sound"]), dtype="float64", always_2d=True)
            t0 = float(o.get("in", src["in"])) - float(o.get("at") or 0.0)  # the file's time at shot time 0
            y = y[int(round(t0 * sr)):] if t0 >= 0 else np.concatenate([np.zeros((int(round(-t0 * sr)), y.shape[1])), y])
            if sr != SR:
                g = math.gcd(SR, sr)
                y = signal.resample_poly(y, SR // g, sr // g, axis=0)
            if y.shape[1] == 1:
                y = np.repeat(y, 2, axis=1)
            a, b = int(o["enable"][0] * SR), min(len(x), int(o["enable"][1] * SR), len(y))
            if b - a > 2 * f:
                w = np.ones(b - a)
                w[:f] = np.linspace(0, 1, f)
                w[-f:] = np.linspace(1, 0, f)
                x[a:b] = x[a:b] * (1 - w)[:, None] + y[a:b, :2] * w[:, None]
    for a_, b_ in s.get("game_mute", []):  # the stem carries this moment (a tape-stop, a silence)
        a, b = max(0, int(a_ * SR)), min(len(x), int(b_ * SR))
        if b > a:
            x[a:b] = 0.0
            x[max(0, a - f):a] *= np.linspace(1, 0, min(f, a))[:, None] if a else 1
    return x


def _read_48k(path: str) -> np.ndarray:
    y, sr = sf.read(path, dtype="float64", always_2d=True)
    if sr != SR:
        g = math.gcd(SR, sr)
        y = signal.resample_poly(y, SR // g, sr // g, axis=0)
    if y.shape[1] == 1:
        y = np.repeat(y, 2, axis=1)
    return y[:, :2]


def _composed(s: dict) -> np.ndarray:
    """A shot's sound from pieces: each a WAV's span [from, to] at a shot time, until a time (looping its span
    if `repeat_from` is set: the loop restarts there, as id's mixer restarts a looping sample)."""
    n = int(round(s["len"] * SR))
    out = np.zeros((n, 2))
    f = int(0.006 * SR)
    for pc in s["source"]["compose"]:
        y = _read_48k(str(FILM / pc["wav"]))
        a = int(round(pc["from"] * SR))
        b = int(round(pc["to"] * SR)) if pc.get("to") else len(y)
        seg = y[a:b]
        at = int(round(pc["at"] * SR))
        until = int(round(pc["until"] * SR)) if pc.get("until") is not None else n
        want = max(0, min(n, until) - at)
        if pc.get("repeat_from") is not None and len(seg) < want:
            lap = y[int(round(float(pc["repeat_from"]) * SR)):b]
            while len(seg) < want:
                seg = np.concatenate([seg, lap])
        seg = seg[:want].copy()
        fi = max(f, int(float(pc.get("fade_in") or 0.0) * SR))   # a piece may come in or go out slowly (a bed)
        fo = max(f, int(float(pc.get("fade_out") or 0.0) * SR))
        if len(seg) > fi + fo:
            seg[:fi] *= (0.5 - 0.5 * np.cos(np.linspace(0, np.pi, fi)))[:, None]
            seg[-fo:] *= (0.5 + 0.5 * np.cos(np.linspace(0, np.pi, fo)))[:, None]
        seg *= 10 ** (float(pc.get("db") or 0.0) / 20)
        out[at:at + len(seg)] += seg
    return out


def _game_audio(s: dict, tmpdir: Path) -> np.ndarray | None:
    src = s["source"]
    if src.get("compose"):
        return _composed(s)
    if src["type"] not in ("video", "burst") or src["status"] == "diagram" or s.get("mute_game"):
        return None
    if src.get("sound"):  # a WAV aligned to the mp4's first frame, at the engine's own rate
        x, sr = sf.read(src.get("frozen_sound") or str(FILM / src["sound"]), dtype="float64", always_2d=True)
        x = x[int(round(src["in"] * sr)): int(round((src["in"] + s["len"]) * sr))]
        if sr != SR:
            g = math.gcd(SR, sr)
            x = signal.resample_poly(x, SR // g, sr // g, axis=0)
        if x.shape[1] == 1:
            x = np.repeat(x, 2, axis=1)
        return x[:, :2] if len(x) else None
    if not (src.get("probe") or {}).get("audio"):
        return None
    p = tmpdir / f"game-{s['id']}.f32"
    cmd = ["ffmpeg", "-y", "-loglevel", "error"] + (["-ss", f"{src['in']:.5f}"] if src["in"] > 0 else []) + [
        "-i", src.get("frozen") or str(FILM / src["path"]), "-t", f"{s['len']:.5f}", "-vn", "-ac", "2", "-ar", str(SR),
        "-f", "f32le", str(p)]
    sh(cmd)
    x = np.fromfile(p, dtype=np.float32).astype(np.float64).reshape(-1, 2)
    return x if len(x) else None


def mix_audio(tl: dict, cfg: dict, n: int, outdir: Path, tmpdir: Path) -> dict:
    lv = cfg["levels"]
    meter = pyln.Meter(SR)
    rep: dict = {"clips": []}

    # the voice: every clip to one loudness, on its sample
    voice = np.zeros(n)
    for v in tl["voice"]:
        if not v.get("file"):  # a line not recorded yet: silence (it still ducks the score, and is subtitled)
            rep["clips"].append({"id": v["id"], "placeholder": True})
            continue
        x, sr = sf.read(str(FILM / v["file"]), dtype="float64")
        if x.ndim > 1:
            x = x.mean(axis=1)
        if v.get("file_in") or v.get("file_out"):  # one clip said as two: this part of it
            a_ = int(round(float(v.get("file_in") or 0.0) * sr))
            b_ = int(round(float(v["file_out"]) * sr)) if v.get("file_out") else len(x)
            x = x[a_:b_].copy()
            f_ = int(0.004 * sr)
            x[:f_] *= np.linspace(0, 1, f_)
            x[-f_:] *= np.linspace(1, 0, f_)
        if v.get("hard_end"):  # cut off mid-word: stop on its last sound, a 2 ms fade against a click
            k = int(round((v["speech"][1] - v["at"]) * SR))
            x = x[:k].copy()
            f = int(0.002 * SR)
            x[-f:] *= np.linspace(1, 0, f)
        L = lufs(meter, x)
        g = lv["voice_clip_lufs"] - L + float(v.get("gain_db", 0.0))  # a line set lower or higher on purpose
        x = x * 10 ** (g / 20)
        a = v["sample"]
        m = min(len(x), n - a)
        voice[a:a + m] += x[:m]
        rep["clips"].append({"id": v["id"], "lufs_in": round(L, 2), "gain_db": round(g, 2)})
    if not any(c.get("lufs_in") is not None for c in rep["clips"]):
        rep["clips"].append({"id": "-", "lufs_in": 0.0, "gain_db": 0.0})
    if lv.get("voice_presence_db") is not None:  # v4: the bus's presence and leveller
        voice, rep["voice_bus"] = voice_bus(voice, meter, lv)
    for s in tl["shots"]:  # a word lifted (a punchline) or a breath lowered: film times, 30 ms ramps
        for a_, b_, d_ in s.get("voice_gains", []):
            r_ = int(0.03 * SR)
            lo, hi = max(0, int((s["start"] + a_) * SR) - r_), min(n, int((s["start"] + b_) * SR) + r_)
            if hi > lo:
                env = np.zeros(hi - lo)
                k_ = min(r_, (hi - lo) // 2)
                if k_:
                    env[:k_], env[-k_:] = np.linspace(1, 0, k_), np.linspace(0, 1, k_)
                g_ = 10 ** (d_ / 20)
                voice[lo:hi] *= g_ + (1 - g_) * env
                rep.setdefault("voice_gains", []).append([s["id"], round(s["start"] + a_, 3), d_])
    # the voice bus: its peaks held to voice_plr above its loudness, so the master limiter rarely touches it
    voice2, vgr = limit(voice[:, None], lv["voice_clip_lufs"] + lv["voice_plr"], look=0.003, release=0.12)
    voice = voice2[:, 0]
    rep["voice_limiter_max_gr_db"] = round(vgr, 2)

    # the score, by section, then its level per section, then ducked under each phrase
    music, how = music_track(tl, n)
    rep["music"] = how
    base = lv["music_db"]
    ref = cfg["music"].get("reference")
    if ref:  # one gain for the whole score, set so that the reference section (the montage) measures ref["lufs"]
        s = next((x for x in tl["sections"] if x["id"] == ref["section"]), None)
        if s:
            Lr = lufs(meter, music[int(s["start"] * SR): int((s["start"] + s["len"]) * SR)])
            if Lr > -70:
                base = ref["lufs"] - Lr
                rep["music_reference"] = {"section": ref["section"], "measured": round(Lr, 2), "gain_db": round(base, 2)}
    music *= 10 ** (section_curve(n, tl, cfg.get("sections", {}), base, music, meter) / 20)[:, None]
    def window_gain(x, a_, b_, db, ramp=0.05):  # x *= db over [a_, b_] (film seconds), raised-cosine ramps outside
        r_ = int(ramp * SR)
        lo, hi = max(0, int(a_ * SR) - r_), min(len(x), int(b_ * SR) + r_)
        if hi <= lo:
            return
        env = np.zeros(hi - lo)
        k_ = min(r_, (hi - lo) // 2)
        if k_:
            env[:k_] = 0.5 + 0.5 * np.cos(np.linspace(0, np.pi, k_))
            env[-k_:] = 0.5 - 0.5 * np.cos(np.linspace(0, np.pi, k_))
        g_ = 10 ** (db / 20)
        x[lo:hi] *= (g_ + (1 - g_) * env)[:, None]
    for s in tl["shots"]:  # a shot's score up or down (the act's written peaks), and dips under its words
        if s.get("music_db"):
            window_gain(music, s["start"], s["start"] + s["len"], s["music_db"])
        for a_, b_, d_ in s.get("music_dips", []):
            window_gain(music, s["start"] + a_, s["start"] + b_, d_, ramp=0.04)
    rep["music_shot_gains"] = {s["id"]: s["music_db"] for s in tl["shots"] if s.get("music_db")}
    for ref in cfg["music"].get("soft_stops", []):  # where the score stops dead into silence: 60 ms, raised cosine
        t_ = timeline.resolve_ref(tl["shots"], ref)
        if t_ is None:
            continue
        b_, k_ = int(t_ * SR), int(0.06 * SR)
        if np.abs(music[b_ + int(0.02 * SR):b_ + int(0.12 * SR)]).max(initial=0.0) < 1e-3:   # only into a silence
            music[b_ - k_:b_] *= (0.5 + 0.5 * np.cos(np.linspace(0, np.pi, k_)))[:, None]
            music[b_:b_ + int(0.12 * SR)] = 0.0
            rep.setdefault("soft_stops", []).append(round(t_, 3))
    if lv.get("duck_ride"):  # v4 (#7): bridged phrases split at hits, the depth ridden, hits in gaps left clear
        hits = hits_in_gaps(tl, score_hits(tl))
        rep["score_hits_in_gaps"] = hits
        ph = phrases_v4(tl, float(lv.get("duck_bridge", lv["duck_hold"])), [h["t"] for h in hits])
        duck, rep["phrases"] = ridden_duck(n, ph, voice, music, lv, meter, [h["t"] for h in hits],
                                           [v["speech"] for v in tl["voice"]])
    else:
        ph = phrases(tl, lv["duck_hold"])
        depths = []
        for a, b in ph:
            i0, i1 = int(a * SR), int(b * SR)
            Lv = lufs(meter, voice[i0:i1])
            Lm = lufs(meter, music[i0:i1])
            d = Lm - (Lv - lv["balance_lu"])
            depths.append(float(np.clip(d, lv["duck_min_db"], lv["duck_max_db"])))
        duck = duck_curve(n, ph, depths, lv["duck_attack"], lv["duck_release"])
        rep["phrases"] = [{"t": [round(a, 2), round(b, 2)], "duck_db": round(d, 1)} for (a, b), d in zip(ph, depths)]
    for s in tl["shots"]:  # a shot whose score is already written under the voice: no duck
        if s.get("music_unducked"):
            a, b = s["frame"] * SPF, (s["frame"] + s["frames"]) * SPF
            r = int(0.05 * SR)
            w = np.ones(b - a + 2 * r)
            w[:r], w[-r:] = np.linspace(0, 1, r), np.linspace(1, 0, r)
            lo = max(0, a - r)
            duck[lo:b + r] *= 1 - w[lo - (a - r):][: len(duck[lo:b + r])]
    music *= 10 ** (duck / 20)[:, None]

    # each shot's own sound
    game = np.zeros((n, 2))
    rep["game"] = []
    for s in tl["shots"]:
        x = game_audio(s, tmpdir)
        if x is None:
            continue
        L = lufs(meter, x)
        if L < -70:
            continue
        x = x * 10 ** ((s["game_lufs"] - L) / 20)
        for gd in s.get("game_dips", []):  # the shot's sound under a word: d_ dB, 40 ms in and out (or a ramp given)
            a_, b_, d_ = gd[:3]
            r_ = int((gd[3] if len(gd) > 3 else 0.04) * SR)
            lo, hi = max(0, int(a_ * SR) - r_), min(len(x), int(b_ * SR) + r_)
            if hi > lo:
                env = np.zeros(hi - lo)            # 1 = untouched, 0 = the full dip
                k_ = min(r_, (hi - lo) // 2)
                if k_:
                    env[:k_] = np.linspace(1, 0, k_)
                    env[-k_:] = np.linspace(0, 1, k_)
                g_ = 10 ** (d_ / 20)
                x[lo:hi] *= (g_ + (1 - g_) * env)[:, None]
        f = int(0.01 * SR)
        x[:f] *= np.linspace(0, 1, f)[:, None]
        nxt = next((q for q in tl["shots"] if q["frame"] == s["frame"] + s["frames"]), None)
        same = nxt is not None and (nxt["source"].get("path") == s["source"].get("path") or
                                    (nxt["source"].get("compose") is not None and s["source"].get("compose") is not None))
        tail = 0.01 if same else max(0.04, float(s.get("game_fade_out", 0.04)))  # a 40 ms tail where the sound ends,
        fo = min(len(x), int(tail * SR))                                          # 10 ms where the same file goes on
        x[-fo:] *= (0.5 + 0.5 * np.cos(np.linspace(0, np.pi, fo)))[:, None]
        a = s["frame"] * SPF
        m = min(len(x), n - a)
        game[a:a + m] += x[:m]
        rep["game"].append({"shot": s["id"], "lufs_in": round(L, 1), "to": s["game_lufs"]})
    if rep["game"]:
        game_duck = duck_curve(n, ph, [lv["game_duck_db"]] * len(ph), lv["duck_attack"], lv["duck_release"])
        for s in tl["shots"]:  # a shot played at full level with the score (HERO): its sound is never ducked
            if s.get("game_unducked"):
                a, b = s["frame"] * SPF, (s["frame"] + s["frames"]) * SPF
                game_duck[a:b] = 0.0
        game *= 10 ** (game_duck / 20)[:, None]

    # a muted shot (G2, the gag's cut-off): every bus but the voice stops on its first frame, no tails
    for s in tl["shots"]:
        for w0, w1 in s.get("mutes") or ([[0.0, s["len"]]] if s.get("mute") else []):
            a, b = s["frame"] * SPF + int(w0 * SR), s["frame"] * SPF + int(w1 * SR)
            music[a:b] = 0.0
            game[a:b] = 0.0
            rep.setdefault("muted", []).append([s["id"], w0, w1])
    # the voice in the centre: each channel 3 dB down, so the stereo mix measures it as loud as the mono clip
    vs = np.repeat(voice[:, None], 2, axis=1) * 10 ** (-3.0103 / 20)
    # the sound designer's stem: made on this clock, ducked a fixed sfx_duck_db under each phrase
    sfx = np.zeros((n, 2))
    stem = find_sfx(tl, cfg, n)
    rep["sfx"] = stem.get("note")
    if stem.get("path"):
        x = read_stereo(FILM / stem["path"])
        k = min(n, len(x))
        sfx[:k] = x[:k] * 10 ** (lv.get("sfx_db", 0.0) / 20)
        for s in tl["shots"]:
            for w0, w1 in s.get("mutes") or ([[0.0, s["len"]]] if s.get("mute") else []):
                sfx[s["frame"] * SPF + int(w0 * SR):s["frame"] * SPF + int(w1 * SR)] = 0.0
        sduck = duck_curve(n, ph, [lv.get("sfx_duck_db", 6.0)] * len(ph), lv["duck_attack"], lv["duck_release"])
        plan = cfg.get("_ladder_plan") or {}
        sw = [t for t in list((plan.get("on") or {}).values()) +
              ([] if lv.get("sfx_switch_lights_only") else list((plan.get("off") or {}).values())) if t is not None]
        w0, w1 = lv.get("sfx_switch_window", [0.1, 0.05])   # the sound designer: each switch's click (50 ms before
        if sw and w0 + w1 > 0:                               # its light) clear of the effects duck, 150 ms
            free = np.zeros(n)
            r = int(0.02 * SR)
            for t in sw:
                a, b = int((t - w0) * SR), int((t + w1) * SR)
                lo, hi = max(0, a - r), min(n, b + r)
                tt = np.arange(lo, hi)
                free[lo:hi] = np.maximum(free[lo:hi], np.clip(np.minimum((tt - (a - r)) / r, ((b + r) - tt) / r), 0, 1))
            sduck *= 1 - free
            rep["sfx_switch_windows"] = len(sw)
        for ref in lv.get("sfx_free", []):  # a hit of the effects' own, on a cut (HERO's downbeat): 0.6 s duck-free
            t_ = timeline.resolve_ref(tl["shots"], ref)
            if t_ is None:
                continue
            a, b, r = int((t_ - 0.05) * SR), int((t_ + 0.6) * SR), int(0.03 * SR)
            lo, hi = max(0, a - r), min(n, b + r)
            tt = np.arange(lo, hi)
            sduck[lo:hi] *= 1 - np.clip(np.minimum((tt - (a - r)) / r, ((b + r) - tt) / r), 0, 1)
            rep.setdefault("sfx_free", []).append(round(t_, 3))
        sfx *= 10 ** (sduck / 20)[:, None]
        for s in tl["shots"]:  # the effects under a word that they still mask (S62's "has a name")
            for a_, b_, d_ in s.get("sfx_dips", []):
                r_ = int(0.03 * SR)
                lo, hi = max(0, int((s["start"] + a_) * SR) - r_), min(n, int((s["start"] + b_) * SR) + r_)
                if hi > lo:
                    env = np.zeros(hi - lo)
                    k_ = min(r_, (hi - lo) // 2)
                    if k_:
                        env[:k_], env[-k_:] = np.linspace(1, 0, k_), np.linspace(0, 1, k_)
                    g_ = 10 ** (d_ / 20)
                    sfx[lo:hi] *= (g_ + (1 - g_) * env)[:, None]
        for ref in cfg["music"].get("soft_stops", []):
            t_ = timeline.resolve_ref(tl["shots"], ref)
            if t_ is None:
                continue
            b_, k_ = int(t_ * SR), int(0.06 * SR)
            if np.abs(sfx[b_ + int(0.02 * SR):b_ + int(0.12 * SR)]).max(initial=0.0) < 1e-3:
                sfx[b_ - k_:b_] *= (0.5 + 0.5 * np.cos(np.linspace(0, np.pi, k_)))[:, None]
        rep["sfx_lufs"] = round(lufs(meter, sfx), 2)
    return rep, {"voice": vs, "music": music, "game": game, "sfx": sfx}


def find_sfx(tl: dict, cfg: dict, n: int) -> dict:
    """The newest SFX stem made on this clock: its .json names the timeline it was rendered on, and its
    length is the film's. Anything else is reported and left out."""
    sc = cfg.get("sfx")
    if not sc:
        return {"note": "no SFX stem configured"}
    cands = sorted(FILM.glob(sc["glob"]), key=lambda p: int(re.search(r"(\d+)$", p.stem).group(1))
                   if re.search(r"(\d+)$", p.stem) else -1, reverse=True)
    seen = []
    for p in cands:
        side = p.with_suffix(".json")
        meta = json.loads(side.read_text()) if side.exists() else {}
        clock = meta.get("timeline", "")
        dur = float(meta.get("duration_s", 0))
        ok_clock = Path(clock).name == Path(sc["clock_must_match"]).name and Path(sc["clock_must_match"]).parent.name in clock
        if ok_clock and abs(dur - n / SR) < 0.05:
            return {"path": str(p.relative_to(FILM)), "note": f"{p.relative_to(FILM)} (on {clock})"}
        seen.append(f"{p.name} ({clock or 'no clock'}, {dur:.2f} s)")
    return {"note": "none on this clock yet" + (": " + "; ".join(seen) if seen else "")}


def master(stems: dict, lv: dict, ceiling: float, tmpdir: Path, rep: dict, tl: dict) -> None:
    """The master: normalised to master_lufs, limited under `ceiling` (true peak), as master.wav."""
    meter = pyln.Meter(SR)
    mix = stems["voice"] + stems["music"] + stems["game"] + stems.get("sfx", 0.0)
    L0 = lufs(meter, mix)
    g = lv["master_lufs"] - L0
    gr = 0.0
    for _ in range(6):
        y, gr = limit(mix * 10 ** (g / 20), ceiling)
        L1 = lufs(meter, y)
        if abs(L1 - lv["master_lufs"]) < 0.1:
            break
        g += lv["master_lufs"] - L1
    gain = 10 ** (g / 20)
    rep["master"] = {"pre_lufs": round(L0, 2), "gain_db": round(g, 2), "ceiling_dbtp": round(ceiling, 2),
                     "limiter_max_gr_db": round(gr, 2), "lufs": round(lufs(meter, y), 2)}
    sf.write(str(tmpdir / "master.wav"), y, SR, subtype="PCM_24")
    post = {k: v * gain for k, v in stems.items()}
    rep["balance"] = balance_report(tl, post, meter)
    return post


def balance_report(tl: dict, stems: dict, meter) -> list[dict]:
    """The voice against the score (after the master gain), 3 s windows in the middle of some long phrases."""
    ph = sorted(phrases(tl, 0.7), key=lambda p: p[0])
    by_sec: dict[str, list] = {}
    for s in tl["sections"]:
        for a, b in ph:
            if s["start"] <= a < s["start"] + s["len"] and b - a >= 3.2:
                by_sec.setdefault(s["id"], []).append((a, b))
    out = []
    for sid, lst in by_sec.items():
        a, b = max(lst, key=lambda p: p[1] - p[0])
        mid = (a + b) / 2
        i0, i1 = int((mid - 1.5) * SR), int((mid + 1.5) * SR)
        Lv = lufs(meter, stems["voice"][i0:i1])
        Lm = lufs(meter, stems["music"][i0:i1])
        out.append({"section": sid, "t": round(mid - 1.5, 2), "voice_lufs": round(Lv, 1), "music_lufs": round(Lm, 1),
                    "voice_minus_music": round(Lv - Lm, 1)})
    # music alone: the montage, the title, the end card
    for sid in ("S14", "S64"):
        s = next((x for x in tl["shots"] if x["id"] == sid), None)
        if s:
            i0, i1 = int(s["start"] * SR), int((s["start"] + min(3.0, s["len"])) * SR)
            out.append({"section": f"{sid} (alone)", "t": round(s["start"], 2), "music_lufs":
                        round(lufs(meter, stems["music"][i0:i1]), 1)})
    mont = [s for s in tl["sections"] if s["id"] == "4.5"]
    if mont:
        s = mont[0]
        i0, i1 = int(s["start"] * SR), int((s["start"] + s["len"]) * SR)
        out.append({"section": "4.5 montage (alone)", "t": round(s["start"], 2),
                    "music_lufs": round(lufs(meter, stems["music"][i0:i1]), 1)})
    return out


# ================================================================ encode ====

def encode_video(paths: list[Path], out: Path, burn_in: bool, tmpdir: Path, height: int | None = None) -> None:
    """The picture's final encode: the segments, concatenated in order, at the frame's size (the master) or scaled
    down to `height` (area-averaged: the 1080p copy of a 4K build). On the GPU (--hw vaapi): HEVC at a constant QP
    (MASTER_QP) for the master, H.264 for the 1080p copy (it plays anywhere); in software: H.264 at CRF 17."""
    lst = tmpdir / "segments.txt"
    lst.write_text("".join(f"file '{p}'\n" for p in paths))
    vf = []
    if burn_in:
        vf.append(f"drawtext=fontfile={mono_font()}:text='%{{pts\\:hms}}':x=w-tw-{30 * S}:y={8 * S}:fontsize={22 * S}:"
                  f"fontcolor=0xEFBF77@0.9:box=1:boxcolor=0x000000@0.55:boxborderw={6 * S}")
    if height:
        vf.append(f"scale=-2:{height}:flags=area")
    vf.append("format=yuv420p")
    cmd = ["ffmpeg", "-y", "-loglevel", "error"]
    if HW == "vaapi":
        vf += ["format=nv12", "hwupload"]
        cmd += ["-vaapi_device", VAAPI_DEVICE]
    cmd += ["-f", "concat", "-safe", "0", "-i", str(lst), "-map", "0:v", "-vf", ",".join(vf), "-fps_mode", "cfr", "-r", "60"]
    if HW == "vaapi" and not height:
        cmd += ["-c:v", "hevc_vaapi", "-rc_mode", "CQP", "-qp", str(MASTER_QP), "-profile:v", "main", "-tag:v", "hvc1"]
    elif HW == "vaapi":
        cmd += ["-c:v", "h264_vaapi", "-rc_mode", "CQP", "-qp", str(MASTER_QP), "-profile:v", "high"]
    else:
        cmd += ["-c:v", "libx264", "-preset", "slow", "-crf", "17", "-pix_fmt", "yuv420p", "-profile:v", "high"]
    cmd += ["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv",
            "-an", str(out)]
    sh(cmd)


def mux(video: Path, master: Path, out: Path, kbps: int = 320) -> None:
    sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(video), "-i", str(master), "-map", "0:v", "-map", "1:a",
        "-c:v", "copy", "-c:a", "aac", "-b:a", f"{kbps}k", "-ar", str(SR), "-shortest", "-movflags", "+faststart", str(out)])


def ebur128(p: Path) -> dict:
    r = subprocess.run(["ffmpeg", "-hide_banner", "-nostats", "-i", str(p), "-map", "0:a", "-af", "ebur128=peak=true",
                        "-f", "null", "-"], capture_output=True, text=True)
    t = r.stderr[r.stderr.rfind("Summary:"):]
    get = lambda pat: float(re.search(pat, t, re.S).group(1))  # noqa: E731
    return {"integrated": get(r"I:\s+(-?[\d.]+) LUFS"), "lra": get(r"LRA:\s+(-?[\d.]+) LU"),
            "true_peak": get(r"Peak:\s+(-?[\d.]+) dBFS")}


def contact_sheet(film: Path, tl: dict, out: Path, tmpdir: Path, every: float = 5.0) -> None:
    from PIL import Image
    d = tmpdir / "contact"
    d.mkdir(exist_ok=True)
    step = int(every * FPS)
    sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(film), "-vf",
        f"select='not(mod(n\\,{step}))',scale=384:216:flags=area", "-fps_mode", "vfr", str(d / "%04d.png")])
    frames = sorted(d.glob("*.png"))
    cols, tw, th, lab = 5, 384, 216, 30
    rows = math.ceil(len(frames) / cols)
    head = 56
    c = cards.Canvas(cards.cairo.ImageSurface(cards.cairo.FORMAT_RGB24, cols * tw, head + rows * (th + lab)))
    c.rect(0, 0, c.w, c.h, fill="bg_deep")
    c.text(f"quake-srp {out.stem} · a frame every {every:g} s · {timeline.tc(tl['duration'])}", 12, 16, 3, "gold")
    starts = [(s["frame"], s["id"]) for s in tl["shots"]]
    for k in range(len(frames)):
        f = k * step
        sid = [i for fr, i in starts if fr <= f][-1]
        r, col = divmod(k, cols)
        c.text(f"{timeline.tc(f / FPS)[:-3]}  {sid}", col * tw + 6, head + r * (th + lab) + th + 7, 2, "dim")
    c.surface.flush()
    from PIL import Image as I
    base = I.frombuffer("RGB", (c.w, c.h), bytes(c.surface.get_data()), "raw", "BGRX", c.surface.get_stride(), 1).copy()
    for k, f in enumerate(frames):
        r, col = divmod(k, cols)
        base.paste(Image.open(f).convert("RGB").resize((tw - 4, th - 4)), (col * tw + 2, head + r * (th + lab) + 2))
    base.save(out)


def loudness_md(rep: dict, final: dict, master: dict, tl: dict, cfg: dict) -> str:
    lv = cfg["levels"]
    L = [f"# Loudness: {tl['duration']:.2f} s", "",
         f"Targets: {lv['master_lufs']} LUFS integrated, true peak at most {lv['master_tp']} dBTP (the limiter's "
         f"ceiling {rep['master']['ceiling_dbtp']} dBTP before AAC; where the encoded film still peaked over the limit, the master was turned down there alone: {sum(len(x) for x in rep.get('codec_fixes', []))} places). The voice: every clip levelled to {lv['voice_clip_lufs']} LUFS, "
         f"then the master gain. The score: {lv['music_db']:+.1f} dB, ducked under each phrase to sit "
         f"{lv['balance_lu']:.0f} LU below the voice ({lv['duck_min_db']:.0f}-{lv['duck_max_db']:.0f} dB).", "",
         "| measured on | integrated (LUFS) | range (LU) | true peak (dBTP) |", "|---|---|---|---|",
         f"| the master WAV (24-bit) | {master['integrated']:.1f} | {master['lra']:.1f} | {master['true_peak']:.1f} |",
         f"| the film (AAC {lv.get('aac_kbps', 256)}k) | {final['integrated']:.1f} | {final['lra']:.1f} | {final['true_peak']:.1f} |", "",
         f"Master gain {rep['master']['gain_db']:+.2f} dB on a mix of {rep['master']['pre_lufs']:.1f} LUFS; the limiter's "
         f"deepest reduction {rep['master']['limiter_max_gr_db']:.2f} dB. The voice bus's own peak limiter "
         f"({lv['voice_clip_lufs'] + lv['voice_plr']:.0f} dBTP): {rep['voice_limiter_max_gr_db']:.2f} dB at most.", "",
         f"Music: {rep['music']}.", "",
         "## The voice against the score (3 s windows, after the master gain)", "",
         "| where | from | voice (LUFS) | score (LUFS) | voice - score (LU) |", "|---|---|---|---|---|"]
    for b in rep["balance"]:
        L.append(f"| {b['section']} | {timeline.tc(b['t'])} | {b.get('voice_lufs', '-')} | {b['music_lufs']} | "
                 f"{b.get('voice_minus_music', '-')} |")
    d = [p["duck_db"] for p in rep["phrases"]]
    L += ["", f"{len(rep['phrases'])} voiced phrases (silences under {lv['duck_hold']} s bridged); duck depth "
          f"{min(d):.1f}-{max(d):.1f} dB, median {float(np.median(d)):.1f} dB.", "",
          f"Clips before levelling: {min(c['lufs_in'] for c in rep['clips']):.1f} to "
          f"{max(c['lufs_in'] for c in rep['clips']):.1f} LUFS; gains {min(c['gain_db'] for c in rep['clips']):+.1f} to "
          f"{max(c['gain_db'] for c in rep['clips']):+.1f} dB.", "",
          "Game sound: " + (", ".join(f"{g['shot']} ({g['lufs_in']} -> {g['to']} LUFS)" for g in rep["game"]) or
                            "none yet (no source with sound).")]
    return "\n".join(L) + "\n"


# ================================================================== main ====

def codec_overshoot(film: Path, n: int, target_db: float) -> tuple[np.ndarray, list]:
    """Per 10 ms of the decoded film, the dB its true peak exceeds target_db: held over 100 ms either side,
    smoothed, as a gain curve (dB of reduction per sample), and the places it found."""
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", str(film), "-vn", "-f", "f32le", "-ac", "2",
                          "-ar", str(SR), "-"], capture_output=True, check=True).stdout
    f = np.frombuffer(raw, dtype=np.float32).astype(np.float64).reshape(-1, 2)[:n]
    tp = 20 * np.log10(np.maximum(true_peak(f), 1e-9))
    blk = SR // 100
    nb = int(math.ceil(n / blk))
    tb = np.pad(tp, (0, nb * blk - len(tp)), constant_values=-120).reshape(nb, blk).max(axis=1)
    ex = np.maximum(tb - target_db, 0.0)
    ex = ndimage.maximum_filter1d(ex, size=21)
    ex = ndimage.uniform_filter1d(ex, size=5)
    spots = [round(i / 100, 2) for i in np.flatnonzero((tb - target_db) > 0)[:20]]
    return np.interp(np.arange(n), np.arange(nb) * blk + blk / 2, ex), spots


def freeze_sources(tl: dict, tmp: Path) -> None:
    """Copy (reflink) every footage file into the build's scratch dir and read only those copies, so a file
    re-rendered while a build runs can't change it halfway. A copy that does not open (caught mid-write) turns
    its shot back into a placeholder slate, said in the shot's source."""
    d = tmp / "src"
    d.mkdir(exist_ok=True)
    for s in tl["shots"]:
        src = s["source"]
        if src["type"] not in ("video", "burst"):
            continue
        for key, fkey in (("path", "frozen"), ("sound", "frozen_sound"), ("a", "frozen_a")):
            if not src.get(key):
                continue
            dst = d / f"{s['id']}-{Path(src[key]).name}"
            subprocess.run(["cp", "--reflink=auto", str(FILM / src[key]), str(dst)], check=True)
            src[fkey] = str(dst)
        if not timeline.probe(Path(src["frozen"])).get("width"):
            log(f"warning: {s['id']}: {src['path']} would not open (being written?); a slate instead")
            s["source"] = {"type": "slate", "status": "placeholder", "unreadable": src["path"]}


def keep_inputs(tl: dict, cfg: dict, config_path: Path, out: Path) -> None:
    """Every file this build reads, kept in OUT/inputs/ at its path under FILM_ROOT (reflinks where the disk
    has them), with inputs/MD5SUMS: the footage, the overlays and art files, the voice, the score and its clock,
    the effects stem, the config (and what it extends), the script, and the edit's own code. Files from the
    repository's film/ (the config, the script, the code) go under inputs/source/ when FILM_ROOT is elsewhere."""
    files: set[str] = set()
    src_root = PIPELINE.parent.resolve()
    def add(p) -> None:
        if not p:
            return
        q = (Path(p) if Path(p).is_absolute() else FILM / p).resolve()
        if not q.is_file():
            return
        if str(q).startswith(str(FILM.resolve()) + "/"):
            files.add(str(q.relative_to(FILM.resolve())))
        elif str(q).startswith(str(src_root) + "/"):
            files.add("source/" + str(q.relative_to(src_root)))
    for s in tl["shots"]:
        src = s["source"]
        for k in ("path", "sound", "a", "b"):
            add(src.get(k))
        for pc in src.get("compose") or []:
            add(pc.get("wav"))
        for o in s["overlays"]:
            add(o.get("path"))
            add(o.get("sound"))
        for v in (s.get("card_opts") or {}).values():
            for x in (v if isinstance(v, list) else [v]):
                if isinstance(x, dict):
                    for y in list(x.get("c") or []) + list(x.get("port") or []):
                        add(y)
                elif isinstance(x, str) and x.endswith(".ppm"):
                    add(x)
    for v in tl["voice"]:
        add(v.get("file"))
    m = tl.get("music") or {}
    for k in ("path", "clock"):
        add(m.get(k))
    if m.get("path"):
        add(str(Path(m["path"]).with_suffix(".json")))
    stem = find_sfx(tl, cfg, tl["frames"] * SPF)
    if stem.get("path"):
        add(stem["path"])
        add(str(Path(stem["path"]).with_suffix(".json")))
    c = config_path
    while c:
        add(c)
        ext = timeline.tomllib.loads(Path(c).read_text()).get("extends")
        c = FILM / ext if ext else None
    pa = cfg.get("paths", {})
    for k in ("shots", "script", "lines"):
        add(pa.get(k))
    for f in (pa.get("inherit") if isinstance(pa.get("inherit"), list) else [pa.get("inherit")]):
        add(f)
    for f in (pa.get("section_rows_from") or {}).values():
        add(f)
    for f in ("voice/clips.json", "voice/clips-v2.json", "voice/clips-v3.json", "voice/clips-v4.json",
              timeline.SHOTS_MD, timeline.DIAGRAMS_MD):
        add(f)
    for f in (EDIT / "build.py", EDIT / "timeline.py", EDIT / "cards.py", EDIT / "qglyph.py", PIPELINE / "filmroot.py",
              *sorted(QKIT.glob("*.py"))):
        add(f)
    keep = out / "inputs"
    for rel_ in sorted(files):
        dst = keep / rel_
        dst.parent.mkdir(parents=True, exist_ok=True)
        src = src_root / rel_[len("source/"):] if rel_.startswith("source/") else FILM / rel_
        subprocess.run(["cp", "--reflink=auto", str(src), str(dst)], check=True)
    sums = subprocess.run(["md5sum", *sorted(files)], cwd=keep, capture_output=True, text=True, check=True).stdout
    (keep / "MD5SUMS").write_text(sums)
    log(f"inputs: {len(files)} files kept in {keep}")


PHONE_FPS, PHONE_AUDIO_K = 30, 64   # a phone file under 10 MB: at 60 fps the picture gets ~121 kbit/s and Quake's
                                    # textures smear; 30 fps, aq-mode 3, AAC 64k
PHONE_VF = f"fps={PHONE_FPS},scale=-2:480:flags=area"
BT709 = ["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]


def check_duration(p: Path, duration: float) -> None:
    """A preview must be the whole film (one was truncated at 06:47): its duration within 0.1 s of the film's."""
    r = sh(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(p)])
    d = float(r.stdout.strip() or 0)
    if abs(d - duration) > 0.1:
        raise RuntimeError(f"{p.name}: {d:.3f} s, the film is {duration:.3f} s")
    log(f"  {p.name}: {d:.3f} s, the film's length")


def _phone_audio(film: Path, out: Path) -> Path:
    """The film's sound for the phone file: limited to -2 dBTP, then AAC at 64k, which overshoots by up to 3.4 dB on
    this film: where the decoded AAC still peaks over -1.3 dBTP, the sound is turned down there alone and encoded
    again (as the master's codec fix). An .m4a in scratch, muxed as it is (-c:a copy)."""
    m4a = SCRATCH / f"{out.stem}-audio.m4a"
    wav = SCRATCH / f"{out.stem}-audio.wav"
    raw = SCRATCH / f"{out.stem}-audio.f32"
    sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(film), "-vn", "-ac", "2", "-ar", str(SR), "-f", "f32le", str(raw)])
    x = np.fromfile(raw, dtype=np.float32).astype(np.float64).reshape(-1, 2)
    raw.unlink()
    y, gr = limit(x, -2.0)
    fixes = 0
    # ffmpeg's AAC at 64k: its perceptual noise substitution and intensity stereo add up to 1.5 dB of overshoot here
    aac = ["-c:a", "aac", "-b:a", f"{PHONE_AUDIO_K}k", "-aac_pns", "0", "-aac_is", "0"]
    for attempt in range(10):
        sf.write(str(wav), y, SR, subtype="PCM_24")
        sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(wav), *aac, str(m4a)])
        tp = ebur128(m4a)["true_peak"]
        if tp <= -1.1:
            break
        if attempt == 9:  # still over: the whole sound down by the excess (a fraction of a dB), once more
            y *= 10 ** ((-1.3 - tp) / 20)
        else:
            env, spots = codec_overshoot(m4a, len(y), -1.3)
            y *= 10 ** (-env / 20)[:, None]
        fixes += 1
    if tp > -1.1:
        sf.write(str(wav), y, SR, subtype="PCM_24")
        sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(wav), *aac, str(m4a)])
        tp = ebur128(m4a)["true_peak"]
    wav.unlink()
    log(f"  phone audio: limited to -2 dBTP ({gr:.2f} dB at most), AAC {PHONE_AUDIO_K}k at {tp:.2f} dBTP after {fixes} local fixes")
    return m4a


def _phone_encode(film: Path, out: Path, duration: float, limit_mb: float, codec: str) -> None:
    """Two passes at the bitrate that fits limit_mb with AAC 64k; under 10 MB, or the bitrate comes down and it
    encodes again (x265 overshoots: 140k once came to 9,999,989 bytes). The sound limited to -2 dBTP first;
    the file's true peak is read back (it must be at most -1.0 dBTP)."""
    wav = _phone_audio(film, out)
    for attempt in range(3):
        kbps = int((limit_mb * 8 * 1000) / duration) - PHONE_AUDIO_K - 8
        if codec == "hevc":
            stats = str(SCRATCH / f"{out.stem}-x265.log")
            common = ["-vf", PHONE_VF, "-pix_fmt", "yuv420p", "-c:v", "libx265", "-preset", "slow", "-profile:v", "main",
                      "-b:v", f"{kbps}k", "-tag:v", "hvc1", *BT709]
            xp = f"log-level=error:stats={stats}:aq-mode=3:colorprim=bt709:transfer=bt709:colormatrix=bt709"
            p1 = ["-x265-params", xp + ":pass=1"]
            p2 = ["-x265-params", xp + ":pass=2"]
        else:
            log_base = str(SCRATCH / f"{out.stem}-2pass")  # the pass logs stay out of the published folder
            common = ["-vf", PHONE_VF, "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "slower", "-profile:v", "high",
                      "-b:v", f"{kbps}k", "-aq-mode", "3", *BT709,
                      "-x264-params", "colorprim=bt709:transfer=bt709:colormatrix=bt709"]
            p1 = ["-pass", "1", "-passlogfile", log_base]
            p2 = ["-pass", "2", "-passlogfile", log_base]
        sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(film), *common, *p1, "-an", "-f", "null", "/dev/null"])
        sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(film), "-i", str(wav), "-map", "0:v", "-map", "1:a",
            *common, *p2, "-c:a", "copy", "-shortest", "-movflags", "+faststart", str(out)])
        mb = out.stat().st_size / 1e6
        log(f"preview ({codec}, {PHONE_FPS} fps): {out} ({mb:.2f} MB at {kbps} kbit/s)")
        if mb < 9.9:
            check_duration(out, duration)
            tp = ebur128(out)["true_peak"]
            log(f"  {out.name}: true peak {tp:.2f} dBTP")
            if tp > -1.0:
                raise RuntimeError(f"{out.name}: true peak {tp:.2f} dBTP, over -1.0")
            wav.unlink()
            return
        limit_mb *= 9.6 / mb
    raise RuntimeError(f"{out.name}: still {mb:.2f} MB after 3 tries")


def preview_480p(film: Path, out: Path, duration: float, limit_mb: float = 9.5) -> None:
    """The H.264 fallback: 854x480 at 30 fps, x264 slower, aq-mode 3, two passes (~138k on a 6:02 film), AAC 64k."""
    _phone_encode(film, out, duration, limit_mb, "h264")


def preview_480p_hevc(film: Path, out: Path, duration: float, limit_mb: float = 9.2) -> None:
    """The phone file: 854x480 at 30 fps, HEVC Main (hvc1), x265 slow, aq-mode 3, two passes (~132k on a 6:02
    film), AAC 64k, faststart, tagged bt709. It keeps the torch flicker that H.264 at 480p erases."""
    _phone_encode(film, out, duration, limit_mb, "hevc")


def next_build_dir(base: Path) -> Path:
    k = 1
    while (base / f"build-{k:03d}").exists():
        k += 1
    return base / f"build-{k:03d}"


def main() -> None:
    ap = argparse.ArgumentParser(description="render the film from its parts")
    ap.add_argument("--config", default=str(timeline.CONFIG), help="the edit's config (default: film/edit.toml)")
    ap.add_argument("--publish", help="also copy the result to OUT/NAME.mp4 (+ -contact.png, -loudness.md)")
    ap.add_argument("--no-burn-in", dest="burn_in", action="store_false", help="no timecode or shot ids burned in")
    ap.add_argument("--preview", action="store_true", help="also a 480p phone preview under 10 MB (NAME-480p.mp4)")
    ap.add_argument("--jobs", type=int, default=4, help="segments encoded at once")
    ap.add_argument("--procs", type=int, default=8, help="processes drawing the art")
    ap.add_argument("--remix", action="store_true", help="the sound only: the newest earlier build's picture, if this "
                    "config draws exactly the same picture (its segments.json), with a new mix (a score or stem landed)")
    ap.add_argument("--range", help="only a stretch of the film, A-B in film seconds or m:ss (an experiment's A/B): "
                    "the shots it touches, the whole film's mix sliced; written as OUT/build-NNN/range.mp4")
    ap.add_argument("--scale", type=int, default=1, help="the frame is SCALE x 1920x1080 (2: 3840x2160); every card, "
                    "label and position is drawn at SCALE x its 1080 geometry")
    ap.add_argument("--root", help="the film's media tree (FILM_ROOT) for this build")
    ap.add_argument("--hw", choices=["vaapi", "none"], default="none",
                    help="vaapi: encode the segments, the master and the 1080p copy on the GPU (VAAPI_DEVICE, default "
                         "/dev/dri/renderD128); none: in software")
    ap.add_argument("--publish-to", help="the folder the published files go to (default: the config's paths.out)")
    a = ap.parse_args()

    global S, HW, CODE_SIG
    S, HW = a.scale, a.hw
    cards.set_scale(S)
    CODE_SIG = digest(CODE_SIG, "scale", S)  # the art is drawn at this scale
    t0 = time.time()
    cfg = timeline.load_config(a.config)
    base = FILM / cfg.get("paths", {}).get("out", "edit")
    pub = Path(a.publish_to).resolve() if a.publish_to else base
    tl = timeline.build(cfg)
    print(timeline.report(tl))
    base.mkdir(parents=True, exist_ok=True)
    out = next_build_dir(base)
    out.mkdir()
    tmp = SCRATCH / f"{base.name}-{out.name}"  # v7-build-NNN
    tmp.mkdir(parents=True, exist_ok=False)
    (out / "timeline.json").write_text(json.dumps(tl, indent=1, ensure_ascii=False))
    if a.range:
        return build_range(a, cfg, tl, out, tmp, t0)
    (base / "timeline.json").write_text(json.dumps(tl, indent=1, ensure_ascii=False))
    (out / "clock.txt").write_text(timeline.report(tl) + "\n\n" + timeline.shot_table(tl) + "\n")
    if cfg.get("paths", {}).get("format") in ("v2", "v3"):
        timeline.write_clock(tl, cfg, base)
        timeline.write_ladder_events(tl, cfg, base)
    log(f"build {out.name}: {tl['frames']} frames, {tl['duration']:.2f} s, {W * S}x{H * S}, hw {HW}")

    keep_inputs(tl, cfg, Path(a.config).resolve(), out)
    freeze_sources(tl, tmp)
    (out / "timeline.json").write_text(json.dumps(tl, indent=1, ensure_ascii=False))
    art, jobs = plan_art(tl, a.burn_in)
    render_art(jobs, a.procs)  # cached art is not redrawn; the segments' names need the art files
    seg_names = [p.name for p in segment_paths(tl, art, a.burn_in)]
    prev_video = None
    if a.remix:  # the newest earlier build whose picture is this one, segment for segment
        roots = [base] + ([FILM / cfg["paths"]["remix_from"]] if cfg.get("paths", {}).get("remix_from") else [])
        for b in [b for r in roots for b in sorted(r.glob("build-[0-9][0-9][0-9]"), reverse=True)]:
            sj = b / "segments.json"
            if b != out and sj.exists() and (b / "film.mp4").exists() and json.loads(sj.read_text()) == seg_names:
                prev_video = b / "film.mp4"
                break
        if prev_video is None:
            sys.exit("--remix: no earlier build draws this picture (the config changed the picture): run a full build")
        log(f"remix: the picture is {prev_video.relative_to(FILM)}'s")
    (out / "segments.json").write_text(json.dumps(seg_names))
    if prev_video is None:
        paths = render_segments(tl, art, a.burn_in, a.jobs)

    n = tl["frames"] * SPF
    log("audio")
    rep, stems = mix_audio(tl, cfg, n, out, tmp)

    log("encode")
    video = tmp / "video.mp4"
    if prev_video is None:
        encode_video(paths, video, a.burn_in, tmp)
    else:
        sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(prev_video), "-map", "0:v", "-c:v", "copy", str(video)])
    got = count_frames(video)
    log(f"picture: {got} frames (want {tl['frames']})")
    if got != tl["frames"]:
        print(f"WARNING: frame count {got} != {tl['frames']}", file=sys.stderr)

    # master, mux, measure the encoded file. AAC overshoots most where the mix is dense and limited (the
    # montage's metal): where the decoded film still peaks over the limit, turn the master down there
    # alone, a little more than the overshoot, and encode again. Loudness elsewhere is untouched.
    film = out / "film.mp4"
    lv = cfg["levels"]
    post = master(stems, lv, lv["limiter_ceiling"], tmp, rep, tl)
    (out / "stems").mkdir()   # the four buses after the master's gain, before its limiter (review/kit/README.md)
    for k in ("voice", "music", "game", "sfx"):
        sf.write(str(out / "stems" / f"{k}.flac"), post.get(k, np.zeros((n, 2))), SR, subtype="PCM_24")
    del post
    y, _ = sf.read(str(tmp / "master.wav"), dtype="float64")
    rep["codec_fixes"] = []
    for attempt in range(5):
        mux(video, tmp / "master.wav", film, int(lv.get("aac_kbps", 256)))
        final = ebur128(film)
        log(f"the film: {final['integrated']:.2f} LUFS, TP {final['true_peak']:.2f} dBTP")
        if final["true_peak"] <= lv["master_tp"] or attempt == 4:
            break
        env, spots = codec_overshoot(film, len(y), lv["master_tp"] - 0.3)
        rep["codec_fixes"].append(spots)
        y *= 10 ** (-env / 20)[:, None]
        sf.write(str(tmp / "master.wav"), y, SR, subtype="PCM_24")
    (out / "mix.json").write_text(json.dumps(rep, indent=1))

    film_1080 = None
    if S > 1:  # the 1080p copy: the same segments, area-averaged down, with the master's sound as it was muxed
        log("encode: the 1080p copy")
        v1080 = tmp / "video-1080p.mp4"
        if prev_video is None:
            encode_video(paths, v1080, a.burn_in, tmp, height=H)
        else:
            sh(["ffmpeg", "-y", "-loglevel", "error", "-i", str(prev_video.with_name("film-1080p.mp4")), "-map", "0:v",
                "-c:v", "copy", str(v1080)])
        film_1080 = out / "film-1080p.mp4"
        mux(v1080, tmp / "master.wav", film_1080, int(lv.get("aac_kbps", 256)))

    log("loudness, contact sheet")
    master_m = ebur128(tmp / "master.wav")
    (out / "loudness.md").write_text(loudness_md(rep, final, master_m, tl, cfg))
    contact_sheet(film_1080 or film, tl, out / "contact.png", tmp)
    if a.publish:
        pub.mkdir(parents=True, exist_ok=True)
        files = [(film, pub / f"{a.publish}.mp4"), (out / "contact.png", pub / f"{a.publish}-contact.png"),
                 (out / "loudness.md", pub / f"{a.publish}-loudness.md")]
        if film_1080:
            files.append((film_1080, pub / f"{a.publish}-1080p.mp4"))
        for src, dst in files:
            sh(["cp", "--reflink=auto", str(src), str(dst)])
        if a.preview:  # the phone files, from the 1080p copy when there is one (the same picture, a quarter to decode)
            src_ = film_1080 or film
            preview_480p_hevc(src_, pub / f"{a.publish}-480p30-hevc.mp4", tl["duration"])   # the phone file
            preview_480p(src_, pub / f"{a.publish}-480p30.mp4", tl["duration"])             # the H.264 fallback
    for f in (video, tmp / "video-1080p.mp4"):  # the picture lives on in film.mp4: at 4K its scratch copy is gigabytes
        f.unlink(missing_ok=True)
    log(f"done in {time.time() - t0:.0f} s: {film} ({final['integrated']:.1f} LUFS, TP {final['true_peak']:.1f} dBTP)")


def build_range(a, cfg: dict, tl: dict, out: Path, tmp: Path, t0: float) -> None:
    """--range A-B: the shots that touch [A, B] rendered and cut to it, with the whole film's mix (so its levels
    are the film's) sliced to the same stretch. Nothing beside the build folder is written."""
    lo_s, hi_s = (timeline.secs(x) if ":" in x else float(x) for x in a.range.split("-"))
    sel = [s for s in tl["shots"] if s["start"] < hi_s and s["start"] + s["len"] > lo_s]
    f0 = sel[0]["start"]
    log(f"range {lo_s:.2f}-{hi_s:.2f}: shots {sel[0]['id']}..{sel[-1]['id']}")
    freeze_sources(tl, tmp)
    art, jobs = plan_art({**tl, "shots": sel}, a.burn_in)
    render_art(jobs, a.procs)
    paths = render_segments(tl, art, a.burn_in, a.jobs, only={s["id"] for s in sel})
    video = tmp / "video.mp4"
    encode_video(paths, video, a.burn_in, tmp)
    n = tl["frames"] * SPF
    rep, stems = mix_audio(tl, cfg, n, out, tmp)
    master(stems, cfg["levels"], cfg["levels"]["limiter_ceiling"], tmp, rep, tl)
    y, _ = sf.read(str(tmp / "master.wav"), dtype="float64")
    sf.write(str(tmp / "range.wav"), y[int(lo_s * SR):int(hi_s * SR)], SR, subtype="PCM_24")
    sh(["ffmpeg", "-y", "-loglevel", "error", "-ss", f"{lo_s - f0:.4f}", "-t", f"{hi_s - lo_s:.4f}", "-i", str(video),
        "-i", str(tmp / "range.wav"), "-map", "0:v", "-map", "1:a", "-c:v", "libx264", "-preset", "slow", "-crf", "17",
        "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "320k", "-shortest", "-movflags", "+faststart", str(out / "range.mp4")])
    (out / "mix.json").write_text(json.dumps(rep, indent=1))
    log(f"done in {time.time() - t0:.0f} s: {out / 'range.mp4'}")


if __name__ == "__main__":
    main()
