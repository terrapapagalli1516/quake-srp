//! Player input — cl_input.c (`CL_BaseMove`/`CL_AdjustAngles` and the `cl_*`
//! move cvars), in_win.c (`IN_MouseMove`) and the game-key half of keys.c
//! (`Key_Event` through the menu's binding table): the key/mouse exports the
//! page calls and the per-frame [`KeyMove`] the client frame consumes.

use quake_rs::render::{
    Menu, BIND_ATTACK, BIND_BACK, BIND_CENTERVIEW, BIND_CHANGEWEAPON, BIND_FORWARD, BIND_JUMP,
    BIND_LEFT, BIND_LOOKDOWN, BIND_LOOKUP, BIND_MOVEDOWN, BIND_MOVELEFT, BIND_MOVERIGHT,
    BIND_MOVEUP, BIND_RIGHT, BIND_SIZEDOWN, BIND_SIZEUP, BIND_SPEED, BIND_STRAFE,
};

use crate::{ensure_app, APP};

/// The legacy analog [`set_move`]/[`set_jump`]/[`set_movedown`] scale (`sv_maxspeed`):
/// those exports predate the bindings-driven key path and feed tests/automation;
/// the page's keyboard input goes through [`key_down`]/[`key_up`] and the
/// faithful `cl_*` move cvars below instead.
pub(crate) const SPEED: f32 = 320.0;

// --- client move cvars (cl_input.c registrations + CL_BaseMove/CL_AdjustAngles) ---
// The server clamps wishspeed to sv_maxspeed (320, ported in server.rs), so a
// 400 run is 320 effective on the ground — exactly WinQuake (walk 200, run 320).

/// `cl_forwardspeed`/`cl_backspeed` ("200"): the walking forward/back rate. The
/// Options "Always Run" toggle swaps them 200 <-> 400 (menu.c M_AdjustSliders
/// case 8); the C sets both cvars to the same value there, so one pair suffices.
/// Always Run defaults ON in this port (Menu's DEVIATION note), so the
/// out-of-the-box rate is the 400 run (320 effective under sv_maxspeed).
const CL_FORWARDSPEED_WALK: f32 = 200.0;
const CL_FORWARDSPEED_RUN: f32 = 400.0;
/// `cl_sidespeed` ("350"): the strafe rate — NOT changed by Always Run.
const CL_SIDESPEED: f32 = 350.0;
/// `cl_upspeed` ("200"): the swim up/down rate — NOT changed by Always Run.
const CL_UPSPEED: f32 = 200.0;
/// `cl_movespeedkey` ("2.0"): the `+speed` modifier multiplies every move.
const CL_MOVESPEEDKEY: f32 = 2.0;
/// `cl_yawspeed` ("140") / `cl_pitchspeed` ("150"): keyboard turn/look rates in
/// deg/sec (CL_AdjustAngles).
pub(crate) const CL_YAWSPEED: f32 = 140.0;
pub(crate) const CL_PITCHSPEED: f32 = 150.0;
/// `cl_anglespeedkey` ("1.5"): `+speed` multiplies the keyboard turn rate.
pub(crate) const CL_ANGLESPEEDKEY: f32 = 1.5;
/// `v_centerspeed` ("500", view.c): the pitch-drift rate centerview/lookspring
/// re-level the view at (V_StartPitchDrift seeds cl.pitchvel with it).
pub(crate) const V_CENTERSPEED: f32 = 500.0;

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

/// One frame of bindings-derived keyboard input, computed in `step` from the
/// held-key table + the menu's binding table and consumed by `step_walk` — a
/// port of `CL_BaseMove`/`CL_AdjustAngles` (cl_input.c) over this port's
/// permanently-held key states (`CL_KeyState`'s fractional first-frame impulse
/// timing needs sub-frame key timestamps the page doesn't deliver; held = 1.0).
#[derive(Clone, Copy, Default)]
pub(crate) struct KeyMove {
    /// `cmd->forwardmove` contribution (cl_forwardspeed/cl_backspeed applied,
    /// including the Always-Run swap and `cl_movespeedkey`).
    pub(crate) fwd: f32,
    /// `cmd->sidemove` contribution (cl_sidespeed; +strafe folds the turn keys in).
    pub(crate) side: f32,
    /// `cmd->upmove` contribution (cl_upspeed).
    pub(crate) up: f32,
    /// `+attack` held.
    pub(crate) attack: bool,
    /// `+jump` held.
    pub(crate) jump: bool,
    /// Keyboard turn direction (+1 = `+left`, -1 = `+right`; 0 with `+strafe`
    /// held — CL_AdjustAngles skips the yaw turn then).
    pub(crate) turn: f32,
    /// Keyboard look direction (+1 = `+lookup`, -1 = `+lookdown`).
    pub(crate) look: f32,
    /// `+speed` held (cl_movespeedkey / cl_anglespeedkey modifiers).
    pub(crate) speed: bool,
}

