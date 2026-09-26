//! Automation — the protocol's `Call` records, answered by `Reply`: the
//! page's own actions (its walk and demo buttons boot a mode), and the hooks
//! the browser checks (`web/verify_*.py`) and the benchmark (`web/bench.py`)
//! drive the game and read it back through. These were the wasm module's
//! `extern "C"` exports; each is still the same function, now named in a
//! call line (`"set_resolution 640 400"`) instead of exported.
//!
//! A call runs between two frames, in the order it arrived among the
//! page's other events. Its answer is a number (`1`/`0` for a flag, `NaN`
//! for a name nobody knows) and, for the few that read text, a string.

use crate::app::{boot, boot_attract, boot_demo, in_walk_mode, APP};
use crate::cl_demo::timedemo_running;
use crate::console::{
    console_backspace, console_char, console_enter, console_toggle, console_visible,
};
use crate::host::step;
use crate::host_cmd::execute_console_command;
use crate::input::{
    key_clear_states, key_down, key_event, key_is_down, key_up, look, mouse_move,
    mouse_sensitivity, player_pitch, pointer_unlocked, set_attack, set_impulse, set_jump,
    set_move, set_movedown,
};
use crate::menu::{
    extras, menu_backspace, menu_bind_grabbing, menu_bind_key, menu_cancel, menu_down, menu_left,
    menu_point, menu_quit_no, menu_quit_yes, menu_right, menu_screen_id, menu_select, menu_tap, menu_up,
    menu_visible, set_extras,
};
use crate::snd_dma::{listener, sound_generation, volume};
use crate::vid::{height, scaled_2d, set_resolution, set_scaled_2d, set_viewsize, set_window, viewsize, width};

/// An answer: a number, and text for the calls that read some.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    pub(crate) value: f64,
    pub(crate) text: String,
}

impl From<f64> for Answer {
    fn from(value: f64) -> Answer {
        Answer { value, text: String::new() }
    }
}

impl From<i32> for Answer {
    fn from(v: i32) -> Answer {
        Answer::from(f64::from(v))
    }
}

impl From<f32> for Answer {
    fn from(v: f32) -> Answer {
        Answer::from(f64::from(v))
    }
}

/// Run a call that answers nothing (0).
fn done(f: impl FnOnce()) -> Answer {
    f();
    Answer::from(0.0)
}

/// The threads the renderer draws the next frame with (`r_threads`
/// resolved against what the host offers).
fn render_threads() -> i32 {
    let mut n = 1;
    crate::app::ensure_app(|a| n = a.settings.cvars.threads.resolve(a.hw_threads));
    n as i32
}

