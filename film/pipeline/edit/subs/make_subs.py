#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["httpx", "numpy", "pillow"]
# ///
"""The film's subtitles, for one cut, in one command:

    uv run film/pipeline/edit/subs/make_subs.py --cut edit/v7/cut.mp4 --voice voice/v7-lines.json --clock edit/v7/clock.json

Relative paths are looked up in the current folder, then under FILM_ROOT. Into FILM_ROOT/edit/subs/ (--out; NAME
is the cut's file name without .mp4):
  NAME.srt, NAME.vtt        the subtitles (the VTT also carries each moved cue's line position)
  NAME.ass                  the burn-in's styling and placement (1920x1080 script coordinates)
  NAME.json                 every caption: its words' times, its shots, where it sits and why
  NAME-check.md             the subtitles against what Scribe hears in the cut (needs the cut)
  NAME-480p-subs.mp4        burned in, 854x480, H.264 two-pass, under 10 MB (needs the cut)
  NAME-480p-subs-hevc.mp4   the same in HEVC at the same budget (keeps the torch flicker)
Scratch (Scribe's transcripts, overlay maps, the 480p master, frame sheets): FILM_SCRATCH/subs/. The burns take
minutes (the HEVC one longest); they run at low priority (nice).

The text is the voice map's (the script's spellings), timed by the edit's clock: each word's time from the shots'
words (timeline.json beside the clock gives their ends too). Captions: at most 2 lines of about 38 characters
(42 at most), 1-6 s, broken at phrase boundaries (a dynamic programme over the word breaks), split at shot cuts
where the phrasing allows, started on a cut when the words begin just after one, on the film's frames. Numbers of
11 and up, and the numbers beside them, show as numerals (`DISPLAY`); `--spoken-numbers` keeps the script's
words. `SOUND_TAGS`: [silence] at the black diff, [click] on id's two clicks (the clock's named events).

Placement: bottom centre unless the film's own text is there. Where that is comes from the timeline's overlays
(labels, captions, cards, the alpha of every overlay movie, mapped on a 20 px grid, 10 times a second) and from
`FOOTAGE_TEXT`, the text burned into footage files, found by looking (add to it when new footage carries text).
Three fixed lanes (BOTTOM_Y1, TOP_Y0, RAISED_Y1; the bottom band ends 40 px above the foot at 480p, clear of a
phone's scrub bar), and one lane per shot: if any caption in a shot must move, the shot's captions move together
(shots joined by a caption share one). Before a shot leaves the bottom, its two-line captions are tried as
one-liners. `--sheets` draws one frame for every shot with captions, burned in, labelled with its lane.

Flags: --no-check (skip Scribe), --no-burn (skip the encodes), --sheets, --name NAME. If the cut's length differs
from the clock's, it says so loudly: subtitles from one clock on another cut's picture are wrong.
Python only through uv. The check needs ElevenLabs' Scribe (speech to text): its key in the environment variable
ELEVENLABS_API_KEY, never printed; without it the check is skipped.
"""

from __future__ import annotations

import argparse
import difflib
import functools
import hashlib
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1]))
from filmroot import FILM, scratch  # noqa: E402

SCRATCH = scratch("subs")
KIT_SCRIBE = scratch("watch") / "cache" / "scribe"   # the viewing kit's Scribe cache (read)
KEY_ENV = "ELEVENLABS_API_KEY"

FPS, W, H = 60, 1920, 1080

# ------------------------------------------------------------------ style ----
MAXL, HARDL = 38, 42            # characters a line: the target, and the most
MIN_DUR, MAX_DUR = 1.0, 6.0     # seconds a caption
MIN_TAG = 0.6                   # a sound tag may be this short when the next line follows it
HANG = 0.5                      # a caption stays this long after its last word ends
GAP = 2 / FPS                   # between two captions
LEAD_SNAP, LAG_SNAP = 0.5, 0.25 # a caption whose words start this soon after (before) a cut starts on the cut
CHAIN = 0.5                     # two captions closer than this, with no cut between, touch (no flicker)
SPLIT_GAP = 0.9                 # a silence this long inside a line always splits the caption

FONT_NAME = "Inter SemiBold"


@functools.cache
def _font() -> tuple[str, int]:
    """Inter SemiBold's file and face index, through fontconfig."""
    r = subprocess.run(["fc-match", "-f", "%{file}|%{index}|%{family}", "Inter:style=SemiBold"], capture_output=True, text=True)
    f, i, fam = (r.stdout.split("|") + ["", "", ""])[:3] if r.returncode == 0 else ("", "", "")
    if "Inter" not in fam or not Path(f).exists():
        raise SystemExit("make_subs.py: needs the Inter font (Inter SemiBold), installed for fontconfig")
    return f, int(i or 0)

FONT_PX = 64                    # ASS Fontsize at 1080: libass sets ascent+descent to it, so Inter's em is 53 px (24 px at 480p)
PAD = 12                        # the band's margin round the text (ASS Outline with BorderStyle 3)
LINE_PX = 64                    # libass's line pitch: the Fontsize (measured on a render: 2 lines + bands, 135 px at 56)
EM = FONT_PX * 2048 / (1984 + 494)   # Inter: unitsPerEm 2048, ascender 1984, descender 494
# Three fixed lanes, px at 1080, each an edge the band keeps whatever its line count. Every shot uses one lane.
#   bottom: the band ends 90 px above the frame's foot (40 px at 480p, above a phone player's scrub bar);
#   top:    the band starts below the film's top labels (they end by y 112 with their margin, LAB4a's aside);
#   raised: the band ends above the film's lower text: S27's ALL PASS, the S18 menu's help (from y 735).
BOTTOM_Y1, TOP_Y0, RAISED_Y1 = 990, 116, 720
LANES = ("bottom", "top", "raised")    # in order of preference
TEXT_RGB, BAND_RGB, BAND_ALPHA = (242, 236, 224), (0, 0, 0), 0.78   # warm white on the film's black band
GRID = 20                       # the occupancy grid's cell, px at 1080
CLEAR = 8                       # a caption keeps this far from a known text box (px at 1080)

# Spoken phrases shown as numerals (the clock's words, joined by spaces) -> the text shown. Longest first.
DISPLAY = [
    ("two hundred and fifty six", "256"), ("two hundred and forty hertz", "240 Hz"),
    ("four hundred and eighty hertz", "480 Hz"), ("a hundred and forty three", "143"),
    ("nineteen ninety six", "1996"), ("twenty twenty six", "2026"), ("seventy two hertz", "72 Hz"),
    ("seventy two", "72"), ("twenty three", "23"), ("six point seven", "6.7"), ("ten eighty", "1080p"),
    ("ninety degrees", "90 degrees"), ("every sixteen pixels", "every 16 pixels"),
    ("every eight pixels", "every 8 pixels"), ("why eight", "why 8"),
    # light styles are strings of letters: the voice says "zee" (a TTS respelling), the screen shows the letter
    ("a for dark", '"a" for dark'), ("zee for bright", '"z" for bright'),
]

# Sound and scene tags, sparingly: (clock event, seconds or None for "until the next caption", text).
SOUND_TAGS = [
    ("diff_black", None, "[silence]"),     # the black diff: the joke is that the sound is gone too
    ("first_click", 1.0, "[click]"),       # id's mixer, before "Like that click"
    ("second_click", 1.0, "[click]"),
]

