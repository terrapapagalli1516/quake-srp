"""Every game footage file the cut reads, and how it is made from film/shots.

A recipe renders one or more shot files and writes the files the edit reads under the name the
cut uses (`NAME.mp4`, with `NAME.json`, `NAME.wav`, `NAME.events.json` where the cut's file had
them). Most are one render straight to the file (PLAIN). The rest lay several renders together,
as the production's footage scripts did; each shot file's header ("Assembly") says how, and the
code below is that layout, ported with every length in 1080p pixels times the film's scale `c.s`.
"""

from __future__ import annotations

import inspect
import json
import os
import subprocess
import wave
from collections.abc import Callable
from dataclasses import dataclass, field

import numpy as np
from PIL import Image, ImageDraw

import kit as K
from kit import BRONZE, FPS, HEAD, LAVA, PALE, RUST, Ctx, frames_of, smooth


@dataclass
class Recipe:
    name: str
    outputs: list[str]  # the files it writes, under OUT/footage/game
    shots: list[str]  # the shot files it reads (film/shots/NAME.shot)
    make: Callable[[Ctx], None]
    sidecars: list[str] = field(default_factory=list)  # film/shots/sidecars/ it copies
    note: str = ""
    uses: list[Callable] = field(default_factory=list)  # the layout helpers it shares with other recipes
    spec: str = ""  # a plain recipe's row
    render_res: str | None = None  # render and lay out at this size whatever the film's ("1920x1080"),
    upscale: str = "nearest"  # then enlarged to the film's size this way

    def render_scale(self, film_scale: int) -> int:
        """The scale it is made at: the film's, or its own `render_res`'s if smaller (which must divide it)."""
        if not self.render_res:
            return film_scale
        w, h = (int(v) for v in self.render_res.split("x"))
        own = w // 1920
        assert w == 1920 * own and h == 1080 * own and self.upscale == "nearest", (self.name, self.render_res)
        if own >= film_scale:
            return film_scale
        if film_scale % own:
            raise ValueError(f"{self.name}: its {self.render_res} does not divide the film's size")
        return own

    def source(self) -> str:
        """What decides its bytes besides its inputs and the tool: its own code and the helpers'."""
        return "\n".join([inspect.getsource(f) for f in [self.make, *self.uses]] + [self.spec])


RECIPES: dict[str, Recipe] = {}


def add(r: Recipe) -> None:
    assert r.name not in RECIPES, r.name
    RECIPES[r.name] = r


# ---------------------------------------------------------------------------
# one render, straight to the file
# ---------------------------------------------------------------------------

# The perspective section (PER1-PER6) is made at 1920x1080 and enlarged 2x, nearest neighbour, into
# a 4K film: at 4K the 16-pixel span's error is a quarter the size on the screen, and the section
# explains the error at the size where it shows.
PERSPECTIVE_RES = "1920x1080"
PERSPECTIVE = {"N2a", "N2b", "N2c", "PER4", "N2e.clean", "S34"}

# name: (shot file, keeps its game sound, how its events are timed, the sidecar's name)
#   events: "map"   mp4 frame = floor((film frame - first) * 60 / fps) (the shots of S42 on);
#           "shift" the tool's own times (the shots S01-S41 and the box phase);
#           "raw"   the tool's file as written; None: the cut's file had none.
PLAIN: dict[str, tuple[str, bool, str | None, str | None]] = {
    "S01": ("S01", False, "shift", "S01"),
    "S02": ("S02", True, "shift", "S02"),
    "S03": ("S03", False, "shift", "S03"),
    "S04": ("S04", True, "shift", "S04"),
    "S05": ("S05", True, "shift", "S05"),
    "S06": ("S06", False, "shift", "S06"),
    "S07": ("S07", False, "shift", "S07"),
    "S08": ("S08", False, "shift", "S08"),
    "S09": ("S09", False, "shift", "S09"),
    "S10b": ("S10b", False, "map", "S10b"),
    "S15c": ("S15c", True, "map", "S15c"),
    "S15s": ("S15s", True, "map", "S15s"),
    "S26": ("S26", True, "shift", "S26"),
    "N1b": ("N1b", True, "map", "N1b"),
    "STG": ("STG", True, "map", "STG"),
    "ST1cG": ("ST1cG", True, "map", "ST1cG"),
    "ST2": ("ST2", True, "map", "ST2"),
    "ST3.g065": ("ST3.g065", True, "shift", "ST3.g065"),
    "N4-box": ("N4-box", True, "map", "N4-box"),
    "BURST2r-a": ("BURST2r-a", True, "map", "BURST2r-a"),
    "BURST2r-b": ("BURST2r-b", True, "map", "BURST2r-b"),
    "ST7": ("ST7", True, "map", "ST7"),
    "LAB9b": ("LAB9b", True, "map", "LAB9b"),
    "PER4": ("PER4", False, "map", "PER4"),
    "N2a": ("N2a", True, "map", "N2a"),
    "N2b": ("N2b", False, None, "N2b"),
    "N2c": ("N2c", False, None, "N2c"),
    "N2e.clean": ("N2e.clean", True, "map", "N2e"),  # its sound, events and sidecar keep N2e's names
    "HERO7": ("HERO7", True, "map", "HERO7"),
    "LAB10a2": ("LAB10a2", False, "map", "LAB10a2"),
    "LAB10d": ("LAB10d", True, "map", "LAB10d"),
    "N7a": ("N7a", True, "map", "N7a"),
    "N8a": ("N8a", True, "map", "N8a"),
    "N8b": ("N8b", True, "map", "N8b"),
    "N10": ("N10", True, "map", "N10"),
    "S59b": ("S59b", False, "map", "S59b"),
    "S60": ("S60", True, "map", "S60"),
    "S61": ("S61", True, "map", "S61"),
    "S63": ("S63", False, "map", "S63"),
    "LAB4a3": ("LAB4a3", True, "raw", None),  # 1440x1080 (x scale): the edit centres it; the cut's had no .json
}


