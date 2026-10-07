#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.14"
# dependencies = ["numpy", "scipy", "numba"]
# ///
"""Render the film's sound-design layer: a cue list on the film's timeline -> one stem.

    uv run film/pipeline/sound/render.py            # cues-v7.md on edit/v7 -> FILM_ROOT/sound/sfx-v7.wav
    uv run film/pipeline/sound/render.py [CUES.md ...] [-o OUT.wav] [--timeline TIMELINE.json]
        [--master DB | --lufs L] [--ceiling -1] [--list] [--preview FILM-AUDIO]

Writes a 48 kHz, 24-bit stereo WAV exactly as long as the timeline (frames / fps), and a JSON
beside it: every cue's film time, sample, sound, gain, and anything skipped and why. `--list`
resolves and prints the cues without rendering. It reads the timeline, the clock.json and
ladder-events.json beside it, the footage's sidecars, event logs and game sound
(FILM_ROOT/footage/game/), id's sounds (extract_id.py) and the designed ones (design.py).
The whole film takes minutes. The stem is delivered unducked: the edit mixes and ducks it.

The cue format. In a .md file only the lines inside fenced blocks tagged `sfx` are read (in any
other file, every line); `#` starts a comment. A cue is `WHEN SOUND [key=value ...]`.

WHEN, with an optional trailing offset (`S22@in-0.4`, `S12~pixel+0.1`):
  83.2, 1:23.20             film seconds
  sec4.4                    a section's start (the timeline's section ids)
  S22, S22@in, S22@out      the cut into the shot, or out of it
  S22+2.28                  shot time: seconds from the shot proper's first frame (the footage
                            sidecar's "shot seconds", a diagram's own clock); it holds however
                            the edit trims the shot, and follows the shot's speed
  S12~pixel, S12~pixel#2, S12~colours.end   where the narration says that word in the shot
  S22:hits, S22:hits[1], S18:menu_events[key=enter]   times named in the clock's picture events
                            and edit hits, the shot's timeline cues, its sidecar (drop `_shot_s`)
                            or its diagram's cue file; a list gives one cue per entry, and entries
                            in trimmed handles are dropped
  F01^1                     the shot's first overlay appearing
  torches_hit+0.2           a named moment from clock.json
  ladder[do=light,item!=TORCH_FLICKER]   the events of ladder-events.json (filters AND-ed, `_`
                            matches a space); never dropped at a cut
  S47+0.4,0.7,0.9           several times at once

SOUND: a designed name (`whoosh-short`), an id path (`weapons/rocket1i` or `id:...`), or
`file:PATH.wav` (relative to FILM_ROOT).

Options: gain=dB (relative to the sound's own level); lufs=L (the cue's loudest 400 ms at L, in
the edit's scale: voice -16, game -30); pan=-0.5 or pan=-1>1 (a glide); pitch=semitones and rate=x
(tape-style); rev=1; reverb= and room= (sends, 0..1); lp=, hp= (Hz); start=, dur= (s; dur is the
output's length); loop=1 dur=s (from id's loop point); fadein=, fadeout=; align=start|sync|end
(which point lands on WHEN; default the sound's sync point); times=N every=s; spread=a>b (pan
across the repeats); sync=s (this instant of the file lands on WHEN); flutter=d[,Hz] (a torch's
flicker); tapestop=s; gap=a,b[;c,d] (breaths cut out around the sync); underwords=dB (lower where
it touches a narrated word); avoid=WORDREF:s; crest=dB (a sustained sound's own peak limit);
force=1 (place an id sound even if the footage already carries it); shot=SID; name=TAG.

`silence FROM TO` cuts every cue that started before FROM, and the reverbs, until TO (the stem is
undithered, so a silence is exact zeros). `events SHOT [only=wizard/] [lufs=L] [map.KIND=SOUND]`
places a shot's event log (the film tool's SHOT.events.json): its `sound` events become id's
sounds at id's own spatialised volumes, each cut off by the next on its entity and channel.

An id sound is skipped (and the JSON says why) when it would double the footage's own sound: a
`sound` event or an ambient loop on a shot that carries its game audio, or a sound already in
that audio within 0.35 s (a band-passed correlation of 0.7 or more). force=1 overrides it.
"""
from __future__ import annotations

import argparse
import json
import math
import re
import subprocess
import sys
import zlib
from pathlib import Path

import numpy as np
from scipy import signal

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import sfxlib  # noqa: E402
from sfxlib import FILM, SR, dsp  # noqa: E402

MASTER_DB = 7.8   # the film's stem, a fixed gain: a cue's lufs= is its level in the edit's mix at its sfx gain of -8 dB

NUM = r"[+-]?(?:\d+\.?\d*|\.\d+)"
OFF = r"[+-](?:\d+\.?\d*|\.\d+)"
SHOT_RE = re.compile(
    rf"^(?P<sid>[A-Za-z][A-Za-z0-9]*?)"
    rf"(?:(?P<clock>{OFF})"
    rf"|@(?P<edge>in|out)"
    rf"|~(?P<word>[^#+\-.\s]+)(?:#(?P<occ>\d+))?(?P<wend>\.end)?"
    rf"|:(?P<key>[A-Za-z0-9_]+)(?:\[(?P<filt>[^\]]*)\])?"
    rf"|\^(?P<ov>\d+))?"
    rf"(?P<off>{OFF})?$")
SEC_RE = re.compile(rf"^sec(?P<sec>\d+(?:\.\d+)?)(?P<off>{OFF})?$")
ABS_RE = re.compile(r"^(?:(?P<m>\d+):)?(?P<s>\d+(?:\.\d+)?)$")
NAME_RE = re.compile(rf"^(?P<name>[a-z][a-z0-9_]*)(?P<off>{OFF})?$")

