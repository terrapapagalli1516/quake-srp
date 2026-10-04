//! The scrolling two-layer sky.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sky.c` (`R_MakeSky`'s layer offsets) and `WinQuake/d_sky.c`
//! (`D_Sky_uv_To_st`, `D_DrawSkyScans8`'s 32-pixel spans).
//!
//! **How id's sky moves.** Two layers of the sky miptexture scroll diagonally:
//! the back (its right half) at `skyspeed` 8 texels a second and the front, the
//! clouds (its left half, index 0 transparent), at twice that. They move in two
//! different ways. `D_Sky_uv_To_st` adds `skytime*skyspeed` to every pixel's
//! 16.16 `(s,t)` as a float, so the whole composite glides: its texel edges
//! cross the screen at the true speed. `R_MakeSky` adds the front layer's own
//! extra 8 texels a second as `xshift = (int)(skytime*skyspeed)`, whole texels,
//! so on top of that glide the clouds jump a whole texel along the diagonal
//! every eighth of a second. At 320x200 a sky texel is about a screen pixel; at
//! 1080p looking up it is 6 pixels, at 4K 11, and the clouds lurch eight times
//! a second. [`SkyScroll::Fluid`], the 2026 sky, gives the front layer the exact
//! `skytime*skyspeed` instead: the clouds glide as the back layer always has,
//! every pixel still one of the texture's own texels, nearest, unfiltered.
//!
//! The port draws id's composite without building it: [`sky_sample`] reads the
//! front layer and, where that is transparent, the back, at each pixel, which is
//! `newsky` texel for texel. That makes the fluid front layer free: the offset
//! is added to the 16.16 coordinate before its texel is taken instead of after.

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::surf::{classify_surface, SurfKind};

/// `SKYSIZE` (d_iface.h): each sky layer is 128x128 texels.
const SKYSIZE: i32 = 128;
/// `SKYMASK` (d_iface.h), and `R_SKY_SMASK`/`R_SKY_TMASK >> 16` (d_local.h).
const SKYMASK: i32 = SKYSIZE - 1;
/// `iskyspeed` (r_sky.c): the scroll, in texels per second.
const SKY_SPEED: f32 = 8.0;

/// How the front (cloud) layer's extra scroll is applied (the module docs): a
/// video cvar, [`VideoCvars::sky`](super::VideoCvars::sky), the console's
/// `r_fluidsky`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkyScroll {
    /// id's: `R_MakeSky`'s `xshift = (int)(skytime*skyspeed)`, whole texels,
    /// so the clouds jump a texel eight times a second.
    #[default]
    Classic,
    /// The exact `skytime*skyspeed` in 16.16, so the clouds glide at their
    /// true speed. The same frame as id's whenever `skytime*skyspeed` is a
    /// whole number.
    Fluid,
}

/// The per-frame sky state a sky pixel needs, porting the globals
/// `D_Sky_uv_To_st` and `R_MakeSky` read: the view basis (`vpn`/`vright`/
/// `vup`), the screen centre, the normaliser, and the scroll.
///
/// The sky is an infinite dome: what a screen pixel shows depends on the view
/// DIRECTION through that pixel, NOT on the wall polygon's `(s,t)`.
#[derive(Clone, Copy)]
pub(super) struct SkyView {
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    /// `(int)vid.width>>1`, `(int)vid.height>>1` less the view's corner on the
    /// screen, in the view's own pixels — `D_Sky_uv_To_st`'s integer
    /// SCREEN centre (not the view's, and not the projection's `xcenter`,
    /// which is half a pixel off).
    half_w: i32,
    half_h: i32,
    /// `max(vrect.width, vrect.height)` — the `temp` normaliser in
    /// `D_Sky_uv_To_st`: a fixed dome angle, independent of the render FOV
    /// ([`sky_dome_scale`]).
    longest: f32,
    /// `skytime*skyspeed`, added to both `s` and `t` (`D_Sky_uv_To_st`).
    scroll: f32,
    /// The front (cloud) layer's extra offset on both axes, 16.16, so it moves
    /// at twice the back's speed: `R_MakeSky`'s `xshift`/`yshift` =
    /// `(int)(skytime*skyspeed)` whole texels ([`SkyScroll::Classic`]), or
    /// `skytime*skyspeed` itself ([`SkyScroll::Fluid`]).
    front: i32,
}

