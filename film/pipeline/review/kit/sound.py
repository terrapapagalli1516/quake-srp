"""The sound: loudness over time (EBU R128), the buses (stems) separately, the voice against its bed,
onsets, and the flags: clipping, dropouts, silences, sound inside a marked silence, clicks and steps at
cuts, and hits too quiet to hear under what plays with them."""

from __future__ import annotations

import json
import re
import subprocess
import threading
from pathlib import Path

import numpy as np
from scipy import ndimage, signal

from common import CACHE, FILM, KIT, Flags, ensure_dir, log, md_table, rel, tc

SR = 48000
BUSES = ("voice", "music", "game", "sfx")
K_SOS = np.array([[1.53512485958697, -2.69169618940638, 1.19839281085285, 1.0, -1.69065929318241, 0.73248077421585],
                  [1.0, -2.0, 1.0, 1.0, -1.99004745483398, 0.99007225036621]])   # BS.1770 K-weighting at 48 kHz


# ----------------------------------------------------------------- reading ----

def decode_audio(cut: Path) -> np.ndarray:
    r = subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(cut), "-vn", "-ac", "2", "-ar", str(SR),
                        "-f", "f32le", "-"], capture_output=True, check=True)
    return np.frombuffer(r.stdout, np.float32).reshape(-1, 2)


def ebur128_summary(cut: Path) -> dict:
    r = subprocess.run(["ffmpeg", "-hide_banner", "-nostats", "-nostdin", "-i", str(cut), "-map", "0:a",
                        "-af", "ebur128=peak=true", "-f", "null", "-"], capture_output=True, text=True)
    t = r.stderr[r.stderr.rfind("Summary:"):]

    def get(pat):
        m = re.search(pat, t, re.S)
        return float(m.group(1)) if m else None
    return {"integrated": get(r"I:\s+(-?[\d.]+) LUFS"), "lra": get(r"LRA:\s+(-?[\d.]+) LU"),
            "true_peak": get(r"Peak:\s+(-?[\d.]+) dBFS")}


def loud_curve(x: np.ndarray, win_s: float, hop_s: float = 0.1) -> tuple[np.ndarray, np.ndarray]:
    """Loudness (LUFS, ungated) of windows of win_s every hop_s; times are the windows' centres."""
    y = signal.sosfilt(K_SOS, x.astype(np.float64), axis=0)
    ms = (y ** 2).sum(axis=1) if y.ndim > 1 else 2 * y ** 2
    del y
    win, hop = int(win_s * SR), int(hop_s * SR)
    c = np.concatenate([[0.0], np.cumsum(ms)])
    ends = np.arange(win, len(ms) + 1, hop)
    m = (c[ends] - c[ends - win]) / win
    return (ends - win / 2) / SR, -0.691 + 10 * np.log10(m + 1e-12)


def lufs(x: np.ndarray) -> float:
    if len(x) < SR // 10:
        return -99.0
    y = signal.sosfilt(K_SOS, x.astype(np.float64), axis=0)
    ms = float(((y ** 2).sum(axis=1) if y.ndim > 1 else 2 * y ** 2).mean())
    return -0.691 + 10 * np.log10(ms + 1e-12)


def env_db(x: np.ndarray, win: int = 480, hop: int = 480) -> np.ndarray:
    """RMS in dBFS of windows (default 10 ms, no overlap); x mono."""
    n = (len(x) - win) // hop + 1
    if n <= 0:
        return np.array([-120.0])
    idx = np.arange(n) * hop
    c = np.concatenate([[0.0], np.cumsum(x.astype(np.float64) ** 2)])
    ms = (c[idx + win] - c[idx]) / win
    return 10 * np.log10(ms + 1e-14)


def rms_db(x: np.ndarray) -> float:
    if len(x) == 0:
        return -120.0
    return float(10 * np.log10(np.mean(x.astype(np.float64) ** 2) + 1e-14))


def mono(x: np.ndarray) -> np.ndarray:
    return x.mean(axis=1) if x.ndim > 1 else x


# ------------------------------------------------------------------ stems ----

