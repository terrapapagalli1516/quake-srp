# Faithfulness Audit — Rust port vs id's original C

Generated 2026-05-31 by a 17-subsystem map→adversarial-verify pass comparing
`quake-rs`/`quake-wasm` against id Software's GPL Quake C (`WinQuake/`). **66
confirmed discrepancies**: 15 high, 24 medium, 27 low (the 27 low are mostly
cosmetic edge cases; see the workflow result if needed). This is the working
roadmap toward a fully faithful single-player port. Out of scope (intentional):
multiplayer/netcode, save/load, audio mixing internals (we use Web Audio),
video/platform init.

Per-subsystem confirmed/rejected: physics 7/0 · user-move 5/1 · builtins 5/2 ·
sbar 10/0 · renderer 6/1 · particles 6/0 · demo 6/0 · sound 4/0 · sv_main 4/1 ·
mdl 4/1 · world/move 3/1 · lighting 3/2 · bsp 2/0 · vm 0/1 · progs 1/7 · pak/wad
0/5 · mathlib 0/4. (The VM, edict loader, asset loaders and mathlib audited
essentially clean — most of their "findings" were rejected as faithful.)

## Session 2 (2026-05-31, extended push)

Beyond the 66-finding audit, this round fixed the user's playtest issues + ongoing refinements:
- ✅ **e1m3 performance** (`ca15409`) — lightmap surface cache + frustum culling + per-face geometry cache. Warm frame ~433ms → ~4ms (~100x). Pixel-identical (golden-verified).
- ✅ **Button/switch alternate textures** (`67f490b`) — BModelInstance.frame threaded so func_button/door select the +a..+j cycle when activated (pressed button turns green).
- ✅ **On-screen messages** (`67f490b`) — centerprint (centered) + sprint/bprint (notify) routed + drawn with Quake timeouts.
- ✅ **Sound choppiness** (`web`) — decoded-WAV cache (content hash) kills per-play decode latency.
- ✅ **Stereo pan law** + **svc_particle** + **demo interpolation activation** (earlier this round).
- A review agent re-verified the prior fixes faithful (SV_WaterMove, colormap, HUD coords, push physics, demo interp) and flagged: Options menu is a 3-row reduction vs id's 14-row M_Options_Draw (sliders/checkboxes), main-menu Help/Quit-confirm missing, rocket-trail if/else order (cosmetic, mutually-exclusive flags), invuln 666/disc (cosmetic).

**Remaining:** Options-menu faithfulness (sliders/checkboxes + functional cvars), Help screen + Quit confirm, ambient sounds (last HIGH — looping audio), invuln 666/disc.

## Session 2 visual-fidelity pass (review-agent driven)

A second review agent hunted rendering diffs vs WinQuake; fixed:
- ✅ **Liquid warp** (`171c01a`) — replaced the GLQuake water warp with software `Turbulent8` (integer-texel-driven 128-cycle table, SPEED=20). Correct ripple wavelength + speed on every water/lava/slime.
- ✅ **Faithful Options menu** (`26df181`) — 13 rows with real sliders (128-131) + checkboxes per `M_Options_Draw`; + 6-page Help screen + Quit confirm.
- ✅ **Powerup/content view tints** (`88eee9a`) — `powerup_cshift` (Quad=blue, Biosuit=green, Ring=gray, Pentagram=yellow) in id's blend order.
- ✅ **Weapon-fire punchangle** kick on the view (`88eee9a`).
- Verified faithful (no fix needed): sky DOME projection (active path), alias frame snap (correct for software), liquid opacity, particle color ramps, EF muzzle/bright/dim dlights, particle size, backface cull.

**Remaining polish (subtle / diminishing returns):** view-roll (strafe lean + damage kick — needs a Camera.roll field across all call sites), weapon viewmodel bob/sway/world-lighting, `.spr` sprite entities (glowing light orbs / bubbles — rare in shareware), the bonus-pickup gold flash (`bf` stuffcmd), invuln "666"/disc, EF_BRIGHTFIELD halo, alias-model colormap LUT. **Ambient sounds** (last HIGH) remains — looping web audio + per-leaf ambient_level; audio, not pixels.

## Session 3 — codebase-wide code reviews (≥3 rounds)

