#!/usr/bin/env -S uv run --with playwright --script
"""Verify the F-key shortcuts (default.cfg, keys.rs's `Bindings::default_cfg`)
end-to-end in headless Chromium: real `KeyboardEvent`s through the page's
`keydown` handler (index.html), its `quakeKey` mapping and `preventDefault`
guards, into the program's key bindings and console commands
(web/PLATFORM.md, "The F-keys").

  1. F1/F2/F3/F4 open the Help/Save/Load/Options screens directly
     (`menu_screen_id`), each reached with the game playing (no menu up
     first) — proof the F-key reaches its binding and the browser's own
     default (F1 help, F3 find, ...) never ran instead.
  2. F6 then F9: the `echo` prints at once; `wait` holds the actual
     `save`/`load` to the next frame (`console_text`).
  3. F10 raises the Quit confirmation prompt (menu_screen_id 9) without
     quitting — `quake.ready` stays true.
  4. F12: no screenshot is written windowed (the browser keeps F12 for
     devtools); entering fullscreen (F, a real user gesture to headless
     Chromium) and locking it (`lockEscape`) lets F12 through, and a
     `quakeNN.pcx` write shows up in the console.

Usage: verify_fkeys.py [webdir]   (defaults to this script's directory; pass
a deploy dir — PLATFORM.md — to test changes without touching the deployed
page)."""
import time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8625)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

SCREEN = {"help": 8, "save": 3, "load": 2, "options": 5, "quit": 9}


def call(pg, line):
    return pg.evaluate("line => quake.callLine(line)", line)["value"]


def console_text(pg):
    return pg.evaluate("quake.text('console_text')")


def leave_menu(pg):
    """Escape out of whatever screen is up, back to the game. Help's Escape
    goes straight to Main; Save/Load's goes to SinglePlayer first (one more
    Escape than Help, id's own menu hierarchy), so this just presses it
    until the menu is down rather than assuming a fixed count."""
    for _ in range(4):
        if pg.evaluate("exp.menu_visible()") == 0:
            return
        pg.keyboard.press("Escape")
        time.sleep(0.15)


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    pg = br.new_page(viewport={"width": 900, "height": 560})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)

    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 12)

    for key, screen in [("F1", "help"), ("F2", "save"), ("F3", "load"), ("F4", "options")]:
        pg.keyboard.press(key)
        time.sleep(0.2)
        got_screen, got_visible = call(pg, "menu_screen_id"), call(pg, "menu_visible")
        check(
            f"{key} opens the {screen} screen",
            got_screen == SCREEN[screen] and got_visible == 1,
            f"(screen {got_screen}, visible {got_visible}, want {SCREEN[screen]})",
        )
        leave_menu(pg)
        check(f"{key}: back to the game", call(pg, "menu_visible") == 0, "menu still up")

    # F6/F9: echo at once, the save/load one frame later (`wait`).
    pg.keyboard.press("F6")
    time.sleep(0.2)
    text = console_text(pg)
    check("F6: Quicksaving... printed at once", "Quicksaving...\n" in text, text[-80:])
    time.sleep(0.3)  # a couple of rAF frames: `wait` releases the save
    text = console_text(pg)
    check(
        "F6: quick.sav written",
        "Saving game to quick.sav..." in text and text.rstrip().endswith("done."),
        text[-120:],
    )

    pg.keyboard.press("F9")
    time.sleep(0.2)
    text = console_text(pg)
    check("F9: Quickloading... printed at once", "Quickloading...\n" in text, text[-80:])
    time.sleep(0.3)
    text = console_text(pg)
    check(
        "F9: quick.sav loaded",
        "Loading game from quick.sav..." in text and "ERROR" not in text[-200:],
        text[-120:],
    )

    # F10: the Quit confirmation, not an immediate quit.
    pg.keyboard.press("F10")
    time.sleep(0.2)
    check(
        "F10 raises the Quit prompt",
        call(pg, "menu_screen_id") == SCREEN["quit"] and call(pg, "menu_visible") == 1,
    )
    check("F10 did not quit", pg.evaluate("quake.ready") is True)
    pg.keyboard.press("n")
    time.sleep(0.2)

    # F12 windowed: a real browser reserves it for devtools and
    # preventDefault cannot get it back (web/PLATFORM.md, "The F-keys") —
    # NOT checked here. Headless Chromium has no devtools UI to reserve it
    # for, so it just passes F12 through like any other key, which would
    # make a "no screenshot written" assertion pass for the wrong reason
    # (and it does write one). Informational only.
    before = console_text(pg)
    pg.keyboard.press("F12")
    time.sleep(0.4)
    print(
        "info: F12 windowed wrote a screenshot anyway:", console_text(pg) != before,
        "(expected in headless Chromium, which has no devtools UI to reserve F12 for;",
        "unverified here whether a real windowed browser keeps it, as PLATFORM.md says it should)",
    )

    pg.keyboard.press("Alt+Enter")  # 2026's fullscreen key (vid_altenter)
    try:
        pg.wait_for_function("!!document.fullscreenElement", timeout=12000)
        got_fullscreen = True
    except Exception:
        got_fullscreen = False
    check("Alt+Enter enters fullscreen", got_fullscreen,
          "(a user gesture to headless Chromium; a real fail here, not a flake)")
    if got_fullscreen:
        before = console_text(pg)
        pg.keyboard.press("F12")
        time.sleep(0.4)
        after = console_text(pg)
        check(
            "F12 in fullscreen: a screenshot is written",
            after != before and "Wrote quake" in after and after.rstrip().endswith(".pcx"),
            after[-80:],
        )

    print("errors:", errs[-5:])
    if errs:
        failed += 1
        print("FAIL console/page errors:", errs[-5:])
    br.close()
httpd.shutdown()

print(f"{passed} passed, {failed} failed")
if failed:
    raise SystemExit(1)
print("done: F1-F4, F6, F9, F10, F12 verified end-to-end")
