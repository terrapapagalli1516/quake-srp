//! The span routines `D_DrawSurfaces` draws brush surfaces with, and their
//! gradients.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `d_edge.c`'s `D_CalcGradients` ([`PolyGrads`]), `d_draw16.s`'s
//! `D_DrawSpans16` ([`span_cached`]), `d_scan.c`'s `Turbulent8`
//! ([`span_turb`]). [`super::edge`] hands each surface its spans. The flat
//! bounding-box triangle ([`raster_triangle`]) is the untextured debug
//! renderer's ([`super::render_bsp`]).

use super::Image;
use crate::math::Vec3;
use super::light::{colormap_row, LightMap, COLORMAP_LEN};
use super::warp::{turb_phase, warp_st, TurbTable, TURB_COORD_MASK};

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

/// Hash a (non-negative) surface index to a stable, reasonably saturated RGB
/// base colour, so distinct textures/surfaces get distinct hues across runs.
pub(super) fn hash_color(index: i64) -> [f32; 3] {
    // A small integer hash (splitmix-ish) to spread adjacent indices apart.
    let mut h = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;

    // Derive a hue in [0,1); keep saturation/value high but not blinding.
    let hue = ((h & 0xFFFF) as f32) / 65536.0;
    hsv_to_rgb(hue, 0.55, 0.95)
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
    image: &mut Image,
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
                    if let Some(p) = image.rgb.get_mut(idx) {
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

    /// The gradients in mip level `mip`'s texels: `s/z`, `t/z` and the eye's
    /// `(s, t)` times `1 / (1 << mip)` — `D_CalcGradients`' `mipscale` on
    /// `d_sdivzstepu`..`d_tdivzorigin` and `sadjust`/`tadjust` (a power of two,
    /// so exact). `1/z` is unchanged. For reading a surface block baked at that
    /// level ([`SurfBlock`](super::surf::SurfBlock)).
    pub(super) fn mip_scaled(&self, mip: u32) -> PolyGrads {
        let k = 1.0 / (1u32 << mip.min(3)) as f64;
        let sc = |l: Linear| Linear { o: l.o * k, dx: l.dx * k, dy: l.dy * k };
        PolyGrads { zi: self.zi, sz: sc(self.sz), tz: sc(self.tz), st_eye: [self.st_eye[0] * k, self.st_eye[1] * k] }
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

/// A surface's accumulators over one span: `1/z`, `s/z` and `t/z`
/// (eye-relative) at the centre of the span's first pixel and their per-pixel
/// steps —
/// `D_DrawSpans8`'s `zi`/`sdivz`/`tdivz` and `d_zistepu`/`d_sdivzstepu`/
/// `d_tdivzstepu`. The span loops step each with one add per pixel, in f64 (the
/// same cost as f32 in wasm, and no drift worth a texel across 1280 pixels);
/// the start is evaluated from the planes per span. The textured loops take
/// `s`/`t` from it either at every pixel ([`Persp::Exact`]) or at 16-pixel
/// segment ends ([`Persp::Spans16`], [`Span::st_at`]).
#[derive(Clone, Copy)]
pub(super) struct Span {
    zi: f64,
    sz: f64,
    tz: f64,
    dzi: f64,
    dsz: f64,
    dtz: f64,
}

impl Span {
    /// The 16.16 texel coordinates at pixel `k` of the span, unclamped: `z =
    /// 0x10000 / zi`, `s = (int)(sdivz * z) + sadjust` — `D_DrawSpans8`'s and
    /// `D_DrawSpans16`'s per-segment divide (the planes evaluated in f64 at
    /// the pixel; id accumulates `sdivz16stepu` in float). A zero `zi`
    /// (rounding at a near-clipped edge) makes `z` infinite and the product
    /// saturates; the add wraps, as the C's `int` does (never a debug-build
    /// overflow panic), and the callers clamp.
    #[inline]
    fn st_at(&self, k: usize, sadjust: i64, tadjust: i64) -> (i64, i64) {
        let kf = k as f64;
        let z = 65536.0 / (self.zi + kf * self.dzi);
        (
            (((self.sz + kf * self.dsz) * z) as i64).wrapping_add(sadjust),
            (((self.tz + kf * self.dtz) * z) as i64).wrapping_add(tadjust),
        )
    }
}

/// How a textured brush span finds its texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Persp {
    /// What 1996 players saw: the x86 WinQuake's `D_DrawSpans16` (`d_draw16.s`,
    /// `d_subdiv16` 1) on the surface cache and `Turbulent8` on liquids —
    /// exact perspective every 16 pixels, affine in between.
    Spans16,
    /// Exact perspective at every pixel: not id; the port's opt-in extra
    /// ([`RenderOptions::exact_perspective`](super::RenderOptions::exact_perspective)).
    Exact,
}

/// `reciprocal_table_16` (d_varsa.s): `1/n` for `n = 2..=15` in 1.31 fixed
/// point, `floor(2^31 / n)`. `D_DrawSpans16`'s last segment steps by
/// `(snext - s) * 2` times this, keeping the high word: `floor(ds * R / 2^31)`.
const RECIPROCAL_16: [i64; 16] = [
    0, 0, 0x4000_0000, 0x2aaa_aaaa, 0x2000_0000, 0x1999_9999, 0x1555_5555, 0x1249_2492,
    0x1000_0000, 0x0e38_e38e, 0x0ccc_cccc, 0x0ba2_e8ba, 0x0aaa_aaaa, 0x09d8_9d89, 0x0924_9249,
    0x0888_8888,
];

/// A surface-cache block's fixed-point frame for [`span16_cached`]: `sadjust`/
/// `tadjust` (the eye's block coordinate, 16.16) and `bbextents`/`bbextentt`
/// (`(extent << 16) - 1`: the last position inside the surface).
pub(super) struct BlockFixed {
    sadjust: i64,
    tadjust: i64,
    bbextents: i64,
    bbextentt: i64,
}

impl BlockFixed {
    /// The frame of a `bw x bh` block whose texel `(0, 0)` is the surface's
    /// `texmins` (at the block's mip level, as `grads`): the span routines'
    /// 16.16 texel arithmetic, `z = 0x10000 / zi`, then `s = (int)(sdivz * z) +
    /// sadjust` — the eye-relative part truncated toward zero, the eye's block
    /// coordinate `sadjust` rounded — and the texel `s >> 16`. In f64, so an
    /// exact texel is the perspective one up to id's own 1/65536 steps.
    /// `D_CalcGradients`' `bbextents = ((extents << 16) >> miplevel) - 1`: the
    /// block is `extents >> miplevel` texels a side (`face_surf_block`).
    pub(super) fn new(grads: &PolyGrads, texmins: [f32; 2], bw: usize, bh: usize) -> BlockFixed {
        let st_eye = grads.st_eye;
        BlockFixed {
            sadjust: ((st_eye[0] - texmins[0] as f64) * 65536.0 + 0.5).floor() as i64,
            tadjust: ((st_eye[1] - texmins[1] as f64) * 65536.0 + 0.5).floor() as i64,
            bbextents: ((bw as i64) << 16) - 1,
            bbextentt: ((bh as i64) << 16) - 1,
        }
    }
}

/// `D_DrawSpans16` (d_draw16.s) over one of id's spans of a surface-cache
/// block (`crow`, its pixels): the texel coordinates are exact at the first
/// pixel, then at the end of every full 16-pixel segment, and in between
/// stepped by `(snext - s) / 16` exactly (the asm carries the step's 20
/// fractional bits: pixel `k` reads `(16*s + k*(snext - s)) >> 20`); the last
/// segment of `n + 1` pixels lands on the span's last pixel, stepped by
/// `reciprocal_table_16` (`floor((snext - s) * R[n] / 2^31)`, 16.16), one pixel
/// alone at the previous segment's end. The first position is clamped to
/// `[0, bbextents]`, every later one to `[4096, bbextents]` (the asm's low
/// clamp, 1/16 texel). Every position is then inside the surface — a full
/// segment's lie between its clamped ends, and the last segment's floor-biased
/// steps undershoot its end by at most `n` < 4096 — so each texel is in the
/// block without a per-pixel clamp. The 16-pixel grid starts at the span's
/// first pixel, so it restarts wherever the surface comes out from behind a
/// nearer one (`R_ScanEdges` cuts its spans there).
fn span16_cached(
    crow: &mut [[u8; 3]],
    sp: &Span,
    fx: &BlockFixed,
    block: &[u8],
    bw: usize,
    palette: &[[u8; 3]; 256],
) {
    let end = crow.len();
    let (s0, t0) = sp.st_at(0, fx.sadjust, fx.tadjust);
    let (mut s, mut t) = (s0.clamp(0, fx.bbextents), t0.clamp(0, fx.bbextentt));
    let mut k0 = 0;
    while k0 < end {
        let left = end - k0;
        // The positions this segment steps through: `16*s + i*ds` with 20
        // fractional bits (a full segment), or `s + i*ds` with 16.
        let (n, shift, mut sa, mut ta, ds, dt, next);
        if left > 16 {
            // A full segment: exact again at pixel k0 + 16.
            let (sn, tn) = sp.st_at(k0 + 16, fx.sadjust, fx.tadjust);
            let (sn, tn) = (sn.max(4096).min(fx.bbextents), tn.max(4096).min(fx.bbextentt));
            (n, shift, sa, ta, ds, dt, next) = (16, 20, s * 16, t * 16, sn - s, tn - t, (sn, tn));
        } else {
            // The last segment: `left - 1` steps land on the span's last pixel.
            let steps = left - 1;
            let (mut ss, mut ts) = (0i64, 0i64);
            if steps > 0 {
                let (sn, tn) = sp.st_at(k0 + steps, fx.sadjust, fx.tadjust);
                let (dss, dts) = (sn.max(4096).min(fx.bbextents) - s, tn.max(4096).min(fx.bbextentt) - t);
                (ss, ts) = if steps == 1 {
                    (dss, dts)
                } else {
                    ((dss * RECIPROCAL_16[steps]) >> 31, (dts * RECIPROCAL_16[steps]) >> 31)
                };
            }
            (n, shift, sa, ta, ds, dt, next) = (left, 16, s, t, ss, ts, (s, t));
        }
        for c in &mut crow[k0..k0 + n] {
            let texel = block.get((ta >> shift) as usize * bw + (sa >> shift) as usize);
            *c = palette[texel.copied().unwrap_or(0) as usize];
            sa += ds;
            ta += dt;
        }
        (s, t) = next;
        k0 += n;
    }
}

/// Below this |2 x area| (in square pixels) even the best vertex triple of a
/// polygon is degenerate: an edge-on sliver that covers (next to) no pixel
/// centre, whose gradients would be noise ([`PolyGrads::from_vertices`]).
#[cfg(test)]
const MIN_TRIPLE_AREA2: f64 = 1e-6;

/// A liquid's `sadjust`/`tadjust`: the eye's 16.16 coordinates in the frame
/// `Mod_LoadFaces` gives turbulent surfaces (`texturemins` -8192).
fn turb_adjust(grads: &PolyGrads) -> (i64, i64) {
    let st_eye = grads.st_eye;
    (
        ((st_eye[0] + 8192.0) * 65536.0 + 0.5).floor() as i64,
        ((st_eye[1] + 8192.0) * 65536.0 + 0.5).floor() as i64,
    )
}

/// `Turbulent8` (d_scan.c; C in the x86 build too) over one of id's spans of a
/// liquid (`crow`, its pixels): the 16.16 coordinates exact at the span's first
/// pixel (clamped to `[0, bbextents]`) and at each 16-pixel segment's end
/// (clamped to `[16, bbextents]`), stepped by `(snext - s) >> 4` in between;
/// the last segment ends on the span's last pixel, stepped by the C division
/// `(snext - s) / (spancount - 1)`. Each segment's start is masked to
/// `(CYCLE << 16) - 1` and `D_DrawTurbulent8Span` warps every pixel
/// ([`TurbTable::texel`]). The face's frame is `Mod_LoadFaces`' for turbulent
/// surfaces (`texturemins` -8192, `extents` 16384). The raw texel, no colormap.
#[allow(clippy::too_many_arguments)]
#[inline]
fn turb16_span(
    crow: &mut [[u8; 3]],
    sp: &Span,
    sadjust: i64,
    tadjust: i64,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
    turb: &TurbTable,
    phase: usize,
) {
    const BBEXTENTS: i64 = (16384 << 16) - 1;
    let (tw_i, th_i) = (tw as i32, th as i32);
    let end = crow.len();
    let (s0, t0) = sp.st_at(0, sadjust, tadjust);
    let (mut s, mut t) = (s0.clamp(0, BBEXTENTS), t0.clamp(0, BBEXTENTS));
    let mut k0 = 0;
    while k0 < end {
        let n = (end - k0).min(16);
        let (sn, tn, ss, ts);
        if k0 + n < end {
            let (a, b) = sp.st_at(k0 + 16, sadjust, tadjust);
            (sn, tn) = (a.clamp(16, BBEXTENTS), b.clamp(16, BBEXTENTS));
            (ss, ts) = ((sn - s) >> 4, (tn - t) >> 4);
        } else {
            let (a, b) = sp.st_at(k0 + n - 1, sadjust, tadjust);
            (sn, tn) = (a.clamp(16, BBEXTENTS), b.clamp(16, BBEXTENTS));
            (ss, ts) = if n > 1 { ((sn - s) / (n as i64 - 1), (tn - t) / (n as i64 - 1)) } else { (0, 0) };
        }
        // In the C's `int`s from here: the masked start and the steps.
        let (mut a, mut b) = ((s as i32) & TURB_COORD_MASK, (t as i32) & TURB_COORD_MASK);
        let (ss, ts) = (ss as i32, ts as i32);
        for c in &mut crow[k0..k0 + n] {
            let (sturb, tturb) = turb.texel(phase, a, b);
            let texel = pixels.get(tturb.rem_euclid(th_i) as usize * tw + sturb.rem_euclid(tw_i) as usize);
            *c = palette[texel.copied().unwrap_or(0) as usize];
            a = a.wrapping_add(ss);
            b = b.wrapping_add(ts);
        }
        (s, t) = (sn, tn);
        k0 += n;
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

/// `(*d_drawspans)` on a surface-cache block: `D_DrawSpans16` (the default) or
/// the port's exact-perspective extra (the texel of the exact perspective at
/// every pixel), over one span.
#[allow(clippy::too_many_arguments)]
pub(super) fn span_cached(
    crow: &mut [[u8; 3]],
    sp: &Span,
    fx: &BlockFixed,
    block: &[u8],
    bw: usize,
    bh: usize,
    palette: &[[u8; 3]; 256],
    persp: Persp,
) {
    if persp == Persp::Spans16 {
        span16_cached(crow, sp, fx, block, bw, palette);
        return;
    }
    let (bw_i, bh_i) = (bw as i64, bh as i64);
    let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
    for c in crow.iter_mut() {
        // No z test: a non-positive `zi` (rounding at a clipped edge) saturates
        // and the clamp keeps the read in the block.
        let z = 65536.0 / zi;
        let bx = (((sz * z) as i64).wrapping_add(fx.sadjust) >> 16).clamp(0, bw_i - 1) as usize;
        let by = (((tz * z) as i64).wrapping_add(fx.tadjust) >> 16).clamp(0, bh_i - 1) as usize;
        *c = palette[block[by * bw + bx] as usize];
        zi += sp.dzi;
        sz += sp.dsz;
        tz += sp.dtz;
    }
}

/// `Turbulent8` ([`turb16_span`]) or, as the extra, the warp at the exact
/// perspective texel of every pixel, over one span of a liquid surface.
#[allow(clippy::too_many_arguments)]
pub(super) fn span_turb(
    crow: &mut [[u8; 3]],
    sp: &Span,
    grads: &PolyGrads,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
    turb: &TurbTable,
    time: f32,
    persp: Persp,
) {
    if tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    if persp == Persp::Spans16 {
        let (sadjust, tadjust) = turb_adjust(grads);
        turb16_span(crow, sp, sadjust, tadjust, pixels, tw, th, palette, turb, turb_phase(time));
        return;
    }
    let st_eye = grads.st_eye;
    let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
    for c in crow.iter_mut() {
        let z = 1.0 / zi;
        let (s2, t2) = warp_st(turb, (sz * z + st_eye[0]) as f32, (tz * z + st_eye[1]) as f32, time);
        let tx = s2.rem_euclid(tw as i32) as usize;
        let ty = t2.rem_euclid(th as i32) as usize;
        *c = palette[pixels.get(ty * tw + tx).copied().unwrap_or(0) as usize];
        zi += sp.dzi;
        sz += sp.dsz;
        tz += sp.dtz;
    }
}

/// A wall with no surface-cache block (no colormap, no lightmap, or over the
/// block size cap — never in id's maps), per pixel: the texel at the exact
/// perspective, lit by the lightmap's factor (or `shade`) through the colormap
/// row, or without a colormap the linear `palette[texel] * brightness`.
#[allow(clippy::too_many_arguments)]
pub(super) fn span_tex(
    crow: &mut [[u8; 3]],
    sp: &Span,
    grads: &PolyGrads,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
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
        let texel = pixels.get(ty * tw + tx).copied().unwrap_or(0) as usize;
        let brightness = match lightmap {
            Some(lm) => lm.factor_at(s, t),
            None => shade,
        };
        *c = match colormap {
            Some(cm) => palette[cm[colormap_row(brightness) * 256 + texel] as usize],
            None => {
                let rgb = palette[texel];
                [
                    (rgb[0] as f32 * brightness).clamp(0.0, 255.0) as u8,
                    (rgb[1] as f32 * brightness).clamp(0.0, 255.0) as u8,
                    (rgb[2] as f32 * brightness).clamp(0.0, 255.0) as u8,
                ]
            }
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
    fn spans(w: usize, h: usize, x0: usize, g: &PolyGrads, mut f: impl FnMut(&mut [[u8; 3]], &Span)) -> Image {
        let mut img = Image::new(w, h, [0, 0, 0]);
        for y in 0..h {
            let sp = span_at(g, x0, y);
            f(&mut img.rgb[y * w + x0..(y + 1) * w], &sp);
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
        assert!(render(Some(&cm)).rgb.iter().all(|p| p[0] == want_index), "colormap path: palette index {want_index}");
        // Without the colormap: the legacy linear multiply, exactly palette[200].
        assert!(render(None).rgb.iter().all(|&p| p == [TEXEL; 3]), "None path is palette[texel]*brightness");
        assert_ne!(want_index, TEXEL, "test ramp should remap the index at row 31");
    }

    /// Liquids write the RAW texel (`D_DrawTurbulent8Span`: no colormap at all)
    /// — neither row 0 (the old overbright) nor any other row.
    #[test]
    fn turb_writes_the_raw_texel() {
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }
        const TEXEL: u8 = 77;
        // 64x64 so the Turb warp's index wrap is well-defined; fill with TEXEL.
        let pixels = vec![TEXEL; 64 * 64];
        let turb = TurbTable::new();
        let (w, h) = (40usize, 16usize);
        let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        let g = PolyGrads::from_vertices(&[v0, v1, v2]).expect("triangle");
        for persp in [Persp::Spans16, Persp::Exact] {
            let img = spans(w, h, 0, &g, |row, sp| span_turb(row, sp, &g, &pixels, 64, 64, &pal, &turb, 0.0, persp));
            assert!(img.rgb.iter().all(|&p| p == [TEXEL; 3]), "turb stores the raw texel ({persp:?})");
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
    fn cached_quad(persp: Persp, zl: f32, zr: f32, start: usize) -> Vec<u8> {
        let (w, h) = (40usize, 8usize);
        let (bw, bh) = (256usize, 256usize);
        let block: Vec<u8> = (0..bw * bh).map(|i| ((i % bw) + 3 * (i / bw)) as u8).collect();
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, 0, 0];
        }
        let v = |x: f32, y: f32, vz: f32, s: f32, t: f32| AttrVert { x, y, vz, s, t };
        let quad = [
            v(0.0, 0.0, zl, 20.3, 30.3),
            v(w as f32, 0.0, zr, 180.3, 30.3),
            v(w as f32, h as f32, zr, 180.3, 40.3),
            v(0.0, h as f32, zl, 20.3, 40.3),
        ];
        let g = PolyGrads::from_vertices(&quad).expect("quad");
        let fx = BlockFixed::new(&g, [0.0, 0.0], bw, bh);
        let img = spans(w, h, start, &g, |row, sp| span_cached(row, sp, &fx, &block, bw, bh, &pal, persp));
        img.rgb.iter().map(|p| p[0]).collect()
    }

    #[test]
    fn spans16_is_exact_where_one_over_z_is_constant() {
        // A surface parallel to the screen: s/z and 1/z are both linear, so the
        // affine segments land on the exact texels everywhere.
        assert_eq!(cached_quad(Persp::Spans16, 2.0, 2.0, 0), cached_quad(Persp::Exact, 2.0, 2.0, 0));
    }

    #[test]
    fn spans16_is_exact_at_each_segment_start_of_a_span() {
        // An oblique wall (1/z from 1 to 1/8 across 40 px): D_DrawSpans16
        // divides at the span's first pixel and every 16 pixels after it (0, 16,
        // 32: the last segment is 32..40), stepping affinely between — so it
        // agrees with exact perspective there and not everywhere else.
        let w = 40usize;
        let exact = cached_quad(Persp::Exact, 1.0, 8.0, 0);
        let s16 = cached_quad(Persp::Spans16, 1.0, 8.0, 0);
        for y in 0..8 {
            for x in [0usize, 16, 32] {
                assert_eq!(s16[y * w + x], exact[y * w + x], "row {y} pixel {x}");
            }
        }
        assert!(s16.iter().zip(&exact).filter(|(a, b)| a != b).count() > 40, "segments are affine");
        // The same wall behind a nearer surface over its first 5 columns: id's
        // span (R_ScanEdges) starts at the first visible pixel, so the grid
        // restarts there: exact at 5, 21 and 37.
        let hidden = cached_quad(Persp::Spans16, 1.0, 8.0, 5);
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

    #[test]
    fn a_span_at_zero_1_over_z_wraps_like_the_c_int() {
        // zi exactly 0 at a near-clipped edge: z = 0x10000 / 0 is infinite,
        // `(sdivz * z) as i64` saturates, and `+ sadjust` overflowed (a panic
        // in a debug build). It wraps as the C's `s = (int)(sdivz * z) +
        // sadjust` does, and the span routines clamp into the surface.
        let sp = Span { zi: 0.0, sz: 1.0, tz: -1.0, dzi: 0.0, dsz: 0.0, dtz: 0.0 };
        assert_eq!(sp.st_at(0, 5, -5), (i64::MAX.wrapping_add(5), i64::MIN.wrapping_add(-5)));
        let fx = BlockFixed { sadjust: 5, tadjust: -5, bbextents: (4 << 16) - 1, bbextentt: (4 << 16) - 1 };
        let block = [7u8; 16];
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [1, 2, 3];
        for persp in [Persp::Spans16, Persp::Exact] {
            let mut row = [[9u8; 3]; 20];
            span_cached(&mut row, &sp, &fx, &block, 4, 4, &pal, persp);
            assert!(row.iter().all(|&p| p == [1, 2, 3]), "{persp:?}: every pixel reads the block");
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
            let ti = crate::bsp::TexInfo {
                vecs: [[1.0, 0.0, 0.0, 5.5], [0.0, 0.7, 0.7, -3.0]],
                miptex: 0,
                flags: 0,
            };
            let (u, _) = normalize(cross(n, [0.0, 0.0, 1.0]));
            let v = cross(n, u);
            for origin in [[0.0f32; 3], [64.0, -32.0, 8.0]] {
                // The model's frame: the plane and texinfo are local; the eye too.
                let eye = [cam.pos[0] - origin[0], cam.pos[1] - origin[1], cam.pos[2] - origin[2]];
                let g = PolyGrads::for_plane(&view, eye, n, dist, Some(&ti)).expect("eye off the plane");
                for (a, b) in [(0.0f32, 0.0f32), (150.0, 20.0), (-80.0, 90.0), (300.0, -120.0)] {
                    let p = [n[0] * dist + a * u[0] + b * v[0], n[1] * dist + a * u[1] + b * v[1], n[2] * dist + a * u[2] + b * v[2]];
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
}
