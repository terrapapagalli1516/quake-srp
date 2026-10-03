#!/usr/bin/env -S uv run --with playwright --script
"""Verify the WASI host runs threads, in headless Chromium (or $QUAKE_BROWSER):
`quake-wasm`'s `threadcheck` program, built for wasm32-wasip1-threads, runs
in `wasi.js` on a shared `env.memory`; four `std::thread::scope` threads and
a spawned one (a worker each, through `wasi.thread-spawn`) sum their parts
and answer over a channel, then 200 rounds of seven scoped threads reuse the
host's workers the way the renderer does every frame, then eight threads
allocate while the heap fills (as the torch set's build and the frame's
bakes do), and the main thread checks the lot (PLATFORM.md, "Threads").

The last stage is the regression check for Chromium's trap: a worker thread
that touches memory another thread has just grown can trap ("memory access
out of bounds"), about one run in four on a growable memory. The threads
build's memory is fixed (quake-wasm/build.rs), so the program runs RUNS times
(default 8) and none may trap or fail. `--growable` also builds it the old
way (QUAKE_WASM_GROWABLE=1, its own target dir) and runs that RUNS times, to
show the trap is there to catch: it reports how many trapped (expected some,
in Chromium; Firefox does not trap), and does not fail on them.

Usage: verify_threads.py [threadcheck.wasm] [--runs N] [--growable]
       (default: builds it with cargo)
"""
import os, shutil, subprocess, sys, time
from playwright.sync_api import sync_playwright
import isolated

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.join(os.path.dirname(HERE), "quake-wasm")
args = sys.argv[1:]
GROWABLE = "--growable" in args
RUNS = int(args[args.index("--runs") + 1]) if "--runs" in args else 8
paths = [a for i, a in enumerate(args) if not a.startswith("--") and (i == 0 or args[i - 1] != "--runs")]


def build(growable):
    """threadcheck.wasm, the threads build's (fixed memory) or the old growable one."""
    env = dict(os.environ)
    target_dir = os.path.join(CRATE, "target")
    if growable:
        env["QUAKE_WASM_GROWABLE"] = "1"
        target_dir = os.path.join(CRATE, "target", "growable")
    subprocess.run(["cargo", "build", "--release", "--target", "wasm32-wasip1-threads", "--bin", "threadcheck",
                    "--target-dir", target_dir], cwd=CRATE, check=True, env=env)
    return os.path.join(target_dir, "wasm32-wasip1-threads/release/threadcheck.wasm")


WASM = paths[0] if paths else build(False)
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


def run_program(br, wasm):
    """One run of `wasm` in a fresh page: (its log, its exit, the page's errors)."""
    shutil.copy(wasm, os.path.join(WEB, "threadcheck.wasm"))
    pg = br.new_page()
    errs = []
    pg.on("pageerror", lambda e: errs.append(str(e)))
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    try:
        pg.wait_for_function("window.done", timeout=120000)
    except Exception:
        errs.append("the program did not finish")
    done = pg.evaluate("window.done || null")
    logs = pg.evaluate("window.logs")
    isolated_ok = pg.evaluate("crossOriginIsolated")
    pg.close()
    return logs, done, errs, isolated_ok


fails = []
with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    t0 = time.time()
    trapped = 0
    for run in range(RUNS):
        logs, done, errs, isolated_ok = run_program(br, WASM)
        line = next((l for l in logs if "threadcheck:" in l), "")
        if run == 0:
            print("\n".join(logs))
            print(f"exit: {done}; cross-origin isolated: {isolated_ok}")
        if not done or done["run"] != 2:
            fails.append(f"run {run}: the program did not exit cleanly: {done}")
        if not line.endswith(": ok"):
            fails.append(f"run {run}: no threadcheck line saying ok")
        if "(fixed)" not in line and not paths:
            fails.append(f"run {run}: the memory is not the fixed size: {line[-80:]}")
        if errs:
            trapped += any("out of bounds" in e for e in errs)
            fails.append(f"run {run}: page errors: {errs[-2:]}")
    print(f"the threads build: {RUNS} runs, {trapped} trapped, {time.time() - t0:.1f} s")
    if GROWABLE:
        old = build(True)
        bad = 0
        for run in range(RUNS):
            logs, done, errs, _ = run_program(br, old)
            bad += any("out of bounds" in e for e in errs + logs)
        print(f"the growable build (the old link): {RUNS} runs, {bad} trapped")
    br.close()
httpd.shutdown()
if fails:
    print("FAIL:", "; ".join(fails[:6]))
    sys.exit(1)
print("done: threads verified (thread::scope, a spawned thread, and threads allocating as the heap fills, through the WASI host)")
