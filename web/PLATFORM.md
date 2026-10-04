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
  a quick frame: spin on ACK ≥ seq           host::step(dt): Host_FilterTime,
  (≤ 30 ms); a slow one is not waited        IN_Commands (the pad's keys), the
  for: it is drawn at the refresh after      client frame (IN_JoyMove), menu,
  it is done, and the next TICK goes out     console: an 8-bit frame and its
  when it comes (Atomics.waitAsync)          palette (V_UpdatePalette); id's mixer
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
- **The page presents in the same refresh, when a frame is quick.** The
  refresh that posts a tick spins (the main thread may not `Atomics.wait`)
  until the program answers, then presents. That keeps the old page's
  timing, which computed the frame inside the refresh; only the hand-off
  below is added. One tick is in flight at a time; a refresh that
  finds the last one unanswered posts none (its time goes into the next
  tick's `dt`) and presents whatever has come. The spin is bounded by 30 ms
  (`WAIT_MS`), so a level load or a long automation call does not freeze
  the page.
- **A slow frame is not waited for.** The refresh then only draws what has
  come and asks for the next frame, and a frame that comes back after a
  refresh has gone by without it is followed by the next tick at that
  moment (`Atomics.waitAsync` on the Syncs), so the program draws back to
  back while it is behind (`index.html`'s `pacing`). What "slow" is depends
  on the device, because the wait's two costs do:
  - *To the screen.* From the tick to the display compositor's swap
    (`web/swap_trace.py`, below: a trace of headless Chromium on a desktop's
    GPU at 60 Hz, the frames held with `stall_ms`; median ms):

    | the frame, in refreshes | waited for | not waited for |
    |---|---|---|
    | 0.6 | 10.7–17.2 | the same (it is waited for everywhere) |
    | 0.85 | 17.0 | 17.9 |
    | 1.1 | 18.8 | 28.6 |
    | 1.4 | 23.9 | 33.2 |
    | 2.0 (33 ms: past the wait's 30) | 64.7 | 41.3 |

    Under a refresh it is the same swap either way (which swap a frame of
    0.6 makes is the display's own phase, run by run; at 0.85 the wait's
    frame made the earlier one in one run of six). Past a refresh the
    wait's frame is committed the moment it is done and swapped at once,
    and the other is drawn at the next refresh's callback: 10 ms later at
    60 Hz, a refresh later on the glass more often than not. Past the
    wait's limit it is the wait that loses, and badly: the refresh gives up
    at 30 ms, and the frame that arrives after is drawn only at the end of
    the *next* wait (which is also where the page's old latency probe
    credited a key to the frame before its own: a key to the draw call at
    2.0 refreshes is 81 ms waited for and 57–59 not, where that probe said
    47).
  - *To the game.* The wait is a spin: a core busy for the whole frame. On
    a desktop that is one core of many. On a phone it was the fastest core,
    with the game's threads on the others ("On an Android phone", below: at
    2640x1080, warm, 50 frames a second shown with the wait and 57 without
    at 60 Hz; 58 and 67 with a finger down).

  So there are two pairs of lines (`pacingLines`), each a moving average
  of the program's time to answer a tick with a frame, with a second line
  below the first to come back by:
  - *a touch screen* (the page's touch-screen test, `pacing.scarceCores`:
    the one place this is decided) stops waiting above 0.8 of a refresh and
    waits again below 0.6: a frame of most of a refresh misses its own
    refresh's swap anyway, and a longer one costs a refresh on the glass
    (8 ms at the 120 Hz a finger brings) for frames that are a fifth
    shorter and far fewer late ones;
  - *anywhere else* only a frame the wait would give up on is not waited
    for: above 0.9 of the limit (27 ms), back below 0.75 (22.5 ms). Up to
    there the page is the page it was.

  The display's period is the refreshes' shortest spacing lately. A
  browser without `Atomics.waitAsync` waits always; `?wait` in the address
  keeps the wait too, to compare by feel. `verify_pacing.py` checks both
  kinds of device, both ways and the switches, with frames made slow on
  purpose (`stall_ms`, a bench build).

  *The frame-rate cap* (`host_maxfps`, Picture and sound > Frame rate cap:
  60, id's 72, 120, 144, 240, none) holds the frames drawn, not the game,
  and is the program's, on top of all this: the page posts a tick every
  refresh whatever the cap, the program runs a host frame on every one —
  the game at the display's rate, the 60 to 480 Hz `quaketool framerate
  --check` proves — and draws its picture only on the first refresh at
  least 1/cap after the last picture (5% less, for a refresh's time a hair
  early; `client::host::FrameCap::picture_due`). A frame not drawn
  (`cl_main::walk_frame_undrawn`) does everything but the pixels — the
  server, the clocks, effects, particles, lights, fades, the view's kick
  and smoothing, the sound — and answers the tick with no frame, which the
  pacing does not count as a frame's time; it costs what `framerate
  --budget`'s "sim" column says (0.04 ms against 1–2 for a whole frame on
  one thread here), so the cap keeps its heat saving. So 60 on a 120 Hz
  panel draws every second refresh, evenly; on 110 Hz (a phone's page with
  a finger down) every second too, 55 a second; on 144 Hz every third, 48;
  a cap above the display's rate draws every refresh; and the game steps at
  the display's rate in every case. A tick the relaxed pacing posts between
  refreshes draws no sooner. 72 is id's own gate (`Host_FilterTime`,
  Classic's: the game's frames held with the pictures), none draws every
  frame. Slop starts at none on every machine (a touch screen too, since
  2026-10-04: the user's call). `quaketool
  framerate --cap 60 --check` runs every scenario's capped twin at 60, 72,
  90, 105, 110, 120, 144 and 240 Hz: each value is the uncapped one at its
  rate exactly. `verify_pacing.py` counts the drawn gaps and the game's
  host frames (`host_frames`) through the page's own tick at 120, 144 and
  110 Hz, and in the page's loop at those rates (its refreshes from a clock
  at that rate: a headless browser's are 60) on a touch screen, at 120
  waited for and relaxed.

  *Measure to the swap, not to the draw call.* The page's latency probe
  (`quake.latency`, `latency.py`) stops at its own draw call, and a draw
  call can be early without the picture being: `web/swap_trace.py DEPLOY
  [--touch] [--stalls ...]` (a bench build; `QUAKE_GPU=1` for WebGL2)
  marks each tick and each draw from outside the page (`performance.mark`
  around its `sendTick` and `present`), takes a Chromium trace of the same
  seconds, and follows every draw to the main frame that commits it
  (`ProxyMain::BeginMainFrame`), the renderer compositor's draw after it,
  and the display compositor's `Display::DrawAndSwap` after that; it
  prints tick → draw call and tick → swap side by side. Headless, so the
  browser's own scheduler at 60 Hz with no display behind it: a real one
  shows a swap at its next refresh.

  *Not built: drawing the frame the moment it comes.* The continuation
  that posts the next tick could also draw (the review's prototype). To
  the draw call it looks like the wait (a key to the draw, frames of 1.1
  refreshes: 29.9 ms against the wait's 28.6 and 37.9 at the next
  refresh), but a canvas drawn outside a refresh's callback is committed
  with the next refresh's main frame all the same: its swap came no sooner
  (28.0 ms against 25.8 at 1.1 refreshes, 32.5 against 32.9 at 1.4), so
  the screen would not show it, and the probe that measures to the draw
  call would say it did.

  *Open: a presenter that may sleep.* What a touch screen pays for not
  waiting — the 10 ms at the swap past a refresh, above — is the price of
  a main thread that can only wait by spinning. A worker may sleep
  (`Atomics.wait`): a presenter worker on an `OffscreenCanvas` could wait
  for every frame, draw it the moment it is done and hand it to the
  compositor itself, with no core kept busy: the wait's column of the
  table with the phone's gain from not spinning (2640x1080 in touch play,
  58 → 67 frames shown a second). It is the parked `fleet/present120`
  branch's shape with its spin made a sleep; that branch is some 1000
  lines against a page that has since changed under it. Not built.
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
| 4 | CLEAR_KEYS | — (a release may be lost: `ClearAllStates`, "Input") |
| 5 | POINTER_UNLOCKED | — (the port's `+mlook` release: lookspring) |
| 6 | AUDIO_READY | `ready u8`, `0 ×3`, `rate u32` (the AudioContext's sample rate; 0 none yet) |
| 7 | CALL | `id u32`, then the UTF-8 line |
| 8 | END | — (written by the host, not the page: "nothing more queued") |
| 9 | WINDOW | `w u32`, `h u32`: the page's box for the picture in device pixels (its CSS size x `devicePixelRatio`; the whole screen in fullscreen), sent at start and on every resize (a page may send its `devicePixelRatio` after them, `f32`, which the program does not read: a touch screen is the command line's `-touch`, "Settings") |
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
| 20 | RUMBLE | `strong f32`, `weak f32`, `ms u32`, `pad u32` (1: the pad is read): the pad's two motors, or a phone's vibration (`joy_rumble`) |

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
  does, and Firefox 155; else through one copy into a staging buffer, the smallest copy that
  works: a byte a pixel, 0.06 / 0.25 / 0.56 ms at 1280×800 / 1440p / 4K
  here, the upload after it no slower), and a fragment shader draws each pixel as
  `texelFetch(palette, texelFetch(frame, p).r)` — exact integers, no
  filtering, blending, dithering or colour conversion, so the canvas holds
  exactly the RGBA the program's own pack would. No RGBA pack runs in the
  program, and a palette shift costs 1 KB. The frame goes up before its
  palette, and the order matters: Chromium sends a context's uploads
  through one transfer buffer, which it resizes by what is in use when an
  upload asks. A 1 KB palette asked for first, with the last frame's bytes
  already consumed, made it shrink the megabytes it had grown to, and the
  frame behind it made it grow again: fresh shared memory, faulted in page
  by page, twice a frame (a trace on the phone: `TransferBuffer::Free` 340
  times in 3 s). Asked for right after the frame, the palette finds the
  buffer in use and nothing is resized: on the phone the uploads
  and the draw call went from 2.27 to 0.29 ms at 2640x1080 and from 1.16
  to 0.13 at 1320x540, every frame under 1 ms (one page, 15 s each way;
  64 `Free`s in 3 s). The old order was sometimes quick too — the state is
  sticky either way — which is how the same frame measured 2.6 ms in one
  minute and 0.3 in the next. On a desktop (headless Chromium on the GPU,
  the page's own loop at 60 Hz, main's page and this one twice each): 0.55
  → 0.15 ms at 2.6 megapixels and 1.12 → 0.57 at 3806×2076, 120 `Free`s a second
  → none.
- **2-D canvas** (no WebGL2 — a headless Firefox with no display to ask —, a WebGL2 drawn by the CPU,
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
everywhere, in headless Chromium (SwiftShader and the GPU) and Firefox 155
(WebGL2 on the GPU, taking the shared views: no staging copy was needed; and
the 2-D canvas). A headless Firefox has WebGL2 only when it has a display to
ask (`DISPLAY` set, here the Wayland compositor's X11 layer and the integrated GPU; with none it says
`AllowWebgl2:false` and the page takes the 2-D canvas), so the checks run
both ways. Natively, `quaketool play
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
  settings the id way (`config.rs`, `Host_WriteConfiguration`): the preset,
  then the `bind` lines and archived cvars that differ from its values on this
  machine, written when
  one of them changes and exec'd at startup as quake.rc does. When a written file is closed, `wasi.js` sends it
  to the page, which keeps it in IndexedDB (`quake-rs`, store `files`, keyed
  by path) and hands every kept file back at the next start. Without
  IndexedDB the page falls back to localStorage (`quake-rs.file.<path>`).
- **Migration.** The old page kept saves as `quake-rs.sav.<name>` and the
  settings as `quake-rs.resolution`/`.viewsize`/`.extras` in localStorage. At
  start the page moves them once into the game directory — the saves to
  `id1/<name>`, the settings as the `config.cfg` lines the program would have
  written — and removes the keys. A migrated `viewsize 100` was the old page's
  default, not a choice, so it is dropped like the other restated defaults
  (`LEGACY_DEFAULTS`): that player gets the preset's own Screen size, 110 in
  slop. Any other size is kept.
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
  sound does, and a hidden tab (or a phone held upright) pauses it with the
  game (WinQuake paused the CD when it lost the screen). A looping track loops in the element; a
  track played once reports its end (the `cd_ended` call: MCI's notify).
- **Without music** there is no drive: no `-cdtracks`, no `CD` records,
  nothing in the program changes (id's `cd_null.c`, which the C oracle is
  built with). The `cd` command says "No CD in player.".

The checks read the drive through `quake.cd.state()` (what the program
asked for; the element's time, loop and level; the output's RMS) and the
`cd_state` call.

## Settings, and how the page shows the picture

Every setting is the program's (`quake_rs::settings`: id's cvars and key
bindings, and the port's slop options, which the presets **Classic** and
**slop** set; the controls are the same in both, and `idcontrols` is the
console's one step to id's own). A few are numbers the machine picks once, at
start, from the command line (`settings::Machine`): `-touch`, a coarse pointer
(a phone or a tablet: 2x, at most four threads), and `-hwthreads N`, the
threads offered (all of them elsewhere, at 1x). No machine caps the frame
rate: slop's cap is none everywhere, Classic's id's 72.
The page passes `-touch` from `touchScreen` (`commandLine`), the one test the
touch controls and the pacing's lines (`pacing.scarceCores`) use too, so the
three never disagree about a device. The page needs three settings, and
hears them in the `STATE` record:

- **Native resolution** (`vid_native`, slop). The page sends its box for the
  picture in device pixels (`WINDOW`); the program renders the box divided
  by a whole pixel size (`vid_pixelsize` 1..4, the machine's to start with;
  past what the threads build's memory holds, 12 million pixels a frame,
  the next size up: "What 512 MiB holds"; and in a box so small that the
  size would make a frame under id's 320x200, the next size down, so the
  picture never outgrows its box) and says the size in
  `pixel_size`; the page makes the canvas exactly `W x pixel_size` device
  pixels wide and `H x pixel_size` tall (`fitCanvas`), `image-rendering:
  pixelated`, so every picture pixel is a whole square of screen pixels at
  the box's own aspect (the view is Hor+: `fov_adapt`). Off (Classic), the
  picture is the video mode (`_vid_resolution`, Options > Video Options)
  in the largest 4:3 box the window fits, as before.
- **Alt+Enter toggles fullscreen** (`vid_altenter`, on in both presets; with
  `idcontrols` the chord is id's ALT `+strafe` and ENTER `+jump`): "Fullscreen", below.
- **The preset from the address.** `?classic` and `?slop` (or the older
  `?2026`) add `-preset classic` / `-preset slop` to the program's command
  line (`wasi.js` hands it `args`). After `config.cfg` the program applies
  it only when it is not the preset the stored settings were last set to
  (`sys.rs`, `address_preset`): a first visit to a bookmarked `?classic`
  gets Classic, and the player's own changes on top of it — the controls
  too — survive every reload. The console's `preset` (and a `+preset` on
  the command line) applies every time.
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
with whole pixels at devicePixelRatio 1 and 2, `?classic`, Reset to Classic
and `preset slop`, the reload), `verify_touch.py` a touch screen's numbers;
the checks that pin id's behaviour open the page as `?classic`, and
`bench.py` does too, so its frames hash as `quaketool play`'s.

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
- **Every key up when a release may never come.** A key let go while the
  page did not have the keyboard never comes up to it, and Chromium also
  drops every keyup after a key the browser handled itself (the Esc that
  ends the pointer lock) until the next keydown: W held through that Esc
  stayed `+forward` for good. So, as id's `vid_win.c` ran `ClearAllStates`
  on every activation change and mode set (vid_win.c: "fix the leftover Alt from any
  Alt-Tab"), the page sends one `CLEAR_KEYS` record whenever a release may
  have been lost: the window blurs, the tab hides, the pointer lock ends,
  fullscreen ends, a phone is turned upright ("Touch"). The program (`input.rs`, `clear_all_states`) runs
  `Key_Event (key, false)` for every key, so each `+` binding lets go as
  its release would, then `Key_ClearStates` and `IN_ClearStates`; the page
  ends its unlocked mouse drag. A key still held presses again with its
  next autorepeat (its repeat count starts over); one whose repeat a later
  key stopped waits for its next press, as in id's. Not on entering
  fullscreen (no keyup was lost there) nor on `pointerlockerror` (a lock
  never taken loses nothing). The touch controls' `releaseAll` and the pad
  are unchanged. `verify_input.py` (8) checks each trigger headless with the
  `keys_held` call; a headed Chromium 146 (2026-10-02, as in "Fullscreen")
  showed the stuck W before, and nothing left held after in 15 cases (Esc
  windowed and held in fullscreen, fullscreen without Keyboard Lock, the
  focus to another window with Alt or the fire button held, a click
  elsewhere, Alt+Enter both ways, F11, a new tab); a headed Firefox 155
  (2026-10-03, as in "Fullscreen") left nothing held in the one case tried:
  W held through the Esc that ends the lock and fullscreen together. Not
  tried: Safari, macOS, Windows, a real Alt-Tab (XTEST's keys never reach
  the Wayland compositor there; another window was activated instead).
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
  (Chromium's raw input, on Windows, macOS and ChromeOS; Firefox's since
  152, on Windows and macOS; Safari's on macOS): id's `IN_StartupMouse`
  switched Windows' pointer acceleration off while the game ran, so a count
  was always the same turn. Refused (Linux, an older browser), the plain
  lock, at once and from then on (Chromium refuses a burst of lock
  requests); `?plainlock` never asks (the mouse check's). Each mouse
  `pointermove` is a `MOUSE` record: the sum of its coalesced samples'
  `movementX/Y` (`getCoalescedEvents()`; floats, never rounded), and the
  `mousemove` that follows it is skipped; a `mousemove` with no
  `pointermove` before it carries its own. Chromium and Firefox sum a
  refresh's samples into the one event they send, so there the sum is the
  event's own movement, count for count; Safari keeps only the newest
  sample's in it, and the list holds the rest ("The mouse at any frame
  rate", below). The program adds each record as it comes: the turn per
  count is the same at any frame rate. `pointerrawupdate` would deliver
  samples sooner within a refresh, but the frame starts at the refresh
  either way, so it would not show them sooner.
- **The wheel** (2026's weapon cycle, `Bindings::with_wheel`). id's
  `WM_MOUSEWHEEL` (`vid_win.c`) turns every message into one press+release of
  `K_MWHEELUP`/`K_MWHEELDOWN`, whatever the message's own delta — Windows
  already chunks a wheel's spin into one message per notch. A browser's
  `wheel` event has no notches, and its `deltaY` says little: Chrome's notch
  is 100 px on Windows (`WebMouseWheelEventBuilder`: the notches × the
  system's scroll lines × 100/3) and 120 on Linux (`kWheelDelta`), but on
  macOS a notchy mouse's is NSEvent's accelerated `deltaY` times 40
  (`kScrollbarPixelsPerCocoaTick`, `web_input_event_builders_mac.mm`;
  WebKit's `pixelsPerLineStep` the same): 4 px for a slow notch, 72 for a
  quick one (a Mac, Chrome 153). Three rules, the first that
  applies:
  1. *The platform's notches.* Chromium's legacy `wheelDeltaY` is
     `wheel_ticks` × 120 (`kTickMultiplier`, `wheel_event.cc`; divided by
     the page zoom): the notch count for a wheel mouse — `WHEEL_DELTA`'s on
     Windows, an XInput notch's on Linux, the raw
     `kCGScrollWheelEventDeltaAxis1` on macOS — but for a precise device on
     macOS (a trackpad) its pixels ÷ 40, so 3 × `deltaY`; WebKit's is
     always 3 × `deltaY` (`wheelTicks` = `deltaY` ÷ 40, `WheelEvent.cpp`).
     So a pixel event whose `wheelDeltaY` is a multiple of 120 and not
     3 × its `deltaY` is that many notches, however close the next comes:
     a Mac's 72 px notches 29 and 51 ms apart, which the second rule
     made one switch, are two. (Chromium's DevTools give every synthesized
     wheel event one such notch, `input_handler.cc`.) Firefox's events are
     lines (`deltaMode` 1) with the same count in `wheelDeltaY`, so rule 1
     takes them too: a notch is 6 lines on Linux (headed Firefox 155 on X11,
     real wheel clicks through XTEST, 2026-10-03: `deltaY` ±6, `wheelDeltaY`
     ±120; Chromium's own, on the same desktop, ±120 px), not the 3 the page had
     assumed, and a Mac's have no ticks (rule 2).
  2. *A lone event.* Else an event more than 100 ms after the last is a
     notch of its own, whatever its size — a mouse's notch comes alone:
     Safari's, and Firefox's on a Mac (`deltaMode` 1, and no ticks:
     `nsCocoaWindow.mm` sets none) — fired at once and counted as a whole
     notch's worth.
  3. *A stream.* Else (a trackpad, a fast spin in Safari) the events
     accumulate at 100 px a notch — `deltaMode` 1's lines at a third of it
     — the direction's flip dropping what was carried. (Until 2026-10-03
     rule 1 read pixel events only, and Firefox's lines went here at 3 a
     notch: its real 6 counted two notches an event in a spin quicker than
     100 ms a notch, so 6 clicks switched 11 weapons; now 6, at every gap
     from 300 ms to none. Chromium's 6 were 6 before.)
  Each event is capped at three notches, so a fast flick or a trackpad's
  fling can't cycle through every weapon. It needs no pointer lock — id's
  own never did — and fires whatever has the keyboard: `Key_Event` routes it
  itself (the console already scrolls on it, `consolekey()`; Customize
  controls' bind grab takes it like any key). `default.cfg` predates the
  wheel, so Classic leaves it unbound, as id's players who bound it
  themselves; 2026 binds a notch up to `impulse 10` (next weapon) and down
  to `impulse 12` (previous). `verify_input.py` (5b) feeds Playwright's
  notches, Firefox-shaped ones (6 and 3 lines with their ticks 30 ms apart,
  and a Mac's lines with none), a trackpad's burst (`wheelDeltaY` 3 × `deltaY`), Safari-shaped
  notches (4, 8 and 12 px 150 ms apart: three; a stream of 6 px; a turn
  back; a 1000 px flick), a Mac's Chrome events (72 px, `wheelDeltaY`
  −120, 29 and 51 ms apart: three, and one back up), two notches in one
  event, and a Mac trackpad's 40 px steps 16 ms apart (accumulated: 4 of
  10). `?mousecheck` logs each wheel event (`deltaY`, its mode,
  `wheelDeltaY`, the time since the last, the notches made of it and the
  rule: `ticks`, `alone`, `stream`), the last three in its box. A page zoom
  other than 100% in Chromium, and Firefox's and Safari's quick notches on a
  Mac, fall to rules 2 and 3.
- **The gamepad.** The Gamepad API has no events for a pad's state, so the
  page polls `navigator.getGamepads()` once per refresh, just before the tick
  (as late as the frame allows), and sends a `GAMEPAD` record when the state
  changed: the first connected pad with the standard mapping, else the first
  connected. The program keeps it, and the host frame the tick runs reads it
  as id's joystick: `IN_Commands` (buttons as `JOY1`.., `AUX5`.., the D-pad as
  the hat's `AUX29`..`AUX32`) and `IN_JoyMove`; quake-rs
  `client/in_win.rs` has the mapping from a standard pad to winmm's axes and
  buttons. Both profiles read it (`joystick 1`, a control: the same in both): a
  twin-stick layout of `bind` lines and `joy*` settings, with its buttons as the
  menu's keys (`joy_menukeys`); id's own is `idcontrols` (`joystick 0`, then `joystick 1`
  for the plain joystick). A pad's
  button also takes the click-to-play scrim away (a browser may not count it
  as the gesture audio needs: then the first click or key starts the sound).
- **Rumble** (`joy_rumble`, both profiles): a `RUMBLE` record after a frame in which
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
pad, id's `joystick 0` and `1` (after `idcontrols`), Classic's default pad, and on a touch page the rumble going to
the pad or the phone, whichever was used last).
The synthetic pad stands in for the browsers' own Gamepad API; no real pad
was tried.

**The mouse at any frame rate.** Nothing on the mouse path is per frame:
the page forwards each event's movement and `IN_MouseMove` adds it to the
view angles when the record arrives, before the frame that draws them.
`sys::tests::the_mouse_turns_the_view_the_same_at_any_refresh_rate` feeds
the program a 1000 Hz mouse's second of motion (1000 counts) the way a
browser delivers it — a whole number of counts per refresh, in one to three
events — through its own loop at 60, 144, 240 and 480 Hz in the 2026 profile
and behind Classic's 72 fps gate: the drawn view turns 160° each time.
`verify_input.py` does the same in the page at headless Chromium's 60 Hz and
with its frame-rate limit off (several hundred refreshes a second; Firefox:
`layout.frame_rate 480`), with synthetic events: headless Chromium's own
pointer lock dispatches a `mousemove` at screen (0, 0) every frame, which
cancels every real move (it is also the view's jump to pitch −70 on
locking). The browsers' side was measured with real input on a Linux desktop
(2026-10-02: the Wayland compositor's virtual output, its X11 layer, XTEST relative motion, 1000
counts at 1000 Hz, the pointer locked). In the game, Chromium 146 and
Firefox 155 delivered all 1000 counts and the view turned 160° at a 60 Hz
and at a 455–710 Hz refresh; on a bare page with its main thread busy (up
to 14 ms of a 60 Hz frame, 3 ms of a 480 Hz one) they and WebKitGTK 26.6
delivered every count, in `mousemove` and in `pointermove`'s coalesced
samples alike.

Fractional and tiny counts arrive whole. The record is an f32, and
`mouse_move` (`quake-wasm/src/input.rs`) multiplies it by `sensitivity` in
f32 — no `(int)` mickeys, no `m_filter` — and adds it to the yaw, which
rounds only to its own step:
`sys::tests::fractional_mouse_counts_turn_the_view_in_full_at_any_refresh_rate`
turns the view 48° with 1000 events of 0.3 counts from a 1000 Hz mouse (and
1.6° with 0.01), at 60 and at 480 Hz, a record an event or a refresh's
summed, in both profiles. The one loss was the yaw's range: the port's
`CL_AdjustAngles` never brought it back within a turn (id's does, with
`anglemod`), and an f32 far from 0 has coarse steps — after a hundred
turns (36000°, a step of 2^-8°) a tenth of a count turned the view 2%
short and a hundredth not at all. `math::angle_wrap` keeps it within
[−180°, 180°) by whole turns, exactly (not `anglemod`'s truncation to
1/65536 of a turn, which at 480 Hz would make the turn itself depend on the
frame rate): `a_small_mouse_delta_turns_the_view_after_any_number_of_turns`.
Nor is the drawn view a stepped or smoothed copy of the turn: the camera's
yaw is the frame's `v_angle` (the move's yaw) plus the weapon's kick, and at
480 Hz one count before every refresh turns every drawn frame by exactly
0.16° (`every_frame_draws_the_mouse_turn_so_far`; Classic draws at most 72
of the refreshes, each turned by the counts since the last).

The look takes `pointermove`'s coalesced samples because of WebKit. From
the browsers' sources (read 2026-10): Chromium's and Firefox's `movementX`
is an integer, which their per-refresh coalescing sums (Chromium's raw input
copies the OS's counts — Windows' `WM_INPUT`, macOS's unaccelerated field —
and its plain lock differences truncated positions, which telescopes), and
the coalesced samples add up to the event's own. WebKit's is a double, but
`WebPageProxy::handleMouseEvent` replaces a queued move with the next
(`removeOldRedundantEvent`) and, since 2023 (WebKit bug 259408), sends the
page one a frame: the event's own movement is the newest sample's, the
coalesced list has them all, and summed they are every count again. (Under
Safari's unadjusted lock the samples are the accelerated movement and only
the event's own is raw, so the turn follows macOS's acceleration there:
accepted, against losing most of it. Safari also runs pages near 60 fps by
default.) What no page can recover is a fractional delta truncated per OS
event before the page sees it: Chromium on macOS with the plain lock (the
page asks for the unadjusted one, which Chromium grants there; macOS 26
also delivers one move per display frame, crbug 465798393) and Firefox's
Wayland relative pointer. Between the page and the program nothing is per
frame either: the input ring holds 64 KiB, some 5400 `MOUSE` records (a
full ring drops a record and counts it, `inputDropped`), in order with the
keys; `timeStamp` is only for the latency measurement; the program reads
every record before its next frame. `verify_input.py` (10): 1000 records of
0.3 counts turn 48°, a `pointermove` and its `mousemove` count once, and a
real drag counts the same through the samples as through `mousemove`.

The frame clock is not in the turn either. The game's only clock is the
display's: a tick's dt is the difference of two `requestAnimationFrame`
timestamps (in steps of 5 µs in Chromium and 20 µs in Firefox in this
cross-origin-isolated page, measured; the differences telescope), which the
program adds to `realtime` and `host_filter_time_display` makes a frame's
`host_frametime`; the WASI clock only times a frame's own work. Nothing on
the turn's path is scaled by time or by a count of ticks (no `m_filter`, no
average, no remainder carried; `Tick72` steps only the palette's fades):
`sys::tests::the_turn_does_not_depend_on_the_frame_clock` turns the view
48° (320°) with 1000 records of 0.3 (2) counts at 60, 240, 480 and 1000 Hz,
and with every tick's time reported at half and at twice the real one. A
wrong clock would show instead as the whole game running fast or slow, and
in the mouse check's `clock:` (below). What does depend on the frame rate is
id's mouse *movement* (`+strafe`, `lookstrafe`, or mouse Y without mouse
look): a count is a wish speed for the frame it lands in (`cmd.sidemove +=
m_side * mouse_x`), so 1000 counts of `lookstrafe` moved the player 4.6
units at 72 Hz, 1.2 at 144 Hz and 0.1 at 480 Hz (open). The turn and the
pitch are per count.

On Linux there is no raw input and `movementX` is in CSS pixels: at a
device pixel ratio of 1.5 the same motion is 668 counts (Chromium) or 665
(Firefox), at every rate — the pixel ratio scales the turn there, the frame
rate does not. Not verified: Windows and macOS, Safari, and a real 480 Hz
display.

**The mouse against the trackpad (macOS).** The turn per count is
`sensitivity` × 0.16°/3 (`M_YAW_PORT`, `quake-wasm/src/input.rs`; id's is
0.022° a mickey, so at the default `sensitivity 3` the port turns 0.16° a
count, 2.4 times id's 0.066°). Under the unadjusted lock a count is the
mouse's own: 2250 counts a full turn, 5.7 cm at 1000 CPI, 14 cm at 400 — a
fast turn, if every count arrives. Chromium on macOS reads each event's
`kCGEventUnacceleratedPointerMovementX` as an integer
(`web_input_event_builders_mac.mm`, `GetWebEventLocationForEventInView`),
WebKit the same field as a double (`unadjustedMovementForEvent`; WebKit
offers the unadjusted lock on every Mac, `HAVE_MOUSE_UNACCELERATED_MOVEMENT`
— which Safari first shipped it, not checked), whatever the device; what
macOS puts in it for a trackpad's motion is not documented. And since
macOS 26 AppKit merges mouse moves into one event a display frame, before
the browser sees them (crbug 465798393, open: a 1000 Hz mouse reads at
120 Hz on a 120 Hz Mac; Chromium's fix, `mouseCoalescingEnabled` off, was
reverted; by a comment of 2026-10-01 Safari 27 now does the same). By that
report the moves it merges are lost —
"sensitivity becomes extremely slow" in web FPS games with 500–1000 Hz mice,
in Chromium only, back then — and `getCoalescedEvents()` cannot bring them
back (its list holds one sample there). That fits a mouse that turns too
little at 120 and at 480 Hz alike beside a trackpad, whose own rate is
near the display's, that turns as it should. Not verified here (no Mac);
the mouse check measures it (below).

What the merged event carries nobody on the bug measured, and AppKit's
source is closed; Chromium adds nothing of its own (it reads the one
event's field). The reading that fits what is in hand is the newest
report's movement alone, not the sum: the reporter's games turned slow, and
so does the mouse that was measured, where a sum would have made it fast (7 cm a full
turn at 800 CPI). The game then gets the display's rate ÷ the mouse's
polling rate of the counts: for a 1000 Hz mouse a third at the 328 Hz the
mouse check saw, an eighth on a 120 Hz Mac, a sixteenth on a 60 Hz one
(and half of an office mouse's 125 Hz there) — a constant factor on a given
display, which Mouse Speed makes up. A full turn in centimetres of mouse
travel, 1000 Hz mouse (id's own default was 35 cm with 1996's 400 CPI; 15
to 40 is the usual range):

| | every count | 120 Hz Mac | 60 Hz Mac |
|---|---|---|---|
| 800 CPI, Mouse Speed 3 (the default) | 7 | 60 | 119 |
| 800 CPI, Mouse Speed 11 (the slider's end) | 2 | 16 | 32 |
| 1600 CPI, the default | 3.6 | 30 | 60 |
| 1600 CPI, the slider's end | 1 | 8 | 16 |

So id's 1 to 11 reaches a usual turn in every one of them (800 CPI at
60 Hz only at its end), and the slider stays id's; the console's
`sensitivity 20` goes past it (no limit, saved in `config.cfg`; the slider
then shows its end, as id's). What Mouse Speed cannot give back is the
fineness: one report stands for a frame's, so at Mouse Speed 11 a count is
0.59°, and a slow, small move (under a count a millisecond) reads as
nothing in most frames. A mouse that polls no faster than the display
refreshes (125 Hz on a 120 Hz Mac) loses nothing; the default is then fast
(the first column) and the slider goes down instead. A pointer utility
is not in it: its pointer settings are properties of the
system's acceleration (`HIDPointerResolution`, `HIDMouseAcceleration`,
`HIDUseLinearScalingMouseAcceleration`), which
shape the accelerated pointer the unadjusted lock does not read, and its
event tap leaves moves alone but for its hold-a-button gestures
(its event tap); a hardware DPI it sets on a mouse changes
the counts as any DPI setting does. The page does nothing by itself here
(it cannot know a mouse's polling rate): the README and the keys drawer say
to raise Mouse Speed. Not verified: the factor on a real Mac (the mouse
check's counts over 10 cm along a ruler would give it), Firefox there.

**The mouse check.** For a display, a device and a browser the local checks cannot
run, the page measures itself. Open it with `?mousecheck` in the address —
`?mousecheck=mouse,trackpad` names the runs in turn — start a game and click
the view: a box at the top left says which run is next and on what ("run #1
(mouse): move the mouse now — 10 seconds from the first move; Esc ends it"),
counts the run down with its live counts, then keeps its summary and, while
the pointer stays captured, arms the next run 1.5 s later (Esc and a click
do too). The box keeps the last four summaries, each numbered, stamped with
the time its run started and named, and the last three wheel events; the
console also has a line a second. `quake.mousecheck(seconds)` runs one from
the console and resolves to its summary. `?plainlock` added never asks for
the unadjusted lock: the system's accelerated pointer, as the desktop moves
it. A run with real input here (2026-10-03, XTEST on the desktop's Wayland compositor's virtual
X11 layer, 8000 counts at 1000 Hz, Firefox's frame-rate limit off):

```
#1 01:51:24 (mouse) mousecheck 10.0 s: 401 Hz refresh, 401 fps, 1.00 a refresh | clock: tick 2.08 ms median, game 1.000 s a second | mousemove 3182 × 8000.00 counts | pointermove 3182 × 8000.00 counts, 2.51 an event, peak 480/s, in 3824 samples (1.20 an event), 0 fractional | rawupdate 3824 × 8000.00, peak 660/s | records 3182 sent, 0 dropped, 3182 read × 8000.00 | turned 1280.00°, 0.1600°/count | smallest 1.000 | Firefox 155 Linux, dpr 1, plain lock
```

In order: the run, the time it started and its name; the display's refresh
as the page sees it (`requestAnimationFrame`), the game's frames a second
and a refresh; the clock: the ticks' median dt (1000 ÷ the refresh rate, in
ms, while the frames keep up) and the game's time a second (`host_time`:
1.000 unless frames are clamped at 0.1 s or the clock is wrong); the
browser's `mousemove`s and their own |movementX| summed; the `pointermove`s,
their coalesced samples' sum (what the look takes) and a mean per event,
their peak rate (the most in a tenth of a second, × 10: the rate while the
hand moves, not the run's mean), the samples and a mean per event, and how
many had a fractional part; the `pointerrawupdate`s (Chromium, on a secure
page, and Firefox) and their peak rate; the `MOUSE` records the page sent
and dropped, and those the program read with their counts; the turn they
gave the view, and per count (0.16° at `sensitivity 3`, at any rate); the
smallest sample; the browser, its system, the pixel ratio and the lock
asked for. Within a line: `mousemove` short of `pointermove` is a browser
keeping only the newest sample in the event (the look sums them already);
`pointermove` short of `rawupdate`, its per-refresh coalescing losing
counts; `read` short of `sent`, or anything dropped, the ring; another
°/count, the game. The rates say who sets the pace: `rawupdate`'s peak is
the moves the browser gets a second — the mouse's polling rate (125, 500,
1000 Hz) when nothing merges them, the display's refresh when the system
merges them into one a frame (macOS 26, above), so a peak at the refresh
with a mouse polling faster is counts merged before the page. With one
sample an event and `rawupdate` equal to `pointermove`, as on a
Mac (Chrome 153, 328 Hz: 625 events, 7.37 counts each), the browser never
got two moves in a frame: the peak, or the mouse's polling rate, tells the
merge from a slow mouse. Between runs of the same motion — the same sweep
across the mouse pad, or 10 cm along a ruler — the counts are the measure:
with the unadjusted lock a mouse's are its CPI ÷ 2.54 a centimetre (394 at
1000 CPI), at any speed and any rate. The mouse's run against the
trackpad's gives their ratio in a minute; fewer counts than the mouse's CPI
says, with one sample an event, is counts lost before the page; the same
run with `?plainlock` says whether the system's accelerated pointer keeps
them. Here, in Chromium 146 and Firefox 155, at 60 Hz and at 406–496 Hz,
every column read 8000 (a few Chromium runs at the higher rate read 7997 to
7999 in every column: counts lost before the page, to its plain lock on X).

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
tick, and a frame the wait can wait for is presented in the refresh that
ticked (a slower one, or on a touch screen one that takes most of a refresh,
at the refresh after it is done: "A frame"). What would cut more is the
browser's (`?lowlatency`, below) or the frame's own time.

## Fullscreen

**The ways in and out.** The bar's *fullscreen* button, in every profile;
the fullscreen key, **Alt+Enter** (Option+Return on a Mac), in both profiles
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
chord to the page. With id's own controls (`idcontrols`, `vid_altenter 0`) it stays the
game's: `default.cfg` binds ALT `+strafe` and ENTER `+jump`, a strafe-jump in WinQuake, so
there is the button and F11. `vid_altenter` was `vid_fkey`; a
`config.cfg` with the old name still sets it (`quake_rs::cvar`'s
`OLD_NAMES`) and the next save writes the new one.

**Esc in fullscreen.** Browsers reserve Esc. Where the Keyboard Lock API
exists (Chromium: Chrome, Edge, Opera) the page locks Escape (and F12, "The
F-keys" above) on entering fullscreen: a tapped Esc is the game's, id's
`togglemenu`, with the mouse still captured; a held Esc is the browser's
way out. Elsewhere the browser's own Esc stands. Measured in a headed
Firefox 155 (below): with the mouse captured one Esc ends the lock and
fullscreen together, and the page opens the menu (the lock loss, "Input");
with the mouse free the first Esc only leaves fullscreen and the page gets no
key, the next opens the menu. Either way the game is windowed, with
nothing left held when the mouse was captured, and Alt+Enter goes back to
fullscreen. The hint on entering says what is on offer.

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
Keyboard Lock but no browser Esc handling. In a headed Firefox 155
(2026-10-03, the same recipe, Firefox with no Keyboard Lock API): a real
click on the canvas takes the lock; Alt+Enter enters fullscreen with the
lock kept and leaves it with the lock gone and no menu (the page's own
doing); one Esc in fullscreen with the mouse captured left both at once, opened
the menu, and a W held through it was not left held (nor after its release);
a click with the menu up does nothing, the next Esc closes the menu and the
click after it captures again; Esc in fullscreen with the mouse free only left
fullscreen, the next opened the menu. F11 did nothing in that Playwright-run
window (not a finding about Firefox's). Not tried: Safari, macOS, Windows.

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
  so the sound stops and goes on where the game does. A touch screen held
  upright does the same (the game waits behind the rotate prompt, "Touch"):
  `awayNow()` is the hidden tab or that.

**Classic and slop.** The setting is `snd_modern` (`Cvars::sound`, a
`quake_rs::snd::SoundMode`; Options > Slop Options > Picture and sound >
"Full-rate sound"),
a slop option: off in the Classic preset, on in slop. Classic
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
  declares (the JS API does not tell them, so `importedMemory` reads them):
  for the game, initial = maximum = 512 MiB, a memory that never grows
  (below, "A memory that never grows"). If the browser will not make it, the
  page stops and says so ("the game did not start: this browser would not
  give the game the 512 MB of memory it needs to run"): there is no other
  mode;
- before the program starts, makes a pool of thread workers (as many as
  `navigator.hardwareConcurrency`, 2–16), each another instance of
  `wasi.js` — a worker made after its parent has blocked may never start.
  If the browser will not make them (`Worker` throws, or one fails to
  load) the page stops and says so, on the boot panel and the status line, as
  for the memory ("the game did not start: this browser would not start the
  worker threads the game needs to run (…)"), and the pool made so far is
  ended: the threads build never runs on one thread instead — a game that
  quietly runs slower, in a mode nobody chose, is one more thing to keep
  track of (until 2026-10-03 it did, and `-hwthreads 1` was the sign;
  `verify_crash.py` makes `Worker` throw and checks the message). The
  player's own `r_threads 1` is not that and stays: a setting, in either
  build;
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
  (`hardwareConcurrency`, at most the pool plus its own).

**What the browser must give.** When it will not, the page says so once,
plainly, and the game does not start — never a degraded mode: the memory and
the thread workers (above), and WebAssembly SIMD, which `wasi.js` probes
(`SIMD_PROBE`, the usual `i8x16.popcnt` module) before it compiles the
program: "the game did not start: this browser has no WebAssembly SIMD, which
the game needs to run (Chrome 91, Firefox 89 and Safari 16.4 have it)". A
build that uses SIMD (`-C target-feature=+simd128`) cannot be compiled
without it, and asking first keeps the line from depending on how a browser
words its compile error; for a build without SIMD the probe is harmless,
since every current browser passes it. `verify_crash.py` checks all three
lines (stubbing `Worker`, `WebAssembly.Memory`, and `WebAssembly.validate`
with `compile`).

A thread has the clocks, randomness, sleep and stderr (to its worker's
console: the parent never reads messages again). The files, stdin and
stdout stay the main program's, and a thread cannot spawn threads yet.

**A thread's stack** (1 MiB, malloc'd, no guard page) is what `std` asks
wasi-libc's `pthread_create` for:
1 MiB (`std::thread`'s wasip1 `DEFAULT_MIN_STACK_SIZE`; the frame's scoped
threads, which bake and draw the bands, ask nothing more), allocated from the program's own
heap, with no guard below it — linear memory has no unmapped pages. A
thread that recurses past it does not fault: it writes over whatever the
heap holds below its stack (the review of the bakes measured 1500 KiB of
frames silently overwriting the heap). Nothing the threads run recurses
deeply: the light tool's trace (quake-rs `render/torch.rs`'s `test_line`)
goes as deep as the world's node tree, tens of frames.

**A memory that never grows.** In Chromium a worker thread can trap —
`RuntimeError: memory access out of bounds`, in `calloc` or wherever it
first touches the memory — when another thread grows the shared memory
(`memory.grow`, from `malloc`'s `sbrk`) while it runs: one thread grows the
heap, another is handed and touches what lies in the new pages. The game hit
it about one page load in four on the registered episode
(`verify_content.py`, a worker of the torch set's build after a map load:
2 of 12 runs) once its threads allocated as the heap grew; 40 lines of safe
Rust show it alone (`threadcheck`'s last stage: eight scoped threads each
allocating and keeping buffers, so the heap grows under them) — 6 of 20
Chromium runs trapped, 0 of 10 in Firefox. With the memory linked at
initial = maximum, so that `sbrk` never grows it, 0 of 40 (and `verify_content.py`
24 of 24). So `quake-wasm/build.rs` links the threads build that way, 512
MiB (`QUAKE_WASM_GROWABLE=1` links it growable again), the program checks
the size at startup (a line on stderr if it is not), and `verify_threads.py`
runs `threadcheck` repeatedly, fails on any trap, and in a Chromium before
157 shows the growable link still trapping. The single-thread build has no
shared memory and no workers, and keeps its growable one.

Why: a V8 bug, fixed upstream on 2026-09-29 (V8 commit
[3424101](https://chromium-review.googlesource.com/c/v8/v8/+/8466625),
"Atomic memory.size and dynamic bounds checks for shared memory"; Chromium
issues [529880019](https://issues.chromium.org/issues/529880019) and
[533026477](https://issues.chromium.org/issues/533026477)), first in Chrome
157. Up to 156 each worker keeps its own copy of a shared memory's size,
refreshed only when it enters wasm from JS, when it grows the memory itself,
or when it handles the grow's interrupt. Ordinary loads and stores are
covered by guard pages and see the new pages, but `memory.fill`,
`memory.copy`, the atomics and `memory.size` are checked against the stale
size — and `calloc` (Rust's `vec![x; n]`) is a `memory.fill`, a `Vec`'s
`realloc` a `memory.copy`. So a worker handed pages another thread has just
grown traps. Safari 26.0–26.2 had the same class of bug
([WebKit 303387](https://bugs.webkit.org/show_bug.cgi?id=303387)); Firefox
reads the live size. Others met it: napi-rs
([#3552](https://github.com/napi-rs/napi-rs/issues/3552), in rolldown) and
Emscripten ([#25905](https://github.com/emscripten-core/emscripten/issues/25905)),
whose pthreads build is a memory that never grows by default. It must be
the link's setting: wasi-libc's heap starts at the size the module was
linked with, so a bigger `WebAssembly.Memory` from the host would not give
`malloc` the room. The fixed memory stays after Chrome 157: older Chromes
and Safaris stay in use.

**A crashed thread** ends the game, it does not freeze it. A trap in a
thread (a panic aborts, `panic = "abort"`, so it traps too) used to leave
the thread's worker logging it and the program's main thread waiting for
ever to join it, and the page waiting for the program. Now the worker sets
the control block's run state to crashed, wakes every Sync waiter, and tells
the page on a `BroadcastChannel` the page names at `init` (the program's
worker, the thread workers' parent, may be the one blocked in that join and
read no messages); the page stops the program's worker, and its thread
workers with it, refuses any call waiting on the program, and shows "The game
stopped: a thread stopped (...)" over the view with a button to reload —
the same box a trap of the main thread, a full memory or a `Sys_Error`
during play now shows. `verify_crash.py` checks it with a deliberate panic
in a thread (automation's `crash_a_thread`): the box is up within a second.

**What 512 MiB holds.** The paks stay in the page's file store and are read
as the game asks, so the program's memory is its frames (about 14 bytes a
pixel: the 8-bit frame, the 16-bit z-buffer, the RGBA slots) and its caches.
Measured with a growable build (its size is the heap's high-water mark)
through every map of a game, each looked all the way round, in Chromium:
id1 (pak0 and pak1) 64 MB at 1886×996 and 153 MB at 3806×2076 (4K, pixel
size 1); Scourge of Armagon 68 and 153; Dissolution of Eternity 68 and 154
(this round's research measured 46 MiB at 1280×720 and 152 MiB at a 4K
window). With 1x the default on a desktop since 2026-10-03, the program
holds a frame to what the memory holds (`quake-wasm/src/vid.rs`,
`MAX_FRAME_PIXELS`, 12 million pixels; past it the next pixel size, the
player's own pick too). Measured the same way (shareware maps e1m1, e1m3,
e1m4 and e1m7, 1x on 8 threads): a page started at a size holds about 26 MB
and 19 bytes a pixel (1920x920 56 MB, 3840x2000 145, 5120x2720 286,
6400x3440 437, 7680x4160 623: 8K at 1x does not fit). 4K and a 5120x2160
ultrawide draw at 1x; 5K, 6K and 8K at 2x. A frame's buffers that grew with
the window did not hand the old frame's memory to the next, larger one (a
grown buffer moves, and the hole it leaves is too small for the next one):
4800x2880 then 5120x3200 took 618 MB where a page started at 5120x3200 takes
333, and on e1m3 a window walked up to 4224x2656 through 4, 6 and 17 sizes
held 307, 284 and 282 MB where a page opened there held 188 — by an amount
no setting bounded. So the threads build allocates every frame-sized buffer
(the frame pool's, the z-buffer, a presented frame's RGBA) once, for the
largest frame there is (`render::reserve_frames`, from `main`): every one is
one size, a hole fits the next, and the same walks hold 202 MB from the first
frame at any size to the end. The reserve is address space in a memory
already made whole; only the pixels a frame writes are touched.
`verify_present.py` walks a window up through sixteen sizes to the largest
frame on the fixed build, the game never stopped. It is half the threads build's old
maximum: a fixed memory is committed whole when it is made — free on Linux
and Android until a page is touched (here the page's resident memory with it
fixed is the growable build's: Chromium, all processes, 984 MB against 972 at
1 GiB), but a commit charge on Windows; 32-bit Chrome retries a smaller
reservation only for a growable memory; iOS counts a shared memory's
maximum against a pool (the research's notes). A full memory stops the game
cleanly on the main thread — `memory allocation of N bytes failed`, then
`std`'s abort — and the page shows "The game stopped: out of memory ...: a
larger pixel size, in Video Options, needs less" (a 96 MiB build at 4K).

**The renderer's threads** are the cvar `r_threads`, a number (at least 1;
0 on the console or in a file is this machine's number, as for
`vid_pixelsize`): the presets start it at the machine's — every thread the host
offers (`-hwthreads`; a `wasm32-wasip1` build, without threads, gets 1), at
most four on a touch screen (`settings::Machine::render_threads`; "On an
Android phone", below, has the measurements and the why) — and `vid::render_threads`
is where the frame reads it. `host::step` hands the count to the renderer of
whichever game draws the frame, every frame, so each
`Walk` and `DemoPlay` the host builds (a boot, a load, the attract loop's
next demo) draws with it from its first frame. A spawn the host refuses
(more threads asked than workers) leaves its bands to the threads that did
start, so any count draws the frame. The `render_threads` call reports the
count. The RGBA pack runs on the same threads.

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
page checks pass on the threads build as on `wasm32-wasip1`'s (Chromium; and
in Firefox 155 the 16 that take a deploy dir). Firefox on the threads build
is `crossOriginIsolated` and draws on 16 render threads: `timedemo demo1`
(Classic) at 1920×1200, one run each, 240 fps on one thread and 577 on 16
against Chromium's 280 and 773; and `verify_timedemo.py`'s 960×600, headless
on the 2-D canvas, six runs of each browser interleaved, medians: Chromium
958 fps, Firefox 821.

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
(Chrome on Windows and ChromeOS), at the risk of tearing. A quick frame
still arrives inside the refresh that ticked, so the hint matters exactly as
much as before. Off by default, and not verifiable headless: there it holds the
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
`calc_refdef` uses to keep the 3-D view off the bar (in 2026 too: the
"Status bar overlay", `scr_sbaroverlay`, only draws the world on under the
view beside the bar), so it is exactly right for every `viewsize` (0, 24
or 48 virtual rows; 2026 starts at 110, the 24-row status bar alone, and
Classic at id's 100, with the inventory strip over it), the "scaled 2-D"
extra's whole-number blow-up, and an intermission (always full screen, so 0). The page turns that into a CSS
custom property, `--bar` (`touch.js`'s
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
and Invert Mouse apply, and `freelook` (on by default) is what makes a
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
Options slider's knob (or a settings page's: Torch flicker); Help pages by
halves. A drag moves the cursor with
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
presses reach them the same way, and Slop Options and its pages
(Options' row 12) take OK, ▲▼ and ◀▶ as Options does — ◀▶ step
Torch flicker's slider — and BACK backs out a screen at a time; so does a
gamepad's A, B and D-pad (`joy_menukeys`). Taps and drags on the menu are unchanged;
the pad is in addition.

**The phone's keyboard.** KEYBOARD focuses a hidden text field (in the
tap's own handler, the only way iOS shows its keyboard); what the field
receives becomes KEY records (printable ASCII as its keynum with the typed
character; Enter; a deleted zero-width sentinel is Backspace; Android's
composed words when they end), and its key events never reach the page's
own keyboard handler. Multiplayer > Setup's name rows get the same button.

**Quake plays only sideways.** The user saw the game sometimes play
for a while upright on a phone and asked for landscape only, with the
usual rotate-your-phone prompt shown whenever the phone is turned upright.
(Until 2026-10-03 the prompt could be tapped away — "or tap to play
upright" — after which the game ran upright for good; and it ran on behind
the prompt while it showed.) Now, on a touch screen (`html.touch`: a coarse
pointer, or `?touch`), whenever the page's box is taller than wide (CSS
`(orientation: portrait)`: height at least width — the room the page has,
not the device's turn, so a split screen is judged by its own box), `#tRotate`
covers everything — the start prompt, the layer, a menu — opaque, so the
game's last picture does not show through. It is plain CSS, up in the
refresh the box turns, and there is nothing to dismiss it and no upright
mode left. The game waits behind it:

- **No ticks, no frames.** index.html's frame loop reads the same media
  query (`portraitNow()`, once a refresh) and on the change does what a hidden tab already
  does to it: sends no tick, presents nothing. The game's time stands still
  (a tick carries the time since the last; none comes), so the attract loop,
  a menu, the console and a game in progress stop alike with no state of the
  game's own changed. Turned back, `lastTick` starts over and everything
  goes on from the same instant — same game, same menu row, same demo frame.
  Not id's `pause`, which a hidden tab's touch handling uses (below): a demo
  ignores it (`host_pause`: "not really connected"), `pausable 0` refuses
  it, it toggles, and it draws a plaque nobody looks at; and the hidden tab
  wants a menu to come back to, where turning the phone back is the
  resuming.
- **Every key and finger is let go**: `clearAllStates()` (the CLEAR_KEYS
  record, as a blur sends it), touch.js's `releaseAll()` (stick, fire,
  jump), the menu pad's repeat timers, the phone's keyboard — the prompt
  takes the touches from here, so no release would come, and a finger still
  down when the phone turns back walks nobody on (it must lift and land
  again). A keyboard beside a tablet is not heard while it waits
  (`keyEvent`, `mouseMove`), so no stale presses wait for the turn back.
- **Sound and CD stop** as on a hidden tab (`awayNow()`: hidden, or this;
  `syncAudioAway`, `cdSync`) and go on where they were; the Screen Wake Lock
  is let go.
- **The window's box is not reported** (`sendWindow` returns): the program
  keeps its landscape frame instead of resizing to a tall one nobody sees
  and back; `releaseGame` sends the box if it changed. A page that loads
  upright reports none until its first turn. The report goes out from the
  frame loop, a refresh after the resize event (`windowDirty`), not from
  the event: Firefox evaluates a media query at layout, after the event, so
  a handler there still read the old orientation and sent the tall box.
- **A tap on the prompt asks for fullscreen and the landscape lock** (the
  gesture the covered "tap to start" would have given). A phone whose
  rotation is locked never reports landscape by itself; Android Chrome's lock,
  in fullscreen, overrides that and turns the page. Where a page has no
  fullscreen (an iPhone) the hint says to unlock rotation instead.

By reasoning, not run: a **tablet** is a touch device and follows the same
rule (an iPad held upright waits, as a phone does). A **foldable** is judged
by the box the page is given: a foldable phone unfolded is a tall box
upright (the prompt) and a wide one sideways (the game). Its cover screen is
close to square, so the page's box there is a coin
toss between the two and not checked; nor is what box Chrome gives a page
half folded. A desktop browser in a tall window has no `touch` class and
is unaffected (with `?touch` it follows the rule). The phone's keyboard
only shortens the box, so a sideways page stays sideways under it.

**Around the controls.** The first tap ("tap to start") also asks for
fullscreen and `screen.orientation.lock('landscape')` where the browser has
them (Android; a fullscreen button stays while not fullscreen). During a game a
Screen Wake Lock keeps the display on. Every touch resumes audio if the
browser suspended it (iOS "interrupts" it in the background). When the
page is hidden the audio is suspended, and with the touch controls on (in
both presets; off by hand, the game only stops getting ticks, as on a desktop) a
live game pauses (`pause`, id's plaque; STATE 512) under its menu; back in
the game — the menu closed, by the player — the pause ends. Haptics:
`QuakeTouch.rumble(weak, strong, ms)` takes the Gamepad API's dual-rumble
magnitudes and buzzes `navigator.vibrate` (Android; iOS Safari has none)
for longer the stronger it is; nothing calls it yet — it is the hook for
the `input` agent's gamepad rumble events (damage, heavy weapons).

**Phones.** *(As of 2026-09-26; since 2026-10-04 a touch screen's pixel size,
threads and frame cap are the machine's numbers — 2x, at most four threads,
60 — from the page's `-touch`: "Settings, and how the page shows the
picture".)* On
an iPhone in landscape (844×390 CSS px, devicePixelRatio 3, so a 2532×1170
box) Auto picked a pixel size of 2 with the single-threaded build: a
1266×585 frame, 2×2 device pixels a picture pixel (0.67 CSS px, finer than
the eye resolves at arm's length), and the scaled 2-D layer at
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
deploy the single-threaded build, or set `vid_pixelsize 2`. (Since then a
touch screen starts at 2×2 on four threads whatever it offers, the machine's
numbers, and a phone has been measured: "On an Android phone", below.)
iOS Safari: the game needs iOS 16.4 (WebAssembly SIMD, which the browser
builds use: "What the browser must give") and https for `SharedArrayBuffer`
(the page says so when either is missing); rAF runs at 60 Hz (Safari's
default even on 120 Hz screens), 30 Hz in Low Power Mode; Web Audio
follows the silent switch; the Screen Wake Lock needs iOS 16.4 (18.4 in a
home-screen app). Memory:
the single-threaded build's memory grows as the game needs it (the pak
stays outside it, "Files"); the threads build declares a shared memory of
up to 1 GiB (16384 pages), which a browser reserves up front for a
shared memory — the kind of reservation iOS has refused in other wasm
games, one more reason to give a phone the single-threaded build.

**On an Android phone (2026-10-03).** The phone (an SoC with
three small cores, four middle, one fast;
Chrome 154), measured over USB with `web/phone.py` (below). Played
fullscreen, the page has a phone-sized landscape viewport: a 2640×1080
frame at a pixel size of 1, 1320×540 at Auto's 2. `hardwareConcurrency` is
8. The threads build, the 2026 profile, demo1.

- **A finger on the glass doubles the frame rate.** The panel is adaptive:
  60 Hz idle, 120 Hz while a finger moves. Chrome follows it: the page's
  own `requestAnimationFrame` runs at 60 Hz with no finger down (also on a
  panel held at 120 Hz: frames then show for two refreshes each, cleanly)
  and at 105–110 Hz with one moving. So in touch play the game is asked for
  twice the frames in half the time (8.3 ms), the fast core is busy 80–99%
  instead of 4%, and the phone warms twice as fast. Every number below says
  which case it is; tables made with no finger are the easy case.
- **Heat decides.** A minute of 2640×1080 play and the phone caps its
  middle cores at 1171 MHz and the fast one at 1478 (42–44% of their top
  clocks) at a skin temperature of only 40 °C; half an hour of touch play
  and they are at 940 and 1248 (41–42 °C). A frame then costs 2.2–2.7×
  what it does cool. Even the 1320×540 frame gets there in touch play. The
  numbers that matter are the warm ones.
- **A frame in play costs more than back to back**: 8.4 ms against 3 at
  1320×540, cool, at 60 Hz. Between frames the cores idle and the
  scheduler clocks them down and keeps the game's threads on the slower
  ones.
- **Fewer frames, in touch play** (measured for a decision, nothing built).
  At 1320×540 the 72 fps gate (`wasm_uncapped 0`: at a 120 Hz page, a
  frame every second refresh) shows 61–62 a second instead of 108–115, the
  fast core 8–26% busy instead of 65–80%, and the phone cools while it
  plays: the caps came back from 940/1248 to 1286/1593 MHz in two minutes.
  At 2640×1080 there is nothing to hold: a frame is longer than the gate
  (58 shown either way), and a page that asks only every second refresh
  shows 33, since with the gaps the cores clock down and the same frame
  takes 24.6 ms instead of 14–17. And id's gate is not an even 60 on the
  phone's 120 Hz: its 1/72 s lands between refreshes, and the panel's
  refreshes and the relaxed pacing's back-to-back asks come unevenly, so
  in touch play it showed about 270 gaps of more than 20 ms in 45 s, the fast
  core 3–5% busy (2026-10-04). The frame-rate cap's 60 (`host_maxfps`, a
  slop option: "A frame") draws a picture on the first refresh at least
  1/60 s after the last — every second refresh of a 120 Hz panel, evenly —
  while the game still runs every refresh. A touch screen started at it for
  a day; since the user tried it (2026-10-04) slop starts with no cap on
  every machine, and 60 is a player's pick. What it does on the phone is for
  a phone run to say: `with.sh phone NAME -- uv run --with playwright
  web/phone.py DEPLOY --fullscreen --touch --px 2,1 --threads 4 --cvar
  host_maxfps=60,0 --secs 45 --cool 120`.

Back to back (`timedemo demo1`), cool, 8 / 6 / 4 threads: 323 / 375 / 367
fps at 1320×540; 148 / 162 / 155 at 2640×1080 with exact perspective; 182 /
– / 194 with `r_perspspan 16`. Warm (1171/1478 MHz) on 8 threads: 69 exact,
85 with span 16.

In play, warm, before this round (main `263eb54`) → after (the page's two
changes and Auto's four threads, below), median frame ms ; frames shown a
second ; frames more than 20 ms apart in 45 s:

| | 1320×540 (Auto) | 2640×1080, exact perspective |
|---|---|---|
| no finger, 60 Hz | 11 ; 59 ; 3 → the same | 18.3 ; 50 ; 365 → 14.6 ; 57 ; 133 |
| a finger moving, 120 Hz | 7.0–8.6 ; 91–105 ; 70–79 → 6.7–7.1 ; 114–115 ; 10–13 | 16.0–18.3 ; 50–58 ; 449–798 → 13.2–14.2 ; 70–74 ; 41–76 |

(Main's page and program and this branch's in turn, a kit's tab each, real
fullscreen; the no-finger row is the page's changes alone on 8 threads.)
With `r_perspspan 16` the 2640×1080 frame in touch play is 11.7–12.1 ms and
82–84 are shown a second, against 14.4 and 69 exact in the same minutes.

A day later (2026-10-04), after main's own renderer round (one round of
threads a frame; the default span now 8), the same comparison in touch play —
main `b3a773d` → this branch with main merged in, a kit tab each, in turn
twice — frames shown a second ; frames more than 20 ms apart in 45 s ;
median frame ms:

| | 1320×540 | 2640×1080 |
|---|---|---|
| exact perspective | 102–114 ; 31–134 ; 4.4–6.1 → 117 ; 11–12 ; 6.0–6.2 | 64 ; 251–277 ; 15.1 → 85–86 ; 17–19 ; 11.2–11.5 |
| `r_perspspan 8` (the default) | 113–114 ; 26–27 ; 5.5 → 116 ; 10–11 ; 5.1–5.5 | 77–84 ; 51–70 ; 10.3–12.0 → 96–100 ; 13–16 ; 9.2–9.8 |

Main's round shows on the phone (2640×1080 exact was 50–58 a second on
`263eb54`), and this round's changes pay on top of it as before: the
frame's 99th percentile at 1320×540 goes from 16–24 ms to 8.5–8.9.

- **What changed it.** On a touch screen the page no longer spins for a
  frame of most of a refresh or more ("A frame"): the spin sat on the fast
  core, 97–99% busy, while the game drew on the others. The upload's order ("Presentation"): 2.3 → 0.3 ms of the
  main thread at 2640×1080. And **a phone draws on four threads, not
  eight** (`settings::Machine::TOUCH_THREADS`): in touch play, 8 → 4 threads
  took 2640×1080 from 67 to 70–74 frames shown a second and the late ones
  from 116 to 41–76, and at 1320×540 the frame's 99th percentile from 20.7
  ms to 9.5 — the 20 ms hitches twice a second were a band's thread put
  off a core. Three threads are worse (53 a second at 2640×1080 against
  59–64), five and six no better than four (58–59), at both sizes.
- **Why four, and where.** The browser offers every core it sees, but a
  frame needs some of them for the page's own thread, the compositor, the
  GPU process and the sound; a phone's cores are of two or three kinds, of
  which four or five are fast on any current one; and every busy core is
  heat, which is paid back in clock. So on a touch screen (`-touch`, the
  page's coarse-pointer test: a phone, and a tablet too) the machine starts
  `r_threads` at four of the threads offered at most; with fewer offered,
  those. Any other machine — a desktop, a laptop, a tablet whose primary
  pointer is fine — starts on every thread offered, and `r_threads N` is N
  anywhere. (Until 2026-10-04 the test was the screen's: `devicePixelRatio`
  2 or more in a box whose shorter side is at most 540 CSS px, which left a
  tablet on every thread; one test of the device now decides the touch
  controls, the pacing's lines and these numbers alike.) A rule with no device in it would be
  "half the threads offered": the same four here, and eight on this
  16-thread desktop, where eight and sixteen measure the same; but it would
  halve the threads of machines whose cores are all fast and unshared (an
  Apple silicon Pro, an x86 without SMT), which nothing here could
  measure, so it is not the rule.
- **Presenting from a worker** (the parked `fleet/present120`) was built
  on Chrome keeping a page's main-thread refresh at 60 on a 120 Hz panel.
  That holds only with no finger down; in touch play the main thread
  already gets the panel's rate, so for the rate a worker would add 120 Hz
  only to a demo nobody touches or a gamepad, at the cost of the heat
  above. What a worker would give touch play is the other thing: a wait
  that sleeps (`Atomics.wait`) instead of spinning, so a slow frame could
  be drawn the moment it is done without a core kept busy for it — the
  refresh on the glass that not waiting costs ("A frame"). Not built.
- **Not measured:** the delay from a touch to the glass. By the
  compositor's swap in a headless trace ("A frame"), a frame longer than a
  refresh is a refresh later on the glass when it is not waited for: 8 ms
  at the 120 Hz of touch play, against frames 3–4 ms shorter and 12–24
  more of them a second; `?wait` in the address plays the old way, to
  compare by feel. Also not measured: real play (the finger is `adb shell
  input swipe`, the game is demo1); other phones; a long session's
  battery.

**`web/phone.py`** is the kit: one command, a table — for each pixel size
and thread count (and one more cvar's values: `--cvar r_perspspan=16,1`),
`timedemo demo1`, then a stretch of `playdemo demo1` at the page's own
pace, with each kind of core's clock, cap and load and the skin temperature
read through adb beside the page's numbers (the frame's time, the frames
shown, the gaps, the upload; the program's phases on a bench build). It
measures the tab already open in the phone's Chrome and puts its cvars
back, or serves a deploy dir to the phone over `adb reverse`
(`http://localhost` is a secure context, so the threads build runs) in a
tab it closes after, or runs against the local Chromium (`--local`).
`--cool` rests the phone before each timedemo until no core is capped;
`--touch` keeps a finger moving on the screen through every row (`adb shell
input swipe`, in the middle of the picture, where a demo ignores it), and
every row says the panel's refresh as SurfaceFlinger reports it. Three
things it learned the hard way: a row with no finger measures a 60 Hz game
the player never has; a page given part of the screen measures a
smaller frame, so the device's state is read with every row and anything but
the whole screen is refused; and Chrome hides its bars for a page's fullscreen
only while no DevTools client is attached, so `--fullscreen` disconnects,
sends the page's own Alt+Enter as a key event from Android, and connects
again. A baseline of both pixel sizes at three thread counts with a minute
of play each takes about 15 minutes of the phone, and it must be awake and
unlocked with Chrome in front (a moving finger keeps it awake; with none it
sleeps at its own timeout).

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
`Atomics.wait` in a worker. Checked here (2026-10-03): headless Chromium 153
(Playwright's) and headless Firefox 155, each all 20 `verify_*.py` (Firefox:
`QUAKE_BROWSER=firefox`; "Build, serve, deploy" lists what differs and why),
`verify_threads.py` in both, the benchmark in Chromium; Firefox's 17
deploy-dir checks on the threads build, and 16 of them (all but
`verify_crash.py`, which is about the threads build's own refusals) with a
display for its WebGL2 too, and on `wasm32-wasip1` with one. Headed, on the Wayland compositor's virtual output with real input
through XTEST: Chromium 146 (2026-10-02) and Firefox 155 ("Fullscreen",
"Input"); in Firefox also the wheel, a save across a reload, and the sound's
start: under
its strictest autoplay setting (Block Audio and Video: `getAutoplayPolicy`
says `disallowed` before a gesture) a real click, Enter or Space on the first
screen leaves the context running a second later, the ring played and no
underruns. WebGL2 is optional: without it the page presents through the 2-D
canvas. Playwright's WebKit would not start here (missing system
libraries). A GPU is checked through headless Chromium (`QUAKE_GPU=1`: the
local GPU, ANGLE on GL; `verify_present.py` and the benchmark) and
through Firefox with a display (WebGL2 on the same GPU, shared views
taken). Not checked: Safari, iOS, Firefox on Windows or macOS, a GPU driving a
real high-refresh display. From the platforms' documentation, not from a
run: Safari has `SharedArrayBuffer` under COOP/COEP since 15.2 (iOS 15.2),
with `Atomics.wait` in workers, and WebAssembly SIMD since 16.4 (iOS 16.4,
March 2023), which makes 16.4 the floor; iOS has no pointer lock, and a
phone plays with the touch controls ("Touch"). The program's own memory no
longer holds the 18 MB pak, which helps where wasm memory is tight (iOS).

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

**A public demo** (shareware only): `web/publish.sh OUTDIR [PAK0.PAK]`
builds the threads program and assembles a new deploy dir: the page,
`quake.wasm`, `id1/pak0.pak` (refused unless its sha256 is id's unmodified
shareware pak's), id's shareware licence beside it as `id1/slicnse.txt`
when `SLICNSE.TXT` sits next to the pak's folder (`ci/fetch_shareware.sh`
leaves it there), no `files.json`, and a `_headers` file:

```
/*               Cross-Origin-Opener-Policy: same-origin
                 Cross-Origin-Embedder-Policy: require-corp
/id1/pak0.pak    Cache-Control: public, max-age=31536000, immutable
/quake.wasm      Cache-Control: no-cache
```

Cloudflare Pages and Netlify read `_headers`: a request gets every
matching rule's headers, and a `Cache-Control` there replaces Pages'
default (`public, max-age=0, must-revalidate`, which the page's other files
keep). Any other host works through the service worker. The pak is id's and
never changes, so a year; the program and the page change under the same
names with each deploy, so they revalidate on every load, a 304 when
nothing changed. Pages takes files up to 25 MiB; the pak is 17.8 MiB.
`web/isolated.py` is the checks' server. `uv run --with playwright
web/bench.py DEPLOYDIR` and `QUAKE_VERIFY_PORT=… uv run --with playwright
web/verify_walk.py DEPLOYDIR` take a deploy dir, and `bench.py --build`
assembles one under `quake-wasm/target/bench-web`.

**Firefox.** `QUAKE_BROWSER=firefox` runs any `verify_*.py` in Playwright's
Firefox (`isolated.launch`, which also gives it the autoplay preferences
Chromium takes as a flag; `webkit` does not start here). All 19 pass
(Firefox 155, 2026-10-03). What differs, each said by the script that
differs:

- `verify_extras` skips the Keyboard Lock checks, 17 of Chromium's 70: Firefox
  has no such API (its own no-API half runs).
- `verify_present` tests WebGL2 only where Firefox has one, and a headless
  Firefox has one only with a display to ask: set `DISPLAY` (here
  `env -u WAYLAND_DISPLAY DISPLAY=:0`, the Wayland compositor's X11 layer and the integrated GPU) and the
  default page is WebGL2, 30 checks; with none the page takes the 2-D canvas
  and 26 run. Every other check runs on whichever the page picked, so run the
  suite both ways.
- `verify_settings` skips its devicePixelRatio 2 section (3 checks): Playwright's
  Firefox loses a context's `device_scale_factor` on a cross-origin isolated
  page (a plain page keeps it), which is every page served here.
- `verify_touch` runs on taps only: 137 checks pass and 8 are skipped, each saying
  why (the stick, the look drag, two thumbs, FIRE, JUMP, a held menu-pad
  arrow: Playwright's Firefox touchscreen taps and does nothing else, and has
  no `isMobile`), at devicePixelRatio 1 (so its "@3" and "@2.6" checks are
  layout checks at 1).
- The rest run the same checks with the same counts. `verify_threads` and
  `verify_audio_resilience` pass in both, and `verify_timedemo` prints Firefox's
  own rate.

**Natively**, the same program runs on a pipe:
`cargo run --release -- -basedir <dir with id1/pak0.pak>` reads the records
on stdin and writes them on stdout (its tests drive it that way,
`sys.rs`).
