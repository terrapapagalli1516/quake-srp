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
$QUAKE_BROWSER.
"""
import functools
import http.server
import json
import os
import shutil
import socketserver
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


def write_manifest(dest):
    """Write `dest`/files.json, the manifest a deploy uses to offer its own
    id1/pak1.pak and CD tracks (web/PLATFORM.md, "A server's own files"):
    one entry per file actually found at dest/id1/pak1.pak and
    dest/id1/music/*, each with its size. Call this after those files are in
    place; skip it (or leave it unwritten) for a pak0-only deploy — its
    absence is exactly what tells the page to ask for nothing else."""
    files = []
    pak1 = os.path.join(dest, "id1", "pak1.pak")
    if os.path.isfile(pak1):
        files.append({"path": "id1/pak1.pak", "size": os.path.getsize(pak1)})
    music_dir = os.path.join(dest, "id1", "music")
    if os.path.isdir(music_dir):
        for name in sorted(os.listdir(music_dir)):
            path = os.path.join(music_dir, name)
            if os.path.isfile(path):
                files.append({"path": f"id1/music/{name}", "size": os.path.getsize(path)})
    with open(os.path.join(dest, "files.json"), "w") as f:
        json.dump({"files": files}, f)


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
