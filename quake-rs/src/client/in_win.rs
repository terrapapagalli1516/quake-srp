//! The joystick — the joystick half of in_win.c: `IN_StartupJoystick`,
//! `Joy_AdvancedUpdate_f`, `IN_ReadJoystick`, `IN_Commands` and `IN_JoyMove`,
//! reading a pad the platform hands it ([`Pad`]: the browser's Gamepad API)
//! the way WinQuake read winmm's first joystick. And the port's additions for
//! a slop pad, each a setting in [`JoyCvars`]: a round dead zone and a
//! response curve for the sticks, the pad's buttons in the menus, and
//! rumble. The gamepad is a *control* ([`crate::settings`]'s module docs):
//! on by default in both presets, off only with id's own 1996 controls
//! ([`JoyCvars::classic`], [`crate::cvar::Cvars::with_id_controls`]).
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/in_win.c`. Its mouse half (`IN_MouseMove`) is the
//! platform's: quake-wasm's `input.rs`.
//!
//! ## A pad as winmm showed it
//!
//! `joyGetPosEx` gives six axes, X Y Z R U V (0..65535, centred at 32768),
//! up to 32 buttons and a POV hat. A pad with the Gamepad API's "standard"
//! mapping (any current pad) reads here as an Xbox 360 pad read through winmm
//! on Windows, the most common pad of WinQuake's later years:
//!
//! | winmm | standard pad |
//! |---|---|
//! | X, Y | left stick (Y down, as winmm's) |
//! | Z | the triggers: left raises it, right lowers it |
//! | R | right stick, vertical |
//! | U | right stick, horizontal |
//! | V | — |
//! | button `i + 1` | button `i`: A B X Y, LB RB, LT RT, Back Start, L3 R3, then Guide (16) |
//! | POV hat | the D-pad (buttons 12..15) |
//!
//! So `IN_Commands` keys A, B, X, Y as `JOY1`..`JOY4` and the rest from
//! `AUX5` (LB `AUX5`, RB `AUX6`, LT `AUX7`, RT `AUX8`, Back `AUX9`, Start
//! `AUX10`, L3 `AUX11`, R3 `AUX12`, Guide `AUX17`), and the D-pad as the
//! hat's `AUX29`..`AUX32` (up, right, down, left). With id's `joyadvanced 0`
//! X turns and Y walks: the left stick drives, as a 1996 joystick did. (The
//! 360 pad's own winmm driver had ten buttons, Back and Start 7 and 8, and
//! its triggers only on Z; here the triggers are buttons too, in the Gamepad
//! API's order, so they can be bound.) A pad without the standard mapping
//! reads its first six axes as X..V and its buttons in order, with no hat.
//!
//! ## Where the port departs
//!
//! - `IN_Commands` reads the pad itself. id's keys the buttons that
//!   `IN_JoyMove` read the frame before, a host frame later; the port keys
//!   the newest state, and with `joystick 0` (id's stale read) nothing.
//! - With the pad gone or `joystick` switched off, every held pad key comes
//!   up (id's kept the last read, so a held button stayed held).
//! - A diagonal on the D-pad is a diagonal on the hat, which id's
//!   `IN_Commands` does not key (it compares `dwPOV` with the four straight
//!   directions); that is kept.

use crate::cvar::Cvars;
use crate::keys::{
    K_AUX1, K_AUX29, K_AUX32, K_DOWNARROW, K_ENTER, K_ESCAPE, K_JOY1, K_LEFTARROW, K_RIGHTARROW, K_UPARROW,
};
use crate::settings::Preset;

use super::cl_input::{CL_MOVESPEEDKEY, CL_PITCHSPEED, CL_SIDESPEED, CL_YAWSPEED};

/// `JOY_MAX_AXES`: X, Y, Z, R, U, V.
pub const JOY_MAX_AXES: usize = 6;

/// The joystick's settings: in_win.c's `joy*` cvars and the port's `joy_*`.
///
/// id's are the `joystick` switch and the "advanced controller
/// configuration": `joyadvanced`, which axis drives what (`joyadvaxis*`), and
/// each control's threshold and sensitivity. The presets' twin-stick layout
/// (the controls are shared: both have it) is id's own advanced
/// configuration ([`JoyCvars::twin_stick`]), plus the port's stick shaping. (in_win.c archives none of them but `joystick`:
/// "advanced controller configuration needs to be executed each time". The
/// port keeps them in `config.cfg` like every setting a preset sets, so a
/// player's changes to the layout last.)
#[derive(Debug, Clone, PartialEq)]
pub struct JoyCvars {
    /// `joystick` (`in_joystick`): read the joystick at all. id's default
    /// is off; on in slop.
    pub enabled: bool,
    /// `joyname`: the controller's name, which `Joy_AdvancedUpdate_f` prints
    /// ("%s configured") when it is not "joystick".
    pub name: String,
    /// `joyadvanced`: the axis maps come from `joyadvaxis*`; off, X turns and
    /// Y walks (or looks, with `+mlook`).
    pub advanced: bool,
    /// `joyadvaxisx`..`joyadvaxisv`: what each axis drives — 0 nothing, 1
    /// forward, 2 look, 3 side, 4 turn — plus 16 for a relative axis (a
    /// spinner or trackball: its value is a movement, not a position).
    pub advaxis: [f32; JOY_MAX_AXES],
    /// `joyforwardthreshold`, `joysidethreshold`, `joypitchthreshold`,
    /// `joyyawthreshold`: an axis below it does nothing (id's: 0.15).
    pub forward_threshold: f32,
    pub side_threshold: f32,
    pub pitch_threshold: f32,
    pub yaw_threshold: f32,
    /// `joyforwardsensitivity` (-1), `joysidesensitivity` (-1),
    /// `joypitchsensitivity` (1), `joyyawsensitivity` (-1): each control's
    /// scale, its sign the direction.
    pub forward_sensitivity: f32,
    pub side_sensitivity: f32,
    pub pitch_sensitivity: f32,
    pub yaw_sensitivity: f32,
    /// `joywwhack1`, `joywwhack2`: the Logitech WingMan Warrior's fixes (its
    /// U axis centred 100 low; its spinner's turn curve).
    pub wwhack1: f32,
    pub wwhack2: f32,
    /// `joy_deadzone` (the port's): each stick's dead zone as a circle, and
    /// what is past it rescaled from 0, where id's thresholds cut each axis
    /// alone and jump from 0 to the threshold. 0 (Classic): off.
    pub deadzone: f32,
    /// `joy_exponent` (the port's): the look stick's response curve, its
    /// deflection raised to this power (2: fine aim near the centre, full
    /// speed at the edge). 1 (Classic): id's straight line.
    pub exponent: f32,
    /// `joy_menukeys` (the port's): in the menus the pad's A, B, Start and
    /// D-pad are Enter, Escape, Escape and the arrows, as menu.c knows only
    /// the keyboard's keys.
    pub menu_keys: bool,
    /// `joy_rumble` (the port's): the pad's rumble on damage and on firing a
    /// heavy weapon, at this strength (0: none).
    pub rumble: f32,
}

impl JoyCvars {
    /// id's defaults (in_win.c's registrations), the port's additions off.
    pub fn classic() -> JoyCvars {
        JoyCvars {
            enabled: false,
            name: "joystick".to_string(),
            advanced: false,
            advaxis: [0.0; JOY_MAX_AXES],
            forward_threshold: 0.15,
            side_threshold: 0.15,
            pitch_threshold: 0.15,
            yaw_threshold: 0.15,
            forward_sensitivity: -1.0,
            side_sensitivity: -1.0,
            pitch_sensitivity: 1.0,
            yaw_sensitivity: -1.0,
            wwhack1: 0.0,
            wwhack2: 0.0,
            deadzone: 0.0,
            exponent: 1.0,
            menu_keys: false,
            rumble: 0.0,
        }
    }

    /// The presets' pad: a modern twin-stick layout as id's advanced
    /// configuration — the left stick (X, Y) walks and strafes, the right
    /// stick (U, R) turns and looks — with no thresholds (the port's round
    /// dead zone does their work), the strafe the right way round for a side
    /// axis (id's -1 suits a turn axis strafing with `+strafe`), a 245°/s
    /// turn at full tilt (id's 140), the look curve, menu keys and rumble.
    pub fn twin_stick() -> JoyCvars {
        JoyCvars {
            enabled: true,
            advanced: true,
            advaxis: [
                f32::from(AxisControl::Side as u8),
                f32::from(AxisControl::Forward as u8),
                0.0,
                f32::from(AxisControl::Look as u8),
                f32::from(AxisControl::Turn as u8),
                0.0,
            ],
            forward_threshold: 0.0,
            side_threshold: 0.0,
            pitch_threshold: 0.0,
            yaw_threshold: 0.0,
            side_sensitivity: 1.0,
            yaw_sensitivity: -1.75,
            deadzone: 0.2,
            exponent: 2.0,
            menu_keys: true,
            rumble: 1.0,
            ..JoyCvars::classic()
        }
    }
}

// ---------------------------------------------------------------------------
// The pad and what winmm would have said of it
// ---------------------------------------------------------------------------

/// A pad as the platform reads it (the page's `GAMEPAD` record: the fields of
/// the Gamepad API's `Gamepad`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pad {
    /// `mapping == "standard"`: the W3C layout the module's table reads.
    pub standard: bool,
    /// `buttons.length`, at most 32.
    pub num_buttons: u8,
    /// `buttons[i].pressed`, as bit `i`.
    pub pressed: u32,
    /// `axes[0..4]` (-1..1; x right, y down), then for a standard pad the
    /// triggers' `buttons[6].value` and `buttons[7].value` (0..1), for
    /// another `axes[4..6]`.
    pub axes: [f32; JOY_MAX_AXES],
}

/// The standard mapping's D-pad buttons.
const DPAD_UP: u32 = 12;
const DPAD_DOWN: u32 = 13;
const DPAD_LEFT: u32 = 14;
const DPAD_RIGHT: u32 = 15;
const DPAD_BITS: u32 = (1 << DPAD_UP) | (1 << DPAD_DOWN) | (1 << DPAD_LEFT) | (1 << DPAD_RIGHT);

/// `JOYINFOEX` as `joyGetPosEx` fills it: each axis centred
/// (`dwXpos - 32768`: -32768..32767), the buttons, the hat.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct JoyInfo {
    axes: [f32; JOY_MAX_AXES],
    buttons: u32,
    /// `dwPOV` in hundredths of a degree clockwise from forward; `None` is
    /// `JOY_POVCENTERED`.
    pov: Option<u16>,
}

impl JoyInfo {
    /// What winmm would report for `pad` (the module's table).
    fn from_pad(pad: &Pad) -> JoyInfo {
        let a = pad.axes;
        if !pad.standard {
            return JoyInfo { axes: a.map(raw_axis), buttons: pad.pressed, pov: None };
        }
        JoyInfo {
            axes: [a[0], a[1], a[4] - a[5], a[3], a[2], 0.0].map(raw_axis),
            buttons: pad.pressed & !DPAD_BITS,
            pov: dpad_pov(pad.pressed),
        }
    }
}

/// A Gamepad API axis (-1..1) as winmm's centred reading: whole steps of
/// 1/32768, -32768..32767.
fn raw_axis(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    (v * 32768.0).round().clamp(-32768.0, 32767.0)
}

/// The D-pad as a POV hat: one of the eight directions, or centred (no
/// direction, or two opposite ones).
fn dpad_pov(pressed: u32) -> Option<u16> {
    let held = |b: u32| i32::from(pressed & (1 << b) != 0);
    let x = held(DPAD_RIGHT) - held(DPAD_LEFT);
    let y = held(DPAD_UP) - held(DPAD_DOWN);
    match (x, y) {
        (0, 1) => Some(0),
        (1, 1) => Some(4500),
        (1, 0) => Some(9000),
        (1, -1) => Some(13500),
        (0, -1) => Some(18000),
        (-1, -1) => Some(22500),
        (-1, 0) => Some(27000),
        (-1, 1) => Some(31500),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The joystick
// ---------------------------------------------------------------------------

/// `_ControlList`: what an axis drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AxisControl {
    /// `AxisNada`.
    #[default]
    Nada = 0,
    /// `AxisForward`: walk (with `joyadvanced 0` and `+mlook`, look).
    Forward = 1,
    /// `AxisLook`: pitch, in mouse look.
    Look = 2,
    /// `AxisSide`: strafe.
    Side = 3,
    /// `AxisTurn`: yaw (strafe with `+strafe`, or `lookstrafe` in mouse look).
    Turn = 4,
}

impl AxisControl {
    /// `dwAxisMap[i] = dwTemp & 0x0000000f`: any other value drives nothing
    /// (the switch has no case for it).
    fn from_bits(bits: u32) -> AxisControl {
        match bits & 0x0f {
            1 => AxisControl::Forward,
            2 => AxisControl::Look,
            3 => AxisControl::Side,
            4 => AxisControl::Turn,
            _ => AxisControl::Nada,
        }
    }
}

/// `JOY_RELATIVE_AXIS`: the axis is a movement (spinner, trackball).
const JOY_RELATIVE_AXIS: u32 = 0x10;

/// The pad keys, `K_JOY1`..`K_AUX32`.
const NUM_PAD_KEYS: usize = (K_AUX32 - K_JOY1) as usize + 1;

/// Where the pad's keys go this frame, for the port's `joy_menukeys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadKeys {
    /// To `Key_Event` as themselves: the game, the console, a key being
    /// bound on Customize controls.
    Game,
    /// The menu has the keyboard.
    Menu,
    /// A yes/no prompt has it (Quit, New Game's "Are you sure?"), which
    /// answers only `y`, `n` and Escape.
    YesNo,
}

