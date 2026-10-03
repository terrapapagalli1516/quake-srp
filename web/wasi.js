// quake-rs's WASI host: runs the game (quake.wasm, a wasm32-wasip1 program)
// inside this Web Worker. The program blocks here between frames, in its
// read of stdin, so it is an ordinary `fn main()` loop: the page writes its
// events into a shared ring (Atomics.wait wakes the read), and what the
// program writes to stdout comes back to the page — each frame's pixels into
// shared frame slots the page presents from (or, when the program's memory
// is shared, the frame's place in it), its sound into a shared ring the
// page's AudioWorklet plays, everything else (UI state, the sound's counts,
// answers to calls) as one message per turn. Files are a small
// in-memory file system the page filled from its storage; what the program
// writes goes back to the page to keep. web/PLATFORM.md has the protocol and
// the shared-memory layout; quake-wasm/src/proto.rs is the program's side.
//
// The worker's own event loop never runs again once the program starts (it
// never returns), so nothing can reach it by postMessage after `init`:
// everything the page sends goes through the shared ring.
//
// A program built for wasm32-wasip1-threads imports a shared `env.memory`
// and `wasi.thread-spawn`: each of its threads runs in another worker
// running this file (`threads` below).
'use strict';

// --- Shared memory layout (web/PLATFORM.md, "Shared memory") -------------
// The control block: an Int32Array over the first CTL_BYTES of `shared`.
const C = {
  IN_WRITE: 0,     // bytes the page has written into the input ring (wraps)
  IN_READ: 1,      // bytes the program has read
  ACK: 2,          // the last tick the program consumed (its Sync's seq)
  SYNCS: 3,        // Syncs so far: the page waits on this
  LATEST: 4,       // the frame slot holding the newest complete frame (-1: none)
  FRAMES: 5,       // frames published so far
  READING: 6,      // the slot the page is presenting from (-1: none)
  RUN: 7,          // 0 starting, 1 running, 2 exited, 3 crashed
  WAIT: 8,         // 1: the program waits for ticks; 0: it polls (timedemo)
  SLOT_W: 9,       // + slot: width, height, format of each slot's frame
  SLOT_H: 12,
  SLOT_F: 15,
  SHOWN: 18,       // FRAMES as of the page's last present
  SLOTS_GEN: 19,   // which set of frame slots LATEST and the SLOT_* fields are about
  SLOT_SRC: 20,    // + slot: 0 the frame is in the frame slots; 1 in the program's memory
  SLOT_ADDR: 23,   // + slot: in the program's memory, where the pixels lie
  SLOT_PAL: 26,    // + slot: ... and the palette (an indexed frame)
};
const CTL_BYTES = 256;
const RING_BYTES = 1 << 16;              // the input ring, after the control block
const SLOTS = 3;                         // frame slots (triple buffering), and the program's ring
// The sound ring (web/PLATFORM.md, "Sound"): made by the page, written here,
// played by the page's AudioWorklet. The control block's Int32 fields:
const A = { POS: 0, WRITE: 1, RATE: 2, UNDER: 3, PLAYED: 4, CLEARS: 5, PEAK: 6, QUANTA: 7 };
const AUDIO_CTL_BYTES = 64, RING_PAIRS = 32768;

// --- Protocol constants (quake-wasm/src/proto.rs) ---------------------------
const IN_END = 8, IN_AUDIO_WAKE = 11;
const OUT_FRAME = 1, OUT_SYNC = 2, OUT_PCM = 14, OUT_FRAME_AT = 17;
const PCM_CLEAR = 1;

// --- WASI errno values (wasi_snapshot_preview1) ------------------------------
const E = { SUCCESS: 0, BADF: 8, EXIST: 20, INVAL: 28, ISDIR: 31, NOENT: 44, NOSYS: 52, NOTDIR: 54, SPIPE: 70 };
const FILETYPE_DIR = 3, FILETYPE_FILE = 4, FILETYPE_CHAR = 2;
const O_CREAT = 1, O_DIRECTORY = 2, O_EXCL = 4, O_TRUNC = 8;
const FDFLAG_APPEND = 1;
const RIGHT_FD_WRITE = 1n << 6n;

class Exit { constructor(code) { this.code = code; } }

let memory;                              // the program's WebAssembly.Memory
let ctl, ring;                           // views on the shared control block and ring
// The frame slots: made here, as large as the largest frame so far, and
// made anew (and sent to the page) when a frame outgrows them.
let slots = null, slotBytes = 0, slotsGen = 0;

