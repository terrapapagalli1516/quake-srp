"""The film's text, item by item, for the readtime and textsize checks: what each caption, label, card and
diagram says (or how much), how tall its letters are at phone size (640 wide), and when it is fully on screen.

Two sources:
- The timeline, where it says it all: labels, captions and bumpers (their text, Quake's 8x8 letters at their
  scale, a capital being 6 of the 8 rows; their timing from `at`, `until`, the shot's end and fades, the way
  edit/build.py and cards.py draw them).
- Measured, where only the pixels know: title and end cards, the cards.py overlays (rules, menu rows, the
  terminal), diagram movies and PNG overlays. Each is sampled at 10 frames a second (diagram movies at 5); its text lines are found
  on the brightest moment, and each line is fully visible while its letters keep 60% of their best contrast.
  Words are estimated from a line's width (Quake's letter cell is 8 rows to a capital's 6; 6 cells a word with its
  space). A line counts as text when its letters are one height on one baseline, on a plain ground."""

from __future__ import annotations

import math
from pathlib import Path

import numpy as np

import layers as LY

CAP = 6                 # a conchars capital: 6 of its 8 rows (18 px at 3x in the cut; the 7th is the descenders')
K640 = 640 / 1920
RATE = 10.0             # samples a second for measured text
VIS = 0.6               # fully visible: 60% of the line's best contrast (a flickering bumper still reads)


def words_in(text: str) -> int:
    return max(1, len([w for w in text.replace("\n", " ").split() if any(ch.isalnum() for ch in w)]))


def est_words(w: float, cap: float) -> int:
    return max(1, int(round(w / (1.33 * max(cap, 1.0)) / 6.0)))   # a letter cell is 8 rows to the capital's 6


def visible_end(s: dict) -> float:
    return float(s["start"] + s["len"] - (s.get("fade_out") or 0.0))


# ------------------------------------------------------------ the timeline ----

def timeline_items(edit) -> list[dict]:
    items = []
    for s in edit.shots:
        a0, end = float(s["start"]), visible_end(s)
        for o in s.get("overlays", []):
            ty = o.get("type")
            at = float(o.get("at") or 0.0)
            until = s["start"] + float(o["until"]) if o.get("until") is not None else end
            if ty == "label" or (ty == "card" and o.get("name") == "label"):
                sc = int(o.get("scale", 3))
                text = str(o.get("text", ""))
                fade = 0.15 if (o.get("at") or o.get("until")) else 0.0
                items.append({"kind": "label", "shot": s["id"], "text": text, "words": words_in(text), "scale": sc,
                              "cap": CAP * sc * K640, "t0": a0 + at + fade, "t1": min(until, end), "appear": a0 + at,
                              "what": f"label \"{text}\" ({sc}x)", "key": ("label", text.lower())})
            elif ty == "caption":
                text, sub = str(o.get("text", "")), o.get("sub")
                items.append({"kind": "caption", "shot": s["id"], "text": text + (f" / {sub}" if sub else ""),
                              "words": words_in(text) + (words_in(sub) if sub else 0), "scale": 4,
                              "cap": CAP * (2 if sub else 4) * K640, "t0": a0 + at + 6 / 60, "t1": end, "appear": a0 + at,
                              "what": f"caption \"{text}\"" + (f" / \"{sub}\" (2x)" if sub else " (4x)"), "key": ("caption", text.lower())})
            elif ty == "bumper":
                text = str(o.get("text", ""))
                hold = float(o.get("hold", 1.5))
                sc = int(o.get("scale", 10))
                items.append({"kind": "bumper", "shot": s["id"], "text": text, "words": words_in(text), "scale": sc,
                              "cap": CAP * sc * K640, "t0": a0 + at + 6 / 60, "t1": min(a0 + at + hold - 0.3, end),
                              "appear": a0 + at, "what": f"bumper \"{text}\" ({sc}x)", "key": ("bumper", text.lower())})
    # the same text carried on into the next shot (it starts as this one ends): one item
    items.sort(key=lambda x: x["t0"])
    merged: list[dict] = []
    for it in items:
        prev = next((m for m in reversed(merged) if m["key"] == it["key"] and abs(m["t1"] - (it["appear"])) < 0.05
                     and it["appear"] - next(s["start"] for s in edit.shots if s["id"] == it["shot"]) < 0.05), None)
        if prev is not None:
            prev["t1"] = it["t1"]
            prev["shots"].append(it["shot"])
            continue
        merged.append(it | {"shots": [it["shot"]]})
    return merged


