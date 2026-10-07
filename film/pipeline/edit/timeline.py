#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""The edit decision list, derived: the shot list and settings in film/edit.toml + the narration's clips -> timeline.json.

    uv run film/pipeline/edit/timeline.py --dry       # print the clock: every shot and line, against the plan
    uv run film/pipeline/edit/timeline.py             # and write edit/timeline.json, clock.json, clock.txt,
                                                      # ladder-events.json (the config's paths.out, under FILM_ROOT)

The narration drives the clock.
- Clips are laid end to end with the script's pauses, as the voice's first draft laid them:
  speech-to-speech silence is the [pause] after a paragraph, and the voice's own sentence
  gap between clips split from one paragraph.
- A voiced shot (one with an "Over:" line) is cut in the silence before its first word: at
  most `lead_max` before it, or half the silence if that is shorter. Its words are found in
  the clips' Scribe word times, so a shot that starts mid-sentence ("raced through it, | and
  competed in it") is cut on its word.
- A voiceless shot (S01's corridor, the title, S46b's ogre, the montage, the end card) keeps
  its planned length. It starts when the voice before it has finished its pause (less an
  `overlap`, for an L-cut), and the voice resumes `voice_after_silent` after it.
- Every cut is rounded to a frame (1/60 s); every clip to a sample.

Sources are resolved now, from what exists on disk (see edit.toml), so timeline.json says
which shots are real, which are stand-ins and which are slates.

film/edit.toml carries the shot list itself ([[shotlist_sections]], [[shotlist]]) and every shot's settings,
resolved. The production kept them in versions: a shot list in Markdown tables per script version
(parse_shots, parse_shots_v2, parse_shots_v3) and configs that extend and inherit from one another
(load_config, v2_shot_settings, v3_shot_settings). That code stays, for reading those; the film's
config doesn't need it.
"""

from __future__ import annotations

import argparse
import copy
import difflib
import glob
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

EDIT = Path(__file__).resolve().parent
sys.path.insert(0, str(EDIT.parent))
from filmroot import FILM, PIPELINE  # noqa: E402

SHOTS_MD = FILM / "script" / "shots.md"        # the first script version's shot list and diagram list (unused by
DIAGRAMS_MD = FILM / "script" / "diagrams.md"  # film/edit.toml)
CLIPS_JSON = FILM / "voice" / "clips.json"
CONFIG = PIPELINE.parent / "edit.toml"

SECTION_RE = re.compile(r"^#{2,3} (\d+(?:\.\d+)?)\.? (.+?) — (\d+:\d+(?:\.\d+)?) to (\d+:\d+(?:\.\d+)?)")
SHOT_RE = re.compile(
    r"^\*\*((?:S\d+[ab]?)|(?:F\d+))\*\* · (\d+:\d+\.\d+)–(\d+:\d+\.\d+) · ([\d.]+) s(?: \(([^)]*)\))? · \*\*([^*]+)\*\*(.*)$"
)
DIAGRAM_RE = re.compile(r"^## (D\d\d) — (.+?) \(")


def secs(tc: str) -> float:
    m, s = tc.split(":")
    return int(m) * 60 + float(s)


def tc(t: float) -> str:
    sign = "-" if t < 0 else ""
    t = abs(t)
    return f"{sign}{int(t // 60)}:{t % 60:05.2f}"


def plain(s: str) -> str:
    """Markdown to plain text: no bold, no backticks, no italics markers."""
    s = s.replace("**", "").replace("`", "")
    s = re.sub(r"(?<!\w)\*([^*]+)\*(?!\w)", r"\1", s)
    return s.strip()


# --------------------------------------------------------------- parsing ----

def parse_shots(md: str) -> tuple[list[dict], list[dict]]:
    sections, shots = [], []
    cur = None
    for line in md.splitlines():
        m = SECTION_RE.match(line)
        if m:
            sid, title, a, b = m.groups()
            sections.append({"id": sid, "title": title.strip(), "planned_start": secs(a), "planned_end": secs(b)})
            cur = None
            continue
        if line.startswith("#"):
            cur = None
            continue
        m = SHOT_RE.match(line)
        if m:
            sid, a, b, length, bars, kind, extra = m.groups()
            cur = {
                "id": sid, "section": sections[-1]["id"], "kind": kind.strip(), "extra": plain(extra),
                "planned_start": secs(a), "planned_len": float(length), "bars": bars, "bullets": [],
            }
            shots.append(cur)
            continue
        if cur is None:
            continue
        if line.startswith("- "):
            cur["bullets"].append(line[2:].strip())
        elif line.startswith("  - ") and cur["bullets"]:
            sep = " " if cur["bullets"][-1].endswith(":") else "; "
            cur["bullets"][-1] += sep + line.strip()[2:]
    for sh in shots:
        sh["over"] = None
        sh["caption"] = None
        sh["caption_sub"] = None
        sh["notice"] = None
        for b in sh["bullets"]:
            if b.startswith("Over:"):
                q = re.findall(r'"([^"]+)"', b)
                sh["over"] = " ".join(q) if q else None
            elif b.startswith("Caption:"):
                m = re.match(r"Caption: \*\*(.+?)\*\*\.?(?: Second line: \"(.+?)\")?", b)
                if m:
                    sh["caption"], sh["caption_sub"] = m.group(1), m.group(2)
            elif b.startswith("Notice:") and sh["notice"] is None:
                sh["notice"] = plain(b[len("Notice:"):])
        firsts = [b for b in sh["bullets"] if not b.startswith(("Over:", "Caption:", "Notice:", "Source:", "Fallback"))]
        sh["desc"] = plain(firsts[0]) if firsts else ""
        m = re.search(r"\b(D\d\d)\b", sh["extra"])
        sh["diagram"] = m.group(1) if (m and sh["kind"] == "diagram") else None
        part = re.search(r"part ([ab])\b", sh["extra"])
        sh["diagram_part"] = part.group(1) if part else None
    return sections, shots


def parse_diagrams(md: str) -> dict[str, dict]:
    out, cur = {}, None
    for line in md.splitlines():
        m = DIAGRAM_RE.match(line)
        if m:
            cur = {"title": m.group(2).strip(), "shows": ""}
            out[m.group(1)] = cur
            continue
        if line.startswith("## "):
            cur = None
        elif cur is not None and line.startswith("What it shows:") and not cur["shows"]:
            cur["shows"] = plain(line[len("What it shows:"):])
    return out


# ------------------------------------------------------------------ words ----

def toks(s: str) -> list[str]:
    s = s.lower().replace("’", "'")
    s = re.sub(r"[-–—/]", " ", s)
    out = []
    for w in s.split():
        w = w.strip(".,:;!?\"()'*")
        if w:
            out.append(w)
    return out


def clip_tokens(clip: dict) -> list[dict]:
    """The clip's script tokens with times (s, into the clip file), from Scribe's words where they match."""
    script = toks(clip["text"])
    heard = []
    for w in clip["words"]:
        for t in toks(w["text"]):
            heard.append((t, w["start"], w["end"]))
    sm = difflib.SequenceMatcher(a=script, b=[h[0] for h in heard], autojunk=False)
    times: list[tuple[float, float] | None] = [None] * len(script)
    for blk in sm.get_matching_blocks():
        for k in range(blk.size):
            _, s, e = heard[blk.b + k]
            times[blk.a + k] = (s, e)
    # unmatched words (numbers Scribe writes as digits, respellings): spread between their neighbours
    lo_t = clip["speech_start_s"]
    hi_t = clip["speech_end_s"]
    i = 0
    while i < len(script):
        if times[i] is not None:
            i += 1
            continue
        j = i
        while j < len(script) and times[j] is None:
            j += 1
        a = times[i - 1][1] if i > 0 else lo_t
        b = times[j][0] if j < len(script) else hi_t
        n = j - i
        for k in range(n):
            times[i + k] = (a + (b - a) * k / n, a + (b - a) * (k + 1) / n)
        i = j
    return [{"w": w, "s": s, "e": e} for w, (s, e) in zip(script, times)]


# ------------------------------------------------------------- the clock ----

class Clock:
    def __init__(self, cfg: dict, clips: list[dict], natural_gap: float):
        self.fps = cfg["clock"]["fps"]
        self.head = cfg["clock"]["voice_head"]
        self.tail = cfg["clock"]["voice_tail"]
        self.clips = clips
        self.gap_natural = natural_gap
        self.at: list[float | None] = [None] * len(clips)  # each clip file's start, film time
        self.resume_at: float | None = 0.0                  # where the next clip's speech starts after a voiceless shot
        self.pauses = cfg.get("pauses", {})                # the edit's own pauses, by clip id (never inside a clip)

    def frame(self, t: float) -> int:
        return int(round(t * self.fps))

    def gap_after(self, k: int) -> float:
        c = self.clips[k]
        num = c["id"].split("-")[0] if c["id"][:1].isdigit() else None
        for key in (c["id"], num):
            if key and key in self.pauses:
                return float(self.pauses[key])
        return c["pause_after_s"] if c["paragraph_end"] else self.gap_natural

    def nominal_end(self, k: int) -> float:
        c = self.clips[k]
        if c.get("hard_end"):  # a line cut off mid-word: it ends on its last sound, with no tail
            return self.at[k] + c["speech_end_s"]
        return self.at[k] + c["duration_s"] - self.tail

    def place_until(self, k: int) -> None:
        for c in range(k + 1):
            if self.at[c] is not None:
                continue
            if self.resume_at is not None:
                start = self.resume_at
                self.resume_at = None
            else:
                start = self.nominal_end(c - 1) + self.gap_after(c - 1)
            self.at[c] = start - self.head

    def last_placed(self) -> int:
        k = -1
        for i, a in enumerate(self.at):
            if a is not None:
                k = i
        return k


TERMINAL_CMD = "] uv run oracle/classic_check.py"
TERMINAL_CHECKS = ["goldens", "play", "timedemo", "census", "edicts", "oracle", "exact", "screen2d", "demolerp", "sound"]
TERMINAL_AGAINST_C = {"edicts", "oracle", "exact", "screen2d", "demolerp", "sound"}   # v6: the six run against id's C


def terminal_times(length: float, allpass: float | None) -> tuple[float, float, list[float], float]:
    """S27's card: typing starts, typing ends, the nine PASS lines, ALL PASS (on its words)."""
    t_type = 0.15
    allpass = allpass if allpass is not None else length - 1.2
    t_typed = t_type + len(TERMINAL_CMD) / 40
    first = t_typed + 0.25
    step = min(0.25, max(0.12, (allpass - 0.35 - first) / (len(TERMINAL_CHECKS) - 1)))
    return t_type, t_typed, [first + i * step for i in range(len(TERMINAL_CHECKS))], allpass


