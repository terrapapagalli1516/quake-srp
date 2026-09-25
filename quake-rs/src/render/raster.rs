//! The rasterisers that fill brush-model faces.
//!
//! The port's own design, standing where id's edge/span pipeline is (`r_edge.c`,
//! `d_edge.c`'s `D_CalcGradients`, `d_scan.c`'s `D_DrawSpans8`): each clipped
//! face is scan-converted as one convex polygon, row by row, into spans — flat,
//! textured (perspective-correct, lightmapped, turb and sky modes) or
//! surface-cached — sharing one z-buffer. The flat bounding-box triangle
//! ([`raster_triangle`]) remains for the untextured debug renderer.

use super::Image;
use crate::math::Vec3;
use super::light::{colormap_row, LightMap, COLORMAP_LEN};
use super::sky::{sky_texel_view, SkySpans, SkyView};
use super::stats::stat;
use super::warp::{warp_st, TurbTable};

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

/// How the per-pixel `(s,t)` -> texel step of [`raster_poly_tex`] behaves.
///
/// `Normal` is the existing wall path (optional lightmap). `Turb` and `Sky`
/// drive the animated special-surface sampling above; both are unlit (Quake never
/// lightmaps liquids or sky): they ignore the `lightmap`/`shade` inputs and store
/// the raw texel, with no colormap row, as `D_DrawTurbulent8Span`/`D_DrawSkyScans8`.
#[derive(Clone, Copy)]
pub(super) enum SurfaceMode<'a> {
    /// Ordinary wall: sample `pixels` at the interpolated `(s,t)`.
    Normal,
    /// Liquid: SIN-warp `(s,t)` by `time` before sampling. Unlit.
    Turb { turb: &'a TurbTable, time: f32 },
    /// Sky: project the per-pixel VIEW DIRECTION onto the scrolling sky dome
    /// (`D_Sky_uv_To_st`) rather than mapping wall `(s,t)`. Unlit. With `defer`
    /// (the world pass) the pixel only takes the depth and is recorded for
    /// [`resolve_sky_spans`](super::sky::resolve_sky_spans) under the face key; without, it is sampled exactly.
    Sky { view: SkyView, defer: Option<(&'a std::cell::RefCell<SkySpans>, u32)> },
}

/// A projected polygon vertex: screen `x`/`y`, where pixel `(px, py)`'s centre
/// is `(px + 0.5, py + 0.5)`. The rasterisers take only the OUTLINE from the
/// polygon; `1/z`, `s/z` and `t/z` come from the face plane ([`PolyGrads`]).
#[derive(Clone, Copy)]
pub(super) struct ProjT {
    pub(super) x: f32,
    pub(super) y: f32,
}

/// A synthetic polygon vertex with its depth and texel coordinates, for tests
/// that have no face plane: see [`PolyGrads::from_vertices`] and [`outline`].
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) struct AttrVert {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) vz: f32,
    pub(super) s: f32,
    pub(super) t: f32,
}

/// The screen outline of synthetic vertices.
#[cfg(test)]
pub(super) fn outline(verts: &[AttrVert]) -> Vec<ProjT> {
    verts.iter().map(|v| ProjT { x: v.x, y: v.y }).collect()
}

