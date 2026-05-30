# quake-rs

A **faithful, memory-safe Rust port of the self-contained subsystems of id Software's _Quake_** (1996),
ported directly from the GPLv2 C source at [`id-Software/Quake`](https://github.com/id-Software/Quake).

> **Honest scope.** The full Quake engine is ~100,000 lines of C (software + GL renderers, client,
> server, netcode, sound, the QuakeC virtual machine, physics). Nobody ports all of that, correctly and
> idiomatically, in one sitting. This crate ports the layers that are **tractable and verifiable in
> isolation** — the math library and every on-disk asset format — with unit tests against the exact byte
> layouts, and documents the rest as a concrete roadmap. These are the foundation the rest of the engine
> reads through, so they are the right place to start a real port.

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
| QuakeC builtins | `pr_cmds.c` | `builtins`, `server` | ✅ pure builtins + engine builtins (setmodel/setorigin/precache/traceline/droptofloor/pointcontents/…) |
| BSP collision hull trace | `world.c` | `world` | ✅ `SV_RecursiveHullCheck` + `SV_HullPointContents` (hulls 0/1/2), box trace |
| Player movement (slide + walk) | `sv_phys.c` | `world` | ✅ `ClipVelocity` + `SV_FlyMove` slide, stair step-up, ground-snap |
| Demo + net protocol playback | `cl_demo.c`, `cl_parse.c`, `protocol.h` | `demo` | ✅ `.dem` framing + `svc_*` demux + bit-packed entity deltas → per-frame snapshots |
| Server: map spawn + physics | `pr_edict.c`, `sv_phys.c` | `server` | ✅ `ED_LoadFromFile` spawn + minimal `SV_Physics` tick |
| Software renderer (from-data BSP rasteriser) | new (not a port of `d_*.c`) | `render` | ✅ z-buffer, backface cull, **perspective-correct textured** world, **baked BSP lightmaps**, **alias models in-scene** |
| Little-endian byte reader, error type | (replaces `LittleLong`/`Sys_Error`) | `read`, `error` | ✅ scaffold |

The crate is **~11,000 lines of zero-dependency, `unsafe`-free Rust with 150+ tests**.

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
`Vec<[u8;3]>` framebuffer). The sibling `quake-wasm` crate is a ~120-line `cdylib` shell that `include_bytes!`s the
pak and exports `boot`/`step`/`width`/`height`/`framebuffer`/`set_input` via plain `extern "C"` — **no `wasm-bindgen`,
no dependencies, and not a single `unsafe {}` block** (the only "unsafe" is the `#[no_mangle]` export attribute; the
page reads the framebuffer out of linear memory itself, Rust only hands out `Vec::as_ptr()`). `web/index.html` boots
it, runs a `requestAnimationFrame` loop blitting the framebuffer to a `<canvas>`, and maps WASD/arrows to the
slide-move player. Verified running interactively in headless chromium (`web/shoot.py`): **Quake e1m1, walkable, in
a browser, in safe Rust.**

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
```

The repo includes `gen_samples.py` / `gen_progs.py` — independent Python assemblers that emit synthetic
assets and a `progs.dat`, so the loaders, VM, and renderer can be exercised without owning a copy of Quake.

## What is *not* here yet — the engine roadmap

The remaining ~90k lines fall into clear layers. A realistic port order, each layer building on the last:

**Phase 1 — platform & core services** (mostly done here)
- `read`/`error` ✅, file formats ✅, math ✅
- *next:* `zone.c` (hunk/zone/cache allocator → a Rust arena or just `Vec`/`Box`), `cmd.c` + `cvar.c`
  (console command + variable system → a registry), `common.c` filesystem search-path / `COM_*`.

**Phase 2 — the data model in memory**
- `model.c` `Mod_LoadBrushModel` → turn the `bsp` structs into a renderable `model_t` (surfaces, the
  node/leaf tree, PVS decompression `Mod_DecompressVis`), plus alias/sprite model setup.

**Phase 3 — server & game logic**
- `pr_*.c` — the **QuakeC virtual machine** ✅ *done* (`progs` + `vm` + `builtins`): `progs.dat` loader,
  bytecode interpreter (all 66 opcodes), edict/string/global runtime, and the builtin table. Pure builtins
  (`ftos`, `vtos`, `normalize`, `vlen`, `vectoyaw`/`vectoangles`, `rint`/`floor`/`ceil`/`fabs`, `random`,
  `spawn`/`remove`, `find`/`nextent`, `print`/`dprint`/`bprint`, `makevectors`, …) are implemented; engine
  builtins (world, sound, network, cvars) are stubbed to fault cleanly.
  - *next:* `sv_main.c`, `sv_phys.c`, `world.c` — server entity management and the BSP collision/physics
    hull trace, which is what the stubbed world builtins (`setorigin`, `traceline`, `setmodel`) need.

**Phase 4 — client & presentation**
- **Renderer.** A from-scratch software rasteriser (`render`) ✅ *working* — driven by the parsed BSP/palette
  data, with a z-buffer, backface culling, and flat per-face shading; renders the world to a PPM. This is a
  reimplementation, **not** a port of Quake's asm-heavy `d_*.c` span/surface-cache renderer (`r_*.c`/`d_*.c`)
  or `gl_*.c`. A production path would more likely target `wgpu`, fed by the same parsed data.
- `cl_*.c` — client state, prediction, entity interpolation.
- `snd_*.c` — sound mixing; `in_*.c`/`vid_*.c` — input and video (would map to `winit` + `cpal`).

**Phase 5 — the host loop**
- `host.c`, `host_cmd.c`, `sys_*.c` — frame timing, save/load, the glue that owns everything.

The dependency arrow runs **formats → model → {server, client} → renderer/sound → host**. This crate
delivers the root of that graph with tests, so each later phase has a verified foundation to read through.

## Licensing

The original Quake source is © 1996–1997 id Software, released under the **GNU General Public License v2**.
This port is a derivative work and is therefore distributed under **GPL-2.0-or-later**. Game *data*
(`.pak` files, maps, models) is not included and is not covered by the GPL.
