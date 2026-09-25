//! The view: head-bob, screen blends, gamma, and where the weapon sits.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/view.c` — `V_CalcBob`, the contents/powerup cshifts and
//! `V_CalcBlend` (applied as `V_UpdatePalette` does), `BuildGammaTable`, and the
//! gun placement of `V_CalcRefdef` / `CalcGunAngle`.

use crate::math::Vec3;
use super::{Camera, Image, IT_INVISIBILITY, IT_INVULNERABILITY, IT_QUAD, IT_SUIT};

/// Quake's `V_CalcBob` (view.c): the sinusoidal head-bob amount (world units) to
/// add to the eye height while moving, so the view rocks up and down with each
/// step. `vel_xy` is the player's horizontal speed (units/sec) and `time` the
/// game clock (seconds). Uses the stock cvar defaults `cl_bob = 0.02`,
/// `cl_bobcycle = 0.6`, `cl_bobup = 0.5`; the result is clamped to `[-7, 4]`
/// exactly as the C. At rest (`vel_xy == 0`) the bob is 0, so a standing view is
/// unchanged. The first-person weapon, being anchored to the camera, stays
/// screen-stable while the world bobs — the classic Quake look.
pub fn view_bob(vel_xy: f32, time: f32) -> f32 {
    use std::f32::consts::PI;
    const CL_BOB: f32 = 0.02;
    const CL_BOBCYCLE: f32 = 0.6;
    const CL_BOBUP: f32 = 0.5;
    if !time.is_finite() || !vel_xy.is_finite() {
        return 0.0;
    }
    // Phase within the bob cycle, in [0, 1).
    let mut cycle = (time - (time / CL_BOBCYCLE).floor() * CL_BOBCYCLE) / CL_BOBCYCLE;
    cycle = if cycle < CL_BOBUP {
        PI * cycle / CL_BOBUP
    } else {
        PI + PI * (cycle - CL_BOBUP) / (1.0 - CL_BOBUP)
    };
    // Bob is proportional to horizontal speed, mostly the sin term.
    let base = vel_xy * CL_BOB;
    let bob = base * 0.3 + base * 0.7 * cycle.sin();
    bob.clamp(-7.0, 4.0)
}

/// The full-screen colour shift for a leaf content type (Quake's `cshift_water`
/// / `cshift_slime` / `cshift_lava` from view.c), as `(rgb, percent)` where
/// `percent` is 0..150. Empty / solid / sky return `None` (no tint).
pub fn content_cshift(contents: i32) -> Option<([u8; 3], f32)> {
    match contents {
        crate::bsp::CONTENTS_WATER => Some(([130, 80, 50], 128.0)),
        crate::bsp::CONTENTS_SLIME => Some(([0, 25, 5], 150.0)),
        crate::bsp::CONTENTS_LAVA => Some(([255, 80, 0], 150.0)),
        _ => None,
    }
}

/// `V_CalcPowerupCshift` (view.c): the full-screen tint while a powerup is held —
/// Quad=blue, Biosuit=green, Ring(invisibility)=gray, Pentagram(invulnerability)=
/// yellow — as `(rgb, percent)`, or `None` with no powerup. id uses an `else if`
/// chain, so the FIRST match wins with priority QUAD > SUIT > INVISIBILITY >
/// INVULNERABILITY (a combined Quad+Pentagram shows the Quad's blue tint).
pub fn powerup_cshift(items: i32) -> Option<([u8; 3], f32)> {
    if items & IT_QUAD != 0 {
        Some(([0, 0, 255], 30.0))
    } else if items & IT_SUIT != 0 {
        Some(([0, 255, 0], 20.0))
    } else if items & IT_INVISIBILITY != 0 {
        Some(([100, 100, 100], 100.0))
    } else if items & IT_INVULNERABILITY != 0 {
        Some(([255, 255, 0], 30.0))
    } else {
        None
    }
}