def plain(name: str) -> Callable[[Ctx], None]:
    shot, sound, events, side = PLAIN[name]
    base = side or name

    def make(c: Ctx) -> None:
        st = c.stream(shot)
        wr = c.writer(name, st.w, st.h)
        while (f := st.read()) is not None:
            wr.write(f)
        st.close()
        n = wr.close()
        if sound:
            K.keep_wav(c, st, base)
        if events == "map":
            K.events_mapped(c, st, base)
        elif events == "shift":
            K.events_shifted(c, st, base)
        elif events == "raw":
            K.events_raw(c, st, base)
        if side:
            K.sidecar(c, base, n)
    return make


for _name, (_shot, _sound, _events, _side) in PLAIN.items():
    _base = _side or _name
    add(Recipe(_name, [f"{_name}.mp4"] + ([f"{_base}.wav"] if _sound else [])
               + ([f"{_base}.events.json"] if _events else []) + ([f"{_base}.json"] if _side else []),
               [_shot], plain(_name), [_side] if _side else [], spec=repr((_name, PLAIN[_name])),
               render_res=PERSPECTIVE_RES if _name in PERSPECTIVE else None))


# ---------------------------------------------------------------------------
# ST1a2: the tool's own wipe, its 1440x1080 box on a black frame
# ---------------------------------------------------------------------------


def st1a2(c: Ctx) -> None:
    st = c.stream("ST1a2")
    wr = c.writer("ST1a2")
    canvas = c.canvas()
    x0 = 240 * c.s
    while (f := st.read()) is not None:
        canvas[:, x0:x0 + st.w] = f
        wr.write(canvas)
    st.close()
    n = wr.close()
    K.keep_wav(c, st, "ST1a2")
    K.events_split(c, st, "ST1a2")
    K.sidecar(c, "ST1a2", n)


add(Recipe("ST1a2", ["ST1a2.mp4", "ST1a2.wav", "ST1a2.events.json", "ST1a2.events-a.json", "ST1a2.json"],
           ["ST1a2"], st1a2, ["ST1a2"]))


# ---------------------------------------------------------------------------
# side by side in the 4:3 box: ST1b, LAB1
# ---------------------------------------------------------------------------


def box_label(c: Ctx, out, s: str, x: int, y: int = 24, right: bool = False) -> None:
    """`s` in conchars 3x on an 82% dark band, its band's left (or right) edge at x (output pixels)."""
    lab = c.text(s, 3)
    w = lab.shape[1] + 32 * c.s
    x0 = x - w if right else x
    y = y * c.s
    K.band(out, x0, y, x0 + w, y + lab.shape[0] + 24 * c.s, 0.82)
    K.paste(out, lab, x0 + 16 * c.s, y + 12 * c.s)


