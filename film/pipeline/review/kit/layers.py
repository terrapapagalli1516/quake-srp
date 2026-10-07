"""Layers: the cut's frame against its own source's frame at the same moment, so the kit can tell what the
edit drew (its overlays) from what the footage carries (text burned into it). The source is decoded the way
edit/build.py places it (its in-point, speed, fit or 4:3 box, held last frame, dim), from the build's frozen
copy when it is still there. Where the two differ, the edit drew something.

Used by the ghosts, overtext and shape checks (full-size frame pairs at a few moments per shot) and by the
readtime and textsize checks (text tracked through cards and card overlays)."""

from __future__ import annotations

import math
import subprocess
from pathlib import Path

import numpy as np

FW, FH = 1920, 1080
D_OVER = 30.0          # luma levels: a pixel the edit changed (an overlay), well above the codec's noise


# ------------------------------------------------------------ decoding ----

def _sel(ks: list[int]) -> str:
    return "select='" + "+".join(f"eq(n\\,{k})" for k in ks) + "'"


def _read(cmd: list[str], n: int, w: int, h: int, ch: int = 3) -> list[np.ndarray]:
    r = subprocess.run(cmd, capture_output=True)
    fb = w * h * ch
    k = len(r.stdout) // fb
    return [np.frombuffer(r.stdout[i * fb:(i + 1) * fb], np.uint8).reshape(h, w, ch) for i in range(min(k, n))]


VAAPI = "/dev/dri/renderD128"
_HW: bool | None = None


def _hw(cut: Path) -> bool:
    """Decode the cut on the GPU (VA-API) when there is one, to spare the CPUs (as the picture pass does)."""
    global _HW
    if _HW is None:
        try:
            r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-hwaccel", "vaapi", "-hwaccel_device", VAAPI,
                                "-hwaccel_output_format", "vaapi", "-i", str(cut), "-frames:v", "1", "-vf",
                                "hwdownload,format=nv12,scale=64:36,format=rgb24", "-f", "rawvideo", "-"],
                               capture_output=True, timeout=30)
            _HW = r.returncode == 0 and len(r.stdout) == 64 * 36 * 3
        except Exception:  # noqa: BLE001
            _HW = False
    return _HW


def cut_frames(cut: Path, frames: list[int], fps: float, w: int = FW, h: int = FH) -> dict[int, np.ndarray]:
    """The cut's frames (absolute numbers), frame-exact, as RGB; one decoder run over their span."""
    if not frames:
        return {}
    fs = sorted(set(frames))
    a = fs[0]
    ss = ["-ss", f"{(a - 0.5) / fps:.6f}"] if a > 0 else []
    conv = f"scale={w}:{h}:in_color_matrix=bt709:in_range=tv:flags=area,format=rgb24"
    for hw in ([True, False] if _hw(cut) else [False]):
        pre = ["-hwaccel", "vaapi", "-hwaccel_device", VAAPI, "-hwaccel_output_format", "vaapi"] if hw else ["-threads", "4"]
        vf = f"{_sel([f - a for f in fs])}," + ("hwdownload,format=nv12," if hw else "") + conv
        cmd = ["ffmpeg", "-v", "error", "-nostdin", *pre, *ss, "-i", str(cut), "-an", "-sn", "-vf", vf,
               "-fps_mode", "passthrough", "-frames:v", str(len(fs)), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
        got = _read(cmd, len(fs), w, h)
        if len(got) == len(fs):
            break
    return {f: x for f, x in zip(fs, got)}


def source_file(src: dict, film: Path) -> str | None:
    if src.get("frozen") and Path(src["frozen"]).exists():
        return src["frozen"]
    p = film / src.get("path", "")
    return str(p) if src.get("path") and p.exists() else None


def source_chain(src: dict, length: float, w: int, h: int, ks: list[int]) -> str:
    """edit/build.py's source_chain(): the source from its in-point at 60 fps, fitted (or in the 4:3 box), held;
    the wanted frames picked before the (per-frame, timing-neutral) scaling, so only they are converted."""
    f = "setpts=PTS-STARTPTS"
    sp = float(src.get("speed", 1.0))
    if sp != 1.0:
        f += f",setpts=PTS/{sp:.6f}"
    f += ",fps=60"
    if src.get("out") is not None:
        span = float(src["out"]) - float(src.get("in", 0.0))
        if span > 0:
            f += f",trim=end={span:.5f}"
    f += f",tpad=stop_mode=clone:stop_duration={length + 1:.3f}," + _sel(ks)
    if src.get("box"):
        f += ",scale=1440:1080:flags=bicubic,pad=1920:1080:240:0:color=black,setsar=1,format=gbrp"
    else:
        f += (",scale=1920:1080:force_original_aspect_ratio=decrease:flags=bicubic,"
              "pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=gbrp")
    if (w, h) != (FW, FH):
        f += f",scale={w}:{h}:flags=area"
    return f


def source_frames(src: dict, film: Path, length: float, ks: list[int], w: int = FW, h: int = FH) -> dict[int, np.ndarray]:
    """A video shot's source at shot frames ks (0 = the shot's first frame), placed as the build places it."""
    p = source_file(src, film)
    if p is None or not ks:
        return {}
    dur = float((src.get("probe") or {}).get("duration") or 0.0)
    at = float(src.get("in", 0.0))
    at = min(at, max(0.0, dur - 2 / 60)) if dur else at
    ks = sorted(set(ks))
    vf = source_chain(src, length, w, h, ks) + ",format=rgb24"
    # on the GPU when it can (a few levels of chroma rounding apart from the build's own decode: far under D_OVER)
    for hw in ([True, False] if _hw(Path(p)) else [False]):
        pre = ["-hwaccel", "vaapi", "-hwaccel_device", VAAPI, "-hwaccel_output_format", "vaapi"] if hw else ["-threads", "4"]
        cmd = ["ffmpeg", "-v", "error", "-nostdin", *pre, *(["-ss", f"{at:.5f}"] if at > 0 else []), "-i", p,
               "-an", "-sn", "-vf", ("hwdownload,format=nv12," if hw else "") + vf, "-fps_mode", "passthrough",
               "-frames:v", str(len(ks)), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
        got = _read(cmd, len(ks), w, h)
        if len(got) == len(ks):
            break
    return {k: x for k, x in zip(ks, got)}


def overlay_rgba(path: Path, at: float, w: int = FW, h: int = FH) -> np.ndarray | None:
    """One frame of an overlay movie (with its alpha) at its own time `at`."""
    r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-ss", f"{max(0.0, at):.4f}", "-i", str(path), "-frames:v", "1",
                        "-vf", f"scale={w}:{h}:flags=area,format=rgba", "-f", "rawvideo", "-"], capture_output=True)
    if len(r.stdout) != w * h * 4:
        return None
    return np.frombuffer(r.stdout, np.uint8).reshape(h, w, 4)


