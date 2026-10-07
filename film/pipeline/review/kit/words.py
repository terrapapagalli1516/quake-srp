"""Words against picture: Scribe's transcript of the cut (ElevenLabs' speech-to-text), the words heard in each
shot, the transcript against the script (missing, extra, misheard, a cut-off word heard whole), each word's
time against the timeline's, and the voice's own cut marks against the cuts.

Scribe needs an ElevenLabs API key in the environment variable ELEVENLABS_API_KEY; the kit reads it only
here, for this part, and never prints it. Without it (and with no transcript of this cut's sound in the
cache) the words part says so and lists no words, and the sound and sync parts run without them."""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
import statistics
import subprocess
from pathlib import Path

from common import CACHE, FILM, Flags, ensure_dir, log, md_table, rel, tc

KEY_ENV = "ELEVENLABS_API_KEY"
CUT = "✂"   # marks a scripted word that is cut off mid-way ("interme—")


# ------------------------------------------------------------------ Scribe ----

def scribe(cut: Path, out: Path) -> tuple[dict | None, str]:
    """Scribe on the cut's own audio (AAC copied out, not re-encoded), with word times and sound events
    tagged. Cached by the audio's bytes, so a re-run of the same cut asks Scribe nothing."""
    ensure_dir(CACHE / "scribe")
    audio = out / "cut-audio.m4a"
    subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(cut), "-vn", "-c:a", "copy", str(audio)], check=True)
    h = hashlib.sha256(audio.read_bytes()).hexdigest()[:20]
    cp = CACHE / "scribe" / f"{h}.json"
    if cp.exists():
        return json.loads(cp.read_text()), f"Scribe (scribe_v1), cached `{cp}`"
    key = os.environ.get(KEY_ENV, "").strip()
    if not key:
        return None, f"no Scribe: {KEY_ENV} is not set"
    import httpx
    log("words: Scribe is listening to the cut")
    try:
        with open(audio, "rb") as f:
            r = httpx.post("https://api.elevenlabs.io/v1/speech-to-text", headers={"xi-api-key": key},
                           data={"model_id": "scribe_v1", "timestamps_granularity": "word", "language_code": "en",
                                 "tag_audio_events": "true"},
                           files={"file": (audio.name, f, "audio/mp4")}, timeout=900)
    except Exception as ex:  # noqa: BLE001  (never echo the request: it carries the key)
        return None, f"no Scribe: {type(ex).__name__}"
    finally:
        key = None  # noqa: F841
    if r.status_code != 200:
        return None, f"no Scribe: HTTP {r.status_code}"
    j = r.json()
    cp.write_text(json.dumps(j))
    return j, f"Scribe (scribe_v1, run now), cached `{cp}`"


def heard_words(sc: dict) -> tuple[list[dict], list[dict]]:
    words = [{"text": w["text"], "start": float(w["start"]), "end": float(w["end"]), "logprob": w.get("logprob")}
             for w in sc.get("words", []) if w.get("type", "word") == "word"]
    events = [{"text": w["text"], "start": float(w["start"]), "end": float(w["end"])}
              for w in sc.get("words", []) if w.get("type") == "audio_event"]
    return words, events


# ----------------------------------------------------------- normalising ----