PLR = 18.0   # a lufs= cue's true peak at most this far above its level (short ticks would otherwise peak near 0 dBFS)
PLR_LONG = 12.0   # a sustained lufs= cue's peaks are limited to this far above its level
CUE_KEYS = {"gain", "pan", "pitch", "rate", "rev", "reverb", "room", "lp", "hp", "start", "dur", "loop",
            "fadein", "fadeout", "align", "times", "every", "force", "shot", "name",
            "lufs", "sync", "spread", "flutter", "tapestop", "crest", "gap", "underwords", "avoid"}
EVENT_KEYS = CUE_KEYS | {"kinds", "handles", "file", "only"}


class CueError(Exception):
    pass


# ---------------------------------------------------------------- the cue files

def read_cue_lines(path: Path) -> list[tuple[str, int, str]]:
    """(file, line number, text) for each cue line. In a .md file, only the lines inside fenced
    blocks whose info string is `sfx`; elsewhere every line. '#' starts a comment at the start of
    a line or after a space."""
    out = []
    md = path.suffix.lower() in (".md", ".markdown")
    fence, take = None, not md
    for i, raw in enumerate(path.read_text().splitlines(), 1):
        s = raw.strip()
        if md and s.startswith("```"):
            if fence is None:
                fence, take = s, s[3:].strip().split()[:1] == ["sfx"]
            else:
                fence, take = None, False
            continue
        if not take:
            continue
        s = re.sub(r"(^|\s)#.*$", "", raw).strip()
        if s:
            out.append((str(path), i, s))
    return out


def parse_opts(tokens: list[str], allowed: set[str], where: str) -> dict:
    opts = {}
    for tok in tokens:
        if "=" not in tok:
            raise CueError(f"{where}: '{tok}' is not key=value")
        k, v = tok.split("=", 1)
        base = k.split(".", 1)[0]
        if base not in allowed and base != "map":
            raise CueError(f"{where}: unknown option '{k}' (known: {', '.join(sorted(allowed))}, map.KIND)")
        opts[k] = v
    return opts


# ---------------------------------------------------------------- the film's clock

