//! The triangle rasterisers that fill brush-model faces.
//!
//! The port's own design, standing where id's edge/span pipeline is (`r_edge.c`,
//! `d_scan.c`'s `D_DrawSpans8`): flat, textured (perspective-correct, lightmapped,
//! turb and sky modes) and surface-cached triangles sharing one z-buffer.

use super::Image;
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
pub(super) fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
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

/// How the per-pixel `(s,t)` -> texel step of [`raster_triangle_tex`] behaves.
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
    /// [`resolve_sky_spans`] under the face key; without, it is sampled exactly.
    Sky { view: SkyView, defer: Option<(&'a std::cell::RefCell<SkySpans>, u32)> },
}

/// A projected vertex carrying texture coordinates for perspective-correct
/// sampling. `vz` is forward depth (used linearly for the z-buffer, to stay
/// consistent with the flat path); `s`/`t` are Quake surface texel coordinates
/// (they also index the face lightmap, which shares the texinfo axes).
#[derive(Clone, Copy)]
pub(super) struct ProjT {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) vz: f32,
    pub(super) s: f32,
    pub(super) t: f32,
}

/// Perspective-correct textured triangle. `pixels` is `tw * th` palette indices.
/// The z-buffer uses linearly-interpolated `vz` (matching [`raster_triangle`]),
/// while `s`/`t` are interpolated with perspective correction (`s/z`, `1/z`).
#[allow(clippy::too_many_arguments)]
pub(super) fn raster_triangle_tex(
    image: &mut Image,
    zbuf: &mut [f32],
    v0: ProjT,
    v1: ProjT,
    v2: ProjT,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
    shade: f32,
    lightmap: Option<&LightMap>,
    mode: SurfaceMode,
    colormap: Option<&[u8]>,
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 || tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    // Only use a correctly-sized colormap; a malformed one falls back to the
    // linear multiply (never reads out of bounds).
    let colormap = colormap.filter(|cm| cm.len() >= COLORMAP_LEN);
    let min_xf = v0.x.min(v1.x).min(v2.x);
    let max_xf = v0.x.max(v1.x).max(v2.x);
    let min_yf = v0.y.min(v1.y).min(v2.y);
    let max_yf = v0.y.max(v1.y).max(v2.y);
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
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;
    let (iz0, iz1, iz2) = (1.0 / v0.vz, 1.0 / v1.vz, 1.0 / v2.vz);
    let (soz0, soz1, soz2) = (v0.s * iz0, v1.s * iz1, v2.s * iz2);
    let (toz0, toz1, toz2) = (v0.t * iz0, v1.t * iz1, v2.t * iz2);
    // The barycentric weights are LINEAR in the pixel position, so step them
    // incrementally (3 adds/pixel) instead of three full `edge()` cross-products
    // per pixel — the classic span-rasteriser speedup Quake's D_DrawSpans used.
    // `w_i` is recomputed exactly via `edge()` at each ROW START (so drift never
    // accumulates across rows), then advanced by `dw_i_dx` across the row. The
    // accumulation differs from a per-pixel recompute by at most a few ULPs over a
    // row, which can only flip the inside-test on a sub-pixel sliver at a triangle
    // edge — visually identical.
    let dw0dx = -(v2.y - v1.y) * inv_area;
    let dw1dx = -(v0.y - v2.y) * inv_area;
    let dw2dx = -(v1.y - v0.y) * inv_area;
    // A deferred sky face records its pixels instead of drawing them (one
    // borrow per triangle).
    let mut sky_defer = match mode {
        SurfaceMode::Sky { defer: Some((cell, key)), .. } => Some((cell.borrow_mut(), key)),
        _ => None,
    };

    for py in min_y..=max_y {
        let sy = py as f32 + 0.5;
        let sx0 = min_x as f32 + 0.5;
        let mut w0 = edge(v1.x, v1.y, v2.x, v2.y, sx0, sy) * inv_area;
        let mut w1 = edge(v2.x, v2.y, v0.x, v0.y, sx0, sy) * inv_area;
        let mut w2 = edge(v0.x, v0.y, v1.x, v1.y, sx0, sy) * inv_area;
        for px in min_x..=max_x {
            // A labelled block so the early-outs can `break 'pixel` to the per-pixel
            // weight step below (a `continue` would skip the increment and desync).
            'pixel: {
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                break 'pixel;
            }
            // Perspective-correct depth (1/z interpolation, then invert), matching
            // Quake's `zi`-keyed z-buffer and the flat path. `inv_z` is reused for
            // the s/t perspective divide below, so this costs nothing extra.
            let inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
            if inv_z <= 0.0 {
                break 'pixel;
            }
            let depth = 1.0 / inv_z;
            let idx = (py as usize) * w + (px as usize);
            let zc = match zbuf.get_mut(idx) {
                Some(z) => z,
                None => break 'pixel,
            };
            if depth >= *zc {
                break 'pixel;
            }
            // Perspective divide reuses `depth` (= 1/inv_z) as a multiply instead of
            // two more reciprocals — the affine numerators times 1/z. (Differs from
            // `/inv_z` by at most a ULP, which never crosses a texel boundary.)
            let s = (w0 * soz0 + w1 * soz1 + w2 * soz2) * depth;
            let t = (w0 * toz0 + w1 * toz1 + w2 * toz2) * depth;

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
                    // A baked lightmap (indexed by the same surface (s,t), which
                    // shares the texinfo axes) replaces the flat Lambert `shade`.
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
                        spans.record(idx, py as usize, *key, depth);
                        break 'pixel;
                    }
                    // Project the pixel's VIEW DIRECTION onto the scrolling sky
                    // dome (`D_Sky_uv_To_st`) — the sky does not use wall (s,t).
                    // id passes the integer pixel `(u,v)`; unlit.
                    (sky_texel_view(pixels, tw, px as i32, py as i32, &view) as usize, 1.0)
                }
            };
            *zc = depth;
            if let Some(p) = image.rgb.get_mut(idx) {
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
            }
            } // 'pixel
            // Step the barycentric weights one pixel across the row (always, even on
            // an early-out, so the running values stay in sync with `px`).
            w0 += dw0dx;
            w1 += dw1dx;
            w2 += dw2dx;
        }
    }
}

