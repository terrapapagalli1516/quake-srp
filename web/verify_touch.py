#!/usr/bin/env -S uv run --with playwright --script
"""Verify the page on a phone: the touch controls (web/touch.js), the tappable
menus, and offline play through the service worker (web/sw.js), in headless
Chromium emulating a phone held sideways (844x390 CSS pixels at
devicePixelRatio 3, `isMobile`, `hasTouch`). Taps are Playwright's
touchscreen; drags are CDP touch events (Input.dispatchTouchEvent), so the
whole check is Chromium's.

QUAKE_BROWSER=firefox runs the part Playwright's Firefox can drive: taps
(its touchscreen has nothing else: no drag, no second finger, no hold),
`hasTouch` without `isMobile` (Firefox has none; its coarse pointer needs
only `hasTouch`). Each check that needs a drag or a held finger is SKIPped
with the reason: 3's stick, look, two thumbs, FIRE and JUMP, and 2b's held
arrow (a quick tap of it runs). The rest, 95 of Chromium's 102 checks, runs
on Gecko's touch events and pointer events.

  1. The page: touch.js loads on the coarse pointer, the touch layout fills
     the screen, "tap to start"; the rotate prompt shows upright and a tap
     dismisses it; the manifest and its icons.
  2. The menu by tapping (the engine's own item layout: menu_tap): a tap on
     the demo brings the menu, Single Player > New Game starts the game;
     Options' Always Run takes a tap to point and one to flip; Quit asks,
     with YES / NO buttons; BACK backs out. The menu pad (▲▼◀▶, OK): shown
     only in menu mode, clear of the menu's own layout at three phone
     sizes; the arrows move the cursor and step a slider (held, several
     notches), OK enters a submenu, Help's pages turn; hidden while the
     menu asks y/n and while Customize controls grabs a key.
  3. Play: the left stick walks (the player moves), a drag on the right
     turns the view, FIRE shoots (a shell spent), JUMP jumps, WEAPON cycles
     (shotgun to axe), MENU opens the menu.
  4. The console from Options > Go to console, typed on the phone's keyboard
     (a hidden field: `echo` prints), BACK closes it.
  5. Hidden page: the game pauses under its menu; back in the game it runs.
  6. Classic (in_touch off): no game controls, the menu still a tap away.
  7. Offline: every file kept by the service worker; with the server gone,
     a reload plays from the cache.
  8. Any static host: served with no isolation headers, the page reloads
     under its service worker and runs isolated.
  9./10. Clear of the status bar, at several phone sizes and the phone profile's
     (PHONE_26, devicePixelRatio 2.6).
  11. The game picker (PHONE_26), on a deploy offering a synthesized mission
     pack: the start overlay's buttons, the running game marked; a finger on
     Scourge of Armagon (pointerdown, pointerup, click) reloads the page as
     ?game=hipnotic and is never also the tap that starts the game; GAME in
     the menu opens the same choices without tapping the menu under them;
     the quit screen offers them too. A shareware-only deploy shows none.

Not verifiable here: Safari and iOS (Playwright's WebKit does not start on
the test host), a real phone's touch screen, fullscreen and the landscape
lock (headless has no screen), vibration, the wake lock's effect, and a
hard reload on a server without the headers (CDP's cache-ignoring reload
also bypasses the service worker for the reload the page then asks for,
which a person's shift-reload is documented not to).

Usage: verify_touch.py [webdir]   (a deploy dir, PLATFORM.md: index.html,
wasi.js, touch.js, sw.js, manifest.webmanifest, icons/, quake.wasm,
id1/pak0.pak.) Screenshots verify_touch_*.png go into it.
"""
import functools, http.server, json, math, os, shutil, socketserver, sys, tempfile, threading, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8176)
PHONE = dict(viewport={"width": 844, "height": 390}, device_scale_factor=3, is_mobile=True, has_touch=True)
MAIN, SINGLE, OPTIONS = 0, 1, 5


class Handler(isolated.Handler):
    """isolated.py's server as a deploy would be: files the browser may keep
    (no `no-store`), with the isolation headers or (isolation=False) none."""
    isolation = True

    def end_headers(self):
        if self.isolation:
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        http.server.SimpleHTTPRequestHandler.end_headers(self)


def serve(port, isolation, directory=WEB):
    socketserver.ThreadingTCPServer.allow_reuse_address = True
    handler = type("H", (Handler,), {"isolation": isolation})
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", port), functools.partial(handler, directory=directory))
    httpd.daemon_threads = True
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


def pack_deploy():
    """A deploy offering Scourge of Armagon (web/PLATFORM.md, "The game
    picker"), from synthesized data (isolated.py): WEB's page, engine and
    pak0 (symlinked), a registered pak1 — a mission pack runs only on the
    registered game (COM_CheckRegistered) — with id's end screen as its
    end2.bin, and a hipnotic/pak0.pak whose hip1m1 is e1m1; files.json
    lists both."""
    d = tempfile.mkdtemp(prefix="quake-touch-packs-")
    isolated.copy_page(d)
    for sub in ("id1", "hipnotic"):
        os.makedirs(os.path.join(d, sub), exist_ok=True)
    for rel in ("quake.wasm", os.path.join("id1", "pak0.pak")):
        os.symlink(os.path.abspath(os.path.join(WEB, rel)), os.path.join(d, rel))
    pak0 = os.path.join(WEB, "id1", "pak0.pak")
    e1m1, end1 = isolated.pak_file(pak0, "maps/e1m1.bsp"), isolated.pak_file(pak0, "end1.bin")
    with open(os.path.join(d, "id1", "pak1.pak"), "wb") as f:
        f.write(isolated.write_pak([("gfx/pop.lmp", isolated.POP_LMP), ("maps/e2m1.bsp", e1m1), ("end2.bin", end1)]))
    with open(os.path.join(d, "hipnotic", "pak0.pak"), "wb") as f:
        f.write(isolated.write_pak([("maps/hip1m1.bsp", e1m1)]))
    isolated.write_manifest(d)
    return d


