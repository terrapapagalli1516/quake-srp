//! Lighting: lightmaps, light styles, dynamic lights, and the colormap row.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_surf.c` — `R_BuildLightMap`, `R_AddDynamicLights`;
//! `WinQuake/r_light.c` — `R_MarkLights`, `R_LightPoint`; `WinQuake/model.c` —
//! `CalcSurfaceExtents`.

use crate::bsp::Bsp;
use crate::math::{dot, Vec3};
use super::surf::face_world_poly;
use super::torch::FaceTorches;

// ---------------------------------------------------------------------------
// BSP lightmaps (Quake's baked static lighting from the LIGHTING lump)
// ---------------------------------------------------------------------------

/// The luxel store behind a [`LightMap`]: either the face's static one-byte
/// luxels borrowed straight from `Bsp::lighting` (the common case, with no
/// dynamic light touching the face), or an owned `f32` grid that is the static
/// luxels plus the per-luxel dynamic-light contributions (`R_AddDynamicLights`).
///
/// Keeping a borrowed variant means a face with no dynamic light reads the exact
/// same bytes the pre-dlight code did, so its rendered output is byte-identical:
/// passing an empty dlight slice changes nothing.
#[cfg_attr(test, derive(Clone))]
pub(super) enum Luxels<'a> {
    /// Borrowed static luxels, one byte each (`0..=255`).
    Static(&'a [u8]),
    /// Owned augmented luxels (`static + dynamic`), already in `0..=255`-luxel
    /// units but stored as `f32` so overbright (dynamic) values exceed 255.
    Owned(Vec<f32>),
}

impl Luxels<'_> {
    /// The luxel value at flat index `idx`, or `255.0` (fullbright) for any
    /// out-of-range index — so a malformed lightmap never reads garbage / panics.
    #[inline]
    fn at(&self, idx: usize) -> f32 {
        match self {
            Luxels::Static(s) => s.get(idx).copied().unwrap_or(255) as f32,
            Luxels::Owned(v) => v.get(idx).copied().unwrap_or(255.0),
        }
    }
}

/// A face's baked lightmap, borrowed from `Bsp::lighting` for the common case of
/// a single steady style-0 face, or an owned `f32` grid that is the multi-style
/// combine (`R_BuildLightMap`) plus any dynamic-light contributions.
///
/// `luxels` is a `lmw * lmh` grid (one value per luxel). `texmins` is the surface
/// texture-coordinate origin (in texels) used to convert a face's surface `(s,t)`
/// into luxel coordinates. The lightmap shares the face's `texinfo.vecs` with the
/// wall texture, so the same surface `(s,t)` the rasteriser already interpolates
/// indexes both.
#[cfg_attr(test, derive(Clone))]
pub(super) struct LightMap<'a> {
    pub(super) luxels: Luxels<'a>,
    pub(super) lmw: usize,
    pub(super) lmh: usize,
    pub(super) texmins: [f32; 2],
}

/// Upper bound on the lightmap brightness factor. Quake's software renderer
/// clamped `blocklights` (the static+dynamic luxel sum, in 8.8 units) to a max
/// before the final shift; here we clamp the *factor* instead. The static path
/// tops out at `(255/255)*2 = 2.0`; dynamic lights add on top, so we allow up to
/// `4.0` (the rough WinQuake overbright ceiling of ~`4*255` luxels) and clamp
/// there, keeping a near light bright without letting a huge `(rad-dist)` blow
/// out to NaN/Inf or wrap a palette index.
const MAX_LIGHT_FACTOR: f32 = 4.0;

/// Number of animated light styles (`MAX_LIGHTSTYLES`), matching
/// [`crate::server::MAX_LIGHTSTYLES`]. The renderer takes a `[f32; LIGHTSTYLES]`
/// per-style brightness scale (1.0 == normal) so it can combine a face's
/// multiple lightmap layers (`R_BuildLightMap`).
pub const LIGHTSTYLES: usize = 64;

/// A no-op light-style scale table: every style at the "normal" `1.0`. As a
/// scene's [`light_styles`](super::Scene::light_styles) (the
/// [`Scene::new`](super::Scene::new) default) it leaves lightmaps exactly as
/// the static (style-0) renderer produced them. The animated front-ends
/// instead pass `server.lightstyle_scales(time, lerp)` (stepped as id's, or
/// gliding: `r_lerplightstyles`).
pub const NEUTRAL_LIGHTSTYLE_SCALES: [f32; LIGHTSTYLES] = [1.0; LIGHTSTYLES];

/// `DFace.styles` slot value meaning "this lightmap layer is unused".
pub(super) const STYLE_NONE: u8 = 255;

impl LightMap<'_> {
    /// Bilinearly sample the lightmap at surface texture coordinate `(s, t)`,
    /// returning a brightness factor (1.0 == neutral, up to ~2.0 overbright for
    /// the static lightmap, up to [`MAX_LIGHT_FACTOR`] with dynamic lights).
    ///
    /// Luxel indices are clamped into `[0, lm-1]`. Any index that somehow falls
    /// outside the luxel store is treated as the fullbright value 255, so a
    /// malformed lightmap never reads garbage and never panics.
    pub(super) fn factor_at(&self, s: f32, t: f32) -> f32 {
        // lmw/lmh are >= 1 by construction in `face_lightmap`.
        let max_x = self.lmw.saturating_sub(1) as f32;
        let max_y = self.lmh.saturating_sub(1) as f32;
        let lxf = ((s - self.texmins[0]) / 16.0).clamp(0.0, max_x);
        let lyf = ((t - self.texmins[1]) / 16.0).clamp(0.0, max_y);

        let x0 = lxf.floor() as usize;
        let y0 = lyf.floor() as usize;
        let x1 = (x0 + 1).min(self.lmw.saturating_sub(1));
        let y1 = (y0 + 1).min(self.lmh.saturating_sub(1));
        let fx = lxf - x0 as f32;
        let fy = lyf - y0 as f32;

        let at = |x: usize, y: usize| -> f32 { self.luxels.at(y * self.lmw + x) };

        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bot = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        let light = top * (1.0 - fy) + bot * fy;

        // The static byte path is byte-identical to before; the clamp only ever
        // bites when dynamic lights push a luxel above ~510.
        ((light / 255.0) * 2.0).min(MAX_LIGHT_FACTOR)
    }

    /// `R_BuildLightMap`'s `blocklights`, after its "bound, invert, and shift":
    /// one value per luxel (`lmw * lmh`, row-major) in `out`, each
    /// `(255*256 - bl) >> (8 - VID_CBITS)` clamped to `>= 1 << 6`, where `bl` is
    /// the C's 8.8 sum — every style's luxel times its `d_lightstylevalue`, plus
    /// `R_AddDynamicLights`' truncated `(rad - dist)*256`. The row of the colormap
    /// a texel takes is `light >> 8` of the value interpolated between these
    /// ([`draw_surface_block`](super::surf::draw_surface_block)).
    ///
    /// The luxels here are that same sum over 256 (`d_lightstylevalue / 256` is a
    /// style's scale, and a dynamic light adds a whole 8.8 step over 256), each
    /// term exact in `f32`, so `luxel * 256` is the C's integer `bl`. A luxel the
    /// store does not have reads 255, as [`Luxels::at`] does.
    pub(super) fn blocklights_into(&self, out: &mut Vec<i32>) {
        let n = self.lmw * self.lmh;
        out.clear();
        out.extend((0..n).map(|i| {
            let bl = (self.luxels.at(i) as f64 * 256.0).round();
            // `bl` is at most a few million, so the i64 is exact; a non-finite
            // luxel (never produced) saturates and lands on a clamp.
            let t = (65280 - bl as i64) >> (8 - VID_CBITS);
            t.clamp(1 << 6, 65280 >> (8 - VID_CBITS)) as i32
        }));
    }
}

/// `VID_CBITS`: the colormap has `1 << VID_CBITS` light rows.
const VID_CBITS: u32 = 6;

/// Quake's `gfx/colormap.lmp` is `COLORMAP_ROWS * 256` bytes: `COLORMAP_ROWS`
/// successive light rows of 256 palette indices each. Row 0 is the brightest
/// (the base palette colour), the last row is the darkest. `VID_CBITS == 6`, so
/// `VID_GRADES == 64` rows.
pub(super) const COLORMAP_ROWS: usize = 64; // 1 << VID_CBITS, VID_CBITS == 6
/// A correctly-sized colormap is exactly this many bytes.
pub(super) const COLORMAP_LEN: usize = COLORMAP_ROWS * 256;

/// Map a per-pixel lightmap `brightness` factor (as produced by
/// [`LightMap::factor_at`]: `1.0` neutral, `2.0` the static fullbright ceiling,
/// up to [`MAX_LIGHT_FACTOR`] with dynamic lights) to a Quake colormap ROW in
/// `0..COLORMAP_ROWS`.
///
/// C basis — `R_BuildLightMap` (`r_surf.c`) then the `D_DrawSurfaceBlock8` inner
/// loop (`r_surf.c` / `d_scan.c`):
///
/// ```c
/// // r_surf.c, R_BuildLightMap, "bound, invert, and shift":
/// t = (255*256 - (int)blocklights[i]) >> (8 - VID_CBITS);   // VID_CBITS == 6
/// if (t < (1 << 6)) t = (1 << 6);                            // clamp t >= 64
/// blocklights[i] = t;
/// // d_scan.c inner loop:
/// prowdest[b] = ((unsigned char *)vid.colormap)[(light & 0xFF00) + pix];
/// //                                              ^ row = light >> 8
/// ```
///
/// `blocklights[i]` is the combined light in 8.8 units. For the canonical static
/// single-style surface Quake scales the luxel by `d_lightstylevalue == 256`
/// (1.0 in 8.8), so `blocklights == luxel * 256`. This renderer's `brightness`
/// is `(luxel/255)*2`, hence `luxel == brightness*127.5` and
/// `blocklights == brightness * 32640` (`= 127.5 * 256`). Substituting into the
/// C: `t = (65280 - brightness*32640) >> 2`, clamped to `>= 64`, and the
/// colormap row is `t >> 8`. brightness `2.0` (and anything brighter — dynamic
/// lights) collapses to row 0 via the `t >= 64` clamp, reproducing Quake's
/// no-overbright ceiling; brightness `0.0` maps to the darkest row.
#[inline]
pub(super) fn colormap_row(brightness: f32) -> usize {
    // blocklights in 8.8 units; round to match the integer `(int)blocklights`.
    let bl = (brightness * 32640.0 + 0.5).floor() as i32;
    // C: (255*256 - blocklights) >> (8 - VID_CBITS) == ... >> 2
    let mut t = (65280 - bl) >> 2;
    // C: if (t < (1<<6)) t = (1<<6);  -- the no-overbright clamp.
    if t < (1 << 6) {
        t = 1 << 6;
    }
    // C: light & 0xFF00, i.e. row = t >> 8. Clamp the row into the table; a
    // fully dark luxel (bl == 0) gives t == 16320 -> row 63, already the last
    // row, but guard against any future widening.
    ((t >> 8) as usize).min(COLORMAP_ROWS - 1)
}

