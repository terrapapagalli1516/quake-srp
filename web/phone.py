#!/usr/bin/env -S uv run --with playwright --script
"""The phone kit: what a frame costs on a real phone, as one table.

A phone is not a slow desktop: its eight cores are three kinds (an Android
phone: one prime, four mid, three little), a governor sets their clocks by
how busy they were a moment ago, and they are capped as the phone warms. So
the same frame costs one thing drawn back to back (`timedemo`) and another
drawn once a refresh with the cores idle in between — and the second is the
game. This script measures both on the phone itself, over USB:

  uv run --with playwright web/phone.py              # the page open in the phone's Chrome, as it is
  uv run --with playwright web/phone.py DEPLOY       # this deploy dir, served to the phone (adb reverse)
  uv run --with playwright web/phone.py DEPLOY --local   # no phone: the local Chromium, a phone-sized page

For each pixel size (--px, default 2,1) and thread count (--threads, default
4,6,8) — and each value of one more cvar, if asked (--cvar r_perspspan=16,8,4,1)
— it runs `timedemo demo1`, then --secs (default 60) of `playdemo demo1`
paced by the page's own loop, and prints a row:

  size        the frame, in pixels
  timedemo    frames per second, back to back (the cores busy throughout)
  fps         frames shown per second in play; >20 the frames more than 20 ms apart
  wait        the page's wait for a frame, tick to answer: median / p95 / p99 ms
  step        the program's own time for the frame, and its phases sim / 3-D /
              2-D (a `--features bench` build; else blank). wait - step is the
              wake-up of the program's worker and the turn around the frame
  put         the page's upload and draw call (the GPU's own work is not in it)
  MHz, cap    each kind of core's clock in play, and the lowest cap it was held to
  load        how busy each kind was, %
  skin        the phone's skin temperature, start -> end of the row
  panel       the display's own refresh through the row (SurfaceFlinger's active
              mode: an adaptive panel idles at 60 Hz and runs at 120 only while a
              finger moves), and with --touch whether a finger was kept moving

The phone: USB debugging on, plugged in, Chrome in front (the tab only runs
there; if another app comes to the front the script waits and redoes the
interrupted row; it restarts Chrome itself only from the launcher). Nothing
on the phone is changed: with no DEPLOY the tab's own `vid_pixelsize` and
`r_threads` (and --cvar's) are set for each row and put back at the end; with one, the page
is a new tab on `http://localhost:PORT` (a secure context, so the threads
build runs), closed at the end with its storage cleared and the port
forwarding removed. --top prints the busiest threads mid-row (which core
runs what). --cool SECONDS rests the phone before each timedemo (the page's
ticks paused) until no core is capped, or that long. --touch keeps a finger
moving on the screen through every row (`adb shell input swipe`, slow, in
the middle of the picture, where a demo ignores it): play always has one,
and it is what takes an adaptive panel to 120 Hz.

The frame is the page's box, so every row says what the page had: a folding
phone must be open flat (its posture is read with each row, and a row taken
in any other is refused and waited out: half folded, the page has half the
screen), and the row is marked `windowed` unless the page is fullscreen.
Chrome on Android hides its bars for a page's fullscreen only if no DevTools
client is attached at that moment: asked for while this script is connected
(by its own tap, or by a finger), `document.fullscreenElement` is set but
the bars stay and the page keeps its windowed box. So the script's own first
tap has the page's request turned off, and --fullscreen lets go of the
browser, presses the page's fullscreen chord as a key event from Android
(`input keycombination`: Alt+Enter, `vid_altenter`), and connects again. It
never leaves fullscreen.

adb is $ADB, else `adb` on the PATH. Chrome only (its DevTools socket).
"""
import argparse, json, os, re, shutil, signal, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import isolated  # noqa: E402

ADB = os.environ.get("ADB") or shutil.which("adb") or os.path.expanduser("~/.local/opt/platform-tools/adb")
CHROME = "com.android.chrome"
CHROME_MAIN = CHROME + "/com.google.android.apps.chrome.Main"
DEVTOOLS_PORT = 9222
TIMEDEMO_LINE = re.compile(r"^(-?\d+) frames +(\d+\.\d) seconds +(\d+\.\d) fps$")


