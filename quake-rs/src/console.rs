//! The drop-down console and the notify lines.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/console.c` — `Con_Print`, `Con_DrawInput`, `Con_DrawConsole`,
//! `Con_DrawNotify`; `WinQuake/keys.c` — `Key_Console`, the line editing.

use crate::draw::{draw_char_scaled, draw_string_scaled, fill_rect, screen_2d, Screen2d};
use crate::keys::{
    K_BACKSPACE, K_DOWNARROW, K_END, K_ENTER, K_HOME, K_LEFTARROW, K_MWHEELDOWN, K_MWHEELUP,
    K_PGDN, K_PGUP, K_TAB, K_UPARROW,
};
use crate::menu::realtime_blink_bit;
use crate::render::Image;
use std::collections::VecDeque;

/// Draw the notify lines (`bprint`/`sprint`, Con_DrawNotify): stacked from
/// the very top of the [`screen_2d`] screen (`v = 0`), each character at
/// `(x+1)<<3`.
pub fn draw_notify(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    lines: &[&str],
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = screen_2d(image.w, image.h).scale;
    let mut vy = 0.0;
    for line in lines {
        draw_string_scaled(image, conchars, 8.0, vy, line, scale, 0.0, 0.0);
        vy += 8.0;
    }
}

// ---------------------------------------------------------------------------
// Drop-down console
// ---------------------------------------------------------------------------

/// `CON_TEXTSIZE` (console.c): the console text ring's bytes. It holds
/// `con_totallines = CON_TEXTSIZE / con_linewidth` lines — 431 on a 320-wide
/// screen, 138 at 960 — and so does the scrollback here, the oldest line
/// dropping off the top ([`Console::totallines`]).
pub const CON_TEXTSIZE: usize = 16384;

/// `con_linewidth` (console.c `Con_CheckResize`: `(vid.width >> 3) - 2`) on a
/// 320-wide screen: where [`ConCursor`] starts.
pub const CON_LINEWIDTH: usize = (320 >> 3) - 2;

/// `con_linewidth` for a `vid_w x vid_h` framebuffer: `(vid.width >> 3) - 2`
/// of its [`screen_2d`] screen — 38 at 320 wide, 118 at 960 (38 in every mode
/// under the "scaled 2-D" extra).
pub fn con_linewidth(vid_w: usize, vid_h: usize) -> usize {
    ((screen_2d(vid_w, vid_h).w >> 3) - 2).max(1) as usize
}

/// One step of `Con_Print` on the console text buffer (see [`ConCursor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConOp {
    /// `Con_Linefeed`: a new, empty line starts (and `con_times` is stamped).
    Linefeed,
    /// `con_current--` after a `\r`: the line just started is dropped, so the
    /// next linefeed starts it over.
    Unlinefeed,
    /// A character goes at the end of the current line.
    Char(u8),
}

/// `Con_Print`'s cursor (console.c): `con_x`, the column the next character
/// goes to (0 = a new line starts with it), the pending `\r`, and
/// `con_linewidth` ([`CON_LINEWIDTH`] until [`ConCursor::set_width`]). Text is
/// laid into `con_linewidth`-wide lines: a word that would cross the edge starts a
/// new line (one longer than a line runs on until its remainder fits the next
/// line), `\n` ends the line and `\r` returns to its start. The console scrollback and the notify
/// lines are the same text in the C (one buffer, `con->text`).
#[derive(Debug, Clone, Copy)]
pub struct ConCursor {
    con_x: usize,
    cr: bool,
    width: usize,
}

impl Default for ConCursor {
    fn default() -> Self {
        ConCursor { con_x: 0, cr: false, width: CON_LINEWIDTH }
    }
}

impl ConCursor {
    /// `con_linewidth`: the width text is laid out at.
    pub fn width(&self) -> usize {
        self.width
    }

    /// `Con_CheckResize`'s new `con_linewidth` (at least 1); the text already
    /// laid out is the caller's to cut ([`truncate_line`]).
    pub fn set_width(&mut self, width: usize) {
        self.width = width.max(1);
        self.con_x = self.con_x.min(self.width - 1);
    }

    /// `Con_Print(txt)`: hand each buffer operation to `buf`, in order.
    pub fn print(&mut self, txt: &str, mut buf: impl FnMut(ConOp)) {
        let width = self.width;
        let b = txt.as_bytes();
        for i in 0..b.len() {
            let c = b[i];
            // count word length: `txt[l] <= ' '` on a (signed) char, so a
            // high-bit byte ends a word too.
            let l = b[i..].iter().take(width).take_while(|&&ch| (ch as i8) > b' ' as i8).count();
            // word wrap
            if l != width && self.con_x + l > width {
                self.con_x = 0;
            }
            if self.cr {
                buf(ConOp::Unlinefeed);
                self.cr = false;
            }
            if self.con_x == 0 {
                buf(ConOp::Linefeed);
            }
            match c {
                b'\n' => self.con_x = 0,
                b'\r' => {
                    self.con_x = 0;
                    self.cr = true;
                }
                _ => {
                    buf(ConOp::Char(c));
                    self.con_x += 1;
                    if self.con_x >= width {
                        self.con_x = 0;
                    }
                }
            }
        }
    }
}

/// `NUM_CON_TIMES` (console.c): the notify overlay shows the last 4 lines.
const NUM_CON_TIMES: usize = 4;
/// `con_notifytime` ("3"): seconds a notify line stays up.
const CON_NOTIFYTIME: f32 = 3.0;

