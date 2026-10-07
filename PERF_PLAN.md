# quake-rust — performance plan

*Status (2026-10-02): a working log. "Where it stands" sums it up; the numbered sections
below are the rounds of work, each measured as it landed (§14 and §15 are the latest). The
current figures are in `README.md`, "Numbers".*

## Where it stands (2026-09-26, the 2026 push)

The day's changes on top of 2026-09-25's (below):

- **Multicore.** The renderer draws the 3-D view on every core, byte-identical at any
  thread count (§11).
- **B5.** Frames are 8-bit, and the page's WebGL2 applies the palette (B5 below).
- **Platform.** The browser build is a WASI program in a worker, and the pak is a file
  read on demand, not embedded (`web/PLATFORM.md`).
- **Frame rate.** The 2026 profile has no 72 fps cap (`FRAMERATE.md`).

In one line each (demo1; details and sittings in §11):

- **Native, Classic, one thread:** `timedemo demo1` runs 2563 / 1113 / 617 fps at
  320×200 / 640×400 / 960×600, against id's portable C at 1895 / 774 / 433 in the same
  sitting (1.35–1.44x).
- **Native, 2026 video:** `timedemo demo1` at 1920×1080 / 2560×1440 / 3840×2160 runs
  208 / 120 / 54 fps on 1 thread, 689 / 455 / 210 on 8, and 682 / 450 / 224 on 16.
- **Browser** (headless Chromium on a desktop GPU, demo1 at 2560×1440, the page's
  frame): 3.42 ms on 8 threads, against 10.62 before B5 in the same sitting. On one
  thread it is 10.17, against 20.50 (`web/PLATFORM.md`, "Presentation"). The closing
  review measured 3.3 ms on the threads build and 10.8 on the single-thread build.
- **Input to present:** 3.16 ms at 2560×1440, uncapped, down from 10.49
  (`bench.py --latency`).

## Where it stood (2026-09-25, `3ba835f`)

**Before and after.** Wasm step median / p95 per frame, native median in parentheses, ms,
headless Chromium, `uv run --with playwright web/bench.py --build --native`. *Before* is
§3's baseline (`5af4fa1` plus the harness, load 2.3–3.3); *after* was run on `3ba835f`
for this summary (10:45, load 2.6 → 2.3). Two sittings at similar load, so read the
ratios, not the last digit. The inputs are the same; the frames are not quite: the game
now draws what id's does (the view above the status bar, the 4:3 pixel aspect, only the
entities the server sends).

| workload | 320×200 | 640×400 | 1280×800 |
|---|---|---|---|
| demo1 | 2.41 / 2.88 (1.65) → **0.58 / 0.72 (0.45)** | 6.40 / 8.00 (3.87) → **1.41 / 1.69 (1.05)** | 22.64 / 33.10 (13.14) → **4.53 / 5.02 (3.23)** |
| attract | 2.66 / 3.48 (1.62) → **0.61 / 0.75 (0.51)** | 6.88 / 9.46 (4.38) → **1.44 / 1.66 (1.18)** | 23.13 / 30.06 (15.21) → **4.82 / 5.47 (3.62)** |
| walk_e1m1 | 2.10 / 3.90 (1.66) → **0.43 / 0.78 (0.27)** | 4.90 / 8.43 (3.37) → **1.09 / 1.54 (0.71)** | 16.13 / 24.95 (9.90) → **3.49 / 4.15 (2.52)** |
| fire_e1m1 | 2.58 / 4.75 (2.09) → **0.38 / 0.63 (0.29)** | 5.88 / 10.56 (4.05) → **1.03 / 1.40 (0.77)** | 19.17 / 34.65 (11.31) → **3.62 / 4.39 (2.67)** |
| walk_e1m3 | 4.57 / 6.00 (3.60) → **0.50 / 0.62 (0.38)** | 7.43 / 8.88 (5.44) → **1.17 / 1.32 (0.87)** | 20.67 / 24.58 (14.23) → **3.86 / 4.25 (2.82)** |

About 5x faster at 1280×800, and the spikes are gone: fire_e1m1's p95 was 1.8x its
median (dynamic lights), now 1.2x; demo1's damage-flash frames no longer cost 11 ms
extra. A wasm frame costs 1.2–1.6x the native one (it was 1.2–1.7x).

Where demo1's 4.53 ms at 1280×800 goes now (was 22.64): the 3-D view 3.68 (was 19.42),
of which the world 2.91 (15.01), alias models 0.45 (1.34), the gun 0.20 (1.37); the
status bar 0.09 (0.76); the RGBA pack 0.45 (2.36); the palette shift 0 on every frame
(11.9 on a shifted frame). The page adds 0.42 for the copy and `putImageData`.

Elsewhere, from the branch reports below:

- **Game logic:** `quaketool simbench` e1m3 2.41–2.64 → 0.29 ms per tick (D2). In the
  browser, walk_e1m3's sim phase at 320×200: 0.74 → 0.17 ms (this run).
- **Memory:** 81 → 53 MB after boot, 171 → 62 MB after four map loads (D3, measured on
  `quake/host`).
- **Startup, local:** navigation to first frame 204 → 173 ms (this run); served
  compressed over an emulated 50 Mbit/s link, 3.5 → 1.8 s (D4).
- **High-refresh displays:** the 72 fps cap (D1) halves the work at 144 Hz, and a 120 Hz
  display runs at 60 fps. Argued from the code and unit-tested, not measured on a real
  display.
- **Against id's own renderer** (native, warm, world only, `oracle/compare.py --bench`):
  the port takes 0.30–0.34x id's time at 320×200, 0.37–0.45x at 640×480 and 0.43–0.51x
  at 1280×1024 (A3).
- **Against id's whole frame, id's way:** `timedemo demo1` (§10, `quake/timedemo`), the
  same 969 frames in both. At the page's pixel aspect, one sitting: id's portable C 1822 /
  745 / 419 fps at 320×200 / 640×400 / 960×600; the port natively 2602 / 1007 / 516
  (1.23–1.43x), in the browser 1916 / 723 / 381 (0.91–1.05x). Before the edge renderer
  the port was at 0.56–0.86x natively.

**The items**, in the order of §5:

| item | what | status |
|---|---|---|
| A1 | polygon spans instead of bounding-box triangles | done (`quake/w1`), then replaced by A3 |
| A2 | dynamically lit walls through the surface cache | done (`quake/w1`) |
| A0 | sort only the faces that survive the culls | done (`quake/w1`); moot since A3 |
| A3 | id's edge-sorted span renderer | done (`quake/edge`): wasm frame −23 to −33%, native −43 to −57%; the polygon walker deleted |
| A4 | the view above the status bar; the pixel aspect | done (`quake/options`, `quake/w2b`) |
| A5 | mip levels in the surface cache | done (`quake/w2a`), with `R_DrawSurfaceBlock8`'s lightmap stepping |
| B1 | RGBA pack in place | done (`quake/perf-b`) |
| B2 | palette shift as `V_UpdatePalette`'s ramps | done (`quake/perf-b`), id's truncation |
| B3 | keep the frame buffers | done (`quake/perf-b`) |
| B4 | HUD, menu and console blits by rows | done (`quake/perf-b`) |
| B5 | the 8-bit framebuffer | done (`q26/present`): frames are palette indices, the palette applied at presentation, WebGL2 the DAC (B5 below) |
| B6 | present from wasm memory | done (`quake/host`); `?lowlatency` opt-in |
| C1 | cull entities as the server does | done (`quake/sim`); also fixed CENSUS L22 |
| C2 | alias models through `D_PolysetDraw` | done (`quake/fid1`, as a fidelity fix) |
| C3 | the gun on the shared z-buffer, placed by `V_CalcRefdef` | done (`quake/options`, `quake/fid1`) |
| C4 | cache the external boxes' surfaces | **not done**; after C1 and A5 walk_e1m3 bakes 2 box faces a frame (the `surf_bypa` counter), too little to matter |
| D1 | `Host_FilterTime`'s 72 fps cap | done (`quake/host`); since 2026-09-26 Classic's only: the 2026 profile runs uncapped (`wasm_uncapped`, `FRAMERATE.md`) |
| D2 | resolve entity fields once | done (`quake/sim`) |
| D3 | the pak as one static slice | done (`quake/host`) |
| D4 | compressed, streamed delivery | compression and streaming done (`quake/host`); the pak split out of the wasm done (`q26/platform`: `quake.wasm` is 1.3 MB, the pak a file read on demand, cached by the service worker); **open:** `wasm-opt` |

**Still open or unmeasured:** D4's `wasm-opt`; C4 (not worth it now); a real browser on a
real display (every measurement is headless Chromium, on a desktop GPU at best), Safari,
phones and a real 120–480 Hz display (§9); the page's sound cost (the bench runs with
audio locked).

The rest of this file is the plan as written on branch `quake/perf`, with each item's
outcome added under it by the branch that did it. Its numbers are the baseline's unless an
item says otherwise. §10, at the end, is `timedemo` against id's C.

---

## The plan (branch `quake/perf`), with outcomes

2026-09-25, branch `quake/perf`. Measurement and a plan. The only code in this round is the
benchmark harness and its opt-in timers. Implementers: the code is about to be split into modules,
so everything below names **functions and mechanisms**, never line numbers.

**The rule still holds:** faithful to WinQuake by default. Where the port does **more work than
Quake did**, doing what Quake did is both faster and more faithful, so those items come first. A
change that is faster but not faithful can only ship as an opt-in extra.

---

## 1. Summary

*At the baseline, `5af4fa1`. The current numbers are at the top of this file.*

At 1280×800 in the browser, a frame of id's demo1 costs **~19–23 ms** in wasm. The time goes to:

- **the world rasteriser: 66%**
- the RGB→RGBA pack: 10%
- alias models plus the gun: 12%
- the status bar: 3%

When a palette shift is active (damage flash, underwater tint, powerup), a **per-pixel blend adds
another 12 ms**. When a dynamic light touches the view (muzzle flash, explosion, rocket), lit walls
leave the surface cache for a slow per-pixel path. Wasm runs **1.5–1.9× slower than native** on the
same code and the same workload.

The world rasteriser does far more work than Quake's. It fan-triangulates every face and walks
each triangle's **bounding box**, so it visits **3.8–5.2× the screen's pixels** per frame. About
1.3× pass the inside test and 1.0× get drawn, and every covered pixel pays a divide and a z-buffer
test. Quake's edge-sorted span renderer touches each pixel once and does no z test at all.

**Top 5**

| # | item | measured evidence | expected gain | fidelity |
|---|------|-------------------|---------------|----------|
| 1 | **A1** Scan-convert each face polygon row by row (spans), not bounding boxes of fan triangles | prototype, wasm demo1 1280×800: world **13.8 → 4.6 ms**, whole frame **19.3 → 10.2 ms**; walk p95 25 → 12.6 ms | about −45% of the frame at 1280×800 | neutral: 0.02% of pixels differ, and no cracks |
| 2 | **B2** Palette shift as `V_UpdatePalette`'s 256-entry ramps (per-channel lookup table), not float math on every pixel | prototype: blend **12.0 → 0.79 ms** at 1280×800, output identical | removes 11–12 ms spikes on every damage, underwater or powerup frame | faithful (this is the C's own mechanism) |
| 3 | **B1** RGB→RGBA pack as a chunked copy, not four `Vec::push` per pixel | prototype: pack **2.34 → 0.73 ms** at 1280×800, framebuffer hashes identical | −1.6 ms per frame at 1280×800, −0.4 ms at 640×400 | byte-identical |
| 4 | **A2** Dynamically lit walls rebuilt through the surface cache (`D_CacheSurface` + `R_AddDynamicLights`), not the per-pixel path | fire_e1m1: firing costs **+19% median / +39% p95**; native, a light injected at the eye: **16.5 → 41.7 ms** | removes the firing and explosion spikes (~−15–25 ms native at 1280×800) | faithful (lighting per texel, as Quake) |
| 5 | **D1** The `Host_FilterTime` cap: at most 72 frames per second | by construction: the page steps and renders once per rAF, at the display's rate | half the work on 120/144 Hz displays | faithful; the port currently departs from Quake by running uncapped |

The next largest item is **A4**: render the 3-D view only above the status bar, as `R_SetVrect`
does at viewsize 100. That is 24% fewer 3-D pixels, and it fixes two framing bugs (§4).

**Committed quick wins: none, deliberately.** I measured the build-configuration candidates:

- `+simd128`: no measurable change. (Since 2026-10-03 the browser builds use it: with today's
  span loops the vectorized z-buffer row pays, 2–9% of a frame: §15.)
- `wasm-opt -O3`: −2 to −5% and identical output, but it needs binaryen in the build (§6).
- `opt-level`, `lto`, `codegen-units` and `panic=abort` are already optimal in `quake-wasm/Cargo.toml`.

The wins that do exist are code changes, so per the brief they stay in this plan. Three of them
(B1, B2, and caching the external boxes in C4) are byte-identical and mechanical.

---

## 2. The harness: how to measure

**Browser:**
```sh
uv run --with playwright web/bench.py --build --native      # builds the `--features bench` wasm; runs everything below
uv run --with playwright web/bench.py --build --workloads demo1 --res 1280x800 --profile  # plus a CDP CPU profile
uv run --with playwright web/bench.py DIR --hash-every 60   # any index.html + wasm; framebuffer hashes for A/B identity
uv run --with playwright web/bench.py --build --live 15 --vsync   # sample the page's OWN loop (pacing at 60 Hz)
```

(`--with playwright` is needed: the script's shebang carries it, but `uv run script.py`
does not read the shebang, and a plain `uv run web/bench.py` stops at the playwright
import.)

- **What it runs.** `web/bench.py` boots the real page in headless Chromium, pauses its rAF loop,
  and drives these workloads at dt = 1/72:
  - `demo1`: id's e1m3 demo, menu closed.
  - `attract`: demo1 under the main menu.
  - `walk_e1m1` and `walk_e1m3`: a live server with a scripted look-around and run.
  - `fire_e1m1`: the e1m1 walk with +attack held.
- **Resolutions and settings.** 320×200, 640×400 and 1280×800 (the clamp maximum). The server
  sends COOP/COEP headers, which raises `performance.now()` resolution to 5 µs.
- **What it reports.** Median and p95 per frame for:
  - JS: `step`, `copy`, `put`, `raf`.
  - Wasm frame phases: sim, render3d, post3d, hud2d, menu, console, blend, pack.
  - The engine's `RenderStats` split of render3d: world (itself split into pvs, sort,
    setup+raster, light and surf), submodel, external, alias, particle, sprite and viewmodel.
  - Counters: faces culled by PVS and by frustum, faces drawn, pixels, surface-cache misses,
    rebakes and bypass bakes.
- **The timers.** They live in `quake-wasm/src/bench.rs` behind the `bench` cargo feature. That
  build imports `quake_bench.now_ms`, which only the harness supplies. Without the feature, every
  hook is an empty inline function and the deployed build is unchanged.
- **The native twin.** `--native` runs `native_bench`, which drives the same workloads through the
  same exported functions on the host. Every row therefore gets a wasm/native ratio.
- **The engine side.** `render::set_render_stats_clock` makes the existing `RenderStats` timers
  work in wasm.
- **Checked:**
  - Framebuffer hashes are identical across HEAD, the new default build and the bench build.
  - The bench build's step time is within noise of the production build (≤ 3%).
- **Native tools, unchanged:**
  - `QUAKE_BENCH=N QUAKE_RES=WxH quaketool scene …`: a fixed camera. It clones the external boxes,
    so its `external` row is pessimistic.
  - `quaketool simbench`: sim only, at dt = 0.1.
- **Noise.** Timings are noisy, with other agents running: load was 1.8–3.3 during these runs. Always
  A/B in the same sitting, and trust ratios over absolute ms. Attract-workload hashes include the
  menu cursor's animation phase, which depends on the App clock. Compare identity on the other
  workloads.

---

## 3. Baseline (HEAD `5af4fa1` + harness)

Wasm step median / p95, with the native median of the same workload in parentheses. Times are in
ms. Headless Chromium 153, x86-64, load 2.3–3.3.

| workload | 320×200 | 640×400 | 1280×800 |
|---|---|---|---|
| demo1 (e1m3 demo) | 2.41 / 2.88 (1.65) | 6.40 / 8.00 (3.87) | 22.64 / 33.10 (13.14) |
| attract (demo + menu) | 2.66 / 3.48 (1.62) | 6.88 / 9.46 (4.38) | 23.13 / 30.06 (15.21) |
| walk_e1m1 | 2.10 / 3.90 (1.66) | 4.90 / 8.43 (3.37) | 16.13 / 24.95 (9.90) |
| fire_e1m1 | 2.58 / 4.75 (2.09) | 5.88 / 10.56 (4.05) | 19.17 / 34.65 (11.31) |
| walk_e1m3 | 4.57 / 6.00 (3.60) | 7.43 / 8.88 (5.44) | 20.67 / 24.58 (14.23) |

