#!/usr/bin/env -S uv run --with playwright --with pillow --script
"""Verify the two profiles and the settings, end-to-end in headless Chromium:

  1. The default is the 2026 profile, and the picture fills the window:
     at 1280x800 (devicePixelRatio 1) the page's box is 1246x716 (the window
     less the chrome), the frame 1246x716 at one device pixel a pixel (Auto),
     the canvas that box exactly; any window shape (1600x600: a 1566x516
     frame). The State says native, and the pixel size.
  2. Whole pixels: at devicePixelRatio 2 the box is 2492x1432 device
     pixels; Auto (one render thread) picks 2x2, the frame 1246x716 and the
     canvas 2492 device pixels wide — every frame pixel a whole 2x2 square of
     the screenshot, never smoothed; vid_pixelsize 3 gives 830x477 at 3x3.
  3. The 2026 frame: id's crosshair at the view's centre (crosshair 1 and 0
     differ only in its cell), no 72 fps cap (a second of 1/144 s steps runs
     144 frames), and W walks.
  4. `?classic` is Classic: the profile, config.cfg with nothing changed from
     it, every departure off, id's keys (`w` unbound, `a` +lookup), the 72
     fps cap (72 frames in a second at 144 Hz), the video mode in the 4:3 box
     (960x600), no crosshair.
  5. The one switch: Options > "Classic / 2026", left and right, flips the
     profile, and the canvas goes from the window to the 4:3 box and back.
  6. The settings survive a reload: a 2026 session's pixel size, a binding
     and Screen size; a `?classic` visit sticks for a plain reload after it;
     a returning visitor's old localStorage settings keep their choice (Show
     FPS) and get the 2026 defaults (uncapped, scaled 2-D).
  7. Video Options is honest about native resolution (review: it used to show
     960x600 as current and a pick silently turned Native off): opened while
     actually native, the cursor lands on the live native row (not a stale
     preset) and `vid_native` is still 1; picking a fixed mode turns it off,
     visibly (that mode is now the one marked); and native is one Up/Enter
     away again, at the picked pixel size — the same list, never a dead end.

Screenshots (in the web dir): verify_settings_2026.png, verify_settings_classic.png,
verify_settings_video_native.png.
Also passes with QUAKE_BROWSER=firefox, whose headless build ignores the
context's devicePixelRatio: section 2 is skipped there. Not verified: a real
high-DPI screen, a GPU compositor, Safari.

Usage: verify_settings.py [webdir]   (a deploy dir — PLATFORM.md.)
"""
import io, os, sys, time
from playwright.sync_api import sync_playwright
from PIL import Image
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8177)
httpd = isolated.serve(WEB, PORT)
URL = f"http://127.0.0.1:{PORT}/index.html"
OPTIONS, EXTRAS = 5, 10
NATIVE, FKEY = 32, 64
ROW_VIDEO = 12
VIDEO_PRESETS = 7  # quake_rs::menu::RESOLUTION_PRESETS.len(): the native rows follow these

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

def boot(pg, query=""):
    pg.goto(URL + query, wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    pg.evaluate("document.getElementById('overlay').click()")   # the first gesture
    time.sleep(0.3)

def walk(pg):
    """The page's walk button, and the menu it opens closed."""
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    pg.keyboard.press("Escape")
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)

def frames(pg, n=4):
    pg.evaluate(f"""() => new Promise(r => {{ let k = {n};
        const f = () => (--k > 0 ? requestAnimationFrame(f) : r()); requestAnimationFrame(f); }})""")

# The canvas: its backing store, its CSS box, the device pixel ratio, the
# page's State, and the frame's size.
CANVAS = """() => { const c = document.getElementById('c'), r = c.getBoundingClientRect();
    const b = 2 * parseFloat(getComputedStyle(c).borderLeftWidth);   // the box less its border
    return { w: c.width, h: c.height, cssW: r.width - b, cssH: r.height - b, dpr: devicePixelRatio,
             flags: quake.state.flags, px: quake.state.pixelSize, iw: innerWidth, ih: innerHeight,
             rendering: getComputedStyle(c).imageRendering }; }"""
text = lambda pg, call: pg.evaluate(f"quake.text('{call}')")
cvar = lambda pg, name: pg.evaluate(f"quake.text('cvar', '{name}')")
second_at_144 = """async () => { quake.pause();
    const n = (await Promise.all(Array.from({ length: 144 }, () => exp.step(1 / 144)))).reduce((a, b) => a + b, 0);
    quake.resume(); return n; }"""
