#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""One looped sample alone, beside a still listener: a script for the sound oracle's language,
and the port's mixer run on it.

    uv run mkloop.py RATE SAMPLE OUT.txt     # the script, e.g. 11025 ambience/hum1.wav hum1-11025.txt

The script starts SAMPLE looping on an entity beside a still listener, then runs 300 frames of
1/72 s, each advancing the mixer to the whole number of sample pairs the clock has reached at
RATE. `quaketool sndscript PAK OUT.txt OUT.raw --rate RATE [--fixes]` mixes it: id's mixer as
written (id's own C, oracle/build/snd-oracle, gives the same bytes for it), or with `--fixes`
the slop mixer. The raw output is interleaved 16-bit stereo, little-endian.

D14's waveforms are `mixer_raw(11025)`, `mixer_raw(11025, fixes=True)` and
`mixer_raw(48000, fixes=True)` of ambience/hum1.wav, cached under FILM_SCRATCH.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # film/pipeline: filmroot

import filmroot  # noqa: E402


def loop_script(rate: int, sample: str) -> str:
    """The sound-oracle script: SAMPLE looping beside a still listener, 300 frames at 72 Hz."""
    lines = ["viewent 1", "leaf 0 0 0 0", "listener 0 0 0 0.000000000 1.000000000 0 1.000000000 -0.000000000 0",
             "frametime 0.013888888888888889", f"start 30 2 {sample} 0 64 0 255 64"]
    clock, pairs = 0.0, 0
    for _ in range(300):
        lines.append("update")
        clock += 1 / 72
        target = int(clock * rate)
        lines.append(f"advance {target - pairs}")
        pairs = target
    return "\n".join(lines) + "\n"


def mixer_raw(rate: int, fixes: bool = False, sample: str = "ambience/hum1.wav") -> Path:
    """The port's mixer on loop_script(rate, sample): a raw file under FILM_SCRATCH, made once."""
    d = filmroot.scratch("diagrams") / "sound"
    d.mkdir(exist_ok=True)
    stem = f"one-{Path(sample).stem}-{rate}"
    txt, raw = d / f"{stem}.txt", d / f"{stem}.{'fix' if fixes else 'id'}.raw"
    if not raw.exists():
        if not filmroot.QUAKETOOL.exists():
            raise SystemExit(f"{filmroot.QUAKETOOL} is missing: build it with "
                             "`cd quake-rs && cargo build --release --bin quaketool`")
        txt.write_text(loop_script(rate, sample))
        part = raw.with_suffix(".part")
        subprocess.run([str(filmroot.QUAKETOOL), "sndscript", str(filmroot.PAK), str(txt), str(part),
                        "--rate", str(rate), *(["--fixes"] if fixes else [])], check=True, stdout=subprocess.DEVNULL)
        part.rename(raw)
    return raw


if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser(description="write a sound-oracle script: one looped sample beside a still listener")
    ap.add_argument("rate", type=int, help="the mixer's rate in Hz (11025, 48000)")
    ap.add_argument("sample", help="the sample's path in the pak, e.g. ambience/hum1.wav")
    ap.add_argument("out", help="the script to write")
    a = ap.parse_args()
    Path(a.out).write_text(loop_script(a.rate, a.sample))
