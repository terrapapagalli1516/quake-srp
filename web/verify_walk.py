#!/usr/bin/env -S uv run --with playwright --script
"""Boot walk mode (live server), close the menu, WALK, and prove the camera moved.

Usage: verify_walk.py [webdir]   (defaults to this script's directory; pass a
deploy dir — PLATFORM.md: index.html, wasi.js, a freshly built quake.wasm and
id1/pak0.pak — to test changes without touching the deployed page)."""
import os, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8161)
httpd = isolated.serve(WEB, PORT)

fails = []

with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    # The game runs in a worker; the page's `quake.ready` is up once its
    # startup ended (and main() wrote its 'ready' status, which would
    # otherwise race the walk button's), so the run is ordered.
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    # Boot walk. boot() lands in the main menu (faithful boot experience), so
    # close it with Escape before walking — the menu gates gameplay input.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    # `exp.name()` asks the program (a Promise: answered between two frames).
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)
    time.sleep(0.3)

    grab = """() => {
        const c = document.getElementById('c');
        return Array.from(quake.readback());
    }"""
    before = pg.evaluate(grab)
    # Walk forward ~3 s of live-server frames.
    pg.keyboard.down("w")
    time.sleep(3.0)
    pg.keyboard.up("w")
    time.sleep(0.3)
    after = pg.evaluate(grab)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_walk.png"))

    # 1. The framebuffer is drawn (not all background).
    nonbg = sum(
        1
        for i in range(0, len(after), 4)
        if after[i] > 25 or after[i + 1] > 25 or after[i + 2] > 25
    )
    print("non-background pixels:", nonbg)
    if nonbg < 50000:
        fails.append(f"frame mostly background ({nonbg} px)")

    # 2. The camera actually MOVED: a large fraction of pixels must differ
    #    between the pre-walk and post-walk frames (a static spawn with the
    #    menu up — the old vacuous mode — changes almost nothing).
    npx = min(len(before), len(after)) // 4
    moved = sum(
        1
        for i in range(0, npx * 4, 4)
        if abs(before[i] - after[i]) > 12 or abs(before[i + 1] - after[i + 1]) > 12
    )
    frac = moved / max(1, npx)
    print(f"pixels changed by walking: {moved}/{npx} ({frac:.1%})")
    if frac < 0.30:
        fails.append(f"camera did not move (only {frac:.1%} of pixels changed)")

    # 3. The menu is really down and the walk is live.
    menu_up = pg.evaluate("exp.menu_visible()")
    if menu_up:
        fails.append("menu still up after Escape")
    print("status:", pg.eval_on_selector("#status", "e=>e.textContent"))
    print("errors:", errs[-5:])
    if errs:
        fails.append(f"console errors: {errs[-5:]}")
    br.close()
httpd.shutdown()

if fails:
    print("FAIL:", "; ".join(fails))
    raise SystemExit(1)
print("done: walk verified (drawn, menu closed, camera moved, no errors)")
