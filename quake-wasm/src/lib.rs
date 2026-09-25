//! Browser (WASM) shell for `quake-rs`: an interactive first-person walk *and*
//! recorded-demo playback, rendered to an RGBA framebuffer the page blits to a
//! canvas.
//!
//! The engine crate stays `#![forbid(unsafe_code)]` and compiles to
//! `wasm32-unknown-unknown` unchanged. This shell has **zero `unsafe {}` blocks**
//! — it just can't `forbid(unsafe_code)` because modern Rust treats the
//! `#[no_mangle]` export attribute as unsafe-adjacent. The pak is embedded with
//! `include_bytes!` so nothing crosses JS→WASM (no `from_raw_parts`); state lives
//! in a `thread_local`; the page reads the framebuffer out of linear memory via
//! the `Vec::as_ptr()` we hand back. No `wasm-bindgen`, no dependencies.

mod app;
mod bench;
mod cl_demo;
mod cl_tent;
mod cl_walk;
mod console;
mod host;
mod host_cmd;
mod input;
mod menu;
mod savegame;
mod snd_dma;
mod vid;
#[cfg(test)]
mod test_util;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;

