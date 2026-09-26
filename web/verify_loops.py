#!/usr/bin/env -S uv run --with playwright --script
"""Verify looping one-shots (CENSUS F9) end-to-end in headless Chromium, on id's
own demo1:

  1. a door "moving" sample (doors/stndr1.wav, recorded at demo t 18.2 s on
     entity 26, CHAN_VOICE) plays as a LOOPING source from its `cue ` point —
     GetWavinfo + SND_PaintChannels — and is re-spatialized every frame from
     its fixed origin like the C channel;
  2. the door's stop sound on the same (entity, channel) (doors/stndr2.wav,
     t 19.0 s) overrides it: SND_PickChannel ends the loop, nothing hums on;
  3. an INAUDIBLE sound on the key of a hum whose first decode is still
     pending ends it too (S_StartSound picks the channel before the
     audibility test): fed to the page's sound path as SAMPLE and SOUND
     records would be (quake.audio.onSample / onSound), the hum never starts
     (and, as a control, the same hum alone does);
  4. a one-shot's sides are clamped at full before the master volume
     (snd_mix.c) and it is re-spatialized every frame (S_Update);
  5. no console errors.

Usage: verify_loops.py [webdir]   (defaults to the repo's web/; pass a deploy
dir — PLATFORM.md).
"""
import sys
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8168)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=60000)
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
            const p = l.lp, L = quake.audio.listener;
            const dx = p.ox - L.x, dy = p.oy - L.y, dz = p.oz - L.z;
            const dist = Math.hypot(dx, dy, dz);
            let pan = 0;
            if (dist > 1e-3) {
                pan = (dx * L.rx + dy * L.ry + dz * L.rz) / dist;
                pan = Math.min(1, Math.max(-1, pan));
            }
            // Each side clamped at full BEFORE the master volume (snd_mix.c).
            const g = Math.max(0, 1 - dist * p.atten / 1000) * p.vol, m = masterVolume();
            return Math.abs(l.lg.gain.value - Math.min(1, Math.max(0, g * (1 - pan))) * m) < 0.05
                && Math.abs(l.rg.gain.value - Math.min(1, Math.max(0, g * (1 + pan))) * m) < 0.05;
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

    # 3. An inaudible override while the hum's first decode is pending. Feed
    #    the page's sound path what the program would send: a looping,
    #    never-decoded 8-bit sample (SAMPLE) played on entity 900 channel 2 at
    #    the listener (SOUND), then (unless `alone`) a sound on the same key
    #    10000 units away (gain 0). Let the decode land, and see whether the
    #    hum started.
    wav = """(n) => {
        const wav = new Uint8Array(44 + n);
        const dv = new DataView(wav.buffer);
        const tag = (o, s) => { for (let i = 0; i < 4; i++) wav[o + i] = s.charCodeAt(i); };
        tag(0, 'RIFF'); dv.setUint32(4, 36 + n, true); tag(8, 'WAVE');
        tag(12, 'fmt '); dv.setUint32(16, 16, true); dv.setUint16(20, 1, true);
        dv.setUint16(22, 1, true); dv.setUint32(24, 11025, true); dv.setUint32(28, 11025, true);
        dv.setUint16(32, 1, true); dv.setUint16(34, 8, true);
        tag(36, 'data'); dv.setUint32(40, n, true);
        for (let i = 0; i < n; i++) wav[44 + i] = 128 + ((Math.random() * 64) | 0) - 32;  // unique: never cached
        return wav;
    }"""
    pg.evaluate(f"window._wav = {wav}")
    hum = """async (alone) => {
        const A = quake.audio, L = A.listener;
        const id = 900000 + ((Math.random() * 1e6) | 0);   // a sample id the program never sends
        A.onSample(id, _wav(2000));
        const base = { id, oy: L.y, oz: L.z, vol: 1, atten: 1, ent: 900, chan: 2, view: false, ls: 0, le: 0 };
        A.onSound({ ...base, ox: L.x });
        if (!alone) A.onSound({ ...base, ox: L.x + 10000 });
        await new Promise(r => setTimeout(r, 1500));
        const started = A.dynLoops.some(l => l.src === A.playingByKey.get('900:2'));
        A.stopKey(900, 2);
        return started;
    }"""
    check("control: a lone hum on a fresh sample starts once decoded", pg.evaluate(hum, True))
    check("an inaudible sound on its key keeps a pending hum from starting", not pg.evaluate(hum, False))

    # 4. A one-shot (no cue point) 50 units to the listener's right, fed the
    #    same way: each side is clamped at full BEFORE the master volume
    #    (snd_mix.c clamps leftvol/rightvol at 255, S_TransferPaintBuffer then
    #    scales by `volume`), so its near side is the master volume, not full;
    #    and S_Update re-spatializes it every frame from its origin while the
    #    recorded player moves on.
    shot = """async () => {
        const A = quake.audio, L = A.listener;
        const id = 900000 + ((Math.random() * 1e6) | 0);
        A.onSample(id, _wav(22050));
        const o = [L.x + 50 * L.rx, L.y + 50 * L.ry, L.z + 50 * L.rz];
        const before = A.dynShots.length;
        A.onSound({ id, ox: o[0], oy: o[1], oz: o[2], vol: 1, atten: 1, ent: 901, chan: 0,
                    view: false, ls: -1, le: 0 });
        for (let i = 0; i < 40 && A.dynShots.length === before; i++) await new Promise(r => setTimeout(r, 25));
        const l = A.dynShots[A.dynShots.length - 1];
        if (!l || l.lp.ox !== o[0]) return { ok: false };
        const first = [l.lg.gain.value, l.rg.gain.value];
        const lx0 = [L.x, L.y];
        await new Promise(r => setTimeout(r, 700));
        const law = () => {
            const dx = o[0] - L.x, dy = o[1] - L.y, dz = o[2] - L.z;
            const dist = Math.hypot(dx, dy, dz);
            const pan = Math.min(1, Math.max(-1, (dx * L.rx + dy * L.ry + dz * L.rz) / dist));
            const g = Math.max(0, 1 - dist / 1000), m = A.masterVolume();
            return [Math.min(1, g * (1 - pan)) * m, Math.min(1, g * (1 + pan)) * m];
        };
        const now = [l.lg.gain.value, l.rg.gain.value], want = law();
        const moved = Math.hypot(L.x - lx0[0], L.y - lx0[1]);
        l.src.stop();
        return { ok: true, first, master: A.masterVolume(), now, want, moved };
    }"""
    r = pg.evaluate(shot)
    check("a one-shot is tracked for re-spatialization", r["ok"], str(r))
    if r["ok"]:
        check("its near side is clamped before the master volume (right = volume, not 1)",
              abs(r["first"][1] - r["master"]) < 0.02 and r["first"][0] < 0.05 and r["master"] < 1, str(r))
        check("it is re-spatialized each frame from its origin as the player moves",
              r["moved"] > 1 and all(abs(a - b) < 0.03 for a, b in zip(r["now"], r["want"])), str(r))

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