/// Run one call line, `name arg...` (numbers, or the rest of the line for
/// `exec`).
pub(crate) fn call(line: &str) -> Answer {
    let line = line.trim();
    let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
    let args: Vec<f64> = rest.split_whitespace().map(|a| a.parse().unwrap_or(f64::NAN)).collect();
    // Argument `i` as the export's parameter type took it (a JS number into
    // an i32 / f32 parameter: truncated / rounded; a missing one is 0).
    let f = |i: usize| args.get(i).copied().unwrap_or(0.0);
    let int = |i: usize| f(i) as i32;
    let real = |i: usize| f(i) as f32;
    match name {
        // Modes (the page's walk / demo buttons, its boot).
        "boot" => boot().into(),
        "boot_demo" => boot_demo().into(),
        "boot_attract" => boot_attract().into(),
        "in_walk_mode" => in_walk_mode().into(),
        "timedemo_running" => timedemo_running().into(),
        // The game's files: registered (1) or shareware (0), and the search
        // path (`path`'s lines).
        "content" => Answer {
            value: f64::from(u8::from(crate::common::registered())),
            text: crate::common::path_lines().join("\n"),
        },
        // One host frame outside the page's refresh (automation: `dt` 0 is
        // the frozen frame); its picture is not sent.
        "step" => step(real(0)).into(),
        // Video.
        "width" => width().into(),
        "height" => height().into(),
        "set_resolution" => done(|| set_resolution(int(0), int(1))),
        // The page's box in device pixels (what its `Window` record says).
        "set_window" => done(|| set_window(f(0) as u32, f(1) as u32)),
        "set_video" => crate::vid::set_video(rest).into(),
        "render_threads" => render_threads().into(),
        "viewsize" => viewsize().into(),
        "set_viewsize" => done(|| set_viewsize(real(0))),
        "scaled_2d" => scaled_2d().into(),
        "set_scaled_2d" => done(|| set_scaled_2d(int(0))),
        // Input.
        "key_event" => done(|| key_event(int(0), int(1), int(2))),
        "key_down" => done(|| key_down(int(0))),
        "key_up" => done(|| key_up(int(0))),
        "key_clear_states" => done(key_clear_states),
        "key_is_down" => key_is_down(int(0)).into(),
        "mouse_move" => done(|| mouse_move(real(0), real(1))),
        "pointer_unlocked" => done(pointer_unlocked),
        "look" => done(|| look(real(0), real(1))),
        "player_pitch" => player_pitch().into(),
        "player_field" => player_field(rest.trim()).into(),
        "mouse_sensitivity" => mouse_sensitivity().into(),
        "set_move" => done(|| set_move(real(0), real(1))),
        "set_attack" => done(|| set_attack(int(0))),
        "set_jump" => done(|| set_jump(int(0))),
        "set_movedown" => done(|| set_movedown(int(0))),
        "set_impulse" => done(|| set_impulse(int(0))),
        // The menu.
        "menu_up" => done(menu_up),
        "menu_down" => done(menu_down),
        "menu_left" => done(menu_left),
        "menu_right" => done(menu_right),
        "menu_select" => done(menu_select),
        "menu_cancel" => done(menu_cancel),
        "menu_quit_yes" => done(menu_quit_yes),
        "menu_quit_no" => done(menu_quit_no),
        "menu_backspace" => done(menu_backspace),
        "menu_bind_grabbing" => menu_bind_grabbing().into(),
        "menu_bind_key" => done(|| menu_bind_key(int(0))),
        "menu_screen_id" => menu_screen_id().into(),
        // A finger on the menu (the touch controls): the frame pixel it
        // lifted from, or the one it is on.
        "menu_tap" => menu_tap(real(0), real(1)).into(),
        "menu_point" => menu_point(real(0), real(1)).into(),
        "menu_visible" => menu_visible().into(),
        // (Checks.) Forget a Load/Save slot's listing, as if M_ScanSaves
        // had found no file: opening Load or Save must list it again.
        "menu_forget_save" => done(|| forget_save(int(0))),
        "extras" => extras().into(),
        "set_extras" => done(|| set_extras(int(0))),
        // The console.
        "console_toggle" => done(console_toggle),
        "console_visible" => console_visible().into(),
        "console_char" => done(|| console_char(f(0) as u32)),
        "console_backspace" => done(console_backspace),
        "console_enter" => done(console_enter),
        "console_text" => Answer { value: 0.0, text: console_text() },
        // The settings: a cvar's value (its number, and its text), the
        // profile, and config.cfg's text for them now.
        "cvar" => cvar_value(rest.trim()),
        "profile" => text_answer(|a| a.settings.profile.name().to_string()),
        // The live game's map (`maps/e1m1.bsp`; empty with none).
        "map_name" => text_answer(|a| a.walk.as_ref().filter(|_| a.mode == 0).map(|w| w.map_name.clone()).unwrap_or_default()),
        "config_text" => text_answer(|a| a.settings.config_text()),
        // A console line, as if typed and entered (`Cmd_ExecuteString`).
        "exec" => done(|| execute_console_command(rest)),
        // Sound.
        "volume" => volume().into(),
        "sound_generation" => sound_generation().into(),
        "listener_x" => listener().pos[0].into(),
        "listener_y" => listener().pos[1].into(),
        "listener_z" => listener().pos[2].into(),
        "listener_fwd_x" => listener().forward[0].into(),
        "listener_fwd_y" => listener().forward[1].into(),
        "listener_fwd_z" => listener().forward[2].into(),
        "listener_right_x" => listener().right[0].into(),
        "listener_right_y" => listener().right[1].into(),
        "listener_right_z" => listener().right[2].into(),
        // The checks' view of the frame and a place to look from.
        "frame_hash" => frame_hash(),
        "setpos" => setpos([real(0), real(1), real(2)]).into(),
        _ => bench_call(name, rest).unwrap_or_else(|| f64::NAN.into()),
    }
}

/// The newest frame as the page should show it (RGBA, [`crate::present`]),
/// hashed as the page's `frameHash` and `quaketool play --hash-every` hash
/// theirs: FNV-1a over little-endian 32-bit words, as a number and as hex.
fn frame_hash() -> Answer {
    let rgba = APP.with(|c| c.borrow().as_ref().map(|a| a.present.rgba()).unwrap_or_default());
    let h = rgba
        .chunks_exact(4)
        .fold(0x811c_9dc5u32, |h, w| (h ^ u32::from_le_bytes([w[0], w[1], w[2], w[3]])).wrapping_mul(0x0100_0193));
    Answer { value: f64::from(h), text: format!("{h:08x}") }
}