def word_ref(words: list[dict], ref: str) -> float | None:
    """'word' or 'word#N': the start (relative) of the Nth occurrence of the word in a shot's words.
    'word$' is its end; a trailing '+0.2' or '-0.1' moves it."""
    m = re.match(r"^(.*?)([+-]\d+(?:\.\d+)?)?$", ref)
    ref, off = m.group(1), float(m.group(2) or 0.0)
    end = ref.endswith("$")
    ref = ref.rstrip("$")
    w, _, n = ref.partition("#")
    n = int(n) if n else 1
    seen = 0
    for x in words:
        if x["w"] == w.lower():
            seen += 1
            if seen == n:
                return (x["e"] if end and "e" in x else x["t"]) + off
    return None


def probe(path: Path) -> dict:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration:stream=codec_type,width,height,r_frame_rate",
         "-of", "json", str(path)], capture_output=True, text=True)
    try:
        j = json.loads(out.stdout)
    except json.JSONDecodeError:
        return {}
    v = next((s for s in j.get("streams", []) if s.get("codec_type") == "video"), {})
    num, _, den = v.get("r_frame_rate", "0/1").partition("/")
    return {
        "duration": float(j.get("format", {}).get("duration", 0) or 0),
        "width": v.get("width"), "height": v.get("height"),
        "fps": float(num) / float(den or 1) if den and float(den) else 0.0,
        "audio": any(s.get("codec_type") == "audio" for s in j.get("streams", [])),
    }


def resolve_source(sh: dict, cfg: dict, over: dict) -> dict:
    """The shot's picture: the first that exists of explicit source, diagram, footage, card, stand-in, slate."""
    def rel(p: Path) -> str:
        return str(p.resolve().relative_to(FILM))

    def diagram_file(d: str) -> Path | None:
        pat = str(FILM / cfg["footage"]["diagram_glob"].format(d=d))
        hits = [Path(p) for p in sorted(glob.glob(pat)) if not re.search(r"_alpha|_\d", Path(p).stem)]
        return hits[0] if hits else None

    src = over.get("source")
    if src:
        p = diagram_file(src.split(":", 1)[1]) if src.startswith("diagram:") else FILM / src
        if p and p.exists():
            out = {"type": "video", "path": rel(p), "in": float(over.get("in", 0.0)), "status": "final"}
            if over.get("sound") and (FILM / over["sound"]).exists():
                out["sound"] = over["sound"]
            return out
    if sh["diagram"]:
        p = diagram_file(sh["diagram"])
        if p:
            parts = cfg.get("diagram_parts", {}).get(sh["diagram"], [])
            a, b = 0.0, None
            if parts == "continuous":  # one render across its shots, on the edit's clock
                return {"type": "video", "path": rel(p), "in": 0.0, "continuous": True, "status": "diagram"}
            if sh["diagram_part"] and parts:
                idx = ord(sh["diagram_part"]) - ord("a")
                a = 0.0 if idx == 0 else parts[idx - 1]
                b = parts[idx] if idx < len(parts) else None
            return {"type": "video", "path": rel(p), "in": float(over.get("in", a)), "out": b, "status": "diagram"}
    for g in cfg["footage"]["globs"]:
        hits = sorted(glob.glob(str(FILM / g.format(id=sh["id"])), recursive=True))
        hits = [h for h in hits if not h.endswith(("_contact.png",))]
        hits = [h for h in hits if probe(Path(h)).get("width")]  # a file still being written has no video yet
        if hits:
            p = Path(hits[0])
            head = 0.0
            for d, h in cfg["footage"].get("head_by_dir", {}).items():  # a folder's convention for handles
                if str(p.resolve()).startswith(str((FILM / d).resolve()) + "/"):
                    head = float(h)
            side = p.with_suffix(".json")
            meta = {}
            if side.exists():
                try:
                    meta = json.loads(side.read_text())
                    head = float(meta.get("head_s", head))
                except (ValueError, json.JSONDecodeError):
                    meta = {}
            out = {"type": "video", "path": rel(p), "in": float(over.get("in", head)), "status": "footage"}
            if meta.get("shot_s"):
                out["shot_s"] = float(meta["shot_s"])
            snd = meta.get("sound")
            if snd and (p.parent / snd).exists() and not over.get("mute"):
                out["sound"] = rel(p.parent / snd)  # aligned to the mp4's first frame, handles included
            return out
    if over.get("card"):
        return {"type": "card", "name": over["card"], "status": "card"}
    if over.get("standin") and (FILM / over["standin"]).exists():
        return {"type": "video", "path": over["standin"], "in": float(over.get("in", 0.0)), "status": "standin"}
    return {"type": "slate", "status": "placeholder"}


