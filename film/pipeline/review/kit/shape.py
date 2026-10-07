"""The picture's state against the story: in act 4 the frame's shape follows the ladder. Before NATIVE PIXELS
lights, the game's picture is the 4:3 box (its pillars, x < 240 and x >= 1680, black or carrying only the
ladder); after it, the picture is full width. A shot that shows the full picture before the burst breaks
the story.

The pillars are measured on each act-4 game shot's own source, placed as the build places it, so the ladder
and the diagrams laid over the pillars do not count. A side-by-side comparison of one view (its halves alike)
is a lab layout, not the game's frame: it is listed, not flagged. Web captures and diagrams are not the game's
picture and are left out. The moment NATIVE PIXELS lights comes from the edit's ladder-events.json (checked
against this cut's shots), else the clock's `burst`."""

from __future__ import annotations

import json
from pathlib import Path

from common import Flags, md_table, rel, tc
from ghosts import _rel

WIDE, BLACK, PAIR = 0.30, 0.03, 0.60


def ladder_events(edit) -> tuple[dict | None, str]:
    """The ladder's events on this cut's clock: edit/vN/ladder-events.json whose spans start where this cut's
    shots of the same name start."""
    cands = []
    for d in [edit.build.parent if edit.build else None, edit.timeline_path.parent if edit.timeline_path else None,
              edit.config_path.parent if edit.config_path else None]:
        if d is not None and (d / "ladder-events.json").exists() and d / "ladder-events.json" not in cands:
            cands.append(d / "ladder-events.json")
    starts = {s["id"]: float(s["start"]) for s in edit.shots}
    note = "no ladder-events.json beside this cut's build"
    for p in cands:
        j = json.loads(p.read_text())
        spans = j.get("spans") or []
        bad = [sp["name"] for sp in spans if sp["name"] not in starts or abs(starts[sp["name"]] - float(sp["from"])) > 0.02]
        if spans and not bad:
            return j, f"`{rel(p)}`"
        note = f"`{rel(p)}` is another edit's ladder ({', '.join(bad[:4])} start elsewhere)"
    return None, note


def native_time(edit) -> tuple[float | None, str]:
    j, how = ladder_events(edit)
    if j:
        t = next((float(e["t"]) for e in j.get("events", []) if e.get("do") == "light" and "NATIVE" in str(e.get("item", "")).upper()), None)
        if t is not None:
            return t, f"NATIVE PIXELS lights at {tc(t, 3)} ({how})"
        how += ": no NATIVE PIXELS row lights"
    ev = next((e for e in (edit.clock or {}).get("events", []) if e.get("name") == "burst"), None)
    if ev:
        return float(ev["t"]), f"{how}; the clock's `burst` at {tc(float(ev['t']), 3)} instead"
    return None, how


def game_picture(s: dict) -> bool:
    src = s.get("source", {})
    if src.get("type") == "burst":
        return True
    p = str(src.get("path", ""))
    return src.get("type") == "video" and not p.startswith(("footage/web", "diagrams/")) and "/web/" not in p