/// A `surf->dlightbits` mask with every slot set: "marked by every light".
/// Used where the `R_MarkLights` recursion does not apply — direct unit calls
/// on a bare face, and the no-node-tree fallback in [`mark_dlights`] — so the
/// per-light distance test in [`add_dynamic_lights`] is the only gate (the
/// pre-gating behaviour).
pub(super) const ALL_DLIGHT_BITS: u32 = u32::MAX;

/// `R_AddDynamicLights` (`r_surf.c`): fold the dynamic lights in `dlights` that
/// touch a face into an owned augmented luxel buffer.
///
/// `dlightbits` is the face's `surf->dlightbits` mask from the `R_MarkLights`
/// BSP recursion ([`mark_dlights`]): bit `i` set means `dlights[i]` reached this
/// face through the node tree. A light whose bit is clear is skipped exactly as
/// the C `if (!(surf->dlightbits & (1<<lnum))) continue;` — so a light cannot
/// brighten a face the BSP says it never reaches (e.g. through a wall). Callers
/// outside the marked render passes (unit tests on a bare face) pass
/// [`ALL_DLIGHT_BITS`] to apply the pure distance test.
///
/// `base` is the face's pre-combined luxel buffer when it already differs from
/// the plain static style-0 bytes — i.e. the multi-style combine from
/// [`build_styled_luxels`] (animated light styles). When `base` is `Some`, that
/// buffer is the starting point and any reaching dynamic light adds onto it, so
/// the result is `Some` even if no light reaches (the animated combine must still
/// be used). When `base` is `None`, the buffer is lazily materialised from
/// `static_samples` and the function returns `None` if no light reaches — so a
/// steady single-style face with an empty dlight slice keeps borrowing the static
/// bytes (byte-identical to before).
///
/// For each light: `dist = dot(origin, plane.normal) - plane.dist` (the RAW
/// plane, as the C uses `surf->plane->normal` directly — `dist.abs()` covers
/// both sides), `rad = radius - dist.abs()`. Skip if `rad < minlight`. Project
/// the light onto the surface plane (`impact = origin - normal*dist`), map it to
/// luxel space through the same `texinfo.vecs`/`texmins` the static lightmap
/// uses, and for each luxel add `rad - dist2` where
/// `dist2 = max(sd,td) + (min(sd,td) >> 1)` is Quake's cheap distance estimate,
/// in the C's integers: `sd`/`td` are the luxel offsets truncated to `int`.
///
/// The add is in `0..255` luxel units: the C adds `(rad-dist)*256` to 8.8
/// `blocklights`, truncated to an integer, so here `trunc((rad-dist)*256)/256`
/// (exact in `f32`). The `factor_at` clamp bounds the result.
#[allow(clippy::too_many_arguments)]
fn add_dynamic_lights(
    bsp: &Bsp,
    face: &crate::bsp::DFace,
    ti: &crate::bsp::TexInfo,
    texmins: [f32; 2],
    lmw: usize,
    lmh: usize,
    static_samples: &[u8],
    base: Option<Vec<f32>>,
    dlights: &[crate::dlight::DynamicLight],
    dlightbits: u32,
) -> Option<Vec<f32>> {
    // No dlights — or none marked for this face by the BSP recursion: the
    // animated combine (if any) is the final buffer; otherwise there is nothing
    // to do and the caller keeps the static borrow.
    if dlights.is_empty() || dlightbits == 0 {
        return base;
    }
    // No usable plane: we can't project lights, but a pre-combined animated
    // buffer must still be returned so styles still animate.
    let plane = match (face.planenum as i64)
        .try_into()
        .ok()
        .and_then(|pi: usize| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return base,
    };
    let normal = plane.normal;

    // Start from the pre-combined animated buffer when present; otherwise the
    // buffer is lazily materialised from the static bytes on first contribution.
    let mut buf: Option<Vec<f32>> = base;

    for (lnum, dl) in dlights.iter().enumerate() {
        // C: `if (!(surf->dlightbits & (1<<lnum))) continue;` — only lights the
        // R_MarkLights recursion marked onto this face apply. Lights past bit 31
        // cannot be expressed in the mask (C MAX_DLIGHTS is 32) and are skipped.
        if lnum >= u32::BITS as usize || dlightbits & (1u32 << lnum) == 0 {
            continue;
        }
        let dist = dot(dl.origin, normal) - plane.dist;
        let rad = dl.radius - dist.abs();
        let minlight = dl.minlight;
        if rad < minlight {
            continue; // this light does not reach the face
        }
        let reach = rad - minlight; // C `minlight = rad - minlight`

        let impact = [
            dl.origin[0] - normal[0] * dist,
            dl.origin[1] - normal[1] * dist,
            dl.origin[2] - normal[2] * dist,
        ];

        // Project the impact point into luxel space via the texinfo axes (the
        // same vecs the static lightmap / surface extents use).
        let local0 = impact[0] * ti.vecs[0][0]
            + impact[1] * ti.vecs[0][1]
            + impact[2] * ti.vecs[0][2]
            + ti.vecs[0][3]
            - texmins[0];
        let local1 = impact[0] * ti.vecs[1][0]
            + impact[1] * ti.vecs[1][1]
            + impact[2] * ti.vecs[1][2]
            + ti.vecs[1][3]
            - texmins[1];

        // Lazily materialise the owned buffer from the static bytes the first
        // time a light actually contributes, so a non-reaching light slice still
        // returns None (borrow stays static, output unchanged).
        let buffer = buf.get_or_insert_with(|| {
            let mut v = vec![0.0f32; lmw * lmh];
            for (i, cell) in v.iter_mut().enumerate() {
                *cell = static_samples.get(i).copied().unwrap_or(0) as f32;
            }
            v
        });

        for t in 0..lmh {
            // C: `td = local[1] - t*16;` into an int (truncation), then abs.
            let td = ((local1 - (t * 16) as f32) as i32).abs();
            for s in 0..lmw {
                let sd = ((local0 - (s * 16) as f32) as i32).abs();
                let dist2 = if sd > td { sd + (td >> 1) } else { td + (sd >> 1) } as f32;
                if dist2 < reach {
                    let idx = t * lmw + s;
                    if let Some(cell) = buffer.get_mut(idx) {
                        // C: `blocklights[i] += (rad - dist)*256;` (unsigned).
                        *cell += ((rad - dist2) * 256.0) as u32 as f32 / 256.0;
                    }
                }
            }
        }
    }

    buf
}

/// `R_BuildLightMap` multi-style combine: read every active style block of a
/// face's lightmap and combine them into an owned `lmw*lmh` `f32` luxel buffer,
/// scaling each block by its style's brightness (`light_styles[style]`).
///
/// The LIGHTING lump stores one `lmw*lmh` luxel block PER ACTIVE style slot,
/// CONCATENATED in slot order at `face.lightofs`: the block for `styles[0]`
/// first, then `styles[1]`, etc. A slot value of `255` ([`STYLE_NONE`]) means the
/// layer is unused (no block stored). For each active slot `k` the combine adds
/// `block_k[i] * light_styles[styles[k]]` into luxel `i`.
///
/// Returns:
/// * `None` when the face has a single style-0 layer whose scale is exactly the
///   normal `1.0` — the common steady case. The caller then borrows the static
///   bytes directly so the rendered pixels are BYTE-IDENTICAL to the pre-style
///   renderer (`effective_luxel == block0`).
/// * `Some(buf)` for every other case (a non-neutral scale, or 2+ active styles).
///   `buf` is the combined `f32` grid, the base the dynamic-light step adds onto.
///
/// `light_styles` is the per-style brightness (`1.0` == normal). An out-of-range
/// style index reads as `1.0` (treated as normal), matching the server's
/// "missing style -> normal" rule so a face never goes dark referencing an unset
/// style.
///
/// Bounds: the whole `n_active * block` range (`block == lmw*lmh` luxels) is
/// checked against `lighting`. If it does not fit, returns [`StyleCombine::TooShort`]
/// so the caller falls back to the single-block (style-0) read or fullbright —
/// never reading out of bounds.
fn build_styled_luxels(
    face: &crate::bsp::DFace,
    lighting: &[u8],
    start: usize,
    block: usize,
    light_styles: &[f32; LIGHTSTYLES],
) -> StyleCombine {
    // Active style slots, in stored order. `255` marks an unused slot. The
    // LIGHTING lump stores exactly one block per *leading* active slot, so we
    // STOP at the first `255` (matching the C `R_BuildLightMap` loop). Reading
    // past a 255 would pull adjacent faces' luxels into a phantom block.
    let mut active: [(u8, f32); crate::bsp::MAXLIGHTMAPS] = [(STYLE_NONE, 1.0); crate::bsp::MAXLIGHTMAPS];
    let mut n_active = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        let scale = light_styles.get(style as usize).copied().unwrap_or(1.0);
        active[n_active] = (style, scale);
        n_active += 1;
    }

    // No active style (lightofs >= 0 but styles all unused): nothing to combine;
    // let the caller keep the single static block (its existing behaviour).
    if n_active == 0 {
        return StyleCombine::StaticBlock;
    }

    // The single steady style-0 (or any single style) case at the normal scale is
    // byte-identical to reading the static block, so keep the borrow.
    if n_active == 1 && (active[0].1 - 1.0).abs() < f32::EPSILON {
        return StyleCombine::StaticBlock;
    }

    // The whole concatenated multi-block range must fit; otherwise fall back.
    let total = match block.checked_mul(n_active).and_then(|t| start.checked_add(t)) {
        Some(t) if t <= lighting.len() => t,
        _ => return StyleCombine::TooShort,
    };
    let all = &lighting[start..total];

    let mut buf = vec![0.0f32; block];
    for (k, &(_style, scale)) in active.iter().take(n_active).enumerate() {
        let off = k * block;
        // `off..off+block` is in range by the `total` check above.
        let blk = &all[off..off + block];
        for (i, cell) in buf.iter_mut().enumerate() {
            cell.add_assign_scaled(blk[i] as f32, scale);
        }
    }
    StyleCombine::Combined(buf)
}

/// Outcome of [`build_styled_luxels`].
enum StyleCombine {
    /// Keep the borrowed single static block (steady style-0 at normal scale).
    StaticBlock,
    /// Use this owned combined `f32` buffer as the base.
    Combined(Vec<f32>),
    /// The multi-block range did not fit the lighting slice; fall back to the
    /// single-block read (or fullbright) without panicking.
    TooShort,
}

/// Tiny FMA helper so the combine reads clearly; no `unsafe`, no intrinsics.
trait AddAssignScaled {
    fn add_assign_scaled(&mut self, value: f32, scale: f32);
}
impl AddAssignScaled for f32 {
    #[inline]
    fn add_assign_scaled(&mut self, value: f32, scale: f32) {
        *self += value * scale;
    }
}

