"""The sync table: each named moment of the edit, each scripted sound cue and each sound the footage
logged, where it should land on the film (the picture's action), and where a rise in the right bus
shows it landed; plus whether a short hit can be heard over what plays with it."""

from __future__ import annotations

import csv
import json
import re
import statistics
from pathlib import Path

import numpy as np

from common import FILM, PIPELINE, Flags, md_table, rel, tc
from sound import SR, mono, rms_db

FRAME = 1 / 60


def resolve(edit, ref: str) -> float | None:
    """A cue's anchor on this cut: an event name (+/-x), SHOT@in/@out(+/-x), SHOT+x, SHOT~word[#n][.end](+/-x)."""
    ev = {e["name"]: e["t"] for e in (edit.clock or {}).get("events", []) if e.get("t") is not None}
    m = re.match(r"^([A-Za-z_]\w*)([+-][\d.]+)?$", ref)
    if m and m.group(1) in ev:
        return ev[m.group(1)] + float(m.group(2) or 0)
    by = {s["id"]: s for s in edit.shots}
    m = re.match(r"^(\w+)(?:@(in|out)([+-][\d.]+)?|~([^\s+]+?)(\.end)?([+-][\d.]+)?|([+-][\d.]+))$", ref)
    if not m or m.group(1) not in by:
        return None
    s = by[m.group(1)]
    if m.group(2):
        return s["start"] + (s["len"] if m.group(2) == "out" else 0.0) + float(m.group(3) or 0)
    if m.group(7):
        return s["start"] + float(m.group(7))
    w, _, k = m.group(4).partition("#")
    k = int(k) if k else 1
    seen = 0
    for x in s.get("words", []):
        if x["w"] == w.lower():
            seen += 1
            if seen == k:
                return s["start"] + (x.get("e", x["t"]) if m.group(5) else x["t"]) + float(m.group(6) or 0)
    return None


def word_at(words: list[dict] | None, t: float, pad: float = 0.03) -> dict | None:
    for w in words or []:
        if w["start"] - pad <= t <= w["end"] + pad:
            return w
    return None


def _ons(res: dict, bus: str, t: float, before: float, after: float):
    o = (res.get("onsets") or {}).get(bus)
    return o.near(t, before, after) if o else None


def _audibility(st: dict, bus: str, t: float, dur: float = 0.05) -> float | None:
    """The bus against everything else, in the 50 ms from t: dB (negative: under the rest)."""
    if not st or bus not in st:
        return None
    a, b = int(t * SR), int((t + dur) * SR)
    me = rms_db(mono(st[bus][a:b]))
    rest = sum(mono(st[k][a:b]) for k in st if k != bus)
    return me - rms_db(rest) if not np.isscalar(rest) else None


def events_table(edit, res: dict, words: list[dict] | None) -> list[dict]:
    rows = []
    ck = edit.clock or {}
    for e in ck.get("events", []):
        if e.get("t") is None:
            continue
        rows.append({"kind": "event", "name": e["name"], "ref": e["ref"], "t": e["t"]})
    for grp in ("edit_hits", "picture_events"):
        for g in ck.get(grp, []):
            for k, t in enumerate(g["t"]):
                rows.append({"kind": grp.replace("_", " ").rstrip("s"), "name": f"{g['shot']} {g['what']} #{k + 1}",
                             "ref": g["shot"], "t": t})
    for r in rows:
        for bus in ("music", "sfx", "game", "mix"):
            o = _ons(res, bus, r["t"], 0.12, 0.12)
            r[bus] = None if o is None else o["t"] - r["t"]
            r[bus + "_rise"] = None if o is None else o["rise_db"]
        m = re.search(r"~([\w']+)", r.get("ref", ""))
        if m and words:
            w = min((w for w in words if w["text"].lower().strip(".,:;!?\"") == m.group(1).lower()),
                    key=lambda w: abs(w["start"] - r["t"]), default=None)
            if w and abs(w["start"] - r["t"]) < 1.0:
                r["word_heard"] = w["start"]
    return sorted(rows, key=lambda r: r["t"])


