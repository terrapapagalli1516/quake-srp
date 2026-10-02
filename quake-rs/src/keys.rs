//! Key numbers, their names, and the key bindings.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/keys.h` / `keys.c` (`K_*`, `Key_KeynumToString`,
//! `Key_StringToKeynum`, `keybindings[]` with `Key_SetBinding` and
//! `Key_WriteBindings`) and the binds of `default.cfg`.
//!
//! id's `keybindings[256]` holds a command line per key. The port keeps a
//! [`Binding`] per key: one of [`BIND_COMMANDS`], the commands the host runs
//! itself (the moves `CL_BaseMove` reads every frame, the impulses, the view
//! size, pause, the console), or any other console line, run through the
//! console when the key goes down, as `Key_Event` puts it in the command
//! buffer.

/// Quake key numbers (keys.h): printable ASCII is itself; the special keys take
/// the 128+ block. Only the keys a browser page can sensibly deliver are named
/// here; the bindings table spans the full `0..256` like the C `keybindings`.
pub const K_TAB: u8 = 9;
pub const K_ENTER: u8 = 13;
pub const K_ESCAPE: u8 = 27;
pub const K_SPACE: u8 = 32;
pub const K_BACKSPACE: u8 = 127;
pub const K_UPARROW: u8 = 128;
pub const K_DOWNARROW: u8 = 129;
pub const K_LEFTARROW: u8 = 130;
pub const K_RIGHTARROW: u8 = 131;
pub const K_ALT: u8 = 132;
pub const K_CTRL: u8 = 133;
pub const K_SHIFT: u8 = 134;
pub const K_F1: u8 = 135;
pub const K_F12: u8 = 146;
pub const K_INS: u8 = 147;
pub const K_DEL: u8 = 148;
pub const K_PGDN: u8 = 149;
pub const K_PGUP: u8 = 150;
pub const K_HOME: u8 = 151;
pub const K_END: u8 = 152;
pub const K_MOUSE1: u8 = 200;
pub const K_MOUSE2: u8 = 201;
pub const K_MOUSE3: u8 = 202;
/// The joystick's first four buttons (keys.h `K_JOY1`..`K_JOY4`):
/// in_win.c's `IN_Commands` keys button `i` < 4 as `K_JOY1 + i`.
pub const K_JOY1: u8 = 203;
pub const K_JOY4: u8 = 206;
/// The "auxiliary" keys (keys.h `K_AUX1`..`K_AUX32`): `IN_Commands` keys
/// button `i` >= 4 as `K_AUX1 + i` (so AUX1..AUX4 are never sent: the fifth
/// button is AUX5), and the POV hat's four directions as `K_AUX29`..`K_AUX32`
/// (forward, right, back, left).
pub const K_AUX1: u8 = 207;
pub const K_AUX29: u8 = 235;
pub const K_AUX32: u8 = 238;
pub const K_MWHEELUP: u8 = 239;
pub const K_MWHEELDOWN: u8 = 240;
pub const K_PAUSE: u8 = 255;

/// keys.c's `consolekeys[]` (`Key_Init`): the keys the console keeps for
/// itself while it has the keyboard — 32..127 but the toggle keys `` ` `` and
/// `~`, Enter, Tab, the arrows, Backspace (127), PgUp/PgDn, Shift and the
/// mouse wheel. Every other key goes to its binding even with the console
/// down (so Home/End reach `centerview`, not `Key_Console`'s scrolling).
pub fn consolekey(k: u8) -> bool {
    matches!(
        k,
        32..=127
            | K_ENTER
            | K_TAB
            | K_LEFTARROW
            | K_RIGHTARROW
            | K_UPARROW
            | K_DOWNARROW
            | K_PGUP
            | K_PGDN
            | K_SHIFT
            | K_MWHEELUP
            | K_MWHEELDOWN
    ) && k != b'`'
        && k != b'~'
}

/// keys.c's `menubound[]`: Escape and F1..F12, the only keys whose bindings
/// run while the menu has the keyboard (Escape is handled before the
/// bindings anyway, so no key can take the menu away).
pub fn menubound(k: u8) -> bool {
    k == K_ESCAPE || (K_F1..=K_F12).contains(&k)
}

