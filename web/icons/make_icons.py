#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# ///
"""The page's icons: the home-screen app's (manifest.webmanifest, iOS's
apple-touch-icon) and the favicon. Original art, not id's: a torch flame in
a bronze bowl on dark stone, drawn as 32x32 pixel art and scaled by whole
cells, nearest neighbour, as the game draws its own pixels. The flame and
bowl stay inside the middle 80% circle, so a launcher may crop the icon to
any mask ("maskable").

Standard library only (zlib and struct write the PNGs). Run it from
anywhere; it writes beside itself:

    uv run web/icons/make_icons.py

and prints the favicon as the data: URI index.html inlines.
"""
import base64
import os
import struct
import zlib

# The palette: two stones, a glow, the flame from its edge to its core, a
# bronze bowl in three tones (the page's --bronze is 8).
PALETTE = {
    "s": (0x0E, 0x0B, 0x08),  # stone
    "t": (0x18, 0x12, 0x0C),  # stone, lighter
    "g": (0x2C, 0x14, 0x0A),  # glow, two cells out
    "h": (0x46, 0x1C, 0x0C),  # glow, one cell out
    "2": (0x8A, 0x2A, 0x0C),  # flame edge
    "3": (0xC4, 0x50, 0x1A),
    "4": (0xE8, 0x8A, 0x2A),
    "5": (0xF5, 0xC1, 0x50),
    "6": (0xFF, 0xF0, 0xB0),  # flame core
    "7": (0x5A, 0x3C, 0x16),  # bronze, shade
    "8": (0xB5, 0x83, 0x2F),  # bronze
    "9": (0xD9, 0xA5, 0x46),  # bronze, light
}

# The flame and its bowl, 16 cells wide; "." is stone.
ART = """
........3.......
.......33.......
.......343......
......2343......
......23443.....
.....234443.....
.....2345443....
....23455443....
....234555443...
...2345565443...
...2345666543...
...2345666543...
....23455543....
.....234443.....
......2332......
...9988888877...
....98888877....
.....988877.....
.......77.......
""".strip().splitlines()

SIZE = 32  # the art's grid


def master():
    """The 32x32 icon as rows of palette keys."""
    grid = [["s"] * SIZE for _ in range(SIZE)]
    # Stone: a fixed scatter of lighter cells (a small LCG, so every run
    # draws the same stones).
    seed = 1996
    for y in range(SIZE):
        for x in range(SIZE):
            seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
            if (seed >> 16) % 10 < 3:
                grid[y][x] = "t"
    top, left = (SIZE - len(ART)) // 2, (SIZE - len(ART[0])) // 2
    fire = set()
    for y, row in enumerate(ART):
        for x, c in enumerate(row):
            if c != ".":
                grid[top + y][left + x] = c
                if c in "23456":
                    fire.add((top + y, left + x))
    # The glow: stone within two cells of the flame warms, nearer more.
    for y in range(SIZE):
        for x in range(SIZE):
            if grid[y][x] not in "st":
                continue
            d = min((max(abs(y - fy), abs(x - fx)) for fy, fx in fire), default=99)
            if d == 1:
                grid[y][x] = "h"
            elif d == 2:
                grid[y][x] = "g"
    return grid


def png(size, grid):
    """`grid` scaled to `size` x `size` by nearest neighbour, as PNG bytes."""
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter: none
        row = grid[y * SIZE // size]
        for x in range(size):
            raw.extend(PALETTE[row[x * SIZE // size]])

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)  # 8-bit RGB
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    grid = master()
    for name, size in [("icon-512.png", 512), ("icon-192.png", 192),
                       ("apple-touch-icon.png", 180), ("favicon-32.png", 32)]:
        with open(os.path.join(here, name), "wb") as f:
            f.write(png(size, grid))
        print("wrote", name)
    print("data:image/png;base64," + base64.b64encode(png(32, grid)).decode())


if __name__ == "__main__":
    main()
