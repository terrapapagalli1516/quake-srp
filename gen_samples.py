#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Generate synthetic Quake asset files (independent of the Rust impl) so we can
exercise the `quaketool` reader end-to-end. Byte layouts follow the id source."""

import struct
import math
import os
import sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "samples"
os.makedirs(OUT, exist_ok=True)

def i32(v): return struct.pack("<i", v)
def u32(v): return struct.pack("<I", v)
def f32(v): return struct.pack("<f", v)
def vec3(x, y, z): return f32(x) + f32(y) + f32(z)
def name_field(s, n):
    b = s.encode()[:n]
    return b + b"\x00" * (n - len(b))

# ---- BSP v29 (header + entities + a ring of vertices for the minimap) -------
def build_bsp():
    verts = []
    # A ring + diagonals, so the top-down minimap shows a recognizable shape.
    for k in range(48):
        a = 2 * math.pi * k / 48
        verts.append((512 + 480 * math.cos(a), 512 + 480 * math.sin(a), 0.0))
    for t in range(20):
        verts.append((32 + t * 48.0, 32 + t * 48.0, 16.0))     # diagonal
        verts.append((992 - t * 48.0, 32 + t * 48.0, 16.0))    # anti-diagonal
    ents = (b'{\n"classname" "worldspawn"\n"wad" "gfx.wad"\n"message" "Rust Test Map"\n}\n'
            b'{\n"classname" "info_player_start"\n"origin" "512 512 24"\n}\n'
            b'{\n"classname" "light"\n"origin" "256 256 64"\n}\n'
            b'{\n"classname" "monster_army"\n"origin" "700 300 24"\n}\n'
            b'{\n"classname" "light"\n"origin" "768 768 64"\n}\n\x00')
    vert_bytes = b"".join(vec3(*v) for v in verts)

    HEADER = 124
    ent_ofs = HEADER
    vert_ofs = ent_ofs + len(ents)
    lumps = [(0, 0)] * 15
    lumps[0] = (ent_ofs, len(ents))      # LUMP_ENTITIES
    lumps[3] = (vert_ofs, len(vert_bytes))  # LUMP_VERTEXES

    out = i32(29)
    for o, l in lumps:
        out += i32(o) + i32(l)
    out += ents + vert_bytes
    return out

# ---- MDL alias model --------------------------------------------------------
def build_mdl():
    SW, SH, NV, NT, NF, NS = 4, 4, 3, 1, 1, 1
    out = b""
    out += b"IDPO"            # ident
    out += i32(6)             # version
    out += vec3(1, 1, 1)      # scale
    out += vec3(0, 0, 0)      # scale_origin
    out += f32(10.0)          # boundingradius
    out += vec3(0, 0, 2)      # eyeposition
    out += i32(NS) + i32(SW) + i32(SH)
    out += i32(NV) + i32(NT) + i32(NF)
    out += i32(0)             # synctype ST_SYNC
    out += i32(0)             # flags
    out += f32(1.0)           # size
    # skins: type=0 single, SW*SH bytes
    out += i32(0) + bytes(range(SW * SH))
    # stverts: NV * (onseam, s, t)
    for k in range(NV):
        out += i32(0) + i32(k * 2) + i32(k * 3)
    # triangles: NT * (facesfront, vertindex[3])
    out += i32(1) + i32(0) + i32(1) + i32(2)
    # frames: type=0 single; bboxmin(4)+bboxmax(4)+name[16]; NV trivertx(4)
    out += i32(0)
    out += bytes([0, 0, 0, 0]) + bytes([255, 255, 255, 0]) + name_field("frame1", 16)
    for k in range(NV):
        out += bytes([k * 10, k * 11, k * 12, 0])
    return out

# ---- SPR sprite -------------------------------------------------------------
def build_spr():
    W, H, NF = 2, 2, 1
    out = b"IDSP"
    out += i32(1)             # version
    out += i32(2)             # type SPR_VP_PARALLEL
    out += f32(5.0)           # boundingradius
    out += i32(W) + i32(H) + i32(NF)
    out += f32(0.0)           # beamlength
    out += i32(0)             # synctype
    # one SPR_SINGLE frame: type=0, origin[2], w, h, then W*H pixels
    out += i32(0)
    out += i32(-1) + i32(1)   # origin
    out += i32(W) + i32(H)
    out += bytes([1, 2, 3, 4])
    return out

# ---- WAD2 (palette + a tiny qpic) ------------------------------------------
def build_wad():
    palette = bytes((i % 256) for i in range(768))
    qpic = i32(2) + i32(2) + bytes([9, 8, 7, 6])  # width, height, 4 px
    HEADER = 12
    data = palette + qpic
    infotableofs = HEADER + len(data)
    lumps = b""
    # lumpinfo: filepos, disksize, size, type, compression, pad1, pad2, name[16]
    lumps += i32(HEADER) + i32(len(palette)) + i32(len(palette)) + bytes([64, 0, 0, 0]) + name_field("PALETTE", 16)
    lumps += i32(HEADER + len(palette)) + i32(len(qpic)) + i32(len(qpic)) + bytes([66, 0, 0, 0]) + name_field("DISC", 16)
    out = b"WAD2" + i32(2) + i32(infotableofs) + data + lumps
    return out

# ---- PAK (contains the BSP + some text) ------------------------------------
def build_pak(files):
    HEADER = 12
    body = b""
    offsets = []
    for _, data in files:
        offsets.append(HEADER + len(body))
        body += data
    directory = b""
    for (nm, data), ofs in zip(files, offsets):
        directory += name_field(nm, 56) + i32(ofs) + i32(len(data))
    dirofs = HEADER + len(body)
    return b"PACK" + i32(dirofs) + i32(len(directory)) + body + directory

bsp = build_bsp()
mdl = build_mdl()
spr = build_spr()
wad = build_wad()

def write(p, b):
    with open(os.path.join(OUT, p), "wb") as f:
        f.write(b)
    print(f"  wrote {p:18} {len(b):>7} bytes")

write("demo.bsp", bsp)
write("soldier.mdl", mdl)
write("flame.spr", spr)
write("gfx.wad", wad)

pak = build_pak([
    ("maps/demo.bsp", bsp),
    ("gfx.wad", wad),
    ("progs/soldier.mdl", mdl),
    ("readme.txt", b"This PAK was generated by gen_samples.py to test quaketool.\n"),
])
write("pak0.pak", pak)
print("done.")
