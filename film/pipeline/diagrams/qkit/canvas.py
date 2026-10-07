"""The drawing surface: cairo underneath, the film's primitives on top.

Coordinates are the 1920x1080 frame's pixels, y down. Colours are role names
from `look` ('lava'), palette indices, or (r, g, b) in 0..1. Every primitive
takes `alpha`; lines and arrows take `draw` (0..1, how much of them is drawn
yet), so an element is animated by passing it numbers from `anim`.
"""

from __future__ import annotations

import functools
import math
from contextlib import contextmanager
from pathlib import Path

import cairo
import numpy as np

from . import look, text
from .look import H, W


def surface_from_array(arr: np.ndarray) -> cairo.ImageSurface:
    """(h, w, 3) RGB or (h, w, 4) RGBA uint8 -> a cairo surface."""
    if arr.ndim == 2:
        arr = np.dstack([arr] * 3)
    if arr.shape[2] == 3:
        arr = np.dstack([arr, np.full(arr.shape[:2], 255, np.uint8)])
    return text._surface_from_rgba(np.ascontiguousarray(arr, dtype=np.uint8))


@functools.lru_cache(maxsize=32)
def load_image(path: str) -> cairo.ImageSurface:
    """A PNG/PPM file as a cairo surface (cached per process)."""
    from PIL import Image

    im = Image.open(path).convert("RGBA")
    return surface_from_array(np.asarray(im))


@functools.lru_cache(maxsize=4)
def _background(w: int, h: int, vignette: float) -> cairo.ImageSurface:
    s = cairo.ImageSurface(cairo.FORMAT_RGB24, w, h)
    c = cairo.Context(s)
    c.set_source_rgb(*look.BG)
    c.paint()
    if vignette > 0:
        g = cairo.RadialGradient(w / 2, h / 2, h * 0.35, w / 2, h / 2, math.hypot(w, h) * 0.62)
        g.add_color_stop_rgba(0, *look.BG_DEEP, 0)
        g.add_color_stop_rgba(1, *look.BG_DEEP, vignette)
        c.set_source(g)
        c.paint()
    return s


