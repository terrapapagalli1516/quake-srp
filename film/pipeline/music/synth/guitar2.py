"""guitar2: a metal rhythm guitar, rebuilt from the string up.

A whole-track patch: it sees every note of its track at once, as a player does, and runs one
signal path per take, so notes meet in one amp the way they do in a real rig.

  strings  a digital waveguide per string (six strings in drop D, and a slot for pinch
           harmonics): a pick (a shaped pulse, combed at the pick position, with scrape noise);
           a loss filter whose decay is set at the fundamental and at 3 kHz, by pitch and by
           palm-mute pressure; a dispersion all-pass (string stiffness); a pitch glide on hard
           attacks (tension); passive coupling at the bridge; a humbucker (two coils, each a
           comb at its position along the string, and the pickup's electrical resonance).
  hands    a new note on a string stops the old one; the fretting hand damps a released note,
           with a little noise; shifts along the wound strings squeak; fast notes alternate
           down and up strokes, and the strings of a chord are struck in the stroke's order.
  amp      16x oversampled: a mid-hump boost in front; four triode stages (asymmetric clipping,
           coupling and Miller filters, a bright shelf, a bias that shifts with the level); a
           bass/middle/treble stack solved from its circuit (Marshall values); a push-pull power
           amp with sag, then the speaker's resonance and presence.
  gate     keyed from the guitar's own signal in front of the amp, applied after it.
  cabinet  a synthesized closed-back 4x12 impulse response (box resonance and modes, cone
           break-up peaks and notches, a steep top, leakage from the other speakers), seen by two
           mics, close on-axis and off-axis, blended.
  takes    two complete performances, hard left and right, each with its own timing, drift,
           dynamics, tuning and pick position.
"""
from __future__ import annotations

from functools import lru_cache

import numpy as np
from numba import njit
from scipy import signal

from dsp import SR, follow
from patches import _CHORDS, patch

OS = 16                                                    # amp oversampling
OPEN = 73.416 * 2.0 ** (np.array([0, 7, 12, 17, 21, 26]) / 12.0)   # drop D: D A D G B E
WOUND = (True, True, True, True, False, False)
HSLOT = 6                                                  # the pinch harmonic's own slot
NSLOT = 7
COILS = (0.056, 0.084)          # humbucker coils, as fractions of the scale from the bridge
# the preamp's four triode stages: coupling high-pass (Hz), bright shelf (dB), gain, bias,
# Miller low-pass (Hz). The first stage's gain is scaled by drive/22.
STAGES = [(25.0, 4.0, 6.0, 0.25, 14000.0),
          (140.0, 8.0, 8.0, -0.35, 10000.0),
          (220.0, 2.0, 6.0, 0.30, 8500.0),
          (40.0, 0.0, 2.0, -0.15, 9000.0)]
GATE_HOLD, GATE_REL = 0.012, 0.025
HISS_DB = -92.0                 # hiss at the amp input, re a hard chord
DI_NORM = 5.0                   # calibrated: a hard open D power chord peaks near 1
OUT_NORM = 2.16                 # calibrated: the test riff as loud as the old guitar at the same track gain


# ================================================================== the strings

