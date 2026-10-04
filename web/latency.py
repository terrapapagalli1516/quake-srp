#!/usr/bin/env -S uv run --with playwright --script
"""Input-to-present latency in the page: from each input event's timeStamp
(when the browser got it, so the wait for the next refresh counts) to the
putImageData of the first frame that consumed it — a key, the mouse, the
gamepad — in the live game on the slop preset (a frame every refresh), at
960x600 (`vid_native 0`, the size PLATFORM.md's older table used).

Two refresh rates:

- 60 Hz: headless Chromium's own requestAnimationFrame (its compositor's
  refresh; Firefox's likewise).
- 240 Hz, emulated: the page's own loop paused and a 4.17 ms timer driving
  the same tick-and-present (`quake.tick`), as a 240 Hz display's refresh
  would. The mouse is left out there: the browser still delivers mousemove
  on its own 60 Hz refresh (continuous input is aligned to it), which a real
  240 Hz display would not.

What it cannot see: from putImageData to light — the compositor and the
display, a refresh or two more (`?lowlatency` asks to skip one, where the
browser can) — nor a device's own latency (USB polling, the browser's
gamepad poll).

Usage: latency.py [webdir] [--seconds 15]   ($QUAKE_BROWSER=firefox for Firefox)
Reports median / p95 / max in ms, per input and rate, and the frame's own
time (tick to present).
"""
import argparse, json, random, statistics, sys, time
from playwright.sync_api import sync_playwright
import isolated

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("webdir", nargs="?")
ap.add_argument("--seconds", type=float, default=15.0)
ap.add_argument("--json", help="write the raw samples here")
args = ap.parse_args()
WEB = args.webdir or isolated.webdir()
PORT = isolated.port(8176)
httpd = isolated.serve(WEB, PORT)

# A synthetic pad whose right stick the script moves (verify_gamepad.py has
# the full one); `timestamp` is when its state changed, as a real pad's.
FAKE_PAD = r"""
(() => {
  const buttons = Array.from({ length: 17 }, () => ({ pressed: false, touched: false, value: 0 }));
  const pad = window.__pad = { id: 'latency pad', index: 0, connected: true, mapping: 'standard',
                               timestamp: 0, axes: [0, 0, 0, 0], buttons };
  window.__stick = (x) => { pad.axes = [0, 0, x, 0]; pad.timestamp = performance.now(); };
  navigator.getGamepads = () => [pad, null, null, null];
})();
"""

# The page's loop at an emulated refresh: paused, and a timer ticking it.
EMULATE = r"""
(hz) => {
  // A timer loop at the scheduler's user-blocking priority where there is one
  // (Chromium): a normal task (setTimeout's, a MessageChannel's) waits out
  // the browser's next 60 Hz frame after an input event, as it favours
  // rendering then, which a real 240 Hz refresh would not; setTimeout is
  // clamped to 4 ms besides. Elsewhere a MessageChannel loop.
  quake.pause();
  const period = 1000 / hz, ch = new MessageChannel();
  const post = (typeof scheduler !== 'undefined' && scheduler.postTask)
    ? (f, delay) => scheduler.postTask(f, { priority: 'user-blocking', delay })
    : (f) => { ch.port1.onmessage = f; ch.port2.postMessage(0); };
  let next = performance.now(), last = next;
  window.__emuFrames = [];
  window.__emu = true;
  const turn = () => {
    if (!window.__emu) { quake.resume(); return; }
    const now = performance.now();
    if (now >= next) {
      const r = quake.tick((now - last) / 1000);
      last = now;
      __emuFrames.push(r.wait + r.copy + r.put);
      next = Math.max(next + period, now);
    }
    post(turn, Math.max(0, next - performance.now()));
  };
  post(turn, 0);
}
"""


def stats(xs):
    if not xs:
        return {"n": 0}
    xs = sorted(xs)
    return {"n": len(xs), "median": statistics.median(xs), "p95": xs[int(0.95 * (len(xs) - 1))], "max": xs[-1]}


def fmt(s):
    return "      —" if not s.get("n") else f"{s['median']:6.2f} / {s['p95']:6.2f} / {s['max']:6.2f}  ({s['n']})"


