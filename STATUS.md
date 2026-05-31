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

Warm-frame render cost @1920×1080, measured on an IDLE machine earlier this session
(absolute ms is unreliable under load — see Caveat 3 — so these are the idle readings;
the *relative* steps are the trustworthy part):

| stage | e1m1 @1080p | note |
|-------|-------------|------|
| start of this perf goal (surface-cache era) | ~49.5 ms | baseline |
| + front-to-back ordering (`fb4725d`) | ~39.8 ms | byte-identical (0 px) |
| + linear-step perspective (`ae3ba68`) | ~38.0 ms | +57 px sub-pixel drift |
| + submodel surface cache (`d2fc0d6`) | ~35 ms | byte-identical (0 px) |

So **~1.4× this session**, ~3.9× vs the original per-pixel rasteriser. **The 2× goal
(≤24.75 ms) was NOT reached.** Phase split: world dominates (~25 ms), submodel ~8 ms,
alias ~1.8 ms. The world pass is the remaining lever and is pure per-pixel cost
(overdraw is ~1.0× after front-to-back, so culling is already optimal).

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

## ⚠️ Caveats / debt

1. **Golden baseline is `fb14bd65` / `a6f98d8a` / `0211e6d4`** (e1m1/e1m2/e1m3, post
   submodel cache). The submodel surface cache (`d2fc0d6`) was re-verified
   **byte-identical** to its parent (0 px diff on e1m1), so its "byte-identical"
   message is correct. (Note: each *world*-cache texel-baking step earlier in the
   history legitimately re-baselined the golden; that's expected and faithful —
   texel-resolution lighting is what Quake's software renderer actually does.)
2. **`ae3ba68` (linear-step perspective) really is NOT byte-identical** — it shifts
   ~57 px (sub-pixel edge ULP drift); corrected by `3fce66c`. That one stands.
3. **Benchmark absolute numbers are unreliable when the host is loaded.** During the
   wrap-up the CPU was throttled (~3–10× slower across ALL resolutions
   uniformly — a dead giveaway it's host load, not code). Re-measure on an idle
   machine before trusting any ms figure; compare A/B in one sitting.
4. The `quaketool` bench's submodel profiler line label may lag the actual counters
   (cosmetic only; the numbers are right).

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
