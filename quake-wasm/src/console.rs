//! The drop-down console's key half — console.c's `Con_ToggleConsole_f` and
//! keys.c's `Key_Console` (typing, backspace, enter), which `Key_Event`
//! ([`crate::input::key_event`]) hands the console's keys to; plus the
//! calls automation types into the console with. Submitted lines run
//! through [`execute_console_command`].

use quake_rs::keys::{K_BACKSPACE, K_ENTER};

use crate::app::{ensure_app, App, APP};
use crate::host_cmd::execute_console_command;

/// `Key_Console`: a key the console has the keyboard for (Shift applied),
/// `text` the character it types — history, Tab completion over this port's
/// commands and cvars ([`crate::host_cmd::complete`]), the scrollback on the
/// 2-D screen's height. Returns the line Enter submitted, for the caller to
/// execute once the App borrow ends.
pub(crate) fn key_console(a: &mut App, key: u8, text: Option<u8>) -> Option<String> {
    let vid_h = quake_rs::draw::screen_2d(a.render_w, a.render_h).h.max(0) as usize;
    a.console.key(key, text, vid_h, crate::host_cmd::complete)
}

// --- drop-down console: toggle / typing / execution (the `~` key; the
// automation's calls) ---------------------------------------------------------

/// `Con_ToggleConsole_f` — what the console key's `toggleconsole` binding
/// runs (the page sends the key through `key_event`): the console slides
/// down over whatever is playing, or back up. With nothing playing
/// (disconnected, the console covering the screen) closing it brings up the
/// main menu instead: there is no game to go back to.
pub(crate) fn console_toggle() {
    ensure_app(App::toggle_console);
}

/// `1` when the console is open (capturing the keyboard), else `0`. The page
/// reads this to route keys to the console instead of the game / menu.
/// Disconnected, the console is forced up and takes the typing unless the
/// menu is up (keys.c: `key_game` with `con_forcedup` goes to `Key_Console`).
pub(crate) fn console_visible() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.console_has_keys() as i32).unwrap_or(0))
}

/// Type one character into the console input line (automation: the page
/// sends keys through `key_event`). `code` is a Unicode scalar value; only
/// printable ASCII types (`Key_Console`: 32..127) — the backtick/tilde (the
/// toggle key) not either, nor anything while the console does not have the
/// keyboard. A no-op once the input line is full.
pub(crate) fn console_char(code: u32) {
    ensure_app(|a| {
        if !a.console_has_keys() {
            return;
        }
        // Reject invalid scalar values; `putchar` further filters control chars
        // and the backtick/tilde toggle key.
        if let Some(ch) = char::from_u32(code) {
            a.console.putchar(ch);
        }
    });
}

/// Backspace in the console (`Key_Console`). A no-op when the console does
/// not have the keyboard or the line is empty.
pub(crate) fn console_backspace() {
    ensure_app(|a| {
        if a.console_has_keys() {
            let _ = key_console(a, K_BACKSPACE, None);
        }
    });
}

