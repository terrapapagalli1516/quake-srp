//! The span routines `D_DrawSurfaces` draws brush surfaces with, and their
//! gradients.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `d_edge.c`'s `D_CalcGradients` ([`calc_gradients`]), `d_draw16.s`'s
//! `D_DrawSpans16` and `d_scan.c`'s `D_DrawSpans8` ([`span_cached`]),
//! `d_scan.c`'s `Turbulent8` ([`span_turb`]); how often they divide is
//! [`PerspSpan`]. [`super::edge`] hands each surface its spans. The flat
//! bounding-box triangle ([`raster_triangle`]) is the untextured debug
//! renderer's ([`super::render_bsp`]).

use super::light::{COLORMAP_LEN, LightMap, colormap_row};
use super::warp::{TURB_COORD_MASK, TurbTable, turb_phase};
use super::{Image, Palette, nearest_index};
use crate::math::{Vec3, dot};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A vertex after projection: integer-ish screen position kept as `f32` for
/// sub-pixel barycentric coverage, plus the camera-space forward depth used by
/// the z-buffer.
#[derive(Clone, Copy)]
pub(super) struct Projected {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) depth: f32,
}

/// A small integer hash (splitmix-ish) of a surface index, spreading
/// adjacent indices apart.
fn surface_hash(index: i64) -> u64 {
    let mut h = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^ (h >> 32)
}

/// Hash a (non-negative) surface index to a stable, reasonably saturated RGB
/// base colour, so distinct textures/surfaces get distinct hues across runs
/// (the true-colour debug view, [`super::render_bsp`]).
pub(super) fn hash_color(index: i64) -> [f32; 3] {
    // Derive a hue in [0,1); keep saturation/value high but not blinding.
    let hue = ((surface_hash(index) & 0xFFFF) as f32) / 65536.0;
    hsv_to_rgb(hue, 0.55, 0.95)
}

/// Hash a surface index to a stable palette index for a textureless face
/// (test maps): one of the middle 192, so its shading shows either way.
pub(super) fn hash_index(index: i64) -> u8 {
    32 + (surface_hash(index.wrapping_add(1)) % 192) as u8
}

/// Palette index `index` shaded by `shade` the port's linear way: the
/// palette entry nearest its colour times `shade` (textureless or
/// colormap-less synthetic faces; id's walls go through the colormap).
pub(super) fn shade_index(palette: &Palette, index: u8, shade: f32) -> u8 {
    nearest_index(palette, palette[usize::from(index)].map(|v| (f32::from(v) * shade).clamp(0.0, 255.0) as u8))
}

/// Convert HSV (each in `[0,1]`) to linear-ish RGB in `[0,1]`.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h.fract().abs() * 6.0).min(5.999_999);
    let i = h6 as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Edge function: signed area (times two) of the triangle `(a, b, c)`. Positive
/// when `c` is to the left of the directed edge `a -> b` in screen space.
#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

/// Rasterise one projected triangle into `image`/`zbuf` with barycentric
/// coverage and per-pixel depth interpolation. `color` is the already-shaded
/// 8-bit RGB for the whole triangle (flat shading).
pub(super) fn raster_triangle(
    image: &mut Image<[u8; 3]>,
    zbuf: &mut [f32],
    v0: Projected,
    v1: Projected,
    v2: Projected,
    color: [u8; 3],
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 {
        return;
    }

    // Screen-space bounding box, clamped to the framebuffer.
    let min_xf = v0.x.min(v1.x).min(v2.x);
    let max_xf = v0.x.max(v1.x).max(v2.x);
    let min_yf = v0.y.min(v1.y).min(v2.y);
    let max_yf = v0.y.max(v1.y).max(v2.y);

    // Reject triangles entirely off-screen or with non-finite coordinates.
    if !(min_xf.is_finite() && max_xf.is_finite() && min_yf.is_finite() && max_yf.is_finite()) {
        return;
    }

    let min_x = min_xf.floor().max(0.0) as i64;
    let max_x = max_xf.ceil().min((w as i64 - 1) as f32) as i64;
    let min_y = min_yf.floor().max(0.0) as i64;
    let max_y = max_yf.ceil().min((h as i64 - 1) as f32) as i64;
    if min_x > max_x || min_y > max_y {
        return;
    }

    // Twice the signed triangle area; if ~0 the triangle is degenerate.
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;
    // FAITHFULNESS (perspective-correct depth): Quake's span renderer keyed the
    // z-buffer on `1/z` (`d_scan.c`/`d_edge.c` interpolate `zi`), because `1/z`
    // is linear in screen space while view-`z` is NOT — so a linear `vz`
    // interpolation mis-sorts intersecting polys. We interpolate `1/z` (exact in
    // screen space) and recover the true per-pixel depth `z = 1/(Σ wᵢ/zᵢ)`. That
    // keeps the existing "smaller depth = nearer" convention (and the `INFINITY`
    // init / `<` tests everywhere) intact while making the test perspective
    // correct. A vertex with non-positive depth is guarded below.
    let (iz0, iz1, iz2) = (1.0 / v0.depth, 1.0 / v1.depth, 1.0 / v2.depth);

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            // Sample at pixel centres.
            let sx = px as f32 + 0.5;
            let sy = py as f32 + 0.5;

            // Barycentric weights via edge functions.
            let w0 = edge(v1.x, v1.y, v2.x, v2.y, sx, sy) * inv_area;
            let w1 = edge(v2.x, v2.y, v0.x, v0.y, sx, sy) * inv_area;
            let w2 = edge(v0.x, v0.y, v1.x, v1.y, sx, sy) * inv_area;

            // Inside test: all weights non-negative (covers both windings since
            // inv_area carries the sign).
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }

            // Perspective-correct depth: interpolate 1/z (linear in screen space)
            // and invert. `inv_z <= 0` means a vertex was at/behind the eye; skip.
            let inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
            if inv_z <= 0.0 {
                continue;
            }
            let depth = 1.0 / inv_z;

            // Indices are provably in range: px in [0, w-1], py in [0, h-1].
            let idx = (py as usize) * w + (px as usize);
            if let Some(z) = zbuf.get_mut(idx) {
                if depth < *z {
                    *z = depth;
                    if let Some(p) = image.pixels.get_mut(idx) {
                        *p = color;
                    }
                }
            }
        }
    }
}

/// A synthetic polygon vertex with its depth and texel coordinates, for tests
/// that have no face plane: see [`PolyGrads::from_vertices`].
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) struct AttrVert {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) vz: f32,
    pub(super) s: f32,
    pub(super) t: f32,
}

// ---------------------------------------------------------------------------
// Gradients and spans
// ---------------------------------------------------------------------------
//
// Pixel `(u, v)`'s centre is `(u + 0.5, v + 0.5)` in the gradients' screen
// coordinates (id's centres are on the integers, `xcenter = w/2 - 0.5`).

/// A screen-plane linear function `a(x, y) = o + dx*x + dy*y` over the
/// continuous screen coordinates — one of `D_CalcGradients`' three planes
/// (`d_ziorigin`/`d_zistepu`/`d_zistepv` and the `s/z`, `t/z` pairs).
#[derive(Clone, Copy)]
struct Linear {
    o: f64,
    dx: f64,
    dy: f64,
}

impl Linear {
    #[inline]
    fn at(&self, x: f64, y: f64) -> f64 {
        self.o + self.dx * x + self.dy * y
    }
}

/// The projection a face's gradients are taken in: the view basis (`vright`,
/// `vup`, `vpn`) and `R_ViewChanged`'s centre and scales, as the brush passes
/// project a vertex: `x = cx + xscale*vx/vz`, `y = cy - yscale*vy/vz`
/// ([`Projection`](super::Projection); `yscale = xscale * pixelAspect`).
#[derive(Clone, Copy)]
pub(super) struct ScreenProj {
    pub(super) forward: Vec3,
    pub(super) right: Vec3,
    pub(super) up: Vec3,
    pub(super) cx: f32,
    pub(super) cy: f32,
    pub(super) xscale: f32,
    pub(super) yscale: f32,
}

/// A face's screen-plane gradients of `1/z`, `s/z` and `t/z`, with `s`/`t`
/// relative to the eye's, plus the eye's `(s, t)` to add back: what
/// `D_CalcGradients` (`d_sdivz*`, `d_tdivz*`, `sadjust`, `tadjust`) and
/// `R_RenderFace` (`d_zi*`) leave for `D_DrawSpans8`.
///
/// The split at the eye is id's: `(s - s_eye)/z` is the texinfo axis dotted
/// with the view ray — about one texel per unit anywhere on screen — while an
/// absolute `s/z` carries the texture offset and the map coordinate (thousands
/// of texels) into every step's rounding.
#[derive(Clone, Copy)]
pub(super) struct PolyGrads {
    zi: Linear,
    sz: Linear,
    tz: Linear,
    st_eye: [f64; 2],
}

/// An eye within this distance of a face's plane sees it edge-on: no pixel.
const MIN_PLANE_DIST: f64 = 1e-6;

impl PolyGrads {
    /// The gradients of the face on the plane `normal . p = dist` with texinfo
    /// `ti` (none: `s = t = 0`), seen from `eye` — all in the face's model space
    /// (a brush entity passes the eye minus its origin). Analytic, as id derives
    /// them: `1/z` from the view-space normal over the eye's distance to the
    /// plane (`R_RenderFace`'s `distinv`), `s/z` and `t/z` from the view-space
    /// texinfo axes (`D_CalcGradients`' `TransformVector`), in f64. `None` when
    /// the eye is on the plane.
    pub(super) fn for_plane(
        view: &ScreenProj,
        eye: Vec3,
        normal: Vec3,
        dist: f32,
        ti: Option<&crate::bsp::TexInfo>,
    ) -> Option<PolyGrads> {
        let d3 = |a: [f64; 3], b: Vec3| a[0] * b[0] as f64 + a[1] * b[1] as f64 + a[2] * b[2] as f64;
        let as64 = |v: Vec3| [v[0] as f64, v[1] as f64, v[2] as f64];
        // TransformVector: into (right, up, forward) components.
        let tv = |v: [f64; 3]| [d3(v, view.right), d3(v, view.up), d3(v, view.forward)];
        let eye64 = as64(eye);
        let (cx, cy) = (view.cx as f64, view.cy as f64);
        let (inv_xs, inv_ys) = (1.0 / view.xscale as f64, 1.0 / view.yscale as f64);
        // A view-space vector `p` gives the screen plane `p . (x', y', 1)` with
        // `x' = (x - cx)/xscale`, `y' = (cy - y)/yscale` (the ray through (x, y)).
        let plane = |p: [f64; 3], scale: f64| {
            let dx = p[0] * inv_xs * scale;
            let dy = -p[1] * inv_ys * scale;
            Linear { o: p[2] * scale - cx * dx - cy * dy, dx, dy }
        };
        let denom = dist as f64 - d3(eye64, normal);
        if denom.is_nan() || denom.abs() < MIN_PLANE_DIST {
            return None;
        }
        let zi = plane(tv(as64(normal)), 1.0 / denom);
        let (sz, tz, st_eye) = match ti {
            Some(ti) => {
                let axis = |k: usize| [ti.vecs[k][0] as f64, ti.vecs[k][1] as f64, ti.vecs[k][2] as f64];
                let st = |k: usize| {
                    let a = axis(k);
                    a[0] * eye64[0] + a[1] * eye64[1] + a[2] * eye64[2] + ti.vecs[k][3] as f64
                };
                (plane(tv(axis(0)), 1.0), plane(tv(axis(1)), 1.0), [st(0), st(1)])
            }
            None => {
                let zero = Linear { o: 0.0, dx: 0.0, dy: 0.0 };
                (zero, zero, [0.0, 0.0])
            }
        };
        Some(PolyGrads { zi, sz, tz, st_eye })
    }

    /// The same gradients recovered from synthetic vertices (their `vz`, and
    /// `s`/`t` taken as absolute: `st_eye` is zero) — the unit tests' polygons,
    /// which have no plane. Solved on the vertex triple of LARGEST area, the
    /// best-conditioned choice. `None` when that triple is degenerate (below
    /// [`MIN_TRIPLE_AREA2`]) or a vertex is not finite or not in front of the eye.
    #[cfg(test)]
    pub(super) fn from_vertices(poly: &[AttrVert]) -> Option<PolyGrads> {
        let n = poly.len();
        if n < 3 {
            return None;
        }
        let area2 = |i: usize, j: usize, k: usize| -> f64 {
            let (a, b, c) = (&poly[i], &poly[j], &poly[k]);
            let (x1, y1) = (b.x as f64 - a.x as f64, b.y as f64 - a.y as f64);
            let (x2, y2) = (c.x as f64 - a.x as f64, c.y as f64 - a.y as f64);
            (x1 * y2 - x2 * y1).abs()
        };
        let mut best = (0, 1, 2);
        let mut best_a = area2(0, 1, 2);
        for i in 0..n {
            for j in i + 1..n {
                for k in j + 1..n {
                    let a = area2(i, j, k);
                    if a > best_a {
                        best_a = a;
                        best = (i, j, k);
                    }
                }
            }
        }
        // A NaN area (a non-finite vertex) is not finite.
        if !best_a.is_finite() || best_a < MIN_TRIPLE_AREA2 {
            return None;
        }
        let (a, b, c) = (&poly[best.0], &poly[best.1], &poly[best.2]);
        if a.vz <= 0.0 || b.vz <= 0.0 || c.vz <= 0.0 {
            return None;
        }
        let (ax, ay) = (a.x as f64, a.y as f64);
        let (x1, y1) = (b.x as f64 - ax, b.y as f64 - ay);
        let (x2, y2) = (c.x as f64 - ax, c.y as f64 - ay);
        let inv_det = 1.0 / (x1 * y2 - x2 * y1);
        // Per-vertex attributes: 1/z, s/z, t/z.
        let attr = |v: &AttrVert| -> [f64; 3] {
            let zi = 1.0 / v.vz as f64;
            [zi, v.s as f64 * zi, v.t as f64 * zi]
        };
        let (fa, fb, fc) = (attr(a), attr(b), attr(c));
        let lin = |k: usize| -> Linear {
            let (f1, f2) = (fb[k] - fa[k], fc[k] - fa[k]);
            let dx = (f1 * y2 - f2 * y1) * inv_det;
            let dy = (f2 * x1 - f1 * x2) * inv_det;
            Linear { o: fa[k] - dx * ax - dy * ay, dx, dy }
        };
        Some(PolyGrads { zi: lin(0), sz: lin(1), tz: lin(2), st_eye: [0.0, 0.0] })
    }
}

/// A surface's accumulators over one span in double, for the per-pixel
/// paths: `1/z`, `s/z` and `t/z` (eye-relative) at the span's first pixel and
/// their per-pixel steps — `D_DrawSpans8`'s `zi`/`sdivz`/`tdivz` and
/// `d_zistepu`/`d_sdivzstepu`/`d_tdivzstepu`, from id's gradients
/// ([`Span::from_grads`]) for the exact perspective ([`PerspSpan::Exact`]),
/// which takes `s`/`t` from it at every pixel: by its own accumulators, one
/// add a pixel ([`span_exact_reference`]), or the same texels from knots
/// every 16 pixels ([`Span::knot`], [`span_exact_cached`]); from the port's
/// analytic planes ([`span_at`]) for the walls with no surface block. id's
/// own span routines step in floats ([`FloatSpan`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    zi: f64,
    sz: f64,
    tz: f64,
    dzi: f64,
    dsz: f64,
    dtz: f64,
}

impl Span {
    /// The exact perspective's accumulators at pixel `(u, v)` from id's
    /// gradients `g`: each plane evaluated in double in the C's order
    /// (`origin + v*stepv + u*stepu`), and its step along the row.
    pub(super) fn from_grads(g: &SurfGrads, u: usize, v: usize) -> Span {
        let (du, dv) = (u as f64, v as f64);
        let at = |p: FloatPlane| f64::from(p.origin) + dv * f64::from(p.stepv) + du * f64::from(p.stepu);
        Span {
            zi: at(g.zi),
            sz: at(g.sdivz),
            tz: at(g.tdivz),
            dzi: g.zi.stepu.into(),
            dsz: g.sdivz.stepu.into(),
            dtz: g.tdivz.stepu.into(),
        }
    }

