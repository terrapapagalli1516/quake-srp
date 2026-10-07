"""Ghost labels: text burned into the footage that still reads beside or through a band the edit laid over it
(a label in the game's picture that the edit's own label was meant to cover).

Each overlay moment's frame is compared with its source's frame (layers.py). A source line counts as a
burned-in label when its letters are one height, on one baseline, on a dark plain band and hold still through
the shot. It is a ghost when the overlay reaches it (some of its letters are hidden, dimmed or under the
overlay's own letters) yet two or more of its letters still read: through a translucent band (at least 7 luma
levels of contrast left) or beside it."""

from __future__ import annotations

from pathlib import Path

from common import Flags, md_table, tc


def findings(pairs: list[dict]) -> list[dict]:
    """One finding per shot: its worst moment (most letters still reading) and every line seen."""
    by: dict[str, dict] = {}
    for r in pairs:
        for ln in r.get("ghosts") or []:
            f = by.setdefault(r["shot"], {"shot": r["shot"], "lines": {}, "moments": 0, "worst": None})
            key = (round(ln["box"][0] / 8), round(ln["box"][1] / 8))
            prev = f["lines"].get(key)
            if prev is None or ln["legible"] > prev["legible"]:
                f["lines"][key] = ln
            score = sum(x["legible"] for x in r["ghosts"])
            if f["worst"] is None or score > f["worst"][0]:
                f["worst"] = (score, r)
    out = []
    for f in by.values():
        r = f["worst"][1]
        f["t"], f["img"] = r["t"], r.get("ghost_img")
        f["moments"] = sum(1 for x in pairs if x["shot"] == f["shot"] and x.get("ghosts"))
        f["samples"] = sum(1 for x in pairs if x["shot"] == f["shot"] and "text" in x.get("why", []))
        f["lines"] = sorted(f["lines"].values(), key=lambda ln: (ln["box"][1], ln["box"][0]))
        out.append(f)
    return sorted(out, key=lambda f: f["t"])


def check(edit, pairs: list[dict], out: Path, fl: Flags) -> str:
    fs = findings(pairs)
    for f in fs:
        leg = sum(ln["legible"] for ln in f["lines"])
        tot = sum(ln["letters"] for ln in f["lines"])
        where = "; ".join(f"x {ln['box'][0]}, y {ln['box'][1]}: {ln['legible']} of {ln['letters']}" for ln in f["lines"][:4])
        sev = "fix" if leg >= 3 else "look"
        fl.add(sev, f["t"], f"{f['shot']}: burned-in text reads beside or through the overlay's band: {leg} of {tot} "
               f"letters still read ({where}), at {f['moments']} of {f['samples']} moments", _rel(f["img"], out))
    L = ["# Ghost labels", "",
         "Text burned into the footage that still reads beside or through a band the edit laid over it. Each overlay's "
         "span is looked at three times, full size: the cut against its source at the same moment (the build's frozen "
         "copy, placed as the build places it). A burned-in label is a source line of letters of one height, on one "
         "baseline, on a dark plain band, holding still through the shot. It is a ghost when the overlay reaches it but "
         "two or more of its letters still read (7 or more luma levels of contrast left).", ""]
    if not fs:
        L.append("None found.")
    else:
        rows = []
        for f in fs:
            for ln in f["lines"]:
                states = "".join({"shown": "s", "through": "t", "hidden": "h", "under": "u"}[m["state"]] for m in ln["states"])
                rows.append([f["shot"], tc(f["t"], 3), f"{ln['box']}", f"{ln['h']:.0f}", f"{ln['legible']}/{ln['letters']}", states,
                             f"[frame]({_rel(f['img'], out)})" if f.get("img") else "-"])
        L.append(md_table(["shot", "worst moment", "line (x, y, w, h at 1080p)", "letter px", "still read", "letters",
                           "evidence"], rows))
        L.append("")
        L.append("Letters: s shown untouched, t read through the band, h hidden, u under the overlay's own letters.")
    L.append("")
    return "\n".join(L)


def _rel(p: str | None, out: Path) -> str:
    if not p:
        return ""
    try:
        return str(Path(p).relative_to(out))
    except ValueError:
        return str(p)
