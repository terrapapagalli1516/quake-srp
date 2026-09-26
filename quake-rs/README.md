# quake-rs

The engine crate of quake-rust: a Rust port of id Software's *Quake* (1996) for single
player, ported from id's GPLv2 WinQuake C source
([`id-Software/Quake`](https://github.com/id-Software/Quake)). A library (`quake_rs`) plus
the `quaketool` CLI. Only the standard library; `#![forbid(unsafe_code)]` on the library,
the tool and the integration tests.

It holds the whole game except the platform:
- the file system and file formats;
- the QuakeC VM and builtins;
- the server (spawn, physics, collision, AI, combat, changelevel, intermission,
  savegames);
- the software renderer, which draws on any number of threads;
- the 2-D layer (status bar, menus, console);
- the game client (the live frame and demo playback);
- the console's commands and variables, and the Classic and 2026 profiles;
- id's sound mixer and the CD player's state.

The platform owns the window or canvas, reading input, the audio device and the disk. The
browser's platform is `../quake-wasm` (a WASI program; `../web/PLATFORM.md`), and
`quaketool play` runs the same client natively. Not here: multiplayer and netcode. The
faithfulness ledger is `../AUDIT.md`.

## Modules

Every module in `src/`, with the id source it ports. Modules are named after id's files
where one file maps to one module; the port's own modules say so.

| module | id's C | what |
|---|---|---|
| `math` | `mathlib.c` | vectors, matrices, angles, `BoxOnPlaneSide` |
| `crc` | `crc.c` | CRC-16/CCITT (the PAK check) |
| `read`, `error` | (`LittleLong`, `Sys_Error`) | the bounds-checked little-endian reader, the error type |
| `pak` | `common.c` | PAK archives (`COM_LoadPackFile`), opened as files and read on demand, each one element of the search path |
| `common` | `common.c`, `pr_edict.c` | the search path (`COM_InitFilesystem`, `COM_AddGameDirectory`, `COM_FindFile`), `COM_CheckRegistered` and `pop[]`, `com_modified`, `path`; `PR_LoadProgs`' checks |
| `wad` | `wad.c` | WAD2 archives (`gfx.wad`: palette, pics, fonts) |
| `bsp` | `bspfile.h`, `model.c` | BSP v29 maps, with the mip levels and `Mod_DecompressVis` |
| `mdl` | `modelgen.h`, `model.c` | alias models |
| `spr` | `spritegn.h`, `model.c` | sprites |
| `progs` | `pr_comp.h`, `progs.h`, `pr_edict.c` | the `progs.dat` format, opcodes decoded once at load (`Op`), and a disassembler |
| `vm` | `pr_exec.c`, `pr_edict.c` | the QuakeC interpreter, edicts and strings; entity fields and globals through handles resolved once per progs (`Vm::fo`, `Vm::go`); private state behind accessors |
| `vm::print` | `pr_exec.c`, `pr_edict.c` | `PR_RunError`'s report: `PR_PrintStatement`, `PR_StackTrace`, `ED_Print` |
| `builtins` | `pr_cmds.c` | the builtins that need no map: maths, strings, prints, `spawn`/`remove`/`find`, `random`, `stuffcmd` |
| `qrand` | (libc `rand`) | the session's two random streams, QuakeC's `random()` and `SV_NewChaseDir`'s, carried across level loads |
| `world` | `world.c`, `sv_phys.c` | BSP hull traces (`SV_RecursiveHullCheck`), the box hull, point contents, and the world-only slide move (`ClipVelocity`, `SV_FlyMove`) the `walk` tool uses |
| `server` (`server/mod.rs`) | `server.h` | the `Server`: a VM with the engine builtins over a loaded map, and its reports; `MoveType`, `Solid`, `EntFlags` |
| `server::sv_main` | `sv_main.c` | `SV_SpawnServer`, `SV_ConnectClient` (with `Host_Spawn_f`), `SV_CleanupEnts`, `SV_FatPVS` and which entities the client is sent, entity dynamic lights |
| `server::pr_edict` | `pr_edict.c`, `common.c` | `ED_LoadFromFile` and the entity-text parsers |
| `server::pr_cmds` | `pr_cmds.c` | the builtins that touch the world (`setmodel`, `traceline`, `droptofloor`, `aim`, `checkclient`, `findradius`, …) and the `pr_builtin[]` install |
| `server::sv_phys` | `sv_phys.c` | `SV_Physics`: thinks, every movetype, pushers, the player's move; the uncapped gravity lead |
| `server::sv_user` | `sv_user.c` | `SV_ClientThink`, friction and acceleration, swimming, `SV_SetIdealPitch` |
| `server::sv_world` | `world.c` | `SV_Move` against the world and every solid edict, `SV_LinkEdict`, touching, `SV_Impact` |
| `server::sv_move` | `sv_move.c` | monster movement: `SV_movestep`, `SV_NewChaseDir`, `SV_MoveToGoal`, `SV_CheckBottom` |
| `server::msg` | `sv_main.c`, `pr_cmds.c`, `cl_parse.c` | the server-to-client messages without a network: the `Outbox` the builtins write into (sounds, particles, prints, temp entities), the `Write*` builtins parsed per buffer |
| `server::lightstyle` | `pr_cmds.c`, `r_light.c` | `PF_lightstyle`'s table and `R_AnimateLight`'s scales |
| `server::host` | `host_cmd.c`, `sv_main.c` | `ServerCvars` (`skill`, `sv_gravity`), the deferred `changelevel`/`restart`, spawn parms and serverflags, `Host_Kill_f`, the signon settle frames |
| `save` | `host_cmd.c` | savegames: `Host_Savegame_f`'s `.sav` text and `Host_Loadgame_f`'s parse |
| `demo` | `cl_demo.c`, `cl_parse.c`, `protocol.h` | the `.dem` framing and the network-message decoder, one frame per message with the state `CL_RelinkEntities` reads |
| `particles` | `r_part.c` | the particle spawners and their motion |
| `tent` | `cl_tent.c` | beams: `CL_ParseBeam`'s slots and `CL_UpdateTEnts`' bolt pieces |
| `dlight` | `cl_main.c` | the dynamic-light pool: `CL_AllocDlight`, `CL_DecayLights` |
| `stepping` | (the port's own) | `Stepping::Classic` or `Uncapped`: how a host frame of any length steps what drifts with the frame rate (`../FRAMERATE.md`) |
| `snd` (`snd/mod.rs`) | `snd_dma.c`, `snd_mix.c`, `snd_mem.c` | id's mixer: `SoundMode` (Classic at 11025 Hz, or the 2026 mixer) and `Fixes` |
| `snd::dma` | `snd_dma.c` | the `Mixer`: the channel table, `S_StartSound` (`SND_PickChannel`, `SND_Spatialize`), `S_StaticSound`, `S_StopSound`, `S_Update` with the leaf ambients, `S_Update_`'s mix-ahead |
| `snd::mix` | `snd_mix.c` | `S_PaintChannels`, `SND_PaintChannelFrom8`/`16`, the scale table, `S_TransferStereo16` |
| `snd::mem` | `snd_mem.c` | `GetWavinfo`, `S_LoadSound` + `ResampleSfx`, the `known_sfx` table |
| `cd_audio` | `cd_win.c`, `cd_audio.c` | the CD player's state: `CDAudio_Play`/`Stop`/`Pause`/`Resume`, `CD_f`, the end-of-track notify; the level from DOS `cd_audio.c` |
| `render` (`render/mod.rs`) | `r_main.c` | the 3-D view: `Renderer` (all its state: edges, caches, z-buffer, warp, threads) and `Scene` (id's `refdef_t` plus the entity lists), `R_ViewChanged`, `Image<u8>` frames of palette indices |
| `render::edge` | `r_bsp.c`, `r_draw.c`, `r_edge.c`, `d_edge.c` | the world and brush models: the BSP walk, edge clipping and the edge cache, `R_ScanEdges`, `D_DrawSurfaces`, the 16-bit z-buffer |
| `render::band` | (the port's own) | the view in row bands on several threads after the edge scan, each band through id's passes in id's order: the same pixels for any thread count |
| `render::raster` | `d_edge.c`, `d_draw16.s`, `d_scan.c` | the span routines: `D_CalcGradients`, `D_DrawSpans16`, `Turbulent8`; the exact-perspective setting |
| `render::surf` | `r_surf.c`, `d_surf.c` | `R_TextureAnimation`, mip selection (`D_MipLevelForScale`), the surface cache (`D_CacheSurface`) |
| `render::light` | `r_surf.c`, `r_light.c` | `R_BuildLightMap`, `R_AddDynamicLights`, `R_MarkLights`, `R_LightPoint` |
| `render::sky` | `r_sky.c`, `d_sky.c` | the two-layer sky and its 32-pixel spans |
| `render::warp` | `d_scan.c`, `r_main.c` | liquid turbulence and the underwater `D_WarpScreen` |
| `render::alias` | `r_alias.c`, `r_aclip.c`, `r_main.c` | alias models and the gun: setup, lighting, bbox test, clipping |
| `render::polyse` | `d_polyse.c` | `D_PolysetDraw`, the affine triangle filler |
| `render::sprite` | `r_sprite.c`, `d_sprite.c` | sprites |
| `render::part` | `r_part.c`, `d_part.c` | drawing particles (`D_DrawParticle`) |
| `render::view` | `view.c` | `V_CalcBob`, the palette-shift ramps and gamma (`V_UpdatePalette`) as a `FramePalette`, `pack_rgba`, the gun's placement |
| `render::video` | (the port's own) | `VideoCvars`: views past id's 1280x1024 (`hires`) and Hor+ (`FovMode`); both off is Classic |
| `render::vis` | `model.c` | `Mod_PointInLeaf` |
| `render::world` | `r_main.c`, `d_edge.c` | the brush entities handed to the renderer, a face's gradients |
| `render::stats` | (the port's own; id has `r_speeds`) | per-phase timers and counters, off unless asked for |
| `render::fixtures`, `server::testutil` | — | test fixtures (test builds only) |
| `draw` | `draw.c` | pics, characters, strings, fade, tile clear; the scaled 2-D layer's whole scale |
| `screen` | `screen.c` | `SCR_CalcRefdef` (the view rectangle from `viewsize`), the composed frame, centre prints, the pause plaque |
| `sbar` | `sbar.c` | status bar, inventory, scoreboards, intermission and finale overlays |
| `menu` | `menu.c`, `vid_win.c` | every menu, the "Classic / 2026" settings page (`SETTING_ROWS`), and taps (`Menu::tap`) |
| `keys` | `keys.c`, `default.cfg` | key numbers and names, and `Bindings`: `default.cfg`'s, the 2026 WASD and gamepad layouts, `bind` lines |
| `console` | `console.c` | the drop-down console, `Con_Print`, the notify lines, history and completion |
| `cvar` | `cvar.c` | `Cvars`, every console variable as a typed field, and `CVARS`, the console's table of them (names, archive flags, the departures) |
| `cmd` | `cmd.c`, `common.c` | the command buffer's line splitting, `Cmd_TokenizeString` / `COM_Parse`, and the command table type |
| `settings` | (`config.cfg`) | the host session's settings (cvars and bindings), the Classic and 2026 profiles, and `config.cfg` as `Host_WriteConfiguration` writes it |
| `client` (`client/mod.rs`) | `client.h` | the game client's state (`Walk`, `DemoPlay`, `Vid`) and a frame's output (`ClientFrame`: the image, the frame palette, the sound calls) |
| `client::cl_main` | `cl_main.c`, `cl_parse.c`, `view.c`, `screen.c` | the live frame: the move into the server, the client side of the messages, `CL_RelinkEntities`, `V_CalcRefdef`, the screen |
| `client::cl_demo` | `cl_demo.c`, `cl_parse.c`, `view.c` | demo playback (`CL_ReadFromServer`, `CL_LerpPoint`, `CL_RelinkEntities`), the attract loop, and `timedemo` |
| `client::cl_tent` | `cl_tent.c`, `cl_main.c` | temp-entity effects and trails |
| `client::cl_input` | `cl_input.c` | the move from the held keys and the mouse (`CL_BaseMove`, `CL_AdjustAngles`, `+mlook`) |
| `client::in_win` | `in_win.c` | the joystick: `IN_StartupJoystick`, `Joy_AdvancedUpdate_f`, `IN_Commands`, `IN_JoyMove`; the 2026 pad's dead zone, curve, menu keys and rumble |
| `client::lerpmove` | (QuakeSpasm's `r_lerpmove`) | `r_lerpmove`: step movers glide between their steps (a 2026 setting) |
| `client::view` | `view.c` | `V_ParseDamage`, the view kick, `V_BonusFlash_f`, the colour shifts' fades |
| `client::host` | `host.c` | `Host_FilterTime` (the 72 fps gate, and the uncapped one), `Host_Error` |
| `client::host_cmd` | `host_cmd.c` | `map`, `changelevel`, `restart`, loading a save, and the cheats (`god`, `noclip`, `fly`, `kill`, `give`, `impulse`) |
| `bin/quaketool/` | — | the CLI below: `main.rs` (the command table), `assets`, `render`, `sim`, `census`, `play`, `timedemo`, `sound`, `framerate`, `video`, `entities` |

The crate is about 70,600 lines of Rust including its test modules. `cargo test --release`
runs 707 library, 6 `quaketool`, 8 integration tests and 1 doctest, none needing game
data. `../quake-wasm` adds 172 end-to-end tests against the real shareware pak.

## Checked against the shareware data

What `quaketool` prints for id's shareware `pak0.pak` (re-run on 2026-09-26):

- **PAK and CRC:** `quaketool ls` reports `339 files, dir crc 0x80d5, stock pak0`, id's
  `PAK0_COUNT` 339 and `PAK0_CRC` 32981.
- **BSP and MDL:** `e1m1.bsp` has 7,358 vertices and 5,516 faces; `player.mdl` 212 vertices
  and 408 triangles.
- **QuakeC:** `quaketool dis progs.dat` disassembles 2,091 functions and 20,940 statements.
- **Server:** `quaketool sim progs.dat e1m1.bsp 20` finds the floor 24 units below the spawn,
  spawns 336 entities from 369 blocks (33 skill-inhibited, 0 errors), leaves 163 live edicts
  after the lights remove themselves, and fires 450 thinks in 20 frames.
- **Player:** `quaketool playtest pak0.pak maps/e1m1.bsp` spawns the player with id's
  loadout (health 100, `items=0x1101`, shotgun, 25 shells) and runs 1040 units down the
  entrance hall in 40 frames.
- **Scene:** `quaketool scene` draws 29 alias models on e1m1 and 50 on `start`; it is the
  golden-render tool (`../README.md`, "How Classic is proven").
- **Demo:** `quaketool demo pak0.pak demo1.dem out` reads 972 server frames of id's demo1 on
  e1m3 and renders them.

## Runs in the browser

The library builds for `wasm32-wasip1` and `wasm32-wasip1-threads` unchanged. It reads
files through `std::fs` (the pak on demand, through the search path), draws 8-bit frames
and paints PCM, and it starts render threads with `std::thread::scope`. A thread that
will not start leaves its rows to the others, so a build without threads draws the same
frames. `../quake-wasm` is the WASI program around it: events on stdin, frames and sound
on stdout, saves and `config.cfg` as files. It has no exports, no dependencies and no
`unsafe`.

## Design

1. **No dependencies.** Only `std`; builds offline.
2. **`#![forbid(unsafe_code)]`.** id's C casts file buffers onto structs; here every field is
   read through a bounds-checked reader, so a bad file is an error, not undefined behaviour.
3. **Little-endian everywhere** through `from_le_bytes`, so it is right on any host.
4. **Classic is id's.** Where the port's behaviour could differ from id's, it follows
   the C, and the doc comments name the C function. Every deliberate departure is a
   setting (`cvar::CVARS` marks them), off in the Classic profile
   (`../AUDIT.md`, "The profiles and the departures").
5. **State lives with its owner.** The renderer, the server, the mixer and the host
   session own their state, so the renderer can run on several threads and the tests
   are isolated by construction. The thread-locals that remain are listed in
   `../CODE_PLAN.md`.

## Build and test

```sh
cargo build --release            # the library and quaketool
cargo test --release             # 707 + 6 + 8 + 1 tests, no game data needed
cargo run --release --bin quaketool -- --help
```

The tests use synthetic assets (`../gen_samples.py`, `../gen_progs.py` generate them), so
the suite runs without a copy of Quake.

## `quaketool`

`quaketool --help` lists every command with its arguments. They are one table
(`src/bin/quaketool/main.rs`, `COMMANDS`), which drives both the dispatch and the help.
By area:

- **id's files:** `info`, `ls`, `cat`, `bsp`, `map`, `mdl`, `spr`, `wad`, `dis`, `run`.
- **The renderer:** `render`, `render-demo`, `menu`, `scene` (the goldens), `view` (one
  exact view, for the C oracle), `shot` (the game screen at any size and video setting).
- **The server, headless:** `sim`, `simbench`, `playtest`, `changelevel`, `walk`,
  `demo`, `census`, `census-edicts`.
- **The game as the page runs it:** `play` (the browser's client, with frame hashes),
  `timedemo`, `sound` and `sndscript` (the mixer), `framerate` (60–480 Hz against 72).

Several take `[video options]` (`--video classic|modern`, `--hires`, `--fov-mode`,
`--display`, `--scaled2d`, `--threads N`); `--threads` never changes a pixel. `scene`
takes `QUAKE_BENCH=<iterations>`, `QUAKE_RES=WxH` and `QUAKE_DLIGHT=x,y,z,r|eye[:r]` for
timing and for injecting a dynamic light. `view` takes the oracle's options
(`../oracle/README.md`). `play` prints the same frame hashes as `../web/bench.py
--hash-every N`.

## Not here, on purpose

- **Multiplayer and netcode** (`net_*.c`): the server and client talk in-process.
- **The platform:** the window, input devices, the audio device, the disk. The engine
  paints PCM for the caller and names the CD track to play. The page plays the player's
  own track files.
- **`zone.c`:** Rust ownership replaces the allocator.

What is still unlike id's game is listed in `../AUDIT.md`, "Open, as of 2026-09-26".

## Licensing

The original Quake source is © 1996–1997 id Software, under the GNU General Public License v2.
This port is a derivative work, so GPL-2.0-or-later. Game data (`.pak` files) is not included
and not covered by the GPL.
