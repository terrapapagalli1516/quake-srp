//! Player input — in_win.c (`IN_MouseMove`) and keys.c's `Key_Event`: the
//! key/mouse exports the page calls. Every key goes through [`key_event`],
//! which hands it to the menu, the console or its binding as id's does; the
//! per-frame `KeyMove` the held bindings feed (cl_input.c's `CL_BaseMove`/
//! `CL_AdjustAngles`) is [`quake_rs::client::cl_input`]'s.

use quake_rs::client::cl_input::{clamp_pitch, V_CENTERSPEED};
use quake_rs::keys::{
    consolekey, keynum_to_string, keyshift, menubound, K_BACKSPACE, K_ESCAPE, K_PAUSE, K_SHIFT,
};
use quake_rs::menu::{BIND_IMPULSE_0, BIND_PAUSE, BIND_TOGGLECONSOLE};
use quake_rs::render::{BIND_CENTERVIEW, BIND_CHANGEWEAPON, BIND_SIZEDOWN, BIND_SIZEUP, BIND_STRAFE};

use crate::app::{ensure_app, App, KeyDest, APP};
use crate::menu::{apply_menu_action, run_menu_deferred, MenuDeferred};

// --- mouse cvars (in_win.c IN_MouseMove) -----------------------------------

/// The port's `m_yaw`/`m_pitch` magnitude: degrees of turn per (browser mouse
/// count x `sensitivity`). DEVIATION (calibration only): the C's m_yaw/m_pitch
/// are 0.022 deg per Windows mickey; browser `movementX` counts aren't mickeys,
/// and this port has always shipped a 0.16 deg/count feel at the default
/// sensitivity 3 — so the constant is 0.16/3. The multiplicative STRUCTURE is
/// the C's exactly: counts x sensitivity x m_yaw — and the m_pitch SIGN is the
/// Invert Mouse toggle (`m_pitch.value < 0`).
const M_YAW_PORT: f32 = 0.16 / 3.0;
const M_PITCH_PORT: f32 = 0.16 / 3.0;
/// `m_side` ("0.8"): sidemove units per (count x sensitivity) when mouse X is
/// routed to strafe (lookstrafe / +strafe). The C's literal value — the result
/// feeds wishspeed, which sv_maxspeed clamps, so calibration is forgiving.
const M_SIDE: f32 = 0.8;
/// `m_forward` ("1.0"): forwardmove units per count while `+strafe` holds mouse
/// Y out of the pitch path (IN_MouseMove's else branch).
const M_FORWARD: f32 = 1.0;

pub(crate) fn set_move(fwd: f32, side: f32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_fwd = fwd;
            w.in_side = side;
        }
    });
}

/// Set whether the attack button is held (drives the QuakeC weapon code).
pub(crate) fn set_attack(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_attack = on != 0;
        }
    });
}

/// Set whether the jump key is held. Maps to UserCmd button bit 1 -> the
/// player's `button2`, which the QuakeC PlayerJump reads to jump when on the
/// ground (velocity_z = 270).
pub(crate) fn set_jump(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_jump = on != 0;
        }
    });
}

/// Set whether the swim-DOWN key (`c`, the `+movedown` key) is held. Maps to a
/// negative `UserCmd.upmove`, which `SV_WaterMove` reads to sink while waist-deep
/// in water. Out of water it has no effect (the walk move ignores upmove).
pub(crate) fn set_movedown(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_down = on != 0;
        }
    });
}

/// Queue a one-shot impulse for the next frame (e.g. weapon select: 1 = axe,
/// 2 = shotgun, 3 = super shotgun, 4 = nailgun, ... — exactly the QuakeC
/// `impulse` numbers). Applied to the next `step_walk` UserCmd then cleared.
pub(crate) fn set_impulse(n: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.next_impulse = n;
        }
    });
}

// --- keys.c Key_Event: every key by Quake keynum ----------------------------

/// What a key event leaves for after the App borrow: a menu action that
/// replaces the walk or reaches the page, or a console line to execute.
pub(crate) enum KeyAfter {
    Menu(MenuDeferred),
    Console(String),
}