/// Fast rasteriser for a wall whose lit+colormapped surface block is already baked
/// (see [`face_surf_block`]) — Quake's `D_DrawSpans` over a cached surface. Same
/// perspective-correct projection, incremental-edge stepping, near-clip handling
/// and z-test as [`raster_triangle_tex`], but the inner pixel is ONE block read
/// (the texture, lightmap and colormap are already folded into the block) plus a
/// palette lookup, instead of a texture sample + bilinear lightmap + colormap row
/// + colormap index per pixel. This is the warm-frame hot path for static walls.
#[allow(clippy::too_many_arguments)]
pub(super) fn raster_triangle_cached(
    image: &mut Image,
    zbuf: &mut [f32],
    v0: ProjT,
    v1: ProjT,
    v2: ProjT,
    block: &[u8],
    bw: usize,
    bh: usize,
    texmins: [f32; 2],
    palette: &[[u8; 3]; 256],
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 || bw == 0 || bh == 0 || block.len() < bw.saturating_mul(bh) {
        return;
    }
    let min_xf = v0.x.min(v1.x).min(v2.x);
    let max_xf = v0.x.max(v1.x).max(v2.x);
    let min_yf = v0.y.min(v1.y).min(v2.y);
    let max_yf = v0.y.max(v1.y).max(v2.y);
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
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;
    let (iz0, iz1, iz2) = (1.0 / v0.vz, 1.0 / v1.vz, 1.0 / v2.vz);
    let (soz0, soz1, soz2) = (v0.s * iz0, v1.s * iz1, v2.s * iz2);
    let (toz0, toz1, toz2) = (v0.t * iz0, v1.t * iz1, v2.t * iz2);
    let dw0dx = -(v2.y - v1.y) * inv_area;
    let dw1dx = -(v0.y - v2.y) * inv_area;
    let dw2dx = -(v1.y - v0.y) * inv_area;
    // Per-pixel-x derivatives of the perspective accumulators. `inv_z`, `s/z` and
    // `t/z` are each EXACTLY linear in screen x (they are linear combinations of the
    // barycentric weights, which themselves step by `dw*dx` per pixel), so they can
    // be advanced with a single add per pixel instead of re-dotting the three weights
    // every pixel — removing ~9 multiplies/pixel. We still step w0/w1/w2 for the
    // edge inside-test. (Additive accumulation differs from the per-pixel re-dot by
    // a few ULPs across a span — the same negligible drift class as the incremental
    // edge stepping; verified to leave the world render essentially unchanged.)
    let dinvz = iz0 * dw0dx + iz1 * dw1dx + iz2 * dw2dx;
    let dsoz = soz0 * dw0dx + soz1 * dw1dx + soz2 * dw2dx;
    let dtoz = toz0 * dw0dx + toz1 * dw1dx + toz2 * dw2dx;
    let (bw_i, bh_i) = (bw as i64, bh as i64);

    // Local written-pixel tally (overdraw metric), folded into the profiler ONCE at
    // the end so the hot loop never touches a thread-local.
    let mut drawn = 0u64;
    // min_x/max_x are clamped to 0..w and min_y/max_y to 0..h above, so every row's
    // [xa, xb] slice of the framebuffer and z-buffer is provably in bounds. Taking a
    // per-row &mut slice and indexing it with the LOCAL offset `px - xa` lets the
    // compiler drop the per-pixel bounds checks the old `get_mut(idx)` paid on every
    // covered pixel — the span-oriented access Quake's D_DrawSpans used. The texel
    // read still clamps (block extent is independent of the screen rect). Output is
    // identical: same pixels, same values, same z-writes.
    let xa = min_x as usize;
    let xb = max_x as usize;
    let span = xb - xa + 1;
    for py in min_y..=max_y {
        let sy = py as f32 + 0.5;
        let sx0 = xa as f32 + 0.5;
        let mut w0 = edge(v1.x, v1.y, v2.x, v2.y, sx0, sy) * inv_area;
        let mut w1 = edge(v2.x, v2.y, v0.x, v0.y, sx0, sy) * inv_area;
        let mut w2 = edge(v0.x, v0.y, v1.x, v1.y, sx0, sy) * inv_area;
        // Row-start perspective accumulators (exact dot at the first pixel of the
        // row; stepped by the derivatives after each pixel).
        let mut inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
        let mut soz = w0 * soz0 + w1 * soz1 + w2 * soz2;
        let mut toz = w0 * toz0 + w1 * toz1 + w2 * toz2;
        let row = (py as usize) * w;
        let zrow = &mut zbuf[row + xa..row + xa + span];
        let crow = &mut image.rgb[row + xa..row + xa + span];
        for k in 0..span {
            'pixel: {
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    break 'pixel;
                }
                if inv_z <= 0.0 {
                    break 'pixel;
                }
                let depth = 1.0 / inv_z;
                let zc = &mut zrow[k];
                if depth >= *zc {
                    break 'pixel;
                }
                let s = soz * depth;
                let t = toz * depth;
                // Nearest surface texel within the block extent (the block is 1:1
                // with surface texels at mip 0).
                let bx = ((s - texmins[0]) as i64).clamp(0, bw_i - 1) as usize;
                let by = ((t - texmins[1]) as i64).clamp(0, bh_i - 1) as usize;
                let pal_idx = block[by * bw + bx] as usize;
                *zc = depth;
                crow[k] = palette[pal_idx];
                drawn += 1;
            }
            w0 += dw0dx;
            w1 += dw1dx;
            w2 += dw2dx;
            inv_z += dinvz;
            soz += dsoz;
            toz += dtoz;
        }
    }
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
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
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
        let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        raster_triangle_tex(
            &mut img, &mut zb, v0, v1, v2,
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
}
