# Quake-RS — status and hand-off

Last updated 2026-09-25, after the overnight push (`quake/overnight` at `3ba835f`). The
first section is where things stand; the second is what the night changed; the rest is
the older history, kept as evidence, with superseded items marked.

---

## Where things stand

- **What it is.** The shareware episode plays in the browser and natively, single player,
  as id's WinQuake plays it: attract demos, New Game, E1M1–E1M8 with Chthon, death and
  respawn, intermission and finale, save and load, Options, the console. `README.md` says
  how to run it.
- **The rule** is faithful to WinQuake by default, Always Run the only intended default
  departure, everything else an opt-in Web extra (four exist: uncapped framerate, show FPS,
  exact perspective, scaled 2-D layer).
- **Two things for the user to decide:**
  1. Four control departures are still on by default (CENSUS.md, "Rule departures on by
     default"): mouse look held while the pointer is locked, WASD, `f` for fullscreen,
     Space swimming up faster. Keep them as recorded exceptions, or make them extras?
  2. The 72 fps cap is on by default, as in id's `Host_FilterTime`: a 144 Hz display runs
     at 72 fps, a 120 Hz one at 60 (every other refresh). It is faithful, and the chair
     kept it; "Uncapped framerate" in Web extras turns it off. Flagging it because it is
     the one faithful change most likely to feel like a regression.
- **Measured against id.** id's WinQuake renderer, built headless from the C (`oracle/`),
  matches the port on 99.91–99.98% of pixels in the four standard views against id's x86
  16-pixel spans, 100.00% at the page's 4:3 aspect, entity pixels 100%. The 2-D layer
  matches id's composited screen except three explained residues (`oracle/README.md`).
  The gameplay census found 18 HIGH/MED differences; all are fixed (`CENSUS.md`).
- **Speed.** In the browser (headless Chromium, wasm), id's demo1 at 1280x800 takes 4.5 ms
  a frame, from 22.6 ms at the start of the night (median; p95 33.1 → 5.0 ms). Details
  and caveats in `PERF_PLAN.md`.
- **Checks at `3ba835f`** (run for this document): `cargo test --release` passes in both
  crates, 581 + 1 + 8 in quake-rs and 118 in quake-wasm (1 ignored: the `oracle_screen`
  harness). Goldens (`quaketool scene`, sha256 prefix): e1m1 `4807aaa1`, e1m2 `9ae2b478`,
  e1m3 `c65b7046`. The eight `web/verify_*.py` scripts pass (walk, ambient 13/13, demo
  9/9, input 36/36, menu 61/61, save, loops 10/10, extras 40/40; headless Chromium,
  run on a scratch copy of the page).
- **Deployed.** `http://localhost:8196/index.html` serves the `3ba835f` build
  (`miniserve -C`, from a work directory).
- **In flight when this was written:** `quake/timedemo` (id's `timedemo`, `playdemo`,
  `stopdemo`, `startdemos`, `demos`, and `pause`) and a final review of `3ba835f`.
- **What is left:** `AUDIT.md`, "Open, as of 2026-09-25", one list. The largest items: no
  `pause` or loading plaque (pause is on `quake/timedemo`), no dynamic lights in demo
  playback, the control departures above, and nothing measured on a real GPU browser or a
  real high-refresh display.

---

## 2026-09-25: the overnight push

the user's brief: faithful by default (only Always Run departs; anything
else becomes an opt-in extra), three bugs they had noticed (Chthon has no electricity; the
Options cursor blinks too fast; Screen size does the wrong thing), performance, well
structured code. The chair split it into branches, one agent each, merged into
`quake/overnight` in order (`git log --first-parent 5af4fa1..3ba835f`; each merge message
summarises its branch). The chair's ledger is
a `PLAN.md` outside this repository.

**The three reported bugs.**
- *Chthon's lightning* (`ce2dbf8`): boss.qc writes `TE_LIGHTNING3` to `MSG_ALL`, and the
  port read temp entities only from the broadcast buffer, so the bolts were never drawn.
  The server now parses each message buffer the way the client would. An end-to-end test
  fights Chthon on e1m7 and kills him with the lightning.
- *The Options cursor* (`962c82b`): it blinked off a 10 Hz frame counter; id's is
  `(int)(realtime*4)&1`, 4 Hz. The console cursor was 2 Hz; also 4 now.
- *Screen size* (`1e9a299`): the row had been turned into a resolution picker. It is id's
  `viewsize` again (30–120, the view shrinks inside a tiled border, 110 drops the
  inventory, 120 the status bar), with the view rendered above the status bar as
  `SCR_CalcRefdef` does: before, the view filled the screen and the bar was pasted over
  its bottom 24%, so the horizon sat too low. Resolution moved to Video Options.
  AUDIT.md: "Options menu + screen framing".