/// keys.c's `keyshift[]`: what a key types with Shift held — the US layout
/// id's `Key_Init` spells out (letters upper-case, the digit row's symbols,
/// the punctuation pairs); every other key is itself.
pub fn keyshift(k: u8) -> u8 {
    match k {
        b'a'..=b'z' => k - b'a' + b'A',
        b'1' => b'!',
        b'2' => b'@',
        b'3' => b'#',
        b'4' => b'$',
        b'5' => b'%',
        b'6' => b'^',
        b'7' => b'&',
        b'8' => b'*',
        b'9' => b'(',
        b'0' => b')',
        b'-' => b'_',
        b'=' => b'+',
        b',' => b'<',
        b'.' => b'>',
        b'/' => b'?',
        b';' => b':',
        b'\'' => b'"',
        b'[' => b'{',
        b']' => b'}',
        b'`' => b'~',
        b'\\' => b'|',
        _ => k,
    }
}

/// `Key_KeynumToString` (keys.c): printable ASCII (33..=126) is the character
/// itself (lowercase, as `Key_Event` delivers it); the named specials come from
/// the `keynames` table; anything else is the C's `<UNKNOWN KEYNUM>` (shortened
/// to fit the 320-wide menu column).
pub fn keynum_to_string(keynum: u8) -> String {
    if keynum > 32 && keynum < 127 {
        return (keynum as char).to_string();
    }
    match keynum {
        K_TAB => "TAB",
        K_ENTER => "ENTER",
        K_ESCAPE => "ESCAPE",
        K_SPACE => "SPACE",
        K_BACKSPACE => "BACKSPACE",
        K_UPARROW => "UPARROW",
        K_DOWNARROW => "DOWNARROW",
        K_LEFTARROW => "LEFTARROW",
        K_RIGHTARROW => "RIGHTARROW",
        K_ALT => "ALT",
        K_CTRL => "CTRL",
        K_SHIFT => "SHIFT",
        K_INS => "INS",
        K_DEL => "DEL",
        K_PGDN => "PGDN",
        K_PGUP => "PGUP",
        K_HOME => "HOME",
        K_END => "END",
        K_MOUSE1 => "MOUSE1",
        K_MOUSE2 => "MOUSE2",
        K_MOUSE3 => "MOUSE3",
        K_MWHEELUP => "MWHEELUP",
        K_MWHEELDOWN => "MWHEELDOWN",
        K_PAUSE => "PAUSE",
        f @ K_F1..=K_F12 => return format!("F{}", f - K_F1 + 1),
        j @ K_JOY1..=K_JOY4 => return format!("JOY{}", j - K_JOY1 + 1),
        a @ K_AUX1..=K_AUX32 => return format!("AUX{}", a - K_AUX1 + 1),
        _ => "UNKNOWN",
    }
    .to_string()
}

/// `Key_StringToKeynum` (keys.c): a single character is itself (lower case,
/// as `Key_Event` delivers letters, so `bind W` binds the key that types
/// `w`); anything longer is one of the `keynames`, in any case — `SEMICOLON`
/// for `;`, which would end a command line. `None`: not a key.
pub fn string_to_keynum(s: &str) -> Option<u8> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (None, _) => None,
        (Some(c), None) => u8::try_from(c).ok().map(|k| k.to_ascii_lowercase()),
        _ if s.eq_ignore_ascii_case("SEMICOLON") => Some(b';'),
        _ => (0..=255u8).find(|&k| !(33..127).contains(&k) && keynum_to_string(k).eq_ignore_ascii_case(s)),
    }
}

// ---------------------------------------------------------------------------
// What a key runs
// ---------------------------------------------------------------------------

/// The commands this port's bindings run themselves, by index: first the
/// rows of menu.c's `bindnames` (what Customize controls lists, in its
/// order), then the rest of what `default.cfg` binds that the port runs. A
/// binding to anything else is a console line ([`Binding::Line`]).
pub const BIND_COMMANDS: [&str; NUM_BIND_COMMANDS] = [
    "+attack",
    "impulse 10",
    "+jump",
    "+forward",
    "+back",
    "+left",
    "+right",
    "+speed",
    "+moveleft",
    "+moveright",
    "+strafe",
    "+lookup",
    "+lookdown",
    "centerview",
    "+mlook",
    "+klook",
    "+moveup",
    "+movedown",
    "sizeup",
    "sizedown",
    "+showscores",
    "impulse 0",
    "impulse 1",
    "impulse 2",
    "impulse 3",
    "impulse 4",
    "impulse 5",
    "impulse 6",
    "impulse 7",
    "impulse 8",
    "pause",
    "toggleconsole",
];
/// The number of [`BIND_COMMANDS`].
pub const NUM_BIND_COMMANDS: usize = 32;