# ------------------------------------------------------------- text marks ----

def luma(rgb: np.ndarray) -> np.ndarray:
    x = rgb.astype(np.float32)
    return x[..., 0] * 0.2126 + x[..., 1] * 0.7152 + x[..., 2] * 0.0722


def glyph_mask(y: np.ndarray, k: int = 15, thr: float = 28.0, floor: float = 48.0) -> np.ndarray:
    """Bright, thin strokes: what letters on a darker ground look like (a white top-hat narrower than k)."""
    from scipy import ndimage
    op = ndimage.grey_opening(y, size=(k, k))
    return ((y - op) > thr) & (y > floor)


def marks_of(mask: np.ndarray, min_h: int = 5, max_h: int = 200):
    """Connected strokes that could be letters: (label image, list of marks {i, box, area, sl})."""
    from scipy import ndimage
    lab, n = ndimage.label(mask, structure=np.ones((3, 3)))
    if n == 0:
        return lab, []
    objs = ndimage.find_objects(lab)
    areas = ndimage.sum(mask, lab, index=np.arange(1, n + 1))
    out = []
    for i, sl in enumerate(objs):
        h = sl[0].stop - sl[0].start
        w = sl[1].stop - sl[1].start
        if h < min_h or h > max_h or areas[i] < 6:
            continue
        fill = areas[i] / (h * w)
        if fill < 0.10 or (w > 2.5 * h and not (0.12 <= fill <= 0.8 and w <= 45 * h)):
            continue
        out.append({"i": i + 1, "box": (sl[1].start, sl[0].start, w, h), "area": float(areas[i]), "sl": sl})
    return lab, out


def group_lines(marks: list[dict], min_marks: int = 3, allow_wide: bool = False) -> list[dict]:
    """Marks of similar height on one row, close beside each other: lines of text. Each line: box, h (the
    median mark height), marks (indices into `marks`)."""
    if not marks:
        return []
    c = np.array([m["box"] for m in marks], float)
    par = list(range(len(c)))

    def find(i):
        while par[i] != i:
            par[i] = par[par[i]]
            i = par[i]
        return i
    order = np.argsort(c[:, 0])
    cy = c[:, 1] + c[:, 3] / 2
    n = len(c)
    # each mark against the next 40 by x, all at once; then join the pairs that sit on one line
    win = 40
    idx = np.arange(n)[:, None] + np.arange(1, win + 1)[None, :]
    ok = idx < n
    I = np.repeat(order[:, None], win, axis=1)
    J = order[np.minimum(idx, n - 1)]
    hi, hj = c[I, 3], c[J, 3]
    hm = np.maximum(hi, hj)
    gap = c[J, 0] - (c[I, 0] + c[I, 2])
    join = ok & (gap <= 1.4 * hm) & (np.abs(cy[I] - cy[J]) < 0.35 * hm) & (hi / hj > 1 / 1.6) & (hi / hj < 1.6)
    for i, j in zip(I[join].tolist(), J[join].tolist()):
        par[find(i)] = find(j)
    groups: dict[int, list[int]] = {}
    for i in range(len(c)):
        groups.setdefault(find(i), []).append(i)
    lines = []
    for g in groups.values():
        wide = allow_wide and len(g) == 1 and c[g[0], 2] >= 2.5 * c[g[0], 3]
        if len(g) < min_marks and not wide:
            continue
        x0, y0 = c[g, 0].min(), c[g, 1].min()
        x1, y1 = (c[g, 0] + c[g, 2]).max(), (c[g, 1] + c[g, 3]).max()
        hs = np.sort(c[g, 3])
        lines.append({"box": [int(x0), int(y0), int(x1 - x0), int(y1 - y0)], "h": float(np.median(hs)),
                      "cap": float(hs[int(0.7 * (len(hs) - 1))]), "marks": sorted(g, key=lambda k: c[k, 0])})
    return lines


