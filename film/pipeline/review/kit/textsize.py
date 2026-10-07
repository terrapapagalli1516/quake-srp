"""Text too small: each overlay text's capital height at phone size, 640 wide (a 1080p film on a phone held
sideways), against 8 px. Labels, captions and bumpers from their scale (Quake's 8x8 letters, a capital 6 of 8
rows); cards, card overlays, diagram movies and PNGs measured on their letters (texts.py); the ladder on one
frame of each of its movies. Under 6 px is a **fix**; 6 to 8 px a *look*. Like items are flagged together (all
the labels of one scale, all the ladder's spans)."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from common import Flags, md_table, tc
from ghosts import _rel

MIN_PX, BAD_PX = 8.0, 6.0


def check(edit, timeline: list[dict], measured: list[dict], ladder: list[dict], out: Path, fl: Flags) -> str:
    rows = []
    groups: dict[tuple, list] = {}
    for it in timeline:
        rows.append([tc(it["t0"], 2), "+".join(it["shots"]), it["what"][:60], f"{it['cap']:.1f}", "scale", _v(it["cap"]), "-"])
        if it["cap"] < MIN_PX:
            groups.setdefault((it["kind"], round(it["cap"], 1)), []).append(it)
    for it in measured:
        ls = [ln for ln in it.get("lines", []) if ln["marks"] >= 2 and ln["box"][2] >= 1.5 * ln["box"][3]]
        if not ls:
            continue
        caps = sorted(ln["cap"] for ln in ls)
        small = [c for c in caps if c < MIN_PX]
        med = float(np.median(caps))
        ev = _rel(it.get("evidence"), out)
        rows.append([tc(it["t0"], 2), it["shot"], it["name"], f"{caps[0]:.1f} (median {med:.1f}, {len(small)} of {len(caps)} lines under 8)",
                     "measured", _v(med if len(caps) > 2 else caps[0]), f"[frames]({ev})" if ev else "-"])
        if small:
            sev = "fix" if med < BAD_PX or sum(c < BAD_PX for c in caps) >= 3 else "look"
            fl.add(sev, it["t0"], f"{it['shot']}: {it['name']}: {len(small)} of its {len(caps)} text lines under 8 px at phone size "
                   f"(smallest {caps[0]:.1f}, median {med:.1f})", ev or "textsize.md")
    if ladder:
        caps = [x["cap_median"] for x in ladder]
        rows.append([tc(ladder[0]["t"], 2), f"{len(ladder)} spans", "the ladder's rows", f"{min(caps):.1f}–{max(caps):.1f}",
                     "measured", _v(float(np.median(caps))), "-"])
        if float(np.median(caps)) < MIN_PX:
            fl.add("fix" if float(np.median(caps)) < BAD_PX else "look", ladder[0]["t"],
                   f"the ladder's rows are {float(np.median(caps)):.1f} px tall at phone size, in all {len(ladder)} of its spans "
                   f"({', '.join(x['shot'] for x in ladder[:6])}{' …' if len(ladder) > 6 else ''})", "textsize.md")
    for (kind, cap), its in sorted(groups.items(), key=lambda kv: kv[1][0]["t0"]):
        sev = "fix" if cap < BAD_PX else "look"
        names = "; ".join(f"{'+'.join(i['shots'])} \"{i['text'][:28]}\"" for i in its[:8]) + (" …" if len(its) > 8 else "")
        fl.add(sev, its[0]["t0"], f"{len(its)} {kind}{'s' if len(its) > 1 else ''} at {cap:.1f} px at phone size ({names})",
               "textsize.md")
    L = ["# Text too small", "",
         f"Each overlay text's capital height at phone size (640 wide, 1:1), against {MIN_PX:g} px; under {BAD_PX:g} px is a "
         "**fix**. Labels, captions and bumpers from their scale (Quake's 8x8 letters: a capital is 6 of the 8 rows, 18 px at 3x in the cut); cards, "
         "cards.py overlays, diagram movies and PNGs measured on their letters at 1280x720 (the median of a line's taller "
         "letters); the ladder on one frame of each of its movies.", ""]
    L.append(md_table(["from", "shot", "text", "capital px at 640", "how", "verdict", "see"], rows))
    L.append("")
    return "\n".join(L)


def _v(c: float) -> str:
    return "**too small**" if c < BAD_PX else ("small" if c < MIN_PX else "ok")
