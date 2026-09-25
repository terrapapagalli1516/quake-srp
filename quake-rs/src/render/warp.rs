//! Liquid turbulence and the underwater screen warp.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/d_scan.c` — `Turbulent8` / `D_DrawTurbulent8Span`'s texel
//! math and `D_WarpScreen`; the sine table is `R_InitTurb` (`r_main.c`).

use super::Image;

/// `D_WarpScreen` (d_scan.c): the underwater sine wobble, applied when the
/// view leaf is in water/slime/lava (`r_waterwarp`, default on). `view` is the
/// frame id renders into `r_warpbuffer` (at most 320x200, see
/// [`warp_vrect`](crate::screen::warp_vrect)); the result is the screen's
/// `out_w x out_h` view rectangle (`scr_vrect`), each pixel sampling the view
/// displaced by a per-row/per-column sine (`AMP2 = 3`, `SPEED = 20`,
/// `intsintable` from the 128-cycle phase) and stretched by
/// `wratio = w / scr_vrect.width` (`hratio` likewise), with the slight edge
/// compression `dim / (dim + 2*AMP2)` so it never reads outside the view.
/// The row displacement is driven by the column's sine and vice versa. The
/// ratios and row/column tables are the C's `float` arithmetic; `clock` drives
/// the phase. Applied to the 3-D frame BEFORE the content tint
/// (V_SetContentsColor), so wobble and tint compose as in software Quake. The
/// view's buffer goes back to the frame pool.
pub fn apply_warp(view: Image, out_w: usize, out_h: usize, clock: f32) -> Image {
    const AMP2: usize = 3;
    const SPEED: f64 = 20.0;
    let (w, h) = (view.w, view.h);
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 || view.rgb.len() < w * h {
        super::recycle_image(view);
        return Image::new(out_w, out_h, [0, 0, 0]);
    }
    let wratio = w as f32 / out_w as f32;
    let hratio = h as f32 / out_h as f32;
    // rowptr[v] = (int)((float)v * hratio * h / (h + AMP2*2)), v < scr height + 2*AMP2
    let rowptr: Vec<usize> = (0..out_h + 2 * AMP2)
        .map(|v| ((v as f32 * hratio * h as f32 / (h + 2 * AMP2) as f32) as usize).min(h - 1))
        .collect();
    // column[u] = (int)((float)u * wratio * w / (w + AMP2*2)), u < scr width + 2*AMP2
    let column: Vec<usize> = (0..out_w + 2 * AMP2)
        .map(|u| ((u as f32 * wratio * w as f32 / (w + 2 * AMP2) as f32) as usize).min(w - 1))
        .collect();
    let phase = ((clock as f64 * SPEED) as i64 & 127) as usize;
    // `turb = intsintable + phase`, read at `turb[u]` and `turb[v]` across the
    // whole screen: the table (R_InitTurb) is not wrapped to one cycle.
    let sintable = intsintable(phase + out_w.max(out_h));
    let mut out = Image::reused_uncleared(out_w, out_h);
    for (v, row) in out.rgb.chunks_exact_mut(out_w).take(out_h).enumerate() {
        let tv = sintable[phase + v] as usize; // 0..2*AMP2
        for (u, px) in row.iter_mut().enumerate() {
            let tu = sintable[phase + u] as usize; // 0..2*AMP2
            *px = view.rgb[rowptr[v + tu] * w + column[tv + u]];
        }
    }
    super::recycle_image(view);
    out
}

