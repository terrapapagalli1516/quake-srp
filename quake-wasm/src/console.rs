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
/// Disconnected, with the console covering the screen, it brings up the main
/// menu instead (`M_Menu_Main_f`: there is no game to go back to).
#[no_mangle]
pub extern "C" fn console_toggle() {
    // Con_ToggleConsole_f: `memset (con_times, 0, sizeof(con_times))` too —
    // the notify lines are gone after the console goes down or up.
    ensure_app(|a| {
        if a.disconnected && (a.console.open || !a.menu.visible) {
            if a.console.open {
                a.toggle_console();
            }
            a.menu.open();
        } else {
            a.toggle_console();
        }
    });
}

/// `1` when the console is open (capturing the keyboard), else `0`. The page
/// reads this to route keys to the console instead of the game / menu.
/// Disconnected, the console is forced up and takes the typing unless the
/// menu is up (keys.c: `key_game` with `con_forcedup` goes to `Key_Console`).
#[no_mangle]
pub extern "C" fn console_visible() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.console_has_keys() as i32).unwrap_or(0))
}

/// Append one typed character to the console input line. `code` is a Unicode
/// scalar value (the page passes `key.charCodeAt(0)` / `key.codePointAt(0)`).
/// Non-printable codes, the backtick/tilde (the toggle key), and anything while
/// the console is closed are ignored. A no-op once the input line is full.
#[no_mangle]
pub extern "C" fn console_char(code: u32) {
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

/// Delete the last character of the console input line (Backspace). A no-op when
/// the console is closed or the line is empty.
#[no_mangle]
pub extern "C" fn console_backspace() {
    ensure_app(|a| {
        if a.console_has_keys() {
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
            .filter(|a| a.console_has_keys())
            .and_then(|a| a.console.take_input())
    });
    if let Some(line) = line {
        execute_console_command(&line);
    }
}

thread_local! {
    /// The scrollback as [`console_text_len`] last laid it out.
    static CONSOLE_TEXT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A read-only verification export (like `key_is_down`): lay the console's
/// scrollback out as text — one line per `\n`, a byte per character — and
/// return its length; [`console_text_ptr`] points at it. The browser
/// harnesses read what the game printed (the `timedemo` line, "player paused
/// the game") through it.
#[no_mangle]
pub extern "C" fn console_text_len() -> i32 {
    let text: Vec<u8> = APP.with(|c| {
        let b = c.borrow();
        let mut t = Vec::new();
        if let Some(a) = b.as_ref() {
            for line in a.console.lines() {
                t.extend(line.chars().map(|ch| ch as u32 as u8));
                t.push(b'\n');
            }
        }
        t
    });
    let n = text.len() as i32;
    CONSOLE_TEXT.with(|c| *c.borrow_mut() = text);
    n
}

/// Pointer to the text [`console_text_len`] laid out.
#[no_mangle]
pub extern "C" fn console_text_ptr() -> *const u8 {
    CONSOLE_TEXT.with(|c| c.borrow().as_ptr())
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

    #[test]
    fn console_text_lays_out_the_scrollback() {
        ensure_app(|a| {
            a.console.clear();
            a.console.println("first");
            a.console.println("second line");
        });
        let n = console_text_len();
        let text = CONSOLE_TEXT.with(|c| c.borrow().clone());
        assert_eq!(n as usize, text.len());
        assert_eq!(text, b"first\nsecond line\n");
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
