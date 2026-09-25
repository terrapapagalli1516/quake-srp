#!/usr/bin/env -S uv run --with playwright --script
"""Browser benchmark for the wasm build: where does a frame's time go?

Boots the real page (index.html + quake_wasm.wasm) in headless Chromium, pauses
the page's own requestAnimationFrame loop, and drives a DETERMINISTIC workload
through the same exports the page calls, one frame per rAF at Quake's fixed
1/72 s step (host_maxfps). Per frame it records:

  * JS side (any wasm build): `step` (the whole exp.step call), `copy` (the
    img.data.set of the RGBA framebuffer), `put` (ctx.putImageData), `js`
    (step+copy+put) and `raf` (the rAF-to-rAF period, which adds everything the
    browser does per frame: paint, composite, GC);
  * wasm side (a `--features bench` build, see quake-wasm/src/bench.rs): the
    frame phases input/sim/render3d/post3d/hud2d/menu/console/blend/pack, the
    engine RenderStats split of render3d (world/submodel/external/alias/particle/
    sprite/viewmodel) and its counters (faces, pixels, surface-cache hits).

and prints median / p95 per phase per resolution. `--native` runs the SAME
workloads through the SAME exports natively (`native_bench` in bench.rs) so each
phase gets a wasm/native ratio. Workloads (all at dt = 1/72):

  attract    boot_attract(): demo1 (e1m3) playing under the main menu
  demo1      boot_demo(): id's recorded e1m3 run, menu closed
  walk_e1m1  boot() + Esc: live server, scripted look-around + run (walkInput below)
  walk_e1m3  as walk_e1m1 after the console's `map e1m3` (the dense-sim map)
  fire_e1m1  walk_e1m1 with +attack held (muzzle-flash dynamic lights every shot)

Usage:
  uv run web/bench.py --build                 # build the bench wasm, run the default set
  uv run web/bench.py WEBDIR                  # benchmark index.html + quake_wasm.wasm in WEBDIR
  options: --workloads demo1,walk_e1m1  --res 320x200,640x400,1280x800
           --frames 600 --warmup 60  --native  --profile  --live SECONDS
           --hash-every N (framebuffer FNV hashes, to prove two builds render
           identically)  --json OUT.json  --port 8230 (or QUAKE_VERIFY_PORT)

Hashes: attract frames include the menu cursor, animated on the App clock,
which also counts the page's own frames before the harness took over; prove
build identity on the other workloads.

Timer resolution: the server sends COOP/COEP so the page is cross-origin
isolated and performance.now() is ~5 us, not the 100 us of a normal page.
Headless Chromium composites in software: `put` and `raf - js` are indicative
of the browser's share, not of a GPU-accelerated desktop browser.
"""
import argparse, atexit, collections, functools, gzip, http.server, json, os, shutil
import socketserver, statistics, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
PROJ = os.path.dirname(HERE)

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("webdir", nargs="?", help="dir with index.html + quake_wasm.wasm (default: web/)")
ap.add_argument("--build", action="store_true",
                help="cargo-build the --features bench wasm and serve it from a temp dir")
ap.add_argument("--workloads", default="demo1,attract,walk_e1m1,fire_e1m1,walk_e1m3")
ap.add_argument("--res", default="320x200,640x400,1280x800")
ap.add_argument("--frames", type=int, default=600)
ap.add_argument("--warmup", type=int, default=60)
ap.add_argument("--native", action="store_true", help="also run the native twin (cargo test)")
ap.add_argument("--profile", action="store_true", help="CDP CPU profile of each fixed run")
ap.add_argument("--live", type=float, default=0.0,
                help="also sample the page's OWN loop for N seconds (frame pacing; use "
                     "--vsync: uncapped headless rAF is not a display, and with the 72 fps "
                     "cap its rate follows the page's own work)")
ap.add_argument("--hash-every", type=int, default=0)
ap.add_argument("--json", help="write every raw per-frame series here")
ap.add_argument("--port", type=int, default=int(os.environ.get("QUAKE_VERIFY_PORT", "8230")))
ap.add_argument("--vsync", action="store_true",
                help="keep Chromium's 60 Hz rAF cap (default: uncapped, so `raf` shows browser cost)")
