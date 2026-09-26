# The browser platform

The browser build is an ordinary Rust program. `quake-wasm` builds a binary,
`quake.wasm` (`wasm32-wasip1`), with a `fn main()` that reads the page's
events from stdin, writes each frame's picture and sounds to stdout, and
keeps its saves and `config.cfg` through `std::fs`. It has no exports beyond
WASI's `_start`, no imports beyond what `std` asks of WASI, and
`#![forbid(unsafe_code)]`. The page is a small WASI host around it.

There are three pieces:

| file | runs in | what it does |
|---|---|---|
| `quake-wasm/` → `quake.wasm` | a Web Worker | the game: `sys::run`, a host frame per tick (`quake-wasm/src/sys.rs`); the records it reads and writes (`proto.rs`) |
| `web/wasi.js` | the same Worker | the WASI host: stdin from a shared ring, stdout into shared frame slots and one message per turn, an in-memory file system, the clocks |
| `web/index.html` | the page | the canvas, keyboard and mouse, Web Audio, IndexedDB, and the display's refresh, which it hands the program as ticks |

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
  TICK(seq, dt) ───────────────────▶ ring ─▶ fd_read(0) returns the tick
  spin on ACK ≥ seq (≤ 30 ms)                host::step(dt): Host_FilterTime, the
                                             client frame, menu, console, blend, pack
                                             fd_write(1): FRAME ─▶ pixels copied into a
                                                                   free frame slot
                                                          sounds, LISTENER, STATE ─▶ kept
                                                          SYNC ─▶ ACK = seq, notify;
                                                                  post the kept records
  copy the newest slot into the
  canvas's ImageData, putImageData
message event: sounds → Web Audio,
  STATE → the page's UI, REPLY → calls
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
| 6 | AUDIO_READY | `ready u8` |
| 7 | CALL | `id u32`, then the UTF-8 line |
| 8 | END | — (written by the host, not the page: "nothing more queued") |
| 9 | WINDOW | `w u32`, `h u32`: the page's box for the picture in device pixels (its CSS size x `devicePixelRatio`; the whole screen in fullscreen), sent at start and on every resize |

A record whose payload is shorter than its kind's reads the missing fields as
zeros, and an unknown kind is skipped, so either side can grow a record.

**Out** (program → page, stdout): `[kind u8][0 u8 ×3][len u32]`, then `len` bytes.

| kind | record | payload |
|---|---|---|
| 1 | FRAME | `w u16`, `h u16`, `format u8` (0 = RGBA8), `0 ×3`, the pixels |
| 2 | SYNC | `seq u32` (last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
| 3 | STATE | `flags u32` (1 menu, 2 console has the keyboard, 4 live game, 8 binding a key, 16 timedemo, 32 native resolution, 64 F toggles fullscreen, 128 touch controls (`in_touch`), 256 the menu asks y or n, 512 the live game is paused), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel) |
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

