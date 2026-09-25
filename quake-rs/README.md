# quake-rs

The engine crate of quake-rust: a Rust port of id Software's *Quake* (1996) for single
player, ported from id's GPLv2 WinQuake C source
([`id-Software/Quake`](https://github.com/id-Software/Quake)). A library (`quake_rs`) plus
the `quaketool` CLI. Only the standard library; `#![forbid(unsafe_code)]`.

It holds the whole game except the platform: the file formats, the QuakeC VM and builtins,
the server (spawn, physics, collision, AI, combat, changelevel, intermission, savegames), the
software renderer, the 2-D layer (status bar, menus, console), the game client (the live
frame and demo playback) and the sound decisions (which sound plays where, how loud). The
platform — a window or canvas, input, the audio mixer, storage — is the caller's: the
browser shell is `../quake-wasm`, and `quaketool play` runs the same client natively. Not
here: multiplayer and netcode, CD audio. The faithfulness ledger is `../AUDIT.md`.

## Modules

Every module in `src/`, with the id source it ports. Modules are named after id's files
where one file maps to one module.

| module | id's C | what |
|---|---|---|
| `math` | `mathlib.c` | vectors, matrices, angles, `BoxOnPlaneSide` |
| `crc` | `crc.c` | CRC-16/CCITT (the PAK check) |
| `read`, `error` | (`LittleLong`, `Sys_Error`) | the bounds-checked little-endian reader, the error type |
| `pak` | `common.c` | PAK archives, including the embedded-image variant the browser uses |
| `wad` | `wad.c` | WAD2 archives (`gfx.wad`: palette, pics, fonts) |
| `bsp` | `bspfile.h`, `model.c` | BSP v29 maps, with the mip levels and `Mod_DecompressVis` |
| `mdl` | `modelgen.h`, `model.c` | alias models |
| `spr` | `spritegn.h`, `model.c` | sprites |
| `progs` | `pr_comp.h`, `progs.h`, `pr_edict.c` | the `progs.dat` format and a disassembler |
| `vm` | `pr_exec.c`, `pr_edict.c` | the QuakeC interpreter, edicts and strings; entity fields resolved once per progs (`Vm::fo`) |
| `builtins` | `pr_cmds.c` | the builtins that need no map: maths, strings, prints, `spawn`/`remove`/`find`, `random`, `stuffcmd` |
| `world` | `world.c`, `sv_phys.c` | BSP hull traces (`SV_RecursiveHullCheck`), the box hull, point contents, and the world-only slide move (`ClipVelocity`, `SV_FlyMove`) the `walk` tool uses |
| `server` (`server/mod.rs`) | `server.h` | the `Server`: a VM with the engine builtins over a loaded map, and its reports |
| `server::sv_main` | `sv_main.c` | `SV_SpawnServer`, `SV_ConnectClient` (with `Host_Spawn_f`), `SV_CleanupEnts`, `SV_FatPVS` and which entities the client is sent, entity dynamic lights |
| `server::pr_edict` | `pr_edict.c`, `common.c` | `ED_LoadFromFile` and the entity-text parsers |
| `server::pr_cmds` | `pr_cmds.c` | the builtins that touch the world (`setmodel`, `traceline`, `droptofloor`, `aim`, `checkclient`, `findradius`, …) and the `pr_builtin[]` install |
| `server::sv_phys` | `sv_phys.c` | `SV_Physics`: thinks, every movetype, pushers, the player's move |
| `server::sv_user` | `sv_user.c` | `SV_ClientThink`, friction and acceleration, swimming, `SV_SetIdealPitch` |
| `server::sv_world` | `world.c` | `SV_Move` against the world and every solid edict, `SV_LinkEdict`, touching, `SV_Impact` |
| `server::sv_move` | `sv_move.c` | monster movement: `SV_movestep`, `SV_NewChaseDir`, `SV_MoveToGoal`, `SV_CheckBottom` |
| `server::msg` | `sv_main.c`, `pr_cmds.c`, `cl_parse.c` | the server-to-client messages without a network: sound, particle and print queues, the `Write*` builtins parsed per buffer (temp entities, intermission, finale) |
| `server::lightstyle` | `pr_cmds.c`, `r_light.c` | `PF_lightstyle`'s table and `R_AnimateLight`'s scales |
| `server::host` | `host_cmd.c`, `sv_main.c` | `skill`, `sv_gravity`, the deferred `changelevel`/`restart`, spawn parms and serverflags, `Host_Kill_f`, the signon settle frames |
| `save` | `host_cmd.c` | savegames: `Host_Savegame_f`'s `.sav` text and `Host_Loadgame_f`'s parse |
| `demo` | `cl_demo.c`, `cl_parse.c`, `protocol.h` | the `.dem` framing and the network-message decoder |
| `particles` | `r_part.c` | the particle spawners and their motion |
| `tent` | `cl_tent.c` | beams: `CL_ParseBeam`'s slots and `CL_UpdateTEnts`' bolt pieces |
| `dlight` | `cl_main.c` | the dynamic-light pool: `CL_AllocDlight`, `CL_DecayLights` |
| `snd` | `snd_dma.c`, `snd_mem.c` | what reaches the mixer: `SND_PickChannel`'s override, the view-entity rule, static loops, the leaf ambients (`S_UpdateAmbientSounds`), `GetWavinfo`'s loop points |
| `render` (`render/mod.rs`) | `r_main.c` | the 3-D view's entry points (`R_RenderView`), the camera and projection (`R_ViewChanged`), the frame's buffers |
| `render::edge` | `r_bsp.c`, `r_draw.c`, `r_edge.c`, `d_edge.c` | the world and brush models: the BSP walk, edge clipping and the edge cache, `R_ScanEdges`, `D_DrawSurfaces`, the 16-bit z-buffer |
| `render::raster` | `d_edge.c`, `d_draw16.s`, `d_scan.c` | the span routines: `D_CalcGradients`, `D_DrawSpans16`, `Turbulent8`; the exact-perspective extra |
| `render::surf` | `r_surf.c`, `d_surf.c` | `R_TextureAnimation`, mip selection (`D_MipLevelForScale`), the surface cache (`D_CacheSurface`) |
| `render::light` | `r_surf.c`, `r_light.c` | `R_BuildLightMap`, `R_AddDynamicLights`, `R_MarkLights`, `R_LightPoint` |
| `render::sky` | `r_sky.c`, `d_sky.c` | the two-layer sky and its 32-pixel spans |
| `render::warp` | `d_scan.c`, `r_main.c` | liquid turbulence and the underwater `D_WarpScreen` |
| `render::alias` | `r_alias.c`, `r_aclip.c`, `r_main.c` | alias models and the gun: setup, lighting, bbox test, clipping |
| `render::polyse` | `d_polyse.c` | `D_PolysetDraw`, the affine triangle filler |
| `render::sprite` | `r_sprite.c`, `d_sprite.c` | sprites |
| `render::part` | `r_part.c`, `d_part.c` | drawing particles (`D_DrawParticle`) |
| `render::view` | `view.c` | `V_CalcBob`, the palette-shift ramps (`V_UpdatePalette`), gamma, the gun's placement |
| `render::vis` | `model.c` | `Mod_PointInLeaf` |
| `render::world` | `r_main.c`, `d_edge.c` | the brush entities handed to the renderer, a face's gradients |
| `render::stats` | (the port's own; id has `r_speeds`) | per-phase timers and counters, off unless asked for |
| `render::fixtures`, `server::testutil` | — | test fixtures (test builds only) |
| `draw` | `draw.c` | pics, characters, strings, fade, tile clear |
| `screen` | `screen.c` | `SCR_CalcRefdef` (the view rectangle from `viewsize`), the composed frame, centre prints |
| `sbar` | `sbar.c` | status bar, inventory, scoreboards, intermission and finale overlays |
| `menu` | `menu.c`, `vid_win.c` | every menu, and the Web extras page (`WEB_EXTRAS`) |
| `keys` | `keys.c`, `default.cfg` | key numbers, names, default bindings |
| `console` | `console.c` | the drop-down console, `Con_Print`, the notify lines |
| `client` (`client/mod.rs`) | `client.h` | the game client's state (`Walk`, `DemoPlay`) and a frame's output (`ClientFrame`: the image, the palette shifts, the sound calls) |
| `client::cl_main` | `cl_main.c`, `cl_parse.c`, `view.c`, `screen.c` | the live frame: the move into the server, the client side of the messages, `CL_RelinkEntities`, `V_CalcRefdef`, the screen |
| `client::cl_demo` | `cl_demo.c`, `cl_parse.c`, `view.c` | demo playback and the attract loop |
| `client::cl_tent` | `cl_tent.c`, `cl_main.c` | temp-entity effects and trails |
| `client::cl_input` | `cl_input.c` | the move from the held keys and the mouse (`CL_BaseMove`, `CL_AdjustAngles`) |
| `client::view` | `view.c` | `V_ParseDamage`, the view kick, `V_BonusFlash_f` |
| `client::host` | `host.c` | `Host_FilterTime`, the 72 fps gate |
| `client::host_cmd` | `host_cmd.c` | `map`, `changelevel`, `restart`, loading a save, and the cheats (`god`, `noclip`, `fly`, `kill`, `give`, `impulse`) |
| `bin/quaketool.rs` (+ `census.rs`, `play.rs`) | — | the CLI below |

The crate is about 58,000 lines, about 36,000 of them outside the test modules. `cargo test
--release` runs 581 library, 1 `quaketool` and 8 integration tests, none needing game data;
`../quake-wasm` adds 118 end-to-end tests against the real shareware pak.

## Checked against the shareware data

What `quaketool` prints for id's shareware `pak0.pak` (re-run on 2026-09-25):

- **PAK and CRC:** `quaketool ls` ends `339 files, dir crc 0x80d5, stock pak0`, id's
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
  golden-render tool (`../README.md`, "How it is checked").
- **Demo:** `quaketool demo pak0.pak demo1.dem out` reads 972 server frames of id's demo1 on
  e1m3 and renders them.

## Runs in the browser

The library compiles to `wasm32-unknown-unknown` unchanged. It keeps everything in memory
(`Pak::from_bytes`/`from_static`, loaders over `&[u8]`, an RGB framebuffer), so the
browser shell (`../quake-wasm`) only adds the exports the page calls, the host state and
the platform's sound and storage. It has no `wasm-bindgen`, no dependencies and no
`unsafe` block.

## Design

1. **No dependencies.** Only `std`; builds offline.
2. **`#![forbid(unsafe_code)]`.** id's C casts file buffers onto structs; here every field is
   read through a bounds-checked reader, so a bad file is an error, not undefined behaviour.
3. **Little-endian everywhere** through `from_le_bytes`, so it is right on any host.
4. **Faithful first.** Where the port's behaviour could differ from id's, it follows the C,
   and the doc comments name the C function. Deliberate departures are opt-in extras
   (`../README.md`, "The rule").

## Build and test

```sh
cargo build --release            # the library and quaketool
cargo test --release             # 590 tests, no game data needed
cargo run --release --bin quaketool -- --help
```

The tests use synthetic assets (`../gen_samples.py`, `../gen_progs.py` generate them), so
the suite runs without a copy of Quake.

## `quaketool`

```
quaketool info   <file>                     # detect the format (PAK/WAD2/BSP/MDL/SPR), summarize
quaketool ls     <pak>                      # list a PAK
quaketool cat    <pak> <name>               # one file from a PAK to stdout
quaketool bsp    <file.bsp>                 # lump counts, models, entities
quaketool map    <file.bsp>                 # ASCII top-down map
quaketool mdl    <file.mdl>                 # alias-model header, skins, frames
quaketool spr    <file.spr>                 # sprite header, frames
quaketool wad    <file.wad>                 # WAD2 lumps
quaketool dis    <progs.dat>                # disassemble QuakeC
quaketool run    <progs.dat> <fn>           # run one QuakeC function (no engine builtins)
quaketool render <bsp> <out.ppm> [palette]  # render a BSP
quaketool render-demo <out.ppm>             # render the built-in test room
quaketool menu <pak> <out.ppm>              # the main menu over the e1m1 view
quaketool sim <progs.dat> <bsp> [frames]    # spawn a map and tick its physics
quaketool scene <pak> <map> <out.ppm>       # a map with its spawned models: the golden renders
quaketool view <pak> <map> <out.ppm> [...]  # one exactly specified view, for the oracle
quaketool walk <pak> <map> <out> [steps]    # walk forward from the spawn, a PPM per step
quaketool demo <pak> <dem> <out> [stride]   # replay and render a demo
quaketool playtest <pak> <map> [out.ppm]    # spawn a player, walk, report
quaketool simbench <pak> <map> [frames]     # time the game logic alone
quaketool changelevel <pak> <map>           # run the player through the exit, check the inventory carries
quaketool census <pak> [map ...]            # headless playthrough of start and e1m1..e1m8 (../CENSUS.md)
quaketool census-edicts <pak> <map> <t,..>  # the port's edicts at server times, for the oracle diff
quaketool play <pak> <workloads> [frames] [--res WxH,..] [--hash-every N] [--ppm PREFIX]
                                            # the browser's game client, natively
```

`scene` takes `QUAKE_BENCH=<iterations>`, `QUAKE_RES=WxH` and `QUAKE_DLIGHT=x,y,z,r|eye[:r]`
for timing and for injecting a dynamic light. `view` takes the oracle's options
(`../oracle/README.md`). `play` prints the same frame hashes as `../web/bench.py
--hash-every N`.

## Not here, on purpose

- **Multiplayer and netcode** (`net_*.c`): the server and client talk in-process.
- **Audio mixing** (`snd_mix.c`): the engine decides which sounds play, where and how loud;
  the platform mixes them (Web Audio in the browser).
- **CD audio** (`cd_*.c`): `svc_cdtrack` is read and ignored.
- **`zone.c`, `cmd.c`, `cvar.c` as such:** Rust ownership replaces the allocator, and the
  console commands and cvars exist where the game uses them, not as a general registry.

What is still unlike id's game is listed in `../AUDIT.md`, "Open, as of 2026-09-25".

## Licensing

The original Quake source is © 1996–1997 id Software, under the GNU General Public License v2.
This port is a derivative work, so GPL-2.0-or-later. Game data (`.pak` files) is not included
and not covered by the GPL.