_ONES = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen " \
        "seventeen eighteen nineteen".split()
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
    """'1996' -> 'nineteen ninety six' (years and '1080' are said in pairs), '6.7' -> 'six point seven'."""
    s = s.replace(",", "")
    if "." in s:
        a, b = s.split(".", 1)
        return _say(int(a or 0)) + " point " + " ".join(_ONES[int(c)] for c in b if c.isdigit())
    n = int(s)
    if 1000 <= n <= 2099 and n % 100 != 0 and not 2000 <= n <= 2009:
        return _say(n // 100) + " " + (_say(n % 100) if n % 100 >= 10 else "oh " + _say(n % 100))
    return _say(n)


def toks(text: str) -> list[str]:
    s = text.lower().replace("’", "'").replace("‘", "'")
    s = re.sub(r"~~.*?~~", " ", s)
    s = re.sub(r"\[[^\]]*\]", " ", s)
    s = re.sub(r"(\w)[—–]-?(?=\s|$|[\"”])", r"\1" + CUT, s)      # a word cut off: "interme—"
    s = re.sub(r"\ba (hundred|thousand)\b", r"one \1", s)
    s = re.sub(r"(\d+(?:\.\d+)?)\s*%", r"\1 percent", s)
    s = re.sub(r"(\d)x(\d)", r"\1 by \2", s)
    s = re.sub(r"\d[\d,]*(?:\.\d+)?", lambda m: " " + say_number(m.group()) + " ", s)
    s = re.sub(r"[-–—/+:;,.!?\"“”()*_]", " ", s)
    s = s.replace("'", "")
    return [w for w in s.split() if w]


# ------------------------------------------------------------- the script ----

def script_lines(path: Path) -> list[dict]:
    """The script's spoken lines in order: `**ID** · MARK · text` (Markdown; DROPPED lines and struck text
    left out), or a voice map's `lines` (JSON)."""
    if path.suffix == ".json":
        j = json.loads(path.read_text())
        return [{"id": x["id"], "text": x["text"]} for x in j.get("lines", []) if x.get("text")]
    out = []
    for ln in path.read_text().splitlines():
        m = re.match(r"^\*\*([A-Z0-9][\w.\-]*(?:,\s*[\w.\-]+)*)\*\*\s*·\s*([^·]+?)\s*·\s*(.+)$", ln)
        if not m:
            continue
        mark, text = m.group(2), m.group(3)
        if "DROPPED" in mark.upper():
            continue
        text = re.sub(r"<!--.*?-->", "", text)
        out.append({"id": m.group(1), "text": text.strip()})
    return out


def default_script(edit) -> Path | None:
    p = ((edit.config or {}).get("paths") or {}).get("script")
    if p and (FILM / p).exists():
        return FILM / p
    return None


# ------------------------------------------------------------------- diff ----

def diff_script(lines: list[dict], words: list[dict]) -> list[dict]:
    a, a_line = [], []
    for ln in lines:
        for t in toks(ln["text"]):
            a.append(t)
            a_line.append(ln["id"])
    b, b_w = [], []
    for w in words:
        for t in toks(w["text"]):
            b.append(t)
            b_w.append(w)
    a_plain = [t.rstrip(CUT) for t in a]
    sm = difflib.SequenceMatcher(None, a_plain, b, autojunk=False)
    out = []
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            continue
        sa, sb = a[i1:i2], b[j1:j2]
        line = a_line[i1] if i1 < len(a_line) else (a_line[-1] if a_line else "")
        t = b_w[j1]["start"] if j1 < len(b_w) and sb else (b_w[j1 - 1]["end"] if j1 > 0 and j1 - 1 < len(b_w) else None)
        rec = {"op": op, "script": " ".join(sa), "heard": " ".join(sb), "line": line, "t": t,
               "context": " ".join(a_plain[max(0, i1 - 3):i1]) + " [" + " ".join(sa) + "] " + " ".join(a_plain[i2:i2 + 2])}
        ja, jb = "".join(x.rstrip(CUT) for x in sa), "".join(sb)
        if op == "replace" and ja == jb:
            rec["kind"] = "spelling"
        elif sa and sa[-1].endswith(CUT) and sb and sb[-1].startswith(sa[-1].rstrip(CUT)) and len(sb[-1]) > len(sa[-1].rstrip(CUT)):
            rec["kind"] = "cut-off word heard whole"
        elif op == "replace" and difflib.SequenceMatcher(None, ja, jb).ratio() >= 0.75:
            rec["kind"] = "near (spelling or a sound)"
        elif op == "replace":
            rec["kind"] = "misheard"
        elif op == "delete":
            rec["kind"] = "missing"
        else:
            rec["kind"] = "extra"
        out.append(rec)
    return sorted(out, key=lambda r: (r["t"] is None, r["t"] or 0))


# ------------------------------------------------- planned against heard ----

def planned_words(edit) -> list[dict]:
    out = []
    for s in edit.shots:
        for w in s.get("words", []):
            out.append({"w": w["w"], "t": s["start"] + float(w["t"]), "e": s["start"] + float(w.get("e", w["t"])), "shot": s["id"]})
    return sorted(out, key=lambda x: x["t"])


def offsets(edit, words: list[dict]) -> list[dict]:
    pw = planned_words(edit)
    a = [toks(x["w"])[0] if toks(x["w"]) else x["w"] for x in pw]
    b, bw = [], []
    for w in words:
        tt = toks(w["text"])
        if tt:
            b.append(tt[0])
            bw.append(w)
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    out = []
    for bl in sm.get_matching_blocks():
        for k in range(bl.size):
            p, h = pw[bl.a + k], bw[bl.b + k]
            out.append({"w": p["w"], "planned": p["t"], "planned_end": p["e"], "heard": h["start"], "heard_end": h["end"],
                        "d": h["start"] - p["t"], "shot": p["shot"]})
    return out


def voice_cut_marks(edit, words: list[dict], fl: Flags) -> list[dict]:
    """A clip with its own cut mark (`cut_at_s` in voice/clips*.json, a line cut off mid-word): where the mark
    lands on the film against the cut, and where the clip really stops."""
    marks = {}
    for p in sorted((FILM / "voice").glob("clips*.json")):
        try:
            for c in json.loads(p.read_text()).get("clips", []):
                if c.get("cut_at_s") is not None:
                    marks[c["id"]] = (float(c["cut_at_s"]), rel(p), c.get("cut_note", ""))
        except Exception:  # noqa: BLE001
            continue
    out = []
    cuts = edit.cuts()
    for v in (edit.timeline or {}).get("voice", []):
        cid = v["id"]
        if cid not in marks and not v.get("hard_end"):
            continue
        at = float(v["at"])
        mark = at + marks[cid][0] if cid in marks else None
        stop = float(v["speech"][1]) if v.get("hard_end") else at + float(v.get("duration", 0))
        ref = mark if mark is not None else stop
        c = min(cuts, key=lambda x: abs(x - ref)) if cuts else None
        last = [w for w in words if w["start"] < stop + 0.05 and w["end"] > at] if words else []
        lw = last[-1] if last else None
        rec = {"clip": cid, "mark": mark, "stops": stop, "cut": c, "into": edit.shot_id_at(c + 0.5 / edit.fps) if c is not None else None,
               "src": marks.get(cid, (None, None, ""))[1], "heard_last": lw["text"] if lw else None,
               "heard_end": lw["end"] if lw else None}
        out.append(rec)
        if mark is not None and c is not None and abs(c - mark) > 0.5 / edit.fps:
            fl.add("fix", c, f"the voice clip {cid} is marked to be cut at {marks[cid][0]:.2f} s into it (film {tc(mark, 3)}), "
                   f"but the picture cuts to {rec['into']} at {tc(c, 3)}: {c - mark:+.3f} s"
                   + (f"; the clip plays on to {tc(stop, 3)}" if stop > mark + 0.01 else "")
                   + (f"; Scribe hears \"{lw['text']}\" end at {tc(lw['end'], 3)}" if lw else ""), "words.md")
    return out


# ----------------------------------------------------------------- report ----

def words_md(edit, sc_how: str, words: list[dict], events: list[dict], lines: list[dict], script_path: Path | None,
             diffs: list[dict], offs: list[dict], marks: list[dict], fl: Flags) -> str:
    L = ["# Words against picture", ""]
    L.append(f"**Ear:** {sc_how}. {len(words)} words heard" + (f", {len(events)} sound events tagged" if events else "") + ".")
    L.append("")
    # the diff
    L.append("## The transcript against the script")
    L.append("")
    if script_path is None:
        L.append("No script given or found (`--script`).")
    else:
        n_script = sum(len(toks(x["text"])) for x in lines)
        real = [d for d in diffs if d["kind"] not in ("spelling",)]
        L.append(f"Script: `{rel(script_path)}`, {len(lines)} lines, {n_script} words (numbers said as words; hyphens split; "
                 f"\"interme—\" is a word cut off). **{len(real)} differences** beyond spelling, {len(diffs) - len(real)} spelling only.")
        L.append("")
        rows = [[tc(d["t"], 2), d["line"], d["kind"] if d["kind"] == "spelling" else f"**{d['kind']}**", d["script"] or "-",
                 d["heard"] or "-", d["context"]] for d in diffs]
        L.append(md_table(["heard at", "line", "kind", "script", "heard", "in context"], rows) if rows else "Every word heard, in order.")
    L.append("")
    # word times
    if offs:
        ds = [o["d"] for o in offs]
        late = [o for o in offs if abs(o["d"]) > 0.15]
        L.append("## Each word's time against the timeline's")
        L.append("")
        L.append(f"{len(offs)} words matched to the timeline's own word times: median {statistics.median(ds) * 1000:+.0f} ms, "
                 f"worst {max(ds, key=abs) * 1000:+.0f} ms; {len(late)} off by more than 150 ms.")
        if late:
            L.append("")
            L.append(md_table(["word", "shot", "planned", "heard", "off"],
                              [[o["w"], o["shot"], tc(o["planned"], 3), tc(o["heard"], 3), f"{o['d'] * 1000:+.0f} ms"] for o in late[:40]]))
        L.append("")
    if marks:
        L.append("## Lines cut off on purpose")
        L.append("")
        rows = [[m["clip"], tc(m["mark"], 3) if m["mark"] is not None else "-", tc(m["stops"], 3), tc(m["cut"], 3), m["into"] or "-",
                 f"{(m['cut'] - m['mark']):+.3f} s" if m["mark"] is not None and m["cut"] is not None else "-",
                 f"\"{m['heard_last']}\" ends {tc(m['heard_end'], 3)}" if m["heard_last"] else "-"] for m in marks]
        L.append(md_table(["clip", "its cut mark", "clip stops", "the cut", "into", "cut - mark", "Scribe's last word"], rows))
        L.append("")
    # per shot
    L.append("## Words in each shot")
    L.append("")
    L.append("Each heard word goes to the shot its middle falls in; `|` marks a word that runs across the cut out of the shot. "
             "Low-confidence words (Scribe's logprob under -1) are in italics.")
    L.append("")
    rows = []
    for s in edit.shots:
        a, b = s["start"], s["start"] + s["len"]
        ws = [w for w in words if a <= (w["start"] + w["end"]) / 2 < b]
        txt = []
        for w in ws:
            x = w["text"]
            if w.get("logprob") is not None and w["logprob"] < -1.0:
                x = f"*{x}*"
            if w["end"] > b + 0.02:
                x += " |"
            txt.append(x)
        ev = [e["text"] for e in events if a <= (e["start"] + e["end"]) / 2 < b]
        note = []
        if not ws and s["len"] >= 1.5:
            note.append("no voice")
        if ev:
            note.append("sounds: " + ", ".join(ev))
        rows.append([s["id"], f"{tc(a)}–{tc(b)}", (tc(ws[0]["start"]) if ws else "-"), " ".join(txt) or "-", "; ".join(note)])
    L.append(md_table(["shot", "film time", "first word", "heard", "note"], rows))
    L.append("")
    return "\n".join(L)


def word_flags(edit, words: list[dict], events: list[dict], diffs: list[dict], fl: Flags) -> None:
    by_line: dict[str, list[dict]] = {}
    for d in diffs:
        if d["kind"] != "spelling":
            by_line.setdefault(d["line"], []).append(d)
    order = {"fix": 0, "look": 1, "info": 2}
    for line, ds in by_line.items():
        sevs = ["fix" if d["kind"] in ("missing", "misheard", "cut-off word heard whole") else
                ("info" if d["kind"].startswith("near") else "look") for d in ds]
        sev = min(sevs, key=order.get)
        what = "; ".join(f"{d['kind']}: script \"{d['script'] or '-'}\", heard \"{d['heard'] or '-'}\"" for d in ds)
        fl.add(sev, ds[0]["t"], f"line {line}: {what}", "words.md")
    for w in words:
        if w.get("logprob") is not None and w["logprob"] < -2.0:
            fl.add("look", w["start"], f"Scribe is unsure of \"{w['text']}\" (logprob {w['logprob']:.1f}): masked or unclear?", "words.md")
    for e in events:
        fl.add("info", e["start"], f"Scribe tags a sound: {e['text']}", "words.md", t1=e["end"])