def soft_height(img: np.ndarray, m: dict, frac: float = 0.3) -> int:
    """A letter's full height: Quake's letters are shaded, bright at the top and dark at the foot, so the strict
    stroke mask clips them. Over the letter's columns, the rows whose brightest pixel stands above the local
    ground by `frac` of the letter's own contrast, contiguous around its brightest row."""
    x, y, w, h = m["box"]
    H, W = img.shape
    y0, y1 = max(0, int(y - 0.6 * h)), min(H, int(y + h + 0.6 * h) + 1)
    col = img[y0:y1, x:x + w]
    if col.size == 0:
        return h
    prof = col.max(axis=1)
    ground = float(np.median(col))
    peak = float(prof.max())
    if peak - ground <= 1:
        return h
    on = prof >= ground + frac * (peak - ground)
    i = int(np.argmax(prof))
    a = i
    while a > 0 and on[a - 1]:
        a -= 1
    b = i
    while b < len(on) - 1 and on[b + 1]:
        b += 1
    return max(h, b - a + 1)


def rows_of(lines: list[dict], gap: float = 2.6) -> list[dict]:
    """Lines on one row with word-sized gaps between them (a word space is a whole letter cell): one row."""
    ls = sorted(lines, key=lambda ln: (ln["box"][1] + ln["box"][3] / 2, ln["box"][0]))
    rows: list[dict] = []
    for ln in sorted(ls, key=lambda ln: ln["box"][0]):
        cy = ln["box"][1] + ln["box"][3] / 2
        for r in rows:
            rcy = r["box"][1] + r["box"][3] / 2
            hm = max(r["h"], ln["h"])
            if abs(cy - rcy) < 0.35 * hm and 1 / 1.6 < r["h"] / max(ln["h"], 1) < 1.6 and \
                    ln["box"][0] - (r["box"][0] + r["box"][2]) < gap * hm:
                x0, y0 = min(r["box"][0], ln["box"][0]), min(r["box"][1], ln["box"][1])
                x1 = max(r["box"][0] + r["box"][2], ln["box"][0] + ln["box"][2])
                y1 = max(r["box"][1] + r["box"][3], ln["box"][1] + ln["box"][3])
                r["box"] = [x0, y0, x1 - x0, y1 - y0]
                r["marks"] = r["marks"] + ln["marks"]
                r["parts"].append(ln)
                break
        else:
            rows.append(dict(ln) | {"parts": [ln], "marks": list(ln["marks"])})
    return rows


def grow(box, dx: float, dy: float | None = None, w: int = FW, h: int = FH) -> tuple[int, int, int, int]:
    dy = dx if dy is None else dy
    x, y, bw, bh = box
    x0, y0 = max(0, int(x - dx)), max(0, int(y - dy))
    x1, y1 = min(w, int(math.ceil(x + bw + dx))), min(h, int(math.ceil(y + bh + dy)))
    return x0, y0, x1, y1


def boxes_touch(a, b, pad: float = 0.0) -> bool:
    return (a[0] - pad < b[0] + b[2] and b[0] - pad < a[0] + a[2] and a[1] - pad < b[1] + b[3] and b[1] - pad < a[1] + a[3])


# ------------------------------------------------------- the frame pair ----

