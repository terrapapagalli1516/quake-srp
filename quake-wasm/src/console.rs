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
