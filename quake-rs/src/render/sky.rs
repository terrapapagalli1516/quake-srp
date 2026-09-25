//! The scrolling two-layer sky.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sky.c` (`R_MakeSky`'s layer offsets) and `WinQuake/d_sky.c`
//! (`D_Sky_uv_To_st`, `D_DrawSkyScans8`'s 32-pixel spans).

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::Image;
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
    /// `D_Sky_uv_To_st`: a fixed dome angle, independent of the render FOV.
    longest: f32,
    /// `skytime*skyspeed`, added to both `s` and `t` (`D_Sky_uv_To_st`).
    scroll: f32,
    /// `R_MakeSky`'s `xshift`/`yshift` = `(int)(skytime*skyspeed)`: the extra
    /// offset of the front (cloud) layer, so it moves at twice the back's speed.
    shift: i32,
}

impl SkyView {
    /// The sky state for a `w`x`h` view whose screen's centre is `centre` in
    /// its own pixels ([`RenderOptions`](super::RenderOptions)`::sky_centre`),
    /// at game `time`. `R_SetSkyFrame`
    /// (r_sky.c): `skytime = cl.time - (int)(cl.time/temp)*temp` with
    /// `temp = SKYSIZE*s1*s2` = 512, where `s1`/`s2` are `iskyspeed` 8 and
    /// `iskyspeed2` 2 over their gcd.
    pub(super) fn new(
        forward: Vec3,
        right: Vec3,
        up: Vec3,
        w: usize,
        h: usize,
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
            longest: w.max(h) as f32,
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

/// Sample the sky for screen pixel `(u, v)`: [`sky_uv_to_st`] then
/// [`sky_sample`]. (id evaluates `D_Sky_uv_To_st` exactly only every 32 pixels
/// of a span and steps linearly between — see [`resolve_sky_spans`].)
#[inline]
pub(super) fn sky_texel_view(pixels: &[u8], tw: usize, u: i32, v: i32, sky: &SkyView) -> u8 {
    let (s, t) = sky_uv_to_st(u, v, sky);
    sky_sample(pixels, tw, s, t, sky.shift)
}

/// `SKY_SPAN_SHIFT` (d_sky.c): `D_DrawSkyScans8` evaluates `D_Sky_uv_To_st`
/// exactly every `1 << 5` = 32 pixels of a span and steps linearly between.
const SKY_SPAN_SHIFT: i32 = 5;
const SKY_SPAN_MAX: i32 = 1 << SKY_SPAN_SHIFT;

/// The world pass's sky pixels, kept until the brush passes are done so they
/// can be drawn as id draws them: `D_DrawSkyScans8` walks each sky SPAN — a run
/// of pixels on one scanline where one sky face is the nearest surface (what
/// `R_LeadingEdge`/`R_TrailingEdge` emit) — and interpolates the sky
/// coordinates across 32-pixel segments from the span's first pixel. Which
/// pixels form a span is only known once every nearer surface is drawn, so the
/// world pass records `(face, depth)` per sky pixel and [`resolve_sky_spans`]
/// recovers the runs afterwards: a pixel is still sky iff the z-buffer still
/// holds the depth the sky wrote (any nearer write lowers it).
pub(super) struct SkySpans {
    w: usize,
    h: usize,
    /// Per pixel: `1 +` the sky face that won the depth test there (0 = none).
    key: Vec<u32>,
    /// Per pixel: the depth that sky face wrote to the z-buffer.
    depth: Vec<f32>,
    /// Rows written since the last reset, `[lo, hi)`.
    lo: usize,
    hi: usize,
    /// The frame's sky state (every sky face of a frame shares it).
    pub(super) view: Option<SkyView>,
}

impl SkySpans {
    pub(super) const EMPTY: SkySpans =
        SkySpans { w: 0, h: 0, key: Vec::new(), depth: Vec::new(), lo: 0, hi: 0, view: None };

    /// Start a frame of `w`x`h`: forget the previous frame's pixels (only the
    /// rows it touched are cleared, so a sky-less frame costs nothing).
    pub(super) fn reset(&mut self, w: usize, h: usize) {
        let n = w.saturating_mul(h);
        if self.w != w || self.h != h || self.key.len() != n {
            self.key = vec![0; n];
            self.depth = vec![0.0; n];
            self.w = w;
            self.h = h;
        } else if self.lo < self.hi {
            self.key[self.lo * w..self.hi * w].fill(0);
        }
        self.lo = h;
        self.hi = 0;
        self.view = None;
    }

    #[inline]
    pub(super) fn record(&mut self, idx: usize, row: usize, key: u32, depth: f32) {
        if let (Some(k), Some(d)) = (self.key.get_mut(idx), self.depth.get_mut(idx)) {
            *k = key;
            *d = depth;
            self.lo = self.lo.min(row);
            self.hi = self.hi.max(row + 1);
        }
    }
}

/// `D_DrawSkyScans8` (d_sky.c) for one span of `count` pixels starting at screen
/// `(u, v)`, written into `out` (that scanline's pixels from `u`): the sky
/// coordinates are exact at the span start and every 32 pixels, stepped by
/// `(next - cur) >> 5` between; the last segment steps by an integer division
/// over its `count - 1` so it ends exactly on the span's last pixel.
#[allow(clippy::too_many_arguments)]
fn draw_sky_span(
    out: &mut [[u8; 3]],
    u: i32,
    v: i32,
    count: i32,
    pixels: &[u8],
    tw: usize,
    view: &SkyView,
    palette: &[[u8; 3]; 256],
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
                *p = palette[sky_sample(pixels, tw, s, t, view.shift) as usize];
            }
            s = s.wrapping_add(sstep);
            t = t.wrapping_add(tstep);
        }
        s = snext;
        t = tnext;
    }
}

