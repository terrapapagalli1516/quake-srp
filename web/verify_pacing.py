#!/usr/bin/env -S uv run --with playwright --script
"""Verify how a refresh gets its frame (index.html's `pacing`; PLATFORM.md,
"A frame"), in headless Chromium:

  1. quick frames: the refresh that asks for a frame waits for it and shows
     it (the wait is the frame's whole time; a frame a refresh);
  2. slow frames (`stall_ms`, a frame longer than a refresh): within two
     seconds the refresh stops waiting (`quake.pacing.relaxed`; the main
     thread's wait is nothing), and the program draws back to back: the
     frames shown a second are what its frame time allows, not one every
     other refresh;
  3. quick again: the wait is back within two seconds;
  4. input still reaches the canvas and is measured (the latency probe: a
     key's time to the present of the frame that consumed it is at least the
     frame's own time, with the wait and without);
  5. a browser without `Atomics.waitAsync` waits always, as before;
  6. no console errors.

`stall_ms` makes a host frame slow on demand and exists only in a
`--features bench` build (verify_audio_resilience.py says why and how to
build one); pass a deploy dir made from it.

Usage: verify_pacing.py <deploy-dir>   (a `--features bench` build)
"""
import statistics, sys, time
from playwright.sync_api import sync_playwright
import isolated

if len(sys.argv) < 2:
    sys.exit("usage: verify_pacing.py <deploy-dir>   (a --features bench build)")

WEB = isolated.webdir()
PORT = isolated.port(8573)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

STALL_MS = 25          # with the frame's own time, well past a 60 Hz refresh

# `secs` of the page's own loop: refreshes, frames shown, the main thread's
# wait in each refresh, the program's time for each frame shown, and how many
# refreshes ran relaxed.
WINDOW = """(secs) => new Promise(done => {
  quake.live = [];
  setTimeout(() => {
    const L = quake.live; quake.live = null;
    const S = L.filter(x => x.shown);
    done({ refreshes: L.length, shown: S.length, waits: L.map(x => x.wait), turns: S.map(x => x.turn),
           relaxed: L.filter(x => x.relaxed).length, refresh: quake.pacing.refresh });
  }, secs * 1000);
})"""

def boot(pg):
    pg.goto(f"http://127.0.0.1:{PORT}/index.html?2026", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)
    if pg.evaluate("quake.call('stall_ms', 0)") != 0:
        sys.exit("this deploy answers no stall_ms: build it with --features bench (verify_audio_resilience.py)")
    # A small picture, so a frame is quick on any machine: the checks are
    # about who waits, not about the renderer.
    pg.evaluate("quake.callLine('exec vid_pixelsize 4')")
    time.sleep(1.0)

def window(pg, secs=3.0):
    w = pg.evaluate(WINDOW, secs)
    w["wait"] = statistics.median(w["waits"]) if w["waits"] else 0.0
    w["turn"] = statistics.median(w["turns"]) if w["turns"] else 0.0
    w["fps"] = w["shown"] / secs
    return w

def settle(pg, relaxed, secs=2.0):
    """Wait for the page to take the mode; true when it did within `secs`."""
    return isolated.wait_until(pg, f"quake.pacing.relaxed === {'true' if relaxed else 'false'}", secs, raising=False)

