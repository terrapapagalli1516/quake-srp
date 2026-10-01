#!/usr/bin/env -S uv run --with playwright --script
"""Verify the full menu is LIVE end-to-end in headless Chromium:

  1. boot lands in the attract demo with no menu; a key brings the menu up;
     arrow navigation queues real menu sounds (the window.__menuSounds
     counter increments);
  2. every Main row responds: Single Player (Load list + the Save no-game
     gate), Multiplayer (screen opens, Esc returns), Options, Help, Quit (N
     backs out);
  3. Options rows act: sliders move real cvars (mouse_sensitivity / volume /
     viewsize — Screen size is id's viewsize, it never resizes the
     framebuffer), Go-to-console opens the console, Reset-to-defaults
     restores, Customize controls opens the Keys screen (bind grab works),
     Video Options applies a resolution (WinQuake's M_Video mode list);
  4. walk-mode behaviors: BRIGHTNESS visibly brightens the canvas (gamma LUT)
     and restores byte-fair at 1.0; ALWAYS RUN (on by default in this port)
     toggles the measured displacement between the run and walk speeds;
     INVERT MOUSE flips the pitch sign; LOOKSPRING recentres on pointer
     unlock; a REBOUND key drives +forward and the old key stops; Save opens
     in-game and Enter closes (SaveSlot host no-op);
  5. review-fix regressions: shift-variant punctuation resolves by e.code
     (Shift+',' still strafes — keynum 44 — and a shifted release can't stick
     the key), and a re-boot / New Game keeps the live options + key rebinds
     (Menu::reset_nav resets navigation only);
  6. the resolution picked in Video Options survives a page reload (the
     program's config.cfg, kept in the page's IndexedDB), and there are no
     console errors anywhere.

Usage: verify_menu.py [webdir]   (defaults to the repo's web/; pass a deploy
dir — PLATFORM.md — to test changes without touching the deployed page).

`exp.name()` asks the program (a Promise, answered between two frames).
"""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8173)
httpd = isolated.serve(WEB, PORT)

# menu_screen_id values (the `menu_screen_id` call's mapping).
MAIN, SP, LOAD, SAVE, MULTI, OPTIONS, KEYS, VIDEO, HELP, QUIT = range(10)
VIDEO_PRESETS = 7  # quake_rs::menu::RESOLUTION_PRESETS.len(): the native rows follow these

passed, failed = 0, 0
def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok: passed += 1
    else: failed += 1

