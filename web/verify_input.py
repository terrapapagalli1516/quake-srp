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
  6. THE CANVAS BOX (Classic: a video mode in the 4:3 box; the 2026 profile
     fills the window, verify_settings.py) — at 1440x900 the 960-wide framebuffer gets a 960x720
     box (a whole pixel per column: the 1088 the window fits would double
     one column in 7), at 1920x1080 the natural 1328x996 (1.38 is no near
     whole number), at 1024x768 a 912x684 box drawn smooth (a pixelated
     shrink drops columns).
  7. KEYS BY THEIR PLACE — an AZERTY key event (code KeyW, key 'z') is
     keynum 'w' in the game (id's scancodes), and types 'z' in the console.
  8. EVERY KEY UP WHEN A RELEASE MAY NEVER COME — vid_win.c's ClearAllStates
     when the pointer lock ends, the window blurs, the tab hides, fullscreen
     ends: the engine holds no key after each (keys and mouse buttons held
     down through it), a held key's next autorepeat presses it again, and a
     fresh press works.
  9. THE MOUSE AT ANY FRAME RATE — under pointer lock, 1000 counts from a
     1000 Hz mouse, delivered a refresh at a time as browsers deliver them,
     turn the view 160° at headless Chromium's 60 Hz refresh and again with
     its frame-rate limit off (several hundred refreshes a second; Firefox:
     `layout.frame_rate` 480). Synthetic `mousemove`s: headless Chromium's
     own lock cancels every real move (PLATFORM.md, "Input").
 10. THE MOUSE'S MOVEMENT, WHOLE — 1000 records of 0.3 counts (the page's
     record path; Chromium's and Firefox's movementX is an integer) turn
     48°; a pointermove and its mousemove count once; `?mousecheck`'s
     summary adds up; a real drag counts the same through pointermove's
     coalesced samples (what the page takes) as through mousemove's own
     movement (what it took before); `?plainlock` asks for the plain lock;
     `?mousecheck=mouse,trackpad` numbers, stamps and names its runs.

Headless fullscreen is approximate: the Alt+Enter/fullscreen checks are best-effort
here (skipped with a note when the headless browser refuses) — see the manual
test script in the session notes for the real-monitor pass.

Usage: verify_input.py [webdir]   (defaults to this script's directory; pass a
deploy dir — PLATFORM.md — to test changes without touching the deployed page).

`exp.name()` asks the program (a Promise, answered between two frames), so
the checks await it.
"""
import os, re, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8171)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def open_drawer(pg):
    """The page's shortcuts and the console-command list live in the "keys"
    drawer under the view; open it (once) before clicking them like a user."""
    if pg.evaluate("document.getElementById('drawer').hidden"):
        pg.locator("#keysBtn").click()
        time.sleep(0.2)


def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

def boot_page(pg):
    """Load the page and wait for the ready prompt (the game running, the
    attract loop booted)."""
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)

def mouse_turn(pg):
    """(8) Walk mode, the pointer locked, then a second of a 1000 Hz mouse
    moving right a count a millisecond: each refresh, the counts since the
    last one, in one to three `mousemove`s (a whole number each), as a
    browser coalesces them into the refresh. Returns (refreshes a second,
    degrees turned right, degrees expected)."""
    pg.evaluate("exp.boot().then(() => exp.menu_cancel())")
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)
    time.sleep(0.3)
    pg.locator("#c").click(position={"x": 320, "y": 240})
    pg.wait_for_function("document.pointerLockElement === document.getElementById('c')", timeout=5000)
    time.sleep(0.5)   # past the headless lock's own jump
    yaw = lambda: pg.evaluate("Promise.all([exp.listener_fwd_x(), exp.listener_fwd_y()])"
                              ".then(([x, y]) => Math.atan2(y, x) * 180 / Math.PI)")
    y0 = yaw()
    hz = pg.evaluate("""() => new Promise(done => {
        const c = document.getElementById('c');
        let sent = 0, refreshes = 0, t0 = null;
        function refresh(t) {
          if (t0 === null) t0 = t;
          refreshes++;
          const due = Math.min(1000, Math.floor(t - t0)), n = due - sent, parts = 1 + refreshes % 3;
          for (let i = 0; i < parts; i++) {
            const k = i < parts - 1 ? Math.floor(n / parts) : n - (parts - 1) * Math.floor(n / parts);
            if (k) c.dispatchEvent(new MouseEvent('mousemove', { movementX: k }));
          }
          sent = due;
          if (sent < 1000) requestAnimationFrame(refresh); else done(refreshes * 1000 / (t - t0));
        }
        requestAnimationFrame(refresh);
    })""")
    time.sleep(0.3)
    turned = (y0 - yaw() + 540) % 360 - 180
    want = 1000 * 0.16 * pg.evaluate("exp.mouse_sensitivity()")
    return hz, turned, want

def yaw(pg):
    """The drawn view's yaw, from the listener's facing (degrees)."""
    return pg.evaluate("Promise.all([exp.listener_fwd_x(), exp.listener_fwd_y()])"
                       ".then(([x, y]) => Math.atan2(y, x) * 180 / Math.PI)")

def mouse_count(pg):
    """The program's `mouse_count`: records read, their counts, the turn, frames."""
    return [float(v) for v in pg.evaluate("quake.text('mouse_count')").split()]