/// The keys `CL_BaseMove` holds that `IN_JoyMove` reads: `in_speed`,
/// `in_strafe` and `in_mlook` (with the slop `freelook`, mouse look is on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Held {
    pub speed: bool,
    pub strafe: bool,
    pub mlook: bool,
}

/// What `IN_JoyMove` did to the frame's move and view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct JoyMove {
    /// Added to `cmd->forwardmove` and `cmd->sidemove`.
    pub forward: f32,
    pub side: f32,
    /// Added to `cl.viewangles[YAW]` and `[PITCH]`; the caller bounds the
    /// pitch (`IN_JoyMove`'s last lines, [`super::cl_input::clamp_pitch`]).
    pub yaw: f32,
    pub pitch: f32,
    /// `V_StopPitchDrift ()` ran.
    pub stop_drift: bool,
}

/// in_win.c's joystick statics, and the platform's newest reading.
#[derive(Debug, Clone)]
pub struct Joystick {
    /// `-nojoy`: `IN_StartupJoystick` gave up, nothing is ever read.
    nojoy: bool,
    /// The platform's newest reading; `None`: no pad (or it went away).
    pad: Option<Pad>,
    /// `joy_avail`: a pad has been seen.
    avail: bool,
    /// `joy_numbuttons` and `joy_haspov` (`joyGetDevCaps`): of the last pad
    /// seen, so a pad that goes away still has its keys let go.
    num_buttons: u32,
    has_pov: bool,
    /// `joy_advancedinit`, as the preset the axis maps were made under: a
    /// preset makes them again, as `joyadvancedupdate` would.
    advanced_init: Option<Preset>,
    /// `dwAxisMap`, and `dwControlMap` (true: `JOY_RELATIVE_AXIS`).
    axis_map: [AxisControl; JOY_MAX_AXES],
    relative: [bool; JOY_MAX_AXES],
    /// `joy_oldbuttonstate`, `joy_oldpovstate`.
    old_buttons: u32,
    old_pov: u32,
    /// The key each held pad key went to `Key_Event` as (itself, or its
    /// menu key), so its release goes to the same key.
    sent_as: [u8; NUM_PAD_KEYS],
    /// `Con_Printf`s since the platform last took them.
    prints: Vec<String>,
}

