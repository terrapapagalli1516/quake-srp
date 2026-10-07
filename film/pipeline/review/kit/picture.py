"""The picture: one decode of the whole cut (640x360, every frame), its statistics, the contact sheets,
the picture flags, the phone-size sheet of every captioned moment, and full-rate filmstrips of a range."""

from __future__ import annotations

import json
import math
import subprocess
import threading
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

from common import FILM, Flags, ensure_dir, font, log, md_table, rel, tc

PW, PH = 640, 360            # the decode size: also "a phone, sideways", the size text is judged at
VAAPI = "/dev/dri/renderD128"
GOLD, DIM, RED, CYAN, WHITE = (230, 180, 60), (150, 150, 150), (235, 70, 60), (80, 200, 230), (240, 240, 240)
BG = (18, 18, 20)


# ------------------------------------------------------------- the decode ----

def _vf(hw: bool, w: int, h: int) -> str:
    conv = f"scale=in_color_matrix=bt709:in_range=tv:flags=fast_bilinear,format=rgb24"
    if hw:
        return f"scale_vaapi=w={w}:h={h}:format=nv12,hwdownload,format=nv12,{conv}"
    return f"scale={w}:{h}:flags=area:in_color_matrix=bt709:in_range=tv,format=rgb24"


def decode_cmd(cut: Path, a: int, n: int, fps: float, hw: bool, w: int = PW, h: int = PH, every: int = 1) -> list[str]:
    """ffmpeg reading frames [a, a+n) of the cut as raw RGB, frame-exact (seek to half a frame before a)."""
    ss = ["-ss", f"{(a - 0.5) / fps:.6f}"] if a > 0 else []
    pre = ["-hwaccel", "vaapi", "-hwaccel_device", VAAPI, "-hwaccel_output_format", "vaapi"] if hw else ["-threads", "3"]
    vf = _vf(hw, w, h)
    if every > 1:
        vf = f"select='not(mod(n\\,{every}))',{vf}"
    return ["ffmpeg", "-v", "error", "-nostdin", *pre, *ss, "-i", str(cut), "-frames:v", str(n if every == 1 else math.ceil(n / every)),
            "-an", "-sn", "-vf", vf, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]


_HW: bool | None = None


def hw_ok(cut: Path, fps: float) -> bool:
    global _HW
    if _HW is None:
        try:
            r = subprocess.run(decode_cmd(cut, 0, 1, fps, True), capture_output=True, timeout=30)
            _HW = r.returncode == 0 and len(r.stdout) == PW * PH * 3
        except Exception:  # noqa: BLE001
            _HW = False
        log("decode:", "VAAPI (the GPU)" if _HW else "software")
    return _HW