/// `D_Sky_uv_To_st`'s `temp`, the dome's scale in pixels, for a `w x h` view
/// drawn with the horizontal field of view `fov_x` for the `fov` cvar
/// `scr_fov`: id's `max(vrect.width, vrect.height)` — at `fov 90` the dome's
/// rays through the screen's columns are the view's own — whenever the two are
/// the same (Classic, and Hor+ on a 4:3 screen). When Hor+ has widened the
/// view the sky keeps the scale of the 4:3 view it widens,
/// `w * tan(scr_fov/2) / tan(fov_x/2)` across, so it stays put against the
/// walls as the view turns and only more of it shows at the sides.
pub(super) fn sky_dome_scale(w: usize, h: usize, scr_fov: f32, fov_x: f32) -> f32 {
    if fov_x == scr_fov {
        return w.max(h) as f32;
    }
    let half_tan = |f: f32| (f as f64 * 0.5).to_radians().tan();
    let ratio = half_tan(scr_fov) / half_tan(fov_x);
    if !(ratio.is_finite() && ratio > 0.0) {
        return w.max(h) as f32;
    }
    (w as f64 * ratio).max(h as f64) as f32
}

impl SkyView {
    /// The sky state for a view whose dome scale is `longest` pixels
    /// ([`sky_dome_scale`]) and whose screen's centre is `centre` in its own
    /// pixels ([`RenderOptions`](super::RenderOptions)`::sky_centre`), at game
    /// `time`, its clouds scrolled as `mode` says. `R_SetSkyFrame`
    /// (r_sky.c): `skytime = cl.time - (int)(cl.time/temp)*temp` with
    /// `temp = SKYSIZE*s1*s2` = 512, where `s1`/`s2` are `iskyspeed` 8 and
    /// `iskyspeed2` 2 over their gcd. Both modes wrap there with every layer
    /// on a whole number of turns (4096 texels, 32 turns of 128).
    pub(super) fn new(
        forward: Vec3,
        right: Vec3,
        up: Vec3,
        longest: f32,
        centre: (i32, i32),
        time: f32,
        mode: SkyScroll,
    ) -> SkyView {
        const TEMP: f64 = 512.0;
        let t = time as f64;
        let skytime = (t - ((t / TEMP) as i32 as f64) * TEMP) as f32;
        let scroll = skytime * SKY_SPEED;
        let front = match mode {
            // R_MakeSky: xshift = skytime*skyspeed, truncated to int.
            SkyScroll::Classic => (scroll as i32) << 16,
            // Exact: scroll is at most 4096, so this is scroll * 2^16 to the
            // f32's last bit.
            SkyScroll::Fluid => (scroll * 65536.0) as i32,
        };
        SkyView {
            forward,
            right,
            up,
            half_w: centre.0,
            half_h: centre.1,
            longest,
            scroll,
            front,
        }
    }
}

/// `D_Sky_uv_To_st` (d_sky.c): the 16.16 sky coordinates for screen pixel
/// `(u,v)` — build the ray `4096*vpn + wu*vright + wv*vup` (screen offsets from
/// the integer centre scaled by `8192/longest`), squash it vertically
/// (`end[2] *= 3`), normalise, then
/// `s = (skytime*skyspeed + 6*(SKYSIZE/2-1)*end[0]) * 0x10000` and the same
/// for `t` with `end[1]`. Float math as the C (`wu`/`wv` computed in double,
/// stored to float; `VectorNormalize` multiplies by `1/length`).
#[inline]
fn sky_uv_to_st(u: i32, v: i32, sky: &SkyView) -> (i32, i32) {
    let longest = if sky.longest > 0.0 { sky.longest as f64 } else { 1.0 };
    let wu = (8192.0 * (u - sky.half_w) as f64 / longest) as f32;
    let wv = (8192.0 * (sky.half_h - v) as f64 / longest) as f32;
    let (f, r, up) = (sky.forward, sky.right, sky.up);
    let mut end = [
        4096.0 * f[0] + wu * r[0] + wv * up[0],
        4096.0 * f[1] + wu * r[1] + wv * up[1],
        4096.0 * f[2] + wu * r[2] + wv * up[2],
    ];
    end[2] *= 3.0;
    // VectorNormalize (mathlib.c)
    let length = (end[0] * end[0] + end[1] * end[1] + end[2] * end[2]).sqrt();
    if length != 0.0 {
        let ilength = 1.0 / length;
        end[0] *= ilength;
        end[1] *= ilength;
    }
    // 6*(SKYSIZE/2-1) = 378
    const DOME: f32 = (6 * (SKYSIZE / 2 - 1)) as f32;
    let s = ((sky.scroll + DOME * end[0]) * 65536.0) as i32;
    let t = ((sky.scroll + DOME * end[1]) * 65536.0) as i32;
    (s, t)
}