/// Compute a face's static lightmap, or `None` if the face is fullbright. A thin
/// wrapper over [`face_lightmap_dyn`] with no dynamic lights and neutral light
/// styles, so its output is the borrowed static byte slice (byte-identical to the
/// pre-dlight behaviour).
///
/// The renderer always calls [`face_lightmap_dyn`] directly (threading its live
/// `dlights` and style scales); this wrapper is retained for the lightmap unit
/// tests, which assert the static-borrow path is unchanged.
#[cfg(test)]
fn face_lightmap<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
) -> Option<LightMap<'a>> {
    face_lightmap_dyn(bsp, face, world_poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
}

/// [`face_lightmap_with`] with no torch flicker: the lightmap unit tests'.
#[cfg(test)]
pub(super) fn face_lightmap_dyn<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    dlightbits: u32,
) -> Option<LightMap<'a>> {
    face_lightmap_with(bsp, face, world_poly, light_styles, FaceTorches::NONE, dlights, dlightbits)
}

/// EXTRA (`r_torchflicker`, [`super::torch`]): the steady torches' change
/// to a face's luxels this frame, added to the style combine `base` — or to
/// the borrowed style-0 bytes `samples`, owned now — before any dynamic
/// light. A face no torch moves this frame keeps `base` as it is, so with the
/// extra off (or no torch near) the lightmap is id's, bit for bit. The
/// torches' light is in the style-0 block, so it is scaled by style 0's
/// value (`scale0`).
fn add_torch_flicker(base: Option<Vec<f32>>, samples: &[u8], torches: FaceTorches, scale0: f32) -> Option<Vec<f32>> {
    if torches.is_still() {
        return base;
    }
    let mut buf = base.unwrap_or_else(|| samples.iter().map(|&b| f32::from(b)).collect());
    torches.add_to(&mut buf, scale0);
    Some(buf)
}

/// Compute a face's lightmap: the multi-style combine (`R_BuildLightMap`) scaled
/// by `light_styles`, the steady torches' flicker (`torches`, the 2026
/// extra), plus any dynamic lights in `dlights` that reach the face
/// (`R_AddDynamicLights`). Returns `None` if the face is fullbright.
///
/// `None` (the caller's fallback shade) when there is no `lighting` lump, the
/// surface is special (sky/liquid — `TEX_SPECIAL`), or the computed luxel grid
/// would not fit in the remaining `lighting` slice. A normal face without samples
/// (`lightofs < 0`) gets an all-zero (black) map plus any reaching dlights.
///
/// `light_styles` is the per-style brightness scale (`1.0` == normal,
/// [`NEUTRAL_LIGHTSTYLE_SCALES`] disables animation). A face's `styles[0..3]`
/// (255 == unused) select which scales apply. For a single steady style-0 face at
/// the neutral scale (and no reaching dynamic light) the returned [`LightMap`]
/// *borrows* the static `Bsp::lighting` bytes, so the sampled factor — and the
/// rendered pixels — are byte-identical to the pre-style renderer. Otherwise the
/// [`LightMap`] owns an `f32` grid: `(sum of style blocks * style scale)` plus any
/// dynamic-light contributions, clamped by `factor_at`.
pub(super) fn face_lightmap_with<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
    light_styles: &[f32; LIGHTSTYLES],
    torches: FaceTorches,
    dlights: &[crate::dlight::DynamicLight],
    // The face's `surf->dlightbits` mask from the `R_MarkLights` BSP recursion
    // (see [`mark_dlights`]); [`ALL_DLIGHT_BITS`] where marking does not apply.
    dlightbits: u32,
) -> Option<LightMap<'a>> {
    use crate::bsp::TEX_SPECIAL;

    if bsp.lighting.is_empty() {
        return None;
    }
    let ti = (face.texinfo as i64)
        .try_into()
        .ok()
        .and_then(|i: usize| bsp.texinfo.get(i))?;
    if ti.flags & TEX_SPECIAL != 0 {
        // Sky / liquid: never lightmapped, never dlit (C `SURF_DRAWTILED`).
        return None;
    }

    // A NORMAL wall with no light samples (`lightofs < 0`: the light tool found
    // nothing reaching it — hundreds of such faces per id map) is BLACK, not
    // fullbright: `R_BuildLightMap` clears the block to the ambient
    // (`r_refdef.ambientlight`, `r_ambient` 0), has no samples to add
    // (`surf->samples` is NULL), adds any dynamic lights (`R_AddDynamicLights`),
    // then inverts — so 0 is colormap row 63. We build that zero base, plus the
    // lights that reach the face.
    if face.lightofs < 0 {
        let (texmins, extent) = surface_extents(ti, world_poly)?;
        let lmw = (extent[0] / 16 + 1) as usize;
        let lmh = (extent[1] / 16 + 1) as usize;
        let count = lmw.checked_mul(lmh)?;
        let texmins_f = [texmins[0] as f32, texmins[1] as f32];
        // An EMPTY `static_samples` and a `None` base: `add_dynamic_lights`
        // materialises the zero ("clear to ambient") buffer only when a light
        // actually reaches this face, else returns `None` — then it is all zero.
        let no_samples: &[u8] = &[];
        let lit = if dlights.is_empty() || dlightbits == 0 {
            None
        } else {
            add_dynamic_lights(bsp, face, ti, texmins_f, lmw, lmh, no_samples, None, dlights, dlightbits)
        };
        let luxels = Luxels::Owned(lit.unwrap_or_else(|| vec![0.0; count]));
        return Some(LightMap { luxels, lmw, lmh, texmins: texmins_f });
    }

    let (texmins, extent) = surface_extents(ti, world_poly)?;

    // Luxel grid dimensions: one luxel per 16-unit block, plus one.
    let lmw = (extent[0] / 16 + 1) as usize;
    let lmh = (extent[1] / 16 + 1) as usize;
    let count = lmw.checked_mul(lmh)?;

    let start: usize = face.lightofs.try_into().ok()?;
    // The style-0 (first) block must always fit; this is the static-borrow slice
    // and the fallback when the multi-style range does not fit.
    let samples = bsp.lighting.get(start..start.checked_add(count)?)?;

    let texmins_f = [texmins[0] as f32, texmins[1] as f32];

    // Combine the active style blocks (R_BuildLightMap). `StaticBlock` means the
    // common steady style-0-at-normal case: keep borrowing the static bytes.
    // `TooShort` means the multi-block range overran the lighting slice: fall back
    // to the single static block rather than going fullbright or panicking.
    let base: Option<Vec<f32>> =
        match build_styled_luxels(face, &bsp.lighting, start, count, light_styles) {
            StyleCombine::StaticBlock | StyleCombine::TooShort => None,
            StyleCombine::Combined(buf) => Some(buf),
        };
    let base = add_torch_flicker(base, samples, torches, light_styles[0]);

    // Add any reaching dynamic lights on top of the (possibly style-combined)
    // base. When `base` is None and no light reaches (or `dlights` is empty), the
    // result is None and we keep the byte-identical static borrow.
    let luxels = match add_dynamic_lights(bsp, face, ti, texmins_f, lmw, lmh, samples, base, dlights, dlightbits) {
        Some(owned) => Luxels::Owned(owned),
        None => Luxels::Static(samples),
    };

    Some(LightMap {
        luxels,
        lmw,
        lmh,
        texmins: texmins_f,
    })
}

/// Quake's `CalcSurfaceExtents`: returns `(texmins, extent)` in surface texels
/// for the given texinfo and world-space polygon.
///
/// For each axis `j` in 0..2 the surface coordinate of every vertex `p` is
/// `p·vecs[j].xyz + vecs[j][3]`; tracking its min/max gives
/// `texmins[j] = floor(min/16)*16` and `extent[j] = (ceil(max/16) - floor(min/16))*16`.
pub(super) fn surface_extents(ti: &crate::bsp::TexInfo, world_poly: &[Vec3]) -> Option<([i32; 2], [i32; 2])> {
    if world_poly.len() < 3 {
        return None;
    }
    let mut mins = [f32::INFINITY; 2];
    let mut maxs = [f32::NEG_INFINITY; 2];
    for p in world_poly {
        for j in 0..2 {
            let val =
                p[0] * ti.vecs[j][0] + p[1] * ti.vecs[j][1] + p[2] * ti.vecs[j][2] + ti.vecs[j][3];
            if val < mins[j] {
                mins[j] = val;
            }
            if val > maxs[j] {
                maxs[j] = val;
            }
        }
    }
    let mut texmins = [0i32; 2];
    let mut extent = [0i32; 2];
    for j in 0..2 {
        if !mins[j].is_finite() || !maxs[j].is_finite() {
            return None;
        }
        let bmin = (mins[j] / 16.0).floor() as i32;
        let bmax = (maxs[j] / 16.0).ceil() as i32;
        texmins[j] = bmin.checked_mul(16)?;
        let ext = bmax.checked_sub(bmin)?.checked_mul(16)?;
        if ext < 0 {
            return None;
        }
        extent[j] = ext;
    }
    Some((texmins, extent))
}

/// `R_LightPoint` (`r_light.c`): sample the baked world light at world-space
/// point `p`, returning a 0..255 brightness (the average of the active light
/// styles at the surface directly below `p`).
///
/// Casts a ray straight down (`p` -> `p - 2048z`) through the worldmodel's BSP
/// and, at the first lightmapped surface the segment crosses, reads that
/// surface's lightmap luxel (summing each active style's value scaled by
/// `light_styles`, like `RecursiveLightPoint`). Returns:
///  * the sampled brightness (`0..=255`) when a lightmapped surface is hit;
///  * `0` when the ray hits a tiled (sky/liquid) surface or a surface with no
///    samples (the C returns 0 there);
///  * `255` (fullbright) when the map has no lighting data at all
///    (`!cl.worldmodel->lightdata`), matching the C early-out.
///
/// `light_styles` scales each style's luxel (1.0 == the C's `d_lightstylevalue`
/// of 256 mapped to neutral). SAFETY: the recursion is depth-bounded and every
/// index is `.get()`-checked, so corrupt node/plane/face data yields a default
/// (no light) rather than a panic or unbounded recursion.
///
/// It also returns the luxel it read: `(face, luxel)` — the world face the ray
/// landed on and the luxel's index in its grid — when that face has samples.
/// The steady torches' flicker ([`super::torch`]) moves a model's light by
/// that luxel's change.
pub(super) fn r_light_point_hit(bsp: &Bsp, p: Vec3, light_styles: &[f32; LIGHTSTYLES]) -> LightPoint {
    if bsp.lighting.is_empty() {
        return (255.0, None); // C: `if (!worldmodel->lightdata) return 255;`
    }
    let headnode = match bsp.models.first().and_then(|m| m.headnode.first().copied()) {
        Some(h) => h,
        None => return (0.0, None),
    };
    let end = [p[0], p[1], p[2] - 2048.0];
    let depth = bsp.nodes.len().saturating_add(2);
    match recursive_light_point(bsp, headnode, p, end, light_styles, depth) {
        Some((r, hit)) => (r.max(0.0), hit),
        None => (0.0, None), // C: `if (r == -1) r = 0;`
    }
}