def measure(pg, drive, seconds):
    """The latencies of `seconds` of `drive`, and each event's wait for its
    handler (part of them)."""
    pg.evaluate("quake.latency = []; quake.dispatch = []")
    rnd = random.Random(7)
    t_end = time.time() + seconds
    while time.time() < t_end:
        drive(rnd)
    time.sleep(0.2)
    lat, disp = pg.evaluate("[quake.latency, quake.dispatch]")
    pg.evaluate("quake.latency = null")
    return lat, disp


def key(pg):
    def drive(rnd):
        pg.keyboard.down("d")
        time.sleep(rnd.uniform(0.02, 0.08))
        pg.keyboard.up("d")
        time.sleep(rnd.uniform(0.02, 0.08))
    return drive


def mouse(pg):
    pg.evaluate("dragging = true")   # a drag, not the pointer lock (headless locks move nothing)
    x = [300]

    def drive(rnd):
        x[0] = 300 + rnd.randint(-40, 40)
        pg.mouse.move(x[0], 300)
        time.sleep(rnd.uniform(0.02, 0.08))
    return drive


def stick(pg):
    def drive(rnd):
        pg.evaluate("__stick(0.5)")
        time.sleep(rnd.uniform(0.02, 0.08))
        pg.evaluate("__stick(0)")
        time.sleep(rnd.uniform(0.02, 0.08))
    return drive


results = {}
with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    pg = br.new_page(viewport={"width": 1280, "height": 860})
    pg.add_init_script(FAKE_PAD)
    pg.goto(f"http://127.0.0.1:{PORT}/index.html?slop", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    pg.evaluate("hideOverlayForever()")
    for line in ("boot", "menu_cancel", "exec vid_native 0", "set_resolution 960 600"):
        pg.evaluate("line => quake.callLine(line)", line)
    time.sleep(1.0)
    pg.mouse.move(300, 300)

    # 60 Hz: the page's own loop.
    pg.evaluate("quake.live = []")
    results["60 Hz key"] = measure(pg, key(pg), args.seconds)
    results["60 Hz mouse"] = measure(pg, mouse(pg), args.seconds)
    pg.evaluate("dragging = false")
    results["60 Hz pad"] = measure(pg, stick(pg), args.seconds)
    live = pg.evaluate("quake.live.filter(x => x.shown).map(x => x.wait + x.copy + x.put)")
    rafs = pg.evaluate("quake.live.map(x => x.t)")
    pg.evaluate("quake.live = null")
    frame60 = stats(live)
    hz60 = (len(rafs) - 1) / ((rafs[-1] - rafs[0]) / 1000) if len(rafs) > 1 else 0

    # 240 Hz, emulated.
    pg.evaluate(EMULATE, 240)
    time.sleep(0.5)
    t0, n0 = time.time(), pg.evaluate("__emuFrames.length")
    results["240 Hz key"] = measure(pg, key(pg), args.seconds)
    results["240 Hz pad"] = measure(pg, stick(pg), args.seconds)
    hz240 = (pg.evaluate("__emuFrames.length") - n0) / (time.time() - t0)
    frame240 = stats(pg.evaluate("__emuFrames.slice()"))
    pg.evaluate("__emu = false")
    res = pg.evaluate("quake.size()")
    br.close()
httpd.shutdown()

print(f"input -> present, ms (median / p95 / max, samples); frame {res[0]}x{res[1]}, slop preset;")
print("the event's wait for its handler (median) in brackets")
for name, (lat, disp) in results.items():
    d = stats(disp)
    print(f"  {name:12s} {fmt(stats(lat))}   [{d['median']:.2f}]" if d.get("n") else f"  {name:12s} {fmt(stats(lat))}")
print(f"  the frame itself (tick -> present): 60 Hz {fmt(frame60)} at {hz60:.1f} Hz; "
      f"240 Hz emulated {fmt(frame240)} at {hz240:.1f} Hz")
if args.json:
    json.dump({k: {"latency": v[0], "dispatch": v[1]} for k, v in results.items()}, open(args.json, "w"))
sys.exit(0)
