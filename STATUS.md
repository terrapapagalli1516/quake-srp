# Quake-RS — working status / hand-off

Last updated 2026-06-11 (demo-parity branch). This file is the honest "where
things stand" note — read it before continuing.

---

## The game client moves into the engine (2026-09-25, branch `quake/client`)

Structure only; no behaviour changed. The part of quake-wasm that is id's
`cl_*.c`/`view.c`/client half of `host.c`/`host_cmd.c` is now
`quake-rs/src/client/` (`mod.rs` Walk/DemoPlay/Vid/ClientFrame/SoundCall,
`cl_main` walk_frame, `cl_demo` demo_frame + CL_PlayDemo_f, `cl_tent`,
`cl_input` KeyMove, `view` V_ParseDamage, `host` Host_FilterTime, `host_cmd`
level loads + cheats); ConNotify joined `quake_rs::console`, snd_dma.c's
channel choice and static-loop gates `quake_rs::snd`. A client frame takes a
`Vid` and returns a `ClientFrame { image, cshifts, sound }`: the sound calls
(S_StartSound batches, S_StopSound, S_StopAllSounds, S_StaticSound,
S_Update's listener + ambient leaf) are recorded in order instead of pushed
into quake-wasm's thread-locals; quake-wasm's `snd_dma::play` carries them
out. quake-wasm is the platform layer (exports, App, queues, localStorage,
extras); its `step_walk`/`step_demo` keep their signatures and its e2e tests.
Proof: `quaketool play pak0.pak demo1,walk_e1m1,walk_e1m3,fire_e1m1,quad_e1m1
--res 320x200,640x400` prints the browser's `bench.py --hash-every 30` table
byte for byte. Frame hashes, ABI, goldens, simbench/census, tests (moved
ones counted), clippy and the eight verify scripts unchanged.

---

## Projection and spans (2026-09-25, branch `quake/w2b`)

The 3-D view as DOS/Windows players saw it (details in AUDIT.md's section of
the same name): the pixel aspect in the projection (every preset is 16:10
and the page shows 4:3, so the world had been 1.2x too tall; now
`R_ViewChanged`'s `yscale = xscale * pixelAspect` everywhere); the x86
build's 16-pixel perspective spans (`D_DrawSpans16`, `Turbulent8`) over id's
spans, with brush entities cutting the world's; the sky centred on the
screen below viewsize 120. Exact per-pixel perspective is an opt-in extra,
`wasm_exactpersp 1`; all extras live in `quake-wasm/src/extras.rs`. Oracle
against id's x86 spans: 99.96 / 99.21 / 99.98 / 99.91 (320x200), 100.00 on
all four at the page's aspect at 640x400. Wasm world −16 to −23%; native
+30-40% (not understood; PERF_PLAN §6).

## The 2-D layer measured and matched (2026-09-25, branch `quake/fid2d`)

`uv run oracle/screen2d.py` diffs the status bar, menus, console, text and
overlays against id's composited screen (63 shots x 320x200/640x400/960x600;
`oracle/README.md`). 57 of 63 are now pixel-exact in each mode; the rest are
explained there. **Visible change:** WinQuake draws the 2-D layer at its own
pixel size in every mode, so at the browser's default 960x600 the bar is 320
wide at the bottom centre and the menus sit top centre. The old blown-up
layout is the opt-in **"scaled 2-D" extra**: the wasm export
`set_scaled_2d(1)` — not yet wired into the page's Extras menu or saved.
Found for other branches (AUDIT.md): menu cursors not remembered per menu,
`sv.time` accumulated in f32, WASD default binds, no pause/loading plaques,
`give` unlike `Host_Give_f`.

---

## Options menu + screen framing (2026-09-25, branch `quake/options`)

User-reported: Options cursor blinked too fast; Screen size seemed wrong.
Fixed faithful to the C (details + evidence in AUDIT.md's section of the
same name): 4 Hz realtime cursors (menu + console; `step(dt)` takes raw dt
and splits it like Host_FilterTime); Screen size is `viewsize` again
(30..120, `sizeup`/`sizedown`/`viewsize`, `-`/`=` binds) and the view is
framed by SCR_CalcRefdef/R_SetVrect — ABOVE the status bar, backtile border
below 100, sbar/inventory by sb_lines; resolution only in Video Options
(localStorage persistence unchanged); the gun at V_CalcRefdef's origin with
the alias clip plane; menus fade the frame and print bronze (M_Print); Reset
to defaults = default.cfg only. Goldens unchanged.

---

## Demo playback parity (2026-06-11, branch `ship/demo-parity`)

User (twice): the attract demo must match the real game — sound, status bar,
everything. In the C the demo IS the game client rendering a recorded stream;
this port's demo path decoded the stream but discarded most of it. Closed in
4 commits (`6b4417c`/`bd806d1`/`e58997a`/`8306aed`):

- **Decode side** (`quake-rs/src/demo.rs`, vs cl_parse.c/view.c): svc_sound per
  CL_ParseStartSoundPacket (SND_VOLUME default 255, atten byte/64 default 1.0,
  ent=ch>>3 / chan=ch&7, precache-resolved sample — id's demo1 carried **595
  recorded one-shots that were silently discarded**); svc_stopsound;
  svc_lightstyle into a per-demo copy-on-write `cl_lightstyle` table;
  svc_clientdata per CL_ParseClientdata's EXACT bit order (viewheight/
  idealpitch, punch char + velocity char*16 interleaved, items, ONGROUND/
  INWATER, weaponframe/armor/weapon + the fixed health/ammo/active-weapon
  trailer); svc_damage (V_ParseDamage); svc_print/centerprint; svc_stufftext
  consumed-inert with the C's would-exec documented.