def read_frames(cut: Path, a: int, n: int, fps: float, w: int, h: int, every: int = 1):
    """Yield (frame index, HxWx3 uint8) for frames a.. a+n-1 (every Nth)."""
    hw = hw_ok(cut, fps)
    p = subprocess.Popen(decode_cmd(cut, a, n, fps, hw, w, h, every), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    fb = w * h * 3
    k = 0
    try:
        while True:
            b = p.stdout.read(fb)
            if len(b) < fb:
                break
            yield a + k * every, np.frombuffer(b, np.uint8).reshape(h, w, 3)
            k += 1
    finally:
        p.stdout.close()
        p.wait()


STAT_KEYS = ["mean", "std", "p02", "p98", "r", "g", "b", "flat"]   # flat: share of pixels within 5 of the median


def decode_pass(cut: Path, nframes: int, fps: float, want: set[int], frames_dir: Path, workers: int = 4) -> dict:
    """Every frame once: per-frame luma statistics and a 80x45 luma thumbnail; the wanted frames saved as
    640x360 JPEGs (frames/NNNNNN.jpg). Frame-exact in parallel chunks."""
    ensure_dir(frames_dir)
    st = np.zeros((nframes, len(STAT_KEYS)), np.float32)
    tiny = np.zeros((nframes, 45, 80), np.uint8)
    seen = np.zeros(nframes, bool)
    hw_ok(cut, fps)
    bounds = np.linspace(0, nframes, workers + 1).astype(int)
    errs: list[str] = []

    def work(a: int, b: int) -> None:
        try:
            for i, arr in read_frames(cut, a, b - a, fps, PW, PH):
                if i >= nframes:
                    break
                sm = arr[::4, ::4].astype(np.float32)
                y = sm[..., 0] * 0.2126 + sm[..., 1] * 0.7152 + sm[..., 2] * 0.0722
                yf = y.ravel()
                k2, k50, k98 = int(0.02 * yf.size), yf.size // 2, int(0.98 * yf.size)
                part = np.partition(yf, [k2, k50, k98])
                lo, med, hi = part[k2], part[k50], part[k98]
                m = sm.reshape(-1, 3).mean(0)
                st[i] = (yf.mean(), yf.std(), lo, hi, m[0], m[1], m[2], float((np.abs(yf - med) < 5).mean()))
                tiny[i] = np.clip(y[::2, ::2], 0, 255).astype(np.uint8)
                seen[i] = True
                if i in want:
                    Image.fromarray(arr).save(frames_dir / f"{i:06d}.jpg", quality=93)
        except Exception as ex:  # noqa: BLE001
            errs.append(f"{a}-{b}: {ex}")

    th = [threading.Thread(target=work, args=(int(bounds[k]), int(bounds[k + 1]))) for k in range(workers)]
    for t in th:
        t.start()
    for t in th:
        t.join()
    if errs:
        log("decode errors:", errs)
    miss = int((~seen).sum())
    if miss:
        log(f"decode: {miss} frames not read")
    return {"stats": st, "tiny": tiny, "seen": seen}


def frame_path(frames_dir: Path, i: int) -> Path:
    return frames_dir / f"{i:06d}.jpg"


def get_frame(cut: Path, frames_dir: Path, i: int, fps: float) -> Image.Image:
    p = frame_path(frames_dir, i)
    if p.exists():
        return Image.open(p).convert("RGB")
    for _, arr in read_frames(cut, i, 1, fps, PW, PH):
        im = Image.fromarray(arr.copy())
        im.save(p, quality=93)
        return im
    return Image.new("RGB", (PW, PH), (60, 0, 60))


# --------------------------------------------------------- what to save ----

def shot_frames(s: dict) -> tuple[int, int, int]:
    a = int(s["frame"])
    n = int(s["frames"])
    return a, a + n // 2, a + n - 1


def overview_frames(nframes: int, fps: float, every_s: float) -> list[int]:
    step = max(1, int(round(every_s * fps)))
    return list(range(0, nframes, step))


# ------------------------------------------------------- text and phones ----

CAP = 6  # a conchars capital is 6 of its 8 rows (measured in the cut: 18 px at 3x; the 7th row is the descenders')


def text_moments(edit) -> list[dict]:
    """The captioned moments the timeline knows of: one frame per text overlay (and per card shot), with
    the text's cap height at phone size where the overlay's kind says it (Quake's 8x8 font times its
    scale), and the overlay file to measure where it does not."""
    out = []
    k = 640 / 1920
    for s in edit.shots:
        st, ln = s["start"], s["len"]
        src = s["source"]
        if src.get("type") == "card" and src.get("name") not in ("black",):
            out.append({"t": st + 0.6 * ln, "shot": s["id"], "what": f"card {src.get('name')}", "cap": None})
        for o in s.get("overlays", []):
            ty = o.get("type")
            at = float(o.get("at") or 0.0)
            if ty == "label" or (ty == "card" and o.get("name") == "label"):
                sc = int(o.get("scale", 3))
                w = max(len(o.get("text", "")) * 8 * sc, int(o.get("min_w", 0)))
                box = (int(o.get("x", 40)), int(o.get("y", 40)), w, 8 * sc)
                out.append({"t": st + at + 0.55 * (ln - at), "shot": s["id"], "what": f"label \"{o.get('text', '')}\" ({sc}x)",
                            "cap": CAP * sc * k, "box": box})
            elif ty == "caption":
                sub = o.get("sub")
                n = len(o.get("text", ""))
                block = 32 + (28 if sub else 0)
                box = (96, 1080 - 120 - block, max(n * 32, len(sub or "") * 16), block)
                cap = CAP * (2 if sub else 4) * k
                out.append({"t": st + at + min(1.0, 0.5 * (ln - at)), "shot": s["id"],
                            "what": f"caption \"{o.get('text', '')}\"" + (f" / \"{sub}\" (2x)" if sub else " (4x)"),
                            "cap": cap, "box": box})
            elif ty == "bumper":
                hold = float(o.get("hold", 1.5))
                sc = int(o.get("scale", 10))
                out.append({"t": st + at + min(0.8, hold / 2, 0.5 * (ln - at)), "shot": s["id"],
                            "what": f"bumper \"{o.get('text', '')}\" ({sc}x)", "cap": CAP * sc * k})
            elif ty == "card":
                out.append({"t": st + 0.6 * ln, "shot": s["id"], "what": f"overlay card {o.get('name')}", "cap": None})
            elif ty == "png" and o.get("path"):
                out.append({"t": min(st + ln - 0.1, st + at + 0.5), "shot": s["id"], "what": f"png {Path(o['path']).name}",
                            "cap": None, "measure": {"kind": "png", "path": o["path"], "x": 0, "y": 0, "scale": 1.0}})
            elif ty == "video" and o.get("path") and not o.get("sound") and "footage/" not in o["path"]:
                ins = float(o.get("in", 0.0))
                for f in (0.5, 0.9):
                    t = st + f * ln
                    out.append({"t": t, "shot": s["id"], "what": f"overlay {Path(o['path']).stem}", "cap": None,
                                "measure": {"kind": "video", "path": o["path"], "at": ins + (t - st), "t_rel": t - st,
                                            "x": int(o.get("x", 0)), "y": int(o.get("y", 0)),
                                            "scale": float(o.get("scale", 1.0)), "panel": o.get("panel")}})
    out.sort(key=lambda m: m["t"])
    # one frame per moment, at least 0.25 s apart within a shot
    keep = []
    for m in out:
        if keep and keep[-1]["shot"] == m["shot"] and abs(keep[-1]["t"] - m["t"]) < 0.25:
            keep[-1]["what"] += " + " + m["what"]
            for key in ("cap", "box", "measure"):
                if keep[-1].get(key) is None and m.get(key) is not None:
                    keep[-1][key] = m[key]
            if m.get("cap") is not None and keep[-1].get("cap") is not None:
                keep[-1]["cap"] = min(keep[-1]["cap"], m["cap"])
            continue
        keep.append(dict(m))
    for m in keep:
        m["frame"] = int(round(m["t"] * edit.fps))
    return keep


def _overlay_rgba(m: dict) -> np.ndarray | None:
    p = FILM / m["path"]
    if not p.exists():
        return None
    if m["kind"] == "png":
        return np.asarray(Image.open(p).convert("RGBA"))
    from common import probe
    info = probe(p)
    w, h = info["width"], info["height"]
    at = max(0.0, min(float(m["at"]), info["duration"] - 0.05))
    r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-ss", f"{at:.4f}", "-i", str(p), "-frames:v", "1",
                        "-f", "rawvideo", "-pix_fmt", "rgba", "-"], capture_output=True)
    if len(r.stdout) != w * h * 4:
        return None
    return np.frombuffer(r.stdout, np.uint8).reshape(h, w, 4)