/// One sky texel for the 16.16 sky coordinates `(s,t)`, porting what
/// `D_DrawSkyScans8` reads — `r_skysource[((t & R_SKY_TMASK) >> 8) +
/// ((s & R_SKY_SMASK) >> 16)]`, i.e. row `(t>>16)&127`, column `(s>>16)&127` of
/// `newsky` — and what `R_MakeSky` composited there: the front layer (the
/// miptexture's LEFT half, `R_InitSky`'s `bottomsky`, index 0 transparent)
/// read `front` (16.16) further along on both axes, over the back layer (the
/// RIGHT half, unshifted). With `front` a whole number of texels this is id's
/// `newsky[y][x]` = front `[y+shift][x+shift]` over back `[y][x]` exactly; the
/// fluid sky's fraction moves where the front's texel edges fall.
///
/// `pixels` is the `tw`-wide sky miptexture (256x128 in every id map). Every read
/// is `.get()`-guarded, so a malformed sky never panics (it yields index 0).
#[inline]
fn sky_sample(pixels: &[u8], tw: usize, s: i32, t: i32, front: i32) -> u8 {
    let x = (s >> 16) & SKYMASK;
    let y = (t >> 16) & SKYMASK;
    // Wrapping is harmless: the mask keeps bits 16..22, which a wrap of 2^32
    // leaves as they are.
    let fx = ((s.wrapping_add(front) >> 16) & SKYMASK) as usize;
    let fy = ((t.wrapping_add(front) >> 16) & SKYMASK) as usize;
    match pixels.get(fy * tw + fx).copied() {
        Some(cloud) if cloud != 0 => cloud,
        _ => pixels.get(y as usize * tw + (tw / 2) + x as usize).copied().unwrap_or(0),
    }
}

/// `SKY_SPAN_SHIFT` (d_sky.c): `D_DrawSkyScans8` evaluates `D_Sky_uv_To_st`
/// exactly every `1 << 5` = 32 pixels of a span and steps linearly between.
const SKY_SPAN_SHIFT: i32 = 5;
const SKY_SPAN_MAX: i32 = 1 << SKY_SPAN_SHIFT;

/// `D_DrawSkyScans8` (d_sky.c) for one span of `count` pixels starting at screen
/// `(u, v)`, written into `out` (that scanline's pixels from `u`): the sky
/// coordinates are exact at the span start and every 32 pixels, stepped by
/// `(next - cur) >> 5` between; the last segment steps by an integer division
/// over its `count - 1` so it ends exactly on the span's last pixel.
pub(super) fn draw_sky_span(
    out: &mut [u8],
    u: i32,
    v: i32,
    count: i32,
    pixels: &[u8],
    tw: usize,
    view: &SkyView,
) {
    // id's 256x128 sky reads its two layers with no bounds check
    // ([`sky_layers_sample`]); any other texture, never id's, the guarded way.
    match sky_layers(pixels, tw) {
        Some(layers) => sky_span(out, u, v, count, view, |s, t| sky_layers_sample(layers, s, t, view.front)),
        None => sky_span(out, u, v, count, view, |s, t| sky_sample(pixels, tw, s, t, view.front)),
    }
}

/// The sky miptexture as every id map has it: `SKYSIZE` rows of the two
/// layers side by side, front then back.
type SkyLayers = [u8; LAYERS_WIDTH * SKYSIZE as usize];
/// The width of [`SkyLayers`]: two layers.
const LAYERS_WIDTH: usize = 2 * SKYSIZE as usize;

/// `pixels` as [`SkyLayers`], when it is a sky of id's shape (`tw` 256 wide,
/// 128 rows or more): an array, so that the texel indices
/// [`sky_layers_sample`] masks to `SKYMASK` are known to be inside it.
fn sky_layers(pixels: &[u8], tw: usize) -> Option<&SkyLayers> {
    if tw != LAYERS_WIDTH {
        return None;
    }
    pixels.get(..LAYERS_WIDTH * SKYSIZE as usize)?.try_into().ok()
}

/// [`sky_sample`] on id's 256x128 sky, for [`draw_sky_span`]'s pixel loop:
/// the same two texels, read with no bounds check (each index is inside the
/// array by its masks). A tenth off a sky pixel, which still costs about
/// twice a wall's: two texel addresses a pixel, and `D_Sky_uv_To_st`'s
/// square root and divisions every 32 (PERF_PLAN.md, §15).
#[inline]
fn sky_layers_sample(layers: &SkyLayers, s: i32, t: i32, front: i32) -> u8 {
    let texel = |c: i32| ((c >> 16) & SKYMASK) as usize;
    match layers[texel(t.wrapping_add(front)) * LAYERS_WIDTH + texel(s.wrapping_add(front))] {
        0 => layers[texel(t) * LAYERS_WIDTH + LAYERS_WIDTH / 2 + texel(s)],
        cloud => cloud,
    }
}

