#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""The SLOP OPTIONS ladder: the film's scoreboard, driven by a JSON of timed events.

Fourteen rows, the game's own Slop Options labels word for word (quake-rs/src/menu.rs,
PICTURE_ROWS and MOTION_ROWS; the two whose slop value is not "on" carry the menu's value:
FRAME RATE CAP: NONE, PERSPECTIVE SPAN: 8; NATIVE PIXELS is vid_native's own words). A name
wider than 14 letters at 2x wraps at a word break onto a second line inside its row (18 px);
the font never shrinks and nothing is abbreviated.

The look can be set in the JSON's optional "style": {"off": palette index, "off_alpha":
opacity, "on": "tint" (palette 106) | "bronze" (id's own bronze letters, as the heading)}.
A span can override its placement (`place`: pillar, panel, `grid` (two columns at `x`, `y`)
or `column` (one column at `x`, from `head_y`, rows from `y0`, `dy` apart); `nudge`: false
ignores the events' nudges; `dx`, `dy`: a fixed shift) and be lab-style (`lab`: up only
around the span's first light, `before` and `hold` seconds, whatever the events say);
`--overrides FILE` merges such keys into the spans by name (ladder-span-overrides.json).

    uv run ladder.py EVENTS.json                  # every span in the JSON: OUT/ladder_NAME.mov (alpha),
                                                  # and ladder_NAME.json: the span and the events' md5
    uv run ladder.py EVENTS.json --spans ST0 HERO # some of them
    uv run ladder.py EVENTS.json --preview        # OUT/ladder_preview.mp4: the spans, one after another, over black
    uv run ladder.py EVENTS.json --still 3:17.2   # RGBA stills at film times (OUT/stills/)
    --out DIR (default: FILM_ROOT's diagrams/), --jobs N, --keep-frames

The film's events are the edit's: its timeline writes them, on the film's clock, to
edit/v7/ladder-events.json. The JSON (times are the film's clock: seconds, or "m:ss.s"):

    {"fps": 60,
     "events": [{"t": "1:45.3", "do": "show"},
                {"t": "1:51.2", "do": "light", "item": "TORCHES"},
                {"t": "3:16.9", "do": "fly", "to": "panel"}, ...],
     "spans": [{"name": "ST0", "from": "1:44.4", "to": "1:50.9"}, ...]}

Events (`dur` is optional everywhere; the defaults are below):
- show, fade: the whole ladder fades in (0.3 s) or out (0.5 s).
- light ITEM: white (palette 254) for `flash_frames` (6; `frames` on the event overrides), then
  bronze (106). ITEM is a name or 1..14.
- dark ITEM: back to off (palette 26 at 45%) over 0.12 s.
- all-flash: every item lights at once (white, then bronze); all-dark: every item off at once.
- fly (to: "pillar" | "panel"), or fly-to-panel / fly-to-pillar: the move between the box phase's
  right pillar (x 1680-1920, 2x conchars from y 160, 52 px apart) and the top-right panel
  (x 1630-1900, y 30-758, on 55% black), over 0.5 s, eased in and out.
- nudge (dx, dy): shift the whole ladder, eased over 0.3 s (to clear a shot's subject).
- back (alpha): a black backing behind the pillar column (default 0), for a lab shot's footage.
Nothing is drawn before the first show. Off-span time costs nothing: each span renders alone.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # qkit/

import cairo  # noqa: E402

from qkit import look, render  # noqa: E402
from qkit.quake import filmroot  # noqa: E402
from qkit.anim import EASE, clamp01, lerp  # noqa: E402
from qkit.canvas import Canvas, new_canvas  # noqa: E402

ITEMS = ["TORCH FLICKER", "GLIDING LIGHTS", "SMOOTH MONSTERS", "SMOOTH ANIMATIONS", "FRAME RATE CAP: NONE",
         "FLUID SKY", "NATIVE PIXELS", "WIDESCREEN FOV", "SCALED 2-D LAYER", "STATUS BAR OVERLAY", "CROSSHAIR",
         "NAILS FROM BARRELS", "FULL-RATE SOUND", "PERSPECTIVE SPAN: 8"]
WRAP = 14  # letters a line: 14 at 2x from x 1688 fill the pillar
LINE = 18  # the second line's pitch inside a row


def wrap(name: str) -> list[str]:
    """Greedy word wrap at WRAP letters; a word is never split."""
    lines, cur = [], ""
    for w in name.split():
        trial = f"{cur} {w}".strip()
        if cur and len(trial) > WRAP:
            lines.append(cur)
            cur = w
        else:
            cur = trial
    lines.append(cur)
    assert all(len(ln) <= WRAP for ln in lines), lines
    return lines


LINES = [wrap(n) for n in ITEMS]
HEADING = "SLOP OPTIONS"
OFF, OFF_A = look.pal(26), 0.45
ON, FLASH = look.pal(106), look.pal(254)
SCALE = 2  # conchars 2x: 16-pixel letters

# Where things sit: (heading x, heading y, first item y, pitch), the items' x = the heading's.
PILLAR = dict(x=1688, head_y=104, y0=160, dy=52)
PANEL = dict(x=1653, head_y=52, y0=106, dy=46, rect=(1630, 30, 270, 728), back=0.55)
# grid: two columns of seven at (x, y), for a lab shot whose subject fills the right side
GRID = dict(col=248, head_dy=0, y0_dy=58, dy=52, w=512, h=446)
DEFAULT_DUR = {"show": 0.3, "fade": 0.5, "fly": 0.5, "nudge": 0.3, "back": 0.3, "dark": 0.12}


def seconds(v) -> float:
    """A film time: a number of seconds, or "m:ss.s"."""
    if isinstance(v, (int, float)):
        return float(v)
    m, _, s = str(v).rpartition(":")
    return (60 * float(m) if m else 0.0) + float(s)


def item_index(v) -> int:
    if isinstance(v, int) or (isinstance(v, str) and v.isdigit()):
        i = int(v) - 1
    else:
        i = ITEMS.index(str(v).upper())
    if not 0 <= i < len(ITEMS):
        raise ValueError(f"no ladder item {v!r}")
    return i


class Channel:
    """A value that moves to targets at given times, eased: value(t)."""

    def __init__(self, init: float):
        self.init = init
        self.moves: list[tuple[float, float, float, str]] = []  # (t, target, dur, ease)

    def add(self, t: float, target: float, dur: float, ease: str = "inout") -> None:
        self.moves.append((t, target, dur, ease))
        self.moves.sort(key=lambda m: m[0])

    def value(self, t: float) -> float:
        v0, seg = self.init, None
        for m in self.moves:
            if m[0] > t:
                break
            if seg is not None:
                v0 = self._at(seg, v0, m[0])
            seg = m
        return self.init if seg is None else self._at(seg, v0, t)

    @staticmethod
    def _at(seg, v0: float, t: float) -> float:
        t0, target, dur, ease = seg
        u = 1.0 if dur <= 0 else EASE[ease](clamp01((t - t0) / dur))
        return v0 + (target - v0) * u


class Ladder:
    def __init__(self, spec: dict):
        self.fps = int(spec.get("fps", 60))
        self.flash = int(spec.get("flash_frames", 6)) / self.fps
        self.vis, self.place = Channel(0.0), Channel(0.0)
        self.dx, self.dy, self.back = Channel(0.0), Channel(0.0), Channel(0.0)
        self.items: list[list[tuple[float, str, float]]] = [[] for _ in ITEMS]  # (t, "light"|"dark", dur)
        for e in spec["events"]:
            self._event(e)
        for evs in self.items:
            evs.sort(key=lambda e: e[0])
        self.spans = [(s["name"], seconds(s["from"]), seconds(s["to"])) for s in spec.get("spans", [])]
        self.span_opts = {s["name"]: s for s in spec.get("spans", [])}
        st = spec.get("style", {})
        self.off = look.pal(int(st.get("off", 26)))
        self.off_a = float(st.get("off_alpha", OFF_A))
        self.on_style = st.get("on", "tint")
        self.opts: dict = {}  # the span being rendered's overrides

    def _event(self, e: dict) -> None:
        t, do = seconds(e["t"]), e["do"]
        dur = float(e.get("dur", DEFAULT_DUR.get(do.split("-")[0], 0.3)))
        if do == "show":
            self.vis.add(t, 1.0, dur, "smooth")
        elif do == "fade":
            self.vis.add(t, 0.0, dur, "smooth")
        elif do in ("fly", "fly-to-panel", "fly-to-pillar"):
            to = e.get("to", do[7:] if do != "fly" else "panel")
            self.place.add(t, {"pillar": 0.0, "panel": 1.0}[to], dur, "inout")
        elif do == "nudge":
            self.dx.add(t, float(e.get("dx", 0)), dur)
            self.dy.add(t, float(e.get("dy", 0)), dur)
        elif do == "back":
            self.back.add(t, float(e["alpha"]), dur, "smooth")
        elif do in ("light", "dark", "all-flash", "all-dark"):
            kind = "light" if do in ("light", "all-flash") else "dark"
            # a light's third field is its white flash, a dark's its fade
            third = int(e.get("frames", self.flash * self.fps)) / self.fps if kind == "light" else dur
            targets = self.items if do.startswith("all") else [self.items[item_index(e["item"])]]
            for evs in targets:
                evs.append((t, kind, third))
        else:
            raise ValueError(f"unknown ladder event {do!r}")

    def item_look(self, i: int, t: float):
        """[(colour, alpha), ...]: the layers item i is drawn with at t (a dark fade is two)."""
        on = "gold" if self.on_style == "bronze" else ON
        off = (self.off, self.off_a)
        last, state, before = None, "off", "off"
        for ev in self.items[i]:
            if ev[0] > t:
                break
            before, state, last = state, ("on" if ev[1] == "light" else "off"), ev
        if last is None:
            return [off]
        t0, kind, dur = last
        if kind == "light":
            return [(FLASH, 1.0)] if t - t0 < dur - 1e-9 else [(on, 1.0)]
        if before == "off":
            return [off]
        u = clamp01((t - t0) / max(dur, 1e-6))
        return [(self.off, self.off_a * u), (on, 1.0 - u)]

    def use_span(self, name: str, t0: float, t1: float) -> None:
        """Render with this span's overrides (its lab window keyed to its first light)."""
        self.opts = dict(self.span_opts.get(name, {}))
        if self.opts.get("lab"):
            self.opts["_light"] = self.first_light(t0, t1)

    def first_light(self, t0: float, t1: float) -> float | None:
        ts = [ev[0] for evs in self.items for ev in evs if ev[1] == "light" and t0 <= ev[0] < t1]
        return min(ts) if ts else None

    def visibility(self, t: float) -> float:
        lab = self.opts.get("lab")
        if lab:
            tl = self.opts.get("_light")
            if tl is None:
                return 0.0
            before, hold = float(lab.get("before", 0.25)), float(lab.get("hold", 1.5))
            return ramp_in(t, tl - before, 0.2) * (1 - ramp_in(t, tl - before + hold - 0.35, 0.35))
        return self.vis.value(t)

    def draw(self, c: Canvas, t: float) -> None:
        a = self.visibility(t)
        if a <= 0.002:
            return
        place = self.opts.get("place")
        if place == "grid":
            return self._draw_grid(c, t, a)
        if place == "column":
            x, hy = float(self.opts.get("x", 1688)), float(self.opts.get("head_y", 22))
            y0, dy = float(self.opts.get("y0", 72)), float(self.opts.get("dy", 39))
            bottom = y0 + 13 * dy + LINE + 16
            c.rect(x - 14, hy - 14, 1920 - (x - 14), bottom + 12 - (hy - 14), fill=(0, 0, 0),
                   alpha=a * PANEL["back"], radius=6)
            return self._column(c, t, a, x, hy, y0, dy, range(len(ITEMS)))
        m = {"pillar": 0.0, "panel": 1.0}.get(place, self.place.value(t))
        nudge = self.opts.get("nudge", True)  # false: this span ignores the events' nudges
        ox = (self.dx.value(t) if nudge else 0.0) + float(self.opts.get("dx", 0))
        oy = (self.dy.value(t) if nudge else 0.0) + float(self.opts.get("dy", 0))
        x = lerp(PILLAR["x"], PANEL["x"], m) + ox
        hy = lerp(PILLAR["head_y"], PANEL["head_y"], m) + oy
        y0 = lerp(PILLAR["y0"], PANEL["y0"], m) + oy
        dy = lerp(PILLAR["dy"], PANEL["dy"], m)
        rx, ry, rw, rh = PANEL["rect"]
        pillar_rect = (1680, 70, 240, PILLAR["y0"] + 13 * PILLAR["dy"] + LINE + 16 + 24 - 70)
        bx, by, bw, bh = (lerp(p, q, m) for p, q in zip(pillar_rect, (rx, ry, rw, rh)))
        back = lerp(self.back.value(t), PANEL["back"], m)
        if back > 0:
            c.rect(bx + ox, by + oy, bw, bh, fill=(0, 0, 0), alpha=a * back, radius=8 * m)
        self._column(c, t, a, x, hy, y0, dy, range(len(ITEMS)))

    def _column(self, c, t, a, x, hy, y0, dy, rows, heading=True) -> None:
        if heading:
            c.text(HEADING, x, hy, SCALE, "gold", a)
            c.line(x, hy + 28, x + 16 * len(HEADING), hy + 28, look.pal(100), 2, a * 0.8, cap=cairo.LINE_CAP_BUTT)
        for k, i in enumerate(rows):
            for col, ia in self.item_look(i, t):
                for j, ln in enumerate(LINES[i]):
                    c.text(ln, x, y0 + k * dy + j * LINE, SCALE, col, a * ia)

    def _draw_grid(self, c, t, a) -> None:
        gx, gy = float(self.opts.get("x", 718)), float(self.opts.get("y", 110))
        c.rect(gx - 20, gy - 22, GRID["w"], GRID["h"], fill=(0, 0, 0), alpha=a * PANEL["back"], radius=8)
        c.text(HEADING, gx, gy, SCALE, "gold", a)
        c.line(gx, gy + 28, gx + 16 * len(HEADING), gy + 28, look.pal(100), 2, a * 0.8, cap=cairo.LINE_CAP_BUTT)
        half = (len(ITEMS) + 1) // 2
        self._column(c, t, a, gx, 0, gy + GRID["y0_dy"], GRID["dy"], range(half), heading=False)
        self._column(c, t, a, gx + GRID["col"], 0, gy + GRID["y0_dy"], GRID["dy"], range(half, len(ITEMS)),
                     heading=False)


def ramp_in(t: float, t0: float, dur: float) -> float:
    return EASE["smooth"](clamp01((t - t0) / dur)) if dur > 0 else float(t >= t0)


# ------------------------------------------------------------- rendering ----

_L: Ladder | None = None
_PREVIEW_LABEL: dict[float, tuple[str, str]] = {}
_PREVIEW_OPTS: dict[str, dict] = {}


def _frame(c: Canvas, t: float) -> None:
    _L.draw(c, t)


def _preview_frame(c: Canvas, t: float) -> None:
    c.ctx.set_source_rgb(0, 0, 0)
    c.ctx.paint()
    name, lab = _PREVIEW_LABEL.get(round(t, 4), ("", ""))
    _L.opts = _PREVIEW_OPTS.get(name, {})
    _L.draw(c, t)
    if lab:
        c.text(lab, 40, 1030, 2, "dim")


def _render(job):
    i, t, path, preview = job
    if preview:
        cv = new_canvas(look.W, look.H)
        _preview_frame(cv, t)
        cv.surface.flush()
        from PIL import Image

        Image.frombuffer("RGB", (look.W, look.H), bytes(cv.surface.get_data()), "raw", "BGRX",
                         cv.surface.get_stride(), 1).save(path, compress_level=1)
    else:
        render.render_frame(t).save(path, compress_level=1)
    return i


def _frames(jobs_list, jobs: int) -> None:
    with render._pool(jobs) as pool:
        for k, _ in enumerate(pool.imap_unordered(_render, jobs_list, chunksize=8)):
            if k % 600 == 0:
                print(f"  {k}/{len(jobs_list)} frames", flush=True)


def md5(path: str) -> str:
    import hashlib

    return hashlib.md5(Path(path).read_bytes()).hexdigest()


def tc(t: float) -> str:
    return f"{int(t // 60)}:{t % 60:05.2f}"


def main() -> None:
    global _L
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("events", help="the JSON of events and spans")
    ap.add_argument("--spans", nargs="+", help="render only these spans (by name)")
    ap.add_argument("--preview", action="store_true", help="the spans one after another over black, as an mp4")
    ap.add_argument("--still", nargs="+", help="RGBA stills at these film times")
    ap.add_argument("--overrides", help="a JSON of span overrides by name (place, x, y, dx, dy, lab)")
    ap.add_argument("--out", default=str(filmroot.FILM / "diagrams"), help="the folder for the movies")
    ap.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 4))
    ap.add_argument("--keep-frames", action="store_true", help="keep each span's PNG frames (FILM_SCRATCH)")
    a = ap.parse_args()
    scratch = filmroot.scratch("diagrams") / "frames" / "ladder"
    spec = json.loads(Path(a.events).read_text())
    if a.overrides:
        over = json.loads(Path(a.overrides).read_text())
        for sp in spec.get("spans", []):
            sp.update({k: v for k, v in over.get(sp["name"], {}).items() if not k.startswith("_")})
    _L = Ladder(spec)
    render._FRAME, render._ALPHA = _frame, True
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    fps = _L.fps

    if a.still:
        (out / "stills").mkdir(exist_ok=True)
        for v in a.still:
            t = seconds(v)
            span = next((sp for sp in _L.spans if sp[1] <= t < sp[2]), None)
            if span:
                _L.use_span(*span)
            p = out / "stills" / f"ladder_{tc(t).replace(':', 'm')}.png"
            render.render_frame(t).save(p)
            print(p)
        return

    spans = [s for s in _L.spans if not a.spans or s[0] in a.spans]
    if a.spans and len(spans) != len(a.spans):
        raise SystemExit(f"unknown spans: {set(a.spans) - {s[0] for s in spans}}")
    stamp = time.strftime("%Y%m%d-%H%M%S")

    if a.preview:
        d = scratch / f"{stamp}-preview"
        d.mkdir(parents=True)
        jobs, k = [], 0
        for name, t0, t1 in spans:
            _L.use_span(name, t0, t1)
            _PREVIEW_OPTS[name] = dict(_L.opts)
            for f in range(int(round(t0 * fps)), int(round(t1 * fps))):
                t = f / fps
                _PREVIEW_LABEL[round(t, 4)] = (name, f"{name}  {tc(t)}")
                jobs.append((k, t, d / f"{k:05d}.png", True))
                k += 1
        _frames(jobs, a.jobs)
        p = out / "ladder_preview.mp4"
        render.encode(d, p, fps)
        print(p)
        if not a.keep_frames:
            shutil.rmtree(d)
        return

    for name, t0, t1 in spans:
        _L.use_span(name, t0, t1)
        d = scratch / f"{stamp}-{name}"
        d.mkdir(parents=True)
        jobs = [(k, f / fps, d / f"{k:05d}.png", False)
                for k, f in enumerate(range(int(round(t0 * fps)), int(round(t1 * fps))))]
        _frames(jobs, a.jobs)
        p = out / f"ladder_{name}.mov"
        render.encode(d, p, fps, alpha=True)
        print(p)
        made = {"span": name, "from": t0, "to": t1, "events": Path(a.events).name, "events_md5": md5(a.events),
                "overrides_md5": md5(a.overrides) if a.overrides else None}
        p.with_suffix(".json").write_text(json.dumps(made, indent=1) + "\n")
        if not a.keep_frames:
            shutil.rmtree(d)


if __name__ == "__main__":
    main()