onmessage = async (e) => {
  if (e.data.t === 'hello') { postMessage({ t: 'ready' }); return; }   // a thread worker, made
  if (e.data.t === 'thread') { runThread(e.data); return; }
  if (e.data.t !== 'init') return;
  const { wasm, shared, audio: audioShared, files, args, crash } = e.data;
  ctl = new Int32Array(shared, 0, CTL_BYTES / 4);
  threads.shared = shared;
  threads.crash = crash || null;
  ring = new Uint8Array(shared, CTL_BYTES, RING_BYTES);
  if (audioShared) sound.init(audioShared);
  for (const [path, data] of files) fs.files.set(path, { data, size: data.length });
  let module;
  try {
    module = await WebAssembly.compile(wasm);
  } catch (err) {
    finish(3, 'compile: ' + err);
    return;
  }
  let shared_memory;
  try {
    shared_memory = importedMemory(new Uint8Array(wasm));
  } catch (err) {
    // The browser would not make the memory the program declares (the
    // threads build's fixed size, PLATFORM.md "Threads"): the game cannot
    // run here. Said once, plainly, as a Sys_Error would be.
    const mb = memoryLimits(new Uint8Array(wasm)).initial * 64 / 1024;
    stderr.line(`quake: this browser would not give the game the ${mb} MB of memory it needs to run (${err && err.message || err})`);
    finish(3, 'memory: ' + (err && err.message || err));
    return;
  }
  let argv = args || [];
  if (shared_memory && shared_memory.buffer instanceof SharedArrayBuffer) {
    // The page can read the program's memory: frames stay where the program
    // drew them (FRAME_AT), and the page reads them there.
    argv = [...argv, '-sharedframes'];
    postMessage({ t: 'memory', memory: shared_memory });
  }
  if (WebAssembly.Module.imports(module).some(i => i.module === 'wasi' && i.name === 'thread-spawn')) {
    try {
      await threads.start(module, shared_memory);
    } catch (err) {
      // The browser would not make the workers the threads build runs its
      // threads in: the game does not run here — said once, plainly, like
      // the refused memory above, and not on one thread instead (a game
      // that quietly runs slower, in a mode nobody chose, is not a thing
      // to keep track of: PLATFORM.md "Threads").
      stderr.line(`quake: this browser would not start the worker threads the game needs to run (${err && err.message || err})`);
      finish(3, 'threads: ' + (err && err.message || err));
      return;
    }
    // The threads it may use: the pool's and its own (quake-wasm's main.rs).
    argv = [...argv, '-hwthreads', String(threads.offer())];
  }
  const inst = await WebAssembly.instantiate(module, importObject(module, argv, shared_memory));
  memory = shared_memory || inst.exports.memory;
  Atomics.store(ctl, C.RUN, 1);
  try {
    inst.exports._start();
    finish(2, 'exit 0');
  } catch (err) {
    if (err instanceof Exit) finish(err.code === 0 ? 2 : 3, 'exit ' + err.code);
    else finish(3, String(err && err.stack || err));
  }
};

// The program ended (returned, exited or trapped): say so, and wake a page
// that may be waiting for its next Sync.
function finish(run, why) {
  stderr.flush();
  Atomics.store(ctl, C.RUN, run);
  Atomics.add(ctl, C.SYNCS, 1);
  Atomics.notify(ctl, C.SYNCS);
  postMessage({ t: 'exit', run, why });
}

// Views on the program's memory, re-made when it grows.
function u8() { return new Uint8Array(memory.buffer); }
function dv() { return new DataView(memory.buffer); }
function str(ptr, len) { return new TextDecoder().decode(u8().slice(ptr, ptr + len)); }

// The (ptr, len) pairs of an iovec array.
function iovecs(iovs, n) {
  const d = dv(), out = [];
  for (let i = 0; i < n; i++) out.push([d.getUint32(iovs + 8 * i, true), d.getUint32(iovs + 8 * i + 4, true)]);
  return out;
}

// --- stdin: the page's events -----------------------------------------------
// The ring's bytes are whole records (the page publishes each one at once).
// After a polling Sync the program reads until an End record: the host
// hands it one as soon as the ring is empty, instead of blocking.
const stdin = {
  pendingEnd: null,                      // the End record still to hand out
  pendingWake: null,                     // an AudioWake still to hand out (sound.wake)
  read(dst, cap) {
    for (;;) {
      const w = Atomics.load(ctl, C.IN_WRITE), r = Atomics.load(ctl, C.IN_READ);
      const avail = (w - r) | 0;
      if (avail > 0) {
        const n = Math.min(avail, cap), at = r & (RING_BYTES - 1);
        const first = Math.min(n, RING_BYTES - at);
        const m = u8();
        m.set(ring.subarray(at, at + first), dst);
        if (n > first) m.set(ring.subarray(0, n - first), dst + first);
        Atomics.store(ctl, C.IN_READ, (r + n) | 0);
        return n;
      }
      for (const k of ['pendingEnd', 'pendingWake']) {
        const b = this[k];
        if (!b) continue;
        const n = Math.min(b.length, cap);
        u8().set(b.subarray(0, n), dst);
        this[k] = n < b.length ? b.subarray(n) : null;
        return n;
      }
      if (Atomics.wait(ctl, C.IN_WRITE, w, sound.wakeMs()) === 'timed-out') this.pendingWake = sound.wake();
    }
  },
};