/// Indices into [`BIND_COMMANDS`]: the `bindnames` rows (menu.c) first.
pub const BIND_ATTACK: usize = 0;
pub const BIND_CHANGEWEAPON: usize = 1;
pub const BIND_JUMP: usize = 2;
pub const BIND_FORWARD: usize = 3;
pub const BIND_BACK: usize = 4;
pub const BIND_LEFT: usize = 5;
pub const BIND_RIGHT: usize = 6;
pub const BIND_SPEED: usize = 7;
pub const BIND_MOVELEFT: usize = 8;
pub const BIND_MOVERIGHT: usize = 9;
pub const BIND_STRAFE: usize = 10;
pub const BIND_LOOKUP: usize = 11;
pub const BIND_LOOKDOWN: usize = 12;
pub const BIND_CENTERVIEW: usize = 13;
/// `+mlook`: mouse look while held (`in_mlook`, cl_input.c); the 2026
/// profile's `freelook` holds it for good while the pointer is locked.
pub const BIND_MLOOK: usize = 14;
/// `+klook`: listed and bindable; keyboard look is not modelled, so holding
/// it does nothing.
pub const BIND_KLOOK: usize = 15;
pub const BIND_MOVEUP: usize = 16;
pub const BIND_MOVEDOWN: usize = 17;
/// What `default.cfg` binds that `M_Keys_Draw` doesn't list: past the
/// `bindnames` rows, so Customize controls never shows them, but a rebind
/// over their key or `Reset to defaults` treats them like any other.
/// `bind + "sizeup"`, `bind = "sizeup"`, `bind - "sizedown"`.
pub const BIND_SIZEUP: usize = 18;
pub const BIND_SIZEDOWN: usize = 19;
/// `bind TAB "+showscores"`: `sb_showscores` while held, so `Sbar_Draw`
/// shows the scorebar and `Sbar_SoloScoreboard`.
pub const BIND_SHOWSCORES: usize = 20;
/// `bind 0 "impulse 0"` .. `bind 8 "impulse 8"`: `"impulse N"` is
/// `BIND_IMPULSE_0 + N` for N in 0..=8 (`"impulse 10"` is the listed
/// [`BIND_CHANGEWEAPON`] row).
pub const BIND_IMPULSE_0: usize = 21;
/// `bind PAUSE "pause"`: `Host_Pause_f`.
pub const BIND_PAUSE: usize = 30;
/// `` bind ` "toggleconsole" `` and `bind ~ "toggleconsole"`:
/// `Con_ToggleConsole_f`. A binding like any other, so the console key
/// opens the console only where `Key_Event` runs bindings — not over the
/// menu.
pub const BIND_TOGGLECONSOLE: usize = 31;

/// A key's binding: `keybindings[key]`, a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// One of [`BIND_COMMANDS`], by index: the host runs it itself.
    Command(usize),
    /// Any other command line, run through the console on a key down
    /// (`Key_Event`'s `Cbuf_AddText (kb)`).
    Line(Box<str>),
}

impl Binding {
    /// The binding for command line `line` (`Key_SetBinding`): one of
    /// [`BIND_COMMANDS`] when it is one (case aside, as `Cmd_ExecuteString`
    /// compares names), else the line itself. An empty line is no binding.
    pub fn parse(line: &str) -> Option<Binding> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        Some(match BIND_COMMANDS.iter().position(|c| c.eq_ignore_ascii_case(line)) {
            Some(i) => Binding::Command(i),
            None => Binding::Line(line.into()),
        })
    }

    /// The command line, as `bind` prints it and `config.cfg` keeps it.
    pub fn text(&self) -> &str {
        match self {
            Binding::Command(i) => BIND_COMMANDS.get(*i).copied().unwrap_or(""),
            Binding::Line(l) => l,
        }
    }
}

