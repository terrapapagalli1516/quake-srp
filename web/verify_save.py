#!/usr/bin/env -S uv run --with playwright --script
"""Savegame round-trip in a real browser: drive the drop-down console to
`save` (the .sav text lands under a per-name localStorage key), keep playing,
`load` (the world snaps back to the saved spot — pixel evidence), then RELOAD
the page and `load` again (persistence across sessions), and check the
reloaded page's Load menu lists the slot saved in the first session and loads
it on Enter (id rescans the saves whenever Load/Save opens, M_ScanSaves).

Usage: verify_save.py [webdir]   (defaults to this script's directory; pass a
temp dir holding index.html + a freshly built quake_wasm.wasm to test changes
without touching the deployed wasm)."""
import functools, http.server, os, socketserver, sys, threading, time
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("QUAKE_VERIFY_PORT", "8163"))
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

KEY = "quake-rs.sav.evidence.sav"
SLOT = 0
SLOT_KEY = f"quake-rs.sav.s{SLOT}.sav"
fails = []

GRAB = """() => {
    const c = document.getElementById('c');
    const g = c.getContext('2d'); if (!g) return null;
    return Array.from(g.getImageData(0,0,c.width,c.height).data);
}"""


def diff_frac(a, b):
    """Fraction of pixels whose R or G channel differs by more than 12."""
    npx = min(len(a), len(b)) // 4
    moved = sum(
        1
        for i in range(0, npx * 4, 4)
        if abs(a[i] - b[i]) > 12 or abs(a[i + 1] - b[i + 1]) > 12
    )
    return moved / max(1, npx)


def wait_ready(pg):
    # `exp` is a top-level `let` binding; the predicate must end in a BOOLEAN
    # (a function-valued string predicate would be *called* by the waiter).
    pg.wait_for_function(
        "typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'",
        timeout=120000,
    )
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('ready')",
        timeout=30000,
    )


def boot_walk(pg):
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")  # boot lands in the menu; close it
    pg.wait_for_function("!exp.menu_visible || !exp.menu_visible()", timeout=5000)
    time.sleep(0.3)


def console_line(pg, line):
    """Open the drop-down console, type `line`, submit it."""
    pg.keyboard.press("Backquote")
    pg.wait_for_function("exp.console_visible && exp.console_visible() === 1", timeout=5000)
    pg.keyboard.type(line, delay=15)
    pg.keyboard.press("Enter")


