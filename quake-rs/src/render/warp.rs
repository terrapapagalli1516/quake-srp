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
/// `out.w x out.h` view rectangle (`scr_vrect`), each pixel sampling the view
/// displaced by a per-row/per-column sine (`AMP2 = 3`, `SPEED = 20`,
/// `intsintable` from the 128-cycle phase) and stretched by
/// `wratio = w / scr_vrect.width` (`hratio` likewise), with the slight edge
/// compression `dim / (dim + 2*AMP2)` so it never reads outside the view.
/// The row displacement is driven by the column's sine and vice versa. The
/// ratios and row/column tables are the C's `float` arithmetic; `clock` drives
/// the phase. Applied to the 3-D frame BEFORE the content tint
/// (V_SetContentsColor), so wobble and tint compose as in software Quake.
/// `tables` are the renderer's, kept across frames ([`WarpTables`]); the
/// rows are written in runs on up to `threads` threads (each output row is
/// its own map of the view: the same bytes for any count).
///
/// With the hires extra (`hires`, [`VideoCvars::hires`](super::VideoCvars))
/// the view is rendered at the screen's own size and the wobble is scaled to
/// it ([`warp_scale`]): id's sine swings 3 pixels over a 128-pixel cycle in
/// SCREEN pixels at every resolution, so on a 320x200 screen it bends the view
/// by a hundredth and ripples twice across, and at 3840x2160 it would be a
/// fine shimmer over a picture blown up twelve times. Scaled, it is 320x200's
/// wobble at full resolution; at 320x200 it is id's to the pixel.
pub(super) fn warp_screen(tables: &mut WarpTables, view: &Image, out: WarpTarget, clock: f32, hires: bool, threads: usize) {
    let scale = if hires { warp_scale(out.w, out.h) } else { 1.0 };
    warp_scaled(tables, view, out, clock, scale, threads);
}

/// Where [`warp_screen`] writes: `w x h` pixels at column `x0` of `rows`,
/// `stride` pixels a row (the screen's view rectangle, or an image of its own),
/// and `below` more rows under them (EXTRA, not id: the 2026 status bar
/// overlay's world under the view, whose `view` is as many rows taller; 0 for
/// id's warp). The warp is laid out on the `w x h` alone — its scale, its
/// tables' compression — and the rows under it read on as the tables
/// continue, so the `h` rows are the same with or without them.
pub(super) struct WarpTarget<'a> {
    pub(super) rows: &'a mut [u8],
    pub(super) stride: usize,
    pub(super) x0: usize,
    pub(super) w: usize,
    pub(super) h: usize,
    pub(super) below: usize,
}

/// The hires warp's scale for an `out_w x out_h` view: its linear size in
/// 320x200 screens, `sqrt(w*h / (320*200))`, never below 1 (a view at most
/// 320x200 is id's own). The square root of the area keeps the wobble's share
/// of the view whatever its shape — a widescreen view, or one above a status
/// bar.
pub(crate) fn warp_scale(out_w: usize, out_h: usize) -> f64 {
    ((out_w as f64 * out_h as f64) / (320.0 * 200.0)).sqrt().max(1.0)
}

/// [`warp_screen`] with the sine's amplitude and cycle `scale` times id's
/// (`AMP2 * scale` pixels over `CYCLE * scale`, the phase advancing `SPEED *
/// scale` a second): at 1 exactly `D_WarpScreen`. A degenerate view or
/// target writes black.
fn warp_scaled(tables: &mut WarpTables, view: &Image, out: WarpTarget, clock: f32, scale: f64, threads: usize) {
    const SPEED: f64 = 20.0;
    let WarpTarget { rows, stride, x0, w: out_w, h: out_h, below } = out;
    // The view's own rows, and the `below` under them (id: none).
    let (w, h) = (view.w, view.h.saturating_sub(below));
    let out_rows = out_h + below;
    if stride == 0 || x0 + out_w > stride || rows.len() < out_rows * stride {
        return;
    }
    let rows = &mut rows[..out_rows * stride];
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 || view.pixels.len() < w * (h + below) {
        for row in rows.chunks_mut(stride) {
            row[x0..x0 + out_w].fill(0);
        }
        return;
    }
    let scale = if scale.is_finite() && scale > 1.0 { scale } else { 1.0 };
    let cycle = (WARP_CYCLE as f64 * scale).round() as i64;
    let phase = ((clock as f64 * SPEED * scale) as i64).rem_euclid(cycle) as usize;
    tables.prepare(w, h, out_w, out_h, below, scale, phase + out_w.max(out_rows));
    let WarpTables { rowptr, column, sin, .. } = &*tables;
    super::band::for_rows(threads, out_rows, rows, stride, |v0, run| {
        for (k, row) in run.chunks_mut(stride).enumerate() {
            let v = v0 + k;
            let tv = sin[phase + v] as usize; // 0..2*amp
            for (u, px) in row[x0..x0 + out_w].iter_mut().enumerate() {
                let tu = sin[phase + u] as usize; // 0..2*amp
                *px = view.pixels[rowptr[v + tu] + column[tv + u]];
            }
        }
    });
}

