"""The footage stage's shared parts: `quaketool film` streams, the encoder, the event logs and
sidecars, and the drawing the composites use, at any whole-number scale of the film's 1080p.

Every composite was laid out at 1920x1080. At a larger size (3840x2160 is scale 2) each
render is made natively at the shot's own size times the scale (`--size` given last, as an
override), and every position, crop, line and letter of the layout is the 1080p one times
the scale: lengths are rounded at 1080p and then multiplied, so the geometry is exactly 2x,
3x... of v7's, and the glyphs are Quake's conchars at twice their scale. At scale 1 the
composites are the production's own, operation for operation, so their frames are v7's.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # film/pipeline: filmroot
import filmroot  # noqa: E402

REPO = filmroot.REPO
SHOTS = REPO / "film" / "shots"
SIDECARS = SHOTS / "sidecars"
FPS = 60
HEAD = 1.0

# The film's colours: Quake palette entries.
BRONZE = (0xAF, 0x63, 0x2F)  # 106
RUST = (0x9F, 0x4F, 0x33)  # 105
LAVA = (0xDB, 0x7F, 0x3B)  # 235
DEEP_LAVA = (0xC3, 0x4B, 0x1B)  # 233
PALE = (0xEF, 0xBF, 0x77)  # 238
WHITE = (0xFF, 0xFF, 0xFF)  # 254

VAAPI_DEVICE = "/dev/dri/renderD128"
VAAPI_QP = 16  # hevc_vaapi, constant QP: at 4K within 0.3 dB of a lossless 4:2:0 encode on Classic's pixels


def smooth(u: float) -> float:
    u = min(1.0, max(0.0, u))
    return u * u * (3 - 2 * u)


def frames_of(seconds: float) -> int:
    return round(seconds * FPS)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------------------
# shot files
# ---------------------------------------------------------------------------


def shot_path(name: str) -> Path:
    return SHOTS / f"{name}.shot"


def shot_lines(path: Path) -> list[str]:
    """The lines `quaketool film` reads: a `#` that starts a word starts a comment."""
    out = []
    for raw in path.read_text().splitlines():
        words = []
        for w in raw.split():
            if w.startswith("#"):
                break
            words.append(w)
        if words:
            out.append(" ".join(words))
    return out


@dataclass
class ShotInfo:
    size: tuple[int, int] = (1920, 1080)
    fps: float = 60.0
    frames: tuple[int, int] | None = None


def shot_info(path: Path, extra: list[str] = ()) -> ShotInfo:
    info = ShotInfo()
    for line in shot_lines(path) + list(extra):
        key, _, rest = line.partition(" ")
        if key == "size":
            w, h = rest.lower().split("x")
            info.size = (int(w), int(h))
        elif key == "fps":
            info.fps = float(rest)
        elif key == "frames":
            a, b = rest.split("..")
            info.frames = (int(a), int(b))
    return info


# ---------------------------------------------------------------------------
# a render's context
# ---------------------------------------------------------------------------


@dataclass
class Ctx:
    """One recipe's run: the scale, the tool, the encoder, where it writes."""

    s: int  # the film's scale: 1 = 1920x1080, 2 = 3840x2160
    quaketool: Path
    pak: Path
    hw: str  # "vaapi" or "none"
    game: Path  # OUT/footage/game
    work: Path  # this run's scratch folder
    texts: Path  # the lettering cache
    streams: list = field(default_factory=list)
    writers: list = field(default_factory=list)

    @property
    def W(self) -> int:
        return 1920 * self.s

    @property
    def H(self) -> int:
        return 1080 * self.s

    def canvas(self) -> np.ndarray:
        return np.zeros((self.H, self.W, 3), np.uint8)

    # -- lettering ------------------------------------------------------------

    def text(self, s: str, scale: int = 3, font: str = "gold", shadow: int | None = None) -> np.ndarray:
        """`s` in Quake's conchars (`quaketool filmtext`), RGBA, at `scale` times the film's scale."""
        sc = scale * self.s
        key = hashlib.sha256(json.dumps([s, sc, font, shadow, sha_tool(self.quaketool)]).encode()).hexdigest()[:24]
        png = self.texts / f"{key}.png"
        if not png.exists():
            self.texts.mkdir(parents=True, exist_ok=True)
            tmp = self.texts / f"{key}.{os.getpid()}.part.png"
            cmd = [str(self.quaketool), "filmtext", str(self.pak), str(tmp), s, "--font", font, "--scale", str(sc)]
            if shadow is not None:
                cmd += ["--shadow", str(shadow * self.s)]
            subprocess.run(cmd, check=True, capture_output=True)
            os.replace(tmp, png)
        return np.asarray(Image.open(png).convert("RGBA")).copy()

    def label(self, dst, s: str, x: int, y: int, scale: int = 3, alpha: float = 1.0, pad: int | None = None):
        """A conchars label on a 60% black band (the film's label style); x, y in output pixels."""
        t = self.text(s, scale)
        p = (pad if pad is not None else 3 * scale) * self.s
        band(dst, x - p, y - p, x + t.shape[1] + p, y + t.shape[0] + p, 0.6 * alpha)
        paste(dst, t, x, y, alpha)

    # -- streams and writers ----------------------------------------------------

    def stream(self, shot: str, *, native: bool = False, lines=(), threads: int | None = None,
               frames: tuple[int, int] | None = None) -> Stream:
        """`quaketool film` on film/shots/SHOT.shot: its own size times the scale (or, `native`,
        its own size: an analysis pass), extra shot lines given last."""
        st = Stream(self, shot, native=native, lines=list(lines), threads=threads, frames=frames)
        self.streams.append(st)
        return st

    def writer(self, name: str, w: int | None = None, h: int | None = None) -> Writer:
        wr = Writer(self, self.game / f"{name}.mp4", w or self.W, h or self.H)
        self.writers.append(wr)
        return wr

    def abandon(self) -> None:
        """After a failure: stop this run's renders and encoders, and remove their part files."""
        for st in self.streams:
            st.kill()
        for wr in self.writers:
            wr.kill()