def relist(edit, sfx_meta: dict, scratch: Path) -> tuple[list[tuple[float, str, str]] | None, str]:
    """The sound designer's own resolver (film/pipeline/sound/render.py --list) run on THIS cut's timeline and
    clock: where each cue would land now. Against the stem's placements, it shows a stem made on another clock
    or from an older cue list. The cue files the stem's sidecar names are looked for under FILM_ROOT, then in
    film/pipeline/sound/."""
    import shutil
    import subprocess
    files = []
    for f in sfx_meta.get("cue_files", []):
        p = next((c for c in (FILM / f, FILM / "sound" / f, PIPELINE / "sound" / Path(f).name) if c.exists()), None)
        if p is not None:
            files.append(str(p))
    render = PIPELINE / "sound" / "render.py"
    if not files or not render.exists() or not edit.timeline_path:
        return None, "the cue files or sound/render.py are missing"
    d = scratch / "relist"
    d.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(edit.timeline_path, d / "timeline.json")
    if edit.clock_path:
        shutil.copyfile(edit.clock_path, d / "clock.json")
        lad = edit.clock_path.parent / "ladder-events.json"
        if lad.exists():
            shutil.copyfile(lad, d / "ladder-events.json")
    r = subprocess.run(["uv", "run", "-q", str(render), *files, "--timeline", str(d / "timeline.json"), "--list"],
                       capture_output=True, text=True, cwd=str(FILM), timeout=600)
    if r.returncode != 0:
        return None, "render.py --list failed: " + (r.stderr.strip().splitlines() or ["?"])[-1][:160]
    out = []
    for ln in r.stdout.splitlines():
        m = re.match(r"^(\d+):(\d+\.\d+)\s+(\S+)\s+(.+?)\s+(\S+)(?:\s+\[.*\])?$", ln)
        if m and "[" not in ln:
            out.append((int(m.group(1)) * 60 + float(m.group(2)), m.group(4).strip(), m.group(5)))
    return out, f"{len(out)} cues re-placed by `sound/render.py --list` on this cut's timeline"


def cue_table(edit, res: dict, st: dict, sfx_meta: dict | None, words: list[dict] | None, fl: Flags,
              scratch: Path) -> tuple[list[dict], str]:
    rows = []
    cues = [c for c in (sfx_meta or {}).get("cues", []) if c.get("status") == "placed"]
    now, how = relist(edit, sfx_meta, scratch) if cues else (None, "")
    pool: dict[tuple[str, str], list[float]] = {}
    for t, snd, when in now or []:
        pool.setdefault((snd, when), []).append(t)
    group_n: dict[str, int] = {}
    for c in cues:
        group_n[c["where"]] = group_n.get(c["where"], 0) + 1
    moved = []
    masked_seen = set()
    for c in cues:
        t = float(c["film_t"])
        dur = float(c.get("end_t", t)) - float(c.get("start_t", t))
        lst = pool.get((c["sound"], c["when"]))
        again = lst.pop(0) if lst else None
        o = _ons(res, "sfx", t, 0.06, 0.06)
        r = {"where": c["where"], "when": c["when"], "sound": c["sound"], "placed": t, "dur": dur, "now": again,
             "moved": None if again is None else t - again, "onset": None if o is None else o["t"],
             "rise": None if o is None else o["rise_db"], "repeats": group_n[c["where"]]}
        r["landed"] = None if o is None else o["t"] - t
        tt = t   # the cue's own hit moment (a rise found nearby may be another sound's)
        r["over_rest"] = _audibility(st, "sfx", tt) if st else None
        w = word_at(words, tt)
        r["word"] = w["text"] if w else None
        rows.append(r)
        if again is not None and abs(t - again) > 0.0015 + 1.5 * FRAME:
            moved.append(r)
        # a hit placed on a spoken word (the anchor names the word, or an event that does): the word masks it
        ev_ref = next((e.get("ref", "") for e in (edit.clock or {}).get("events", []) if e["name"] == c["when"].split("+")[0].split("-")[0]), "")
        on_word = "~" in c["when"] or "~" in ev_ref
        hit = dur < 0.6 or abs(float(c.get("sync") or 0.0)) > 1e-3   # short, or a file whose sync point is its hit
        if r["over_rest"] is not None and r["over_rest"] < -6 and hit and on_word and w and group_n[c["where"]] <= 2 \
                and (c["when"], round(t, 2)) not in masked_seen:
            masked_seen.add((c["when"], round(t, 2)))
            fl.add("look", tt, f"the hit {c['sound']} ({c['where']}, \"{c['when']}\"{' = ' + ev_ref if ev_ref else ''}) is placed on "
                   f"the spoken \"{w['text']}\" and sits {-r['over_rest']:.0f} dB under the rest of the mix in its first 50 ms: "
                   "the word masks it", "sync.md")
    if moved:
        worst = max(moved, key=lambda r: abs(r["moved"]))
        fl.add("look", worst["placed"], f"{len(moved)} SFX cues sit where this cut would no longer put them (worst: {worst['sound']}, "
               f"{worst['where']}, {worst['moved']:+.3f} s): the stem was made on another clock or an older cue list", "sync.md")
    return rows, how


