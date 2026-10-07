"""Graphs of functions: a box on the frame that maps data to pixels.

    ax = Axes(c, x=200, y=300, w=1200, h=400, xlim=(0, 32), ylim=(790, 935))
    ax.frame(xticks=[0, 16, 32], yticks=[800, 850, 900])
    ax.plot(xs, ys, color="flame", draw=ramp(t, 2, 1))
    ax.fill_between(xs, ys, ys2, color="lava", alpha=0.4)

Data points outside the box are clipped to it.
"""

from __future__ import annotations

import math

import cairo
import numpy as np

from . import look
from .canvas import Canvas


class Axes:
    def __init__(self, c: Canvas, x: float, y: float, w: float, h: float, xlim=(0.0, 1.0), ylim=(0.0, 1.0)):
        self.c, self.x, self.y, self.w, self.h = c, x, y, w, h
        self.xlim, self.ylim = xlim, ylim

    # ---------------------------------------------------- mapping ----

    def px(self, xv):
        x0, x1 = self.xlim
        return self.x + (np.asarray(xv, float) - x0) / (x1 - x0) * self.w

    def py(self, yv):
        y0, y1 = self.ylim
        return self.y + self.h - (np.asarray(yv, float) - y0) / (y1 - y0) * self.h

    def pt(self, xv, yv) -> tuple[float, float]:
        return float(self.px(xv)), float(self.py(yv))

    def _clipped(self):
        c = self.c.ctx
        c.save()
        c.rectangle(self.x - 2, self.y - 2, self.w + 4, self.h + 4)
        c.clip()

    # ------------------------------------------------------ frame ----

    def frame(self, xticks=(), yticks=(), xlabels=None, ylabels=None, color="axis", alpha: float = 1.0,
              label_scale: int = 2, label_color="dim", box: bool = False, grid: bool = False, draw: float = 1.0,
              tick: float = 8) -> None:
        """Axis lines (left and bottom), ticks with labels in Quake's font, optional grid."""
        c = self.c
        if alpha <= 0:
            return
        if grid:
            for xv in xticks:
                X = float(self.px(xv))
                c.line(X, self.y, X, self.y + self.h, "grid", 1, alpha, draw, cap=cairo.LINE_CAP_BUTT)
            for yv in yticks:
                Y = float(self.py(yv))
                c.line(self.x, Y, self.x + self.w, Y, "grid", 1, alpha, draw, cap=cairo.LINE_CAP_BUTT)
        if box:
            c.rect(self.x, self.y, self.w, self.h, stroke=color, lw=2, alpha=alpha)
        c.line(self.x, self.y + self.h, self.x + self.w, self.y + self.h, color, 2, alpha, draw, cap=cairo.LINE_CAP_SQUARE)
        c.line(self.x, self.y + self.h, self.x, self.y, color, 2, alpha, draw, cap=cairo.LINE_CAP_SQUARE)
        if draw < 1:
            return
        for i, xv in enumerate(xticks):
            X = float(self.px(xv))
            c.line(X, self.y + self.h, X, self.y + self.h + tick, color, 2, alpha, cap=cairo.LINE_CAP_BUTT)
            lab = xlabels[i] if xlabels is not None else _fmt(xv)
            if lab:
                c.text(lab, X, self.y + self.h + tick + 6, label_scale, label_color, alpha, "center")
        for i, yv in enumerate(yticks):
            Y = float(self.py(yv))
            c.line(self.x - tick, Y, self.x, Y, color, 2, alpha, cap=cairo.LINE_CAP_BUTT)
            lab = ylabels[i] if ylabels is not None else _fmt(yv)
            if lab:
                c.text(lab, self.x - tick - 8, Y, label_scale, label_color, alpha, "right", "middle")

    # ------------------------------------------------------ marks ----

    def plot(self, xs, ys, color="flame", lw: float = 4, alpha: float = 1.0, draw: float = 1.0, dash=None):
        """A polyline through data points, drawn on left to right. Returns the pen's tip (pixels)."""
        if alpha <= 0 or draw <= 0:
            return None
        pts = list(zip(self.px(xs).tolist(), self.py(ys).tolist()))
        self._clipped()
        tip = self.c.polyline(pts, color, lw, alpha, draw, dash)
        self.c.ctx.restore()
        return tip

    def func(self, f, x0: float, x1: float, n: int = 240, **kw):
        xs = np.linspace(x0, x1, n)
        return self.plot(xs, f(xs), **kw)

    def fill_between(self, xs, y1, y2, color="lava", alpha: float = 0.35) -> None:
        if alpha <= 0:
            return
        X = self.px(xs).tolist()
        top = list(zip(X, self.py(y1).tolist()))
        bot = list(zip(X, self.py(y2).tolist()))
        self._clipped()
        self.c.poly(top + bot[::-1], fill=color, alpha=alpha)
        self.c.ctx.restore()

    def dots(self, xs, ys, r: float = 6, color="flame", alpha: float = 1.0, stroke=None, lw: float = 2) -> None:
        """Round marks at data points, clipped to the box like the lines."""
        if alpha <= 0:
            return
        self._clipped()
        for xv, yv in zip(np.atleast_1d(xs), np.atleast_1d(ys)):
            self.c.circle(float(self.px(xv)), float(self.py(yv)), r, fill=color, stroke=stroke, lw=lw, alpha=alpha)
        self.c.ctx.restore()

    def bars(self, xs, heights, width: float, color="lava", alpha: float = 1.0, base: float = 0.0, colors=None) -> None:
        """Vertical bars centred on xs, `width` in data units."""
        for i, (xv, hv) in enumerate(zip(xs, heights)):
            X0, X1 = float(self.px(xv - width / 2)), float(self.px(xv + width / 2))
            Y0, Y1 = float(self.py(base)), float(self.py(hv))
            col = colors[i] if colors is not None else color
            self.c.rect(X0, min(Y0, Y1), X1 - X0, abs(Y1 - Y0), fill=col, alpha=alpha)

    def vline(self, xv, color="rule", lw: float = 2, alpha: float = 1.0, dash=None, draw: float = 1.0) -> None:
        X = float(self.px(xv))
        self.c.line(X, self.y + self.h, X, self.y, color, lw, alpha, draw, dash, cap=cairo.LINE_CAP_BUTT)

    def hline(self, yv, color="rule", lw: float = 2, alpha: float = 1.0, dash=None, draw: float = 1.0) -> None:
        Y = float(self.py(yv))
        self.c.line(self.x, Y, self.x + self.w, Y, color, lw, alpha, draw, dash, cap=cairo.LINE_CAP_BUTT)


def _fmt(v) -> str:
    if isinstance(v, str):
        return v
    if float(v).is_integer():
        return str(int(v))
    return f"{v:g}"


def nice_ticks(lo: float, hi: float, n: int = 5) -> list[float]:
    """Round tick values covering [lo, hi], about n of them."""
    span = hi - lo
    step = 10 ** math.floor(math.log10(span / n))
    for m in (1, 2, 5, 10):
        if span / (step * m) <= n:
            step *= m
            break
    first = math.ceil(lo / step) * step
    return [round(first + i * step, 10) for i in range(int((hi - first) / step) + 1)]


__all__ = ["Axes", "nice_ticks", "look"]
