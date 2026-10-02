# The browser platform

The browser build is an ordinary Rust program. `quake-wasm` builds a binary,
`quake.wasm` (`wasm32-wasip1`), with a `fn main()` that reads the page's
events from stdin, writes each frame's picture and sound to stdout, and
keeps its saves and `config.cfg` through `std::fs`. It has no exports beyond
WASI's `_start`, no imports beyond what `std` asks of WASI, and
`#![forbid(unsafe_code)]`. The page is a small WASI host around it.

There are three pieces:

| file | runs in | what it does |
|---|---|---|
| `quake-wasm/` → `quake.wasm` | a Web Worker | the game: `sys::run`, a host frame per tick (`quake-wasm/src/sys.rs`); the records it reads and writes (`proto.rs`) |
| `web/wasi.js` | the same Worker | the WASI host: stdin from a shared ring, stdout into shared frame slots (or, the threads build, the frames left where they lie in the program's shared memory), a sound ring, and one message per turn, an in-memory file system, the clocks |
| `web/index.html` | the page | the canvas and the display's DAC (WebGL2, else a 2-D canvas), keyboard and mouse, the audio device (an AudioWorklet playing the program's samples), IndexedDB, and the display's refresh, which it hands the program as ticks |

The rest of this file is the design, the protocol, what it measured against
the page it replaced (the wasm cdylib with ~84 `#[no_mangle]` exports the
page called every frame, `3866e1b`), browser support, and how to build and
serve it.

## A frame

```
page (main thread)                  worker: wasi.js + quake.wasm
------------------                  -----------------------------
keydown/mouse ──KEY/MOUSE records──▶ ring ─▶ fd_read(0) ─▶ Key_Event, IN_MouseMove
                                            (the program is blocked here, in
                                             Atomics.wait, between frames)
requestAnimationFrame
  poll the gamepad; if it changed,
  GAMEPAD ─────────────────────────▶ ring ─▶ fd_read(0) ─▶ kept for the frame
  AUDIO_CLOCK, TICK(seq, dt) ──────▶ ring ─▶ fd_read(0) returns the tick
  spin on ACK ≥ seq (≤ 30 ms)                host::step(dt): Host_FilterTime,
                                             IN_Commands (the pad's keys), the
                                             client frame (IN_JoyMove), menu,
                                             console: an 8-bit frame and its
                                             palette (V_UpdatePalette); id's mixer
                                             paints to the clock + mix-ahead
                                             fd_write(1): PCM ─▶ samples copied into the
                                                                 sound ring
                                                          FRAME ─▶ copied into a free
                                                                   frame slot, or
                                                          FRAME_AT ─▶ where it lies in
                                                                   shared memory
                                                          AUDIO, STATE ─▶ kept
                                                          SYNC ─▶ ACK = seq, notify;
                                                                  post the kept records
  the newest frame to the GPU (indices
  + palette, WebGL2) and drawn, or its
  RGBA through putImageData
message event: STATE → the page's UI,
  REPLY → calls, AUDIO → counts
                                    (between ticks, every 8 ms while the
                                     worklet plays: AUDIO_WAKE ─▶ the mixer
                                     tops the ring up, PCM)
AudioWorklet (audio thread): plays the sound ring, moves its clock
```

- **The program waits in a read.** Between frames `sys::run` is blocked
  reading stdin; `wasi.js`'s `fd_read` waits with `Atomics.wait` on the
  ring's write index. A key or a mouse move wakes it, is applied at once
  (`Key_Event`, `IN_MouseMove`), and it reads on; a `TICK` ends the read and
  runs a host frame. The worker's own event loop never runs again after
  `_start`, so nothing reaches the program by `postMessage`: every event goes
  through the ring.
- **The page presents in the same refresh.** The refresh that posts a tick
  spins (the main thread may not `Atomics.wait`) until the program answers,
  then presents. That keeps the old page's timing, which computed the frame
  inside the refresh; only the hand-off below is added. One tick is in flight at a time; a refresh that
  finds the last one unanswered posts none (its time goes into the next
  tick's `dt`) and presents whatever has come. The spin is bounded by 30 ms,
  so a level load or a long automation call does not freeze the page.
- **The 72 fps gate stays in the program.** A tick is the display's refresh
  and `dt` is the raw time since the last one, exactly the old `step(dt)`
  export's argument; `Host_FilterTime` decides whether a host frame runs.
  A skipped tick answers with a `SYNC` and no `FRAME`, and the page presents
  nothing new.
- **A timedemo polls.** While `timedemo` runs, the program's `SYNC` says
  `wait = 0`; the host then answers the next read at once with whatever is
  queued plus an `END` record instead of blocking, so frames run back to back
  as `Host_Frame` does, each timed by the program's own clock (`Instant`,
  i.e. `clock_time_get`). The page shows the newest frame each refresh; the
  three frame slots let the program write while the page reads.
- **Automation is a call.** A `CALL` record carries a line, `name arg...`,
  which `automation.rs` maps to the functions that used to be exports; the
  answer is a `REPLY` in a turn of its own (a `SYNC` with the same ack). The
  page's own buttons use it (`boot`, `boot_demo`), and so do the checks
  (`window.quake.call`, and `exp.name(args)`, a Proxy for the same).

## The protocol

All little-endian. `quake-wasm/src/proto.rs` is the reference and has
round-trip tests; `wasi.js` and `index.html` carry the same constants.

**In** (page → program, stdin): `[kind u8][0 u8][len u16]`, then `len` bytes.

