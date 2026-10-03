//! Player input — in_win.c (`IN_MouseMove`, and the host side of the
//! joystick: `IN_Commands`, `IN_JoyMove`) and keys.c's `Key_Event`: what the
//! page's key, mouse and gamepad records do. Every key goes through
//! [`key_event`], which hands it to the menu, the console or its binding as
//! id's does; the per-frame `KeyMove` the held bindings feed (cl_input.c's
//! `CL_BaseMove`/`CL_AdjustAngles`) is [`quake_rs::client::cl_input`]'s, and
//! the pad as winmm's joystick is [`quake_rs::client::in_win`]'s.

use quake_rs::client::cl_input::{clamp_pitch, V_CENTERSPEED};
use quake_rs::client::in_win::{Held, Joystick, Pad, PadKeys, Rumble};
use quake_rs::client::Walk;
use quake_rs::render::MenuScreen;
use quake_rs::keys::{
    consolekey, keynum_to_string, keyshift, menubound, Binding, BIND_CENTERVIEW, BIND_CHANGEWEAPON,
    BIND_IMPULSE_0, BIND_MLOOK, BIND_PAUSE, BIND_SIZEDOWN, BIND_SIZEUP, BIND_STRAFE, BIND_TOGGLECONSOLE,
    K_BACKSPACE, K_ESCAPE, K_PAUSE, K_SHIFT,
};

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
        if key >= 200 && a.settings.binds.get(key).is_none() {
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
            let action = a.menu.keydown(K_ESCAPE, None, &mut a.settings);
            return apply_menu_action(a, action).map(KeyAfter::Menu);
        }
        a.m_toggle_menu();
        return None;
    }
    // Key ups only release the `+` button commands (the `-` half), in every
    // key_dest.
    if !down {
        return key_up_binding(a, key);
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
        return run_binding(a, key);
    }
    let key = if a.shift_down { keyshift(key) } else { key };
    let text = match ch {
        0 => (32..127).contains(&key).then_some(key),
        c => (32..127).contains(&c).then_some(c as u8),
    };
    match dest {
        KeyDest::Menu => {
            let action = a.menu.keydown(key, text, &mut a.settings);
            apply_menu_action(a, action).map(KeyAfter::Menu)
        }
        KeyDest::Game | KeyDest::Console => crate::console::key_console(a, key, text).map(KeyAfter::Console),
    }
}

/// `Key_Event`'s command dispatch for a key down: `kb = keybindings[key]`.
/// A `+` command is held until its key comes up (`keys_held`, the button
/// state `CL_BaseMove` reads each frame); the rest run once, now. A binding
/// to a console line comes back to be run once the App borrow ends.
fn run_binding(a: &mut App, key: u8) -> Option<KeyAfter> {
    let cmd = match a.settings.binds.get(key)? {
        Binding::Command(cmd) => *cmd,
        Binding::Line(line) => return Some(KeyAfter::Console(line.to_string())),
    };
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
                start_pitch_drift(w);
            }
        }
        // default.cfg's `+`/`=` "sizeup" and `-` "sizedown" (SCR_SizeUp_f /
        // SCR_SizeDown_f): step the viewsize; the next frame reframes.
        BIND_SIZEUP => a.settings.cvars.size_up(),
        BIND_SIZEDOWN => a.settings.cvars.size_down(),
        BIND_PAUSE => crate::host_cmd::host_pause(a),
        BIND_TOGGLECONSOLE => a.toggle_console(),
        _ => {}
    }
    None
}

/// `Key_Event` for a key up: the key is released, so a `+` command it held
/// is let go (`-cmd`) — `IN_MLookUp` re-levels the view with `lookspring`
/// when mouse look ends with it — and a `+` console line runs its `-` half.
fn key_up_binding(a: &mut App, key: u8) -> Option<KeyAfter> {
    let was_held = std::mem::replace(&mut a.keys_held[key as usize], false);
    match a.settings.binds.get(key)? {
        Binding::Command(BIND_MLOOK) if was_held && !mouse_look(a) && a.settings.cvars.lookspring => {
            if let Some(w) = a.walk.as_mut() {
                start_pitch_drift(w);
            }
            None
        }
        Binding::Line(line) => line.strip_prefix('+').map(|rest| KeyAfter::Console(format!("-{rest}"))),
        Binding::Command(_) => None,
    }
}