/// What `R_LightPoint` read: the brightness, and the `(face, luxel)` it came
/// from when the face has samples.
pub(super) type LightPoint = (f32, Option<(usize, usize)>);

/// One step of `RecursiveLightPoint`. `node` is a child reference (negative =>
/// leaf, "didn't hit anything"). Returns `Some(brightness, luxel)` on a hit,
/// `None` for "didn't hit anything" (the C `-1`). `depth` bounds the recursion.
fn recursive_light_point(
    bsp: &Bsp,
    node: i32,
    start: Vec3,
    end: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
    depth: usize,
) -> Option<LightPoint> {
    if depth == 0 {
        return None;
    }
    if node < 0 {
        return None; // leaf: didn't hit anything (C `node->contents < 0`).
    }
    let ni: usize = node.try_into().ok()?;
    let node_rec = bsp.nodes.get(ni)?;
    let pi: usize = (node_rec.planenum as i64).try_into().ok()?;
    let plane = bsp.planes.get(pi)?;

    let front = dot(start, plane.normal) - plane.dist;
    let back = dot(end, plane.normal) - plane.dist;
    let side = front < 0.0; // C `side = front < 0`
    let side_child = |s: bool| -> Option<i32> {
        if s {
            node_rec.children.get(1).copied().map(|c| c as i32)
        } else {
            node_rec.children.first().copied().map(|c| c as i32)
        }
    };

    // If both endpoints are on the same side, recurse into that side only.
    if (back < 0.0) == side {
        return recursive_light_point(bsp, side_child(side)?, start, end, light_styles, depth - 1);
    }

    // Split: compute the midpoint on the plane.
    let denom = front - back;
    if denom == 0.0 {
        return recursive_light_point(bsp, side_child(side)?, start, end, light_styles, depth - 1);
    }
    let frac = front / denom;
    let mid = [
        start[0] + (end[0] - start[0]) * frac,
        start[1] + (end[1] - start[1]) * frac,
        start[2] + (end[2] - start[2]) * frac,
    ];

    // Go down the front side first.
    if let Some(r) = recursive_light_point(bsp, side_child(side)?, start, mid, light_styles, depth - 1)
    {
        return Some(r); // hit something
    }
    // (back<0)==side already handled above; here the sides differ, so check this
    // node's surfaces for an impact, then go down the back side.
    if let Some(r) = light_point_check_node(bsp, node_rec, mid, light_styles) {
        return Some(r);
    }
    recursive_light_point(bsp, side_child(!side)?, mid, end, light_styles, depth - 1)
}

/// Check this node's surfaces for the impact point `mid`, porting the surface
/// loop in `RecursiveLightPoint`. Returns `Some(brightness)` if `mid` lands on a
/// lightmapped surface (including `Some(0.0)` for a surface with no samples),
/// else `None` (the segment did not land on any of this node's surfaces).
fn light_point_check_node(
    bsp: &Bsp,
    node: &crate::bsp::DNode,
    mid: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
) -> Option<LightPoint> {
    use crate::bsp::TEX_SPECIAL;
    let first = node.firstface as usize;
    let count = node.numfaces as usize;
    let mut world_poly: Vec<Vec3> = Vec::new();
    for face_index in first..first.saturating_add(count) {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        let ti = match (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i))
        {
            Some(t) => t,
            None => continue,
        };
        // Tiled surfaces (sky/liquid) have no lightmaps (C `SURF_DRAWTILED`).
        if ti.flags & TEX_SPECIAL != 0 {
            continue;
        }

        // Surface coordinate of `mid`.
        let s = mid[0] * ti.vecs[0][0] + mid[1] * ti.vecs[0][1] + mid[2] * ti.vecs[0][2] + ti.vecs[0][3];
        let t = mid[0] * ti.vecs[1][0] + mid[1] * ti.vecs[1][1] + mid[2] * ti.vecs[1][2] + ti.vecs[1][3];

        // Need the surface extents (texmins/extent) to test the bounds, exactly
        // as `RecursiveLightPoint` uses surf->texturemins / surf->extents.
        if !face_world_poly(bsp, face, &mut world_poly) {
            continue;
        }
        let (texmins, extent) = match surface_extents(ti, &world_poly) {
            Some(v) => v,
            None => continue,
        };
        // C RecursiveLightPoint declares s,t,ds,dt as int: the surface coordinate is
        // TRUNCATED to int before the texmins subtract and the >>4 luxel select. Do
        // the same integer arithmetic so the chosen luxel matches the C exactly
        // (the float path could drift one luxel near integer boundaries).
        let s_i = s as i32; // (int) truncation toward zero, as the C cast
        let t_i = t as i32;
        let ds = s_i - texmins[0];
        let dt = t_i - texmins[1];
        if ds < 0 || dt < 0 || ds > extent[0] || dt > extent[1] {
            continue;
        }

        // The point is on this surface. With no samples the C returns 0.
        if face.lightofs < 0 {
            return Some((0.0, None));
        }
        let lmw = (extent[0] / 16 + 1) as usize;
        let lmh = (extent[1] / 16 + 1) as usize;
        let block = match lmw.checked_mul(lmh) {
            Some(b) => b,
            None => return Some((0.0, None)),
        };
        let start: usize = match face.lightofs.try_into() {
            Ok(s) => s,
            Err(_) => return Some((0.0, None)),
        };
        // Luxel coordinate within the block (C `ds>>4`, `dt>>4`; ds,dt >= 0 here).
        let lx = ((ds >> 4) as i64).clamp(0, lmw as i64 - 1) as usize;
        let ly = ((dt >> 4) as i64).clamp(0, lmh as i64 - 1) as usize;
        let luxel = ly * lmw + lx;

        // Sum each active style's luxel, scaled by its light-style value. The C
        // multiplies by `d_lightstylevalue` (256 == neutral) then `>>8`; here the
        // neutral scale is 1.0, so we sum `luxel_byte * scale`.
        let mut r = 0.0f32;
        for (k, &style) in face.styles.iter().enumerate() {
            if style == STYLE_NONE {
                break;
            }
            let off = match start.checked_add(k * block).and_then(|o| o.checked_add(luxel)) {
                Some(o) => o,
                None => break,
            };
            let sample = match bsp.lighting.get(off) {
                Some(&b) => b as f32,
                None => break,
            };
            let scale = light_styles.get(style as usize).copied().unwrap_or(1.0);
            r += sample * scale;
        }
        return Some((r, Some((face_index, luxel))));
    }
    None
}

/// Does any dynamic light in `dlights` actually REACH this face? Mirrors the
/// reach test inside [`add_dynamic_lights`] (`rad = radius - |dist|`, skip if
/// `rad < minlight`) WITHOUT touching luxels, so the lightmap/surface caches
/// can decide whether the static/style buffer is safe to reuse (dlights move
/// every frame, so a touched face must rebuild).
///
/// `dlightbits` is the face's `R_MarkLights` mask (see [`mark_dlights`]): only
/// marked lights are considered, so a light the BSP recursion never carried to
/// this face — e.g. one entirely on the far side of a wall — cannot flag the
/// face as touched, exactly as the C only dlights faces whose
/// `dlightframe == r_dlightframecount`.
///
/// Beyond the mask + plane tests, the light's impact point is tested against
/// the face's texture-space extent with the same `max(sd,td) + min(sd,td)/2`
/// distance estimate `add_dynamic_lights` uses per luxel — i.e. "would this
/// light add light to at least one luxel of this face". The C rebuilds a
/// surface-cache block on the marking alone (`D_CacheSurface` checks
/// `surf->dlightframe`), and `R_MarkLights` marks EVERY face stored on a
/// straddled node regardless of lateral distance (e.g. the whole length of a
/// long floor plane while a rocket flies past one end). A marked face that no
/// luxel of receives light bakes to exactly its unlit block, so skipping it is
/// output-identical and saves the rebake — the only purpose of the extent test.
/// Conservative at the rim: the extent is the luxel grid's quantized bounds and
/// the distance is the continuous minimum, less 2 units — `add_dynamic_lights`
/// truncates `sd`/`td` to integers and halves the smaller with `>> 1`, which
/// shortens a luxel's distance by less than 2 — so a light is never declared
/// "not reaching" when `add_dynamic_lights` would contribute; a missing plane
/// or texinfo falls back to `false`/plane-only (no light is folded without a
/// plane; without texinfo stay conservative).
pub(super) fn any_dlight_reaches(
    bsp: &Bsp,
    face: &crate::bsp::DFace,
    dlights: &[crate::dlight::DynamicLight],
    dlightbits: u32,
) -> bool {
    if dlights.is_empty() || dlightbits == 0 {
        return false;
    }
    let plane = match (face.planenum as i64)
        .try_into()
        .ok()
        .and_then(|pi: usize| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return false,
    };
    let normal = plane.normal;
    let texinfo = (face.texinfo as i64)
        .try_into()
        .ok()
        .and_then(|i: usize| bsp.texinfo.get(i));

    // The face's texture-space bounds (the projection CalcSurfaceExtents uses),
    // quantized outward to the 16-unit luxel grid the lightmap actually spans.
    // Inner `None` when the texinfo/vertex walk fails — then plane-only
    // (conservative). Computed LAZILY, only when the first masked light passes
    // the plane test: the common "a light is live somewhere, none near this
    // face's node" frame pays only the mask check, no edge walk.
    let compute_bounds = || -> Option<[f32; 4]> {
        let ti = texinfo?;
        let mut smin = f32::MAX;
        let mut smax = f32::MIN;
        let mut tmin = f32::MAX;
        let mut tmax = f32::MIN;
        let firstedge = face.firstedge as i64;
        if firstedge < 0 || face.numedges < 3 {
            return None;
        }
        for i in 0..face.numedges as i64 {
            let se_index: usize = (firstedge + i).try_into().ok()?;
            let &se = bsp.surfedges.get(se_index)?;
            let (edge_index, slot): (usize, usize) = if se >= 0 {
                (se as usize, 0)
            } else {
                ((se as i64).checked_neg()? as usize, 1)
            };
            let vid = *bsp.edges.get(edge_index)?.v.get(slot)? as usize;
            let p = bsp.vertexes.get(vid)?.point;
            let s = p[0] * ti.vecs[0][0] + p[1] * ti.vecs[0][1] + p[2] * ti.vecs[0][2]
                + ti.vecs[0][3];
            let t = p[0] * ti.vecs[1][0] + p[1] * ti.vecs[1][1] + p[2] * ti.vecs[1][2]
                + ti.vecs[1][3];
            smin = smin.min(s);
            smax = smax.max(s);
            tmin = tmin.min(t);
            tmax = tmax.max(t);
        }
        Some([
            (smin / 16.0).floor() * 16.0,
            (smax / 16.0).ceil() * 16.0,
            (tmin / 16.0).floor() * 16.0,
            (tmax / 16.0).ceil() * 16.0,
        ])
    };
    let mut bounds: Option<Option<[f32; 4]>> = None;

    // The same projection helper for the light's impact point.
    let project = |p: [f32; 3], v: &[f32; 4]| p[0] * v[0] + p[1] * v[1] + p[2] * v[2] + v[3];

    for (lnum, dl) in dlights.iter().enumerate() {
        // Same mask skip as `add_dynamic_lights` (C `surf->dlightbits & (1<<lnum)`).
        if lnum >= u32::BITS as usize || dlightbits & (1u32 << lnum) == 0 {
            continue;
        }
        let dist = dot(dl.origin, normal) - plane.dist;
        let rad = dl.radius - dist.abs();
        if rad < dl.minlight {
            continue;
        }
        let b = *bounds.get_or_insert_with(compute_bounds);
        let (Some([smin, smax, tmin, tmax]), Some(ti)) = (b, texinfo) else {
            return true; // plane reached; no extent info -> conservative
        };
        // Project the light onto the plane and into texture space, then measure
        // the dist2 estimate to the nearest point of the face's luxel extent.
        let impact = [
            dl.origin[0] - normal[0] * dist,
            dl.origin[1] - normal[1] * dist,
            dl.origin[2] - normal[2] * dist,
        ];
        let ls = project(impact, &ti.vecs[0]);
        let lt = project(impact, &ti.vecs[1]);
        let sd = (smin - ls).max(ls - smax).max(0.0);
        let td = (tmin - lt).max(lt - tmax).max(0.0);
        let dist2 = if sd > td { sd + td * 0.5 } else { td + sd * 0.5 };
        if dist2 - 2.0 < rad - dl.minlight {
            return true;
        }
    }
    false
}

