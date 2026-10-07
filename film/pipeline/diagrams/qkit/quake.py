"""Quake's own files, read straight from id's shareware pak.

PAK (the archive), WAD2 (gfx.wad: conchars, the status bar's digits), qpic
(a 2-D picture: width, height, palette indices), the palette, and a small BSP
reader (planes, faces, texinfo, textures) for diagrams that need a real wall's
numbers. Everything is cached per process.

The pak, the repository and the quaketool binary are filmroot's (film/pipeline/filmroot.py):
`REPO` is the repository as checked out, `PAK0` id's shareware pak in it, `QUAKETOOL` the
release build (`cd quake-rs && cargo build --release --bin quaketool`).
"""

from __future__ import annotations

import functools
import os
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))  # film/pipeline: filmroot
import filmroot  # noqa: E402

REPO = filmroot.REPO
PAK0 = filmroot.PAK
QUAKETOOL = filmroot.QUAKETOOL


def need_quaketool() -> Path:
    """The quaketool binary, or a clear error saying how to build it."""
    if not QUAKETOOL.exists():
        raise SystemExit(f"{QUAKETOOL} is missing: build it with "
                         "`cd quake-rs && cargo build --release --bin quaketool`")
    return QUAKETOOL


# ---------------------------------------------------------------- PAK ----