def stems_for(edit, out: Path, derive: bool) -> tuple[dict, str]:
    """The four buses: written by the build (BUILD/stems/<bus>.wav|flac), else re-made by the build's own
    mixer (derive_stems.py, cached per build), else none."""
    import soundfile as sf
    if edit.build:
        for d in (edit.build / "stems", edit.build):
            got = {}
            for b in BUSES:
                for ext in ("flac", "wav"):
                    p = d / f"{b}.{ext}" if d.name == "stems" else d / f"stem-{b}.{ext}"
                    if p.exists():
                        got[b] = p
            if len(got) >= 2:
                return {b: sf.read(str(p), dtype="float32", always_2d=True)[0] for b, p in got.items()}, \
                    f"written by the build: `{rel(d)}`"
    if not edit.build or not edit.config_path:
        return {}, "no stems: the build writes none, and without its build folder they cannot be re-made"
    key = f"{edit.build.parent.name}-{edit.build.name}-{int((edit.build / 'film.mp4').stat().st_mtime) if (edit.build / 'film.mp4').exists() else 0}"
    cdir = CACHE / "stems" / key
    if not (cdir / "meta.json").exists() and not derive:
        return {}, "no stems: the build writes none (and they were not re-made: --no-derive-stems or a part that does not need them)"
    if not (cdir / "meta.json").exists():
        log(f"stems: re-making the buses with build.py's own mixer ({rel(edit.build)})")
        r = subprocess.run(["uv", "run", "-q", str(KIT / "derive_stems.py"), str(edit.build), str(edit.config_path), str(cdir)],
                           capture_output=True, text=True, cwd=str(FILM))
        if r.returncode != 0 or not (cdir / "meta.json").exists():
            log("stems: failed:", r.stderr[-800:])
            return {}, "no stems: re-making them failed (" + (r.stderr.strip().splitlines() or ["?"])[-1][:200] + ")"
    meta = json.loads((cdir / "meta.json").read_text())
    global STEMS_DIR
    STEMS_DIR = cdir
    st = {}
    for b in BUSES:
        p = cdir / f"{b}.wav"
        if p.exists():
            st[b] = sf.read(str(p), dtype="float32", always_2d=True)[0]
    how = (f"re-made by the edit's own mix_audio() (build.py) from `{rel(edit.build)}` (its timeline, its frozen sources, "
           f"SFX `{meta.get('sfx')}`, master gain {meta.get('master_gain_db')} dB), cached in `{cdir}`")
    if meta.get("notes"):
        how += "; " + "; ".join(meta["notes"])
    return st, how


def stems_check(mix: np.ndarray, st: dict) -> dict:
    """How well the buses add up to the cut's own sound (the limiter and the AAC encode are the rest)."""
    if not st:
        return {}
    n = min(len(mix), *(len(v) for v in st.values()))
    s = sum(mono(v[:n]) for v in st.values())
    m = mono(mix[:n])
    # the encoder's delay, if any: the best lag within +-2048 samples, on a loud stretch
    i = int(np.argmax(np.convolve(np.abs(m[::480]), np.ones(100), "same"))) * 480
    a, b = max(4096, i - SR), min(n - 4096, i + SR)
    lags = np.arange(-2048, 2049, 4)
    best = int(max(lags, key=lambda L: float(np.dot(m[a:b], s[a - L:b - L]))))
    if best:
        s = np.roll(s, best)
    res = rms_db(m - s) - rms_db(m)
    # where they do not: 0.25 s windows, each after its own best gain (the master limiter is a slow gain), whose
    # remainder is still within 12 dB of the cut's own level: the buses there are not the cut's
    w = SR // 4
    k = n // w
    mm, ss = m[: k * w].reshape(k, w).astype(np.float64), s[: k * w].reshape(k, w).astype(np.float64)
    pm, ps = (mm ** 2).sum(1), (ss ** 2).sum(1)
    g = (mm * ss).sum(1) / (ps + 1e-12)
    rem = ((mm - g[:, None] * ss) ** 2).sum(1)
    rdb = 10 * np.log10(rem / (pm + 1e-12) + 1e-12)
    level = 10 * np.log10(pm / w + 1e-14)
    bad = np.where((rdb > -12) & (level > -50))[0]
    spans: list[list[float]] = []
    for i in bad:
        t0 = i * 0.25
        if spans and t0 - spans[-1][1] <= 0.5:
            spans[-1][1] = t0 + 0.25
            spans[-1][2] = max(spans[-1][2], float(rdb[i]))
        else:
            spans.append([t0, t0 + 0.25, float(rdb[i])])
    return {"lag_samples": best, "residual_db": round(res, 1), "mismatch": spans}


# ----------------------------------------------------------------- onsets ----

class Onsets:
    """Rises in level (2.5 ms hop, 5 ms windows) of one signal, for finding where a hit lands."""
    HOP = 120

    SOS_HF = signal.butter(4, 4000, "highpass", fs=SR, output="sos")

    def __init__(self, x: np.ndarray):
        m = mono(x)
        self.e = env_db(m, 240, self.HOP)
        # a rise in the whole signal, or in its top (above 4 kHz): a tick or a metal hit barely moves the
        # broadband level but jumps out of the highs
        hf = env_db(signal.sosfilt(self.SOS_HF, m.astype(np.float64)), 240, self.HOP)
        self.rise = np.maximum(self._rise(self.e), np.where(hf > -75, self._rise(hf), 0.0))

    @staticmethod
    def _rise(e: np.ndarray) -> np.ndarray:
        prev = ndimage.maximum_filter1d(e, size=6, origin=2)   # the loudest of the 6 frames before (15 ms)
        prev = np.concatenate([[e[0]] * 3, prev[:-3]])[:len(e)]
        return np.clip(e - prev, 0, None)

    def near(self, t: float, before: float = 0.08, after: float = 0.08, min_rise: float = 6.0) -> dict | None:
        k0 = max(0, int((t - before) * SR / self.HOP))
        k1 = min(len(self.e), int((t + after) * SR / self.HOP) + 1)
        if k1 <= k0:
            return None
        k = k0 + int(np.argmax(self.rise[k0:k1]))
        if self.rise[k] < min_rise or self.e[k] < -70:
            return None
        return {"t": (k * self.HOP + 120) / SR, "rise_db": float(self.rise[k]), "level_db": float(self.e[k])}


