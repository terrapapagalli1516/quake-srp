#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D14, id's mixer at the device's rate, as a band (LAB9, 7.6 s) over the footage, clear of
the ladder's panel (y 30-758): the loop click, and the fix.

AUDIT.md, the slop options table, full-rate sound: "id's mixer at the device's rate with
four of its faults fixed: the click at a loop's restart, 48 kHz pitch 1.4% flat, ambient
fades stalling above 100 fps, `S_StopSound`'s channel range". The loop seam (AUDIT.md, "The
engine's own mixer"): `SND_PaintChannelFrom8` paints a loop's restart from `paintbuffer[0]`,
so "after a loop restart inside a paint pass the samples land over the pass's start and the
rest of the pass gets nothing from that channel - a click on every lap".

The waveforms are real: one looped `ambience/hum1.wav` beside a still listener (mkloop.py)
through the port's mixer, `quaketool sndscript ... --rate 11025` (id's mixer as written; id's
own C gives the same bytes for this script) and `--rate 48000 --fixes` (the slop mixer).
Left channel, the same 54 ms of time from both: 11025 Hz pairs 39,700-40,299 and their 48 kHz
span. The step ringed is where id's paint pass ends its silence and the channel comes back.

Cues `ring` (the click plays) and `hit` (the waveform redraws clean at the device's rate).
`--teleporter` labels the sound as the film's LAB9 hears it: "HUM1.WAV · A TELEPORTER'S HUM ·
54 MS SHOWN" (the 54 ms is the stretch of waveform drawn, not the loop's length).
"""

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from common import *  # noqa: F403

import numpy as np  # noqa: E402

from mkloop import mixer_raw  # noqa: E402

TELEPORTER = "--teleporter" in sys.argv
if TELEPORTER:
    sys.argv.remove("--teleporter")

# ------------------------------------------------------------ the facts ----

ID = np.fromfile(mixer_raw(11025), "<i2").reshape(-1, 2)[:, 0].astype(float)
FIX11 = np.fromfile(mixer_raw(11025, fixes=True), "<i2").reshape(-1, 2)[:, 0].astype(float)
FIX48 = np.fromfile(mixer_raw(48000, fixes=True), "<i2").reshape(-1, 2)[:, 0].astype(float)
S0, N = 39700, 600
T11 = (S0 + np.arange(N)) / 11025
K0, K1 = int(S0 * 48000 / 11025), int((S0 + N) * 48000 / 11025)
T48 = np.arange(K0, K1) / 48000
W_ID, W_FIX = ID[S0 : S0 + N], FIX48[K0:K1]
_seam = np.nonzero(ID[S0 : S0 + N] != FIX11[S0 : S0 + N])[0]
STEP = int(_seam[-1])  # last differing sample at 11025; the jump follows it
assert np.all(W_ID[STEP - 40 : STEP + 1] == W_ID[STEP]), "no flat run before the step"
TAGS = ["loop click", "48 kHz pitch", "fades above 100 fps", "StopSound's range"]
GREEN = "slime_light"

# ----------------------------------------------------------------- time ----

CUES = Cues(head=0.2, wave=0.5, tags=1.0, ring=3.0, hit=5.8).argv()
DURATION = duration_arg(7.6)

# --------------------------------------------------------------- layout ----

BX, BY, BW, BH = 40, 800, 1840, 260
WAVE_LABEL = "HUM1.WAV · A TELEPORTER'S HUM · 54 MS SHOWN" if TELEPORTER else "HUM1.WAV LOOPING, 54 MS SHOWN"
WX, WY, WW, WH = 380, 40, 1000, 170  # the waveform, in the band


def X(tv):
    return WX + (np.asarray(tv) - T11[0]) / (T11[-1] - T11[0]) * WW


def Y(v):
    return WY + WH / 2 - np.asarray(v) / 8000 * (WH / 2 - 8)


def frame(c, t):
    c.background()
    a = fade(t, 0.0, d_in=0.3)
    hit = ramp(t, CUES.hit, 0.25, "out")
    with boxed(c, (BX, BY, 1.0)):
        panel(c, 0, 0, BW, BH, a, dark=0.84)
        # the rate: 11025 Hz, struck through on the hit; 48000 Hz under it
        ha = a * ramp(t, CUES.head, 0.3)
        c.text("ID'S MIXER AT", 30, 40, 2, "dim", ha)
        hb = c.text("11025 HZ", 30, 72, 4, "rust", ha * (1 - 0.45 * hit))
        if hit > 0:
            c.line(hb[0] - 6, hb[1] + 16, hb[0] - 6 + (hb[2] + 12) * hit, hb[1] + 16, "rust", 6, ha)
            c.text("48000 HZ", 30, 130, 4, GREEN, a * hit)
            c.text("THE DEVICE'S RATE", 30, 178, 2, "dim", a * hit)
        # the waveform
        c.rect(WX, WY, WW, WH, fill="panel", alpha=a)
        c.line(WX, WY + WH / 2, WX + WW, WY + WH / 2, "rule", 1, a)
        with c.clip(WX, WY, WW, WH):
            c.polyline(list(zip(X(T11), Y(W_ID))), 31, 4, a * (1 - hit), draw=ramp(t, CUES.wave, 0.6, "linear"))
            if hit > 0:
                c.polyline(list(zip(X(T48), Y(W_FIX))), GREEN, 4, a, draw=ramp(t, CUES.hit, 0.35, "out"))
        ra = ramp(t, CUES.ring, 0.25, "back") * (1 - ramp(t, CUES.hit, 0.2))
        if ra > 0:
            sx = float(X(T11[STEP] + 0.5 / 11025))
            sy = float((Y(W_ID[STEP]) + Y(W_ID[STEP + 1])) / 2)
            c.circle(sx, sy, 36 * min(1.2, ra), stroke=251, lw=5, alpha=a * min(1, ra))
            c.text("CLICK", sx + 50, sy - 60, 3, 251, a * min(1, ra))
        c.text(WAVE_LABEL, WX, WY + WH + 14, 2, "dim", a)
        # the four fixes
        c.text("FOUR FIXES", 1420, 24, 2, "dim", a * ramp(t, CUES.tags, 0.3))
        for i, s in enumerate(TAGS):
            ta = a * min(1.0, ramp(t, CUES.tags + 0.35 * i, 0.25, "back"))
            if ta > 0:
                y = 52 + 48 * i
                c.rect(1420, y, 390, 40, fill="panel", stroke="slime", lw=2, alpha=ta, radius=4)
                c.text(s.upper(), 1420 + 195, y + 20, 2, GREEN, ta, "center", "middle")


if __name__ == "__main__":
    main(frame, DURATION, "D14-mixer-teleporter" if TELEPORTER else "D14-mixer", CUES, "e1m1")
