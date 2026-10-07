"""The web stage's plumbing: build the page, serve it, drive a headless Chromium, and turn
its frames into a clip.

Frames come from two clocks:
- ticked: the page's own benchmark driver (index.html, `quake.pause()` and `quake.tick(dt)`):
  one game tick of exactly 1/60 s per output frame, the frame presented as the page presents
  it, then a CDP screenshot of the whole page at its device pixels. Deterministic 60 fps
  whatever the machine's load.
- realtime: CDP's screencast (every compositor frame, JPEG quality 100), each frame stamped
  by the browser, resampled onto a fixed number of 60 fps frames. Only F01's boot uses it.

A clip's frames are composed one by one and piped straight into ffmpeg (no frame files):
a 4K frame is 25 MB as pixels, and a clip has hundreds of them.
"""

from __future__ import annotations

import base64
import functools
import hashlib
import io
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import filmroot  # noqa: E402
import vaapi  # noqa: E402  (the film's VAAPI device, and the software decode every encode passes)

REPO = filmroot.REPO
sys.path.insert(0, str(REPO / "web"))
import isolated  # noqa: E402  (the repository's own server and its page's file list)

FPS = 60
PORTS = range(9300, 9310)
VAAPI_DEVICE = vaapi.DEVICE


def log(*a):
    print(*a, flush=True)


# --- The page -------------------------------------------------------------------------------

def build_wasm(target_dir: Path | None) -> Path:
    """The threads build of the program, built from this repository (quake-wasm/, whose
    .cargo/config.toml turns on SIMD, so cargo runs from there). Cargo is incremental: a
    build with nothing changed takes a second."""
    env = dict(os.environ)
    if target_dir:
        env["CARGO_TARGET_DIR"] = str(target_dir)
    log("building the page's program (cargo build --release --target wasm32-wasip1-threads)")
    subprocess.run(["cargo", "build", "--release", "--locked", "--target", "wasm32-wasip1-threads"],
                   cwd=REPO / "quake-wasm", env=env, check=True)
    tdir = Path(env.get("CARGO_TARGET_DIR") or REPO / "quake-wasm" / "target")
    wasm = tdir / "wasm32-wasip1-threads" / "release" / "quake.wasm"
    if not wasm.exists():
        raise SystemExit(f"no {wasm} after the build")
    return wasm


def make_deploy(wasm: Path, dest: Path) -> Path:
    """A deploy dir as web/PLATFORM.md lays one out: the page's files, quake.wasm, and id's
    shareware pak as id1/pak0.pak."""
    dest.mkdir(parents=True)
    isolated.copy_page(str(dest))
    shutil.copy(wasm, dest / "quake.wasm")
    (dest / "id1").mkdir()
    if not filmroot.PAK.exists():
        raise SystemExit(f"id's shareware pak is missing: {filmroot.PAK}")
    shutil.copy(filmroot.PAK, dest / "id1" / "pak0.pak")
    return dest


def tree_digest(paths: list[Path]) -> str:
    """A digest of files' bytes (directories walked in name order)."""
    h = hashlib.sha256()
    for p in paths:
        files = sorted(q for q in p.rglob("*") if q.is_file()) if p.is_dir() else [p]
        for f in files:
            if "__pycache__" in f.parts:
                continue
            h.update(f.name.encode())
            h.update(f.read_bytes())
    return h.hexdigest()


# --- Serving --------------------------------------------------------------------------------

class SlowHandler(isolated.Handler):
    """isolated.py's server with a bandwidth cap (`rate` bytes a second for the whole
    response): a download you can see, as over a real link."""
    rate = 10_000_000

    def copyfile(self, source, outputfile):
        chunk = 64 * 1024
        t0 = time.monotonic()
        sent = 0
        while True:
            buf = source.read(chunk)
            if not buf:
                break
            outputfile.write(buf)
            sent += len(buf)
            ahead = sent / self.rate - (time.monotonic() - t0)
            if ahead > 0:
                time.sleep(ahead)