/// `intsintable` (`R_InitTurb`, r_main.c): `AMP2 + sin(i*3.14159*2/CYCLE)*AMP2`,
/// truncated, for the first `n` indices (id fills `SIN_BUFFER_SIZE` = 1280+128,
/// enough for `phase + u` over its widest mode). With id's truncated `3.14159`
/// the argument falls just short of `pi/2` at i = 32, so the peak is 5, never 6;
/// and it is not 128-periodic: at i = 128, 256, ... the sine is a hair below 0
/// and the entry is 2 where i = 0 gives 3. `D_WarpScreen` indexes it without
/// wrapping, so neither may be folded to one cycle.
#[allow(clippy::approx_constant)]
fn intsintable(n: usize) -> Vec<i32> {
    const AMP2: f64 = 3.0;
    (0..n)
        .map(|i| (AMP2 + ((i as f64) * 3.14159 * 2.0 / 128.0).sin() * AMP2) as i32)
        .collect()
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
    // `3.14159` is id's literal; exact pi would move entries (see `intsintable`).
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
/// pixel: the port's exact-perspective extra. id steps it linearly across
/// 16-pixel segments, as the default does (`raster_turb16`; class 7 in
/// `oracle/README.md`, the span-subdivision item, shared with the walls).
#[inline]
pub(super) fn warp_st(turb: &TurbTable, s: f32, t: f32, time: f32) -> (i32, i32) {
    turb.texel(turb_phase(time), turb_fixed(s), turb_fixed(t))
}

/// `Turbulent8`'s table phase: `r_turb_turb = sintable + ((int)(cl.time*SPEED)
/// & (CYCLE-1))`.
#[inline]
pub(super) fn turb_phase(time: f32) -> usize {
    ((time * TURB_SPEED) as i32 & (TURB_CYCLE as i32 - 1)) as usize
}

/// `(CYCLE << 16) - 1`: `Turbulent8` masks each segment's start coordinates
/// with it before `D_DrawTurbulent8Span` steps them.
pub(super) const TURB_COORD_MASK: i32 = ((TURB_CYCLE as i32) << 16) - 1;

impl TurbTable {
    /// `D_DrawTurbulent8Span`'s texel for the 16.16 coordinates `(s, t)` at the
    /// table `phase` ([`turb_phase`]), before the `& 63`:
    /// `((s + turb[(t>>16) & (CYCLE-1)]) >> 16`, and `t` the other way round.
    #[inline]
    pub(super) fn texel(&self, phase: usize, s: i32, t: i32) -> (i32, i32) {
        const MASK: i32 = TURB_CYCLE as i32 - 1;
        let sturb = s.wrapping_add(self.tab[phase + ((t >> 16) & MASK) as usize]) >> 16;
        let tturb = t.wrapping_add(self.tab[phase + ((s >> 16) & MASK) as usize]) >> 16;
        (sturb, tturb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::synthetic_liquid_pixels;
    use crate::render::raster::{outline, raster_poly_tex, AttrVert, Persp, PolyGrads, SurfaceMode};

    /// A `w x h` image whose pixel (x, y) is `[x, y, 0]`, to read back which
    /// source pixel the warp chose.
    fn coord_image(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h, [0, 0, 0]);
        for y in 0..h {
            for x in 0..w {
                img.rgb[y * w + x] = [x as u8, y as u8, 0];
            }
        }
        img
    }

    #[test]
    fn intsintable_is_r_initturbs_unwrapped_table() {
        let t = intsintable(1408);
        assert_eq!(&t[..4], &[3, 3, 3, 3]);
        assert_eq!(t[32], 5, "3.14159: the peak stays one short of 6");
        // Every 128th entry past the first is 2 (sin a hair below 0), not 3.
        for k in 1..11 {
            assert_eq!((t[128 * k], t[0]), (2, 3), "entry {}", 128 * k);
        }
        let differ = (0..1408).filter(|&i| t[i] != t[i & 127]).count();
        assert_eq!(differ, 10, "exactly the multiples of 128 differ from a wrapped cycle");
    }

    #[test]
    fn warp_reads_intsintable_past_the_first_cycle() {
        // D_WarpScreen: dest[v][u] = rowptr[v + turb[u]][column[turb[v] + u]],
        // turb = intsintable + phase. At phase 0, column 128 displaces its row
        // by intsintable[128] = 2 (a wrapped table would give 3).
        let (w, h) = (200usize, 40usize);
        let img = apply_warp(coord_image(w, h), w, h, 0.0);
        let rowptr = |i: usize| i * h / (h + 6);
        for v in 0..h {
            assert_eq!(img.rgb[v * w + 128][1] as usize, rowptr(v + 2), "row {v}");
        }
        // And column 0 (intsintable[0] = 3) for contrast.
        assert_eq!(img.rgb[5 * w][1] as usize, rowptr(5 + 3));
    }

    #[test]
    fn warp_stretches_the_warp_buffer_over_the_screen_view() {
        // A 320x200 warp-buffer view stretched over a 640x400 scr_vrect (and a
        // 960x600 one, whose ratio 1/3 is inexact): every output pixel is
        // D_WarpScreen's sample, with the C's float row/column tables.
        let (w, h) = (320usize, 200usize);
        for (ow, oh, clock) in [(640usize, 400usize, 0.37f32), (960, 600, 5.0)] {
            let out = apply_warp(coord_image(w, h), ow, oh, clock);
            assert_eq!((out.w, out.h), (ow, oh));
            let (wr, hr) = (w as f32 / ow as f32, h as f32 / oh as f32);
            let phase = (clock as f64 * 20.0) as usize & 127;
            let t = intsintable(phase + ow);
            for v in (0..oh).step_by(7) {
                for u in (0..ow).step_by(5) {
                    let (tu, tv) = (t[phase + u] as usize, t[phase + v] as usize);
                    let row = ((v + tu) as f32 * hr * h as f32 / (h + 6) as f32) as usize;
                    let col = ((tv + u) as f32 * wr * w as f32 / (w + 6) as f32) as usize;
                    let got = out.rgb[v * ow + u];
                    // coord_image stores x mod 256 in channel 0.
                    assert_eq!((got[0], got[1]), ((col & 255) as u8, row as u8), "({u},{v})");
                }
            }
        }
        // A 1:1 warp (a mode no larger than 320x200) is the integer-exact case.
        let out = apply_warp(coord_image(w, h), w, h, 0.0);
        assert_eq!(out.rgb[10 * w][1] as usize, (10 + 3) * h / (h + 6)); // turb[0] = 3
        // Degenerate sizes never panic.
        assert_eq!(apply_warp(Image::new(0, 0, [0; 3]), 4, 4, 0.0).rgb.len(), 16);
        assert!(apply_warp(coord_image(4, 4), 0, 0, 0.0).rgb.is_empty());
    }

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
        // Drive `raster_poly_tex` in Turb mode over a single screen-filling
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
            let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 128.0, t: 0.0 };
            let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 128.0 };
            let tri = [v0, v1, v2];
            let g = PolyGrads::from_vertices(&tri).expect("triangle");
            raster_poly_tex(
                &mut img, &mut zb, &outline(&tri), &g,
                &pixels, 64, 64, &pal, 1.0, None,
                SurfaceMode::Turb { turb: &turb, time, persp: Persp::Exact },
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