/// keys.c's `keybindings[256]`: what each key runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    keys: Vec<Option<Binding>>,
}

impl Default for Bindings {
    /// No key bound (`unbindall`).
    fn default() -> Self {
        Bindings { keys: vec![None; 256] }
    }
}

impl Bindings {
    /// id's `default.cfg`: `unbindall`, then its `bind` lines for every key
    /// the page can deliver whose command the port runs — including the
    /// function-key shortcuts (F1 `help`, F2 `menu_save`, F3 `menu_load`, F4
    /// `menu_options`, F6/F9 quicksave/quickload, F10 `quit`, F12
    /// `screenshot`; F5/F7/F8/F11 are unbound, as in id's own file). (Not
    /// ported: `messagemode` on `t`, the `zoom_in` alias; Escape's
    /// `togglemenu` is `Key_Event`'s own.)
    pub fn default_cfg() -> Bindings {
        let mut b = Bindings::default();
        for (key, cmd) in [
            (K_ALT, BIND_STRAFE),
            (b',', BIND_MOVELEFT),
            (b'.', BIND_MOVERIGHT),
            (K_DEL, BIND_LOOKDOWN),
            (K_PGDN, BIND_LOOKUP),
            (K_END, BIND_CENTERVIEW),
            (b'z', BIND_LOOKDOWN),
            (b'a', BIND_LOOKUP),
            (b'd', BIND_MOVEUP),
            (b'c', BIND_MOVEDOWN),
            (K_SHIFT, BIND_SPEED),
            (K_CTRL, BIND_ATTACK),
            (K_UPARROW, BIND_FORWARD),
            (K_DOWNARROW, BIND_BACK),
            (K_LEFTARROW, BIND_LEFT),
            (K_RIGHTARROW, BIND_RIGHT),
            (K_SPACE, BIND_JUMP),
            (K_ENTER, BIND_JUMP),
            (K_TAB, BIND_SHOWSCORES),
            (b'/', BIND_CHANGEWEAPON),
            (b'\\', BIND_MLOOK),
            (K_PAUSE, BIND_PAUSE),
            (b'~', BIND_TOGGLECONSOLE),
            (b'`', BIND_TOGGLECONSOLE),
            (b'+', BIND_SIZEUP),
            (b'=', BIND_SIZEUP),
            (b'-', BIND_SIZEDOWN),
            (K_INS, BIND_KLOOK),
            (K_MOUSE1, BIND_ATTACK),
            (K_MOUSE2, BIND_FORWARD),
            (K_MOUSE3, BIND_MLOOK),
        ] {
            b.bind(key, cmd);
        }
        // bind 1 "impulse 1" .. bind 8 "impulse 8", bind 0 "impulse 0" — by key
        // NUMBER, so the digit row selects weapons whatever Shift or the layout.
        for n in 0..=8u8 {
            b.bind(b'0' + n, BIND_IMPULSE_0 + n as usize);
        }
        // The function-key shortcuts: none of these is one of BIND_COMMANDS
        // (the host's per-frame moves/impulses/pause/console), so each is a
        // console Line, like the gamepad's "impulse 12" below — run once
        // through the console when the key goes down. F6/F9's `wait` holds
        // the `save`/`load` to the NEXT host frame, after the `echo` has had
        // one frame on screen (AUDIT.md's "A stuffed bf runs in the same
        // host frame" note is the same simplification, the other way).
        for (key, line) in [
            (K_F1, "help"),
            (K_F1 + 1, "menu_save"),
            (K_F1 + 2, "menu_load"),
            (K_F1 + 3, "menu_options"),
            (K_F1 + 5, "echo Quicksaving...; wait; save quick"),
            (K_F1 + 8, "echo Quickloading...; wait; load quick"),
            (K_F1 + 9, "quit"),
            (K_F1 + 11, "screenshot"),
        ] {
            b.set(key, Binding::parse(line));
        }
        b
    }