args = ap.parse_args()

# --- assemble the web dir ---------------------------------------------------
if args.build:
    wasm_crate = os.path.join(PROJ, "quake-wasm")
    subprocess.run(["cargo", "build", "--release", "--target", "wasm32-unknown-unknown",
                    "--features", "bench", "--target-dir", "target/bench"],
                   cwd=wasm_crate, check=True)
    WEB = tempfile.mkdtemp(prefix="quake-bench-")
    atexit.register(shutil.rmtree, WEB, True)
    shutil.copy(os.path.join(HERE, "index.html"), WEB)
    shutil.copy(os.path.join(wasm_crate, "target/bench/wasm32-unknown-unknown/release/quake_wasm.wasm"), WEB)
else:
    WEB = args.webdir or HERE
wasm_path = os.path.join(WEB, "quake_wasm.wasm")
wasm_bytes = open(wasm_path, "rb").read()


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        # Cross-origin isolation -> performance.now() at ~5 us resolution.
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *a):
        pass


socketserver.ThreadingTCPServer.allow_reuse_address = True
httpd = socketserver.ThreadingTCPServer(("127.0.0.1", args.port),
                                        functools.partial(Handler, directory=WEB))
httpd.daemon_threads = True
threading.Thread(target=httpd.serve_forever, daemon=True).start()

