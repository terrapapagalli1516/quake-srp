//! Browser (WASM) shell for `quake-rs`: the live game *and* recorded-demo
//! playback, rendered to an RGBA framebuffer the page blits to a canvas.
//!
//! This crate is the platform layer — what id's source has in `vid_*.c`,
//! `snd_*.c`, `in_*.c` and `sys_*.c`, plus the host state around the game:
//! the `extern "C"` exports the page calls, the [`App`](app::App) held in a
//! `thread_local`, the queues the page drains (sounds, saves), the
//! localStorage bridge and the Web extras. The game client itself — the live
//! frame against the local server, demo playback, the level loads, the
//! cheats — is [`quake_rs::client`], which native tools run too
//! (`quaketool play`). Each client frame hands back a
//! [`ClientFrame`](quake_rs::client::ClientFrame): the screen, its palette
//! shifts, and the calls it made into the sound layer, which [`snd_dma`]
//! carries out for the page.
//!
//! The engine crate stays `#![forbid(unsafe_code)]` and compiles to
//! `wasm32-unknown-unknown` unchanged. This shell has **zero `unsafe {}` blocks**
//! — it just can't `forbid(unsafe_code)` because modern Rust treats the
//! `#[no_mangle]` export attribute as unsafe-adjacent. The pak is embedded with
//! `include_bytes!` so nothing crosses JS→WASM (no `from_raw_parts`); state lives
//! in a `thread_local`; the page reads the framebuffer out of linear memory via
//! the `Vec::as_ptr()` we hand back. No `wasm-bindgen`, no dependencies.
//!
//! ## Layout
//!
//! Each module is named after the WinQuake file whose platform or host side
//! it ports, and every `extern "C"` export lives next to its subsystem.
//!
//! | module      | id counterpart                          | what                                             |
//! |-------------|-----------------------------------------|--------------------------------------------------|
//! | `app`       | host.c, client.h                        | the `App` (host state around the client's `Walk`/`DemoPlay`), the embedded pak, menu assets, the client's level loads with their sound calls carried out, the `boot*` exports |
//! | `host`      | host.c `Host_Frame`                     | `step`: the 72 fps gate (`wasm_uncapped`), the mode's client frame, the menu/console overlays, the fps readout, blend, gamma pack |
//! | `cl_walk`   | cl_main.c                               | `step_walk`: `client::cl_main::walk_frame` on the page's `Vid`, its sound calls to `snd_dma`; the live game's end-to-end tests |
//! | `cl_demo`   | cl_demo.c                               | `step_demo`: `client::cl_demo::demo_frame` likewise; the playback tests |
//! | `cl_tent`   | cl_tent.c                               | (tests only) Chthon's lightning end to end       |
//! | `input`     | in_win.c, keys.c                        | mouse look, key exports (the moves: `client::cl_input`) |
//! | `menu`      | menu.c `M_Keydown`                      | menu key exports and the actions they return     |
//! | `console`   | console.c, keys.c `Key_Console`         | console toggle/typing exports                    |
//! | `extras`    | —                                       | the Web extras' `wasm_*` cvars (values in the menu), the renderer's per-frame copy |
//! | `host_cmd`  | cmd.c `Cmd_ExecuteString`               | console command dispatch, `map` (the loads and cheats: `client::host_cmd`) |
//! | `savegame`  | host_cmd.c `Host_Savegame_f`/`_Loadgame_f` | save/load over the page's localStorage        |
//! | `snd_dma`   | snd_win.c                               | the client's sound calls carried out: the queues for the page's Web Audio, the ambient ramps, the exports (the channel and loop gates: `quake_rs::snd`) |
//! | `vid`       | vid_win.c                               | resolution, framebuffer, viewsize, the client frames' `Vid` |
//! | `bench`     | —                                       | `--features bench` frame-phase timers            |
//!
//! Tests live with the code they exercise (the client's end-to-end tests
//! here, since they need the embedded pak; its data-free unit tests in
//! quake-rs); `test_util` holds the fixtures several share, `census_tests`
//! the CENSUS.md acceptance tests, `oracle_screen` the 2-D oracle's port side.
//!
//! ## The JS ↔ wasm ABI
//!
//! The module imports nothing (a `--features bench` build imports
//! `quake_bench.now_ms`) and exports its `memory` plus these functions:
//!
//! | module     | exports |
//! |------------|---------|
//! | `app`      | `boot` `boot_demo` `boot_attract` `in_walk_mode` |
//! | `host`     | `step` |
//! | `cl_demo`  | `timedemo_running` |
//! | `vid`      | `width` `height` `set_resolution` `framebuffer` `viewsize` `set_viewsize` `set_scaled_2d` `scaled_2d` |
//! | `input`    | `key_down` `key_up` `key_is_down` `mouse_move` `pointer_unlocked` `look` `player_pitch` `mouse_sensitivity`; legacy/automation: `set_move` `set_attack` `set_jump` `set_movedown` `set_impulse` |
//! | `menu`     | `menu_up` `menu_down` `menu_left` `menu_right` `menu_select` `menu_cancel` `menu_quit_yes` `menu_quit_no` `menu_backspace` `menu_bind_grabbing` `menu_bind_key` `menu_screen_id` `menu_visible`; the Web extras: `extras` `set_extras` |
//! | `console`  | `console_toggle` `console_visible` `console_char` `console_backspace` `console_enter`; verification: `console_text_len` `console_text_ptr` |
//! | `savegame` | `poll_save` `save_name_len` `save_name_ptr` `save_text_ptr` `save_store_failed` `poll_load_request` `load_request_ptr` `load_failed` `sav_alloc` `load_game` `extract_save_comment` `save_comment_ptr` `menu_set_save_comment` |
//! | `snd_dma`  | `set_audio_ready` `volume` `poll_sound` `poll_menu_sound` `poll_stop_sound` `poll_static_sound` `load_sound` `load_ambient_sound` `sound_ptr` `sound_origin_{x,y,z}` `sound_volume` `sound_attenuation` `sound_is_view_entity` `sound_entity` `sound_channel` `sound_generation` `sound_loop_start` `sound_loop_end` `ambient_gain` `listener_{x,y,z}` `listener_fwd_{x,y,z}` `listener_right_{x,y,z}` |
//! | `bench`    | (`--features bench` only) `bench_enable` `bench_names_len` `bench_names_ptr` `bench_value` |
//!
//! The page keeps three settings in localStorage across reloads, reading
//! each after a frame and restoring it after boot: the video mode (`width`
//! `height` / `set_resolution`), `viewsize` (`viewsize` / `set_viewsize`, as
//! id's config.cfg keeps it) and the Web extras (`extras` / `set_extras`).
//! The Web extras are the port's opt-in departures from id's Quake, all off
//! by default, switched on Options > Web extras or by console command; as
//! bits: 1 `wasm_uncapped` (no 72 fps cap), 2 `wasm_showfps` (frame-rate
//! readout), 4 `wasm_exactpersp` (exact perspective at every pixel), 8
//! `wasm_scaled2d` (the 2-D layer scaled up to the screen).

mod app;
mod bench;
mod cl_demo;
#[cfg(test)]
mod cl_tent;
mod cl_walk;
mod console;
mod extras;
mod host;
mod host_cmd;
mod input;
mod menu;
mod savegame;
mod snd_dma;
mod vid;
#[cfg(test)]
mod oracle_screen;
#[cfg(test)]
mod test_util;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;