class _Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def handle_error(self, request, client_address):
        # A browser that drops a response half way (a navigation, a cancelled fetch) is
        # not an error worth a traceback.
        if not isinstance(sys.exc_info()[1], (BrokenPipeError, ConnectionResetError)):
            super().handle_error(request, client_address)


class Server:
    """The deploy served by isolated.py's handler (the isolation headers threads need) on the
    first free port of 9300-9309, from a thread of this process; stopped on exit."""

    def __init__(self, directory: Path, handler=isolated.Handler):
        self.directory, self.handler = directory, handler
        self.httpd = None
        self.port = None

    def __enter__(self):
        for port in PORTS:
            try:
                self.httpd = _Server(("127.0.0.1", port), functools.partial(self.handler, directory=str(self.directory)))
            except OSError:
                continue
            self.port = port
            threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
            return self
        raise SystemExit("no free port in 9300-9309")

    def __exit__(self, *exc):
        self.httpd.shutdown()
        self.httpd.server_close()

    @property
    def url(self):
        return f"http://localhost:{self.port}/"


# --- The browser ----------------------------------------------------------------------------

def launch(p, args=()):
    try:
        return p.chromium.launch(headless=True, args=["--no-sandbox", "--autoplay-policy=no-user-gesture-required", *args])
    except Exception as e:
        if "Executable doesn't exist" in str(e):
            raise SystemExit("Playwright's Chromium is not installed: "
                             "uv run --with playwright==1.63.0 playwright install chromium") from e
        raise


def desktop_context(p, css_w, css_h, dpr):
    """A desktop window of css_w x css_h CSS px at devicePixelRatio dpr, as the browser's own
    (--force-device-scale-factor, --window-size) rather than Playwright's emulation: then
    CDP's screencast delivers device pixels too (under emulation it delivers CSS pixels)."""
    br = launch(p, [f"--force-device-scale-factor={dpr}", f"--window-size={css_w},{css_h}"])
    return br, br.new_context(no_viewport=True)


# Pause the page's own ticks the moment the game is ready (before its first frame), so every
# game frame after that is one of ours.
PAUSE_AT_READY_JS = """(() => {
  const iv = setInterval(() => {
    if (window.quake && quake.ready) { quake.pause(); clearInterval(iv); window.__capturePaused = performance.now(); }
  }, 1);
})();"""

# headless Chromium grants requestFullscreen by resizing its own window; the checks turn it
# off the same way (web/verify_touch.py's NO_FULLSCREEN_JS).
NO_FULLSCREEN_JS = "HTMLElement.prototype.requestFullscreen = () => Promise.reject(new Error('disabled for the capture'));"


class Shooter:
    """CDP screenshots of a page at its device pixels, as PNG bytes."""

    def __init__(self, ctx, pg, css_w, css_h, dpr):
        self.pg = pg
        self.cdp = ctx.new_cdp_session(pg)
        self.size = (round(css_w * dpr), round(css_h * dpr))
        self.params = None
        # Under Playwright's emulation a plain capture is CSS-sized and a clip scaled by the
        # ratio gives the device pixels; with the browser's own scale factor a plain capture
        # already is. Take whichever gives them.
        self.candidates = [{"format": "png", "optimizeForSpeed": True},
                           {"format": "png", "optimizeForSpeed": True,
                            "clip": {"x": 0, "y": 0, "width": css_w, "height": css_h, "scale": dpr}}]

    def png(self) -> bytes:
        from PIL import Image
        if self.params is None:
            got = []
            for c in self.candidates:
                d = base64.b64decode(self.cdp.send("Page.captureScreenshot", c)["data"])
                size = Image.open(io.BytesIO(d)).size
                got.append(size)
                if abs(size[0] - self.size[0]) <= 1 and abs(size[1] - self.size[1]) <= 1:
                    self.params = c
                    self.size = size
                    return d
            raise RuntimeError(f"no screenshot call gives {self.size} (got {got})")
        return base64.b64decode(self.cdp.send("Page.captureScreenshot", self.params)["data"])


