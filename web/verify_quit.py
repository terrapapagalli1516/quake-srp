#!/usr/bin/env -S uv run --with playwright --script
"""Verify Quit: fullscreen exit, pointer-lock release, id's end screen, and
restarting into a running game, end-to-end in headless Chromium
(web/PLATFORM.md, "Quit").

  1. Fullscreen (2026's F key, a real user gesture to headless Chromium —
     Playwright's key press counts) and the pointer lock (a canvas click),
     both acquired while walking.
  2. Menu > Quit > Y (menu_up wraps to Main's last item, Quit; menu_select
     raises the confirm prompt; menu_quit_yes answers it): fullscreen and
     the pointer lock are BOTH released without the player touching Esc,
     and id's end screen (end1.bin: this deploy's pak is the unregistered
     shareware one) shows over the whole page, drawn as a VGA text-mode
     screen, not the plain-text fallback.
  3. Dismissing it (a click) reloads the page into a running game again —
     `quake.ready` and the attract demo, as a fresh boot.
  4. The console's `quit` (key_dest == key_console) quits at once, no
     confirmation: the end screen comes up with no Y press at all.

Usage: verify_quit.py [webdir]   (defaults to this script's directory; pass
a deploy dir — PLATFORM.md.)
"""
import os, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8524)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    pg = br.new_page(viewport={"width": 900, "height": 560})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)

    # Boot walk (2026 default: F is bound to fullscreen, vid_fkey on), close
    # the menu, and capture fullscreen + the pointer.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=12000)
    pg.keyboard.press("f")
    try:
        pg.wait_for_function("!!document.fullscreenElement", timeout=12000)
        got_fullscreen = True
    except Exception:
        got_fullscreen = False
    check("F enters fullscreen", got_fullscreen,
          "(a user gesture to headless Chromium; a real fail here, not a flake)")
    pg.locator("#c").click()
    try:
        pg.wait_for_function(
            "document.pointerLockElement === document.getElementById('c')", timeout=12000)
        got_lock = True
    except Exception:
        got_lock = False
    check("the canvas click captures the pointer", got_lock)

    # Menu > Quit > Y, through the automation calls (deterministic: no
    # reliance on Main's cursor position in the DOM or a screenshot, and no
    # real DOM Escape — a physical Esc while fullscreen AND pointer-locked
    # races the browser's own reserved-Escape/lock-loss handling, already
    # covered by verify_extras.py; menu_cancel reaches the same Key_Event as
    # Escape inside the engine without any of that).
    pg.evaluate("exp.menu_cancel()")       # the menu, over the still-fullscreen, still-locked game
    pg.wait_for_function("exp.menu_visible().then(v => !!v)", timeout=12000)
    pg.evaluate("exp.menu_up()")           # Main's last item is Quit
    pg.evaluate("exp.menu_select()")       # raises the confirm prompt
    # A generous timeout: each automation call is its own round trip through
    # the worker, and timings are noisy (other programs may load the CPU).
    pg.wait_for_function("quake.state.flags & 256", timeout=15000)   # ST.ASK
    pg.evaluate("exp.menu_quit_yes()")

    try:
        pg.wait_for_function("!document.fullscreenElement", timeout=12000)
        left_fullscreen = True
    except Exception:
        left_fullscreen = False
    check("quitting leaves fullscreen", left_fullscreen)
    try:
        pg.wait_for_function("!document.pointerLockElement", timeout=12000)
        released_lock = True
    except Exception:
        released_lock = False
    check("...and releases the pointer lock", released_lock)

    try:
        pg.wait_for_selector("#quakeEndScreen", timeout=12000)
        shown = True
    except Exception:
        shown = False
    check("the end screen overlay appears", shown)
    if shown:
        label = pg.eval_on_selector("#quakeEndScreen", "e => e.getAttribute('aria-label')")
        check("it says shareware (this deploy's pak, no pak1.pak)",
              "shareware" in label, label)
        has_grid = pg.eval_on_selector("#quakeEndScreen", "e => !!e.querySelector('.qesGrid')")
        check("id's end1.bin drew (not the no-screen fallback)", has_grid)
        # Row 0's red strip carries the DOS build's version, patched in by
        # `sys::end_screen` exactly as `Sys_Quit` did.
        row0 = pg.eval_on_selector(
            "#quakeEndScreen .qesGrid", "e => e.children[0].textContent")
        check('row 0 carries " v1.09"', " v1.09" in row0, repr(row0))
        full_text = pg.eval_on_selector("#quakeEndScreen .qesGrid", "e => e.textContent")
        check("CP437's box-drawing lines decoded (not raw bytes or mojibake)",
              "─" in full_text or "═" in full_text, repr(full_text[:40]))
        pg.screenshot(path=os.path.join(WEB, "verify_quit_endscreen.png"))

    # A click anywhere on it reloads the page into a running game again.
    pg.locator("#quakeEndScreen").click()
    pg.wait_for_load_state("load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    check("dismissing restarts: the page is ready again",
          pg.evaluate("quake.ready") is True)
    check("...a fresh page, no end-screen overlay carried over",
          not pg.evaluate("!!document.getElementById('quakeEndScreen')"))

    # The console's `quit`: immediate, no Y/N prompt (Host_Quit_f's
    # key_dest == key_console branch).
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=12000)
    pg.evaluate("quake.callLine('console_toggle')")
    pg.evaluate("quake.callLine('exec quit')")
    try:
        pg.wait_for_selector("#quakeEndScreen", timeout=12000)
        console_quit_shown = True
    except Exception:
        console_quit_shown = False
    check("the console's `quit` skips the prompt and quits at once", console_quit_shown)

    print("status:", pg.eval_on_selector("#status", "e=>e.textContent") if pg.query_selector("#status") else "?")
    print("errors:", errs[-5:])
    if errs:
        failed += 1
        print("FAIL console/page errors:", errs[-5:])
    br.close()
httpd.shutdown()

print(f"{passed} passed, {failed} failed")
if failed:
    raise SystemExit(1)
print("done: quit verified (fullscreen + pointer lock released, end screen, restart, console quit)")
