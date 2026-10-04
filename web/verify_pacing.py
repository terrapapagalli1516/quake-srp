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
  11. the frame-rate cap (`host_maxfps`) holds the frames drawn, not the
      game: through the page's own tick, 60 against a 120 Hz refresh draws
      every second refresh, evenly (the gaps counted), every third at 144 Hz
      (48 a second) and every second at 110 Hz (55), while the game runs a
      host frame every refresh (`host_frames`); a cap above the refresh does
      nothing; 72 is id's gate (every second refresh at 144 Hz and at 120,
      the game's frames with it). Then the page's loop at 120, 144 and 110 Hz
      (its refreshes come from a clock at that rate: a headless browser's
      are 60): a touch screen starts with no cap, as a desktop does, every
      refresh drawn; at 60 it draws every second, third and second refresh,
      a host frame every one; at 120 relaxed as well as waited for; 144
      there draws every refresh;
  12. no console errors.

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
           relaxed: L.filter(x => x.relaxed).length, refresh: quake.pacing.refresh, drew: L.map(x => x.shown) });
  }, secs * 1000);
})"""

# The refreshes of a panel at HZ: the page's requestAnimationFrame answered
# on an exact grid (8.33 ms at 120 Hz; every callback asked for before a
# refresh runs at it, with its time; never two at one), where a headless
# browser's own run at 60 Hz.
REFRESH = """(() => {
  const period = 1000 / HZ;
  let queue = [], timer = null, last = 0;
  window.requestAnimationFrame = cb => {
    queue.push(cb);
    if (timer === null) {
      const now = performance.now(), at = Math.max(last + period, (Math.floor(now / period) + 1) * period);
      timer = setTimeout(() => { const q = queue; queue = []; timer = null; last = at; for (const f of q) f(at); },
                         Math.max(0, at - now));
    }
    return queue.length;
  };
})();"""

# `n` ticks of `dt` through the page's own tick (its loop paused): which
# ones drew a frame, after `warm` ticks for the cap to take, and how many
# host frames the `n` ran (`host_frames`: the game's ticks, drawn or not).
TICKS = """async ([n, dt, warm]) => {
  quake.pause();
  await new Promise(r => setTimeout(r, 50));
  for (let i = 0; i < warm; i++) quake.tick(dt);
  const f0 = await quake.call('host_frames');
  const drew = [];
  for (let i = 0; i < n; i++) drew.push(quake.tick(dt).presented);
  const game = (await quake.call('host_frames')) - f0;
  quake.resume();
  return { drew, game };
}"""

def gaps(drew):
    """The refreshes between one frame and the next."""
    at = [i for i, d in enumerate(drew) if d]
    return [b - a for a, b in zip(at, at[1:])]

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

def boot(pg, query="?slop"):
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
    def page(init=None, query="?slop", **context):
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
    always_waits("?wait", page(query="?slop&wait"))

    # 10. A touch screen is taken for one.
    pg = page(query="?slop&touch")
    check("?touch: taken for a touch screen", pg.evaluate("quake.pacing.scarceCores") is True)
    pg.context.close()
    if CHROMIUM:
        pg = page(viewport={"width": 844, "height": 390}, device_scale_factor=3, is_mobile=True, has_touch=True)
        check("an emulated phone: taken for a touch screen", pg.evaluate("quake.pacing.scarceCores") is True)
        pg.context.close()

    # 11. The frame-rate cap. Exactly, through the page's own tick.
    pg = page()
    host_frames = lambda pg: int(pg.evaluate("quake.call('host_frames')"))
    def cadence(name, cap, hz, every, game_every=1):
        """`hz` refreshes' worth of ticks twice over: the drawn ones every
        `every`th, evenly, and the game's host frames every `game_every`th."""
        pg.evaluate(f"quake.callLine('exec host_maxfps {cap}')")
        n, warm = int(hz) * 2, 8
        r = pg.evaluate(TICKS, [n, 1 / hz, warm])
        g, ran = gaps(r["drew"]), r["game"]
        check(f"{name}: drawn every {['', '', 'second ', 'third '][every] if every > 1 else ''}refresh, evenly",
              len(g) >= n // every - 2 and all(x == every for x in g),
              f"host_maxfps {cap} at {hz} Hz: {len(g) + 1} drawn in {n} refreshes, gaps {sorted(set(g))}")
        check(f"{name}: the game {'every refresh' if game_every == 1 else 'with the frames drawn'}",
              abs(ran - n / game_every) <= 1, f"{ran} host frames in {n} refreshes")
    cadence("60 against 120 Hz", 60, 120.0, 2)
    cadence("60 against 144 Hz (48 drawn a second: 60 does not divide 144)", 60, 144.0, 3)
    cadence("60 against 110 Hz (55 drawn a second: a phone's page with a finger down)", 60, 110.0, 2)
    cadence("a cap above the refresh does nothing (144 against 120 Hz)", 144, 120.0, 1)
    cadence("id's 72 against 144 Hz: id's gate", 72, 144.0, 2, game_every=2)
    cadence("id's 72 against 120 Hz", 72, 120.0, 2, game_every=2)
    cadence("no cap", 0, 120.0, 1)
    pg.context.close()

    # The page's own loop at 144, 110 and 120 Hz, a touch screen capped at 60.
    def evenly(w, every):
        g = gaps(w["drew"])
        share = sum(1 for x in g if x == every) / len(g) if g else 0.0
        return share >= 0.9 and w["shown"] >= 0.85 * w["refreshes"] / every, \
            f"{w['shown']} drawn in {w['refreshes']} refreshes ({w['refresh']:.2f} ms), {share:.0%} of the gaps {every}"
    def window_with_game(pg):
        """A window of the page's loop, and the host frames run in it."""
        f0 = host_frames(pg)
        w = window(pg)
        w["game"] = host_frames(pg) - f0
        return w
    for hz, every in [(144, 3), (110, 2)]:
        pg = page(REFRESH.replace("HZ", str(hz)), query="?slop&touch")
        pg.evaluate("quake.callLine('exec host_maxfps 60')")
        time.sleep(0.5)
        w = window_with_game(pg)
        ok, detail = evenly(w, every)
        check(f"a touch screen at {hz} Hz: drawn every {['', '', 'second', 'third'][every]} refresh", ok, detail)
        check(f"...and the game every refresh at {hz} Hz", w["game"] >= 0.95 * w["refreshes"],
              f"{w['game']} host frames in {w['refreshes']} refreshes")
        pg.context.close()
    pg = page(REFRESH.replace("HZ", "120"), query="?slop&touch")
    cap = pg.evaluate("quake.text('cvar', 'host_maxfps')")
    check("a touch screen starts with no cap, as a desktop does (the slop preset)", cap == "0", f"host_maxfps {cap}")
    ok, detail = evenly(window(pg), 1)
    check("...every refresh drawn at 120 Hz", ok, detail)
    pg.evaluate("quake.callLine('exec host_maxfps 60')")
    time.sleep(0.5)
    w = window_with_game(pg)
    ok, detail = evenly(w, 2)
    check("a touch screen at 120 Hz, waited for: drawn every second refresh", ok and w["relaxed"] == 0, detail)
    check("...and the game every refresh", w["game"] >= 0.95 * w["refreshes"], f"{w['game']} host frames in {w['refreshes']} refreshes")
    pg.evaluate("quake.pacing.force = true")
    settle(pg, True)
    w = window(pg)
    ok, detail = evenly(w, 2)
    check("...relaxed (not waited for): drawn every second refresh still", ok and w["relaxed"] == w["refreshes"], detail)
    pg.evaluate("quake.pacing.force = null")
    pg.evaluate("quake.callLine('exec host_maxfps 144')")
    time.sleep(0.5)
    ok, detail = evenly(window(pg), 1)
    check("...144, above the refresh: every refresh", ok, detail)
    pg.context.close()

    check("no console errors", not errs, "; ".join(errs[:3]))
    br.close()

print(f"\n{passed} passed, {failed} failed")
httpd.shutdown()
sys.exit(1 if failed else 0)
