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
| `web/wasi.js` | the same Worker | the WASI host: stdin from a shared ring, stdout into shared frame slots (or, the threads build, the frames left where they lie in the program's shared memory) and one message per turn, an in-memory file system, the clocks |
| `web/index.html` | the page | the canvas and the display's DAC (WebGL2, else a 2-D canvas), keyboard and mouse, Web Audio, IndexedDB, and the display's refresh, which it hands the program as ticks |

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
                                             client frame, menu, console: an 8-bit
                                             frame and its palette (V_UpdatePalette)
                                             fd_write(1): FRAME ─▶ copied into a free
                                                                   frame slot, or
                                                          FRAME_AT ─▶ where it lies in
                                                                   shared memory
                                                          sounds, LISTENER, STATE ─▶ kept
                                                          SYNC ─▶ ACK = seq, notify;
                                                                  post the kept records
  the newest frame to the GPU (indices
  + palette, WebGL2) and drawn, or its
  RGBA through putImageData
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
| 9 | PRESENT | `format u8`: how the page shows frames from now on (0 RGBA8, 1 INDEXED8; RGBA8 until it says). The page sends it before the first tick |

A record whose payload is shorter than its kind's reads the missing fields as
zeros, and an unknown kind is skipped, so either side can grow a record.

**Out** (program → page, stdout): `[kind u8][0 u8 ×3][len u32]`, then `len` bytes.

| kind | record | payload |
|---|---|---|
| 1 | FRAME | `w u16`, `h u16`, `format u8` (0 RGBA8, 1 INDEXED8), `0 ×3`, then for INDEXED8 the palette (256 × RGBA), then the pixels (4 bytes each, or one palette index) |
| 2 | SYNC | `seq u32` (last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
| 3 | STATE | `flags u32` (1 menu, 2 console has the keyboard, 4 live game, 8 binding a key, 16 timedemo), `menu_screen i32` |
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
| 16 | FRAME_AT | `w u16`, `h u16`, `format u8`, `slot u8`, `0 u16`, `pixels u32`, `palette u32`: a frame left in the program's shared memory, ring slot `slot`, its pixels and palette at those addresses (`-sharedframes`) |

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
  video mode, `viewsize` and the Web extras (`config.rs`,
  `Host_WriteConfiguration`), written when one of them changes and exec'd at
  startup as quake.rc does. When a written file is closed, `wasi.js` sends it
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
with it (old page and new, WebGL2 and 2-D alike) against 2–3 ms without.

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
