"""Lettering: Quake's console font (conchars), the status bar's big digits,
a clean sans for dense labels, and a small equation layout in Quake's font.

conchars is id's 8x8 font from gfx.wad: codes 0-127 are the grey letters,
128-255 the bronze ones id's menus use. Its lower case is a set of small
capitals (u, z, x read as smaller U, Z, X), which is what the equations use
for their variables. A few maths signs id's font lacks (÷ × → ≈ Δ ...) are
drawn here in its style: two-pixel strokes over a one-pixel shadow.

Everything is drawn with nearest-neighbour scaling at whole multiples, at
whole-pixel positions, so it stays as crisp as the game draws it.
"""

from __future__ import annotations

import functools
import re
from dataclasses import dataclass, field

import cairo
import numpy as np

from . import look
from .quake import conchars, palette, wad_pic

# ----------------------------------------------------- extra glyphs ----

_EXTRA_MASKS = {
    "÷": ["........", "...##...", "........", ".######.", "........", "...##...", "........", "........"],
    "×": ["........", "........", ".##..##.", "..####..", "...##...", "..####..", ".##..##.", "........"],
    "→": ["........", "....#...", "....##..", "#######.", "....##..", "....#...", "........", "........"],
    "←": ["........", "..#.....", ".##.....", ".#######", ".##.....", "..#.....", "........", "........"],
    "↑": ["...##...", "..####..", ".######.", "...##...", "...##...", "...##...", "...##...", "........"],
    "↓": ["...##...", "...##...", "...##...", "...##...", ".######.", "..####..", "...##...", "........"],
    "≈": ["........", "........", ".###..#.", "#...##..", "........", ".###..#.", "#...##..", "........"],
    "Δ": ["........", "...##...", "..####..", "..#..#..", ".##..##.", ".#....#.", "#######.", "........"],
    "·": ["........", "........", "........", "...##...", "...##...", "........", "........", "........"],
    "±": ["........", "...##...", ".######.", "...##...", "........", ".######.", "........", "........"],
    "≤": ["........", "....##..", "..##....", "##......", "..##....", "....##..", "######..", "........"],
    "≥": ["........", "##......", "..##....", "....##..", "..##....", "##......", "######..", "........"],
    "°": [".##.....", "#..#....", ".##.....", "........", "........", "........", "........", "........"],
    "√": ["....####", "....#...", "....#...", "#..#....", ".#.#....", "..##....", "...#....", "........"],
    "∞": ["........", "........", ".##..##.", "#..##..#", "#..##..#", ".##..##.", "........", "........"],
    "∝": ["........", "........", ".##.###.", "#..#....", "#..#....", ".##.###.", "........", "........"],
    "✓": ["........", "......##", ".....##.", "##..##..", ".####...", "..##....", "........", "........"],
    "′": ["..##....", "..##....", ".##.....", "........", "........", "........", "........", "........"],
    "│": ["...##...", "...##...", "...##...", "...##...", "...##...", "...##...", "...##...", "........"],
}
EXTRA_CHARS = list(_EXTRA_MASKS)

# TeX-ish names for the extras, for equations typed in ASCII.
NAMES = {
    "div": "÷", "times": "×", "to": "→", "gets": "←", "uparrow": "↑", "downarrow": "↓", "approx": "≈",
    "Delta": "Δ", "cdot": "·", "pm": "±", "le": "≤", "ge": "≥", "deg": "°", "sqrt": "√", "infty": "∞",
    "propto": "∝", "check": "✓", "prime": "′", "vert": "│",
}