The full per-phase tables for demo1 and walk_e1m3 follow. They show wasm median / p95 · native
median · the wasm/native ratio, in ms. `bench.py --native` prints all five.

**demo1**

| phase | 320×200 | 640×400 | 1280×800 |
|---|---|---|---|
| step (whole wasm frame) | 2.41 / 2.88 · 1.65 · 1.5× | 6.40 / 8.00 · 3.87 · 1.7× | 22.64 / 33.10 · 13.14 · 1.7× |
| sim (demo parse) | 0.00 | 0.01 | 0.02 |
| render3d | 2.12 / 2.49 · 1.42 · 1.5× | 5.57 / 6.76 · 3.22 · 1.7× | 19.42 / 25.18 · 10.67 · 1.8× |
| · world | 1.34 / 1.68 · 0.91 · 1.5× | 3.98 / 5.22 · 2.28 · 1.8× | 15.01 / 20.68 · 7.93 · 1.9× |
| · submodel | 0.03 / 0.12 | 0.07 / 0.36 | 0.24 / 1.49 · 0.13 · 1.8× |
| · alias | 0.57 / 0.75 · 0.37 · 1.5× | 0.78 / 1.29 · 0.45 · 1.7× | 1.34 / 3.13 · 0.75 · 1.8× |
| · viewmodel | 0.11 / 0.17 · 0.08 · 1.4× | 0.36 / 0.62 · 0.20 · 1.8× | 1.37 / 2.54 · 0.79 · 1.7× |
| hud2d (sbar) | 0.06 · 1.1× | 0.20 · 1.1× | 0.76 / 0.81 · 0.70 · 1.1× |
| pack (RGB→RGBA) | 0.15 · 1.4× | 0.58 · 1.4× | 2.36 / 2.53 · 1.71 · 1.4× |
| JS copy + putImageData | 0.01 | 0.06 | 0.42 / 0.56 |
| rAF period | 2.54 | 6.73 | 23.53 / 35.12 |

Per frame, the counters are: 142 faces drawn, 3,882 culled by PVS, 314 by frustum, and 3 faces on
the per-pixel path. The p95 of 33 ms at 1280×800 comes from the recorded damage flash, where blend
is about 11.1 ms (§4, B2). Attract adds the menu: 0.08 / 0.27 / 1.03 ms.

**walk_e1m3** (live server, the densest sim)

| phase | 320×200 | 640×400 | 1280×800 |
|---|---|---|---|
| step | 4.57 / 6.00 · 3.60 · 1.3× | 7.43 / 8.88 · 5.44 · 1.4× | 20.67 / 24.58 · 14.23 · 1.5× |
| sim | 0.74 / 1.34 · 0.45 · 1.6× | 0.77 / 1.10 · 0.49 · 1.5× | 0.84 / 1.29 · 0.62 · 1.3× |
| · world | 0.93 / 1.48 · 0.57 · 1.6× | 2.81 / 4.08 · 1.62 · 1.7× | 11.37 / 15.72 · 6.53 · 1.7× |
| · submodel | 0.12 | 0.12 | 0.26 / 2.58 |
| · external (b_*.bsp) | **0.85** / 1.30 · 1.20 | **0.79** | **0.87** / 1.50 |
| · alias | **1.22** / 1.84 · 0.76 · 1.6× | 1.29 · 0.80 | 1.53 / 2.69 · 0.96 · 1.6× |
| · viewmodel | 0.13 | 0.41 | 1.57 · 0.96 · 1.6× |
| hud2d | 0.06 | 0.19 | 0.76 |
| pack | 0.14 | 0.58 | 2.38 · 1.75 · 1.4× |

At 320×200 the fixed costs dominate: external 0.85 ms, alias 1.22 ms and sim 0.74 ms make up 62%
of this frame. The external boxes cost 84 bypass bakes per frame.

**Native `quaketool` on the same machine (load 1.8)**

- **`scene`**, fixed camera, ms per frame:

  | map | 320×200 | 640×400 | 1280×800 | 1280×800 world |
  |---|---|---|---|---|
  | e1m1 | 2.86 | 6.51 | 16.73 | 13.67 |
  | e1m2 | 3.38 | 5.12 | 11.43 | 7.17 |
  | e1m3 | 4.97 | 8.23 | 19.13 | 14.74 |

- **`simbench`** (dt 0.1): e1m1 0.83 ms per tick, e1m2 0.73, e1m3 2.41 (522 traces per tick).
- **Injected dynamic light** (`QUAKE_DLIGHT=eye`) on e1m1 at 1280×800: 16.55 → 37.87 ms at radius
  200, and 41.70 ms at radius 350. The world pass alone goes from 13.0 to 33.6 ms.

**Where the time goes, from a CPU profile of the production wasm** (1280×800,
`bench.py --profile`, % of self time):

| function | demo1 | walk_e1m3 |
|---|---|---|
| `raster_triangle_cached` | 64.5% | 53.4% |
| `raster_triangle_tex` | 9.9% | 10.0% |
| `step`, self (the pack, `apply_blend` and other shell code inlined into it) | 10.6% | 11.4% |
| `blit_qpic` | 3.5% | 3.5% |
| `face_surf_block` bake closure (external boxes) | ~0.2% | 2.9% |
| `Progs::field_offset` + SipHash `hash_one` | ~0% | 3% inclusive |
| `putImageData` | ~1% | ~1% |

**Spikes measured with a forced effect** (a scratch build, 1280×800, wasm):

- `apply_blend`: 11.9 ms. It is 3.3 ms at 640×400 and 7.9 ms native.
- `apply_warp`, the underwater `D_WarpScreen`: 1.7 ms.

**Rasteriser iteration counts** (native counters, 640×400):

| workload | pixels visited | pass the inside test | drawn | per-pixel path, bbox visits |
|---|---|---|---|---|
| demo1 | 5.21× screen | 1.38× | 1.02× | 0.46× |
| walk_e1m1 | 3.77× | 1.26× | 1.00× | 0.28× |
| walk_e1m3 | 3.79× | 1.21× | 1.00× | 0.42× |

**Alias models handed to the renderer per frame** (native counters):

| workload | models | triangles | models with a front-facing, on-screen vertex |
|---|---|---|---|
| walk_e1m3 | 98 | 20,307 | 24.7 |
| walk_e1m1 | 29 | 8,928 | 9.3 |
| demo1 | 54.5 | 5,827 | 24.1 |

Both counts come from scratch-instrumented builds, not from the committed harness.

**Page loop, startup and memory**

- **The page's own loop** (the attract loop at the default 960×600, headless, 60 Hz vsync): step
  median 14.2 ms, p95 19.8 ms. **55 of 886 frames (6%) took longer than 20 ms**, and each is a
  missed vsync. Uncapped, the period median is 13.6 ms and p99 21.7 ms.
- **Startup**, served locally:

  | step | time |
  |---|---|
  | wasm fetch | 75 ms |
  | `WebAssembly.instantiate` | 7 ms |
  | `boot_attract` | 32 ms |
  | navigation to first frame | 204 ms |

  The wasm is 18.65 MB, and 8.41 MB with gzip -6. The pak is 18.7 MB of that, and the code
  section is 0.77 MB.
- **Memory:** linear memory is **81 MB** after boot and **175 MB** after four map loads. Wasm
  memory never shrinks. §7 explains why.

---

## 4. Port versus WinQuake: where the port does more work

| mechanism | WinQuake (C) | port | consequence (measured) |
|---|---|---|---|
| world visibility and overdraw | `r_edge.c` (`R_ScanEdges` / `R_GenerateSpans`): edge-sorted spans, each pixel drawn once, **no z test** | fan triangles; bounding-box barycentric raster (`raster_triangle_cached` / `_tex`); f32 z test and write on every pixel; centroid sort of all world faces every frame | 3.8–5.2× screen visits; ~25% of inside pixels are z-rejected (A1 done: spans; A3 done: as the C) |
| perspective | `D_DrawSpans16` (asm; `d_subdiv16` defaults to 1): one divide per 16 px, affine between | one divide per inside pixel | not the wasm bottleneck. The 16-px prototype gave no wasm gain (native −15%) |
| z-buffer | 16-bit 1/z (`d_pzbuffer`), written by `D_DrawZSpans`, only **tested** by entities, **never cleared** | f32 depth cleared every frame; the RGB image is also cleared every frame | 0.39 ms per frame at 1280×800 for allocation and clear (A3 done: as the C) |
| framebuffer and palette | 8-bit indices. Cshift and gamma are a **256-entry** palette operation (`V_UpdatePalette` ramps, then `VID_ShiftPalette`) | `[u8;3]` RGB per pixel; `apply_blend` does float math per pixel per channel; pack uses 4 `Vec::push` per pixel | blend 11.9 ms, pack 2.3 ms at 1280×800 |
| surface cache | `D_CacheSurface` at `D_MipLevelForScale`'s mip (4 levels; `basemip` 1, 0.4, 0.2). A fixed, LRU-rover cache of `SURFCACHE_SIZE_AT_320X200` (600 KB) + 3 B/px above 64,000 px. Lit surfaces rebuilt with `R_AddDynamicLights` | mip 0 only; one unbounded block per face; lit faces bypass the cache into `raster_triangle_tex`: bilinear lightmap + `colormap_row` on every **screen pixel** | the firing and explosion spikes above; lighting per pixel, not per texel |
| 3-D viewport | `R_SetVrect`: at viewsize 100, the vrect height is vid.height − `sb_lines` (48 of 200 lines). `R_ViewChanged`: `yscale = xscale · pixelAspect` (0.833 at 16:10) | full-height render with the sbar painted over the bottom 24%. Square-pixel projection, then **presented** at 4:3 (AUDIT H6) | 24% of 3-D pixels are thrown away. The horizon sits too low, and the world is stretched 1.2× vertically (see A4) |
| which entities are drawn | `SV_WriteEntitiesToClient` sends only entities touching `SV_FatPVS`; statics use efrags on visible leaves (`R_StoreEfrags`); `R_AliasCheckBBox` rejects by frustum | every edict with a model index, every frame, one triangle at a time (C1 done: as the C) | ~75% of alias triangles per frame belong to models with no on-screen vertex |
| alias raster | `R_AliasPreparePoints` (each vertex transformed once). `D_PolysetDraw`: affine, Gouraud light through `acolormap`, `lzi >= *lpz` test. Viewmodel shares the z-buffer with `ziscale × 3` (`R_AliasDrawModel`) | 3 transforms per triangle; flat Lambert as an RGB multiply (**not colormapped**); perspective-correct bbox raster. Viewmodel allocates and clears a **full-resolution local z-buffer every frame** | viewmodel 1.4–1.9 ms at 1280×800 |
| frame rate | `Host_FilterTime`: return early when less than 1/72 s has passed | `step(dt)` once per rAF at the display rate | 2× work at 144 Hz |
| external brush boxes (`b_*.bsp`) | surfaces go through the surface cache like any brush surface | `cache_surf = false`: re-baked every frame | 0.27–0.87 ms per frame, at any resolution |
| underwater warp | `D_WarpScreen`, 8-bit, into the view buffer | clones the whole RGB frame, plus 3 `Vec` allocations per call | 1.7 ms at 1280×800 |
| entity field access | direct `entvars_t` struct members | `ent_get_*(ent, "name")`: a `HashMap<String>` SipHash lookup per field per entity (D2 done: resolved once per progs) | about 3% of the frame on e1m3; most of sim |

The port column is the baseline's. Every row has since been done the C's way (the item
table at the top) except two: the surface cache keeps a block per face and
mip level with no fixed-size pool like id's (its resident size is measured under A5);
and the external boxes still bypass it (C4, now 2 faces a frame). The framebuffer is
8-bit since B5, the palette shift and gamma applied to the palette. The underwater view renders into id's 320×200
warp buffer since `quake/polish`, and the warp keeps its tables since `quake/polish3`.

---

## 5. The plan, ranked

Every item gives its evidence, expected gain, fidelity class, risk and the functions it touches.
The fidelity classes are:

- **byte-identical**: the goldens and wasm hashes must not move.
- **faithful**: moves toward the C. A golden re-baseline is expected and gets recorded in `AUDIT.md`.
- **neutral**: sub-pixel differences, re-baselined and documented the way `ae3ba68` was.
- **departure**: opt-in extras only.

### A. World pass and surface cache

**A1. Scan-convert faces as polygons, not bounding-box triangles.** *(neutral, the top gain)*

- **Evidence:**
  - The prototype `raster_poly_cached`, mode 1, was built in scratch and is not committed.
  - Wasm: demo1 world 13.8 → 4.6 ms at 1280×800 and 3.9 → 1.5 ms at 640×400. walk_e1m1 world
    8.3 → 4.0 ms. Whole frame 19.3 → 10.2 ms. walk p95 25 → 12.6 ms.
  - Native: 7.6 → 4.4 ms.
  - Output: 0.024% of pixels differ on demo1 and 0.0008% on the walk, with no background-colour
    cracks.
  - Adding 16-px affine subdivision (mode 2) gave **no further wasm gain**. The wasm loop was
    bound by the bounding-box walk, not by the divide.
- **Mechanism.** For the clipped convex polygon that `draw_world_textured` already builds (`proj`):
  - Take `1/z`, `s/z` and `t/z` as screen-plane gradients, as `D_CalcGradients` does. Compute them
    from the largest-area vertex triple, not `proj[0..3]`.
  - Walk rows at pixel centres and intersect the polygon's edges to get [xl, xr).
  - Step the three accumulators across the span with one add per pixel.
  - Keep the existing z test and writes, the texel clamp, and the `world_pixels` counter.
- **Functions:**
  - Replace `raster_triangle_cached` at its call sites in `draw_world_textured` and `draw_submodel`.
  - Apply the same span walker to `raster_triangle_tex`'s world uses (turb, sky, no-cache paths),
    which are 10% of the profile. The per-pixel body stays as it is.
  - The alias uses of `raster_triangle_tex` belong to C2.
- **Risk: medium.**
  - Fill convention on shared edges: cracks or double-draws between adjacent faces. Pick one rule
    (pixel centre inside, half-open on the right and bottom) and test on the goldens with a
    background-pixel count.
  - Near-degenerate polygons: guard on the gradient determinant.
  - The goldens will move by a handful of pixels. Record the count in `AUDIT.md`.
- **This is a stepping stone to A3.** A1 is contained and measured; A3 is the faithful endpoint.
- **Done** (branch `quake/w1`): `scan_poly` walks each clipped polygon row by row under id's fill
  rule (pixel centre, top-left, half-open). The span loops in `raster_poly_cached`,
  `raster_poly_tex` and `raster_poly_flat` cover the cached, turb, sky, per-pixel and flat faces.
  A `Span` is the place for 16-pixel subdivision (done on `quake/w2b`, §6). The gradients are not solved from a vertex
  triple; they are `D_CalcGradients`' analytic planes (`PolyGrads::for_plane`), with s and t
  relative to the eye and f64 steps: the absolute-s f32 interpolation misplaced ~0.3% of texels.
  - **Speed,** A/B against A0 in one sitting, median ms (load 3.5–5):

    | workload | wasm 640×400 world / step | wasm 1280×800 world / step | native 1280×800 world / step |
    |---|---|---|---|
    | demo1 | 3.58 → 1.37 / 5.70 → 3.25 | 12.34 → 4.27 / 19.35 → 10.67 | 6.33 → 3.16 / 11.80 → 8.27 |
    | walk_e1m1 | 1.99 → 0.98 / 4.03 → 3.08 | 7.86 → 3.57 / 14.21 → 9.81 | 4.21 → 2.68 / 9.27 → 7.93 |
    | fire_e1m1 | 2.35 → 1.14 / 4.61 → 3.66 | 9.69 → 3.88 / 16.81 → 10.62 | 4.97 → 2.87 / 10.52 → 8.53 |
    | walk_e1m3 | 2.25 → 1.02 / 6.23 → 4.78 | 8.25 → 3.53 / 16.88 → 11.58 | 4.53 → 2.85 / 11.76 → 10.17 |

    wasm demo1 at 1280×800: step p95 24.7 → 13.4 ms. The submodel pass moves by at most
    ±0.06 ms at the median, and its p95 drops.
  - **Pixels:** the goldens move by 151, 709 and 729 pixels, all at texel boundaries; the old
    renderer was the inexact one. Background pixels over 288 oracle views go from 35 to 0.
    `AUDIT.md` has the hashes and the evidence.
  - **Oracle,** the 16 standard rows (`compare.py --modes world`, 4 maps; id as shipped, and id
    at mip 0 + exact perspective):

    | config | e1m1 | e1m2 | e1m3 | e1m7 |
    |---|---|---|---|---|
    | 320×200 as shipped | 84.76 → 84.74 | 64.07 → 64.06 | 65.83 → 65.88 | 75.65 → 75.62 |
    | 320×200 mip 0 + exact | 95.63 → 95.61 | 97.41 → 97.40 | 98.39 → 98.50 | 98.68 → 98.66 |
    | 640×480 as shipped | 90.79 → 90.84 | 78.19 → 78.28 | 90.74 → 90.73 | 94.55 → 94.58 |
    | 640×480 mip 0 + exact | 95.89 → 95.93 | 97.48 → 97.58 | 98.27 → 98.51 | 98.75 → 98.80 |

    Seven rows fall by 0.01–0.03 points: single boundary pixels where the old rounding agreed
    with id's. Over 144 views per configuration the mean rises: 79.873 → 79.907, 97.682 →
    97.760, 90.636 → 90.774 and 97.611 → 97.801. At 640×480 with mip 0 + exact, all 144
    views improve.