// ---------------------------------------------------------------------------
// The polygon span walker (world, submodel and external brush faces)
// ---------------------------------------------------------------------------
//
// Each clipped face is scan-converted ONCE, as the convex polygon it is, row by
// row — the shape of id's `R_ScanEdges`/`D_DrawSpans8` pipeline, minus the edge
// sorting (the port keeps its z-buffer for visibility, so faces are still drawn
// independently, front to back).
//
// FILL RULE (one rule, for every brush pass): a pixel belongs to a polygon iff
// its CENTRE `(px + 0.5, py + 0.5)` lies inside it, half-open on the right and
// bottom — `x_left <= cx < x_right` on a row, `y_top <= cy < y_bottom` for an
// edge to cross that row. This is id's rule: `R_EmitEdge` takes rows
// `ceil(v0) ..= ceil(v1) - 1` and `R_GenerateSpans` columns `ceil(u_left) ..
// ceil(u_right)`, with id's pixel centres on the integers (`xcenter = w/2 -
// 0.5`). Two faces that share an edge therefore split the pixels on it with no
// crack and no double draw — PROVIDED both compute the same crossing: every
// edge's crossing is computed from its TOP endpoint to its BOTTOM one (by y),
// so the two faces, which walk a shared edge in opposite directions, get the
// same bits (and `clip_poly_near_into` makes a near-clipped shared edge's new
// vertex the same in both faces, for the same reason).

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
/// `vup`, `vpn`), the screen centre and the focal length, as the brush passes
/// project a vertex: `x = cx + focal*vx/vz`, `y = cy - focal*vy/vz`.
#[derive(Clone, Copy)]
pub(super) struct ScreenProj {
    pub(super) forward: Vec3,
    pub(super) right: Vec3,
    pub(super) up: Vec3,
    pub(super) cx: f32,
    pub(super) cy: f32,
    pub(super) focal: f32,
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
        let inv_focal = 1.0 / view.focal as f64;
        // A view-space vector `p` gives the screen plane `p . (x', y', 1)` with
        // `x' = (x - cx)/focal`, `y' = (cy - y)/focal` (the ray through (x, y)).
        let plane = |p: [f64; 3], scale: f64| {
            let dx = p[0] * inv_focal * scale;
            let dy = -p[1] * inv_focal * scale;
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

/// One span of a polygon: pixels `x0..x1` of row `y`, with `1/z`, `s/z` and
/// `t/z` (eye-relative) at the centre of pixel `x0` and their per-pixel steps —
/// `D_DrawSpans8`'s `zi`/`sdivz`/`tdivz` and `d_zistepu`/`d_sdivzstepu`/
/// `d_tdivzstepu`. The span loops step each with one add per pixel, in f64 (the
/// same cost as f32 in wasm, and no drift worth a texel across 1280 pixels);
/// the start is evaluated from the planes per span. This is where
/// `D_DrawSpans16`'s 16-pixel subdivision would go (divide at the segment ends,
/// step `s`/`t` affinely between).
#[derive(Clone, Copy)]
struct Span {
    y: usize,
    x0: usize,
    x1: usize,
    zi: f64,
    sz: f64,
    tz: f64,
    dzi: f64,
    dsz: f64,
    dtz: f64,
}

/// Below this |2 x area| (in square pixels) even the best vertex triple of a
/// polygon is degenerate: an edge-on sliver that covers (next to) no pixel
/// centre, whose gradients would be noise ([`PolyGrads::from_vertices`]).
#[cfg(test)]
const MIN_TRIPLE_AREA2: f64 = 1e-6;

/// Scan-convert the convex screen polygon `poly` (either winding) under the fill
/// rule above, clipped to the `w`x`h` screen, calling `f` once per non-empty
/// span with the accumulators from `grads`.
///
/// Each row's `[x_left, x_right)` is the min/max crossing of the edges that
/// straddle the row's centre line; a convex polygon has exactly two (a top
/// vertex row gives an empty span, horizontal edges never cross). The polygon
/// is the clipped face `draw_world_textured` built, so this is `O(rows x edges)`
/// setup plus the pixels themselves — no bounding box, no inside test.
fn scan_poly(poly: &[ProjT], w: usize, h: usize, grads: &PolyGrads, mut f: impl FnMut(Span)) {
    let n = poly.len();
    if n < 3 || w == 0 || h == 0 {
        return;
    }
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for v in poly {
        if !(v.x.is_finite() && v.y.is_finite()) {
            return;
        }
        ymin = ymin.min(v.y as f64);
        ymax = ymax.max(v.y as f64);
    }
    // Rows whose centre `py + 0.5` is in [ymin, ymax).
    let py0 = (ymin - 0.5).ceil().max(0.0) as usize;
    let py1 = ((ymax - 0.5).ceil().min(h as f64)).max(0.0) as usize;
    let (dzi, dsz, dtz) = (grads.zi.dx, grads.sz.dx, grads.tz.dx);
    for py in py0..py1 {
        let cy = py as f64 + 0.5;
        let mut xl = f64::INFINITY;
        let mut xr = f64::NEG_INFINITY;
        for i in 0..n {
            let (p, q) = (&poly[i], &poly[if i + 1 == n { 0 } else { i + 1 }]);
            // Canonical direction: top (smaller y) to bottom; horizontal: skip.
            let (top, bot) = if p.y < q.y {
                (p, q)
            } else if q.y < p.y {
                (q, p)
            } else {
                continue;
            };
            let (ty, by) = (top.y as f64, bot.y as f64);
            if cy < ty || cy >= by {
                continue;
            }
            let slope = (bot.x as f64 - top.x as f64) / (by - ty);
            let x = top.x as f64 + (cy - ty) * slope;
            xl = xl.min(x);
            xr = xr.max(x);
        }
        // No crossing leaves (+inf, -inf); a top vertex row, a single point.
        if xl >= xr {
            continue;
        }
        // Columns whose centre `px + 0.5` is in [xl, xr).
        let x0 = (xl - 0.5).ceil().max(0.0) as usize;
        let x1 = ((xr - 0.5).ceil().min(w as f64)).max(0.0) as usize;
        if x0 >= x1 {
            continue;
        }
        let cx = x0 as f64 + 0.5;
        f(Span {
            y: py,
            x0,
            x1,
            zi: grads.zi.at(cx, cy),
            sz: grads.sz.at(cx, cy),
            tz: grads.tz.at(cx, cy),
            dzi,
            dsz,
            dtz,
        });
    }
}

/// A flat-coloured polygon (textureless faces): the span walker with the
/// z test and write only.
pub(super) fn raster_poly_flat(
    image: &mut Image,
    zbuf: &mut [f32],
    poly: &[ProjT],
    grads: &PolyGrads,
    color: [u8; 3],
) {
    let (w, h) = (image.w, image.h);
    if zbuf.len() < w * h || image.rgb.len() < w * h {
        return;
    }
    scan_poly(poly, w, h, grads, |sp| {
        let row = sp.y * w;
        let zrow = &mut zbuf[row + sp.x0..row + sp.x1];
        let crow = &mut image.rgb[row + sp.x0..row + sp.x1];
        let mut zi = sp.zi;
        for (zc, c) in zrow.iter_mut().zip(crow.iter_mut()) {
            if zi > 0.0 {
                let depth = (1.0 / zi) as f32;
                if depth < *zc {
                    *zc = depth;
                    *c = color;
                }
            }
            zi += sp.dzi;
        }
    });
}

/// Perspective-correct textured polygon, the per-pixel path: turb, sky, walls
/// without a baked surface block (no colormap, or no lightmap). `pixels` is
/// `tw * th` palette indices. Every pixel pays the texture sample, the lightmap
/// factor and the colormap row (see [`raster_poly_cached`] for the cached walls).
#[allow(clippy::too_many_arguments)]
pub(super) fn raster_poly_tex(
    image: &mut Image,
    zbuf: &mut [f32],
    poly: &[ProjT],
    grads: &PolyGrads,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
    shade: f32,
    lightmap: Option<&LightMap>,
    mode: SurfaceMode,
    colormap: Option<&[u8]>,
) {
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 || tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    if zbuf.len() < w * h || image.rgb.len() < w * h {
        return;
    }
    // Only use a correctly-sized colormap; a malformed one falls back to the
    // linear multiply (never reads out of bounds).
    let colormap = colormap.filter(|cm| cm.len() >= COLORMAP_LEN);
    let st_eye = grads.st_eye;
    // A deferred sky face records its pixels instead of drawing them (one
    // borrow per polygon).
    let mut sky_defer = match mode {
        SurfaceMode::Sky { defer: Some((cell, key)), .. } => Some((cell.borrow_mut(), key)),
        _ => None,
    };
    scan_poly(poly, w, h, grads, |sp| {
        let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
        for px in sp.x0..sp.x1 {
            // A labelled block so the early-outs still reach the accumulator
            // step below.
            'pixel: {
                // 1/z is positive inside a polygon in front of the eye; guard
                // the rounding at a near-clipped edge.
                if zi <= 0.0 {
                    break 'pixel;
                }
                let z = 1.0 / zi;
                let depth = z as f32;
                let idx = sp.y * w + px;
                let zc = &mut zbuf[idx];
                if depth >= *zc {
                    break 'pixel;
                }
                // The perspective divide, then the eye's (s,t) added back
                // (id's sadjust/tadjust).
                let s = (sz * z + st_eye[0]) as f32;
                let t = (tz * z + st_eye[1]) as f32;

                // Resolve the palette index and per-pixel brightness per surface
                // mode. Liquids/sky are fullbright (brightness 1.0, no lightmap);
                // walls keep the lightmap-or-`shade` brightness.
                let (texel, brightness) = match mode {
                    SurfaceMode::Normal => {
                        let tx = (s as i64).rem_euclid(tw as i64) as usize;
                        let ty = (t as i64).rem_euclid(th as i64) as usize;
                        let p = match pixels.get(ty * tw + tx) {
                            Some(&p) => p as usize,
                            None => break 'pixel,
                        };
                        // A baked lightmap (indexed by the same surface (s,t),
                        // which shares the texinfo axes) replaces the flat
                        // Lambert `shade`.
                        let b = match lightmap {
                            Some(lm) => lm.factor_at(s, t),
                            None => shade,
                        };
                        (p, b)
                    }
                    SurfaceMode::Turb { turb, time } => {
                        // SIN-warp the (s,t) before the (tiling) wrap; unlit.
                        let (s2, t2) = warp_st(turb, s, t, time);
                        let tx = s2.rem_euclid(tw as i32) as usize;
                        let ty = t2.rem_euclid(th as i32) as usize;
                        let p = match pixels.get(ty * tw + tx) {
                            Some(&p) => p as usize,
                            None => break 'pixel,
                        };
                        (p, 1.0)
                    }
                    SurfaceMode::Sky { view, .. } => {
                        if let Some((spans, key)) = sky_defer.as_mut() {
                            // Drawn later, span by span (`resolve_sky_spans`).
                            *zc = depth;
                            spans.record(idx, sp.y, *key, depth);
                            break 'pixel;
                        }
                        // Project the pixel's VIEW DIRECTION onto the scrolling
                        // sky dome (`D_Sky_uv_To_st`) — the sky does not use wall
                        // (s,t). id passes the integer pixel `(u,v)`; unlit.
                        (sky_texel_view(pixels, tw, px as i32, sp.y as i32, &view) as usize, 1.0)
                    }
                };
                *zc = depth;
                let p = &mut image.rgb[idx];
                match colormap {
                    // Quake's exact software shading: pick a colormap ROW from
                    // the brightness, then index the colormap to get a PALETTE
                    // INDEX, which is finally looked up in the palette. This is
                    // an INDEX lookup (no RGB multiply) and so can never
                    // overbright past the base colour. Liquids and sky take NO
                    // colormap at all: `D_DrawTurbulent8Span` and
                    // `D_DrawSkyScans8` store the raw texel (`*pdest =
                    // *(pbase + ...)`, `r_skysource[...]`) — the identity, which
                    // sits around row 31/32, not the brightest row 0.
                    Some(cm) => {
                        let pal_index = match mode {
                            // row < COLORMAP_ROWS and texel < 256, so this index
                            // is < COLORMAP_LEN <= cm.len() (checked above).
                            SurfaceMode::Normal => cm[colormap_row(brightness) * 256 + texel] as usize,
                            SurfaceMode::Turb { .. } | SurfaceMode::Sky { .. } => texel,
                        };
                        *p = palette[pal_index];
                    }
                    // Fallback: the original linear `palette[texel] * brightness`
                    // (byte-for-byte unchanged when no colormap is supplied).
                    None => {
                        let rgb = palette[texel];
                        *p = [
                            (rgb[0] as f32 * brightness).clamp(0.0, 255.0) as u8,
                            (rgb[1] as f32 * brightness).clamp(0.0, 255.0) as u8,
                            (rgb[2] as f32 * brightness).clamp(0.0, 255.0) as u8,
                        ];
                    }
                }
            } // 'pixel
            zi += sp.dzi;
            sz += sp.dsz;
            tz += sp.dtz;
        }
    });
}

/// A wall whose lit+colormapped surface block is already baked (see
/// [`face_surf_block`](super::surf::face_surf_block)) — `D_DrawSpans8` over a
/// cached surface. The inner pixel is ONE block read (texture, lightmap and
/// colormap are folded into the block) plus a palette lookup; the z test and
/// write stay. This is the warm-frame hot path for walls.
#[allow(clippy::too_many_arguments)]
pub(super) fn raster_poly_cached(
    image: &mut Image,
    zbuf: &mut [f32],
    poly: &[ProjT],
    grads: &PolyGrads,
    block: &[u8],
    bw: usize,
    bh: usize,
    texmins: [f32; 2],
    palette: &[[u8; 3]; 256],
) {
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 || bw == 0 || bh == 0 || block.len() < bw.saturating_mul(bh) {
        return;
    }
    if zbuf.len() < w * h || image.rgb.len() < w * h {
        return;
    }
    let st_eye = grads.st_eye;
    let (bw_i, bh_i) = (bw as i64, bh as i64);
    // `D_DrawSpans8`'s 16.16 texel arithmetic: `z = 0x10000 / zi`, then
    // `s = (int)(sdivz * z) + sadjust` — the eye-relative part truncated toward
    // zero, the eye's block coordinate `sadjust` rounded — and the texel
    // `s >> 16`. In f64, so the texel is the exact perspective one up to id's
    // own 1/65536 steps. (The z-buffer's `z * 2^-16` is a power-of-two scale:
    // bit-identical to `1.0 / zi`, which the other span loops store.)
    let sadjust = ((st_eye[0] - texmins[0] as f64) * 65536.0 + 0.5).floor() as i64;
    let tadjust = ((st_eye[1] - texmins[1] as f64) * 65536.0 + 0.5).floor() as i64;
    // Local written-pixel tally (overdraw metric), folded into the profiler ONCE
    // at the end so the hot loop never touches a thread-local.
    let mut drawn = 0u64;
    scan_poly(poly, w, h, grads, |sp| {
        let row = sp.y * w;
        // Per-span row slices: in bounds by the span's screen clip, so the loop
        // indexes them without per-pixel bounds checks.
        let zrow = &mut zbuf[row + sp.x0..row + sp.x1];
        let crow = &mut image.rgb[row + sp.x0..row + sp.x1];
        let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
        for (zc, c) in zrow.iter_mut().zip(crow.iter_mut()) {
            if zi > 0.0 {
                let z = 65536.0 / zi;
                let depth = (z * (1.0 / 65536.0)) as f32;
                if depth < *zc {
                    // Nearest surface texel within the block extent (the block is
                    // 1:1 with surface texels at mip 0). The clamp keeps a texel
                    // read in the block, as `bbextents` does in id's span loop.
                    let bx = ((((sz * z) as i64) + sadjust) >> 16).clamp(0, bw_i - 1) as usize;
                    let by = ((((tz * z) as i64) + tadjust) >> 16).clamp(0, bh_i - 1) as usize;
                    *zc = depth;
                    *c = palette[block[by * bw + bx] as usize];
                    drawn += 1;
                }
            }
            zi += sp.dzi;
            sz += sp.dsz;
            tz += sp.dtz;
        }
    });
    stat(|s| s.world_pixels += drawn);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::light::COLORMAP_ROWS;

    /// A mid-shade Normal-surface pixel must route through
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

        // Synthetic colormap (64 rows x 256): a known ramp where
        // colormap[row*256 + col] = (col + row) mod 256. Picking row R and
        // col=TEXEL therefore yields palette index (TEXEL + R) mod 256.
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256usize {
                cm[row * 256 + col] = ((col + row) % 256) as u8;
            }
        }