def analyse_pair(cut: np.ndarray, src: np.ndarray, dim: float = 1.0, ignore: list | None = None) -> dict:
    """The cut against its source at one moment: where the edit drew (D > D_OVER), the overlay's own text
    lines, and the source's text lines with each mark's state in the cut: shown (unchanged), through (the
    overlay changed it, the letter still reads) or hidden."""
    from scipy import ndimage
    yc = luma(cut)
    ys = luma(src) * dim
    d = ndimage.uniform_filter(np.abs(yc - ys), 3)
    over = ndimage.binary_opening(d > D_OVER, np.ones((2, 2)))
    for bx, by, bw, bh in ignore or []:        # a review build's burned-in shot id and timecode
        over[max(0, by):by + bh, max(0, bx):bx + bw] = False
    # the overlay's area: its bands filled in, so its letters count as its own even where they repeat the
    # footage's letters pixel for pixel (a label redrawn exactly over the burned-in one)
    band = ndimage.binary_fill_holes(ndimage.binary_closing(over, np.ones((5, 5)))) if over.any() else over
    # letters matter only where the overlay is, or near it (a ghost beside a band): look there alone
    gc = np.zeros_like(over)
    gs = np.zeros_like(over)
    near = over
    if over.any():
        near = ndimage.maximum_filter(over, size=81)
        rows, cols = np.where(near.any(axis=1))[0], np.where(near.any(axis=0))[0]
        y0, y1 = max(0, rows[0] - 8), min(over.shape[0], rows[-1] + 9)
        x0, x1 = max(0, cols[0] - 8), min(over.shape[1], cols[-1] + 9)
        gc[y0:y1, x0:x1] = glyph_mask(yc[y0:y1, x0:x1])
        gs[y0:y1, x0:x1] = glyph_mask(ys[y0:y1, x0:x1])
        gc &= band
        gs &= near
    # the overlay's text: letters in the cut that are not the source's
    labc, mc = marks_of(gc)
    ov_lines = group_lines(mc, 2)
    for ln in ov_lines:
        ln["kind"] = "overlay"
    # the source's text, and what became of each letter in the cut
    labs, ms = marks_of(gs)
    src_lines = [ln for ln in group_lines(ms, 3) if not any(boxes_touch(ln["box"], b) for b in ignore or [])]
    ov_lines = [ln for ln in ov_lines if not any(boxes_touch(ln["box"], b) for b in ignore or [])]
    ovg = ndimage.binary_dilation(gc, iterations=2)            # the overlay's own letters, and their edges
    for ln in src_lines:
        st = []
        for k in ln["marks"]:
            m = ms[k]
            sl = m["sl"]
            stroke = labs[sl] == m["i"]
            x0, y0, x1, y1 = grow(m["box"], max(3, m["box"][3] / 3))
            ring = np.ones((y1 - y0, x1 - x0), bool)
            sy, sx = sl[0].start - y0, sl[1].start - x0
            dil = ndimage.binary_dilation(stroke, iterations=2)
            ring[sy:sy + dil.shape[0], sx:sx + dil.shape[1]] &= ~dil
            ring &= ~ovg[y0:y1, x0:x1]
            free = stroke & ~ovg[sl]                       # the letter's strokes the overlay's letters leave alone
            dm = float(d[sl][stroke].mean())
            if free.sum() < 0.35 * stroke.sum():
                st.append({"box": m["box"], "state": "under", "c_src": 0.0, "c_cut": 0.0, "d": dm})
                continue
            cs = float(ys[sl][free].mean() - np.median(ys[y0:y1, x0:x1][ring])) if ring.any() else 0.0
            cc = float(yc[sl][free].mean() - np.median(yc[y0:y1, x0:x1][ring])) if ring.any() else 0.0
            dm = float(d[sl][free].mean())
            state = "shown" if dm < 20 else ("hidden" if cc < max(7.0, 0.12 * cs) else "through")
            st.append({"box": m["box"], "state": state, "c_src": cs, "c_cut": cc, "d": dm})
        ln["states"] = st
        ln["kind"] = "source"
        x0, y0, x1, y1 = grow(ln["box"], ln["h"] * 0.6)
        ln["over_near"] = float(over[y0:y1, x0:x1].mean())
        # what a label looks like: letters of one height on one baseline, on a plain band
        bx0, by0, bx1, by1 = grow(ln["box"], ln["h"] * 0.3)
        bg = ~ndimage.binary_dilation(gs[by0:by1, bx0:bx1], iterations=2)
        vals = ys[by0:by1, bx0:bx1][bg]
        ln["bg_med"] = float(np.median(vals)) if vals.size else 0.0
        ln["bg_std"] = float(np.std(vals)) if vals.size else 99.0
        hs = np.array([ms[k]["box"][3] for k in ln["marks"]], float)
        bots = np.array([ms[k]["box"][1] + ms[k]["box"][3] for k in ln["marks"]], float)
        ln["h_cv"] = float(hs.std() / max(1.0, hs.mean()))
        ln["base_spread"] = float((bots.max() - bots.min()) / max(1.0, np.median(hs)))
    return {"over": over, "ov_lines": ov_lines, "src_lines": src_lines, "gs": gs, "yc": yc, "ys": ys}


def burn_in(cut: Path, nframes: int, fps: float) -> bool:
    """Whether the cut is a review build with a timecode burned in at the top right (edit/build.py's default):
    a row of letters there on four frames spread through the film."""
    from concurrent.futures import ThreadPoolExecutor
    fs = [int(nframes * f) for f in (0.13, 0.38, 0.63, 0.88)]
    with ThreadPoolExecutor(4) as ex:
        got = list(ex.map(lambda f: cut_frames(cut, [f], fps).get(f), fs))
    hits = 0
    for fr in got:
        if fr is None:
            continue
        y = luma(fr[0:46, 1680:1920])
        _, ms = marks_of(glyph_mask(y, k=9, thr=25.0, floor=60.0), min_h=6, max_h=30)
        if any(len(ln["marks"]) >= 6 for ln in group_lines(ms, 6)):
            hits += 1
    return hits >= 3


def ignore_boxes(shot: dict, burned: bool) -> list:
    """The review build's own marks: the timecode's box at the top right, and a video shot's id at the top left
    (cards.tag: 16 px a letter, at 32, 28)."""
    if not burned:
        return []
    out = [(1660, 0, 260, 46)]
    if shot.get("source", {}).get("type") == "video":
        out.append((26, 22, len(str(shot["id"])) * 16 + 36, 44))
    return out