- **Frame emission gates on signon completion** (the first entity fast-update
  is "the final signon stage", cl_parse.c:340) — the ~1.2 s of void-camera
  frames at demo start AND loop wrap are gone at the source; the wrap also
  resets the damage/kick/notify/oldz POV state.
- **Playback side** (`quake-wasm` step_demo): recorded one-shots +
  CL_ParseTEnt impact sounds queue through the SAME `queue_sounds` spatialized
  path as live (listener = recorded camera); recorded-stats `render::Hud`
  sbar; SU_WEAPON/SU_WEAPONFRAME weapon viewmodel with the R_DrawViewModel
  hide gates; recorded lightstyle flicker via the shared
  `server::lightstyle_scales_at` (literal R_AnimateLight math, proven
  byte-identical to the live path); V_ParseDamage flash + directional kick
  (v_kickroll/v_kickpitch 0.6, v_kicktime 0.5) decayed in V_CalcViewRoll;
  notify/centerprint overlays (Con_Print '\n' accumulation, same gating as
  live); full V_CalcRefdef camera (V_CalcBob from recorded SU_VELOCITY, oldz
  stair smoothing on recorded SU_ONGROUND, strafe lean, dead-view roll=80
  assignment semantics, punchangle added LAST); underwater D_WarpScreen +
  content/damage/powerup blends deferred to the dispatcher like live.
- **Sound-channel override CLOSED for live + demo** (was a known-deferred
  LOW): the page-side (entity,channel) playing-source registry in
  `web/index.html` implements SND_PickChannel's cross-frame "always override
  sound from same entity" + S_StopSound, fed by new `sound_entity`/
  `sound_channel`/`poll_stop_sound` exports. id's demo1/2/3 send **zero**
  svc_stopsound (engine-asserted census) — the stop path is protocol
  completeness; the override fix is live behaviour.
- **Evidence:** `web/verify_demo.py` (new permanent headless-Chromium harness,
  9/9: first rendered frame in-world, one-shots actually play via the page's
  `__sndStats` counter, sbar band drawn + lit, stop/override drain clean, zero
  console errors) + verify_walk/verify_ambient green on the same assembly;
  **458 lib + 48 wasm tests** (real-demo1 decode census, zero-stopsound
  census, loop-wrap seam, damage math, sbar A/B pixel diff); clippy 0/0;
  goldens byte-identical `fb14bd65`/`a6f98d8a`/`0211e6d4` (scene renders no
  demos).

