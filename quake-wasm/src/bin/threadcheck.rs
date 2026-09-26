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
//! round takes is the cost of starting and joining threads.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

const N: u64 = 4_000_000;

fn main() -> ExitCode {
    let numbers: Vec<u64> = (1..=N).collect();
    let sums: Vec<u64> = thread::scope(|s| {
        let workers: Vec<_> = numbers.chunks(numbers.len() / 4).map(|part| s.spawn(move || part.iter().sum::<u64>())).collect();
        workers.into_iter().map(|w| w.join().unwrap_or(0)).collect()
    });
    let total: u64 = sums.iter().sum();
    let (tx, rx) = mpsc::channel();
    let echo = thread::spawn(move || tx.send(format!("{:?}", thread::current().id())));
    let answer = rx.recv().unwrap_or_default();
    let joined = echo.join().is_ok();
    let (rounds, per_round, reused) = rounds_of_threads(&numbers);
    let ok = total == N * (N + 1) / 2 && sums.len() == 4 && joined && !answer.is_empty() && reused;
    eprintln!("threadcheck: {} scoped threads summed {total} (want {}); a spawned thread answered {answer}; \
        {rounds} rounds of 7 scoped threads {}, {per_round:.0} us a round: {}",
        sums.len(), N * (N + 1) / 2, if reused { "right" } else { "WRONG" }, if ok { "ok" } else { "FAILED" });
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