**A2. Dynamically lit surfaces through the surface cache.** *(faithful)*

- **Evidence:**
  - fire_e1m1 against walk_e1m1 at 1280×800: step median 16.1 → 19.2 ms, p95 25 → 34.7 ms.
  - Native `QUAKE_DLIGHT=eye`: 16.5 → 41.7 ms.
  - In `face_surf_block`, `dlit == true` returns `None`, so the face goes through
    `raster_triangle_tex` with `LightMap::factor_at` + `colormap_row` on every screen pixel.
- **Mechanism.** Follow `D_CacheSurface`:
  - When `any_dlight_reaches(…)` holds, build the lightmap with dlights
    (`face_lightmap_world_cached` already does `R_AddDynamicLights`).
  - Bake the block exactly like the static path, and draw it with the cached span raster.
  - Mark the cache entry `dlight`, as the C's `cache->dlight` does, so the next unlit frame
    rebuilds it.
- **Cost of the bake:** texels at 1/16 lightmap resolution, not screen pixels. At 1280×800 a lit
  face is usually far smaller in texels than in pixels.
- **Functions:** `face_surf_block`, `draw_world_textured` (the `surf` match), `draw_submodel`.
- **Risk: low.** The goldens have no dlights, so they must stay byte-identical. The wasm hashes
  change only in lit frames. The old per-pixel path remains for turb, sky and textures without a
  colormap.
- **Fidelity.** This also removes the port-specific tightening "flip baked → per-pixel", noted in
  STATUS, which the C never had.
- **Done** (branch `quake/w1`): `face_surf_block` bakes a dlit wall with the light and marks the
  entry `dlight`, and its hit test is `D_CacheSurface`'s. That test also takes the texture, which
  fixes animated wall textures frozen by the cache. `any_dlight_reaches` now only saves rebakes of
  faces the light marks but never reaches.
  - **Speed,** A/B against A1 in one sitting, fire_e1m1, median / p95 ms:

    | build | step, wasm 640×400 | step, wasm 1280×800 | step, native 1280×800 | render3d p95, wasm 1280×800 |
    |---|---|---|---|---|
    | A1 | 3.66 / 6.50 | 11.15 / 20.64 | 9.28 / 22.15 | 14.87 |
    | A2 | 3.25 / 5.26 | 9.89 / 12.86 | 8.83 / 15.31 | 7.37 |

    Native `QUAKE_DLIGHT=eye` on e1m1 at 1280×800, three runs: radius 350 **30.6–31.8 → 14.0–14.5
    ms** (world 24.0–24.6 → 9.6–10.0); radius 200 28.2–29.7 → 11.7–12.2 ms; with no light, 7.7–8.3
    ms for both. The other workloads move within noise.
  - **Identity:** the goldens and the oracle's standard rows are identical to A1's. Wasm hashes
    differ only on fire_e1m1's lit frames.
  - **Remaining cost of a lit frame:** the bake runs at mip 0 with a bilinear `factor_at` per
    texel. Mip levels (A5) and `R_DrawSurfaceBlock8`'s luxel-block stepping would cut it.

**A0. Sort only the faces that survive the PVS and frustum culls.** *(byte-identical)*

- **Evidence:** `world_sort` takes 0.12–0.14 ms per frame at every resolution, sorting all 5,059
  world faces through `face_geom_cached`. Only about 700–860 survive the PVS and frustum culls, and
  about 30–140 are drawn.
- **Mechanism:** filter first, then sort. `sort_by` is stable, so the order of the survivors is
  unchanged.
- **Functions:** `draw_world_textured`. Optionally, cache `compute_visible_faces` by view leaf; it
  allocates three `Vec<bool>` per frame.
- **Done** (branch `quake/w1`): cull first, then sort the survivors. `world_sort` (now the PVS and
  frustum filter plus the sort) **0.11–0.14 → 0.016–0.045 ms** per frame, native and wasm, at
  640×400 and 1280×800; the whole frame moves within noise. Goldens and wasm frame hashes are
  identical (demo1, walk_e1m1, fire_e1m1 and walk_e1m3 at 640×400, every 20th frame). Not done:
  the view-leaf cache of `compute_visible_faces`. `world_pvs` measures 0.011–0.020 ms, too little
  to be worth a cache.

**A3. Quake's edge-sorted span renderer.** *(faithful, the endpoint; a large job)*

- **What to port:**
  - `r_edge.c`: `R_EmitEdge`, `R_ScanEdges`, `R_GenerateSpans`.
  - `d_edge.c`: `D_DrawSurfaces` with `D_CalcGradients`, and `D_DrawSpans16`, which gives Quake's
    16-px affine look.
  - `D_DrawZSpans`, writing a 1/z buffer. Alias models, particles and sprites then test `izi`
    against it; they currently test depth.
- **What it removes:** all world overdraw (the 1.2–1.4× inside ratio), the world z test, and both
  per-frame clears (image and z).
- **Brush submodels** go through the edge list (`R_DrawSolidClippedSubmodelPolygons`), so doors
  sort with the world.
- **Expected gain:** another 20–35% of the world pass after A1. My estimate: overdraw 1.25–1.38×
  becomes 1.0×, plus the clears and the z tests go away.
- **Risk: high.** It is a rewrite of the core pass, and it re-baselines the goldens.
- **Do it after A1, A2 and A4 have landed.** Their gains do not depend on it.
- **Done** (branch `quake/edge`, `render/edge.rs`; `AUDIT.md`, "World pass: id's edge
  renderer"): `R_RecursiveWorldNode` with keys and `R_MarkLeaves`, `R_RenderFace` /
  `R_ClipEdge` / `R_EmitEdge` with the edge cache, the brush entities through
  `R_DrawSubmodelPolygons` / `R_DrawSolidClippedSubmodelPolygons`, `R_ScanEdges` with
  `R_LeadingEdge`'s key and 1/z sort, `D_DrawSurfaces` with `D_DrawZSpans`, and the
  entities against the 16-bit z-buffer. The polygon walker was kept behind a switch for the
  A/B below, then deleted (byte-identical).
  - **Speed,** A/B in one sitting (the tree before A3, exported, against this branch),
    two rounds, `bench.py --build --native`, median of the per-round medians, ms, load
    1.7–2.1. "brush" is world + submodel + external: the edge renderer books the brush
    entities' edge setup under submodel and draws their spans with the world's.

    | workload | wasm 640×400 step med / p95 | wasm 1280×800 step med / p95 | wasm 1280×800 brush med | native 1280×800 step med |
    |---|---|---|---|---|
    | demo1 | 1.88 / 2.36 → 1.29 / 1.63 | 5.66 / 6.79 → 4.11 / 4.63 | 3.58 → 2.44 | 6.02 → 2.77 |
    | walk_e1m1 | 1.37 / 2.07 → 0.92 / 1.34 | 4.37 / 5.84 → 3.21 / 3.98 | 2.78 → 2.01 | 4.97 → 2.13 |
    | fire_e1m1 | 1.23 / 1.86 → 0.93 / 1.34 | 4.31 / 5.64 → 3.28 / 4.05 | 2.74 → 2.02 | 4.81 → 2.36 |
    | walk_e1m3 | 1.44 / 1.74 → 1.09 / 1.31 | 4.51 / 5.11 → 3.47 / 4.02 | 2.79 → 2.13 | 4.82 → 2.33 |

    Wasm whole frame −23 to −33% at the median and −21 to −36% at p95; the brush pass
    −22 to −39% (−26 to −39% at 640×400); native −43 to −57% per frame and −56 to −66%
    for the brush pass. The estimate was −20–35% of the world pass. The native gain is
    larger because the polygon walker's per-pixel z test (one divide per pixel, w2b's
    native regression, §6) is gone; in wasm the frame's other phases stay. Warm frame
    against id's own renderer (`compare.py --bench 100`, world only): 0.30–0.34× id's
    time at 320×200, 0.37–0.45× at 640×480, 0.43–0.51× at 1280×1024 (the polygon walker
    in the same sitting: 0.48–0.81×, 0.75–1.32×, 1.06–1.94×).
  - **Where the wasm world pass goes now** (native 1280×800, e1m1 `scene`): the world
    walk to edges 0.25 ms, the scan 0.63 ms, `D_DrawSurfaces` 1.46 ms.
  - **Fidelity:** 372 oracle cases 99.798 → 99.833% mean, none worse by more than one
    pixel; e1m2 99.21 → 99.94 (the face 733 quirk); entity pixels 100% everywhere; 144
    brush-entity views 94.35 → 99.37. Goldens: e1m3 `3531e9cd` → `1867f5a7` (150 px),
    e1m1 and e1m2 unchanged.

**A4. The 3-D viewport above the status bar, with Quake's pixel aspect.** *(faithful)*

- **Evidence:** `R_SetVrect` and `SCR_CalcRefdef`: at the default viewsize 100, `sb_lines` is 48
  and the 3-D vrect is 320×152 of 320×200. The port renders 320×200 and paints the 48-line
  sbar/ibar over it (`draw_hud_into`), so **24% of 3-D pixels** are rendered and discarded.
- **Second fix, same code.** `R_ViewChanged` sets `yscale = xscale · pixelAspect`, with
  `vid.aspect` = (h/w)·(4/3) = 0.833 at 16:10. The port projects square pixels and then
  **stretches** at presentation. AUDIT H6's fix stretched the art correctly but also stretched the
  world 1.2× vertically, which Quake's projection compensates for. Fix both together.
- **Mechanism:**
  - Pass a vrect (x, y, w, h) and a pixel aspect into the camera and projection setup: `cx`, `cy`,
    `focal_x`, `focal_y`.
  - `ycenter = vrect.h/2 − 0.5 + vrect.y`, as in `R_ViewChanged`.
  - Intermission forces a full screen (`sb_lines = 0`), as `R_SetVrect` does.
- **Functions:** every pass computes its own `cx`/`cy`/`focal`: `draw_world_textured`,
  `draw_submodel`, `draw_alias_model`, `draw_viewmodel`, `draw_sprites`, `draw_particles`,
  `Frustum::from_camera`, and `apply_warp` (vrect). Also the callers in `step_walk` and `step_demo`.
- **Gain:** −24% of resolution-proportional 3-D work. That is about 3 ms at 1280×800 now, and about
  1.5 ms after A1.
- **Risk: medium,** because the change cuts across every pass. It re-baselines every golden.
- **Done.** The vrect half on branch `quake/options` (`calc_refdef`, the view rendered above
  the status bar). The pixel aspect on `quake/w2b`: `RenderOptions::pixel_aspect` and one
  `Projection` (`yscale = xscale · pixelAspect`) for every pass and the frustum; the page
  passes `vid_aspect(w, h, 4/3)`, 0.8333 at every preset. Goldens unchanged (the scene tool
  stays square). Speed within noise: the aspect adds no work, it only moves rows, and the
  vertical field of view grows by 1.2x, so ~20% more of the world can be in view (see
  `AUDIT.md`, "Projection and spans").

**A5. Mip levels in the surface cache (`D_MipLevelForScale`).** *(faithful)*

- **What it changes:** distant surfaces bake at 1/2, 1/4 or 1/8 of the texture's resolution. That
  gives Quake's filtered distant walls, where the port currently shows mip-0 shimmer; smaller
  blocks; and fewer texels per lightstyle rebake.
- **Gain:** modest for speed; the main benefits are memory and fidelity.
- **Mechanism:** `face_surf_block` keyed by (face, mip), with the C's `scale_for_mip` and
  `mipadjust`.
- **Risk:** medium. It re-baselines the goldens.
- **Done** (branch `quake/w2a`): `MipView` picks each face's level as `D_DrawSurfaces` does
  (`nearzi` over the frustum-clipped outline × `scale_for_mip` × `mipadjust`, `d_scalemip`,
  `d_minmip`), and `face_surf_block` bakes and caches one `extents >> miplevel` block per face per
  level from the BSP's own mip levels; `draw_surface_block` lights it with
  `R_DrawSurfaceBlock8_mip0..3`'s integer stepping (oracle class 6), dlit faces included. Oracle,
  320×200 world as shipped: 84.74 / 64.06 / 65.88 / 75.62 → 92.20 / 91.03 / 96.68 / 92.58 with
  the levels → 97.44 / 97.12 / 99.16 / 94.96 with the stepping (e1m1/2/3/7); against id's exact
  perspective 99.94 / 99.18 / 99.98 / 99.91.
  - **Speed,** native twin, fresh process per resolution, base → A5: texels baked per frame
    fire_e1m1 48,660 → 16,322–23,880, walk_e1m1 35,185 → 3,963–11,482, walk_e1m3 68,628 →
    2,902–10,767 (320×200–1280×800); `face_surf_block` time on fire_e1m1 0.45 → 0.03–0.05 ms per
    frame; fire_e1m1 step p95 3.79 → 1.35 ms (320×200), 4.92 → 2.75 (640×400), 8.91 → 7.40
    (1280×800). `QUAKE_DLIGHT=eye` at 1280×800: 14.1–15.1 → 7.4–9.1 ms, about the unlit frame.
    A warm static frame costs what it did (`compare.py --bench`).
  - **Memory,** surface cache resident: demo1 3.4 MB → 2.1 / 2.9 / 3.3 MB (320×200 / 640×400 /
    1280×800), walk_e1m1 1.9 → 1.0 / 1.3 / 1.7 MB, walk_e1m3 3.3 → 1.0 / 1.7 / 2.0 MB. New bench
    counters `surf_texels` and `surfcache_kb`. `AUDIT.md` has the rest and the goldens.

### B. Frame composition and presentation (wasm shell and 2-D)

**Group result** (branch `quake/perf-b`: B1–B4 against `a83bcdb`, one sitting, wasm step median
ms; native in parentheses). B5 is not done; its "what it would take" is below.

| workload | 640×400 | 1280×800 |
|---|---|---|
| demo1 | 6.28 → **4.13** (3.37 → 2.44) | 19.24 → **14.55** (11.76 → 7.77) |
| attract (demo + menu) | 5.84 → **4.20** (3.96 → 2.57) | 20.16 → **15.07** (13.08 → 9.29) |
| walk_e1m1 | 3.94 → **2.87** (2.83 → 2.05) | 13.51 → **9.36** (9.07 → 5.92) |
| quad_e1m1 (a cshift every frame) | 9.65 → **4.95** (8.57 → 5.81) | 32.80 → **17.12** (30.28 → 20.50) |

Everything outside render3d (post3d + hud2d + menu + blend + pack) at 1280×800 in wasm went from
4.96 ms to 0.90 on demo1, and from 16.6 to 1.43 on quad_e1m1.

**B2. Cshift and gamma as per-channel 256-entry ramps.** *(faithful, byte-identical variant available)*

- **Evidence:**
  - `apply_blend` measured 11.9 ms at 1280×800 and 3.3 ms at 640×400 in wasm.
  - It fires in the attract demo itself: demo1 frames 464–471 at 1280×800 take 36.6 ms against a
    22 ms median.
  - A scratch LUT version took 0.79 ms, and a native frame diff showed 0 differing pixels.
- **Mechanism, which is `V_UpdatePalette`:** `ramps[c][i] = gammatable[clamp(i·(1−a) + 255·blend_c·a)]`.
  Then:
  - either fold it into the pack (B1), giving one pass that goes from RGB to shifted, gamma'd RGBA;
  - or, with B5, apply it to the 256-entry palette.
- **Choosing the variant:**
  - The C truncates (`ir = i*a + r` into an int). The port rounds.
  - Use the C's truncation to be faithful: blended pixels move by at most 1 level, and only while a
    shift is active. The goldens have no shifts and stay put.
  - Use the port's rounding for a byte-identical first step.