/// `R_PushDlights` (`r_light.c`): compute every face's `surf->dlightbits` mask
/// for this frame by running the `R_MarkLights` BSP recursion from `headnode`
/// once per live light (`dlights[i]` marks bit `1 << i`).
///
/// `bits` is a caller-owned scratch vector, cleared and resized to
/// `bsp.faces.len()` (so once it has grown to the map's face count the per-frame
/// marking allocates nothing). With no live lights it is left EMPTY — every
/// lookup then reads 0 ("no light marked"), and the zero-fill is skipped.
///
/// The C resets stale masks with the `dlightframe != r_dlightframecount` check;
/// zero-filling the scratch each frame is the equivalent here.
///
/// FALLBACK: a `Bsp` with no node tree (synthetic fixtures like [`demo_room`](super::demo_room))
/// has nothing to recurse, so every face is marked with [`ALL_DLIGHT_BITS`] and
/// the per-light distance test in [`add_dynamic_lights`] remains the only gate —
/// the pre-gating behaviour. Real maps always carry a node tree.
pub(super) fn mark_dlights(
    bsp: &Bsp,
    headnode: i32,
    dlights: &[crate::dlight::DynamicLight],
    bits: &mut Vec<u32>,
) {
    bits.clear();
    if dlights.is_empty() {
        return;
    }
    if bsp.nodes.is_empty() {
        bits.resize(bsp.faces.len(), ALL_DLIGHT_BITS);
        return;
    }
    bits.resize(bsp.faces.len(), 0);
    for (i, dl) in dlights.iter().take(u32::BITS as usize).enumerate() {
        // In a well-formed tree each node is visited at most once per light, so
        // a budget of `nodes.len()` visits never truncates a legitimate walk; it
        // only stops a malformed (cyclic) node graph from recursing forever.
        let mut budget = bsp.nodes.len();
        mark_lights_r(bsp, dl, 1u32 << i, headnode, &mut budget, bits);
    }
}

/// [`mark_dlights`] for one more model of the same `bsp` — a brush entity's
/// own subtree, `R_MarkLights` from its `firstclipnode` in
/// `R_DrawBEntitiesOnList` — OR-ing its lights' bits into `bits` without
/// clearing what the world (or another entity) marked there: the models' face
/// ranges are disjoint, so one mask serves them all. `dlights` are the world's
/// lights, untranslated, in the same order (the same bits): id marks a moved
/// door's subtree with `cl_dlights` as they are, against the model's own
/// planes.
pub(super) fn mark_dlights_more(
    bsp: &Bsp,
    headnode: i32,
    dlights: &[crate::dlight::DynamicLight],
    bits: &mut Vec<u32>,
) {
    if dlights.is_empty() {
        return;
    }
    if bsp.nodes.is_empty() {
        bits.clear();
        bits.resize(bsp.faces.len(), ALL_DLIGHT_BITS);
        return;
    }
    if bits.len() < bsp.faces.len() {
        bits.resize(bsp.faces.len(), 0);
    }
    for (i, dl) in dlights.iter().take(u32::BITS as usize).enumerate() {
        let mut budget = bsp.nodes.len();
        mark_lights_r(bsp, dl, 1u32 << i, headnode, &mut budget, bits);
    }
}