@njit(cache=True)
def _strings(n, exc_s, exc_i, exc_v, seg_s, seg_i, seg_P, seg_g, seg_a, seg_c1, seg_c2,
             seg_glide, seg_tau, seg_vib, seg_vrate, seg_v0, seg_damp, seg_cp, disp, nd, couple, nslot):
    """Run all strings together, sample by sample.

    exc_*: excitation samples (string, sample index, value), sorted by index.
    seg_*: parameter segments (string, start sample, base period, loop gain per pass, loss
    coefficient, coil 1 and 2 positions as fractions of the period, attack glide in cents and
    its time constant in samples, vibrato depth in cents, rate in cycles per sample and start
    sample, a factor applied to the string's vibration when the segment starts, and whether the
    string is sounding: only sounding strings share energy at the bridge).
    disp: dispersion all-pass coefficient per string; nd: how many all-passes in each loop.
    Returns the pickup's output (n,)."""
    mask = 8191
    buf = np.zeros((nslot, 8192))
    lp = np.zeros(nslot)
    apx = np.zeros((nslot, 8))
    apy = np.zeros((nslot, 8))
    loop = np.zeros(nslot)
    P0 = np.full(nslot, 400.0)
    G = np.full(nslot, 0.5)
    A = np.full(nslot, 0.5)
    C1 = np.zeros(nslot)
    C2 = np.zeros(nslot)
    GL = np.zeros(nslot)
    TAU = np.ones(nslot)
    VB = np.zeros(nslot)
    VR = np.zeros(nslot)
    V0 = np.zeros(nslot, dtype=np.int64)
    I0 = np.zeros(nslot, dtype=np.int64)
    CP = np.zeros(nslot)
    out = np.zeros(n)
    ie = 0
    js = 0
    ne = exc_i.shape[0]
    ns = seg_i.shape[0]
    two_pi = 2.0 * np.pi
    for i in range(n):
        while js < ns and seg_i[js] <= i:
            s = seg_s[js]
            P0[s] = seg_P[js]
            G[s] = seg_g[js]
            A[s] = seg_a[js]
            C1[s] = seg_c1[js]
            C2[s] = seg_c2[js]
            GL[s] = seg_glide[js]
            TAU[s] = seg_tau[js]
            VB[s] = seg_vib[js]
            VR[s] = seg_vrate[js]
            V0[s] = seg_v0[js]
            I0[s] = seg_i[js]
            CP[s] = seg_cp[js]
            f = seg_damp[js]
            if f != 1.0:                       # the pick (or a hand) stops what was ringing
                for k in range(8192):
                    buf[s, k] *= f
                lp[s] *= f
                for k in range(nd):
                    apx[s, k] *= f
                    apy[s, k] *= f
            js += 1
        mean = 0.0
        ncp = 0.0
        for s in range(nslot):
            dt = i - I0[s]
            cents = GL[s] * np.exp(-dt / TAU[s])
            if VB[s] != 0.0 and i > V0[s]:
                ramp = min((i - V0[s]) / (0.3 * 48000.0), 1.0)
                cents += VB[s] * ramp * np.sin(two_pi * VR[s] * (i - V0[s]))
            P = P0[s] * 2.0 ** (-cents / 1200.0)
            w = two_pi / P
            a = A[s]
            c = disp[s]
            # phase delays at the fundamental: the loss filter and the all-passes
            tl = np.arctan2(a * np.sin(w), 1.0 - a * np.cos(w)) / w
            ph = np.arctan2(-np.sin(w), c + np.cos(w)) - np.arctan2(-c * np.sin(w), 1.0 + c * np.cos(w))
            tap = -ph / w
            D = P - tl - nd * tap
            if D < 3.0:
                D = 3.0
            m = int(D)
            x = D - (m - 1)                     # 1 <= x < 2: taps at delays m-1 .. m+2
            h0 = -(x - 1.0) * (x - 2.0) * (x - 3.0) / 6.0
            h1 = x * (x - 2.0) * (x - 3.0) / 2.0
            h2 = -x * (x - 1.0) * (x - 3.0) / 2.0
            h3 = x * (x - 1.0) * (x - 2.0) / 6.0
            b0 = (i - (m - 1)) & mask
            v = (h0 * buf[s, b0] + h1 * buf[s, (b0 - 1) & mask] + h2 * buf[s, (b0 - 2) & mask]
                 + h3 * buf[s, (b0 - 3) & mask])
            lp[s] = (1.0 - a) * v + a * lp[s]
            z = G[s] * lp[s]
            for k in range(nd):
                y = c * z + apx[s, k] - c * apy[s, k]
                apx[s, k] = z
                apy[s, k] = y
                z = y
            loop[s] = z
            if CP[s] > 0.0:
                mean += z
                ncp += 1.0
        if ncp > 0.0:
            mean /= ncp
        while ie < ne and exc_i[ie] < i:
            ie += 1
        tot = 0.0
        for s in range(nslot):
            e = 0.0
            k = ie
            while k < ne and exc_i[k] == i:
                if exc_s[k] == s:
                    e += exc_v[k]
                k += 1
            if CP[s] > 0.0 and ncp > 1.0:
                val = e + (1.0 - couple) * loop[s] + couple * mean
            else:
                val = e + loop[s]
            buf[s, i & mask] = val
            # the humbucker: each coil sees the string at its own position (a comb)
            P = P0[s]
            d1 = C1[s] * P
            d2 = C2[s] * P
            j1 = int(d1)
            f1 = d1 - j1
            j2 = int(d2)
            f2 = d2 - j2
            t1 = (1.0 - f1) * buf[s, (i - j1) & mask] + f1 * buf[s, (i - j1 - 1) & mask]
            t2 = (1.0 - f2) * buf[s, (i - j2) & mask] + f2 * buf[s, (i - j2 - 1) & mask]
            tot += val - 0.5 * (t1 + t2)
        out[i] = tot
    return out


def _loss(f0, T0, Th, fh=3000.0):
    """One-pole loss filter (1-a)/(1-a z^-1) times g, so the loop decays to -60 dB in T0 seconds
    at the fundamental and in Th at fh."""
    G0 = 10.0 ** (-3.0 / (max(T0, 1e-3) * f0))
    Gh = 10.0 ** (-3.0 / (max(Th, 1e-4) * f0))
    r2 = min((Gh / G0) ** 2, 0.999999)
    w0 = 2 * np.pi * f0 / SR
    wh = 2 * np.pi * min(fh, 0.45 * SR) / SR
    c0, ch = np.cos(w0), np.cos(wh)
    q = c0 - r2 * ch
    disc = q * q - (1 - r2) ** 2
    a = 0.95 if disc < 0 else (q - np.sqrt(disc)) / (1 - r2)
    a = float(np.clip(a, 0.0, 0.95))
    H0 = (1 - a) / np.sqrt(1 - 2 * a * c0 + a * a)
    g = min(G0 / H0, 0.99999)
    return g, a