/// Run what a key event left for after the App borrow.
pub(crate) fn run_key_after(after: Option<KeyAfter>) {
    match after {
        Some(KeyAfter::Menu(d)) => run_menu_deferred(d),
        Some(KeyAfter::Console(line)) => crate::host_cmd::execute_console_command(&line),
        None => {}
    }
}

/// keys.c's `Key_Event (key, down)`, the one door every key goes through, by
/// Quake keynum (keys.h: printable ASCII is itself, lower case; the arrows,
/// modifiers, F-keys and editing keys take the 128+ block; mouse buttons
/// 200+). In id's order:
///
/// - New Game's "Are you sure?" (`SCR_ModalMessage`) takes every key first;
/// - a second key down without a key up is autorepeat, ignored but for
///   Backspace and Pause; an unbound mouse button (keys 200 and up) prints
///   "MOUSE2 is unbound, hit F4 to set.";
/// - Escape is special, so no binding can take the menu away: in the menu it
///   is the screen's Escape (`M_Keydown`), elsewhere `M_ToggleMenu_f`;
/// - during demo playback, with the game's keyboard, a console key (any
///   printable key, Enter, Tab, the arrows, ...) brings up the main menu;
/// - a key up releases its `+` binding wherever the keyboard is (so a key
///   released behind the menu or console never sticks);
/// - a key down runs its binding where id's runs it — in the game (with
///   nothing playing, only the keys the forced-up console does not keep),
///   with the console down only the keys it does not keep (`consolekeys`),
///   with the menu up only Escape and F1..F12 (`menubound`): `+` commands
///   hold (`keys_held`, read each frame by `CL_BaseMove`), the others run
///   once (`impulse N`, `centerview`, `sizeup`, `pause`, `toggleconsole`);
/// - any other key down, with Shift applied (`keyshift[]`), goes to the menu
///   (`M_Keydown`) or the console (`Key_Console`).
///
/// `ch` is the character the page's keyboard layout typed with the key (0
/// for none): what the console and the Setup name fields insert, where id
/// inserts the key number shifted by its US `keyshift[]` table (used when
/// `ch` is 0). A non-ASCII `ch` types nothing (`Key_Console`: only 32..127).
pub(crate) fn key_event_in(a: &mut App, key: u8, down: bool, ch: u32) -> Option<KeyAfter> {
    let k = key as usize;
    if !down {
        a.key_repeats[k] = 0;
    }
    // SCR_ModalMessage: `key_count = -1` — every event is the question's.
    // (A key up still releases its `+` binding here, so nothing held when
    // the question came up sticks: id's returns before that.)
    if a.menu.visible && a.menu.new_game_confirm() {
        if down {
            let action = a.menu.modal_key(key);
            return apply_menu_action(a, action).map(KeyAfter::Menu);
        }
        a.keys_held[k] = false;
        return None;
    }
    if down {
        a.key_repeats[k] = a.key_repeats[k].saturating_add(1);
        if key != K_BACKSPACE && key != K_PAUSE && a.key_repeats[k] > 1 {
            return None; // ignore most autorepeats
        }
        if key >= 200 && a.menu.action_for_key(key).is_none() {
            a.console.println(format!("{} is unbound, hit F4 to set.", keynum_to_string(key)));
        }
    }
    if key == K_SHIFT {
        a.shift_down = down;
    }
    // Escape is handled specially, so the user can never unbind it.
    if key == K_ESCAPE {
        if !down {
            return None;
        }
        if a.key_dest() == KeyDest::Menu {
            let action = a.menu.keydown(K_ESCAPE, None);
            return apply_menu_action(a, action).map(KeyAfter::Menu);
        }
        a.m_toggle_menu();
        return None;
    }
    // Key ups only release the `+` button commands (the `-` half), in every
    // key_dest.
    if !down {
        a.keys_held[k] = false;
        return None;
    }
    // During demo playback, most keys bring up the main menu.
    if a.demoplayback() && consolekey(key) && a.key_dest() == KeyDest::Game {
        a.m_toggle_menu();
        return None;
    }
    let dest = a.key_dest();
    let to_binding = match dest {
        KeyDest::Menu => menubound(key),
        KeyDest::Console => !consolekey(key),
        // con_forcedup: nothing plays, the console covers the screen.
        KeyDest::Game => !a.disconnected || !consolekey(key),
    };
    if to_binding {
        run_binding(a, key);
        return None;
    }
    let key = if a.shift_down { keyshift(key) } else { key };
    let text = match ch {
        0 => (32..127).contains(&key).then_some(key),
        c => (32..127).contains(&c).then_some(c as u8),
    };
    match dest {
        KeyDest::Menu => {
            let action = a.menu.keydown(key, text);
            apply_menu_action(a, action).map(KeyAfter::Menu)
        }
        KeyDest::Game | KeyDest::Console => crate::console::key_console(a, key, text).map(KeyAfter::Console),
    }
}