def static_share(line: dict, gs_other: np.ndarray, gs: np.ndarray) -> float:
    """How much of a source line's strokes are strokes at another moment of the same shot too (burned-in
    text holds still; the game's bright details move)."""
    x, y, w, h = line["box"]
    a = gs[y:y + h, x:x + w]
    b = gs_other[y:y + h, x:x + w]
    if a.sum() == 0:
        return 0.0
    from scipy import ndimage
    return float((a & ndimage.binary_dilation(b, iterations=1)).sum() / a.sum())


# ---------------------------------------------------------- which moments ----

def overlay_windows(s: dict) -> list[tuple[float, float, dict]]:
    """Each overlay's span in shot time (it draws from `at` to `until`, or its enable window, or the shot's end)."""
    out = []
    ln = float(s["len"])
    for o in s.get("overlays", []):
        a = float(o.get("at") or 0.0)
        b = float(o["until"]) if o.get("until") is not None else ln
        if o.get("enable"):
            a, b = max(a, float(o["enable"][0])), min(b, float(o["enable"][1]))
        if o.get("type") == "bumper":
            b = min(b, a + float(o.get("hold", 1.5)))
        if o.get("type") == "caption":
            a += 0.12
        if b - a > 0.05:
            out.append((a, b, o))
    return out


def safe_span(s: dict) -> tuple[float, float]:
    """The part of a shot with no fade or transition on it (shot time)."""
    a = float(s.get("fade_in") or 0.0) + 0.05
    tr = s.get("transition_in")
    if tr:
        a = max(a, float(tr.get("dur", 0.5)) + 0.05)
    b = float(s["len"]) - float(s.get("fade_out") or 0.0) - 1.5 / 60
    return a, b


def opaque_footage_overlay(o: dict) -> bool:
    p = str(o.get("path", ""))
    return o.get("type") == "video" and p.endswith(".mp4") and "footage/" in p


def pair_samples(edit, want_text: bool, want_shape: bool) -> list[dict]:
    """The moments to look at, per shot: three in each overlay's span (for the text checks) and three in each
    act-4 game shot (for the frame's shape). Each: shot index, shot frame k, film frame, why."""
    jobs = []
    for si, s in enumerate(edit.shots):
        src = s.get("source", {})
        if src.get("type") not in ("video", "card", "burst") or src.get("reverse"):
            continue
        a0, b0 = safe_span(s)
        ts: dict[float, set] = {}
        if want_text and s.get("overlays"):
            for a, b, o in overlay_windows(s):
                if opaque_footage_overlay(o):
                    continue
                a, b = max(a, a0), min(b, b0)
                if b - a < 0.1:
                    continue
                for f in (0.2, 0.6, 0.95):
                    ts.setdefault(round(a + f * (b - a), 3), set()).add("text")
        if want_shape and str(s.get("section", "")).startswith("4") and src.get("type") in ("video", "burst"):
            for f in (0.12, 0.5, 0.88):
                ts.setdefault(round(a0 + f * (b0 - a0), 3), set()).add("shape")
        if not ts:
            continue
        keep: list[tuple[float, set]] = []
        for t in sorted(ts):
            if keep and t - keep[-1][0] < 0.15:
                keep[-1][1].update(ts[t])
                continue
            keep.append((t, set(ts[t])))
        if len(keep) > 9:   # many overlays: the shape's moments, and text moments spread over the shot up to nine
            sh = [k for k in keep if "shape" in k[1]]
            tx = [k for k in keep if "shape" not in k[1]]
            m = max(1, 9 - len(sh))
            tx = [tx[int(round(i * (len(tx) - 1) / max(1, m - 1)))] for i in range(m)] if len(tx) > m else tx
            keep = sorted(sh + tx, key=lambda k: k[0])
        for t, why in keep:
            k = min(int(s["frames"]) - 1, max(0, int(round(t * edit.fps))))
            jobs.append({"shot": si, "k": k, "frame": int(s["frame"]) + k, "t": s["start"] + k / edit.fps, "why": sorted(why)})
    return jobs


def source_for_sample(s: dict, k: int, fps: float) -> tuple[dict | None, int, str]:
    """Which file shows under the overlays at shot frame k: the shot's source, or an opaque footage clip laid
    over it from `at` (HERO's action half). Returns (source dict for source_frames, its frame, note)."""
    t = k / fps
    for o in s.get("overlays", []):
        if opaque_footage_overlay(o) and t >= float(o.get("at") or 0.0):
            if int(o.get("x", 0)) or int(o.get("y", 0)) or float(o.get("scale", 1.0)) != 1.0 or o.get("fade_in_at") is not None:
                return None, 0, "an opaque clip laid over part of the frame"
            ka = int(round((t - float(o.get("at") or 0.0)) * fps))
            return {"type": "video", "path": o["path"], "in": float(o.get("in", 0.0)), "probe": {}}, ka, "the clip laid over it"
    return s.get("source"), k, ""


