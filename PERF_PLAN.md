# quake-rust — performance plan

2026-09-25, branch `quake/perf`. Measurement and a plan. The only code in this round is the
benchmark harness and its opt-in timers. Implementers: the code is about to be split into modules,
so everything below names **functions and mechanisms**, never line numbers.

**The rule still holds:** faithful to WinQuake by default. Where the port does **more work than
Quake did**, doing what Quake did is both faster and more faithful, so those items come first. A
change that is faster but not faithful can only ship as an opt-in extra.

---

## 1. Summary

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

- `+simd128`: no measurable change.
- `wasm-opt -O3`: −2 to −5% and identical output, but it needs binaryen in the build (§6).
- `opt-level`, `lto`, `codegen-units` and `panic=abort` are already optimal in `quake-wasm/Cargo.toml`.

The wins that do exist are code changes, so per the brief they stay in this plan. Three of them
(B1, B2, and caching the external boxes in C4) are byte-identical and mechanical.

---

## 2. The harness: how to measure

**Browser:**
```sh
uv run web/bench.py --build --native      # builds the `--features bench` wasm; runs everything below
uv run web/bench.py --build --workloads demo1 --res 1280x800 --profile  # plus a CDP CPU profile
uv run web/bench.py DIR --hash-every 60   # any index.html + wasm; framebuffer hashes for A/B identity
uv run web/bench.py --build --live 15 --vsync   # sample the page's OWN loop (pacing at 60 Hz)
```

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
| world visibility and overdraw | `r_edge.c` (`R_ScanEdges` / `R_GenerateSpans`): edge-sorted spans, each pixel drawn once, **no z test** | fan triangles; bounding-box barycentric raster (`raster_triangle_cached` / `_tex`); f32 z test and write on every pixel; centroid sort of all world faces every frame | 3.8–5.2× screen visits; ~25% of inside pixels are z-rejected |
| perspective | `D_DrawSpans16` (asm; `d_subdiv16` defaults to 1): one divide per 16 px, affine between | one divide per inside pixel | not the wasm bottleneck. The 16-px prototype gave no wasm gain (native −15%) |
| z-buffer | 16-bit 1/z (`d_pzbuffer`), written by `D_DrawZSpans`, only **tested** by entities, **never cleared** | f32 depth cleared every frame; the RGB image is also cleared every frame | 0.39 ms per frame at 1280×800 for allocation and clear |
| framebuffer and palette | 8-bit indices. Cshift and gamma are a **256-entry** palette operation (`V_UpdatePalette` ramps, then `VID_ShiftPalette`) | `[u8;3]` RGB per pixel; `apply_blend` does float math per pixel per channel; pack uses 4 `Vec::push` per pixel | blend 11.9 ms, pack 2.3 ms at 1280×800 |
| surface cache | `D_CacheSurface` at `D_MipLevelForScale`'s mip (4 levels; `basemip` 1, 0.4, 0.2). A fixed, LRU-rover cache of `SURFCACHE_SIZE_AT_320X200` (600 KB) + 3 B/px above 64,000 px. Lit surfaces rebuilt with `R_AddDynamicLights` | mip 0 only; one unbounded block per face; lit faces bypass the cache into `raster_triangle_tex`: bilinear lightmap + `colormap_row` on every **screen pixel** | the firing and explosion spikes above; lighting per pixel, not per texel |
| 3-D viewport | `R_SetVrect`: at viewsize 100, the vrect height is vid.height − `sb_lines` (48 of 200 lines). `R_ViewChanged`: `yscale = xscale · pixelAspect` (0.833 at 16:10) | full-height render with the sbar painted over the bottom 24%. Square-pixel projection, then **presented** at 4:3 (AUDIT H6) | 24% of 3-D pixels are thrown away. The horizon sits too low, and the world is stretched 1.2× vertically (see A4) |
| which entities are drawn | `SV_WriteEntitiesToClient` sends only entities touching `SV_FatPVS`; statics use efrags on visible leaves (`R_StoreEfrags`); `R_AliasCheckBBox` rejects by frustum | every edict with a model index, every frame, one triangle at a time | ~75% of alias triangles per frame belong to models with no on-screen vertex |
| alias raster | `R_AliasPreparePoints` (each vertex transformed once). `D_PolysetDraw`: affine, Gouraud light through `acolormap`, `lzi >= *lpz` test. Viewmodel shares the z-buffer with `ziscale × 3` (`R_AliasDrawModel`) | 3 transforms per triangle; flat Lambert as an RGB multiply (**not colormapped**); perspective-correct bbox raster. Viewmodel allocates and clears a **full-resolution local z-buffer every frame** | viewmodel 1.4–1.9 ms at 1280×800 |
| frame rate | `Host_FilterTime`: return early when less than 1/72 s has passed | `step(dt)` once per rAF at the display rate | 2× work at 144 Hz |
| external brush boxes (`b_*.bsp`) | surfaces go through the surface cache like any brush surface | `cache_surf = false`: re-baked every frame | 0.27–0.87 ms per frame, at any resolution |
| underwater warp | `D_WarpScreen`, 8-bit, into the view buffer | clones the whole RGB frame, plus 3 `Vec` allocations per call | 1.7 ms at 1280×800 |
| entity field access | direct `entvars_t` struct members | `ent_get_*(ent, "name")`: a `HashMap<String>` SipHash lookup per field per entity | about 3% of the frame on e1m3; most of sim |

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
  A `Span` is the place for 16-pixel subdivision. The gradients are not solved from a vertex
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