def text_lines(rgba: np.ndarray) -> list[dict]:
    """Lines of text in an overlay: bright, opaque strokes grouped into rows of similar-height marks.
    Returns each line's box (x, y, w, h) and its letter height, in the overlay's pixels."""
    from scipy import ndimage
    a = rgba[..., 3].astype(np.float32) / 255
    y = rgba[..., 0] * 0.2126 + rgba[..., 1] * 0.7152 + rgba[..., 2] * 0.0722
    mask = (a > 0.45) & (y * a > 70)
    lab, n = ndimage.label(mask, structure=np.ones((3, 3)))
    if n == 0:
        return []
    objs = ndimage.find_objects(lab)
    areas = ndimage.sum(mask, lab, index=np.arange(1, n + 1))
    comps = []
    for i, sl in enumerate(objs):
        h = sl[0].stop - sl[0].start
        w = sl[1].stop - sl[1].start
        if h < 4 or h > 140 or areas[i] < 8:
            continue
        fill = areas[i] / (h * w)
        if fill < 0.12 or (w > 2.5 * h and not (0.15 <= fill <= 0.75 and w <= 45 * h)):
            continue
        comps.append([sl[1].start, sl[0].start, w, h])
    if not comps:
        return []
    c = np.array(comps, float)
    # union marks into lines: same height (within 1.6x), same row, close beside each other
    par = list(range(len(c)))

    def find(i):
        while par[i] != i:
            par[i] = par[par[i]]
            i = par[i]
        return i
    order = np.argsort(c[:, 0])
    cy = c[:, 1] + c[:, 3] / 2
    for ii, i in enumerate(order):
        for j in order[ii + 1: ii + 40]:
            hi, hj = c[i, 3], c[j, 3]
            hm = max(hi, hj)
            gap = c[j, 0] - (c[i, 0] + c[i, 2])
            if gap > 1.4 * hm:
                continue
            if abs(cy[i] - cy[j]) < 0.35 * hm and 1 / 1.6 < hi / hj < 1.6:
                par[find(i)] = find(j)
    groups: dict[int, list[int]] = {}
    for i in range(len(c)):
        groups.setdefault(find(i), []).append(i)
    lines = []
    for g in groups.values():
        wide = len(g) == 1 and c[g[0], 2] >= 2.5 * c[g[0], 3]
        if len(g) < 3 and not wide:
            continue
        x0 = c[g, 0].min()
        y0 = c[g, 1].min()
        x1 = (c[g, 0] + c[g, 2]).max()
        y1 = (c[g, 1] + c[g, 3]).max()
        lines.append({"box": [int(x0), int(y0), int(x1 - x0), int(y1 - y0)], "h": float(np.median(c[g, 3])), "marks": len(g)})
    return lines


def measure_text(m: dict) -> dict:
    """The smallest text line of an overlay, at phone size (640 wide), and its boxes in film pixels."""
    meas = m.get("measure")
    if not meas:
        return {}
    rgba = _overlay_rgba(meas)
    if rgba is None:
        return {"note": "overlay not readable"}
    lines = text_lines(rgba)
    if not lines:
        return {"note": "no text found in the overlay"}
    x0, y0, sc = placement(meas)
    note = f"a moving panel, here at {sc:.2f}x" if meas.get("panel") else ""
    boxes = [[x0 + b["box"][0] * sc, y0 + b["box"][1] * sc, b["box"][2] * sc, b["box"][3] * sc] for b in lines]
    # a line wholly off the screen (a panel parked half outside) is not seen: leave it out
    vis = [(bx, b) for bx, b in zip(boxes, lines) if bx[0] + bx[2] > 0 and bx[1] + bx[3] > 0 and bx[0] < 1920 and bx[1] < 1080]
    if not vis:
        return {"note": "the overlay's text is off screen here"}
    hs = sorted(b["h"] * sc * 640 / 1920 for _, b in vis)
    return {"cap_min": hs[0], "cap_median": float(np.median(hs)), "lines": len(vis),
            "small_lines": sum(1 for h in hs if h < 6.0), "boxes": [bx for bx, _ in vis], "note": note}


def placement(meas: dict) -> tuple[float, float, float]:
    """Where build.py puts a video overlay at this moment: x, y and scale (a `panel` moves and grows)."""
    if meas.get("panel"):
        (px, py, ps), (fa, fb, fd) = meas["panel"]["to"], meas["panel"]["full"]

        def ease(u: float) -> float:
            return 4 * u ** 3 if u < 0.5 else 1 - (-2 * u + 2) ** 3 / 2
        t = float(meas.get("t_rel", 0.0))
        ui = min(1.0, max(0.0, (t - fa) / fd))
        uo = min(1.0, max(0.0, (t - (fb - fd)) / fd))
        mm = ease(ui) * (1 - ease(uo))
        return px * (1 - mm), py * (1 - mm), ps + (1 - ps) * mm
    return float(meas.get("x", 0)), float(meas.get("y", 0)), float(meas.get("scale") or 1.0)