/// The console text as the notify overlay sees it — `Con_Print` (console.c)
/// laying printed text into `con_linewidth`-wide lines, word-wrapped
/// ([`ConCursor`]), each line stamped with the time its first character
/// arrived (`con_times`), and `Con_DrawNotify` showing the last
/// `NUM_CON_TIMES` lines younger than `con_notifytime`. A line shows as soon
/// as it starts (a print need not end in `\n`), blank lines included.
///
/// In the C the notify lines are the tail of the console's own text buffer:
/// the same text also goes to the drop-down console's scrollback. The mode
/// that printed it keeps it in [`ConNotify::take_printed`] until the host
/// hands it to its drop-down [`Console`] ([`Console::print_notified`]); what
/// the host prints there comes back the other way ([`ConNotify::lay`]).
#[derive(Default)]
pub struct ConNotify {
    /// The last console lines and their `con_times` stamps.
    lines: VecDeque<(String, f32)>,
    /// `Con_Print`'s position.
    cursor: ConCursor,
    /// Text printed since the host last took it for the console scrollback.
    printed: String,
}

impl ConNotify {
    /// `Con_Print(txt)` at clock `now`: the notify lines, and kept for the
    /// console scrollback ([`ConNotify::take_printed`]).
    pub fn print(&mut self, txt: &str, now: f32) {
        self.lay(txt, now);
        self.printed.push_str(txt);
    }

    /// `Con_Print(txt)`'s notify half at clock `now`, for text the console
    /// scrollback already has (what the host printed there,
    /// [`Console::take_unnotified`]).
    pub fn lay(&mut self, txt: &str, now: f32) {
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
    }

    /// The text printed since the last call, for the console scrollback.
    pub fn take_printed(&mut self) -> String {
        std::mem::take(&mut self.printed)
    }

    /// `Con_DrawNotify`'s lines at clock `now`, top to bottom: the last
    /// `NUM_CON_TIMES` console lines, skipping any older than
    /// `con_notifytime`.
    pub fn visible(&self, now: f32) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|(_, t)| now - t <= CON_NOTIFYTIME)
            .map(|(l, _)| l.as_str())
            .collect()
    }

    /// `Con_CheckResize` for a `vid_w x vid_h` framebuffer: text is laid out
    /// `con_linewidth` wide ([`con_linewidth`]); a new width
    /// cuts the lines to it and `Con_ClearNotify`s them.
    pub fn check_resize(&mut self, vid_w: usize, vid_h: usize) {
        let width = con_linewidth(vid_w, vid_h);
        if width != self.cursor.width() {
            self.cursor.set_width(width);
            self.lines.clear();
        }
    }

    /// `Con_ClearNotify` (a level load): nothing is shown until new text. The
    /// console keeps the text (and still gets what was printed).
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// `Con_CheckResize` keeps the first `min(old, new)` columns of each line.
pub fn truncate_line(line: &mut String, width: usize) {
    if let Some((i, _)) = line.char_indices().nth(width) {
        line.truncate(i);
    }
}

/// The most characters the input line takes: keys.c's `key_lines[][MAXCMDLINE]`
/// (256) holds the `]` prompt, the typing and its terminator, and a key is
/// typed only `if (key_linepos < MAXCMDLINE-1)`.
pub const CONSOLE_INPUT_CAP: usize = 256 - 2;

/// keys.c's command history: `key_lines[32]`, a ring the input line is one
/// slot of.
const KEY_LINES: usize = 32;

/// `scr_conspeed` (screen.c, "300"): how fast the console slides, in screen
/// rows per second of `host_frametime`.
const SCR_CONSPEED: f32 = 300.0;

/// `Con_DrawInput` (console.c) stamps `10 + ((int)(realtime*con_cursorspeed) & 1)`
/// at the edit position: conchars cell 10 is blank and 11 is the block cursor.
const CONSOLE_CURSOR_BASE: u8 = 10;
/// `con_cursorspeed` (console.c: `float con_cursorspeed = 4;`): the input cursor
/// toggles 4 times per second of real time.
const CON_CURSORSPEED: f64 = 4.0;

/// The console input cursor's conchars cell this frame (`Con_DrawInput`):
/// `10 + ((int)(realtime*con_cursorspeed) & 1)` — blank, then the block, each
/// for a quarter second of REAL time.
pub fn console_cursor_glyph(realtime: f64) -> u8 {
    CONSOLE_CURSOR_BASE + realtime_blink_bit(realtime, CON_CURSORSPEED)
}

/// The Quake drop-down console: a panel slid over the top of the screen holding
/// a capped scrollback history plus a single editable input line. Toggled with
/// the `~` / backtick key; while open it owns the keyboard and executes the
/// commands typed into it.
///
/// The struct is pure state + editing/history methods; *drawing* is
/// [`draw_console`] and *command execution* lives in the host (the wasm shell),
/// which holds the live game world the commands act on.
pub struct Console {
    /// Whether the console is dropped down (drawn + capturing the keyboard).
    pub open: bool,
    /// Scrollback history, oldest first: the `con->text` rows [`Console::print`]
    /// lays text into, at most [`Console::totallines`] of them; pushing past
    /// that drops the oldest line.
    lines: std::collections::VecDeque<String>,
    /// `Con_Print`'s position in the last line.
    cursor: ConCursor,
    /// keys.c `key_lines`: the ring of command lines (the text after the `]`
    /// prompt) — the one being typed is `key_lines[edit_line]`, the others
    /// are the history. Each at most [`CONSOLE_INPUT_CAP`] characters.
    key_lines: [String; KEY_LINES],
    /// keys.c `edit_line`: the slot being typed.
    edit_line: usize,
    /// keys.c `history_line`: the slot Up/Down last brought back.
    history_line: usize,
    /// console.c `con_backscroll`: how many lines up from the bottom the text
    /// shows (PgUp/PgDn; any print puts it back to 0).
    backscroll: usize,
    /// `scr_con_current` (screen.c): how many 2-D screen rows the console
    /// covers now — it slides toward half the screen while open and back to
    /// nothing when closed ([`Console::slide`]).
    current: f32,
    /// `con_forcedup` (console.c): nothing is playing (the client is not
    /// connected), so the console covers the whole screen and shows its input
    /// line whether or not it is open. The host sets it each frame.
    pub forced_up: bool,
    /// Text [`Console::print`] laid into the scrollback that the notify lines
    /// have not had yet ([`Console::take_unnotified`]).
    unnotified: String,
}

impl Default for Console {
    fn default() -> Self {
        Console::new()
    }
}