def adb(*args, timeout=20):
    return subprocess.run([ADB, *args], capture_output=True, text=True, timeout=timeout).stdout


# --- The phone's state: clocks, caps, load, temperatures ----------------------

# One shell round: each core's clock and cap (kHz), its jiffies, the thermal
# service's temperatures.
SAMPLE = ("cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq; echo ---;"
          "cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_max_freq; echo ---;"
          "cat /sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq; echo ---;"
          "grep '^cpu[0-9]' /proc/stat; echo ---;"
          "dumpsys thermalservice 2>/dev/null | grep -E 'Thermal Status|mName=' | head -24; echo ---;"
          "dumpsys SurfaceFlinger 2>/dev/null | grep -m1 'activeMode='")


def parse_sample(text):
    """{cur, cap, top: MHz per core; stat: (busy, total) jiffies per core; temps; status}."""
    part = text.split("---")
    if len(part) < 5:
        return None
    mhz = lambda s: [int(x) // 1000 for x in s.split()]
    stat = []
    for line in part[3].strip().splitlines():
        f = [int(x) for x in line.split()[1:]]
        idle = f[3] + f[4]
        stat.append((sum(f) - idle, sum(f)))
    temps, status = {}, 0
    for line in part[4].splitlines():
        m = re.search(r"mValue=([\d.]+).*mName=(\w+)", line)
        if m:
            temps.setdefault(m[2], float(m[1]))
        m = re.search(r"Thermal Status: (\d+)", line)
        if m:
            status = int(m[1])
    # The first display's active mode: the panel's refresh right now.
    hz = re.search(r"vsyncRate=([\d.]+)", part[5]) if len(part) > 5 else None
    return {"cur": mhz(part[0]), "cap": mhz(part[1]), "top": mhz(part[2]), "stat": stat, "temps": temps, "status": status,
            "hz": round(float(hz[1])) if hz else None}


def phone_sample():
    return parse_sample(adb("shell", SAMPLE))


def awake():
    """The phone is on and unlocked (a tab only runs then)."""
    power = adb("shell", "dumpsys power | grep mWakefulness=; dumpsys window | grep -m1 isKeyguardShowing=")
    return "mWakefulness=Awake" in power and "isKeyguardShowing=true" not in power


def posture():
    """A folding phone's posture (`OPENED`, `HALF_OPENED`, `CLOSED`...), or None on
    a phone that does not say (it does not fold)."""
    m = re.search(r"Committed state: DeviceState\{[^}]*name='(\w+)'", adb("shell", "cmd device_state state 2>/dev/null"))
    return m[1] if m else None


def local_sample():
    """The local machine's own clocks and load, for --local (no caps worth the name, no temperatures)."""
    import glob
    read = lambda pat: " ".join(open(f).read().strip() for f in sorted(glob.glob(pat), key=lambda p: int(re.search(r"cpu(\d+)", p)[1])))
    cur = read("/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq")
    cap = read("/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_max_freq")
    top = read("/sys/devices/system/cpu/cpu[0-9]*/cpufreq/cpuinfo_max_freq")
    stat = "".join(l for l in open("/proc/stat") if re.match(r"cpu\d", l))
    return parse_sample("---".join([cur, cap, top, stat, "", ""]))


def kinds(sample):
    """The cores grouped by their top clock, slowest first: [(top MHz, [core, ...])]."""
    by = {}
    for core, top in enumerate(sample["top"]):
        by.setdefault(top, []).append(core)
    return sorted(by.items())


def summarize(samples):
    """Over a row's samples, per kind of core: mean clock, lowest cap, load; the skin
    temperature first -> last; the worst thermal status."""
    s = [x for x in samples if x and x["cur"]]
    if len(s) < 2:
        return None
    out = {"kinds": [], "status": max(x["status"] for x in s)}
    for top, cores in kinds(s[0]):
        busy = sum(s[-1]["stat"][c][0] - s[0]["stat"][c][0] for c in cores)
        total = sum(s[-1]["stat"][c][1] - s[0]["stat"][c][1] for c in cores)
        out["kinds"].append({
            "cores": len(cores), "top": top,
            "mhz": round(sum(x["cur"][c] for x in s for c in cores) / (len(s) * len(cores))),
            "cap": min(x["cap"][c] for x in s for c in cores),
            "load": round(100 * busy / total) if total else 0})
    for name in ("SKIN", "AP", "BAT"):
        if name in s[0]["temps"]:
            out[name] = [s[0]["temps"][name], s[-1]["temps"].get(name)]
    out["panel"] = sorted({x["hz"] for x in s if x.get("hz")})   # every refresh the panel was seen at
    return out


# --- The page's side -----------------------------------------------------------

# Collect a stretch of the page's own loop: quake.live (a record a refresh)
# and, on a bench build, the program's values a frame.
LIVE_START = """async () => {
  const names = (await quake.text('bench_names')).split(',').filter(Boolean);
  window.__phone = { names, values: [] };
  if (names.length) { quake.onBench = v => window.__phone.values.push(v); await quake.call('bench_enable', 1); }
  quake.live = [];
}"""
LIVE_STOP = """async () => {
  const L = quake.live || []; quake.live = null;
  const { names, values } = window.__phone;
  if (names.length) { await quake.call('bench_enable', 0); quake.onBench = null; }
  const q = (a, p) => { if (!a.length) return null; const s = Float64Array.from(a).sort(); return +s[Math.min(s.length - 1, Math.floor(p * s.length))].toFixed(2); };
  const three = (a) => [q(a, .5), q(a, .95), q(a, .99)];
  const S = L.filter(x => x.shown), gaps = [];
  for (let i = 1; i < S.length; i++) gaps.push(S[i].t - S[i - 1].t);
  const refresh = []; for (let i = 1; i < L.length; i++) refresh.push(L[i].t - L[i - 1].t);
  const secs = L.length ? (L[L.length - 1].t - L[0].t) / 1000 : 0;
  const out = { secs: +secs.toFixed(1), refreshHz: secs ? +((L.length - 1) / secs).toFixed(1) : 0,
                fps: secs ? +(S.length / secs).toFixed(1) : 0, gap: three(gaps),
                over20: gaps.filter(x => x > 20).length, over34: gaps.filter(x => x > 34).length,
                wait: three(S.map(x => x.wait)), put: three(S.map(x => x.put)), copy: three(S.map(x => x.copy)) };
  // An instrumented page (the experiments' own) also says each turn's time and the worker's wake-up.
  for (const k of ['turn', 'wake']) if (S.length && S[0][k] !== undefined) out[k] = three(S.map(x => x[k]));
  if (S.length > 1 && S[0].spawns !== undefined) out.spawns = +((S[S.length - 1].spawns - S[0].spawns) / (S.length - 1)).toFixed(1);
  if (names.length && values.length) {
    const col = (n) => { const i = names.indexOf(n); return values.map(v => v[i]); };
    out.step = three(values.map(v => v.slice(0, 9).reduce((a, b) => a + b, 0)));
    out.phases = {};
    for (const n of ['sim', 'render3d', 'post3d', 'hud2d', 'pack', 'world', 'alias', 'viewmodel', 'bands', 'band_threads'])
      if (names.includes(n)) out.phases[n] = q(col(n), .5);
    out.phases.two_d = q(values.map(v => v[3] + v[4] + v[5] + v[6] + v[7] + v[8]), .5);
  }
  return out;
}"""
PAGE = """() => new Promise(done => {
  // The refresh as the page sees it: its own requestAnimationFrame over two seconds.
  let n = 0; const t0 = performance.now();
  const tick = (t) => { n++; if (t - t0 < 2000) requestAnimationFrame(tick); else done({
    ua: navigator.userAgent, cores: navigator.hardwareConcurrency, dpr: devicePixelRatio,
    inner: [innerWidth, innerHeight], screen: [screen.width, screen.height],
    fullscreen: !!document.fullscreenElement, wholeScreen: innerWidth === screen.width && innerHeight === screen.height,
    refreshHz: +((n - 1) / ((t - t0) / 1000)).toFixed(1),
    isolated: crossOriginIsolated, presenter: quake.presenter(), size: quake.size() }); };
  requestAnimationFrame(tick);
})"""
VIEW = "({ inner: [innerWidth, innerHeight], fullscreen: !!document.fullscreenElement && innerWidth === screen.width })"


class Finger:
    """A finger kept moving on the phone's screen: `adb shell input swipe`, back and
    forth across a few hundred pixels around (x, y), ten seconds a stroke, until
    stopped. Real touch events to Android (what takes an adaptive panel to 120 Hz);
    a demo ignores a touch that moves."""

    def __init__(self, x, y, reach=150):
        import threading
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, args=(x, y, reach), daemon=True)
        self.thread.start()

    def run(self, x, y, reach):
        a, b = x - reach, x + reach
        while not self.stop.is_set():
            # Only ever on the game's page: Chrome in front.
            if CHROME in adb("shell", "dumpsys activity activities | grep topResumedActivity"):
                adb("shell", "input", "swipe", str(a), str(y), str(b), str(y), "10000", timeout=30)
            else:
                time.sleep(1.0)
            a, b = b, a

    def lift(self):
        self.stop.set()
        self.thread.join(timeout=15)


