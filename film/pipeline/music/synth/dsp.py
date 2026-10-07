"""DSP core: oscillators, envelopes, filters, reverb, delay, loudness, limiter, WAV I/O.

Everything runs at SR = 48 kHz in float64. Per-sample loops (filters with moving
cutoff, envelope followers, the limiter) are numba kernels, cached on first use.
"""
from __future__ import annotations

import subprocess
import struct

import numpy as np
from numba import njit
from scipy import signal

SR = 48000
TAU = 2.0 * np.pi


# ---------------------------------------------------------------- helpers

def db(x):
    return 10.0 ** (np.asarray(x, dtype=float) / 20.0)


def to_db(x, floor=-200.0):
    x = np.maximum(np.abs(np.asarray(x, dtype=float)), 10.0 ** (floor / 20.0))
    return 20.0 * np.log10(x)


def secs(n):
    return np.arange(n) / SR


def nsamp(seconds):
    return max(1, int(round(seconds * SR)))


def stereo(x):
    """Mono (n,) -> (2, n); stereo passes through."""
    x = np.asarray(x, dtype=float)
    return np.vstack([x, x]) if x.ndim == 1 else x


def pan_gains(p):
    """Constant-power pan, normalised so centre = (1, 1)."""
    p = float(np.clip(p, -1.0, 1.0))
    ang = (p + 1.0) * np.pi / 4.0
    return np.sqrt(2.0) * np.cos(ang), np.sqrt(2.0) * np.sin(ang)


def vel_amp(vel, curve=1.6):
    return float(np.clip(vel, 0.0, 1.5)) ** curve


def fit(x, n):
    """Pad with zeros or cut to length n along the last axis."""
    if x.shape[-1] >= n:
        return x[..., :n]
    pad = [(0, 0)] * (x.ndim - 1) + [(0, n - x.shape[-1])]
    return np.pad(x, pad)


# ---------------------------------------------------------------- oscillators

def phase_ramp(freq, n, phase0=0.0):
    """Phase in cycles (not wrapped) for a frequency that may vary per sample."""
    f = np.broadcast_to(np.asarray(freq, dtype=float), (n,))
    ph = np.empty(n)
    ph[0] = 0.0
    np.cumsum(f[:-1] / SR, out=ph[1:])
    return ph + phase0


def sine(freq, n, phase0=0.0):
    return np.sin(TAU * phase_ramp(freq, n, phase0))


def _blep_saw_from_phase(ph, dt):
    ph = ph % 1.0
    y = 2.0 * ph - 1.0
    m = ph < dt
    t = ph[m] / dt[m]
    y[m] -= t + t - t * t - 1.0
    m = ph > 1.0 - dt
    t = (ph[m] - 1.0) / dt[m]
    y[m] -= t * t + t + t + 1.0
    return y


def saw(freq, n, phase0=0.0):
    """Band-limited (polyBLEP) sawtooth, -1..1."""
    f = np.broadcast_to(np.asarray(freq, dtype=float), (n,))
    dt = np.minimum(f / SR, 0.5)
    return _blep_saw_from_phase(phase_ramp(f, n, phase0), dt)


def pulse(freq, n, phase0=0.0, width=0.5):
    f = np.broadcast_to(np.asarray(freq, dtype=float), (n,))
    dt = np.minimum(f / SR, 0.5)
    ph = phase_ramp(f, n, phase0)
    return 0.5 * (_blep_saw_from_phase(ph, dt) - _blep_saw_from_phase(ph + width, dt))


def lfo(rate, n, phase0=0.0):
    return np.sin(TAU * (rate * secs(n) + phase0))


# ---------------------------------------------------------------- envelopes