_TOOL_SHA: dict[str, str] = {}


def sha_tool(path: Path) -> str:
    k = str(path)
    if k not in _TOOL_SHA:
        _TOOL_SHA[k] = sha256(path)
    return _TOOL_SHA[k]


class Stream:
    """A `quaketool film` run streaming raw RGB24 frames; its sound.wav and events.json land in a
    fresh scratch folder."""

    def __init__(self, ctx: Ctx, shot: str, native: bool, lines: list[str], threads: int | None,
                 frames: tuple[int, int] | None):
        self.shot = shot
        path = shot_path(shot)
        info = shot_info(path, lines)
        w, h = info.size
        if not native:
            w, h = w * ctx.s, h * ctx.s
            lines = lines + [f"size {w}x{h}"]
        self.w, self.h, self.fps = w, h, info.fps
        self.dir = Path(tempfile.mkdtemp(prefix=f"{shot}-", dir=ctx.work))
        cmd = [str(ctx.quaketool), "film", str(ctx.pak), str(path), str(self.dir), "--raw"]
        for line in lines:
            key, _, rest = line.strip().partition(" ")
            cmd += [f"--{key}", rest.strip()]
        if frames:
            cmd += ["--frames", f"{frames[0]}..{frames[1]}"]
        if threads:
            cmd += ["--threads", str(threads)]
        self.cmd = cmd
        self.log = open(self.dir / "report.txt", "w+b")
        self.proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=self.log)
        self.n = 0
        self.closed = False

    def read(self) -> np.ndarray | None:
        size = self.w * self.h * 3
        buf = bytearray(size)
        view = memoryview(buf)
        got = 0
        while got < size:
            k = self.proc.stdout.readinto(view[got:])
            if not k:
                return None
            got += k
        self.n += 1
        return np.frombuffer(buf, dtype=np.uint8).reshape(self.h, self.w, 3)

    def all(self) -> list[np.ndarray]:
        out = []
        while (f := self.read()) is not None:
            out.append(f)
        self.close()
        return out

    def close(self) -> str:
        if self.closed:
            return self.report
        self.proc.stdout.close()
        code = self.proc.wait()
        self.log.seek(0)
        self.report = self.log.read().decode(errors="replace").strip()
        self.log.close()
        self.closed = True
        if code != 0:
            raise RuntimeError(f"quaketool film {self.shot} failed ({code}):\n{self.report[-2000:]}")
        return self.report

    def kill(self) -> None:
        if not self.closed:
            self.proc.kill()
            self.proc.wait()
            self.log.close()
            self.closed = True

    @property
    def wav(self) -> Path:
        p = self.dir / "sound.wav"
        if not p.exists():
            raise RuntimeError(f"{self.shot}: the render wrote no sound.wav")
        return p

    @property
    def events(self) -> Path:
        p = self.dir / "events.json"
        if not p.exists():
            raise RuntimeError(f"{self.shot}: the render wrote no events.json")
        return p


