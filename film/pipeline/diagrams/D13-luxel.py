#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D13, one spot by a flickering flame (ST1c, 6.7 s): it wanders, and averages to id's.
A panel at the bottom right of the Classic box (x 240-1680).

FRAMERATE.md, "Steady torches that flicker", and the repository's quake-rs/src/render/torch.rs
(read as checked out): each frame a steady torch adds to each luxel it lights

    strength · depth · Σ_k (s_k(t)/s̄_k - 1)/√2 · share · d_lightstylevalue[0]/256

with s_k world.qc's two flicker strings (FLICKER_1 = style 1, FLICKER_6 = style 6) through
the light-style glide, each at the flame kind's rate and at the torch's own phase, s̄_k each
string's mean: zero-mean, "so every luxel's light averages to id's". The flicker here is
computed with torch.rs's own rules (`Torch::scale`, `phase_of`, `lightstyle_value_at` with
GLIDE_STEP 2) for the flame ST1c films: e1m3's `light_flame_small_yellow` at (-1352, -432,
14), a SmallFlame (style 1 at 0.8 of a light style's rate, style 6 at 0.9, depth 1.0),
strength 1 (slop's).

Schematic: the luxel's baked value (120) and the flame's share of it (80); the shape of the
line is the flame's. Cue `cl0` is cl.time at the shot's first frame (3.4 for ST1c: the film
tool's warm-up of 2.4 s plus the 1.0 s handle). `--scale S` scales the panel about its bottom
right corner, (1664, 1064), so it stays in the box.
"""

import math
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

import numpy as np  # noqa: E402

SCALE = 1.0
if "--scale" in sys.argv:
    i = sys.argv.index("--scale")
    SCALE = float(sys.argv[i + 1])
    del sys.argv[i : i + 2]

# ------------------------------------------------------------ the facts ----

_src = (quake.REPO / "quake-rs/src/render/torch.rs").read_text()
FLICKER_1 = re.search(r'const FLICKER_1: &\[u8\] = b"([a-z]+)";', _src).group(1)
FLICKER_6 = re.search(r'const FLICKER_6: &\[u8\] = b"([a-z]+)";', _src).group(1)
_small = re.search(r"TorchKind::SmallFlame => Flicker \{ voices: \[voice\(FLICKER_(\d), ([\d.]+)\), "
                   r"voice\(FLICKER_(\d), ([\d.]+)\)\], depth: ([\d.]+) \}", _src)
VOICES = [({"1": FLICKER_1, "6": FLICKER_6}[_small.group(1)], float(_small.group(2))),
          ({"1": FLICKER_1, "6": FLICKER_6}[_small.group(3)], float(_small.group(4)))]
DEPTH = float(_small.group(5))
assert [(v[0] == FLICKER_1, v[1]) for v in VOICES] == [(True, 0.8), (False, 0.9)] and DEPTH == 1.0

# the flame, from e1m3's entity lump
_ents = quake.bsp("e1m3").entities
FLAME = None
for _blk in re.findall(r"\{([^}]*)\}", _ents):
    kv = dict(re.findall(r'"([^"]*)"\s+"([^"]*)"', _blk))
    if kv.get("classname") == "light_flame_small_yellow" and kv.get("origin") == "-1352 -432 14":
        FLAME = tuple(float(v) for v in kv["origin"].split())
assert FLAME is not None
STRENGTH = 1.0
LUXEL, SHARE = 120.0, 80.0  # schematic
STYLE0 = 264  # d_lightstylevalue[0]: style 0 is "m"

_M = 0xFFFFFFFF


def phase_of(origin, voice: int, n: int) -> float:
    """torch.rs's phase_of: FNV-1a over the whole units, murmur3's finaliser, sixteenths of a letter."""
    h = (0x811C9DC5 ^ voice) & _M
    for c in origin:
        h = ((h ^ (int(round(c)) & _M)) * 0x01000193) & _M
    h ^= h >> 16
    h = (h * 0x85EBCA6B) & _M
    h ^= h >> 13
    h = (h * 0xC2B2AE35) & _M
    h ^= h >> 16
    return (h % (n * 16)) / 16


def letter(p: str, i: int) -> int:
    return (ord(p[i % len(p)]) - 97) * 22


def glide(p: str, letters: float) -> int:
    """lightstyle_value_at, Smooth: from letter i toward i+1 in whole steps of 2."""
    i = math.floor(letters)
    frac = float(np.float32(letters - i))
    here, nxt = letter(p, i), letter(p, i + 1)
    x = (nxt - here) * frac / 2
    return here + (math.floor(x + 0.5) if x >= 0 else -math.floor(-x + 0.5)) * 2


PHASES = [phase_of(FLAME, k, len(VOICES[k][0])) for k in range(2)]
MEANS = [sum(letter(p, i) for i in range(len(p))) / len(p) for p, _ in VOICES]


def flicker_scale(cl: float) -> float:
    """Torch::scale: strength · depth · Σ (s_k/s̄_k - 1) / √2."""
    tenths = float(np.float32(cl)) * 10
    swing = sum(glide(p, tenths * r + PHASES[k]) / MEANS[k] - 1 for k, (p, r) in enumerate(VOICES))
    return STRENGTH * DEPTH * swing / math.sqrt(2)


# ----------------------------------------------------------------- time ----

CUES = Cues(panel=0.0, mean=4.6, label=5.4, cl0=3.4).argv()
DURATION = duration_arg(6.7)  # TS below reads it at import
TS = np.arange(0, DURATION + 1e-9, 1 / 240)
_CURVE: dict[float, tuple] = {}


def curve():
    """The luxel at 240 Hz over the shot, and its running mean (computed once per cl0)."""
    if CUES.cl0 not in _CURVE:
        val = np.array([LUXEL + flicker_scale(CUES.cl0 + x) * SHARE * STYLE0 / 256 for x in TS])
        _CURVE[CUES.cl0] = (val, np.cumsum(val) / np.arange(1, len(val) + 1))
    return _CURVE[CUES.cl0]


# --------------------------------------------------------------- layout ----

CORNER = (1664, 1064)
PW, PH = 640, 196
PX, PY = CORNER[0] - PW, CORNER[1] - PH
GX, GY, GW, GH = PX + 24, PY + 52, PW - 48, 120


def frame(c, t):
    c.background()
    a = ramp(t, CUES.panel, 0.4, "smooth")
    with c.moved(scale=SCALE, about=CORNER):
        panel(c, PX, PY, PW, PH, a, dark=0.84)
        c.text("ONE SPOT BY THE FLAME", PX + 24, PY + 16, 2, "text", a)
        ax = Axes(c, GX, GY, GW, GH, xlim=(0, DURATION), ylim=(LUXEL - 22, LUXEL + 30))
        c.rect(GX, GY, GW, GH, fill="panel", alpha=a)
        c.line(GX, GY + GH, GX + GW, GY + GH, "axis", 2, a)
        ax.hline(LUXEL, 105, 4, a)
        val, runmean = curve()
        n = int(np.searchsorted(TS, t, side="right"))
        if n >= 2:
            ax.plot(TS[:n], val[:n], 235, 3, a)
        ma = ramp(t, CUES.mean, 0.8, "smooth")
        k0 = int(0.3 * 240)  # from 0.3 s, past the first few samples' swing
        if ma > 0 and n > k0 + 1:
            ax.plot(TS[k0:n], runmean[k0:n], "white", 3, a * ma, dash=[7, 5])
        c.text("ID'S (BAKED)", GX + 8, GY + 8, 2, 105, a)
        c.text("MEAN = ID'S", GX + GW - 8, GY + 6, 3, 254, ramp(t, CUES.label, 0.5), "right")


if __name__ == "__main__":
    main(frame, DURATION, "D13-luxel", CUES, "e1m1")
