//! Player input — in_win.c (`IN_MouseMove`) and the game-key half of keys.c
//! (`Key_Event` through the menu's binding table): the key/mouse exports the
//! page calls. The per-frame `KeyMove` they feed (cl_input.c's `CL_BaseMove`/
//! `CL_AdjustAngles`) is [`quake_rs::client::cl_input`]'s.

use quake_rs::client::cl_input::{clamp_pitch, V_CENTERSPEED};
use quake_rs::menu::BIND_IMPULSE_0;
use quake_rs::render::{BIND_CENTERVIEW, BIND_CHANGEWEAPON, BIND_SIZEDOWN, BIND_SIZEUP, BIND_STRAFE};

use crate::app::{ensure_app, APP};

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

#[no_mangle]
pub extern "C" fn set_move(fwd: f32, side: f32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_fwd = fwd;
            w.in_side = side;
        }
    });
}

/// Set whether the attack button is held (drives the QuakeC weapon code).
#[no_mangle]
pub extern "C" fn set_attack(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_attack = on != 0;
        }
    });
}

/// Set whether the jump key is held. Maps to UserCmd button bit 1 -> the
/// player's `button2`, which the QuakeC PlayerJump reads to jump when on the
/// ground (velocity_z = 270).
#[no_mangle]
pub extern "C" fn set_jump(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_jump = on != 0;
        }
    });
}

/// Set whether the swim-DOWN key (`c`, the `+movedown` key) is held. Maps to a
/// negative `UserCmd.upmove`, which `SV_WaterMove` reads to sink while waist-deep
/// in water. Out of water it has no effect (the walk move ignores upmove).
#[no_mangle]
pub extern "C" fn set_movedown(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_down = on != 0;
        }
    });
}

/// Queue a one-shot impulse for the next frame (e.g. weapon select: 1 = axe,
/// 2 = shotgun, 3 = super shotgun, 4 = nailgun, ... — exactly the QuakeC
/// `impulse` numbers). Applied to the next `step_walk` UserCmd then cleared.
#[no_mangle]
pub extern "C" fn set_impulse(n: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.next_impulse = n;
        }
    });
}

// --- bindings-driven game keys (keys.c Key_Event -> keybindings consult) -----

/// A game key went down, by Quake keynum (keys.h: printable ASCII is itself
/// lowercase; arrows/modifiers take the 128+ block; mouse buttons 200+). The
/// held state feeds the per-frame `CL_BaseMove` derivation through the menu's
/// binding table; the non-`+` commands (`impulse 10`, `centerview`) fire their
/// one-shot here like `Key_Event`'s command dispatch. The page must not route
/// keys here while the menu/console own the keyboard (`key_dest != key_game`) —
/// and the engine gates the one-shots regardless.
#[no_mangle]
pub extern "C" fn key_down(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        a.keys_held[keynum as usize] = true;
        if a.menu.visible || a.console.open {
            return; // key_dest != key_game: no command dispatch.
        }
        match a.menu.action_for_key(keynum as u8) {
            // "impulse N" (IN_Impulse: `in_impulse = atoi(argv[1])`), sent with
            // the next move.
            Some(c) if (BIND_IMPULSE_0..=BIND_IMPULSE_0 + 8).contains(&c) => {
                if let Some(w) = a.walk.as_mut() {
                    w.next_impulse = (c - BIND_IMPULSE_0) as i32;
                }
            }
            Some(BIND_CHANGEWEAPON) => {
                // "impulse 10": queue the next-weapon impulse once, like the
                // console command (Cbuf -> IN_Impulse).
                if let Some(w) = a.walk.as_mut() {
                    w.next_impulse = 10;
                }
            }
            Some(BIND_CENTERVIEW) => {
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
            Some(BIND_SIZEUP) => a.menu.size_up(),
            Some(BIND_SIZEDOWN) => a.menu.size_down(),
            _ => {}
        }
    });
}

/// A game key went up, by Quake keynum. Always honoured — even while the
/// menu/console are up — so a key released behind an overlay can never stick
/// held (Key_Event delivers key-ups to `+` commands regardless of key_dest).
#[no_mangle]
pub extern "C" fn key_up(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        a.keys_held[keynum as usize] = false;
    });
}

/// 1 when the engine currently believes Quake keynum `keynum` is held — a
/// read-only verification/debug export (like [`menu_screen_id`]). The browser
/// harness uses it to prove the page's `e.code` punctuation mapping keeps
/// key-down/key-up SYMMETRIC under Shift (press ',', add Shift, release ','
/// must clear keynum 44, even though the release reports `key == '<'` —
/// the C's scancode semantics, in_win.c `scantokey`).
#[no_mangle]
pub extern "C" fn key_is_down(keynum: i32) -> i32 {
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
#[no_mangle]
pub extern "C" fn mouse_move(dx: f32, dy: f32) {
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
#[no_mangle]
pub extern "C" fn pointer_unlocked() {
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
#[no_mangle]
pub extern "C" fn player_pitch() -> f32 {
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
#[no_mangle]
pub extern "C" fn mouse_sensitivity() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.mouse_sensitivity())
            .unwrap_or(1.0)
    })
}

#[no_mangle]
pub extern "C" fn look(dyaw: f32, dpitch: f32) {
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
    use crate::menu::{menu_cancel, menu_down, menu_right, menu_select, menu_visible};
    use crate::snd_dma::{listener_x, listener_y};
    use crate::test_util::*;

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
        let (x0, y0) = (listener_x(), listener_y());
        for _ in 0..20 {
            step(0.05);
        }
        let dist = ((listener_x() - x0).powi(2) + (listener_y() - y0).powi(2)).sqrt();
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
        menu_cancel();
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..10 {
            menu_down(); // ROW_LOOKSPRING (M_AdjustSliders case 10)
        }
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