def check(edit, pairs: list[dict], out: Path, fl: Flags) -> str:
    tn, how = native_time(edit)
    L = ["# The frame's shape against the ladder", "",
         "Act 4 switches the slop on one setting at a time; the picture's shape must follow the ladder. Before NATIVE "
         "PIXELS lights, the game's picture is the 4:3 box (its pillars, x < 240 and x ≥ 1680 at 1080p, black or "
         "carrying only the ladder); after it, full width. Measured on each act-4 game shot's own source, placed as the "
         "build places it (so the ladder and diagrams over the pillars do not count): *wide* is a picture in both "
         f"pillars (≥ {WIDE:.0%} of their pixels), *boxed* is both black (≤ {BLACK:.0%}). A side-by-side comparison of one "
         f"view (its halves correlate ≥ {PAIR}) is a lab layout, listed but not flagged.", "",
         f"- **The switch:** {how}.", ""]
    if tn is None:
        fl.add("info", None, "the frame's shape was not checked: " + how, "shape.md")
        L.append("Not checked: no time for NATIVE PIXELS.")
        return "\n".join(L) + "\n"
    by: dict[str, list[dict]] = {}
    for r in pairs:
        if "shape" in r.get("why", []):
            by.setdefault(r["shot"], []).append(r)
    rows = []
    for sid, rs in by.items():
        s = next(x for x in edit.shots if x["id"] == sid)
        if not game_picture(s):
            continue
        a, b = float(s["start"]), float(s["start"] + s["len"])
        if s["source"].get("type") == "burst":
            src = s["source"]
            T = a + float(src.get("at", 0.0))
            D = float(src.get("dur", 0.5))
            before = [r for r in rs if r["t"] < T - 0.05]
            after = [r for r in rs if r["t"] > T + D + 0.05]
            ok_b = all(r["cut_pillars"]["left"] <= BLACK for r in before)
            ok_a = all(r["cut_pillars"]["left"] >= WIDE for r in after)
            verdict = "the box bursts" if ok_b and ok_a else "the burst does not open the box"
            if not (ok_b and ok_a):
                fl.add("fix", a, f"{sid}: the burst at {tc(T, 3)} does not go from the 4:3 box to full width (the cut's left "
                       f"pillar: {', '.join(f'{r['cut_pillars']['left']:.2f}' for r in rs)})",
                       _rel(rs[0].get("shape_img"), out))
            if abs(T - tn) > 0.5:
                fl.add("look", T, f"{sid}: the burst is at {tc(T, 3)}, NATIVE PIXELS lights at {tc(tn, 3)}", "shape.md")
            rows.append([sid, tc(a), tc(b), "burst", "-", "-", verdict, _ev(rs, out)])
            continue
        meas = [r for r in rs if r.get("pillars")]
        if not meas:
            rows.append([sid, tc(a), tc(b), "-", "-", "-", "no source frame (" + (rs[0].get("note") or "?") + ")", "-"])
            continue
        wide = [r for r in meas if r["pillars"]["left"] >= WIDE and r["pillars"]["right"] >= WIDE]
        boxed = [r for r in meas if r["pillars"]["left"] <= BLACK and r["pillars"]["right"] <= BLACK]
        pair = max(r.get("halves_corr", 0.0) for r in meas) >= PAIR
        state = "wide" if len(wide) * 2 > len(meas) else ("boxed" if len(boxed) * 2 > len(meas) else "mixed")
        pil = ", ".join(f"{r['pillars']['left']:.2f}/{r['pillars']['right']:.2f}" for r in meas)
        corr = max(r.get("halves_corr", 0.0) for r in meas)
        if b <= tn + 0.02:
            phase = "before"
            if state == "wide" and not pair:
                verdict = "**the full picture before NATIVE PIXELS**"
                worst = max(wide, key=lambda r: r["pillars"]["left"] + r["pillars"]["right"])
                fl.add("fix", a, f"{sid}: the picture fills the frame {tn - b:.1f} s before NATIVE PIXELS "
                       f"lights; until then it is the 4:3 box (its source's pillars {pil})", _rel(worst.get("shape_img"), out),
                       t1=b)
            elif state == "wide":
                verdict = "a side-by-side comparison (a lab layout)"
                fl.add("info", a, f"{sid}: full width before NATIVE PIXELS, but a side-by-side comparison (halves alike, "
                       f"{corr:.2f}): a lab layout", "shape.md", t1=b)
            elif state == "mixed" and wide and not pair:
                verdict = "partly the full picture"
                fl.add("look", wide[0]["t"], f"{sid}: the picture fills the frame at {len(wide)} of {len(meas)} moments before "
                       f"NATIVE PIXELS lights (pillars {pil})", _rel(wide[0].get("shape_img"), out), t1=b)
            else:
                verdict = "the box" if state == "boxed" else state
        elif a >= tn - 0.02:
            phase = "after"
            if state == "boxed" and len(boxed) == len(meas):
                verdict = "**back in the box after NATIVE PIXELS**"
                fl.add("fix", a, f"{sid}: the picture is the 4:3 box after NATIVE PIXELS lit at {tc(tn, 3)} (pillars {pil})",
                       _rel(boxed[0].get("shape_img"), out), t1=b)
            elif boxed:
                verdict = f"boxed at {len(boxed)} of {len(meas)} moments"
                fl.add("info", boxed[0]["t"], f"{sid}: boxed at {len(boxed)} of {len(meas)} moments after NATIVE PIXELS "
                       "(a comparison that starts from id's view?)", _rel(boxed[0].get("shape_img"), out), t1=b)
            else:
                verdict = "full width" if state == "wide" else state
        else:
            phase, verdict = "across", state
        rows.append([sid, tc(a), tc(b), phase, pil, f"{corr:.2f}", verdict, _ev(rs, out)])
    L.append(md_table(["shot", "from", "to", "NATIVE PIXELS", "source pillars L/R (share lit)", "halves alike", "verdict",
                       "frames"], rows))
    L.append("")
    return "\n".join(L)


def _ev(rs: list[dict], out: Path) -> str:
    return " ".join(f"[{tc(r['t'], 1)}]({_rel(r.get('shape_img'), out)})" for r in rs if r.get("shape_img"))
