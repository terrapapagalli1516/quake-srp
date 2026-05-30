//! # quake-rs
//!
//! A faithful, memory-safe Rust port of the self-contained, verifiable
//! subsystems of id Software's *Quake* (1996, GPLv2): the math library and the
//! on-disk asset-format loaders.
//!
//! The original engine is ~100k lines of C (renderer, server, client, netcode,
//! sound, QuakeC virtual machine). This crate ports the layers that are
//! *tractable and checkable in isolation* — the foundation everything else
//! sits on — and documents the rest as a roadmap (see `README.md`).
//!
//! ## What is ported here
//!
//! | module      | from (C)                | what it is                                  |
//! |-------------|-------------------------|---------------------------------------------|
//! | [`math`]    | `mathlib.c`             | vec3 / matrix / angle math, `BoxOnPlaneSide` |
//! | [`crc`]     | `crc.c`                 | CRC-16/CCITT (XMODEM) used for PAK integrity |
//! | [`wad`]     | `wad.c`, `wad.h`        | WAD2 archive (gfx.wad: pics, palette, fonts) |
//! | [`pak`]     | `common.c`              | PAK archive (pak0.pak / pak1.pak)            |
//! | [`bsp`]     | `bspfile.h`, `model.c`  | BSP version 29 map loader                    |
//! | [`mdl`]     | `modelgen.h`, `model.c` | MDL alias (animated) model loader            |
//! | [`spr`]     | `spritegn.h`, `model.c` | SPR sprite loader                            |
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
pub mod bsp;
pub mod mdl;
pub mod spr;

pub mod progs;
pub mod vm;
pub mod builtins;

pub mod world;
pub mod server;

pub mod render;
pub mod demo;

pub use error::{QError, Result};
pub use math::Vec3;