# Text burned into footage files (found by looking): picture path -> [(x0, y0, x1, y1), ...] at 1080, the whole shot.
# "path" or "path#SHOT" -> [(x0, y0, x1, y1, file_from, file_to)], the times in the source file's seconds (None: open).
FOOTAGE_TEXT: dict[str, list[tuple]] = {
    # the slop-options submenu's help text under the list (NOT IN ID'S QUAKE, its three lines)
    # the options menu (banner, the QUAKE/id column, its rows and sliders), then the slop-options submenu with its
    # help text under it (NOT IN ID'S QUAKE, two lines at first, then three): file times as cut-v4b shows them
    "footage/web/S18-slop-options-menu.mp4": [(240, 15, 410, 740, None, None), (395, 15, 1700, 760, None, 3.2),
                                              (395, 15, 1470, 500, 3.2, None), (360, 735, 1570, 899, 3.2, 3.5),
                                              (360, 735, 1570, 950, 3.5, None)],
    # the oracle comparison: the panels' labels, the map name, and the difference panel with its label
    "footage/game/S21.mp4#S21": [(55, 45, 215, 80, None, None), (975, 45, 1180, 80, None, None),
                                 (55, 760, 130, 780, None, None), (770, 775, 1330, 1062, None, None)],
    # the perspective grid's own caption, bottom right of the slop panel
    "footage/game/S34.mp4": [(500, 1033, 958, 1070, None, None)],
}

WEAK = set("a an the of to in on at by for from with and or its his their your my every each ids no as so is "
           "it this that very most own".split())
NUMBERS = set("one two three four five six seven eight nine ten eleven twelve sixteen twenty".split())
CONJ = set("and but or so then as with because while when where which that".split())
PREP = set("to from at in on for of by into across under between above beside without than".split())


def log(*a) -> None:
    print("[subs]", *a, file=sys.stderr, flush=True)


def resolve(p: str | None) -> Path | None:
    if p is None:
        return None
    q = Path(p)
    if q.is_absolute():
        return q
    return q.resolve() if (Path.cwd() / q).exists() else (FILM / q)


# ------------------------------------------------------------ the words ----

def norm(s: str) -> list[str]:
    """Words as the clock writes them: lower case, apostrophes dropped, split on anything else."""
    s = s.lower().replace("’", "'").replace("'", "")
    return [w for w in re.split(r"[^a-z0-9]+", s) if w]


def film_words(clock: dict, tl: dict | None) -> list[dict]:
    out = []
    if tl:
        for s in tl["shots"]:
            for w in s.get("words", []):
                t = s["start"] + float(w["t"])
                out.append({"w": norm(w["w"]), "t": t, "e": s["start"] + float(w.get("e", w["t"]))})
    else:
        for s in clock["shots"]:
            for w in s.get("words", []):
                out.append({"w": norm(w["w"]), "t": float(w["t"]), "e": None})
    out.sort(key=lambda x: x["t"])
    flat = []
    for x in out:
        for k, w in enumerate(x["w"]):     # "id's" is one word; a word the clock wrote with a hyphen splits
            flat.append({"w": w, "t": x["t"], "e": x["e"]})
    for i, x in enumerate(flat):
        if x["e"] is None:
            x["e"] = flat[i + 1]["t"] if i + 1 < len(flat) else x["t"] + 0.3
    return flat


def tokens(text: str, numerals: bool) -> list[dict]:
    """The line's words as shown (with their punctuation), each with the words it speaks."""
    toks = [{"d": d, "sp": norm(d)} for d in text.split()]
    toks = [t for t in toks if t["sp"]] or toks
    if not numerals:
        return toks
    out, i = [], 0
    while i < len(toks):
        hit = None
        for phrase, shown in DISPLAY:
            ph = phrase.split()
            acc, j = [], i
            while j < len(toks) and len(acc) < len(ph):
                acc += toks[j]["sp"]
                j += 1
            if acc == ph:
                hit = (j, shown)
                break
        if hit:
            j, shown = hit
            first, last = toks[i]["d"], toks[j - 1]["d"]
            lead = re.match(r"^[\"“(]*", first).group()
            trail = re.search(r"[.,:;!?\"”)]*$", last).group()
            word = shown[0].upper() + shown[1:] if first.lstrip("\"“(")[:1].isupper() else shown
            out.append({"d": lead + word + trail, "sp": sum((t["sp"] for t in toks[i:j]), [])})
            i = j
        else:
            out.append(toks[i])
            i += 1
    return out


def lines_with_times(clock: dict, voice: dict, fw: list[dict], numerals: bool) -> list[dict]:
    """Each voice-map line, its clock entries (a line the edit split is several), and its words timed."""
    vm = {x["id"]: x for x in voice["lines"]}
    entries: dict[str, list[dict]] = {}
    order = []
    for v in clock["voice"]:
        lid = v["id"] if v["id"] in vm else re.sub(r"[a-z]$", "", v["id"])
        if lid not in vm:
            log(f"warning: clock line {v['id']} is not in the voice map; its own text is used")
            vm[lid] = {"id": lid, "text": v["text"]}
        if lid not in entries:
            entries[lid] = []
            order.append(lid)
        entries[lid].append(v)
    out = []
    for lid in order:
        ents = entries[lid]
        toks = tokens(vm[lid]["text"], numerals)
        lo, hi = min(e["speech"][0] for e in ents) - 0.12, max(e["speech"][1] for e in ents) + 0.12
        words = [w for w in fw if lo <= w["t"] <= hi]
        a = [s for t in toks for s in t["sp"]]
        whose = [k for k, t in enumerate(toks) for _ in t["sp"]]
        b = [w["w"] for w in words]
        sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
        at: dict[int, dict] = {}
        for blk in sm.get_matching_blocks():
            for k in range(blk.size):
                at[blk.a + k] = words[blk.b + k]
        unmatched = [a[i] for i in range(len(a)) if i not in at]
        if unmatched:
            log(f"warning: {lid}: no clock time for {' '.join(unmatched)} (interpolated)")
        for k, t in enumerate(toks):
            idx = [i for i in range(len(a)) if whose[i] == k and i in at]
            t["t0"] = at[idx[0]]["t"] if idx else None
            t["t1"] = at[idx[-1]]["e"] if idx else None
        # interpolate the unmatched from their neighbours, inside the line's speech
        for k, t in enumerate(toks):
            if t["t0"] is None:
                prev = next((toks[j]["t1"] for j in range(k - 1, -1, -1) if toks[j]["t1"] is not None), lo + 0.12)
                nxt = next((toks[j]["t0"] for j in range(k + 1, len(toks)) if toks[j]["t0"] is not None), hi - 0.12)
                t["t0"], t["t1"] = prev, max(prev + 0.05, nxt)
        # a word's end never runs past its clock entry's speech end
        for t in toks:
            for e in ents:
                if e["speech"][0] - 0.12 <= t["t0"] <= e["speech"][1] + 0.12:
                    t["t1"] = min(t["t1"], e["speech"][1])
                    t["entry"] = e["id"]
            t["t1"] = max(t["t1"], t["t0"] + 0.05)
        out.append({"id": lid, "entries": [e["id"] for e in ents], "tokens": toks})
    return out


# ---------------------------------------------------------- the captions ----

def brk(a: str, b: str | None) -> float:
    """What a break after shown word a (before b) costs: 0 at a sentence's end, more inside a phrase."""
    if b is None:
        return 0.0
    if re.search(r"[.?!][\"”)]*$", a):
        return 0.0
    if re.search(r"[:;][\"”)]*$", a):
        return 1.0
    if re.search(r",[\"”)]*$", a):
        return 2.0
    aw, bw = (norm(a) or [""])[-1], (norm(b) or [""])[0]
    if aw in WEAK:
        return 15.0
    if bw in CONJ:
        return 4.0
    if bw in ("of", "from"):            # "strings of letters", "departure from id": they bind to the noun before
        return 9.0
    if bw in PREP or bw in NUMBERS or re.match(r"^[\"“]?\d", b):   # a quantity starts a phrase
        return 5.0
    return 8.0