class Film:
    def __init__(self, timeline: Path):
        self.path = timeline
        tl = json.loads(timeline.read_text())
        self.tl = tl
        self.fps = tl["fps"]
        self.duration = tl["frames"] / tl["fps"]
        self.n = int(round(self.duration * SR))
        self.shots = {s["id"]: s for s in tl["shots"]}
        self.order = [s["id"] for s in tl["shots"]]
        self.sections = {s["id"]: s for s in tl["sections"]}
        self._side, self._game = {}, {}
        # the edit's clock.json beside the timeline: named moments, picture events, shot zeros
        cj = timeline.parent / "clock.json"
        c = json.loads(cj.read_text()) if cj.exists() else {}
        self.clock_path = cj if cj.exists() else None
        self.moments = {e["name"]: float(e["t"]) for e in c.get("events", [])}
        self.picture = {(e["shot"], e["what"]): [float(t) for t in e["t"]]
                        for e in c.get("picture_events", []) + c.get("edit_hits", [])}   # both in film time
        self.zero = {e["id"]: float(e["shot_time_zero"]) for e in c.get("shots", [])
                     if e.get("shot_time_zero") is not None}
        self.words = sorted((s["start"] + w["t"], s["start"] + w["e"], w["w"]) for s in tl["shots"]
                            for w in s.get("words", []))       # every narrated word, in film time
        lj = timeline.parent / "ladder-events.json"            # the SLOP OPTIONS ladder, in film time
        self.ladder = json.loads(lj.read_text())["events"] if lj.exists() else []

    # the shot's sidecar (footage/game/ID.json), its handles and its own sound
    def sidecar(self, sid: str) -> dict:
        if sid not in self._side:
            src = self.shots[sid]["source"]
            p = FILM / src["path"] if src.get("path") else None
            j = {}
            if p is not None:
                base = p.parent / (p.name.split(".", 1)[0] + ".json")     # S38.v1.mp4 -> S38.json
                for q in (p.with_suffix(".json"), base):
                    if q.exists():
                        j = json.loads(q.read_text())
                        j["_path"] = str(q)
                        break
                if "head_s" not in j and p.parent.name == "game" and p.parent.parent.name == "footage":
                    j["head_s"] = 1.0                                        # edit.toml's head_by_dir
            self._side[sid] = j
        return self._side[sid]

    def head(self, sid: str) -> float:
        return float(self.sidecar(sid).get("head_s", 0.0) or 0.0)

    def shot(self, sid: str) -> dict:
        if sid not in self.shots:
            raise CueError(f"no shot {sid} in {self.path.name}")
        return self.shots[sid]

    def window(self, sid: str) -> tuple[float, float]:
        s = self.shot(sid)
        return s["start"], s["start"] + s["len"]

    def clock(self, sid: str, t_shot: float) -> float:
        """Film time of shot time t (seconds from the shot proper's first frame: the source's own
        clock minus its head handle; a diagram's own clock; a card's offset from its start)."""
        s = self.shot(sid)
        src = s["source"]
        sp = float(src.get("speed") or 1.0)                   # the edit may play a shot slower or faster
        if sid in self.zero:                                  # the edit's own mapping
            return self.zero[sid] + t_shot / sp
        if src.get("type") == "video" and src.get("path"):
            return s["start"] + (self.head(sid) + t_shot - float(src.get("in", 0.0))) / sp
        return s["start"] + t_shot

    def shot_at(self, t: float) -> str | None:
        for sid in self.order:
            a, b = self.window(sid)
            if a <= t < b:
                return sid
        return None

    def game_audio(self, sid: str) -> tuple[np.ndarray, Path] | None:
        """The shot's own game sound (aligned to its mp4's first frame), at 48 kHz, if it has one."""
        if sid in self._game:
            return self._game[sid]
        s = self.shot(sid)
        src = s["source"]
        snd = src.get("sound") or self.sidecar(sid).get("sound")
        if snd:
            snd = re.sub(r"\s*\(.*\)\s*$", "", snd)          # "ST1a.wav (side B)" -> ST1a.wav
        res = None
        if snd and src.get("path"):
            p = FILM / snd if (FILM / snd).exists() else (FILM / src["path"]).parent / snd
            if p.exists():
                x, _ = sfxlib.load(p)
                res = (x, p)
        self._game[sid] = res
        return res

    def wav_sample(self, sid: str, film_t: float) -> int:
        """The sample of the shot's own WAV (aligned to its mp4's first frame) heard at film_t."""
        s = self.shot(sid)
        src = s["source"]
        sp = float(src.get("speed") or 1.0)
        return int(round((float(src.get("in", 0.0)) + (film_t - s["start"]) * sp) * SR))

    def carries_sound(self, sid: str) -> bool:
        s = self.shot(sid)
        src = s["source"]
        return bool(src.get("sound") or self.sidecar(sid).get("sound") or (src.get("probe") or {}).get("audio"))

    # WHEN -> film times
    def resolve(self, when: str) -> list[tuple[float, str | None]]:
        m = re.match(r"^(.+\+)([0-9.]+(?:,[0-9.]+)+)$", when)
        if m:                                             # SID+t1,t2,... or SID@in+t1,t2,...: several times
            return [x for t in m[2].split(",") for x in self.resolve(f"{m[1]}{t}")]
        m = ABS_RE.match(when)
        if m:
            return [(int(m["m"] or 0) * 60 + float(m["s"]), None)]
        m = SEC_RE.match(when)
        if m:
            if m["sec"] not in self.sections:
                raise CueError(f"no section {m['sec']} (sections: {', '.join(self.sections)})")
            return [(self.sections[m["sec"]]["start"] + float(m["off"] or 0), None)]
        m = re.match(rf"^ladder(?:\[(?P<filt>[^\]]*)\])?(?P<off>{OFF})?$", when)
        if m:                                             # ladder[do=light,item!=TORCHES]: the ladder's events
            if not self.ladder:
                raise CueError("no ladder-events.json beside the timeline")
            ts = pick(self.ladder, m["filt"], "ladder")
            return [(t + float(m["off"] or 0), self.shot_at(t)) for t in ts]
        m = NAME_RE.match(when)
        if m and m["name"] in self.moments:
            t = self.moments[m["name"]] + float(m["off"] or 0)
            return [(t, self.shot_at(t))]
        m = SHOT_RE.match(when)
        if not m:
            raise CueError(f"can't read the time '{when}'")
        sid = m["sid"]
        s = self.shot(sid)
        off = float(m["off"] or 0)
        if m["clock"]:
            return [(self.clock(sid, float(m["clock"])) + off, sid)]
        if m["edge"] == "out":
            return [(s["start"] + s["len"] + off, sid)]
        if m["word"]:
            want = norm_word(m["word"])
            hits = [w for w in s.get("words", []) if norm_word(w["w"]) == want]
            k = int(m["occ"] or 1)
            if len(hits) < k:
                said = " ".join(w["w"] for w in s.get("words", [])) or "(no narration)"
                raise CueError(f"{sid}: the word '{m['word']}' #{k} isn't spoken in the shot: {said}")
            w = hits[k - 1]
            return [(s["start"] + (w["e"] if m["wend"] else w["t"]) + off, sid)]
        if m["key"]:
            return [(t + off, sid) for t in self.key_times(sid, m["key"], m["filt"])]
        if m["ov"]:
            ovs = s.get("overlays", [])
            k = int(m["ov"])
            if not 1 <= k <= len(ovs):
                raise CueError(f"{sid} has {len(ovs)} overlays; ^{k} doesn't exist")
            return [(s["start"] + float(ovs[k - 1].get("at", 0.0)) + off, sid)]
        return [(s["start"] + off, sid)]                                  # SID, SID@in

    def key_times(self, sid: str, key: str, filt: str | None) -> list[float]:
        """SID:KEY -> film times. Looked up in order: the timeline's own cues for the shot (film
        offsets from its start); the footage sidecar (shot seconds: a number, a list of numbers,
        or a list of objects with 't'; KEY may drop the '_shot_s'); the diagram's cue file
        (film/pipeline/diagrams/cues/NAME.json, the diagram's clock)."""
        s = self.shot(sid)
        for k in (key, key.removesuffix("_shot_s")):
            if (sid, k) in self.picture:                       # clock.json's picture events: film times
                return [t for t in pick(self.picture[(sid, k)], filt, f"{sid}:{k}")]
        if (s.get("cues") or {}).get(key) is not None:
            return [s["start"] + float(s["cues"][key])]
        side = self.sidecar(sid)
        for k in (key, f"{key}_shot_s"):
            if k in side:
                return [self.clock(sid, t) for t in pick(side[k], filt, f"{sid}:{k}")]
        src = s["source"]
        if src.get("path"):
            cf = HERE.parent / "diagrams" / "cues" / (Path(src["path"]).stem + ".json")
            if cf.exists():
                cues = json.loads(cf.read_text())
                if key in cues:
                    return [self.clock(sid, t) for t in pick(cues[key], filt, f"{sid}:{key}")]
        known = sorted(set((s.get("cues") or {})) | {k for k, v in side.items() if k.endswith("_s") or k.endswith("_shot_s")})
        raise CueError(f"{sid}: no '{key}' in the shot's cues, sidecar or diagram cues (try: {', '.join(known) or 'none'})")


def norm_word(w: str) -> str:
    return re.sub(r"[^a-z0-9]", "", w.lower())