@functools.lru_cache(maxsize=None)
def pak_directory(pak: str = str(PAK0)) -> dict[str, tuple[int, int]]:
    """name -> (offset, length) for every file in a PAK."""
    if not Path(pak).exists():
        raise SystemExit(f"{pak} is missing: the diagrams read id's shareware pak (PAK0.PAK) there")
    with open(pak, "rb") as f:
        magic, dirofs, dirlen = struct.unpack("<4sii", f.read(12))
        if magic != b"PACK":
            raise ValueError(f"{pak}: not a PAK")
        f.seek(dirofs)
        raw = f.read(dirlen)
    out = {}
    for i in range(dirlen // 64):
        name, pos, length = struct.unpack_from("<56sii", raw, i * 64)
        out[name.split(b"\0")[0].decode("latin-1")] = (pos, length)
    return out


@functools.lru_cache(maxsize=64)
def pak_file(name: str, pak: str = str(PAK0)) -> bytes:
    """One file out of the PAK, by its path inside it (e.g. 'gfx/palette.lmp')."""
    pos, length = pak_directory(pak)[name]
    with open(pak, "rb") as f:
        f.seek(pos)
        return f.read(length)


# ------------------------------------------------------------ palette ----


@functools.lru_cache(maxsize=None)
def palette() -> np.ndarray:
    """id's 256-colour palette, (256, 3) uint8, from gfx/palette.lmp."""
    return np.frombuffer(pak_file("gfx/palette.lmp"), dtype=np.uint8).reshape(256, 3).copy()


def pal(i: int) -> tuple[float, float, float]:
    """Palette index -> an (r, g, b) in 0..1, for cairo."""
    r, g, b = palette()[i]
    return (r / 255.0, g / 255.0, b / 255.0)


# --------------------------------------------------------------- WAD2 ----


@functools.lru_cache(maxsize=None)
def wad_lumps() -> dict[str, bytes]:
    """gfx.wad's lumps by lower-case name."""
    data = pak_file("gfx.wad")
    magic, num, ofs = struct.unpack_from("<4sii", data, 0)
    if magic != b"WAD2":
        raise ValueError("gfx.wad: not WAD2")
    out = {}
    for i in range(num):
        pos, disksize, size, _typ, _comp, _pad, name = struct.unpack_from("<iiibbh16s", data, ofs + 32 * i)
        out[name.split(b"\0")[0].decode("latin-1").lower()] = data[pos : pos + disksize]
    return out


def qpic(raw: bytes) -> np.ndarray:
    """A qpic (width, height, then indices) -> (h, w) uint8 indices."""
    w, h = struct.unpack_from("<ii", raw, 0)
    return np.frombuffer(raw, dtype=np.uint8, count=w * h, offset=8).reshape(h, w).copy()


def wad_pic(name: str) -> np.ndarray:
    return qpic(wad_lumps()[name.lower()])


def lmp_pic(name: str) -> np.ndarray:
    """A gfx/*.lmp picture from the pak (e.g. 'gfx/qplaque.lmp')."""
    return qpic(pak_file(name))


@functools.lru_cache(maxsize=None)
def conchars() -> np.ndarray:
    """The console font: (128, 128) indices, 16x16 glyphs of 8x8; index 0 is transparent."""
    raw = wad_lumps()["conchars"]
    return np.frombuffer(raw, dtype=np.uint8, count=128 * 128).reshape(128, 128).copy()


def indices_to_rgba(idx: np.ndarray, transparent: int | None = None) -> np.ndarray:
    """Palette indices -> (h, w, 4) uint8 RGBA (straight alpha)."""
    rgb = palette()[idx]
    a = np.full(idx.shape, 255, np.uint8)
    if transparent is not None:
        a[idx == transparent] = 0
    return np.dstack([rgb, a])


# ---------------------------------------------------------------- BSP ----

LUMP_ENTITIES, LUMP_PLANES, LUMP_TEXTURES, LUMP_VERTEXES, LUMP_VISIBILITY, LUMP_NODES, LUMP_TEXINFO = range(7)
LUMP_FACES, LUMP_LIGHTING, LUMP_CLIPNODES, LUMP_LEAFS, LUMP_MARKSURFACES, LUMP_EDGES, LUMP_SURFEDGES, LUMP_MODELS = range(
    7, 15
)


@dataclass
class Texture:
    name: str
    width: int
    height: int
    mips: list[np.ndarray]  # four levels of (h >> m, w >> m) indices


@dataclass
class Bsp:
    planes_n: np.ndarray  # (P, 3) normals
    planes_d: np.ndarray  # (P,) dists
    verts: np.ndarray  # (V, 3)
    edges: np.ndarray  # (E, 2)
    surfedges: np.ndarray  # (S,)
    faces: np.ndarray  # structured: planenum, side, firstedge, numedges, texinfo
    tex_vecs: np.ndarray  # (T, 2, 4): s and t: x, y, z, offset
    tex_miptex: np.ndarray  # (T,)
    textures: list[Texture | None]
    world_faces: range  # model 0's faces
    entities: str

    def face_polygon(self, fi: int) -> np.ndarray:
        """The face's vertices in order, (n, 3)."""
        f = self.faces[fi]
        se = self.surfedges[f["firstedge"] : f["firstedge"] + f["numedges"]]
        idx = np.where(se >= 0, self.edges[np.abs(se), 0], self.edges[np.abs(se), 1])
        return self.verts[idx]

    def face_plane(self, fi: int) -> tuple[np.ndarray, float]:
        """The face's front-facing plane (normal, dist): flipped when its side is set."""
        f = self.faces[fi]
        n, d = self.planes_n[f["planenum"]], float(self.planes_d[f["planenum"]])
        return (-n, -d) if f["side"] else (n, d)

    def face_texture(self, fi: int) -> Texture | None:
        return self.textures[self.tex_miptex[self.faces[fi]["texinfo"]]]

    def face_st(self, fi: int, points: np.ndarray) -> np.ndarray:
        """World points (..., 3) -> texture (s, t) in mip-0 texels, (..., 2)."""
        v = self.tex_vecs[self.faces[fi]["texinfo"]]
        return points @ v[:, :3].T + v[:, 3]

    def cast(self, origin, dirs: np.ndarray, faces=None) -> tuple[np.ndarray, np.ndarray]:
        """The nearest world face each ray meets, front side: (face index or -1, distance t).

        dirs (n, 3); a point is origin + t * dir. Brute force over the faces
        (a few thousand), fine for a row of pixels.
        """
        origin = np.asarray(origin, float)
        dirs = np.atleast_2d(dirs)
        best_t = np.full(len(dirs), np.inf)
        best_f = np.full(len(dirs), -1)
        for fi in faces if faces is not None else self.world_faces:
            n, dist = self.face_plane(fi)
            den = dirs @ n
            with np.errstate(divide="ignore", invalid="ignore"):
                t = (dist - origin @ n) / den
            ok = (den < 0) & (t > 0) & (t < best_t)
            if not ok.any():
                continue
            poly = self.face_polygon(fi)
            p = origin + t[:, None] * dirs
            sides = np.stack([np.cross(b - a, p - a) @ n for a, b in zip(poly, np.roll(poly, -1, axis=0))])
            inside = ok & ((sides <= 1e-3).all(axis=0) | (sides >= -1e-3).all(axis=0))
            best_t = np.where(inside, t, best_t)
            best_f = np.where(inside, fi, best_f)
        return best_f, best_t


@functools.lru_cache(maxsize=8)
def bsp(map_name: str) -> Bsp:
    """A map from the pak by name ('e1m6' or 'maps/e1m6.bsp')."""
    name = map_name if map_name.startswith("maps/") else f"maps/{map_name}.bsp"
    data = pak_file(name)
    version = struct.unpack_from("<i", data, 0)[0]
    if version != 29:
        raise ValueError(f"{name}: BSP version {version}")
    lumps = [struct.unpack_from("<ii", data, 4 + 8 * i) for i in range(15)]

    def lump(i: int) -> bytes:
        o, n = lumps[i]
        return data[o : o + n]

    pl = np.frombuffer(lump(LUMP_PLANES), dtype=np.dtype([("n", "<f4", 3), ("d", "<f4"), ("t", "<i4")]))
    verts = np.frombuffer(lump(LUMP_VERTEXES), dtype="<f4").reshape(-1, 3).astype(np.float64)
    edges = np.frombuffer(lump(LUMP_EDGES), dtype="<u2").reshape(-1, 2).astype(np.int64)
    surfedges = np.frombuffer(lump(LUMP_SURFEDGES), dtype="<i4").astype(np.int64)
    faces = np.frombuffer(
        lump(LUMP_FACES),
        dtype=np.dtype(
            [("planenum", "<u2"), ("side", "<i2"), ("firstedge", "<i4"), ("numedges", "<i2"), ("texinfo", "<i2"),
             ("styles", "u1", 4), ("lightofs", "<i4")]
        ),
    )
    ti = np.frombuffer(lump(LUMP_TEXINFO), dtype=np.dtype([("vecs", "<f4", (2, 4)), ("miptex", "<i4"), ("flags", "<i4")]))
    models = np.frombuffer(
        lump(LUMP_MODELS),
        dtype=np.dtype([("mins", "<f4", 3), ("maxs", "<f4", 3), ("origin", "<f4", 3), ("head", "<i4", 4),
                        ("visleafs", "<i4"), ("firstface", "<i4"), ("numfaces", "<i4")]),
    )
    tl = lump(LUMP_TEXTURES)
    textures: list[Texture | None] = []
    (count,) = struct.unpack_from("<i", tl, 0)
    for i in range(count):
        (ofs,) = struct.unpack_from("<i", tl, 4 + 4 * i)
        if ofs < 0:
            textures.append(None)
            continue
        nm, w, h, *mo = struct.unpack_from("<16sII4I", tl, ofs)
        mips = []
        for m in range(4):
            mw, mh = w >> m, h >> m
            mips.append(np.frombuffer(tl, dtype=np.uint8, count=mw * mh, offset=ofs + mo[m]).reshape(mh, mw).copy())
        textures.append(Texture(nm.split(b"\0")[0].decode("latin-1"), w, h, mips))
    m0 = models[0]
    return Bsp(
        planes_n=pl["n"].astype(np.float64),
        planes_d=pl["d"].astype(np.float64),
        verts=verts,
        edges=edges,
        surfedges=surfedges,
        faces=faces,
        tex_vecs=ti["vecs"].astype(np.float64),
        tex_miptex=ti["miptex"],
        textures=textures,
        world_faces=range(int(m0["firstface"]), int(m0["firstface"] + m0["numfaces"])),
        entities=lump(LUMP_ENTITIES).split(b"\0")[0].decode("latin-1"),
    )


# ------------------------------------------------------------- camera ----


def angle_vectors(pitch: float, yaw: float, roll: float) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """id's AngleVectors (mathlib.c): forward, right, up for angles in degrees."""
    p, y, r = np.radians([pitch, yaw, roll])
    sp, cp, sy, cy, sr, cr = np.sin(p), np.cos(p), np.sin(y), np.cos(y), np.sin(r), np.cos(r)
    forward = np.array([cp * cy, cp * sy, -sp])
    right = np.array([-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp])
    up = np.array([cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp])
    return forward, right, up


@dataclass
class Camera:
    """A software-renderer view: id's projection, pixel centres on the integers.

    `xcenter = w/2 - 0.5`; screen x = xcenter + xscale * (right . d) / (forward . d),
    screen y = ycenter - yscale * (up . d) / (forward . d). `tan_half_x` is the
    horizontal half-angle's tangent (Hor+ at 16:9 with fov 90: 0.75 * 16/9).
    """

    origin: np.ndarray
    angles: tuple[float, float, float]
    width: int
    height: int
    tan_half_x: float

    @classmethod
    def horplus(cls, origin, angles, width: int, height: int, fov: float = 90.0) -> "Camera":
        """The slop preset's Hor+: `fov` spans a 4:3 screen; a wider one sees more at the sides."""
        tan_v = np.tan(np.radians(fov) / 2) * 0.75
        return cls(np.asarray(origin, float), tuple(angles), width, height, tan_v * width / height)

    @property
    def xscale(self) -> float:
        return (self.width / 2) / self.tan_half_x

    @property
    def yscale(self) -> float:
        return self.xscale  # square pixels

    def ray(self, x: np.ndarray | float, y: np.ndarray | float) -> np.ndarray:
        """World directions through pixel centres (x, y): (..., 3), forward component 1."""
        f, r, u = angle_vectors(*self.angles)
        xc, yc = self.width / 2 - 0.5, self.height / 2 - 0.5
        a = (np.asarray(x, float) - xc) / self.xscale
        b = -(np.asarray(y, float) - yc) / self.yscale
        return f + a[..., None] * r + b[..., None] * u

    def depth(self, points: np.ndarray) -> np.ndarray:
        """z: the distance along the view's forward axis."""
        f, _, _ = angle_vectors(*self.angles)
        return (points - self.origin) @ f


# ---------------------------------------------------- engine frames ----

def game_frame(command: str, map_name: str, *args: str) -> np.ndarray:
    """A frame from the engine itself, (h, w, 3) uint8, cached on disk by its arguments.

        game_frame("view", "e1m6", "--res", "1920x1080", "--origin", "504,500,242",
                   "--angles", "0,100,0", "--video", "modern", "--perspspan", "16")

    command is `view` (the 3-D view alone, the oracle's camera) or `shot` (the
    screen as a player sees it, with the gun and status bar); args are
    quaketool's own (run it with no arguments for the list). The cache is
    filmroot.scratch("qkit"), keyed by the arguments alone: empty it after
    rebuilding quaketool with a change that alters its pictures.
    """
    import hashlib
    import subprocess

    from PIL import Image

    m = map_name if map_name.startswith("maps/") else f"maps/{map_name}.bsp"
    key = hashlib.sha1(" ".join([command, m, *args]).encode()).hexdigest()[:16]
    cache = filmroot.scratch("qkit")
    png = cache / f"{command}-{Path(m).stem}-{key}.png"
    if not png.exists():
        ppm = cache / f"{key}.{os.getpid()}.ppm"
        subprocess.run([str(need_quaketool()), command, str(PAK0), m, str(ppm), *args], check=True,
                       stdout=subprocess.DEVNULL)
        Image.open(ppm).convert("RGB").save(png)
        ppm.unlink()
    return np.asarray(Image.open(png).convert("RGB"))