/// `Key_Event`'s command dispatch for a key down: `kb = keybindings[key]`.
/// A `+` command is held until its key comes up (`keys_held`, the button
/// state `CL_BaseMove` reads each frame); the rest run once, now.
fn run_binding(a: &mut App, key: u8) {
    let Some(cmd) = a.menu.action_for_key(key) else { return };
    a.keys_held[key as usize] = true;
    match cmd {
        // "impulse N" (IN_Impulse: `in_impulse = atoi(argv[1])`), sent with
        // the next move; "impulse 10" is the change-weapon row.
        c if (BIND_IMPULSE_0..=BIND_IMPULSE_0 + 8).contains(&c) || c == BIND_CHANGEWEAPON => {
            let n = if c == BIND_CHANGEWEAPON { 10 } else { (c - BIND_IMPULSE_0) as i32 };
            if let Some(w) = a.walk.as_mut() {
                w.next_impulse = n;
            }
        }
        BIND_CENTERVIEW => {
            // "centerview" -> V_StartPitchDrift (view.c): seed the drift.
            if let Some(w) = a.walk.as_mut() {
                if !w.pitch_drift || w.pitch_vel == 0.0 {
                    w.pitch_vel = V_CENTERSPEED;
                    w.pitch_drift = true;
                }
            }
        }
        // default.cfg's `+`/`=` "sizeup" and `-` "sizedown" (SCR_SizeUp_f /
        // SCR_SizeDown_f): step the viewsize; the next frame reframes.
        BIND_SIZEUP => a.menu.size_up(),
        BIND_SIZEDOWN => a.menu.size_down(),
        BIND_PAUSE => crate::host_cmd::host_pause(a),
        BIND_TOGGLECONSOLE => a.toggle_console(),
        _ => {}
    }
}

/// One key event by Quake keynum — keys.c's `Key_Event` ([`key_event_in`]):
/// `down` 1 for a press (the keyboard's autorepeats included: they are
/// ignored here as in id, Backspace aside), 0 for the release. `ch` is the
/// character the key typed on the page's keyboard layout (0 for none): the
/// text the console and the name fields insert. The page sends every key
/// here, so the menu, the console and the game split the keyboard as id's
/// does.
pub(crate) fn key_event(keynum: i32, down: i32, ch: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    let mut after = None;
    ensure_app(|a| after = key_event_in(a, keynum as u8, down != 0, ch.max(0) as u32));
    run_key_after(after);
}

/// A key press and release, as the automation's and tests' menu exports
/// send them.
pub(crate) fn press(key: u8) {
    key_event(key as i32, 1, 0);
    key_event(key as i32, 0, 0);
}

/// A key down, by Quake keynum: [`key_event`] with no layout character.
pub(crate) fn key_down(keynum: i32) {
    key_event(keynum, 1, 0);
}

/// A key up, by Quake keynum: [`key_event`].
pub(crate) fn key_up(keynum: i32) {
    key_event(keynum, 0, 0);
}

/// vid_win.c's `ClearAllStates`, what the page runs when the window loses
/// the keyboard (a key released elsewhere never comes up here): an up for
/// every key, so no `+` command stays held, then `Key_ClearStates` (the
/// autorepeat counts; Shift is up).
pub(crate) fn key_clear_states() {
    ensure_app(|a| {
        a.keys_held = [false; 256];
        a.key_repeats = [0; 256];
        a.shift_down = false;
    });
}