class Screencast:
    """CDP's screencast: every compositor frame, with the browser's time."""

    def __init__(self, cdp, max_w, max_h):
        self.cdp, self.max_w, self.max_h = cdp, max_w, max_h
        self.frames = []
        self.on = False
        cdp.on("Page.screencastFrame", self._frame)

    def _frame(self, ev):
        if self.on:
            self.frames.append((ev["metadata"]["timestamp"], base64.b64decode(ev["data"])))
        try:
            self.cdp.send("Page.screencastFrameAck", {"sessionId": ev["sessionId"]})
        except Exception:
            pass

    def start(self):
        self.frames = []
        self.on = True
        self.cdp.send("Page.startScreencast", {"format": "jpeg", "quality": 100, "everyNthFrame": 1,
                                               "maxWidth": self.max_w, "maxHeight": self.max_h})

    def stop(self):
        self.cdp.send("Page.stopScreencast")
        self.on = False
        return self.frames


def raf(pg, n=1):
    """Wait for n of the page's animation frames (touch.js flushes the stick once a display
    frame)."""
    pg.evaluate("n => new Promise(r => { const step = k => k ? requestAnimationFrame(() => step(k - 1)) : r(); step(n); })", n)


def tick(pg, dt=1 / FPS):
    r = pg.evaluate(f"quake.tick({dt})")
    if not r["ok"]:
        log("  warning: a tick was not answered in time", r)
    return r


# The CSS cursor over a point of the page (CSS px): what a real pointer would show there.
CURSOR_KIND_JS = """([x, y]) => { const e = document.elementFromPoint(x, y);
  if (!e) return 'default';
  const c = getComputedStyle(e).cursor; return c === 'auto' ? 'default' : c; }"""


def cursor_kind(pg, x, y, dpr):
    try:
        return pg.evaluate(CURSOR_KIND_JS, [x / dpr, y / dpr])
    except Exception:
        return "default"


# --- Encoding -------------------------------------------------------------------------------

def vaapi_works() -> bool:
    """A one-frame hevc_vaapi encode: the device is there and the driver takes it."""
    if not Path(VAAPI_DEVICE).exists():
        return False
    r = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=256x256:r=60",
                        "-frames:v", "1", "-vaapi_device", VAAPI_DEVICE, "-vf", "format=nv12,hwupload",
                        "-c:v", "hevc_vaapi", "-f", "null", "-"], capture_output=True)
    return r.returncode == 0


TAGS = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]
# The frames as pixels, the BT.709 conversion v7's encode used: RGB to 4:2:0 at TV range.
TO_YUV = "scale=out_color_matrix=bt709:out_range=tv"


def encoder_args(hw: str, qp: int) -> tuple[list[str], list[str]]:
    """(input options, output options) for `hw`:
    - vaapi: HEVC on the GPU (VAAPI), constant QP `qp`, 8-bit 4:2:0;
    - none: software H.264, CRF 16, preset slow: v7's encode exactly."""
    if hw == "vaapi":
        return (["-vaapi_device", VAAPI_DEVICE],
                ["-vf", f"{TO_YUV},format=nv12,hwupload", "-c:v", "hevc_vaapi", "-rc_mode", "CQP", "-qp", str(qp),
                 "-profile:v", "main"])     # never tagged hvc1: see vaapi.py
    return ([], ["-vf", f"{TO_YUV},format=yuv420p", "-c:v", "libx264", "-preset", "slow", "-crf", "16", "-pix_fmt", "yuv420p"])


def decodes_clean(mp4: Path, frames: int) -> str:
    """'' when all of `mp4` decodes clean in software (vaapi.decodes_clean) and it has `frames`
    frames; else what went wrong."""
    bad = vaapi.decodes_clean(mp4)
    if bad:
        return bad.splitlines()[0]
    n = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames", "-show_entries",
                        "stream=nb_read_frames", "-of", "csv=p=0", str(mp4)], capture_output=True, text=True).stdout.strip()
    return "" if n == str(frames) else f"{n} frames decoded, {frames} written"


class BadEncode(RuntimeError):
    """A clip whose encode does not decode clean."""