- **Functions:** `apply_blend`, `build_gamma_table`, and the pack in `step`.
- **Risk:** very low.
- **Done** (branch `quake/perf-b`), the faithful variant, and more faithful than this item
  assumed: the software `V_UpdatePalette` (view.c's `!GLQUAKE` branch) never calls `V_CalcBlend`
  (that is GLQuake's). It walks `cl.cshifts` in order over each palette level with
  `v += (percent*(destcolor-v)) >> 8` (`int` percent, arithmetic shift), then `gammatable[v]`.
  `render::cshift_ramps` builds exactly that as three 256-entry ramps; the frames hand the
  dispatcher their cshift list instead of a (colour, alpha); `pack_rgba` maps every pixel through
  the ramps (gamma folded in, so a shift costs one pass). `apply_blend`/`combine_cshifts` are gone.
  quad_e1m1 (the Quad's shift on every frame), median ms, A/B in one sitting:

  | | blend + pack | step |
  |---|---|---|
  | wasm 640×400 | 2.86 + 0.12 → **0 + 0.23** | 8.38 → 5.62 |
  | wasm 1280×800 | 11.38 + 0.45 → **0 + 0.93** | 30.79 → 19.93 |
  | native 1280×800 | 7.12 + 0.42 → 0 + 0.70 | 28.88 → 22.18 |

  Frames with no shift are byte-identical (demo1, walk_e1m1 hashes equal; goldens unchanged).
  Shifted frames move by at most 2 levels per channel (`AUDIT.md`, "Frame composition").

**B1. The RGB→RGBA pack.** *(byte-identical)*

- **Evidence:** 2.34 → 0.73 ms at 1280×800, with identical hashes. The prototype is
  `fb.resize(n*4)` plus `chunks_exact_mut(4).zip(&img.rgb)`, in place of `clear()` and 4× `push`.
- **Functions:** the pack at the end of `step`, in both the gamma-1 and gamma-LUT arms.
- **Done** (branch `quake/perf-b`): `pack_rgba` in `quake-wasm/src/host.rs` resizes `vid.buffer`
  once (a no-op at a steady resolution, so its pointer stays put) and writes each pixel's four
  bytes in place through `chunks_exact_mut(4)`. Pack, median ms, A/B in one sitting:

  | | 640×400 | 1280×800 |
  |---|---|---|
  | wasm | 0.55 → **0.11** | 2.41 → **0.46** |
  | native | 0.46 → 0.11 | 1.79 → 0.42 |

  wasm step, demo1 at 1280×800: 19.9 → 17.3 ms. Framebuffer hashes (`--hash-every 60`) identical
  on demo1, walk_e1m1 and quad_e1m1 at both sizes.

**B3. Keep frame buffers across frames.** *(byte-identical)*

- **Evidence:**
  - `Image::new` plus the z-buffer `vec!` in `render_scene_ext_sprited` cost 0.39 ms per frame at
    1280×800.
  - `draw_viewmodel` allocates and fills a full-resolution `local_z` every frame: 4 MB at 1280×800.
  - `apply_warp` clones the frame and allocates 3 `Vec`s.
- **Mechanism:** a render context that owns the image, the z-buffer, the viewmodel z and the warp
  scratch, reused across frames and resized only when the resolution changes. The return-by-value
  `Image` API has to change; keep the old functions as wrappers for the tests.
- **Coordination:** A3 later removes the clears. Until then, keep `fill` so the output stays
  byte-identical.
- **Done** (branch `quake/perf-b`), without changing any signature: a small per-thread pool in
  `render/mod.rs` (like the sky-span and surface caches). The host hands each presented frame
  back (`render::recycle_image`); the next frame's view (`render_scene_ext_sprited`, still
  filled with its background exactly as before), its z-buffer, the composed screen and
  `apply_warp`'s snapshot reuse those allocations. The bigger win was in `compose_view`, which
  since the vrect framing (viewsize 100 renders the view above the status bar) allocated a
  zeroed screen, tile-cleared **all** of it and then copied the view over 76% of it: now the tile
  goes only to the four bands around the view and the screen needs no clear (a test composes over
  dirty spare buffers for every viewsize, against the old full-clear compose). post3d (warp +
  compose), median ms, A/B in one sitting:

  | | 640×400 | 1280×800 |
  |---|---|---|
  | wasm | 0.45 → **0.13** | 1.78 → **0.59** |
  | native | 0.41 → 0.11 | 1.61 → 0.47 |

  wasm step at 1280×800: demo1 17.0 → 16.1, walk_e1m1 11.7 → 10.7 ms. render3d is unchanged
  within noise (reusing its buffers saves the allocation, not the fills). Hashes identical on
  demo1, attract and walk_e1m1 at both sizes; goldens unchanged. (The viewmodel's private
  full-resolution z-buffer measured above is already gone: since the `quake/fid1` alias port the
  gun draws into the shared z-buffer with its 1/z tripled.) **Not done:** the warp could write
  straight into the composed screen instead of warping the view in place (underwater only).

**B4. HUD, menu and console blits.** *(byte-identical)*

- **Evidence:** `blit_qpic` is 3.5% of the profile. hud2d costs 0.76 ms at 1280×800 and menu costs
  1.03 ms. Per pixel, each does a float→usize source mapping plus a bounds-checked `Image::put`.
- **Mechanism:** compute a source-x map once per blit, clip rows and columns up front, and write
  the row slice directly. The same applies to `blit_qpic_at`, `draw_string_scaled` and
  `draw_char_scaled`.
- **Done** (branch `quake/perf-b`): one blit, `draw::blit_scaled` (the source-column map and the
  clipping once per blit, rows written as slices, the same float expressions as before), under
  `blit_qpic_at`, the status bar's `blit_qpic` and `draw_sbar_char`; glyph blocks
  (`draw_string_scaled`/`draw_char_scaled`, one `stamp_glyph`) as clipped row fills
  (`fill_rect`); `draw_tile_clear` with a column map and each repeated tile row copied;
  `fade_screen` as black runs between precomputed kept columns; the console's conback with a
  column map. Median ms, wasm, two A/B rounds in one sitting (they agree to ±0.01):

  | phase | 640×400 | 1280×800 |
  |---|---|---|
  | hud2d (status bar) | 0.21 → **0.09** | 0.77 → **0.28** |
  | menu (attract) | 0.39 → **0.18** | 1.51 → **0.61** |
  | post3d (tile under the bar) | 0.15 → 0.06 | 0.57 → 0.20 |
  | console (native, 40 lines) | 0.36 → 0.21 | 1.20 → 0.54 |

  wasm step at 1280×800: attract 17.2 → 15.5, demo1 15.9 → 15.2 ms. Byte-identical: bench hashes
  equal on demo1 and walk_e1m1 (attract's differ only by its menu-cursor phase, see §2); a native
  sweep of 456 distinct frames — every menu screen, the Options rows, Keys, Video, Help, Quit,
  the HUD at viewsizes 30–120, the console, the Quad, demo1 — at eight resolutions (320×200 to
  1280×800, plus 333×211 and 1280×720) is identical before and after; differential tests pin each
  helper to the per-pixel loop it replaced at fractional scales.

**B5. The 8-bit framebuffer, which is Quake's architecture.** *(faithful, later)*

- **Mechanism:** render palette indices, and apply palette + ramps + gamma once at presentation.
  One byte is stored per pixel instead of three.
- **What it removes:** B1/B2's work; the blend becomes free.
- **Requires:** every writer to emit palette indices. **Blocked on C2**: alias shading must go
  through the colormap first. The flat-hash and linear fallbacks (no colormap) are test-only paths.
- **Enables:** a WebGL present, below.
- **What it would take, seen from B1–B4** (not implemented; 2026-09-25):
  - *The blocker has moved.* C2 has in effect landed (`quake/fid1`: alias models and the gun go
    through `D_PolysetDraw` and `acolormap`; the oracle's nonpal% is 0 on every map). What still
    writes RGB that is not a palette colour: the `colormap: None` linear fallbacks in
    `raster.rs` (maps or tests without `gfx/colormap.lmp`), `hash_color` (the flat `render_bsp`,
    test-only), the untextured alias `setup.flat`, the view's `[10, 10, 14]` background (visible
    through the void; which index id shows there is for the oracle to say) and the console's
    `[10, 10, 14]` no-conback fallback.
  - *The 2-D layer is ready.* Every blit now goes through `draw::blit_scaled`, `stamp_glyph`,
    `fill_rect`, `draw_tile_clear` and `fade_screen`, and each of them looks up `palette[texel]`
    at one place: emitting the index instead is a one-line change per helper.
  - *Presentation is ready.* B2 already builds `V_UpdatePalette`'s ramps; with indices, apply them
    to the 256 palette colours once per frame (768 lookups, exactly the C), make a `[u32; 256]`
    and pack with one lookup and one 4-byte store per pixel — the cost of today's identity pack
    (0.46 ms at 1280×800), with any shift free.
  - *The work:* `Image.rgb: Vec<[u8; 3]>` → `Vec<u8>` plus the palette, through every writer
    (world, raster, surf, light, sky, warp, alias/polyse, sprite, part, the 2-D layer) and every
    test that reads pixels as RGB (they would read `palette[idx]`). The goldens are PPMs through
    the palette, so they should not move. Frame fills, the view→screen copy and the warp move a
    third of the bytes. Do it after A1 (it rewrites the world writers anyway), as one mechanical
    sweep behind a byte-identity check of the goldens and the bench hashes.
- **Done** (branch `q26/present`, 2026-09-26), as described: `render::Image` is
  `Image<u8>`, palette indices, and every writer stores the index it already had
  (the 2-D layer and `Hud` no longer take the palette). `FramePalette` is
  `V_UpdatePalette`'s shifted, gamma'd palette; `pack_rgba` maps a pixel through it
  with one 4-byte store (the 2-D canvas's path, quaketool's PPMs and hashes). The
  colours with no index (a skinless model's debug colour, the linear shading of
  colormap-less synthetic scenes, textureless test faces) take the nearest palette
  entry. Byte-identical: goldens, and `quaketool play --hash-every` over 28 runs
  including the Quad's shift. Native timedemo demo1 1280×800 on one thread: 285–300
  → 346–356 fps. In the page the frame goes to WebGL2 as an `R8UI` texture plus a
  256×1 palette (web/PLATFORM.md, "Presentation"): demo1 at 2560×1440 on 8 threads,
  page frame 10.6 → 3.4 ms on a desktop GPU.

**B6. Present path in the page.** *(neutral; lands with D because it touches index.html)*

- **Evidence:**
  - `copy` + `put` cost 0.42 ms at 1280×800 in headless. Headless composites in software; on a
    GPU-backed canvas, `putImageData` is an upload and typically costs more. I did not measure that.
- **Options:**
  - Build `ImageData` directly over wasm memory. That removes `img.data.set`; recreate the
    `ImageData` if memory grows.
  - `getContext('2d', {alpha: false, desynchronized: true})`. The second flag also cuts a frame of
    compositor latency.
  - After B5: a WebGL palette texture, which uploads 1 byte per pixel plus 256 colours.
- **Done** (branch `quake/host`): the page presents an `ImageData` built over the framebuffer in
  wasm memory (rebuilt when `memory.buffer`, the pointer or the size changes), on an
  `alpha: false` context; `image-rendering: pixelated` unchanged. The page's per-frame work
  outside `step` at 1280×800 (60 Hz headless, two sittings of 3 × 4 s): **0.34–0.37 → 0.13–0.14
  ms**. `alpha: false` alone measured neutral (0.143–0.149 without it). A present probe hashes
  the canvas against the wasm framebuffer through a demo, a JS `memory.grow`, a map load and a
  resolution change: equal everywhere, and equal to the old copy path. `desynchronized` is
  **opt-in** (`index.html?lowlatency`): neutral and identical in headless, but headless cannot
  show what it changes (Chrome on Windows/ChromeOS skips the compositor, which can tear), so it
  is not the default.

### C. Entities

**C1. Cull entities the way Quake did.** *(faithful)*

- **Evidence:**
  - walk_e1m3: 98 alias models and 20,307 triangles per frame are transformed and projected, but
    only 24.7 models have a front-facing vertex on screen.
  - walk_e1m1: 29 models and 8,928 triangles, with 9.3 on screen.
  - The alias phase costs as much at 320×200 as at 1280×800 (1.22 against 1.53 ms), so it is
    per-triangle overhead, not pixels.
- **Mechanism:**
  - For live walks, apply `SV_WriteEntitiesToClient`'s test in the entity gather of `step_walk`:
    an entity is visible when it touches the view's fat PVS (`SV_FatPVS`, via the entity's leaves
    from `SV_LinkEdict`).
  - Static entities follow `R_StoreEfrags`, per visible leaf.
  - Then apply `R_AliasCheckBBox`'s frustum test, per frame bbox, in `draw_alias_model`.
- **Demos:** the recorded stream is already PVS-culled, so only the frustum test applies.
- **Gain:** −60–75% of the alias phase and of `ModelInstance` construction. That is about −0.9 ms
  on e1m3 at every resolution.
- **Risk:** low. The visible output should be unchanged, because a model outside the PVS is behind
  solid geometry. Verify with the goldens and hashes. The scene golden frames a monster, which must
  stay.
- **Done** (branch `quake/sim`):
  - `SV_LinkEdict` records up to 16 touched leaves per edict (`SV_FindTouchedLeafs`:
    `Bsp::touched_leafs`, `Vm::edict_leafs`). `Server::entities_sent_to_client` is
    `SV_WriteEntitiesToClient`'s test against `SV_FatPVS` at the player's `origin + view_ofs`.
    `step_walk` relinks only those entities: their `EF_*` lights, trails, spin and drawing.
  - `makestatic` marks the edict a client static; the port keeps the edict. A static is drawn
    when one of its `R_AddEfrags` leaves is in the view leaf's PVS, after the relinked entities, as
    `R_StoreEfrags` appends it. Demo playback is unchanged.
  - `R_AliasCheckBBox` was already the first thing `draw_alias_model` does; nothing is
    transformed before it.
  - Counters, per frame at 320×200, native means over 720 frames. The new `alias_models`,
    `alias_accepted` and `alias_tris` counters are in `RenderStats` and `bench.py`.
    - walk_e1m3: 98 → 3.8 alias models handed to the renderer; 22.1 → 0.6 accepted by the bbox
      test (4,482 → 83 triangles); 64.8 → 4.7 external-box bakes; 185 → 5 submodel faces.
    - walk_e1m1: 29 → 0 models and 33 → 2 bakes.
  - **Frame hashes are identical** on walk_e1m1, walk_e1m3, fire_e1m1 and demo1: 720 frames each
    at 320×200, hashed every 30th. Nothing that was culled had been visible.
  - Speed: wasm medians of two interleaved rounds, native in parentheses, ms.

    | walk_e1m3 | 320×200 | 640×400 | 1280×800 |
    |---|---|---|---|
    | step | 1.57 → 1.33 (1.13 → 0.87) | 2.56 → 2.22 (1.95 → 1.58) | 6.37 → 5.80 (5.20 → 4.29) |
    | render3d | 0.68 → 0.39 | 1.50 → 1.11 | 4.56 → 3.91 |

    walk_e1m1 is within noise in wasm; native it is 4–7% faster.
  - Cost: the sim phase gains about 0.05–0.08 ms in wasm. That is `SV_FindTouchedLeafs` on every
    link, as the C pays, plus the fat PVS. It is not measurable natively.
  - The simbench counts, the census output and the goldens are unchanged. The
    `quaketool scene` tool view does not cull.
  - The L22 test (CENSUS.md) and the accepted gaps are in `AUDIT.md`.

**C2. Alias models via `D_PolysetDraw`.** *(faithful, re-baseline)*

- **Mechanism:**
  - Transform each frame vertex once, as `R_AliasPreparePoints` / `R_AliasTransformFinalVert`
    do. The port currently re-transforms each triangle's 3 vertices.
  - Light per vertex from `r_avertexnormals` · `r_plightvec`, as `R_AliasTransformFinalVert`
    does, shading through the **colormap** with the `(light & 0xFF00) + texel` lookup.
  - Use an affine texture with the 1/z test.
- **Fixes:** the current flat Lambert RGB multiply, which the code comment wrongly calls "matching
  the C". It is not colormapped and can overbright.
- **Prerequisite for:** B5.
- **Functions:** `draw_alias_model`, `mdl_frame_verts`, `mdl_vertex_model_space`, `draw_viewmodel`.
- **Done** (branch `quake/fid1`, as a fidelity fix: `render/alias.rs`, `render/polyse.rs`;
  `AUDIT.md`, "Session 7"). Entity pixels against the oracle 15.9–71.0% → 98.2–99.7%, and
  100% since the edge renderer. Its speed was not measured on its own.

**C3. The viewmodel on the shared z-buffer with Quake's ×3 bias.** *(faithful)*

- **Mechanism:** use `ziscale × 3` from `R_AliasDrawModel`, in place of a per-frame
  full-resolution `local_z` (see B3). Replace the ad-hoc `OFS_*` placement with `V_CalcRefdef`'s
  view-entity origin.
- **Gain:** part of the viewmodel's 1.4–1.9 ms at 1280×800.
- **Output:** changes where the gun meets walls.
- **Done**: the placement on `quake/options` (`V_CalcRefdef`'s origin), the shared
  z-buffer with the tripled 1/z on `quake/fid1`. The gun costs 0.20 ms at 1280×800 on
  demo1 now (was 1.37).

**C4. Cache the surfaces of external brush boxes.** *(byte-identical)*

- **Evidence:** 27 (e1m1) and 84 (e1m3) bypass bakes per frame, costing 0.27–0.87 ms regardless of
  resolution. In the wasm path, the box's `Bsp` lives in `Walk::bmodel_cache` and **is stable** (it
  is not re-cloned; only `quaketool scene` clones).
- **Mechanism:** a small per-model cache keyed by the model name or the `Bsp` pointer, alongside the
  world slot in `face_surf_block`.
- **Owner:** Agent A, because the change is inside `face_surf_block`'s bypass branch.
- **Not done.** C1 (only the boxes the server sends) and A5 (mip levels) took most of its
  cost away: walk_e1m3 bakes 2 box faces a frame at `3ba835f` (`surf_bypa`), from 84.

### D. Host loop, sim glue, startup (owns `web/index.html`)

**D1. `Host_FilterTime`: at most 72 frames per second.** *(faithful)*

- **The port today:** one `step(dt)` per rAF, with `dt` the rAF interval clamped to 0.1. The
  ambient-sound code already fakes the 1/72 accumulator.
- **Mechanism:**
  - Accumulate real time and skip the rAF (no step, no blit) while less than 1/72 s has passed.
  - Otherwise step with the accumulated time, clamped to [0.001, 0.1] as the C does.
  - Allow about half a vsync of tolerance. Otherwise, on a 144 Hz display, two intervals come out
    at 13.887 ms, just under the 13.889 ms threshold, and the rate judders between 72 and 48 fps.
    Document that tolerance as the only deviation.
- **Where:** in the page's `frame()`, or as a gate inside `step()`, which is safer for other hosts.
- **Gain:** half the work on 120/144 Hz displays; nothing at 60 Hz.
- **Done** (branch `quake/host`): the gate is inside `step()` (`host_filter_time`), which now
  returns 1 when a frame ran and 0 when the cap skipped the call; the page presents only on 1.
  `realtime` still takes every call's time, `oldrealtime` jumps to `realtime` on a run (the C's
  dropped overshoot), `dt = 0` stays the automation's always-render frozen frame. Tolerance
  **1 ms**, mid-window: above 0.56 ms a 75 Hz display runs every refresh, below 1.39 ms 165 and
  240 Hz stay at or under 72 fps (half a 144 Hz vsync, 3.5 ms, would give 82.5 and 80). Unit
  tests drive 0.1 ms-coarsened rAF stamps: 60 → 60, 75 → 75 (the tolerance's one overshoot),
  90 → 45, 100 → 50, 120 → 60, 144 → 72, 165 → 55, 240 → 60, 360 → 72 fps, each at one fixed
  vsync count per frame; jittered 144 Hz stays at 72; game time equals real time. Headless:
  60 Hz vsync unchanged (60.2 host frames/s both); uncapped rAF at 320×200, step CPU **952 →
  ~150 ms per second** (606 → ~47 host frames/s; uncapped headless rAF is not a display, so its
  rate is not a refresh rate). Bench hashes identical; `bench.py` realigns the gate before a
  fixed run and its `--live` counts skipped refreshes.

**D2. Resolve hot entity fields once.** *(byte-identical)*

- **Evidence:** 3% of the profile on e1m3 is `Progs::field_offset` plus SipHash `hash_one`. Every
  `ent_get_float(ent, "modelindex")` and similar call hashes a string, for every entity, every
  frame, in `step_walk` and in the server.
- **Mechanism:** a `FieldOfs` struct of offsets filled at progs load, standing in for `entvars_t`,
  used by the hot readers: `step_walk`'s entity gather, `client_frame`, the physics and the HUD
  stats.
- **Also:** `ent_get_string(e, "model")` allocates a `String` per entity per frame. Compare
  borrowed `&str` instead.
- **Done** (branch `quake/sim`):
  - `Vm::fo` (`FieldOfs`: every `entvars_t` field, plus `gravity`, which `SV_AddGravity` looks up
    by name) and `Vm::go` (`GlobalOfs`: `self`, `other`, `time`, `frametime`,
    `force_retouch`) are resolved by name once, in `Vm::new`. The `Fld` and `Glb` handles are
    read and written with `ent_float`, `set_ent_vec`, `ent_str`, `glob_int` and the rest. The
    by-name accessors now resolve the name and call the same code, so a missing field still
    reads 0 and drops writes.
  - Converted:
    - `SV_Move`'s edict scan, `SV_LinkEdict`, trigger touching and `SV_Impact`.
    - All of `sv_phys.rs`: the per-edict loop, thinks, the pusher, step, toss and walk moves,
      and the water checks.
    - `sv_move.rs` (monster steps).
    - The builtins that scan every edict: `PF_checkclient` (it looks for `FL_CLIENT` from edict
      1, and the port's player is the last edict), `PF_aim` and `PF_findradius`. `PF_find`
      compares borrowed strings.
    - `SV_CleanupEnts`, the entity dlights, C1's send test, `ED_Free`, and `step_walk`'s
      gather. The model-cache loop borrows the model name; it used to allocate a `String` per
      edict per frame.
  - Name lookups per frame: walk_e1m3 29,300 → 90, walk_e1m1 10,800 → 85. The rest are
    once-per-frame reads of the player and the HUD. Before, 79% of the lookups were `solid`,
    `owner`, `mins`, `maxs`, `absmin` and `absmax`: the fields `SV_Move`'s scan reads for every
    edict on every trace.
  - **Byte-identical:**
    - The simbench counts (e1m1, e1m3) and the census output (nine maps) are unchanged.
    - So are the `census-edicts` dumps of all nine maps at five times, and the goldens.
    - 720 frame hashes match C1's: walk_e1m1, walk_e1m3, fire_e1m1, quad_e1m1 and demo1 at
      320×200 and 640×400, every 10th of 720 frames.
  - Speed:
    - simbench, per tick: e1m1 0.89 → 0.11 ms, e1m3 2.64 → 0.29 ms.
    - Bench: wasm medians of two interleaved rounds, native in parentheses, ms.

      | | 320×200 | 640×400 | 1280×800 |
      |---|---|---|---|
      | walk_e1m3 step | 1.39 → 0.77 (0.89 → 0.51) | 2.46 → 1.82 (1.66 → 1.24) | 6.25 → 5.51 (4.68 → 4.35) |
      | walk_e1m3 sim | 0.83 → 0.23 (0.51 → 0.13) | 0.96 → 0.28 | 0.96 → 0.30 |
      | walk_e1m1 step | 0.97 → 0.54 (0.55 → 0.41) | 2.02 → 1.46 | 5.68 → 5.28 |
      | fire_e1m1 step | 1.05 → 0.64 (0.57 → 0.41) | 1.96 → 1.59 | 5.64 → 5.46 |

**D3. The pak as one static slice.** *(byte-identical; memory and jank)*

- **Evidence:**
  - `pak()` does `PAK.to_vec()`, an 18.7 MB copy. It is called by `boot`, `boot_demo`,
    `boot_attract`, `build_walk_map`, the savegame loader, `load_ambient_sound` (once per
    channel), `poll_menu_sound` and `load_sound`.
  - `Walk` and `DemoPlay` each own a copy, and `Server::with_pak` clones another.
  - Memory: 81 MB → 175 MB after four map loads.
- **Mechanism:** `Pak::from_static(&'static [u8])`, or a `Source::Static` variant, cloned as a
  cheap handle.
- **Done** (branch `quake/host`): `Pak::from_static` over a `Source::Static` image; the wasm
  `pak()` uses it, so every `Pak` clone (Walk, DemoPlay, `Server::with_pak`) copies only the
  directory. Measured in headless Chromium: linear memory after boot **81.4 → 52.9 MB**, after
  `map e1m1`…`map e1m4` **170.8 → 61.5 MB**; each `map` command 4–5 ms faster (e1m1 20.2 →
  16.4 ms). Framebuffer hashes and goldens identical. `server.rs` untouched.

**D4. Delivery.** *(neutral)*

- **Compression:** serve the wasm compressed; gzip is 8.41 MB against 18.65 MB. The page's
  progress bar already handles a compressed `Content-Length`.
- **Streaming compile:** use `WebAssembly.compileStreaming` or `instantiateStreaming` on the tee'd
  response, which keeps the progress bar. V8 then compiles while downloading and caches the code.
- **Split the pak out of the wasm** (option). The code is 0.77 MB; today every deploy
  re-downloads 18.7 MB. The page would fetch `pak0.pak` separately, cacheable, and copy it in
  through an alloc export, the same pattern as `sav_alloc`.
- **`wasm-opt -O3`** as an optional deploy step (§6).
- **Done** (branch `quake/host`), compression + streaming:
  - `miniserve -C` compresses on the fly: brotli 8.6 MB (Chrome's pick), gzip 9.6, zstd 8.3, for
    ~0.4 s of server CPU per download. README's browser section has the command. First frame,
    headless at an emulated 50 Mbit/s: 3.51 s → 1.76 s; on localhost it is a loss (0.19 → 0.54 s).
  - The page streams: a byte-counting `TransformStream` feeds `instantiateStreaming` (a
    pass-through rather than a `tee`, so nothing is buffered twice), re-wrapped as an
    `application/wasm` `Response`, so a wrong server MIME type still streams. No 19 MB JS copy,
    and the 30 ms "let the bar paint" pause is gone. Local first frame 218–223 → 191–196 ms
    (three interleaved runs of 7). Browsers without `instantiateStreaming` or `TransformStream`
    keep the buffered path. Checked: right MIME, `application/octet-stream`, no streaming API, and
    gzip with a compressed `Content-Length` (the bar now shows only the MB counter when a
    `Content-Encoding` is set). `bench.py` hooks both entry points.
  - Not done: the pak split and `wasm-opt`.

---

## 6. Measured and not recommended (or not now)

- **`-C target-feature=+simd128`**: identical hashes and no measurable change. LLVM
  auto-vectorised almost nothing (+67 bytes). It would also drop Safari < 16.4. Revisit only with
  hand-written SIMD, for example for A3's span loop or the B1/B2 pack. *(Revisited 2026-10-03:
  once the span loops were tight, the one loop LLVM vectorizes, the z-buffer row, was worth
  2–9% of a frame, and the browser builds now use it, dropping Safari < 16.4: §15.)*
- **16-pixel affine subdivision on its own**: no wasm gain on top of A1. Do it for fidelity, as
  part of A3, not for speed. **Done anyway, for fidelity** (branch `quake/w2b`: `D_DrawSpans16`
  and `Turbulent8` over z-test runs, see `AUDIT.md`), and in wasm it is a gain after all, because
  the row is now two tight loops — the z test with its one divide per pixel, then the texels by
  integer steps over the runs that passed. A/B on the merged base (`quake/overnight` `eb76c04`,
  with the mip levels) against `quake/w2b`, one sitting, two rounds, median ms:

  | workload | wasm world 640×400 | wasm world 1280×800 | wasm step 1280×800 | native world 1280×800 |
  |---|---|---|---|---|
  | demo1 | 1.25/1.30 → 1.07/1.07 | 4.11/4.17 → 3.30/3.34 | 6.04/6.15 → 5.28/5.45 | 3.10/3.16 → 4.30/4.14 |
  | walk_e1m1 | 0.83/0.83 → 0.64/0.63 | 3.10/3.07 → 2.38/2.39 | 4.98/5.07 → 4.30/4.49 | 2.33/2.38 → 3.21/4.06 |
  | fire_e1m1 | 0.85/0.85 → 0.69/0.66 | 3.15/3.11 → 2.49/2.43 | 5.15/5.07 → 4.53/4.63 | 2.37/2.34 → 3.20/3.34 |
  | walk_e1m3 | 0.92/0.93 → 0.80/0.75 | 3.35/3.37 → 2.73/2.60 | 6.16/6.19 → 5.86/5.67 | 2.67/2.56 → 3.64/3.56 |

  Wasm world −16 to −23%, step −5 to −13% (the same as before the merge). **Native is the other
  way:** world +30-40%. Of four structures tried, a depth pass into a row buffer and then the
  texels was the fastest natively (−10% against exact) but +8% in wasm; the shipped one is the
  fastest in wasm. Not understood; the browser is the target. (Gone with A3: id's spans need no
  z test, and the native frame is −43 to −57%.)
- **`wasm-opt -O3`** (binaryen v132 via `bunx -p binaryen`):
  - identical hashes; step −2 to −5% (demo1 1280×800: 19.6 → 18.7 ms);
  - 640 KB smaller: code 773 → 662 KB, and the name section is stripped.
  - Worth adding as an **optional** deploy step, with `-g` if profiling names are wanted. It is not
    committed, because it adds a non-cargo tool to the build.
- **Build profile**: `quake-wasm` already has `opt-level=3`, fat LTO, `codegen-units=1` and
  `panic=abort`. There was nothing left to tune.
- **Departures that are faster but not faithful** (opt-in extras only; I recommend none now):
  - an uncapped frame rate: the opposite of D1, for high-refresh displays;
  - a GL-style renderer;
  - worker-thread band rendering: needs SharedArrayBuffer/COOP and wasm threads; the output would
    be identical, but the infrastructure is heavy.

---

## 7. What makes it feel slow, beyond frame time

*At the baseline. Since then: the spikes (B2, A2), the pak copies (D3) and the work per
display refresh (D1) are fixed; the mip levels (A5) shrank the surface cache; the page's
canvas is the largest 4:3 box the window fits (976×732 in a 1440×900 window, so the
default 960×600 is no longer squeezed into 640×480); the download (the pak split, D4) and
the input latency are as described.*

- **Missed vsyncs at the default resolution.** At 960×600 in the page's own loop, 6% of frames took
  longer than 20 ms at 60 Hz: a step median of 14.2 ms against a 16.7 ms budget, with p95
  19.8 ms. A1 + B1 + B2 should bring 960×600 to about 7 ms.
- **Spikes exactly when the action happens:**
  - damage flash, underwater tint, powerups: +12 ms at 1280×800 (B2);
  - muzzle flashes, explosions, rockets: dynamic lights, +39% p95 when firing (A2);
  - underwater warp: +1.7 ms (B3);
  - first sight of a surface: a synchronous bake.
- **Frame pacing on high-refresh displays.** The sim and renderer run at the display rate with
  variable dt, which is 2× the work of Quake at 144 Hz (D1). With D1, keep the tolerance so 144 Hz
  gives a steady 72 fps and does not judder.
- **Input latency.** Keys and the mouse accumulate and are read at the next rAF. Then comes the
  step, then the compositor: about 2–3 frames. `desynchronized: true` (B6) can remove one.
- **Startup.** It is 0.2 s locally. Anywhere real, it is dominated by the 18.7 MB download (D4),
  and every engine update re-downloads the pak too.
- **Memory.** 81 → 175 MB after a few level loads (D3), and the mip-0 surface cache grows without
  bound (A5). Wasm memory never shrinks, so on phones this is the risk.
- **Resolution against display.** The default 960×600 is shown in a 640×480 CSS box. On a DPR-1
  display that renders 1.9× the pixels it shows, then down-samples them with `pixelated`, which
  wastes work and adds shimmer. I leave this as an observation: the default was the user's choice
  (STATUS 2026-06-09).

---

## 8. Parallel split for 2–4 agents (after the module refactor)

| agent | items | touches (functions) | must not touch |
|---|---|---|---|
| **A: world** | A1 → A2 → C4 → A0; later A5, A3 | `draw_world_textured`, `draw_submodel`, `raster_triangle_cached`, `raster_triangle_tex` (world uses), `face_surf_block`, `face_lightmap_world_cached`, `any_dlight_reaches`, `compute_visible_faces` | alias, viewmodel, 2-D, the wasm shell |
| **B: frame and 2-D** | B1 + B2 (one commit each) → B3 → B4; later B5 | the pack in `step`, `apply_blend`, `apply_warp`, `build_gamma_table`, the buffer setup in `render_scene_ext_sprited`, `blit_qpic*`, `draw_hud_into`, `draw_menu`, `draw_string_scaled`/`draw_char_scaled`, `draw_console` | the world raster internals |
| **C: entities** | C1 → C2 → C3 | `step_walk`/`step_demo` entity gather, `draw_alias_model`, `draw_viewmodel`, the `mdl_*` helpers, `draw_sprites`/`draw_particles` z tests | `face_surf_block` |
| **D: host, sim, delivery** | D1, D2, D3, D4, B6 | `web/index.html` (only D edits it), `Server`/`Progs` field access, `pak.rs` + the wasm `pak()`, the build/deploy notes | the renderer |

**A4 (vrect and pixel aspect)** touches every pass's projection setup. Land it **last and alone**,
after A1 and C2, as its own golden re-baseline, taken by whichever of A or C finishes first.

**Order.** The byte-identical items first: B1, B2 (rounding variant), C4, A0, D2, D3. Then A1 and
A2. Then the faithful re-baselines: C1, C2/C3, A4, then A5/A3.

**For every item:**
- Run `bench.py --native` before and after, in the same sitting.
- Check the goldens, and diff the frame hashes with `--hash-every` for the byte-identical items.
- Add an `AUDIT.md` entry with pixel counts for anything that moves the goldens.

**Projected result** (additive, from the measured prototypes plus the estimates marked in §5): demo1
at 1280×800 goes from ~19 ms to **~6–8 ms**:

| item | saving |
|---|---|
| A1 | −9 ms |
| B1 | −1.6 ms |
| A4 | −1.5 ms |
| B3 | −0.4 ms |
| C1 / C3 | ~−1 ms |

With B2 and A2, the damage-flash and firing spikes are gone too. At the default 960×600, that
means about 4–5 ms per frame, far inside a 60 Hz budget.

*Outcome: demo1 at 1280×800 measured 4.53 ms at `3ba835f` (the table at the top), below
the projection, mostly because A3 was done as well.*

---

## 9. Open, or not verified

- **Real GPU browsers.** Everything was measured in headless Chromium with software compositing on
  a loaded 16-core x86. I did not measure Firefox, Safari, a GPU-backed canvas (where
  `putImageData` is an upload), phones, or a real 120/144 Hz display. D1's gain is argued from
  the code, not measured. *(2026-09-26: headless Chromium on a desktop GPU is measured
  since `q26/present`, `QUAKE_GPU=1`, and Firefox runs the checks headless. A real display,
  Safari and phones are still unmeasured.)*
- **Prototypes are evidence, not implementations.** The A1, B1 and B2 prototypes ran in a scratch
  copy, with atomic counters present. A1's prototype takes its gradients from one triangle and was
  checked for cracks only by counting background pixels on two frames.
- **Estimates, not measurements:** the gain for C2. (C1's and A3's savings are measured; see
  C1 and A3.)
- **Fidelity issues found, not fixed** (they belong in `AUDIT.md`) — *all fixed since:*
  - A4: vrect and pixelAspect. (Fixed on `quake/options` and `quake/w2b`.)
  - C2: alias models are not colormapped, and the code comment claims otherwise. (Fixed on
    `quake/fid1`.)
  - B2: the cshift rounds where the C truncates. (Fixed on `quake/perf-b`.)
  - A2: dlit faces are lit per pixel. (Fixed on `quake/w1`.)
  - C3: the viewmodel placement is ad hoc. (Fixed on `quake/options` and `quake/fid1`.)
- **Not investigated:**
  - The surface cache's steady-state memory per map (A5).
  - Sound decode/playback cost in the page: the bench runs with audio locked, so the page's
    `drain*Sounds` do not play anything.

---

## 10. id's own measurement: `timedemo` (2026-09-25, branch `quake/timedemo`)

`timedemo demo1` — how 1996 measured Quake — runs in the port as id's `CL_TimeDemo_f`: the
demo plays one recorded message per host frame with no 72 fps cap and no pacing (`cls.timedemo`:
`Host_FilterTime` never skips; the demo clock is each message's own time, as `CL_LerpPoint` gives
in a timedemo), frames count from the second one after the command (`td_startframe`,
`td_starttime`), and `CL_FinishTimeDemo` prints `"%i frames %5.1f seconds %5.1f fps"` to the
console. Two ways in:

```sh
quaketool timedemo pak0.pak demo1 --res 320x200,640x400     # native: the page's client, key_game
oracle/build/quake-oracle -basedir DIR -oracle_realtime -width 640 -height 400 +timedemo demo1
```

and the browser console's `timedemo demo1` (`web/verify_timedemo.py` runs it). **The frame counts
are id's:** demo1 969, demo2 985, demo3 1090 frames, the port and id's C alike.

**What a frame includes.** id's C (the oracle): portable C, gcc -O2 x87, one core, null
drivers — the demo message, the 3-D view into the 8-bit buffer, the status bar and text; no
`VID_Update` blit, no sound mixing. The port natively (`quaketool timedemo`): what the page's
`step` does for a frame with the menu and console closed — the message, the 3-D view, the status
bar and text, and the frame through the palette-shift ramps into RGBA (the page's pack; the C has
no such step); the sound calls are dropped. `realtime` is the wall clock at the top of each frame
in both.

**The browser** (the console's `timedemo demo1` in headless Chromium, `web/verify_timedemo.py`):
the page runs host frames back to back in ~12 ms slices, one per animation frame, and presents
the last of each; every `step` is handed the previous one's own duration, so `realtime` adds up
the frames' time and not the pauses between slices. A frame is the whole `step` — the message,
the 3-D view, the 2-D layer (the menu and console were closed), the palette ramps and the RGBA
pack — in wasm; not the canvas's `putImageData` (once a slice) nor the page's sound drain.

On `quake/overnight` with id's edge renderer (`quake/edge`), demo1, median fps; native and id's
C in one sitting, alternated, 5 rounds (load 2.5–4.6 during the runs); the browser 3 runs.
id's C at square pixels and at the page's pixel aspect (`-oracle_aspect 0.8333333`: every preset
is 16:10 shown at 4:3, which the port draws):

| demo1 | id's C, square pixels | id's C, the page's aspect | port, native | port / id's (same aspect) | port, browser (wasm) |
|---|---:|---:|---:|---:|---:|
| 320×200 | 1880 | 1822 | 2602 | 1.43 | 1916 |
| 640×400 | 767 | 745 | 1007 | 1.35 | 723 |
| 960×600 | 427 | 419 | 516 | 1.23 | 381 |

The port's native frame includes the RGBA pack the C does not have (6–8% of a native frame,
measured before the edge renderer). Before `quake/edge` (the polygon-span world pass, same
harness): native 1678 / 530 / 249 fps against id's 1961 / 786 / 445 — the edge renderer took the
port from 0.56–0.86x of id's C to 1.23–1.43x.

---

## 11. Every core (2026-09-26, branches `q26/multicore` and `q26/present`)

id's renderer ran on one CPU. The port's draws a frame on as many threads as the platform
offers, with the same pixels for any count (`render/band.rs`).

- **What is done once.** The frame splits after the edge scan. What the whole frame
  decides runs once, in id's order: the world walk and `R_ScanEdges`,
  `D_DrawSurfaces`' per-surface setup with the surface cache filled, the alias models'
  vertices, light and clipped triangles, and the particles' squares.
- **What runs in bands.** The view is cut into bands of whole rows, four a thread,
  handed out as threads come free. Each band runs the same passes in id's order on its
  own rows: the world's spans and `D_DrawZSpans`, then the alias models, particles,
  sprites and the gun, each through the 16-bit z-buffer. Every pixel sees the same writes
  in the same order whichever thread draws its band, so the frame is the same for any
  count.
- **How the threads are made.** They are scoped threads (`std::thread::scope`), the only
  safe way to lend a frame's buffers. A thread the system refuses leaves its bands to the
  others, so a build without threads draws the same frame.
- **The rest of the frame.** The underwater warp and the 2-D canvas's RGBA pack run in
  row runs on the same threads.

It rests on R3 (`CODE_PLAN.md`): the `Renderer` owns every cache and buffer that used
to be a thread-local.

**Identity.**
- Natively, at 1, 2, 3 and 16 threads: the goldens, the `play` hashes (320×200 and
  640×400 Classic, 1920×1080 2026 video), the timedemo frame counts, and `shot` at
  1280×800 to 4K are byte-identical.
- In the browser, `bench.py --hash-every 30` prints the same hashes at 1 and 8 threads.
- A unit test draws a scene with every kind of entity at 1–16 threads.

**Native** (`quaketool timedemo <pak> demo1 --res W×H --video modern --threads N`; the
frame includes the RGBA pack). Median of three rounds, 2026-09-26 on `244bcd5`, the
16-thread desktop at load 1–3, frames per second:

| threads | 1920×1080 | 2560×1440 | 3840×2160 |
|---|---:|---:|---:|
| 1 | 208 | 120 | 54 |
| 8 | 689 | 455 | 210 |
| 16 | 682 | 450 | 224 |

Eight threads give 3.3–3.9x. Sixteen (on 8 cores, 2 threads each) add nothing at 1080p and
1440p and 7% at 4K. What stays serial (the edge scan, the surface-cache fills, the demo
message) bounds it. The branch's own sitting on `a2c2944`, before B5, at 8 threads:
1080p 163 → 610 fps, 1440p 97 → 385, 4K 45 → 173.

**In the browser** (`bench.py --build --threads-build --video modern --threads 1,8`,
demo1, headless Chromium).
- The page's frame at 2560×1440 took 19.07 ms on one thread and 9.89 ms on 8
  (`q26/multicore`, `web/PLATFORM.md` "Threads"). The copies around the frame then
  dominated: 0.9 ms each at 1440p.
- With B5 and the frames read where they lie in shared memory (`q26/present`, on the
  local GPU), 8 threads take 3.42 ms, against 10.62 before, in one sitting
  (`web/PLATFORM.md`, "Presentation").
- The closing review, demo1 at 2560×1440 with pixel size 1, median page frame:

  | build | on the GPU | software compositing |
  |---|---:|---:|
  | threads (16) | 3.3 ms | 5.4 ms |
  | single-thread | 10.8 ms | 15.4 ms |

**The knob.** `r_threads` (0, the default, is Auto: every thread the host offers). The
page's host hands the program `-hwthreads` (its pool of workers plus one). The 2026
profile's Auto pixel size grows its budget with the threads: a 1080p frame's pixels
times the whole square root of the thread count, so 4–8 threads draw a 1440p screen at
pixel size 1.

## 12. Underwater and phone-resolution frames (2026-09-30, branch `fleet/water`)

The user's report: on an Android phone (the threads build, 2026 profile, pixel size
1 so the 3-D view is roughly 2244x1080–2640x1080), underwater levels (e1m2) felt a tiny
bit slow. The chair's native measurement at 2640x1080 found underwater +40–50%
over dry and poor thread scaling at this resolution.

**Where the time actually goes.** Isolated with `quaketool view --bench` (the
`--vrect` flag forces a plain `render()` with no warp, even with the eye underwater, so
render cost can be measured apart from the warp) and two focused unit benchmarks (since
removed; see "Method" below):

- The hires extra (`VideoCvars::hires`, on by default in 2026) renders the submerged view
  at the SCREEN's own size (`screen::warp_vrect`), not id's 320x200 buffer, so
  `D_WarpScreen`'s per-pixel gather (`render/warp.rs`'s `warp_scaled`) runs over the whole
  frame — at 2640x1080, 2.85M pixels of gather, every underwater frame.
- `warp_scaled`'s inner loop computed `rowptr[v + tu] * w` — a multiply by the view's
  width on every pixel — when `rowptr` could hold that row's byte offset already
  multiplied, once, at table-build time (`WarpTables::prepare`, called only when the
  view/screen sizes change).
- Independently of the warp: `render/raster.rs`'s `turb16_span` (*since 2026-10-03
  `turb_span::<N>`, `Turbulent8` at any perspective span, 16 id's*; `Turbulent8`,
  `D_DrawTurbulent8Span`'s span sampler — the DEFAULT liquid renderer in both profiles,
  `exact_perspective` is off by default; *since 2026-10-03 the 2026 profile draws
  `r_perspspan 8` (`RenderOptions::persp_span`; it drew exact for part of that day), so
  there the liquids take `turb_span::<8>`, and `r_perspspan 1` takes `span_turb`'s exact
  branch, which has the mask too*) did two `i32::rem_euclid` divisions per liquid
  pixel to wrap into the 64x64 texture. Quake's liquid miptextures are always a power of
  two (64x64), and for a power-of-two modulus `n`, two's-complement `v & (n-1)` equals
  `v.rem_euclid(n)` for every `i32`, negative included — so the wrap is a mask, not a
  division, whenever the texture size is a power of two (checked at runtime; any other
  size, never id's data, still divides). This is on the critical path for EVERY visible
  liquid pixel, dry or underwater — looking at a lake from above pays it too.

**Changes (both byte-identical; see "Proof").**

- `render/raster.rs`: added `wrap_texel(v, n)`, used by `turb16_span` and by
  `span_turb`'s exact-perspective branch (the same wrap, written once instead of twice).
- `render/warp.rs`: `WarpTables::prepare` now stores `rowptr[v] * w` instead of `rowptr[v]`,
  so `warp_scaled`'s inner loop does `view.pixels[rowptr[v + tu] + column[tv + u]]`, one
  fewer multiply per output pixel.

**Tried and reverted:** hoisting the column's `tu = sin[phase + u]` lookup out of the row
loop into a once-per-frame `Vec<usize>` (it only depends on `u`, not `v`, so it is read
`out_h` times more often than it needs to be). Measured SLOWER (a probe at 2640x1080, 1
thread: 2.2 ms with the inline lookup vs 6.0 ms hoisted into a fresh `Vec` every frame) —
the sine table is small enough (one L1-resident array) that the redundant reads are
nearly free, and the extra allocation and indirection cost more than they saved. Not kept.

**Method.** Two temporary `#[ignore]`d benchmarks (removed before this commit; the
numbers below are from them and from `quaketool view --bench`) measured `warp_scaled` and
`turb16_span` in isolation, each the median of several trials (timings are noisy,
load 7–12 during this round) to avoid chasing one noisy sample. End-to-end numbers below
are `quaketool view`'s `--bench`, median of 5 interleaved runs of a `before`/`after`
binary pair (same process, alternating, so both see the same load swings).

**Isolated** (2640x1080, hires scale ~6.7, median of 9 trials of 40 frames):

| | 1 thread | 8 threads |
|---|---:|---:|
| `warp_scaled`, before | 2.52 ms | 0.52 ms |
| `warp_scaled`, after | 2.21 ms | 0.47 ms |

| | before | after |
|---|---:|---:|
| `turb16_span`, 2640x100, ns/pixel | 4.92 | 2.17 |

**End to end**, native, e1m2, `--video modern`, median of 5 interleaved runs
(`quaketool view ... --origin 1788,296,Z --angles 0,-90,0 --bench 120`), Z=96
underwater / Z=180 dry, same spot:

| resolution | condition | threads | before | after |
|---|---|---:|---:|---:|
| 2640x1080 | underwater | 1 | 11.15 ms | 9.57 ms (−14%) |
| 2640x1080 | underwater | 8 | 2.11 ms | 1.90 ms (−10%) |
| 2640x1080 | dry | 1 | 7.56 ms | 5.94 ms (−21%) |
| 2640x1080 | dry | 8 | 1.83 ms | 1.47 ms (−20%) |
| 1320x540 | underwater | 1 | 2.31 ms | 1.96 ms (−15%) |
| 1320x540 | underwater | 8 | 0.735 ms | 0.691 ms (−6%) |
| 1320x540 | dry | 1 | 2.06 ms | 1.61 ms (−22%) |
| 1320x540 | dry | 8 | 0.643 ms | 0.564 ms (−12%) |

The dry view at this spot already shows a lot of liquid (the pool from above), which is
why it gains as much as or more than the submerged one: `turb16_span`'s fix pays off
wherever a liquid surface is on screen, not only underwater. The warp's own fix is the
smaller of the two (12–14% of `warp_scaled` alone); most of the frame-level gain is the
division removed from the liquid span. Underwater is still slower than dry at the same
spot (the gather is real, additional work the C's 320x200 buffer never paid at this
resolution) — this round made both cheaper, not equal.

In the browser (headless Chromium, the threads build, `web/bench.py --build
--threads-build --workloads walk_e1m2 --res 1280x800 --threads 1,8 --video modern`;
1280x800 is the harness's clamp): `post3d` (which includes the warp) read 0 — the
scripted walk from e1m2's spawn (a 360° look, 1 s forward, about-face, back, about-face)
never reaches water in its 10 s loop, so this workload does not exercise the warp.
`quake-wasm` links the same `quake-rs` engine crate unchanged by platform, and its own
172 tests (byte-identical framebuffers included) pass against this round's code, so the
native proof above carries over; a true in-browser underwater timing would need a
scripted teleport or a longer/aimed walk script, not built this round.

**Proof.** Byte-for-byte: the three native goldens; `oracle/classic_check.py` (9/9);
`quaketool framerate --check` (22 scenarios); `cargo test --release` in both crates (709
/ 172, 0 failed); `cargo clippy --release --all-targets` in both crates and
`--target wasm32-wasip1` in `quake-wasm` (0 warnings); all 15 `web/verify_*.py` against a
threads-build deploy dir (`verify_threads.py` builds its own `threadcheck` program —
run it with no deploy-dir argument, not against `$D`). Hashes of underwater and dry
frames at 2640x1080, 1320x540, 640x400 and 320x200, Classic and `--video modern`, 1 and 8
threads, were recorded before this round's changes and compared identical after (32
frames, all match). A new test,
`render::tests::an_underwater_hires_frame_is_the_same_on_any_thread_count`, renders a
submerged hires-scale view with a liquid surface through `Renderer::render` +
`Renderer::warp_into` at 1, 2, 3, 5 and 8 threads and asserts the same bytes; a second,
`render::raster::tests::wrap_texel_matches_rem_euclid_for_every_modulus`, checks the new
helper against `rem_euclid` directly for power-of-two and non-power-of-two moduli across
negative, zero and boundary `i32` values.

## 13. The frame's bakes on every core (2026-10-03, branch `fleet/torchlight`)

§11 left the surface cache's fills in the frame's serial part: `D_DrawSurfaces`' setup
baked each stale block (`R_DrawSurface`) on the calling thread as it came to the face,
before the bands. The 2026 extras made that the largest serial piece: the torch flicker
(`r_torchflicker`) rebakes most of a torch-lit room's blocks every frame at 72 Hz, the
light-style glide (`r_lerplightstyles`) a flickering light's blocks at every step, and a
dynamic light every block it touches. At 1080p on 8 threads the torch-lit views took
2.2–3.3 ms at 72 Hz, two-thirds of it serial, past 480 Hz's 2.08 ms.

- **Look up first, bake after.** The setup now only looks the blocks up, in id's order.
  A miss stores its cache entry at once, marked pending, and adds a `surf::BakeJob` to
  the frame's list: the lightmap (built as before, serially — a few hundred luxels a
  face), the texture's level, the colormap and the block's size. A later ask for the same
  face in the frame (an inline model drawn by two entities) finds the pending entry and
  shares the job, dynamic light or not; a miss on a face asked for earlier replaces the
  entry, so the cache ends a frame as one-by-one baking left it.