impl Default for Joystick {
    fn default() -> Self {
        Joystick {
            nojoy: false,
            pad: None,
            avail: false,
            num_buttons: 0,
            has_pov: false,
            advanced_init: None,
            axis_map: [AxisControl::Nada; JOY_MAX_AXES],
            relative: [false; JOY_MAX_AXES],
            old_buttons: 0,
            old_pov: 0,
            sent_as: std::array::from_fn(|i| K_JOY1 + i as u8),
            prints: Vec::new(),
        }
    }
}

impl Joystick {
    /// `IN_StartupJoystick`'s `-nojoy`: never read a pad.
    pub fn set_nojoy(&mut self) {
        self.nojoy = true;
    }

    /// The platform's newest reading of the pad (`None`: none, or it went
    /// away). The first pad seen is `IN_StartupJoystick`'s detection, which
    /// prints "joystick detected" — when the pad first shows itself, since a
    /// browser shows none before a button is pressed.
    pub fn set_pad(&mut self, pad: Option<Pad>) {
        if self.nojoy {
            return;
        }
        if let Some(p) = &pad {
            if !self.avail {
                self.avail = true;
                self.prints.push("\njoystick detected\n\n".to_string());
            }
            self.num_buttons = u32::from(p.num_buttons).min(32);
            self.has_pov = p.standard;
        }
        self.pad = pad;
    }