class Clip:
    """One output clip: frames (PIL RGB images of the clip's size) piped into ffmpeg as raw
    RGB, a log of events by frame, and an optional copy of every frame as a PNG.

    The finished clip is decoded in full, in software, before it is kept (vaapi.py says why: a
    VAAPI encode can look right to the GPU's decoder and be garbage to everyone else's). A clip
    that does not decode clean raises BadEncode and is not kept; the frames are gone by then, so
    the caller captures the shot again."""

    def __init__(self, path: Path, size: tuple[int, int], hw: str, qp: int, keep: Path | None = None):
        self.path, self.size = path, size
        self.n = 0
        self.events = []
        self.encoder = "hevc_vaapi" if hw == "vaapi" else "libx264"
        self.keep = keep
        if keep:
            keep.mkdir(parents=True, exist_ok=True)
        pre, codec = encoder_args(hw, qp)
        self.tmp = path.with_name(path.stem + ".partial.mp4")
        cmd = ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", *pre,
               "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{size[0]}x{size[1]}", "-framerate", str(FPS), "-i", "-",
               *codec, *TAGS, "-movflags", "+faststart", str(self.tmp)]
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)

    def add(self, im):
        if im.size != self.size or im.mode != "RGB":
            raise ValueError(f"frame {self.n}: {im.size} {im.mode}, the clip is {self.size} RGB")
        self.proc.stdin.write(im.tobytes())
        if self.keep:
            im.save(self.keep / f"c{self.n:05d}.png", compress_level=1)
        self.n += 1

    def mark(self, what, frame=None):
        f = self.n if frame is None else frame
        self.events.append([f, round(f / FPS, 3), what])
        log(f"  frame {f} ({f / FPS:.2f} s): {what}")

    def close(self):
        self.proc.stdin.close()
        if self.proc.wait() != 0:
            raise RuntimeError(f"ffmpeg failed on {self.path.name}")
        bad = decodes_clean(self.tmp, self.n)
        if bad:
            self.tmp.unlink()
            raise BadEncode(f"{self.path.name}: the {self.encoder} encode does not decode clean: {bad}")
        self.tmp.replace(self.path)

    def abort(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        self.proc.kill()
        self.proc.wait()
        if self.tmp.exists():
            self.tmp.unlink()


def font_file(pattern: str) -> str:
    """A font's file through fontconfig (`fc-match`)."""
    r = subprocess.run(["fc-match", "-f", "%{file}", pattern], capture_output=True, text=True)
    if r.returncode != 0 or not r.stdout:
        raise SystemExit(f"fontconfig found no font for {pattern!r}")
    return r.stdout


def contact_sheet(mp4: Path, out_png: Path, scale: int, cols=4, rows=4):
    """A grid of frames spread evenly over the clip, each stamped with its time, at `scale`
    times v7's tile size."""
    dur = float(subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(mp4)],
                               capture_output=True, text=True, check=True).stdout.strip())
    step = dur / (cols * rows)
    font = font_file("Noto Sans Mono").replace(":", r"\:")
    s = scale
    vf = (f"fps=1/{step:.4f}:start_time=0,scale={480 * s}:-2,"
          f"drawtext=fontfile={font}:fontsize={20 * s}:fontcolor=white:"
          f"box=1:boxcolor=black@0.6:x={6 * s}:y={6 * s}:text='%{{pts\\:hms}}',"
          f"tile={cols}x{rows}:padding={4 * s}:color=0x202020")
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(mp4), "-vf", vf, "-frames:v", "1", "-update", "1",
                    str(out_png)], check=True)


def write_events(clip: Clip, out_json: Path, extra: dict | None = None):
    out_json.write_text(json.dumps({"frames": clip.n, "seconds": round(clip.n / FPS, 3),
                                    "events": sorted(clip.events, key=lambda e: e[0]), **(extra or {})}, indent=1))


def scratch_dir(part: str) -> Path:
    """A fresh folder under FILM_SCRATCH/web/ (removed by the caller, by its path)."""
    return Path(tempfile.mkdtemp(prefix=part + "-", dir=filmroot.scratch("web")))