**Instruments, so "faithful" is measured instead of argued.**
- *The oracle* (`oracle/`): id's WinQuake built from the C with null drivers renders the
  same view, clock and entities as the port; `compare.py` counts matching palette
  indices. `screen2d.py` does the same for the whole composited 2-D layer.
- *The census* (`CENSUS.md`): `quaketool census` plays all nine maps headless through the
  real QuakeC, and id's own server edicts are dumped and diffed against the port's. It
  found 4 HIGH, 14 MED and 25 LOW differences.
- *The benchmark* (`web/bench.py`): the real page in headless Chromium, fixed workloads,
  per-phase timers, a native twin; `PERF_PLAN.md` holds the baseline and the plan.

**The 3-D renderer, now id's.** In four steps, each measured by the oracle
(AUDIT.md sections of the same names): id's alias-model pipeline, raw-texel liquids and
sky, the sky's layer offset, the gun's placement ("Session 7"); id's mip levels and
integer lightmap stepping ("mip levels, lightmap stepping"); the pixel aspect of a 16:10
mode on a 4:3 screen — the world had been drawn 1.2x too tall — and the x86 build's
16-pixel perspective spans ("Projection and spans"); and finally id's edge-sorted span
renderer for the world and brush models, which replaced the port's own polygon walker
("World pass: id's edge renderer"). Outcome, world pixels matching id's x86 renderer at
320x200 in the four standard views (e1m1 / e1m2 / e1m3 / e1m7): **80.39 / 59.87 / 65.33 /
67.80% when the oracle first ran → 99.96 / 99.94 / 99.98 / 99.91%**, and 100.00% at the
page's aspect; entity pixels 0.0 / 14.6 / 18.6 / 8.5% → 100%.

**The 2-D layer, now id's** ("The 2-D layer against id's composited screen"). WinQuake
draws the status bar, menus and console 1:1 in every mode; the port blew up a 320x200
screen. Now 1:1 by default (the old look is the "Scaled 2-D layer" extra), with id's
sliding console, the DOS quit prompt, and a handful of pixel offsets. `screen2d.py` at
640x400: 1–55% of 2-D pixels matched before, 100% on every shot after except three
explained residues. To keep the 1:1 bar a sensible size, the page's canvas is now the
largest 4:3 box the window fits (976x732 at 1440x900, was a fixed 640x480).

**Gameplay** ("Census client/host fixes", "Census fixes, server side"). All 18 HIGH and
MED census findings are fixed: teleporters turn the view; single player pauses behind the
menu and console; e1m8 has its low gravity; a weapon switch pressed during a cooldown is
kept; two health boxes no longer fall out of e1m1 and e1m6; the gold pickup flash; the
player is named ("player was shot by a Grunt"); level-start doors open as in id; door and
lift sounds loop until they stop; runes show on the status bar; Tab shows the scores;
weapon keys work on any keyboard layout; damage flashes in god mode; new weapons flash;
demos get trails, skins and the demo1 → demo2 → demo3 cycle. Plus 16 of the 25 LOWs, and
two in part.

**Web extras** ("Web extras: the opt-in departures"). Options > Web extras, in the slot
id's Windows build uses for its 14th row: uncapped framerate, show FPS, exact
perspective, scaled 2-D layer. Off by default, persisted by the page, each a `wasm_*`
console variable. Also: `viewsize` persists across reloads, and Esc works in fullscreen
through the Keyboard Lock API (not verified in a real browser).

**Performance** (`PERF_PLAN.md`). Mostly by doing what Quake did, which also made it more
faithful: the 72 fps `Host_FilterTime` cap; the embedded pak as one static slice (memory
81 → 53 MB after boot, 171 → 62 MB after four map loads); the palette shift as
`V_UpdatePalette`'s 256-entry ramps (a Quad frame at 1280x800: 32.8 → 17.1 ms, before the
later world-pass gains); dynamically lit walls through the surface cache; mip levels;
id's edge renderer (wasm frame −23 to −33%, native −43 to −57%); only the entities the
server would send (e1m3's 98 alias models per frame → 3.8; it also stopped muzzle flashes
lighting walls from behind); entity fields resolved once per progs instead of hashed on
every access (the e1m3 game tick 2.64 → 0.29 ms). demo1 at 1280x800 in wasm: 22.6 → 4.5
ms a frame.

