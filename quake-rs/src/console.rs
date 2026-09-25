//! The drop-down console and the notify lines.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/console.c` — `Con_Print`, `Con_DrawInput`, `Con_DrawConsole`,
//! `Con_DrawNotify`.

use crate::draw::{draw_char_scaled, draw_string_scaled, HUD_TRANSPARENT, HUD_VIRT_W, MENU_VIRT_H};
use crate::menu::realtime_blink_bit;
use crate::render::Image;

/// Draw the notify lines (`bprint`/`sprint`, Con_DrawNotify): stacked at the
/// top-left of the 320x200 virtual screen, scaled to the framebuffer.
pub fn draw_notify(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    lines: &[&str],
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = image.w as f32 / HUD_VIRT_W;
    let mut vy = 8.0;
    for line in lines {
        draw_string_scaled(image, conchars, 8.0, vy, line, scale, 0.0, 0.0, palette);
        vy += 8.0;
    }
}

// ---------------------------------------------------------------------------
// Drop-down console
// ---------------------------------------------------------------------------

/// The maximum number of scrollback lines the console retains; older lines drop
/// off the top once this is exceeded (Quake's `con_text` is a fixed ring — this
/// is the same bounded-history idea with an owned [`VecDeque`]).
pub const CONSOLE_SCROLLBACK_CAP: usize = 200;

/// The maximum length of the console input line (characters). Quake's
/// `key_lines` buffer is `MAXCMDLINE = 256`; we cap a little lower and never let
/// a runaway paste/hold grow the `String` without bound.
pub const CONSOLE_INPUT_CAP: usize = 256;

