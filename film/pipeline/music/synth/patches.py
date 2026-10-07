"""Instruments (patches).

A patch is a function  fn(f, dur, vel, p, rng) -> ndarray (2, n)  where
  f    frequency in Hz (None for an unpitched note: the patch uses its `freq` default),
  dur  gate length in seconds (how long the note is held),
  vel  velocity 0..1 (louder and, for most patches, brighter),
  p    parameters: the patch defaults, overridden by the track line, then the note,
  rng  a numpy Generator seeded from the note, so renders are reproducible.
Sample 0 of the returned array is the note's onset. Output may ring past `dur`.

Register with @patch(name, defaults, doc). Unknown parameter names are errors.
"""
from __future__ import annotations

import numpy as np
from scipy import signal

from dsp import (SR, adsr, amp_sim, butter, dc_block, ks_string, ladder, make_ir,
                 convolve_reverb, nsamp, onepole_lp, perc_env, phase_ramp, pulse, saw, secs,
                 sine, softclip, svf, tail_fade, vel_amp)

PATCHES: dict[str, tuple] = {}


def patch(name, defaults, doc, mono=False):
    """mono=True: fn(notes, n, p, rng) renders a whole track at once (legato voices);
    notes is a list of (t0, t1, freq, vel)."""
    def deco(fn):
        PATCHES[name] = (fn, dict(defaults), doc, mono)
        return fn
    return deco


def _pans(k, width, rng):
    if k == 1:
        return np.zeros(1)
    p = np.linspace(-width, width, k)
    rng.shuffle(p)
    return p


def _lr(p):
    ang = (np.clip(p, -1, 1) + 1.0) * np.pi / 4.0
    return np.cos(ang) * np.sqrt(2), np.sin(ang) * np.sqrt(2)


# ------------------------------------------------------------------ drone

@patch("drone", dict(freq=36.0, attack=4.0, release=5.0, drift=5.0, drift_rate=0.05,
                     beat=0.11, breathe=0.18, breathe_rate=0.06, grit=0.3, grit_cut=110.0,
                     grit_move=2.0, grit_rate=0.035, drive=1.3, level=0.8),
       "Sub drone: sine fundamental with slowly beating partners, octave, and a saw 'grit' "
       "layer under a low-pass that drifts over tens of seconds. Long notes.")
def drone(f, dur, vel, p, rng):
    f = f or p["freq"]
    env = adsr(dur, a=p["attack"], d=0.1, s=1.0, r=p["release"])
    n = env.size
    t = secs(n)
    ph = rng.uniform(0, 1, 6)
    drift = p["drift"] * (0.6 * np.sin(2 * np.pi * (p["drift_rate"] * t + ph[0]))
                          + 0.4 * np.sin(2 * np.pi * (p["drift_rate"] * 1.618 * t + ph[1])))
    fc = f * 2.0 ** (drift / 1200.0)
    fund = sine(fc, n)
    octave = 0.22 * sine(2.0 * fc, n, ph[2])
    pl = sine(fc + p["beat"], n, ph[3])
    pr = sine(fc - 0.83 * p["beat"], n, ph[4])
    cut = p["grit_cut"] * 2.0 ** (p["grit_move"] * 0.5 * (1 + np.sin(2 * np.pi * (p["grit_rate"] * t + ph[5]))))
    g_src = saw(fc * 1.0015, n, rng.uniform()) + 0.5 * saw(fc * 2.004, n, rng.uniform())
    g_l = svf(g_src, cut, 0.35)
    g_r = svf(saw(fc * 0.9985, n, rng.uniform()) + 0.5 * saw(fc * 1.996, n, rng.uniform()), cut * 1.07, 0.35)
    breathe = 1.0 + p["breathe"] * np.sin(2 * np.pi * (p["breathe_rate"] * t + rng.uniform()))
    left = 0.8 * fund + 0.35 * pl + octave + p["grit"] * g_l
    right = 0.8 * fund + 0.35 * pr + octave + p["grit"] * g_r
    out = softclip(np.vstack([left, right]) * 0.8, p["drive"])
    return out * env * breathe * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ air

@patch("air", dict(freq=500.0, attack=3.0, release=4.0, move=1.5, rate=0.05, res=0.55,
                   hp=70.0, level=0.5),
       "Dark air: noise through a slowly wandering band-pass (the pitch sets its centre). "
       "A corridor's breath under everything.")
def air(f, dur, vel, p, rng):
    f = f or p["freq"]
    env = adsr(dur, a=p["attack"], d=0.1, s=1.0, r=p["release"])
    n = env.size
    t = secs(n)
    noise = rng.standard_normal((2, n))
    out = []
    for ch in range(2):
        ph = rng.uniform(0, 1, 2)
        wander = 0.6 * np.sin(2 * np.pi * (p["rate"] * t + ph[0])) + 0.4 * np.sin(2 * np.pi * (p["rate"] * 2.37 * t + ph[1]))
        out.append(svf(noise[ch], f * 2.0 ** (p["move"] * 0.5 * wander), p["res"], "bp"))
    out = butter(np.vstack(out), "highpass", p["hp"])
    return out * env * vel_amp(vel) * p["level"] * 0.5


# ------------------------------------------------------------------ pad

@patch("pad", dict(voices=6, detune=14.0, attack=2.0, decay=1.0, sustain=1.0, release=3.0,
                   cutoff=500.0, cutoff_end=0.0, env_amt=0.0, lfo_rate=0.11, lfo_depth=0.7,
                   lfo2_rate=0.0, lfo2_depth=0.0, res=0.2, width=0.9, sub=0.0, drive=1.0,
                   keytrack=0.3, bend=0.0, bend_at=0.0, bend_len=1.0, level=0.5),
       "Detuned saw pad (several band-limited saws spread in stereo) through a resonant low-pass "
       "that moves: cutoff -> cutoff_end over the note, plus a slow LFO (lfo_depth in octaves) "
       "and an optional second one (lfo2_*). bend: cents reached bend_len s after bend_at s.")