def _place(fr, chord_iv):
    """Strings and frets for a chord whose root is fr Hz: the lowest string that holds the root
    at or below the 12th fret, the other notes on the strings above it."""
    s0 = 0
    for s in range(6):
        if 12 * np.log2(fr / OPEN[s]) <= 12.5:
            s0 = s
            break
    else:
        s0 = 5
    out = []
    for k, iv in enumerate(chord_iv):
        s = min(s0 + k, 5)
        f = fr * 2.0 ** (iv / 12.0)
        fret = max(12 * np.log2(f / OPEN[s]), 0.0)
        out.append((s, f, fret))
    return out


def _pulse(width, beta_P, amp, rng, scrape):
    """The pick: a raised-cosine push `width` samples long, combed at the pick position
    (beta_P samples), and a short scrape of noise."""
    w = max(int(width), 3)
    p = np.hanning(w + 2)[1:-1]
    p = p / p.sum()
    k = max(int(round(beta_P)), 1)
    e = np.zeros(w + k + 1)
    e[:w] += p
    e[k:k + w] -= p
    nn = int(0.004 * SR)
    noise = rng.standard_normal(nn) * np.exp(-np.arange(nn) / (0.0012 * SR))
    noise = signal.lfilter([1, -1], [1], noise) * 0.5            # bright
    e2 = np.zeros(max(e.size, nn))
    e2[:e.size] += e * amp
    e2[:nn] += noise * amp * scrape * 0.04
    return e2