# --- in-page instrumentation (runs before the page's own script) -------------
INIT_JS = r"""
(() => {
  // 1. A gate on requestAnimationFrame: once __benchRAFPaused is set the page's
  //    loop stops rescheduling itself and the harness drives frames instead.
  const raf = window.requestAnimationFrame.bind(window);
  window.__benchRAF = raf;
  window.__benchRAFPaused = false;
  window.requestAnimationFrame = (cb) => window.__benchRAFPaused ? 0 : raf(cb);
  // 2. Startup timing + the bench build's clock import (quake_bench.now_ms). An
  //    import the module does not declare is ignored, so the stock wasm loads too.
  //    In live mode the exports are re-wrapped so step() is timed per call.
  //    Both entry points are hooked: the page streams (instantiateStreaming,
  //    whose start is the start of the download) and falls back to buffered
  //    instantiate.
  window.__startup = {};
  window.__live = { step: [], put: [], raf: [], skipped: 0 };
  const hook = (inst) => async function (src, imports) {
    imports = Object.assign({}, imports || {});
    imports.quake_bench = { now_ms: () => performance.now() };
    window.__startup.instantiate_start = performance.now();
    const r = await inst.call(WebAssembly, src, imports);
    window.__startup.instantiate_end = performance.now();
    const real = r.instance.exports;
    const ex = Object.create(null);
    for (const k of Object.keys(real)) ex[k] = real[k];
    ex.boot_attract = function () {
      const t0 = performance.now(); const v = real.boot_attract();
      window.__startup.boot_attract_ms = performance.now() - t0; return v;
    };
    if (window.__benchLiveWrap) {
      // Host_FilterTime's 72 fps cap: step() returns 0 on a refresh it skipped.
      // Record only the frames that ran; count the skipped calls.
      ex.step = function (dt) {
        const t0 = performance.now(); const ran = real.step(dt);
        if (ran === 0) { window.__live.skipped++; return ran; }
        window.__live.step.push(performance.now() - t0);
        window.__live.raf.push(t0);
        return ran;
      };
    }
    if (window.__startup.first_step === undefined) {
      const s = ex.step;
      ex.step = function (dt) {
        if (window.__startup.first_step === undefined) window.__startup.first_step = performance.now();
        return s(dt);
      };
    }
    return { module: r.module, instance: { exports: ex } };
  };
  WebAssembly.instantiate = hook(WebAssembly.instantiate);
  if (WebAssembly.instantiateStreaming) {
    WebAssembly.instantiateStreaming = hook(WebAssembly.instantiateStreaming);
  }
  if (window.__benchLiveWrap) {
    const put = CanvasRenderingContext2D.prototype.putImageData;
    CanvasRenderingContext2D.prototype.putImageData = function (...a) {
      const t0 = performance.now(); put.apply(this, a);
      window.__live.put.push(performance.now() - t0);
    };
  }

  // The live-walk input script (native twin: walk_input in quake-wasm/src/bench.rs),
  // a 720-frame (10 s) cycle that looks everywhere and comes back: a slow 360 deg
  // sweep in place, run forward 1 s, about-face, run back, about-face, idle.
  // Returns [forward fraction, yaw degrees to turn RIGHT this frame].
  function walkInput(f) {
    const ph = f % 720;
    if (ph < 360) return [0, 1];
    if (ph < 432) return [1, 0];
    if (ph < 504) return [0, 2.5];
    if (ph < 576) return [1, 0];
    if (ph < 648) return [0, 2.5];
    return [0, 0];
  }
  // Boot a workload (native twin: start() in bench.rs).
  function startWorkload(e, wl) {
    if (wl === 'attract') return e.boot_attract() === 1;
    if (wl === 'demo1') return e.boot_demo() === 1;
    if (wl.startsWith('walk_') || wl.startsWith('fire_')) {
      const map = wl.slice(5);
      if (e.boot() !== 1) return false;
      if (map !== 'e1m1') {
        e.console_toggle();
        for (const ch of 'map ' + map) e.console_char(ch.charCodeAt(0));
        e.console_enter();
        if (e.console_visible()) e.console_toggle();
      }
      if (e.menu_visible()) e.menu_cancel();
      return e.in_walk_mode() === 1 && !e.menu_visible();
    }
    return false;
  }
  function fnv(bytes) {   // FNV-1a over 32-bit words of an offset-0 RGBA buffer
    const u = new Uint32Array(bytes.buffer, 0, bytes.length >> 2);
    let h = 0x811c9dc5;
    for (let i = 0; i < u.length; i++) h = Math.imul(h ^ u[i], 0x01000193);
    return (h >>> 0).toString(16).padStart(8, '0');
  }

  // 3. The fixed-step run: one frame per rAF, dt = cfg.dt, timed per phase.
  window.__benchRun = async (cfg) => {
    const e = exp;   // the page's top-level `let exp` (a global binding)
    window.__benchRAFPaused = true;
    await new Promise(r => setTimeout(r, 100));   // let the page's in-flight frame drain
    // Realign Host_FilterTime's gate: a refresh the page's loop skipped left
    // realtime ahead of the last frame, and that leftover would ride into the
    // first measured frame. One long step runs a frame and consumes it, so
    // every measured step advances exactly cfg.dt (as before the 72 fps cap).
    e.step(0.2);
    if (!startWorkload(e, cfg.workload)) return { error: 'workload failed to boot: ' + cfg.workload };
    if (typeof hideOverlayForever === 'function') hideOverlayForever();   // the click-to-play scrim
    e.set_resolution(cfg.w, cfg.h);
    const W = e.width(), H = e.height();
    const canvas = document.getElementById('c');
    const ctx = canvas.getContext('2d');
    canvas.width = W; canvas.height = H;
    const img = ctx.createImageData(W, H);
    const bench = typeof e.bench_enable === 'function';
    const names = bench ? new TextDecoder().decode(
      new Uint8Array(e.memory.buffer, e.bench_names_ptr(), e.bench_names_len())).split(',') : [];
    const cols = { raf: [], step: [], copy: [], put: [], js: [] };
    for (const n of names) cols[n] = [];
    const hashes = [];
    if (bench) e.bench_enable(1);
    const total = cfg.warmup + cfg.frames;
    let f = 0, last = -1;
    await new Promise(resolve => {
      function tick(ts) {
        if (cfg.workload.startsWith('walk_') || cfg.workload.startsWith('fire_')) {
          const [fwd, turn] = walkInput(f);
          e.set_move(fwd, 0); e.look(-turn, 0);
          e.set_attack(cfg.workload.startsWith('fire_') ? 1 : 0);
        }
        const t0 = performance.now();
        e.step(cfg.dt);
        const t1 = performance.now();
        img.data.set(new Uint8Array(e.memory.buffer, e.framebuffer(), W * H * 4));
        const t2 = performance.now();
        ctx.putImageData(img, 0, 0);
        const t3 = performance.now();
        if (f >= cfg.warmup) {
          cols.raf.push(last < 0 ? NaN : ts - last);
          cols.step.push(t1 - t0); cols.copy.push(t2 - t1);
          cols.put.push(t3 - t2); cols.js.push(t3 - t0);
          for (let i = 0; i < names.length; i++) cols[names[i]].push(e.bench_value(i));
        }
        if (cfg.hashEvery && f % cfg.hashEvery === 0) hashes.push(fnv(img.data));
        last = ts; f++;
        if (f < total) window.__benchRAF(tick); else resolve();
      }
      window.__benchRAF(tick);
    });
    if (bench) e.bench_enable(0);
    return { W, H, cols, hashes, bench, isolated: self.crossOriginIsolated };
  };
})();
"""


