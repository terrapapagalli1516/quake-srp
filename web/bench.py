#!/usr/bin/env -S uv run --with playwright --script
"""Browser benchmark for the wasm build: where does a frame's time go?

Boots the real page (index.html, wasi.js, quake.wasm, id1/pak0.pak) in
headless Chromium, pauses the page's own ticks, and drives a DETERMINISTIC
workload one frame per requestAnimationFrame at Quake's fixed 1/72 s step
(host_maxfps), through the page's own tick and present (window.quake.tick).
Per frame it records:

  * page side (any build): `wait` (from posting the tick to the program's
    answer: the worker's whole turn — the host frame, its picture into a
    frame slot or left in place in shared memory, its sounds), `copy` (any
    copy of the frame the page makes: into the 2-D canvas's ImageData, or
    into a staging buffer where WebGL refuses a shared view; 0 when WebGL2
    takes the shared view), `put` (putImageData, or WebGL2's texture uploads
    and draw call — the GPU's own work is not in it), `js` (wait+copy+put:
    the main thread's time for the frame) and `raf` (the rAF-to-rAF period,
    which adds everything the browser does per frame, the GPU's included
    when it is the bottleneck);
  * program side (a `--features bench` build, see quake-wasm/src/bench.rs):
    the frame phases input/sim/render3d/post3d/hud2d/menu/console/blend/pack
    (their sum is `step`, the host frame), the engine RenderStats split of
    render3d (world/submodel/external/alias/particle/sprite/viewmodel) and its
    counters (faces, pixels, surface-cache hits).

and prints median / p95 per phase per resolution. `--native` runs the SAME
workloads natively (`native_bench` in bench.rs) so each phase gets a
wasm/native ratio. Workloads (all at dt = 1/72):

  attract    boot_attract: demo1 (e1m3) playing, the attract loop
  demo1      boot_demo: id's recorded e1m3 run, menu closed
  walk_e1m1  boot + Esc: live server, scripted look-around + run (walk_input
             in bench.rs)
  walk_e1m3  as walk_e1m1 after the console's `map e1m3` (the dense-sim map)
  fire_e1m1  walk_e1m1 with +attack held (muzzle-flash dynamic lights every shot)
  quad_e1m1  walk_e1m1 after the console's `impulse 255` (id's QuadCheat): the
             Quad's blue V_UpdatePalette cshift is on every frame (not in the
             default set; `--workloads quad_e1m1`)

A bench build boots each workload in the program (`bench_start`) and scripts
the walk's input there; a stock build gets the same boot and input as calls.

Threads: a `wasm32-wasip1-threads` build draws its 3-D view on the host's
thread workers (`r_threads`, 0 = as many as the host offers: PLATFORM.md,
"Threads"). `--threads 1,2,4,8` runs every workload and size at each count
(the program's `r_threads`), and `--build --threads-build` builds the bench
program for that target. The page runs the Classic profile (`?classic`: id's
game, the frames `quaketool play` hashes); `--video modern` switches to the
2026 profile's native picture first (`set_video`: square pixels, Hor+, sizes
past 1280x800), each --res then the size of the window it fills. The frames are the
same at every count, but the runs of one page share the game's random stream
(QuakeC's `random()`), so to compare hashes across counts run each count in
a page of its own (one invocation per `--threads` value, the same workloads).

Usage:
  uv run web/bench.py --build                 # build the bench wasm, run the default set
  uv run web/bench.py WEBDIR                  # benchmark the page in WEBDIR (PLATFORM.md's deploy dir)
  options: --workloads demo1,walk_e1m1  --res 320x200,640x400,1280x800
           --frames 600 --warmup 60  --native  --profile  --live SECONDS
           --latency SECONDS (input -> present in the page's own loop, at the
           first --res)
           --hash-every N (framebuffer FNV hashes, to prove two builds render
           identically; `quaketool play --hash-every` prints the same natively)
           --json OUT.json  --port 8230 (or QUAKE_VERIFY_PORT)

Hashes: attract frames include the menu cursor, animated on the App clock,
which also counts the page's own frames before the harness took over; prove
build identity on the other workloads.

Timer resolution: the page is cross-origin isolated (it has to be, for its
SharedArrayBuffers), so performance.now() is ~5 us, not the 100 us of a
normal page. Headless Chromium composites in software: `put` and `raf - js`
are indicative of the browser's share, not of a GPU-accelerated desktop
browser.
"""
import argparse, collections, gzip, json, os, shutil, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
PROJ = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import isolated  # noqa: E402

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("webdir", nargs="?", help="the deploy dir to serve (default: web/)")
ap.add_argument("--build", action="store_true",
                help="cargo-build the --features bench program and serve it from quake-wasm/target/bench-web")