def build(cfg: dict) -> dict:
    fmt = cfg.get("paths", {}).get("format")
    v2 = fmt in ("v2", "v3")
    diagrams = parse_diagrams(DIAGRAMS_MD.read_text()) if DIAGRAMS_MD.exists() else {}
    if cfg.get("shotlist"):  # film/edit.toml: the shot list and every shot's settings, resolved, in one file
        sections, shots = flat_shots(cfg)
        clips, gap = clips_v3(cfg)
        spoken = {}
        shot_cfg = cfg.get("shots", {})
    elif fmt == "v3":
        sections, shots = parse_shots_v3((FILM / cfg["paths"]["shots"]).read_text(), cfg)
        clips, gap = clips_v3(cfg)
        spoken = {}
        shot_cfg = v3_shot_settings(cfg, shots)
    elif v2:
        sections, shots = parse_shots_v2((FILM / cfg["paths"]["shots"]).read_text())
        clips, gap = clips_v2(cfg)
        spoken: dict = {}
        shot_cfg = v2_shot_settings(cfg, shots)
    else:
        sections, shots = parse_shots(SHOTS_MD.read_text())
        cj = json.loads(CLIPS_JSON.read_text())
        clips, gap = cj["clips"], cj["natural_sentence_gap_s"]
        spoken = cj.get("sections", {})
        shot_cfg = cfg.get("shots", {})
    fps = cfg["clock"]["fps"]
    shots = [sh for sh in shots if not shot_cfg.get(sh["id"], {}).get("drop")]  # a shot the edit leaves out
    mv = cfg.get("paths", {}).get("move")
    if mv:  # an experiment's order: a whole section, its shots and its lines, moved after a shot
        sec_ids = {sh["id"] for sh in shots if sh["section"] == mv["section"]}
        moved = [sh for sh in shots if sh["id"] in sec_ids]
        rest = [sh for sh in shots if sh["id"] not in sec_ids]
        k = next(i for i, sh in enumerate(rest) if sh["id"] == mv["after"]) + 1
        shots = rest[:k] + moved + rest[k:]
        after_sec = next(sh["section"] for sh in rest if sh["id"] == mv["after"])
        mc = [c for c in clips if c["section"] == mv["section"]]
        rc = [c for c in clips if c["section"] != mv["section"]]
        last = max(i for i, c in enumerate(rc) if c["section"] == after_sec)
        clips = rc[:last + 1] + mc + rc[last + 1:]
        ms = [s for s in sections if s["id"] == mv["section"]]
        rs = [s for s in sections if s["id"] != mv["section"]]
        j = next(i for i, s in enumerate(rs) if s["id"] == after_sec) + 1
        sections = rs[:j] + ms + rs[j:]
    clock = Clock(cfg, clips, gap)
    for sh in shots:  # a voiceless shot's length: the plan's, or edit.toml's `len`
        sh["edit_len"] = float(shot_cfg.get(sh["id"], {}).get("len", sh["planned_len"]))
    for sh in shots:  # an Over line the shot list lacks, given in edit.toml
        if shot_cfg.get(sh["id"], {}).get("over"):
            sh["over"] = shot_cfg[sh["id"]]["over"]
            sh["over_from_edit"] = not v2
        if shot_cfg.get(sh["id"], {}).get("voiceless"):
            sh["over"] = None

    # the narration as one token stream: (clip index, token)
    stream = []
    for k, c in enumerate(clips):
        for t in clip_tokens(c):
            stream.append({"clip": k, **t})

    # pass 1: each voiced shot's first token, in order
    cursor = 0
    for sh in shots:
        sh["tok0"] = None
        if not sh["over"]:
            continue
        want = toks(sh["over"])[:3]
        found = None
        for j in range(cursor, len(stream) - len(want) + 1):
            if [stream[j + q]["w"] for q in range(len(want))] == want:
                found = j
                break
        if found is None:
            raise SystemExit(f"{sh['id']}: its Over line {sh['over']!r} is not in the narration after token {cursor}")
        sh["tok0"] = found
        cursor = found + 1
    voiced = [sh for sh in shots if sh["tok0"] is not None]
    for a, b in zip(voiced, voiced[1:] + [None]):
        a["tok1"] = b["tok0"] if b else len(stream)  # one past the shot's last token

    def tok_time(j: int, key: str = "s") -> float:
        k = stream[j]["clip"]
        clock.place_until(k)
        return clock.at[k] + stream[j][key]

    # pass 2: the cuts
    prev_voiced = None   # was the shot before voiced?
    t_prev_end = 0.0
    for i, sh in enumerate(shots):
        oc = shot_cfg.get(sh["id"], {})
        if sh["tok0"] is not None:
            if i == 0 or not prev_voiced:
                start = t_prev_end
                if clock.resume_at is None and clock.last_placed() < stream[sh["tok0"]]["clip"]:
                    clock.resume_at = start + cfg["clock"]["voice_after_silent"]
            else:
                ts = tok_time(sh["tok0"], "s")
                te = tok_time(sh["tok0"] - 1, "e")
                if "lead" in oc:  # a bumper or a picture beat before the words: as much of the silence as allowed
                    lead = min(float(oc["lead"]), max(0.0, ts - te - 0.1))
                else:
                    lead = min(cfg["clock"]["lead_max"], max(0.0, ts - te) / 2)
                start = ts - lead + float(oc.get("lag", 0.0))  # `lag`: the shot before holds on under these words
            prev_voiced = True
        else:
            if prev_voiced:
                # the voice before it finishes its clip and its pause (all clips before the next voiced shot)
                nxt = next((s for s in shots[i + 1:] if s["tok0"] is not None), None)
                last_clip = (stream[nxt["tok0"]]["clip"] - 1) if nxt else len(clips) - 1
                clock.place_until(last_clip)
                start = clock.nominal_end(last_clip) + clock.gap_after(last_clip) - float(oc.get("overlap", 0.0))
                if oc.get("cut_on"):  # the cut on a word of the shot before ("on$": the end of its last "on")
                    pv = shots[i - 1]
                    ref = str(oc["cut_on"])
                    mo = re.match(r"^(.*?)([+-]\d+(?:\.\d+)?)?$", ref)
                    w_, off_ = mo.group(1), float(mo.group(2) or 0.0)
                    hits = [j for j in range(pv["tok0"], pv.get("tok1", len(stream))) if stream[j]["w"] == w_.rstrip("$").lower()]
                    if hits:
                        start = tok_time(hits[-1], "e" if w_.endswith("$") else "s") + off_
            else:
                start = t_prev_end
            prev_voiced = False
        sh["frame"] = clock.frame(start)
        if i > 0:
            shots[i - 1]["frames"] = sh["frame"] - shots[i - 1]["frame"]
        if sh["tok0"] is None:
            t_prev_end = (sh["frame"] + round(sh["edit_len"] * fps)) / fps
            nxt_voiced = i + 1 < len(shots) and shots[i + 1]["tok0"] is not None
            if nxt_voiced:
                after = shot_cfg.get(shots[i + 1]["id"], {}).get("voice_after", cfg["clock"]["voice_after_silent"])
                clock.resume_at = t_prev_end + float(after)
        else:
            t_prev_end = sh["frame"] / fps
    last = shots[-1]
    last["frames"] = round(last["edit_len"] * fps) if last["tok0"] is None else None
    if last["frames"] is None:  # a voiced last shot: its words, its pause
        k = len(clips) - 1
        clock.place_until(k)
        last["frames"] = clock.frame(clock.nominal_end(k) + clock.gap_after(k)) - last["frame"]
    clock.place_until(len(clips) - 1)
    total_frames = last["frame"] + last["frames"]

    # words per shot, relative to the shot's start
    for sh in shots:
        sh["start"] = sh["frame"] / fps
        sh["len"] = sh["frames"] / fps
        sh["words"] = []
        if sh["tok0"] is not None:
            for j in range(sh["tok0"], sh["tok1"]):
                sh["words"].append({"w": stream[j]["w"], "t": round(tok_time(j, "s") - sh["start"], 3),
                                    "e": round(tok_time(j, "e") - sh["start"], 3)})

    # sections
    sec_out = []
    for s in sections:
        mine = [sh for sh in shots if sh["section"] == s["id"]]
        if not mine:
            continue
        a = mine[0]["frame"] / fps
        b = (mine[-1]["frame"] + mine[-1]["frames"]) / fps
        planned = s["planned_end"] - s["planned_start"]
        sec_out.append({
            "id": s["id"], "title": s["title"], "planned_start": s["planned_start"], "planned_len": round(planned, 3),
            "start": round(a, 4), "len": round(b - a, 4), "delta": round(b - a - planned, 3),
            "spoken": spoken.get(s["id"]), "voiced": any(sh["tok0"] is not None for sh in mine),
        })

    # the montage: on the beat?
    beat_issues = []
    mont = [sh for sh in shots if sh["section"] == cfg["clock"].get("beat_section")]
    if mont:
        f0 = mont[0]["frame"]
        spb = round(fps * 0.5)
        for sh in mont:
            if (sh["frame"] - f0) % spb:
                beat_issues.append(sh["id"])

    # the shots' sources, overlays, cues
    out_shots = []
    run_len: dict[str, float] = {}  # a continuous diagram's slots so far
    render_first: dict[str, float] = {}  # a render's (or chain's) first use: film time of its shot time 0
    for sh in shots:
        oc = shot_cfg.get(sh["id"], {})
        src = resolve_source_v2(sh, cfg, oc, shots) if v2 else resolve_source(sh, cfg, oc)
        chain = oc.get("chain") or src.get("render")
        if src["type"] == "video" and chain:  # one render across several shots: the camera's own clock
            if chain in render_first:
                src["in"] = round(src.get("head", 0.0) + sh["frame"] / fps - render_first[chain], 5)
            else:
                render_first[chain] = sh["frame"] / fps - (src["in"] - src.get("head", 0.0))
        if src.get("continuous"):
            src["in"] = round(run_len.get(sh["diagram"], 0.0), 5)
            run_len[sh["diagram"]] = run_len.get(sh["diagram"], 0.0) + sh["frames"] / fps
        if src["type"] == "video":
            info = probe(FILM / src["path"])
            src["probe"] = info
            sync = oc.get("sync")
            if sync and src["status"] in ("footage", "final"):
                wt = word_ref(sh["words"], sync["word"])
                if wt is not None:
                    want = src["in"] + float(sync["at"]) - wt
                    src["sync"] = {"word": sync["word"], "word_t": wt, "at": sync["at"]}
                    if want < 0 and sync.get("stretch"):  # short: play it from its first frame, a little slower
                        src["speed"] = round((src["in"] + float(sync["at"])) / wt, 4)
                        src["sync"]["stretched"] = src["speed"]
                        want = 0.0
                    elif want < 0:  # the source needs a head handle this long for its event to land on the word
                        src["sync"]["short_by"] = round(-want, 3)
                        print(f"warning: {sh['id']}: sync on {sync['word']!r} needs {-want:.2f} s more head handle",
                              file=sys.stderr)
                    src["in"] = round(max(0.0, want), 4)
            if info.get("width") == 960 and info.get("height") == 600 and oc.get("box", True):
                src["box"] = True  # Classic's 960x600, shown 4:3 in a 1440x1080 box
            avail = (src.get("out") or info.get("duration", 0)) - src["in"]
            if src.get("fit") and avail < sh["len"]:  # short: borrow from the head handle, then slow it a little
                take = 0.0 if src["fit"] == "slow" else min(src["in"], sh["len"] - avail)
                src["in"] = round(src["in"] - take, 4)
                avail += take
                if avail < sh["len"]:
                    src["speed"] = round(max(float(oc.get("fit_min", 0.75)), avail / sh["len"]), 4)
                    avail = avail / src["speed"]
            elif src.get("speed"):
                avail = avail / float(src["speed"])
            src["hold"] = round(max(0.0, sh["len"] - avail), 3)  # held on its last frame for this long
            src["cut"] = round(max(0.0, avail - sh["len"]), 3)    # this much of the source is left unused
        cues = {}
        for name, ref in oc.get("cues", {}).items():
            v = word_ref(sh["words"], ref)
            if v is None:
                print(f"warning: {sh['id']}: cue {name}={ref!r} not among its words", file=sys.stderr)
            cues[name] = v
        def when(v):  # seconds into the shot, or a word reference
            if isinstance(v, (int, float)):
                return float(v)
            w = word_ref(sh["words"], v)
            if w is None:
                print(f"warning: {sh['id']}: {v!r} not among its words", file=sys.stderr)
            return w
        ovl = []  # the switches first: opaque pictures, under every drawn overlay
        for sw in oc.get("switches", []):  # the same camera and clock in another file, from `at` to `until`
            a_, b_ = when(sw["at"]), when(sw.get("until", 1e9))
            if a_ is None or b_ is None or not (FILM / sw["path"]).exists():
                continue
            o = {"type": "video", "path": sw["path"], "in": src.get("in", 0.0), "enable": [round(a_, 3), round(b_, 3)]}
            if "in" in sw:  # another camera or clock: its file time `in` at the switch (a cut inside the shot)
                o["in"], o["at"] = float(sw["in"]), round(a_, 4)
            wav = (FILM / sw["path"]).with_suffix(".wav")
            if wav.exists():
                o["sound"] = str(wav.relative_to(FILM))
            ovl.append(o)
        ovl += [dict(o) for o in oc.get("overlays", [])]
        all_mutes = []
        if oc.get("mute"):
            all_mutes.append([0.0, round(sh["frames"] / fps, 4)])
        for m_ in oc.get("mute_windows", []):  # every bus but the voice: [from, to], numbers or word references
            a_, b_ = when(m_[0]), when(m_[1])
            if a_ is not None and b_ is not None:
                all_mutes.append([round(a_, 3), round(b_, 3)])
        if oc.get("burst"):  # the box bursts: (a) the Classic box, held, then scaled out to (b) the full frame
            bz = oc["burst"]
            fa, fb = FILM / bz["a"], FILM / bz["b"]
            src["at"] = round(when(bz["at"]) or 0.0, 4)
            if fa.exists() and fb.exists():
                src = {"type": "burst", "status": "footage", "a": bz["a"], "b": bz["b"],
                       "in": float(bz.get("in", 1.0)), "at": round(when(bz["at"]) or 0.0, 4), "dur": float(bz.get("dur", 0.5)),
                       "path": bz["b"], "probe": probe(fb), "a_probe": probe(fa)}
                wav = fb.with_suffix(".wav")
                if wav.exists():
                    src["sound"] = str(wav.relative_to(FILM))
        if oc.get("sound_compose") and src.get("type") in ("video", "burst"):  # a shot's sound built from pieces
            pieces = []
            for pc in oc["sound_compose"]:
                at_ = when(pc["at"])
                if at_ is None:
                    continue
                pieces.append({"wav": pc["wav"], "from": float(pc.get("from", 0.0)), "to": pc.get("to"),
                               "at": round(at_, 4), "until": when(pc["until"]) if "until" in pc else None,
                               "repeat_from": pc.get("repeat_from"), "db": float(pc.get("db", 0.0)),
                               "fade_in": float(pc.get("fade_in", 0.0)), "fade_out": float(pc.get("fade_out", 0.0))})
            src["compose"] = pieces
        mutes = []
        for m_ in oc.get("game_mute", []):  # [from, to] or [from, seconds]; each a number or a word reference
            a_ = when(m_[0])
            b_ = when(m_[1]) if isinstance(m_[1], str) else (None if a_ is None else a_ + float(m_[1]))
            if a_ is not None and b_ is not None:
                mutes.append([round(a_, 3), round(b_, 3)])
        overlays = ovl
        for o in overlays:
            for key in ("at", "reset_at"):
                if isinstance(o.get(key), str):
                    o[key] = when(o[key])
            if o.get("offset") == "source":  # the source's own shot time = t + offset
                o["offset"] = round(src.get("in", 0.0) - src.get("head", 0.0), 4)
        for o in overlays:  # an overlay that comes in on a word
            if o.get("cue"):
                v = word_ref(sh["words"], o["cue"])
                if v is None:
                    print(f"warning: {sh['id']}: overlay cue {o['cue']!r} not among its words", file=sys.stderr)
                o["at"] = round(max(0.0, (v if v is not None else 0.0) - float(o.get("lead", 0.1))), 3)
        if sh["caption"]:
            overlays.append({"type": "caption", "text": sh["caption"], "sub": sh["caption_sub"]})
        for cap in [c_ for c_ in sh.get("captions", []) if c_["text"] not in oc.get("drop_captions", [])]:
            at = 0.0 if cap["at_planned"] is None else max(0.0, cap["at_planned"] - sh["planned_start"])
            o = {"type": cap["kind"], "text": cap["text"], "at": round(at, 3)}
            o.update(oc.get("caption_opts", {}).get(cap["text"], {}))
            if o.get("cue"):
                w = word_ref(sh["words"], o["cue"])
                if w is not None:
                    o["at"] = round(max(0.0, w - 0.05), 3)
            overlays.append(o)
        pending = []
        kept = []
        for o in overlays:  # an overlay whose file has not landed yet is left out, and listed
            if o.get("type") == "video" and o.get("glob") and not o.get("path"):
                hits = []
                for g in (o["glob"] if isinstance(o["glob"], list) else [o["glob"]]):
                    hits = sorted(glob.glob(str(FILM / g)))
                    if hits:
                        break
                if hits:
                    o["path"] = str(Path(hits[0]).relative_to(FILM))
                else:
                    pending.append(o["glob"] if isinstance(o["glob"], str) else o["glob"][0])
                    continue
            if o.get("chain_from"):  # one overlay render across shots: this shot takes it from where it is now
                first = next((x for x in shots if x["id"] == o["chain_from"]), None)
                if first is not None:
                    o["in"] = round(sh["frame"] / fps - first["frame"] / fps, 4)
            kept.append(o)
        overlays = kept
        desc = sh["desc"]
        notice = sh["notice"] or ""
        if sh["diagram"]:
            d = diagrams.get(sh["diagram"], {})
            desc = f"{sh['diagram']}: {d.get('title', '')}" + (f", part {sh['diagram_part']}" if sh["diagram_part"] else "")
            notice = d.get("shows", "") or notice
        out_shots.append({
            "id": sh["id"], "section": sh["section"], "kind": sh["kind"], "extra": sh["extra"],
            "desc": desc, "notice": notice, "over": sh["over"],
            "planned_start": sh["planned_start"], "planned_len": sh["planned_len"],
            "start": round(sh["start"], 4), "frame": sh["frame"], "frames": sh["frames"], "len": round(sh["len"], 4),
            "words": sh["words"], "source": src, "cues": cues, "overlays": overlays,
            "fade_in": float(oc.get("fade_in", 0.0)), "fade_out": float(oc.get("fade_out", 0.0)),
            "transition_in": oc.get("transition_in"), "dim": float(oc.get("dim", 1.0)),
            "game_lufs": float(oc.get("game_lufs", cfg["levels"]["game_lufs"])),
            "diagram": sh["diagram"], "diagram_part": sh["diagram_part"],
            "over_from_edit": sh.get("over_from_edit", False), "pending_overlays": pending,
            "mute": bool(oc.get("mute", False)), "mutes": all_mutes, "game_mute": mutes,
            "card_opts": oc.get("card_opts", {}), "mute_game": bool(oc.get("mute_game", False)),
            "game_unducked": bool(oc.get("game_unducked", False)),
            "music_unducked": bool(oc.get("music_unducked", False)),
            "game_dips": [[when(g[0]), when(g[1]), float(g[2])] + ([float(g[3])] if len(g) > 3 else [])
                          for g in oc.get("game_dips", []) if when(g[0]) is not None and when(g[1]) is not None],
            "music_dips": [[when(a_), when(b_), float(d_)] for a_, b_, d_ in oc.get("music_dips", [])
                           if when(a_) is not None and when(b_) is not None],
            "music_db": float(oc.get("music_db", 0.0)),
            "sfx_dips": [[when(a_), when(b_), float(d_)] for a_, b_, d_ in oc.get("sfx_dips", [])
                         if when(a_) is not None and when(b_) is not None],
            "voice_gains": [[when(a_), when(b_), float(d_)] for a_, b_, d_ in oc.get("voice_gains", [])
                            if when(a_) is not None and when(b_) is not None],
            "game_fade_out": float(oc.get("game_fade_out", 0.01)),
            "picture": sh.get("picture"), "sound_text": sh.get("sound_text"),
            # narration under the shot that its Over line does not name (it was cut before the next shot's words)
            "unlisted_words": "" if v2 else (" ".join(w["w"] for w in sh["words"][len(toks(sh["over"] or "")):])
                                           if len(sh["words"]) > len(toks(sh["over"] or "")) + 1 else ""),
        })

    voice = []
    for k, c in enumerate(clips):
        f = c.get("file")
        voice.append({
            "id": c["id"], "section": c["section"], "file": (f if v2 else "voice/" + f) if f else None,
            "at": round(clock.at[k], 5), "sample": int(round(clock.at[k] * cfg["clock"]["sample_rate"])),
            "speech": [round(clock.at[k] + c["speech_start_s"], 4), round(clock.at[k] + c["speech_end_s"], 4)],
            "duration": c["duration_s"], "text": c["text"], "placeholder": bool(c.get("placeholder")),
            "hard_end": bool(c.get("hard_end")), "file_in": c.get("file_in", 0.0), "file_out": c.get("file_out"),
            "gain_db": float(cfg.get("voice", {}).get("gain", {}).get(c["id"], 0.0)),
        })
    if v2:
        add_placeholder_subtitles(out_shots, voice, fps)
    if fmt == "v3" and cfg.get("ladder"):
        ladder_plan(out_shots, cfg, fps)

    return {
        "fps": fps, "sample_rate": cfg["clock"]["sample_rate"], "frames": total_frames,
        "duration": round(total_frames / fps, 4), "planned_duration": shots[-1]["planned_start"] + shots[-1]["planned_len"],
        "sections": sec_out, "shots": out_shots, "voice": voice, "montage_off_beat": beat_issues,
        "music": music_plan(cfg, sec_out, total_frames / fps),
        "levels": cfg["levels"],
    }