def pick(v, filt: str | None, where: str) -> list[float]:
    """A sidecar value as a list of times; [i] picks one, [k=v] filters a list of objects."""
    items = v if isinstance(v, list) else [v]
    if filt:
        if re.fullmatch(r"-?\d+", filt.strip()):
            i = int(filt)
            try:
                items = [items[i]]
            except IndexError:
                raise CueError(f"{where} has {len(items)} entries; [{i}] doesn't exist")
        else:                                       # k=v[,k!=v...]: all must hold; '_' matches a space
            for cond in filt.split(","):
                neg = "!=" in cond
                k, _, val = cond.partition("!=" if neg else "=")
                k, val = k.strip(), val.strip().replace("_", " ")
                items = [x for x in items if isinstance(x, dict)
                         and ((str(x.get(k)).replace("_", " ") == val) != neg)]
            if not items:
                raise CueError(f"{where}: nothing matches [{filt}]")
    out = []
    for x in items:
        t = x.get("t", x.get("shot_s")) if isinstance(x, dict) else x
        if t is None:
            raise CueError(f"{where}: an entry without a time")
        out.append(float(t))
    return out


# ---------------------------------------------------------------- sounds

def load_sound(name: str) -> tuple[np.ndarray, float, dict, str]:
    """(audio (2, n) at 48 kHz, its sync point in seconds, info, resolved name)."""
    if name.startswith("file:"):
        p = FILM / name[5:]
        x, info = sfxlib.load(p)
        return x, 0.0, info, name
    if name.startswith("id:") or "/" in name:
        rel = name.removeprefix("id:")
        rel = rel if rel.endswith(".wav") else rel + ".wav"
        p = sfxlib.ID_DIR / rel
        if not p.exists():
            raise CueError(f"no id sound '{rel}' (see sound/id/LISTING.md)")
        x, info = sfxlib.load(p)
        return x, 0.0, info, "id:" + rel.removesuffix(".wav")
    nm = name.removeprefix("designed:")
    idx = sfxlib.designed_index()
    if nm not in idx:
        raise CueError(f"no designed sound '{nm}' (see sound/designed/index.json)")
    x, info = sfxlib.load(sfxlib.SOUND / idx[nm]["file"])
    return x, float(idx[nm]["sync"]), info, nm


def fnum(opts, k, default=None):
    v = opts.get(k)
    if v is None:
        return default
    try:
        return float(v)
    except ValueError:
        raise CueError(f"{k}={v} is not a number")


def shape(x: np.ndarray, sync: float, info: dict, o: dict) -> tuple[np.ndarray, float]:
    """Apply a cue's options to a sound; returns it and its (moved) sync point.
    The sync point may fall before the (trimmed) start or after the end: the cue still lands it."""
    if o.get("sync") is not None:
        sync = fnum(o, "sync")                    # file seconds: this instant of the file lands on WHEN
    st = fnum(o, "start", 0.0)
    if st:
        x = x[:, dsp.nsamp(st):]
        sync = sync - st
    r = fnum(o, "rate", 1.0) * 2 ** (fnum(o, "pitch", 0.0) / 12)
    dur = fnum(o, "dur")
    if dur is not None:
        dur = dur * r                             # dur is the output's length: so much source at this rate
    if o.get("loop") in ("1", "true", "yes"):
        if dur is None:
            raise CueError("loop=1 needs dur=")
        ls = int(info.get("loop_start_48k", 0)) - dsp.nsamp(st) if st else int(info.get("loop_start_48k", 0))
        ls = max(0, ls)
        need = dsp.nsamp(dur)
        body, cyc = x[:, :ls], x[:, ls:]
        if cyc.shape[1] < 32:
            raise CueError("the sound is too short to loop")
        reps = math.ceil(max(0, need - body.shape[1]) / cyc.shape[1]) + 1
        x = np.concatenate([body] + [cyc] * reps, axis=1)
    if dur is not None:
        x = x[:, :dsp.nsamp(dur)].copy()
        if fnum(o, "fadeout") is None:
            dsp.tail_fade(x, min(0.03, dur / 4 / r))
    if o.get("rev") in ("1", "true", "yes"):
        x = x[:, ::-1].copy()
        sync = x.shape[1] / SR - sync
    if abs(r - 1) > 1e-6:
        x = sfxlib.varispeed(x, r)
        sync /= r
    if fnum(o, "hp"):
        x = dsp.butter(x, "highpass", fnum(o, "hp"), 2, zero_phase=False)
    if fnum(o, "lp"):
        x = dsp.butter(x, "lowpass", fnum(o, "lp"), 2, zero_phase=False)
    x = np.array(x, dtype=float, copy=True)
    if fnum(o, "tapestop"):                       # the last T seconds wind down to a stop, tape-style
        T = fnum(o, "tapestop")
        n = x.shape[1]
        i0 = max(0, n - dsp.nsamp(T))
        u = np.arange(n - i0) / max(1, n - i0)
        pos = i0 + np.concatenate([[0.0], np.cumsum((1 - u) ** 1.5)[:-1]])
        k = np.clip(np.floor(pos).astype(int), 0, n - 2)
        f = pos - k
        tail = x[:, k] * (1 - f) + x[:, k + 1] * f
        tail = dsp.butter(tail * (1 - u) ** 0.5, "lowpass", 6000.0, 2, zero_phase=False)
        x = np.concatenate([x[:, :i0], tail], axis=1)
    if o.get("flutter"):                          # a torch's flicker: random steps at 10 Hz, smoothed
        depth, _, rate = o["flutter"].partition(",")
        depth, rate = float(depth), float(rate or 10.0)
        rng = np.random.default_rng(zlib.crc32(o["flutter"].encode()) % 1000 + x.shape[1])   # the same every run
        n = x.shape[1]
        steps = rng.random(int(n / SR * rate) + 2)
        g = 1 - depth * steps[(np.arange(n) / SR * rate).astype(int)]
        g = dsp.onepole_lp(g, 40.0)
        x = x * g
    fi, fo = fnum(o, "fadein"), fnum(o, "fadeout")
    if fi:
        m = min(x.shape[1], dsp.nsamp(fi))
        x[:, :m] *= np.linspace(0, 1, m)
    if fo:
        m = min(x.shape[1], dsp.nsamp(fo))
        x[:, -m:] *= np.linspace(1, 0, m)
    al = o.get("align", "sync")
    if al == "start":
        sync = 0.0
    elif al == "end":
        sync = x.shape[1] / SR
    elif al != "sync":
        raise CueError(f"align={al}: start | sync | end")
    pan = o.get("pan")
    if pan:
        n = x.shape[1]
        if ">" in pan:
            a, b = (float(v) for v in pan.split(">", 1))
            p = np.clip(np.linspace(a, b, n), -1, 1)
        else:
            p = np.full(n, float(np.clip(float(pan), -1, 1)))
        ang = (p + 1) * np.pi / 4
        x = x * np.vstack([np.cos(ang), np.sin(ang)]) * np.sqrt(2)
    for gw in (o.get("gap") or "").split(";"):    # gap=a,b[;c,d]: breaths cut out of the sound, around its sync
        if not gw:
            continue
        ga, _, gb = gw.partition(",")
        i0 = max(0, int(round((sync + float(ga)) * SR)))
        i1 = min(x.shape[1], int(round((sync + float(gb)) * SR)))
        f = dsp.nsamp(0.003)
        if i1 > i0:
            x = x.copy()
            x[:, max(0, i0 - f):i0] *= np.linspace(1, 0, i0 - max(0, i0 - f))[None, :]
            x[:, i0:i1] = 0
            x[:, i1:i1 + f] *= np.linspace(0, 1, min(f, x.shape[1] - i1))[None, :]
    if o.get("_lr"):                              # an event's own left/right volumes (id's spatialisation)
        gl, gr = (float(v) for v in o["_lr"].split(","))
        x = x * np.array([[gl], [gr]])
    g = fnum(o, "gain", 0.0)
    if o.get("lufs") is not None:                 # level: the cue's loudest 400 ms at this loudness...
        m = sfxlib.momentary_max(x)
        if np.isfinite(m) and m > -90:
            L = fnum(o, "lufs")
            g += L - m
            tp = dsp.true_peak(x) + g
            crest = fnum(o, "crest", PLR_LONG)
            if x.shape[1] > dsp.nsamp(0.5) and tp > L + crest:
                # a sustained sound with spiky peaks (id's 8-bit crackle): its own fast limiter, so the
                # stem's limiter never pumps under it; the level stays where it was asked
                y, _ = dsp.limit(x * 10 ** (g / 20), L + crest, lookahead_ms=2.0, release_ms=40.0)
                return y, sync
            if tp > L + PLR:                      # ...a click's peak no more than PLR dB above it
                g -= tp - (L + PLR)
    return x * 10 ** (g / 20), sync


