#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.14"
# dependencies = ["numpy", "scipy", "numba"]
# ///
"""Render a .score file to a 48 kHz, 24-bit stereo WAV (plus a .json sidecar).

    uv run film/pipeline/music/synth/render.py film/pipeline/music/score-v10.score --stems --midi

  -o PATH            output WAV (default: FILM_ROOT/music/NAME.wav, NAME the score's)
  --voice PATH       narration (any format ffmpeg reads); the score ducks under it
  --voice-at SEC     where the narration starts in the cue (default 0)
  --preview PATH     also write music + narration together, to check the balance
  --stems [DIR]      one WAV per track group (`group=` on the track lines), each with its
                     own reverb; the stems sum exactly to the mix (same gain, same limiter).
                     Default DIR: FILM_ROOT/music/stems-NAME (without "score-": stems-v10)
  --midi [PATH]      write a Standard MIDI File of the score (default FILM_ROOT/music/NAME.mid)
  --solo a,b         render only these tracks
  --list             print the expanded notes with their times and stop
  --patches          describe the instruments and their parameters and stop

The film's whole score takes minutes; example-cue.score (16 s) takes seconds. The format is
described at the top of score.py; `--patches` lists the instruments.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from filmroot import FILM  # noqa: E402

import numpy as np  # noqa: E402
from scipy.ndimage import maximum_filter1d  # noqa: E402

from dsp import (SR, butter, convolve_reverb, db, dc_block, follow, limiter_gain,  # noqa: E402
                 loudness, make_ir, nsamp, pan_gains, pingpong, read_audio, rms_env,
                 short_term_curve, stereo, svf, tail_fade, to_db, true_peak, write_wav24)
from patches import PATCHES, describe  # noqa: E402
from score import Score, ScoreError  # noqa: E402


def log(msg):
    print(msg, file=sys.stderr, flush=True)


# ---------------------------------------------------------------- automation curves

def curve(points, n, logscale=False):
    """Per-sample values from (sec, value) points: straight lines between, held at the ends."""
    if not points:
        return None
    ts = np.array([p[0] for p in points])
    vs = np.array([p[1] for p in points], dtype=float)
    ts = ts + np.arange(ts.size) * 1e-9          # equal times: a step, kept in order
    t = np.arange(n) / SR
    if logscale:
        return 2.0 ** np.interp(t, ts, np.log2(np.maximum(vs, 1.0)))
    return np.interp(t, ts, vs)


def apply_cutoff(x, pts, n):
    c = curve(pts, n, logscale=True)
    if c is None:
        return x
    return svf(svf(x, c, 0.05), c, 0.05)          # 24 dB/octave


# ---------------------------------------------------------------- tracks

def render_track(score, name, notes, n):
    tr = score.tracks[name]
    fn, defaults, _, mono = PATCHES[tr.patch]
    p = dict(defaults)
    p.update(tr.params)
    x = np.zeros((2, n))
    if mono == "notes":   # a whole-track patch that also reads each note's own parameters (guitar2)
        rng = np.random.default_rng(notes[0].seed)
        full = []
        for nt in notes:
            q = dict(p)
            q.update(nt.params)
            q.pop("pan", None)
            full.append(dict(t0=nt.t0, t1=nt.t1, freq=nt.freq or 110.0, vel=nt.vel, p=q))
        return stereo(fn(full, n, p, rng))
    if mono:
        rng = np.random.default_rng(notes[0].seed)
        mono_notes = [(nt.t0, nt.t1, nt.freq or 220.0, nt.vel) for nt in notes]
        return stereo(fn(mono_notes, n, p, rng))
    ht, hv = tr.mix["human_t"] / 1000.0, tr.mix["human_v"]
    for nt in notes:
        q = dict(p)
        q.update(nt.params)
        note_pan = q.pop("pan", None)
        rng = np.random.default_rng(nt.seed)
        t0, vel = nt.t0, nt.vel
        if ht or hv:     # a player's hands: a little early or late, a little soft or hard
            hr = np.random.default_rng(nt.seed ^ 0x5EED)
            t0 = t0 + float(np.clip(hr.normal(0, ht), -3 * ht, 3 * ht)) if ht else t0
            vel = float(np.clip(vel * (1 + hr.normal(0, hv)), 0.05, 1.2)) if hv else vel
        try:
            a = stereo(fn(nt.freq, max(nt.t1 - nt.t0, 1e-3), vel, q, rng))
        except Exception as e:  # report which note broke
            raise RuntimeError(f"patch {tr.patch!r}, note on line {nt.line} at {nt.t0:.3f}s: {e}") from e
        a = tail_fade(a.copy(), 0.003)   # no note ends on a step
        if note_pan is not None:
            gl, gr = pan_gains(note_pan)
            a = a * np.array([[gl], [gr]])
        s0 = int(round(t0 * SR))
        if s0 < 0:
            a, s0 = a[:, -s0:], 0
        m = min(a.shape[1], n - s0)
        if m > 0:
            x[:, s0:s0 + m] += a[:, :m]
    return x


def process_track(score, name, x, n):
    tr = score.tracks[name]
    if tr.mix["hp"]:
        x = butter(x, "highpass", tr.mix["hp"], 2)
    if tr.mix["lp"]:
        x = butter(x, "lowpass", tr.mix["lp"], 2)
    if tr.mix["eq_lo"] or tr.mix["eq_mid"] or tr.mix["eq_hi"]:
        lo = butter(x, "lowpass", 250.0, 2)
        hi = butter(x, "highpass", 4000.0, 2)
        x = db(tr.mix["eq_lo"]) * lo + db(tr.mix["eq_mid"]) * (x - lo - hi) + db(tr.mix["eq_hi"]) * hi
    x = apply_cutoff(x, score.automation(name, "cutoff"), n)
    w = curve(score.automation(name, "width"), n)
    wk = tr.mix["width"] * (w if w is not None else 1.0)
    if np.any(np.asarray(wk) != 1.0):
        mid, side = 0.5 * (x[0] + x[1]), 0.5 * (x[0] - x[1]) * wk
        x = np.vstack([mid + side, mid - side])
    pc = curve(score.automation(name, "pan"), n)
    if pc is None:
        gl, gr = pan_gains(tr.mix["pan"])
        x[0] *= gl
        x[1] *= gr
    else:
        ang = (np.clip(pc + tr.mix["pan"], -1, 1) + 1.0) * np.pi / 4.0
        x[0] *= np.sqrt(2) * np.cos(ang)
        x[1] *= np.sqrt(2) * np.sin(ang)
    g = db(tr.mix["gain"])
    lv = curve(score.automation(name, "level"), n)
    if lv is not None:
        g = g * db(lv)
    gn = curve(score.automation(name, "gain"), n)
    if gn is not None:
        g = g * gn
    x = x * g
    for ch, key in ((0, "gainl"), (1, "gainr")):
        c = curve(score.automation(name, key), n)
        if c is not None:
            x[ch] *= c
    return x


# ---------------------------------------------------------------- ducking

def duck_activity(voice, n, at_s, d):
    v = np.zeros(n)
    s0 = nsamp(at_s) if at_s > 0 else 0
    m = min(voice.size, n - s0)
    if m > 0:
        v[s0:s0 + m] = voice[:m]
    env = to_db(rms_env(v, 0.03))
    act = np.clip((env - d["threshold"]) / 6.0, 0.0, 1.0)
    act = maximum_filter1d(act, size=nsamp(d["hold"]))
    la = nsamp(d["lookahead"])
    act = np.concatenate([act[la:], np.zeros(la)])
    return np.clip(follow(act, d["attack"], d["release"]), 0.0, 1.0)


def apply_duck(x, r, depth_db, mid_db):
    gb = db(-depth_db * r)
    if mid_db <= 0:
        return x * gb
    gm = db(-mid_db * r)
    mid = butter(x, "bandpass", [250.0, 4000.0], 2)
    return x * gb - mid * gb * (1.0 - gm)


# ---------------------------------------------------------------- mix

def mix_groups(score, n, irs, solo=None, duck_r=None):
    """Render the tracks one at a time into per-group buses; return {group: stem (2, n)}."""
    by_track = {}
    for nt in score.notes:
        by_track.setdefault(nt.track, []).append(nt)
    buses = {}
    d = score.duck
    t0 = time.time()
    count = 0
    dry = curve(score.automation("master", "dry"), n)   # 0..1 on every track before its sends: a stop whose reverb rings
    dry = None if dry is None else dry.astype(np.float32)
    for name, tr in score.tracks.items():
        notes = [nt for nt in by_track.get(name, []) if score.length is None or nt.t0 < score.length]
        if not notes or tr.mix["mute"] or (solo and name not in solo):
            continue
        x = render_track(score, name, notes, n)
        x = process_track(score, name, x, n)
        if duck_r is not None and tr.mix["duck"] > 0:
            x = apply_duck(x, duck_r, d["depth"] * tr.mix["duck"], d["mid"] * tr.mix["duck"])
        if dry is not None:
            x = x * dry
        g = tr.mix["group"] + ("|post" if tr.mix["bypass"] else "")   # bypass: around the master automation
        if g not in buses:
            buses[g] = {k: np.zeros((2, n), dtype=np.float32) for k in ("dry", "hall", "room")}
        b = buses[g]
        b["dry"] += x
        for k in ("hall", "room"):
            if tr.mix[k] > 0:
                b[k] += x * tr.mix[k]
        if tr.mix["delay"] > 0:
            b["dry"] += pingpong(x * tr.mix["delay"], tr.mix["delay_time"], tr.mix["delay_fb"])
        count += len(notes)
        del x
    log(f"  rendered {count} notes on {len(score.tracks)} tracks in {time.time() - t0:.1f}s")
    stems = {}
    for g, b in buses.items():
        y = b.pop("dry").astype(np.float64)
        for k in ("hall", "room"):
            bus = b.pop(k)
            if np.any(bus):
                wet = convolve_reverb(bus.astype(np.float64), irs[k])
                if duck_r is not None:
                    wet = apply_duck(wet, duck_r, d["depth"], d["mid"])
                y += wet
        stems[g] = y
    return stems


def varispeed(y, a, b, pts):
    """Between a and b the audio plays at a varying speed (tape-like: pitch follows), read from
    the timeline starting at a. Speed 0 is silence. After b, the timeline again."""
    n = y.shape[1]
    s0, s1 = int(round(a * SR)), min(n, int(round(b * SR)))
    if s0 >= n or s1 <= s0:
        return y
    tt = np.arange(s0, s1) / SR
    sp = np.interp(tt, [p[0] for p in pts], [p[1] for p in pts])
    pos = s0 + np.concatenate([[0.0], np.cumsum(sp[:-1])])
    seg = np.vstack([np.interp(pos, np.arange(n), c) for c in y])
    gate = follow(np.clip(sp / 0.04, 0.0, 1.0), 0.002, 0.004)      # stopped tape is silent
    out = y.copy()
    out[:, s0:s1] = seg * gate
    return out


def master_chain(score, stems, n_out):
    """Master automation, DC block and fades: linear, so applied to every stem alike."""
    n = next(iter(stems.values())).shape[1]
    cut = score.automation("master", "cutoff")
    lv = curve(score.automation("master", "level"), n)
    gn = curve(score.automation("master", "gain"), n)
    t = np.arange(n) / SR
    fade = np.ones(n)
    if score.fadein > 0:
        fade *= np.clip(t / score.fadein, 0, 1) ** 2
    if score.length is not None and score.fadeout > 0:
        fade *= np.clip((score.length - t) / score.fadeout, 0, 1) ** 2
    if lv is not None:
        fade *= db(lv)
    if gn is not None:
        fade *= gn
    pocket = curve(score.automation("master", "pocket"), n)
    mwidth = curve(score.automation("master", "width"), n)
    endfade = np.ones(n)
    if score.length is not None and score.fadeout > 0:
        endfade = np.clip((score.length - t) / score.fadeout, 0, 1) ** 2
    out = {}
    for g, y in stems.items():
        if g.endswith("|post"):          # tracks that bypass the master's automation (a gag's punchline)
            y = dc_block(y, 10.0) * endfade
        else:
            y = dc_block(y, 10.0)             # before the varispeed, so a stopped tape stays silent
            for (a_, b_, pts_) in score.tapestops:
                y = varispeed(y, a_, b_, pts_)
            y = apply_cutoff(y, cut, n)
            if mwidth is not None:            # the stereo field, as a whole
                mid, side = 0.5 * (y[0] + y[1]), 0.5 * (y[0] - y[1]) * mwidth
                y = np.vstack([mid + side, mid - side])
            if pocket is not None and np.any(pocket):
                mid = butter(y, "bandpass", [250.0, 4000.0], 2)
                y = y - mid * (1.0 - db(-pocket))
            y = y * fade
        y = tail_fade(y[:, :n_out].copy(), 0.01)
        key = g.split("|")[0]
        out[key] = out[key] + y if key in out else y
    return out


def section_loudness(score, x):
    secs_, _ = score.markers()
    bounds = [t for t, _ in secs_] + [x.shape[1] / SR]
    rows = []
    for (t0, name), t1 in zip(secs_, bounds[1:]):
        seg = x[:, int(t0 * SR):int(t1 * SR)]
        if seg.shape[1] < SR:
            continue
        integ = loudness(seg)[0]
        ts, ls = short_term_curve(seg)
        rows.append({"section": name, "from": round(t0, 3), "to": round(t1, 3),
                     "integrated_lufs": round(integ, 1) if np.isfinite(integ) else None,
                     "short_term_max": round(float(ls.max()), 1) if ls.size else None})
    return rows


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("score", nargs="?")
    ap.add_argument("-o", "--out")
    ap.add_argument("--voice")
    ap.add_argument("--voice-at", type=float, default=0.0)
    ap.add_argument("--preview")
    ap.add_argument("--stems", nargs="?", const="")
    ap.add_argument("--midi", nargs="?", const="")
    ap.add_argument("--solo")
    ap.add_argument("--window", nargs=2, type=float, metavar=("FROM", "TO"),
                    help="write only this stretch of the timeline (a cue to audition)")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--patches", action="store_true")
    a = ap.parse_args()

    if a.patches:
        print(describe())
        return
    if not a.score:
        ap.error("a score file is required")
    try:
        score = Score.load(a.score)
    except ScoreError as e:
        sys.exit(f"{a.score}: {e}")

    if a.list:
        for nt in score.notes:
            f = f"{nt.freq:8.2f}Hz" if nt.freq else "       -  "
            print(f"{nt.t0:9.4f}s {nt.t1 - nt.t0:7.3f}s  {nt.track:10s} {f} vel {nt.vel:.2f}  "
                  f"{' '.join(f'{k}={v}' for k, v in nt.params.items())}")
        secs_, hits = score.markers()
        for t, name in secs_:
            print(f"{t:9.4f}s  SECTION {name}")
        for t, name in hits:
            print(f"{t:9.4f}s  HIT {name}")
        return

    t_start = time.time()
    name = Path(a.score).stem
    out = Path(a.out) if a.out else FILM / "music" / f"{name}.wav"
    if a.stems == "":
        a.stems = str(FILM / "music" / f"stems-{name.removeprefix('score-')}")   # stems-v10
    if a.midi == "":
        a.midi = str(FILM / "music" / f"{name}.mid")
    solo = set(a.solo.split(",")) if a.solo else None
    for line in score.tempo_report():
        log("  tempo: " + line)

    hall_t60 = score.reverbs["hall"]["t60"]
    last = max((nt.t1 for nt in score.notes), default=0.0)
    n_out = nsamp(score.length) if score.length is not None else nsamp(last + hall_t60 + 3.0)
    n = n_out + nsamp(0.05)
    irs = {b: make_ir(**score.reverbs[b]) for b in ("hall", "room")}

    stems = master_chain(score, mix_groups(score, n, irs, solo), n_out)
    pre = sum(stems.values())

    # gain: hit the loudness target on the un-ducked score (or use `master`)
    if score.loudness is not None:
        gain_db = score.loudness - loudness(pre)[0]
        for _ in range(4):
            g, _gr = limiter_gain(pre * db(gain_db), score.ceiling)
            err = score.loudness - loudness(pre * db(gain_db) * g)[0]
            if abs(err) < 0.05:
                break
            gain_db += err
    else:
        gain_db = score.master

    voice = None
    if a.voice:
        voice = read_audio(a.voice, mono=True)
        duck_r = duck_activity(voice, n, a.voice_at, score.duck)
        stems = master_chain(score, mix_groups(score, n, irs, solo, duck_r), n_out)
        pre = sum(stems.values())
    if a.window:
        w0, w1 = int(a.window[0] * SR), int(a.window[1] * SR)
        stems = {k_: v[:, w0:w1] for k_, v in stems.items()}
        pre = sum(stems.values())
        if score.loudness is not None:
            gain_db = score.loudness - loudness(pre)[0]
    g, gr = limiter_gain(pre * db(gain_db), score.ceiling)
    k = db(gain_db) * g
    stems = {name: y * k for name, y in stems.items()}
    final = sum(stems.values())
    del pre

    out.parent.mkdir(parents=True, exist_ok=True)
    write_wav24(out, final)

    integ, lra, stmax = loudness(final)
    secs_, hits = score.markers()
    wav = out.resolve()
    meta = {
        "title": score.title, "score": Path(a.score).name,
        "wav": str(wav.relative_to(FILM)) if wav.is_relative_to(FILM) else wav.name,
        "tuning": {"midi": score.tuning[0], "hz": score.tuning[1]},
        "sample_rate": SR, "samples": int(final.shape[1]), "seconds": final.shape[1] / SR,
        "sections": [{"name": nm, "t": round(t, 6), "sample": int(round(t * SR))} for t, nm in secs_],
        "hits": [{"name": nm, "t": round(t, 6), "sample": int(round(t * SR))} for t, nm in hits],
        "tempo": score.tempo_report(),
        "gain_db": round(gain_db, 2), "limiter_max_gr_db": round(gr, 2),
        "loudness_lufs": round(integ, 2), "lra_lu": round(lra, 2), "short_term_max_lufs": round(stmax, 2),
        "true_peak_dbtp": round(true_peak(final), 2),
        "sample_peak_dbfs": round(float(to_db(np.abs(final).max())), 2),
        "dc_offset": [float(f"{v:.2e}") for v in final.mean(axis=1)],
        "section_loudness": section_loudness(score, final),
        "stems": sorted(stems) if a.stems else None,
        "ducked_under": a.voice, "voice_at": a.voice_at if a.voice else None,
    }
    out.with_suffix(".json").write_text(json.dumps(meta, indent=2) + "\n")

    if a.stems:
        sd = Path(a.stems)
        sd.mkdir(parents=True, exist_ok=True)
        for name, y in stems.items():
            write_wav24(sd / f"{name}.wav", y)
        log(f"  stems: {', '.join(sorted(stems))} (max |sum - mix| = "
            f"{np.abs(sum(stems.values()) - final).max():.1e})")
    if a.midi:
        from midi import write_midi
        write_midi(score, a.midi)
    if a.preview and voice is not None:
        v = np.zeros(n_out)
        s0 = nsamp(a.voice_at) if a.voice_at > 0 else 0
        m = min(voice.size, n_out - s0)
        v[s0:s0 + m] = voice[:m]
        pv = final + stereo(v)
        gp, _ = limiter_gain(pv, -1.0)
        write_wav24(a.preview, pv * gp)

    log(f"  {out}: {meta['seconds']:.3f}s, {integ:.1f} LUFS, true peak {meta['true_peak_dbtp']} dBTP, "
        f"limiter {gr:.1f} dB, gain {gain_db:+.1f} dB, {time.time() - t_start:.1f}s")


if __name__ == "__main__":
    main()
