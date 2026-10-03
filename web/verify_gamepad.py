#!/usr/bin/env -S uv run --with playwright --script
"""Verify the gamepad end to end in a headless browser, with a synthetic pad
(`navigator.getGamepads` replaced before the page loads by one standard-mapped
pad this script moves, whose `vibrationActuator` records the rumble it is
asked for):

  1. THE FIRST GESTURE — a pad button takes the click-to-play scrim away.
  2. THE MENUS (2026, `joy_menukeys`) — Start opens the menu over the attract
     demo, the D-pad moves, A enters, B backs out, and A on Single Player >
     New Game starts the game.
  3. A WALK — the left stick walks the player through e1m1's start, the right
     stick turns the view.
  4. RUMBLE — the right trigger (AUX8, +attack) fires the rocket launcher:
     the pad kicks ("dual-rumble", 150 ms); a rocket at the player's own feet
     hurts, and the damage shakes it too (a longer rumble).
  5. A LOST PAD — unplugged with the trigger held, nothing stays held.
  6. CLASSIC (`?classic`) — the gamepad is a shared control now: the
     twin-stick layout works by default there too. `idcontrols` is the one
     step to id's own: `joystick 0` reads no pad; after `joystick 1` the
     left stick is id's joystick (Y walks), A is JOY1, unbound.
  7. PAD AND TOUCH (`?touch`: the touch controls, `navigator.vibrate`
     recorded) — a rumble goes to whichever was used last, never both: the
     pad after a pad button, the phone's vibration after a touch, and the
     phone when the pad is not read.

Usage: verify_gamepad.py [webdir]   (a deploy dir, PLATFORM.md: index.html,
wasi.js, quake.wasm, id1/pak0.pak). $QUAKE_BROWSER=firefox for Firefox.
"""
import math, os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8175)
httpd = isolated.serve(WEB, PORT)

# The synthetic pad: a standard-mapped Xbox-style pad, 17 buttons, 4 axes.
FAKE_PAD = r"""
(() => {
  const buttons = Array.from({ length: 17 }, () => ({ pressed: false, touched: false, value: 0 }));
  const rumbles = window.__rumbles = [];
  const pad = window.__pad = {
    id: 'verify_gamepad synthetic pad (STANDARD GAMEPAD)', index: 0, connected: true,
    mapping: 'standard', timestamp: 0, axes: [0, 0, 0, 0], buttons,
    vibrationActuator: {
      type: 'dual-rumble', effects: ['dual-rumble'],
      playEffect(type, params) { rumbles.push({ type, ...params }); return Promise.resolve('complete'); },
      reset() { return Promise.resolve('complete'); },
    },
  };
  // Press these buttons (standard indices) and set the axes; the rest let go.
  window.__setPad = (pressed, axes) => {
    buttons.forEach((b, i) => { const on = pressed.includes(i); b.pressed = b.touched = on; b.value = on ? 1 : 0; });
    pad.axes = (axes || [0, 0, 0, 0]).slice();
    pad.timestamp = performance.now();
  };
  navigator.getGamepads = () => [pad.connected ? pad : null, null, null, null];
})();
"""

A, B, X, Y, LB, RB, LT, RT, BACK, START = range(10)
DUP, DDOWN, DLEFT, DRIGHT = 12, 13, 14, 15

# A finger's tap on the screen's corner (a touch pointer, as a phone sends).
TOUCH_TAP = r"""() => {
    const at = document.getElementById('c').getBoundingClientRect();
    const ev = (type) => new PointerEvent(type, { pointerType: 'touch', clientX: at.x + 5, clientY: at.y + 5 });
    dispatchEvent(ev('pointerdown'));
    dispatchEvent(ev('pointerup'));
}"""

passed, failed = 0, 0


def check(name, ok, detail=""):
    global passed, failed
    print(("PASS" if ok else "FAIL"), name, detail)
    if ok:
        passed += 1
    else:
        failed += 1


def pad(pg, pressed=(), axes=(0, 0, 0, 0), hold=0.15):
    """Set the pad and let a few refreshes read it (each host frame runs
    IN_Commands and IN_JoyMove on the page's newest reading)."""
    pg.evaluate("([p, a]) => __setPad(p, a)", [list(pressed), list(axes)])
    time.sleep(hold)