- **Bake together.** Once every surface is looked up, `surf::bake_all` bakes the jobs on
  the renderer's threads (`band::map_jobs`: scoped threads taking jobs one at a time
  from a counter, the largest first so they end together, the results in the jobs'
  order). Then each waiting surface and pending entry takes its block, and the bands
  draw as before: nothing is baked inside the bands, no lock is in a span loop, no block
  is baked twice, and nothing is baked that is not drawn.
- **Why this shape.** It is the dumb robust one: a bake reads only its job (the
  lightmap it owns, a texture level, the colormap), so it is a pure function of the job,
  and the frame and the cache are the same for any thread count — the rule of
  `band.rs`, with no new state shared between threads. The threads are scoped threads,
  as the bands' (a pool kept across frames would need the frame's data owned or
  `'static`); in the page each is a wake-up of one of the host's pooled workers
  (`web/wasi.js`), as the bands' are. Baking inside the bands, at a band's first touch of
  a face, would save the second round of thread starts but needs a lock or a once-cell
  per block in the span setup and bakes a face shared by two bands on whichever comes
  first; not built here. (§14 built it: a once-cell per block, and every thread takes
  the bake jobs before its first band.)
- **When it pays.** A texel of baking costs about 0.7 ns, and a round of threads
  (spawn, run, join) about 32, 61 and 77 µs natively for 1, 3 and 7 helpers when
  threads ran a moment before, 145–415 µs after an idle gap (14 ms: a 72 Hz
  frame's first round); in the page 10–40 µs warm and 160–265 cold (the bakes'
  review, 2026-10-03). `bake_all` starts one thread per 32K texels of the frame's
  bakes (`BAKE_TEXELS_PER_THREAD`), and none unless that makes three
  (`BAKE_MIN_THREADS`): with the display's real time between frames
  (`framerate --bake --paced`, 1080p, 72 and 480 Hz, the four views below, two
  sittings), two threads won nothing on any view and lost up to 0.2 ms (e1m3's
  flames at 72 Hz: 5.81 ms with the bakes on one thread, 6.02 on two), four and
  eight won up to 0.7 and 1.3 ms (e1m3's flames at 8 threads 4.62 → 3.34 at 72
  Hz, 2.36 → 1.75 at 480), and a share of 64K or 128K texels a thread instead of
  32K changed nothing beyond the noise. Under three threads' worth of texels —
  a frame's usual: a muzzle flash's few blocks, a light style's step — and with
  one or two threads, the bakes stay on the calling thread, with no thread
  started.

