# quake-rust

id Software's *Quake* (1996), ported to Rust from the WinQuake C source. It uses only the
standard library and has no `unsafe` code. It plays in a browser; natively, `quaketool`
runs the same engine without a window.

It can be id's game, checked against id's own code, or that same software renderer given a
2026 machine.

| Classic | 2026 (the default) |
|:---:|:---:|
| ![E1M1 in the Classic profile](screenshots/classic-e1m1.png) | ![E1M1 in the 2026 profile](screenshots/2026-e1m1.png) |
| 320x200 in a 4:3 frame, id's status bar, id's 72 fps cap | the window's shape and size in whole pixels (here 960x540 shown at 2x), no frame-rate cap |

Both are stills from `quaketool shot`, so the 2026 crosshair isn't drawn.

## What it is

- **A port of WinQuake's single-player game, file by file:**
  - the file formats and the QuakeC virtual machine;
  - the server's physics, monsters and combat;
  - the edge-sorted software renderer;
  - the status bar, the menus and the console;
  - the sound mixer.

  It plays the shareware episode, the registered game with your own `pak1.pak`, and the
  mission packs — Scourge of Armagon and Dissolution of Eternity — with your own
  `hipnotic`/`rogue` game directory alongside `id1`'s, natively (`-hipnotic`/`-rogue`;
  `AUDIT.md`, "The mission packs' own file layout and progs"). There is no multiplayer.
