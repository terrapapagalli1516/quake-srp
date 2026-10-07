#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.14"
# dependencies = ["numpy", "scipy", "numba"]
# ///
"""The film's designed sounds: whooshes, hits, stingers, risers, the two-pixel glitch, the
scratch, UI ticks and the torches. Made from the score's own instruments (music/synth) and
fresh DSP, in its world (dark, metallic, industrial) and its tuning (D2 = 72 Hz, the game's
tick rate).

    uv run film/pipeline/sound/design.py      # every sound -> FILM_ROOT/sound/designed/NAME.wav + index.json
    uv run film/pipeline/sound/design.py torch-whoosh glitch     # just these

Each sound is 48 kHz, 24-bit stereo. Its `sync` (seconds from the file's start) is the point
a cue lands on the cue's time: the peak of a whoosh, the hit of a stinger, the dead stop of
a riser or a reversed whoosh. Levels: each is set to a designed loudness (its loudest 400 ms,
momentary, in LUFS) that places it among id's own sounds (median -9 LUFS), true peak under
-1 dBTP. The renderer plays files at these levels; a cue's gain= is relative to them.
"""
from __future__ import annotations

import json
import sys
import zlib
from pathlib import Path

import numpy as np
from scipy import signal

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sfxlib  # noqa: E402
from sfxlib import SR, dsp  # noqa: E402

sys.path.insert(0, str(sfxlib.HERE.parent / "music" / "synth"))
from patches import PATCHES  # noqa: E402

OUT = sfxlib.DESIGNED_DIR

# The score's tuning: D2 = 72 Hz.
D1, D2, D3, D4, D5, D6, D7 = 36.0, 72.0, 144.0, 288.0, 576.0, 1152.0, 2304.0
A2, A3, A4, A5, A6 = 108.0, 216.0, 432.0, 864.0, 1728.0
F3 = D3 * 2 ** (3 / 12)
Bb2 = D2 * 2 ** (8 / 12)


# ---------------------------------------------------------------- building blocks

def P(name, f=None, dur=0.5, vel=0.8, seed=1, **params):
    """One note of a score instrument, (2, n)."""
    fn, defaults, _, mono = PATCHES[name]
    assert not mono
    p = dict(defaults)
    unknown = set(params) - set(p)
    assert not unknown, f"{name}: unknown {unknown}"
    p.update(params)
    return fn(f, dur, vel, p, np.random.default_rng(seed))


def t_(n):
    return np.arange(n) / SR


def N(s):
    return dsp.nsamp(s)


def pink(n, rng):
    """Pink-ish noise (Kellet's filter on white)."""
    w = rng.standard_normal(n)
    b = [0.049922035, -0.095993537, 0.050612699, -0.004408786]
    a = [1.0, -2.494956002, 2.017265875, -0.522189400]
    y = signal.lfilter(b, a, w)
    return y / (np.std(y) + 1e-12)


def lp_noise(n, rate, rng):
    """Slow random motion: white noise low-passed at `rate` Hz, unit std."""
    y = dsp.butter(rng.standard_normal(n), "lowpass", rate, 2)
    return y / (np.std(y) + 1e-12)


def place(dst, x, at_s, gain=1.0):
    """Add stereo x into dst at at_s seconds (cut at dst's end)."""
    x = dsp.stereo(x)
    a = int(round(at_s * SR))
    if a < 0:
        x, a = x[:, -a:], 0
    m = min(x.shape[1], dst.shape[1] - a)
    if m > 0:
        dst[:, a:a + m] += gain * x[:, :m]
    return dst


def pan_glide(mono, p0, p1, curve=None):
    """Mono -> stereo with a constant-power pan moving from p0 to p1 (curve: 0..1 per sample)."""
    n = mono.size
    u = curve if curve is not None else np.linspace(0, 1, n)
    p = np.clip(p0 + (p1 - p0) * u, -1, 1)
    ang = (p + 1) * np.pi / 4
    return np.vstack([mono * np.cos(ang), mono * np.sin(ang)]) * np.sqrt(2)


def comb(x, f0, fb=0.75, mix=0.3):
    """Feedback comb at f0 (a metallic, pitched ring tuned to the score)."""
    d = max(1, int(round(SR / f0)))
    a = np.zeros(d + 1)
    a[0], a[d] = 1.0, -fb
    y = signal.lfilter([1.0 - fb], a, x, axis=-1)
    return (1 - mix) * x + mix * y


def reverb(x, t60=2.0, damp=0.35, predelay=0.02, mix=0.25, seed=7, width=1.0):
    x = dsp.stereo(x)
    ir = dsp.make_ir(t60=t60, damp=damp, predelay=predelay, width=width, seed=seed)
    n = x.shape[1] + ir.shape[1]
    wet = dsp.convolve_reverb(dsp.fit(x, n), ir)
    return dsp.fit(x, n) * (1 - 0.5 * mix) + wet * mix


def bell_curve(n, peak_s, rise_s, fall_s):
    """0..1 envelope: Gaussian rise to peak at peak_s, Gaussian fall after."""
    t = t_(n)
    e = np.where(t < peak_s, np.exp(-((t - peak_s) / max(rise_s, 1e-3)) ** 2),
                 np.exp(-((t - peak_s) / max(fall_s, 1e-3)) ** 2))
    return e


def smoothstep(u):
    u = np.clip(u, 0, 1)
    return u * u * (3 - 2 * u)


def bitcrush(x, bits):
    q = 2.0 ** (bits - 1)
    return np.round(x * q) / q