def adsr(gate_s, a=0.01, d=0.2, s=0.7, r=0.3, n_total=None):
    """Attack (sin^2 ramp), exponential decay to sustain, exact-zero release.

    Returns an array of length gate + release (or n_total if given)."""
    ng = nsamp(gate_s)
    nr = nsamp(max(r, 1e-3))
    t = secs(ng)
    env = np.empty(ng)
    a = max(a, 1e-4)
    att = t < a
    env[att] = np.sin(0.5 * np.pi * t[att] / a) ** 2
    td = t[~att] - a
    env[~att] = s + (1.0 - s) * np.exp(-td / max(d / 3.0, 1e-4))
    last = env[-1]
    tr = secs(nr)
    k = 4.6 / max(r, 1e-3)
    rel = last * (np.exp(-k * tr) - np.exp(-k * r)) / (1.0 - np.exp(-k * r))
    out = np.concatenate([env, rel])
    if n_total is not None:
        out = fit(out, n_total)
    return out


def perc_env(n, attack=0.001, decay=0.5, curve=1.0):
    """Fast attack, exponential decay (decay = time to -60 dB)."""
    t = secs(n)
    att = np.clip(t / max(attack, 1e-5), 0.0, 1.0)
    return att * np.exp(-6.91 * (t / max(decay, 1e-4)) ** curve)


def tail_fade(x, seconds=0.005):
    """Short linear fade at the end to avoid clicks."""
    m = min(x.shape[-1], nsamp(seconds))
    x[..., -m:] *= np.linspace(1.0, 0.0, m)
    return x


# ---------------------------------------------------------------- filters (numba)

@njit(cache=True)
def _svf(x, fc, res, mode, sr):
    n = x.shape[0]
    y = np.empty(n)
    ic1 = 0.0
    ic2 = 0.0
    k = 2.0 - 2.0 * res
    if k < 0.02:
        k = 0.02
    for i in range(n):
        f = fc[i]
        if f < 8.0:
            f = 8.0
        if f > 0.45 * sr:
            f = 0.45 * sr
        g = np.tan(np.pi * f / sr)
        a1 = 1.0 / (1.0 + g * (g + k))
        a2 = g * a1
        a3 = g * a2
        v3 = x[i] - ic2
        v1 = a1 * ic1 + a2 * v3
        v2 = ic2 + a2 * ic1 + a3 * v3
        ic1 = 2.0 * v1 - ic1
        ic2 = 2.0 * v2 - ic2
        if mode == 0:
            y[i] = v2
        elif mode == 1:
            y[i] = k * v1
        else:
            y[i] = x[i] - k * v1 - v2
    return y


_SVF_MODES = {"lp": 0, "bp": 1, "hp": 2}


def svf(x, fc, res=0.0, mode="lp"):
    """TPT state-variable filter with per-sample cutoff. x mono or stereo."""
    if x.ndim == 2:
        return np.vstack([svf(c, fc, res, mode) for c in x])
    fcv = np.ascontiguousarray(np.broadcast_to(np.asarray(fc, dtype=float), x.shape))
    return _svf(np.ascontiguousarray(x, dtype=float), fcv, float(res), _SVF_MODES[mode], float(SR))


@njit(cache=True)
def _ladder(x, fc, res, drive, sr):
    n = x.shape[0]
    y = np.empty(n)
    s1 = 0.0
    s2 = 0.0
    s3 = 0.0
    s4 = 0.0
    for i in range(n):
        f = fc[i]
        if f < 8.0:
            f = 8.0
        if f > 0.4 * sr:
            f = 0.4 * sr
        g = 1.0 - np.exp(-2.0 * np.pi * f / sr)
        u = np.tanh(drive * (x[i] - 4.0 * res * s4))
        s1 += g * (u - s1)
        s2 += g * (s1 - s2)
        s3 += g * (s2 - s3)
        s4 += g * (s3 - s4)
        y[i] = s4
    return y


def ladder(x, fc, res=0.2, drive=1.0):
    """4-pole ladder low-pass with tanh input stage (bass, brass)."""
    if x.ndim == 2:
        return np.vstack([ladder(c, fc, res, drive) for c in x])
    fcv = np.ascontiguousarray(np.broadcast_to(np.asarray(fc, dtype=float), x.shape))
    y = _ladder(np.ascontiguousarray(x, dtype=float), fcv, float(res), float(drive), float(SR))
    return y * (1.0 + 1.5 * res) / max(np.tanh(drive), 1e-3)