SAFE = (96, 54, 1824, 1026)   # EBU R95 graphics-safe (90%) in 1920x1080


def outside_safe(box) -> bool:
    x, y, w, h = box
    return x < SAFE[0] - 1 or y < SAFE[1] - 1 or x + w > SAFE[2] + 1 or y + h > SAFE[3] + 1


# ------------------------------------------------------------------ sheets ----

def _label(d: ImageDraw.ImageDraw, xy, text, size=14, fill=WHITE, bold=False):
    d.text(xy, text, font=font(size, bold), fill=fill)


def _wrap(text: str, width: int) -> list[str]:
    import textwrap
    return textwrap.wrap(text, width) or [""]


def shot_sheets(edit, frames_dir: Path, out: Path, rows_per_page: int = 8) -> list[Path]:
    """Per section: one row per shot, its first, middle and last frame, with id and film times."""
    tw, th = 384, 216
    lw = 380
    pad = 10
    paths = []
    by_sec: dict[str, list[dict]] = {}
    for s in edit.shots:
        by_sec.setdefault(s.get("section", "?"), []).append(s)
    for sec in edit.sections or [{"id": "?", "title": "", "start": 0, "len": edit.duration}]:
        shots = by_sec.get(sec["id"], [])
        pages = [shots[i:i + rows_per_page] for i in range(0, len(shots), rows_per_page)] or [[]]
        for pi, page in enumerate(pages):
            W = lw + 3 * (tw + pad) + pad
            rh = th + 30
            H = 56 + rh * len(page) + 10
            im = Image.new("RGB", (W, H), BG)
            d = ImageDraw.Draw(im)
            end = sec["start"] + sec.get("len", sec.get("end", 0) - sec["start"])
            _label(d, (pad, 12), f"{edit.cut.name} · {sec['id']} {sec.get('title', '')} · {tc(sec['start'])}–{tc(end)}"
                   + (f" · page {pi + 1}/{len(pages)}" if len(pages) > 1 else ""), 22, GOLD, True)
            for r, s in enumerate(page):
                y0 = 56 + r * rh
                a, mid, b = shot_frames(s)
                _label(d, (pad, y0), s["id"], 26, GOLD, True)
                _label(d, (pad, y0 + 34), f"{tc(s['start'], 3)} → {tc(s['start'] + s['len'], 3)}", 15, WHITE)
                _label(d, (pad, y0 + 54), f"{s['len']:.3f} s · {s['frames']} frames · {s.get('kind', '')}", 13, DIM)
                src = s.get("source", {})
                sname = Path(src.get("path", "")).name if src.get("path") else src.get("name", src.get("type", ""))
                _label(d, (pad, y0 + 72), f"src: {sname}"[:44], 13, DIM)
                yy = y0 + 92
                over = s.get("over") or ""
                if over:
                    for ln in _wrap(f"“{over}”", 40)[:4]:
                        _label(d, (pad, yy), ln, 13, CYAN)
                        yy += 17
                ovs = [o.get("name") or o.get("type") for o in s.get("overlays", [])]
                if ovs:
                    _label(d, (pad, yy), ("overlays: " + ", ".join(str(x) for x in ovs))[:46], 12, DIM)
                    yy += 16
                if s.get("mute"):
                    _label(d, (pad, yy), "MUTED (every bus but the voice)", 12, RED)
                for k, (fi, tag) in enumerate(((a, "first"), (mid, "middle"), (b, "last"))):
                    x0 = lw + k * (tw + pad)
                    fr = get_frame(edit.cut, frames_dir, fi, edit.fps).resize((tw, th), Image.LANCZOS)
                    im.paste(fr, (x0, y0))
                    _label(d, (x0, y0 + th + 4), f"{tag} · f{fi} · {tc(fi / edit.fps, 3)}", 13, DIM)
            p = out / (f"shots-{sec['id']}" + ("" if len(pages) == 1 else f"-p{pi + 1}") + ".jpg")
            im.save(p, quality=90)
            paths.append(p)
    return paths