/// The fraction of the framebuffer height the console panel covers when open.
/// Quake slides the console down (`scr_con_current`); a fixed top 60% is a
/// faithful-enough stand-in for the fully-dropped console and keeps the draw
/// allocation-light and deterministic (no per-frame slide state to advance).
const CONSOLE_HEIGHT_FRAC: f32 = 0.6;

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
    /// Scrollback history, oldest first. Capped at [`CONSOLE_SCROLLBACK_CAP`];
    /// pushing past the cap drops the oldest line.
    lines: std::collections::VecDeque<String>,
    /// The current input line (the text after the `]` prompt), without the
    /// prompt or the cursor. Capped at [`CONSOLE_INPUT_CAP`] characters.
    input: String,
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
            input: String::new(),
        }
    }

    /// Toggle the console open/closed (Quake's `Con_ToggleConsole_f`). Opening
    /// does not clear the scrollback or input — the panel slides back over the
    /// history it had.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Append one printable character to the input line, ignoring control
    /// characters and the backtick/tilde (which toggle the console, never type).
    /// A no-op once the input reaches [`CONSOLE_INPUT_CAP`] characters.
    pub fn putchar(&mut self, c: char) {
        // Only printable ASCII (and any other non-control char) is accepted; the
        // backtick and tilde are the toggle key and must never enter the buffer.
        if c == '`' || c == '~' || c.is_control() {
            return;
        }
        if self.input.chars().count() >= CONSOLE_INPUT_CAP {
            return;
        }
        self.input.push(c);
    }

    /// Delete the last character of the input line (backspace). A no-op on an
    /// empty line.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// The current input line (without the prompt), for the host to inspect.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Take the entered command line: echo `"]" + line` into the scrollback,
    /// clear the input, and return the line for the host to execute. Returns
    /// `None` (drawing nothing into the scrollback) when the input is blank, so
    /// pressing Enter on an empty line is a harmless no-op.
    pub fn take_input(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.input);
        if line.trim().is_empty() {
            return None;
        }
        // Echo the command into the scrollback with the `]` prompt, exactly as
        // Quake's `Con_Printf` shows the line the player just submitted.
        self.println(format!("]{line}"));
        Some(line)
    }

    /// Push one line into the scrollback, dropping the oldest line once the
    /// history exceeds [`CONSOLE_SCROLLBACK_CAP`]. Embedded newlines are split so
    /// a multi-line message counts as multiple capped lines.
    pub fn println(&mut self, line: impl Into<String>) {
        let line = line.into();
        for part in line.split('\n') {
            self.lines.push_back(part.to_string());
            while self.lines.len() > CONSOLE_SCROLLBACK_CAP {
                self.lines.pop_front();
            }
        }
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

    /// Clear the scrollback history (Quake's `Con_Clear_f`). Leaves the input
    /// line untouched.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Draw the drop-down console over `image`, a port of `Con_DrawConsole` /
/// `Con_DrawInput`. When the console is closed this is a no-op (draws nothing).
///
/// When open it paints, in order:
///  1. the `conback` background ([`crate::wad::Qpic`], a 320x200 console picture)
///     stretched across the **top [`CONSOLE_HEIGHT_FRAC`]** of the frame. A
///     missing `conback` falls back to a dark fill rectangle so the panel is
///     always visible.
///  2. the last few scrollback lines, drawn bottom-up just above the input line,
///     via [`draw_string`] in the conchars font.
///  3. the input line as `"]" + input` plus the flashing cursor glyph
///     ([`console_cursor_glyph`]: cells 10/11 toggling at 4 Hz on `realtime`,
///     the C's unclamped wall clock).
///
/// Text is drawn at the same conchars scale the menu uses
/// (`scale = framebuffer_height / 200`, the 320x200 virtual canvas), so the font
/// is legible at any framebuffer size. A missing `conchars` skips all text (the
/// background still draws). Every blit is bounds-clipped; nothing panics on a
/// short/empty pic or a tiny framebuffer.
pub fn draw_console(
    image: &mut Image,
    console: &Console,
    conback: Option<&crate::wad::Qpic>,
    conchars: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
    realtime: f64,
) {
    if !console.open || image.w == 0 || image.h == 0 {
        return;
    }

    // The panel covers the top CONSOLE_HEIGHT_FRAC of the framebuffer.
    let panel_h = ((image.h as f32 * CONSOLE_HEIGHT_FRAC).round() as usize)
        .clamp(1, image.h);

    // 1. Background. The conback is a 320x200 QPIC; stretch its FULL extent into
    //    the panel rectangle (its own aspect is ignored — Quake also stretches
    //    conback to the console width). A missing/short conback => a dark fill.
    let drew_back = match conback {
        Some(pic) if pic.width > 0 && pic.height > 0 => {
            let pw = pic.width as usize;
            let ph = pic.height as usize;
            if pic.data.len() < pw.saturating_mul(ph) {
                false
            } else {
                for py in 0..panel_h {
                    // Map this panel row back to a source texel row (nearest).
                    let sy = (py * ph) / panel_h.max(1);
                    let sy = sy.min(ph - 1);
                    for px in 0..image.w {
                        let sx = (px * pw) / image.w.max(1);
                        let sx = sx.min(pw - 1);
                        let texel = match pic.data.get(sy * pw + sx) {
                            Some(&t) => t,
                            None => continue,
                        };
                        // conback is fully opaque; index 255 stays transparent
                        // to be safe (matches the other blits).
                        if texel == HUD_TRANSPARENT {
                            continue;
                        }
                        image.put(px as i32, py as i32, palette[texel as usize]);
                    }
                }
                true
            }
        }
        _ => false,
    };
    if !drew_back {
        // Dark fill fallback so the panel is always visible without a conback.
        let fill = [10u8, 10, 14];
        for py in 0..panel_h {
            for px in 0..image.w {
                image.put(px as i32, py as i32, fill);
            }
        }
    }

    // 2 + 3. Text. Without conchars there is nothing to draw the font with.
    let Some(cc) = conchars else { return };

    // Scale the conchars to the framebuffer the same way the menu does: the
    // 320x200 virtual canvas mapped by the HEIGHT, so an 8px glyph stays 8 real
    // px at 320x200 and scales up with a larger frame.
    let scale = (image.h as f32 / MENU_VIRT_H).max(1.0);
    let glyph = 8.0 * scale; // one conchars cell, in framebuffer pixels
    let line_step = glyph; // one text row, in framebuffer pixels

    // The input line sits at the BOTTOM of the panel, with a small margin so the
    // descender isn't clipped by the panel edge.
    let margin_x = (8.0 * scale).round();
    let input_y = (panel_h as f32 - line_step - 2.0 * scale).max(0.0);

    // 3. The input line: "]" + input + the flashing cursor. Drawn directly in
    //    framebuffer pixels (scale folded into the position + the glyph block).
    let prompt = format!("]{}", console.input());
    draw_string_scaled(image, cc, 0.0, 0.0, &prompt, scale, margin_x, input_y, palette);
    // Con_DrawInput: text[key_linepos] = 10 + ((int)(realtime*con_cursorspeed)&1)
    // — the cursor cell sits at the edit position (the end of the line: this
    // console has no cursor keys) and alternates blank/block at 4 Hz.
    let cursor_col = prompt.chars().count() as f32; // 8 virtual px per char
    let glyph = console_cursor_glyph(realtime);
    draw_char_scaled(image, cc, cursor_col * 8.0, 0.0, glyph, scale, margin_x, input_y, palette);

    // 2. Scrollback: the lines just above the input, drawn bottom-up. How many
    //    rows fit between the top margin and the input line.
    let top_margin = (2.0 * scale).round();
    let avail = (input_y - top_margin).max(0.0);
    let rows = (avail / line_step).floor() as usize;
    if rows == 0 {
        return;
    }
    // Take the last `rows` scrollback lines and stack them so the newest sits
    // directly above the input line.
    let total = console.lines.len();
    let start = total.saturating_sub(rows);
    for (i, line) in console.lines.iter().skip(start).enumerate() {
        // i = 0 is the OLDEST of the shown rows (highest up); the newest sits
        // just above the input line.
        let shown = total - start; // number of lines we'll actually draw
        let row_from_bottom = (shown - 1 - i) as f32; // 0 = closest to input
        let y = input_y - line_step * (row_from_bottom + 1.0);
        if y < top_margin - line_step {
            continue;
        }
        draw_string_scaled(image, cc, 0.0, 0.0, line, scale, margin_x, y, palette);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{ramp_palette, solid_pic};

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
        // The input length is capped.
        for _ in 0..(CONSOLE_INPUT_CAP + 50) {
            c.putchar('x');
        }
        assert_eq!(c.input().chars().count(), CONSOLE_INPUT_CAP, "input length is capped");
    }

    #[test]
    fn console_take_input_returns_and_clears_and_echoes() {
        let mut c = Console::new();
        for ch in "give h 100".chars() {
            c.putchar(ch);
        }
        let before = c.line_count();
        let got = c.take_input();
        assert_eq!(got.as_deref(), Some("give h 100"), "take_input returns the line");
        assert_eq!(c.input(), "", "take_input clears the input line");
        assert_eq!(c.line_count(), before + 1, "the submitted line is echoed to scrollback");
        // A blank line is a no-op: nothing returned, nothing echoed.
        let n = c.line_count();
        assert_eq!(c.take_input(), None, "blank input returns None");
        assert_eq!(c.line_count(), n, "blank input echoes nothing");
        for ch in "   ".chars() {
            c.putchar(ch);
        }
        assert_eq!(c.take_input(), None, "whitespace-only input returns None");
        assert_eq!(c.line_count(), n);
    }

    #[test]
    fn console_println_caps_the_scrollback() {
        let mut c = Console::new();
        // Push well past the cap; the history must never exceed it.
        for i in 0..(CONSOLE_SCROLLBACK_CAP * 2) {
            c.println(format!("line {i}"));
        }
        assert_eq!(
            c.line_count(),
            CONSOLE_SCROLLBACK_CAP,
            "scrollback is capped at CONSOLE_SCROLLBACK_CAP"
        );
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
    fn console_toggle_flips_open() {
        let mut c = Console::new();
        assert!(!c.open);
        c.toggle();
        assert!(c.open, "toggle opens");
        c.toggle();
        assert!(!c.open, "toggle closes");
    }

    #[test]
    fn draw_console_closed_is_a_noop_open_draws() {
        let pal = ramp_palette();
        let cc = lit_conchars();
        let conback = solid_pic(320, 200, 5); // opaque index-5 background

        let bg = [9u8, 9, 9];
        // Closed: draws nothing.
        let mut c = Console::new();
        let mut img = Image::new(320, 200, bg);
        let before = img.rgb.clone();
        draw_console(&mut img, &c, Some(&conback), Some(&cc), &pal, 0.0);
        assert_eq!(img.rgb, before, "a closed console draws nothing");

        // Open: the panel background paints index-5 across the TOP region.
        c.toggle();
        c.println("hello console");
        for ch in "god".chars() {
            c.putchar(ch);
        }
        draw_console(&mut img, &c, Some(&conback), Some(&cc), &pal, 0.0);
        assert_ne!(img.rgb, before, "an open console draws pixels");
        // A pixel in the top-left of the panel must be the conback colour (5).
        assert_eq!(img.rgb[2 * img.w + 2], pal[5], "the conback background paints at the top");
        // A pixel BELOW the panel (bottom of the frame) is untouched.
        let bottom = (img.h - 1) * img.w + 2;
        assert_eq!(img.rgb[bottom], bg, "below the panel is untouched");
    }

    #[test]
    fn draw_console_missing_conback_fills_dark_no_panic() {
        let pal = ramp_palette();
        let cc = lit_conchars();
        let bg = [200u8, 200, 200];
        let mut c = Console::new();
        c.toggle();
        c.println("text");
        let mut img = Image::new(320, 200, bg);
        // Missing conback => a dark fill rectangle, not a panic, not the bg.
        draw_console(&mut img, &c, None, Some(&cc), &pal, 0.0);
        assert_ne!(img.rgb[2 * img.w + 2], bg, "missing conback still fills the panel");

        // Missing conchars => the background still draws, text is skipped, no panic.
        let conback = solid_pic(320, 200, 5);
        let mut img2 = Image::new(320, 200, bg);
        draw_console(&mut img2, &c, Some(&conback), None, &pal, 0.0);
        assert_eq!(img2.rgb[2 * img2.w + 2], pal[5], "background draws without conchars");

        // A tiny framebuffer must not panic either.
        let mut tiny = Image::new(1, 1, bg);
        draw_console(&mut tiny, &c, Some(&conback), Some(&cc), &pal, 0.0);
        let mut zero = Image::new(0, 0, bg);
        draw_console(&mut zero, &c, Some(&conback), Some(&cc), &pal, 0.0);
    }
}