def pad(f, dur, vel, p, rng):
    env = adsr(dur, a=p["attack"], d=p["decay"], s=p["sustain"], r=p["release"])
    n = env.size
    t = secs(n)
    k = int(p["voices"])
    cents = np.linspace(-0.5, 0.5, k) * p["detune"] + rng.normal(0, 1.0, k)
    pans = _pans(k, p["width"], rng)
    fb = f
    if p["bend"]:
        u = np.clip((t - p["bend_at"]) / max(p["bend_len"], 1e-3), 0, 1)
        fb = f * 2.0 ** (p["bend"] * (u * u * (3 - 2 * u)) / 1200.0)
    lr = np.zeros((2, n))
    for c, pn in zip(cents, pans):
        s = saw(fb * 2.0 ** (c / 1200.0), n, rng.uniform())
        gl, gr = _lr(pn)
        lr[0] += gl * s
        lr[1] += gr * s
    lr /= np.sqrt(k)
    c_end = p["cutoff_end"] or p["cutoff"]
    frac = np.clip(t / max(dur, 1e-3), 0, 1)
    base = p["cutoff"] * (c_end / p["cutoff"]) ** frac
    base = base * (f / 220.0) ** p["keytrack"] * (0.6 + 0.6 * vel) * 2.0 ** (p["env_amt"] * env)
    ph = rng.uniform()
    cut_l = base * 2.0 ** (p["lfo_depth"] * np.sin(2 * np.pi * (p["lfo_rate"] * t + ph)))
    cut_r = base * 2.0 ** (p["lfo_depth"] * np.sin(2 * np.pi * (p["lfo_rate"] * t + ph + 0.12)))
    if p["lfo2_depth"]:
        ph2 = rng.uniform()
        cut_l = cut_l * 2.0 ** (p["lfo2_depth"] * np.sin(2 * np.pi * (p["lfo2_rate"] * t + ph2)))
        cut_r = cut_r * 2.0 ** (p["lfo2_depth"] * np.sin(2 * np.pi * (p["lfo2_rate"] * t + ph2 + 0.12)))
    out = np.vstack([svf(lr[0], cut_l, p["res"]), svf(lr[1], cut_r, p["res"])])
    if p["sub"] > 0:
        out += p["sub"] * sine(f / 2.0, n)
    if p["drive"] > 1.0:
        out = softclip(out, p["drive"])
    return out * env * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ bells and metal

@patch("bell", dict(ratio=3.5, index=4.0, index_decay=0.8, decay=5.0, attack=0.002, hum=0.25,
                    detune=2.5, level=0.45),
       "FM bell: carrier with an inharmonic modulator (ratio) whose index decays, plus a hum "
       "partial an octave down. Rings for `decay` seconds whatever the note length.")
def bell(f, dur, vel, p, rng):
    n = nsamp(p["decay"])
    t = secs(n)
    idx = p["index"] * np.exp(-t / p["index_decay"]) * (0.5 + 0.5 * vel)
    env = perc_env(n, p["attack"], p["decay"])
    out = []
    for sgn in (-0.5, 0.5):
        fc = f * 2.0 ** (sgn * p["detune"] / 1200.0)
        ph = rng.uniform(0, 1, 2)
        mod = np.sin(2 * np.pi * (fc * p["ratio"] * t + ph[0]))
        car = np.sin(2 * np.pi * fc * t + idx * mod)
        hum = p["hum"] * np.sin(2 * np.pi * (0.5 * fc * t + ph[1])) * np.exp(-t / (0.5 * p["decay"]))
        out.append(car + hum)
    return np.vstack(out) * env * vel_amp(vel) * p["level"]


_BAR = np.array([1.0, 2.756, 5.404, 8.933, 13.344, 18.64, 24.81, 31.87])


@patch("strike", dict(freq=330.0, decay=1.6, bright=0.6, click=0.5, inharm=0.015, modes=8,
                      width=0.6, level=0.5),
       "Metallic strike (modal): the inharmonic modes of a struck bar, higher ones dying faster, "
       "with a noise click. Pitched: the note sets the lowest mode.")