    /// The 16.16 texel coordinates at pixel `k` of the span, unclamped: `z =
    /// 0x10000 / zi`, `s = (sdivz * z) as i64 + sadjust`, the planes evaluated
    /// in f64 at the pixel. A zero `zi` (rounding at a near-clipped edge)
    /// makes `z` infinite and the cast saturates (a NaN is 0: the exact
    /// perspective's own rule, which the oracle's transcription of it
    /// follows); the add wraps (never a debug-build overflow panic), and the
    /// callers clamp.
    #[inline]
    fn st_at(&self, k: usize, sadjust: i64, tadjust: i64) -> (i64, i64) {
        let Knot { s, t, .. } = self.knot(k, sadjust, tadjust);
        (s, t)
    }

    /// [`Span::st_at`] and the `z` it divides by: a knot of
    /// [`span_exact_cached`]'s parabolas.
    #[inline]
    fn knot(&self, k: usize, sadjust: i64, tadjust: i64) -> Knot {
        let kf = k as f64;
        let z = 65536.0 / (self.zi + kf * self.dzi);
        Knot {
            s: (((self.sz + kf * self.dsz) * z) as i64).wrapping_add(sadjust),
            t: (((self.tz + kf * self.dtz) * z) as i64).wrapping_add(tadjust),
            z,
        }
    }
}

/// A pixel's 16.16 texel coordinates by the divide, and its `z`
/// ([`Span::knot`]).
#[derive(Clone, Copy, Debug)]
struct Knot {
    s: i64,
    t: i64,
    z: f64,
}

// ---------------------------------------------------------------------------
// D_CalcGradients and the span accumulators, in id's floats
// ---------------------------------------------------------------------------
//
// A wall or a liquid is drawn with the C's own arithmetic, operation for
// operation: a `float` where the C has a float, rounded to single precision
// at every step as id's C compiled with SSE rounds it (the oracle's
// `quake-oracle-sse`); a double where the C promotes one (`+ 0.5` is a
// double literal); `int` conversions that truncate, and the C's order of
// operations (Rust never fuses a multiply and an add). So `D_CalcGradients`'
// `sadjust` is the float sum it is in the C — the eye's 16.16 coordinate
// rounded to a float's 24 bits — and `D_DrawSpans16` steps its `s/z` in
// float adds: every texel coordinate is id's to the last bit, not near it.
// Only the port's exact perspective ([`PerspSpan::Exact`], not id's) goes on
// in double from these gradients ([`Span::from_grads`]).

/// `(int)x` of a C `float` as x86's `cvttss2si` gives it: truncation toward
/// zero, and `0x80000000` for a NaN or a value out of range, where Rust's
/// `as` saturates. Below the range `as` already gives `0x80000000`, so one
/// test does: a NaN fails it too. (Every span's segment end converts two:
/// the test is cheaper than a range's two.)
#[inline]
pub(super) fn c_ftoi(x: f32) -> i32 {
    if x < 2_147_483_648.0 { x as i32 } else { i32::MIN }
}

/// `(int)x` of a C `double` (`cvttsd2si`): as [`c_ftoi`].
#[inline]
pub(super) fn c_dtoi(x: f64) -> i32 {
    if x < 2_147_483_648.0 { x as i32 } else { i32::MIN }
}

/// A screen-space plane as `D_CalcGradients` and `R_RenderFace` leave it, in
/// id's floats: its value at pixel `(u, v)` (id's pixel centres are on the
/// integers) is `origin + v*stepv + u*stepu`, evaluated in that order as
/// `D_DrawSpans8` does.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct FloatPlane {
    pub(super) origin: f32,
    pub(super) stepu: f32,
    pub(super) stepv: f32,
}

impl FloatPlane {
    /// The value at `(du, dv)`: `origin + dv*stepv + du*stepu`.
    #[inline]
    fn at(self, du: f32, dv: f32) -> f32 {
        self.origin + dv * self.stepv + du * self.stepu
    }
}

/// What the span routines read of one surface at one mip level:
/// `R_RenderFace`'s `1/z` plane and what `D_CalcGradients` (d_edge.c) sets —
/// `d_sdivz*`/`d_tdivz*` (`(s - s_eye)/z` and `(t - t_eye)/z` in the level's
/// texels), `sadjust`/`tadjust` (the eye's 16.16 coordinates in the surface
/// block) and `bbextents`/`bbextentt` (the last 16.16 position inside it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SurfGrads {
    pub(super) zi: FloatPlane,
    pub(super) sdivz: FloatPlane,
    pub(super) tdivz: FloatPlane,
    pub(super) sadjust: i32,
    pub(super) tadjust: i32,
    pub(super) bbextents: i32,
    pub(super) bbextentt: i32,
}

/// The view `D_CalcGradients` projects a surface's texture axes with: the
/// frame's `vright`/`vup`/`vpn` (a brush entity's rotated by `R_RotateBmodel`),
/// `xscaleinv`/`yscaleinv` and `xcenter`/`ycenter` (`R_ViewChanged`).
#[derive(Clone, Copy, Debug)]
pub(super) struct GradView {
    pub(super) vright: Vec3,
    pub(super) vup: Vec3,
    pub(super) vpn: Vec3,
    pub(super) xscaleinv: f32,
    pub(super) yscaleinv: f32,
    pub(super) xcenter: f32,
    pub(super) ycenter: f32,
}

impl GradView {
    /// `TransformVector` (r_misc.c): `v` along `vright`, `vup`, `vpn`.
    #[inline]
    pub(super) fn transform(&self, v: Vec3) -> Vec3 {
        [dot(v, self.vright), dot(v, self.vup), dot(v, self.vpn)]
    }
}

/// The parts of a face `D_CalcGradients` reads besides the view: its texinfo
/// `vecs`, `texturemins` and `extents` (`Mod_LoadFaces`: a liquid's are
/// -8192 and 16384), and the mip level its block is at.
#[derive(Clone, Copy, Debug)]
pub(super) struct GradFace<'v> {
    pub(super) vecs: &'v [[f32; 4]; 2],
    pub(super) texturemins: [i32; 2],
    pub(super) extents: [i32; 2],
    pub(super) miplevel: u32,
}

/// `D_CalcGradients` (d_edge.c) for a face seen through `view` from the eye
/// at `transformed_modelorg` (`TransformVector (modelorg)`: the eye along the
/// frame's unrotated axes), with `R_RenderFace`'s `1/z` plane `zi`.
///
/// `sadjust = ((fixed16_t)(DotProduct (p_temp1, p_saxis) * 0x10000 + 0.5)) -
/// ((texturemins[0] << 16) >> miplevel) + vecs[0][3]*t`: the dot product a
/// float, `+ 0.5` a double, the cast a truncation, then the `int` difference
/// converted to a float and added to the float product — a float sum, so
/// the eye's coordinate keeps a float's 24 bits (1000 texels out, its step is
/// 8 units of 16.16), which the conversion to `fixed16_t` truncates.
pub(super) fn calc_gradients(view: &GradView, transformed_modelorg: Vec3, face: GradFace, zi: FloatPlane) -> SurfGrads {
    let GradFace { vecs, texturemins, extents, miplevel } = face;
    let mipscale = 1.0 / (1u32 << miplevel) as f32;
    let axis = |k: usize| view.transform([vecs[k][0], vecs[k][1], vecs[k][2]]);
    let (p_saxis, p_taxis) = (axis(0), axis(1));
    let t = view.xscaleinv * mipscale;
    let (sstepu, tstepu) = (p_saxis[0] * t, p_taxis[0] * t);
    let t = view.yscaleinv * mipscale;
    let (sstepv, tstepv) = (-p_saxis[1] * t, -p_taxis[1] * t);
    let origin = |a: Vec3, stepu: f32, stepv: f32| a[2] * mipscale - view.xcenter * stepu - view.ycenter * stepv;
    let p_temp1 = transformed_modelorg.map(|c| c * mipscale);
    let t = 65536.0 * mipscale;
    let adjust = |a: Vec3, k: usize| {
        let eye = c_dtoi(f64::from(dot(p_temp1, a) * 65536.0) + 0.5);
        let mins = (texturemins[k] << 16) >> miplevel;
        c_ftoi(eye.wrapping_sub(mins) as f32 + vecs[k][3] * t)
    };
    SurfGrads {
        zi,
        sdivz: FloatPlane { origin: origin(p_saxis, sstepu, sstepv), stepu: sstepu, stepv: sstepv },
        tdivz: FloatPlane { origin: origin(p_taxis, tstepu, tstepv), stepu: tstepu, stepv: tstepv },
        sadjust: adjust(p_saxis, 0),
        tadjust: adjust(p_taxis, 1),
        bbextents: ((extents[0] << 16) >> miplevel) - 1,
        bbextentt: ((extents[1] << 16) >> miplevel) - 1,
    }
}

#[cfg(test)]
impl SurfGrads {
    /// A synthetic polygon's gradients `g` as `D_CalcGradients` would leave
    /// them for a `bw x bh` block whose texel `(0, 0)` is `texmins`: the
    /// planes in floats with id's pixel centres (on the integers, the
    /// polygon's at `+ 0.5`), `sadjust` the eye's block coordinate rounded.
    pub(super) fn from_poly(g: &PolyGrads, texmins: [f32; 2], bw: usize, bh: usize) -> SurfGrads {
        let plane = |l: Linear| FloatPlane {
            origin: (l.o + 0.5 * l.dx + 0.5 * l.dy) as f32,
            stepu: l.dx as f32,
            stepv: l.dy as f32,
        };
        let adjust = |k: usize| ((g.st_eye[k] - f64::from(texmins[k])) * 65536.0 + 0.5).floor() as i32;
        SurfGrads {
            zi: plane(g.zi),
            sdivz: plane(g.sz),
            tdivz: plane(g.tz),
            sadjust: adjust(0),
            tadjust: adjust(1),
            bbextents: ((bw as i32) << 16) - 1,
            bbextentt: ((bh as i32) << 16) - 1,
        }
    }
}

/// The span routines' float accumulators, `sdivz`, `tdivz` and `zi`, from
/// the span's first pixel (`du = (float)pspan->u`, `dv = (float)pspan->v`)
/// on: stepped by float adds, each segment's end divided for as the C does.
#[derive(Clone, Copy, Debug)]
struct FloatSpan {
    sdivz: f32,
    tdivz: f32,
    zi: f32,
}

impl FloatSpan {
    #[inline]
    fn start(g: &SurfGrads, u: usize, v: usize) -> FloatSpan {
        let (du, dv) = (u as f32, v as f32);
        FloatSpan { sdivz: g.sdivz.at(du, dv), tdivz: g.tdivz.at(du, dv), zi: g.zi.at(du, dv) }
    }

    /// The accumulators `n` pixels on, by the C's `sdivz += d_sdivzstepu * n`
    /// (one float product, one float add; `n` a float, as the C's
    /// `sdivz16stepu`/`spancountminus1` are).
    #[inline]
    fn step(&mut self, g: &SurfGrads, n: f32) {
        self.sdivz += g.sdivz.stepu * n;
        self.tdivz += g.tdivz.stepu * n;
        self.zi += g.zi.stepu * n;
    }

    /// The 16.16 position, unclamped: `z = (float)0x10000 / zi`, `s =
    /// (int)(sdivz * z) + sadjust` (the `int` add wraps, as the C's does). A
    /// zero `zi` (rounding at a near-clipped edge) makes `z` infinite and the
    /// cast `0x80000000`; the callers clamp.
    #[inline]
    fn st(&self, g: &SurfGrads) -> (i32, i32) {
        let z = 65536.0 / self.zi;
        (c_ftoi(self.sdivz * z).wrapping_add(g.sadjust), c_ftoi(self.tdivz * z).wrapping_add(g.tadjust))
    }
}

/// How often a textured brush span (a wall from the surface cache, a liquid)
/// finds its texel exactly — the perspective divide — and steps affinely in
/// between: the console's `r_perspspan 64|32|16|8|4|1`
/// ([`RenderOptions::persp_span`](super::RenderOptions::persp_span)). The
/// affine error of a run grows with the square of its length, so 8's is a
/// quarter of 16's, 4's a sixteenth, 32's four times and 64's sixteen times;
/// where it crosses a texel's edge the texel is taken a pixel off, which at a
/// grazing angle shows as mortar lines stepped every segment, and in motion
/// as their kinks swimming. What the eye takes in is the error in texels, and
/// that goes with the square of the span's angle, not its pixels: id's 16 at
/// 320x200 spanned what about 36 pixels do on a wide 1315x535 frame and 72
/// at 1920x1080 (FRAMERATE.md, "The perspective span").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PerspSpan {
    /// Every 64 pixels: `D_DrawSpans8`'s arithmetic at 64 ([`span_c_cached`]),
    /// and `Turbulent8`'s. Not id: at 1080p about what id's 16 was at
    /// 320x200.
    Spans64,
    /// Every 32 pixels, the same arithmetic. Not id: on a wide 1315x535 frame
    /// about what id's 16 was at 320x200.
    Spans32,
    /// Every 16 pixels, what 1996 players saw: the x86 WinQuake's
    /// `D_DrawSpans16` (`d_draw16.s`, `d_subdiv16` 1) on the surface cache
    /// and `Turbulent8` on liquids. id's, and Classic's.
    #[default]
    Spans16,
    /// Every 8 pixels: id's own portable C, `D_DrawSpans8` (d_scan.c), which
    /// the x86 build replaced with the asm's 16 ([`span_c_cached`] says how
    /// the two differ beyond the count); on liquids `Turbulent8`'s
    /// arithmetic at 8. Not what id's players saw.
    Spans8,
    /// Every 4 pixels: `D_DrawSpans8`'s arithmetic at 4, and `Turbulent8`'s
    /// at 4. Not id.
    Spans4,
    /// At every pixel: not id; the 2026 profile's.
    Exact,
}

impl PerspSpan {
    /// The six, from the longest to exact: the order the settings page steps
    /// them (right is finer).
    pub const ALL: [PerspSpan; 6] = [
        PerspSpan::Spans64,
        PerspSpan::Spans32,
        PerspSpan::Spans16,
        PerspSpan::Spans8,
        PerspSpan::Spans4,
        PerspSpan::Exact,
    ];

    /// The pixels from one divide to the next: 64, 32, 16, 8, 4 or 1 (the
    /// cvar's value).
    #[must_use]
    pub fn pixels(self) -> u8 {
        match self {
            PerspSpan::Spans64 => 64,
            PerspSpan::Spans32 => 32,
            PerspSpan::Spans16 => 16,
            PerspSpan::Spans8 => 8,
            PerspSpan::Spans4 => 4,
            PerspSpan::Exact => 1,
        }
    }

    /// The span a cvar value names: the longest of 64, 32, 16, 8, 4 and 1
    /// not longer than `v` (12 is 8, 100 is 64, 2 is exact). Below 1 — 0, a
    /// word (`atof` 0), a negative or NaN — is id's 16: the port's cvars read
    /// 0 as the Classic value.
    #[must_use]
    pub fn from_pixels(v: f32) -> PerspSpan {
        if v.is_nan() || v < 1.0 {
            return PerspSpan::Spans16;
        }
        PerspSpan::ALL.into_iter().find(|p| v >= f32::from(p.pixels())).unwrap_or(PerspSpan::Exact)
    }
}

/// `reciprocal_table_16` (d_varsa.s): `1/n` for `n = 2..=15` in 1.31 fixed
/// point, `floor(2^31 / n)`. `D_DrawSpans16`'s last segment steps by
/// `(snext - s) * 2` times this, keeping the high word: `floor(ds * R / 2^31)`.
#[rustfmt::skip] // a table, eight to a line
const RECIPROCAL_16: [i64; 16] = [
    0, 0, 0x4000_0000, 0x2aaa_aaaa, 0x2000_0000, 0x1999_9999, 0x1555_5555, 0x1249_2492,
    0x1000_0000, 0x0e38_e38e, 0x0ccc_cccc, 0x0ba2_e8ba, 0x0aaa_aaaa, 0x09d8_9d89, 0x0924_9249,
    0x0888_8888,
];

/// A surface-cache block's fixed-point frame as the exact perspective
/// ([`span_exact_cached`]) reads it, in `i64`: [`SurfGrads`]' `sadjust`/
/// `tadjust` (the eye's block coordinate, 16.16) and `bbextents`/`bbextentt`
/// (`(extent << 16) - 1`: the last position inside the surface).
#[derive(Clone, Copy, Debug)]
pub(super) struct BlockFixed {
    sadjust: i64,
    tadjust: i64,
    bbextents: i64,
    bbextentt: i64,
}

