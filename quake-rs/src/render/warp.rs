//! Liquid turbulence and the underwater screen warp.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/d_scan.c` — `Turbulent8` / `D_DrawTurbulent8Span`'s texel
//! math and `D_WarpScreen`; the sine table is `R_InitTurb` (`r_main.c`).

use super::Image;

/// `D_WarpScreen` (d_scan.c): the underwater full-screen sine wobble applied when
/// the view leaf is in water/slime/lava (`r_waterwarp`, default on). Each output
/// pixel samples a source pixel displaced by a per-row/per-column sine offset
/// (`AMP2 = 3`, `SPEED = 20`, 128-cycle `intsintable`), with a slight edge
/// compression (`dim/(dim + 2*AMP2)`) so the warp never reads outside the frame.
/// The row displacement is driven by the column's sine and vice-versa (the classic
/// cross-coupled warp). Operates on a snapshot of the frame; `clock` drives the
/// phase. Applied to the 3-D frame BEFORE the content tint (V_SetContentsColor),
/// so wobble and tint compose exactly as in stock software Quake.
// The `3.14159` below is id's truncated literal (see the in-body comment): using
// `std::f64::consts::PI` would shift the table by one index and break the warp's
// byte-identity, so the clippy::approx_constant lint is deliberately allowed here.
#[allow(clippy::approx_constant)]
pub fn apply_warp(image: &mut Image, clock: f32) {
    const AMP2: i32 = 3;
    const SPEED: f64 = 20.0;
    let w = image.w as i32;
    let h = image.h as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // intsintable[i] = (int)(AMP2 + AMP2*sin(i*3.14159*2/128)) — truncated, 0..2*AMP2.
    // id's R_InitTurb uses the truncated literal 3.14159 (NOT exact pi), so the table
    // tops out at 5 (a broad plateau), never 6: at i=32 the argument falls just short
    // of pi/2 so sin<1 and (int)5.999..=5. Using exact 2*pi would give 6 at i=32 — a
    // one-index divergence. Match the C literal for bit-identical warp.
    let mut sintable = [0i32; 128];
    for (i, s) in sintable.iter_mut().enumerate() {
        let f = AMP2 as f64 + AMP2 as f64 * ((i as f64) * 3.14159 * 2.0 / 128.0).sin();
        *s = f as i32; // (int) truncation, matching the C table build
    }
    // rowptr[i] = compressed source row for stretched index i in 0..h+2*AMP2.
    let rspan = (h + 2 * AMP2) as usize;
    let mut rowptr = vec![0usize; rspan];
    for (i, r) in rowptr.iter_mut().enumerate() {
        let v = (i as i64 * h as i64 / (h + 2 * AMP2) as i64) as i32;
        *r = v.clamp(0, h - 1) as usize;
    }
    // column[j] = compressed source column for stretched index j in 0..w+2*AMP2.
    let cspan = (w + 2 * AMP2) as usize;
    let mut column = vec![0usize; cspan];
    for (j, c) in column.iter_mut().enumerate() {
        let u = (j as i64 * w as i64 / (w + 2 * AMP2) as i64) as i32;
        *c = u.clamp(0, w - 1) as usize;
    }
    let phase = ((clock as f64 * SPEED) as i64 & 127) as usize;
    let src = image.rgb.clone(); // pre-warp snapshot
    let (wu, hu) = (w as usize, h as usize);
    for v in 0..hu {
        let tv = sintable[(phase + v) & 127] as usize; // 0..2*AMP2
        for u in 0..wu {
            let tu = sintable[(phase + u) & 127] as usize; // 0..2*AMP2
            let src_row = rowptr[v + tu];
            let src_col = column[tv + u];
            image.rgb[v * wu + u] = src[src_row * wu + src_col];
        }
    }
}

// ---------------------------------------------------------------------------
// Animated special surfaces: liquid turbulent warp + scrolling sky
// ---------------------------------------------------------------------------
//
// Quake's `TEX_SPECIAL` faces (liquids and sky) are not lightmapped: they store
// the raw texel (no colormap) and are *animated* every frame. This port
// reproduces the software renderer's two animations against the same
// perspective-correct `(s,t)` the textured rasteriser already interpolates:
//
//  * **Liquids** (miptex name begins with `*`: `*water1`, `*lava1`, `*slime`,
//    `*teleport`, …) get `Turbulent8`/`D_DrawTurbulent8Span`'s warp (`d_scan.c`):
//    each axis of the 16.16 sample is displaced by a sine of the OTHER axis plus
//    time. See [`TurbTable`] / [`warp_st`].
//  * **Sky** (miptex name begins with `sky`: `sky1`, `sky4`, …) gets the
//    two-layer scroll of `R_MakeSky` (`r_sky.c`) sampled along the view ray by
//    `D_DrawSkyScans8` (`d_sky.c`). See [`sky_texel_view`].

