#!/usr/bin/env -S uv run --with playwright --script
"""Verify looping one-shots (CENSUS F9) end to end in headless Chromium, on id's
own demo1, through id's mixer in the program and the page's AudioWorklet:

  1. a door "moving" sample (doors/stndr1.wav on entity 26, CHAN_VOICE)
     plays on a dynamic channel, and the door's stop sound on the same
     (entity, channel) (doors/stndr2.wav) takes the channel from it:
     SND_PickChannel's override ends the hum;
  2. the train's hum (plats/train1.wav on entity 195, from demo t ~21 s)
     LOOPS from its `cue ` point lap after lap — GetWavinfo +
     SND_PaintChannels restart it, so its position falls back and its end
     moves on — re-spatialized every frame from the train as it and the
     recorded camera move (S_Update), until the train's stop sound
     (plats/train2.wav, t ~38.7 s) overrides it;
  3. meanwhile the worklet plays the ring without running dry, and what it
     plays is sound;
  4. no console errors.

Usage: verify_loops.py [webdir]   (defaults to the repo's web/; pass a deploy
dir — PLATFORM.md).
"""
import sys, time
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

def channels(pg):
    """The mixer's channels: index, sample, left, right, master, pos, end, entity, channel."""
    out = []
    for line in pg.evaluate("quake.text('snd_channels')").splitlines():
        f = line.split()
        out.append({"i": int(f[0]), "sample": f[1], "left": int(f[2]), "right": int(f[3]),
                    "pos": int(f[5]), "end": int(f[6]), "ent": int(f[7]), "chan": int(f[8])})
    return out

DOOR, DOOR_STOP, DOOR_KEY = "doors/stndr1.wav", "doors/stndr2.wav", (26, 2)
TRAIN, TRAIN_STOP, TRAIN_KEY = "plats/train1.wav", "plats/train2.wav", (195, 2)

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=60000)
    # The attract demo is running; start audio (headless has no gesture, the
    # autoplay flag lets resume() succeed).
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")
    pg.wait_for_function("quake.audio.ring().running && quake.audio.ring().worklet", timeout=10000)
    u0 = pg.evaluate("quake.audio.ring().underruns")
    pg.evaluate("quake.audio.resetPeak()")

    # Watch the mixer's channels every 50 ms until the train has stopped.
    looks = []                 # (key, sample, channel dict) per look
    deadline = time.time() + 75
    train_done = False
    while time.time() < deadline and not train_done:
        for c in channels(pg):
            looks.append(((c["ent"], c["chan"]), c["sample"], c))
        seen = [s for k, s, _ in looks if k == TRAIN_KEY]
        train_done = TRAIN in seen and seen[-1] == TRAIN_STOP
        time.sleep(0.05)

    def on(key):
        return [(s, c) for k, s, c in looks if k == key]

    # 1. The door: its hum, then its stop sound on the same key.
    door = on(DOOR_KEY)
    names = [s for s, _ in door]
    check("a door hum plays on a dynamic channel", DOOR in names
          and all(4 <= c["i"] < 12 for s, c in door if s == DOOR), str(sorted(set(names))))
    check("the door's stop sound takes its (entity, channel)", DOOR in names and DOOR_STOP in names
          and names.index(DOOR_STOP) > names.index(DOOR), "")

    # 2. The train's hum loops, follows the train, and its stop sound ends it.
    hum = [c for s, c in on(TRAIN_KEY) if s == TRAIN]
    laps = sum(1 for a, b in zip(hum, hum[1:]) if b["pos"] < a["pos"])
    ends = sorted({c["end"] for c in hum})
    check("the train's hum loops from its cue point", laps >= 2 and len(ends) >= 3,
          f"{len(hum)} looks, {laps} laps, {len(ends)} ends")
    vols = {(c["left"], c["right"]) for c in hum}
    check("re-spatialized from the train as it and the camera move", len(vols) >= 5, f"{len(vols)} volume pairs")
    names = [s for s, _ in on(TRAIN_KEY)]
    check("the train's stop sound ends the loop", train_done and names[-1] == TRAIN_STOP
          and TRAIN not in names[names.index(TRAIN_STOP):], "")

    # 3. The ring all along.
    u1 = pg.evaluate("quake.audio.ring().underruns")
    peak = pg.evaluate("quake.audio.ring().peak")
    check("the worklet never ran dry meanwhile", u1 == u0, f"+{u1 - u0}")
    check("what it played is sound", peak > 500, f"peak {peak}")

    check("no console errors", not errs, str(errs[-5:]))
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
