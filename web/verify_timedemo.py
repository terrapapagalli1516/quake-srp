#!/usr/bin/env -S uv run --with playwright --script
"""Verify id's `timedemo` and `pause` end-to-end in headless Chromium,
through the page's own keyboard path:

  1. timedemo: ~, `timedemo demo1`, Enter, ~ —
     the program then runs host frames back to back (timedemo_running: it
     polls for input instead of waiting for the page's ticks), the page
     shows the demo while it does, and the console gets CL_FinishTimeDemo's line
     "%i frames %5.1f seconds %5.1f fps": well formed, 969 frames (id's C
     draws 969 for demo1), a plausible rate that agrees with frames/seconds.
     The attract loop's next demo plays afterwards, under the 72 fps cap
     again. Run at 960x600 (the page's default) and 640x400 (or the modes in
     QUAKE_TIMEDEMO_RES, e.g. "320x200,640x400,960x600"); the lines are
     printed (PERF_PLAN.md's browser numbers). Screenshot:
     verify_timedemo.png (mid-run).
  2. pause, in a game (the page's walk button, Esc): the PAUSE key (default.cfg
     `bind PAUSE "pause"`) puts id's plaque up — gfx/pause.lmp, read from the
     pak here, texel for texel at ((w-128)/2, (h-48-24)/2) — and the console
     says "player paused the game"; the world freezes: +forward held for a
     second moves nothing (the listener stays put), and once the notify line
     has gone the frames are identical. PAUSE again: "player unpaused the
     game", the plaque gone, +forward moves the player. PAUSE also works with
     the console down. Screenshot: verify_pause.png.

Usage: verify_timedemo.py [webdir]   (defaults to this script's directory;
pass a deploy dir — PLATFORM.md.)
"""
import os, re, struct, sys, time
from playwright.sync_api import sync_playwright
import isolated

PAK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "quake-data", "ID1", "PAK0.PAK")

