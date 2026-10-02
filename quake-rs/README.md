# quake-rs

The engine of [quake-rust](../README.md), as a Rust library. It holds the game's systems:
- id's file formats;
- QuakeC;
- the server;
- the client;
- the software renderer;
- the status bar, menus and console;
- the settings;
- id's sound mixer.

It is ported from id's GPLv2 WinQuake C source
([`id-Software/Quake`](https://github.com/id-Software/Quake)), one module per id file where
that fits. It uses only the standard library, has `#![forbid(unsafe_code)]`, and ships a
command-line tool, `quaketool`. A host supplies the rest; see [Not here](#not-here).

## Using it

A host (a platform) drives the game one frame at a time. It hands in the frame's length,
the player's input and the screen's size. It gets back three things:
- an 8-bit image;
- the frame's colour shifts (damage, pickups, underwater, powerups). The host combines them
  with id's base palette and gamma into the palette it shows the image through;
- the frame's sound calls, which it feeds to id's mixer.

The browser build and `quaketool play` are two such hosts.
[`examples/one_second.rs`](examples/one_second.rs) is a third, as a short program. It
starts a game on E1M1, walks forward firing for one second, and saves what the player saw
and heard:

```sh
cargo run --release --example one_second -- ../quake-data/ID1/PAK0.PAK target/e1m1.ppm target/e1m1.wav
```

The WAV runs a little over a second, because the mixer paints slightly ahead of the clock
as id's does. The core of the example, with the setup and the file writing left out:

```rust
// A game on e1m1: server, client and renderer, owned by one value.
let mut walk = host_cmd::build_walk_map(pak.clone(), "maps/e1m1.bsp", &Rc::new(QRand::new()), &mut sound)
    .ok_or("maps/e1m1.bsp would not load")?;
let mut mixer = Mixer::new(&pak, RATE, Fixes::NONE);
mixer.run(&pak, &sound); // the level's own sounds, from loading it

for n in 1..=72 {
    walk.in_fwd = 1.0; // "forward" and "fire" held, as keys or a pad would
    walk.in_attack = true;
    let frame = cl_main::walk_frame(&mut walk, FRAME, false, &vid);

    // Sound: the frame's calls into id's mixer, then paint up to "now".
    mixer.run(&pak, &frame.sound);
    let now = (f64::from(n) * FRAME * f64::from(RATE)) as i64;
    let pairs = mixer.samples_ahead(now, BUFFER_PAIRS);
    let at = pcm.len();
    pcm.resize(at + 2 * pairs, 0);
    mixer.paint(&mut pcm[at..]);

    last = Some(frame);
}
let frame = last.ok_or("no frame")?;

// Picture: the 8-bit frame through its palette (id's VID_SetPalette: the
// base palette, this frame's colour shifts, then gamma).
let palette = FramePalette::new(&base_palette, &frame.cshifts, &gamma);
render::pack_rgba(&frame.image, &palette, &mut rgba, 1);
```

`walk_frame` runs one of id's host frames, in id's order:
1. the client builds the move from the held keys, the mouse and the pad (`CL_BaseMove`);
2. the server runs its frame (`SV_Physics`, the QuakeC thinks and touches) and writes
   its messages;
3. the client reads them, relinks the entities (`CL_RelinkEntities`) and places the
   camera (`V_CalcRefdef`);
4. the renderer draws the view, and the 2-D layer draws the status bar and centre prints
   over it.

The host then draws the menu or the console on top, when either is open (`menu`,
`console`). A demo takes the server's place with `cl_demo::demo_frame`.

## How it is organised

The crate is roughly layered, from id's files up to the client, which drives the rest:

```
client       client, demo, particles, tent, dlight     the live frame, demos, effects, the host clock
settings     cvar, cmd, settings, stepping             variables, commands, the Classic and 2026 profiles
sound        snd, cd_audio                             id's mixer and the CD player's state
2-D          draw, screen, sbar, menu, console, keys   the status bar, menus, console, key bindings
renderer     render                                    the 3-D view, on any number of threads
server       server, world, save                       physics, collision, monsters, savegames
QuakeC       progs, vm, builtins                       the program, the interpreter, the builtins
formats      pak, common, wad, bsp, mdl, spr           the search path and id's file formats
```

State belongs to values: `Server`, `Walk` (a live game), `DemoPlay`, `Renderer`, `Mixer`
and `Settings`. Two games or two tests never share it, and because the renderer's state is
in `Renderer`, it can draw on several threads. What is still thread-local is small: a pool
of spare frame buffers, the 2-D layer's scale flag, a hull cache and a trace counter in
`world`, and two test hooks (a benchmark timer and the 2-D oracle's harness).
[CODE_PLAN.md](../CODE_PLAN.md) tracks them.

### Modules, with the id source each one ports

**Formats**

| module | id's C | what |
|---|---|---|
| `read`, `error`, `crc` | `LittleLong`, `Sys_Error`, `crc.c` | bounds-checked little-endian reads, the error type, CRC-16 |
| `pak`, `common` | `common.c` | pak archives, the search path (`COM_FindFile`), `COM_CheckRegistered` |
| `wad` | `wad.c` | `gfx.wad`: small pictures (mostly the status bar's) and the console font |
| `bsp`, `mdl`, `spr` | `bspfile.h`, `model.c` | maps (with vis decompression), alias models, sprites |
| `math` | `mathlib.c` | vectors, angles, `BoxOnPlaneSide` |

**QuakeC**

| module | id's C | what |
|---|---|---|
| `progs` | `pr_comp.h`, `pr_edict.c` | `progs.dat`, opcodes decoded once at load, a disassembler |
| `vm`, `vm::print` | `pr_exec.c`, `pr_edict.c` | the interpreter, edicts and strings; `PR_RunError`'s stack trace |
| `builtins` | `pr_cmds.c` | the builtins that need no map |
| `qrand` | libc `rand` | the session's random streams |

**Server**

| module | id's C | what |
|---|---|---|
| `server` | `server.h` | `Server`, and typed entity fields (`MoveType`, `Solid`, `EntFlags`) |
| `server::sv_main` | `sv_main.c` | spawning a map, connecting the player, which entities the client is sent |
| `server::sv_phys`, `sv_user` | `sv_phys.c`, `sv_user.c` | thinks, every move type, pushers; the player's move, friction, swimming |
| `server::sv_world`, `world` | `world.c` | hull traces, `SV_Move` against the world and every solid, touching |
| `server::sv_move` | `sv_move.c` | monster movement |
| `server::pr_cmds`, `pr_edict` | `pr_cmds.c`, `pr_edict.c` | the builtins that touch the world, loading entities from a map |
| `server::msg`, `lightstyle`, `host` | `sv_main.c`, `pr_cmds.c`, `host_cmd.c` | the messages to the client, light styles, level changes and settings |
| `save` | `host_cmd.c` | savegames, in id's text format |

**Client**

| module | id's C | what |
|---|---|---|
| `client` | `client.h` | `Walk`, `DemoPlay`, `Vid`, and a frame's output, `ClientFrame` |
| `client::cl_main`, `cl_demo` | `cl_main.c`, `cl_demo.c`, `cl_parse.c` | the live frame; demo playback with id's interpolation; `timedemo` |
| `client::cl_input`, `in_win` | `cl_input.c`, `in_win.c` | the move from keys and mouse; the joystick |
| `client::view` | `view.c` | the damage kick and the colour shifts (damage, bonus flash) and their fades |
| `client::host`, `host_cmd` | `host.c`, `host_cmd.c` | `Host_FilterTime`, `Host_Error`; `map`, `load`, the cheats |
| `client::lerpmove` | QuakeSpasm's `r_lerpmove` | monsters glide between steps (2026) |
| `client::lerpmodels` | QuakeSpasm's `r_lerpmodels` | an alias model's animation blends between frames (2026) |
| `client::cl_tent`, `tent`, `particles`, `dlight` | `cl_tent.c`, `r_part.c`, `cl_main.c` | beams, temp entities, trails, particles, dynamic lights |
| `demo` | `cl_demo.c`, `cl_parse.c` | `.dem` framing and the message decoder |
| `stepping` | the port's | what the uncapped frame steps so it plays like 72 Hz (see [FRAMERATE.md](../FRAMERATE.md)) |

**Renderer**

| module | id's C | what |
|---|---|---|
| `render` | `r_main.c` | `Renderer` (all its state), `Scene` (id's `refdef_t`), `Image` of palette indices |
| `render::edge` | `r_bsp.c`, `r_edge.c`, `r_draw.c`, `d_edge.c` | the BSP walk, edge clipping, `R_ScanEdges`, the z-buffer |
| `render::band` | the port's | the view in row bands on several threads, each through id's passes in id's order |
| `render::raster`, `surf`, `light` | `d_scan.c`, `d_draw16.s`, `r_surf.c`, `d_surf.c`, `r_light.c` | 16-pixel perspective spans, mip levels, the surface cache, lightmaps and dynamic lights |
| `render::sky`, `warp` | `r_sky.c`, `d_sky.c`, `d_scan.c` | the two-layer sky, liquids, the underwater wobble |
| `render::alias`, `polyse`, `sprite`, `part` | `r_alias.c`, `d_polyse.c`, `r_sprite.c`, `d_part.c` | models and the gun, the affine triangle filler, sprites, particles |
| `render::view` | `view.c` | view bob, the gun's placement, the frame's palette (`V_UpdatePalette` into `FramePalette`), packing to RGBA |
| `render::video` | the port's | past id's 1280x1024 limit, and wider views on wide screens (2026) |
| `render::world`, `vis`, `stats` | `r_main.c`, `model.c` | brush entities, `Mod_PointInLeaf`, timers |

**2-D, sound, settings**

| module | id's C | what |
|---|---|---|
| `draw`, `screen`, `sbar` | `draw.c`, `screen.c`, `sbar.c` | pics and text, the view rectangle and the composed screen, the status bar |
| `menu`, `console`, `keys` | `menu.c`, `console.c`, `keys.c` | every menu (and the Classic / 2026 page), the console, key bindings |
| `snd::dma`, `snd::mix`, `snd::mem` | `snd_dma.c`, `snd_mix.c`, `snd_mem.c` | id's mixer: channels, spatialization, painting, resampling |
| `cd_audio` | `cd_win.c` | which CD track plays; the host plays it |
| `cvar`, `cmd`, `settings` | `cvar.c`, `cmd.c` | typed console variables, the command table, profiles and `config.cfg` |

`src/bin/quaketool/` is the CLI. The test fixtures (`render::fixtures`, `server::testutil`)
exist only in test builds.

## Design choices

- **Only `std`.** It builds offline, and there is no dependency to update.
- **No `unsafe`.** id's C casts file buffers onto structs. Here every field goes through a
  bounds-checked reader, so a damaged file is an error, not undefined behaviour. The same
  rule holds in the browser build, which is why it is a WASI program with no exports of its
  own.
- **id's shape where it matters.** The edge-sorted renderer, the span loops and the
  QuakeC dispatch keep the structure of id's code on purpose: that is what makes the pixels
  and the game state come out identical. Doc comments name the C function each piece
  ports, and explain any departure.
- **Every departure is a setting.** Anything that differs from id's game is a typed field
  in `cvar::Cvars`, or a key binding in `keys::Bindings`, and the Classic profile turns it
  off. [AUDIT.md](../AUDIT.md) lists them all.
- **The same result on any machine.** The renderer draws the same pixels on 1 thread or
  16. In 2026, the game steps anything that would otherwise drift with the frame rate, so
  high frame rates play like id's 72.

## Testing

```sh
cargo test --release                   # no game data needed
cargo run --release --bin quaketool -- --help
```

The tests build their own maps, models, paks and QuakeC programs in Rust (for example
`tests/integration.rs` and `render::fixtures`), so they run without a copy of Quake.
`../gen_samples.py` and `../gen_progs.py` write similar files to disk, for trying
`quaketool` by hand. Two other places test against the real shareware pak:
- `../quake-wasm`'s end-to-end tests;
- `../oracle/classic_check.py`, which compares Classic with id's own C (see the
  [top-level README](../README.md#proof)).

## quaketool

`quaketool --help` lists every command with its arguments. One table,
`src/bin/quaketool/main.rs`'s `COMMANDS`, drives both the dispatch and the help.

- **id's files:** `info`, `ls`, `cat`, `bsp`, `map`, `mdl`, `spr`, `wad`, `dis`, `run`.
- **Rendering:**
  - `scene`: the reference renders;
  - `view`: one exact view, for id's renderer to match;
  - `shot`: the game screen at any size and video setting;
  - `render`, `render-demo`, `menu`.
- **The game, headless:** `sim`, `simbench`, `playtest`, `walk`, `demo`, `changelevel`,
  `census`, `census-edicts`.
- **The game as the browser runs it:**
  - `play`: frame hashes;
  - `timedemo`: id's benchmark;
  - `sound`, `sndscript`: the mixer to WAV;
  - `framerate --check`: gameplay at high frame rates, compared with 72 Hz.

## Not here

- **Multiplayer and netcode.** The server and client talk in the same process.
- **The host's part.** A host supplies:
  - the frame loop, which calls `client::host`'s 72 fps gate;
  - routing keys to the game, the menu or the console (`Key_Event`);
  - running console commands and acting on menu choices;
  - reading and writing saves and `config.cfg`;
  - showing the frame and playing the PCM.

  The browser's host is `../quake-wasm` (see [web/PLATFORM.md](../web/PLATFORM.md)).
  Moving more of this into an engine-owned `Host` is on [CODE_PLAN.md](../CODE_PLAN.md)'s
  list.
- **`zone.c`.** Ownership replaces id's allocator.

## License

The Quake source is © 1996–1997 id Software, under the GNU GPL v2. This port is a
derivative work, so GPL-2.0-or-later. Game data is not included.