impl Console {
    /// A fresh, closed console with empty scrollback and input.
    pub fn new() -> Console {
        Console {
            open: false,
            lines: std::collections::VecDeque::new(),
            cursor: ConCursor::default(),
            key_lines: Default::default(),
            edit_line: 0,
            history_line: 0,
            backscroll: 0,
            current: 0.0,
            forced_up: false,
            unnotified: String::new(),
        }
    }

    /// Toggle the console open/closed (Quake's `Con_ToggleConsole_f`). Opening
    /// does not clear the scrollback or input — the panel slides back over the
    /// history it had.
    pub fn toggle(&mut self) {
        // Con_ToggleConsole_f: closing it (key_dest back to the game) clears
        // any typing (`key_lines[edit_line][1] = 0`).
        if self.open {
            self.key_lines[self.edit_line].clear();
        }
        // It also zeroes con_times: nothing printed so far becomes a notify
        // line (the host clears the mode's notify lines).
        self.unnotified.clear();
        self.open = !self.open;
    }

    /// `SCR_SetUpToDrawConsole` (screen.c): `Con_CheckResize` for the
    /// screen's width, then slide the console toward its
    /// height — half the 2-D screen while it is open (`scr_conlines =
    /// vid.height/2`), none when closed — by `scr_conspeed * host_frametime`
    /// rows, stopping there. A `vid_w x vid_h` framebuffer; `frametime` 0 (a
    /// frozen frame) leaves it where it is.
    pub fn slide(&mut self, frametime: f32, vid_w: usize, vid_h: usize) {
        // Con_CheckResize: a new con_linewidth keeps each line's first columns.
        let width = con_linewidth(vid_w, vid_h);
        if width != self.cursor.width() {
            for line in self.lines.iter_mut() {
                truncate_line(line, width);
            }
            self.cursor.set_width(width);
            // The ring holds con_totallines of the new width (the newest
            // kept), and the text shows from the bottom again.
            while self.lines.len() > self.totallines() {
                self.lines.pop_front();
            }
            self.backscroll = 0;
        }
        let sc = screen_2d(vid_w, vid_h);
        if self.forced_up {
            // con_forcedup: `scr_conlines = vid.height; scr_con_current =
            // scr_conlines;` — full screen at once.
            self.current = sc.h as f32;
            return;
        }
        let conlines = if self.open { (sc.h / 2) as f32 } else { 0.0 };
        let step = SCR_CONSPEED * frametime.max(0.0);
        if conlines < self.current {
            self.current = (self.current - step).max(conlines);
        } else if conlines > self.current {
            self.current = (self.current + step).min(conlines);
        }
    }

    /// `scr_con_current`: the 2-D screen rows the console covers now.
    pub fn current(&self) -> f32 {
        self.current
    }

    /// Put the console where [`Console::slide`] would leave it at `lines`
    /// rows (tests, and a host that wants it down at once).
    pub fn set_current(&mut self, lines: f32) {
        self.current = lines.max(0.0);
    }

    /// Append one character to the input line, as `Key_Console` types a key:
    /// printable ASCII only (`key >= 32 && key <= 127`, so nothing else), and
    /// never the backtick/tilde (the toggle key's, which `Key_Event` never
    /// hands the console). A no-op once the input reaches
    /// [`CONSOLE_INPUT_CAP`] characters.
    pub fn putchar(&mut self, c: char) {
        if c == '`' || c == '~' || !(' '..='~').contains(&c) {
            return;
        }
        self.type_char(c as u8);
    }

    /// `Key_Console`'s last branch: the key goes at the end of the line while
    /// it has room (`key_linepos < MAXCMDLINE-1`).
    fn type_char(&mut self, c: u8) {
        let line = &mut self.key_lines[self.edit_line];
        if line.len() < CONSOLE_INPUT_CAP {
            line.push(c as char);
        }
    }

    /// Delete the last character of the input line (backspace). A no-op on an
    /// empty line.
    pub fn backspace(&mut self) {
        self.key_lines[self.edit_line].pop();
    }

    /// The current input line (without the prompt), for the host to inspect.
    pub fn input(&self) -> &str {
        &self.key_lines[self.edit_line]
    }

    /// `con_totallines`: the lines the text ring holds at this width,
    /// `CON_TEXTSIZE / con_linewidth`.
    pub fn totallines(&self) -> usize {
        CON_TEXTSIZE / self.cursor.width().max(1)
    }

    /// `con_backscroll`: the lines the text is scrolled up from the bottom.
    pub fn backscroll(&self) -> usize {
        self.backscroll
    }