# ---------------------------------------------------------------- expanding the lines into cues

def events_file(film: Film, sid: str, opts: dict) -> Path:
    if opts.get("file"):
        p = Path(opts["file"])
        return p if p.is_absolute() else FILM / p
    src = film.shot(sid)["source"]
    if not src.get("path"):
        raise CueError(f"{sid} has no footage, so no events")
    mp4 = FILM / src["path"]
    side = film.sidecar(sid)
    cands = ([mp4.parent / side["events"]] if side.get("events") else []) + [
        mp4.with_suffix(".events.json"), mp4.parent / f"{mp4.stem}-events.json", mp4.parent / mp4.stem / "events.json"]
    for p in cands:
        if p.exists():
            return p
    raise CueError(f"{sid}: no events file (looked for {', '.join(str(c.relative_to(FILM)) for c in cands)})")


def expand(film: Film, lines: list[tuple[str, int, str]]) -> tuple[list[dict], list[dict], list[tuple]]:
    """Cue dicts {where, when, sound, opts, t, shot}, the lines' errors, and the silences."""
    cues, errors, silences = [], [], []
    for f, ln, text in lines:
        where = f"{Path(f).name}:{ln}"
        tok = text.split()
        try:
            if tok[0] == "events":
                if len(tok) < 2:
                    raise CueError("events SHOT [options]")
                sid = tok[1]
                o = parse_opts(tok[2:], EVENT_KEYS, where)
                cues += expand_events(film, sid, o, where, text)
                continue
            if tok[0] == "silence":                     # silence FROM TO: cut what started before FROM
                if len(tok) != 3:
                    raise CueError("silence FROM TO")
                (t0, _), (t1, _) = film.resolve(tok[1])[0], film.resolve(tok[2])[0]
                if t1 <= t0:
                    raise CueError(f"silence: {tok[2]} ({t1:.3f}) is not after {tok[1]} ({t0:.3f})")
                silences.append((t0, t1, where))
                continue
            if len(tok) < 2:
                raise CueError("a cue is: WHEN SOUND [key=value ...]")
            o = parse_opts(tok[2:], CUE_KEYS, where)
            times = film.resolve(tok[0])
            n_rep, every = int(fnum(o, "times", 1)), fnum(o, "every", 0.0)
            all_t = [(t + k * every, sid) for t, sid in times for k in range(n_rep)]
            spread = None
            if o.get("spread"):
                lo, _, hi = o["spread"].partition(">")
                spread = (float(lo), float(hi or lo))
            for i, (t, sid) in enumerate(all_t):
                oo = o
                if spread:                              # pan stepped across the cue's repeats
                    u = i / max(1, len(all_t) - 1)
                    oo = dict(o, pan=f"{spread[0] + (spread[1] - spread[0]) * u:.3f}")
                cues.append({"where": where, "line": text, "when": tok[0], "sound": tok[1], "opts": oo,
                             "t": t, "shot": o.get("shot") or sid,
                             "clip": len(times) > 1 and not tok[0].startswith("ladder")})  # ladder times are film events
        except CueError as e:
            errors.append({"where": where, "line": text, "error": str(e)})
    return cues, errors, silences