impl BlockFixed {
    /// The frame `D_CalcGradients` left in `g`.
    pub(super) fn of(g: &SurfGrads) -> BlockFixed {
        BlockFixed {
            sadjust: g.sadjust.into(),
            tadjust: g.tadjust.into(),
            bbextents: g.bbextents.into(),
            bbextentt: g.bbextentt.into(),
        }
    }
}

/// `if (v > hi) v = hi; else if (v < lo) v = lo;`: the C's clamp of a
/// segment's end (`D_DrawSpans8`, `Turbulent8`; `D_DrawSpans16` tests the
/// low bound first, the same while `hi >= lo`, as every block's is).
#[inline]
fn clamp_c(v: i32, lo: i32, hi: i32) -> i32 {
    if v > hi {
        hi
    } else if v < lo {
        lo
    } else {
        v
    }
}

/// The end of each full `N`-pixel segment of a span of `end` pixels from
/// `acc` (the accumulators at the span's first pixel), clamped to `[low,
/// bbextents]`: what the span loops step through, a segment ahead.
///
/// The span loops ask for the end of the segment AFTER the one they are
/// about to draw. id's routines divide for a segment's end on reaching the
/// segment, and its pixels cannot start before the quotient is there; asked
/// for a segment early, the divide runs while the segment before is drawn.
/// The values are the same — the float adds and the divides are the C's, in
/// its order; only the pixels are drawn later — and the wall spans a tenth
/// to a fifth faster (PERF_PLAN.md, §15). Once the full segments are done,
/// [`SegmentEnds::last`] gives the last segment's end from where the
/// accumulators stopped, the last full segment's end.
struct SegmentEnds<'g, const N: usize> {
    g: &'g SurfGrads,
    acc: FloatSpan,
    /// `sdivz16stepu`, `tdivz16stepu`, `zi16stepu` (for `N` 16): `d_*stepu * N`.
    nstep: [f32; 3],
    low: i32,
    end: usize,
    /// The first pixel of the segment whose end [`SegmentEnds::next`] gives.
    k0: usize,
}

impl<'g, const N: usize> SegmentEnds<'g, N> {
    #[inline]
    fn new(g: &'g SurfGrads, acc: FloatSpan, end: usize, low: i32) -> Self {
        let nf = N as f32;
        let nstep = [g.sdivz.stepu * nf, g.tdivz.stepu * nf, g.zi.stepu * nf];
        SegmentEnds { g, acc, nstep, low, end, k0: 0 }
    }

    /// The position the accumulators give, clamped: `snext`, `tnext`.
    #[inline]
    fn clamped(&self) -> (i32, i32) {
        let (s, t) = self.acc.st(self.g);
        (clamp_c(s, self.low, self.g.bbextents), clamp_c(t, self.low, self.g.bbextentt))
    }

    /// The clamped end of the next full segment (`sdivz += sdivz16stepu`
    /// ...), or `None` when no full segment is left (a full segment has
    /// pixels after it: the C's `count > 16`, `count -= spancount; if
    /// (count)`).
    #[inline]
    fn next(&mut self) -> Option<(i32, i32)> {
        if self.k0 + N >= self.end {
            return None;
        }
        self.k0 += N;
        let FloatSpan { sdivz, tdivz, zi } = &mut self.acc;
        *sdivz += self.nstep[0];
        *tdivz += self.nstep[1];
        *zi += self.nstep[2];
        Some(self.clamped())
    }

    /// The last segment's: its `n` steps (`count - 1`) on from the last full
    /// segment's end, by `sdivz += d_sdivzstepu * n`, clamped; `None` for a
    /// segment of one pixel, which takes no step.
    #[inline]
    fn last(mut self) -> Option<(usize, (i32, i32))> {
        let n = self.end.checked_sub(self.k0 + 1).filter(|&n| n > 0)?;
        self.acc.step(self.g, n as f32);
        Some((n, self.clamped()))
    }
}

/// `D_DrawSpans16` (d_draw16.s) over one of id's spans of a surface-cache
/// block (`crow`, its pixels, from pixel `(u, v)`): the texel coordinates
/// are exact at the first pixel, then at the end of every full 16-pixel
/// segment, and in between stepped by `(snext - s) / 16` exactly (the asm
/// carries the step's 20 fractional bits: pixel `k` reads `(16*s +
/// k*(snext - s)) >> 20`); the last segment of `n + 1` pixels lands on the
/// span's last pixel, stepped by `reciprocal_table_16` (`floor((snext - s) *
/// R[n] / 2^31)`, 16.16), one pixel alone at the previous segment's end.
/// The first position is clamped to `[0, bbextents]`, every later one to
/// `[4096, bbextents]` (the asm's low clamp, 1/16 texel). Every position is
/// then inside the surface — a full segment's lie between its clamped ends,
/// and the last segment's floor-biased steps undershoot its end by at most
/// `n` < 4096 — so each texel is in the block without a per-pixel clamp. The
/// 16-pixel grid starts at the span's first pixel, so it restarts wherever
/// the surface comes out from behind a nearer one (`R_ScanEdges` cuts its
/// spans there). The float arithmetic is the C's ([`FloatSpan`]): the asm's
/// is x87 at single precision, which the oracle's C reproduces in floats.
fn span16_cached(crow: &mut [u8], u: usize, v: usize, g: &SurfGrads, block: &[u8], bw: usize) {
    let acc = FloatSpan::start(g, u, v);
    let (s0, t0) = acc.st(g);
    let (mut s, mut t) = (clamp_c(s0, 0, g.bbextents), clamp_c(t0, 0, g.bbextentt));
    let mut ends = SegmentEnds::<16>::new(g, acc, crow.len(), 4096);
    // The full segments: exact again at pixel k0 + 16, the positions `16*s +
    // i*ds` with 20 fractional bits. (A loop of a constant 16, which the
    // compiler unrolls.)
    let mut k0 = 0;
    let mut ahead = ends.next();
    while let Some((sn, tn)) = ahead {
        ahead = ends.next();
        let (mut sa, mut ta) = (i64::from(s) * 16, i64::from(t) * 16);
        let (ds, dt) = (i64::from(sn - s), i64::from(tn - t));
        let seg: &mut [u8; 16] = (&mut crow[k0..k0 + 16]).try_into().expect("16 pixels");
        for c in seg {
            *c = block.get((ta >> 20) as usize * bw + (sa >> 20) as usize).copied().unwrap_or(0);
            sa += ds;
            ta += dt;
        }
        (s, t) = (sn, tn);
        k0 += 16;
    }
    // The last segment: `n` steps land on the span's last pixel, the
    // positions `s + i*ds` with 16.
    let (ss, ts) = match ends.last() {
        Some((1, (sn, tn))) => (i64::from(sn - s), i64::from(tn - t)),
        Some((n, (sn, tn))) => (
            i64::from(((i64::from(sn - s) * RECIPROCAL_16[n]) >> 31) as i32),
            i64::from(((i64::from(tn - t) * RECIPROCAL_16[n]) >> 31) as i32),
        ),
        None => (0, 0),
    };
    let (mut ps, mut pt) = (i64::from(s), i64::from(t));
    for c in &mut crow[k0..] {
        *c = block.get((pt >> 16) as usize * bw + (ps >> 16) as usize).copied().unwrap_or(0);
        ps += ss;
        pt += ts;
    }
}

/// `D_DrawSpans8` (d_scan.c: id's portable C, which the x86 build replaced
/// with [`span16_cached`]) over one of id's spans of a surface-cache block,
/// `N` pixels a segment: id's 8, or the same arithmetic at 64, 32 or 4. The 16.16
/// texel coordinates are exact at the first pixel (clamped to `[0,
/// bbextents]`) and at the end of every full segment (clamped to `[N,
/// bbextents]`), stepped by `(snext - s) >> log2(N)` in between; the last
/// segment of `n` pixels (`n <= N`) lands on the span's last pixel, stepped
/// by the C's division `(snext - s) / (n - 1)`. A full segment is one with
/// pixels after it, as the C's `count -= spancount; if (count)`. The float
/// arithmetic is the C's ([`FloatSpan`]): `sdivz8stepu = d_sdivzstepu * 8`.
///
/// How it differs from `D_DrawSpans16` beyond the count, each a 1/65536
/// texel or so: a full segment's step is truncated to 16.16 (the shift
/// floors it), where the asm keeps 20 fractional bits, so its pixels run up
/// to `(N-1)/65536` texel below the straight line between its ends; the last
/// segment's step is the C's division, toward zero, where the asm multiplies
/// by `reciprocal_table_16`, toward minus infinity; and the segment ends'
/// low clamp is `N`/65536 texel (the C's 8) where the asm's is 4096 (1/16
/// texel). The clamp is the C's guard against a floored negative step
/// running a segment below the surface's edge, and holds at any `N`: pixel
/// `i` of a segment from `s >= 0` to `snext >= N` is at least `i/N` in
/// 16.16. Every position is inside the block, as in [`span16_cached`].
///
/// Why this form and not the asm's at 32 and 64: the C is id's own pattern
/// for any power of two (`>> 3` is the only 8 in it), where the asm is tied
/// to 16 by `reciprocal_table_16` (1/2 to 1/15, for the last segment) and its
/// 20-bit carry. Over 64 pixels the C's arithmetic still holds: the ends are
/// clamped into `[N, bbextents]` and the positions stay between them; the
/// floored step loses under one 16.16 unit a pixel, `63/65536` texel by a
/// full segment's last pixel; the guard keeps the lowest position at `i/64
/// >= 0`; a span's two divides (the last segment's) are the only ones not
/// by a power of two. (`render::raster`'s fuzz puts it against a literal
/// transcription of the C at every `N`.)
fn span_c_cached<const N: usize>(crow: &mut [u8], u: usize, v: usize, g: &SurfGrads, block: &[u8], bw: usize) {
    let (shift, low) = (N.trailing_zeros(), N as i32);
    let acc = FloatSpan::start(g, u, v);
    let (s0, t0) = acc.st(g);
    let (mut s, mut t) = (clamp_c(s0, 0, g.bbextents), clamp_c(t0, 0, g.bbextentt));
    let mut ends = SegmentEnds::<N>::new(g, acc, crow.len(), low);
    // The full segments, `N` pixels each: a loop of a constant length, which
    // the compiler unrolls (one of a variable length cost 8 nearly what
    // exact perspective costs). Then the last, `n` pixels.
    // (The pixels step in `i64`: the same values as the C's `int`s, which the
    // clamps keep between a segment's ends, and no sign extension a pixel.)
    let mut k0 = 0;
    let mut ahead = ends.next();
    while let Some((snext, tnext)) = ahead {
        ahead = ends.next();
        let (sstep, tstep) = (i64::from((snext - s) >> shift), i64::from((tnext - t) >> shift));
        let (mut ps, mut pt) = (i64::from(s), i64::from(t));
        let seg: &mut [u8; N] = (&mut crow[k0..k0 + N]).try_into().expect("N pixels");
        for c in seg {
            *c = block.get((pt >> 16) as usize * bw + (ps >> 16) as usize).copied().unwrap_or(0);
            ps += sstep;
            pt += tstep;
        }
        (s, t) = (snext, tnext);
        k0 += N;
    }
    let (sstep, tstep) = match ends.last() {
        Some((n, (snext, tnext))) => (i64::from((snext - s) / n as i32), i64::from((tnext - t) / n as i32)),
        None => (0, 0),
    };
    let (mut ps, mut pt) = (i64::from(s), i64::from(t));
    for c in &mut crow[k0..] {
        *c = block.get((pt >> 16) as usize * bw + (ps >> 16) as usize).copied().unwrap_or(0);
        ps += sstep;
        pt += tstep;
    }
}

/// The exact perspective's texel at every pixel of one span of a
/// surface-cache block: `D_DrawSpans8`'s 16.16 arithmetic with the divide at
/// every pixel, clamped into the block.
fn span_exact_reference(crow: &mut [u8], sp: &Span, fx: &BlockFixed, block: &[u8], bw: usize, bh: usize) {
    let (bw_i, bh_i) = (bw as i64, bh as i64);
    let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
    for c in crow.iter_mut() {
        // No z test: a non-positive `zi` (rounding at a clipped edge) saturates
        // and the clamp keeps the read in the block.
        let z = 65536.0 / zi;
        let s = ((sz * z) as i64).wrapping_add(fx.sadjust) >> 16;
        let t = ((tz * z) as i64).wrapping_add(fx.tadjust) >> 16;
        // Nearly every pixel is inside the block: one test for both
        // coordinates (as unsigned, a negative is past any width) before the
        // four of the two clamps.
        let (bx, by) = if (s as u64) < bw as u64 && (t as u64) < bh as u64 {
            (s as usize, t as usize)
        } else {
            (s.clamp(0, bw_i - 1) as usize, t.clamp(0, bh_i - 1) as usize)
        };
        *c = block[by * bw + bx];
        zi += sp.dzi;
        sz += sp.dsz;
        tz += sp.dtz;
    }
}

// Cheap exact: the exact perspective with a divide every 16 pixels, the
// same pixels as a divide at every pixel ([`span_exact_reference`]).
//
// Along a span a texel coordinate is `X(k) = 65536 (sz + k dsz) / (zi + k
// dzi)` (16.16, `sadjust` aside), a hyperbola in the pixel `k`. Through three
// of its points `H` = 16 pixels apart (the knots, by the divide) a parabola
// stays close to it. The parabola is stepped by forward differences in fixed
// point, s and t side by side in one `u64` ([`Lanes`]), and a pixel whose
// parabola position is further from both its texel's edges than the guard
// reads the divide's texel; the others are drawn by the divide
// ([`ExactReplay`]). A span or segment the guard cannot vouch for is drawn
// by the divide.
//
// Why the guard vouches for a pixel. One coordinate, in 16.16 units: `X` the
// true value, `R` the reference's before its `>> 16`, `K` a knot's, `y` the
// lane's (in 1/128 units, so `y/128`).
// (a) Rounding. The reference's accumulators after `k` adds, its divide and
//     its product put the value it truncates within `u reach (k (1 + hi/lo)
//     + 2)` of `X` (`u` = ε/2; [`exact_plan`]'s `reach` and `hi/lo`): at
//     most half its `noise`, which it keeps under a quarter. So
//     `|R - X| < 1.125`, the same for a knot (fewer roundings), and
//     `|K - R| <= 1`.
// (b) The knots' errors through the parabola: at most their 1.125 times
//     1.25, the most the three Lagrange weights of equally spaced knots sum
//     to (in absolute value) between the outer knots: under 1.41.
// (c) Interpolation: the parabola through the true `X` at the knots is
//     within `max|X'''| / 6 * max|t (t - H) (t - 2H)|` of `X`, which is
//     `es` ([`interpolation_bounds`]): `|X'''| = 6 |sz dzi - dsz zi| dzi^2
//     z^4 / 65536^3` is largest where `z` is, at an outer knot (`zi` is
//     linear and positive over the span, so `z` is monotone).
// (d) The forward differences, floored ([`Parabola::through`]): at `H` = 16
//     the first loses at most 3/4 of a lane unit and the second at most 1/2,
//     so by a segment's last pixel (31) `y` is below the exact parabola by
//     at most 31 * 3/4 + 465 * 1/2 = 255.75 lane units, under 2.0 units,
//     and never above it.
// So `|R - y/128| < es + 1.125 + 1.41 + 2.0 = es + 4.54`, under the guard
// `g = floor(es) + GUARD_SLACK` (9), and:
// (e) A pixel the guard test clears ([`Lanes::carries`]) is at least `g`
//     from both its texel's edges, so `R` lies in the same texel.
// (f) [`Parabola::through`] keeps the parabola `g` inside the block, and
//     `y` is at most 2 below it: the texel is the block's, as `R`'s is, and
//     the reference's clamp does not act. Each lane, lifted by its guard,
//     stays in `[0, 2^32)` (blocks of at most [`LANE_TEXELS`]), so the
//     packed sums give back both lanes whole: a negative s step borrows
//     from the t lane and the sum returns it.
// (g) The test adds `room`, a texel less twice the guard, to each lane's
//     fraction: `g` stays under half a texel ([`GUARD_MAX`]).
// (h) A pixel the test does not clear is drawn by the divide: the knot's
//     arithmetic, the reference's texel when the knot's fraction is at least
//     2 from both edges (`|K - R| <= 1`; one would do), else the
//     reference's own accumulators, replayed to it.