# ------------------------------------------------------------- measured ----

def measure_jobs(edit) -> list[dict]:
    """What to track: card shots, cards.py overlays other than labels, diagram movies with alpha (not the
    ladder) and PNG overlays."""
    jobs = []
    film = None
    for si, s in enumerate(edit.shots):
        src = s.get("source", {})
        a0 = float(s["start"])
        if src.get("type") == "card" and src.get("name") not in ("black",):
            jobs.append({"kind": "card", "name": f"card {src.get('name')}", "shots": [si], "t0": a0, "t1": visible_end(s)})
        for o in s.get("overlays", []):
            ty = o.get("type")
            at = float(o.get("at") or 0.0)
            b = float(o["until"]) if o.get("until") is not None else float(s["len"])
            if o.get("enable"):
                at, b = max(at, float(o["enable"][0])), min(b, float(o["enable"][1]))
            if ty == "card" and o.get("name") not in ("label",):
                jobs.append({"kind": "card overlay", "name": f"{o.get('name')} (cards.py)", "shots": [si], "t0": a0 + at,
                             "t1": min(a0 + b, visible_end(s))})
            elif ty == "video" and str(o.get("path", "")).endswith(".mov") and not o.get("ladder"):
                jobs.append({"kind": "diagram", "name": Path(o["path"]).stem, "path": o["path"], "shots": [si],
                             "t0": a0 + at, "t1": min(a0 + b, visible_end(s)), "in": float(o.get("in", 0.0)) - at,
                             "anchor": a0, "panel": o.get("panel"), "x": int(o.get("x", 0)), "y": int(o.get("y", 0)),
                             "scale": float(o.get("scale", 1.0))})
            elif ty == "png" and o.get("path"):
                jobs.append({"kind": "png", "name": Path(o["path"]).name, "path": o["path"], "shots": [si],
                             "t0": a0 + at + (0.15 if o.get("at") else 0.0), "t1": min(a0 + b, visible_end(s)), "static": True})
    # a diagram movie that runs on over the next shot (the same file, picking up where it left off): one job
    out: list[dict] = []
    for j in jobs:
        prev = next((p for p in out if p["kind"] == "diagram" and j["kind"] == "diagram" and p.get("path") == j.get("path")
                     and abs(p["t1"] - j["t0"]) < 0.05), None)
        if prev is not None:
            prev["t1"] = j["t1"]
            prev["shots"].append(j["shots"][0])
            prev.setdefault("parts", [dict(prev)]).append(dict(j))
            continue
        out.append(j)
    return out


def _times(t0: float, t1: float, rate: float = RATE) -> list[float]:
    n = max(2, int(math.floor((t1 - t0) * rate)) + 1)
    return list(np.linspace(t0 + 0.02, t1 - 0.02, n))


def _alpha_frames(path: Path, t_file: list[float], w: int, h: int) -> list[np.ndarray]:
    """An overlay movie's frames (RGBA) at its own times: a seek and one frame each (ProRes is all key frames,
    so this decodes a frame a sample instead of every frame), a few at a time."""
    import subprocess
    from concurrent.futures import ThreadPoolExecutor

    def one(t: float):
        r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", "-ss", f"{max(0.0, t):.4f}", "-i", str(path),
                            "-frames:v", "1", "-vf", f"scale={w}:{h}:flags=area,format=rgba", "-f", "rawvideo", "-"],
                           capture_output=True)
        return np.frombuffer(r.stdout, np.uint8).reshape(h, w, 4) if len(r.stdout) == w * h * 4 else None
    with ThreadPoolExecutor(3) as ex:
        return list(ex.map(one, t_file))


