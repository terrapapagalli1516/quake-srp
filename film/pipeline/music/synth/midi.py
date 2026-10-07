"""Standard MIDI File (type 1) writer for a resolved Score.

The MIDI file carries the notes on the musical grid with the *effective* tempo map
(anchor stretches included), so it lines up with the rendered WAV in any DAW.
Pitches round to the nearest equal-tempered MIDI note at A4=440 (a score tuned
elsewhere, like D2=72 Hz, plays a fraction of a semitone off in a GM synth).
Percussion patches go to channel 10 with General MIDI drum notes.
"""
from __future__ import annotations

import math
import struct

GM_PROGRAM = {"drone": 95, "air": 122, "pad": 89, "bell": 14, "strike": 114, "glass": 8,
              "bass": 38, "brass": 62, "swell": 119}
GM_DRUM = {"impact": 35, "hit": 40, "clank": 56, "tick": 37, "ticks": 42}


def _vlq(n):
    out = [n & 0x7F]
    n >>= 7
    while n:
        out.append(0x80 | (n & 0x7F))
        n >>= 7
    return bytes(reversed(out))


def _track(events):
    """events: list of (tick, order, bytes). Returns an MTrk chunk."""
    events.sort(key=lambda e: (e[0], e[1]))
    data = bytearray()
    last = 0
    for tick, _, msg in events:
        data += _vlq(tick - last) + msg
        last = tick
    data += _vlq(0) + b"\xff\x2f\x00"
    return b"MTrk" + struct.pack(">I", len(data)) + bytes(data)


def _meta(kind, payload):
    return bytes([0xFF, kind]) + _vlq(len(payload)) + payload


def write_midi(score, path, ppq=480):
    tm = score.tmap
    end_beat = max((tm.beat(n.t1) for n in score.notes), default=0.0) + 4
    # conductor: tempo every quarter beat where it changes, time signatures
    conductor = [(0, 0, _meta(0x03, (score.title or "score").encode()))]
    last_us = None
    b = 0.0
    while b < end_beat:
        spb = (tm.sec(b + 0.25) - tm.sec(b)) / 0.25
        us = int(round(spb * 1e6))
        if last_us is None or abs(us - last_us) > last_us * 1e-4:
            conductor.append((int(round(b * ppq)), 1, _meta(0x51, us.to_bytes(3, "big"))))
            last_us = us
        b += 0.25
    for bar, (num, den) in sorted(score.meters.items()):
        tick = int(round(float(score.bar_start(bar)) * ppq))
        conductor.append((tick, 0, _meta(0x58, bytes([num, int(math.log2(den)), 24, 8]))))
    chunks = [_track(conductor)]
    chan = 0
    for name, tr in score.tracks.items():
        notes = [n for n in score.notes if n.track == name]
        if not notes:
            continue
        drum = tr.patch in GM_DRUM
        if drum:
            ch = 9
        else:
            ch = chan
            chan += 1
            if chan == 9:
                chan += 1
            ch %= 16
        ev = [(0, 0, _meta(0x03, name.encode()))]
        if not drum:
            ev.append((0, 1, bytes([0xC0 | ch, GM_PROGRAM.get(tr.patch, 0)])))
        for n in notes:
            if drum:
                key = GM_DRUM[tr.patch]
            elif n.freq:
                key = int(round(69 + 12 * math.log2(n.freq / 440.0)))
            else:
                key = 60
            key = max(0, min(127, key))
            vel = max(1, min(127, int(round(n.vel * 127))))
            t0 = int(round(tm.beat(n.t0) * ppq))
            t1 = max(t0 + 1, int(round(tm.beat(n.t1) * ppq)))
            ev.append((max(t0, 0), 3, bytes([0x90 | ch, key, vel])))
            ev.append((max(t1, 1), 2, bytes([0x80 | ch, key, 0])))
        chunks.append(_track(ev))
    head = b"MThd" + struct.pack(">IHHH", 6, 1, len(chunks), ppq)
    with open(path, "wb") as f:
        f.write(head + b"".join(chunks))