/// `AMP2` (d_local.h): the warp's sine swings `0..2*AMP2` pixels.
const WARP_AMP2: f64 = 3.0;
/// `CYCLE` (r_local.h): the sine's period in table entries (screen pixels).
const WARP_CYCLE: usize = 128;

/// [`warp_screen`]'s tables, kept across frames like id's static arrays
/// (`D_WarpScreen`'s `rowptr`/`column` and R_InitTurb's `intsintable`), so an
/// underwater frame allocates nothing: `rowptr` and `column` for the last
/// view and screen sizes and scale, and `intsintable` (at that scale) as far
/// as any frame has read it. The [`Renderer`](super::Renderer)'s.
pub(super) struct WarpTables {
    /// `(view w, view h, screen w, screen h, rows below)` the row/column
    /// tables are for.
    sizes: (usize, usize, usize, usize, usize),
    /// The scale `sin` is for (1: id's `intsintable`).
    scale: f64,
    /// `D_WarpScreen`'s `rowptr`, pre-multiplied by the view's width `w`: the
    /// gather in [`warp_scaled`] then adds `column[..]` straight to a row's
    /// starting offset instead of multiplying by `w` on every pixel.
    rowptr: Vec<usize>,
    column: Vec<usize>,
    sin: Vec<i32>,
}

impl Default for WarpTables {
    fn default() -> WarpTables {
        WarpTables { sizes: (0, 0, 0, 0, 0), scale: 1.0, rowptr: Vec::new(), column: Vec::new(), sin: Vec::new() }
    }
}

impl WarpTables {
    /// The tables for a `w x h` view warped over an `out_w x out_h` screen at
    /// `scale`, with at least `n` entries of `intsintable`. The arithmetic is
    /// `D_WarpScreen`'s `float`s, as it was per frame; the rows and columns
    /// past the edge are `2*AMP2` (times the scale, rounded up). `below`
    /// (the overlay's, [`WarpTarget`]) continues `rowptr` that many rows
    /// further, into as many rows of the view under its `h`: the same
    /// formula, so the wobble runs on unbroken, and every entry before them
    /// is id's.
    #[allow(clippy::too_many_arguments)]
    fn prepare(&mut self, w: usize, h: usize, out_w: usize, out_h: usize, below: usize, scale: f64, n: usize) {
        if self.scale != scale {
            self.scale = scale;
            self.sin.clear();
            self.sizes = (0, 0, 0, 0, 0);
        }
        extend_intsintable(&mut self.sin, n, scale);
        if self.sizes == (w, h, out_w, out_h, below) {
            return;
        }
        self.sizes = (w, h, out_w, out_h, below);
        let margin = (2.0 * WARP_AMP2 * scale).ceil() as usize;
        let wratio = w as f32 / out_w as f32;
        let hratio = h as f32 / out_h as f32;
        // rowptr[v] = (int)((float)v * hratio * h / (h + AMP2*2)), v < scr height + 2*AMP2,
        // times `w`: the row's starting offset into `view.pixels` directly.
        let row = |v: usize| (v as f32 * hratio * h as f32 / (h + margin) as f32) as usize;
        self.rowptr.clear();
        self.rowptr.extend((0..out_h + margin).map(|v| row(v).min(h - 1) * w));
        self.rowptr.extend((out_h + margin..out_h + below + margin).map(|v| row(v).min(h + below - 1) * w));
        // column[u] = (int)((float)u * wratio * w / (w + AMP2*2)), u < scr width + 2*AMP2
        self.column.clear();
        self.column.extend((0..out_w + margin).map(|u| {
            ((u as f32 * wratio * w as f32 / (w + margin) as f32) as usize).min(w - 1)
        }));
    }
}

