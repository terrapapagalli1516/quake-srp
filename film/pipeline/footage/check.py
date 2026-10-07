#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy", "pillow"]
# ///
"""Look at the footage stage's work: against a reference footage folder, or as contact sheets.

    uv run film/pipeline/footage/check.py diff REF/game NEW/game S01 S45 ...   frame by frame
    uv run film/pipeline/footage/check.py sheet NEW/game OUT_DIR S01 S45 ...    contact sheets

`diff` decodes both files of each name (NAME.mp4, at the reference's size: a larger render is
area-averaged down to it first, `--scaled`) and counts, frame by frame, the pixels whose largest
channel differs by more than --threshold levels (the encoders' own noise stays under it on
identical pictures; 0 counts every change); it also compares the game sound's samples and the
event logs' sounds. `sheet` draws twelve frames of each file in a grid, with the shot's time,
and a 1:1 crop of the middle frame (the pixels and the lettering at their own size).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import wave
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


def probe(path: Path) -> tuple[int, int, int]:
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                          "stream=width,height,nb_read_packets", "-of", "csv=p=0", str(path)],
                         capture_output=True, text=True, check=True).stdout.strip().split(",")
    return int(out[0]), int(out[1]), int(out[2])


def decoder(path: Path, w: int, h: int, scaled: bool):
    vf = f"scale={w}:{h}:flags=area" if scaled else "null"
    return subprocess.Popen(["ffmpeg", "-v", "error", "-i", str(path), "-vf", vf, "-f", "rawvideo", "-pix_fmt",
                             "rgb24", "-"], stdout=subprocess.PIPE)


def read(p, w: int, h: int) -> np.ndarray | None:
    n = w * h * 3
    buf = p.stdout.read(n)
    if len(buf) < n:
        return None
    return np.frombuffer(buf, np.uint8).reshape(h, w, 3)


def md5(path: Path) -> str:
    return hashlib.md5(path.read_bytes()).hexdigest()


def wav_samples(path: Path) -> tuple[int, int, bytes]:
    with wave.open(str(path)) as w:
        return w.getframerate(), w.getnchannels(), w.readframes(w.getnframes())


def sounds(path: Path) -> list:
    ev = json.loads(path.read_text())
    return [(e.get("frame"), e.get("sample")) for e in ev.get("events", []) if e.get("kind") == "sound"]


def diff_one(ref: Path, new: Path, name: str, thr: int) -> dict:
    a, b = ref / f"{name}.mp4", new / f"{name}.mp4"
    rw, rh, rn = probe(a)
    nw, nh, nn = probe(b)
    scaled = (nw, nh) != (rw, rh)
    r = {"name": name, "frames": [rn, nn], "size": [[rw, rh], [nw, nh]], "same_file": md5(a) == md5(b)}
    pa, pb = decoder(a, rw, rh, False), decoder(b, rw, rh, scaled)
    same = 0
    shares, worst = [], (0.0, -1)
    k = 0
    while (fa := read(pa, rw, rh)) is not None and (fb := read(pb, rw, rh)) is not None:
        d = np.abs(fa.astype(np.int16) - fb.astype(np.int16)).max(axis=2)
        if not d.any():
            same += 1
        share = float((d > thr).mean())
        shares.append(share)
        if share > worst[0]:
            worst = (share, k)
        k += 1
    pa.stdout.close(), pb.stdout.close()
    pa.wait(), pb.wait()
    mean = round(100 * float(np.mean(shares)), 4) if shares else None
    r.update(compared=k, identical_frames=same, mean_pct_over=mean,
             worst_pct_over=round(100 * worst[0], 4), worst_frame=worst[1])
    base = name.split(".")[0] if name.endswith((".clean", ".zoom")) else name
    for suf in (".wav",):
        if (ref / f"{base}{suf}").exists():
            if not (new / f"{base}{suf}").exists():
                r["wav"] = "missing"
                continue
            ra, rb = wav_samples(ref / f"{base}{suf}"), wav_samples(new / f"{base}{suf}")
            r["wav"] = "same samples" if ra == rb else (f"differ: {ra[0]} Hz {len(ra[2])} B against {rb[0]} Hz "
                                                        f"{len(rb[2])} B")
    if (ref / f"{base}.events.json").exists():
        if (new / f"{base}.events.json").exists():
            sa, sb = sounds(ref / f"{base}.events.json"), sounds(new / f"{base}.events.json")
            r["sound_events"] = "same" if sa == sb else f"differ ({len(sa)} against {len(sb)})"
        else:
            r["sound_events"] = "missing"
    return r


def sheet_one(game: Path, out: Path, name: str, cols: int = 4, rows: int = 3) -> Path:
    mp4 = game / f"{name}.mp4"
    w, h, n = probe(mp4)
    side = game / f"{name.split('.')[0] if name.endswith(('.clean', '.zoom')) else name}.json"
    head = round(json.loads(side.read_text()).get("head_s", 1.0) * 60) if side.exists() else 60
    k = cols * rows
    picks = sorted({round(i * (n - 1) / (k - 1)) for i in range(k)})
    tw = 960
    th = round(h * tw / w)
    p = decoder(mp4, w, h, False)
    tiles, mid, i = {}, None, 0
    want_mid = picks[len(picks) // 2]
    while (f := read(p, w, h)) is not None:
        if i in picks:
            tiles[i] = Image.fromarray(f).resize((tw, th), Image.Resampling.BOX)
        if i == want_mid:
            mid = f.copy()
        i += 1
    p.stdout.close()
    p.wait()
    img = Image.new("RGB", (cols * (tw + 6), rows * (th + 6) + 30), (24, 24, 24))
    d = ImageDraw.Draw(img)
    d.text((8, 8), f"{name}: {w}x{h}, {n} frames (head {head})", fill=(255, 220, 120))
    for j, f in enumerate(picks):
        if f not in tiles:
            continue
        t = tiles[f]
        dd = ImageDraw.Draw(t)
        tag = f"#{f}  shot {(f - head) / 60:+.2f}s"
        dd.rectangle((0, 0, 7 * len(tag) + 6, 14), fill=(0, 0, 0))
        dd.text((3, 2), tag, fill=(255, 255, 0))
        r, c = divmod(j, cols)
        img.paste(t, (c * (tw + 6), 30 + r * (th + 6)))
    out.mkdir(parents=True, exist_ok=True)
    path = out / f"{name}.png"
    img.save(path)
    if mid is not None:  # 1:1: a quarter of the frame, its top left (where most labels sit)
        Image.fromarray(mid[: h // 2, : w // 2]).save(out / f"{name}-1to1.png")
    return path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("diff")
    d.add_argument("ref", type=Path)
    d.add_argument("new", type=Path)
    d.add_argument("names", nargs="+")
    d.add_argument("--threshold", type=int, default=24, help="levels a channel may differ by before a pixel counts")
    d.add_argument("--json", type=Path, help="also write the results here")
    s = sub.add_parser("sheet")
    s.add_argument("game", type=Path)
    s.add_argument("out", type=Path)
    s.add_argument("names", nargs="+")
    a = ap.parse_args()
    if a.cmd == "sheet":
        for n in a.names:
            print(sheet_one(a.game, a.out, n), flush=True)
        return 0
    results = []
    for n in a.names:
        r = diff_one(a.ref, a.new, n, a.threshold)
        results.append(r)
        print(json.dumps(r), flush=True)
    if a.json:
        a.json.write_text(json.dumps(results, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