# ------------------------------------------------------------- the checks ----

def phrases(edit, hold: float = 0.7) -> list[list[float]]:
    sp = sorted(v["speech"] for v in (edit.timeline or {}).get("voice", []) if v.get("speech"))
    out: list[list[float]] = []
    for a, b in sp:
        if out and a - out[-1][1] < hold:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def marked_silences(edit, sfx_meta: dict | None) -> list[dict]:
    """Where the edit means silence: muted shots (every bus but the voice) and the sound designer's
    `silence` lines (sfx-vN.json: tails cut from FROM to TO)."""
    out = []
    for s in edit.shots:
        for w0, w1 in s.get("mutes") or ([[0.0, s["len"]]] if s.get("mute") else []):
            out.append({"from": s["start"] + w0, "to": s["start"] + w1, "kind": "mute", "where": f"{s['id']} mute",
                        "buses": ["music", "game", "sfx"]})
    for x in (sfx_meta or {}).get("silences", []):
        out.append({"from": float(x["from"]), "to": float(x["to"]), "kind": "sfx silence", "where": x.get("where", "sfx"),
                    "buses": ["sfx"]})
    return sorted(out, key=lambda w: w["from"])


STEMS_DIR: Path | None = None   # where the re-made buses came from (its sfx.json is the cue list of that SFX stem)


def find_sfx_meta(edit) -> tuple[dict | None, str]:
    """The SFX stem's sidecar (sound/sfx-vN.json): its placed cues and silences. The copy kept with the
    re-made buses first (the file may have been rewritten since), else a sidecar made on this cut's clock."""
    if STEMS_DIR is not None and (STEMS_DIR / "sfx.json").exists():
        return json.loads((STEMS_DIR / "sfx.json").read_text()), f"the cue list kept with the buses (`{STEMS_DIR / 'sfx.json'}`)"
    from common import pick_sfx
    tdir = str(edit.build.parent) if edit.build else (str(edit.timeline_path.parent) if edit.timeline_path else "")
    glob = ((edit.config or {}).get("sfx") or {}).get("glob")
    p, how = pick_sfx((edit.mix or {}).get("sfx"), tdir, edit.duration, glob)
    if p is None:
        return None, how
    return json.loads(p.with_suffix(".json").read_text()), how


def score_meta(edit) -> tuple[dict | None, str]:
    """The score's sidecar (music/score-vN.json beside the WAV the timeline plays): its named hits."""
    p = ((edit.timeline or {}).get("music") or {}).get("path")
    if not p or not (FILM / p).with_suffix(".json").exists():
        return None, "no score sidecar"
    j = (FILM / p).with_suffix(".json")
    meta = json.loads(j.read_text())
    if abs(float(meta.get("seconds", 0)) - edit.duration) > 0.1:
        return None, f"`{rel(j)}` lasts {meta.get('seconds')} s, not the film's {edit.duration:.2f}: rewritten since?"
    return meta, f"`{rel(j)}`"


def speech_mask(edit, words: list[dict] | None, n: int, pad: float = 0.06) -> np.ndarray:
    m = np.zeros(n, bool)
    spans = [v["speech"] for v in (edit.timeline or {}).get("voice", []) if v.get("speech")]
    spans += [[w["start"], w["end"]] for w in (words or [])]
    for a, b in spans:
        m[max(0, int((a - pad) * SR)): min(n, int((b + pad) * SR))] = True
    return m