    /// `Key_Console` (keys.c): a key down the console has the keyboard for,
    /// Shift applied (`keyshift[]`), with `text` the character it types (if
    /// any; the host passes only printable ASCII, as the C's `key >= 32 &&
    /// key <= 127`). `vid_h` is the 2-D screen's height (`vid.height`), which
    /// bounds the scrollback; `complete` is `Cmd_CompleteCommand` then
    /// `Cvar_CompleteVariable` for Tab.
    ///
    /// - Enter submits the line: echoed into the scrollback with its `]`
    ///   prompt (`Con_Printf ("%s\n", key_lines[edit_line])`, an empty line
    ///   too), kept in the 32-line history (`edit_line` moves on), and
    ///   returned for the host to execute (`Cbuf_AddText`);
    /// - Tab completes the whole line as a command or cvar name, and a space
    ///   after it (`"map "`), when one starts with it;
    /// - Backspace and Left take back the last character;
    /// - Up and Down walk the history, skipping empty lines (Down past the
    ///   newest gives an empty line back);
    /// - PgUp/PgDn (and the mouse wheel) scroll the text 2 lines,
    ///   `con_backscroll` bounded by `con_totallines - (vid.height>>3) - 1`;
    ///   Home/End go to the top/bottom (unreachable in id's routing: they are
    ///   no console keys, so `Key_Event` runs their bindings);
    /// - a printable key types `text`. Every other key does nothing.
    pub fn key(
        &mut self,
        key: u8,
        text: Option<u8>,
        vid_h: usize,
        complete: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let edit = self.edit_line;
        let max_back = (self.totallines() as i64 - (vid_h as i64 >> 3) - 1).max(0) as usize;
        match key {
            K_ENTER => {
                let line = self.key_lines[edit].clone();
                self.println(format!("]{line}"));
                self.edit_line = (edit + 1) % KEY_LINES;
                self.history_line = self.edit_line;
                self.key_lines[self.edit_line].clear();
                return Some(line);
            }
            K_TAB => {
                if let Some(cmd) = complete(&self.key_lines[edit]) {
                    let mut line = cmd;
                    line.push(' ');
                    line.truncate(CONSOLE_INPUT_CAP);
                    self.key_lines[edit] = line;
                }
            }
            K_BACKSPACE | K_LEFTARROW => {
                self.key_lines[edit].pop();
            }
            K_UPARROW => {
                loop {
                    self.history_line = (self.history_line + KEY_LINES - 1) % KEY_LINES;
                    if self.history_line == edit || !self.key_lines[self.history_line].is_empty() {
                        break;
                    }
                }
                if self.history_line == edit {
                    self.history_line = (edit + 1) % KEY_LINES;
                }
                self.key_lines[edit] = self.key_lines[self.history_line].clone();
            }
            K_DOWNARROW => {
                if self.history_line == edit {
                    return None;
                }
                loop {
                    self.history_line = (self.history_line + 1) % KEY_LINES;
                    if self.history_line == edit || !self.key_lines[self.history_line].is_empty() {
                        break;
                    }
                }
                if self.history_line == edit {
                    self.key_lines[edit].clear();
                } else {
                    self.key_lines[edit] = self.key_lines[self.history_line].clone();
                }
            }
            K_PGUP | K_MWHEELUP => self.backscroll = (self.backscroll + 2).min(max_back),
            K_PGDN | K_MWHEELDOWN => self.backscroll = self.backscroll.saturating_sub(2),
            K_HOME => self.backscroll = max_back,
            K_END => self.backscroll = 0,
            _ => {
                if let Some(c) = text {
                    self.type_char(c);
                }
            }
        }
        None
    }

    /// `Con_Print(txt)` (console.c): lay `txt` into the scrollback, word-wrapped
    /// at [`CON_LINEWIDTH`] ([`ConCursor`]). Text without a final `\n` leaves
    /// the line open, so the next print continues it (a pickup's `sprint`
    /// fragments join on one line). The oldest line drops once the history
    /// exceeds [`Console::totallines`]. The notify lines are the same text
    /// in the C (`con_times` stamps the console's own lines), so it is also
    /// kept for them ([`Console::take_unnotified`]): "Saving game to s0.sav..."
    /// after a menu save shows over the game.
    pub fn print(&mut self, txt: &str) {
        self.print_notified(txt);
        self.unnotified.push_str(txt);
    }

    /// [`Console::print`] for text the notify lines already have (what the
    /// game printed through [`ConNotify::print`]): the scrollback only.
    pub fn print_notified(&mut self, txt: &str) {
        // Con_Print: `con_backscroll = 0` — new text shows.
        self.backscroll = 0;
        let cap = self.totallines();
        let lines = &mut self.lines;
        self.cursor.print(txt, |op| match op {
            ConOp::Linefeed => {
                lines.push_back(String::new());
                while lines.len() > cap {
                    lines.pop_front();
                }
            }
            ConOp::Unlinefeed => {
                lines.pop_back();
            }
            ConOp::Char(c) => {
                if let Some(l) = lines.back_mut() {
                    l.push(c as char);
                }
            }
        });
    }

    /// `Con_Printf("%s\n", line)`: [`Console::print`] the line and a newline
    /// (so it wraps at the console width, and continues a line a print left
    /// open, as in the C).
    pub fn println(&mut self, line: impl Into<String>) {
        let mut line = line.into();
        line.push('\n');
        self.print(&line);
    }

    /// The text [`Console::print`] has laid into the scrollback since the last
    /// call, for the host to hand to the notify lines ([`ConNotify::lay`]).
    pub fn take_unnotified(&mut self) -> String {
        std::mem::take(&mut self.unnotified)
    }

