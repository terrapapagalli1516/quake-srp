"""Hard stops: a music, game or SFX bus that goes from audible to digital silence in under 10 ms, mid-note
(the score and a shot's game sound both cut dead at one cut). At a cut that the clock does not mark as an
intended silence, the ear hears the sound break.

Read on the buses (the build's saved stems, else the kit's re-made ones): each bus's level per millisecond;
a stop is a run of at least 80 ms below -96 dBFS whose sound before it was audible (-50 dBFS or louder over
the 40 ms before) and fell from within 6 dB of that level to silence in 10 ms or less. A stop the other buses
cover (6 dB louder over the 100 ms after it) is listed, not flagged; so is one the clock or a mute marks."""

from __future__ import annotations

import re
from pathlib import Path

import numpy as np

from common import Flags, md_table, tc

SR = 48000
BUSES = ("music", "game", "sfx")
MARK_WORDS = re.compile(r"silen|mute|stop|hush|cut_?out|_out$|dead|quiet", re.I)


def _mono_power(x: np.ndarray) -> np.ndarray:
    x = x.astype(np.float64)
    return (x ** 2).mean(axis=1) if x.ndim > 1 else x ** 2


def ms_db(x: np.ndarray, win: int = 48) -> np.ndarray:
    p = _mono_power(x)
    n = len(p) // win
    return 10 * np.log10(p[: n * win].reshape(n, win).mean(axis=1) + 1e-20)


def level_db(x: np.ndarray, a: int, b: int) -> float:
    a, b = max(0, a), max(a + 1, b)
    return float(10 * np.log10(_mono_power(x[a:b]).mean() + 1e-20)) if b <= len(x) else -200.0


def find_stops(x: np.ndarray, min_sil_ms: int = 80, audible: float = -50.0, fall_ms: int = 10) -> list[dict]:
    from picture import runs
    db = ms_db(x)
    out = []
    for a, b in runs(db < -96):
        if b - a < min_sil_ms or a < 60:
            continue
        L = level_db(x, (a - 50) * 48, (a - 10) * 48)
        if L < audible:
            continue
        j = a - 1
        while j > a - 60 and db[j] < L - 6:
            j -= 1
        fall = a - j
        if fall > fall_ms:
            continue
        tail = level_db(x, (a - 15) * 48, (a - 5) * 48)
        if tail < L - 10:            # already fading away: not mid-note
            continue
        # the stop to the sample: the last sample above -96 dBFS before the run
        s0 = a * 48
        seg = np.abs(x[max(0, s0 - 960): s0 + 48]).max(axis=1) if x.ndim > 1 else np.abs(x[max(0, s0 - 960): s0 + 48])
        nz = np.where(seg > 10 ** (-96 / 20))[0]
        t = (max(0, s0 - 960) + (int(nz[-1]) + 1 if len(nz) else 960)) / SR
        out.append({"t": t, "level_db": L, "fall_ms": int(fall), "silent_s": (b - a) / 1000})
    return out