def turned_right(pg, y0):
    time.sleep(0.3)
    return (y0 - yaw(pg) + 540) % 360 - 180

def mouse_paths(pg):
    """(10) The mouse's movement through the page, after mouse_turn (the
    walk, the pointer locked): fractional counts reach the game whole; a
    pointermove and the mousemove that follows it count once; ?mousecheck's
    lines add up; and a real drag (the browser's own events, the lock
    refused) counts the same through pointermove's coalesced samples as
    through mousemove's own movement."""
    per = 0.16 * pg.evaluate("exp.mouse_sensitivity()")
    frac = pg.evaluate("new MouseEvent('mousemove', { movementX: 0.3 }).movementX")
    print(f"INFO this browser's movementX is {'a double' if frac == 0.3 else 'an integer'} (0.3 -> {frac})")
    # A thousand records of 0.3 counts, a 1000 Hz mouse's pace through the
    # refreshes (the page's own record path: a float, never rounded).
    y0, c0 = yaw(pg), mouse_count(pg)
    pg.evaluate("""() => new Promise(done => {
        let sent = 0, t0 = null;
        function refresh(t) {
          if (t0 === null) t0 = t;
          for (const due = Math.min(1000, Math.floor(t - t0)); sent < due; sent++) mouseMove(0.3, 0, 0);
          if (sent < 1000) requestAnimationFrame(refresh); else done();
        }
        requestAnimationFrame(refresh);
    })""")
    t = turned_right(pg, y0)
    c = [b - a for a, b in zip(c0, mouse_count(pg))]
    check(f"mouse: 1000 records of 0.3 counts turn {300 * per:.0f}°", abs(t - 300 * per) < 0.05,
          f"{t:.3f}° (the game read {c[0]:.0f} records, {c[1]:.2f} counts, turned {c[2]:.3f}° in {c[3]:.0f} frames)")
    # A pointermove and its mousemove, as a browser sends them: once.
    y0 = yaw(pg)
    pg.evaluate("""() => { const c = document.getElementById('c');
        for (let i = 0; i < 100; i++) {
          c.dispatchEvent(new PointerEvent('pointermove', { pointerType: 'mouse', movementX: 1 }));
          c.dispatchEvent(new MouseEvent('mousemove', { movementX: 1 }));
        } }""")
    t = turned_right(pg, y0)
    check(f"mouse: 100 pointermoves and their mousemoves turn {100 * per:.0f}°, once",
          abs(t - 100 * per) < 0.05, f"{t:.3f}°")
    # The mouse check: its summary of 50 moves of 2 counts (and headless
    # Chromium's own lock's moves of none, a refresh at a time).
    summary = pg.evaluate("""async () => {
        const c = document.getElementById('c'), done = quake.mousecheck(1);
        for (let i = 0; i < 50; i++) c.dispatchEvent(new MouseEvent('mousemove', { movementX: -2 }));
        return done; }""")
    print("INFO", summary)
    sent = re.search(r"records (\d+) sent, 0 dropped, (\d+) read", summary)
    want = ("× 100.00 counts |", "read × 100.00 |", f"turned {100 * per:.2f}°, {per:.4f}°/count")
    check("mousecheck: its summary adds up",
          all(w in summary for w in want) and sent and sent.group(1) == sent.group(2), summary)
    # A real drag, the lock refused: the browser's own pointermove (its
    # coalesced samples) and mousemove, read side by side by the check.
    pg.evaluate("expectUnlock = true; document.exitPointerLock()")
    pg.wait_for_function("document.pointerLockElement === null", timeout=5000)
    pg.evaluate("document.getElementById('c').requestPointerLock = () => Promise.resolve()")
    box = pg.locator("#c").bounding_box()
    x, y = box["x"] + 100, box["y"] + box["height"] / 2
    pg.mouse.move(x, y)
    pg.mouse.down()
    time.sleep(0.3)
    y0 = yaw(pg)
    pg.evaluate("() => { window.mouseRun = quake.mousecheck(2); }")
    for i in range(1, 41):
        pg.mouse.move(x + 5 * i, y)
    pg.mouse.up()
    summary = pg.evaluate("window.mouseRun")
    t = turned_right(pg, y0)
    print("INFO", summary)
    field = lambda pat: float(re.search(pat, summary).group(1))
    moved = field(r"mousemove \d+ × ([\d.]+) counts")
    sampled = field(r"pointermove \d+ × ([\d.]+) counts")
    read = field(r"read × ([\d.]+)")
    check("mouse: a real drag counts the same through pointermove's samples as mousemove's",
          moved == 200 and sampled == moved and read == moved and abs(t - 200 * per) < 0.05,
          f"mousemove {moved}, coalesced {sampled}, the game {read}; turned {t:.3f}°")
    # ?plainlock: the lock never asks for unadjusted movement (the check's
    # other half, the system's accelerated pointer).
    other = pg.context.browser.new_page()
    other.route("**/*", lambda r: r.continue_() if r.request.resource_type == "document" else r.abort())
    asked = {}
    for q in ("", "?plainlock"):
        other.goto(f"http://127.0.0.1:{PORT}/index.html{q}", wait_until="load")
        asked[q] = other.evaluate("rawMouse")
    check("?plainlock: the lock does not ask for unadjusted movement",
          asked == {"": True, "?plainlock": False}, str(asked))
    # ?mousecheck=mouse,trackpad: the box says which run is next and on what
    # (the page alone: no game, no lock), and a run's summary is numbered,
    # stamped and named, the next run named in turn.
    other.goto(f"http://127.0.0.1:{PORT}/index.html?mousecheck=mouse,trackpad", wait_until="load")
    box = lambda: other.evaluate("document.getElementById('mouseCheck').textContent")
    before = box()
    summary = other.evaluate("quake.mousecheck(1)")
    after = box()
    other.close()
    check("?mousecheck=mouse,trackpad: run #1 asks for the mouse; its summary is #1, stamped, named; #2 asks for the trackpad",
          before.startswith("run #1 (mouse): in the game, click the view") and re.match(r"#1 \d\d:\d\d:\d\d \(mouse\) mousecheck 1\.\d s: ", summary)
          and after.startswith(summary + "\nrun #2 (trackpad): in the game, click the view"), f"{before!r} / {summary[:60]!r} / {after[:300]!r}")