def _perform(notes, take, p, rng, double):
    """One take: the notes as this player plays them. Returns lists for the kernel and the
    hand-noise events."""
    exc = []          # (string, start sample, array)
    segs = []         # tuples for the kernel
    noises = []       # (start sample, kind, amplitude, length)
    last = [None] * NSLOT      # (fret, end sample) per string
    rel_at = [None] * NSLOT    # where each string's pending release sits in segs
    pick0 = float(p["pick"]) * (1.0 + (0.07 if take else -0.07) * double)
    tune0 = (0.5 if take else -0.5) * float(p["detune"]) * double + rng.normal(0, 0.8)
    drift = 0.0
    t_prev = None
    stroke = 1                  # 1 down, -1 up
    slop, vary = float(p["slop"]), float(p["vary"])
    for nt in notes:
        q = nt["p"]
        t0, t1 = nt["t0"], nt["t1"]
        # timing: a slow drift (rushing, dragging) and a per-note jitter, both this take's own
        if t_prev is not None:
            rho = np.exp(-(t0 - t_prev) / 1.5)
            drift = rho * drift + np.sqrt(1 - rho * rho) * rng.normal(0, float(p["drift"]))
        else:
            drift = rng.normal(0, float(p["drift"]))
        jit = float(np.clip(rng.normal(0, slop), -3 * slop, 3 * slop)) if slop > 0 else 0.0
        # picking: fast notes alternate, a note after a rest is a downstroke
        if t_prev is not None and (t0 - t_prev) < 0.2:
            stroke = -stroke
        else:
            stroke = 1
        t_prev = t0
        s0 = t0 + drift + jit
        s1 = max(t1 + drift + jit + rng.normal(0, 0.003), s0 + 0.012)
        k0, k1 = int(round(s0 * SR)), int(round(s1 * SR))
        vel = float(np.clip(nt["vel"] * (1 + rng.normal(0, vary)) * (0.92 if stroke < 0 else 1.0), 0.05, 1.3))
        mute = float(np.clip(q["mute"], 0, 1))
        bright = float(np.clip(q["bright"], 0, 1)) * (1.08 if stroke < 0 else 1.0)
        iv = _CHORDS.get(str(q["chord"]), (0, 7, 12))
        harm = float(q["harm"])
        placed = _place(nt["freq"], iv)
        if stroke < 0:
            placed = placed[::-1]
        if q["strum"] > 0:
            spread = float(q["strum"])
        else:
            spread = (0.0012 + 0.0025 * (1 - mute)) * (0.7 + 0.6 * rng.uniform())
        for order, (s, f, fret) in enumerate(placed):
            is_root = (stroke > 0 and order == 0) or (stroke < 0 and order == len(placed) - 1)
            v = vel * (1.0 if is_root else 0.92)
            if harm and len(placed) > 1 and not is_root:
                v *= 0.35
            ks = k0 + int(round(order * spread * SR))
            cents = tune0 + rng.normal(0, 1.2) + 2.0 * mute
            Lv = 2.0 ** (-fret / 12.0)                 # the vibrating length's share of the scale
            slot, ff = s, f
            if harm and is_root:
                slot, ff = HSLOT, f * harm
            P = SR / (ff * 2.0 ** (cents / 1200.0))
            beta = np.clip(pick0 * (1 + rng.normal(0, 0.05)) / Lv, 0.02, 0.45)
            if slot == HSLOT:
                beta = np.clip(beta * harm, 0.02, 0.45)
            c1 = min(COILS[0] / Lv * (harm if slot == HSLOT else 1.0), 0.48)
            c2 = min(COILS[1] / Lv * (harm if slot == HSLOT else 1.0), 0.48)
            # decay at the fundamental and at 3 kHz: open strings ring, the palm kills the top
            T0o = float(q["decay"]) * (1.0 if WOUND[s] else 0.8) * (2.0 ** (-fret / 24.0))
            Tho = (0.30 if WOUND[s] else 0.45) * (0.6 + 0.8 * bright)
            T0 = np.exp((1 - mute ** 0.7) * np.log(T0o) + mute ** 0.7 * np.log(0.16))
            Th = np.exp((1 - mute ** 0.5) * np.log(Tho) + mute ** 0.5 * np.log(0.012))
            if slot == HSLOT:
                T0, Th = max(float(q["decay"]), 2.5), 0.6
            g, a = _loss(ff, T0, Th)
            glide = float(p["glide"]) * v * v * (1.4 if WOUND[s] else 0.8)
            vib = float(q["vibrato"]) if slot == HSLOT else 0.0
            damp = 1.0
            prev = last[slot]
            if prev is not None and prev[1] > ks - int(0.3 * SR):
                damp = 0.12                              # the pick lands on a moving string
            if rel_at[slot] is not None and segs[rel_at[slot]] is not None and segs[rel_at[slot]][1] >= ks:
                segs[rel_at[slot]] = None                # the old note's release came too late
            if slot == HSLOT:                            # the pinched string itself stops
                segs.append((s, ks, SR / OPEN[s], *_loss(OPEN[s], 0.04, 0.01), 0.1, 0.1,
                             0.0, 1.0, 0.0, 0.0, 0, 0.1, 0.0))
                if rel_at[s] is not None and segs[rel_at[s]] is not None and segs[rel_at[s]][1] >= ks:
                    segs[rel_at[s]] = None
                last[s] = (fret, ks)
            # position shifts on wound strings squeak
            if prev is not None and WOUND[s] and slot != HSLOT and abs(prev[0] - fret) >= 2 \
                    and ks - prev[1] < int(0.08 * SR):
                noises.append((ks - int(0.045 * SR), "squeak", 0.004 * float(p["noise"]) * (0.7 + 0.6 * rng.uniform()),
                               int(0.04 * SR), 1 if fret > prev[0] else -1))
            segs.append((slot, ks, P, g, a, c1, c2, glide, 0.05 * SR, vib, 5.6 / SR, ks + int(0.17 * SR), damp,
                         0.0 if slot == HSLOT else 1.0))
            # the pick: shorter and brighter when hard and bright
            width = (0.00018 + 0.0009 * (1 - bright)) * SR * (1.25 - 0.45 * min(v, 1.0))
            amp = (0.15 + 0.85 * min(v, 1.2)) ** 1.4 * (float(q["harm_level"]) * 0.8 if slot == HSLOT else 1.0)
            exc.append((slot, ks, _pulse(width, beta * P, amp, rng, float(p["noise"]))))
            # the release: the fretting hand (or the palm) damps the string
            gr, ar = _loss(ff, 0.05 if mute < 0.5 else 0.035, 0.008)
            rel_at[slot] = len(segs)
            segs.append((slot, k1, P, gr, ar, c1, c2, 0.0, 1.0, 0.0, 0.0, 0, 1.0, 0.0))
            if mute < 0.5 and (k1 - ks) > int(0.15 * SR):
                noises.append((k1, "release", 0.006 * float(p["noise"]) * v, int(0.012 * SR), 0))
            last[slot] = (fret, k1)
    return exc, [sg for sg in segs if sg is not None], noises


def _hand_noise(n, noises, rng):
    y = np.zeros(n)
    for (k, kind, amp, ln, direction) in noises:
        if k < 0 or k >= n:
            continue
        m = min(ln, n - k)
        t = np.arange(m) / SR
        z = rng.standard_normal(m)
        if kind == "release":
            z = signal.lfilter(*signal.butter(2, [700, 3500], "bandpass", fs=SR), z)
            env = np.exp(-t / 0.003)
        else:                                          # a squeak: a band of noise that glides
            fc = 900 * 2.0 ** (direction * 1.2 * t / max(t[-1], 1e-6))
            ph = 2 * np.pi * np.cumsum(fc) / SR
            z = signal.lfilter(*signal.butter(2, [400, 6000], "bandpass", fs=SR), z) * (1 + np.sin(ph))
            env = np.sin(np.pi * t / max(t[-1], 1e-6)) ** 2
        y[k:k + m] += amp * z * env
    return y


