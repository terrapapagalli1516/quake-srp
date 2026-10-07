"""Compositing for the page captures, at any whole scale of v7's 1920x1080 geometry: a plain
browser window drawn around a desktop capture, a plain phone outline around a phone capture,
the mouse cursor and touch marks (headless screenshots show neither), all on a black frame of
1920s x 1080s. Every length below is v7's, times `s`, so `s = 1` draws v7's frames exactly.
Everything here is drawn from scratch: no real browser's or device's branding, no real
address."""

from __future__ import annotations

import base64
import io
import re
from functools import lru_cache

from PIL import Image, ImageDraw, ImageFont

import webcap

BRONZE = (181, 131, 47)


@lru_cache(maxsize=None)
def font(size: int):
    return ImageFont.truetype(webcap.font_file("Noto Sans:style=Regular"), size)


def page_favicon(index_html) -> Image.Image:
    """The page's own favicon: the 32-pixel PNG inlined in index.html."""
    html = open(index_html, encoding="utf-8").read()
    m = re.search(r'rel="icon" type="image/png" href="data:image/png;base64,([^"]+)"', html)
    return Image.open(io.BytesIO(base64.b64decode(m.group(1)))).convert("RGBA")


def _open(capture) -> Image.Image:
    if isinstance(capture, Image.Image):
        return capture.convert("RGBA")
    return Image.open(io.BytesIO(capture) if isinstance(capture, bytes) else capture).convert("RGBA")


# --- The desktop window -------------------------------------------------------------------
# v7, in device pixels (the capture at devicePixelRatio 2): the page's viewport is 1840x944,
# under a 100 px chrome: a tab row and a toolbar with the address.

