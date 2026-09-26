#!/usr/bin/env -S uv run --with playwright --script
"""Verify the looping ambient audio end-to-end in headless Chromium:

  1. boot walk e1m1 -> the placed static loops (machine hums) AND the two
     automatic leaf-ambient loops (water1/wind2) start as live Web Audio nodes;
  2. `map e1m2` (a real level swap) -> sound_generation bumps, every old loop
     node is STOPPED, and the new level's loops (the 24 fire1.wav torches)
     restart in their place;
  3. demo<->walk mode transitions tear loops down the same way;
  4. no console errors anywhere.

Usage: verify_ambient.py [webdir]   (defaults to the repo's web/; pass a
deploy dir — PLATFORM.md — to test changes without touching the deployed
page).

The page's loops are its own (`staticLoops`, `ambientLoops`, each with the
placement `lp` the program sent); the listener is the page's copy of the
program's last LISTENER record, the one the live gains were set from.
"""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8167)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox",
        # Let audioCtx.resume() succeed without a user gesture so the loop
        # graph actually runs under headless.
        "--autoplay-policy=no-user-gesture-required",
    ])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=60000)

    # Count stop() calls on loop sources to PROVE teardown on level change.
    pg.evaluate("""() => {
        window._stopped = 0;
        const orig = AudioBufferSourceNode.prototype.stop;
        AudioBufferSourceNode.prototype.stop = function(...a) {
            window._stopped++; return orig.apply(this, a);
        };
    }""")

    # Boot the walk. The page only creates its AudioContext on a real canvas
    # mousedown (a gesture the overlay intercepts under headless), so create +
    # resume it directly — the autoplay flag above lets resume() succeed.
    pg.evaluate("document.getElementById('walkBtn').click()")
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")
    time.sleep(2.5)  # frames tick; statics decode + start

    state = pg.evaluate("""async () => ({
        ctx: audioCtx ? audioCtx.state : 'none',
        gen: await exp.sound_generation(),
        statics: staticLoops.length,
        ambients: ambientLoops.length,
        ambChans: ambientLoops.map(l => l.ch).sort(),
        // Re-derive the C pan/distance law independently for every loop and
        // compare with the LIVE gain node values the page set this frame.
        lawHolds: staticLoops.length ? staticLoops.every(l => {
            const L = quake.audio.listener, p = l.lp;
            const dx = p.ox - L.x, dy = p.oy - L.y, dz = p.oz - L.z;
            const dist = Math.hypot(dx, dy, dz);
            let pan = 0;
            if (dist > 1e-3) {
                pan = (dx * L.rx + dy * L.ry + dz * L.rz) / dist;
                pan = Math.min(1, Math.max(-1, pan));
            }
            const mono = Math.max(0, 1 - dist * p.atten / 1000) * p.vol * masterVolume();
            const el = Math.min(1, Math.max(0, mono * (1 - pan)));
            const er = Math.min(1, Math.max(0, mono * (1 + pan)));
            return Math.abs(l.lg.gain.value - el) < 1e-4
                && Math.abs(l.rg.gain.value - er) < 1e-4;
        }) : false,
        minDist: staticLoops.length ? Math.min(...staticLoops.map(l =>
            Math.hypot(l.lp.ox - quake.audio.listener.x, l.lp.oy - quake.audio.listener.y,
                       l.lp.oz - quake.audio.listener.z))) : -1,
        looping: staticLoops.every(l => l.src.loop === true)
                 && ambientLoops.every(l => l.src.loop === true),
    })""")
    check("audio context running", state["ctx"] == "running", state["ctx"])
    check("e1m1 static loops started", state["statics"] >= 5, f"{state['statics']} loops")
    check("both leaf-ambient loops up (water+wind)", state["ambChans"] == [0, 1],
          str(state["ambChans"]))
    check("every node is a looping source", state["looping"])
    check("live gains match the C pan/distance law", state["lawHolds"],
          f"nearest loop {state['minDist']:.0f}u away")

    # A REAL level change through the console: map e1m2.
    pg.evaluate("""() => {
        exp.console_toggle();
        for (const c of 'map e1m2') exp.console_char(c.codePointAt(0));
        return exp.console_enter();
    }""")
    time.sleep(2.5)
    s2 = pg.evaluate("""async () => ({
        gen: await exp.sound_generation(),
        statics: staticLoops.length,
        stopped: window._stopped,
        // e1m2's spawn hall is torch-lit: some loop is in earshot (its live
        // L/R gains are non-zero) right from the start.
        anyAudible: staticLoops.some(l => l.lg.gain.value > 0 || l.rg.gain.value > 0),
        minDist: staticLoops.length ? Math.min(...staticLoops.map(l =>
            Math.hypot(l.lp.ox - quake.audio.listener.x, l.lp.oy - quake.audio.listener.y,
                       l.lp.oz - quake.audio.listener.z))) : -1,
    })""")
    check("changelevel bumps sound_generation", s2["gen"] != state["gen"],
          f"{state['gen']} -> {s2['gen']}")
    old_total = state["statics"] + state["ambients"]
    check("old loops stopped on changelevel", s2["stopped"] >= old_total,
          f"{s2['stopped']} stop() calls >= {old_total} old loops")
    check("e1m2 loops restarted (the torches)", s2["statics"] >= 20,
          f"{s2['statics']} loops")
    # The nearest torch sits beyond ATTN_STATIC's ~333u earshot at spawn (the
    # gains being 0 there IS the faithful behaviour, verified by the law check
    # above). Walk into the level until one comes into range and turns audible.
    pg.keyboard.down("w")
    time.sleep(4.0)
    pg.keyboard.up("w")
    s2b = pg.evaluate("""() => ({
        anyAudible: staticLoops.some(l => l.lg.gain.value > 0 || l.rg.gain.value > 0),
        minDist: staticLoops.length ? Math.min(...staticLoops.map(l =>
            Math.hypot(l.lp.ox - quake.audio.listener.x, l.lp.oy - quake.audio.listener.y,
                       l.lp.oz - quake.audio.listener.z))) : -1,
    })""")
    check("walking toward a torch makes its loop audible", s2b["anyAudible"],
          f"nearest loop {s2['minDist']:.0f}u -> {s2b['minDist']:.0f}u")

    # Mode transition (walk -> demo) tears down + rebuilds from the demo signon.
    pg.evaluate("document.getElementById('demoBtn').click()")
    time.sleep(2.5)
    s3 = pg.evaluate("""async () => ({
        gen: await exp.sound_generation(),
        statics: staticLoops.length,
        stopped: window._stopped,
    })""")
    check("demo boot bumps generation again", s3["gen"] != s2["gen"])
    check("walk loops stopped on demo boot", s3["stopped"] > s2["stopped"])
    check("demo signon statics looping", s3["statics"] > 0, f"{s3['statics']} loops")

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
