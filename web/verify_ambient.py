#!/usr/bin/env -S uv run --with playwright --script
"""Verify the level's sound end to end in headless Chromium: id's mixer runs
in the program (quake_rs::snd::Mixer), its samples reach the page's
AudioWorklet through the shared ring, and the level's loops live and die
with the level:

  1. boot walk e1m1 -> the placed static loops (machine hums) are registered
     with the mixer and the two automatic leaf-ambient channels (water1,
     wind2) are up; the ring runs at the device's rate (the 2026 mixer) and
     the worklet plays it without running dry;
  2. `map e1m2` (a real level swap) -> S_StopAllSounds: the generation bumps,
     the ring is cleared of what was mixed ahead, and the new level's loops
     (the torches among them) take the old ones' place;
  3. walking toward a torch makes its channel audible, and the worklet's
     output is sound, not silence;
  4. Classic (`sound_mode classic`): id's mixer at 11025 Hz, which the
     worklet reconstructs at the device's rate, the level's loops kept;
  5. demo<->walk mode transitions stop everything the same way;
  6. the ring's lead (how far ahead of the device the program has mixed: the
     sound latency before the context's own) is measured and printed;
  7. no console errors anywhere.

Usage: verify_ambient.py [webdir]   (defaults to the repo's web/; pass a
deploy dir — PLATFORM.md — to test changes without touching the deployed
page).

The mixer's state comes from the program's own calls (`snd_stats`,
`snd_channels`: every channel with a sound, its volumes and position); the
ring's from the page (`quake.audio.ring()`: the worklet's position, what the
program wrote, underruns, the loudest sample played).
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

def stats(pg):
    text = pg.evaluate("quake.text('snd_stats')")
    return {k: v for k, v in (kv.split("=", 1) for kv in text.split())}

def channels(pg):
    """The mixer's channels: index, sample, left, right, master, pos, end, entity, channel."""
    out = []
    for line in pg.evaluate("quake.text('snd_channels')").splitlines():
        f = line.split()
        out.append({"i": int(f[0]), "sample": f[1], "left": int(f[2]), "right": int(f[3]),
                    "master": int(f[4]), "pos": int(f[5]), "end": int(f[6]), "ent": int(f[7]), "chan": int(f[8])})
    return out

