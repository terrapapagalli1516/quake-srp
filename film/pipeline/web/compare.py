# /// script
# requires-python = ">=3.12"
# dependencies = ["pillow", "numpy"]
# ///
"""Compare a web clip with another (a re-capture with the film's own, a 2x capture with a 1x
one): PSNR over every frame, and a strip of chosen frames side by side with their difference.

    uv run film/pipeline/web/compare.py NEW.mp4 OLD.mp4 [--frames 0,120,300] [--strip OUT.png]

When the two differ in size the larger is scaled down to the smaller first (area average), so
a 2x capture is compared with its 1x original as a viewer at 1x would see it.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


def probe(p: Path) -> tuple[int, int, int]:
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                          "stream=width,height,nb_read_packets", "-of", "csv=p=0", str(p)],
                         capture_output=True, text=True, check=True).stdout.strip().split(",")
    return int(out[0]), int(out[1]), int(out[2])


def frames(p: Path, w: int, h: int):
    """Every frame of `p` as an RGB array, scaled to w x h (area) if it is another size."""
    cmd = ["ffmpeg", "-v", "error", "-i", str(p), "-vf", f"scale={w}:{h}:flags=area", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE)
    n = w * h * 3
    while True:
        b = proc.stdout.read(n)
        if len(b) < n:
            break
        yield np.frombuffer(b, np.uint8).reshape(h, w, 3)
    proc.wait()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("new", type=Path)
    ap.add_argument("old", type=Path)
    ap.add_argument("--frames", default="", help="frames to show side by side (comma separated)")
    ap.add_argument("--strip", type=Path, help="where to write the strip (PNG)")
    a = ap.parse_args()
    (wn, hn, nn), (wo, ho, no) = probe(a.new), probe(a.old)
    w, h = min(wn, wo), min(hn, ho)
    pick = [int(x) for x in a.frames.split(",") if x]
    shown = {}
    psnr = []
    for i, (fa, fb) in enumerate(zip(frames(a.new, w, h), frames(a.old, w, h))):
        mse = np.mean((fa.astype(np.float32) - fb.astype(np.float32)) ** 2)
        psnr.append(99.0 if mse == 0 else 10 * np.log10(255 ** 2 / mse))
        if i in pick:
            shown[i] = (fa.copy(), fb.copy())
    p = np.array(psnr)
    print(f"{a.new.name}: {wn}x{hn}, {nn} frames; {a.old.name}: {wo}x{ho}, {no} frames; compared at {w}x{h}")
    print(f"PSNR over {len(p)} frames: mean {p.mean():.2f} dB, median {np.median(p):.2f}, "
          f"min {p.min():.2f} (frame {int(p.argmin())}), frames under 35 dB: {int((p < 35).sum())}")
    if a.strip and shown:
        tw = 640
        th = round(h * tw / w)
        sheet = Image.new("RGB", (tw * 3, th * len(shown)), (32, 32, 32))
        for r, i in enumerate(sorted(shown)):
            fa, fb = shown[i]
            d = np.clip(np.abs(fa.astype(np.int16) - fb.astype(np.int16)) * 4, 0, 255).astype(np.uint8)
            for c, arr in enumerate((fa, fb, d)):
                im = Image.fromarray(arr).resize((tw, th), Image.LANCZOS)
                if c == 0:
                    ImageDraw.Draw(im).text((6, 6), f"frame {i}  PSNR {psnr[i]:.1f}", fill=(255, 255, 0))
                sheet.paste(im, (c * tw, r * th))
        sheet.save(a.strip)
        print("strip (new | old | difference x4):", a.strip)
    return 0


if __name__ == "__main__":
    sys.exit(main())