/// WinQuake `R_InitTurb` constants (r_main.c / r_local.h / r_shared.h): the
/// software liquid warp drives a 128-cycle sine table by the integer part of the
/// OTHER axis' 16.16 texel coordinate, scrolled by `time*SPEED`.
const TURB_CYCLE: usize = 128;
/// `AMP` (`8*0x10000`) — the table swings `8 + 8*sin` texels in 16.16 fixed point
/// (0..16 texels; the +8 DC bias is wrapped off by the 64-texel liquid texture
/// downstream, exactly as `&63` in `d_scan.c`).
const TURB_AMP: f64 = (8 * 0x10000) as f64;
/// `SPEED` — the table phase advances by `time*20` per second.
const TURB_SPEED: f32 = 20.0;

/// WinQuake's `sintable` (`R_InitTurb`, r_main.c):
/// `sintable[i] = AMP + sin(i*3.14159*2/CYCLE)*AMP`, 16.16 fixed point, truncated.
/// `Turbulent8` indexes it as `(sintable + phase)[coord & 127]` — up to entry 254 —
/// and id's truncated `3.14159` makes the table NOT exactly 128-periodic, so the
/// first `2*CYCLE` entries are kept rather than one wrapped cycle.
pub(super) struct TurbTable {
    tab: [i32; 2 * TURB_CYCLE],
}

impl TurbTable {
    /// Build the table once at render start (`f64::sin` is not `const`).
    // `3.14159` is id's literal; exact pi would move entries (see `apply_warp`).
    #[allow(clippy::approx_constant)]
    pub(super) fn new() -> TurbTable {
        let mut tab = [0i32; 2 * TURB_CYCLE];
        for (i, e) in tab.iter_mut().enumerate() {
            // C: int = int + double*int -> double, then truncated to int.
            *e = (TURB_AMP + ((i as f64) * 3.14159 * 2.0 / TURB_CYCLE as f64).sin() * TURB_AMP) as i32;
        }
        TurbTable { tab }
    }
}

/// A liquid surface coordinate in `Turbulent8`'s 16.16 fixed point:
/// `(int)(sdivz*z) + sadjust`, clamped to `[0, bbextents]` — with the extents
/// `Mod_LoadFaces` gives every turbulent face (`texturemins = -8192`,
/// `extents = 16384`), so the fixed value is `(s + 8192) * 0x10000`. `s` is the
/// texinfo coordinate the rasteriser interpolates.
#[inline]
fn turb_fixed(s: f32) -> i32 {
    // s*0x10000 is exact in f32 (a power of two); `as` truncates like the C cast.
    let v = (s * 65536.0) as i32 as i64 + (8192 << 16);
    v.clamp(0, (16384 << 16) - 1) as i32
}

