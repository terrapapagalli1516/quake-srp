#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Fit the score's named events to the edit's clock.

    uv run film/pipeline/music/v7/fit_events.py -o film/pipeline/music/v7/events-v7.inc

Reads events.tsv beside this file (name, nominal time, source) and the edit's clock
(default FILM_ROOT/edit/clock.json, which the edit's timeline step writes, with the
ladder-events.json beside it), and finds each event there: `cut:SHOT+s` (the cut to SHOT),
`moment:NAME` (the clock's named events), `pic:SHOT:what:i` (picture events), `hit:SHOT:what:i`
(the edit's hits), `next:SHOT` (the cut after SHOT's), `voice:ID:start|end`, `ladder:ITEM` /
`ladder_off:ITEM` (ladder-events.json), `film_end`; `a|b` tries a, then b. Anything it can't
find keeps its nominal time, and says so. Writes the score's `let` lines (to stdout, or -o);
the score includes them. The clock is not in the repository, so events-v7.inc is: it is the
fit of the clock the v7 cut was made on.
"""
from __future__ import annotations

import argparse
import io
import json
import re
import sys
from contextlib import redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1]))

from filmroot import FILM  # noqa: E402


def load_clock(path):
    if not path or not Path(path).exists():
        return None
    clock = json.loads(Path(path).read_text())
    lad = Path(path).with_name("ladder-events.json")
    if lad.exists():
        clock["_ladder"] = json.loads(lad.read_text())
    return clock


def resolve(src, clock):
    if clock is None:
        return None
    m = re.fullmatch(r"cut:([\w-]+)([+-][\d.]+)?", src)
    if m:
        for c in clock.get("cuts", []):
            if c.get("to") == m.group(1):
                return c["t"] + float(m.group(2) or 0)
        return None
    m = re.fullmatch(r"next:([\w-]+)", src)                  # the cut that follows SHOT's
    if m:
        cuts = sorted(clock.get("cuts", []), key=lambda c: c["t"])
        for i, c in enumerate(cuts):
            if c.get("to") == m.group(1) and i + 1 < len(cuts):
                return cuts[i + 1]["t"]
        return None
    m = re.fullmatch(r"voice:([\w.-]+):(start|end)([+-][\d.]+)?", src)   # a line's speech span
    if m:
        for v in clock.get("voice", []):
            if v.get("id") == m.group(1):
                return v["speech"][0 if m.group(2) == "start" else 1] + float(m.group(3) or 0)
        return None
    m = re.fullmatch(r"moment:(\w+)", src)
    if m:
        for e in clock.get("events", []):
            if e.get("name") == m.group(1):
                return e["t"]
        return None
    m = re.fullmatch(r"(pic|hit):([\w-]+):(.+):(\d+)", src)
    if m:
        table = clock.get("picture_events" if m.group(1) == "pic" else "edit_hits", [])
        for e in table:
            if e.get("shot") == m.group(2) and e.get("what") == m.group(3):
                ts = e.get("t", [])
                i = int(m.group(4))
                return ts[i] if i < len(ts) else None
        return None
    m = re.fullmatch(r"(ladder|ladder_off):(.+)", src)       # ladder-events.json, beside the clock
    if m:
        ladder = clock.get("_ladder", {})
        kind = "light" if m.group(1) == "ladder" else "dark"
        for e in ladder.get("events", []):
            if e.get("do") == kind and e.get("item") == m.group(2):
                return e["t"]
        return None
    if src == "film_end":
        film = clock.get("film", {})
        return film.get("duration") or film.get("seconds")
    return None


def fit(clock):
    rows = [l.rstrip("\n").split("\t") for l in (HERE / "events.tsv").read_text().splitlines()
            if l and not l.startswith("#")]
    print("# The score's named events, fitted by v7/fit_events.py "
          + ("to the edit's clock." if clock else "(no edit clock yet: every time is nominal)."))
    missing = []
    for name, nominal, src, what in rows:
        if nominal.startswith("="):
            expr = re.sub(r"\b([A-Z][A-Z0-9_]+)\b", r"$\1", nominal[1:])
            print(f"let {name:<16s} = {expr:<14s}  # {what}".rstrip())
            continue
        t = None
        for alt in src.split("|"):              # alternatives, tried in order
            t = resolve(alt, clock)
            if t is not None:
                src = alt
                break
        if t is None:
            if clock is not None and src not in ("nominal", "derived"):
                missing.append(f"{name} ({src})")
            t, tag = float(nominal), "nominal"
        else:
            tag = src
        print(f"let {name:<16s} = {t:<14.3f}  # {tag}{': ' + what if what else ''}")
    if missing:
        print(f"# not in the clock, nominal kept: {', '.join(missing)}")
        print(f"fit_events: {len(missing)} events kept at nominal: {', '.join(missing)}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("clock", nargs="?", type=Path, default=FILM / "edit" / "clock.json",
                    help="the edit's clock.json (default: FILM_ROOT/edit/clock.json)")
    ap.add_argument("-o", "--out", type=Path, help="write the .inc here instead of to stdout")
    a = ap.parse_args()
    clock = load_clock(a.clock)
    if clock is None:
        sys.exit(f"fit_events: no clock at {a.clock} (the edit's timeline step writes it); events-v7.inc is unchanged")
    buf = io.StringIO()
    with redirect_stdout(buf):
        fit(clock)
    if a.out:
        a.out.write_text(buf.getvalue())
    else:
        sys.stdout.write(buf.getvalue())


if __name__ == "__main__":
    main()
