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

Usage: verify_content.py [deploydir]   (PLATFORM.md: index.html, wasi.js,
quake.wasm, id1/pak0.pak). Screenshots go to $QUAKE_SHOTS (default: the
deploy dir)."""
import base64, io, math, os, struct, time, wave
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8171)
SHOTS = os.environ.get("QUAKE_SHOTS", WEB)

# common.c's pop[]: gfx/pop.lmp is these 128 shorts, big-endian.
POP = [
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x6600, 0x0000, 0x0000, 0x0000, 0x6600, 0x0000,
    0x0000, 0x0066, 0x0000, 0x0000, 0x0000, 0x0000, 0x0067, 0x0000, 0x0000, 0x6665, 0x0000, 0x0000, 0x0000, 0x0000, 0x0065, 0x6600,
    0x0063, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6563, 0x0064, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6564,
    0x0064, 0x6564, 0x0000, 0x6469, 0x6969, 0x6400, 0x0064, 0x6564, 0x0063, 0x6568, 0x6200, 0x0064, 0x6864, 0x0000, 0x6268, 0x6563,
    0x0000, 0x6567, 0x6963, 0x0064, 0x6764, 0x0063, 0x6967, 0x6500, 0x0000, 0x6266, 0x6769, 0x6a68, 0x6768, 0x6a69, 0x6766, 0x6200,
    0x0000, 0x0062, 0x6566, 0x6666, 0x6666, 0x6666, 0x6562, 0x0000, 0x0000, 0x0000, 0x0062, 0x6364, 0x6664, 0x6362, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0062, 0x6662, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6661, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0000, 0x6500, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x6400, 0x0000, 0x0000, 0x0000,
]
POP_LMP = b"".join(struct.pack(">H", v) for v in POP)


def read_pak(path):
    """A pak's directory: name -> (filepos, filelen)."""
    with open(path, "rb") as f:
        magic, dirofs, dirlen = struct.unpack("<4sii", f.read(12))
        assert magic == b"PACK", path
        f.seek(dirofs)
        d = f.read(dirlen)
    return {d[i:i + 56].split(b"\0")[0].decode(): struct.unpack("<ii", d[i + 56:i + 64]) for i in range(0, dirlen, 64)}


def pak_file(path, name):
    pos, n = read_pak(path)[name]
    with open(path, "rb") as f:
        f.seek(pos)
        return f.read(n)


def write_pak(files):
    """A PACK image of (name, bytes) pairs: header, contents, directory."""
    body = b"".join(b for _, b in files)
    out = struct.pack("<4sii", b"PACK", 12 + len(body), 64 * len(files)) + body
    pos = 12
    for name, b in files:
        out += name.encode().ljust(56, b"\0") + struct.pack("<ii", pos, len(b))
        pos += len(b)
    return out


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

    def ready():
        """The page is up and has the program's word on its files. Polled, so
        a start the page answers with another reload (a refusal) is waited
        out too."""
        until = time.time() + 180
        while time.time() < until:
            try:
                if pg.evaluate("!!(window.quake && quake.ready && quake.contentPath !== undefined)"):
                    return
            except Exception:
                pass                    # the page is reloading
            time.sleep(0.2)
        raise SystemExit("the page did not come up")

    def start_audio():
        pg.mouse.click(450, 300)       # the first gesture: the overlay; audio runs
        pg.wait_for_function("quake.audio.ring().running", timeout=10000)

    def cd():
        return pg.evaluate("quake.cd.state()")

    def call(line):
        return pg.evaluate(f"quake.callLine({line!r})")

    def level(secs=0.8):
        """The CD's output RMS: the median of a few analyser windows over
        `secs`, after a moment for a change to reach the output."""
        time.sleep(0.3)
        got = []
        for _ in range(8):
            got.append(pg.evaluate("quake.cd.state().level"))
            time.sleep(secs / 8)
        return sorted(got)[len(got) // 2]

    pg.goto(f"http://127.0.0.1:{PORT}/index.html", wait_until="load")
    ready()
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
    br.close()
httpd.shutdown()

if fails:
    print("FAIL:", "; ".join(fails))
    raise SystemExit(1)
print("done: the player's files verified (registered from a dropped pak1, e2m1, the CD's tracks, level, pause, refusals, removal)")
