#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Diff id's server edicts against the port's at the same server time.

Inputs are two dumps in the `oracle_edicts` format (census/oracle_run.py's
`oracle_edicts PATH` on the C side, `quaketool census-edicts` on the port side).
Edict numbers differ between the two (the port reserves the player after the
map's entities), so entities are matched by (classname, model) and then
greedily by nearest origin.

    uv run census/edict_diff.py C.txt PORT.txt [--t 4.7] [--fields frame,movetype,mins,maxs,...]

Prints the entities only one side has, and matched pairs whose origin differs by
more than --tol units or whose compared fields differ. Monster AI is random-
driven (id's rand() vs the port's PRNG), so monsters wander apart once awake;
read those rows with that in mind.
"""

import argparse
import math
from collections import defaultdict

COLS = ["num", "classname", "model", "origin", "angles", "frame", "movetype", "solid", "flags",
        "health", "nextthink", "effects", "targetname", "mins", "maxs"]


def load(path):
    blocks = {}
    cur = None
    for line in open(path):
        line = line.rstrip("\n")
        if line.startswith("# t="):
            t = float(line.split()[1][2:])
            cur = blocks.setdefault(round(t, 1), [])
            continue
        if not line or cur is None:
            continue
        f = line.split("\t")
        f += [""] * (len(COLS) - len(f))
        d = dict(zip(COLS, f))
        d["origin"] = tuple(float(x) for x in d["origin"].split())
        d["angles"] = tuple(float(x) for x in d["angles"].split())
        # mins/maxs (the box setmodel/setsize gave it): empty in older dumps
        d["mins"] = tuple(float(x) for x in d["mins"].split())
        d["maxs"] = tuple(float(x) for x in d["maxs"].split())
        for k in ("frame", "movetype", "solid", "flags", "health", "nextthink", "effects"):
            try:
                d[k] = float(d[k])
            except ValueError:
                d[k] = 0.0
        cur.append(d)
    return blocks


def dist(a, b):
    return math.sqrt(sum((x - y) ** 2 for x, y in zip(a, b)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("c")
    ap.add_argument("port")
    ap.add_argument("--t", type=float, default=None, help="server time block to compare (default: every common one)")
    ap.add_argument("--tol", type=float, default=1.0)
    ap.add_argument("--fields", default="frame,movetype,solid,effects")
    ap.add_argument("--skip", default="bodyque,player", help="classnames to ignore")
    a = ap.parse_args()
    C, P = load(a.c), load(a.port)
    times = [a.t] if a.t is not None else sorted(set(C) & set(P))
    fields = [f for f in a.fields.split(",") if f]
    skip = set(a.skip.split(","))
    for t in times:
        t = round(t, 1)
        if t not in C or t not in P:
            print(f"t={t}: missing on one side (C has {sorted(C)}, port has {sorted(P)})")
            continue
        print(f"=== t={t}: C {len(C[t])} edicts, port {len(P[t])} edicts")
        cg, pg = defaultdict(list), defaultdict(list)
        for d in C[t]:
            if d["classname"] not in skip:
                cg[(d["classname"], d["model"])].append(d)
        for d in P[t]:
            if d["classname"] not in skip:
                pg[(d["classname"], d["model"])].append(d)
        only_c, only_p, diffs = [], [], []
        for key in sorted(set(cg) | set(pg)):
            cs, ps = list(cg.get(key, [])), list(pg.get(key, []))
            pairs = sorted(((dist(c["origin"], p["origin"]), i, j) for i, c in enumerate(cs) for j, p in enumerate(ps)))
            used_c, used_p = set(), set()
            for dd, i, j in pairs:
                if i in used_c or j in used_p:
                    continue
                used_c.add(i)
                used_p.add(j)
                c, p = cs[i], ps[j]
                fd = [f"{f} {val(c[f])}/{val(p[f])}" for f in fields if c[f] != p[f]]
                if dd > a.tol or fd:
                    diffs.append(f"  {key[0]:24s} {key[1]:22s} C#{c['num']:>4} {fmt(c['origin'])} port#{p['num']:>4} {fmt(p['origin'])} d={dd:.1f} {' '.join(fd)}")
            only_c += [f"  {key[0]:24s} {key[1]:22s} C#{cs[i]['num']:>4} {fmt(cs[i]['origin'])} tn={cs[i]['targetname']}" for i in range(len(cs)) if i not in used_c]
            only_p += [f"  {key[0]:24s} {key[1]:22s} port#{ps[j]['num']:>4} {fmt(ps[j]['origin'])} tn={ps[j]['targetname']}" for j in range(len(ps)) if j not in used_p]
        print(f"only in C ({len(only_c)}):")
        print("\n".join(only_c))
        print(f"only in port ({len(only_p)}):")
        print("\n".join(only_p))
        print(f"matched but different ({len(diffs)}; fields C/port):")
        print("\n".join(diffs))


def fmt(v):
    return "(" + " ".join(f"{x:.1f}" for x in v) + ")"


def val(x):
    return fmt(x) if isinstance(x, tuple) else f"{x:g}"


if __name__ == "__main__":
    main()
