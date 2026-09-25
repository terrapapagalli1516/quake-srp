#!/usr/bin/env -S uv run --with playwright --script
"""Verify the web shell's input contract end-to-end in headless Chromium:

  1. ONE-GESTURE START — the click-to-play scrim appears only before the first
     user gesture ever; that click (or Enter/Space) unlocks audio AND reveals
     the running attract loop — id's demos, no menu until a key (the gesture
     itself is not one) — and the scrim never covers the canvas again
     (attract->walk->demo transitions included).
  2. ESC vs POINTER LOCK — losing the pointer lock without the page asking
     (the browser's reserved Esc, simulated via document.exitPointerLock())
     OPENS the in-game menu; plain Esc keydowns then navigate BACK in the menu
     while unlocked; closing it re-locks on the next canvas click (no auto-grab).
  3. SPACE/ARROWS NEVER HIT THE UI — 20 Space presses re-trigger no button
     (the 🔊 toggle was the offender) and arrows scroll the page in NO state
     (game / menu / console).
  4. CAPTURE CHIP — the "click to capture mouse" chip shows ONLY in unlocked
     walk mode with no menu/console up; never in attract/demo/locked states.
  5. KEYBOARD-ONLY PLAY — arrows move the camera and Ctrl fires (+attack)
     without the pointer ever being locked.
  6. THE CANVAS BOX — at 1440x900 the 960-wide framebuffer gets a 960x720
     box (a whole pixel per column: the 976 the window fits doubled one
     column in 60), at 1920x1080 the natural 1216x912 (1.27 is no near
     whole number), at 1024x768 an 800x600 box drawn smooth (a pixelated
     shrink drops columns).

Headless fullscreen is approximate: the F/fullscreen checks are best-effort
here (skipped with a note when the headless browser refuses) — see the manual
test script in the session notes for the real-monitor pass.

Usage: verify_input.py [webdir]   (defaults to this script's directory; pass a
temp dir holding index.html + a freshly built quake_wasm.wasm to test changes
without touching the deployed wasm).
"""
import functools, http.server, os, socketserver, sys, threading, time
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("QUAKE_VERIFY_PORT", "8171"))
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