def score_table(edit, res: dict, fl: Flags) -> list[dict]:
    """The composer's named hits (music/score-vN.json): the picture moment each is aimed at (the nearest cut
    or named moment within 0.25 s), and where a rise in the score bus shows it landed."""
    marks = [(s["start"], f"cut to {s['id']}") for s in edit.shots[1:]]
    ck = edit.clock or {}
    marks += [(e["t"], e["name"]) for e in ck.get("events", []) if e.get("t") is not None]
    for grp in ("edit_hits", "picture_events"):
        for g in ck.get(grp, []):
            marks += [(t, f"{g['shot']} {g['what']}") for t in g["t"]]
    rows = []
    for h in res.get("score_hits") or []:
        t = float(h["t"])
        aim = min(marks, key=lambda m: abs(m[0] - t)) if marks else None
        if aim and abs(aim[0] - t) > 0.25:
            aim = None
        o = _ons(res, "music", t, 0.06, 0.06)
        r = {"name": h.get("name", "?"), "t": t, "aim": aim[1] if aim else None, "aim_t": aim[0] if aim else None,
             "off_aim": (t - aim[0]) if aim else None, "landed": None if o is None else o["t"] - t}
        rows.append(r)
        if aim and abs(t - aim[0]) > 0.04:
            fl.add("look", t, f"the score's hit \"{r['name']}\" is written {1000 * (t - aim[0]):+.0f} ms from {aim[1]} "
                   f"({tc(aim[0], 3)})", "sync.md")
    return rows


def _events_file(src: dict) -> Path | None:
    p = FILM / src.get("path", "")
    stem = re.sub(r"\.(clean|zoom|full|v\d+)$", "", p.stem)
    for c in (p.with_name(stem + ".events.json"), p.with_name(p.stem + ".events.json")):
        if c.exists():
            return c
    return None


