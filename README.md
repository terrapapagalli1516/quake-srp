# quake-rust

A faithful, **dependency-free, `#![forbid(unsafe_code)]` Rust reimplementation** of id Software's *Quake*
(1996), ported subsystem by subsystem from the original GPLv2 C source — and validated against the real
shareware data at every step. It loads Quake's files, runs its QuakeC virtual machine, collides against its
BSP worlds, spawns maps by executing the real game logic, moves a player through them with collision and
physics, plays back recorded demos, renders the world with **baked lightmaps + textures + models**, and runs
**interactively in a web browser** via WebAssembly.

> **Honest framing.** This is the verifiable *engine core*, not a finished shippable game. Combat, animated
> models, moving doors, and a sound mixer are not done yet (see the roadmap). What *is* done is real and tested:
> 185 tests, zero dependencies, no `unsafe`, every layer checked against id's shareware `pak0.pak`.

## Layout

| Path | What |
|------|------|
| `quake-rs/` | the engine crate (lib + `quaketool` CLI). All the subsystems live in `quake-rs/src/`. |
| `quake-wasm/` | a ~120-line `cdylib` shell that compiles the engine to `wasm32` and exposes it to a `<canvas>`. |
| `web/` | the browser page (`index.html`) + a headless-verify script (`shoot.py`). |
| `gen_samples.py`, `gen_progs.py` | independent Python asset/bytecode generators, so tests need no real data. |
| `screenshots/` | rendered output from real e1m1 / start (the lit shots, the walkthrough GIF). |

## What works (validated on the real shareware)

- **Asset formats** — PAK (+ CRC-16/CCITT, exact-match against stock `pak0.pak`), WAD2, BSP v29, MDL, SPR, palette.
- **QuakeC VM** — the full bytecode interpreter (all 66 opcodes), edict/string/global runtime, builtins.
- **Server** — `ED_LoadFromFile` spawns a map by running id's real spawn functions; `SV_Physics` tick; entity-vs-entity
  collision (`SV_Move`), touch/impact, item pickups; a real **player client** (`PutClientInServer` + `SV_ClientThink`
  movement); monster-movement builtins (`walkmove`/`movetogoal`/chase-dir/`checkclient`/`findradius`).
- **Renderer** — a from-scratch software rasteriser (z-buffer, backface cull, perspective-correct textures,
  **baked BSP lightmaps**, alias models in-scene). Not a port of Quake's asm `d_*.c` pipeline.
- **Demo playback** — parses the `.dem` net-protocol stream into per-frame entity snapshots and renders id's
  recorded attract demo.
- **Browser** — the engine compiles to `wasm32-unknown-unknown` unchanged; WASD + mouse-look + fullscreen, lit.

See `quake-rs/README.md` for the full subsystem table, the C-source provenance of each module, and the verified
`quaketool` command transcripts.

## Build & run

```sh
cd quake-rs
cargo test          # 185 tests, no game data required (uses synthetic fixtures)
cargo run --release --bin quaketool -- --help
```

`quaketool` subcommands: `info ls cat bsp map mdl spr wad dis run render render-demo scene sim playtest demo walk`.

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

## Roadmap (remaining work)

- **Combat** (#4) — weapon fire `traceline`→`T_Damage`, projectiles, so the shotgun hits a grunt. Mostly emergent
  from the QuakeC once the plumbing connects.
- **Pusher physics** (#7) — `SV_Physics_Pusher` so doors/platforms move and carry/block entities.
- **MDL animation** (#8) — frame interpolation so monsters move instead of standing in frame 0.
- **Sound mixer** (#9) — channel mixing + attenuation (the decode-to-Web-Audio path is proven; the mixer isn't built).
- **PVS culling** — use the visibility lump to stop drawing the whole map (perf + correctness).

## Licensing

Derivative of Quake's GPLv2 source (© 1996–1997 id Software) → distributed under **GPL-2.0-or-later**. No game
data is included.