# A frozen frame with the page's ticks paused, its pixels kept as window[name].
GRAB = """name => { quake.pause(); quake.tick(0); quake.tick(0);
    window[name] = quake.readback(); }"""
DIFF = """([a, b]) => { const A = window[a], B = window[b], w = document.getElementById('c').width;
    let n = 0, x0 = 1e9, x1 = -1, y0 = 1e9, y1 = -1;
    for (let i = 0; i < A.length; i += 4) if (A[i] !== B[i] || A[i + 1] !== B[i + 1] || A[i + 2] !== B[i + 2]) {
        const p = i / 4, x = p % w, y = (p - x) / w; n++;
        x0 = Math.min(x0, x); x1 = Math.max(x1, x); y0 = Math.min(y0, y); y1 = Math.max(y1, y); }
    return n ? { n, x0, x1, y0, y1 } : null; }"""

def whole_pixels(pg, px, crop=240):
    """How many px x px squares of the canvas's screenshot (device pixels)
    are not one colour, on the grid's best phase (the element's box is
    rounded to device pixels for the shot): 0 means every frame pixel is a
    whole square of the screen's."""
    png = pg.locator("#c").screenshot()
    img = Image.open(io.BytesIO(png)).convert("RGB")
    at = 8 + px * 4                        # clear of the border
    def mixed(ox, oy):
        bad = 0
        for sy in range(at + oy, at + oy + crop, px):
            for sx in range(at + ox, at + ox + crop, px):
                c0 = img.getpixel((sx, sy))
                bad += any(img.getpixel((sx + dx, sy + dy)) != c0 for dy in range(px) for dx in range(px))
        return bad
    return min(mixed(ox, oy) for oy in range(px) for ox in range(px))

