#!/usr/bin/env -S uv run --with playwright --script
"""Serve a deploy dir (PLATFORM.md; default web/) locally, load it in headless
chromium, walk forward, screenshot. Proves the quake-rs WASM build actually
runs Quake in a browser.

Usage: shoot.py [webdir]"""
import os, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8143)
httpd = isolated.serve(WEB, PORT)

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, args=["--no-sandbox"])
    page = browser.new_page(viewport={"width": 900, "height": 640})
    msgs = []
    page.on("console", lambda m: msgs.append(m.text))
    page.on("pageerror", lambda e: msgs.append("PAGEERROR: " + str(e)))
    page.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    try:
        page.wait_for_function(
            "document.getElementById('status').textContent.includes('ready')",
            timeout=40000,
        )
        booted = True
    except Exception as e:
        booted = False
        print("wait failed:", e)
    if booted:
        box = page.locator("#c").bounding_box()
        cx, cy = box["x"] + box["width"] / 2, box["y"] + box["height"] / 2
        # Walk forward (W) while drag-looking the mouse to turn ~ right.
        page.keyboard.down("w")
        page.mouse.move(cx, cy)
        page.mouse.down()
        for i in range(20):
            page.mouse.move(cx + i * 10, cy)  # drag right -> turn
            time.sleep(0.05)
        page.mouse.up()
        time.sleep(0.6)
        page.keyboard.up("w")
        time.sleep(0.3)
        page.locator("#c").screenshot(path=os.path.join(WEB, "browser_shot.png"))
        print("WALK_SHOT_OK")

        # Switch to demo playback and let it run a couple seconds.
        page.eval_on_selector("#demoBtn", "b => b.click()")
        page.wait_for_function(
            "document.getElementById('status').textContent.includes('playing demo')",
            timeout=40000,
        )
        time.sleep(2.5)
        page.locator("#c").screenshot(path=os.path.join(WEB, "browser_demo.png"))
        print("DEMO_SHOT_OK")

        # Sound: the sound button plays a real Quake .wav (`play
        # items/r_item1.wav`; output is silent in headless, but the status
        # line proves the audio path ran).
        page.eval_on_selector("#sndBtn", "b => b.click()")
        time.sleep(1.0)
        print("sound_status:", page.eval_on_selector("#status", "e=>e.textContent"))
    print("status:", page.eval_on_selector("#status", "e=>e.textContent"))
    print("console_tail:", msgs[-6:])
    browser.close()
httpd.shutdown()
print("done")