with sync_playwright() as p:
    br = isolated.launch(p, [
        "--no-sandbox",
        # Let audioCtx.resume() succeed without a user gesture so the menu
        # sound drain actually runs under headless.
        "--autoplay-policy=no-user-gesture-required",
    ])
    pg = br.new_page(viewport={"width": 820, "height": 540})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)

    scr = lambda: pg.evaluate("exp.menu_screen_id()")
    vis = lambda: pg.evaluate("exp.menu_visible()")
    key = lambda k, n=1: [pg.keyboard.press(k) or time.sleep(0.06) for _ in range(n)]

    # Resume audio (the autoplay flag lets it run without a gesture) so the
    # page's menu-sound drain counts pops.
    pg.evaluate("""() => {
        audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
        return audioCtx.resume();
    }""")
    time.sleep(0.4)

    # 0. First-gesture overlay (merged webui-input behavior): the click-to-play
    #    scrim is up at boot and would consume the first Enter/Space as the
    #    start gesture. Dismiss it with a click so the keys below drive the
    #    MENU, like a player who already started.
    pg.evaluate("document.getElementById('overlay').click()")
    time.sleep(0.4)

    # 1. Attract boot: the demo plays with no menu (key_dest starts at
    #    key_game); any key brings up the main menu (Key_Event during demo
    #    playback); arrows queue menu1.
    check("attract boot: the demo, no menu", vis() == 0)
    key("Space")
    check("a key brings up the main menu", vis() == 1 and scr() == MAIN)
    snd0 = pg.evaluate("window.__menuSounds")
    key("ArrowDown"); key("ArrowUp")
    time.sleep(0.4)
    snd1 = pg.evaluate("window.__menuSounds")
    check("menu navigation queues local sounds", snd1 > snd0, f"{snd0} -> {snd1}")

    # 2a. Single Player > Save is GATED in attract (no local game running);
    #     Load opens its 12-slot list and an unused slot refuses Enter.
    key("Enter")            # Main item 0 -> SinglePlayer
    check("Single Player opens", scr() == SP)
    key("ArrowDown", 2); key("Enter")
    check("Save refuses without a running game", scr() == SP)
    key("ArrowUp"); key("Enter")
    check("Load opens its slot list", scr() == LOAD)
    key("ArrowDown", 3); key("Enter")
    check("Enter on an unused slot stays put", scr() == LOAD and vis() == 1)
    key("Escape")
    check("Esc on Load returns to Single Player", scr() == SP)
    key("Escape")

    # 2b. Multiplayer: the screen opens (mp_menu art + the no-comms line),
    #     Enter responds without going anywhere, Esc returns.
    key("ArrowDown"); key("Enter")
    check("Multiplayer opens its screen", scr() == MULTI)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_menu_multi.png"))
    key("ArrowDown"); key("Enter")
    check("Multiplayer Enter responds in place (no net)", scr() == MULTI)
    # Setup (row 2) is M_Menu_Setup_f: its own screen; type into the name and
    # step a colour, then Escape back without accepting.
    key("ArrowDown"); key("Enter")
    check("Multiplayer > Setup opens", scr() == 11)
    key("ArrowUp", 3); key("Backspace"); key("x"); key("ArrowDown"); key("ArrowRight")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_menu_setup.png"))
    key("Escape")
    check("Esc on Setup returns to Multiplayer", scr() == MULTI)
    key("Escape")
    check("Esc on Multiplayer returns to Main", scr() == MAIN)

    # 2c. Help pages; Quit prompt answers N. (Every menu keeps its own cursor,
    # menu.c's m_main_cursor & co: Esc from Multiplayer left Main on row 1.)
    key("ArrowDown", 2); key("Enter")
    check("Help opens", scr() == HELP)
    key("ArrowRight", 2); key("Escape")
    key("ArrowDown", 1); key("Enter")   # Main still on Help (row 3): Quit
    check("Quit raises the confirm prompt", scr() == QUIT)
    # M_Quit_Key answers only y/Y and n/N/Esc: Enter and the console key do
    # nothing (the console key over the menu is M_Keydown's, not a console).
    key("Enter"); key("`")
    check("Enter and ` leave the Quit prompt up",
          scr() == QUIT and vis() == 1 and pg.evaluate("exp.console_visible()") == 0)
    key("n")
    check("N answers the Quit prompt", scr() == MAIN and vis() == 1)

    # 3. Options rows. (Answering "No" restored the screen with the cursor
    # still on Quit — the C keeps m_main_cursor — so go UP two to Options.)
    key("ArrowUp", 2); key("Enter")
    check("Options opens", scr() == OPTIONS)

    # Mouse Speed (row 5) moves the sensitivity cvar.
    sens0 = pg.evaluate("exp.mouse_sensitivity()")
    key("ArrowDown", 5); key("ArrowRight", 2)
    sens1 = pg.evaluate("exp.mouse_sensitivity()")
    check("Mouse Speed slider moves the cvar", sens1 > sens0, f"{sens0:.2f} -> {sens1:.2f}")
    # Sound Volume (row 7).
    key("ArrowDown", 2)
    vol0 = pg.evaluate("exp.volume()")
    key("ArrowLeft", 2)
    vol1 = pg.evaluate("exp.volume()")
    check("Sound Volume slider moves the cvar", vol1 < vol0, f"{vol0:.2f} -> {vol1:.2f}")
    # CD Music Volume (row 6) + the four checkboxes (rows 8-11) all respond
    # (each adjust queues menu3 — count the sounds).
    sndA = pg.evaluate("window.__menuSounds")
    key("ArrowUp")          # row 6 (CD volume)
    key("ArrowLeft")
    key("ArrowDown", 2)     # row 8 always run
    key("ArrowRight")
    key("ArrowDown"); key("ArrowRight")   # row 9 invert
    key("ArrowDown"); key("ArrowRight")   # row 10 lookspring
    key("ArrowDown"); key("ArrowRight")   # row 11 lookstrafe
    # ...and toggle the four back off for a clean slate.
    key("ArrowRight"); key("ArrowUp"); key("ArrowRight")
    key("ArrowUp"); key("ArrowRight"); key("ArrowUp"); key("ArrowRight")
    time.sleep(0.4)
    sndB = pg.evaluate("window.__menuSounds")
    check("every slider/checkbox row responds audibly", sndB - sndA >= 12, f"+{sndB - sndA}")

    # Screen size (row 3) is viewsize: left/right step it by 10 (clamped
    # 30..120) and the framebuffer size never changes.
    w0 = pg.evaluate("exp.width()")
    key("ArrowUp", 5)       # from row 8 back to row 3
    check("viewsize defaults to 100", pg.evaluate("exp.viewsize()") == 100)
    key("ArrowLeft", 2)
    check("Screen size row steps viewsize", pg.evaluate("exp.viewsize()") == 80,
          f"{pg.evaluate('exp.viewsize()')}")
    key("ArrowRight", 6)
    check("...clamped at 120", pg.evaluate("exp.viewsize()") == 120)
    check("...and never resizes the framebuffer", pg.evaluate("exp.width()") == w0)
    key("ArrowLeft", 3)     # 90

    # Reset to defaults (row 2) restores the cvars.
    key("ArrowUp"); key("Enter")
    check("Reset to defaults restores sensitivity",
          abs(pg.evaluate("exp.mouse_sensitivity()") - 1.0) < 1e-5)
    check("Reset to defaults restores viewsize 100 (default.cfg)",
          pg.evaluate("exp.viewsize()") == 100)

    # Customize controls (row 0): the Keys screen + a bind grab that Escape
    # cancels (full rebinding is proven in walk mode below).
    key("ArrowUp", 2); key("Enter")
    check("Customize controls opens the Keys screen", scr() == KEYS)
    key("ArrowDown", 2)
    check("not grabbing before Enter", pg.evaluate("exp.menu_bind_grabbing()") == 0)
    key("Enter")
    check("Enter starts the bind grab", pg.evaluate("exp.menu_bind_grabbing()") == 1)
    key("Escape")
    check("Escape cancels the grab on the Keys screen",
          pg.evaluate("exp.menu_bind_grabbing()") == 0 and scr() == KEYS)
    # The mouse buttons are keys to bind (K_MOUSE1..3): the page forwards a
    # click while the grab waits ("change weapon", row 1: one key, so the
    # grab adds MOUSE2 to it without unbinding '/').
    key("ArrowUp"); key("Enter")
    pg.locator("#c").click(button="right")
    time.sleep(0.1)
    check("a right click is the key a grab binds (MOUSE2)",
          pg.evaluate("exp.menu_bind_grabbing()") == 0 and scr() == KEYS and vis() == 1)
    key("ArrowDown")
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_menu_keys.png"))
    key("Escape")
    check("Esc on Keys returns to Options", scr() == OPTIONS)

    # Video Options (row 12): honest about native resolution (review: it used
    # to show 960x600 as current no matter what the screen actually was). In
    # this real browser the picture is already native by the time the menu
    # reaches here (the page reports its box on layout, no gesture needed),
    # so the list opens on the live native row (Auto), not a stale preset.
    # Four Ups reach 800x500 (preset index 3); applying it is kept for the
    # reload check at the end.
    key("ArrowDown", 12); key("Enter")
    check("Video Options opens the mode list", scr() == VIDEO)
    cur0 = pg.evaluate("exp.menu_cursor()")
    check("...on the live native row (Auto), not a stale preset", cur0 == VIDEO_PRESETS, f"cursor={cur0}")
    key("ArrowUp", VIDEO_PRESETS - 3)
    check("moving the line alone keeps the picture", pg.evaluate("exp.width()") == w0)
    check("...native resolution is still on", pg.evaluate("quake.text('cvar', 'vid_native')") == "1")
    key("Enter")
    time.sleep(0.2)
    wv, hv = pg.evaluate("Promise.all([exp.width(), exp.height()])")
    check("Enter applies the highlighted mode", (wv, hv) == (800, 500), f"{w0} -> {wv}x{hv}")
    check("...and native resolution is visibly off now", pg.evaluate("quake.text('cvar', 'vid_native')") == "0")
    # The program writes config.cfg on the next frame (Host_WriteConfiguration)
    # and the page keeps it.
    try:
        pg.wait_for_function("quake.kept('id1/config.cfg').then(t => !!t && t.includes('_vid_resolution 800x500'))",
                             timeout=5000)
        kept = True
    except Exception:
        kept = False
    check("config.cfg keeps it", kept, str(pg.evaluate("quake.kept('id1/config.cfg')")))
    check("the mode never touches viewsize", pg.evaluate("exp.viewsize()") == 100)
    # Reversible: wrapping Up from the first preset reaches a native row
    # (Enter there would turn native back on) — not pressed, so the applied
    # 800x500 survives for the reload check below (verify_settings.py's
    # section 7 presses Enter there and checks the full round trip).
    key("ArrowUp", 4)  # row 3 -> row 0 -> wraps to the last row (native, 4x)
    cur = pg.evaluate("exp.menu_cursor()")
    check("wrapping up from the first preset reaches a native row", cur >= VIDEO_PRESETS, f"cursor={cur}")
    key("Escape")
    check("Esc on Video returns to Options", scr() == OPTIONS)

    # Go to console (row 1) opens the drop-down console. (Esc from Video left
    # options_cursor on Video Options, row 12: down past Classic / 2026 wraps to 1.)
    key("ArrowDown", 3)
    key("Enter")
    check("Go to console opens the console",
          pg.evaluate("exp.console_visible()") == 1 and vis() == 0)
    key("Backquote")        # close the console

    # 4. Walk mode behaviors.
    pg.evaluate("document.getElementById('walkBtn').click()")
    time.sleep(1.0)
    key("Escape")           # close the boot menu
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
    time.sleep(0.3)

    grab_lum = """() => {
        const c = document.getElementById('c');
        const d = quake.readback();
        let s = 0, n = 0;
        for (let i = 0; i < d.length; i += 16) { s += d[i] + d[i+1] + d[i+2]; n += 3; }
        return s / n;
    }"""

    # BRIGHTNESS: gamma 0.6 visibly brightens the canvas; 1.0 restores.
    lum1 = pg.evaluate(grab_lum)
    key("Escape")           # open menu
    key("ArrowDown", 2); key("Enter")     # Options
    key("ArrowDown", 4)                   # Brightness row
    key("ArrowRight", 8)                  # gamma 1.0 -> 0.6
    key("Escape"); key("Escape")          # close
    time.sleep(0.3)
    lum2 = pg.evaluate(grab_lum)
    check("gamma 0.6 visibly brightens the frame", lum2 > lum1 * 1.10,
          f"mean {lum1:.1f} -> {lum2:.1f}")
    # The menu reopens on "Options", and Options on Brightness (kept cursors).
    key("Escape"); key("Enter")
    key("ArrowLeft", 8)
    key("Escape"); key("Escape")
    time.sleep(0.3)
    lum3 = pg.evaluate(grab_lum)
    check("gamma 1.0 restores the brightness", abs(lum3 - lum1) < lum1 * 0.05,
          f"mean back to {lum3:.1f}")

    # ALWAYS RUN: displacement per second drops when it is toggled OFF (this
    # port defaults it ON and the checkbox pass above left it on; Reset to
    # defaults never touches it: 400 -> server-clamped 320 vs the 200 walk).
    # Turn the player 180 between runs (1125 counts * 0.16 deg) so each run
    # retraces the same free corridor instead of piling into a wall.
    turn_around = lambda: pg.evaluate("exp.mouse_move(1125, 0)")
    def walk_dist(secs, keyname="w"):
        x0, y0 = pg.evaluate("Promise.all([exp.listener_x(), exp.listener_y()])")
        pg.keyboard.down(keyname); time.sleep(secs); pg.keyboard.up(keyname)
        time.sleep(0.2)
        x1, y1 = pg.evaluate("Promise.all([exp.listener_x(), exp.listener_y()])")
        return ((x1 - x0) ** 2 + (y1 - y0) ** 2) ** 0.5
    d_run = walk_dist(1.0)
    key("Escape"); key("Enter")                   # Options, on Brightness (4)
    key("ArrowDown", 4); key("ArrowRight")        # Always Run off
    key("Escape"); key("Escape")
    turn_around()
    d_walk = walk_dist(1.0)
    check("Always Run (default on) runs faster than walking", d_run > d_walk * 1.25,
          f"run {d_run:.0f}u vs walk {d_walk:.0f}u per 1.0s")

    # INVERT MOUSE: the same mouse-down delta flips the pitch sign.
    p0 = pg.evaluate("exp.player_pitch()")
    pg.evaluate("exp.mouse_move(0, 300)")
    p1 = pg.evaluate("exp.player_pitch()")
    check("mouse-down looks down by default", p1 > p0, f"{p0:.1f} -> {p1:.1f}")
    key("Escape"); key("Enter")                   # Options, on Always Run (8)
    key("ArrowDown"); key("ArrowRight")           # Invert Mouse on
    key("Escape"); key("Escape")
    pg.evaluate("exp.mouse_move(0, 300)")
    p2 = pg.evaluate("exp.player_pitch()")
    check("Invert Mouse flips the pitch direction", p2 < p1, f"{p1:.1f} -> {p2:.1f}")

    # LOOKSPRING: pointer unlock recentres the pitch (the page calls
    # pointer_unlocked from pointerlockchange; headless can't lock, so drive
    # the same hook directly).
    key("Escape"); key("Enter")                   # Options, on Invert Mouse (9)
    key("ArrowDown"); key("ArrowRight")           # Lookspring on
    key("Escape"); key("Escape")
    pg.evaluate("exp.mouse_move(0, -400)")        # look well off-centre
    # (Invert Mouse is still ON from the previous check, so the sign is
    # flipped — only the magnitude matters here.)
    pp = pg.evaluate("exp.player_pitch()")
    pg.evaluate("exp.pointer_unlocked()")
    time.sleep(1.0)
    pr = pg.evaluate("exp.player_pitch()")
    check("Lookspring recentres on pointer unlock", abs(pr) < 1.0 and abs(pp) > 20,
          f"{pp:.1f} -> {pr:.1f}")

    # REBIND: Customize controls really rebinds +forward (row 3) to 'o'.
    key("Escape"); key("Enter")                   # Options, on Lookspring (10)
    key("ArrowDown", 4); key("Enter")             # past Classic / 2026 to Customize controls
    key("ArrowDown", 3); key("Enter")             # grab on +forward
    pg.keyboard.press("o"); time.sleep(0.1)
    check("the grab bound the new key", pg.evaluate("exp.menu_bind_grabbing()") == 0)
    key("Escape"); key("Escape"); key("Escape")   # Keys -> Options -> Main -> closed
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=5000)
    turn_around()                                 # retrace the free corridor
    d_new = walk_dist(0.7, "o")
    time.sleep(0.9)                               # let friction stop the coast
    d_old = walk_dist(0.7, "w")
    check("the rebound key walks forward", d_new > 80, f"{d_new:.0f}u")
    check("the old key was unbound by the two-key rule", d_old < 20, f"{d_old:.0f}u")

    # SAVE opens in-game; Enter saves to the slot (s0.sav) and closes.
    key("Escape"); key("ArrowUp", 2); key("Enter")  # Main (on Options) -> SinglePlayer
    key("ArrowDown", 2); key("Enter")
    check("Save opens with a game running", scr() == SAVE)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_menu_save.png"))
    key("Enter")
    check("Save Enter closes the menu", vis() == 0)

    # 5. Review-fix regressions.
    # 5a. SHIFT-VARIANT PUNCTUATION: the page resolves punctuation keynums from
    #     e.code (the C's scancode semantics, in_win.c scantokey), so the seeded
    #     default.cfg layout works while RUNNING (+speed is Shift): Shift+','
    #     still delivers keynum 44 (+moveleft), and a release whose e.key
    #     reports '<' still clears it (no stuck strafe).
    kb = pg.keyboard
    kb.down("Shift"); time.sleep(0.05)
    kb.down("Comma"); time.sleep(0.1)          # e.key is '<' here; e.code Comma
    check("Shift+',' delivers keynum 44 (strafe works while running)",
          pg.evaluate("exp.key_is_down(44)") == 1)
    kb.up("Comma"); time.sleep(0.05)
    check("...and its release clears it", pg.evaluate("exp.key_is_down(44)") == 0)
    kb.up("Shift"); time.sleep(0.05)
    # The stuck-key order: ',' down, ADD Shift, release ',' (keyup says '<').
    kb.down("Comma"); time.sleep(0.05)
    kb.down("Shift"); time.sleep(0.05)
    kb.up("Comma"); time.sleep(0.05)
    check("a shifted release can't stick the comma strafe",
          pg.evaluate("exp.key_is_down(44)") == 0)
    kb.up("Shift"); time.sleep(0.05)
    check("Shift (+speed) itself releases clean",
          pg.evaluate("exp.key_is_down(134)") == 0)

    # 5b. RE-BOOT KEEPS USER CHOICES (Menu::reset_nav): set Mouse speed
    #     off-default, then re-boot via the walk button — the flow that used to
    #     rebuild the Menu wholesale — and then New Game; the cvar, the 'o'
    #     rebind, and the unbinding of 'w' must all survive (WinQuake's
    #     `map start` never resets cvars or keybindings).
    key("Escape"); key("ArrowDown", 2); key("Enter")   # Options
    key("ArrowDown", 5); key("ArrowRight", 2)          # Mouse speed +2 steps
    sens_set = pg.evaluate("exp.mouse_sensitivity()")
    key("Escape"); key("Escape")
    # default.cfg binds '-' to sizedown and '=' to sizeup (in the game only).
    key("Minus", 2); key("Equal")
    check("'-' / '=' step viewsize in the game", pg.evaluate("exp.viewsize()") == 90,
          f"{pg.evaluate('exp.viewsize()')}")
    pg.evaluate("document.getElementById('walkBtn').click()")  # re-boot e1m1
    time.sleep(1.2)
    check("re-boot reopens the menu at Main", vis() == 1 and scr() == MAIN)
    check("Mouse speed survives the re-boot", sens_set > 1.0 and
          abs(pg.evaluate("exp.mouse_sensitivity()") - sens_set) < 1e-5,
          f"{sens_set:.2f}")
    key("Enter")            # Single Player
    key("Enter")            # New Game: a game runs, so SCR_ModalMessage asks
    check("New Game in a running game asks first (menu stays)",
          vis() == 1 and scr() == SP)
    key("y")                # "Are you sure?" -> y: start.bsp, menu closes
    pg.wait_for_function("exp.menu_visible().then(v => !v)", timeout=15000)
    check("New Game keeps the Mouse speed cvar",
          abs(pg.evaluate("exp.mouse_sensitivity()") - sens_set) < 1e-5)
    check("New Game keeps viewsize", pg.evaluate("exp.viewsize()") == 90)
    time.sleep(0.5)
    d_reb = walk_dist(0.7, "o")
    check("the rebound +forward key survives re-boot + New Game", d_reb > 80,
          f"{d_reb:.0f}u")
    time.sleep(0.9)                               # let friction stop the coast
    d_w2 = walk_dist(0.7, "w")
    check("'w' stays unbound (bindings aren't reseeded)", d_w2 < 20,
          f"{d_w2:.0f}u")

    # 6. The Video Options mode survives a reload (the program execs the
    #    config.cfg it wrote, before its first frame).
    pg.reload(wait_until="load")
    pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=120000)
    check("the Video Options mode survives a reload",
          pg.evaluate("Promise.all([exp.width(), exp.height()])") == [800, 500],
          f"{pg.evaluate('Promise.all([exp.width(), exp.height()])')}")
    check("...and the canvas backing store follows it",
          pg.evaluate("[document.getElementById('c').width, document.getElementById('c').height]")
          == [800, 500])

    # 7. Console must be clean.
    print("errors:", errs[-5:])
    check("no console errors", not errs)
    br.close()
httpd.shutdown()

print(f"done: {passed} passed, {failed} failed")
if failed:
    raise SystemExit(1)