def analyse(edit, out: Path, mix: np.ndarray, st: dict, stems_how: str, words: list[dict] | None,
            fl: Flags, silence_min: float = 0.3) -> dict:
    """Every sound check; returns the numbers the Markdown and the sync table need."""
    n = len(mix)
    res: dict = {"stems_how": stems_how}
    summ = {}
    th = threading.Thread(target=lambda: summ.update(ebur128_summary(edit.cut)))
    th.start()
    m1 = mono(mix)
    # clipping
    clipped = int((np.abs(mix) >= 0.9999).sum())
    peak = float(np.abs(mix).max())
    res["clipped_samples"], res["sample_peak"] = clipped, peak
    if clipped:
        k = np.where(np.abs(mix).max(axis=1) >= 0.9999)[0]
        fl.add("fix", k[0] / SR, f"{clipped} clipped samples (first at {tc(k[0] / SR, 3)}, last at {tc(k[-1] / SR, 3)})",
               "sound.md", t1=k[-1] / SR)
    # the buses
    sfx_meta, sfx_meta_path = find_sfx_meta(edit)
    res["sfx_meta_path"] = sfx_meta_path
    if st:
        res["stems_check"] = sc = stems_check(mix, st)
        mm = sc.get("mismatch") or []
        if mm:
            tot = sum(b - a for a, b, _ in mm)
            fl.add("look", mm[0][0], f"the buses do not match the cut's own sound for {tot:.1f} s in {len(mm)} places ("
                   + ", ".join(f"{tc(a)}–{tc(b)}" for a, b, _ in mm[:12]) + (" …" if len(mm) > 12 else "")
                   + "): a part file was rewritten since the build; bus-based findings there are unreliable", "sound.md")
    # silences, dropouts, marked silences
    e10 = env_db(m1, 480, 480)                      # 10 ms
    sil = e10 < -60
    marks = marked_silences(edit, sfx_meta)
    res["marks"] = marks
    res["silences"] = []
    from picture import runs
    for a, b in runs(sil):
        t0, t1 = a * 0.01, b * 0.01
        if t1 - t0 < 0.03:
            continue
        before = e10[max(0, a - 10):a]
        after = e10[b:b + 10]
        loud_sides = len(before) and len(after) and np.median(before) > -40 and np.median(after) > -40
        inside = [w for w in marks if w["from"] - 0.05 <= t0 and t1 <= w["to"] + 0.05]
        overlap = [w for w in marks if w["from"] < t1 and t0 < w["to"]]
        edge = t0 < 0.1 or t1 > edit.duration - 0.1
        rec = {"from": t0, "to": t1, "marked": bool(inside or overlap), "where": ", ".join(w["where"] for w in overlap)}
        res["silences"].append(rec)
        if t1 - t0 < silence_min:
            if loud_sides and not overlap and not edge:
                fl.add("fix", t0, f"a dropout: {1000 * (t1 - t0):.0f} ms of silence between sound on both sides", "--range", t1=t1)
            continue
        if overlap or edge:
            fl.add("info", t0, f"silence {t1 - t0:.2f} s, marked ({rec['where'] or 'the film edge'})", "sound.md", t1=t1)
        else:
            fl.add("look", t0, f"silence {t1 - t0:.2f} s that nothing marks as intended", "sound.md", t1=t1)
    # sound inside a marked silence: per bus, if the buses are known
    res["leaks"] = []
    sp = speech_mask(edit, words, n)
    for w in marks:
        a, b = int(w["from"] * SR), int(w["to"] * SR)
        if b <= a:
            continue
        srcs = {k: st[k] for k in w["buses"] if k in st} if st else {"mix (no voice)": mix}
        for name, x in srcs.items():
            seg = mono(x[a:b]).copy()
            if name.startswith("mix"):
                seg[sp[a:b]] = 0.0
            e = env_db(seg, 480, 480)
            on = e > -60
            for ra, rb in runs(on):
                if rb - ra < 3:
                    continue
                t0, t1 = w["from"] + ra * 0.01, w["from"] + rb * 0.01
                res["leaks"].append({"mark": w["where"], "bus": name, "from": t0, "to": t1, "peak_db": float(e[ra:rb].max())})
    # cues that a mute cuts in two: the mute hides their start, the rest plays after it
    res["cut_in_two"] = []
    if sfx_meta:
        mutes = [w for w in marks if w["kind"] == "mute"]
        for c in sfx_meta.get("cues", []):
            if c.get("status") != "placed":
                continue
            s0, s1 = float(c.get("start_t", c["film_t"])), float(c.get("end_t", c["film_t"]))
            for w in mutes:
                if w["from"] - 0.02 <= s0 < w["to"] and s1 > w["to"] + 0.05:
                    aud = None
                    if "sfx" in st:
                        aud = rms_db(mono(st["sfx"][int(w["to"] * SR): int(min(s1, w["to"] + 0.5) * SR)]))
                    res["cut_in_two"].append({"cue": c, "mute": w, "after_s": s1 - w["to"], "after_db": aud})
                    fl.add("fix", w["to"], f"the {c['sound']} cue ({c['where']}, {tc(s0, 3)}–{tc(s1, 3)}) is cut in two by "
                           f"{w['where']} ({tc(w['from'], 3)}–{tc(w['to'], 3)}): the mute hides its first {w['to'] - s0:.2f} s, "
                           f"its last {s1 - w['to']:.2f} s plays after" + (f" at {aud:.0f} dBFS" if aud is not None else ""),
                           "sound.md, --range", t1=s1)
    for lk in res["leaks"]:
        w = next(x for x in marks if x["where"] == lk["mark"])
        cues = [c for c in (sfx_meta or {}).get("cues", []) if c.get("status") == "placed"
                and float(c.get("start_t", 0)) < lk["to"] and float(c.get("end_t", 0)) > lk["from"]] if lk["bus"] == "sfx" else []
        lk["cues"] = [f"{c['sound']} ({c['where']})" for c in cues]
        started_inside = all(float(c.get("start_t", 0)) >= w["from"] - 0.02 for c in cues) if cues else False
        sev = "fix" if w["kind"] == "mute" else ("info" if started_inside and lk["bus"] == "sfx" and lk["from"] > w["from"] + 0.3
                                                 and not any(x["cue"] in cues for x in res["cut_in_two"]) else "look")
        fl.add(sev, lk["from"], f"{lk['bus']} audible inside the marked silence {tc(w['from'], 3)}–{tc(w['to'], 3)} "
               f"({w['where']}), {lk['peak_db']:.0f} dBFS peak" + (": " + ", ".join(lk["cues"]) if lk["cues"] else ""),
               "sound.md", t1=lk["to"])
    # onsets per bus (the sync table uses them too), then what explains a burst or a step at a cut
    res["onsets"] = {b: Onsets(st[b]) for b in st} if st else {}
    res["onsets"]["mix"] = Onsets(mix)
    cue_starts = [(float(c.get("start_t", c["film_t"])), c) for c in (sfx_meta or {}).get("cues", []) if c.get("status") == "placed"]
    score, score_how = score_meta(edit)
    res["score_meta_path"] = score_how
    res["score_hits"] = (score or {}).get("hits", [])

    def explain(t: float, bus: str | None) -> str | None:
        if any(w["kind"] == "mute" and (abs(w["from"] - t) < 0.006 or abs(w["to"] - t) < 0.006) for w in marks):
            return "a mute's hard edge (the dead stop is meant; its click may not be)"
        if bus in (None, "sfx"):
            c = next((c for s0, c in cue_starts if abs(s0 - t) < 0.006), None)
            if c:
                return f"the SFX cue {c['sound']} ({c['where']}) starts here"
        if bus in (None, "music"):
            h = next((h for h in res["score_hits"] if abs(float(h["t"]) - t) < 0.015), None)
            if h:
                return f"the score's hit \"{h.get('name', '?')}\" ({1000 * (float(h['t']) - t):+.0f} ms)"
            o = res["onsets"]["music"].near(t, 0.010, 0.010, 6.0) if "music" in res["onsets"] else None
            if o:
                return f"the score's own attack (+{o['rise_db']:.0f} dB at {1000 * (o['t'] - t):+.0f} ms)"
        return None
    # clicks: 1 ms bursts of >10 kHz far above their neighbourhood (on the mix, then whose bus)
    res["clicks"] = clicks(edit, mix, st, fl, explain)
    # steps at the cuts, per bus
    res["steps"] = steps_at_cuts(edit, mix, st, fl, explain)
    # loudness curves
    res["curves"] = {"mix_s": loud_curve(mix, 3.0), "mix_m": loud_curve(mix, 0.4)}
    for b in BUSES:
        if b in st:
            res["curves"][b] = loud_curve(st[b], 3.0)
    # the voice against its bed
    res["margins"] = margins(edit, st, fl) if st else []
    th.join()
    res["ebur128"] = summ
    tp = summ.get("true_peak")
    if tp is not None and tp > -1.0:
        fl.add("fix" if tp > 0 else "look", None, f"true peak {tp:.2f} dBTP (the master's target is -1.0)", "sound.md")
    return res


