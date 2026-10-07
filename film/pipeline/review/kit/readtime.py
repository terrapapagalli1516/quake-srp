"""Text on screen too briefly: each caption, label, card and diagram block against its reading time, about
0.3 s a word and never under 1.5 s (an end card's joke fully up for 1.2 s is gone before it is read).

The items and their fully visible spans come from texts.py: the timeline's labels, captions and bumpers with
their exact words; measured cards, card overlays, diagram movies and PNGs with their words estimated from
each line's width. Lines of one item that come up within half a second of each other are read as one block:
its time runs from the last of them fully up to the median moment they start to go."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from common import Flags, md_table, tc
from ghosts import _rel

PER_WORD, MIN_S = 0.3, 1.5


def need(words: int) -> float:
    return max(MIN_S, PER_WORD * words)


def blocks(item: dict) -> list[dict]:
    ls = [ln for ln in item.get("lines", []) if not ln.get("changing")]
    ls.sort(key=lambda ln: ln["t0"])
    out: list[list[dict]] = []
    for ln in ls:
        if out and ln["t0"] - out[-1][0]["t0"] < 0.5:
            out[-1].append(ln)
        else:
            out.append([ln])
    res = []
    for g in out:
        t0 = max(ln["t0"] for ln in g)
        t1 = float(np.median([ln["t1"] for ln in g]))
        res.append({"t0": t0, "t1": t1, "words": sum(ln["words"] for ln in g), "lines": len(g)})
    return res


def check(edit, timeline: list[dict], measured: list[dict], out: Path, fl: Flags) -> str:
    rows = []
    bad = []
    for it in timeline:
        have = it["t1"] - it["t0"]
        nd = need(it["words"])
        verdict = "ok" if have >= nd else ("**too short**" if have < 0.75 * nd or have < 1.0 else "short")
        rows.append([tc(it["t0"], 2), "+".join(it["shots"]), it["what"][:60], str(it["words"]), f"{have:.2f}", f"{nd:.1f}", verdict, "-"])
        if have < nd:
            bad.append((it, have, nd, verdict))
    for it in measured:
        if it.get("error"):
            rows.append([tc(it["t0"], 2), it["shot"], it["name"], "-", "-", "-", "not measured: " + it["error"][:60], "-"])
            continue
        bs = blocks(it)
        if not bs:
            continue
        for k, b in enumerate(bs):
            have = b["t1"] - b["t0"]
            nd = need(b["words"])
            verdict = "ok" if have >= nd else ("**too short**" if have < 0.75 * nd or have < 1.0 else "short")
            what = f"{it['name']}" + (f", block {k + 1} of {len(bs)}" if len(bs) > 1 else "") + f" ({b['lines']} lines)"
            ev = _rel(it.get("evidence"), out)
            rows.append([tc(b["t0"], 2), it["shot"], what, f"≈{b['words']}", f"{have:.2f}", f"{nd:.1f}", verdict,
                         f"[frames]({ev})" if ev else "-"])
            if have < nd:
                bad.append(({"shots": [it["shot"]], "what": what, "t0": b["t0"], "t1": b["t1"], "words": b["words"],
                             "est": True, "evidence": ev}, have, nd, verdict))
    for it, have, nd, verdict in bad:
        sev = "fix" if "too short" in verdict else "look"
        words = f"≈{it['words']} words (from its width)" if it.get("est") else f"{it['words']} words"
        ev = it.get("evidence") or _timeline_evidence(edit, it, out)
        fl.add(sev, it["t0"], f"{'+'.join(it['shots'])}: {it['what']} is fully on screen {have:.2f} s ({tc(it['t0'], 2)}–"
               f"{tc(it['t1'], 2)}); {words} need {nd:.1f} s", ev, t1=it["t1"])
    L = ["# Text on screen too briefly", "",
         f"Each text's fully visible time against its reading time: {PER_WORD} s a word, never under {MIN_S} s. **too short** "
         "is under three quarters of it (or under a second). Labels, captions and bumpers: their words and timing from the "
         "timeline (a label fades in over 0.15 s, a caption types on in 6 frames, both last to the shot's end or `until`, "
         "and carry on into the next shot when it has the same text). Cards, cards.py overlays, diagram movies and PNGs: "
         "measured at 10 frames a second; a line is fully visible while its letters keep 60% of their best contrast (a fade-in counts from about its middle; a flickering bumper still reads); lines "
         "that come up together are one block; words are estimated from a line's width.", ""]
    L.append(md_table(["fully up", "shot", "text", "words", "seconds up", "needs", "verdict", "see"], rows))
    L.append("")
    return "\n".join(L)


def _timeline_evidence(edit, it: dict, out: Path) -> str:
    """Its first and last fully visible frames, at phone size."""
    from PIL import Image, ImageDraw
    import layers as LY
    from common import font
    fs = [int(round((it["t0"] + 0.02) * edit.fps)), int(round((it["t1"] - 0.02) * edit.fps))]
    got = LY.cut_frames(edit.cut, fs, edit.fps, 640, 360)
    im = Image.new("RGB", (1290, 400), (18, 18, 20))
    d = ImageDraw.Draw(im)
    d.text((6, 6), f"{it['what'][:70]} · first and last fully visible frames", font=font(14), fill=(230, 180, 60))
    for k, f in enumerate(fs):
        if f in got:
            im.paste(Image.fromarray(got[f]), (k * 650, 32))
            d.text((k * 650, 372), tc(f / edit.fps, 3), font=font(12), fill=(200, 200, 200))
    ev = out / "evidence"
    ev.mkdir(exist_ok=True)
    p = ev / f"readtime-{it['shots'][0]}-{tc(it['t0'], 2).replace(':', 'm')}.jpg"
    im.save(p, quality=88)
    return str(p.relative_to(out))