def pct(xs, p):
    xs = sorted(x for x in xs if x == x)
    if not xs:
        return float("nan")
    k = min(len(xs) - 1, max(0, int(round(p / 100 * (len(xs) - 1)))))
    return xs[k]


def med(xs):
    return pct(xs, 50)


def demangle(n):
    """Legacy Rust mangling -> path (enough for a profile table)."""
    if not n.startswith("_ZN"):
        return n
    s, i, parts = n[3:], 0, []
    while i < len(s) and s[i].isdigit():
        j = i
        while s[j].isdigit():
            j += 1
        ln = int(s[i:j])
        part = s[j:j + ln]
        i = j + ln
        if part.startswith("h") and len(part) == 17:
            break
        parts.append(part.replace("$LT$", "<").replace("$GT$", ">").replace("$u20$", " ")
                     .replace("$u7b$", "{").replace("$u7d$", "}").replace("..", "::"))
    return "::".join(parts)


def summarize_profile(prof, top=22):
    nodes = {n["id"]: n for n in prof["nodes"]}
    parent = {c: n["id"] for n in prof["nodes"] for c in n.get("children", [])}
    cnt = collections.Counter(prof["samples"])
    selfc, incl = collections.Counter(), collections.Counter()
    for nid, c in cnt.items():
        selfc[demangle(nodes[nid]["callFrame"]["functionName"] or "(anon)")] += c
        seen, x = set(), nid
        while x is not None:
            nm = demangle(nodes[x]["callFrame"]["functionName"] or "(anon)")
            if nm not in seen:
                incl[nm] += c
                seen.add(nm)
            x = parent.get(x)
    tot = max(1, sum(cnt.values()))
    lines = [f"    {'self%':>6} {'incl%':>6}  function"]
    for nm, c in selfc.most_common(top):
        lines.append(f"    {100 * c / tot:6.1f} {100 * incl[nm] / tot:6.1f}  {nm[:100]}")
    return "\n".join(lines)


# --- run ----------------------------------------------------------------------
from playwright.sync_api import sync_playwright  # noqa: E402

flags = ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"]
if not args.vsync:
    flags += ["--disable-frame-rate-limit", "--disable-gpu-vsync"]