def music_plan(cfg: dict, sections: list[dict], duration: float) -> dict:
    mc = cfg["music"]
    for sc in mc.get("scores", []):  # the first score that exists and is complete (its sidecar's length matches)
        score = FILM / sc["path"]
        side = score.with_suffix(".json")
        if not (score.exists() and side.exists()):
            continue
        meta = json.loads(side.read_text())
        if meta.get("samples") and score.stat().st_size < meta["samples"] * 6:  # 24-bit stereo, still being written
            continue
        return {"mode": "score", "path": sc["path"], "meta_seconds": meta.get("seconds"),
                "clock": sc.get("clock", "planned"), "xfade_conform": mc.get("conform_xfade", 0.25)}
    if not mc.get("standin"):
        return {"mode": "none"}
    regions = []
    smap = mc.get("standin_map", {})
    for s in sections:
        m = smap.get(s["id"])
        if not m:
            continue
        regions.append({"section": s["id"], "start": s["start"], "end": round(s["start"] + s["len"], 4),
                        "in": m["in"], "out": m["out"], "exact": bool(m.get("exact", False)),
                        "align": m.get("align", "start")})
    return {"mode": "standin", "path": mc["standin"], "regions": regions, "xfade": mc.get("region_xfade", 1.0)}


def _last_part(tl: dict, sh: dict) -> str:
    """The id of the last shot that shows the same continuous diagram."""
    same = [x["id"] for x in tl["shots"] if x.get("diagram") and x["diagram"] == sh.get("diagram")]
    return same[-1] if same else sh["id"]


def report(tl: dict) -> str:
    rows = [f"film {tc(tl['duration'])} ({tl['duration']:.2f} s, {tl['frames']} frames) against shots.md's "
            f"{tc(tl['planned_duration'])}: {tl['duration'] - tl['planned_duration']:+.2f} s", "",
            "section                     planned   spoken    edit      delta   start (planned -> edit)"]
    for s in tl["sections"]:
        flag = "  <-- off by more than 0.5 s" if abs(s["delta"]) > 0.5 else ""
        sp = f"{s['spoken']:6.2f}" if s["spoken"] is not None else "   -  "
        rows.append(f"{s['id']:>4} {s['title'][:22]:22s} {s['planned_len']:6.2f}   {sp}   {s['len']:6.2f}   {s['delta']:+6.2f}"
                    f"   {tc(s['planned_start'])} -> {tc(s['start'])}{flag}")
    rows.append("")
    counts: dict[str, list[str]] = {}
    for sh in tl["shots"]:
        counts.setdefault(sh["source"]["status"], []).append(sh["id"])
    for k in ("footage", "final", "diagram", "card", "standin", "placeholder"):
        if k in counts:
            rows.append(f"{k:12s} {len(counts[k]):3d}: {' '.join(counts[k])}")
    if tl["montage_off_beat"]:
        rows.append(f"montage cuts off the beat: {tl['montage_off_beat']}")
    for sh in tl["shots"]:
        if sh["over_from_edit"]:
            rows.append(f"{sh['id']}: Over line supplied by edit.toml (shots.md has none): {sh['over']!r}")
        if sh["unlisted_words"]:
            rows.append(f"{sh['id']}: also under it, not in its Over line: {sh['unlisted_words']!r}")
    for sh in tl["shots"]:
        s = sh["source"]
        if s.get("shot_s") and abs(sh["len"] - s["shot_s"]) > 0.3:
            d = sh["len"] - s["shot_s"]
            what = (f"{-d:.2f} s of its action unseen" if d < 0 else f"runs {d:.2f} s into its tail handle"
                    + (f", then holds {s['hold']:.2f} s" if s["hold"] > 0.05 else ""))
            rows.append(f"{sh['id']}: a {sh['len']:.2f} s slot for {s['shot_s']:.2f} s of footage: {what}")
            continue
        if s.get("shot_s") or (s.get("continuous") and s["cut"] > 0 and sh["id"] != _last_part(tl, sh)):
            continue
        if s["type"] == "video" and (s["hold"] > 0.05 or s["cut"] > 0.05) and s["status"] in ("diagram", "final", "footage"):
            what = f"held {s['hold']:.2f} s on its last frame" if s["hold"] > 0.05 else f"{s['cut']:.2f} s of it unused"
            rows.append(f"{sh['id']}: {s['path']} from {s['in']:.2f} s into a {sh['len']:.2f} s slot: {what}")
    short = [f"{sh['id']} {sh['len']:.2f}s" for sh in tl["shots"] if sh["len"] < 0.9]
    if short:
        rows.append(f"very short shots: {', '.join(short)}")
    return "\n".join(rows)