/// [`draw_sky_span`]'s walk of the span's 16.16 coordinates, each pixel
/// written as `sample(s, t)` (the tests sample other composites along the
/// same walk).
///
/// `D_DrawSkyScans8` works out a segment's exact end on reaching the
/// segment, and its pixels wait for that square root and those divisions.
/// Here each segment's end is asked for a segment ahead, before the pixels of
/// the one before, so it is worked out while they are drawn: the same values
/// (each end is a function of its pixel alone), a view half sky 6% faster in
/// the browser and 1-4% natively (PERF_PLAN.md, §15).
#[inline]
fn sky_span(out: &mut [u8], u: i32, v: i32, count: i32, view: &SkyView, sample: impl Fn(i32, i32) -> u8) {
    // Where the segment from pixel `k0` ends, exactly: the next segment's
    // first pixel when one follows (the C's `count -= spancount; if
    // (count)`), else the span's last pixel; none for a one-pixel last
    // segment, which takes no step.
    let end_of = |k0: i32| {
        if k0 + SKY_SPAN_MAX < count {
            Some(sky_uv_to_st(u + k0 + SKY_SPAN_MAX, v, view))
        } else if count - k0 > 1 {
            Some(sky_uv_to_st(u + count - 1, v, view))
        } else {
            None
        }
    };
    let (mut s, mut t) = sky_uv_to_st(u, v, view);
    let mut end = end_of(0);
    let mut out = out;
    let mut k0 = 0;
    while k0 < count {
        let full = k0 + SKY_SPAN_MAX < count;
        let ahead = if full { end_of(k0 + SKY_SPAN_MAX) } else { None };
        let spancount = (count - k0).min(SKY_SPAN_MAX);
        let (snext, tnext) = end.unwrap_or((s, t));
        let (sstep, tstep) = if full {
            (snext.wrapping_sub(s) >> SKY_SPAN_SHIFT, tnext.wrapping_sub(t) >> SKY_SPAN_SHIFT)
        } else if spancount > 1 {
            (snext.wrapping_sub(s) / (spancount - 1), tnext.wrapping_sub(t) / (spancount - 1))
        } else {
            (0, 0)
        };
        // The segment's pixels: as many as `out` still has (the callers'
        // `count` is `out`'s length).
        let n = (spancount as usize).min(out.len());
        let (segment, rest) = std::mem::take(&mut out).split_at_mut(n);
        for p in segment {
            *p = sample(s, t);
            s = s.wrapping_add(sstep);
            t = t.wrapping_add(tstep);
        }
        out = rest;
        (s, t, end) = (snext, tnext, ahead);
        k0 += SKY_SPAN_MAX;
    }
}

