# quake-rust

A faithful, **dependency-free, `#![forbid(unsafe_code)]` Rust reimplementation** of id Software's *Quake*
(1996), ported subsystem by subsystem from the original GPLv2 C source — and validated against the real
shareware data at every step. It loads Quake's files, runs its QuakeC virtual machine, collides against its
BSP worlds, spawns maps by executing the real game logic, moves a player through them with collision and
physics, plays back recorded demos, renders the world with **baked lightmaps + textures + models**, and runs
**interactively in a web browser** via WebAssembly.

> **Honest framing.** This is a genuinely playable single-player port. It boots into the **Quake main menu drawn
> over the attract demo**, New Game drops you in the **`start` skill/episode hub**, and you can walk, fight monsters
> that wake/chase/attack, take damage (with the red flash), **die and respawn** (the full QuakeC death chain,
> proven end-to-end), switch weapons — **including the thunderbolt's rendered lightning** — pick up items +
> ammo/health/explosive boxes, open doors, ride elevators, see blood/explosions/dynamic lights (gated by the real
> `R_MarkLights` BSP recursion, so light can't bleed through walls)/flickering torches, hear **spatialized in-game
> sound + the placed ambient loops and leaf ambients** (torch crackle, machine hums, wind and water), and reach the
> exit to the **intermission stats screen** (Time / Secrets / Kills from the QC-placed camera) — through to the
> **episode-end finale text** — with your inventory carried to the next map: the whole shareware episode. There's a
> working **Options menu** (screen size, mouse, volume; resolution under Video Options) and a **drop-down console** (`~`) with
> `god`/`noclip`/`fly`/`give`/`impulse`/`map`/`kill`, and it saves and loads (Single Player > Save/Load, the
> `save`/`load` commands; the page keeps the `.sav` text in localStorage). What it is *not*: multiplayer/netcode
> (out of scope). Everything claimed below is real and tested: **~580 engine + ~120 wasm tests**, zero dependencies, no
> `unsafe` in the engine, every layer checked against id's shareware `pak0.pak`, and renderer changes verified
> against golden scene renders (byte-identical unless a fidelity fix deliberately re-baselines — each such
> re-baseline is recorded in `AUDIT.md`).

## Layout

| Path | What |
|------|------|
| `quake-rs/` | the engine crate (lib + `quaketool` CLI). All the subsystems live in `quake-rs/src/`, the game client too: `client/` is the live frame against the local server and demo playback (id's `cl_*.c`, `view.c`, the client half of `host.c`/`host_cmd.c`), which the browser runs and `quaketool play` runs natively. |
| `quake-wasm/` | the `cdylib` browser shell — the platform layer (~3.0k lines + ~5.4k of e2e tests): compiles the engine to `wasm32`, holds the host state (`App`: mode, menu, console, clocks, framebuffer) around `quake_rs::client`, carries out the sound calls each client frame returns for the page's Web Audio, bridges saves to localStorage, and exposes plain `extern "C"` exports to a `<canvas>` — no `wasm-bindgen`, no deps. Modules are named after the id file they port the platform/host side of (`host` = `Host_Frame`, `vid`, `snd_dma`, `input`, `menu`, `console`, `host_cmd`, `savegame`, plus `app` for the state and boots; `cl_walk`/`cl_demo` run the client's frames for the page); `src/lib.rs` maps every export to its module. |
| `web/` | the browser page (`index.html`) + eight headless-Chromium verify scripts (`verify_*.py`: walk, ambient, demo, input, menu, save, loops, extras). |
| `oracle/` | id's own WinQuake software renderer built headless from the C (null drivers, docker i386 build) + `compare.py`: renders the same view in both and diffs them pixel for pixel. See `oracle/README.md` for how to run it and the ranked fidelity findings. |
| `gen_samples.py`, `gen_progs.py` | independent Python asset/bytecode generators, so tests need no real data. |
| `screenshots/` | rendered output from real e1m1 / start (the lit shots, the walkthrough GIF). |

## What works (validated on the real shareware)

- **Asset formats** — PAK (+ CRC-16/CCITT, exact-match against stock `pak0.pak`), WAD2, BSP v29, MDL, SPR, palette.
- **QuakeC VM** — the full bytecode interpreter (all 66 opcodes), edict/string/global runtime, builtins.
- **Server** — `ED_LoadFromFile` spawns a map by running id's real spawn functions; `SV_Physics` tick (walk, toss,
  bounce, fly, **`SV_Physics_Pusher`** for doors/platforms); entity-vs-entity collision (`SV_Move`), touch/impact,
  **item pickups**; a real **player client** (`PutClientInServer` + `SV_ClientThink` movement, **impulse weapon
  switching**, the C's **signon settle frames** before frame 0); **monster AI** (sight/`FindTarget`/`checkclient`,
  chase, attack) and the movement builtins (`walkmove`/`movetogoal`/chase-dir/`findradius`); **combat**
  (`traceline`→QuakeC `T_Damage`, player damage + **death → corpse physics → respawn**, proven through the real
  QuakeC chain); **`changelevel`** with inventory carried across maps (`SetChangeParms`/`DecodeLevelParms`) and the
  **intermission/finale flow** (the MSG_ALL `svc_intermission`/`svc_finale` stream from QC's `execute_changelevel`).
- **Renderer** — id's **edge-sorted span renderer** for the world and brush models (`r_edge.c`: the BSP walked
  front to back into one edge list, each pixel drawn once, a 16-bit 1/z buffer for the entities; PVS and
  frustum culling, 16-pixel perspective spans, mip levels): **the full Quake lighting model** — baked BSP lightmaps + **dynamic
  lights** (`R_AddDynamicLights`, gated by the **`R_MarkLights` BSP recursion** so light never crosses solid
  planes) + **animated light styles** (`R_AnimateLight`: flickering torches); **alias-model frame animation** +
  skins; brush submodels + **external `b_*.bsp` brush-model items** (explosive boxes, ammo/health boxes); turbulent
  **water/lava/slime warp** + scrolling sky; first-person **weapon viewmodel**; **particles** (blood) +
  **temp-entity effects** (fiery explosions, impacts, **lightning bolts** — `cl_tent.c`'s beam store expanding into
  bolt models for the shambler/thunderbolt/Chthon trap); **head-bob** (`V_CalcBob`); **screen blends**
  (`V_UpdatePalette`'s palette shifts: damage flash, underwater tint); a **status-bar HUD** and the **intermission/finale overlays**
  (`Sbar_IntermissionOverlay`, the 8-chars/sec finale text reveal).
- **UI** — the **main menu** (`M_Menu_*`: plaque/title/list + animated cursor, rendered from the pak's `.lmp` pics)
  with **Single Player → `start` hub**, a working **Options** screen (screen size, mouse +
  volume; the render resolution under Video Options), and a `~` **drop-down console** (conback + conchars scrollback + input line) running `god`/`noclip`/
  `fly`/`give`/`impulse`/`map`/`kill`/`clear`. Boots into the menu **over the playing attract demo**.
- **Web extras** — the port is id's Quake by default (Always Run aside). Its departures are opt-in, all
  off by default, on one page: **Options > Web extras**, drawn like id's Options page, each also a
  `wasm_*` console variable (listed in `quake-wasm/src/extras.rs`): an **uncapped frame rate** (no
  72 fps cap, for 120/144 Hz displays; `wasm_uncapped 1`), an **FPS readout** in QuakeWorld's style
  (`wasm_showfps 1`) and **exact perspective** on every pixel instead of id's 16-pixel spans
  (`wasm_exactpersp 1`). The page remembers them across reloads, as it does the resolution and
  Screen size. Recorded in `AUDIT.md` ("Web extras").
- **Sound** — the QuakeC `sound` + temp-entity sounds drive a queue the browser plays through Web Audio with
  **distance/stereo spatialization** relative to the player (samples resolved under the `sound/` pak dir); **placed
  `ambientsound()` loops** (torch crackle, machine hums — `svc_spawnstaticsound` semantics, wire-byte-exact
  volume/attenuation) and the **automatic leaf ambients** (water/wind, ramped per `S_UpdateAmbientSounds` with the
  C's integer math at its 72 fps frame cap).
- **Demo playback** — parses the `.dem` net-protocol stream into per-frame entity snapshots **+ svc_particle /
  svc_temp_entity effects + static sounds + intermission state**, and (demo-parity pass, 2026-06-11) the full
  client-visible stream the C replays: **recorded svc_sound one-shots** (spatialized like live), **recorded
  lightstyles**, **svc_clientdata stats driving the live status bar + weapon viewmodel**, **svc_damage flash +
  view kick**, **svc_print/centerprint overlays**, and **V_CalcRefdef head-bob/lean** — frame emission gated on
  signon completion so the attract loop starts (and wraps) in-world. The demo IS the game rendering a recorded
  stream, as in WinQuake.
- **Browser** — the engine compiles to `wasm32-unknown-unknown` unchanged; WASD + mouse-look + fullscreen, fire
  (click), weapon select (1–8), `~` console, Esc menu, selectable resolution, lit, with HUD + sound.

See `quake-rs/README.md` for the full subsystem table, the C-source provenance of each module, and the verified
`quaketool` command transcripts.

## Build & run

```sh
cd quake-rs
cargo test          # 575 lib + 1 bin + 8 integration tests, no game data required (synthetic fixtures)
cargo run --release --bin quaketool -- --help
```

`quaketool` subcommands: `info ls cat bsp map mdl spr wad dis run render render-demo menu sim scene view walk demo playtest simbench changelevel census census-edicts play`. `play` runs the browser's game client natively: `quaketool play pak0.pak demo1,walk_e1m1,walk_e1m3,fire_e1m1,quad_e1m1 --res 320x200,640x400` prints the same frame hashes as `uv run --with playwright web/bench.py --hash-every 30` with those workloads and resolutions, byte for byte.

### Getting the game data (not committed)

The repo is **data-free** — Quake assets are copyrighted and excluded by `.gitignore`. To run against real maps,
fetch id's freely-redistributable **shareware** `pak0.pak` (md5 `5906e599...`):

```sh
# quake106.zip -> resource.1 (LZH) -> id1/PAK0.PAK
curl -sL -o quake106.zip https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/main/bin/quake106.zip
unzip quake106.zip resource.1 && bsdtar -xf resource.1 ID1/PAK0.PAK
cargo run --release --bin quaketool -- render ID1/PAK0.PAK ... # etc.
```

The browser build (`quake-wasm`) `include_bytes!`s a pak at build time, so it needs the data present to compile;
the engine lib and all tests do **not**.

### In the browser

```sh
cd quake-wasm && cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quake_wasm.wasm ../web/
miniserve --port 8080 -C ../web      # any static server works; open /index.html
```

The wasm is 18.7 MB, nearly all of it the embedded pak (the code is ~0.8 MB). miniserve's `-C`
(`--compress-response`) compresses it on the fly: Chrome gets brotli at **8.6 MB** (gzip 9.6 MB,
zstd 8.3 MB). That costs the server ~0.4 s of CPU per download (nothing is cached), so it pays on
links slower than ~400 Mbit/s: at an emulated 50 Mbit/s, first frame 3.5 → 1.8 s; on localhost it
is slower (0.19 → 0.54 s). The page stream-compiles the module (`WebAssembly.instantiateStreaming`)
whatever MIME type the server sends, and falls back to a buffered load on browsers without it;
behind compression the loading bar shows a MB counter instead of a percentage. Splitting the pak
out of the wasm, so an engine update doesn't re-download 18.7 MB of unchanged data, is an open
option (PERF_PLAN D4).

## Performance

The software renderer is per-pixel bound, so frame time scales with resolution. A
built-in benchmark renders a map repeatedly and reports the warm per-frame cost plus a
per-phase breakdown (world / submodel / alias / particle / …) and counters (faces drawn,
overdraw pixels, surface-cache hit rate):

```sh
QUAKE_BENCH=30 QUAKE_RES=1920x1080 \
  cargo run --release --bin quaketool -- scene pak0.pak maps/e1m1.bsp out.ppm
```

**Relative cost is the meaningful part — absolute ms swings several-fold with host
load** (an idle machine measured e1m1 @1080p ~33 ms; under load the same binary measured
~91 ms). Always A/B two builds in one sitting. The shape: the **world (BSP wall) pass
dominates** and scales ~linearly with pixel count; with id's edge-sorted spans it draws
**each pixel once**, with no z test and nothing cleared (PERF_PLAN A3).

Key optimisations (in `render/` / `vm.rs` / `server.rs`): a **lit surface cache**
(Quake's `d_surf.c` — bake texture × lightmap × colormap per surface once, then one
byte/pixel), id's **edge-sorted span renderer** (`r_edge.c`: no overdraw, no z test),
16-pixel perspective spans with integer steps in between, an **O(1) field-offset
cache** in the VM, and an **abs-box broadphase** in `sv_move` (the ~25× sim speedup on
dense maps). See `AUDIT.md` for the per-change ledger and `STATUS.md` for current WIP +
the honest perf scorecard.

## Roadmap

The single-player shareware experience is **feature-complete**: the core loop (fight, die, respawn, exit), the UI
(menu / options / help / console), intermission + finale screens, in-game + demo sound with ambient loops,
particles + lightning, selectable + persistent resolution, the `start` hub, the attract loop, and the external
brush-model items are all done — every subsystem audited against id's C across seven review rounds (66-finding
ledger in `AUDIT.md`, all HIGHs closed). What remains:

- **A documented divergence tail** — one narrow MEDIUM (maps with no lighting lump render Lambert where id
  is fullbright; test maps only) and assorted cosmetic LOWs (the demo path's missing explosion dlight).
  Tracked with plans in `AUDIT.md`; the gameplay census's findings in `CENSUS.md`.
- **Multiplayer** — out of scope for this single-player, headless-server port.

## Licensing

Derivative of Quake's GPLv2 source (© 1996–1997 id Software) → distributed under **GPL-2.0-or-later**. No game
data is included.