def footage_table(edit, res: dict, fl: Flags) -> tuple[list[dict], list[dict]]:
    """Each sound the engine logged while rendering a shot's footage, on the film's clock."""
    rows, per_shot = [], []
    for s in edit.shots:
        src = s.get("source", {})
        if src.get("type") not in ("video", "burst") or src.get("reverse"):
            continue
        ef = _events_file(src)
        if not ef:
            continue
        try:
            evs = json.loads(ef.read_text()).get("events", [])
        except Exception:  # noqa: BLE001
            continue
        sp = float(src.get("speed", 1.0))
        a, b = s["start"], s["start"] + s["len"]
        heard_bus = not (s.get("mute") or s.get("mute_game")) and bool(src.get("sound") or (src.get("probe") or {}).get("audio")
                                                                        or src.get("compose"))
        mutes = [(a + x, a + y) for x, y in s.get("game_mute", [])]
        offs = []
        n = 0
        for e in evs:
            if e.get("kind") != "sound" or "null.wav" in e.get("sample", ""):
                continue
            t = a + (float(e["t"]) - float(src.get("in", 0.0))) / sp
            if not a <= t < b:
                continue
            n += 1
            muted = any(x <= t < y for x, y in mutes)
            o = _ons(res, "game", t, 0.03, 0.06) if heard_bus and not muted else None
            r = {"shot": s["id"], "t": t, "sample": e.get("sample"), "frame": e.get("frame"), "screen": e.get("screen"),
                 "bus_plays": heard_bus and not muted, "landed": None if o is None else o["t"] - t,
                 "rise": None if o is None else o["rise_db"]}
            rows.append(r)
            if r["landed"] is not None:
                offs.append(r["landed"])
        if n:
            per_shot.append({"shot": s["id"], "events": n, "bus": "plays" if heard_bus else "muted / no sound",
                             "found": len(offs), "median": statistics.median(offs) if offs else None,
                             "worst": max(offs, key=abs) if offs else None, "file": rel(ef), "speed": sp})
            per_shot[-1]["t"] = a
    # a constant lag comes with the footage (and with how a rise is read): flag a shot only where its sounds
    # sit apart from the film's norm
    allo = [r["landed"] for r in rows if r["landed"] is not None]
    norm = statistics.median(allo) if allo else 0.0
    for p in per_shot:
        if p["found"] >= 3 and abs(p["median"] - norm) > 0.04:
            fl.add("look", p["t"], f"{p['shot']}'s game sounds land {p['median'] * 1000:+.0f} ms (median of {p['found']}) from the "
                   f"frames that made them, against {norm * 1000:+.0f} ms across the film", "sync.md")
    for p in per_shot:
        p["norm"] = norm
    return rows, per_shot


def _ms(x) -> str:
    return "-" if x is None else f"{x * 1000:+.0f}"