def run_pairs(cut: Path, fps: float, film: Path, shot: dict, samples: list[dict], out: Path | None = None,
              burned: bool = False) -> list[dict]:
    """Worker: decode one shot's moments from the cut and from its source, analyse each pair, judge the
    text findings (ghost_lines, overtext_hits) and draw their evidence into out/evidence/."""
    src = shot.get("source", {})
    res: list[dict] = []
    cf = cut_frames(cut, [x["frame"] for x in samples], fps)
    by_src: dict[str, tuple[dict, list[int]]] = {}
    plan = []
    for x in samples:
        sd, ka, note = source_for_sample(shot, x["k"], fps)
        plan.append((x, sd, ka, note))
        if sd is not None and sd.get("type") == "video":
            by_src.setdefault(sd.get("path", ""), (sd, []))[1].append(ka)
    sfr: dict[tuple[str, int], np.ndarray] = {}
    for key, (sd, ks) in by_src.items():
        for ka, fr in source_frames(sd, film, float(shot["len"]), ks).items():
            sfr[(key, ka)] = fr
    imgs, gs_all = {}, {}
    for x, sd, ka, note in plan:
        c = cf.get(x["frame"])
        if c is None:
            continue
        if src.get("type") == "card":
            s_img = np.zeros_like(c)
        elif sd is None or sd.get("type") != "video":
            s_img = None
        else:
            s_img = sfr.get((sd.get("path", ""), ka))
        r = {"t": x["t"], "frame": x["frame"], "k": x["k"], "why": x["why"], "note": note, "full": True}
        if s_img is not None and "text" not in x["why"]:          # the frame's shape alone: the pillars
            ys = luma(s_img[::3, ::3]) * float(shot.get("dim") or 1.0)
            r["pillars"], r["halves_corr"] = pillar_stats(ys), halves_corr(ys)
        elif s_img is not None:
            a = analyse_pair(c, s_img, float(shot.get("dim") or 1.0) if src.get("type") != "card" else 1.0,
                             ignore_boxes(shot, burned))
            gs_all[x["frame"]] = a["gs"]
            r["ov_lines"] = a["ov_lines"]
            r["src_lines"] = a["src_lines"]
            r["over_share"] = float(a["over"].mean())
            r["pillars"] = pillar_stats(a["ys"])
            r["halves_corr"] = halves_corr(a["ys"])
        r["cut_pillars"] = pillar_stats(luma(c))
        imgs[x["frame"]] = (c, s_img)
        res.append(r)
    # burned-in text holds still: the share of a source line's strokes that are strokes at another moment too
    frames = sorted(gs_all)
    for r in res:
        if r["frame"] not in gs_all:
            continue
        others = [f for f in frames if f != r["frame"]]
        for ln in r.get("src_lines", []):
            ln["static"] = max((static_share(ln, gs_all[f], gs_all[r["frame"]]) for f in others), default=None)
        r["ghosts"] = ghost_lines(r)
        r["overtext"] = overtext_hits(r)
    if out is not None:
        ev = out / "evidence"
        ev.mkdir(parents=True, exist_ok=True)
        for r in res:
            if not r.get("full"):
                continue
            c, s_img = imgs[r["frame"]]
            if r.get("ghosts"):
                r["ghost_img"] = str(draw_text_evidence(ev / f"ghost-{shot['id']}-f{r['frame']}.jpg", c, s_img,
                                                        [ln for ln in r["ghosts"]], [], shot["id"], r["t"]))
            if r.get("overtext"):
                r["overtext_img"] = str(draw_text_evidence(ev / f"overtext-{shot['id']}-f{r['frame']}.jpg", c, s_img,
                                                           [h["src"] for h in r["overtext"]], [h["ov"] for h in r["overtext"]],
                                                           shot["id"], r["t"]))
            if "shape" in r["why"]:
                r["shape_img"] = str(draw_shape_evidence(ev / f"shape-{shot['id']}-f{r['frame']}.jpg", c, s_img, shot["id"], r["t"]))
    return res


def shape_only(cut: Path, fps: float, film: Path, shot: dict, samples: list[dict], out: Path | None) -> list[dict]:
    """Moments looked at only for the frame's shape: the source's pillars at 640x360 and the cut beside it."""
    if not samples:
        return []
    w, h = 640, 360
    cf = cut_frames(cut, [x["frame"] for x in samples], fps, w, h)
    sf: dict[int, np.ndarray] = {}
    plan = [(x,) + source_for_sample(shot, x["k"], fps) for x in samples]
    by: dict[str, tuple[dict, list[int]]] = {}
    for x, sd, ka, note in plan:
        if sd is not None and sd.get("type") == "video":
            by.setdefault(sd.get("path", ""), (sd, []))[1].append(ka)
    got = {}
    for key, (sd, ks) in by.items():
        for ka, fr in source_frames(sd, film, float(shot["len"]), ks, w, h).items():
            got[(key, ka)] = fr
    res = []
    for x, sd, ka, note in plan:
        c = cf.get(x["frame"])
        if c is None:
            continue
        s_img = got.get((sd.get("path", ""), ka)) if sd is not None and sd.get("type") == "video" else None
        r = {"t": x["t"], "frame": x["frame"], "k": x["k"], "why": x["why"], "note": note, "cut_pillars": pillar_stats(luma(c))}
        if s_img is not None:
            ys = luma(s_img) * float(shot.get("dim") or 1.0)
            r["pillars"], r["halves_corr"] = pillar_stats(ys), halves_corr(ys)
        if out is not None:
            ev = out / "evidence"
            ev.mkdir(parents=True, exist_ok=True)
            r["shape_img"] = str(draw_shape_evidence(ev / f"shape-{shot['id']}-f{x['frame']}.jpg", c, s_img, shot["id"], x["t"]))
        res.append(r)
    return res


