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
it also sends the frames through the staging copy, loses the context and
restores it, and — the palette changing every few milliseconds — reads the
canvas right after each of the presenter's own draws: every frame through
the palette it came with, never the one before or after (the readback of the
other cases draws the frame again with whatever textures are current, so it
could not see a palette that reached the GPU late). With `?canvas2d` it also
walks the window up through sixteen sizes to the largest frame there is, on
the threads build's fixed memory: every frame the box's size, the game never
stopped.

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

# Each frame through its own palette (WebGL2). While the palette flips (gamma
# 1.0 <-> 0.6 from the console, every few ms, so consecutive frames often
# differ in it), every presenter.draw is watched: the frame's indices and
# palette are copied as the draw starts, and the canvas is read right after
# the draw's own drawArrays, before any later GL call, and compared pixel by
# pixel with palette[index]. Answers the draws watched, how many came with a
# palette other than the frame before's, and the draws whose canvas differed.
OWN_PALETTE = """async ([secs, flipMs]) => {
    const gl = presenter.gl, draw = presenter.draw, drawArrays = gl.drawArrays;
    const log = { draws: 0, changes: 0, wrong: 0, wrongPixels: 0 };
    let expect = null, last = null;
    gl.drawArrays = (...a) => {
        drawArrays.apply(gl, a);
        const e = expect; expect = null;
        if (!e) return;
        const { w, h, px, pal } = e, out = new Uint8Array(w * h * 4);
        gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, out);        // the bottom row first
        let bad = 0;
        for (let y = 0; y < h; y++) {
            const o = (h - 1 - y) * w * 4, i0 = y * w;
            for (let x = 0; x < w; x++) {
                const c = px[i0 + x] * 4, q = o + x * 4;
                if (out[q] !== pal[c] || out[q + 1] !== pal[c + 1] || out[q + 2] !== pal[c + 2]) bad++;
            }
        }
        if (bad) { log.wrong++; log.wrongPixels += bad; }
    };
    presenter.draw = (frame, done) => {
        if (frame.indexed) {
            const pal = frame.palette.slice();
            expect = { w: frame.w, h: frame.h, px: frame.pixels.slice(), pal };
            const key = pal.join();
            log.draws++;
            if (last !== null && key !== last) log.changes++;
            last = key;
        }
        return draw.call(presenter, frame, done);
    };
    let n = 0;
    const flip = setInterval(() => quake.callLine('exec gamma ' + (n++ & 1 ? '1.0' : '0.6')), flipMs);
    quake.resume();
    await new Promise(r => setTimeout(r, secs * 1000));
    clearInterval(flip);
    quake.pause();
    await new Promise(r => setTimeout(r, 100));
    presenter.draw = draw; gl.drawArrays = drawArrays;
    return log;
}"""

fails = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""))
    if not ok:
        fails.append(name)


def walk_sizes(pg):
    """A window walked up through sixteen sizes to a 4224x2816 box, 11.9
    million pixels a frame at 1x (`vid::MAX_FRAME_PIXELS` is 12), on the
    2-D canvas, whose RGBA frames take the most memory: each frame the box's
    size, and the game never stops. The threads build's memory is a fixed
    512 MiB; its frame buffers are allocated once, for the largest frame, so
    no size leaves a hole the next cannot use (quake-wasm `main`'s
    `reserve_frames`)."""
    pg.evaluate("line => quake.callLine(line)", "exec vid_native 1; vid_pixelsize 1")
    pg.evaluate("() => quake.resume()")
    sizes = [(1280 + (4224 - 1280) * i // 15, 720 + (2816 - 720) * i // 15) for i in range(16)]
    wrong = []
    for w, h in sizes:
        pg.set_viewport_size({"width": w + 34, "height": h + 84})
        time.sleep(0.4)
        size = pg.evaluate("Promise.all([exp.width(), exp.height()])")
        if size != [w, h]:
            wrong.append(f"{w}x{h}: {size[0]}x{size[1]}")
    stopped = pg.evaluate("document.body.innerText.includes('The game stopped')")
    check("a window walked up to a 4224x2816 box: every frame its size, and the game goes on",
          not wrong and not stopped, "; ".join(wrong) or ("the game stopped" if stopped else ""))
    r = pg.evaluate(FRAME, [2, 1 / 72])
    check("...and the largest frame on the canvas is the program's RGBA", r["presented"] and r["page"] == r["prog"],
          f"{r['size'][0]}x{r['size'][1]} canvas {r['page']} program {r['prog']}")
    pg.set_viewport_size({"width": 820, "height": 540})


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
    if query == "?canvas2d":
        walk_sizes(pg)
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
        # Every frame through the palette it came with, the palette flipping.
        r = pg.evaluate(OWN_PALETTE, [3, 7])
        check("a changing palette: every frame is drawn through its own",
              r["wrong"] == 0 and r["draws"] >= 30 and r["changes"] >= 10,
              f"{r['draws']} draws, {r['changes']} with a new palette, {r['wrong']} wrong ({r['wrongPixels']} pixels)")
        pg.evaluate("line => quake.callLine(line)", "exec gamma 0.7")
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
