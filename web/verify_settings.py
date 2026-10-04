#!/usr/bin/env -S uv run --with playwright --with pillow --script
"""Verify the two presets and the settings, end-to-end in headless Chromium:

  1. The default is the slop preset, and the picture fills the window:
     at 1280x800 (devicePixelRatio 1) the page's box is 1246x716 (the window
     less the chrome), the frame 1246x716 at one device pixel a pixel (a
     desktop's 1x), the canvas that box exactly; any window shape (1600x600:
     a 1566x516 frame). The State says native, and the pixel size.
  2. Whole pixels: at devicePixelRatio 2 the box is 2492x1432 device
     pixels, and a desktop still draws it at 1x (no guess from the ratio:
     the machine's number); vid_pixelsize 2 gives a 1246x716 frame, the
     canvas 2492 device pixels wide — every frame pixel a whole 2x2 square of
     the screenshot, never smoothed; vid_pixelsize 3 gives 830x477 at 3x3.
  3. The slop frame: the slop cross centred on the view (crosshair 1 and 0
     differ only there), id's + crossing the same centre at the 2-D scale
     (crosshair 2), the perspective span at 8 (r_perspspan 8; id's 16 and
     exact redraw the walls, and 8 again restores the frame), no 72 fps cap (a second of 1/144 s steps runs 144
     frames), and W walks.
  4. `?classic` is Classic: the preset, config.cfg with nothing changed from
     it, every engine slop option off (id's 72 fps cap: 72 frames in a
     second at 144 Hz; the video mode in the 4:3 box, 960x600; no
     crosshair), but the controls the same as slop's — WASD, mouse look,
     Space swims up, Alt+Enter, Always Run (cl_forwardspeed 400): W walks,
     the mouse looks, the wheel does nothing (it is slop's alone).
     `idcontrols` is the one step to id's own (`w` unbound, `a` +lookup,
     Always Run off).
  5. Options > "Reset to Classic" (its 15th row, up from the first) asks,
     and yes sets every slop option to Classic's: the canvas goes from the
     window to the 4:3 box, a slop control set by hand goes back to the
     preset's, the keys stay (WASD bound, a rebind kept) but the wheel,
     Screen size goes with it while the player has not moved it (110 in
     slop, the status bar alone; id's 100 in Classic, with the inventory
     bar). The console's `preset slop` brings slop back, keys kept.
  6. The settings survive a reload: a slop session's pixel size, a binding
     and Screen size; a `?classic` visit sticks for a plain reload after it;
     a returning visitor's old localStorage settings keep their choice (Show
     FPS) and get the slop values (no cap, scaled 2-D, the span at 8, and
     Screen size 110: the old page's viewsize 100 was its default).
  7. Video Options is honest about native resolution (review: it used to show
     960x600 as current and a pick silently turned Native off): opened while
     actually native, the cursor lands on the live native row (1x, not a
     stale preset) and `vid_native` is still 1; picking a fixed mode turns it
     off, visibly (that mode is now the one marked); and native is one
     Up/Enter away again, at the picked pixel size — the same list, never a
     dead end.

Screenshots (in the web dir): verify_settings_2026.png, verify_settings_classic.png,
verify_settings_video_native.png.
Also passes with QUAKE_BROWSER=firefox, where Playwright loses the context's
devicePixelRatio on a cross-origin isolated page (this one; a plain page
keeps it): section 2 is skipped there. Not verified: a real high-DPI
screen, a GPU compositor, Safari.

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
NATIVE, ALT_ENTER = 32, 64
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
    isolated.wait_until(pg, "exp.menu_visible().then(v => !v)", 5)

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
def bind_of(pg, key):
    """The console's answer to `bind KEY`: '"w" = "+forward"', or "is not bound"."""
    pg.evaluate(f"quake.callLine('exec bind {key}')")
    return pg.evaluate("quake.text('console_text')").splitlines()[-1]
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

    # 1. The slop default fills the window.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    pg = page(ctx)
    boot(pg)
    check("the default is the slop preset", text(pg, "preset") == "slop")
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

    # 3. The slop frame: the crosshair, no cap, W walks.
    pg.set_viewport_size({"width": 1280, "height": 800})
    pg.wait_for_function("document.getElementById('c').width === 1246", timeout=5000)
    pg.evaluate(GRAB, "_x1")
    pg.evaluate("quake.callLine('exec crosshair 0')")
    pg.evaluate(GRAB, "_x0")
    pg.evaluate("quake.callLine('exec crosshair 1')")
    d = pg.evaluate(DIFF, ["_x1", "_x0"])
    # Slop starts Screen size at 110: the status bar alone, 24 rows at the
    # 2-D layer's 3x on a 1246x716 screen, 72 rows, and no inventory strip.
    bar = pg.evaluate("quake.callLine('sbar_height').then(r => r.value)")
    check("Screen size starts at 110 in slop: the status bar alone, 24 rows at 3x",
          cvar(pg, "viewsize") == "110" and bar == 72, f"viewsize {cvar(pg, 'viewsize')}, {bar} rows")
    # The view is above the status bar: its centre pixel (623, cy) with cy
    # = (716 - 72) / 2 = 322 (at id's 100 the bar is 144 rows and cy 286).
    # The cross on 716 rows: 1-pixel arms 5 long, 1 from the open centre
    # pixel, so 617..629 x cy-6..cy+6, and its black outline one further
    # (where the scene is not already black).
    cy = (716 - bar) // 2
    ok = d is not None and 616 <= d["x0"] <= 617 and 629 <= d["x1"] <= 630 and cy - 7 <= d["y0"] <= cy - 6 and cy + 6 <= d["y1"] <= cy + 7
    check("the crosshair: the slop cross about the view's centre, and nothing else", ok, f"{d}; centre row {cy}")
    # crosshair 2: id's + at the 2-D scale (3), its crossing on the centre:
    # the 8x8 cell's corner at (623 - 4*3, cy - 4.5*3) = (611, cy - 13), the
    # glyph's grey texels (columns 1-6, rows 2-6) 614..632 x cy-7..cy+7, its
    # dark shadow to 634 x cy+10.
    pg.evaluate("quake.callLine('exec crosshair 2')")
    pg.evaluate(GRAB, "_x2")
    pg.evaluate("quake.callLine('exec crosshair 1')")
    d = pg.evaluate(DIFF, ["_x2", "_x0"])
    ok = d is not None and (d["x0"], d["y0"]) == (614, cy - 7) and 632 <= d["x1"] <= 634 and cy + 7 <= d["y1"] <= cy + 10
    check("crosshair 2: id's + at the 2-D scale, crossing the view's centre", ok, str(d))
    # The perspective is found every 8 pixels in slop (id's portable C loop;
    # at 1080p and above id's 16-pixel spans wobble along a wall seen at a
    # grazing angle); id's 16 redraws the walls, exact does too, and 8 again
    # is the same frame.
    check("the perspective span is 8 in slop (r_perspspan 8, not exact)",
          cvar(pg, "r_perspspan") == "8" and cvar(pg, "wasm_exactpersp") == "0")
    pg.evaluate(GRAB, "_p8")
    pg.evaluate("quake.callLine('exec r_perspspan 16')")
    pg.evaluate(GRAB, "_p16")
    pg.evaluate("quake.callLine('exec r_perspspan 1')")
    pg.evaluate(GRAB, "_p1")
    pg.evaluate("quake.callLine('exec r_perspspan 8')")
    pg.evaluate(GRAB, "_p8b")
    d = pg.evaluate(DIFF, ["_p8", "_p16"])
    d1 = pg.evaluate(DIFF, ["_p8", "_p1"])
    check("...id's 16 and exact redraw the walls, 8 again is the same frame, byte for byte",
          d is not None and d["n"] > 1000 and d1 is not None and d1["n"] > 1000
          and pg.evaluate(DIFF, ["_p8", "_p8b"]) is None, f"16: {d}; exact: {d1}")
    pg.evaluate("quake.resume()")
    check("no frame cap (host_maxfps 0): 144 frames in a second at 144 Hz",
          cvar(pg, "host_maxfps") == "0" and pg.evaluate(second_at_144) == 144)
    x0 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    pg.keyboard.down("w"); time.sleep(1.0); pg.keyboard.up("w")
    x1 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    check("W walks", abs(x1[0] - x0[0]) + abs(x1[1] - x0[1]) > 50, f"{x0} -> {x1}")

    # 5. Options > Reset to Classic (asks; y), and the console back to slop.
    pg.evaluate("quake.callLine('exec cl_jumpswim 0; bind j \"+jump\"')")   # a slop control by hand; a key
    pg.keyboard.press("Escape")
    pg.keyboard.press("ArrowDown"); pg.keyboard.press("ArrowDown"); pg.keyboard.press("Enter")
    pg.keyboard.press("ArrowUp")          # row 0 wraps to the 15th: Reset to Classic
    time.sleep(0.2)
    pg.keyboard.press("Enter")
    pg.wait_for_function("quake.state.flags & 256", timeout=5000)    # ST.ASK: the question
    check("Reset to Classic asks first", text(pg, "preset") == "slop" and pg.evaluate("quake.state.flags & 32") != 0)
    pg.keyboard.press("y")
    pg.wait_for_function("!(quake.state.flags & 32)", timeout=5000)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("yes: Classic, the 960x600 mode in the 4:3 box",
          text(pg, "preset") == "classic" and (c["w"], c["h"]) == (960, 600)
          and abs(c["cssW"] / c["cssH"] - 4 / 3) < 0.01, str(c))
    check("...the slop control back to the preset's, the keys kept (WASD, the rebind), the wheel off",
          cvar(pg, "cl_jumpswim") == "1" and cvar(pg, "freelook") == "1" and bind_of(pg, "w") == '"w" = "+forward"'
          and bind_of(pg, "j") == '"j" = "+jump"' and bind_of(pg, "MWHEELUP") == '"MWHEELUP" is not bound')
    check("...Screen size, never moved, follows to id's 100 (the inventory bar is back)",
          cvar(pg, "viewsize") == "100" and pg.evaluate("quake.callLine('sbar_height').then(r => r.value)") == 48,
          cvar(pg, "viewsize"))
    check("...and id's own 72 fps cap", cvar(pg, "host_maxfps") == "72" and 71 <= pg.evaluate(second_at_144) <= 73)
    pg.evaluate("quake.callLine('exec preset slop')")
    pg.wait_for_function("quake.state.flags & 32", timeout=5000)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("`preset slop`: slop again, the window filled", text(pg, "preset") == "slop" and c["w"] == 1246, str(c))
    check("...Screen size follows back to slop's 110", cvar(pg, "viewsize") == "110", cvar(pg, "viewsize"))
    check("...the keys kept, the wheel back (impulse 10)",
          bind_of(pg, "j") == '"j" = "+jump"' and bind_of(pg, "w") == '"w" = "+forward"'
          and bind_of(pg, "MWHEELUP") == '"MWHEELUP" = "impulse 10"')
    pg.evaluate("quake.callLine('exec unbind j')")
    for _ in range(3):
        pg.keyboard.press("Escape")

    # 6a. A slop session's settings survive a reload.
    pg.evaluate("quake.callLine('exec vid_pixelsize 2; bind j \"+jump\"; viewsize 90')")
    frames(pg)
    isolated.wait_until(pg, "quake.kept('id1/config.cfg').then(t => !!t && t.includes('vid_pixelsize \"2\"'))", 5)
    boot(pg)
    check("reloaded: the preset, the pixel size, the binding, Screen size",
          text(pg, "preset") == "slop" and cvar(pg, "vid_pixelsize") == "2" and cvar(pg, "viewsize") == "90"
          and "+jump" in pg.evaluate("quake.callLine('exec bind j').then(() => quake.text('console_text'))"),
          text(pg, "config_text"))
    ctx.close()

    # 2. Whole pixels at devicePixelRatio 2.
    ctx = br.new_context(viewport={"width": 1280, "height": 800}, device_scale_factor=2)
    pg = page(ctx)
    boot(pg)
    if pg.evaluate("devicePixelRatio") != 2:
        print("SKIP the devicePixelRatio 2 checks (this browser lost the context's device_scale_factor on the isolated page)")
    else:
        walk(pg)
        pg.wait_for_function("document.getElementById('c').width === 2492", timeout=5000)
        c = pg.evaluate(CANVAS)
        check("dpr 2: a 2492x1432 box, drawn at a desktop's 1x (the machine's number, no guess from the ratio)",
              (c["w"], c["h"], c["px"]) == (2492, 1432, 1) and cvar(pg, "vid_pixelsize") == "1", str(c))
        pg.evaluate("quake.callLine('exec vid_pixelsize 2')")
        pg.wait_for_function("document.getElementById('c').width === 1246", timeout=5000)
        c = pg.evaluate(CANVAS)
        check("vid_pixelsize 2: a 1246x716 frame, the canvas 2492 device pixels wide",
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
    check("?classic: the Classic preset", text(pg, "preset") == "classic")
    check("config.cfg: the preset and nothing changed from it",
          text(pg, "config_text") == '// generated by quake, do not modify\npreset "classic"\n', text(pg, "config_text"))
    # The engine: every slop option off. The controls are the same as
    # slop's: mouse look, Space swims up, Alt+Enter, Always Run (400).
    departures = ["wasm_uncapped", "wasm_scaled2d", "scr_sbaroverlay", "vid_native", "fov_adapt", "crosshair",
                  "wasm_showfps", "wasm_exactpersp"]
    controls = ["freelook", "cl_jumpswim", "vid_altenter", "joystick"]
    vals = {n: cvar(pg, n) for n in departures + controls + ["cl_forwardspeed"]}
    check("every engine departure off", all(vals[n] == "0" for n in departures) and cvar(pg, "r_perspspan") == "16", str(vals))
    check("Screen size is id's 100 (the page started 2026; the address switched it, never moved)",
          cvar(pg, "viewsize") == "100", cvar(pg, "viewsize"))
    check("the controls are slop's: mouse look, Space swims up, Alt+Enter, the gamepad, Always Run (400)",
          all(vals[n] == "1" for n in controls) and vals["cl_forwardspeed"] == "400", str(vals))
    pg.evaluate("quake.callLine('exec bind w; bind a; bind MWHEELUP')")
    lines = pg.evaluate("quake.text('console_text')").splitlines()[-3:]
    check("the keys: WASD bound, the wheel not (slop's alone)",
          lines == ['"w" = "+forward"', '"a" = "+moveleft"', '"MWHEELUP" is not bound'], str(lines))
    check("the 72 fps cap: 72 frames in a second at 144 Hz", 71 <= pg.evaluate(second_at_144) <= 73)
    walk(pg)
    frames(pg)
    c = pg.evaluate(CANVAS)
    check("the 960x600 mode in the 4:3 box, Alt+Enter on", (c["w"], c["h"]) == (960, 600)
          and not c["flags"] & NATIVE and c["flags"] & ALT_ENTER
          and abs(c["cssW"] / c["cssH"] - 4 / 3) < 0.01, str(c))
    pg.evaluate(GRAB, "_c0")
    pg.evaluate("quake.callLine('exec crosshair 1')")
    pg.evaluate(GRAB, "_c1")
    check("no crosshair (crosshair 1 adds one)", pg.evaluate(DIFF, ["_c0", "_c1"]) is not None)
    pg.evaluate("quake.callLine('exec crosshair 0')")
    pg.evaluate("quake.resume()")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_settings_classic.png"))
    frames(pg)

    # The Classic walk: W walks, the mouse looks (freelook, no +mlook held),
    # the wheel changes nothing.
    x0 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    pg.keyboard.down("w"); time.sleep(1.0); pg.keyboard.up("w")
    x1 = pg.evaluate("exp.listener_x()"), pg.evaluate("exp.listener_y()")
    check("Classic walk: W walks", abs(x1[0] - x0[0]) + abs(x1[1] - x0[1]) > 50, f"{x0} -> {x1}")
    pg.locator("#c").click(position={"x": 320, "y": 240})
    pg.wait_for_function("document.pointerLockElement === document.getElementById('c')", timeout=5000)
    time.sleep(0.5)                          # past the headless lock's own jump
    pitch = lambda: pg.evaluate("quake.callLine('player_pitch').then(r => r.value)")
    p0 = pitch()
    pg.evaluate("document.getElementById('c').dispatchEvent(new MouseEvent('mousemove', { movementY: 40 }))")
    time.sleep(0.3)
    p1 = pitch()
    check("Classic walk: the mouse looks up and down (freelook on, no +mlook held)", abs(p1 - p0) > 3, f"pitch {p0:.1f} -> {p1:.1f}")
    weapon = lambda: pg.evaluate("quake.callLine('player_field weapon').then(r => r.value)")
    def wait_weapon(want, timeout=5.0):
        deadline = time.time() + timeout
        w = weapon()
        while w != want and time.time() < deadline:
            time.sleep(0.05)
            w = weapon()
        return w
    pg.evaluate("quake.callLine('exec impulse 9')")
    wait_weapon(32)                          # every weapon: the rocket launcher
    pg.evaluate("quake.callLine('exec impulse 3')")
    w0 = wait_weapon(2)                      # the super shotgun, between two owned
    pg.mouse.move(640, 300)
    pg.mouse.wheel(0, -120); pg.mouse.wheel(0, -120); pg.mouse.wheel(0, 120)
    time.sleep(0.5)
    check("Classic walk: the wheel does nothing (slop's alone)", weapon() == w0 == 2, f"{w0:.0f} -> {weapon():.0f}")

    # idcontrols: the one step to id's own 1996 controls, the engine untouched.
    pg.evaluate("quake.callLine('exec idcontrols')")
    vals = {n: cvar(pg, n) for n in departures + controls + ["cl_forwardspeed"]}
    check("idcontrols: id's controls (mouse look, swim-up, Alt+Enter, gamepad off, Always Run off)",
          all(vals[n] == "0" for n in controls) and vals["cl_forwardspeed"] == "200", str(vals))
    check("...the engine untouched", all(vals[n] == "0" for n in departures) and text(pg, "preset") == "classic")
    check("...id's keys: w unbound, a +lookup",
          [bind_of(pg, "w"), bind_of(pg, "a")] == ['"w" is not bound', '"a" = "+lookup"'])
    boot(pg)
    check("a plain reload after ?classic stays Classic", text(pg, "preset") == "classic")
    ctx.close()

    # 6b. A returning visitor of the old page: its localStorage settings.
    ctx = br.new_context(viewport={"width": 1280, "height": 800})
    ctx.add_init_script("""if (!sessionStorage.getItem('seeded')) { sessionStorage.setItem('seeded', '1');
        localStorage.setItem('quake-rs.resolution', '960x600'); localStorage.setItem('quake-rs.viewsize', '100');
        localStorage.setItem('quake-rs.extras', '2'); }""")
    pg = page(ctx)
    boot(pg)
    ext = pg.evaluate("exp.extras()")
    check("old settings: the choice kept (Show FPS), the slop values on (no cap, scaled 2-D, the span at 8)",
          text(pg, "preset") == "slop" and ext == 11 and cvar(pg, "r_perspspan") == "8",
          f"extras {ext}; {text(pg, 'config_text')!r}")
    # The old page's viewsize 100 was its default, not a choice (verify_save.py
    # migrates a chosen 80, which stays): the player who never moved Screen
    # size gets slop's own start.
    check("old settings: their viewsize 100 was the old default, so Screen size is slop's 110",
          cvar(pg, "viewsize") == "110", cvar(pg, "viewsize"))
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
    check("Video Options opens on the live native row (1x), not a stale preset",
          cursor() == VIDEO_PRESETS, f"cursor={cursor()}, pixel_size={cvar(pg, 'vid_pixelsize')}")
    check("...and native resolution is still on (no silent change just from opening)",
          cvar(pg, "vid_native") == "1")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_settings_video_native.png"))

    # Picking a fixed mode (4 Up from the 1x row: preset index 3, 800x500)
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
    check("...at the row's own pixel size", cvar(pg, "vid_pixelsize") == str(cur - VIDEO_PRESETS + 1))
    for _ in range(3):
        pg.keyboard.press("Escape")
    ctx.close()

    print("errors:", errs[-5:])
    check("no console errors", not errs)
    br.close()
httpd.shutdown()
print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
