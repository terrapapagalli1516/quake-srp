//! The client side of view.c the live and demo frames share: `V_ParseDamage`
//! (the damage colour shift, the directional view kick and the status-bar
//! pain face an `svc_damage` starts), `V_CalcViewRoll`'s kick, and
//! `V_BonusFlash_f` (the gold pickup flash a stuffed `bf` starts). The rest of
//! view.c the renderer uses (`V_CalcBob`, the cshift ramps, the gun placement)
//! is [`crate::render`]'s `view`.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/view.c`.

use crate::math::{Vec3, angle_vectors, dot, normalize};
use crate::stepping::{ID_FRAMETIME, Stepping, Tick72};

/// `v_kicktime` (view.c, default "0.5"): how long an svc_damage view kick lasts.
pub const V_KICKTIME: f32 = 0.5;
/// `v_kickroll` (view.c, default "0.6"): roll degrees per damage count*side.
pub const V_KICKROLL: f32 = 0.6;
/// `v_kickpitch` (view.c, default "0.6"): pitch degrees per damage count*side.
pub const V_KICKPITCH: f32 = 0.6;
/// `V_ParseDamage`'s `cl.faceanimtime = cl.time + 0.2`: how long the status
/// bar shows the pain face after a hit.
pub const FACE_ANIM_TIME: f32 = 0.2;

/// `V_BonusFlash_f` (view.c): `cl.cshifts[CSHIFT_BONUS]` becomes this colour at
/// [`BONUS_PERCENT`]; V_UpdatePalette drops it by `host_frametime*100`.
pub const BONUS_COLOR: [u8; 3] = [215, 186, 69];
pub const BONUS_PERCENT: f32 = 50.0;
/// V_UpdatePalette's bonus drop per second (`host_frametime*100`).
pub const BONUS_FADE: f32 = 100.0;
/// V_UpdatePalette's damage drop per second (`host_frametime*150`).
pub const DAMAGE_FADE: f32 = 150.0;

/// `cl.cshifts[CSHIFT_DAMAGE].percent += 3*count` then the 0..150 clamp
/// (V_ParseDamage). `percent` is an `int` in the C (client.h `cshift_t`), so
/// the float sum truncates back to a whole percent.
pub fn cshift_add(percent: f32, add: f32) -> f32 {
    ((percent + add) as i32).clamp(0, 150) as f32
}

/// V_UpdatePalette's per-frame drop, `percent -= host_frametime*rate; if
/// (percent <= 0) percent = 0;` — on an `int`, so every frame truncates: the
/// damage flash loses 3 a frame at 72 fps (not 2.08), the bonus flash 2.
pub fn cshift_drop(percent: f32, frametime: f32, rate: f32) -> f32 {
    let p = (percent - frametime * rate) as i32;
    if p <= 0 { 0.0 } else { p as f32 }
}

/// V_UpdatePalette's damage and bonus fades over a frame of `frametime`,
/// stepped as `stepping` says. Classic: id's one [`cshift_drop`] a frame —
/// which loses at least a whole percent every frame, so uncapped at 480 Hz
/// the flashes fade two to three times as fast as at 72. Uncapped: one of
/// id's 72 Hz drops per whole 1/72 s `clock` counts, so the flashes last as
/// long as id's at any rate.
pub fn fade_cshifts(damage: &mut f32, bonus: &mut f32, frametime: f32, stepping: Stepping, clock: &mut Tick72) {
    match stepping {
        Stepping::Classic => {
            *damage = cshift_drop(*damage, frametime, DAMAGE_FADE);
            *bonus = cshift_drop(*bonus, frametime, BONUS_FADE);
        }
        Stepping::Uncapped => {
            for _ in 0..clock.ticks(frametime) {
                *damage = cshift_drop(*damage, ID_FRAMETIME, DAMAGE_FADE);
                *bonus = cshift_drop(*bonus, ID_FRAMETIME, BONUS_FADE);
            }
        }
    }
}

/// Run server-stuffed text (`svc_stufftext`: `Cbuf_AddText`, then
/// `Cbuf_Execute` splits it into commands at `;` and newlines) for the only
/// command id1 stuffs, `bf` (V_BonusFlash_f — every item pickup and
/// CheckPowerups, 16 `stuffcmd` sites): true when it contains one. Other
/// commands are ignored. Accepted gap: the C's Cbuf_Execute runs the text at
/// the start of the NEXT host frame, so id's flash starts one frame (~14 ms)
/// later than here.
pub fn stufftext_bonus_flash(text: &str) -> bool {
    text.split(['\n', ';']).any(|cmd| cmd.split_whitespace().next() == Some("bf"))
}