/// 1 when the engine currently believes Quake keynum `keynum` is held — a
/// read-only verification/debug export (like [`menu_screen_id`]). The browser
/// harness uses it to prove the page's `e.code` punctuation mapping keeps
/// key-down/key-up SYMMETRIC under Shift (press ',', add Shift, release ','
/// must clear keynum 44, even though the release reports `key == '<'` —
/// the C's scancode semantics, in_win.c `scantokey`).
pub(crate) fn key_is_down(keynum: i32) -> i32 {
    if !(0..256).contains(&keynum) {
        return 0;
    }
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.keys_held[keynum as usize] as i32)
            .unwrap_or(0)
    })
}

/// Raw mouse deltas (browser `movementX`/`movementY` counts) — a port of
/// IN_MouseMove (in_win.c): counts scale by the `sensitivity` cvar; mouse X
/// turns yaw, OR strafes (`m_side`) while `lookstrafe` is on or `+strafe` is
/// held; mouse Y drives pitch (sign = Invert Mouse, `m_pitch.value < 0`),
/// clamped 80/-70, OR feeds forwardmove (`m_forward`) while `+strafe` holds it
/// out of the pitch path. Mouse-look is permanent under pointer lock (`+mlook`
/// held), so any motion stops an active pitch drift (V_StopPitchDrift). Gated
/// behind the menu/console like `look`.
pub(crate) fn mouse_move(dx: f32, dy: f32) {
    ensure_app(|a| {
        if a.menu.visible || a.console.open {
            return;
        }
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        // mouse_x *= sensitivity.value (the raw 1..11 cvar, like the C — the
        // 0.16/3 port calibration lives in M_YAW_PORT/M_PITCH_PORT).
        let mx = dx * a.menu.sensitivity();
        let my = dy * a.menu.sensitivity();
        let strafe_held = a
            .keys_held
            .iter()
            .enumerate()
            .any(|(k, &h)| h && a.menu.action_for_key(k as u8) == Some(BIND_STRAFE));
        let lookstrafe = a.menu.lookstrafe();
        let invert = a.menu.invert_mouse();
        if let Some(w) = a.walk.as_mut() {
            // if (in_strafe || (lookstrafe && in_mlook)) sidemove += m_side*mx
            // else viewangles[YAW] -= m_yaw*mx. (+mlook is always held here.)
            if strafe_held || lookstrafe {
                w.mouse_side += M_SIDE * mx;
            } else {
                w.yaw -= M_YAW_PORT * mx;
            }
            // if (in_mlook) V_StopPitchDrift() — every mlook mouse move.
            w.pitch_drift = false;
            w.pitch_vel = 0.0;
            // if (in_mlook && !in_strafe) pitch += m_pitch*my (clamped 80/-70)
            // else forwardmove -= m_forward*my.
            if !strafe_held {
                let m_pitch = if invert { -M_PITCH_PORT } else { M_PITCH_PORT };
                w.pitch = clamp_pitch(w.pitch + m_pitch * my);
            } else {
                w.mouse_fwd -= M_FORWARD * my;
            }
        }
    });
}

/// The pointer lock was released. This port's `+mlook` is permanently held
/// while the pointer is locked, so unlock IS the mlook release — the faithful
/// `lookspring` trigger (`IN_MLookUp`, cl_input.c: when `+mlook` releases and
/// `lookspring.value` is set, `V_StartPitchDrift()` re-centres the view).
pub(crate) fn pointer_unlocked() {
    ensure_app(|a| {
        if !a.menu.lookspring() {
            return;
        }
        if let Some(w) = a.walk.as_mut() {
            // V_StartPitchDrift (view.c): seed pitchvel, clear nodrift.
            if !w.pitch_drift || w.pitch_vel == 0.0 {
                w.pitch_vel = V_CENTERSPEED;
                w.pitch_drift = true;
            }
        }
    });
}

/// The player's current look pitch in degrees (+down, Quake convention) — a
/// read-only verification/debug export (the browser checks Invert Mouse and
/// lookspring flip/centre the pitch through it). 0 when no walk is live.
pub(crate) fn player_pitch() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|a| a.walk.as_ref().map(|w| w.pitch))
            .unwrap_or(0.0)
    })
}

