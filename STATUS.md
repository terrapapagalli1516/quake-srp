# Quake-RS — working status / hand-off

Last updated end of the 2026-05-31 session. This file is the honest "where things
stand" note — read it before continuing, especially the **Caveats** and **WIP**.

---

## TL;DR

- The shareware episode (E1) is playable in the browser; 400 lib tests + 25 wasm tests pass.
- This session: **6 codebase-wide code-review rounds** (committed), **4 user-reported
  playtest bugs fixed**, a **sim-perf ~25× fix**, and a **render-perf pass** that took
  e1m1 @1080p from ~49.5 ms → ~33 ms/frame (**~1.5×** — see the perf note, this is
  **short of the 2× target**).
- The deployed `web/quake_wasm.wasm` is current as of the last commit.

---

## Performance — current state & honest scorecard

Warm-frame render cost @1920×1080 (release; varies with machine load — one run mid-session
read 91 ms purely from host load, settling back to ~33 ms when idle, so treat absolute
numbers as approximate and always compare A/B on the same machine state):

| stage | e1m1 @1080p | note |
|-------|-------------|------|
| start of this perf goal (surface-cache era) | ~49.5 ms | baseline |
| + front-to-back ordering (`fb4725d`) | ~39.8 ms | **byte-identical** |
| + linear-step perspective (`ae3ba68`) | ~38.0 ms | +57 px drift (not byte-identical) |
| + submodel surface cache (`d2fc0d6`) | ~33 ms | +1247 px drift |

So **~1.5× this session**, ~3.9× vs the original per-pixel rasteriser. **The 2× goal
(≤24.75 ms) was NOT reached.** Phase breakdown now: world ~22.6 ms, submodel ~7.7 ms,
alias ~1.8 ms. The world pass is the remaining lever and is pure per-pixel cost
(overdraw is ~1.0× after front-to-back, so culling is optimal).

### Next perf ideas (not yet done)
- **wasm SIMD (`simd128`)** — confirmed available in this toolchain
  (`rustc --print target-features --target wasm32-unknown-unknown` lists `simd128`).
  No `.cargo/config.toml` exists yet; the rasteriser inner loop (palette/colormap byte
  reads, the z-test) is a candidate. Untried.
- **16-pixel affine spans** — Quake's actual `D_DrawSpans` does the perspective divide
  every 16 px and lerps between; would cut the per-pixel `1/z` but introduces sub-pixel
  drift (a fidelity trade like linear-step).
- **Mip selection** — large distant surfaces over the per-face cache cap stay on the
  per-pixel path; mip-LOD would let them use the cache and shrink block memory.

### Profiler
`render.rs` has an opt-in `RenderStats` (per-phase ns timers + face/tri/pixel/cache
counters), zero-cost when off, surfaced by the `QUAKE_BENCH` harness in `quaketool`.

---

## ⚠️ Caveats / debt (please fix the record when convenient)

1. **`d2fc0d6` commit message is WRONG** — it claims the submodel surface cache is
   "BYTE-IDENTICAL on all three maps". It is **not**: it shifts ~1247 px (0.49%, avg
   Δ2) on e1m1 — the same texel-baked-vs-bilinear class as the world cache, faithful
   but not identical. The golden hashes legitimately moved to
   e1m1 `fb14bd65` / e1m2 `a6f98d8a` / e1m3 `0211e6d4`. (`ae3ba68` had the same
   mislabel for linear-step; corrected by `3fce66c`.)
2. **Golden baseline is now `fb14bd65` / `a6f98d8a` / `0211e6d4`** (post submodel
   cache). Earlier ledgers reference older hashes; that's expected — each faithful
   texel-baking step re-baselines.
3. The `quaketool` bench's submodel profiler line prints the older
   "… (no cache)" format without the hit/miss columns — a cosmetic format edit didn't
   make it into a commit. The numbers are correct; only the label lags.

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