def press(pg, button):
    pad(pg, [button])
    pad(pg, [])


def call(pg, name, *args):
    return pg.evaluate("([n, a]) => quake.call(n, ...a)", [name, list(args)])


def where(pg):
    return (call(pg, "listener_x"), call(pg, "listener_y"))


def facing(pg):
    return math.degrees(math.atan2(call(pg, "listener_fwd_y"), call(pg, "listener_fwd_x")))


def boot_page(br, query=""):
    pg = br.new_page(viewport={"width": 820, "height": 560})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
    pg.add_init_script(FAKE_PAD)
    pg.goto(f"http://127.0.0.1:{PORT}/index.html{query}", wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    return pg, errs


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])

    # ---- the 2026 profile (the page's default) ------------------------------
    pg, errs = boot_page(br)
    check("the scrim is up before any gesture", not pg.evaluate("overlay.classList.contains('hidden')"))
    press(pg, A)
    check("1. a pad button takes the scrim away", pg.evaluate("overlay.classList.contains('hidden')"))
    check("   and the attract demo plays on, no menu", call(pg, "menu_visible") == 0 and call(pg, "in_walk_mode") == 0)

    press(pg, START)
    check("2. Start opens the menu over the demo", call(pg, "menu_visible") == 1 and call(pg, "menu_screen_id") == 0)
    press(pg, DDOWN)
    press(pg, A)
    check("   D-pad down, A: Multiplayer", call(pg, "menu_screen_id") == 4)
    press(pg, B)
    check("   B backs out to Main", call(pg, "menu_visible") == 1 and call(pg, "menu_screen_id") == 0)
    press(pg, DUP)
    press(pg, A)
    check("   D-pad up, A: Single Player", call(pg, "menu_screen_id") == 1)
    press(pg, A)
    pg.wait_for_function("quake.call('in_walk_mode').then(v => v === 1)", timeout=10000)
    time.sleep(0.5)
    check("   A on New Game starts the game", call(pg, "menu_visible") == 0 and call(pg, "in_walk_mode") == 1)
    check("   the console heard IN_StartupJoystick", "joystick detected" in pg.evaluate("quake.text('console_text')"))

    x0, y0 = where(pg)
    pad(pg, [], (0, -1, 0, 0), hold=2.0)
    pad(pg, [])
    x1, y1 = where(pg)
    dist = math.hypot(x1 - x0, y1 - y0)
    check("3. the left stick walks", dist > 150, f"{dist:.0f} units")
    f0 = facing(pg)
    pad(pg, [], (0, 0, 1, 0), hold=0.5)
    pad(pg, [])
    turned = (f0 - facing(pg) + 540) % 360 - 180
    check("   the right stick turns right", 30 < turned < 180, f"{turned:.0f} degrees")

    call(pg, "exec", "impulse 9")   # every weapon and its ammo
    time.sleep(0.3)
    call(pg, "exec", "impulse 7")   # the rocket launcher
    time.sleep(1.0)
    pg.evaluate("__rumbles.length = 0")
    pad(pg, [RT], hold=0.3)
    check("4. RT is AUX8, +attack", call(pg, "key_is_down", 214) == 1)
    pad(pg, [], hold=0.5)
    shots = pg.evaluate("__rumbles.slice()")
    check("   firing a rocket kicks the pad", any(r["type"] == "dual-rumble" and r["duration"] == 150 for r in shots),
          str(shots[:3]))
    # A rocket at the player's feet: the right stick down pitches the view to
    # the floor, and the blast hurts.
    pad(pg, [], (0, 0, 0, 1), hold=1.0)
    pad(pg, [])
    time.sleep(1.2)                    # the launcher's refire
    pg.evaluate("__rumbles.length = 0")
    pad(pg, [RT], hold=0.2)
    pad(pg, [], hold=1.0)
    hits = pg.evaluate("__rumbles.slice()")
    check("   a rocket at the feet: the damage shakes it (a longer rumble)",
          any(r["duration"] != 150 and r["strongMagnitude"] > 0 for r in hits), str(hits[:4]))

    call(pg, "exec", "impulse 1")   # the axe: no more rockets at the feet
    pad(pg, [], (0, 0, 0, -1), hold=0.5)   # and the view back up
    pad(pg, [RT], hold=0.3)
    pg.evaluate("__pad.connected = false")
    time.sleep(0.3)
    check("5. a pad unplugged with RT held lets go of +attack", call(pg, "key_is_down", 214) == 0)
    pg.evaluate("__pad.connected = true; __setPad([], [0, 0, 0, 0])")
    time.sleep(0.2)
    pg.locator("#c").screenshot(path=os.path.join(WEB, "verify_gamepad.png"))
    check("no page errors (2026)", not errs, str(errs[-3:]))
    pg.close()

    # ---- Classic: the gamepad is shared by default; id's is one step away ----
    pg, errs = boot_page(br, "?classic")
    pg.evaluate("hideOverlayForever()")
    call(pg, "boot")
    call(pg, "menu_cancel")
    time.sleep(0.5)
    x0, y0 = where(pg)
    pad(pg, [], (0, -1, 0, 0), hold=1.0)
    pad(pg, [])
    x1, y1 = where(pg)
    check("6. Classic: the gamepad is shared by default, the left stick walks",
          math.hypot(x1 - x0, y1 - y0) > 100, f"{math.hypot(x1 - x0, y1 - y0):.0f} units")
    call(pg, "exec", "idcontrols")
    time.sleep(0.8)                 # the walk's momentum dies away
    x1, y1 = where(pg)
    pad(pg, [A], (0, -1, 0, 0), hold=1.0)
    pad(pg, [])
    x2, y2 = where(pg)
    check("   idcontrols: id's own joystick 0 reads no pad",
          math.hypot(x2 - x1, y2 - y1) < 1 and call(pg, "key_is_down", 203) == 0)
    call(pg, "exec", "joystick 1")
    pad(pg, [], (0, -1, 0, 0), hold=1.5)
    pad(pg, [])
    x3, y3 = where(pg)
    check("   joystick 1: Y walks, as id's joystick", math.hypot(x3 - x2, y3 - y2) > 100,
          f"{math.hypot(x3 - x2, y3 - y2):.0f} units")
    press(pg, A)
    check("   A is JOY1, unbound in default.cfg", "JOY1 is unbound" in pg.evaluate("quake.text('console_text')"))
    check("no page errors (Classic)", not errs, str(errs[-3:]))
    pg.close()

    # ---- a touch screen with a pad: the rumble goes to the one in use ----------
    pg, errs = boot_page(br, "?touch")
    pg.evaluate("window.__vibes = []; navigator.vibrate = (ms) => { __vibes.push(ms); return true; }")
    pg.wait_for_function("window.QuakeTouch !== undefined", timeout=10000)
    pg.evaluate("hideOverlayForever()")
    call(pg, "boot")
    call(pg, "menu_cancel")
    time.sleep(0.5)
    press(pg, X)                    # the pad is used (X is unbound in 2026)
    pg.evaluate("__rumbles.length = 0; __vibes.length = 0; onRumble(0.6, 0.3, 200, true)")
    got = pg.evaluate("[__rumbles.length, __vibes.length]")
    check("7. pad used last: the pad rumbles, the phone does not", got == [1, 0], str(got))
    pg.evaluate(TOUCH_TAP)
    pg.evaluate("__rumbles.length = 0; __vibes.length = 0; onRumble(0.6, 0.3, 200, true)")
    got = pg.evaluate("[__rumbles.length, __vibes.slice()]")
    check("   the screen touched since: the phone vibrates, the pad does not",
          got[0] == 0 and len(got[1]) == 1 and got[1][0] > 0, str(got))
    press(pg, X)
    pg.evaluate("__rumbles.length = 0; __vibes.length = 0; onRumble(0.6, 0.3, 200, false)")
    got = pg.evaluate("[__rumbles.length, __vibes.length]")
    check("   the pad not read (joystick 0): the phone", got == [0, 1], str(got))
    check("no page errors (touch)", not errs, str(errs[-3:]))
    pg.close()
    br.close()
httpd.shutdown()

print(f"done: {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