def _render_strings(notes, n0, n, take, p, rng, double):
    exc, segs, noises = _perform(notes, take, p, rng, double)
    # the excitations as (sample, string, value), sorted by sample
    ii, ss, vv = [], [], []
    for (s, k, e) in exc:
        idx = np.arange(k - n0, k - n0 + e.size)
        ok = (idx >= 0) & (idx < n)
        ii.append(idx[ok])
        ss.append(np.full(int(ok.sum()), s))
        vv.append(e[ok])
    exc_i = np.concatenate(ii).astype(np.int64) if ii else np.zeros(0, np.int64)
    exc_s = np.concatenate(ss).astype(np.int64) if ss else np.zeros(0, np.int64)
    exc_v = np.concatenate(vv).astype(np.float64) if vv else np.zeros(0)
    o = np.argsort(exc_i, kind="stable")
    exc_i, exc_s, exc_v = exc_i[o], exc_s[o], exc_v[o]
    # the unplayed strings are held quiet by the hands
    allseg = []
    for s in range(NSLOT):
        f = OPEN[s] if s < 6 else OPEN[0] * 4
        g, a = _loss(f, 0.08, 0.01)
        allseg.append((s, 0, SR / f, g, a, 0.06, 0.09, 0.0, 1.0, 0.0, 0.0, 0, 1.0, 0.0))
    for sg in segs:
        allseg.append((sg[0], max(sg[1] - n0, 0)) + tuple(sg[2:11]) + (sg[11] - n0 if sg[11] else 0, sg[12], sg[13]))
    allseg.sort(key=lambda z: z[1])          # stable: the quiet strings first at sample 0
    cols = list(zip(*allseg))
    ints = (0, 1, 11)
    seg = [np.asarray(c, dtype=np.int64 if k in ints else np.float64) for k, c in enumerate(cols)]
    disp = np.array([-0.10, -0.12, -0.14, -0.16, -0.22, -0.25, -0.20]) * float(p["stiff"])
    y = _strings(n, exc_s, exc_i, exc_v, *seg, disp, 4, float(p["couple"]), NSLOT)
    y += _hand_noise(n, [(z[0] - n0,) + tuple(z[1:]) for z in noises], rng)
    # the pickup's electrical resonance (the coil's inductance against the cable's capacitance)
    b, a = _rbj_lowpass(3200.0, 1.9, SR)
    y = signal.lfilter(b, a, y)
    return y * DI_NORM


# ================================================================== the amp

def _rbj_lowpass(f, q, fs):
    w = 2 * np.pi * f / fs
    al = np.sin(w) / (2 * q)
    b = np.array([(1 - np.cos(w)) / 2, 1 - np.cos(w), (1 - np.cos(w)) / 2])
    a = np.array([1 + al, -2 * np.cos(w), 1 - al])
    return b / a[0], a / a[0]


def _rbj(kind, f, q, gain_db, fs):
    A = 10 ** (gain_db / 40)
    w = 2 * np.pi * f / fs
    al = np.sin(w) / (2 * q)
    cw = np.cos(w)
    if kind == "peak":
        b = [1 + al * A, -2 * cw, 1 - al * A]
        a = [1 + al / A, -2 * cw, 1 - al / A]
    elif kind == "highshelf":
        sq = 2 * np.sqrt(A) * al
        b = [A * ((A + 1) + (A - 1) * cw + sq), -2 * A * ((A - 1) + (A + 1) * cw), A * ((A + 1) + (A - 1) * cw - sq)]
        a = [(A + 1) - (A - 1) * cw + sq, 2 * ((A - 1) - (A + 1) * cw), (A + 1) - (A - 1) * cw - sq]
    elif kind == "lowshelf":
        sq = 2 * np.sqrt(A) * al
        b = [A * ((A + 1) - (A - 1) * cw + sq), 2 * A * ((A - 1) - (A + 1) * cw), A * ((A + 1) - (A - 1) * cw - sq)]
        a = [(A + 1) + (A - 1) * cw + sq, -2 * ((A - 1) + (A + 1) * cw), (A + 1) + (A - 1) * cw - sq]
    else:
        raise ValueError(kind)
    b, a = np.array(b), np.array(a)
    return b / a[0], a / a[0]


def _hp1(x, f, fs):
    b, a = signal.butter(1, f, "highpass", fs=fs)
    return signal.lfilter(b, a, x)


def _lp1(x, f, fs):
    b, a = signal.butter(1, f, "lowpass", fs=fs)
    return signal.lfilter(b, a, x)


def _triode(u):
    """Asymmetric: grid conduction flattens the positive side hard, cutoff rounds the negative
    side later and softer."""
    pos = u / (1.0 + np.abs(u) ** 2.5) ** 0.4
    neg = -1.6 * np.tanh(-u / 1.6)
    return np.where(u >= 0, pos, neg)