def shot_table(tl: dict) -> str:
    """Every shot, planned against the edit: for the score's hits and the diagrams' cues."""
    rows = ["shot  sect  planned start/len   edit start/len     frame   source", ]
    for sh in tl["shots"]:
        s = sh["source"]
        src = s.get("path") or s.get("name") or "slate"
        words = f"  words {sh['words'][0]['t']:.2f}-{sh['words'][-1]['e']:.2f}" if sh["words"] else ""
        rows.append(f"{sh['id']:5s} {sh['section']:>4}  {tc(sh['planned_start']):>8} {sh['planned_len']:5.2f}    "
                    f"{tc(sh['start']):>8} {sh['len']:6.3f}   {sh['frame']:6d}   {s['status']}: {src}{words}")
    return "\n".join(rows)


def _merge(base: dict, over: dict) -> dict:
    """A config over the one it extends: tables merge, values replace, and "__delete__" removes a key."""
    out = dict(base)
    for k, v in over.items():
        if v == "__delete__":
            out.pop(k, None)
            continue
        out[k] = _merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def load_config(path: str | Path | None = None) -> dict:
    """An edit.toml; one with `extends = "edit/v4/edit.toml"` is that config with its own tables merged over.
    A config that carries its shot list (film/edit.toml) names its script beside itself, not under FILM_ROOT."""
    path = Path(path or CONFIG).resolve()
    cfg = tomllib.loads(path.read_text())
    if cfg.get("extends"):
        cfg = _merge(load_config(FILM / cfg.pop("extends")), cfg)
    if cfg.get("shotlist") and cfg.get("paths", {}).get("script"):
        cfg["paths"]["script"] = str(path.parent / cfg["paths"]["script"])
    return cfg


SHOT_DEFAULTS = {"over": None, "caption": None, "caption_sub": None, "diagram": None, "diagram_part": None,
                 "bars": None, "extra": "", "desc": "", "notice": "", "captions": [], "reuse_in": None,
                 "reuse_path": None, "reuse_id": None, "renders": []}


def flat_shots(cfg: dict) -> tuple[list[dict], list[dict]]:
    """film/edit.toml's [[shotlist_sections]] and [[shotlist]], as the shot-list parsers give them (TOML has no
    null: a key left out is its default). ([sections] is another table: a section's own score level.)"""
    sections = [dict(s) for s in cfg["shotlist_sections"]]
    shots = []
    for e in cfg["shotlist"]:
        sh = copy.deepcopy(SHOT_DEFAULTS) | copy.deepcopy(e)
        for c in sh["captions"]:
            c.setdefault("at_planned", None)
        shots.append(sh)
    return sections, shots


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dry", action="store_true")
    ap.add_argument("--config", default=str(CONFIG), help="the edit's config (default: film/edit.toml)")
    ap.add_argument("--out", help="the timeline.json to write (default: the config's out dir, or edit/)")
    a = ap.parse_args()
    cfg = load_config(a.config)
    tl = build(cfg)
    print(report(tl))
    if not a.dry:
        out_dir = FILM / cfg.get("paths", {}).get("out", "edit")
        out = Path(a.out) if a.out else out_dir / "timeline.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out_dir.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(tl, indent=1, ensure_ascii=False))
        print(f"\n-> {out}")
        if cfg.get("paths", {}).get("format") in ("v2", "v3"):
            write_clock(tl, cfg, out_dir)
            write_ladder_events(tl, cfg, out_dir)



# ================================================================ version 2 ====
# Version 2's shot list is tables (script/v2/shots.md), its narration a mix of v1's recorded clips and
# new lines (script/v2/script.md), recorded or not yet. A v2 config (edit/v2/edit.toml) names them.

V2_SECTION_RE = re.compile(r"^## (\d+(?:\.\d+)?)\.? (.+?) — (\d+:\d+(?:\.\d+)?)–(\d+:\d+(?:\.\d+)?)")
V2_ROW_RE = re.compile(r"^\| \**([SGFN]\w*|bump)\** \| (\d+:\d+(?:\.\d+)?)–(\d+:\d+(?:\.\d+)?) \| (.*)\|\s*$")
V2_LINE_RE = re.compile(r"^\*\*(V2-\w+)\*\* · (?:NEW|CHANGED)[^·]*· (.+)$")


def parse_shots_v2(md: str) -> tuple[list[dict], list[dict]]:
    sections, shots = [], []
    for line in md.splitlines():
        m = V2_SECTION_RE.match(line)
        if m:
            sid, title, a, b = m.groups()
            title = re.sub(r":.*$", "", title).strip()
            sections.append({"id": sid, "title": title, "planned_start": secs(a), "planned_end": secs(b)})
            continue
        if line.startswith("## New footage"):
            break
        m = V2_ROW_RE.match(line)
        if not m or not sections:
            continue
        sid, a, b, rest = m.groups()
        cells = [c.strip() for c in rest.split(" | ")]
        picture = cells[0]
        overlay = cells[1] if len(cells) >= 3 else "—"
        sound = cells[-1]
        sh = {"id": sid, "section": sections[-1]["id"], "planned_start": secs(a), "planned_len": round(secs(b) - secs(a), 3),
              "picture": picture, "overlay_text": overlay, "sound_text": sound, "over": None,
              "caption": None, "caption_sub": None, "diagram": None, "diagram_part": None, "bars": None}
        pl = plain(picture)
        if pl.startswith("REUSE") or "REUSE" in pl[:30]:
            kind = "game"
        elif "NEW" in pl:
            kind = "new"
        elif pl.startswith("full D"):
            kind = "diagram"
            sh["diagram"] = re.search(r"D\d\d", pl).group(0)
        elif "card" in pl:
            kind = "title"
        else:
            kind = "gag"
        sh["kind"] = kind
        sh["extra"] = ""
        sh["desc"] = plain(picture)
        sh["notice"] = plain(overlay) if overlay not in ("—", "-") else ""
        # captions and bumpers named in the overlay column: **TEXT**
        caps = []
        for mm in re.finditer(r"(bumper(?: caption)?|caption) \*\*(.+?)\*\*(?:[^|]*?at (\d+:\d+(?:\.\d+)?))?", overlay):
            kind_, text, at = mm.groups()
            caps.append({"kind": "bumper" if kind_.startswith("bumper") else "caption", "text": text,
                         "at_planned": secs(at) if at else None})
        sh["captions"] = caps
        # REUSE's file range: "file a–b" or "from a" (seconds into the source, handles included)
        fr = re.search(r"file (\d+(?:\.\d+)?)–(\d+(?:\.\d+)?)", pl) or re.search(r"from (\d+(?:\.\d+)?)", pl)
        sh["reuse_in"] = float(fr.group(1)) if fr else None
        rm = re.search(r"`(footage/[^`]+)`", picture) or None
        sh["reuse_path"] = rm.group(1) if rm else None
        ri = re.search(r"REUSE (S\d+[ab]?|F\d+)", pl)
        sh["reuse_id"] = ri.group(1) if ri else None
        rn = re.findall(r"NEW (N\d+[a-z]?)", pl)
        sh["renders"] = rn
        shots.append(sh)
    return sections, shots


def script_lines_v2(md: str) -> dict[str, dict]:
    """The new and changed lines of version 2's script: id -> {text, section, cut}."""
    out, sec = {}, None
    for line in md.splitlines():
        m = re.match(r"^#{2,3} (\d+(?:\.\d+)?)\.? ", line)
        if m:
            sec = m.group(1)
        m = V2_LINE_RE.match(line)
        if m:
            text = m.group(2).strip()
            cut = "[CUT]" in text
            text = text.replace("[CUT]", "").strip()
            out[m.group(1)] = {"text": text, "section": sec, "cut": cut}
    return out


def placeholder_clip(cid: str, text: str, section: str, wpm: float, head: float, tail: float) -> dict:
    """A line not recorded yet: its words evenly spread at `wpm`, silent, the same shape as a recorded clip."""
    words = text.split()
    dur = len(words) / wpm * 60
    ws, t = [], head
    step = dur / max(1, len(words))
    for w in words:
        ws.append({"text": w, "start": round(t, 3), "end": round(t + step * 0.85, 3)})
        t += step
    return {"id": cid, "section": section, "file": None, "text": text, "duration_s": round(head + dur + tail, 3),
            "speech_start_s": head, "speech_end_s": round(head + dur, 3), "words": ws, "placeholder": True,
            "pause_after_s": 0.6, "paragraph_end": True}


def clips_v2(cfg: dict) -> tuple[list[dict], float]:
    """Version 2's narration in order: v1's kept clips, the new lines' recordings where they exist, else placeholders."""
    p = cfg["paths"]
    v1 = json.loads((FILM / p["clips_v1"]).read_text())
    by_num = {c["id"].split("-")[0]: dict(c, file="voice/" + c["file"]) for c in v1["clips"]}
    new = {}
    side = FILM / p["clips_v2"]
    if side.exists():
        j = json.loads(side.read_text())
        base = side.parent
        for c in j.get("clips", []):
            f = c.get("file")
            if f and (base / f).exists():
                new[c["id"]] = dict(c, file=str((base / f).relative_to(FILM)))
    lines = script_lines_v2((FILM / p["script"]).read_text())
    head, tail = cfg["clock"]["voice_head"], cfg["clock"]["voice_tail"]
    out = []
    for cid in cfg["voice"]["order"]:
        if cid in by_num:
            c = dict(by_num[cid])
        elif cid in new:
            c = dict(new[cid])
            if c.get("pause_after_s") is None:
                c["pause_after_s"] = 0.6
            if c.get("paragraph_end") is None:
                c["paragraph_end"] = True
        elif cid in lines:
            wpm = cfg["voice"].get("wpm", {}).get(cid, cfg["voice"].get("wpm_default", 160))
            c = placeholder_clip(cid, lines[cid]["text"], lines[cid]["section"], wpm, head, tail)
        else:
            raise SystemExit(f"voice order: {cid!r} is neither a v1 clip, a recorded v2 clip nor a line of the script")
        if cid in lines:
            c["section"] = lines[cid]["section"]
            c["hard_end"] = lines[cid]["cut"]
        out.append(c)
    return out, v1["natural_sentence_gap_s"]



