#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.14"
# dependencies = ["numpy", "scipy", "numba"]
# ///
"""Print {name: loudest 400 ms in LUFS} for every sound in FILM_ROOT/sound/id/ (at 48 kHz), as JSON.
extract_id.py runs it for id/LISTING.md's loudness column."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sfxlib  # noqa: E402

argparse.ArgumentParser(description=__doc__).parse_args()
idx = json.loads((sfxlib.ID_DIR / "index.json").read_text())
print(json.dumps({e["name"]: round(sfxlib.momentary_max(sfxlib.load(sfxlib.ID_DIR / e["name"])[0]), 1) for e in idx}))