workloads = [w for w in args.workloads.split(",") if w]
resolutions = [tuple(int(v) for v in r.split("x")) for r in args.res.split(",") if r]
results = {"wasm": [], "native": [], "startup": {}, "live": {}, "profiles": {}}
load = open("/proc/loadavg").read().split()[:3] if os.path.exists("/proc/loadavg") else []
results["load_before"] = load

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=flags)

    def fresh_page(live_wrap=False):
        pg = br.new_page(viewport={"width": 1020, "height": 700})
        errs = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        if live_wrap:
            pg.add_init_script("window.__benchLiveWrap = true;")
        pg.add_init_script(INIT_JS)
        pg.goto(f"http://127.0.0.1:{args.port}/index.html", wait_until="load")
        pg.wait_for_function("typeof exp !== 'undefined' && !!exp && typeof exp.boot === 'function'",
                             timeout=120000)
        pg.wait_for_function("document.getElementById('status').textContent.includes('ready')",
                             timeout=60000)
        return pg, errs

    pg, errs = fresh_page()
    st = pg.evaluate("""() => {
        const s = window.__startup;
        const r = performance.getEntriesByType('resource').find(e => e.name.endsWith('quake_wasm.wasm'));
        return { fetch_ms: r ? r.responseEnd - r.startTime : null,
                 // From the end of the download to an instantiated module (the
                 // streaming path compiles during the download).
                 instantiate_ms: r ? s.instantiate_end - r.responseEnd : null,
                 boot_attract_ms: s.boot_attract_ms,
                 nav_to_first_frame_ms: s.first_step,
                 isolated: self.crossOriginIsolated }
    }""")
    st["wasm_mb"] = round(len(wasm_bytes) / 1048576, 2)
    st["wasm_gzip6_mb"] = round(len(gzip.compress(wasm_bytes, 6)) / 1048576, 2)
    results["startup"] = st
    if not st["isolated"]:
        print("WARNING: page not cross-origin isolated; performance.now() is coarse (100 us)")

    for wl in workloads:
        for (w, h) in resolutions:
            cdp = None
            if args.profile:
                cdp = pg.context.new_cdp_session(pg)
                cdp.send("Profiler.enable")
                cdp.send("Profiler.setSamplingInterval", {"interval": 100})
                cdp.send("Profiler.start")
            r = pg.evaluate("cfg => window.__benchRun(cfg)", {
                "workload": wl, "w": w, "h": h, "dt": 1 / 72, "warmup": args.warmup,
                "frames": args.frames, "hashEvery": args.hash_every})
            if cdp is not None:
                prof = cdp.send("Profiler.stop")["profile"]
                results["profiles"][f"{wl}@{w}x{h}"] = summarize_profile(prof)
                cdp.detach()
            if "error" in r:
                print("ERROR:", r["error"])
                continue
            r.update({"side": "wasm", "workload": wl, "w": r["W"], "h": r["H"]})
            results["wasm"].append(r)
            print(f"  wasm {wl} {r['W']}x{r['H']}: step median {med(r['cols']['step']):.2f} ms",
                  file=sys.stderr)

    if args.live > 0:
        lp, lerrs = fresh_page(live_wrap=True)
        lp.evaluate("window.__live = { step: [], put: [], raf: [], skipped: 0 }")
        time.sleep(args.live)
        lv = lp.evaluate("window.__live")
        st_ = lv["raf"]
        gaps = [b - a for a, b in zip(st_, st_[1:])]
        results["live"] = {
            "res": lp.evaluate("[exp.width(), exp.height()]"), "frames": len(st_),
            "period_median": med(gaps), "period_p95": pct(gaps, 95), "period_p99": pct(gaps, 99),
            "step_median": med(lv["step"]), "step_p95": pct(lv["step"], 95),
            "put_median": med(lv["put"]), "long_frames_over_20ms": sum(1 for g in gaps if g > 20),
            "skipped": lv.get("skipped", 0), "seconds": args.live,
        }
        errs += lerrs
        lp.close()
    br.close()
httpd.shutdown()

if args.native:
    env = dict(os.environ, QUAKE_BENCH_WORKLOADS=",".join(workloads),
               QUAKE_BENCH_RES=",".join(f"{w}x{h}" for w, h in resolutions),
               QUAKE_BENCH_FRAMES=str(args.frames), QUAKE_BENCH_WARMUP=str(args.warmup))
    out = subprocess.run(["cargo", "test", "--release", "--features", "bench", "--lib", "--target-dir",
                          "target/bench-native", "--", "--ignored", "--nocapture", "native_bench"],
                         cwd=os.path.join(PROJ, "quake-wasm"), env=env, capture_output=True, text=True)
    for line in out.stdout.splitlines():
        if line.startswith("BENCHJSON "):
            results["native"].append(json.loads(line[len("BENCHJSON "):]))
    if not results["native"]:
        print(out.stdout[-2000:], out.stderr[-2000:])

results["load_after"] = open("/proc/loadavg").read().split()[:3] if os.path.exists("/proc/loadavg") else []

