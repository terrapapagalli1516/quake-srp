"""F05b-phone-touch: touch play on a phone held sideways, in e1m1's skylight hall at skill 2:
rockets into the grunts.

The phone is web/verify_touch.py's PHONE_26 (1012x412 CSS px, isMobile, hasTouch) at
devicePixelRatio 2.6s, the default preset. At s = 2 the game's pixel size goes from the touch
preset's 2 to 4, so the game draws the same 1315x535 frame as at s = 1 and only the
page around it (the touch controls) is drawn finer. The drawn phone outline and fingers.

After "tap to start", the page's own calls (quake.callLine):
- skill 2 the game's own way: start's Hard hall, then its episode 1 portal (this port has no
  `skill` command, and `map` starts at skill 1), checked in a save's skill line;
- brightness 0.8 (id's slider, as the film's other shots);
- god, impulse 9 and 7 (the rocket launcher);
- setpos into the corridor before the hall's doorway (eye 700,2824,-34, yaw 180, pitch -6),
  noclip (off again: setpos leaves the player in noclip);
- the console toggled down and up, so the notify lines go (Con_ToggleConsole_f).
From the portal on the page is paused and every step is one of ours, so a run is the same
each time. Then every frame is one 1/60 s tick, with CDP touch events between ticks.

The grunts see the player at once and come at him: walk in through the doorway, a small aim,
the first rocket at 1.03 s into the five of them, JUMP, a second rocket down the hall, forward
and right into the hall while the look turns left, a third rocket, a look up at the skylight
and back down. Nothing is picked up (no notify line).
"""

from __future__ import annotations

import io
import time

from PIL import Image, ImageStat
from playwright.sync_api import sync_playwright

import compose
import webcap

ID, NAME = "F05b", "F05b-phone-touch"
DESC = "touch play on a phone, rockets into e1m1's grunts"

CSS_W, CSS_H, DPR1 = 1012, 412, 2.6
X, Y, Z, YAW, PITCH = 700.0, 2824.0, -54.0, 180.0, -6.0
GAMMA = "0.8"
SETTLE = 2
END = 480
# The plan, in frames at 60 fps.
# STICKS: (start, end, dx, dy): CSS px off where the left thumb lands, held;
# LOOKS: (start, end, dx, dy): a drag of the right thumb from a fresh landing, eased;
# PRESS: (frame, button): a 9-frame tap.
STICKS = [(24, 70, 2, -24), (186, 300, 12, -18)]
LOOKS = [(30, 58, 10, 0), (190, 270, -24, 0), (320, 370, 0, -70), (400, 450, 0, 60)]
PRESS = [(62, "fire"), (120, "jump"), (150, "fire"), (286, "fire")]
STICK0 = (165.0, 290.0)
LOOK_AT = [(690.0, 215.0), (720.0, 200.0), (660.0, 225.0)]


def lerp(a, b, k):
    return (a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k)


def ease(t):
    t = min(max(t, 0.0), 1.0)
    return t * t * (3 - 2 * t)


def game_luma(im: Image.Image) -> float:
    """Mean luma of the game area: the capture without the touch layer's edges and the status
    bar (the middle of the picture)."""
    g = im.convert("L")
    w, h = g.size
    return ImageStat.Stat(g.crop((int(w * 0.08), int(h * 0.05), int(w * 0.92), int(h * 0.80)))).mean[0]


def blasts(lum: list[float], fires: list[int]) -> list[tuple[int, int | None]]:
    """Each FIRE's blast, from the game area's luma: a FIRE lights the walls first (the
    rocket's own light, a few frames), then the explosion lifts it again. The blast frame is
    the first frame of that second rise."""
    out = []
    for f0 in fires:
        pk = max(range(f0, min(f0 + 10, len(lum))), key=lambda i: lum[i])
        i = pk
        while i + 1 < len(lum) and lum[i + 1] <= lum[i]:
            i += 1                                   # down from the muzzle's peak
        best = None
        for j in range(i + 1, min(f0 + 90, len(lum))):
            if lum[j] - min(lum[i:j]) > 2.5:
                best = j
                break
        out.append((f0, best))
    return out