/// `intsintable` (`R_InitTurb`, r_main.c): `AMP2 + sin(i*3.14159*2/CYCLE)*AMP2`,
/// truncated, for the first `n` indices (id fills `SIN_BUFFER_SIZE` = 1280+128,
/// enough for `phase + u` over its widest mode). With id's truncated `3.14159`
/// the argument falls just short of `pi/2` at i = 32, so the peak is 5, never 6;
/// and it is not 128-periodic: at i = 128, 256, ... the sine is a hair below 0
/// and the entry is 2 where i = 0 gives 3. `D_WarpScreen` indexes it without
/// wrapping, so neither may be folded to one cycle.
#[cfg(test)]
fn intsintable(n: usize) -> Vec<i32> {
    let mut t = Vec::new();
    extend_intsintable(&mut t, n, 1.0);
    t
}

/// Grow `tab` (the first entries of `intsintable`, its amplitude and cycle
/// `scale` times id's) to at least `n`. At scale 1 the arithmetic is the C's.
#[allow(clippy::approx_constant)]
fn extend_intsintable(tab: &mut Vec<i32>, n: usize, scale: f64) {
    let amp = WARP_AMP2 * scale;
    let cycle = WARP_CYCLE as f64 * scale;
    for i in tab.len()..n {
        tab.push((amp + ((i as f64) * 3.14159 * 2.0 / cycle).sin() * amp) as i32);
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
//    `D_DrawSkyScans8` (`d_sky.c`): not the face's `(s,t)`, in `sky.rs`.

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
    use crate::render::raster::{span_at, span_turb, AttrVert, PerspSpan, PolyGrads};

    /// `D_WarpScreen` at `scale` into an `out_w x out_h` image of its own,
    /// on fresh tables, on `threads` threads.
    fn warp_at(view: Image, out_w: usize, out_h: usize, clock: f32, scale: f64, threads: usize) -> Image {
        let mut out = Image::new(out_w, out_h, 7);
        let target = WarpTarget { rows: &mut out.pixels, stride: out_w, x0: 0, w: out_w, h: out_h, below: 0 };
        warp_scaled(&mut WarpTables::default(), &view, target, clock, scale, threads);
        out
    }

    /// `D_WarpScreen` with id's scale.
    fn apply_warp(view: Image, out_w: usize, out_h: usize, clock: f32) -> Image {
        warp_at(view, out_w, out_h, clock, 1.0, 1)
    }

    #[test]
    fn the_rows_below_continue_the_wobble_and_leave_the_views_rows_as_they_were() {
        // The 2026 overlay's underwater frame: a view 60 rows taller than the
        // warped rectangle (hires: drawn at its size, scale 4). Its own rows
        // are id's warp to the byte; the rows under it read on down the
        // source as the tables continue, each source row at most a wobble
        // from where the row above read.
        let (w, h, below, scale) = (640, 400, 60, 4.0);
        let src = coord_image(w, h + below, Y);
        let mut alone = Image::new(w, h, 7);
        let target = WarpTarget { rows: &mut alone.pixels, stride: w, x0: 0, w, h, below: 0 };
        warp_scaled(&mut WarpTables::default(), &Image { w, h, pixels: src.pixels[..w * h].to_vec() }, target, 1.25, scale, 1);
        let mut tables = WarpTables::default();
        let mut on = Image::new(w, h + below, 7);
        let target = WarpTarget { rows: &mut on.pixels, stride: w, x0: 0, w, h, below };
        warp_scaled(&mut tables, &src, target, 1.25, scale, 3);
        assert!(on.pixels[..w * h] == alone.pixels[..], "the view's rows are id's");
        let reach = (2.0 * WARP_AMP2 * scale).ceil() as usize + 2;
        for x in [0, 100, 333, 639] {
            let col: Vec<usize> = (0..h + below).map(|y| on.pixels[y * w + x] as usize).collect();
            for y in h - 1..h + below {
                // Source rows mod 256: unwrap against the row above.
                let (a, b) = (col[y - 1], col[y]);
                let step = (b + 256 - a) % 256;
                assert!(step <= reach || 256 - step <= reach, "column {x}, row {y}: {a} then {b}");
            }
        }
        // The same tables again, and with nothing below: id's tables back.
        let mut again = Image::new(w, h, 7);
        let target = WarpTarget { rows: &mut again.pixels, stride: w, x0: 0, w, h, below: 0 };
        warp_scaled(&mut tables, &Image { w, h, pixels: src.pixels[..w * h].to_vec() }, target, 1.25, scale, 2);
        assert!(again.pixels == alone.pixels);
    }

    /// The axes of [`coord_image`].
    const X: usize = 0;
    const Y: usize = 1;

    /// A `w x h` image whose pixel (x, y) is its `axis` coordinate (mod 256),
    /// to read back which source pixel the warp chose, one axis at a time.
    fn coord_image(w: usize, h: usize, axis: usize) -> Image {
        let mut img = Image::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                img.pixels[y * w + x] = [x, y][axis] as u8;
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
        let img = apply_warp(coord_image(w, h, Y), w, h, 0.0);
        let rowptr = |i: usize| i * h / (h + 6);
        for v in 0..h {
            assert_eq!(img.pixels[v * w + 128] as usize, rowptr(v + 2), "row {v}");
        }
        // And column 0 (intsintable[0] = 3) for contrast.
        assert_eq!(img.pixels[5 * w] as usize, rowptr(5 + 3));
    }

    #[test]
    fn warp_stretches_the_warp_buffer_over_the_screen_view() {
        // A 320x200 warp-buffer view stretched over a 640x400 scr_vrect (and a
        // 960x600 one, whose ratio 1/3 is inexact): every output pixel is
        // D_WarpScreen's sample, with the C's float row/column tables.
        let (w, h) = (320usize, 200usize);
        for (ow, oh, clock) in [(640usize, 400usize, 0.37f32), (960, 600, 5.0)] {
            let out = apply_warp(coord_image(w, h, X), ow, oh, clock);
            let out_y = apply_warp(coord_image(w, h, Y), ow, oh, clock);
            assert_eq!((out.w, out.h), (ow, oh));
            let (wr, hr) = (w as f32 / ow as f32, h as f32 / oh as f32);
            let phase = (clock as f64 * 20.0) as usize & 127;
            let t = intsintable(phase + ow);
            for v in (0..oh).step_by(7) {
                for u in (0..ow).step_by(5) {
                    let (tu, tv) = (t[phase + u] as usize, t[phase + v] as usize);
                    let row = ((v + tu) as f32 * hr * h as f32 / (h + 6) as f32) as usize;
                    let col = ((tv + u) as f32 * wr * w as f32 / (w + 6) as f32) as usize;
                    let got = (out.pixels[v * ow + u], out_y.pixels[v * ow + u]);
                    // coord_image stores x mod 256.
                    assert_eq!(got, ((col & 255) as u8, row as u8), "({u},{v})");
                }
            }
        }
        // A 1:1 warp (a mode no larger than 320x200) is the integer-exact case.
        let out = apply_warp(coord_image(w, h, Y), w, h, 0.0);
        assert_eq!(out.pixels[10 * w] as usize, (10 + 3) * h / (h + 6)); // turb[0] = 3
        // Degenerate sizes never panic.
        assert_eq!(apply_warp(Image::new(0, 0, 0), 4, 4, 0.0).pixels.len(), 16);
        assert!(apply_warp(coord_image(4, 4, X), 0, 0, 0.0).pixels.is_empty());
    }

    /// The warp kept its tables across frames (second review: it allocated
    /// `rowptr`, `column` and `intsintable` every underwater frame): frames at
    /// changing sizes and clocks give exactly what the per-frame computation
    /// gave, reproduced here from the code before the change.
    #[test]
    fn warp_tables_kept_across_frames_change_nothing() {
        fn per_frame(view: &Image, out_w: usize, out_h: usize, clock: f32) -> Vec<u8> {
            let (w, h) = (view.w, view.h);
            let (wratio, hratio) = (w as f32 / out_w as f32, h as f32 / out_h as f32);
            let rowptr: Vec<usize> = (0..out_h + 6)
                .map(|v| ((v as f32 * hratio * h as f32 / (h + 6) as f32) as usize).min(h - 1))
                .collect();
            let column: Vec<usize> = (0..out_w + 6)
                .map(|u| ((u as f32 * wratio * w as f32 / (w + 6) as f32) as usize).min(w - 1))
                .collect();
            let phase = ((clock as f64 * 20.0) as i64 & 127) as usize;
            let sintable = intsintable(phase + out_w.max(out_h));
            let mut out = vec![0u8; out_w * out_h];
            for v in 0..out_h {
                for u in 0..out_w {
                    let (tu, tv) = (sintable[phase + u] as usize, sintable[phase + v] as usize);
                    out[v * out_w + u] = view.pixels[rowptr[v + tu] * w + column[tv + u]];
                }
            }
            out
        }
        let frames = [
            (320, 152, 960, 456, 1.6f32),
            (320, 152, 960, 456, 1.62),
            (320, 200, 320, 200, 7.3),
            (266, 200, 800, 600, 0.0),
            (320, 152, 960, 456, 12.9),
            (320, 200, 1280, 800, 3.37),
            (320, 152, 960, 456, 1.6),
        ];
        for (w, h, ow, oh, clock) in frames {
            for axis in [X, Y] {
                let view = coord_image(w, h, axis);
                let want = per_frame(&view, ow, oh, clock);
                let got = apply_warp(view, ow, oh, clock);
                assert!(got.pixels == want, "{w}x{h} over {ow}x{oh} at {clock}, axis {axis}");
            }
        }
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
        // Draw a liquid surface covering the view (one span per row, as
        // D_DrawSurfaces would) and confirm that sampling the SAME geometry at two different
        // `time` values produces a DIFFERENT framebuffer (it animates), while
        // every sampled index stays in bounds (no panic, no garbage).
        let turb = TurbTable::new();
        let pixels = synthetic_liquid_pixels();

        // A surface spanning a range of (s,t) so the warp samples many texels.
        let (w, h) = (40usize, 40usize);
        let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 128.0, t: 0.0 };
        let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 128.0 };
        let g = PolyGrads::from_vertices(&[v0, v1, v2]).expect("triangle");
        let render_at = |time: f32| {
            let mut img = Image::new(w, h, 0);
            for y in 0..h {
                let row = &mut img.pixels[y * w..(y + 1) * w];
                span_turb(row, &span_at(&g, 0, y), &g, &pixels, 64, 64, &turb, time, PerspSpan::Exact);
            }
            img
        };
        let a = render_at(0.0);
        let b = render_at(0.5);

        // Animated: the two frames must differ somewhere.
        let changed = a.pixels.iter().zip(b.pixels.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "turbulent surface must animate between two times");

        // Every drawn pixel is one of the texture's texels, proving the sample
        // stayed in bounds (out-of-range would have continued).
        assert!(a.pixels.iter().any(|&p| p != 0), "turbulent surface drew nothing");
        for p in a.pixels.iter().chain(b.pixels.iter()) {
            assert!(pixels.contains(p), "sampled index {p} is not a texel of the liquid");
        }
    }

    #[test]
    fn the_hires_warp_is_id_at_320x200_and_scales_above() {
        // Scale 1 is D_WarpScreen: a 320x200 screen under the hires extra is
        // warped as id's, and warp_scale never goes below 1.
        assert_eq!(warp_scale(320, 200), 1.0);
        assert_eq!(warp_scale(320, 152), 1.0);
        assert_eq!(warp_scale(1280, 800), 4.0);
        assert!((warp_scale(3840, 2160) - 11.384).abs() < 1e-3);
        for axis in [X, Y] {
            let id = apply_warp(coord_image(320, 200, axis), 320, 200, 2.7);
            let hires = warp_at(coord_image(320, 200, axis), 320, 200, 2.7, warp_scale(320, 200), 1);
            assert!(id.pixels == hires.pixels);
        }
        // At scale 4 (1280x800) the sine swings 0..24 rows over a 512-column
        // cycle: column u of output row v reads row rowptr[v + t[u]], t the
        // scaled table, and the swing is 4x id's.
        let (w, h) = (1280usize, 800usize);
        let out = warp_at(coord_image(w, h, Y), w, h, 0.0, 4.0, 1);
        assert!(warp_at(coord_image(w, h, Y), w, h, 0.0, 4.0, 5).pixels == out.pixels, "any thread count");
        let mut t = Vec::new();
        extend_intsintable(&mut t, w, 4.0);
        assert_eq!((t.iter().min(), t.iter().max()), (Some(&0), Some(&23)));
        let rowptr = |i: usize| (i as f32 * h as f32 / (h + 24) as f32) as usize;
        for u in [0, 100, 128, 256, 511] {
            assert_eq!(out.pixels[10 * w + u] as usize, rowptr(10 + t[u] as usize) & 255, "column {u}");
        }
    }
}