/// Draw the world pass's deferred sky ([`SkySpans`]) as `D_DrawSkyScans8` does,
/// once every brush surface that can occlude it is in the z-buffer (and before
/// the alias models, which in id are drawn after `D_DrawSurfaces` too). Each run
/// of pixels on a row that one sky face still owns is one span. The texture is
/// `r_skysource`: `R_InitSky` runs for every `sky*` miptexture `Mod_LoadTextures`
/// loads, so the last one wins.
pub(super) fn resolve_sky_spans(image: &mut Image, zbuf: &[f32], bsp: &Bsp, palette: &[[u8; 3]; 256]) {
    SKY_SPANS_SCRATCH.with(|cell| {
        let mut sp = cell.borrow_mut();
        let (w, h) = (image.w, image.h);
        let sky_tex = bsp
            .textures
            .iter()
            .rev()
            .flatten()
            .find(|mt| classify_surface(&mt.name) == SurfKind::Sky && !mt.pixels.is_empty());
        if let (Some(view), Some(mt), true) = (sp.view, sky_tex, sp.w == w && sp.h == h) {
            let tw = mt.width as usize;
            for y in sp.lo..sp.hi.min(h) {
                let row = y * w;
                let mut x = 0usize;
                while x < w {
                    let k = sp.key[row + x];
                    let live = |x: usize| {
                        sp.key[row + x] == k && zbuf.get(row + x) == Some(&sp.depth[row + x])
                    };
                    if k == 0 || !live(x) {
                        x += 1;
                        continue;
                    }
                    let u0 = x;
                    while x < w && live(x) {
                        x += 1;
                    }
                    if let Some(out) = image.rgb.get_mut(row + u0..row + x) {
                        draw_sky_span(out, u0 as i32, y as i32, (x - u0) as i32, &mt.pixels, tw, &view, palette);
                    }
                }
            }
        }
        sp.reset(w, h);
    });
}