def v2_shot_settings(cfg: dict, shots: list[dict]) -> dict:
    """Each v2 shot's settings: v1's (edit.toml) where v2 reuses the shot under the same id, then what v2's
    shot list says (a REUSE's file range), then the v2 config's own."""
    v1 = tomllib.loads((FILM / cfg["paths"]["inherit"]).read_text()).get("shots", {}) if cfg["paths"].get("inherit") else {}
    v1_over = {sh["id"]: sh["over"] for sh in parse_shots(SHOTS_MD.read_text())[1]}
    v1_over.update({k: v["over"] for k, v in v1.items() if v.get("over")})
    mine = cfg.get("shots", {})
    out = {}
    for sh in shots:
        st: dict = {}
        same = sh.get("reuse_id") in (sh["id"], None) or sh["id"] in v1
        if same and sh["id"] in v1 and not mine.get(sh["id"], {}).get("no_inherit"):
            st.update(v1[sh["id"]])
        if sh.get("reuse_in") is not None:
            st["in"] = sh["reuse_in"]
        if sh.get("reuse_path"):
            st["source"] = sh["reuse_path"]
        if sh["id"] in v1_over and v1_over[sh["id"]] and "over" not in mine.get(sh["id"], {}):
            st.setdefault("over", v1_over[sh["id"]])
        st.update(mine.get(sh["id"], {}))
        out[sh["id"]] = st
    return out


def _footage(path: Path, cfg: dict, over: dict, status: str = "footage") -> dict:
    head = 0.0
    for d, h in cfg["footage"].get("head_by_dir", {}).items():
        if str(path.resolve()).startswith(str((FILM / d).resolve()) + "/"):
            head = float(h)
    meta = {}
    side = path.with_suffix(".json")
    if not side.exists() and path.stem.endswith(".clean"):  # X.clean.mp4: X's own sidecar
        side = path.with_name(path.stem[:-6] + ".json")
    if side.exists():
        try:
            meta = json.loads(side.read_text())
            head = float(meta.get("head_s", head))
        except (ValueError, json.JSONDecodeError):
            meta = {}
    out = {"type": "video", "path": str(path.resolve().relative_to(FILM)),
           "in": float(over.get("in", head)) + float(over.get("in_add", 0.0)), "head": head, "status": status}
    if meta.get("shot_s"):
        out["shot_s"] = float(meta["shot_s"])
    snd = meta.get("sound")
    if snd:  # "X.wav (side B)": the file is X.wav
        snd = re.sub(r"\s*\(.*\)\s*$", "", snd)
    if snd and path.stem.endswith(".v1") and not (path.parent / snd).exists():  # an old take kept as ID.v1.*
        snd = Path(snd).stem + ".v1" + Path(snd).suffix
    if snd and (path.parent / snd).exists() and not over.get("mute"):
        out["sound"] = str((path.parent / snd).resolve().relative_to(FILM))
    if over.get("sound") and (FILM / over["sound"]).exists() and not over.get("mute"):  # a file with no sidecar
        out["sound"] = over["sound"]
    for k in ("speed", "reverse", "fit", "out", "mask43", "fit_min", "lift"):
        if k in over:
            out[k] = over[k]
    return out


def _find(name: str, cfg: dict) -> Path | None:
    for g in cfg["footage"]["globs"]:
        for h in sorted(glob.glob(str(FILM / g.format(id=name)), recursive=True)):
            if h.endswith(".mp4") or h.endswith(".mov"):
                if probe(Path(h)).get("width"):
                    return Path(h)
    return None


def resolve_source_v2(sh: dict, cfg: dict, over: dict, shots: list[dict]) -> dict:
    """v2: an explicit source; a new render (by its N id, or the shot's own id); a full-screen diagram;
    the reused v1 file; an edit card; else a slate."""
    for s_ in ([over["source"]] if isinstance(over.get("source"), str) else over.get("source") or []):
        p = FILM / s_  # a list: the first that has landed (a new render first, then the one it replaces)
        if p.exists():
            return _footage(p, cfg, over, "footage" if "footage/" in s_ else "final")
    for r in over.get("render", sh.get("renders") or []) if isinstance(over.get("render", []), list) else [over["render"]]:
        p = _find(r, cfg)
        if p:
            out = _footage(p, cfg, over, "footage")
            out["render"] = r
            return out
    if sh.get("renders"):
        p = _find(sh["id"], cfg) if not over.get("no_own_id") else None
        if p:
            return _footage(p, cfg, over, "footage")
        return {"type": "slate", "status": "placeholder", "waiting_for": over.get("render", sh["renders"])}
    if sh["kind"] == "diagram" and sh["diagram"]:
        hits = [h for h in sorted(glob.glob(str(FILM / cfg["footage"]["diagram_glob"].format(d=sh["diagram"]))))
                if not re.search(r"_alpha|_\d", Path(h).stem)]
        if hits:
            return {"type": "video", "path": str(Path(hits[0]).relative_to(FILM)), "in": float(over.get("in", 0.0)),
                    "status": "diagram"}
    if sh.get("reuse_id"):
        p = _find(sh["id"], cfg) if cfg.get("paths", {}).get("format") == "v3" and sh["id"] != sh["reuse_id"] else None
        if p:  # a RE-RENDER written under the shot's own id
            return _footage(p, cfg, over, "footage")
        p = _find(sh["reuse_id"], cfg)
        if p:
            return _footage(p, cfg, over, "footage")
    if over.get("card"):
        return {"type": "card", "name": over["card"], "status": "card"}
    return {"type": "slate", "status": "placeholder"}


def add_placeholder_subtitles(shots: list[dict], voice: list[dict], fps: int) -> None:
    """Where a line is not recorded yet, its words as a subtitle over the shots it runs under."""
    for v in voice:
        if not v["placeholder"]:
            continue
        a, b = v["speech"]
        for s in shots:
            s0, s1 = s["start"], s["start"] + s["len"]
            if s1 <= a or s0 >= b:
                continue
            s["overlays"].append({"type": "card", "name": "subtitle", "text": v["text"], "id": v["id"],
                                  "from": round(max(0.0, a - s0), 3), "to": round(min(s["len"], b - s0), 3)})


def events_v2(tl: dict, cfg: dict) -> list[dict]:
    """The named moments the score and the sound effects anchor to (the config's [events])."""
    by = {s["id"]: s for s in tl["shots"]}
    out = []
    for name, ref in cfg.get("events", {}).items():
        m = re.match(r"^(\w+)(?:@(in|out)([+-][\d.]+)?|~(\S+)|\+([\d.]+))$", ref)
        if not m or m.group(1) not in by:
            t = resolve_ref(tl["shots"], ref)  # SHOT@burst and the rest
            out.append({"name": name, "ref": ref, "t": None if t is None else round(t, 4),
                        "frame": None if t is None else int(round(t * tl["fps"]))})
            continue
        s = by[m.group(1)]
        if m.group(2):
            t = s["start"] + (s["len"] if m.group(2) == "out" else 0.0) + float(m.group(3) or 0)
        elif m.group(4):
            w = word_ref(s["words"], m.group(4))
            t = None if w is None else s["start"] + w
        else:
            t = s["start"] + float(m.group(5))
        out.append({"name": name, "ref": ref, "t": None if t is None else round(t, 4),
                    "frame": None if t is None else int(round(t * tl["fps"]))})
    return out



def picture_events(tl: dict) -> list[dict]:
    """The footage's own events (every `*_shot_s` list in its sidecar) on the film's clock, where on screen."""
    out = []
    for s in tl["shots"]:
        src = s["source"]
        if src.get("type") != "video" or src.get("status") != "footage":
            continue
        side = (FILM / src["path"]).with_suffix(".json")
        if not side.exists():
            continue
        meta = json.loads(side.read_text())
        sp = float(src.get("speed", 1.0))
        off = float(meta.get("head_s", 0.0)) - src["in"]
        for k, v in meta.items():
            if not k.endswith("_shot_s"):
                continue
            groups: dict[str, list] = {}
            for x in (v if isinstance(v, list) else [v]):
                if isinstance(x, (int, float)):
                    groups.setdefault(k[:-7], []).append(float(x))
                elif isinstance(x, dict) and "t" in x:
                    groups.setdefault(k[:-7], []).append(float(x["t"]))
                elif isinstance(x, list) and x and isinstance(x[0], (int, float)):
                    groups.setdefault(f"{k[:-7]}: {x[1]}" if len(x) > 1 else k[:-7], []).append(float(x[0]))
            for what, vals in groups.items():
                ts = [(t + off) / sp for t in vals]
                ts = [round(s["start"] + t, 4) for t in ts if -0.05 <= t <= s["len"]]
                if ts:
                    out.append({"shot": s["id"], "what": what, "t": ts})
    return out


def edit_hits(tl: dict, cfg: dict) -> list[dict]:
    """The hits the edit itself makes ([event_lists] in the config: shot-relative seconds, or word references)."""
    by = {s["id"]: s for s in tl["shots"]}
    out = []
    for name, d in cfg.get("event_lists", {}).items():
        s = by.get(d["shot"])
        if s is None:
            continue
        base = by.get(d.get("base", d["shot"]), s)  # times counted from another shot's cut (an overlay's start)
        ts = []
        for v in d["at"]:
            if isinstance(v, str):
                w = word_ref(s["words"], v)
                if w is not None:
                    ts.append(round(s["start"] + w, 4))
            else:
                ts.append(round(base["start"] + float(v), 4))
        out.append({"shot": d["shot"], "what": name, "t": ts})
    for s in tl["shots"]:  # S27's console: its PASS lines, as the card types them (cards.card_terminal)
        if any(o.get("name") in ("terminal", "terminal_over") for o in s["overlays"]) or s["source"].get("name") == "terminal":
            _, _, passes, allpass = terminal_times(s["len"], s["cues"].get("allpass"))
            out.append({"shot": s["id"], "what": "pass_lines", "t": [round(s["start"] + x, 4) for x in passes]})
            out.append({"shot": s["id"], "what": "all_pass_card", "t": [round(s["start"] + allpass, 4)]})
    return out


