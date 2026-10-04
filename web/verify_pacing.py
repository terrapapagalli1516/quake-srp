#!/usr/bin/env -S uv run --with playwright --script
"""Verify how a refresh gets its frame (index.html's `pacing`; PLATFORM.md,
"A frame"), in a headless browser. A refresh waits for a quick frame and
does not wait for a slow one; where the line lies depends on the device:

  A desktop (the wait's spin is one core of many):
  1. quick frames: the refresh that asks for a frame waits for it and draws
     it (the wait is the frame's whole time; a frame a refresh);
  2. frames longer than a refresh but shorter than the wait's limit (30 ms)
     are still waited for: not waiting would only show them later;
  3. frames the wait would give up on: the refresh stops waiting (the main
     thread's wait is nothing), and the program draws back to back — the
     frames shown a second are what its frame time allows;
  4. quick again: the wait is back.

  A touch screen (the spin takes a core the game needs; `pacing.scarceCores`,
  set from the page's touch-screen test):
  5. a frame over 0.8 of a refresh is not waited for, and still a frame a
     refresh is shown; a frame longer than a refresh is drawn back to back;
  6. quick again: the wait is back.

  Everywhere:
  7. input reaches the canvas and is measured (the latency probe: a key's
     time to the draw of the frame that consumed it is never under a frame's
     own time, with the wait and without);
  8. a browser without `Atomics.waitAsync` waits always, as before;
  9. `?wait` in the address: slow frames are waited for too;
  10. the page takes a touch screen for one (`?touch`; an emulated phone in
      Chromium), and a desktop for none;
  11. no console errors.

`stall_ms` makes a host frame slow on demand and exists only in a
`--features bench` build (verify_audio_resilience.py says why and how to
build one). On a plain build the frames cannot be made slow, so the page is
told to stop waiting instead (`quake.pacing.force`): the checks of the
relaxed refresh with quick frames, without the lines (said in the output).

The waits for a mode to change are generous (the machine may be busy); the
frame times compared are the ones of the same stretch of frames, so a busy
machine moves both sides alike.

Usage: verify_pacing.py [deploy-dir]   (a `--features bench` build checks it all)
"""
import os, statistics, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8573)
httpd = isolated.serve(WEB, PORT)
CHROMIUM = os.environ.get("QUAKE_BROWSER", "chromium") == "chromium"

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

# Host frames held this long (ms; the frame's own time comes on top), against
# a 60 Hz refresh (16.7 ms) and the wait's limit (WAIT_MS, 30 ms):
OVER_A_REFRESH = 18    # 1.2 refreshes, well under the limit
OVER_THE_LIMIT = 34    # the wait gives up on every frame (and two refreshes are not enough: a page that asked only at a refresh would show 20 a second, not 28)
MOST_OF_A_REFRESH = 13 # about 0.9 of a refresh
SETTLE = 6.0           # seconds a mode may take to change (about 1 s on a quiet machine)

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

# Twelve key presses, and the frames drawn meanwhile: each key's time to the
# draw of the frame that consumed it, and every frame's own time.
KEYS = """async () => {
  quake.latency = []; quake.dispatch = []; quake.live = [];
  for (let i = 0; i < 12; i++) {
    document.dispatchEvent(new KeyboardEvent('keydown', { code: 'ArrowLeft', key: 'ArrowLeft', bubbles: true }));
    await new Promise(r => setTimeout(r, 90));
    document.dispatchEvent(new KeyboardEvent('keyup', { code: 'ArrowLeft', key: 'ArrowLeft', bubbles: true }));
    await new Promise(r => setTimeout(r, 90));
  }
  const lat = quake.latency, L = quake.live; quake.latency = null; quake.live = null;
  return { lat, turns: L.filter(x => x.shown).map(x => x.turn) };
}"""

def boot(pg, query="?2026"):
    pg.goto(f"http://127.0.0.1:{PORT}/index.html{query}", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)
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

def stall(pg, ms):
    pg.evaluate(f"quake.call('stall_ms', {ms})")

def settle(pg, relaxed, secs=SETTLE):
    """Wait for the page to take the mode; true when it did within `secs`."""
    return isolated.wait_until(pg, f"quake.pacing.relaxed === {'true' if relaxed else 'false'}", secs, raising=False)

def waited(w):
    """Every refresh of the stretch waited for its frame: none relaxed, and the wait was the frame's time."""
    return w["relaxed"] == 0 and abs(w["wait"] - w["turn"]) < 1.0

def not_waited(name, w, back_to_back):
    """The checks of a stretch of refreshes that did not wait."""
    check(f"{name}: every refresh ran relaxed", w["relaxed"] == w["refreshes"], f"{w['relaxed']} of {w['refreshes']}")
    check(f"{name}: the main thread does not wait", w["wait"] < 1.0 and max(w["waits"]) < 5.0,
          f"median {w['wait']:.3f} ms, max {max(w['waits']):.2f} ms")
    possible = min(1000.0 / w["turn"], 1000.0 / w["refresh"]) if w["turn"] else 0.0
    check(f"{name}: frames come as fast as the program draws them (a refresh each at most)",
          w["fps"] >= 0.85 * possible, f"{w['fps']:.1f} shown a second, {possible:.1f} possible")
    if back_to_back:
        check(f"{name}: a frame takes longer than a refresh", w["turn"] > w["refresh"], f"{w['turn']:.1f} ms")