class Interrupted(Exception):
    """The tab left the front mid-row."""


class Target:
    """The page under measurement and the machine it runs on."""

    def __init__(self, pg, phone):
        self.pg, self.phone = pg, phone
        self.cdp = pg.context.new_cdp_session(pg)
        self.sample = phone_sample if phone else local_sample

    def front(self):
        """Chrome in front, the tab visible, and a folding phone open flat."""
        if not self.phone:
            return True
        return CHROME in adb("shell", "dumpsys activity activities | grep topResumedActivity") \
            and self.pg.evaluate("document.visibilityState") == "visible" and posture() in (None, "OPENED")

    def check(self):
        if not self.front():
            raise Interrupted()

    def wait_front(self):
        """Until Chrome is in front again. From the launcher the script brings it
        back; from any other app (the phone is in use) it waits."""
        while not self.front():
            top = adb("shell", "dumpsys activity activities | grep topResumedActivity").strip()
            if not awake():
                print("  (the phone is asleep or locked; waiting)", flush=True)
            elif posture() not in (None, "OPENED"):
                print(f"  (the phone is {posture()}, not open flat: no row is taken like this; waiting)", flush=True)
            elif "launcher" in top.lower():
                print("  (the launcher is in front: starting Chrome again)", flush=True)
                adb("shell", "am", "start", "-n", CHROME_MAIN)
            else:
                print("  (waiting for Chrome to come to the front: " + top[-60:] + ")", flush=True)
            time.sleep(5)

    def call(self, name, *args):
        return self.pg.evaluate("l => quake.call(l)", " ".join([name, *map(str, args)]))

    def exec(self, line):
        self.pg.evaluate("l => quake.callLine('exec ' + l)", line)

    def cvar(self, name):
        return self.pg.evaluate("n => quake.callLine('cvar ' + n).then(r => r.text)", name)

    def tap(self, x, y):
        """A finger's tap (Playwright's touchscreen refuses on a connected browser)."""
        for kind, points in (("touchStart", [{"x": x, "y": y}]), ("touchEnd", [])):
            self.cdp.send("Input.dispatchTouchEvent", {"type": kind, "touchPoints": points})
            time.sleep(0.06)

    def start(self):
        """The first gesture, in the kit's own tab: a tap on "tap to start" (the
        sound starts). The page's request for fullscreen is turned off first: made
        through DevTools it leaves Chrome's bars up and the page laid out wrong."""
        if self.pg.evaluate("getComputedStyle(document.getElementById('overlay')).display") != "none":
            self.pg.evaluate("() => { wrap.requestFullscreen = () => Promise.reject(new Error('not with DevTools attached')); }")
            w, h = self.pg.evaluate("[innerWidth, innerHeight]")
            self.tap(w / 2, h / 2)
            time.sleep(1.5)
            self.pg.evaluate("() => { delete wrap.requestFullscreen; }")

    def cool(self, secs):
        """Rest the phone, the page's ticks paused, until no core is capped and the
        thermal status is 0, or `secs` have passed. Returns the seconds it took."""
        t0 = time.time()
        self.pg.evaluate("quake.pause()")
        try:
            while time.time() - t0 < secs:
                s = self.sample()
                if s and s["status"] == 0 and all(c >= t for c, t in zip(s["cap"], s["top"])):
                    break
                time.sleep(5)
        finally:
            self.pg.evaluate("quake.resume()")
            time.sleep(0.5)
        return round(time.time() - t0)

    def timedemo(self):
        """`timedemo demo1`: id's line, and the machine's state through it."""
        samples = [self.sample()]
        self.exec("timedemo demo1")
        t0 = time.time()
        time.sleep(0.5)
        while self.call("timedemo_running") and time.time() - t0 < 300:
            self.check()
            samples.append(self.sample())
            time.sleep(1.0)
        self.check()
        samples.append(self.sample())
        lines = [l for l in self.pg.evaluate("quake.text('console_text')").split("\n") if " frames " in l]
        m = TIMEDEMO_LINE.match(lines[-1].strip()) if lines else None
        return {"frames": int(m[1]) if m else 0, "fps": float(m[3]) if m else 0.0, "sys": summarize(samples)}

    def play(self, secs, top):
        """`secs` of demo1 at the page's own pace."""
        self.exec("playdemo demo1")
        time.sleep(1.0)
        samples = [self.sample()]
        self.pg.evaluate(LIVE_START)
        t0, threads = time.time(), None
        while time.time() - t0 < secs:
            time.sleep(min(5.0, max(0.1, secs - (time.time() - t0))))
            self.check()
            samples.append(self.sample())
            if top and self.phone and threads is None and time.time() - t0 > secs / 2:
                threads = adb("shell", "top -H -b -n 1 -m 14 -o TID,CPU,%CPU,CMD -s 3 | tail -14")
        out = self.pg.evaluate(LIVE_STOP)
        out["sys"] = summarize(samples)
        out["top"] = threads
        return out