    /// The current number of scrollback lines (for tests / host inspection).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Iterate the scrollback lines, oldest first (for tests / host
    /// inspection — e.g. asserting the savegame commands print the C's
    /// exact messages).
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Clear the scrollback history (Quake's `Con_Clear_f`, which blanks
    /// `con->text` and leaves `con_x`: a line left open continues at its
    /// column). Leaves the input line untouched.
    pub fn clear(&mut self) {
        self.lines.clear();
        if self.cursor.con_x != 0 {
            self.lines.push_back(" ".repeat(self.cursor.con_x));
        }
    }
}

/// `VERSION` (quakedef.h) as `Draw_ConsoleBackground` prints it, `"%4.2f"`.
const CON_VERSION: &str = "1.09";

/// `Draw_ConsoleBackground` (draw.c): the bottom `lines` rows of `conback`
/// stretched over the top `lines` rows of the 2-D screen `sc` (row `y` shows
/// conback row `(vid.height - lines + y)*200/vid.height`, columns stepped in
/// 16.16 fixed point when the screen is not 320 wide), with the version
/// number stamped into the pic first (`Draw_CharToConback`: each lit conchars
/// texel `t` becomes `0x60 + t`). The version is the DOS build's — the plain
/// `"%4.2f"` at conback (320-43, 186); WinQuake's Windows build wrote
/// "(WinQuake) 1.09" further left and id's Linux build (the oracle's)
/// "(Linux Quake 1.30) 1.09". Missing or malformed pics leave the rows black.
fn draw_console_background(
    image: &mut Image,
    sc: Screen2d,
    lines: i32,
    conback: Option<&crate::wad::Qpic>,
    conchars: Option<&crate::wad::Qpic>,
) {
    let rows = sc.px(lines).clamp(0, image.h as i64) as usize;
    let pic = conback.filter(|p| p.width == 320 && p.height == 200 && p.data.len() >= 320 * 200);
    let Some(pic) = pic else {
        fill_rect(image, 0, 0, image.w as i64, rows as i64, 0);
        return;
    };
    let mut data = pic.data[..320 * 200].to_vec();
    if let Some(cc) = conchars.filter(|c| c.width == 128 && c.height == 128 && c.data.len() >= 128 * 128) {
        let dest = 320 - 43 + 320 * 186;
        for (i, ch) in CON_VERSION.bytes().enumerate() {
            let (row, col) = ((ch >> 4) as usize, (ch & 15) as usize);
            let src = (row << 10) + (col << 3);
            for line in 0..8 {
                for x in 0..8 {
                    let t = cc.data[src + line * 128 + x];
                    if t != 0 {
                        data[dest + (i << 3) + line * 320 + x] = 0x60u8.wrapping_add(t);
                    }
                }
            }
        }
    }
    // The conback column of every 2-D screen column (memcpy at 320 wide, else
    // `fstep = 320*0x10000/vid.conwidth` stepping from 0).
    let fstep = (320i64 << 16) / sc.w.max(1) as i64;
    let src_col: Vec<usize> = (0..sc.w as i64).map(|x| (((x * fstep) >> 16) as usize).min(319)).collect();
    let inv = 1.0 / sc.scale;
    let cols: Vec<usize> =
        (0..image.w).map(|px| src_col[((px as f32 * inv) as usize).min(src_col.len() - 1)]).collect();
    let height = sc.h.max(1) as i64;
    for py in 0..rows {
        let y = ((py as f32 * inv) as i64).min(lines as i64 - 1);
        let v = (((height - lines as i64 + y) * 200 / height).clamp(0, 199)) as usize;
        let srow = &data[v * 320..v * 320 + 320];
        let row = &mut image.pixels[py * image.w..(py + 1) * image.w];
        for (out, &sx) in row.iter_mut().zip(&cols) {
            *out = srow[sx];
        }
    }
}

/// `Draw_ConsoleBackground (vid.height)`: the console background over the
/// whole 2-D screen, no text — what `M_Draw` puts under the menu while the
/// console is out (`scr_con_current`, e.g. forced up with nothing playing),
/// in place of the fade.
pub fn draw_console_background_full(
    image: &mut Image,
    conback: Option<&crate::wad::Qpic>,
    conchars: Option<&crate::wad::Qpic>,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);
    draw_console_background(image, sc, sc.h, conback, conchars);
}