**Identity.** The three goldens, `classic_check` (9 checks), and the `play` hashes of
demo1–3, `walk_e1m1`, `fire_e1m2` and `walk_e1m3` at 640×400, Classic and `--video
modern` (the torches flickering, the glide on), 1 and 8 threads, are the same before and
after. Unit tests (`render::surf::tests`): the lightmapped room's frames and cache, a light
moving and a style stepping, on 1, 2, 3, 8 and 16 threads; e1m3's torch-lit flames with a
moving light on 1–16 threads (each frame bakes enough for at least four threads); warm and
cold renderers hold the same blocks; e1m2's model 52 drawn twice bakes each block once,
lit or not; a face the frame does not draw is not baked; small bakes and a single job stay
on the calling thread.

**Native** (`quaketool framerate <pak0>,<pak1> --bake --threads 1,2,4,8,16 --reps 2
--secs 3 --res W×H`: the live game standing at each view, the 2026 video settings, the 3-D
view's median ms a frame over interleaved runs, before → after in the same sitting,
2026-10-03, load 1–6; "serial" is the view's time less its bands' wall time at 8 threads,
from a run with the counters on). 1920×1080:

| view | Hz | blocks (texels) a frame | 1 | 2 | 4 | 8 | 16 | serial at 8 |
|---|---|---|---|---|---|---|---|---|
| e1m2's start | 72 | 110 (0.68M) | 4.49 → 4.85 | 3.44 → 3.57 | 2.58 → 2.31 | 2.19 → 1.75 | 2.13 → 1.74 | 1.19 (54%) → 0.94 (54%) |
| e1m2's start | 480 | 61 (0.37M) | 4.72 → 6.69 | 3.52 → 3.68 | 2.39 → 2.41 | 1.91 → 1.82 | 1.84 → 1.76 | 1.08 (56%) → 0.91 (50%) |
| e1m3's flames | 72 | 134 (1.50M) | 5.62 → 5.33 | 4.32 → 3.94 | 3.40 → 2.70 | 3.06 → 2.12 | 2.97 → 2.12 | 1.91 (62%) → 1.14 (54%) |
| e1m3's flames | 480 | 85 (1.05M) | 5.60 → 5.41 | 3.87 → 3.82 | 3.19 → 2.48 | 2.52 → 1.89 | 2.50 → 2.01 | 1.64 (65%) → 1.08 (57%) |
| e4m5's flames | 72 | 188 (1.62M) | 5.55 → 6.30 | 4.48 → 4.55 | 3.57 → 3.07 | 3.31 → 2.13 | 3.20 → 2.10 | 2.06 (62%) → 1.29 (60%) |
| e4m5's flames | 480 | 76 (0.69M) | 5.29 → 5.93 | 3.86 → 4.01 | 3.02 → 2.77 | 2.44 → 2.27 | 2.36 → 2.32 | 1.34 (55%) → 1.16 (51%) |
| e2m5's gliding torches | 72 | 41 (0.38M) | 3.60 → 3.75 | 2.60 → 2.60 | 1.87 → 1.60 | 1.19 → 1.06 | 1.15 → 1.03 | 0.58 (49%) → 0.45 (42%) |
| e2m5's gliding torches | 480 | 17 (0.17M) | 3.51 → 3.50 | 2.47 → 2.50 | 1.62 → 1.56 | 1.03 → 1.02 | 0.94 → 0.95 | 0.32 (31%) → 0.33 (32%) |
| e1m1, firing rockets | 72 | 79 (0.24M) | 4.29 → 4.93 | 3.09 → 3.37 | 2.21 → 2.22 | 1.51 → 1.52 | 1.47 → 1.42 | 0.72 (47%) → 0.71 (46%) |
| e1m1, firing rockets | 480 | 76 (0.23M) | 4.25 → 4.97 | 3.09 → 3.31 | 2.21 → 2.15 | 1.51 → 1.54 | 1.46 → 1.53 | 0.71 (47%) → 0.71 (46%) |