def wrap(words: list[str]) -> tuple[list[str], float, int | None] | None:
    """One line, or two broken at the best phrase boundary (and the word index of the break); None if it can't fit."""
    text = " ".join(words)
    best = None
    if len(text) <= MAXL:
        return [text], 0.0, None
    if len(text) <= HARDL:
        best = ([text], (len(text) - MAXL) * 2.5, None)
    for k in range(1, len(words)):
        l1, l2 = " ".join(words[:k]), " ".join(words[k:])
        if max(len(l1), len(l2)) > HARDL:
            continue
        c = 2.0 * brk(words[k - 1], words[k]) + abs(len(l1) - len(l2)) / 5.0 + (0.8 if len(l1) > len(l2) else 0.0)
        c += 1.5 * (max(0, len(l1) - MAXL) + max(0, len(l2) - MAXL))
        if best is None or c < best[1]:
            best = ([l1, l2], c, k)
    return best


def segment(toks: list[dict], cuts: list[float], one_line: bool = False) -> list[list[dict]]:
    """Split a stretch of speech into captions: dynamic programming over the word boundaries."""
    n = len(toks)
    words = [t["d"] for t in toks]

    def seg_cost(i: int, j: int) -> float:
        w = wrap(words[i:j])
        if w is None or (one_line and len(w[0]) > 1):
            return math.inf if j - i > 1 else 50.0
        lines, c, _ = w
        d = toks[j - 1]["t1"] - toks[i]["t0"]
        c += 4.0                                           # each caption costs: fewer, fuller captions
        if d + HANG > MAX_DUR:
            c += 40.0 * (d + HANG - MAX_DUR)
        if d < 0.6:
            c += 6.0 * (0.6 - d)
        chars = sum(len(x) for x in lines)
        if chars < 14 and not (i == 0 and j == n):
            c += 4.0
        if chars / max(0.5, d + HANG) > 20.0:              # reading speed, characters a second
            c += 0.5 * (chars / max(0.5, d + HANG) - 20.0)
        for k in range(i, j - 1):                          # a sentence ending inside the caption
            if re.search(r"[.?!][\"”)]*$", words[k]):
                c += 8.0 if k + 1 - i != (w[2] or -1) else 3.0     # mid-line is worse than at the line break
                if not re.search(r"[.?!:][\"”)]*$", words[j - 1]):
                    c += 5.0                                  # and the next sentence runs on past the caption
        for ct in cuts:                                    # spanning a cut, where the phrasing allows otherwise
            if toks[i]["t0"] + 0.2 < ct < toks[j - 1]["t1"] - 0.2:
                c += 8.0
        return c

    def boundary(j: int) -> float:
        if j == n:
            return 0.0
        c = 3.0 * brk(words[j - 1], words[j])
        a, b = toks[j - 1]["t1"], toks[j]["t0"]
        if any(a - 0.15 <= ct <= b + 0.15 for ct in cuts):  # a break on a cut is welcome
            c -= 5.0
        return c

    best = [0.0] + [math.inf] * n
    back = [0] * (n + 1)
    for j in range(1, n + 1):
        for i in range(max(0, j - 30), j):
            if best[i] == math.inf:
                continue
            c = best[i] + seg_cost(i, j) + boundary(j)
            if c < best[j]:
                best[j], back[j] = c, i
    out, j = [], n
    while j > 0:
        i = back[j]
        out.append(toks[i:j])
        j = i
    return out[::-1]


def build_captions(lines: list[dict], cuts: list[float]) -> list[dict]:
    caps = []
    for ln in lines:
        toks = ln["tokens"]
        units, cur = [], [toks[0]]
        for a, b in zip(toks, toks[1:]):
            if b["t0"] - a["t1"] > SPLIT_GAP or b.get("entry") != a.get("entry"):
                units.append(cur)
                cur = []
            cur.append(b)
        units.append(cur)
        for u in units:
            for part in segment(u, cuts):
                caps.append(speech_caption(ln["id"], [{"d": t["d"], "spoken": " ".join(t["sp"]),
                                                       "t0": round(t["t0"], 3), "t1": round(t["t1"], 3)} for t in part]))
    return caps


def speech_caption(line: str, words: list[dict]) -> dict:
    lines_, _, k = wrap([w["d"] for w in words]) or ([" ".join(w["d"] for w in words)], 0, None)
    return {"line": line, "text": lines_, "break_at": k, "t_first": words[0]["t0"], "start": words[0]["t0"],
            "speech_end": words[-1]["t1"], "words": words, "kind": "speech"}


def as_one_liners(c: dict, cuts: list[float]) -> list[dict]:
    """A two-line caption's words again, as captions of one line each."""
    toks = [{"d": w["d"], "sp": w["spoken"].split(), "t0": w["t0"], "t1": w["t1"]} for w in c["words"]]
    return [speech_caption(c["line"], [{"d": t["d"], "spoken": " ".join(t["sp"]), "t0": t["t0"], "t1": t["t1"]}
                                       for t in part]) for part in segment(toks, cuts, one_line=True)]


def as_two_lines(c: dict) -> dict:
    """A wide one-line caption as two lines, broken at its best phrase boundary (narrower, it may clear a column)."""
    words = [w["d"] for w in c["words"]]
    best = None
    for k in range(1, len(words)):
        l1, l2 = " ".join(words[:k]), " ".join(words[k:])
        cost = 2.0 * brk(words[k - 1], words[k]) + abs(len(l1) - len(l2)) / 5.0
        if best is None or cost < best[0]:
            best = (cost, [l1, l2], k)
    d = speech_caption(c["line"], c["words"])
    if best:
        d["text"], d["break_at"] = best[1], best[2]
    return d


def settle(caps: list[dict], cuts: list[float], duration: float, occ: list[dict], shots: list[dict]) -> list[dict]:
    """Time the captions and give each shot its lane. A shot that is off the bottom lane, or not clear, then tries
    its two-line captions as one-liners; a shot with no clear lane also tries its wide one-liners as two lines. A
    trial is kept if it brings the shot back to the bottom lane clear, or (with no clear lane) halves what the
    captions cover."""
    def run(cs: list[dict]) -> None:
        time_captions(cs, cuts, duration)
        for c in cs:
            c["shots"] = [s["id"] for s in shots if s["start"] < c["end"] - 0.02 and s["end"] > c["start"] + 0.02]
        place(cs, occ, shots)

    def one(c: dict) -> list[dict]:
        return as_one_liners(c, cuts) if c["kind"] == "speech" and len(c["text"]) == 2 else [c]

    def narrow(c: dict) -> list[dict]:
        return [as_two_lines(c)] if c["kind"] == "speech" and len(c["text"]) == 1 and len(c["text"][0]) >= 30 else [c]

    trials = [one, narrow, lambda c: [y for x in one(c) for y in narrow(x)]]   # the last: re-cut, then narrow
    run(caps)
    tried: set = set()
    while True:
        todo = [c for c in caps if (c["where"] != "bottom" or c.get("placement_note")) and tuple(c["lane_group"]) not in tried]
        if not todo:
            return caps
        grp = tuple(todo[0]["lane_group"])
        tried.add(grp)
        for n_trial, trial_of in enumerate(trials):
            members = [c for c in caps if tuple(c["lane_group"]) == grp]
            before = sum(c["hits"] for c in members)
            if before == 0 and (n_trial > 0 or all(c["where"] == "bottom" for c in members)):
                break           # narrowing is only for a shot with no clear lane, not to win back the bottom
            changed, trial = False, []
            for c in caps:
                pieces = trial_of(c) if c in members else [c]
                changed |= pieces != [c]
                trial += pieces
            if not changed:
                continue
            run(trial)
            new = [c for c in trial if set(c["lane_group"]) & set(grp)]
            ok_len = all(c["end"] - c["start"] >= MIN_DUR - 1e-3 for c in new if c["kind"] == "speech")
            if ok_len and (all(c["where"] == "bottom" and not c.get("placement_note") for c in new) or
                           (before > 0 and sum(c["hits"] for c in new) < before / 2)):
                caps = trial
                for c in caps:      # the regrouped shots count as tried
                    if set(c["lane_group"]) & set(grp):
                        tried.add(tuple(c["lane_group"]))
            else:
                run(caps)


