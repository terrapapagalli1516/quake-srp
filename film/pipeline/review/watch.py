#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy", "scipy", "pillow", "matplotlib", "soundfile", "httpx"]
# ///
"""watch.py: the viewing kit. Watch a cut the way a careful editor would and write one folder with an
index.md linking everything: contact sheets per section (a row per shot), an overview sheet every 0.5 s,
full-rate filmstrips of chosen ranges, the words against the picture (ElevenLabs Scribe), the sound
(loudness, buses, flags), the picture's flags (black, frozen, flat, flashes, text at phone size, the safe
area), a sync table, and the checks of the cut against its own sources. A whole run takes minutes.

    uv run film/pipeline/review/watch.py edit/v7/build-004/film.mp4
    uv run film/pipeline/review/watch.py CUT --range 1:43.0-1:45.0              # plus every frame of a range
    uv run film/pipeline/review/watch.py CUT --out DIR --only words             # re-run one part into DIR

A relative path (the cut, --build, --timeline, --clock, --config, --script) is looked up in the current
folder, then under FILM_ROOT (filmroot.py). The report goes to a fresh folder under FILM_SCRATCH/watch/runs/
unless --out names one; the kit writes nothing else but its cache (FILM_SCRATCH/watch/cache/, or WATCH_CACHE).

Needs ffmpeg and ffprobe. The words part needs ELEVENLABS_API_KEY (Scribe, ElevenLabs' speech-to-text);
without it, and with no transcript of this cut's sound cached, the words are left out and the other parts
run without them.

Parts (all by default; --only/--skip a comma list): shots, overview, picture, words, sound, sync, range
(range runs whenever --range is given), and the checks: ghosts, overtext, shape, stops, readtime, textsize.
See kit/README.md.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
KIT = HERE / "kit"
sys.path.insert(0, str(HERE.parent))   # film/pipeline: filmroot
sys.path.insert(0, str(KIT))

import numpy as np  # noqa: E402

import common  # noqa: E402
from common import FILM, Flags, ensure_dir, fresh_run_dir, locate, log, md_table, rel, tc  # noqa: E402
from filmroot import SCRATCH  # noqa: E402

PARTS = ["shots", "overview", "picture", "words", "sound", "sync", "range",
         "ghosts", "overtext", "shape", "stops", "readtime", "textsize"]
SEV_ORDER = {"fix": 0, "look": 1, "info": 2}


def main() -> None:
    ap = argparse.ArgumentParser(description="the viewing kit: watch a cut and write a report folder (this takes minutes)")
    ap.add_argument("cut", help="the film file, e.g. edit/v7/build-004/film.mp4 (relative: the current folder, then FILM_ROOT)")
    ap.add_argument("--out", help=f"the report folder (default: a fresh one under {SCRATCH / 'watch' / 'runs'}); "
                    "an existing one is reused, its caches kept")
    ap.add_argument("--only", help="comma list of parts to run: " + ",".join(PARTS))
    ap.add_argument("--skip", help="comma list of parts to leave out")
    ap.add_argument("--range", action="append", default=[], help="A-B in film time (1:17.2-1:24.1): a filmstrip of every frame; repeatable")
    ap.add_argument("--every", type=int, default=1, help="filmstrips: every Nth frame (default 1)")
    ap.add_argument("--tile", type=int, default=320, help="filmstrips: tile width in pixels (default 320)")
    ap.add_argument("--overview-every", type=float, default=0.5, help="overview sheets: a frame every S seconds (default 0.5)")
    ap.add_argument("--timeline", help="the edit's timeline.json (default: the build that made the file)")
    ap.add_argument("--build", help="the build folder that made the file (default: found by the file's bytes)")
    ap.add_argument("--clock", help="the edit's clock.json (default: the build's, if it matches)")
    ap.add_argument("--config", help="the edit's edit.toml (default: beside the build)")
    ap.add_argument("--script", help="the script to check the words against: a script .md or a voice map .json "
                    "(default: the config's paths.script)")
    ap.add_argument("--silence", type=float, default=0.3, help="flag unmarked silences longer than this (s)")
    ap.add_argument("--no-derive-stems", action="store_true", help="do not re-make the buses with build.py's mixer")
    ap.add_argument("--workers", type=int, default=4, help="parallel decoders for the picture pass")
    ap.add_argument("--check-workers", type=int, default=4, help="processes for the frame-pair and text checks (each)")
    a = ap.parse_args()

    if common.env_niceness() == 0:
        try:
            os.nice(10)   # a long, CPU-heavy run: let other work go first
        except OSError:
            pass
    cut = film_path(a.cut)
    if not cut.exists():
        raise SystemExit(f"no such file: {a.cut}")
    for k in ("timeline", "build", "clock", "config", "script"):
        if getattr(a, k):
            setattr(a, k, str(film_path(getattr(a, k))))
    parts = set(PARTS) if not a.only else {p.strip() for p in a.only.split(",")}
    if a.skip:
        parts -= {p.strip() for p in a.skip.split(",")}
    if not a.range:
        parts.discard("range")
    elif a.only is None:
        parts.add("range")
    bad = parts - set(PARTS)
    if bad:
        raise SystemExit(f"unknown parts: {', '.join(sorted(bad))}")
    out = ensure_dir(common.absolute(a.out)) if a.out else fresh_run_dir(cut)
    t_start = time.time()
    edit = locate(cut, a.timeline, a.build, a.clock, a.config)
    log(f"{cut.name}: {edit.nframes} frames, {tc(edit.duration)}; build {rel(edit.build) if edit.build else '-'}; out {out}")
    for nline in edit.notes:
        log(nline)
    timings: dict[str, float] = {}

    # ---- Scribe (network), in the background
    words_state: dict = {}
    need_words = bool(parts & {"words", "sync", "sound"})
    th_words = None
    if need_words:
        def do_scribe():
            import words as W
            t0 = time.time()
            try:
                sc, how = W.scribe(edit.cut, out)
            except Exception as ex:  # noqa: BLE001
                sc, how = None, f"no Scribe: {type(ex).__name__}"
            words_state.update(sc=sc, how=how)
            if sc:
                words_state["words"], words_state["events"] = W.heard_words(sc)
            timings["scribe"] = time.time() - t0
        th_words = threading.Thread(target=do_scribe)
        th_words.start()

    # ---- the cut against its sources (ghosts, overtext, shape) and the film's text (readtime, textsize):
    # processes of their own, in the background
    pair_state: dict = {}
    th_pairs = None
    if parts & {"ghosts", "overtext", "shape"}:
        def do_pairs():
            import layers as LY
            t0 = time.time()
            try:
                pair_state["pairs"] = LY.all_pairs(edit, out, bool(parts & {"ghosts", "overtext"}), "shape" in parts,
                                                   a.check_workers)
            except Exception as ex:  # noqa: BLE001
                traceback.print_exc()
                pair_state["error"] = f"{type(ex).__name__}: {ex}"
            timings["frame pairs"] = time.time() - t0
        th_pairs = threading.Thread(target=do_pairs)
        th_pairs.start()
    text_state: dict = {}
    th_texts = None
    if parts & {"readtime", "textsize"}:
        def do_texts():
            import texts as TX
            t0 = time.time()
            try:
                text_state["timeline"] = TX.timeline_items(edit)
                text_state["measured"] = TX.measured_items(edit, out, max(2, a.check_workers - 1))
                text_state["ladder"] = TX.ladder_sizes(edit) if "textsize" in parts else []
            except Exception as ex:  # noqa: BLE001
                traceback.print_exc()
                text_state["error"] = f"{type(ex).__name__}: {ex}"
            timings["text tracks"] = time.time() - t0
        th_texts = threading.Thread(target=do_texts)
        th_texts.start()

    # ---- the sound (CPU), in the background while the picture decodes: audio, buses, checks, plots
    sound_state: dict = {}
    need_sound = bool(parts & {"sound", "sync", "range", "stops"})
    th_sound = None
    if need_sound:
        def do_sound():
            import sound as S
            try:
                t0 = time.time()
                mix = S.decode_audio(edit.cut)
                derive = (not a.no_derive_stems) and bool(parts & {"sound", "sync", "stops"})
                st, how = S.stems_for(edit, out, derive)
                sound_state.update(mix=mix, st=st, how=how)
                timings["audio + buses"] = time.time() - t0
                if "stops" in parts:
                    import stops as ST
                    t0 = time.time()
                    fl = Flags("stops")
                    (out / "stops.md").write_text(ST.check(edit, st, mix, out, fl) + flags_md(fl.items))
                    fl.save(out)
                    timings["stops"] = time.time() - t0
                if not parts & {"sound", "sync"}:
                    return
                if th_words is not None:
                    th_words.join()
                t0 = time.time()
                fl = Flags("sound")
                res = S.analyse(edit, out, mix, st, how, words_state.get("words"), fl, a.silence)
                sound_state["res"] = res
                timings["sound checks"] = time.time() - t0
                if "sound" in parts:
                    t0 = time.time()
                    plots = S.plots(edit, res, out, fl.items)
                    (out / "sound.md").write_text(S.sound_md(edit, res, plots) + flags_md(fl.items))
                    slim = {k: v for k, v in res.items() if k not in ("curves", "onsets")}
                    (out / "sound.json").write_text(json.dumps(slim, indent=1, default=float))
                    fl.save(out)
                    timings["sound plots"] = time.time() - t0
            except Exception as ex:  # noqa: BLE001
                traceback.print_exc()
                sound_state["error"] = f"{type(ex).__name__}: {ex}"
        th_sound = threading.Thread(target=do_sound)
        th_sound.start()

    # ---- the picture: one decode for the sheets and the flags
    import picture as P
    frames_dir = out / "frames"
    dec = None
    moments = P.text_moments(edit) if "picture" in parts else []
    if parts & {"shots", "overview", "picture"}:
        want: set[int] = set()
        if "shots" in parts:
            for s in edit.shots:
                want |= set(P.shot_frames(s))
        if "overview" in parts:
            want |= set(P.overview_frames(edit.nframes, edit.fps, a.overview_every))
        want |= {m["frame"] for m in moments}
        sig = {"size": edit.info["size"], "mtime": int(cut.stat().st_mtime), "frames": edit.nframes, "stats": len(P.STAT_KEYS)}
        sp = out / "picture-stats.npz"
        have_frames = all(P.frame_path(frames_dir, i).exists() for i in want)
        cached = None
        if sp.exists():
            z = np.load(sp, allow_pickle=True)
            if json.loads(str(z["sig"])) == sig:
                cached = {"stats": z["stats"], "tiny": z["tiny"], "seen": z["seen"]}
        if cached is not None and have_frames:
            dec = cached
            log("picture: statistics and frames from the last run")
        elif "picture" in parts or not have_frames:
            t0 = time.time()
            log(f"picture: decoding {edit.nframes} frames ({len(want)} kept)")
            dec = P.decode_pass(edit.cut, edit.nframes, edit.fps, want, frames_dir, a.workers)
            np.savez_compressed(sp, stats=dec["stats"], tiny=dec["tiny"], seen=dec["seen"], sig=json.dumps(sig))
            timings["picture decode"] = time.time() - t0
            log(f"picture: decoded in {timings['picture decode']:.0f} s")
    if "shots" in parts and edit.shots:
        t0 = time.time()
        sheets = P.shot_sheets(edit, frames_dir, out)
        (out / "shots.md").write_text("# Shots\n\nOne row per shot: its first, middle and last frame, with frame numbers and film "
                                      "times; the planned narration in blue.\n\n" + "\n".join(f"- [{p.name}]({p.name})" for p in sheets) + "\n")
        timings["shot sheets"] = time.time() - t0
    if "overview" in parts:
        t0 = time.time()
        ov = P.overview_sheets(edit, frames_dir, out, a.overview_every)
        (out / "overview.md").write_text(f"# Overview\n\nA frame every {a.overview_every:g} s; a red bar marks a tile after a cut.\n\n" +
                                         "\n".join(f"- [{p.name}]({p.name})" for p in ov) + "\n")
        timings["overview sheets"] = time.time() - t0
    if "picture" in parts and dec is not None:
        t0 = time.time()
        fl = Flags("picture")
        pf = P.picture_flags(edit, dec, fl)
        with ThreadPoolExecutor(6) as ex:
            measured = list(ex.map(lambda m: P.measure_text(m) if m.get("measure") else {}, moments))
        for m, me in zip(moments, measured):     # text size is flagged by the textsize check; here, the table
            scaled = m.get("cap") is not None           # the edit's own captions: size known from their scale
            m["cap_from"] = "scale" if scaled else ("measured" if me.get("cap_median") is not None else None)
            if not scaled and me.get("cap_median") is not None:
                m["cap"] = me["cap_median"]
            boxes = ([m["box"]] if m.get("box") else []) + (me.get("boxes") or [])
            if any(P.outside_safe(bx) for bx in boxes):
                fl.add("info", m["t"], f"{m['shot']}: text outside the 90% graphics-safe area ({m['what'][:60]})", "picture.md")
        planned_text(edit, fl)
        phone = P.phone_sheets(edit, frames_dir, out, moments)
        (out / "picture.md").write_text(P.picture_md(edit, pf, moments, measured, phone, out) + flags_md(fl.items))
        fl.save(out)
        timings["picture checks + phone sheets"] = time.time() - t0

    # ---- words
    if need_words:
        th_words.join()
        if "words" in parts:
            import words as W
            words, events = words_state.get("words"), words_state.get("events", [])
            fl = Flags("words")
            spath = common.absolute(a.script) if a.script else W.default_script(edit)
            lines = W.script_lines(spath) if spath else []
            diffs = W.diff_script(lines, words) if (words and lines) else []
            offs = W.offsets(edit, words) if words else []
            marks = W.voice_cut_marks(edit, words or [], fl)
            if words:
                W.word_flags(edit, words, events, diffs, fl)
            (out / "words.md").write_text(W.words_md(edit, words_state.get("how", ""), words or [], events, lines, spath, diffs,
                                                     offs, marks, fl) + flags_md(fl.items))
            (out / "words.json").write_text(json.dumps({"words": words, "events": events, "diffs": diffs, "marks": marks}, indent=1,
                                                       default=float))
            fl.save(out)

    # ---- sync
    if need_sound:
        th_sound.join()
        if sound_state.get("error"):
            log("sound failed:", sound_state["error"])
    res = sound_state.get("res")
    if "sync" in parts and res is not None:
        import sound as S
        import sync as Y
        t0 = time.time()
        fl = Flags("sync")
        sfx_meta, sfx_how = S.find_sfx_meta(edit)
        words = words_state.get("words")
        ev = Y.events_table(edit, res, words)
        cues, relist_how = Y.cue_table(edit, res, sound_state.get("st") or {}, sfx_meta, words, fl, out)
        foot, per_shot = Y.footage_table(edit, res, fl)
        score = Y.score_table(edit, res, fl)
        (out / "sync.md").write_text(Y.sync_md(edit, ev, cues, foot, per_shot, out, sfx_how, bool(sound_state.get("st")),
                                               relist_how, score, res.get("score_meta_path", "")) + flags_md(fl.items))
        fl.save(out)
        timings["sync"] = time.time() - t0

    # ---- the checks on the cut against its sources
    if th_pairs is not None:
        th_pairs.join()
        pairs = pair_state.get("pairs") or []
        if pair_state.get("error"):
            log("frame pairs failed:", pair_state["error"])
        errs = [r for r in pairs if r.get("error")]
        for r in errs[:3]:
            log("frame pairs: a shot failed:", r["error"], r.get("trace", "")[-300:])
        import ghosts as GH
        import overtext as OT
        import shape as SH
        for part, mod in (("ghosts", GH), ("overtext", OT), ("shape", SH)):
            if part in parts:
                fl = Flags(part)
                if pair_state.get("error"):
                    fl.add("look", None, f"the {part} check failed: {pair_state['error']}", "")
                    md = f"# {part}\n\nFailed: {pair_state['error']}\n"
                else:
                    md = mod.check(edit, pairs, out, fl)
                    if errs:
                        md += f"\n{len(errs)} shots could not be read: " + "; ".join(f"{r['shot']}: {r['error'][:80]}" for r in errs) + "\n"
                (out / f"{part}.md").write_text(md + flags_md(fl.items))
                fl.save(out)
    if th_texts is not None:
        th_texts.join()
        import readtime as RT
        import textsize as TS
        if text_state.get("error"):
            log("text tracks failed:", text_state["error"])
        for part in ("readtime", "textsize"):
            if part not in parts:
                continue
            fl = Flags(part)
            if text_state.get("error"):
                fl.add("look", None, f"the {part} check failed: {text_state['error']}", "")
                md = f"# {part}\n\nFailed: {text_state['error']}\n"
            elif part == "readtime":
                md = RT.check(edit, text_state["timeline"], text_state["measured"], out, fl)
            else:
                md = TS.check(edit, text_state["timeline"], text_state["measured"], text_state["ladder"], out, fl)
            (out / f"{part}.md").write_text(md + flags_md(fl.items))
            fl.save(out)

    # ---- ranges
    if "range" in parts:
        t0 = time.time()
        audio = None
        if sound_state.get("mix") is not None:
            audio = {"sr": 48000, "mix": sound_state["mix"]} | (sound_state.get("st") or {})
        strips = []
        for r in a.range:
            ra, rb = common.parse_range(r)
            strips += P.filmstrip(edit, ra, rb, out, a.every, audio, tw=a.tile)
        md = out / "ranges.md"
        old = md.read_text() if md.exists() else "# Ranges\n\nEvery frame (or every Nth) of a range, the waveform above "\
            "(the mix, then each bus scaled to its own peak), frame ticks, cuts in red.\n\n"
        md.write_text(old + "\n".join(f"- [{p.name}]({p.name})" for p in strips) + "\n")
        timings["filmstrips"] = time.time() - t0

    timings["total"] = time.time() - t_start
    write_index(edit, out, timings, parts)
    log(f"done in {timings['total']:.0f} s: {out / 'index.md'}")
    print(out / "index.md")


def film_path(s: str) -> Path:
    """A path from the command line: as given if absolute, else in the current folder, else under FILM_ROOT."""
    p = Path(s)
    if p.is_absolute():
        return p
    return common.absolute(p) if p.exists() else FILM / p


def planned_text(edit, fl: Flags) -> None:
    """Text the shot list plans (the shot's notice names a caption, a label or a bumper) that the cut lacks."""
    import re
    for s in edit.shots:
        note = s.get("notice") or ""
        if not re.search(r"\b(caption|label|bumper|title|header)\b", note, re.I):
            continue
        has = [o for o in s.get("overlays", []) if o.get("type") in ("caption", "label", "bumper", "card", "png") or
               (o.get("type") == "video" and "footage/" not in str(o.get("path", "")))]
        if not has and s.get("source", {}).get("type") != "card":
            fl.add("look", s["start"], f"{s['id']}: the shot list plans text (\"{note[:110]}\"), the cut draws none", "picture.md",
                   t1=s["start"] + s["len"])


def flags_md(items: list[dict]) -> str:
    if not items:
        return "\n## Flags\n\nNone.\n"
    rows = [[f"**{f['sev']}**" if f["sev"] == "fix" else f["sev"], tc(f["t"], 3) if f["t"] is not None else "-",
             f["what"], f.get("evidence", "")] for f in sorted(items, key=lambda f: (SEV_ORDER[f["sev"]], f["t"] or 0))]
    return "\n## Flags\n\n" + md_table(["", "time", "what", "see"], rows) + "\n"


def write_index(edit, out: Path, timings: dict, parts: set[str]) -> None:
    flags = []
    for p in sorted(out.glob("flags-*.json")):
        flags += json.loads(p.read_text())
    L = [f"# Watching {edit.cut.name}", ""]
    L.append(f"`{rel(edit.cut)}`: {edit.nframes} frames at {edit.fps:g} fps, {tc(edit.duration)}, {edit.info['width']}x{edit.info['height']}.")
    L.append("")
    L.append(f"- **Made by:** {('`' + rel(edit.build) + '`') if edit.build else 'no build found'}; timeline "
             f"{('`' + rel(edit.timeline_path) + '`') if edit.timeline_path else '-'}; clock {('`' + rel(edit.clock_path) + '`') if edit.clock_path else '-'}; "
             f"config {('`' + rel(edit.config_path) + '`') if edit.config_path else '-'}.")
    for nline in edit.notes:
        if not nline.startswith("made by"):
            L.append(f"- {nline}")
    L.append(f"- **This run:** {', '.join(p for p in PARTS if p in parts)} in {timings['total']:.0f} s ("
             + ", ".join(f"{k} {v:.0f} s" for k, v in timings.items() if k != "total") + "; parts overlap).")
    glance = []
    if (out / "sound.json").exists():
        sj = json.loads((out / "sound.json").read_text())
        e = sj.get("ebur128") or {}
        glance.append(f"loudness {e.get('integrated')} LUFS, LRA {e.get('lra')} LU, true peak {e.get('true_peak')} dBTP, "
                      f"{sj.get('clipped_samples', 0)} clipped samples")
        mg = [m["margin"] for m in sj.get("margins", [])]
        if mg:
            glance.append(f"voice over its bed {min(mg):.1f}–{max(mg):.1f} LU across {len(mg)} voiced passages")
        sc = sj.get("stems_check") or {}
        if sc:
            glance.append(f"the buses add up to the cut within {sc['residual_db']} dB")
    if (out / "words.json").exists():
        wj = json.loads((out / "words.json").read_text())
        real = [d for d in wj.get("diffs", []) if d["kind"] != "spelling"]
        glance.append(f"Scribe heard {len(wj.get('words') or [])} words; {len(real)} differ from the script beyond spelling")
    if glance:
        L.append("- **At a glance:** " + "; ".join(glance) + ".")
    L.append("")
    L.append("## What the kit flags")
    L.append("")
    L.append("Machine findings to look at, not verdicts: **fix** is likely a real fault, *look* is worth a look; info items "
             "are listed in each part. Times are film times (m:ss.sss).")
    L.append("")
    main_ = sorted([f for f in flags if f["sev"] in ("fix", "look")], key=lambda f: (SEV_ORDER[f["sev"]], f["t"] or 0))
    if main_:
        L.append(md_table(["", "time", "part", "what", "see"],
                          [[f"**{f['sev']}**" if f["sev"] == "fix" else f["sev"], tc(f["t"], 3) if f["t"] is not None else "-",
                            f["part"], f["what"], f.get("evidence", "")] for f in main_]))
    else:
        L.append("Nothing.")
    n_info = sum(1 for f in flags if f["sev"] == "info")
    L.append("")
    L.append(f"Plus {n_info} info items in the parts below.")
    L.append("")
    L.append("## The parts")
    L.append("")
    links = [("shots.md", "Shots: a contact sheet per section, one row per shot (first, middle, last frame)"),
             ("overview.md", "Overview: a frame every 0.5 s"),
             ("ranges.md", "Ranges: every frame of chosen ranges, with the waveform"),
             ("words.md", "Words against picture: the transcript per shot, against the script, word times, cut-off lines"),
             ("sound.md", "Sound: loudness over time with the shots, the buses, voice against bed, silences, clicks, steps"),
             ("picture.md", "Picture: text at phone size, the safe area, black, frozen, flat and flashing frames"),
             ("sync.md", "Sync: named moments, the sound designer's cues and the footage's sounds, where they land"),
             ("ghosts.md", "Ghost labels: burned-in text that reads beside or through an overlay's band"),
             ("overtext.md", "Text over text: the overlay's letters on burned-in letters that still read"),
             ("shape.md", "The frame's shape against the ladder: the 4:3 box before NATIVE PIXELS, full width after"),
             ("stops.md", "Hard stops: a bus falling from audible to digital silence in under 10 ms"),
             ("readtime.md", "Text on screen too briefly: each text's fully visible time against its reading time"),
             ("textsize.md", "Text too small: each text's capital height at phone size against 8 px")]
    for f, what in links:
        if (out / f).exists():
            L.append(f"- [{what}]({f})")
    L.append("")
    L.append("## What it cannot judge")
    L.append("")
    L.append("Taste: whether a joke lands, the pacing, whether a picture shows what the words say, whether a comparison is "
             "unmistakable, whether the film delights. Truth: no claim is checked against the repository. Legibility beyond "
             "size and time (contrast, a busy background); burned-in text is found only as Quake's letters on a dark band; "
             "text that slides across the frame reads as up for less time than it is. Whether a flagged silence, freeze or "
             "click is meant. Someone still has to watch the frames that matter.")
    L.append("")
    (out / "index.md").write_text("\n".join(L))


if __name__ == "__main__":
    main()
