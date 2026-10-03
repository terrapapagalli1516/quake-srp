#!/usr/bin/env -S uv run --with playwright --script
"""The canvas shows the program's frame, byte for byte, however it gets there.

The program draws 8-bit frames and the page is the display's DAC (web/
PLATFORM.md, "A frame"): with WebGL2 it takes each frame as palette indices
plus the frame's 256 colours and looks the pixels up on the GPU; with a 2-D
canvas (no WebGL2, or `?canvas2d`) it takes the program's RGBA. For frames in
the states where the palette or the picture is not the plain one — the
attract demo (Classic), the live walk, the Quad's cshift, underwater (the
warp and the water's cshift), the menu's fade, the console, a gamma — this
reads the canvas back (`quake.readback()`: WebGL2's readPixels, or the 2-D
canvas's getImageData) and compares its FNV-1a hash with the program's own
RGBA for the same frame (the `frame_hash` call: what the 2-D path puts, and
what `quaketool play --hash-every` hashes natively). It runs the page as it
is (WebGL2 on a GPU), with `?webgl` (WebGL2 even drawn by the CPU, as
headless Chromium's is without $QUAKE_GPU) and with `?canvas2d`; on WebGL2
it also sends the frames through the staging copy, and loses the context and
restores it.

A headless Firefox has WebGL2 only with a display to ask (`DISPLAY` set, the
GPU behind it; with none the page takes the 2-D canvas and the WebGL2 parts
are skipped): run it both ways (PLATFORM.md, "Build, serve, deploy").

Usage: verify_present.py [webdir]   ($QUAKE_BROWSER, $QUAKE_GPU: see isolated.py)"""
import time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8165)
httpd = isolated.serve(WEB, PORT)

# One frame, frozen (dt 0 always renders), presented as the page presents;
# then the canvas's hash and the program's for that frame. `steps` real
# frames first let the state settle (a cshift arrives, the console slides).
FRAME = """async ([steps, dt]) => {
    quake.pause();
    await new Promise(r => setTimeout(r, 50));
    for (let i = 0; i < steps; i++) quake.tick(dt);
    const t = quake.tick(0);
    return { presented: t.presented, page: quake.frameHash(), prog: await quake.text('frame_hash'),
             size: quake.size() };
}"""

# The states, each (name, setup calls, settle steps, dt).
STATES = [
    ("attract demo", [], 0, 0),
    ("live walk", ["boot", "menu_cancel"], 3, 1 / 72),
    ("quad cshift", ["exec impulse 255"], 10, 1 / 72),
    ("underwater", ["setpos 750 898 -354"], 3, 1 / 72),
    ("menu fade", ["key_event 27 1 0", "key_event 27 0 0"], 1, 1 / 72),
    ("console", ["key_event 27 1 0", "key_event 27 0 0", "console_toggle"], 20, 0.05),
    ("gamma 0.7", ["console_toggle", "exec gamma 0.7"], 3, 1 / 72),
]

fails = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""))
    if not ok:
        fails.append(name)


def run(pg, query):
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html{query}", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    pg.wait_for_function("quake.firstFrameAt > 0", timeout=60000)
    time.sleep(0.3)
    info = pg.evaluate("quake.presenter()")
    print(f"-- page{query or ''}: {info}")
    if query == "?canvas2d":
        check("?canvas2d presents through the 2-D canvas", not info["gl"] and info["format"] == 0)
    if query == "?webgl":
        webgl2 = pg.evaluate("() => !!document.createElement('canvas').getContext('webgl2')")
        check("?webgl presents through WebGL2 where there is one", info["gl"] == webgl2 and info["format"] == int(webgl2))
    for name, calls, steps, dt in STATES:
        for c in calls:
            pg.evaluate("line => quake.callLine(line)", c)
        r = pg.evaluate(FRAME, [steps, dt])
        check(f"{name}: canvas = program's RGBA", r["presented"] and r["page"] == r["prog"],
              f"{r['size'][0]}x{r['size'][1]} canvas {r['page']} program {r['prog']}")
    if info["gl"]:
        # A WebGL that refuses views on shared memory (Firefox's): the frame
        # goes up through a copy, the same pixels.
        pg.evaluate("() => { presenter.sharedOk = false; }")
        r = pg.evaluate(FRAME, [2, 1 / 72])
        check("through the staging copy: canvas = program's RGBA", r["presented"] and r["page"] == r["prog"],
              f"canvas {r['page']} program {r['prog']}")
        # A lost WebGL context (a GPU reset): nothing breaks, and once it is
        # restored the frames are the program's again.
        pg.evaluate("""() => {
            const ext = document.getElementById('c').getContext('webgl2').getExtension('WEBGL_lose_context');
            ext.loseContext();
            window.__restoreGl = () => ext.restoreContext();
        }""")
        time.sleep(0.3)
        pg.evaluate("() => { quake.resume(); }")
        time.sleep(0.3)
        pg.evaluate("() => window.__restoreGl()")
        time.sleep(0.5)
        r = pg.evaluate(FRAME, [2, 1 / 72])
        check("after a lost and restored context: canvas = program's RGBA", r["presented"] and r["page"] == r["prog"],
              f"canvas {r['page']} program {r['prog']}")
    check(f"no page errors{query}", not errs, "; ".join(errs[-3:]))


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"])
    for query in ["", "?webgl", "?canvas2d"]:
        pg = br.new_page(viewport={"width": 820, "height": 540})
        run(pg, query)
        pg.close()
    br.close()
httpd.shutdown()

if fails:
    print(f"FAIL: {len(fails)} checks: " + "; ".join(fails))
    raise SystemExit(1)
print("done: the canvas is the program's frame in every state, WebGL2 and 2-D")