/// Enter in the console (`Key_Console`): echo the line into the scrollback
/// and execute it against the live game. A no-op when the console does not
/// have the keyboard. The command may swap the level (`map`) and close the
/// console.
pub(crate) fn console_enter() {
    // Take the line under the borrow, then execute it (execute_console_command
    // borrows the App again to touch the walk / open-state).
    let line = APP.with(|c| {
        c.borrow_mut()
            .as_mut()
            .filter(|a| a.console_has_keys())
            .and_then(|a| key_console(a, K_ENTER, None))
    });
    if let Some(line) = line {
        execute_console_command(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_the_console_clears_the_notify_lines() {
        // Con_ToggleConsole_f zeroes con_times: nothing printed before the
        // console went down (or up) shows as a notify line after it.
        use crate::app::boot;
        use crate::test_util::{close_menu, walk_mut};
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| {
            let t = w.host_time;
            w.notify.print("You got the shells\n", t);
            assert_eq!(w.notify.visible(t), ["You got the shells"]);
        });
        console_toggle();
        assert!(walk_mut(|w| w.notify.visible(w.host_time).is_empty()), "cleared going down");
        walk_mut(|w| {
            let t = w.host_time;
            w.notify.print("printed while it was down\n", t);
        });
        console_toggle();
        assert!(walk_mut(|w| w.notify.visible(w.host_time).is_empty()), "cleared going up");
    }

    /// CENSUS L11 (the rest): Con_Print writes the console's text buffer, so
    /// what the game prints reaches the drop-down console's scrollback as
    /// well as the notify lines — word-wrapped at con_linewidth (38) there
    /// too. Standing on e1m1's first box of shells makes the QuakeC sprint
    /// "You got the shells" in three fragments.
    #[test]
    fn game_prints_reach_the_console_scrollback() {
        use crate::app::boot;
        use crate::host::step;
        use crate::test_util::{close_menu, walk_mut};
        use quake_rs::progs::OFS_PARM0;
        assert_eq!(boot(), 1);
        close_menu();
        // A 320-wide screen: con_linewidth 38 (Con_CheckResize).
        crate::vid::set_resolution(320, 200);
        step(0.05);
        walk_mut(|w| {
            let vm = &mut w.server.vm;
            let shells = (0..vm.num_edicts() as i32)
                .find(|&e| !vm.edict_free[e as usize] && vm.ent_get_string(e, "classname") == "item_shells")
                .expect("e1m1 has shells");
            let o = vm.ent_get_vector(shells, "origin");
            vm.set_gi(OFS_PARM0, w.player);
            vm.set_gv(OFS_PARM0 + 3, [o[0] + 16.0, o[1] + 16.0, o[2] + 24.0]);
            vm.argc = 2;
            let setorigin = vm.builtins[2];
            setorigin(vm).expect("setorigin");
        });
        step(0.05);
        step(0.05);
        let console = || {
            APP.with(|c| {
                c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect::<Vec<_>>()
            })
        };
        assert!(console().iter().any(|l| l == "You got the shells"), "{:?}", console());
        assert!(walk_mut(|w| w.notify.visible(w.host_time).contains(&"You got the shells")));
        // Fragments join and long text wraps at the console width.
        walk_mut(|w| {
            let t = w.host_time;
            w.notify.print("You got the ", t);
            w.notify.print("Grenade Launcher and a very long tail to wrap\n", t);
        });
        step(0.05);
        let lines = console();
        assert_eq!(
            &lines[lines.len() - 2..],
            ["You got the Grenade Launcher and a ", "very long tail to wrap"]
        );
    }

    /// Second review: Con_Print stamps con_times for every line it lays into
    /// the console text, not only the game's svc_prints, so what the host
    /// prints is a notify line too — "Saving game to s1.sav..." and "done."
    /// after a save from the menu (M_Save_Key sets key_dest = key_game and
    /// stuffs `save s1`). Toggling the console (con_times zeroed) and a level
    /// load (SCR_EndLoadingPlaque's Con_ClearNotify) drop them; the `map`
    /// command's own "loading" line (Host_Map_f prints none) stays out.
    #[test]
    fn console_prints_reach_the_notify_lines() {
        use crate::app::boot;
        use crate::host::step;
        use crate::host_cmd::execute_console_command;
        use crate::menu::{menu_down, menu_select, menu_visible};
        use crate::test_util::walk_mut;
        let notify = || walk_mut(|w| w.notify.visible(w.host_time).iter().map(|l| l.to_string()).collect::<Vec<_>>());
        assert_eq!(boot(), 1); // the menu is up over e1m1
        step(0.05);
        menu_select(); // Single Player
        menu_down();
        menu_down();
        menu_select(); // Save
        menu_down();
        menu_select(); // slot 1: the menu closes and `save s1` runs
        assert_eq!(menu_visible(), 0);
        assert_eq!(notify(), ["Saving game to s1.sav...", "done."]);
        step(0.05);
        assert_eq!(notify(), ["Saving game to s1.sav...", "done."], "shown over the game");
        let scrollback = APP.with(|c| c.borrow().as_ref().unwrap().console.lines().count());
        assert!(scrollback >= 2);
        // Printed with the console down: gone when it goes up again.
        console_toggle();
        execute_console_command("echo typed in the console");
        console_toggle();
        assert!(notify().is_empty());
        // A level load clears them, and `map`'s own line is console-only.
        execute_console_command("echo before the map");
        assert_eq!(notify(), ["before the map"]);
        execute_console_command("map e1m2");
        assert!(notify().is_empty(), "{:?}", notify());
        let last = APP.with(|c| c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string));
        assert_eq!(last.as_deref(), Some("loading e1m2"));
    }

    /// Options > "Go to console" is Con_ToggleConsole_f (M_Options_Key), which
    /// zeroes con_times: the notify lines go (the port opened the console
    /// without it).
    #[test]
    fn go_to_console_is_con_toggleconsole_f() {
        use crate::app::boot;
        use crate::menu::{menu_cancel, menu_down, menu_select, menu_visible};
        use crate::test_util::{close_menu, walk_mut};
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| {
            let t = w.host_time;
            w.notify.print("You got the shells\n", t);
        });
        menu_cancel(); // Escape: Main
        menu_down();
        menu_down();
        menu_select(); // Options
        menu_down(); // Go to console
        menu_select();
        assert_eq!((menu_visible(), console_visible()), (0, 1));
        assert!(walk_mut(|w| w.notify.visible(w.host_time).is_empty()));
    }

    /// Final review (UI): Key_Console's history, Tab completion and
    /// scrollback, through Key_Event as the page sends them. Up/Down walk the
    /// lines entered; Tab completes a command (Cmd_CompleteCommand) and else
    /// a cvar (Cvar_CompleteVariable) with a space after it; PgUp/PgDn move
    /// con_backscroll; Home and End are no console keys, so id's Key_Event
    /// runs their bindings (End: centerview) instead of scrolling; a
    /// character outside ASCII types nothing.
    #[test]
    fn console_history_completion_and_backscroll_through_key_event() {
        use crate::app::boot;
        use crate::input::{key_event, press};
        use crate::test_util::close_menu;
        use quake_rs::keys::{K_DOWNARROW, K_END, K_ENTER, K_PGUP, K_TAB, K_UPARROW};
        assert_eq!(boot(), 1);
        close_menu();
        let typ = |s: &str| {
            for b in s.bytes() {
                key_event(i32::from(b), 1, i32::from(b));
                key_event(i32::from(b), 0, 0);
            }
        };
        let input = || APP.with(|c| c.borrow().as_ref().unwrap().console.input().to_string());
        press(b'`');
        typ("echo one");
        press(K_ENTER);
        typ("echo two");
        press(K_ENTER);
        press(K_UPARROW);
        assert_eq!(input(), "echo two");
        press(K_UPARROW);
        assert_eq!(input(), "echo one");
        press(K_DOWNARROW);
        press(K_DOWNARROW);
        assert_eq!(input(), "");
        typ("tim");
        press(K_TAB);
        assert_eq!(input(), "timedemo ", "a command");
        press(K_UPARROW);
        press(K_DOWNARROW);
        typ("vi");
        press(K_TAB);
        assert_eq!(input(), "viewsize ", "else a cvar");
        typ("110");
        press(K_ENTER);
        assert_eq!(crate::vid::viewsize(), 110.0, "the completed line runs");
        typ("s");
        press(K_TAB);
        assert_eq!(input(), "sizedown ", "the command id registered last comes first");
        press(K_UPARROW);
        press(K_DOWNARROW);
        typ("wasm_h");
        press(K_TAB);
        assert_eq!(input(), "wasm_help ");
        press(K_UPARROW);
        press(K_DOWNARROW);
        key_event(i32::from(b'e'), 1, 0xe9); // é
        key_event(i32::from(b'e'), 0, 0);
        assert_eq!(input(), "", "non-ASCII types nothing");
        // Scrollback: enough lines to scroll, then PgUp; End is centerview's.
        for i in 0..40 {
            execute_console_command(&format!("echo {i}"));
        }
        let back = || APP.with(|c| c.borrow().as_ref().unwrap().console.backscroll());
        press(K_PGUP);
        press(K_PGUP);
        assert_eq!(back(), 4);
        walk_pitch_drift(false);
        press(K_END);
        assert_eq!(back(), 4, "End is not Key_Console's in id's routing");
        assert!(walk_pitch_drift(true), "it ran its binding, centerview");
        execute_console_command("echo new");
        assert_eq!(back(), 0, "a print shows the bottom again");
    }

    /// Set (`set` false: clear) or read the walk's pitch drift flag.
    fn walk_pitch_drift(read: bool) -> bool {
        crate::test_util::walk_mut(|w| {
            if !read {
                w.pitch_drift = false;
                w.pitch_vel = 0.0;
            }
            w.pitch_drift
        })
    }

    /// Every command Tab completes to is one this console runs.
    #[test]
    fn every_completion_is_a_command_the_console_knows() {
        use crate::app::boot;
        use crate::test_util::close_menu;
        for name in crate::host_cmd::COMMANDS {
            assert_eq!(boot(), 1);
            close_menu();
            execute_console_command(name);
            let last = APP.with(|c| c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string));
            assert_ne!(last, Some(format!("unknown command: {name}")), "{name}");
        }
    }

    #[test]
    fn console_text_lays_out_the_scrollback() {
        ensure_app(|a| {
            a.console.clear();
            a.console.println("first");
            a.console.println("second line");
        });
        let text = crate::automation::call("console_text").text;
        assert_eq!(text, "first\nsecond line\n");
    }

    #[test]
    fn console_toggle_flips_visibility_and_gates_typing() {

        // ensure_app exists; start closed.
        ensure_app(|_| {});
        assert_eq!(console_visible(), 0, "console starts closed");
        // Typing while closed is ignored.
        console_char('x' as u32);
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().console.input(), ""));
        console_toggle();
        assert_eq!(console_visible(), 1, "toggle opens the console");
        // The backtick toggle char is never typed even while open.
        console_char('`' as u32);
        console_char('a' as u32);
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().console.input(), "a"));
        console_toggle();
        assert_eq!(console_visible(), 0, "toggle closes the console");
    }
}