---

## ⭐ SHIP PUSH (2026-06-10) — six features landed in one coordinated push

A 6-branch parallel implementation (each adversarially reviewed by 2–3
independent lenses, blocking findings fixed on-branch, then merged serially
with tests + goldens verified after every merge). **449 lib + 42 wasm tests
pass; goldens byte-identical throughout (`fb14bd65`/`a6f98d8a`/`0211e6d4`).**

1. **Death→respawn CLOSED and PROVEN** (was the one gameplay-breaking gap).
   The full QuakeC chain runs on the real progs.dat: T_Damage → Killed →
   PlayerDie (deadflag=DYING, movetype=TOSS) → death-anim thinks while dead →
   DEAD_DEAD → PlayerDeathThink (DEAD_RESPAWNABLE on button release) → +attack
   → respawn() → localcmd("restart") → try_restart reloads with level-ENTRY
   parms. e2e wasm tests cover a self-rocket kill AND an environment (slime)
   kill. Two fidelity fixes en route: console `kill` was a health hack → now
   ports Host_Kill_f via `Server::client_kill` (runs QC ClientKill); and
   SV_Physics_Client's TOSS/BOUNCE arm was missing (dead corpse froze mid-air)
   → routes to physics_toss per sv_phys.c.
2. **Intermission + finale screens** (QC-driven, faithful svc flow). Root
   causes: WriteByte/WriteString builtins dropped every MSG_ALL write (engine
   never saw svc_intermission/svc_finale), AND the port never did
   SV_SpawnServer's world-edict setup — `world.model`/`mapname` were empty so
   QC's episode-end check could never match (new `Server::set_map_name`).
   V_CalcIntermissionRefdef camera, Sbar_IntermissionOverlay (complete/inter
   plaques + big numbers: Time min:sec, Secrets, Kills), e1m7 finale text at
   8 chars/sec + CONGRATULATIONS plaque, svc_sellscreen → Help menu. Demo path
   carries cl.stats + intermission state. Review fix: completed_time latches
   the QC `time` global (epoch 1.0, = cl.time) not w.clock (epoch 0) — the
   displayed Time was 1s low; co-fixed the death-scoreboard clock.
3. **Lightning/beam temp entities render** (TE_LIGHTNING1/2/3 + TE_BEAM; was
   the deferred HIGH). New `quake-rs/src/tent.rs` ports cl_tent.c: CL_ParseBeam
   slot store (24 slots, same-entity replacement so a held thunderbolt is ONE
   refreshing beam), CL_UpdateTEnts expansion (bolt piece per 30 units, exact
   integer vectoangles, rand()%360 roll), view-entity re-anchoring. Live +
   demo paths. Bonus fix: PF_WriteEntity read its parm as float instead of an
   int global (would have collapsed all beams onto one slot).