def clicks(edit, mix: np.ndarray, st: dict, fl: Flags, explain) -> list[dict]:
    sos = signal.butter(4, 10000, "highpass", fs=SR, output="sos")

    def hf_db(x):
        y = signal.sosfilt(sos, mono(x).astype(np.float64))
        w = 48
        e = np.sqrt((y[: len(y) // w * w].reshape(-1, w) ** 2).mean(1)) + 1e-9
        return 20 * np.log10(e)
    db = hf_db(mix)
    base = ndimage.median_filter(db, size=101)
    spk = np.where((db - base > 18) & (db > -60))[0]
    ev: list[list] = []
    for i in spk:
        if ev and i - ev[-1][1] <= 5:
            ev[-1][1] = i
            ev[-1][2] = max(ev[-1][2], db[i] - base[i])
        else:
            ev.append([i, i, db[i] - base[i]])
    cuts = edit.cuts()
    out = []
    bus_hf = {}
    for i0, i1, h in ev[:400]:
        t = i0 / 1000
        near = min(cuts, key=lambda c: abs(c - t)) if cuts else None
        dc = (t - near) if near is not None else None
        who = []
        for b in st:
            if b not in bus_hf:
                bus_hf[b] = hf_db(st[b])
            bd = bus_hf[b]
            bb = ndimage.median_filter(bd[max(0, i0 - 60): i1 + 60], size=101)
            k = i0 - max(0, i0 - 60)
            if bd[i0: i1 + 1].max() - bb[k] > 12:
                who.append(b)
        rec = {"t": t, "over_db": float(h), "level_db": float(db[i0]), "cut_dt": dc, "shot": edit.shot_id_at(t), "bus": who}
        if dc is not None and abs(dc) < 0.006:
            why = [explain(near, b) for b in (who or [None])]
            rec["why"] = "; ".join(w for w in why if w) if all(why) else None
            fl.add("info" if rec["why"] else "look", t, f"a 1 ms burst of >10 kHz {h:.0f} dB over its surroundings at the cut into "
                   f"{edit.shot_id_at(near + 0.5 / edit.fps)}" + (f" ({', '.join(who)})" if who else "")
                   + (f": {rec['why']}" if rec["why"] else ": a click?"), "--range")
        out.append(rec)
    return out


def steps_at_cuts(edit, mix: np.ndarray, st: dict, fl: Flags, explain) -> list[dict]:
    """A step in the waveform at a shot boundary: the jump across the cut's sample against the signal's
    typical sample-to-sample change around it, per bus."""
    out = []
    sigs = {"mix": mix} | {b: st[b] for b in st}
    for s in edit.shots[1:]:
        c = int(round(s["frame"] * SR / edit.fps))   # the cut's own sample (frame x 800)
        row = {"t": s["start"], "to": s["id"]}
        worst = None
        for name, x in sigs.items():
            if c < 300 or c > len(x) - 300:
                continue
            m = mono(x[c - 240: c + 240]).astype(np.float64)
            d = np.abs(np.diff(m))
            jump = float(d[238:241].max())                       # the cut's sample, give or take one
            # a step is one jump that stands alone: the samples on either side change far less (a drum's
            # attack, which also jumps, keeps changing fast after it)
            near = max(float(d[218:237].max()), float(d[242:262].max())) + 1e-6
            r = jump / near
            row[name] = round(r, 1)
            if name != "mix" and jump > 0.01 and r > 4 and (worst is None or r > worst[1]):
                worst = (name, r, jump)
        if worst is None and row.get("mix", 0) > 4 and float(np.abs(np.diff(mono(mix[c - 2:c + 2]))).max()) > 0.01:
            worst = ("mix", row["mix"], 0.0)
        if worst:
            row["worst"] = worst[0]
            why = explain(s["start"], worst[0] if worst[0] != "mix" else None)
            row["why"] = why
            fl.add("info" if why else "look", s["start"], f"a step in the {worst[0]} at the cut into {s['id']}: "
                   f"a jump {worst[1]:.0f}x any sample-to-sample change around it" + (f": {why}" if why else ": a click?"), "--range")
        out.append(row)
    return out


def margins(edit, st: dict, fl: Flags) -> list[dict]:
    """Each voiced passage: the voice's loudness against the bed (music + game + sfx) over the passage, and
    the worst second of speech in it (seconds where the voice is within 4 LU of its passage level, so a
    pause between sentences does not count)."""
    if "voice" not in st:
        return []
    bed = sum(st[b] for b in ("music", "game", "sfx") if b in st)
    out = []
    tv, lv = loud_curve(st["voice"], 1.0, 0.1)
    tb, lb = loud_curve(bed, 1.0, 0.1)
    for a, b in phrases(edit):
        i0, i1 = int(a * SR), int(b * SR)
        Lv, Lb = lufs(st["voice"][i0:i1]), lufs(bed[i0:i1])
        sel = (tv >= a + 0.4) & (tv <= b - 0.4) & (lv > Lv - 4)
        worst = float((lv[sel] - lb[sel]).min()) if sel.any() else None
        wt = float(tv[sel][np.argmin(lv[sel] - lb[sel])]) if sel.any() else None
        row = {"from": a, "to": b, "voice": Lv, "bed": Lb, "margin": Lv - Lb, "worst": worst, "worst_t": wt,
               "shot": edit.shot_id_at(a)}
        out.append(row)
        if Lv - Lb < 8:
            fl.add("look", a, f"the voice only {Lv - Lb:.1f} LU over its bed in {tc(a)}–{tc(b)}", "sound.md", t1=b)
        elif worst is not None and worst < 4:
            fl.add("look", wt, f"the voice {worst:.1f} LU over its bed in its worst second of speech ({tc(wt)})", "sound.md")
    return out


# --------------------------------------------------------------- the plot ----

BUS_COL = {"voice": "#1f77b4", "music": "#9467bd", "game": "#2ca02c", "sfx": "#d68910"}


def mmss(t: float) -> str:
    t = int(round(t))
    return f"{t // 60}:{t % 60:02d}"


def plots(edit, res: dict, out: Path, flags: list[dict], span: float = 60.0) -> list[Path]:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    cv = res["curves"]
    paths = []
    n = int(np.ceil(edit.duration / span))
    for k in range(n):
        t0, t1 = k * span, min(edit.duration, (k + 1) * span)
        fig, ax = plt.subplots(figsize=(18, 5), dpi=100)
        for w in res["marks"]:
            if w["to"] > t0 and w["from"] < t1:
                ax.axvspan(max(t0, w["from"]), min(t1, w["to"]), color="#f4c7c3" if w["kind"] == "mute" else "#fde2b8",
                           zorder=0, lw=0)
        for s in edit.shots:
            if t0 <= s["start"] <= t1:
                ax.axvline(s["start"], color="#bbbbbb", lw=0.8, zorder=1)
                ax.text(s["start"] + 0.08, -6.5, s["id"], rotation=90, fontsize=8, va="top", color="#555555")
        for sct in edit.sections:
            if t0 <= sct["start"] <= t1:
                ax.axvline(sct["start"], color="#333333", lw=1.6, zorder=1)
                ax.text(sct["start"] + 0.15, -54, f"{sct['id']} {sct.get('title', '')}", fontsize=9, color="#333333")
        t, l = cv["mix_m"]
        sel = (t >= t0) & (t <= t1)
        ax.plot(t[sel], l[sel], color="#cccccc", lw=0.7, label="mix, momentary (0.4 s)")
        t, l = cv["mix_s"]
        sel = (t >= t0) & (t <= t1)
        ax.plot(t[sel], l[sel], color="black", lw=1.8, label="mix, short-term (3 s)")
        for b in BUSES:
            if b in cv:
                t, l = cv[b]
                sel = (t >= t0) & (t <= t1)
                ax.plot(t[sel], l[sel], color=BUS_COL[b], lw=1.1, label=f"{b} (3 s)")
        for f in flags:
            if f.get("t") is not None and t0 <= f["t"] <= t1 and f["sev"] in ("fix", "look"):
                ax.plot([f["t"]], [-57], marker="v" if f["sev"] == "fix" else "o", color="red" if f["sev"] == "fix" else "orange",
                        ms=7, zorder=5)
        ax.set_xlim(t0, t0 + span)
        ax.set_ylim(-60, -5)
        ax.set_ylabel("LUFS")
        ticks = np.arange(np.ceil(t0 / 5) * 5, t1 + 0.01, 5)
        ax.set_xticks(ticks)
        ax.set_xticklabels([mmss(x) for x in ticks], fontsize=8)
        ax.grid(axis="y", color="#eeeeee")
        ax.legend(loc="lower right", fontsize=8, ncol=3, framealpha=0.85)
        ax.set_title(f"{edit.cut.name} · loudness {mmss(t0)}–{mmss(t1)} · grey lines: cuts · "
                     "pink: muted · peach: the SFX stem's silences · markers: flags", fontsize=10, loc="left")
        fig.tight_layout()
        p = out / f"loudness-{k + 1:02d}.png"
        fig.savefig(p)
        plt.close(fig)
        paths.append(p)
    return paths


# --------------------------------------------------------------- Markdown ----

def sound_md(edit, res: dict, plots_: list[Path]) -> str:
    L = ["# Sound", ""]
    e = res.get("ebur128", {})
    L.append(f"**The cut:** {e.get('integrated')} LUFS integrated, LRA {e.get('lra')} LU, true peak {e.get('true_peak')} dBTP "
             f"(ffmpeg ebur128); sample peak {res['sample_peak']:.3f}, {res['clipped_samples']} clipped samples.")
    L.append("")
    L.append(f"**The buses:** {res['stems_how']}.")
    sc = res.get("stems_check")
    if sc:
        L.append(f"They add up to the cut's own sound within {sc['residual_db']} dB (the rest is the master limiter and the "
                 f"AAC encode; lag {sc['lag_samples']} samples)." + (" **That is too far: these buses may not be this cut's.**"
                                                                     if sc["residual_db"] > -15 else ""))
        mm = sc.get("mismatch") or []
        L.append("")
        L.append(("**Where they do not match** (0.25 s windows whose remainder, after their own best gain, is within 12 dB "
                  "of the cut: a part was rewritten since the build, so findings from the buses there are unreliable): "
                  + ", ".join(f"{tc(a)}–{tc(b)} ({r:+.0f} dB)" for a, b, r in mm) + ".")
                 if mm else "They match the cut everywhere (every 0.25 s window's remainder is more than 12 dB under it).")
    L.append("")
    L.append("## Loudness over time")
    L.append("")
    L.append("Short-term (3 s) loudness of the mix and of each bus, momentary (0.4 s) in grey; shot cuts and ids; "
             "marked silences shaded. Times are window centres.")
    L.append("")
    for p in plots_:
        L.append(f"![{p.stem}]({p.name})")
    L.append("")
    if res.get("margins"):
        L.append("## The voice against its bed")
        L.append("")
        L.append("Each voiced passage (the timeline's speech, gaps under 0.7 s joined): the voice's loudness and the bed's "
                 "(music + game + SFX) over the passage, the margin, and the worst one-second window in it.")
        L.append("")
        rows = [[tc(m["from"]), tc(m["to"]), m["shot"], f"{m['voice']:.1f}", f"{m['bed']:.1f}", f"**{m['margin']:.1f}**"
                 if m["margin"] < 8 else f"{m['margin']:.1f}", "-" if m["worst"] is None else f"{m['worst']:.1f} at {tc(m['worst_t'])}"]
                for m in res["margins"]]
        L.append(md_table(["from", "to", "shot", "voice LUFS", "bed LUFS", "margin LU", "worst second"], rows, "lllrrrl"))
        L.append("")
    L.append("## Marked silences")
    L.append("")
    if res["marks"]:
        rows = []
        for w in res["marks"]:
            lk = [x for x in res["leaks"] if x["mark"] == w["where"]]
            heard = "; ".join(f"{x['bus']} {tc(x['from'], 3)}–{tc(x['to'], 3)} ({x['peak_db']:.0f} dBFS"
                              + (": " + ", ".join(x.get("cues", [])) if x.get("cues") else "") + ")" for x in lk) or "nothing"
            rows.append([w["where"], tc(w["from"], 3), tc(w["to"], 3), "+".join(w["buses"]), heard])
        L.append(md_table(["mark", "from", "to", "must be silent", "heard inside (besides the voice)"], rows))
    else:
        L.append("None in the timeline or the SFX stem.")
    L.append("")
    for c in res.get("cut_in_two", []):
        L.append(f"- **Cut in two:** `{c['cue']['sound']}` ({c['cue']['where']}) runs {tc(c['cue']['start_t'], 3)}–"
                 f"{tc(c['cue']['end_t'], 3)}; {c['mute']['where']} ends at {tc(c['mute']['to'], 3)}, so its last "
                 f"{c['after_s']:.2f} s plays" + (f" ({c['after_db']:.0f} dBFS)" if c["after_db"] is not None else "") + ".")
    L.append("")
    sil = [s for s in res["silences"] if s["to"] - s["from"] >= 0.1]
    L.append("## Silences in the mix (under -60 dBFS for 0.1 s or more)")
    L.append("")
    L.append(md_table(["from", "to", "length", "marked"], [[tc(s["from"], 2), tc(s["to"], 2), f"{s['to'] - s['from']:.2f} s",
                                                              s["where"] or ("film edge" if s["from"] < 0.1 or s["to"] > edit.duration - 0.1 else "**no**")]
                                                             for s in sil]) if sil else "None.")
    L.append("")
    L.append("## Clicks")
    L.append("")
    L.append("1 ms bursts of energy above 10 kHz, 18 dB or more over their 100 ms surroundings, on the mix; the bus is "
             "the one where the burst also stands out. Speech consonants and percussion land here too: the list is for "
             "listening, not a verdict, except at a cut.")
    L.append("")
    cl = res["clicks"]
    rows = [[tc(c["t"], 3), c["shot"], f"{c['over_db']:.0f}", f"{c['level_db']:.0f}",
             ("at the cut: " + c["why"] if c.get("why") else "**at the cut**") if c["cut_dt"] is not None and abs(c["cut_dt"]) < 0.006 else
             (f"{1000 * c['cut_dt']:+.0f} ms from a cut" if c["cut_dt"] is not None and abs(c["cut_dt"]) < 0.1 else ""),
             ", ".join(c["bus"])] for c in cl[:60]]
    L.append(md_table(["time", "shot", "dB over", "level dB", "cut", "bus"], rows, "llrrll") if rows else "None.")
    if len(cl) > 60:
        L.append(f"\n({len(cl) - 60} more in `sound.json`.)")
    L.append("")
    steps = [s for s in res["steps"] if s.get("worst")]
    L.append("## Steps at the cuts")
    L.append("")
    L.append("The jump across each cut's sample against the largest sample-to-sample change in the 0.4 ms on either side, "
             "per bus: a jump over 0.01 that is 4x anything around it stands alone, a step, which clicks (a drum's attack "
             "keeps changing fast and does not count).")
    L.append("")
    L.append(md_table(["cut", "into", "worst bus", "explained by", "ratio per bus"],
                      [[tc(s["t"], 3), s["to"], s["worst"], s.get("why") or "**nothing: a click?**",
                        ", ".join(f"{k} {v}" for k, v in s.items() if k not in ("t", "to", "worst", "why"))] for s in steps])
             if steps else "None.")
    L.append("")
    return "\n".join(L)