**Structure.** `render.rs` (15,000 lines), `server.rs` (9,900) and quake-wasm's `lib.rs`
(8,000) were split along id's own files, byte-identical, one agent per file. Then the game
client moved from quake-wasm into `quake_rs::client` (quake-wasm's code 6.3k → 3.0k lines),
so `quaketool play` runs the browser's client natively and prints the same frame hashes.
The polygon walker was deleted once the edge renderer won (−2.5k lines). The source grew
from 52,000 to 67,000 lines.

**Reviews.** Two adversarial reviews of the merged tree; their findings were fixed on
`quake/polish` ("Review fixes"), `quake/polish2` and `quake/polish3` ("Second review
fixes", both sides). The visible ones: loading a save kept no options; the underwater
view is rendered at id's 320x200 warp buffer; the client clock is `cl.time`; `sv.time` is
a double (the new-weapon flash was a frame early); particles are `D_DrawParticle`; every
menu keeps its own cursor; no weapon-icon flash at level start; a busy frame could leave
a door hum looping forever.

**Goldens.** They moved with each deliberate fidelity fix, every move recorded in
AUDIT.md with its pixel count: `fb14bd65` / `a6f98d8a` / `0211e6d4` at the start →
`4807aaa1` / `9ae2b478` / `c65b7046` at `3ba835f`.

**Tests.** 475 library + 8 integration and 64 quake-wasm tests at the start; 581 + 1 + 8
and 118 at `3ba835f`.

## How to work here

- **Measure, don't claim.** After any renderer change, re-render the three goldens
  (`quaketool scene <pak> maps/e1mN.bsp out.ppm`, sha256) and, for a fidelity change, run
  `uv run oracle/compare.py`; record every golden move in `AUDIT.md` with its pixel count.
  "Byte-identical" means the goldens and the `bench.py --hash-every` / `quaketool play
  --hash-every` frame hashes did not move.
- **Faithful first.** Read id's C for the thing you are changing (the WinQuake tree is at
  `quake-c/WinQuake`) and name the C
  function in the doc comment. A change that departs from id belongs in the Web extras.
- **Benchmarks:** A/B two builds in one sitting and note the load; absolute milliseconds
  swing 2–3x with what else is running.
- **The web scripts need playwright:** `uv run --with playwright web/<script>.py` (their
  shebang carries `--with playwright`; plain `uv run web/bench.py` fails). Parallel runs
  take `QUAKE_VERIFY_PORT` so they don't collide.
- **Commits:** only this project's files — the repo root has unrelated files; never
  `git add` broadly. The built `web/quake_wasm.wasm` is gitignored (it embeds the pak);
  the verify scripts write their screenshots into the web dir they serve, so point them at
  a scratch copy unless you mean to update the committed ones.

## Quick commands

```bash
# tests (quake-rs: no game data; quake-wasm: the embedded shareware pak)
cd quake-rs && cargo test --release        # 581 lib + 1 bin + 8 integration
cd quake-wasm && cargo test --release      # 118 (+1 ignored harness)

# goldens (needs the pak): sha256 prefixes 4807aaa1 / 9ae2b478 / c65b7046 at 3ba835f
./target/release/quaketool scene ../quake-data/ID1/PAK0.PAK maps/e1m1.bsp /tmp/e1m1.ppm

# the browser's client natively, frame hashes as bench.py prints them
./target/release/quaketool play ../quake-data/ID1/PAK0.PAK demo1,walk_e1m1 --res 320x200,640x400 --hash-every 30

# benchmark (the page in headless Chromium + a native twin)
uv run --with playwright web/bench.py --build --native

# wasm build + serve
cd quake-wasm && cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quake_wasm.wasm ../web/ && cd .. && miniserve -C -p 8196 web
```

---

# History before 2026-09-25

Kept as evidence. Numbers and file names below are as they were then; where
something has since been superseded it says so in place.

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

*Superseded: the renderer described here (and every timing below) was replaced on
2026-09-25; the surface-cache fix itself still stands (`surf.rs`).*

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

*Superseded in part (2026-09-25): the Options row called "Screen size" here is id's
`viewsize` again, and the resolution list lives in Options > Video Options. The 960x600
default, the 16:10 presets and the localStorage persistence stand.*

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

*2026-05-31. The triangle rasteriser these numbers measure was replaced on 2026-09-25
(polygon spans, then id's edge renderer); current numbers are in `PERF_PLAN.md`.*

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

## Performance scorecard (2026-05-31, idle host, e1m1; superseded by `PERF_PLAN.md`)

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

### Next perf ideas (2026-05-31) — superseded by `PERF_PLAN.md`

The list (clone-hunt leftovers, caching inline submodels' lightmaps, `simd128`, 16-pixel
spans, mip selection) was re-measured on 2026-09-25: `simd128` gave nothing, the 16-pixel
spans and the mip levels are done (as fidelity fixes), and `draw_submodel` and
`compute_visible_faces` no longer exist (id's edge renderer draws the brush models).

### Profiler / benchmarks

(Current tools; the numbers in this bullet list are 2026-05-31's.)

- **Browser (since 2026-09-25): `uv run --with playwright web/bench.py --build --native`** —
  the real page in headless Chromium, deterministic workloads at dt=1/72, per-phase
  median/p95 for wasm and its native twin (see its docstring). `PERF_PLAN.md` has the
  measurements.
- **Render:** `render/stats.rs` is an opt-in `RenderStats` (per-phase ns timers and face,
  pixel, surface-cache and alias counters), zero-cost when off, surfaced by
  `QUAKE_BENCH=<iters> QUAKE_RES=WxH quaketool scene <pak> <map> <out>` and by `bench.py`.
  Wrap any new per-face work in it to keep the cost honest.
- **Sim:** `quaketool simbench <pak> <map> [frames]` benchmarks the game-logic tick (no
  rendering): per-frame ms + VM statements + BSP traces + thinks. Deterministic
  (same counts run-to-run). After the hull-cache fix: e1m1 ≈ 0.82 ms/frame; e1m3 ≈ 2.44 ms/frame
  (522 traces/frame). Since the 2026-09-25 field-offset work (PERF_PLAN D2): e1m1 0.11,
  e1m3 0.29 ms per tick.

---

## ⚠️ Caveats / debt

1. *Superseded: the goldens are `4807aaa1` / `9ae2b478` / `c65b7046` since 2026-09-25
   (top of this file).* **Golden baseline is `fb14bd65` / `a6f98d8a` / `0211e6d4`**
   (e1m1/e1m2/e1m3, post submodel cache). The submodel surface cache (`d2fc0d6`) was re-verified
   **byte-identical** to its parent (0 px diff on e1m1), so its "byte-identical"
   message is correct. (Note: each *world*-cache texel-baking step earlier in the
   history legitimately re-baselined the golden; that's expected and faithful —
   texel-resolution lighting is what Quake's software renderer actually does.)
2. **`ae3ba68` (linear-step perspective) really is NOT byte-identical** — it shifts
   ~57 px (sub-pixel edge ULP drift); corrected by `3fce66c`. That one stands. *(Moot
   since 2026-09-25: that rasteriser is deleted.)*
3. **Benchmark absolute numbers swing with host load.** This session's numbers were
   taken on an **idle** host (load ~0.4/16) and are trustworthy; the prior session's
   "throttling" was likely partly the fixed surf-rebake cost (now fixed) making every
   resolution look uniformly slow. Still: re-check `uptime` before trusting ms, and A/B
   in one sitting.
4. The `quaketool` bench's submodel profiler line label may lag the actual counters
   (cosmetic only; the numbers are right). *(Not re-checked since the edge renderer
   replaced the submodel pass.)*
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

*As of 2026-06-11. The current list is `AUDIT.md`, "Open, as of 2026-09-25"; the
marks added below say what happened to each since.*

All previous HIGH/MED deferred items (lightning beams, R_MarkLights gating,
intermission/finale, ambient sounds, texture pop) shipped in the 2026-06-10
push. What remains is the LOW tail, all reviewer-vetted as non-blocking:

- Demo explosion dlight (demo path emits no dynamic lights). *Still open.*
- quaketool CLI walk paths skip the signon settle (deliberate — keeps CLI
  artifacts byte-stable; unify later with a walk-golden re-baseline).
- Console `kill` drains only a pending restart, not a same-frame changelevel
  (unreachable conflict with vanilla progs).
- The C's CL_UpdateTEnts MAX_VISEDICTS half-cap and its outer-loop index
  clobber (UB in the C) are deliberately not modeled.
- ~~Sound-channel override only dedups within a frame~~ — ✅ CLOSED
  (demo-parity branch, 2026-06-11): the page-side (entity,channel) registry
  implements SND_PickChannel's cross-frame override + S_StopSound, live + demo.
- ~~Per-ammo sbar nits; pain-frame face anim~~ — ✅ the status bar matches id's
  composited screen pixel for pixel since 2026-09-25 (`oracle/screen2d.py`), the pain
  face with census F16; assorted Round-2 LOW list items (in AUDIT.md's open list).
- From the final whole-diff review (all vetted non-blocking): the demo loop
  wrap keeps the ambient ramp warm (deliberate seamless loop; the C's restart
  re-ramps from 0); ~~the intermission idle-sway phase uses w.clock (constant,
  invisible phase offset vs cl.time)~~ — ✅ 2026-09-25 (`quake/polish`: the live
  client clock is `cl.time`); ~~demo1 playback shows ~1.2 s of
  void-camera frames at start/loop-wrap~~ — ✅ CLOSED (demo-parity branch:
  frame emission now gates on signon completion, so the void frames are never
  emitted; loop wrap equally clean, seam-tested); submodel dlight marking uses
  entity-local light origins where the C used world-space (deliberate —
  consistent with the port's local per-luxel submodel lighting; arguably fixes
  a C quirk that mis-lights moved doors). *(Not re-checked since the edge
  renderer, which follows `R_DrawBEntitiesOnList`.)*

