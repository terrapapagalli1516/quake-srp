//! quake-rs in the browser, as an ordinary program: `fn main()`, the page's
//! events on stdin, the frames and sounds on stdout, the saves and
//! `config.cfg` through `std::fs`. Built for `wasm32-wasip1`, it runs inside
//! a Web Worker under `web/wasi.js`, a small WASI host that turns those calls
//! into the page's canvas, Web Audio and storage (`web/PLATFORM.md`). Built
//! natively, the same program runs on a pipe — which is how its tests run it.
//!
//! This crate is the platform layer — what id's source has in `sys_*.c`,
//! `vid_*.c`, `snd_*.c` and `in_*.c`, plus the host state around the game:
//! the [`App`](app::App) held in a `thread_local`, the protocol and the loop
//! the page drives, the sound queues, the saves and `config.cfg`. The game
//! client itself — the live frame against the local server, demo playback,
//! the level loads, the cheats — is [`quake_rs::client`], which native tools
//! run too (`quaketool play`). Each client frame hands back a
//! [`ClientFrame`](quake_rs::client::ClientFrame): the screen, its palette
//! shifts, and the calls it made into the sound layer, which [`snd_dma`]
//! carries out for the page.
//!
//! No `unsafe`, no exports, no imports beyond what `std` asks of WASI, no
//! dependencies.
//!
//! ## Layout
//!
//! Each module is named after the WinQuake file whose platform or host side
//! it ports.
//!
//! | module      | id counterpart                          | what                                             |
//! |-------------|-----------------------------------------|--------------------------------------------------|
//! | `sys`       | sys_win.c `main`                        | the loop: events in, a host frame per tick, frames and sounds out |
//! | `proto`     | —                                       | the records on stdin and stdout                  |
//! | `automation`| —                                       | the protocol's calls: the page's buttons, the browser checks' hooks |
//! | `common`    | common.c `COM_InitFilesystem`           | `-basedir`, the game directory, `pak0.pak`, the game's own files |
//! | `config`    | host.c `Host_WriteConfiguration`        | `config.cfg`: the settings' changes, written on change, exec'd at startup |
//! | `app`       | host.c, client.h                        | the `App` (host state around the client's `Walk`/`DemoPlay`: the settings, menu, console, clocks), menu assets, the client's level loads with their sound calls carried out, the boots |
//! | `host`      | host.c `Host_Frame`                     | `step`: the frame gate (id's 72 fps, or every refresh stepped as 72 Hz runs), the mode's client frame, the menu/console overlays, the fps readout, blend, gamma pack |
//! | `cl_walk`   | cl_main.c                               | `step_walk`: `client::cl_main::walk_frame` on the page's `Vid`, its sound calls to `snd_dma`; the live game's end-to-end tests |
//! | `cl_demo`   | cl_demo.c                               | `step_demo`: `client::cl_demo::demo_frame` likewise; the playback tests |
//! | `cl_tent`   | cl_tent.c                               | (tests only) Chthon's lightning end to end       |
//! | `input`     | in_win.c, keys.c `Key_Event`            | mouse look, every key through `Key_Event` (the moves: `client::cl_input`) |
//! | `menu`      | menu.c `M_Keydown`                      | the menu's keys and the actions they return      |
//! | `console`   | console.c, keys.c `Key_Console`         | console toggle and typing                        |
//! | `host_cmd`  | cmd.c `Cmd_ExecuteString`               | the console's command table, `Cvar_Command`, `bind`, `map` (the loads and cheats: `client::host_cmd`) |
//! | `savegame`  | host_cmd.c `Host_Savegame_f`/`_Loadgame_f`, menu.c `M_ScanSaves` | save/load as `.sav` files |
//! | `snd_dma`   | snd_win.c                               | the client's sound calls carried out: the queues the loop sends, the ambient ramps (the channel and loop gates: `quake_rs::snd`) |
//! | `vid`       | vid_win.c                               | the picture's size (a mode in a 4:3 box, or native), framebuffer, the client frames' `Vid` |
//! | `bench`     | —                                       | `--features bench` frame-phase timers and workloads |
//!
//! Tests live with the code they exercise (the client's end-to-end tests
//! here, since they need the shareware pak; its data-free unit tests in
//! quake-rs); `test_util` holds the fixtures several share, `census_tests`
//! the CENSUS.md acceptance tests, `oracle_screen` the 2-D oracle's port side.
//!
//! ## Settings
//!
//! Every setting is in the App's [`quake_rs::settings::Settings`]: id's cvars
//! and key bindings, and the port's departures from id's game, which two
//! profiles switch — **2026**, the default, and **Classic**, WinQuake
//! exactly (Options > "Classic / 2026", `profile classic|2026` on the
//! console, `?classic` / `?2026` in the page's address). `config.cfg` in the
//! game directory keeps the profile and whatever the player changed from
//! it, the id way (`bind` lines and archived cvars).

#![forbid(unsafe_code)]

mod app;
mod automation;
mod bench;
mod cl_demo;
#[cfg(test)]
mod cl_tent;
mod cl_walk;
mod common;
mod config;
mod console;
mod host;
mod host_cmd;
mod input;
mod menu;
mod proto;
mod savegame;
mod snd_dma;
mod sys;
mod vid;
#[cfg(test)]
mod oracle_screen;
#[cfg(test)]
mod test_util;

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;

use std::io::{self, BufWriter};
use std::path::PathBuf;
use std::process::ExitCode;

/// `COM_InitFilesystem`'s `-basedir <dir>` (default `.`): where `id1/` is.
fn basedir() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "-basedir")
        .and_then(|i| args.get(i + 1))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn main() -> ExitCode {
    if let Err(e) = common::init(&basedir()) {
        eprintln!("quake: {e}");
        return ExitCode::FAILURE;
    }
    // stdout goes through a buffer the size of a turn's small records, so
    // a turn reaches the host in a few writes; a frame's pixels pass
    // straight through it.
    let out = BufWriter::with_capacity(64 * 1024, io::stdout().lock());
    match sys::run(io::stdin().lock(), out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quake: {e}");
            ExitCode::FAILURE
        }
    }
}
