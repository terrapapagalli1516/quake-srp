#!/usr/bin/env -S uv run --with playwright --script
"""Verify the WASI host runs threads, in headless Chromium (or $QUAKE_BROWSER):
`quake-wasm`'s `threadcheck` program, built for wasm32-wasip1-threads, runs
in `wasi.js` on a shared `env.memory`; four `std::thread::scope` threads and
a spawned one (a worker each, through `wasi.thread-spawn`) sum their parts
and answer over a channel, and the main thread checks the lot (PLATFORM.md,
"Threads"). The game itself does not use threads yet; this is the host's
half, proven ahead of it.

Usage: verify_threads.py [threadcheck.wasm]   (default: builds it with cargo)
"""
import os, shutil, subprocess, sys, time
from playwright.sync_api import sync_playwright
import isolated

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.join(os.path.dirname(HERE), "quake-wasm")
if len(sys.argv) > 1:
    WASM = sys.argv[1]
else:
    subprocess.run(["cargo", "build", "--release", "--target", "wasm32-wasip1-threads", "--bin", "threadcheck"],
                   cwd=CRATE, check=True)
    WASM = os.path.join(CRATE, "target/wasm32-wasip1-threads/release/threadcheck.wasm")
WEB = os.path.join(CRATE, "target", "threadcheck-web")
os.makedirs(WEB, exist_ok=True)
shutil.copy(os.path.join(HERE, "wasi.js"), WEB)
shutil.copy(WASM, os.path.join(WEB, "threadcheck.wasm"))
# The smallest host page: the control block and ring, the worker, the logs.
with open(os.path.join(WEB, "index.html"), "w") as f:
    f.write("""<!doctype html><title>threadcheck</title><script>
window.logs = [];
const shared = new SharedArrayBuffer(256 + 65536);
const worker = new Worker('wasi.js');
worker.onmessage = e => {
  if (e.data.t === 'log') logs.push(e.data.text);
  if (e.data.t === 'exit') window.done = e.data;
};
fetch('threadcheck.wasm').then(r => r.arrayBuffer())
  .then(wasm => worker.postMessage({ t: 'init', wasm, shared, files: [] }, [wasm]));
</script>""")

PORT = isolated.port(8177)
httpd = isolated.serve(WEB, PORT)
fails = []
with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    pg = br.new_page()
    errs = []
    pg.on("pageerror", lambda e: errs.append(str(e)))
    t0 = time.time()
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    try:
        pg.wait_for_function("window.done", timeout=60000)
    except Exception:
        fails.append("the program did not finish")
    done = pg.evaluate("window.done || null")
    logs = pg.evaluate("window.logs")
    print("\n".join(logs))
    print(f"exit: {done} after {time.time() - t0:.1f} s; cross-origin isolated: {pg.evaluate('crossOriginIsolated')}")
    if not done or done["run"] != 2:
        fails.append(f"the program did not exit cleanly: {done}")
    if not any("threadcheck:" in l and l.endswith(": ok") for l in logs):
        fails.append("no threadcheck line saying ok")
    if errs:
        fails.append(f"page errors: {errs[-3:]}")
    br.close()
httpd.shutdown()
if fails:
    print("FAIL:", "; ".join(fails))
    sys.exit(1)
print("done: threads verified (thread::scope and a spawned thread through the WASI host)")
