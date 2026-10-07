"""Time: easing, ramps, fades, keyframes, and named cues.

A diagram is a function of time, `frame(c, t)`. Everything that moves asks
one of these helpers how far along it is at `t` (seconds). Name the moments
in a `Cues` table at the top of the diagram, so the whole thing can be retimed
to the narration by editing numbers (or a JSON file, `--cues`), never code.
"""

from __future__ import annotations

import json
import math
from pathlib import Path


# ------------------------------------------------------------ easing ----


def linear(u: float) -> float:
    return u


def smooth(u: float) -> float:
    """Smoothstep: gentle in and out."""
    return u * u * (3 - 2 * u)


def inout(u: float) -> float:
    """Cubic in-out: the default for moves."""
    return 4 * u * u * u if u < 0.5 else 1 - (-2 * u + 2) ** 3 / 2


def inout5(u: float) -> float:
    """Quintic in-out: long, decisive moves (zooms)."""
    return 16 * u**5 if u < 0.5 else 1 - (-2 * u + 2) ** 5 / 2


def out(u: float) -> float:
    """Cubic out: things arriving."""
    return 1 - (1 - u) ** 3


def in_(u: float) -> float:
    """Cubic in: things leaving."""
    return u**3


def back(u: float) -> float:
    """Out with a small overshoot: a label popping in."""
    c1 = 1.70158
    c3 = c1 + 1
    return 1 + c3 * (u - 1) ** 3 + c1 * (u - 1) ** 2


EASE = {"linear": linear, "smooth": smooth, "inout": inout, "inout5": inout5, "out": out, "in": in_, "back": back}


def clamp01(u: float) -> float:
    return 0.0 if u < 0 else 1.0 if u > 1 else u


def ramp(t: float, t0: float, dur: float, ease: str = "inout") -> float:
    """0 before t0, 1 after t0 + dur, eased between."""
    if dur <= 0:
        return 1.0 if t >= t0 else 0.0
    return EASE[ease](clamp01((t - t0) / dur))


def fade(t: float, t_in: float, t_out: float | None = None, d_in: float = 0.4, d_out: float = 0.4) -> float:
    """An opacity: rises over d_in from t_in, falls over d_out to end at t_out (None: stays)."""
    a = ramp(t, t_in, d_in, "smooth")
    if t_out is not None:
        a *= 1 - ramp(t, t_out - d_out, d_out, "smooth")
    return a


def lerp(a, b, u: float):
    """Numbers or tuples, linearly."""
    if isinstance(a, (tuple, list)):
        return type(a)(x + (y - x) * u for x, y in zip(a, b))
    return a + (b - a) * u


def lerp_log(a: float, b: float, u: float) -> float:
    """Between two scales evenly in log space (a zoom that feels steady)."""
    return math.exp(math.log(a) + (math.log(b) - math.log(a)) * u)


def keys(t: float, frames: list[tuple]) -> object:
    """Piecewise keyframes: [(t0, v0), (t1, v1, 'ease'), ...] -> the value at t.

    The ease named on a key shapes the move that ends at that key.
    """
    if t <= frames[0][0]:
        return frames[0][1]
    for k0, k1 in zip(frames, frames[1:]):
        if t <= k1[0]:
            ease = k1[2] if len(k1) > 2 else "inout"
            u = EASE[ease](clamp01((t - k0[0]) / max(1e-9, k1[0] - k0[0])))
            return lerp(k0[1], k1[1], u)
    return frames[-1][1]


def typed(text: str, u: float) -> str:
    """The first share u of a string: a typewriter."""
    return text[: int(round(clamp01(u) * len(text)))]


def stagger(t: float, t0: float, i: int, step: float, dur: float, ease: str = "out") -> float:
    """Item i of a row that appears one after another, `step` apart."""
    return ramp(t, t0 + i * step, dur, ease)


# -------------------------------------------------------------- cues ----


class Cues:
    """Named moments, in seconds: `cue = Cues(title=0.3, zoom=1.2)`; `cue.zoom` -> 1.2.

    `Cues.load(path)` overrides some of them from a JSON object (the film's
    narration timings), so a diagram is retimed without touching its code.
    `cue.shift(after, dt)` moves every cue at or after a moment by dt.
    """

    def __init__(self, **times: float):
        object.__setattr__(self, "_t", dict(times))

    def __getattr__(self, name: str) -> float:
        try:
            return self._t[name]
        except KeyError:
            raise AttributeError(f"no cue {name!r}; cues are {sorted(self._t)}") from None

    def __setattr__(self, name: str, value: float) -> None:
        self._t[name] = float(value)

    def __getitem__(self, name: str) -> float:
        return self._t[name]

    def items(self):
        return sorted(self._t.items(), key=lambda kv: kv[1])

    def load(self, path: str | Path) -> "Cues":
        data = json.loads(Path(path).read_text())
        unknown = set(data) - set(self._t)
        if unknown:
            raise KeyError(f"{path}: unknown cues {sorted(unknown)}")
        self._t.update({k: float(v) for k, v in data.items()})
        return self

    def argv(self) -> "Cues":
        """Load `--cues FILE` from the command line now, at import: for a script that computes
        something from its cues before run() does (run() loads the file again, harmlessly).
        `CUES = Cues(...).argv()`."""
        import sys

        if "--cues" in sys.argv[:-1]:
            self.load(sys.argv[sys.argv.index("--cues") + 1])
        return self

    def shift(self, after: float, dt: float) -> "Cues":
        for k, v in self._t.items():
            if v >= after:
                self._t[k] = v + dt
        return self

    def dump(self) -> str:
        return json.dumps(dict(self.items()), indent=2)