def add_tags(caps: list[dict], events: dict[str, float]) -> list[dict]:
    for ev, dur, text in SOUND_TAGS:
        if ev in events:
            t = events[ev]
            nxt = min((c["start"] for c in caps if c["start"] > t + 0.05), default=t + 3.0)
            end = nxt if dur is None else t + dur
            caps.append({"line": f"tag:{ev}", "text": [text], "start": t, "t_first": t, "speech_end": min(end, nxt),
                         "words": [], "kind": "tag", "tag_end": end})
    caps.sort(key=lambda c: c["start"])
    return caps


def time_captions(caps: list[dict], cuts: list[float], duration: float) -> None:
    """Start on the first word, end HANG after the last, with shot changes as the subtitler's grid: a caption whose
    words start just after a cut starts on it, one whose words end just before a cut ends on it; captions closer
    than CHAIN with no cut between touch; 1-6 s each, GAP between."""
    for k, c in enumerate(caps):
        s = c["t_first"]
        prev_speech = max([p["speech_end"] for p in caps[:k] if p["kind"] == "speech"], default=0.0)
        before = [ct for ct in cuts if s - LEAD_SNAP <= ct < s and ct >= prev_speech]
        after = [ct for ct in cuts if s < ct <= s + LAG_SNAP and ct < c["speech_end"] - 0.3]
        if c["kind"] == "speech":
            if before:
                s = before[-1]
            elif after:
                s = after[0]
        c["start"] = s
        c["end"] = c["tag_end"] if c["kind"] == "tag" else c["speech_end"] + HANG
    for k, c in enumerate(caps):
        nxt = caps[k + 1]["start"] if k + 1 < len(caps) else duration
        if c["kind"] == "speech":
            # a cut soon after the last word: end on it (if that still leaves a second)
            near = [ct for ct in cuts if c["speech_end"] - 0.02 <= ct <= c["speech_end"] + HANG + 0.5
                    and ct >= c["start"] + MIN_DUR]
            if near:
                c["end"] = near[0]
        between = [ct for ct in cuts if c["end"] < ct < nxt]
        if not between and nxt - c["end"] < CHAIN:
            c["end"] = nxt - GAP
        if c["end"] - c["start"] < MIN_DUR:
            c["end"] = c["start"] + MIN_DUR
        c["end"] = min(c["end"], c["start"] + MAX_DUR, nxt - GAP, duration)
    for c in caps:   # on the film's frames: a caption that starts on a cut shows on the cut's first frame
        c["start"], c["end"] = round(c["start"] * FPS) / FPS, round(c["end"] * FPS) / FPS


# ------------------------------------------------------------ placement ----

def text_width(lines: list[str]) -> int:
    from PIL import ImageFont
    f = ImageFont.truetype(_font()[0], round(EM), index=_font()[1])
    return int(max(f.getlength(x) for x in lines))


def box_size(lines: list[str]) -> tuple[int, int]:
    return text_width(lines) + 2 * PAD, LINE_PX * len(lines) + 2 * PAD


