//! A `std::thread::scope` round trip through the browser's WASI host: built
//! for `wasm32-wasip1-threads`, it proves `web/wasi.js` runs threads (a
//! shared `env.memory`, `wasi.thread-spawn`, a worker per thread, futexes as
//! wasm atomics) before the game uses any. `web/verify_threads.py` runs it;
//! natively it is an ordinary program.
//!
//! Four scoped threads each sum a quarter of `1..=N` into their own result;
//! the main thread joins them and checks the total, and a fifth thread
//! answers over a channel. Then threads the way the renderer starts them —
//! a `thread::scope` of short threads, round after round, more rounds than
//! the host has workers — so the host's workers are reused, and the time a
//! round takes is the cost of starting and joining threads. Last, threads
//! that allocate while the heap fills, as the torch set's build and the
//! frame's bakes do: on a memory that grows, Chromium traps a worker
//! ("memory access out of bounds") when another thread grows it under it,
//! about one run in four; the threads build's memory is fixed (`build.rs`),
//! and must never.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

const N: u64 = 4_000_000;

fn main() -> ExitCode {
    let numbers: Vec<u64> = (1..=N).collect();
    let sums: Vec<u64> = thread::scope(|s| {
        let workers: Vec<_> =
            numbers.chunks(numbers.len() / 4).map(|part| s.spawn(move || part.iter().sum::<u64>())).collect();
        workers.into_iter().map(|w| w.join().unwrap_or(0)).collect()
    });
    let total: u64 = sums.iter().sum();
    let (tx, rx) = mpsc::channel();
    let echo = thread::spawn(move || tx.send(format!("{:?}", thread::current().id())));
    let answer = rx.recv().unwrap_or_default();
    let joined = echo.join().is_ok();
    let (rounds, per_round, reused) = rounds_of_threads(&numbers);
    let (grown, filled) = threads_filling_the_heap();
    let ok = total == N * (N + 1) / 2 && sums.len() == 4 && joined && !answer.is_empty() && reused && filled;
    eprintln!(
        "threadcheck: {} scoped threads summed {total} (want {}); a spawned thread answered {answer}; \
        {rounds} rounds of 7 scoped threads {}, {per_round:.0} us a round; 8 threads allocated {grown} MB {} in a memory of {} MB ({}): {}",
        sums.len(),
        N * (N + 1) / 2,
        if reused { "right" } else { "WRONG" },
        if filled { "right" } else { "WRONG" },
        memory_bytes() >> 20,
        if memory_fixed() { "fixed" } else { "growable" },
        if ok { "ok" } else { "FAILED" }
    );
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// `ROUNDS` rounds of seven scoped threads, each summing a slice: the
/// rounds, the microseconds a round took, and whether every sum was right.
fn rounds_of_threads(numbers: &[u64]) -> (usize, f64, bool) {
    const ROUNDS: usize = 200;
    let part = &numbers[..7000];
    let want: u64 = part.iter().sum();
    let t = std::time::Instant::now();
    let mut right = true;
    for _ in 0..ROUNDS {
        let got: u64 = thread::scope(|s| {
            let workers: Vec<_> = part.chunks(1000).map(|c| s.spawn(move || c.iter().sum::<u64>())).collect();
            workers.into_iter().map(|w| w.join().unwrap_or(0)).sum()
        });
        right &= got == want;
    }
    (ROUNDS, t.elapsed().as_secs_f64() * 1e6 / ROUNDS as f64, right)
}

/// `ROUNDS` rounds of eight scoped threads, each allocating 64 buffers —
/// small ones and 64 KiB ones, as a face's torch shares and a lit block —
/// all kept, so every round takes the heap past where it has been: the
/// megabytes allocated, and whether every buffer read back what was written.
fn threads_filling_the_heap() -> (usize, bool) {
    const ROUNDS: usize = 30;
    let mut kept: Vec<Vec<u8>> = Vec::new();
    for round in 0..ROUNDS {
        let made: Vec<Vec<Vec<u8>>> = thread::scope(|s| {
            let workers: Vec<_> = (0..8)
                .map(|t| {
                    s.spawn(move || {
                        let size = |k: usize| if k.is_multiple_of(4) { 64 * 1024 } else { 1000 + 37 * k + t };
                        (0..64).map(|k| vec![(round + k + t) as u8; size(k)]).collect::<Vec<_>>()
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap_or_default()).collect()
        });
        kept.extend(made.into_iter().flatten());
    }
    let right = kept.len() == ROUNDS * 8 * 64
        && kept.chunks(64).enumerate().all(|(i, bufs)| {
            let (round, t) = (i / 8, i % 8);
            bufs.iter().enumerate().all(|(k, b)| b.iter().all(|&x| x == (round + k + t) as u8))
        });
    (kept.iter().map(Vec::len).sum::<usize>() >> 20, right)
}

/// The program's linear memory, in bytes.
fn memory_bytes() -> u64 {
    #[cfg(target_arch = "wasm32")]
    return core::arch::wasm32::memory_size::<0>() as u64 * 65536;
    #[cfg(not(target_arch = "wasm32"))]
    0
}

/// Whether the memory is the fixed size the threads build links
/// (`build.rs`'s `QUAKE_WASM_FIXED_MEMORY`), so it never grows.
fn memory_fixed() -> bool {
    option_env!("QUAKE_WASM_FIXED_MEMORY").and_then(|v| v.parse::<u64>().ok()) == Some(memory_bytes())
}
