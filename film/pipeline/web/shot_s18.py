"""S18-slop-options-menu: the game's own menu, Options > Slop Options > Picture and sound, the
cursor stepping down its rows. The film tool cannot open menus, so this is the page.

Full frame, no page around it: the canvas alone at 1920s x 1080s (devicePixelRatio s, a
window whose canvas box is exactly 1920x1080 CSS px), the default preset (slop), the start
map, the player put at S17's end with the page's `setpos` call (eye 544,520,54) and turned to
yaw 90, level, with `look`. From the moment `map start` is asked for, every frame is one 1/60 s
tick and the keys go in between ticks, so a run is the same each time. The page's own "click
to capture mouse" chip (over the canvas until the menu opens) is hidden with a style rule:
this is the game's picture, not the page's.
"""

from __future__ import annotations

import base64
import io
import time

from PIL import Image
from playwright.sync_api import sync_playwright

import webcap

ID, NAME = "S18", "S18-slop-options-menu"
DESC = "the game's own menu: Options, Slop Options, Picture and sound"

# The keys, by frame: the main menu, Options, up past the end to Reset to slop, Reset to
# Classic and Slop Options, its three pages, Picture and sound, then down a row every 1/3 s.
PLAN = {
    40: ("Escape", "Esc: the main menu"),
    64: ("ArrowDown", None), 80: ("ArrowDown", "on Options"),
    100: ("Enter", "Options"),
    128: ("ArrowUp", "up past the end: Reset to slop"), 146: ("ArrowUp", "Reset to Classic"), 164: ("ArrowUp", "Slop Options"),
    190: ("Enter", "the Slop Options screen: its three pages"),
    226: ("Enter", "Picture and sound"),
}
for _k, _f in enumerate(range(262, 262 + 7 * 20, 20)):
    PLAN[_f] = ("ArrowDown", f"down a row ({_k + 1})")
END = 262 + 7 * 20 + 90
SETTLE = 60          # ticks between the player's placing and the first frame


def run(job) -> dict:
    s = job.scale
    css_w, css_h = 1920 + 34, 1080 + 84      # the canvas's box is the window less the page's chrome
    size = (1920 * s, 1080 * s)
    with webcap.Server(job.deploy) as srv, sync_playwright() as p:
        br, ctx = webcap.desktop_context(p, css_w, css_h, s)
        try:
            pg = ctx.new_page()
            errs = []
            pg.on("pageerror", lambda e: errs.append(str(e)))
            cdp = ctx.new_cdp_session(pg)
            call = lambda line: pg.evaluate("l => quake.callLine(l).then(r => r.value)", line)
            text = lambda line: pg.evaluate("l => quake.callLine(l).then(r => r.text)", line)
            pg.goto(srv.url, wait_until="load")
            pg.wait_for_function("window.quake && quake.ready", timeout=120000)
            time.sleep(0.5)
            pg.mouse.click(css_w / 2, css_h / 2)
            time.sleep(0.3)
            pg.evaluate("document.getElementById('walkBtn').click()")
            time.sleep(1.2)
            if pg.evaluate("exp.menu_visible()"):
                pg.keyboard.press("Escape")
            time.sleep(0.3)
            # From here every game step is one of ours.
            pg.evaluate("quake.pause()")
            call("exec map start")
            for _ in range(600):
                webcap.tick(pg)
                if text("map_name") == "maps/start.bsp":
                    break
            else:
                raise RuntimeError("start did not load")
            if pg.evaluate("exp.menu_visible()"):
                pg.keyboard.press("Escape")
                webcap.tick(pg)
            call("setpos 544 520 32")         # in noclip, so the eye stays at 54
            webcap.tick(pg)
            yaw0, pitch0 = call("player_field v_angle_y"), call("player_pitch")
            call(f"look {90 - yaw0} {-pitch0}")
            for _ in range(SETTLE):
                webcap.tick(pg)
            eye = [round(call(f"listener_{a}"), 2) for a in "xyz"]
            webcap.log("map:", text("map_name"), "size:", pg.evaluate("quake.size()"), "eye:", eye,
                       "yaw:", call("player_field v_angle_y"), "pitch:", call("player_pitch"))
            pg.mouse.move(2, 2)
            # The page's "click to capture mouse" chip sits over the canvas in an unlocked
            # walk: page chrome, not the game's picture, so hidden here.
            pg.add_style_tag(content="#lockChip { display: none !important; }")
            box = pg.evaluate("(() => { const c = document.getElementById('c'), r = c.getBoundingClientRect(); "
                              "return [r.left + c.clientLeft, r.top + c.clientTop, c.clientWidth, c.clientHeight]; })()")
            webcap.log("canvas box (CSS px):", box)
            clip_rect = {"x": box[0], "y": box[1], "width": box[2], "height": box[3], "scale": 1}

            def shot():
                return base64.b64decode(cdp.send("Page.captureScreenshot", {"format": "png", "optimizeForSpeed": True,
                                                                             "clip": clip_rect})["data"])

            first = Image.open(io.BytesIO(shot()))
            if first.size != size:
                clip_rect["scale"] = size[0] / first.size[0]       # a CSS-sized capture: ask for device px
            clip = job.clip(NAME, size)
            try:
                for i in range(END):
                    if i in PLAN:
                        key, what = PLAN[i]
                        pg.keyboard.press(key)
                        if what:
                            clip.mark(what)
                    webcap.tick(pg)
                    im = Image.open(io.BytesIO(shot())).convert("RGB")
                    clip.add(im)
                clip.close()
            except BaseException:
                clip.abort()
                raise
            webcap.log("errors:", errs[:3])
            return {"clip": clip}
        finally:
            br.close()
