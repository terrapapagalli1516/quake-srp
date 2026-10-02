"""The web server the browser checks (verify_*.py) and bench.py run the page
on: a directory served over HTTP with the two headers that make the page
cross-origin isolated, which its SharedArrayBuffers need (PLATFORM.md,
"Serving"):

    Cross-Origin-Opener-Policy: same-origin
    Cross-Origin-Embedder-Policy: require-corp

A web directory holds the page (PAGE_FILES: `copy_page` puts them in
place), quake.wasm and id1/pak0.pak; `webdir()` finds the one to serve:
the script's argument, or `web/` itself
(with the engine and the pak put in place by PLATFORM.md's deploy recipe).
`launch()` starts the browser the checks run in: Chromium, or
$QUAKE_BROWSER. And the game data a check needs beyond id's shareware pak
is synthesized here (`write_pak`, `POP_LMP`): a registered pak1, a mission
pack's pak0.
"""
import functools
import http.server
import json
import os
import re
import shutil
import socketserver
import struct
import sys
import threading


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".wasm": "application/wasm", ".js": "text/javascript",
                      ".webmanifest": "application/manifest+json"}

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *a):
        pass


def serve(directory, port):
    """Serve `directory` on 127.0.0.1:`port` from a daemon thread; returns the
    server (call .shutdown() when done)."""
    socketserver.ThreadingTCPServer.allow_reuse_address = True
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", port),
                                            functools.partial(Handler, directory=directory))
    httpd.daemon_threads = True
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


# The page's own files, as a deploy dir holds them beside quake.wasm and
# id1/pak0.pak (PLATFORM.md, "Build, serve, deploy").
PAGE_FILES = ["index.html", "wasi.js", "touch.js", "endscreen.js", "sw.js", "manifest.webmanifest",
              "icons/icon-192.png", "icons/icon-512.png", "icons/apple-touch-icon.png"]


def copy_page(dest):
    """Copy PAGE_FILES from web/ into the deploy dir `dest`."""
    here = os.path.dirname(os.path.abspath(__file__))
    for f in PAGE_FILES:
        os.makedirs(os.path.dirname(os.path.join(dest, f)), exist_ok=True)
        shutil.copy(os.path.join(here, f), os.path.join(dest, f))


# The port's own game directories (quake-rs's common.rs: `mod_dirs`) a
# deploy or a drop can offer beyond the engine's own baseline id1/pak0.pak —
# id1 for its registered pak1 (and music), hipnotic/rogue for their own
# pak0 (and music), each only ever fetched when that game is the one
# starting (index.html's planServerFiles).
GAME_DIRS = ("id1", "hipnotic", "rogue")


def write_manifest(dest):
    """Write `dest`/files.json, the manifest a deploy uses to offer its own
    extra game files beyond the engine's own id1/pak0.pak
    (web/PLATFORM.md, "A server's own files"): one entry per pak and music
    file actually found under `dest`/id1, `dest`/hipnotic and `dest`/rogue —
    id1/pak0.pak itself excepted, since that one is always the deploy's own
    baseline fetch, never the manifest's. Call this after those files are in
    place; skip it (or leave it unwritten) for a pak0-only deploy — its
    absence is exactly what tells the page to ask for nothing else."""
    files = []
    for game in GAME_DIRS:
        game_dir = os.path.join(dest, game)
        if os.path.isdir(game_dir):
            for name in sorted(os.listdir(game_dir)):
                path = os.path.join(game_dir, name)
                if not os.path.isfile(path) or not re.fullmatch(r"pak\d+\.pak", name):
                    continue
                if game == "id1" and name == "pak0.pak":
                    continue   # the engine's own baseline fetch, not the manifest's
                files.append({"path": f"{game}/{name}", "size": os.path.getsize(path)})
        music_dir = os.path.join(game_dir, "music")
        if os.path.isdir(music_dir):
            for name in sorted(os.listdir(music_dir)):
                path = os.path.join(music_dir, name)
                if os.path.isfile(path):
                    files.append({"path": f"{game}/music/{name}", "size": os.path.getsize(path)})
    with open(os.path.join(dest, "files.json"), "w") as f:
        json.dump({"files": files}, f)