def sync_md(edit, ev: list[dict], cues: list[dict], foot: list[dict], per_shot: list[dict], out: Path,
            sfx_meta_path: str | None, has_stems: bool, relist_how: str = "", score: list[dict] | None = None,
            score_how: str = "") -> str:
    L = ["# Sync", ""]
    L.append("Where each moment should land (the picture's action, or the anchor the cue names) against where a rise in "
             "level shows it landed: the strongest rise of 6 dB or more within the window, in the bus that should carry it "
             f"(milliseconds, + is late; a 2.5 ms hop, so ±3 ms is the same). Buses: {'the build’s own' if has_stems else 'none: the mix only'}.")
    L.append("")
    L.append("## The edit's named moments")
    L.append("")
    if ev:
        rows = [[tc(r["t"], 3), r["name"], r.get("ref", ""), _ms(r["music"]), _ms(r["sfx"]), _ms(r["game"]), _ms(r["mix"]),
                 (f"{tc(r['word_heard'], 3)} ({_ms(r['word_heard'] - r['t'])})" if r.get("word_heard") else "")]
                for r in ev]
        L.append(md_table(["picture", "moment", "ref", "score ms", "sfx ms", "game ms", "mix ms", "its word heard"], rows,
                          "llllrrrrl"))
        L.append("")
        L.append("Within ±120 ms; `-` is no clear rise there. Events come from the cut's `clock.json` "
                 "(events, edit hits, the footage's own picture events).")
    else:
        L.append("No clock.json for this cut.")
    L.append("")
    L.append("## The score's hits")
    L.append("")
    if score:
        L.append(f"The composer's named hits ({score_how}): the picture moment each is aimed at (the nearest cut or named "
                 "moment within 0.25 s), how far from it the hit is written, and where a rise in the score bus (broadband or "
                 "above 4 kHz) shows it landed, within ±60 ms.")
        L.append("")
        rows = [[tc(r["t"], 3), r["name"], r["aim"] or "-", "-" if r["aim_t"] is None else tc(r["aim_t"], 3),
                 "-" if r["off_aim"] is None else (f"**{_ms(r['off_aim'])}**" if abs(r["off_aim"]) > 0.04 else _ms(r["off_aim"])),
                 _ms(r["landed"])] for r in score]
        L.append(md_table(["written", "hit", "aimed at", "at", "off ms", "landed ms"], rows, "lllrrr"))
    else:
        L.append(f"No score hits for this cut ({score_how}).")
    L.append("")
    L.append("## The sound designer's cues")
    L.append("")
    if cues:
        L.append(f"Cue list: {sfx_meta_path}. Each cue line once (a line placed several times: ×N, its first placement shown, "
                 "the worst of the others in the columns): where its sync point sits in the stem; where the sound designer's own "
                 f"resolver puts it on this cut now ({relist_how or 'not run'}); the rise found in the SFX bus within ±60 ms of it; "
                 "and the SFX bus against everything else in the 50 ms from its sync point (dB; under -6 a short hit is likely lost).")
        L.append("")
        groups: dict[str, list[dict]] = {}
        for r in cues:
            groups.setdefault(r["where"], []).append(r)
        rows = []
        for g in groups.values():
            r = g[0]
            mv = [x["moved"] for x in g if x["moved"] is not None]
            ld = [x["landed"] for x in g if x["landed"] is not None]
            ov = [x["over_rest"] for x in g if x["over_rest"] is not None]
            worst_mv = max(mv, key=abs) if mv else None
            worst_ld = max(ld, key=abs) if ld else None
            low = min(ov) if ov else None
            rows.append([tc(r["placed"], 3), r["where"] + (f" ×{len(g)}" if len(g) > 1 else ""), r["when"], r["sound"][:30],
                         "-" if worst_mv is None else (f"**{_ms(worst_mv)}**" if abs(worst_mv) > 0.03 else _ms(worst_mv)),
                         _ms(worst_ld), "-" if low is None else (f"**{low:.0f}**" if low < -6 else f"{low:.0f}"),
                         r["word"] or ""])
        L.append(md_table(["placed", "cue", "anchor", "sound", "moved since ms", "landed ms", "over the rest dB", "word spoken"],
                          rows, "llllrrrl"))
    else:
        L.append(f"No SFX cue list for this cut: {sfx_meta_path}.")
    L.append("")
    L.append("## The footage's own sounds")
    L.append("")
    if per_shot:
        L.append("Each sound the engine logged while rendering a shot (`footage/**/*.events.json`), on the film's clock "
                 "(in-point and speed applied), against the first rise in the game bus from 30 ms before to 60 ms after. "
                 f"Across the film they land {_ms(per_shot[0].get('norm'))} ms after their frames (median): a lag that comes "
                 "with the footage (its sound and its frames are one render, and the onset finder reads a sound's steepest rise, "
                 "not its first sample); a shot is flagged only where it sits 40 ms or more from that. "
                 "All of them are in `sync.tsv`.")
        L.append("")
        rows = [[p["shot"], p["events"], p["bus"], p["found"], _ms(p["median"]), _ms(p["worst"]), p["file"]] for p in per_shot]
        L.append(md_table(["shot", "sounds", "game bus", "rises found", "median ms", "worst ms", "events file"], rows, "lrlrrrl"))
    else:
        L.append("No shot has an events file.")
    L.append("")
    with open(out / "sync.tsv", "w", newline="") as f:
        w = csv.writer(f, delimiter="\t")
        w.writerow(["table", "film_t", "name", "ref", "sound", "landed_ms", "detail"])
        for r in ev:
            w.writerow(["moment", f"{r['t']:.4f}", r["name"], r.get("ref", ""), "", _ms(r["sfx"] if r["sfx"] is not None else r["mix"]),
                        f"music {_ms(r['music'])} sfx {_ms(r['sfx'])} game {_ms(r['game'])}"])
        for r in cues:
            w.writerow(["cue", f"{r['placed']:.4f}", r["where"], r["when"], r["sound"], _ms(r["landed"]),
                        f"moved since {_ms(r['moved'])}; over the rest {r['over_rest']}; word {r['word'] or ''}"])
        for r in foot:
            w.writerow(["footage", f"{r['t']:.4f}", r["shot"], f"frame {r['frame']}", r["sample"], _ms(r["landed"]),
                        "bus plays" if r["bus_plays"] else "bus muted"])
    return "\n".join(L)
