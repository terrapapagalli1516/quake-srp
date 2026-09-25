# quake-rust

A Rust port of id Software's *Quake* (1996), ported file by file from id's GPLv2 WinQuake C
source. No dependencies beyond the standard library, `#![forbid(unsafe_code)]`, and checked
against the shareware data and against id's own renderer. It runs natively (the `quaketool`
CLI) and in a web browser (WebAssembly, `<canvas>`, Web Audio).

It plays the shareware episode in single player: the attract demos behind the main menu,
New Game into the `start` hub, the eight E1 maps with their monsters, doors, lifts,
secrets and Chthon, death and respawn, intermission and finale screens, save and load, the
Options menu, and the drop-down console. Multiplayer and netcode are out of scope.

## The rule

**Faithful to id's WinQuake by default.** The only intended default departure is Always
Run. Anything else that differs from id's game is an opt-in **Web extra**, off by default
(Options > Web extras, below).

Two things follow that a player notices:

- **Frame rate.** id's `Host_FilterTime` caps the game at 72 frames a second, so the port
  does too: a 144 Hz display runs at 72 fps and a 120 Hz display at 60 (every other
  refresh). "Uncapped framerate" is a Web extra.
- **Screen size and resolution.** Options > Screen size is id's `viewsize` (the 3-D view
  shrinks inside a tiled border; 110 drops the inventory, 120 the status bar). The
  render resolution is under Options > Video Options (960x600 by default). As in
  WinQuake, the status bar and menus are drawn 1:1 at that resolution, so they get
  smaller as it grows ("Scaled 2-D layer" is the Web extra that blows them up).

**Not yet within the rule:** four control departures are still on by default and wait for
a decision (CENSUS.md, "Rule departures on by default"): mouse look is held permanently
while the pointer is locked (id: `+mlook` off), WASD moves (id binds `a`/`d` to look up
and move up), `f` toggles fullscreen, and Space in water also adds upward speed. And one
kept on purpose (AUDIT.md, "Demo commands, timedemo, pause"): the attract demos keep
cycling behind the main menu, where id's menu stops the loop after the current demo.

## Run it

The repo has no game data. Fetch id's freely redistributable shareware `pak0.pak` into
`quake-data/ID1/PAK0.PAK` (the browser build embeds it at compile time):

```sh
curl -sL -o quake106.zip https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/main/bin/quake106.zip
unzip quake106.zip resource.1 && bsdtar -xf resource.1 ID1/PAK0.PAK   # quake106.zip -> resource.1 (LZH) -> ID1/PAK0.PAK
mkdir -p quake-data && mv ID1 quake-data/
```

**In the browser:**

```sh
cd quake-wasm && cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/quake_wasm.wasm ../web/
cd .. && miniserve -C -p 8196 web      # -C compresses responses: 9.1 MB with brotli instead of 19.7
```

Then open `http://<host>:8196/index.html` (on the dev server: `http://localhost:8196/index.html`).
`index.html?lowlatency` asks for a low-latency canvas (can tear; off by default). Any static
server works. The wasm is 19.7 MB, of which 18.7 MB is the embedded pak.

The page shows the game in the largest 4:3 box the window fits (never under 640x480), the
way a 1996 monitor showed WinQuake's 16:10 modes. The window sets how big the picture is;
the resolution only sets how fine its pixels are. Controls: mouse to look and click to
fire, WASD, 1–8 for weapons, Tab for the scores, Esc for the menu, `~` for the console,
`f` for fullscreen. The page keeps the resolution, Screen size, the Web extras and the
saves (`.sav` text) in localStorage.

**Natively** (the same game client the browser runs, no window):

```sh
cd quake-rs && cargo build --release
# id's demo1, 660 frames at 640x400; every 60th frame hashed and written as /tmp/f-demo1-640x400-NNNN.ppm
./target/release/quaketool play ../quake-data/ID1/PAK0.PAK demo1 --res 640x400 --hash-every 60 --ppm /tmp/f
./target/release/quaketool --help          # every subcommand
```

## What works

Checked on the real shareware data.