4. **Ambient sounds (H11 — the LAST audit HIGH) closed.** PF_ambientsound →
   StaticSound registry (vol/atten byte-quantized exactly like
   svc_spawnstaticsound) + `Demo::static_sounds`; new `quake-rs/src/snd.rs`
   ports S_UpdateAmbientSounds (water1/wind2 only, like the C) + GetWavinfo
   (cue-chunk loop gate). Web: per-static looping sources through the same
   spatialGain law as one-shots; `sound_generation()` lifecycle stops every
   loop on boot/New Game/map/changelevel/restart/demo↔walk. Review fix: the
   ambient ramp runs the C's literal INTEGER master_vol math on a fixed 1/72s
   accumulator (Host_FilterTime's cap) — faithful asymmetric fade at any
   display refresh rate, no high-Hz stall. Also: ambience/* one-shots
   un-silenced (E1M6 wind tunnels), PF_ambientsound precache gate per the C.
   Ground truth: e1m1 = 14-loop base soundscape (fl_hum1/comp1 — no torches),
   e1m2 = 24 fire1.wav torches.
5. **First-frames texture/lighting "pop" root-caused and fixed** — caveat 5's
   cold-cache guess was WRONG (the surf cache bakes synchronously; can't pop).
   Two real causes: (a) frame 0 rendered with ZERO physics after spawn, so the
   player's ~5u spawn-settle fall played on camera — `Server::run_signon_frames`
   ports the C's two signon SV_Physics ticks (0.1s each), called by every wasm
   walk-building path (boot/New Game/map/changelevel/restart; quaketool CLI
   deliberately unchanged); (b) `any_dlight_reaches` was plane-distance-only,
   so ANY dlight anywhere kicked all near-coplanar in-view faces off the baked
   surface cache onto the per-pixel path (13.3% whole-view shimmer ~0.55s
   after New Game from a distant lavaball trail). Headless before/after:
   17.8% + 13.3% pops → max 0.86% (sky scroll + flicker only).
6. **R_MarkLights BSP dlight gating** (deferred-twice MED): per-face u32
   dlightbits via faithful sphere-vs-plane node recursion (world: headnode[0];
   inline submodels: own headnode, entity-local origins per
   R_DrawBEntitiesOnList). A/B on e1m1 with an injected light: 90,525 affected
   px → 10,411 (80,114 bleed px removed, 0 added — strict subset). Composed
   at merge with (5): mask (C-faithful "may contribute") → plane test → luxel-
   extent test (port-specific CACHE-PATH tightening only — the C keys rebuilds
   on marking alone but always renders through its surface cache; we'd flip
   baked→per-pixel and shimmer for zero pixel change). New debug knob:
   `QUAKE_DLIGHT=x,y,z,r|eye[:r]` on quaketool scene injects a light for A/B.

**Also landed the same session:** an idiomatic-Rust pass (cargo clippy
--all-targets = **0 warnings in both crates**, proven byte-identical: goldens
+ simbench VM-stmt counts unchanged), the **HTML shell redesign** (product
presentation, streaming download progress, structured help, favicon,
touch-screen notice, audio-state button — single dependency-free file), the
**deployed `web/quake_wasm.wasm` rebuilt at HEAD**, and a 5-lens final
whole-diff review (integration / faithfulness / regression+determinism /
docs / real-browser playtest — the playtest proved all six features live in
headless Chromium with zero console errors, including flying to e1m1's real
exit trigger for a genuine intermission). Verdicts: ship. Remaining LOWs
recorded below.

---

## TL;DR (pre-push state, 2026-05-31)

- The shareware episode (E1) is playable in the browser; **401 lib + 25 wasm tests pass**.
- **⭐ THE render bottleneck is FOUND AND FIXED.** The prior session's "fixed ~60 ms
  per-face cost, NOT per-pixel" lead was exactly right. Root cause: the lit-surface
  cache was a single thread-local slot **shared between the world pass and the external
  brush-model pass** (each `b_*.bsp` item box is a distinct `Bsp` with its own
  fingerprint + face count), so every external box reset the 5000-entry world cache and
  the world **re-baked every face every frame**. Fixed by making external models
  **bypass** the surface cache (they re-clone their `Bsp` every frame, so they could never
  hit it) and keeping a **single world-model slot** for the world + its inline submodels
  (commit `30c6866` + adversarial-review follow-up). Result on an **idle** host, e1m1:

  | res | before | after | speedup |
  |-----|--------|-------|---------|
  | 64×48 | 65.3 ms | **1.14 ms** | 57× |
  | 320×200 | 65.2 ms | **2.28 ms** | 29× |
  | 640×400 | ~68 ms | **4.97 ms** | ~14× |
  | 1920×1080 | 89.5 ms (11 fps) | **27.97 ms (36 fps)** | **3.2×** |

  **Byte-identical** output (e1m1/e1m2/e1m3 golden sha256 unchanged at
  `fb14bd65`/`a6f98d8a`/`0211e6d4`). The frame now scales with resolution (per-pixel
  bound) as a software renderer should. A regression test
  (`external_models_bypass_and_dont_evict_world_surf_cache`, which renders 26 distinct
  external models — past where the first-attempt LRU broke) guards it.
- **New benchmarks** cover both halves of the engine: the render `QUAKE_BENCH` harness
  gained world-pass **sub-phase timers** (pvs/sort/setup/lightmap/surf + cache hit/rebake
  counters — these localised the bug), and a new **`quaketool simbench`** measures the
  **game-logic** tick (physics/VM/AI/collision) with VM-statement + BSP-trace counts
  (commit `0591b70`).
- The deployed `web/quake_wasm.wasm` is **rebuilt + current** (headless-verified booting
  e1m1, no errors).

---

## Resolution: higher default + it now PERSISTS (2026-06-09)

User report: picking a resolution in Options and then starting the game reverted it
to the default. Root cause: `boot()` / `boot_demo()` / `boot_attract()` and the
**New Game** branch of `menu_select()` each force-called
`set_render_size(DEFAULT_W, DEFAULT_H)` (and `Menu::new()`), throwing away the
menu-picked size every mode transition. (The framebuffer is the documented source of
truth and `step()` already syncs the menu label to it each frame — so the *only*
thing reverting the size was those four explicit resets.)

**Fix:**
- Those four resets now **preserve** the live framebuffer and instead call
  `a.menu.sync_resolution(render_w, render_h)` so the fresh menu's "Screen size"
  preset tracks the preserved size. `set_resolution` (export) also syncs the label
  now, so a programmatic set can't leave it stale.
- **Default bumped 320×200 → 960×600** (`DEFAULT_W/DEFAULT_H`; must stay a
  `RESOLUTION_PRESETS` member so the label can sync). Per the user's pick.
- `RESOLUTION_PRESETS` extended to a consistent 16:10 ladder up to the **1280×800**
  clamp cap: `320×200 … 960×600, 1120×700, 1280×800` (7 presets).
- **Cross-reload persistence** (`web/index.html`): the chosen size is saved to
  `localStorage['quake-rs.resolution']` as `"WxH"` in `syncCanvasSize()` (the single
  choke point for size changes) and restored after `boot_attract()` via
  `restoreResolution()` (hands the raw value to `set_resolution`, which clamps — a
  stale/garbage value can never break boot).
- The `map <name>` console path (`run_map_command`) already preserved the framebuffer;
  it now also eagerly `sync_resolution`s the fresh menu's label (was the one
  `Menu::new()` site relying on `step()`'s per-frame sync — surfaced by an adversarial
  multi-lens review of the diff; the other 3 lenses found nothing).

**Verified:** 401 lib + 26 wasm tests pass (added
`chosen_resolution_persists_across_reboot`; updated the preset-cycle + clamp tests).
End-to-end headless Chromium check (in a work directory, `
verify_resolution.py`, 9/9 pass): fresh boot = 960×600, persists to localStorage,
reload restores a picked 640×400, **walk/boot + New Game both preserve it** (the bug),
oversized saved value clamps to 1280×800, garbage falls back to default, no console
errors. `web/quake_wasm.wasm` rebuilt + deployed.

---

## ⭐ PERFORMANCE — render bottleneck RESOLVED (was the "key lead")

The prior session's lead was correct: **a FIXED per-face cost independent of resolution**
dominated the frame. With the host **idle** (load 0.4/16 — the prior session's "throttling"
was likely partly this same fixed cost making all resolutions look uniformly slow), a
resolution sweep + new world-pass **sub-phase timers** pinned it exactly:

```
e1m1 @ 64×48, BEFORE:  world 61.1 ms  (surf phase 59.4 ms!)  — 0 cache true-hits, 942 REBAKES/frame
e1m1 @ 64×48, AFTER:   world  0.76 ms (surf phase  0.03 ms)  — 942 true-hits, 0 rebakes
```

**Root cause:** `face_surf_block`'s `SURF_CACHE` was a single thread-local slot keyed by
one `WorldFingerprint`. It is shared by the world pass **and** the external brush-model
pass — and every external `b_*.bsp` item box is a *separate* `Bsp` (distinct pointer
**and** `faces.len()=6` vs the world's `5516`). So each box `needs_reset` wiped the whole
world cache to a 6-entry one, the next frame's world pass wiped it back, and **every world
face re-baked its texture×lightmap×colormap block every frame** — the entire ~60 ms.
(The geom + lightmap caches were spared only because the world pass alone touches them.)

**Fix (`30c6866` + review follow-up):** A first attempt keyed a small per-model LRU set by
fingerprint; an adversarial review then found that the game **re-clones each external item
box's `Bsp` every frame**, so externals get a fresh fingerprint every frame and instance —
they can *never* hit the cache and, 24+ at once, would still evict the world cache (the
regression returns). The shipped fix: external models **bypass** the surface cache (bake
fresh each frame — a 6-face box is ~free), and the world + its inline submodels (which
share the one world `Bsp`) use a **single slot** that self-invalidates on a changelevel.
So exactly one model is ever cached and it can never be evicted. Byte-identical (goldens
unchanged). Regression test: `external_models_bypass_and_dont_evict_world_surf_cache`. The
bench's surf line now splits `cached-rebakes` (should be ~0 warm) from `external-bypass-bakes`
(expected, one per visible item-box face).

## Performance — current scorecard (idle host, e1m1)

| res | before | after | speedup | phase split (after) |
|-----|--------|-------|---------|---------------------|
| 64×48 | 65.3 ms | **1.14 ms** | 57× | world 0.76 (sort .15, setup .46, surf .03) |
| 320×200 | 65.2 ms | **2.28 ms** | 29× | world 1.65 |
| 640×400 | ~68 ms | **4.97 ms** | ~14× | world 3.90 |
| 1920×1080 | 89.5 ms (11 fps) | **27.97 ms (36 fps)** | **3.2×** | world 23.6, submodel 2.1, alias 1.8 |

The 2× target is **beaten** (3.2× @1080p, much more at low res). The frame is now genuinely
**per-pixel bound** — the remaining ~23.6 ms @1080p world is real shading work (2.5M px ×
texel-read + lightmap + palette/colormap + z-test), and overdraw is ~1.0× (culling optimal).

### ⭐ Clone/alloc hunt (byte-identical wins) — the SIM nearly DOUBLED

An adversarial per-frame/per-tick clone+alloc hunt (15 confirmed findings) landed the
following byte-identical removals (render goldens unchanged; sim VM-stmt/trace/think counts
IDENTICAL run-to-run, proving behaviour is unchanged):

| sim ms/frame | before | after |
|--------------|--------|-------|
| e1m1 | 1.42 | **0.82** (1.7×) |
| e1m3 | 5.00 | **2.44** (2.05×) |
| start | 0.44 | **0.29** |

- **🔥 `build_hull` rebuilt the ENTIRE clip-node table on every trace** (`world.rs`) — thousands
  of entries, hundreds of times per tick. Now cached per-world (`HULL_CACHE`, fingerprint-keyed
  like the render caches); `Hull.clipnodes` is an `Rc<Vec<ClipNode>>`. This is the bulk of the
  sim win. (`commit 321eff7`)
- `FaceGeom.poly` → `Rc<Vec<Vec3>>` (was deep-cloned ~2× per face/frame; ~5% at browser res).
- `clip_poly_near` writes a reused scratch buffer (no per-face `Vec`); VM `EqS/NeS/NotS` compare
  borrowed `&str` (no per-op `String`); `sv_move` borrows the `*N` model name; `fly_move` uses a
  fixed `[Vec3;5]`. (`commit 2216cc2`)

Render @ browser res now: 320×200 **2.81 ms (355 fps)**, 640×400 **5.49 ms (182 fps)**,
1280×800 (the wasm cap) **15.5 ms (65 fps)**.

### Next perf ideas (now genuinely optional — diminishing returns)
- **Remaining LOW clone-hunt findings (deferred, marginal):** lightmap luxel `Vec<f32>` clone on
  cache hit → `Rc` (only ~0.04 ms — needs a `Luxels` enum variant); `compute_visible_faces`
  allocates two `Vec<bool>` per frame (cache by view-leaf); `touch_triggers`/`draw_submodel`
  `local_dlights` scratch Vecs; pre-`with_capacity` the per-frame scratch Vecs. All byte-identical
  but small.
- **Inline submodels rebuild their lightmap every frame (no cache)** — `draw_submodel`
  calls `face_lightmap_dyn` directly. Routing inline (cache_surf=true) Normal faces through
  `face_lightmap_world_cached` would cache them. ~1 ms of the 2.1 ms submodel phase. Deferred.
- **wasm/native SIMD (`simd128`)** — the per-pixel inner loop (palette/colormap byte reads,
  z-test) could process 4–8 px/instruction for a further ~2× on the *remaining* per-pixel
  cost. Substantial, genuinely-different work; only worth it if 36 fps @1080p isn't enough.
- **16-pixel affine spans** (Quake's `D_DrawSpans`) — perspective divide every 16 px;
  introduces sub-pixel drift (a fidelity trade).
- **Mip selection** — large distant surfaces over the per-face cache cap stay on the
  per-pixel path; mip-LOD would let them use the cache and shrink block memory.

### Profiler / benchmarks
- **Browser (2026-09-25): `uv run web/bench.py --build --native`** — the real page in headless
  Chromium, deterministic workloads at dt=1/72, per-phase median/p95 for wasm and its native twin
  (see its docstring). The measured baseline and the ranked optimisation plan are in `PERF_PLAN.md`.
- **Render:** `render.rs` has an opt-in `RenderStats` (per-phase ns timers + face/tri/
  pixel/cache counters), zero-cost when off, surfaced by `QUAKE_BENCH=<iters>
  QUAKE_RES=WxH quaketool scene <pak> <map> <out>`. It now also reports **world-pass
  sub-phases** (`pvs/sort/setup+raster/lightmap/surf`) and **surf-cache true-hits vs
  rebakes** — wrap any new per-face work in these to keep the cost honest.
- **Sim:** `quaketool simbench <pak> <map> [frames]` benchmarks the game-logic tick (no
  rendering): per-frame ms + VM statements + BSP traces + thinks. Deterministic
  (same counts run-to-run). After the hull-cache fix: e1m1 ≈ 0.82 ms/frame; e1m3 ≈ 2.44 ms/frame
  (522 traces/frame — still the dense-collision map, now ~2× faster).

---

## ⚠️ Caveats / debt

1. **Golden baseline is `fb14bd65` / `a6f98d8a` / `0211e6d4`** (e1m1/e1m2/e1m3, post
   submodel cache). The submodel surface cache (`d2fc0d6`) was re-verified
   **byte-identical** to its parent (0 px diff on e1m1), so its "byte-identical"
   message is correct. (Note: each *world*-cache texel-baking step earlier in the
   history legitimately re-baselined the golden; that's expected and faithful —
   texel-resolution lighting is what Quake's software renderer actually does.)
2. **`ae3ba68` (linear-step perspective) really is NOT byte-identical** — it shifts
   ~57 px (sub-pixel edge ULP drift); corrected by `3fce66c`. That one stands.
3. **Benchmark absolute numbers swing with host load.** This session's numbers were
   taken on an **idle** host (load ~0.4/16) and are trustworthy; the prior session's
   "throttling" was likely partly the fixed surf-rebake cost (now fixed) making every
   resolution look uniformly slow. Still: re-check `uptime` before trusting ms, and A/B
   in one sitting.
4. The `quaketool` bench's submodel profiler line label may lag the actual counters
   (cosmetic only; the numbers are right).
5. ~~**Texture "pop" on first frames**~~ — RESOLVED in the ship push (see top).
   The cold-cache guess was wrong; the real causes were the unsettled spawn
   rendering on camera + plane-only dlight gating. Lesson: the surf cache
   bakes synchronously on first visibility — it cannot pop by itself.

---

## ~~WIP — single-player respawn~~ — ✅ CLOSED AND PROVEN (ship push, see top)

The plumbing (`bi_localcmd` → `RESTART_REQUEST` → `take_pending_restart` →
`try_restart`) was sound; the e2e chain is now driven and asserted by wasm
tests (`real_death_chain_respawns_via_restart`,
`environment_slime_kill_enters_the_same_death_chain`) with a per-frame
deadflag trace. The two real gaps were the `kill` health-hack and the missing
client TOSS physics arm — both fixed (details in the ship-push section).

---

## Known-deferred items (LOW severity, documented in AUDIT.md)

All previous HIGH/MED deferred items (lightning beams, R_MarkLights gating,
intermission/finale, ambient sounds, texture pop) shipped in the 2026-06-10
push. What remains is the LOW tail, all reviewer-vetted as non-blocking:

- Demo explosion dlight (demo path emits no dynamic lights).
- quaketool CLI walk paths skip the signon settle (deliberate — keeps CLI
  artifacts byte-stable; unify later with a walk-golden re-baseline).
- Console `kill` drains only a pending restart, not a same-frame changelevel
  (unreachable conflict with vanilla progs).
- The C's CL_UpdateTEnts MAX_VISEDICTS half-cap and its outer-loop index
  clobber (UB in the C) are deliberately not modeled.
- ~~Sound-channel override only dedups within a frame~~ — ✅ CLOSED
  (demo-parity branch, 2026-06-11): the page-side (entity,channel) registry
  implements SND_PickChannel's cross-frame override + S_StopSound, live + demo.
- Per-ammo sbar nits; pain-frame face anim; assorted Round-2 LOW list items.
- From the final whole-diff review (all vetted non-blocking): the demo loop
  wrap keeps the ambient ramp warm (deliberate seamless loop; the C's restart
  re-ramps from 0); the intermission idle-sway phase uses w.clock (constant,
  invisible phase offset vs cl.time); ~~demo1 playback shows ~1.2 s of
  void-camera frames at start/loop-wrap~~ — ✅ CLOSED (demo-parity branch:
  frame emission now gates on signon completion, so the void frames are never
  emitted; loop wrap equally clean, seam-tested); submodel dlight marking uses
  entity-local light origins where the C used world-space (deliberate —
  consistent with the port's local per-luxel submodel lighting; arguably fixes
  a C quirk that mis-lights moved doors).

---

## How to work here (lessons from this session)

- **Build/test/bench are reliable; trust them.** The "bash is broken" scare this
  session was a self-inflicted misread from firing too many parallel tool calls at once
  and mismatching results — NOT a tool fault. Work **serially**: one Edit, verify, one
  Bash, read the result.
- After any renderer change, re-render the 3 golden maps and `ppmdiff.py` against the
  prior PPM (in a work directory) — don't claim "byte-identical"
  without measuring.
- Bench A/B in the same sitting (machine load swings results 2–3×).
- Commit only `quake-rs/` + `quake-wasm/` sources (+ this file / README / AUDIT). The
  repo root has unrelated untracked files (other projects) — never `git add` broadly.
- The deployed `web/quake_wasm.wasm` is gitignored; rebuild + `cp`, don't commit it.

---

## Quick commands

```bash
# tests
cd quake-rs && cargo test --lib            # 458 tests (data-free)
cd quake-wasm && cargo test                # 48 tests (real embedded pak)

# render a map to PPM (needs a real pak0.pak)
cargo run --release --bin quaketool -- scene <pak> maps/e1m1.bsp out.ppm

# benchmark + per-phase profile
QUAKE_BENCH=30 QUAKE_RES=1920x1080 \
  cargo run --release --bin quaketool -- scene <pak> maps/e1m1.bsp out.ppm

# wasm build + deploy
cd quake-wasm && cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quake_wasm.wasm ../web/
```