def _taper(x):                                  # an audio-taper pot
    x = float(np.clip(x, 0, 1))
    return (np.exp(4.0 * x) - 1) / (np.exp(4.0) - 1)


def _stack_response(w, t, m, l, R1=220e3, R2=1e6, R3=25e3, R4=33e3, C1=470e-12, C2=22e-9, C3=22e-9,
                    Rs=1.2e3, RL=1e6):
    """The bass/middle/treble network solved by nodal analysis at angular frequencies w.
    Nodes: A (treble pot top), O (wiper, the output), B (treble pot bottom = bass pot top),
    S (behind the slope resistor), M (mid pot top). Driven by a cathode follower (Rs) from Vi=1."""
    H = np.zeros(w.size, complex)
    ta, tb = max(R1 * (1 - t), 1.0), max(R1 * t, 1.0)
    rb = max(R2 * l, 1.0)
    rm = max(R3 * m, 1.0)
    for k, wk in enumerate(w):
        s = 1j * wk
        # unknowns: Vin' (after Rs), A, O, B, S, M
        Y = np.zeros((6, 6), complex)
        I = np.zeros(6, complex)

        def add(i, j, y):
            Y[i, i] += y
            if j is not None:
                Y[j, j] += y
                Y[i, j] -= y
                Y[j, i] -= y
        add(0, None, 1 / Rs)
        I[0] += 1 / Rs                      # the source through Rs
        add(0, 1, s * C1)                   # Vin' - A: C1
        add(1, 2, 1 / ta)                   # A - O
        add(2, 3, 1 / tb)                   # O - B
        add(0, 4, 1 / R4)                   # Vin' - S: slope resistor
        add(4, 3, s * C2)                   # S - B: bass cap
        add(4, 5, s * C3)                   # S - M: mid cap
        add(3, 5, 1 / rb)                   # B - M: bass pot
        add(5, None, 1 / rm)                # M - ground: mid pot
        add(2, None, 1 / RL)                # O - ground: the next stage
        V = np.linalg.solve(Y, I)
        H[k] = V[2]
    return H


@lru_cache(maxsize=16)
def _stack_sos(t, m, l, fs):
    """Fit the network's exact third-order rational response, then the bilinear transform."""
    t, m, l = float(t), _taper(m), _taper(l)
    w = 2 * np.pi * np.geomspace(10, 30000, 120)
    H = _stack_response(w, t, m, l)
    wn = 2 * np.pi * 1000.0
    sn = 1j * w / wn
    # H (1 + a1 s + a2 s^2 + a3 s^3) = b0 + b1 s + b2 s^2 + b3 s^3, linear in the unknowns
    Acol = [np.ones_like(sn), sn, sn ** 2, sn ** 3, -H * sn, -H * sn ** 2, -H * sn ** 3]
    M = np.stack(Acol, axis=1)
    M2 = np.vstack([M.real, M.imag])
    r2 = np.concatenate([H.real, H.imag])
    sol, *_ = np.linalg.lstsq(M2, r2, rcond=None)
    b = sol[:4] / wn ** np.arange(4)
    a = np.concatenate([[1.0], sol[4:] / wn ** np.arange(1, 4)])
    bz, az = signal.bilinear(b[::-1], a[::-1], fs=fs)
    return signal.tf2sos(bz, az)


