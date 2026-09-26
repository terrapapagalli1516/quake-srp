//! Console commands — `Cmd_ExecuteString`'s dispatch (cmd.c) against the
//! App: echo/clear, `help` (the Help/Ordering screen), the viewsize cvar
//! commands, the Web extras and the port's `wasm_help` list, `map`,
//! save/load, `pause`, the demo commands (cl_demo.c's `playdemo`/`timedemo`,
//! host_cmd.c's demo loop control `startdemos`/`demos`/`stopdemo`), and the
//! cheats god/noclip/fly/kill/give/impulse, which act on the live
//! [`Walk`](crate::app::Walk) through the client's host_cmd.c
//! ([`quake_rs::client::host_cmd`], which also holds the level swaps
//! `changelevel` and `restart`).

use quake_rs::client::cl_demo::MAX_DEMOS;
use quake_rs::client::host_cmd::run_game_command;

use crate::app::{build_walk_map, ensure_app, App};
use crate::cl_demo::{cl_disconnect, cl_next_demo, cl_play_demo, cl_stop_playback, cl_timedemo};
use crate::savegame::{do_load_command, do_save_command};
use crate::snd_dma;

/// `MAX_DEMONAME` (client.h): a `cls.demos` slot holds 15 characters.
const MAX_DEMONAME: usize = 16;

/// The commands this console runs, in the order `Cmd_CompleteCommand` meets
/// id's (`cmd_functions`: `Cmd_AddCommand` puts each in front, so the one
/// registered last — `timedemo`, in `CL_Init` — comes first; `play`, from
/// `S_Init`, after `CL_Init`'s; `echo`, from `Cmd_Init`, last), then the
/// port's own.
pub(crate) const COMMANDS: &[&str] = &[
    "timedemo", "playdemo", "impulse", "play", "sizedown", "sizeup", "help", "clear", "stopdemo", "demos",
    "startdemos", "give", "save", "load", "pause", "kill", "color", "noclip", "name", "map", "fly",
    "god", "echo", "wasm_help",
];

/// The cvars this console reads and sets, in `cvar_vars` order (registered
/// last, found first: `_cl_color` and `_cl_name` in `CL_Init`, `viewsize` in
/// `SCR_Init`, `hostname` in `NET_Init`), then the port's `_vid_resolution`
/// (`config.cfg`'s video mode) and the Web extras' `wasm_*`.
const CVARS: &[&str] = &["_cl_color", "_cl_name", "viewsize", "hostname", "_vid_resolution", "r_threads"];

/// `Cmd_CompleteCommand` then `Cvar_CompleteVariable` (cmd.c, cvar.c), what
/// Tab in the console runs: the first command, else the first cvar, whose
/// name starts with `partial` (case matters, `Q_strncmp`); nothing for an
/// empty line.
pub(crate) fn complete(partial: &str) -> Option<String> {
    if partial.is_empty() {
        return None;
    }
    let cvars = CVARS.iter().copied().chain(crate::extras::cvar_names());
    COMMANDS.iter().copied().chain(cvars).find(|name| name.starts_with(partial)).map(str::to_string)
}

/// `Host_Startdemos_f`: `startdemos <demo> ...` sets the demo loop (at most
/// [`MAX_DEMOS`]) and, with nothing running (`!sv.active &&
/// !cls.demoplayback`) and the loop not switched off, starts it
/// (`CL_NextDemo`); otherwise the loop is off until `demos`.
pub(crate) fn host_startdemos(a: &mut App, names: &[&str]) {
    let mut c = names.len();
    if c > MAX_DEMOS {
        a.console.println(format!("Max {MAX_DEMOS} demos in demoloop"));
        c = MAX_DEMOS;
    }
    a.console.println(format!("{c} demo(s) in loop"));
    for (slot, name) in a.cls.demos.iter_mut().zip(&names[..c]) {
        // strncpy (cls.demos[i-1], Cmd_Argv(i), sizeof(cls.demos[0])-1)
        *slot = name.chars().take(MAX_DEMONAME - 1).collect();
    }
    if !a.sv_active() && a.cls.demonum != -1 && !a.demoplayback() {
        a.cls.demonum = 0;
        cl_next_demo(a);
    } else {
        a.cls.demonum = -1;
    }
}

/// `Host_Demos_f`: back to the demo loop — disconnect and play its next demo
/// (the second, if the loop was off).
fn host_demos(a: &mut App) {
    if a.cls.demonum == -1 {
        a.cls.demonum = 1;
    }
    cl_disconnect(a);
    cl_next_demo(a);
}

/// `pause` from this client: `Host_Pause_f` is forwarded to the server
/// (`Cmd_ForwardToServer`), which toggles `sv.paused` and broadcasts who did
/// it ([`quake_rs::server::Server::pause`]). During demo playback it goes
/// nowhere ("not really connected"); with nothing running it cannot go.
pub(crate) fn host_pause(a: &mut App) {
    if a.demoplayback() {
        return;
    }
    match a.walk.as_mut().filter(|_| a.mode == 0) {
        Some(w) => w.server.pause(),
        None => a.console.println("Can't \"pause\", not connected"),
    }
}

/// `Host_Stopdemo_f`: stop the playing demo and disconnect (the loop keeps
/// its place: `demos`, or leaving the menu, resumes it).
fn host_stopdemo(a: &mut App) {
    if !a.demoplayback() {
        return;
    }
    cl_stop_playback(a);
    cl_disconnect(a);
}