1315×535 (a wide frame):

| view | Hz | blocks (texels) a frame | 1 | 2 | 4 | 8 | 16 | serial at 8 |
|---|---|---|---|---|---|---|---|---|
| e1m2's start | 72 | 109 (0.23M) | 2.18 → 1.87 | 1.67 → 1.72 | 1.35 → 1.18 | 1.06 → 0.94 | 1.17 → 1.00 | 0.83 (78%) → 0.64 (68%) |
| e1m2's start | 480 | 61 (0.13M) | 1.79 → 1.77 | 1.63 → 1.63 | 1.27 → 1.14 | 0.96 → 0.90 | 1.00 → 0.96 | 0.64 (67%) → 0.59 (66%) |
| e1m3's flames | 72 | 154 (0.78M) | 2.36 → 2.36 | 2.20 → 2.07 | 1.92 → 1.44 | 1.57 → 1.13 | 1.65 → 1.28 | 1.25 (80%) → 0.82 (73%) |
| e1m3's flames | 480 | 98 (0.51M) | 2.21 → 2.16 | 2.03 → 1.93 | 1.67 → 1.43 | 1.40 → 1.10 | 1.46 → 1.23 | 1.09 (78%) → 0.77 (70%) |
| e4m5's flames | 72 | 204 (1.01M) | 2.71 → 2.72 | 2.56 → 2.32 | 2.20 → 1.68 | 2.05 → 1.29 | 2.16 → 1.41 | 1.47 (71%) → 0.93 (72%) |
| e4m5's flames | 480 | 84 (0.45M) | 2.27 → 2.28 | 2.13 → 2.09 | 1.71 → 1.52 | 1.42 → 1.21 | 1.49 → 1.33 | 1.02 (72%) → 0.83 (69%) |
| e2m5's gliding torches | 72 | 39 (0.36M) | 1.41 → 1.41 | 1.29 → 1.24 | 0.91 → 0.69 | 0.72 → 0.54 | 0.75 → 0.61 | 0.46 (65%) → 0.30 (57%) |
| e2m5's gliding torches | 480 | 16 (0.16M) | 1.20 → 1.21 | 1.10 → 1.10 | 0.72 → 0.63 | 0.52 → 0.46 | 0.54 → 0.53 | 0.20 (39%) → 0.20 (44%) |
| e1m1, firing rockets | 72 | 74 (0.16M) | 1.54 → 1.57 | 1.46 → 1.48 | 1.06 → 0.95 | 0.78 → 0.74 | 0.85 → 0.82 | 0.47 (60%) → 0.44 (60%) |
| e1m1, firing rockets | 480 | 71 (0.16M) | 1.53 → 1.54 | 1.44 → 1.48 | 1.05 → 0.96 | 0.78 → 0.73 | 0.88 → 0.82 | 0.46 (59%) → 0.45 (60%) |

- The torch-lit views at 1080p on 8 threads drop from 2.2–3.3 ms to 1.75–2.1 at 72 Hz
  (−20 to −36%), and 1.9–2.5 to 1.8–2.3 at 480; the serial part from 1.2–2.1 ms to
  0.9–1.3. At 1315×535, −11 to −37% at 8 threads.
- The 1080p 1-thread column of the "after" run sat at the sitting's busiest moment (load
  4.5 against 1.1): the same views on one thread, before and after interleaved five times
  in a quiet sitting, are within ±1.5% of each other (e1m2's start 4.61 → 4.65 ms and 4.39
  → 4.43, e4m5 5.67 → 5.61 and 4.99 → 5.04, the rockets 4.09 → 4.02 and 4.11 → 4.08):
  one thread costs nothing extra.
- **What did not pay:** the rocket fight. Its explosions and muzzle flashes rebake 75–80
  blocks a frame, but small ones (0.24M texels, 0.2 ms of baking), and a dynamic light's
  serial work is mostly elsewhere (marking the faces, building the lit lightmaps); its
  frames are the same within noise. The glide at 480 Hz (17 blocks) neither.
- **What is serial now.** On e1m3's flames at 1080p, 8 threads (a profile): the edge scan
  0.53 ms, the world walk 0.08, the lookups with their lightmaps about 0.15, the bakes'
  own wall time 0.26 (0.97 on one thread: 3.7x on eight — the thread starts and the
  largest block bound it). The edge scan is id's `R_ScanEdges` and stays one pass.

**`timedemo demo1`** (every message a frame; 1920×1080, `--video modern --display 16:9`,
median of five interleaved rounds, frames per second):

| threads | 1 | 2 | 4 | 8 | 16 |
|---|---:|---:|---:|---:|---:|
| native, before | 199 | 265 | 381 | 494 | 489 |
| native, after | 199 | 270 | 421 | 567 | 554 |

In the page (the threads build on this round's deploy, headless, a 1886×996 2026 frame at
pixel size 1, `r_threads N` then `timedemo demo1`, median of three rounds): Chromium 164
→ 164 fps on one thread, 296 → 339 on 4, 337 → 390 on 8, 323 → 383 on 16; Firefox 139 →
141, 242 → 279, 280 → 318, 271 → 314.

## 14. One round of threads a frame, and less on one thread (2026-10-03, branch `fleet/opt-serial`)

An Android phone draws a native 2640×1080 frame in 6–7 ms back to
back and 17–18 ms in play at 60 Hz: its cores sleep between frames, every round of
threads wakes them, the governor caps the fast cores, and what a frame does on one
thread runs on a core at about 1.2 GHz. So this round counted what a frame of the
page's 2026 profile does that is not pixels (`quaketool framerate --serial`: exact
perspective, the status bar overlay, the scaled 2-D layer, natively) and went after it.

**What it found** (e1m3's flames at 2631×1071, 8 threads, main): of a 3.7 ms frame the
bands were 1.7; the rest ran on the calling thread — the edge scan 0.58, the bakes'
own round 0.36, the game 0.24, the 2-D layer 0.27, the world walk 0.17, the surface
lookups 0.14, the entities' setup 0.11, a z-buffer fill 0.10 — and the frame started
three to six rounds of threads: with the overlay a frame is three views (four where
the view does not stand on the bar: 1315×535), each with a round for its bands and,
in a torch-lit room, one for its bakes.

**What changed** (all the same pixels: "Proof"):

- **One round a frame.** The bakes have no round of their own: each thread of the
  bands' round takes bakes first, the largest first, until none is left, then bands
  (`surf::Bakes`; a span reads a block the frame bakes through it). And a frame's views
  share the round (`Renderer::render_into_with`, `band::Target`): each is prepared in
  turn on the calling thread, then the frame's rows are cut into strips wherever a view
  begins or ends, each strip holding a band of every view lying in it, all taken from
  one queue. Three to eight rounds a frame became one. (`client::draw_view` is the one
  call; demo playback draws through it as live play does.)
- **The spans in row order.** The scan's spans stay as it makes them, row after row,
  each naming its surface; a band draws one run of them. Before, each of the 32 bands
  walked every surface's whole span list to find its rows.
- **The scan in one walk.** id walks the active edges three times a scanline
  (`R_GenerateSpans`, `R_RemoveEdges`, `R_StepActiveU`); each edge is now removed or
  stepped as soon as its spans are generated. id's walks stay in the tests as the
  reference (726,000 spans, the same in the same order).
- **The z-buffer keeps its size** (the overlay's small windows made every frame refill
  megabytes of it); **a blown-up pic's repeated rows are copies** (the status bar at
  the scaled 2-D layer's 5 drew every row texel by texel); **a band skips an alias
  model it does not reach** (every band asked each of its triangles); **the surface
  bake is a routine per mip level**, as id's four (no division a row, no bounds test
  a texel: a third less time a texel); the world walk tests a node's visframe before
  it reads its box.

**Proof.** Main's `quaketool` against the branch's, 355 frame hashes (a script of
`quaketool` runs kept with the round's scratch): the goldens on 1 and 8 threads; `play`'s hashes of
demo1–3 and three walks in Classic and 2026 video on 1 and 8 threads; 168 `shot`s of
the live client — seven views (one under water, one firing) at 2631×1071, 1920×1080,
1315×535 and 640×400, with the overlay's corners and the scaled 2-D layer, at exact
perspective and at 16, without the bar, and Classic — and 162 `view`s (nine eyes,
three sizes, three video settings), each on 1 and 8 threads: identical after every
commit. Every frame of demo1–3 and four walks at 640×400 and 960×600 (`play
--hash-every 1`, 2026 video, 3 threads) hashes as main's. `classic_check` ALL PASS.
New tests: the views of a frame in one round against the view and then each window
(screen, z, surface cache, 1–8 threads); the strips' cut; `Bakes` on several threads;
the scan against id's three walks; the bake against the loop as first written; the
blit over opaque rows. 858 tests in `quake-rs`, 211 in `quake-wasm`; clippy clean, the
wasm targets too.

**Native** (`quaketool framerate <pak> --serial --res 2631x1071 --threads 1,4,8
--rates 60`: the live game standing at each view, the page's 2026 frame, the whole
client frame's median ms, main → branch, three interleaved rounds under the fleet's
measurement lock, frames back to back):

| view | 1 thread | 4 threads | 8 threads |
|---|---|---|---|
| e1m2's start | 7.62 → 8.16 (see below) | 3.97 → 3.32 | 3.23 → 2.35 |
| e1m3's flames | 9.26 → 8.19 | 4.58 → 3.63 | 3.67 → 2.37 |
| e1m1, firing rockets | 7.61 → 7.43 | 3.65 → 2.92 | 2.93 → 1.96 |

On eight threads a frame takes 27–35% less, on four 16–21%. What the calling thread
does alone (e1m3's flames, 8 threads, the counters' means): 1.37 ms with the bakes'
round → 0.83: the scan 0.57 → 0.45, the z-buffer fill 0.09 → 0, the lookups 0.12 →
0.09, the walk 0.155 → 0.145; the 2-D layer 0.26 → 0.13; the bands with the bakes
1.67 + 0.31 → 1.20. (e1m2's start on one thread is the CPU's two speeds, not the
code: the game's own time, the same code in both builds, read 0.11 or 0.145 ms from
run to run, and the frames of the fast runs are 7.55 and 7.62 against 7.52.)

`timedemo demo1` (one view, no overlay, the RGBA pack in the frame, back to back; three
interleaved rounds): natively at 2631×1071, 2026 video, 99.7 → 102.4 fps on one thread,
254 → 260 on four, 327 → 353 on eight. In the page (headless Chromium, the threads
build, `?2026` with the overlay, a 2538×828 frame at pixel size 1): 316 → 369 fps on
8 threads (3.17 → 2.71 ms a frame), 268 → 299 on 4, and 386 and 301 once demo playback
drew through `client::draw_view` too (it had kept a round a view).

**What one thread pays.** The 2026 frame above is level on one thread only because
its 2-D layer got 0.12 ms cheaper: the world itself draws slower there. The review
measured it against the branch's base, interleaved, under the lock (medians; the 3-D
view alone, warm): Classic at 2631×1071 2.10 → 2.26 ms on e1m3 and 1.89 → 2.10 on e1m1
(+8%, +11%), the 2026 view at that size 7.29 → 7.54 (+3%), Classic at 640×400
0.615 → 0.623 (+1%); `timedemo demo1` on one thread, Classic, 348 → 330 fps at
2631×1071 and 1283 → 1243 at 640×400 (−5%, −3%), 2026 native 111.8 → 111.0. Commit by
commit it is the spans in row order (a band's run in place of each surface's list: one
thread loses the surface-by-surface order, +11–12% on the Classic native views) and
the bake lookup in the span's setup (+4–7%); the one-walk scan and the shared round
give a little back. Every browser deploy and every multi-core run is faster; a run on
one thread is the price, and `WorldDraw::draw_band` is where to win it back.

