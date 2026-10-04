#!/usr/bin/env -S uv run --with playwright --script
"""From a tick to the screen: how long after the page asks for a frame the
display compositor swaps it in — measured in a trace, not at the draw call.

The latency probe (`quake.latency`, latency.py) stops at the page's draw
call. That is the page's own part of the delay, and it can mislead: a
canvas drawn outside a refresh's callback has an early draw call and the
same swap as one drawn in the next callback (the browser commits a canvas
with a refresh's main frame). This script follows a frame to the swap:

  * in the page, a mark as each tick is posted and as each frame is drawn
    (`performance.mark`, wrapped around the page's own `sendTick` and
    `present`: nothing in the page is changed), numbered by tick;
  * in a Chromium trace of the same seconds, the main frame that commits
    the draw (the `ProxyMain::BeginMainFrame` it fell in, else the next to
    start), the renderer compositor's draw after it
    (`ProxyImpl::ScheduledActionDraw`), and the display compositor's
    `Display::DrawAndSwap` after that: its end is the swap.

For each frame time asked for (`--stalls`: the program's frames are held
with `stall_ms`, so it takes a `--features bench` build) it prints the
frame's time, whether the page waited for its frames, and the medians and
95th percentiles of tick -> draw call and tick -> swap. Run it on two
deploys to compare two pages (PLATFORM.md, "A frame", has such a table).

What it is not: light on a screen. Headless Chromium's compositor is the
browser's own scheduler ticking at 60 Hz, with no display behind it: a real
one shows a swap at its next refresh. $QUAKE_GPU=1 takes the page through
WebGL2 on the GPU, as a phone or a desktop does (isolated.py).

Usage: swap_trace.py DEPLOY [--stalls 0,8,12,16,21,34] [--touch] [--query '?2026&wait'] [--secs 5]
       (--touch: the page is told it is a touch screen's, `quake.pacing.scarceCores`)
"""
import argparse, bisect, json, statistics, time
from playwright.sync_api import sync_playwright
import isolated

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("deploy", help="a deploy dir of a --features bench build")
ap.add_argument("--stalls", default="0,8,12,16,21,34", help="ms each host frame is held (stall_ms), a row each")
ap.add_argument("--touch", action="store_true", help="tell the page it is a touch screen's (pacing.scarceCores)")
ap.add_argument("--query", default="?2026", help="the page's query (`?2026&wait` keeps the wait)")
ap.add_argument("--secs", type=float, default=5, help="seconds traced a row")
ap.add_argument("--port", type=int, default=isolated.port(8574))
a = ap.parse_args()

# The marks: 'qt<seq>' as tick <seq> is posted; 'qp<seq>' as a frame is drawn,
# <seq> the last tick the program had answered when the frame was claimed
# (a turn's frame comes before its Sync: that tick's frame is the one drawn).
MARKS = """() => {
  const send = window.sendTick, show = window.present;
  window.sendTick = (seq, dt) => { performance.mark('qt' + seq); return send(seq, dt); };
  window.present = () => { const ack = Atomics.load(ctl, C.ACK), r = show(); if (r) performance.mark('qp' + ack); return r; };
}"""


def pct(xs, p):
    xs = sorted(xs)
    return xs[int(p / 100 * (len(xs) - 1))] if xs else float("nan")


def follow(events):
    """Each tick's first drawn frame: (tick -> draw call, tick -> swap), ms."""
    marks = sorted((e["ts"], e["name"]) for e in events
                   if e.get("name", "")[:2] in ("qt", "qp") and e["name"][2:].isdigit() and e.get("ph") in ("R", "I", "i", "n", "b"))
    posted = {int(n[2:]): t for t, n in marks if n.startswith("qt")}
    spans = lambda name: sorted((e["ts"], e["ts"] + e["dur"]) for e in events if e.get("ph") == "X" and e["name"] == name)
    main_frames, swaps = spans("ProxyMain::BeginMainFrame"), spans("Display::DrawAndSwap")
    draws = [s for s, _ in spans("ProxyImpl::ScheduledActionDraw")]
    starts, swap_starts = [s for s, _ in main_frames], [s for s, _ in swaps]
    out, seen = [], set()
    for t, n in marks:
        seq = int(n[2:])
        if not n.startswith("qp") or seq not in posted or seq in seen:
            continue
        seen.add(seq)
        j = bisect.bisect_right(starts, t) - 1                 # the main frame the draw fell in...
        if j < 0 or main_frames[j][1] < t:
            j += 1                                             # ...else the next one to start
        if j >= len(main_frames):
            continue
        k = bisect.bisect_left(draws, main_frames[j][1])       # the renderer compositor's draw after the commit
        if k >= len(draws):
            continue
        m = bisect.bisect_left(swap_starts, draws[k])          # the display compositor's after that
        if m >= len(swaps):
            continue
        out.append(((t - posted[seq]) / 1000, (swaps[m][1] - posted[seq]) / 1000))
    return out


httpd = isolated.serve(a.deploy, a.port)
with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    print("stall  frame ms  waited for   tick -> draw call   tick -> swap      (median / p95 ms)")
    for stall in [int(s) for s in a.stalls.split(",")]:
        pg = br.new_context(viewport={"width": 1280, "height": 860}).new_page()
        pg.goto(f"http://127.0.0.1:{a.port}/index.html{a.query}", wait_until="load")
        pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
        pg.evaluate("document.getElementById('overlay').click()")           # the first gesture
        for line in ("boot", "menu_cancel", "exec vid_native 0", "set_resolution 960 600"):
            pg.evaluate("line => quake.callLine(line)", line)
        if pg.evaluate("quake.call('stall_ms', 0)") != 0:
            raise SystemExit("this deploy answers no stall_ms: build it with --features bench (verify_audio_resilience.py)")
        if a.touch:
            pg.evaluate("() => { if (quake.pacing) quake.pacing.scarceCores = true; }")
        pg.evaluate("s => quake.call('stall_ms', s)", stall)
        pg.evaluate(MARKS)
        time.sleep(3.0)                                                     # the page takes its way of waiting
        br.start_tracing(page=pg, categories=["cc", "viz", "benchmark", "blink.user_timing"])
        pg.evaluate("quake.live = []")
        time.sleep(a.secs)
        live = pg.evaluate("quake.live.map(x => [x.shown, x.wait, x.turn || 0, !!x.relaxed])")
        trace = json.loads(br.stop_tracing())
        rows = follow(trace["traceEvents"] if isinstance(trace, dict) else trace)
        shown = [x for x in live if x[0]]
        # A page without `pacing` (before 2026-10-03) says no frame time: its wait is the frame's.
        frame = statistics.median((x[2] or x[1]) for x in shown) if shown else 0.0
        waited = 1 - sum(1 for x in live if x[3]) / max(1, len(live))
        if rows:
            d, s = [r[0] for r in rows], [r[1] for r in rows]
            print(f"{stall:5}  {frame:8.1f}  {waited:9.0%}    {statistics.median(d):7.1f} / {pct(d, 95):5.1f}    "
                  f"{statistics.median(s):7.1f} / {pct(s, 95):5.1f}     {len(rows)} frames", flush=True)
        else:
            print(f"{stall:5}  no frame followed to a swap ({len(shown)} shown)", flush=True)
        pg.context.close()
    br.close()
httpd.shutdown()
