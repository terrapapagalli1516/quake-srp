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
| `web/wasi.js` | the same Worker | the WASI host: stdin from a shared ring, stdout into shared frame slots and one message per turn, an in-memory file system, the clocks |
| `web/index.html` | the page | the canvas, keyboard and mouse, the audio device (an AudioWorklet playing the program's samples), IndexedDB, and the display's refresh, which it hands the program as ticks |

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
  AUDIO_CLOCK, TICK(seq, dt) ──────▶ ring ─▶ fd_read(0) returns the tick
  spin on ACK ≥ seq (≤ 30 ms)                host::step(dt): Host_FilterTime, the
                                             client frame, menu, console, blend, pack;
                                             id's mixer paints to the clock + mix-ahead
                                             fd_write(1): PCM ─▶ samples copied into the
                                                                 sound ring
                                                          FRAME ─▶ pixels copied into a
                                                                   free frame slot
                                                          AUDIO, STATE ─▶ kept
                                                          SYNC ─▶ ACK = seq, notify;
                                                                  post the kept records
  copy the newest slot into the
  canvas's ImageData, putImageData
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
| 9 | WINDOW | `w u32`, `h u32`: the page's box for the picture in device pixels (its CSS size x `devicePixelRatio`; the whole screen in fullscreen), sent at start and on every resize |
| 10 | AUDIO_CLOCK | `pos u32`: the sound ring's play position, in sample pairs (wrapping); sent before every TICK |
| 11 | AUDIO_WAKE | `pos u32`: the same, written by the host between ticks while the worklet plays: "mix now" |

A record whose payload is shorter than its kind's reads the missing fields as
zeros, and an unknown kind is skipped, so either side can grow a record.

**Out** (program → page, stdout): `[kind u8][0 u8 ×3][len u32]`, then `len` bytes.

| kind | record | payload |
|---|---|---|
| 1 | FRAME | `w u16`, `h u16`, `format u8` (0 = RGBA8), `0 ×3`, the pixels |
| 2 | SYNC | `seq u32` (last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
| 3 | STATE | `flags u32` (1 menu, 2 console has the keyboard, 4 live game, 8 binding a key, 16 timedemo, 32 native resolution, 64 F toggles fullscreen), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel) |
| 4–11 | — | retired: the sound records of the page's own mixing, before the program mixed |
| 12 | REPLY | `id u32`, `value f64`, then UTF-8 text |
| 13 | BENCH | `f64` per value (`--features bench`; names from the `bench_names` call) |
| 14 | PCM | `start u32` (the pair of the ring's clock it plays at), `rate u32`, `flags u32` (1: silence the ring first, `S_ClearBuffer`), then 16-bit stereo pairs. Copied into the sound ring by `wasi.js`, never posted |
| 15 | AUDIO | `rate u32`, `mode u32` (0 Classic, 1 2026), then counts: `starts`, `local`, `stops`, `clears`, `painted` (u32 each) |

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
- **The sound ring** (64 bytes + 16384 stereo pairs of 16 bits, made by the
  page, handed to the worker and to the AudioWorklet). Its control block is
  an `Int32Array`: `POS` (the pair the device plays next — the clock the
  program mixes ahead of), `WRITE` (where the program's samples reach),
  `RATE` (their rate), `UNDER` (quanta the worklet played short once the
  program had written anything), `PLAYED`, `CLEARS`, `PEAK` (the loudest
  sample played since the page last reset it), `QUANTA`. Samples go in at
  `start & 16383`; the worklet plays pair `POS` when `0 < WRITE - POS <=
  16384`, silence otherwise. The same table is at the top of `wasi.js` and
  in the page's sound section.

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
with `Atomics.wait` in workers; iOS has no pointer lock, and the page already
says it needs a keyboard and a mouse. The program's own memory no longer
holds the 18 MB pak, which helps where wasm memory is tight (iOS).

## Build, serve, deploy

**Build:**

```sh
cd quake-wasm && cargo build --release --target wasm32-wasip1          # draws on one thread
cd quake-wasm && cargo build --release --target wasm32-wasip1-threads  # the renderer's threads ("Threads")
```

Either `quake.wasm` runs in the same page; the threads build's is under
`target/wasm32-wasip1-threads/release/`.

**A deploy dir** holds four things:

```
deploy/index.html          web/index.html
deploy/wasi.js             web/wasi.js
deploy/quake.wasm          quake-wasm/target/wasm32-wasip1/release/quake.wasm
deploy/id1/pak0.pak        quake-data/ID1/PAK0.PAK (lower-case name)
```

```sh
mkdir -p deploy/id1
cp web/index.html web/wasi.js deploy/
cp quake-wasm/target/wasm32-wasip1/release/quake.wasm deploy/
cp quake-data/ID1/PAK0.PAK deploy/id1/pak0.pak
```

**Serve** with the two cross-origin isolation headers (without them the page
says so and stops):

```sh
miniserve -C -p 8196 \
  --header "Cross-Origin-Opener-Policy:same-origin" \
  --header "Cross-Origin-Embedder-Policy:require-corp" deploy
```

Any static server works if it sends those headers. `web/isolated.py` is the
checks' server; `uv run web/bench.py DEPLOYDIR` and
`QUAKE_VERIFY_PORT=… uv run --with playwright web/verify_walk.py DEPLOYDIR`
take a deploy dir, and `bench.py --build` assembles one under
`quake-wasm/target/bench-web`.

**Natively**, the same program runs on a pipe:
`cargo run --release -- -basedir <dir with id1/pak0.pak>` reads the records
on stdin and writes them on stdout (its tests drive it that way,
`sys.rs`).