def row(t, px, threads, extra, secs, cool, top):
    """One row: the settings (`extra`: one more cvar and its value, or None), the
    timedemo, the play; redone if the tab left the front or the phone was folded."""
    while True:
        try:
            t.wait_front()
            t.exec(f"vid_pixelsize {px}")
            t.exec(f"r_threads {threads}")
            if extra:
                t.exec(" ".join(extra))
            time.sleep(1.0)
            r = {"px": px, "threads": t.call("render_threads"), "size": t.pg.evaluate("quake.size()"),
                 "cvar": {extra[0]: t.cvar(extra[0])} if extra else {},
                 "at": time.strftime("%H:%M:%S"), "posture": posture() if t.phone else None, **t.pg.evaluate(VIEW)}
            if cool > 0:
                r["cooled"] = t.cool(cool)
            r["timedemo"] = t.timedemo()
            if secs > 0:
                r["play"] = t.play(secs, top)
            return r
        except Interrupted:
            print("  (the tab left the front: redoing this row)", flush=True)


def fmt3(v):
    return "/".join("-" if x is None else f"{x:.1f}" for x in v) if v else "-"


def show(r):
    """A row of the table (two lines when the machine's state is known)."""
    td, pl = r["timedemo"], r.get("play")
    line = f"px {r['px']}  {r['threads']} thr  " + "".join(f"{k} {v}  " for k, v in r["cvar"].items()) \
        + f"{r['size'][0]}x{r['size'][1]:<5}{'' if r['fullscreen'] else ' windowed'}{' UPRIGHT' if r['size'][1] > r['size'][0] else ''}" \
        + f" timedemo {td['fps']:6.1f} fps"
    if pl:
        line += f" | play {pl['fps']:5.1f} fps  >20ms {pl['over20']:<3} wait {fmt3(pl['wait'])}"
        if pl.get("step"):
            p = pl["phases"]
            line += f"  step {fmt3(pl['step'])} (sim {p['sim']:.1f}, 3-D {p['render3d']:.1f}, 2-D {p['two_d']:.1f})"
        for k in ("turn", "wake"):
            if pl.get(k):
                line += f"  {k} {fmt3(pl[k])}"
        if "spawns" in pl:
            line += f"  spawns {pl['spawns']}"
        line += f"  put {fmt3(pl['put'])}"
    print(line, flush=True)
    for name, part in (("timedemo", td), ("play", pl)):
        s = part and part.get("sys")
        if not s:
            continue
        k = s["kinds"]
        skin = s.get("SKIN")
        print(f"      {name:8} MHz {'/'.join(str(x['mhz']) for x in k)}  cap {'/'.join(str(x['cap']) for x in k)}"
              f"  load {'/'.join(str(x['load']) for x in k)}%"
              + (f"  skin {skin[0]} -> {skin[1]} C" if skin else "")
              + (f"  panel {'/'.join(map(str, s['panel']))} Hz" if s.get("panel") else "")
              + ("  finger moving" if r.get("touch") else "")
              + (f"  THERMAL STATUS {s['status']}" if s["status"] else ""), flush=True)
    if pl and pl.get("top"):
        print(pl["top"], flush=True)


