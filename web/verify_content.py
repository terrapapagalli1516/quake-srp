#!/usr/bin/env -S uv run --with playwright --script
"""The player's own Quake files, end to end in the page, with synthesized data
only (no registered data is in the repo, and none is needed):

- a `pak1.pak` made here: id's `gfx/pop.lmp` from common.c's `pop[]` table and
  the shareware `maps/e1m1.bsp` copied as `maps/e2m1.bsp`;
- CD tracks 2, 3 and 6 as short generated tones (`track02.wav`…).

Dropped on the page (a real `drop` event), they restart the game registered
(`registered` 1, `map e2m1` loads from pak1), and the CD plays: the attract
loop's demo1 forces track 2 and it loops; `bgmvolume` sets its level; a level
plays its own track; `pause` pauses it. Then the refusals: a file that is not a
pak, the 2021 re-release's folder, and a pak1 without `pop.lmp` (a modified
shareware game: the program refuses it, and the page takes it out again).
Removing the files brings the shareware game back.

Then a server's own files (web/PLATFORM.md, "A server's own files"): a
manifest-less deploy's network log never asks for pak1 or any track; a
files.json offering pak1 and tracks plays registered with nothing dropped; a
player's own drop still wins over it, per file; and a broken server pak1
(not a pak at all) leaves the deploy on pak0, quietly.

Then a mission pack in files.json ("The game picker"): a synthesized
`hipnotic/pak0.pak` (the same `e1m1.bsp` trick, as `maps/hip1m1.bsp`) is
never fetched playing plain id1, and the start overlay's picker offers it;
`?game=hipnotic` fetches only it (never `rogue/pak0.pak`, never offered
here), no 404s, hip1m1 loads and plays its own CD tracks.

Usage: verify_content.py [deploydir]   (PLATFORM.md: index.html, wasi.js,
quake.wasm, id1/pak0.pak). Screenshots go to $QUAKE_SHOTS (default: the
deploy dir)."""
import base64, io, math, os, shutil, struct, tempfile, time, wave
from playwright.sync_api import sync_playwright
import isolated
from isolated import POP_LMP, pak_file, write_pak

WEB = isolated.webdir()
PORT = isolated.port(8171)
SERVER_PORT = PORT + 1   # a second, independent deploy: "A server's own files"
SERVER_PORT2 = PORT + 2  # a third, for the broken-pak1 deploy
SERVER_PORT3 = PORT + 3  # a fourth, for a mission pack's own files.json entry
SHOTS = os.environ.get("QUAKE_SHOTS", WEB)


def tone(freq, secs, rate=22050):
    """A mono 16-bit WAV of a sine at `freq` Hz, faded at the ends (no click
    at the loop seam), at 0.5 of full scale."""
    n = int(secs * rate)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        fade = rate // 50
        w.writeframes(b"".join(
            struct.pack("<h", int(16000 * math.sin(2 * math.pi * freq * i / rate) * min(1, i / fade, (n - i) / fade)))
            for i in range(n)))
    return buf.getvalue()


PAK0 = os.path.join(WEB, "id1", "pak0.pak")
E1M1 = pak_file(PAK0, "maps/e1m1.bsp")
PAK1 = write_pak([("gfx/pop.lmp", POP_LMP), ("maps/e2m1.bsp", E1M1)])
PAK1_MODIFIED = write_pak([("maps/e2m1.bsp", E1M1)])       # no pop.lmp: a mod on shareware
TRACKS = {2: tone(440, 0.8), 3: tone(660, 0.6), 6: tone(550, 1.0)}
# A synthesized hipnotic/pak0.pak — the same e1m1.bsp copied under the name
# hip1m1 would have, exactly PAK1's own trick for e2m1 — proves the page's
# file-serving and search-path plumbing end to end with no real mission-pack
# data in the repo ("The game picker").
HIP_PAK0 = write_pak([("maps/hip1m1.bsp", E1M1)])
HIP_TRACKS = {2: tone(220, 0.5), 6: tone(330, 0.6)}   # 2: demo1's attract loop; 6: e1m1's own (PAK1's test, above)


