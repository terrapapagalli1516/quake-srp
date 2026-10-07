"""The .score text format: parser, pattern expansion, tempo map, automation.

The format, in short: one statement per line, `#` starts a comment:

    let     T_HIT = 12.40          # a macro: $T_HIT expands, also inside tokens (@$T_HIT+0.5)
    title   Theme sketch
    tuning  D2=72                  # reference pitch (default A4=440)
    length  60                     # exact output length in seconds
    tempo   1:1 60                 # quarter-note bpm from bar 1
    tempo   9:1 90 ramp            # ramp linearly from the previous tempo point to 90 at bar 9
    meter   6 7/8                  # from bar 6, bars of 7/8
    anchor  9:1 $T_HIT             # bar 9 lands at exactly 12.40 s (tempo before it is scaled)
    section 1:1 dark
    hit     9:1 big-impact         # a named marker, reported in seconds in the .json sidecar
    track   bass patch=bass gain=-6 pan=0 hall=0.2 group=melodic cutoff=200
    1:1     bass D2 2 0.9          # POS TRACK PITCH(ES) DUR [VEL] [param=value ...]
    @12.40  boom - 3s 1.0          # @ = absolute seconds; '-' = unpitched; '3s' = seconds
    @$T_HIT pad D3 >@20            # '>POS' = hold until POS
    pattern motif length=8
      0     D3 1.5 0.9             # pattern lines: relative beats, track optional
    end
    play    motif 2:1 track=bass transpose=-12 stretch=2 times=2
    play    motif @$T_HIT track=bell until=@20
    auto    pad cutoff @10 300 @12 3000     # automation: level gain gainl gainr width cutoff pan
    lightstyle pad @30 mmamammmmammamamaaamammma rate=10 dur=11

Positions: a beat number from 0 (quarter notes; fractions, sums and differences allowed:
12+1/3), `bar:beat` (1-based; the beat unit is the meter's denominator; `5:1+2.5` is 2.5
beats after bar 5 starts), or `@seconds` (also `@34.2-0.01`).
Durations: beats (quarter notes), seconds with an `s` suffix, or `>POS` (until POS).
Pitches: `D2`, `F#3`, `Bb4` (C4 is middle C), `m62` (MIDI), `72hz`, a chord `D3,F3,A3`, or `-`
(unpitched: the patch's `freq` applies).

The other statements:

    include FILE                   # read FILE (relative to this file) in place
    seeds   stable                 # each note's randomness from its track, time and pitch alone,
                                   # so an edit elsewhere never re-rolls it
    fadein S / fadeout S           # fades at the start and at the end of `length`
    loudness -16                   # normalise the integrated loudness; or `loudness off` + `master DB`
    ceiling -1                     # the limiter's true-peak ceiling, dBTP
    anchor  auto SEC [grid=1/2] [tempo=BPM]   # the hit at SEC lands on the grid step nearest
                                   # where the written tempo would put it; the tempo flexes to meet it
    arp     POS TRACK CHORD DUR [step=1/4] [order=0,1,2] [vel=..] [vel_end=..] [accent=0,4]
    level   TRACK POS DB           # short for `auto TRACK level POS DB`
    auto    master level|gain|cutoff|pocket|width|dry POS VALUE ...   # on the whole mix:
                                   # pocket = a dB cut in 250 Hz-4 kHz, a place for the narration;
                                   # dry (0..1) scales every track before its reverb sends
    reverb  hall t60=4.5 damp=0.3 predelay=0.03     # the shared `hall` and `room` reverbs
    duck    depth=8 mid=4 threshold=-42 ...         # how the score gets out of a --voice's way
    varispeed FROM TO POS SPEED ...                 # tape-style speed (0 = stopped) from FROM to TO

A `track` line's mixer keys are gain (dB), pan, hall, room, delay (sends), delay_time,
delay_fb, duck, hp, lp, eq_lo/eq_mid/eq_hi (dB; 250 Hz and 4 kHz crossovers), width,
human_t (ms) and human_v (a player's timing and dynamics), group (the stem), mute and bypass
(around the master's automation). Every other key=value is a parameter of the patch; a note's
own parameters override the track's, and unknown names are errors.
"""
from __future__ import annotations

import math
import re
import shlex
import zlib
from dataclasses import dataclass, field
from fractions import Fraction

import numpy as np

MIX_KEYS = {"patch", "gain", "pan", "hall", "room", "delay", "delay_time", "delay_fb",
            "duck", "hp", "lp", "mute", "group", "width", "human_t", "human_v",
            "eq_lo", "eq_mid", "eq_hi", "bypass"}