| kind | record | payload |
|---|---|---|
| 1 | TICK | `seq u32`, `dt f64` (seconds since the last tick) |
| 2 | KEY | `keynum u8`, `down u8`, `0 u16`, `ch u32` (the character the layout typed; 0 none) |
| 3 | MOUSE | `dx f32`, `dy f32` (raw `movementX`/`movementY`) |
| 4 | CLEAR_KEYS | — (window blur: `ClearAllStates`) |
| 5 | POINTER_UNLOCKED | — (the port's `+mlook` release: lookspring) |
| 6 | AUDIO_READY | `ready u8`, `0 ×3`, `rate u32` (the AudioContext's sample rate; 0 none yet) |
| 7 | CALL | `id u32`, then the UTF-8 line |
| 8 | END | — (written by the host, not the page: "nothing more queued") |
| 9 | WINDOW | `w u32`, `h u32`: the page's box for the picture in device pixels (its CSS size x `devicePixelRatio`; the whole screen in fullscreen), sent at start and on every resize; `dpr f32`: that `devicePixelRatio` (an older page sends none: read as 1) |
| 10 | AUDIO_CLOCK | `pos u32`: the sound ring's play position, in sample pairs (wrapping); sent before every TICK |
| 11 | AUDIO_WAKE | `pos u32`: the same, written by the host between ticks while the worklet plays: "mix now" |
| 12 | PRESENT | `format u8`: how the page shows frames from now on (0 RGBA8, 1 INDEXED8; RGBA8 until it says). The page sends it before the first tick |
| 20 | GAMEPAD | `connected u8`, `standard u8`, `buttons u8`, `0 u8`, `pressed u32` (bit per button), `axes f32×6` (a standard pad: the sticks, then the triggers' values): the pad's state, polled each refresh before the tick and sent when it changed ("Input", below) |

A record whose payload is shorter than its kind's reads the missing fields as
zeros, and an unknown kind is skipped, so either side can grow a record.

**Out** (program → page, stdout): `[kind u8][0 u8 ×3][len u32]`, then `len` bytes.

| kind | record | payload |
|---|---|---|
| 1 | FRAME | `w u16`, `h u16`, `format u8` (0 RGBA8, 1 INDEXED8), `0 ×3`, then for INDEXED8 the palette (256 × RGBA), then the pixels (4 bytes each, or one palette index) |
| 2 | SYNC | `seq u32` (last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
| 3 | STATE | `flags u32` (1 menu, 2 console has the keyboard, 4 live game, 8 binding a key, 16 timedemo, 32 native resolution, 64 Alt+Enter toggles fullscreen), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel) |
| 4–11 | — | retired: the sound records of the page's own mixing, before the program mixed |

| 3 | STATE | `flags u32` (1 menu, 2 console has the keyboard, 4 live game, 8 binding a key, 16 timedemo, 32 native resolution, 64 Alt+Enter toggles fullscreen (`vid_altenter`), 128 touch controls (`in_touch`), 256 the menu asks y or n, 512 the live game is paused), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel) |
| 4 | SAMPLE | `id u32`, a RIFF/WAV (each distinct sample once, by content) |
| 5 | SOUND | `id u32`, `origin f32×3`, `volume f32`, `attenuation f32`, `entity i32`, `channel i32`, `view u32`, `loop_start f32`, `loop_end f32` |
| 6 | STOP_SOUND | `entity i32`, `channel i32` |
| 7 | STATIC_SOUND | `id u32`, `origin f32×3`, `volume f32`, `attenuation f32`, `loop_start f32`, `loop_end f32` |
| 8 | AMBIENT | `channel u32`, `id u32`, `loop_start f32`, `loop_end f32` |
| 9 | LISTENER | `origin f32×3`, `forward f32×3`, `right f32×3`, `ambient f32×4`, `volume f32` |
| 10 | GENERATION | `generation u32`: `S_StopAllSounds` (a level or mode change) |
| 11 | LOCAL_SOUND | `id u32`: `S_LocalSound` (menu clicks) and `play` |
| 12 | REPLY | `id u32`, `value f64`, then UTF-8 text |
| 13 | BENCH | `f64` per value (`--features bench`; names from the `bench_names` call) |
| 14 | PCM | `start u32` (the pair of the ring's clock it plays at), `rate u32`, `flags u32` (1: silence the ring first, `S_ClearBuffer`), then 16-bit stereo pairs. Copied into the sound ring by `wasi.js`, never posted |
| 15 | AUDIO | `rate u32`, `mode u32` (0 Classic, 1 2026), then counts: `starts`, `local`, `stops`, `clears`, `painted` (u32 each) |
| 16 | CD | `serial u32` (a new value: play the track from its top), `track u8`, `looping u8`, `mode u8` (0 stopped, 1 playing, 2 paused), `0 u8`, `volume f32` (0..1): the CD player's state, written when it changes, and only with a disc ("CD music") |
| 17 | FRAME_AT | `w u16`, `h u16`, `format u8`, `slot u8`, `0 u16`, `pixels u32`, `palette u32`: a frame left in the program's shared memory, ring slot `slot`, its pixels and palette at those addresses (`-sharedframes`) |
| 20 | RUMBLE | `strong f32`, `weak f32`, `ms u32`, `pad u32` (1: the pad is read): the pad's two motors, or a phone's vibration (2026's `joy_rumble`) |

A turn's records end with its `SYNC`. A tick's `PCM` comes before its
`FRAME`, so the samples reach the ring before the pixels are copied.

## Shared memory

Three `SharedArrayBuffer`s:

- **Control and ring** (`CTL_BYTES` 256 + 64 KB, made by the page). The
  control block is an `Int32Array`: `IN_WRITE`/`IN_READ` (the ring's byte
  counters; the page writes whole records and then moves `IN_WRITE`, so the
  program never sees half a record), `ACK` (the last tick the program
  consumed), `SYNCS` (turns so far), `LATEST`/`FRAMES`/`READING`/`SHOWN`/
  `SLOTS_GEN` (the frame slots), `RUN` (starting, running, exited,
  crashed), `WAIT` (whether the program waits for ticks), and each slot's
  width, height and format, and where its frame is (`SLOT_SRC`: the frame
  slots, or the program's memory at `SLOT_ADDR`, its palette at
  `SLOT_PAL`). The same table is at the top of `wasi.js` and of the page's
  script.
- **Frame slots**: three, made by the worker as large as the largest frame
  so far. The worker writes a frame into a slot that is neither the newest
  (`LATEST`) nor the one the page is reading (`READING`), then publishes it;
  the page claims `LATEST` in `READING` (re-checking it did not move) before
  copying out. A frame larger than the slots (the first one, or a higher
  resolution) gets a new set, which the worker sends the page and numbers in
  `SLOTS_GEN`; the page presents nothing until it holds the set `SLOTS_GEN`
  names, so the frame shows one refresh later. No resolution limit lives in
  the host, and at the default 960×600 the slots take 1.7 MB (indexed
  frames; 6.9 MB RGBA).
- **In place** (the threads build): the program's own memory is a shared
  `WebAssembly.Memory`, so `wasi.js` passes `-sharedframes` and sends the
  page the memory, and the program leaves each frame where it drew it,
  saying where in a `FRAME_AT` (`quake-wasm/src/present.rs`). It keeps its
  last three frames, a ring: each frame takes the next slot, and the buffer
  it replaces goes back to the renderer's frame pool. The page claims the
  newest as before; the host lets a `FRAME_AT` return only once the page is
  not reading the ring's next slot (`Atomics.wait` on `READING`, which the
  page notifies when it lets go). The page can only claim the newest frame,
  so once a frame is published nothing can start reading the slot after it:
  a frame's memory is written, or handed back, only while no one reads it.
  No frame slots, and no copy in the worker. A grown memory reaches the page
  as a fresh `memory.buffer`.
- **The sound ring** (64 bytes + 32768 stereo pairs of 16 bits — 682 ms at
  48 kHz, enough for 2026's adaptive lead, "Sound" below — made by the page,
  handed to the worker and to the AudioWorklet). Its control block is an
  `Int32Array`: `POS` (the pair the device plays next — the clock the
  program mixes ahead of), `WRITE` (where the program's samples reach),
  `RATE` (their rate), `UNDER` (quanta the worklet played short once the
  program had written anything), `PLAYED`, `CLEARS`, `PEAK` (the loudest
  sample played since the page last reset it), `QUANTA`. Samples go in at
  `start & 32767`; the worklet plays pair `POS` when `0 < WRITE - POS <=
  32768`, silence otherwise. The same table is at the top of `wasi.js` and
  in the page's sound section.

## Presentation

The program draws 8-bit frames, a palette index a pixel, as Quake's
`vid.buffer` holds them, and `V_UpdatePalette` sets the palette they are
shown through each frame (the cshifts, then gamma: the renderer's
`FramePalette`). The page is the display's DAC:

- **WebGL2** (on a GPU): the page asks for `INDEXED8` (`PRESENT`). The
  frame goes up as an `R8UI` texture and the palette as a 256×1 `RGBA8`
  one, straight from the shared view where the browser takes one (Chromium
  does; else through one copy into a staging buffer, the smallest copy that
  works: a byte a pixel, 0.06 / 0.25 / 0.56 ms at 1280×800 / 1440p / 4K
  here, the upload after it no slower), and a fragment shader draws each pixel as
  `texelFetch(palette, texelFetch(frame, p).r)` — exact integers, no
  filtering, blending, dithering or colour conversion, so the canvas holds
  exactly the RGBA the program's own pack would. No RGBA pack runs in the
  program, and a palette shift costs 1 KB.
- **2-D canvas** (no WebGL2 — headless Firefox —, a WebGL2 drawn by the CPU,
  or `?canvas2d`): the page asks for `RGBA8`; the program packs its frame
  through the palette on the renderer's threads (`render::pack_rgba`, one
  4-byte store a pixel), and the page copies it into an `ImageData` for
  `putImageData`, which takes no shared memory.

A WebGL2 drawn by the CPU (SwiftShader, llvmpipe: the renderer string says
so) costs more than the 2-D canvas's copy — headless Chromium's
SwiftShader, demo1 at 2560×1440 on 8 threads: a page frame of 23.7 ms
against 7.2 — so the page takes the 2-D canvas there (`?webgl` takes WebGL2
anyway). A lost WebGL context (a GPU reset) is waited out: nothing is drawn
until the browser restores it, then the textures are made again and the next
frame shows; the game runs on. A context that never comes back leaves the
canvas blank until a reload (the 2-D canvas cannot take over a canvas that
had a WebGL context).

**The same pixels.** `web/verify_present.py` reads the canvas back
(`quake.readback()`: `readPixels` of the last frame drawn again, or
`getImageData`) and compares its hash with the program's RGBA for the same
frame (the `frame_hash` call) in the attract demo, the live walk, the Quad's
cshift, underwater (the warp and the water's shift), the menu's fade, the
console and at gamma 0.7, with WebGL2 (the shared views and the staging
copy, and after a lost and restored context) and with the 2-D canvas: equal
everywhere, in headless Chromium (SwiftShader and the GPU) and Firefox (the
2-D canvas; its headless build has no WebGL2). Natively, `quaketool play
--hash-every` over 28 runs (seven workloads at three Classic sizes, the
Quad's shift among them; 1080p modern on 8 threads; 1440p with the scaled
2-D layer) prints the same hashes as before the renderer went 8-bit
(`d5db64a`), and the goldens are unchanged.

**Measured.** `bench.py --video modern --threads 1,8`, demo1, the threads
build's bench program before (`d5db64a`: RGB frames packed to RGBA, copied
into a frame slot, copied out, `putImageData`) and after, in one sitting on
a 16-thread desktop (load 4–8), headless Chromium on a desktop GPU
(`QUAKE_GPU=1`: the integrated GPU through ANGLE on GL), uncapped rAF; median ms.
The host frame is the program's (`step`); the page frame is the rAF period,
everything the browser does for the frame included:

| | host frame before → after | page frame before → after (WebGL2) | after, 2-D canvas |
|---|---|---|---|
| 1280×800, 1 thread | 4.94 → 3.16 | 6.85 → 3.83 | 4.43 |
| 1920×1080, 1 thread | 8.72 → 5.65 | 11.71 → 6.43 | 8.26 |
| 2560×1440, 1 thread | 14.35 → 9.11 | 20.50 → 10.17 | 14.77 |
| 3840×2160, 1 thread | 29.45 → 19.20 | 41.87 → 20.70 | 30.05 |
| 1280×800, 8 threads | 2.06 → 1.32 | 3.75 → 1.72 | 2.58 |
| 1920×1080, 8 threads | 2.99 → 1.99 | 6.13 → 2.58 | 4.53 |
| 2560×1440, 8 threads | 4.67 → 2.87 | 10.62 → 3.42 | 7.61 |
| 3840×2160, 8 threads | 8.94 → 4.97 | 21.66 → 6.06 | 13.73 |

At 8 threads the page used to add 1.7 / 3.1 / 6.0 / 12.7 ms to the host
frame (the worker's copy into a slot, the page's copy out, `putImageData`,
and the RGBA the program packed); with WebGL2 it adds 0.4 / 0.6 / 0.6 /
1.1 ms (the page's own time, `js - wait`, is 0.2–0.6 ms: the uploads and the
draw call; the rest is the browser's). A frame in place costs the worker
0.1 ms where it cost ~3 ms at 1440p. The host frame itself shrank too: no
RGBA pack (0.8 ms at 1440p, 2.0 at 4K, on 8 threads), and the renderer
stores a byte a pixel instead of three. So 1440p on 8 threads fits a
240 Hz refresh (4.2 ms) and 1280×800 a 480 Hz one (2.1 ms), on an integrated GPU.
The 2-D canvas gains from the same program changes but still pays its two
copies and `putImageData`. On headless Chromium's software GL (SwiftShader,
no `QUAKE_GPU`) the 2-D canvas is the page's choice: page frame at 8 threads
3.91 / 6.44 / 10.45 / 21.03 before, 2.82 / 4.74 / 7.19 / 13.39 after
(WebGL2 forced, `?webgl`: 7.21 / 13.45 / 23.65 / 47.05 — the CPU drawing
the textures). The single-thread build (`wasm32-wasip1`, indexed frames
copied through the slots), GPU, page frame: 6.24 → 3.73, 11.28 → 6.74,
20.01 → 10.12 at 1280×800, 1920×1080, 2560×1440.

Input to present (`bench.py --latency 20`: the live walk uncapped, keys at
random moments, to the present of the first frame that consumed each; GPU,
8 threads, median / p95 ms): 1280×800 3.92 / 6.40 → 1.83 / 4.37;
2560×1440 10.49 / 12.00 → 3.16 / 4.42.

## Files

`wasi.js` keeps one directory tree in memory and preopens it as `/`, so the
program's relative paths work as on disk (`common.rs`, `COM_InitFilesystem`):
`id1/pak0.pak`, `id1/config.cfg`, `id1/s0.sav`… It implements what `std`
imports (`path_open`, `fd_read`/`fd_write`/`fd_seek`/`fd_close`,
`fd_filestat_get`, the prestat calls) plus `path_filestat_get`,
`path_unlink_file` and a few no-ops (a directory is any prefix of a file's
path; there is no `fd_readdir`); anything else a newer `std` imports answers
`ENOSYS` and is logged once.

- **The pak is a file.** The page downloads `id1/pak0.pak` beside
  `quake.wasm` and hands it to the worker's file system; the program opens it
  with `Pak::open` (as `quaketool` does) and reads each lump on demand, so no
  copy of the archive lives in the program's memory. It reads through id's
  search path (`quake_rs::common`: `pak0.pak`, `pak1.pak`, … over the game
  directory's loose files, the last pack searched first), so a player's own
  `pak1.pak` is one more file ("Your files"). Measured against the
  same program with the pak embedded (`include_bytes!` and
  `Pak::from_static`, the old page's way), six loads each on a local server:
  navigation to the first frame 169–328 ms from the file, 167–330 ms
  embedded, the same; the renderer process's memory 157–176 MB from the
  file, 191–214 MB embedded (the embedded pak lives in the module's bytes and
  in the linear memory). Besides, an engine update no longer re-downloads
  18 MB, the build needs no game data, and a player's own `pak1.pak` can be
  one more file (the worker cannot take files once running, so the page
  restarts the game to add one: "Your files").
- **Saves and settings go through `std::fs`.** `save s0` writes
  `id1/s0.sav` (`Host_Savegame_f`), the Load and Save menus list the slots
  from the files (`M_ScanSaves`, when they open), and `config.cfg` holds the
  settings the id way (`config.rs`, `Host_WriteConfiguration`): the profile,
  then the `bind` lines and archived cvars that differ from it, written when
  one of them changes and exec'd at startup as quake.rc does. When a written file is closed, `wasi.js` sends it
  to the page, which keeps it in IndexedDB (`quake-rs`, store `files`, keyed
  by path) and hands every kept file back at the next start. Without
  IndexedDB the page falls back to localStorage (`quake-rs.file.<path>`).
- **Migration.** The old page kept saves as `quake-rs.sav.<name>` and the
  settings as `quake-rs.resolution`/`.viewsize`/`.extras` in localStorage. At
  start the page moves them once into the game directory — the saves to
  `id1/<name>`, the settings as the `config.cfg` lines the program would have
  written — and removes the keys.
- A storage failure after the fact (quota) is printed on the console with
  `echo`, since the program's write already succeeded.

## Your files

A player who owns Quake (the Steam, GOG and CD versions all ship id's
original `id1/pak0.pak` and `id1/pak1.pak`) adds their files, and plays the
registered game — episodes 2–4 — with their CD soundtrack; a player who
owns Scourge of Armagon or Dissolution of Eternity adds that pack's own
`hipnotic/pak0.pak` or `rogue/pak0.pak` the same way, and plays it with
`?game=hipnotic`/`?game=rogue` (the page's game picker switches there,
once a pack is in reach — "The game picker", below).

- **Adding.** Drop the files, or the whole Quake folder, anywhere on the
  page, or pick them with the drawer's line ("Own Quake? …"). The page takes
  `pak1.pak` or a mission pack's own `pak0.pak` — which game directory a
  dropped file means is read from its own path (`hipnotic/pak0.pak`,
  `Quake/rogue/pak0.pak`, however deep a whole-folder drop nests it;
  `index.html`'s `dropGameDir`), id1 where there is none, exactly as
  before mission packs — a `pak0.pak` that is not id's shareware one (the
  same file in every 1.06 copy: recognised by `COM_LoadPackFile`'s count
  and CRC, 339 and 32981, and skipped), and CD tracks as files,
  `track02.ogg`… (the track number from the name: `track02`, `Track 2`, a
  leading `02`; any format the browser can play), under the same game
  directory as the pak beside them (`hipnotic/music/track02.ogg`, this
  page's own convention — the mission packs' own CDs had no file layout to
  match). It checks a pak as `COM_LoadPackFile` reads one — the `PACK`
  header, a directory inside the file of at most 2048 entries, each inside
  the file — and says what it left out and why.
- **Keeping.** A pak goes into its game directory, the IndexedDB store the
  program's files live in (`id1/pak1.pak`, `hipnotic/pak0.pak`…), and so to
  the worker's file system at every start; the music goes to a store of its
  own (`music`, keyed by `"<game dir>:<track>"` — each game directory has
  its own track 2..11, so a mission pack's soundtrack never collides with
  id1's or another pack's), which the worker never sees: the page hands the
  program the active game directory's own list of tracks (`-cdtracks
  2,3,…`, "CD music") and plays a track's file when the program asks for
  it. The files stay in this browser until removed (the drawer's "remove
  them", which removes every game directory's kept files at once).
- **Restarting.** The worker takes its files before it starts, so adding
  or removing files reloads the page: the engine and `pak0.pak` come from
  the HTTP cache, the saves and settings from storage. (A reload asks for
  the click that starts audio again; adding files is rare enough.)
- **The program decides**, as id's did: `COM_CheckRegistered` compares
  `gfx/pop.lmp` with the table in `common.c` ("Playing registered
  version." on the console, the `registered` cvar the QuakeC's episode gates
  read), and refuses a modified game without it ("You must have the
  registered version to use modified games") or a `pop.lmp` that is not
  id's ("Corrupted data file."); `PR_LoadProgs` refuses a `progs.dat` made
  against other system globals; and the port refuses one that *calls* a
  builtin id's engine never had (not merely declares one: the mission
  packs' own re-release `progs.dat` declare two id's never did, but never
  call either — AUDIT.md, "The mission packs' own file layout and progs").
  A refusal ends the program before it starts, its message on stderr; if it
  came with files just added, the page takes them out again, restarts, and
  shows the message in the drawer. With `?game=hipnotic`/`?game=rogue` and
  no such file in reach, the mission pack's own game directory just
  contributes nothing to the search path — the program starts anyway, as
  plain shareware (or registered, if `pak1` is there), not a refusal.
- **Not supported, and why.** The 2021 re-release's files
  (`rerelease/id1/pak0.pak`) are a different game build: its `progs.dat`
  calls the new engine's builtins by name (numbered `#0`, resolved at load),
  which the port does not have and will not fake — the port plays id's 1996
  WinQuake. The page leaves out anything under a `rerelease/` folder and
  says to use the `id1/` files beside it; a re-release pak dropped on its
  own reaches the program, which refuses it (a modified game, or its
  progs). A mod beyond the two mission packs (`-game` with any other
  directory) is out of the port's scope; the page only ever recognises
  `id1`, `hipnotic` and `rogue`.

`verify_content.py` checks it all with synthesized data: a `pak1.pak` made
from `common.c`'s `pop[]` table and the shareware `maps/e1m1.bsp` copied as
`maps/e2m1.bsp`, and generated tones as tracks 2, 3 and 6; and a synthesized
`hipnotic/pak0.pak` (the same `e1m1.bsp`, as `maps/hip1m1.bsp`) for the
mission-pack case — no real mission-pack data is in the repo, or needed.

## The game picker

With a mission pack in reach — a server's `files.json` lists its
`hipnotic/pak0.pak` or `rogue/pak0.pak`, or the player kept their own
(`packsAvailable`) — the page offers a choice of game: Quake (`id1`),
Scourge of Armagon, Dissolution of Eternity (and whichever one `?game=`
asks for even if its data is missing, so there is always a way back to
Quake: the previous bullet's silent fallback). A plain `id1` deploy shows
no picker anywhere. One renderer (`gameList`) puts the same choices in
reach at every stage:

- **the start overlay**, under "click to start" / "tap to start", through
  the download too;
- **the bar** under the view: "game", beside fullscreen · sound · keys,
  opens them over the view's corner (a click elsewhere, or Esc, closes
  them);
- **a phone's menu**: GAME, beside BACK while the menu is up (the attract
  demo's first tap, or MENU in a game, gets there), opens them in a panel
  over the menu (touch.js);
- **the quit screen** (endscreen.js), under id's end screen; any other key
  or tap still restarts the game that quit.

Each is a button: the game running filled and marked (`aria-current`), the
others outlined, 44 CSS px tall on a touch screen.

**The address is the choice.** A choice is a plain link to the same page
with `?game=hipnotic` / `?game=rogue` (none for Quake; `gameHref` keeps the
rest of the query as written: `?2026&game=hipnotic`) — a reload, not a live
switch: a game directory is a command-line argument to a fresh program
(`-hipnotic`), as it always was. `?game=` is the only state; nothing is kept
that could disagree with it, so a bookmark starts there and Back returns to
the game before. The reload keeps what the browser keeps (saves,
`config.cfg`, the player's files); an unsaved game ends, which the bar's
menu and the GAME panel say.

**A press on a choice is only that** (`gameList`: the link stops its own
`pointerdown`, `touchstart` and `click`). The user's report that on a phone the
expansion packs could not be chosen (2026-10-02) was this: the picker sat in
`#overlay`, whose click is the first gesture, and touch.js's overlay click
asks for fullscreen and the landscape lock. So until the next page arrived
(a network-first navigation through the service worker; on a phone's
network, a while) the old page took the tap as "tap to start" — overlay
gone, id1's attract demo under the finger, fullscreen asked for — and then
the reload threw that away into a boot screen whose picker looked as before:
the running game was plain text in the links' own grey, and a phone has no
hover to tell a link. Small text, too: 12.5 CSS px, two millimetres on the
phone. Now the overlay stays up, the chosen button pulses until the new
page replaces it, and the new page marks its game. Likewise the GAME panel
keeps its fingers from the touch layer (whose `pointerdown` captures a
finger for a menu tap), and GAME opens it on its `click`, not its
`pointerup`, or the panel would be under the finger when that tap's own
click arrives and take it as a tap beside the choices; on the quit screen
a choice's `pointerdown` stops before "any tap restarts".

**The installed app** (`start_url: ./`) opens Quake: it does not remember
the last game. Remembering is a second piece of state beside `?game=` — the
address would no longer say which game runs, and a deploy that later drops
a pack would open into a fallback. Accepted gap: a player who mostly plays
a pack picks it once per launch (one reload; its pak is already kept by the
service worker). Not done either: manifest `shortcuts` to each pack — the
manifest is one file for every deploy, so a shareware deploy would offer
packs it does not have.

**Also accepted.** On a desktop in fullscreen the bar is off screen (leave
fullscreen, or quit, to choose); the bar's menu and the quit screen's
choices are for the mouse (Tab is the scoreboard here, and any key on the
quit screen restarts).

`verify_touch.py` (11., at the phone size) checks the overlay's
buttons, a finger's pointerdown, pointerup and click on Scourge of Armagon
reloading as `?game=hipnotic` without the old page starting, GAME and its
panel (a tap beside the choices touches no menu row), and the quit screen's
choices; `verify_content.py` the bar's menu; both that a shareware-only
deploy shows none of it.

## A server's own files

A deploy can offer `id1/pak1.pak`, a mission pack's own `hipnotic/pak0.pak`
or `rogue/pak0.pak`, and the CD's tracks itself, so a player doesn't have to
drop them in every session — the point for a private deploy
of the registered game. It is the same "Your files" above, with the server
as one more source: optional, discovered quietly, and merged in before the
player's own drops. A mission pack's files are fetched only when that game
is the one starting (`planServerFiles`); listing one is what puts the game
picker on every player's page ("The game picker", above).

- **Discovery, without noise for a plain deploy.** The page fetches an
  optional manifest, `files.json`, beside `index.html`, once, at boot:
  ```json
  { "files": [
    { "path": "id1/pak1.pak", "size": 41894404 },
    { "path": "id1/music/track02.ogg", "size": 3456789 }
  ] }
  ```
  Absent (offline, or bad JSON) means pak0 only, and nothing else is
  fetched: no `GET id1/pak1.pak` to fail, no guessing which of tracks 2–11
  exist. That one quiet probe, instead of up to eleven failing ones, is the
  whole point of asking a manifest rather than trying paths directly — and
  it is truly quiet: a 404 for `files.json` would itself be a failing
  request Chromium logs as a console error no matter how the page's own
  code handles it, so the service worker (`web/sw.js`'s `manifestFirst`)
  answers a missing manifest with an empty one, `{"files":[]}`, 200, before
  it ever reaches the page — a plain deploy's console stays as clean as it
  was. Only a manifest present but broken logs anything (`console.warn`,
  below).
- **Each path is literal**, used both as the URL to fetch (relative to
  `index.html`) and as the key the engine's file system sees, exactly as
  `id1/pak0.pak` already is — so a manifest entry is spelled the way the
  files actually sit under the deploy dir, lower-case, `id1/`-prefixed.
  `size` is optional and used only where a response's `Content-Length`
  can't be trusted (below); it is never otherwise checked against what
  arrives.
- **One path into the program.** A manifest entry is sorted into a pak or a
  track by the same rules a dropped file gets (`isPakPath`'s pattern,
  `musicTrack`'s name parsing and format check): there is no second set of
  rules to keep in sync. A pak the manifest lists (pak0 itself is always the
  deploy's own, never the manifest's) is read with the same `readPak` a
  drop gets before it is trusted — a deploy mistake (a truncated upload, an
  unrelated file at that path) is left out with a `console.warn`, and the
  deploy stays pak0-only rather than refusing to start at all. A track that
  parses is kept by number; its bytes are never fetched here — "CD music"
  plays its URL directly once asked for, so offering ten tracks costs
  nothing until one is actually played.
- **Precedence: the server is the base, the player's own files win.** The
  server's paks are merged into the file list right after the deploy's own
  `pak0.pak` and before whatever the player has dropped and kept
  (IndexedDB), so a path they both provide — a player's own `pak1.pak`, say
  — ends up the one the engine sees, the same rule the shareware `pak0.pak`
  already followed. A track works the same, per number: the player's own
  kept track for N, if there is one, plays over the server's. Reasoning: the
  server is a convenience, set once by whoever runs it; a player who
  deliberately drops their own file onto the page is making a specific
  choice that should not silently lose to it.
- **Caching.** `id1/pak1.pak` and `id1/music/*` are routed through the
  service worker exactly as `id1/*.pak` already was — cache first, since
  id's own data never changes once deployed — so a reload plays the
  registered game and its music without a 40+80 MB re-fetch; bump
  `DATA_CACHE` to force a refetch after replacing the files on the server.
  `files.json` itself goes through the page's own network-first path, so a
  deploy that starts offering pak1 later is only picked up once a player has
  been online since — same trade-off as every other page file ("Offline and
  install").
- **The loading bar accounts for the extra bytes**: the manifest's paks are
  downloaded in the same `download()` call as `quake.wasm` and `id1/pak0.pak`,
  under the one combined progress bar. Their declared `size` is used as a
  fallback total only where a response's `Content-Length` is unusable (a
  compressed transfer: "Loading"'s comment) — most servers need it for
  nothing at all.
- **A pak that passes `readPak` but isn't actually id's registered data**
  (the wrong `pop.lmp`, say) is the engine's call, not the page's: it
  refuses to start at all, as it does for a dropped pak1 like it ("Your
  files"). There the page can undo the drop and retry once automatically;
  here there is no drop to undo, so the page shows the refusal and — unlike
  a player's own files — does not offer a "remove" button that could not
  remove a server's file anyway. Accepted gap: a broken server-offered pak1
  fails the same way on every load until the deploy is fixed. For a deploy
  one person runs for themselves, that is the right trade: loud and
  immediate, not a silent fallback that leaves the question of a missing pak1
  unanswered.
- **Deploying it.** Put the files where the client expects them and write
  the manifest from what is actually there:
  ```sh
  cp your-pak1.pak deploy/id1/pak1.pak
  mkdir -p deploy/id1/music && cp your-tracks/track*.ogg deploy/id1/music/
  mkdir -p deploy/hipnotic && cp your-hipnotic/pak0.pak deploy/hipnotic/   # a mission pack, if any
  (cd web && uv run python -c "import isolated; isolated.write_manifest('$D')")
  ```
  `isolated.write_manifest(dest)` (`web/isolated.py`) writes `dest/files.json`
  from whatever paks and `music/*` it finds under `dest/id1`, `dest/hipnotic`
  and `dest/rogue` (`id1/pak0.pak` excepted: the deploy's own), sizes
  included — never written by hand, so it can't drift from the files beside
  it. Skip the call (or delete `files.json`) for a pak0-only deploy.

`verify_content.py` extends its synthesized-data checks to a server-offered
pak1 and tracks (registered play and music from `files.json`, a player's own
drop taking precedence over it per track and per pak, a broken server pak1
leaving the deploy on pak0, and that a manifest-less deploy's network log
never shows a request for `pak1.pak` or any track).

## CD music

In 1996 Quake's music was the CD's own audio tracks, which the drive played
beside the game's mix, never through it: the engine only told the drive
"track N, looping", and the drive played it at its level. The port keeps
that split.

- **The program** asks where id's client asked (`quake_rs::cd_audio`,
  `cd_win.c`'s state): every level's signon (`svc_cdtrack`, the
  worldspawn's `sounds`: e1m1 is track 6, the start map 4), the QuakeC's
  intermission track 3 and episode-end track 2, a demo's (its header line
  forces one: id's `demo1` plays track 2 all through the attract loop), and
  `svc_setpause` pauses it. The same track asked for again goes on; another
  stops it and starts from the top. The `cd` command is id's (`cd play N`,
  `loop`, `stop`, `pause`, `resume`, `remap`, `info`, …). The drive's
  state goes to the page in a `CD` record when it changes.
- **The level** is `bgmvolume` (Options > CD Music Volume) as id's DOS
  driver set it, `(int)(bgmvolume * 255)`; WinQuake's MCI could not set a
  CD's level, so there the slider only switched the music off and on. Both
  profiles: a CD playing is id's behaviour, so Classic plays the player's
  music too.
- **The page** plays the track's file in an `<audio>` element (streamed:
  a seven-minute track is not decoded whole into memory), through a gain
  node (the level) into the page's AudioContext, beside the worklet that
  plays the program's mix: it starts with the first click, as the game's
  sound does, and a hidden tab pauses it with the game (WinQuake paused the
  CD when it lost the screen). A looping track loops in the element; a
  track played once reports its end (the `cd_ended` call: MCI's notify).
- **Without music** there is no drive: no `-cdtracks`, no `CD` records,
  nothing in the program changes (id's `cd_null.c`, which the C oracle is
  built with). The `cd` command says "No CD in player.".

The checks read the drive through `quake.cd.state()` (what the program
asked for; the element's time, loop and level; the output's RMS) and the
`cd_state` call.

## Settings, and how the page shows the picture

Every setting is the program's (`quake_rs::settings`: id's cvars and key
bindings, and the port's departures, which the profiles **Classic** and
**2026** switch). The page needs three of them, and hears them in the
`STATE` record:

- **Native resolution** (`vid_native`, 2026). The page sends its box for the
  picture in device pixels and its `devicePixelRatio` (`WINDOW`); the
  program renders the box divided by a whole pixel size (`vid_pixelsize`:
  1..4, or Auto, the smallest that keeps the frame within a 1080p frame's
  pixels per whole square root of the renderer's threads, from 2 on a
  phone: a ratio of 2 or more in a box whose shorter side is at most 540 CSS
  pixels, since a phone's cores are several times slower than a desktop's
  and slow further as it warms) and says the size in `pixel_size`; the page makes the canvas exactly `W x pixel_size` device
  pixels wide and `H x pixel_size` tall (`fitCanvas`), `image-rendering:
  pixelated`, so every picture pixel is a whole square of screen pixels at
  the box's own aspect (the view is Hor+: `fov_adapt`). Off (Classic), the
  picture is the video mode (`_vid_resolution`, Options > Video Options)
  in the largest 4:3 box the window fits, as before.
- **Alt+Enter toggles fullscreen** (`vid_altenter`, 2026; in Classic the
  chord is id's ALT `+strafe` and ENTER `+jump`): "Fullscreen", below.
- **The profile from the address.** `?classic` and `?2026` add `+profile
  classic` / `+profile 2026` to the program's command line (`wasi.js` hands
  it `args`), which quake.rc's `stuffcmds` runs after `config.cfg`: the same
  switch as the menu's, so it sticks.
- **A mission pack from the address.** `?game=hipnotic` / `?game=rogue` add
  `-hipnotic` / `-rogue` to the command line — `COM_InitFilesystem`'s own
  flags (`quake-rs`'s `common.rs`): the program layers that pack's own game
  directory over `id1`'s on its search path, exactly as `-basedir` always
  worked, so `hipnotic/pak0.pak` or `rogue/pak0.pak` beside `id1/pak0.pak`
  is all it needs. This is only the argument: whether the page actually has
  that file to send at all — a drop, or a server's own `files.json` — is a
  separate question ("Your files", "A server's own files"; the page's
  picker sets the argument: "The game picker"). Without the file, the
  program starts anyway and just plays plain `id1` (an empty `hipnotic`/
  `rogue` directory contributes nothing to the search path), the same as
  id's own engine would.

`verify_settings.py` checks all of it in the browser (the window filled
with whole pixels at devicePixelRatio 1 and 2, `?classic`, the switch, the
reload); the checks that pin id's behaviour open the page as `?classic`,
and `bench.py` does too, so its frames hash as `quaketool play`'s.

## Input

The page sends what the player does as it happens; the program decides what
it means, as id's `Key_Event`, `IN_MouseMove` and the joystick code do.

- **Keys by their place.** A key is its place on the keyboard (`KeyboardEvent.code`:
  letters, digits and punctuation as the US key in that place), as WinQuake's
  keys were scancodes (`scantokey`). So WASD walks on AZERTY or Dvorak too,
  `bind` names a place, and a key's release matches its press whatever Shift
  did in between; what the layout typed goes with the key (`ch`) for the
  console and the name fields. A key `code` does not name falls back to
  `key`. (`verify_input.py`: AZERTY's key in W's place is `w` in the game and
  types `z` in the console; letters used to follow the layout, `z`.)
- **The F-keys.** `default.cfg`'s shortcuts (F1 help, F2/F3 the Save/Load
  screen, F4 Options, F6/F9 quicksave/quickload, F10 quit, F12 a screenshot)
  reach the game as ordinary `KEY` records like any other key; the page's
  `keydown` handler calls `preventDefault()` for F1/F3/F5/F6/F10 (F5 is
  unbound, but losing the game to a reload is worse than losing the browser
  shortcut) so the browser's own help/find/reload/address-bar/menu-bar never
  fires alongside the bind. F11 and F12 are reserved by every browser and
  `preventDefault()` cannot get them back outside fullscreen; in fullscreen,
  `lockEscape()` locks F12 along with Esc (Chromium only — the "Esc in
  fullscreen" comment above `lockEscape` has the why), so F12 reaches the
  page there too. Windowed, F12's `screenshot` bind is unreachable, which
  the keys drawer says. `web/verify_fkeys.py` drives all eight end to end:
  F1-F4 each open their screen directly, F6/F9 round-trip `quick.sav`
  (`wait`'s one-frame delay included), F10 raises the Quit prompt without
  quitting, and F12 writes a `.pcx` once fullscreen locks it. It also
  presses F12 windowed, informationally: headless Chromium has no devtools
  UI to reserve F12 for, so it passes the key through regardless, and a
  `.pcx` is written there too — whether a real windowed browser keeps F12
  for itself, as this section says it should, is NOT verified by this
  check.
- **The raw mouse.** The pointer lock asks for `unadjustedMovement`
  (Chromium's raw input, on Windows, macOS and ChromeOS): id's
  `IN_StartupMouse` switched Windows' pointer acceleration off while the game
  ran, so a count was always the same turn. Refused (Linux, Firefox), the
  plain lock, at once and from then on (Chromium refuses a burst of lock
  requests). Each `mousemove` is a `MOUSE` record; a browser that coalesces
  samples into one event per refresh sums their movement into it, so every
  count arrives, and the program adds each as it comes: the turn per count
  is the same at any frame rate (`mouse_turns_the_same_at_60_and_480_hz`).
  `pointerrawupdate` would deliver samples sooner within a refresh, but the
  frame starts at the refresh either way, so it would not show them sooner.
- **The gamepad.** The Gamepad API has no events for a pad's state, so the
  page polls `navigator.getGamepads()` once per refresh, just before the tick
  (as late as the frame allows), and sends a `GAMEPAD` record when the state
  changed: the first connected pad with the standard mapping, else the first
  connected. The program keeps it, and the host frame the tick runs reads it
  as id's joystick: `IN_Commands` (buttons as `JOY1`.., `AUX5`.., the D-pad as
  the hat's `AUX29`..`AUX32`) and `IN_JoyMove`; quake-rs
  `client/in_win.rs` has the mapping from a standard pad to winmm's axes and
  buttons. Classic reads it only after `joystick 1` (id's default is 0); the
  2026 profile's pad is a twin-stick layout of `bind` lines and `joy*`
  settings, with its buttons as the menu's keys (`joy_menukeys`). A pad's
  button also takes the click-to-play scrim away (a browser may not count it
  as the gesture audio needs: then the first click or key starts the sound).
- **Rumble** (2026, `joy_rumble`): a `RUMBLE` record after a frame in which
  the player took damage (its strength from `V_ParseDamage`'s count) or fired
  a heavy weapon, saying whether the pad is read (`joystick`). The page plays
  it on the pad's `vibrationActuator` (`"dual-rumble"`, Chromium) or
  `hapticActuators[0].pulse` (Firefox, where enabled) — unless the pad is not
  read, or the touch screen was touched since the pad was last used: then a
  phone vibrates, through the touch controls' `rumble()` (`navigator.vibrate`,
  in the game only; Android). Never both. On a device with both, the pad and
  the touch controls otherwise just add up: the pad's move is `IN_JoyMove`'s,
  the touch stick's the client's analog `set_move`, and neither holds the
  other's keys.

`web/verify_gamepad.py` drives all of it with a synthetic pad (the scrim, the
menus, a walk, a turn, the rocket's kick and its blast's rumble, an unplugged
pad, Classic's `joystick 0` and `1`, and on a touch page the rumble going to
the pad or the phone, whichever was used last).
The synthetic pad stands in for the browsers' own Gamepad API; no real pad
was tried.

**Latency.** `web/latency.py` measures from each input event's `timeStamp`
(when the browser got it, so the wait for the refresh counts; a pad's
`timestamp`) to the `putImageData` of the first frame that consumed it, live
game, 2026 profile at 960×600, keys pressed, the mouse dragged and the right
stick moved at random moments for 15 s each (median / p95, ms; in brackets
the event's wait for its handler; a 16-core desktop, load 4–8):

| | key | mouse | pad | the frame (tick to present) |
|---|---|---|---|---|
| Chromium, 60 Hz rAF | 13.2 / 20.9 [0.3] | 11.9 / 20.5 [7.2] | 12.5 / 17.2 [8.9] | 4.1 / 5.8 |
| Chromium, 240 Hz emulated | 6.9 / 14.8 [2.0] | — | 4.2 / 5.5 [1.0] | 3.4 / 4.6 |
| Firefox, 60 Hz rAF | 13.7 / 17.7 [0.1] | 11.5 / 20.2 [7.0] | 11.8 / 16.9 [8.1] | 4.2 / 5.9 |

At 60 Hz an event waits on average half a refresh for the tick that takes
it, then the frame's 4 ms: 12–13 ms to the canvas, whatever the input. The
mouse's wait is in its dispatch (browsers deliver `mousemove` with the
refresh), a key's after it; the pad's is the poll's. At an emulated 240 Hz
(the page's loop paused and a 4.17 ms timer driving `quake.tick`, since
headless browsers refresh at 60 Hz) the pad, polled just before each tick,
takes 4.2 ms, one frame; a key 6.9 ms, with a tail where the browser held a
task behind its own 60 Hz frame after the input, which a real 240 Hz refresh
would not (the mouse is left out: its events still come at 60 Hz). Firefox's
240 Hz run lost most of its key presses to its test driver and is not in the
table. What none of this sees: from `putImageData` to light (the compositor
and the display: a refresh or two, one less with `?lowlatency` where it
works), and a device's own latency (USB polling, the browser's gamepad
poll).

Nothing cheap is left in the page: keys and mouse go to the program when
they happen and it applies them at once, the pad is read as late as the
tick, and the frame is presented in the refresh that ticked. What would cut
more is the browser's (`?lowlatency`, below) or the frame's own time.

## Fullscreen

**The ways in and out.** The bar's *fullscreen* button, in every profile;
the fullscreen key, **Alt+Enter** (Option+Return on a Mac), in 2026
(`vid_altenter`); the browser's own F11, which fills the screen with the
whole window (Alt+Enter still works inside it); and on a phone the first tap
(`touch.js`, "Touch").

**One rule for a shortcut: it works whatever has the keyboard** — the game,
the menu, the console, a demo, with the mouse captured or not. The page
takes Alt+Enter in a capture-phase `keydown` listener, before any other:
the game never sees the Enter (the Alt reaches it: `+strafe`, for a
moment), and what the key toggles is the browser's own
`document.fullscreenElement`, never a copy.

The key was F until 2026-10-02, and only in the live game with the menu and
the console down (they type and bind letters). In a real Chromium that
broke the most natural sequence: hold Esc to leave fullscreen, then F to go
back. A held Esc's first press is a tap the page gets (Keyboard Lock, below),
so the menu opens; 1.5 s later the browser drops the pointer lock and leaves
fullscreen, the menu still up; and F, gated on the menu, did nothing until
Esc closed it. A letter can't keep the rule (the console types it, a
player's bind takes it), so F is the game's again, unbound as in id's
`default.cfg`.

Why Alt+Enter: web games mostly offer a button in their own chrome and leave
the browser its F11; video players take F because they have no text to type
and no binds; games with a native heritage use Alt+Enter — QuakeSpasm's
`VID_Toggle`, DOSBox, Windows games at large — and browsers leave the
chord to the page. In Classic it stays the game's: `default.cfg`
binds ALT `+strafe` and ENTER `+jump`, a strafe-jump in WinQuake, so
Classic has the button and F11. `vid_altenter` was `vid_fkey`; a
`config.cfg` with the old name still sets it (`quake_rs::cvar`'s
`OLD_NAMES`) and the next save writes the new one.

**Esc in fullscreen.** Browsers reserve Esc. Where the Keyboard Lock API
exists (Chromium: Chrome, Edge, Opera) the page locks Escape (and F12, "The
F-keys" above) on entering fullscreen: a tapped Esc is the game's, id's
`togglemenu`, with the mouse still captured; a held Esc is the browser's
way out. Elsewhere the browser's two steps stand: the first Esc releases
the mouse (the page opens the menu, "Input"), the next leaves fullscreen.
The hint on entering says which.

**When the browser says no** — no user activation (a script's call, not a
key or a click), a frame embedding the page without `allow="fullscreen"`
(`document.fullscreenEnabled` false), no element fullscreen at all (an
iPhone's Safari) — the page says so in the console, with the browser's
reason, and on the view; the next press of the key or the button is a fresh
gesture.

**Verified.** In a headed Chromium 146 (2026-10-02: X11 on the Wayland compositor's virtual
output on a Linux desktop, keys typed through the X server's XTEST; keys sent over
the DevTools protocol reach the page but not the browser's own Esc handling,
so a held one never left fullscreen there): the walk above, before and
after — click to play, the key, play, hold Esc, the key again, from the
menu, the console, after a `changelevel`, after the mouse was released and
captured again, in Classic and with `vid_fkey 1`, F11, and a request
without activation (`TypeError: Permissions check failed`, reported).
`verify_extras.py` checks the page's side headless, where Chromium has
Keyboard Lock but no browser Esc handling. Not tried: Firefox and Safari
headed, macOS, Windows.

## Quit

`Host_Quit_f` (Menu > Quit > Y, or the console's `quit`) sends one more
record, `Quit` (`quake-wasm/src/sys.rs`'s `maybe_quit`), and the program
ends right after: `_start` returns, `wasi.js` marks it exited (`RUN` 2) and
posts `{t: 'exit', run: 2}`, which `gameExited` already treats as quiet —
as id's `Sys_Quit` called `exit(0)`. Typing `quit` where the console is the
keyboard's destination skips the confirmation and quits at once, exactly as
`Host_Quit_f` does; raised any other way (Menu > Quit, or a key bound to
`quit` while playing) it asks first, the same Quit prompt the menu always
had (`screen2d`'s, unchanged).

A page cannot exit, so the host does the next best thing:

- **Leaves fullscreen and releases the pointer and keyboard locks**
  (`document.exitFullscreen()`, `document.exitPointerLock()`,
  `navigator.keyboard.unlock()` where "Esc in fullscreen" held one) — the
  one thing the brief requires; everything past this is the "wanted" part.
- **Shows id's end screen**, if the pak has one: DOS Quake's `Sys_Quit`
  (`sys_dos.c`) copied `end2.bin` (registered) or `end1.bin` (shareware)
  onto the text screen — 80x25 of (character, attribute) VGA text-mode
  bytes, the DOS build's version stamped into row 0
  (`quake_rs::console::CON_VERSION`, the same string the console background
  carries). The `Quit` record's `registered` byte says which file the
  program read, and its 4000 bytes are that lump exactly
  (`quake-wasm/src/sys::end_screen`), so the page need not fetch or guess
  anything. `web/endscreen.js` draws them: CP437 mapped to Unicode (an
  inline 256-entry table — no font file), the 16-colour VGA palette for
  each byte's foreground/background nibble, and the attribute's top bit as
  a one-second blink. Without a screen (a modified pak missing both files,
  or none loaded at all) it shows a plain message instead; leaving
  fullscreen happens either way.
- **Restarts on any key, click or tap** — `location.reload()`. The
  simplest robust choice: the worker's `main` has already returned, so
  there is no live game left to hand input to, and a reload costs the
  player nothing — `config.cfg` and the saves are already files kept in
  IndexedDB ("Files", above), and the pak is re-served from the cache.

Phones get the same screen, scaled to fit a landscape width; a tap anywhere
dismisses it, same as a click. With a mission pack on offer the game
choices sit under the screen ("The game picker"): one starts that game, any
other key or tap this one again.

`quake-wasm/src/sys.rs`'s tests cover both quit paths (immediate from the
console, confirmed from the menu) and the record's exact bytes, including
the version patch; `web/verify_quit.py` drives the browser side: fullscreen
and the pointer lock, both released; the end screen on screen; a dismiss
that reloads into a running game again.

## Sound

The program mixes, as WinQuake did: id's `snd_dma.c`, `snd_mix.c` and
`snd_mem.c`, ported as `quake_rs::snd::Mixer`, run in the worker
(`quake-wasm/src/snd_dma.rs` is the device behind it, `snd_win.c`'s part).
The page only plays what it paints.

- **Samples out.** After every tick the program runs the frame's sound calls
  through the mixer, then `S_Update_`: it paints from where it left off to
  the ring's play position plus `_snd_mixahead`, and writes that as a `PCM`
  record. `wasi.js` copies the samples straight into the sound ring (no
  message to the page). An AudioWorklet on the page's AudioContext plays the
  ring and moves its `POS`.
- **The clock** is `POS`, the device's own: the page sends it before every
  tick (`AUDIO_CLOCK`). Between ticks, while the worklet plays, `wasi.js`
  wakes the program every 8 ms with an `AUDIO_WAKE`, and it mixes again —
  id's `S_ExtraUpdate`, so the ring stays fed whatever the display does. If
  the device overtakes the mixer (a level load, a stall), the mixer skips to
  it, as `S_Update_` did; the worklet plays silence for what was missing and
  counts an underrun.
- **Before the first click** (browsers start an AudioContext suspended until
  a gesture) the page moves `POS` on itself in real time, at most 0.1 s a
  refresh: the mixer runs unheard, as a sound card's DMA ran whether a
  speaker was on or not, so nothing queues up to play at once when sound
  starts (the old page had to drop sounds until then). The first gesture
  (the overlay, a button, the canvas, F) creates or resumes the context and
  attaches the worklet (its module is a string in `index.html`, loaded from a
  `blob:` URL: no extra file to deploy); `AUDIO_READY` tells the program it
  runs and at what rate.
- **Level changes.** `S_StopAllSounds` asks for `S_ClearBuffer`: the next
  `PCM` carries the clear flag and `wasi.js` zeroes the ring, so what was
  mixed ahead of the old level falls silent at once.
- **Menu and pause** are id's: `S_Update` runs every host frame whatever has
  the keyboard, so the level's loops and ambients play on under the menu and
  over a paused game (a test in `snd_dma.rs`); the menu's clicks are
  `S_LocalSound`s through the mixer.
- **A hidden tab** stops the game (no refreshes, no ticks). The page
  suspends the AudioContext with it and resumes it when the tab comes back,
  so the sound stops and goes on where the game does.

**Classic and 2026.** The setting is `snd_modern` (`Cvars::sound`, a
`quake_rs::snd::SoundMode`; "Full-rate sound" on the Classic / 2026 page),
a departure: off in the Classic profile, on in 2026. Classic
is id's mixer as written (`Fixes::NONE`) at id's `desired_speed`, 11025 Hz,
mixing id's 0.1 s ahead. The 2026 mixer (`Fixes::ALL`: the loop seam, exact
resampling steps, the ambient ramp at any frame rate, `S_StopSound`'s range;
`AUDIT.md`) runs at the device's own rate, 0.05 s ahead. A change of mode
makes a new mixer (the ring is cleared, the level's placed sounds are
registered again).

**Classic's 11025 Hz on a 48 kHz device.** The worklet reconstructs it: a
windowed sinc (Blackman, 16 input samples, 256 phases) band-limited at
5.5 kHz, as a sound card's DAC and output filter reconstructed id's 11025 Hz
for its speakers. Chosen over an AudioContext created at 11025 Hz (the
browser resampling it) because the filter is then the same in every browser
and explicit here, the context never has to be re-created — which can need a
new gesture in Safari — when the mode changes, and a context at 11025 Hz is
not guaranteed everywhere. The 2026 mixer's samples play pair for pair: its
character is id's point resampling at the device's rate (what `-sspeed 48000`
gave), images and all.

**Latency** (headless Chromium, the attract demo, 20 s per mode): the ring's
lead — how far ahead of the device the program has mixed, which is how long
a sound started now waits — is at the mix-ahead between wakes: 2026 p1 39,
median 50, max 50 ms; Classic p1 89, median 100 ms. The context adds its own
`baseLatency` + `outputLatency`: 10.7 + 40 ms headless (a real device's
differ). No underruns in either mode (also none at a 30 ms mix-ahead; at
20 ms the headless device ran dry 281 times in 20 s: its callbacks take
larger bites), nor in `verify_loops.py`'s 40 s. The checks read the ring
(`quake.audio.ring()`: its position, lead, underruns, loudest sample) and
the mixer (`snd_stats`, `snd_channels`: every channel's sample, volumes,
position and key).

**A late host frame.** `wasi.js` runs the program and mixes it on the same
worker: while a host frame is busy — a slow render pass, a GC pause, a core
another process is using — the worker cannot mix, and cannot answer the
page's `AudioWake` between ticks either (that only fires while the program
blocks waiting for the next tick, which a busy frame is not doing). A 2026
lead of `MODERN_MIXAHEAD` (50 ms) only outlasts a frame about that long; a
slower one runs the ring dry mid-frame, and the `PCM` that finally ends the
stall is itself painted from the device's position at the *top* of the frame
(like id's own `S_Update_`), so by the time it lands that same stretch of
real time has already passed again — what's left over from it is what the
*next* frame like it draws on, which is why surviving a *run* of slow frames
needs a lead at a little over twice one of them, not once (`quake-wasm/src
/snd_dma.rs`'s `adapt_modern_ahead`, below).

This is 2026-only host plumbing, not a departure to weigh against Classic:
id's mixer (`Mixer::samples_ahead`) is untouched, and Classic's
`_snd_mixahead` stays id's fixed 0.1 s always, whatever a frame takes — the
same way the ring itself, or `wasi.js`'s turn-taking, are plumbing rather
than settings. What changes is only the value 2026 hands that same,
unmodified mixer call:

- **The ring is twice the size** (`RING_PAIRS`: 32768, was 16384 — 682 ms at
  48 kHz, 743 ms at 44.1 kHz) so a grown lead has somewhere to live; a
  `samples_ahead` call not shortened by the ring's own cap was the point, not
  a latency change by itself (the cap was never reached before this, and
  `MODERN_MIXAHEAD` did not move).
- **The 2026 lead is adaptive**, `quake-wasm/src/snd_dma.rs`'s
  `Audio::modern_ahead`, fed by how long each host frame actually took to
  compute (an `Instant` around `step` in `Sys::frame`, `quake-wasm/src/
  sys.rs`): a frame that ran long jumps the lead at once to 2.5x it (one to
  cover the stall just measured, one more left over for a repeat of it, plus
  a margin against the next one running a little longer still) — clamped to
  550 ms, comfortably short of the ring's new cap — and holds there, not
  easing down the moment it catches up (a *sustained* run at the same length
  must not reopen the gap it just closed, the bug an earlier, stricter
  version of this had: `snd_dma.rs`'s tests name it). Short frames ease the
  lead back towards `MODERN_MIXAHEAD` over a few seconds, so a fast desktop
  keeps today's latency. The first frame of a new stall still glitches once —
  nothing can see it coming — and a *sustained* stretch past about
  220 ms/frame (under 4.5 fps) still underruns sometimes, bounded by how far
  past it the frame runs: there is only so much a bounded ring can buy.
  `snd_stats`'s `mixahead_ms` shows the lead live.

**Proof (headless Chromium, phone viewport, a built-in test hook).**
Chromium's own CPU throttle (`Emulation.setCPUThrottlingRate`) does not
reach a Worker — measured: the page's `wait` for the program's frame is
unchanged by it, throttled or not. `quake-wasm/src/bench.rs`'s `maybe_stall`
(`--features bench`; zero code otherwise) holds a host frame late on
purpose instead, set with the `stall_ms` automation call
(`web/verify_audio_resilience.py`). Timings here are noisy, and at the time
of this round another job was pinning a core at 900%+ CPU
continuously for hours — a live instance of the very thing above ("a core
another process is using") — so every number below was taken under that
load, both sides of the comparison, back to back, same machine, same
minute, e1m1, the 2026 profile (a `stall_ms` sweep, the same measurement
`web/verify_audio_resilience.py` makes at one level, each level held 6 s,
against a build with `adapt_modern_ahead`'s call sited to always return
`MODERN_MIXAHEAD` unmoved — "before" otherwise meaning the code before this
round):

| `stall_ms` | before: underruns/min (`mixahead_ms`) | after: underruns/min (`mixahead_ms`) |
|---:|---:|---:|
| 0 (unthrottled) | 280 (50) | **0** (50-70) |
| 80 | 9,660 (50) | 208 (266) |
| 100 | 13,005 (50) | 218 (243) |
| 120 | 14,698 (50) | 170 (343) |
| 150 | 14,455 (50) | 548 (410) |
| 180 | 14,675 (50) | 1,174 (548) |

Before this fix, `mixahead_ms` never leaves the 50 ms floor (it cannot: the
cvar was never touched) and every stall level underruns at essentially the
device's full rate — the ring never catches up. After it, every level is
cut by 12-85x, the lead visibly grows with the stall, and the unthrottled
row is the sharpest line: even with no deliberate stall, a contended
machine alone cost the unfixed build 280 underruns/min, and the fix cut
that to zero — the adaptive lead defends against real, unplanned
contention, not only a synthetic one. 180 ms/frame, held forever, is past
`adapt_modern_ahead`'s derived limit (~220 ms/frame) even before this
load's own contribution, so it alone is not driven to zero — the ring (even
doubled) cannot buy an unbounded lead; a `stall_ms` run on a quiet machine,
or any level at or under 150 ms here, clears that bar. After the stall
ends, the lead eases back under 90 ms within 5 quick seconds, underruns
rare while it does. `web/verify_audio_resilience.py`'s bars are set
generously enough to pass under this same contention.

## Threads

A program built for `wasm32-wasip1-threads` imports a shared `env.memory` and
`wasi.thread-spawn`, and the game's renderer then draws each frame's 3-D view
on several threads (quake-rs `render/band.rs`: row bands after the edge scan,
the same pixels for any count). `wasi.js`:

- makes the shared memory with the limits the module's import section
  declares (the JS API does not tell them, so `importedMemory` reads them);
- before the program starts, makes a pool of thread workers (as many as
  `navigator.hardwareConcurrency`, 2–16), each another instance of
  `wasi.js` — a worker made after its parent has blocked may never start;
- answers `thread-spawn` by claiming a free worker. Its first thread goes
  by message (the module, the memory, the thread id and start argument);
  the worker instantiates the module once, calls `wasi_thread_start`, marks
  itself free when the thread ends and then waits, with `Atomics.wait`, on
  its own slot of a shared `jobs` array (`[seq, tid, arg]`). A later
  `thread-spawn` writes the thread there, bumps `seq` and wakes it: a thread
  costs a wake-up, not a message and an instantiation. `std`'s futexes are
  wasm atomics on the shared memory, so `join`, `Mutex` and channels need
  nothing more from the host. With every worker busy it answers EAGAIN;
- passes the program `-hwthreads N`, the threads it may count on
  (`hardwareConcurrency`, at most the pool plus its own). If the pool cannot
  be made, the program runs alone (`-hwthreads 1`).

A thread has the clocks, randomness, sleep and stderr (to its worker's
console: the parent never reads messages again). The files, stdin and
stdout stay the main program's, and a thread cannot spawn threads yet.

**The renderer's threads** are the cvar `r_threads` (quake-wasm `App::
render_threads`, the typed `quake_rs::render::Threads`): 0, the default,
takes every thread the host offers (`-hwthreads`; a `wasm32-wasip1` build,
without threads, gets 1), n takes n. `host::step` hands the resolved count
to the renderer of whichever game draws the frame, every frame, so each
`Walk` and `DemoPlay` the host builds (a boot, a load, the attract loop's
next demo) draws with it from its first frame. A spawn the host refuses
(more threads asked than workers) leaves its bands to the threads that did
start, so any count draws the frame. The `render_threads` call reports the
resolved count. The RGBA pack runs on the same threads.

**Measured.** `threadcheck`'s rounds of seven scoped threads (a round:
spawn, run, join) take 25 µs with the workers kept, against 214 µs when
each thread instantiated the module afresh. The game, bench build
(`bench.py --build --threads-build --video modern --threads 1,2,4,8`), demo1,
headless Chromium on a 16-thread desktop (load 2.6 → 6.2), median ms:

| | render3d 1 / 2 / 4 / 8 threads | host frame 1 / 8 | page's frame (`js`) 1 / 8 |
|---|---|---|---|
| 1280×800 | 4.14 / 2.83 / 2.30 / 1.82 | 4.80 / 2.18 | 6.11 / 3.47 |
| 1920×1080 | 7.43 / 4.96 / 3.56 / 2.92 | 8.57 / 3.50 | 11.19 / 6.20 |
| 2560×1440 | 12.34 / 7.55 / 5.39 / 4.22 | 14.47 / 5.21 | 19.07 / 9.89 |

walk_e1m1 is alike (2560×1440: render3d 9.87 → 3.10 ms). At 8 threads the
host frame is half the page's: the frame's pixels into the shared slot,
the page's copy out and `putImageData` (0.9 ms each at 1440p) are one
thread's. With the threads build's shared memory the page now reads the
frame where it lies ("Shared memory", "Presentation"). The frames are the same at
every count: `bench.py --hash-every 30` over fire_e1m1, walk_e1m3 and demo1
at 1920×1080 prints the same hashes at 1 and 8 threads (one page per count:
a page's runs share QuakeC's random stream).

`web/verify_threads.py` builds `quake-wasm`'s `threadcheck`
(`src/bin/threadcheck.rs`: four `std::thread::scope` threads sum their part
of 1..4,000,000, a spawned thread answers over a channel, then 200 rounds
of seven scoped threads reuse the workers) and runs it in the host: it
passes in headless Chromium and Firefox (a round 25 and 37 µs). The nine
page checks pass on the threads build as on `wasm32-wasip1`'s (Chromium;
walk and demo in Firefox too).

## Measurements

Headless Chromium on a 16-core Linux desktop, the old page (`3866e1b`,
`--features bench` where noted) against this one, A/B in one sitting (load
average 1.5–3.8). Absolute milliseconds swing with load; compare within a
row.

**The same frames.** `bench.py --hash-every 30` prints the FNV hash of every
30th presented frame. Over demo1, walk_e1m1, walk_e1m3, fire_e1m1 and
quad_e1m1 at 320×200 and 640×400 (22 hashes each), this page's hashes equal
`quaketool play`'s natively, on the stock build and the bench build; demo1
and walk_e1m1 at 640×400 and 1280×800 equal the old page's too. The canvas
bytes are the same.

**The same host frame.** The program's frame (the sum of its phase timers,
bench builds; fixed dt 1/72, uncapped rAF, 600 frames after 60 warm-up,
median of two rounds, ms):

| workload | 640×400 old / new | 960×600 old / new | 1280×800 old / new |
|---|---|---|---|
| demo1 | 1.43 / 1.60 | 2.78 / 2.93 | 4.57 / 4.62 |
| walk_e1m1 | 1.07 / 1.09 | 2.19 / 2.29 | 3.70 / 3.76 |

Within 5%, but for demo1 at 640×400 (+12%, both rounds). At 60 Hz with the
page's own loop, the 1280×800 walk's frame measured 4.92 / 5.33 ms old and
4.87–5.20 ms new. The page's own loop paces the same: at 60 Hz both show a
frame every refresh with none more than 20 ms apart, and in headless
Chromium's uncapped refresh (about 92 Hz here, where the 72 fps gate runs
every other one) the attract demo shows 47 frames a second old and 45 new.

**Timedemo.** `timedemo demo1` in the page (`verify_timedemo.py`), two
rounds each, fps:

| | 320×200 | 640×400 | 960×600 |
|---|---|---|---|
| old | 2063 / 2028 | 752 / 739 | 376 / 357 |
| new | 1983 / 1913 | 734 / 714 | 358 / 339 |

3–5% fewer: each frame is still a turn (a polling read, a `SYNC`, a message
to the page). The pixels of a frame the page has not shown the last of are
not handed over (as the old page presented one frame per 12 ms slice);
before that change the new page did 328 fps at 960×600.

**The hand-off.** What the worker adds to each frame (non-bench build, 60 Hz,
median ms): the program's pixels into the shared slot, the page's copy out of
it (the old page presented an `ImageData` over wasm memory with no copy), the
rest of the turn, and waking the worker.

| | 640×400 | 960×600 | 1280×800 |
|---|---|---|---|
| pixels into the slot (worker) | 0.07 | 0.19 | 0.24 |
| slot into the ImageData (page) | 0.06 | 0.18 | 0.25 |
| sounds, listener, state, `postMessage` (the page's own mixing then) | 0.02–0.05 | 0.02–0.05 | 0.02–0.05 |
| waking the worker | < 0.05 | < 0.05 | < 0.05 |

So a frame costs about 0.15 / 0.4 / 0.5 ms more end to end, and the page's
`putImageData` is unchanged. A `SharedArrayBuffer` copy runs at about 17 GB/s
here against 50 GB/s for a plain one.

**Input latency.** From the keydown handler to the `putImageData` of the
first frame that consumed the key, live walk, `wasm_uncapped` on so every
refresh is a frame, ~390 key presses at random moments (median / p95, ms):

| | 640×400 old | new | 960×600 old | new |
|---|---|---|---|---|
| uncapped rAF | 1.47 / 1.85 | 1.89 / 2.51 | 2.61 / 3.42 | 3.44 / 4.18 |
| 60 Hz rAF | 9.96 / 16.59 | 11.12 / 16.86 | 12.27 / 16.96 | 13.20 / 17.50 |

Between +0.4 and +1.2 ms at the median (the 60 Hz medians carry about
±0.5 ms of noise from where each press falls in the refresh), which is the
hand-off above, stretched a little by the longer frame period.

**Startup and size.** Local server: navigation to the first frame 155–215 ms
old, 148–197 ms new. The download is `quake.wasm` 1.05 MB (0.35 MB gzip) plus
`id1/pak0.pak` 17.8 MB, against an 18.9 MB wasm (8.5 MB gzip) that embedded
the pak. The renderer process's memory (PSS, attract demo running) was
155–157 MB old and 157–162 MB new: the worker's own heap and the pak held in
it, where the old page held the pak twice (the module's bytes and its linear
memory). "Files" above has the pak's own comparison.

**Presentation.** At the port, frames were RGB and the page kept the 2-D
canvas: for RGBA, headless Chromium's software GL took 0.58 ms to upload and
draw a 960×600 frame against 0.23 ms for copy plus `putImageData`. The
renderer has since gone 8-bit (PERF_PLAN B5) and the page presents through
WebGL2 on a GPU: "Presentation" above has the design and its measurements.

**Verdict.** Equal frames, equal host frame time and startup, similar memory,
and a sub-millisecond hand-off per frame that the 8-bit framebuffer would cut
by three quarters. Not clearly worse, so everything was ported.

## `?lowlatency`

Kept, same meaning: it asks for a `desynchronized` canvas (WebGL2's or the
2-D one), which can skip a compositor frame where the browser supports it
(Chrome on Windows and ChromeOS), at the risk of tearing. The frame still
arrives inside the refresh that ticked, so the hint matters exactly as much
as before. Off by default, and not verifiable headless: there it holds the
refresh near 60 Hz, so input to present measured 12–16 ms at the median
with it (old page and new, WebGL2 and 2-D alike) against 2–3 ms without. It
stays off in 2026 too: it would save up to a refresh (2 ms at 480 Hz, 17 ms
at 60) only where the browser supports it, and risks tearing there — an
unverifiable change to every frame's look is not one to make by default. A
player who wants it opens the page as `?lowlatency`.

## Touch

On a touch screen (a coarse primary pointer; `?touch` forces it on a
desktop) the page loads `web/touch.js` and hands it a few entry points
(`startTouch` in index.html: the KEY and MOUSE records, `callLine`, the
State, the audio unlock); a desktop never loads it. It switches the page to
a touch layout — the picture fills the screen, under a phone's notch too
(`viewport-fit=cover`), and the controls keep to the safe area — and shows
what the game's State calls for:

| state | on screen |
|---|---|
| the live game, `in_touch` on (2026) | a stick wherever the left thumb lands (the left 45%); look by dragging anywhere else; FIRE (hold; dragging it aims too), JUMP, WEAPON (`impulse 10`, the next weapon owned), MENU |
| the live game, `in_touch` off (Classic) | MENU only: id's game has no touch controls, but a phone must never be left without a way back to the menu |
| a demo (the attract loop) | MENU; a tap anywhere is Escape, as any key is during id's demo playback |
| the menu | taps on the menu itself; a pad (▲▼◀▶, OK), keys like a keyboard's, held arrows repeating; BACK (Escape); YES / NO when it asks (STATE 256; the pad hides then, and while Customize controls waits for a key to bind); GAME beside BACK, with a mission pack on offer ("The game picker") |
| the console (Options > Go to console) | KEYBOARD (a tap on the console too), TAB, ▲ (the previous line); BACK closes it; a drag scrolls (PgUp/PgDn) |

**Clear of the status bar.** FIRE, JUMP, WEAPON and the stick's resting
hint (the dashed circle) sit above the HUD's numbers and icons, at any
phone size, Screen size (viewsize) and resolution — the thing a touch
control must never cover is the one piece of the page the program itself
draws, not the page. The program reports it: `sbar_height` (an automation
call, `quake_rs::screen::status_bar_rows`) answers the framebuffer rows,
bottom-anchored, the status bar covers *this frame* — the same arithmetic
`calc_refdef` uses to keep the 3-D view off the bar, so it is exactly right
for every `viewsize` (0, 24 or 48 virtual rows), the "scaled 2-D" extra's
whole-number blow-up, and an intermission (always full screen, so 0). The
page turns that into a CSS custom property, `--bar` (`touch.js`'s
`refreshBar`/`applyBar`): the frame rows at the canvas box's own CSS-pixel-
per-frame-pixel ratio, re-read whenever that ratio or the bar might have
changed — a resize, a rotation, fullscreen, and leaving the menu or console
(where Screen size and the resolution are set). The four controls' CSS
`bottom` is then `max(their own fixed default, --bar + a 6px breath + their
usual gap above the lowest of them, JUMP)`, so raising `--bar` lifts the
whole cluster together without disturbing its layout — a visible gap above
the bar rather than the pixel-exact edge, which a phone's devicePixelRatio
can round a hair short of on the actual screen — and with no status bar
(Screen size 120) `--bar` is 0 and they sit exactly where they always did.
The live stick itself still appears wherever the thumb lands (its whole
point), so a finger placed directly on the bar can still summon it there;
only the controls with a fixed position are kept off it.

**What a finger sends.** The stick is the client's analog move
(`set_move fwd side`: `in_fwd`/`in_side`, full speed at 56 CSS px of
thumb, a 12% dead zone, at most once a display frame); FIRE and JUMP hold
`set_attack`/`set_jump`; WEAPON is `set_impulse 10`. These bypass the key
bindings on purpose: a touch button is its action, whatever the player
bound. Look is the MOUSE record, IN_MouseMove's input, 2 counts per CSS
pixel: 0.32° a pixel at the default Mouse Speed, so Options > Mouse Speed
and Invert Mouse apply, and `freelook` (on in 2026) is what makes a
vertical drag pitch. `in_touchaccel` (console, 0..4, default 0) turns a
fast drag up to 1 + that many times as far (full at 2 px/ms). Escape,
Tab, y/n and the typed characters are KEY records through `Key_Event`.

**The menu by tapping.** The page does not guess items from pixels: it
maps the finger to a frame pixel through the canvas's box and asks the
program (`menu_tap x y`, and `menu_point x y` while a finger drags), and
the engine's menu (`Menu::tap`, quake-rs `menu.rs`, "Taps") finds the row
from the same constants its `draw_*` functions draw with and answers with
the key a player would press: Enter on a picture list's item (Main,
Single Player, Multiplayer: 20-line items, a fingertip) at once; on a text
list (8-line rows: 10–13 CSS px on a phone) a first tap moves the cursor
and a second on the highlighted row acts — Enter, or left/right of an
Options slider's knob; Help pages by halves. A drag moves the cursor with
the finger without acting, which is the easy way onto a small row.

**The menu pad.** A text list's 8-line rows (10–13 CSS px on a phone, a
third of a fingertip) are a fight to tap, and a slider's ten notches each
take one; so while the menu is up the touch layer also shows ▲▼◀▶ and OK,
off to the right of the menu's own centred 320-wide layout — at every size
this was checked at (`web/verify_touch.py`'s `PHONE`, `PHONE_26`, and its
748×360 case) that right margin is well past what the pad needs; a much
narrower window has not been checked and could start to crowd it. Each
sends the key id's own menu already reads (`Menu::keydown`, quake-rs
`menu.rs`) exactly as BACK sends Escape — the engine drives as a keyboard
always drove it, nothing menu-pad-specific added there. Holding an arrow
repeats it (touch.js's own timer, a keyboard's autorepeat: once at ~350 ms,
then ~12 a second); each repeat is a fresh key down *and* up, because
`Key_Event` ignores a key held down without an up between but for
Backspace and Pause (`input.rs`'s `key_repeats`). OK is Enter, sent on
release like BACK (so sliding off first cancels it); the arrows act on
touch, so the first step and the first repeat both land without waiting
for a release. Hidden where a pad key would be wrong: while the menu asks
y or n (STATE 256) and while Customize controls waits for a key to bind
(STATE 8, `BIND_GRAB`) — every other mode, and Classic too (a phone still
has no keys). Help pages already take ◀▶ (id's `M_Help_Key`); the pad's
presses reach them the same way. Taps and drags on the menu are unchanged;
the pad is in addition.

**The phone's keyboard.** KEYBOARD focuses a hidden text field (in the
tap's own handler, the only way iOS shows its keyboard); what the field
receives becomes KEY records (printable ASCII as its keynum with the typed
character; Enter; a deleted zero-width sentinel is Backspace; Android's
composed words when they end), and its key events never reach the page's
own keyboard handler. Multiplayer > Setup's name rows get the same button.

**Around the controls.** Held upright, a prompt asks for landscape (a tap
dismisses it). The first tap ("tap to start") also asks for fullscreen and
`screen.orientation.lock('landscape')` where the browser has them
(Android; a fullscreen button stays while not fullscreen). During a game a
Screen Wake Lock keeps the display on. Every touch resumes audio if the
browser suspended it (iOS "interrupts" it in the background). When the
page is hidden the audio is suspended, and with the touch controls on (not
in Classic, where the game only stops getting ticks, as on a desktop) a
live game pauses (`pause`, id's plaque; STATE 512) under its menu; back in
the game — the menu closed, by the player — the pause ends. Haptics:
`QuakeTouch.rumble(weak, strong, ms)` takes the Gamepad API's dual-rumble
magnitudes and buzzes `navigator.vibrate` (Android; iOS Safari has none)
for longer the stronger it is; nothing calls it yet — it is the hook for
the `input` agent's gamepad rumble events (damage, heavy weapons).

**Phones.** On an iPhone in landscape (844×390 CSS px, devicePixelRatio 3,
so a 2532×1170 box) Auto picks a pixel size of 2 with the single-threaded
build: a 1266×585 frame, 2×2 device pixels a picture pixel (0.67 CSS px,
finer than the eye resolves at arm's length), and the scaled 2-D layer at
2×. What that costs, measured on a desktop, not on a phone
(`bench.py --video modern`, one thread, headless Chromium on an 8-core desktop CPU
under load 6, median page ms per frame, demo1 / walk_e1m1): 1266×585
4.4 / 4.7, a 2400×1080 Android at 2.6 (1200×540) 3.8 / 4.2, and the same
iPhone at a pixel size of 1 (2532×1170) 16.0 / 17.8. A recent iPhone's
fast core is about this one's by the published single-thread scores (a
mid-range Android's about 0.4 of it), so the chosen size should fit
Safari's 60 Hz on one core with room to spare, and a mid-range phone
should manage 60 Hz — estimates, not runs. Decided: no phone rule in Auto.
The single-threaded build, the default deploy, gives a phone the 2×2
pixel above; the threads build offers `hardwareConcurrency` threads, and
Auto's budget grows with them (4 and up: twice the pixels), so a 6-core
phone would get the 1×1 picture, 3.4× the pixels, drawn in equal row
bands on unequal cores (a phone's efficiency cores take ~3× as long, and
every band waits for the slowest): hotter and not smoother. For a phone,
deploy the single-threaded build, or set `vid_pixelsize 2`. (Open: a
phone-aware thread offer in wasi.js — the `present`/`platform` side.)
iOS Safari: `SharedArrayBuffer` needs iOS 15.2 and https (the page says so
when it is missing); rAF runs at 60 Hz (Safari's default even on 120 Hz
screens), 30 Hz in Low Power Mode; Web Audio follows the silent switch;
the Screen Wake Lock needs iOS 16.4 (18.4 in a home-screen app). Memory:
the single-threaded build's memory grows as the game needs it (the pak
stays outside it, "Files"); the threads build declares a shared memory of
up to 1 GiB (16384 pages), which a browser reserves up front for a
shared memory — the kind of reservation iOS has refused in other wasm
games, one more reason to give a phone the single-threaded build.

## Offline and install

**Install.** `manifest.webmanifest` (`display: fullscreen`, landscape,
standalone as the fallback) and iOS's `apple-mobile-web-app-*` tags make
the page an app on the home screen: on iOS that is the only fullscreen a
page gets, and it is how an iPhone should run it. The icons are
original pixel art (`web/icons/make_icons.py`, standard library only, a
torch flame on stone; not id's logo or any of its art), their subject
inside the middle 80% circle so launchers may mask them; the page's
favicon is the 32-pixel one, inlined.

**The service worker** (`web/sw.js`, one file: loaded by the page it
registers itself). What it answers:

- **the game data, cache first**: a pak (`id1/*.pak`) or a CD track
  (`id1/music/*`) from the cache once kept, else the network, kept as it
  streams to the page. id's own data never changes once deployed — pak0,
  and a server's own pak1 and tracks alike ("A server's own files"); bumping
  `DATA_CACHE` refetches it all.
- **everything else, network first**: the page, `wasi.js`, `touch.js`,
  `quake.wasm`, the app manifest, the icons, and a server's own `files.json`
  come from the network and are kept; when the network fails, from what was
  kept. So online a player always runs what is deployed — an update takes
  effect at the next load, with nothing to version or bump — and offline,
  what they last played (a deploy that starts offering pak1 only reaches
  that offline copy once a player has been online since). The page's small
  files are kept at install, because the first visit's page loaded before
  the worker existed.
- **nothing marked `no-store`**, which `isolated.py` sends: the checks
  leave no 19 MB copies in the browser profiles (verify_touch.py serves
  without it to check offline play).

The worker takes over at once (`skipWaiting`, `clients.claim`): with the
page's files from the network there is no old cache to keep an open page
consistent with. The page waits for it before downloading (`main` awaits
`window.quakeServiceWorker`, at most 3 s, once: after that the page is
already controlled), so the first visit's downloads go through it and are
kept — offline works after one visit — at the cost of the worker's
install on that first visit. A reload finds the pak locally: no 18 MB
download, the "fast reloads". Measured on a local server (the same page
with and without `sw.js`, three fresh profiles each, navigation to the
first frame): the first visit 263–283 ms against 180–246 ms; a reload's
downloads 74–92 ms against 47–75 ms from the HTTP cache. So on a local
network the worker costs a few tens of milliseconds; its point is a
phone's network, where the pak is 18 MB and the HTTP cache may not keep
it, and no network at all. Trade-off accepted: on a network
that hangs rather than fails, network-first waits for it; and a load that
lost the network half-way could pair a new page with a kept older engine.

**Isolation on any host** (the coi-serviceworker technique). Every answer
the worker gives carries COOP `same-origin`, COEP `require-corp` and CORP
`same-origin`. A server that sends the headers loses nothing. On one that
cannot (GitHub Pages, a plain static host), the first load is not
isolated; the page registers the worker, waits for it and reloads once
(a sessionStorage flag stops a loop where a browser ignores the headers),
and the reloaded page is isolated. Costs: the first visit loads twice; it
needs service workers (not Firefox's private windows); a page on http
other than localhost gets neither; and a hard reload, which bypasses the
worker, is not isolated, so it reloads once more (not verified: headless
Chromium's cache-ignoring reload bypasses the worker for that second load
too). Checked in headless Chromium (verify_touch.py, 8.); Safari and iOS
honour worker-supplied isolation headers by their documentation, not by a
run here.

## Browser support

The design needs cross-origin isolation (below) for `SharedArrayBuffer`, and
`Atomics.wait` in a worker. Checked here: headless Chromium (the nine
checks, `verify_present.py`, `verify_threads.py` and the benchmark) and
headless Firefox 155 (the nine checks and `verify_present.py` with
`QUAKE_BROWSER=firefox`, `verify_extras.py` skipping its Keyboard Lock half,
which Firefox has no API for; `verify_threads.py`); each on both builds.
WebGL2 is optional: without it the page presents through the 2-D canvas.
Playwright's WebKit would not start here (missing system
libraries). A GPU is checked through headless Chromium (`QUAKE_GPU=1`: the
local GPU, ANGLE on GL; `verify_present.py` and the benchmark). Not
checked: Safari, iOS, Firefox's WebGL2 (its headless build has none; its
refusal of shared views is emulated, `verify_present.py`'s staging copy), a
GPU driving a real high-refresh display. From the platforms' documentation, not from a
run: Safari has `SharedArrayBuffer` under COOP/COEP since 15.2 (iOS 15.2),
with `Atomics.wait` in workers; iOS has no pointer lock, and a phone plays
with the touch controls ("Touch"). The program's own memory no longer
holds the 18 MB pak, which helps where wasm memory is tight (iOS).

## Build, serve, deploy

**Build:**

```sh
cd quake-wasm && cargo build --release --target wasm32-wasip1          # draws on one thread
cd quake-wasm && cargo build --release --target wasm32-wasip1-threads  # the renderer's threads ("Threads")
```

Either `quake.wasm` runs in the same page; the threads build's is under
`target/wasm32-wasip1-threads/release/`.

**A deploy dir** holds the page's files (`isolated.PAGE_FILES`), the engine
and the pak:

```
deploy/index.html          web/index.html
deploy/wasi.js             web/wasi.js
deploy/touch.js            web/touch.js           (touch screens only)
deploy/endscreen.js        web/endscreen.js       (id's end screen, "Quit")
deploy/sw.js               web/sw.js              (offline, isolation anywhere)
deploy/manifest.webmanifest  web/manifest.webmanifest
deploy/icons/*.png         web/icons/icon-192.png, icon-512.png, apple-touch-icon.png
deploy/quake.wasm          quake-wasm/target/wasm32-wasip1/release/quake.wasm
deploy/id1/pak0.pak        quake-data/ID1/PAK0.PAK (lower-case name)
deploy/id1/pak1.pak        your own (registered) pak — optional
deploy/id1/music/track02.ogg…   your own CD rip — optional
deploy/files.json          isolated.write_manifest(dir) — only if either optional row is there
```

```sh
mkdir -p deploy/id1 deploy/icons
cp web/index.html web/wasi.js web/touch.js web/endscreen.js web/sw.js web/manifest.webmanifest deploy/
cp web/icons/icon-192.png web/icons/icon-512.png web/icons/apple-touch-icon.png deploy/icons/
cp quake-wasm/target/wasm32-wasip1/release/quake.wasm deploy/
cp quake-data/ID1/PAK0.PAK deploy/id1/pak0.pak
```

(`isolated.copy_page(dir)` copies the page's files; `bench.py --build` uses
it.) A deploy without `sw.js` still plays, but the page's `<script>` for it
answers 404, which the checks count as a console error.

**Offering the registered game and its music** (your own files — none of
this is in the repo) is the same deploy dir plus two more rows and a
generated manifest ("A server's own files"):

```sh
cp your-pak1.pak deploy/id1/pak1.pak
mkdir -p deploy/id1/music && cp your-tracks/track*.ogg deploy/id1/music/
(cd web && uv run python -c "import isolated; isolated.write_manifest('$D')")
```

Leave both rows (and `files.json`) out for a pak0-only deploy; the page then
makes no further request than it already did.

**Serve** with the two cross-origin isolation headers (without them, and
without the service worker, the page says so and stops):

```sh
miniserve -C -p 8080 --index index.html \
  --header "Cross-Origin-Opener-Policy:same-origin" \
  --header "Cross-Origin-Embedder-Policy:require-corp" deploy
```

Any static server works if it sends those headers, and one that cannot
works through the service worker, after one reload ("Offline and install").
Either way the page must be a secure context — https, or localhost — for
`SharedArrayBuffer` and service workers alike. A phone reaching the server
by name over plain http gets neither, so serve it over https: any reverse
proxy or tunnel that terminates TLS in front of the server works.
`web/isolated.py` is the checks' server. `uv run --with playwright
web/bench.py DEPLOYDIR` and `QUAKE_VERIFY_PORT=… uv run --with playwright
web/verify_walk.py DEPLOYDIR` take a deploy dir, and `bench.py --build`
assembles one under `quake-wasm/target/bench-web`.

**Natively**, the same program runs on a pipe:
`cargo run --release -- -basedir <dir with id1/pak0.pak>` reads the records
on stdin and writes them on stdout (its tests drive it that way,
`sys.rs`).