def sample_hold(x, k):
    """Hold every k-th sample (sample-rate reduction to SR / k)."""
    k = max(1, int(k))
    idx = (np.arange(x.shape[-1]) // k) * k
    return x[..., idx]


def edge(x, s=0.0015):
    """Short fades at both ends (no clicks)."""
    m = min(x.shape[-1] // 2, N(s))
    if m > 1:
        r = np.linspace(0, 1, m)
        x[..., :m] *= r
        x[..., -m:] *= r[::-1]
    return x


def dense(x, drive):
    """Peak-normalise, then a tanh soft-clip: a denser, more industrial hit (lower crest factor)."""
    pk = np.max(np.abs(x)) + 1e-12
    return dsp.softclip(x / pk, drive) * pk


def trim(x, floor_db=-62.0, keep_start=True):
    """Cut the silent tail (below floor_db of the peak), with a short fade."""
    a = np.abs(x).max(axis=0)
    thr = a.max() * 10 ** (floor_db / 20)
    idx = np.nonzero(a > thr)[0]
    end = idx[-1] + N(0.02) if idx.size else x.shape[1]
    y = x[:, :min(end, x.shape[1])].copy()
    return dsp.tail_fade(y, 0.01)


# ---------------------------------------------------------------- whooshes

def whoosh(dur, peak, f_lo, f_hi, pan0=-0.7, pan1=0.7, metal=0.25, metal_f=D3, res=0.45,
           sub=0.0, tone=0.0, rev=0.15, seed=1):
    """Air moving past: band-passed pink noise whose band and level swell to `peak` and fall,
    panned across, with a comb ring tuned to the score (metal) and an optional sub and tonal
    Doppler layer."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    t = t_(n)
    a = bell_curve(n, peak, peak / 2.0, (dur - peak) / 2.4)
    fc = f_lo * (f_hi / f_lo) ** (a ** 0.8)
    nz = pink(n, rng)
    band = dsp.svf(nz, fc, res, "bp")
    body = dsp.svf(nz, fc * 1.6, 0.1, "lp") * 0.5
    y = (band + body)
    y = comb(y, metal_f, 0.8, metal) if metal else y
    y *= a ** 1.3
    if tone:
        fd = metal_f * 2 * (1.06 - 0.12 * smoothstep(t / dur))           # a falling Doppler
        y += tone * dsp.saw(fd, n) * a ** 2 * 0.3
    st = pan_glide(y, pan0, pan1, smoothstep((t - peak) / (dur * 0.9) + 0.5))
    if sub:
        s = np.sin(2 * np.pi * dsp.phase_ramp(D2 * (1.0 - 0.5 * smoothstep(t / dur)), n)) * a ** 2
        st += sub * dsp.softclip(s, 1.5)
    st = edge(st, 0.003)
    return reverb(st, t60=1.6, damp=0.4, mix=rev, seed=seed) if rev else st


def whoosh_reverse(dur, seed=3):
    """A whoosh sucked backwards into the cut: rises and stops dead at its end."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    t = t_(n)
    nz = pink(n, rng)
    a = dsp.perc_env(n, 0.004, dur * 0.55, 0.9)
    fc = 600 + 5000 * np.exp(-t / (dur * 0.25))
    y = dsp.svf(nz, fc, 0.35, "bp") * a + 0.4 * dsp.svf(nz, 400, 0.0, "lp") * a
    y = comb(y, D4, 0.7, 0.25)
    st = pan_glide(y, 0.5, -0.5)
    wet = reverb(st, t60=dur * 0.9, damp=0.45, mix=0.7, predelay=0.0, seed=seed)[:, :n]
    y = wet[:, ::-1].copy()
    y *= (t / dur) ** 0.6
    return dsp.tail_fade(y, 0.002)


# ---------------------------------------------------------------- hits, stingers

def hit_caption():
    x = np.zeros((2, N(1.4)))
    place(x, P("clank", D5, 0.3, 0.85, seed=11, decay=0.35, ring=0.35, width=0.5, click=0.25), 0)
    place(x, P("kick", None, 0.3, 0.7, seed=12, freq=D2, start=2.6, decay=0.22, click=0.4), 0, 0.8)
    place(x, P("tick", None, 0.05, 0.9, seed=13, tone=D7, decay=0.008, click=0.3), 0, 0.25)
    return trim(reverb(dense(x, 3.0), t60=0.9, damp=0.3, mix=0.18, seed=14))


def hit_soft():
    x = np.zeros((2, N(0.9)))
    place(x, P("kick", None, 0.2, 0.5, seed=21, freq=D2, start=2.0, decay=0.18, click=0.15, drive=1.5), 0)
    place(x, P("strike", D5, 0.2, 0.5, seed=22, decay=0.35, bright=0.4, click=0.2), 0, 0.35)
    return trim(reverb(x, t60=0.8, damp=0.3, mix=0.15, seed=23))


def hit_heavy():
    x = np.zeros((2, N(4.5)))
    place(x, P("impact", None, 1.0, 1.0, seed=31, freq=D1, decay=2.4, crack=0.2), 0)
    place(x, P("strike", D3, 1.0, 0.9, seed=32, decay=2.6, bright=0.55), 0, 0.7)
    place(x, P("clank", D4, 0.5, 0.9, seed=33, decay=0.6, click=0.25), 0, 0.5)
    place(x, dsp.butter(P("crash", None, 1.0, 0.8, seed=34, decay=2.0, tone=0.8), "lowpass", 6000), 0, 0.5)
    return trim(reverb(dense(x, 2.5), t60=3.2, damp=0.3, mix=0.3, seed=35))


def stinger_bumper(pre=0.8):
    """A reversed swell into a hit: an impact, an open power chord on D, a bell fifth on top."""
    x = np.zeros((2, N(pre + 4.5)))
    place(x, P("swell", D4, pre, 0.9, seed=41, source="mix"), 0, 0.8)
    place(x, P("impact", None, 1.0, 1.0, seed=42, freq=D1, decay=2.2), pre)
    place(x, P("chug", D2, 1.2, 0.9, seed=43, mute=0.15, release=0.6), pre, 0.9)
    place(x, P("bell", D4, 1.0, 0.7, seed=44, decay=3.0), pre, 0.45)
    place(x, P("bell", A4, 1.0, 0.6, seed=45, decay=3.0), pre + 0.004, 0.35)
    place(x, dsp.butter(P("crash", None, 1.0, 0.7, seed=46, decay=2.2), "lowpass", 7000), pre, 0.4)
    return trim(reverb(x, t60=2.8, damp=0.35, mix=0.28, seed=47))


def stinger_metal():
    """The montage's punctuation: kick, a palm-muted then ringing D5 power chord, a crash."""
    x = np.zeros((2, N(3.5)))
    place(x, P("kick", None, 0.4, 1.0, seed=51, freq=D2 * 0.75), 0)
    place(x, P("chug", D2, 0.12, 0.95, seed=52, mute=0.9), 0, 0.9)
    place(x, P("chug", D2, 1.4, 0.95, seed=53, mute=0.1, release=0.5), 0.15, 0.9)
    place(x, P("kick", None, 0.4, 0.9, seed=54, freq=D2 * 0.75), 0.15, 0.8)
    place(x, P("crash", None, 1.0, 0.9, seed=55, decay=2.4), 0.15, 0.6)
    return trim(reverb(x, t60=2.0, damp=0.35, mix=0.2, seed=56))


def stinger_reveal():
    """A cold shimmer for a revelation: glass and bell on D minor's top, fanned out."""
    x = np.zeros((2, N(4.0)))
    for k, (f, pan) in enumerate([(D5, -0.5), (A5, 0.4), (D6, -0.2), (D6 * 2 ** (3 / 12), 0.6)]):
        g = P("glass", f, 0.5, 0.75, seed=61 + k, decay=1.8)
        place(x, g * np.array([[1 - 0.5 * max(pan, 0)], [1 + 0.5 * min(pan, 0)]]), 0.035 * k, 0.6)
    place(x, P("bell", D5, 1.0, 0.6, seed=66, decay=3.0, index=2.5), 0, 0.35)
    place(x, P("strike", D4, 0.5, 0.5, seed=67, decay=1.5, bright=0.4), 0, 0.3)
    return trim(reverb(x, t60=3.0, damp=0.55, mix=0.4, seed=68))


def boom_sub():
    n = N(2.4)
    t = t_(n)
    f = D1 + (D2 * 1.25 - D1) * np.exp(-t / 0.18)
    y = np.sin(2 * np.pi * dsp.phase_ramp(f, n)) * dsp.perc_env(n, 0.002, 2.2, 0.8)
    y = dsp.softclip(y * 1.2, 1.8)
    x = dsp.stereo(y)
    place(x, P("kick", None, 0.3, 0.6, seed=71, freq=D2, click=0.5, decay=0.2), 0, 0.5)
    return trim(x)


# ---------------------------------------------------------------- risers

def riser(dur, seed=81):
    """Noise and a detuned saw cluster climbing two octaves from D2 to D4, a tremolo that
    speeds up, everything stopping dead at the end."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    t = t_(n)
    u = t / dur
    nz = pink(n, rng)
    fc = 200.0 * (7000.0 / 200.0) ** (u ** 1.3)
    noise = dsp.svf(nz, fc, 0.55, "bp") + 0.3 * dsp.svf(nz, fc * 0.5, 0.0, "lp")
    f = D2 * 4 ** (u ** 1.5)
    cl = np.zeros((2, n))
    for k, c in enumerate([-14, -5, 0, 6, 13]):
        s = dsp.saw(f * 2 ** (c / 1200) * (1, 1.5, 2, 1, 1.5)[k] , n, rng.uniform())
        ang = (np.linspace(-0.8, 0.8, 5)[k] + 1) * np.pi / 4
        cl += np.vstack([s * np.cos(ang), s * np.sin(ang)]) * np.sqrt(2) / np.sqrt(5)
    cl = dsp.svf(cl, 300 + 5000 * u ** 2, 0.25)
    trem_rate = 4 + 20 * u ** 2
    trem = 0.75 + 0.25 * np.sin(2 * np.pi * dsp.phase_ramp(trem_rate, n))
    y = (dsp.stereo(noise) * 0.9 + cl * 0.6) * trem * u ** 2.2
    return dsp.tail_fade(y, 0.003)


def riser_ticks(dur=2.0):
    """A clock speeding up from 4 ticks a second to 72 a second, where the ticks fuse into a
    buzz on D2: the game's tick rate as the score's tonic. Stops dead."""
    n = N(dur)
    u = t_(n) / dur
    glide = 0.75 * dur                       # 4 -> 72 ticks a second, then the D2 buzz holds
    x = dsp.fit(P("ticks", D2, glide, 0.9, seed=91, rate=4.0, shape=1.4, tone=D7, decay=0.008, accent=4), n)
    place(x, P("ticks", None, dur - glide, 0.9, seed=93, rate=D2, rate_end=D2, tone=D7, decay=0.008), glide)
    x += 0.25 * riser(dur, seed=92) * u
    return dsp.tail_fade(x, 0.003)


# ---------------------------------------------------------------- the two-pixel gag

def pixel_blip(f):
    """One pixel found: a square-wave blip, crushed to 4 bits."""
    n = N(0.14)
    t = t_(n)
    y = np.sign(np.sin(2 * np.pi * f * t)) * dsp.perc_env(n, 0.001, 0.12)
    y = y + 0.3 * np.sign(np.sin(2 * np.pi * 2 * f * t)) * dsp.perc_env(n, 0.001, 0.05)
    y = bitcrush(dsp.svf(y, 7000.0, 0.0) * 0.8, 4)
    return edge(dsp.stereo(y), 0.001)


def glitch(dur=0.7, seed=101):
    """Digital failure: a bright synth chord and noise sliced into 10-60 ms pieces, stuttered,
    reversed, pitched, crushed to 3-5 bits and down to 2-6 kHz sample rates, with dropouts."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    src_n = N(1.0)
    ts = t_(src_n)
    src = (0.5 * np.sign(np.sin(2 * np.pi * D6 * ts)) + 0.3 * dsp.saw(A5, src_n)
           + 0.3 * np.sin(2 * np.pi * D3 * ts) + 0.25 * rng.standard_normal(src_n))
    out = np.zeros((2, n))
    pos = 0
    while pos < n:
        ln = N(rng.uniform(0.012, 0.06))
        kind = rng.choice(["play", "stutter", "rev", "pitch", "gap", "crush"], p=[.2, .25, .1, .2, .1, .15])
        a = int(rng.integers(0, src_n - ln - 1))
        piece = src[a:a + ln].copy()
        if kind == "gap":
            pos += ln
            continue
        if kind == "rev":
            piece = piece[::-1].copy()
        if kind == "pitch":
            piece = sfxlib.varispeed(piece[None], float(rng.choice([0.5, 0.75, 1.5, 2.0])))[0]
        if kind == "crush":
            piece = sample_hold(bitcrush(piece, int(rng.integers(2, 4))), int(rng.integers(8, 24)))
        else:
            piece = sample_hold(bitcrush(piece, int(rng.integers(3, 6))), int(rng.integers(2, 10)))
        reps = int(rng.integers(2, 6)) if kind == "stutter" else 1
        pan = rng.uniform(-0.8, 0.8)
        gl, gr = dsp.pan_gains(pan)
        for _ in range(reps):
            m = min(piece.size, n - pos)
            if m <= 0:
                break
            p = edge(piece[:m].copy(), 0.0004)
            out[0, pos:pos + m] += gl * p
            out[1, pos:pos + m] += gr * p
            pos += m
    out *= np.clip(1.3 - t_(n) / dur, 0, 1)[None] ** 0.5
    return edge(out * 0.7, 0.001)


def glitch_two():
    """The gag's opener: two pixel blips, then the glitch."""
    x = np.zeros((2, N(1.1)))
    place(x, pixel_blip(D6), 0, 0.8)
    place(x, pixel_blip(A6), 0.16, 0.8)
    place(x, glitch(0.7, seed=102), 0.34)
    return x


# ---------------------------------------------------------------- the cut-off (scratch, tape stop)

def _bed(dur, seed=111):
    """Synth material to scratch: the score's D minor chord on brass and pad, a bass, a kick
    pattern at 120 bpm. Never a sample of a real record."""
    x = np.zeros((2, N(dur + 1)))
    place(x, P("brass", D3, dur, 0.85, seed=seed), 0, 0.8)
    place(x, P("brass", A3, dur, 0.8, seed=seed + 1), 0, 0.6)
    place(x, P("brass", F3 * 2, dur, 0.75, seed=seed + 2), 0, 0.5)
    place(x, P("bass", D2, dur, 0.9, seed=seed + 3), 0, 0.8)
    place(x, P("pad", D3, dur, 0.6, seed=seed + 4, attack=0.05, cutoff=1500), 0, 0.5)
    for k in range(int(dur * 2)):
        place(x, P("kick", None, 0.3, 0.9, seed=seed + 40 + k, freq=D2 * 0.75), 0.5 * k, 0.7)
        place(x, P("hit", None, 0.2, 0.7, seed=seed + 60 + k), 0.5 * k + 0.25, 0.4)
    return x


def _read_at(bed, pos):
    """Read the bed at fractional sample positions (linear interpolation)."""
    pos = np.clip(pos, 0, bed.shape[1] - 2)
    i = np.floor(pos).astype(int)
    f = pos - i
    return bed[:, i] * (1 - f) + bed[:, i + 1] * f


def scratch():
    """A scratch made from the synth: the bed plays, a hand drags it back, forward, back, and it
    stops. sync = the moment the hand lands (0.25 s in)."""
    rng = np.random.default_rng(121)
    bed = _bed(3.0)
    lead = 0.25
    # playback speed per sample: 1 until the hand lands, then the scratch strokes, then stop
    pts = [(0.0, 1.0), (lead, 1.0), (lead + 0.03, 0.0), (lead + 0.09, -2.6), (lead + 0.16, 0.0),
           (lead + 0.21, 2.2), (lead + 0.27, 0.0), (lead + 0.33, -1.8), (lead + 0.40, 0.0),
           (lead + 0.46, 0.9), (lead + 0.55, 0.0), (lead + 0.9, 0.0)]
    tt, vv = zip(*pts)
    n = N(pts[-1][0])
    v = np.interp(t_(n), tt, vv)
    pos = N(1.0) + np.cumsum(v)                      # start a second into the bed
    y = _read_at(bed, pos)
    after = t_(n) > lead
    # vinyl-like friction: noise whose level and colour follow the hand's speed
    fr = dsp.svf(rng.standard_normal(n), 900 + 1800 * np.abs(v), 0.4, "bp") * np.abs(v) * after * 0.35
    y = y + dsp.stereo(fr)
    # the needle's dull low-passing as the speed drops
    y = dsp.svf(y, 300 + 9000 * np.clip(np.abs(v), 0, 1.5) / 1.5, 0.0)
    y[:, t_(n) > lead + 0.6] = 0
    return dsp.tail_fade(edge(y, 0.002), 0.01), lead


def tapestop(dur=0.8):
    """The bed powered down: speed falls from 1 to 0 over `dur` (sync 0 = the moment it starts)."""
    bed = _bed(3.0, seed=131)
    lead = 0.3
    n = N(lead + dur + 0.1)
    t = t_(n)
    v = np.where(t < lead, 1.0, np.clip(1 - (t - lead) / dur, 0, 1) ** 1.6)
    y = _read_at(bed, N(1.0) + np.cumsum(v))
    y = dsp.svf(y, 200 + 12000 * v ** 1.5, 0.0)
    return dsp.tail_fade(edge(y, 0.002), 0.02), lead


# ---------------------------------------------------------------- UI

def ui_tick(f):
    return trim(P("tick", None, 0.05, 0.9, seed=141, tone=f, decay=0.01, click=0.6))


def ui_type():
    rng = np.random.default_rng(151)
    n = N(0.04)
    c = dsp.svf(rng.standard_normal(n), 4200.0, 0.5, "bp") * dsp.perc_env(n, 0.0002, 0.012)
    k = np.sin(2 * np.pi * D6 * t_(n)) * dsp.perc_env(n, 0.0002, 0.008) * 0.3
    return edge(dsp.stereo(c + k), 0.0005)


def ui_blip(fs, step=0.06):
    n = N(step * len(fs) + 0.1)
    y = np.zeros(n)
    for k, f in enumerate(fs):
        m = N(step + 0.05)
        tt = t_(m)
        s = np.sign(np.sin(2 * np.pi * f * tt)) * dsp.perc_env(m, 0.001, step + 0.04)
        a = N(step * k)
        y[a:a + m] += s[: n - a]
    y = bitcrush(dsp.svf(y, 5000.0, 0.1) * 0.7, 5)
    return edge(dsp.stereo(y), 0.001)


# ---------------------------------------------------------------- TORCHES

def fire(dur, intensity, seed=161, crackle_rate=28.0, roar=1.0, hiss=1.0, crackle=1.0):
    """A fire, after the classic recipe: hiss (high noise that flickers), crackles (sparse
    resonant pops), and the roar's lapping (low band-passed noise that moves slowly).
    `intensity` is an envelope (n,) or a number; everything follows it."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    I = np.broadcast_to(np.asarray(intensity, float), (n,))
    out = np.zeros((2, n))
    for ch in range(2):
        w = rng.standard_normal(n)
        flick = np.clip(lp_noise(n, 9.0, rng) * 0.5 + 0.6, 0, None) ** 3
        h = dsp.butter(w, "highpass", 2500.0, 2) * flick * 0.18
        lap_f = 180 * 2 ** (0.8 * lp_noise(n, 0.7, rng))
        lap = dsp.svf(pink(n, rng), lap_f, 0.55, "bp")
        lap *= np.clip(lp_noise(n, 3.0, rng) * 0.4 + 0.8, 0, None)
        rumble = dsp.butter(pink(n, rng), "lowpass", 110.0, 2) * 0.6
        out[ch] = hiss * h + roar * (0.55 * lap + rumble)
    # crackles: a Poisson stream of short resonant pops
    lam = crackle_rate * I / SR
    hits = np.nonzero(rng.random(n) < lam)[0]
    for a in hits:
        ln = N(rng.uniform(0.002, 0.012))
        m = min(ln, n - a)
        if m < 8:
            continue
        pop = dsp.svf(rng.standard_normal(m), rng.uniform(1200, 6500), 0.6, "bp")
        pop *= dsp.perc_env(m, 0.0001, ln * 0.8) * min(rng.pareto(2.5), 3.0) * 0.9
        gl, gr = dsp.pan_gains(rng.uniform(-0.7, 0.7))
        out[0, a:a + m] += gl * crackle * pop
        out[1, a:a + m] += gr * crackle * pop
    return out * I


def torch_ignite(seed=171, size=1.0):
    """A torch catching: a breath of air sucked in, a 'fwoomp' (the low-pass flung open, a low
    D2 that sags), then the fire settling. sync = the fwoomp's peak."""
    rng = np.random.default_rng(seed)
    peak = 0.14
    dur = 2.4
    n = N(dur)
    t = t_(n)
    a = np.where(t < peak, (t / peak) ** 2, np.exp(-(t - peak) / 0.35))
    cut = 150 + 4500 * np.where(t < peak, (t / peak) ** 1.5, np.exp(-(t - peak) / 0.25)) + 600 * (t > peak)
    nz = np.vstack([pink(n, rng), pink(n, rng)])
    fw = dsp.svf(nz, cut, 0.35) * a
    body = np.sin(2 * np.pi * dsp.phase_ramp(D2 * (1 - 0.3 * smoothstep((t - peak) / 0.5)), n))
    body = dsp.softclip(body * np.where(t < peak, (t / peak) ** 2, np.exp(-(t - peak) / 0.22)), 2.0)
    settle = np.clip((t - peak) / 0.15, 0, 1) * (0.25 + 0.75 * np.exp(-(t - peak).clip(0) / 0.9))
    f = fire(dur, settle * size, seed=seed + 1, crackle_rate=40)
    y = 1.1 * fw + 0.7 * dsp.stereo(body) + 0.9 * f
    y = reverb(dense(y, 2.0), t60=1.4, damp=0.35, mix=0.2, seed=seed + 2)
    return trim(dsp.tail_fade(dsp.fit(y, N(dur + 0.8)), 0.6)), peak


def torch_whoosh():
    """TORCHES: fire sucked backwards for a second, then the ignition, an impact on D1, a low
    open D power chord, and the fire roaring on. sync = 1.0 s (the hit)."""
    pre = 1.0
    dur = 4.6
    x = np.zeros((2, N(pre + dur)))
    # reversed fire: a burst of flame through a hall, played backwards
    burst = fire(1.2, dsp.perc_env(N(1.2), 0.005, 0.9), seed=181, crackle_rate=60)
    burst += 0.8 * dsp.svf(np.vstack([pink(N(1.2), np.random.default_rng(182)) for _ in range(2)]),
                           2500.0, 0.2) * dsp.perc_env(N(1.2), 0.002, 0.5)
    rv = reverb(burst, t60=1.1, damp=0.4, mix=0.75, predelay=0.0, seed=183)[:, :N(pre)]
    rv = rv[:, ::-1].copy() * (t_(N(pre)) / pre) ** 1.2
    place(x, dsp.tail_fade(rv, 0.002), 0, 1.0)
    ig, pk = torch_ignite(seed=184, size=1.4)
    place(x, ig, pre - pk, 1.1)
    place(x, P("impact", None, 1.0, 0.9, seed=185, freq=D1, decay=2.0, noise=0.9), pre, 0.8)
    place(x, P("chug", D2, 1.6, 0.8, seed=186, mute=0.05, release=0.8, level=0.35), pre, 0.6)
    roar_n = N(dur)
    env = np.clip(t_(roar_n) / 0.3, 0, 1) * np.exp(-t_(roar_n) / 2.2)
    place(x, fire(dur, env, seed=187, crackle_rate=50, roar=1.4), pre, 0.9)
    return trim(reverb(dense(x, 2.5), t60=2.2, damp=0.35, mix=0.18, seed=188)), pre


def torch_roar(dur=6.0):
    n = N(dur)
    t = t_(n)
    env = smoothstep(t / 0.8) * smoothstep((dur - t) / 1.2)
    return reverb(fire(dur, env, seed=191, crackle_rate=30, roar=1.2), t60=1.2, mix=0.15, seed=192)[:, :n]


def torch_crackle(dur=3.0):
    n = N(dur)
    t = t_(n)
    env = smoothstep(t / 0.2) * smoothstep((dur - t) / 0.6)
    return fire(dur, env, seed=195, crackle_rate=35, roar=0.15, hiss=0.6, crackle=1.3)


# ---------------------------------------------------------------- short, dry sounds in the same key

Eb6 = D6 * 2 ** (1 / 12)
A7, F7 = A6 * 2, D7 * 2 ** (3 / 12)


def glass_note(f, decay=0.4, vel=0.7, seed=0, partial=0.2):
    return P("glass", f, 0.1, vel, seed=seed, decay=decay, partial=partial)


def shimmer():
    x = np.zeros((2, N(1.0)))
    for k, f in enumerate([D6, A6, D7, F7]):
        place(x, glass_note(f, 0.7, 0.6 + 0.08 * k, seed=201 + k) * np.array([[1 - 0.3 * (k % 2)], [0.7 + 0.3 * (k % 2)]]),
              0.025 * k, 0.6)
    place(x, P("bell", D6, 0.5, 0.5, seed=205, decay=0.8, index=2.0), 0, 0.25)
    return trim(reverb(x, t60=0.6, damp=0.5, mix=0.12, seed=206))


def shimmer_rise(dur=2.5):
    x = np.zeros((2, N(dur + 0.6)))
    notes = [D5, F5 := D5 * 2 ** (3 / 12), A5, D6, D6 * 2 ** (3 / 12), A6, D7]
    k_n = 16
    u = 1 - (1 - np.linspace(0, 1, k_n)) ** 1.8           # accelerating towards the end
    for k, uk in enumerate(u):
        f = notes[min(len(notes) - 1, int(uk * len(notes)))]
        pan = -0.7 + 1.4 * uk
        g = glass_note(f, 0.35, 0.4 + 0.5 * uk, seed=211 + k)
        place(x, g * np.array([[1 - 0.5 * max(pan, 0)], [1 + 0.5 * min(pan, 0)]]), uk * (dur - 0.05), 0.5)
    n = N(dur)
    t = t_(n)
    nz = dsp.svf(pink(n, np.random.default_rng(219)), 800 * 10 ** (t / dur), 0.4, "bp") * (t / dur) ** 2 * 0.25
    place(x, dsp.stereo(nz), 0)
    y = x[:, :N(dur)]
    return dsp.tail_fade(reverb(y, t60=0.8, mix=0.15, seed=220)[:, :N(dur)], 0.004)


def clack():
    rng = np.random.default_rng(231)
    n = N(0.25)
    t = t_(n)
    y = (np.sin(2 * np.pi * D5 * t) * np.exp(-t / 0.018) + 0.6 * np.sin(2 * np.pi * D5 * 2.71 * t) * np.exp(-t / 0.010)
         + 0.4 * np.sin(2 * np.pi * D5 * 4.15 * t) * np.exp(-t / 0.006))
    cl = dsp.svf(rng.standard_normal(n), 2200.0, 0.4, "bp") * dsp.perc_env(n, 0.0002, 0.006)
    return trim(edge(dsp.stereo(0.7 * y + 0.8 * cl), 0.0005))


def stamp():
    x = np.zeros((2, N(0.5)))
    place(x, P("kick", None, 0.2, 0.6, seed=241, freq=D3, start=1.8, decay=0.09, click=0.3, drive=1.4), 0)
    place(x, P("tick", None, 0.05, 0.8, seed=242, tone=D6, decay=0.006, click=0.5), 0, 0.5)
    return trim(x)


def bit_tick(f=D7):
    n = N(0.06)
    t = t_(n)
    y = np.sign(np.sin(2 * np.pi * f * t)) * dsp.perc_env(n, 0.0005, 0.035)
    return edge(dsp.stereo(bitcrush(dsp.svf(y, 6000.0, 0.0) * 0.8, 4)), 0.0005)


def tick_scatter(dur=2.4, n_ticks=44):
    rng = np.random.default_rng(251)
    x = np.zeros((2, N(dur + 0.1)))
    times = np.sort(dur * rng.random(n_ticks) ** 2.2)          # dense first, thinning out
    for k, tk in enumerate(times):
        f = rng.choice([D7, A7, F7, D7 * 2])
        tk_x = P("tick", None, 0.03, 0.6, seed=252 + k, tone=f, decay=0.004, click=0.3)
        gl, gr = dsp.pan_gains(rng.uniform(-0.9, 0.9))
        place(x, tk_x * np.array([[gl], [gr]]), tk, (1 - tk / dur) * 0.6 + 0.2)
    return trim(x)


def whoosh_widen(dur=2.5, peak=1.6):
    """Air that opens out: a whoosh that starts in the middle and spreads to the full width."""
    y = whoosh(dur, peak, 250, 3500, pan0=0.0, pan1=0.0, metal=0.2, seed=261, rev=0.0)[:, :N(dur)]
    rng = np.random.default_rng(262)
    n = y.shape[1]
    t = t_(n)
    mono = y.mean(0)
    # decorrelate: a second take for the side signal, then the width opens 0 -> 1
    alt = dsp.svf(pink(n, rng), 300 + 3000 * bell_curve(n, peak, peak / 2, (dur - peak) / 2.4), 0.45, "bp")
    alt *= np.abs(mono).max() / (np.abs(alt).max() + 1e-9) * bell_curve(n, peak, peak / 2, (dur - peak) / 2.4) ** 1.3
    w = smoothstep(t / (peak + 0.3))
    out = np.vstack([mono + w * alt, mono - w * alt])
    return reverb(out, t60=1.2, mix=0.12, seed=263)


def lock():
    x = np.zeros((2, N(0.6)))
    place(x, P("clank", D6, 0.05, 0.6, seed=271, decay=0.08, ring=0.2, click=0.4), 0.0, 0.5)
    place(x, P("clank", D5, 0.08, 0.8, seed=272, decay=0.14, ring=0.2, click=0.5), 0.06, 0.8)
    place(x, P("kick", None, 0.2, 0.5, seed=273, freq=D2, start=2.0, decay=0.15, click=0.2, drive=1.3), 0.06, 0.6)
    return trim(x), 0.06


def click():
    rng = np.random.default_rng(281)
    n = N(0.05)
    y = np.zeros(n)
    for k, (a, f) in enumerate([(0, 3200.0), (N(0.007), 1400.0)]):
        m = N(0.02)
        b = dsp.svf(rng.standard_normal(m), f, 0.5, "bp") * dsp.perc_env(m, 0.0001, 0.006)
        y[a:a + m] += b[: n - a] * (1.0 if k == 0 else 0.7)
    return edge(dsp.stereo(y), 0.0005)


def ping(f):
    n = N(0.8)
    t = t_(n)
    idx = 1.2 * np.exp(-t / 0.05)
    y = np.sin(2 * np.pi * f * t + idx * np.sin(2 * np.pi * 2 * f * t))
    y += 0.25 * np.sin(2 * np.pi * 2 * f * t) * np.exp(-t / 0.12)
    y *= dsp.perc_env(n, 0.001, 0.7)
    return trim(edge(dsp.stereo(y), 0.0005))


def chalk_tap():
    rng = np.random.default_rng(291)
    n = N(0.08)
    t = t_(n)
    y = dsp.svf(rng.standard_normal(n), 2600.0, 0.6, "bp") * dsp.perc_env(n, 0.0003, 0.02)
    y += 0.5 * np.sin(2 * np.pi * 210 * t) * np.exp(-t / 0.012)
    return edge(dsp.stereo(y), 0.0005)


def slam():
    x = np.zeros((2, N(1.2)))
    place(x, P("impact", None, 0.5, 0.9, seed=301, freq=D2, decay=0.6, start=2.5, crack=0.3, noise=0.8), 0)
    place(x, P("clank", D4, 0.2, 0.8, seed=302, decay=0.25, click=0.3), 0, 0.6)
    return trim(reverb(dense(x, 2.0), t60=0.7, damp=0.4, mix=0.15, seed=303))


def zap():
    rng = np.random.default_rng(311)
    n = N(0.22)
    t = t_(n)
    fc = D4 + (D7 - D4) * np.exp(-t / 0.04)
    y = np.sin(2 * np.pi * dsp.phase_ramp(fc, n) + 3.0 * np.sin(2 * np.pi * dsp.phase_ramp(fc * 1.5, n)))
    y = 0.7 * y + 0.3 * dsp.svf(rng.standard_normal(n), 4000.0, 0.3, "bp")
    y = sample_hold(bitcrush(y * dsp.perc_env(n, 0.001, 0.2), 5), 3)
    return edge(dsp.stereo(y), 0.001)


def thunk():
    x = np.zeros((2, N(0.5)))
    place(x, P("kick", None, 0.2, 0.5, seed=321, freq=D2, start=1.6, decay=0.16, click=0.05, drive=1.2), 0)
    rng = np.random.default_rng(322)
    n = N(0.08)
    place(x, dsp.stereo(dsp.butter(rng.standard_normal(n), "lowpass", 600.0, 2) * dsp.perc_env(n, 0.001, 0.05)), 0, 0.4)
    return trim(x)


def fizz(dur=1.2):
    rng = np.random.default_rng(331)
    n = N(dur)
    t = t_(n)
    u = t / dur
    hiss = dsp.butter(rng.standard_normal((2, n)), "highpass", 3000.0, 2) * u ** 2 * 0.25
    out = hiss
    lam = (20 + 900 * u ** 2) / SR
    for a in np.nonzero(rng.random(n) < lam)[0]:
        m = min(N(0.003), n - a)
        if m < 8:
            continue
        pop = dsp.svf(rng.standard_normal(m), rng.uniform(2500, 8000), 0.5, "bp") * dsp.perc_env(m, 0.0001, 0.002)
        gl, gr = dsp.pan_gains(rng.uniform(-0.8, 0.8))
        out[0, a:a + m] += gl * pop * (0.3 + u[a])
        out[1, a:a + m] += gr * pop * (0.3 + u[a])
    return dsp.tail_fade(bitcrush(out * 0.8, 6), 0.003)


def chord_tick():
    x = np.zeros((2, N(0.8)))
    place(x, glass_note(D5, 0.5, 0.5, seed=341, partial=0.1), 0, 0.6)
    place(x, glass_note(A5, 0.5, 0.45, seed=342, partial=0.1), 0.012, 0.5)
    place(x, glass_note(D6 * 2 ** (3 / 12) / 2 * 2, 0.45, 0.35, seed=343, partial=0.1), 0.024, 0.35)
    return trim(x)


def squeeze(dur=1.5):
    rng = np.random.default_rng(351)
    n = N(dur)
    t = t_(n)
    u = t / dur
    src = np.vstack([pink(n, rng), pink(n, rng)]) * 0.5 + P("pad", D3, dur, 0.7, seed=352, attack=0.02, cutoff=3000)[:, :n]
    cut = 8000 * (300 / 8000) ** smoothstep(u)
    y = dsp.svf(src, cut, 0.35)
    m, sd = y.mean(0), (y[0] - y[1]) / 2 * (1 - smoothstep(u))   # the width closes as it narrows
    y = np.vstack([m + sd, m - sd]) * (np.clip(t / 0.05, 0, 1) * (1 - 0.6 * u))
    return dsp.tail_fade(y, 0.08)


def thud():
    x = np.zeros((2, N(0.5)))
    place(x, P("kick", None, 0.2, 0.6, seed=361, freq=D2 * 0.75, start=1.8, decay=0.25, click=0.05, drive=1.2), 0)
    return trim(dsp.butter(x, "lowpass", 500.0, 2))


def skate(dur=0.8):
    rng = np.random.default_rng(371)
    n = N(dur)
    t = t_(n)
    a = bell_curve(n, dur * 0.55, dur * 0.35, dur * 0.25)
    y = dsp.svf(rng.standard_normal(n), 3500 + 2500 * a, 0.5, "bp") * a ** 1.5
    y += 0.3 * dsp.svf(rng.standard_normal(n), 900 + 600 * a, 0.3, "bp") * a
    return dsp.tail_fade(pan_glide(y, -0.4, 0.4), 0.01)


def elec_tik():
    n = N(0.035)
    t = t_(n)
    y = dsp.svf(dsp.saw(D3, n), 2200.0, 0.5, "bp") * dsp.perc_env(n, 0.0005, 0.025)
    return edge(dsp.stereo(y), 0.0005)


def key_click():
    rng = np.random.default_rng(381)
    n = N(0.06)
    y = np.zeros(n)
    for a, f, g in [(0, 2500.0, 1.0), (N(0.018), 4500.0, 0.6)]:
        m = N(0.02)
        y[a:a + m] += (dsp.svf(rng.standard_normal(m), f, 0.55, "bp") * dsp.perc_env(m, 0.0001, 0.008) * g)[: n - a]
    return edge(dsp.stereo(y), 0.0005)


def strike_low():
    x = np.zeros((2, N(1.6)))
    place(x, P("strike", D3, 0.5, 0.8, seed=391, decay=1.2, bright=0.4, click=0.3), 0)
    place(x, P("kick", None, 0.2, 0.4, seed=392, freq=D2, decay=0.18, click=0.1), 0, 0.5)
    return trim(reverb(x, t60=0.9, mix=0.12, seed=393))


def glass_tick():
    return trim(glass_note(D7, 0.12, 0.7, seed=401, partial=0.1))


def grain(dur=3.0):
    """A faint crawl: tiny filtered grains, a few hundred a second, slowly breathing."""
    rng = np.random.default_rng(411)
    n = N(dur)
    t = t_(n)
    imp = (rng.random((2, n)) < 300 / SR) * rng.standard_normal((2, n))
    y = dsp.svf(imp, 3000.0, 0.6, "bp") * (0.6 + 0.4 * np.sin(2 * np.pi * 0.7 * t))
    return dsp.tail_fade(y * smoothstep(t / 0.3), 0.3)


# ---------------------------------------------------------------- v3

def slide(dur=2.9):
    """Two panels sliding together: a soft friction that swells and narrows into the meeting (sync = end)."""
    rng = np.random.default_rng(421)
    n = N(dur)
    t = t_(n)
    u = t / dur
    nz = pink(n, rng)
    y = dsp.svf(nz, 500 + 700 * u, 0.45, "bp") * (0.25 + 0.75 * smoothstep(u)) * np.clip(t / 0.3, 0, 1)
    y += 0.4 * dsp.svf(rng.standard_normal(n), 2500 + 1500 * u, 0.3, "bp") * smoothstep(u) ** 2
    st = np.vstack([y * (1 - 0.6 * u), y * (1 - 0.6 * u)])           # wide at first...
    side = dsp.svf(pink(n, rng), 700.0, 0.4, "bp") * (1 - u) * 0.5    # ...closing to the middle
    st[0] += side
    st[1] -= side
    return dsp.tail_fade(st, 0.004)


def clunk():
    x = np.zeros((2, N(0.9)))
    place(x, P("kick", None, 0.25, 0.8, seed=431, freq=D2, start=2.2, decay=0.22, click=0.3, drive=1.6), 0)
    place(x, P("clank", D4, 0.12, 0.7, seed=432, decay=0.18, ring=0.15, click=0.3), 0, 0.5)
    place(x, P("strike", D3, 0.3, 0.5, seed=433, decay=0.5, bright=0.3, click=0.2), 0, 0.35)
    return trim(reverb(dense(x, 1.8), t60=0.6, damp=0.4, mix=0.12, seed=434))


def burst(pre=0.45):
    """The box bursts to the full frame: a short suck in, a deep impact on D1, and a whoosh flung
    out to the full stereo width (sync = the impact)."""
    rng = np.random.default_rng(441)
    x = np.zeros((2, N(pre + 3.2)))
    place(x, whoosh_reverse(pre, seed=442), 0, 0.7)
    place(x, P("impact", None, 1.0, 1.0, seed=443, freq=D1, decay=2.6, noise=0.8, crack=0.3), pre)
    place(x, boom_sub(), pre, 0.7)
    n = N(2.4)
    t = t_(n)
    a = np.exp(-t / 0.55) * np.clip(t / 0.02, 0, 1)
    fc = 300 + 5000 * np.exp(-t / 0.35)
    l = dsp.svf(pink(n, rng), fc, 0.3, "bp") * a
    r = dsp.svf(pink(n, rng), fc * 1.07, 0.3, "bp") * a
    m, sd = (l + r) / 2, (l - r) / 2 * smoothstep(t / 0.35) * 2.0      # opens to full width in 0.35 s
    place(x, comb(np.vstack([m + sd, m - sd]), D3, 0.6, 0.2), pre, 1.1)
    return trim(reverb(x, t60=2.2, damp=0.35, mix=0.22, seed=444)), pre


def breath(dur=4.6):
    """A slow, uneven breath: soft air swelling and falling with the lightmap's swing."""
    rng = np.random.default_rng(451)
    n = N(dur)
    t = t_(n)
    swing = np.clip(0.55 + 0.45 * lp_noise(n, 0.9, rng), 0, 1.2)
    y = dsp.svf(pink(n, rng), 350 + 500 * swing, 0.35, "bp") * swing ** 1.5
    y *= smoothstep(t / 0.5) * smoothstep((dur - t) / 0.6)
    return reverb(pan_glide(y, -0.2, 0.2), t60=1.0, mix=0.15, seed=452)[:, :n]


# ---------------------------------------------------------------- v4: the big hits, for a phone speaker too
# A phone plays little below 300 Hz. These keep v3's low end and add the weight a phone can play: an open
# D3/A3 power chord through the score's cab, a bright crash, a saturated upper-mid flare.

def flare(dur, f_lo=700.0, f_hi=3200.0, decay=0.9, seed=461):
    """A bright, saturated noise flare: the upper-mid body of a hit, fast in, decaying."""
    rng = np.random.default_rng(seed)
    n = N(dur)
    t = t_(n)
    a = np.clip(t / 0.008, 0, 1) * np.exp(-t / decay)
    out = []
    for ch in range(2):
        nz = pink(n, rng)
        y = dsp.svf(nz, f_lo + (f_hi - f_lo) * np.exp(-t / (decay * 0.6)), 0.35, "bp") * a
        out.append(np.tanh(2.2 * y / (np.abs(y).max() + 1e-9)))
    return np.vstack(out) * a


def upper_stab(seed=471, vel=0.95, ring=1.6):
    """An open D power chord an octave above the score's bass, double-tracked, with a crash."""
    x = np.zeros((2, N(ring + 2.5)))
    place(x, P("chug", D3, ring, vel, seed=seed, mute=0.05, release=0.7, tone=5200.0, drive=18.0), 0, 0.9)
    place(x, P("chug", A3, ring, vel * 0.9, seed=seed + 1, mute=0.05, release=0.7, tone=5200.0, drive=18.0, fifth=0.0), 0, 0.45)
    place(x, P("crash", None, 1.0, 1.0, seed=seed + 2, decay=2.0, tone=1.15, noise=0.8), 0, 0.9)
    place(x, P("clank", D5, 0.2, 0.9, seed=seed + 3, decay=0.35, click=0.4), 0, 0.5)
    return x


def _at_level(y, ref_m, rel_db):
    """y scaled so its loudest 400 ms sits rel_db from ref_m (LUFS)."""
    return y * 10 ** ((ref_m + rel_db - sfxlib.momentary_max(y)) / 20)


def metal_hit(seed=521):
    x = np.zeros((2, N(2.0)))
    place(x, P("clank", D5, 0.2, 1.0, seed=seed, decay=0.5, ring=0.4, click=0.6), 0, 1.0)
    place(x, P("strike", A4, 0.5, 0.9, seed=seed + 1, decay=1.2, bright=0.8, click=0.6), 0, 0.7)
    place(x, P("crash", None, 1.0, 1.0, seed=seed + 2, decay=1.8, tone=1.15, noise=0.8), 0, 1.0)
    return x


def torch_whoosh_v4():
    """TORCHES for a phone too: v3's hit (its low end) under a flame's bright flare as loud as it, and
    dense crackle: most of what a phone can play of a fire sits between 600 Hz and 4 kHz."""
    x, pre = torch_whoosh()
    a = N(pre)
    m = sfxlib.momentary_max(x[:, a:a + N(0.8)])
    y = np.zeros((2, x.shape[1] + N(0.5)))
    place(y, x, 0, 10 ** (-2 / 20))
    place(y, _at_level(flare(2.4, 600.0, 3800.0, 0.9, seed=481), m, 0.0), pre)
    crk = fire(2.6, np.exp(-t_(N(2.6)) / 1.3), seed=495, crackle_rate=110, roar=0.0, hiss=1.4, crackle=2.0)
    place(y, _at_level(crk, m, -4.0), pre)
    place(y, _at_level(metal_hit(seed=531)[:, :N(1.5)], m, -8.0), pre)
    return trim(dense(y, 1.4)), pre


def burst_v4():
    """The burst for a phone too: v3's impact and whoosh (2 dB down) under a bright noise blast as loud
    as them, a metal clank and a crash."""
    x, pre = burst()
    a = N(pre)
    m = sfxlib.momentary_max(x[:, a:a + N(0.8)])
    y = np.zeros((2, x.shape[1] + N(0.5)))
    place(y, x, 0, 10 ** (-2 / 20))
    blast = flare(1.8, 900.0, 5500.0, 0.5, seed=511)
    bl = blast.mean(0)
    wide = np.vstack([bl + 0.8 * (blast[0] - blast[1]), bl - 0.8 * (blast[0] - blast[1])])  # flung wide
    place(y, _at_level(wide, m, 0.0), pre)
    place(y, _at_level(metal_hit(seed=541), m, -3.0), pre)
    return trim(dense(y, 1.4)), pre


def room_tone(dur=16.0):
    """A stone room's air: dark noise through a slowly wandering band, no pitch, no hum."""
    rng = np.random.default_rng(551)
    n = N(dur)
    t = t_(n)
    out = []
    for ch in range(2):
        y = dsp.svf(pink(n, rng), 260 * 2 ** (0.5 * lp_noise(n, 0.08, rng)), 0.3, "bp")
        y += 0.5 * dsp.butter(pink(n, rng), "lowpass", 180.0, 2)
        out.append(y)
    y = np.vstack(out) * smoothstep(t / 1.0) * smoothstep((dur - t) / 1.0)
    return dsp.butter(y, "highpass", 45.0, 2)


def switch_click():
    """The ladder's switch: a contact click and a short bright ring (D7, A7, D8 and a 2-6 kHz band), all of
    it inside 60 ms, so its energy above 1.5 kHz is high for its peak (a low crest, unlike a tick)."""
    rng = np.random.default_rng(561)
    n = N(0.09)
    t = t_(n)
    contact = dsp.butter(rng.standard_normal(n), "highpass", 1200.0, 2) * dsp.perc_env(n, 0.0002, 0.004)
    hold = np.clip((0.050 - t) / 0.012, 0, 1) ** 0.5                      # held through ~45 ms, then gone
    ring = sum(a * np.sin(2 * np.pi * f * t + rng.uniform(0, 6.28)) * np.exp(-t / d)
               for f, a, d in [(D7, 1.0, 0.060), (A6 * 2, 0.7, 0.045), (D7 * 2, 0.5, 0.035)]) * hold
    band = dsp.svf(rng.standard_normal(n), 3800.0, 0.35, "bp") * np.exp(-t / 0.040) * hold
    y = 0.9 * contact + 0.45 * ring + 0.8 * band
    y = np.tanh(2.4 * y / np.abs(y).max())
    return edge(np.vstack([y, y]), 0.0005)


# ---------------------------------------------------------------- v5: the climb, as heard

def burst_v5():
    """The burst, brighter: v3's low end 5 dB down, the bright blast 2 dB over it, the metal at -1."""
    x, pre = burst()
    a = N(pre)
    m = sfxlib.momentary_max(x[:, a:a + N(0.8)])
    y = np.zeros((2, x.shape[1] + N(0.5)))
    place(y, x, 0, 10 ** (-5 / 20))
    blast = flare(1.8, 1000.0, 5500.0, 0.55, seed=511)
    bl = blast.mean(0)
    wide = np.vstack([bl + 0.8 * (blast[0] - blast[1]), bl - 0.8 * (blast[0] - blast[1])])
    place(y, _at_level(wide, m, 2.0), pre)
    place(y, _at_level(metal_hit(seed=541), m, -1.0), pre)
    return trim(dense(y, 1.4)), pre


def hero_hit():
    """ALL ON's downbeat: on its first sample, no pre-roll. A brass stab on D (D4 A4 D5), a metal hit and a
    crash, a bright flare, and a low impact under them; its weight is 300 Hz-4 kHz, so a phone plays it."""
    n0 = N(3.0)
    x = np.zeros((2, n0))
    place(x, P("impact", None, 1.0, 0.9, seed=571, freq=D1, decay=1.6, crack=0.4), 0, 0.22)
    stab = np.zeros((2, n0))
    for k, f in enumerate([D4, A4, D5]):
        place(stab, P("brass", f, 0.55, 1.0, seed=572 + k, attack=0.004, env_attack=0.01, cutoff=900.0,
                      drive=2.2, release=0.5), 0)
    m = sfxlib.momentary_max(stab)
    place(x, stab, 0)
    place(x, _at_level(metal_hit(seed=581), m, -1.0), 0)
    place(x, _at_level(flare(1.2, 800.0, 4000.0, 0.45, seed=591), m, -2.0), 0)
    y = reverb(dense(x, 3.2), t60=1.4, damp=0.4, mix=0.15, seed=592)      # dense: a low crest, loud for its peak
    return trim(y)


# ---------------------------------------------------------------- v6

def burst_v6():
    """burst-v5 with 2.5 dB more between 300 Hz and 4 kHz (the phone's band), the rest as it was."""
    x, pre = burst_v5()
    band = signal.sosfiltfilt(signal.butter(2, [300.0, 4000.0], btype="bandpass", fs=SR, output="sos"), x, axis=1)
    y = x + (10 ** (2.5 / 20) - 1) * band
    return trim(dense(y, 2.0)), pre                      # denser: louder for its peak


# ---------------------------------------------------------------- v7 repair

def whoosh_narrow(dur=0.55, peak=0.2):
    """The box closing in: whoosh-widen-short's mirror. It starts at the full width and collapses to the middle in
    0.4 s, dry, and is gone by `dur` (it must clear the next word)."""
    rng = np.random.default_rng(601)
    y = whoosh(dur, peak, 250, 3500, pan0=0.0, pan1=0.0, metal=0.2, seed=602, rev=0.0)[:, :N(dur)]
    n = y.shape[1]
    t = t_(n)
    mono = y.mean(0)
    env = bell_curve(n, peak, peak / 2, (dur - peak) / 2.4)
    alt = dsp.svf(pink(n, rng), 300 + 3000 * env, 0.45, "bp")
    alt *= np.abs(mono).max() / (np.abs(alt).max() + 1e-9) * env ** 1.3
    w = 1 - smoothstep(t / 0.4)                               # wide -> narrow
    out = np.vstack([mono + w * alt, mono - w * alt])
    return dsp.tail_fade(out, 0.04)


# ---------------------------------------------------------------- the palette

def S(fn, sync=0.0, level=-12.0, kind="", desc=""):
    return dict(fn=fn, sync=sync, level=level, kind=kind, desc=desc)


PALETTE = {
    # whooshes (sync = the loudest instant, where a cut or wipe should sit)
    "whoosh-short": S(lambda: whoosh(0.5, 0.26, 350, 3200, seed=1), 0.26, -15, "whoosh",
                      "quick air past the camera, left to right, a faint D3 comb ring; for wipes and slides"),
    "whoosh-long": S(lambda: whoosh(1.5, 0.95, 220, 2600, pan0=-0.8, pan1=0.8, metal=0.3, tone=0.6, seed=2),
                     0.95, -14, "whoosh", "slow pass with a falling metallic Doppler tone; for long wipes, dissolves"),
    "whoosh-heavy": S(lambda: whoosh(1.7, 1.05, 90, 1100, pan0=-0.4, pan1=0.4, metal=0.35, metal_f=D2 * 2,
                                     sub=0.7, res=0.35, rev=0.25, seed=4), 1.05, -11, "whoosh",
                      "low, heavy pass with a sub falling D2->D1; chapter changes"),
    "whoosh-flick": S(lambda: whoosh(0.24, 0.09, 1200, 7000, pan0=-0.3, pan1=0.3, metal=0.15, metal_f=D5,
                                     rev=0.08, seed=5), 0.09, -19, "whoosh", "a tiny air flick; small captions, page turns"),
    "whoosh-reverse": S(lambda: whoosh_reverse(1.0), 1.0, -14, "whoosh",
                        "a whoosh sucked backwards, rising and stopping dead at its end (sync = end): into a cut"),
    "whoosh-reverse-short": S(lambda: whoosh_reverse(0.45, seed=6), 0.45, -16, "whoosh",
                              "the same, 0.45 s"),
    # hits and stingers (sync = the transient)
    "hit-caption": S(hit_caption, 0.0, -16, "hit", "metal clank on D5 over a short D2 thump; caption reveals"),
    "hit-soft": S(hit_soft, 0.0, -19, "hit", "a muted thump and a small struck bar; minor captions, labels"),
    "hit-heavy": S(hit_heavy, 0.0, -7, "hit", "impact on D1, struck bar on D3, dark crash, long hall; big beats"),
    "stinger-bumper": S(stinger_bumper, 0.8, -8, "stinger",
                        "reversed swell into an impact, open D power chord and a D-A bell fifth (sync = the hit at 0.8 s); chapter bumpers"),
    "stinger-metal": S(stinger_metal, 0.0, -7, "stinger",
                       "kick, chugged then ringing D power chord, crash; the montage's punctuation"),
    "stinger-reveal": S(stinger_reveal, 0.0, -15, "stinger", "cold glass-and-bell shimmer on D minor; a revelation"),
    "boom-sub": S(boom_sub, 0.0, -12, "hit", "sub drop to D1 (36 Hz) with a kick; weight under a cut"),
    # risers (sync = the end, where they stop dead)
    "riser-1s": S(lambda: riser(1.0, seed=82), 1.0, -13, "riser", "noise and saws climbing D2->D4, 1 s, stops dead"),
    "riser-2s": S(lambda: riser(2.0, seed=83), 2.0, -12, "riser", "the same, 2 s"),
    "riser-4s": S(lambda: riser(4.0, seed=84), 4.0, -12, "riser", "the same, 4 s"),
    "riser-ticks": S(lambda: riser_ticks(2.0), 2.0, -14, "riser",
                     "a clock speeding from 4 to 72 ticks a second by 1.5 s, where it fuses into a D2 buzz (72 Hz, the game's tick) that holds to a dead stop at 2.0 s"),
    # the two-pixel gag
    "pixel-blip-1": S(lambda: pixel_blip(D6), 0.0, -18, "gag", "pixel one found: a crushed square blip on D6"),
    "pixel-blip-2": S(lambda: pixel_blip(A6), 0.0, -18, "gag", "pixel two found: the same on A6"),
    "glitch": S(lambda: glitch(0.7), 0.0, -13, "gag", "digital failure: stutters, reversals, bit-crush, dropouts (0.7 s)"),
    "glitch-short": S(lambda: glitch(0.25, seed=103), 0.0, -14, "gag", "a 0.25 s glitch"),
    "glitch-two": S(glitch_two, 0.0, -13, "gag", "two pixel blips (0 and 0.16 s) then the glitch from 0.34 s"),
    "scratch": S(None, 0.25, -11, "gag",
                 "the cut-off: a synth bed (D minor brass, bass, kick) scratched back, forward, back, and stopped; "
                 "sync = the hand landing at 0.25 s; silence from 0.85 s"),
    "tapestop": S(None, 0.3, -12, "gag", "the same bed powered down over 0.8 s (sync = the moment it starts slowing)"),
    # UI
    "ui-tick": S(lambda: ui_tick(D7), 0.0, -24, "ui", "a high clock tick on D7"),
    "ui-tick-low": S(lambda: ui_tick(D6), 0.0, -24, "ui", "a tick on D6"),
    "ui-type": S(ui_type, 0.0, -28, "ui", "a dry type-on click, one per character or label"),
    "ui-blip": S(lambda: ui_blip([D6]), 0.0, -21, "ui", "a crushed square blip"),
    "ui-confirm": S(lambda: ui_blip([D6, A6]), 0.0, -21, "ui", "two blips up a fifth, D6 A6"),
    # TORCHES
    "torch-ignite": S(None, 0.14, -9, "torch",
                      "a torch catching: breath in, fwoomp (sync at its peak, 0.14 s), fire settling"),
    "torch-whoosh": S(None, 1.0, -7, "torch",
                      "TORCHES: a second of fire sucked backwards, then ignition, an impact on D1, an open D power chord, "
                      "the fire roaring on (sync = the hit at 1.0 s)"),
    "torch-roar": S(lambda: torch_roar(6.0), 0.0, -16, "torch", "6 s of fire (lapping, hiss, crackles), faded in and out; a bed"),
    "torch-crackle": S(lambda: torch_crackle(3.0), 0.0, -22, "torch", "3 s of crackles and hiss, little roar"),

    # short, dry sounds in the same key
    "shimmer": S(shimmer, 0.0, -24, "v2", "a bright glassy shimmer, D6 A6 D7 F7 fanned 25 ms apart (0.8 s); the Quad"),
    "shimmer-rise": S(lambda: shimmer_rise(2.5), 2.5, -24, "v2", "glass notes climbing D5->D7, faster and faster, to a stop at 2.5 s (sync = end)"),
    "clack": S(clack, 0.0, -24, "v2", "a soft wooden clack on D5"),
    "stamp": S(stamp, 0.0, -24, "v2", "a dull stamp: a short D3 knock and a tick"),
    "bit-tick": S(lambda: bit_tick(D7), 0.0, -26, "v2", "a 30 ms crushed square blip on D7: one bit"),
    "tick-scatter": S(tick_scatter, 0.0, -26, "v2", "tiny ticks scattered across the stereo field, dense then thinning out (2.4 s)"),
    "whoosh-widen": S(whoosh_widen, 1.6, -22, "v2", "air that starts in the middle and opens to the full width (2.5 s, peak 1.6)"),
    "lock": S(None, 0.06, -22, "v2", "a mechanical lock: a latch click, a heavier click and a low thunk (sync = the second click)"),
    "click": S(click, 0.0, -26, "v2", "a dry relay click"),
    "ping-1": S(lambda: ping(D6), 0.0, -20, "v2", "a bright ping on D6"),
    "ping-2": S(lambda: ping(Eb6), 0.0, -20, "v2", "a bright ping on Eb6, a semitone above ping-1"),
    "chalk-tap": S(chalk_tap, 0.0, -26, "v2", "a chalk tap on a board"),
    "slam": S(slam, 0.0, -20, "v2", "a short metal slam on D2/D4 (pitch= raises it)"),
    "zap": S(zap, 0.0, -22, "v2", "a 0.2 s electric zap falling D7->D4, crushed"),
    "thunk": S(thunk, 0.0, -24, "v2", "a soft felt thunk on D2"),
    "fizz": S(lambda: fizz(1.2), 1.2, -24, "v2", "a rising fizz of crackles and hiss, stopping at 1.2 s (sync = end)"),
    "chord-tick": S(chord_tick, 0.0, -26, "v2", "a soft glass chord tick, D5 A5 F6"),
    "squeeze": S(lambda: squeeze(1.5), 0.0, -24, "v2", "a squeeze: noise and a D3 pad, the filter closing 8 kHz->300 Hz and the width narrowing (1.5 s)"),
    "thud": S(thud, 0.0, -24, "v2", "a low soft thud on A1"),
    "skate": S(lambda: skate(0.8), 0.0, -30, "v2", "a blade gliding on ice, 0.8 s"),
    "elec-tik": S(elec_tik, 0.0, -30, "v2", "a soft electric tik: 25 ms of a D3 buzz"),
    "key-click": S(key_click, 0.0, -26, "v2", "a keycap going down"),
    "strike-low": S(strike_low, 0.0, -22, "v2", "a low struck bar on D3 with a soft kick"),
    "glass-tick": S(glass_tick, 0.0, -28, "v2", "a tiny glass tick on D7"),
    "grain": S(lambda: grain(3.0), 0.0, -36, "v2", "a faint crawling grain, 3 s"),
    # v3
    "slide": S(lambda: slide(2.9), 2.9, -28, "v3", "two panels sliding together, closing to the middle (2.9 s, sync = end)"),
    "clunk": S(clunk, 0.0, -22, "v3", "a heavy clunk: a D2 knock, a muted clank and a struck D3"),
    "burst": S(None, 0.45, -14, "v3", "the box bursting: a short suck in, a deep impact on D1, a whoosh flung out to full width (sync = the impact)"),
    "breath": S(lambda: breath(4.6), 0.0, -30, "v3", "a slow, uneven breath of air (4.6 s)"),
    # v4
    "torch-whoosh-v4": S(None, 1.0, -9, "v4", "TORCHES for a phone too: v3's torch-whoosh 3 dB down, plus a bright flame flare, an open D3/A3 power chord, a crash, dense crackle (sync = the hit at 1.0 s)"),
    "switch": S(switch_click, 0.0, -20, "v4", "the ladder's switch: a contact click and a bright 2-6 kHz ring inside 60 ms (low crest: its energy above 1.5 kHz is high for its peak)"),
    "burst-v5": S(None, 0.45, -12, "v5", "the burst, brighter: v3's low end 5 dB down, a bright blast 2 dB over it, metal (sync = the impact)"),
    "hero-hit": S(hero_hit, 0.0, -12, "v5", "ALL ON's downbeat on its first sample: a brass stab on D, metal, a crash, a bright flare, a low impact under them"),
    "burst-v6": S(None, 0.45, -12, "v6", "burst-v5 with +2.5 dB in 300 Hz-4 kHz, for a phone (sync = the impact)"),
    "whoosh-widen-short": S(lambda: whoosh_widen(0.8, 0.4), 0.0, -24, "v6", "a short widening whoosh, 0.8 s, peaking 0.4 s in: the box opening"),
    "whoosh-narrow-short": S(lambda: whoosh_narrow(0.55, 0.2), 0.0, -26, "v7", "the box closing in: a short whoosh from full width to the middle in 0.4 s, dry, gone by 0.55 s (whoosh-widen-short's mirror)"),
    "room-tone": S(lambda: room_tone(16.0), 0.0, -36, "v4", "a stone room's air: dark, unpitched, no hum (16 s; trim it with dur=)"),
    "burst-v4": S(None, 0.45, -12, "v4", "the burst for a phone too: v3's burst 3 dB down, plus an open D3/A3 power chord, a crash, a bright flare (sync = the impact)"),
}


def build(name: str) -> tuple[np.ndarray, float]:
    e = PALETTE[name]
    if name == "scratch":
        x, sync = scratch()
    elif name == "tapestop":
        x, sync = tapestop()
    elif name == "torch-ignite":
        x, sync = torch_ignite()
    elif name == "torch-whoosh":
        x, sync = torch_whoosh()
    elif name == "lock":
        x, sync = lock()
    elif name == "burst":
        x, sync = burst()
    elif name == "torch-whoosh-v4":
        x, sync = torch_whoosh_v4()
    elif name == "burst-v4":
        x, sync = burst_v4()
    elif name == "burst-v5":
        x, sync = burst_v5()
    elif name == "burst-v6":
        x, sync = burst_v6()
    else:
        x, sync = e["fn"](), e["sync"]
    x = dsp.dc_block(dsp.stereo(x), 15.0)
    return x, sync


def normalise(x: np.ndarray, level: float, ceiling: float = -1.0) -> tuple[np.ndarray, float, float]:
    """To `level` (momentary max, LUFS), then down if the true peak passes `ceiling`."""
    m = sfxlib.momentary_max(x)
    x = x * 10 ** ((level - m) / 20)
    tp = dsp.true_peak(x)
    if tp > ceiling + 6.0:                 # the limiter takes at most 6 dB; past that, turn it down
        x = x * 10 ** ((ceiling + 6.0 - tp) / 20)
    if dsp.true_peak(x) > ceiling:
        x, _ = dsp.limit(x, ceiling - 0.15, lookahead_ms=2.0, release_ms=60.0)
    tp = dsp.true_peak(x)
    if tp > ceiling:                       # clicks the limiter can't hold: a plain trim, always safe
        x = x * 10 ** ((ceiling - 0.05 - tp) / 20)
    return x, sfxlib.momentary_max(x), dsp.true_peak(x)


def main(names: list[str]) -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    idx_path = OUT / "index.json"
    index = {e["name"]: e for e in json.loads(idx_path.read_text())} if idx_path.exists() else {}
    for name in names or list(PALETTE):
        e = PALETTE[name]
        x, sync = build(name)
        x, m, tp = normalise(x, e["level"])
        dsp.write_wav24(OUT / f"{name}.wav", x, seed=zlib.crc32(name.encode()) & 0xFFFF)   # the same dither every run
        index[name] = {"name": name, "file": f"designed/{name}.wav", "kind": e["kind"],
                       "seconds": round(x.shape[1] / SR, 4), "sync": round(sync, 4),
                       "momentary_max_lufs": round(m, 1), "true_peak_dbtp": round(tp, 1), "desc": e["desc"]}
        print(f"{name:22s} {x.shape[1] / SR:6.2f} s  sync {sync:5.2f}  M {m:6.1f}  TP {tp:5.1f}", file=sys.stderr)
    order = list(PALETTE)
    idx_path.write_text(json.dumps([index[k] for k in order if k in index], indent=1) + "\n")


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("names", nargs="*", metavar="NAME", help=f"only these sounds (of: {', '.join(PALETTE)})")
    a = ap.parse_args()
    unknown = [n for n in a.names if n not in PALETTE]
    if unknown:
        ap.error(f"no designed sound {', '.join(unknown)}")
    main(a.names)