with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    errs = []
    def page(ctx):
        pg = ctx.new_page()
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        return pg

    # 1. The 2026 default fills the window.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    pg = page(ctx)
    boot(pg)
    check("the default is the 2026 profile", text(pg, "profile") == "2026")
    pg.evaluate("quake.callLine('exec r_threads 1')")   # Auto's budget on one thread
    walk(pg)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("1280x800: a 1246x716 frame at 1x1, native", (c["w"], c["h"], c["px"]) == (1246, 716, 1)
          and c["flags"] & NATIVE, str(c))
    check("...and the canvas is the page's box exactly",
          (round(c["cssW"]), round(c["cssH"])) == (c["iw"] - 34, c["ih"] - 84) and c["rendering"] == "pixelated", str(c))
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_settings_2026.png"))
    pg.set_viewport_size({"width": 1600, "height": 600})
    pg.wait_for_function("document.getElementById('c').width === 1566", timeout=5000)
    c = pg.evaluate(CANVAS)
    check("any shape: 1600x600 fills with a 1566x516 frame", (c["w"], c["h"], round(c["cssW"]), round(c["cssH"]))
          == (1566, 516, 1566, 516), str(c))

    # 3. The 2026 frame: the crosshair, no cap, W walks.
    pg.set_viewport_size({"width": 1280, "height": 800})
    pg.wait_for_function("document.getElementById('c').width === 1246", timeout=5000)
    pg.evaluate(GRAB, "_x1")
    pg.evaluate("quake.callLine('exec crosshair 0')")
    pg.evaluate(GRAB, "_x0")
    pg.evaluate("quake.callLine('exec crosshair 1')")
    d = pg.evaluate(DIFF, ["_x1", "_x0"])
    # The view is above the status bar (viewsize 100, the 2-D layer at 3x on
    # a 1246x716 screen: 144 rows): its centre (623, 286), on the 2-D
    # layer's grid (621, 285), a 24x24 cell.
    ok = d is not None and 621 <= d["x0"] and d["x1"] < 621 + 24 and 285 <= d["y0"] and d["y1"] < 285 + 24
    check("the crosshair: a + at the view's centre, and nothing else", ok, str(d))
    pg.evaluate("quake.resume()")
    check("no 72 fps cap: 144 frames in a second at 144 Hz", pg.evaluate(second_at_144) == 144)
    x0 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    pg.keyboard.down("w"); time.sleep(1.0); pg.keyboard.up("w")
    x1 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    check("W walks", abs(x1[0] - x0[0]) + abs(x1[1] - x0[1]) > 50, f"{x0} -> {x1}")

    # 5. The one switch: Options > Classic / 2026.
    pg.keyboard.press("Escape")
    pg.keyboard.press("ArrowDown"); pg.keyboard.press("ArrowDown"); pg.keyboard.press("Enter")
    pg.keyboard.press("ArrowUp")          # row 0 wraps to the 14th: Classic / 2026
    time.sleep(0.2)
    pg.keyboard.press("ArrowLeft")
    pg.wait_for_function("!(quake.state.flags & 32)", timeout=5000)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("left: Classic, the 960x600 mode in the 4:3 box",
          text(pg, "profile") == "classic" and (c["w"], c["h"]) == (960, 600)
          and abs(c["cssW"] / c["cssH"] - 4 / 3) < 0.01, str(c))
    pg.keyboard.press("ArrowRight")
    pg.wait_for_function("quake.state.flags & 32", timeout=5000)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("right: 2026 again, the window filled", text(pg, "profile") == "2026" and c["w"] == 1246, str(c))
    for _ in range(3):
        pg.keyboard.press("Escape")

    # 6a. A 2026 session's settings survive a reload.
    pg.evaluate("quake.callLine('exec vid_pixelsize 2; bind j \"+jump\"; viewsize 90')")
    frames(pg)
    pg.wait_for_function("quake.kept('id1/config.cfg').then(t => !!t && t.includes('vid_pixelsize \"2\"'))", timeout=5000)
    boot(pg)
    check("reloaded: the profile, the pixel size, the binding, Screen size",
          text(pg, "profile") == "2026" and cvar(pg, "vid_pixelsize") == "2" and cvar(pg, "viewsize") == "90"
          and "+jump" in pg.evaluate("quake.callLine('exec bind j').then(() => quake.text('console_text'))"),
          text(pg, "config_text"))
    ctx.close()

    # 2. Whole pixels at devicePixelRatio 2.
    ctx = br.new_context(viewport={"width": 1280, "height": 800}, device_scale_factor=2)
    pg = page(ctx)
    boot(pg)
    if pg.evaluate("devicePixelRatio") != 2:
        print("SKIP the devicePixelRatio 2 checks (this browser ignores the context's device_scale_factor)")
    else:
        pg.evaluate("quake.callLine('exec r_threads 1')")
        walk(pg)
        pg.wait_for_function("document.getElementById('c').width === 1246", timeout=5000)
        c = pg.evaluate(CANVAS)
        check("dpr 2: a 2492x1432 box, Auto picks 2x2, a 1246x716 frame",
              (c["w"], c["h"], c["px"], round(c["cssW"] * c["dpr"])) == (1246, 716, 2, 2492), str(c))
        frames(pg)
        bad = whole_pixels(pg, 2)
        check("every frame pixel a whole 2x2 square of the screen", bad == 0, f"{bad} squares mixed")
        pg.evaluate("quake.callLine('exec vid_pixelsize 3')")
        pg.wait_for_function("document.getElementById('c').width === 830", timeout=5000)
        frames(pg)
        c = pg.evaluate(CANVAS)
        bad = whole_pixels(pg, 3)
        check("vid_pixelsize 3: an 830x477 frame, whole 3x3 squares",
              (c["w"], c["h"], c["px"]) == (830, 477, 3) and bad == 0, f"{c}; {bad} mixed")
    ctx.close()

    # 4. ?classic is Classic, and it sticks.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    pg = page(ctx)
    boot(pg, "?classic")
    frames(pg)
    check("?classic: the Classic profile", text(pg, "profile") == "classic")
    check("config.cfg: the profile and nothing changed from it",
          text(pg, "config_text") == '// generated by quake, do not modify\nprofile "classic"\n', text(pg, "config_text"))
    departures = ["wasm_uncapped", "wasm_scaled2d", "vid_native", "fov_adapt", "crosshair", "freelook",
                  "cl_jumpswim", "vid_fkey", "wasm_showfps", "wasm_exactpersp"]
    vals = {n: cvar(pg, n) for n in departures + ["cl_forwardspeed"]}
    check("every departure off, cl_forwardspeed 200 (Always Run off)",
          all(vals[n] == "0" for n in departures) and vals["cl_forwardspeed"] == "200", str(vals))
    pg.evaluate("quake.callLine('exec bind w; bind a')")
    lines = pg.evaluate("quake.text('console_text')").splitlines()[-2:]
    check("id's keys: w unbound, a +lookup", lines == ['"w" is not bound', '"a" = "+lookup"'], str(lines))
    check("the 72 fps cap: 72 frames in a second at 144 Hz", 71 <= pg.evaluate(second_at_144) <= 73)
    walk(pg)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("the 960x600 mode in the 4:3 box", (c["w"], c["h"]) == (960, 600) and not c["flags"] & (NATIVE | FKEY)
          and abs(c["cssW"] / c["cssH"] - 4 / 3) < 0.01, str(c))
    pg.evaluate(GRAB, "_c0")
    pg.evaluate("quake.callLine('exec crosshair 1')")
    pg.evaluate(GRAB, "_c1")
    check("no crosshair (crosshair 1 adds one)", pg.evaluate(DIFF, ["_c0", "_c1"]) is not None)
    pg.evaluate("quake.callLine('exec crosshair 0')")
    pg.evaluate("quake.resume()")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_settings_classic.png"))
    frames(pg)
    boot(pg)
    check("a plain reload after ?classic stays Classic", text(pg, "profile") == "classic")
    ctx.close()

    # 6b. A returning visitor of the old page: its localStorage settings.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    ctx.add_init_script("""if (!sessionStorage.getItem('seeded')) { sessionStorage.setItem('seeded', '1');
        localStorage.setItem('quake-rs.resolution', '960x600'); localStorage.setItem('quake-rs.viewsize', '100');
        localStorage.setItem('quake-rs.extras', '2'); }""")
    pg = page(ctx)
    boot(pg)
    ext = pg.evaluate("exp.extras()")
    check("old settings: the choice kept (Show FPS), the 2026 defaults on (uncapped, scaled 2-D)",
          text(pg, "profile") == "2026" and ext == 11, f"extras {ext}; {text(pg, 'config_text')!r}")
    ctx.close()

    # 7. Video Options is honest about native resolution, and reversible.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    pg = page(ctx)
    boot(pg)
    walk(pg)
    frames(pg)
    cursor = lambda: pg.evaluate("exp.menu_cursor()")
    c = pg.evaluate(CANVAS)
    check("walking in: native resolution is actually on", bool(c["flags"] & NATIVE), str(c))

    pg.keyboard.press("Escape")
    pg.keyboard.press("ArrowDown"); pg.keyboard.press("ArrowDown"); pg.keyboard.press("Enter")  # -> Options
    for _ in range(ROW_VIDEO):
        pg.keyboard.press("ArrowDown")
    pg.keyboard.press("Enter")  # -> Video Options
    time.sleep(0.2)
    check("Video Options opens on the live native row (Auto), not a stale preset",
          cursor() == VIDEO_PRESETS, f"cursor={cursor()}, pixel_size={cvar(pg, 'vid_pixelsize')}")
    check("...and native resolution is still on (no silent change just from opening)",
          cvar(pg, "vid_native") == "1")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_settings_video_native.png"))

    # Picking a fixed mode (3 Up from the Auto row: preset index 3, 800x500)
    # turns native off, visibly — a preset is now the one Enter marked.
    for _ in range(VIDEO_PRESETS - 3):
        pg.keyboard.press("ArrowUp")
    pg.keyboard.press("Enter")
    time.sleep(0.2)
    check("picking a mode turns native off, visibly", cvar(pg, "vid_native") == "0")
    c = pg.evaluate(CANVAS)
    check("...and the picture is that mode (800x500)", (c["w"], c["h"]) == (800, 500), str(c))
    check("still on the Video Options list", pg.evaluate("exp.menu_screen_id()") == 7)

    # Reversible: wrapping Up past the first preset reaches a native row;
    # Enter there turns native back on, at that row's pixel size.
    for _ in range(4):
        pg.keyboard.press("ArrowUp")
    cur = int(cursor())
    check("wrapped up into a native row", cur >= VIDEO_PRESETS, f"cursor={cur}")
    pg.keyboard.press("Enter")
    time.sleep(0.2)
    check("native resolution chosen back", cvar(pg, "vid_native") == "1")
    check("...at the row's own pixel size", cvar(pg, "vid_pixelsize") == str(cur - VIDEO_PRESETS))
    for _ in range(3):
        pg.keyboard.press("Escape")
    ctx.close()

    print("errors:", errs[-5:])
    check("no console errors", not errs)
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