ap.add_argument("--workloads", default="demo1,attract,walk_e1m1,fire_e1m1,walk_e1m3")
ap.add_argument("--res", default="320x200,640x400,1280x800")
ap.add_argument("--frames", type=int, default=600)
ap.add_argument("--warmup", type=int, default=60)
ap.add_argument("--native", action="store_true", help="also run the native twin (cargo test)")
ap.add_argument("--profile", action="store_true", help="CDP CPU profile of each fixed run (main thread)")
ap.add_argument("--live", type=float, default=0.0,
                help="also sample the page's OWN loop for N seconds (frame pacing; use "
                     "--vsync: uncapped headless rAF is not a display, and with the 72 fps "
                     "cap its rate follows the page's own work)")
ap.add_argument("--latency", type=float, default=0.0,
                help="also measure input -> present for N seconds of key presses in the live walk")
ap.add_argument("--hash-every", type=int, default=0)
ap.add_argument("--json", help="write every raw per-frame series here")
ap.add_argument("--port", type=int, default=isolated.port(8230))
ap.add_argument("--threads", default="",
                help="comma list of r_threads values to run each workload at (0 = all the host offers)")
ap.add_argument("--threads-build", action="store_true",
                help="with --build: build for wasm32-wasip1-threads (the renderer's threads)")
ap.add_argument("--video", default="", help="classic or modern: the video cvars to run with (set_video)")
ap.add_argument("--vsync", action="store_true",
                help="keep Chromium's 60 Hz rAF cap (default: uncapped, so `raf` shows browser cost)")
ap.add_argument("--query", default="",
                help="the page's query string, e.g. ?canvas2d (the 2-D presenter) or ?lowlatency")
args = ap.parse_args()

# --- assemble the web dir ---------------------------------------------------
if args.build:
    wasm_crate = os.path.join(PROJ, "quake-wasm")
    target = "wasm32-wasip1-threads" if args.threads_build else "wasm32-wasip1"
    subprocess.run(["cargo", "build", "--release", "--target", target, "--bin", "quake",
                    "--features", "bench", "--target-dir", "target/bench"],
                   cwd=wasm_crate, check=True)
    WEB = os.path.join(wasm_crate, "target", "bench-web")
    os.makedirs(os.path.join(WEB, "id1"), exist_ok=True)
    isolated.copy_page(WEB)
    shutil.copy(os.path.join(wasm_crate, f"target/bench/{target}/release/quake.wasm"), WEB)
    pak = os.path.join(WEB, "id1", "pak0.pak")
    if not os.path.exists(pak):
        os.symlink(os.path.join(PROJ, "quake-data", "ID1", "PAK0.PAK"), pak)
else:
    WEB = args.webdir or HERE
wasm_bytes = open(os.path.join(WEB, "quake.wasm"), "rb").read()
httpd = isolated.serve(WEB, args.port)