// --- The sound ring ------------------------------------------------------------
// A PCM record's samples go straight from the program's memory into the
// ring, at the pairs its `start` names; the ring's WRITE then says how far
// they reach. A record that asks for it (S_ClearBuffer) silences the ring
// first: what was mixed ahead of the device before a level change.
const sound = {
  ctl: null, data: null,                 // the control block and the ring's bytes
  at: 0,                                 // the byte the next sample byte goes to
  start: 0, bytes: 0,                    // the current record's first pair, and its bytes so far
  init(sab) {
    this.ctl = new Int32Array(sab, 0, AUDIO_CTL_BYTES / 4);
    this.data = new Uint8Array(sab, AUDIO_CTL_BYTES, RING_PAIRS * 4);
  },
  begin(start, rate, flags) {
    if (!this.ctl) return;
    if (flags & PCM_CLEAR) { this.data.fill(0); Atomics.add(this.ctl, A.CLEARS, 1); }
    if (Atomics.load(this.ctl, A.RATE) !== rate) Atomics.store(this.ctl, A.RATE, rate);
    this.start = start; this.bytes = 0;
    this.at = (start & (RING_PAIRS - 1)) * 4;
  },
  write(src) {
    if (!this.ctl) return;
    for (let i = 0; i < src.length;) {
      const k = Math.min(src.length - i, this.data.length - this.at);
      this.data.set(src.subarray(i, i + k), this.at);
      this.at = (this.at + k) % this.data.length;
      i += k;
    }
    this.bytes += src.length;
  },
  end() {
    if (this.ctl) Atomics.store(this.ctl, A.WRITE, (this.start + (this.bytes >> 2)) | 0);
  },
  // Between the page's ticks the program waits in a read; while the
  // worklet plays, the read wakes every WAKE_MS and hands it an AUDIO_WAKE
  // with the device's position, so it mixes again (id's S_ExtraUpdate): the
  // ring stays fed whatever the display's refresh does.
  WAKE_MS: 8,
  quanta: -1,
  wakeMs() { return this.ctl && Atomics.load(this.ctl, A.RATE) ? this.WAKE_MS : Infinity; },
  wake() {
    if (!this.ctl) return null;
    const q = Atomics.load(this.ctl, A.QUANTA);
    if (q === this.quanta) return null;        // the worklet is not playing
    this.quanta = q;
    const rec = new Uint8Array(8);
    rec[0] = IN_AUDIO_WAKE; rec[2] = 4;
    new DataView(rec.buffer).setUint32(4, Atomics.load(this.ctl, A.POS) >>> 0, true);
    return rec;
  },
};