def write_clock(tl: dict, cfg: dict, out_dir: Path) -> None:
    """clock.json and clock.txt (in the config's out folder): the cut, for the score and the sound effects."""
    fps = tl["fps"]
    ev = events_v2(tl, cfg)
    shots = []
    for s in tl["shots"]:
        src = s["source"]
        d = {"id": s["id"], "section": s["section"], "start": s["start"], "end": round(s["start"] + s["len"], 4),
             "frame": s["frame"], "frames": s["frames"], "len": s["len"],
             "nominal_start": s["planned_start"], "nominal_len": s["planned_len"],
             "picture": src.get("path") or src.get("name") or "placeholder", "status": src["status"],
             "words": [{"w": w["w"], "t": round(s["start"] + w["t"], 4)} for w in s["words"]]}
        if src.get("type") == "video":
            # film time = start + (shot time + head - in) / speed, for the footage's own event times
            d["source_in"] = src["in"]
            d["source_head"] = src.get("head", 0.0)
            if src.get("speed"):
                d["source_speed"] = src["speed"]
            d["shot_time_zero"] = round(s["start"] + (d["source_head"] - src["in"]) / float(src.get("speed", 1.0)), 4)
        shots.append(d)
    clock = {
        "film": {"duration": tl["duration"], "frames": tl["frames"], "fps": fps,
                 "written": __import__("time").strftime("%Y-%m-%d %H:%M:%S"),
                 "voice_placeholders": [v["id"] for v in tl["voice"] if v["placeholder"]],
                 "note": "Times are film seconds. Lines not recorded yet are timed at the script's nominal rate: "
                         "when they land, this clock moves; rerun the build and re-anchor to the new file."},
        "events": ev,
        "sections": [{"id": x["id"], "title": x["title"], "start": x["start"], "end": round(x["start"] + x["len"], 4),
                      "nominal_start": x["planned_start"]} for x in tl["sections"]],
        "cuts": [{"t": s["start"], "frame": s["frame"], "to": s["id"]} for s in tl["shots"]],
        "shots": shots,
        "voice": [{"id": v["id"], "at": v["at"], "speech": v["speech"], "text": v["text"],
                   "placeholder": v["placeholder"], "hard_end": v["hard_end"]} for v in tl["voice"]],
        "picture_events": picture_events(tl),
        "edit_hits": edit_hits(tl, cfg),
    }
    (out_dir / "clock.json").write_text(json.dumps(clock, indent=1, ensure_ascii=False))
    L = [f"quake-srp v2: the edit's clock ({clock['film']['written']})", "",
         f"film {tc(tl['duration'])} ({tl['duration']:.3f} s, {tl['frames']} frames at {fps} fps)",
         "lines not recorded yet (timed at the script's nominal rate; the clock moves when they land): "
         + (", ".join(clock["film"]["voice_placeholders"]) or "none"), "",
         "THE NAMED MOMENTS"]
    for e in ev:
        L.append(f"  {e['name']:<18} {tc(e['t']) if e['t'] is not None else '   -   '}  "
                 f"{'' if e['t'] is None else f'({e[chr(116)]:.3f} s, frame {e[chr(102) + chr(114) + chr(97) + chr(109) + chr(101)]})'}   {e['ref']}")
    L += ["", "SECTIONS"]
    for x in clock["sections"]:
        L.append(f"  {x['id']:>4} {x['title'][:24]:24s} {tc(x['start'])} - {tc(x['end'])}   (nominal {tc(x['nominal_start'])})")
    L += ["", "SHOTS  (start - end, length; the first words under it; its picture)"]
    for s in shots:
        w = " ".join(x["w"] for x in s["words"][:6])
        L.append(f"  {s['id']:<5} {tc(s['start'])} - {tc(s['end'])}  {s['len']:6.2f} s  "
                 f"{('“' + w + '…”') if w else '(no voice)':<44} {s['status']}: {Path(s['picture']).name}")
    L += ["", "VOICE"]
    for v in clock["voice"]:
        flag = " [placeholder]" if v["placeholder"] else ""
        flag += " [cut off: ends hard at its last sound]" if v["hard_end"] else ""
        L.append(f"  {v['id']:<10} {tc(v['speech'][0])} - {tc(v['speech'][1])}{flag}  {v['text'][:70]}")
    L += ["", "THE EDIT'S HITS (its cards, its sound switches, the diagram overlays' cues)"]
    for e in clock["edit_hits"]:
        L.append(f"  {e['shot']:<5} {e['what']:<28} " + ", ".join(f"{t:.3f}" for t in e["t"]))
    L += ["", "PICTURE EVENTS (from the footage sidecars)"]
    for e in clock["picture_events"]:
        L.append(f"  {e['shot']:<5} {e['what']:<24} " + ", ".join(f"{t:.3f}" for t in e["t"]))
    (out_dir / "clock.txt").write_text("\n".join(L) + "\n")



# ================================================================ version 3 ====
# v3: script/v3 (tables like v2's, ids like ST0, LAB3a, PER1, BD2; section 1's rows are v2's), the voice
# from voice/v3-lines.json (every V3 line -> its clip, TAKE or RECORD, with the script's pauses).

V3_SECTION_RE = re.compile(r"^#{2,3} (\d+(?:\.\d+)?)\.? (.+?) — (\d+:\d+(?:\.\d+)?)–(\d+:\d+(?:\.\d+)?)")
V3_ROW_RE = re.compile(r"^\| \**([A-Z][A-Za-z0-9]*)\** \| (\d+:\d+(?:\.\d+)?)–(\d+:\d+(?:\.\d+)?) \| (.*)\|\s*$")


def parse_shots_v3(md: str, cfg: dict) -> tuple[list[dict], list[dict]]:
    sections, shots = [], []
    borrowed = cfg["paths"].get("section_rows_from", {})
    for line in md.splitlines():
        m = V3_SECTION_RE.match(line)
        if m:
            sid, title, a, b = m.groups()
            title = re.sub(r":.*$", "", title).strip()
            sections.append({"id": sid, "title": title, "planned_start": secs(a), "planned_end": secs(b)})
            if sid in borrowed:  # a section the shot list keeps from an earlier version, its rows from there
                _, old = parse_shots_v2((FILM / borrowed[sid]).read_text())
                for sh in old:
                    if sh["section"] == sid:
                        shots.append(dict(sh))
            continue
        if line.startswith("## New footage"):
            break
        m = V3_ROW_RE.match(line)
        if not m or not sections:
            continue
        sid, a, b, rest = m.groups()
        cells = [c.strip() for c in rest.split(" | ")]
        picture, overlay, sound = cells[0], (cells[1] if len(cells) >= 3 else "—"), cells[-1]
        pl = plain(picture)
        sh = {"id": sid, "section": sections[-1]["id"], "planned_start": secs(a), "planned_len": round(secs(b) - secs(a), 3),
              "picture": picture, "overlay_text": overlay, "sound_text": sound, "over": None,
              "caption": None, "caption_sub": None, "diagram": None, "diagram_part": None, "bars": None,
              "extra": "", "desc": pl, "notice": plain(overlay) if overlay not in ("—", "-") else ""}
        sh["kind"] = ("new" if pl.startswith(("NEW", "RE-RENDER")) else "edit" if pl.startswith("EDIT") else
                      "title" if "card" in pl else "game")
        caps = []
        for mm in re.finditer(r"(?:(bumper(?: caption)?|caption) \*\*(.+?)\*\*|\*\*bumper (.+?)\*\*)"
                              r"(?:[^|]*?at (\d+:\d+(?:\.\d+)?))?", overlay):
            k_, t1, t2, at = mm.groups()
            caps.append({"kind": "bumper" if (t2 or (k_ or "").startswith("bumper")) else "caption",
                         "text": t1 or t2, "at_planned": secs(at) if at else None})
        sh["captions"] = caps
        fr = re.search(r"file (\d+(?:\.\d+)?)–(\d+(?:\.\d+)?)", pl) or re.search(r"from (\d+(?:\.\d+)?)", pl)
        sh["reuse_in"] = float(fr.group(1)) if fr else None
        rm = re.search(r"`((?:footage/)?web/[^`]+)`", picture)
        sh["reuse_path"] = (rm.group(1) if rm.group(1).startswith("footage/") else "footage/" + rm.group(1)) if rm else None
        ri = re.search(r"(?:REUSE|RE-RENDER) ([SFN]\d+[a-z]?(?:\.clean)?)", pl)
        sh["reuse_id"] = ri.group(1) if ri else None
        sh["renders"] = [sid] if sh["kind"] == "new" and not sh["reuse_id"] else []
        shots.append(sh)
    return sections, shots