# --------------------------------------------------------------- judging ----

def label_like(ln: dict, static_min: float = 0.85) -> bool:
    """A source line that looks like a burned-in label: letters at least 9 px tall (1080p), of one height, on
    one baseline, on a dark plain band, holding still through the shot."""
    st = ln.get("static")
    return (ln["h"] >= 9 and ln.get("h_cv", 1) <= 0.22 and ln.get("base_spread", 1) <= 0.25 and ln.get("bg_med", 99) <= 36
            and (st is None or st >= static_min))


def legible(ln: dict) -> list[dict]:
    return [m for m in ln.get("states", []) if m["state"] in ("shown", "through")]


def ghost_lines(r: dict) -> list[dict]:
    """Burned-in labels an overlay reaches but does not hide: some of its letters are under the overlay
    (hidden, dimmed or under its letters), and at least two still read, through the band or beside it."""
    out = []
    for ln in r.get("src_lines", []):
        if not label_like(ln):
            continue
        sts = [m["state"] for m in ln["states"]]
        reached = sum(s in ("through", "hidden", "under") for s in sts)
        leg = legible(ln)
        if reached and len(leg) >= 2:
            out.append(ln | {"legible": len(leg), "letters": len(sts)})
    return out


def overtext_hits(r: dict) -> list[dict]:
    """The overlay's own text lines drawn where burned-in text still reads: three or more legible source letters
    inside an overlay line's box (grown by half a letter)."""
    hits = []
    srcs = [ln for ln in r.get("src_lines", []) if label_like(ln, 0.4) and len(legible(ln)) >= 2]
    for ov in r.get("ov_lines", []):
        if ov["h"] < 6 or len(ov["marks"]) < 2:
            continue
        g = (ov["box"][0] - 0.5 * ov["h"], ov["box"][1] - 0.4 * ov["h"], ov["box"][2] + ov["h"], ov["box"][3] + 0.8 * ov["h"])
        for ln in srcs:
            k = sum(1 for m in legible(ln) if boxes_touch(m["box"], g))
            if k >= 3:
                hits.append({"ov": ov, "src": ln, "letters": k})
    return hits


# -------------------------------------------------------------- evidence ----

def _font(size: int):
    from common import font
    return font(size)


def draw_text_evidence(path: Path, cut: np.ndarray, src: np.ndarray | None, src_lines: list[dict], ov_lines: list[dict],
                       shot: str, t: float) -> Path:
    """The frame at 960 wide with the lines boxed, and below it the region at full size from the cut and from
    its source: letters that still read in red, letters hidden in green, the overlay's text in cyan."""
    from PIL import Image, ImageDraw
    from common import tc
    # the crop: round the line with the most letters still reading, and whatever else fits beside it
    first = max(src_lines, key=lambda ln: sum(m["state"] in ("shown", "through") for m in ln.get("states", []))) \
        if src_lines else ov_lines[0]
    cx = first["box"][0] + first["box"][2] / 2
    cy = first["box"][1] + first["box"][3] / 2
    boxes = [ln["box"] for ln in src_lines + ov_lines
             if abs(ln["box"][0] + ln["box"][2] / 2 - cx) < 450 and abs(ln["box"][1] + ln["box"][3] / 2 - cy) < 150]
    x0 = max(0, min(b[0] for b in boxes) - 60)
    y0 = max(0, min(b[1] for b in boxes) - 40)
    x1 = min(FW, max(b[0] + b[2] for b in boxes) + 60)
    y1 = min(FH, max(b[1] + b[3] for b in boxes) + 40)
    if x1 - x0 > 900:
        x1 = x0 + 900
    if y1 - y0 > 300:
        y1 = y0 + 300
    zoom = 2 if (x1 - x0) <= 470 else 1
    crop_c = Image.fromarray(cut).crop((x0, y0, x1, y1))
    crop_s = Image.fromarray(src).crop((x0, y0, x1, y1)) if src is not None else None

    def mark(im: Image.Image) -> Image.Image:
        im = im.resize((im.width * zoom, im.height * zoom), Image.NEAREST)
        d = ImageDraw.Draw(im)
        for ln in src_lines:
            for m in ln.get("states", []):
                bx, by, bw, bh = m["box"]
                col = (255, 60, 60) if m["state"] in ("shown", "through") else (60, 220, 90)
                d.rectangle([(bx - x0) * zoom - 1, (by - y0) * zoom - 1, (bx + bw - x0) * zoom + 1, (by + bh - y0) * zoom + 1], outline=col)
        for ln in ov_lines:
            bx, by, bw, bh = ln["box"]
            d.rectangle([(bx - x0) * zoom - 3, (by - y0) * zoom - 3, (bx + bw - x0) * zoom + 3, (by + bh - y0) * zoom + 3],
                        outline=(80, 220, 255), width=2)
        return im
    a = mark(crop_c)
    b = mark(crop_s) if crop_s is not None else None
    full = Image.fromarray(cut).resize((960, 540), Image.BILINEAR)
    d = ImageDraw.Draw(full)
    d.rectangle([x0 / 2, y0 / 2, x1 / 2, y1 / 2], outline=(255, 200, 0), width=2)
    W = max(960, a.width + (b.width + 10 if b is not None else 0))
    H = 30 + 540 + 10 + a.height + 22
    im = Image.new("RGB", (W, H), (18, 18, 20))
    dd = ImageDraw.Draw(im)
    dd.text((6, 6), f"{shot} · {tc(t, 3)} · red: burned-in letters that still read · green: hidden · cyan: the overlay's text",
            font=_font(14), fill=(230, 180, 60))
    im.paste(full, (0, 30))
    yy = 30 + 540 + 10
    im.paste(a, (0, yy))
    dd.text((4, yy + a.height + 3), f"the cut, {zoom}x", font=_font(12), fill=(200, 200, 200))
    if b is not None:
        im.paste(b, (a.width + 10, yy))
        dd.text((a.width + 14, yy + b.height + 3), "its source at the same moment", font=_font(12), fill=(200, 200, 200))
    im.save(path, quality=90)
    return path


