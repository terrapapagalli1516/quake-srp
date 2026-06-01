# Quake-RS — working status / hand-off

Last updated 2026-05-31 (late session). This file is the honest "where things
stand" note — read it before continuing.

---

## TL;DR

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
5. **Texture "pop" on first frames** (deferred item below): now has a likely cause — the
   surf cache is cold on frame 0 and warms over the first 1–2 frames as faces come into
   view. Worth re-checking now that the cache is per-model and actually persists.

---

## WIP — single-player respawn (`c5d3df9`, INCOMPLETE)

User reported: dying in-game doesn't respawn. Root cause found: `PF_localcmd` (#46) was
a no-op, so QuakeC's `localcmd("restart\n")` (the death→respawn path) was ignored.

**Done:** `bi_localcmd` now recognises `restart`/`changelevel`/`map`; added
`RESTART_REQUEST` + `Server::take_pending_restart` + host `try_restart()` (reloads the
current level with the level-entry spawn parms) + `Walk.entry_parms`. The
localcmd→flag→drain path is **unit-tested and passing**.

**NOT done / the gap:** the end-to-end death→respawn was never verified. An integration
harness that just set `health = 0` did **not** fire the restart, because that doesn't
drive the real QuakeC chain (`T_Damage` → `PlayerDie` → `deadflag = DEAD_RESPAWNABLE`
→ `PlayerDeathThink` → `respawn()` → `localcmd`). **Next step:** drive a real death
(apply damage through the proper path, or call the death-think functions) and confirm
`take_pending_restart()` returns true, then that the level reloads with inventory.

---

## Other known-deferred items (from the audit, documented in AUDIT.md)

- Lightning/beam temp entities (TE_LIGHTNING1/2/3, TE_BEAM) decode but don't render
  (shambler/Chthon bolts, thunderbolt). Plan in AUDIT.md.
- R_MarkLights BSP dlight gating (dynamic light can bleed through thin walls).
- Intermission/finale camera.
- A reported **one-time texture "pop"/shift** on the first frames — likely the surface
  cache populating (cold→warm); not yet root-caused or fixed.

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
cd quake-rs && cargo test --lib            # 400 tests

# render a map to PPM (needs a real pak0.pak)
cargo run --release --bin quaketool -- scene <pak> maps/e1m1.bsp out.ppm

# benchmark + per-phase profile
QUAKE_BENCH=30 QUAKE_RES=1920x1080 \
  cargo run --release --bin quaketool -- scene <pak> maps/e1m1.bsp out.ppm

# wasm build + deploy
cd quake-wasm && cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quake_wasm.wasm ../web/
```