def overview_sheets(edit, frames_dir: Path, out: Path, every_s: float, cols: int = 8, rows: int = 8) -> list[Path]:
    frames = overview_frames(edit.nframes, edit.fps, every_s)
    tw, th = 232, 130
    pad = 6
    per = cols * rows
    paths = []
    cuts = edit.cuts()
    for pi in range(0, len(frames), per):
        page = frames[pi:pi + per]
        W = cols * (tw + pad) + pad
        H = 44 + math.ceil(len(page) / cols) * (th + 22) + 6
        im = Image.new("RGB", (W, H), BG)
        d = ImageDraw.Draw(im)
        t0, t1 = page[0] / edit.fps, page[-1] / edit.fps
        _label(d, (pad, 10), f"{edit.cut.name} · a frame every {every_s:g} s · {tc(t0)}–{tc(t1)} · "
               f"page {pi // per + 1}/{math.ceil(len(frames) / per)} · a red bar: a cut since the previous tile", 18, GOLD, True)
        for k, fi in enumerate(page):
            x0 = pad + (k % cols) * (tw + pad)
            y0 = 44 + (k // cols) * (th + 22)
            fr = get_frame(edit.cut, frames_dir, fi, edit.fps).resize((tw, th), Image.LANCZOS)
            im.paste(fr, (x0, y0))
            t = fi / edit.fps
            prev = (fi - int(round(every_s * edit.fps))) / edit.fps
            if any(prev < c <= t + 1e-6 for c in cuts):
                d.rectangle([x0, y0, x0 + 4, y0 + th], fill=RED)
            _label(d, (x0, y0 + th + 3), f"{tc(t)}  {edit.shot_id_at(t)}", 13, WHITE)
        p = out / f"overview-{pi // per + 1:02d}.jpg"
        im.save(p, quality=88)
        paths.append(p)
    return paths


def phone_sheets(edit, frames_dir: Path, out: Path, moments: list[dict], cols: int = 2, rows: int = 3) -> list[Path]:
    """Each captioned moment at 640x360, 1:1 (a phone's view of a 1080p film), the 90% graphics-safe
    frame dashed, and what the timeline says of its text's size."""
    pad = 12
    per = cols * rows
    paths = []
    for pi in range(0, len(moments), per):
        page = moments[pi:pi + per]
        W = cols * (PW + pad) + pad
        rh = PH + 52
        H = 40 + math.ceil(len(page) / cols) * rh + 6
        im = Image.new("RGB", (W, H), BG)
        d = ImageDraw.Draw(im)
        _label(d, (pad, 10), f"{edit.cut.name} · captioned frames at phone size (640 wide, 1:1) · dashed: 90% graphics-safe · "
               f"page {pi // per + 1}/{math.ceil(len(moments) / per)}", 16, GOLD, True)
        for k, m in enumerate(page):
            x0 = pad + (k % cols) * (PW + pad)
            y0 = 40 + (k // cols) * rh
            fr = get_frame(edit.cut, frames_dir, m["frame"], edit.fps)
            im.paste(fr, (x0, y0))
            sx0, sy0, sx1, sy1 = (v / 3 for v in SAFE)
            for xa in range(int(sx0), int(sx1), 8):
                d.line([x0 + xa, y0 + sy0, x0 + xa + 3, y0 + sy0], fill=(255, 0, 255))
                d.line([x0 + xa, y0 + sy1, x0 + xa + 3, y0 + sy1], fill=(255, 0, 255))
            for ya in range(int(sy0), int(sy1), 8):
                d.line([x0 + sx0, y0 + ya, x0 + sx0, y0 + ya + 3], fill=(255, 0, 255))
                d.line([x0 + sx1, y0 + ya, x0 + sx1, y0 + ya + 3], fill=(255, 0, 255))
            cap = m.get("cap")
            col = RED if cap is not None and cap < 6 else (GOLD if cap is not None and cap < 7.5 else WHITE)
            capt = f"text {cap:.1f} px tall here" if cap is not None else "text size: look"
            _label(d, (x0, y0 + PH + 4), f"{tc(m['t'])} · f{m['frame']} · {m['shot']} · {capt}", 14, col, True)
            _label(d, (x0, y0 + PH + 24), m["what"][:86], 12, DIM)
        p = out / f"phone-{pi // per + 1:02d}.jpg"
        im.save(p, quality=93)
        paths.append(p)
    return paths


# ------------------------------------------------------------------- flags ----

def runs(mask: np.ndarray) -> list[tuple[int, int]]:
    """[a, b) runs of True."""
    if not mask.any():
        return []
    d = np.diff(np.concatenate([[0], mask.astype(np.int8), [0]]))
    return list(zip(np.where(d == 1)[0], np.where(d == -1)[0]))


def source_span(edit, s: dict, t0: float, t1: float) -> dict:
    """What a video shot's own source file shows over film times [t0, t1): its mean luma (limited range,
    black is 16) and median motion at 80x45; and the film time where the source ends (the build then
    holds its last frame)."""
    src = s.get("source", {})
    if src.get("type") != "video" or src.get("reverse") or not src.get("path"):
        return {}
    p = src["frozen"] if src.get("frozen") and Path(src["frozen"]).exists() else str(FILM / src["path"])
    sp = float(src.get("speed", 1.0))
    i0 = float(src.get("in", 0.0))
    dur = float((src.get("probe") or {}).get("duration") or 0.0)
    end = min(dur, float(src["out"])) if (src.get("out") is not None and dur) else dur
    out: dict = {"end_film": s["start"] + (end - i0) / sp if end else None}
    a, b = i0 + (t0 - s["start"]) * sp, i0 + (t1 - s["start"]) * sp
    if end:
        if a >= end - 1.5 / 60:
            out["ran_out"] = True
            return out
        b = min(b, end)
    if b - a < 2 / 60:
        return out
    r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-ss", f"{a:.4f}", "-i", p, "-t", f"{b - a:.4f}", "-an",
                        "-vf", "scale=80:45:flags=area,format=gray", "-f", "rawvideo", "-"], capture_output=True)
    fr = np.frombuffer(r.stdout, np.uint8)
    k = len(fr) // 3600
    if k < 2:
        return out
    fr = fr[: k * 3600].reshape(k, 45, 80).astype(np.int16)
    out["mean"] = float(fr.mean())
    out["motion"] = float(np.median(np.abs(fr[1:] - fr[:-1]).mean(axis=(1, 2))))
    return out


