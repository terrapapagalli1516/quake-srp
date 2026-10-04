//! The view: head-bob, screen blends, gamma, and where the weapon sits.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/view.c` — `V_CalcBob`, the contents/powerup cshifts and
//! the software `V_UpdatePalette`'s ramps, `BuildGammaTable`, and the gun
//! placement of `V_CalcRefdef` / `CalcGunAngle`.

use crate::math::Vec3;
use crate::sbar::{IT_INVISIBILITY, IT_INVULNERABILITY, IT_QUAD, IT_SUIT};
use super::{Camera, Image, Palette};

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

/// `V_SetContentsColor` (view.c): the full-screen colour shift for the view
/// leaf's contents (`cshift_empty` / `cshift_lava` / `cshift_slime` /
/// `cshift_water`), as `(rgb, percent)` where `percent` is 0..150. Empty and
/// solid have none; the C's `default:` is water — every other contents, sky
/// included (an eye in a sky volume, noclip only, is water-tinted).
pub fn content_cshift(contents: i32) -> Option<([u8; 3], f32)> {
    match contents {
        crate::bsp::CONTENTS_EMPTY | crate::bsp::CONTENTS_SOLID => None,
        crate::bsp::CONTENTS_SLIME => Some(([0, 25, 5], 150.0)),
        crate::bsp::CONTENTS_LAVA => Some(([255, 80, 0], 150.0)),
        _ => Some(([130, 80, 50], 128.0)),
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

/// `V_UpdatePalette` (view.c, the software build's — not GLQuake's
/// `V_CalcBlend`): the frame's colour shifts `(destcolor, percent)`, in
/// `cl.cshifts` order (CONTENTS, DAMAGE, BONUS, POWERUP), with the gamma
/// table after them, as one 256-entry ramp per channel. Every level `v` is
/// moved toward each shift's colour in turn with the C's integer arithmetic,
///
/// ```text
/// v += (percent * (destcolor - v)) >> 8;
/// ```
///
/// where `percent` is `cshift_t`'s `int` (a fractional percent truncates)
/// and `>>` is an arithmetic shift (a negative step rounds toward minus
/// infinity), and then becomes `gammatable[v]`. The C runs this over the
/// 256 palette colours and hands the result to `VID_ShiftPalette`. A channel
/// only ever depends on itself, so a palette colour looked up in these ramps
/// channel by channel IS the C's shifted palette entry ([`FramePalette`]),
/// through which the whole screen is shown (3-D view, status bar, menu,
/// console), exactly as the palette shift tints it. No shifts and the
/// identity gamma give the identity ramps.
pub fn cshift_ramps(shifts: &[([u8; 3], f32)], gamma: &[u8; 256]) -> [[u8; 256]; 3] {
    let mut ramps = [[0u8; 256]; 3];
    for (c, ramp) in ramps.iter_mut().enumerate() {
        for (i, out) in ramp.iter_mut().enumerate() {
            let mut v = i as i32;
            for &(dest, percent) in shifts {
                // `as` truncates like the C's float-to-int (a NaN reads as 0);
                // the clamp to client.h's "0-256" keeps the product in range
                // and never moves a real percent (they top out at 150).
                let p = (percent as i32).clamp(0, 256);
                v += (p * (dest[c] as i32 - v)) >> 8;
            }
            // With p in 0..=256, v stays between its start and the colour.
            *out = gamma[v.clamp(0, 255) as usize];
        }
    }
    ramps
}

/// The palette a frame is shown with, as the display's DAC holds it:
/// `V_UpdatePalette` over `gfx/palette.lmp` — the frame's colour shifts, then
/// gamma ([`cshift_ramps`]) — as handed to `VID_ShiftPalette`. 256 RGBA
/// colours (alpha 255), the form presentation takes them in: 1024 bytes for
/// a 256x1 texture, or one 4-byte store per pixel ([`pack_rgba`]).
///
/// A shifted colour is its base colour looked up channel by channel in the
/// ramps, which is what the C computes for each of the 256 entries; the
/// frame's pixels never change, only the palette they are shown through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramePalette(pub [[u8; 4]; 256]);

impl FramePalette {
    /// `V_UpdatePalette` for a frame: `base` through the cshifts `shifts` (in
    /// `cl.cshifts` order) and the gamma table. No shifts at the identity
    /// gamma leave `base` as it is.
    #[must_use]
    pub fn new(base: &Palette, shifts: &[([u8; 3], f32)], gamma: &[u8; 256]) -> FramePalette {
        let [r, g, b] = cshift_ramps(shifts, gamma);
        FramePalette(base.map(|[cr, cg, cb]| [r[usize::from(cr)], g[usize::from(cg)], b[usize::from(cb)], 255]))
    }

    /// The 256 colours as bytes, RGBA per entry.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 1024] {
        std::array::from_fn(|i| self.0[i / 4][i % 4])
    }
}

/// `VID_Update` through a DAC, in software: the finished 8-bit `image` into
/// `out` as RGBA through `palette` — one 4-byte store per pixel, whatever
/// the shifts — in row runs on up to `threads` threads. Each pixel is its own
/// lookup, so the bytes are the same for any thread count.
pub fn pack_rgba(image: &Image, palette: &FramePalette, out: &mut Vec<u8>, threads: usize) {
    let (w, h) = (image.w, image.h);
    let n = w.saturating_mul(h).min(image.pixels.len());
    super::resize_frame_buffer(out, n * 4, 4, 255);
    super::band::map_rows(threads, h, out, w * 4, &image.pixels[..n], w, |out, pixels| {
        for (o, &px) in out.chunks_exact_mut(4).zip(pixels) {
            o.copy_from_slice(&palette.0[usize::from(px)]);
        }
    });
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
/// ([`viewmodel_fudge`]). `gun_angles` are [`Viewmodel::angles`](super::alias::Viewmodel::angles); they differ
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
/// default gamma is BYTE-EXACT, not merely close). The host applies it to the
/// palette the frame is shown through ([`FramePalette`]), the C's
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
    fn content_cshifts_are_view_c_s() {
        assert_eq!(content_cshift(crate::bsp::CONTENTS_WATER), Some(([130, 80, 50], 128.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_SLIME), Some(([0, 25, 5], 150.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_LAVA), Some(([255, 80, 0], 150.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_EMPTY), None);
        assert_eq!(content_cshift(crate::bsp::CONTENTS_SOLID), None);
        // V_SetContentsColor's `default:` is water — sky included.
        assert_eq!(content_cshift(crate::bsp::CONTENTS_SKY), Some(([130, 80, 50], 128.0)));
        assert_eq!(powerup_cshift(IT_QUAD | IT_INVULNERABILITY), Some(([0, 0, 255], 30.0)));
        assert_eq!(powerup_cshift(0), None);
    }

    /// The 8-bit frame shown through its palette is, byte for byte, the RGB
    /// frame packed through the ramps channel by channel (the pack before
    /// the frame went 8-bit), whatever the shifts and the gamma.
    #[test]
    fn the_palette_pack_is_the_rgb_frames_ramp_pack() {
        let base: Palette = std::array::from_fn(|i| [i as u8, (i * 7 + 3) as u8, (255 - i) as u8]);
        let image = Image { w: 16, h: 16, pixels: (0..=255u8).collect() };
        let water = ([130, 80, 50], 128.0);
        let quad = ([0, 0, 255], 30.0);
        for (shifts, g) in [(vec![], 1.0), (vec![water], 1.0), (vec![water, ([255, 0, 0], 150.0), quad], 0.7)] {
            let gamma = build_gamma_table(g);
            let [r, gr, b] = cshift_ramps(&shifts, &gamma);
            let old: Vec<u8> = image.to_rgb(&base).pixels.iter().flat_map(|p| [r[p[0] as usize], gr[p[1] as usize], b[p[2] as usize], 255]).collect();
            for threads in [1, 3] {
                let mut new = Vec::new();
                pack_rgba(&image, &FramePalette::new(&base, &shifts, &gamma), &mut new, threads);
                assert_eq!(new, old, "{} shifts, gamma {g}, {threads} threads", shifts.len());
            }
        }
        let unshifted = FramePalette(base.map(|[r, g, b]| [r, g, b, 255]));
        assert_eq!(FramePalette::new(&base, &[], &build_gamma_table(1.0)), unshifted, "no shift: VID_SetPalette's");
    }

    #[test]
    fn cshift_ramps_are_v_update_palettes_integer_steps() {
        let id = build_gamma_table(1.0);
        let identity: [[u8; 256]; 3] = [id, id, id];
        // No shifts (or only zero percents) and gamma 1: the identity.
        assert_eq!(cshift_ramps(&[], &id), identity);
        assert_eq!(cshift_ramps(&[([255, 0, 0], 0.0), ([9, 9, 9], 0.9)], &id), identity);
        // The damage flash at its cap, by hand from the C:
        //   r: 10 + (150*(255-10) >> 8) = 10 + (36750 >> 8) = 10 + 143 = 153
        //   g: 10 + (150*(0-10)   >> 8) = 10 + (-1500 >> 8) = 10 - 6   = 4
        // (the float alpha-blend this replaces gave 154 and 4).
        let dmg = cshift_ramps(&[([255, 0, 0], 150.0)], &id);
        assert_eq!((dmg[0][10], dmg[1][10], dmg[2][10]), (153, 4, 4));
        assert_eq!((dmg[0][255], dmg[1][0]), (255, 0), "the ends stay in range");
        // cshift_t.percent is an int: 22.9 steps like 22.
        assert_eq!(cshift_ramps(&[([255, 0, 0], 22.9)], &id), cshift_ramps(&[([255, 0, 0], 22.0)], &id));
        // The shifts apply in order. Level 100, red, water then damage:
        //   100 + (128*30 >> 8) = 115;  115 + (150*140 >> 8) = 115 + 82 = 197
        // and the other way round:
        //   100 + (150*155 >> 8) = 190; 190 + (128*-60 >> 8) = 190 - 30 = 160.
        let water = ([130, 80, 50], 128.0);
        let damage = ([255, 0, 0], 150.0);
        assert_eq!(cshift_ramps(&[water, damage], &id)[0][100], 197);
        assert_eq!(cshift_ramps(&[damage, water], &id)[0][100], 160);
        // Gamma comes last: gammatable[shifted level].
        let g = build_gamma_table(0.7);
        let lit = cshift_ramps(&[water, damage], &g);
        let shifted = cshift_ramps(&[water, damage], &id);
        for (c, (lit, shifted)) in lit.iter().zip(&shifted).enumerate() {
            for (i, (&l, &s)) in lit.iter().zip(shifted).enumerate() {
                assert_eq!(l, g[s as usize], "channel {c} level {i}");
            }
        }
        // A garbage percent is contained, never a panic or an out-of-range level.
        let wild = cshift_ramps(&[([255, 255, 255], f32::NAN), ([0, 0, 0], 1.0e9)], &id);
        assert_eq!(wild[0][200], 0, "a huge percent is the whole colour");
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
