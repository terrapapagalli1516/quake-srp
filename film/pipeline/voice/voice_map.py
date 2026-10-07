#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Write film/voice.json, the narration's map: every line of the cut in order, with its clip and its words' times,
in one self-contained file (the edit's clock reads it; the clips themselves are not in the repository).

    uv run film/pipeline/voice/voice_map.py VOICE_DIR [-o film/voice.json]

VOICE_DIR is the production's voice folder: the line map (`v7-lines.json`: each line's clip, length, text and pause)
and the word times Scribe heard in each clip (`clips*.json`). Each line keeps the fields the clock reads: its clip
file (relative to VOICE_DIR), length, text, the script's pause after it, the clip's own pause and paragraph end, and
the words. Nothing else (the takes' checks, prompts and seeds) is copied.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("voice", type=Path, help="the production's voice folder (v7-lines.json, clips*.json, the clips)")
ap.add_argument("--lines", default="v7-lines.json", help="the line map in it (default v7-lines.json)")
ap.add_argument("-o", "--out", type=Path, default=Path(__file__).resolve().parents[2] / "voice.json")
a = ap.parse_args()

doc = json.loads((a.voice / a.lines).read_text())
clips = {}
for f in sorted(a.voice.glob("clips*.json")):
    for c in json.loads(f.read_text())["clips"]:
        clips[(f.name, c["id"])] = c
gap = json.loads((a.voice / "clips.json").read_text())["natural_sentence_gap_s"]
out = []
for ln in doc["lines"]:
    src = ln if ln.get("words") else clips[(ln["words_from"], ln["clip"])]
    if not (a.voice / ln["file"]).exists():
        raise SystemExit(f"{ln['id']}: {ln['file']} is not in {a.voice}")
    e = {"id": ln["id"], "kind": ln["kind"], "clip": ln["clip"], "file": ln["file"], "duration_s": ln["duration_s"],
         "text": ln["text"], "pause_after_s": ln.get("pause_after_s")}
    if ln.get("replaces"):
        e["replaces"] = ln["replaces"]
    if src is not ln:
        e["clip_pause_after_s"] = src.get("pause_after_s")
        e["clip_paragraph_end"] = bool(src.get("paragraph_end", True))
    e["speech_start_s"], e["speech_end_s"] = src["speech_start_s"], src["speech_end_s"]
    e["words"] = [{"text": w["text"], "start": w["start"], "end": w["end"]} for w in src["words"]]
    out.append(e)
m = {"about": "The narration's map: every line of the cut in order, its clip (relative to the voice folder, "
              "`make.py --voice`), length, text, pauses and each word's time in the clip (Scribe). Written by "
              "film/pipeline/voice/voice_map.py; the clips are not in the repository.",
     "natural_sentence_gap_s": gap, "lines": out}
a.out.write_text(json.dumps(m, indent=1, ensure_ascii=False) + "\n")
print(f"{a.out}: {len(out)} lines, {sum(len(e['words']) for e in out)} words")