def halves(c: Ctx, a: np.ndarray, b: np.ndarray, la: str, lb: str) -> np.ndarray:
    """A and B side by side in the box at x 240: the central 720 columns of each side's 1440x1080
    picture, a 4 px bronze seam, the labels at the top."""
    s = c.s
    out = c.canvas()
    c0 = (1440 - 720) // 2 * s
    bx, bw, hw = 240 * s, 1440 * s, 720 * s
    out[:, bx:bx + hw] = a[:, c0:c0 + hw]
    out[:, bx + hw:bx + bw] = b[:, c0:c0 + hw]
    out[:, (240 + 718) * s:(240 + 722) * s] = BRONZE
    box_label(c, out, la, bx + 16 * s)
    box_label(c, out, lb, bx + bw - 16 * s, right=True)
    return out


def st1b(c: Ctx) -> None:
    a, b = c.stream("ST1b-off", threads=8), c.stream("ST1b-on", threads=8)
    wr = c.writer("ST1b")
    while (fa := a.read()) is not None and (fb := b.read()) is not None:
        wr.write(halves(c, fa, fb, "ID'S: BAKED, STILL", "SLOP: FLICKERING"))
    a.close(), b.close()
    n = wr.close()
    K.keep_wav(c, b, "ST1b")
    K.events_shifted(c, b, "ST1b")
    K.events_raw(c, a, "ST1b", ".events-a.json")
    K.sidecar(c, "ST1b", n)


add(Recipe("ST1b", ["ST1b.mp4", "ST1b.wav", "ST1b.events.json", "ST1b.events-a.json", "ST1b.json"],
           ["ST1b-off", "ST1b-on"], st1b, ["ST1b"], uses=[halves, box_label]))


def lab1(c: Ctx) -> None:
    a, b = c.stream("LAB1-off-light", threads=4), c.stream("LAB1-on-light", threads=4)
    at, bt = c.stream("LAB1-off", threads=4), c.stream("LAB1-on", threads=4)
    wr = c.writer("LAB1")
    k = 0
    while True:
        fa, fb, ta, tb = a.read(), b.read(), at.read(), bt.read()
        if fa is None or fb is None or ta is None or tb is None:
            break
        u = smooth((k / FPS - HEAD - 4.5) / 0.6)
        wr.write(halves(c, K.blend(fa, ta, u), K.blend(fb, tb, u), "ID'S: BAKED, STILL", "SLOP: FLICKERING"))
        k += 1
    for st in (a, b, at, bt):
        st.close()
    n = wr.close()
    K.keep_wav(c, b, "LAB1")
    K.events_shifted(c, b, "LAB1")
    K.events_raw(c, a, "LAB1", ".events-a.json")
    K.sidecar(c, "LAB1", n)


add(Recipe("LAB1", ["LAB1.mp4", "LAB1.wav", "LAB1.events.json", "LAB1.events-a.json", "LAB1.json"],
           ["LAB1-off-light", "LAB1-on-light", "LAB1-off", "LAB1-on"], lab1, ["LAB1"],
           uses=[halves, box_label]))


# ---------------------------------------------------------------------------
# two 940x1040 panels: F13, LAB5 (2x crops), S45 (1x crops), S46a (a crop that follows), N7b
# ---------------------------------------------------------------------------


def two_crops(c: Ctx, name: str, left: str, right: str, crop, k: int, labels, repeat: int = 1,
              sound: str | None = None, events: str = "left"):
    """The (x, y, w, h) crops of two renders, enlarged k times, at (13,20) and (967,20) of a black
    frame, labelled; each film frame written `repeat` times."""
    s = c.s
    a = c.stream(left, threads=8)
    b = c.stream(right, threads=8)
    x, y, cw, ch = (v * s for v in crop)
    wr = c.writer(name)
    while (fa := a.read()) is not None and (fb := b.read()) is not None:
        out = c.canvas()
        K.place(out, K.zoom(fa[y:y + ch, x:x + cw], k), 13 * s, 20 * s)
        K.place(out, K.zoom(fb[y:y + ch, x:x + cw], k), 967 * s, 20 * s)
        c.label(out, labels[0], (13 + 24) * s, (20 + 24) * s)
        c.label(out, labels[1], (967 + 24) * s, (20 + 24) * s)
        for _ in range(repeat):
            wr.write(out)
    a.close(), b.close()
    n = wr.close()
    if sound:
        K.keep_wav(c, {"left": a, "right": b}[sound], name)
    K.events_mapped(c, {"left": a, "right": b}[events], name)
    K.sidecar(c, name, n)


def f13(c: Ctx) -> None:
    two_crops(c, "F13", "F13-id", "F13-barrels", (725, 450, 470, 520), 2, ("nails: id's", "nails: from the barrels"),
              sound="right", events="right")


