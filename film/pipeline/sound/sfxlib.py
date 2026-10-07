"""Shared pieces for the film's sound-design layer: reading id's WAVs, resampling, loudness.

Everything is float64 at 48 kHz, shaped (2, n). The film's synthesizer (music/synth/dsp.py)
supplies filters, reverb, loudness and the limiter; this module imports it. The sounds live
under FILM_ROOT/sound/: id's in id/ (extract_id.py), the designed ones in designed/ (design.py).
"""
from __future__ import annotations

import json
import struct
import sys
from functools import lru_cache
from math import gcd
from pathlib import Path

import numpy as np
from scipy import signal

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "music" / "synth"))

import dsp  # noqa: E402  (the score's DSP: svf, make_ir, loudness, limiter, write_wav24)
from filmroot import FILM  # noqa: E402

SR = dsp.SR
SOUND = FILM / "sound"
ID_DIR = SOUND / "id"
DESIGNED_DIR = SOUND / "designed"


def read_wav_any(path: Path) -> tuple[np.ndarray, int, dict]:
    """A RIFF WAV of 8-bit unsigned, 16- or 24-bit PCM, any rate -> ((ch, n) float -1..1, rate, info).
    id's files carry 'cue ' and 'LIST' chunks after the data; they are skipped (info keeps the loop)."""
    b = Path(path).read_bytes()
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE", f"{path}: not a RIFF WAVE"
    i, info, data = 12, {}, None
    while i + 8 <= len(b):
        cid, n = b[i:i + 4], struct.unpack_from("<I", b, i + 4)[0]
        body = b[i + 8:i + 8 + n]
        if cid == b"fmt ":
            fmt, ch, rate, _, _, bits = struct.unpack_from("<HHIIHH", body)
            info.update(format=fmt, channels=ch, rate=rate, bits=bits)
        elif cid == b"data":
            data = body
        elif cid == b"cue " and len(body) >= 28:
            info["loop_start"] = struct.unpack_from("<I", body, 24)[0]
        i += 8 + n + (n & 1)
    ch, bits = info["channels"], info["bits"]
    if bits == 8:
        x = (np.frombuffer(data, np.uint8).astype(np.float64) - 128.0) / 128.0
    elif bits == 16:
        x = np.frombuffer(data[: len(data) // 2 * 2], "<i2").astype(np.float64) / 32768.0
    elif bits == 24:
        raw = np.frombuffer(data[: len(data) // 3 * 3], np.uint8).reshape(-1, 3)
        v = (raw[:, 0].astype(np.int32) | (raw[:, 1].astype(np.int32) << 8) | (raw[:, 2].astype(np.int32) << 16))
        v = np.where(v >= 1 << 23, v - (1 << 24), v)
        x = v.astype(np.float64) / float(1 << 23)
    else:
        raise ValueError(f"{path}: {bits}-bit PCM not handled")
    x = x[: len(x) // ch * ch].reshape(-1, ch).T
    return x, info["rate"], info


def resample(x: np.ndarray, sr_in: int, sr_out: int = SR) -> np.ndarray:
    """Polyphase resampling with a long Kaiser filter (clean: no imaging above id's 5.5 kHz)."""
    if sr_in == sr_out:
        return x
    g = gcd(sr_in, sr_out)
    up, down = sr_out // g, sr_in // g
    return signal.resample_poly(x, up, down, axis=-1, window=("kaiser", 9.0))


def varispeed(x: np.ndarray, rate: float) -> np.ndarray:
    """Play x at `rate` times its speed (tape-style: pitch and length change together)."""
    if abs(rate - 1.0) < 1e-6:
        return x
    # rational approximation of the rate, then polyphase
    from fractions import Fraction
    fr = Fraction(rate).limit_denominator(400)
    return signal.resample_poly(x, fr.denominator, fr.numerator, axis=-1, window=("kaiser", 9.0))


@lru_cache(maxsize=None)
def _load_cached(path: str) -> tuple[np.ndarray, dict]:
    x, sr, info = read_wav_any(Path(path))
    if x.shape[0] == 1:
        x = np.vstack([x[0], x[0]])
    x = resample(x[:2], sr)
    info = dict(info, source_rate=sr)
    if "loop_start" in info:
        info["loop_start_48k"] = int(round(info["loop_start"] * SR / sr))
    x.setflags(write=False)
    return x, info


def load(path: Path) -> tuple[np.ndarray, dict]:
    """Any WAV as (2, n) at 48 kHz (a copy) and its info."""
    x, info = _load_cached(str(Path(path).resolve()))
    return x.copy(), info


def momentary_max(x: np.ndarray) -> float:
    """Loudest 400 ms (momentary loudness, BS.1770), in LUFS; short sounds are padded to 400 ms."""
    x = dsp.stereo(x)
    if x.shape[1] < dsp.nsamp(0.4):
        x = dsp.fit(x, dsp.nsamp(0.4))
    xk = dsp._kweight(x)
    z = dsp._block_power(xk, 0.4, 0.02)
    return float(-0.691 + 10 * np.log10(max(z.max(), 1e-20)))


def designed_index() -> dict:
    p = DESIGNED_DIR / "index.json"
    return {e["name"]: e for e in json.loads(p.read_text())} if p.exists() else {}


# ---------------------------------------------------------------- finding a sound inside the game's mix

_BAND = signal.butter(4, [250.0, 4000.0], btype="bandpass", fs=SR, output="sos")
MATCH_R = 0.7          # band-passed normalised correlation. Tested on the footage: true matches 0.70-1.00;
                       # unrelated sounds mostly <= 0.4, a few 0.60-0.69 (noisy bursts against noisy bursts)


def band(x: np.ndarray) -> np.ndarray:
    """250 Hz-4 kHz: where id's 11 kHz samples and the engine's own resampling agree."""
    return signal.sosfilt(_BAND, x)


def onset(x: np.ndarray, rel: float = 0.05) -> int:
    a = np.abs(x)
    idx = np.nonzero(a > rel * (a.max() + 1e-12))[0]
    return int(idx[0]) if idx.size else 0


def ncc(game_b: np.ndarray, tmpl_b: np.ndarray) -> np.ndarray:
    """Normalised cross-correlation of a (band-passed) template along a (band-passed) signal."""
    if game_b.size < tmpl_b.size or not np.any(tmpl_b):
        return np.zeros(0)
    c = signal.fftconvolve(game_b, tmpl_b[::-1], mode="valid")
    e = np.concatenate([[0.0], np.cumsum(game_b * game_b)])
    win = e[tmpl_b.size:] - e[:-tmpl_b.size]
    return c / (np.sqrt(np.maximum(win, 1e-12)) * np.linalg.norm(tmpl_b))


def templates(sound_mono: np.ndarray) -> list[tuple[np.ndarray, int]]:
    """Band-passed templates of 80 ms and 250 ms from the sound's audible onset, with that onset.
    The short one finds sounds the engine cut off (a channel restarted: the nailgun every 0.1 s)."""
    o = onset(sound_mono)
    b = band(sound_mono)
    return [(b[o:o + dsp.nsamp(L)], o) for L in (0.08, 0.25) if sound_mono.size - o > dsp.nsamp(L) // 2]


def find_near(game_mono: np.ndarray, at: int, sound_mono: np.ndarray, slack_s: float = 0.35) -> tuple[float, float]:
    """Is the sound in the game's mix, starting within slack_s of sample `at`? (best r, offset s)"""
    best, off = 0.0, 0.0
    for tm, o in templates(sound_mono):
        sl = dsp.nsamp(slack_s)
        a, b = max(0, at + o - sl), min(game_mono.size, at + o + sl + tm.size)
        r = ncc(band(game_mono[a:b]), tm)
        if r.size and r.max() > best:
            k = int(np.argmax(r))
            best, off = float(r[k]), (a + k - o - at) / SR
    return best, off


def find_all(game_mono: np.ndarray, sound_mono: np.ndarray, r_min: float = MATCH_R) -> list[tuple[float, float]]:
    """Every place the sound starts in the game's mix: [(seconds, r)], at least 0.2 s apart."""
    gb = band(game_mono)
    hits: dict[int, float] = {}
    for tm, o in templates(sound_mono):
        r = ncc(gb, tm)
        if not r.size:
            continue
        for k in signal.find_peaks(r, height=r_min, distance=dsp.nsamp(0.2))[0]:
            s = k - o
            near = [h for h in hits if abs(h - s) < dsp.nsamp(0.05)]
            if near:
                hits[near[0]] = max(hits[near[0]], float(r[k]))
            else:
                hits[s] = float(r[k])
    return sorted((s / SR, r) for s, r in hits.items())