/// The Options "Mouse speed" as a sensitivity multiplier (default 1.0). The page
/// multiplies its baseline look sensitivity by this. Reads from the App-level menu;
/// 1.0 when the app has not been created yet.
pub(crate) fn mouse_sensitivity() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.mouse_sensitivity())
            .unwrap_or(1.0)
    })
}

pub(crate) fn look(dyaw: f32, dpitch: f32) {
    ensure_app(|a| {
        // While the menu OR console is up, Quake freezes the view (key_dest !=
        // key_game stops feeding mouse-look). Match that: ignore look input
        // behind either overlay so the idle world doesn't rotate underneath it.
        if a.menu.visible || a.console.open {
            return;
        }
        if let Some(w) = a.walk.as_mut() {
            w.yaw += dyaw;
            w.pitch = clamp_pitch(w.pitch + dpitch);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::boot;
    use crate::host::step;
    use crate::menu::{menu_backspace, menu_cancel, menu_down, menu_right, menu_select, menu_visible};
    use crate::snd_dma::listener;
    use crate::test_util::*;

    /// Second review: Key_Event hands a key down to its binding only where
    /// key_dest lets it through (keys.c: `key_dest == key_menu &&
    /// menubound[key]`, `key_dest == key_console && !consolekeys[key]`, or
    /// the game); Tab pressed with the menu up is M_Keydown's, so it is not
    /// +showscores when the menu closes with Tab still held. The key up is
    /// harmless. With the console down, Shift (a console key) is the
    /// console's, Ctrl (not one) reaches +attack as in the C.
    #[test]
    fn keys_pressed_in_the_menu_or_console_do_not_hold_their_binding() {
        use quake_rs::client::cl_input::derive_key_move;
        let showscores = || APP.with(|c| {
            let a = c.borrow();
            let a = a.as_ref().unwrap();
            derive_key_move(&a.menu, &a.keys_held).showscores
        });
        assert_eq!(boot(), 1);
        assert_eq!(menu_visible(), 1, "boot opens the menu over e1m1");
        key_down(9); // Tab, into the menu
        menu_cancel(); // the menu closes, Tab still held
        assert_eq!(menu_visible(), 0);
        step(0.05);
        assert_eq!(key_is_down(9), 0);
        assert!(!showscores(), "Tab went to M_Keydown, not +showscores");
        key_up(9);
        key_down(9); // pressed in the game: the scoreboard
        assert!(showscores());
        key_up(9);
        assert!(!showscores());
        crate::console::console_toggle();
        key_down(134); // Shift: consolekeys[K_SHIFT]
        key_down(133); // Ctrl: not a console key
        assert_eq!((key_is_down(134), key_is_down(133)), (0, 1));
        key_up(133);
        key_up(134);
        crate::console::console_toggle();
    }

    /// Final review (UI): M_Quit_Key answers only y/Y (quit) and n/N/Escape
    /// (back); Enter and every other key leave the prompt up — the port used
    /// to take Enter as Yes.
    #[test]
    fn the_quit_prompt_takes_only_y_and_n() {
        use quake_rs::keys::{K_DOWNARROW, K_ENTER, K_UPARROW};
        assert_eq!(boot(), 1);
        let screen = || crate::menu::menu_screen_id();
        press(K_UPARROW); // Main: Quit
        press(K_ENTER);
        assert_eq!(screen(), 9, "the prompt");
        for k in [K_ENTER, b' ', b'q', K_DOWNARROW, b'`'] {
            press(k);
            assert_eq!((menu_visible(), screen()), (1, 9), "key {k} leaves the prompt up");
        }
        press(b'n');
        assert_eq!((menu_visible(), screen()), (1, 0), "n: back to Main");
        press(K_ENTER);
        key_event(i32::from(K_SHIFT), 1, 0);
        key_event(i32::from(b'n'), 1, i32::from(b'N'));
        key_event(i32::from(b'n'), 0, 0);
        assert_eq!(screen(), 0, "N too");
        press(K_ENTER);
        key_event(i32::from(b'y'), 1, i32::from(b'Y'));
        key_event(i32::from(b'y'), 0, 0);
        key_event(i32::from(K_SHIFT), 0, 0);
        assert_eq!(menu_visible(), 0, "Y quits (the page: the menu closes)");
    }

    /// Key_Event's order: the console key is a binding (`toggleconsole`),
    /// and with the menu up only Escape and F1..F12 reach bindings, so over
    /// the menu it is M_Keydown's (which ignores it) — the port's page opened
    /// the console over the menu. During a key grab M_Keys_Key refuses it
    /// and the grab ends. In the game it opens the console, in the console it
    /// closes it.
    #[test]
    fn the_console_key_over_the_menu_is_the_menus() {
        use quake_rs::keys::K_ENTER;
        assert_eq!(boot(), 1);
        press(b'`');
        assert_eq!((menu_visible(), crate::console::console_visible()), (1, 0), "not over the menu");
        menu_down();
        menu_down();
        menu_select(); // Options
        menu_select(); // Customize controls
        menu_select(); // grab for "attack"
        assert_eq!(crate::menu::menu_bind_grabbing(), 1);
        press(b'`');
        assert_eq!(crate::menu::menu_bind_grabbing(), 0, "the grab ends");
        assert_eq!(crate::console::console_visible(), 0);
        let bound = APP.with(|c| c.borrow().as_ref().unwrap().menu.action_for_key(b'`'));
        assert_eq!(bound, Some(BIND_TOGGLECONSOLE), "and the console key stays the console's");
        for _ in 0..3 {
            press(K_ESCAPE);
        }
        assert_eq!(menu_visible(), 0);
        press(b'`');
        assert_eq!(crate::console::console_visible(), 1, "in the game it opens the console");
        press(b'`');
        assert_eq!(crate::console::console_visible(), 0, "and closes it");
        // Escape with the console down: M_ToggleMenu_f -> Con_ToggleConsole_f.
        press(b'`');
        press(K_ESCAPE);
        assert_eq!((crate::console::console_visible(), menu_visible()), (0, 0));
        let _ = K_ENTER;
    }

    /// M_Keys_Key binds whatever key comes: the mouse buttons are K_MOUSE1..3
    /// (in_win.c), bindable on Customize controls as in id — the page
    /// forwards them while a key is being grabbed. An unbound mouse button
    /// says so (Key_Event: "MOUSE2 is unbound, hit F4 to set.").
    #[test]
    fn mouse_buttons_bind_on_customize_controls() {
        use quake_rs::keys::{K_MOUSE2, K_MOUSE3};
        assert_eq!(boot(), 1);
        menu_down();
        menu_down();
        menu_select(); // Options
        menu_select(); // Customize controls
        menu_down();
        menu_down(); // "jump / swim up"
        menu_select();
        key_event(i32::from(K_MOUSE3), 1, 0);
        key_event(i32::from(K_MOUSE3), 0, 0);
        let (jump, grab) = APP.with(|c| {
            let a = c.borrow();
            let m = &a.as_ref().unwrap().menu;
            (m.action_for_key(K_MOUSE3), m.bind_grabbing())
        });
        assert_eq!((jump, grab), (Some(quake_rs::render::BIND_JUMP), false), "MOUSE3 is now +jump");
        // Backspace on the "walk forward" row unbinds MOUSE2 with the rest.
        menu_down(); // "walk forward"
        menu_backspace();
        for _ in 0..3 {
            menu_cancel();
        }
        let lines = || APP.with(|c| c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect::<Vec<_>>());
        key_event(i32::from(K_MOUSE2), 1, 0);
        key_event(i32::from(K_MOUSE2), 0, 0);
        assert_eq!(lines().last().map(String::as_str), Some("MOUSE2 is unbound, hit F4 to set."));
        key_event(i32::from(K_MOUSE3), 1, 0);
        assert_eq!(key_is_down(i32::from(K_MOUSE3)), 1, "MOUSE3 holds +jump");
        key_event(i32::from(K_MOUSE3), 0, 0);
    }

    /// Key_Event ignores the keyboard's autorepeat (a second down without an
    /// up) but for Backspace: a held key types one character in the console,
    /// a held Backspace keeps erasing.
    #[test]
    fn autorepeat_is_ignored_but_for_backspace() {
        use quake_rs::keys::K_BACKSPACE;
        assert_eq!(boot(), 1);
        close_menu();
        press(b'`');
        for _ in 0..4 {
            key_event(i32::from(b'a'), 1, i32::from(b'a'));
        }
        key_event(i32::from(b'a'), 0, 0);
        press(b'b');
        press(b'c');
        let input = || APP.with(|c| c.borrow().as_ref().unwrap().console.input().to_string());
        assert_eq!(input(), "abc", "one a for the held key");
        for _ in 0..2 {
            key_event(i32::from(K_BACKSPACE), 1, 0);
        }
        key_event(i32::from(K_BACKSPACE), 0, 0);
        assert_eq!(input(), "a", "the held Backspace repeated");
        // Shift types keyshift[] when the page gives no character.
        key_event(i32::from(K_SHIFT), 1, 0);
        press(b'2');
        key_event(i32::from(K_SHIFT), 0, 0);
        // ...and the page's character (its keyboard layout) when it does; a
        // key that types none of ASCII types nothing.
        key_event(i32::from(b'2'), 1, 0xe9); // AZERTY's unshifted 2 is é
        key_event(i32::from(b'2'), 0, 0);
        key_event(i32::from(b';'), 1, i32::from(b'm'));
        key_event(i32::from(b';'), 0, 0);
        assert_eq!(input(), "a@m");
    }

    /// CENSUS F17: the digit row is `bind N "impulse N"` by key NUMBER (the
    /// page sends e.code's Digit* keynums, so Shift+2 and AZERTY's unshifted
    /// row still deliver keynum '2'): impulse 7 selects the rocket launcher.
    #[test]
    fn digit_keys_are_impulse_bindings() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.next_impulse = 9); // all weapons + ammo
        step(0.05);
        key_down(i32::from(b'7'));
        key_up(i32::from(b'7'));
        assert_eq!(walk_mut(|w| w.next_impulse), 7, "'7' queues impulse 7");
        for _ in 0..3 {
            step(0.05);
        }
        assert_eq!(player_field("weapon") as i32, IT_RL, "impulse 7 selected the launcher");
        key_down(i32::from(b'0'));
        assert_eq!(walk_mut(|w| w.next_impulse), 0, "'0' is impulse 0");
        key_up(i32::from(b'0'));
    }

    /// CENSUS F11: Tab is default.cfg's `+showscores` — while it is held the
    /// status bar shows the scorebar + Sbar_SoloScoreboard; it does not open
    /// the menu.
    #[test]
    fn tab_held_shows_the_solo_scoreboard() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        let sbar = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                let rows = 24 * a.render_w / 320; // the status strip, scaled
                a.fb[(a.render_h - rows) * a.render_w * 4..].to_vec()
            })
        };
        step(0.0);
        let stats = sbar();
        key_down(9); // K_TAB
        step(0.0);
        assert!(walk_mut(|w| w.key_move.showscores), "+showscores is held");
        assert_eq!(menu_visible(), 0, "Tab does not open the menu");
        let scores = sbar();
        assert_ne!(stats, scores, "the scoreboard replaces the stats while Tab is held");
        key_up(9);
        step(0.0);
        assert_eq!(sbar(), stats, "releasing Tab brings the stats back");
    }

    #[test]
    fn key_down_drives_movement_through_bindings_and_always_run_swaps_speeds() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Default binding: w = +forward at cl_forwardspeed 400 — Always Run
        // defaults ON in this port (Menu's DEVIATION note).
        key_down(i32::from(b'w'));
        step(0.05);
        let fwd_run = walk_mut(|w| w.key_move.fwd);
        assert_eq!(fwd_run, 400.0, "+forward runs at cl_forwardspeed 400 (Always Run default)");

        // Hold +speed (Shift, default.cfg): cl_movespeedkey doubles it.
        key_down(134); // K_SHIFT
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 800.0, "+speed doubles via cl_movespeedkey");
        key_up(134);

        // The player really moves (the server clamps wishspeed to sv_maxspeed
        // 320, so 400 is 320 effective — exactly WinQuake's run).
        let (x0, y0) = (listener().pos[0], listener().pos[1]);
        for _ in 0..20 {
            step(0.05);
        }
        let dist = ((listener().pos[0] - x0).powi(2) + (listener().pos[1] - y0).powi(2)).sqrt();
        assert!(dist > 100.0, "held +forward displaces the player (moved {dist:.1}u)");

        // Always Run (Options row 8) swaps cl_forwardspeed 400 -> 200.
        menu_cancel(); // open the menu
        menu_down();
        menu_down();
        menu_select(); // -> Options (Main cursor 2)
        for _ in 0..8 {
            menu_down(); // ROW_ALWAYSRUN (M_AdjustSliders case 8)
        }
        menu_right(); // toggle OFF
        menu_cancel(); // Options -> Main
        menu_cancel(); // Main -> closed
        assert_eq!(menu_visible(), 0);
        step(0.05);
        assert_eq!(
            walk_mut(|w| w.key_move.fwd),
            200.0,
            "toggling Always Run off drops the walk to 200"
        );

        // Releasing the key stops the contribution.
        key_up(i32::from(b'w'));
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 0.0, "key_up ends +forward");
    }

    #[test]
    fn invert_mouse_flips_pitch_and_lookspring_recentres_on_unlock() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Mouse pulled down (positive movementY) looks DOWN (positive pitch).
        walk_mut(|w| w.pitch = 0.0);
        mouse_move(0.0, 100.0);
        let p = player_pitch();
        assert!(p > 0.0, "non-inverted mouse-down looks down (pitch {p})");

        // Toggle Invert Mouse (Options row 9): the m_pitch sign flips.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..9 {
            menu_down(); // ROW_INVERTMOUSE (M_AdjustSliders case 9)
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        walk_mut(|w| w.pitch = 0.0);
        mouse_move(0.0, 100.0);
        let p = player_pitch();
        assert!(p < 0.0, "inverted mouse-down looks up (pitch {p})");

        // Lookspring OFF: pointer unlock leaves the pitch alone.
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        for _ in 0..10 {
            step(0.05);
        }
        assert_eq!(player_pitch(), -40.0, "no lookspring, no recentre");

        // Lookspring ON (row 10): unlock starts the V_StartPitchDrift recentre.
        // (Main reopens on "Options" and Options on Invert Mouse: menu.c
        // keeps m_main_cursor and options_cursor.)
        menu_cancel();
        menu_select();
        menu_down(); // ROW_LOOKSPRING (M_AdjustSliders case 10)
        menu_right();
        menu_cancel();
        menu_cancel();
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        for _ in 0..30 {
            step(0.05);
        }
        let p = player_pitch();
        assert!(p.abs() < 0.5, "lookspring recentred the view (pitch {p})");
        // ...and a mouse move stops an in-flight drift (V_StopPitchDrift).
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        mouse_move(0.0, 1.0);
        for _ in 0..10 {
            step(0.05);
        }
        assert!(player_pitch() < -30.0, "mlook motion stops the drift");
    }

    #[test]
    fn lookstrafe_routes_mouse_x_to_sidemove() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Default: mouse X turns (yaw changes, no sidemove accumulates).
        let yaw0 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert!(walk_mut(|w| w.yaw) < yaw0, "mouse-right turns right (yaw -= m_yaw*mx)");
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0);

        // Lookstrafe ON (Options row 11): mouse X strafes instead.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..11 {
            menu_down(); // ROW_LOOKSTRAFE (M_AdjustSliders case 11)
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        let yaw1 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert_eq!(walk_mut(|w| w.yaw), yaw1, "lookstrafe holds the yaw still");
        // sidemove += m_side * (mx * sensitivity 3) = 0.8 * 300 = 240.
        assert_eq!(walk_mut(|w| w.mouse_side), 240.0, "mouse X became sidemove units");
        // The accumulator drains into the next frame's cmd.
        step(0.05);
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0, "step drained the strafe units");
    }
}
