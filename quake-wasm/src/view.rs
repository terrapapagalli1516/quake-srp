//! The client side of view.c the live and demo frames share: `V_ParseDamage`
//! (the damage colour shift, the directional view kick and the status-bar
//! pain face an `svc_damage` starts), `V_CalcViewRoll`'s kick, and
//! `V_BonusFlash_f` (the gold pickup flash a stuffed `bf` starts).

use quake_rs::math::{angle_vectors, dot, normalize, Vec3};

/// `v_kicktime` (view.c, default "0.5"): how long an svc_damage view kick lasts.
pub(crate) const V_KICKTIME: f32 = 0.5;
/// `v_kickroll` (view.c, default "0.6"): roll degrees per damage count*side.
pub(crate) const V_KICKROLL: f32 = 0.6;
/// `v_kickpitch` (view.c, default "0.6"): pitch degrees per damage count*side.
pub(crate) const V_KICKPITCH: f32 = 0.6;
/// `V_ParseDamage`'s `cl.faceanimtime = cl.time + 0.2`: how long the status
/// bar shows the pain face after a hit.
pub(crate) const FACE_ANIM_TIME: f32 = 0.2;

/// `V_BonusFlash_f` (view.c): `cl.cshifts[CSHIFT_BONUS]` becomes this colour at
/// [`BONUS_PERCENT`]; V_UpdatePalette drops it by `host_frametime*100`.
pub(crate) const BONUS_COLOR: [u8; 3] = [215, 186, 69];
pub(crate) const BONUS_PERCENT: f32 = 50.0;
/// V_UpdatePalette's bonus drop per second (`host_frametime*100`).
pub(crate) const BONUS_FADE: f32 = 100.0;

/// Run server-stuffed text (`svc_stufftext`: `Cbuf_AddText`, then
/// `Cbuf_Execute` splits it into commands at `;` and newlines) for the only
/// command id1 stuffs, `bf` (V_BonusFlash_f — every item pickup and
/// CheckPowerups, 16 `stuffcmd` sites): true when it contains one. Other
/// commands are ignored. Accepted gap: the C's Cbuf_Execute runs the text at
/// the start of the NEXT host frame, so id's flash starts one frame (~14 ms)
/// later than here.
pub(crate) fn stufftext_bonus_flash(text: &str) -> bool {
    text.split(['\n', ';']).any(|cmd| cmd.split_whitespace().next() == Some("bf"))
}

/// `CL_ParseClientdata` (cl_parse.c:549): when `cl.items` changes, every bit
/// newly set gets `cl.item_gettime[j] = cl.time` (the status bar flashes the
/// new weapon's icon for a second). `CL_ClearState` zeroes `cl.items`, so a
/// level (or demo) start stamps everything owned.
pub(crate) fn stamp_item_gettime(cl_items: &mut i32, gettime: &mut [f32; 32], items: i32, time: f32) {
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
pub(crate) struct ParsedDamage {
    /// `3*count`, what `cl.cshifts[CSHIFT_DAMAGE].percent` gains (the caller
    /// clamps to 0..150).
    pub(crate) percent: f32,
    /// `cshifts[CSHIFT_DAMAGE].destcolor`: armour-dominant pink, some armour
    /// orange-red, pure blood red.
    pub(crate) color: [u8; 3],
    /// `v_dmg_roll` / `v_dmg_pitch`: the kick, from the attack's side.
    pub(crate) roll: f32,
    pub(crate) pitch: f32,
}

/// `V_ParseDamage`: `armor`/`blood` are the message's bytes, `from` the
/// attack origin, `ent_origin` the view entity's origin and `viewangles`
/// `cl.viewangles` (pitch +down, yaw, roll). The kick uses the view entity's
/// angles as V_CalcRefdef keeps them — `YAW = viewangles[YAW]`, `PITCH =
/// -viewangles[PITCH]` (entity pitch is stored backwards), roll ~0 — so
/// AngleVectors sees the pitch mirrored, as in the C.
pub(crate) fn parse_damage(
    armor: i32,
    blood: i32,
    from: Vec3,
    ent_origin: Vec3,
    viewangles: Vec3,
) -> ParsedDamage {
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