class Canvas:
    """One frame. `c.ctx` is the cairo context, for anything the helpers lack.

    `scale`: the surface is that many times W x H's coordinates. Drawing stays in 1080's
    coordinates (`c.w`, `c.h` are the logical size); the cairo transform does the rest, so a
    line or Inter is drawn sharp at the scale's resolution and id's 8x8 glyphs at their scale
    times it, nearest neighbour. `c.surface` is the device-size surface (`canvas_image(c)`
    reads it). A canvas built directly keeps scale 1, its surface's own size, as before;
    new_canvas() gives look.SCALE.
    """

    def __init__(self, surface: cairo.ImageSurface, transparent: bool = False, scale: int = 1):
        self.surface = surface
        self.transparent = transparent  # an overlay render (--alpha): background() draws nothing
        self.scale = int(scale)
        self.ctx = cairo.Context(surface)
        self.ctx.set_line_join(cairo.LINE_JOIN_ROUND)
        if self.scale != 1:
            self.ctx.scale(self.scale, self.scale)
        self.w = surface.get_width() // self.scale
        self.h = surface.get_height() // self.scale

    # ------------------------------------------------------- state ----

    @contextmanager
    def group(self, alpha: float = 1.0):
        """Draw several things, then lay them down together at one opacity."""
        self.ctx.push_group()
        try:
            yield self
        finally:
            self.ctx.pop_group_to_source()
            self.ctx.paint_with_alpha(max(0.0, min(1.0, alpha)))

    @contextmanager
    def clip(self, x: float, y: float, w: float, h: float):
        self.ctx.save()
        self.ctx.rectangle(x, y, w, h)
        self.ctx.clip()
        try:
            yield self
        finally:
            self.ctx.restore()

    @contextmanager
    def moved(self, dx: float = 0, dy: float = 0, scale: float = 1.0, about=(0.0, 0.0)):
        """Translate, and scale about a point: for moving or zooming a whole group."""
        self.ctx.save()
        self.ctx.translate(dx, dy)
        if scale != 1.0:
            ax, ay = about
            self.ctx.translate(ax, ay)
            self.ctx.scale(scale, scale)
            self.ctx.translate(-ax, -ay)
        try:
            yield self
        finally:
            self.ctx.restore()

    def _rgba(self, color, alpha: float):
        self.ctx.set_source_rgba(*look.color(color), alpha)

    # -------------------------------------------------- background ----

    def background(self, vignette: float = 0.55) -> None:
        """id's near-black, darkening to black at the corners (nothing in a transparent render)."""
        if self.transparent:
            return
        if self.scale == 1:
            self.ctx.set_source_surface(_background(self.w, self.h, vignette), 0, 0)
            self.ctx.paint()
            return
        self.ctx.save()  # the gradient made at the surface's own size, not stretched
        self.ctx.identity_matrix()
        self.ctx.set_source_surface(_background(self.surface.get_width(), self.surface.get_height(), vignette), 0, 0)
        self.ctx.paint()
        self.ctx.restore()

    # ------------------------------------------------------ shapes ----

    def rect(self, x, y, w, h, fill=None, stroke=None, lw: float = 2, alpha: float = 1.0, radius: float = 0) -> None:
        if alpha <= 0:
            return
        c = self.ctx
        if radius > 0:
            r = min(radius, w / 2, h / 2)
            c.new_sub_path()
            c.arc(x + w - r, y + r, r, -math.pi / 2, 0)
            c.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
            c.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
            c.arc(x + r, y + r, r, math.pi, 1.5 * math.pi)
            c.close_path()
        else:
            c.rectangle(x, y, w, h)
        if fill is not None:
            self._rgba(fill, alpha)
            c.fill_preserve() if stroke is not None else c.fill()
        if stroke is not None:
            self._rgba(stroke, alpha)
            c.set_line_width(lw)
            c.stroke()
        c.new_path()

    def circle(self, x, y, r, fill=None, stroke=None, lw: float = 2, alpha: float = 1.0) -> None:
        if alpha <= 0 or r <= 0:
            return
        c = self.ctx
        c.new_sub_path()
        c.arc(x, y, r, 0, 2 * math.pi)
        if fill is not None:
            self._rgba(fill, alpha)
            c.fill_preserve() if stroke is not None else c.fill()
        if stroke is not None:
            self._rgba(stroke, alpha)
            c.set_line_width(lw)
            c.stroke()
        c.new_path()

    def poly(self, pts, fill=None, stroke=None, lw: float = 2, alpha: float = 1.0, closed: bool = True) -> None:
        if alpha <= 0 or len(pts) < 2:
            return
        c = self.ctx
        c.move_to(*pts[0])
        for p in pts[1:]:
            c.line_to(*p)
        if closed:
            c.close_path()
        if fill is not None:
            self._rgba(fill, alpha)
            c.fill_preserve() if stroke is not None else c.fill()
        if stroke is not None:
            self._rgba(stroke, alpha)
            c.set_line_width(lw)
            c.stroke()
        c.new_path()

    def polyline(self, pts, color="text", lw: float = 3, alpha: float = 1.0, draw: float = 1.0, dash=None,
                 cap=cairo.LINE_CAP_ROUND):
        """A line through points, drawn on from the first to `draw` of its length. Returns the pen's tip."""
        if alpha <= 0 or draw <= 0 or len(pts) < 2:
            return None
        pts = [tuple(map(float, p)) for p in pts]
        if draw < 1:
            seg = [math.dist(a, b) for a, b in zip(pts, pts[1:])]
            goal = sum(seg) * draw
            out = [pts[0]]
            for (a, b), L in zip(zip(pts, pts[1:]), seg):
                if goal >= L:
                    out.append(b)
                    goal -= L
                else:
                    u = goal / L if L else 0
                    out.append((a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u))
                    break
            pts = out
        c = self.ctx
        c.save()
        c.set_line_cap(cap)
        c.set_line_width(lw)
        if dash:
            c.set_dash(dash)
        self._rgba(color, alpha)
        c.move_to(*pts[0])
        for p in pts[1:]:
            c.line_to(*p)
        c.stroke()
        c.restore()
        return pts[-1]

    def line(self, x0, y0, x1, y1, color="text", lw: float = 3, alpha: float = 1.0, draw: float = 1.0, dash=None,
             cap=cairo.LINE_CAP_ROUND):
        return self.polyline([(x0, y0), (x1, y1)], color, lw, alpha, draw, dash, cap)

    def arrow(self, x0, y0, x1, y1, color="text", lw: float = 3, head: float = 16, alpha: float = 1.0,
              draw: float = 1.0, both: bool = False) -> None:
        """A line with a filled head at its end (and its start, `both`), drawn on with `draw`."""
        if alpha <= 0 or draw <= 0:
            return
        ang = math.atan2(y1 - y0, x1 - x0)
        tx, ty = x0 + (x1 - x0) * draw, y0 + (y1 - y0) * draw
        back = head * 0.8
        self.line(x0 + (math.cos(ang) * back if both else 0), y0 + (math.sin(ang) * back if both else 0),
                  tx - math.cos(ang) * back, ty - math.sin(ang) * back, color, lw, alpha, cap=cairo.LINE_CAP_BUTT)

        def tip(px, py, a):
            self.poly([(px, py), (px - head * math.cos(a - 0.42), py - head * math.sin(a - 0.42)),
                       (px - head * math.cos(a + 0.42), py - head * math.sin(a + 0.42))], fill=color, alpha=alpha)

        tip(tx, ty, ang)
        if both:
            tip(x0, y0, ang + math.pi)

    def bracket(self, x0, x1, y, color="dim", lw: float = 2, tick: float = 10, alpha: float = 1.0, up: bool = False):
        """A span bracket ⊓/⊔ from x0 to x1 at y (ticks pointing down, or up)."""
        d = -tick if up else tick
        self.polyline([(x0, y + d), (x0, y), (x1, y), (x1, y + d)], color, lw, alpha, cap=cairo.LINE_CAP_SQUARE)

    def grid(self, x, y, w, h, step: float, color="grid", lw: float = 1, alpha: float = 1.0) -> None:
        c = self.ctx
        self._rgba(color, alpha)
        c.set_line_width(lw)
        gx = x
        while gx <= x + w + 0.01:
            c.move_to(round(gx) + 0.5, y)
            c.line_to(round(gx) + 0.5, y + h)
            gx += step
        gy = y
        while gy <= y + h + 0.01:
            c.move_to(x, round(gy) + 0.5)
            c.line_to(x + w, round(gy) + 0.5)
            gy += step
        c.stroke()

    # ------------------------------------------------------- text ----

    @staticmethod
    def _anchor(x, y, w, h, align, valign, baseline=None):
        if align == "center":
            x -= w / 2
        elif align == "right":
            x -= w
        if valign == "middle":
            y -= h / 2
        elif valign == "bottom":
            y -= h
        elif valign == "baseline" and baseline is not None:
            y -= baseline
        return x, y

    def text(self, s: str, x, y, scale: int = 3, color="text", alpha: float = 1.0, align: str = "left",
             valign: str = "top"):
        """Quake's font. color 'gold' is id's bronze menu set; 'white' its grey set; else a tint.
        Returns (x, y, w, h) of what it drew."""
        w, h = text.qtext_width(s, scale), text.GLYPH * scale
        x, y = self._anchor(x, y, w, h, align, valign, text.BASELINE * scale)
        text.draw_qtext(self.ctx, s, x, y, scale, color, alpha)
        return (round(x), round(y), w, h)

    def lines(self, rows: list[str], x, y, scale: int = 3, color="text", alpha: float = 1.0, align: str = "left",
              leading: float = 1.5):
        """Several lines of Quake's font, `leading` cells apart."""
        for i, s in enumerate(rows):
            self.text(s, x, y + i * text.GLYPH * scale * leading, scale, color, alpha, align)

    def bignum(self, s: str, x, y, scale: int = 2, alt: bool = False, alpha: float = 1.0, align: str = "left",
               valign: str = "top"):
        """The status bar's big digits (0-9 - / : .); alt is id's red set."""
        w, h = text.bignum_width(s, scale), 24 * scale
        x, y = self._anchor(x, y, w, h, align, valign)
        text.draw_bignum(self.ctx, s, x, y, scale, alt, alpha)
        return (round(x), round(y), w, h)

    def sans(self, s: str, x, y, size: float = 28, color="text", alpha: float = 1.0, align: str = "left",
             weight: str = "regular", mono: bool = False):
        """A clean sans for small, dense labels. (x, y) is on the baseline."""
        fam = text.MONO if mono else text.SANS
        w, asc, desc = text.sans_extent(self.ctx, s, size, weight, fam)
        if align == "center":
            x -= w / 2
        elif align == "right":
            x -= w
        text.draw_sans(self.ctx, s, x, y, size, color, alpha, weight, fam)
        return (x, y - asc, w, asc + desc)

    def paragraph(self, s: str, x, y, width: float, size: float = 28, color="text", alpha: float = 1.0,
                  leading: float = 1.4, weight: str = "regular") -> float:
        """Sans text wrapped to `width` pixels, first baseline at y. Returns the y below it."""
        words, line, lines = s.split(), "", []
        for w in words:
            trial = f"{line} {w}".strip()
            if line and text.sans_extent(self.ctx, trial, size, weight)[0] > width:
                lines.append(line)
                line = w
            else:
                line = trial
        if line:
            lines.append(line)
        for i, ln in enumerate(lines):
            text.draw_sans(self.ctx, ln, x, y + i * size * leading, size, color, alpha, weight)
        return y + len(lines) * size * leading

    def eq(self, src: str, x, y, scale: int = 4, color="text", alpha: float = 1.0, align: str = "left",
           valign: str = "baseline"):
        """An equation in Quake's font (see text.draw_eq for the syntax). Returns its box."""
        w, asc, desc = text.eq_extent(src, scale)
        if align == "center":
            x -= w / 2
        elif align == "right":
            x -= w
        if valign == "top":
            y += asc
        elif valign == "middle":
            y += (asc - desc) / 2
        elif valign == "bottom":
            y -= desc
        text.draw_eq(self.ctx, src, x, y, scale, color, alpha)
        return (round(x), round(y - asc), w, asc + desc)

    # ------------------------------------------------------ images ----

    def image(self, img, x, y, scale: float = 1.0, alpha: float = 1.0, nearest: bool = True, src=None) -> None:
        """A picture (a path, a numpy array, or a cairo surface), its top-left at (x, y).

        src = (sx, sy, sw, sh) crops it first. nearest keeps game pixels square.
        """
        if alpha <= 0:
            return
        if isinstance(img, (str, Path)):
            surf = load_image(str(img))
        elif isinstance(img, np.ndarray):
            surf = surface_from_array(img)
        else:
            surf = img
        sx, sy, sw, sh = src if src else (0, 0, surf.get_width(), surf.get_height())
        c = self.ctx
        c.save()
        c.translate(x, y)
        c.scale(scale, scale)
        c.set_source_surface(surf, -sx, -sy)
        c.get_source().set_filter(cairo.FILTER_NEAREST if nearest else cairo.FILTER_GOOD)
        c.rectangle(0, 0, sw, sh)
        c.clip()
        c.paint_with_alpha(alpha)
        c.restore()

    def pixel_row(self, x, y, colors, cell: float, gap: float = 2, alpha: float = 1.0, outline=None,
                  marks=None, mark_color="blood", mark_lw: float = 3, reveal: float = 1.0) -> None:
        """A row of magnified screen pixels: colors is (n, 3) uint8 (or 0..1 floats).

        gap: dark space between cells; marks: a boolean per cell to outline in
        mark_color (the pixels that differ); reveal: the share of cells shown, left first.
        """
        if alpha <= 0:
            return
        cols = np.asarray(colors, float)
        if cols.max() > 1.0:
            cols = cols / 255.0
        n = len(cols)
        shown = n if reveal >= 1 else int(math.ceil(n * max(0.0, reveal)))
        c = self.ctx
        for i in range(shown):
            c.set_source_rgba(*cols[i], alpha)
            c.rectangle(x + i * cell + gap / 2, y + gap / 2, cell - gap, cell - gap)
            c.fill()
        if outline is not None:
            self.rect(x, y, n * cell, cell, stroke=outline, lw=1, alpha=alpha * 0.6)
        if marks is not None:
            for i in range(shown):
                if marks[i]:
                    self.rect(x + i * cell + 1, y + 1, cell - 2, cell - 2, stroke=mark_color, lw=mark_lw, alpha=alpha)

    def texture_strip(self, tex_rgb: np.ndarray, x, y, cell: float, alpha: float = 1.0, gap: float = 0) -> None:
        """A texture (h, w, 3) drawn texel by texel at `cell` pixels a texel (a strip of texture)."""
        self.image(tex_rgb, x, y, scale=cell, alpha=alpha)
        if gap:
            self.grid(x, y, tex_rgb.shape[1] * cell, tex_rgb.shape[0] * cell, cell, "bg", gap, alpha)

    @contextmanager
    def lens(self, src, dst, zoom: float, r: float, ring="dim", alpha: float = 1.0, connector: bool = True):
        """A magnifier: what is drawn inside the `with` appears around `src`, `zoom` times
        larger, in a circle of radius r at `dst`. Yields the zoom so the caller can divide
        line widths by it. Draw the same things you drew outside, at the same coordinates.

            with c.lens((700, 400), (1500, 600), 6, 120) as z:
                ax.plot(xs, ys, lw=4 / z)
        """
        c = self.ctx
        sx, sy = src
        dx, dy = dst
        if alpha <= 0:
            c.save()
            c.rectangle(0, 0, 0, 0)
            c.clip()
            try:
                yield zoom
            finally:
                c.restore()
            return
        if connector:
            ang = math.atan2(dy - sy, dx - sx)
            self.line(sx + math.cos(ang) * r / zoom, sy + math.sin(ang) * r / zoom,
                      dx - math.cos(ang) * r, dy - math.sin(ang) * r, ring, 2, alpha * 0.8)
            self.circle(sx, sy, r / zoom, stroke=ring, lw=2, alpha=alpha * 0.8)
        c.push_group()
        c.save()
        c.new_sub_path()
        c.arc(dx, dy, r, 0, 2 * math.pi)
        c.clip()
        c.set_source_rgb(*look.BG)
        c.paint()
        c.translate(dx, dy)
        c.scale(zoom, zoom)
        c.translate(-sx, -sy)
        try:
            yield zoom
        finally:
            c.restore()
            c.pop_group_to_source()
            c.paint_with_alpha(alpha)
            self.circle(dx, dy, r, stroke=ring, lw=3, alpha=alpha)

    def callout(self, x0, y0, x1, y1, label: str = "", color="dim", scale: int = 2, alpha: float = 1.0,
                draw: float = 1.0, align: str = "left") -> None:
        """A thin leader line from a point (x0, y0) to a label at (x1, y1)."""
        self.line(x0, y0, x1, y1, color, 2, alpha, draw)
        self.circle(x0, y0, 4, fill=color, alpha=alpha)
        if label and draw >= 1:
            self.text(label, x1 + (8 if align == "left" else -8), y1, scale, color, alpha, align, "middle")


def new_canvas(w: int = W, h: int = H, alpha: bool = False, scale: int | None = None) -> Canvas:
    """A canvas of w x h in 1080's coordinates, at `scale` (default: look.SCALE): an opaque
    RGB24 surface, or with `alpha` a transparent ARGB32 one (background() then draws nothing)."""
    s = look.SCALE if scale is None else int(scale)
    fmt = cairo.FORMAT_ARGB32 if alpha else cairo.FORMAT_RGB24
    return Canvas(cairo.ImageSurface(fmt, w * s, h * s), transparent=alpha, scale=s)


def canvas_image(c: Canvas):
    """The canvas's pixels as a Pillow image at its device size: RGBA for an ARGB32 surface
    (unpremultiplied), else RGB."""
    from PIL import Image

    c.surface.flush()
    size = (c.surface.get_width(), c.surface.get_height())
    data, stride = bytes(c.surface.get_data()), c.surface.get_stride()
    if c.surface.get_format() == cairo.FORMAT_ARGB32:
        return Image.frombuffer("RGBA", size, data, "raw", "BGRa", stride, 1)
    return Image.frombuffer("RGB", size, data, "raw", "BGRX", stride, 1)