# --- in-page instrumentation ---------------------------------------------------
BENCH_JS = r"""
(() => {
  // The live-walk input script for a stock build (a bench build runs the
  // same script in the program: walk_input in quake-wasm/src/bench.rs), a
  // 720-frame (10 s) cycle that looks everywhere and comes back: a slow 360
  // deg sweep in place, run forward 1 s, about-face, run back, about-face,
  // idle. Returns [forward fraction, yaw degrees to turn RIGHT this frame].
  function walkInput(f) {
    const ph = f % 720;
    if (ph < 360) return [0, 1];
    if (ph < 432) return [1, 0];
    if (ph < 504) return [0, 2.5];
    if (ph < 576) return [1, 0];
    if (ph < 648) return [0, 2.5];
    return [0, 0];
  }
  function isWalk(wl) {
    return wl.startsWith('walk_') || wl.startsWith('fire_') || wl.startsWith('quad_');
  }
  // Boot a workload with calls (a stock build), exactly as start() in
  // bench.rs does: the console lines are typed into the console.
  async function startWorkload(wl) {
    const q = quake.call;
    const type = async (line) => {
      q('console_toggle');
      for (const ch of line) q('console_char', ch.charCodeAt(0));
      q('console_enter');
      if (await q('console_visible')) await q('console_toggle');
    };
    if (wl === 'attract') return await q('boot_attract') === 1;
    if (wl === 'demo1') return await q('boot_demo') === 1;
    if (!isWalk(wl)) return false;
    const map = wl.slice(5);
    if (await q('boot') !== 1) return false;
    if (map !== 'e1m1') await type('map ' + map);
    if (await q('menu_visible')) await q('menu_cancel');
    if (wl.startsWith('quad_')) await type('impulse 255');
    return await q('in_walk_mode') === 1 && !(await q('menu_visible'));
  }
  // The fixed-step run: one frame per rAF, dt = cfg.dt, timed per phase.
  window.__benchRun = async (cfg) => {
    quake.pause();
    await new Promise(r => setTimeout(r, 100));   // let the page's in-flight frame drain
    // The picture (set_video: `modern` is the 2026 profile's native picture,
    // sized as a window of cfg.w x cfg.h device pixels at one pixel a pixel;
    // else a video mode) and the renderer's threads.
    if (cfg.video) await quake.call('set_video', cfg.video);
    if (cfg.threads !== null) await quake.call('exec', 'r_threads ' + cfg.threads);
    const setSize = async () => {
      if (cfg.video === 'modern') { await quake.call('set_window', cfg.w, cfg.h); await quake.call('step', 0); }
      else await quake.call('set_resolution', cfg.w, cfg.h);
    };
    // One frozen frame at the run's size first: the worker's frame slots
    // grow to fit it now, not on the run's first (hashed) frame.
    await setSize();
    quake.tick(0);
    await new Promise(r => setTimeout(r, 100));
    const names = (await quake.text('bench_names')).split(',').filter(Boolean);
    const bench = names.length > 0;
    // Realign Host_FilterTime's gate: a refresh the page's loop skipped left
    // realtime ahead of the last frame, and that leftover would ride into the
    // first measured frame. One long step runs a frame and consumes it, so
    // every measured step advances exactly cfg.dt.
    await quake.call('step', 0.2);
    const ok = bench ? await quake.call('bench_start', cfg.workload) === 1 : await startWorkload(cfg.workload);
    if (!ok) return { error: 'workload failed to boot: ' + cfg.workload };
    hideOverlayForever();   // the click-to-play scrim
    await setSize();
    const [W, H] = [await quake.call('width'), await quake.call('height')];
    const cols = { raf: [], wait: [], copy: [], put: [], js: [], step: [] };
    for (const n of names) cols[n] = [];
    const values = [];
    quake.onBench = v => values.push(v);
    const hashes = [];
    if (bench) await quake.call('bench_enable', 1);
    const total = cfg.warmup + cfg.frames;
    let f = 0, last = -1;
    await new Promise(resolve => {
      function tick(ts) {
        if (!bench && isWalk(cfg.workload)) {
          const [fwd, turn] = walkInput(f);
          quake.callLine(`set_move ${fwd} 0`); quake.callLine(`look ${-turn} 0`);
          quake.callLine(`set_attack ${cfg.workload.startsWith('fire_') ? 1 : 0}`);
        }
        const r = quake.tick(cfg.dt);
        if (f >= cfg.warmup) {
          cols.raf.push(last < 0 ? NaN : ts - last);
          cols.wait.push(r.wait); cols.copy.push(r.copy);
          cols.put.push(r.put); cols.js.push(r.wait + r.copy + r.put);
        }
        if (cfg.hashEvery && f % cfg.hashEvery === 0) hashes.push(quake.frameHash());
        last = ts; f++;
        if (f < total) requestAnimationFrame(tick); else resolve();
      }
      requestAnimationFrame(tick);
    });
    if (bench) await quake.call('bench_enable', 0);
    quake.onBench = null;
    // The program's values, one BENCH record per frame after the warmup.
    for (const v of values.slice(values.length - cfg.frames)) {
      let step = 0;
      for (let i = 0; i < names.length; i++) {
        cols[names[i]].push(v[i]);
        if (i < 9) step += v[i];                   // the nine frame phases
      }
      cols.step.push(step);
    }
    if (!bench) delete cols.step;
    const threads = await quake.call('render_threads');
    return { W, H, cols, hashes, bench, threads, isolated: self.crossOriginIsolated };
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


def summarize_profile(prof, top=22):
    nodes = {n["id"]: n for n in prof["nodes"]}
    parent = {c: n["id"] for n in prof["nodes"] for c in n.get("children", [])}
    cnt = collections.Counter(prof["samples"])
    selfc, incl = collections.Counter(), collections.Counter()
    for nid, c in cnt.items():
        selfc[nodes[nid]["callFrame"]["functionName"] or "(anon)"] += c
        seen, x = set(), nid
        while x is not None:
            nm = nodes[x]["callFrame"]["functionName"] or "(anon)"
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
thread_counts = [int(t) for t in args.threads.split(",") if t] or [None]
resolutions = [tuple(int(v) for v in r.split("x")) for r in args.res.split(",") if r]
results = {"wasm": [], "native": [], "startup": {}, "live": {}, "latency": {}, "profiles": {}}
results["load_before"] = open("/proc/loadavg").read().split()[:3] if os.path.exists("/proc/loadavg") else []

with sync_playwright() as p:
    br = p.chromium.launch(headless=True, args=flags + isolated.gpu_flags())

    def fresh_page():
        pg = br.new_page(viewport={"width": 1020, "height": 700})
        errs = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: errs.append("PAGEERROR: " + str(e)))
        # The Classic profile: id's game, whose frames `quaketool play`
        # hashes natively (`--video modern` then switches the picture);
        # --query adds to it.
        query = "?classic" + args.query.replace("?", "&", 1)
        pg.goto(f"http://127.0.0.1:{args.port}/index.html{query}", wait_until="load")
        pg.wait_for_function("window.quake && quake.ready", timeout=120000)
        pg.wait_for_function("quake.firstFrameAt > 0", timeout=60000)
        pg.add_script_tag(content=BENCH_JS)
        return pg, errs

    pg, errs = fresh_page()
    st = pg.evaluate("""() => {
        const r = performance.getEntriesByType('resource');
        const end = (name) => { const e = r.find(x => x.name.endsWith(name)); return e ? e.responseEnd : null; };
        return { download_ms: quake.downloadedAt - quake.startedAt,
                 wasm_end_ms: end('quake.wasm'), pak_end_ms: end('pak0.pak'),
                 // From the end of the downloads to the program's first Sync:
                 // the worker's compile + instantiate, quake.rc's startup.
                 start_ms: quake.readyAt - quake.downloadedAt,
                 nav_to_first_frame_ms: quake.firstFrameAt,
                 isolated: self.crossOriginIsolated }
    }""")
    st["wasm_mb"] = round(len(wasm_bytes) / 1048576, 2)
    st["wasm_gzip6_mb"] = round(len(gzip.compress(wasm_bytes, 6)) / 1048576, 2)
    results["startup"] = st
    if not st["isolated"]:
        print("WARNING: page not cross-origin isolated")

    for t, wl, (w, h) in [(t, wl, r) for t in thread_counts for wl in workloads for r in resolutions]:
        cdp = None
        if args.profile:
            cdp = pg.context.new_cdp_session(pg)
            cdp.send("Profiler.enable")
            cdp.send("Profiler.setSamplingInterval", {"interval": 100})
            cdp.send("Profiler.start")
        r = pg.evaluate("cfg => window.__benchRun(cfg)", {
            "workload": wl, "w": w, "h": h, "dt": 1 / 72, "warmup": args.warmup,
            "frames": args.frames, "hashEvery": args.hash_every, "threads": t, "video": args.video})
        if cdp is not None:
            prof = cdp.send("Profiler.stop")["profile"]
            results["profiles"][f"{wl}@{w}x{h}"] = summarize_profile(prof)
            cdp.detach()
        if "error" in r:
            print("ERROR:", r["error"])
            continue
        r.update({"side": "wasm", "workload": wl, "w": r["W"], "h": r["H"]})
        results["wasm"].append(r)
        key = "step" if "step" in r["cols"] else "wait"
        print(f"  wasm {wl} {r['W']}x{r['H']} on {r['threads']} thread(s): {key} median "
              f"{med(r['cols'][key]):.2f} ms", file=sys.stderr)

    if args.live > 0:
        lp, lerrs = fresh_page()
        lp.evaluate("quake.live = []")
        time.sleep(args.live)
        lv = lp.evaluate("quake.live")
        shown = [x for x in lv if x["shown"]]
        stamps = [x["t"] for x in shown]
        gaps = [b - a for a, b in zip(stamps, stamps[1:])]
        results["live"] = {
            "res": lp.evaluate("quake.size()"), "refreshes": len(lv), "frames": len(shown),
            "period_median": med(gaps), "period_p95": pct(gaps, 95), "period_p99": pct(gaps, 99),
            "wait_median": med([x["wait"] for x in shown]), "wait_p95": pct([x["wait"] for x in shown], 95),
            "put_median": med([x["put"] for x in shown]), "long_frames_over_20ms": sum(1 for g in gaps if g > 20),
            "skipped": len(lv) - len(shown), "seconds": args.live,
        }
        errs += lerrs
        lp.close()

    if args.latency > 0:
        # The live walk, uncapped (a frame every refresh, so the 72 fps gate
        # adds nothing), keys pressed at random moments: each event's time to
        # the present of the first frame that consumed it.
        lp, lerrs = fresh_page()
        lp.evaluate("hideOverlayForever()")
        lw, lh = resolutions[0]
        video = f".then(() => quake.call('set_video', '{args.video}'))" if args.video else ""
        lp.evaluate(f"quake.call('boot').then(() => quake.call('menu_cancel'))"
                    f".then(() => quake.call('set_extras', 1)){video}.then(() => quake.call('set_resolution', {lw}, {lh}))")
        time.sleep(1.0)
        lp.evaluate("quake.latency = []")
        import random
        rnd = random.Random(7)
        t_end = time.time() + args.latency
        while time.time() < t_end:
            lp.keyboard.down("d")
            time.sleep(rnd.uniform(0.02, 0.08))
            lp.keyboard.up("d")
            time.sleep(rnd.uniform(0.02, 0.08))
        lat = lp.evaluate("quake.latency")
        results["latency"] = {"res": lp.evaluate("quake.size()"), "events": len(lat), "median": med(lat), "p95": pct(lat, 95),
                              "min": min(lat) if lat else float("nan"), "max": max(lat) if lat else float("nan")}
        errs += lerrs
        lp.close()
    br.close()
httpd.shutdown()

if args.native:
    env = dict(os.environ, QUAKE_BENCH_WORKLOADS=",".join(workloads),
               QUAKE_BENCH_RES=",".join(f"{w}x{h}" for w, h in resolutions),
               QUAKE_BENCH_FRAMES=str(args.frames), QUAKE_BENCH_WARMUP=str(args.warmup))
    out = subprocess.run(["cargo", "test", "--release", "--features", "bench", "--target-dir",
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
        " surf", "submodel", "external", "alias", "particle", "sprite", "viewmodel", "bands", "post3d",
        "hud2d", "menu", "console", "blend", "pack", "wait", "copy", "put", "js", "raf"]
# Indented rows are world-pass sub-phases (" setup" includes the raster itself).
SUBKEY = {" pvs": "world_pvs", " sort": "world_sort", " setup": "world_setup",
          " light": "world_light", " surf": "world_surf"}
COUNTERS = ["faces_pvs_culled", "faces_frustum_culled", "faces_drawn", "world_tris", "world_px",
            "surf_hits", "surf_misses", "surf_rebakes", "surf_bypass_bakes", "sub_faces_drawn",
            "surf_texels", "surfcache_kb", "alias_models", "alias_accepted", "alias_tris", "band_threads"]
print(f"\nquake-rust browser bench — {len(wasm_bytes) / 1048576:.1f} MB wasm, frames={args.frames} "
      f"warmup={args.warmup}, dt=1/72, load {' '.join(results['load_before'])} -> "
      f"{' '.join(results['load_after'])}")
s = results["startup"]
print(f"startup: download {s['download_ms']:.0f} ms (local; wasm and pak together), started "
      f"{s['start_ms']:.0f} ms after it (compile, instantiate, quake.rc), navigation->first frame "
      f"{s['nav_to_first_frame_ms']:.0f} ms; wasm gzip -6 {s['wasm_gzip6_mb']} MB")
native_by = {(n["workload"], n["w"], n["h"]): n["values"] for n in results["native"]}
for wl in workloads:
    runs = [r for r in results["wasm"] if r["workload"] == wl]
    if not runs:
        continue
    label = lambda r: f"{r['w']}x{r['h']}" + (f"/{r['threads']}t" if r.get("threads") else "")
    head = f"\n{wl:<11}" + "".join(f"| {label(r):<14} med p95 nat x " for r in runs)
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
          f"{lv['res'][0]}x{lv['res'][1]}, real dt): {lv['frames']} frames in {lv['refreshes']} refreshes, "
          f"period median {lv['period_median']:.2f} / p95 {lv['period_p95']:.2f} / p99 "
          f"{lv['period_p99']:.2f} ms, wait median {lv['wait_median']:.2f} / p95 {lv['wait_p95']:.2f}, "
          f"put median {lv['put_median']:.2f}, frames >20 ms apart: {lv['long_frames_over_20ms']}; "
          f"{lv['frames'] / lv['seconds']:.1f} frames/s, {lv['skipped']} refreshes without a new frame "
          f"(the 72 fps cap)")
if results["latency"]:
    la = results["latency"]
    print(f"\ninput -> present ({'uncapped rAF' if not args.vsync else '60 Hz rAF'}, live walk at "
          f"{la['res'][0]}x{la['res'][1]}, wasm_uncapped): {la['events']} key events, median {la['median']:.2f} / p95 {la['p95']:.2f} "
          f"ms (min {la['min']:.2f}, max {la['max']:.2f})")
print("\nrows: ms per frame (median, p95); nat = native median of the same workload via "
      "native_bench; x = wasm/native. step = the program's host frame (bench builds); wait = "
      "tick to the program's answer; js = wait + copy + put; raf - js = the browser's own per-frame work.")
if errs:
    print("page errors:", errs[-5:])
if args.json:
    json.dump(results, open(args.json, "w"))
    print("raw series ->", args.json)