with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=["--no-sandbox"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    wait_ready(pg)
    pg.evaluate(f"localStorage.removeItem('{KEY}')")
    pg.evaluate("for (let i = 0; i < 12; i++) localStorage.removeItem('quake-rs.sav.s' + i + '.sav')")

    boot_walk(pg)

    # Walk ~2 s so the saved spot is visibly distinct from the spawn.
    pg.keyboard.down("w")
    time.sleep(2.0)
    pg.keyboard.up("w")
    time.sleep(0.4)
    f_saved = pg.evaluate(GRAB)

    # SAVE through the real console; the page persists it on the next frame.
    console_line(pg, "save evidence")
    pg.wait_for_function(f"localStorage.getItem('{KEY}') !== null", timeout=5000)
    # ...and into menu slot SLOT (the Load menu's sN.sav), the same instant.
    pg.wait_for_function("exp.console_visible && exp.console_visible() === 1", timeout=5000)
    pg.keyboard.type(f"save s{SLOT}", delay=15)
    pg.keyboard.press("Enter")
    pg.wait_for_function(f"localStorage.getItem('{SLOT_KEY}') !== null", timeout=5000)
    pg.keyboard.press("Backquote")  # close the console again
    size = pg.evaluate(f"(localStorage.getItem('{KEY}')||'').length")
    head = pg.evaluate(f"(localStorage.getItem('{KEY}')||'').slice(0, 2)")
    print(f"localStorage['{KEY}'] size:", size)
    if not (20000 < size < 2_000_000):
        fails.append(f"implausible .sav size {size}")
    if head != "5\n":
        fails.append(f".sav text does not start with the version-5 header: {head!r}")
    else:
        print("PASS save key appears with a plausible version-5 .sav")

    # Keep playing: walk further so the live world diverges from the save.
    time.sleep(0.2)
    pg.keyboard.down("w")
    time.sleep(2.5)
    pg.keyboard.up("w")
    time.sleep(0.4)
    f_diverged = pg.evaluate(GRAB)
    d_walked = diff_frac(f_saved, f_diverged)
    print(f"pixels changed by post-save play: {d_walked:.1%}")
    if d_walked < 0.30:
        fails.append(f"the post-save walk barely changed the view ({d_walked:.1%})")

    # LOAD through the console; on success load_game() closes the console.
    console_line(pg, "load evidence")
    pg.wait_for_function("exp.console_visible && exp.console_visible() === 0", timeout=5000)
    time.sleep(0.5)
    f_loaded = pg.evaluate(GRAB)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_save.png"))
    d_restore = diff_frac(f_loaded, f_saved)
    d_not = diff_frac(f_loaded, f_diverged)
    print(f"loaded frame vs saved instant: {d_restore:.1%}; vs diverged play: {d_not:.1%}")
    if not (d_restore < 0.25 and d_restore < d_not):
        fails.append(
            f"load did not restore the saved view (vs saved {d_restore:.1%}, vs diverged {d_not:.1%})"
        )
    else:
        print("PASS load snaps the view back to the saved instant")

    # PAGE RELOAD: the save must survive a whole new session and load again.
    pg.reload(wait_until="load")
    wait_ready(pg)
    still = pg.evaluate(f"localStorage.getItem('{KEY}') !== null")
    if not still:
        fails.append("the save did not survive the page reload")
    boot_walk(pg)
    console_line(pg, "load evidence")
    pg.wait_for_function("exp.console_visible && exp.console_visible() === 0", timeout=5000)
    time.sleep(0.5)
    f_reloaded = pg.evaluate(GRAB)
    d_again = diff_frac(f_reloaded, f_saved)
    print(f"post-page-reload loaded frame vs saved instant: {d_again:.1%}")
    if d_again >= 0.25:
        fails.append(f"reload+load did not restore the saved view ({d_again:.1%})")
    else:
        print("PASS the save survives a page reload and loads again")

    # THE LOAD MENU after that reload: id rescans the saves each time Load
    # or Save opens (M_ScanSaves in M_Menu_Load_f / M_Menu_Save_f). Forget
    # what the page listed at boot, open Load: it must list slot SLOT again
    # (its row is not "--- UNUSED SLOT ---") and load it on Enter (M_Load_Key
    # refuses a slot that is not loadable). SLOT is 0, where the cursor
    # starts, so no cursor keys are involved.
    LOAD = 2
    scr = lambda: pg.evaluate("exp.menu_screen_id()")
    forget = f"exp.sav_alloc(0); exp.menu_set_save_comment({SLOT})"
    pg.evaluate(forget)
    pg.keyboard.press("Escape")  # the menu, over the loaded game (paused)
    pg.wait_for_function("exp.menu_visible() === 1", timeout=5000)
    for k in ["Enter", "ArrowDown", "Enter"]:  # Single Player > Load
        pg.keyboard.press(k)
        time.sleep(0.15)
    if scr() != LOAD:
        fails.append(f"could not open the Load menu (screen {scr()})")
    time.sleep(0.4)
    listed = pg.evaluate(GRAB)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_save_loadmenu.png"))
    # The same screen with the slot forgotten again: its row must change. The
    # world is paused behind the menu; only the cursor (left of the text) blinks.
    pg.evaluate(forget)
    time.sleep(0.4)
    unlisted = pg.evaluate(GRAB)
    w = pg.evaluate("exp.width()")
    ox = (w - 320) // 2  # the 2-D layer at its own pixel size, top centre

    def row_band(img, row):
        y0 = 32 + 8 * row
        return [img[(y * w + x) * 4] for y in range(y0, y0 + 8) for x in range(ox + 16, ox + 16 + 8 * 39)]

    changed = row_band(listed, SLOT) != row_band(unlisted, SLOT)
    others_same = row_band(listed, SLOT + 1) == row_band(unlisted, SLOT + 1)
    print(f"Load menu row {SLOT}: listed != unused: {changed}; row {SLOT + 1} unchanged: {others_same}")
    if not (changed and others_same):
        fails.append("opening Load did not list the saved slot (no M_ScanSaves on entry)")
    # Leave and reopen Load (another rescan), then Enter on the slot. Single
    # Player reopens on Load (m_singleplayer_cursor is kept, menu.c); a port
    # whose cursor resets lands on New Game, whose "Are you sure?" N answers.
    pg.keyboard.press("Escape")
    time.sleep(0.15)
    pg.keyboard.press("Enter")
    time.sleep(0.3)
    if scr() != LOAD:
        for k in ["n", "ArrowDown", "Enter"]:
            pg.keyboard.press(k)
            time.sleep(0.15)
    if scr() != LOAD:
        fails.append(f"could not reopen the Load menu (screen {scr()})")
    time.sleep(0.3)
    pg.keyboard.press("Enter")
    time.sleep(0.8)
    if pg.evaluate("exp.menu_visible()") != 0:
        fails.append("Enter on the reopened Load menu's slot did not load it (not loadable)")
    else:
        d_slot = diff_frac(pg.evaluate(GRAB), f_saved)
        print(f"Load menu slot {SLOT} loaded frame vs saved instant: {d_slot:.1%}")
        if d_slot >= 0.25:
            fails.append(f"loading slot {SLOT} from the menu did not restore the saved view ({d_slot:.1%})")
        else:
            print("PASS a reloaded page's Load menu lists the saved slot and loads it")

    print("errors:", errs[-5:])
    if errs:
        fails.append(f"console errors: {errs[-5:]}")
    br.close()
httpd.shutdown()

if fails:
    print("FAIL:", "; ".join(fails))
    raise SystemExit(1)
print("done: save/load verified (key persisted, view restored, survives reload, Load menu lists it)")
