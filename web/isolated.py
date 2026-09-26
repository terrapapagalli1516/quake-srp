"""The web server the browser checks (verify_*.py) and bench.py run the page
on: a directory served over HTTP with the two headers that make the page
cross-origin isolated, which its SharedArrayBuffers need (PLATFORM.md,
"Serving"):

    Cross-Origin-Opener-Policy: same-origin
    Cross-Origin-Embedder-Policy: require-corp

A web directory holds index.html, wasi.js, quake.wasm and id1/pak0.pak;
`webdir()` finds the one to serve: the script's argument, or `web/` itself
(with the engine and the pak put in place by PLATFORM.md's deploy recipe).
"""
import functools
import http.server
import os
import socketserver
import sys
import threading


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".wasm": "application/wasm", ".js": "text/javascript"}

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


def webdir():
    """The directory to serve: argv[1], else this script's own directory."""
    return sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))


def port(default):
    """The port: $QUAKE_VERIFY_PORT, else the script's default."""
    return int(os.environ.get("QUAKE_VERIFY_PORT", str(default)))