def encoder_cmd(hw: str, w: int, h: int, out: Path) -> list[str]:
    """ffmpeg reading raw RGB24 frames on stdin. Both paths convert to 4:2:0 in BT.709 (tv range)
    the same way; `none` is v7's H.264 (libx264, CRF 16, medium), `vaapi` HEVC on the GPU at a
    low constant QP."""
    vf = "scale=out_color_matrix=bt709:out_range=tv"
    head = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
    raw = ["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{w}x{h}", "-r", str(FPS), "-i", "-"]
    tags = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]
    if hw == "vaapi":
        return (head + ["-vaapi_device", VAAPI_DEVICE] + raw
                + ["-vf", vf + ",format=nv12,hwupload", "-c:v", "hevc_vaapi", "-rc_mode", "CQP", "-qp", str(VAAPI_QP),
                   "-profile:v", "main", "-tag:v", "hvc1"] + tags + ["-movflags", "+faststart", str(out)])
    return (head + raw + ["-vf", vf + ",format=yuv420p", "-c:v", "libx264", "-preset", "medium", "-crf", "16",
                          "-pix_fmt", "yuv420p"] + tags + ["-movflags", "+faststart", str(out)])


def encoder_id(hw: str) -> str:
    return f"hevc_vaapi CQP {VAAPI_QP}" if hw == "vaapi" else "libx264 CRF 16 medium"


class Writer:
    """An mp4 written through a part file, renamed into place when it closes cleanly."""

    def __init__(self, ctx: Ctx, out: Path, w: int, h: int):
        self.out, self.w, self.h = out, w, h
        self.part = out.with_name(out.stem + ".part.mp4")
        self.n = 0
        self.proc = subprocess.Popen(encoder_cmd(ctx.hw, w, h, self.part), stdin=subprocess.PIPE)

    def write(self, frame: np.ndarray) -> None:
        assert frame.shape == (self.h, self.w, 3), (self.out.name, frame.shape)
        self.proc.stdin.write(np.ascontiguousarray(frame, dtype=np.uint8).tobytes())
        self.n += 1

    def close(self) -> int:
        self.proc.stdin.close()
        if self.proc.wait() != 0:
            raise RuntimeError(f"ffmpeg failed for {self.out.name}")
        os.replace(self.part, self.out)
        return self.n

    def kill(self) -> None:
        if self.proc.poll() is None:
            try:
                self.proc.stdin.close()
            except OSError:
                pass
            self.proc.kill()
            self.proc.wait()
        self.part.unlink(missing_ok=True)


# ---------------------------------------------------------------------------
# sound, events, sidecars
# ---------------------------------------------------------------------------


def keep_wav(ctx: Ctx, st: Stream, name: str) -> None:
    """The render's game sound beside the mp4, as the engine's mixer wrote it (sample 0 at frame 0)."""
    shutil.copyfile(st.wav, ctx.game / f"{name}.wav")


def write_json(path: Path, obj, indent=1) -> None:
    tmp = path.with_name(path.name + ".part")
    tmp.write_text(json.dumps(obj, indent=indent) + "\n")
    os.replace(tmp, path)


def events_mapped(ctx: Ctx, st: Stream, name: str) -> None:
    """The render's events on the mp4's clock: film frame f of a render at `fps` whose first written
    frame is `first` is mp4 frame floor((f - first) * 60 / fps), t that frame's second. A window's
    marks move with it. (The footage scripts of shots S42 on.)"""
    ev = json.loads(st.events.read_text())
    a = int(ev.get("first", 0) or 0)
    k = FPS / st.fps
    out = []
    for e in ev.get("events", []):
        e = dict(e)
        if "frame" in e:
            e["film_frame"] = e["frame"]
            e["frame"] = int(np.floor((e["frame"] - a) * k))
            e["t"] = round(e["frame"] / FPS, 4)
        out.append(e)
    meta = {key: v for key, v in ev.items() if key != "events"}
    if a and meta.get("marks"):
        for m in meta["marks"]:
            for f in m["frames"]:
                f["film_frame"] = f["frame"]
                f["frame"] = f["frame"] - a
                f["t"] = round(f["frame"] / FPS, 4)
    meta.update({"mp4_fps": FPS, "note": "frame and t are the mp4's (handles included); film_frame is the render's. "
                 "Loops (kind loop) are the map's ambient sounds, playing from the start.", "events": out})
    write_json(ctx.game / f"{name}.events.json", meta)


