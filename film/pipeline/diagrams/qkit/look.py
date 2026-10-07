"""The film's look: frame size, rate, and colours taken from id's palette.

Every colour here is an entry of gfx/palette.lmp, named by its role. Use the
names in diagrams (`Q.LAVA`), not raw numbers, so the whole film can be
re-coloured in one place. `pal(i)` gives any other palette entry.
"""

from __future__ import annotations

from .quake import pal

W, H, FPS = 1920, 1080, 60

# Palette indices (rows of 16: greys 0-15, browns 16-31, slate 32-47,
# olive 48-63, reds 64-79, ochre 80-95, rust and gold 96-111, flesh
# 112-127, ... grey-green 176-191, yellows 192-207, blues 208-223,
# fire 224-239, fullbrights 240-254).
IDX = {
    "BG": 16,  # (15, 11, 7)   near-black, warm
    "BG_DEEP": 0,  # black, the vignette's edge
    "PANEL": 17,  # (23, 15, 11)  a panel a shade above the background
    "GRID": 2,  # (31, 31, 31)  faint grid lines
    "RULE": 4,  # (63, 63, 63)  minor rules, ghosts
    "AXIS": 8,  # (123,123,123) axes and ticks
    "DIM": 10,  # (155,155,155) secondary text
    "TEXT": 14,  # (219,219,219) primary text
    "WHITE": 254,  # (255,255,255) fullbright white, sparingly
    "RUST": 104,  # (143, 67, 51) rust brown
    "RUST_DARK": 100,  # (87, 43, 23)
    "BRONZE": 92,  # (139, 91, 19)
    "GOLD": 108,  # (207,143, 43)
    "YELLOW": 111,  # (255,243, 27) fullbright yellow: a highlight
    "LAVA": 234,  # (207, 99, 43) lava orange
    "EMBER": 236,  # (227,151, 79)
    "FLAME": 238,  # (239,191,119) pale flame: the "true" curve
    "BLOOD": 251,  # (255,  0,  0) fullbright red: errors
    "BLOOD_DARK": 248,  # (139,  0,  0)
    "SLIME": 63,  # (107,107, 15) sickly green
    "SLIME_LIGHT": 196,  # (187,167, 15)
    "SLATE": 44,  # (115,115,163) slate blue-grey
    "SLATE_DARK": 36,  # (47, 47, 63)
    "MOSS": 178,  # (95,115,103) grey-green
    "SKY": 245,  # (171,231,255) fullbright pale blue
    "SKY_DEEP": 244,  # (127,191,255)
}

BG = pal(IDX["BG"])
BG_DEEP = pal(IDX["BG_DEEP"])
PANEL = pal(IDX["PANEL"])
GRID = pal(IDX["GRID"])
RULE = pal(IDX["RULE"])
AXIS = pal(IDX["AXIS"])
DIM = pal(IDX["DIM"])
TEXT = pal(IDX["TEXT"])
WHITE = pal(IDX["WHITE"])
RUST = pal(IDX["RUST"])
RUST_DARK = pal(IDX["RUST_DARK"])
BRONZE = pal(IDX["BRONZE"])
GOLD = pal(IDX["GOLD"])
YELLOW = pal(IDX["YELLOW"])
LAVA = pal(IDX["LAVA"])
EMBER = pal(IDX["EMBER"])
FLAME = pal(IDX["FLAME"])
BLOOD = pal(IDX["BLOOD"])
BLOOD_DARK = pal(IDX["BLOOD_DARK"])
SLIME = pal(IDX["SLIME"])
SLIME_LIGHT = pal(IDX["SLIME_LIGHT"])
SLATE = pal(IDX["SLATE"])
SLATE_DARK = pal(IDX["SLATE_DARK"])
MOSS = pal(IDX["MOSS"])
SKY = pal(IDX["SKY"])
SKY_DEEP = pal(IDX["SKY_DEEP"])

COLORS = {name.lower(): pal(i) for name, i in IDX.items()}


def color(c) -> tuple[float, float, float]:
    """A colour from a role name ('lava'), a palette index (234, or '234' as in an equation's
    \\c{234}{...}), or an (r, g, b) in 0..1."""
    if isinstance(c, str):
        return pal(int(c)) if c.isdigit() else COLORS[c.lower()]
    if isinstance(c, int):
        return pal(c)
    return tuple(c)  # type: ignore[return-value]


def mix(a, b, u: float) -> tuple[float, float, float]:
    """Linear blend of two colours, u = 0 gives a."""
    a, b = color(a), color(b)
    return tuple(x + (y - x) * u for x, y in zip(a, b))  # type: ignore[return-value]


def shade(c, k: float) -> tuple[float, float, float]:
    """A colour scaled toward black (k < 1) or brighter (k > 1), clipped."""
    return tuple(min(1.0, max(0.0, x * k)) for x in color(c))  # type: ignore[return-value]


# The film's storyboard palette: its roles by name, as palette indices, for the diagrams
# drawn in it: `Q.SB["rust"]` -> 105. Its sickly green (63) is too dark for text and small
# marks on black, so those use it lifted, `shade(Q.SB["green"], 1.6)`.
SB = {
    "black": 0,  # background
    "panel": 16,  # brown-black
    "brown": 26,  # id's, Classic
    "brown_light": 31,
    "rust": 105,  # id's 16-pixel spans, id's curves
    "rust_dark": 103,  # grid lines, texel edges
    "bronze": 106,  # labels
    "green": 63,  # sickly green: slop, "passes"
    "grey_green": 183,  # secondary slop
    "lava": 235,  # the true curve, highlights
    "lava_deep": 233,
    "flame": 238,  # peaks, labels on dark
    "slate": 36,  # axes, wireframes
    "slate_light": 40,  # 1/z
    "blue": 244,  # exact, the parabola
    "red": 251,  # error, "divides"
    "yellow": 111,  # the one bit, a flash
    "white": 254,  # rare: key results
}
