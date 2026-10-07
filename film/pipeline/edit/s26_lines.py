#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy", "pillow"]
# ///
"""S26's check lines: five full-frame transparent PNGs, each line of the proof's checks over demo1.

    uv run film/pipeline/edit/s26_lines.py [--scale 2]   # FILM_ROOT/edit/v5/art/S26-line0..4.png (the path film/edit.toml names)

Each line is in Quake's own lettering, drawn by `quaketool filmtext` (id's conchars from the shareware pak),
at 3x, in capitals (Quake's lower case is small capitals), on an opaque band at the top right of the 4:3 box.
Quake's charset has no "·" and no tick: both are drawn here, in the conchars' manner. The edit brings each
line in on its word (film/edit.toml, [event_lists] s26_lines). Needs quaketool built
(`cargo build --release --bin quaketool` in quake-rs/) and the pak at quake-data/ID1/PAK0.PAK.
"""

from __future__ import annotations

import argparse
import io
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from filmroot import FILM, PAK, QUAKETOOL  # noqa: E402

W, H = 1920, 1080
BRONZE = (0xAF, 0x63, 0x2F)
CHECK = ["........", ".......#", "......##", "#....##.", "##..##..", ".####...", "..##....", "........"]
DOT = ["........", "........", "........", "...##...", "...##...", "........", "........", "........"]
LINES = [("SOUND: SAMPLE FOR SAMPLE · 28 OF 28", False), ("FRAME BY FRAME: 17,500 FRAMES", False), ("CAMERA ", True),
         ("EVERY ENTITY ", True), ("EVERY DYNAMIC LIGHT ", True)]


def label_image(text: str, scale: int = 3, font: str = "gold") -> np.ndarray:
    """`text` in Quake's lettering (quaketool filmtext) as an RGBA array."""
    with tempfile.TemporaryDirectory(prefix="s26-") as d:
        out = Path(d) / "label.png"
        subprocess.run([str(QUAKETOOL), "filmtext", str(PAK), str(out), text, "--font", font, "--scale", str(scale)],
                       check=True, capture_output=True)
        return np.asarray(Image.open(out).convert("RGBA"))


def glyph(rows: list[str], scale: int = 3) -> np.ndarray:
    """A drawn 8x8 sign, in bronze with the conchars' one-pixel black shadow."""
    g = np.array([[c == "#" for c in row] for row in rows], bool).repeat(scale, 0).repeat(scale, 1)
    out = np.zeros((g.shape[0] + scale, g.shape[1] + scale, 4), np.uint8)
    out[scale:, scale:][g] = (0, 0, 0, 255)
    out[:g.shape[0], :g.shape[1]][g] = (*BRONZE, 255)
    return out


def text(s: str) -> list[np.ndarray]:
    parts = []
    for k, piece in enumerate(s.split("·")):
        if k:
            parts.append(glyph(DOT))
        if piece:
            parts.append(label_image(piece, 3, "gold"))
    return parts


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", type=Path, default=FILM / "edit" / "v5" / "art", help="where the PNGs go")
    ap.add_argument("--scale", type=int, default=1, help="the frame is SCALE x 1920x1080: each line drawn at 1080, "
                    "then every pixel repeated SCALE x SCALE times (Quake's lettering at SCALE x its size, exactly)")
    a = ap.parse_args()
    if not QUAKETOOL.exists() or not PAK.exists():
        sys.exit(f"s26_lines.py: needs {QUAKETOOL} (cargo build --release --bin quaketool) and {PAK}")
    right, top, step = 1680 - 24, 24, 44
    a.out.mkdir(parents=True, exist_ok=True)
    for i, (s, tick) in enumerate(LINES):
        parts = text(s) + ([glyph(CHECK)] if tick else [])
        w = sum(p.shape[1] for p in parts)
        h = max(p.shape[0] for p in parts)
        img = np.zeros((H, W, 4), np.uint8)
        x0, y0 = right - w, top + i * step
        img[y0 - 8:y0 + h + 6, x0 - 12:right + 12] = (0, 0, 0, 255)      # an opaque band
        x = x0
        for p in parts:
            region = img[y0:y0 + p.shape[0], x:x + p.shape[1]]
            al = p[..., 3:4].astype(np.float32) / 255
            region[..., :3] = (region[..., :3] * (1 - al) + p[..., :3] * al).astype(np.uint8)
            region[..., 3] = np.maximum(region[..., 3], p[..., 3])
            x += p.shape[1]
        if a.scale > 1:
            img = img.repeat(a.scale, 0).repeat(a.scale, 1)
        buf = io.BytesIO()
        Image.fromarray(img, "RGBA").save(buf, "PNG")
        dst = a.out / f"S26-line{i}.png"
        if not dst.exists() or dst.read_bytes() != buf.getvalue():  # unchanged files keep their time: the edit's
            dst.write_bytes(buf.getvalue())                         # segment cache goes by it
        print(f"S26-line{i}.png", s, x0, y0)


if __name__ == "__main__":
    main()