/// The checks' `setpos x y z`: the live game's player moved there as
/// QuakeC's `setorigin` would (`PF_setorigin`, builtin #2), in noclip so it
/// stays — e.g. into e1m1's start pool (750 898 -354) for an underwater
/// view. 1 when there is a live game to move.
fn setpos(origin: [f32; 3]) -> i32 {
    const MOVETYPE_NOCLIP: f32 = 8.0;
    let mut moved = 0;
    crate::app::ensure_app(|a| {
        let Some(w) = a.walk.as_mut() else { return };
        let (p, vm) = (w.player, &mut w.server.vm);
        vm.ent_set_float(p, "movetype", MOVETYPE_NOCLIP);
        vm.set_gi(quake_rs::progs::OFS_PARM0, p);
        vm.set_gv(quake_rs::progs::OFS_PARM0 + 3, origin);
        vm.argc = 2;
        let setorigin = vm.builtins[2];
        if setorigin(vm).is_ok() {
            vm.ent_set_vector(p, "velocity", [0.0; 3]);
            moved = 1;
        }
    });
    moved
}

/// An answer read off the App: `f`'s text (empty before the App exists).
fn text_answer(f: impl FnOnce(&crate::app::App) -> String) -> Answer {
    let text = APP.with(|c| c.borrow().as_ref().map(f)).unwrap_or_default();
    Answer { value: 0.0, text }
}

/// A float field of the player's edict in the live game, by name (`health`,
/// `ammo_shells`, `weapon`; 0 with no game): what the checks read the
/// game's state back through.
fn player_field(name: &str) -> f32 {
    APP.with(|c| {
        let b = c.borrow();
        let w = b.as_ref().and_then(|a| a.walk.as_ref());
        w.map_or(0.0, |w| w.server.vm.ent_get_float(w.player, name))
    })
}

/// Cvar `name`'s value: its text, and its number (`NaN` for no such cvar).
fn cvar_value(name: &str) -> Answer {
    let Some(var) = quake_rs::cvar::find(name) else { return f64::NAN.into() };
    let text = APP.with(|c| c.borrow().as_ref().map(|a| var.get(&a.settings.cvars))).unwrap_or_default();
    Answer { value: text.parse().unwrap_or(f64::NAN), text }
}

/// Blank slot `slot`'s listing in the Load/Save menus.
fn forget_save(slot: i32) {
    if let Ok(slot) = usize::try_from(slot) {
        crate::app::ensure_app(|a| a.menu.set_save_comment(slot, String::new()));
    }
}

/// The console's scrollback as text, one line per `\n`, a character per
/// byte of the console's font (what the browser checks read the game's
/// prints through: the `timedemo` line, "player paused the game").
fn console_text() -> String {
    APP.with(|c| {
        let b = c.borrow();
        let mut t = String::new();
        if let Some(a) = b.as_ref() {
            for line in a.console.lines() {
                t.push_str(line);
                t.push('\n');
            }
        }
        t
    })
}

/// The benchmark's calls (`--features bench`, [`crate::bench`]): the value
/// names, the timers on or off, and a workload booted with its scripted
/// input.
#[cfg(feature = "bench")]
fn bench_call(name: &str, rest: &str) -> Option<Answer> {
    Some(match name {
        "bench_names" => Answer { value: 0.0, text: crate::bench::NAMES.to_string() },
        "bench_enable" => done(|| crate::bench::bench_enable(rest.trim().parse().unwrap_or(0))),
        "bench_start" => crate::bench::bench_start(rest.trim()).into(),
        _ => return None,
    })
}

#[cfg(not(feature = "bench"))]
fn bench_call(_name: &str, _rest: &str) -> Option<Answer> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_reach_the_functions_the_page_exported() {
        assert_eq!(call("boot").value, 1.0);
        assert_eq!(call("menu_visible").value, 1.0, "boot opens the menu");
        call("menu_cancel");
        assert_eq!(call("menu_visible").value, 0.0);
        call("set_resolution 640 400");
        assert_eq!((call("width").value, call("height").value), (640.0, 400.0));
        call("exec viewsize 70");
        assert_eq!(call("viewsize").value, 70.0);
        call("exec echo hello there");
        assert!(call("console_text").text.contains("hello there\n"));
        call("exec vid_pixelsize 3");
        assert_eq!((call("cvar vid_pixelsize").value, call("cvar vid_pixelsize").text.as_str()), (3.0, "3"));
        assert!(call("cvar nosuch").value.is_nan());
        assert_eq!(call("profile").text, "classic", "the tests start in Classic");
        assert!(call("config_text").text.contains("vid_pixelsize \"3\"\n"));
        assert!(call("no_such_call").value.is_nan());
        assert_eq!(call("player_field health").value, 100.0, "the booted walk's player");
        assert!(call("").value.is_nan());
    }

    #[test]
    fn the_checks_read_the_frame_and_move_the_player() {
        assert_eq!(call("boot").value, 1.0);
        call("menu_cancel");
        call("step 0");
        let seen = call("frame_hash");
        assert_eq!(seen.text, format!("{:08x}", seen.value as u32));
        assert_eq!(call("setpos 750 898 -354").value, 1.0, "into e1m1's start pool");
        call("step 0");
        assert_ne!(call("frame_hash").text, seen.text, "the view moved underwater");
    }
}