        // A full-framebuffer triangle at constant (s,t)=(0,0) so every covered
        // pixel samples texel 200. No LightMap -> brightness == `shade`.
        let (w, h) = (8usize, 8usize);
        let shade = 1.0f32; // -> colormap_row(1.0) == 31
        let expected_row = colormap_row(shade);

        let render = |colormap: Option<&[u8]>| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zb = vec![f32::INFINITY; w * h];
            let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
            let tri = [v0, v1, v2];
            let g = PolyGrads::from_vertices(&tri).expect("triangle");
            raster_poly_tex(
                &mut img, &mut zb, &outline(&tri), &g,
                &pixels, 1, 1, &pal, shade, None, SurfaceMode::Normal,
                colormap,
            );
            img
        };

        // With the colormap: pixel = palette[colormap[row*256 + 200]] where the
        // ramp gives index (200 + row) mod 256, so red channel == that index.
        let with_cm = render(Some(&cm));
        let drawn: Vec<[u8; 3]> = with_cm.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn.is_empty(), "colormapped triangle drew nothing");
        let want_index = ((TEXEL as usize + expected_row) % 256) as u8;
        for p in &drawn {
            assert_eq!(
                p[0], want_index,
                "colormap path must yield palette index {want_index} (row {expected_row}, texel {TEXEL})"
            );
        }

        // Without the colormap: the legacy linear multiply. shade==1.0 so the
        // pixel is exactly palette[200] = (200,200,200) (no darkening).
        let without_cm = render(None);
        let drawn2: Vec<[u8; 3]> = without_cm.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn2.is_empty(), "fallback triangle drew nothing");
        for p in &drawn2 {
            assert_eq!(*p, [TEXEL, TEXEL, TEXEL], "None path must be the linear palette[texel]*brightness");
        }

        // Sanity: the two paths actually differ (the colormap is doing work).
        assert_ne!(want_index, TEXEL, "test ramp should remap the index at row 31");
    }

    /// Liquids write the RAW texel (`D_DrawTurbulent8Span`: no colormap at all),
    /// even with a colormap supplied and a neutral `brightness` — neither row 0
    /// (the old overbright) nor any other row.
    #[test]
    fn colormap_turb_writes_the_raw_texel() {
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }
        const TEXEL: u8 = 77;
        // 64x64 so the Turb warp's index wrap is well-defined; fill with TEXEL.
        let pixels = vec![TEXEL; 64 * 64];

        // Colormap ramp: colormap[row*256+col] = (col + row + 1) mod 256 — NO row
        // is the identity, so any colormap use would move the index off TEXEL.
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256usize {
                cm[row * 256 + col] = ((col + row + 1) % 256) as u8;
            }
        }

        let turb = TurbTable::new();
        let (w, h) = (16usize, 16usize);
        let mut img = Image::new(w, h, [0, 0, 0]);
        let mut zb = vec![f32::INFINITY; w * h];
        let v0 = AttrVert { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = AttrVert { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = AttrVert { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        let tri = [v0, v1, v2];
        let g = PolyGrads::from_vertices(&tri).expect("triangle");
        raster_poly_tex(
            &mut img, &mut zb, &outline(&tri), &g,
            &pixels, 64, 64, &pal, 1.0, None,
            SurfaceMode::Turb { turb: &turb, time: 0.0 },
            Some(&cm),
        );
        let drawn: Vec<[u8; 3]> = img.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn.is_empty(), "turb triangle drew nothing");
        for p in &drawn {
            assert_eq!(*p, [TEXEL, TEXEL, TEXEL], "turb must store the raw texel, no colormap");
        }
    }

    // -- The polygon span walker's fill rule --------------------------------

    /// Coverage mask of `poly` under the span walker's fill rule: 1 where the
    /// polygon draws (fresh z-buffer, so nothing is z-rejected).
    fn coverage(poly: &[AttrVert], w: usize, h: usize) -> Vec<u8> {
        let mut img = Image::new(w, h, [0, 0, 0]);
        let mut zb = vec![f32::INFINITY; w * h];
        if let Some(g) = PolyGrads::from_vertices(poly) {
            raster_poly_flat(&mut img, &mut zb, &outline(poly), &g, [255, 255, 255]);
        }
        img.rgb.iter().map(|p| (p[0] == 255) as u8).collect()
    }

    fn pv(x: f32, y: f32, vz: f32) -> AttrVert {
        AttrVert { x, y, vz, s: 0.0, t: 0.0 }
    }

    /// A tiny deterministic generator (no deps): coordinates with fractions.
    struct Lcg(u64);
    impl Lcg {
        fn f(&mut self, lo: f32, hi: f32) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            lo + (hi - lo) * ((self.0 >> 40) as f32 / (1u64 << 24) as f32)
        }
    }

    /// No crack, no double draw: a convex polygon cut into a fan of pieces
    /// around an interior point — every shared edge walked in opposite
    /// directions by its two pieces — is covered exactly once per pixel, and
    /// exactly where the whole polygon is. Random vertices, including ones off
    /// screen, many trials.
    #[test]
    fn shared_edges_split_pixels_exactly_once() {
        let (w, h) = (48usize, 40usize);
        let mut rng = Lcg(0x5eed);
        for trial in 0..300 {
            // A convex polygon: points on an ellipse at sorted random angles.
            let n = 3 + (trial % 6);
            let (cx, cy) = (rng.f(-8.0, 56.0), rng.f(-8.0, 48.0));
            let (rx, ry) = (rng.f(2.0, 40.0), rng.f(2.0, 40.0));
            let mut angles: Vec<f32> = (0..n).map(|_| rng.f(0.0, std::f32::consts::TAU)).collect();
            angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // A plane in view space: 1/z linear over the screen, positive here.
            // (|x|, |y| < 100 here, so 1/z >= 0.03 - 0.02 > 0: in front of the eye.)
            let (za, zb, zc) = (rng.f(0.03, 0.2), rng.f(-1e-4, 1e-4), rng.f(-1e-4, 1e-4));
            let on_plane = |x: f32, y: f32| pv(x, y, 1.0 / (za + zb * x + zc * y));
            let poly: Vec<AttrVert> =
                angles.iter().map(|a| on_plane(cx + rx * a.cos(), cy + ry * a.sin())).collect();
            let whole = coverage(&poly, w, h);
            // Fan pieces around the centroid.
            let (mut sx, mut sy) = (0.0, 0.0);
            for v in &poly {
                sx += v.x / n as f32;
                sy += v.y / n as f32;
            }
            let c = on_plane(sx, sy);
            let mut sum = vec![0u8; w * h];
            for i in 0..n {
                let piece = [c, poly[i], poly[(i + 1) % n]];
                for (s, p) in sum.iter_mut().zip(coverage(&piece, w, h)) {
                    *s += p;
                }
            }
            for (k, (&s, &wh)) in sum.iter().zip(&whole).enumerate() {
                assert!(s <= 1, "trial {trial}: pixel {k} drawn {s} times (double draw)");
                // A sliver piece may be dropped by the determinant guard only
                // if it covers no pixel centre; so the pieces tile the whole.
                assert_eq!(s, wh, "trial {trial}: pixel {k} pieces {s} vs whole {wh} (crack)");
            }
        }
    }

    /// Pixel centres exactly on an edge go to one side: the left and top edges
    /// are inclusive, the right and bottom exclusive (id's `R_EmitEdge` /
    /// `R_GenerateSpans` rule).
    #[test]
    fn fill_rule_is_top_left_half_open() {
        let (w, h) = (8usize, 8usize);
        // A square whose edges run exactly through pixel centres: x in [2.5, 5.5),
        // y in [1.5, 4.5) -> columns 2..=4, rows 1..=3.
        let sq = [pv(2.5, 1.5, 1.0), pv(5.5, 1.5, 1.0), pv(5.5, 4.5, 1.0), pv(2.5, 4.5, 1.0)];
        let cov = coverage(&sq, w, h);
        for y in 0..h {
            for x in 0..w {
                let want = (2..=4).contains(&x) && (1..=3).contains(&y);
                assert_eq!(cov[y * w + x] == 1, want, "pixel ({x},{y})");
            }
        }
        // The same square in the other winding covers the same pixels.
        let rev: Vec<AttrVert> = sq.iter().rev().copied().collect();
        assert_eq!(coverage(&rev, w, h), cov);
    }

    /// The degenerate guard: a zero-area polygon draws nothing and a
    /// non-finite vertex is rejected without panicking.
    #[test]
    fn degenerate_polygons_draw_nothing() {
        let (w, h) = (8usize, 8usize);
        let line = [pv(0.0, 0.0, 1.0), pv(4.0, 4.0, 1.0), pv(8.0, 8.0, 1.0)];
        assert!(coverage(&line, w, h).iter().all(|&c| c == 0));
        let bad = [pv(0.0, 0.0, 1.0), pv(f32::NAN, 4.0, 1.0), pv(8.0, 0.0, 1.0)];
        assert!(coverage(&bad, w, h).iter().all(|&c| c == 0));
        let inf = [pv(0.0, 0.0, 1.0), pv(f32::INFINITY, 4.0, 1.0), pv(8.0, 0.0, 1.0)];
        assert!(coverage(&inf, w, h).iter().all(|&c| c == 0));
    }

    /// The gradients reproduce the vertices' perspective attributes: a
    /// polygon's s/t at a pixel centre match the exact perspective-correct
    /// value, whichever vertex triple the solve picked.
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
        scan_poly(&outline(&poly), 32, 32, &g, |sp| {
            let (mut zi, mut sz, mut tz) = (sp.zi, sp.sz, sp.tz);
            for px in sp.x0..sp.x1 {
                // The view ray through this pixel centre hits the plane through
                // the four points; recover (vx, vy, vz) from 1/z exactly.
                let z = 1.0 / zi;
                let focal = focal as f64;
                let vx = (px as f64 + 0.5 - 16.0) * z / focal;
                let vy = (16.0 - (sp.y as f64 + 0.5)) * z / focal;
                assert!((sz * z - (10.0 * vx + 3.0)).abs() < 1e-4, "s at ({px},{})", sp.y);
                assert!((tz * z - (7.0 * vy - 1.0)).abs() < 1e-4, "t at ({px},{})", sp.y);
                checked += 1;
                zi += sp.dzi;
                sz += sp.dsz;
                tz += sp.dtz;
            }
        });
        assert!(checked > 20, "the quad covers pixels");
    }

    /// `PolyGrads::for_plane` (the analytic `D_CalcGradients`) reproduces, at
    /// the projection of points on the plane, their exact 1/z and texinfo (s,t)
    /// — for a world face and for a brush-entity face in its local frame.
    #[test]
    fn plane_gradients_match_projected_points() {
        use crate::math::{cross, dot, normalize};
        let cam = crate::render::Camera::looking_at([10.0, -20.0, 30.0], [200.0, 50.0, -10.0], 90.0);
        let (forward, right, up) = cam.basis();
        let (cx, cy, focal) = (160.0f32, 100.0f32, 160.0f32);
        let view = ScreenProj { forward, right, up, cx, cy, focal };
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
                let (x, y) = ((cx + focal * vx / vz) as f64, (cy - focal * vy / vz) as f64);
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
