# quake-rust

A Rust port of id Software's *Quake* (1996), ported file by file from id's GPLv2 WinQuake
C source. Only the standard library in every crate (no crates.io, no vendored code), no
`unsafe` anywhere (`#![forbid(unsafe_code)]` on every crate and binary), and a browser
page of plain JS/HTML with no npm and no build step beyond cargo.

It has two profiles:

- **Classic** is id's WinQuake. With every extra off, the port draws the same frames as
  id's own renderer built from the C, keeps the same game state, and runs at id's 72 fps
  `Host_FilterTime` cap. One command proves it (below).
- **2026** is the default: an idealized software-rendered Quake on a 2026 machine. There
  is no frame cap, and it plays the same from 60 to 480 Hz. The picture fills the window
  at its native resolution in whole, crisp pixels, with a widescreen field of view. It
  keeps the look of a software render: palette-true, never filtered, with id's lighting
  and colormaps.

It plays the shareware episode in single player: the attract demos, New Game into the
`start` hub, E1M1–E1M8 with Chthon, intermission and finale, save and load, the menus and
the console. With the player's own `pak1.pak` it plays the registered game. It has no
multiplayer and no netcode.

It runs in a browser, where the game is a WASI program in a Web Worker, and natively
through `quaketool`, which has no window.

## Play it in a browser

The commands run from the repository root.

**The game data.** The repo has none. Fetch id's freely redistributable shareware
`pak0.pak`:

```sh
curl -sL -o quake106.zip https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/main/bin/quake106.zip
unzip quake106.zip resource.1 && bsdtar -xf resource.1 ID1/PAK0.PAK   # the zip holds an LZH archive
mkdir -p quake-data && mv ID1 quake-data/
```

**Build.** There are two builds of the same program. Either one runs in the same page.

```sh
(cd quake-wasm && cargo build --release --target wasm32-wasip1-threads)   # the 3-D view on every core
(cd quake-wasm && cargo build --release --target wasm32-wasip1)           # one thread
```

The threads build is the one to play on a desktop: at 2560x1440 it draws a frame in about
a third of the single-thread build's time. The single-thread build needs less memory,
which suits phones (iOS especially).

**Assemble a directory** with the page, the program and the pak, at any path outside the
repo (for the single-thread build, copy its `quake.wasm` from `wasm32-wasip1` instead):

```sh
D=$HOME/quake-web
(cd web && uv run python -c "import isolated; isolated.copy_page('$D')")
cp quake-wasm/target/wasm32-wasip1-threads/release/quake.wasm "$D/"
mkdir -p "$D/id1" && cp quake-data/ID1/PAK0.PAK "$D/id1/pak0.pak"
```

**Serve it.** The page needs `SharedArrayBuffer`, so it must be *cross-origin isolated*.
That takes two things:

- a **secure context**: `https://`, or `http://localhost`;
- the two headers `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp`.

```sh
miniserve -C -p 8080 --index index.html --header "Cross-Origin-Opener-Policy:same-origin" --header "Cross-Origin-Embedder-Policy:require-corp" "$D"
```

Then open `http://localhost:8080/`. To play from another device, put the server behind
https: any reverse proxy or tunnel that terminates TLS works. Plain http to a machine's
name is not a secure context, and the page cannot run there. A server that cannot send
the headers still works: the page's service worker adds them, after one reload.
`web/PLATFORM.md` ("Build, serve, deploy") has the details.

**Controls** in the 2026 profile:

- Move and fight: WASD and the mouse (click to capture it), Space to jump or swim up,
  C to swim down, click to fire, 1–8 for weapons, Tab for the scores.
- The rest: Esc for the menu, `~` for the console, F for fullscreen.
- A gamepad works as a twin-stick pad, and a phone gets touch controls.

Classic has id's `default.cfg` keys:

- the arrows to move and turn, `,` and `.` to strafe;
- `a`/`z` to look up and down, `d`/`c` to swim up and down;
- the mouse turns and walks; it looks up and down only while `\` or the middle button is
  held (`+mlook`).

The page's "keys" button lists everything.

## Classic and 2026

Every departure from id's game is a setting. The 2026 profile turns most of them on;
Classic turns them all off and restores `default.cfg`'s bindings. What 2026 changes:

- **Frame rate:** no 72 fps cap, a frame every display refresh. Jumps, flashes, trails
  and clocks are stepped so they match id's 72 Hz values (`FRAMERATE.md`).
- **Picture:** it fills the window at the window's own shape, in whole pixels (Pixel
  size: Auto, or 1–4 screen pixels a game pixel), past id's 1280x1024 limit. It uses Hor+
  widescreen. The status bar, menus and console are blown up by a whole number.
- **Look:** monsters glide between their 0.1 s steps, and id's crosshair is on.
- **Controls:** mouse look, Always Run, WASD, Space swims up, F for fullscreen, a modern
  gamepad layout with rumble, and touch controls on a touch screen.
- **Sound:** id's mixer at the device's rate, with four of its bugs fixed. Classic mixes
  at id's 11025 Hz.

Show FPS and Exact perspective are off in both profiles. `AUDIT.md`, "The profiles and
the departures", has the table: each setting, its console name, and why it exists.

**To switch:**

- **Options > Classic / 2026:** ←/→ switches profile, and Enter lists every setting to
  change one by one.
- **The address:** open the page as `?classic` or `?2026`.
- **The console:** `profile classic` or `profile 2026`.

The choice is kept in `config.cfg`, in the browser's storage. The file holds a `profile`
line plus only what the player changed.

## How Classic is proven

```sh
uv run oracle/classic_check.py      # about a minute once built; prints ALL PASS, exit 0
```

It builds the port and runs nine checks with every departure off. Five compare the port
with a recording from a tree known to be Classic:

- `goldens`: three golden renders;
- `play`: the browser's client frames, natively (id's three demos and four walks at
  three sizes);
- `timedemo`: the frame counts;
- `census`: a headless playthrough of all nine maps;
- `edicts`: id's server edicts diffed against the port's.

Four compare it live with id's WinQuake, built headless from id's C (`oracle/`):

- `oracle`: the 3-D view;
- `screen2d`: the 2-D layer;
- `demolerp`: demo playback, frame by frame;
- `sound`: the mixer, sample by sample.

The C oracles need docker once (`oracle/build.sh`, `oracle/build_sound.sh`).
`oracle/README.md` has every number and what each residue is.

The rest of the checks:

- `cargo test --release` in `quake-rs`: no game data needed.
- `cargo test --release` in `quake-wasm`: against the real pak.
- `cargo clippy --release --all-targets`: 0 warnings in both crates.
- 15 headless-browser checks: `QUAKE_VERIFY_PORT=8561 uv run --with playwright
  web/verify_walk.py "$D"`, likewise `verify_menu`, `_settings` (add `--with pillow`),
  `_present`, `_touch`, `_gamepad`, and so on.

## Tools

`quaketool` is the engine from the command line: id's files printed, maps rendered, the
game run headless, and the port's side of every check against id's C.

```sh
cd quake-rs && cargo build --release
./target/release/quaketool --help                                          # every command
./target/release/quaketool scene ../quake-data/ID1/PAK0.PAK maps/e1m1.bsp e1m1.ppm  # a golden
./target/release/quaketool timedemo ../quake-data/ID1/PAK0.PAK demo1 --res 640x400  # id's benchmark
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --check    # 60–480 Hz against 72
```

## Numbers

Measured on 2026-09-26 on a 16-thread desktop; timings are noisy, so read ratios, not the last
digit.

- **Classic against id's C.** The 3-D view is 100.00% of pixels on the eight standard
  views at the page's aspect (e1m7: 99.997%, two pixels). The 2-D layer matches except
  three explained residues. The mixer is sample-identical in 28 of 28 cases. Demo
  playback over 17,500 frames has `cl.time` identical in every frame.
- **Speed, id's way:** `timedemo demo1` on one thread, alternated with id's portable C
  built from the source.

  | demo1 | the port | id's C | ratio |
  |---|---:|---:|---:|
  | 320x200 | 2563 fps | 1895 fps | 1.35x |
  | 640x400 | 1113 fps | 774 fps | 1.44x |
  | 960x600 | 617 fps | 433 fps | 1.42x |

- **At 2026 sizes:** native `timedemo demo1` with the 2026 video settings, frames per
  second.

  | threads | 1920x1080 | 2560x1440 | 3840x2160 |
  |---|---:|---:|---:|
  | 1 | 208 | 120 | 54 |
  | 8 | 689 | 455 | 210 |
  | 16 | 682 | 450 | 224 |

- **In the browser** (headless Chromium, demo1 at 2560x1440, pixel size 1, median page
  frame): the threads build takes 3.3 ms on the GPU and 5.4 ms with software compositing.
  The single-thread build takes 10.8 and 15.4 ms.
- **Input to screen:** 12–13 ms median at 60 Hz for a key, the mouse or a pad. That is
  half a refresh's wait plus a frame (`web/latency.py`).
- **Download:** `quake.wasm` is 1.3 MB (0.43 MB gzipped), and the pak is 17.8 MB. The
  service worker caches both for offline play.

## Layout

| path | what |
|---|---|
| `quake-rs/` | the engine crate: the library and `quaketool`. `quake-rs/README.md` lists every module with the id file it ports. |
| `quake-wasm/` | the browser platform: the WASI program (`fn main`, events on stdin, frames and sound on stdout, files through `std::fs`) and its end-to-end tests |
| `web/` | the page (`index.html`, `wasi.js`, `touch.js`, `sw.js`), the browser checks (`verify_*.py`), `bench.py`, `latency.py` |
| `oracle/` | id's WinQuake built headless from the C, and the scripts that diff it against the port |
| `census/` | the gameplay census's helpers (`CENSUS.md`) |
| `gen_samples.py`, `gen_progs.py` | synthetic assets, so the engine's tests need no game data |
| `screenshots/` | old renders, from before 2026-09-25 |

## Read more

- `STATUS.md`: where things stand, what is not verified, what is left, and the history.
- `AUDIT.md`: every difference from id's game found so far, with its evidence, the
  departures table, and the one open list.
- `web/PLATFORM.md`: the browser platform (worker, protocol, shared memory,
  presentation, threads, sound, input, touch, offline), and how to build, serve and
  deploy.
- `FRAMERATE.md`: the same game from 60 to 480 Hz.
- `oracle/README.md`: the oracle, its results and its caveats.
- `CENSUS.md`: the gameplay census.
- `PERF_PLAN.md`: performance, measured.
- `CODE_PLAN.md`: the code-quality plan, what is done and what is next.

## Licensing

Derived from Quake's GPLv2 source (© 1996–1997 id Software), so GPL-2.0-or-later. No game data
is included.