def run(job) -> dict:
    s = job.scale
    dpr = DPR1 * s
    phone = compose.Phone(s)
    with webcap.Server(job.deploy) as srv, sync_playwright() as p:
        br = webcap.launch(p)
        try:
            ctx = br.new_context(viewport={"width": CSS_W, "height": CSS_H}, device_scale_factor=dpr, is_mobile=True, has_touch=True)
            ctx.add_init_script(webcap.NO_FULLSCREEN_JS)
            pg = ctx.new_page()
            errs = []
            pg.on("pageerror", lambda e: errs.append(str(e)))
            pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
            sh = webcap.Shooter(ctx, pg, CSS_W, CSS_H, dpr)
            cdp = sh.cdp
            mode = lambda: pg.evaluate("document.getElementById('touch') && document.getElementById('touch').dataset.mode")
            line = lambda q: pg.evaluate("q => quake.callLine(q).then(r => [r.value, r.text])", q)
            field = lambda name: line(f"player_field {name}")[0]
            box = lambda sel: pg.locator(sel).bounding_box()
            centre = lambda b: (b["x"] + b["width"] / 2, b["y"] + b["height"] / 2)

            def ticks(n):
                for _ in range(n):
                    webcap.raf(pg)
                    webcap.tick(pg)

            pg.goto(srv.url, wait_until="load")
            pg.wait_for_function("window.quake && quake.ready && window.QuakeTouch", timeout=120000)
            time.sleep(0.5)
            pg.touchscreen.tap(CSS_W / 2, CSS_H / 2)                     # tap to start
            pg.wait_for_function("document.getElementById('touch').dataset.mode === 'demo'", timeout=10000)

            # Skill 2 the game's own way: a new server takes the running one's skill at a
            # changelevel, and `map` starts at skill 1. start's Hard hall, its
            # trigger_setskill "2" (x 809-919, y 1345-1359), walked into short of the
            # teleporter behind it (y 1369-1383)...
            line("exec map start")
            time.sleep(2.0)
            if mode() == "menu":
                pg.touchscreen.tap(*centre(box("#tBack")))
            pg.wait_for_function("document.getElementById('touch').dataset.mode === 'play'", timeout=15000)
            line("setpos 864 1338 12")
            line("exec noclip")
            time.sleep(1.0)
            # ... then the episode 1 portal (trigger_changelevel e1m1: x -87..-41, y
            # 1601..1655, z 97..255). From here every game step is one of ours.
            pg.evaluate("quake.pause()")
            line("setpos -64 1628 130")
            line("exec noclip")
            # The game's own state decides when e1m1 has loaded, tick by tick (the page's touch
            # mode is the DOM's, updated on the browser's clock, so it must not count ticks).
            for k in range(600):
                ticks(1)
                if line("map_name")[1] == "maps/e1m1.bsp":
                    break
            else:
                raise RuntimeError("the portal did not take the player to e1m1")
            webcap.log(f"e1m1 after {k + 1} tick(s)")
            line("exec save skillcheck")
            ticks(6)
            sav = None
            for _ in range(40):
                sav = pg.evaluate("quake.kept('id1/skillcheck.sav')")
                if sav:
                    break
                time.sleep(0.1)
            skill = float(sav.split("\n")[18]) if sav else None
            if skill != 2.0:
                raise RuntimeError(f"skill {skill}, not 2")
            line(f"exec gamma {GAMMA}")
            if s != 1:
                line(f"exec vid_pixelsize {2 * s}")  # the phone profile's 1315x535 frame, at any scale
            # god, the weapons and the launcher at e1m1's start, far from the hall. Their
            # notify lines go with noclip's, below.
            line("exec god")
            line("exec impulse 9")
            ticks(6)
            line("exec impulse 7")
            ticks(30)

            # Into the hall: setpos, the eye turned, noclip (off: the player stands and
            # walks), the notify lines dropped (toggling the console zeroes their times).
            line(f"setpos {X} {Y} {Z}")
            webcap.tick(pg)
            yaw0, pitch0 = field("v_angle_y"), line("player_pitch")[0]
            line(f"look {YAW - yaw0} {PITCH - pitch0}")
            line("exec noclip")
            line("console_toggle")
            line("console_toggle")
            ticks(SETTLE)
            webcap.log("pos", [field(f"origin_{c}") for c in "xyz"], "yaw", field("v_angle_y"), "pitch", line("player_pitch")[0],
                       "skill", skill, "weapon", field("weapon"), "rockets", field("ammo_rockets"),
                       "game frame", pg.evaluate("quake.size()"))
            fire, jump = centre(box("#tFire")), centre(box("#tJump"))
            btn = {"fire": fire, "jump": jump}

            # The fingers: id -> (x, y) in CSS px, sent whole on every change (CDP compares
            # each event's points with the last: a point gone is a lift).
            down, last = {}, {}

            def send():
                nonlocal last
                if down == last:
                    return
                pts = [{"x": x, "y": y, "id": i} for i, (x, y) in sorted(down.items())]
                if not pts:
                    cdp.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
                elif set(down) - set(last):
                    cdp.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": pts})
                else:
                    cdp.send("Input.dispatchTouchEvent", {"type": "touchMove", "touchPoints": pts})
                last = dict(down)

            def plan(i):
                down.pop(1, None)
                for (a, e, dx, dy) in STICKS:              # left thumb: the stick
                    if a <= i < e:
                        down[1] = lerp(STICK0, (STICK0[0] + dx, STICK0[1] + dy), ease((i - a) / 10))
                down.pop(2, None)
                for k, (a, e, dx, dy) in enumerate(LOOKS):  # right thumb: look
                    if a <= i < e:
                        x0, y0 = LOOK_AT[k % len(LOOK_AT)]
                        if dx < 0:
                            x0 += 60                       # a leftward drag starts further right
                        y0 -= dy / 2                       # a vertical drag is centred on the screen
                        down[2] = lerp((x0, y0), (x0 + dx, y0 + dy + 2), ease((i - a) / max(e - a - 4, 1)))
                for k, (a, w) in enumerate(PRESS):         # the buttons
                    if a <= i < a + 9:
                        down[3 + k] = btn[w]
                    else:
                        down.pop(3 + k, None)

            marks = {}
            for (a, e, dx, dy) in STICKS:
                marks[a] = f"left thumb lands: the stick ({'forward' if dx < 4 else 'forward and right'})"
                marks[e] = "stick released"
            for (a, e, dx, dy) in LOOKS:
                marks[a] = "right thumb drags: look " + ("up" if dy < -abs(dx) else "down" if dy > abs(dx) else "right" if dx > 0 else "left")
                marks[e] = "look released"
            n = 0
            for (a, w) in PRESS:
                n += w == "fire"
                marks[a] = f"FIRE (rocket {n})" if w == "fire" else "JUMP"
            fires = [a for a, w in PRESS if w == "fire"]

            rockets0 = field("ammo_rockets")
            lum = []
            clip = job.clip(NAME, (phone.out_w, phone.out_h))
            try:
                for i in range(END):
                    if i in marks:
                        clip.mark(marks[i])
                    plan(i)
                    send()
                    webcap.raf(pg)                  # touch.js sends the stick once a display frame
                    webcap.tick(pg)
                    im = Image.open(io.BytesIO(sh.png())).convert("RGBA")
                    lum.append(game_luma(im))
                    clip.add(phone.frame(im, touches=list(down.values()), dpr=dpr))
                rockets1, health1 = field("ammo_rockets"), field("health")
                for f0, b in blasts(lum, fires):
                    if b is None:
                        clip.mark(f"no blast found in luma after the FIRE at frame {f0}", frame=f0)
                    else:
                        clip.mark(f"blast of the rocket fired at frame {f0} (luma {lum[b - 1]:.1f} -> {lum[b]:.1f})", frame=b)
                clip.mark(f"end: rockets {rockets0:g} -> {rockets1:g}, health {health1:g}", frame=END)
                clip.close()
            except BaseException:
                clip.abort()
                raise
            webcap.log("errors:", errs[:5])
            return {"clip": clip}
        finally:
            br.close()