def track(job: dict, cut: str, fps: float, film: str, shots: list[dict], evdir: str | None) -> dict:
    """Worker: the job's strength images over time (the brightness of its own letters), its lines on the
    brightest moment, and each line's fully visible span."""
    from scipy import ndimage
    W, H = 1280, 720
    filmp = Path(film)
    # diagram movies at 5 samples a second (a seek each), the cut at 10
    times = _times(job["t0"], job["t1"], RATE / 2 if job["kind"] == "diagram" else RATE) if not job.get("static") else [job["t0"] + 0.05]
    strength: list[np.ndarray] = []
    thumbs: list[np.ndarray] = []
    if job["kind"] in ("diagram", "png"):
        p = filmp / job["path"]
        if job["kind"] == "png":
            from PIL import Image
            rgba = np.asarray(Image.open(p).convert("RGBA").resize((W, H), Image.BILINEAR))
            frames = [rgba]
        else:
            parts = job.get("parts") or [job]
            tf = []
            for t in times:
                part = next((x for x in parts if x["t0"] - 1e-6 <= t <= x["t1"] + 1e-6), parts[-1])
                tf.append(part["in"] + (t - part["anchor"]))
            frames = _alpha_frames(p, tf, W, H)
        for fr in frames:
            if fr is None:
                strength.append(np.zeros((H, W), np.float32))
                continue
            strength.append(LY.luma(fr[..., :3]) * (fr[..., 3].astype(np.float32) / 255))
    else:
        fr_idx = [int(round(t * fps)) for t in times]
        cf = LY.cut_frames(Path(cut), fr_idx, fps, W, H)
        srcs = {}
        if job["kind"] == "card overlay":
            s = shots[job["shots"][0]]
            ks = [f - int(s["frame"]) for f in fr_idx]
            sf = LY.source_frames(s["source"], filmp, float(s["len"]), ks, W, H)
            srcs = {f: sf.get(k) for f, k in zip(fr_idx, ks)}
        for f in fr_idx:
            c = cf.get(f)
            if c is None:
                strength.append(np.zeros((H, W), np.float32))
                continue
            y = LY.luma(c)
            if job["kind"] == "card overlay":
                sy = LY.luma(srcs[f]) if srcs.get(f) is not None else np.zeros_like(y)
                y = np.where(ndimage.uniform_filter(np.abs(y - sy), 3) > LY.D_OVER, y, 0.0)
            strength.append(y.astype(np.float32))
            thumbs.append(c[::2, ::2].copy())
    if not strength:
        return job | {"lines": [], "note": "nothing decoded"}
    peak = np.max(np.stack(strength), axis=0)
    gm = LY.glyph_mask(peak, k=11, thr=28.0, floor=40.0)
    lab, ms = LY.marks_of(gm, min_h=3, max_h=140)
    for m in ms:
        m["soft_h"] = LY.soft_height(peak, m)
    lines = LY.rows_of(LY.group_lines(ms, 2, allow_wide=True))
    out_lines = []
    for ln in lines:
        hs = np.sort([ms[k]["soft_h"] for k in ln["marks"]])
        ln["cap"] = float(hs[int(0.7 * (len(hs) - 1))])
        ln["h"] = float(np.median(hs))
        # letters, not the game's bright details: one height, one baseline, on a plain ground
        bots = np.array([ms[k]["box"][1] + ms[k]["box"][3] for k in ln["marks"]], float)
        base = float(np.percentile(bots, 80) - np.percentile(bots, 20)) / max(1.0, ln["h"])
        bx0, by0, bx1, by1 = LY.grow(ln["box"], max(2, ln["h"] * 0.3), w=W, h=H)
        ground = peak[by0:by1, bx0:bx1][~ndimage.binary_dilation(gm[by0:by1, bx0:bx1], iterations=2)]
        if ground.size and (float(np.std(ground)) > 18 or base > 0.35):
            continue
        x, y, w, h = ln["box"]
        x0, y0, x1, y1 = LY.grow(ln["box"], max(3, ln["h"] * 0.5), w=W, h=H)
        stroke = np.zeros((y1 - y0, x1 - x0), bool)
        for k in ln["marks"]:
            m = ms[k]
            sl = m["sl"]
            stroke[sl[0].start - y0: sl[0].stop - y0, sl[1].start - x0: sl[1].stop - x0] |= lab[sl] == m["i"]
        ring = ~ndimage.binary_dilation(stroke, iterations=2)
        if not ring.any() or not stroke.any():
            continue
        con = np.array([_contrast(fr, x0, y0, x1, y1, stroke, ring) for fr in strength])
        best = float(con.max())
        if best < 20:
            continue
        vis = con >= VIS * best
        from picture import runs
        rs = runs(vis)
        # the longest fully visible run, closing one-sample gaps
        merged = []
        for a, b in rs:
            if merged and a - merged[-1][1] <= 1:
                merged[-1] = (merged[-1][0], b)
            else:
                merged.append((a, b))
        a, b = max(merged, key=lambda r: r[1] - r[0])
        dt = (times[1] - times[0]) if len(times) > 1 else 1 / RATE
        t0 = times[a] - (0.5 * dt if a > 0 else (times[0] - job["t0"]))
        t1 = times[b - 1] + (0.5 * dt if b < len(times) else (job["t1"] - times[-1]))
        scale = 1.0
        if job["kind"] == "diagram" and (job.get("panel") or job.get("scale", 1.0) != 1.0):
            from picture import placement
            mid = 0.5 * (t0 + t1)
            part = next((p for p in (job.get("parts") or [job]) if p["t0"] <= mid <= p["t1"]), job)
            scale = placement({"panel": job.get("panel"), "t_rel": mid - part["anchor"], "x": job.get("x", 0),
                               "y": job.get("y", 0), "scale": job.get("scale", 1.0)})[2]
        cap = ln["cap"] * scale * 640 / W
        out_lines.append({"box": [int(v * 1920 / W) for v in ln["box"]], "cap": cap, "marks": len(ln["marks"]),
                          "words": est_words(w, ln["cap"]), "t0": t0, "t1": t1, "changing": bool(vis.mean() < 0.15 and len(times) > 4),
                          "best": best})
    res = job | {"lines": out_lines, "n_samples": len(times)}
    if evdir and thumbs:
        res["thumbs_at"] = times
        res["evidence"] = _evidence(Path(evdir), job, times, thumbs, out_lines)
    elif evdir and job["kind"] in ("diagram", "png"):
        res["evidence"] = _evidence_alpha(Path(evdir), job, peak, out_lines)
    return res


