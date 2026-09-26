//! The scrolling two-layer sky.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sky.c` (`R_MakeSky`'s layer offsets) and `WinQuake/d_sky.c`
//! (`D_Sky_uv_To_st`, `D_DrawSkyScans8`'s 32-pixel spans).

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::surf::{classify_surface, SurfKind};

/// `SKYSIZE` (d_iface.h): each sky layer is 128x128 texels.
const SKYSIZE: i32 = 128;
/// `SKYMASK` (d_iface.h), and `R_SKY_SMASK`/`R_SKY_TMASK >> 16` (d_local.h).
const SKYMASK: i32 = SKYSIZE - 1;
/// `iskyspeed` (r_sky.c): the scroll, in texels per second.
const SKY_SPEED: f32 = 8.0;

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
    /// `R_MakeSky`'s `xshift`/`yshift` = `(int)(skytime*skyspeed)`: the extra
    /// offset of the front (cloud) layer, so it moves at twice the back's speed.
    shift: i32,
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
    /// `time`. `R_SetSkyFrame`
    /// (r_sky.c): `skytime = cl.time - (int)(cl.time/temp)*temp` with
    /// `temp = SKYSIZE*s1*s2` = 512, where `s1`/`s2` are `iskyspeed` 8 and
    /// `iskyspeed2` 2 over their gcd.
    pub(super) fn new(
        forward: Vec3,
        right: Vec3,
        up: Vec3,
        longest: f32,
        centre: (i32, i32),
        time: f32,
    ) -> SkyView {
        const TEMP: f64 = 512.0;
        let t = time as f64;
        let skytime = (t - ((t / TEMP) as i32 as f64) * TEMP) as f32;
        let scroll = skytime * SKY_SPEED;
        SkyView {
            forward,
            right,
            up,
            half_w: centre.0,
            half_h: centre.1,
            longest,
            scroll,
            shift: scroll as i32,
        }
    }
}

/// `D_Sky_uv_To_st` (d_sky.c): the 16.16 sky coordinates for screen pixel
/// `(u,v)` — build the ray `4096*vpn + wu*vright + wv*vup` (screen offsets from
/// the integer centre scaled by `8192/longest`), squash it vertically
/// (`end[2] *= 3`), normalise, then `s = (skytime*skyspeed + 6*(SKYSIZE/2-1)*end[0])
/// * 0x10000` and the same for `t` with `end[1]`. Float math as the C (`wu`/`wv`
/// computed in double, stored to float; `VectorNormalize` multiplies by `1/length`).
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
/// shifted by `shift` texels on both axes, over the back layer (the RIGHT half,
/// unshifted).
///
/// `pixels` is the `tw`-wide sky miptexture (256x128 in every id map). Every read
/// is `.get()`-guarded, so a malformed sky never panics (it yields index 0).
#[inline]
fn sky_sample(pixels: &[u8], tw: usize, s: i32, t: i32, shift: i32) -> u8 {
    let x = (s >> 16) & SKYMASK;
    let y = (t >> 16) & SKYMASK;
    let fy = ((y + shift) & SKYMASK) as usize;
    let fx = ((x + shift) & SKYMASK) as usize;
    match pixels.get(fy * tw + fx).copied() {
        Some(front) if front != 0 => front,
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
    let mut u = u;
    let mut count = count;
    let (mut s, mut t) = sky_uv_to_st(u, v, view);
    let (mut sstep, mut tstep) = (0i32, 0i32);
    let mut out = out.iter_mut();
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
            if let Some(p) = out.next() {
                *p = sky_sample(pixels, tw, s, t, view.shift);
            }
            s = s.wrapping_add(sstep);
            t = t.wrapping_add(tstep);
        }
        s = snext;
        t = tnext;
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
            SkyView::new(f, right, up, w.max(h) as f32, ((w as i32) >> 1, (h as i32) >> 1), time)
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
        // opaque it wins — read `shift` texels further along on both axes.
        let pixels = synthetic_sky_pixels();
        let tw = 256usize;
        let fx = |x: i32| x << 16; // a texel column as a 16.16 coordinate
        // Front column 0 is transparent (x < 42): the back's (0,0) = 1.
        assert_eq!(sky_sample(&pixels, tw, fx(0), fx(0), 0), 1);
        // Front column 64 is opaque: 200.
        assert_eq!(sky_sample(&pixels, tw, fx(64), fx(0), 0), 200);
        // Shifted by 50, column 0 reads the front's column 50 (opaque) ...
        assert_eq!(sky_sample(&pixels, tw, fx(0), fx(0), 50), 200);
        // ... and column 100 wraps to the front's 150 & 127 = 22 (transparent),
        // so the back shows at the UNSHIFTED (100, 3): 1 + (100+3)%200.
        assert_eq!(sky_sample(&pixels, tw, fx(100), fx(3), 50), 104);
        // Coordinates wrap at 128 texels (`R_SKY_SMASK`), negatives included.
        assert_eq!(sky_sample(&pixels, tw, fx(128 + 64), fx(-128), 0), 200);
        // A degenerate (too-small) sky texture never panics: index 0.
        let tiny = vec![0u8; 4];
        assert_eq!(sky_sample(&tiny, 2, fx(1000), fx(-1000), 5), 0);
    }

    #[test]
    fn sky_view_front_layer_scrolls_twice_as_fast() {
        // R_SetSkyFrame + R_MakeSky: the whole sky scrolls skytime*8 texels
        // (D_Sky_uv_To_st) and the front layer another (int)(skytime*8) on top.
        let v = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), 1.6);
        assert_eq!((v.scroll, v.shift), (12.8, 12));
        // skytime wraps at SKYSIZE*4*1 = 512 s.
        let w = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), 513.0);
        assert_eq!((w.scroll, w.shift), (8.0, 8));
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
        let v = SkyView::new([0.6, 0.8, 0.0], [0.8, -0.6, 0.0], [0.0, 0.0, 1.0], 320.0, (160, 100), 3.3);
        let (u0, row, n) = (17, 60, 40);
        let mut out = vec![0u8; n as usize];
        draw_sky_span(&mut out, u0, row, n, &pixels, 256, &v);
        let (s0, t0) = sky_uv_to_st(u0, row, &v);
        let (s1, t1) = sky_uv_to_st(u0 + 32, row, &v);
        let (s2, t2) = sky_uv_to_st(u0 + 39, row, &v);
        let mut want = Vec::new();
        let (ss, ts) = ((s1 - s0) >> 5, (t1 - t0) >> 5);
        for i in 0..32 {
            want.push(sky_sample(&pixels, 256, s0 + i * ss, t0 + i * ts, v.shift));
        }
        let (ss, ts) = ((s2 - s1) / 7, (t2 - t1) / 7);
        for i in 0..8 {
            want.push(sky_sample(&pixels, 256, s1 + i * ss, t1 + i * ts, v.shift));
        }
        assert_eq!(out, want);
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
}