def id_gains(ev: dict) -> tuple[float, float]:
    """An event's left and right volume as id's mixer set them (snd_dma.c SND_Spatialize): the
    log's own left/right when it has them, else volume x (1 - dist x attenuation / 1000) x (1 -+ pan)."""
    if "left" in ev and "right" in ev:
        return min(1.0, ev["left"] / 255), min(1.0, ev["right"] / 255)
    vol = float(ev.get("volume", ev.get("vol", 1.0)))
    att = float(ev.get("attenuation", 1.0))
    if att == 0:
        return vol, vol
    sc = 1.0 - float(ev.get("dist", 0.0)) * att / 1000.0
    pan = float(ev.get("pan") or 0.0)
    return (min(1.0, max(0.0, vol * sc * (1 - pan))), min(1.0, max(0.0, vol * sc * (1 + pan))))


def expand_events(film: Film, sid: str, o: dict, where: str, text: str) -> list[dict]:
    """A shot's event log. The film tool's logs give `t` on the mp4's clock (handles included),
    `sample`, `volume`, `attenuation`, `dist`, `pan` (or `left`/`right`), `entity` and `channel`.
    A sound cuts off the one before it on the same entity and channel, as id's mixer does."""
    p = events_file(film, sid, o)
    j = json.loads(p.read_text())
    evs = j["events"] if isinstance(j, dict) else j
    meta = j if isinstance(j, dict) else {}
    base = meta.get("time_base") or ("file" if ("frames" in meta or "first" in meta) else "shot")
    if base not in ("shot", "file"):
        raise CueError(f"{p.name}: time_base '{base}' (the renderer reads 'shot' or 'file' seconds)")
    src = film.shot(sid)["source"]
    probe = src.get("probe") or {}
    if meta.get("frames") and probe.get("duration") and probe.get("fps"):
        tl_frames = round(probe["duration"] * probe["fps"])
        if abs(tl_frames - meta["frames"]) > 1:
            raise CueError(f"{p.name} is {meta['frames']} frames, {src['path']} is {tl_frames}: another take")
    kinds = set((o.get("kinds") or "sound").split(","))
    maps = {k.split(".", 1)[1]: v for k, v in o.items() if k.startswith("map.")}
    kinds |= set(maps)
    only = [x for x in (o.get("only") or "").split(",") if x]
    a, b = film.window(sid)
    keep_handles = o.get("handles") in ("1", "true", "yes")
    cue_opts = {k: v for k, v in o.items() if k in CUE_KEYS and k != "lufs"}
    # each channel's next start, for the cut-offs
    snd_evs = sorted((e for e in evs if e.get("kind") == "sound"), key=lambda e: float(e.get("t", 0)))
    nxt = {}
    last = {}
    for e in reversed(snd_evs):
        key = (e.get("entity"), e.get("channel"))
        if e.get("channel"):
            nxt[id(e)] = last.get(key)
            last[key] = float(e["t"])
    out = []
    for ev in evs:
        kind = ev.get("kind", "sound")
        if kind not in kinds:
            continue
        t = ev.get("t", ev.get("shot_s"))
        if t is None:
            continue
        t_file = float(t)
        t = t_file - (film.head(sid) if base == "file" else 0.0)
        ft = film.clock(sid, t)
        if not keep_handles and not (a <= ft < b):
            continue
        oo = dict(cue_opts)
        if kind == "sound":
            snd = ev.get("sample") or ev.get("sound") or ev.get("name")
            if not snd or "misc/null" in snd:
                continue
            snd = snd.removeprefix("sound/")
            if only and not any(snd.startswith(x) for x in only):
                continue
            gl, gr = id_gains(ev)
            if max(gl, gr) < 0.03:
                continue                                 # too far away to hear: id's mixer drops it too
            oo["_lr"] = f"{gl:.4f},{gr:.4f}"
            if nxt.get(id(ev)) is not None and "dur" not in oo:
                oo["dur"] = f"{(nxt[id(ev)] - t_file) / float(src.get('speed') or 1.0):.4f}"
                oo.setdefault("fadeout", "0.005")
            snd = "id:" + snd
            oo["_event"] = "sound"
        else:
            snd = maps[kind]
            oo["_event"] = kind
        out.append({"where": where, "line": text, "when": f"{sid} event {kind} @{t:.3f}", "sound": snd,
                    "opts": oo, "t": ft, "shot": sid})
    if o.get("lufs") is not None and out:               # the layer's loudest event at this level
        ms = []
        for c in out:
            try:
                x, sync, info, _ = load_sound(c["sound"])
                y, _ = shape(x, sync, info, c["opts"])
                ms.append(sfxlib.momentary_max(y))
            except CueError:
                pass
        if ms:
            g = fnum(o, "lufs") - max(ms)
            for c in out:
                c["opts"] = dict(c["opts"], gain=f"{fnum(c['opts'], 'gain', 0.0) + g:.3f}")
    return out


# ---------------------------------------------------------------- the mix

def cut_silences(seg: np.ndarray, lo: int, silences: list[tuple], started: float | None = None) -> np.ndarray:
    """Zero seg (which sits at sample lo) inside each silence, with a 5 ms fade into it. With
    `started`, only silences that begin after it (a cue starting inside one is left alone)."""
    for t0, t1, _ in silences:
        if started is not None and started >= t0:
            continue
        a, b = int(round(t0 * SR)) - lo, int(round(t1 * SR)) - lo
        if b <= 0 or a >= seg.shape[1]:
            continue
        f = dsp.nsamp(0.005)
        fa = max(0, a - f)
        if a > 0:
            seg[:, fa:a] *= np.linspace(1, 0, a - fa)[None, :]
        seg[:, max(0, a):max(0, min(b, seg.shape[1]))] = 0
    return seg