# What a page did with a finger on a game choice, kept for the page after
# it (sessionStorage, at pagehide): the pointer events and click the
# choice's link saw (capture phase, so the link's own stopPropagation does
# not hide them), and whether the page also took the tap as its start —
# the overlay gone, fullscreen asked for (index.html's gameList).
PICK_PROBE_JS = """(() => {
  const ask = HTMLElement.prototype.requestFullscreen;
  HTMLElement.prototype.requestFullscreen = function (...a) { window.__fullscreenAsked = true; return ask.apply(this, a); };
  const seq = [];
  for (const t of ['pointerdown', 'pointerup', 'click'])
    addEventListener(t, e => { const a = e.target.closest && e.target.closest('.games a'); if (a) seq.push(t + ' ' + a.dataset.game); }, true);
  addEventListener('pagehide', () => {
    const o = document.getElementById('overlay'), t = document.getElementById('touch');
    sessionStorage.setItem('pickProbe', JSON.stringify({ seq, overlayHidden: !!o && o.classList.contains('hidden'),
      fullscreenAsked: !!window.__fullscreenAsked, mode: t ? t.dataset.mode : null }));
  });
})();"""


# Playwright's Firefox has no CDP session and its touchscreen only taps
# (module docstring): the drags and holds below go through `touches`/`drag`
# in Chromium and are SKIPped, or run as a tap, in Firefox.
FIREFOX = os.environ.get("QUAKE_BROWSER", "chromium") == "firefox"

passed, failed, skipped = 0, 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

def skip(name, why):
    global skipped
    print("SKIP", name, f"({why})")
    skipped += 1

# The client point of a point of the menu's 320x200 layout: quake-rs
# menu.rs menu_layout_point run backwards (the 2-D scale, the centred menu),
# then the frame pixel through the canvas's CSS box.
MENU_POINT = """async ([vx, vy]) => {
  const [W, H] = quake.size();
  const scaled = await exp.scaled_2d();
  let s = Math.floor(Math.min(W / 320, H / 200)), sw = W;
  if (scaled && s > 1) sw = Math.max(Math.round(W / s), 320); else s = 1;
  const ox = ((sw - 320) >> 1) * s;
  const c = document.getElementById('c'), r = c.getBoundingClientRect();
  return [r.left + c.clientLeft + (ox + vx * s) * c.clientWidth / W,
          r.top + c.clientTop + vy * s * c.clientHeight / H];
}"""

# The real Fullscreen API, disabled: headless Chromium grants touch.js's
# first-tap requestFullscreen (goFullscreen) by actually resizing the
# browser's own window to the screen, and CDP then refuses to resize a
# fullscreen window back (9./10. resize the viewport mid-session to sweep
# phone sizes) — not a thing a page script would ever need to care about,
# so it is turned off for every context instead of worked around per call.
NO_FULLSCREEN_JS = "HTMLElement.prototype.requestFullscreen = () => Promise.reject(new Error('disabled for verify_touch.py'));"