def check(edit, st: dict, mix: np.ndarray | None, out: Path, fl: Flags) -> str:
    L = ["# Hard stops", "",
         "A bus that goes from audible to digital silence in under 10 ms, mid-note. Each bus's level per millisecond: a "
         "stop is 80 ms or more below -96 dBFS, after sound at -50 dBFS or louder over the 40 ms before, which fell from "
         "within 6 dB of that level in 10 ms or less. **fix**: at a cut, the clock marking no silence there, and the "
         "whole sound falls 10 dB or more (the ear hears it break). *look*: the other buses carry on, or it is not at a "
         "cut. Listed only: a stop the other buses cover (6 dB louder over the 100 ms after it), or one a mute or the "
         "clock marks.",
         ""]
    if not st:
        fl.add("info", None, "hard stops not checked: no buses (the build saved none and they could not be re-made)", "stops.md")
        return "\n".join(L + ["Not checked: no buses.", ""])
    cuts = [(float(s["start"]), s["id"]) for s in edit.shots[1:]]
    clock_marks = [(float(e["t"]), e["name"]) for e in (edit.clock or {}).get("events", []) if MARK_WORDS.search(e["name"])]
    mutes = []
    for s in edit.shots:
        for w0, w1 in s.get("mutes") or ([[0.0, s["len"]]] if s.get("mute") else []):
            mutes += [s["start"] + w0, s["start"] + w1]
    import sound as S
    score, _ = S.score_meta(edit)
    hits = [(float(h["t"]), h.get("name", "?")) for h in (score or {}).get("hits", []) if MARK_WORDS.search(h.get("name", ""))
            or re.search(r"rest|out", h.get("name", ""))]
    rows, found = [], []
    for bus in BUSES:
        if bus not in st:
            continue
        for sp in find_stops(st[bus]):
            t = sp["t"]
            c = min(cuts, key=lambda x: abs(x[0] - t)) if cuts else None
            at_cut = c is not None and abs(c[0] - t) <= 0.025
            a = int(t * SR)
            others = sum(st[k][a: a + int(0.1 * SR)] for k in st if k != bus and st[k] is not None)
            rest = level_db(others, 0, int(0.1 * SR)) if not np.isscalar(others) else -200.0
            allb = sum(st[k][a - int(0.05 * SR): a - int(0.01 * SR)] for k in st if st[k] is not None)
            before = level_db(allb, 0, len(allb)) if not np.isscalar(allb) else -200.0
            drop = before - rest                     # how far the whole sound falls at the stop
            masked = rest > sp["level_db"] + 6
            mark = next((f"the clock's `{n}`" for tt, n in clock_marks if abs(tt - t) <= 0.05), None) or \
                (f"a mute's edge" if any(abs(m - t) <= 0.02 for m in mutes) else None)
            note = next((f"the score names it \"{n}\": the silence is meant, the dead stop is not" for tt, n in hits
                         if abs(tt - t) <= 0.06), "")
            where = f"at the cut into {c[1]}" if at_cut else f"in {edit.shot_id_at(t)}, {1000 * (t - c[0]):+.0f} ms from a cut" if c else ""
            sev = "info" if (mark or masked) else ("fix" if at_cut and drop >= 10 else "look")
            img = draw(out, edit, st, mix, t, bus) if sev != "info" else None
            why = mark or (f"covered by the other buses ({rest:.0f} dBFS after it)" if masked else
                           ("nothing else plays after it" if rest < -90 else f"the whole sound falls {drop:.0f} dB")
                           if drop >= 10 else "the other buses carry on")
            fl.add(sev, t, f"the {bus} stops dead {where}: {sp['level_db']:.0f} dBFS to digital silence in {sp['fall_ms']} ms, "
                   f"silent {sp['silent_s']:.2f} s after" + (f" ({why})" if why else "") + (f"; {note}" if note else ""),
                   str(img.relative_to(out)) if img else "stops.md")
            found.append(sp | {"bus": bus, "where": where, "sev": sev, "why": why or note})
            rows.append([tc(t, 3), bus, where, f"{sp['level_db']:.0f}", str(sp["fall_ms"]), f"{sp['silent_s']:.2f}", sev,
                         why or note or "-", f"[plot]({img.relative_to(out)})" if img else "-"])
    if rows:
        L.append(md_table(["time", "bus", "where", "dBFS before", "fall ms", "silent s", "", "why", "see"],
                          sorted(rows, key=lambda r: r[0])))
    else:
        L.append("None found.")
    L.append("")
    return "\n".join(L)


def draw(out: Path, edit, st: dict, mix: np.ndarray | None, t: float, bus: str) -> Path:
    """Each bus's level around the stop (±0.4 s), the cuts in red, as the kit's filmstrips draw it."""
    from PIL import Image, ImageDraw
    import picture as P
    from common import font
    ev = out / "evidence"
    ev.mkdir(exist_ok=True)
    p = ev / f"stop-{bus}-{tc(t, 3).replace(':', 'm')}.png"
    audio = {"sr": SR} | ({"mix": mix} if mix is not None else {}) | {k: v for k, v in st.items() if v is not None}
    im = Image.new("RGB", (1000, 300), (18, 18, 20))
    d = ImageDraw.Draw(im)
    d.text((8, 6), f"{edit.cut.name} · the {bus} stops at {tc(t, 3)} · {tc(t - 0.4, 2)}–{tc(t + 0.4, 2)} · red: cuts",
           font=font(14), fill=(230, 180, 60))
    P._wave(d, audio, t - 0.4, t + 0.4, 8, 30, 984, 260, edit.fps, 1, edit.cuts())
    im.save(p)
    return p