def render(film: Film, cues: list[dict], dry_run: bool = False, silences: list[tuple] = ()) -> tuple[dict, list[dict]]:
    n = film.n
    bus = {"dry": np.zeros((2, n)), "hall": np.zeros((2, n)), "room": np.zeros((2, n))} if not dry_run else None
    placed = []
    for c in sorted(cues, key=lambda c: c["t"]):
        rec = {"where": c["where"], "when": c["when"], "sound": c["sound"], "film_t": round(c["t"], 4),
               "shot": c["shot"] or film.shot_at(c["t"])}
        o = c["opts"]
        try:
            x, sync, info, name = load_sound(c["sound"])
            rec["sound"] = name
            y, sync = shape(x, sync, info, o)
        except CueError as e:
            rec["status"] = f"error: {e}"
            placed.append(rec)
            continue
        t0 = c["t"] - sync
        if o.get("avoid"):                         # avoid=WORDREF:SEC: nothing this near that word
            ref, _, near = o["avoid"].rpartition(":")
            try:
                ws = film.resolve(ref)[0][0]
                sid_ = film.shot_at(ws)
                we = next((w[1] for w in film.words if abs(w[0] - ws) < 1e-3), ws)
            except CueError as e:
                rec["status"] = f"error: {e}"
                placed.append(rec)
                continue
            if t0 < we + float(near) and t0 + 0.06 > ws - float(near):
                rec["status"] = f"skipped: within {near} s of {ref}"
                placed.append(rec)
                continue
        if o.get("underwords"):                    # underwords=DB: this much lower where it overlaps a word
            span = (t0, t0 + min(y.shape[1] / SR, 0.12))
            hit = [w for w in film.words if w[0] - 0.03 < span[1] and w[1] + 0.03 > span[0]]
            if hit:
                y = y * 10 ** (float(o["underwords"]) / 20)
                rec["under_word"] = hit[0][2]
        a = int(round(t0 * SR))
        rec.update(start_t=round(t0, 4), end_t=round(t0 + y.shape[1] / SR, 4), sample=a,
                   sync=round(sync, 4), gain_db=fnum(o, "gain", 0.0),
                   level_lufs=round(sfxlib.momentary_max(y), 1))
        warn = []
        if rec["shot"]:
            sa, sb = film.window(rec["shot"])
            outside = not (sa - 1e-6 <= c["t"] <= sb + 1e-6)
            if outside and c.get("clip"):                 # one entry of a list, in a trimmed handle
                rec["status"] = f"skipped: outside {rec['shot']}'s picture ({sa:.3f}-{sb:.3f})"
                placed.append(rec)
                continue
            if outside and "_event" not in o:
                warn.append(f"lands at {c['t']:.3f}, outside {rec['shot']}'s picture ({sa:.3f}-{sb:.3f})")
        # never double a sound the footage already carries
        if name.startswith("id:") and o.get("force") not in ("1", "true", "yes"):
            looped = "loop_start" in info
            if (o.get("_event") == "sound" or looped) and rec["shot"] and film.carries_sound(rec["shot"]):
                rec["status"] = (f"skipped: {rec['shot']}'s footage carries its own sound"
                                 + (" (and its ambient loops); force=1 places it anyway" if looped else ""))
                placed.append(rec)
                continue
            r_best, who = 0.0, None
            for sid in {s for s in (rec["shot"], film.shot_at(t0), film.shot_at(t0 + 0.1)) if s}:
                g = film.game_audio(sid)
                if g is None:
                    continue
                game = g[0].mean(0)
                at = film.wav_sample(sid, t0)
                if abs(fnum(o, "rate", 1.0) * 2 ** (fnum(o, "pitch", 0.0) / 12) - 1) > 1e-3 or o.get("rev"):
                    continue                                    # a changed sound is not the footage's sound
                r, dt = sfxlib.find_near(game, at - dsp.nsamp(fnum(o, "start", 0.0)), x.mean(0))
                if r > r_best:
                    r_best, who = r, (sid, dt)
            if who:
                rec["match_in_game"] = {"shot": who[0], "r": round(r_best, 3), "dt": round(who[1], 3)}
            if r_best >= sfxlib.MATCH_R:
                rec["status"] = (f"skipped: already in {who[0]}'s own sound (r={r_best:.2f}, "
                                 f"{who[1]:+.3f} s from the cue); force=1 places it anyway")
                placed.append(rec)
                continue
        if a + y.shape[1] <= 0 or a >= n:
            rec["status"] = "skipped: outside the film"
            placed.append(rec)
            continue
        if a < 0:
            warn.append(f"starts {-a / SR:.3f} s before the film; cut")
        rec["status"] = "placed"
        if warn:
            rec["warnings"] = warn
        if bus is not None:
            lo, hi = max(0, a), min(n, a + y.shape[1])
            seg = cut_silences(y[:, lo - a:hi - a].copy(), lo, list(silences), started=t0)
            bus["dry"][:, lo:hi] += seg
            for k in ("reverb", "room"):
                v = fnum(o, k, 0.0)
                if v:
                    bus["hall" if k == "reverb" else "room"][:, lo:hi] += v * seg
        placed.append(rec)
    return bus, placed