def boot_page(pg):
    """Load the page and wait for the ready prompt (exp built, attract booted)."""
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    # `exp` is a top-level `let` binding — probe as a bare identifier, ending in
    # a boolean (a function-valued string predicate would get CALLED).
    pg.wait_for_function(
        "typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'",
        timeout=120000,
    )
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('ready')",
        timeout=30000,
    )

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=[
        "--no-sandbox",
        # Let the single first gesture genuinely unlock audio under headless.
        "--autoplay-policy=no-user-gesture-required",
    ])
    errs = []

    # ---- pass 1: the literal CLICK start (separate page — a first gesture
    # only happens once per load) --------------------------------------------
    pg = br.new_page(viewport={"width": 820, "height": 560})
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg)
    check("scrim up before any gesture",
          pg.evaluate("!overlay.classList.contains('hidden')"))
    pg.locator("#wrap").screenshot(path=os.path.join(WEB, "verify_input_overlay.png"))
    pg.locator("#overlay").click()
    s = pg.evaluate("""() => ({
        hidden: overlay.classList.contains('hidden'),
        audio: audioCtx ? audioCtx.state : 'none',
        menu: exp.menu_visible(), walk: exp.in_walk_mode(),
    })""")
    check("one click: scrim gone", s["hidden"])
    check("one click: audio unlocked", s["audio"] == "running", s["audio"])
    check("one click: attract revealed (the demo, no menu)",
          s["menu"] == 0 and s["walk"] == 0)
    check("the prompt says a key brings the menu",
          "any key for the menu" in pg.evaluate("document.getElementById('play').textContent"))
    # Mode transitions never resurrect the scrim.
    pg.locator("#walkBtn").click(); time.sleep(0.3)
    pg.locator("#demoBtn").click(); time.sleep(0.3)
    check("walk->demo transitions keep the scrim hidden",
          pg.evaluate("overlay.classList.contains('hidden')"))
    pg.close()

    # ---- pass 2: the full keyboard/pointer flow (Enter start) ---------------
    pg = br.new_page(viewport={"width": 820, "height": 560})
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg)

    # The scroll checks below are only load-bearing if the page CAN scroll.
    check("page is scrollable (scroll assertions meaningful)",
          pg.evaluate("document.documentElement.scrollHeight > innerHeight"))

    # (1) Enter is a first gesture too.
    pg.keyboard.press("Enter")
    s = pg.evaluate("""() => ({
        hidden: overlay.classList.contains('hidden'),
        audio: audioCtx ? audioCtx.state : 'none',
        menu: exp.menu_visible(),
    })""")
    check("Enter starts: scrim gone + audio unlocked, the demo with no menu",
          s["hidden"] and s["audio"] == "running" and s["menu"] == 0, str(s))
    check("no capture chip in attract mode",
          pg.evaluate("!lockChip.classList.contains('show')"))
    # Then any key during the demo brings up the menu (Key_Event).
    pg.keyboard.press("Enter")
    check("a key during the demo brings up the menu", pg.evaluate("exp.menu_visible()") == 1)

    # (3a) Space/arrows over the attract MENU: swallowed, never scroll.
    # (Scroll checks compare the DELTA across the key presses — Playwright's
    # own clicks legitimately auto-scroll elements into view between phases.)
    sy = pg.evaluate("scrollY")
    for _ in range(10):
        pg.keyboard.press("Space")
        pg.keyboard.press("ArrowDown")
    check("menu state: 10 Space + 10 arrows scroll nothing",
          pg.evaluate("scrollY") == sy)

    # Walk mode (boot lands in the menu); Esc — unlocked — closes it.
    pg.locator("#walkBtn").click()
    time.sleep(0.5)
    pg.keyboard.press("Escape")
    pg.wait_for_function("!exp.menu_visible()", timeout=5000)
    time.sleep(0.3)
    check("walk mode entered (in_walk_mode)", pg.evaluate("exp.in_walk_mode() === 1"))
    # (4) Chip: visible in unlocked walk mode with the menu down.
    pg.wait_for_function("lockChip.classList.contains('show')", timeout=5000)
    check("capture chip shows in unlocked walk mode", True)
    pg.locator("#wrap").screenshot(path=os.path.join(WEB, "verify_input_chip.png"))

    # (3b) The 🔊 button focus trap: click it, then jump 20 times — the button
    # must not re-trigger (Space keyup on a focused button fires its click).
    pg.evaluate("""() => {
        window._snd = 0;
        sndBtn.addEventListener('click', () => window._snd++);
    }""")
    pg.locator("#sndBtn").click()
    check("sound button blurred after click",
          pg.evaluate("document.activeElement !== sndBtn"))
    sy = pg.evaluate("scrollY")
    for _ in range(20):
        pg.keyboard.press("Space")
    s = pg.evaluate("({ snd: window._snd, sy: scrollY })")
    check("20 Space presses: sound button never re-triggers",
          s["snd"] == 1, f"{s['snd']} clicks")
    check("game state: 20 Space presses scroll nothing", s["sy"] == sy)

    # ...and the help drawer's <summary> can't be Space-toggled either.
    pg.locator("#conHelp summary").click()
    check("details summary blurred after click",
          pg.evaluate("document.activeElement.tagName !== 'SUMMARY'"))
    for _ in range(5):
        pg.keyboard.press("Space")
    check("drawer stays open through 5 Space presses (no focus trap)",
          pg.evaluate("document.getElementById('conHelp').open === true"))
    pg.evaluate("document.getElementById('conHelp').open = false; scrollTo(0, 0)")

    # (5) Keyboard-only play: arrows move the camera, Ctrl fires — and the
    # pointer is never locked through any of it.
    grab = """() => {
        const c = document.getElementById('c');
        return Array.from(c.getContext('2d').getImageData(0,0,c.width,c.height).data);
    }"""
    before = pg.evaluate(grab)
    pg.keyboard.down("ArrowUp")
    time.sleep(1.5)
    pg.keyboard.up("ArrowUp")
    time.sleep(0.3)
    after = pg.evaluate(grab)
    npx = min(len(before), len(after)) // 4
    moved = sum(1 for i in range(0, npx * 4, 4)
                if abs(before[i] - after[i]) > 12 or abs(before[i+1] - after[i+1]) > 12)
    check("arrows alone move the camera (keyboard play)",
          moved / max(1, npx) > 0.25, f"{moved/max(1,npx):.1%} px changed")
    # Ctrl -> +attack: swap exp for a spy copy so set_attack calls are
    # observable (wasm export properties are non-configurable, so a Proxy
    # can't intercept them — a plain spread copy can).
    pg.evaluate("""() => {
        window._real = exp; window._atk = [];
        exp = { ...window._real };
        // Merged input path: Ctrl is K_CTRL(133) through Key_Event and the
        // BINDINGS table (default.cfg: ctrl = +attack, rebindable) — spy
        // key_event's downs and ups.
        exp.key_event = (k, d, c) => { if (k === 133) window._atk.push(d); return window._real.key_event(k, d, c); };
    }""")
    pg.keyboard.down("Control"); time.sleep(0.15); pg.keyboard.up("Control")
    atk = pg.evaluate("window._atk")
    pg.evaluate("exp = window._real")
    check("Ctrl fires (+attack press/release)", atk == [1, 0], str(atk))
    check("pointer never locked during keyboard play",
          pg.evaluate("document.pointerLockElement === null"))

    # (2) The Esc pattern. A real canvas click captures the mouse...
    pg.locator("#c").click(position={"x": 320, "y": 240})
    try:
        pg.wait_for_function("document.pointerLockElement === document.getElementById('c')",
                             timeout=5000)
        locked = True
    except Exception:
        locked = False
    check("canvas click captures the mouse in walk mode", locked)
    if locked:
        # The page syncs the chip in its pointerlockchange handler, which runs
        # AFTER pointerLockElement is already visible to the waiter above.
        try:
            pg.wait_for_function("!lockChip.classList.contains('show')", timeout=2000)
            chip_hidden = True
        except Exception:
            chip_hidden = False
        check("chip hides while locked", chip_hidden)
        # ...and losing the lock WITHOUT the page asking (= the browser's
        # reserved Esc; simulated -- headless can't deliver a real lock-Esc)
        # opens the in-game menu.
        pg.evaluate("document.exitPointerLock()")
        pg.wait_for_function("exp.menu_visible() === 1", timeout=5000)
        check("Esc-driven lock loss opens the in-game menu", True)
        check("chip hides while the menu is up",
              pg.evaluate("!lockChip.classList.contains('show')"))
        pg.locator("#wrap").screenshot(path=os.path.join(WEB, "verify_input_menu.png"))
        # Plain Esc keydowns now navigate BACK in the menu (deliverable: no lock).
        pg.keyboard.press("Enter")    # Main > Single Player (submenu)
        check("menu: Enter descends to a submenu", pg.evaluate("exp.menu_visible() === 1"))
        pg.keyboard.press("Escape")   # submenu -> main screen (still visible)
        check("menu: Esc backs out of the submenu (menu still up)",
              pg.evaluate("exp.menu_visible() === 1"))
        pg.keyboard.press("Escape")   # main screen -> closed
        pg.wait_for_function("!exp.menu_visible()", timeout=5000)
        check("menu: Esc on the main screen closes it", True)
        # No auto-grab: the pointer is still free; the chip invites the click.
        check("no auto re-lock after the menu closes",
              pg.evaluate("document.pointerLockElement === null"))
        pg.wait_for_function("lockChip.classList.contains('show')", timeout=5000)
        check("chip returns in unlocked walk mode", True)
        # The promised re-capture: one canvas click locks again.
        pg.locator("#c").click(position={"x": 320, "y": 240})
        pg.wait_for_function("document.pointerLockElement === document.getElementById('c')",
                             timeout=5000)
        check("next canvas click re-captures the mouse", True)
        pg.evaluate("document.exitPointerLock()")
        pg.wait_for_function("exp.menu_visible() === 1", timeout=5000)
        pg.keyboard.press("Escape")   # leave the menu closed for the next phase
        time.sleep(0.3)

    # (3c) Console state: keys are owned by the console, and arrows can't scroll.
    pg.keyboard.press("`")
    check("console opens", pg.evaluate("exp.console_visible() === 1"))
    sy = pg.evaluate("scrollY")
    for _ in range(10):
        pg.keyboard.press("ArrowDown")
        pg.keyboard.press("Space")
    check("console state: arrows/Space scroll nothing", pg.evaluate("scrollY") == sy)
    pg.keyboard.press("Escape")
    check("Esc closes the console", pg.evaluate("exp.console_visible() === 0"))

    # (4b) Demo mode: chip hidden, scrim hidden, canvas clicks don't lock.
    pg.locator("#demoBtn").click()
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('playing demo')",
        timeout=30000)
    time.sleep(0.5)
    pg.locator("#c").click(position={"x": 320, "y": 240})
    time.sleep(0.5)
    s = pg.evaluate("""() => ({
        chip: lockChip.classList.contains('show'),
        scrim: !overlay.classList.contains('hidden'),
        locked: document.pointerLockElement !== null,
        walk: exp.in_walk_mode(),
    })""")
    check("demo mode: no chip, no scrim, canvas click does not lock",
          not s["chip"] and not s["scrim"] and not s["locked"] and s["walk"] == 0,
          str(s))

    # Fullscreen is approximate under headless: try F and report, don't insist.
    pg.keyboard.press("f")
    time.sleep(0.5)
    if pg.evaluate("document.fullscreenElement !== null"):
        check("F enters fullscreen; transient hint shows",
              pg.evaluate("fsHint.classList.contains('show')"))
        pg.keyboard.press("f")
        time.sleep(0.5)
        check("F leaves fullscreen; hint hides",
              pg.evaluate("document.fullscreenElement === null"
                          " && !fsHint.classList.contains('show')"))
    else:
        print("SKIP fullscreen checks (headless refused requestFullscreen) — "
              "covered by the manual test script")

    # (6) The canvas box (fitCanvas): the largest 4:3 box the window fits,
    # snapped to a whole number of pixels per framebuffer column when it is
    # at most 1/8 past one (no doubled column in the pixelated upscale),
    # smoothed when it is narrower than the framebuffer (no dropped column).
    box = lambda: pg.evaluate("""(() => { const c = document.getElementById('c');
        return [parseFloat(c.style.width), parseFloat(c.style.height),
                getComputedStyle(c).imageRendering, exp.width()]; })()""")
    for (vw, vh), want in [((1440, 900), (960, 720, "pixelated")),
                           ((1920, 1080), (1216, 912, "pixelated")),
                           ((1024, 768), (800, 600, "auto"))]:
        pg.set_viewport_size({"width": vw, "height": vh})
        time.sleep(0.3)
        b = box()
        check(f"{vw}x{vh}: the {b[3]}-wide framebuffer in a {want[0]}x{want[1]} box, {want[2]}",
              b[3] == 960 and (round(b[0]), round(b[1]), b[2]) == want, str(b))

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
