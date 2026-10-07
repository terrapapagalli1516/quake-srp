#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow", "soundfile", "pyloudnorm", "scipy"]
# ///
"""Re-make a build's four buses (voice, music, game, sfx) with the edit's own mixer, for a build that did not
save them in BUILD/stems/: film/pipeline/edit/build.py's mix_audio() on the build's own timeline.json (whose
sources are the build's frozen copies while they last, else the files it names under FILM_ROOT), the SFX stem
the build's mix.json names, and the master gain it applied. The result is each bus as the film plays it,
before the master limiter. watch.py runs this by itself when it needs the buses.

    uv run film/pipeline/review/kit/derive_stems.py BUILD_DIR CONFIG OUT_DIR

Writes OUT_DIR/{voice,music,game,sfx}.wav (float, stereo, 48 kHz) and OUT_DIR/meta.json. Its dependencies
are build.py's.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf

KIT = Path(__file__).resolve().parent
sys.path.insert(0, str(KIT))
from common import FILM, PIPELINE, absolute, pick_sfx  # noqa: E402

sys.path.insert(0, str(PIPELINE / "edit"))   # the edit's own code: build.py, timeline.py


def main() -> None:
    ap = argparse.ArgumentParser(description="re-make a build's four buses with the edit's own mixer")
    ap.add_argument("build", type=Path, help="the build folder, edit/vN/build-NNN")
    ap.add_argument("config", type=Path, help="the edit.toml it was built from")
    ap.add_argument("out", type=Path, help="where to write {voice,music,game,sfx}.wav and meta.json")
    a = ap.parse_args()
    bdir, cfgp, out = absolute(a.build), absolute(a.config), absolute(a.out)
    out.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    import build  # noqa: E402  (the build's own code: its mixer, its levels)
    import timeline  # noqa: E402
    tl = json.loads((bdir / "timeline.json").read_text())
    cfg = timeline.load_config(cfgp)
    mix = json.loads((bdir / "mix.json").read_text()) if (bdir / "mix.json").exists() else {}
    note = []
    glob = (cfg.get("sfx") or {}).get("glob")
    p, how = pick_sfx(mix.get("sfx"), str(bdir.parent), tl["frames"] / tl["fps"], glob)
    sfx_path = str(p.relative_to(FILM)) if p else None
    if mix.get("sfx") and not str(mix.get("sfx")).startswith(("no ", "none")):
        # the stem this build used, if it is still this clock's; else one made on the same clock; else none
        build.find_sfx = (lambda tl, cfg, n: {"path": sfx_path, "note": sfx_path}) if sfx_path else \
            (lambda tl, cfg, n: {"note": "none"})
        if not how.startswith("`"):
            note.append("SFX: " + how)
    else:  # the build mixed no SFX stem: neither does this
        sfx_path = None
        build.find_sfx = lambda tl, cfg, n: {"note": "none"}
    missing = [s["id"] for s in tl["shots"] if s["source"].get("frozen") and not Path(s["source"]["frozen"]).exists()]
    for s in tl["shots"]:  # a frozen copy cleaned away: fall back to the file the timeline names
        src = s["source"]
        for k in ("frozen", "frozen_sound", "frozen_a"):
            if src.get(k) and not Path(src[k]).exists():
                src.pop(k)
    if missing:
        note.append(f"{len(missing)} frozen sources gone; their current files used: {', '.join(missing[:8])}")
    n = tl["frames"] * build.SPF
    tmp = out / "tmp"
    tmp.mkdir(exist_ok=True)
    rep, stems = build.mix_audio(tl, cfg, n, tmp, tmp)
    gain_db = float((mix.get("master") or {}).get("gain_db", 0.0))
    if "master" not in mix:
        note.append("no master gain recorded: stems are pre-master")
    g = 10 ** (gain_db / 20)
    for k in ("voice", "music", "game", "sfx"):
        x = stems.get(k)
        if x is None or np.isscalar(x):
            continue
        sf.write(str(out / f"{k}.wav"), (x * g).astype(np.float32), build.SR, subtype="FLOAT")
        stems[k] = None
    if sfx_path and (FILM / sfx_path).with_suffix(".json").exists():   # keep the cue list that goes with this stem
        (out / "sfx.json").write_text((FILM / sfx_path).with_suffix(".json").read_text())
    (out / "meta.json").write_text(json.dumps({
        "build": str(bdir), "config": str(cfgp), "sfx": sfx_path, "music": rep.get("music"),
        "master_gain_db": gain_db, "seconds": round(time.time() - t0, 1), "notes": note,
        "muted": rep.get("muted")}, indent=1))
    print(json.dumps({"ok": True, "seconds": round(time.time() - t0, 1), "notes": note}))


if __name__ == "__main__":
    main()
