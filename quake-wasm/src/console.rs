//! The drop-down console's key half — console.c's `Con_ToggleConsole_f` and
//! keys.c's `Key_Console` (typing, backspace, enter): the exports the page
//! routes the keyboard to while the console is down. Submitted lines run
//! through [`execute_console_command`].

use crate::app::{ensure_app, APP};
use crate::host_cmd::execute_console_command;

// --- drop-down console: toggle / typing / execution exports (the `~` key) ---

/// Toggle the drop-down console (the `~` / backtick key, Quake's
/// `Con_ToggleConsole_f`). Opening slides the panel down over whatever is
/// playing; closing slides it back. While open the console owns the keyboard.
#[no_mangle]
pub extern "C" fn console_toggle() {
    ensure_app(|a| {
        a.console.toggle();
        // Con_ToggleConsole_f: `memset (con_times, 0, sizeof(con_times))` —
        // the notify lines are gone after the console goes down or up.
        if let Some(w) = a.walk.as_mut() {
            w.notify.clear();
        }
        if let Some(d) = a.demo.as_mut() {
            d.notify.clear();
        }
    });
}

/// `1` when the console is open (capturing the keyboard), else `0`. The page
/// reads this to route keys to the console instead of the game / menu.
#[no_mangle]
pub extern "C" fn console_visible() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.console.open as i32)
            .unwrap_or(0)
    })
}

/// Append one typed character to the console input line. `code` is a Unicode
/// scalar value (the page passes `key.charCodeAt(0)` / `key.codePointAt(0)`).
/// Non-printable codes, the backtick/tilde (the toggle key), and anything while
/// the console is closed are ignored. A no-op once the input line is full.
#[no_mangle]
pub extern "C" fn console_char(code: u32) {
    ensure_app(|a| {
        if !a.console.open {
            return;
        }
        // Reject invalid scalar values; `putchar` further filters control chars
        // and the backtick/tilde toggle key.
        if let Some(ch) = char::from_u32(code) {
            a.console.putchar(ch);
        }
    });
}

/// Delete the last character of the console input line (Backspace). A no-op when
/// the console is closed or the line is empty.
#[no_mangle]
pub extern "C" fn console_backspace() {
    ensure_app(|a| {
        if a.console.open {
            a.console.backspace();
        }
    });
}

/// Submit the console input line (Enter): echo it into the scrollback and
/// execute it against the live game. A no-op when the console is closed or the
/// line is blank. The command may swap the level (`map`) and close the console.
#[no_mangle]
pub extern "C" fn console_enter() {
    // Take the line under the borrow, then execute it (execute_console_command
    // borrows the App again to touch the walk / open-state).
    let line = APP.with(|c| {
        c.borrow_mut()
            .as_mut()
            .filter(|a| a.console.open)
            .and_then(|a| a.console.take_input())
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
