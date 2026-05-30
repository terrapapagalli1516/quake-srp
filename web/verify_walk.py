#!/usr/bin/env -S uv run --with playwright --script
"""Boot walk mode (live server), tick frames, confirm no crash + a drawn view."""
import functools, http.server, os, socketserver, threading, time
from playwright.sync_api import sync_playwright

WEB = "web"
PORT = 8161
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=["--no-sandbox"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    try:
        pg.wait_for_function("window.exp && exp.boot", timeout=40000)
    except Exception as e:
        print("no exports:", e)
    # Boot walk + let the live server tick ~3s of frames while walking forward.
    pg.evaluate("document.getElementById('walkBtn').click()")
    box = pg.locator("#c").bounding_box()
    cx, cy = box["x"] + box["width"]/2, box["y"] + box["height"]/2
    pg.keyboard.down("w")
    time.sleep(3.0)
    pg.keyboard.up("w")
    time.sleep(0.3)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_walk.png"))
    # Did the framebuffer get drawn (not all background)? sample via canvas data.
    nonbg = pg.evaluate("""() => {
        const c = document.getElementById('c');
        const g = c.getContext('2d'); if (!g) return -1;
        const d = g.getImageData(0,0,c.width,c.height).data;
        let n=0; for (let i=0;i<d.length;i+=4){ if (d[i]>25||d[i+1]>25||d[i+2]>25) n++; }
        return n;
    }""")
    print("SHOT_OK; non-background pixels:", nonbg)
    print("status:", pg.eval_on_selector("#status", "e=>e.textContent"))
    print("errors:", errs[-5:])
    br.close()
httpd.shutdown()
print("done")