NOTE_UNIVERSAL = {"pan", "cents", "seed"}  # any note may carry these, whatever its patch (seed: see resolve)
AUTO_PARAMS = {"level", "gain", "gainl", "gainr", "width", "cutoff", "pan"}
MASTER_AUTO = {"level", "gain", "cutoff", "pocket", "width", "dry"}   # dry: 0..1 before the sends (the reverb rings on)   # pocket: dB cut in 250 Hz-4 kHz, a place for the voice
NOTE_NAMES = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}
KEYWORDS = {"title", "tuning", "length", "fadein", "fadeout", "loudness", "master", "ceiling",
            "tempo", "meter", "anchor", "section", "hit", "track", "level", "auto", "lightstyle",
            "reverb", "duck", "pattern", "end", "play", "arp", "let", "tapestop", "varispeed",
            "include", "seeds"}


class ScoreError(Exception):
    pass


def _err(line_no, msg):
    raise ScoreError(f"line {line_no}: {msg}")


_TERM = re.compile(r"[+-]?[^+-]+")


def num(s):
    """'3', '1.5', '1/3', '2+1/3', '-0.5', '34.2-0.01' -> Fraction."""
    s = str(s).strip()
    try:
        terms = _TERM.findall(s)
        if not terms or "".join(terms) != s:
            raise ValueError
        return sum((Fraction(t) for t in terms), Fraction(0))
    except (ValueError, ZeroDivisionError):
        raise ValueError(f"not a number: {s!r}")


def value(s):
    try:
        return float(num(s))
    except ValueError:
        return s


def parse_kv(tokens, line_no):
    out = {}
    for t in tokens:
        if "=" not in t:
            _err(line_no, f"expected key=value, got {t!r}")
        k, v = t.split("=", 1)
        out[k] = value(v)
    return out


_PITCH = re.compile(r"^([A-Ga-g])([#b]*)(-?\d+)$")


def parse_pitch(tok):
    """-> ('m', midi) | ('f', hz) | None"""
    if tok in ("-", "x"):
        return None
    if tok.lower().endswith("hz"):
        return ("f", float(tok[:-2]))
    if tok.startswith("m") and tok[1:].replace(".", "", 1).isdigit():
        return ("m", float(tok[1:]))
    m = _PITCH.match(tok)
    if not m:
        raise ValueError(f"not a pitch: {tok!r}")
    pc = NOTE_NAMES[m.group(1).upper()] + m.group(2).count("#") - m.group(2).count("b")
    return ("m", float(12 * (int(m.group(3)) + 1) + pc))


def looks_like_pitch(tok):
    try:
        for t in tok.split(","):
            parse_pitch(t)
        return True
    except ValueError:
        return False


def transpose(p, semis):
    if p is None or semis == 0:
        return p
    if p[0] == "m":
        return ("m", p[1] + semis)
    return ("f", p[1] * 2.0 ** (semis / 12.0))


@dataclass
class Track:
    name: str
    patch: str
    mix: dict
    params: dict
    line: int


@dataclass
class Note:
    pos: tuple            # ('b', beats) or ('s', seconds)
    track: str
    pitch: tuple | None
    dur: tuple            # ('b', beats), ('s', seconds) or ('u', until-position)
    vel: float
    params: dict
    line: int
    # filled in by Score.resolve()
    t0: float = 0.0
    t1: float = 0.0
    freq: float | None = None
    seed: int = 0


@dataclass
class Pattern:
    name: str
    length: float | None
    lines: list = field(default_factory=list)   # (line_no, tokens)