def find_page(p, port, deploy):
    """Connect to the phone's Chrome and find the page: the kit's own tab on
    `port`, or the tab that runs the game."""
    br = p.chromium.connect_over_cdp(f"http://127.0.0.1:{DEVTOOLS_PORT}")
    pages = [pg for ctx in br.contexts for pg in ctx.pages]
    mine = [pg for pg in pages if pg.url.startswith(f"http://localhost:{port}/")] if deploy else \
           [pg for pg in pages if pg.evaluate("!!(window.quake && quake.call)")]
    if not mine:
        sys.exit("no quake page among the phone's tabs: " + ", ".join(pg.url for pg in pages))
    return br, mine[0]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("deploy", nargs="?", help="a deploy dir to serve to the phone (default: the page already open in its Chrome)")
    ap.add_argument("--local", action="store_true", help="no phone: the local Chromium at a phone's size (needs a deploy dir)")
    ap.add_argument("--px", default="2,1", help="pixel sizes (vid_pixelsize; 0 is Auto)")
    ap.add_argument("--threads", default="4,6,8", help="thread counts (r_threads; 0 is every thread)")
    ap.add_argument("--cvar", default="", help="one more cvar to step, NAME=V1,V2,... (r_perspspan=16,8,4,1)")
    ap.add_argument("--secs", type=float, default=60, help="seconds of play a row (0: the timedemo only)")
    ap.add_argument("--cool", type=float, default=0, help="rest before each timedemo until no core is capped, at most this many seconds")
    ap.add_argument("--top", action="store_true", help="print the busiest threads mid-row")
    ap.add_argument("--touch", action="store_true", help="keep a finger moving on the screen through every row (the phone)")
    ap.add_argument("--fullscreen", action="store_true", help="put the page in fullscreen first (the phone: a key chord from Android)")
    ap.add_argument("--query", default="?2026", help="the deploy page's query")
    ap.add_argument("--port", type=int, default=isolated.port(9100))
    ap.add_argument("--json", help="append each row here, a JSON line each")
    a = ap.parse_args()
    if a.local and not a.deploy:
        ap.error("--local needs a deploy dir")

    # A stopped run (a time limit's SIGTERM) still puts the tab back as it was.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit("stopped"))
    from playwright.sync_api import sync_playwright
    httpd = isolated.serve(a.deploy, a.port) if a.deploy else None
    url = f"http://localhost:{a.port}/index.html{a.query}"
    with sync_playwright() as p:
        if a.local:
            br = isolated.launch(p, ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"])
            # A phone-sized landscape viewport in fullscreen: 880x360 CSS px at 3.
            ctx = br.new_context(viewport={"width": 880, "height": 360}, device_scale_factor=3, is_mobile=True, has_touch=True)
            pg = ctx.new_page()
            pg.goto(url, wait_until="load")
        else:
            if "\tdevice" not in adb("devices"):
                sys.exit("no phone: `adb devices` lists none (USB debugging on? the cable?)")
            adb("forward", f"tcp:{DEVTOOLS_PORT}", "localabstract:chrome_devtools_remote")
            if a.deploy:
                adb("reverse", f"tcp:{a.port}", f"tcp:{a.port}")
                adb("shell", "am", "start", "-n", CHROME_MAIN, "-a", "android.intent.action.VIEW", "-d", url)
                time.sleep(3.0)
            br, pg = find_page(p, a.port, a.deploy)
        pg.wait_for_function("window.quake && quake.ready && quake.firstFrameAt > 0", timeout=180000)
        t = Target(pg, not a.local)
        t.wait_front()
        if a.deploy:
            t.start()
        if a.fullscreen and t.phone and not pg.evaluate(VIEW)["fullscreen"]:
            # No DevTools client may be attached while the page asks (the doc
            # above): let go, press the page's chord from Android, come back.
            br.close()
            if CHROME in adb("shell", "dumpsys activity activities | grep topResumedActivity"):
                adb("shell", "input", "keycombination", "57", "66")   # ALT_LEFT + ENTER
            time.sleep(3.0)
            br, pg = find_page(p, a.port, a.deploy)
            t = Target(pg, True)
            if not pg.evaluate(VIEW)["fullscreen"]:
                print("  (the page did not go fullscreen: its rows are windowed)", flush=True)
        if t.phone and pg.evaluate("!!document.fullscreenElement && innerHeight > innerWidth"):
            # Fullscreen but upright (the phone lies flat): the game is played
            # sideways, and the page's own fullscreen button locks it so.
            pg.evaluate("screen.orientation.lock('landscape').then(() => true).catch(() => false)")
            time.sleep(2.5)
        # (A run stopped while it rested the phone leaves the page's ticks paused.)
        pg.evaluate("() => { if (quake.paused) quake.resume(); }")
        name, _, values = a.cvar.partition("=")
        extras = [(name, v) for v in values.split(",") if v] if name else [None]
        saved = {n: t.cvar(n) for n in ["vid_pixelsize", "r_threads"] + ([name] if name else [])}
        out = open(a.json, "a") if a.json else None
        finger = None
        try:
            if a.touch and t.phone:
                w, h, dpr = pg.evaluate("[screen.width, screen.height, devicePixelRatio]")
                finger = Finger(round(w * dpr / 2), round(h * dpr / 2))
                time.sleep(1.5)
            page = pg.evaluate(PAGE)
            s = t.sample()
            page["kinds"] = [f"{len(c)} x {top} MHz" for top, c in kinds(s)] if s else []
            page["posture"] = posture() if t.phone else None
            print(json.dumps(page), flush=True)
            if out:
                out.write(json.dumps({"page": page, "url": pg.url, "at": time.strftime("%F %T")}) + "\n")
            for px in [int(x) for x in a.px.split(",")]:
                for threads in [int(x) for x in a.threads.split(",")]:
                    for extra in extras:
                        r = row(t, px, threads, extra, a.secs, a.cool, a.top)
                        r["touch"] = bool(finger)
                        show(r)
                        if out:
                            out.write(json.dumps(r) + "\n")
                            out.flush()
        finally:
            if finger:
                finger.lift()
            # The tab as it was found: its own cvars back (a kept config.cfg with them).
            for n, v in saved.items():
                t.exec(f"{n} {v}")
            time.sleep(0.5)
            print("cvars put back:", ", ".join(f"{n} {t.cvar(n)}" for n in saved), flush=True)
            if a.deploy and not a.local:
                # The kit's own tab: its storage cleared, the tab closed, the forwarding gone.
                try:
                    t.cdp.send("Storage.clearDataForOrigin", {"origin": f"http://localhost:{a.port}", "storageTypes": "all"})
                    pg.close()
                except Exception as e:  # the tab may be gone already
                    print("  (closing the tab:", e, ")")
                adb("reverse", "--remove", f"tcp:{a.port}")
        br.close()
    if httpd:
        httpd.shutdown()


if __name__ == "__main__":
    main()