def server_deploy(pak1=None, tracks=None, packs=None):
    """A fresh deploy dir ("A server's own files"): WEB's page files,
    quake.wasm and id1/pak0.pak (symlinked — the same bytes, not an 18 MB
    copy), plus `pak1` at id1/pak1.pak and `tracks` (track number -> bytes)
    at id1/music/trackNN.wav if given, and `packs` — {game: {"pak0": bytes,
    "tracks": {n: bytes}}} — at <game>/pak0.pak and <game>/music/trackNN.wav
    for a mission pack ("The game picker"). files.json is
    isolated.write_manifest's, generated from whatever of those is there."""
    d = tempfile.mkdtemp(prefix="quake-content-server-")
    os.makedirs(os.path.join(d, "id1", "music"), exist_ok=True)
    isolated.copy_page(d)
    for rel in ("quake.wasm", os.path.join("id1", "pak0.pak")):
        os.symlink(os.path.abspath(os.path.join(WEB, rel)), os.path.join(d, rel))
    if pak1 is not None:
        with open(os.path.join(d, "id1", "pak1.pak"), "wb") as f:
            f.write(pak1)
    for n, data in (tracks or {}).items():
        with open(os.path.join(d, "id1", "music", f"track{n:02d}.wav"), "wb") as f:
            f.write(data)
    for game, spec in (packs or {}).items():
        os.makedirs(os.path.join(d, game, "music"), exist_ok=True)
        with open(os.path.join(d, game, "pak0.pak"), "wb") as f:
            f.write(spec["pak0"])
        for n, data in (spec.get("tracks") or {}).items():
            with open(os.path.join(d, game, "music", f"track{n:02d}.wav"), "wb") as f:
                f.write(data)
    isolated.write_manifest(d)
    return d


DROP = """async (files) => {
    const dt = new DataTransfer();
    for (const [name, b64, type] of files) {
        const bytes = Uint8Array.from(atob(b64), c => c.charCodeAt(0));
        dt.items.add(new File([bytes], name, { type }));
    }
    document.body.dispatchEvent(new DragEvent('dragenter', { dataTransfer: dt, bubbles: true }));
    document.body.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }));
}"""
ADD = """async (items) => quake.content.add(items.map(([name, path, b64]) =>
    ({ name, path, bytes: Uint8Array.from(atob(b64), c => c.charCodeAt(0)) })))"""


def b64(b):
    return base64.b64encode(b).decode()


