//! The threads build's memory is fixed: linked with its initial size equal to
//! its maximum, so it never grows.
//!
//! In Chromium a worker thread can trap ("memory access out of bounds")
//! when another thread grows the shared memory (`memory.grow`, from
//! `malloc`'s `sbrk`) while it runs: one thread allocating, another touching
//! what it just got from the new pages. A memory that never grows cannot
//! race (web/PLATFORM.md, "Threads"). The single-thread build (no shared
//! memory, no workers) keeps id's growable one. `QUAKE_WASM_GROWABLE=1`
//! links the threads build growable again, for `verify_threads.py` to show
//! the trap.

/// The threads build's memory, initial and maximum: 1 GiB, the most
/// `wasm32-wasip1-threads` links for by default (web/PLATFORM.md, "Threads",
/// for what it holds and what a full one does).
const FIXED_MEMORY: u64 = 1 << 30;

fn main() {
    println!("cargo:rerun-if-env-changed=QUAKE_WASM_GROWABLE");
    let threads = std::env::var("TARGET").is_ok_and(|t| t == "wasm32-wasip1-threads");
    let growable = std::env::var("QUAKE_WASM_GROWABLE").is_ok_and(|v| v == "1");
    if threads && !growable {
        println!("cargo:rustc-link-arg-bins=--initial-memory={FIXED_MEMORY}");
        println!("cargo:rustc-link-arg-bins=--max-memory={FIXED_MEMORY}");
        println!("cargo:rustc-env=QUAKE_WASM_FIXED_MEMORY={FIXED_MEMORY}");
    }
}