/// `in_mlook`: mouse look is on — `+mlook` held, or the 2026 `freelook`
/// holding it for good.
fn mouse_look(a: &App) -> bool {
    a.settings.cvars.freelook || a.settings.binds.held(BIND_MLOOK, &a.keys_held)
}

/// `V_StartPitchDrift` (view.c): seed the drift that re-levels the view.
fn start_pitch_drift(w: &mut Walk) {
    if !w.pitch_drift || w.pitch_vel == 0.0 {
        w.pitch_vel = V_CENTERSPEED;
        w.pitch_drift = true;
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

/// A key press and release, as the automation's and tests' menu calls send
/// them.
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

/// vid_win.c's `ClearAllStates`, what the page runs whenever a key's release
/// may never come: the window lost the keyboard, the pointer lock or
/// fullscreen, or the tab was hidden (`web/PLATFORM.md`, "Input"). id's
/// ran it on every activation change and video mode set ("fix the leftover
/// Alt from any Alt-Tab"). In id's order: `Key_Event (i, false)` for every
/// key, so each `+` binding lets go as its key's release would (a `+` line
/// runs its `-` half, `+mlook`'s release re-levels with `lookspring`), then
/// `Key_ClearStates` (`keydown[]`, `key_repeats[]`) and `IN_ClearStates`
/// (the mouse movement not yet in a move: `mx_accum`, `my_accum`). A key
/// the player still holds presses again with its next autorepeat: its
/// repeat count starts over, so `Key_Event` takes it as a press.
pub(crate) fn clear_all_states() {
    for key in 0..=u8::MAX {
        key_event(i32::from(key), 0, 0);
    }
    ensure_app(|a| {
        a.keys_held = [false; 256];
        a.key_repeats = [0; 256];
        if let Some(w) = a.walk.as_mut() {
            w.mouse_fwd = 0.0;
            w.mouse_side = 0.0;
        }
    });
}

/// The keys the engine holds down (`keydown[]`), by `Key_KeynumToString`
/// name, space-separated, and how many — a read-only call for the browser
/// checks: after the page loses the keyboard it must be none.
pub(crate) fn keys_held() -> (usize, String) {
    APP.with(|c| {
        let names: Vec<String> = c.borrow().as_ref().map_or_else(Vec::new, |a| {
            (0..=u8::MAX).filter(|&k| a.keys_held[usize::from(k)]).map(keynum_to_string).collect()
        });
        (names.len(), names.join(" "))
    })
}

/// 1 when the engine currently believes Quake keynum `keynum` is held — a
/// read-only verification/debug call (like
/// [`menu_screen_id`](crate::menu::menu_screen_id)). The browser harness
/// uses it to prove the page's `e.code` punctuation mapping keeps
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
/// turns yaw, OR strafes (`m_side`) while `+strafe` is held or `lookstrafe`
/// is on in mouse look; in mouse look ([`mouse_look`]: `+mlook` held, or the
/// 2026 `freelook`) mouse Y drives pitch (sign = Invert Mouse, `m_pitch.value
/// < 0`), clamped 80/-70, and stops an active pitch drift (V_StopPitchDrift);
/// otherwise — id's default, or with `+strafe` held — it feeds forwardmove
/// (`m_forward`). Gated behind the menu/console like `look`.
pub(crate) fn mouse_move(dx: f32, dy: f32) {
    ensure_app(|a| {
        a.mouse.records += 1;
        if a.menu.visible || a.console.open {
            return;
        }
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        a.mouse.counts += f64::from(dx.abs());
        // mouse_x *= sensitivity.value (the raw 1..11 cvar, like the C — the
        // 0.16/3 port calibration lives in M_YAW_PORT/M_PITCH_PORT).
        let c = &a.settings.cvars;
        let (mx, my) = (dx * c.sensitivity, dy * c.sensitivity);
        let strafe_held = a.settings.binds.held(BIND_STRAFE, &a.keys_held);
        let (lookstrafe, invert, mlook) = (c.lookstrafe, c.invert_mouse(), mouse_look(a));
        if let Some(w) = a.walk.as_mut() {
            // if (in_strafe || (lookstrafe && in_mlook)) sidemove += m_side*mx
            // else viewangles[YAW] -= m_yaw*mx.
            if strafe_held || (lookstrafe && mlook) {
                w.mouse_side += M_SIDE * mx;
            } else {
                w.yaw -= M_YAW_PORT * mx;
                a.mouse.turned += f64::from((M_YAW_PORT * mx).abs());
            }
            // if (in_mlook) V_StopPitchDrift() — every mlook mouse move.
            if mlook {
                w.pitch_drift = false;
                w.pitch_vel = 0.0;
            }
            // if (in_mlook && !in_strafe) pitch += m_pitch*my (clamped 80/-70)
            // else forwardmove -= m_forward*my.
            if mlook && !strafe_held {
                let m_pitch = if invert { -M_PITCH_PORT } else { M_PITCH_PORT };
                w.pitch = clamp_pitch(w.pitch + m_pitch * my);
            } else {
                w.mouse_fwd -= M_FORWARD * my;
            }
        }
    });
}

/// What the page's mouse has done since the program started, for its
/// `?mousecheck` (`web/PLATFORM.md`, "The mouse at any frame rate"): the
/// `Mouse` records read, the horizontal counts of those the game took
/// (`|dx|`; it takes none behind the menu or console) and the yaw they
/// turned the view (`|°|`).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MouseCount {
    pub(crate) records: u64,
    pub(crate) counts: f64,
    pub(crate) turned: f64,
}

/// [`MouseCount`] and the host frames run (`host_framecount`), as the
/// `mouse_count` call's text: `records counts turned frames`.
pub(crate) fn mouse_count() -> String {
    APP.with(|c| {
        let (m, frames) = c.borrow().as_ref().map_or((MouseCount::default(), 0), |a| (a.mouse, a.host_framecount));
        format!("{} {} {} {frames}", m.records, m.counts, m.turned)
    })
}

/// The pointer lock was released. With `freelook` (2026) `+mlook` is held
/// for as long as the pointer is locked, so unlock IS the mlook release — the
/// `lookspring` trigger (`IN_MLookUp`, cl_input.c: when `+mlook` releases and
/// `lookspring.value` is set, `V_StartPitchDrift()` re-centres the view).
/// Without it (id's) the lock holds nothing, and mouse look ends with its key
/// ([`key_up_binding`]).
pub(crate) fn pointer_unlocked() {
    ensure_app(|a| {
        if !(a.settings.cvars.freelook && a.settings.cvars.lookspring) {
            return;
        }
        if let Some(w) = a.walk.as_mut() {
            start_pitch_drift(w);
        }
    });
}

// --- in_win.c's joystick: the page's gamepad --------------------------------

/// The gamepad's host state: in_win.c's joystick ([`Joystick`]: its reading,
/// `IN_Commands`' keys, `IN_JoyMove`'s axis maps) and the 2026 rumble's.
#[derive(Debug, Default)]
pub(crate) struct PadHost {
    pub(crate) joy: Joystick,
    /// Rumbles for the loop to send with this frame (`Rumble` records).
    rumbles: Vec<Rumble>,
    /// The pad was read (`joystick` on, a pad there) as of the last frame:
    /// the page rumbles the pad, else a phone's vibration.
    pub(crate) pad_read: bool,
    /// The player's `punchangle` pitch as the last frame left it: a weapon's
    /// kick makes it jump down, and the rumble takes that as a shot.
    punch: f32,
}

impl PadHost {
    /// The frame's rumbles, for the loop to send.
    pub(crate) fn take_rumbles(&mut self) -> Vec<Rumble> {
        std::mem::take(&mut self.rumbles)
    }
}

/// The page's reading of the pad, each display refresh (the `Gamepad`
/// record; `None`: no pad connected).
pub(crate) fn gamepad(pad: Option<Pad>) {
    ensure_app(|a| {
        a.pad.joy.set_pad(pad);
        joy_prints(a);
    });
}

/// The joystick's `Con_Printf`s ("joystick detected") to the console.
fn joy_prints(a: &mut App) {
    for text in a.pad.joy.take_prints() {
        a.console.print(&text);
    }
}

/// Where the pad's keys go now (for the 2026 `joy_menukeys`): the menu's,
/// unless a key is being bound (it is the key to bind), or a yes/no
/// prompt's.
fn pad_keys(a: &App) -> PadKeys {
    let menu = &a.menu;
    if a.key_dest() != KeyDest::Menu || menu.bind_grabbing() {
        PadKeys::Game
    } else if menu.new_game_confirm() || menu.screen() == MenuScreen::Quit {
        PadKeys::YesNo
    } else {
        PadKeys::Menu
    }
}

/// `IN_Commands` (host.c runs it after `Host_FilterTime`, before the
/// frame's commands and move): the pad's buttons and hat that changed, each
/// through `Key_Event` as its `JOY`/`AUX` key — or, in a menu with
/// `joy_menukeys`, as the keyboard key the menu knows.
pub(crate) fn in_commands() {
    let mut events = Vec::new();
    ensure_app(|a| {
        let dest = pad_keys(a);
        events = a.pad.joy.commands(&a.settings.cvars.joy, dest);
    });
    for (key, down) in events {
        key_event(i32::from(key), i32::from(down), 0);
    }
}

/// `IN_JoyMove` for the live game's frame of `frametime` seconds (after
/// `CL_BaseMove`'s keys are in `key_move`): the pad's walk and strafe join
/// the frame's move, its turn and look the view angles (the pitch bounded
/// as `IN_JoyMove` bounds it). Gated behind the menu and the console like
/// the mouse, where id's turned the view behind the menu.
pub(crate) fn in_joy_move(a: &mut App, frametime: f64, gated: bool) {
    if a.mode != 0 || a.walk.is_none() {
        return;
    }
    let held = Held {
        speed: a.walk.as_ref().is_some_and(|w| w.key_move.speed),
        strafe: a.settings.binds.held(BIND_STRAFE, &a.keys_held),
        mlook: mouse_look(a),
    };
    let m = a.pad.joy.joy_move(&a.settings.cvars, a.settings.profile, held, frametime as f32);
    joy_prints(a);
    let Some(w) = a.walk.as_mut().filter(|_| !gated) else { return };
    w.key_move.fwd += m.forward;
    w.key_move.side += m.side;
    w.yaw += m.yaw;
    w.pitch = clamp_pitch(w.pitch + m.pitch);
    if m.stop_drift {
        // V_StopPitchDrift.
        w.pitch_drift = false;
        w.pitch_vel = 0.0;
    }
}

/// The 2026 rumble (`joy_rumble`) after a live frame: on the damage the
/// frame's `V_ParseDamage` counted, and on a weapon's kick (the player's
/// `punchangle` pitch jumping down) with a heavy weapon up. The page plays
/// it on the pad while the pad is read ([`PadHost::pad_read`]), else on a
/// phone's vibration with the touch controls.
pub(crate) fn rumble_after_frame(a: &mut App) {
    let strength = a.settings.cvars.joy.rumble;
    a.pad.pad_read = a.pad.joy.active(&a.settings.cvars.joy);
    let Some(w) = a.walk.as_mut().filter(|_| a.mode == 0) else { return };
    let damage = std::mem::take(&mut w.damage_count);
    let vm = &w.server.vm;
    let punch = vm.ent_vec(w.player, vm.fo().punchangle)[0];
    let kicked = punch < a.pad.punch;
    a.pad.punch = punch;
    if strength <= 0.0 {
        return;
    }
    if damage > 0.0 {
        a.pad.rumbles.push(Rumble::damage(damage, strength));
    }
    if kicked && let Some(r) = Rumble::shot(vm.ent_float(w.player, vm.fo().weapon) as i32, strength) {
        a.pad.rumbles.push(r);
    }
}

/// The player's current look pitch in degrees (+down, Quake convention) — a
/// read-only verification/debug call (the browser checks Invert Mouse and
/// lookspring flip/centre the pitch through it). 0 when no walk is live.
pub(crate) fn player_pitch() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|a| a.walk.as_ref().map(|w| w.pitch))
            .unwrap_or(0.0)
    })
}