def pak_file(name):
    """One file out of id's pak0.pak (the PACK directory: 64-byte entries)."""
    with open(PAK, "rb") as f:
        data = f.read()
    ofs, length = struct.unpack_from("<ii", data, 4)
    for i in range(length // 64):
        n, fo, fl = struct.unpack_from("<56sii", data, ofs + 64 * i)
        if n.split(b"\0")[0].decode() == name:
            return data[fo:fo + fl]
    raise KeyError(name)

WEB = isolated.webdir()
PORT = isolated.port(8176)
httpd = isolated.serve(WEB, PORT)

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

LINE = re.compile(r"^(-?\d+) frames +(\d+\.\d) seconds +(\d+\.\d) fps$")

# The console scrollback (the console_text call).
CONSOLE = "() => quake.text('console_text')"
GRAB = """name => {
    const c = document.getElementById('c');
    window[name] = quake.readback();
}"""
SAME = """([a, b]) => {
    const A = window[a], B = window[b];
    if (A.length !== B.length) return false;
    for (let i = 0; i < A.length; i++) if (A[i] !== B[i]) return false;
    return true;
}"""

def boot_page(pg):
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    errs = []
    ctx = br.new_context(viewport={"width": 820, "height": 560})
    pg = ctx.new_page()
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    boot_page(pg)
    key = lambda k, n=1: [pg.keyboard.press(k) or time.sleep(0.05) for _ in range(n)]
    running = lambda: pg.evaluate("exp.timedemo_running()")

    check("the attract demo plays with no menu", pg.evaluate("exp.menu_visible()") == 0)
    results = {}
    modes = os.environ.get("QUAKE_TIMEDEMO_RES", "960x600,640x400")
    for (w, h) in [tuple(int(v) for v in m.split("x")) for m in modes.split(",")]:
        pg.evaluate(f"exp.set_resolution({w}, {h})")
        time.sleep(0.3)
        key("Backquote")
        pg.keyboard.type("timedemo demo1")
        key("Enter")
        key("Backquote")                           # watch it without the console
        check(f"{w}x{h}: timedemo starts", running() == 1)
        # The page keeps presenting while it runs: two grabs differ.
        time.sleep(0.4)
        pg.evaluate(GRAB, "_a")
        time.sleep(0.25)
        pg.evaluate(GRAB, "_b")
        if running():
            check(f"{w}x{h}: the demo is shown as it runs", not pg.evaluate(SAME, ["_a", "_b"]))
            if not os.path.exists(os.path.join(WEB, "verify_timedemo.png")) or w == 960:
                pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_timedemo.png"))
        t0 = time.time()
        while running() and time.time() - t0 < 120:
            time.sleep(0.2)
        check(f"{w}x{h}: timedemo ends", running() == 0, f"{time.time() - t0:.1f} s after")
        text = pg.evaluate(CONSOLE)
        lines = [l for l in text.split("\n") if " frames " in l]
        line = lines[-1] if lines else ""
        m = LINE.match(line)
        check(f"{w}x{h}: CL_FinishTimeDemo's line, well formed", m is not None, repr(line))
        if m:
            frames, secs, fps = int(m[1]), float(m[2]), float(m[3])
            results[(w, h)] = line
            check(f"{w}x{h}: 969 frames, as id's timedemo demo1", frames == 969, line)
            check(f"{w}x{h}: a plausible rate", 20 <= fps <= 100000, line)
            # %5.1f rounds the seconds; the rate is frames / the unrounded time.
            lo, hi = frames / (secs + 0.05), frames / max(secs - 0.05, 1e-3)
            check(f"{w}x{h}: fps agrees with frames/seconds", lo - 0.1 <= fps <= hi + 0.1,
                  f"{fps} in [{lo:.1f}, {hi:.1f}]")
        check(f"{w}x{h}: 'Playing demo from demo1.dem.' before it",
              "Playing demo from demo1.dem." in text)
        check(f"{w}x{h}: the attract loop goes on", pg.evaluate("exp.in_walk_mode()") == 0)
        capped = pg.evaluate("""async () => {
            quake.pause();
            const ran = await Promise.all(Array.from({ length: 144 }, () => exp.step(1 / 144)));
            quake.resume();
            return ran.reduce((a, b) => a + b, 0);
        }""")
        check(f"{w}x{h}: the 72 fps cap is back", 71 <= capped <= 73, str(capped))
    for (w, h), line in results.items():
        print(f"browser timedemo demo1 {w}x{h}: {line}")

    # 2. pause, in a game.
    pw, ph = struct.unpack_from("<ii", pak_file("gfx/pause.lmp"))
    texels = pak_file("gfx/pause.lmp")[8:]
    pal = pak_file("gfx/palette.lmp")
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")                                  # the boot menu away
    pg.wait_for_function("Promise.all([exp.menu_visible(), exp.in_walk_mode()]).then(([m, w]) => !m && w === 1)",
                         timeout=5000)
    time.sleep(0.5)
    W, H = pg.evaluate("Promise.all([exp.width(), exp.height()])")
    x0, y0 = (W - pw) // 2, (H - 48 - ph) // 2
    PLAQUE = """([x0, y0, w, h]) => Array.from(quake.readback(x0, y0, w, h))"""
    def plaque_up():
        px = pg.evaluate(PLAQUE, [x0, y0, pw, ph])
        return all(px[4 * i:4 * i + 3] == list(pal[3 * t:3 * t + 3]) for i, t in enumerate(texels))
    listener = lambda: pg.evaluate("Promise.all([exp.listener_x(), exp.listener_y(), exp.listener_z()])")
    check("not paused: no plaque", not plaque_up())
    key("Pause")
    time.sleep(0.3)
    check("PAUSE: id's plaque, texel for texel", plaque_up(), f"{pw}x{ph} at ({x0},{y0}) on {W}x{H}")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_pause.png"))
    check("'player paused the game' (SV_BroadcastPrintf)",
          "player paused the game" in pg.evaluate(CONSOLE))
    l0 = listener()
    pg.keyboard.down("w")
    time.sleep(1.0)
    check("+forward held a second while paused: nothing moves", listener() == l0, f"{l0} -> {listener()}")
    pg.keyboard.up("w")
    time.sleep(3.2)                                # the notify line expires (con_notifytime 3)
    pg.evaluate(GRAB, "_p1")
    time.sleep(0.5)
    pg.evaluate(GRAB, "_p2")
    check("the paused world is frozen: the frames are identical", pg.evaluate(SAME, ["_p1", "_p2"]))
    key("Pause")
    time.sleep(0.3)
    check("PAUSE again: the plaque is gone", not plaque_up())
    check("'player unpaused the game'", "player unpaused the game" in pg.evaluate(CONSOLE))
    pg.keyboard.down("w")
    time.sleep(0.7)
    pg.keyboard.up("w")
    check("+forward moves the player again", listener() != l0, f"{l0} -> {listener()}")
    # PAUSE is no console key: it pauses with the console down too.
    key("Backquote")
    key("Pause")
    key("Backquote")
    time.sleep(0.3)
    check("PAUSE with the console down pauses", plaque_up())
    key("Pause")
    time.sleep(0.3)
    check("...and unpauses", not plaque_up())

    check("no console errors", not errs, "; ".join(errs[:3]))
    br.close()

print(f"\n{passed} passed, {failed} failed")
httpd.shutdown()
sys.exit(1 if failed else 0)