    /// What `Con_Printf` said since the last call.
    pub fn take_prints(&mut self) -> Vec<String> {
        std::mem::take(&mut self.prints)
    }

    /// A pad is there and `joystick` is on: what the rumble needs.
    pub fn active(&self, cv: &JoyCvars) -> bool {
        self.read(cv).is_some()
    }

    /// `Joy_AdvancedUpdate_f` (`joyadvancedupdate`): make the axis maps from
    /// the cvars — X turn and Y forward, or with `joyadvanced` each axis's
    /// `joyadvaxis*`.
    pub fn advanced_update(&mut self, cv: &JoyCvars) {
        self.axis_map = [AxisControl::Nada; JOY_MAX_AXES];
        self.relative = [false; JOY_MAX_AXES];
        if !cv.advanced {
            self.axis_map[0] = AxisControl::Turn;
            self.axis_map[1] = AxisControl::Forward;
            return;
        }
        if cv.name != "joystick" {
            self.prints.push(format!("\n{} configured\n\n", cv.name));
        }
        for (i, &v) in cv.advaxis.iter().enumerate() {
            // `dwTemp = (DWORD) joy_advaxisx.value`.
            let bits = if v.is_finite() && v > 0.0 { v as u32 } else { 0 };
            self.axis_map[i] = AxisControl::from_bits(bits);
            self.relative[i] = bits & JOY_RELATIVE_AXIS != 0;
        }
    }

    /// `IN_ReadJoystick`, where `IN_JoyMove` calls it: the reading, or
    /// `None` when there is none to take — no pad, `-nojoy`, or `joystick`
    /// off. (`joywwhack1`: the WingMan Warrior's U axis sits 100 low.)
    fn read(&self, cv: &JoyCvars) -> Option<JoyInfo> {
        if !self.avail || !cv.enabled {
            return None;
        }
        let mut ji = JoyInfo::from_pad(self.pad.as_ref()?);
        if cv.wwhack1 != 0.0 {
            ji.axes[4] += 100.0;
        }
        Some(ji)
    }

    /// `IN_Commands`: a key event for each button and hat direction that
    /// changed since the last call — button `i` is `K_JOY1 + i` for the first
    /// four and `K_AUX1 + i` after, the hat's forward, right, back and left
    /// `K_AUX29`..`K_AUX32` — in `(keynum, down)` pairs for `Key_Event`,
    /// in id's order. With `joy_menukeys`, a key that goes down while `dest`
    /// is a menu is its menu key ([`menu_key`]), and comes up as that key.
    pub fn commands(&mut self, cv: &JoyCvars, dest: PadKeys) -> Vec<(u8, bool)> {
        let ji = self.read(cv).unwrap_or_default();
        let dest = if cv.menu_keys { dest } else { PadKeys::Game };
        let mut events = Vec::new();
        for i in 0..self.num_buttons {
            let (now, was) = (ji.buttons & (1 << i) != 0, self.old_buttons & (1 << i) != 0);
            if now != was {
                let key = if i < 4 { K_JOY1 + i as u8 } else { K_AUX1 + i as u8 };
                events.push(self.pad_key(key, now, dest));
            }
        }
        self.old_buttons = ji.buttons;
        if self.has_pov {
            // Only the four straight directions: `dwPOV == JOY_POVFORWARD` ...
            let povstate = match ji.pov {
                Some(0) => 1,
                Some(9000) => 2,
                Some(18000) => 4,
                Some(27000) => 8,
                _ => 0,
            };
            for i in 0..4u8 {
                let (now, was) = (povstate & (1 << i) != 0, self.old_pov & (1 << i) != 0);
                if now != was {
                    events.push(self.pad_key(K_AUX29 + i, now, dest));
                }
            }
            self.old_pov = povstate;
        }
        events
    }

    /// One pad key's event as it reaches `Key_Event`: a key down as itself or
    /// its menu key, remembered; a key up as whatever its down was.
    fn pad_key(&mut self, key: u8, down: bool, dest: PadKeys) -> (u8, bool) {
        let slot = usize::from(key - K_JOY1);
        if down {
            self.sent_as[slot] = menu_key(key, dest).unwrap_or(key);
            (self.sent_as[slot], true)
        } else {
            (std::mem::replace(&mut self.sent_as[slot], key), false)
        }
    }

    /// `IN_JoyMove`: the frame's joystick move and turn, for `frametime`
    /// (`host_frametime`) seconds with the keys `held`. The axis maps are
    /// made on the first call (`joy_advancedinit`), and again after another
    /// `preset` is applied. Nothing without a reading (`Joystick::read`).
    pub fn joy_move(&mut self, cvars: &Cvars, preset: Preset, held: Held, frametime: f32) -> JoyMove {
        let cv = &cvars.joy;
        if self.advanced_init != Some(preset) {
            self.advanced_update(cv);
            self.advanced_init = Some(preset);
        }
        let Some(ji) = self.read(cv) else { return JoyMove::default() };
        let speed = if held.speed { CL_MOVESPEEDKEY } else { 1.0 };
        let mut values = [0.0; JOY_MAX_AXES];
        for (i, v) in values.iter_mut().enumerate() {
            *v = self.axis_value(cv, i, ji.axes[i]);
        }
        shape_sticks(&mut values, &self.axis_map, &self.relative, cv);
        let ctx = MoveContext { cvars, held, speed, aspeed: speed * frametime };
        let mut m = JoyMove::default();
        for (i, &v) in values.iter().enumerate() {
            ctx.axis(self.axis_map[i], self.relative[i], v, &mut m);
        }
        m
    }