def lab5(c: Ctx) -> None:
    two_crops(c, "LAB5", "LAB5-id", "LAB5-slop", (700, 40, 470, 520), 2, ("id's", "slop"), events="right")


def s45(c: Ctx) -> None:
    two_crops(c, "S45", "S45-id", "S45-slop", (350, 20, 940, 1040), 1, ("id's", "slop"), repeat=2, events="left")


add(Recipe("F13", ["F13.mp4", "F13.wav", "F13.events.json", "F13.json"], ["F13-id", "F13-barrels"], f13, ["F13"],
           uses=[two_crops]))
add(Recipe("LAB5", ["LAB5.mp4", "LAB5.events.json", "LAB5.json"], ["LAB5-id", "LAB5-slop"], lab5, ["LAB5"],
           uses=[two_crops]))
add(Recipe("S45", ["S45.mp4", "S45.events.json", "S45.json"], ["S45-id", "S45-slop"], s45, ["S45"],
           uses=[two_crops]))


def grunt_track(c: Ctx) -> np.ndarray:
    """The grunt's screen centre per film frame, in 1080p pixels: S46a-track (the moving models'
    wireframe on black, at its own 960x540, whatever the film's size), each frame's lit pixels less
    those lit in most frames; a cubic through them is the crop's path."""
    st = c.stream("S46a-track", native=True)
    masks = [f.max(axis=2) > 40 for f in st.all()]
    still = np.mean(masks, axis=0) > 0.6
    cs = []
    for m in masks:
        ys, xs = np.nonzero(m & ~still)
        cs.append((xs.mean(), ys.mean()) if len(xs) > 20 else (np.nan, np.nan))
    cen = np.array(cs) * (1920 / st.w)
    for k in range(2):
        v = cen[:, k]
        ok = ~np.isnan(v)
        if ok.any():
            cen[:, k] = np.interp(np.arange(len(v)), np.flatnonzero(ok), v[ok])
    t = np.linspace(-1, 1, len(cen))
    return np.stack([np.polyval(np.polyfit(t, cen[:, k], 3), t) for k in range(2)], axis=1)


def s46a(c: Ctx) -> None:
    s = c.s
    cs = grunt_track(c)
    cw, ch = 470, 520
    held, blended = c.stream("S46a-held", threads=8), c.stream("S46a-blended", threads=8)
    wr = c.writer("S46a")
    k = 0
    while (a := held.read()) is not None and (b := blended.read()) is not None:
        cx, cy = cs[min(k, len(cs) - 1)]
        x0 = int(np.clip(round(cx - cw / 2), 0, 1920 - cw)) * s
        y0 = int(np.clip(round(cy - ch / 2 - 10), 0, 1080 - ch)) * s
        pa, pb = a[y0:y0 + ch * s, x0:x0 + cw * s], b[y0:y0 + ch * s, x0:x0 + cw * s]
        out = c.canvas()
        K.place(out, K.zoom(pa, 2), 13 * s, 20 * s)
        K.place(out, K.zoom(pb, 2), 967 * s, 20 * s)
        c.label(out, "poses held (id's)", (13 + 24) * s, (20 + 24) * s)
        c.label(out, "poses blended (slop)", (967 + 24) * s, (20 + 24) * s)
        for _ in range(2):
            wr.write(out)
        k += 1
    held.close(), blended.close()
    n = wr.close()
    K.events_mapped(c, held, "S46a")
    K.sidecar(c, "S46a", n)


add(Recipe("S46a", ["S46a.mp4", "S46a.events.json", "S46a.json"], ["S46a-track", "S46a-held", "S46a-blended"],
           s46a, ["S46a"], uses=[grunt_track]))


