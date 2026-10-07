"""Text over text: the edit's own text drawn where text burned into the footage still reads, two layers of
letters in one place at once (the ladder's rows over a list the footage itself shows).

From the same frame pairs as the ghosts (layers.py): the overlay's text lines are letters in the cut that are
not the source's; the footage's are label-like source lines (as ghosts.py, but allowed to change a little, as a
list being struck through does). A hit is three or more footage letters still reading inside an overlay line's
box, grown by half a letter."""

from __future__ import annotations

from pathlib import Path

from common import Flags, md_table, tc
from ghosts import _rel


def findings(pairs: list[dict]) -> list[dict]:
    by: dict[str, dict] = {}
    for r in pairs:
        hs = r.get("overtext") or []
        if not hs:
            continue
        f = by.setdefault(r["shot"], {"shot": r["shot"], "moments": 0, "worst": None})
        f["moments"] += 1
        score = (len({tuple(h["ov"]["box"]) for h in hs}), sum(h["letters"] for h in hs))
        if f["worst"] is None or score > f["worst"][0]:
            f["worst"] = (score, r)
    out = []
    for f in by.values():
        (nov, nlet), r = f["worst"]
        f.update(t=r["t"], img=r.get("overtext_img"), ov_lines=nov, letters=nlet,
                 samples=sum(1 for x in pairs if x["shot"] == f["shot"] and "text" in x.get("why", [])),
                 hits=r["overtext"])
        out.append(f)
    return sorted(out, key=lambda f: f["t"])


def check(edit, pairs: list[dict], out: Path, fl: Flags) -> str:
    fs = findings(pairs)
    for f in fs:
        sev = "fix" if f["ov_lines"] >= 2 or f["letters"] >= 4 else "look"
        fl.add(sev, f["t"], f"{f['shot']}: the overlay's text sits on text burned into the footage: {f['ov_lines']} of its "
               f"lines over {f['letters']} letters that still read, at {f['moments']} of {f['samples']} moments",
               _rel(f["img"], out))
    L = ["# Text over text", "",
         "Two layers of letters in one place at once: the edit's own text (letters in the cut that are not its source's) "
         "drawn where letters burned into the footage still read. The same frame pairs as the ghost labels.", ""]
    if not fs:
        L.append("None found.")
    else:
        rows = []
        for f in fs:
            ovs = sorted({tuple(h["ov"]["box"]) for h in f["hits"]})
            rows.append([f["shot"], tc(f["t"], 3), str(f["ov_lines"]), str(f["letters"]),
                         "; ".join(f"{list(b)}" for b in ovs[:4]) + (" …" if len(ovs) > 4 else ""),
                         f"{f['moments']}/{f['samples']}", f"[frame]({_rel(f['img'], out)})" if f.get("img") else "-"])
        L.append(md_table(["shot", "worst moment", "overlay lines", "footage letters under them", "overlay lines (x, y, w, h)",
                           "moments", "evidence"], rows))
    L.append("")
    return "\n".join(L)