@lru_cache(maxsize=8)
def _cab_ir(mic, seed=7):
    """A closed-back 4x12 with 12-inch speakers, synthesized: the magnitude response of the
    box, the cone and each mic, made minimum phase, plus leakage from the other speakers.
    mic: the share of the off-axis mic in the blend."""
    nfft = 16384
    f = np.fft.rfftfreq(nfft, 1 / SR)
    f[0] = 1.0
    rng = np.random.default_rng(seed)

    def peak(fc, q, g):
        return g / (1 + (q * (f / fc - fc / f)) ** 2)

    def hp2(fc, q):
        r = f / fc
        return 20 * np.log10(r ** 2 / np.sqrt((1 - r ** 2) ** 2 + (r / q) ** 2))

    def lpn(fc, order):
        return -10 * np.log10(1 + (f / fc) ** (2 * order))

    box = hp2(92.0, 1.15) + peak(230, 6, -2.0) + peak(470, 6, -1.5) + peak(700, 6, -1.0) + peak(420, 1.4, -2.5)
    cone = (3.0 * (1 / (1 + (1600 / f) ** 2)) + peak(1500, 2.5, 1.5) + peak(1050, 3, -3.0)
            + peak(2500, 3.0, 5.0) + peak(3300, 6, -7.0) + peak(4200, 3.5, 3.5) + peak(5600, 5, -8.0)
            + lpn(5200, 4) + lpn(7200, 4))
    # cone break-up: small irregular peaks and dips through the upper mids
    wig = np.zeros_like(f)
    for _ in range(14):
        fc = np.exp(rng.uniform(np.log(900), np.log(6500)))
        wig += peak(fc, rng.uniform(4, 10), rng.uniform(-2.5, 2.5))
    spk = box + cone + wig
    micA = spk + 2.0 / (1 + (f / 160) ** 2) + peak(3200, 1.0, 3.0) + lpn(14000, 2)          # close, on axis
    micB = spk + 1.0 / (1 + (f / 140) ** 2) + lpn(2600, 1) + lpn(9000, 2) + peak(160, 1.2, 1.5)  # off axis, back

    def minphase(mag_db):
        mag = 10 ** (mag_db / 20)
        cep = np.fft.irfft(np.log(np.maximum(mag, 1e-6)), nfft)
        fold = np.zeros(nfft)
        fold[0] = cep[0]
        fold[1:nfft // 2] = 2 * cep[1:nfft // 2]
        fold[nfft // 2] = cep[nfft // 2]
        h = np.fft.irfft(np.exp(np.fft.rfft(fold)), nfft)
        return h[:4096]

    hA, hB = minphase(micA), minphase(micB)
    ir = np.zeros(4096 + 400)
    ir[:4096] += hA * (1 - mic)
    dB = int(round(0.0001 * SR))      # mic B sits further back, time-aligned to within 0.1 ms
    ir[dB:dB + 4096] += hB * mic
    # the other three speakers leak into the close mic, later and darker
    b, a = signal.butter(2, 2500, "lowpass", fs=SR)
    for d_ms, g in ((0.95, 0.10), (1.25, 0.08), (1.55, 0.06)):
        k = int(round(d_ms * 1e-3 * SR))
        ir[k:k + 4096] += g * signal.lfilter(b, a, hA)
    # the closed box: the back panel's reflection through the cone, low and late
    b2, a2 = signal.butter(2, 900, "lowpass", fs=SR)
    k = int(round(0.0021 * SR))
    ir[k:k + 4096] += 0.12 * signal.lfilter(b2, a2, hA)
    ir = ir[:4096]
    ir *= np.concatenate([np.ones(3072), np.hanning(2048)[1024:]])
    # unity gain at 1-2 kHz
    H = np.abs(np.fft.rfft(ir, 8192))
    fr = np.fft.rfftfreq(8192, 1 / SR)
    ir /= np.sqrt(np.mean(H[(fr > 800) & (fr < 2500)] ** 2))
    return ir


@njit(cache=True)
def _gate_kernel(key, thr_open, thr_close, hold, att, rel, floor):
    n = key.shape[0]
    g = np.empty(n)
    state = 0.0
    open_ = False
    hc = 0
    for i in range(n):
        if key[i] > thr_open:
            open_ = True
            hc = hold
        elif key[i] < thr_close:
            if hc > 0:
                hc -= 1
            else:
                open_ = False
        target = 1.0 if open_ else floor
        c = att if target > state else rel
        state = target + c * (state - target)
        g[i] = state
    return g


def _gate(di, thr_db):
    if thr_db <= -119:
        return np.ones_like(di)
    look = int(0.001 * SR)
    key = follow(np.abs(di), 0.0002, 0.012)
    key = np.concatenate([key[look:], np.zeros(look)])        # open a hair early
    thr = 10 ** (thr_db / 20)
    return _gate_kernel(key, thr, thr * 0.6, int(GATE_HOLD * SR), np.exp(-1 / (0.0004 * SR)),
                        np.exp(-1 / (GATE_REL * SR)), 10 ** (-70 / 20))


def _amp(di, p, rng):
    """DI (48 kHz) -> amp and cabinet -> (48 kHz)."""
    fs = SR * OS
    x = di + 10 ** (HISS_DB / 20) * rng.standard_normal(di.size)       # the pickup and the first tube hiss
    # the boost in front: a mid hump (the bass passes at unity), mild diode clipping
    hp = _hp1(x, 720.0, SR)
    y = x + (2.0 + 6.0 * float(p["boost"])) * hp
    y = _lp1(y, 5500.0, SR)
    up = signal.resample_poly(y, OS, 1)                  # every clipper runs oversampled
    up = np.tanh(up * 0.8) / 0.8 * (0.55 + 0.5 * float(p["boost"]))
    drive = float(p["drive"]) / 22.0
    # four triode stages: (coupling high-pass, bright shelf dB, gain, bias, Miller low-pass)
    for k, (fhp, bright, gain, bias, flp) in enumerate(STAGES):
        gain = gain * drive if k == 0 else gain
        up = _hp1(up, fhp, fs)
        if bright:
            b, a = _rbj("highshelf", 1800.0, 0.6, bright, fs)
            up = signal.lfilter(b, a, up)
        u = gain * up
        shift = 0.12 * follow(np.maximum(u, 0.0) / (1 + np.maximum(u, 0.0)), 0.001, 0.03)
        up = -(_triode(u + bias - shift) - _triode(np.full(1, bias))[0])
        up = _lp1(up, flp, fs)
    # the tone stack, after the gain stages, driven by a cathode follower
    up = signal.sosfilt(_stack_sos(float(p["treble"]), float(p["middle"]), float(p["bass"]), fs), up)
    up = up * 6.0                                       # make up the stack's loss
    # the power amp: push-pull (symmetric), with sag
    x2 = up * (0.6 + 2.4 * float(p["master"]))
    env = follow(np.abs(x2), 0.002, 0.12)
    h = 1.0 / (1.0 + 0.6 * float(p["sag"]) * env)
    y = h * np.tanh(x2 / h)
    y = _hp1(y, 45.0, fs)
    b, a = signal.butter(2, 15000.0, "lowpass", fs=fs)
    y = signal.lfilter(b, a, y)
    y = signal.resample_poly(y, 1, OS)[: di.size]
    # the speaker's impedance against the amp: the resonance (low) and presence (high)
    b, a = _rbj("peak", 100.0, 1.3, 5.0 * float(p["resonance"]), SR)
    y = signal.lfilter(b, a, y)
    b, a = _rbj("highshelf", 2500.0, 0.7, 8.0 * float(p["presence"]) - 2.0, SR)
    y = signal.lfilter(b, a, y)
    # the gate, keyed by the guitar in front of the amp
    y = y * _gate(di, float(p["gate"]))
    # the cabinet and the mics
    y = signal.fftconvolve(y, _cab_ir(round(float(p["mic"]), 3)))[: di.size]
    y = signal.sosfilt(signal.butter(2, 70.0, "highpass", fs=SR, output="sos"), y)
    y = signal.sosfilt(signal.butter(2, float(p["tone"]), "lowpass", fs=SR, output="sos"), y)
    return y * OUT_NORM


# ================================================================== the patch

def _phrases(notes, gap=1.0):
    """Notes grouped into phrases separated by at least `gap` seconds of silence."""
    out, cur, end = [], [], -1e9
    for nt in notes:
        if cur and nt["t0"] > end + gap:
            out.append(cur)
            cur, end = [], -1e9
        cur.append(nt)
        end = max(end, nt["t1"] + 0.3)
    if cur:
        out.append(cur)
    return out


@patch("guitar2", dict(chord="power", mute=0.0, decay=4.0, bright=0.6, pick=0.085, strum=0.0,
                       harm=0, harm_level=0.6, vibrato=35.0, glide=9.0, stiff=1.0, couple=0.004,
                       drive=22.0, boost=0.6, bass=0.55, middle=0.5, treble=0.7, master=0.5,
                       sag=0.5, presence=0.65, resonance=0.5, gate=-44.0, mic=0.35, tone=12000.0,
                       noise=1.0, double=1, detune=4.0, slop=0.004, drift=0.003, vary=0.06,
                       level=0.5),
       "Metal guitar, rebuilt: waveguide strings in drop D (pick position, "
       "pitch- and mute-dependent damping, stiffness, a pitch glide on hard attacks, bridge "
       "coupling, a humbucker), the player's hands (string stops, release noise, squeaks, "
       "alternate picking, strum order), then one rig per take: a boost, four oversampled triode "
       "stages, a bass/middle/treble stack solved from its circuit, a push-pull power amp with "
       "sag, a gate, a synthesized 4x12 seen by two mics. Two takes hard left and right. "
       "Per note: chord, mute (palm 0..1), decay, bright, pick, strum (0 = by the stroke), harm "
       "(pinch harmonic on partial K), harm_level, vibrato. The rest are the rig's and the "
       "player's, set on the track: drive (pre-gain; 22 is high gain, 3 near clean), boost, bass, "
       "middle, treble, master, sag, presence, resonance (0..1), gate (dB, -120 = off), mic "
       "(share of the off-axis mic), tone (Hz, a final low-pass), noise (hand noises), double, "
       "detune (cents between takes), slop and drift (s, timing), vary (velocity).",
       mono="notes")
def guitar2(notes, n, p, rng):
    notes = sorted(notes, key=lambda z: z["t0"])
    out = np.zeros((2, n))
    double = int(p["double"])
    for ph in _phrases(notes):
        a = max(ph[0]["t0"] - 0.08, 0.0)
        b = max(z["t1"] for z in ph) + 1.2
        n0, n1 = int(a * SR), min(int(b * SR), n)
        if n1 <= n0:
            continue
        m = n1 - n0
        for take in range(2 if double else 1):
            r = np.random.default_rng([int(rng.integers(1 << 30)), take, n0])
            di = _render_strings(ph, n0, m, take, p, r, double)
            y = _amp(di, p, r)
            fade = min(int(0.3 * SR), m)
            y[-fade:] *= np.linspace(1, 0, fade) ** 2
            if double:
                out[take, n0:n1] += y
            else:
                out[0, n0:n1] += y
                out[1, n0:n1] += y
    return out * float(p["level"])