/// `CL_ParseClientdata` (cl_parse.c:549): when `cl.items` changes, every bit
/// newly set gets `cl.item_gettime[j] = cl.time` (the status bar flashes the
/// new weapon's icon for a second).
///
/// A level (or demo) start does NOT flash what the player carries.
/// `CL_ClearState` zeroes `cl.items` but also `cl.time`, and the first
/// clientdata (Host_Spawn_f's, in the signon) is parsed in
/// `CL_ReadFromServer` after `cl.time += host_frametime` and BEFORE
/// `CL_RelinkEntities`' `CL_LerpPoint` snaps `cl.time` to the server's
/// message time: every owned bit is stamped at about `host_frametime`, and by
/// the first drawn frame `cl.time` is at least SV_SpawnServer's 1.0 plus its
/// two 0.1 s frames, so `(int)((cl.time - item_gettime)*10)` is already past
/// 10. Every `CL_ClearState` site here therefore seeds the last items with
/// the spawn items and leaves the get-times at 0, which draws the same bar.
pub fn stamp_item_gettime(cl_items: &mut i32, gettime: &mut [f32; 32], items: i32, time: f32) {
    if items == *cl_items {
        return;
    }
    let (new, old) = (items as u32, *cl_items as u32);
    for (j, t) in gettime.iter_mut().enumerate() {
        if new & (1 << j) != 0 && old & (1 << j) == 0 {
            *t = time;
        }
    }
    *cl_items = items;
}

/// What one `svc_damage` does to the view (`V_ParseDamage`, view.c).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedDamage {
    /// `3*count`, what `cl.cshifts[CSHIFT_DAMAGE].percent` gains (the caller
    /// clamps to 0..150).
    pub percent: f32,
    /// `cshifts[CSHIFT_DAMAGE].destcolor`: armour-dominant pink, some armour
    /// orange-red, pure blood red.
    pub color: [u8; 3],
    /// `v_dmg_roll` / `v_dmg_pitch`: the kick, from the attack's side.
    pub roll: f32,
    pub pitch: f32,
}