def butter(x, kind, freq, order=2, zero_phase=True):
    """Static Butterworth filter. kind: lowpass/highpass/bandpass."""
    sos = signal.butter(order, freq, btype=kind, fs=SR, output="sos")
    if zero_phase:
        return signal.sosfiltfilt(sos, x, axis=-1)
    return signal.sosfilt(sos, x, axis=-1)


def onepole_lp(x, fc):
    a = np.exp(-TAU * fc / SR)
    return signal.lfilter([1.0 - a], [1.0, -a], x, axis=-1)


def dc_block(x, fc=12.0):
    return butter(x, "highpass", fc, order=2, zero_phase=True)


def softclip(x, drive=1.0, bias=0.0):
    """tanh waveshaper; bias adds even harmonics (DC removed afterwards)."""
    y = np.tanh(drive * x + bias) - np.tanh(bias)
    return y / max(np.tanh(drive), 1e-3)


# ---------------------------------------------------------------- envelope follower / smoothing

@njit(cache=True)
def _follow(x, att, rel):
    """Asymmetric one-pole: rises with coef att, falls with coef rel."""
    n = x.shape[0]
    y = np.empty(n)
    s = x[0]
    for i in range(n):
        v = x[i]
        if v > s:
            s = v + att * (s - v)
        else:
            s = v + rel * (s - v)
        y[i] = s
    return y


def follow(x, attack_s, release_s):
    att = np.exp(-1.0 / (max(attack_s, 1e-5) * SR))
    rel = np.exp(-1.0 / (max(release_s, 1e-5) * SR))
    return _follow(np.ascontiguousarray(x, dtype=float), att, rel)


def rms_env(x, window_s=0.03):
    w = nsamp(window_s)
    k = np.ones(w) / w
    return np.sqrt(np.maximum(signal.fftconvolve(x * x, k, mode="same"), 0.0))


# ---------------------------------------------------------------- reverb

def make_ir(t60=4.0, damp=0.35, predelay=0.02, length=None, early=12,
            early_span=0.08, width=1.0, seed=7):
    """Stereo impulse response: band-split decaying noise + sparse early reflections.

    t60: decay time of the low-mid bands (s).
    damp: high-band T60 as a fraction of t60 (stone: low, metal: high).
    Normalised so each channel has unit energy."""
    length = length or t60 * 1.25 + predelay
    n = nsamp(length)
    t = secs(n)
    rng = np.random.default_rng(seed)
    edges = [20, 150, 300, 600, 1200, 2400, 4800, 9600, 20000]
    centers = np.sqrt(np.array(edges[:-1]) * np.array(edges[1:]))
    # T60 per band: full at low end, falling to damp * t60 at the top
    frac = np.clip((np.log2(centers) - np.log2(300)) / (np.log2(12000) - np.log2(300)), 0, 1)
    t60s = t60 * (1.0 - (1.0 - damp) * frac)
    t60s[0] *= 1.1
    ir = np.zeros((2, n))
    for ch in range(2):
        noise = rng.standard_normal(n)
        acc = np.zeros(n)
        for (lo, hi), tb in zip(zip(edges[:-1], edges[1:]), t60s):
            hi = min(hi, 0.45 * SR)
            sos = signal.butter(3, [lo, hi], btype="bandpass", fs=SR, output="sos")
            acc += signal.sosfilt(sos, noise) * np.exp(-6.91 * t / tb)
        build = 1.0 - np.exp(-t / 0.025)
        acc *= build
        er = np.zeros(n)
        for _ in range(early):
            ti = rng.uniform(0.004, early_span)
            g = rng.choice([-1, 1]) * 0.6 * np.exp(-ti / 0.06)
            er[nsamp(ti)] += g
        er = onepole_lp(er, 6000.0)
        acc = acc / np.sqrt(np.sum(acc ** 2)) + er * 0.8
        ir[ch] = acc
    mid = ir.mean(0)
    side = (ir[0] - ir[1]) / 2.0
    ir = np.vstack([mid + width * side, mid - width * side])
    pd = nsamp(predelay)
    ir = np.pad(ir, ((0, 0), (pd, 0)))[:, :n]
    ir /= np.sqrt(np.sum(ir ** 2, axis=1, keepdims=True))
    return ir


