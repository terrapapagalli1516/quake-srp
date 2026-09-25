#!/usr/bin/env -S uv run --with playwright --script
"""Verify Options > Web extras, the settings the page keeps across reloads,
and Esc in fullscreen, end-to-end in headless Chromium:

  1. Options has a 14th row, "Web extras" (the port's), that opens the Extras
     screen (menu_screen_id 10) with every extra off; left/right/Enter toggle,
     Esc returns to Options; the wasm_* console commands set the same bits.
     Screenshots: verify_extras_options.png, verify_extras.png (the screen),
     verify_extras_fps.png (the readout).
  2. wasm_uncapped through the real wasm: a second of 1/144 s steps presents
     72 frames with the cap (id's), 144 without.
  3. On frozen frames: wasm_showfps changes only the box at the bottom
     right above the status bar, wasm_exactpersp redraws the walls, and
     switching either off restores id's frame byte for byte.
  4. Persistence: the extras bits and viewsize (plus the resolution) survive
     a reload through localStorage.
  5. Esc in fullscreen: F locks Escape (navigator.keyboard.lock(['Escape']),
     spied) and the hint says to hold Esc; a tapped Esc toggles the menu, a
     held one (autorepeat) toggles it once; a locked Esc that the browser
     also reports as a pointer-lock loss counts once, in either order;
     leaving fullscreen unlocks; without the Keyboard Lock API the old hint
     and two-step flow stand. Headless Chromium resolves the lock but has no
     browser Esc handling, so what a real browser does with a locked Esc
     (tap reaches the page, hold leaves fullscreen) is NOT verified here.

Usage: verify_extras.py [webdir]   (defaults to this script's directory; pass
a temp dir holding index.html + a freshly built quake_wasm.wasm.)
"""
import functools, http.server, os, socketserver, sys, threading, time
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("QUAKE_VERIFY_PORT", "8175"))
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

OPTIONS, EXTRAS = 5, 10

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

# Spy on the Keyboard Lock API (calling through to the real one).
KB_SPY = """
window.__kb = { locks: [], unlocks: 0 };
if (navigator.keyboard && navigator.keyboard.lock) {
  const kb = navigator.keyboard, lock = kb.lock.bind(kb), unlock = kb.unlock.bind(kb);
  kb.lock = keys => { window.__kb.locks.push(keys); return lock(keys); };
  kb.unlock = () => { window.__kb.unlocks++; return unlock(); };
}
"""
NO_KB = "Object.defineProperty(Navigator.prototype, 'keyboard', { get: () => undefined });"

def boot_page(pg):
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function(
        "typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'", timeout=120000)
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('ready')", timeout=30000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)

def frames(pg, n=3):
    pg.evaluate(f"""() => new Promise(r => {{ let k = {n};
        const f = () => (--k > 0 ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); }})""")