def strike(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"])
    t = secs(n)
    out = np.zeros((2, n))
    m = int(p["modes"])
    ratios = _BAR[:m] * (1 + rng.normal(0, p["inharm"], m))
    for k, r in enumerate(ratios):
        fk = f * r
        if fk > 18000:
            break
        a = (p["bright"] * (0.6 + 0.5 * vel)) ** (0.8 * k) / (1 + 0.3 * k)
        d = p["decay"] / (1 + 0.7 * k)
        s = a * np.sin(2 * np.pi * (fk * t + rng.uniform())) * np.exp(-6.91 * t / d)
        gl, gr = _lr(rng.uniform(-p["width"], p["width"]))
        out[0] += gl * s
        out[1] += gr * s
    nc = nsamp(0.006)
    cl = rng.standard_normal((2, nc)) * perc_env(nc, 0.0001, 0.005)
    cl = svf(cl, min(max(f * 6, 2000), 12000), 0.3, "bp")
    out[:, :nc] += p["click"] * cl
    out *= perc_env(n, 0.0005, p["decay"] * 1.5)
    return out * vel_amp(vel) * p["level"]


_PLATE = np.array([1.0, 1.59, 2.14, 2.30, 2.65, 2.92, 3.16, 3.50, 3.60, 4.06, 4.83])


@patch("clank", dict(freq=420.0, decay=0.5, modes=7, ring=0.5, ring_ratio=1.37, click=0.7,
                     width=0.7, level=0.5),
       "Metal clank: a few plate modes (randomly chosen per note), ring-modulated for a harsh "
       "industrial edge, and a sharp click. Short.")
def clank(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"] * 1.4)
    t = secs(n)
    out = np.zeros((2, n))
    picks = np.sort(rng.choice(len(_PLATE), size=min(int(p["modes"]), len(_PLATE)), replace=False))
    for k, i in enumerate(picks):
        fk = f * _PLATE[i] * (1 + rng.normal(0, 0.01))
        d = p["decay"] * rng.uniform(0.4, 1.0) / (1 + 0.15 * k)
        s = np.sin(2 * np.pi * (fk * t + rng.uniform())) * np.exp(-6.91 * t / d) / (1 + 0.25 * k)
        gl, gr = _lr(rng.uniform(-p["width"], p["width"]))
        out[0] += gl * s
        out[1] += gr * s
    rm = 1.0 - p["ring"] + p["ring"] * np.sin(2 * np.pi * f * p["ring_ratio"] * t)
    out *= rm
    nc = nsamp(0.004)
    cl = svf(rng.standard_normal((2, nc)), 3500.0, 0.2, "bp") * perc_env(nc, 0.0001, 0.003)
    out[:, :nc] += p["click"] * 2.0 * cl
    return softclip(out * 0.6, 1.5) * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ bass

@patch("bass", dict(attack=0.004, decay=0.35, sustain=0.6, release=0.12, cutoff=180.0,
                    env_amt=2.5, env_decay=0.18, res=0.35, drive=3.5, bias=0.25, sub=0.6,
                    tone=4500.0, level=0.55),
       "Distorted bass: saw + square through a ladder low-pass with a plucky envelope "
       "(env_amt octaves, env_decay s), then asymmetric tanh drive and a tone low-pass.")
def bass(f, dur, vel, p, rng):
    env = adsr(dur, a=p["attack"], d=p["decay"], s=p["sustain"], r=p["release"])
    n = env.size
    t = secs(n)
    ph = rng.uniform()
    osc = 0.6 * saw(f, n, ph) + 0.4 * pulse(f * 1.002, n, ph, 0.5) + p["sub"] * sine(f, n)
    cut = p["cutoff"] * 2.0 ** (p["env_amt"] * np.exp(-t / p["env_decay"]) * (0.4 + 0.6 * vel))
    y = ladder(osc, cut, p["res"], 1.0)
    y = softclip(y * 0.8, p["drive"], p["bias"])
    y = butter(y, "lowpass", p["tone"], 2)
    y = dc_block(y, 20.0)
    y = y * env * vel_amp(vel) * p["level"]
    # low end stays mono; the top is delayed 0.4 ms on the right for a little width
    hi = butter(y, "highpass", 500.0, 2)
    d = nsamp(0.0004)
    hd = np.concatenate([np.zeros(d), hi[:-d]])
    return np.vstack([y, y - hi + hd])


# ------------------------------------------------------------------ percussion

@patch("impact", dict(freq=45.0, decay=2.2, start=4.0, drop=0.08, noise=0.6, noise_decay=0.4,
                      crack=0.5, drive=2.0, level=0.9),
       "Big low impact: a sine whose pitch falls from start*f to f, a noise burst whose low-pass "
       "closes, a crack on the transient, driven. The transient is at the note time.")
def impact(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"])
    t = secs(n)
    fcurve = f * (1.0 + (p["start"] - 1.0) * np.exp(-t / p["drop"]))
    body = np.sin(2 * np.pi * phase_ramp(fcurve, n)) * perc_env(n, 0.0004, p["decay"] * 0.7, 0.8)
    noise = rng.standard_normal((2, n))
    noise = svf(noise, 150.0 + 7000.0 * np.exp(-t / 0.06), 0.2) * perc_env(n, 0.0003, p["noise_decay"])
    nc = nsamp(0.003)
    crack = np.zeros((2, n))
    crack[:, :nc] = rng.standard_normal((2, nc)) * np.linspace(1, 0, nc)
    y = body + p["noise"] * noise + p["crack"] * crack
    y = softclip(y * 0.9, p["drive"])
    return dc_block(y, 15.0) * vel_amp(vel) * p["level"]


@patch("hit", dict(freq=1500.0, res=0.4, decay=0.25, sweep=1.0, sizzle=0.3, body=0.5,
                   body_freq=180.0, drive=2.0, level=0.55),
       "Filtered noise hit (industrial snare): band-passed noise whose centre falls `sweep` "
       "octaves, a short tonal body, sizzle above 5 kHz, driven.")
def hit(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"])
    t = secs(n)
    noise = rng.standard_normal((2, n))
    center = f * 2.0 ** (p["sweep"] * np.exp(-t / 0.03))
    bp = svf(noise, center, p["res"], "bp") * perc_env(n, 0.0003, p["decay"])
    sz = svf(noise, 5000.0, 0.0, "hp") * perc_env(n, 0.0003, p["decay"] * 0.5)
    body = sine(p["body_freq"] * (1 + np.exp(-t / 0.01)), n) * perc_env(n, 0.0004, 0.12)
    y = softclip(bp + p["sizzle"] * sz + p["body"] * body, p["drive"])
    return y * vel_amp(vel) * p["level"]


def _tick_kernel(tone, decay, click, rng):
    n = nsamp(decay * 1.6 + 0.003)
    k = np.sin(2 * np.pi * tone * secs(n)) * perc_env(n, 0.0001, decay)
    c = rng.standard_normal(n) * perc_env(n, 0.00005, 0.002)
    return k + click * butter(c, "highpass", 2000.0, 2)


@patch("tick", dict(tone=3200.0, decay=0.012, click=1.0, level=0.35),
       "A clock tick: a short high sine blip and a click. The pitch, if given, sets the tone.")
def tick(f, dur, vel, p, rng):
    k = _tick_kernel(f or p["tone"], p["decay"], p["click"], rng)
    return np.vstack([k, k]) * vel_amp(vel) * p["level"]


@patch("ticks", dict(rate=4.0, rate_end=0.0, shape=1.0, accent=0, tone=3200.0, decay=0.010,
                     click=0.8, level=0.3),
       "A train of ticks for the note's length, its rate gliding (exponentially) from `rate` to "
       "`rate_end` Hz. If the note is pitched and rate_end is 0, it glides to the note's pitch: "
       "fast enough, the ticks fuse into a buzz at that pitch (72 Hz = this score's tonic).")
def ticks(f, dur, vel, p, rng):
    n = nsamp(dur)
    t = secs(n)
    r0 = p["rate"]
    r1 = p["rate_end"] or (f if f else r0)
    rate = r0 * (r1 / r0) ** ((t / max(dur, 1e-3)) ** p["shape"])
    ph = phase_ramp(rate, n)
    idx = np.concatenate([[0], np.nonzero(np.diff(np.floor(ph)) > 0)[0] + 1])
    imp = np.zeros(n)
    amp = np.ones(idx.size)
    if p["accent"]:
        amp *= 0.55
        amp[:: int(p["accent"])] = 1.0
    imp[idx] = amp
    k = _tick_kernel(p["tone"], p["decay"], p["click"], rng)
    y = signal.fftconvolve(imp, k)
    return np.vstack([y, y]) * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ glass

@patch("glass", dict(ratio=4.0, index=1.4, index_decay=0.05, decay=1.2, attack=0.0015,
                     partial=0.2, detune=4.0, level=0.4),
       "Glassy tone for arpeggios: a sine lightly FM'd at ratio 4 (a bright, brief shimmer on "
       "the attack), an octave partial, slightly detuned between left and right.")
def glass(f, dur, vel, p, rng):
    n = nsamp(p["decay"])
    t = secs(n)
    idx = p["index"] * np.exp(-t / p["index_decay"]) * (0.4 + 0.6 * vel)
    out = []
    for sgn in (-0.5, 0.5):
        fc = f * 2.0 ** (sgn * p["detune"] / 1200.0)
        y = np.sin(2 * np.pi * fc * t + idx * np.sin(2 * np.pi * fc * p["ratio"] * t))
        y += p["partial"] * np.sin(2 * np.pi * 2.0 * fc * t) * np.exp(-t / (0.25 * p["decay"]))
        out.append(y)
    return np.vstack(out) * perc_env(n, p["attack"], p["decay"]) * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ swell

@patch("swell", dict(freq=220.0, source="mix", bright=4.0, curve=1.0, level=0.6),
       "Reversed swell: a bell-and-noise strike sent through a reverb, then played backwards, "
       "so it rises over the note's length and stops dead exactly at the note's END. "
       "source = mix | bell | noise.")
def swell(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(dur)
    t = secs(n)
    src = str(p["source"])
    dry = np.zeros((2, n))
    if src in ("mix", "bell"):
        idx = 5.0 * np.exp(-t / 0.5)
        b = np.sin(2 * np.pi * f * t + idx * np.sin(2 * np.pi * f * 3.5 * t))
        b += 0.5 * np.sin(2 * np.pi * 2 * f * t + idx * np.sin(2 * np.pi * f * 7.0 * t))
        dry += b * perc_env(n, 0.001, max(dur, 0.2))
    if src in ("mix", "noise"):
        nb = nsamp(min(0.4, dur))
        nz = svf(rng.standard_normal((2, nb)), min(f * p["bright"], 12000.0), 0.3, "bp")
        dry[:, :nb] += 1.5 * nz * perc_env(nb, 0.0005, 0.35)
    ir = make_ir(t60=max(0.3, 0.9 * dur), damp=0.5, predelay=0.0, early=4,
                 length=dur, seed=int(rng.integers(1 << 30)))
    wet = convolve_reverb(dry, ir)
    y = (0.3 * dry + wet)[:, ::-1].copy()
    y *= (t / max(dur, 1e-3)) ** p["curve"]
    y /= max(np.max(np.abs(y)), 1e-9)
    return tail_fade(y, 0.002) * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ brass

@patch("brass", dict(attack=0.04, decay=0.5, sustain=0.75, release=0.35, cutoff=500.0,
                     env_amt=2.2, env_attack=0.07, env_decay=0.6, res=0.15, voices=3,
                     detune=10.0, drive=1.6, vibrato=10.0, vib_rate=5.2, vib_delay=0.35,
                     keytrack=0.5, width=0.4, level=0.45),
       "Synth brass: detuned saws through a ladder low-pass whose envelope swells on the attack, "
       "a delayed vibrato, a little drive. The triumphant voice.")
def brass(f, dur, vel, p, rng):
    env = adsr(dur, a=p["attack"], d=p["decay"], s=p["sustain"], r=p["release"])
    n = env.size
    t = secs(n)
    fenv = adsr(dur, a=p["env_attack"], d=p["env_decay"], s=0.35, r=p["release"], n_total=n)
    vib = p["vibrato"] * np.sin(2 * np.pi * p["vib_rate"] * t) * np.clip((t - p["vib_delay"]) / 0.4, 0, 1)
    k = int(p["voices"])
    cents = np.linspace(-0.5, 0.5, k) * p["detune"]
    pans = _pans(k, p["width"], rng)
    lr = np.zeros((2, n))
    for c, pn in zip(cents, pans):
        s = saw(f * 2.0 ** ((c + vib) / 1200.0), n, rng.uniform())
        gl, gr = _lr(pn)
        lr[0] += gl * s
        lr[1] += gr * s
    lr /= np.sqrt(k)
    cut = p["cutoff"] * 2.0 ** (p["env_amt"] * fenv * (0.5 + 0.5 * vel)) * (f / 220.0) ** p["keytrack"]
    y = ladder(lr, cut, p["res"], p["drive"])
    return y * env * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ glide (legato, mono)

@patch("glide", dict(porta=0.06, shape="sine", attack=0.04, release=0.4, band=0.0,
                     band_level=0.7, index=1.2, ratio=3.5, cutoff=900.0, res=0.2, drive=1.0,
                     vibrato=0.0, vib_rate=5.0, level=0.4),
       "Legato voice: one oscillator for the whole track whose pitch glides from note to note "
       "(porta = glide time constant, s) and stays up between overlapping or touching notes. "
       "shape = sine | fm (bell-like, index/ratio) | saw (low-passed at cutoff, driven). "
       "band = cents of a soft detuned pair around it (a band of width +-band).", mono=True)
def glide(notes, n, p, rng):
    notes = sorted(notes)
    s_lo = max(0, int((notes[0][0] - 0.05) * SR))
    s_hi = min(n, int((max(nt[1] for nt in notes) + p["release"] * 1.5 + 0.1) * SR))
    m = s_hi - s_lo
    if m <= 0:
        return np.zeros((2, n))
    on = np.array([int(round(nt[0] * SR)) - s_lo for nt in notes])
    lf = np.array([np.log2(nt[2]) for nt in notes])
    idx = np.clip(np.searchsorted(on, np.arange(m), side="right") - 1, 0, len(notes) - 1)
    target = lf[idx]
    a = np.exp(-1.0 / (max(p["porta"], 1e-4) * SR))
    from scipy.signal import lfilter
    logf, _ = lfilter([1 - a], [1, -a], target, zi=[a * target[0]])
    amp = np.zeros(m)
    for (t0, t1, f, v) in notes:
        i0, i1 = max(0, int(round(t0 * SR)) - s_lo), max(0, int(round(t1 * SR)) - s_lo)
        amp[i0:i1] = np.maximum(amp[i0:i1], vel_amp(v))
    from dsp import follow
    amp = follow(amp, p["attack"], p["release"])
    t = secs(m) + s_lo / SR
    freq = 2.0 ** logf
    if p["vibrato"]:
        freq = freq * 2.0 ** (p["vibrato"] * np.sin(2 * np.pi * p["vib_rate"] * t) / 1200.0)

    def osc(fr):
        ph = phase_ramp(fr, m, rng.uniform())
        shape = str(p["shape"])
        if shape == "fm":
            return np.sin(2 * np.pi * ph + p["index"] * np.sin(2 * np.pi * p["ratio"] * ph))
        if shape == "saw":
            y = svf(saw(fr, m, rng.uniform()) + 0.5 * pulse(fr * 0.5, m), p["cutoff"], p["res"])
            return softclip(y, p["drive"]) if p["drive"] > 1 else y
        return np.sin(2 * np.pi * ph)
    y = osc(freq)
    out = np.vstack([y, y])
    if p["band"]:
        hi = osc(freq * 2.0 ** (p["band"] / 1200.0))
        lo = osc(freq * 2.0 ** (-p["band"] / 1200.0))
        out = out + p["band_level"] * np.vstack([0.8 * hi + 0.2 * lo, 0.2 * hi + 0.8 * lo])
        out /= 1.0 + p["band_level"]
    full = np.zeros((2, n))
    full[:, s_lo:s_hi] = out * amp * p["level"]
    return full


# ------------------------------------------------------------------ metal

@patch("chug", dict(fifth=1.0, octave=0.4, mute=0.75, drive=14.0, tone=3600.0, body=110.0,
                    scoop=650.0, attack=0.002, release=0.05, detune=7.0, tight=0.5, level=0.45),
       "Palm-muted power chord, double-tracked: root, fifth and octave saws, a pre-filter that "
       "closes fast (mute 0..1: 1 = a tight chug, 0 = an open ring), two-stage clipping, then a "
       "cabinet: a body bump, a mid scoop and a steep top cut. Left and right are separate takes.")
def chug(f, dur, vel, p, rng):
    mute = float(np.clip(p["mute"], 0, 1))
    env = adsr(dur, a=p["attack"], d=0.05 + 0.6 * (1 - mute), s=0.25 + 0.7 * (1 - mute),
               r=p["release"])
    n = env.size
    t = secs(n)
    out = []
    for _ in range(2):
        fr = f * 2.0 ** (rng.normal(0, p["detune"] / 2) / 1200.0)
        x = saw(fr, n, rng.uniform())
        x += p["fifth"] * saw(fr * 1.5 * 2.0 ** (rng.normal(0, 2.0) / 1200.0), n, rng.uniform())
        x += p["octave"] * saw(fr * 2.0 * 2.0 ** (rng.normal(0, 2.0) / 1200.0), n, rng.uniform())
        pre = 220.0 + (900.0 + 3000.0 * (1 - mute)) * np.exp(-t / (0.012 + 0.25 * (1 - mute)))
        x = svf(x, pre, 0.15)
        x = np.tanh(p["drive"] * (0.6 + 0.6 * vel) * x)
        x = np.tanh(2.5 * x + 0.1) - np.tanh(0.1)
        x = x + 0.8 * svf(x, p["body"], 0.5, "bp") - 0.5 * svf(x, p["scoop"], 0.3, "bp")
        x = butter(x, "lowpass", p["tone"], 4, zero_phase=False)
        x = butter(x, "highpass", 70.0, 2, zero_phase=False)
        out.append(x)
    y = np.vstack(out) * env * vel_amp(vel) * p["level"]
    return y


@patch("kick", dict(freq=52.0, start=3.2, drop=0.03, decay=0.38, click=0.7, drive=2.5, level=0.8),
       "Metal kick: a sine diving from start*f to f in a few tens of ms, a beater click, driven.")
def kick(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"] * 1.3)
    t = secs(n)
    body = np.sin(2 * np.pi * phase_ramp(f * (1 + (p["start"] - 1) * np.exp(-t / p["drop"])), n))
    body *= perc_env(n, 0.0003, p["decay"], 0.9)
    nc = nsamp(0.006)
    cl = svf(rng.standard_normal(nc), 3800.0, 0.4, "bp") * perc_env(nc, 0.00005, 0.004)
    body[:nc] += p["click"] * 3.0 * cl
    y = softclip(body, p["drive"])
    return np.vstack([y, y]) * vel_amp(vel) * p["level"]


_CYM = np.array([205.3, 304.4, 369.6, 522.7, 540.0, 800.0])


@patch("crash", dict(decay=2.4, tone=1.0, noise=0.6, width=0.8, level=0.35),
       "Crash cymbal: six inharmonic square waves (metal) and noise, band-limited high, a fast "
       "attack and a long decay. Each side its own set.")
def crash(f, dur, vel, p, rng):
    n = nsamp(p["decay"] * 1.2)
    t = secs(n)
    out = []
    for _ in range(2):
        fr = _CYM * p["tone"] * (1 + rng.normal(0, 0.03, _CYM.size))
        x = sum(np.sign(np.sin(2 * np.pi * (fk * t + rng.uniform()))) for fk in fr)
        x = x / 6.0 + p["noise"] * rng.standard_normal(n)
        x = butter(x, "highpass", 3500.0, 2, zero_phase=False)
        x = x + 0.5 * svf(x, 8000.0, 0.3, "bp")
        out.append(x * (perc_env(n, 0.0005, p["decay"]) + 0.6 * perc_env(n, 0.0002, 0.15)))
    m = 0.5 * (out[0] + out[1])
    sd = 0.5 * (out[0] - out[1]) * p["width"]
    return np.vstack([m + sd, m - sd]) * vel_amp(vel) * p["level"] * 0.5


# ================================================================== the second palette
# ------------------------------------------------------------------ guitar

_CHORDS = {"power": (0, 7, 12), "single": (0,), "fifth": (0, 7), "octave": (0, 12),
           "power5": (0, 7, 12, 19), "major": (0, 7, 12, 16), "minor": (0, 7, 12, 15),
           "sus": (0, 7, 12, 14)}


@patch("guitar", dict(chord="power", mute=0.0, decay=3.0, bright=0.7, pick=0.12, strum=0.004,
                      drive=22.0, tone=5200.0, body=0.6, scoop=0.5, presence=0.45, harm=0,
                      harm_level=0.5, vibrato=25.0, detune=5.0, double=1, slop=0.008, level=0.4),
       "Distorted guitar: plucked strings (Karplus-Strong) for each note of `chord` (power, fifth, "
       "octave, single, power5, major, minor, sus), strummed `strum` s apart, through an amp "
       "and cabinet (amp_sim). mute 0..1 palm-mutes (short, dark, chuggy). harm=K adds a pinch "
       "harmonic on the K-th partial, with vibrato. double=1 renders two takes, hard left and "
       "right, `slop` s apart and slightly detuned.", mono=False)
def guitar(f, dur, vel, p, rng):
    mute = float(np.clip(p["mute"], 0, 1))
    decay = p["decay"] * (1 - mute) + 0.14 * mute
    ring = max(dur, 0.05) + (0.06 if mute > 0.5 else min(decay, 1.5))
    n = nsamp(ring + 0.05)
    t = secs(n)
    intervals = _CHORDS.get(str(p["chord"]), (0, 7, 12))

    def take(cents):
        x = np.zeros(n)
        for i, iv in enumerate(intervals):
            fi = f * 2.0 ** ((iv * 100 + cents + rng.normal(0, 2.0)) / 1200.0)
            st = ks_string(fi, ring + 0.05, decay=decay, bright=p["bright"] * (1 - 0.55 * mute) * (0.6 + 0.4 * vel),
                           pick=p["pick"], damp=0.55 * mute, rng=rng)[:n]
            off = nsamp(i * p["strum"]) if i else 0
            x[off:] += st[: n - off] * (1.0 if i == 0 else 0.8)
        if p["harm"]:
            k = float(p["harm"])
            vib = p["vibrato"] * np.sin(2 * np.pi * 5.5 * t) * np.clip((t - 0.15) / 0.3, 0, 1)
            h = np.sin(2 * np.pi * phase_ramp(f * k * 2.0 ** ((cents + vib) / 1200.0), n))
            x = 0.35 * x + p["harm_level"] * h * perc_env(n, 0.004, max(decay, 1.0) * 1.5)
        # the player's hand: the note stops when it is released
        gate = np.ones(n)
        rel = nsamp(max(dur, 0.02))
        fade = nsamp(0.03 if mute > 0.5 else 0.08)
        gate[rel:] = 0.0
        gate[rel:rel + fade] = np.linspace(1, 0, min(fade, max(n - rel, 0)))
        x = x * gate
        y = amp_sim(x * (0.5 + 0.7 * vel), p["drive"], p["tone"], p["body"], p["scoop"], p["presence"])
        return y
    if int(p["double"]):
        l = take(-p["detune"] / 2)
        r = take(p["detune"] / 2)
        d = nsamp(abs(rng.normal(p["slop"], p["slop"] / 3)))
        r = np.concatenate([np.zeros(d), r[: n - d]])
        out = np.vstack([l, r])
    else:
        y = take(0.0)
        out = np.vstack([y, y])
    return tail_fade(out, 0.01) * vel_amp(vel, 1.0) * p["level"]


@patch("bassg", dict(mute=0.0, decay=1.6, bright=0.45, pick=0.2, sub=0.5, drive=2.0, tone=2600.0,
                     level=0.55),
       "Bass guitar: a plucked string (Karplus-Strong) with a pick, a sine sub under it, a little "
       "drive and a tone low-pass. Mono. mute palm-mutes it, so it locks with the guitar's chugs.")
def bassg(f, dur, vel, p, rng):
    mute = float(np.clip(p["mute"], 0, 1))
    decay = p["decay"] * (1 - mute) + 0.18 * mute
    ring = max(dur, 0.05) + (0.05 if mute > 0.5 else 0.25)
    n = nsamp(ring)
    st = ks_string(f, ring, decay=decay, bright=p["bright"] * (0.6 + 0.4 * vel), pick=p["pick"],
                   damp=0.5 * mute, rng=rng)[:n]
    env = adsr(max(dur, 0.02), a=0.004, d=0.3, s=0.85, r=0.05 if mute > 0.5 else 0.15, n_total=n)
    y = st + p["sub"] * np.sin(2 * np.pi * f * secs(n)) * env
    y = softclip(y * 0.8, p["drive"])
    y = butter(y, "lowpass", p["tone"], 2, zero_phase=False)
    y = dc_block(y, 25.0) * env
    return np.vstack([y, y]) * vel_amp(vel, 1.2) * p["level"]


# ------------------------------------------------------------------ drums

@patch("kick2", dict(freq=50.0, start=180.0, drop=0.035, decay=0.42, punch=0.6, click=0.6,
                     drive=1.8, level=0.8),
       "Layered kick: a sub body diving from `start` Hz to `freq`, a punch layer near 110 Hz, a "
       "beater click (filtered noise and an impulse), lightly saturated.")
def kick2(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"] * 1.4)
    t = secs(n)
    fr = f + (p["start"] - f) * np.exp(-t / p["drop"])
    body = np.sin(2 * np.pi * phase_ramp(fr, n)) * perc_env(n, 0.0003, p["decay"], 0.85)
    punch = np.sin(2 * np.pi * 112.0 * t) * perc_env(n, 0.0005, 0.07)
    nc = nsamp(0.008)
    cl = svf(rng.standard_normal(nc), 4200.0, 0.3, "bp") * perc_env(nc, 0.00005, 0.005)
    cl[:12] += np.linspace(1, 0, 12)
    y = body + p["punch"] * punch
    y[:nc] += p["click"] * 2.5 * cl * (0.5 + 0.5 * vel)
    y = softclip(y, p["drive"])
    return np.vstack([y, y]) * vel_amp(vel, 1.3) * p["level"]


@patch("snare2", dict(freq=185.0, body=0.7, noise=0.8, wires=0.5, decay=0.22, crack=0.6, drive=1.6,
                      level=0.6),
       "Snare: two shell tones with a little pitch drop, band-passed noise for the head, "
       "high wires with a longer tail, a crack on the transient, lightly driven, slightly wide.")
def snare2(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"] * 2.0)
    t = secs(n)
    drop = 1.0 + 0.25 * np.exp(-t / 0.012)
    shell = (np.sin(2 * np.pi * phase_ramp(f * drop, n)) * perc_env(n, 0.0004, 0.16)
             + 0.6 * np.sin(2 * np.pi * phase_ramp(f * 1.78 * drop, n)) * perc_env(n, 0.0004, 0.1))
    out = []
    for _ in range(2):
        nz = rng.standard_normal(n)
        head = butter(nz, "bandpass", [1200.0, 9000.0], 2, zero_phase=False) * perc_env(n, 0.0004, p["decay"])
        wires = butter(nz, "highpass", 4000.0, 2, zero_phase=False) * perc_env(n, 0.002, p["decay"] * 1.6)
        y = p["body"] * shell + p["noise"] * head + p["wires"] * wires
        nc = nsamp(0.002)
        y[:nc] += p["crack"] * 2.0 * rng.standard_normal(nc)
        out.append(softclip(y * (0.6 + 0.5 * vel), p["drive"]))
    m, sd = 0.5 * (out[0] + out[1]), 0.5 * (out[0] - out[1]) * 0.4
    return np.vstack([m + sd, m - sd]) * vel_amp(vel, 1.3) * p["level"]


_HAT = np.array([263.0, 400.0, 421.0, 474.0, 587.0, 845.0])


@patch("hats", dict(open=0.0, decay=0.06, tone=1.0, noise=0.35, level=0.35),
       "Hi-hat: six inharmonic square waves (metal) and noise, high-passed. open 0..1 lengthens "
       "it from a tight chick to an open wash.")
def hats(f, dur, vel, p, rng):
    dec = p["decay"] + p["open"] * 0.5
    n = nsamp(dec * 1.5 + 0.01)
    t = secs(n)
    fr = _HAT * p["tone"] * 1.6 * (1 + rng.normal(0, 0.01, _HAT.size))
    x = sum(np.sign(np.sin(2 * np.pi * (fk * t + rng.uniform()))) for fk in fr) / 6.0
    x = x + p["noise"] * rng.standard_normal(n)
    x = butter(x, "highpass", 6500.0, 4, zero_phase=False)
    x = x + 0.6 * svf(x, 10000.0, 0.4, "bp")
    y = x * perc_env(n, 0.0003, dec)
    return np.vstack([y, 0.9 * y]) * vel_amp(vel, 1.3) * p["level"]


@patch("tom", dict(freq=110.0, decay=0.6, drop=0.06, mallet=0.5, level=0.6),
       "Tom: a membrane (a falling fundamental and two higher modes) and a mallet's noise.")
def tom(f, dur, vel, p, rng):
    f = f or p["freq"]
    n = nsamp(p["decay"] * 1.3)
    t = secs(n)
    dr = 1.0 + 0.35 * np.exp(-t / p["drop"])
    y = np.sin(2 * np.pi * phase_ramp(f * dr, n)) * perc_env(n, 0.0005, p["decay"])
    y += 0.35 * np.sin(2 * np.pi * phase_ramp(f * 1.59 * dr, n)) * perc_env(n, 0.0005, p["decay"] * 0.5)
    y += 0.2 * np.sin(2 * np.pi * phase_ramp(f * 2.14 * dr, n)) * perc_env(n, 0.0005, p["decay"] * 0.3)
    nm = nsamp(0.02)
    y[:nm] += p["mallet"] * svf(rng.standard_normal(nm), 1500.0, 0.2, "bp") * perc_env(nm, 0.0001, 0.015)
    y = softclip(y, 1.4)
    return np.vstack([y, y]) * vel_amp(vel, 1.3) * p["level"]


_TIMP = np.array([1.0, 1.504, 1.742, 2.0, 2.245, 2.494])


@patch("timp", dict(decay=2.4, mallet=0.4, level=0.6),
       "Timpani: the modes of a kettle drum (pitched by the note), each with its own decay, "
       "a slight pitch settle on the stroke, and a felt mallet.")
def timp(f, dur, vel, p, rng):
    f = f or 73.4
    n = nsamp(p["decay"] * 1.2)
    t = secs(n)
    settle = 1.0 + 0.02 * np.exp(-t / 0.06)
    y = np.zeros(n)
    amps = [1.0, 0.5, 0.35, 0.25, 0.18, 0.12]
    for k, (r, a) in enumerate(zip(_TIMP, amps)):
        y += a * np.sin(2 * np.pi * phase_ramp(f * r * settle, n)) * perc_env(n, 0.002, p["decay"] / (1 + 0.6 * k))
    nm = nsamp(0.03)
    y[:nm] += p["mallet"] * onepole_lp(rng.standard_normal(nm), 900.0) * perc_env(nm, 0.0005, 0.02)
    return np.vstack([y, y]) * vel_amp(vel, 1.2) * p["level"]


# ------------------------------------------------------------------ heroic and wry

@patch("stab", dict(voices=5, detune=12.0, cutoff=900.0, env_amt=3.0, decay=0.7, boom=0.6,
                    noise=0.4, bell=0.3, level=0.5),
       "Heroic stab (an orchestral hit): a brassy saw stack whose filter snaps open, a noise "
       "burst, a low boom and a bell partial, all short and loud. Play chords with it.")
def stab(f, dur, vel, p, rng):
    n = nsamp(max(dur, 0.1) + p["decay"])
    t = secs(n)
    env = adsr(max(dur, 0.1), a=0.004, d=p["decay"] * 0.5, s=0.35, r=p["decay"], n_total=n)
    k = int(p["voices"])
    lr = np.zeros((2, n))
    for c, pn in zip(np.linspace(-0.5, 0.5, k) * p["detune"], np.linspace(-0.8, 0.8, k)):
        s_ = saw(f * 2.0 ** (c / 1200.0), n, rng.uniform())
        gl, gr = _lr(pn)
        lr[0] += gl * s_
        lr[1] += gr * s_
    lr /= np.sqrt(k)
    cut = p["cutoff"] * 2.0 ** (p["env_amt"] * np.exp(-t / 0.15) * (0.5 + 0.5 * vel))
    y = ladder(lr, cut, 0.2, 1.6) * env
    nb = nsamp(0.08)
    y[:, :nb] += p["noise"] * svf(rng.standard_normal((2, nb)), 3000.0, 0.2, "bp") * perc_env(nb, 0.0005, 0.07)
    y += p["boom"] * np.sin(2 * np.pi * phase_ramp(55.0 * (1 + 2 * np.exp(-t / 0.04)), n)) * perc_env(n, 0.001, 0.6)
    y += p["bell"] * np.sin(2 * np.pi * 4 * f * t + 2 * np.exp(-t / 0.2) * np.sin(2 * np.pi * 14 * f * t)) * perc_env(n, 0.001, 1.2)
    return y * vel_amp(vel, 1.2) * p["level"]


@patch("pluck", dict(decay=0.6, bright=0.35, body=0.5, width=0.4, level=0.45),
       "A dry pizzicato: a short soft plucked string with a little wooden body. For wry figures.")
def pluck(f, dur, vel, p, rng):
    n = nsamp(p["decay"] * 1.2 + 0.05)
    y = ks_string(f, n / SR, decay=p["decay"], bright=p["bright"] * (0.6 + 0.4 * vel), pick=0.2, rng=rng)
    y = y + p["body"] * (svf(y, 220.0, 0.6, "bp") + 0.6 * svf(y, 1100.0, 0.4, "bp"))
    y = butter(y, "highpass", 80.0, 2, zero_phase=False)
    y = tail_fade(y.copy(), 0.01)
    d = nsamp(0.0006)
    r = np.concatenate([np.zeros(d), y[:-d]])
    return np.vstack([y, (1 - p["width"]) * y + p["width"] * r]) * vel_amp(vel, 1.2) * p["level"]


@patch("slide", dict(to=-12.0, shape=2.0, breath=0.25, vibrato=15.0, timbre=0.4, level=0.45),
       "A comic slide: a whistle-like tone that glides from the note by `to` semitones over the "
       "note (shape > 1 falls late, like a deflating 'bwomp'), with a little breath.")
def slide(f, dur, vel, p, rng):
    n = nsamp(max(dur, 0.05) + 0.08)
    t = secs(n)
    u = np.clip(t / max(dur, 0.05), 0, 1) ** p["shape"]
    fr = f * 2.0 ** (p["to"] * u / 12.0) * 2.0 ** (p["vibrato"] * np.sin(2 * np.pi * 6 * t) / 1200.0)
    ph = phase_ramp(fr, n)
    y = np.sin(2 * np.pi * ph) + p["timbre"] * np.sin(4 * np.pi * ph) + 0.5 * p["timbre"] * np.sin(6 * np.pi * ph)
    br = svf(rng.standard_normal(n), fr * 2.0, 0.6, "bp") * p["breath"]
    env = adsr(max(dur, 0.05), a=0.02, d=0.1, s=0.9, r=0.06, n_total=n)
    y = (y + br) * env
    return np.vstack([y, y]) * vel_amp(vel) * p["level"]


# ------------------------------------------------------------------ dark ambient, evolving

@patch("drone2", dict(partials=24, tilt=1.1, stretch=0.0008, evolve=0.07, formant=1.0,
                      f_lo=180.0, f_hi=1400.0, f_rate=0.03, detune=6.0, sub=0.5, attack=4.0,
                      release=5.0, level=0.5),
       "Evolving drone: a stack of partials (slightly stretched, like a big string or a metal "
       "bar) whose amplitudes drift on slow random walks (evolve, Hz), a formant that wanders "
       "between f_lo and f_hi, a detuned twin for width, and a sub. It never sits still.")
def drone2(f, dur, vel, p, rng):
    env = adsr(dur, a=p["attack"], d=0.1, s=1.0, r=p["release"])
    n = env.size
    t = secs(n)
    step = 480
    m = n // step + 2
    tc = np.arange(m) * step / SR

    def walk(rate):
        w = np.cumsum(rng.normal(0, 1, m))
        w = onepole_lp(w - w.mean(), rate * 2.0)
        w = (w - w.min()) / max(np.ptp(w), 1e-9)
        return np.interp(t, tc, w)
    fpos = p["f_lo"] * (p["f_hi"] / p["f_lo"]) ** walk(p["f_rate"])
    out = np.zeros((2, n))
    for ch, cents in ((0, -p["detune"] / 2), (1, p["detune"] / 2)):
        fc = f * 2.0 ** (cents / 1200.0)
        for k in range(1, int(p["partials"]) + 1):
            fk = fc * k * (1 + p["stretch"] * k * k)
            if fk > 12000:
                break
            base = k ** -p["tilt"]
            form = 1.0 + 2.5 * p["formant"] * np.exp(-0.5 * (np.log2(fk / fpos) / 0.45) ** 2)
            a = base * (0.25 + 0.75 * walk(p["evolve"])) * form
            out[ch] += a * np.sin(2 * np.pi * (fk * t + rng.uniform()))
    out /= np.abs(out).max() + 1e-9
    out += p["sub"] * np.sin(2 * np.pi * f * 0.5 * t)
    return out * env * vel_amp(vel) * p["level"]


@patch("texture", dict(density=18.0, grain=0.09, notes="0,3,7,10,14", octaves=2, source="glass",
                       cutoff=2500.0, spread=1.0, attack=2.0, release=3.0, level=0.4),
       "Granular texture: a cloud of short grains drawn from `notes` (semitones above the pitch, "
       "over `octaves`), at `density` grains a second, each its own place in the stereo field. "
       "source = glass (sine with a glint), saw (filtered) or noise (band-passed).")
def texture(f, dur, vel, p, rng):
    env = adsr(dur, a=p["attack"], d=0.1, s=1.0, r=p["release"])
    n = env.size
    out = np.zeros((2, n))
    notes = [float(x) for x in str(p["notes"]).split(",") if x != ""]
    count = int(p["density"] * n / SR)
    for _ in range(count):
        g = nsamp(p["grain"] * rng.uniform(0.6, 1.6))
        s0 = int(rng.uniform(0, max(n - g, 1)))
        semi = rng.choice(notes) + 12 * int(rng.integers(0, int(p["octaves"])))
        fg = f * 2.0 ** (semi / 12.0) * 2.0 ** (rng.normal(0, 5) / 1200.0)
        tg = secs(g)
        w = np.sin(np.pi * np.arange(g) / g) ** 2
        src = str(p["source"])
        if src == "saw":
            y = svf(saw(fg, g, rng.uniform()), p["cutoff"], 0.2)
        elif src == "noise":
            y = svf(rng.standard_normal(g), fg, 0.85, "bp")
        else:
            y = np.sin(2 * np.pi * fg * tg + 0.6 * np.sin(2 * np.pi * 3.0 * fg * tg))
        gl, gr = _lr(rng.uniform(-p["spread"], p["spread"]))
        out[0, s0:s0 + g] += gl * y * w
        out[1, s0:s0 + g] += gr * y * w
    out /= np.sqrt(max(p["density"] * p["grain"], 1.0))
    return out * env * vel_amp(vel) * p["level"]


@patch("organ", dict(bars="8,6,8,4,0,3,0,2,2", click=0.3, leslie=5.8, leslie_depth=0.35, drive=1.3,
                     attack=0.01, release=0.15, level=0.35),
       "Drawbar organ: sine ranks at 16', 5 1/3', 8', 4', 2 2/3', 2', 1 3/5', 1 1/3', 1' "
       "(levels 0..8 in `bars`), a key click, a rotating-speaker tremolo, a little drive. "
       "Pompous on a big chord.")
def organ(f, dur, vel, p, rng):
    env = adsr(max(dur, 0.02), a=p["attack"], d=0.05, s=1.0, r=p["release"])
    n = env.size
    t = secs(n)
    ratios = [0.5, 1.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0]
    bars = [float(x) for x in str(p["bars"]).split(",")]
    out = np.zeros((2, n))
    rot = 2 * np.pi * p["leslie"] * t + rng.uniform(0, 6.28)
    for r, b in zip(ratios, bars):
        if b <= 0 or f * r > 15000:
            continue
        y = (b / 8.0) * np.sin(2 * np.pi * (f * r * t + rng.uniform()))
        out[0] += y * (1 + p["leslie_depth"] * np.sin(rot + r))
        out[1] += y * (1 + p["leslie_depth"] * np.sin(rot + r + 1.7))
    nc = nsamp(0.006)
    out[:, :nc] += p["click"] * rng.standard_normal((2, nc)) * perc_env(nc, 0.0001, 0.004)
    out = softclip(out / 3.0, p["drive"])
    return out * env * vel_amp(vel) * p["level"]


def describe():
    lines = []
    for name, (_, d, doc, _mono) in PATCHES.items():
        lines.append(f"{name}: {doc}\n    params: " + ", ".join(f"{k}={v}" for k, v in d.items()))
    return "\n".join(lines)


# ------------------------------------------------------------------ guitar2
import guitar2  # noqa: E402,F401  (registers the `guitar2` patch; it lives in its own file)