def convolve_reverb(x, ir):
    """Stereo in -> stereo wet, with some cross-feed so hard-panned sources get a full tail."""
    x = stereo(x)
    l_in = 0.7 * x[0] + 0.3 * x[1]
    r_in = 0.3 * x[0] + 0.7 * x[1]
    n = x.shape[1]
    wl = signal.oaconvolve(l_in, ir[0])[:n]
    wr = signal.oaconvolve(r_in, ir[1])[:n]
    return np.vstack([wl, wr])


def pingpong(x, delay_s, fb=0.45, repeats=8, lp=5000.0):
    """Ping-pong echo: each repeat alternates side and gets darker."""
    x = stereo(x)
    n = x.shape[1]
    d = nsamp(delay_s)
    src = x.mean(0)
    out = np.zeros((2, n))
    cur = src
    for k in range(1, repeats + 1):
        if k * d >= n:
            break
        cur = onepole_lp(cur, lp)
        g = fb ** (k - 1)
        out[(k - 1) % 2, k * d:] += g * cur[: n - k * d]
    return out


# ---------------------------------------------------------------- loudness (ITU-R BS.1770-4)

_K1_B = [1.53512485958697, -2.69169618940638, 1.19839281085285]
_K1_A = [1.0, -1.69065929318241, 0.73248077421585]
_K2_B = [1.0, -2.0, 1.0]
_K2_A = [1.0, -1.99004745483398, 0.99007225036621]


def _kweight(x):
    y = signal.lfilter(_K1_B, _K1_A, x, axis=-1)
    return signal.lfilter(_K2_B, _K2_A, y, axis=-1)


def _block_power(xk, block_s, hop_s):
    n = xk.shape[1]
    b, h = nsamp(block_s), nsamp(hop_s)
    if n < b:
        return np.array([np.sum(np.mean(xk ** 2, axis=1))])
    cs = np.concatenate([np.zeros((xk.shape[0], 1)), np.cumsum(xk ** 2, axis=1)], axis=1)
    starts = np.arange(0, n - b + 1, h)
    ms = (cs[:, starts + b] - cs[:, starts]) / b
    return ms.sum(axis=0)


def loudness(x):
    """Integrated loudness (LUFS), loudness range (LU), max short-term (LUFS)."""
    xk = _kweight(stereo(x))
    z = _block_power(xk, 0.4, 0.1)
    lk = -0.691 + 10 * np.log10(np.maximum(z, 1e-20))
    g1 = lk > -70.0
    if not g1.any():
        return -np.inf, 0.0, -np.inf
    rel = -0.691 + 10 * np.log10(np.mean(z[g1])) - 10.0
    g2 = g1 & (lk > rel)
    integrated = -0.691 + 10 * np.log10(np.mean(z[g2]))
    zs = _block_power(xk, 3.0, 0.1)
    ls = -0.691 + 10 * np.log10(np.maximum(zs, 1e-20))
    s1 = ls > -70.0
    lra = 0.0
    if s1.any():
        relr = -0.691 + 10 * np.log10(np.mean(zs[s1])) - 20.0
        s2 = s1 & (ls > relr)
        if s2.sum() > 1:
            lo, hi = np.percentile(ls[s2], [10, 95])
            lra = hi - lo
    return integrated, lra, float(ls.max())


def short_term_curve(x, hop_s=0.1):
    """Short-term loudness (3 s window) every hop_s seconds, centred."""
    xk = _kweight(stereo(x))
    z = _block_power(xk, 3.0, hop_s)
    t = np.arange(len(z)) * hop_s + 1.5
    return t, -0.691 + 10 * np.log10(np.maximum(z, 1e-20))