// --- stdout: the program's records ------------------------------------------
// A streaming parser, since a record can span writes: a FRAME's pixels (an
// indexed frame's palette first) go straight from the program's memory into
// a free frame slot, a PCM record's samples into the sound ring; a FRAME_AT
// says where a frame lies in the program's shared memory, and the page reads
// it there; every other record is kept, and the lot goes to the page as one
// message at each Sync.
const stdout = {
  head: new Uint8Array(8), headN: 0,     // the record header being read
  kind: 0, left: 0,                      // the current record, and its bytes still to come
  fixed: new Uint8Array(16), fixedN: 0,  // a FRAME's (8), PCM's (12) or FRAME_AT's (16) fixed fields
  slot: -1, slotAt: 0,                   // where a FRAME's pixels are going
  batch: new Uint8Array(1 << 16), batchN: 0,

  write(src, n) {
    const m = u8();
    let i = 0;
    while (i < n) {
      if (this.headN < 8) {                  // the 8-byte header
        const k = Math.min(8 - this.headN, n - i);
        this.head.set(m.subarray(src + i, src + i + k), this.headN);
        this.headN += k; i += k;
        if (this.headN < 8) break;
        this.kind = this.head[0];
        this.left = new DataView(this.head.buffer).getUint32(4, true);
        this.fixedN = 0; this.slot = -1;
        if (this.kind !== OUT_FRAME && this.kind !== OUT_PCM && this.kind !== OUT_FRAME_AT) this.keep(this.head, 0, 8);
        if (this.left === 0) this.done();
        continue;
      }
      const k = Math.min(this.left, n - i);
      if (this.kind === OUT_FRAME) this.framePart(m, src + i, k);
      else if (this.kind === OUT_PCM) this.pcmPart(m, src + i, k);
      else if (this.kind === OUT_FRAME_AT) this.fixedPart(m, src + i, k, 16);
      else this.keep(m, src + i, k);
      this.left -= k; i += k;
      if (this.left === 0) this.done();
    }
  },

  // Append bytes to this turn's batch.
  keep(from, at, k) {
    if (this.batchN + k > this.batch.length) {
      const bigger = new Uint8Array(Math.max(this.batch.length * 2, this.batchN + k));
      bigger.set(this.batch.subarray(0, this.batchN));
      this.batch = bigger;
    }
    this.batch.set(from.subarray(at, at + k), this.batchN);
    this.batchN += k;
  },

  // Up to `n` fixed bytes of a record's payload; returns how many of `k` it took.
  fixedPart(m, at, k, n) {
    const f = Math.min(Math.max(n - this.fixedN, 0), k);
    this.fixed.set(m.subarray(at, at + f), this.fixedN);
    this.fixedN += f;
    return f;
  },

  // Some of a FRAME's payload: its 8 fixed bytes, then the palette (an
  // indexed frame) and the pixels, into a frame slot.
  framePart(m, at, k) {
    if (this.fixedN < 8) {
      const f = this.fixedPart(m, at, k, 8);
      at += f; k -= f;
      if (this.fixedN === 8) this.slot = this.wanted() ? freeSlot(this.left - f) : -1;
      this.slotAt = 0;
    }
    if (k > 0 && this.slot >= 0) {
      slots.set(m.subarray(at, at + k), this.slot * slotBytes + this.slotAt);
      this.slotAt += k;
    }
  },

  // Some of a PCM record's payload: its 12 fixed bytes, then samples.
  pcmPart(m, at, k) {
    if (this.fixedN < 12) {
      const f = this.fixedPart(m, at, k, 12);
      at += f; k -= f;
      if (this.fixedN === 12) {
        const d = new DataView(this.fixed.buffer);
        sound.begin(d.getUint32(0, true) | 0, d.getUint32(4, true), d.getUint32(8, true));
      }
    }
    if (k > 0) sound.write(m.subarray(at, at + k));
  },

  // Whether the page wants this frame copied. It always does while the
  // program waits for its ticks (the page is waiting for the frame). A
  // timedemo's frames come faster than any display: one the page has not yet
  // shown the last of is rendered but not copied over (the old page likewise
  // ran a slice of frames per refresh and presented the last).
  wanted() {
    return Atomics.load(ctl, C.WAIT) === 1 || Atomics.load(ctl, C.FRAMES) === Atomics.load(ctl, C.SHOWN);
  },

  // A record is complete.
  done() {
    this.headN = 0;
    if (this.kind === OUT_FRAME && this.slot >= 0) {
      const f = new DataView(this.fixed.buffer);
      publish(this.slot, f, 0, 0, 0);
    } else if (this.kind === OUT_FRAME_AT) {
      this.frameAt(new DataView(this.fixed.buffer));
    } else if (this.kind === OUT_PCM) {
      sound.end();
    } else if (this.kind === OUT_SYNC) {
      this.sync();
    }
  },

  // A FRAME_AT: the frame lies in ring slot `slot` of the program's shared
  // memory. Always published (it costs no copy, and the newest frame must
  // be the ring's newest: a timedemo's unshown frames too), and the
  // program's next frame takes the ring's next slot, so this returns only
  // once the page is not reading that one (the page claims only the newest
  // frame, so once this one is published it cannot start to).
  frameAt(f) {
    const slot = f.getUint8(5);
    publish(slot, f, 1, f.getUint32(8, true), f.getUint32(12, true));
    const next = (slot + 1) % SLOTS;
    while (Atomics.load(ctl, C.READING) === next) Atomics.wait(ctl, C.READING, next, 50);
  },

  // A Sync: publish the turn — the batch to the page, the tick's ack, and
  // whether the next read may block.
  sync() {
    const at = this.batchN - 8 - 5;        // the Sync record is the batch's last
    const d = new DataView(this.batch.buffer, this.batch.byteOffset + at + 8, 5);
    const seq = d.getUint32(0, true), wait = d.getUint8(4);
    const out = this.batch.slice(0, this.batchN);
    this.batchN = 0;
    postMessage({ t: 'out', bytes: out.buffer }, [out.buffer]);
    stdin.pendingEnd = wait ? null : new Uint8Array([IN_END, 0, 0, 0]);
    Atomics.store(ctl, C.WAIT, wait);
    Atomics.store(ctl, C.ACK, seq);
    Atomics.add(ctl, C.SYNCS, 1);
    Atomics.notify(ctl, C.SYNCS);
  },
};

// Publish frame slot `slot` as the newest frame: its size and format (the
// FRAME's fixed fields in `f`), where it is (`src` 0: the frame slots; 1: the
// program's memory at `addr`, its palette at `pal`).
function publish(slot, f, src, addr, pal) {
  Atomics.store(ctl, C.SLOT_W + slot, f.getUint16(0, true));
  Atomics.store(ctl, C.SLOT_H + slot, f.getUint16(2, true));
  Atomics.store(ctl, C.SLOT_F + slot, f.getUint8(4));
  Atomics.store(ctl, C.SLOT_SRC + slot, src);
  Atomics.store(ctl, C.SLOT_ADDR + slot, addr | 0);
  Atomics.store(ctl, C.SLOT_PAL + slot, pal | 0);
  Atomics.store(ctl, C.LATEST, slot);
  Atomics.add(ctl, C.FRAMES, 1);
}

// A slot to write the next frame (`bytes` long) into: neither the newest
// frame (the page may be about to present it) nor the one the page is
// presenting. A frame larger than the slots gets a new, larger set, which
// goes to the page; until the page has it, it presents nothing (SLOTS_GEN).
function freeSlot(bytes) {
  if (bytes > slotBytes) {
    slotBytes = Math.ceil(bytes / 65536) * 65536;
    const frames = new SharedArrayBuffer(SLOTS * slotBytes);
    slots = new Uint8Array(frames);
    Atomics.store(ctl, C.LATEST, -1);
    Atomics.store(ctl, C.SLOTS_GEN, ++slotsGen);
    postMessage({ t: 'slots', frames, slotBytes, gen: slotsGen });
  }
  const latest = Atomics.load(ctl, C.LATEST), reading = Atomics.load(ctl, C.READING);
  for (let s = 0; s < SLOTS; s++) if (s !== latest && s !== reading) return s;
  return -1;
}

