# Faithfulness census — game and client (2026-09-25)

**Status on 2026-09-26** (`quake/2026`): all 18 HIGH and MED findings (F1–F18) are
fixed, each by the commit named in its row. Of the 25 LOWs:
- 18 are fixed: L1 L2 L4–L11 L14–L16 L18 L20–L22 L24. L10 was fixed on `quake/polish4a`
  and L16 on `q26/server`.
- Two are fixed in part: L12 (all but the F-keys, `t` and `zoom_in`) and L25.
- Five are open: L3 L13 L17 L19 L23. `AUDIT.md`, "Open, as of 2026-09-26", lists them
  with the rest.

The departures in "Rule departures on by default" below are settled: since
`q26/settings` each is a setting of the 2026 profile, and the Classic profile has
`default.cfg`'s bindings and none of them. The text below is the census as written on
2026-09-25 at `31775f5`, with the fixes marked in its rows.

A systematic sweep for bugs of the "Chthon has no electricity" kind: things a
player of id's WinQuake would notice, found by method rather than by luck. This
round finds and proves; it fixes nothing (fixes wait for the render.rs /
server.rs / quake-wasm lib.rs refactor). Chthon's missing lightning itself
(TE_LIGHTNING3 written to MSG_ALL, dropped by `te_feed`) is fixed in
ce2dbf8 and is not repeated here. Items already in `AUDIT.md` or in the
oracle's renderer classes (`oracle/README.md`) are not repeated unless the
ledger is wrong about them (see "Corrections to the ledger").

**Severity.** HIGH: a player would notice in a normal single-player
playthrough (gameplay, or an obvious sight/sound difference). MED: noticeable
if you look or listen, or a whole feature missing on a less-used path (demos,
HUD extras). LOW: cosmetic, edge cases, or code-level only. A finding needs
evidence; anything marked *hunch* has none beyond reading.

## How it was checked

| instrument | what it does |
|---|---|
| `quaketool census <pak> [map..]` (new, `quake-rs/src/bin/quaketool/census.rs`) | Headless playthrough of `start` and e1m1..e1m8 through the real QuakeC, spawned the way the browser build spawns them: idle, god + `impulse 9`, an 8 s duel with one monster of each kind, wake/fight/kill every monster (half by exact damage, half gibbed), touch every item, three passes over every trigger/button/shootable, a scripted Chthon fight (electrodes up, lightning button, three hits), then the exit and `ExitIntermission`. Wraps `vm.builtins` to log every builtin executed with the arguments of the interesting ones; reports QC faults, sounds vs the pak, prints, MSG_ALL commands, temp entities, and brush movers that never moved. Runs all nine maps in ~6 s. |
| `oracle_edicts` / `oracle_client` / `oracle_quit` (added to `oracle/c/oracle.c`) + `census/oracle_run.py` | Runs **id's own WinQuake** headless through a console script (0.1 s per frame, `waits N` = N frames) and dumps its server edicts / client state. |
| `quaketool census-edicts` + `census/edict_diff.py` | Dumps the port's edicts at the same `sv.time` in the same format and diffs them against id's (matched by classname+model, then nearest origin; `--fields` picks what is compared, `mins,maxs` too since `quake/polish4a`). Run for all nine maps at t = 1.7, 4.7, 10.7, 20.7, 40.7 s (player idle at the start). |
| `census/qcsym.py` | Symbolic disassembler for `progs.dat` (the QC source is not on disk): resolves globals, immediates and call targets, and lists every call site of a builtin with its constant arguments — every `stuffcmd`, `sound`, `cvar`, `cvar_set`, `lightstyle`, `WriteByte` the id1 progs issue. |
| `quake-wasm/src/census_tests.rs` (new) | Nine tests (eleven since) that assert id's behaviour through the live path (`build_walk_map` + `step_walk`) on the real pak, written `#[ignore]`d, each the acceptance test for its fix and un-ignored in the fixing commit; all now run in the normal suite, as the module doc says since `quake/polish3`. Other fixes' tests live next to the code they test. |
| code reading | Every `svc_*` in `cl_parse.c`, every `TE_*` in `cl_tent.c`, every builtin the progs call (`pr_cmds.c`), `CL_RelinkEntities`, `r_part.c`, `view.c`, the sound paths, `menu.c`/`keys.c`/`cl_input.c`/`host_cmd.c`, compared line by line with the port. |

## Findings, ranked