def true_peak(x):
    x = stereo(x)
    return max(float(to_db(np.max(np.abs(signal.resample_poly(c, 4, 1))))) for c in x)


# ---------------------------------------------------------------- limiter

@njit(cache=True)
def _limiter_gain(req, look, rel):
    n = req.shape[0]
    m = np.empty(n)
    # forward-looking running minimum over [i, i + look]
    for i in range(n):
        v = 1.0
        hi = i + look + 1
        if hi > n:
            hi = n
        for j in range(i, hi):
            if req[j] < v:
                v = req[j]
        m[i] = v
    # backward moving average over [i - look, i]: ramps down before each peak
    a = np.empty(n)
    acc = 0.0
    for i in range(n):
        acc += m[i]
        if i - look - 1 >= 0:
            acc -= m[i - look - 1]
        cnt = i + 1 if i < look + 1 else look + 1
        a[i] = (acc + (look + 1 - cnt)) / (look + 1.0)  # samples before 0 count as gain 1
    # release: follow down instantly, recover slowly
    g = np.empty(n)
    s = 1.0
    for i in range(n):
        v = a[i]
        if v < s:
            s = v
        else:
            s = v + rel * (s - v)
        g[i] = s
    return g


def limiter_gain(x, ceiling_db=-1.0, lookahead_ms=1.5, release_ms=120.0):
    """Gain curve (n,) of a look-ahead peak limiter on 4x-oversampled peaks, and its deepest
    reduction in dB. Applying the same curve to stems keeps them summing to the mix."""
    x = stereo(x)
    n = x.shape[1]
    peak = np.abs(x).max(axis=0)
    for ch in range(2):          # one channel at a time: a 6-minute file oversampled is large
        up = np.abs(signal.resample_poly(x[ch], 4, 1))[: 4 * n]
        peak = np.maximum(peak, np.pad(up, (0, 4 * n - up.size)).reshape(n, 4).max(axis=1))
        del up
    ceil = db(ceiling_db)
    req = np.minimum(1.0, ceil / np.maximum(peak, 1e-12))
    if req.min() >= 1.0:
        return np.ones(n), 0.0
    look = max(1, int(lookahead_ms * 1e-3 * SR))
    rel = np.exp(-1.0 / (release_ms * 1e-3 * SR))
    g = _limiter_gain(np.ascontiguousarray(req), look, rel)
    return g, float(to_db(g.min()))


def limit(x, ceiling_db=-1.0, lookahead_ms=1.5, release_ms=120.0):
    """Look-ahead peak limiter on 4x-oversampled peaks. Never shifts timing."""
    x = stereo(x)
    g, gr = limiter_gain(x, ceiling_db, lookahead_ms, release_ms)
    return x * g, gr


def true_peak_mono(x):
    return float(to_db(np.max(np.abs(signal.resample_poly(x, 4, 1)))))


# ---------------------------------------------------------------- I/O

def write_wav24(path, x, dither=True, seed=0):
    """48 kHz, 24-bit PCM, stereo WAV (TPDF dither)."""
    x = stereo(x)
    scale = 2 ** 23 - 1
    y = x.T * scale
    if dither:
        rng = np.random.default_rng(seed)
        y = y + rng.uniform(-0.5, 0.5, y.shape) + rng.uniform(-0.5, 0.5, y.shape)
    y = np.clip(np.round(y), -(2 ** 23), 2 ** 23 - 1).astype("<i4")
    raw = y.reshape(-1).view(np.uint8).reshape(-1, 4)[:, :3].tobytes()
    nch, bits = 2, 24
    hdr = b"RIFF" + struct.pack("<I", 36 + len(raw)) + b"WAVE"
    hdr += b"fmt " + struct.pack("<IHHIIHH", 16, 1, nch, SR, SR * nch * bits // 8, nch * bits // 8, bits)
    hdr += b"data" + struct.pack("<I", len(raw))
    with open(path, "wb") as f:
        f.write(hdr)
        f.write(raw)


def read_audio(path, mono=False):
    """Decode any audio file through ffmpeg to float64 at 48 kHz; (2, n) or (n,)."""
    ch = 1 if mono else 2
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-acodec", "pcm_f32le",
         "-ac", str(ch), "-ar", str(SR), "-"],
        check=True, capture_output=True).stdout
    a = np.frombuffer(out, dtype="<f4").astype(float)
    return a if mono else a.reshape(-1, 2).T


