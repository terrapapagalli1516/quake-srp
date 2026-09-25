//! Player input — cl_input.c's `CL_BaseMove`/`CL_AdjustAngles` and the
//! `cl_*` move cvars: the per-frame [`KeyMove`] the live client frame
//! consumes, derived from the held keys through the binding table (the
//! game-key half of keys.c's `Key_Event`). The platform feeds it: which keys
//! are held, and the mouse (in_win.c's `IN_MouseMove`).
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_input.c`.

use crate::menu::BIND_SHOWSCORES;
use crate::render::{
    Menu, BIND_ATTACK, BIND_BACK, BIND_FORWARD, BIND_JUMP, BIND_LEFT, BIND_LOOKDOWN, BIND_LOOKUP,
    BIND_MOVEDOWN, BIND_MOVELEFT, BIND_MOVERIGHT, BIND_MOVEUP, BIND_RIGHT, BIND_SPEED, BIND_STRAFE,
};

/// The legacy analog `set_move`/`set_jump`/`set_movedown` scale (`sv_maxspeed`):
/// those exports predate the bindings-driven key path and feed tests/automation;
/// the page's keyboard input goes through `key_down`/`key_up` and the
/// faithful `cl_*` move cvars below instead.
pub const SPEED: f32 = 320.0;

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
pub const CL_YAWSPEED: f32 = 140.0;
pub const CL_PITCHSPEED: f32 = 150.0;
/// `cl_anglespeedkey` ("1.5"): `+speed` multiplies the keyboard turn rate.
pub const CL_ANGLESPEEDKEY: f32 = 1.5;
/// `v_centerspeed` ("500", view.c): the pitch-drift rate centerview/lookspring
/// re-level the view at (V_StartPitchDrift seeds cl.pitchvel with it).
pub const V_CENTERSPEED: f32 = 500.0;

/// One frame of bindings-derived keyboard input, computed in `step` from the
/// held-key table + the menu's binding table and consumed by `step_walk` — a
/// port of `CL_BaseMove`/`CL_AdjustAngles` (cl_input.c) over this port's
/// permanently-held key states (`CL_KeyState`'s fractional first-frame impulse
/// timing needs sub-frame key timestamps the page doesn't deliver; held = 1.0).
#[derive(Clone, Copy, Default)]
pub struct KeyMove {
    /// `cmd->forwardmove` contribution (cl_forwardspeed/cl_backspeed applied,
    /// including the Always-Run swap and `cl_movespeedkey`).
    pub fwd: f32,
    /// `cmd->sidemove` contribution (cl_sidespeed; +strafe folds the turn keys in).
    pub side: f32,
    /// `cmd->upmove` contribution (cl_upspeed).
    pub up: f32,
    /// `+attack` held.
    pub attack: bool,
    /// `+jump` held.
    pub jump: bool,
    /// Keyboard turn direction (+1 = `+left`, -1 = `+right`; 0 with `+strafe`
    /// held — CL_AdjustAngles skips the yaw turn then).
    pub turn: f32,
    /// Keyboard look direction (+1 = `+lookup`, -1 = `+lookdown`).
    pub look: f32,
    /// `+speed` held (cl_movespeedkey / cl_anglespeedkey modifiers).
    pub speed: bool,
    /// `+showscores` held (`sb_showscores`): the status bar shows the solo
    /// scoreboard instead of the stats.
    pub showscores: bool,
}

/// Derive this frame's [`KeyMove`] from the page-held keys through the menu's
/// binding table (keys.c `keybindings` consulted by `Key_Event`; move math per
/// `CL_BaseMove` + `CL_AdjustAngles`).
pub fn derive_key_move(menu: &Menu, held: &[bool; 256]) -> KeyMove {
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
        showscores: st(BIND_SHOWSCORES) > 0.0,
    }
}

/// Clamp the view pitch the way `CL_AdjustAngles` (cl_input.c) does: pitch is
/// limited to `[-70, 80]`. In this codebase positive pitch = looking down
/// (UserCmd pitch is QuakeC's +down convention), so +80 is the further-down
/// bound and -70 the looking-up bound — an asymmetry matching Quake's feel.
pub fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(-70.0, 80.0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