/// Pixels between two knots of [`span_exact_cached`]'s parabolas.
const EXACT_KNOT: usize = 16;
/// A parabola's pixels: two knot intervals.
const EXACT_SEG: usize = 2 * EXACT_KNOT;
/// The widest and tallest block a parabola's 32-bit lanes hold: 9 bits of
/// texel over [`LANE_FRAC`] of fraction.
const LANE_TEXELS: usize = 512;
/// A lane's fraction bits: 16.16 with 7 more for the forward differences.
const LANE_FRAC: u32 = 23;
/// Both lanes' fractions.
const LANE_FRAC_MASK: u64 = ((1 << LANE_FRAC) - 1) | (((1 << LANE_FRAC) - 1) << 32);
/// The carry of each lane's guard test: set when the pixel is clear of its
/// texel's edges.
const LANE_CLEAR: u64 = (1 << LANE_FRAC) | (1 << (LANE_FRAC + 32));
/// The widest interpolation bound (16.16 units) a segment is drawn by
/// parabola with: past it too many pixels would need the divide.
const GUARD_MAX: f64 = 6000.0;
/// What the guard adds to the interpolation bound, in 16.16 units: proof
/// (a), (b) and (d)'s 4.54, the bound's truncation to whole units (under
/// 1), and room. `cheap_exact_steered_to_its_guards_is_the_divide` fails
/// with a slack of 3.
const GUARD_SLACK: i64 = 9;
// (g): the guard test's `room` is not negative.
const _: () = assert!(GUARD_MAX as i64 + GUARD_SLACK <= 1 << 15, "a guard must stay under half a texel");

/// What [`span_exact_cached`] needs of a span to draw it by parabolas: the
/// scales of s's and t's interpolation bounds ([`interpolation_bounds`]).
/// `None` sends the span to the reference: 32 pixels or fewer, a block
/// wider or taller than [`LANE_TEXELS`] (or shorter than `bw * bh`), `1/z`
/// not positive and finite at both ends, or rounding noise over a quarter
/// unit (huge `s/z` or `t/z`, NaNs).
fn exact_plan(n: usize, sp: &Span, bw: usize, bh: usize, len: usize) -> Option<(f64, f64)> {
    if n <= EXACT_SEG || bw == 0 || bh == 0 || bw > LANE_TEXELS || bh > LANE_TEXELS || len < bw * bh {
        return None;
    }
    let last = (n - 1) as f64;
    let zi_end = sp.zi + last * sp.dzi;
    // (Compared by hand: `f64::min` and `max` are calls in a wasm build.)
    let (lo, hi) = if sp.zi < zi_end { (sp.zi, zi_end) } else { (zi_end, sp.zi) };
    if !(sp.zi > 0.0 && zi_end > 0.0 && hi.is_finite()) {
        return None;
    }
    // Proof (a): the reference's k-th `zi`, `sz` and `tz` each carry k
    // roundings of at most `u` times the largest value they pass (for `zi`,
    // `hi`; for the others `reach`'s numerator), its divide and product two
    // more; relative to `X` that is `u reach (k (1 + hi/lo) + 2)`, and
    // `noise` is at least twice it for every `k < n`.
    let larger = |a: f64, b: f64| if a > b { a } else { b };
    let ends = |a: f64, d: f64| larger(a.abs(), (a + last * d).abs());
    let reach = 65536.0 * larger(ends(sp.sz, sp.dsz), ends(sp.tz, sp.dtz)) / lo;
    let noise = f64::EPSILON * ((2 * n + 4) as f64 * (hi / lo) + 4.0) * reach;
    if noise.is_nan() || noise > 0.25 {
        return None;
    }
    // Proof (c): `|X'''| = 6 |W| dzi^2 z^4 / 65536^3`, `W = sz dzi - dsz zi`
    // constant along the span (with a little for its own rounding), and
    // `max|t (t - H) (t - 2H)| / 6 = 2 H^3 / (3 sqrt 3) / 6 = 0.06415 H^3`.
    let w =
        |a: f64, d: f64| (a * sp.dzi - d * sp.zi).abs() + 4.0 * f64::EPSILON * ((a * sp.dzi).abs() + (d * sp.zi).abs());
    let h = EXACT_KNOT as f64;
    let c = 6.0 * 0.0642 * h * h * h * sp.dzi * sp.dzi / (65536.0 * 65536.0 * 65536.0);
    Some((c * w(sp.sz, sp.dsz), c * w(sp.tz, sp.dtz)))
}

/// Proof (c)'s interpolation bound `es` for s and t over the segment
/// between the knots `a` and `b`: [`exact_plan`]'s scales times the
/// segment's largest `z^4`, which is at `a` or `b`.
#[inline]
fn interpolation_bounds(a: Knot, b: Knot, (s_scale, t_scale): (f64, f64)) -> (f64, f64) {
    let z = if a.z > b.z { a.z } else { b.z };
    let z4 = (z * z) * (z * z);
    (s_scale * z4, t_scale * z4)
}

/// One coordinate's parabola over a segment, in lane fixed point
/// ([`LANE_FRAC`]): its first pixel's position and its first and second
/// forward differences; and the guard (16.16 units), how near a texel's
/// edge a pixel of it may come and still be vouched for.
#[derive(Clone, Copy, Debug)]
struct Parabola {
    start: i64,
    d1: i64,
    d2: i64,
    guard: i64,
}

impl Parabola {
    /// The parabola through the knot values `x0`, `xh`, `x2h` (16.16, `H`
    /// apart) whose interpolation bound is `bound`, when the bound is at most
    /// [`GUARD_MAX`] and every pixel of it is at least the guard inside
    /// `0..=max` (proof (f)).
    #[inline]
    fn through(x0: i64, xh: i64, x2h: i64, bound: f64, max: i64) -> Option<Parabola> {
        if bound.is_nan() || bound > GUARD_MAX {
            return None;
        }
        let guard = bound as i64 + GUARD_SLACK;
        // The parabola is within `|dd| / 2` of the chord from `x0` to `x2h`.
        let dd = x2h - 2 * xh + x0;
        let bulge = (dd.abs() + 1) / 2;
        if x0.min(x2h) - bulge < guard || x0.max(x2h) + bulge > max - guard {
            return None;
        }
        // p(k) = x0 + k A + k^2 B, B = dd / (2 H^2), A = (4 xh - 3 x0 - x2h) / (2 H);
        // d1 = A + B and d2 = 2 B, times 128 and floored (proof (d): d1 in
        // quarters of a lane unit, d2 in halves, at H = 16).
        let h = EXACT_KNOT as i64;
        Some(Parabola {
            start: x0 << 7,
            d1: (64 * h * (4 * xh - 3 * x0 - x2h) + 64 * dd).div_euclid(h * h),
            d2: (128 * dd).div_euclid(h * h),
            guard,
        })
    }
}

/// The two parabolas of the segment through the knots `a`, `m`, `b`, for
/// s and t, or `None` when it is drawn by the divide.
#[inline]
fn segment_parabolas(
    a: Knot,
    m: Knot,
    b: Knot,
    scale: (f64, f64),
    (smax, tmax): (i64, i64),
) -> Option<(Parabola, Parabola)> {
    let (es, et) = interpolation_bounds(a, b, scale);
    Some((Parabola::through(a.s, m.s, b.s, es, smax)?, Parabola::through(a.t, m.t, b.t, et, tmax)?))
}

/// `s` in the low 32 bits of a `u64` and `t` in the high: two lanes.
#[inline]
fn side_by_side(s: i64, t: i64) -> u64 {
    ((t as u64) << 32).wrapping_add(s as u64)
}

/// A segment's s and t parabolas side by side ([`side_by_side`]), stepped
/// together one add a difference, and tested together one AND a pixel.
#[derive(Clone, Copy, Debug)]
struct Lanes {
    /// The first pixel's positions, each lifted by its guard: a clear
    /// pixel's lifted position is still in its texel (proof (e)).
    start: u64,
    d1: u64,
    d2: u64,
    /// A texel less twice its guard, per lane (proof (g)).
    room: u64,
}

impl Lanes {
    #[inline]
    fn new(s: Parabola, t: Parabola) -> Lanes {
        Lanes {
            start: side_by_side(s.start + (s.guard << 7), t.start + (t.guard << 7)),
            d1: side_by_side(s.d1, t.d1),
            d2: side_by_side(s.d2, t.d2),
            room: side_by_side((1 << LANE_FRAC) - (s.guard << 8), (1 << LANE_FRAC) - (t.guard << 8)),
        }
    }

    /// The guard test of the pixel at `q`: its lane's bit of [`LANE_CLEAR`]
    /// is set when the lane's fraction (lifted by the guard) is at least
    /// twice the guard, i.e. the pixel is at least the guard from both its
    /// texel's edges. (Fraction plus `room` stays under 2^24: the lanes do
    /// not mix.)
    #[inline]
    fn carries(self, q: u64) -> u64 {
        (q & LANE_FRAC_MASK).wrapping_add(self.room)
    }

    /// The texel at the lane position `q`.
    #[inline]
    fn texel(q: u64, block: &[u8], bw: usize) -> u8 {
        block.get((q >> (LANE_FRAC + 32)) as usize * bw + ((q as u32) >> LANE_FRAC) as usize).copied().unwrap_or(0)
    }

    /// The segment's pixels by the parabolas, into `seg`; whether every
    /// one is clear of its texel's edges.
    #[inline]
    fn draw(self, seg: &mut [u8; EXACT_SEG], block: &[u8], bw: usize) -> bool {
        let (mut q, mut d1, mut clear) = (self.start, self.d1, LANE_CLEAR);
        for c in seg {
            clear &= self.carries(q);
            *c = Lanes::texel(q, block, bw);
            q = q.wrapping_add(d1);
            d1 = d1.wrapping_add(self.d2);
        }
        clear & LANE_CLEAR == LANE_CLEAR
    }

    /// The second pass over the segment from `k0` (of the span's `crow`):
    /// the pixels from `from` on the guard test did not clear, by the
    /// divide.
    #[inline]
    fn redraw_near_edges(self, crow: &mut [u8], k0: usize, from: usize, replay: &mut ExactReplay) {
        let (mut q, mut d1) = (self.start, self.d1);
        for (k, c) in (k0..).zip(&mut crow[k0..k0 + EXACT_SEG]) {
            if k >= from && self.carries(q) & LANE_CLEAR != LANE_CLEAR {
                *c = replay.texel(k);
            }
            q = q.wrapping_add(d1);
            d1 = d1.wrapping_add(self.d2);
        }
    }
}

/// The pixels [`span_exact_cached`]'s parabolas do not vouch for, by the
/// divide (proof (h)): the knot's arithmetic, or where that comes within 2
/// units of a texel's edge, the reference's own accumulators, stepped to
/// the pixel. The pixels come in order, so the accumulators only step on.
struct ExactReplay<'a> {
    sp: &'a Span,
    fx: &'a BlockFixed,
    block: &'a [u8],
    bw: usize,
    bh: usize,
    /// The pixel the accumulators are at, and their values there.
    k: usize,
    zi: f64,
    sz: f64,
    tz: f64,
}

impl<'a> ExactReplay<'a> {
    fn new(sp: &'a Span, fx: &'a BlockFixed, block: &'a [u8], bw: usize, bh: usize) -> Self {
        ExactReplay { sp, fx, block, bw, bh, k: 0, zi: sp.zi, sz: sp.sz, tz: sp.tz }
    }

    /// The exact texel of pixel `k`, at or after the last one asked for.
    #[inline]
    fn texel(&mut self, k: usize) -> u8 {
        let (sp, fx) = (self.sp, self.fx);
        let (mut s, mut t) = sp.st_at(k, fx.sadjust, fx.tadjust);
        let near = |v: i64| !(2..=0xFFFD).contains(&(v & 0xFFFF));
        if near(s) || near(t) {
            debug_assert!(k >= self.k, "the replay steps forward only");
            while self.k < k {
                self.zi += sp.dzi;
                self.sz += sp.dsz;
                self.tz += sp.dtz;
                self.k += 1;
            }
            let z = 65536.0 / self.zi;
            s = ((self.sz * z) as i64).wrapping_add(fx.sadjust);
            t = ((self.tz * z) as i64).wrapping_add(fx.tadjust);
        }
        let bx = (s >> 16).clamp(0, self.bw as i64 - 1) as usize;
        let by = (t >> 16).clamp(0, self.bh as i64 - 1) as usize;
        self.block[by * self.bw + bx]
    }
}

/// The exact perspective's texel at every pixel of one span of a
/// surface-cache block — what [`span_exact_reference`] draws with a divide
/// at every pixel — from a divide every sixteen.
fn span_exact_cached(crow: &mut [u8], sp: &Span, fx: &BlockFixed, block: &[u8], bw: usize, bh: usize) {
    let n = crow.len();
    let Some(scale) = exact_plan(n, sp, bw, bh, block.len()) else {
        return span_exact_reference(crow, sp, fx, block, bw, bh);
    };
    let max = (((bw as i64) << 16) - 1, ((bh as i64) << 16) - 1);
    let knot = |k: usize| sp.knot(k, fx.sadjust, fx.tadjust);
    let mut replay = ExactReplay::new(sp, fx, block, bw, bh);
    // The segments: 32 pixels from every 32nd, and a last one ending a pixel
    // short of the span's end (over pixels already drawn, if it must). Each
    // segment's two further knots are asked for a segment ahead, so their
    // divides run while the one before is drawn.
    let last = n - 1 - EXACT_SEG;
    let (mut k0, mut done) = (0, 0);
    let mut a = knot(0);
    let mut ahead = (knot(EXACT_KNOT), knot(EXACT_SEG));
    loop {
        let (m, b) = ahead;
        let next = (k0 < last).then(|| (k0 + EXACT_SEG).min(last));
        let next_a = match next {
            Some(k) if k != k0 + EXACT_SEG => knot(k),
            _ => b,
        };
        if let Some(k) = next {
            ahead = (knot(k + EXACT_KNOT), knot(k + EXACT_SEG));
        }
        match segment_parabolas(a, m, b, scale, max) {
            Some((s, t)) => draw_segment(crow, k0, done, Lanes::new(s, t), block, bw, &mut replay),
            None => {
                let from = done.max(k0);
                for (k, c) in (from..).zip(&mut crow[from..k0 + EXACT_SEG]) {
                    *c = replay.texel(k);
                }
            }
        }
        done = k0 + EXACT_SEG;
        let Some(k) = next else { break };
        (a, k0) = (next_a, k);
    }
    // The span's last pixel.
    crow[n - 1] = replay.texel(n - 1);
}

/// The segment of [`EXACT_SEG`] pixels from `k0` by its parabolas, the
/// pixels near a texel's edge again by the divide. Where the last segment
/// overlaps the one before (`k0 < done`) it is drawn aside and only its new
/// pixels kept: the ones before are done, and the replay never goes back.
#[inline]
fn draw_segment(
    crow: &mut [u8],
    k0: usize,
    done: usize,
    lanes: Lanes,
    block: &[u8],
    bw: usize,
    replay: &mut ExactReplay,
) {
    let mut aside = [0u8; EXACT_SEG];
    let seg: &mut [u8; EXACT_SEG] =
        if k0 < done { &mut aside } else { (&mut crow[k0..k0 + EXACT_SEG]).try_into().expect("a segment") };
    let all_clear = lanes.draw(seg, block, bw);
    if k0 < done {
        crow[done..k0 + EXACT_SEG].copy_from_slice(&aside[done - k0..]);
    }
    if !all_clear {
        lanes.redraw_near_edges(crow, k0, done, replay);
    }
}

/// Below this |2 x area| (in square pixels) even the best vertex triple of a
/// polygon is degenerate: an edge-on sliver that covers (next to) no pixel
/// centre, whose gradients would be noise ([`PolyGrads::from_vertices`]).
#[cfg(test)]
const MIN_TRIPLE_AREA2: f64 = 1e-6;