// stderr: the program's own messages, a line at a time to the page's console
// (from a thread, to this worker's console: its parent never reads again).
const stderr = {
  text: '',
  write(src, n) {
    this.text += new TextDecoder().decode(u8().slice(src, src + n));
    let nl;
    while ((nl = this.text.indexOf('\n')) >= 0) {
      this.line(this.text.slice(0, nl));
      this.text = this.text.slice(nl + 1);
    }
  },
  line(text) {
    if (threads.self) console.log(`[quake thread ${threads.self}]`, text);
    else postMessage({ t: 'log', text });
  },
  flush() { if (this.text) this.line(this.text); this.text = ''; },
};

// --- Threads (wasm32-wasip1-threads) ------------------------------------------
// Each thread is a worker running this file, on the program's shared memory:
// `thread-spawn` hands one the thread's start argument, and it calls the
// module's `wasi_thread_start`. The workers are made before the program
// starts (a worker made while its parent is blocked may never start) and
// reused as threads end; `busy` marks the ones running a thread. A worker
// instantiates the module once, on its first thread, and then waits on its
// own slot of `jobs` (`[seq, tid, arg]`) with Atomics.wait: a later
// `thread-spawn` writes the thread there and bumps `seq`, so a program that
// starts threads every frame (the renderer's bands) costs a wake-up per
// thread, not a message and an instantiation. A thread has the clocks,
// randomness, sleep and stderr; the files, stdin and stdout are the main
// program's. A thread cannot spawn threads yet.
const JOB = 3;                  // Int32s per worker in `jobs`: seq, tid, arg
const threads = {
  pool: [],
  busy: null,
  jobs: null,
  started: [],             // per worker: has it had its first thread (and an instance)?
  module: null,
  memory: null,
  next: 1,                 // thread ids (the main thread is 0)
  self: 0,                 // in a thread's worker: its id
  async start(module, memory) {
    const n = Math.max(2, Math.min(16, navigator.hardwareConcurrency || 4));
    this.busy = new Int32Array(new SharedArrayBuffer(4 * n));
    this.jobs = new Int32Array(new SharedArrayBuffer(4 * JOB * n));
    this.started = new Array(n).fill(false);
    this.module = module;
    this.memory = memory;
    try {
      await Promise.all(Array.from({ length: n }, () => new Promise((resolve, reject) => {
        const w = new Worker(self.location.href);
        w.onmessage = (e) => { if (e.data.t === 'ready') resolve(); };
        w.onerror = (e) => reject(new Error('a thread worker failed: ' + e.message));
        w.postMessage({ t: 'hello' });
        this.pool.push(w);
      })));
    } catch (err) {
      // Not a pool the program can use: none of it stays (the caller stops).
      for (const w of this.pool) w.terminate();
      this.pool = [];
      throw err;
    }
  },
  // The threads the program may count on: this machine's, at most the pool
  // plus the program's own (the player's `r_threads` goes lower, never
  // higher).
  offer() {
    return Math.max(1, Math.min(navigator.hardwareConcurrency || 1, this.pool.length + 1));
  },
  // wasi.thread-spawn: a positive thread id, or a negative errno.
  spawn(arg) {
    for (let i = 0; i < this.pool.length; i++) {
      if (Atomics.compareExchange(this.busy, i, 0, 1) === 0) {
        const tid = this.next++;
        if (!this.started[i]) {
          this.started[i] = true;
          this.pool[i].postMessage({ t: 'thread', module: this.module, memory: this.memory,
                                     tid, arg, busy: this.busy, jobs: this.jobs, slot: i,
                                     shared: this.shared, crash: this.crash });
        } else {
          Atomics.store(this.jobs, JOB * i + 1, tid);
          Atomics.store(this.jobs, JOB * i + 2, arg);
          Atomics.add(this.jobs, JOB * i, 1);
          Atomics.notify(this.jobs, JOB * i);
        }
        return tid;
      }
    }
    return -6;                                   // EAGAIN: every worker is busy
  },
};

// A thread trapped (a panic aborts, so it traps too): the program cannot go
// on — whoever joins the thread would wait for ever, and the page with it.
// End it as the main thread's trap ends it: the run state crashed (the
// page stops asking for frames), every Sync waiter woken, and the reason
// to the page on its own channel (the program's worker, this worker's
// parent, may be blocked in that join and read no messages); the page then
// stops the program's worker, and its threads with it.
function threadCrashed(tid, err, crash) {
  stderr.flush();
  if (ctl) {
    Atomics.store(ctl, C.RUN, 3);
    Atomics.add(ctl, C.SYNCS, 1);
    Atomics.notify(ctl, C.SYNCS);
  }
  if (crash && typeof BroadcastChannel === 'function') {
    const ch = new BroadcastChannel(crash);
    ch.postMessage({ t: 'exit', run: 3, why: `a thread stopped (${tid}): ` + String(err && err.stack || err) });
    ch.close();
  }
}