def keycap(c: Ctx, letter: str, lit: float) -> np.ndarray:
    """A keycap, 84 px at 1080p, in line art: bronze, filled pale flame when lit."""
    s = c.s
    size = 84 * s
    img = Image.new("RGBA", (size + 8 * s, size + 8 * s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    fill = tuple(int(a * lit + b * (1 - lit)) for a, b in zip(PALE, (16, 12, 8))) + (235,)
    d.rounded_rectangle((4 * s, 4 * s, size + 4 * s, size + 4 * s), radius=12 * s, fill=fill,
                        outline=BRONZE + (255,), width=4 * s)
    arr = np.asarray(img).copy()
    tt = c.text(letter, 5, font="white", shadow=0)
    tt[..., :3] = (16, 12, 8) if lit >= 0.5 else BRONZE
    th, tw = tt.shape[:2]
    oy, ox = (arr.shape[0] - th) // 2 + 2 * s, (arr.shape[1] - tw) // 2 + 2 * s
    a = tt[..., 3:4].astype(np.float32) / 255
    reg = arr[oy:oy + th, ox:ox + tw].astype(np.float32)
    reg[..., :3] = reg[..., :3] * (1 - a) + tt[..., :3] * a
    reg[..., 3:4] = np.maximum(reg[..., 3:4], tt[..., 3:4])
    arr[oy:oy + th, ox:ox + tw] = reg.astype(np.uint8)
    return arr


def n7b(c: Ctx) -> None:
    s = c.s
    left, right = c.stream("N7b"), c.stream("N7b-2026")
    wr = c.writer("N7b")
    cx, cy, cw, ch = ((1920 - 940) // 2) * s, 20 * s, 940 * s, 1040 * s
    head = frames_of(HEAD)
    caps = {(k, lit): keycap(c, k, lit) for k in "WASD" for lit in (0.0, 1.0)}
    held = {"A": (0.3, 0.8), "D": (1.2, 2.0)}  # shot seconds the key acts
    k = 0
    while (a := left.read()) is not None and (b := right.read()) is not None:
        t = (k - head) / FPS
        out = c.canvas()
        K.place(out, a[cy:cy + ch, cx:cx + cw], 13 * s, 20 * s)
        K.place(out, b[cy:cy + ch, cx:cx + cw], 967 * s, 20 * s)
        c.label(out, "1996", (13 + 24) * s, (20 + 24) * s)
        c.label(out, "2026", (967 + 24) * s, (20 + 24) * s)
        for px in (13, 967):
            gx, gy = px + 470 - 3 * 46, 1040 - 2 * 96
            for key, (dx, dy) in {"W": (1, 0), "A": (0, 1), "S": (1, 1), "D": (2, 1)}.items():
                lit = 1.0 if key in held and held[key][0] <= t < held[key][1] else 0.0
                K.paste(out, caps[(key, lit)], (gx + dx * 92) * s, (gy + dy * 92) * s)
        wr.write(out)
        k += 1
    left.close(), right.close()
    n = wr.close()
    K.keep_wav(c, left, "N7b")
    K.events_mapped(c, left, "N7b")
    K.sidecar(c, "N7b", n)


add(Recipe("N7b", ["N7b.mp4", "N7b.wav", "N7b.events.json", "N7b.json"], ["N7b", "N7b-2026"], n7b, ["N7b"],
           uses=[keycap]))


# ---------------------------------------------------------------------------
# S48: two 940x529 panels at 1/16 speed
# ---------------------------------------------------------------------------


def s48(c: Ctx) -> None:
    s = c.s
    ids, slop = c.stream("S48-id", threads=8), c.stream("S48-slop", threads=8)
    wr = c.writer("S48")
    py = 196
    while (a := ids.read()) is not None and (b := slop.read()) is not None:
        out = c.canvas()
        K.place(out, a, 20 * s, py * s)
        K.place(out, b, 960 * s, py * s)
        c.label(out, "id's", (20 + 18) * s, (py + 18) * s)
        c.label(out, "slop", (960 + 18) * s, (py + 18) * s)
        for _ in range(4):
            wr.write(out)
    ids.close(), slop.close()
    n = wr.close()
    K.events_mapped(c, ids, "S48")
    K.sidecar(c, "S48", n)


add(Recipe("S48", ["S48.mp4", "S48.events.json", "S48.json"], ["S48-id", "S48-slop"], s48, ["S48"]))


# ---------------------------------------------------------------------------
# F06: the gamepad that shakes on each hit
# ---------------------------------------------------------------------------


def f06(c: Ctx) -> None:
    s = c.s
    side = json.loads((K.SIDECARS / "F06.json").read_text())
    head = frames_of(side["head_s"])
    # The red damage flashes (each hit), as the cut's sidecar times them.
    jumps = [round((t + side["head_s"]) * FPS) for t in side["red_flash_shot_s"]]
    st = c.stream("F06")
    padw = 360
    pad = Image.new("RGBA", ((padw + 20) * s, 250 * s), (0, 0, 0, 0))
    K.draw_pad(pad, 10 * s, 10 * s, padw * s, BRONZE + (255,), s)
    pad_a = np.asarray(pad)
    rumble = c.text("rumble", 2)
    rng = np.random.default_rng(7)
    px, py = 1920 - padw - 90, 1080 - 250 - 150
    wr = c.writer("F06")
    k = 0
    while (f := st.read()) is not None:
        out = f.copy()
        dx = dy = 0
        # 0.3 s of rumble from every red flash (each hit) from the shot's start on
        if [j for j in jumps if j >= head - 2 and 0 <= k - j < frames_of(0.3)]:
            dx, dy = (int(v) for v in rng.integers(-6, 7, 2))
        K.band(out, (px - 16) * s, (py - 10) * s, (px + padw + 36) * s, (py + 240) * s, 0.45)
        K.paste(out, pad_a, (px + dx) * s, (py + dy) * s)
        last = max([j for j in jumps if head - 2 <= j <= k], default=None)
        if last is not None:
            a = 1.0 - smooth((k - last - frames_of(0.6)) / frames_of(0.3))
            if a > 0:
                K.paste(out, rumble, px * s - rumble.shape[1] - 24 * s, (py + 100) * s, a)
        wr.write(out)
        k += 1
    st.close()
    n = wr.close()
    K.keep_wav(c, st, "F06")
    K.events_mapped(c, st, "F06")
    K.sidecar(c, "F06", n)


add(Recipe("F06", ["F06.mp4", "F06.wav", "F06.events.json", "F06.json"], ["F06"], f06, ["F06"]))


# ---------------------------------------------------------------------------
# S13: id's 16-pixel segments sweeping in over the picture
# ---------------------------------------------------------------------------


def s13(c: Ctx) -> None:
    a, b = c.stream("S13-game"), c.stream("S13-segments")
    wr = c.writer("S13")
    k = 0
    while (fa := a.read()) is not None and (fb := b.read()) is not None:
        x = round(1920 * smooth((k / FPS - HEAD - 0.9) / 0.6)) * c.s
        out = fa.copy()
        out[:, :x] = fb[:, :x]
        wr.write(out)
        k += 1
    a.close(), b.close()
    K.sidecar(c, "S13", wr.close())


add(Recipe("S13", ["S13.mp4", "S13.json"], ["S13-game", "S13-segments"], s13, ["S13"]))


# ---------------------------------------------------------------------------
# S34: the four spans, 2x crops in a grid
# ---------------------------------------------------------------------------


def s34(c: Ctx) -> None:
    s = c.s
    spans = [64, 16, 8, 1]
    files = {64: "S33-S34-64", 16: "S33-S34-16", 8: "S33-S34-8", 1: "S33-S34-exact"}
    a34, n = frames_of(7.4 - HEAD), frames_of(17.4)  # S34 is the master render's frames 384..1043
    st = {sp: c.stream(files[sp], threads=4, frames=(a34, n)) for sp in spans}
    x, y, w, h = (v * s for v in (420, 120, 470, 260))
    lab = {64: c.text("64"), 16: c.text("id's 16"), 8: c.text("8 (slop)"), 1: c.text("exact")}
    cap = c.text("+10% time, 1/4 of the error", 2)
    wr = c.writer("S34")
    for k in range(a34, n):
        fr = {sp: st[sp].read() for sp in spans}
        if any(v is None for v in fr.values()):
            break
        s34 = k / FPS - 7.4
        tint = 0.6 * smooth((s34 - 4.3) / 0.4)
        out = c.canvas()
        ex = fr[1][y:y + h, x:x + w]
        for j, sp in enumerate(spans):
            r, cc = divmod(j, 2)
            cell = fr[sp][y:y + h, x:x + w].copy()
            mask = np.any(cell != ex, axis=2)
            if tint > 0:
                cell[mask] = (cell[mask] * (1 - tint) + np.array(LAVA) * tint).astype(np.uint8)
            cell = K.zoom(cell, 2)
            ox, oy = (20 + cc * 960) * s, (13 + r * 540) * s
            K.place(out, cell, ox, oy)
            K.band(out, ox, oy, ox + lab[sp].shape[1] + 32 * s, oy + lab[sp].shape[0] + 24 * s, 0.6)
            K.paste(out, lab[sp], ox + 16 * s, oy + 12 * s)
            if sp == 8:
                K.rect(out, ox - 4 * s, oy - 4 * s, ox + 944 * s, oy + 524 * s, RUST, 4 * s)
                ca = smooth((s34 - 5.2) / 0.4)
                if ca > 0:
                    cx0 = ox + 940 * s - cap.shape[1] - 16 * s
                    cy0 = oy + 520 * s - cap.shape[0] - 14 * s
                    K.band(out, cx0 - 12 * s, cy0 - 10 * s, ox + 940 * s, oy + 520 * s, 0.7 * ca)
                    K.paste(out, cap, cx0, cy0, ca)
        wr.write(out)
    for v in st.values():
        v.close()
    K.sidecar(c, "S34", wr.close())


add(Recipe("S34", ["S34.mp4", "S34.json"], ["S33-S34-64", "S33-S34-16", "S33-S34-8", "S33-S34-exact"], s34,
           ["S34"], render_res=PERSPECTIVE_RES))


# ---------------------------------------------------------------------------
# S38 and S39: 4:3 widening to id's 16:9, then Hor+ (one pass, three renders)
# ---------------------------------------------------------------------------


def s38_s39(c: Ctx) -> None:
    s = c.s
    W, H = c.W, c.H
    a = c.stream("S38-S41-4x3", threads=5)  # 1920x1440 (x scale), frames 0..617
    b = c.stream("S38-S41-id", threads=5)  # frames 240..677
    cc = c.stream("S38-S41-horplus", threads=5)  # frames 498..965
    w38, w39 = c.writer("S38"), c.writer("S39")
    fb = fc = None
    for k in range(frames_of(16.1)):
        t = k / FPS
        fa = a.read() if k < frames_of(10.3) else None
        if frames_of(4.0) <= k < frames_of(11.3):
            fb = b.read()
        if k >= frames_of(8.3):
            fc = cc.read()
        if k < frames_of(10.3):  # S38
            sh = t - 1.0
            u = smooth((sh - 3.5) / 1.5)
            g = 1.0 + u / 3.0  # the box's scale: 1 (1440x1080) to 4/3 (1920x1440, cut to the frame)
            bw, bh = round(1440 * g), round(1080 * g)
            pic = K.area(fa, bw * s, bh * s) if bw < 1920 else fa
            out = c.canvas()
            K.place(out, pic, (1920 - bw) // 2 * s, (1080 - bh) // 2 * s)
            la = smooth((sh - 2.0) / 0.5)
            if la > 0 and u < 1:
                half = 405 * g
                y0, y1 = round(540 - half) * s, round(540 + half) * s
                xa, xb = max(0, (1920 - bw) // 2) * s, min(1920, (1920 + bw) // 2) * s
                t3 = 3 * s
                if y0 > 0:
                    K.band(out, xa, 0, xb, y0, 0.45 * la)
                    reg = out[max(0, y0 - t3):y0, xa:xb]
                    out[max(0, y0 - t3):y0, xa:xb] = K.blend(reg, np.full_like(reg, RUST), la)
                if y1 < H:
                    K.band(out, xa, y1, xb, H, 0.45 * la)
                    reg = out[y1:min(H, y1 + t3), xa:xb]
                    out[y1:min(H, y1 + t3), xa:xb] = K.blend(reg, np.full_like(reg, RUST), la)
            # late in the widening, settle onto the native 16:9 render (the same picture)
            if u > 0.85 and fb is not None:
                out = K.blend(out, fb, smooth((u - 0.85) / 0.15))
            w38.write(out)
        if k >= frames_of(8.3):  # S39
            sh = t - 9.3
            out = K.blend(fb, fc, smooth(sh / 1.0))
            oa = smooth((sh - 1.3) / 0.5)
            if oa > 0:
                out = out.copy()
                K.rect(out, 240 * s, 0, 1680 * s, 1080 * s, BRONZE, 4 * s, oa)
            w39.write(out)
    for st in (a, b, cc):
        st.close()
    n38, n39 = w38.close(), w39.close()
    assert W == 1920 * s
    K.events_shifted(c, a, "S38", 0.0, n38 / FPS)
    K.events_shifted(c, cc, "S39", 8.3, n39 / FPS)
    K.sidecar(c, "S38", n38)
    K.sidecar(c, "S39", n39)


add(Recipe("S38", ["S38.mp4", "S38.events.json", "S38.json", "S39.mp4", "S39.events.json", "S39.json"],
           ["S38-S41-4x3", "S38-S41-id", "S38-S41-horplus"], s38_s39, ["S38", "S39"],
           note="S38 and S39 are one pass (S39 is made with S38)"))


# ---------------------------------------------------------------------------
# S62.clean: nine renders, one more option off at each, dissolved
# ---------------------------------------------------------------------------


def s62(c: Ctx) -> None:
    streams = [c.stream(f"S62-{i}", threads=2) for i in range(9)]
    head = frames_of(HEAD)
    wr = c.writer("S62.clean")
    k = 0
    while True:
        fs = [st.read() for st in streams]
        if any(f is None for f in fs):
            break
        t = (k - head) / FPS
        step = int(np.clip(np.floor((t - 0.4) / 0.4) + 1, 0, len(fs) - 1)) if t >= 0.4 else 0
        if step > 0:
            out = K.blend(fs[step - 1], fs[step], smooth((t - 0.4 * step) / 0.2))
        else:
            out = fs[step]
        wr.write(out)
        k += 1
    for st in streams:
        st.close()
    n = wr.close()
    K.events_mapped(c, streams[0], "S62")
    K.sidecar(c, "S62", n)


add(Recipe("S62.clean", ["S62.clean.mp4", "S62.events.json", "S62.json"], [f"S62-{i}" for i in range(9)], s62,
           ["S62"]))


# ---------------------------------------------------------------------------
# N3w-box.zoom: a 480 Hz run, the eight frames of each 60 Hz frame along the box's top
# ---------------------------------------------------------------------------


def n3w_box_zoom(c: Ctx) -> None:
    s = c.s
    bx0, bx1 = 240, 1680
    tw, th = 176, 132
    x0, y0 = bx0 + (bx1 - bx0 - 8 * tw) // 2, 16
    cx, cy = (bx0 + bx1) // 2 - tw // 2, 400 - th // 2  # the zoom tiles: 1:1 crops about (960,400)
    st = c.stream("N3w-box")
    nums = [K.tint_text(c.text(str(i + 1), 2), PALE) for i in range(8)]
    hz = c.text("480 HZ", 3)
    wr = c.writer("N3w-box.zoom")
    group: list[np.ndarray] = []
    while (f := st.read()) is not None:
        group.append(f)
        if len(group) < 8:
            continue
        out = group[0].copy()
        K.band(out, bx0 * s, 0, bx1 * s, (y0 + th + 16) * s, 0.55)
        for i, g in enumerate(group):
            K.place(out, g[cy * s:(cy + th) * s, cx * s:(cx + tw) * s], (x0 + i * tw) * s, y0 * s)
            K.paste(out, nums[i], (x0 + i * tw + 6) * s, (y0 + 5) * s)
        for i in range(1, 8):
            out[y0 * s:(y0 + th) * s, (x0 + i * tw - 1) * s:(x0 + i * tw + 1) * s] = 0
        c.label(out, "480 HZ", bx1 * s - hz.shape[1] - 36 * s, (y0 + th + 34) * s)
        wr.write(out)
        group = []
    st.close()
    n = wr.close()
    if group:
        raise RuntimeError(f"N3w-box: {len(group)} 480 Hz frames left over")
    K.keep_wav(c, st, "N3w-box")
    K.events_mapped(c, st, "N3w-box")
    K.sidecar(c, "N3w-box", n)


add(Recipe("N3w-box.zoom", ["N3w-box.zoom.mp4", "N3w-box.wav", "N3w-box.events.json", "N3w-box.json"],
           ["N3w-box"], n3w_box_zoom, ["N3w-box"]))


# ---------------------------------------------------------------------------
# N9-click-id.wav, N9-click-slop.wav: one hum1.wav loop through id's mixer and slop's
# ---------------------------------------------------------------------------


def n9_click(c: Ctx) -> None:
    for rate, fixes, out in ((11025, False, "N9-click-id"), (48000, True, "N9-click-slop")):
        txt = c.work / f"hum1-{rate}.txt"
        raw = c.work / f"hum1-{rate}.raw"
        txt.write_text(K.loop_script(rate))
        subprocess.run([str(c.quaketool), "sndscript", str(c.pak), str(txt), str(raw), "--rate", str(rate),
                        *(["--fixes"] if fixes else [])], check=True, stdout=subprocess.DEVNULL)
        part = c.game / f"{out}.part.wav"
        with wave.open(str(part), "wb") as w:
            w.setnchannels(2)
            w.setsampwidth(2)
            w.setframerate(rate)
            w.writeframes(np.fromfile(raw, "<i2").tobytes())
        os.replace(part, c.game / f"{out}.wav")


add(Recipe("N9-click", ["N9-click-id.wav", "N9-click-slop.wav"], [], n9_click,
           note="sound only: quaketool sndscript, the same at any size"))

# The cut's footage files that are not made here: the proof's frames and S21m (proof.py).
SIZE_FREE = {"N9-click"}  # recipes whose output does not depend on the film's size