/// `V_ParseDamage`: `armor`/`blood` are the message's bytes, `from` the
/// attack origin, `ent_origin` the view entity's origin and `viewangles`
/// `cl.viewangles` (pitch +down, yaw, roll). The kick uses the view entity's
/// angles as V_CalcRefdef keeps them — `YAW = viewangles[YAW]`, `PITCH =
/// -viewangles[PITCH]` (entity pitch is stored backwards), roll ~0 — so
/// AngleVectors sees the pitch mirrored, as in the C.
pub fn parse_damage(armor: i32, blood: i32, from: Vec3, ent_origin: Vec3, viewangles: Vec3) -> ParsedDamage {
    let count = (blood as f32 * 0.5 + armor as f32 * 0.5).max(10.0);
    let color = if armor > blood {
        [200, 100, 100]
    } else if armor > 0 {
        [220, 50, 50]
    } else {
        [255, 0, 0]
    };
    let delta = [from[0] - ent_origin[0], from[1] - ent_origin[1], from[2] - ent_origin[2]];
    let (from_dir, _len) = normalize(delta);
    let (forward, right, _up) = angle_vectors([-viewangles[0], viewangles[1], 0.0]);
    ParsedDamage {
        percent: 3.0 * count,
        color,
        roll: count * dot(from_dir, right) * V_KICKROLL,
        pitch: count * dot(from_dir, forward) * V_KICKPITCH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cshift_percents_are_ints() {
        // 150 at 72 fps: 150 - 2.083 = 147.9 -> 147 (the C's int), 3 a frame.
        assert_eq!(cshift_drop(150.0, 1.0 / 72.0, DAMAGE_FADE), 147.0);
        assert_eq!(cshift_drop(50.0, 1.0 / 72.0, BONUS_FADE), 48.0);
        assert_eq!(cshift_drop(2.0, 1.0 / 72.0, DAMAGE_FADE), 0.0);
        // count 10.5 -> 3*count 31.5 -> 31; the clamp at 150.
        assert_eq!(cshift_add(0.0, 31.5), 31.0);
        assert_eq!(cshift_add(140.0, 30.0), 150.0);
    }

    /// How long flashes starting at `damage` and `bonus` percent show at `hz`.
    fn flash_frames(hz: f32, stepping: Stepping, damage: f32, bonus: f32) -> (f32, f32) {
        let (mut d, mut b, mut clock) = (damage, bonus, Tick72::default());
        let (mut td, mut tb, mut t) = (0.0, 0.0, 0.0);
        while d > 0.0 || b > 0.0 {
            fade_cshifts(&mut d, &mut b, 1.0 / hz, stepping, &mut clock);
            t += 1.0 / hz;
            td = if d > 0.0 { t } else { td };
            tb = if b > 0.0 { t } else { tb };
        }
        (td, tb)
    }

    /// The uncapped fades last as id's do at 72 Hz, to one 72 Hz tick, at
    /// 60, 144 and 480 Hz; id's per-frame fade does not (at 480 Hz a bonus
    /// flash is gone in a third of the time).
    #[test]
    fn uncapped_flashes_fade_like_72_hz() {
        let (d72, b72) = flash_frames(72.0, Stepping::Classic, 37.0, BONUS_PERCENT);
        assert!((d72 - 12.0 / 72.0).abs() < 1e-4 && (b72 - 24.0 / 72.0).abs() < 1e-4, "{d72} {b72}");
        for hz in [60.0, 144.0, 480.0] {
            let (d, b) = flash_frames(hz, Stepping::Uncapped, 37.0, BONUS_PERCENT);
            assert!((d - d72).abs() <= 1.0 / 72.0 + 1e-4, "{hz} Hz damage flash {d} vs {d72}");
            assert!((b - b72).abs() <= 1.0 / 72.0 + 1e-4, "{hz} Hz bonus flash {b} vs {b72}");
        }
        let (_, b480) = flash_frames(480.0, Stepping::Classic, 37.0, BONUS_PERCENT);
        assert!(b480 < b72 / 2.0, "id's per-frame fade at 480 Hz: {b480}");
    }

    #[test]
    fn item_gettime_stamps_only_newly_set_bits() {
        let (mut items, mut gt) = (0, [0.0f32; 32]);
        stamp_item_gettime(&mut items, &mut gt, 0b11 | 1 << 31, 1.5);
        assert_eq!((gt[0], gt[1], gt[31], gt[2]), (1.5, 1.5, 1.5, 0.0));
        stamp_item_gettime(&mut items, &mut gt, 0b111 | 1 << 31, 7.0);
        assert_eq!((gt[0], gt[2]), (1.5, 7.0), "only the new bit is stamped");
        stamp_item_gettime(&mut items, &mut gt, 0b101, 9.0);
        stamp_item_gettime(&mut items, &mut gt, 0b111, 9.5);
        assert_eq!(gt[1], 9.5, "lost and got again: stamped again");
    }

    #[test]
    fn stufftext_runs_bf_commands() {
        assert!(stufftext_bonus_flash("bf\n"));
        assert!(stufftext_bonus_flash("echo hi; bf"));
        assert!(!stufftext_bonus_flash("bfx\n"));
        assert!(!stufftext_bonus_flash("reconnect\n"));
    }

    #[test]
    fn parse_damage_counts_colours_and_kicks_like_v_parse_damage() {
        // 4 blood, no armour: count floors at 10 -> percent 30, pure red; hit
        // from straight ahead of a level view: all pitch, no roll.
        let d = parse_damage(0, 4, [100.0, 0.0, 0.0], [0.0; 3], [0.0; 3]);
        assert_eq!(d.percent, 30.0);
        assert_eq!(d.color, [255, 0, 0]);
        assert!((d.pitch - 10.0 * V_KICKPITCH).abs() < 1e-4 && d.roll.abs() < 1e-4);
        // Armour took most: pink; count (30+10)/2 = 20; hit from the right
        // (yaw 0: right is -y) rolls the view.
        let d = parse_damage(30, 10, [0.0, -100.0, 0.0], [0.0; 3], [0.0; 3]);
        assert_eq!(d.percent, 60.0);
        assert_eq!(d.color, [200, 100, 100]);
        assert!((d.roll - 20.0 * V_KICKROLL).abs() < 1e-4 && d.pitch.abs() < 1e-4);
        // Some armour but less than blood: orange-red.
        assert_eq!(parse_damage(5, 20, [1.0, 0.0, 0.0], [0.0; 3], [0.0; 3]).color, [220, 50, 50]);
    }
}