| # | sev | finding | id's WinQuake | the port | evidence | fix goes in |
|---|---|---|---|---|---|---|
| F1 | HIGH | ✅ fixed in 43c4fff. **Teleporters don't turn the view.** | `teleport_touch` sets `other.angles = t.mangle; other.fixangle = 1`; `SV_WriteClientdataToMessage` sends `svc_setangle`, so `cl.viewangles` = the destination's facing (pitch 0). Same for `PutClientInServer`. | `w.yaw`/`w.pitch` are written only by input (and `player_start` at load); `client_think` clears `fixangle` without telling the client. You come out of every teleporter facing the way you went in, with the old pitch. | `census_teleport_turns_the_view_to_the_destination` (start.bsp: dest yaw 90, port keeps 180 / pitch 20) | `step_walk` after `client_frame` (copy `angles` into `w.yaw`/`w.pitch` when `fixangle` is set); clear `fixangle` where `SV_WriteClientdataToMessage` does, not in `Server::client_think` |
| F2 | HIGH | ✅ fixed in 5ea7d59. **The world keeps running behind the menu and console.** | `Host_ServerFrame`: `if (!sv.paused && (svs.maxclients > 1 \|\| key_dest == key_game)) SV_Physics ();` ("always pause in single player if in console or menus"); `cl.time` follows, so particles/animations freeze too. | `step_walk` always calls `client_frame` with a zeroed usercmd ("the world still TICKS") and advances `w.clock`. Press Esc mid-fight and the monsters keep shooting. | `census_single_player_pauses_behind_the_menu` (sv.time 1.9 → 2.9 with the menu up) | `step` / `step_walk` (skip the server frame and the game clock while `gate_gameplay`; keep realtime for the UI) |
| F3 | HIGH | ✅ fixed in 5ce3b4a. **e1m8 (Ziggurat Vertigo) has normal gravity.** | QC `worldspawn`: `if (self.model == "maps/e1m8.bsp") cvar_set("sv_gravity","100")`; `SV_AddGravity`, `SV_Physics_Step`'s hitsound threshold and `R_DrawParticles` (`sv_gravity*0.05`) read the cvar. | `bi_cvar_set` keeps only `skill`; `add_gravity` uses `const SV_GRAVITY = 800`; particles use `800*0.05`. The level's defining low-gravity jumps are gone. | `census_e1m8_has_low_gravity` (one 0.1 s frame: vz −80, id −10); census log `cvar_set("sv_gravity","100")` on e1m8 | `bi_cvar_set`/`cvar_value` (a live `sv_gravity`), `Server::add_gravity`, `physics_step`, the particle `advance` calls in `step_walk`/`step_demo` |
| F4 | HIGH | ✅ fixed in ea73bf5 (re-homed into server/sv_phys.rs + sv_user.rs by the merge 2d60a74). **Weapon switches pressed during a cooldown are dropped.** | `SV_ReadClientMove` only ever sets `v.impulse` (`if (i) ...impulse = i`); `W_WeaponFrame` returns early `if (time < self.attack_finished)`; `ImpulseCommands` clears it when it finally runs. So "press 2 while the rocket launcher reloads" (or while holding fire with the nailgun/thunderbolt) switches when the cooldown ends. | `Server::physics_client` zeroes `impulse` after every `PlayerPostThink`. The key press is lost. | `census_weapon_switch_survives_the_cooldown` (RL stays selected, id switches to the shotgun) | `Server::physics_client` (drop the clear; `apply_usercmd_to_edict` already writes only non-zero impulses) |
| F5 | MED | ✅ fixed in c5a2ffd (two causes: the double spread made the explosive box 34 wide; the health boxes, which setsize themselves, fell out because `world::clip_box` counted boxes that only touch as overlapping, where id's box hull is half-open). **External brush-model items are pixel-spread twice**, so two health boxes vanish and the explosive box floats. | `Mod_LoadSubmodels` spreads bounds by 1 once: `b_explob.bsp` raw (1,1,1)–(31,31,63) → (0,0,0)–(32,32,64). A 32-wide box traces in hull1. | `Bsp::parse` spreads, then `WorldModel::precache_model` spreads again → (−1,−1,−1)–(33,33,65): 34 wide, so traces use hull2 and `droptofloor` starts solid near walls/monsters. e1m1 loses the 10-health box at (1224,2464,−304) and e1m6 the 25-health box at (−672,832,0) ("Bonus item fell out of level"); e1m1's explosive box rests 2 units up. | `census_bmodel_item_bounds_are_spread_once`; oracle edict diff (id keeps both boxes, box at z −208 vs port −206); id's log has no "fell out" line | `WorldModel::precache_model` (drop the second spread) |
| F6 | MED | ✅ fixed in b7c0163. **No gold flash on pickups** (or when a powerup runs out). | Every item touch and `CheckPowerups` do `stuffcmd(other,"bf\n")` (16 sites); `V_BonusFlash_f` sets the bonus cshift (215,186,69) at 50%, fading 100/s. | `stuffcmd` is `bi_noop`; the demo path consumes svc_stufftext inertly. | `census_pickup_flashes_the_screen_gold`; `qcsym.py calls stuffcmd` | `stuffcmd` builtin → a client-command hook; the bonus cshift in `step_walk`'s blend list (and `step_demo`) |
| F7 | MED | ✅ fixed in d7e8e82. **The player has no name**: " entered the game", " was shot by a Grunt". | `Host_Spawn_f`: `ent->v.netname = host_client->name` ("player", `cl_name` default); also `colormap`, `team`. | `connect_client_inner` never sets `netname`; `ClientConnect`/`ClientObituary`/`ClientKill` print an empty subject on every level start and every death. | `census_player_netname_is_player`; census prints `" entered the game\n"` vs id's log `player entered the game` | `Server::connect_client_inner` |
| F8 | MED | ✅ fixed in 8c6b319. **Level-start `force_retouch` is not modelled**: doors that open at once in id's game stay shut. | `PutClientInServer` ends with `spawn_tdeath` → `force_retouch = 2`; for two frames `SV_Physics` relinks every edict with touches. Monsters standing in door trigger fields open them (e1m6 doors *31/*76, e1m8 *6: an ogre in each field); anything standing on a teleport destination is telefragged. | No `force_retouch`: those doors stay closed until the monster moves; a stationary monster on a destination is not telefragged. (AUDIT lists force_retouch as an unscheduled LOW without these effects.) | `census_force_retouch_opens_e1m6_start_door`; oracle diff (e1m6 *31 at −56 by t=10.7, e1m8 *6 at −72; port 0) | `Server::client_frame` / `run_frame` (honour the global like `SV_Physics`) |
| F9 | MED | ✅ fixed in d7583c6. **Door, lift and train "moving" sounds stop after one play.** | `GetWavinfo` reads the `cue ` loop point; `SND_PaintChannels` loops the channel until the stop sound overrides it on the same (entity, channel). | `playBuffer` never loops. `doormv1` 1.25 s, `hydro1` 0.99 s, `basesec1`, `stndr1`, `plat1` 1.82 s, `medplat1`, `train1` 1.92 s all carry cue points; any travel longer than the sample goes silent before the stop clunk (e1m1's lifts, e1m5's trains). The `queue_sounds` doc comment admits a loop deviation but cites `ambience/windfly.wav`, which has no cue chunk. | cue chunks read from pak0 (see "Sound" below); `web/index.html playBuffer` | `queue_sounds` + `poll_sound` (carry `loop_start` like `poll_static_sound`), `playBuffer`; stop skipping `misc/null.wav`, which is the override that ends a loop |
| F10 | MED | ✅ fixed in 29f5645. **Rune icons never reach the status bar.** | `SV_WriteClientdataToMessage`: `items = v.items \| (serverflags << 28)`; `Sbar_DrawInventory` draws sigil *i* on bit 28+*i*; `sigil_touch` only sets `serverflags`. | The live `Hud` gets `items: stat("items")`. Picking up e1m7's rune shows nothing. | `census_rune_icons_reach_the_status_bar` (passes when bit 28 is injected) | `step_walk` HUD build |
| F11 | MED | ✅ fixed in 37a75c3. **Tab opens the menu; the solo scoreboard can't be shown mid-level.** | default.cfg `bind TAB +showscores`; `Sbar_Draw` shows `Sbar_SoloScoreboard` (monsters, secrets, time) while held. | `index.html` routes Tab to `menu_cancel` (a fullscreen-safe menu key); the HUD passes `show_scores: false` ("not wired yet"). | `web/index.html` keydown (`k === 'tab'`); `step_walk` `show_scores: false` | page key routing + a `+showscores` held state feeding `Hud::show_scores` |
| F12 | MED | ✅ fixed in 4d03a2c (the viewmodel work merged just before this census: `Viewmodel::angles` = `viewmodel_angles`, CalcGunAngle's view before the punch, roll `cl.viewangles[ROLL]`; unit-tested in render/view.rs). **The gun moves with the view kick and the strafe roll.** | `V_CalcRefdef`: `CalcGunAngle()` (view.c:925) runs before `VectorAdd(r_refdef.viewangles, cl.punchangle, ...)` (view.c:958), and the gun's roll is `cl.viewangles[ROLL]` (0). Every shot kicks the view −2° (−4° double barrel) while the gun stays put, so it dips on screen; strafing tilts the view but not the gun. | `draw_viewmodel` poses the gun with `cam.basis()`, and the camera includes punch and roll: the gun is glued to the screen. | view.c 925/958 vs `step_walk` camera + `render::draw_viewmodel` | `render::Viewmodel` gets its own angles (view angles without punch/roll); `step_walk`, `step_demo` |
| F13 | MED | ✅ fixed in 726a7cc. **Demos draw no rocket/grenade smoke, gib blood or wizard tracers.** | `CL_RelinkEntities` runs `R_RocketTrail` for model flags in playback exactly as live. | Only `step_walk` calls `rocket_trail_type`/`spawn_rocket_trail`. | code (`step_demo` has no trail code); demo1–3 precache missile/grenade/gibs/w_spike | `step_demo` |
| F14 | MED | ✅ fixed in 230ad15. **Demos drop entity skins** (yellow armour draws green). | `CL_ParseUpdate` `U_SKIN` → `skinnum`; baseline skin. | `demo.rs` reads and discards both (`let _ = r.read_byte(); // skin — consumed`, `let _skin` in `parse_baseline`); `step_demo` passes skin 0. Yellow armour (`armor.mdl` skin 1) has a baseline in every demo (demo1 ×1, demo2 ×2, demo3 ×2). | `demo.rs parse_update`, `parse_baseline`; baseline skins read from the three .dem files | `demo.rs` (`EntSnapshot` skin), `step_demo` |
| F15 | MED | ✅ fixed in 62dc78e. **The attract loop plays demo1 forever.** | quake.rc `startdemos demo1 demo2 demo3`; `svc_disconnect` → `CL_NextDemo`. | `const DEMO_FILE = "demo1.dem"`; `SVC_DISCONNECT` stops and the page wraps to frame 0 of demo1. | `quake.rc` in pak0; `quake-wasm DEMO_FILE` | `build_demo` / `step_demo` demo cycling |
| F16 | MED | ✅ fixed in bcf01c3 (with the live directional kick and the pain face). **No damage flash (or kick) while invulnerable or in god mode.** | QC `T_Damage` adds to `dmg_take`/`dmg_save` *before* the god-mode and `invincible_finished` returns; `SV_WriteClientdataToMessage` sends `svc_damage` whenever they are non-zero, then zeroes them. | The live flash is inferred from health/armour deltas; with the Pentagram (or `god`) there is no delta, so no flash. `dmg_take`/`dmg_save` are never read or zeroed. | `qcsym.py func T_Damage` (stmts 1462–1470 before 1494/1497) | `step_walk` damage block: read + zero `dmg_take`/`dmg_save`/`dmg_inflictor` — this also closes AUDIT's open live damage-kick direction |
| F17 | MED | ✅ fixed in d10aebd. **Pressing number keys through `e.key`**: Shift+digit and non-US layouts can't select weapons. | Key_Event works on key numbers: `bind 2 "impulse 2"`. | `index.html` tests `k >= '1' && k <= '8'` on `e.key`; on AZERTY the unshifted row is `&é"'(-è_`, and Shift+2 gives `@`. | `web/index.html` keydown | page: use `quakeKey(e)`'s `Digit*` codes, or put `impulse 1..8` in `default_bindings` |
| F18 | MED | ✅ fixed in 91d28ba (only weapons visibly flash: Sbar_DrawInventory's `flashon` is 0 for keys/powerups/runes). **New inventory icons don't flash.** | `CL_ParseClientdata` stamps `cl.item_gettime[j] = cl.time` for every newly set item bit; `Sbar_DrawInventory` cycles the `inva1..5` frames for 1 s (weapons, keys/powerups, runes). ~~`CL_ClearState` zeroes `cl.items`, so every owned weapon also flashes at each level start.~~ Wrong premise, corrected in the second review (branch `quake/polish3`): `CL_ClearState` zeroes `cl.time` too, and the signon clientdata is parsed before `CL_LerpPoint` first snaps it to the server's (`CL_ReadFromServer`), so the owned bits are stamped at about `host_frametime` and the flash is over by the first drawn frame (`cl.time` >= 1.2): nothing flashes at a level, load or demo start. The port had copied the premise; it now seeds the spawn items. | No per-item gettime; the settled icon is drawn from the first frame (the `render.rs` weapon-strip comment says so). | cl_parse.c:549, sbar.c:570; `render::draw_hud_into` weapon strip | `Hud` gains item get-times (set by the front-end on each new items bit), `draw_hud_into` |

### LOW (verified unless marked)

| # | finding | evidence / where |
|---|---|---|
| L1 | ✅ fixed in 35bbd60. Live `punchangle` is not truncated to whole degrees (`MSG_WriteChar`): id's shotgun kick steps −2 → −1 → 0, the port's eases smoothly. Demo path is right. | sv_main.c `MSG_WriteChar(msg, ent->v.punchangle[i])`; `step_walk` camera |
| L2 | ✅ fixed in 3ec96f4. Damage flash fade: the C's `percent` is an `int` (client.h:53) decremented by `host_frametime*150` and truncated → 3 per frame, 180/s at 60 fps and 216/s at id's 72 fps cap; the port fades a float at 150/s, so the flash lasts 20–45% longer. (The bonus flash, once F6 exists, has the same shape at 100/s.) | view.c:643 `V_UpdatePalette`; `step_walk`/`step_demo` `damage_blend` |
| L3 | Pitch drift: `cl.idealpitch` from the server is ignored (fixed 0) and drift is not cancelled off-ground; with id's default (no mlook) keyboard players get the auto-tilt on stairs, the port never does. Documented in code as a simplification, not in AUDIT. | view.c `V_DriftPitch`; `step_walk` drift block |
| L4 | ✅ fixed in 6611dda. `PlayerPreThink` sees a stale `time` global (the last think's `thinktime`); C sets `time = sv.time` first. PreThink timers (air, lava damage, `IntermissionThink`) can fire a frame early. | sv_phys.c `SV_Physics_Client`; `Server::physics_client` |
| L5 | ✅ fixed in b060c82 (id's oracle: a standing forward jump now matches frame for frame). Client think order: the C runs `SV_ClientThink` (SV_RunClients) before `PlayerPreThink`; the port runs PreThink first, so QC `WaterMove`'s velocity trim (`0.8*waterlevel*frametime`) is undone by the engine's acceleration — swimming ~3.5% fast at 72 fps, derived from the code (a harness at 0.1 s frames measured a steady 224 u/s where id's order gives ~170). AUDIT has this as a LOW without an effect; LOW stands. | host.c `Host_ServerFrame`; `Server::physics_client` |
| L6 | ✅ fixed in 1bc7030. `ED_Alloc` reuses a freed slot immediately and `ED_Free` zeroes every field; the C waits 0.5 s ("so the client doesn't think the entity morphed ... bad trails") and clears only some fields. A same-frame free+spawn can draw a stray trail from the old slot. | pr_edict.c; `Vm::spawn`/`spawn_checked`/`free_edict` |
| L7 | ✅ fixed in fdc9c7c. `restart` (respawn after death) carries the *live* `serverflags`; the C restores the level-entry `svs.serverflags` — die on e1m7 after taking the rune and id loses it. | sv_main.c `SV_SpawnServer`; quake-wasm `try_restart` |
| L8 | ✅ fixed in 5c34a9f. One in five spray particles (`die = cl.time + 0.1*(rand()%5)`) is never drawn: the C draws before it moves/kills, the port advances then draws. | r_part.c `R_DrawParticles`; `ParticleSystem::advance` + call order in `step_walk` |
| L9 | ✅ fixed in 076c217. Explosion dlights decay one frame before first drawn (C decays after `SCR_UpdateScreen`). | host.c; `step_walk` `dlights.advance` |
| L10 | ✅ fixed on `quake/polish4a`: precache resolves every model file's `mod->mins/maxs` by its magic (±16 alias, ±maxwidth/2 sprite, submodel 0 of a b_*.bsp), `alias_and_sprite_models_get_mod_load_model_bounds`; the port's edict boxes change only for the flames L17 keeps alive, and match id's for every entity both have (`edict_diff.py --fields mins,maxs`; id's rocket explosion is ±28, `s_explod.spr` 56x56). `setmodel` on alias/sprite models sets a zero box; WinQuake uses ±16 (alias) / ±maxwidth/2 (sprite). Only entities that never `setsize` afterwards (explosion sprites, flames, `viewthing`) — no shareware effect found. The `model_bbox` comment claims the C does zero. | model.c `Mod_LoadAliasModel`; `WorldModel::model_bbox` |
| L11 | ✅ fixed in b9feb31 (position, Con_Print wrap/stamps) and on `quake/polish` (prints reach the console scrollback, word-wrapped the same; the "still open" console-output-to-notify note is moot: `Con_ToggleConsole_f` zeroes `con_times`, 0cdcb1a). Notify lines start at y=8 (C: y=0) and wrap only on `\n` (C word-wraps at `con_linewidth`); prints never reach the console scrollback. | console.c `Con_DrawNotify`; `render::draw_notify` |
| L12 | ✅ partly fixed in 15eb688 (ENTER, MOUSE2, `\`/MOUSE3, INS) and on `quake/timedemo` (PAUSE: the `pause` command and `SCR_DrawPause`'s plaque, `census_pause_stops_the_game_and_shows_the_plaque`; the 2-D oracle's `pause` scenario is pixel-exact); the F-keys and `t` still missing. Missing default.cfg binds: ENTER `+jump`, MOUSE2 `+forward`, MOUSE3/`\` `+mlook`, INS `+klook`, PAUSE `pause` (no `pause` command or plaque at all), F1–F4/F6/F9/F10/F12, `t` messagemode (inert is fine). | default.cfg in pak0; `render::default_bindings`, page mouse routing |
| L13 | `give` differs from `Host_Give_f`: sets `weapon` without `W_SetCurrentAmmo` (stale viewmodel/ammo), clamps, has an `a` case, prints. Cheat-only. | host_cmd.c; `run_give_command` |
| L14 | ✅ fixed in d21a2b9. New Game while a game runs has no "Are you sure?" (`M_SinglePlayer_Key` → `SCR_ModalMessage`). | menu.c |
| L15 | ✅ fixed: `misc/null.wav` queues and overrides, and an inaudible sound ends the sound on its (entity, channel), since d7583c6 (F9) — one whose first decode is still pending too since eddd9e0 (`quake/polish2`); each side clamped at full before the master volume, and one-shots re-spatialised every frame, on `quake/polish3` (`web/verify_loops.py` section 4). Sound: near-side gain clamped after the master volume (C clamps per side at 255 before `volume`) — up to 1.43× louder close and panned at volume 0.7; one-shots are not re-spatialised each frame; an inaudible new sound (gain ≤ 0.02) does not cut the old one on its (entity, channel); `misc/null.wav` never overrides. | snd_dma.c `SND_Spatialize`/`SND_PickChannel`; `web/index.html playRouted`, `queue_sounds` |
| L16 | ✅ fixed on `q26/server` (2026-09-26): `error` and `objerror` print id's banner and `ED_Print (self)`, `objerror` frees `self`, and `Host_Error` ends the game (AUDIT.md, "QuakeC errors end the game"). Was: ⏸ left open on quake/fix-server: making `objerror`/`error` end the game (Host_Error) needs a disconnect-to-console path the browser shell does not have, for a teleporter no normal play reaches. `objerror`/`error` are non-fatal (C: `Host_Error`, which ends the game; `objerror` also frees `self`). The census hits one: start.bsp's `trigger_teleport` targeting `t11` (an 18-unit box at z −673..−655) points at an `info_null`, which removes itself at spawn → "couldn't find target"; id would drop to the console, the port carries on. Probably unreachable in normal play (*hunch* — the box sits far below the hub). | pr_cmds.c `PF_objerror`; `builtins::pf_objerror` |
| L17 | ⏸ left open on quake/fix-server: freeing the flames needs a static-entity list the renderer draws (cl_walk/render, other agents' files) and savegame handling, for no visible change. `makestatic` is a no-op: the 6–44 flames per map stay live edicts (C frees them after `svc_spawnstatic`). No visible effect; costs `MAX_EDICTS` headroom and savegame size. | pr_cmds.c `PF_makestatic`; `install_engine_builtins` #69 |
| L18 | ✅ fixed in 0d7f01c. The world edict has `solid 0`/`movetype 0`; `SV_SpawnServer` sets `SOLID_BSP`/`MOVETYPE_PUSH`. The only progs reader (`ClientObituary`) also checks `attacker != world`: no effect found. | oracle edict diff (`worldspawn movetype 7/0 solid 4/0`); `Server::set_map_name` |
| L19 | `checkclient` uses a line-of-sight trace, not the cached 0.1 s client PVS; `FindTarget` follows it with `visible()` so the result only differs by the PVS staleness. | pr_cmds.c `PF_checkclient`; `bi_checkclient` |
| L20 | ✅ fixed in a19ec64. The spawn settle frames skip `StartFrame` (C's `SV_Physics` always runs it); nothing reads `skill`/`framecount` in those 0.2 s. | sv_phys.c; `Server::spawn_entities` |
| L21 | ✅ fixed by the viewmodel work of `quake/fid1` (`render/view.rs` `viewmodel_origin_ofs` moves the gun along the full view pitch; `AUDIT.md`, "Session 7", Viewmodel). The gun's forward bob uses a third of the view pitch; V_CalcRefdef sets `ent->angles[PITCH] = -cl.viewangles[PITCH]` first (≤1.6 units). | view.c 885/907; `render::viewmodel_origin_ofs` |
| L22 | ✅ fixed by PERF_PLAN C1 (branch `quake/sim`); the hunch was right. `entity_dlights` and trails covered every edict; the C only relinks entities the server sent (model + PVS), so an out-of-view muzzle flash lit surfaces through walls. `R_AddDynamicLights` lights by \|distance\| to the plane: on start.bsp a flash 54 units behind the wall ahead lights >100 px of its near side. Test `an_entity_outside_the_fat_pvs_is_not_drawn_and_its_flash_lights_nothing`. | sv_main.c `SV_WriteEntitiesToClient`; `Server::entity_dlights` |
| L23 | The gibbed player's head (`h_player`, EF_GIB) gets no blood trail: the C skips only *drawing* the view entity, after trails. | cl_main.c:610; `step_walk` entity loop |
| L24 | ✅ fixed in 3c8a862. Eye inside a sky volume (noclip only): C's `default:` gives the water tint, the port none. | view.c `V_SetContentsColor`; `render::content_cshift` |
| L25 | ✅ half fixed in cdb4707 (the loop bound); running the player first is left open — the C reserves edict 1 for the client, a numbering change across spawn, connect and savegames. Frame order: the C's `SV_Physics` re-reads `sv.num_edicts` every iteration and runs the player first (edict 1), so a rocket fired in the player's think moves on its spawn frame; the port fixes `n` at frame start and runs the player after every map entity, so a new missile can sit still for one frame (a few units at 60 fps) and monsters think before the player. | sv_phys.c `SV_Physics`; `Server::client_frame` |

Engineering, not faithfulness: `Vm::intern` never de-duplicates (every `setmodel`, `ftos`, `vtos` appends to the string heap) and `vm.output` collects every print and is never drained in the browser build — both grow for the life of a level (the VM is rebuilt on changelevel).

## Rule departures on by default (other than Always Run)

*Settled on 2026-09-26 (`q26/settings`). Each is a setting of the 2026 profile:
`freelook`, the WASD bindings, `vid_fkey`, `cl_jumpswim`. Classic has none of them
(`AUDIT.md`, "The profiles and the departures").*

The rule: faithful by default, Always Run the only default departure. These are
deliberate, documented in code, and each needs the user's decision (make opt-in,
or accept and record):

| departure | id's default | port default | where |
|---|---|---|---|
| Mouse look | off: mouse Y moves forward/back; `+mlook` (`\`, MOUSE3) holds it | `+mlook` permanently held under pointer lock | quake-wasm `mouse_move` |
| WASD | default.cfg: `a` = `+lookup`, `d` = `+moveup`, `w`/`s` unbound | `w`/`s`/`a`/`d` move, overriding id's `a`/`d` | `render::default_bindings` |
| Tab | `+showscores` | ✅ `+showscores` again (F11, 37a75c3) | `web/index.html` |
| `f` | unbound | toggles fullscreen | `web/index.html` |
| Space in water/fly | `+jump` only (PlayerJump's 100 u/s swim-up) | also adds `upmove` (swims up ~140 u/s; rises in fly/noclip) | quake-wasm `KeyMove` / key handling |

Opt-in departures, off by default, live on Options > Web extras (`wasm_*`
console commands); AUDIT.md "Web extras" lists them.

## Corrections to the ledger (AUDIT.md / code comments)

- **Round 3 "Refuted: PF_makestatic ED_Free"** — the C does free: `// throw the entity away now ED_Free (ent);` (pr_cmds.c `PF_makestatic`). The port's no-op is harmless (L17) but the refutation is wrong.
- **force_retouch** (AUDIT LOW list) has visible effects: level-start doors and stationary telefrags (F8).
- **"client_think pre/post-think order"** (AUDIT Round-2 LOW): its effect is ~3.5% extra swim speed at 72 fps, derived (L5); LOW is right.
- **`queue_sounds` doc comment**: the looping deviation it documents is real but mis-scoped — it cites `ambience/windfly.wav` (no cue chunk) and misses every door/lift/train mover (F9).
- **`WorldModel::model_bbox` comment** ("the C setmodel also set a zero box for non-brush models") is wrong for WinQuake (L10). Fixed with it on `quake/polish4a`.
- **quaketool did not build at 7db9a14** (the options merge added `Viewmodel::origin_ofs`; the oracle's `view` subcommand built the struct without it), so `cargo test` in quake-rs failed on `quake/overnight`. Fixed on this branch in `68ec277`.

## What was checked and found faithful

**Map playthrough** (`quaketool census`, all nine maps, skill 1): every classname has a
spawn function; zero QC faults on e1m1–e1m8 (start: only L16's
teleporter, the same map data in id's game); every precached/set model and sound is in the pak; every monster
dies through QuakeC `T_Damage` (normal deaths and gibs, `killed_monsters` =
`total_monsters`); every shareware monster attack runs without a fault in
the duels (grunt shots, dog bite, ogre chainsaw and grenades, fiend leap,
knight sword, scrag spit → TE_WIZSPIKE, zombie gib throws, shambler claws and
lightning → TE_LIGHTNING1, Chthon's lava balls); **Chthon**: the sigil wakes him (`boss_awake`, TE_LAVASPLASH),
electrodes + lightning button hurt him only at STATE_TOP, three hits kill him
(`boss_death`, 21/21 kills), the t9 exit doors open, and the exit gives
`svc_intermission` → `svc_finale` (the shareware text) → `svc_sellscreen`. Every
brush mover moves when its opener fires, except ones whose openers are
skill-inhibited on medium (e1m3 t149/t157, e1m4 t105, e1m5 t134 — easy-only
triggers), registered-only (start t2/t3/t5 behind `trigger_onlyregistered`,
`func_bossgate`), `func_wall`s, and e1m2's shootable button *56 (its travel is
`size - lip` = 0 by design). Trigger chains (counters, relays, secret doors,
shootable triggers, key doors, teleporters incl. monster teleports, lights via
`lightstyle` — the renderer re-reads the table every frame) all fire; the
changelevel chain is e1m1→e1m2→…→e1m7, e1m8→e1m5; `start`'s skill halls set
`skill` 0–3 and the registered episodes print their centerprints.

**id's simulation vs the port's** (oracle edict diff, idle player, t up to
40.7 s): every item, door, plat, train and button position matches
except F5's two boxes and F8's doors; e1m5's trains are identical at 40.7 s;
patrolling monsters stay within a few units (random-free paths; divergence
after that is the PRNG, by design).

**Server→client messages** (demo path + the live equivalent): nop, time,
version, print, centerprint, serverinfo, setview, lightstyle, sound (decode:
volume/attenuation defaults, ent/channel split, precache resolve), stopsound,
particle (incl. the 255→1024 sentinel), spawnbaseline (except skin, F14),
spawnstatic, temp_entity, signonnum, killedmonster/foundsecret/updatestat
(the live HUD reads the same QC globals), spawnstaticsound, cdtrack (inert:
no CD audio), intermission/finale/cutscene/sellscreen, clientdata bit order,
U_* bits (except skin), damage (demo path). Live-only gaps are F1, F6, F10,
F16, L1 (all fixed since, as is the skin, F14).

**Temp entities** (live and demo): SPIKE/SUPERSPIKE (count 10/20, tink1 on
`rand()%5`, else ric1/2/3), GUNSHOT, EXPLOSION (1024 particles, r_exp3, dlight
350/0.5 s/300 live), TAREXPLOSION (R_BlobExplosion, no dlight), WIZSPIKE and
KNIGHTSPIKE (+ their hit sounds), LAVASPLASH, TELEPORT (incl. the C's i/j
swap), EXPLOSION2, LIGHTNING1/2 + BEAM — colours, counts, lifetimes, sounds
match. MSG_ALL LIGHTNING3 fixed in ce2dbf8. Known open: demo dlights (AUDIT).

**Builtins** (all 57 the id1 progs call): makevectors, setorigin, setsize,
random (range), sound (for id1's arguments), normalize, vlen, vectoyaw and
vectoangles (int truncation), spawn/remove (except L6), traceline, find
(skips free edicts, strcmp, `""` matches unset), precache_* (return their
argument), findradius, bprint, sprint, dprint (hidden), ftos/vtos formats,
walkmove, droptofloor, lightstyle, rint/floor/ceil/fabs, checkbottom,
pointcontents, aim (sv_aim 0.93 cone), cvar (every name the progs read:
teamplay, skill, samelevel, registered = 0 as shareware, noexit, timelimit,
fraglimit, temp1), localcmd (`restart`), particle (except AUDIT's known dir
quantisation), ChangeYaw, WriteByte/Coord/String/Entity (BROADCAST temp
entities, MSG_ALL intermission/finale), movetogoal, changelevel, centerprint,
ambientsound, setspawnparms (coop-only). Think/touch semantics: `nextthink`
window, `time` = thinktime, self/other save/restore, `frametime`; ED_ParseEdict
quirks (`angle`, `light`, `_` keys), skill/deathmatch spawnflag filters,
`world.model`/`mapname`/`serverflags` before spawn.

**Client effects**: model flags come from each .mdl header (shareware: rotate
on armour/weapons/keys/powerups/backpack, EF_GIB on gibs and heads, ZOMGIB,
TRACER on w_spike, ROCKET on missile *and lavaball* (Chthon's balls trail fire),
GRENADE); trail types 0–6 (colours, ramps, 3-unit steps, tracer velocities,
lifetimes); EF_ROTATE (`anglemod(100*t)`, no bob — WinQuake has none);
EF_MUZZLEFLASH/BRIGHTLIGHT/DIMLIGHT dlights (EF_BRIGHTFIELD unused by id1);
particle physics per type; explosion/blob/lava/teleport spawners; V_CalcBob,
V_CalcRoll, V_AddIdle (0 in play, 1 at intermission), intermission camera,
stair smoothing, death camera (−8 view, roll 80), contents and powerup tints,
V_UpdatePalette's whole-screen shift, underwater warp. The local player's
colormap is never visible in single player (no chase cam, bodies are coop-only).

**Sound**: 190 of the 226 samples the progs name are in pak0; the other 36 are
registered-only (`precache_sound2`), and no shareware map precaches them.
Engine sounds: menu1 (cursor), menu2 (enter/back), menu3 (sliders only), the
cl_tent sounds, `misc/h2ohit1.wav` (SV_CheckWaterTransition, non-clients),
`demon/dland2.wav`, the water1/wind2 ambients. `misc/talk.wav` on messages is
QC (`SUB_UseTargets`), not the engine — Con_Print plays it only for `say` text,
which single player never sends; the port plays it where id does. QC player
sounds (land/land2, plyrjmp8, inh2o/outwater/h2ojump, pain on hard landing)
fire on the same frames. CHAN_AUTO never overrides; same-entity override;
attenuation law; the view entity at full volume in both ears.

**Input/console**: impulses 1–12 and 255 go through QC `ImpulseCommands`
unmodified (except F4's clear); `/` = impulse 10; lookspring/lookstrafe/invert;
centerview; god/noclip/fly/kill messages ("godmode ON", "Can't suicide --
allready dead!").

## Not checked

Registered content (e2–e4, the registered-only monsters/sounds); coop,
deathmatch and multiplayer; skills 0, 2 and 3 (the playthrough ran medium);
e1m4's second (secret) exit; save/load; the renderer beyond what
`oracle/README.md` covers; status-bar pixels against id's (only the rune cell);
CD audio; browser-level input and audio beyond code reading (no
`verify_*.py` run — the census changes no runtime code); the network protocol
itself; performance.

## Rerunning

```sh
cd quake-rs && cargo build --release
./target/release/quaketool census ../quake-data/ID1/PAK0.PAK            # all nine maps
./target/release/quaketool census ../quake-data/ID1/PAK0.PAK e1m7       # one map
cd .. && oracle/build.sh                                                  # id's side (docker)
uv run census/oracle_run.py --dev --out /tmp/o 'map e1m6' 'waits 5' 'oracle_edicts {out}/c.txt'
quake-rs/target/release/quaketool census-edicts quake-data/ID1/PAK0.PAK e1m6 1.7 > /tmp/o/port.txt
uv run census/edict_diff.py /tmp/o/c.txt /tmp/o/port.txt --t 1.7 --skip bodyque,player,light_torch_small_walltorch,light_flame_large_yellow,light_flame_small_yellow,light_flame_small_white
uv run census/qcsym.py quake-data/ID1/PAK0.PAK calls stuffcmd cvar_set   # the progs' own calls
cd quake-wasm && cargo test --release census                              # the fixed findings' acceptance tests
cd quake-wasm && cargo test --release census -- --ignored                 # a new open finding's test (none today)
```

A console script longer than 8 KB overflows id's command buffer and the oracle
never quits — keep `waits` under ~1500 per run (one map per run for long idles).