def draw_shape_evidence(path: Path, cut: np.ndarray, src: np.ndarray | None, shot: str, t: float) -> Path:
    """The cut and its source side by side at 640 wide, the 4:3 box's edges marked."""
    from PIL import Image, ImageDraw
    from common import tc
    im = Image.new("RGB", (1290, 400), (18, 18, 20))
    d = ImageDraw.Draw(im)
    d.text((6, 6), f"{shot} · {tc(t, 3)} · left: the cut · right: its source as the build places it · dashed: the 4:3 box",
           font=_font(14), fill=(230, 180, 60))
    for k, x in enumerate([cut, src]):
        if x is None:
            continue
        p = Image.fromarray(x).resize((640, 360), Image.BILINEAR)
        im.paste(p, (k * 650, 32))
        for xx in (80, 560):
            for yy in range(32, 392, 10):
                d.line([k * 650 + xx, yy, k * 650 + xx, yy + 5], fill=(255, 0, 255))
    im.save(path, quality=88)
    return path


def pillar_stats(y: np.ndarray) -> dict:
    """The 4:3 box's pillars (x < 240, x >= 1680 at 1920 wide): the share of pixels showing a picture."""
    w = y.shape[1]
    p = int(round(240 * w / FW))
    left, right = y[:, :p], y[:, w - p:]
    return {"left": float((left > 22).mean()), "right": float((right > 22).mean()),
            "left_mean": float(left.mean()), "right_mean": float(right.mean())}


def halves_corr(y: np.ndarray) -> float:
    """How alike the frame's left and right halves are (a side-by-side comparison of one view: high)."""
    k = max(1, y.shape[1] // 240)
    s = y[::k, ::k]
    a, b = s[:, : s.shape[1] // 2], s[:, s.shape[1] // 2: 2 * (s.shape[1] // 2)]
    a = a - a.mean()
    b = b - b.mean()
    den = float(np.sqrt((a * a).sum() * (b * b).sum()))
    return float((a * b).sum() / den) if den > 0 else 0.0


def pair_worker(args) -> tuple[int, list[dict]]:
    cut, fps, film, si, shot, samples, out, burned = args
    import os
    os.environ.setdefault("FILM_ROOT", film)   # the workers see the same film
    try:
        return si, run_pairs(Path(cut), fps, Path(film), shot, samples, Path(out) if out else None, burned)
    except Exception as ex:  # noqa: BLE001
        import traceback
        return si, [{"error": f"{type(ex).__name__}: {ex}", "trace": traceback.format_exc()[-1500:]}]


def all_pairs(edit, out: Path, want_text: bool, want_shape: bool, workers: int = 6) -> list[dict]:
    """Every sampled moment of the cut against its source, in parallel processes (a shot each)."""
    import multiprocessing as mp
    from concurrent.futures import ProcessPoolExecutor
    from common import FILM
    jobs = pair_samples(edit, want_text, want_shape)
    by: dict[int, list[dict]] = {}
    for j in jobs:
        by.setdefault(j["shot"], []).append(j)
    # the longest shots first, so the pool ends together
    order = sorted(by, key=lambda si: -len(by[si]))
    burned = burn_in(edit.cut, edit.nframes, edit.fps)
    if burned:
        from common import log
        log("frame pairs: a review build (timecode and shot ids burned in): their boxes are left out")
    args = [(str(edit.cut), edit.fps, str(FILM), si, edit.shots[si], by[si], str(out), burned) for si in order]
    res: list[dict] = []
    with ProcessPoolExecutor(workers, mp_context=mp.get_context("spawn")) as ex:
        for si, rs in ex.map(pair_worker, args):
            for r in rs:
                r["shot"] = edit.shots[si]["id"]
                r["si"] = si
            res += rs
    res.sort(key=lambda r: r.get("t", 0))
    return res
