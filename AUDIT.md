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
| H11 | Ambient sounds missing — placed `ambientsound()` loops + the 4 automatic leaf ambients | ⬜ TODO |
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

## Wave 2 (next)

1. **Particle/TE spawn-wiring** — the wave-1 particle functions are ported but unused; wire them: per-frame R_RocketTrail on rocket/grenade/gib/tracer entities (by model flags), and route TE_LAVASPLASH/TE_TELEPORT/TE_TAREXPLOSION/TE_EXPLOSION2 + the `svc_particle` 255 sentinel to the right functions (split TAREXPLOSION out of the dlight group). Touches lib.rs/quaketool.rs/demo.rs.
2. **HUD completeness (H12–H14 + sbar mediums)** — inventory bar + weapon icons + current-weapon flash, animated face (health/pain/powerup), item/key/sigil icons, active-weapon ammo-type icon, armor-type icon, per-ammo small counts, scorebar/intermission.
3. **Sound** — ambient sounds (placed `ambientsound()` + the 4 leaf ambients), stereo pan law (linear vs equal-power), channel override.
4. **Demo interpolation activation** — switch lib.rs `boot_demo` / quaketool `cmd_demo` to `parse_demo_interpolated` + feed EF_ROTATE model indices (needs `Mdl` flags getter).
5. **Colormap-LUT lighting** (medium) — load `gfx/colormap.lmp`, thread it through `render_scene_ext`.

**Performance** is paused per the user (perf acceptable now); the ranked plan (style-value-keyed lightmap cache, frustum cull, persist framebuffer/zbuf — must stay pixel-identical) stays here for if/when it resumes.

## LOW (27)

Tracked but deferred (cosmetic/edge). A few already landed in wave 1: SV_SetIdealPitch, SV_CheckStuck, groundentity-on-landed-entity, perspective-correct z-buffer (1/z), continuous 1/z particle size, debug builtins inert, light-style default, frame-index reset-to-0. Remaining low items (SV_TryUnstick/WallFriction, force_retouch, sky case-sensitivity, affine span subdivision [= the perf item], TE color-ramp edge cases, audio cull threshold, etc.) are low-value and unscheduled.