with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    errs = []
    def page(init=None):
        ctx = br.new_context(viewport={"width": 820, "height": 560})
        if init:
            ctx.add_init_script(init)
        pg = ctx.new_page()
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        boot(pg)
        return pg

    pg = page()
    refresh = pg.evaluate("quake.pacing.refresh")

    # 1. Quick frames: waited for.
    check("quick frames: the refresh waits for its frame", settle(pg, False))
    q = window(pg)
    check("quick: no refresh ran relaxed", q["relaxed"] == 0, f"{q['relaxed']} of {q['refreshes']}")
    check("quick: a frame a refresh", q["shown"] >= 0.9 * q["refreshes"], f"{q['shown']} of {q['refreshes']}")
    check("quick: the wait is the frame's time", abs(q["wait"] - q["turn"]) < 0.5 and q["turn"] < 0.6 * refresh,
          f"wait {q['wait']:.2f} ms, frame {q['turn']:.2f} ms, refresh {refresh:.2f} ms")

    # 2. Slow frames: not waited for, drawn back to back.
    pg.evaluate(f"quake.call('stall_ms', {STALL_MS})")
    check("slow frames: the refresh stops waiting within 2 s", settle(pg, True))
    s = window(pg)
    check("slow: every refresh ran relaxed", s["relaxed"] == s["refreshes"], f"{s['relaxed']} of {s['refreshes']}")
    check("slow: the main thread does not wait", s["wait"] < 1.0 and max(s["waits"]) < 5.0,
          f"median {s['wait']:.3f} ms, max {max(s['waits']):.2f} ms")
    check("slow: a frame takes longer than a refresh", s["turn"] > refresh, f"{s['turn']:.1f} ms")
    back_to_back = 1000.0 / s["turn"]
    check("slow: frames come as fast as the program draws them",
          s["fps"] >= 0.85 * back_to_back, f"{s['fps']:.1f} shown a second, {back_to_back:.1f} drawn back to back")

    # 4a. The latency probe in relaxed mode: a key reaches the canvas no
    # sooner than a frame takes.
    LAT = """async () => {
      quake.latency = []; quake.dispatch = [];
      for (let i = 0; i < 12; i++) {
        document.dispatchEvent(new KeyboardEvent('keydown', { code: 'ArrowLeft', key: 'ArrowLeft', bubbles: true }));
        await new Promise(r => setTimeout(r, 90));
        document.dispatchEvent(new KeyboardEvent('keyup', { code: 'ArrowLeft', key: 'ArrowLeft', bubbles: true }));
        await new Promise(r => setTimeout(r, 90));
      }
      const l = quake.latency; quake.latency = null; return l;
    }"""
    lat = pg.evaluate(LAT)
    check("slow: keys are measured to their frame's present", len(lat) >= 12 and min(lat) >= 0.9 * s["turn"],
          f"{len(lat)} samples, min {min(lat) if lat else 0:.1f} ms, frame {s['turn']:.1f} ms")

    # 3. Quick again.
    pg.evaluate("quake.call('stall_ms', 0)")
    check("quick again: the wait is back within 2 s", settle(pg, False))
    q2 = window(pg)
    check("quick again: a frame a refresh, waited for", q2["relaxed"] == 0 and q2["shown"] >= 0.9 * q2["refreshes"]
          and abs(q2["wait"] - q2["turn"]) < 0.5, f"{q2['shown']} of {q2['refreshes']}, wait {q2['wait']:.2f} ms")
    lat = pg.evaluate(LAT)
    check("quick: keys are measured too", len(lat) >= 12 and min(lat) >= 0.9 * q2["turn"],
          f"{len(lat)} samples, median {statistics.median(lat) if lat else 0:.1f} ms")
    pg.context.close()

    # 5. No Atomics.waitAsync: the old way, always.
    pg = page("Atomics.waitAsync = undefined;")
    pg.evaluate(f"quake.call('stall_ms', {STALL_MS})")
    time.sleep(2.0)
    o = window(pg)
    check("no Atomics.waitAsync: slow frames are still waited for", o["relaxed"] == 0 and abs(o["wait"] - o["turn"]) < 1.0,
          f"wait {o['wait']:.1f} ms, frame {o['turn']:.1f} ms")
    pg.evaluate("quake.call('stall_ms', 0)")
    pg.context.close()

    check("no console errors", not errs, "; ".join(errs[:3]))
    br.close()

print(f"\n{passed} passed, {failed} failed")
httpd.shutdown()
sys.exit(1 if failed else 0)