// --- console command execution -------------------------------------------

/// Parse `line` into whitespace argv and run the matching console command
/// against the live [`Walk`], appending any output to the console scrollback.
/// An empty line does nothing; an unknown command prints
/// `"unknown command: <cmd>"`. Commands that touch the player edict guard on a
/// live walk and print `"no active game"` when there is none. Nothing here
/// panics on a bad/missing argument (all parsing uses `.ok()`/defaults).
pub(crate) fn execute_console_command(line: &str) {
    let argv: Vec<&str> = line.split_whitespace().collect();
    let Some(&cmd) = argv.first() else { return };
    let cmd_lower = cmd.to_ascii_lowercase();

    // Commands that don't need the walk: echo / clear / help / wasm_help.
    match cmd_lower.as_str() {
        "clear" => {
            ensure_app(|a| a.console.clear());
            return;
        }
        "echo" => {
            let text = if argv.len() > 1 {
                argv[1..].join(" ")
            } else {
                String::new()
            };
            ensure_app(|a| a.console.println(text));
            return;
        }
        // S_Play (snd_dma.c): each named sample at the listener.
        "play" => {
            snd_dma::s_play(&argv[1..]);
            return;
        }
        // M_Menu_Help_f (menu.c registers it as `help`): the Help/Ordering
        // screen, on its first page, with the keyboard (key_dest =
        // key_menu: the console goes up).
        "help" => {
            ensure_app(App::m_menu_help);
            return;
        }
        // Not id's: the renderer's threads, as a cvar (0: as many as the
        // host offers; the pixels are the same for any count).
        "r_threads" => {
            ensure_app(|a| match argv.get(1) {
                None => {
                    let (n, now) = (a.render_threads.cvar(), a.render_threads.resolve(a.hw_threads));
                    a.console.println(format!("\"r_threads\" is \"{n}\" ({now} of {} threads)", a.hw_threads));
                }
                Some(v) => a.render_threads = quake_rs::render::Threads::from_cvar(v.parse::<f32>().unwrap_or(0.0)),
            });
            return;
        }
        // Not id's: the port's command list (`help` is id's Help screen).
        "wasm_help" => {
            ensure_app(|a| {
                a.console.println("this port's commands:");
                a.console.println("  god noclip fly kill  pause");
                a.console.println("  give <h|a|s|n|r|c|1-8> [n]");
                a.console.println("  impulse <n>   map <name>");
                a.console.println("  save <name>   load <name>");
                a.console.println("  playdemo <name>  timedemo <name>");
                a.console.println("  stopdemo  demos  startdemos <d..>");
                a.console.println("  sizeup  sizedown  viewsize [n]");
                a.console.println("  echo <text>   clear   help");
                a.console.println("  r_threads <n> (0: all the host offers)");
                a.console.println("  wasm_help (this list)");
                a.console.println("web extras (not id's; see Options):");
                for line in crate::extras::help_lines() {
                    a.console.println(line);
                }
            });
            return;
        }
        // Host_Name_f's client half: print, or set `_cl_name` (one argument,
        // else the whole argument string; 15 characters). The server's side —
        // renaming the connected player — is not done: the port's server
        // connects the player as "player" (sv_main.rs).
        "name" => {
            ensure_app(|a| {
                if argv.len() == 1 {
                    a.console.println(format!("\"name\" is \"{}\"", a.menu.name()));
                } else {
                    // Cmd_Args, its quotes as Cmd_TokenizeString takes them.
                    let args = line.trim_start()[cmd.len()..].trim();
                    let name = if argv.len() == 2 { argv[1] } else { args };
                    let name = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')).unwrap_or(name);
                    a.menu.set_name(name);
                }
            });
            return;
        }
        // Host_Color_f's client half: print, or `_cl_color` from one colour
        // (both) or two, each 0..13.
        "color" => {
            ensure_app(|a| {
                if argv.len() == 1 {
                    let c = a.menu.color();
                    a.console.println(format!("\"color\" is \"{} {}\"", c >> 4, c & 15));
                    a.console.println("color <0-13> [0-13]");
                } else {
                    let top = atoi(argv[1]);
                    let bottom = if argv.len() == 2 { top } else { atoi(argv[2]) };
                    a.menu.set_color(top, bottom);
                }
            });
            return;
        }
        // Cvar_Command for the name/colour cvars and `hostname`: print the
        // value, or set it from the first argument.
        "hostname" | "_cl_name" | "_cl_color" => {
            ensure_app(|a| {
                let name = cmd_lower.as_str();
                match argv.get(1) {
                    None => {
                        let v = match name {
                            "hostname" => a.menu.hostname().to_string(),
                            "_cl_name" => a.menu.name().to_string(),
                            _ => a.menu.color().to_string(),
                        };
                        a.console.println(format!("\"{name}\" is \"{v}\""));
                    }
                    Some(v) => match name {
                        "hostname" => a.menu.set_hostname(v),
                        "_cl_name" => a.menu.set_name(v),
                        _ => a.menu.set_color_value(v.parse::<f32>().unwrap_or(0.0) as i32),
                    },
                }
            });
            return;
        }
        // The port's archived video mode (`config.cfg`; id's archives
        // `_vid_default_mode_win`, a mode number): `WxH`, clamped like a
        // Video Options pick. No argument prints it.
        "_vid_resolution" => {
            match argv.get(1).and_then(|arg| arg.split_once('x')) {
                Some((w, h)) => crate::vid::set_resolution(atoi(w), atoi(h)),
                None => ensure_app(|a| {
                    let v = format!("{}x{}", a.render_w, a.render_h);
                    a.console.println(format!("\"_vid_resolution\" is \"{v}\""));
                }),
            }
            return;
        }
        // SCR_SizeUp_f / SCR_SizeDown_f: viewsize +/- 10 (SCR_CalcRefdef bounds
        // it to 30..120 on the next frame).
        "sizeup" => {
            ensure_app(|a| a.menu.size_up());
            return;
        }
        "sizedown" => {
            ensure_app(|a| a.menu.size_down());
            return;
        }
        // The `viewsize` cvar (Cvar_Command): no argument prints it the C's way,
        // one argument sets it (bounded like SCR_CalcRefdef).
        "viewsize" => {
            ensure_app(|a| match argv.get(1) {
                None => {
                    let v = a.menu.viewsize();
                    a.console.println(format!("\"viewsize\" is \"{}\"", cvar_string(v)));
                }
                Some(arg) => a.menu.set_viewsize(arg.parse::<f32>().unwrap_or(0.0)),
            });
            return;
        }
        _ => {}
    }

    // The demo commands: cl_demo.c's CL_PlayDemo_f / CL_TimeDemo_f (the
    // `Cmd_Argc() != 2` usage lines are the C's, "play" included) and
    // host_cmd.c's demo loop control.
    if cmd_lower == "pause" {
        ensure_app(host_pause);
        return;
    }
    if matches!(cmd_lower.as_str(), "playdemo" | "timedemo" | "stopdemo" | "startdemos" | "demos") {
        ensure_app(|a| match cmd_lower.as_str() {
            "playdemo" if argv.len() != 2 => a.console.println("play <demoname> : plays a demo"),
            "playdemo" => {
                cl_play_demo(a, argv[1], false);
            }
            "timedemo" if argv.len() != 2 => a.console.println("timedemo <demoname> : gets demo speeds"),
            "timedemo" => cl_timedemo(a, argv[1]),
            "stopdemo" => host_stopdemo(a),
            "startdemos" => host_startdemos(a, &argv[1..]),
            _ => host_demos(a),
        });
        return;
    }

    // The Web extras (`wasm_*`, not id's; all off by default): `extras.rs`.
    let mut extra = false;
    ensure_app(|a| extra = crate::extras::console_command(a, &argv));
    if extra {
        return;
    }

    // `map <name>` rebuilds the walk on a new level; handle it specially because
    // it replaces the whole Walk (can't be done while holding a &mut to it).
    if cmd_lower == "map" {
        run_map_command(argv.get(1).copied());
        return;
    }

    // `save`/`load` (Host_Savegame_f / Host_Loadgame_f): handled at this level
    // because load replaces the whole Walk (via the page round-trip) and save
    // runs guards that need the App, not just the walk. The C's Cmd_Argc()!=2
    // check covers extra args too, so pass None unless exactly one argument.
    if cmd_lower == "save" {
        do_save_command(if argv.len() == 2 { Some(argv[1]) } else { None });
        return;
    }
    if cmd_lower == "load" {
        do_load_command(if argv.len() == 2 { Some(argv[1]) } else { None });
        return;
    }

    // The remaining commands act on the live player edict. Run them under a
    // single borrow; guard a missing walk with "no active game".
    ensure_app(|a| {
        let has_walk = a.walk.is_some();
        if !has_walk {
            a.console.println("no active game");
            return;
        }
        // Split the borrow: the walk (player edict + vm) and the console output.
        // Take the player index + a raw pointer-free reference via the App.
        let mut out: Vec<String> = Vec::new();
        if let Some(w) = a.walk.as_mut() {
            let mut sound = Vec::new();
            run_game_command(w, &cmd_lower, &argv, &mut out, &mut sound);
            snd_dma::play(&w.pak, sound);
        }
        for line in out {
            a.console.println(line);
        }
    });

    // An unrecognised command: report it. (Handled here so the borrow above can
    // finish first; run_game_command pushes nothing for an unknown verb.)
    let known = matches!(
        cmd_lower.as_str(),
        "god" | "noclip" | "fly" | "kill" | "give" | "impulse"
    );
    if !known {
        ensure_app(|a| a.console.println(format!("unknown command: {cmd}")));
    }
}