def _contrast(fr: np.ndarray, x0: int, y0: int, x1: int, y1: int, stroke: np.ndarray, ring: np.ndarray) -> float:
    """The line's letters against their ground in one frame, the best over small shifts (a panel sliding in
    carries its text a few pixels a frame)."""
    H, W = fr.shape
    best = -1e9
    for dy in (-4, 0, 4):
        for dx in (-8, -4, 0, 4, 8):
            a, b, c, d = y0 + dy, y1 + dy, x0 + dx, x1 + dx
            if a < 0 or c < 0 or b > H or d > W:
                continue
            roi = fr[a:b, c:d]
            v = float(roi[stroke].mean() - np.median(roi[ring]))
            best = max(best, v)
    return best


def _evidence(evdir: Path, job: dict, times: list[float], thumbs: list[np.ndarray], lines: list[dict]) -> str:
    """A strip of the job's frames across its span (up to 8), its lines boxed on the brightest."""
    from PIL import Image, ImageDraw
    from common import font, tc
    evdir.mkdir(parents=True, exist_ok=True)
    idx = sorted({int(round(i)) for i in np.linspace(0, len(thumbs) - 1, min(8, len(thumbs)))})
    tw, th = 320, 180
    im = Image.new("RGB", (4 * (tw + 6) + 6, 30 + math.ceil(len(idx) / 4) * (th + 20)), (18, 18, 20))
    d = ImageDraw.Draw(im)
    d.text((6, 6), f"{job['name']} · {tc(job['t0'], 2)}–{tc(job['t1'], 2)}", font=font(14), fill=(230, 180, 60))
    for n, i in enumerate(idx):
        x0, y0 = 6 + (n % 4) * (tw + 6), 30 + (n // 4) * (th + 20)
        im.paste(Image.fromarray(thumbs[i]).resize((tw, th), Image.BILINEAR), (x0, y0))
        t = times[i]
        for ln in lines:
            if ln["t0"] <= t <= ln["t1"]:
                bx, by, bw, bh = (v * tw / 1920 for v in ln["box"])
                d.rectangle([x0 + bx - 1, y0 + by - 1, x0 + bx + bw + 1, y0 + by + bh + 1], outline=(80, 220, 255))
        d.text((x0, y0 + th + 3), tc(t, 2), font=font(12), fill=(200, 200, 200))
    p = evdir / f"text-{job['name'].split()[0]}-{tc(job['t0'], 2).replace(':', 'm')}.jpg"
    im.save(p, quality=88)
    return str(p)


def _evidence_alpha(evdir: Path, job: dict, peak: np.ndarray, lines: list[dict]) -> str:
    from PIL import Image, ImageDraw
    from common import font, tc
    evdir.mkdir(parents=True, exist_ok=True)
    im = Image.fromarray(np.clip(peak, 0, 255).astype(np.uint8)).convert("RGB").resize((960, 540))
    d = ImageDraw.Draw(im)
    for ln in lines:
        bx, by, bw, bh = (v / 2 for v in ln["box"])
        d.rectangle([bx - 1, by - 1, bx + bw + 1, by + bh + 1], outline=(80, 220, 255))
    d.text((6, 6), f"{job['name']} · its letters at their brightest · {tc(job['t0'], 2)}–{tc(job['t1'], 2)}", font=font(14),
           fill=(230, 180, 60))
    p = evdir / f"text-{job['name'][:40]}-{tc(job['t0'], 2).replace(':', 'm')}.jpg"
    im.save(p, quality=88)
    return str(p)


def track_worker(args) -> dict:
    job, cut, fps, film, shots, evdir = args
    import os
    os.environ.setdefault("FILM_ROOT", film)   # the workers see the same film
    try:
        return track(job, cut, fps, film, shots, evdir)
    except Exception as ex:  # noqa: BLE001
        import traceback
        return job | {"lines": [], "error": f"{type(ex).__name__}: {ex}", "trace": traceback.format_exc()[-1200:]}


def measured_items(edit, out: Path, workers: int = 6) -> list[dict]:
    import multiprocessing as mp
    from concurrent.futures import ProcessPoolExecutor
    from common import FILM
    jobs = measure_jobs(edit)
    jobs.sort(key=lambda j: -(j["t1"] - j["t0"]))
    args = [(j, str(edit.cut), edit.fps, str(FILM), edit.shots, str(out / "evidence")) for j in jobs]
    with ProcessPoolExecutor(workers, mp_context=mp.get_context("spawn")) as ex:
        res = list(ex.map(track_worker, args))
    for r in res:
        r["shot"] = "+".join(edit.shots[i]["id"] for i in r["shots"])
    return sorted(res, key=lambda r: r["t0"])


def ladder_sizes(edit) -> list[dict]:
    """The ladder's letters: one frame of each span's movie, measured as the picture part measures overlays."""
    out = []
    for s in edit.shots:
        for o in s.get("overlays", []):
            if o.get("type") == "video" and o.get("ladder") and o.get("path"):
                p = LY_film() / o["path"]
                rgba = LY.overlay_rgba(p, 0.5 * float(s["len"]))
                if rgba is None:
                    continue
                import picture as P
                ls = P.text_lines(rgba)
                if ls:
                    caps = sorted(ln["h"] * float(o.get("scale", 1.0)) * K640 for ln in ls)
                    out.append({"shot": s["id"], "t": s["start"] + 0.5 * s["len"], "cap": caps[0], "cap_median": float(np.median(caps)),
                                "lines": len(ls), "path": o["path"]})
    return out


def LY_film() -> Path:
    from common import FILM
    return FILM
