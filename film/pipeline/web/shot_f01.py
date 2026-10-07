"""F01-page-load: the page loads in a browser window and the game starts.

A fresh browser profile opens the page (the default preset, slop) from a server capped at
10 MB/s, so the download shows. The boot is filmed in real time (the screencast) and laid on
exactly BOOT_FRAMES frames, so the rest of the clip keeps its times on every run; from the
moment the program is ready every frame is one 1/60 s tick of the page's own driver. The
start prompt over the attract demo (demo1, from its first frame), the pointer glides to it, a
click, the overlay goes, the demo plays on. The window (920x472 CSS px at devicePixelRatio
2s) is drawn around the page, with the local server's address.
"""

from __future__ import annotations

import io
import math
import time

from PIL import Image
from playwright.sync_api import sync_playwright

import compose
import webcap

ID, NAME = "F01", "F01-page-load"
DESC = "the page loads in a browser window and the game starts"

CSS_W, CSS_H = 920, 472
BOOT_FRAMES = 118                     # the boot as v7 has it: 1.97 s
MOVE0, MOVE1, CLICK, AWAY0, AWAY1 = 12, 62, 78, 104, 150
END = CLICK + 360


def ease(t):
    t = min(max(t, 0.0), 1.0)
    return t * t * (3 - 2 * t)


def run(job) -> dict:
    s = job.scale
    dpr = 2 * s
    rest = (1530.0 * s, 860.0 * s)        # where the pointer rests during the boot (device px, viewport)
    desk = compose.Desktop(s)
    with webcap.Server(job.deploy, webcap.SlowHandler) as srv, sync_playwright() as p:
        br, ctx = webcap.desktop_context(p, CSS_W, CSS_H, dpr)
        try:
            ctx.add_init_script(webcap.PAUSE_AT_READY_JS)
            pg = ctx.new_page()
            errs = []
            pg.on("pageerror", lambda e: errs.append(str(e)))
            pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
            sh = webcap.Shooter(ctx, pg, CSS_W, CSS_H, dpr)
            sc = webcap.Screencast(sh.cdp, CSS_W * dpr, CSS_H * dpr)
            pg.goto("about:blank")
            pg.mouse.move(rest[0] / dpr, rest[1] / dpr)
            chrome = desk.chrome("quake-srp · Quake in safe-Rust WebAssembly", f"localhost:{srv.port}",
                                 compose.page_favicon(job.deploy / "index.html"))

            # --- The boot, in real time ---------------------------------------------------
            sc.start()
            t_nav = time.time()
            pg.goto(srv.url, wait_until="commit")
            paused = None
            while paused is None and time.time() - t_nav < 60:
                pg.wait_for_timeout(20)
                try:
                    paused = pg.evaluate("window.__capturePaused || null")
                except Exception:
                    pass
            pg.wait_for_timeout(60)
            boot_kind = webcap.cursor_kind(pg, rest[0], rest[1], dpr)
            t_ready = time.time()
            shots = sc.stop()
            # The first frame of the new document (about:blank's white goes first).
            shots = [x for x in shots if x[0] >= t_nav]
            while shots and sum(Image.open(io.BytesIO(shots[0][1])).convert("L").resize((32, 16)).tobytes()) / 512 > 120:
                shots.pop(0)
            if not shots:
                raise RuntimeError("the screencast caught no frame of the boot")
            t0, t1 = shots[0][0], shots[-1][0] + 1 / webcap.FPS
            webcap.log(f"boot: first page frame {t0 - t_nav:.2f} s after navigation, ready+paused {t_ready - t_nav:.2f} s, "
                       f"{len(shots)} screencast frames over {t1 - t0:.2f} s, laid on {BOOT_FRAMES} frames")

            clip = job.clip(NAME, (desk.out_w, desk.out_h))
            try:
                clip.mark("page's first frame: the boot panel")
                k = 0
                for i in range(BOOT_FRAMES):
                    ti = t0 + (t1 - t0) * i / BOOT_FRAMES
                    while k + 1 < len(shots) and shots[k + 1][0] <= ti:
                        k += 1
                    clip.add(desk.frame(shots[k][1], chrome, cursor=rest, kind=boot_kind))
                clip.mark("the program is ready: start prompt; ticked from here")

                # --- Ticked: the prompt, the click, the attract demo ----------------------
                click_at = target = rest2 = None
                for i in range(END):
                    if i == MOVE0:
                        # The layout after the first frame (the canvas takes the window's
                        # shape): the prompt's words, and a resting place for the pointer at
                        # the view's lower right, over the game (a crosshair there).
                        play = pg.evaluate("(() => { const r = document.getElementById('play').getBoundingClientRect(); "
                                           "return [r.x + r.width * 0.30, r.y + r.height / 2]; })()")
                        target = (play[0] * dpr, play[1] * dpr)
                        c = pg.evaluate("(() => { const r = document.getElementById('c').getBoundingClientRect(); return [r.right, r.bottom]; })()")
                        rest2 = (c[0] * dpr - 150 * s, c[1] * dpr - 240 * s)
                    if i < MOVE0:
                        cur = rest
                    elif i <= MOVE1:
                        k = ease((i - MOVE0) / (MOVE1 - MOVE0))
                        # a gentle arc, as a hand moves
                        cur = (rest[0] + (target[0] - rest[0]) * k, rest[1] + (target[1] - rest[1]) * k - 60 * s * math.sin(math.pi * k))
                    elif i < AWAY0:
                        cur = target
                    else:
                        k = ease((i - AWAY0) / (AWAY1 - AWAY0))
                        cur = (target[0] + (rest2[0] - target[0]) * k, target[1] + (rest2[1] - target[1]) * k + 40 * s * math.sin(math.pi * k))
                    pg.mouse.move(cur[0] / dpr, cur[1] / dpr)
                    if i == CLICK:
                        pg.mouse.down()
                        pg.mouse.up()
                        click_at = cur
                        clip.mark("click: the overlay goes, the demo plays on")
                    webcap.tick(pg)
                    ring = (click_at[0], click_at[1], (i - CLICK) / 18) if click_at and i - CLICK <= 18 else None
                    kind = webcap.cursor_kind(pg, cur[0], cur[1], dpr)
                    clip.add(desk.frame(sh.png(), chrome, cursor=cur, ring=ring, kind=kind))
                clip.close()
            except BaseException:
                clip.abort()
                raise
            webcap.log("status line:", pg.evaluate("document.getElementById('status').textContent"),
                       "frame size:", pg.evaluate("quake.size()"), "errors:", errs[:5])
            return {"clip": clip, "boot": {"screencast_frames": len(shots), "seconds": round(t1 - t0, 3)}}
        finally:
            br.close()