/// Wrap `v` into `0..n`, the way `D_DrawTurbulent8Span`'s `&63` does for id's
/// 64-texel liquids: for a power-of-two `n`, two's-complement `v & (n-1)` is
/// `v.rem_euclid(n)` for every `i32`, negative included, so a liquid texture's
/// (always power-of-two, 64x64 in id's data) wrap costs a mask, not a
/// division. Any other `n` (never id's data) falls back to the division.
#[inline]
fn wrap_texel(v: i32, n: usize) -> usize {
    if n.is_power_of_two() { (v & (n as i32 - 1)) as usize } else { v.rem_euclid(n as i32) as usize }
}

/// A liquid's texture, as `D_DrawTurbulent8Span` reads it: `tw x th` texels
/// (64x64 in id's data), at the table phase of the frame ([`turb_phase`]).
#[derive(Clone, Copy)]
struct Liquid<'a> {
    pixels: &'a [u8],
    tw: usize,
    th: usize,
    turb: &'a TurbTable,
    phase: usize,
}

impl Liquid<'_> {
    /// `D_DrawTurbulent8Span` over `seg`: the 16.16 coordinates from `(s, t)`
    /// (a segment's start, masked to `(CYCLE << 16) - 1`) stepped by `(ss,
    /// ts)` in the C's `int`s, each pixel warped by the table.
    #[inline]
    fn draw(&self, seg: &mut [u8], (mut s, mut t): (i32, i32), (ss, ts): (i32, i32)) {
        for c in seg {
            let (sturb, tturb) = self.turb.texel(self.phase, s, t);
            let texel = self.pixels.get(wrap_texel(tturb, self.th) * self.tw + wrap_texel(sturb, self.tw));
            *c = texel.copied().unwrap_or(0);
            s = s.wrapping_add(ss);
            t = t.wrapping_add(ts);
        }
    }
}

/// `Turbulent8` (d_scan.c; C in the x86 build too) over one of id's spans of a
/// liquid (`crow`, its pixels, from pixel `(u, v)`), `N` pixels a segment:
/// id's 16, or its arithmetic at 64, 32, 8 or 4 (`r_perspspan`; not id). The
/// 16.16 coordinates exact at the span's first pixel (clamped to `[0,
/// bbextents]`) and at each `N`-pixel segment's end (clamped to `[N,
/// bbextents]`, id's 16 — the guard [`span_c_cached`] keeps at its `N` too),
/// stepped by `(snext - s) >> log2(N)` in between; the last segment ends on
/// the span's last pixel, stepped by the C division `(snext - s) /
/// (spancount - 1)`. Each segment's start is masked to `(CYCLE << 16) - 1`
/// and `D_DrawTurbulent8Span` warps every pixel ([`TurbTable::texel`]). The
/// face's frame is `Mod_LoadFaces`' for turbulent surfaces (`texturemins`
/// -8192, `extents` 16384), in `g`. The raw texel, no colormap.
fn turb_span<const N: usize>(crow: &mut [u8], u: usize, v: usize, g: &SurfGrads, liquid: &Liquid) {
    let (shift, low) = (N.trailing_zeros(), N as i32);
    let acc = FloatSpan::start(g, u, v);
    let (s0, t0) = acc.st(g);
    let (mut s, mut t) = (clamp_c(s0, 0, g.bbextents), clamp_c(t0, 0, g.bbextentt));
    let mut ends = SegmentEnds::<N>::new(g, acc, crow.len(), low);
    let start = |s: i32, t: i32| (s & TURB_COORD_MASK, t & TURB_COORD_MASK);
    // The full segments, a loop of a constant `N` each ([`span_c_cached`]),
    // then the last; each segment in the C's `int`s from its masked start.
    let mut k0 = 0;
    let mut ahead = ends.next();
    while let Some((sn, tn)) = ahead {
        ahead = ends.next();
        let seg: &mut [u8; N] = (&mut crow[k0..k0 + N]).try_into().expect("N pixels");
        liquid.draw(seg, start(s, t), ((sn - s) >> shift, (tn - t) >> shift));
        (s, t) = (sn, tn);
        k0 += N;
    }
    let steps = match ends.last() {
        Some((n, (sn, tn))) => ((sn - s) / n as i32, (tn - t) / n as i32),
        None => (0, 0),
    };
    liquid.draw(&mut crow[k0..], start(s, t), steps);
}

/// The exact perspective's liquid (not id's): `Turbulent8`'s 16.16
/// coordinates divided for at every pixel, in double from id's gradients as
/// the exact walls are ([`span_exact_reference`]), each pixel clamped to the
/// liquid's extents and masked as a segment's start is, then warped.
fn turb_exact(crow: &mut [u8], sp: &Span, fx: &BlockFixed, liquid: &Liquid) {
    let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
    for c in crow.iter_mut() {
        let z = 65536.0 / zi;
        let s = ((sz * z) as i64).wrapping_add(fx.sadjust).clamp(0, fx.bbextents) as i32;
        let t = ((tz * z) as i64).wrapping_add(fx.tadjust).clamp(0, fx.bbextentt) as i32;
        liquid.draw(std::slice::from_mut(c), (s & TURB_COORD_MASK, t & TURB_COORD_MASK), (0, 0));
        zi += sp.dzi;
        sz += sp.dsz;
        tz += sp.dtz;
    }
}

// ---------------------------------------------------------------------------
// The span routines of `D_DrawSurfaces` (d_edge.c)
// ---------------------------------------------------------------------------
//
// `edge.rs` hands each surface its spans — the runs of a scanline where it is
// the nearest surface, cut by `R_ScanEdges` — and every pixel of the view is in
// exactly one of them, so these draw with no z test. A span is `count` pixels
// from `(u, v)`; `crow` is the image row from `u`; the surface's gradients are
// evaluated at the span's first pixel centre.

/// The accumulators of a span starting at pixel `(u, v)` from `grads` (pixel
/// `u`'s centre is `u + 0.5` in the gradients' screen coordinates, id's `u`):
/// `D_DrawSpans8`'s `du = (float)pspan->u`, `dv = (float)pspan->v`.
pub(super) fn span_at(grads: &PolyGrads, u: usize, v: usize) -> Span {
    let (cx, cy) = (u as f64 + 0.5, v as f64 + 0.5);
    Span {
        zi: grads.zi.at(cx, cy),
        sz: grads.sz.at(cx, cy),
        tz: grads.tz.at(cx, cy),
        dzi: grads.zi.dx,
        dsz: grads.sz.dx,
        dtz: grads.tz.dx,
    }
}

/// `(*d_drawspans)` on a surface-cache block, over one span from pixel `(u,
/// v)`: `D_DrawSpans16` (id's x86, the default), `D_DrawSpans8` at 64, 32,
/// 8 or 4, or the texel of the exact perspective at every pixel
/// ([`PerspSpan`]), from the surface's gradients `g`.
#[allow(clippy::too_many_arguments)]
pub(super) fn span_cached(
    crow: &mut [u8],
    u: usize,
    v: usize,
    g: &SurfGrads,
    block: &[u8],
    bw: usize,
    bh: usize,
    persp: PerspSpan,
) {
    match persp {
        PerspSpan::Spans64 => span_c_cached::<64>(crow, u, v, g, block, bw),
        PerspSpan::Spans32 => span_c_cached::<32>(crow, u, v, g, block, bw),
        PerspSpan::Spans16 => span16_cached(crow, u, v, g, block, bw),
        PerspSpan::Spans8 => span_c_cached::<8>(crow, u, v, g, block, bw),
        PerspSpan::Spans4 => span_c_cached::<4>(crow, u, v, g, block, bw),
        PerspSpan::Exact => span_exact_cached(crow, &Span::from_grads(g, u, v), &BlockFixed::of(g), block, bw, bh),
    }
}

/// `Turbulent8` ([`turb_span`]) at 16 (id's), 64, 32, 8 or 4, or the warp at the
/// exact perspective texel of every pixel ([`turb_exact`]), over one span of
/// a liquid surface from pixel `(u, v)`, at `cl.time` `time`.
#[allow(clippy::too_many_arguments)]
pub(super) fn span_turb(
    crow: &mut [u8],
    u: usize,
    v: usize,
    g: &SurfGrads,
    pixels: &[u8],
    tw: usize,
    th: usize,
    turb: &TurbTable,
    time: f64,
    persp: PerspSpan,
) {
    if tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    let liquid = Liquid { pixels, tw, th, turb, phase: turb_phase(time) };
    match persp {
        PerspSpan::Spans64 => turb_span::<64>(crow, u, v, g, &liquid),
        PerspSpan::Spans32 => turb_span::<32>(crow, u, v, g, &liquid),
        PerspSpan::Spans16 => turb_span::<16>(crow, u, v, g, &liquid),
        PerspSpan::Spans8 => turb_span::<8>(crow, u, v, g, &liquid),
        PerspSpan::Spans4 => turb_span::<4>(crow, u, v, g, &liquid),
        PerspSpan::Exact => turb_exact(crow, &Span::from_grads(g, u, v), &BlockFixed::of(g), &liquid),
    }
}

