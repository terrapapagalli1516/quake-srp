#!/usr/bin/env -S uv run --with playwright --script
"""Verify looping one-shots (CENSUS F9) end-to-end in headless Chromium, on id's
own demo1:

  1. a door "moving" sample (doors/stndr1.wav, recorded at demo t 18.2 s on
     entity 26, CHAN_VOICE) plays as a LOOPING source from its `cue ` point —
     GetWavinfo + SND_PaintChannels — and is re-spatialized every frame from
     its fixed origin like the C channel;
  2. the door's stop sound on the same (entity, channel) (doors/stndr2.wav,
     t 19.0 s) overrides it: SND_PickChannel ends the loop, nothing hums on;
  3. no console errors.

Usage: verify_loops.py [webdir]   (defaults to the repo's web/; pass a temp dir
holding index.html + a freshly built quake_wasm.wasm).
"""
import functools, http.server, os, socketserver, sys, threading
from playwright.sync_api import sync_playwright

WEB = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("QUAKE_VERIFY_PORT", "8168"))
Handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=WEB)
socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=[
        "--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("typeof exp !== 'undefined' && exp && exp.boot", timeout=60000)
    # The attract demo is running behind the menu; start audio (headless has
    # no gesture, the autoplay flag lets resume() succeed).
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")

    # 1. The first loop of demo1 (t 18.2 s).
    pg.wait_for_function("window.__sndStats && (window.__sndStats.loops || 0) >= 1", timeout=60000)
    s1 = pg.evaluate("""() => ({
        n: dynLoops.length,
        looping: dynLoops.every(l => l.src.loop === true && l.src.loopStart >= 0),
        keyed: dynLoops.some(l => [...playingByKey.values()].includes(l.src)),
        // The live L/R gains follow the C law from the loop's fixed origin.
        lawHolds: dynLoops.every(l => {
            const p = l.lp;
            const dx = p.ox - exp.listener_x(), dy = p.oy - exp.listener_y(),
                  dz = p.oz - exp.listener_z();
            const dist = Math.hypot(dx, dy, dz);
            let pan = 0;
            if (dist > 1e-3) {
                pan = (dx * exp.listener_right_x() + dy * exp.listener_right_y()
                     + dz * exp.listener_right_z()) / dist;
                pan = Math.min(1, Math.max(-1, pan));
            }
            const mono = Math.max(0, 1 - dist * p.atten / 1000) * p.vol * masterVolume();
            return Math.abs(l.lg.gain.value - Math.min(1, Math.max(0, mono * (1 - pan)))) < 0.05
                && Math.abs(l.rg.gain.value - Math.min(1, Math.max(0, mono * (1 + pan)))) < 0.05;
        }),
    })""")
    check("a door hum plays as a looping source", s1["n"] >= 1 and s1["looping"], str(s1))
    check("the loop is keyed on its (entity, channel)", s1["keyed"])
    check("its gains follow the C pan/distance law", s1["lawHolds"])

    # 2. The stop sound (t 19.0 s) overrides it.
    stops0 = pg.evaluate("window.__sndStats.stops")
    pg.wait_for_function("dynLoops.length === 0", timeout=10000)
    stops1 = pg.evaluate("window.__sndStats.stops")
    check("the door's stop sound ends the loop", stops1 > stops0, f"stops {stops0} -> {stops1}")

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