def keys(pg, name):
    """The latency probe: no key reaches the canvas sooner than a frame takes (the quickest frame of the
    same stretch: a key is consumed by one of them, and waits for it whole)."""
    k = pg.evaluate(KEYS)
    lat, turns = k["lat"], [t for t in k["turns"] if t > 0]
    floor = min(turns) if turns else 0.0
    check(f"{name}: keys are measured to their frame's draw", len(lat) >= 12 and min(lat) >= 0.9 * floor,
          f"{len(lat)} samples, min {min(lat) if lat else 0:.1f} ms, median {statistics.median(lat) if lat else 0:.1f} ms, "
          f"the quickest frame {floor:.1f} ms")

with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    errs = []
    def page(init=None, query="?2026", **context):
        ctx = br.new_context(**{"viewport": {"width": 820, "height": 560}, **context})
        if init:
            ctx.add_init_script(init)
        pg = ctx.new_page()
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        boot(pg, query)
        return pg

    pg = page()
    refresh = pg.evaluate("quake.pacing.refresh")
    # A bench build answers stall_ms with 0; a plain one knows no such call (NaN).
    bench = pg.evaluate("quake.call('stall_ms', 0)") == 0
    if not bench:
        print("NOTE not a --features bench build: frames stay quick, the relaxed refresh is forced, the lines are not checked")
    check("a desktop is not taken for a touch screen", pg.evaluate("quake.pacing.scarceCores") is False)

    # 1. Quick frames: waited for.
    check("quick frames: the refresh waits for its frame", settle(pg, False))
    q = window(pg)
    check("quick: no refresh ran relaxed", q["relaxed"] == 0, f"{q['relaxed']} of {q['refreshes']}")
    check("quick: a frame a refresh", q["shown"] >= 0.9 * q["refreshes"], f"{q['shown']} of {q['refreshes']}")
    check("quick: the wait is the frame's time", abs(q["wait"] - q["turn"]) < 0.5 and q["turn"] < 0.6 * refresh,
          f"wait {q['wait']:.2f} ms, frame {q['turn']:.2f} ms, refresh {refresh:.2f} ms")
    keys(pg, "quick")

    if bench:
        # 2. A desktop: longer than a refresh, shorter than the wait's limit: waited for.
        stall(pg, OVER_A_REFRESH)
        time.sleep(3.0)
        w = window(pg)
        check("a desktop, frames over a refresh: still waited for", waited(w) and w["turn"] > refresh,
              f"{w['relaxed']} of {w['refreshes']} relaxed, wait {w['wait']:.1f} ms, frame {w['turn']:.1f} ms")

        # 3. A desktop: frames the wait gives up on: not waited for, back to back.
        stall(pg, OVER_THE_LIMIT)
        check("a desktop, frames over the wait's limit: the refresh stops waiting", settle(pg, True))
        not_waited("over the limit", window(pg), True)
        keys(pg, "over the limit")

        # 4. Quick again.
        stall(pg, 0)
        check("quick again: the wait is back", settle(pg, False))
        w = window(pg)
        check("quick again: a frame a refresh, waited for", waited(w) and w["shown"] >= 0.9 * w["refreshes"],
              f"{w['shown']} of {w['refreshes']}, wait {w['wait']:.2f} ms")

        # 5. A touch screen: over 0.8 of a refresh is not waited for.
        pg.evaluate("quake.pacing.scarceCores = true")
        stall(pg, MOST_OF_A_REFRESH)
        check("a touch screen, frames of most of a refresh: the refresh stops waiting", settle(pg, True))
        w = window(pg)
        not_waited("most of a refresh", w, False)
        stall(pg, OVER_A_REFRESH)
        time.sleep(1.5)
        not_waited("a touch screen, over a refresh", window(pg), True)
        keys(pg, "a touch screen, over a refresh")

        # 6. Quick again.
        stall(pg, 0)
        check("a touch screen, quick again: the wait is back", settle(pg, False))
        w = window(pg)
        check("a touch screen, quick again: a frame a refresh, waited for", waited(w) and w["shown"] >= 0.9 * w["refreshes"],
              f"{w['shown']} of {w['refreshes']}, wait {w['wait']:.2f} ms")
    else:
        pg.evaluate("quake.pacing.force = true")
        check("told not to wait: the refresh stops waiting", settle(pg, True))
        not_waited("told not to wait", window(pg), False)
        keys(pg, "told not to wait")
        pg.evaluate("quake.pacing.force = null")
        check("left to itself again: the wait is back", settle(pg, False))
    pg.context.close()

    # 8. No Atomics.waitAsync: the old way, always (as a touch screen, with slow frames, on a bench build).
    def always_waits(name, pg):
        if bench:
            pg.evaluate("quake.pacing.scarceCores = true")
            stall(pg, OVER_A_REFRESH)
        else:
            pg.evaluate("quake.pacing.force = quake.pacing.force === false ? false : true")
        time.sleep(3.0)
        o = window(pg)
        check(f"{name}: slow frames are still waited for", waited(o), f"wait {o['wait']:.1f} ms, frame {o['turn']:.1f} ms")
        if bench:
            stall(pg, 0)
        pg.context.close()
    always_waits("no Atomics.waitAsync", page("Atomics.waitAsync = undefined;"))

    # 9. ?wait: the old way by choice.
    always_waits("?wait", page(query="?2026&wait"))

    # 10. A touch screen is taken for one.
    pg = page(query="?2026&touch")
    check("?touch: taken for a touch screen", pg.evaluate("quake.pacing.scarceCores") is True)
    pg.context.close()
    if CHROMIUM:
        pg = page(viewport={"width": 844, "height": 390}, device_scale_factor=3, is_mobile=True, has_touch=True)
        check("an emulated phone: taken for a touch screen", pg.evaluate("quake.pacing.scarceCores") is True)
        pg.context.close()

    check("no console errors", not errs, "; ".join(errs[:3]))
    br.close()

print(f"\n{passed} passed, {failed} failed")
httpd.shutdown()
sys.exit(1 if failed else 0)