def _extra_block() -> np.ndarray:
    """The extra glyphs as palette indices in conchars' style, one 8x8 cell each, 16 to a row."""
    n = len(EXTRA_CHARS)
    out = np.zeros((8 * ((n + 15) // 16), 128), np.uint8)
    for k, ch in enumerate(EXTRA_CHARS):
        m = np.array([[c == "#" for c in row] for row in _EXTRA_MASKS[ch]], bool)
        cell = np.zeros((8, 8), np.uint8)
        shadow = np.zeros_like(m)
        shadow[1:, 1:] = m[:-1, :-1]
        shadow[:, 1:] |= m[:, :-1]
        shadow[1:, :] |= m[:-1, :]
        cell[shadow & ~m] = 2
        yy, xx = np.nonzero(m)
        cell[yy, xx] = 7 + ((xx * 3 + yy * 5) % 3)  # conchars' mottled greys 7-9
        r, col = divmod(k, 16)
        out[8 * r : 8 * r + 8, 8 * col : 8 * col + 8] = cell
    return out


@functools.lru_cache(maxsize=1)
def _glyph_indices() -> np.ndarray:
    """conchars (128x128) with the extras appended below, 16 to a row."""
    return np.vstack([conchars(), _extra_block()])


def _glyph_cell(ch: str, gold: bool) -> tuple[int, int]:
    """Atlas pixel offset (x, y) of a character's 8x8 cell."""
    if ch in _EXTRA_MASKS:
        r, col = divmod(EXTRA_CHARS.index(ch), 16)
        return 8 * col, 128 + 8 * r
    code = ord(ch) & 0x7F if ord(ch) < 256 else ord("?")
    if gold:
        code |= 0x80
    return 8 * (code % 16), 8 * (code // 16)


def _surface_from_rgba(rgba: np.ndarray) -> cairo.ImageSurface:
    """(h, w, 4) straight-alpha RGBA uint8 -> a premultiplied cairo ARGB32 surface."""
    h, w, _ = rgba.shape
    a = rgba[..., 3:4].astype(np.uint16)
    pm = (rgba[..., :3].astype(np.uint16) * a // 255).astype(np.uint8)
    bgra = np.dstack([pm[..., 2], pm[..., 1], pm[..., 0], rgba[..., 3]])
    surf = cairo.ImageSurface(cairo.FORMAT_ARGB32, w, h)
    stride = surf.get_stride()
    buf = np.ndarray((h, stride // 4, 4), np.uint8, buffer=surf.get_data())
    buf[:, :w] = bgra
    surf.mark_dirty()
    return surf


# The two sets as id drew them are dim on a modern screen next to the diagrams'
# lines, so 'white' and 'gold' lift them by a gain; 'id_white' and 'id_gold'
# are the palette's colours exactly.
RAW_STYLES = {"white": 1.3, "gold": 1.45, "id_white": 1.0, "id_gold": 1.0}


@functools.lru_cache(maxsize=64)
def _atlas(tint, gain: float = 1.0) -> cairo.ImageSurface:
    """The glyph atlas: palette colours times `gain` (tint None), or every glyph recoloured to `tint`.

    A tint keeps each glyph's own shading: a pixel's grey, relative to the
    letters' middle grey (palette 8, 123), scales the tint; the shadow (palette 2)
    stays a quarter of it.
    """
    idx = _glyph_indices()
    rgb = palette()[idx].astype(np.float64)
    alpha = np.where(idx == 0, 0, 255).astype(np.uint8)
    if tint is not None:
        lum = rgb.mean(axis=2, keepdims=True) / 123.0
        rgb = lum * np.array(tint)[None, None, :] * 255.0
    rgb = np.clip(rgb * gain, 0, 255)
    rgba = np.dstack([rgb.astype(np.uint8), alpha])
    return _surface_from_rgba(rgba)


def _atlas_for(c) -> cairo.ImageSurface:
    if c is None:
        c = "white"
    if isinstance(c, str) and c in RAW_STYLES:
        return _atlas(None, RAW_STYLES[c])
    return _atlas(tuple(round(x, 4) for x in look.color(c)))


def _paint(ctx: cairo.Context, surf: cairo.ImageSurface, sx: int, sy: int, w: int, h: int, x: float, y: float, scale: float):
    """Paint the (sx, sy, w, h) cell of an atlas at (x, y), scaled, nearest-neighbour."""
    ctx.save()
    ctx.translate(x, y)
    ctx.scale(scale, scale)
    ctx.set_source_surface(surf, -sx, -sy)
    ctx.get_source().set_filter(cairo.FILTER_NEAREST)
    ctx.rectangle(0, 0, w, h)
    ctx.fill()
    ctx.restore()


# ------------------------------------------------------- conchars ----

GLYPH = 8  # conchars cell, pixels
BASELINE = 7  # rows above the baseline in a cell (the 8th row is the shadow)


def qtext_width(s: str, scale: int) -> int:
    return len(s) * GLYPH * scale


def draw_qtext(ctx: cairo.Context, s: str, x: float, y: float, scale: int = 3, color="white", alpha: float = 1.0) -> None:
    """Quake's font at a whole-number scale, top-left at (x, y).

    color: 'white' (id's grey letters), 'gold' (id's bronze menu letters), both
    lifted a little for the screen ('id_white', 'id_gold': exactly id's), or any
    colour (a role name, palette index, or rgb), which tints the grey set.
    """
    if alpha <= 0 or not s:
        return
    gold = color in ("gold", "id_gold")
    surf = _atlas_for(color)
    x, y = round(x), round(y)
    if alpha < 1:
        ctx.push_group()
    for i, ch in enumerate(s):
        if ch == " ":
            continue
        sx, sy = _glyph_cell(ch, gold)
        _paint(ctx, surf, sx, sy, GLYPH, GLYPH, x + i * GLYPH * scale, y, scale)
    if alpha < 1:
        ctx.pop_group_to_source()
        ctx.paint_with_alpha(alpha)


# ------------------------------------------------- big status digits ----

_BIG = {str(d): f"num_{d}" for d in range(10)} | {"-": "num_minus", "/": "num_slash", ":": "num_colon"}


@functools.lru_cache(maxsize=None)
def _big_pic(ch: str, alt: bool) -> tuple[cairo.ImageSurface, int, int]:
    """A status-bar digit as a surface (transparent where id's qpic has 255)."""
    if ch == ".":
        # id's set has no point: a square cut from the digit 0's own shading.
        src = wad_pic("anum_0" if alt else "num_0")
        pic = np.full((24, 10), 255, np.uint8)
        pic[17:23, 2:8] = src[17:23, 4:10]
    else:
        name = _BIG[ch]
        if alt and (name[4:].isdigit() or name == "num_minus"):  # id's red set: anum_0-9, anum_minus
            name = "a" + name
        pic = wad_pic(name)
    rgba = np.dstack([palette()[pic], np.where(pic == 255, 0, 255).astype(np.uint8)])
    return _surface_from_rgba(rgba), pic.shape[1], pic.shape[0]


def bignum_width(s: str, scale: int) -> int:
    w = 0
    for ch in s:
        w += (12 if ch == " " else _big_pic(ch, False)[1]) * scale
    return w


def draw_bignum(ctx: cairo.Context, s: str, x: float, y: float, scale: int = 2, alt: bool = False, alpha: float = 1.0) -> None:
    """The status bar's 24x24 digits (and - / : .), top-left at (x, y). alt: id's red set."""
    if alpha <= 0:
        return
    x, y = round(x), round(y)
    if alpha < 1:
        ctx.push_group()
    for ch in s:
        if ch == " ":
            x += 12 * scale
            continue
        surf, w, h = _big_pic(ch, alt)
        _paint(ctx, surf, 0, 0, w, h, x, y, scale)
        x += w * scale
    if alpha < 1:
        ctx.pop_group_to_source()
        ctx.paint_with_alpha(alpha)


# ---------------------------------------------------------- sans ----

SANS = "Inter"
MONO = "JetBrains Mono"


def _sans_face(ctx: cairo.Context, size: float, weight: str, family: str) -> None:
    w = cairo.FONT_WEIGHT_BOLD if weight == "bold" else cairo.FONT_WEIGHT_NORMAL
    ctx.select_font_face(family, cairo.FONT_SLANT_NORMAL, w)
    ctx.set_font_size(size)
    fo = cairo.FontOptions()
    fo.set_antialias(cairo.ANTIALIAS_GRAY)
    fo.set_hint_style(cairo.HINT_STYLE_SLIGHT)
    ctx.set_font_options(fo)


def sans_extent(ctx: cairo.Context, s: str, size: float = 28, weight: str = "regular", family: str = SANS):
    """(width, ascent, descent) of a sans string."""
    ctx.save()
    _sans_face(ctx, size, weight, family)
    xb, yb, w, h, xa, ya = ctx.text_extents(s)
    asc, desc, *_ = ctx.font_extents()
    ctx.restore()
    return xa, asc, desc


def draw_sans(ctx, s: str, x: float, y: float, size: float = 28, color="text", alpha: float = 1.0,
              weight: str = "regular", family: str = SANS) -> None:
    """A clean sans (Inter) for small dense labels; (x, y) is the baseline's left end."""
    if alpha <= 0 or not s:
        return
    ctx.save()
    _sans_face(ctx, size, weight, family)
    ctx.set_source_rgba(*look.color(color), alpha)
    ctx.move_to(x, y)
    ctx.show_text(s)
    ctx.restore()


# ------------------------------------------------------- equations ----


@dataclass
class _Box:
    w: float
    asc: float
    desc: float
    items: list = field(default_factory=list)  # (kind, dx, dy, payload, color)


def _tokenize(src: str) -> list[str]:
    return re.findall(r"\\[A-Za-z]+|\\.|[{}_^]|.", src, re.S)


class _Parser:
    def __init__(self, src: str):
        self.toks = _tokenize(src)
        self.i = 0

    def peek(self):
        return self.toks[self.i] if self.i < len(self.toks) else None

    def take(self):
        t = self.peek()
        self.i += 1
        return t

    def group(self):
        """One argument: {...} or a single token."""
        t = self.take()
        if t == "{":
            out = self.seq(stop="}")
            self.take()
            return out
        return self.atom(t)

    def seq(self, stop=None):
        out = []
        while self.peek() is not None and self.peek() != stop:
            t = self.take()
            if t in ("_", "^"):
                base = out.pop() if out else ("row", [])
                out.append(("sub" if t == "_" else "sup", base, self.group()))
            else:
                out.append(self.atom(t))
        return ("row", out)

    def atom(self, t):
        if t == "{":
            r = self.seq(stop="}")
            self.take()
            return r
        if t == "\\frac":
            return ("frac", self.group(), self.group())
        if t == "\\c":  # \c{name}{...}: the name is read raw (role names have underscores)
            if self.take() != "{":
                raise ValueError("equation: \\c needs {colour}")
            name = []
            while self.peek() not in ("}", None):
                name.append(self.take())
            self.take()
            return ("color", "".join(name), self.group())
        if t == "\\,":
            return ("space", 0.5)
        if t == "\\ ":
            return ("space", 1.0)
        if t.startswith("\\") and t[1:] in NAMES:
            return ("ch", NAMES[t[1:]])
        if t.startswith("\\") and len(t) == 2:
            return ("ch", t[1])
        return ("ch", t)


def _sub_scale(scale: int) -> int:
    return max(2, int(round(scale * 0.7)))


def _layout(node, scale: int, color) -> _Box:
    kind = node[0]
    s = scale
    if kind == "ch":
        ch = node[1]
        if ch == " ":
            return _Box(GLYPH * s, BASELINE * s, s)
        return _Box(GLYPH * s, BASELINE * s, s, [("glyph", 0, 0, (ch, s), color)])
    if kind == "space":
        return _Box(GLYPH * s * node[1], 0, 0)
    if kind == "color":
        return _layout(node[2], scale, node[1])
    if kind == "row":
        box = _Box(0, BASELINE * s, s)
        for child in node[1]:
            b = _layout(child, scale, color)
            box.items += [(k, dx + box.w, dy, p, c) for (k, dx, dy, p, c) in b.items]
            box.w += b.w
            box.asc = max(box.asc, b.asc)
            box.desc = max(box.desc, b.desc)
        return box
    if kind in ("sub", "sup"):
        base = _layout(node[1], scale, color)
        ss = _sub_scale(scale)
        script = _layout(node[2], ss, color)
        dy = 3 * s if kind == "sub" else -4 * s
        box = _Box(base.w + script.w, max(base.asc, script.asc - dy), max(base.desc, script.desc + dy), list(base.items))
        box.items += [(k, dx + base.w, ddy + dy, p, c) for (k, dx, ddy, p, c) in script.items]
        return box
    if kind == "frac":
        num, den = _layout(node[1], scale, color), _layout(node[2], scale, color)
        w = max(num.w, den.w) + 2 * s
        bar_top = -4 * s  # relative to the baseline: the maths axis, mid '='
        items = [("bar", 0, bar_top, (w, s), color)]
        ny = bar_top - s - num.desc  # numerator baseline
        dy = bar_top + 2 * s + s + den.asc  # denominator baseline (past the bar's shadow)
        items += [(k, dx + (w - num.w) / 2, ddy + ny, p, c) for (k, dx, ddy, p, c) in num.items]
        items += [(k, dx + (w - den.w) / 2, ddy + dy, p, c) for (k, dx, ddy, p, c) in den.items]
        return _Box(w + s, num.asc - ny, dy + den.desc, items)
    raise ValueError(f"equation: unknown node {kind}")


@functools.lru_cache(maxsize=256)
def _eq_box(src: str, scale: int, color_key) -> _Box:
    return _layout(_Parser(src).seq(), scale, color_key)


def eq_extent(src: str, scale: int = 4) -> tuple[float, float, float]:
    """(width, ascent, descent) of an equation."""
    b = _eq_box(src, scale, "white")
    return b.w, b.asc, b.desc


def draw_eq(ctx: cairo.Context, src: str, x: float, baseline: float, scale: int = 4, color="text", alpha: float = 1.0) -> None:
    """A small equation in Quake's font, the left end of its baseline at (x, baseline).

    Syntax: plain characters; \\frac{a}{b}; x_0, x_{ab}; x^2; {groups};
    \\c{lava}{coloured part}; \\, (half space); and names for the extra signs:
    \\div \\times \\to \\approx \\Delta \\cdot \\pm \\le \\ge (or type ÷ × → ≈ Δ directly).
    """
    if alpha <= 0:
        return
    box = _eq_box(src, scale, color if isinstance(color, (str, int)) else tuple(color))
    x, baseline = round(x), round(baseline)
    if alpha < 1:
        ctx.push_group()
    for kind, dx, dy, payload, col in box.items:
        if kind == "glyph":
            ch, s = payload
            draw_qtext(ctx, ch, x + round(dx), baseline + round(dy) - BASELINE * s, s, col)
        elif kind == "bar":
            w, s = payload
            c = look.color(col) if col not in RAW_STYLES else (look.GOLD if "gold" in col else look.TEXT)
            ctx.set_source_rgb(*[v * 0.25 for v in c])
            ctx.rectangle(x + round(dx) + s, baseline + round(dy) + s, round(w) - s, s)
            ctx.fill()
            ctx.set_source_rgb(*c)
            ctx.rectangle(x + round(dx), baseline + round(dy), round(w) - s, s)
            ctx.fill()
    if alpha < 1:
        ctx.pop_group_to_source()
        ctx.paint_with_alpha(alpha)
