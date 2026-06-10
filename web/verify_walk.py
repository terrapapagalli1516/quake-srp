#!/usr/bin/env -S uv run --with playwright --script
"""Boot walk mode (live server), close the menu, WALK, and prove the camera moved.

Usage: verify_walk.py [webdir]   (defaults to this script's directory; pass a
temp dir holding index.html + a freshly built quake_wasm.wasm to test changes
without touching the deployed wasm)."""
import functools, http.server, os, socketserver, sys, threading, time
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = 8161
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

fails = []

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=["--no-sandbox"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    # NB: `exp` is a top-level `let` (a global *binding*, not a window
    # property), so probe it as a bare identifier — and the predicate must end
    # in a BOOLEAN: a string predicate that evaluates to a function value makes
    # Playwright *call* it (it would silently boot() the game from the waiter).
    pg.wait_for_function(
        "typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'",
        timeout=120000,
    )
    # ...and for main() to finish its boot sequence (otherwise its final
    # 'ready' status write races the walk button's), so the run is ordered.
    pg.wait_for_function(
        "document.getElementById('status').textContent.includes('ready')",
        timeout=30000,
    )
    # Boot walk. boot() lands in the main menu (faithful boot experience), so
    # close it with Escape before walking — the menu gates gameplay input.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    pg.wait_for_function("!exp.menu_visible || !exp.menu_visible()", timeout=5000)
    time.sleep(0.3)

    grab = """() => {
        const c = document.getElementById('c');
        const g = c.getContext('2d'); if (!g) return null;
        return Array.from(g.getImageData(0,0,c.width,c.height).data);
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
    menu_up = pg.evaluate("exp.menu_visible ? exp.menu_visible() : 0")
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