    /// The loop's first half for axis `i`: the centred reading (with
    /// `joywwhack2`'s WingMan spinner curve on a turn axis) as -1..1.
    fn axis_value(&self, cv: &JoyCvars, i: usize, raw: f32) -> f32 {
        let mut v = raw;
        if cv.wwhack2 != 0.0 && self.axis_map[i] == AxisControl::Turn {
            // y = 300 x^1.3 over x in steps of 800, bounded; `abs` is C's
            // integer abs of the float.
            let x = f64::from((v as i32).unsigned_abs()) / 800.0;
            let t = (300.0 * x.powf(1.3)).min(14000.0) as f32;
            v = if v > 0.0 { t } else { -t };
        }
        v / 32768.0
    }
}

/// `IN_JoyMove`'s loop body: what one axis's value does, by what it drives.
struct MoveContext<'a> {
    cvars: &'a Cvars,
    held: Held,
    /// `speed`: `cl_movespeedkey` while `+speed` is held, else 1.
    speed: f32,
    /// `aspeed = speed * host_frametime`.
    aspeed: f32,
}

impl MoveContext<'_> {
    /// The loop's switch on `dwAxisMap[i]`.
    fn axis(&self, control: AxisControl, relative: bool, v: f32, m: &mut JoyMove) {
        let (c, cv) = (self.cvars, &self.cvars.joy);
        match control {
            AxisControl::Forward if !cv.advanced && self.held.mlook => {
                // "user wants forward control to become look control"; only
                // here does Invert Mouse (m_pitch < 0) turn the stick over.
                if v.abs() > cv.pitch_threshold {
                    let d = v * cv.pitch_sensitivity * self.aspeed * CL_PITCHSPEED;
                    m.pitch += if c.invert_mouse() { -d } else { d };
                    m.stop_drift = true;
                } else if !c.lookspring {
                    // "*** this code can be removed when the lookspring bug
                    // is fixed": an idle look axis stops the drift.
                    m.stop_drift = true;
                }
            }
            AxisControl::Forward => {
                if v.abs() > cv.forward_threshold {
                    m.forward += v * cv.forward_sensitivity * self.speed * c.cl_forwardspeed;
                }
            }
            AxisControl::Side => {
                if v.abs() > cv.side_threshold {
                    m.side += v * cv.side_sensitivity * self.speed * CL_SIDESPEED;
                }
            }
            AxisControl::Turn if self.held.strafe || (c.lookstrafe && self.held.mlook) => {
                // "user wants turn control to become side control".
                if v.abs() > cv.side_threshold {
                    m.side -= v * cv.side_sensitivity * self.speed * CL_SIDESPEED;
                }
            }
            AxisControl::Turn => {
                if v.abs() > cv.yaw_threshold {
                    let rate = if relative { self.speed * 180.0 } else { self.aspeed * CL_YAWSPEED };
                    m.yaw += v * cv.yaw_sensitivity * rate;
                }
            }
            AxisControl::Look if self.held.mlook => {
                if v.abs() > cv.pitch_threshold {
                    let rate = if relative { self.speed * 180.0 } else { self.aspeed * CL_PITCHSPEED };
                    m.pitch += v * cv.pitch_sensitivity * rate;
                    m.stop_drift = true;
                } else if !c.lookspring {
                    m.stop_drift = true; // the lookspring bug, as above
                }
            }
            AxisControl::Look | AxisControl::Nada => {}
        }
    }
}

/// The port's stick shaping (`joy_deadzone`, `joy_exponent`), between the
/// reading and id's switch; with both off (Classic) the axes reach it as
/// read. The axes are paired by what they drive — walk with strafe, turn
/// with look — as a physical stick is: each pair's dead zone is a circle and
/// what is past it starts again from 0, so a small push is a slow move in any
/// direction where id's thresholds cut each axis alone (a square dead zone,
/// and a jump from 0 to the threshold). The look pair's deflection is raised
/// to `joy_exponent`, a curve for fine aim; walking stays a straight line.
/// Relative axes (movements, not positions) are left alone.
fn shape_sticks(
    values: &mut [f32; JOY_MAX_AXES],
    map: &[AxisControl; JOY_MAX_AXES],
    relative: &[bool; JOY_MAX_AXES],
    cv: &JoyCvars,
) {
    if cv.deadzone <= 0.0 && cv.exponent == 1.0 {
        return;
    }
    let find = |c: AxisControl| (0..JOY_MAX_AXES).find(|&i| map[i] == c && !relative[i]);
    let move_pair = [find(AxisControl::Side), find(AxisControl::Forward)];
    let look_pair = [find(AxisControl::Turn), find(AxisControl::Look)];
    shape_pair(values, move_pair, cv.deadzone, 1.0);
    shape_pair(values, look_pair, cv.deadzone, cv.exponent);
}

/// One stick's round dead zone and curve, over the axes `pair` names.
fn shape_pair(values: &mut [f32; JOY_MAX_AXES], pair: [Option<usize>; 2], deadzone: f32, exponent: f32) {
    let dz = deadzone.clamp(0.0, 0.95);
    let get = |i: Option<usize>| i.map_or(0.0, |i| values[i]);
    let m = get(pair[0]).hypot(get(pair[1]));
    let scale = if m <= dz || m == 0.0 {
        0.0
    } else {
        let r = ((m - dz) / (1.0 - dz)).min(1.0);
        r.powf(exponent.max(0.1)) / m
    };
    for i in pair.into_iter().flatten() {
        values[i] *= scale;
    }
}

