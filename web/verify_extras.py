#!/usr/bin/env -S uv run --with playwright --script
"""Verify Options > Classic / 2026 and its settings page, the settings the
page keeps across reloads, and Esc in fullscreen, end-to-end in headless
Chromium. The page opens as `?classic` (every departure off, id's keys):

  1. Options' 14th row, "Classic / 2026" (the port's): left/right switch the
     whole profile (the 2026 one turns wasm_uncapped and wasm_scaled2d on),
     Enter opens the settings page (menu_screen_id 10), whose rows switch
     each setting (Uncapped framerate row 1, Show FPS row 11, Exact
     perspective row 12: left/right/Enter), Esc returns to Options; the
     wasm_* console variables set the same settings, with the console's
     history and Tab completion. Screenshots: verify_extras_options.png,
     verify_extras.png (the page), verify_extras_fps.png (the readout).
  2. wasm_uncapped through the real program: a second of 1/144 s steps runs
     72 host frames with the cap (id's), 144 without.
  3. On frozen frames: wasm_showfps changes only the box at the bottom
     right above the status bar, wasm_exactpersp redraws the walls, and
     switching either off restores id's frame byte for byte.
  4. Persistence: config.cfg keeps the profile and what differs from it
     (`wasm_showfps "1"`, `viewsize "80"`, ...), and a plain reload (no
     `?classic`) comes back Classic with them.
  5. Esc in fullscreen (with vid_fkey on: F is the page's fullscreen key in
     2026 only): F locks Escape (navigator.keyboard.lock(['Escape']),
     spied) and the hint says to hold Esc; a tapped Esc toggles the menu, a
     held one (autorepeat) toggles it once; a locked Esc that the browser
     also reports as a pointer-lock loss counts once, in either order;
     leaving fullscreen unlocks; without the Keyboard Lock API the old hint
     and two-step flow stand. Headless Chromium resolves the lock but has no
     browser Esc handling, so what a real browser does with a locked Esc
     (tap reaches the page, hold leaves fullscreen) is NOT verified here.

Usage: verify_extras.py [webdir]   (defaults to this script's directory; pass
a deploy dir — PLATFORM.md.)
"""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8175)
httpd = isolated.serve(WEB, PORT)

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

def boot_page(pg, query="?classic"):
    pg.goto(f"http://127.0.0.1:{PORT}/index.html{query}", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)

def frames(pg, n=3):
    pg.evaluate(f"""() => new Promise(r => {{ let k = {n};
        const f = () => (--k > 0 ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); }})""")

# A frozen frame (dt 0: the automation's frame, no clock moves), presented,
# with the page's own ticks paused.
FROZEN = "() => { quake.tick(0); quake.tick(0); }"

