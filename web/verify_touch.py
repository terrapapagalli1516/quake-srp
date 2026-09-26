#!/usr/bin/env -S uv run --with playwright --script
"""Verify the page on a phone: the touch controls (web/touch.js), the tappable
menus, and offline play through the service worker (web/sw.js), in headless
Chromium emulating a phone held sideways (844x390 CSS pixels at
devicePixelRatio 3, `isMobile`, `hasTouch`). Taps are Playwright's
touchscreen; drags are CDP touch events (Input.dispatchTouchEvent), so this
check is Chromium's only.

  1. The page: touch.js loads on the coarse pointer, the touch layout fills
     the screen, "tap to start"; the rotate prompt shows upright and a tap
     dismisses it; the manifest and its icons.
  2. The menu by tapping (the engine's own item layout: menu_tap): a tap on
     the demo brings the menu, Single Player > New Game starts the game;
     Options' Always Run takes a tap to point and one to flip; Quit asks,
     with YES / NO buttons; BACK backs out.
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
import functools, http.server, json, math, os, socketserver, sys, threading, time
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


def serve(port, isolation):
    socketserver.ThreadingTCPServer.allow_reuse_address = True
    handler = type("H", (Handler,), {"isolation": isolation})
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", port), functools.partial(handler, directory=WEB))
    httpd.daemon_threads = True
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

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

def main():
    httpd = serve(PORT, True)
    with sync_playwright() as p:
        br = p.chromium.launch(headless=True, args=["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
        ctx = br.new_context(**PHONE)
        pg = ctx.new_page()
        errs = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        cdp = ctx.new_cdp_session(pg)

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

        mode = lambda: pg.evaluate("document.getElementById('touch') && document.getElementById('touch').dataset.mode")
        flags = lambda: pg.evaluate("quake.state.flags")
        screen_id = lambda: pg.evaluate("exp.menu_screen_id()")
        call = lambda line: pg.evaluate(f"quake.call({json.dumps(line.split()[0])}, ...{json.dumps(line.split()[1:])})")
        field = lambda name: pg.evaluate(f"quake.callLine('player_field {name}').then(r => r.value)")
        def tap_menu(vx, vy):
            x, y = pg.evaluate(MENU_POINT, [vx, vy])
            pg.touchscreen.tap(x, y)
            time.sleep(0.25)
        def tap_el(sel):
            box = pg.locator(sel).bounding_box()
            pg.touchscreen.tap(box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)
            time.sleep(0.25)
        def shown(sel):
            return pg.evaluate(f"(() => {{ const e = document.querySelector({json.dumps(sel)}); return !!e && getComputedStyle(e).display !== 'none' && !e.hidden; }})()")
        def wait(js, timeout=5000):
            try:
                pg.wait_for_function(js, timeout=timeout)
                return True
            except Exception:
                return False
        def listener():
            return pg.evaluate("Promise.all(['listener_x','listener_y','listener_z','listener_fwd_x','listener_fwd_y'].map(n => quake.call(n)))")

        # --- 1. The page -------------------------------------------------------
        pg.goto(f"http://127.0.0.1:{PORT}/index.html?2026", wait_until="load")
        pg.wait_for_function("window.quake && quake.ready && window.QuakeTouch", timeout=120000)
        check("touch.js on a coarse pointer, the touch layout",
              pg.evaluate("document.documentElement.classList.contains('touch') && !!document.getElementById('touch')"))
        check("no keyboard-and-mouse note", pg.evaluate("!document.getElementById('touchNote')"))
        check("the start prompt says tap", pg.evaluate("document.getElementById('play').textContent") == "tap to start")
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
        pg.touchscreen.tap(500, 200)                      # any tap: the menu
        check("a tap on the demo opens the menu", wait("quake.state.flags & 1") and mode() == "menu" and shown("#tBack"))
        pg.screenshot(path=os.path.join(WEB, "verify_touch_menu.png"))
        tap_menu(160, 42)                                 # Main > Single Player
        check("Single Player by a tap", wait("quake.state.menuScreen === 1"), str(screen_id()))
        tap_menu(160, 42)                                 # New Game
        check("New Game by a tap: the game starts, the controls show",
              wait("document.getElementById('touch').dataset.mode === 'play'", 20000)
              and all(shown(s) for s in ["#tFire", "#tJump", "#tWeapon", "#tMenu"]), mode())
        time.sleep(1.0)

        # --- 3. Play -------------------------------------------------------------
        pg.screenshot(path=os.path.join(WEB, "verify_touch_play.png"))
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
    print(f"done: {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


main()