    /// The WASD layout over these bindings: `w`/`s` forward and back, `a`/`d`
    /// step left and right — in place of `default.cfg`'s `a` `+lookup` and
    /// `d` `+moveup`, which mouse look and Space make unneeded. The 2026
    /// profile's keys.
    pub fn with_wasd(mut self) -> Bindings {
        self.bind(b'w', BIND_FORWARD);
        self.bind(b's', BIND_BACK);
        self.bind(b'a', BIND_MOVELEFT);
        self.bind(b'd', BIND_MOVERIGHT);
        self
    }

    /// The 2026 profile's gamepad layout, as `bind` lines over id's joystick
    /// keys (a standard pad's buttons are `IN_Commands`' keys:
    /// [`crate::client::in_win`] has the mapping). The sticks are axes, not
    /// keys (`joyadvanced` and its axis maps: left moves, right looks):
    ///
    /// | pad | key | binding |
    /// |---|---|---|
    /// | A, left trigger | JOY1, AUX7 | `+jump` |
    /// | B | JOY2 | `+movedown` (swim down) |
    /// | Y, right bumper, D-pad right | JOY4, AUX6, AUX30 | `impulse 10` (next weapon) |
    /// | left bumper, D-pad left | AUX5, AUX32 | `impulse 12` (previous weapon) |
    /// | right trigger | AUX8 | `+attack` |
    /// | Back | AUX9 | `+showscores` |
    /// | Start | AUX10 | `togglemenu` |
    /// | left stick click | AUX11 | `+speed` |
    /// | D-pad up / down | AUX29 / AUX31 | `+moveup` / `+movedown` |
    ///
    /// X, the right stick's click and the Guide button are left free. In the
    /// menus the pad's A, B, Start and D-pad are Enter, Escape and the arrows
    /// (`joy_menukeys`, the platform's), so these bindings are the game's.
    pub fn with_gamepad(mut self) -> Bindings {
        let (a, b, y) = (K_JOY1, K_JOY1 + 1, K_JOY1 + 3);
        let aux = |n: u8| K_AUX1 + n - 1;
        for (key, cmd) in [
            (a, BIND_JUMP),
            (aux(7), BIND_JUMP),
            (b, BIND_MOVEDOWN),
            (y, BIND_CHANGEWEAPON),
            (aux(6), BIND_CHANGEWEAPON),
            (aux(30), BIND_CHANGEWEAPON),
            (aux(8), BIND_ATTACK),
            (aux(9), BIND_SHOWSCORES),
            (aux(11), BIND_SPEED),
            (aux(29), BIND_MOVEUP),
            (aux(31), BIND_MOVEDOWN),
        ] {
            self.bind(key, cmd);
        }
        for (key, line) in [(aux(5), "impulse 12"), (aux(32), "impulse 12"), (aux(10), "togglemenu")] {
            self.set(key, Binding::parse(line));
        }
        self
    }

    /// `keybindings[key]`.
    pub fn get(&self, key: u8) -> Option<&Binding> {
        self.keys[key as usize].as_ref()
    }

    /// The [`BIND_COMMANDS`] index `key` runs, if it runs one of them.
    pub fn command(&self, key: u8) -> Option<usize> {
        match self.get(key) {
            Some(Binding::Command(i)) => Some(*i),
            _ => None,
        }
    }

    /// `Key_SetBinding`: from now on `key` runs `binding` (`None`: nothing).
    pub fn set(&mut self, key: u8, binding: Option<Binding>) {
        self.keys[key as usize] = binding;
    }

    /// Bind `key` to command `cmd` of [`BIND_COMMANDS`].
    pub fn bind(&mut self, key: u8, cmd: usize) {
        self.set(key, Some(Binding::Command(cmd)));
    }

    /// `Key_Unbindall_f`.
    pub fn unbind_all(&mut self) {
        self.keys.fill(None);
    }

    /// Every bound key and its binding, by keynum.
    pub fn iter(&self) -> impl Iterator<Item = (u8, &Binding)> {
        self.keys.iter().enumerate().filter_map(|(k, b)| b.as_ref().map(|b| (k as u8, b)))
    }

    /// `M_FindKeysForCommand` (menu.c): the first two keys bound to `cmd`,
    /// in keynum order (the C scans 0..256 ascending).
    pub fn find_keys_for_command(&self, cmd: usize) -> [Option<u8>; 2] {
        let mut out = [None; 2];
        let keys = (0..=255u8).filter(|&k| self.command(k) == Some(cmd));
        for (slot, k) in out.iter_mut().zip(keys) {
            *slot = Some(k);
        }
        out
    }