# ---------------------------------------------------------------- plucked strings and amps

@njit(cache=True)
def _ks(exc, n, period, g, c):
    """Karplus-Strong string: a delay line of `period` samples (fractional), the classic
    two-point average in the loop, then a one-pole low-pass of coefficient c (darker as c
    rises). Its delay is compensated so the pitch stays put."""
    d = period - 0.5 - c / (1.0 - c)
    if d < 2.0:
        d = 2.0
    L = int(d)
    fr = d - L
    y = np.zeros(n)
    lp = 0.0
    m = exc.shape[0]
    for i in range(n):
        x = exc[i] if i < m else 0.0
        j = i - L
        a = y[j] if j >= 0 else 0.0
        b = y[j - 1] if j - 1 >= 0 else 0.0
        b2 = y[j - 2] if j - 2 >= 0 else 0.0
        avg = 0.5 * (((1.0 - fr) * a + fr * b) + ((1.0 - fr) * b + fr * b2))
        lp = (1.0 - c) * avg + c * lp
        y[i] = x + g * lp
    return y


def ks_string(f, seconds, decay=2.0, bright=0.5, pick=0.13, damp=0.0, rng=None, exc_len=1.0):
    """One plucked string at f Hz: `decay` is the time to -60 dB, `bright` (0..1) the pick's
    brightness, `pick` the pick position along the string (a comb on the excitation),
    `damp` (0..0.95) extra loop darkening (palm mutes)."""
    rng = rng or np.random.default_rng()
    n = nsamp(seconds)
    period = SR / f
    ne = max(4, int(period * exc_len))
    e = rng.uniform(-1, 1, ne)
    e = onepole_lp(e, 300.0 + 12000.0 * bright ** 2)
    k = max(1, int(round(pick * period)))
    e = e - np.concatenate([np.zeros(k), e[:-k]])
    g = 10.0 ** (-3.0 / (max(decay, 0.02) * f))
    g = min(g, 0.99995)
    y = _ks(np.ascontiguousarray(e), n, period, g, float(np.clip(damp, 0.0, 0.95)))
    return y / max(np.abs(y).max(), 1e-9)


def amp_sim(x, drive=20.0, tone=5200.0, body=0.6, scoop=0.5, presence=0.4, tight=110.0):
    """Amp and cabinet: a tight high-pass and mid hump before the clipping, two clipping stages
    at 4x oversampling (no aliasing fizz), then a cabinet: low-body bump, mid scoop,
    presence peak and a steep top roll-off."""
    x = butter(x, "highpass", tight, 2, zero_phase=False)
    x = x + 0.8 * svf(x, 750.0, 0.3, "bp")
    up = signal.resample_poly(x, 4, 1)
    y = np.tanh(drive * up + 0.15) - np.tanh(0.15)
    y = np.tanh(2.2 * y)
    y = signal.resample_poly(y, 1, 4)[: x.shape[0]]
    y = y + body * svf(y, 115.0, 0.45, "bp") - scoop * svf(y, 520.0, 0.25, "bp") + presence * svf(y, 2700.0, 0.35, "bp")
    y = butter(y, "lowpass", tone, 4, zero_phase=False)
    y = butter(y, "lowpass", min(9000.0, tone * 1.8), 2, zero_phase=False)
    y = butter(y, "highpass", 75.0, 2, zero_phase=False)
    return y