/// The keyboard key a pad key is in a menu (the port's `joy_menukeys`), for
/// menu.c, which knows only the keyboard's: A is Enter (on a yes/no prompt,
/// `y`), B and Start are Escape, the D-pad's hat keys are the arrows.
/// `None`: the key is itself.
pub fn menu_key(key: u8, dest: PadKeys) -> Option<u8> {
    const A: u8 = K_JOY1;
    const B: u8 = K_JOY1 + 1;
    const START: u8 = K_AUX1 + 9;
    match (dest, key) {
        (PadKeys::Game, _) => None,
        (PadKeys::YesNo, A) => Some(b'y'),
        (PadKeys::Menu, A) => Some(K_ENTER),
        (_, B | START) => Some(K_ESCAPE),
        (_, K_AUX29) => Some(K_UPARROW),
        (_, k) if k == K_AUX29 + 1 => Some(K_RIGHTARROW),
        (_, k) if k == K_AUX29 + 2 => Some(K_DOWNARROW),
        (_, K_AUX32) => Some(K_LEFTARROW),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rumble (the port's)
// ---------------------------------------------------------------------------

/// defs.qc's weapon bits (`self.weapon`) that rumble when fired.
const IT_SUPER_SHOTGUN: i32 = 2;
const IT_GRENADE_LAUNCHER: i32 = 16;
const IT_ROCKET_LAUNCHER: i32 = 32;
const IT_LIGHTNING: i32 = 64;

/// A rumble of the pad's two motors (the Gamepad API's "dual-rumble"): the
/// low, strong one and the high, weak one, 0..1, for `ms` milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rumble {
    pub strong: f32,
    pub weak: f32,
    pub ms: u32,
}

impl Rumble {
    /// Taking damage: `count` is `V_ParseDamage`'s (half the blood and
    /// armour lost, at least 10) — what the damage flash is made of, so the
    /// pad shakes as hard as the screen flashes: a grunt's pellets briefly
    /// and lightly, a rocket (count 50 and up) fully for most of half a
    /// second. `strength` is `joy_rumble`.
    pub fn damage(count: f32, strength: f32) -> Rumble {
        let s = (count / 50.0).clamp(0.2, 1.0) * strength;
        let ms = (80.0 + 6.0 * count).clamp(120.0, 450.0) as u32;
        Rumble { strong: s, weak: s * 0.6, ms }
    }

    /// Firing `weapon` (the player's `weapon`, an `IT_*` bit): a kick for the
    /// heavy weapons — super shotgun, grenade and rocket launchers — and a
    /// buzz for each of the lightning gun's bolts; nothing for the rest.
    pub fn shot(weapon: i32, strength: f32) -> Option<Rumble> {
        let (strong, weak, ms) = match weapon {
            IT_SUPER_SHOTGUN => (0.45, 0.25, 110),
            IT_GRENADE_LAUNCHER => (0.4, 0.2, 110),
            IT_ROCKET_LAUNCHER => (0.65, 0.35, 150),
            IT_LIGHTNING => (0.15, 0.35, 110),
            _ => return None,
        };
        Some(Rumble { strong: strong * strength, weak: weak * strength, ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{K_AUX1, K_JOY4};

    /// A standard pad at rest with 17 buttons (Guide last).
    fn pad() -> Pad {
        Pad { standard: true, num_buttons: 17, ..Pad::default() }
    }

    /// id's own joystick (the pad is a shared control, on by default in
    /// both presets now, so this forces id's 1996 one — [`JoyCvars::classic`]
    /// — on explicitly, as [`Cvars::with_id_controls`] leaves it).
    fn classic_on() -> Cvars {
        let mut c = Cvars::classic().with_id_controls();
        c.joy.enabled = true;
        c
    }

    fn detected(p: Pad) -> Joystick {
        let mut j = Joystick::default();
        j.set_pad(Some(p));
        j
    }

    #[test]
    fn a_standard_pad_reads_as_an_xbox_pad_through_winmm() {
        let mut p = pad();
        p.axes = [0.5, -1.0, 0.25, 0.75, 1.0, 0.0];
        p.pressed = 1 | (1 << DPAD_UP) | (1 << DPAD_RIGHT) | (1 << 16);
        let ji = JoyInfo::from_pad(&p);
        assert_eq!(ji.axes, [16384.0, -32768.0, 32767.0, 24576.0, 8192.0, 0.0], "X Y Z(LT-RT) R(RY) U(RX) V");
        assert_eq!(ji.buttons, 1 | (1 << 16), "the D-pad is the hat, not buttons");
        assert_eq!(ji.pov, Some(4500), "up and right: the diagonal");
        p.standard = false;
        let ji = JoyInfo::from_pad(&p);
        assert_eq!(ji.axes[4], 32767.0, "another pad's axes in order");
        assert_eq!((ji.pov, ji.buttons), (None, p.pressed));
        assert_eq!(raw_axis(f32::NAN), 0.0);
        let opposite = (1 << DPAD_UP) | (1 << DPAD_DOWN);
        assert_eq!([dpad_pov(0), dpad_pov(1 << DPAD_LEFT), dpad_pov(opposite)], [None, Some(27000), None]);
    }

    /// IN_Commands: buttons 0..3 are JOY1..JOY4, button 4 on is AUX5 on
    /// (AUX1..AUX4 never come), the hat's straight directions AUX29..AUX32;
    /// a diagonal keys nothing; every change once.
    #[test]
    fn in_commands_keys_buttons_as_joy_and_aux_and_the_hat_as_aux29() {
        let cv = classic_on();
        let mut j = detected(pad());
        assert_eq!(j.take_prints(), ["\njoystick detected\n\n"]);
        let mut p = pad();
        p.pressed = 1 | (1 << 3) | (1 << 4) | (1 << 9) | (1 << DPAD_DOWN);
        j.set_pad(Some(p));
        let ev = j.commands(&cv.joy, PadKeys::Game);
        assert_eq!(ev, [(K_JOY1, true), (K_JOY4, true), (K_AUX1 + 4, true), (K_AUX1 + 9, true), (K_AUX29 + 2, true)]);
        assert!(j.commands(&cv.joy, PadKeys::Game).is_empty(), "no change, no event");
        p.pressed = (1 << DPAD_DOWN) | (1 << DPAD_LEFT);
        j.set_pad(Some(p));
        let ev = j.commands(&cv.joy, PadKeys::Game);
        let up = [(K_JOY1, false), (K_JOY4, false), (K_AUX1 + 4, false), (K_AUX1 + 9, false), (K_AUX29 + 2, false)];
        assert_eq!(ev, up, "a diagonal keys no direction");
    }

    #[test]
    fn nothing_is_keyed_with_joystick_off_and_a_lost_pad_lets_go() {
        let id = Cvars::classic().with_id_controls();
        let mut j = detected(Pad { pressed: 1, ..pad() });
        assert!(j.commands(&id.joy, PadKeys::Game).is_empty(), "id's own controls: joystick 0");
        let on = classic_on();
        assert_eq!(j.commands(&on.joy, PadKeys::Game), [(K_JOY1, true)]);
        j.set_pad(None);
        assert_eq!(j.commands(&on.joy, PadKeys::Game), [(K_JOY1, false)], "unplugged: released");
        let mut off = Joystick::default();
        off.set_nojoy();
        off.set_pad(Some(Pad { pressed: 1, ..pad() }));
        assert!(off.commands(&on.joy, PadKeys::Game).is_empty() && off.take_prints().is_empty(), "-nojoy");
    }

    /// The port's menu keys: A is Enter (y on a prompt), B and Start Escape,
    /// the D-pad the arrows — and a key comes up as what it went down as,
    /// wherever the keyboard went in between.
    #[test]
    fn menu_keys_translate_and_release_symmetrically() {
        let mut cv = Cvars::slop();
        let mut j = detected(pad());
        j.set_pad(Some(Pad { pressed: 1 | (1 << DPAD_UP), ..pad() }));
        assert_eq!(j.commands(&cv.joy, PadKeys::Menu), [(K_ENTER, true), (K_UPARROW, true)]);
        j.set_pad(Some(pad()));
        assert_eq!(j.commands(&cv.joy, PadKeys::Game), [(K_ENTER, false), (K_UPARROW, false)], "up as it went down");
        j.set_pad(Some(Pad { pressed: 1, ..pad() }));
        assert_eq!(j.commands(&cv.joy, PadKeys::YesNo), [(b'y', true)]);
        j.set_pad(Some(pad()));
        assert_eq!(j.commands(&cv.joy, PadKeys::Menu), [(b'y', false)]);
        cv.joy.menu_keys = false;
        j.set_pad(Some(Pad { pressed: 1 << 9, ..pad() }));
        assert_eq!(j.commands(&cv.joy, PadKeys::Menu), [(K_AUX1 + 9, true)], "joy_menukeys 0: Start is AUX10");
        assert_eq!([K_JOY1 + 1, K_AUX1 + 9, K_AUX29 + 1, K_AUX32, K_JOY1 + 2].map(|k| menu_key(k, PadKeys::Menu)),
                   [Some(K_ESCAPE), Some(K_ESCAPE), Some(K_RIGHTARROW), Some(K_LEFTARROW), None]);
    }

    /// IN_JoyMove at id's defaults (`joystick 1`): X turns at cl_yawspeed
    /// (140°/s full), Y walks at cl_forwardspeed, both past the 0.15
    /// threshold only; `+speed` doubles both; `+mlook` makes Y look.
    #[test]
    fn in_joymove_at_ids_defaults_turns_with_x_and_walks_with_y() {
        let cv = classic_on();
        let mut j = detected(pad());
        let held = Held::default();
        j.set_pad(Some(Pad { axes: [1.0, -0.5, 0.0, 0.0, 0.0, 0.0], ..pad() }));
        let m = j.joy_move(&cv, Preset::Classic, held, 0.1);
        assert!((m.yaw - -(32767.0 / 32768.0) * 14.0).abs() < 1e-4, "right turns right, 140°/s: {}", m.yaw);
        assert_eq!(m.forward, 100.0, "up walks: 0.5 x 200");
        let fast = j.joy_move(&cv, Preset::Classic, Held { speed: true, ..held }, 0.1);
        assert_eq!((fast.forward, fast.yaw), (200.0, m.yaw * 2.0));
        j.set_pad(Some(Pad { axes: [0.1, 0.1, 0.0, 0.0, 0.0, 0.0], ..pad() }));
        assert_eq!(j.joy_move(&cv, Preset::Classic, held, 0.1), JoyMove::default(), "under the thresholds");
        j.set_pad(Some(Pad { axes: [0.0, 0.5, 0.0, 0.0, 0.0, 0.0], ..pad() }));
        let look = j.joy_move(&cv, Preset::Classic, Held { mlook: true, ..held }, 0.1);
        assert_eq!((look.forward, look.pitch, look.stop_drift), (0.0, 7.5, true), "+mlook: Y pitches at 150°/s");
        let mut inv = cv.clone();
        inv.m_pitch = -inv.m_pitch;
        assert_eq!(j.joy_move(&inv, Preset::Classic, Held { mlook: true, ..held }, 0.1).pitch, -7.5, "Invert Mouse");
        let id = Cvars::classic().with_id_controls();
        assert_eq!(j.joy_move(&id, Preset::Classic, held, 0.1), JoyMove::default(), "id's own controls: joystick 0");
    }

    /// joyadvanced: the maps come from joyadvaxis* (bits 0..3, 16 relative)
    /// and only after joyadvancedupdate — or another preset.
    #[test]
    fn joyadvanced_maps_axes_after_joyadvancedupdate() {
        let mut cv = classic_on();
        let mut j = detected(pad());
        j.set_pad(Some(Pad { axes: [0.0, 0.0, 1.0, 0.0, 0.0, 0.0], ..pad() }));
        let held = Held::default();
        assert_eq!(j.joy_move(&cv, Preset::Classic, held, 0.1), JoyMove::default(), "U drives nothing by default");
        cv.joy.advanced = true;
        cv.joy.name = "pad".into();
        cv.joy.advaxis[4] = 4.0 + 16.0; // U: relative turn
        assert_eq!(j.joy_move(&cv, Preset::Classic, held, 0.1), JoyMove::default(), "not until joyadvancedupdate");
        j.advanced_update(&cv.joy);
        assert_eq!(j.take_prints().last().map(String::as_str), Some("\npad configured\n\n"));
        let m = j.joy_move(&cv, Preset::Classic, held, 0.1);
        let full = 32767.0 / 32768.0;
        assert!((m.yaw + 180.0 * full).abs() < 1e-3, "a relative axis: 180 a read, not per second: {}", m.yaw);
        cv.joy.advaxis[4] = 0.0;
        let m = j.joy_move(&cv, Preset::Slop, held, 0.1);
        assert_eq!(m.yaw, 0.0, "another preset remakes the maps");
        assert_eq!([0, 1, 2, 3, 4, 5, 20].map(AxisControl::from_bits)[6], AxisControl::Turn);
    }

    /// The slop layout: left stick walks and strafes, right stick turns and
    /// looks through the round dead zone and the curve; a rest drifts nothing.
    #[test]
    fn the_slop_pad_is_twin_stick_with_a_round_dead_zone_and_a_curve() {
        let cv = Cvars::slop();
        let mut j = detected(pad());
        let held = Held { mlook: true, ..Held::default() };
        let at = |j: &mut Joystick, axes: [f32; 6]| {
            j.set_pad(Some(Pad { axes, ..pad() }));
            j.joy_move(&cv, Preset::Slop, held, 0.1)
        };
        let rest = at(&mut j, [0.15, -0.1, 0.12, 0.1, 0.0, 0.0]);
        assert_eq!((rest.forward, rest.side, rest.yaw, rest.pitch), (0.0, 0.0, 0.0, 0.0), "inside the dead zone");
        let walk = at(&mut j, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let full = 350.0 * 32767.0 / 32768.0;
        assert!((walk.side - full).abs() < 0.1 && walk.forward == 0.0, "left stick right strafes right: {walk:?}");
        let up = at(&mut j, [0.0, -0.6, 0.0, 0.0, 0.0, 0.0]);
        assert!((up.forward - 400.0 * 0.5).abs() < 0.1, "up walks, rescaled from the dead zone's edge: {up:?}");
        let turn = at(&mut j, [0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        assert!((turn.yaw + 24.5).abs() < 0.01, "full right: 245°/s: {turn:?}");
        let half = at(&mut j, [0.0, 0.0, 0.6, 0.0, 0.0, 0.0]);
        assert!((half.yaw + 24.5 * 0.25).abs() < 0.01, "half past the dead zone: a quarter the speed: {half:?}");
        let down = at(&mut j, [0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert!((down.pitch - 15.0).abs() < 0.01 && down.stop_drift, "stick down looks down: {down:?}");
        let diag = at(&mut j, [0.0, 0.0, 0.8, 0.8, 0.0, 0.0]);
        assert!((diag.yaw / diag.pitch + 24.5 / 15.0).abs() < 1e-3, "a round stick keeps the direction: {diag:?}");
        let trig = at(&mut j, [0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert_eq!(trig, JoyMove { stop_drift: true, ..JoyMove::default() }, "the triggers are keys, Z drives nothing");
    }

    #[test]
    fn frame_rate_does_not_change_how_far_the_stick_turns() {
        let cv = Cvars::slop();
        let mut j = detected(Pad { axes: [0.0, 0.0, 0.7, 0.0, 0.0, 0.0], ..pad() });
        let held = Held { mlook: true, ..Held::default() };
        let turn = |j: &mut Joystick, hz: f32| {
            (0..hz as usize).map(|_| j.joy_move(&cv, Preset::Slop, held, 1.0 / hz).yaw).sum::<f32>()
        };
        let (at60, at480) = (turn(&mut j, 60.0), turn(&mut j, 480.0));
        assert!((at60 - at480).abs() < 1e-3, "a second of stick at 60 and 480 Hz: {at60} {at480}");
    }

    #[test]
    fn rumble_follows_the_damage_count_and_the_heavy_weapons() {
        let light = Rumble::damage(10.0, 1.0);
        let rocket = Rumble::damage(60.0, 1.0);
        assert!(light.strong < rocket.strong && light.ms < rocket.ms && rocket.strong == 1.0, "{light:?} {rocket:?}");
        assert_eq!(Rumble::damage(60.0, 0.5).strong, 0.5, "joy_rumble scales it");
        assert!(Rumble::shot(IT_ROCKET_LAUNCHER, 1.0).is_some() && Rumble::shot(IT_LIGHTNING, 1.0).is_some());
        assert_eq!([1, 4, 8, 4096].map(|w| Rumble::shot(w, 1.0)), [None; 4], "shotgun, nailguns, axe: none");
    }
}