    /// `M_UnbindCommand` (menu.c): clear every key bound to `cmd`.
    pub fn unbind_command(&mut self, cmd: usize) {
        for b in &mut self.keys {
            if *b == Some(Binding::Command(cmd)) {
                *b = None;
            }
        }
    }

    /// `CL_KeyState`'s "down" for command `cmd`: a held key runs it.
    pub fn held(&self, cmd: usize, held: &[bool; 256]) -> bool {
        held.iter().enumerate().any(|(k, &h)| h && self.command(k as u8) == Some(cmd))
    }

    /// `Key_WriteBindings`, as the lines that turn `base` into these
    /// bindings: `bind "KEY" "command"` for each key bound otherwise than in
    /// `base`, and `unbind "KEY"` for each key `base` binds and these do not.
    pub fn write_changes(&self, base: &Bindings, out: &mut String) {
        for k in 0..=255u8 {
            match (self.get(k), base.get(k)) {
                (Some(b), was) if was != Some(b) => {
                    out.push_str(&format!("bind \"{}\" \"{}\"\n", keynum_to_string(k), b.text()));
                }
                (None, Some(_)) => out.push_str(&format!("unbind \"{}\"\n", keynum_to_string(k))),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keynum_names_match_key_keynum_to_string() {
        assert_eq!(keynum_to_string(b'a'), "a");
        assert_eq!(keynum_to_string(b'/'), "/");
        assert_eq!(keynum_to_string(K_SPACE), "SPACE");
        assert_eq!(keynum_to_string(K_UPARROW), "UPARROW");
        assert_eq!(keynum_to_string(K_MOUSE1), "MOUSE1");
        assert_eq!(keynum_to_string(K_F1), "F1");
        assert_eq!(keynum_to_string(K_F12), "F12");
        assert_eq!(keynum_to_string(K_DEL), "DEL");
        assert_eq!(keynum_to_string(K_PAUSE), "PAUSE");
        assert_eq!(keynum_to_string(0), "UNKNOWN");
        // keys.c's keynames: JOY1..JOY4 at 203, AUX1..AUX32 at 207 (keys.h).
        assert_eq!([K_JOY1, 204, K_JOY4].map(keynum_to_string), ["JOY1", "JOY2", "JOY4"]);
        assert_eq!([K_AUX1, 211, K_AUX29, K_AUX32].map(keynum_to_string), ["AUX1", "AUX5", "AUX29", "AUX32"]);
        assert_eq!((string_to_keynum("joy3"), string_to_keynum("AUX10")), (Some(205), Some(216)));
    }

    #[test]
    fn string_to_keynum_is_key_string_to_keynum() {
        assert_eq!(string_to_keynum("a"), Some(b'a'));
        assert_eq!(string_to_keynum("W"), Some(b'w'), "one character: the key that types it");
        assert_eq!(string_to_keynum("/"), Some(b'/'));
        assert_eq!(string_to_keynum("space"), Some(K_SPACE), "names in any case");
        assert_eq!(string_to_keynum("MOUSE2"), Some(K_MOUSE2));
        assert_eq!(string_to_keynum("F10"), Some(K_F1 + 9));
        assert_eq!(string_to_keynum("SEMICOLON"), Some(b';'));
        assert_eq!(string_to_keynum("PAUSE"), Some(K_PAUSE));
        assert_eq!((string_to_keynum(""), string_to_keynum("NOSUCHKEY")), (None, None));
        for k in (1..=255u8).filter(|&k| keynum_to_string(k) != "UNKNOWN" && k != b' ') {
            assert_eq!(string_to_keynum(&keynum_to_string(k)), Some(k.to_ascii_lowercase()), "{k}");
        }
    }

    #[test]
    fn a_binding_is_a_known_command_or_a_console_line() {
        assert_eq!(Binding::parse("+FORWARD"), Some(Binding::Command(BIND_FORWARD)));
        assert_eq!(Binding::parse("impulse 3"), Some(Binding::Command(BIND_IMPULSE_0 + 3)));
        assert_eq!(Binding::parse("god"), Some(Binding::Line("god".into())));
        assert_eq!(Binding::parse(""), None);
        assert_eq!(Binding::parse("god").unwrap().text(), "god");
        assert_eq!(Binding::Command(BIND_PAUSE).text(), "pause");
        for (i, c) in BIND_COMMANDS.iter().enumerate() {
            assert_eq!(Binding::parse(c), Some(Binding::Command(i)));
        }
    }

    #[test]
    fn default_cfg_binds_enter_mouse2_mlook_klook() {
        let b = Bindings::default_cfg();
        assert_eq!(b.command(K_ENTER), Some(BIND_JUMP));
        assert_eq!(b.command(K_MOUSE2), Some(BIND_FORWARD));
        assert_eq!(b.command(b'\\'), Some(BIND_MLOOK));
        assert_eq!(b.command(K_MOUSE3), Some(BIND_MLOOK));
        assert_eq!(b.command(K_INS), Some(BIND_KLOOK));
    }

    #[test]
    fn default_cfg_is_ids_and_wasd_is_the_2026_layout() {
        let id = Bindings::default_cfg();
        assert_eq!((id.command(b'a'), id.command(b'd'), id.command(b'c')), (Some(BIND_LOOKUP), Some(BIND_MOVEUP), Some(BIND_MOVEDOWN)));
        assert_eq!((id.get(b'w'), id.get(b's')), (None, None), "default.cfg leaves w and s unbound");
        let wasd = id.clone().with_wasd();
        assert_eq!(
            [b'w', b's', b'a', b'd', b'c'].map(|k| wasd.command(k)),
            [Some(BIND_FORWARD), Some(BIND_BACK), Some(BIND_MOVELEFT), Some(BIND_MOVERIGHT), Some(BIND_MOVEDOWN)]
        );
        let mut changes = String::new();
        wasd.write_changes(&id, &mut changes);
        assert_eq!(
            changes,
            "bind \"a\" \"+moveleft\"\nbind \"d\" \"+moveright\"\nbind \"s\" \"+back\"\nbind \"w\" \"+forward\"\n"
        );
        let mut changes = String::new();
        id.write_changes(&wasd, &mut changes);
        assert_eq!(changes, "bind \"a\" \"+lookup\"\nbind \"d\" \"+moveup\"\nunbind \"s\"\nunbind \"w\"\n");
    }

    /// default.cfg binds no joystick key (id's joystick users bound their
    /// own); the 2026 pad layout binds them by the modern twin-stick habit.
    #[test]
    fn the_2026_pad_layout_binds_joy_and_aux_keys() {
        let id = Bindings::default_cfg();
        assert!((K_JOY1..=K_AUX32).all(|k| id.get(k).is_none()));
        let pad = id.with_gamepad();
        let aux = |n: u8| K_AUX1 + n - 1;
        assert_eq!([K_JOY1, aux(7), aux(8), aux(6), aux(9)].map(|k| pad.command(k)),
                   [Some(BIND_JUMP), Some(BIND_JUMP), Some(BIND_ATTACK), Some(BIND_CHANGEWEAPON), Some(BIND_SHOWSCORES)]);
        assert_eq!(pad.get(aux(10)).map(Binding::text), Some("togglemenu"), "Start");
        assert_eq!(pad.get(aux(5)).map(Binding::text), Some("impulse 12"), "LB: the previous weapon");
        assert_eq!((pad.get(K_JOY1 + 2), pad.get(aux(12))), (None, None), "X and R3 free");
    }

    /// AUDIT.md's "Missing `default.cfg` binds: F1-F4, F6, F9, F10, F12":
    /// each is a console Line (none is one of BIND_COMMANDS), so Customize
    /// controls (the BINDNAMES rows) never lists it, but a rebind over it —
    /// or Reset to defaults — treats it like any other key. F5/F7/F8/F11
    /// stay unbound, as in id's own `default.cfg`.
    #[test]
    fn default_cfg_binds_ids_function_key_shortcuts() {
        let b = Bindings::default_cfg();
        let line = |k| b.get(k).map(Binding::text);
        assert_eq!(line(K_F1), Some("help"));
        assert_eq!(line(K_F1 + 1), Some("menu_save"));
        assert_eq!(line(K_F1 + 2), Some("menu_load"));
        assert_eq!(line(K_F1 + 3), Some("menu_options"));
        assert_eq!(line(K_F1 + 5), Some("echo Quicksaving...; wait; save quick"));
        assert_eq!(line(K_F1 + 8), Some("echo Quickloading...; wait; load quick"));
        assert_eq!(line(K_F1 + 9), Some("quit"));
        assert_eq!(line(K_F1 + 11), Some("screenshot"));
        for k in [K_F1 + 4, K_F1 + 6, K_F1 + 7, K_F1 + 10] {
            assert_eq!(b.get(k), None, "F5/F7/F8/F11 stay unbound");
        }
        for k in [K_F1, K_F1 + 1, K_F1 + 2, K_F1 + 3, K_F1 + 5, K_F1 + 8, K_F1 + 9, K_F1 + 11] {
            assert!(matches!(b.get(k), Some(Binding::Line(_))), "F{}", k - K_F1 + 1);
        }
    }

    #[test]
    fn pause_is_bound_to_pause_as_in_default_cfg() {
        assert_eq!(Bindings::default_cfg().command(K_PAUSE), Some(BIND_PAUSE));
    }

    #[test]
    fn the_console_key_is_toggleconsole_as_in_default_cfg() {
        let b = Bindings::default_cfg();
        assert_eq!(b.command(b'`'), Some(BIND_TOGGLECONSOLE));
        assert_eq!(b.command(b'~'), Some(BIND_TOGGLECONSOLE));
    }

    /// Key_Init's tables: what the console keeps, what the menu lets through,
    /// what Shift types.
    #[test]
    fn key_init_tables_are_ids() {
        for k in 32..=127u8 {
            assert_eq!(consolekey(k), k != b'`' && k != b'~', "{k}");
        }
        for k in [K_ENTER, K_TAB, K_UPARROW, K_DOWNARROW, K_LEFTARROW, K_RIGHTARROW, K_PGUP, K_PGDN, K_SHIFT, K_MWHEELUP, K_MWHEELDOWN] {
            assert!(consolekey(k), "{k}");
        }
        for k in [K_ESCAPE, K_HOME, K_END, K_DEL, K_INS, K_CTRL, K_ALT, K_F1, K_MOUSE1, K_PAUSE, 0] {
            assert!(!consolekey(k), "{k}");
        }
        assert!(menubound(K_ESCAPE) && menubound(K_F1) && menubound(K_F12));
        assert!(!menubound(b'`') && !menubound(K_ENTER) && !menubound(K_MOUSE1));
        assert_eq!((keyshift(b'a'), keyshift(b'2'), keyshift(b'`'), keyshift(b'\\')), (b'A', b'@', b'~', b'|'));
        assert_eq!((keyshift(b'A'), keyshift(K_ENTER), keyshift(K_UPARROW)), (b'A', K_ENTER, K_UPARROW));
    }

    #[test]
    fn tab_shows_the_scores_as_in_default_cfg() {
        assert_eq!(Bindings::default_cfg().command(K_TAB), Some(BIND_SHOWSCORES));
    }

    #[test]
    fn digits_are_impulses_as_in_default_cfg() {
        let b = Bindings::default_cfg();
        for n in 0..=8u8 {
            assert_eq!(b.command(b'0' + n), Some(BIND_IMPULSE_0 + n as usize));
        }
        assert_eq!(b.get(b'9'), None, "default.cfg leaves 9 unbound");
    }

    #[test]
    fn find_and_unbind_by_command_as_menu_c() {
        let mut b = Bindings::default_cfg();
        assert_eq!(b.find_keys_for_command(BIND_JUMP), [Some(K_ENTER), Some(K_SPACE)]);
        assert_eq!(b.find_keys_for_command(BIND_ATTACK), [Some(K_CTRL), Some(K_MOUSE1)]);
        b.unbind_command(BIND_JUMP);
        assert_eq!(b.find_keys_for_command(BIND_JUMP), [None, None]);
        let mut held = [false; 256];
        held[K_CTRL as usize] = true;
        assert!(b.held(BIND_ATTACK, &held) && !b.held(BIND_FORWARD, &held));
    }
}