def main():
    httpd = serve(PORT, True)
    with sync_playwright() as p:
        br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
        # Firefox has no `isMobile`; `hasTouch` alone gives it the coarse pointer.
        phone = lambda **o: br.new_context(**{k: v for k, v in o.items() if not (FIREFOX and k == "is_mobile")})
        ctx = phone(**PHONE)
        ctx.add_init_script(NO_FULLSCREEN_JS)
        pg = ctx.new_page()
        errs = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        cdp = None if FIREFOX else ctx.new_cdp_session(pg)

        def touches(kind, points):
            cdp.send("Input.dispatchTouchEvent", {"type": kind, "touchPoints": [
                {"x": x, "y": y, "id": i} for i, (x, y) in points]})

        def drag(points, to, steps=12, hold=0.0, release=True):
            """Fingers `points` [(id, (x, y))] move to `to` [(x, y)] in `steps`."""
            touches("touchStart", points)
            for k in range(1, steps + 1):
                touches("touchMove", [(i, (x + (tx - x) * k / steps, y + (ty - y) * k / steps))
                                      for (i, (x, y)), (tx, ty) in zip(points, to)])
                time.sleep(0.016)
            time.sleep(hold)
            if release:
                touches("touchEnd", [])

        # `pg_` defaults to the main phone's page; the status-bar safe-zone
        # sweep (9./10. below) passes a second phone's page through it.
        mode = lambda pg_=None: (pg_ or pg).evaluate("document.getElementById('touch') && document.getElementById('touch').dataset.mode")
        flags = lambda: pg.evaluate("quake.state.flags")
        screen_id = lambda: pg.evaluate("exp.menu_screen_id()")
        cursor = lambda: pg.evaluate("exp.menu_cursor()")
        make_call = lambda pg_: (lambda line: pg_.evaluate(f"quake.call({json.dumps(line.split()[0])}, ...{json.dumps(line.split()[1:])})"))
        call = make_call(pg)
        field = lambda name: pg.evaluate(f"quake.callLine('player_field {name}').then(r => r.value)")
        def tap_menu(vx, vy):
            x, y = pg.evaluate(MENU_POINT, [vx, vy])
            pg.touchscreen.tap(x, y)
            time.sleep(0.25)
        def tap_el(sel):
            box = pg.locator(sel).bounding_box()
            pg.touchscreen.tap(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            time.sleep(0.25)
        def shown(sel, pg_=None):
            pg_ = pg_ or pg
            return pg_.evaluate(f"(() => {{ const e = document.querySelector({json.dumps(sel)}); return !!e && getComputedStyle(e).display !== 'none' && !e.hidden; }})()")
        def wait(js, timeout=5000, pg_=None):
            try:
                (pg_ or pg).wait_for_function(js, timeout=timeout)
                return True
            except Exception:
                return False
        def listener():
            return pg.evaluate("Promise.all(['listener_x','listener_y','listener_z','listener_fwd_x','listener_fwd_y'].map(n => quake.call(n)))")
        # The menu pad's buttons (PLATFORM.md "The menu pad"): a quick tap,
        # or held for `seconds` (touchStart/wait/touchEnd, as a real hold),
        # to drive the repeat (touch.js padHoldStart/Stop).
        def pad_tap(sel, seconds=0.0):
            box = pg.locator(sel).bounding_box()
            pt = (box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            if FIREFOX:                                    # taps only: no hold
                pg.touchscreen.tap(*pt)
            else:
                touches("touchStart", [(77, pt)])
                if seconds:
                    time.sleep(seconds)
                touches("touchEnd", [])
            time.sleep(0.15)
        def goto_row(target):
            """Step the pad's UP/DOWN until the menu's cursor is on `target`."""
            for _ in range(50):
                if cursor() == target:
                    return
                pad_tap("#tPadUp" if cursor() > target else "#tPadDown")
            raise AssertionError(f"goto_row({target}): stuck on {cursor()}")

        # --- 1. The page -------------------------------------------------------
        pg.goto(f"http://127.0.0.1:{PORT}/index.html?2026", wait_until="load")
        pg.wait_for_function("window.quake && quake.ready && window.QuakeTouch", timeout=120000)
        check("touch.js on a coarse pointer, the touch layout",
              pg.evaluate("document.documentElement.classList.contains('touch') && !!document.getElementById('touch')"))
        check("no keyboard-and-mouse note", pg.evaluate("!document.getElementById('touchNote')"))
        check("the start prompt says tap", pg.evaluate("document.getElementById('play').textContent") == "tap to start")
        check("a shareware-only deploy: no game picker", not shown("#gamePicker"))
        # The first frame sizes the canvas; `ready` can come a moment before it.
        try:
            pg.wait_for_function("(() => { const r = document.getElementById('c').getBoundingClientRect();"
                                 " return Math.abs(r.width - 844) <= 1 && Math.abs(r.height - 390) <= 1; })()", timeout=5000)
        except Exception:
            pass
        box = pg.evaluate("(() => { const r = document.getElementById('c').getBoundingClientRect(); return [r.width, r.height]; })()")
        check("the picture fills the screen", abs(box[0] - 844) <= 1 and abs(box[1] - 390) <= 1, str(box))
        manifest = pg.evaluate("fetch('manifest.webmanifest').then(r => r.json())")
        icons = pg.evaluate("Promise.all(%s.map(i => fetch(i.src).then(r => r.status + ' ' + r.headers.get('Content-Type'))))"
                            % json.dumps(manifest["icons"]))
        check("the manifest: fullscreen, landscape, icons",
              manifest["display"] == "fullscreen" and manifest["orientation"] == "landscape"
              and all(s == "200 image/png" for s in icons), str(icons))
        pg.set_viewport_size({"width": 390, "height": 844})
        time.sleep(0.3)
        check("upright: the rotate prompt", shown("#tRotate"))
        tap_el("#tRotate")
        check("a tap dismisses it", not shown("#tRotate"))
        pg.set_viewport_size({"width": 844, "height": 390})
        time.sleep(0.5)
        pg.screenshot(path=os.path.join(WEB, "verify_touch_start.png"))

        # --- 2. The menu by tapping --------------------------------------------
        pg.touchscreen.tap(422, 195)                      # tap to start
        check("started: the attract demo, the MENU button", wait("document.getElementById('touch').dataset.mode === 'demo'")
              and shown("#tMenu"), mode())
        check("menu pad hidden in demo mode", not shown("#tMenuPad"))
        pg.touchscreen.tap(500, 200)                      # any tap: the menu
        check("a tap on the demo opens the menu", wait("quake.state.flags & 1") and mode() == "menu" and shown("#tBack"))
        check("menu pad shown in menu mode", shown("#tMenuPad"))
        check("no GAME in the menu: no mission pack on this deploy", not shown("#tGame"))
        pg.screenshot(path=os.path.join(WEB, "verify_touch_menu.png"))
        tap_menu(160, 42)                                 # Main > Single Player
        check("Single Player by a tap", wait("quake.state.menuScreen === 1"), str(screen_id()))
        tap_menu(160, 42)                                 # New Game
        check("New Game by a tap: the game starts, the controls show",
              wait("document.getElementById('touch').dataset.mode === 'play'", 20000)
              and all(shown(s) for s in ["#tFire", "#tJump", "#tWeapon", "#tMenu"]), mode())
        check("menu pad hidden in play mode", not shown("#tMenuPad"))
        time.sleep(1.0)

        # --- 3. Play -------------------------------------------------------------
        pg.screenshot(path=os.path.join(WEB, "verify_touch_play.png"))
        if FIREFOX:
            for name in ("the stick walks", "let go, the player stops", "a drag on the right turns the view right",
                         "two thumbs: walk and turn together", "FIRE shoots", "JUMP jumps"):
                skip(name, "a drag, a second finger or a held one: Playwright's Firefox touchscreen only taps")
        else:
            before = listener()
            drag([(1, (150, 280))], [(150, 220)], hold=1.5)   # the stick, thumb up: forward
            time.sleep(0.2)
            after = listener()
            walked = ((after[0] - before[0]) ** 2 + (after[1] - before[1]) ** 2) ** 0.5
            check("the stick walks", walked > 100, f"{walked:.0f} units")
            time.sleep(0.3)
            still = listener()
            time.sleep(0.3)
            check("let go, the player stops", abs(listener()[0] - still[0]) + abs(listener()[1] - still[1]) < 2)
            yaw = lambda l: math.degrees(math.atan2(l[4], l[3]))
            y0 = yaw(listener())
            drag([(2, (600, 200))], [(700, 200)])             # look: a drag right
            time.sleep(0.2)
            turned = (y0 - yaw(listener()) + 540) % 360 - 180
            check("a drag on the right turns the view right", 15 < turned < 60, f"{turned:.1f} degrees")
            # Stick and look at once (two thumbs).
            before = listener()
            drag([(3, (150, 280)), (4, (600, 200))], [(150, 220), (650, 200)], hold=0.5)
            after = listener()
            check("two thumbs: walk and turn together",
                  ((after[0] - before[0]) ** 2 + (after[1] - before[1]) ** 2) ** 0.5 > 30
                  and abs(yaw(after) - yaw(before)) > 5)
            shells = field("ammo_shells")
            fire = pg.locator("#tFire").bounding_box()
            fx, fy = fire["x"] + fire["width"] / 2, fire["y"] + fire["height"] / 2
            touches("touchStart", [(5, (fx, fy))])
            time.sleep(0.3)
            touches("touchEnd", [])
            time.sleep(0.4)
            check("FIRE shoots", field("ammo_shells") < shells, f"shells {shells:.0f} -> {field('ammo_shells'):.0f}")
            z0 = listener()[2]
            jump = pg.locator("#tJump").bounding_box()
            touches("touchStart", [(6, (jump["x"] + 30, jump["y"] + 30))])
            top = z0
            for _ in range(12):
                time.sleep(0.05)
                top = max(top, listener()[2])
            touches("touchEnd", [])
            check("JUMP jumps", top - z0 > 20, f"{top - z0:.0f} units up")
        w0 = field("weapon")
        tap_el("#tWeapon")
        time.sleep(0.3)
        check("WEAPON cycles (shotgun to axe)", (w0, field("weapon")) == (1, 4096), f"{w0:.0f} -> {field('weapon'):.0f}")
        tap_el("#tMenu")
        check("MENU opens the menu", wait("quake.state.flags & 1") and mode() == "menu")

        # Options: a tap points at Always Run, a second flips it.
        tap_menu(160, 82)                                 # Main > Options
        check("Options by a tap", wait("quake.state.menuScreen === 5"))
        speed = lambda: pg.evaluate("quake.callLine('cvar cl_forwardspeed').then(r => r.value)")
        s0 = speed()
        tap_menu(100, 32 + 8.5 * 8)
        s1 = speed()
        tap_menu(100, 32 + 8.5 * 8)
        check("Always Run: the first tap points, the second flips", s0 == s1 and speed() != s0, f"{s0} {s1} {speed()}")
        tap_menu(100, 32 + 8.5 * 8)                       # back as it was
        pg.screenshot(path=os.path.join(WEB, "verify_touch_options.png"))

        # --- 2b. The menu pad ----------------------------------------------------
        # PLATFORM.md "The menu pad": ▲▼◀▶ and OK drive the menu by key, off
        # to the right of its own centred layout; holding an arrow repeats
        # it (touch.js padHoldStart/Stop). Still on Options (screen_id() 5)
        # from the Always Run test above.
        c0 = cursor()
        pad_tap("#tPadUp")
        check("the pad's UP moves the cursor", cursor() == c0 - 1, f"{c0} -> {cursor()}")
        goto_row(4)                                       # Brightness (gamma): a slider row
        gamma = lambda: pg.evaluate("quake.callLine('cvar gamma').then(r => r.value)")
        g0 = gamma()
        pad_tap("#tPadRight")
        check("the pad's RIGHT steps a slider's cvar", gamma() != g0, f"{g0} -> {gamma()}")
        g1 = gamma()
        if FIREFOX:
            skip("holding RIGHT moves several notches", "a held finger: Playwright's Firefox touchscreen only taps")
        else:
            pad_tap("#tPadRight", seconds=1.2)             # held: the repeat
            moved = abs(gamma() - g1)
            check("holding RIGHT moves several notches", moved >= 0.1, f"{g1} -> {gamma()} ({moved / 0.05:.1f} notches)")
        call("exec gamma 1")                               # back to default

        goto_row(0)                                        # Customize controls
        pad_tap("#tPadOk")
        check("OK enters a submenu", wait("quake.state.menuScreen === 6"), str(screen_id()))   # Keys
        pad_tap("#tPadOk")                                  # Enter on a bind row: grabs the key
        check("bind grab hides the pad (STATE 8, BIND_GRAB)",
              wait("quake.state.flags & 8") and not shown("#tMenuPad"), str(flags()))
        pg.keyboard.press("Escape")                         # cancel the grab
        check("cancelled: the pad is back", wait("!(quake.state.flags & 8)") and shown("#tMenuPad"))
        pg.keyboard.press("Escape")                         # back to Options
        check("back on Options", wait("quake.state.menuScreen === 5"), str(screen_id()))

        # Help pages (id's M_Help_Key) take the pad's ◀▶ too.
        pg.keyboard.press("Escape")                         # back to Main
        check("back on Main", wait("quake.state.menuScreen === 0"), str(screen_id()))
        goto_row(3)                                         # Help/Ordering
        pad_tap("#tPadOk")
        check("OK opens Help", wait("quake.state.menuScreen === 8"), str(screen_id()))
        h0 = call("frame_hash")
        pad_tap("#tPadRight")
        time.sleep(0.3)
        h1 = call("frame_hash")
        check("the pad's RIGHT pages Help forward", h1 != h0, f"{h0} -> {h1}")
        pg.keyboard.press("Escape")                         # back to Main
        tap_menu(160, 82)                                   # Main > Options, for what follows
        check("back on Options for the console test", wait("quake.state.menuScreen === 5"), str(screen_id()))

        # --- 4. The console ----------------------------------------------------
        tap_menu(100, 32 + 1.5 * 8)
        tap_menu(100, 32 + 1.5 * 8)                       # Go to console
        check("the console from Options, its buttons", wait("quake.state.flags & 2") and mode() == "console"
              and shown("#tKeyboard") and shown("#tBack"), mode())
        tap_el("#tKeyboard")
        check("KEYBOARD focuses the phone's keyboard", pg.evaluate("document.activeElement.id") == "tType")
        pg.keyboard.insert_text("echo tapped hello")
        pg.keyboard.press("Enter")
        time.sleep(0.3)
        check("typed on it: echo prints", "tapped hello" in pg.evaluate("quake.text('console_text')"))
        pg.keyboard.insert_text("echo tapped abx")
        pg.keyboard.press("Backspace")
        pg.keyboard.insert_text("c")
        pg.keyboard.press("Enter")
        time.sleep(0.3)
        check("Backspace on it deletes", "\ntapped abc\n" in pg.evaluate("quake.text('console_text')"))
        pg.screenshot(path=os.path.join(WEB, "verify_touch_console.png"))
        tap_el("#tBack")
        check("BACK closes the console", wait("!(quake.state.flags & 2)") and mode() == "play", mode())

        # Quit asks: YES / NO.
        tap_el("#tMenu")
        wait("quake.state.flags & 1")
        tap_menu(160, 122)                                # Main > Quit
        check("Quit asks: YES and NO", wait("quake.state.flags & 256") and shown("#tYes") and shown("#tNo"))
        check("menu pad hidden while it asks (STATE 256)", not shown("#tMenuPad"))
        tap_el("#tNo")
        check("NO answers", wait("!(quake.state.flags & 256)") and screen_id() == MAIN)
        tap_el("#tBack")
        check("BACK closes the menu", wait("!(quake.state.flags & 1)") and mode() == "play")

        # --- 5. Hidden page ------------------------------------------------------
        def visibility(hidden):
            pg.evaluate("""h => { Object.defineProperty(document, 'hidden', { value: h, configurable: true });
                                  Object.defineProperty(document, 'visibilityState', { value: h ? 'hidden' : 'visible', configurable: true });
                                  document.dispatchEvent(new Event('visibilitychange')); }""", hidden)
        visibility(True)
        check("hidden: paused, under the menu", wait("(quake.state.flags & 513) === 513"), str(flags()))
        visibility(False)
        tap_el("#tBack")
        check("back in the game: it runs", wait("(quake.state.flags & 513) === 0") and mode() == "play", str(flags()))

        # --- 6. Classic ----------------------------------------------------------
        call("exec profile classic")
        check("Classic: no game controls, MENU still there",
              wait("document.getElementById('touch').dataset.mode === 'game'") and shown("#tMenu") and not shown("#tFire"))
        tap_el("#tMenu")
        check("Classic: the menu is a tap away", wait("quake.state.flags & 1"))
        tap_menu(160, 82)
        check("Classic: its menu taps", wait("quake.state.menuScreen === 5"))
        call("exec profile 2026")

        # --- 6b. The menu pad clears the menu's own layout ------------------------
        # PLATFORM.md "The menu pad": off to the right of the menu's centred
        # 320-wide layout (menu_layout_point run the other way, as
        # MENU_POINT above does): the left edge of `#tMenuPad`'s box must
        # never land inside it, at any of the three phone sizes checked.
        # (Classic's own menu tap above left the menu open, on Options, and
        # switching profile does not close it — still true here.)
        check("2026 again, the menu is still open on Options", wait("quake.state.menuScreen === 5"), str(screen_id()))
        MENU_EDGE_JS = """async () => {
          const [W, H] = quake.size();
          const scaled = await exp.scaled_2d();
          let s = Math.floor(Math.min(W / 320, H / 200)), sw = W;
          if (scaled && s > 1) sw = Math.max(Math.round(W / s), 320); else s = 1;
          const ox = ((sw - 320) >> 1) * s;
          const c = document.getElementById('c'), r = c.getBoundingClientRect();
          return r.left + (ox + 320 * s) * r.width / W;
        }"""
        def menu_pad_clear_check(label, pg_=None):
            pg_ = pg_ or pg
            menu_right = pg_.evaluate(MENU_EDGE_JS)
            pad = pg_.locator("#tMenuPad").bounding_box()
            check(f"{label}: the menu pad clears the menu's own text", pad["x"] >= menu_right,
                  f"menu's right edge {menu_right:.0f}px, pad's left {pad['x']:.0f}px")

        for w, h in [(844, 390), (1012, 412), (748, 360)]:
            pg.set_viewport_size({"width": w, "height": h})
            pg.evaluate("window.dispatchEvent(new Event('resize'))")
            time.sleep(0.3)
            menu_pad_clear_check(f"{w}x{h}@3")
        pg.set_viewport_size({"width": 844, "height": 390})
        pg.evaluate("window.dispatchEvent(new Event('resize'))")
        time.sleep(0.3)
        tap_el("#tBack")                                  # Options -> Main
        tap_el("#tBack")                                  # Main -> closed
        check("menu closes, back to play", wait("!(quake.state.flags & 1)") and mode() == "play")

        # --- 9. Clear of the status bar -------------------------------------------
        # FIRE, JUMP, WEAPON and the stick's resting hint must never cover the
        # HUD's numbers, icons and inventory strip (web/PLATFORM.md, "Clear of
        # the status bar"). `sbar_height` (automation.rs) answers the
        # framebuffer rows the status bar covers right now; turned into a CSS
        # rect here independently of touch.js's own `--bar` (so a bug in its
        # arithmetic cannot hide from this check) and compared against every
        # static control's actual box, at several phone sizes and every
        # Screen size (viewsize 100/110/120).
        BAR_RECT_JS = """async () => {
          const rows = (await quake.callLine('sbar_height')).value;
          const [, H] = quake.size();
          const r = document.getElementById('c').getBoundingClientRect();
          const h = H > 0 && rows > 0 ? rows * r.height / H : 0;
          return { left: r.left, right: r.right, top: r.bottom - h, bottom: r.bottom, rows };
        }"""
        BAR_SETTLED_JS = """async () => {
          const rows = (await quake.callLine('sbar_height')).value;
          const [, H] = quake.size();
          const r = document.getElementById('c').getBoundingClientRect();
          const h = H > 0 && rows > 0 ? rows * r.height / H : 0;
          const cur = parseFloat(getComputedStyle(document.getElementById('touch')).getPropertyValue('--bar')) || 0;
          return Math.abs(cur - h) < 0.75;
        }"""
        SAFE_ZONE_CONTROLS = ["#tFire", "#tJump", "#tWeapon", "#tHint"]
        def overlaps(a, b):
            return a["left"] < b["right"] and b["left"] < a["right"] and a["top"] < b["bottom"] and b["top"] < a["bottom"]
        def set_viewsize(pg_, call_, vs):
            call_(f"exec viewsize {vs}")
            pg_.evaluate("window.dispatchEvent(new Event('resize'))")
            isolated.wait_until(pg_, BAR_SETTLED_JS, 5)   # async: `wait` (wait_for_function) would not wait
        def safe_zone_check(pg_, label):
            bar = pg_.evaluate(BAR_RECT_JS)
            bad = []
            for sel in SAFE_ZONE_CONTROLS:
                if not shown(sel, pg_):
                    continue
                box = pg_.locator(sel).bounding_box()
                rect = {"left": box["x"], "right": box["x"] + box["width"], "top": box["y"], "bottom": box["y"] + box["height"]}
                if overlaps(rect, bar):
                    bad.append((sel, rect))
            check(f"{label}: FIRE/JUMP/WEAPON/the hint clear the status bar", not bad,
                  f"bar {bar}" + (f" overlaps {bad}" if bad else ""))

        for w, h in [(844, 390), (1012, 412), (748, 360)]:
            pg.set_viewport_size({"width": w, "height": h})
            pg.evaluate("window.dispatchEvent(new Event('resize'))")
            time.sleep(0.3)
            for vs in (100, 110, 120):
                set_viewsize(pg, call, vs)
                safe_zone_check(pg, f"{w}x{h}@3 viewsize {vs}")
            if (w, h) == (748, 360):
                pg.screenshot(path=os.path.join(WEB, "verify_touch_safezone_748x360.png"))
        set_viewsize(pg, call, 100)
        pg.set_viewport_size({"width": 844, "height": 390})
        pg.evaluate("window.dispatchEvent(new Event('resize'))")
        time.sleep(0.3)

        # --- 10. The same, at a different pixel ratio and more phone sizes --------
        # A second phone (a phone-sized landscape viewport at devicePixelRatio 2.6):
        # native resolution's whole-pixel quantization rounds differently at
        # a different ratio, so this is not just the same math re-run.
        PHONE_26 = dict(viewport={"width": 1012, "height": 412}, device_scale_factor=2.6, is_mobile=True, has_touch=True)
        ctx3 = phone(**PHONE_26)
        ctx3.add_init_script(NO_FULLSCREEN_JS)
        pg3 = ctx3.new_page()
        errs3 = []
        pg3.on("console", lambda m: errs3.append(m.text) if m.type == "error" else None)
        pg3.on("pageerror", lambda e: errs3.append("PAGEERROR: " + str(e)))
        call3 = make_call(pg3)
        pg3.goto(f"http://127.0.0.1:{PORT}/index.html?2026", wait_until="load")
        pg3.wait_for_function("window.quake && quake.ready && window.QuakeTouch", timeout=120000)
        pg3.touchscreen.tap(506, 206)                     # tap to start (viewport centre)
        wait("document.getElementById('touch') && document.getElementById('touch').dataset.mode === 'demo'", pg_=pg3)
        call3("boot")                                     # New Game, the walkBtn shortcut
        time.sleep(0.5)                                    # e1m1 built synchronously inside it
        pg3.keyboard.press("Escape")                       # close the menu it lands on
        check("1012x412@2.6: reaches play",
              wait("document.getElementById('touch').dataset.mode === 'play'", 20000, pg3), mode(pg3))
        for vs in (100, 110, 120):
            set_viewsize(pg3, call3, vs)
            safe_zone_check(pg3, f"1012x412@2.6 viewsize {vs}")
        pg3.screenshot(path=os.path.join(WEB, "verify_touch_safezone_1012x412.png"))

        # The menu pad, at the phone profile's size (PHONE_26 — web/PLATFORM.md
        # "The menu pad"): Options, with the pad showing and clear of it.
        def tap_el3(sel):
            box = pg3.locator(sel).bounding_box()
            pg3.touchscreen.tap(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            time.sleep(0.25)
        tap_el3("#tMenu")
        wait("quake.state.flags & 1", pg_=pg3)
        x, y = pg3.evaluate(MENU_POINT, [160, 82])        # Main > Options
        pg3.touchscreen.tap(x, y)
        time.sleep(0.25)
        check("1012x412@2.6: Options by a tap", wait("quake.state.menuScreen === 5", pg_=pg3), mode(pg3))
        check("1012x412@2.6: the menu pad shows over Options", shown("#tMenuPad", pg3))
        menu_pad_clear_check("1012x412@2.6", pg3)
        pg3.screenshot(path=os.path.join(WEB, "verify_touch_menupad_1012x412.png"))
        tap_el3("#tBack")                                  # Options -> Main
        tap_el3("#tBack")                                  # Main -> closed
        check("1012x412@2.6: back to play", wait("!(quake.state.flags & 1)", pg_=pg3) and mode(pg3) == "play")

        pg3.set_viewport_size({"width": 915, "height": 412})
        pg3.evaluate("window.dispatchEvent(new Event('resize'))")
        time.sleep(0.3)
        for vs in (100, 110, 120):
            set_viewsize(pg3, call3, vs)
            safe_zone_check(pg3, f"915x412@2.6 viewsize {vs}")
        set_viewsize(pg3, call3, 100)

        # A portrait phone: the "turn sideways" prompt, not the play layout,
        # but MENU (a tap to play upright, then the demo's own MENU button)
        # must still be reachable.
        pg3.set_viewport_size({"width": 412, "height": 1012})
        pg3.evaluate("window.dispatchEvent(new Event('resize'))")
        time.sleep(0.3)
        check("412x1012 portrait: the rotate prompt", shown("#tRotate", pg3))
        pg3.locator("#tRotate").click()
        check("a tap plays upright anyway", not shown("#tRotate", pg3) and shown("#tMenu", pg3))
        pg3.screenshot(path=os.path.join(WEB, "verify_touch_portrait.png"))

        check("other phone sizes: no console errors", not errs3, str(errs3[-5:]))
        ctx3.close()

        # --- 11. The game picker -------------------------------------------------
        # PLATFORM.md "The game picker", in the phone profile (PHONE_26), with a
        # deploy offering a mission pack (pack_deploy). Every tap is CDP
        # touch events, a finger's touchStart/touchEnd, from which the
        # browser makes the pointerdown, pointerup and click itself.
        packs = pack_deploy()
        httpd4 = serve(PORT + 2, True, packs)
        ctx4 = phone(**PHONE_26)
        ctx4.add_init_script(NO_FULLSCREEN_JS)
        ctx4.add_init_script(PICK_PROBE_JS)
        pg4 = ctx4.new_page()
        errs4 = []
        pg4.on("console", lambda m: errs4.append(m.text) if m.type == "error" else None)
        pg4.on("pageerror", lambda e: errs4.append("PAGEERROR: " + str(e)))
        cdp4 = None if FIREFOX else ctx4.new_cdp_session(pg4)
        base4 = f"http://127.0.0.1:{PORT + 2}/index.html"
        def finger(x, y):
            if FIREFOX:
                pg4.touchscreen.tap(x, y)
            else:
                cdp4.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": [{"x": x, "y": y, "id": 9}]})
                time.sleep(0.06)
                cdp4.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
            time.sleep(0.25)
        def finger_on(sel):
            box = pg4.locator(sel).bounding_box()
            finger(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
        def up4():
            pg4.wait_for_function("window.quake && quake.ready && window.QuakeTouch", timeout=120000)
        def went(url):
            try:
                pg4.wait_for_url(url, timeout=15000)
                up4()
                return True
            except Exception:
                return False
        def probe():
            return json.loads(pg4.evaluate("sessionStorage.getItem('pickProbe') || 'null'") or "null")
        CHOICES = """sel => [...document.querySelectorAll(sel + ' .game')].map(g => { const r = g.getBoundingClientRect();
            return { game: g.dataset.game, name: g.textContent, cur: g.classList.contains('cur'), href: g.getAttribute('href'), h: r.height }; })"""
        choices = lambda sel: pg4.evaluate(CHOICES, sel)
        def marked(sel, game):
            cs = choices(sel)
            return bool(cs) and all(c["cur"] == (c["game"] == game) and (c["href"] is None) == c["cur"] for c in cs)
        mode4 = lambda: mode(pg4)

        pg4.goto(base4 + "?2026", wait_until="load")
        up4()
        cs = choices("#gamePicker")
        check("11. the start overlay offers Quake and Scourge of Armagon, Quake marked",
              [c["name"] for c in cs] == ["Quake", "Scourge of Armagon"] and marked("#gamePicker", "id1"), str(cs))
        check("...each a fingertip's height (44 CSS px)", all(c["h"] >= 44 for c in cs), str([c["h"] for c in cs]))
        pg4.screenshot(path=os.path.join(WEB, "verify_touch_picker_start.png"))
        finger_on("#gamePicker a.game[data-game=hipnotic]")
        check("a finger on Scourge of Armagon reloads the page as ?game=hipnotic",
              went(base4 + "?2026&game=hipnotic"), pg4.url)
        pr = probe()
        check("...its pointerdown, pointerup and click all on the link",
              pr and pr["seq"] == ["pointerdown hipnotic", "pointerup hipnotic", "click hipnotic"], str(pr))
        check("...and never also the tap that starts: the overlay stayed, no fullscreen asked",
              pr and not pr["overlayHidden"] and not pr["fullscreenAsked"] and pr["mode"] == "boot", str(pr))
        check("the new page marks Scourge of Armagon, the engine runs it",
              marked("#gamePicker", "hipnotic") and pg4.evaluate("quake.content.state().game") == "hipnotic", str(choices("#gamePicker")))
        pg4.screenshot(path=os.path.join(WEB, "verify_touch_picker_hipnotic.png"))

        finger_on("#play")                                # tap to start (the running game: no choice)
        check("tap to start: the attract demo, no GAME outside the menu",
              wait("document.getElementById('touch').dataset.mode === 'demo'", pg_=pg4) and not shown("#tGame", pg4), mode4())
        finger(500, 200)                                  # any tap: the menu
        check("in the menu: GAME beside BACK", wait("quake.state.flags & 1", pg_=pg4) and mode4() == "menu"
              and shown("#tGame", pg4) and shown("#tBack", pg4), mode4())
        menu_left = pg4.evaluate(MENU_POINT, [0, 0])[0]
        game_box = pg4.locator("#tGame").bounding_box()
        check("...clear of the menu's own layout", game_box["x"] + game_box["width"] <= menu_left,
              f"GAME's right {game_box['x'] + game_box['width']:.0f}px, the menu's left {menu_left:.0f}px")
        pg4.screenshot(path=os.path.join(WEB, "verify_touch_picker_menu.png"))
        finger_on("#tGame")
        check("GAME opens the same choices, Scourge of Armagon marked", shown("#tGames", pg4) and marked("#tGames", "hipnotic"),
              str(choices("#tGames")))
        # Beside the choices, on Main's Quit row (the menu's layout x 8,
        # left of the panel's column): with the panel up, it only closes it.
        qx, qy = pg4.evaluate(MENU_POINT, [8, 120])
        before = (pg4.evaluate("exp.menu_screen_id()"), pg4.evaluate("exp.menu_cursor()"))
        finger(qx, qy)
        time.sleep(0.3)
        after = (pg4.evaluate("exp.menu_screen_id()"), pg4.evaluate("exp.menu_cursor()"))
        check("a tap beside the choices closes them, the menu under them untouched",
              not shown("#tGames", pg4) and before == after and mode4() == "menu" and not pg4.evaluate("quake.state.flags & 256"),
              f"{before} -> {after}")
        finger(qx, qy)                                    # the control: without the panel, that spot is Quit
        check("...where, without the panel, the same tap is the menu's (Quit asks)",
              wait("quake.state.flags & 256", pg_=pg4) and mode4() == "ask", mode4())
        finger_on("#tNo")
        wait("!(quake.state.flags & 256)", pg_=pg4)
        finger_on("#tGame")
        pg4.screenshot(path=os.path.join(WEB, "verify_touch_picker_panel.png"))
        finger_on("#tGamesCancel")
        check("CANCEL closes them", not shown("#tGames", pg4) and mode4() == "menu")
        finger_on("#tGame")
        finger_on("#tGames a.game[data-game=id1]")
        check("its Quake reloads the page as plain ?2026", went(base4 + "?2026"), pg4.url)
        pr = probe()
        check("...from a finger on the link", pr and pr["seq"] == ["pointerdown id1", "pointerup id1", "click id1"], str(pr))
        check("Quake marked again", marked("#gamePicker", "id1"), str(choices("#gamePicker")))

        # The quit screen (endscreen.js) offers them too: any tap there
        # restarts this game, a choice starts that one.
        finger_on("#play")
        wait("document.getElementById('touch').dataset.mode === 'demo'", pg_=pg4)
        pg4.evaluate("quake.callLine('console_toggle')")
        pg4.evaluate("quake.callLine('exec quit')")
        try:
            pg4.wait_for_selector("#quakeEndScreen", timeout=12000)
            ended = True
        except Exception:
            ended = False
        check("the quit screen offers the choices, Quake marked", ended and marked("#quakeEndScreen", "id1")
              and pg4.evaluate("!!document.querySelector('#quakeEndScreen .qesGrid')"), str(choices("#quakeEndScreen")) if ended else "no end screen")
        pg4.screenshot(path=os.path.join(WEB, "verify_touch_picker_end.png"))
        finger_on("#quakeEndScreen a.game[data-game=hipnotic]")
        check("...its Scourge of Armagon starts that game, not this one again",
              went(base4 + "?2026&game=hipnotic") and marked("#gamePicker", "hipnotic"), pg4.url)
        check("the game picker: no console errors", not errs4, str(errs4[-5:]))
        ctx4.close()
        httpd4.shutdown()
        httpd4.server_close()
        shutil.rmtree(packs, ignore_errors=True)

        errs_online = list(errs)
        print("errors:", errs_online[-5:])
        check("no console errors", not errs_online)

        # --- 7. Offline ----------------------------------------------------------
        kept = pg.evaluate("""async () => { const out = [];
            for (const k of await caches.keys()) for (const r of await (await caches.open(k)).keys()) out.push(new URL(r.url).pathname);
            return out; }""")
        want = ["/index.html", "/wasi.js", "/touch.js", "/quake.wasm", "/id1/pak0.pak"]
        check("the service worker controls the page and kept its files",
              pg.evaluate("!!navigator.serviceWorker.controller") and all(w in kept for w in want), str(sorted(kept)))
        httpd.shutdown()
        httpd.server_close()
        pg.reload(wait_until="load")
        ok = wait("window.quake && quake.ready", 60000)
        time.sleep(1.0)
        drawn = pg.evaluate("quake.pixels().filter((v, i) => i % 4 === 0 && v > 25).length") if ok else 0
        check("offline: the reload plays from the cache", ok and drawn > 20000, f"{drawn} lit pixels")
        pg.screenshot(path=os.path.join(WEB, "verify_touch_offline.png"))
        ctx.close()

        # --- 8. A server with no isolation headers ---------------------------------
        plain = serve(PORT + 1, False)
        ctx2 = br.new_context()
        pg2 = ctx2.new_page()
        pg2.goto(f"http://127.0.0.1:{PORT + 1}/index.html", wait_until="load")
        try:
            pg2.wait_for_function("window.quake && quake.ready", timeout=60000)
            ok = True
        except Exception:
            ok = False
        check("no headers: the service worker isolates the page, the game runs",
              ok and pg2.evaluate("self.crossOriginIsolated"),
              pg2.evaluate("document.getElementById('status').textContent"))
        ctx2.close()
        plain.shutdown()
        br.close()
    print(f"done: {passed} passed, {failed} failed" + (f", {skipped} skipped" if skipped else ""))
    sys.exit(1 if failed else 0)


main()
