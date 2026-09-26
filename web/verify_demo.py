#!/usr/bin/env -S uv run --with playwright --script
"""Verify demo-playback parity end-to-end in headless Chromium:

  1. the page boots into the attract demo with no menu (id's key_dest starts
     at key_game) and the FIRST rendered frames are in-world (no void-camera
     intro — the signon gate); a key brings up the menu, Escape closes it;
  2. recorded svc_sound one-shots actually PLAY through Web Audio during the
     attract loop (the page's __sndStats.plays counter);
  3. the recorded status bar is drawn: the bottom sbar band stays stable
     across frames while the 3-D scene above it changes (the camera is moving);
  4. the (entity, channel) stop/override plumbing is wired (the page keeps
     its source registry, and id's demos send no STOP_SOUND record);
  5. no console errors; a screenshot is saved for visual gun/sbar inspection.

Usage: verify_demo.py [webdir]   (defaults to the repo's web/; pass a deploy
dir — PLATFORM.md — to test changes without touching the deployed page)."""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8169)
httpd = isolated.serve(WEB, PORT)

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

def nonblack_fraction(f, y0=0, y1=None):
    """Fraction of non-black pixels within rows y0..y1 (default: all)."""
    w, d = f["w"], f["data"]
    y1 = f["h"] if y1 is None else y1
    lit = sum(1 for i in range(y0 * w * 4, y1 * w * 4, 4) if d[i] or d[i+1] or d[i+2])
    return lit / max(1, (y1 - y0) * w)

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
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    pg.wait_for_function("quake.firstFrameAt > 0", timeout=30000)

    # 1. The attract demo starts in-world: the very first canvas frames carry a
    # real scene (the old signon void rendered ~1.2 s of black from a zeroed
    # camera). Sample as early as possible after boot, between the top of the
    # screen and the status bar. No menu over it: Quake starts at key_game.
    first = pg.evaluate(GRAB)
    frac = nonblack_fraction(first, int(first["h"] * 0.4), int(first["h"] * 0.75)) if first else 0.0
    check("first rendered frame is in-world (no void intro)", frac > 0.15,
          f"{frac:.0%} non-black")
    check("the attract demo plays with no menu", pg.evaluate("exp.menu_visible()") == 0)

    # 2. Recorded one-shot sounds fire through Web Audio. The page only builds
    # its AudioContext on a user gesture; create + resume it directly (the
    # autoplay flag lets resume() succeed) — the page's next refresh tells the
    # program audio is running, and it starts sending the recorded svc_sound
    # events.
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

    # Any key during demo playback brings up the main menu (Key_Event); Escape
    # on Main closes it and the demo view is bare again. (The overlay's first
    # gesture is dismissed by a click, which is no key.)
    pg.evaluate("document.getElementById('overlay').click()")
    pg.keyboard.press("KeyA")
    check("a key during the demo brings up the menu", pg.evaluate("exp.menu_visible()") == 1)

    # 3. Close the menu (Escape) so the bare demo view + sbar show, then prove
    # the status bar: the bottom band (the 24-row sbar strip) is drawn from the
    # recorded stats and stays STABLE across frames, while the 3-D scene above
    # changes (the recorded camera is moving). Without the sbar the band would
    # be live 3-D and change like the rest.
    # A whole-screen palette shift (V_UpdatePalette: demo1's pickups flash the
    # gold bonus shift, hits the damage shift) repaints every pixel of the bar
    # too, so a pair of grabs that straddles a flash says nothing about the
    # bar: take up to four pairs and judge the steadiest.
    pg.keyboard.press("Escape")
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
    time.sleep(0.3)
    best = None
    for _ in range(4):
        a = pg.evaluate(GRAB)
        time.sleep(1.5)
        b = pg.evaluate(GRAB)
        h = a["h"]
        # the sbar strip: id's 24 rows, or 24 of the 320x200 screen under the
        # "scaled 2-D" extra
        scaled = pg.evaluate("exp.scaled_2d()")
        bar_h = max(1, round(h * 24 / 200)) if scaled else 24
        scene = changed_fraction(a, b, 0, int(h * 0.6))
        band = changed_fraction(a, b, h - bar_h, h)
        if best is None or band - scene < best[0] - best[1]:
            best = (band, scene, a)
        if band < scene / 3:
            break
    band, scene, a = best
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

    # 4. The stop/override plumbing is wired: the page's stop path exists, and
    # no STOP_SOUND record came (id's demos send no svc_stopsound — engine-
    # asserted — so there is none to carry).
    plumbing = pg.evaluate("""() => ({
        haveStops: typeof quake.audio.stopKey === 'function',
        stopRecords: window.__sndStats.stopRecords || 0,
        registry: quake.audio.playingByKey instanceof Map,
    })""")
    check("the stop path is present", plumbing["haveStops"])
    check("no stop records (id demos send none)", plumbing["stopRecords"] == 0,
          str(plumbing["stopRecords"]))
    check("page keeps the (entity,channel) source registry", plumbing["registry"])

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
