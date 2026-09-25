//! The drop-down console's key half — console.c's `Con_ToggleConsole_f` and
//! keys.c's `Key_Console` (typing, backspace, enter): the exports the page
//! routes the keyboard to while the console is down. Submitted lines run
//! through [`execute_console_command`].

use std::collections::VecDeque;

use crate::app::{ensure_app, APP};
use crate::host_cmd::execute_console_command;

/// `con_linewidth` for the 320-wide virtual screen the overlays draw in:
/// `(vid.width >> 3) - 2`.
const CON_LINEWIDTH: usize = (320 >> 3) - 2;
/// `NUM_CON_TIMES` (console.c): the notify overlay shows the last 4 lines.
const NUM_CON_TIMES: usize = 4;
/// `con_notifytime` ("3"): seconds a notify line stays up.
const CON_NOTIFYTIME: f32 = 3.0;

/// The console text as the notify overlay sees it — `Con_Print` (console.c)
/// laying printed text into `con_linewidth`-wide lines, word-wrapped, each
/// line stamped with the time its first character arrived (`con_times`), and
/// `Con_DrawNotify` showing the last [`NUM_CON_TIMES`] lines younger than
/// `con_notifytime`. A line shows as soon as it starts (a print need not end
/// in `\n`), blank lines included.
#[derive(Default)]
pub(crate) struct ConNotify {
    /// The last console lines and their `con_times` stamps.
    lines: VecDeque<(String, f32)>,
    /// `con_x`: the column the next character goes to; 0 = a new line starts.
    con_x: usize,
    /// A `\r` was printed: the next character overwrites the current line.
    cr: bool,
}

impl ConNotify {
    /// `Con_Print(txt)` at clock `now`.
    pub(crate) fn print(&mut self, txt: &str, now: f32) {
        let b = txt.as_bytes();
        for i in 0..b.len() {
            let c = b[i];
            // word wrap: the word starting here doesn't fit on this line.
            let l = b[i..].iter().take(CON_LINEWIDTH).take_while(|&&ch| ch > b' ').count();
            if l != CON_LINEWIDTH && self.con_x + l > CON_LINEWIDTH {
                self.con_x = 0;
            }
            if self.cr {
                self.lines.pop_back(); // con_current--
                self.cr = false;
            }
            if self.con_x == 0 {
                // Con_Linefeed, and "mark time for transparent overlay".
                self.lines.push_back((String::new(), now));
                while self.lines.len() > NUM_CON_TIMES {
                    self.lines.pop_front();
                }
            }
            match c {
                b'\n' => self.con_x = 0,
                b'\r' => {
                    self.con_x = 0;
                    self.cr = true;
                }
                _ => {
                    if let Some((line, _)) = self.lines.back_mut() {
                        line.push(c as char);
                    }
                    self.con_x += 1;
                    if self.con_x >= CON_LINEWIDTH {
                        self.con_x = 0;
                    }
                }
            }
        }
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

    /// `Con_ClearNotify` (a level load): nothing is shown until new text.
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
    ensure_app(|a| a.console.toggle());
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
