//! The drop-down console's key half — console.c's `Con_ToggleConsole_f` and
//! keys.c's `Key_Console` (typing, backspace, enter): the exports the page
//! routes the keyboard to while the console is down. Submitted lines run
//! through [`execute_console_command`].

use std::collections::VecDeque;

use quake_rs::console::{ConCursor, ConOp};

use crate::app::{ensure_app, APP};
use crate::host_cmd::execute_console_command;

/// `NUM_CON_TIMES` (console.c): the notify overlay shows the last 4 lines.
const NUM_CON_TIMES: usize = 4;
/// `con_notifytime` ("3"): seconds a notify line stays up.
const CON_NOTIFYTIME: f32 = 3.0;

/// The console text as the notify overlay sees it — `Con_Print` (console.c)
/// laying printed text into `con_linewidth`-wide lines, word-wrapped
/// ([`ConCursor`]), each line stamped with the time its first character
/// arrived (`con_times`), and `Con_DrawNotify` showing the last
/// [`NUM_CON_TIMES`] lines younger than `con_notifytime`. A line shows as soon
/// as it starts (a print need not end in `\n`), blank lines included.
///
/// In the C the notify lines are the tail of the console's own text buffer:
/// the same text also goes to the drop-down console's scrollback. The mode
/// that printed it keeps it in [`ConNotify::take_printed`] until the host
/// hands it to the App's console ([`Console::print`](quake_rs::console::Console::print)).
#[derive(Default)]
pub(crate) struct ConNotify {
    /// The last console lines and their `con_times` stamps.
    lines: VecDeque<(String, f32)>,
    /// `Con_Print`'s position.
    cursor: ConCursor,
    /// Text printed since the host last took it for the console scrollback.
    printed: String,
}

impl ConNotify {
    /// `Con_Print(txt)` at clock `now`.
    pub(crate) fn print(&mut self, txt: &str, now: f32) {
        let lines = &mut self.lines;
        self.cursor.print(txt, |op| match op {
            ConOp::Linefeed => {
                // Con_Linefeed, and "mark time for transparent overlay".
                lines.push_back((String::new(), now));
                while lines.len() > NUM_CON_TIMES {
                    lines.pop_front();
                }
            }
            ConOp::Unlinefeed => {
                lines.pop_back(); // con_current--
            }
            ConOp::Char(c) => {
                if let Some((line, _)) = lines.back_mut() {
                    line.push(c as char);
                }
            }
        });
        self.printed.push_str(txt);
    }

    /// The text printed since the last call, for the console scrollback.
    pub(crate) fn take_printed(&mut self) -> String {
        std::mem::take(&mut self.printed)
    }

    /// `Con_DrawNotify`'s lines at clock `now`, top to bottom: the last
    /// [`NUM_CON_TIMES`] console lines, skipping any older than
    /// `con_notifytime`.
    pub(crate) fn visible(&self, now: f32) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|(_, t)| now - t <= CON_NOTIFYTIME)
            .map(|(l, _)| l.as_str())
            .collect()
    }

    /// `Con_CheckResize` for a `vid_w x vid_h` framebuffer: text is laid out
    /// `con_linewidth` wide ([`quake_rs::console::con_linewidth`]); a new width
    /// cuts the lines to it and `Con_ClearNotify`s them.
    pub(crate) fn check_resize(&mut self, vid_w: usize, vid_h: usize) {
        let width = quake_rs::console::con_linewidth(vid_w, vid_h);
        if width != self.cursor.width() {
            self.cursor.set_width(width);
            self.lines.clear();
        }
    }

    /// `Con_ClearNotify` (a level load): nothing is shown until new text. The
    /// console keeps the text (and still gets what was printed).
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
    }
}

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

    /// CENSUS L11: Con_Print lays text into 38-column console lines (word
    /// wrapped, a line stamped when it starts) and Con_DrawNotify shows the
    /// last 4 younger than con_notifytime — fragments join, blank lines count.
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

    #[test]
    fn notify_lines_follow_con_print() {
        use super::ConNotify;
        let mut n = ConNotify::default();
        n.print("You receive ", 1.0);
        n.print("25", 1.0);
        assert_eq!(n.visible(1.0), ["You receive 25"], "a partial line already shows");
        n.print(" health\n", 1.0);
        assert_eq!(n.visible(1.0), ["You receive 25 health"]);
        // 38 columns: the word that would cross the edge starts a new line.
        n.print("aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd\n", 2.0);
        assert_eq!(
            n.visible(2.0),
            ["You receive 25 health", "aaaaaaaaaa bbbbbbbbbb cccccccccc ", "dddddddddd"]
        );
        n.print("\n", 2.5); // a blank line takes a slot
        n.print("last\n", 2.5);
        assert_eq!(n.visible(2.5), ["aaaaaaaaaa bbbbbbbbbb cccccccccc ", "dddddddddd", "", "last"]);
        assert_eq!(n.visible(5.2), ["", "last"], "con_notifytime 3 s from each line's start");
        n.clear();
        assert!(n.visible(5.2).is_empty());
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