/// A wall with no surface-cache block (no colormap, no lightmap, or over the
/// block size cap — never in id's maps), per pixel: the texel at the exact
/// perspective, lit by the lightmap's factor (or `shade`) through the colormap
/// row, or without a colormap the palette entry nearest the linear
/// `palette[texel] * brightness` (synthetic scenes only).
#[allow(clippy::too_many_arguments)]
pub(super) fn span_tex(
    crow: &mut [u8],
    sp: &Span,
    grads: &PolyGrads,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &Palette,
    shade: f32,
    lightmap: Option<&LightMap>,
    colormap: Option<&[u8]>,
) {
    if tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    let colormap = colormap.filter(|cm| cm.len() >= COLORMAP_LEN);
    let st_eye = grads.st_eye;
    let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
    for c in crow.iter_mut() {
        let z = 1.0 / zi;
        let s = (sz * z + st_eye[0]) as f32;
        let t = (tz * z + st_eye[1]) as f32;
        let tx = (s as i64).rem_euclid(tw as i64) as usize;
        let ty = (t as i64).rem_euclid(th as i64) as usize;
        let texel = pixels.get(ty * tw + tx).copied().unwrap_or(0);
        let brightness = match lightmap {
            Some(lm) => lm.factor_at(s, t),
            None => shade,
        };
        *c = match colormap {
            Some(cm) => cm[colormap_row(brightness) * 256 + usize::from(texel)],
            None => shade_index(palette, texel, brightness),
        };
        zi += sp.dzi;
        sz += sp.dsz;
        tz += sp.dtz;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::light::COLORMAP_ROWS;

    /// Draw `rows` of a `w`-wide image through `f`, one span per row from
    /// column `x0`, with the gradients `g` (the rows D_DrawSurfaces would get
    /// for a surface covering them).
    fn spans(w: usize, h: usize, x0: usize, g: &PolyGrads, mut f: impl FnMut(&mut [u8], &Span)) -> Image {
        let mut img = Image::new(w, h, 0);
        for y in 0..h {
            let sp = span_at(g, x0, y);
            f(&mut img.pixels[y * w + x0..(y + 1) * w], &sp);
        }
        img
    }

    /// The same rows drawn by id's span routines, each handed its row's
    /// pixels from column `x0` and the span's first pixel `(x0, y)`.
    fn id_spans(w: usize, h: usize, x0: usize, mut f: impl FnMut(&mut [u8], usize, usize)) -> Image {
        let mut img = Image::new(w, h, 0);
        for y in 0..h {
            f(&mut img.pixels[y * w + x0..(y + 1) * w], x0, y);
        }
        img
    }

    /// A wall pixel with no surface block must route through
    /// `palette[colormap[row*256 + texel]]` (an INDEX lookup) when a colormap is
    /// supplied, and fall back to the linear `palette[texel]*brightness` multiply
    /// when it is `None`.
    #[test]
    fn colormap_routes_normal_pixel_through_index_lookup() {
        // Palette: index i -> grey (i,i,i), so a palette index is recoverable
        // from the written pixel's red channel.
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }
        // A 1x1 texture whose only texel is index 200.
        const TEXEL: u8 = 200;
        let pixels = [TEXEL];
        // Synthetic colormap (64 rows x 256): colormap[row*256 + col] = (col +
        // row) mod 256, so row R and col=TEXEL give palette index (TEXEL + R).
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256usize {
                cm[row * 256 + col] = ((col + row) % 256) as u8;
            }
        }
        // A surface at constant (s,t)=(0,0) over the whole 8x8 view. No
        // LightMap -> brightness == `shade`.
        let (w, h) = (8usize, 8usize);
        let shade = 1.0f32; // -> colormap_row(1.0) == 31
        let expected_row = colormap_row(shade);
        let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        let g = PolyGrads::from_vertices(&[v0, v1, v2]).expect("triangle");
        let render = |colormap: Option<&[u8]>| {
            spans(w, h, 0, &g, |row, sp| span_tex(row, sp, &g, &pixels, 1, 1, &pal, shade, None, colormap))
        };
        // With the colormap: pixel = palette[colormap[row*256 + 200]].
        let want_index = ((TEXEL as usize + expected_row) % 256) as u8;
        assert!(render(Some(&cm)).pixels.iter().all(|&p| p == want_index), "colormap path: palette index {want_index}");
        // Without the colormap: the linear multiply's nearest entry, palette[200].
        assert!(render(None).pixels.iter().all(|&p| p == TEXEL), "None path is palette[texel]*brightness");
        assert_ne!(want_index, TEXEL, "test ramp should remap the index at row 31");
    }

    /// Liquids write the RAW texel (`D_DrawTurbulent8Span`: no colormap at all)
    /// — neither row 0 (the old overbright) nor any other row.
    #[test]
    fn turb_writes_the_raw_texel() {
        const TEXEL: u8 = 77;
        // 64x64 so the Turb warp's index wrap is well-defined; fill with TEXEL.
        let pixels = vec![TEXEL; 64 * 64];
        let turb = TurbTable::new();
        let (w, h) = (40usize, 16usize);
        let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        let g = PolyGrads::from_vertices(&[v0, v1, v2]).expect("triangle");
        let g = SurfGrads::from_poly(&g, [-8192.0; 2], 16384, 16384);
        for persp in PerspSpan::ALL {
            let img = id_spans(w, h, 0, |row, u, v| span_turb(row, u, v, &g, &pixels, 64, 64, &turb, 0.0, persp));
            assert!(img.pixels.iter().all(|&p| p == TEXEL), "turb stores the raw texel ({persp:?})");
        }
    }

    // -- D_DrawSpans16 over the surface cache --------------------------------

    /// A 40x8 surface over a 256x256 block whose texel at (bx, by) is
    /// `(bx + 3*by) & 255`, drawn cached with `persp` in spans that start at
    /// column `start` (a nearer surface over the columns before it); `zl`/`zr`
    /// are the depth at the left and right edges (the texel s runs 20.3 ->
    /// 180.3 across, t 30.3 -> 40.3 down, perspective-correct, never on a texel
    /// boundary: the last segment's reciprocal steps run up to n/65536 texel
    /// low). Returns palette indices (0 where nothing was drawn).
    fn cached_quad(persp: PerspSpan, zl: f32, zr: f32, start: usize) -> Vec<u8> {
        let (w, h) = (40usize, 8usize);
        let (bw, bh) = (256usize, 256usize);
        let block: Vec<u8> = (0..bw * bh).map(|i| ((i % bw) + 3 * (i / bw)) as u8).collect();
        let v = |x: f32, y: f32, vz: f32, s: f32, t: f32| AttrVert { x, y, vz, s, t };
        let quad = [
            v(0.0, 0.0, zl, 20.3, 30.3),
            v(w as f32, 0.0, zr, 180.3, 30.3),
            v(w as f32, h as f32, zr, 180.3, 40.3),
            v(0.0, h as f32, zl, 20.3, 40.3),
        ];
        let g = PolyGrads::from_vertices(&quad).expect("quad");
        let g = SurfGrads::from_poly(&g, [0.0, 0.0], bw, bh);
        let img = id_spans(w, h, start, |row, u, v| span_cached(row, u, v, &g, &block, bw, bh, persp));
        img.pixels
    }

    #[test]
    fn spans16_is_exact_where_one_over_z_is_constant() {
        // A surface parallel to the screen: s/z and 1/z are both linear, so the
        // affine segments land on the exact texels everywhere — at every span.
        for persp in PerspSpan::ALL {
            assert_eq!(cached_quad(persp, 2.0, 2.0, 0), cached_quad(PerspSpan::Exact, 2.0, 2.0, 0), "{persp:?}");
        }
    }

    /// `D_DrawSpans8` at 8 and at 4 divides at the span's first pixel and
    /// every `N` pixels after it, the last segment landing on the span's last
    /// pixel: exact there, affine between; the grid restarts with the span.
    #[test]
    fn spans8_and_4_are_exact_at_each_segment_start_of_a_span() {
        let w = 40usize;
        let exact = cached_quad(PerspSpan::Exact, 1.0, 8.0, 0);
        for (persp, n) in [(PerspSpan::Spans8, 8usize), (PerspSpan::Spans4, 4)] {
            for start in [0usize, 5] {
                let img = cached_quad(persp, 1.0, 8.0, start);
                let mut at: Vec<usize> = (start..w - 1).step_by(n).collect();
                at.push(w - 1);
                for y in 0..8 {
                    for &x in &at {
                        assert_eq!(img[y * w + x], exact[y * w + x], "{persp:?} from {start}: row {y} pixel {x}");
                    }
                }
                let differ = img.iter().zip(&exact).filter(|(a, b)| a != b).count();
                assert!(differ > 0 && differ < 8 * (w - start), "{persp:?}: {differ} pixels differ: affine between");
            }
        }
    }

    #[test]
    fn spans8_steps_like_d_draw_spans8() {
        // A full segment's step is `(snext - s) >> 3`: from s = 0xFFF0 towards
        // snext = s + 15, each pixel steps 1 (15/8 floored) and pixel 7 reads
        // 0xFFF7, texel 0, where D_DrawSpans16's 20-bit steps would reach
        // 0xFFF0 + 7*15/8 = 0xFFFD.2; and toward s - 15, -2 (floored) a pixel.
        let (s, ds) = (0xFFF0i64, 15i64);
        assert_eq!(((s + 7 * (ds >> 3)) >> 16, s + 7 * (ds >> 3), s + 7 * (-ds >> 3)), (0, 0xFFF7, 0xFFF0 - 14));
        // The last segment's step is the C's division, toward zero: over 3
        // steps, -100000 steps by -33333 (the asm's reciprocal: -33334).
        assert_eq!((-100_000i64 / 3, (-100_000i64 * RECIPROCAL_16[3]) >> 31), (-33_333, -33_334));
        // A span of 3 pixels on a 4x1 block whose ends clamp to N/65536 texel:
        // the start may be 0, every later end at least 8 (D_DrawSpans8's guard).
        let flat = |origin| FloatPlane { origin, stepu: 0.0, stepv: 0.0 };
        let g = SurfGrads {
            zi: flat(1.0),
            sdivz: flat(-1.0 / 65536.0),
            tdivz: flat(0.0),
            sadjust: 0,
            tadjust: 0,
            bbextents: (4 << 16) - 1,
            bbextentt: (1 << 16) - 1,
        };
        let mut row = [9u8; 20];
        span_c_cached::<8>(&mut row, 0, 0, &g, &[1, 2, 3, 4], 4);
        assert!(row.iter().all(|&p| p == 1), "clamped into texel 0: {row:?}");
    }

    /// The point of the shorter spans: the affine error of a run grows with
    /// the square of its length, so against exact perspective 8's error is
    /// about a quarter of 16's and 4's a sixteenth. Measured on a steeply
    /// oblique wall whose texels are a few pixels wide, so the error is
    /// tenths of a texel to texels and its mean, over every pixel, is
    /// measured to a fraction of a texel: the block's texel at `bx` is `bx`,
    /// so a pixel's error is how many texels it is off. (2026-10-03: 2.97,
    /// 0.77 and 0.19 texels, so 0.26 and 0.25.)
    #[test]
    fn the_error_against_exact_shrinks_with_the_square_of_the_span() {
        let (w, h) = (128usize, 4usize);
        let (bw, bh) = (256usize, 4usize);
        let block: Vec<u8> = (0..bw * bh).map(|i| (i % bw) as u8).collect();
        let v = |x: f32, y: f32, vz: f32, s: f32| AttrVert { x, y, vz, s, t: 1.5 };
        // 1/z from 1 to 1/12 across the row: the near end's texels are 4
        // pixels wide, the far end's a third of a pixel.
        let quad = [
            v(0.0, 0.0, 1.0, 0.5),
            v(w as f32, 0.0, 12.0, 255.0),
            v(w as f32, h as f32, 12.0, 255.0),
            v(0.0, h as f32, 1.0, 0.5),
        ];
        let g = PolyGrads::from_vertices(&quad).expect("quad");
        let g = SurfGrads::from_poly(&g, [0.0, 0.0], bw, bh);
        let draw = |persp| id_spans(w, h, 0, |row, u, v| span_cached(row, u, v, &g, &block, bw, bh, persp)).pixels;
        let exact = draw(PerspSpan::Exact);
        let err = |persp| {
            let img = draw(persp);
            img.iter().zip(&exact).map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs()).sum::<f64>() / img.len() as f64
        };
        let (e16, e8, e4) = (err(PerspSpan::Spans16), err(PerspSpan::Spans8), err(PerspSpan::Spans4));
        assert!(e16 > 1.0, "16's error is texels on this wall: {e16}");
        let (q8, q4) = (e8 / e16, e4 / e8);
        assert!((0.15..0.35).contains(&q8), "8 against 16: {q8:.3} ({e16:.3} -> {e8:.3} texels)");
        assert!((0.15..0.35).contains(&q4), "4 against 8: {q4:.3} ({e8:.3} -> {e4:.3} texels)");
        // And the longer ones, near four times each (10.34 and 31.09 texels:
        // a 64-pixel segment is half this 128-pixel row, so 64's falls short).
        let (e32, e64) = (err(PerspSpan::Spans32), err(PerspSpan::Spans64));
        let (q32, q64) = (e16 / e32, e32 / e64);
        assert!((0.15..0.35).contains(&q32), "16 against 32: {q32:.3} ({e32:.3} -> {e16:.3} texels)");
        assert!((0.15..0.35).contains(&q64), "32 against 64: {q64:.3} ({e64:.3} -> {e32:.3} texels)");
    }

    #[test]
    fn spans16_is_exact_at_each_segment_start_of_a_span() {
        // An oblique wall (1/z from 1 to 1/8 across 40 px): D_DrawSpans16
        // divides at the span's first pixel and every 16 pixels after it (0, 16,
        // 32: the last segment is 32..40), stepping affinely between — so it
        // agrees with exact perspective there and not everywhere else.
        let w = 40usize;
        let exact = cached_quad(PerspSpan::Exact, 1.0, 8.0, 0);
        let s16 = cached_quad(PerspSpan::Spans16, 1.0, 8.0, 0);
        for y in 0..8 {
            for x in [0usize, 16, 32] {
                assert_eq!(s16[y * w + x], exact[y * w + x], "row {y} pixel {x}");
            }
        }
        assert!(s16.iter().zip(&exact).filter(|(a, b)| a != b).count() > 40, "segments are affine");
        // The same wall behind a nearer surface over its first 5 columns: id's
        // span (R_ScanEdges) starts at the first visible pixel, so the grid
        // restarts there: exact at 5, 21 and 37.
        let hidden = cached_quad(PerspSpan::Spans16, 1.0, 8.0, 5);
        for y in 0..8 {
            assert!(hidden[y * w..y * w + 5].iter().all(|&p| p == 0), "hidden pixels untouched");
            for x in [5usize, 21, 37] {
                assert_eq!(hidden[y * w + x], exact[y * w + x], "row {y} pixel {x}");
            }
        }
        assert_ne!(hidden[5 * w..6 * w], s16[5 * w..6 * w], "the grid moved with the span");
    }

    #[test]
    fn spans16_steps_like_d_draw16_s() {
        // The last segment's step is reciprocal_table_16's floor(ds * R / 2^31),
        // not the C division (which truncates toward zero): over 3 steps,
        // 100000 steps by 33333 either way, -100000 by -33334 (the C: -33333).
        assert_eq!((100_000i64 * RECIPROCAL_16[3]) >> 31, 33_333);
        assert_eq!((-100_000i64 * RECIPROCAL_16[3]) >> 31, -33_334);
        for (n, &r) in RECIPROCAL_16.iter().enumerate().skip(2) {
            assert_eq!(r, (1i64 << 31) / n as i64, "1/{n} in 1.31");
        }
        // A full segment's step keeps 20 fractional bits (the asm's frac << 12
        // with carry): from s = 0xFFF0 towards snext = s + 31, pixel 15 is at
        // 0xFFF0 + 15*31/16 = 0x1_0001.1 -> texel 1, where D_DrawSpans8-style
        // `(snext - s) >> 4` steps (1 each) stop at 0xFFFF -> texel 0.
        let (s, ds) = (0xFFF0i64, 31i64);
        assert_eq!(((16 * s + 15 * ds) >> 20, (s + 15 * (ds >> 4)) >> 16), (1, 0));
    }

    /// The exact spans by parabolas against the divide at every pixel, over
    /// random spans: level floors, oblique and grazing walls, spans hanging
    /// off the block, and spans steered so that a pixel lands within two
    /// units of a texel's edge. (It cannot tell a guard slack of 3 from 9:
    /// [`cheap_exact_steered_to_its_guards_is_the_divide`] can.)
    #[test]
    fn exact_by_parabolas_is_the_divide_at_every_pixel() {
        let mut seed = 0x243F_6A88_85A3_08D3u64;
        let mut next = move || {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            seed.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        let mut range = |lo: f64, hi: f64| lo + (next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo);
        let spans: usize = std::env::var("QUAKE_FUZZ_SPANS").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000);
        let (mut fast, mut steered_hits) = (0usize, 0usize);
        for i in 0..spans {
            let len = 1 + range(0.0, if i % 5 == 0 { 1400.0 } else { 300.0 }) as usize;
            let (bw, bh) = (4 + range(0.0, 508.0) as usize, 4 + range(0.0, 508.0) as usize);
            let block: Vec<u8> = (0..bw * bh).map(|j| (j * 13 + j / bw) as u8).collect();
            // 1/z at the span's ends (a ratio up to 30, or level), the
            // texels a pixel (0.02 to 3), where the span starts in the block.
            let zi0 = range(1e-4, 0.05);
            let ratio = match i % 4 {
                0 => 1.0,
                1 => 1.0 + range(-1e-3, 1e-3),
                2 => range(0.5, 2.0),
                _ => range(1.0 / 30.0, 30.0),
            };
            let dzi = zi0 * (ratio - 1.0) / len as f64;
            let zi1 = zi0 + dzi * (len - 1).max(1) as f64;
            let (rs, rt) = (range(-3.0, 3.0), range(-3.0, 3.0));
            let (s0, t0) = (range(-2.0, bw as f64 + 2.0), range(-2.0, bh as f64 + 2.0));
            let (s1, t1) = (s0 + rs * len as f64, t0 + rt * len as f64);
            // s/z linear from s0*zi0 to s1*zi1 (the eye at texel 0: sadjust
            // carries nothing but the steering below).
            let lin = |a: f64, b: f64| (a * zi0, (b * zi1 - a * zi0) / (len - 1).max(1) as f64);
            let ((sz, dsz), (tz, dtz)) = (lin(s0, s1), lin(t0, t1));
            let sp = Span { zi: zi0, sz, tz, dzi, dsz, dtz };
            let mut fx = BlockFixed {
                sadjust: 0,
                tadjust: 0,
                bbextents: ((bw as i64) << 16) - 1,
                bbextentt: ((bh as i64) << 16) - 1,
            };
            // Steer: a pixel's s (or t) to within two units of a texel edge.
            if i % 3 == 0 {
                let k = range(0.0, len as f64) as usize;
                let (s, t) = sp.st_at(k, 0, 0);
                let off = range(-2.0, 3.0) as i64;
                if i % 2 == 0 { fx.sadjust = off - (s & 0xFFFF) } else { fx.tadjust = off - (t & 0xFFFF) }
            }
            let (mut new, mut old) = (vec![0u8; len], vec![0u8; len]);
            span_exact_cached(&mut new, &sp, &fx, &block, bw, bh);
            span_exact_reference(&mut old, &sp, &fx, &block, bw, bh);
            assert!(
                new == old,
                "span {i}: {len} pixels on {bw}x{bh}, {sp:?}, sadjust {} tadjust {}: first difference at {:?}",
                fx.sadjust,
                fx.tadjust,
                new.iter().zip(&old).position(|(a, b)| a != b)
            );
            fast += usize::from(exact_plan(len, &sp, bw, bh, block.len()).is_some());
            steered_hits += usize::from(i % 3 == 0);
        }
        assert!(fast * 2 > spans, "most spans take the parabolas: {fast} of {spans}");
        assert!(steered_hits > 0);
    }

    /// A span for cheap exact's proof tests: the fuzz's shapes, and also
    /// `1/z` varying up to 1000x along the span (grazing the near plane) and
    /// nearly level spans with tiny texel rates. Its length, block size,
    /// the span and its `BlockFixed`.
    fn proof_span(r: &mut Rng, i: usize) -> (usize, usize, usize, Span, BlockFixed) {
        let len = 33 + r.range(0.0, if i % 5 == 0 { 1400.0 } else { 300.0 }) as usize;
        let (bw, bh) = (4 + r.below(509) as usize, 4 + r.below(509) as usize);
        let zi0 = r.range(1e-4, 0.05);
        let ratio = match i % 6 {
            0 => 1.0,
            1 => 1.0 + r.range(-1e-3, 1e-3),
            2 => r.range(0.5, 2.0),
            3 => r.range(1.0 / 30.0, 30.0),
            4 => r.range(-1000f64.ln(), 1000f64.ln()).exp(),
            _ => r.range(-100f64.ln(), 100f64.ln()).exp(),
        };
        let dzi = zi0 * (ratio - 1.0) / len as f64;
        let zi1 = zi0 + dzi * (len - 1) as f64;
        let rate = if i % 7 == 0 { 0.02 } else { 3.0 };
        let (rs, rt) = (r.range(-rate, rate), r.range(-rate, rate));
        let (s0, t0) = (r.range(-2.0, bw as f64 + 2.0), r.range(-2.0, bh as f64 + 2.0));
        let (s1, t1) = (s0 + rs * len as f64, t0 + rt * len as f64);
        let lin = |a: f64, b: f64| (a * zi0, (b * zi1 - a * zi0) / (len - 1) as f64);
        let ((sz, dsz), (tz, dtz)) = (lin(s0, s1), lin(t0, t1));
        let sp = Span { zi: zi0, sz, tz, dzi, dsz, dtz };
        let fx = BlockFixed {
            sadjust: 0,
            tadjust: 0,
            bbextents: ((bw as i64) << 16) - 1,
            bbextentt: ((bh as i64) << 16) - 1,
        };
        (len, bw, bh, sp, fx)
    }

    /// The reference's 16.16 positions at every pixel, before `>> 16` (`R`
    /// of the proof, s and t), and the products they truncate.
    fn reference_positions(sp: &Span, fx: &BlockFixed, n: usize) -> Vec<([i64; 2], [f64; 2])> {
        let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let z = 65536.0 / zi;
            let (fs, ft) = (sz * z, tz * z);
            out.push(([(fs as i64).wrapping_add(fx.sadjust), (ft as i64).wrapping_add(fx.tadjust)], [fs, ft]));
            zi += sp.dzi;
            sz += sp.dsz;
            tz += sp.dtz;
        }
        out
    }

    /// The segments [`span_exact_cached`] draws by parabolas, in its order:
    /// each one's first pixel, its s and t parabolas and their
    /// interpolation bounds `es`.
    fn parabola_segments(
        sp: &Span,
        fx: &BlockFixed,
        n: usize,
        bw: usize,
        bh: usize,
        scale: (f64, f64),
    ) -> Vec<(usize, [Parabola; 2], [f64; 2])> {
        let max = (((bw as i64) << 16) - 1, ((bh as i64) << 16) - 1);
        let knot = |k: usize| sp.knot(k, fx.sadjust, fx.tadjust);
        let last = n - 1 - EXACT_SEG;
        let mut out = Vec::new();
        let mut k0 = 0;
        loop {
            let (a, m, b) = (knot(k0), knot(k0 + EXACT_KNOT), knot(k0 + EXACT_SEG));
            if let Some((s, t)) = segment_parabolas(a, m, b, scale, max) {
                let (es, et) = interpolation_bounds(a, b, scale);
                out.push((k0, [s, t], [es, et]));
            }
            if k0 == last {
                break;
            }
            k0 = (k0 + EXACT_SEG).min(last);
        }
        out
    }

    /// The proof of cheap exact at every pixel a parabola draws: `|R -
    /// y/128| < es + 4.54` ((a)-(d)), so under the guard; and each of its
    /// parts at its worst — the interpolation `|Q - X| <= es` (`Q` the
    /// parabola through the true `X` at the knots) and the rounding of the
    /// reference's and the knots' products, at most half of
    /// [`exact_plan`]'s `noise`. 20,000 spans (`QUAKE_FUZZ_SPANS`; 2,000,000
    /// with `--nocapture` print the worst cases: 2.98, 0.998 and 0.457).
    #[test]
    fn cheap_exact_is_within_its_proven_bound_at_every_pixel() {
        let spans: usize = std::env::var("QUAKE_FUZZ_SPANS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
        let mut r = Rng(0x5eed_0f7e_71e5 | 1);
        let (mut worst, mut worst_interp, mut worst_noise) = (0f64, 0f64, 0f64);
        let (mut segments, mut lane_pixels) = (0usize, 0u64);
        for i in 0..spans {
            let (n, bw, bh, sp, fx) = proof_span(&mut r, i);
            let Some(scale) = exact_plan(n, &sp, bw, bh, bw * bh) else { continue };
            let reference = reference_positions(&sp, &fx, n);
            // exact_plan's noise, recomputed.
            let last = (n - 1) as f64;
            let zi_end = sp.zi + last * sp.dzi;
            let (lo, hi) = if sp.zi < zi_end { (sp.zi, zi_end) } else { (zi_end, sp.zi) };
            let ends = |a: f64, d: f64| a.abs().max((a + last * d).abs());
            let reach = 65536.0 * ends(sp.sz, sp.dsz).max(ends(sp.tz, sp.dtz)) / lo;
            let noise = f64::EPSILON * ((2 * n + 4) as f64 * (hi / lo) + 4.0) * reach;
            // X, the true coordinate (straight from the span in f64: its own
            // error is ~1e-16 relative, far below the units measured here).
            let x = |k: usize, a: f64, d: f64| 65536.0 * (a + k as f64 * d) / (sp.zi + k as f64 * sp.dzi);
            if noise > 0.0 {
                for (k, (_, products)) in reference.iter().enumerate() {
                    let knot = sp.knot(k, 0, 0);
                    let z = knot.z;
                    for (c, (a, d)) in [(sp.sz, sp.dsz), (sp.tz, sp.dtz)].into_iter().enumerate() {
                        let xk = x(k, a, d);
                        worst_noise = worst_noise
                            .max((products[c] - xk).abs() / noise)
                            .max(((a + k as f64 * d) * z - xk).abs() / noise);
                    }
                }
            }
            for (k0, parabolas, bounds) in parabola_segments(&sp, &fx, n, bw, bh, scale) {
                segments += 1;
                for (c, (p, es)) in parabolas.into_iter().zip(bounds).enumerate() {
                    let (a, d) = if c == 0 { (sp.sz, sp.dsz) } else { (sp.tz, sp.dtz) };
                    let (x0, xh, x2h) = (x(k0, a, d), x(k0 + EXACT_KNOT, a, d), x(k0 + EXACT_SEG, a, d));
                    let (mut y, mut d1) = (p.start, p.d1);
                    for j in 0..EXACT_SEG {
                        let k = k0 + j;
                        let dist = (128 * reference[k].0[c] - y).abs();
                        assert!(
                            dist < 128 * p.guard,
                            "pixel {k} of span {i}, lane {c}: |128 R - y| = {dist}, guard {} (es {es})",
                            p.guard
                        );
                        worst = worst.max(dist as f64 / 128.0 - es);
                        // Q through the true X at the knots, at t = j / H.
                        let t = j as f64 / EXACT_KNOT as f64;
                        let q = x0 * (t - 1.0) * (t - 2.0) / 2.0 - xh * t * (t - 2.0) + x2h * t * (t - 1.0) / 2.0;
                        if es > 1e-3 {
                            worst_interp = worst_interp.max((q - x(k, a, d)).abs() / es);
                        }
                        y += d1;
                        d1 += p.d2;
                        lane_pixels += 1;
                    }
                }
            }
        }
        eprintln!(
            "cheap exact's bound: {spans} spans, {segments} segments, {lane_pixels} lane pixels; worst |R - y/128| - es = {worst:.4} (proof: < 4.54), |Q - X| / es = {worst_interp:.4} (<= 1), rounding / noise = {worst_noise:.4} (<= 1/2)"
        );
        assert!(segments > spans, "most spans have segments by parabola: {segments}");
        assert!(worst < 4.54 && worst_interp <= 1.0 && worst_noise <= 0.5);
    }

    /// The texel fuzz made tight: in one parabola segment of each span the
    /// pixel whose reference position is furthest from the parabola's is
    /// moved (by `sadjust` or `tadjust`, which move the knots, the parabola
    /// and the reference by the same whole units) to sit just inside its
    /// guard, on the side that puts the reference across the texel's edge if
    /// the guard is too small. With the code's guard the texels are the
    /// divide's; with a [`GUARD_SLACK`] of 3 they are not, where the random
    /// fuzz needs a pixel to land there by luck. 100,000 spans
    /// (`QUAKE_FUZZ_SPANS`; `--nocapture` prints the smallest margin: 5.99
    /// units over 2,000,000).
    #[test]
    fn cheap_exact_steered_to_its_guards_is_the_divide() {
        const TEXEL: i64 = 1 << LANE_FRAC;
        let spans: usize = std::env::var("QUAKE_FUZZ_SPANS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
        let mut r = Rng(0x57ee_12ed | 1);
        let (mut steered, mut min_margin, mut differ) = (0usize, i64::MAX, Vec::new());
        for i in 0..spans {
            let (n, bw, bh, sp, unmoved) = proof_span(&mut r, i);
            let Some(scale) = exact_plan(n, &sp, bw, bh, bw * bh) else { continue };
            let reference = reference_positions(&sp, &unmoved, n);
            let segments = parabola_segments(&sp, &unmoved, n, bw, bh, scale);
            if segments.is_empty() {
                continue;
            }
            let (k0, parabolas, _) = segments[r.below(segments.len() as u64) as usize];
            let c = i % 2;
            let p = parabolas[c];
            // The segment's pixel furthest from the reference: `dist` = 128 R - y.
            let (mut y, mut d1) = (p.start, p.d1);
            let (mut dist, mut at) = (0i64, p.start);
            for j in 0..EXACT_SEG {
                let off = 128 * reference[k0 + j].0[c] - y;
                if off.abs() >= dist.abs() {
                    (dist, at) = (off, y);
                }
                y += d1;
                d1 += p.d2;
            }
            // Its fraction moved to just inside the guard, on the reference's
            // side (in whole 16.16 units: 128 lane units).
            let guard = 128 * p.guard;
            let frac = at.rem_euclid(TEXEL);
            let (target, gap) = if dist < 0 {
                let t = guard + frac.rem_euclid(128);
                (t, t)
            } else {
                let t = (TEXEL - guard - 1) - (TEXEL - guard - 1 - frac).rem_euclid(128);
                (t, TEXEL - t)
            };
            let units = (target - frac) / 128;
            let fx = if c == 0 {
                BlockFixed { sadjust: unmoved.sadjust + units, ..unmoved }
            } else {
                BlockFixed { tadjust: unmoved.tadjust + units, ..unmoved }
            };
            // Still drawn by its parabola? (The move may take it off the block.)
            if !parabola_segments(&sp, &fx, n, bw, bh, scale).iter().any(|s| s.0 == k0) {
                continue;
            }
            steered += 1;
            min_margin = min_margin.min(gap - dist.abs() - i64::from(dist > 0));
            let block: Vec<u8> = (0..bw * bh).map(|j| (j * 13 + j / bw) as u8).collect();
            let (mut new, mut old) = (vec![0u8; n], vec![0u8; n]);
            span_exact_cached(&mut new, &sp, &fx, &block, bw, bh);
            span_exact_reference(&mut old, &sp, &fx, &block, bw, bh);
            if new != old {
                differ.push(format!(
                    "span {i} ({n} px on {bw}x{bh}, segment {k0}, lane {c}): first difference at {:?}",
                    new.iter().zip(&old).position(|(a, b)| a != b)
                ));
            }
        }
        eprintln!(
            "cheap exact steered: {steered} segments, smallest margin {:.3} units (GUARD_SLACK {GUARD_SLACK}), {} differ",
            min_margin as f64 / 128.0,
            differ.len()
        );
        assert!(steered * 3 > spans, "most spans steered: {steered}");
        assert!(
            differ.is_empty(),
            "{} steered segments differ from the divide, the first: {}",
            differ.len(),
            differ[0]
        );
    }

    #[test]
    fn a_span_at_zero_1_over_z_wraps_like_the_c_int() {
        // zi exactly 0 at a near-clipped edge: z = 0x10000 / 0 is infinite,
        // the cast gives the C's 0x80000000 (the exact perspective's double
        // saturates), and `+ sadjust` wraps as the C's `s = (int)(sdivz * z) +
        // sadjust` does (an overflow panicked in a debug build once); the span
        // routines clamp into the surface.
        let sp = Span { zi: 0.0, sz: 1.0, tz: -1.0, dzi: 0.0, dsz: 0.0, dtz: 0.0 };
        assert_eq!(sp.st_at(0, 5, -5), (i64::MAX.wrapping_add(5), i64::MIN.wrapping_add(-5)));
        let flat = |origin| FloatPlane { origin, stepu: 0.0, stepv: 0.0 };
        let g = SurfGrads {
            zi: flat(0.0),
            sdivz: flat(1.0),
            tdivz: flat(-1.0),
            sadjust: 5,
            tadjust: -5,
            bbextents: (4 << 16) - 1,
            bbextentt: (4 << 16) - 1,
        };
        assert_eq!(FloatSpan::start(&g, 0, 0).st(&g), (i32::MIN.wrapping_add(5), i32::MIN.wrapping_add(-5)));
        let block = [7u8; 16];
        for persp in PerspSpan::ALL {
            let mut row = [9u8; 20];
            span_cached(&mut row, 0, 0, &g, &block, 4, 4, persp);
            assert!(row.iter().all(|&p| p == 7), "{persp:?}: every pixel reads the block");
        }
    }

    /// The test gradients' guard: a zero-area or non-finite triangle has none.
    #[test]
    fn degenerate_vertices_have_no_gradients() {
        let pv = |x: f32, y: f32| AttrVert { x, y, vz: 1.0, s: 0.0, t: 0.0 };
        assert!(PolyGrads::from_vertices(&[pv(0.0, 0.0), pv(4.0, 4.0), pv(8.0, 8.0)]).is_none());
        assert!(PolyGrads::from_vertices(&[pv(0.0, 0.0), pv(f32::NAN, 4.0), pv(8.0, 0.0)]).is_none());
        assert!(PolyGrads::from_vertices(&[pv(0.0, 0.0), pv(f32::INFINITY, 4.0), pv(8.0, 0.0)]).is_none());
    }

    /// The gradients reproduce the vertices' perspective attributes: a span's
    /// accumulators give, at every pixel centre, the exact perspective-correct
    /// s and t of the plane through the vertices, whichever vertex triple the
    /// solve picked.
    #[test]
    fn gradients_are_perspective_correct() {
        // Four view-space points on the plane vz = 6 + 0.5*vx + 0.3*vy.
        let focal = 16.0f32;
        let pts = [(-3.0f32, -2.0f32), (5.0, -2.5), (6.0, 4.0), (-4.0, 3.0)]
            .map(|(vx, vy)| (vx, vy, 6.0 + 0.5 * vx + 0.3 * vy));
        // s = 10*vx + 3, t = 7*vy - 1 (affine in world space, as texinfo is).
        let poly: Vec<AttrVert> = pts
            .iter()
            .map(|&(vx, vy, vz)| AttrVert {
                x: 16.0 + focal * vx / vz,
                y: 16.0 - focal * vy / vz,
                vz,
                s: 10.0 * vx + 3.0,
                t: 7.0 * vy - 1.0,
            })
            .collect();
        let g = PolyGrads::from_vertices(&poly).expect("well-conditioned quad");
        let mut checked = 0;
        for y in 8..24 {
            let sp = span_at(&g, 8, y);
            let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
            for px in 8..24 {
                // The view ray through this pixel centre hits the plane through
                // the four points; recover (vx, vy, vz) from 1/z exactly.
                let z = 1.0 / zi;
                let focal = focal as f64;
                let vx = (px as f64 + 0.5 - 16.0) * z / focal;
                let vy = (16.0 - (y as f64 + 0.5)) * z / focal;
                assert!((sz * z - (10.0 * vx + 3.0)).abs() < 1e-4, "s at ({px},{y})");
                assert!((tz * z - (7.0 * vy - 1.0)).abs() < 1e-4, "t at ({px},{y})");
                checked += 1;
                zi += sp.dzi;
                sz += sp.dsz;
                tz += sp.dtz;
            }
        }
        assert!(checked > 20, "the plane covers pixels");
    }

    /// `PolyGrads::for_plane` (the analytic `D_CalcGradients`) reproduces, at
    /// the projection of points on the plane, their exact 1/z and texinfo (s,t)
    /// — for a world face and for a brush-entity face in its local frame.
    #[test]
    fn plane_gradients_match_projected_points() {
        use crate::math::{cross, dot, normalize};
        let cam = crate::render::Camera::looking_at([10.0, -20.0, 30.0], [200.0, 50.0, -10.0], 90.0);
        let (forward, right, up) = cam.basis();
        let (cx, cy, xscale) = (160.0f32, 100.0f32, 160.0f32);
        // Square pixels and id's 320x200 on a 4:3 screen (pixelAspect 0.8333).
        for yscale in [xscale, xscale * (200.0 / 320.0 * 4.0 / 3.0)] {
            let view = ScreenProj { forward, right, up, cx, cy, xscale, yscale };
            let (n, _) = normalize([0.3, -0.5, 0.8]);
            let dist = 40.0f32;
            let ti = crate::bsp::TexInfo { vecs: [[1.0, 0.0, 0.0, 5.5], [0.0, 0.7, 0.7, -3.0]], miptex: 0, flags: 0 };
            let (u, _) = normalize(cross(n, [0.0, 0.0, 1.0]));
            let v = cross(n, u);
            for origin in [[0.0f32; 3], [64.0, -32.0, 8.0]] {
                // The model's frame: the plane and texinfo are local; the eye too.
                let eye = [cam.pos[0] - origin[0], cam.pos[1] - origin[1], cam.pos[2] - origin[2]];
                let g = PolyGrads::for_plane(&view, eye, n, dist, Some(&ti)).expect("eye off the plane");
                for (a, b) in [(0.0f32, 0.0f32), (150.0, 20.0), (-80.0, 90.0), (300.0, -120.0)] {
                    let p = [
                        n[0] * dist + a * u[0] + b * v[0],
                        n[1] * dist + a * u[1] + b * v[1],
                        n[2] * dist + a * u[2] + b * v[2],
                    ];
                    let rel = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
                    let (vx, vy, vz) = (dot(rel, right), dot(rel, up), dot(rel, forward));
                    if vz <= 1.0 {
                        continue;
                    }
                    let (x, y) = ((cx + xscale * vx / vz) as f64, (cy - yscale * vy / vz) as f64);
                    let zi = g.zi.at(x, y);
                    let s = g.sz.at(x, y) / zi + g.st_eye[0];
                    let t = g.tz.at(x, y) / zi + g.st_eye[1];
                    let want_s = (p[0] * 1.0 + 5.5) as f64;
                    let want_t = (p[1] * 0.7 + p[2] * 0.7 - 3.0) as f64;
                    assert!((zi * vz as f64 - 1.0).abs() < 1e-4, "1/z at ({a},{b}): {zi} vs {}", 1.0 / vz);
                    assert!((s - want_s).abs() < 2e-3, "s at ({a},{b}): {s} vs {want_s}");
                    assert!((t - want_t).abs() < 2e-3, "t at ({a},{b}): {t} vs {want_t}");
                }
            }
            // An eye on the plane sees it edge-on: no gradients.
            assert!(PolyGrads::for_plane(&view, [5.0, 7.0, 40.0], [0.0, 0.0, 1.0], 40.0, None).is_none());
        }
    }

    // -- The C span routines at every length, fuzzed against id's C ---------

    /// A small deterministic generator (xorshift64*) for the fuzz below.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        /// Uniform in `[lo, hi)`.
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Random gradients for a span of `len` pixels from `(u, v)` over a `bw x
    /// bh` block: its 1/z from far to near, now and then zero or negative
    /// (rounding at a clipped edge), steps that turn the texel coordinate by
    /// up to several blocks across the span, and now and then huge ones; the
    /// eye anywhere from a block before the block to a block past it. The
    /// planes are floats, as `D_CalcGradients` leaves them.
    fn random_grads(r: &mut Rng, len: usize, (u, v): (usize, usize), bw: usize, bh: usize) -> SurfGrads {
        let texels = bw.max(bh) as f64;
        let zi = match r.below(16) {
            0 => 0.0,
            1 => -r.range(0.0, 0.01),
            _ => r.range(1e-4, 0.05),
        };
        let dzi = match r.below(8) {
            0 => 0.0,
            _ => r.range(-1.0, 1.0) * zi.abs().max(1e-4) / len as f64,
        };
        let scale = if r.below(32) == 0 { 1e6 } else { 1.0 };
        let (s0, t0) = (r.range(-texels, 2.0 * texels), r.range(-texels, 2.0 * texels));
        let (s1, t1) = (r.range(-3.0, 3.0) * texels * scale, r.range(-3.0, 3.0) * texels * scale);
        let zi1 = zi + dzi * len as f64;
        // The planes through those values at (u, v), with a random v step.
        let plane = |r: &mut Rng, at: f64, stepu: f64| {
            let stepv = r.range(-1.0, 1.0) * stepu.abs().max(at.abs() * 1e-3);
            let (stepu, stepv) = (stepu as f32, stepv as f32);
            FloatPlane { origin: (at - v as f64 * f64::from(stepv) - u as f64 * f64::from(stepu)) as f32, stepu, stepv }
        };
        SurfGrads {
            zi: plane(r, zi, dzi),
            sdivz: plane(r, s0 * zi, (s1 * zi1 - s0 * zi) / len as f64),
            tdivz: plane(r, t0 * zi, (t1 * zi1 - t0 * zi) / len as f64),
            sadjust: r.range(-1.0, 2.0) as i32 * ((bw as i32) << 16) + r.below(1 << 16) as i32,
            tadjust: r.range(-1.0, 2.0) as i32 * ((bh as i32) << 16) + r.below(1 << 16) as i32,
            bbextents: ((bw as i32) << 16) - 1,
            bbextentt: ((bh as i32) << 16) - 1,
        }
    }

    /// `if (v > hi) v = hi; else if (v < lo) v = lo;`, as the C writes it.
    fn c_clamp(v: i32, lo: i32, hi: i32) -> i32 {
        if v > hi {
            hi
        } else if v < lo {
            lo
        } else {
            v
        }
    }

    /// The C's span setup, as `D_DrawSpans8`, `D_DrawSpans16` and
    /// `Turbulent8` all write it: the accumulators at `du = (float)pspan->u`,
    /// `dv = (float)pspan->v` and the first, clamped position.
    fn c_span_start(g: &SurfGrads, u: usize, v: usize) -> ([f32; 3], (i32, i32)) {
        let (du, dv) = (u as f32, v as f32);
        let sdivz = g.sdivz.origin + dv * g.sdivz.stepv + du * g.sdivz.stepu;
        let tdivz = g.tdivz.origin + dv * g.tdivz.stepv + du * g.tdivz.stepu;
        let zi = g.zi.origin + dv * g.zi.stepv + du * g.zi.stepu;
        let z = 65536.0 / zi;
        let s = c_clamp(c_ftoi(sdivz * z).wrapping_add(g.sadjust), 0, g.bbextents);
        let t = c_clamp(c_ftoi(tdivz * z).wrapping_add(g.tadjust), 0, g.bbextentt);
        ([sdivz, tdivz, zi], (s, t))
    }

    /// A segment end as the C's: the accumulators moved by `d*_stepu * n`
    /// (`n` a float: 8, 16, or `spancountminus1`), the divide, the clamp to
    /// `[lo, bb]` in `D_DrawSpans8`'s order.
    fn c_segment_end(g: &SurfGrads, acc: &mut [f32; 3], n: f32, lo: i32) -> (i32, i32) {
        acc[0] += g.sdivz.stepu * n;
        acc[1] += g.tdivz.stepu * n;
        acc[2] += g.zi.stepu * n;
        let z = 65536.0 / acc[2];
        let s = c_clamp(c_ftoi(acc[0] * z).wrapping_add(g.sadjust), lo, g.bbextents);
        let t = c_clamp(c_ftoi(acc[1] * z).wrapping_add(g.tadjust), lo, g.bbextentt);
        (s, t)
    }

    /// `D_DrawSpans8` as d_scan.c writes it, `n_px` for its 8 (`>> 3` its
    /// shift, 8 its guard, `sdivz8stepu = d_sdivzstepu * 8`): one span of the
    /// block from `(u, v)`, reading `block[...]` unchecked by anything but
    /// the slice's own bound — a position off the block panics.
    #[allow(clippy::too_many_arguments)]
    fn d_draw_spans8_as_written(
        n_px: i32,
        crow: &mut [u8],
        u: usize,
        v: usize,
        g: &SurfGrads,
        block: &[u8],
        bw: usize,
    ) {
        let shift = n_px.trailing_zeros();
        let mut count = crow.len() as i32;
        let (mut acc, (mut s, mut t)) = c_span_start(g, u, v);
        let (mut sstep, mut tstep) = (0i32, 0i32);
        let mut pdest = 0usize;
        loop {
            let spancount = if count >= n_px { n_px } else { count };
            count -= spancount;
            let (snext, tnext);
            if count != 0 {
                (snext, tnext) = c_segment_end(g, &mut acc, n_px as f32, n_px);
                (sstep, tstep) = ((snext - s) >> shift, (tnext - t) >> shift);
            } else {
                (snext, tnext) = c_segment_end(g, &mut acc, (spancount - 1) as f32, n_px);
                if spancount > 1 {
                    (sstep, tstep) = ((snext - s) / (spancount - 1), (tnext - t) / (spancount - 1));
                }
            }
            let mut left = spancount;
            loop {
                crow[pdest] = block[(s >> 16) as usize + (t >> 16) as usize * bw];
                pdest += 1;
                s += sstep;
                t += tstep;
                left -= 1;
                if left <= 0 {
                    break;
                }
            }
            (s, t) = (snext, tnext);
            if count <= 0 {
                break;
            }
        }
    }

    /// `Turbulent8` and `D_DrawTurbulent8Span` as d_scan.c writes them, at
    /// `n_px` for its 16, in the C's `int`s from each segment's masked start.
    #[allow(clippy::too_many_arguments)]
    fn turbulent8_as_written(
        n_px: i32,
        crow: &mut [u8],
        u: usize,
        v: usize,
        g: &SurfGrads,
        pixels: &[u8],
        turb: &TurbTable,
        phase: usize,
    ) {
        let shift = n_px.trailing_zeros();
        let mut count = crow.len() as i32;
        let (mut acc, (mut s, mut t)) = c_span_start(g, u, v);
        let (mut sstep, mut tstep) = (0i32, 0i32);
        let mut pdest = 0usize;
        loop {
            let spancount = if count >= n_px { n_px } else { count };
            count -= spancount;
            let (snext, tnext);
            if count != 0 {
                (snext, tnext) = c_segment_end(g, &mut acc, n_px as f32, n_px);
                (sstep, tstep) = ((snext - s) >> shift, (tnext - t) >> shift);
            } else {
                (snext, tnext) = c_segment_end(g, &mut acc, (spancount - 1) as f32, n_px);
                if spancount > 1 {
                    (sstep, tstep) = ((snext - s) / (spancount - 1), (tnext - t) / (spancount - 1));
                }
            }
            let (mut a, mut b) = (s & TURB_COORD_MASK, t & TURB_COORD_MASK);
            let mut left = spancount;
            loop {
                let (sturb, tturb) = turb.texel(phase, a, b);
                crow[pdest] = pixels[(((tturb & 63) << 6) + (sturb & 63)) as usize];
                pdest += 1;
                a = a.wrapping_add(sstep);
                b = b.wrapping_add(tstep);
                left -= 1;
                if left <= 0 {
                    break;
                }
            }
            (s, t) = (snext, tnext);
            if count <= 0 {
                break;
            }
        }
    }

    /// `D_DrawSpans16` as the oracle's C writes it (`Oracle_DrawSpans16`,
    /// d_draw16.s's arithmetic), each segment's end divided for on reaching
    /// the segment: the reference the fuzz holds [`span16_cached`], which
    /// asks for each a segment ahead, to.
    fn d_draw_spans16_in_order(crow: &mut [u8], u: usize, v: usize, g: &SurfGrads, block: &[u8], bw: usize) {
        let end = crow.len();
        let (mut acc, (mut s, mut t)) = c_span_start(g, u, v);
        let mut k0 = 0;
        while k0 + 16 < end {
            let (sn, tn) = c_segment_end(g, &mut acc, 16.0, 4096);
            let (mut sa, mut ta, ds, dt) = (i64::from(s) * 16, i64::from(t) * 16, i64::from(sn - s), i64::from(tn - t));
            for c in &mut crow[k0..k0 + 16] {
                *c = block[(ta >> 20) as usize * bw + (sa >> 20) as usize];
                sa += ds;
                ta += dt;
            }
            (s, t) = (sn, tn);
            k0 += 16;
        }
        let steps = end - k0 - 1;
        let (mut ss, mut ts) = (0i32, 0i32);
        if steps > 0 {
            let (sn, tn) = c_segment_end(g, &mut acc, steps as f32, 4096);
            (ss, ts) = if steps == 1 {
                (sn - s, tn - t)
            } else {
                (
                    ((i64::from(sn - s) * RECIPROCAL_16[steps]) >> 31) as i32,
                    ((i64::from(tn - t) * RECIPROCAL_16[steps]) >> 31) as i32,
                )
            };
        }
        for c in crow.iter_mut().skip(k0) {
            *c = block[(t >> 16) as usize * bw + (s >> 16) as usize];
            s += ss;
            t += ts;
        }
    }

    /// The exact span with each coordinate clamped at every pixel, as
    /// [`span_exact_reference`] was before it tested "inside the block"
    /// first: the fuzz's reference for it and for [`span_exact_cached`].
    fn exact_clamped_every_pixel(crow: &mut [u8], sp: &Span, fx: &BlockFixed, block: &[u8], bw: usize, bh: usize) {
        let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
        for c in crow.iter_mut() {
            let z = 65536.0 / zi;
            let bx = (((sz * z) as i64).wrapping_add(fx.sadjust) >> 16).clamp(0, bw as i64 - 1) as usize;
            let by = (((tz * z) as i64).wrapping_add(fx.tadjust) >> 16).clamp(0, bh as i64 - 1) as usize;
            *c = block[by * bw + bx];
            zi += sp.dzi;
            sz += sp.dsz;
            tz += sp.dtz;
        }
    }

    /// The C span routines at every length the setting has — the walls'
    /// `D_DrawSpans8` at 4, 8, 32 and 64, the liquids' `Turbulent8` at 4, 8,
    /// 16, 32 and 64 — against literal transcriptions of id's C, its floats
    /// and its `int`s, over random spans (lengths 1 to 1400, blocks 1 to 300
    /// texels a side, 1/z zero or negative now and then, huge steps): the
    /// same pixels, and no position off the block (the transcription's
    /// unchecked read panics). A debug build (`cargo test --lib fuzz`)
    /// checks every add for overflow too. And `D_DrawSpans16` against its
    /// arithmetic in the C's order: the span loops ask for each segment's end
    /// a segment ahead ([`SegmentEnds`]), the references on reaching it. And
    /// the exact span, both ways (by parabolas and by the divide), against
    /// its two clamps at every pixel.
    #[test]
    fn the_c_spans_at_every_length_are_ids_c_and_stay_in_the_block() {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        let turb = TurbTable::new();
        let liquid: Vec<u8> = (0..64 * 64).map(|i| (i * 7 + i / 64) as u8).collect();
        // QUAKE_FUZZ_SPANS=N runs N spans instead.
        let spans = std::env::var("QUAKE_FUZZ_SPANS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(if cfg!(debug_assertions) { 1000 } else { 20_000 });
        let mut in_block = 0usize;
        for _ in 0..spans {
            let longest = if r.below(4) == 0 { 1400 } else { 200 };
            let len = 1 + r.below(longest) as usize;
            let at = (r.below(2000) as usize, r.below(1200) as usize);
            let (u, v) = at;
            let (bw, bh) = (1 + r.below(300) as usize, 1 + r.below(300) as usize);
            let block: Vec<u8> = (0..bw * bh).map(|i| (i * 13 + i / bw) as u8).collect();
            let g = random_grads(&mut r, len, at, bw, bh);
            let (mut port, mut c) = (vec![0u8; len], vec![0u8; len]);
            for (persp, n) in
                [(PerspSpan::Spans4, 4), (PerspSpan::Spans8, 8), (PerspSpan::Spans32, 32), (PerspSpan::Spans64, 64)]
            {
                span_cached(&mut port, u, v, &g, &block, bw, bh, persp);
                d_draw_spans8_as_written(n, &mut c, u, v, &g, &block, bw);
                assert_eq!(port, c, "{persp:?}, {len} pixels on {bw}x{bh} from {at:?}, {g:?}");
            }
            span_cached(&mut port, u, v, &g, &block, bw, bh, PerspSpan::Spans16);
            d_draw_spans16_in_order(&mut c, u, v, &g, &block, bw);
            assert_eq!(port, c, "D_DrawSpans16, {len} pixels on {bw}x{bh} from {at:?}, {g:?}");
            let (sp, fx) = (Span::from_grads(&g, u, v), BlockFixed::of(&g));
            span_cached(&mut port, u, v, &g, &block, bw, bh, PerspSpan::Exact);
            exact_clamped_every_pixel(&mut c, &sp, &fx, &block, bw, bh);
            assert_eq!(port, c, "exact, {len} pixels on {bw}x{bh} from {at:?}, {g:?}");
            span_exact_reference(&mut port, &sp, &fx, &block, bw, bh);
            assert_eq!(port, c, "exact (the reference), {len} pixels on {bw}x{bh} from {at:?}, {g:?}");
            // A span whose texels vary: it ran through the block, not only
            // along one clamped edge.
            in_block += usize::from(port.iter().any(|&p| p != port[0]));
            let liquid_g = SurfGrads {
                sadjust: r.range(-1e9, 1e9) as i32,
                tadjust: r.range(-1e9, 1e9) as i32,
                bbextents: (16384 << 16) - 1,
                bbextentt: (16384 << 16) - 1,
                ..g
            };
            let time = r.range(0.0, 100.0);
            let phase = turb_phase(time);
            for (persp, n) in [
                (PerspSpan::Spans4, 4),
                (PerspSpan::Spans8, 8),
                (PerspSpan::Spans16, 16),
                (PerspSpan::Spans32, 32),
                (PerspSpan::Spans64, 64),
            ] {
                span_turb(&mut port, u, v, &liquid_g, &liquid, 64, 64, &turb, time, persp);
                turbulent8_as_written(n, &mut c, u, v, &liquid_g, &liquid, &turb, phase);
                assert_eq!(port, c, "Turbulent8 at {n}, {len} pixels from {at:?}, {liquid_g:?}");
            }
        }
        assert!(in_block * 2 > spans, "most spans run through their block: {in_block} of {spans}");
    }

    /// [`wrap_texel`] against `rem_euclid` directly, across power-of-two
    /// (id's liquids are always 64x64) and non-power-of-two moduli, and the
    /// negative, zero and boundary values `D_DrawTurbulent8Span`'s fixed-point
    /// math can produce.
    #[test]
    fn wrap_texel_matches_rem_euclid_for_every_modulus() {
        for n in [1usize, 2, 4, 8, 64, 128, 3, 5, 60, 96] {
            for v in [-200i32, -65, -64, -63, -1, 0, 1, 63, 64, 65, 200, i32::MIN / 2, i32::MAX / 2] {
                assert_eq!(wrap_texel(v, n), v.rem_euclid(n as i32) as usize, "n={n} v={v}");
            }
        }
    }
}