- **Checked against id's code.** id's C, built headless (the "oracle", in `oracle/`), is the
  reference. With every extra switched off, the port and id's C agree on:
  - 100.00% of pixels in the standard 3-D views (two pixels differ on one map);
  - the status bar, menus and console, apart from a few explained differences (the version
    string, the video-mode list, the port's own Options rows);
  - the mixer's output, sample for sample, on 28 scripted cases;
  - demo playback, frame by frame.

  One command re-runs all of it; see [Proof](#proof).
- **A 2026 profile.** It shows what software-rendered Quake looks like when the hardware is
  no longer the limit:
  - native resolution in whole pixels, at any window shape, with a wider field of view;
  - no frame-rate cap: a new frame on every display refresh;
  - still 8-bit, with id's palette, colormaps and lighting, and textures are never
    filtered.
- **Plain Rust.** `#![forbid(unsafe_code)]` is on every crate and binary. That includes the
  browser build: an ordinary `fn main()` program (WASI) in a Web Worker, with no exports of
  its own, no bindings and no JavaScript toolchain.

## Play it

The commands are for a POSIX shell (`sh`, `bash`, `zsh`), run from the repository's root.
You need:
- Rust 1.85 or later, with the `wasm32-wasip1-threads` target;
- `uv`;
- `curl`, `unzip` and `bsdtar` (Debian's `libarchive-tools`);
- a static file server that can send headers. The examples use `miniserve`.

**1. The data.** The repo has none. Fetch id's freely redistributable shareware pak:

```sh
curl -sL -o quake106.zip https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/main/bin/quake106.zip
unzip quake106.zip resource.1 && bsdtar -xf resource.1 ID1/PAK0.PAK   # the zip holds an LZH archive
mkdir -p quake-data && mv ID1 quake-data/ && rm quake106.zip resource.1
```

**2. Build and assemble** a directory with the page, the program and the pak:

```sh
rustup target add wasm32-wasip1-threads
(cd quake-wasm && cargo build --release --target wasm32-wasip1-threads)
D=$HOME/quake-web
(cd web && uv run python -c "import isolated; isolated.copy_page('$D')")
cp quake-wasm/target/wasm32-wasip1-threads/release/quake.wasm "$D/"
mkdir -p "$D/id1" && cp quake-data/ID1/PAK0.PAK "$D/id1/pak0.pak"
```

The target `wasm32-wasip1` builds a single-threaded program instead. It runs in the same
page and needs less memory: add that target and copy its `quake.wasm` from
`target/wasm32-wasip1/release/`.

**3. Serve it** with two headers, and open `http://localhost:8080/`:

```sh
miniserve -p 8080 --index index.html \
  --header "Cross-Origin-Opener-Policy:same-origin" \
  --header "Cross-Origin-Embedder-Policy:require-corp" "$D"
```

The game uses shared memory, which browsers allow only on a *cross-origin isolated* page.
That needs those two headers and a secure context: `https://`, or `localhost`. To play from
another device, put the server behind anything that serves https. If a host can't send the
headers, the page's service worker adds them after one reload.

In the 2026 profile you play with WASD and the mouse (click the game to capture the mouse):

| key or input | does |
|---|---|
| Space | jump, or swim up |
| C | swim down |
| click | fire |
| 1–8 | choose a weapon |
| Esc | the menu |
| `~` | the console |
| F | fullscreen |

A gamepad or a touch screen also works. Classic uses id's own `default.cfg` keys. The
page's **keys** button lists them all.

## Classic and 2026

Every difference from id's game is a named setting. Classic turns them all off; 2026, the
default, turns most of them on.

| | Classic | 2026 |
|---|---|---|
| frame rate | id's 72 fps cap | a frame every display refresh; jumps, lifts, flashes and trails are stepped to match 72 Hz ([FRAMERATE.md](FRAMERATE.md)) |
| picture | a fixed mode (960x600 by default) in a 4:3 frame, id's 90° field of view | the window's own size and shape in whole pixels (pixel size Auto or 1–4), a wider view on wide screens |
| status bar, menus, console | 1:1, as id drew them | scaled up by a whole number |
| monsters | move in id's 0.1 s steps | glide between the steps; their animation frames are not blended |
| controls | id's `default.cfg` | WASD, mouse look, Always Run, a crosshair, a twin-stick gamepad with rumble, touch controls |
| sound | id's mixer at 11025 Hz | id's mixer at the device's rate, with four of id's bugs fixed |

In both profiles the renderer splits each frame across all CPU cores, and the picture is the
same on any number of them.

To switch, use **Options > Classic / 2026**, the address (`?classic` or `?2026`), or the
console (`profile classic`). The choice is saved in `config.cfg`, in the browser's storage,
and that file keeps only what you changed. [AUDIT.md](AUDIT.md) ("The profiles and the
departures") lists every setting and why it exists.

Two extras sit outside the profiles:
- drop your own `pak1.pak` onto the page to play episodes 2–4, and your CD tracks to hear
  them as the CD played them — or, running your own server, put them beside its
  `index.html` and it offers them itself, for every player on it ([PLATFORM.md](web/PLATFORM.md),
  "A server's own files"; no game data is in this repo);
- the page can be installed as an app, and it works offline.

## How it works

```
 the page (web/)                          the program (quake.wasm, in a Web Worker)
 ───────────────                          ─────────────────────────────────────────
 keys, mouse, pad, touch  ── stdin ───▶   fn main(): wait for a tick, run one frame
 WebGL2 canvas            ◀── shared ──   8-bit pixels and a 256-colour palette
 AudioWorklet             ◀── shared ──   16-bit PCM from id's mixer, in a ring
 pak via HTTP, IndexedDB  ◀── WASI ───▶   std::fs: pak0.pak, s0.sav, config.cfg
```

Events go in on stdin. Frames and sound come back through memory that the page and the
worker share.

- **The engine is a library.** [`quake-rs`](quake-rs/README.md) holds the game's systems.
  Each frame it takes the player's input and returns three things: an 8-bit image, the
  frame's colour shifts for its palette, and sound calls. A host adds the frame loop,
  input routing, the files, the screen and the speakers. `quake-wasm` is the browser's
  host; `quaketool` drives the same frames natively.
- **The browser build is a normal program.** `quake-wasm` has a `main` loop that reads
  events from stdin and writes records to stdout. `web/wasi.js` is a small WASI host. It
  lets the worker block between frames on `Atomics.wait`, and it serves the program's
  files: the pak it fetched, plus the saves and `config.cfg` that the page keeps in
  IndexedDB. Saves and the config are real files, written the way the C wrote them.
- **The frame stays 8-bit to the end.** The page uploads the palette indices, and a shader
  looks each one up in the frame's 256-colour palette, like a VGA DAC. A 2-D canvas is the
  fallback.
- **Every core, the same picture.** After id's BSP walk and edge scan, which run on one
  thread, the view is cut into row bands. Each band goes through id's drawing passes in
  id's order on its own thread, so every thread count gives the same frame.
- **id's mixer, not the browser's.** The engine paints PCM with `snd_mix.c`'s code into a
  ring it shares with an AudioWorklet.

[web/PLATFORM.md](web/PLATFORM.md) covers the protocol, shared memory, threads, sound,
input, touch and offline play in detail.

## Proof

```sh
uv run oracle/classic_check.py      # about a minute once built; prints ALL PASS
```

This runs the Classic profile through nine checks. Four compare the port with values
recorded from a tree known to be right. Five run id's C next to the port:

| check | compares | against |
|---|---|---|
| `goldens` | three reference renders (by hash) | recorded |
| `play` | the browser's client run natively on id's demos and scripted walks (frame hashes) | recorded |
| `timedemo` | id's benchmark: the frame counts | recorded |
| `census` | a headless playthrough of all nine maps | recorded |
| `edicts` | the entities' fields against id's server on all nine maps (the known differences, such as static flames and random numbers, are recorded) | id's C |
| `oracle` | the 3-D view, pixel by pixel | id's C |
| `screen2d` | the status bar, menus and console | id's C |
| `demolerp` | demo playback, 17,500 frames | id's C |
| `sound` | the mixer, sample by sample | id's C |

The id's-C checks need id's WinQuake source ([id-Software/Quake](https://github.com/id-Software/Quake))
and docker to build the oracle once. Put the source at `quake-c/` (so that
`quake-c/WinQuake` exists), or point `QUAKE_C_SRC` at its `WinQuake` directory.
[oracle/README.md](oracle/README.md) has every result and explains each remaining
difference.

Beyond Classic:
- `cargo test` passes in both crates, and clippy shows zero warnings.
- `quake-rs/target/release/quaketool framerate quake-data/ID1/PAK0.PAK --check` runs 22
  gameplay scenarios at high frame rates and compares them with id's 72 Hz.
- 17 headless-browser checks (`web/verify_*.py`) cover everything from walking and the
  menus to the gamepad, touch, quitting, sound through late frames, and reading back the
  canvas.

## Numbers

Measured on 2026-09-26 on an 8-core desktop CPU; timings are noisy. Read the ratios,
not the last digit.

- **Classic, against id's own C:** on one core, `timedemo demo1` runs 1.35–1.44x as fast as
  id's portable C built from the same source, from 320x200 to 960x600
  ([PERF_PLAN.md](PERF_PLAN.md)).
- **2026 video, natively** (`timedemo demo1`, frames per second):

  | threads | 1920x1080 | 2560x1440 | 3840x2160 |
  |---:|---:|---:|---:|
  | 1 | 208 | 120 | 54 |
  | 8 | 689 | 455 | 210 |

- **In the browser** (headless Chromium, 2560x1440): a frame takes about 3.3 ms with the
  threads build on the GPU, and 10.8 ms on one thread.
- **Input to canvas:** about 12 ms at 60 Hz, from the event to the frame on the page's
  canvas.
- **Download:** `quake.wasm` is 1.3 MB, and the pak is 18.7 MB.

None of it has been measured yet on a real phone, in Safari, or on a real high-refresh
display.

## The repository

| path | what |
|---|---|
| [`quake-rs/`](quake-rs/README.md) | the engine (about 70,000 lines with its tests) and `quaketool` |
| `quake-wasm/` | the browser program: the WASI `main` loop, its protocol, and end-to-end tests against the real pak |
| `web/` | the page, the WASI host (`wasi.js`), touch controls, the service worker, the browser checks |
| `oracle/` | id's WinQuake built headless from the C, and the scripts that compare it with the port |
| `census/` | helpers for the gameplay census ([CENSUS.md](CENSUS.md)) |
| `screenshots/` | the two renders at the top of this README |

Further reading:

| file | what it covers |
|---|---|
| [STATUS.md](STATUS.md) | where things stand, what isn't verified, what's next, and the history |
| [AUDIT.md](AUDIT.md) | every difference from id's game found so far, with evidence |
| [CODE_PLAN.md](CODE_PLAN.md) | the code-quality plan: done, and next |
| [PERF_PLAN.md](PERF_PLAN.md) | performance work, measured |

## License

Derived from id Software's GPLv2 Quake source (© 1996–1997 id Software), so
GPL-2.0-or-later. No game data is included.