- **Files:** PAK (with id's CRC), WAD2, BSP v29, MDL, SPR, the palette and colormap.
- **QuakeC:** the bytecode VM (all 66 opcodes) and the builtins the id1 progs call.
- **Server:** `SV_SpawnServer`, the physics tick (walk, step, toss, bounce, fly, push),
  collision against the world and every entity, triggers, monster AI and movement, combat,
  death and respawn, `changelevel` with the inventory carried, intermission and finale,
  e1m8's low gravity, and savegames in id's `.sav` text format.
- **Renderer** (id's software renderer): the world and brush models through id's
  edge-sorted span renderer (each pixel drawn once, 16-pixel perspective spans, mip
  levels, the surface cache), lightmaps with animated light styles and dynamic lights,
  alias models and the gun through `D_PolysetDraw`, sprites, particles, beams, liquids,
  the two-layer sky, the underwater warp, the palette shifts (damage, pickups, powerups,
  water), and the pixel aspect of a 16:10 mode on a 4:3 screen.
- **2-D layer:** status bar and inventory, scoreboards, intermission and finale overlays,
  centre prints, notify lines, the sliding console, every menu (Main, Single Player,
  Load/Save, Options, Customize controls, Video, Help, Quit), drawn 1:1 as WinQuake draws
  them.
- **Client:** the live frame against the local server, the view (bob, roll, kick,
  stair smoothing, the gun's placement), temp entities, trails, dynamic lights, and demo
  playback of id's demo1–demo3 (the attract loop cycles them, with sound, status bar and
  gun).
- **Sound:** what id's `snd_dma.c` decides — channel choice and override, spatialisation,
  placed ambient loops, the leaf ambients (water, wind), looping mover sounds. The mixing
  is the platform's: Web Audio in the browser.
- **Console:** `god`, `noclip`, `fly`, `give`, `impulse`, `kill`, `map`, `save`, `load`,
  `pause` (also the PAUSE key, with id's plaque), id's demo commands (`playdemo`,
  `timedemo`, `stopdemo`, `startdemos`, `demos`), `viewsize`, `sizeup`/`sizedown`, the
  `wasm_*` extras, `clear`, `help`.

## Web extras

Options > Web extras, drawn like id's Options page. All off by default; each is also a
console variable; the page remembers them. With all of them off, none of what they change
departs from id's. The list is one table, `WEB_EXTRAS` in `quake-rs/src/menu.rs`;
`AUDIT.md`, "Web extras", has the details.

| extra | console | what it changes |
|---|---|---|
| Uncapped framerate | `wasm_uncapped 1` | no 72 fps cap: a frame on every display refresh |
| Show FPS | `wasm_showfps 1` | QuakeWorld's `"%3d FPS"` readout, bottom right |
| Exact perspective | `wasm_exactpersp 1` | exact perspective at every pixel instead of id's 16-pixel spans |
| Scaled 2-D layer | `wasm_scaled2d 1` | status bar, menus and console blown up from 320x200, as the port drew them before |

## Layout

| path | what |
|---|---|
| `quake-rs/` | the engine crate: a library plus the `quaketool` CLI. `quake-rs/README.md` lists every module with the id file it ports. |
| `quake-rs/src/` (top level) | file formats (`pak`, `wad`, `bsp`, `mdl`, `spr`, `crc`), `math`, the QuakeC VM (`progs`, `vm`, `builtins`), BSP collision (`world`), savegames (`save`), demo parsing (`demo`), particles, beams, dynamic lights and sound control (`particles`, `tent`, `dlight`, `snd`) |
| `quake-rs/src/server/` | the server, split along id's files: `sv_main`, `pr_edict`, `pr_cmds`, `sv_phys`, `sv_user`, `sv_world`, `sv_move`, `msg`, `lightstyle`, `host` |
| `quake-rs/src/render/` | the 3-D renderer, split along id's files: `edge` (`r_edge.c`, `r_bsp.c`, `r_draw.c`, `d_edge.c`), `raster` (the span routines), `surf`, `light`, `sky`, `warp`, `alias`, `polyse`, `sprite`, `part`, `view`, `vis`, `world`, `stats` |
| `quake-rs/src/` 2-D | `draw`, `screen`, `sbar`, `menu`, `keys`, `console` |
| `quake-rs/src/client/` | the game client: `cl_main` (the live frame), `cl_demo`, `cl_tent`, `cl_input`, `view`, `host` (`Host_FilterTime`), `host_cmd` (level loads, cheats). The browser runs it; `quaketool play` runs it natively. |
| `quake-wasm/` | the browser's platform layer (about 3k lines of code and 6k of end-to-end tests): the exported functions the page calls, the host state (menu, console, clocks, framebuffer), carrying out the client's sound calls for Web Audio, saves in localStorage, the `wasm_*` extras. No `wasm-bindgen`, no dependencies. |
| `web/` | the page (`index.html`), nine headless-Chromium checks (`verify_*.py`), the benchmark (`bench.py`) and a screenshot tool (`shoot.py`) |
| `oracle/` | id's WinQuake built headless from the C, and the scripts that diff its frames against the port's (`oracle/README.md`) |
| `census/` | helpers for the gameplay census (`CENSUS.md`): id's edicts dumped and diffed against the port's, a QuakeC symbol dump |
| `gen_samples.py`, `gen_progs.py` | synthetic assets and progs, so the engine's tests need no game data |
| `screenshots/` | older rendered output, from before the 2026-09-25 renderer work |

## How it is checked

- **Tests.** `cargo test --release` in `quake-rs`: 587 library + 1 `quaketool` + 8
  integration tests, no game data needed. In `quake-wasm`: 126 end-to-end tests against the
  embedded shareware pak (plus one ignored harness, `oracle_screen`). All pass at `31775f5`.
- **Golden renders.** `quaketool scene <pak> maps/e1mN.bsp out.ppm` for e1m1, e1m2, e1m3;
  the sha256 prefixes at `31775f5` are `4807aaa1`, `9ae2b478`, `c65b7046`. A change leaves
  them byte-identical, or it is a deliberate fidelity fix and `AUDIT.md` records the move
  with its pixel count.
- **The oracle.** `uv run oracle/compare.py` renders the same view, clock and entities in id's
  renderer and the port and counts matching palette indices. Against id's x86 16-pixel spans
  the standard views (e1m1, e1m2, e1m3, e1m7) match 99.91–99.98% with square pixels and
  100.00% at the page's 4:3 aspect (e1m7 99.997); entity pixels 100%. `oracle/screen2d.py`
  does the same for the 2-D layer. `oracle/README.md` has the numbers and what is left.
- **The census.** `quaketool census` plays all nine maps headless through the real QuakeC;
  with `census/` it diffs id's server edicts against the port's (`CENSUS.md`).
- **The browser.** `uv run --with playwright web/verify_<name>.py` for walk, ambient, demo,
  input, menu, save, loops, extras and timedemo: each boots the real page in headless
  Chromium.
- **Native and browser agree.** `quaketool play <pak> demo1,walk_e1m1,walk_e1m3,fire_e1m1,quad_e1m1
  --res 320x200,640x400 --hash-every 30` prints the same frame hashes as the browser
  (`uv run --with playwright web/bench.py --hash-every 30` with those workloads).
- **Speed.** `uv run --with playwright web/bench.py --build --native` (the page in headless
  Chromium plus a native twin); `QUAKE_BENCH=30 QUAKE_RES=WxH quaketool scene ...` (one fixed
  view); `quaketool simbench` (game logic only). `PERF_PLAN.md` has the measurements.

## Performance

A software renderer, so the cost grows with the pixel count. In the browser (headless
Chromium, wasm), id's demo1 at 1280x800 took 22.6 ms a frame at the start of 2026-09-25
and 4.5 ms after it (median; the two measured in different sittings at similar load —
`PERF_PLAN.md` has the table and what each change bought). The largest gains came from
doing what Quake did: id's edge-sorted span renderer, the palette shift as 256-entry ramps,
the surface cache for dynamically lit walls, mip levels, and sending the client only the
entities in its PVS. Absolute milliseconds swing with machine load; compare builds in one
sitting.

Quake measured itself with `timedemo demo1`, and so does the port: type it in the console,
or run `quaketool timedemo <pak> demo1 --res 640x400`. It draws the same 969 frames as id's
C (985 and 1090 for demo2 and demo3) and prints id's line. Against id's portable C built
from the source (`oracle/`) at the page's pixel aspect, one sitting: the port natively runs
demo1 at 2602 / 1007 / 516 fps at 320x200 / 640x400 / 960x600 where id's C runs 1822 / 745 /
419, and the browser (wasm) at 1916 / 723 / 381 (`PERF_PLAN.md` §10).

## What is left

- **The control departures** above: a decision, not work.
- **Missing binds:** `default.cfg`'s F-keys and `t` (`messagemode`). (No loading plaque
  either, on purpose: a level loads within one frame.)
- **Demo playback makes no dynamic lights** (explosions light the walls live, not in demos).
- **Smaller faithfulness gaps**, each small or rare: `give` is not `Host_Give_f`; no
  pitch drift on slopes (`cl.idealpitch` fixed at 0); a gibbed player's head leaves no blood
  trail; `objerror` does not end the game; torch flames stay live edicts; the player is not
  edict 1, so edict numbers are one off against id's; several nearby torches of one sample
  are louder than id's (one Web Audio source each, where id combines them); a map with no
  lighting lump renders lit where id draws it fullbright (test maps only).
- **Not measured:** real GPU browsers, Firefox, Safari, phones, a real 120/144 Hz display;
  the Keyboard Lock handling of Esc in fullscreen (headless has none).
- **Delivery:** the pak is inside the wasm, so an engine update re-downloads it
  (splitting it out is PERF_PLAN D4).

The full list, with the evidence, is `AUDIT.md`, "Open, as of 2026-09-25".

## More

- `STATUS.md` — where things stand, and the history.
- `AUDIT.md` — every faithfulness finding and fix, with its evidence.
- `CENSUS.md` — the gameplay census.
- `PERF_PLAN.md` — the performance plan and its measurements.
- `oracle/README.md` — the oracle, its results and its caveats.

## Licensing

Derived from Quake's GPLv2 source (© 1996–1997 id Software), so GPL-2.0-or-later. No game data
is included.