A turn's records end with its `SYNC`. After a frame the sound records come in
causal order: `GENERATION` (with the new level's `AMBIENT`s) first, then
`LISTENER`, the one-shots, stops, local sounds, and the level's placed loops,
so every sound is spatialized against this frame's listener and nothing
queued after a level change is stopped by it.

## Shared memory

Two `SharedArrayBuffer`s:

- **Control and ring** (`CTL_BYTES` 256 + 64 KB, made by the page). The
  control block is an `Int32Array`: `IN_WRITE`/`IN_READ` (the ring's byte
  counters; the page writes whole records and then moves `IN_WRITE`, so the
  program never sees half a record), `ACK` (the last tick the program
  consumed), `SYNCS` (turns so far), `LATEST`/`FRAMES`/`READING`/`SHOWN`/
  `SLOTS_GEN` (the frame slots), `RUN` (starting, running, exited,
  crashed), `WAIT` (whether the program waits for ticks), and each slot's
  width, height and format. The same table is at the top of `wasi.js` and of
  the page's script.
- **Frame slots**: three, made by the worker as large as the largest frame
  so far. The worker writes a frame into a slot that is neither the newest
  (`LATEST`) nor the one the page is reading (`READING`), then publishes it;
  the page claims `LATEST` in `READING` (re-checking it did not move) before
  copying out. A frame larger than the slots (the first one, or a higher
  resolution) gets a new set, which the worker sends the page and numbers in
  `SLOTS_GEN`; the page presents nothing until it holds the set `SLOTS_GEN`
  names, so the frame shows one refresh later. No resolution limit lives in
  the host, and at the default 960×600 the slots take 6.9 MB.

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
  copy of the archive lives in the program's memory. Measured against the
  same program with the pak embedded (`include_bytes!` and
  `Pak::from_static`, the old page's way), six loads each on a local server:
  navigation to the first frame 169–328 ms from the file, 167–330 ms
  embedded, the same; the renderer process's memory 157–176 MB from the
  file, 191–214 MB embedded (the embedded pak lives in the module's bytes and
  in the linear memory). Besides, an engine update no longer re-downloads
  18 MB, the build needs no game data, and a player's own `pak1.pak` can be
  one more file (a later change: the worker cannot take files once running,
  so it would be added before start, or the worker restarted).
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

## Settings, and how the page shows the picture

Every setting is the program's (`quake_rs::settings`: id's cvars and key
bindings, and the port's departures, which the profiles **Classic** and
**2026** switch). The page needs three of them, and hears them in the
`STATE` record:

- **Native resolution** (`vid_native`, 2026). The page sends its box for the
  picture in device pixels (`WINDOW`); the program renders the box divided by
  a whole pixel size (`vid_pixelsize`: 1..4, or Auto, the smallest that
  keeps the frame within a 1080p frame's pixels) and says the size in
  `pixel_size`; the page makes the canvas exactly `W x pixel_size` device
  pixels wide and `H x pixel_size` tall (`fitCanvas`), `image-rendering:
  pixelated`, so every picture pixel is a whole square of screen pixels at
  the box's own aspect (the view is Hor+: `fov_adapt`). Off (Classic), the
  picture is the video mode (`_vid_resolution`, Options > Video Options)
  in the largest 4:3 box the window fits, as before.
- **F toggles fullscreen** (`vid_fkey`, 2026; id's `default.cfg` leaves F
  unbound).
- **The profile from the address.** `?classic` and `?2026` add `+profile
  classic` / `+profile 2026` to the program's command line (`wasi.js` hands
  it `args`), which quake.rc's `stuffcmds` runs after `config.cfg`: the same
  switch as the menu's, so it sticks.

`verify_settings.py` checks all of it in the browser (the window filled
with whole pixels at devicePixelRatio 1 and 2, `?classic`, the switch, the
reload); the checks that pin id's behaviour open the page as `?classic`,
and `bench.py` does too, so its frames hash as `quaketool play`'s.

## Sound

The program decides what plays (`snd_dma.rs`, `quake_rs::snd`: channel
choice and override, the loop windows, the ambient ramps); the page mixes it
with Web Audio as before, one source per sound, the same pan law, the same
per-frame re-spatialization. What changed is only the channel: the page used
to poll ~30 exports each frame; now it handles the records above as they
come, and samples cross once. The page remembers the current level's loops,
so audio that starts later (the first click) still gets them.

**Engine PCM later.** A later change will mix in the program (id's
`snd_mix.c`) and feed an AudioWorklet. The protocol has room for it: a record
of PCM frames per turn, or — better, because the worklet runs on its own
clock — a third `SharedArrayBuffer` ring the worker's `fd_write` of a
dedicated file descriptor (or a record kind) fills and the worklet drains.
Either way the program stays a writer of bytes; nothing in this design
assumes the page mixes.

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
thread's. With the threads build's shared memory the page could read the
frame straight out of the program's memory. The frames are the same at
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
| sounds, listener, state, `postMessage` | 0.02–0.05 | 0.02–0.05 | 0.02–0.05 |
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

**Presentation.** The brief's idea — an 8-bit frame plus its palette, with
the GPU doing the VGA DAC in WebGL2 — needs the renderer to write palette
indices, and it composes RGB today (PERF_PLAN B5), so frames are RGBA. For
RGBA, WebGL2 measured worse where it could be measured: Chromium accepts a
shared view in `texSubImage2D` (so the page's copy could go) and draws it
byte-exact with `texelFetch`, but headless Chromium's software GL took
0.58 ms to upload and draw a 960×600 frame against 0.23 ms for copy plus
`putImageData`, and headless Firefox has no WebGL2 at all. The page keeps the
2-D canvas. With B5 the case changes: a quarter of the bytes through both
copies and no pack in the program (0.47 ms at 1280×800), which would make
this design cheaper per frame than the old one; the `FRAME` record's
`format` byte is there for it.

**Verdict.** Equal frames, equal host frame time and startup, similar memory,
and a sub-millisecond hand-off per frame that the 8-bit framebuffer would cut
by three quarters. Not clearly worse, so everything was ported.

## `?lowlatency`

Kept, same meaning: it asks for a `desynchronized` 2-D canvas, which can skip
a compositor frame where the browser supports it (Chrome on Windows and
ChromeOS), at the risk of tearing. The frame still arrives inside the
refresh that ticked, so the hint matters exactly as much as before. Off by
default, and not verifiable headless.

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
| the menu | taps on the menu itself; BACK (Escape); YES / NO when it asks (STATE 256) |
| the console (Options > Go to console) | KEYBOARD (a tap on the console too), TAB, ▲ (the previous line); BACK closes it; a drag scrolls (PgUp/PgDn) |

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

- **the pak, cache first**: `id1/*.pak` from the cache once kept, else the
  network, kept as it streams to the page. id's shareware data never
  changes; bumping `DATA_CACHE` refetches it.
- **everything else, network first**: the page, `wasi.js`, `touch.js`,
  `quake.wasm`, the manifest and icons come from the network and are kept;
  when the network fails, from what was kept. So online a player always
  runs what is deployed — an update takes effect at the next load, with
  nothing to version or bump — and offline, what they last played. The
  page's small files are kept at install, because the first visit's page
  loaded before the worker existed.
- **nothing marked `no-store`**, which `isolated.py` sends: the checks
  leave no 19 MB copies in the browser profiles (verify_touch.py serves
  without it to check offline play).

The worker takes over at once (`skipWaiting`, `clients.claim`): with the
page's files from the network there is no old cache to keep an open page
consistent with. The page waits for it before downloading (`main` awaits
`window.quakeServiceWorker`, at most 3 s, once: after that the page is
already controlled), so the first visit's downloads go through it and are
kept — offline works after one visit — at the cost of the worker's
install on that first visit (0.2 s here). A reload finds the pak locally:
no 18 MB download, the "fast reloads". Trade-off accepted: on a network
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
checks, `verify_threads.py` and the benchmark) and headless Firefox 155 (the
nine checks with `QUAKE_BROWSER=firefox`, `verify_extras.py` skipping its
Keyboard Lock half, which Firefox has no API for; `verify_threads.py`).
Playwright's WebKit would not start here (missing system
libraries). Not checked: Safari, iOS, a real GPU, a real high-refresh
display. From the platforms' documentation, not from a
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
deploy/sw.js               web/sw.js              (offline, isolation anywhere)
deploy/manifest.webmanifest  web/manifest.webmanifest
deploy/icons/*.png         web/icons/icon-192.png, icon-512.png, apple-touch-icon.png
deploy/quake.wasm          quake-wasm/target/wasm32-wasip1/release/quake.wasm
deploy/id1/pak0.pak        quake-data/ID1/PAK0.PAK (lower-case name)
```

```sh
mkdir -p deploy/id1 deploy/icons
cp web/index.html web/wasi.js web/touch.js web/sw.js web/manifest.webmanifest deploy/
cp web/icons/icon-192.png web/icons/icon-512.png web/icons/apple-touch-icon.png deploy/icons/
cp quake-wasm/target/wasm32-wasip1/release/quake.wasm deploy/
cp quake-data/ID1/PAK0.PAK deploy/id1/pak0.pak
```

(`isolated.copy_page(dir)` copies the page's files; `bench.py --build` uses
it.) A deploy without `sw.js` still plays, but the page's `<script>` for it
answers 404, which the checks count as a console error.

**Serve** with the two cross-origin isolation headers (without them, and
without the service worker, the page says so and stops):

```sh
miniserve -C -p 8196 \
  --header "Cross-Origin-Opener-Policy:same-origin" \
  --header "Cross-Origin-Embedder-Policy:require-corp" deploy
```

Any static server works if it sends those headers, and one that cannot
works through the service worker, after one reload ("Offline and install").
Either way the page must be a secure context — https, or localhost — for
`SharedArrayBuffer` and service workers alike: a phone reaching the server
by name over plain http (`http://<host>:8196`) gets neither, so serve
it over https (an https reverse proxy, for one, gives the server an https name).
`web/isolated.py` is the
checks' server; `uv run web/bench.py DEPLOYDIR` and
`QUAKE_VERIFY_PORT=… uv run --with playwright web/verify_walk.py DEPLOYDIR`
take a deploy dir, and `bench.py --build` assembles one under
`quake-wasm/target/bench-web`.

**Natively**, the same program runs on a pipe:
`cargo run --release -- -basedir <dir with id1/pak0.pak>` reads the records
on stdin and writes them on stdout (its tests drive it that way,
`sys.rs`).