/// Combine colour shifts `(rgb, percent 0..255)` into a single blend colour and
/// alpha (0..1), porting Quake's `V_CalcBlend` accumulation (each shift is
/// alpha-over the running total). Empty list / all-zero percents give alpha 0.
// `!(percent > 0.0)` is deliberate (a hardened V_CalcBlend skip): it also skips a
// NaN percent, which the clippy-suggested `percent <= 0.0` would let through.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn combine_cshifts(shifts: &[([u8; 3], f32)]) -> ([u8; 3], f32) {
    let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &(color, percent) in shifts {
        if !(percent > 0.0) {
            continue;
        }
        let a2 = (percent / 255.0).clamp(0.0, 1.0);
        a += a2 * (1.0 - a);
        if a <= 0.0 {
            continue;
        }
        let an = (a2 / a).clamp(0.0, 1.0); // share of the new colour in the mix
        r = r * (1.0 - an) + color[0] as f32 * an;
        g = g * (1.0 - an) + color[1] as f32 * an;
        b = b * (1.0 - an) + color[2] as f32 * an;
    }
    let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    ([to_u8(r), to_u8(g), to_u8(b)], a.clamp(0.0, 1.0))
}

/// Blend `color` over every pixel of `image` at `alpha` (0..1) — the full-screen
/// polyblend (damage flash, underwater/lava/slime tint). `alpha <= 0` is a no-op.
/// Software Quake's `V_UpdatePalette` runs LAST in `SCR_UpdateScreen` and shifts the
/// whole VGA palette, so the tint covers the ENTIRE composited screen — 3-D view,
/// status bar, centerprint, menu and console alike. Apply this to the FINISHED frame
/// after every overlay, not just the 3-D viewport (which would be the GL look).
// `!(alpha > 0.0)` is deliberate: a NaN alpha must also be a no-op, which the
// clippy-suggested `alpha <= 0.0` would not guarantee.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn apply_blend(image: &mut Image, color: [u8; 3], alpha: f32) {
    if !(alpha > 0.0) {
        return;
    }
    let a = alpha.min(1.0);
    let inv = 1.0 - a;
    for px in image.rgb.iter_mut() {
        for c in 0..3 {
            px[c] = (px[c] as f32 * inv + color[c] as f32 * a).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// CalcGunAngle's `cl.viewent.angles` (view.c) for a camera built from the
/// view angles plus `punch` (`cl.punchangle`, QuakeC order: pitch +down, yaw,
/// roll): the camera's pitch and yaw with the punch taken back out, and the
/// client's own view roll `view_roll` (`cl.viewangles[ROLL]`, 0 in play) in
/// place of the camera's lean. (The yaw/pitch lag terms of CalcGunAngle are
/// always 0 — it subtracts the view angles from themselves — and the idle
/// sway is 0 at `v_idlescale 0`.)
pub fn viewmodel_angles(cam: &Camera, punch: Vec3, view_roll: f32) -> Vec3 {
    [cam.pitch + punch[0], cam.yaw - punch[1], view_roll]
}

/// V_CalcRefdef's viewsize "fudge" (view.c): "fudge position around to keep
/// amount of weapon visible roughly equal with different FOV" — the gun is
/// raised 1 unit at viewsize 110, 2 at 100, 1 at 90 and 0.5 at 80 (exact
/// compares on the cvar, as in the C), 0 otherwise.
pub fn viewmodel_fudge(viewsize: f32) -> f32 {
    if viewsize == 110.0 {
        1.0
    } else if viewsize == 100.0 {
        2.0
    } else if viewsize == 90.0 {
        1.0
    } else if viewsize == 80.0 {
        0.5
    } else {
        0.0
    }
}

/// The gun origin relative to the camera, as V_CalcRefdef builds it: both
/// start at the entity origin + `viewheight` + the vertical bob (so those
/// cancel); the camera then gets the 1/32 "never sit exactly on a node line"
/// epsilon on each axis and the gun does not, the gun moves `forward * bob *
/// 0.4` — `forward` from the player entity's angles, which V_CalcRefdef has
/// just set to the view's yaw and pitch (`ent->angles[PITCH] =
/// -cl.viewangles[PITCH]`) — and up by the viewsize fudge
/// ([`viewmodel_fudge`]). `gun_angles` are [`Viewmodel::angles`]; they differ
/// from `cl.viewangles` only by a demo's damage-kick pitch (an accepted
/// hundredth-of-a-unit gap). The epsilon is relative, so it is right whether or
/// not the caller's camera carries it.
pub fn viewmodel_origin_ofs(gun_angles: Vec3, bob: f32, viewsize: f32) -> Vec3 {
    const EPSILON: f32 = 1.0 / 32.0;
    let yaw = (gun_angles[1] as f64).to_radians();
    let elev = (gun_angles[0] as f64).to_radians();
    let f = (bob * 0.4) as f64;
    [
        (f * elev.cos() * yaw.cos()) as f32 - EPSILON,
        (f * elev.cos() * yaw.sin()) as f32 - EPSILON,
        (f * elev.sin()) as f32 + viewmodel_fudge(viewsize) - EPSILON,
    ]
}

/// Build the 256-entry gamma LUT, a port of `BuildGammaTable` (view.c):
/// `gammatable[i] = 255 * pow((i+0.5)/255.5, g) + 0.5`, clamped to `0..=255` —
/// and the C's exact `g == 1.0` special case, a literal identity table (so the
/// default gamma is BYTE-EXACT, not merely close). The host applies this where
/// the finished frame becomes presented RGB, the same boundary as the C's
/// `V_UpdatePalette` -> `VID_ShiftPalette` hardware-palette write (gamma there
/// runs AFTER the cshift blend; the host matches that order). quaketool's PPM
/// scene path never applies it (the C's default boot state), so the golden
/// renders are untouched.
pub fn build_gamma_table(g: f32) -> [u8; 256] {
    let mut table = [0u8; 256];
    if g == 1.0 {
        for (i, t) in table.iter_mut().enumerate() {
            *t = i as u8;
        }
        return table;
    }
    for (i, t) in table.iter_mut().enumerate() {
        // The C computes pow in double and truncates the +0.5-rounded value to
        // int, then clamps; mirror that exactly.
        let inf = (255.0 * ((i as f64 + 0.5) / 255.5).powf(g as f64) + 0.5) as i32;
        *t = inf.clamp(0, 255) as u8;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_bob_is_zero_at_rest_and_oscillates_when_moving() {
        // Standing still: no bob, at any time.
        for &t in &[0.0, 0.13, 0.5, 1.7, 42.0] {
            assert_eq!(view_bob(0.0, t), 0.0, "rest bob must be 0 at t={t}");
        }
        // Moving: the bob is bounded to [-7, 4] and actually varies over a cycle
        // (it is a sinusoid of the phase), so min and max across a cycle differ.
        let speed = 320.0; // typical run speed
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for i in 0..120 {
            let t = i as f32 * 0.01; // sweep ~2 bob cycles (cl_bobcycle = 0.6)
            let b = view_bob(speed, t);
            assert!(b.is_finite());
            assert!((-7.0..=4.0).contains(&b), "bob {b} out of clamp range");
            lo = lo.min(b);
            hi = hi.max(b);
        }
        assert!(hi - lo > 0.5, "bob should oscillate over a cycle (got {lo}..{hi})");
        // Faster movement bobs at least as hard as slower (monotone in speed at a
        // fixed phase where sin is positive).
        let t = 0.15; // within the bob-up half, sin(cycle) > 0
        assert!(view_bob(320.0, t) > view_bob(80.0, t));
    }

    #[test]
    fn screen_blend_damage_tint_and_apply() {
        // Content shifts: water/slime/lava tint, empty/solid none.
        assert_eq!(content_cshift(crate::bsp::CONTENTS_WATER), Some(([130, 80, 50], 128.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_LAVA), Some(([255, 80, 0], 150.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_EMPTY), None);

        // No shifts => fully transparent.
        let (_c, a0) = combine_cshifts(&[]);
        assert_eq!(a0, 0.0);

        // A red damage shift gives a reddish blend with partial alpha.
        let (c, a) = combine_cshifts(&[([255, 0, 0], 150.0)]);
        assert!(a > 0.0 && a < 1.0, "alpha {a} should be partial");
        assert!(c[0] > c[1] && c[0] > c[2], "blend should be reddish, got {c:?}");

        // apply_blend with alpha 0 is a no-op; with alpha>0 it moves pixels toward
        // the blend colour.
        let mut img = Image { w: 2, h: 1, rgb: vec![[10, 10, 10], [10, 10, 10]] };
        apply_blend(&mut img, [255, 0, 0], 0.0);
        assert_eq!(img.rgb[0], [10, 10, 10], "alpha 0 must not change pixels");
        apply_blend(&mut img, [255, 0, 0], 0.5);
        assert!(img.rgb[0][0] > 100 && img.rgb[0][1] < 10, "red 0.5 blend: {:?}", img.rgb[0]);
    }

    #[test]
    fn viewmodel_origin_follows_v_calcrefdef() {
        // The viewsize fudge: exact compares on the cvar, as in view.c.
        assert_eq!(viewmodel_fudge(100.0), 2.0);
        assert_eq!(viewmodel_fudge(110.0), 1.0);
        assert_eq!(viewmodel_fudge(90.0), 1.0);
        assert_eq!(viewmodel_fudge(80.0), 0.5);
        for vs in [30.0, 50.0, 70.0, 120.0, 95.0] {
            assert_eq!(viewmodel_fudge(vs), 0.0, "viewsize {vs}");
        }
        // No bob: the gun sits above the eye by the fudge (world Z), less the
        // camera's 1/32 node-line epsilon on every axis.
        const E: f32 = 1.0 / 32.0;
        assert_eq!(viewmodel_origin_ofs([30.0, 37.0, 0.0], 0.0, 100.0), [-E, -E, 2.0 - E]);
        // Bob pushes it along the view's facing (V_CalcRefdef has just set the
        // entity angles to the view's) by 0.4 * bob.
        let o = viewmodel_origin_ofs([0.0, 90.0, 0.0], 5.0, 120.0);
        assert!((o[0] + E).abs() < 1e-5 && (o[1] - 2.0 + E).abs() < 1e-5 && (o[2] + E).abs() < 1e-5, "{o:?}");
        let o = viewmodel_origin_ofs([90.0, 0.0, 0.0], 5.0, 120.0);
        assert!((o[2] - 2.0 + E).abs() < 1e-5 && (o[0] + E).abs() < 1e-5, "{o:?}");
        // CalcGunAngle: the punch comes back out of the camera's pitch and yaw;
        // the gun takes the client's own roll, not the camera's lean.
        let cam = Camera { pos: [0.0; 3], yaw: 40.0, pitch: -12.0, roll: 3.0, fov_deg: 90.0 };
        assert_eq!(viewmodel_angles(&cam, [2.0, 1.0, 0.5], 0.0), [-10.0, 39.0, 0.0]);
    }

    #[test]
    fn gamma_table_identity_at_one_and_curve_below() {
        // BuildGammaTable's g == 1.0 special case is a literal identity — the
        // host skips the LUT entirely there, so default gamma is byte-exact.
        let id = build_gamma_table(1.0);
        for (i, &v) in id.iter().enumerate() {
            assert_eq!(v as usize, i, "gamma 1.0 must be the identity at {i}");
        }
        // Below 1.0 the curve BRIGHTENS (x^g > x for x in (0,1), g < 1) and is
        // monotonic; the top end stays pinned by the clamp.
        let g = build_gamma_table(0.5);
        assert_eq!(g[255], 255);
        for i in 1..255usize {
            assert!(g[i] >= id[i], "gamma 0.5 must brighten every level ({i})");
            assert!(g[i] >= g[i - 1], "gamma table must be monotonic ({i})");
        }
        // The C's exact formula spot-check: i=64, g=0.5 ->
        // 255*sqrt(64.5/255.5)+0.5 = 128.6... -> truncates to 128.
        assert_eq!(g[64], 128);
        // ...and the bottom level: 255*sqrt(0.5/255.5)+0.5 = 11.78 -> 11.
        assert_eq!(g[0], 11);
    }
}