class TempoMap:
    """Beats <-> seconds. Tempo points (step or linear ramp) give the nominal map; anchors
    pin chosen beats to exact seconds by uniformly scaling the nominal time between them."""

    STEP = 1.0 / 192.0

    def __init__(self, tempos, anchors, max_beat):
        pts = sorted(tempos, key=lambda p: p[0])
        if not pts:
            pts = [(0.0, 120.0, False)]
        if pts[0][0] > 0:
            pts.insert(0, (0.0, pts[0][1], False))
        self.pts = pts
        hi = max(max_beat, pts[-1][0], max((a[0] for a in anchors), default=0)) + 64.0
        grid = np.arange(0.0, hi + self.STEP, self.STEP)
        extra = [p[0] for p in pts] + [a[0] for a in anchors]
        grid = np.union1d(grid, np.array(extra, dtype=float))
        # seconds per beat at each interval's midpoint: tempo steps fall on grid points,
        # so no interval straddles one; ramps integrate to second order
        mid_spb = 60.0 / self._bpm(0.5 * (grid[1:] + grid[:-1]))
        nominal = np.concatenate([[0.0], np.cumsum(mid_spb * np.diff(grid))])
        spb = 60.0 / self._bpm(grid)
        anchors = sorted(anchors)
        if not anchors or anchors[0][0] > 0:
            anchors = [(0.0, 0.0)] + anchors
        self.anchors = anchors
        tsec = nominal.copy()
        self.stretch = []
        for (b0, s0), (b1, s1) in zip(anchors, anchors[1:]):
            n0, n1 = np.interp([b0, b1], grid, nominal)
            if s1 <= s0 or n1 <= n0:
                raise ScoreError(f"anchors out of order: beat {b0:g} at {s0}s, beat {b1:g} at {s1}s")
            k = (s1 - s0) / (n1 - n0)
            m = (grid >= b0) & (grid <= b1)
            tsec[m] = s0 + (nominal[m] - n0) * k
            self.stretch.append((b0, b1, k))
        bl, sl = anchors[-1]
        nl = np.interp(bl, grid, nominal)
        m = grid > bl
        tsec[m] = sl + (nominal[m] - nl)
        self.grid, self.tsec, self.spb = grid, tsec, spb
        self.k0 = self.stretch[0][2] if self.stretch else 1.0

    def _bpm(self, b):
        pts = self.pts
        beats = np.array([p[0] for p in pts])
        i = np.clip(np.searchsorted(beats, b, side="right") - 1, 0, len(pts) - 1)
        out = np.array([pts[j][1] for j in i], dtype=float)
        for j in range(len(pts) - 1):
            if pts[j + 1][2]:   # ramp into point j+1
                m = (b >= pts[j][0]) & (b < pts[j + 1][0])
                u = (b[m] - pts[j][0]) / (pts[j + 1][0] - pts[j][0])
                out[m] = pts[j][1] + u * (pts[j + 1][1] - pts[j][1])
        return out

    def sec(self, b):
        b = float(b)
        if b < 0:
            return self.tsec[0] + b * self.spb[0] * self.k0
        if b > self.grid[-1]:
            return self.tsec[-1] + (b - self.grid[-1]) * self.spb[-1]
        return float(np.interp(b, self.grid, self.tsec))

    def beat(self, s):
        s = float(s)
        if s < self.tsec[0]:
            return (s - self.tsec[0]) / (self.spb[0] * self.k0)
        if s > self.tsec[-1]:
            return self.grid[-1] + (s - self.tsec[-1]) / self.spb[-1]
        return float(np.interp(s, self.tsec, self.grid))

    def effective_bpm(self, b0, b1):
        return 60.0 * (b1 - b0) / (self.sec(b1) - self.sec(b0))