def picture_flags(edit, dec: dict, fl: Flags, frozen_min_s: float = 0.25) -> dict:
    st, tiny = dec["stats"], dec["tiny"]
    n = len(st)
    fps = edit.fps
    mean, std, p98 = st[:, 0], st[:, 1], st[:, 3]
    t16 = tiny.astype(np.int16)
    motion = np.zeros(n, np.float32)
    motion[1:] = np.abs(t16[1:] - t16[:-1]).mean(axis=(1, 2))
    skip2 = np.zeros(n, np.float32)       # frame i-1 against i+1
    skip2[1:-1] = np.abs(t16[2:] - t16[:-2]).mean(axis=(1, 2))
    shot_of = np.zeros(n, int) - 1
    for k, s in enumerate(edit.shots):
        shot_of[s["frame"]: s["frame"] + s["frames"]] = k

    def shot(i):
        k = shot_of[min(max(i, 0), n - 1)]
        return edit.shots[k] if k >= 0 else None

    def in_fade(i) -> bool:
        s = shot(i)
        if not s:
            return False
        rel_t = (i - s["frame"]) / fps
        return rel_t < float(s.get("fade_in") or 0) + 0.05 or (s["len"] - rel_t) < float(s.get("fade_out") or 0) + 0.05

    def intended_black(s) -> bool:
        src = (s or {}).get("source", {})
        return src.get("name") == "black" or "black" in str((s or {}).get("desc", "")).lower()


    def merge(rs, gap):
        out = []
        for a, b in rs:
            if out and a - out[-1][1] <= gap:
                out[-1] = (out[-1][0], b)
            else:
                out.append((a, b))
        return out

    def known_still(s, a, b) -> list[str]:
        desc = (str(s.get("desc", "")) + " " + str(s.get("picture", ""))).lower()
        known = [w for w in ("freeze", "still", "hold", "pause") if w in desc]
        return known + [e["name"] for e in (edit.clock or {}).get("events", [])
                        if "freeze" in e["name"] and a / fps - 0.2 <= e["t"] <= b / fps]

    # candidates, then each checked against what its own source shows there
    jobs = []
    black = (mean < 4) & (p98 < 14)
    for a, b in merge(runs(black), 12):
        s = shot(a)
        if not s or b - a < 2 or (in_fade(a) and in_fade(b - 1)):
            continue
        if s["source"].get("type") != "video" or intended_black(s):
            fl.add("info", a / fps, f"black for {b - a} frames ({(b - a) / fps:.2f} s) in {s['id']}"
                   + (", the timeline's black card" if intended_black(s) else f" ({s['source'].get('type')})"), "overview sheets",
                   t1=b / fps)
            continue
        jobs.append(("black", a, b, s))
    for a, b in runs(motion < 0.12):
        s = shot(a)
        if not s or (b - a) / fps < frozen_min_s or s["source"].get("type") != "video":
            continue
        jobs.append(("still", a, b, s))
    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(6) as ex:
        probes = list(ex.map(lambda j: source_span(edit, j[3], j[1] / fps, j[2] / fps), jobs))
    static = []
    for (kind, a, b, s), pr in zip(jobs, probes):
        t0, t1 = a / fps, b / fps
        end = pr.get("end_film")
        if kind == "black":
            if pr.get("mean") is not None and pr["mean"] < 24:
                fl.add("info", t0, f"black for {b - a} frames ({t1 - t0:.2f} s) in {s['id']}: its source is dark there too",
                       "overview sheets", t1=t1)
            elif pr.get("mean") is not None:
                fl.add("fix", t0, f"black for {b - a} frames ({t1 - t0:.2f} s) in {s['id']}, where its source shows a picture "
                       f"(mean luma {pr['mean']:.0f})", "--range", t1=t1)
            else:
                fl.add("look", t0, f"black for {b - a} frames ({t1 - t0:.2f} s) in {s['id']}" +
                       (" after its source ends" if pr.get("ran_out") else ""), "--range", t1=t1)
            continue
        known = known_still(s, a, b)
        if end is not None and t1 > end + 0.25:
            held = t1 - max(t0, end)
            fl.add("info" if known or held < 0.5 else "look", max(t0, end),
                   f"{s['id']} holds its last frame for {held:.2f} s: its source ends at {tc(end, 3)}, "
                   f"{s['start'] + s['len'] - end:.2f} s before the shot does" + (f" (the shot names it: {', '.join(known)})" if known else ""),
                   "overview sheets / --range", t1=t1)
        elif pr.get("motion") is not None and pr["motion"] > 1.5:
            fl.add("info" if known else "fix", t0, f"{s['id']} is frozen for {t1 - t0:.2f} s ({b - a} frames) where its source "
                   f"moves (motion {pr['motion']:.1f})" + (f"; the shot names it: {', '.join(known)}" if known else ""),
                   "--range", t1=t1)
        else:
            static.append((s["id"], t0, t1))
    # flat full-screen colour: the picture gone (not black)
    flatness = st[:, 7] if st.shape[1] > 7 else (std < 3.0).astype(np.float32)
    for a, b in merge(runs((flatness > 0.85) & (mean >= 8)), 2):
        if b - a < 3:
            continue
        s = shot(a)
        r, g, bl = st[a, 4:7]
        nb = np.concatenate([mean[max(0, a - 6):a], mean[b:b + 6]])
        jump = float(np.abs(mean[a:b].mean() - nb.mean())) if len(nb) else 0.0
        card = (s or {}).get("source", {}).get("type") != "video"
        sev = "info" if card or jump < 40 else "look"
        fl.add(sev, a / fps, f"flat colour over most of the frame for {b - a} frames ({(b - a) / fps:.2f} s, rgb ≈ {int(r)},{int(g)},{int(bl)}) "
               f"in {s['id'] if s else '-'}" + (f", {jump:.0f} levels from the frames around it: reads as a dropped picture or a flash"
                                                 if jump >= 40 else ""), "--range", t1=b / fps)
    # single-frame glitches: a frame unlike both neighbours, whose neighbours are alike
    gl = np.where((motion[1:-1] > 12) & (np.roll(motion, -1)[1:-1] > 12) & (skip2[1:-1] < 3))[0] + 1
    for i in gl:
        fl.add("look", i / fps, f"one frame unlike both its neighbours (f{i}, in {(shot(i) or {}).get('id', '-')}): "
               "a flash, a glitch or a one-frame insert", "--range")
    # flashes: big luma swings; and more than three a second (photosensitivity guidance)
    dl = np.diff(mean, prepend=mean[0])
    big = np.where(np.abs(dl) >= 25)[0]
    pairs = []
    for i in big:
        j = big[(big > i) & (big <= i + 20)]
        j = j[np.sign(dl[j]) == -np.sign(dl[i])]
        if len(j):
            pairs.append((i, int(j[0])))
    seen_pairs = set()
    for i, j in pairs:
        if (i, j) in seen_pairs:
            continue
        seen_pairs.add((i, j))
        lo = min(mean[i - 1] if i else mean[i], mean[j])
        fl.add("info", i / fps, f"a flash: mean luma {lo:.0f} → {max(mean[i:j]):.0f} → {mean[j]:.0f} in {(j - i) / fps:.2f} s "
               f"({(shot(i) or {}).get('id', '-')})", "--range", t1=j / fps)
    if pairs:
        starts = np.array([p[0] for p in pairs]) / fps
        for t in starts:
            k = ((starts >= t) & (starts < t + 1.0)).sum()
            if k > 3:
                fl.add("fix", t, f"{k} flashes within one second: over the 3-a-second photosensitivity guideline", "--range")
                break
    # the cuts: does the picture change on the timeline's frame?
    med = float(np.median(motion[1:])) if n > 1 else 0.0
    offs = []
    for s in edit.shots[1:]:
        c = int(s["frame"])
        if c <= 3 or c >= n - 3 or s.get("transition_in"):
            continue
        w = motion[c - 3: c + 4]
        k = int(np.argmax(w)) - 3
        if w.max() > max(8.0, 6 * med) and k != 0 and motion[c] < 0.5 * w.max():
            offs.append((s["id"], k))
            fl.add("look", c / fps, f"the picture changes {k:+d} frame(s) from the timeline's cut into {s['id']}", "--range")
    return {"motion": motion, "skip2": skip2, "median_motion": med, "cut_offsets": offs, "static": static}