def events_shifted(ctx: Ctx, st: Stream, name: str, t0: float = 0.0, seconds: float | None = None) -> None:
    """The render's events on the mp4's clock, for a file cut from a longer render at t0 (seconds
    long): times shifted and trimmed. (The footage scripts of shots S01-S41 and the box phase.)"""
    e = json.loads(st.events.read_text())
    out = []
    for ev in e["events"]:
        ev = dict(ev)
        t = ev.get("t", 0.0) - t0
        if ev.get("kind") == "loop":
            t = max(0.0, t)
        if t < 0 or (seconds is not None and t >= seconds):
            continue
        ev["t"] = round(t, 4)
        if "frame" in ev:
            ev["frame"] = ev["frame"] - round(t0 * FPS)
        out.append(ev)
    e["events"] = out
    e["shifted_by_s"] = t0
    e["first"] = 0
    if seconds is not None:
        e["frames"] = round(seconds * FPS)
    e["note"] = "times and frames on this mp4's clock (frame 0 = its first frame, the head handle)"
    write_json(ctx.game / f"{name}.events.json", e, indent=0)


def events_raw(ctx: Ctx, st: Stream, name: str, suffix: str = ".events.json") -> None:
    shutil.copyfile(st.events, ctx.game / f"{name}{suffix}")


def events_split(ctx: Ctx, st: Stream, name: str) -> None:
    """An `ab` render's events by take: NAME.events.json (b) and NAME.events-a.json (a)."""
    ev = json.loads(st.events.read_text())
    meta = {k: v for k, v in ev.items() if k not in ("events", "marks")}
    meta["note"] = ("times and frames on this mp4's clock (frame 0 = its first frame, the head handle); "
                    "loops (kind loop) are the map's ambient sounds, playing from the start")
    for take, suffix in (("b", ".events.json"), ("a", ".events-a.json")):
        out = dict(meta, take=take,
                   events=[e for e in ev.get("events", []) if e.get("take", take) == take],
                   marks=[m for m in ev.get("marks", []) if m.get("take", take) == take])
        write_json(ctx.game / f"{name}{suffix}", out)


def sidecar(ctx: Ctx, name: str, frames: int, src: str | None = None) -> None:
    """NAME.json: the cut's sidecar (film/shots/sidecars/: handles, sound, the times of what happens,
    in shot seconds) with this render's size, tool and encoder. Its frame count must be the cut's."""
    side = json.loads((SIDECARS / f"{src or name}.json").read_text())
    want = side.get("frames")
    if want is not None and want != frames:
        raise RuntimeError(f"{name}: {frames} frames written, the cut's file has {want}")
    side["render"] = {"size": [ctx.W, ctx.H], "scale": ctx.s, "quaketool_sha256": sha_tool(ctx.quaketool)[:16],
                      "encoder": encoder_id(ctx.hw)}
    write_json(ctx.game / f"{name}.json", side)


# ---------------------------------------------------------------------------
# drawing (coordinates in output pixels; the recipes scale v7's 1080p ones)
# ---------------------------------------------------------------------------


def paste(dst: np.ndarray, src: np.ndarray, x, y, alpha: float = 1.0) -> None:
    """Alpha-composite RGBA `src` onto RGB `dst` at (x, y), clipped."""
    x, y = int(round(x)), int(round(y))
    h, w = src.shape[:2]
    x0, y0 = max(0, x), max(0, y)
    x1, y1 = min(dst.shape[1], x + w), min(dst.shape[0], y + h)
    if x1 <= x0 or y1 <= y0:
        return
    s = src[y0 - y:y1 - y, x0 - x:x1 - x]
    a = (s[..., 3:4].astype(np.float32) / 255.0) * alpha
    region = dst[y0:y1, x0:x1].astype(np.float32)
    dst[y0:y1, x0:x1] = (region * (1 - a) + s[..., :3].astype(np.float32) * a).astype(np.uint8)


def band(dst, x0, y0, x1, y1, alpha=0.6):
    """A black band under a label."""
    x0, y0, x1, y1 = (max(0, int(v)) for v in (x0, y0, x1, y1))
    dst[y0:y1, x0:x1] = (dst[y0:y1, x0:x1].astype(np.float32) * (1 - alpha)).astype(np.uint8)