def master(bus: dict, args, silences: list[tuple] = ()) -> tuple[np.ndarray, dict]:
    out = bus["dry"]
    for k, (t60, damp, pre, seed) in {"hall": (2.6, 0.35, 0.025, 71), "room": (0.7, 0.5, 0.008, 72)}.items():
        if np.any(bus[k]):
            ir = dsp.make_ir(t60=t60, damp=damp, predelay=pre, seed=seed)
            wet = np.vstack([signal.oaconvolve(0.7 * bus[k][0] + 0.3 * bus[k][1], ir[0])[:out.shape[1]],
                             signal.oaconvolve(0.3 * bus[k][0] + 0.7 * bus[k][1], ir[1])[:out.shape[1]]])
            out = out + cut_silences(wet, 0, list(silences))
    pre_i, _, _ = dsp.loudness(out)
    if args.master is not None:
        g = args.master
    elif np.isfinite(pre_i):
        g = args.lufs - pre_i
    else:
        g = 0.0
    out = out * 10 ** (g / 20)
    out, gr = dsp.limit(out, args.ceiling - 0.1, lookahead_ms=1.5, release_ms=120.0)
    tp = dsp.true_peak(out)
    if tp > args.ceiling:                                     # a click the limiter can't hold
        out = out * 10 ** ((args.ceiling - 0.05 - tp) / 20)
    i, lra, st = dsp.loudness(out)
    rep = {"master_gain_db": round(g, 2), "loudness_before_db": round(pre_i, 2) if np.isfinite(pre_i) else None,
           "integrated_lufs": round(i, 2) if np.isfinite(i) else None, "lra_lu": round(lra, 2),
           "max_short_term_lufs": round(st, 2) if np.isfinite(st) else None, "true_peak_dbtp": round(dsp.true_peak(out), 2),
           "sample_peak_dbfs": round(float(dsp.to_db(np.abs(out).max())), 2), "limiter_max_gr_db": round(gr, 2),
           "clipped_samples": int(np.sum(np.abs(out) >= 1.0))}
    return out, rep


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cues", nargs="*", type=Path, default=[HERE / "cues-v7.md"],
                    help="cue lists (default: cues-v7.md beside this file)")
    ap.add_argument("-o", "--out", type=Path, default=FILM / "sound" / "sfx-v7.wav",
                    help="the stem (default FILM_ROOT/sound/sfx-v7.wav); its .json goes beside it")
    ap.add_argument("--timeline", type=Path, default=FILM / "edit" / "v7" / "timeline.json",
                    help="the edit's timeline (default FILM_ROOT/edit/v7/timeline.json); its clock.json "
                         "and ladder-events.json are read from beside it")
    ap.add_argument("--master", type=float, default=None,
                    help=f"a fixed master gain in dB (default {MASTER_DB}, the film's: about -20 LUFS)")
    ap.add_argument("--lufs", type=float, default=None, help="normalise the stem to this integrated loudness instead")
    ap.add_argument("--ceiling", type=float, default=-1.0, help="true-peak ceiling, dBTP")
    ap.add_argument("--list", action="store_true", help="resolve and print the cues; render nothing")
    ap.add_argument("--preview", type=Path, help="mix this audio (the edit's film audio) with the stem to an mp3 (ffmpeg)")
    ap.add_argument("--only-name", help="render only the cues tagged name=THIS (e.g. the switch clicks, to measure them)")
    args = ap.parse_args()
    if args.master is None and args.lufs is None:
        args.master = MASTER_DB
    if not args.timeline.exists():
        sys.exit(f"no timeline at {args.timeline}: the edit's timeline step writes it")

    film = Film(args.timeline.absolute())
    lines = [ln for p in args.cues for ln in read_cue_lines(p.absolute())]
    cues, errors, silences = expand(film, lines)
    if args.only_name:
        cues = [c for c in cues if c["opts"].get("name") == args.only_name]
    bus, placed = render(film, cues, dry_run=args.list, silences=silences)
    for t0, t1, where in silences:
        print(f"silence {t0:.3f}-{t1:.3f} ({where})", file=sys.stderr)
    for e in errors:
        print(f"ERROR {e['where']}: {e['error']}\n      {e['line']}", file=sys.stderr)
    for r in placed:
        mm, ss = divmod(r["film_t"], 60)
        st = r.get("status", "")
        flag = "" if st == "placed" else f"  [{st}]"
        print(f"{int(mm)}:{ss:06.3f}  {r['shot'] or '-':5s} {r['sound']:28s} {r['when']}{flag}"
              + "".join(f"\n      warning: {w}" for w in r.get("warnings", [])))
    n_ok = sum(r.get("status") == "placed" for r in placed)
    print(f"{n_ok} placed, {len(placed) - n_ok} skipped or failed, {len(errors)} line errors", file=sys.stderr)
    if args.list:
        return
    y, rep = master(bus, args, silences)
    out = args.out.absolute()
    out.parent.mkdir(parents=True, exist_ok=True)
    dsp.write_wav24(out, y, dither=False)          # undithered: a silence is exact zeros (24-bit needs no dither here)
    meta = {"wav": out.name, "timeline": str(film.path.relative_to(FILM)) if film.path.is_relative_to(FILM) else str(film.path),
            "cue_files": [p.name for p in args.cues], "duration_s": film.duration, "samples": film.n,
            "sample_rate": SR, "target_lufs": None if args.master is not None else args.lufs,
            "clock": str(film.clock_path.relative_to(FILM)) if film.clock_path and film.clock_path.is_relative_to(FILM) else None,
            "silences": [{"from": round(a, 4), "to": round(b, 4), "where": w} for a, b, w in silences],
            **rep, "errors": errors, "cues": placed}
    out.with_suffix(".json").write_text(json.dumps(meta, indent=1) + "\n")
    print(f"{out}: {film.duration:.3f} s, I {rep['integrated_lufs']} LUFS, TP {rep['true_peak_dbtp']} dBTP, "
          f"gain {rep['master_gain_db']} dB, limiter {rep['limiter_max_gr_db']} dB", file=sys.stderr)
    if args.preview:
        mp3 = out.with_name(out.stem + "-preview.mp3")
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(args.preview.absolute()), "-i", str(out),
                        "-filter_complex", "[0:a][1:a]amix=inputs=2:normalize=0[a]", "-map", "[a]",
                        "-b:a", "192k", str(mp3)], check=True)
        print(mp3, file=sys.stderr)


if __name__ == "__main__":
    main()
