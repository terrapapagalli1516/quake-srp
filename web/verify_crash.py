#!/usr/bin/env -S uv run --with playwright --script
"""Verify a crash ends the game visibly, in headless Chromium (or $QUAKE_BROWSER).

A thread that traps (a panic aborts, so it traps too) used to freeze the page:
the program's main thread waits for ever to join it, and the page for the
program. Now the thread's worker sets the run state crashed, wakes every Sync
waiter and tells the page on its own channel; the page stops the program's
worker and shows what stopped it, with a reload (web/PLATFORM.md, "Threads").

  1. boot, then `crash_a_thread` (quake-wasm's automation: a scoped thread
     that panics): within a second the page shows "The game stopped" over
     the view, with the thread's reason and a reload button, the status line
     says so, the pending call is refused, and no frame comes after;
  2. a fresh page with a memory the browser will not make (wasi.js's
     `WebAssembly.Memory` made to throw, as a browser refusing the threads
     build's fixed size would): the page says the game did not start, and
     why, in plain words;
  3. a fresh page whose thread workers the browser will not make (`Worker`
     made to throw): the same, in the same words — the threads build does not
     fall back to one thread (the player's own `threads 1` is another thing)
     — within a second of the load, and the game never ran.

Usage: verify_crash.py [deploydir]   (PLATFORM.md: a threads build's deploy)
"""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8178)
httpd = isolated.serve(WEB, PORT)
fails = []


def check(what, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail else ""), flush=True)
    if not ok:
        fails.append(what)


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    pg = br.new_page(viewport={"width": 960, "height": 600})
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    threads = pg.evaluate("exp.render_threads()")
    check("a threads build, with threads", threads > 1, f"{threads} threads")
    pg.evaluate("window._crash = exp.crash_a_thread().then(() => 'answered', (e) => 'refused: ' + e.message)")
    t0 = time.time()
    try:
        pg.wait_for_selector("#stopped", timeout=1000)
        shown = time.time() - t0
    except Exception:
        shown = None
    check("the page shows the game stopped within a second", shown is not None, f"{shown:.2f} s" if shown else "not shown")
    text = pg.evaluate("document.getElementById('stopped') ? document.getElementById('stopped').textContent : ''")
    check("it says a thread stopped, and why", "a thread stopped" in text, text[:160])
    check("it offers a reload", "reload" in text)
    check("the status line says so", "the game stopped" in pg.evaluate("document.getElementById('status').textContent"))
    answer = pg.evaluate("window._crash")
    check("the call waiting on the program is refused", answer.startswith("refused"), answer[:100])
    syncs = pg.evaluate("Atomics.load(ctl, C.SYNCS)")
    time.sleep(0.5)
    check("no frame after", pg.evaluate("Atomics.load(ctl, C.SYNCS)") == syncs)
    pg.screenshot(path=os.path.join(WEB, "verify_crash.png"))
    pg.close()

    # The deploy's page again, from a subdirectory of its own whose wasi.js
    # starts with a line that makes the browser refuse something (rewritten
    # each run; the engine and the pak linked, not copied).
    def refusing(name, prefix):
        sub = os.path.join(WEB, name)
        os.makedirs(os.path.join(sub, "id1"), exist_ok=True)
        for f in os.listdir(WEB):
            src = os.path.join(WEB, f)
            if os.path.isfile(src) and f != "wasi.js" and not f.endswith(".png"):
                dst = os.path.join(sub, f)
                if not os.path.lexists(dst):
                    os.symlink(src, dst)
        if os.path.isdir(os.path.join(WEB, "icons")) and not os.path.lexists(os.path.join(sub, "icons")):
            os.symlink(os.path.join(WEB, "icons"), os.path.join(sub, "icons"))
        for f in os.listdir(os.path.join(WEB, "id1")):
            dst = os.path.join(sub, "id1", f)
            if not os.path.lexists(dst):
                os.symlink(os.path.join(WEB, "id1", f), dst)
        with open(os.path.join(sub, "wasi.js"), "w") as f:
            f.write(prefix + "\n")
            f.write(open(os.path.join(WEB, "wasi.js")).read())
        return f"http://127.0.0.1:{PORT}/{name}/index.html"

    def says(url, what):
        """Open `url`; wait for the status line to say the game did not start
        and `what`; (the status, seconds from the page's load to it, the page)."""
        pg = br.new_page(viewport={"width": 960, "height": 600})
        pg.goto(url, wait_until="load")
        t0 = time.time()
        try:
            pg.wait_for_function(f"document.getElementById('status').textContent.includes({what!r})", timeout=60000)
        except Exception:
            pass
        return pg.evaluate("document.getElementById('status').textContent"), time.time() - t0, pg

    # 2. A memory the browser will not make.
    url = refusing("verify-crash-refused", "WebAssembly.Memory = class { constructor() { throw new RangeError("
                   "'WebAssembly.Memory(): could not allocate memory'); } };")
    status, took, pg = says(url, "did not start")
    check("a refused memory: the game did not start, and says why", "did not start" in status and "memory it needs" in status, status[:200])
    pg.close()

    # 3. Thread workers the browser will not make (a `Worker` that throws, as
    # a browser out of workers does): the threads build does not run on one
    # thread instead; the page says the game did not start, and why, as it
    # does for a refused memory, and the game never ran.
    url = refusing("verify-crash-noworkers", "self.Worker = class { constructor() { throw new Error("
                   "'Worker(): too many workers'); } };")
    status, took, pg = says(url, "did not start")
    check("no thread workers: the game did not start, and says why",
          "did not start" in status and "worker threads the game needs" in status and "too many workers" in status, status[:240])
    check("it says so within a second of the page's load", took < 1.0, f"{took:.2f} s")
    check("and it never ran: not ready, run state crashed, not a frame — no one-thread mode",
          not pg.evaluate("quake.ready") and pg.evaluate("Atomics.load(ctl, C.RUN)") == 3
          and pg.evaluate("Atomics.load(ctl, C.FRAMES)") == 0,
          f"ready={pg.evaluate('quake.ready')} run={pg.evaluate('Atomics.load(ctl, C.RUN)')} frames={pg.evaluate('Atomics.load(ctl, C.FRAMES)')}")
    pg.screenshot(path=os.path.join(WEB, "verify_crash_noworkers.png"))
    pg.close()
    br.close()
httpd.shutdown()
if fails:
    print("FAIL:", "; ".join(fails))
    sys.exit(1)
print("done: a crashed thread ends the game with a message, and a refused memory or refused workers are said plainly")