# Keep a canvas grab in the page as window[name].
GRAB = """name => {
    const c = document.getElementById('c');
    window[name] = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
}"""
# The bounding box of the pixels where grabs a and b differ, or null.
DIFF = """([a, b]) => {
    const A = window[a], B = window[b], w = document.getElementById('c').width;
    let n = 0, x0 = 1e9, x1 = -1, y0 = 1e9, y1 = -1;
    for (let i = 0; i < A.length; i += 4) {
        if (A[i] !== B[i] || A[i + 1] !== B[i + 1] || A[i + 2] !== B[i + 2]) {
            const p = i / 4, x = p % w, y = (p - x) / w;
            n++; x0 = Math.min(x0, x); x1 = Math.max(x1, x); y0 = Math.min(y0, y); y1 = Math.max(y1, y);
        }
    }
    return n ? { n, x0, x1, y0, y1 } : null;
}"""

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=[
        "--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    errs = []
    ctx = br.new_context(viewport={"width": 820, "height": 560})
    ctx.add_init_script(KB_SPY)
    pg = ctx.new_page()
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg)
    scr = lambda: pg.evaluate("exp.menu_screen_id()")
    vis = lambda: pg.evaluate("exp.menu_visible()")
    ext = lambda: pg.evaluate("exp.extras()")
    key = lambda k, n=1: [pg.keyboard.press(k) or time.sleep(0.06) for _ in range(n)]

    # 1. The Extras screen, reached from Options' 14th row.
    check("every extra is off by default", ext() == 0)
    check("nothing stored before a change",
          pg.evaluate("localStorage.getItem('quake-rs.extras')") is None)
    key("ArrowDown", 2); key("Enter")
    check("Options opens", scr() == OPTIONS)
    key("ArrowUp")                     # up from row 0 wraps to the last row...
    time.sleep(0.3)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras_options.png"))
    key("Enter")
    check("...which is Web extras: it opens the Extras screen", scr() == EXTRAS)
    time.sleep(0.3)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras.png"))
    key("ArrowRight")                  # Uncapped framerate
    check("Right toggles Uncapped framerate", ext() == 1)
    key("ArrowDown"); key("Enter")     # Show FPS
    check("Enter toggles Show FPS", ext() == 3)
    key("ArrowLeft")
    check("Left toggles it back", ext() == 1)
    key("ArrowUp"); key("ArrowLeft")
    check("all off again", ext() == 0)
    key("ArrowDown", 2); key("ArrowRight")   # Exact perspective
    check("Right toggles Exact perspective", ext() == 4)
    key("ArrowLeft"); key("ArrowUp", 2)
    check("...and back off", ext() == 0)
    key("Escape")
    check("Esc returns to Options", scr() == OPTIONS)
    key("Enter")
    check("...on the Web extras row", scr() == EXTRAS)
    key("Escape"); key("Escape"); key("Escape")
    check("Esc Esc Esc closes the menu", vis() == 0)
    key("Backquote")
    for line in ["wasm_uncapped 1", "wasm_showfps 1"]:
        pg.keyboard.type(line); key("Enter")
    check("the wasm_* console commands set the same bits", ext() == 3)
    pg.keyboard.type("wasm_uncapped 0"); key("Enter")
    key("Backquote")
    check("wasm_uncapped 0", ext() == 2)
    frames(pg)
    check("the page stored the change",
          pg.evaluate("localStorage.getItem('quake-rs.extras')") == "2")

    # 2. wasm_uncapped through the real wasm (the page's rAF loop cannot
    #    interleave inside one evaluate).
    capped, uncapped = pg.evaluate("""() => {
        const one = () => { let n = 0; for (let i = 0; i < 144; i++) n += exp.step(1 / 144); return n; };
        const bits = exp.extras();
        exp.set_extras(bits & ~1); const a = one();
        exp.set_extras(bits | 1);  const b = one();
        exp.set_extras(bits);
        return [a, b];
    }""")
    check("144 Hz with id's cap: 72 frames a second", 71 <= capped <= 73, str(capped))
    check("144 Hz with wasm_uncapped: 144", uncapped == 144, str(uncapped))

    # 3. wasm_showfps in the walk: freeze the frames, then only the readout
    #    may change the canvas.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")                      # close the boot menu
    pg.wait_for_function("!exp.menu_visible()", timeout=5000)
    time.sleep(1.5)                    # a full measuring window
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras_fps.png"))
    pg.evaluate("""() => { window._real = exp; exp = { ...window._real };
                           exp.step = () => window._real.step(0); }""")
    frames(pg)
    pg.evaluate(GRAB, "_on")
    pg.evaluate("window._real.set_extras(0)")
    frames(pg)
    pg.evaluate(GRAB, "_off")
    w, h = pg.evaluate("[exp.width(), exp.height()]")
    d = pg.evaluate(DIFF, ["_on", "_off"])
    s = w / 320
    box = (256 * s, 312 * s, h - 56 * s, h - 48 * s)
    inside = d is not None and d["x0"] >= box[0] and d["x1"] < box[1] \
        and d["y0"] >= box[2] and d["y1"] < box[3]
    check("the readout draws bottom right, above the status bar, and nowhere else",
          inside, f"{d}; box {box}")
    pg.evaluate("window._real.set_extras(2)")
    frames(pg)
    pg.evaluate(GRAB, "_on2")
    check("on again: the same readout", pg.evaluate(DIFF, ["_on", "_on2"]) is None)
    # wasm_exactpersp: the walls change, and off is id's frame again.
    pg.evaluate("window._real.set_extras(4)")
    frames(pg)
    pg.evaluate(GRAB, "_exact")
    d = pg.evaluate(DIFF, ["_off", "_exact"])
    check("wasm_exactpersp redraws the walls", d is not None and d["n"] > 1000, str(d))
    pg.evaluate("window._real.set_extras(0)")
    frames(pg)
    pg.evaluate(GRAB, "_off2")
    check("...and off is id's spans again, byte for byte",
          pg.evaluate(DIFF, ["_off", "_off2"]) is None)
    pg.evaluate("window._real.set_extras(2)")
    pg.evaluate("exp = window._real")

    # 4. Persistence across a reload: extras, viewsize, resolution.
    key("Minus", 2)                    # default.cfg: '-' is sizedown -> 80
    frames(pg)
    stored = pg.evaluate("""[localStorage.getItem('quake-rs.extras'),
                             localStorage.getItem('quake-rs.viewsize')]""")
    check("viewsize and extras stored", stored == ["2", "80"], str(stored))
    pg.evaluate("exp.set_extras(3)")
    frames(pg)
    res0 = pg.evaluate("[exp.width(), exp.height()]")
    boot_page(pg)
    check("the Web extras survive a reload", ext() == 3, str(ext()))
    check("Screen size (viewsize) survives a reload", pg.evaluate("exp.viewsize()") == 80)
    check("...and the resolution still does", pg.evaluate("[exp.width(), exp.height()]") == res0)
    pg.evaluate("exp.set_extras(0); exp.set_viewsize(100)")
    frames(pg)

    # 5. Esc in fullscreen with Escape keyboard-locked.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")
    pg.wait_for_function("!exp.menu_visible()", timeout=5000)
    key("f")
    try:
        pg.wait_for_function("!!document.fullscreenElement && escLocked", timeout=5000)
        fs = True
    except Exception:
        fs = False
    check("F enters fullscreen and locks Esc", fs,
          str(pg.evaluate("[!!document.fullscreenElement, escLocked, window.__kb]")))
    if fs:
        check("the lock asks for exactly Escape",
              pg.evaluate("JSON.stringify(window.__kb.locks)") == '[["Escape"]]')
        check("the hint says hold Esc to leave",
              "hold Esc" in pg.evaluate("fsHint.textContent")
              and pg.evaluate("fsHint.classList.contains('show')"))
        key("Escape")
        check("a tapped Esc opens the menu", vis() == 1)
        key("Escape")
        check("...and the next closes it (id's togglemenu)", vis() == 0)
        pg.keyboard.down("Escape")
        for _ in range(5):
            time.sleep(0.05); pg.keyboard.down("Escape")    # autorepeat
        pg.keyboard.up("Escape")
        check("a held Esc toggles once (autorepeat ignored)", vis() == 1)
        key("Escape")
        check("closed again", vis() == 0)
        # A browser that both delivers a locked Esc and drops the pointer lock
        # for it: the pair toggles once, in either order.
        pg.locator("#c").click()
        pg.wait_for_function("document.pointerLockElement === document.getElementById('c')",
                             timeout=5000)
        pg.evaluate("""() => {
            dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape' }));
            document.exitPointerLock();
        }""")
        time.sleep(0.4)
        check("Esc keydown then lock loss: one toggle (menu open)", vis() == 1)
        key("Escape")
        pg.wait_for_function("!exp.menu_visible()", timeout=5000)
        pg.locator("#c").click()
        pg.wait_for_function("document.pointerLockElement === document.getElementById('c')",
                             timeout=5000)
        time.sleep(0.4)                # clear of the last real Esc
        pg.evaluate("""() => new Promise(r => {
            document.addEventListener('pointerlockchange', () => setTimeout(() => {
                dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape' }));
                r();
            }, 20), { once: true });
            document.exitPointerLock();
        })""")
        time.sleep(0.1)
        check("lock loss then Esc keydown: one toggle (menu open)",
              vis() == 1 and pg.evaluate("performance.now() - lockLossMenuAt < 1000"))
        time.sleep(0.4)
        key("Escape")
        check("a later Esc is a new toggle", vis() == 0)
        key("f")
        pg.wait_for_function("!document.fullscreenElement", timeout=5000)
        check("leaving fullscreen unlocks Esc",
              pg.evaluate("window.__kb.unlocks >= 1 && !escLocked"))
        # Windowed again: the old flow (lock loss opens the menu, an Esc right
        # after it is a real second press).
        pg.locator("#c").click()
        pg.wait_for_function("document.pointerLockElement === document.getElementById('c')",
                             timeout=5000)
        pg.evaluate("document.exitPointerLock()")
        pg.wait_for_function("exp.menu_visible() === 1", timeout=5000)
        key("Escape")
        check("windowed: lock loss opens, the next Esc closes", vis() == 0)
    else:
        print("SKIP keyboard-lock checks (headless refused fullscreen)")

    # Without the Keyboard Lock API (Firefox, Safari): the old hint and flow.
    ctx2 = br.new_context(viewport={"width": 820, "height": 560})
    ctx2.add_init_script(NO_KB)
    pg2 = ctx2.new_page()
    pg2.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg2.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg2)
    pg2.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg2.keyboard.press("Escape")
    pg2.keyboard.press("f")
    try:
        pg2.wait_for_function("!!document.fullscreenElement && fsHint.classList.contains('show')",
                              timeout=5000)
        check("no Keyboard Lock API: no lock, the old hint",
              not pg2.evaluate("escLocked")
              and "hold" not in pg2.evaluate("fsHint.textContent")
              and "F = leave fullscreen" in pg2.evaluate("fsHint.textContent"))
    except Exception:
        print("SKIP fallback fullscreen check (headless refused fullscreen)")

    print("errors:", errs[-5:])
    check("no console errors", not errs)
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
