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
//!
//! ## Layout
//!
//! Each module is named after the WinQuake file whose client/host side it
//! ports, and every `extern "C"` export lives next to its subsystem.
//!
//! | module      | id counterpart                          | what                                             |
//! |-------------|-----------------------------------------|--------------------------------------------------|
//! | `app`       | client.h, host.c, cl_demo.c `CL_PlayDemo_f` | `App`/`Walk`/`DemoPlay` state, asset loads, walk/demo builders, boot |
//! | `host`      | host.c `Host_Frame`                     | `step`: the 72 fps gate, the mode's frame, overlays, blend, gamma pack |
//! | `cl_walk`   | cl_main.c, cl_parse.c, view.c, screen.c | `step_walk`: the live client frame               |
//! | `cl_demo`   | cl_demo.c, cl_parse.c, view.c           | `step_demo`: the recorded-demo client frame      |
//! | `cl_tent`   | cl_tent.c                               | (tests only) Chthon's lightning end to end; the code is `client::cl_tent` |
//! | `input`     | in_win.c, keys.c                        | mouse look, key exports (the moves: `client::cl_input`) |
//! | `menu`      | menu.c `M_Keydown`                      | menu key exports and the actions they return     |
//! | `console`   | console.c, keys.c `Key_Console`         | console toggle/typing exports                    |
//! | `extras`    | —                                       | the Web extras' `wasm_*` cvars (values in the menu), the renderer's per-frame copy |
//! | `host_cmd`  | host_cmd.c, cmd.c                       | console commands, `map`, changelevel, restart    |
//! | `savegame`  | host_cmd.c `Host_Savegame_f`/`_Loadgame_f` | save/load over the page's localStorage        |
//! | `snd_dma`   | snd_dma.c                               | sound queues for the page's Web Audio            |
//! | `vid`       | vid_win.c, screen.c                     | resolution, framebuffer, viewsize, backtile      |
//! | `bench`     | —                                       | `--features bench` frame-phase timers            |
//!
//! Tests live with the code they exercise; `test_util` holds the fixtures
//! several share, `census_tests` the CENSUS.md acceptance tests.
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
//! | `vid`      | `width` `height` `set_resolution` `framebuffer` `viewsize` `set_viewsize` `set_scaled_2d` `scaled_2d` |
//! | `input`    | `key_down` `key_up` `key_is_down` `mouse_move` `pointer_unlocked` `look` `player_pitch` `mouse_sensitivity`; legacy/automation: `set_move` `set_attack` `set_jump` `set_movedown` `set_impulse` |
//! | `menu`     | `menu_up` `menu_down` `menu_left` `menu_right` `menu_select` `menu_cancel` `menu_quit_yes` `menu_quit_no` `menu_backspace` `menu_bind_grabbing` `menu_bind_key` `menu_screen_id` `menu_visible`; the Web extras: `extras` `set_extras` |
//! | `console`  | `console_toggle` `console_visible` `console_char` `console_backspace` `console_enter` |
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
//! readout), 4 `wasm_exactpersp` (exact perspective at every pixel).

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