STATIC0 = 12   # the first static channel: 4 ambients + 8 dynamic

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox",
        # Let audioCtx.resume() succeed without a user gesture so the
        # worklet actually plays under headless.
        "--autoplay-policy=no-user-gesture-required",
    ])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=60000)

    # Boot the walk. The page only creates its AudioContext on a real canvas
    # mousedown (a gesture the overlay intercepts under headless), so create +
    # resume it directly — the autoplay flag above lets resume() succeed.
    pg.evaluate("document.getElementById('walkBtn').click()")
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")
    time.sleep(2.5)  # frames tick; the worklet starts

    r = pg.evaluate("quake.audio.ring()")
    s1 = stats(pg)
    ch = channels(pg)
    check("audio context running, the worklet playing the ring", r["running"] and r["worklet"], str(r))
    check("the 2026 mixer at the device's rate", s1["mode"] == "2026" and int(s1["rate"]) == int(s1["device_rate"]),
          f"mode {s1['mode']} rate {s1['rate']} device {s1['device_rate']}")
    check("e1m1's static loops registered with the mixer", int(s1["statics"]) >= 5, f"{s1['statics']} loops")
    amb = sorted(c["i"] for c in ch if c["i"] < 4)
    check("both leaf-ambient channels up (water+wind)", amb == [0, 1],
          str([(c["i"], c["sample"]) for c in ch if c["i"] < 4]))
    on_statics = [c for c in ch if c["i"] >= STATIC0]
    check("the placed loops hold the static channels", len(on_statics) >= 5,
          f"{len(on_statics)} channels: {sorted(set(c['sample'] for c in on_statics))}")

    # A steady stretch: the worklet never runs dry, and the lead is measured.
    pg.evaluate("quake.audioLead = []")
    u0 = pg.evaluate("quake.audio.ring().underruns")
    time.sleep(3.0)
    u1 = pg.evaluate("quake.audio.ring().underruns")
    lead = sorted(pg.evaluate("quake.audioLead"))
    r = pg.evaluate("quake.audio.ring()")
    check("no underruns in 3 s of play", u1 == u0, f"+{u1 - u0}")
    if lead:
        print(f"      lead (ms, at each refresh): min {lead[0]:.1f} median {lead[len(lead)//2]:.1f} "
              f"max {lead[-1]:.1f} over {len(lead)} refreshes; context base {r['baseLatencyMs']:.1f} "
              f"+ output {r['outputLatencyMs']:.1f} ms")

    # A REAL level change through the console: map e1m2.
    gen0 = pg.evaluate("exp.sound_generation()")
    clears0 = pg.evaluate("quake.audio.ring().clears")
    pg.evaluate("""() => {
        exp.console_toggle();
        for (const c of 'map e1m2') exp.console_char(c.codePointAt(0));
        return exp.console_enter();
    }""")
    time.sleep(2.5)
    s2 = stats(pg)
    ch2 = channels(pg)
    torches = [c for c in ch2 if c["i"] >= STATIC0]
    check("changelevel bumps sound_generation", pg.evaluate("exp.sound_generation()") != gen0)
    check("changelevel clears the ring (S_ClearBuffer)", pg.evaluate("quake.audio.ring().clears") > clears0)
    old_set, new_set = set(c["sample"] for c in on_statics), set(c["sample"] for c in torches)
    # S_StopAllSounds empties the table: e1m2's loops alone, not added to
    # e1m1's (e1m1's machine hums are gone, e1m2's torches are there).
    check("e1m2's loops replace e1m1's (the torches among them)", int(s2["statics"]) >= 20
          and len(torches) == int(s2["statics"]) and "ambience/fire1.wav" in new_set
          and not {"ambience/buzz1.wav", "ambience/comp1.wav"} & new_set,
          f"{s1['statics']} -> {s2['statics']} loops: {sorted(new_set)}")
    # The nearest torch sits beyond ATTN_STATIC's ~333u earshot at spawn.
    # Walk into the level until one comes into range and turns audible.
    pg.evaluate("quake.audio.resetPeak()")
    pg.keyboard.down("w")
    time.sleep(4.0)
    pg.keyboard.up("w")
    near = [c for c in channels(pg) if c["sample"] == "ambience/fire1.wav" and (c["left"] or c["right"])]
    check("walking toward a torch makes its channel audible", bool(near),
          str([(c["i"], c["left"], c["right"]) for c in near][:3]))
    peak = pg.evaluate("quake.audio.ring().peak")
    check("the worklet's output is sound", peak > 500, f"peak {peak}")

    # Classic: id's mixer at 11025 Hz, reconstructed at the device's rate.
    statics_before = stats(pg)["statics"]
    pg.evaluate("quake.call('sound_mode', 'classic')")
    time.sleep(1.0)
    ra = pg.evaluate("quake.audio.ring()")
    pg.evaluate("quake.audio.resetPeak()")
    time.sleep(2.0)
    rb = pg.evaluate("quake.audio.ring()")
    sc = stats(pg)
    ring_rate = (rb["pos"] - ra["pos"]) / 2.0
    dev_rate = (rb["played"] - ra["played"]) / 2.0
    check("Classic: id's mixer at 11025 Hz, its loops kept", sc["mode"] == "classic" and sc["rate"] == "11025"
          and rb["rate"] == 11025 and sc["statics"] == statics_before, f"{sc['mode']} {sc['rate']} {sc['statics']}")
    check("Classic: the worklet plays 11025 Hz at the device's rate",
          abs(ring_rate - 11025) < 600 and dev_rate > 30000 and rb["underruns"] == ra["underruns"],
          f"ring {ring_rate:.0f}/s, device {dev_rate:.0f}/s, underruns +{rb['underruns'] - ra['underruns']}")
    pg.evaluate("quake.call('sound_mode', '2026')")

    # Mode transition (walk -> demo) stops everything the same way, and the
    # demo's signon registers its own loops.
    gen1 = pg.evaluate("exp.sound_generation()")
    clears1 = pg.evaluate("quake.audio.ring().clears")
    pg.evaluate("document.getElementById('demoBtn').click()")
    time.sleep(2.5)
    s3 = stats(pg)
    check("demo boot bumps generation again", pg.evaluate("exp.sound_generation()") != gen1)
    check("demo boot clears the ring", pg.evaluate("quake.audio.ring().clears") > clears1)
    check("demo signon statics registered", int(s3["statics"]) > 0, f"{s3['statics']} loops")

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
