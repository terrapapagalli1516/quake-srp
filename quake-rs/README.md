# quake-rs

A **faithful, memory-safe Rust port of id Software's _Quake_** (1996) engine for single-player,
ported directly from the GPLv2 C source at [`id-Software/Quake`](https://github.com/id-Software/Quake).

> **Honest scope.** This began as a port of the tractable, verifiable-in-isolation layers (math + every
> on-disk format) and grew, subsystem by audited subsystem, into the complete single-player engine: the
> QuakeC VM and builtins, the server (spawn, physics, collision, AI, combat, changelevel + intermission),
> a faithful software renderer (lightmaps, the lit-surface cache, dynamic lights with `R_MarkLights`
> gating, warp/sky, particles, beams, viewmodel, HUD/menus/console), demo playback, and sound event/
> ambient-loop plumbing (mixing itself is the host's job — the browser uses Web Audio). What it does NOT
> do: multiplayer/netcode, CD audio. Every subsystem was audited against the original C
> (66-finding ledger + seven review rounds in `../AUDIT.md`).

## Status

| Subsystem | C origin | Rust module | Status |
|-----------|----------|-------------|--------|
| Vector / matrix / angle math, `BoxOnPlaneSide` | `mathlib.c` | `math` | ✅ ported + tested |
| CRC-16/CCITT (PAK integrity) | `crc.c` | `crc` | ✅ ported + tested |
| WAD2 archive (palette, pics, fonts) | `wad.c` | `wad` | ✅ ported + tested |
| PAK archive (`pak0.pak`, `pak1.pak`) | `common.c` | `pak` | ✅ ported + tested |
| BSP v29 map loader | `bspfile.h`, `model.c` | `bsp` | ✅ ported + tested |
| MDL alias-model loader | `modelgen.h`, `model.c` | `mdl` | ✅ ported + tested |
| SPR sprite loader | `spritegn.h`, `model.c` | `spr` | ✅ ported + tested |
| QuakeC bytecode format + disassembler | `pr_comp.h`, `progs.h`, `pr_edict.c` | `progs` | ✅ ported + tested |
| QuakeC virtual machine (interpreter) | `pr_exec.c`, `pr_edict.c` | `vm` | ✅ ported + tested |
| QuakeC builtins | `pr_cmds.c` | `builtins`, `server::pr_cmds` (+ `server::msg` / `lightstyle` / `host` / `sv_move`) | ✅ pure builtins + engine builtins (setmodel/setorigin/precache/traceline/droptofloor/pointcontents/…); each engine builtin lives with the server state it drives |
| BSP collision hull trace | `world.c` | `world` | ✅ `SV_RecursiveHullCheck` + `SV_HullPointContents` (hulls 0/1/2), box trace |
| Player movement (slide + walk) | `sv_phys.c` | `world` | ✅ `ClipVelocity` + `SV_FlyMove` slide, stair step-up, ground-snap |
| Demo + net protocol playback | `cl_demo.c`, `cl_parse.c`, `protocol.h` | `demo` | ✅ `.dem` framing + `svc_*` demux + bit-packed entity deltas → per-frame snapshots |
| Server: spawn, physics, AI, combat, client | `server.h` + the files below | `server` (`server/mod.rs`: `Server`, `WorldModel` host, reports, `UserCmd`, re-exports) | ✅ full single-player tick: spawn + settle, walk/toss/bounce/fly/pusher physics, entity collision + touch, monster AI, combat, player client (incl. death→respawn), changelevel + intermission svc flow, signon settle frames |
| ↳ level bring-up, client connect | `sv_main.c`, `cl_main.c` | `server::sv_main` | ✅ `SV_SpawnServer`, `SV_ConnectClient` (+ `Host_Spawn_f`), `SV_CleanupEnts`, `EF_*` entity dlights |
| ↳ entity spawning | `pr_edict.c`, `common.c` | `server::pr_edict` | ✅ `ED_LoadFromFile` (skill filter, settle frames), `ED_ParseEdict` / `ED_ParseEpair`, `COM_Parse` |
| ↳ engine builtins | `pr_cmds.c` | `server::pr_cmds` | ✅ the world-touching `PF_*` (setmodel/setorigin/precache/traceline/droptofloor/aim/checkclient/findradius/…) + the `pr_builtin[]` install |
| ↳ physics tick | `sv_phys.c` | `server::sv_phys` | ✅ `SV_Physics` + `SV_Physics_Client`, movetypes, `SV_PushMove`, `SV_WalkMove` / `SV_FlyMove`, water checks |
| ↳ player input → velocity | `sv_user.c`, `view.c` | `server::sv_user` | ✅ `SV_ClientThink` / `SV_AirMove`, friction + acceleration, `SV_WaterMove` / `SV_WaterJump`, `SV_SetIdealPitch`, `V_CalcRoll` |
| ↳ entity collision + touch | `world.c`, `sv_phys.c` | `server::sv_world` | ✅ `SV_Move` vs world + every solid edict (abs-box broadphase), `SV_LinkEdict` bounds, `SV_TouchLinks`, `SV_Impact` |
| ↳ monster movement | `sv_move.c`, `pr_cmds.c` | `server::sv_move` | ✅ `SV_movestep`, `SV_StepDirection`, `SV_NewChaseDir`, `SV_MoveToGoal`, `SV_CheckBottom`; `walkmove` / `movetogoal` / `checkbottom` |
| ↳ server→client messages | `sv_main.c`, `pr_cmds.c`, `cl_tent.c`, `cl_parse.c` | `server::msg` | ✅ sound / static-sound / particle / print queues; `Write*` → temp-entity decoder + `MSG_ALL` svc recogniser (intermission, finale) |
| ↳ light styles | `pr_cmds.c`, `r_light.c` | `server::lightstyle` | ✅ `PF_lightstyle` table + `R_AnimateLight` scales (shared with demo playback) |
| ↳ host commands | `host_cmd.c`, `sv_main.c` | `server::host` | ✅ skill, deferred `changelevel` / `restart`, `SV_SaveSpawnparms` + serverflags across levels, `Host_Kill_f`, signon frames |
| Software renderer: `Image`, `Camera`, the `render_scene*` entry points (`R_RenderView`) | `r_main.c` | `render` (`render/mod.rs`) | ✅ perspective-correct textured world, baked lightmaps + lit-surface cache, dynamic lights w/ `R_MarkLights` BSP gating, animated styles, liquid/sky warp, alias/sprite/brush models, viewmodel |
| ↳ head-bob, screen blends, gamma, gun placement | `view.c` | `render/view.rs` | ✅ |
| ↳ world, inline submodels, `b_*.bsp` item boxes | `r_bsp.c` | `render/world.rs` | ✅ |
| ↳ triangle rasterisers (the port's own, in place of the edge/span pipeline) | `r_edge.c`, `d_scan.c` | `render/raster.rs` | ✅ |
| ↳ lightmaps, light styles, dynamic lights, `R_LightPoint` | `r_surf.c`, `r_light.c` | `render/light.rs` | ✅ |
| ↳ face geometry, `R_TextureAnimation`, surface caches | `r_surf.c`, `d_surf.c` | `render/surf.rs` | ✅ |
| ↳ liquid turbulence, `D_WarpScreen` | `d_scan.c` | `render/warp.rs` | ✅ |
| ↳ scrolling sky | `r_sky.c`, `d_sky.c` | `render/sky.rs` | ✅ |
| ↳ PVS, view frustum, near-plane clip | `model.c`, `r_main.c` | `render/vis.rs` | ✅ |
| ↳ alias models + the weapon | `r_alias.c`, `r_aclip.c` | `render/alias.rs` | ✅ |
| ↳ alias triangle filler | `d_polyse.c` | `render/polyse.rs` | ✅ |
| ↳ sprites / particle drawing | `r_sprite.c` / `r_part.c` | `render/sprite.rs`, `render/part.rs` | ✅ |
| ↳ render profiler | (the port's own) | `render/stats.rs` | ✅ |
| 2-D primitives: pics, characters, fade, tile clear | `draw.c` | `draw` | ✅ |
| Screen layout (`scr_viewsize` → view rect), centre print | `screen.c` | `screen` | ✅ |
| Status bar, scoreboard, intermission + finale overlays | `sbar.c` | `sbar` | ✅ |
| Menus (main, single player, load/save, options, keys, video, help, quit) | `menu.c` | `menu` | ✅ |
| Key numbers, names, default binds | `keys.c` | `keys` | ✅ |
| Drop-down console, notify lines | `console.c` | `console` | ✅ |
| Particles + temp entities | `r_part.c`, `cl_tent.c` | `particles`, `tent`, `dlight` | ✅ trails/explosions/splashes + lightning-beam store and expansion |
| Ambient sound | `snd_dma.c`, `snd_mem.c`, `pr_cmds.c` | `snd`, `server::msg` | ✅ `S_UpdateAmbientSounds` (leaf ambients, integer ramp at the 72 fps cap) + `PF_ambientsound` static loops + `GetWavinfo` cue-loop gate |
| Little-endian byte reader, error type | (replaces `LittleLong`/`Sys_Error`) | `read`, `error` | ✅ scaffold |

The crate is **~39,000 lines of zero-dependency, `unsafe`-free Rust with ~580 lib + 8 integration tests**
(all data-free; the sibling `quake-wasm` crate adds ~120 e2e tests against the real embedded shareware pak).

### Validated against the real Quake shareware

Run against id's freely-redistributable shareware `pak0.pak` (`quake106.zip` → `resource.1` → `id1/pak0.pak`):

- **PAK + CRC** — `quaketool ls` reports `339 files, dir crc 0x80d5, stock pak0` — an exact match for id's
  `PAK0_COUNT=339` / `PAK0_CRC=32981`, validating the archive parser and the CRC-16/CCITT port byte-for-byte.
- **BSP / MDL** — `e1m1.bsp` parses to 7,358 verts / 5,516 faces / 81 named textures with the entity scanner
  tallying real classnames (190 lights, 34 grunts, doors…); `player.mdl` → 212 verts, 408 tris, 143 frames.
- **QuakeC VM** — `quaketool dis progs.dat` disassembles all 2,091 functions / 20,940 statements; `run` executes
  real game code (`worldspawn` ran 5,308 statements before reaching an unimplemented engine builtin, which
  faults cleanly rather than crashing).
- **Renderer** — `quaketool render maps/e1m1.bsp out.ppm gfx/palette.lmp` produces a recognisable first-person
  view of *Slipgate Complex* with the real wall/floor textures, drawn from the player spawn point.
- **Server (collision + spawn + physics)** — `quaketool sim progs.dat maps/e1m1.bsp 20` runs id's real game
  logic: a hull trace from the spawn finds the floor 24 units below (`normal [0,0,1]`); `ED_LoadFromFile` spawns
  336 entities (0 spawn errors) by executing each entity's real QuakeC spawn function — the plain `light`
  entities then remove themselves exactly as in Quake, leaving 161 live; and 20 physics frames fire 443 think
  calls (torches animating, doors waiting, monster AI ticking) with 0 errors.
- **Scene (models in the world)** — `quaketool scene pak0.pak maps/e1m1.bsp out.ppm` spawns the map, loads each
  spawned entity's `.mdl` from the PAK, and draws it at its world origin/angle into the textured scene sharing
  the world z-buffer: e1m1 renders with an `army` grunt standing in its first room (real `progs/soldier.mdl`),
  the start hub with its `zombie`s — 29 and 50 model instances respectively, 0 failed loads.
- **Walk (movement + collision)** — `quaketool walk pak0.pak maps/e1m1.bsp out 40` drops the player at the spawn
  and walks forward one step per frame via the world slide-move: across 40 frames it advances 720 units down the
  entrance hall and descends 64 units into the first room (stepping/ground-snapping down the slope), rendering
  each frame — a collision-driven first-person walkthrough.
- **Demo playback** — `quaketool demo pak0.pak demo1.dem out` replays id's recorded attract-mode demo: it parses
  975 server frames out of the `.dem` net stream (no desync), reconstructs the camera path and every entity's
  model/origin/angle from the bit-packed updates, and renders the run through `maps/e1m3.bsp` (the Necropolis)
  with its monsters and items — id's own title-screen demo, played back and rendered by this crate.

### Runs in the browser (WebAssembly)

The engine lib compiles to `wasm32-unknown-unknown` **unchanged** (`cargo build --lib --target wasm32-unknown-unknown`)
— the payoff of zero dependencies + an all-in-memory API (`Pak::from_bytes`, loaders over `&[u8]`, the renderer's
`Vec<[u8;3]>` framebuffer). The sibling `quake-wasm` crate is the `cdylib` shell (~5.4k lines incl. its e2e tests)
that `include_bytes!`s the pak, owns the walk/demo/menu/console front-end state, and exports plain `extern "C"`
functions — **no `wasm-bindgen`, no dependencies, and not a single `unsafe {}` block** (the only "unsafe" is the
`#[no_mangle]` export attribute; the page reads the framebuffer out of linear memory itself, Rust only hands out
`Vec::as_ptr()`). `web/index.html` boots it, runs a `requestAnimationFrame` loop blitting the framebuffer to a
`<canvas>`, and maps the full control set (mouse-look, WASD, weapons, menu, console). Verified continuously in
headless Chromium (`web/verify_walk.py`, `web/verify_ambient.py`): **Quake e1m1, playable, in a browser, in safe
Rust.**

### Gameplay (entity collision, touch, player client)

The server now models **entity-vs-entity collision** (`sv_move` clips against monster/item boxes and door
submodels), **touch/impact** (`sv_impact`, trigger/item touch), and a **real player client**: `connect_client`
runs id's `PutClientInServer`, and `client_frame` runs `SV_ClientThink` movement (friction/acceleration, entity-aware
slide + stair step-up) plus `PlayerPreThink`/`PostThink`. Verified on real e1m1 (`quaketool playtest`):

```
playtest maps/e1m1.bsp: 336 entities spawned; player = edict 161
  loadout after PutClientInServer:  health=100  items=0x1101  weapon=1  shells=25
  walked 40 frames (4.0s): moved 1040 units (down the entrance hall) ... health 100
  950 entity/monster thinks fired during play
```

i.e. the player spawns with id's real starting loadout (shotgun + axe + 25 shells, `items=0x1101`, 100 health),
faces the `info_player_start` angle, and runs the full length of the entrance hall (~1040 units) with real
`SV_ClientThink` physics, colliding with the world (no tunnelling, health intact) while 950 entity/monster thinks
fire around it. The single-player server→client path is the in-process short-circuit (render straight from the live
server's player view — no netcode serialization).

### Monster movement (#5)

The monster-AI movement builtins are faithfully ported from `sv_move.c`/`pr_cmds.c`: `SV_movestep`
(step-up/forward/down over the entity-aware trace), `SV_StepDirection`, `SV_NewChaseDir` (the 8-way chase search
with random symmetry-break and turnaround bias), `SV_MoveToGoal`, `SV_CheckBottom`, wired to builtins
`walkmove`(#32), `movetogoal`(#67), `checkbottom`(#40). `checkclient`(#17) returns the live `FL_CLIENT` player by
line-of-sight and `findradius`(#22) chains entities within a radius — so QuakeC `ai.qc`/`fight.qc` can wake, target
and chase. *Not yet visually demonstrated:* a monster actively chasing across the room (needs the player to cross a
monster's sight line during a longer play session).

## Design principles

1. **Zero external dependencies.** Only the Rust standard library. `cargo build` works fully offline; there
   is no version drift. Little-endian decoding uses the built-in `*::from_le_bytes`.
2. **`#![forbid(unsafe_code)]`.** The original C casts a raw file buffer straight onto a struct pointer
   (`(dheader_t *)mod_base`). That is undefined behaviour on misaligned or truncated input. Here every field
   is decoded explicitly through a bounds-checked `read::Reader`, so a malformed file returns a `QError`
   instead of corrupting memory.
3. **Endian-correct everywhere.** Quake's `LittleLong`/`LittleShort` byte-swaps (needed on big-endian hosts)
   are subsumed by `from_le_bytes`, so the loaders are correct on any platform.
4. **Faithful first, idiomatic second.** Struct sizes, field order, and parse order match the C exactly; the
   public surface is idiomatic Rust (owned `Vec`s, `Result`, enums for the tagged frame/skin groups).

## Build & test

```sh
cargo build              # builds the library and the `quaketool` binary
cargo test               # runs all unit + integration tests (no Quake data required)
cargo run --bin quaketool -- <file>     # inspect a real Quake asset
```

The tests are self-contained: each loader is exercised against synthetic in-memory files, so the suite
passes without owning a copy of Quake. To point `quaketool` at real data, use the `pak0.pak` from any Quake
install (shareware or retail).

## `quaketool` — the asset inspector

`quaketool` is a small CLI that ties the loaders together and demonstrates them on real files:

```
quaketool info   <file>          # auto-detect format (PAK/WAD2/BSP/MDL/SPR) and print a summary
quaketool ls     <pak>           # list the directory of a PAK archive
quaketool cat    <pak> <name>    # extract one file from a PAK to stdout
quaketool bsp    <file.bsp>      # dump BSP lump counts, model bounds, entity keys
quaketool map    <file.bsp>      # render a top-down ASCII minimap from the BSP vertices
quaketool mdl    <file.mdl>      # dump alias-model header, skins, frames
quaketool spr    <file.spr>      # dump sprite header and frames
quaketool wad    <file.wad>      # list WAD2 lumps
quaketool dis    <progs.dat>     # disassemble QuakeC bytecode
quaketool run    <progs.dat> <fn> # execute a QuakeC function; show console output + return value
quaketool render <bsp> <out.ppm> # software-render a BSP to a PPM image
quaketool render-demo <out.ppm>  # render the built-in demo room (no map data needed)
quaketool menu <pak> <out.ppm>   # draw the MAIN menu over the e1m1 POV
quaketool sim <progs.dat> <bsp> [frames]   # spawn a map's QuakeC entities + tick physics
quaketool scene <pak> <map> <out.ppm>      # render a map + its spawned MDL entities (golden-render tool)
quaketool walk <pak> <map> <out> [steps]   # collision-driven walkthrough, one PPM per step
quaketool demo <pak> <dem> <out> [stride]  # replay + render a recorded demo
quaketool playtest <pak> <map> [out.ppm]   # spawn a player client, walk, report state
quaketool simbench <pak> <map> [frames]    # benchmark the game-logic tick (no rendering)
quaketool changelevel <pak> <map>          # drive a player through the exit; prove inventory carries
```

`scene` doubles as the **golden-render harness** (`QUAKE_BENCH=<iters>` / `QUAKE_RES=WxH` /
`QUAKE_DLIGHT=x,y,z,r|eye[:r]` env knobs) — see `../STATUS.md` for the current golden hashes and
performance scorecard.

The repo includes `gen_samples.py` / `gen_progs.py` — independent Python assemblers that emit synthetic
assets and a `progs.dat`, so the loaders, VM, and renderer can be exercised without owning a copy of Quake.

## What is *not* here — deliberate scope

Of the original phase roadmap (formats → model → server/client → renderer/sound → host), **every layer the
single-player game needs is now ported and audited**. The deliberate omissions:

- **Multiplayer / netcode** (`net_*.c`) — this is an in-process single-player engine; the server→client path
  is the local short-circuit, no serialization layer.
- **Save / load** (`host_cmd.c`'s `Host_Savegame_f`/`Host_Loadgame_f`) — out of scope; `changelevel` carries
  inventory via the real `SetChangeParms`/`DecodeLevelParms` and death restarts the level like the C.
- **Audio mixing internals** (`snd_mix.c`) — the engine computes *which* sounds play where, at what
  volume/attenuation (including static loops and leaf ambients); the host mixes (the browser uses Web Audio).
  A documented scope note in `../AUDIT.md`.
- **CD audio** (`cd_*.c`) — `svc_cdtrack` is recognised and ignored; there is no CD.
- **`zone.c`/`cmd.c`/`cvar.c` as literal ports** — Rust ownership replaces the allocator; the console/cvar
  surface is implemented where game-visible (skill, the console commands, Options sliders) rather than as a
  general registry.

The remaining *fidelity* gaps are a documented LOW tail (plus one narrow no-lightmap-face MED) — see
"Still open" in `../AUDIT.md`.

## Licensing

The original Quake source is © 1996–1997 id Software, released under the **GNU General Public License v2**.
This port is a derivative work and is therefore distributed under **GPL-2.0-or-later**. Game *data*
(`.pak` files, maps, models) is not included and is not covered by the GPL.