**Not measured:** an Android phone. These are a desktop's cores awake; what a round of
threads costs there when the workers slept 10 ms, and what the calling thread's part
costs at 1.2 GHz, are the phone's to say (`web/phone.py`).

**What is left on one thread** (e1m3's flames at 2631×1071): the scan 0.45 ms, the
game before the renderer 0.20, the walk 0.15 (three views' walks), the 2-D layer 0.13,
the entities' setup 0.11, the lookups 0.09 (a lightmap is still built, or its luxels
copied, for a surface whose block the cache then has: `D_CacheSurface` asks the cache
first).

## 15. The cost of a pixel (2026-10-04, branch `fleet/opt-pixels`)

At an Android phone's native 2640×1080 a frame has four times the pixels of its 2×2 picture,
so the inner loops are four times the work. This round made the pixel loops cheaper
without changing a pixel: two people's work, measured again together on top of §14
(main `d40a672`, merged into the branch).

**What changed** (every commit keeps every pixel: "Proof"):

- **The span loops ask for each segment's end a segment ahead** (`1596242`,
  `raster::segments_ahead`, now `raster::SegmentEnds`). id's portable C (`d_scan.c`:
  `D_DrawSpans8`, `Turbulent8`) divides for a segment's end on reaching the segment, and
  its pixels wait for the quotient; asked for a segment early, the divide runs while the
  segment before is drawn. Spans 64 to 4 and the liquids. The largest single gain,
  and largest at the 2026 default, span 8. It is id's own x86 trick, found again in
  portable code: the assembly (`d_draw16.s`'s `D_DrawSpans16`, what 1996 players
  saw, and `d_draw.s`'s `D_DrawSpans8`) starts the FDIV for the next segment's end
  halfway through the segment it is drawing, "start FDIV for end of next segment in
  flight, so it can overlap", at the instruction id marked "this is what we've gone
  to all this trouble to overlap". The port asks a whole segment early and leaves
  the overlap to the processor. (The x86 build draws liquids with `Turbulent8`, C,
  so there id's pixels did wait.)
- **The z-buffer row vectorizes again** (`ad23c32`). §14's spans in row order bind
  each surface's `izistep` by reference, and the loop that writes `D_DrawZSpans`' 16-bit
  1/z for every world pixel read it again after every store (the compiler cannot
  prove the z row does not overlap it): scalar, an `i32.load` a pixel in wasm, no SSE
  natively. A local copy gives the vector loop back (wasm: `i32x4` lanes narrowed to
  `i16x8`, one `v128.store` per eight pixels; natively `paddd`/`packssdw`). This is
  also most of what §14 found one thread paying: Classic native at 2640×1080 is now
  21–25% faster than main (below).
- **The browser builds use wasm SIMD** (`7d1b7ab`, `quake-wasm/.cargo/config.toml`):
  no source asks for it; what LLVM vectorizes is the z row above. It leaves behind
  Chrome < 91, Firefox < 89 and Safari < 16.4 (March 2023); the page says so plainly
  (`web/PLATFORM.md`, "What the browser must give").
- **The exact span tests "inside the block" once** (`b236d27`): one unsigned compare for
  both coordinates, the four clamps only as the fallback.
- **The alias spans draw on their run of the row, the z test first** (`8eba516`):
  `D_PolysetDrawSpans8`'s loop on one slice of the row, the texel fetched only where
  the z test passes, as id's loop does.
- **The sky**: id's 256×128 sky read as an array, no bounds check (`fb10900`,
  `071f2b7`), and each 32-pixel segment's end (`D_Sky_uv_To_st`: a square root and
  divisions) asked for a segment ahead (`409de9f`).
- **The underwater warp** walks its tables with the row's pixels (`fbd5fff`): two of
  the four reads a pixel lose their index check.

**Proof.** Main's `quaketool` against the branch's: 378 `view`s (nine maps, 320×200 to
2640×1080, Classic and the 2026 video at spans 64, 32, 16, 8, 4 and exact, level, up at
the sky and down at the floors; on 1 and 8 threads), 162 `shot`s under water, slime and
lava (with and without the status bar), 126 `shot`s at monsters and items with the gun
(view sizes 50–120, shots fired), and 48 runs of `play` hashes (demo1–3 and three
walks, Classic and spans 16, 8 and exact, 1 and 8 threads): no difference. The wasm
build, SIMD and all, gives the native `play` hashes under V8. The goldens;
`classic_check` ALL PASS; both crates' tests (each loop against its reference: the C
spans and `D_DrawSpans16` in id's order over 2,000,000 random spans, the alias spans
against the pixel-by-pixel loop, the unchecked sky against the guarded one, the sky
walk against `D_DrawSkyScans8` in its own order).

**Each change's share** (the merged tip against the tip without that change, so each
is measured on top of all the others; demo1's timedemo at 2640×1080, the 2026 video,
the frame-time change at span 8 / 16 / exact; medians, interleaved, under the fleet's
measurement lock, pinned to the 8 physical cores. Native: `quaketool`, 5 runs. V8:
the same program as wasm under node, 5 runs. Page: headless Chromium, the threads
build, `timedemo demo1` at pixel size 1, 3 rounds):

| change | native, 1 thread | native, 8 threads | V8, 1 thread | page, 8 threads |
|---|---|---|---|---|
| segment ends ahead | −12 / −12 / +2% | −8 / −8 / 0% | −20 / −11 / 0% | −8 / −5 / 0% |
| z row vectorized | −10 / −11 / −6% | −8 / −8 / −5% | −11 / −12 / −7% | −5 / −5 / −3% |
| wasm SIMD | — | — | −8 / −9 / −5% | −4 / −3 / −2% |
| exact: one test | 0 / 0 / −9% | 0 / 0 / −6% | 0 / 0 / −8% | 0 / 0 / −3% |
| alias row runs | −3 / −4 / −2% | −2 / −3 / −2% | −5 / −5 / −3% | −2 / −1 / −1% |
| sky read, a sky view | −8 / −11 / −7% | 0 / −5 / −6% | −7 / −8 / −5% | not measured |
| sky ends ahead, a sky view | −2 / −1 / −2% | −4 / −3 / −2% | −6 / −6 / −3% | 0 (demo1) |
| warp, under water | −18 / −20 / −12% | −12 / −13 / −10% | −36 / −37 / −23% | not measured |

(The sky rows are a view half sky, e1m5 looking up; the warp row a view under e1m4's
lake; demo1 has little sky and no water. The `view`s are the renderer alone, 30 frames.)

**Main against the branch** (the whole of it, cheap exact aside):

| | 2640×1080: span 8 / 16 / exact | 1920×1080 |
|---|---|---|
| native, 1 thread | 6.89 → 5.02 / 6.31 → 4.60 / 9.40 → 7.69 ms | 5.29 → 3.91 / 4.88 → 3.60 / 7.14 → 5.83 |
| native, 8 threads | 1.76 → 1.54 / 1.66 → 1.47 / 2.13 → 1.95 | 1.42 → 1.26 / 1.35 → 1.22 / 1.73 → 1.56 |
| V8, 1 thread | 8.43 → 5.99 / 7.04 → 5.45 / 11.42 → 9.62 | 6.48 → 4.66 / 5.50 → 4.29 / 8.71 → 7.34 |
| page, 1 thread | 8.57 → 6.14 / 7.17 → 5.65 / 11.63 → 9.92 | not measured |
| page, 8 threads | 2.62 → 2.23 / 2.38 → 2.15 / 3.10 → 2.88 | 2.03 → 1.73 / 1.86 → 1.68 / 2.39 → 2.18 |

Classic, natively on one thread (`timedemo demo1`, id's 16 with the ends ahead):
2.414 ms against main's 3.234 at 2640×1080 (−25%), 0.730 against 0.858 at 640×400
(−15%); the e1m1 and e1m3 views −21 to −25% at 2640×1080. That is §14's one-thread
price paid back and more.

**The native build is a lottery of ±5–10%, and it is where the loops fall.** With
quake-rs's release profile (fat LTO, 16 codegen units), reverting the sky read or the
warp — code demo1 barely or never runs — made demo1 9–10% slower on one thread. The
timedemo's phases (`--profile 1`) say where: the RGBA pack took 0.73 ms a frame in one
build and 1.18 in the other, the same instructions at other addresses, and the 3-D
phase the same. Built with `-C llvm-args=-align-loops=64` (every loop starting a
64-byte line), both packs take 0.73 and the 3-D phase does not move. With one codegen
unit the pack holds and the draw loops move instead (e1m6's walls: every variant 5–10%
faster than that build of the tip). So the native numbers in the table above are the
ones that held, within a few points, in both profiles; a native difference under ~10%
between two builds is evidence only if it does. The wasm builds showed none of it (the
unrelated reverts: 0.0 ± 0.5%). Aligning the loops would be a flag in a
`quake-rs/.cargo/config.toml` and 5% on the binary; two builds are a reason to try it,
not to adopt it.

**codegen-units = 1 for quake-rs: no.** Against the shipped 16, one unit is slower
natively: demo1 at 2640×1080 +8 / +6 / +5% on one thread (span 8 / 16 / exact), +3 /
+2 / +2% on eight, the e1m6 walls view +16% on one thread; 1920×1080 +5–7% and +2–3%.
(The page's crate already builds with one unit; this is `quaketool` only.)

**Not measured:** a phone (none connected this round); Safari (no local WebKit).

### Cheap exact: the divide every 16 pixels, the same pixels (`raster::span_exact_cached`)

Exact perspective divides at every pixel. Along a span a texel coordinate is a
hyperbola in the pixel, `65536 (sz + k dsz) / (zi + k dzi)`; through three of its points
16 pixels apart (the knots, by the divide) a parabola stays within a bound that follows
from the hyperbola's third derivative, `6 |sz dzi − dsz zi| dzi² z⁴ / 65536³`, times
0.0642 × 16³. So a span is drawn in 32-pixel segments: two divides a segment, asked for
a segment ahead; the parabola stepped by forward differences in fixed point (16.16 with
7 more bits), s and t side by side in one `u64`; and every pixel tested, one AND a
pixel, for being further from both its texel's edges than the bound plus the
arithmetic's own errors (the guard: `GUARD_SLACK` lists them). A pixel that is, reads
the divide's texel. The few that are not are drawn again by the divide, and of those
the ones the divide's own rounding could tip (within 2 units of an edge) by replaying
the reference's accumulators (`zi += dzi` …) up to them. A span of 32 pixels or fewer,
a grazing one (rounding noise over a quarter unit, a guard over 6000), a block wider
than 512 texels, or a segment that would leave the block, is drawn by the divide.

In demo1 at 2640×1080 (counted): 93% of the segments clear, 5% with a pixel near an
edge, 2% at the block's edge; 1.6% of the exact pixels in spans too short.

**Size:** about 210 lines of code and 130 of comments in `raster.rs` (the proof
among them), and 270 lines of tests.

**Proof.** A proof, in `raster.rs`'s comments (the review's, written down there): the
reference's rounding is at most half `exact_plan`'s `noise`, so its value is within
1.125 units of the true coordinate, and a knot's too; the knots' errors reach the
parabola times at most 1.25; the interpolation error is at most `es` (the third
derivative's bound, `z` largest at an outer knot); the fixed-point floors lose under
2.0 units by a segment's last pixel. So the reference is within `es + 4.54` units of
the parabola, under the guard `floor(es) + 9`: a pixel the guard test clears reads the
reference's texel, inside the block, and a pixel it does not is drawn by the divide,
where a 1-unit "near the edge" window would do (the code keeps 2). Tests: the fuzz
against the divide at every pixel (30,000 random spans by default, 2,000,000 pass with
`QUAKE_FUZZ_SPANS`); the proof's inequality checked at every pixel a parabola draws
(worst `|R - y/128| - es` 2.98 units over 2,000,000 spans, the interpolation 0.998 of
`es`, the rounding 0.457 of `noise`); and a steered fuzz that moves the worst pixel of
a segment to just inside its guard on the side that would tip it: the smallest margin
is 5.99 units, and with a guard slack of 3 instead of 9 it fails (at 4 it passes: the
worst case seen needs 4, the proof 6). The random fuzz cannot tell 3 from 9. Against
the branch without it: the view sweep at exact (nine maps, every size, 1 and 8 threads:
1,242 `view`s and `shot`s), the all-spans sweep (540) and `play` hashes at exact (16
runs), no difference; the wasm build's `play` hashes equal the native ones.

**Speed** (demo1, exact, the frame-time change; as in the tables above; measured on
`911ab32`. The same arithmetic with its parts named, `13a9fd7`, draws the renderer's
views at exact 1–3% faster natively and 0–2% in V8, while its native timedemo lost
0.45 ms a frame to the RGBA pack's alignment, the lottery above: against `a7ef153`
7.66 → 7.22 ms, where `911ab32` gave 6.87 in the same sitting):

| | 2640×1080 | 1920×1080 |
|---|---|---|
| native, 1 thread | 7.81 → 6.96 ms (−11%) | 5.92 → 5.42 (−8%) |
| native, 8 threads | 1.95 → 1.84 (−5%) | 1.54 → 1.48 (−4%) |
| V8, 1 thread | 9.74 → 8.26 (−15%) | −14% |
| page, 1 thread | 10.08 → 8.50 (−16%) | not measured |
| page, 8 threads | 2.84 → 2.60 (−9%) | −7% |

In the page spans 8 and 16 do not move (±0.5%). Natively the review measured span 8
3.7% slower with it (5.07 → 5.26 ms, in both of its holds): not the span loops, which it
does not touch, but where the code lands (the lottery above).

The renderer's own views at exact, 2640×1080, one thread: e1m6's walls −13% natively,
−22% in V8; e1m3 −3% / −9%; e1m4's lake −3% / −3%; e1m1's start 0% / −8%, and at
1920×1080 natively +8%. That one is the first prototype's "13% slower": e1m1's start is
an eye on the 16-unit grid looking straight along an axis, and there many pixel centres
fall exactly on texel edges. 19% of its segments have a near pixel (demo1: 5%), 87% of
those pixels need the replay, 1.2 million accumulator steps a frame (demo1: 84,000),
about 0.8 ms natively, where the divide it saves is cheap. The same view from 0.4 unit
and 0.3° away: −17% natively (−12% at 1920×1080), −24% in V8. Play rarely stands on
the grid; the demos do not. (The prototype also redrew the overlapping last segment's
near pixels; that segment is now drawn aside and only its new pixels kept: 4–10 points
faster in V8, a point slower natively. Why V8 gained that much I did not pin down.)

**Exact against the spans, with it** (demo1 at 2640×1080): in the page on one thread
8.50 ms against 6.23 at span 8 and 5.74 at 16 (1.36x and 1.48x), on 8 threads 2.60
against 2.21 and 2.14 (1.18x, 1.21x); in V8 on one thread 8.26 against 5.99 and 5.45
(1.38x, 1.52x); natively 6.96 against 5.02 and 4.60 (1.39x, 1.51x). Without it exact
was 1.3–1.8x span 16; with it 1.2–1.5x: better, still the dearest.