// In a thread's worker: instantiate the module, run the thread, free the
// worker, and wait for the next thread `spawn` puts in this worker's slot.
async function runThread({ module, memory: mem, tid, arg, busy, jobs, slot, shared, crash }) {
  memory = mem;
  if (shared) ctl = new Int32Array(shared, 0, CTL_BYTES / 4);
  let inst;
  try {
    inst = await WebAssembly.instantiate(module, importObject(module, [], mem));
  } catch (err) {
    console.error('[quake thread] instantiate', err);
    Atomics.store(busy, slot, 0);
    return;
  }
  let seq = Atomics.load(jobs, JOB * slot);
  for (;;) {
    threads.self = tid;
    try {
      inst.exports.wasi_thread_start(tid, arg);
    } catch (err) {
      if (!(err instanceof Exit)) {
        console.error(`[quake thread ${tid}]`, err);
        threadCrashed(tid, err, crash);
      }
    }
    stderr.flush();
    Atomics.store(busy, slot, 0);
    // The next thread: `spawn` stores it and bumps `seq` (a thread handed
    // over between the store above and this wait finds `seq` moved).
    Atomics.wait(jobs, JOB * slot, seq);
    seq = Atomics.load(jobs, JOB * slot);
    tid = Atomics.load(jobs, JOB * slot + 1);
    arg = Atomics.load(jobs, JOB * slot + 2);
  }
}

// A module's imported `env.memory`'s limits, in pages ({ initial, maximum,
// shared }), or { initial: 0 } when it defines its own memory.
function memoryLimits(bytes) {
  let found = { initial: 0 };
  importedMemory(bytes, (lim) => { found = lim; return null; });
  return found;
}

// A module's imported `env.memory`, made shared with the limits the module
// declares (the JS API does not tell them, so they come from the import
// section), or null when it defines its own memory. `make` (the limits
// to a memory) is the WebAssembly.Memory constructor by default.
function importedMemory(bytes, make = (lim) => new WebAssembly.Memory(lim)) {
  let i = 8;
  const leb = () => { let r = 0, sh = 0, b; do { b = bytes[i++]; r += (b & 0x7f) * 2 ** sh; sh += 7; } while (b & 0x80); return r; };
  const name = () => { const n = leb(); i += n; return new TextDecoder().decode(bytes.subarray(i - n, i)); };
  while (i < bytes.length) {
    const id = bytes[i++], size = leb(), end = i + size;
    if (id === 2) {                               // the import section
      for (let n = leb(); n > 0; n--) {
        const mod = name(), field = name(), kind = bytes[i++];
        if (kind === 0) leb();                                   // a function: its type
        else if (kind === 1) { i++; if (bytes[i++] & 1) leb(); leb(); }   // a table
        else if (kind === 3) i += 2;                             // a global
        else if (kind === 4) { i++; leb(); }                     // a tag
        else if (kind === 2) {                                   // a memory
          const flags = bytes[i++], min = leb(), max = flags & 1 ? leb() : undefined;
          if (mod === 'env' && field === 'memory') {
            return make({ initial: min, maximum: max, shared: !!(flags & 2) });
          }
        }
      }
      return null;
    }
    i = end;
  }
  return null;
}

// --- The file system ---------------------------------------------------------
// One directory tree in memory, keyed by relative path ("id1/s0.sav"); a
// directory exists when a file lives under it. fd 3 is the preopened root,
// so the program's relative paths resolve against it (wasi-libc's cwd is /).
const fs = {
  files: new Map(),                      // path -> { data: Uint8Array, size }
  fds: new Map([[3, { dir: '' }]]),      // fd -> { dir } | { path, pos, write, append, dirty }
  next: 4,

  isDir(path) {
    if (path === '') return true;
    const p = path + '/';
    for (const k of this.files.keys()) if (k.startsWith(p)) return true;
    return false;
  },
  // `name` under directory fd `dirfd`, normalised; null when it leaves the root.
  resolve(dirfd, name) {
    const base = this.fds.get(dirfd);
    if (!base || base.dir === undefined) return null;
    const parts = base.dir ? base.dir.split('/') : [];
    for (const seg of name.split('/')) {
      if (seg === '' || seg === '.') continue;
      if (seg === '..') { if (!parts.length) return null; parts.pop(); } else parts.push(seg);
    }
    return parts.join('/');
  },
  // A written file goes back to the page, which keeps it.
  persist(path) {
    const f = this.files.get(path);
    postMessage({ t: 'fs', op: 'write', path, data: f.data.slice(0, f.size) });
  },
};

function writeFilestat(buf, type, size) {
  const d = dv();
  for (let i = 0; i < 64; i += 8) d.setBigUint64(buf + i, 0n, true);
  d.setUint8(buf + 16, type);
  d.setBigUint64(buf + 24, 1n, true);          // nlink
  d.setBigUint64(buf + 32, BigInt(size), true);
}

// --- The imports ---------------------------------------------------------------
// Everything the module imports: WASI's functions, and for a threads build
// its shared memory and `thread-spawn`.
function importObject(module, args, sharedMemory) {
  const obj = { wasi_snapshot_preview1: imports(module, args) };
  if (sharedMemory) obj.env = { memory: sharedMemory };
  obj.wasi = { 'thread-spawn': threads.self ? () => -52 : (arg) => threads.spawn(arg) };
  return obj;
}