Goal (the user's): a faithful port of Quake I to Rust, with at least 3
codebase-wide code reviews, fixing issues as they are found. Each round
spawns parallel review agents over the whole tree, then the findings are fixed,
tested (396 lib tests), golden-verified (e1m1/e1m2/e1m3 scene sha256 unchanged at
`8eea4f9c`/`1edb0642`/`2009e041`), and committed.

### Round 1 — fixed
- ✅ **clip_box freeze (HIGH)** — `world.rs`: a move that merely *starts* inside a
  box hull returned `fraction=0/allsolid` and froze the mover; now matches the C
  box hull (startsolid set, but allsolid only when the move never exits the box,
  fraction=1, endpos=end) so movers slide out instead of locking when AABBs touch.
- ✅ **sv_move allsolid early-out (MED)** — `server.rs SV_ClipToLinks`: once the
  move is wholly trapped, break the per-edict loop (C `if (trace.allsolid) return;`)
  so a later entity's clip can't clobber `allsolid` back to false.
- ✅ **SV_WalkMove ground gate (MED)** — `server.rs`: the step-down only latches
  FL_ONGROUND/groundentity when `ent.solid == SOLID_BSP` (the player gets ground
  from the regular SV_FlyMove slide), matching sv_phys.c; previously unconditional.
- ✅ **powerup_cshift priority (MED)** — `render.rs`: first-match `else if` chain
  QUAD > SUIT > INVISIBILITY > INVULNERABILITY (was last-wins, so Quad+Pent showed
  the wrong tint).
- ✅ **Options "Screen size" Enter resize (MED)** — `render.rs`/`lib.rs`: Enter on
  the resolution row now propagates a `MenuAction::ResolutionChanged` so the host
  reallocates the framebuffer (was cycling the preset but snapping back).
- ✅ **Help nav (MED)** — `render.rs`: up=next/down=prev (M_Help_Key) and
  left/right also page the Help screen (were inverted / inert).
- ✅ **Rocket-trail stale origin (MED)** — `lib.rs`: prune `trail_org` of freed
  edicts each frame so a recycled slot doesn't draw a spurious streak.
- ✅ **PF_random range (LOW)** — `builtins.rs`: divide by `0x7fff` (closed [0,1],
  matches id) not `0x8000`.
- ✅ **ED_Alloc ceiling (LOW)** — `vm.rs`: `MAX_EDICTS=600`; PF_Spawn surfaces a
  `run_error` instead of growing memory unbounded on a runaway spawn loop.
- ✅ **`+A..+J` anim slot (LOW)** — `bsp.rs`: uppercase animated-texture frames map
  to the ALTERNATE cycle (the C upper-cases first, so `+a..+j` and `+A..+J` are the
  same alternate branch); was mapped to primary.
- ✅ **centerprint position (LOW)** — `render.rs`: y = 200·0.35 (≤4 lines) else 48,
  per SCR_DrawCenterString (was dead-centre).

### Round 2 — 15-subsystem workflow (Rust vs WinQuake C) + adversarial verify

60 agents, 45 findings, **37 confirmed** (4 HIGH, 14 MED, 19 LOW), 8 refuted. Fixed
across commits `081beda` (2a), `6d39591` (2b), `c4b4751` (2c). Golden hashes held
byte-identical throughout (`8eea4f9c`/`1edb0642`/`2009e041`).

Fixed:
- ✅ **EF_ROTATE spin** (HIGH) — bonus pickups spin (`anglemod(100*t)`); was frozen.
- ✅ **View roll** (HIGH) — Camera.roll: strafe lean + 80° dead-view + punch roll;
  demo cams replay recorded `viewangles[ROLL]`. (Damage-kick roll needs svc_damage
  — since landed for demo playback, Session 6 2026-06-11: V_ParseDamage's
  directional v_dmg kick from the recorded svc_damage. Live play infers the damage
  *flash* from stat deltas and has no `from` direction, so the live kick stays open.)
- ✅ **EF_MUZZLEFLASH clear** (HIGH) — SV_CleanupEnts at frame top; muzzle light was
  latching forever after the first shot.
- ✅ **Dead/Tab scoreboard** (HIGH) — scorebar + Monsters/Secrets/Time/level on death.
- ✅ **Per-entity skin** (MED) — armor.mdl now green/yellow/red, not always green.
- ✅ **Alias pitch/roll** (MED) — projectiles point along flight path (R_AliasSetUp-
  Transform); zero-orientation fast path keeps golden bit-identical.
- ✅ **SV_TryUnstick + SV_WallFriction** (MED) — wedge escape at BSP seams + into-wall
  tangential friction.
- ✅ **clip_box entry-axis pullback** (MED) — diagonal box-vs-box stops at correct depth.
- ✅ **Damage flash** (MED) — blood/armour split, min-10 floor, 3*count (was 2×), tint.
- ✅ **Stair-step oldz smoothing** (MED) — stairs glide instead of jolting.
- ✅ **Underwater warp** (MED) — D_WarpScreen sine wobble when the eye is submerged.
- ✅ **Weapon icon inv2_*** (MED) — active weapon settles to the bright icon.
- ✅ **Invuln 666 + disc** (MED) — Pentagram-of-Protection armour field.
- ✅ **spawn_burst count==1024** (LOW) — fiery explosion, not SlowGrav dust.
- ✅ **SV_WalkMove waterlevel jump gate** (LOW) — swimmers can step up.

Deferred (documented, lower priority / higher risk):
- ✅ **R_MarkLights BSP dlight gating** (MED) — fixed in the ship push (2026-06-10).
- ✅/⬜ **No-lightmap face fullbright/black** (MED) — sample-less faces (`lightofs
  -1`) now black like id (Session 7); a map with no lighting lump at all still
  renders Lambert instead of id's row 0 (lightless/test maps only).
- ✅ **Intermission view** (MED) — fixed in the ship push (2026-06-10).
- ⬜ LOWs: client_think pre/post-think order; PF_particle byte count/dir quantize;
  clip_box inopen/plane_dist coords; SV_NewChaseDir integer abs; OP_ADDRESS world
  guard; AngleVectors f64-vs-float (golden-sensitive); ~~sky foreground drift~~ (✅ Session 7); particle
  on-screen size ramp; ST_RAND syncbase; alias triangle near-clip; tracer parity;
  lightstyle /264-vs-/256 (golden-sensitive); sky-name case sensitivity.

### Round 3 — verify-the-fixes + fresh passes (12 dims) + adversarial verify

23 agents, 12 findings, **6 confirmed** (1 MED, 5 LOW), 6 refuted. Crucially the
skeptics REFUTED several "regression-in-new-fix" alarms — re-deriving the math
confirmed the Round-2 view-roll/punchangle, listener pose, and select_interval
truncation were already correct. All 6 confirmed fixed; golden held byte-identical.

- ✅ **Polyblend whole-screen tint** (MED) — software V_UpdatePalette shifts the whole
  palette LAST in SCR_UpdateScreen, so damage/water/powerup tints now cover the HUD,
  centerprint, menu and console too (was 3-D-viewport-only = the GL look). The blend
  is deferred out of step_walk/step_demo and applied to the composited frame.
- ✅ **Monster relink trigger time** (LOW, regression vs an earlier fix's invariant) —
  sv_movestep/sv_step_direction relink touches now use `vm.sv_time` (frame-start
  sv.time) like world.c SV_TouchLinks, not the clamped per-think `time` global.
- ✅ **apply_warp sine table** (LOW) — use id's truncated `3.14159` literal (table tops
  at 5, not 6) for bit-identical D_WarpScreen.
- ✅ **spawn_explosion even/odd** (LOW) — odd→pt_explode / even→pt_explode2, matching
  R_RunParticleEffect/R_ParticleExplosion.
- ✅ **Console/menu mutual exclusion** (LOW) — the menu is suppressed while the console
  is down (Quake key_dest), so the console no longer renders under the menu.
- ✅ **select_interval doc** (LOW) — corrected the false "C loads intervals as a running
  sum" claim (the loader stores them verbatim).

Refuted (no change — verified faithful): dead-view punchangle roll, centerprint-over-
HUD order, listener-pose bob exclusion, PF_makestatic ED_Free, select_interval floor().

**3 codebase-wide reviews complete.** Remaining deferred items (R_MarkLights dlight
gating, no-lightmap shading, intermission view, + the cosmetic LOWs listed under
Round 2) are tracked above for a future pass.

### Session 4 — user playtest fixes + perf + rounds 4-6

User reported: (1) demo brush models not rendering, (2) multi-line pickup messages,
(3) invisible barrel explosion, (4) e1m3 live-play perf terrible (demo fine).

Fixed (commits 6de1c65, 2904090):
- ✅ **Demo brush submodels** — step_demo passed empty bmodels; now resolves "*N"
  precache names to world submodels (doors/elevators render behind the boot demo).
- ✅ **Multi-line pickups** — notify text now accumulates and breaks only on '\n'
  (Con_Print model); "You receive 25 health" was 3 sprint() calls = 3 lines.
- ✅ **Invisible barrel explosion** — `particle(...,255)` is the explosion sentinel
  the C maps to 1024; the live bi_particle recorded it raw. Now mapped (byte-exact).
- ✅ **e1m3 live-play perf (~25×)** — root cause was `find_field`/`find_global`
  LINEAR-SCANNING every def with a strcmp on every ent_get/set (thousands/frame).
  Cached name->offset in Progs (O(1)); + SV_ClipToLinks abs-box broadphase reject.
  600-frame sim: e1m3 22380ms->898ms, e1m1 5708->301ms. PROVEN result-identical
  (sim output byte-identical before/after; golden scene renders unchanged).

### Round 4 — 27 agents, 16 confirmed of 17 (verify-bugfix + fresh passes)

Fixed (commits 6bd546a, 03d91b7):
- ✅ **SV_Impact on world hits** (HIGH) — `if (trace.ent)` includes the world edict;
  the port's `ent > 0` SKIPPED world hits, so rockets/nails never detonated on walls
  and grenades bounced silently. push_entity + fly_move_core now use `>= 0`.
- ✅ **Sprite-model rendering** (HIGH) — facing-sprite pass (draw_sprites) so the
  barrel's s_explod.spr flash + other .spr effects render (live + demo).
- ✅ **TE impact sounds** (MED) — tink/ric (spike/super-spike), wizard/hit, hknight/hit.
- ✅ **Clear notify/centerprint on changelevel** (MED) — stale text no longer lingers.
- ✅ **Viewmodel hidden when dead / invisible** (MED/LOW) — R_DrawViewModel guard.
- ✅ **bi_particle byte-exact** (LOW) — (count & 0xFF)==255 -> 1024, matches MSG_WriteByte.
- ✅ **Centerprint/notify gated on menu/console** (LOW) — key_dest suppression.

Deferred (confirmed, with concrete plans, lower frequency / higher effort):
- ✅ **Lightning/beam temp entities** (HIGH) — fixed in the ship push (2026-06-10):
  `tent.rs` ports cl_tent.c (slot store + CL_UpdateTEnts), live + demo paths.
- ✅ **R_MarkLights dlight BSP gating** (MED) — fixed in the ship push: per-face
  dlightbits via the faithful node recursion (+ a port-specific luxel-extent
  cache-path gate; see the session entry).
- ⬜ **Demo explosion dlight** (LOW) — the demo path emits no dynamic lights.

### Render-perf pass + Rounds 5-6

**Render perf (commit ff4b6ab).** User: higher resolution slows a lot even in simple
maps. Added a render benchmark (`QUAKE_BENCH`/`QUAKE_RES`); render is per-pixel bound
(e1m1 18.4ms@640x400 -> 129ms@1080p). Optimised the textured-triangle inner loop like
Quake's D_DrawSpans: incremental barycentric stepping (3 edge() -> 3 adds) + s/t reuse
the depth reciprocal. ~13-18% faster; near-identical (32-66 of 256k px differ, sub-pixel
edge picks). Bigger remaining lever = `factor_at` bilinear lightmap (~32%) -> a Quake
surface cache (documented follow-up). Golden re-baselined.

**Round 5 (14 agents, 5 confirmed).** Fixed:
- ✅ **World ~1 colormap row too dark** (MED) — lightstyle_scales normalised by 'm'
  (264); id's worldspawn lightstyle(0,"m") -> style 0 = 264 vs the 255*256 white point
  = 1.03125x. Normalise by 256 instead. (commit 12e0550)
- ✅ **Demo bypassed the colormap LUT** (MED) — overbright boot demo; thread gfx/colormap.
- ✅ **R_LightPoint integer truncation** (LOW) — match C RecursiveLightPoint int math.
- Deferred (LOW, documented): push_entity trigger order vs SV_Impact; sprite group syncbase.

**Round 6 — final (13 agents, 5 confirmed).** Fixed (commit d6bb3f1):
- ✅ **physics_step relink** (MED, regression in the sim-perf broadphase) — the freefall
  branch never recomputed absmin/absmax, so a fast-falling MOVETYPE_STEP monster's stale
  abs box could be wrongly broadphase-rejected (missed collision). Relink (SV_LinkEdict)
  before the trigger pass, as the C does.
- ✅ **Demo world too dark** (MED) — seed the demo style 0 = 264/256 (the Round-5 fix
  hadn't reached the demo path).
- ✅ **point_in_leaf strict `d > 0`** (LOW); **megahealth-rot damage flash** suppressed
  (LOW); stale lightstyle doc comment (LOW).
- Refuted: centerprint/notify-after-HUD ordering (verified faithful).

**Golden baseline (as of Round 6; later superseded — see STATUS.md for the
current `fb14bd65`/`a6f98d8a`/`0211e6d4` baseline):** e1m1 `81bca4da`, e1m2 `b93d088a`, e1m3 `df856aeb`
(perf + lightstyle + colormap fidelity applied). 399 lib + 25 wasm tests pass.

**Six codebase-wide reviews complete** (3 + 3). Outstanding deferred work, all documented
above with plans: lightning/beam temp entities (HIGH), R_MarkLights dlight gating (MED),
intermission view (MED), no-lightmap-face shading (MED), the render surface cache (perf),
animated demo lightstyles, + assorted cosmetic LOWs. *(Historical list — since closed:
beams/MarkLights/intermission in the Session-5 ship push, the surface cache in the
render-perf pass, animated demo lightstyles in Session 6's demo-parity work. Of these
only no-lightmap-face shading remains, tracked under "Still open".)*

### Texture lighting cache (lit surface cache, commit 6e95f8a)

Implemented Quake's `d_surf.c` surface cache (mip 0): each lightmapped world surface
is baked ONCE into a per-surface block of final palette indices (texture × lightmap
× colormap), keyed by world + style scales like the lightmap cache (`Rc` blocks,
persistent, rebuilt when a style ticks; gated off dynamically-lit and colormap-less
faces, and blocks over 1<<20 texels). The rasteriser (`raster_triangle_cached`) then
reads ONE byte per pixel + a palette lookup, instead of a texture sample + bilinear
lightmap + colormap-row + colormap-index every pixel.

- **~2.1× faster** warm frames (e1m1 640×400 15.1→7.5ms, 1080p 110→49ms), ~2.4× vs
  the original 18.4ms (with the earlier incremental-edge rasteriser). Deterministic.
- **Fidelity:** the cache samples the lightmap at TEXEL centres (then nearest per
  pixel), which is exactly what Quake's `R_BuildLightMap` + `D_DrawSurfaceBlock8`
  do — so it is MORE faithful to id's software renderer than the port's previous
  per-pixel bilinear lighting (which was smoother than Quake). The two differ on
  ~39% of pixels (avg Δ≈11, max 56 — i.e. about one colormap step, slightly blockier
  lighting gradients), so the world's lighting is now Quake-accurate, not bit-
  identical to the prior render. (NOTE: commit 84e4eda's message wrongly claimed
  "bit-identical / golden unchanged" — that came from corrupted shell output during a
  sandbox fault; this is the corrected record.)
- **Golden re-baselined** (this batch also fixed quaketool's `scene`/`QUAKE_BENCH` to
  read the colormap from the PAK so it exercises the colormap-LUT path the live game
  uses, vs the old failed filesystem read → linear fallback): e1m1 `87c3ff09`,
  e1m2 `d0f48469`, e1m3 `ac2e03dc`. 399 lib + 25 wasm tests pass. The live game
  (always loads the colormap) gets the full 2× in-game.
- Follow-up: mip-level selection would let very large distant surfaces (currently
  over the per-face cap, staying on the per-pixel path) also use the cache and shrink
  block memory.

## HIGH (15)

| # | Finding | Status |
|---|---------|--------|
| H1 | `aim` (#44) never auto-targets — returned `v_forward` unconditionally | ✅ fixed `1146148` (full PF_aim port) |
| H2 | `SV_WaterMove` entirely missing — no swimming physics underwater | ✅ fixed `114ba66` |
| H3 | `SV_WaterJump` (auto climb-out-of-water push) missing | ✅ fixed `1b29e97` |
| H4 | Animated `+texture` sequencing (`Mod_LoadTextures`) + per-frame `R_TextureAnimation` missing — water/teleporters/switches/lights don't animate | ✅ fixed `8f13cb1` (R_TextureAnimation 10 Hz cycles) |
| H5 | ALIAS_GROUP frames never animate — only the first sub-pose is drawn (e.g. flames) | ✅ fixed `8f13cb1` (group-frame + skin-group anim by time) |
| H6 | `pixelAspect` — frame presented 16:10 square-pixel instead of authored 4:3 | ✅ fixed `9876cb4` (present at 4:3); the projection's `pixelAspect` ✅ `quake/w2b` |
| H7 | `R_LavaSplash` not implemented — TE_LAVASPLASH faked as a 20-particle burst | ✅ fixed `8bda8cb` (TE→R_LavaSplash/R_TeleportSplash) |
| H8 | `R_TeleportSplash` not implemented — TE_TELEPORT faked, wrong color | ✅ fixed `8bda8cb` (TE→R_LavaSplash/R_TeleportSplash) |
| H9 | `R_RocketTrail` entirely missing — no rocket/grenade/gib/tracer/voor trails | ✅ fixed `8bda8cb` (trails wired per model flags) |
| H10 | Stale entities never removed — missing the per-message msgtime/relink cull (demo) | ✅ fixed `8f13cb1` (msgtime cull) |
| H11 | Ambient sounds missing — placed `ambientsound()` loops + the 4 automatic leaf ambients | ✅ fixed (ship push 2026-06-10) |
| H12 | Inventory bar (ibar) with weapon icons + current-weapon flash | ✅ fixed `7cbc202` |
| H13 | Animated player face (health/powerup frames; pain-anim is minor TODO) | ✅ fixed `7cbc202` |
| H14 | Item, key, and sigil icons + ammo/armor-type icons | ✅ fixed `7cbc202` |
| H15 | Ammo number always shows shells instead of the active weapon's ammo | ✅ fixed `10ec5bc` (use currentammo) |

Also fixed this session (was a separate reported bug, not in the audit): the
**explosive box** is now shootable — external `b_*.bsp` collision bounds (`ef2c7b5`).

## MEDIUM (24)

**Fixed in wave 1 (`8f13cb1`)** unless noted: skill selection now applies (cvar/cvar_set + spawn filter + carried across changelevel) · `localcmd` inert · `SV_CheckWaterTransition` (splash + watertype/waterlevel) · `SV_Physics_Step` landing thud · `SV_SpawnServer` settle frames · brush-submodel pixel-spread · per-entity `skinnum` · sky dome · alias world-light + dlights · `svc_setangle` no-override · demo inter-frame interpolation (implemented; lib/quaketool still call `parse_demo` — *activation* pending) · EF_ROTATE spin (mechanism; needs model-flag feed) · `R_BlobExplosion` distinct (function ported; TE-wiring pending). HUD big-number x-positions + gold-when-low color fixed earlier (`9876cb4` era). **Still open:** colormap-LUT lighting (needs colormap.lmp input), sound pan law + channel override, and the remaining sbar elements (below).

- builtins: difficulty hard-locked to skill 1 (`cvar("skill")==1`, `cvar_set` no-op, spawn filter only checks NOT_MEDIUM) — **skill selection portals set difficulty but it never takes effect**
- builtins: `localcmd` (#46) faults instead of being a benign no-op
- physics: `SV_CheckWaterTransition` missing — no water-entry splash sound; watertype/waterlevel never set for toss/step entities
- physics: `SV_Physics_Step` missing the landing `hitsound` (dland2.wav)
- sv_main: `SV_SpawnServer`'s two settle frames never run — monsters/items don't droptofloor / settle before play
- world: brush-submodel bounds not pixel-spread for setmodel (world doors/plats 1u too tight)
- mdl: per-entity `skinnum` ignored (always skin 0)
- renderer: sky maps wall (s,t) onto the poly instead of projecting the view-direction dome
- renderer: alias models lit by fixed directional Lambert instead of world light sample + dlights
- lighting: linear 0..4x multiply instead of the colormap LUT (overbright where software Quake has none)
- particles: `R_BlobExplosion` (TE_TAREXPLOSION) routed through the rocket explosion
- demo: `svc_setangle` overrides recorded view angles instead of being ignored
- demo: no inter-frame interpolation (choppy 10 Hz replay)
- demo: EF_ROTATE pickups don't spin
- demo: `svc_particle` count==255 sentinel rendered as fiery explosion instead of `R_RunParticleEffect`
- sound: stereo pan law diverges (C linear 1±dot w/ up-to-2x near gain; JS equal-power)
- sound: channel overriding only dedups within one frame, never cuts a playing sound
- sbar: per-ammo small-digit counts on the ibar missing
- sbar: armor icon by type (+ invuln 666 + disc) missing
- sbar: active-weapon ammo-type icon on the main bar missing
- sbar: low-value gold (anum) digit color not honored
- sbar: health/armor/ammo big-number x positions diverge from sbar.c
- sbar: scorebar / solo scoreboard on death + intermission/finale overlays not drawn

## Wave 2 — DONE

1. ✅ **Particle/TE spawn-wiring** (`8bda8cb`) — rocket/grenade/gib/tracer trails per model flags; TE_LAVASPLASH/TELEPORT/TAREXPLOSION/EXPLOSION2 routed (TAREXPLOSION split out of the dlight group).
2. ✅ **HUD completeness** (`7cbc202`) — ibar + weapon strip + active flash + ammo counts/icons + keys/sigils + face + armor/ammo-type icons (pain-frame face anim + invuln 666/disc are minor TODOs).
3. ✅ **Colormap-LUT lighting** (`604b2ce`) — `gfx/colormap.lmp` threaded through `render_scene_ext`; no more overbright; visually verified e1m1/e1m3.
4. ✅ **Stereo pan law** (`web`) — linear 1±dot with full near-side gain.
5. ✅ **`svc_particle`** → R_RunParticleEffect (not the rocket explosion).

## Still open

All HIGHs and the actionable MEDs are closed as of the 2026-06-10 ship push
(see the session entry below). The remaining tail, all LOW / niche:

- **Lightless maps** (no lighting lump): Lambert instead of id's fullbright row 0
  (narrow; test maps only). Sample-less faces in lit maps: ✅ Session 7.
- Demo explosion dlight. (~~Sound channel override only dedups within a
  frame~~ — ✅ closed in Session 6: cross-frame (entity,channel) override +
  S_StopSound in the page registry, live + demo.)
- ~~Minor sbar polish (pain-frame face anim)~~ — ✅ census F16 below; the
  Round-2 LOW list (sky case-sensitivity, AngleVectors f64, lightstyle /264,
  etc. — all cosmetic).
- ~~Live-play damage-kick roll~~ — ✅ census F16 below (live play reads
  `dmg_take`/`dmg_save`/`dmg_inflictor` like SV_WriteClientdataToMessage).

## Session 5 — the ship push (2026-06-10)

Six parallel implementation branches (one agent each, isolated worktrees),
every branch adversarially reviewed by 2–3 independent lenses (faithfulness
vs the C, engineering, integration), blocking findings fixed on-branch, then
merged serially with the full suite + golden renders verified after every
merge. **449 lib + 42 wasm tests; goldens `fb14bd65`/`a6f98d8a`/`0211e6d4`
byte-identical throughout.** Full narrative in STATUS.md; the faithfulness
ledger:

- ✅ **Death→respawn e2e** — `Server::client_kill` (Host_Kill_f port; console
  `kill` was a health hack bypassing QC), physics_client TOSS/BOUNCE arm
  (SV_Physics_Client; dead corpse froze mid-air). The QC death chain proven
  on real progs.dat with a per-frame deadflag trace, incl. environment kills.
- ✅ **Intermission/finale (MED ×2)** — MSG_ALL svc recognizer (WriteByte/
  WriteString dropped everything before), `Server::set_map_name`
  (SV_SpawnServer's world-edict setup was missing entirely — `mapname` and
  `world.model` were empty for ALL QC consumers: samelevel/noexit/runes/
  episode-end), V_CalcIntermissionRefdef, Sbar_Intermission/FinaleOverlay,
  8-chars/sec finale reveal, svc_sellscreen→Help. Review fix: completed_time
  latches the QC `time` global (cl.time epoch 1.0), not the render clock.
- ✅ **TE_LIGHTNING1/2/3 + TE_BEAM (HIGH)** — `tent.rs` (cl_tent.c port:
  24-slot store, same-entity replacement, 30-unit pieces, integer
  vectoangles, rand()%360 roll, view-entity re-anchor), live + demo. Bonus:
  PF_WriteEntity read its parm as float, not an int global (G_EDICTNUM).
  Documented deviations: missing beam.mdl skips pieces (no Sys_Error); the
  C's MAX_VISEDICTS half-cap and outer-loop index clobber (UB) not modeled.
- ✅ **Ambient sounds (H11, last HIGH)** — PF_ambientsound → StaticSound
  registry (wire-byte-exact vol/atten), `snd.rs` (S_UpdateAmbientSounds +
  GetWavinfo cue-loop gate; water1/wind2 only, like the C), demo
  static_sounds, Web Audio looping with the one-shot spatialGain law +
  sound_generation lifecycle. Documented deviations: Web Audio mixing
  internals (scope note); the ramp runs the C's integer master_vol math on a
  fixed 1/72 s accumulator (Host_FilterTime's cap) so the faithful asymmetric
  fade holds at any display Hz (literal per-frame trunc would stall >100fps);
  one looping source per static (no same-sfx combine pass).
- ✅ **First-frames "pop"** — `Server::run_signon_frames` (the C's two signon
  SV_Physics ticks; frame 0 previously rendered the spawn-settle fall), and
  the `any_dlight_reaches` luxel-extent gate (below). Headless A/B: 17.8% +
  13.3% px pops → max 0.86%.
- ✅ **R_MarkLights BSP dlight gating (MED)** — faithful per-face dlightbits
  node recursion (R_PushDlights/R_MarkLights); submodels marked via their own
  headnode with entity-local origins (R_DrawBEntitiesOnList). e1m1 A/B with an
  injected light: 90,525 affected px → 10,411 (strict subset). Merge
  composition: mask (C-faithful "may contribute") → plane test → luxel-extent
  test — the extent test is a PORT-SPECIFIC tightening of the cache-path
  decision only (the C keys rebuilds on marking alone but always renders
  through its surface cache; this port would flip baked→per-pixel and shimmer
  for zero pixel change). `QUAKE_DLIGHT` knob on quaketool scene for A/B.

**Performance:** resolved — see STATUS.md's scorecard (the surface-cache fix +
clone/alloc hunt landed; 36 fps @1080p idle, per-pixel bound; SIMD remains the
only further ~2× lever and is unscheduled).

## Session 6 — demo playback parity (2026-06-11, branch `ship/demo-parity`)

User: the attract demo must match the real game. In the C the demo IS the
client rendering a recorded stream (full sound, sbar, viewmodel, flashes,
text, recorded lightstyles); the port's demo path discarded most of it.
Faithfulness ledger (ground truth cl_parse.c / view.c / sbar.c / snd_dma.c;
full narrative in STATUS.md):

- ✅ **svc_sound** — CL_ParseStartSoundPacket decode (SND_VOLUME default 255,
  atten byte/64 default 1.0, ent=ch>>3/chan=ch&7, precache-resolved); queued
  through the SAME spatialized path as live, listener = recorded camera. id's
  demo1 carried 595 silently-discarded one-shots.
- ✅ **svc_stopsound + channel override** — S_StopSound + SND_PickChannel's
  cross-frame "always override sound from same entity" via a page-side
  (entity,channel) source registry (closes the audit MED's remaining half;
  channel 0 never keyed). demo1/2/3 send zero stops (engine-asserted census).
- ✅ **svc_lightstyle** — recorded style strings drive demo lighting through
  the shared literal R_AnimateLight math (`server::lightstyle_scales_at`,
  delegation proven byte-identical); the seeded style-0='m' default remains
  only as the synthetic-demo fallback.
- ✅ **svc_clientdata** — CL_ParseClientdata's EXACT bit order (viewheight/
  idealpitch chars, punch char + velocity char*16 interleaved per axis, items
  long, SU_ONGROUND/INWATER, weaponframe/armor/weapon, fixed health/ammo/
  active-weapon trailer); mvelocity shift + CL_RelinkEntities velocity lerp.
- ✅ **Sbar during demos** — the same `render::Hud` as live, fed from recorded
  cl.stats (Sbar_Draw runs during playback in the C).
- ✅ **Weapon viewmodel during demos** — SU_WEAPON via the demo precache +
  SU_WEAPONFRAME, R_DrawViewModel hide gates (dead/invisible/intermission).
- ✅ **svc_damage** — V_ParseDamage: count=(blood+armor)/2 min 10, percent
  += 3*count clamp 150 fading dt*150, blood/armour tint, directional
  v_dmg_roll/pitch kick (v_kickroll/v_kickpitch 0.6, v_kicktime 0.5) decayed
  in V_CalcViewRoll.
- ✅ **svc_print/centerprint** — the live notify (Con_Print '\n' accumulation)
  + centerprint overlays, same gating; svc_stufftext consumed-inert with the
  C's would-exec documented.
- ✅ **Void-camera start/wrap** — frame emission gates on signon completion
  (first entity fast-update = "the final signon stage", cl_parse.c:340 /
  SCR_EndLoadingPlaque); wrap resets the POV state.
- ✅ **V_CalcRefdef parity** — V_CalcBob from recorded velocity, oldz stair
  smoothing on recorded onground, strafe lean, dead-view roll=80 assignment
  semantics, punchangle added LAST; underwater warp + blends deferred to the
  dispatcher like live.

Evidence: `web/verify_demo.py` (new permanent harness) 9/9 + verify_walk/
verify_ambient green; 458 lib + 48 wasm tests; clippy 0/0; goldens
byte-identical (scene renders no demos).

## Options menu + screen framing (2026-09-25, branch `quake/options`)

The user found the Options menu's cursor blinking too fast, and screen size
seeming not to do the right thing. Both real, plus what they exposed:

- ✅ **Cursor blink** — every 12/13 menu cursor blinked off the menudot frame
  parity (10 Hz, host_time); the C is `12+((int)(realtime*4)&1)` (4 Hz, real
  time). The console input cursor was 2 Hz; Con_DrawInput is
  `10+((int)(realtime*con_cursorspeed)&1)`, speed 4. `step(dt)` now splits
  the raw dt like Host_FilterTime (realtime += dt; host_time += min(dt, 0.1)).
- ✅ **View framing (HIGH-visibility)** — the 3-D view was rendered full-screen
  under a pasted-on status bar: horizon at y=100 instead of 76 at 320x200, 24%
  of pixels drawn only to be covered. Now SCR_CalcRefdef + R_SetVrect
  (`render::calc_refdef`): vrect above sb_lines, projection centred on it,
  backtile border (`draw_tile_clear`, Draw_TileClear), sb_lines 48/24/0 gating
  Sbar_DrawInventory / the status strip (death scoreboard still at 0),
  intermission full screen, D_WarpScreen on the vrect only. Proven on e1m1:
  the 100/110 views are the 120 view shifted up 24/12 rows, pixel for pixel.
- ✅ **Screen size = viewsize** — the row had been repurposed to cycle render
  resolutions. Now scr_viewsize (±10, 30..120, slider (v-30)/90), `sizeup` /
  `sizedown` / `viewsize [n]` console commands, default.cfg's `+`/`=`/`-`
  binds, `viewsize 100` on Reset. Resolution lives only in Video Options
  (M_Video), where the preset list already was; localStorage restore intact.
- ✅ **Viewmodel placement** — the gun hung at a hand-tuned offset (7 fwd, 1.5
  right, 3.5 up, near clip 1) calibrated under the old framing; with the fixed
  framing it showed ~3.6x id's gun. Now V_CalcRefdef's origin (forward bob +
  the viewsize fudge +2/+1/+1/+0.5 at 100/110/90/80) and r_aclip.c's
  ALIAS_Z_CLIP_PLANE 5: e1m1 shotgun IoU 0.92 vs an independent projection of
  v_shot.mdl through R_ViewChanged's math.
- ✅ **M_Draw / M_Print** — menus now Draw_FadeScreen the frame under them
  (3-in-4 dither) and print in the bronze conchars half (c+128); M_PrintWhite
  only where the C uses it (current video mode, "No Communications").
- ✅ **Reset to defaults** — execs exactly default.cfg (binds + viewsize,
  gamma, volume, sensitivity); no longer resets bgmvolume / Always Run /
  m_pitch / lookspring / lookstrafe.
- Open: the Video list is one column (the C's 3-wide grid with "Windowed" /
  "Fullscreen" headers and T/D test/default keys is not modelled); the port's
  2-D layer stays a scaled 320x200 screen (WinQuake draws it 1:1 at higher
  modes, with a tiled strip beside a 320-wide sbar); pixel aspect stays square
  (id's 320x200 uses pixelAspect 0.8333 for 4:3 CRTs) — fixed on `quake/w2b`.

Evidence: 489 lib + 67 wasm tests (blink rates, refdef at 100/110/120/90/70/
50/30 + intermission + scaled modes, tile/compose, sb_lines HUD gating, slider,
binds, console commands, Video mode, e1m1 framing, fudge); clippy 0/0; all six
verify_*.py green (verify_menu 60/60 incl. reload persistence); goldens
byte-identical `fb14bd65`/`a6f98d8a`/`0211e6d4` (the scene camera has no
status bar or viewmodel).

## Session 7 — oracle-measured render fixes (2026-09-25, branch `quake/fid1`)

Each fix measured with `oracle/compare.py` (id's own renderer, headless); the
numbers are exact-palette-index match %. Classes refer to `oracle/README.md`.

- ✅ **Liquids and sky overbright** (class 3) — turb/sky went through colormap
  row 0 (~2x); id's `D_DrawTurbulent8Span`/`D_DrawSkyScans8` store the raw
  texel. **Turb warp** (class 8) — now `Turbulent8`'s 16.16 math: `sintable`
  in fixed point (id's `3.14159`, 256 entries, not periodic), added before the
  `>>16`, on `(s+8192)<<16` (`Mod_LoadFaces`' turb `texturemins`). e1m1 water
  from above 21.1% → 99.45%. Goldens: e1m2 `a6f98d8a` → `76905e15`.
- ✅ **Sky layers + sampling** (class 4; was the LOW "sky foreground drift") —
  the front layer is now `R_MakeSky`'s composite, shifted `(int)(skytime*8)`
  texels over the back (so it scrolls at 2x), `skytime` wrapped at 512 s
  (`R_SetSkyFrame`), `D_Sky_uv_To_st` at the integer pixel and screen centre.
  And `D_DrawSkyScans8`'s spans: the world pass records its sky pixels and
  `resolve_sky_spans` redraws each visible run of one sky face exactly every 32
  pixels, stepped between. e1m2 sky region (130,0,65,32, mip 0 + exact
  perspective on id's side): 15.6% at the start, 23.5% after the class-3 fix,
  93.2% with the layers, 99.8% with the spans. e1m2 world 61.25 → 64.07.
  Goldens unchanged (no sky in them).
- ✅ **Alias models** (class 2; the old "alias-model colormap LUT" item) — the
  port lit them with its own heuristic and an RGB multiply (off-palette
  colours, fullbright flames darkened) and rasterised them perspective-correct.
  Now every alias model and the gun go through a port of id's pipeline:
  `R_DrawEntitiesOnList`/`R_DrawViewModel` light (R_LightPoint + dlights,
  128/192 clamps, gun >= 24), `R_AliasSetupLighting` (`LIGHT_MIN`, the
  `{-1,0,0}` light vector in the model frame), `R_AliasCheckBBox`
  (trivial accept, subdivision beyond `r_aliastransition`, `size/11`),
  per-vertex `r_avertexnormals` light, `R_AliasClipTriangle`, and
  `D_PolysetDraw` — affine, Gouraud, integer vertices, `acolormap[texel +
  (light & 0xFF00)]`, 16-bit-z semantics against the shared z-buffer, the
  gun's 1/z tripled. Entity pixels (id with mip 0 + exact perspective):
  e1m2 15.9% → 99.7%, e1m3 71.0% → 99.5%, e1m7 45.8% → 98.2%; the e1m2 altar
  view (ogre + two flames) 21.8% → 100.0% with id as shipped. nonpal% is 0
  everywhere. Goldens: `fb14bd65` → `d103ba3f`, `76905e15` → `a833cbac`,
  `0211e6d4` → `ed42c092` (monsters/items in all three).
- ✅ **Viewmodel** (class 5; the placement itself landed with the options
  branch) — what still differed from id: the gun is drawn by the alias
  pipeline above (lighting, affine, tripled 1/z in the shared z-buffer
  instead of a private depth buffer, no gun at fov > 90); its origin lacks
  the camera's 1/32 node-line epsilon (-1/32 relative, ~0.5 px at 320x200);
  the bob moves it along the full view pitch (V_CalcRefdef has just set the
  entity angles to the view's, not the server's third); and its angles are
  CalcGunAngle's — the view before `cl.punchangle`, without the view roll
  (`Viewmodel::angles`, `viewmodel_angles`). `quaketool view --viewent` takes
  the C's `cl.viewent`. e1m1/e1m2/e1m3 at `--settle 3`, id with mip 0 + exact
  perspective: the frame with the gun matches as well as the world-only
  frame (93.96/96.88/98.52 vs 93.95/96.85/98.51); the gun region 99.9%.
  Goldens unchanged (no gun in them).
- ✅ **Sample-less faces** (class 9) — they are NOT (only) sky/turb: e1m1 has
  375 world + 211 submodel faces with ordinary textures and `lightofs -1`
  (e1m2 431/241, e1m3 431/490 — the light tool found no light reaching them),
  besides its 304 turb + 63 sky faces. The port drew them with its Lambert
  fallback; `R_BuildLightMap` clears to the ambient (0), has no samples to
  add, adds dlights and inverts — black. Now the same. None is visible in the
  oracle's standard views; the e1m1 golden view shows one (a recessed panel
  edge): that view's region 94.6% → 97.7% against id. Goldens: e1m1
  `d103ba3f` → `5b29abb8`, e1m3 `ed42c092` → `e3d873f0` (3 px), e1m2
  unchanged. Still open: a map with no lighting lump renders Lambert, id
  fullbright.

## Host loop: Host_FilterTime's 72 fps cap (2026-09-25, branch `quake/host`)

- ✅ **Frame cap** — the port stepped and rendered once per rAF, so a 144 Hz
  display ran twice Quake's frames. `step()` now gates like Host_FilterTime:
  `realtime += dt` every call; a frame runs only once 1/72 s has passed since
  the last (`oldrealtime = realtime`, overshoot dropped) and advances the game
  by that time clamped to [0.001, 0.1]; `step` returns 0 on a skipped call and
  the page presents nothing. `dt = 0` remains the tests' frozen frame.
- **Deviation (documented at `HOST_FRAME_TOLERANCE`):** a frame may run up to
  1 ms early. A rAF host sees whole vsyncs; at 144 Hz two are 13.889 ms, on
  the knife edge of 1/72 s, and the strict test judders 72/48 fps. With 1 ms:
  60/75/90/100/120/144/165/240/360 Hz give 60/75/45/50/60/72/55/60/72 fps,
  each at a fixed vsync count per frame (unit-tested). Only 75 Hz exceeds 72.
  Accepted gap: 85–100 Hz displays get half their rate (the C gate needs two
  vsyncs there).

## Frame composition (PERF_PLAN B, 2026-09-25, branch `quake/perf-b`)

- ✅ **Cshifts are the software `V_UpdatePalette`'s integer ramps** (B2). The
  port blended the finished frame per pixel in f32 through `V_CalcBlend`'s
  combined alpha, rounding — but `V_CalcBlend` is GLQuake's (`#ifdef
  GLQUAKE`); the software build walks `cl.cshifts` (CONTENTS, DAMAGE, BONUS,
  POWERUP) over every palette level with `v += (percent*(destcolor-v)) >> 8`
  (`int` percent, arithmetic shift: fractions round toward minus infinity) and
  then `gammatable[v]`. `render::cshift_ramps` is that, per channel, and the
  host packs the finished frame through the three ramps (a palette colour's
  channels looked up in them = the C's shifted palette entry). Unit-tested
  against hand-worked C values, including the order dependence (water then
  damage: level 100 → 197; the other way round 160).
  **Re-baseline:** only frames with a shift on move; the goldens have none and
  are unchanged (`5b29abb8`/`a833cbac`/`e3d873f0`). Over id's palette the new
  ramps differ from the old blend on 180–249 of 256 colours per shift, by 1
  level (2 at most, lava and the 150% damage flash). On real frames at 320×200
  (native dumps, old vs new): the Quad walk — 30 of 30 sampled frames differ,
  76–94% of pixels each, every channel by exactly 1, always darker; id's demo1
  — the 28 damage-flash frames (of 720) differ, 80–100% of pixels, |d| 1
  (2 on 11% of changed pixels), darker except 102 channels. The other 692
  frames are byte-identical.
- **Seen, not changed (the cshift *state*, another branch's):** the C keeps
  `percent` as an `int`, so `cl.cshifts[CSHIFT_DAMAGE].percent -=
  host_frametime*150` truncates every frame (150 → 147 at 72 fps, not
  147.9); the port decays a float and truncates only in the ramp. And
  `V_SetContentsColor`'s `default:` is water — any contents other than
  empty/solid/lava/slime, sky included — where `content_cshift` returns none.

## World pass: polygon spans, dlit surface cache (2026-09-25, branch `quake/w1`, PERF_PLAN A0–A2)

- ✅ **A1: faces scan-converted as polygons.** Each clipped world, submodel
  and external-box face was fan-triangulated and each triangle's bounding box
  walked with an inside test, its s/z and t/z interpolated per triangle from
  the vertices (absolute s, thousands of texels, in f32). Now every face is one
  polygon walked row by row (`raster.rs`: `scan_poly`, `raster_poly_cached`,
  `raster_poly_tex`, `raster_poly_flat`):
  - **Fill rule = id's:** pixel centres, left and top edges inclusive, right
    and bottom exclusive (`R_EmitEdge` takes rows `ceil(v0)..ceil(v1)-1`,
    `R_GenerateSpans` columns `ceil(u_l)..ceil(u_r)-1`). Every edge's
    crossing is computed from its top endpoint, and a near-clipped edge's new
    vertex from its inside endpoint, so two faces sharing an edge get the
    same bits.
  - **Gradients = `D_CalcGradients`:** 1/z from the face plane over the eye's
    distance to it (`R_RenderFace`), s/z and t/z from the view-space texinfo
    axes, with s and t relative to the eye's (`sadjust`/`tadjust`); f64
    accumulators; the cached span loop takes the texel as `D_DrawSpans8` does
    (`(int)(sdivz*z) + sadjust`, `>> 16`).
  - **Goldens:** e1m1 `5b29abb8` → `bb64996e` (151 px, 0.059%), e1m2
    `a833cbac` → `8186a64c` (709 px, 0.28%), e1m3 `e3d873f0` → `f41e8b59`
    (729 px, 0.28%). All are interior texel-boundary pixels: none sits next to
    a colour edge, and there are 0 background pixels before and after. Against
    an exact reference (f64 perspective per pixel) the old renderer was wrong
    on 149 / 731 / 722 of those pixels and the new one on 6 / 34 / 13, so the
    moves remove the old f32 rounding.
  - **Cracks:** over 144 oracle views (8 maps × 6 yaws × 3 pitches), the old
    renderer left 5 background pixels at 320×200 and 30 at 640×480; the new one
    leaves 0 at both.
  - **Oracle,** mean exact% over those 144 views, old → new: 320×200
    79.873 → 79.907 (118 views better, 23 worse), with id at mip 0 + exact
    perspective 97.682 → 97.760 (139 better, 5 worse); 640×480 90.636 →
    90.774 (138 / 6), and 97.611 → 97.801 (144 / 0).
  - **Open:** the 16 standard rows (`compare.py` world, 320×200 and 640×480,
    id as shipped and mip 0 + exact) rise on 9 rows and fall by 0.01–0.03
    points on 7 (e1m1 84.76 → 84.74, e1m7 75.65 → 75.62, …; PERF_PLAN A1
    has the table). The lost pixels are single pixels at texel boundaries
    (some exactly on one, at these axis-aligned start views) and face edges,
    where the port's and id's float rounding fall on different sides. The old
    port's rounding agreed with id's more often on these views, but not on
    average. Tried without effect: floor versus 16.16 texel arithmetic, vertex
    versus plane gradients, f32 versus f64 divides, an id-style float camera
    basis.
- ✅ **A2: dynamically lit walls through the surface cache**
  (`D_CacheSurface` + `R_AddDynamicLights`). A wall a dynamic light reached
  left the surface cache for a per-pixel path, with bilinear lightmap and
  colormap on every screen pixel. Now `face_surf_block` bakes it with the
  light at texel resolution, and marks the entry `dlight` (`cache->dlight`):
  it is never a hit, so the first frame without the light rebuilds it.
  Two fixes come with the C's hit test:
  - **The texture is part of the key** (`cache->texture`). Animated wall
    textures (`+0…`) were frozen on whichever frame was baked first, on every
    cached wall; a unit test now fails on the old code.
  - **The submodel `ent_frame == 0` gate is gone.** An activated button's
    alternate texture is just another texture.

  Goldens unchanged. Oracle muzzle-flash frames (new: `compare.py --c-cmd
  +attack --settle 3`, with id's `cl_dlights` handed to `quaketool view
  --dlight`), mip 0 + exact, 320×200, exact% before → after: e1m1 64.12 →
  90.01, e1m2 88.46 → 96.85, e1m3 76.47 → 97.77; as shipped, 55.96 → 80.30,
  52.28 → 60.66, 40.72 → 60.95.
  - **Pixels the flash lights in id** (e1m3): the port matches 96.1% of them,
    was 23.1%. Of the rest, about 1% are the gunshot's particles (`view`
    draws none) and about 3% are one colormap row off. That is the class-6
    lightmap interpolation (oracle README), which a dlight's steep gradient
    brings out more than static light does. So dlit lighting is per texel
    now, but not id's texel for texel until class 6 is ported.
  - **e1m1's lit frames** also carry the settle ≥ 3 light-style offset of the
    harness (oracle README).
- ✅ **`R_AddDynamicLights` in the C's integers.** The per-luxel distance
  took float `sd`/`td` and `min/2`. The C truncates the offsets to `int`,
  halves with `>> 1` and truncates `(rad - dist)*256` into the 8.8
  `blocklights`. Now the same. `any_dlight_reaches` keeps a 2-unit margin so
  it stays conservative. Flash-lit pixels (mip 0 + exact, 320×200) now match
  83.4% on e1m1 (was 81.8%) and 96.1% on e1m3 (was 95.9%); no row fell.
  Goldens unchanged.

## World pass: mip levels, lightmap stepping (2026-09-25, branch `quake/w2a`, PERF_PLAN A5)

- ✅ **Mip levels** (oracle class 1, the largest departure). The port baked every
  surface from the full-resolution texture; id draws each surface at the mip
  level `D_DrawSurfaces` picks per frame, `D_MipLevelForScale(nearzi *
  scale_for_mip * mipadjust)`, and caches one block per level. Now the same
  (`surf.rs`: `MipView`, `face_surf_block`):
  - **`nearzi`** is the largest `1/z` over the face's outline clipped to the
    frustum's four side planes, `z` clamped to `NEAR_CLIP` 0.01 — what
    `R_RenderFace` gathers from `R_EmitEdge` (the left clip edge and the
    right one's `1/z` included). `scale_for_mip` is the larger focal length
    (`D_ViewChanged`); `mipadjust` is `Mod_LoadTexinfo`'s 1/2/3/4 from the
    mean texture-axis length; the thresholds are `basemip` {1, 0.4, 0.2} times
    `d_mipscale`, floored at `d_mipcap` (`D_SetupFrame`; id sets no higher
    floor at high resolutions). Both cvars are settable
    (`render::set_mip_cvars`, `quaketool view --d-mipscale/--d-mipcap`;
    `compare.py` hands a `--c-cmd "d_mipscale 0"` to both renderers).
  - **The block** is `D_CacheSurface`'s: `extents >> miplevel` texels a side
    (it was `extents + 1` at mip 0 — one column and row id never reads:
    `bbextents` clamps s to `extents - 1`), from the level's texels
    (`bsp.rs` now keeps levels 1..3, `MipTex::mip`), tiled from
    `texturemins >> miplevel`; one cache slot per face per level
    (`cachespots[miplevel]`). The span walker reads it through gradients
    scaled by `1 / (1 << miplevel)` (`PolyGrads::mip_scaled`, `D_CalcGradients`'
    `mipscale`). Submodels and the external `b_*.bsp` boxes pick their levels
    the same way. A texture without levels 1..3 (synthetic tests) stays at
    mip 0; a face whose texinfo maps it to a line (zero extent; id's
    `D_SCAlloc` would `Sys_Error`) keeps the per-pixel path.
  - **Oracle,** exact% world-only 320×200 (id as shipped): e1m1 84.74 →
    92.20, e1m2 64.06 → 91.03, e1m3 65.88 → 96.68, e1m7 75.62 → 92.58;
    640×480: 90.84 → 94.78, 78.28 → 95.40, 90.73 → 97.56, 94.58 → 97.39;
    against id's exact per-pixel perspective (`--spans 1`) 86.95 → 94.55,
    64.26 → 92.64, 66.11 → 97.47, 80.37 → 97.46. With both renderers at mip
    0 (`d_mipscale 0`) every row is unchanged — the mip-0 bake is the old one.
    Muzzle-flash frames (`--c-cmd +attack --settle 3`, as shipped): e1m1 80.64
    → 87.02, e1m2 60.66 → 89.76, e1m3 61.00 → 95.80.
  - **Goldens:** e1m1 `bb64996e` unchanged (every face in that view is at mip
    0), e1m2 `8186a64c` → `3d47c70d` (38,742 px, 15.1%), e1m3 `f41e8b59` →
    `c7e5b50e` (31,730 px, 12.4%): the distant walls, now drawn from id's
    coarser levels.
  - **Not done:** a brush model spanning several BSP leaves is split by
    `R_DrawSolidClippedSubmodelPolygons` into fragments, each with its own
    `nearzi` and so possibly its own level; the port picks one level per face.
    The wasm console has no `d_mipscale`/`d_mipcap` commands yet.

- ✅ **Lightmap stepping** (oracle class 6, ±1 colormap row on 1.5–4.4% of
  pixels). The bake took a float bilinear lightmap factor at each texel's corner
  and then picked the row. id builds `blocklights` in 8.8 integers
  (`R_BuildLightMap`: each style's luxel times its `d_lightstylevalue`, plus
  `R_AddDynamicLights`' truncated `(rad - dist)*256`), inverts and clamps them
  (`(255*256 - bl) >> 2`, at least `1 << 6`), and `R_DrawSurfaceBlock8_mip0..3`
  walks each `16 >> miplevel` cell down its left and right edges in integer
  steps `(bottom - top) >> (4 - miplevel)` and along each row from the RIGHT
  edge's value by `(left - right) >> (4 - miplevel)`, indexing
  `colormap[(light & 0xFF00) + texel]`. Now exactly that
  (`LightMap::blocklights_into`, `surf::draw_surface_block`); the port's f32
  luxels are the C's sum over 256, each term exact, so `luxel * 256` is id's
  integer. The oracle agent's temporary patch of earlier tonight, made
  permanent for all four levels.
  - **Oracle,** exact% world-only 320×200, item 1 → this: as shipped 92.20 →
    97.44, 91.03 → 97.12, 96.68 → 99.16, 92.58 → 94.96 (e1m1/2/3/7);
    640×480 94.78 → 99.20, 95.40 → 98.77, 97.56 → 99.30, 97.39 → 98.77;
    against id's exact perspective (`--spans 1`, its own mips) 94.55 →
    99.94, 92.64 → 99.18, 97.47 → 99.98, 97.46 → 99.91; both at mip 0 with
    exact perspective 95.61 → 99.93, 97.40 → 99.88, 98.50 → 99.98, 98.66 →
    99.91 (640×480: 99.96 / 99.96 / 99.99 / 99.98). What is left as shipped
    is id's 8-pixel affine span segments (class 7, branch `quake/w2b`).
    Over 72 more views (4 maps × 6 yaws × 3 pitches, `--spans 1`) the mean is
    99.99%, the worst 99.83.
  - **Goldens:** e1m1 `bb64996e` → `959d0221` (9,953 px, 3.9%), e1m2
    `3d47c70d` → `0cd18471` (8,041 px, 3.1%), e1m3 `c7e5b50e` → `b63ae8b7`
    (7,555 px, 3.0%): single colormap rows on light gradients. From before
    item 1: 9,953 / 42,613 / 36,634 px.
- ✅ **Dynamic lights at the chosen level** (w1's A2 path): a dlit face is baked
  by the same `face_surf_block`, so it gets the level and the integer stepping
  with the light's `blocklights` folded in (`cache->dlight` as before).
  Muzzle-flash frames (`--c-cmd +attack --settle 3`), 320×200, base → now: as
  shipped e1m1 80.64 → 95.36, e1m2 60.66 → 96.16, e1m3 61.00 → 98.98; mip 0
  + exact 90.35 → 98.11, 96.85 → 99.46, 97.82 → 99.89. On the pixels id's
  flash changes (id's lit frame against id's frame at the same view and
  clock without the shot): e1m1 83.4% → 99.8% (14,296 px), e1m2 90.3% →
  94.9% (825 px; the other 42 are the shot's blood particles, which `view`
  does not draw), e1m3 96.1% → 99.5% (13,211 px). e1m1 keeps the harness's
  settle ≥ 3 light-style offset (oracle README).
- **Speed and memory** (items 1 and 2), native twin of `web/bench.py`, a fresh
  process per resolution, medians of two runs; base → mip levels → + stepping:
  - Texels baked per frame (`surf_texels`, the world cache plus the external
    boxes' uncached bakes): demo1 4,348 → 2,847 / 3,511 / 3,993 (320×200 /
    640×400 / 1280×800); walk_e1m1 35,185 → 3,963 / 6,983 / 11,482;
    fire_e1m1 48,660 → 16,322 / 19,594 / 23,880; walk_e1m3 68,628 → 2,902 /
    5,683 / 10,767.
  - Time in `face_surf_block` (mean ms per frame): fire_e1m1 0.45–0.46 →
    0.34–0.45 → 0.026–0.047; walk_e1m1 0.17–0.18 → 0.09–0.19 → 0.016–0.033.
    Whole step, fire_e1m1 median / p95: 320×200 1.48 / 3.79 → 0.68 / 1.35
    ms, 640×400 2.17 / 4.92 → 1.55 / 2.75, 1280×800 5.37 / 8.91 → 4.97 /
    7.40; walk_e1m3 320×200 2.80 → 1.28, 640×400 3.78 → 2.32, 1280×800 7.09
    → 5.78. demo1 moves within noise.
  - Wasm (`web/bench.py --build`, two runs each, step median / p95 at 640×400,
    median at 1280×800): walk_e1m3 3.42–3.60 / 4.60–4.86 → 2.62–2.78 /
    3.49–3.63 ms, 7.50–7.74 → 6.58–6.85 (the external boxes' uncached bakes
    0.81 → 0.08 ms); fire_e1m1 2.09–2.27 / 4.05–4.18 → 1.72–2.07 / 2.73–3.17,
    5.67–5.94 → 5.37–5.75; walk_e1m1 p95 3.41–3.59 → 2.70–2.94; demo1 within
    noise.
  - `QUAKE_DLIGHT=eye` on e1m1 at 1280×800 (w1 left it at ~14 ms): radius 350
    14.1–15.1 → 10.0–13.9 → 7.4–9.1 ms, radius 200 12.1–12.7 → 8.0–8.5 —
    the unlit frame is 8.2–8.6.
  - Surface cache resident (`surfcache_kb`, the most over a run): demo1 3,489
    KB → 2,166 / 2,985 / 3,359; walk_e1m1 1,973 → 975 / 1,367 / 1,785;
    walk_e1m3 3,350 → 1,071 / 1,757 / 1,998. (id's fixed pool is 600 KB at
    320×200 and ~3.4 MB at 1280×800, `D_SurfaceCacheForRes`.)
- **Still open:** `nearzi` is the clipped outline's, which is id's in all but
  one quirk: `R_RenderFace` re-uses `r_rightexit`/`r_leftexit` when the edge
  that should set them was cached as fully clipped by an earlier face this
  frame, so id can give a face the `1/z` of a stale point from another face —
  a finer level than its geometry (seen: face 733 at e1m2's first frame, id
  mip 0 from a point at z 96, the port mip 1; 0.7% of that frame, 0.17% of
  one of the 72 sweep views, nothing elsewhere). Reproducing it means
  running id's edge clipping and edge cache in `R_RecursiveWorldNode`'s order.

## Census client/host fixes (2026-09-25, branch `quake/fix-client`)

One line per CENSUS.md finding fixed; the evidence and the C are in CENSUS.md
and the commit messages.

- ✅ **F2 single player pauses behind the menu/console** (`Host_ServerFrame`/`SV_RunClients`): `step_walk` runs no server frame while `key_dest != key_game`; `cl.time` (`w.clock`) freezes with it, host-time fades/countdowns keep going (new `Walk::host_time`); the attract demo keeps playing.
- ✅ **F1 teleporters (and spawns) turn the view** (`SV_WriteClientdataToMessage` fixangle → `svc_setangle`; `Host_Spawn_f`'s setangle): after the server frame `step_walk` copies the player's `angles` into the view through `MSG_WriteAngle`/`ReadAngle` quantisation and clears `fixangle`; every walk builder starts facing the spawned player's `angles` (SelectSpawnPoint's spot, `info_player_start2` included) instead of parsing `info_player_start`.
- ✅ **F4 an impulse pressed during the weapon cooldown is kept** (`SV_ReadClientMove` only sets a non-zero impulse; QC `ImpulseCommands` clears it after `W_WeaponFrame`'s cooldown return): `Server::physics_client` no longer zeroes `impulse` after PlayerPostThink (server.rs, a few lines + its two tests).
- ✅ **F10 runes on the status bar** (`SV_WriteClientdataToMessage`: `items | serverflags << 28`): the live HUD's `items` is `client_items(w)`, so `Sbar_DrawInventory`'s sigil cells light up.
- ✅ **F11 Tab = `+showscores`** (default.cfg `bind TAB +showscores`, `Sbar_Draw`'s `sb_showscores`): `BIND_SHOWSCORES` in the bindings table (TAB by default), `KeyMove::showscores` feeds `Hud::show_scores` live and in demo playback; the page no longer opens the menu on Tab (Esc still does).
- ✅ **F17 weapon keys by key number** (default.cfg `bind 1 "impulse 1"`..`bind 8`, `bind 0 "impulse 0"`; Key_Event works on key numbers): `"impulse N"` commands in the bindings table, bound to the digit row by default; the page sends digits through `quakeKey` (e.code `Digit*`), so Shift+digit and AZERTY select weapons; the `e.key` digit path is gone.
- ✅ **F16 damage flash, kick and pain face in god mode / with the Pentagram** (QC `T_Damage` accumulates `dmg_take`/`dmg_save` before its god/invulnerable returns; `SV_WriteClientdataToMessage` sends svc_damage and zeroes them; `V_ParseDamage`): `step_walk` reads and zeroes the three fields after the server frame instead of inferring the hit from health/armour deltas (the megahealth-rot guard is moot); the live view now has V_CalcViewRoll's directional kick and `cl.faceanimtime`'s pain face (`Hud::face_pain`, `face_p*`), demo playback too. `V_ParseDamage` lives in quake-wasm `view.rs`, shared.
- ✅ **F6 gold bonus flash** (QC `stuffcmd(other, "bf\n")` at 16 pickup/powerup sites → `svc_stufftext` → `V_BonusFlash_f`, dropped `host_frametime*100` by V_UpdatePalette): `PF_stuffcmd` queues `(entity, text)` (builtins.rs; server.rs installs it at #21); the live walk runs `bf` for its player, demo playback runs recorded `svc_stufftext`; the bonus cshift sits between damage and powerup. Accepted gap: Cbuf_Execute would run it one host frame later.
- ✅ **F18 new-weapon icon flash** (`CL_ParseClientdata` stamps `cl.item_gettime[j]` for newly set bits; `Sbar_DrawInventory`'s `flashon` picks `inva1..5` for a second): `Hud::item_gettime`, stamped by the live walk and the demo (zeroed with the level, so carried weapons flash at level start); keys/powerups/runes never visibly flash in the C (their `flashon` is 0).
- ✅ **F13 demo trails** (`CL_RelinkEntities` → `R_RocketTrail` for model flags, live or demo): `EntSnapshot::num` + `DemoPlay::trail_org`; step_demo trails like step_walk (no EF_ROCKET dlight: demo dlights are still the open gap).
- ✅ **F14 demo skins** (`CL_ParseUpdate` U_SKIN / baseline skin, `CL_ParseStatic`): demo.rs keeps the skin, `EntSnapshot::skin` → `ModelInstance::skinnum`; yellow armour is yellow in demo2/demo3.
- ✅ **F15 attract loop cycles demo1 → demo2 → demo3** (quake.rc `startdemos`, `svc_disconnect` → `Host_EndGame` → `CL_NextDemo`): `DEMOS` + `build_demo_n`; the dispatcher starts the next demo when one has shown its last frame (same-demo wrap kept as the fallback).
- ✅ **L2 cshift percents are ints** (client.h `cshift_t.percent` is `int`: V_ParseDamage's `+=` and V_UpdatePalette's drops truncate every frame — 150 → 147 at 72 fps): view.rs `cshift_add`/`cshift_drop` for the damage and bonus shifts, live and demo. Closes perf-b's first "seen, not changed" note above.
- ✅ **L24 contents tint defaults to water** (`V_SetContentsColor`'s `default:` — sky included): `content_cshift` returns none only for empty/solid. Closes perf-b's second note.
- ✅ **L1 live punchangle in whole degrees** (`MSG_WriteChar(punchangle[i])`): `client_punchangle` truncates to signed chars for the camera and the gun.
- ✅ **L8 particles drawn before they move** (`R_DrawParticles`: free `die < cl.time`, draw, then move/ramp): `ParticleSystem::retire` + `integrate`, called around the draw list in step_walk/step_demo.
- ✅ **L9 dlights drawn before they decay** (Host_Frame: `CL_DecayLights` after `SCR_UpdateScreen`; R_PushDlights skips `die < cl.time`): step_walk renders `pushed_dlights` and decays after the 3-D view.
- ✅ **L11 notify lines** (`Con_Print` 38-column word-wrapped lines stamped at their start; `Con_DrawNotify` last 4 from `v = 0`): quake-wasm `ConNotify`, live + demo; `draw_notify` from y = 0. ~~Still open: prints never reach the drop-down console's scrollback~~ (✅ `quake/polish`, c71989f).
- ✅ **L12 default.cfg binds** — ENTER `+jump`, MOUSE2 `+forward`, `\` and MOUSE3 `+mlook`, INS `+klook` seeded; the page sends MOUSE2/MOUSE3 while locked. Not done: PAUSE (no `pause`), the F-key commands, `t` messagemode.
- ✅ **L14 New Game asks first while a game runs** (`M_SinglePlayer_Key` → `SCR_ModalMessage`, y/n/Escape, faded screen + `SCR_DrawNotifyString`): `Menu::new_game_confirm`, raised when the host-set `server_active`; y (`menu_quit_yes`) starts the game.

## Census fixes, server side (2026-09-25, branch `quake/fix-server`)

One line per fix; evidence and tests in the commit, the rows in `CENSUS.md`.

- ✅ **Chthon's electricity** — boss.qc `lightning_fire` writes TE_LIGHTNING3 to MSG_ALL (`sv.reliable_datagram`), which `CL_ParseServerMessage` parses like the datagram; the port decoded temp entities only from MSG_BROADCAST, so Chthon died with no bolt drawn. `server/msg.rs` now runs one svc parser per buffer (datagram, reliable), each reading temp entities and commands alike (fix first found by the `chthon` agent, salvaged `00bf4a7`). Test `cl_tent::tests::chthon_lightning_reaches_the_client_and_kills_him_on_e1m7`.
- ✅ **F3 e1m8 low gravity** — `sv_gravity` is a live cvar (`server/host.rs`): world.qc `worldspawn`'s `cvar_set("sv_gravity", "100"|"800")` lands, `cvar("sv_gravity")` reads it, `SV_AddGravity`, `SV_Physics_Step`'s landing-sound threshold and the live `R_DrawParticles` gravity use it; a fresh server starts at 800 (since `quake/polish2` the cvar outlives the map, as id's: the next worldspawn sets it). Test `census_e1m8_has_low_gravity` (+ `sv_gravity_cvar_drives_add_gravity`). The demo path keeps 800 (no server runs during playback).
- ✅ **F5 b_*.bsp item boxes** — two causes. (1) `WorldModel::precache_model` spread the external model's bounds a second time (`Bsp::parse` already applies `Mod_LoadSubmodels`' pixel): the explosive box was 34 wide (hull2) and floated 2 units. (2) `world::clip_box` treated touching boxes as overlapping; the C box hull (`SV_InitBoxHull`) is half-open, `mins <= p < maxs`, so e1m1's 10-health box beside a grunt and e1m6's 25-health box beside an ogre no longer "fall out of the level" in PlaceItem's droptofloor. Oracle edict diff: both boxes present, explosive box at id's z -207.969; nothing else moved on any map. Tests `census_bmodel_item_bounds_are_spread_once`, `clip_box_is_half_open_like_the_c_box_hull`.
- ✅ **F8 force_retouch** — `SV_Physics` relinks every live edict with `SV_LinkEdict(ent, true)` while the QC global is set (`spawn_tdeath`, `teleport_use` set 2) and decrements it after the loop; now both `run_frame` and `client_frame` do (world skipped, SOLID_NOT relinked without touching). e1m6 doors *31/*76 and e1m8 *6 open at the level start as in id (oracle: open by t 4.7; at t 1.7 the port is 0.2 s ahead, the signon-timing gap below). Tests `census_force_retouch_opens_e1m6_start_door`, `force_retouch_relinks_stationary_edicts_for_two_frames`. Open: the port connects the player at sv.time 1.2, id's signon at ~1.4.
- ✅ **F9 looping mover sounds** — `queue_sounds` carries each sample's `cue ` loop window (`GetWavinfo`) through `poll_sound` (`sound_loop_start` -1 = one-shot) and no longer drops `misc/null.wav`; the page loops such a source (`SND_PaintChannels`) until a later sound on its (entity, channel) overrides it, re-spatializes it every frame from its fixed origin like the C channel, lets an inaudible sound still end its key's loop (`S_StartSound` picks the channel before the audibility test), guards the async first decode against an override that landed meanwhile, and stops every dynamic source on a level change (`S_StopAllSounds`). Tests `queue_sounds_carries_the_cue_loop_so_movers_hum_until_their_stop_sound`, `web/verify_loops.py` (demo1's first door hum loops, then its stop sound ends it).
- ✅ **L4 `time` before PlayerPreThink** — `SV_Physics_Client` sets `pr_global_struct->time = sv.time` before `PlayerPreThink` (and `SV_Physics` before `StartFrame`); the port left the thinktime of the last think. Test `player_prethink_sees_sv_time_not_a_preceding_thinktime`.
- ✅ **L20 StartFrame in every frame** — `run_frame` (the spawn settle frames) now runs `StartFrame` at `time = sv.time` like every `SV_Physics`. Test `run_frame_starts_with_startframe_like_sv_physics`.
- ✅ **L25 (half) edict loop bound** — `SV_Physics` re-reads `sv.num_edicts` every iteration, so an edict a think spawns (a missile, a gib) gets physics on its spawn frame; `run_frame`/`client_frame` fixed the bound at frame start. Test `an_edict_spawned_by_a_think_moves_on_its_spawn_frame`. Not done: player-first order (the C reserves edict 1 for the client; the port allocates the player after the map, which also offsets every edict number by one against id's) — a numbering change across spawn, connect and savegames for a sub-frame ordering effect.
- ✅ **L7 `restart` keeps only the level-entry runes** — `Server` keeps `svs.serverflags` (set with the QC global before spawn); `try_restart` respawns with it instead of the live global, as `Host_Restart_f` -> `SV_SpawnServer` does (only `SV_SaveSpawnparms`, at a changelevel, reads the global back). Test `restart_respawns_with_the_level_entry_serverflags`. As in id, a loaded save starts from a fresh `svs.serverflags` (0), since `Host_Loadgame_f` never sets it.
- ✅ **L18 world edict** — `set_map_name` (the port's `SV_SpawnServer` world setup) now also sets edict 0's `modelindex` 1, `SOLID_BSP`, `MOVETYPE_PUSH`, replacing the documented deviation; collision and `SV_PushMove` already skip edict 0 as the C does, and its pusher pass is inert (`nextthink` 0). The oracle edict diff loses its `worldspawn movetype 7/0 solid 4/0` line; the census tool's pusher list skips the world. Test `set_map_name_sets_up_the_world_edict_like_sv_spawnserver`.
- ✅ **L5 client think order** — `Host_ServerFrame` runs `SV_RunClients` (`SV_ReadClientMove` + `SV_ClientThink`) before `SV_Physics`; `client_frame` now does too, so `PlayerPreThink`'s `PlayerJump`/`WaterMove` act on the accelerated velocity (it also gives a dead player `DropPunchAngle`, which only ran for WALK/FLY/NOCLIP). A standing forward jump on e1m1 now follows id's oracle frame for frame (10 u forward per frame; it made 3). Test `census_client_think_runs_before_player_prethink`.
- ✅ **L6 ED_Alloc / ED_Free** — `Vm::spawn` reuses a free slot only if it was freed in the first two seconds of server time or more than 0.5 s ago (`freetime`), so a missile spawned the frame another is removed never inherits its slot (no stray trail); `Vm::free_edict` clears only `ED_Free`'s fields (model, takedamage, modelindex, colormap, skin, frame, origin, angles, solid; nextthink -1) and keeps the rest, as id does. Tests `ed_alloc_waits_half_a_second_before_reusing_a_freed_slot`, `ed_free_clears_only_the_fields_the_c_clears`.
- ✅ **F7 the player's name** — `connect_client_inner` sets up the client edict as `Host_Spawn_f` does before `ClientConnect`: `netname` "player" (cl_name), `team` 1 ((cl_color & 15) + 1), `colormap` = its edict number. Obituaries read "player was shot by a Grunt". Test `census_player_netname_is_player`.

## Review fixes (2026-09-25, branch `quake/polish`)

Findings of the adversarial review of the overnight merge, one line each; the
C followed and the test are in the commit message.

- ✅ **Loading a save kept no options** (MED) — `load_game` rebuilt the Menu
  (`Menu::new()`), so Screen size, Brightness, Always Run, every rebind and the
  slot listings snapped to defaults. `Host_Loadgame_f` never touches a cvar or
  `keybindings[]`; now the same `reset_nav()` as New Game. Test
  `load_keeps_every_option_and_binding` (the `viewsize 60`, `save t`, `load t`
  repro).
- ✅ **Debug-build overflow on sliver alias triangles** (LOW) — after
  `R_AliasClipTriangle` a sliver (d_xdenom of a few units under a long edge)
  gets 1/z and light steps far out of `int` range. id's `(int)` gives
  0x80000000 there (Rust's `as` saturated to 0x7FFFFFFF for positive ones) and
  its `int` sums wrap; a debug build panicked in `scan_left_edge` (`d_zi +=`,
  `d_light +=`). `polyse.rs` now converts with `c_ftoi` and wraps every step
  sum and the subdivision midpoints like the C. Tests
  `sliver_triangles_wrap_like_the_c_ints`, `float_to_int_is_x86s_not_rusts_saturation`;
  the review's stress harness (4000 random gun/entity renders with overflow
  checks) panicked at iteration 3068 before, none now. Goldens unchanged.
- ✅ **`intsintable` is not wrapped** (LOW) — `D_WarpScreen` reads
  `turb = intsintable + phase` at `turb[u]`/`turb[v]` over the whole screen, and
  `R_InitTurb`'s `3.14159` makes the table non-periodic: at i = 128, 256, ...
  the entry is 2 where a wrapped cycle gives 3. `apply_warp` indexed `& 127`;
  now it reads the unwrapped table (`intsintable`). Tests
  `intsintable_is_r_initturbs_unwrapped_table`,
  `warp_reads_intsintable_past_the_first_cycle`. (Turbulent8's `sintable` was
  already unwrapped, fid1.)
- ✅ **Underwater view at the warp buffer's resolution** — with the eye in
  water/slime/lava, id's `R_SetupFrame` renders the view into `r_warpbuffer`
  (at most `WARP_WIDTH` x `WARP_HEIGHT`, 320x200: the mode scaled to 320 wide,
  capped at 200 high, `R_SetVrect` on that with `sb_lines * h/vid.height`)
  and `D_WarpScreen` stretches it over `scr_vrect` (`wratio`/`hratio`, the
  C's float row/column tables). The port rendered and warped at full
  resolution. Now `screen::warp_vrect` + `apply_warp(view, out_w, out_h,
  clock)` (a new signature, so `render/mod.rs` is untouched), live, demo and
  `quaketool view` (which now warps an underwater eye like id's
  `R_RenderView`). Oracle, e1m1's pool (`--view=750,898,-332,0,90,0 --time
  1.6`), exact%: 320x200 97.58; 640x400 48.22 -> 97.59; 960x600 44.14 ->
  97.60; 1280x800 42.65 -> 97.63 (before = the full-resolution warp; id's
  `--spans 8` against the port's then exact perspective). *Re-measured on
  `quake/polish2`* after `compare.py` stopped taking the warp buffer's
  `r_refdef.vrect` for the screen's (it had scored this view 6.6% at 960x600
  since `quake/w2b`): against `--spans 16` 99.89 / 99.89 / 99.90 / 99.89,
  at the page's aspect (`--aspect 0.8333333`) 99.93 at all four; against
  `--spans 8` 98.53 / 98.57 / 98.54 / 98.58. Cheaper
  too: an underwater frame renders 320x152 at every preset. At every 16:10
  mode the C's pixel aspect for the warp buffer equals `vid.aspect`, so the
  square-pixel projection is exact; for a mode taller than 16:10 id squeezes
  320x200 through that aspect and the port narrows the buffer instead
  (`vid.width*200/vid.height` wide: same picture, e.g. 266 columns at 4:3).
  Whoever lands the pixel-aspect projection: the warp buffer's aspect is
  `vid.aspect * (h/w) * (vid.width/vid.height)` (R_ViewChanged). Tests
  `warp_vrect_is_r_setupframes_warp_buffer_view`,
  `warp_stretches_the_warp_buffer_over_the_screen_view`,
  `underwater_view_renders_into_the_warp_buffer`. Goldens unchanged.
- ✅ **The client clock is `cl.time`** — the live walk's `w.clock` started at
  0 and counted `dt`, so the sky, liquids, underwater warp, texture/alias
  animation and `R_AnimateLight`'s `(int)(cl.time*10)` ran about 1.2 s behind
  id's (and reset to 0 at every changelevel, restart and load). On a local
  server `CL_LerpPoint` snaps `cl.time` to the message time, `sv.time` after
  the frame's physics: `w.clock` is now `server.time()` after every server
  frame and at every walk build (spawn, changelevel, restart, load = the
  save's time), and still stops behind the menu. Particles, dlights, beams,
  the bob, rotating pickups and the intermission sway run on it too, as they
  do on `cl.time` in the C. Tests `client_clock_is_the_server_clock`, and the
  load in `save_load_round_trips_the_world_digest`. Demo playback already ran
  on the recorded `cl.time`.
- ✅ **Damage flash percent is an `int`** (review item) — already fixed on
  main by census L2 (`3ec96f4`, `cshift_add`/`cshift_drop`); the demo test
  asserts id's 22 (30 - 7.5 truncated), the ramps take the int percent.
  Nothing further to change.
- ✅ **`quaketool playtest` framing** — the POV shot drew a full-screen view
  (viewsize 120's framing) with the viewsize-100 gun fudge and pasted the
  48-line bar over it, so the gun sat under the bar. Now it is the game's
  default screen: `calc_refdef` at viewsize 100, the view above the bar,
  `compose_view` with the backtile, the gun offset for the same viewsize.
  Test `playtest_frames_the_view_for_the_guns_viewsize`.
- ✅ **Stale comments** — `render_scene_ext`'s doc (the gun "uses its own
  depth buffer", "a plain `[f32; 256]`" sine table, "walls, alias models and
  the viewmodel ignore `time`"), `Viewmodel`'s ("no world origin", "view
  space", frame "clamped"), and the `V_CalcBlend` mentions in `cl_walk.rs`,
  `cl_demo.rs` and the README (the shifts are the software `V_UpdatePalette`
  ramps). Comments only (`render/mod.rs` touched for its doc comment alone).
- ✅ **Census L11: prints reach the console scrollback** — `Con_Print`
  writes the console's text buffer, and the notify lines are its last
  lines; the port showed `svc_print` text only in the notify overlay. Now
  `quake_rs::console::ConCursor` is Con_Print's layout (`con_x`, the word
  wrap at `con_linewidth` 38 — a word longer than a line runs on until its
  remainder fits — `\n`, `\r`), shared by the notify lines and
  `Console::print`; `println` is `Con_Printf("%s\n")` through it, so
  console output wraps too and continues a line a print left open. The
  dispatcher hands each frame's printed text (live or demo) to the console.
  Tests `console_print_is_con_print`, `game_prints_reach_the_console_scrollback`
  (e1m1's shells pickup). ~~Still open: console command output does not reach
  the notify lines~~ — moot: `Con_ToggleConsole_f` zeroes `con_times`, so what
  was printed while the console was down never shows as a notify line in the
  C either (branch `quake/fid2d`, below).

## The 2-D layer against id's composited screen (2026-09-25, branch `quake/fid2d`)

`oracle/screen2d.py` (see `oracle/README.md`, "The 2-D layer") plays the same
scenario through id's WinQuake and the port's live App with the 3-D view one
flat colour on both sides, and diffs the screens: 63 shots x 3 modes. Before
the branch the shots' 2-D pixels matched 37-100% at 320x200 and 0-100% (mostly
under 55%) at 640x400 and 960x600; after, 57 of 63 shots in each mode are
exact and the other six are four explained residues (95.5-99.9%). Goldens unchanged
(`959d0221`/`0cd18471`/`b63ae8b7`, 3-D only).

- ✅ **Ammo counts 4 px left** (`8b960c2`) — `Sbar_DrawCharacter` draws at
  `x + ((vid.width - 320)>>1) + 4` (sbar.c:293); the port's inventory counts
  used the bare x. Test: the shells count starts at x = 8 - 2 + 4.
- ✅ **"quake-rs" under the Main / Single Player menus** (`77a0664`) — not in
  `M_Main_Draw` (menu.c:291). Removed.
- ✅ **A note line under Multiplayer** (`f20d28b`) — `M_MultiPlayer_Draw`
  (menu.c:635) prints only "No Communications Available". Removed.
- ✅ **Centerprints one row low** (`4a92962`) — `y = vid.height*0.35`
  (screen.c:142, and `SCR_DrawNotifyString`'s) is `(int)` of a product the x87
  computes in extended precision, where the double 0.35 leaves it a hair under
  70: row 69 (the oracle's x87 build draws 69, its SSE build 70). The port took
  70. `center_string_top` is the exact integer floor; the plain centerprint now
  goes through the finale's `SCR_DrawCenterString` port (its 40-column scan).
- ✅ **The 2-D layer blown up in every larger mode** (`cdfd57f`) — a default
  departure: WinQuake draws the 2-D layer 1:1 in every video mode — the bar 320
  wide at the bottom centre with backtile either side (`Sbar_Draw`,
  sbar.c:938), menus across the top centre (`M_DrawPic`'s
  `(vid.width-320)>>1`), the intermission at fixed screen coordinates, a 48-row
  bar the view clears, `(int)(48*h/vid.height)` rows of the underwater warp
  buffer, a console `vid.width/8-2` characters wide. The port scaled id's
  320x200 screen to fill the framebuffer. `draw::screen_2d` is now the screen
  id's code lays out on: the framebuffer at scale 1 by default, or with the new
  **"scaled 2-D" extra** (`draw::set_scaled_2d`, wasm export `set_scaled_2d(1)`)
  the old 320x200 blow-up. 640x400: 1-55% of 2-D pixels -> 99-100%. **Needs
  wiring** into the page's extras (not done here: the Extras menu and the
  page's persistence are another branch's); until then the browser's default
  960x600 shows id's small bar and menus.
- ✅ **The console** (`d27eebb`) — a fixed 60% panel that snapped on and off,
  the conback's top rows squeezed into it, its own text layout. Now
  `SCR_SetUpToDrawConsole` (screen.c:458: `scr_con_current` slides 300 rows a
  second to `vid.height/2` and back), `Draw_ConsoleBackground` (draw.c:539: the
  conback's BOTTOM rows, the version stamped in with `0x60 + texel` — the DOS
  build's "1.09"), `Con_DrawConsole` (console.c:580: `(lines-16)>>3` rows from
  `lines-16-rows*8`, x = `(col+1)*8`) and `Con_DrawInput` (the prompt line at
  `lines-16`, prestepped past `con_linewidth`); closing clears the typing
  (`Con_ToggleConsole_f`). `Con_CheckResize`: the console and notify text are
  laid out `con_linewidth = (vid.width>>3)-2` wide (was 38 in every mode).
  320x200: 37% -> 99.1% (the rest: the oracle's Linux version string).
- ✅ **The Quit prompt** (`d6c0fe1`) — an invented black box asking "Are you sure
  you want to quit?". Now `M_Quit_Draw` (menu.c:1674, the non-Windows builds,
  i.e. DOS Quake's): the menu it rose over redrawn without a second fade
  (`wasInMenus`, `m_recursiveDraw`), `M_DrawTextBox (56, 76, 24, 4)` from the
  `gfx/box_*.lmp` pieces (menu.c:181), one of the eight `quitMessage` taunts
  (`msgNumber = rand()&7`) in `M_Print`'s bronze; No/Escape return to that
  menu, or to the game. 66% -> 100%. (WinQuake on Windows showed a credits box.)
- ✅ **The console lingered after `map` / `load`** (`2b024a2`) —
  `SCR_BeginLoadingPlaque` zeroes `scr_con_current`: it goes at once.
- ✅ **Notify lines survived a console toggle** (`0cdcb1a`) —
  `Con_ToggleConsole_f` zeroes `con_times`. Test
  `toggling_the_console_clears_the_notify_lines`.

Found, not fixed (outside the 2-D drawing, or another branch's file):
- ✅ (`quake/polish2`, "Second review fixes") **Menu cursors are not remembered** — id keeps one per menu
  (`m_main_cursor`, `m_singleplayer_cursor`, `options_cursor`, `load_cursor`
  shared by Load and Save, ...): Escape from Options lands on "Options", the
  port's single cursor on "Single Player". `menu.rs` behaviour, left for after
  the Extras-menu work (screen2d `menu_sp.sp_again`, 206 px).
- ✅ (`quake/polish2`, "Second review fixes") **`sv.time` adds up in f32** (`sv_phys.rs`: `gset_float("time", start_time +
  dt)`); id's `sv.time` is a double copied into the QuakeC float each frame.
  After 60 frames of 0.1 s the port reads 7.2999954, so the new-weapon flash
  (`(int)((cl.time - item_gettime)*10)`) is a frame behind (screen2d
  `flash.rl_new`), and the error grows with the session (at an hour, f32 steps
  are ~0.24 ms of a 13.9 ms frame).
- **Default key binds are WASD** (`keys.rs`: w/s/a/d/c over `default.cfg`'s
  a = +lookup, d = +moveup) — a default departure besides Always Run.
- **No pause** — `pause` (default.cfg binds PAUSE) and `SCR_DrawPause`'s plaque;
  **no loading plaque** (`SCR_DrawLoading`).
- **`give` is not `Host_Give_f`** — it clamps, has an `a` (armour) case,
  defaults a missing amount to full, and selects the weapon it gives; id sets
  the field to `atoi(argv[2])` (0 when missing) and only ORs the weapon bit.

## Projection and spans (2026-09-25, branch `quake/w2b`)

**On the merged base** (`quake/overnight` `bbfc6bc`: the mip levels, the census and review fixes), all
four items: goldens e1m1 `959d0221` → `4807aaa1` (4983 px, 1.95%), e1m2
`0cd18471` → `8ce25660` (5808 px, 2.27%), e1m3 `b63ae8b7` → `3531e9cd` (4077 px,
1.59%) — all from the 16-pixel spans (the aspect and the sky centre leave the
square-pixel full-screen scene alone). Oracle world, 320x200, against id's x86
spans (`--spans 16`): 92.84 / 94.36 / 96.56 / 87.20 → **99.96 / 99.21 / 99.98 /
99.91**; at the page's aspect (`--aspect 0.8333333`) 100.00 / 98.73 / 100.00 /
100.00 and 100.00 at 640x400; against id's portable C (`--spans 8`, the
default) 97.44 / 97.12 / 99.16 / 94.96 → 94.57 / 96.09 / 97.27 / 91.72 (8 against
16 now). The exact extra reproduces the merged base byte for byte. The
paragraphs below give each item's own before/after as measured on the old
base.

Measured with `oracle/compare.py` (exact-palette-index match %, 320x200,
world, e1m1/e1m2/e1m3/e1m7 unless stated).

- ✅ **Pixel aspect** (the second half of H6, PERF_PLAN A4). Every render
  preset is 16:10 and the page always shows the canvas at 4:3, as DOS Quake's
  320x200 filled a 4:3 monitor; the port projected square pixels, so the whole
  3-D view was shown 1.2x too tall. id folds the display into the projection:
  `vid.aspect = (h/w)*(320/240)` (vid_win.c) is `R_ViewChanged`'s
  `pixelAspect`, `yscale = xscale*pixelAspect`, and the frustum, the alias
  scales, sprites and particles all take it. Now `render::RenderOptions::
  pixel_aspect`, one `Projection` for every pass (world, submodels, external
  boxes, alias models and the gun, sprites, particles, the frustum); the wasm
  shell passes `vid_aspect(w, h, 4/3)` — 0.8333 at every preset. The sky
  keeps its screen-pixel mapping (`D_Sky_uv_To_st` has no aspect), and a
  particle stays a pixel square (`d_y_aspect_shift` is 0 below 1.4).
  - **Oracle** (`--aspect 0.8333333` now reaches both renderers; id at mip 0
    and exact perspective): 31.35 / 26.90 / 20.23 / 14.23 (the port square)
    → 95.89 / 97.60 / 98.50 / 98.79, against 95.61 / 97.40 / 98.50 / 98.66
    for both square. Entity pixels 100 / 100 / 98.8 / 100; the e1m2 altar
    view (id as shipped) 100% of 684 entity pixels; with the gun
    (`--viewmodel --settle 3`) 94.44 / 97.21 / 98.62.
  - **Goldens unchanged** (`quaketool scene` writes a square-pixel PPM:
    aspect 1). At aspect 1 every pass is bit-identical to before.
  - Page screenshots before/after at 320x200 and 960x600 (e1m1 spawn): the
    rivet grid on the walls, square in the texture, was 1.17:1 tall and is
    1:1; the view shows 1.2x more vertically, and the gun is DOS Quake's.
- ✅ **16-pixel perspective spans** (oracle class 7; the LOW "affine span
  subdivision"). The shipped x86 WinQuake drew the surface cache with
  `D_DrawSpans16` (`d_draw16.s`, `d_subdiv16` 1): exact perspective every 16
  pixels, affine in between; the port divided at every pixel. Now
  `raster.rs`'s `span16_cached` is the asm's integer algorithm: the 16.16
  coordinates exact at a span's first pixel (clamped to `[0, bbextents]`) and
  at each full segment's end (clamped to `[4096, bbextents]`), stepped by
  `(snext - s)/16` with the 20 fractional bits the asm carries; the last
  segment lands on the span's last pixel with `reciprocal_table_16`'s
  `floor(ds * R[n] / 2^31)`. Liquids follow `Turbulent8` (C in both builds):
  16-pixel segments, `>> 4` steps, the C division on the last one, the
  `(CYCLE << 16) - 1` mask per segment. And the spans are id's: `R_ScanEdges`
  cuts a surface's row at every nearer surface's edge, so the 16-pixel grid
  restarts where a surface becomes visible; the port draws front to back
  against its z-buffer, so a span is a run of pixels passing the z test. Sky
  stays on its 32-pixel `D_DrawSkyScans8` runs, found the same way.
  - **Oracle, id at mip 0** (the mip class removed), `--spans 16` on id's
    side: 88.74 / 89.99 / 93.57 / 85.62 → **95.61 / 97.43 / 98.51 / 98.68**,
    the same as exact against exact (95.61 / 97.40 / 98.50 / 98.66); 640x480
    93.69 / 93.85 / 96.57 / 95.01 → 95.93 / 97.60 / 98.50 / 98.75. Without the
    z-test runs (the 16-pixel grid anchored at the polygon's left edge) e1m1 /
    e1m2 read 94.40 / 95.20: the remainder was stripes on partly hidden walls.
    Liquid pixels of an oblique teleporter (e1m1, `--view=1260,1050,-368,0,60,0
    --time 3.25`): 92.15% → 100.00%. Floors and pools, whose 1/z is constant
    along a row, were already exact.
  - **Id as shipped on x86** (`--spans 16`, id's own mips): 80.37 / 63.78 /
    65.41 / 67.94 → 86.95 / 64.29 / 66.14 / 80.35; against id's portable C
    (`--spans 8`, the oracle's default) it falls, as it must: 84.74 / 64.06 /
    65.88 / 75.62 → 82.04 / 63.79 / 65.56 / 72.23.
  - **The oracle's `--spans 16`** was a C re-creation with `D_DrawSpans8`'s
    integer steps (`>> 4`, clamp 16, division); it now has the asm's (the
    exact 1/16 steps, clamp 4096, `reciprocal_table_16`). The two differ on 0 /
    6 / 40 / 40 pixels of the four standard frames; the port with either
    arithmetic matches id within ±0.05 points, the size of id's float noise.
  - **Goldens** (640x400): e1m1 `bb64996e` → `2023d7d9` (5025 px, 1.96%),
    e1m2 `8186a64c` → `1ac070d0` (7612 px, 2.97%), e1m3 `f41e8b59` →
    `cc121e29` (5218 px, 2.04%) — texel steps inside 16-pixel segments.
  - **Brush entities before the world.** id's bmodel faces are in the
    world's edge list, so an item box or a door cuts the spans of the wall
    behind it; the port now draws brush entities first, so they cut the
    world's z-test runs too. e1m3's standard entity frame (the shells box):
    99.94 → 99.98% against `--spans 16` (the wall right of the box had
    16-pixel stripes); 61 views facing doors, plats and buttons on e1m1-e1m3
    unchanged; the exact extra unchanged. Goldens (on the merged base with
    the mip levels): e1m1 `74522852` → `4807aaa1` (288 px), e1m2 `8ce25660`
    unchanged, e1m3 `ce0f5c89` → `3531e9cd` (13 px).
  - **Speed** (on the merged base): wasm world −16 to −23% (demo1 at 1280x800
    4.11 → 3.30 ms, walk_e1m3 3.35 → 2.73), step −5 to −13%; native world
    +30-40%. PERF_PLAN §6 has the table.
  - The old per-pixel perspective stays as an opt-in extra
    (`RenderOptions::exact_perspective`, `quaketool view --exactpersp 1`,
    `compare.py --exactpersp`): byte-identical to before (the oracle's exact
    rows are unchanged). The uncached per-pixel wall path (faces over the
    surface-cache size cap, or no colormap — never in id's maps) stays exact.
- ✅ **`wasm_exactpersp 0|1`** (an extra, not id; default 0). The browser
  reaches the exact-perspective renderer option through a console variable.
  All of the port's opt-in extras live in one place, `quake-wasm/src/
  extras.rs`: a table of `wasm_*` cvars that behave like id's (`wasm_x` prints
  `"wasm_x" is "0"`, `wasm_x 1` sets it, `Q_atof` semantics), process state like
  id's cvars (not saved), read by `vid::render_options` each frame; `help`
  lists them. An extras menu can drive the same table. Always Run, the one
  default departure, stays with the menu options (it is id's own setting).
  (Since the merge with `quake/extras`: the values live in the menu, on
  Options > Web extras, and the page persists them; `extras.rs` keeps the
  cvar table and hands the renderer each frame's copy. See "Web extras".)
- ✅ **Sky centre below viewsize 120** (the open item of oracle class 4).
  `D_Sky_uv_To_st` centres the sky on the SCREEN (`u - (vid.width>>1)`,
  `(vid.height>>1) - v` in screen pixels); the port centred it on the view
  rectangle — 24 rows off at the default viewsize 100, 12 at 110, both axes
  inside a border. `RenderOptions::screen` (the vrect's corner on the
  `vid_w x vid_h` screen) now gives `SkyView` the screen's centre in the
  view's pixels; the page passes it every frame. The oracle compares views
  below 120 now (`compare.py --viewsize N` hands id's vrect to `quaketool
  view --vrect` and crops id's frame to it). e1m2 at `--spans 16`: the sky
  region matches 75.1 / 46.4 / 43.4% at viewsize 100 / 110 / 70 before, 100%
  after; the whole view 98.90 → 99.97, 97.56 → 99.56, 95.98 → 99.03 (what is
  left at 110 and 70, and at 120 too — 99.21 — is a ceiling face at the top
  right (a face at a finer mip in id than its geometry gives, oracle class
  1's open note) and one floor edge, the same with exact perspective on the merged
  base). Goldens unchanged (the scene view is the screen).
- **Underwater** (with `quake/polish`'s warp buffer): an underwater view is
  rendered at `warp_vrect`'s size with the screen's pixel aspect (for a
  16:10 mode id's `vid.aspect*(h/w)*(vid.width/vid.height)` is `vid.aspect`)
  and the sky placed as id places it — `D_Sky_uv_To_st` measures the warp
  buffer's pixels from the SCREEN's centre, so above 320x200 an underwater
  view of the sky is off-centre in WinQuake too, and here. Open: for modes
  taller than 16:10 (not a preset) `warp_vrect` narrows the buffer because
  the port had square pixels; with `pixel_aspect` id's 320-wide buffer and
  its aspect are now possible (not done).

## Web extras: the opt-in departures (2026-09-25, branch `quake/extras`)

The rule: faithful by default, Always Run the only default departure; other
departures live behind an explicit opt-in. Their home is **Options > Web
extras**, a 14th Options row in the slot id's `_WIN32` build gives its own
14th row ("Use Mouse", y=136). It opens a page of Options drawn in
`M_Options_Draw`'s idiom (qplaque + OPTIONS title, a white "Web extras: not
in id's Quake" header, `M_Print` labels, on/off at x=220, the 4 Hz cursor,
menu1/2/3, Esc back to the same Options row) with help lines for the
highlighted row. Each extra is also a console variable. One table lists them
(`render::WEB_EXTRAS`: the value, its `wasm_*` name, its row label and help);
one place holds their values (the menu's `Extras`, what the page draws and
persists); `quake-wasm/src/extras.rs` (from `quake/w2b`) is their console
side and hands the renderer each frame's copy. None is a default.cfg cvar,
so Reset to defaults and re-boots keep them; the page persists them in
localStorage. With every extra off the port is unchanged (goldens, all seven
earlier verify scripts).

| extra | switch | what | status |
|---|---|---|---|
| Uncapped framerate | `wasm_uncapped 0\|1` | `Host_FilterTime` without its 72 fps gate (same [0.001, 0.1] clamps): a host frame per display refresh (120/144 Hz run 120/144 fps). The gate itself is unchanged. | departure, opt-in via Web extras, default off |
| Show FPS | `wasm_showfps 0\|1` | QuakeWorld's `SCR_DrawFPS`: `"%3d FPS"` in white conchars at `vid.width - len*8 - 8`, `vid.height - sb_lines - 8`, not on intermission screens. The rate is presented frames over a window of at least 1 s of `realtime` (QW shows the raw count; count/window reads a steady 60 instead of 60/61). | departure, opt-in via Web extras, default off |
| Exact perspective | `wasm_exactpersp 0\|1` | exact perspective at every pixel of the textured walls and liquids instead of id's 16-pixel spans (`RenderOptions::exact_perspective`, `quake/w2b`'s). | departure, opt-in via Web extras, default off |

Faithful, same branch:
- ✅ **viewsize persists across reloads** — id's `scr_viewsize` is archived
  (config.cfg); since Screen size stopped being the resolution it reset on
  every reload. The page stores it like the resolution (`set_viewsize`).
- ✅ **Esc ignores autorepeat** — `Key_Event` ignores repeats of every key
  but backspace and pause; the page's Esc toggled the menu on each repeat.
- ✅ **Esc in fullscreen** — the page locks Escape with the Keyboard Lock API
  while fullscreen (Chromium), so a tapped Esc toggles the menu like id's
  `togglemenu` with the mouse still captured, and a held Esc leaves
  fullscreen; elsewhere the browser's two-step Esc stands. Unverified: a real
  browser's handling of a locked Esc (headless has none); the page logic is
  tested.

Evidence: quake-rs lib tests for the Extras screen (defaults, toggles, sounds,
Esc row, layout and colours, bits) and `draw_fps` placement; quake-wasm tests
for `host_frame_time` (every refresh at 60..240 Hz, clamps), 72 vs 144 frames
a second at 144 Hz through `step`, the FPS window, the readout confined to its
box and byte-identical when off, the `wasm_*` commands and the exports;
`web/verify_extras.py` 36/36.

## Entity culling and resolved fields (PERF_PLAN C1, D2; 2026-09-25, branch `quake/sim`)

- ✅ **C1: the live client relinks only what the server sends** (`SV_WriteEntitiesToClient`,
  `CL_RelinkEntities`). The port used to relink and draw every edict with a model, every frame.
  That lit walls from behind (CENSUS L22), trailed and spun what nobody could see, and drew 98
  alias models per frame on e1m3 for about 3 visible.
  - Now `SV_LinkEdict` records the edict's leaves (`SV_FindTouchedLeafs`, at most
    `MAX_ENT_LEAFS` = 16, front child first). `SV_FatPVS` unions the PVS of every leaf within 8
    units of the player's `origin + view_ofs`. An entity is sent when it has a `modelindex`, a
    non-empty `model` and a leaf in that set; the player is always sent. `step_walk` gives
    `EF_*` lights, trails, `EF_ROTATE` and drawing only to those. An entity that drops out
    loses its trail history, as `CL_ParseUpdate`'s forcelink restarts it.
  - `PF_makestatic` marks the edict a static. id frees the edict into the signon; the port keeps
    it, because edict numbering and savegames follow it. A static is never relinked. It is drawn
    when a leaf of its `R_AddEfrags` box is in the PVS of the leaf holding the view origin
    (`R_MarkLeaves`, not fattened), after the relinked entities, as `R_StoreEfrags` appends it.
    The box is ±16 for alias models (`Mod_LoadAliasModel`'s "FIXME"), ±maxwidth/2 and
    ±maxheight/2 for sprites, and the model's bounds for brush models.
  - Evidence: frame hashes are identical on four bench workloads (PERF_PLAN C1). The goldens,
    the simbench counts and the census output are unchanged. Tests:
    - `an_entity_outside_the_fat_pvs_is_not_drawn_and_its_flash_lights_nothing`: L22, with a
      control that the light would have lit the wall.
    - `an_entity_in_view_is_drawn_and_its_flash_made`.
    - `statics_draw_through_efrags_in_the_view_pvs`.
    - `touched_leafs_*`, `edict_leafs_cap_at_max_ent_leafs`, `leaf_pvs_*`,
      `fat_pvs_unions_the_leaves_within_8_units`.
  - Accepted gaps:
    - Statics use the edict's float origin and angles, not `svc_spawnstatic`'s coord and angle
      bytes.
    - `R_RecursiveWorldNode`'s frustum test on an efrag's leaf is left out.
      `R_AliasCheckBBox` and the z-buffer hide the same pixels, unless a mesh reaches past its
      efrag box.
    - `SV_WriteEntitiesToClient`'s "packet overflow" cutoff (`MAX_DATAGRAM`) is not modelled.
    - Demo statics are still drawn without the efrag test. The output is the same; only the cost
      differs.
    - The `quaketool scene` tool view draws every entity.
  - Seen, not changed: the port's `SV_ClipToLinks` "points never interact" test uses
    `maxs - mins` where the C reads `v.size`. The two differ only if QuakeC writes
    `mins`/`maxs` without `setsize`.
- ✅ **D2: entity fields are resolved once per progs, not hashed per access.** This is
  byte-identical.
  - id reads `entvars_t` members at fixed offsets. The port looked every field up by name,
    SipHash into a `HashMap<String>`, per access: 29,300 lookups per frame on walk_e1m3. 79% of
    them were the fields `SV_Move`'s per-trace scan reads for every edict.
  - `Vm::fo` (`FieldOfs`: every `entvars_t` field, and `gravity`) and `Vm::go` (the per-frame
    globals) now hold `Fld`/`Glb` handles, resolved by name in `Vm::new`, so any progs works.
    The by-name accessors resolve the name and then run the same code, so the semantics cannot
    drift. A field the progs lacks reads 0 and drops writes.
  - The server's per-frame paths, the edict-scanning builtins and the client gather use the
    handles. The model-cache loop borrows names instead of allocating a `String` per edict per
    frame, and `PF_find` compares borrowed strings. 90 lookups per frame remain: once-per-frame
    reads of the player and the HUD.
  - Proof: the simbench counts, the census output, the `census-edicts` dumps of nine maps at
    five times, the goldens, and 720 frame hashes across five workloads and two resolutions are
    all unchanged. Test `resolved_fields_match_the_by_name_accessors`.
  - Speed: simbench e1m3 2.64 → 0.29 ms per tick. The wasm sim phase is 68–76% lower, and the
    walk_e1m3 frame goes 1.39 → 0.77 ms at 320×200 (PERF_PLAN D2).

## Second review fixes (2026-09-25, branch `quake/polish2`)

Findings of the second adversarial review that live outside `quake-wasm/src/`
(another branch is moving the client there); one line each, the C and the
test in the commit message.

- ✅ **Oracle: underwater views above 320x200 compared the wrong rectangle**
  (MED, tooling) — `compare.py` took the `.json`'s `vrect` (`r_refdef.vrect`)
  for the view's place on the screen. With the eye in a liquid above 320x200
  that is the rectangle in `R_SetupFrame`'s warp buffer (0,0,320,200), not
  the screen's, so it cropped id's frame to its top-left corner and asked the
  port for an unwarped 320x200 view: e1m1's pool scored 6.6% at 960x600. It
  now takes `scr_vrect` (already in the `.json` since `quake/fid2d`), which
  `D_WarpScreen` stretches the buffer over, and warns for an underwater view
  below viewsize 120 (`quaketool view --vrect` draws it unwarped). The pool,
  `--spans 16`: 99.89 / 99.89 / 99.90 / 99.89 at 320x200 / 640x400 / 960x600 /
  1280x800; 99.93 at all four at the page's aspect. The stale 97.6 numbers in
  "Review fixes" are corrected there.
- ✅ **Particles drawn as `D_DrawParticle`** (LOW) — `draw_particles` projected
  with the walls' `xscale`/`yscale` where `R_DrawParticles` scales `vright`/
  `vup` by `R_ViewChanged`'s `xscaleshrink = (vrect.width-6)/
  horizontalFieldOfView` (3 px nearer the centre at a 320-wide view's edge);
  it also sized a centred square by `round(focal/z)`, clipped it at the edge
  and z-tested float depth. Now `render/part.rs` is `D_DrawParticle`
  (`d_part.c`) with `D_ViewChanged`'s constants (`d_modech.c`): `u =
  (int)(xcenter + zi*x + 0.5)`, `pix = (int)(zi*0x8000) >> d_pix_shift`
  clamped to `[d_pix_min, d_pix_max]`, drawn from `(u, v)` right and down,
  dropped whole past `d_vrectright_particle`/`d_vrectbottom_particle`,
  `PARTICLE_Z_CLIP` 8, and id's `pz <= izi` on the quantized 1/z (particles
  of one burst tie often; the later wins). Measured: the oracle now dumps
  the particles it draws (`.parts`) and `quaketool view --particles` draws
  them; the shotgun's puffs on e1m1 match 100% of particle pixels at 320x200
  / 640x400 / 960x600 (were 12.8 / 12.6 / 15.0%). Tests
  `particles_project_with_r_main_c_xscaleshrink`,
  `particle_size_is_d_part_cs_izi_shift`,
  `particles_obey_d_part_cs_clip_and_edges`,
  `particles_tie_on_d_part_cs_quantized_1_over_z`. Goldens unchanged (no
  particles in them).
- ✅ **`sv.time` is a double** (fid2d's leftover) — id's `server_t.time` is a
  `double` (server.h), advanced by `SV_Physics`' `sv.time += host_frametime`
  (a double too), and QuakeC sees its float through each
  `pr_global_struct->time = sv.time`. The port kept the clock in the QC float
  global and added `dt` in f32: 7.2999954 after 63 frames of 0.1 s from 1.0,
  so the new-weapon flash's `(int)((cl.time - item_gettime)*10)` showed the
  frame before (screen2d `flash.rl_new`), and the error grows with the session.
  Now `Vm::sv_time` is an f64 (`Server::sv_time`), `Server::time()` its float
  (the QC global's and `svc_time`'s value), and the double comparisons are
  double: `SV_RunThink`'s `thinktime > sv.time + host_frametime` and
  `thinktime = sv.time`, `SV_Physics_Pusher`'s `ltime + host_frametime`,
  `SV_ClientThink`/`SV_WaterJump`'s `teleport_time`, `ED_Alloc`/`ED_Free`'s
  `freetime`. New entry points `run_frame_f64`/`client_frame_f64` take id's
  `double host_frametime`; the f32 ones widen theirs (quake-wasm's, for now).
  The settle frames and the quaketool drivers (census, census-edicts,
  simbench, sim, playtest, changelevel) run id's exact 0.1. Savegames: written
  `%f` from the double (`Host_Savegame_f`), read into a float and restarted
  from it (`Host_Loadgame_f`'s `float time`). Tests
  `sv_time_is_a_double_and_the_qc_time_global_its_float`,
  `a_think_is_due_by_sv_time_plus_host_frametime_in_double` (a think at
  `time + 0.1` = 7.4f waits a frame at sv.time 7.3, as in the C; the f32 sum
  ran it early), the save tests (`1234.567890` written, the float read back).
  Evidence: screen2d `flash.rl_new` 98.3 / 99.2 / 99.4% -> 100% at 320x200 /
  640x400 / 960x600. id's edicts (the oracle's `oracle_edicts`, nine maps at
  t = 1.7 ... 120.7) against `census-edicts`: `nextthink` mismatches over
  every non-monster edict 310 -> 252, none worse (every map's player idle
  think now on id's phase, e1m1's two start doors at 4.040/4.035, e1m4,
  e1m6, e1m8 movers); the dump at 120.7 no longer reads 120.699. simbench
  and the census change as a one-frame think phase shift propagates through
  random-driven fights (e1m1, e1m3, e1m5-e1m7 simbench counts identical;
  census: no new fault, error or never-moved mover). **Goldens re-baselined**
  (e1m1 unchanged): e1m2 `8ce25660` -> `9ae2b478`, e1m3 `3531e9cd` ->
  `2ca0f916`. The scene is taken after `SV_SpawnServer`'s two settle frames,
  at sv.time 1.2; an item's `PlaceItem` think at `time + 0.2` = 1.2f =
  1.2000000477 is later than 1.1 + 0.1 in double, so it has not run yet and
  the shells box (e1m2) and an ammo box (e1m3) stand where the map put them,
  as in id's (its edict dump at t = 1.2: every item's nextthink 1.200, not yet
  dropped). The f32 sum 1.1f + 0.1f = 1.2f ran it a frame early.
- ✅ **`sv_gravity` outlives the map; the gravity checks can fail** (LOW) —
  `Server::new` reset the cvar to 800 before worldspawn ran, so the census
  test's "the next map's worldspawn sets it back to 800" (e1m5 after e1m8,
  `quake-wasm/src/census_tests.rs`) and the unit test's "a fresh server
  starts at the default" held whatever worldspawn did. id's cvar outlives the
  map (`SV_SpawnServer` never touches it; id1's worldspawn sets it on every
  map). The reset is gone; the unit test (`sv_gravity_cvar_drives_add_gravity`)
  now checks the next server keeps 100 until `cvar_set("sv_gravity", "800")`,
  and the census test fails if that `cvar_set` is dropped (checked by breaking
  it: 100 != 800). Savegame loads run worldspawn (`Host_Loadgame_f` ->
  `SV_SpawnServer`), so an e1m8 save still loads at 100.
- ✅ **Stale docs** — README and quake-rs/README said save/load was out of
  scope and counted 458 + 48 tests; the verify-script list, CENSUS L11's moot
  "still open", L15 (partly fixed by F9, not marked; the per-side clamp after
  the master volume and the one-shots' per-frame spatialisation are still
  open, `web/index.html` `playRouted`), the census tests described as
  `#[ignore]`d, and the fix-client L11 / fix-server F3 lines. Not changed,
  another agent's files: `render/surf.rs`'s `mipadjust` comment is backwards
  (it says a texture scaled up in the editor "drops to a coarser mip sooner";
  its short axes give `mipadjust` 4, which raises `nearzi * scale_for_mip *
  mipadjust`, so it keeps a FINER level longer — and it counts world units
  per texel, not texels per unit); `quake-wasm/src/census_tests.rs`'s module
  doc still says `#[ignore]`.
- ✅ **Menu cursors kept per menu** (fid2d's leftover) — `menu.c` keeps one
  static cursor per menu and no `M_Menu_*_f` resets it (only `M_Menu_Help_f`
  sets `help_page = 0`): Escape from Options lands on "Options", Load after
  a load opens on that slot, the Quit prompt returns to its screen's row.
  The port had one cursor that every screen change put back on row 0.
  `menu.rs` now keeps `m_main_cursor`, `m_singleplayer_cursor`,
  `load_cursor` (Load and Save share it), `m_multiplayer_cursor`,
  `options_cursor`, `keys_cursor`, `vid_line` and the Web extras page's own;
  Help and Quit have none. `vid_line` is a static in `vid_dos.c` too; id's
  starts on the list's first line (the live mode in a default DOS setup),
  the port's list is its own, so the first visit opens on the live mode and
  later ones where the player left it. `reset_nav` (the host's boot) puts
  them all at 0, a program start. Tests
  `each_menu_keeps_its_cursor_like_menu_cs_statics`,
  `a_closed_menu_reopens_on_m_main_cursor`,
  `help_starts_on_page_0_and_quit_returns_to_the_screens_cursor`,
  `video_opens_on_the_live_mode_then_keeps_vid_line`,
  `reset_nav_is_a_program_start_for_the_cursors`. screen2d
  `menu_sp.sp_again` 99.62 / 99.90 / 99.95% -> 100%; every menu shot now
  matches in all three modes except the two explained ones (Options' Web
  extras row, Video's mode list). `web/verify_menu.py`
  follows id's cursors (61/61). **Two quake-wasm tests fail with this
  commit** (they re-navigate assuming the reset; quake-wasm is off limits
  here): see the list below. The commit is the branch's last.

Left for the quake-wasm pass (found here, not changed: `quake-wasm/src/` is
being moved into `quake-rs/src/client/`):
- The live host hands `client_frame` an f32 `dt`, which the server widens;
  call `client_frame_f64` with `Host_FilterTime`'s double `host_frametime`, so
  `sv.time` adds exactly id's frame times.
- `census_tests.rs`'s module doc says the tests are `#[ignore]`d; all twenty
  run in the normal suite.
- The page's sound law (not quake-wasm, but the same pass): `playRouted` /
  `spatializeDynLoop` clamp each side after the master volume (id clamps
  `leftvol`/`rightvol` at 255, `snd_mix.c`, then scales by `volume`), and
  non-looping one-shots are not re-spatialised each frame (`S_Update` runs
  `SND_Spatialize` on every channel) — CENSUS L15's open half.
- Tooling: `quaketool view --vrect` draws an underwater view unwarped
  (`compare.py` warns); the warp at viewsize below 120 is not compared.
- Menu cursors (below): two quake-wasm tests navigate assuming the old reset
  (`host::tests::gamma_changes_the_presented_frame_and_one_is_byte_identity`,
  `input::tests::invert_mouse_flips_pitch_and_lookspring_recentres_on_unlock`:
  after reopening the menu they press DOWN twice for "Options", which from
  the kept "Options" is Quit); each needs its re-navigation shortened to
  `menu_cancel(); menu_select();` (+ one DOWN for Lookspring). And
  `Menu::reset_nav` is shared by `boot()` (a program start, where menu.c's
  statics are 0) and New Game / load / `map` (where id keeps every cursor):
  split it so only the boot resets the cursors (app.rs's re-boot test pins
  cursor 0 after `boot()`).
- ✅ **Census: e1m8's `*6` reported "never moved"** (LOW, tooling) — the mover
  baselines were taken after the signon frames, by which time
  `PutClientInServer`'s `force_retouch` had opened the door (an ogre stands
  in its trigger field, CENSUS F8). Baselines are now taken right after the
  spawn, before the player connects: e1m8 "19 total, 17 moved, never moved:
  func_wall#111(*16) func_wall#159(*27)", `*6` among the movers already
  moving at the idle; no other line of the census changes.
- ✅ **The Load menu was empty after a page reload** (LOW, pre-existing) —
  `refreshSaveComments()` (the page's `M_ScanSaves`: localStorage's
  `s0..s11.sav` into the menu's slot comments) ran only after a save,
  though its comment said "at boot"; a reloaded page listed every slot as
  `--- UNUSED SLOT ---` and refused to load them. id rescans each time the
  screen opens (`M_ScanSaves` in `M_Menu_Load_f` / `M_Menu_Save_f`). The page
  now scans at boot and whenever the menu enters Load or Save
  (`menu_visible` / `menu_screen_id`, checked before the frame that draws
  it); existing exports only. `web/verify_save.py` also saves to slot 0,
  reloads, forgets the boot scan and opens Load: slot 0's row is the save's
  comment and Enter loads it (the old page fails both).
- ✅ **A hum could start after its own stop and loop** (LOW) —
  `drainGameSounds`: an inaudible sound (gain <= 0.02) called `stopKey`
  without bumping `keySeq`, so a looping sample whose first decode was still
  pending when an inaudible sound on its (entity, channel) arrived (a far
  door's stop sound during its hum's first decode) started afterwards and
  looped forever. `S_StartSound` picks the channel, and so overrides the
  old sound, before the audibility test (snd_dma.c); the page now bumps
  `keySeq` first, so the pending decode sees the override. `web/verify_loops.py`
  feeds the two sounds through `drainGameSounds` from a stand-in `exp` (a
  fresh 8-bit WAV at the listener on entity 900 channel 2, then a sound
  10000 units away on the same key): the hum no longer starts (the old page
  fails), and alone it does (the control).

## Demo commands, timedemo, pause (2026-09-25, branch `quake/timedemo`)

Quake's own demo and pause commands (CENSUS L12's pause half), against
`cl_demo.c`, `cl_main.c`, `host.c` and `host_cmd.c`.

- ✅ **`playdemo`, `stopdemo`, `startdemos`, `demos`, and the attract loop
  through them.** `cls`'s demo half is host state (`quake-wasm` `App::cls`:
  `demos[8]`, `demonum`) and the commands are id's: `CL_PlayDemo_f`
  (`CL_Disconnect`, `COM_DefaultExtension` ".dem", "Playing demo from %s.",
  "ERROR: couldn't open." + `demonum = -1`; the usage line "play <demoname> :
  plays a demo" is the C's), `Host_Stopdemo_f`, `Host_Startdemos_f` ("%i
  demo(s) in loop", "Max %i demos in demoloop", 15-character names, starts the
  loop only with nothing running and the loop not off, else switches it off),
  `Host_Demos_f` (off: back at the second slot). The boots run quake.rc's
  `startdemos demo1 demo2 demo3`, and a demo's end is `Host_EndGame`:
  `CL_NextDemo` (wrap after the last listed slot) or, outside the loop,
  `CL_Disconnect`. `map`, `load` and New Game switch the loop off
  (`cls.demonum = -1` in `Host_Map_f`/`Host_Loadgame_f`) and disconnect the
  demo. The attract loop cycles demo1 → demo2 → demo3 as before (F15), now
  printing id's console lines as it goes.
- ✅ **Disconnected** (`cls.state == ca_disconnected`: after `stopdemo`, a demo
  ending outside the loop, a `playdemo` that cannot open its file): nothing
  plays and the console covers the screen (`con_forcedup`: full height at
  once, its input line drawn, typing goes to it — `Key_Event`'s `key_game &&
  con_forcedup`), the menu over it; the console toggle brings up the main
  menu (`Con_ToggleConsole_f` with no connection), and leaving the main menu
  resumes the loop (`M_Main_Key`'s `CL_NextDemo`). Before this there was no
  such state: the port always had a level or a demo.
- Kept, not id's: **the menu does not stop the demo loop.** `M_Menu_Main_f`
  saves `cls.demonum` and sets -1 while the menu is up, so in id's Quake the
  demo playing when the menu opened is the last: at its end the client
  disconnects and the menu sits over the console until it is closed. The
  port boots with the menu open over the attract loop and keeps cycling
  behind it (F15, left as it was).

## LOW (27)

Tracked but deferred (cosmetic/edge). A few already landed in wave 1: SV_SetIdealPitch, SV_CheckStuck, groundentity-on-landed-entity, perspective-correct z-buffer (1/z), continuous 1/z particle size, debug builtins inert, light-style default, frame-index reset-to-0. Remaining low items (SV_TryUnstick/WallFriction, force_retouch, sky case-sensitivity, ~~affine span subdivision~~ (✅ `quake/w2b`, 16-pixel spans), TE color-ramp edge cases, audio cull threshold, etc.) are low-value and unscheduled.