def cells(x0: float, y0: float, x1: float, y1: float, m: float = 0.0) -> np.ndarray:
    x0, y0, x1, y1 = x0 - m, y0 - m, x1 + m, y1 + m
    g = np.zeros((H // GRID, W // GRID), bool)
    a, b = max(0, int(x0 // GRID)), min(W // GRID, int(math.ceil(x1 / GRID)))
    c, d = max(0, int(y0 // GRID)), min(H // GRID, int(math.ceil(y1 / GRID)))
    if a < b and c < d:
        g[c:d, a:b] = True
    return g


def alpha_grids(path: Path) -> dict:
    """An overlay movie's alpha on the grid, 10 times a second: {"dt": 0.1, "grids": [[row, c0, c1], ...] per sample}."""
    st = path.stat()
    key = hashlib.sha256(f"{path}|{st.st_size}|{int(st.st_mtime)}|{GRID}|v2".encode()).hexdigest()[:20]
    cp = SCRATCH / "alpha" / f"{key}.json"
    if cp.exists():
        return json.loads(cp.read_text())
    cp.parent.mkdir(parents=True, exist_ok=True)
    log(f"overlay map: {path.name}")
    p = subprocess.run(["nice", "-n", "10", "ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-vf",
                        f"fps=10,format=yuva444p,alphaextract,scale={W}:{H}", "-f", "rawvideo", "-pix_fmt", "gray", "-"],
                       capture_output=True, check=False)
    raw = np.frombuffer(p.stdout, np.uint8)
    nfr = raw.size // (W * H)
    out = {"dt": 0.1, "grids": []}
    if p.returncode != 0 or nfr == 0:
        log(f"warning: no alpha from {path.name}; it is taken as covering nothing")
        cp.write_text(json.dumps(out))
        return out
    a = raw[: nfr * W * H].reshape(nfr, H // GRID, GRID, W // GRID, GRID).max(axis=(2, 4)) > 40
    for g in a:
        runs = []
        for r in range(g.shape[0]):
            cols = np.flatnonzero(g[r])
            if cols.size:
                # contiguous runs
                splits = np.flatnonzero(np.diff(cols) > 1)
                starts = np.r_[cols[0], cols[splits + 1]]
                ends = np.r_[cols[splits], cols[-1]]
                runs += [[int(r), int(s), int(e)] for s, e in zip(starts, ends)]
        out["grids"].append(runs)
    cp.write_text(json.dumps(out))
    return out


def grid_from_runs(runs: list) -> np.ndarray:
    g = np.zeros((H // GRID, W // GRID), bool)
    for r, a, b in runs:
        g[r, a:b + 1] = True
    return g


def label_grid(o: dict) -> np.ndarray:
    """edit/cards.py's ov_label: the band round the text, and its arrow (a run of small boxes along the line)."""
    sc = int(o.get("scale", 3))
    if o.get("box"):
        return cells(*o["box"], m=CLEAR)
    text = o.get("text", "")
    x, y = o.get("x", 40), o.get("y", 40)
    pad = int(o.get("pad", 0))
    w = max(max(len(s) for s in text.split("\n")) * 8 * sc, int(o.get("min_w", 0)))
    g = cells(x - 12 - pad, y - 10 - pad, x + w + 12 + pad, y + 8 * sc + 10 + int(o.get("pad_h", 0)) + pad, m=CLEAR)
    if "ax" in o:
        x0, y0, x1, y1 = x - 16, y + 4 * sc, o["ax"], o["ay"]
        for k in range(21):
            px, py = x0 + (x1 - x0) * k / 20, y0 + (y1 - y0) * k / 20
            g |= cells(px - 14, py - 14, px + 14, py + 14)
    return g


def caption_rect(o: dict) -> tuple[float, float, float, float]:
    main, sub = o.get("text", ""), o.get("sub") or ""
    block = 32 + ((12 + 16) if sub else 0)
    width = max(len(main) * 32, len(sub) * 16)
    bottom = H - 120
    return 76, bottom - block - 18, 96 + width + 20, bottom + 18


CARD_RECTS = {   # card shots: where their text is (edit/cards.py); a fifth number weighs a picture below text
    "title": [(0, 400, W, 660)],
    "bd1": [(0, 160, W, 206), (40, 200, 1880, 880, 0.2)],     # the two labels; the frames sliding into the diff
    "endcard": [(0, 230, W, 800)], "endcard_v3": [(0, 280, W, 760)], "endcard_v4": [(0, 230, W, 800)],
    "terminal": [(100, 130, W, 600), (0, 790, W, 930)],
}
NAMED_OVERLAY_RECTS = {
    "frame_counter": lambda o: [(o.get("x", 48) - 12, o.get("y", 990) - 10, o.get("x", 48) + 8 * 24 + 12, o.get("y", 990) + 34)],
    "terminal_over": lambda o: [(100, 130, W, 600), (0, 790, W, 930)],
    "rules": lambda o: [(230, 390, W, 630)],
    "menu_row": lambda o: [(W - 96 - 600 - 60, 60, W - 72, 128)],
}


def occupancy(tl: dict) -> list[dict]:
    """Where the film's own text and bands are, in film time: [{t0, t1, grid | grids, what}]."""
    occ = []
    for s in tl["shots"]:
        S, L = float(s["start"]), float(s["len"])
        src = s.get("source", {})
        if src.get("type") == "card" and src.get("name") in ("bd3", "bd4"):   # the small label (2x, at 96, 960);
            cues, co = s.get("cues") or {}, s.get("card_opts") or {}             # bd3: the four tiles on their cue
            full = co.get("label", "ID'S C - THE PORT · E1M1 · 320×200")
            lab = float(cues.get("label", 1.9)) if src["name"] == "bd3" else 0.2
            grid = cues.get("grid") if src["name"] == "bd3" else None
            g_at = S + float(grid) if grid is not None else S + L
            occ.append({"t0": S + lab, "t1": g_at, "grid": cells(90, 956, 100 + 16 * len(full), 980, m=CLEAR),
                        "what": f"{s['id']} card label"})
            if grid is not None:
                short = co.get("label_grid") or full
                occ.append({"t0": g_at, "t1": S + L, "grid": cells(90, 956, 100 + 16 * len(short), 980, m=CLEAR),
                            "what": f"{s['id']} card label"})
                for k in range(4):     # cards.py card_bd3: 520x390 tiles from (410, 70), 60 and 70 apart
                    x, y = 410 + (k % 2) * 580, 70 + (k // 2) * 460
                    occ.append({"t0": g_at, "t1": S + L, "grid": cells(x - 2, y - 2, x + 522, y + 392), "w": 0.2,
                                "what": f"{s['id']} card bd3 tile (a black diff)"})
                    occ.append({"t0": g_at, "t1": S + L, "grid": cells(x - 4, y + 400, x + 260, y + 424, m=CLEAR),
                                "what": f"{s['id']} card bd3 tile label"})
        elif src.get("type") == "card":
            for r in CARD_RECTS.get(src.get("name", ""), []):
                occ.append({"t0": S, "t1": S + L, "grid": cells(*r[:4], m=CLEAR), "w": r[4] if len(r) > 4 else 1.0,
                            "what": f"{s['id']} card {src.get('name')}"})
            if src.get("name") not in CARD_RECTS and src.get("name") not in ("black",):
                log(f"note: {s['id']}: card {src.get('name')} has no known text area; check it by looking")
        fin, sp = float(src.get("in") or 0.0), float(src.get("speed") or 1.0)
        for key in (src.get("path"), *(src.get(k) for k in ("a", "b"))):
            for r in FOOTAGE_TEXT.get(key or "", []) + FOOTAGE_TEXT.get(f"{key}#{s['id']}", []):
                a = 0.0 if r[4] is None else max(0.0, (r[4] - fin) / sp)
                b = L if r[5] is None else min(L, (r[5] - fin) / sp)
                if b > a:
                    occ.append({"t0": S + a, "t1": S + b, "grid": cells(*r[:4], m=CLEAR),
                                "what": f"{s['id']} burned-in text in {key}"})
        for o in s.get("overlays", []):
            ty = o.get("type")
            at = float(o.get("at") or 0.0)
            until = o.get("until")
            t1 = S + (float(until) if until is not None else L)
            if ty == "video":
                p = FILM / o["path"]
                if p.suffix != ".mov" or not p.exists():
                    continue
                ag = alpha_grids(p)
                fin = float(o.get("in") or 0.0)
                sc = float(o.get("scale") or 1.0)
                ox, oy = int(o.get("x") or 0), int(o.get("y") or 0)
                en = o.get("enable")
                n = len(ag["grids"])
                if n == 0:
                    continue
                grids = []
                k0 = 0
                st = 0.0
                while st < L:
                    ft = fin + (st - at)
                    if st >= at and (not en or en[0] <= st < en[1]):
                        k = min(n - 1, max(0, int(ft / ag["dt"])))
                        g = grid_from_runs(ag["grids"][k])
                        if sc != 1.0 or ox or oy:
                            ys, xs = np.nonzero(g)
                            g = np.zeros_like(g)
                            for y_, x_ in zip(ys, xs):
                                g |= cells(ox + x_ * GRID * sc, oy + y_ * GRID * sc, ox + (x_ + 1) * GRID * sc,
                                           oy + (y_ + 1) * GRID * sc)
                        grids.append((S + st, g))
                    st += 0.1
                    k0 += 1
                for (ta, g) in grids:
                    if g.any():
                        occ.append({"t0": ta, "t1": min(ta + 0.1, S + L), "grid": g, "what": f"{s['id']} overlay {p.name}"})
            elif ty == "png":
                from PIL import Image
                im = np.asarray(Image.open(FILM / o["path"]).convert("RGBA"))[..., 3]
                if im.shape != (H, W):
                    continue
                g = im.reshape(H // GRID, GRID, W // GRID, GRID).max(axis=(1, 3)) > 40
                occ.append({"t0": S + at, "t1": t1, "grid": g, "what": f"{s['id']} png {Path(o['path']).name}"})
            elif ty == "caption":
                occ.append({"t0": S + at, "t1": t1, "grid": cells(*caption_rect(o), m=CLEAR), "what": f"{s['id']} caption {o.get('text')}"})
            elif ty == "card" and o.get("name") == "label":
                occ.append({"t0": S + at, "t1": t1, "grid": label_grid(o), "what": f"{s['id']} label {o.get('text')}"})
            elif ty == "card" and o.get("name") in NAMED_OVERLAY_RECTS:
                for r in NAMED_OVERLAY_RECTS[o["name"]](o):
                    occ.append({"t0": S + at, "t1": t1, "grid": cells(*r, m=CLEAR), "what": f"{s['id']} {o['name']}"})
            elif ty == "card":
                log(f"note: {s['id']}: overlay {o.get('name')} has no known text area; check it by looking")
    return occ


def lane_box(c: dict, lane: str) -> list[int]:
    w, h = box_size(c["text"])
    x0, x1 = (W - w) // 2, (W + w) // 2
    y0 = {"bottom": BOTTOM_Y1 - h, "top": TOP_Y0, "raised": RAISED_Y1 - h}[lane]
    return [int(x0), int(y0), int(x1), int(y0 + h)]


def place(caps: list[dict], occ: list[dict], shots: list[dict]) -> None:
    """One of three fixed lanes for each shot, all its captions together: the first lane in LANES that is clear
    of the film's text for every caption in the shot; if none is, the one that covers least (a black tile weighs
    0.2, text 1). Shots that share a caption (one spanning a cut) share a lane."""
    for c in caps:
        live = [o for o in occ if o["t0"] < c["end"] - 0.04 and o["t1"] > c["start"] + 0.04]
        g = np.zeros((H // GRID, W // GRID))
        for o in live:
            g = np.maximum(g, o["grid"] * o.get("w", 1.0))
        c["_g"] = g
        c["_hits"] = {ln: float((g * cells(*(lambda b: (b[0] + 4, b[1] + 4, b[2] - 4, b[3] - 4))(lane_box(c, ln))))
                                .sum()) for ln in LANES}
        b = lane_box(c, "bottom")
        c["because"] = sorted({o["what"] for o in live if (o["grid"] & cells(*b)).any()})
    # group shots joined by a caption, and the captions in them
    parent: dict[str, str] = {}

    def find(x: str) -> str:
        while parent.setdefault(x, x) != x:
            x = parent[x]
        return x
    for c in caps:
        ids = c["shots"] or [next((s["id"] for s in shots if s["start"] <= c["start"] + 0.02 < s["end"]), "?")]
        c["shots"] = ids
        for a in ids[1:]:
            parent[find(a)] = find(ids[0])
    groups: dict[str, list[dict]] = {}
    for c in caps:
        groups.setdefault(find(c["shots"][0]), []).append(c)
    for root, cs in groups.items():
        total = {ln: sum(c["_hits"][ln] for c in cs) for ln in LANES}
        lane = next((ln for ln in LANES if total[ln] == 0), None) or min(LANES, key=lambda ln: total[ln])
        for c in cs:
            c["where"], c["box"], c["hits"] = lane, lane_box(c, lane), c["_hits"][lane]
            c["y0"] = c["box"][1]
            c["lane_group"] = sorted({x for d in cs for x in d["shots"]})
            if c["hits"] > 0:
                c["placement_note"] = f"no clear lane for {'+'.join(c['lane_group'])}: {c['hits']:.1f} cells shared"
            else:
                c.pop("placement_note", None)
    for c in caps:
        c.pop("_g", None)
        c.pop("_hits", None)


# --------------------------------------------------------------- writing ----

def ts(t: float, sep: str) -> str:
    ms = int(math.floor(t * 1000 + 1e-6))     # never later than the frame it belongs to
    h, ms = divmod(ms, 3600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d}{sep}{ms:03d}"


def write_srt(caps: list[dict], path: Path) -> None:
    out = []
    for k, c in enumerate(caps, 1):
        out.append(f"{k}\n{ts(c['start'], ',')} --> {ts(c['end'], ',')}\n" + "\n".join(c["text"]) + "\n")
    path.write_text("\n".join(out))


def write_vtt(caps: list[dict], path: Path) -> None:
    out = ["WEBVTT", ""]
    for k, c in enumerate(caps, 1):
        cue = {"bottom": f" line:{100 * BOTTOM_Y1 / H:.1f}%,end align:center",
               "top": f" line:{100 * TOP_Y0 / H:.1f}% align:center",
               "raised": f" line:{100 * RAISED_Y1 / H:.1f}%,end align:center"}[c["where"]]
        out.append(f"{k}\n{ts(c['start'], '.')} --> {ts(c['end'], '.')}{cue}\n" + "\n".join(c["text"]) + "\n")
    path.write_text("\n".join(out))


def ass_colour(rgb, alpha: float = 0.0) -> str:
    r, g, b = rgb
    return f"&H{int(round(alpha * 255)):02X}{b:02X}{g:02X}{r:02X}"


def ass_time(t: float) -> str:
    cs = int(math.floor(t * 100 + 1e-6))
    h, cs = divmod(cs, 360000)
    m, cs = divmod(cs, 6000)
    s, cs = divmod(cs, 100)
    return f"{h}:{m:02d}:{s:02d}.{cs:02d}"


def write_ass(caps: list[dict], path: Path) -> None:
    band = ass_colour(BAND_RGB, 1.0 - BAND_ALPHA)
    head = f"""[Script Info]
ScriptType: v4.00+
PlayResX: {W}
PlayResY: {H}
WrapStyle: 2
ScaledBorderAndShadow: yes
YCbCr Matrix: TV.709

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Sub,{FONT_NAME},{FONT_PX},{ass_colour(TEXT_RGB)},{ass_colour(TEXT_RGB)},{band},{band},0,0,0,0,100,100,0,0,3,{PAD},0,2,60,60,{H - BOTTOM_Y1 + PAD},1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
"""
    ev = []
    for c in caps:
        x0, y0, x1, y1 = c["box"]
        pos = (f"{{\\an8\\pos({W // 2},{y0 + PAD})}}" if c["where"] == "top" else f"{{\\an2\\pos({W // 2},{y1 - PAD})}}")
        text = "\\N".join(x.replace("{", "(").replace("}", ")") for x in c["text"])
        ev.append(f"Dialogue: 0,{ass_time(c['start'])},{ass_time(c['end'])},Sub,,0,0,0,,{pos}{text}")
    path.write_text(head + "\n".join(ev) + "\n")


# ----------------------------------------------------------------- check ----

_ONES = ("zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen "
         "seventeen eighteen nineteen").split()
_TENS = "_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()


def _say(n: int) -> str:
    if n < 20:
        return _ONES[n]
    if n < 100:
        return _TENS[n // 10] + ("" if n % 10 == 0 else " " + _ONES[n % 10])
    if n < 1000:
        return _ONES[n // 100] + " hundred" + ("" if n % 100 == 0 else " and " + _say(n % 100))
    if n < 1_000_000:
        return _say(n // 1000) + " thousand" + ("" if n % 1000 == 0 else (" and " if n % 1000 < 100 else " ") + _say(n % 1000))
    return str(n)


def say_number(s: str) -> str:
    s = s.replace(",", "")
    if "." in s:
        a, b = s.split(".", 1)
        return _say(int(a or 0)) + " point " + " ".join(_ONES[int(c)] for c in b if c.isdigit())
    n = int(s)
    if 1000 <= n <= 2099 and n % 100 != 0 and not 2000 <= n <= 2009:
        return _say(n // 100) + " " + (_say(n % 100) if n % 100 >= 10 else "oh " + _say(n % 100))
    return _say(n)


def heard_toks(text: str) -> list[str]:
    """Shown or heard text as the words a listener hears (the viewing kit's normalising, and a few more)."""
    s = re.sub(r"\b[A-Z](?: [A-Z]\b){2,}", lambda m: m.group().replace(" ", ""), text)   # "W A S D" -> "WASD"
    s = s.lower().replace("’", "'").replace("‘", "'")
    s = re.sub(r"\[[^\]]*\]", " ", s)
    s = re.sub(r"\ba[\s-]+(hundred|thousand)\b", r"one \1", s)
    s = re.sub(r"(\d+)p\b", r"\1", s)
    s = re.sub(r"\bhz\b", "hertz", s)
    s = re.sub(r"\d[\d,]*(?:\.\d+)?", lambda m: " " + say_number(m.group()) + " ", s)
    s = re.sub(r"[-–—/+:;,.!?\"“”()*_]", " ", s).replace("'", "")
    return [{"z": "zee"}.get(x, x) for x in s.split() if x]


def extract_audio(cut: Path) -> Path:
    a = SCRATCH / "audio" / f"{cut.stem}-{cut.stat().st_size}-{int(cut.stat().st_mtime)}.m4a"
    if not a.exists():
        a.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(cut), "-vn", "-c:a", "copy", str(a)],
                       check=True)
    return a


def scribe(cut: Path) -> dict | None:
    audio = extract_audio(cut)
    h = hashlib.sha256(audio.read_bytes()).hexdigest()[:20]
    for d in (SCRATCH / "scribe", KIT_SCRIBE):
        if (d / f"{h}.json").exists():
            log(f"Scribe: cached ({d / f'{h}.json'})")
            return json.loads((d / f"{h}.json").read_text())
    if not os.environ.get(KEY_ENV):
        log(f"Scribe: no {KEY_ENV}; the check is skipped")
        return None
    import httpx
    log("Scribe is listening to the cut")
    key = os.environ[KEY_ENV].strip()
    try:
        with open(audio, "rb") as f:
            r = httpx.post("https://api.elevenlabs.io/v1/speech-to-text", headers={"xi-api-key": key},
                           data={"model_id": "scribe_v1", "timestamps_granularity": "word", "language_code": "en",
                                 "tag_audio_events": "true"},
                           files={"file": (audio.name, f, "audio/mp4")}, timeout=900)
    except Exception as ex:  # noqa: BLE001  (never echo the request: it carries the key)
        log(f"Scribe: failed ({type(ex).__name__})")
        return None
    finally:
        key = None  # noqa: F841
    if r.status_code != 200:
        log(f"Scribe: HTTP {r.status_code}")
        return None
    j = r.json()
    (SCRATCH / "scribe").mkdir(parents=True, exist_ok=True)
    (SCRATCH / "scribe" / f"{h}.json").write_text(json.dumps(j))
    return j


def check(caps: list[dict], sc: dict, name: str, out: Path) -> dict:
    heard = []
    ws = [w for w in sc.get("words", []) if w.get("type", "word") == "word"]
    for k, w in enumerate(ws):
        nxt = ws[k + 1]["text"].lower().strip(" ,.") if k + 1 < len(ws) else ""
        text = "one" if w["text"].lower().strip() == "a" and nxt in ("hundred", "thousand") else w["text"]
        for t in heard_toks(text):
            heard.append({"w": t, "t": float(w["start"]), "e": float(w["end"]), "raw": w["text"]})
    shown, whose = [], []
    for k, c in enumerate(caps):
        if c["kind"] != "speech":
            continue
        for t in heard_toks(" ".join(c["text"])):
            shown.append(t)
            whose.append(k)
    sm = difflib.SequenceMatcher(None, shown, [h["w"] for h in heard], autojunk=False)
    diffs, offs, outside = [], [], []
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            for k in range(i2 - i1):
                c = caps[whose[i1 + k]]
                hw = heard[j1 + k]
                if not (c["start"] - 0.3 <= hw["t"] and hw["e"] <= c["end"] + 0.3):
                    outside.append(f"{hw['raw']} at {hw['t']:.2f} (caption {whose[i1 + k] + 1}: "
                                   f"{c['start']:.2f}-{c['end']:.2f})")
            continue
        a, b = shown[i1:i2], [h["w"] for h in heard[j1:j2]]
        k = whose[i1] if i1 < len(whose) else whose[-1]
        t = heard[j1]["t"] if j1 < len(heard) and b else caps[k]["start"]
        brit = lambda x: re.sub(r"our(s?)$", r"or\1", re.sub(r"ence$", "ense", x))   # colours/colors, licence/license
        kind = ("spelling" if "".join(a) == "".join(b) or [brit(x) for x in a] == b else
                "near" if a and b and difflib.SequenceMatcher(None, "".join(a), "".join(b)).ratio() >= 0.75 else
                {"replace": "different", "delete": "not heard", "insert": "heard, not shown"}[op])
        diffs.append({"caption": k + 1, "t": round(t, 2), "shown": " ".join(a), "heard": " ".join(b), "kind": kind,
                      "text": " / ".join(caps[k]["text"])})
    # each caption's first word: shown when it is said?
    for k, c in enumerate(caps):
        if c["kind"] != "speech" or not c["words"]:
            continue
        idx = [i for i, o in enumerate(whose) if o == k]
        if not idx:
            continue
    for blk in sm.get_matching_blocks():
        for q in range(blk.size):
            c = caps[whose[blk.a + q]]
            offs.append(heard[blk.b + q]["t"] - c["start"])
    matched = sum(b.size for b in sm.get_matching_blocks())
    res = {"cut": name, "shown_words": len(shown), "heard_words": len(heard), "matched": matched,
           "match_rate": round(matched / max(1, len(shown)), 4), "diffs": diffs, "heard_outside_its_caption": outside}
    md = [f"# {name}: the subtitles against what Scribe hears", "",
          f"- Shown words (as heard): {len(shown)}; Scribe's words: {len(heard)}; in agreement: {matched} "
          f"({100 * res['match_rate']:.1f}%).",
          f"- Words heard outside their caption's time (±0.3 s): {len(outside)}.", "",
          "| # | time | shown | heard | kind | caption |", "|---|---|---|---|---|---|"]
    for d in diffs:
        md.append(f"| {d['caption']} | {d['t']:.2f} | {d['shown']} | {d['heard']} | {d['kind']} | {d['text']} |")
    if outside:
        md += ["", "Heard outside its caption:", ""] + [f"- {x}" for x in outside]
    out.write_text("\n".join(md) + "\n")
    return res


# -------------------------------------------------------------- picture ----

def ffbg(cmd: list[str]) -> None:
    """A heavy job, at low priority."""
    subprocess.run(["nice", "-n", "10"] + cmd, check=True)


def burn(cut: Path, ass: Path, name: str, out_dir: Path, duration: float, limit_mb: float = 9.2) -> list[Path]:
    work = SCRATCH / "enc" / f"{name}-{int(time.time())}"
    work.mkdir(parents=True, exist_ok=True)
    master = work / "master-480p.mkv"
    fonts = str(Path(_font()[0]).parent)
    vf = f"scale=-2:480:flags=area,subtitles=filename={ass}:fontsdir={fonts}"
    log("burn: the 480p master with the subtitles (lossless)")
    ffbg(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(cut), "-vf", vf, "-c:v", "libx264", "-qp", "0",
          "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "copy", str(master)])
    outs = []
    kbps = int((limit_mb * 8 * 1000) / duration) - 96 - 8
    h264 = out_dir / f"{name}-480p-subs.mp4"
    common = ["-c:v", "libx264", "-preset", "slow", "-b:v", f"{kbps}k", "-pix_fmt", "yuv420p"]
    log(f"burn: H.264 two-pass at {kbps} kbit/s")
    ffbg(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(master), *common, "-pass", "1", "-passlogfile",
          str(work / "x264"), "-an", "-f", "null", "/dev/null"])
    ffbg(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(master), *common, "-pass", "2", "-passlogfile",
          str(work / "x264"), "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart", str(h264)])
    log(f"burn: {h264.name} {h264.stat().st_size / 1e6:.2f} MB")
    outs.append(h264)
    hevc = out_dir / f"{name}-480p-subs-hevc.mp4"
    lim = limit_mb
    for attempt in range(3):
        kb = int((lim * 8 * 1000) / duration) - 96 - 8
        common = ["-pix_fmt", "yuv420p", "-c:v", "libx265", "-preset", "slow", "-b:v", f"{kb}k", "-tag:v", "hvc1"]
        log(f"burn: HEVC two-pass at {kb} kbit/s")
        ffbg(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(master), *common, "-x265-params",
              f"log-level=error:pass=1:stats={work / 'x265.log'}", "-an", "-f", "null", "/dev/null"])
        ffbg(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(master), *common, "-x265-params",
              f"log-level=error:pass=2:stats={work / 'x265.log'}", "-c:a", "aac", "-b:a", "96k",
              "-movflags", "+faststart", str(hevc)])
        mb = hevc.stat().st_size / 1e6
        log(f"burn: {hevc.name} {mb:.2f} MB")
        if mb < 9.9:
            break
        lim *= 9.6 / mb
    outs.append(hevc)
    master.unlink()   # 0.5-1 GB of scratch; the published files are the deliverables
    return outs


def sheets(video: Path, caps: list[dict], ass: Path, name: str, shots: list[dict], per: int = 12) -> list[Path]:
    """One frame for every shot that has captions, at 480p with the subtitles burned in from the .ass (no encode):
    the middle of the shot's first caption, inside the shot, labelled with the shot's lane. The lanes are drawn as
    thin guides at the frame's edge, so a sheet shows at a glance that every shot sits on one of three."""
    from PIL import Image, ImageDraw, ImageFont
    d = SCRATCH / "sheets" / f"{name}-lanes-{int(time.time())}"
    d.mkdir(parents=True, exist_ok=True)
    frames = []
    for sh in shots:
        mine = [c for c in caps if sh["id"] in c["shots"]]
        if not mine:
            continue
        c = mine[0]
        a, b = max(c["start"], sh["start"]), min(c["end"], sh["end"])
        t = (a + b) / 2
        p = d / f"{len(frames) + 1:03d}-{sh['id']}.png"
        vf = f"scale=-2:480:flags=area,subtitles=filename={ass}:fontsdir={Path(_font()[0]).parent}"
        subprocess.run(["nice", "-n", "10", "ffmpeg", "-v", "error", "-nostdin", "-y", "-ss", f"{t:.3f}", "-copyts",
                        "-i", str(video), "-vf", vf, "-frames:v", "1", str(p)], check=True)
        lanes = sorted({x["where"] for x in mine})
        frames.append((sh["id"], t, "/".join(lanes), len(mine), p))
    out = []
    font = ImageFont.truetype(_font()[0], 18, index=_font()[1])
    k = 480 / H
    guide = {"bottom": BOTTOM_Y1 * k, "top": TOP_Y0 * k, "raised": RAISED_Y1 * k}
    for s0 in range(0, len(frames), per):
        grp = frames[s0:s0 + per]
        cols = 3
        rows = math.ceil(len(grp) / cols)
        sheet = Image.new("RGB", (cols * 854, rows * (480 + 26)), (40, 40, 40))
        dr = ImageDraw.Draw(sheet)
        for i, (sid, t, lane, n, p) in enumerate(grp):
            x, y = (i % cols) * 854, (i // cols) * (480 + 26)
            sheet.paste(Image.open(p).convert("RGB"), (x, y + 26))
            for ln, gy in guide.items():      # a tick for each lane at the left edge; the shot's lane in yellow
                col = (255, 220, 60) if ln == lane else (110, 110, 110)
                dr.line([(x, y + 26 + gy), (x + 14, y + 26 + gy)], fill=col, width=3)
            dr.text((x + 6, y + 3), f"{sid}  lane: {lane}  ({n} caption{'s' if n != 1 else ''}, frame at {t:.2f}s)",
                    fill=(255, 220, 120), font=font)
        sp = d / f"lanes-{s0 // per + 1:02d}.png"
        sheet.save(sp)
        out.append(sp)
    return out


# ------------------------------------------------------------------ main ----

def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cut", required=True, help="the cut (it may not exist yet: then the text files only)")
    ap.add_argument("--voice", required=True, help="the voice map, voice/vN-lines.json")
    ap.add_argument("--clock", required=True, help="the edit's clock, edit/vN/clock.json (timeline.json beside it)")
    ap.add_argument("--name", help="the files' name (default: the cut's, without .mp4)")
    ap.add_argument("--out", default=str(FILM / "edit" / "subs"), help="where the files go (default: FILM_ROOT/edit/subs)")
    ap.add_argument("--no-check", action="store_true")
    ap.add_argument("--no-burn", action="store_true")
    ap.add_argument("--sheets", action="store_true", help="frame sheets of every caption for looking (scratch)")
    ap.add_argument("--spoken-numbers", action="store_true", help="numbers as the script spells them")
    a = ap.parse_args()

    cut, voice_p, clock_p = resolve(a.cut), resolve(a.voice), resolve(a.clock)
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    name = a.name or cut.stem
    clock = json.loads(clock_p.read_text())
    tl_p = clock_p.parent / "timeline.json"
    tl = json.loads(tl_p.read_text()) if tl_p.exists() else None
    voice = json.loads(voice_p.read_text())
    duration = float(clock["film"]["duration"])
    cuts = sorted(float(c["t"]) for c in clock["cuts"] if float(c["t"]) > 0)
    events = {e["name"]: float(e["t"]) for e in clock.get("events", [])}

    fw = film_words(clock, tl)
    lines = lines_with_times(clock, voice, fw, numerals=not a.spoken_numbers)
    caps = build_captions(lines, cuts)
    caps = add_tags(caps, events)
    shots = clock["shots"]
    occ = occupancy(tl) if tl else []
    if not tl:
        log("warning: no timeline.json beside the clock: placement can't see the film's overlays")
    caps = settle(caps, cuts, duration, occ, shots)

    srt, vtt, ass = out / f"{name}.srt", out / f"{name}.vtt", out / f"{name}.ass"
    write_srt(caps, srt)
    write_vtt(caps, vtt)
    write_ass(caps, ass)

    # the rules, measured
    probs = []
    for k, c in enumerate(caps, 1):
        d = c["end"] - c["start"]
        if c["kind"] == "speech" and not (MIN_DUR - 1e-3 <= d <= MAX_DUR + 1e-3):
            probs.append(f"#{k} lasts {d:.2f} s")
        if c["kind"] == "tag" and d < MIN_TAG - 1e-3:
            probs.append(f"#{k} (tag) lasts {d:.2f} s")
        if len(c["text"]) > 2 or max(len(x) for x in c["text"]) > HARDL:
            probs.append(f"#{k} is {len(c['text'])} lines, longest {max(len(x) for x in c['text'])}")
        spans = [ct for ct in cuts if c["start"] + 0.05 < ct < c["end"] - 0.05]
        c["spans_cuts"] = len(spans)
        if c.get("placement_note"):
            probs.append(f"#{k} {c['placement_note']}")
    cut_ok = cut.exists()
    if cut_ok:
        probe = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
                                str(cut)], capture_output=True, text=True).stdout.strip()
        if probe and abs(float(probe) - duration) > 1.5 / FPS:
            log(f"WARNING: the cut is {float(probe):.3f} s, the clock {duration:.3f} s: not this clock's cut")
            probs.append(f"cut length {float(probe):.3f} s != clock {duration:.3f} s")
    summary = {
        "cut": str(cut), "cut_exists": cut_ok, "voice": str(voice_p), "clock": str(clock_p),
        "clock_written": clock["film"].get("written"), "duration": duration, "captions": len(caps),
        "speech_captions": sum(c["kind"] == "speech" for c in caps), "tags": sum(c["kind"] == "tag" for c in caps),
        "lanes": {w: sum(c["where"] == w for c in caps) for w in LANES},
        "lane_per_shot": {sid: c["where"] for c in caps for sid in c["shots"]},
        "spanning_a_cut": sum(1 for c in caps if c["spans_cuts"]), "problems": probs,
        "style": {"font": FONT_NAME, "px_at_1080": FONT_PX, "band_alpha": BAND_ALPHA, "max_chars": [MAXL, HARDL]},
        "captions_list": [{k: (round(c[k], 3) if k in ("start", "end") else c[k])
                           for k in ("line", "kind", "start", "end", "text", "where", "box", "shots", "spans_cuts",
                                     "hits", "because", "words") if k in c} for c in caps],
    }
    log(f"{len(caps)} captions ({summary['tags']} tags); lanes: {summary['lanes']}; spanning a cut: "
        f"{summary['spanning_a_cut']}; problems: {len(probs)}")
    for p in probs:
        log("  " + p)

    if cut_ok and not a.no_check:
        sc = scribe(cut)
        if sc:
            res = check(caps, sc, name, out / f"{name}-check.md")
            summary["check"] = {k: v for k, v in res.items()}
            log(f"check: {res['matched']}/{res['shown_words']} words agree ({100 * res['match_rate']:.1f}%), "
                f"{len(res['diffs'])} differences, {len(res['heard_outside_its_caption'])} heard outside their caption")
    elif not cut_ok:
        log(f"the cut {cut} does not exist yet: text files only (rerun this command when it lands)")
    (out / f"{name}.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False))
    if cut_ok and a.sheets:
        for p in sheets(cut, caps, ass, name, shots):
            log(f"sheet: {p}")
    if cut_ok and not a.no_burn:
        outs = burn(cut, ass, name, out, duration)
        summary["burned"] = {p.name: round(p.stat().st_size / 1e6, 2) for p in outs}
        (out / f"{name}.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False))
    elif cut_ok:   # a text-only rerun: say which burned files exist, and whether they are older than this .ass
        old = {p.name: {"mb": round(p.stat().st_size / 1e6, 2), "older_than_ass": p.stat().st_mtime < ass.stat().st_mtime}
               for p in (out / f"{name}-480p-subs.mp4", out / f"{name}-480p-subs-hevc.mp4") if p.exists()}
        if old:
            summary["burned"] = old
            (out / f"{name}.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False))
    log("done: " + ", ".join(str(p.relative_to(FILM) if p.is_relative_to(FILM) else p)
                             for p in (srt, vtt, ass)))


if __name__ == "__main__":
    main()