function imports(module, args) {
  const argv = ['quake', ...(args || [])];
  const enc = new TextEncoder();
  const wasi = {
    args_sizes_get(argc, size) {
      const d = dv();
      d.setUint32(argc, argv.length, true);
      d.setUint32(size, argv.reduce((n, a) => n + enc.encode(a).length + 1, 0), true);
      return E.SUCCESS;
    },
    args_get(ptrs, buf) {
      const d = dv(), m = u8();
      for (const a of argv) {
        const b = enc.encode(a);
        d.setUint32(ptrs, buf, true); ptrs += 4;
        m.set(b, buf); m[buf + b.length] = 0; buf += b.length + 1;
      }
      return E.SUCCESS;
    },
    environ_sizes_get(count, size) { dv().setUint32(count, 0, true); dv().setUint32(size, 0, true); return E.SUCCESS; },
    environ_get() { return E.SUCCESS; },
    clock_time_get(id, _precision, out) {
      // 0 realtime, 1 monotonic (and the CPU-time clocks, which the worker
      // cannot tell apart from it).
      const ns = id === 0 ? BigInt(Date.now()) * 1000000n
        : BigInt(Math.round((performance.timeOrigin + performance.now()) * 1e6));
      dv().setBigUint64(out, ns, true);
      return E.SUCCESS;
    },
    clock_res_get(_id, out) { dv().setBigUint64(out, 1000n, true); return E.SUCCESS; },
    random_get(buf, len) {
      const tmp = new Uint8Array(len);
      for (let i = 0; i < len; i += 65536) crypto.getRandomValues(tmp.subarray(i, Math.min(len, i + 65536)));
      u8().set(tmp, buf);
      return E.SUCCESS;
    },
    proc_exit(code) { throw new Exit(code); },
    sched_yield() { return E.SUCCESS; },
    poll_oneoff(subs, events, n, nevents) {
      // thread::sleep: wait out the longest clock subscription.
      const d = dv();
      let ms = 0;
      for (let i = 0; i < n; i++) {
        const s = subs + 48 * i;
        if (d.getUint8(s + 8) === 0) ms = Math.max(ms, Number(d.getBigUint64(s + 24, true)) / 1e6);
      }
      if (ms > 0) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
      for (let i = 0; i < n; i++) {
        const s = subs + 48 * i, e = events + 32 * i;
        d.setBigUint64(e, d.getBigUint64(s, true), true);
        d.setUint16(e + 8, 0, true);
        d.setUint8(e + 10, d.getUint8(s + 8));
      }
      d.setUint32(nevents, n, true);
      return E.SUCCESS;
    },

    fd_write(fd, iovs, n, written) {
      let total = 0;
      const f = fs.fds.get(fd);
      for (const [ptr, len] of iovecs(iovs, n)) {
        if (fd === 1) stdout.write(ptr, len);
        else if (fd === 2) stderr.write(ptr, len);
        else if (f && f.path !== undefined && f.write) fileWrite(f, ptr, len);
        else return E.BADF;
        total += len;
      }
      dv().setUint32(written, total, true);
      return E.SUCCESS;
    },
    fd_read(fd, iovs, n, nread) {
      let total = 0;
      if (fd === 0) {
        // One read of stdin fills what it can of the first buffer with room.
        for (const [ptr, len] of iovecs(iovs, n)) if (len > 0) { total = stdin.read(ptr, len); break; }
      } else {
        const f = fs.fds.get(fd);
        if (!f || f.path === undefined) return E.BADF;
        const file = fs.files.get(f.path);
        for (const [ptr, len] of iovecs(iovs, n)) {
          const k = Math.max(0, Math.min(len, file.size - f.pos));
          u8().set(file.data.subarray(f.pos, f.pos + k), ptr);
          f.pos += k; total += k;
          if (k < len) break;
        }
      }
      dv().setUint32(nread, total, true);
      return E.SUCCESS;
    },
    fd_seek(fd, offset, whence, out) {
      const f = fs.fds.get(fd);
      if (fd <= 2) return E.SPIPE;
      if (!f || f.path === undefined) return E.BADF;
      const size = fs.files.get(f.path).size;
      const base = whence === 0 ? 0 : whence === 1 ? f.pos : size;
      const pos = base + Number(offset);
      if (pos < 0) return E.INVAL;
      f.pos = pos;
      dv().setBigUint64(out, BigInt(pos), true);
      return E.SUCCESS;
    },
    fd_tell(fd, out) {
      const f = fs.fds.get(fd);
      if (!f || f.path === undefined) return E.BADF;
      dv().setBigUint64(out, BigInt(f.pos), true);
      return E.SUCCESS;
    },
    fd_close(fd) {
      const f = fs.fds.get(fd);
      if (!f || fd === 3) return E.BADF;
      fs.fds.delete(fd);
      if (f.dirty) fs.persist(f.path);
      return E.SUCCESS;
    },
    fd_sync() { return E.SUCCESS; },
    fd_datasync() { return E.SUCCESS; },
    fd_fdstat_get(fd, buf) {
      const f = fs.fds.get(fd);
      const type = fd <= 2 ? FILETYPE_CHAR : f ? (f.dir !== undefined ? FILETYPE_DIR : FILETYPE_FILE) : -1;
      if (type < 0) return E.BADF;
      const d = dv();
      d.setUint8(buf, type);
      d.setUint16(buf + 2, f && f.append ? FDFLAG_APPEND : 0, true);
      d.setBigUint64(buf + 8, 0xffffffffffffffffn, true);
      d.setBigUint64(buf + 16, 0xffffffffffffffffn, true);
      return E.SUCCESS;
    },
    fd_fdstat_set_flags() { return E.SUCCESS; },
    fd_filestat_get(fd, buf) {
      const f = fs.fds.get(fd);
      if (fd <= 2) { writeFilestat(buf, FILETYPE_CHAR, 0); return E.SUCCESS; }
      if (!f) return E.BADF;
      if (f.dir !== undefined) writeFilestat(buf, FILETYPE_DIR, 0);
      else writeFilestat(buf, FILETYPE_FILE, fs.files.get(f.path).size);
      return E.SUCCESS;
    },
    fd_filestat_set_size(fd, size) {
      const f = fs.fds.get(fd);
      if (!f || f.path === undefined || !f.write) return E.BADF;
      resize(fs.files.get(f.path), Number(size));
      fs.files.get(f.path).size = Number(size);
      f.dirty = true;
      return E.SUCCESS;
    },
    fd_prestat_get(fd, buf) {
      if (fd !== 3) return E.BADF;
      dv().setUint8(buf, 0);
      dv().setUint32(buf + 4, 1, true);            // "/"
      return E.SUCCESS;
    },
    fd_prestat_dir_name(fd, path, len) {
      if (fd !== 3) return E.BADF;
      if (len >= 1) u8()[path] = 0x2f;
      return E.SUCCESS;
    },
    path_open(dirfd, _dirflags, pathPtr, pathLen, oflags, rightsBase, _rightsInh, fdflags, out) {
      const path = fs.resolve(dirfd, str(pathPtr, pathLen));
      if (path === null) return E.NOENT;
      const exists = fs.files.has(path), dir = !exists && fs.isDir(path);
      if (oflags & O_DIRECTORY || dir) {
        if (exists) return E.NOTDIR;
        if (!dir) return E.NOENT;
        fs.fds.set(fs.next, { dir: path });
      } else {
        const write = (BigInt(rightsBase) & RIGHT_FD_WRITE) !== 0n;
        if (exists && oflags & O_CREAT && oflags & O_EXCL) return E.EXIST;
        if (!exists) {
          if (!(oflags & O_CREAT)) return E.NOENT;
          const parent = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
          if (!fs.isDir(parent)) return E.NOENT;
          fs.files.set(path, { data: new Uint8Array(0), size: 0 });
        }
        const f = { path, pos: 0, write, append: !!(fdflags & FDFLAG_APPEND), dirty: !exists };
        if (oflags & O_TRUNC && write) { fs.files.get(path).size = 0; f.dirty = true; }
        fs.fds.set(fs.next, f);
      }
      dv().setUint32(out, fs.next, true);
      fs.next++;
      return E.SUCCESS;
    },
    path_filestat_get(dirfd, _flags, pathPtr, pathLen, buf) {
      const path = fs.resolve(dirfd, str(pathPtr, pathLen));
      if (path !== null && fs.files.has(path)) writeFilestat(buf, FILETYPE_FILE, fs.files.get(path).size);
      else if (path !== null && fs.isDir(path)) writeFilestat(buf, FILETYPE_DIR, 0);
      else return E.NOENT;
      return E.SUCCESS;
    },
    path_unlink_file(dirfd, pathPtr, pathLen) {
      const path = fs.resolve(dirfd, str(pathPtr, pathLen));
      if (path === null || !fs.files.has(path)) return fs.isDir(path) ? E.ISDIR : E.NOENT;
      fs.files.delete(path);
      postMessage({ t: 'fs', op: 'unlink', path });
      return E.SUCCESS;
    },
  };
  // Whatever else this build of std imports answers "not supported", so a
  // newer program still starts; the first call of each is logged.
  const obj = {};
  for (const imp of WebAssembly.Module.imports(module)) {
    if (imp.module !== 'wasi_snapshot_preview1' || imp.kind !== 'function') continue;
    obj[imp.name] = wasi[imp.name] || ((...a) => {
      postMessage({ t: 'log', text: `wasi: ${imp.name} is not supported` });
      wasi[imp.name] = () => E.NOSYS;
      return E.NOSYS;
    });
  }
  return obj;
}

// Grow a file's buffer to hold `size` bytes.
function resize(file, size) {
  if (size <= file.data.length) return;
  const bigger = new Uint8Array(Math.max(size, file.data.length * 2, 4096));
  bigger.set(file.data.subarray(0, file.size));
  file.data = bigger;
}

// Write `len` bytes of the program's memory at the file's position.
function fileWrite(f, ptr, len) {
  const file = fs.files.get(f.path);
  if (f.append) f.pos = file.size;
  resize(file, f.pos + len);
  file.data.set(u8().subarray(ptr, ptr + len), f.pos);
  f.pos += len;
  file.size = Math.max(file.size, f.pos);
  f.dirty = true;
}