/// `r_skysource`'s miptexture: `R_InitSky` runs for every `sky*` miptexture
/// `Mod_LoadTextures` loads, so the map's last one wins.
pub(super) fn sky_texture(bsp: &Bsp) -> Option<&crate::bsp::MipTex> {
    bsp.textures
        .iter()
        .rev()
        .flatten()
        .find(|mt| classify_surface(&mt.name) == SurfKind::Sky && !mt.pixels.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{cross, normalize};
    use crate::render::fixtures::synthetic_sky_pixels;

    #[test]
    fn sky_sampler_renders_nonbackground_and_animates() {
        // The sky sampler must (1) produce real (non-framebuffer-background)
        // pixels — i.e. show the sky texture, not a flat fill — (2) differ between
        // two times (it scrolls), and (3) differ when the VIEW DIRECTION changes
        // (the sky is projected from the view ray, D_Sky_uv_To_st — not the wall
        // (s,t)).
        let pixels = synthetic_sky_pixels();

        let (w, h) = (48usize, 48usize);
        // Build a SkyView for a given look direction (forward), with an orthonormal
        // right/up basis. This stands in for the camera the world pass passes in.
        let make_view = |forward: Vec3, time: f32| {
            let (f, _) = normalize(forward);
            // right = forward x worldup, up = right x forward (orthonormal-ish).
            let (right, _) = normalize(cross(f, [0.0, 0.0, 1.0]));
            let (up, _) = normalize(cross(right, f));
            SkyView::new(f, right, up, w.max(h) as f32, ((w as i32) >> 1, (h as i32) >> 1), time, SkyScroll::Classic)
        };
        // The view as D_DrawSurfaces draws a sky surface covering it: one span
        // per row (the sky uses the view ray, not a face's (s,t)).
        let render_at = |view: SkyView| {
            let mut frame = vec![0u8; w * h]; // background = index 0
            for (y, row) in frame.chunks_mut(w).enumerate() {
                draw_sky_span(row, 0, y as i32, w as i32, &pixels, 256, &view);
            }
            frame
        };
        let a = render_at(make_view([1.0, 0.0, 0.0], 0.0)); // looking +X
        let b = render_at(make_view([1.0, 0.0, 0.0], 1.0));

        // (1) Non-background: the sky drew real texels (not a flat empty frame).
        let drawn = a.iter().filter(|&&p| p != 0).count();
        assert!(drawn > 0, "sky face rendered no pixels (should show the sky texture)");

        // (2) Animated: scrolling shifts the texels, so the two frames differ.
        let changed = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "sky must scroll (differ) between two times");

        // (3) View-dependent: looking a different direction shows a different patch
        // of sky (the whole point of projecting the view ray).
        let c = render_at(make_view([0.0, 1.0, 0.0], 0.0)); // looking +Y
        let view_diff = a.iter().zip(c.iter()).filter(|(x, y)| x != y).count();
        assert!(view_diff > 0, "sky must change with the view direction (dome projection)");
    }

    #[test]
    fn sky_sample_composites_the_shifted_front_over_the_back() {
        // R_MakeSky: where the front layer (left half) is transparent (index 0)
        // the back layer (right half, unshifted) shows through; where it is
        // opaque it wins — read `front` further along on both axes.
        let pixels = synthetic_sky_pixels();
        let tw = 256usize;
        let fx = |x: i32| x << 16; // a texel column as a 16.16 coordinate
        // Front column 0 is transparent (x < 42): the back's (0,0) = 1.
        assert_eq!(sky_sample(&pixels, tw, fx(0), fx(0), 0), 1);
        // Front column 64 is opaque: 200.
        assert_eq!(sky_sample(&pixels, tw, fx(64), fx(0), 0), 200);
        // Shifted by 50, column 0 reads the front's column 50 (opaque) ...
        assert_eq!(sky_sample(&pixels, tw, fx(0), fx(0), fx(50)), 200);
        // ... and column 100 wraps to the front's 150 & 127 = 22 (transparent),
        // so the back shows at the UNSHIFTED (100, 3): 1 + (100+3)%200.
        assert_eq!(sky_sample(&pixels, tw, fx(100), fx(3), fx(50)), 104);
        // A fraction moves the front's texel edge: column 41.75 is the front's
        // transparent 41 shifted by 0.2 and its opaque 42 shifted by 0.25.
        assert_eq!(sky_sample(&pixels, tw, fx(41) + 0xC000, 0, 0x3333), 1 + 41);
        assert_eq!(sky_sample(&pixels, tw, fx(41) + 0xC000, 0, 0x4000), 200);
        // Coordinates wrap at 128 texels (`R_SKY_SMASK`), negatives included.
        assert_eq!(sky_sample(&pixels, tw, fx(128 + 64), fx(-128), 0), 200);
        // A degenerate (too-small) sky texture never panics: index 0.
        let tiny = vec![0u8; 4];
        assert_eq!(sky_sample(&tiny, 2, fx(1000), fx(-1000), fx(5)), 0);
        // Near i32's edge the offset wraps without changing the texel read.
        assert_eq!(sky_sample(&pixels, tw, i32::MAX, 0, fx(1)), sky_sample(&pixels, tw, i32::MAX - fx(128), 0, fx(1)));
    }

    /// [`sky_layers_sample`], the span loop's unchecked read of id's 256x128
    /// sky, is [`sky_sample`] for every coordinate and offset:
    /// 16.16 values over the whole `i32` range, the patterned sky's
    /// transparent and opaque cloud texels both. A sky of any other shape has
    /// no [`SkyLayers`] and its spans keep the guarded read.
    #[test]
    fn the_unchecked_sky_sample_is_the_guarded_one() {
        let pixels = patterned_sky();
        let layers = sky_layers(&pixels, 256).expect("id's shape");
        let mut seed = 0x9E37_79B9u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i32
        };
        let (mut clouds, mut backs) = (0, 0);
        for i in 0..200_000 {
            // Whole-range values, and small ones around the texel edges.
            let (s, t, front) = if i % 2 == 0 { (next(), next(), next()) } else { (next() >> 9, next() >> 9, next() >> 12) };
            let want = sky_sample(&pixels, 256, s, t, front);
            assert_eq!(sky_layers_sample(layers, s, t, front), want, "s {s} t {t} front {front}");
            let cloud = pixels[(((t.wrapping_add(front)) >> 16) & 127) as usize * 256 + ((s.wrapping_add(front) >> 16) & 127) as usize];
            if cloud != 0 { clouds += 1 } else { backs += 1 }
        }
        assert!(clouds > 50_000 && backs > 50_000, "both layers read: {clouds} cloud, {backs} back");
        for edge in [i32::MIN, -1, 0, 0xFFFF, 0x1_0000, 127 << 16, 128 << 16, i32::MAX] {
            assert_eq!(sky_layers_sample(layers, edge, edge, 0), sky_sample(&pixels, 256, edge, edge, 0), "{edge}");
            assert_eq!(sky_layers_sample(layers, 0, 0, edge), sky_sample(&pixels, 256, 0, 0, edge), "front {edge}");
        }
        // Not id's shape: no layers, and the span is still drawn, guarded.
        assert!(sky_layers(&pixels, 128).is_none() && sky_layers(&pixels[..256 * 127], 256).is_none());
        assert!(sky_layers(&[0u8; 4], 2).is_none());
        let v = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), 3.3, SkyScroll::Fluid);
        let mut out = vec![9u8; 40];
        draw_sky_span(&mut out, 0, 60, 40, &[0u8; 4], 2, &v);
        assert!(out.iter().all(|&p| p == 0), "a degenerate sky draws index 0");
        // A span shorter than its count writes what fits, the same pixels.
        let (mut whole, mut short) = (vec![0u8; 100], vec![0u8; 70]);
        draw_sky_span(&mut whole, 5, 60, 100, &pixels, 256, &v);
        draw_sky_span(&mut short, 5, 60, 100, &pixels, 256, &v);
        assert_eq!(short[..], whole[..70]);
    }

    #[test]
    fn sky_view_front_layer_scrolls_twice_as_fast() {
        // R_SetSkyFrame + R_MakeSky: the whole sky scrolls skytime*8 texels
        // (D_Sky_uv_To_st) and the front layer another (int)(skytime*8) on top,
        // or in the fluid sky skytime*8 itself.
        let at = |time: f32, mode| SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), time, mode);
        let v = at(1.6, SkyScroll::Classic);
        assert_eq!((v.scroll, v.front), (12.8, 12 << 16));
        assert_eq!(at(1.6, SkyScroll::Fluid).front, (12.8f32 * 65536.0) as i32);
        assert_eq!(at(1.625, SkyScroll::Fluid).front, 13 << 16, "whole texels: id's");
        // skytime wraps at SKYSIZE*4*1 = 512 s.
        let w = at(513.0, SkyScroll::Classic);
        assert_eq!((w.scroll, w.front), (8.0, 8 << 16));
        // D_Sky_uv_To_st at the integer screen centre, looking along +X: the ray
        // is +X, so s = (scroll + 378) * 0x10000 and t = scroll * 0x10000.
        assert_eq!((v.half_w, v.half_h), (160, 100));
        let (s, t) = sky_uv_to_st(160, 100, &v);
        assert_eq!((s >> 16, t >> 16), (390, 12));
    }

    #[test]
    fn sky_span_steps_every_32_pixels_like_d_drawskyscans8() {
        // A 40-pixel span: exact at u0 and u0+32, stepped by (next-cur)>>5 in
        // between, then the 8-pixel tail stepped by division over 7.
        let pixels = synthetic_sky_pixels();
        let v = SkyView::new([0.6, 0.8, 0.0], [0.8, -0.6, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), 3.3, SkyScroll::Classic);
        let (u0, row, n) = (17, 60, 40);
        let mut out = vec![0u8; n as usize];
        draw_sky_span(&mut out, u0, row, n, &pixels, 256, &v);
        let (s0, t0) = sky_uv_to_st(u0, row, &v);
        let (s1, t1) = sky_uv_to_st(u0 + 32, row, &v);
        let (s2, t2) = sky_uv_to_st(u0 + 39, row, &v);
        let mut want = Vec::new();
        let (ss, ts) = ((s1 - s0) >> 5, (t1 - t0) >> 5);
        for i in 0..32 {
            want.push(sky_sample(&pixels, 256, s0 + i * ss, t0 + i * ts, v.front));
        }
        let (ss, ts) = ((s2 - s1) / 7, (t2 - t1) / 7);
        for i in 0..8 {
            want.push(sky_sample(&pixels, 256, s1 + i * ss, t1 + i * ts, v.front));
        }
        assert_eq!(out, want);
    }

    /// `D_DrawSkyScans8`'s walk in its own order — each segment's end
    /// worked out on reaching the segment — as [`sky_span`] was before it
    /// asked for the ends a segment ahead: every pixel's `(s, t)`.
    fn d_draw_sky_scans8_in_order(u: i32, v: i32, count: i32, view: &SkyView) -> Vec<(i32, i32)> {
        let (mut u, mut count) = (u, count);
        let (mut s, mut t) = sky_uv_to_st(u, v, view);
        let (mut sstep, mut tstep) = (0i32, 0i32);
        let mut walk = Vec::new();
        while count > 0 {
            let spancount = count.min(SKY_SPAN_MAX);
            count -= spancount;
            let (mut snext, mut tnext) = (s, t);
            if count > 0 {
                u += spancount;
                (snext, tnext) = sky_uv_to_st(u, v, view);
                sstep = snext.wrapping_sub(s) >> SKY_SPAN_SHIFT;
                tstep = tnext.wrapping_sub(t) >> SKY_SPAN_SHIFT;
            } else {
                let spancountminus1 = spancount - 1;
                if spancountminus1 > 0 {
                    u += spancountminus1;
                    (snext, tnext) = sky_uv_to_st(u, v, view);
                    sstep = snext.wrapping_sub(s) / spancountminus1;
                    tstep = tnext.wrapping_sub(t) / spancountminus1;
                }
            }
            for _ in 0..spancount {
                walk.push((s, t));
                s = s.wrapping_add(sstep);
                t = t.wrapping_add(tstep);
            }
            s = snext;
            t = tnext;
        }
        walk
    }

    /// [`sky_span`], which asks for each segment's end a segment ahead, walks
    /// the coordinates `D_DrawSkyScans8` walks: every pixel's `(s, t)` the
    /// same, at every span length from 0 to 300 (a one-pixel last segment,
    /// whole segments only, a tail of each length), from many starts, rows,
    /// eyes and times, both scrolls.
    #[test]
    fn the_sky_span_asked_ahead_walks_d_drawskyscans8s_coordinates() {
        let mut seed = 0x51ED_270Bu32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for case in 0..40 {
            let yaw = (next() % 6283) as f32 / 1000.0;
            let pitch = (next() % 1400) as f32 / 1000.0 - 0.2;
            let (cy, sy, cp, spi) = (yaw.cos(), yaw.sin(), pitch.cos(), pitch.sin());
            let forward = [cp * cy, cp * sy, spi];
            let right = [sy, -cy, 0.0];
            let up = [-spi * cy, -spi * sy, cp];
            let (w, h) = [(320, 200), (1920, 1080), (2640, 1080)][case % 3];
            let scroll = if case % 2 == 0 { SkyScroll::Classic } else { SkyScroll::Fluid };
            let view = SkyView::new(forward, right, up, w as f32, (w / 2, h / 2), (next() % 600_000) as f32 / 1000.0, scroll);
            for count in 0..=300 {
                let (u, v) = ((next() % w as u32) as i32 - 40, (next() % h as u32) as i32);
                let walk = std::cell::RefCell::new(Vec::new());
                let mut out = vec![0u8; count as usize];
                sky_span(&mut out, u, v, count, &view, |s, t| {
                    walk.borrow_mut().push((s, t));
                    0
                });
                assert_eq!(walk.into_inner(), d_draw_sky_scans8_in_order(u, v, count, &view), "case {case}, {count} pixels at ({u}, {v})");
            }
        }
    }

    #[test]
    fn the_dome_keeps_the_4_3_scale_under_hor_plus() {
        // D_Sky_uv_To_st's temp: id's max(width, height) whenever the view's
        // fov is the cvar's; Hor+ at 16:9 (fov_x 106.26 for fov 90) keeps the
        // 1440 of the 4:3 screen of the same height.
        assert_eq!(sky_dome_scale(320, 200, 90.0, 90.0), 320.0);
        assert_eq!(sky_dome_scale(200, 320, 90.0, 90.0), 320.0);
        let fov_x = crate::render::FovMode::HorPlus.fov_x(90.0, 1920, 1080, 1.0);
        assert!((sky_dome_scale(1920, 1080, 90.0, fov_x) - 1440.0).abs() < 0.01);
    }

    /// A 256x128 sky miptexture with both layers patterned on both axes: the
    /// front (left half) transparent at about a third of its texels, the back
    /// never 0.
    fn patterned_sky() -> Vec<u8> {
        let mut seed = 0x2545_F491u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        let mut px = vec![0u8; 256 * 128];
        for row in px.chunks_mut(256) {
            for (x, p) in row.iter_mut().enumerate() {
                let r = next();
                *p = if x < 128 { if r < 85 { 0 } else { r } } else { r.max(1) };
            }
        }
        px
    }

    /// id's `R_InitSky` and `R_MakeSky` (r_sky.c) as written — the 131-wide
    /// `bottomsky`/`bottommask` and `newsky = (top & mask) | bottom`, a byte at
    /// a time (the `!UNALIGNED_OK` loop) — for the sky `pixels` with every
    /// texel made an `m x m` block (`SKYSIZE` `128*m`), composited at
    /// `xshift = yshift = shift` of those finer texels. Returns `newsky`,
    /// `2*SKYSIZE` wide, and samples it as `D_DrawSkyScans8` does at a 16.16
    /// `(s,t)` of the unmagnified sky (scaled by `m`).
    fn id_newsky(pixels: &[u8], m: usize, shift: usize) -> impl Fn(i32, i32) -> u8 {
        let size = 128 * m;
        let mask = size - 1;
        let wide = size + 3;
        let src = |i: usize, j: usize| pixels[(i / m) * 256 + j / m];
        let mut newsky = vec![0u8; size * 2 * size];
        let (mut bottomsky, mut bottommask) = (vec![0u8; size * wide], vec![0u8; size * wide]);
        for i in 0..size {
            for j in 0..size {
                newsky[i * 2 * size + j + size] = src(i, j + size);
            }
            for j in 0..wide {
                let p = src(i, j & mask);
                (bottomsky[i * wide + j], bottommask[i * wide + j]) = if p != 0 { (p, 0) } else { (0, 0xff) };
            }
        }
        for y in 0..size {
            let baseofs = ((y + shift) & mask) * wide;
            for x in 0..size {
                let ofs = baseofs + ((x + shift) & mask);
                let p = y * 2 * size + x;
                newsky[p] = (newsky[p + size] & bottommask[ofs]) | bottomsky[ofs];
            }
        }
        move |s: i32, t: i32| {
            let texel = |c: i32| ((i64::from(c) * m as i64) >> 16) as usize & mask;
            newsky[texel(t) * 2 * size + texel(s)]
        }
    }

    /// A `w x h` view's sky, looking along `forward`, every row one span.
    fn sky_frame(forward: Vec3, w: usize, h: usize, sample: impl Fn(&mut [u8], i32, &SkyView), time: f32, mode: SkyScroll) -> Vec<u8> {
        let (f, _) = normalize(forward);
        let side = if f[2].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
        let (right, _) = normalize(cross(f, side));
        let (up, _) = normalize(cross(right, f));
        let view = SkyView::new(f, right, up, w.max(h) as f32, ((w as i32) >> 1, (h as i32) >> 1), time, mode);
        let mut frame = vec![0u8; w * h];
        for (v, row) in frame.chunks_mut(w).enumerate() {
            sample(row, v as i32, &view);
        }
        frame
    }

    #[test]
    fn each_sky_is_ids_composite_sampled_at_its_offset() {
        // Classic is R_MakeSky's newsky at xshift = (int)(skytime*8), sampled
        // along D_DrawSkyScans8's walk. Fluid, at any time whose skytime*8 is a
        // multiple of 1/8 texel, is the same code's newsky for the sky with
        // texels 8 times finer at xshift = 8*skytime*8 of them — the composite
        // at the exact offset, every pixel still a texel of the sky's own.
        let pixels = patterned_sky();
        let (w, h) = (96usize, 60usize);
        let looks: [Vec3; 3] = [[0.0, 0.0, 1.0], [1.0, 0.3, 0.05], [-0.5, 0.7, 0.6]];
        // skytime*8 = 8 + j/8, 2400 + j/8, and past the 512 s wrap.
        for base in [1.0f32, 300.0, 600.0] {
            for j in 0..8 {
                let time = base + j as f32 / 64.0;
                let scroll = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 1.0, (0, 0), time, SkyScroll::Fluid).scroll;
                assert_eq!((scroll * 8.0).fract(), 0.0, "{time}: a whole number of eighths");
                let classic_ref = id_newsky(&pixels, 1, scroll as usize);
                let fluid_ref = id_newsky(&pixels, 8, (scroll * 8.0) as usize);
                for look in looks {
                    let draw = |mode| sky_frame(look, w, h, |row, v, view| draw_sky_span(row, 0, v, w as i32, &pixels, 256, view), time, mode);
                    let walk = |id: &dyn Fn(i32, i32) -> u8| sky_frame(look, w, h, |row, v, view| sky_span(row, 0, v, w as i32, view, id), time, SkyScroll::Classic);
                    let (classic, fluid) = (draw(SkyScroll::Classic), draw(SkyScroll::Fluid));
                    assert!(classic == walk(&classic_ref), "{time} {look:?}: Classic is id's newsky");
                    assert!(fluid == walk(&fluid_ref), "{time} {look:?}: Fluid is id's newsky at the exact offset");
                    if j == 0 {
                        assert!(fluid == classic, "{time} {look:?}: at a whole texel the fluid sky is id's frame");
                    } else {
                        assert!(fluid != classic, "{time} {look:?}: between whole texels the clouds have moved on");
                    }
                }
            }
        }
    }

    #[test]
    fn the_fluid_clouds_change_as_many_pixels_every_frame() {
        // A quarter second at 240 Hz, looking up: how many of the view's
        // pixels change from one frame to the next. id's clouds glide with the
        // back layer between their jumps (about 6% of this view a frame) and
        // jump a texel when (int)(skytime*8) steps (89%, at 10.125 s and
        // 10.25 s); the fluid clouds change the same share every frame (13%).
        let pixels = patterned_sky();
        let (w, h) = (160usize, 100usize);
        let frame = |time: f32, mode| {
            sky_frame([0.0, 0.0, 1.0], w, h, |row, v, view| draw_sky_span(row, 0, v, w as i32, &pixels, 256, view), time, mode)
        };
        let changed = |mode| -> Vec<usize> {
            let frames: Vec<Vec<u8>> = (0..=60).map(|k| frame(10.0 + k as f32 / 240.0, mode)).collect();
            frames.windows(2).map(|p| p[0].iter().zip(&p[1]).filter(|(a, b)| a != b).count()).collect()
        };
        let (classic, fluid) = (changed(SkyScroll::Classic), changed(SkyScroll::Fluid));
        let cmax = classic.iter().max().copied().unwrap_or(0);
        let (fmin, fmax) = (fluid.iter().min().copied().unwrap_or(0), fluid.iter().max().copied().unwrap_or(0));
        let lurches = classic.iter().filter(|&&n| n * 4 > w * h).count();
        assert_eq!(lurches, 2, "{classic:?}");
        assert!(fmax * 3 < cmax, "the fluid sky's busiest frame {fmax} against id's lurch {cmax}");
        assert!(fmax * 2 < fmin * 3, "the fluid sky changes a steady share: {fluid:?}");
    }
}