/// The Options "Mouse speed" as a multiplier of the default (`sensitivity`
/// over default.cfg's 3; 1.0 before the App exists).
pub(crate) fn mouse_sensitivity() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.settings.cvars.sensitivity / 3.0)
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
            derive_key_move(&a.settings.cvars, &a.settings.binds, &a.keys_held).showscores
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
        let bound = APP.with(|c| c.borrow().as_ref().unwrap().settings.binds.command(b'`'));
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
            let a = a.as_ref().unwrap();
            (a.settings.binds.command(K_MOUSE3), a.menu.bind_grabbing())
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
                a.present.rgba()[(a.render_h - rows) * a.render_w * 4..].to_vec()
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
        use_2026();

        // The 2026 binding: w = +forward at cl_forwardspeed 400 — Always Run
        // on in the 2026 profile.
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

    /// Space is `+jump` (`button2`: QuakeC's own swim stroke in water); with
    /// the 2026 `cl_jumpswim` it swims up too (`upmove`, at `cl_upspeed`).
    #[test]
    fn space_swims_up_only_with_cl_jumpswim() {
        use quake_rs::keys::K_SPACE;
        assert_eq!(boot(), 1);
        close_menu();
        key_down(i32::from(K_SPACE));
        step(0.0);
        let classic = walk_mut(|w| (w.key_move.jump, w.key_move.up));
        crate::host_cmd::execute_console_command("cl_jumpswim 1");
        step(0.0);
        let swim = walk_mut(|w| (w.key_move.jump, w.key_move.up));
        key_up(i32::from(K_SPACE));
        assert_eq!(classic, (true, 0.0), "id's: +jump alone");
        assert_eq!(swim, (true, 200.0), "cl_jumpswim: and upmove");
    }

    /// id's mouse (`freelook` off, Classic): mouse Y walks (`m_forward`) and
    /// leaves the pitch alone; holding `+mlook` (`\\`, MOUSE3) looks; letting
    /// it go with `lookspring` re-levels the view (`IN_MLookUp`) — and a
    /// pointer unlock, which holds nothing here, does not.
    #[test]
    fn ids_mouse_walks_and_mlook_held_looks() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.pitch = 0.0);
        mouse_move(0.0, -100.0);
        assert_eq!(player_pitch(), 0.0, "no mouse look: the pitch stays");
        assert_eq!(walk_mut(|w| w.mouse_fwd), 300.0, "forwardmove -= m_forward * my (sensitivity 3)");
        key_down(i32::from(b'\\'));
        mouse_move(0.0, 100.0);
        assert!(player_pitch() > 0.0, "+mlook held: mouse-down looks down");
        crate::host_cmd::execute_console_command("lookspring 1");
        pointer_unlocked();
        assert!(!walk_mut(|w| w.pitch_drift), "the unlock releases nothing without freelook");
        key_up(i32::from(b'\\'));
        assert!(walk_mut(|w| w.pitch_drift), "releasing +mlook with lookspring starts the drift");
        for _ in 0..30 {
            step(0.05);
        }
        assert!(player_pitch().abs() < 0.5, "and the view re-levels ({})", player_pitch());
    }

    /// vid_win.c's `ClearAllStates` (the page sends it when a key's release
    /// may never come: the lock, fullscreen or the keyboard lost, the tab
    /// hidden): every key lets go as its own release would — `+mlook`'s
    /// release re-levels the view with `lookspring`, which zeroing
    /// `keydown[]` alone (the port's version until 2026-10-02) skipped —
    /// Shift is up, the mouse movement not yet in a move is dropped
    /// (`IN_ClearStates`), and a key still held presses again with its next
    /// autorepeat (`key_repeats` starts over).
    #[test]
    fn clear_all_states_lets_every_key_go_as_its_release_would() {
        use quake_rs::keys::{K_MOUSE1, K_UPARROW};
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        crate::host_cmd::execute_console_command("lookspring 1");
        mouse_move(0.0, -10.0);
        for k in [K_UPARROW, b'\\', K_SHIFT, K_MOUSE1] {
            key_down(i32::from(k));
            key_down(i32::from(k)); // an autorepeat
        }
        assert_eq!(keys_held(), (4, "\\ UPARROW SHIFT MOUSE1".to_string()));
        assert!(!walk_mut(|w| w.pitch_drift));
        assert_eq!(walk_mut(|w| w.mouse_fwd), 30.0, "id's mouse Y: a pending forward move");
        clear_all_states();
        assert_eq!(keys_held(), (0, String::new()));
        assert!(walk_mut(|w| w.pitch_drift), "+mlook let go with lookspring: the view re-levels");
        assert_eq!(walk_mut(|w| w.mouse_fwd), 0.0, "IN_ClearStates: the mouse's pending move dropped");
        let (shift, repeats) = APP.with(|c| {
            let a = c.borrow();
            let a = a.as_ref().unwrap();
            (a.shift_down, a.key_repeats.iter().all(|&r| r == 0))
        });
        assert!(!shift && repeats, "Shift is up and no key counts as repeating");
        key_down(i32::from(K_UPARROW)); // the held key's next autorepeat
        assert_eq!(key_is_down(i32::from(K_UPARROW)), 1, "a key still held presses again");
        key_up(i32::from(K_UPARROW));
    }

    #[test]
    fn invert_mouse_flips_pitch_and_lookspring_recentres_on_unlock() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        use_2026(); // freelook: the mouse looks while the pointer is locked

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

    /// A standard pad (17 buttons) with `pressed` and `axes`.
    fn pad(pressed: u32, axes: [f32; 6]) -> Option<Pad> {
        Some(Pad { standard: true, num_buttons: 17, pressed, axes })
    }

    /// The page's refresh with the pad in this state: its reading, then a
    /// host frame.
    fn pad_frame(pressed: u32, axes: [f32; 6]) {
        gamepad(pad(pressed, axes));
        step(1.0 / 60.0);
    }

    fn yaw() -> f32 {
        walk_mut(|w| w.yaw)
    }

    /// Classic is id's: `joystick 0` reads no pad; with `joystick 1` the
    /// left stick is a 1996 joystick (X turns, Y walks) and the buttons are
    /// id's unbound JOY/AUX keys.
    #[test]
    fn classic_reads_the_pad_only_with_joystick_1_as_ids_joystick() {
        use quake_rs::keys::K_JOY1;
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        let yaw0 = yaw();
        for _ in 0..3 {
            pad_frame(1, [1.0, -1.0, 0.0, 0.0, 0.0, 0.0]);
        }
        assert_eq!((yaw(), key_is_down(i32::from(K_JOY1))), (yaw0, 0), "joystick 0: nothing");
        let lines = || {
            APP.with(|c| c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect::<Vec<_>>())
        };
        assert!(lines().iter().any(|l| l == "joystick detected"), "IN_StartupJoystick's line");
        crate::host_cmd::execute_console_command("joystick 1");
        pad_frame(1, [1.0, -1.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(yaw() < yaw0, "X right turns right");
        assert_eq!(walk_mut(|w| w.key_move.fwd), 200.0, "Y up walks at cl_forwardspeed");
        assert!(lines().iter().any(|l| l == "JOY1 is unbound, hit F4 to set."), "A is JOY1, unbound in default.cfg");
        pad_frame(0, [0.0; 6]);
    }

    /// The 2026 pad: the left stick walks, the right stick turns, the right
    /// trigger fires (`AUX8` `+attack`) — and a rocket's kick rumbles, as
    /// does damage.
    #[test]
    fn the_2026_pad_walks_turns_fires_and_rumbles() {
        use quake_rs::keys::K_AUX1;
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        use_2026();
        pad_frame(0, [0.0; 6]);
        let (x0, y0, yaw0) = (listener().pos[0], listener().pos[1], yaw());
        for _ in 0..30 {
            pad_frame(0, [0.0, -1.0, 0.3, 0.0, 0.0, 0.0]);
        }
        let dist = ((listener().pos[0] - x0).powi(2) + (listener().pos[1] - y0).powi(2)).sqrt();
        assert!(dist > 100.0, "the left stick walks ({dist:.1}u)");
        assert!(yaw() < yaw0, "the right stick turns right");
        let take = || APP.with(|c| c.borrow_mut().as_mut().unwrap().pad.take_rumbles());
        take();
        let p = walk_mut(|w| w.player);
        walk_mut(|w| w.server.vm.ent_set_float(p, "dmg_take", 20.0));
        pad_frame(0, [0.0; 6]);
        let hurt = take();
        assert_eq!(hurt, [Rumble::damage(10.0, 1.0)], "V_ParseDamage's count 10");
        walk_mut(|w| w.next_impulse = 9); // every weapon
        pad_frame(0, [0.0; 6]);
        key_down(i32::from(b'7'));
        key_up(i32::from(b'7'));
        for _ in 0..30 {
            pad_frame(0, [0.0; 6]);
        }
        assert_eq!(player_field("weapon") as i32, IT_RL);
        take();
        pad_frame(1 << 7, [0.0; 6]);
        assert_eq!(key_is_down(i32::from(K_AUX1 + 7)), 1, "RT is AUX8, +attack");
        for _ in 0..5 {
            pad_frame(1 << 7, [0.0; 6]);
        }
        pad_frame(0, [0.0; 6]);
        let shots = take();
        assert!(shots.contains(&Rumble::shot(IT_RL, 1.0).unwrap()), "the rocket's kick: {shots:?}");
        let pad_read = || APP.with(|c| c.borrow().as_ref().unwrap().pad.pad_read);
        assert!(pad_read(), "the pad is read: the page rumbles it");
        // With the pad not read, the rumble still comes, for a phone.
        crate::host_cmd::execute_console_command("joystick 0");
        walk_mut(|w| w.server.vm.ent_set_float(p, "dmg_take", 20.0));
        pad_frame(0, [0.0; 6]);
        assert_eq!((take().len(), pad_read()), (1, false), "joystick 0: a phone's vibration");
        crate::host_cmd::execute_console_command("joy_rumble 0");
        walk_mut(|w| w.server.vm.ent_set_float(p, "dmg_take", 20.0));
        pad_frame(0, [0.0; 6]);
        assert!(take().is_empty(), "joy_rumble 0: none");
    }

    /// The 2026 pad in the menus (`joy_menukeys`): Start opens the menu
    /// (`togglemenu`), the D-pad moves, A enters, B backs out and closes it.
    #[test]
    fn the_2026_pad_works_the_menus() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        use_2026();
        let rest = [0.0; 6];
        let press = |bit: u32| {
            pad_frame(1 << bit, rest);
            pad_frame(0, rest);
        };
        press(9); // Start
        assert_eq!((menu_visible(), crate::menu::menu_screen_id()), (1, 0), "Start: the main menu");
        let cursor = || APP.with(|c| c.borrow().as_ref().unwrap().menu.cursor());
        let at = cursor();
        press(13); // D-pad down
        assert_eq!(cursor(), at + 1, "the D-pad moves the cursor");
        press(12); // up
        press(0); // A: Enter on Single Player
        assert_eq!(crate::menu::menu_screen_id(), 1, "A enters");
        press(1); // B: Escape
        assert_eq!((menu_visible(), crate::menu::menu_screen_id()), (1, 0), "B backs out");
        press(1);
        assert_eq!(menu_visible(), 0, "and closes the menu");
        // A pressed in the game comes up as JOY1 though the menu is up by then.
        pad_frame(1 | 1 << 9, rest); // A and Start down in the game: Start opens the menu
        pad_frame(0, rest);
        assert_eq!(key_is_down(i32::from(quake_rs::keys::K_JOY1)), 0, "nothing stays held");
        press(1);
    }

    /// The mouse turns as far for the same motion at any refresh rate:
    /// IN_MouseMove adds each record's counts as they come, however the
    /// page splits them into events and frames.
    #[test]
    fn mouse_turns_the_same_at_60_and_480_hz() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        use_2026();
        let turn = |hz: u32, per_frame: u32| {
            walk_mut(|w| w.yaw = 0.0);
            for _ in 0..hz {
                for _ in 0..per_frame {
                    mouse_move(480.0 / (hz * per_frame) as f32, 0.0);
                }
                step(1.0 / hz as f32);
            }
            yaw()
        };
        let (at60, at480, at480_split) = (turn(60, 1), turn(480, 1), turn(480, 3));
        assert!((at60 - at480).abs() < 1e-3 && (at480 - at480_split).abs() < 1e-3, "{at60} {at480} {at480_split}");
        assert!((at60 + 480.0 * 0.16).abs() < 1e-2, "0.16° a count at sensitivity 3: {at60}");
    }

    #[test]
    fn lookstrafe_routes_mouse_x_to_sidemove_in_mouse_look() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Default: mouse X turns (yaw changes, no sidemove accumulates).
        let yaw0 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert!(walk_mut(|w| w.yaw) < yaw0, "mouse-right turns right (yaw -= m_yaw*mx)");
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0);

        // Lookstrafe ON (Options row 11): in mouse look, mouse X strafes
        // instead (in_win.c: `lookstrafe.value && (in_mlook.state & 1)`).
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
        assert!(walk_mut(|w| w.yaw) < yaw1, "without mouse look it still turns");
        key_down(i32::from(b'\\')); // +mlook (default.cfg)
        let yaw2 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert_eq!(walk_mut(|w| w.yaw), yaw2, "lookstrafe in mouse look holds the yaw still");
        // sidemove += m_side * (mx * sensitivity 3) = 0.8 * 300 = 240.
        assert_eq!(walk_mut(|w| w.mouse_side), 240.0, "mouse X became sidemove units");
        key_up(i32::from(b'\\'));
        // The accumulator drains into the next frame's cmd.
        step(0.05);
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0, "step drained the strafe units");
    }
}