/// Draw the drop-down console over `image` at its current height
/// ([`Console::slide`]), a port of `SCR_DrawConsole` -> `Con_DrawConsole`
/// (console.c) and `Con_DrawInput`. Draws nothing while the console is up
/// (`scr_con_current` 0).
///
/// On the [`screen_2d`] screen, `lines = (int)scr_con_current` rows:
///  1. `Draw_ConsoleBackground(lines)` (`draw_console_background`);
///  2. the text: `rows = (lines-16)>>3` lines ending with the current one,
///     from `y = lines - 16 - rows*8`, each `con_linewidth = (vid.width>>3) - 2`
///     characters at `x = (col+1)*8`;
///  3. while it is open (`key_dest == key_console`), the input line at
///     `lines - 16`: the `]` prompt, the typing and the cursor cell
///     [`console_cursor_glyph`] at the edit position (the end of the line —
///     this console has no cursor keys), prestepped when it passes the width.
///
/// A missing `conchars` skips the text; every write is clipped.
pub fn draw_console(
    image: &mut Image,
    console: &Console,
    conback: Option<&crate::wad::Qpic>,
    conchars: Option<&crate::wad::Qpic>,
    realtime: f64,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);
    let lines = (console.current as i32).min(sc.h);
    if lines <= 0 {
        return;
    }
    draw_console_background(image, sc, lines, conback, conchars);
    let Some(cc) = conchars else { return };
    let linewidth = ((sc.w >> 3) - 2).max(1) as usize;
    let glyph = |image: &mut Image, col: usize, y: i32, c: u8| {
        if c != b' ' {
            draw_char_scaled(image, cc, ((col + 1) * 8) as f32, y as f32, c, sc.scale, 0.0, 0.0);
        }
    };
    // The text, ending with con_current (the last line) less con_backscroll;
    // lines above the first are the ring's blank ones.
    let rows = (lines - 16) >> 3;
    let mut y = lines - 16 - (rows << 3);
    let n = console.lines.len() as i64 - console.backscroll as i64;
    for i in (n - rows.max(0) as i64)..n {
        if i >= 0 {
            // Con_Print stores bytes; a line holds them as chars 0..=255.
            for (col, c) in console.lines[i as usize].chars().take(linewidth).enumerate() {
                glyph(image, col, y, c as u32 as u8);
            }
        }
        y += 8;
    }
    // Con_DrawInput: only while typing is possible (`key_dest ==
    // key_console`, or the console forced up).
    if console.open || console.forced_up {
        let mut text: Vec<u8> = std::iter::once(b']').chain(console.input().chars().map(|c| c as u32 as u8)).collect();
        let linepos = text.len();
        text.push(console_cursor_glyph(realtime));
        let start = if linepos >= linewidth { 1 + linepos - linewidth } else { 0 };
        for (col, &c) in text[start..].iter().take(linewidth).enumerate() {
            glyph(image, col, lines - 16, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::K_INS;
    use crate::render::fixtures::solid_pic;

    /// CENSUS L11: Con_Print lays text into 38-column console lines (word
    /// wrapped, a line stamped when it starts) and Con_DrawNotify shows the
    /// last 4 younger than con_notifytime — fragments join, blank lines count.
    #[test]
    fn notify_lines_follow_con_print() {
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
    fn console_cursor_flashes_at_con_cursorspeed_on_realtime() {
        // Con_DrawInput: 10 + ((int)(realtime*con_cursorspeed) & 1), speed 4.
        // (The port used to blink cell 11 at 2 Hz off the host clock.)
        assert_eq!(console_cursor_glyph(0.0), 10, "cell 10 (blank) first");
        assert_eq!(console_cursor_glyph(0.3), 11, "the block from 0.25 s");
        assert_eq!(console_cursor_glyph(0.6), 10);
        assert_eq!(console_cursor_glyph(0.8), 11);
        let mut edges = 0;
        let mut prev = console_cursor_glyph(0.0);
        for ms in 1..=1000 {
            let g = console_cursor_glyph(ms as f64 / 1000.0);
            if g != prev {
                edges += 1;
            }
            prev = g;
        }
        assert_eq!(edges, 4, "the console cursor toggles 4 times per real second");
    }

    // -----------------------------------------------------------------------
    // Drop-down console
    // -----------------------------------------------------------------------

    /// A 128x128 conchars atlas where every glyph texel is the lit index 3,
    /// EXCEPT the byte-0 cell (top-left 8x8) which is index-0 transparent — so a
    /// drawn non-space character paints index-3 pixels and a space/byte-0 paints
    /// nothing, matching the menu tests' helper.
    fn lit_conchars() -> crate::wad::Qpic {
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        crate::wad::Qpic { width: 128, height: 128, data }
    }

    #[test]
    fn console_putchar_and_backspace_edit_the_input() {
        let mut c = Console::new();
        assert_eq!(c.input(), "");
        c.putchar('g');
        c.putchar('o');
        c.putchar('d');
        assert_eq!(c.input(), "god");
        c.backspace();
        assert_eq!(c.input(), "go");
        // The backtick/tilde toggle key and control chars never enter the buffer.
        c.putchar('`');
        c.putchar('~');
        c.putchar('\n');
        c.putchar('\t');
        assert_eq!(c.input(), "go", "toggle/control chars are not typed");
        // Backspacing an empty line is a harmless no-op.
        c.backspace();
        c.backspace();
        c.backspace();
        assert_eq!(c.input(), "");
        c.backspace();
        assert_eq!(c.input(), "");
        // Only ASCII types (Key_Console: key >= 32 && key <= 127).
        for ch in ['é', 'ß', '€', '\u{7f}'] {
            c.putchar(ch);
        }
        assert_eq!(c.input(), "", "non-ASCII is ignored");
        // The input length is capped (MAXCMDLINE 256: prompt, 254, NUL).
        for _ in 0..(CONSOLE_INPUT_CAP + 50) {
            c.putchar('x');
        }
        assert_eq!(c.input().chars().count(), 254, "input length is capped");
    }

    /// `Key_Console` on a 200-line screen with nothing to complete.
    fn key(c: &mut Console, key: u8, text: Option<u8>) -> Option<String> {
        c.key(key, text, 200, |_| None)
    }

    /// Type `s` and press Enter.
    fn submit(c: &mut Console, s: &str) {
        for ch in s.bytes() {
            key(c, ch, Some(ch));
        }
        key(c, K_ENTER, None);
    }

    #[test]
    fn key_console_enter_submits_and_echoes_the_line() {
        let mut c = Console::new();
        for ch in "give h 100x".bytes() {
            key(&mut c, ch, Some(ch));
        }
        key(&mut c, K_LEFTARROW, None); // Left arrow takes a character back too
        assert_eq!(c.input(), "give h 100");
        let got = key(&mut c, K_ENTER, None);
        assert_eq!(got.as_deref(), Some("give h 100"), "Enter returns the line");
        assert_eq!(c.input(), "", "and starts a new one");
        assert_eq!(c.lines().collect::<Vec<_>>(), ["]give h 100"], "echoed with its prompt");
        // An empty line echoes its prompt as id's does, and runs nothing.
        assert_eq!(key(&mut c, K_ENTER, None).as_deref(), Some(""));
        assert_eq!(c.lines().nth(1), Some("]"));
        // A key that types nothing is ignored.
        key(&mut c, K_TAB, None);
        key(&mut c, K_INS, None);
        assert_eq!(c.input(), "");
    }

    /// Key_Console's history: `key_lines[32]`, Up walks back over the
    /// non-empty lines (wrapping to the oldest slot when there are none
    /// older), Down forward, past the newest to an empty line; Enter keeps a
    /// line in the ring.
    #[test]
    fn up_and_down_walk_the_32_line_history() {
        let mut c = Console::new();
        key(&mut c, K_DOWNARROW, None);
        assert_eq!(c.input(), "", "Down with no history: nothing");
        for line in ["god", "", "noclip", "map e1m2"] {
            submit(&mut c, line);
        }
        for x in b"typ".iter() {
            key(&mut c, *x, Some(*x));
        }
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "map e1m2", "Up: the last line, the typing replaced");
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "noclip");
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "god", "the empty line is skipped");
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "", "no older line: the slot after edit_line (empty)");
        key(&mut c, K_DOWNARROW, None);
        assert_eq!(c.input(), "god");
        key(&mut c, K_DOWNARROW, None);
        key(&mut c, K_DOWNARROW, None);
        assert_eq!(c.input(), "map e1m2");
        key(&mut c, K_DOWNARROW, None);
        assert_eq!(c.input(), "", "past the newest: an empty line");
        key(&mut c, K_DOWNARROW, None);
        assert_eq!(c.input(), "", "and Down stays there");
        // A line brought back and submitted is the newest.
        key(&mut c, K_UPARROW, None);
        key(&mut c, K_UPARROW, None);
        assert_eq!(key(&mut c, K_ENTER, None).as_deref(), Some("noclip"));
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "noclip");
        // The ring holds 32 slots, one being typed: 31 lines back at most.
        let mut c = Console::new();
        for i in 0..40 {
            submit(&mut c, &format!("echo {i}"));
        }
        for _ in 0..31 {
            key(&mut c, K_UPARROW, None);
        }
        assert_eq!(c.input(), "echo 9", "the oldest kept");
        key(&mut c, K_UPARROW, None);
        assert_eq!(c.input(), "echo 9", "and Up stays there (the slot after edit_line)");
    }

    /// Tab: Cmd_CompleteCommand, then Cvar_CompleteVariable, on the whole
    /// line; a match replaces it with the name and a space.
    #[test]
    fn tab_completes_a_command_then_a_cvar() {
        let complete = |p: &str| -> Option<String> {
            if p.is_empty() {
                return None;
            }
            ["map", "god"].iter().chain(["viewsize"].iter()).find(|n| n.starts_with(p)).map(|n| n.to_string())
        };
        let mut c = Console::new();
        for ch in b"ma" {
            c.key(*ch, Some(*ch), 200, complete);
        }
        c.key(K_TAB, None, 200, complete);
        assert_eq!(c.input(), "map ");
        for ch in b"e1m2" {
            c.key(*ch, Some(*ch), 200, complete);
        }
        c.key(K_TAB, None, 200, complete);
        assert_eq!(c.input(), "map e1m2", "no name starts with the whole line: nothing");
        let mut c = Console::new();
        c.key(b'v', Some(b'v'), 200, complete);
        c.key(K_TAB, None, 200, complete);
        assert_eq!(c.input(), "viewsize ");
        let mut c = Console::new();
        c.key(K_TAB, None, 200, complete);
        assert_eq!(c.input(), "", "an empty line completes to nothing");
    }

    /// PgUp/PgDn: con_backscroll in steps of 2, bounded by con_totallines -
    /// (vid.height>>3) - 1; any print puts it back to 0; the text drawn is
    /// that many lines up.
    #[test]
    fn pgup_and_pgdn_scroll_the_text_back() {
        let cc = lit_conchars();
        let mut c = Console::new();
        c.toggle();
        c.set_current(100.0);
        for i in 0..60 {
            c.println(format!("line {i}"));
        }
        key(&mut c, K_PGUP, None);
        key(&mut c, K_MWHEELUP, None);
        assert_eq!(c.backscroll(), 4);
        key(&mut c, K_PGDN, None);
        assert_eq!(c.backscroll(), 2);
        // The bottom text row (y 76 on a 100-row console) now shows the line
        // two up: draw it and the unscrolled console and compare with the
        // row two above.
        let draw = |c: &Console| {
            let mut img = Image::new(320, 200, 0);
            draw_console(&mut img, c, None, Some(&cc), 0.0);
            img.pixels
        };
        let scrolled = draw(&c);
        let mut plain = Console::new();
        plain.toggle();
        plain.set_current(100.0);
        for i in 0..58 {
            plain.println(format!("line {i}"));
        }
        assert_eq!(scrolled, draw(&plain), "two lines up");
        // Bounded: 431 lines (320 wide) - 25 rows - 1.
        for _ in 0..300 {
            key(&mut c, K_PGUP, None);
        }
        assert_eq!(c.backscroll(), 431 - 25 - 1);
        c.key(K_PGUP, None, 600, |_| None);
        assert_eq!(c.backscroll(), 431 - 75 - 1, "a taller screen shows more, scrolls less");
        key(&mut c, K_PGDN, None);
        assert_eq!(c.backscroll(), 431 - 75 - 3);
        c.println("new text");
        assert_eq!(c.backscroll(), 0, "Con_Print: con_backscroll = 0");
        for _ in 0..3 {
            key(&mut c, K_PGDN, None);
        }
        assert_eq!(c.backscroll(), 0, "not below the bottom");
    }

    #[test]
    fn console_println_caps_the_scrollback() {
        let mut c = Console::new();
        // Push well past the cap; the history must never exceed it:
        // con_totallines = CON_TEXTSIZE / con_linewidth, 431 at 320 wide.
        for i in 0..1000 {
            c.println(format!("line {i}"));
        }
        assert_eq!(c.line_count(), 431, "scrollback is the ring's con_totallines");
        assert_eq!(c.lines().next(), Some("line 569"), "the newest kept");
        // Con_CheckResize: 960 wide holds 16384 / 118 = 138.
        c.slide(0.0, 960, 600);
        assert_eq!(c.line_count(), 138);
        assert_eq!(c.lines().last(), Some("line 999"));
        // A multi-line message counts as multiple lines (split on '\n').
        let mut c2 = Console::new();
        c2.println("a\nb\nc");
        assert_eq!(c2.line_count(), 3, "embedded newlines split into separate lines");
        // clear() empties the scrollback but not the input.
        for ch in "abc".chars() {
            c2.putchar(ch);
        }
        c2.clear();
        assert_eq!(c2.line_count(), 0, "clear empties the scrollback");
        assert_eq!(c2.input(), "abc", "clear leaves the input line untouched");
    }

    #[test]
    fn console_print_is_con_print() {
        // Con_Print: fragments join on one line, a word that would cross
        // con_linewidth (38) starts the next, \r rewrites the line, and a
        // Con_Printf continues a line a print left open.
        let mut c = Console::new();
        c.print("You receive ");
        c.print("25");
        c.print(" health\n");
        c.print("aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd\n");
        c.print("progress 1\rprogress 2\n");
        c.print("You got the ");
        c.println("Shotgun");
        let long = "x".repeat(CON_LINEWIDTH + 5);
        c.println(&long);
        let lines: Vec<&str> = c.lines().collect();
        assert_eq!(
            lines,
            [
                "You receive 25 health",
                "aaaaaaaaaa bbbbbbbbbb cccccccccc ",
                "dddddddddd",
                "progress 2",
                "You got the Shotgun",
                // A word longer than a line: its first characters fill the
                // line until the rest (37) fits on the next, then it wraps.
                &long[..6],
                &long[6..],
            ]
        );
        // Con_Clear_f blanks the text but keeps the column of an open line.
        c.print("half");
        c.clear();
        c.println("way");
        assert_eq!(c.lines().collect::<Vec<_>>(), ["    way"]);
    }

    #[test]
    fn console_toggle_flips_open_and_closing_clears_the_typing() {
        let mut c = Console::new();
        assert!(!c.open);
        c.toggle();
        assert!(c.open, "toggle opens");
        c.putchar('g');
        c.toggle();
        assert!(!c.open, "toggle closes");
        // Con_ToggleConsole_f: key_lines[edit_line][1] = 0.
        assert_eq!(c.input(), "", "closing clears the input line");
    }

    #[test]
    fn con_linewidth_is_the_screen_width_in_characters_less_two() {
        // Con_CheckResize: (vid.width >> 3) - 2, on the 2-D screen.
        assert_eq!(con_linewidth(320, 200), 38);
        assert_eq!(con_linewidth(960, 600), 118);
        {
            let _extra = crate::draw::Scaled2dGuard::set(true);
            assert_eq!(con_linewidth(960, 600), 38, "the extra's 320-wide screen");
        }
        // A new width cuts the old lines and lays new text at it.
        let mut c = Console::new();
        // Con_Print re-counts the word from every character: the 50-character
        // word fits until 13 in, where its 37-character rest no longer does.
        c.println("x".repeat(50));
        assert_eq!(c.lines().collect::<Vec<_>>(), ["x".repeat(13), "x".repeat(37)]);
        c.slide(0.0, 960, 600);
        c.println("x".repeat(60));
        assert_eq!(c.lines().last(), Some("x".repeat(60).as_str()), "118 columns now");
        c.slide(0.0, 160, 200);
        assert!(c.lines().all(|l| l.chars().count() <= 18), "cut to (160>>3)-2");
    }

    #[test]
    fn console_slides_at_scr_conspeed_to_half_the_screen() {
        // SCR_SetUpToDrawConsole: scr_conlines = vid.height/2 while open,
        // scr_con_current moves scr_conspeed (300) * host_frametime toward it.
        let mut c = Console::new();
        c.slide(0.1, 320, 200);
        assert_eq!(c.current(), 0.0, "closed stays up");
        c.toggle();
        c.slide(0.1, 320, 200);
        assert_eq!(c.current(), 30.0);
        c.slide(0.0, 320, 200);
        assert_eq!(c.current(), 30.0, "a frozen frame does not move it");
        for _ in 0..3 {
            c.slide(0.1, 320, 200);
        }
        assert_eq!(c.current(), 100.0, "stops at half the screen");
        c.toggle();
        c.slide(0.1, 320, 200);
        assert_eq!(c.current(), 70.0, "closing slides it back up");
        // id's own pixels in every mode: half of a 600-line screen.
        let mut big = Console::new();
        big.toggle();
        for _ in 0..20 {
            big.slide(0.1, 960, 600);
        }
        assert_eq!(big.current(), 300.0);
    }

    #[test]
    fn draw_console_is_con_drawconsole() {
        let cc = lit_conchars();
        let conback = solid_pic(320, 200, 5); // opaque index-5 background
        let bg = 9u8;
        // All the way up: draws nothing.
        let mut c = Console::new();
        let mut img = Image::new(320, 200, bg);
        let before = img.pixels.clone();
        draw_console(&mut img, &c, Some(&conback), Some(&cc), 0.0);
        assert_eq!(img.pixels, before, "a console that is up draws nothing");

        // Down 100 rows (half of 200).
        c.toggle();
        c.set_current(100.0);
        c.println("hello console");
        for ch in "god".chars() {
            c.putchar(ch);
        }
        draw_console(&mut img, &c, Some(&conback), Some(&cc), 0.0);
        assert_eq!(img.pixels[2 * 320 + 2], 5, "the conback at the top");
        assert_eq!(img.pixels[99 * 320 + 2], 5, "down to row 99");
        assert_eq!(img.pixels[100 * 320 + 2], bg, "nothing below the console");
        // rows = (100-16)>>3 = 10 text lines from y = 100-16-80 = 4: the last
        // ("hello console") at y 76, its first character at x = (0+1)*8.
        assert_eq!(img.pixels[76 * 320 + 8], 3, "text line at (8, 76)");
        assert_eq!(img.pixels[76 * 320 + 7], 5, "nothing left of column 1");
        // The input line at lines - 16 = 84: ']' at x 8, "god" after it.
        assert_eq!(img.pixels[84 * 320 + 8], 3, "input line at (8, 84)");
        // The version number is stamped into the pic at (277, 186):
        // conback row 186 shows at screen row 86 of a 100-row console.
        assert_eq!(img.pixels[86 * 320 + 277], 0x60 + 3, "Draw_CharToConback's 0x60 + texel");

        // Closed but still sliding up: the background and text, no input line.
        c.toggle();
        let mut img2 = Image::new(320, 200, bg);
        draw_console(&mut img2, &c, Some(&conback), Some(&cc), 0.0);
        assert_eq!(img2.pixels[84 * 320 + 8], 5, "no input line once closed");
        assert_eq!(img2.pixels[76 * 320 + 8], 3, "the text still shows");
    }

    #[test]
    fn draw_console_missing_pics_no_panic() {
        let cc = lit_conchars();
        let bg = 200u8;
        let mut c = Console::new();
        c.toggle();
        c.set_current(100.0);
        c.println("text");
        let mut img = Image::new(320, 200, bg);
        // Missing conback => the rows fill black (index 0), not a panic.
        draw_console(&mut img, &c, None, Some(&cc), 0.0);
        assert_eq!(img.pixels[2 * img.w + 2], 0, "missing conback still fills the panel");

        // Missing conchars => the background still draws, text is skipped, no panic.
        let conback = solid_pic(320, 200, 5);
        let mut img2 = Image::new(320, 200, bg);
        draw_console(&mut img2, &c, Some(&conback), None, 0.0);
        assert_eq!(img2.pixels[2 * img2.w + 2], 5, "background draws without conchars");

        // A tiny framebuffer must not panic either.
        let mut tiny = Image::new(1, 1, bg);
        draw_console(&mut tiny, &c, Some(&conback), Some(&cc), 0.0);
        let mut zero = Image::new(0, 0, bg);
        draw_console(&mut zero, &c, Some(&conback), Some(&cc), 0.0);
    }
}
