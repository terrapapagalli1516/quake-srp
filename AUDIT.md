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
| H6 | `pixelAspect` — frame presented 16:10 square-pixel instead of authored 4:3 | ✅ fixed `9876cb4` (present at 4:3) |
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
  (id's 320x200 uses pixelAspect 0.8333 for 4:3 CRTs).

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

## LOW (27)

Tracked but deferred (cosmetic/edge). A few already landed in wave 1: SV_SetIdealPitch, SV_CheckStuck, groundentity-on-landed-entity, perspective-correct z-buffer (1/z), continuous 1/z particle size, debug builtins inert, light-style default, frame-index reset-to-0. Remaining low items (SV_TryUnstick/WallFriction, force_retouch, sky case-sensitivity, affine span subdivision [= the perf item], TE color-ramp edge cases, audio cull threshold, etc.) are low-value and unscheduled.