/// `R_MarkLights` (`r_light.c`): descend the BSP from `node`, OR-ing `bit` into
/// `bits[face]` for every surface the light's sphere reaches through the tree.
///
/// At each node: `dist = dot(light.origin, plane.normal) - plane.dist`. When the
/// whole sphere is on one side (`dist > radius` / `dist < -radius`) only that
/// child subtree is descended — nothing behind a plane the light does not reach
/// can be marked, which is what stops a dynamic light lighting faces through a
/// wall. When the sphere straddles the plane, the surfaces stored on this node
/// (which lie on that plane) are marked and both children are descended.
///
/// A negative `node` is a leaf reference (`-(leaf+1)`) and ends the descent,
/// matching the C `if (node->contents < 0) return;`. Out-of-range node/plane
/// indices return harmlessly; `budget` bounds total node visits (see
/// [`mark_dlights`]).
fn mark_lights_r(
    bsp: &Bsp,
    light: &crate::dlight::DynamicLight,
    bit: u32,
    node: i32,
    budget: &mut usize,
    bits: &mut [u32],
) {
    if node < 0 || *budget == 0 {
        return;
    }
    *budget -= 1;
    let node_rec = match usize::try_from(node).ok().and_then(|ni| bsp.nodes.get(ni)) {
        Some(n) => n,
        None => return,
    };
    let plane = match usize::try_from(node_rec.planenum)
        .ok()
        .and_then(|pi| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return,
    };

    let dist = dot(light.origin, plane.normal) - plane.dist;

    if dist > light.radius {
        mark_lights_r(bsp, light, bit, node_rec.children[0] as i32, budget, bits);
        return;
    }
    if dist < -light.radius {
        mark_lights_r(bsp, light, bit, node_rec.children[1] as i32, budget, bits);
        return;
    }

    // Mark the polygons stored on this node (they lie on the straddled plane).
    let first = node_rec.firstface as usize;
    for entry in bits.iter_mut().skip(first).take(node_rec.numfaces as usize) {
        *entry |= bit;
    }

    mark_lights_r(bsp, light, bit, node_rec.children[0] as i32, budget, bits);
    mark_lights_r(bsp, light, bit, node_rec.children[1] as i32, budget, bits);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlight::DynamicLight;
    use crate::math::sub;
    use crate::render::{demo_room, Camera, Image, Scene};
    use crate::render::fixtures::render_once;
    use crate::render::fixtures::{one_face_bsp, one_face_bsp_zplane, two_style_face_bsp};

    // -- Lightmap: surface extents / luxel math ----------------------------

    /// An axis-aligned texinfo: s along world X, t along world Y, no offset.
    fn axis_texinfo() -> crate::bsp::TexInfo {
        crate::bsp::TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }
    }

    #[test]
    fn surface_extents_basic() {
        let ti = axis_texinfo();
        // A 32x32 square: surface s in [0,32], t in [0,32].
        let poly = [
            [0.0, 0.0, 0.0],
            [32.0, 0.0, 0.0],
            [32.0, 32.0, 0.0],
            [0.0, 32.0, 0.0],
        ];
        let (texmins, extent) = surface_extents(&ti, &poly).unwrap();
        // bmin = floor(0/16) = 0, bmax = ceil(32/16) = 2 -> texmin 0, extent 32.
        assert_eq!(texmins, [0, 0]);
        assert_eq!(extent, [32, 32]);
        // Luxel grid is extent/16 + 1 = 3x3.
        assert_eq!((extent[0] / 16 + 1, extent[1] / 16 + 1), (3, 3));
    }

    #[test]
    fn surface_extents_non_aligned() {
        let ti = axis_texinfo();
        // s in [3, 20]  -> bmin=floor(3/16)=0,  bmax=ceil(20/16)=2 -> mins 0,  ext 32.
        // t in [-5, 5]  -> bmin=floor(-5/16)=-1, bmax=ceil(5/16)=1 -> mins -16, ext 32.
        let poly = [
            [3.0, -5.0, 0.0],
            [20.0, -5.0, 0.0],
            [20.0, 5.0, 0.0],
            [3.0, 5.0, 0.0],
        ];
        let (texmins, extent) = surface_extents(&ti, &poly).unwrap();
        assert_eq!(texmins, [0, -16]);
        assert_eq!(extent, [32, 32]);
    }

    #[test]
    fn surface_extents_rejects_degenerate() {
        let ti = axis_texinfo();
        let poly = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        assert!(surface_extents(&ti, &poly).is_none());
    }

    #[test]
    fn lightmap_factor_bilinear_and_clamp() {
        // 2x2 luxel grid, texmins at origin so luxel coord = s/16.
        let samples = [0u8, 255u8, 255u8, 0u8]; // checker
        let lm = LightMap {
            luxels: Luxels::Static(&samples),
            lmw: 2,
            lmh: 2,
            texmins: [0.0, 0.0],
        };
        // At (0,0): luxel (0,0) = 0 -> factor 0.
        assert!((lm.factor_at(0.0, 0.0) - 0.0).abs() < 1e-6);
        // At (16,0): luxel (1,0) = 255 -> factor 2.0 (overbright).
        assert!((lm.factor_at(16.0, 0.0) - 2.0).abs() < 1e-6);
        // Centre (8,8) averages all four luxels -> 127.5/255*2.
        let mid = lm.factor_at(8.0, 8.0);
        assert!((mid - (127.5 / 255.0 * 2.0)).abs() < 1e-3);
        // Coordinates far past the edge clamp to the last luxel (no panic).
        assert!((lm.factor_at(10_000.0, 10_000.0) - 0.0).abs() < 1e-6); // luxel (1,1)=0
        // Negative coords clamp to luxel 0.
        assert!((lm.factor_at(-500.0, -500.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn lightmap_oob_sample_is_fullbright() {
        // Claims 2x2 but only 1 byte present: out-of-slice luxels read as
        // fullbright (255 -> factor 2.0) rather than panicking.
        let samples = [0u8];
        let lm = LightMap {
            luxels: Luxels::Static(&samples),
            lmw: 2,
            lmh: 2,
            texmins: [0.0, 0.0],
        };
        // luxel (1,1) is index 3, out of the 1-byte slice -> fullbright.
        assert!((lm.factor_at(16.0, 16.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn face_lightmap_present() {
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 0, 0);
        let lm = face_lightmap(&bsp, &face, &poly).expect("lightmap should be present");
        assert_eq!((lm.lmw, lm.lmh), (3, 3));
        match &lm.luxels {
            Luxels::Static(s) => assert_eq!(s.len(), 9),
            Luxels::Owned(_) => panic!("no dlights -> static borrow expected"),
        }
        assert_eq!(lm.texmins, [0.0, 0.0]);
        // Every luxel 200 -> factor 200/255*2 ~= 1.568.
        assert!((lm.factor_at(0.0, 0.0) - (200.0 / 255.0 * 2.0)).abs() < 1e-6);
    }

    #[test]
    fn face_lightmap_fullbright_cases() {
        // No lighting lump -> fullbright.
        let (bsp, face, poly) = one_face_bsp(Vec::new(), 0, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // lightofs < 0 on a normal face -> NOT fullbright: R_BuildLightMap's
        // zero ("ambient") block, i.e. black (colormap row 63).
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], -1, 0);
        let lm = face_lightmap(&bsp, &face, &poly).expect("a sample-less face is lit (black)");
        assert_eq!(lm.factor_at(16.0, 16.0), 0.0);
        assert_eq!(colormap_row(lm.factor_at(16.0, 16.0)), COLORMAP_ROWS - 1);

        // TEX_SPECIAL (sky/liquid) -> fullbright.
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 0, crate::bsp::TEX_SPECIAL);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // Grid needs 9 luxels but only 4 available -> fullbright (no overrun).
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 4], 0, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // lightofs near the end leaves too few bytes -> fullbright.
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 5, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());
    }

    // -- Dynamic lights (R_AddDynamicLights) -------------------------------

    #[test]
    fn dynamic_light_brightens_near_luxel_only() {
        // Static luxels all 100; factor there is 100/255*2 ~= 0.784.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let static_factor = 100.0 / 255.0 * 2.0;

        // A bright light 16 units above luxel (0,0) (world [0,0,16]); small radius
        // so it lights the near corner but not the far one.
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
            .expect("lightmap present");
        // It must have switched to the owned augmented buffer.
        assert!(matches!(lm.luxels, Luxels::Owned(_)), "a reaching light must own the buffer");

        // Near luxel (s=0,t=0): dist=16, rad=60-16=44, dist2=0 -> +44 -> 144.
        // factor 144/255*2 ~= 1.13, brighter than the static 0.784.
        let near = lm.factor_at(0.0, 0.0);
        assert!(near > static_factor + 0.2, "near luxel should brighten: {near} vs {static_factor}");

        // Far luxel (s=32,t=32) = world (32,32,0): sd=td=32, dist2=48 >= rad(44)
        // -> no add, stays at the static value.
        let far = lm.factor_at(32.0, 32.0);
        assert!((far - static_factor).abs() < 1e-4, "far luxel must be unchanged: {far} vs {static_factor}");
    }

    #[test]
    fn dynamic_light_uses_the_c_integer_distances() {
        // R_AddDynamicLights: `sd`/`td` truncated to int, the smaller halved with
        // `>> 1`, and `(rad - dist)*256` truncated into 8.8 blocklights.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        // 10 units above impact (5.7, 21.9): rad = 60 - 10 = 50.
        let dl = DynamicLight::new([5.7, 21.9, 10.0], 60.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
            .expect("lightmap present");
        assert_eq!((lm.lmw, lm.texmins), (3, [0.0, 0.0]), "fixture layout");
        let Luxels::Owned(v) = &lm.luxels else { panic!("a reaching light owns the buffer") };
        // Luxel (0,0): sd = 5, td = 21 -> dist 21 + (5 >> 1) = 23 -> +27
        // (the float estimate would give 24.75 -> +25.25).
        assert_eq!(v[0], 127.0);
        // Luxel (1,1): sd = |trunc(-10.3)| = 10, td = trunc(5.9) = 5 -> 10 + 2 = 12 -> +38.
        assert_eq!(v[4], 138.0);
    }

    #[test]
    fn unlit_face_hit_by_dlight_is_not_fullbright() {
        // A NORMAL wall with no baked lightmap (lightofs < 0) is black (zero base)
        // with no dlights, and a reaching dynamic light adds onto that base.
        let (bsp, face, poly) = one_face_bsp_zplane(0);
        // Force lightofs < 0 (no baked samples) but keep the face NORMAL (flags 0).
        let mut unlit = face.clone();
        unlit.lightofs = -1;

        // With no dlights: all zero (black).
        let dark = face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
            .expect("an unlit face with no dlights is black, not fullbright");
        assert_eq!(dark.factor_at(0.0, 0.0), 0.0);

        // A bright light 16 units above luxel (0,0): the face is now lightmapped,
        // owning a buffer, bright near the impact and dark (not fullbright) away.
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
            .expect("a reaching dlight must build a lightmap for the unlit face");
        assert!(matches!(lm.luxels, Luxels::Owned(_)), "reaching dlight owns the buffer");
        // Near luxel brightens above the zero base; far luxel stays at ~0 (dark,
        // NOT fullbright — which is the whole point of the fix).
        let near = lm.factor_at(0.0, 0.0);
        let far = lm.factor_at(32.0, 32.0);
        assert!(near > 0.1, "near the dlight the unlit face lights up: {near}");
        assert!(far < 0.05, "away from the dlight the unlit face is dark, not fullbright: {far}");

        // A far-away dlight that never reaches leaves the face black.
        let far_dl = DynamicLight::new([0.0, 0.0, 100_000.0], 200.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&far_dl), ALL_DLIGHT_BITS)
            .expect("still a (black) lightmap");
        assert_eq!(lm.factor_at(0.0, 0.0), 0.0, "a non-reaching dlight leaves the unlit face black");
    }

    /// `one_face_bsp_zplane` with the face's surfedge/edge/vertex walk wired to
    /// the SAME 0..32 square as the hand-made poly, so [`any_dlight_reaches`]'s
    /// internal extent walk sees the real face bounds.
    fn one_face_bsp_zplane_with_edges(luxel: u8) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
        let (mut bsp, face, poly) = one_face_bsp_zplane(luxel);
        bsp.vertexes = poly
            .iter()
            .map(|&point| crate::bsp::DVertex { point })
            .collect();
        bsp.edges = (0..4)
            .map(|i| crate::bsp::DEdge {
                v: [i as u16, ((i + 1) % 4) as u16],
            })
            .collect();
        bsp.surfedges = vec![0, 1, 2, 3];
        (bsp, face, poly)
    }

    #[test]
    fn dlight_reach_is_spatial_not_just_planar() {
        // Regression for the live-play "pop": `any_dlight_reaches` used ONLY the
        // plane-distance test, so a dlight anywhere near-coplanar with a face —
        // even 2000+ units away laterally (the start map's lava fireballs vs the
        // spawn hall's floors) — kicked the face off the baked surface cache
        // onto the per-pixel path and the WHOLE view's shading visibly shifted
        // whenever any dlight existed. WinQuake's R_MarkLights recurses the BSP
        // from the light, bounded to ±radius at every split, so distant faces
        // are never marked. The gate now also tests the face's texture-space
        // extent with R_AddDynamicLights' own distance estimate.
        let (bsp, face, _poly) = one_face_bsp_zplane_with_edges(100);

        // Near light above the face: reaches (and genuinely lights luxels).
        let near = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            any_dlight_reaches(&bsp, &face, std::slice::from_ref(&near), ALL_DLIGHT_BITS),
            "a light directly above the face reaches it"
        );

        // Coplanar-but-distant light (the fireball case): 16 above the z=0
        // plane like `near`, but 5000 units away laterally. The old plane-only
        // gate said REACHES (rad = 200-16 = 184 >= 0); it must not.
        let coplanar_far = DynamicLight::new([5000.0, 0.0, 16.0], 200.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&coplanar_far), ALL_DLIGHT_BITS),
            "a laterally distant coplanar light must NOT mark the face"
        );
        // ...and add_dynamic_lights agrees it contributes nothing: every luxel
        // keeps its static value (the gate is exactly "would at least one luxel
        // receive light"; the owned-vs-borrowed buffer kind is an implementation
        // detail of the lazy materialisation).
        let lm = face_lightmap_dyn(
            &bsp,
            &face,
            &_poly,
            &NEUTRAL_LIGHTSTYLE_SCALES,
            std::slice::from_ref(&coplanar_far),
            ALL_DLIGHT_BITS,
        )
        .expect("lightmap present");
        let static_factor = 100.0 / 255.0 * 2.0;
        for &(s, t) in &[(0.0, 0.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (lm.factor_at(s, t) - static_factor).abs() < 1e-6,
                "the distant light adds nothing at ({s},{t})"
            );
        }

        // Far along the normal: fails the plane test as before.
        let above = DynamicLight::new([0.0, 0.0, 300.0], 200.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&above), ALL_DLIGHT_BITS),
            "a light beyond its radius along the normal does not reach"
        );

        // Rim case just inside: impact at s=32+40=72 -> sd=40 (extent quantizes
        // to 0..32), td=0, dist2=40 < reach (60-16=44): reaches.
        let rim = DynamicLight::new([72.0, 16.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            any_dlight_reaches(&bsp, &face, std::slice::from_ref(&rim), ALL_DLIGHT_BITS),
            "a light just within the dist2 estimate of the extent reaches"
        );
        // ...and just outside: sd=48 > 44: does not reach.
        let rim_out = DynamicLight::new([80.0, 16.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&rim_out), ALL_DLIGHT_BITS),
            "a light just outside the dist2 estimate does not reach"
        );

        // Empty slice: never reaches (the no-dlight fast path).
        assert!(!any_dlight_reaches(&bsp, &face, &[], ALL_DLIGHT_BITS));
    }

    #[test]
    fn empty_dlights_keeps_static_borrow_and_factor() {
        // With no dlights the lightmap must borrow the static bytes and sample
        // exactly the pre-dlight factor (byte-identical behaviour).
        let (bsp, face, poly) = one_face_bsp_zplane(150);
        let with_none = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0).expect("present");
        assert!(matches!(with_none.luxels, Luxels::Static(_)), "empty slice keeps static borrow");

        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        // Same sampled factor at several points.
        for &(s, t) in &[(0.0f32, 0.0f32), (8.0, 8.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (with_none.factor_at(s, t) - baseline.factor_at(s, t)).abs() < 1e-7,
                "empty-dlight factor must equal the static factor at ({s},{t})"
            );
        }
    }

    #[test]
    fn distant_light_below_minlight_does_not_contribute() {
        // A light far outside its radius range contributes nothing: rad < minlight
        // -> the face keeps its static borrow, no panic, no change.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        // dist 100000 >> radius 200, so rad < minlight -> no contribution.
        let dl = DynamicLight::new([0.0, 0.0, 100_000.0], 200.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS).expect("present");
        assert!(matches!(lm.luxels, Luxels::Static(_)), "a non-reaching light keeps the static borrow");
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        assert!((lm.factor_at(0.0, 0.0) - baseline.factor_at(0.0, 0.0)).abs() < 1e-7);
    }

    #[test]
    fn dynamic_light_factor_is_clamped() {
        // A huge-radius light right on the surface drives the luxel far past 255;
        // factor_at must clamp to MAX_LIGHT_FACTOR (finite, no overflow/NaN).
        let (bsp, face, poly) = one_face_bsp_zplane(255);
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 100_000.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS).expect("present");
        let f = lm.factor_at(0.0, 0.0);
        assert!(f.is_finite());
        assert!((f - MAX_LIGHT_FACTOR).abs() < 1e-6, "huge add must clamp to {MAX_LIGHT_FACTOR}, got {f}");
    }

    // -- Animated light styles (R_BuildLightMap multi-style combine) -------

    #[test]
    fn single_steady_style0_neutral_is_static_and_byte_identical() {
        // A single steady style-0 face under neutral scales must keep the borrowed
        // static slice and sample exactly the static factor (no regression).
        let (bsp, face, poly) = one_face_bsp_zplane(200);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
            .expect("present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "single steady style-0 at scale 1.0 keeps the static borrow"
        );
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        for &(s, t) in &[(0.0f32, 0.0f32), (8.0, 8.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (lm.factor_at(s, t) - baseline.factor_at(s, t)).abs() < 1e-7,
                "neutral style-0 factor must equal the static factor at ({s},{t})"
            );
        }
    }

    #[test]
    fn second_flicker_style_changes_effective_luxel() {
        // styles[0]=0 (steady), styles[1]=1 (flicker). block0 = 100, block1 = 200.
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);

        // Style 1 dark (scale 0): effective = block0*1 + block1*0 = 100.
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.0;
        let dark = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
        assert!(matches!(dark.luxels, Luxels::Owned(_)), "2-style face owns the combine");
        // Effective luxel 100 -> factor 100/255*2.
        assert!((dark.factor_at(0.0, 0.0) - (100.0 / 255.0 * 2.0)).abs() < 1e-5);

        // Style 1 normal (scale 1): effective = 100 + 200 = 300 -> clamps in factor.
        let mut scales_on = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_on[1] = 1.0;
        let bright = face_lightmap_dyn(&bsp, &face, &poly, &scales_on, &[], 0).expect("present");
        let f_dark = dark.factor_at(0.0, 0.0);
        let f_bright = bright.factor_at(0.0, 0.0);
        assert!(
            f_bright > f_dark + 0.5,
            "raising style-1 scale must brighten the effective luxel: {f_dark} -> {f_bright}"
        );

        // And a partial scale lands strictly between (proves it scales the block).
        let mut scales_half = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_half[1] = 0.5; // effective = 100 + 100 = 200
        let mid = face_lightmap_dyn(&bsp, &face, &poly, &scales_half, &[], 0).expect("present");
        assert!((mid.factor_at(0.0, 0.0) - (200.0 / 255.0 * 2.0)).abs() < 1e-5);
    }

    #[test]
    fn multi_style_too_short_lighting_falls_back_without_panic() {
        // The face declares two styles (needs 18 luxels) but only one block (9) is
        // present. The combine must NOT read out of bounds: it falls back to the
        // single static block (byte-identical to a steady style-0 face).
        let (mut bsp, mut face, poly) = one_face_bsp_zplane(123);
        face.styles = [0, 1, 255, 255]; // two active styles, but only 9 luxels stored
        bsp.lighting = vec![123u8; 9]; // one block only -> second block overruns
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 2.0; // would matter if the (missing) block were read
        // Must not panic; falls back to the single static block.
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "a too-short multi-style lump falls back to the static block borrow"
        );
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        assert!((lm.factor_at(0.0, 0.0) - baseline.factor_at(0.0, 0.0)).abs() < 1e-7);
    }

    #[test]
    fn unset_style_index_treated_as_normal() {
        // A face whose only style references an index whose scale is left at the
        // neutral 1.0 stays at full brightness (the "missing style -> normal"
        // rule). Style index 7, scale 1.0 (neutral): a single-style-at-1.0 face is
        // byte-identical to the static block.
        let (bsp, face, poly) = {
            let (mut bsp, mut face, poly) = one_face_bsp_zplane(180);
            face.styles = [7, 255, 255, 255];
            bsp.lighting = vec![180u8; 9];
            (bsp, face, poly)
        };
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
            .expect("present");
        // Single style at scale 1.0 -> static borrow, full brightness.
        assert!(matches!(lm.luxels, Luxels::Static(_)));
        assert!((lm.factor_at(0.0, 0.0) - (180.0 / 255.0 * 2.0)).abs() < 1e-6);
    }

    #[test]
    fn render_scene_ext_dlight_noop_on_fullbright_world() {
        // demo_room has no lighting lump, so its faces are fullbright (no
        // lightmap) and dlights cannot attach: the frame must be UNCHANGED even
        // with a bright light present. (The actual brightening of a lightmapped
        // surface is exercised by `dynamic_light_brightens_near_luxel_only`.)
        let bsp = demo_room();
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);

        let base = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        // demo_room has no lighting lump, so faces are fullbright (no lightmap)
        // and dlights cannot attach; the frame must therefore be UNCHANGED even
        // with a light present -- proving dlights never touch non-lightmapped
        // faces and never panic.
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 600.0, 10.0, 0.0, 0.0, 0);
        let lit = render_once(&Scene { dlights: std::slice::from_ref(&dl), ..Scene::new(&bsp, cam, 160, 120, &pal) });
        assert_eq!(base.pixels, lit.pixels, "fullbright (lightmap-less) world must ignore dlights");
    }

    // -- R_MarkLights BSP dlight gating (r_light.c) -------------------------

    /// Two rooms separated by a solid wall split at `x=0`, floored at `z=0` by
    /// two COPLANAR faces — face 0 (near room, `x>0`) and face 1 (far room,
    /// `x<0`) — with a real `R_MarkLights` node tree:
    ///
    /// ```text
    ///   node 0 (root): plane 0 (wall x=0), no faces, children [node 1, node 2]
    ///   node 1 (x>0):  plane 1 (floor z=0), owns face 0, children = leaves
    ///   node 2 (x<0):  plane 1 (floor z=0), owns face 1, children = leaves
    /// ```
    ///
    /// Both floors are lightmapped (uniform luxels = 100) and the shared texinfo
    /// S/T axes are scaled by 1/4 (a 4x texture scale, legal in Quake), so
    /// lightmap-space distances are a quarter of world distances. That is
    /// exactly the configuration where the old proximity-only dlight gating
    /// visibly lit the far room's wall-adjacent floor luxels through the wall
    /// (the two floors share one plane, and the lit luxels sit within the
    /// light's lightmap-space reach) while the C's BSP recursion never descends
    /// past a split the light's sphere does not touch.
    fn two_rooms_bsp() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DNode, DPlane, DVertex, TexInfo};

        let mut vertexes: Vec<DVertex> = Vec::new();
        let mut edges: Vec<DEdge> = vec![DEdge { v: [0, 0] }]; // edge 0 unused
        let mut surfedges: Vec<i32> = Vec::new();
        let mut faces: Vec<DFace> = Vec::new();

        // plane 0: the solid wall split at x=0; plane 1: the shared floor z=0.
        let planes = vec![
            DPlane { normal: [1.0, 0.0, 0.0], dist: 0.0, ptype: 0 },
            DPlane { normal: [0.0, 0.0, 1.0], dist: 0.0, ptype: 2 },
        ];

        // One floor quad per room (z=0, +Z normal, so a camera above passes
        // the backface cull), clockwise seen from above as qbsp winds faces
        // (the order the edge renderer takes leading and trailing edges from).
        let mut add_floor = |x0: f32, x1: f32| {
            let base = vertexes.len() as u16;
            for c in [[x0, 128.0, 0.0], [x1, 128.0, 0.0], [x1, -128.0, 0.0], [x0, -128.0, 0.0]] {
                vertexes.push(DVertex { point: c });
            }
            let first_edge = surfedges.len() as i32;
            for k in 0..4u16 {
                let e = edges.len() as i32;
                edges.push(DEdge { v: [base + k, base + (k + 1) % 4] });
                surfedges.push(e);
            }
            faces.push(DFace {
                planenum: 1,
                side: 0,
                firstedge: first_edge,
                numedges: 4,
                texinfo: 0,
                styles: [0, 255, 255, 255],
                lightofs: 0,
            });
        };
        add_floor(0.0, 256.0); // face 0: near room
        add_floor(-256.0, 0.0); // face 1: far room

        // The marking tree (children < 0 are leaf references and end a descent).
        let nodes = vec![
            DNode { planenum: 0, children: [1, 2], mins: [-256, -128, -16], maxs: [256, 128, 16], firstface: 0, numfaces: 0 },
            DNode { planenum: 1, children: [-1, -2], mins: [0, -128, -16], maxs: [256, 128, 16], firstface: 0, numfaces: 1 },
            DNode { planenum: 1, children: [-3, -4], mins: [-256, -128, -16], maxs: [0, 128, 16], firstface: 1, numfaces: 1 },
        ];

        // 1/4-scale S/T axes (4x texture scale): face extents s,t in [-64..64],
        // so each floor is a 5x5 luxel grid (25 bytes per face; lightofs 0).
        let texinfo = vec![TexInfo {
            vecs: [[0.25, 0.0, 0.0, 0.0], [0.0, 0.25, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }];

        let models = vec![DModel {
            mins: [-256.0, -128.0, -16.0],
            maxs: [256.0, 128.0, 16.0],
            origin: [0.0, 0.0, 0.0],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: 0,
            numfaces: faces.len() as i32,
        }];

        Bsp {
            version: 29,
            entities: String::new(),
            planes,
            vertexes,
            edges,
            faces,
            nodes,
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo,
            models,
            marksurfaces: Vec::new(),
            surfedges,
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: vec![100u8; 64],
        }
    }

    #[test]
    fn mark_lights_prunes_subtree_beyond_solid_split() {
        let bsp = two_rooms_bsp();
        // Light in the NEAR room, 40 units from the wall, radius 36: the sphere
        // never crosses the x=0 split, so the far room's subtree is never
        // descended (C `if (dist > light->radius)` recurses children[0] only).
        let dl = DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits.len(), 2);
        assert_eq!(bits[0], 1, "near-room floor must carry light 0's bit");
        assert_eq!(bits[1], 0, "far-room floor must NOT be marked across the solid split");

        // The pure proximity test (the pre-R_MarkLights gating) WOULD have
        // flagged the far face as touched — the floors are coplanar, so the
        // plane distance (8) is well inside the radius (36). This is exactly
        // the divergence the BSP recursion fixes.
        assert!(
            any_dlight_reaches(&bsp, &bsp.faces[1], std::slice::from_ref(&dl), ALL_DLIGHT_BITS),
            "sanity: by plane distance alone the far floor is in reach"
        );
        assert!(
            !any_dlight_reaches(&bsp, &bsp.faces[1], std::slice::from_ref(&dl), bits[1]),
            "with the real mask the far floor reports untouched"
        );
    }

    #[test]
    fn mark_lights_straddling_split_marks_both_sides() {
        let bsp = two_rooms_bsp();
        // The same light moved to 10 units from the wall: its sphere straddles
        // the split, so BOTH children are descended and both floors are marked
        // (the C marks the node's own faces and recurses both sides).
        let dl = DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits[0], 1, "near-room floor marked");
        assert_eq!(bits[1], 1, "far-room floor marked: the sphere reaches across the split");
    }

    #[test]
    fn mark_lights_per_light_bits_accumulate() {
        let bsp = two_rooms_bsp();
        // Light 0 stays in the near room; light 1 straddles the split. Face 0
        // accumulates both bits, face 1 only light 1's (1 << 1) — per-light
        // masks exactly like the C `surf->dlightbits |= bit`.
        let dls = [
            DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0),
            DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0),
        ];
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, &dls, &mut bits);
        assert_eq!(bits[0], 0b11);
        assert_eq!(bits[1], 0b10);
    }

    #[test]
    fn mark_lights_no_node_tree_falls_back_to_all_marked() {
        // A synthetic map with no node tree (demo_room) cannot recurse: every
        // face falls back to "marked by every light" so the per-light distance
        // test in add_dynamic_lights remains the only gate (pre-gating
        // behaviour). With no live lights the scratch stays empty (mask 0).
        let bsp = demo_room();
        assert!(bsp.nodes.is_empty(), "fixture: demo_room must have no nodes");
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 200.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits.len(), bsp.faces.len());
        assert!(bits.iter().all(|&b| b == ALL_DLIGHT_BITS));
        mark_dlights(&bsp, 0, &[], &mut bits);
        assert!(bits.is_empty(), "no live lights -> empty scratch (reads as mask 0)");
    }

    #[test]
    fn mark_lights_terminates_on_malformed_cyclic_tree() {
        // A node whose children point back at itself must terminate via the
        // visit budget (never hang or overflow the stack) — real trees visit
        // each node at most once per light.
        let mut bsp = two_rooms_bsp();
        bsp.nodes[0].children = [0, 0];
        let dl = DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits); // must return
        // Out-of-range headnode is harmless too.
        mark_dlights(&bsp, 999, std::slice::from_ref(&dl), &mut bits);
        assert!(bits.iter().all(|&b| b == 0));
    }

    #[test]
    fn unmarked_face_ignores_geometrically_reaching_dlight() {
        // A light that reaches the face by pure distance must still be ignored
        // when its R_MarkLights bit is clear — the C only folds lights present
        // in `surf->dlightbits` (`R_AddDynamicLights`).
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);

        // Mask 0: the buffer stays the static borrow, as if the light were absent.
        let lm = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), 0,
        )
        .expect("lightmap present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "an unmarked light must not touch the lightmap"
        );

        // Bit 0 set: the very same light brightens (owned buffer).
        let lit = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), 1,
        )
        .expect("lightmap present");
        assert!(matches!(lit.luxels, Luxels::Owned(_)), "a marked reaching light applies");
    }

    /// Project a world point through the same camera basis / focal math the
    /// world pass uses, returning the (clamped) target pixel.
    fn project_px(cam: &Camera, w: usize, h: usize, p: Vec3) -> (usize, usize) {
        let (forward, right, up) = cam.basis();
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let tan_half = (cam.fov_deg as f64 * 0.5).to_radians().tan();
        let focal = (cx as f64 / tan_half) as f32;
        let rel = sub(p, cam.pos);
        let vz = dot(rel, forward).max(1e-3);
        let x = cx + focal * dot(rel, right) / vz;
        let y = cy - focal * dot(rel, up) / vz;
        (
            (x as usize).min(w.saturating_sub(1)),
            (y as usize).min(h.saturating_sub(1)),
        )
    }

    #[test]
    fn dlight_does_not_bleed_into_bsp_region_it_cannot_reach() {
        // End-to-end: render the two-room map with a dlight whose sphere stays
        // inside the near room. The near floor must brighten; the far floor —
        // coplanar, with wall-adjacent luxels inside the light's lightmap-space
        // reach, i.e. visibly lit by the OLD proximity-only gating — must be
        // byte-identical to the unlit frame.
        let bsp = two_rooms_bsp();
        let pal = crate::render::fixtures::ramp_palette();
        // Above and behind the origin, looking down across both rooms.
        let cam = Camera::looking_at([0.0, -220.0, 260.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (200usize, 150usize);

        let base = render_once(&Scene::new(&bsp, cam, w, h, &pal));
        let dl = DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let lit = render_once(&Scene { dlights: std::slice::from_ref(&dl), ..Scene::new(&bsp, cam, w, h, &pal) });

        let px = |img: &Image, p: Vec3| -> u8 {
            let (x, y) = project_px(&cam, w, h, p);
            img.pixels[y * w + x]
        };

        // Near floor under the light brightens.
        assert_ne!(
            px(&base, [40.0, 0.0, 0.0]),
            px(&lit, [40.0, 0.0, 0.0]),
            "near-room floor must brighten under the dlight"
        );
        // Far floor: every sample byte-identical (no light through the wall).
        for sample in [
            [-8.0, 0.0, 0.0],
            [-40.0, 0.0, 0.0],
            [-72.0, 0.0, 0.0],
            [-40.0, 64.0, 0.0],
            [-40.0, -64.0, 0.0],
        ] {
            assert_eq!(
                px(&base, sample),
                px(&lit, sample),
                "far-room floor lit through the wall at {sample:?}"
            );
        }
    }

    #[test]
    fn a_moved_brush_model_is_lit_by_the_lights_where_they_are() {
        // `R_DrawBEntitiesOnList` marks a brush model with `cl_dlights`
        // untranslated (`R_MarkLights (&cl_dlights[k], 1<<k, clmodel->nodes +
        // clmodel->hulls[0].firstclipnode)`) and `R_AddDynamicLights` measures
        // `cl_dlights[lnum].origin` against the face's own plane: a lift
        // lowered 64 units is lit by a light 8 units above where the map put
        // it, and not by one 8 units above where it is now.
        let mut bsp = two_rooms_bsp();
        // The world is the far floor (node 2); model 1, the near floor (x 0..256,
        // z 0, its own subtree node 1), is the lift.
        let model = |firstface, headnode| crate::bsp::DModel {
            mins: [-256.0, -128.0, -16.0],
            maxs: [256.0, 128.0, 16.0],
            origin: [0.0; 3],
            headnode: [headnode, 0, 0, 0],
            visleafs: 0,
            firstface,
            numfaces: 1,
        };
        bsp.models = vec![model(1, 2), model(0, 1)];
        let lift = [crate::render::BModelInstance { model_index: 1, origin: [0.0, 0.0, -64.0], frame: 0, angles: [0.0; 3] }];
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([128.0, -200.0, 200.0], [128.0, 0.0, -64.0], 90.0);
        let (w, h) = (200usize, 150usize);
        let frame = |dl: &[DynamicLight]| {
            render_once(&Scene { bmodels: &lift, dlights: dl, ..Scene::new(&bsp, cam, w, h, &pal) })
        };
        let (x, y) = project_px(&cam, w, h, [128.0, 0.0, -64.0]);
        let at = |img: &Image| img.pixels[y * w + x];
        let dark = frame(&[]);
        let above_the_map = DynamicLight::new([128.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        assert_ne!(at(&frame(&[above_the_map])), at(&dark), "lit as if it had not moved");
        let above_the_lift = DynamicLight::new([128.0, 0.0, -56.0], 36.0, 10.0, 0.0, 0.0, 0);
        assert_eq!(at(&frame(&[above_the_lift])), at(&dark), "56 units from its own plane: unlit");
    }

    /// The brightness -> colormap-row curve must reproduce `R_BuildLightMap`'s
    /// bound/invert/shift exactly (the no-overbright clamp at the top, the
    /// darkest row at the bottom, and the monotone ramp in between).
    /// `R_BuildLightMap`'s bound, invert and shift, from the luxel sum in 8.8:
    /// `luxel * d_lightstylevalue` (256 = a static face, 264 = style 'm').
    #[test]
    fn blocklights_are_r_build_light_map_inverted() {
        let bytes = [100u8, 0, 255, 128];
        let lm = LightMap { luxels: Luxels::Static(&bytes), lmw: 2, lmh: 2, texmins: [0.0, 0.0] };
        let mut out = Vec::new();
        lm.blocklights_into(&mut out);
        // (65280 - 100*256) >> 2 = 9920; 0 -> 16320 (row 63); 255*256 -> 0 -> clamp 64.
        assert_eq!(out, [9920, 16320, 64, (65280 - 128 * 256) >> 2]);
        // Style 'm' (264/256): 100*264 = 26400 -> (65280 - 26400) >> 2 = 9720. Plus
        // a dynamic light's 8.8 step of 1000/256: 27400 -> 9470.
        let owned = vec![100.0 * 264.0 / 256.0, 100.0 * 264.0 / 256.0 + 1000.0 / 256.0, 300.0, 1e9];
        let lm = LightMap { luxels: Luxels::Owned(owned), lmw: 2, lmh: 2, texmins: [0.0, 0.0] };
        lm.blocklights_into(&mut out);
        assert_eq!(out, [9720, 9470, 64, 64]);
    }

    #[test]
    fn colormap_row_matches_quake_curve() {
        // brightness 2.0 (the static fullbright ceiling) and anything brighter
        // collapse to the BRIGHTEST row 0 — Quake's `t >= 64` no-overbright clamp.
        assert_eq!(colormap_row(2.0), 0, "fullbright must be row 0 (no overbright)");
        assert_eq!(colormap_row(3.0), 0, "overbright must clamp to row 0");
        assert_eq!(colormap_row(MAX_LIGHT_FACTOR), 0, "max dynamic light clamps to row 0");

        // brightness 0.0 (fully dark) -> the DARKEST row.
        assert_eq!(colormap_row(0.0), COLORMAP_ROWS - 1, "fully dark must be the last row");

        // The middle: brightness 1.0 -> bl = 32640, t = (65280-32640)>>2 = 8160,
        // row = 8160 >> 8 = 31. This is the exact integer C result.
        assert_eq!(colormap_row(1.0), 31, "neutral brightness must hit row 31");

        // Monotonic: brighter never maps to a darker (larger-index) row, and the
        // row stays inside the table for every factor in the legal range.
        let mut prev = COLORMAP_ROWS; // sentinel above the max row
        let mut b = 0.0f32;
        while b <= MAX_LIGHT_FACTOR + 1e-3 {
            let row = colormap_row(b);
            assert!(row < COLORMAP_ROWS, "row {row} out of table at brightness {b}");
            assert!(row <= prev, "brightness {b} -> row {row} not monotone (prev {prev})");
            prev = row;
            b += 0.05;
        }
    }
}