**A5. Mip levels in the surface cache (`D_MipLevelForScale`).** *(faithful)*

- **What it changes:** distant surfaces bake at 1/2, 1/4 or 1/8 of the texture's resolution. That
  gives Quake's filtered distant walls, where the port currently shows mip-0 shimmer; smaller
  blocks; and fewer texels per lightstyle rebake.
- **Gain:** modest for speed; the main benefits are memory and fidelity.
- **Mechanism:** `face_surf_block` keyed by (face, mip), with the C's `scale_for_mip` and
  `mipadjust`.
- **Risk:** medium. It re-baselines the goldens.

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

**C3. The viewmodel on the shared z-buffer with Quake's ×3 bias.** *(faithful)*

- **Mechanism:** use `ziscale × 3` from `R_AliasDrawModel`, in place of a per-frame
  full-resolution `local_z` (see B3). Replace the ad-hoc `OFS_*` placement with `V_CalcRefdef`'s
  view-entity origin.
- **Gain:** part of the viewmodel's 1.4–1.9 ms at 1280×800.
- **Output:** changes where the gun meets walls.

**C4. Cache the surfaces of external brush boxes.** *(byte-identical)*

- **Evidence:** 27 (e1m1) and 84 (e1m3) bypass bakes per frame, costing 0.27–0.87 ms regardless of
  resolution. In the wasm path, the box's `Bsp` lives in `Walk::bmodel_cache` and **is stable** (it
  is not re-cloned; only `quaketool scene` clones).
- **Mechanism:** a small per-model cache keyed by the model name or the `Bsp` pointer, alongside the
  world slot in `face_surf_block`.
- **Owner:** Agent A, because the change is inside `face_surf_block`'s bypass branch.

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
  hand-written SIMD, for example for A3's span loop or the B1/B2 pack.
- **16-pixel affine subdivision on its own**: no wasm gain on top of A1. Do it for fidelity, as
  part of A3, not for speed.
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

---

## 9. Open, or not verified

- **Real GPU browsers.** Everything was measured in headless Chromium with software compositing on
  a loaded 16-core x86. I did not measure Firefox, Safari, a GPU-backed canvas (where
  `putImageData` is an upload), phones, or a real 120/144 Hz display. D1's gain is argued from
  the code, not measured.
- **Prototypes are evidence, not implementations.** The A1, B1 and B2 prototypes ran in a scratch
  copy, with atomic counters present. A1's prototype takes its gradients from one triangle and was
  checked for cracks only by counting background pixels on two frames.
- **Estimates, not measurements:** the gains for A3 and C2, and C1's exact saving (I counted models
  with an on-screen vertex; I did not measure the PVS pass).
- **Fidelity issues found, not fixed** (they belong in `AUDIT.md`):
  - A4: vrect and pixelAspect.
  - C2: alias models are not colormapped, and the code comment claims otherwise.
  - B2: the cshift rounds where the C truncates. (Fixed on `quake/perf-b`.)
  - A2: dlit faces are lit per pixel.
  - C3: the viewmodel placement is ad hoc.
- **Not investigated:**
  - The surface cache's steady-state memory per map (A5).
  - Sound decode/playback cost in the page: the bench runs with audio locked, so the page's
    `drain*Sounds` do not play anything.