# --- report -------------------------------------------------------------------
ROWS = ["step", "input", "sim", "render3d", "world", " pvs", " sort", " setup", " light",
        " surf", "submodel", "external", "alias", "particle", "sprite", "viewmodel", "post3d",
        "hud2d", "menu", "console", "blend", "pack", "copy", "put", "js", "raf"]
# Indented rows are world-pass sub-phases (" setup" includes the raster itself).
SUBKEY = {" pvs": "world_pvs", " sort": "world_sort", " setup": "world_setup",
          " light": "world_light", " surf": "world_surf"}
COUNTERS = ["faces_pvs_culled", "faces_frustum_culled", "faces_drawn", "world_tris", "world_px",
            "surf_hits", "surf_misses", "surf_rebakes", "surf_bypass_bakes", "sub_faces_drawn"]
print(f"\nquake-rust browser bench — {len(wasm_bytes) / 1048576:.1f} MB wasm, frames={args.frames} "
      f"warmup={args.warmup}, dt=1/72, load {' '.join(results['load_before'])} -> "
      f"{' '.join(results['load_after'])}")
s = results["startup"]
print(f"startup: fetch {s['fetch_ms']:.0f} ms (local), instantiated {s['instantiate_ms']:.0f} ms "
      f"after the download, "
      f"boot_attract {s['boot_attract_ms']:.0f} ms, navigation->first frame "
      f"{s['nav_to_first_frame_ms']:.0f} ms; gzip -6 size {s['wasm_gzip6_mb']} MB")
native_by = {(n["workload"], n["w"], n["h"]): n["values"] for n in results["native"]}
for wl in workloads:
    runs = [r for r in results["wasm"] if r["workload"] == wl]
    if not runs:
        continue
    head = f"\n{wl:<11}" + "".join(f"| {r['w']}x{r['h']:<5} med   p95  nat  x " for r in runs)
    print(head)
    for row in ROWS:
        key = SUBKEY.get(row, row)
        if not any(key in r["cols"] for r in runs):
            continue
        line = f"  {row:<9}"
        for r in runs:
            xs = r["cols"].get(key, [])
            nat = native_by.get((wl, r["w"], r["h"]), {}).get(key)
            nm = med(nat) if nat else float("nan")
            m = med(xs)
            ratio = m / nm if nm and nm > 0.005 else float("nan")
            line += f"| {m:9.3f} {pct(xs, 95):6.2f} {nm:5.2f} {ratio:4.1f} "
        print(line)
    for row in COUNTERS:
        if any(row in r["cols"] for r in runs):
            print(f"  {row[:9]:<9}" + "".join(f"| {med(r['cols'][row]):>28.0f} " for r in runs))
    for r in runs:
        if r["hashes"]:
            print(f"  hashes {r['w']}x{r['h']}: {' '.join(r['hashes'])}")
    for r in runs:
        key = f"{wl}@{r['w']}x{r['h']}"
        if key in results["profiles"]:
            print(f"  CPU profile {key}:\n{results['profiles'][key]}")
if results["live"]:
    lv = results["live"]
    print(f"\nlive page loop ({'uncapped rAF' if not args.vsync else '60 Hz rAF'}, attract at "
          f"{lv['res'][0]}x{lv['res'][1]}, real dt): {lv['frames']} frames, period median "
          f"{lv['period_median']:.2f} / p95 {lv['period_p95']:.2f} / p99 {lv['period_p99']:.2f} ms, "
          f"step median {lv['step_median']:.2f} / p95 {lv['step_p95']:.2f}, put median "
          f"{lv['put_median']:.2f}, frames >20 ms: {lv['long_frames_over_20ms']}; "
          f"{lv['frames'] / lv['seconds']:.1f} host frames/s, {lv['skipped']} refreshes skipped "
          f"by the 72 fps cap")
print("\nrows: ms per frame (median, p95); nat = native median of the same workload via "
      "native_bench; x = wasm/native. raf - js = the browser's own per-frame work.")
if errs:
    print("page errors:", errs[-5:])
if args.json:
    json.dump(results, open(args.json, "w"))
    print("raw series ->", args.json)