class Desktop:
    def __init__(self, s: int):
        self.s = s
        self.out_w, self.out_h = 1920 * s, 1080 * s
        self.view_w, self.view_h = 1840 * s, 944 * s
        self.chrome_h = 100 * s
        self.win_x = (self.out_w - self.view_w) // 2
        self.win_y = (self.out_h - self.view_h - self.chrome_h) // 2
        self.view_x, self.view_y = self.win_x, self.win_y + self.chrome_h
        self._mask = None
        self._sprites = {}

    def chrome(self, title: str, address: str, favicon: Image.Image) -> Image.Image:
        """The window's frame as an RGBA image the size of the output, the viewport left
        transparent."""
        s = self.s
        im = Image.new("RGBA", (self.out_w, self.out_h), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        x0, y0 = self.win_x, self.win_y
        x1, y1 = self.win_x + self.view_w, self.win_y + self.chrome_h + self.view_h
        # The body, its top corners rounded.
        d.rounded_rectangle((x0 - s, y0 - s, x1, y1), radius=18 * s, fill=(58, 58, 60, 255))
        d.rounded_rectangle((x0, y0, x1 - 1, y1 - 1), radius=17 * s, fill=(28, 28, 30, 255))
        # The tab: the page's favicon and title, a close mark.
        tx0, tx1, ty0, ty1 = x0 + 16 * s, x0 + 16 * s + 560 * s, y0 + 8 * s, y0 + 46 * s
        d.rounded_rectangle((tx0, ty0, tx1, ty1 + 14 * s), radius=12 * s, fill=(46, 46, 49, 255))
        im.alpha_composite(favicon.resize((28 * s, 28 * s), Image.NEAREST), (tx0 + 18 * s, ty0 + 6 * s))
        f = font(21 * s)
        t = title
        while d.textlength(t, font=f) > tx1 - tx0 - 110 * s and len(t) > 4:
            t = t[:-2]
        if t != title:
            t = t.rstrip() + "…"
        d.text((tx0 + 58 * s, ty0 + 19 * s), t, font=f, fill=(222, 222, 222, 255), anchor="lm")
        cx, cy = tx1 - 26 * s, ty0 + 19 * s
        for a, b in (((-6, -6), (6, 6)), ((-6, 6), (6, -6))):
            d.line((cx + a[0] * s, cy + a[1] * s, cx + b[0] * s, cy + b[1] * s), fill=(170, 170, 170, 255), width=2 * s)
        # The toolbar.
        by0 = y0 + 46 * s
        d.rectangle((x0, by0, x1 - 1, y0 + self.chrome_h - 1), fill=(46, 46, 49, 255))
        d.line((x0, y0 + self.chrome_h - s, x1 - 1, y0 + self.chrome_h - s), fill=(20, 20, 22, 255), width=s)
        mid = by0 + 27 * s
        grey = (190, 190, 190, 255)
        dim = (110, 110, 110, 255)
        # back, forward (dim: no history), reload
        for i, (col, sgn) in enumerate(((grey, -1), (dim, 1))):
            cx = x0 + (34 + i * 48) * s
            d.line((cx - 10 * s, mid, cx + 10 * s, mid), fill=col, width=3 * s)
            tip = cx + 10 * s * sgn
            d.line((tip, mid, tip - 8 * s * sgn, mid - 8 * s), fill=col, width=3 * s)
            d.line((tip, mid, tip - 8 * s * sgn, mid + 8 * s), fill=col, width=3 * s)
        cx = x0 + (34 + 2 * 48) * s
        d.arc((cx - 10 * s, mid - 10 * s, cx + 10 * s, mid + 10 * s), start=-60, end=250, fill=grey, width=3 * s)
        d.polygon(((cx + 6 * s, mid - 13 * s), (cx + 12 * s, mid - 4 * s), (cx + 2 * s, mid - 4 * s)), fill=grey)
        # The address: the local server only.
        ax0, ax1 = x0 + 180 * s, x1 - 40 * s
        d.rounded_rectangle((ax0, mid - 19 * s, ax1, mid + 19 * s), radius=19 * s, fill=(30, 30, 32, 255))
        d.text((ax0 + 26 * s, mid), address, font=font(21 * s), fill=(225, 225, 225, 255), anchor="lm")
        return im

    def sprite(self, kind: str):
        """(image, hot spot) for a CSS cursor kind: pointer, crosshair, else the arrow; v7's
        sprites were drawn at scale 2 (the capture's devicePixelRatio)."""
        if kind not in self._sprites:
            k = 2 * self.s
            if kind == "pointer":
                self._sprites[kind] = hand_sprite(k)
            elif kind == "crosshair":
                self._sprites[kind] = cross_sprite(k)
            else:
                self._sprites[kind] = (cursor_sprite(k), (2, 2))
        return self._sprites[kind]

    def frame(self, capture, chrome, cursor=None, ring=None, kind="default") -> Image.Image:
        """One output frame: black, the window chrome, the page capture in its viewport, then
        the cursor (device px within the viewport)."""
        s = self.s
        out = Image.new("RGBA", (self.out_w, self.out_h), (0, 0, 0, 255))
        out.alpha_composite(chrome)
        page = _open(capture)
        if page.size != (self.view_w, self.view_h):
            page = page.resize((self.view_w, self.view_h), Image.LANCZOS)
        if ring:
            draw_click_ring(page, ring[0], ring[1], ring[2], 2 * s)
        if cursor is not None:
            ci, (hx, hy) = self.sprite(kind)
            page.alpha_composite(ci, (int(round(cursor[0])) - hx, int(round(cursor[1])) - hy))
        # The viewport's bottom corners follow the window's rounding.
        if self._mask is None:
            self._mask = Image.new("L", (self.view_w, self.view_h), 0)
            ImageDraw.Draw(self._mask).rounded_rectangle((0, -40 * s, self.view_w - 1, self.view_h - 1), radius=17 * s, fill=255)
        out.paste(page, (self.view_x, self.view_y), self._mask)
        return out.convert("RGB")


def cursor_sprite(scale=2):
    """A plain arrow pointer, white with a black edge."""
    pts = [(0, 0), (0, 17), (4, 13), (7, 20), (10, 19), (7, 12), (12, 12)]
    s = scale
    im = Image.new("RGBA", (16 * s + 4, 23 * s + 4), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.polygon([(2 + x * s, 2 + y * s) for x, y in pts], fill=(255, 255, 255, 255), outline=(0, 0, 0, 255), width=max(1, s))
    return im


def hand_sprite(scale=2):
    """A plain pointing hand (CSS `cursor: pointer`), white with a black edge."""
    s = scale
    im = Image.new("RGBA", (20 * s + 4, 24 * s + 4), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    o = 2
    shapes = [(5, 0, 9, 13), (9, 7, 12.5, 15), (12.5, 8, 16, 16), (16, 9, 19, 17), (5, 11, 19, 23), (1, 10, 5, 18)]
    for x0, y0, x1, y1 in shapes:      # black edge first
        d.rounded_rectangle((o + x0 * s - s, o + y0 * s - s, o + x1 * s + s, o + y1 * s + s), radius=2 * s, fill=(0, 0, 0, 255))
    for x0, y0, x1, y1 in shapes:
        d.rounded_rectangle((o + x0 * s, o + y0 * s, o + x1 * s, o + y1 * s), radius=2 * s, fill=(255, 255, 255, 255))
    for x in (9, 12.5, 16):            # the knuckles
        d.line((o + x * s, o + 12 * s, o + x * s, o + 16 * s), fill=(0, 0, 0, 255), width=max(1, s // 2))
    return im, (o + 7 * s, o)          # the hot spot: the fingertip


def cross_sprite(scale=2):
    """A plain crosshair (CSS `cursor: crosshair`)."""
    s = scale
    n = 21 * s
    im = Image.new("RGBA", (n + 4, n + 4), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    c = 2 + n // 2
    for w, col in ((3 * s, (0, 0, 0, 255)), (s, (255, 255, 255, 255))):
        d.line((2, c, 2 + n, c), fill=col, width=w)
        d.line((c, 2, c, 2 + n), fill=col, width=w)
    return im, (c, c)


def draw_click_ring(base, x, y, k, scale=2):
    """A ring that grows and fades over k in 0..1 where the click landed."""
    if not (0 <= k <= 1):
        return
    ov = Image.new("RGBA", base.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(ov)
    r = (6 + 18 * k) * scale
    a = int(200 * (1 - k))
    d.ellipse((x - r, y - r, x + r, y + r), outline=(255, 255, 255, a), width=2 * scale)
    base.alpha_composite(ov)


# --- The phone ------------------------------------------------------------------------------
# v7: the capture is the phone profile's pixels (1012x412 CSS at devicePixelRatio 2.6,
# web/verify_touch.py's PHONE_26: 2631x1071), shown 1640x668 inside a plain outline 1700 px
# wide, centred on black.

class Phone:
    def __init__(self, s: int):
        self.s = s
        self.out_w, self.out_h = 1920 * s, 1080 * s
        self.screen_w, self.screen_h = 1640 * s, 668 * s
        self.body_w, self.body_h = 1700 * s, 730 * s
        self.body_x, self.body_y = (self.out_w - self.body_w) // 2, (self.out_h - self.body_h) // 2
        self.screen_x = self.body_x + (self.body_w - self.screen_w) // 2
        self.screen_y = self.body_y + (self.body_h - self.screen_h) // 2
        self.body = self._body()
        self.mask = Image.new("L", (self.screen_w, self.screen_h), 0)
        ImageDraw.Draw(self.mask).rounded_rectangle((0, 0, self.screen_w - 1, self.screen_h - 1), radius=40 * s, fill=255)

    def _body(self):
        s, bx, by = self.s, self.body_x, self.body_y
        im = Image.new("RGBA", (self.out_w, self.out_h), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        # Side keys on the top edge (the phone is held sideways), then the body.
        for kx0, kx1 in ((bx + 1180 * s, bx + 1330 * s), (bx + 1380 * s, bx + 1450 * s)):
            d.rounded_rectangle((kx0, by - 9 * s, kx1, by + 6 * s), radius=5 * s, fill=(12, 10, 8, 255), outline=BRONZE + (255,), width=3 * s)
        d.rounded_rectangle((bx, by, bx + self.body_w, by + self.body_h), radius=70 * s,
                            fill=(8, 7, 6, 255), outline=BRONZE + (255,), width=4 * s)
        d.rounded_rectangle((bx + 10 * s, by + 10 * s, bx + self.body_w - 10 * s, by + self.body_h - 10 * s), radius=62 * s,
                            outline=(60, 45, 22, 255), width=s)
        # The front camera, in the bezel at the left (the phone's top).
        cy = by + self.body_h // 2
        d.ellipse((bx + 9 * s, cy - 7 * s, bx + 23 * s, cy + 7 * s), fill=(22, 22, 26, 255), outline=(70, 60, 40, 255), width=2 * s)
        return im

    def frame(self, capture, touches=(), dpr=2.6) -> Image.Image:
        """One output frame: the capture (device px) with the fingers drawn on it (CSS px
        points, a 24 CSS px radius), scaled into the phone's screen."""
        cap_im = _open(capture)
        if touches:
            ov = Image.new("RGBA", cap_im.size, (0, 0, 0, 0))
            d = ImageDraw.Draw(ov)
            r = 24 * dpr
            for (x, y) in touches:
                x, y = x * dpr, y * dpr
                d.ellipse((x - r, y - r, x + r, y + r), fill=(255, 255, 255, 64), outline=(255, 255, 255, 150), width=int(2 * dpr))
            cap_im.alpha_composite(ov)
        scr = cap_im.resize((self.screen_w, self.screen_h), Image.LANCZOS)
        out = Image.new("RGBA", (self.out_w, self.out_h), (0, 0, 0, 255))
        out.alpha_composite(self.body)
        out.paste(scr, (self.screen_x, self.screen_y), self.mask)
        return out.convert("RGB")