/// Derive this frame's [`KeyMove`] from the page-held keys through the menu's
/// binding table (keys.c `keybindings` consulted by `Key_Event`; move math per
/// `CL_BaseMove` + `CL_AdjustAngles`).
pub(crate) fn derive_key_move(menu: &Menu, held: &[bool; 256]) -> KeyMove {
    // CL_KeyState: 1.0 while any key bound to `cmd` is held.
    let st = |cmd: usize| -> f32 {
        for (k, &h) in held.iter().enumerate() {
            if h && menu.action_for_key(k as u8) == Some(cmd) {
                return 1.0;
            }
        }
        0.0
    };
    let speed = st(BIND_SPEED) > 0.0;
    let strafe = st(BIND_STRAFE) > 0.0;
    // M_AdjustSliders case 8 ("always run") sets cl_forwardspeed AND
    // cl_backspeed together, so one value serves both directions.
    let fwdspeed = if menu.always_run() {
        CL_FORWARDSPEED_RUN
    } else {
        CL_FORWARDSPEED_WALK
    };
    let mut fwd = fwdspeed * st(BIND_FORWARD) - fwdspeed * st(BIND_BACK);
    let mut side = CL_SIDESPEED * (st(BIND_MOVERIGHT) - st(BIND_MOVELEFT));
    if strafe {
        // CL_BaseMove: with +strafe held the turn keys strafe instead.
        side += CL_SIDESPEED * (st(BIND_RIGHT) - st(BIND_LEFT));
    }
    // +moveup or +jump push up (the port has always let Space double as swim-up
    // in water; on land the ground move ignores upmove and +jump still jumps
    // via button2), +movedown sinks — at cl_upspeed, NOT the run speed.
    let jump = st(BIND_JUMP) > 0.0;
    let mut up = CL_UPSPEED * (st(BIND_MOVEUP).max(st(BIND_JUMP)) - st(BIND_MOVEDOWN));
    if speed {
        // CL_BaseMove: the speed key multiplies forward/side/up by
        // cl_movespeedkey.
        fwd *= CL_MOVESPEEDKEY;
        side *= CL_MOVESPEEDKEY;
        up *= CL_MOVESPEEDKEY;
    }
    KeyMove {
        fwd,
        side,
        up,
        attack: st(BIND_ATTACK) > 0.0,
        jump,
        turn: if strafe { 0.0 } else { st(BIND_LEFT) - st(BIND_RIGHT) },
        look: st(BIND_LOOKUP) - st(BIND_LOOKDOWN),
        speed,
    }
}

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

/// Clamp the view pitch the way `CL_AdjustAngles` (cl_input.c) does: pitch is
/// limited to `[-70, 80]`. In this codebase positive pitch = looking down
/// (UserCmd pitch is QuakeC's +down convention), so +80 is the further-down
/// bound and -70 the looking-up bound — an asymmetry matching Quake's feel.
pub(crate) fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(-70.0, 80.0)
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
    use crate::test_util::*;
    use crate::menu::{menu_cancel, menu_down, menu_right, menu_select, menu_visible};
    use crate::snd_dma::{listener_x, listener_y};
    use crate::{boot, step};

    #[test]
    fn pitch_clamp_is_asymmetric_like_cl_adjustangles() {
        // CL_AdjustAngles clamps pitch to [-70, 80]; positive pitch = down.
        assert_eq!(clamp_pitch(0.0), 0.0, "neutral pitch is unchanged");
        // Looking far down is allowed up to +80, not +70.
        assert_eq!(clamp_pitch(200.0), 80.0, "down clamps at +80");
        assert_eq!(clamp_pitch(75.0), 75.0, "75 down is within the +80 bound");
        assert_eq!(clamp_pitch(80.0), 80.0, "exactly +80 is allowed");
        // Looking up is limited to -70.
        assert_eq!(clamp_pitch(-200.0), -70.0, "up clamps at -70");
        assert_eq!(clamp_pitch(-70.0), -70.0, "exactly -70 is allowed");
        // The asymmetry: +75 survives but -75 is clamped to -70.
        assert!(
            clamp_pitch(75.0) > 70.0 && clamp_pitch(-75.0) == -70.0,
            "down range exceeds 70 while up range does not"
        );
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