# ---------------------------------------------------------------------------
# Synthesized game data for the checks that need more than id's shareware
# pak (verify_content.py, verify_touch.py): no registered or mission-pack
# data is in the repo, or needed — a pak1.pak the engine takes for id's
# registered one is common.c's pop[] table as gfx/pop.lmp plus a map, and a
# mission pack's pak0.pak is a map under that pack's name.
# ---------------------------------------------------------------------------

# common.c's pop[]: gfx/pop.lmp is these 128 shorts, big-endian.
POP = [
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x6600, 0x0000, 0x0000, 0x0000, 0x6600, 0x0000,
    0x0000, 0x0066, 0x0000, 0x0000, 0x0000, 0x0000, 0x0067, 0x0000, 0x0000, 0x6665, 0x0000, 0x0000, 0x0000, 0x0000, 0x0065, 0x6600,
    0x0063, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6563, 0x0064, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6564,
    0x0064, 0x6564, 0x0000, 0x6469, 0x6969, 0x6400, 0x0064, 0x6564, 0x0063, 0x6568, 0x6200, 0x0064, 0x6864, 0x0000, 0x6268, 0x6563,
    0x0000, 0x6567, 0x6963, 0x0064, 0x6764, 0x0063, 0x6967, 0x6500, 0x0000, 0x6266, 0x6769, 0x6a68, 0x6768, 0x6a69, 0x6766, 0x6200,
    0x0000, 0x0062, 0x6566, 0x6666, 0x6666, 0x6666, 0x6562, 0x0000, 0x0000, 0x0000, 0x0062, 0x6364, 0x6664, 0x6362, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0062, 0x6662, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6661, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0000, 0x6500, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x6400, 0x0000, 0x0000, 0x0000,
]
POP_LMP = b"".join(struct.pack(">H", v) for v in POP)


def read_pak(path):
    """A pak's directory: name -> (filepos, filelen)."""
    with open(path, "rb") as f:
        magic, dirofs, dirlen = struct.unpack("<4sii", f.read(12))
        assert magic == b"PACK", path
        f.seek(dirofs)
        d = f.read(dirlen)
    return {d[i:i + 56].split(b"\0")[0].decode(): struct.unpack("<ii", d[i + 56:i + 64]) for i in range(0, dirlen, 64)}


def pak_file(path, name):
    pos, n = read_pak(path)[name]
    with open(path, "rb") as f:
        f.seek(pos)
        return f.read(n)


def write_pak(files):
    """A PACK image of (name, bytes) pairs: header, contents, directory."""
    body = b"".join(b for _, b in files)
    out = struct.pack("<4sii", b"PACK", 12 + len(body), 64 * len(files)) + body
    pos = 12
    for name, b in files:
        out += name.encode().ljust(56, b"\0") + struct.pack("<ii", pos, len(b))
        pos += len(b)
    return out


def webdir():
    """The directory to serve: argv[1], else this script's own directory."""
    return sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))


def port(default):
    """The port: $QUAKE_VERIFY_PORT, else the script's default."""
    return int(os.environ.get("QUAKE_VERIFY_PORT", str(default)))


def gpu_flags():
    """Chromium's flags for the machine's GPU when $QUAKE_GPU is set (1:
    ANGLE on GL/EGL, as Chrome on Linux; `vulkan`: ANGLE on Vulkan). Headless
    Chromium otherwise draws WebGL and composites in software (SwiftShader),
    which is correct but says little about a real browser's speed."""
    gpu = os.environ.get("QUAKE_GPU", "")
    if not gpu:
        return []
    angle = "vulkan" if gpu == "vulkan" else "gl-egl"
    return ["--enable-gpu", "--use-gl=angle", f"--use-angle={angle}", "--ignore-gpu-blocklist"]


def launch(p, args=()):
    """A headless browser: Chromium, or the one $QUAKE_BROWSER names
    (`firefox`, `webkit`; Playwright's builds). Chromium's command-line
    `args` only go to Chromium; Firefox gets the equivalent of
    `--autoplay-policy=no-user-gesture-required` as a preference."""
    name = os.environ.get("QUAKE_BROWSER", "chromium")
    if name == "chromium":
        return p.chromium.launch(headless=True, args=list(args) + gpu_flags())
    prefs = {"media.autoplay.default": 0, "media.autoplay.blocking_policy": 0} if name == "firefox" else None
    kw = {"firefox_user_prefs": prefs} if prefs else {}
    return getattr(p, name).launch(headless=True, **kw)