# ----------------------------------------------------------- filmstrips ----

def filmstrip(edit, a_t: float, b_t: float, out: Path, every: int = 1, audio: dict | None = None,
              cols: int = 6, rows: int = 10, tw: int = 320) -> list[Path]:
    """Every frame (or every Nth) of a range, in pages of cols x rows tiles, each labelled with its frame,
    film time and shot; above each page, the sound of that page's span (the mix and each bus, as a level
    envelope), the frames' ticks and the cuts in red."""
    fps = edit.fps
    a = max(0, int(round(a_t * fps)))
    b = min(edit.nframes, int(round(b_t * fps)) + 1)
    th = int(round(tw * 9 / 16))
    pad = 6
    per = cols * rows
    frames = list(read_frames(edit.cut, a, b - a, fps, tw, th, every))
    cut_frames = {int(s["frame"]) for s in edit.shots[1:]}
    paths = []
    for pi in range(0, len(frames), per):
        page = frames[pi:pi + per]
        W = cols * (tw + pad) + pad
        wave_h = 170 if audio else 0
        H = 44 + wave_h + math.ceil(len(page) / cols) * (th + 20) + 6
        im = Image.new("RGB", (W, H), BG)
        d = ImageDraw.Draw(im)
        f0, f1 = page[0][0], page[-1][0]
        _label(d, (pad, 10), f"{edit.cut.name} · {tc(f0 / fps, 3)}–{tc((f1 + every) / fps, 3)} · every "
               f"{'frame' if every == 1 else f'{every}th frame'} · red: the first frame after a cut", 18, GOLD, True)
        if audio:
            _wave(d, audio, f0 / fps, (f1 + every) / fps, pad, 40, W - 2 * pad, wave_h - 10, fps, every,
                  [f / fps for f in cut_frames])
        for k, (fi, arr) in enumerate(page):
            x0 = pad + (k % cols) * (tw + pad)
            y0 = 44 + wave_h + (k // cols) * (th + 20)
            im.paste(Image.fromarray(arr.copy()), (x0, y0))
            if any(fi - every < c <= fi for c in cut_frames):
                d.rectangle([x0 - 3, y0 - 3, x0 + tw + 2, y0 + th + 2], outline=RED, width=3)
            s = edit.shot_at_frame(fi)
            _label(d, (x0, y0 + th + 2), f"f{fi} {tc(fi / fps, 3)} {s['id'] if s else '-'}", 12, WHITE)
        p = out / (f"range-{tc(a_t, 1).replace(':', 'm')}-{tc(b_t, 1).replace(':', 'm')}"
                   + (f"-every{every}" if every > 1 else "") + f"-p{pi // per + 1:02d}.jpg")
        im.save(p, quality=90)
        paths.append(p)
    return paths


def _wave(d: ImageDraw.ImageDraw, audio: dict, t0: float, t1: float, x: int, y: int, w: int, h: int, fps: float,
          every: int, cuts: list[float]) -> None:
    """Each bus's level per pixel column (the peak of its samples, -60 to 0 dBFS), one lane each, the mix on
    top; frame ticks under it, the cuts as red lines."""
    sr = audio["sr"]
    buses = [(k, audio[k]) for k in ("mix", "voice", "music", "sfx", "game") if audio.get(k) is not None]
    cols = {"mix": WHITE, "voice": CYAN, "music": (170, 120, 230), "sfx": GOLD, "game": (120, 200, 120)}
    lane = (h - 10) / max(1, len(buses))
    d.rectangle([x, y, x + w, y + h], fill=(8, 8, 10))
    a, b = int(t0 * sr), int(t1 * sr)
    half = int(0.0025 * sr)                       # a 5 ms RMS window round each pixel's moment
    centres = np.linspace(a, b, w).astype(int)
    for li, (name, sig) in enumerate(buses):
        lo_, hi_ = max(0, a - half), min(len(sig), b + half + 1)
        seg = sig[lo_:hi_]
        seg = (seg.astype(np.float64) ** 2).mean(axis=1) if seg.ndim > 1 else seg.astype(np.float64) ** 2
        if len(seg) < 2 * half + 2:
            continue
        c = np.concatenate([[0.0], np.cumsum(seg)])
        i0 = np.clip(centres - half - lo_, 0, len(seg))
        i1 = np.clip(centres + half - lo_, 1, len(seg))
        rms = np.sqrt((c[i1] - c[i0]) / np.maximum(1, i1 - i0))
        db = 20 * np.log10(rms + 1e-9)
        hgt = np.clip((db + 50) / 45, 0, 1) * (lane - 3)
        base = y + lane * (li + 1) - 1
        for px in range(w):
            if hgt[px] > 0:
                d.line([x + px, base, x + px, base - hgt[px]], fill=cols[name])
        d.line([x, base, x + w, base], fill=(40, 40, 46))
        lab = f"{name}: 5 ms RMS, -50..-5 dBFS"
        d.rectangle([x + 2, y + lane * li + 1, x + 8 + 7 * len(lab), y + lane * li + 14], fill=(8, 8, 10))
        d.text((x + 4, y + lane * li + 1), lab, font=font(11), fill=cols[name])
    for f in range(int(math.ceil(t0 * fps - 1e-6)), int(t1 * fps) + 1):
        px = x + (f / fps - t0) / (t1 - t0) * w
        d.line([px, y + h - (9 if f % 6 == 0 else 5), px, y + h], fill=DIM)
    for c in cuts:
        if t0 <= c <= t1:
            px = x + (c - t0) / (t1 - t0) * w
            d.line([px, y, px, y + h], fill=RED, width=2)


# ----------------------------------------------------------------- report ----

def picture_md(edit, pf: dict, moments: list[dict], measured: list[dict], phone: list[Path], out: Path) -> str:
    lines = ["# Picture", ""]
    lines.append(f"Decoded every frame ({edit.nframes}) at 640x360. Median frame-to-frame motion "
                 f"{pf['median_motion']:.2f} (mean absolute luma change of an 80x45 thumbnail).")
    lines.append("")
    lines.append("**Black and frozen** stretches are checked against the shot's own source file over the same span: a still "
                 "or black picture whose source is still or black too is the footage's own (not flagged); one whose source moves "
                 "or shows a picture is a fault; one past the source's end is the build holding its last frame.")
    st = pf.get("static") or []
    if st:
        lines.append(f"Still where the source is still too ({len(st)}): " +
                     ", ".join(f"{sid} {tc(a)}–{tc(b)}" for sid, a, b in st[:40]) + (" …" if len(st) > 40 else "") + ".")
    lines.append("")
    lines.append("## Text at phone size and the safe area")
    lines.append("")
    lines.append("Each captioned moment the timeline knows of, at 640x360 1:1, which is how a 1080p film shows on a phone held "
                 "sideways. *Text px* is the height of a capital at that size: from the overlay's own scale "
                 "(Quake's 8x8 letters) where the timeline gives it, else measured in the overlay file (bright opaque "
                 "marks grouped into lines; the median line, and the smallest). Under 6 px is unreadable; 6–7.5 is marginal. "
                 "Text burned into footage is not measured: look at the sheets.")
    lines.append("")
    rows = []
    for m, me in zip(moments, measured):
        if m.get("cap_from") == "scale":
            cap, how = m["cap"], "its scale"
        elif me.get("cap_median") is not None:
            cap, how = me["cap_median"], (f"measured: {me['lines']} lines, {me['small_lines']} under 6 px, smallest "
                                          f"{me['cap_min']:.1f}" + (f"; {me['note']}" if me.get("note") else ""))
        else:
            cap, how = None, me.get("note") or "look"
        verdict = "-" if cap is None else ("**too small**" if cap < 6 else ("marginal" if cap < 7.5 else "ok"))
        boxes = ([m["box"]] if m.get("box") else []) + (me.get("boxes") or [])
        safe = "outside" if any(outside_safe(bx) for bx in boxes) else ("inside" if boxes else "-")
        rows.append([tc(m["t"]), m["shot"], m["what"][:70], "-" if cap is None else f"{cap:.1f}", verdict, how, safe])
    lines.append(md_table(["time", "shot", "text", "text px", "verdict", "how", "90% safe"], rows, "llllllc"))
    lines.append("")
    lines.append("Sheets: " + ", ".join(f"[{p.name}]({p.name})" for p in phone))
    lines.append("")
    return "\n".join(lines)