with sync_playwright() as p:
    br = isolated.launch(p, [
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
    # (Chromium's context runs as it is made; Firefox's resumes a moment later.)
    pg.wait_for_function("audioCtx && audioCtx.state === 'running'", timeout=3000)
    s = pg.evaluate("""async () => ({
        hidden: overlay.classList.contains('hidden'),
        audio: audioCtx ? audioCtx.state : 'none',
        menu: await exp.menu_visible(), walk: await exp.in_walk_mode(),
    })""")
    check("one click: scrim gone", s["hidden"])
    check("one click: audio unlocked", s["audio"] == "running", s["audio"])
    check("one click: attract revealed (the demo, no menu)",
          s["menu"] == 0 and s["walk"] == 0)
    check("the prompt says a key brings the menu",
          "any key for the menu" in pg.evaluate("document.getElementById('play').textContent"))
    # Mode transitions never resurrect the scrim.
    open_drawer(pg)
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

    # The scroll checks below are only load-bearing if the page CAN scroll:
    # with the "keys" drawer shut the page is just the view, so show the
    # drawer (without a click, which would be the first gesture).
    pg.evaluate("document.getElementById('drawer').hidden = false")
    check("page is scrollable (scroll assertions meaningful)",
          pg.evaluate("document.documentElement.scrollHeight > innerHeight"))

    # (1) Enter is a first gesture too.
    pg.keyboard.press("Enter")
    pg.wait_for_function("audioCtx && audioCtx.state === 'running'", timeout=3000)
    s = pg.evaluate("""async () => ({
        hidden: overlay.classList.contains('hidden'),
        audio: audioCtx ? audioCtx.state : 'none',
        menu: await exp.menu_visible(),
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

    # The "keys" button opens the drawer and, like every control, gives its
    # focus back: Space must not toggle it shut again.
    pg.evaluate("document.getElementById('drawer').hidden = true")
    pg.locator("#keysBtn").click()
    time.sleep(0.2)
    check("keys button opens the drawer",
          pg.evaluate("!document.getElementById('drawer').hidden"))
    for _ in range(5):
        pg.keyboard.press("Space")
    check("drawer stays open through 5 Space presses (keys button blurred)",
          pg.evaluate("!document.getElementById('drawer').hidden"))
    # Walk mode (boot lands in the menu); Esc — unlocked — closes it.
    pg.locator("#walkBtn").click()
    time.sleep(0.5)
    pg.keyboard.press("Escape")
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)
    time.sleep(0.3)
    check("walk mode entered (in_walk_mode)", pg.evaluate("exp.in_walk_mode()") == 1)
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
        return Array.from(quake.readback());
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
    # Ctrl -> +attack: spy on the page's keyEvent (the one door every key
    # takes to the program) so Ctrl's press and release are observable.
    pg.evaluate("""() => {
        window._real = keyEvent; window._atk = [];
        // Merged input path: Ctrl is K_CTRL(133) through Key_Event and the
        // BINDINGS table (default.cfg: ctrl = +attack, rebindable) — spy
        // its downs and ups.
        keyEvent = (k, d, c) => { if (k === 133) window._atk.push(d ? 1 : 0); return window._real(k, d, c); };
    }""")
    pg.keyboard.down("Control"); time.sleep(0.15); pg.keyboard.up("Control")
    atk = pg.evaluate("window._atk")
    pg.evaluate("keyEvent = window._real")
    check("Ctrl fires (+attack press/release)", atk == [1, 0], str(atk))
    check("pointer never locked during keyboard play",
          pg.evaluate("document.pointerLockElement === null"))

    # (5b) The wheel (2026): a notch switches weapons, with no pointer lock.
    # "impulse 3" (the super shotgun) sits between two owned weapons so next
    # and previous land somewhere different (`W_ChangeWeapon`/
    # `CycleWeaponCommand`/`CycleWeaponReverseCommand`): axe < shotgun < SUPER
    # SHOTGUN < nailgun < ..., "impulse 9" gives every weapon and its ammo so
    # none of them is skipped for want of ammo. Waited for throughout, not
    # slept past: the two impulses share one scalar field (`self.impulse`),
    # consumed by the next `W_WeaponFrame` tick rather than by the console
    # call that sets it, and the system's load can be noisy (a sleep
    # that outruns a slow tick races the next `exec` into the same field,
    # unconsumed). Polled from Python, not `wait_for_function`: a
    # Promise-returning predicate there is truthy before it resolves, in this
    # Playwright, so it would not actually wait.
    field = lambda name: pg.evaluate(f"quake.callLine('player_field {name}').then(r => r.value)")
    def wait_weapon(want, timeout=5.0):
        deadline = time.time() + timeout
        w = field("weapon")
        while w != want and time.time() < deadline:
            time.sleep(0.05)
            w = field("weapon")
        return w

    pg.evaluate("quake.callLine('exec impulse 9')")
    wait_weapon(32)                         # CheatCommand's own pick, the rocket launcher
    pg.evaluate("quake.callLine('exec impulse 3')")
    w0 = wait_weapon(2)                     # impulse 3's own, the super shotgun
    pg.mouse.move(410, 280)                 # over the view, not a bar button
    pg.mouse.wheel(0, -120)                 # one notch up (negative deltaY): next
    w1 = wait_weapon(4)
    pg.mouse.wheel(0, 120)                  # one notch down: back to the previous
    w2 = wait_weapon(2)
    check("2026: a wheel notch up switches to the next weapon, down back to it",
          (w0, w1, w2) == (2, 4, 2), f"{w0:.0f} -> {w1:.0f} -> {w2:.0f}")
    # A trackpad's burst of small deltas (Chrome's own notch is ~100 px)
    # accumulates and fires exactly once, not once per event. Dispatched with
    # a trackpad's wheelDeltaY (3 x deltaY: Chrome's on a Mac, Safari's), not
    # through Playwright: Chromium's DevTools gives every synthesized wheel
    # event a whole notch (`wheel_ticks_y = delta_y > 0 ? 1 : -1`,
    # input_handler.cc), which the page rightly takes as a mouse's notch.
    pg.evaluate("""async () => { const c = document.getElementById('c');
        for (let i = 0; i < 12; i++) {      // 12 x 15 = 180 px: one notch, up
          const e = new WheelEvent('wheel', { deltaY: -15, cancelable: true, bubbles: true });
          Object.defineProperty(e, 'wheelDeltaY', { value: 45 });
          c.dispatchEvent(e);
          await new Promise(r => setTimeout(r, 20));
        } }""")
    w3 = wait_weapon(4)
    check("a trackpad-style burst of small deltas fires once", w3 == 4, f"{w3:.0f}")
    # Wheels as a Mac's browsers send them. There a notch is NSEvent's
    # accelerated deltaY x 40 px, a few pixels or 72; Chrome's wheelDeltaY
    # is the raw notch count x 120 for a mouse but 3 x deltaY for a trackpad,
    # as Safari's always is (each step's third value; Safari's by default).
    # A wheelDeltaY that counts notches is that many at any spacing; else a
    # lone event is a notch and a stream accumulates 100 px a notch. The
    # page's own count of notches fired (`wheelFired`), events dispatched as
    # the browser would, wheelDeltaY set on each (Firefox's init dictionary
    # has none), the gaps real: timers, or none within a stream.
    def wheel_notches(steps):
        return pg.evaluate("""async steps => {
            const c = document.getElementById('c'), n0 = { ...wheelFired };
            for (const [deltaY, gap, legacy, deltaMode = 0] of steps) {
              if (gap) await new Promise(r => setTimeout(r, gap));
              const e = new WheelEvent('wheel', { deltaY, deltaMode, cancelable: true, bubbles: true });
              Object.defineProperty(e, 'wheelDeltaY', { value: legacy ?? -Math.round(3 * deltaY) });
              c.dispatchEvent(e);
            }
            return [wheelFired.down - n0.down, wheelFired.up - n0.up];
        }""", steps)
    for name, steps, want in [
        ("three slow macOS notches (4, 8, 12 px, 150 ms apart) are three notches",
         [(4, 200, None), (8, 150, None), (12, 150, None)], [3, 0]),
        ("a stream of 40 x 6 px: the first at once, then a notch a 100 px",
         [(6, 200, None)] + [(6, 0, None)] * 39, [2, 0]),
        ("a stream that turns back drops what it carried (10 x 6 down, 20 x 6 up)",
         [(6, 200, None)] + [(6, 0, None)] * 9 + [(-6, 0, None)] * 20, [1, 1]),
        ("a flick of 1000 px in a stream is capped at three notches",
         [(-2, 200, None), (-1000, 0, None)], [0, 4]),
        ("Chrome on a Mac: 72 px notches (wheelDeltaY -120) 29 and 51 ms apart are three, back up one",
         [(72, 200, -120), (72, 29, -120), (72, 51, -120), (-72, 300, 120)], [3, 1]),
        ("Chrome on a Mac: two notches in one event (wheelDeltaY -240) are two",
         [(144, 200, -240)], [2, 0]),
        ("Chrome on a Mac: a trackpad's 40 px steps (wheelDeltaY -120 = 3 x 40) 16 ms apart accumulate",
         [(40, 200, -120)] + [(40, 16, -120)] * 9, [4, 0]),
        # Firefox's notches are lines (deltaMode 1) with the notch count in
        # wheelDeltaY: 6 lines on Linux (measured, Firefox 155, X11: a
        # headed run), 3 on Windows by its source. Three quick ones are three
        # whatever the lines: the 6-line ones were five, a notch's 200 px
        # counted twice in a stream.
        ("Firefox on Linux: 6-line notches (deltaMode 1, wheelDeltaY -120) 30 ms apart are three",
         [(6, 200, -120, 1), (6, 30, -120, 1), (6, 30, -120, 1)], [3, 0]),
        ("Firefox on Windows: 3-line notches 30 ms apart are three, back up one",
         [(3, 200, -120, 1), (3, 30, -120, 1), (3, 30, -120, 1), (-3, 300, 120, 1)], [3, 1]),
        ("Firefox on a Mac: lines with no ticks (wheelDeltaY 0) fall to the lone and stream rules, 3 lines a notch",
         [(3, 200, 0, 1), (3, 30, 0, 1), (3, 30, 0, 1)], [3, 0]),
    ]:
        got = wheel_notches(steps)
        check(f"wheel: {name}", got == want, f"down {got[0]}, up {got[1]}; want {want}")
    # Classic leaves the wheel unbound, as id's default.cfg.
    pg.evaluate("quake.callLine('exec profile classic')")
    time.sleep(0.3)
    pg.evaluate("quake.callLine('exec impulse 9')")
    wait_weapon(32)
    pg.evaluate("quake.callLine('exec impulse 3')")
    wc = wait_weapon(2)
    pg.mouse.wheel(0, -120)
    pg.mouse.wheel(0, -120)
    time.sleep(0.5)
    check("Classic: the wheel is unbound, nothing changes", field("weapon") == wc, f"{wc:.0f} -> {field('weapon'):.0f}")
    pg.evaluate("quake.callLine('exec profile 2026')")
    time.sleep(0.2)

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
        check("Esc-driven lock loss opens the in-game menu",
              isolated.wait_until(pg, "exp.menu_visible().then(v => v === 1)", 5, raising=False))
        check("chip hides while the menu is up",
              pg.evaluate("!lockChip.classList.contains('show')"))
        pg.locator("#wrap").screenshot(path=os.path.join(WEB, "verify_input_menu.png"))
        # Plain Esc keydowns now navigate BACK in the menu (deliverable: no lock).
        pg.keyboard.press("Enter")    # Main > Single Player (submenu)
        check("menu: Enter descends to a submenu", pg.evaluate("exp.menu_visible()") == 1)
        pg.keyboard.press("Escape")   # submenu -> main screen (still visible)
        check("menu: Esc backs out of the submenu (menu still up)",
              pg.evaluate("exp.menu_visible()") == 1)
        pg.keyboard.press("Escape")   # main screen -> closed
        check("menu: Esc on the main screen closes it",
              isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5, raising=False))
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
        isolated.wait_until(pg, "exp.menu_visible().then(v => v === 1)", 5)
        pg.keyboard.press("Escape")   # leave the menu closed for the next phase
        time.sleep(0.3)

    # (3c) Console state: keys are owned by the console, and arrows can't scroll.
    pg.keyboard.press("`")
    check("console opens", pg.evaluate("exp.console_visible()") == 1)
    sy = pg.evaluate("scrollY")
    for _ in range(10):
        pg.keyboard.press("ArrowDown")
        pg.keyboard.press("Space")
    check("console state: arrows/Space scroll nothing", pg.evaluate("scrollY") == sy)
    pg.keyboard.press("Escape")
    check("Esc closes the console", pg.evaluate("exp.console_visible()") == 0)

    # (4b) Demo mode: chip hidden, scrim hidden, canvas clicks don't lock.
    pg.locator("#demoBtn").click()
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('playing demo')",
        timeout=30000)
    time.sleep(0.5)
    pg.locator("#c").click(position={"x": 320, "y": 240})
    time.sleep(0.5)
    s = pg.evaluate("""async () => ({
        chip: lockChip.classList.contains('show'),
        scrim: !overlay.classList.contains('hidden'),
        locked: document.pointerLockElement !== null,
        walk: await exp.in_walk_mode(),
    })""")
    check("demo mode: no chip, no scrim, canvas click does not lock",
          not s["chip"] and not s["scrim"] and not s["locked"] and s["walk"] == 0,
          str(s))

    # Fullscreen is approximate under headless: try Alt+Enter (in demo
    # playback: the key works whatever has the keyboard) and report, don't
    # insist.
    pg.keyboard.press("Alt+Enter")
    time.sleep(0.5)
    if pg.evaluate("document.fullscreenElement !== null"):
        check("Alt+Enter enters fullscreen; transient hint shows",
              pg.evaluate("fsHint.classList.contains('show')"))
        pg.keyboard.press("Alt+Enter")
        time.sleep(0.5)
        check("Alt+Enter leaves fullscreen; hint hides",
              pg.evaluate("document.fullscreenElement === null"
                          " && !fsHint.classList.contains('show')"))
    else:
        print("SKIP fullscreen checks (headless refused requestFullscreen) — "
              "covered by the manual test script")

    # (6) The canvas box (fitCanvas) for a video mode — the Classic profile
    # (the 2026 profile fills the window: verify_settings.py): the largest
    # 4:3 box the window fits, snapped to a whole number of pixels per
    # framebuffer column when it is at most 1/6 past one (no doubled column
    # in the pixelated upscale), smoothed when it is narrower than the
    # framebuffer (no dropped column).
    pg.evaluate("quake.callLine('exec profile classic')")
    pg.wait_for_function("!(quake.state.flags & 32)", timeout=5000)
    box = lambda: pg.evaluate("""(async () => { const c = document.getElementById('c');
        return [parseFloat(c.style.width), parseFloat(c.style.height),
                getComputedStyle(c).imageRendering, await exp.width()]; })()""")
    for (vw, vh), want in [((1440, 900), (960, 720, "pixelated")),
                           ((1920, 1080), (1328, 996, "pixelated")),
                           ((1024, 768), (912, 684, "auto"))]:
        pg.set_viewport_size({"width": vw, "height": vh})
        time.sleep(0.3)
        b = box()
        check(f"{vw}x{vh}: the {b[3]}-wide framebuffer in a {want[0]}x{want[1]} box, {want[2]}",
              b[3] == 960 and (round(b[0]), round(b[1]), b[2]) == want, str(b))

    # (7) Keys by their place (KeyboardEvent.code), as id's keys were
    # scancodes: on AZERTY the key in W's place types 'z' and is keynum 'w'
    # (+forward in the 2026 WASD), while the console types what the layout
    # typed. Synthetic events, since a headless browser has one layout.
    pg.evaluate("quake.callLine('exec profile 2026')")
    pg.evaluate("exp.boot().then(() => exp.menu_cancel())")
    time.sleep(0.5)
    azerty = lambda typ: pg.evaluate(
        "t => dispatchEvent(new KeyboardEvent(t, { code: 'KeyW', key: 'z', bubbles: true, cancelable: true }))", typ)
    azerty("keydown")
    time.sleep(0.1)
    held = [pg.evaluate("exp.key_is_down(119)"), pg.evaluate("exp.key_is_down(122)")]
    azerty("keyup")
    time.sleep(0.1)
    check("AZERTY: the key in W's place is keynum w (+forward), not z",
          held == [1, 0] and pg.evaluate("exp.key_is_down(119)") == 0, str(held))
    pg.keyboard.press("Backquote")
    isolated.wait_until(pg, "exp.console_visible().then(v => v === 1)", 5)
    azerty("keydown")
    azerty("keyup")
    pg.keyboard.press("Enter")
    time.sleep(0.2)
    check("AZERTY: in the console the same key types z",
          'Unknown command "z"' in pg.evaluate("quake.text('console_text')"))
    pg.keyboard.press("Backquote")

    # (8) Every key up when a release may never come (PLATFORM.md, "Input";
    # vid_win.c's ClearAllStates). In a real Chromium the Esc that ends the
    # pointer lock eats the keyups after it (W held through it stayed
    # +forward); headless delivers them, so each step reads the engine's
    # keys while they are still down here.
    held = lambda: pg.evaluate("quake.text('keys_held')")
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)
    # (8a) The pointer lock ends (the browser's Esc: exitPointerLock).
    pg.locator("#c").click(position={"x": 320, "y": 240})
    pg.wait_for_function("document.pointerLockElement === document.getElementById('c')", timeout=5000)
    pg.keyboard.down("w")
    pg.mouse.down()
    check("lock: W and the fire button held", held() == "w MOUSE1", repr(held()))
    pg.evaluate("document.exitPointerLock()")
    isolated.wait_until(pg, "exp.menu_visible().then(v => v === 1)", 5)
    check("lock lost: the engine holds no key, the drag is over",
          held() == "" and pg.evaluate("dragging") is False, repr(held()))
    pg.keyboard.up("w")
    pg.mouse.up()
    pg.keyboard.press("Escape")
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)
    # (8b) The window blurs (Alt-Tab, a click elsewhere).
    pg.keyboard.down("w")
    pg.keyboard.down("Shift")
    check("blur: W and Shift held", held() == "w SHIFT", repr(held()))
    pg.evaluate("dispatchEvent(new FocusEvent('blur'))")
    check("blur: the engine holds no key", held() == "", repr(held()))
    pg.keyboard.up("Shift")
    pg.keyboard.up("w")
    # (8c) The tab hides.
    def visibility(hidden):
        pg.evaluate("""h => { Object.defineProperty(document, 'hidden', { value: h, configurable: true });
                              Object.defineProperty(document, 'visibilityState', { value: h ? 'hidden' : 'visible', configurable: true });
                              document.dispatchEvent(new Event('visibilitychange')); }""", hidden)
    pg.keyboard.down("w")
    visibility(True)
    check("hidden: the engine holds no key", held() == "", repr(held()))
    visibility(False)
    pg.evaluate("delete document.hidden; delete document.visibilityState")
    pg.keyboard.up("w")
    # (8d) Fullscreen ends (the browser's Esc, Alt+Enter, the window manager).
    pg.keyboard.press("Alt+Enter")
    try:
        pg.wait_for_function("!!document.fullscreenElement", timeout=3000)
        fs = True
    except Exception:
        fs = False
    if fs:
        pg.evaluate("""window.__fsOff = 0; document.addEventListener('fullscreenchange',
                       () => { if (!document.fullscreenElement) window.__fsOff++; })""")
        pg.keyboard.down("w")
        check("fullscreen: W held", held() == "w", repr(held()))
        pg.evaluate("document.exitFullscreen()")
        pg.wait_for_function("window.__fsOff > 0", timeout=5000)
        check("fullscreen left: the engine holds no key, Esc unlocked",
              held() == "" and pg.evaluate("escLocked") is False, repr(held()))
        pg.keyboard.up("w")
    else:
        print("SKIP the fullscreen step (headless refused requestFullscreen)")
    # (8e) A key the player still holds presses again with its next
    # autorepeat; and a fresh press works.
    pg.keyboard.down("w")
    pg.evaluate("dispatchEvent(new FocusEvent('blur'))")
    pg.evaluate("dispatchEvent(new KeyboardEvent('keydown', { code: 'KeyW', key: 'w', repeat: true, bubbles: true, cancelable: true }))")
    check("a held key's autorepeat after the clear presses it again", held() == "w", repr(held()))
    pg.keyboard.up("w")
    check("...and its release lets go", held() == "", repr(held()))
    pg.keyboard.down("ArrowUp")
    check("a fresh press holds", held() == "UPARROW", repr(held()))
    pg.keyboard.up("ArrowUp")
    check("...and lets go", held() == "", repr(held()))
    # (9) The mouse turns as far at any frame rate: here at the headless
    # 60 Hz refresh, then in a browser without the frame-rate limit.
    hz, turned, want = mouse_turn(pg)
    check(f"mouse: 1000 counts turn {want:.0f}° at a {hz:.0f} Hz refresh",
          abs(turned - want) < 0.05, f"{turned:.3f}°")
    mouse_paths(pg)

    check("no console errors", not errs, str(errs[-5:]))
    br.close()

    # (8, continued) Several hundred refreshes a second: Chromium's
    # frame-rate limit off, Firefox's refresh driver at 480 Hz.
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required",
                             "--disable-frame-rate-limit", "--disable-gpu-vsync"],
                         prefs={"layout.frame_rate": 480})
    pg = br.new_page(viewport={"width": 820, "height": 560})
    boot_page(pg)
    pg.locator("#overlay").click()
    hz, turned, want = mouse_turn(pg)
    if hz < 200:
        print(f"SKIP mouse at a high refresh rate: this browser refreshed at {hz:.0f} Hz")
    else:
        check(f"mouse: 1000 counts turn {want:.0f}° at a {hz:.0f} Hz refresh",
              abs(turned - want) < 0.05, f"{turned:.3f}°")
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
