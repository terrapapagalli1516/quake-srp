//! Key numbers, their names, and the default bindings.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/keys.h` / `keys.c` (`K_*`, `Key_KeynumToString`) and the
//! binds of `default.cfg`.

use crate::menu::{
    BIND_ATTACK, BIND_BACK, BIND_CENTERVIEW, BIND_CHANGEWEAPON, BIND_FORWARD, BIND_JUMP, BIND_LEFT,
    BIND_LOOKDOWN, BIND_LOOKUP, BIND_MOVEDOWN, BIND_MOVELEFT, BIND_MOVERIGHT, BIND_RIGHT,
    BIND_IMPULSE_0, BIND_KLOOK, BIND_MLOOK, BIND_SHOWSCORES, BIND_SIZEDOWN, BIND_SIZEUP,
    BIND_SPEED, BIND_STRAFE,
};

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
        f @ K_F1..=K_F12 => return format!("F{}", f - K_F1 + 1),
        _ => "UNKNOWN",
    }
    .to_string()
}

/// The boot key bindings: id's `default.cfg` (from the pak) for every key the
/// page delivers, PLUS this port's established WASD layout (the shareware
/// `default.cfg` predates WASD — it binds `a` to `+lookup` and `d` to `+moveup`;
/// this port has always shipped WASD movement, so WASD overrides those four,
/// exactly as a player's `config.cfg` would).
pub(crate) fn default_bindings() -> [Option<u8>; 256] {
    let mut b: [Option<u8>; 256] = [None; 256];
    let mut bind = |key: u8, cmd: usize| b[key as usize] = Some(cmd as u8);
    // default.cfg (id, verbatim — the keys the page can deliver):
    bind(K_ALT, BIND_STRAFE);
    bind(b',', BIND_MOVELEFT);
    bind(b'.', BIND_MOVERIGHT);
    bind(K_DEL, BIND_LOOKDOWN);
    bind(K_PGDN, BIND_LOOKUP);
    bind(K_END, BIND_CENTERVIEW);
    bind(b'z', BIND_LOOKDOWN);
    bind(K_SHIFT, BIND_SPEED);
    bind(b'+', BIND_SIZEUP);
    bind(b'=', BIND_SIZEUP);
    bind(b'-', BIND_SIZEDOWN);
    bind(K_CTRL, BIND_ATTACK);
    bind(K_UPARROW, BIND_FORWARD);
    bind(K_DOWNARROW, BIND_BACK);
    bind(K_LEFTARROW, BIND_LEFT);
    bind(K_RIGHTARROW, BIND_RIGHT);
    bind(K_SPACE, BIND_JUMP);
    bind(K_ENTER, BIND_JUMP);
    bind(K_TAB, BIND_SHOWSCORES);
    // bind 1 "impulse 1" .. bind 8 "impulse 8", bind 0 "impulse 0" — by key
    // NUMBER, so the digit row selects weapons whatever Shift or the layout.
    for n in 0..=8u8 {
        bind(b'0' + n, BIND_IMPULSE_0 + n as usize);
    }
    bind(b'/', BIND_CHANGEWEAPON);
    bind(K_MOUSE1, BIND_ATTACK);
    bind(K_MOUSE2, BIND_FORWARD);
    bind(b'\\', BIND_MLOOK);
    bind(K_MOUSE3, BIND_MLOOK);
    bind(K_INS, BIND_KLOOK);
    // This port's established layout (overrides default.cfg's a=+lookup,
    // d=+moveup; w/s were unbound there):
    bind(b'w', BIND_FORWARD);
    bind(b's', BIND_BACK);
    bind(b'a', BIND_MOVELEFT);
    bind(b'd', BIND_MOVERIGHT);
    bind(b'c', BIND_MOVEDOWN);
    b
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
        assert_eq!(keynum_to_string(0), "UNKNOWN");
    }

    #[test]
    fn default_cfg_binds_enter_mouse2_mlook_klook() {
        let b = default_bindings();
        assert_eq!(b[K_ENTER as usize], Some(BIND_JUMP as u8));
        assert_eq!(b[K_MOUSE2 as usize], Some(BIND_FORWARD as u8));
        assert_eq!(b[b'\\' as usize], Some(BIND_MLOOK as u8));
        assert_eq!(b[K_MOUSE3 as usize], Some(BIND_MLOOK as u8));
        assert_eq!(b[K_INS as usize], Some(BIND_KLOOK as u8));
    }

    #[test]
    fn tab_shows_the_scores_as_in_default_cfg() {
        assert_eq!(default_bindings()[K_TAB as usize], Some(BIND_SHOWSCORES as u8));
    }

    #[test]
    fn digits_are_impulses_as_in_default_cfg() {
        let b = default_bindings();
        for n in 0..=8u8 {
            assert_eq!(b[(b'0' + n) as usize], Some((BIND_IMPULSE_0 + n as usize) as u8));
        }
        assert_eq!(b[b'9' as usize], None, "default.cfg leaves 9 unbound");
    }
}
