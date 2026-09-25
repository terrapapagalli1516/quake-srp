#!/usr/bin/env -S uv run --with playwright --script
"""Verify demo-playback parity end-to-end in headless Chromium:

  1. the page boots into the attract demo and the FIRST rendered frames are
     in-world (no void-camera intro — the signon gate);
  2. recorded svc_sound one-shots actually PLAY through Web Audio during the
     attract loop (the page's __sndStats.plays counter);
  3. the recorded status bar is drawn: the bottom sbar band stays stable
     across frames while the 3-D scene above it changes (the camera is moving);
  4. the (entity, channel) stop/override plumbing is wired (poll_stop_sound /
     sound_entity / sound_channel exports exist and drain clean);
  5. no console errors; a screenshot is saved for visual gun/sbar inspection.

Usage: verify_demo.py [webdir]   (defaults to the repo's web/; pass a temp dir
holding index.html + a freshly built quake_wasm.wasm to test changes without
touching the deployed wasm)."""
import functools, http.server, os, socketserver, sys, threading, time
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("QUAKE_VERIFY_PORT", "8169"))
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

GRAB = """() => {
    const c = document.getElementById('c');
    const g = c.getContext('2d'); if (!g) return null;
    return { w: c.width, h: c.height,
             data: Array.from(g.getImageData(0, 0, c.width, c.height).data) };
}"""

def nonblack_fraction(f):
    d, n = f["data"], f["w"] * f["h"]
    lit = sum(1 for i in range(0, len(d), 4) if d[i] or d[i+1] or d[i+2])
    return lit / max(1, n)

def changed_fraction(a, b, y0, y1):
    """Fraction of pixels that differ between two grabs within rows y0..y1."""
    w = a["w"]; da, db = a["data"], b["data"]
    total = (y1 - y0) * w
    diff = 0
    for y in range(y0, y1):
        row = y * w * 4
        for x in range(0, w * 4, 4):
            i = row + x
            if da[i] != db[i] or da[i+1] != db[i+1] or da[i+2] != db[i+2]:
                diff += 1
    return diff / max(1, total)

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=[
        "--no-sandbox",
        # Let audioCtx.resume() succeed without a user gesture so the recorded
        # one-shots actually play under headless.
        "--autoplay-policy=no-user-gesture-required",
    ])
    pg = br.new_page(viewport={"width": 1020, "height": 700})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function(
        "typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'",
        timeout=120000,
    )
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('ready')",
        timeout=30000,
    )

    # 1. The attract demo starts in-world: the very first canvas frames carry a
    # real scene (the old signon void rendered ~1.2 s of black from a zeroed
    # camera). Sample as early as possible after boot.
    first = pg.evaluate(GRAB)
    frac = nonblack_fraction(first) if first else 0.0
    check("first rendered frame is in-world (no void intro)", frac > 0.40,
          f"{frac:.0%} non-black")

    # 2. Recorded one-shot sounds fire through Web Audio. The page only builds
    # its AudioContext on a user gesture; create + resume it directly (the
    # autoplay flag lets resume() succeed) — drainGameSounds then marks audio
    # ready and the wasm side starts queueing the recorded svc_sound events.
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")
    plays = 0
    for _ in range(20):   # demo1 averages ~6 recorded sounds/sec
        time.sleep(0.5)
        plays = pg.evaluate("window.__sndStats ? window.__sndStats.plays : -1")
        if plays and plays > 0:
            break
    check("recorded svc_sound one-shots play during the attract loop",
          plays > 0, f"{plays} plays")

    # 3. Close the menu (Escape) so the bare demo view + sbar show, then prove
    # the status bar: the bottom band (24/200 of the height) is drawn from the
    # recorded stats and stays STABLE across frames, while the 3-D scene above
    # changes (the recorded camera is moving). Without the sbar the band would
    # be live 3-D and change like the rest.
    pg.keyboard.press("Escape")
    pg.wait_for_function("!exp.menu_visible || !exp.menu_visible()", timeout=5000)
    time.sleep(0.3)
    a = pg.evaluate(GRAB)
    time.sleep(1.5)
    b = pg.evaluate(GRAB)
    h = a["h"]
    bar_h = max(1, round(h * 24 / 200))      # the sbar band (virtual 24 rows)
    scene = changed_fraction(a, b, 0, int(h * 0.6))
    band = changed_fraction(a, b, h - bar_h, h)
    check("3-D scene changes between frames (camera moving)", scene > 0.05,
          f"{scene:.1%} changed")
    check("sbar band is stable while the scene moves (bar is drawn)",
          band < scene / 3, f"band {band:.1%} vs scene {scene:.1%}")
    lit = sum(1 for y in range(h - bar_h, h) for x in range(0, a["w"] * 4, 4)
              if a["data"][y * a["w"] * 4 + x] or a["data"][y * a["w"] * 4 + x + 1]
              or a["data"][y * a["w"] * 4 + x + 2]) / max(1, bar_h * a["w"])
    check("sbar band has content (not void)", lit > 0.5, f"{lit:.0%} lit")

    # Screenshot for visual gun + sbar inspection (saved beside the webdir).
    shot = os.path.join(WEB, "verify_demo.png")
    pg.screenshot(path=shot)
    print("screenshot:", shot)

    # 4. The stop/override plumbing is wired: the exports exist, and the stop
    # queue drains clean (id's demos send no svc_stopsound — engine-asserted —
    # so it must be empty here).
    plumbing = pg.evaluate("""() => ({
        haveStops: typeof exp.poll_stop_sound === 'function',
        haveKey: typeof exp.sound_entity === 'function'
              && typeof exp.sound_channel === 'function',
        drained: typeof exp.poll_stop_sound === 'function'
              ? exp.poll_stop_sound() : -2,
        registry: typeof playingByKey !== 'undefined',
    })""")
    check("stop/override exports present", plumbing["haveStops"] and plumbing["haveKey"])
    check("stop queue drains clean (id demos send none)", plumbing["drained"] == -1,
          str(plumbing["drained"]))
    check("page keeps the (entity,channel) source registry", plumbing["registry"])

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