thread_local! {
    /// Per-thread deferred sky pixels ([`SkySpans`]): the world pass records
    /// into it, [`resolve_sky_spans`] draws and resets it once the brush passes
    /// are done.
    pub(super) static SKY_SPANS_SCRATCH: std::cell::RefCell<SkySpans> = const { std::cell::RefCell::new(SkySpans::EMPTY) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{cross, normalize};
    use crate::render::fixtures::synthetic_sky_pixels;
    use crate::render::raster::{outline, raster_poly_tex, AttrVert, PolyGrads, SurfaceMode};

    #[test]
    fn sky_sampler_renders_nonbackground_and_animates() {
        // The sky sampler must (1) produce real (non-framebuffer-background)
        // pixels — i.e. show the sky texture, not a flat fill — (2) differ between
        // two times (it scrolls), and (3) differ when the VIEW DIRECTION changes
        // (the sky is projected from the view ray, D_Sky_uv_To_st — not the wall
        // (s,t)).
        let pixels = synthetic_sky_pixels();
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            // Map every index to a distinct, clearly non-zero colour so any
            // sampled sky texel is visibly different from the [0,0,0] background.
            *p = [(i as u8).max(1), 255u8.saturating_sub(i as u8), 128];
        }

        let (w, h) = (48usize, 48usize);
        // Build a SkyView for a given look direction (forward), with an orthonormal
        // right/up basis. This stands in for the camera the world pass passes in.
        let make_view = |forward: Vec3, time: f32| {
            let (f, _) = normalize(forward);
            // right = forward x worldup, up = right x forward (orthonormal-ish).
            let (right, _) = normalize(cross(f, [0.0, 0.0, 1.0]));
            let (up, _) = normalize(cross(right, f));
            SkyView::new(f, right, up, w, h, ((w as i32) >> 1, (h as i32) >> 1), time)
        };
        let render_at = |view: SkyView| {
            let mut img = Image::new(w, h, [0, 0, 0]); // background = pure black
            let mut zb = vec![f32::INFINITY; w * h];
            // The (s,t) here are IGNORED by the sky path (it uses the view ray),
            // but a covering triangle is still needed to rasterise the screen area.
            let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
            let tri = [v0, v1, v2];
            let g = PolyGrads::from_vertices(&tri).expect("triangle");
            raster_poly_tex(
                &mut img, &mut zb, &outline(&tri), &g,
                &pixels, 256, 128, &pal, 1.0, None,
                SurfaceMode::Sky { view, defer: None },
                None,
            );
            img
        };
        let a = render_at(make_view([1.0, 0.0, 0.0], 0.0)); // looking +X
        let b = render_at(make_view([1.0, 0.0, 0.0], 1.0));

        // (1) Non-background: the sky drew real texels (not a flat empty frame).
        let drawn = a.rgb.iter().filter(|&&p| p != [0, 0, 0]).count();
        assert!(drawn > 0, "sky face rendered no pixels (should show the sky texture)");

        // (2) Animated: scrolling shifts the texels, so the two frames differ.
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "sky must scroll (differ) between two times");

        // (3) View-dependent: looking a different direction shows a different patch
        // of sky (the whole point of projecting the view ray).
        let c = render_at(make_view([0.0, 1.0, 0.0], 0.0)); // looking +Y
        let view_diff = a.rgb.iter().zip(c.rgb.iter()).filter(|(x, y)| x != y).count();
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
        let v = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320, 200, (160, 100), 1.6);
        assert_eq!((v.scroll, v.shift), (12.8, 12));
        // skytime wraps at SKYSIZE*4*1 = 512 s.
        let w = SkyView::new([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], 320, 200, (160, 100), 513.0);
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
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, 0, 0];
        }
        let v = SkyView::new([0.6, 0.8, 0.0], [0.8, -0.6, 0.0], [0.0, 0.0, 1.0], 320, 200, (160, 100), 3.3);
        let (u0, row, n) = (17, 60, 40);
        let mut out = vec![[0u8; 3]; n as usize];
        draw_sky_span(&mut out, u0, row, n, &pixels, 256, &v, &pal);
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
        let got: Vec<u8> = out.iter().map(|p| p[0]).collect();
        assert_eq!(got, want);
    }
}