httpd = isolated.serve(WEB, PORT)
fails = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        fails.append(what)


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
    pg = br.new_page(viewport={"width": 900, "height": 700})
    errs = []
    pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))

    def ready(page=None):
        """The page is up and has the program's word on its files. Polled, so
        a start the page answers with another reload (a refusal) is waited
        out too. `page` defaults to the main page (pg); the server-files
        checks below pass their own."""
        page = page or pg
        until = time.time() + 180
        while time.time() < until:
            try:
                if page.evaluate("!!(window.quake && quake.ready && quake.contentPath !== undefined)"):
                    return
            except Exception:
                pass                    # the page is reloading
            time.sleep(0.2)
        raise SystemExit("the page did not come up")

    def start_audio(page=None):
        page = page or pg
        page.mouse.click(450, 300)     # the first gesture: the overlay; audio runs
        page.wait_for_function("quake.audio.ring().running", timeout=10000)

    def cd(page=None):
        return (page or pg).evaluate("quake.cd.state()")

    def call(line, page=None):
        return (page or pg).evaluate(f"quake.callLine({line!r})")

    def level(secs=0.8, page=None):
        """The CD's output RMS: the median of a few analyser windows over
        `secs`, after a moment for a change to reach the output."""
        page = page or pg
        time.sleep(0.3)
        got = []
        for _ in range(8):
            got.append(page.evaluate("quake.cd.state().level"))
            time.sleep(secs / 8)
        return sorted(got)[len(got) // 2]

    # --- A manifest-less deploy (WEB has no files.json): the network log
    # never shows a request for pak1 or any track — the point of asking a
    # manifest instead of guessing ("A server's own files").
    reqs = []
    pg.on("request", lambda r: reqs.append(r.url))
    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    ready()
    noisy = [u for u in reqs if "pak1" in u or "/music/" in u]
    check(not noisy, f"a manifest-less deploy asks for nothing extra ({noisy})")
    st = pg.evaluate("quake.content.state()")
    check(st["registered"] is False and st["tracks"] == [] and st["paks"] == [], f"a fresh page is shareware, no files ({st['registered']}, {st['tracks']}, {st['paks']})")
    check(pg.evaluate("quake.cd.state().want") is None, "no music: no CD records")

    # --- Drop pak1.pak and three tracks (and a stray file) on the page.
    files = [["pak1.pak", b64(PAK1), ""], ["readme.txt", b64(b"hello"), "text/plain"]]
    files += [[f"track{n:02d}.wav", b64(t), "audio/wav"] for n, t in TRACKS.items()]
    with pg.expect_navigation(timeout=60000):
        pg.evaluate(DROP, files)
    ready()
    st = pg.evaluate("quake.content.state()")
    print("  after the drop:", st["line"], "|", st["message"])
    check(st["registered"] is True, "pak1.pak with id's pop.lmp: registered")
    check(st["tracks"] == [2, 3, 6] and st["paks"] == ["id1/pak1.pak"], f"kept pak1.pak and tracks 2, 3, 6 ({st['paks']}, {st['tracks']})")
    check("added pak1.pak and 3 CD tracks" in st["message"] and "left out 1 other file" in st["message"], "the drawer says what was added and left out")
    check("id1/pak1.pak (2 files)" in st["path"] and "id1/pak0.pak (339 files)" in st["path"], "the search path: pak1 over pak0")
    call("exec registered")
    check('"registered" is "1"' in pg.evaluate("quake.text('console_text')"), "the console's registered is 1")

    # --- The attract loop's demo1 forces CD track 2, looping.
    start_audio()
    pg.wait_for_function("quake.cd.state().playing", timeout=10000)
    s = cd()
    check(s["want"]["track"] == 2 and s["want"]["looping"] and s["track"] == 2 and s["loop"], f"demo1 plays track 2, looping ({s['want']})")
    times = []
    for _ in range(40):
        times.append(pg.evaluate("quake.cd.state().time"))
        time.sleep(0.05)
    wraps = sum(1 for a, b in zip(times, times[1:]) if b < a - 0.2)
    check(wraps >= 1 and pg.evaluate("quake.cd.state().playing"), f"the 0.8 s track loops ({wraps} wraps in 2 s)")
    loud = level()
    check(loud > 0.05, f"the CD is heard beside the mix (RMS {loud:.3f})")

    # --- bgmvolume sets the drive's level.
    call("exec bgmvolume 0.25")
    pg.wait_for_function("Math.abs(quake.cd.state().gain - 63 / 255) < 1e-3", timeout=5000)
    quiet = level()
    check(0.15 < quiet / max(loud, 1e-9) < 0.4, f"bgmvolume 0.25: the level {quiet:.3f} is about a quarter of {loud:.3f}")
    call("exec bgmvolume 1")

    # --- map e2m1: from pak1, with its level's track (e1m1's data: sounds 6).
    call("exec map e2m1")
    pg.wait_for_function("quake.text('map_name').then(m => m === 'maps/e2m1.bsp')", timeout=20000)
    pg.wait_for_function("quake.cd.state().track === 6 && quake.cd.state().playing", timeout=10000)
    check(True, "map e2m1 loads from pak1 and plays track 6")
    time.sleep(0.5)
    pg.locator("#c").screenshot(path=os.path.join(SHOTS, "verify_content_e2m1.png"))
    serial = cd()["want"]["serial"]
    call("exec pause")
    pg.wait_for_function("!quake.cd.state().playing && quake.cd.state().want.mode === 2", timeout=5000)
    t0 = cd()["time"]
    time.sleep(0.4)
    check(abs(cd()["time"] - t0) < 1e-6, "pause pauses the CD where it was")
    call("exec pause")
    pg.wait_for_function("quake.cd.state().playing", timeout=5000)
    check(cd()["want"]["serial"] == serial, "and resume goes on (no new start)")
    call("exec cd play 3")
    pg.wait_for_function("quake.cd.state().track === 3 && quake.cd.state().playing", timeout=5000)
    pg.wait_for_function("quake.cd.state().want.mode === 0 && !quake.cd.state().playing", timeout=5000)
    check(True, "cd play 3 plays it once, and the drive stops at its end")

    # --- Refusals the page makes itself: not a pak; the re-release's folder.
    pg.evaluate(ADD, [["pak1.pak", "pak1.pak", b64(b"not a pak at all")]])
    msg = pg.evaluate("quake.content.state().message")
    check("pak1.pak: not a pak file" in msg, f"a file that is not a pak is refused ({msg})")
    pg.evaluate(ADD, [["pak0.pak", "Quake/rerelease/id1/pak0.pak", b64(PAK1)]])
    msg = pg.evaluate("quake.content.state().message")
    check("re-release" in msg, f"the 2021 re-release's files are left out ({msg[:80]}…)")

    # --- Remove them all: the shareware game, no drive.
    with pg.expect_navigation(timeout=60000):
        pg.evaluate("quake.content.remove()")
    ready()
    st = pg.evaluate("quake.content.state()")
    check(st["registered"] is False and st["tracks"] == [] and st["paks"] == [], "removed: shareware, no files")
    check("removed your files" in st["message"], "the drawer says so")
    call("exec map e2m1")
    time.sleep(1.0)
    check(pg.evaluate("quake.text('map_name')") != "maps/e2m1.bsp", "shareware has no e2m1")

    # --- A pak1 without pop.lmp: the program refuses it (a modified shareware
    # game), and the page takes it out again and says why.
    with pg.expect_navigation(timeout=60000):
        pg.evaluate(DROP, [["pak1.pak", b64(PAK1_MODIFIED), ""]])
    # The refused start reloads the page once more, without the file.
    time.sleep(1.0)
    ready()
    st = pg.evaluate("quake.content.state()")
    print("  after the refusal:", st["message"])
    check(st["paks"] == [] and "You must have the registered version to use modified games" in st["message"],
          "a modified shareware game is refused with id's words, and the pak is out again")
    pg.locator("#drawer").evaluate("d => d.hidden = false")
    pg.locator("#content").screenshot(path=os.path.join(SHOTS, "verify_content_drawer.png"))

    real = [e for e in errs if "exit 1" not in e and "the game stopped" not in e]
    check(not real, f"no console errors {real[-3:]}")

    # --- A server's own files (web/PLATFORM.md, "A server's own files"): a
    # files.json offering pak1 and three tracks plays registered with
    # nothing dropped, and a player's own drop still wins, per file.
    d1 = server_deploy(pak1=PAK1, tracks=TRACKS)
    httpd1 = isolated.serve(d1, SERVER_PORT)
    pg2 = br.new_page(viewport={"width": 900, "height": 700})
    errs2 = []
    pg2.on("console", lambda m: errs2.append((m.type, m.text)))
    pg2.on("pageerror", lambda e: errs2.append(("error", "PAGEERROR: " + str(e))))
    pg2.goto(f"http://127.0.0.1:{SERVER_PORT}/index.html", wait_until="load")
    ready(pg2)
    st = pg2.evaluate("quake.content.state()")
    check(st["registered"] is True and st["paks"] == ["id1/pak1.pak"] and st["serverPaks"] == ["id1/pak1.pak"],
          f"files.json's pak1 plays registered with nothing dropped ({st['registered']}, {st['paks']}, {st['serverPaks']})")
    check(st["tracks"] == [2, 3, 6] and st["serverTracks"] == [2, 3, 6], f"and its three tracks ({st['tracks']}, {st['serverTracks']})")
    start_audio(pg2)
    pg2.wait_for_function("quake.cd.state().playing", timeout=10000)
    s = cd(pg2)
    check(s["want"]["track"] == 2 and s["want"]["looping"], f"demo1 plays the server's track 2, looping ({s['want']})")
    loud = level(page=pg2)
    check(loud > 0.05, f"the server's CD track is heard beside the mix (RMS {loud:.3f})")

    # --- The player's own pak1 — here, one the engine will refuse (no
    # pop.lmp) — is tried before the server's valid one (precedence):
    # dropping it refuses the whole boot with id's own words, which could
    # only happen if the player's file, not the server's good one, was the
    # one the engine saw. The existing auto-retry (web/PLATFORM.md, "Your
    # files") then takes the bad drop out again and reloads — recovering,
    # here, to the server's still-valid pak1, not bare pak0.
    with pg2.expect_navigation(timeout=60000):
        pg2.evaluate(DROP, [["pak1.pak", b64(PAK1_MODIFIED), ""]])
    time.sleep(1.0)
    ready(pg2)
    st = pg2.evaluate("quake.content.state()")
    check("could not play with pak1.pak" in st["message"] and "You must have the registered version to use modified games" in st["message"],
          f"a player's own pak1 is tried first, proving precedence, even though it is refused ({st['message']})")
    check(st["registered"] is True and st["serverPaks"] == ["id1/pak1.pak"],
          f"and the page recovers to the server's still-valid pak1, not bare pak0 ({st['registered']}, {st['serverPaks']})")

    # --- And a player's own track 2 overrides the server's by number; the
    # server's 3 and 6 are still in effect.
    with pg2.expect_navigation(timeout=60000):
        pg2.evaluate(ADD, [["track02.wav", "track02.wav", b64(tone(880, 0.5))]])
    ready(pg2)
    st = pg2.evaluate("quake.content.state()")
    check(st["serverTracks"] == [3, 6] and st["tracks"] == [2, 3, 6],
          f"a player's own track 2 overrides the server's; 3 and 6 are still its ({st['serverTracks']}, {st['tracks']})")
    httpd1.shutdown()
    shutil.rmtree(d1, ignore_errors=True)

    # --- A broken server pak1 (not a pak at all): left out quietly (a
    # console.warn, not an error), and pak0 still plays.
    d2 = server_deploy(pak1=b"not a pak at all")
    httpd2 = isolated.serve(d2, SERVER_PORT2)
    pg3 = br.new_page(viewport={"width": 900, "height": 700})
    errs3 = []
    pg3.on("console", lambda m: errs3.append((m.type, m.text)))
    pg3.on("pageerror", lambda e: errs3.append(("error", "PAGEERROR: " + str(e))))
    pg3.goto(f"http://127.0.0.1:{SERVER_PORT2}/index.html", wait_until="load")
    ready(pg3)
    st = pg3.evaluate("quake.content.state()")
    check(st["registered"] is False and st["paks"] == [] and st["serverPaks"] == [], f"a broken server pak1 leaves the deploy on pak0 ({st['paks']})")
    check(any(t == "warning" and "not a pak file" in m for t, m in errs3), f"and says why on the console ({errs3})")
    check(not any(t == "error" for t, m in errs3), f"as a warning, not an error ({[m for t, m in errs3 if t == 'error']})")
    httpd2.shutdown()
    shutil.rmtree(d2, ignore_errors=True)

    # --- A mission pack in files.json ("The game picker"): a manifest
    # offering hipnotic/pak0.pak (and one of its own tracks) is never
    # fetched while playing plain id1, and the picker offers it; with
    # ?game=hipnotic it is fetched (no 404s, no request for the absent
    # rogue/pak0.pak), hip1m1 loads from it, and its track plays.
    # A pak1 too: hipnotic's own pak0.pak is not id's shareware one, so
    # com_modified is set (common.rs's Pak::is_modified) the moment it is on
    # the search path — id's own COM_CheckRegistered then refuses to run at
    # all without proof of the registered game, exactly as real WinQuake
    # required owning registered Quake to play a mission pack.
    d3 = server_deploy(pak1=PAK1, packs={"hipnotic": {"pak0": HIP_PAK0, "tracks": HIP_TRACKS}})
    httpd3 = isolated.serve(d3, SERVER_PORT3)
    pg4 = br.new_page(viewport={"width": 900, "height": 700})
    errs4 = []
    pg4.on("console", lambda m: errs4.append((m.type, m.text)))
    pg4.on("pageerror", lambda e: errs4.append(("error", "PAGEERROR: " + str(e))))
    reqs4 = []
    statuses4 = {}
    pg4.on("request", lambda r: reqs4.append(r.url))
    pg4.on("response", lambda r: statuses4.update({r.url: r.status}))
    pg4.goto(f"http://127.0.0.1:{SERVER_PORT3}/index.html", wait_until="load")
    ready(pg4)
    st = pg4.evaluate("quake.content.state()")
    check(st["paks"] == ["id1/pak1.pak"] and st["tracks"] == [], f"plain id1 play keeps the mission pack out of effect ({st['paks']}, {st['tracks']})")
    noisy = [u for u in reqs4 if "hipnotic" in u]
    check(not noisy, f"and never fetches it ({noisy})")
    picker = pg4.eval_on_selector("#gamePicker", "e => e.innerHTML")
    check("Scourge of Armagon" in picker and "?game=hipnotic" in picker, f"the picker offers it ({picker})")

    pg4.goto(f"http://127.0.0.1:{SERVER_PORT3}/index.html?game=hipnotic", wait_until="load")
    ready(pg4)
    st = pg4.evaluate("quake.content.state()")
    check(st["game"] == "hipnotic" and st["paks"] == ["id1/pak1.pak", "hipnotic/pak0.pak"] and st["tracks"] == [2, 6],
          f"?game=hipnotic plays from the manifest's hipnotic/pak0.pak ({st['game']}, {st['paks']}, {st['tracks']})")
    rogue_reqs = [u for u in reqs4 if "rogue" in u]
    check(not rogue_reqs, f"never asks for the other pack ({rogue_reqs})")
    non200 = {u: s for u, s in statuses4.items() if s >= 400}
    check(not non200, f"no 404s ({non200})")
    # #play, not a raw coordinate: the picker (below it in the overlay) has
    # a live "id1" link while a mission pack is playing, which a guessed
    # click point could land on instead.
    pg4.click("#play")
    pg4.wait_for_function("quake.audio.ring().running", timeout=10000)
    pg4.wait_for_function("quake.cd.state().track === 2 && quake.cd.state().playing", timeout=10000)
    check(True, "demo1's attract loop plays hipnotic's own track 2")
    call("exec map hip1m1", page=pg4)
    pg4.wait_for_function("quake.callLine('map_name').then(r => r.text === 'maps/hip1m1.bsp')", timeout=20000)
    # hip1m1.bsp here is literally e1m1.bsp (HIP_PAK0, above) — PAK1's own
    # test, further up, established e1m1's level data wants CD track 6.
    pg4.wait_for_function("quake.cd.state().track === 6 && quake.cd.state().playing", timeout=10000)
    check(True, "hip1m1 loads from the server-offered pack and plays its own track 6")
    real4 = [m for t, m in errs4 if t == "error" and "exit 1" not in m and "the game stopped" not in m]
    check(not real4, f"no console errors on the mission-pack page {real4[-3:]}")
    httpd3.shutdown()
    shutil.rmtree(d3, ignore_errors=True)

    real2 = [m for t, m in errs2 if t == "error" and "exit 1" not in m and "the game stopped" not in m]
    check(not real2, f"no console errors on the server-files page {real2[-3:]}")
    br.close()
httpd.shutdown()

if fails:
    print("FAIL:", "; ".join(fails))
    raise SystemExit(1)
print("done: the player's files verified (registered from a dropped pak1, e2m1, the CD's tracks, level, pause, "
      "refusals, removal, and a server's own files.json: no extra requests when absent, registered play and "
      "music with nothing dropped, the player's own files still winning, a broken server pak1 left out quietly, "
      "and a mission pack in files.json: fetched and offered by the picker only for the game starting)")