/// Apply the SOFTWARE liquid warp to the surface coordinate `(s,t)` at game
/// `time`, returning the texel `(sturb, tturb)` to sample — `D_DrawTurbulent8Span`
/// (`d_scan.c`) on `Turbulent8`'s fixed-point coordinates:
///
/// ```text
/// turb  = sintable + ((int)(cl.time*SPEED) & (CYCLE-1));
/// sturb = ((s + turb[(t>>16)&(CYCLE-1)]) >> 16) & 63;
/// tturb = ((t + turb[(s>>16)&(CYCLE-1)]) >> 16) & 63;
/// ```
///
/// The 16.16 table value is added to the 16.16 coordinate BEFORE the `>> 16`, so
/// the fractional parts carry. The caller wraps into the texture (`rem_euclid`;
/// = `& 63` for the 64x64 liquids id ships). The coordinate here is exact per
/// pixel; id steps it linearly across 16-pixel segments (class 7 in
/// `oracle/README.md`, the span-subdivision item, shared with the walls).
#[inline]
pub(super) fn warp_st(turb: &TurbTable, s: f32, t: f32, time: f32) -> (i32, i32) {
    const MASK: i32 = TURB_CYCLE as i32 - 1;
    let phase = ((time * TURB_SPEED) as i32 & MASK) as usize;
    let sf = turb_fixed(s);
    let tf = turb_fixed(t);
    let sturb = sf.wrapping_add(turb.tab[phase + ((tf >> 16) & MASK) as usize]) >> 16;
    let tturb = tf.wrapping_add(turb.tab[phase + ((sf >> 16) & MASK) as usize]) >> 16;
    (sturb, tturb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{raster_triangle_tex, ProjT, SurfaceMode};
    use crate::render::fixtures::synthetic_liquid_pixels;

    #[test]
    fn turb_table_matches_r_initturb() {
        // R_InitTurb: sintable[i] = (int)(AMP + sin(i*3.14159*2/CYCLE)*AMP), 16.16,
        // DC-biased so the range is [0, 2*AMP]. Spot values computed from the C
        // expression: i=0 -> AMP exactly; i=32 falls just short of pi/2 (id's
        // 3.14159), so it never reaches 2*AMP.
        let turb = TurbTable::new();
        assert_eq!(turb.tab[0], 8 << 16);
        assert_eq!(turb.tab[32], 1_048_575, "3.14159 keeps the peak one unit short");
        assert!(turb.tab.iter().all(|&v| (0..=16 << 16).contains(&v)));
        // Not exactly periodic (3.14159 < pi): entries past the first cycle are
        // their own values, which is why Turbulent8's `phase + (t>>16 & 127)` reads
        // the second cycle rather than wrapping.
        assert!((0..TURB_CYCLE).any(|i| turb.tab[i] != turb.tab[i + TURB_CYCLE]));
    }

    #[test]
    fn warp_st_is_turbulent8_fixed_point() {
        // D_DrawTurbulent8Span on Turbulent8's coordinates: the 16.16 sine is added
        // to the 16.16 coordinate (texturemins -8192) BEFORE the >>16, so a
        // fraction carries into the texel.
        let turb = TurbTable::new();
        let (s, t) = (20.75f32, 33.5f32);
        let phase = (0.37f32 * 20.0) as usize; // 7
        let sf = ((20.75 + 8192.0) * 65536.0) as i32;
        let tf = ((33.5 + 8192.0) * 65536.0) as i32;
        let want_s = (sf + turb.tab[phase + ((tf >> 16) & 127) as usize]) >> 16;
        let want_t = (tf + turb.tab[phase + ((sf >> 16) & 127) as usize]) >> 16;
        assert_eq!(warp_st(&turb, s, t, 0.37), (want_s, want_t));
        // Animated: the time phase shifts the table index.
        assert_ne!(warp_st(&turb, s, t, 0.0), warp_st(&turb, s, t, 0.37));
        // Bounded: the offset from the base texel (plus id's +8192) is in [0, 16].
        for (warped, base) in [(want_s, sf >> 16), (want_t, tf >> 16)] {
            assert!((0..=16).contains(&(warped - base)), "displacement out of range");
        }
    }

    #[test]
    fn turbulent_sampler_animates_at_fixed_st() {
        // Drive `raster_triangle_tex` in Turb mode over a single screen-filling
        // triangle and confirm that sampling the SAME geometry at two different
        // `time` values produces a DIFFERENT framebuffer (it animates), while
        // every sampled index stays in bounds (no panic, no garbage).
        let turb = TurbTable::new();
        let pixels = synthetic_liquid_pixels();
        // A palette that maps each index to a distinct grey so different texels
        // give different colours.
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }

        // One large triangle covering the framebuffer, spanning a range of (s,t)
        // so the warp samples many texels.
        let (w, h) = (40usize, 40usize);
        let render_at = |time: f32| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zb = vec![f32::INFINITY; w * h];
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 128.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 128.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
                &pixels, 64, 64, &pal, 1.0, None,
                SurfaceMode::Turb { turb: &turb, time },
                None,
            );
            img
        };
        let a = render_at(0.0);
        let b = render_at(0.5);

        // Animated: the two frames must differ somewhere.
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "turbulent surface must animate between two times");

        // Every drawn pixel is a real palette colour (grey: all channels equal),
        // proving the sample stayed in bounds (out-of-range would have continued).
        assert!(
            a.rgb.iter().any(|p| *p != [0, 0, 0]),
            "turbulent triangle drew nothing"
        );
        for p in a.rgb.iter().chain(b.rgb.iter()) {
            assert!(p[0] == p[1] && p[1] == p[2], "sampled colour not a palette grey: {p:?}");
        }
    }
}