def rect(dst, x0, y0, x1, y1, color, t=3, alpha=1.0):
    """A rectangle outline `t` pixels thick, inside the bounds (S01-S41's: float64 blending)."""
    H, W = dst.shape[:2]
    x0, y0, x1, y1 = (int(round(v)) for v in (x0, y0, x1, y1))
    c = np.array(color, np.float32)
    for (a, b, c0, c1) in ((y0, y0 + t, x0, x1), (y1 - t, y1, x0, x1), (y0, y1, x0, x0 + t), (y0, y1, x1 - t, x1)):
        a, b, c0, c1 = max(0, a), min(H, b), max(0, c0), min(W, c1)
        if a < b and c0 < c1:
            dst[a:b, c0:c1] = (dst[a:b, c0:c1] * (1 - alpha) + c * alpha).astype(np.uint8)


def blend(a: np.ndarray, b: np.ndarray, u: float) -> np.ndarray:
    if u <= 0:
        return a
    if u >= 1:
        return b
    return (a.astype(np.float32) * (1 - u) + b.astype(np.float32) * u).astype(np.uint8)


def zoom(img: np.ndarray, k: int) -> np.ndarray:
    """Enlarged k times, nearest neighbour."""
    return img.repeat(k, axis=0).repeat(k, axis=1)


def nearest(img: np.ndarray, w: int, h: int) -> np.ndarray:
    """Resized to w x h, nearest neighbour (the film's way with pixels)."""
    ys = np.arange(h) * img.shape[0] // h
    xs = np.arange(w) * img.shape[1] // w
    return img[ys][:, xs]


def area(img: np.ndarray, w: int, h: int) -> np.ndarray:
    """Resized to w x h with area averaging (a downscale)."""
    return np.asarray(Image.fromarray(img).resize((w, h), Image.Resampling.BOX))


def place(dst, img, x, y):
    """RGB `img` into `dst` at (x, y), clipped."""
    h, w = img.shape[:2]
    x, y = int(x), int(y)
    x0, y0 = max(0, x), max(0, y)
    x1, y1 = min(dst.shape[1], x + w), min(dst.shape[0], y + h)
    if x1 > x0 and y1 > y0:
        dst[y0:y1, x0:x1] = img[y0 - y:y1 - y, x0 - x:x1 - x]


def tint_text(rgba: np.ndarray, color) -> np.ndarray:
    """`rgba`'s glyphs in one colour (the shadow kept dark)."""
    out = rgba.copy()
    lum = out[..., :3].max(axis=2) > 40
    out[lum, :3] = color
    return out


def changes(frames: list[np.ndarray], thresh=0.5) -> list[int]:
    """Indices i where frame i differs from frame i-1 (mean absolute difference)."""
    return [i for i in range(1, len(frames))
            if np.abs(frames[i - 1].astype(np.int16) - frames[i].astype(np.int16)).mean() > thresh]


def draw_pad(img: Image.Image, x: float, y: float, w: float, color, s: int) -> None:
    """A twin-stick gamepad in line art, `w` px wide, top-left (x, y); line widths times `s`."""
    d = ImageDraw.Draw(img)
    lw = 4 * s
    k = w / 360

    def P(px, py):
        return (x + px * k, y + py * k)
    body = [P(60, 40), P(300, 40), P(340, 70), P(358, 170), P(345, 215), P(310, 222), P(265, 170), P(95, 170),
            P(50, 222), P(15, 215), P(2, 170), P(20, 70), P(60, 40)]
    d.line(body, fill=color, width=lw, joint="curve")
    d.line([P(70, 40), P(78, 22), P(130, 22), P(138, 40)], fill=color, width=lw)
    d.line([P(222, 40), P(230, 22), P(282, 22), P(290, 40)], fill=color, width=lw)
    for cx, cy in ((120, 128), (240, 128)):
        d.ellipse([P(cx - 26, cy - 26), P(cx + 26, cy + 26)], outline=color, width=lw)
        d.ellipse([P(cx - 12, cy - 12), P(cx + 12, cy + 12)], outline=color, width=lw)
    cx, cy = 72, 82
    d.line([P(cx - 18, cy), P(cx + 18, cy)], fill=color, width=lw + 4 * s)
    d.line([P(cx, cy - 18), P(cx, cy + 18)], fill=color, width=lw + 4 * s)
    for bx, by in ((288, 66), (288, 98), (272, 82), (304, 82)):
        d.ellipse([P(bx - 7, by - 7), P(bx + 7, by + 7)], outline=color, width=3 * s)


def loop_script(rate: int, sample: str = "ambience/hum1.wav") -> str:
    """The sound oracle's script for one looped sample beside a still listener, 300 frames at 72 Hz
    (film/shots/INDEX.md, "LAB9's sound")."""
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