/// `atoi`: the leading integer of `s` (0 for none).
fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')))
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    s[..end].parse().unwrap_or(0)
}

/// A cvar value as the console prints it: `%f` with the trailing zeros (and a
/// bare trailing point) trimmed — `100`, `55.5`. (The C prints the cvar's
/// STRING, which is whatever set it last: "100" from default.cfg, "55" typed,
/// but "110.000000" after `Cvar_SetValue`'s `%f`. This port keeps no cvar
/// strings, so it always prints the short form.)
pub(crate) fn cvar_string(v: f32) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Run `map <name>`: build a fresh walk on `maps/<name>.bsp`. On success swap the
/// walk, close the console, and print `"loading <name>"`; on failure print
/// `"map not found: <name>"` and keep the current level.
fn run_map_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("usage: map <name>"));
        return;
    };
    let path = format!("maps/{name}.bsp");
    // Build the new walk OUTSIDE the borrow (it reads the pak + parses a BSP).
    let new_walk = build_walk_map(&path);
    ensure_app(|a| match new_walk {
        Some(nw) => {
            a.start_game(nw);
            a.console.println(format!("loading {name}"));
            // The port's own line (Host_Map_f prints none): the console keeps
            // it, the notify lines don't — SCR_EndLoadingPlaque's Con_ClearNotify
            // starts the new level with none.
            let _ = a.console.take_unnotified();
            // The level loaded: close the console so the player sees the new map
            // — at once, as SCR_BeginLoadingPlaque zeroes scr_con_current.
            a.console.open = false;
            a.console.set_current(0.0);
            // Keep the menu closed too (a `map` from the console starts play).
            // Navigation-only reset: the C's `map` command never resets cvars or
            // keybindings, so the player's options and rebinds survive here too.
            a.menu.reset_nav();
            // Preserve the player's chosen render resolution across a `map` (the C
            // keeps the video mode): the framebuffer is untouched, and we eagerly
            // point the menu's current video mode at it — same as every other
            // re-boot site — so the Video Options list is correct the instant the
            // player opens it (not relying on the per-frame sync in step()).
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
        None => a.console.println(format!("map not found: {name}")),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::client::host_cmd::{
        try_changelevel, try_restart, FL_GODMODE, IT_SHOTGUN, MOVETYPE_FLY, MOVETYPE_NOCLIP,
        MOVETYPE_WALK,
    };

    use crate::app::{boot, player_start, APP};
    use crate::console::{console_toggle, console_visible};
    use crate::host::step;
    use crate::input::{key_down, key_up, set_attack};
    use crate::menu::menu_cancel;
    use crate::test_util::*;
    use crate::vid::{set_resolution, viewsize};

    #[test]
    fn restart_respawns_with_the_level_entry_serverflags() {
        // CENSUS L7: Host_Restart_f -> SV_SpawnServer writes svs.serverflags
        // (the runes held on ENTRY) into the QC global; only SV_SaveSpawnparms,
        // at a changelevel, reads the live global back. So a rune taken on
        // e1m7 is lost if the player dies there, and a changelevel carries it.
        let mut w = crate::app::build_walk_map("maps/e1m7.bsp").expect("e1m7 boots");
        w.server.vm.gset_float("serverflags", 1.0); // sigil_touch: rune 1
        try_restart(&mut w, &mut Vec::new());
        assert_eq!(w.server.serverflags(), 0.0, "the rune taken on this level is gone");
        // Arrive with rune 1 (a changelevel carries the live bits), take rune 2.
        w.server.vm.gset_float("serverflags", 1.0);
        try_changelevel(&mut w, "e1m7", &mut Vec::new());
        assert_eq!(w.server.level_entry_serverflags(), 1.0, "carried by the changelevel");
        w.server.vm.gset_float("serverflags", 3.0);
        try_restart(&mut w, &mut Vec::new());
        assert_eq!(w.server.serverflags(), 1.0, "restart keeps the entry rune only");
    }

    #[test]
    fn sizeup_sizedown_console_commands_and_default_binds() {
        assert_eq!(boot(), 1);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        console_toggle();
        run_console_line("sizeup");
        assert_eq!(viewsize(), 110.0);
        run_console_line("sizeup");
        run_console_line("sizeup");
        assert_eq!(viewsize(), 120.0, "bounded at 120");
        run_console_line("sizedown");
        assert_eq!(viewsize(), 110.0);
        run_console_line("viewsize");
        let printed = APP.with(|c| {
            c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string)
        });
        assert_eq!(printed.as_deref(), Some("\"viewsize\" is \"110\""), "Cvar_Command print");
        run_console_line("viewsize 5");
        assert_eq!(viewsize(), 30.0, "bounded at 30");
        run_console_line("viewsize 100");
        console_toggle();
        // default.cfg: `-` sizedown, `=` / `+` sizeup — in the game only
        // (key_dest == key_game), like every binding.
        key_down(i32::from(b'-'));
        key_up(i32::from(b'-'));
        assert_eq!(viewsize(), 90.0, "'-' is sizedown");
        key_down(i32::from(b'='));
        key_up(i32::from(b'='));
        key_down(i32::from(b'+'));
        key_up(i32::from(b'+'));
        assert_eq!(viewsize(), 110.0, "'=' and '+' are sizeup");
        menu_cancel(); // open the menu: keys no longer reach the bindings
        key_down(i32::from(b'-'));
        key_up(i32::from(b'-'));
        assert_eq!(viewsize(), 110.0, "no binding runs while the menu is up");
    }

    #[test]
    fn r_threads_is_a_cvar_every_frame_hands_the_renderer() {
        let last_line = || {
            APP.with(|c| c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string))
        };
        let threads = || walk_mut(|w| w.renderer.threads());
        assert_eq!(boot(), 1);
        close_menu();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().hw_threads = 6);
        console_toggle();
        run_console_line("r_threads");
        assert_eq!(last_line().as_deref(), Some("\"r_threads\" is \"0\" (6 of 6 threads)"));
        step(0.0);
        assert_eq!(threads(), 6, "0: every thread the host offers");
        run_console_line("r_threads 0");
        APP.with(|c| c.borrow_mut().as_mut().unwrap().hw_threads = 1);
        step(0.0);
        assert_eq!(threads(), 1, "no threads offered: one");
        run_console_line("r_threads 3");
        step(0.0);
        assert_eq!(threads(), 3);
        console_toggle();
        // A game the host builds afresh draws with it from its first frame.
        assert_eq!(boot(), 1);
        step(0.0);
        assert_eq!(threads(), 3);
    }

    #[test]
    fn wasm_extra_commands_print_and_set_like_cvars() {
        use crate::menu::extras;
        let last_line = || {
            APP.with(|c| c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string))
        };
        // No walk needed: they are host settings, like viewsize.
        console_toggle();
        assert_eq!(extras(), 0, "every extra starts off");
        run_console_line("wasm_uncapped");
        assert_eq!(last_line().as_deref(), Some("\"wasm_uncapped\" is \"0\""));
        run_console_line("wasm_uncapped 1");
        assert_eq!(extras(), 1);
        run_console_line("WASM_SHOWFPS 1"); // Cmd_ExecuteString is case-blind
        assert_eq!(extras(), 3);
        run_console_line("wasm_showfps");
        assert_eq!(last_line().as_deref(), Some("\"wasm_showfps\" is \"1\""));
        run_console_line("wasm_uncapped 0");
        run_console_line("wasm_showfps junk"); // atof("junk") = 0: off
        assert_eq!(extras(), 0);
        run_console_line("wasm_exactpersp 1");
        assert_eq!(extras(), 4);
        run_console_line("wasm_help");
        let help: Vec<String> = APP.with(|c| {
            c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect()
        });
        assert!(help.iter().any(|l| l == "  wasm_uncapped 0|1  no 72 fps cap"), "{help:?}");
        assert!(help.iter().any(|l| l == "  wasm_showfps 0|1   frame rate"), "{help:?}");
        assert!(help.iter().all(|l| l.len() <= 38), "fits a 320-wide console: {help:?}");
    }

    /// Final review (UI): Multiplayer > Setup through the keys (M_Setup_Key),
    /// and the cvars it sets through the console: `name` / `color` (the
    /// client halves of Host_Name_f / Host_Color_f) and `hostname`.
    #[test]
    fn setup_sets_the_name_and_colours_the_console_reads() {
        use crate::input::{key_event, press};
        use quake_rs::keys::{K_BACKSPACE, K_DOWNARROW, K_ENTER, K_ESCAPE, K_RIGHTARROW, K_SHIFT, K_UPARROW};
        assert_eq!(boot(), 1); // the menu over e1m1, on Main
        press(K_DOWNARROW);
        press(K_ENTER); // Multiplayer
        press(K_DOWNARROW);
        press(K_DOWNARROW);
        press(K_ENTER); // Setup, on Accept Changes
        assert_eq!(crate::menu::menu_screen_id(), 11);
        press(K_UPARROW);
        press(K_UPARROW);
        press(K_RIGHTARROW); // shirt 1
        press(K_UPARROW); // Your name
        for _ in 0..6 {
            press(K_BACKSPACE);
        }
        key_event(i32::from(K_SHIFT), 1, 0);
        press(b'r'); // keyshift: R
        key_event(i32::from(K_SHIFT), 0, 0);
        for b in b"anger" {
            press(*b);
        }
        for _ in 0..3 {
            press(K_DOWNARROW);
        }
        press(K_ENTER); // Accept Changes
        assert_eq!(crate::menu::menu_screen_id(), 4, "back on Multiplayer");
        press(K_ESCAPE);
        press(K_ESCAPE);
        console_toggle();
        let last = || APP.with(|c| c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string).unwrap());
        run_console_line("name");
        assert_eq!(last(), "\"name\" is \"Ranger\"");
        run_console_line("color");
        let lines = APP.with(|c| c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect::<Vec<_>>());
        assert_eq!(lines[lines.len() - 2..], ["\"color\" is \"1 0\"", "color <0-13> [0-13]"]);
        run_console_line("color 4 15");
        run_console_line("_cl_color");
        assert_eq!(last(), "\"_cl_color\" is \"77\"", "4*16 + 13 (clamped)");
        run_console_line("color 6");
        run_console_line("_cl_color");
        assert_eq!(last(), "\"_cl_color\" is \"102\"", "one colour is both");
        run_console_line("name \"The Ranger\"");
        run_console_line("_cl_name");
        assert_eq!(last(), "\"_cl_name\" is \"The Ranger\"");
        run_console_line("hostname");
        assert_eq!(last(), "\"hostname\" is \"UNNAMED\"");
        run_console_line("hostname quake-rs");
        run_console_line("hostname");
        assert_eq!(last(), "\"hostname\" is \"quake-rs\"");
    }

    /// Final review (UI): menu.c registers `help` as M_Menu_Help_f — the
    /// Help/Ordering screen on its first page, the menu taking the keyboard
    /// from the console — where the port printed its own command list; that
    /// list is `wasm_help` now. `cmdlist` was the port's too: id has none.
    #[test]
    fn help_is_the_help_screen_and_wasm_help_the_ports_list() {
        use crate::menu::{menu_right, menu_screen_id, menu_visible};
        assert_eq!(boot(), 1);
        close_menu();
        console_toggle();
        run_console_line("help");
        assert_eq!((menu_visible(), menu_screen_id(), console_visible()), (1, 8, 0), "the Help screen");
        menu_right();
        assert_eq!(APP.with(|c| c.borrow().as_ref().unwrap().menu.help_page()), 1);
        menu_cancel(); // M_Help_Key K_ESCAPE -> M_Menu_Main_f
        assert_eq!(menu_screen_id(), 0);
        menu_cancel();
        console_toggle();
        run_console_line("help");
        assert_eq!(APP.with(|c| c.borrow().as_ref().unwrap().menu.help_page()), 0, "help_page = 0");
        menu_cancel();
        menu_cancel();
        console_toggle();
        let lines = || APP.with(|c| c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect::<Vec<_>>());
        run_console_line("wasm_help");
        assert!(lines().iter().any(|l| l == "this port's commands:"), "{:?}", lines());
        run_console_line("cmdlist");
        assert_eq!(lines().last().map(String::as_str), Some("unknown command: cmdlist"));
    }

    // -----------------------------------------------------------------------
    // Drop-down console
    // -----------------------------------------------------------------------

    /// Read the player edict's `flags` field from the live walk (0.0 if no walk).
    fn player_flags() -> i32 {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|a| a.walk.as_ref())
                .map(|w| w.server.vm.ent_get_float(w.player, "flags") as i32)
                .unwrap_or(0)
        })
    }

    fn console_scrollback() -> usize {
        APP.with(|c| {
            c.borrow().as_ref().map(|a| a.console.line_count()).unwrap_or(0)
        })
    }

    #[test]
    fn console_god_toggles_the_player_flags_bit() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        console_toggle(); // open
        assert_eq!(console_visible(), 1);

        let before = player_flags();
        assert_eq!(before & FL_GODMODE, 0, "godmode starts off");
        run_console_line("god");
        let after = player_flags();
        assert_ne!(after & FL_GODMODE, 0, "the god command set FL_GODMODE on the player");
        // The command echoed the input line + its result into the scrollback.
        assert!(console_scrollback() >= 2, "god echoed the line and a result");

        // A second `god` toggles it back off.
        run_console_line("god");
        assert_eq!(player_flags() & FL_GODMODE, 0, "a second god clears FL_GODMODE");
    }

    #[test]
    fn console_unknown_command_prints_an_error() {
        assert_eq!(boot(), 1);
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.clear());
        run_console_line("frobnicate now");
        // Exactly two lines: the echoed "]frobnicate now" and the error message.
        // (A known command would echo the line + its own variable output; an
        // unknown one always produces precisely the echo + one error line.)
        let lines = console_scrollback();
        assert_eq!(lines, 2, "unknown command echoes the line and one error line");
    }

    #[test]
    fn console_give_changes_the_field() {
        assert_eq!(boot(), 1);
        console_toggle();
        // give h 100 sets the player's health field.
        run_console_line("give h 100");
        assert_eq!(player_field("health"), 100.0, "give h set health");
        // give s 50 sets ammo_shells.
        run_console_line("give s 50");
        assert_eq!(player_field("ammo_shells"), 50.0, "give s set ammo_shells");
        // give 2 grants + selects the shotgun (items bit 1, weapon bit 1).
        run_console_line("give 2");
        let items = player_field("items") as i32;
        assert_ne!(items & IT_SHOTGUN, 0, "give 2 set the shotgun items bit");
        assert_eq!(player_field("weapon") as i32, IT_SHOTGUN, "give 2 selected the shotgun");
    }

    #[test]
    fn console_impulse_queues_next_impulse() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("impulse 9");
        let n = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().next_impulse
        });
        assert_eq!(n, 9, "impulse 9 queued the give-all cheat impulse");
    }

    #[test]
    fn console_clear_and_echo_and_noclip_fly_kill() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("echo hello world");
        assert!(console_scrollback() >= 2, "echo printed text");
        run_console_line("clear");
        // After clear, only the echoed "]clear" line (pushed before exec) remains
        // — clear empties everything that came before it.
        assert_eq!(console_scrollback(), 0, "clear empties the scrollback");

        // noclip toggles movetype WALK <-> NOCLIP.
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_NOCLIP, "noclip set NOCLIP");
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "noclip toggled back to WALK");
        // fly toggles WALK <-> FLY.
        run_console_line("fly");
        assert_eq!(player_field("movetype"), MOVETYPE_FLY, "fly set FLY");

        // `kill` routes through the QuakeC ClientKill (Host_Kill_f), whose
        // respawn() issues localcmd("restart\n") in single player: the level
        // reloads and the player comes back ALIVE with the level-ENTRY loadout
        // — wiping the cheats above and the marker rockets we set here.
        run_console_line("give r 5"); // marker: not part of the entry parms
        assert_eq!(player_field("ammo_rockets"), 5.0);

        // An already-dead player is refused (Host_Kill_f's guard): no QuakeC
        // runs, no restart — the live state is untouched.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 0.0);
        });
        run_console_line("kill");
        assert_eq!(
            player_field("ammo_rockets"),
            5.0,
            "a refused kill must not reload the level"
        );
        assert_eq!(player_field("health"), 0.0, "a refused kill leaves the player as-is");

        // Alive again: kill -> ClientKill -> respawn() -> localcmd("restart")
        // -> the level restarts. Fresh player: alive, walking, marker wiped.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 70.0);
        });
        run_console_line("kill");
        assert_eq!(player_field("health"), 100.0, "suicide restarted the level: alive");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(
            player_field("movetype"),
            MOVETYPE_WALK,
            "fresh player walks (the fly cheat did not survive the restart)"
        );
        assert_eq!(
            player_field("ammo_rockets"),
            0.0,
            "restart restored the level-ENTRY parms (the marker is gone)"
        );
    }

    #[test]
    fn console_map_failure_keeps_level_and_prints_not_found() {
        assert_eq!(boot(), 1);
        console_toggle();
        let had_walk = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(had_walk);
        run_console_line("map nosuchmap");
        // The walk is unchanged and the console stays OPEN (failure path).
        let still = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(still, "a missing map leaves the current walk in place");
        assert_eq!(console_visible(), 1, "a failed map keeps the console open");
    }

    #[test]
    fn console_command_guards_missing_walk() {
        // A fresh app with NO walk: a game command prints "no active game", no panic.
        ensure_app(|a| {
            a.walk = None;
            a.console.open = true;
            a.console.clear();
        });
        run_console_line("god");
        // Echoed line + "no active game".
        assert!(console_scrollback() >= 2, "god with no walk prints a guard message");
    }

    /// QuakeC deadflag values (client.qc / defs.qc).
    const DEAD_DYING: f32 = 1.0;
    const DEAD_DEAD: f32 = 2.0;
    const DEAD_RESPAWNABLE: f32 = 3.0;

    /// End-to-end proof of the single-player death -> respawn chain on the REAL
    /// embedded e1m1 + progs.dat, through the exact path the browser uses
    /// (`boot()` / `step()`):
    ///
    ///   self-fired rocket -> T_RadiusDamage -> T_Damage -> Killed -> PlayerDie
    ///   (deadflag = DEAD_DYING, movetype = TOSS: corpse physics) -> the
    ///   death-anim THINKS play out while health < 0 (PlayerDead -> deadflag =
    ///   DEAD_DEAD) -> PlayerDeathThink (run from PlayerPreThink while dead)
    ///   sees all buttons released (deadflag = DEAD_RESPAWNABLE) -> a +attack
    ///   press reaches the QuakeC `button0` field while dead -> respawn() ->
    ///   localcmd("restart\n") -> take_pending_restart -> try_restart reloads
    ///   the level with the level-ENTRY parms: the player is alive at the spawn
    ///   point in a reset world.
    #[test]
    fn real_death_chain_respawns_via_restart() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        set_resolution(320, 200); // keep the per-step debug render cheap
        // boot() opens the main menu, which gates gameplay input; close it.
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Arm the rocket launcher and aim straight down (test SETUP only — the
        // kill itself travels the real QuakeC damage chain). Health 30: one
        // self-rocket deals ~55 (radius 120 minus distance falloff, halved for
        // attacker == target), leaving ~-25 — dead, but above the -40 gib line,
        // so the longer death-ANIM think chain runs.
        let spawn_org = APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.pitch = 80.0; // straight down (the +80 clamp)
            w.server.vm.ent_set_float(p, "health", 30.0);
            let items = w.server.vm.ent_get_float(p, "items") as i32 | IT_RL;
            w.server.vm.ent_set_float(p, "items", items as f32);
            w.server.vm.ent_set_float(p, "ammo_rockets", 5.0);
            player_start(&w.bsp.entities).expect("e1m1 has info_player_start").0
        });

        // Select the RL through the REAL impulse path (PlayerPostThink ->
        // W_WeaponFrame -> ImpulseCommands -> W_ChangeWeapon -> W_SetCurrentAmmo).
        APP.with(|c| {
            c.borrow_mut().as_mut().unwrap().walk.as_mut().unwrap().next_impulse = 7
        });
        step(0.05);
        assert_eq!(
            player_field("weapon") as i32,
            IT_RL,
            "impulse 7 selected the rocket launcher"
        );

        // Settle on the floor, then FIRE for one frame and release.
        for _ in 0..4 {
            step(0.05);
        }
        let mut trace: Vec<(usize, f32, f32)> = Vec::new(); // (frame, health, deadflag)
        trace.push((0, player_field("health"), player_field("deadflag")));
        set_attack(1);
        step(0.05);
        set_attack(0);

        // Ride the death out with all buttons released, tracing deadflag per
        // frame. Once DYING, hold +attack for ONE frame to prove UserCmd buttons
        // reach the QuakeC button0 field while dead (nothing consumes it during
        // DEAD_DYING: PlayerPreThink returns early and W_WeaponFrame is
        // deadflag-gated), then release well before the DEAD_DEAD button-free
        // wait.
        let mut probed_button_while_dead = false;
        for i in 1..=120 {
            step(0.05);
            let (h, df) = (player_field("health"), player_field("deadflag"));
            trace.push((i, h, df));
            if df == DEAD_DYING && !probed_button_while_dead {
                set_attack(1);
                step(0.05);
                assert_eq!(
                    player_field("button0"),
                    1.0,
                    "the attack button reaches QuakeC button0 while dead"
                );
                assert!(player_field("health") < 0.0, "the probe ran while dead");
                set_attack(0);
                step(0.05); // settle the release
                probed_button_while_dead = true;
            }
            if df == DEAD_RESPAWNABLE {
                break;
            }
        }
        assert!(probed_button_while_dead, "the DEAD_DYING phase was observed");

        // The chain, in order: alive -> DYING (PlayerDie, via the real
        // T_Damage) -> DEAD (the death-anim thinks ran out while health < 0 —
        // client thinks RUN while dead) -> RESPAWNABLE (PlayerDeathThink, run
        // from PlayerPreThink while dead, saw every button released).
        let mut seq: Vec<f32> = Vec::new();
        for &(i, h, df) in &trace {
            if seq.last() != Some(&df) {
                seq.push(df);
                // Evidence of the chain as it executed (visible with --nocapture).
                eprintln!("deadflag -> {df} at frame {i} (health {h})");
            }
        }
        assert_eq!(
            seq,
            vec![0.0, DEAD_DYING, DEAD_DEAD, DEAD_RESPAWNABLE],
            "deadflag progression; full trace: {trace:?}"
        );

        // DEAD_RESPAWNABLE: the dead player waits for a button. Press +attack:
        // PlayerDeathThink consumes it and calls respawn() ->
        // localcmd("restart\n"); step_walk drains take_pending_restart() and
        // try_restart() reloads the level inside this same step.
        let t_before = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().server.time()
        });
        set_attack(1);
        step(0.05);
        set_attack(0);

        assert_eq!(player_field("health"), 100.0, "respawned alive (entry health)");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "fresh player walks");
        let items = player_field("items") as i32;
        assert_eq!(
            items & IT_RL,
            0,
            "the cheat rocket launcher did NOT survive (level-ENTRY parms restored)"
        );
        assert_ne!(items & IT_SHOTGUN, 0, "the entry loadout (shotgun) is back");
        assert_eq!(player_field("ammo_rockets"), 0.0, "cheat rockets wiped");
        assert_eq!(player_field("ammo_shells"), 25.0, "entry shells restored");

        // Back at the spawn point, in a rebuilt world (server time restarted).
        let (org, t_after) = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            (w.server.vm.ent_get_vector(w.player, "origin"), w.server.time())
        });
        assert!(
            (org[0] - spawn_org[0]).abs() < 16.0
                && (org[1] - spawn_org[1]).abs() < 16.0
                && (org[2] - spawn_org[2]).abs() < 64.0,
            "respawned at the spawn point: {org:?} vs {spawn_org:?}"
        );
        assert!(
            t_after < t_before,
            "the world was rebuilt: server time restarted ({t_after} < {t_before})"
        );
    }

    /// An ENVIRONMENT kill reaches the same chain: slime damage is dealt by
    /// client.qc `WaterMove` (run from PlayerPreThink) -> `T_Damage(self, world,
    /// world, 4*waterlevel)` -> Killed -> PlayerDie — the attacker==world branch
    /// (no knockback), unlike the rocket. Teleporting the player into e1m1's
    /// slime pool is test setup; the damage itself travels the real QuakeC path,
    /// and the death rides the same anim -> DEAD_RESPAWNABLE -> button ->
    /// restart tail.
    #[test]
    fn environment_slime_kill_enters_the_same_death_chain() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Find a submerged spot: scan the world bounds on a coarse grid for
        // CONTENTS_SLIME that is still slime 48 units higher, so a player with
        // origin 24 above the probe has the eye (origin + 22) under the surface
        // (waterlevel 3 -> 12 damage per slime tick).
        let slime: Option<[f32; 3]> = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            let world = &w.bsp.models[0];
            let (mins, maxs) = (world.mins, world.maxs);
            let mut z = mins[2] + 16.0;
            while z < maxs[2] {
                let mut x = mins[0] + 16.0;
                while x < maxs[0] {
                    let mut y = mins[1] + 16.0;
                    while y < maxs[1] {
                        if quake_rs::world::point_contents(&w.bsp, [x, y, z])
                            == quake_rs::bsp::CONTENTS_SLIME
                            && quake_rs::world::point_contents(&w.bsp, [x, y, z + 48.0])
                                == quake_rs::bsp::CONTENTS_SLIME
                        {
                            return Some([x, y, z]);
                        }
                        y += 64.0;
                    }
                    x += 64.0;
                }
                z += 64.0;
            }
            None
        });
        let p = slime.expect("e1m1 has a slime pool deep enough to submerge in");

        // Drop the player in with 5 health: the first WaterMove slime tick
        // (4 * waterlevel) kills through the real chain. waterlevel is sensed
        // during the move phase, so the kill lands a couple of frames in.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let pl = w.player;
            w.server.vm.ent_set_vector(pl, "origin", [p[0], p[1], p[2] + 24.0]);
            w.server.vm.ent_set_vector(pl, "velocity", [0.0, 0.0, 0.0]);
            w.server.vm.ent_set_float(pl, "health", 5.0);
        });
        let mut died = false;
        for _ in 0..20 {
            step(0.05);
            if player_field("deadflag") >= DEAD_DYING {
                died = true;
                break;
            }
        }
        assert!(died, "slime damage killed through PlayerDie (deadflag set)");
        assert!(player_field("health") < 0.0, "the slime tick took health below zero");

        // Same tail as the rocket death: anim out, button, restart, alive.
        let mut respawnable = false;
        for _ in 0..120 {
            step(0.05);
            if player_field("deadflag") == DEAD_RESPAWNABLE {
                respawnable = true;
                break;
            }
        }
        assert!(respawnable, "the death anim ran out to DEAD_RESPAWNABLE");
        set_attack(1);
        step(0.05);
        set_attack(0);
        assert_eq!(
            player_field("health"),
            100.0,
            "respawned alive after the environment kill"
        );
        assert_eq!(player_field("deadflag"), 0.0);
    }
}