class Score:
    def __init__(self, text, path="<score>"):
        self.path = path
        self.title = ""
        self.tuning = (69.0, 440.0)
        self.length = None
        self.fadein = 0.0
        self.fadeout = 0.0
        self.loudness = -16.0
        self.master = 0.0
        self.ceiling = -1.0
        self.seeds = "index"        # `seeds stable`: a note's randomness from its track, time and pitch only
        self.tempos = []        # (beat, bpm, ramp)
        self.meters = {1: (4, 4)}
        self.anchors = []       # (beat, sec)
        self.sections = []      # (pos, name)
        self.hits = []
        self.tracks: dict[str, Track] = {}
        self.autos = []         # (track | 'master', param, sec, value)
        self.tapestops = []     # varispeed windows: (from, to, [(sec, speed), ...])
        self.reverbs = {"hall": dict(t60=4.5, damp=0.3, predelay=0.03, width=1.0, seed=11),
                        "room": dict(t60=0.9, damp=0.5, predelay=0.008, width=0.8, seed=23)}
        self.duck = dict(depth=8.0, mid=4.0, threshold=-42.0, attack=0.05, release=0.45,
                         hold=0.25, lookahead=0.08)
        self.patterns: dict[str, Pattern] = {}
        self.notes: list[Note] = []
        self.macros = {}
        self._parse(text)
        self.resolve()

    @classmethod
    def load(cls, path):
        with open(path) as f:
            return cls(f.read(), str(path))

    # -------------------------------------------------------------- parsing

    _MACRO = re.compile(r"\$(\w+)")

    def _lines(self, text, base=None, depth=0):
        """Lines as (line id, tokens), with macros expanded and `include FILE` read in place
        (FILE relative to the including file)."""
        import os
        base = base or (os.path.dirname(os.path.abspath(self.path)) if self.path != "<score>" else ".")
        macros = self.macros
        for i, raw in enumerate(text.splitlines(), 1):
            stripped = re.split(r"(?:^|(?<=\s))#", raw, maxsplit=1)[0].strip()
            if stripped.startswith("include "):
                if depth > 8:
                    _err(i, "includes nested too deeply")
                path = os.path.join(base, shlex.split(stripped)[1])
                with open(path) as fh:
                    for ln, tok in self._lines(fh.read(), os.path.dirname(path), depth + 1):
                        yield (f"{os.path.basename(path)}:{ln}" if isinstance(ln, int) else ln), tok
                continue
            line = re.split(r"(?:^|(?<=\s))#", raw, maxsplit=1)[0].strip()   # '#' after space: comment; C#3 is a pitch
            if not line:
                continue
            tok = shlex.split(line)
            out = []
            for t in tok:
                if t.startswith("$") and t[1:] in macros:
                    out.extend(macros[t[1:]])
                    continue

                def sub(m):
                    if m.group(1) not in macros:
                        _err(i, f"unknown macro ${m.group(1)} (define it with: let {m.group(1)} = ...)")
                    return " ".join(macros[m.group(1)])
                out.append(self._MACRO.sub(sub, t))
            if out[0] == "let":
                if len(out) < 3 or out[2] != "=":
                    _err(i, "let NAME = tokens...")
                macros[out[1]] = out[3:]
                continue
            yield i, out

    def _parse(self, text):
        lines = list(self._lines(text))
        rest, timing = [], []
        cur = None
        # pass 1: meters, tuning, tracks, patterns (positions and patches depend on them)
        for ln, tok in lines:
            kw = tok[0]
            if cur is not None:
                if kw == "end":
                    cur = None
                else:
                    cur.lines.append((ln, tok))
                continue
            try:
                if kw == "pattern":
                    if len(tok) < 2:
                        _err(ln, "pattern needs a name")
                    kv = parse_kv(tok[2:], ln)
                    cur = Pattern(tok[1], kv.get("length"))
                    self.patterns[tok[1]] = cur
                elif kw == "end":
                    _err(ln, "'end' without 'pattern'")
                elif kw == "meter":
                    a, b = tok[2].split("/")
                    self.meters[int(tok[1])] = (int(a), int(b))
                elif kw == "tuning":
                    note, hz = tok[1].split("=")
                    p = parse_pitch(note)
                    self.tuning = (p[1], float(hz))
                elif kw == "track":
                    self._track(tok, ln)
                elif kw in ("tempo", "anchor"):
                    timing.append((ln, tok))
                else:
                    rest.append((ln, tok))
            except (ValueError, IndexError) as e:
                _err(ln, f"{' '.join(tok)!r}: {e}")
        if cur is not None:
            raise ScoreError(f"pattern {cur.name!r} has no 'end'")
        # pass 2: the tempo map
        autos = []
        for ln, tok in timing:
            try:
                if tok[0] == "anchor" and tok[1] == "auto":
                    kv = parse_kv(tok[3:], ln)
                    autos.append((float(num(tok[2])), float(kv.get("grid", 0.5)), kv.get("tempo"), ln))
                    continue
                p = self.pos(tok[1])
                if p[0] != "b":
                    _err(ln, f"{tok[0]} positions are musical (beats or bar:beat), not @seconds")
                if tok[0] == "tempo":
                    self.tempos.append((p[1], float(tok[2]), len(tok) > 3 and tok[3] == "ramp"))
                else:
                    self.anchors.append((p[1], float(num(tok[2]))))
            except (ValueError, IndexError) as e:
                _err(ln, f"{' '.join(tok)!r}: {e}")
        self._auto_anchors(autos)
        top = max([t[0] for t in self.tempos] + [a[0] for a in self.anchors] + [0.0])
        self.tmap = TempoMap([t for t in self.tempos], self.anchors, top + 1024.0)
        # pass 3: everything else
        for ln, tok in rest:
            try:
                self._statement(tok, ln)
            except (ValueError, IndexError, KeyError) as e:
                _err(ln, f"{' '.join(tok)!r}: {e}")

    def _auto_anchors(self, autos):
        """anchor auto SEC [grid=1/2] [tempo=BPM]: the hit lands on the grid step nearest to where
        the written tempo, run on from the previous anchor, would put it; the tempo between the
        two is then scaled a little to land it exactly. tempo= sets a new tempo from that beat.
        Moving the hit's time is then the only edit, whatever the cut does to the shot lengths."""
        self.auto_beats = []
        for sec, grid, tempo, ln in sorted(autos):
            same = [a for a in self.anchors if abs(a[1] - sec) < 1e-3]
            if same:                              # already anchored (two cues meet on one cut)
                if tempo is not None:
                    self.tempos = [t for t in self.tempos if abs(t[0] - same[0][0]) > 1e-9]
                    self.tempos.append((same[0][0], float(tempo), False))
                continue
            known = sorted([(0.0, 0.0)] + self.anchors, key=lambda a: a[1])
            prev = [a for a in known if a[1] <= sec + 1e-9][-1]
            nxt = [a for a in known if a[1] > sec + 1e-9]
            pts = sorted(self.tempos) or [(0.0, 120.0, False)]
            bpm = [t for t in pts if t[0] <= prev[0] + 1e-9]
            bpm = (bpm[-1] if bpm else pts[0])[1]
            b = prev[0] + max(1, round((sec - prev[1]) * bpm / 60.0 / grid)) * grid
            if nxt and b >= nxt[0][0] - 1e-9:
                _err(ln, f"anchor auto at {sec:.3f}s falls on beat {b:g}, at or after the next anchor "
                         f"(beat {nxt[0][0]:g} at {nxt[0][1]:.3f}s)")
            self.anchors.append((b, sec))
            self.auto_beats.append((sec, b))
            if tempo is not None:
                self.tempos.append((b, float(tempo), False))

    def _track(self, tok, ln):
        name = tok[1]
        if name in KEYWORDS or name == "master" or looks_like_pitch(name):
            _err(ln, f"track name {name!r} clashes with a keyword or a pitch")
        kv = parse_kv(tok[2:], ln)
        if "patch" not in kv:
            _err(ln, f"track {name!r} needs patch=...")
        from patches import PATCHES
        if kv["patch"] not in PATCHES:
            _err(ln, f"unknown patch {kv['patch']!r}; have: {', '.join(PATCHES)}")
        mix = dict(gain=0.0, pan=0.0, hall=0.0, room=0.0, delay=0.0, delay_time=0.375,
                   delay_fb=0.4, duck=1.0, hp=0.0, lp=0.0, mute=0.0, group="main", width=1.0,
                   human_t=0.0, human_v=0.0, eq_lo=0.0, eq_mid=0.0, eq_hi=0.0, bypass=0.0)
        params = {}
        for k, v in kv.items():
            if k == "patch":
                continue
            (mix if k in MIX_KEYS else params)[k] = v
        mix["group"] = str(mix["group"])
        self._check_params(kv["patch"], params, ln)
        self.tracks[name] = Track(name, kv["patch"], mix, params, ln)

    def _check_params(self, patch_name, params, ln):
        from patches import PATCHES
        known = PATCHES[patch_name][1]
        for k in params:
            if k not in known and k not in NOTE_UNIVERSAL:
                _err(ln, f"patch {patch_name!r} has no parameter {k!r}; it has: {', '.join(known)}")

    def bar_start(self, bar):
        beat = Fraction(0)
        meter = self.meters.get(1, (4, 4))
        for b in range(1, bar):
            meter = self.meters.get(b, meter)
            beat += Fraction(4 * meter[0], meter[1])
        return beat

    def meter_at(self, bar):
        m = (4, 4)
        for b in sorted(self.meters):
            if b <= bar:
                m = self.meters[b]
        return m

    def pos(self, tok):
        tok = str(tok)
        if tok.startswith("@"):
            return ("s", float(num(tok[1:])))
        if ":" in tok:
            bar, beat = tok.split(":")
            bar = int(bar)
            _, den = self.meter_at(bar)
            return ("b", float(self.bar_start(bar) + (num(beat) - 1) * Fraction(4, den)))
        return ("b", float(num(tok)))

    def beat_of(self, pos):
        return pos[1] if pos[0] == "b" else self.tmap.beat(pos[1])

    def sec(self, pos):
        return pos[1] if pos[0] == "s" else self.tmap.sec(pos[1])

    def dur(self, tok):
        if tok.startswith(">"):
            return ("u", self.pos(tok[1:]))
        if tok.endswith("s"):
            return ("s", float(num(tok[:-1])))
        return ("b", float(num(tok)))

    def _statement(self, tok, ln):
        kw = tok[0]
        if kw == "title":
            self.title = " ".join(tok[1:])
        elif kw == "length":
            self.length = float(num(tok[1]))
        elif kw == "fadein":
            self.fadein = float(num(tok[1]))
        elif kw == "fadeout":
            self.fadeout = float(num(tok[1]))
        elif kw == "loudness":
            self.loudness = None if tok[1] == "off" else float(tok[1])
        elif kw == "master":
            self.master = float(tok[1])
        elif kw == "ceiling":
            self.ceiling = float(tok[1])
        elif kw == "seeds":
            if tok[1] not in ("index", "stable"):
                _err(ln, "seeds is index or stable")
            self.seeds = tok[1]
        elif kw in ("section", "hit"):
            (self.sections if kw == "section" else self.hits).append((self.pos(tok[1]), " ".join(tok[2:])))
        elif kw == "level":
            self._auto([tok[1], "level"] + tok[2:], ln)
        elif kw == "auto":
            self._auto(tok[1:], ln)
        elif kw == "lightstyle":
            self._lightstyle(tok[1:], ln)
        elif kw == "tapestop":
            # tapestop POS DUR [resume=POS]: the whole score slows to a halt over DUR, like a
            # tape machine switched off, then stays silent until `resume` (default 4 s later)
            t0 = self.sec(self.pos(tok[1]))
            d = float(num(tok[2][:-1] if tok[2].endswith("s") else tok[2]))
            kv = parse_kv(tok[3:], ln)
            resume = self.sec(self.pos(kv["resume"])) if "resume" in kv else t0 + d + 4.0
            self.tapestops.append((t0, resume, [(t0, 1.0), (t0 + d, 0.0)]))
        elif kw == "varispeed":
            # varispeed FROM TO POS SPEED [POS SPEED ...]: between FROM and TO the whole score
            # plays at a varying speed (pitch with it, like tape): 1 normal, 0.25 a quarter,
            # 0 stopped (silent). At TO it jumps back to the timeline.
            a, b = self.sec(self.pos(tok[1])), self.sec(self.pos(tok[2]))
            pts = [(self.sec(self.pos(p)), float(num(v))) for p, v in zip(tok[3::2], tok[4::2])]
            self.tapestops.append((a, b, pts))
        elif kw == "reverb":
            if tok[1] not in self.reverbs:
                _err(ln, "reverbs are 'hall' and 'room'")
            self.reverbs[tok[1]].update(parse_kv(tok[2:], ln))
        elif kw == "duck":
            kv = parse_kv(tok[1:], ln)
            for k in kv:
                if k not in self.duck:
                    _err(ln, f"duck has no setting {k!r}; it has {', '.join(self.duck)}")
            self.duck.update(kv)
        elif kw == "arp":
            self.notes.extend(self._arp(tok[1:], ln, need_track=True, top=True))
        elif kw == "play":
            base = self.beat_of(self.pos(tok[2]))
            for n in self._expand_play(tok, ln, depth=0, base=base):
                n.pos = ("b", base + n.pos[1])
                self.notes.append(n)
        elif kw in KEYWORDS:
            _err(ln, f"{kw!r} is not allowed here")
        else:
            p = self.pos(kw)
            self.notes.extend(self._note_line(tok[1:], ln, p, need_track=True))

    # -------------------------------------------------------------- automation

    def _auto(self, tok, ln):
        track, param = tok[0], tok[1]
        if track == "master":
            if param not in MASTER_AUTO:
                _err(ln, f"master automates {', '.join(sorted(MASTER_AUTO))}")
        elif track not in self.tracks:
            _err(ln, f"unknown track {track!r}")
        elif param not in AUTO_PARAMS:
            _err(ln, f"automation parameters are {', '.join(sorted(AUTO_PARAMS))}")
        pairs = tok[2:]
        if not pairs or len(pairs) % 2:
            _err(ln, "auto TRACK PARAM POS VALUE [POS VALUE ...]")
        for p, v in zip(pairs[::2], pairs[1::2]):
            self.autos.append((track, param, self.sec(self.pos(p)), float(num(v))))

    def _lightstyle(self, tok, ln):
        """lightstyle TRACK POS LETTERS [rate=10] [dur=SEC] [mode=step|ramp] [phase=0]
                     [channel=both|left|right] [smooth=0.005] [scale=550] [outside=...]
        Track gain follows a Quake light style: letter a..z is (n * 22) / scale, read at `rate`
        letters a second, looping for `dur` seconds. 'step' holds each letter (id's light),
        'ramp' glides to the next (the slop preset's). Outside the window the gain is `outside`
        (default: the value of 'm', normal light)."""
        track = tok[0]
        if track not in self.tracks:
            _err(ln, f"unknown track {track!r}")
        start = self.sec(self.pos(tok[1]))
        letters = tok[2].lower()
        if not re.fullmatch(r"[a-z]+", letters):
            _err(ln, "a light style is letters a-z")
        kv = parse_kv(tok[3:], ln)
        rate = float(kv.pop("rate", 10.0))
        scale = float(kv.pop("scale", 550.0))
        dur = float(kv.pop("dur", len(letters) / rate))
        mode = str(kv.pop("mode", "step"))
        smooth = float(kv.pop("smooth", 0.005))
        outside = float(kv.pop("outside", (ord("m") - 97) * 22 / scale))
        chan = str(kv.pop("channel", "both"))
        lead = int(kv.pop("lead", 1))           # 0: no step from `outside` at the start (chained windows)
        trail = int(kv.pop("trail", 1))         # 0: no step back to `outside` at the end
        phase = float(kv.pop("phase", 0.0))
        depth = float(kv.pop("depth", 1.0))     # above 1 the flicker swings wider around normal (m)
        if kv:
            _err(ln, f"lightstyle has no setting {', '.join(kv)}")
        param = {"both": "gain", "left": "gainl", "right": "gainr"}[chan]
        t0 = start - phase / rate            # the window opens `phase` letters into the string
        steps = int(math.ceil((start + dur - t0) * rate - 1e-9))
        mval = (ord("m") - 97) * 22 / scale
        vals = [max(0.0, mval + depth * ((ord(letters[i % len(letters)]) - 97) * 22 / scale - mval))
                for i in range(steps + 1)]
        pts = []
        for i in range(steps):
            t = t0 + i / rate
            pts.append((t, vals[i]))
            if mode == "step":
                pts.append((t + 1 / rate - smooth, vals[i]))
        end = start + dur
        pts.append((t0 + steps / rate, vals[steps] if mode == "ramp" else vals[steps - 1]))
        ts = np.array([p[0] for p in pts]) + np.arange(len(pts)) * 1e-12
        vs = np.array([p[1] for p in pts])
        inside = [(t, v) for t, v in pts if start < t < end]
        pts = (([(start - smooth, outside)] if lead else []) + [(start, float(np.interp(start, ts, vs)))]
               + inside + [(end - (0 if trail else 1e-6), float(np.interp(end, ts, vs)))]
               + ([(end + smooth, outside)] if trail else []))
        for t, v in pts:
            self.autos.append((track, param, t, v))

    # -------------------------------------------------------------- notes

    def _note_line(self, tok, ln, pos, need_track, default_track=None):
        i = 0
        track = default_track
        if tok and tok[0] in self.tracks:
            track = tok[0]
            i = 1
        elif need_track:
            _err(ln, f"unknown track {tok[0]!r} (declare it with 'track')")
        if track is None:
            track = ""     # a pattern line without a track; filled in by `play track=...`
        pitches = [parse_pitch(t) for t in tok[i].split(",")]
        dur = self.dur(tok[i + 1])
        rest = tok[i + 2:]
        vel = 0.8
        if rest and "=" not in rest[0]:
            vel = float(num(rest[0]))
            rest = rest[1:]
        params = parse_kv(rest, ln)
        return [Note(pos, track, p, dur, vel, dict(params), ln) for p in pitches]

    def _arp(self, tok, ln, need_track, top=False):
        """arp POS [TRACK] CHORD DUR [step=1/4] [order=0,1,2,3] [vel=0.7] [vel_end=..]
               [accent=0,4] [accent_vel=0.95] [gate=1] [param=value ...]
        A note every `step` beats for DUR beats (or until DUR = >POS). `order` indexes the
        chord's notes, extended by octaves: with a 4-note chord, 4 is the root an octave up
        and -1 the top an octave down. `accent` lists positions within one cycle of `order`
        that play at accent_vel."""
        pos = self.pos(tok[0])
        if pos[0] == "s" and not top:
            _err(ln, "pattern positions are relative beats")
        start = self.beat_of(pos)
        i, track = 1, ""
        if tok[1] in self.tracks:
            track, i = tok[1], 2
        elif need_track:
            _err(ln, f"unknown track {tok[1]!r}")
        chord = [parse_pitch(t) for t in tok[i].split(",")]
        if any(c is None for c in chord):
            _err(ln, "arp needs pitched chord notes")
        total = self.dur(tok[i + 1])
        if total[0] == "u":
            if not top:
                _err(ln, "'>POS' lengths are for top-level arps")
            beats = self.beat_of(total[1]) - start
        elif total[0] == "b":
            beats = total[1]
        else:
            _err(ln, "arp length is in beats or >POS")
        kv = parse_kv(tok[i + 2:], ln)
        lst = lambda v: [int(float(x)) for x in str(v).split(",") if x != ""]  # noqa: E731
        step = float(kv.pop("step", 0.25))
        order = lst(kv.pop("order", ",".join(str(k) for k in range(len(chord)))))
        vel = float(kv.pop("vel", 0.7))
        vel_end = float(kv.pop("vel_end", vel))
        accents = set(lst(kv.pop("accent", "")))
        accent_vel = float(kv.pop("accent_vel", min(1.0, vel + 0.2)))
        gate = float(kv.pop("gate", 1.0))
        m = len(chord)
        steps = int(math.ceil(beats / step - 1e-6))      # every step that starts before the end
        out = []
        base = start if top else pos[1]
        for k in range(steps):
            idx = order[k % len(order)]
            p = transpose(chord[idx % m], 12 * (idx // m))
            v = vel + (vel_end - vel) * (k / max(steps - 1, 1))
            if (k % len(order)) in accents:
                v = accent_vel + (v - vel)
            out.append(Note(("b", base + k * step), track, p, ("b", step * gate), v, dict(kv), ln))
        return out

    def _expand_play(self, tok, ln, depth, base=0.0):
        """Notes of `play NAME POS opts`, positions relative to POS (in beats)."""
        if depth > 16:
            _err(ln, "patterns nested too deeply (a cycle?)")
        name = tok[1]
        if name not in self.patterns:
            _err(ln, f"unknown pattern {name!r}")
        pat = self.patterns[name]
        kv = parse_kv(tok[3:], ln)
        track = kv.pop("track", None)
        semis = kv.pop("transpose", 0.0) + 12.0 * kv.pop("octave", 0.0)
        stretch = kv.pop("stretch", 1.0)
        velk = kv.pop("vel", 1.0)
        times = kv.pop("times", None)
        until = kv.pop("until", None)
        start_at = kv.pop("from", None)
        retro = bool(kv.pop("retro", 0))
        base_notes = []
        for pln, ptok in pat.lines:
            if ptok[0] == "arp":
                if ":" in ptok[1] or ptok[1].startswith("@"):
                    _err(pln, "pattern positions are relative beats (e.g. 1.5)")
                base_notes.extend(self._arp(ptok[1:], pln, need_track=False))
            elif ptok[0] == "play":
                rel = self.pos(ptok[2])
                if rel[0] != "b":
                    _err(pln, "pattern positions are relative beats")
                for n in self._expand_play(ptok, pln, depth + 1):
                    n.pos = ("b", rel[1] + n.pos[1])
                    base_notes.append(n)
            else:
                if ":" in ptok[0] or ptok[0].startswith("@"):
                    _err(pln, "pattern positions are relative beats (e.g. 1.5), not bar:beat or @")
                p = self.pos(ptok[0])
                base_notes.extend(self._note_line(ptok[1:], pln, p, need_track=False))
        length = pat.length
        if length is None:
            ends = [n.pos[1] + (n.dur[1] if n.dur[0] == "b" else 0) for n in base_notes]
            length = max(ends, default=0.0)
        every = kv.pop("every", length * stretch)
        end_rel = None
        if until is not None:
            if depth > 0:
                _err(ln, "until= is for top-level plays")
            end_rel = self.beat_of(self.pos(until)) - base
            if times is None:
                times = max(1, math.ceil(end_rel / every - 1e-9))
        times = int(times if times is not None else 1)
        from_rel = None
        if start_at is not None:
            from_rel = self.beat_of(self.pos(start_at)) - base
        if track is not None and track not in self.tracks:
            _err(ln, f"unknown track {track!r}")
        out = []
        for r in range(times):
            for n in base_notes:
                start = n.pos[1]
                d = n.dur
                if retro:
                    start = length - (start + (d[1] if d[0] == "b" else 0.0))
                nd = ("b", d[1] * stretch) if d[0] == "b" else d
                rel = r * every + start * stretch
                if end_rel is not None and rel >= end_rel - 1e-9:
                    continue
                if from_rel is not None and rel < from_rel - 1e-9:
                    continue
                tr = track or n.track
                if not tr:
                    _err(n.line, f"pattern {name!r} line has no track and play gives none")
                prm = dict(n.params)
                prm.update(kv)
                out.append(Note(("b", rel), tr, transpose(n.pitch, semis), nd, n.vel * velk, prm, n.line))
        return out

    # -------------------------------------------------------------- resolution

    def resolve(self):
        ref_m, ref_f = self.tuning
        seen = {}
        for i, n in enumerate(self.notes):
            if n.track not in self.tracks:
                _err(n.line, f"unknown track {n.track!r}")
            self._check_params(self.tracks[n.track].patch, n.params, n.line)
            t0 = self.sec(n.pos)
            if n.dur[0] == "s":
                t1 = t0 + n.dur[1]
            elif n.dur[0] == "u":
                t1 = self.sec(n.dur[1])
            else:
                t1 = self.tmap.sec(self.beat_of(n.pos) + n.dur[1])
            if t1 <= t0:
                _err(n.line, f"note at {t0:.3f}s ends before it starts ({t1:.3f}s)")
            n.t0, n.t1 = t0, t1
            cents = float(n.params.pop("cents", 0.0))
            if n.pitch is None:
                n.freq = None
            elif n.pitch[0] == "f":
                n.freq = n.pitch[1] * 2.0 ** (cents / 1200.0)
            else:
                n.freq = ref_f * 2.0 ** ((n.pitch[1] - ref_m) / 12.0 + cents / 1200.0)
            sd = n.params.get("seed")      # seed=N pins a note's randomness, so edits elsewhere can't move it
            if sd is not None:
                key = f"seed{int(float(sd))}|{n.freq}"
            elif self.seeds == "stable":   # track, time and pitch (and which of identical notes): no edit elsewhere moves it
                base = f"{n.track}|{round(t0 * 1e4)}|{n.freq}"
                k = seen.get(base, 0)
                seen[base] = k + 1
                key = f"{base}|{k}"
            else:                          # the default: also the line and the note's index
                key = f"{n.track}|{round(t0 * 1e4)}|{n.freq}|{n.line}|{i}"
            n.seed = zlib.crc32(key.encode())
        for n in self.notes:
            n.params.pop("seed", None)
        self.notes.sort(key=lambda n: n.t0)

    def markers(self):
        """Sections and hits in seconds."""
        return ([(self.sec(p), name) for p, name in self.sections],
                [(self.sec(p), name) for p, name in self.hits])

    def automation(self, track, param):
        """[(sec, value)] sorted, for a track (or 'master') and parameter."""
        return sorted(((t, v) for tr, p, t, v in self.autos if tr == track and p == param),
                      key=lambda x: x[0])

    def tempo_report(self):
        lines = []
        for (b0, b1, k) in self.tmap.stretch:
            if abs(k - 1.0) > 1e-9:
                lines.append(f"beats {b0:g}-{b1:g} ({self.tmap.sec(b0):.2f}-{self.tmap.sec(b1):.2f}s): "
                             f"nominal time x{k:.4f} to meet the anchor "
                             f"(avg {self.tmap.effective_bpm(b0, b1):.2f} bpm)")
        return lines
