//! # quake-rs
//!
//! id Software's *Quake* (1996, GPLv2) in Rust, ported file by file from
//! WinQuake's C: the file formats, the QuakeC virtual machine, the server,
//! the game client, the software renderer, the 2-D layer and menus, the
//! console and settings, and the sound mixer. The platforms drive it: this
//! crate's `quaketool` runs it natively, and `quake-wasm` runs it in a
//! browser as a WASI program.
//!
//! With every extra off (the Classic profile) it is id's game — frames, game
//! state and timing are checked against id's C (`oracle/`, the goldens, the
//! census). The 2026 profile's departures are typed settings ([`cvar`],
//! [`settings`]), each one a switch.
//!
//! ## Layout
//!
//! | modules | from (C) | what |
//! |---|---|---|
//! | [`pak`], [`common`], [`wad`], [`bsp`], [`mdl`], [`spr`], [`crc`] | `common.c`, `wad.c`, `model.c`, `crc.c` | the search path, the archives and the file formats |
//! | [`progs`], [`vm`], [`builtins`] | `pr_*.c` | QuakeC: the program, the interpreter, the builtins |
//! | [`server`], [`world`], [`save`] | `sv_*.c`, `world.c`, `host_cmd.c` | the server: physics, collision, savegames |
//! | [`client`], [`demo`], [`particles`], [`tent`], [`dlight`], [`stepping`] | `cl_*.c`, `view.c`, `host.c` | the game client: the live frame, demos, effects, the host clock |
//! | [`render`] | `r_*.c`, `d_*.c` | the software renderer |
//! | [`draw`], [`screen`], [`sbar`], [`menu`], [`console`], [`keys`] | `draw.c`, `screen.c`, `sbar.c`, `menu.c`, `console.c`, `keys.c` | the 2-D layer and the keys |
//! | [`cvar`], [`cmd`], [`settings`] | `cvar.c`, `cmd.c` | variables, commands, the Classic and 2026 profiles |
//! | [`snd`], [`cd_audio`] | `snd_*.c`, `cd_win.c` | the sound mixer and the CD player |
//! | [`math`], [`qrand`], [`read`], [`error`] | `mathlib.c` | math, the random streams, byte decoding, errors |
//!
//! ## Design notes
//!
//! * **Zero external dependencies.** Only `std`. Builds offline.
//! * **No `unsafe`.** The C code casts raw file buffers straight into structs;
//!   here every field is decoded explicitly through [`read::Reader`], so a
//!   malformed or truncated file yields a [`QError`] instead of UB.
//! * **Little-endian on disk.** Quake's `LittleLong`/`LittleShort` byte-swaps
//!   are replaced by `*::from_le_bytes`, so loaders are correct on any host.

#![forbid(unsafe_code)]

pub mod error;
pub mod read;

pub mod math;
pub mod crc;
pub mod wad;
pub mod pak;
pub mod common;
pub mod bsp;
pub mod mdl;
pub mod spr;

pub mod progs;
pub mod qrand;
pub mod vm;
pub mod builtins;

pub mod world;
pub mod server;
pub mod save;
pub mod particles;
pub mod tent;

pub mod render;
pub mod draw;
pub mod screen;
pub mod sbar;
pub mod keys;
pub mod cvar;
pub mod cmd;
pub mod settings;
pub mod menu;
pub mod console;
pub mod dlight;
pub mod demo;
pub mod snd;
pub mod cd_audio;
pub mod stepping;

pub mod client;

pub use error::{ProgramError, QError, Result};
pub use math::Vec3;