def clips_v3(cfg: dict) -> tuple[list[dict], float]:
    """v3's narration: voice/v3-lines.json in script order, each line's words from the file it names."""
    vdir = FILM / "voice"
    lp = FILM / cfg["paths"]["lines"]
    if not lp.exists() and cfg["paths"].get("lines_standin"):  # the voice map not landed yet: the edit's stand-in
        print(f"note: {cfg['paths']['lines']} not there yet; using {cfg['paths']['lines_standin']}", file=sys.stderr)
        lp = FILM / cfg["paths"]["lines_standin"]
    doc = json.loads(lp.read_text())
    lines = doc["lines"]
    words_src: dict[str, dict] = {}
    for name in ("clips.json", "clips-v2.json", "clips-v3.json", "clips-v4.json", "clips-v6.json", "clips-v7.json"):
        f = vdir / name
        if f.exists():
            for c in json.loads(f.read_text())["clips"]:
                words_src[(name, c["id"])] = c
    if doc.get("natural_sentence_gap_s") is not None:  # a self-contained map (film/voice.json): every line's words in it
        gap = float(doc["natural_sentence_gap_s"])
    else:
        gap = json.loads((vdir / "clips.json").read_text())["natural_sentence_gap_s"]
    sections = {}
    sec = None
    for line in (FILM / cfg["paths"]["script"]).read_text().splitlines():
        m = re.match(r"^#{2,3} (\d+(?:\.\d+)?)\.? ", line)
        if m:
            sec = m.group(1)
        m = re.match(r"^\*\*(V\d-\w+)\*\*", line)
        if m:
            sections[m.group(1)] = sec
    split = cfg.get("voice", {}).get("split", {})
    out = []
    for ln in lines:
        emb = bool(ln.get("words"))  # the line carries its own words (film/voice.json, a stand-in)
        src = ln if emb else words_src.get((ln["words_from"], ln["clip"]))
        if src is None:
            raise SystemExit(f"{ln['id']}: no word times for clip {ln['clip']} in {ln['words_from']}")
        f_ = ln["file"] if ln["file"].startswith(("voice/", "/")) else "voice/" + ln["file"]
        sec_ = sections.get(ln["id"]) or sections.get(str(ln.get("replaces") or ""), "?")  # a new line: its section is the one it replaces
        c = {"id": ln["id"], "section": sec_, "file": f_, "text": ln["text"],
             "duration_s": ln["duration_s"], "speech_start_s": src["speech_start_s"], "speech_end_s": src["speech_end_s"],
             "words": src["words"]}
        if ln.get("pause_after_s") is not None:
            c["pause_after_s"], c["paragraph_end"] = float(ln["pause_after_s"]), True
        elif ln["kind"] == "TAKE" and (ln.get("clip_pause_after_s") if emb else src.get("pause_after_s")) is not None:
            c["pause_after_s"] = ln["clip_pause_after_s"] if emb else src["pause_after_s"]
            c["paragraph_end"] = bool(ln.get("clip_paragraph_end", True) if emb else src.get("paragraph_end", True))
        else:
            c["pause_after_s"], c["paragraph_end"] = 0.6, True
        if ln["id"] in split:  # one clip said as two: cut in its silence, a gap between
            at = float(split[ln["id"]]["at"])
            before = [w for w in c["words"] if w["end"] <= at]
            after = [dict(w, start=round(w["start"] - at, 3), end=round(w["end"] - at, 3)) for w in c["words"] if w["start"] >= at]
            a = dict(c, id=ln["id"] + "a", text=" ".join(w["text"] for w in before), words=before,
                     speech_end_s=before[-1]["end"], duration_s=round(before[-1]["end"] + 0.15, 3),
                     file_out=round(before[-1]["end"] + 0.15, 3), pause_after_s=0.0, paragraph_end=True)
            b = dict(c, id=ln["id"] + "b", text=" ".join(w["text"] for w in after), words=after,
                     speech_start_s=after[0]["start"], speech_end_s=round(c["speech_end_s"] - at, 3),
                     duration_s=round(c["duration_s"] - at, 3), file_in=at)
            out += [a, b]
            continue
        out.append(c)
    return out, gap


def v3_shot_settings(cfg: dict, shots: list[dict]) -> dict:
    """A v3 shot's settings: those of the earlier versions' shot of the same id (v1, then v2), or, for a
    renamed reuse, of the shot it reuses (its overlays, cues and levels, never its narration or framing);
    then what v3's shot list says; then this config."""
    layers = [tomllib.loads((FILM / f).read_text()).get("shots", {}) for f in cfg["paths"].get("inherit", [])]
    v1_over = {sh["id"]: sh["over"] for sh in parse_shots(SHOTS_MD.read_text())[1]}
    mine = cfg.get("shots", {})
    keep_from_reuse = ("overlays", "cues", "game_lufs", "dim", "fit")
    out = {}
    for sh in shots:
        st: dict = {}
        if not mine.get(sh["id"], {}).get("no_inherit"):
            for layer in layers:
                if sh["id"] in layer:
                    st.update({k: v for k, v in layer[sh["id"]].items() if k != "over"})
                elif sh.get("reuse_id") in layer:
                    st.update({k: v for k, v in layer[sh["reuse_id"]].items() if k in keep_from_reuse})
        if sh.get("reuse_in") is not None:
            st["in"] = sh["reuse_in"]
        if sh.get("reuse_path"):
            st["source"] = sh["reuse_path"]
        if v1_over.get(sh["id"]) and "over" not in mine.get(sh["id"], {}) and sh["section"] in cfg["paths"].get("v1_over_sections", []):
            st["over"] = v1_over[sh["id"]]
        st.update(mine.get(sh["id"], {}))
        out[sh["id"]] = st
    return out



def resolve_ref(shots: list[dict], ref: str) -> float | None:
    """A film time from 'SHOT@in', 'SHOT@out-0.2', 'SHOT@burst', 'SHOT~word' or 'SHOT+1.5'."""
    by = {s["id"]: s for s in shots}
    m = re.match(r"^(\w+)(?:@(in|out|burst)([+-][\d.]+)?|~(\S+)|\+([\d.]+))$", ref)
    if not m or m.group(1) not in by:
        return None
    s = by[m.group(1)]
    if m.group(2) == "burst":
        return s["start"] + s["source"].get("at", 0.0) + float(m.group(3) or 0)
    if m.group(2):
        return s["start"] + (s["len"] if m.group(2) == "out" else 0.0) + float(m.group(3) or 0)
    if m.group(4):
        w = word_ref(s["words"], m.group(4))
        return None if w is None else s["start"] + w
    return s["start"] + float(m.group(5))


def ladder_plan(shots: list[dict], cfg: dict, fps: int) -> None:
    """The SLOP OPTIONS ladder (shots.md): when each item lights, when it moves from the box's pillar to its
    panel, where it shows. Written to the shots as a card overlay, and kept for ladder.py's event file."""
    L = cfg["ladder"]
    on = {}
    for item in L["items"]:
        t = resolve_ref(shots, L["on"][item]) if item in L["on"] else None
        on[item] = None if t is None else round(t, 4)
    panel_at = resolve_ref(shots, L["panel_at"])
    appear = resolve_ref(shots, L["appear"])
    all_at = resolve_ref(shots, L["all_flash"]) if L.get("all_flash") else None
    off = {}
    if L.get("reverse_at"):  # each row dark on its own ref (S62's own steps)
        for item, ref in L["reverse_at"].items():
            t0 = resolve_ref(shots, ref)
            if t0 is not None:
                off[item] = round(t0, 4)
    elif L.get("reverse"):
        t0 = resolve_ref(shots, L["reverse"]["start"])
        if t0 is not None:
            for k, item in enumerate(reversed(L["items"])):
                off[item] = round(t0 + k * float(L["reverse"]["step"]), 4)
    plan = {"items": L["items"], "on": on, "off": off, "appear": appear, "panel_at": panel_at,
            "panel_dur": float(L.get("panel_dur", 0.5)), "all_flash": all_at,
            "all_fade_by": resolve_ref(shots, L["all_fade_by"]) if L.get("all_fade_by") else None}
    stage = set(L.get("stage", []))
    for s in shots:
        a, b = s["start"], s["start"] + s["len"]
        if s["id"] in stage:
            mode = "stage"
        else:
            hits = [t for t in on.values() if t is not None and a <= t < b]
            if not hits:
                continue
            mode = "lab"
        mov = FILM / L.get("movs", "diagrams/v3/ladder_{id}.mov").format(id=s["id"])
        if mov.exists():
            s["overlays"].append({"type": "video", "path": str(mov.relative_to(FILM)), "ladder": True})
        else:
            s["overlays"].append({"type": "card", "name": "ladder", "mode": mode, "plan": plan,
                                  "where": L.get("where", {}).get(s["id"]) or L.get("nudge", {}).get(s["id"])})
    cfg["_ladder_plan"] = plan


def write_ladder_events(tl: dict, cfg: dict, out_dir: Path) -> None:
    """edit/v3/ladder-events.json, in diagrams/v3/ladder.py's own format, on the edit's clock: show and fade
    per shot (stage shots: the whole shot; lab shots: around an item's lighting, on a 55% backing), each item's
    light, the fly to the panel at the burst, HERO's all-flash, and S62's items going dark in reverse."""
    plan = cfg.get("_ladder_plan")
    if not plan:
        return
    ev, spans = [], []
    for s in tl["shots"]:
        o = next((o for o in s["overlays"] if o.get("name") == "ladder" or o.get("ladder")), None)
        if o is None:
            continue
        a, b = round(s["start"], 4), round(s["start"] + s["len"], 4)
        spans.append({"name": s["id"], "from": a, "to": b})
        mode = o.get("mode") or ("stage" if s["id"] in cfg["ladder"].get("stage", []) else "lab")
        if mode == "stage":
            ev.append({"t": a, "do": "back", "alpha": 0, "dur": 0})
            ev.append({"t": max(a, plan["appear"] or a), "do": "show", "dur": 0.1 if a > (plan["appear"] or 0) else 0.3})
        else:
            hits = sorted(t for t in plan["on"].values() if t is not None and a <= t < b)
            ev.append({"t": a, "do": "back", "alpha": 0.55, "dur": 0})
            ev.append({"t": round(max(a, hits[0] - 0.3), 4), "do": "show"})
            ev.append({"t": round(min(b - 0.3, hits[-1] + 1.5), 4), "do": "fade", "dur": 0.3})
        if mode == "stage":
            ev.append({"t": round(b - 0.001, 4), "do": "fade", "dur": 0.001})
        nd = cfg["ladder"].get("nudge", {}).get(s["id"])
        if nd:  # off this shot's subject, for this shot only
            ev.append({"t": a, "do": "nudge", "dx": nd[0], "dy": nd[1], "dur": 0})
            ev.append({"t": round(b - 0.001, 4), "do": "nudge", "dx": 0, "dy": 0, "dur": 0})
    for it, t in plan["on"].items():
        if t is not None:
            ev.append({"t": t, "do": "light", "item": it})
    for it, t in plan["off"].items():
        ev.append({"t": t, "do": "dark", "item": it})
    if plan["panel_at"] is not None:
        ev.append({"t": plan["panel_at"], "do": "fly", "to": "panel", "dur": plan["panel_dur"]})
    if plan["all_flash"] is not None:
        ev.append({"t": plan["all_flash"], "do": "all-flash"})
    if plan["all_fade_by"] is not None:
        ev.append({"t": round(plan["all_fade_by"] - 0.5, 4), "do": "fade", "dur": 0.5})
    ev.sort(key=lambda e: (e["t"], 0 if e["do"] in ("back", "fly") else 1))
    out = {"about": "The ladder on the edit's clock (edit/v3/timeline.json), written by edit/timeline.py: render with "
                    "diagrams/v3/ladder.py; the edit lays diagrams/v3/ladder_SHOT.mov over each span when it exists.",
           "fps": tl["fps"], "flash_frames": 6, "film_duration": tl["duration"], "events": ev, "spans": spans}
    (out_dir / "ladder-events.json").write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