# config.cfg as the page keeps it (the program writes it on the frame after
# a change).
CFG = "quake.kept('id1/config.cfg')"
def cfg_has(pg, *lines):
    try:
        pg.wait_for_function(CFG + ".then(t => !!t && " + " && ".join(f"t.includes('{l}\\n')" for l in lines) + ")",
                             timeout=5000)
        return True
    except Exception:
        return False

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
    br = isolated.launch(p, [
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

    # 1. Options' 14th row switches the profile; Enter opens the page.
    prof = lambda: pg.evaluate("quake.text('profile')")
    check("?classic: the Classic profile, every setting off", prof() == "classic" and ext() == 0)
    frames(pg)
    check("config.cfg keeps the profile the address chose",
          cfg_has(pg, 'profile "classic"'), str(pg.evaluate(CFG)))
    key("Escape")                      # the menu over the attract demo
    key("ArrowDown", 2); key("Enter")
    check("Options opens", scr() == OPTIONS)
    key("ArrowUp")                     # up from row 0 wraps to the last row...
    time.sleep(0.3)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras_options.png"))
    key("ArrowRight")
    check("...Classic / 2026: right switches to 2026", prof() == "2026" and ext() == 9, str(ext()))
    key("ArrowLeft")
    check("left: back to Classic", prof() == "classic" and ext() == 0)
    key("Enter")
    check("Enter opens the settings page", scr() == EXTRAS)
    time.sleep(0.3)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras.png"))
    key("ArrowDown"); key("ArrowRight")    # row 1: Uncapped framerate
    check("Right toggles Uncapped framerate", ext() == 1)
    key("ArrowDown", 10); key("Enter")     # row 11: Show FPS
    check("Enter toggles Show FPS", ext() == 3)
    key("ArrowLeft")
    check("Left toggles it back", ext() == 1)
    key("ArrowUp", 10); key("ArrowLeft")
    check("all off again", ext() == 0)
    key("ArrowDown", 11); key("ArrowRight")  # row 12: Exact perspective
    check("Right toggles Exact perspective", ext() == 4)
    key("ArrowLeft")
    check("...and back off", ext() == 0)
    key("Escape")
    check("Esc returns to Options", scr() == OPTIONS)
    key("Enter")
    check("...on the Classic / 2026 row", scr() == EXTRAS)
    key("Escape"); key("Escape"); key("Escape")
    check("Esc Esc Esc closes the menu", vis() == 0)
    key("Backquote")
    for line in ["wasm_uncapped 1", "wasm_showfps 1"]:
        pg.keyboard.type(line); key("Enter")
    check("the wasm_* console variables set the same settings", ext() == 3)
    # Key_Console's history and Tab, through the page's keys: Up Up brings
    # back "wasm_uncapped 1" (Backspace + 0 turns it off); Tab completes a
    # cvar name ("wasm_ex" -> "wasm_exactpersp ").
    key("ArrowUp", 2); key("Backspace"); pg.keyboard.type("0"); key("Enter")
    check("Up walks the console history", ext() == 2)
    pg.keyboard.type("wasm_ex"); key("Tab"); pg.keyboard.type("1"); key("Enter")
    check("Tab completes the cvar name", ext() == 6)
    pg.keyboard.type("wasm_exactpersp 0"); key("Enter")
    key("Backquote")
    check("wasm_exactpersp 0", ext() == 2)
    frames(pg)
    check("config.cfg keeps the change, and only what differs from Classic",
          cfg_has(pg, 'profile "classic"', 'wasm_showfps "1"')
          and "wasm_uncapped" not in (pg.evaluate(CFG) or ""), str(pg.evaluate(CFG)))

    # 2. wasm_uncapped through the real program, with the page's own ticks
    #    paused so they cannot interleave (the calls run back to back).
    capped, uncapped = pg.evaluate("""async () => {
        quake.pause();
        const one = async () => (await Promise.all(Array.from({ length: 144 }, () => exp.step(1 / 144))))
            .reduce((a, b) => a + b, 0);
        const bits = await exp.extras();
        exp.set_extras(bits & ~1); const a = await one();
        exp.set_extras(bits | 1);  const b = await one();
        await exp.set_extras(bits);
        quake.resume();
        return [a, b];
    }""")
    check("144 Hz with id's cap: 72 frames a second", 71 <= capped <= 73, str(capped))
    check("144 Hz with wasm_uncapped: 144", uncapped == 144, str(uncapped))

    # 3. wasm_showfps in the walk: freeze the frames, then only the readout
    #    may change the canvas.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")                      # close the boot menu
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
    time.sleep(1.5)                    # a full measuring window
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_extras_fps.png"))
    pg.evaluate("quake.pause()")
    pg.evaluate(FROZEN)
    pg.evaluate(GRAB, "_on")
    pg.evaluate("exp.set_extras(0)")
    pg.evaluate(FROZEN)
    pg.evaluate(GRAB, "_off")
    w, h = pg.evaluate("Promise.all([exp.width(), exp.height()])")
    d = pg.evaluate(DIFF, ["_on", "_off"])
    # The 2-D layer is 1:1 as id draws it (the scaled-2-D extra is off):
    # " 60 FPS" at x w-64..w-8, y h-56..h-48 (viewsize 100: sb_lines 48).
    box = (w - 64, w - 8, h - 56, h - 48)
    inside = d is not None and d["x0"] >= box[0] and d["x1"] < box[1] \
        and d["y0"] >= box[2] and d["y1"] < box[3]
    check("the readout draws bottom right, above the status bar, and nowhere else",
          inside, f"{d}; box {box}")
    pg.evaluate("exp.set_extras(2)")
    pg.evaluate(FROZEN)
    pg.evaluate(GRAB, "_on2")
    check("on again: the same readout", pg.evaluate(DIFF, ["_on", "_on2"]) is None)
    # wasm_exactpersp: the walls change, and off is id's frame again.
    pg.evaluate("exp.set_extras(4)")
    pg.evaluate(FROZEN)
    pg.evaluate(GRAB, "_exact")
    d = pg.evaluate(DIFF, ["_off", "_exact"])
    check("wasm_exactpersp redraws the walls", d is not None and d["n"] > 1000, str(d))
    pg.evaluate("exp.set_extras(0)")
    pg.evaluate(FROZEN)
    pg.evaluate(GRAB, "_off2")
    check("...and off is id's spans again, byte for byte",
          pg.evaluate(DIFF, ["_off", "_off2"]) is None)
    pg.evaluate("exp.set_extras(2)")
    pg.evaluate("quake.resume()")

    # 4. Persistence across a plain reload (no ?classic): the profile,
    #    viewsize, the settings, the resolution.
    key("Minus", 2)                    # default.cfg: '-' is sizedown -> 80
    frames(pg)
    check("viewsize and the settings kept in config.cfg",
          cfg_has(pg, 'viewsize "80"', 'wasm_showfps "1"'), str(pg.evaluate(CFG)))
    pg.evaluate("exp.set_extras(3)")
    frames(pg)
    cfg_has(pg, 'wasm_uncapped "1"', 'wasm_showfps "1"')
    res0 = pg.evaluate("Promise.all([exp.width(), exp.height()])")
    boot_page(pg, "")
    check("a plain reload comes back Classic", prof() == "classic", prof())
    check("the settings survive a reload", ext() == 3, str(ext()))
    check("Screen size (viewsize) survives a reload", pg.evaluate("exp.viewsize()") == 80)
    check("...and the resolution still does",
          pg.evaluate("Promise.all([exp.width(), exp.height()])") == res0)
    pg.evaluate("exp.set_extras(0); exp.set_viewsize(100)")
    frames(pg)

    # 5. Esc in fullscreen with Escape keyboard-locked (vid_fkey: the page's
    #    F, on in 2026, off in Classic as id's).
    pg.evaluate("quake.callLine('exec vid_fkey 1')")
    pg.wait_for_function("quake.state.flags & 64", timeout=5000)
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
    key("f")
    has_kb = pg.evaluate("!!(navigator.keyboard && navigator.keyboard.lock)")
    try:
        pg.wait_for_function("!!document.fullscreenElement && escLocked", timeout=5000)
        fs = True
    except Exception:
        fs = False
    if has_kb:
        check("F enters fullscreen and locks Esc", fs,
              str(pg.evaluate("[!!document.fullscreenElement, escLocked, window.__kb]")))
    else:
        print("SKIP keyboard-lock checks (this browser has no Keyboard Lock API; "
              "the fallback below is its flow)")
        if pg.evaluate("!!document.fullscreenElement"):
            key("f")
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
        pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
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
        pg.wait_for_function("exp.menu_visible().then(v => v === 1)", timeout=5000)
        key("Escape")
        check("windowed: lock loss opens, the next Esc closes", vis() == 0)
    elif has_kb:
        print("SKIP keyboard-lock checks (headless refused fullscreen)")

    # Without the Keyboard Lock API (Firefox, Safari): the old hint and flow.
    ctx2 = br.new_context(viewport={"width": 820, "height": 560})
    ctx2.add_init_script(NO_KB)
    pg2 = ctx2.new_page()
    pg2.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg2.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg2, "")   # the default, 2026: F is the fullscreen key
    pg2.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg2.keyboard.press("Escape")
    # The page decides what F does from the game's state as the program last
    # reported it: the Escape's own turn, a millisecond or two later (no
    # player presses Esc and F closer than that; a script does).
    time.sleep(0.1)
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
