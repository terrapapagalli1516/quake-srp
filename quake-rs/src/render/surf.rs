//! Surfaces: face geometry, texture animation, and the per-face caches.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_surf.c` (`R_TextureAnimation`) and `WinQuake/d_surf.c`
//! (the lit surface cache, `D_CacheSurface`); the port adds the static-geometry
//! and lightmap caches the world pass reuses between frames.

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::light::{
    any_dlight_reaches, face_lightmap_dyn, LightMap, Luxels, COLORMAP_LEN,
    LIGHTSTYLES, STYLE_NONE,
};
use super::stats::stat;

/// Reconstruct a face's world-space polygon into `out`. Returns false if any
/// index is out of range (the caller then skips the face). Mirrors the
/// surfedge/edge/vertex walk in [`render_bsp`](super::render_bsp).
pub(super) fn face_world_poly(bsp: &Bsp, face: &crate::bsp::DFace, out: &mut Vec<Vec3>) -> bool {
    out.clear();
    let numedges = face.numedges as i64;
    if numedges < 3 {
        return false;
    }
    let firstedge = face.firstedge as i64;
    if firstedge < 0 {
        return false;
    }
    for i in 0..numedges {
        let se_index: usize = match (firstedge + i).try_into() {
            Ok(x) => x,
            Err(_) => return false,
        };
        let se = match bsp.surfedges.get(se_index) {
            Some(&s) => s,
            None => return false,
        };
        let (edge_index, slot): (usize, usize) = if se >= 0 {
            (se as usize, 0)
        } else {
            match (se as i64).checked_neg() {
                Some(n) if n >= 0 => (n as usize, 1),
                _ => return false,
            }
        };
        let edge = match bsp.edges.get(edge_index) {
            Some(e) => e,
            None => return false,
        };
        let vid = match edge.v.get(slot) {
            Some(&id) => id as usize,
            None => return false,
        };
        match bsp.vertexes.get(vid) {
            Some(v) => out.push(v.point),
            None => return false,
        }
    }
    out.len() >= 3
}

/// Outward face normal from its plane, flipped for back-side faces.
pub(super) fn face_normal(bsp: &Bsp, face: &crate::bsp::DFace) -> Option<Vec3> {
    let pi: usize = (face.planenum as i64).try_into().ok()?;
    let p = bsp.planes.get(pi)?;
    let mut n = p.normal;
    if face.side != 0 {
        n = [-n[0], -n[1], -n[2]];
    }
    Some(n)
}

/// Which animated kind a miptexture name selects: liquids begin with `*`
/// (`*water1`, `*lava1`, `*slime`, `*teleport`), sky begins with `sky`
/// (`sky1`, `sky4`); anything else is an ordinary lightmapped wall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SurfKind {
    Normal,
    Turb,
    Sky,
}

/// Classify a miptexture by its name, exactly as Quake's `Mod_LoadFaces`
/// flags surfaces (`*`-prefixed -> `SURF_DRAWTURB`, `sky`-prefixed ->
/// `SURF_DRAWSKY`). The match is ASCII case-insensitive on the `sky` prefix to
/// tolerate `SKY1`-style names; the `*` check is exact.
pub(super) fn classify_surface(name: &str) -> SurfKind {
    if name.starts_with('*') {
        SurfKind::Turb
    } else if name.len() >= 3 && name.as_bytes()[..3].eq_ignore_ascii_case(b"sky") {
        SurfKind::Sky
    } else {
        SurfKind::Normal
    }
}

/// `R_TextureAnimation` (`r_surf.c`): pick the texture index to draw for an
/// animated (`+`-prefixed) base texture at game `time`, given the drawing
/// entity's `frame` field.
///
/// Returns the index into `bsp.textures` of the frame to render:
///  * If `ent_frame != 0` and the base has an `alternate_anims` cycle, switch to
///    that cycle's first frame as the new base (the `+a..+j` set).
///  * If the (possibly switched) base is not animated (`anim == None`), return it
///    unchanged.
///  * Otherwise walk `anim_next` from the base until the frame whose
///    `[anim_min, anim_max)` window contains `relative = (int)(time*10) %
///    anim_total`, exactly like the C `while (anim_min > rel || anim_max <= rel)`.
///
/// SAFETY: the walk is bounded (the C bounds it at 100 hops; we bound it at the
/// texture count) and every lookup is `.get()`-checked, so a broken cycle in
/// malformed map data returns the last reachable frame instead of looping or
/// panicking (the C `Sys_Error`s).
pub(super) fn texture_animation(bsp: &Bsp, base_index: usize, ent_frame: i32, time: f32) -> usize {
    // Resolve the entity-frame alternate switch first (C: `if (currententity->
    // frame) { if (base->alternate_anims) base = base->alternate_anims; }`).
    let mut idx = base_index;
    if ent_frame != 0 {
        if let Some(Some(tx)) = bsp.textures.get(idx) {
            if let Some(anim) = tx.anim {
                if let Some(alt) = anim.alternate {
                    idx = alt;
                }
            }
        }
    }

    // If not animated, return as-is.
    let anim = match bsp.textures.get(idx) {
        Some(Some(tx)) => match tx.anim {
            Some(a) if a.total > 0 => a,
            _ => return idx,
        },
        _ => return idx,
    };

    // relative = (int)(cl.time*10) % anim_total, in tenths of a second.
    let rel = ((time * 10.0) as i64).rem_euclid(anim.total as i64) as i32;

    // Walk anim_next until rel is inside [anim_min, anim_max).
    let mut cur = idx;
    let mut cur_anim = anim;
    let limit = bsp.textures.len().max(1) + 1;
    let mut count = 0usize;
    while cur_anim.min > rel || cur_anim.max <= rel {
        count += 1;
        if count > limit {
            break; // broken cycle: stop rather than loop (C: Sys_Error).
        }
        cur = cur_anim.next;
        match bsp.textures.get(cur) {
            Some(Some(tx)) => match tx.anim {
                Some(a) => cur_anim = a,
                None => break,
            },
            _ => break,
        }
    }
    cur
}

/// Per-face STATIC geometry for the world model, computed once and reused every
/// frame (the world model never moves, so its faces' polygons, normals,
/// centroids, surface extents and AABBs are frame-invariant). Caching this skips
/// the surfedge/edge/vertex walk, the centroid loop, and `surface_extents` for
/// every visible face on every frame.
#[derive(Clone)]
pub(super) struct FaceGeom {
    /// Reconstructed world-space polygon (same vertices/order `face_world_poly`
    /// produces — so downstream projection/texturing is byte-identical). Behind an
    /// `Rc` so the per-frame cache fetch (`face_geom_cached`, twice per face) is an
    /// O(1) refcount bump, not a deep `Vec` copy — the vertices are immutable once
    /// built.
    pub(super) poly: std::rc::Rc<Vec<Vec3>>,
    /// Outward face normal (`face_normal`), or `None` if the plane was bad.
    pub(super) normal: Option<Vec3>,
    /// Polygon centroid (the exact same accumulate-then-`*1/n` the loop used).
    pub(super) center: Vec3,
    /// World-space AABB of `poly` (for the frustum cull).
    pub(super) mins: Vec3,
    pub(super) maxs: Vec3,
    /// `true` when `face_world_poly` failed (degenerate/out-of-range face); the
    /// loop then skips it exactly as before.
    pub(super) bad: bool,
}

/// The world-model static-geometry cache. Keyed by a cheap world fingerprint so
/// it self-invalidates on a changelevel (face indices are meaningless after the
/// BSP is swapped). `geoms[i]` is lazily filled the first time face `i` is
/// reached; `Frustum` AABBs read from it.
pub(super) struct GeomCache {
    fingerprint: WorldFingerprint,
    geoms: Vec<Option<FaceGeom>>,
}

/// A cheap identity for the loaded world. A changelevel always re-parses the BSP
/// (new `faces`/`lighting`/… lengths AND a fresh `&Bsp` address), so a mismatch
/// reliably means "different world -> the face-index-keyed caches are stale and
/// must be cleared". We combine the `&Bsp` pointer with several lump lengths so
/// the key changes if EITHER the address differs (the common case: a new map)
/// OR the lengths differ (guards against an allocator reusing a freed address
/// for a different-but-same-pointer Bsp). A face index is meaningless across a
/// world swap, so any mismatch forces a full cache rebuild.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct WorldFingerprint {
    ptr: usize,
    faces_len: usize,
    lighting_len: usize,
    planes_len: usize,
    vertexes_len: usize,
}

impl WorldFingerprint {
    pub(super) fn of(bsp: &Bsp) -> WorldFingerprint {
        WorldFingerprint {
            ptr: bsp as *const Bsp as usize,
            faces_len: bsp.faces.len(),
            lighting_len: bsp.lighting.len(),
            planes_len: bsp.planes.len(),
            vertexes_len: bsp.vertexes.len(),
        }
    }
}

/// One cached face lightmap (Quake's `R_BuildLightMap` surface cache entry).
///
/// We cache ONLY the owned, combined-`f32` case (multi-style / non-neutral
/// scale, and NOT touched by a dynamic light): a single steady style-0 face at
/// neutral scale keeps borrowing the static bytes (no cache needed, already
/// byte-identical). The stored `luxels` are the exact buffer
/// `face_lightmap_dyn` produced, so reusing them is bit-for-bit identical to
/// rebuilding.
#[derive(Clone)]
struct LightCacheEntry {
    /// The resolved per-active-style SCALE values at build time (in stored slot
    /// order). The cache is valid only while these are bit-identical — torches
    /// animate at 10 Hz, so at 60 fps the scales are unchanged ~5/6 frames.
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// The cached combined luxel grid (no dynamic light folded in — see the
    /// "dlight_touched" gating in the wrapper).
    luxels: Vec<f32>,
    lmw: usize,
    lmh: usize,
    texmins: [f32; 2],
}

/// The lightmap surface cache, keyed by world fingerprint (so it clears on a
/// changelevel) + per-face style scales (so it rebuilds when a torch ticks).
pub(super) struct LightCache {
    fingerprint: WorldFingerprint,
    entries: Vec<Option<LightCacheEntry>>,
}

/// One cached lit SURFACE block (Quake's `d_surf.c` surface cache entry): the
/// face's texture at one mip level with the lightmap shaded in AND resolved
/// through the colormap to a final palette index, one byte per texel of that
/// level. The rasteriser then reads a single byte per screen pixel (then one
/// palette lookup) instead of sampling the texture, the lightmap and the
/// colormap per pixel — all of that is a per-texel bake done ONCE and reused
/// every frame.
#[derive(Clone)]
struct SurfCacheEntry {
    /// Active styles' resolved scale values at bake time (the cache key, same as the
    /// lightmap cache — a torch tick rebuilds the block).
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// The texture baked in (`cache->texture`): an animated wall's frame index
    /// into `bsp.textures`, so the next animation frame rebuilds the block.
    texture: usize,
    /// Baked with dynamic light folded in (`cache->dlight`): never reused, so the
    /// first frame the light is gone rebuilds the block without it.
    dlight: bool,
    /// Baked palette indices, `bw * bh`, row-major. `Rc` so a frame's draw clones
    /// the handle (a refcount bump), not the (possibly large) buffer.
    block: std::rc::Rc<Vec<u8>>,
    bw: usize,
    bh: usize,
}

/// The world model's lit-surface cache: its [`WorldFingerprint`] identity plus
/// one slot per face per mip level (`surface->cachespots[miplevel]`), each
/// rebuilt when an animated style ticks. Held as a single [`SURF_CACHE`] slot —
/// only the world `Bsp` is ever cached (external brush models bypass it), so it
/// self-invalidates on a changelevel via the fingerprint/`n_faces` check; see
/// [`face_surf_block`]. Unlike id's fixed-size cache with its LRU rover
/// (`D_SCAlloc`), nothing is evicted until the level changes: eviction only ever
/// costs id a rebake, never a different pixel.
pub(super) struct SurfCache {
    fingerprint: WorldFingerprint,
    entries: Vec<[Option<SurfCacheEntry>; NUM_MIPS]>,
}

thread_local! {
    /// Per-thread world static-geometry cache (one world at a time).
    pub(super) static GEOM_CACHE: std::cell::RefCell<Option<GeomCache>> = const { std::cell::RefCell::new(None) };
    /// Per-thread lightmap surface cache.
    pub(super) static LIGHT_CACHE: std::cell::RefCell<Option<LightCache>> = const { std::cell::RefCell::new(None) };
    /// Per-thread lit-surface (texel) cache for the world model (+ its inline
    /// submodels, which share the world `Bsp`). External brush models bypass it
    /// (they re-clone their `Bsp` every frame). See [`face_surf_block`].
    pub(super) static SURF_CACHE: std::cell::RefCell<Option<SurfCache>> = const { std::cell::RefCell::new(None) };
    /// The renderer's `d_mipscale` / `d_mipcap` (see [`set_mip_cvars`]).
    static MIP_CVARS: std::cell::Cell<MipCvars> = const { std::cell::Cell::new(MipCvars::DEFAULT) };
}

/// The bytes the lit-surface cache holds now (every baked block of every face at
/// every mip level), and the number of blocks: the port's counterpart of id's
/// fixed `D_SurfaceCacheForRes` pool, for measurement.
pub fn surface_cache_usage() -> (usize, usize) {
    SURF_CACHE.with(|c| {
        c.borrow().as_ref().map_or((0, 0), |sc| {
            sc.entries.iter().flatten().flatten().fold((0, 0), |(bytes, n), e| (bytes + e.block.len(), n + 1))
        })
    })
}

// ---------------------------------------------------------------------------
// Mip levels: D_MipLevelForScale (d_edge.c), D_SetupFrame (d_init.c)
// ---------------------------------------------------------------------------

/// `NUM_MIPS` (`d_init.c`): a miptex stores four levels.
pub(super) const NUM_MIPS: usize = 4;

/// `basemip` (`d_init.c`): the scales below which a surface drops to mip 1, 2
/// and 3 — `{1.0, 0.5*0.8, 0.25*0.8}`, as the C's `float`s.
const BASEMIP: [f32; NUM_MIPS - 1] = [1.0, 0.4, 0.2];

/// The renderer's two mip cvars (`d_init.c`), with id's defaults: `d_mipscale`
/// 1 multiplies the `basemip` thresholds (0 puts every surface at mip 0, as a
/// scale is never below 0), and `d_mipcap` 0 is the finest level allowed
/// (`d_minmip`; 3 draws everything at the coarsest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MipCvars {
    pub mipscale: f32,
    pub mipcap: f32,
}

impl MipCvars {
    pub const DEFAULT: MipCvars = MipCvars { mipscale: 1.0, mipcap: 0.0 };
}

impl Default for MipCvars {
    fn default() -> MipCvars {
        MipCvars::DEFAULT
    }
}

/// Set `d_mipscale` / `d_mipcap` for the frames this thread renders from now on
/// (the C reads the cvars in `D_SetupFrame`, every frame). The default is id's.
pub fn set_mip_cvars(c: MipCvars) {
    MIP_CVARS.with(|m| m.set(c));
}

/// The current `d_mipscale` / `d_mipcap`.
pub fn mip_cvars() -> MipCvars {
    MIP_CVARS.with(|m| m.get())
}

/// `NEAR_CLIP` (`r_local.h`): `R_EmitEdge` clamps a vertex's view depth to at
/// least this before taking `1/z`.
const NEAR_CLIP: f32 = 0.01;

/// What `D_DrawSurfaces` needs to pick a surface's mip level, set up once per
/// frame and pass: `D_SetupFrame`'s `d_scalemip`/`d_minmip`, `D_ViewChanged`'s
/// `scale_for_mip`, and the view frustum's four side planes `R_ClipEdge` clips
/// the edges against before `R_EmitEdge` records the nearest `1/z`.
pub(super) struct MipView {
    scalemip: [f32; NUM_MIPS - 1],
    minmip: u32,
    scale_for_mip: f32,
    /// The side planes as slopes: a view-space point is inside iff
    /// `|vx| <= tan_x * vz` and `|vy| <= tan_y * vz`.
    tan_x: f32,
    tan_y: f32,
    /// Clip scratch (reused across faces).
    a: Vec<[f32; 3]>,
    b: Vec<[f32; 3]>,
}

impl MipView {
    /// For a view whose screen centre is `(cx, cy)` pixels from its edges, with
    /// `x = cx + focal_x*vx/vz`, `y = cy - focal_y*vy/vz`. `scale_for_mip` is the
    /// larger focal length (`xscale`, or `yscale` when the pixels are taller than
    /// wide); the frustum's sides run through the view rectangle's edges, as
    /// `R_ViewChanged`'s `screenedge` planes do.
    pub(super) fn new(cx: f32, cy: f32, focal_x: f32, focal_y: f32) -> MipView {
        let cv = mip_cvars();
        // d_minmip = d_mipcap.value (float to int truncates), clamped to 0..3.
        let minmip = (cv.mipcap as i32).clamp(0, NUM_MIPS as i32 - 1) as u32;
        MipView {
            scalemip: BASEMIP.map(|b| b * cv.mipscale),
            minmip,
            scale_for_mip: if focal_y > focal_x { focal_y } else { focal_x },
            tan_x: cx / focal_x,
            tan_y: cy / focal_y,
            a: Vec::new(),
            b: Vec::new(),
        }
    }

    /// `D_MipLevelForScale` (`d_edge.c`): 0 at or above `d_scalemip[0]`, 1 above
    /// `[1]`, 2 above `[2]`, else 3; never finer than `d_minmip`.
    pub(super) fn level_for_scale(&self, scale: f32) -> u32 {
        let level = if scale >= self.scalemip[0] {
            0
        } else if scale >= self.scalemip[1] {
            1
        } else if scale >= self.scalemip[2] {
            2
        } else {
            3
        };
        level.max(self.minmip)
    }

    /// The mip level `D_DrawSurfaces` draws a face at: `D_MipLevelForScale(
    /// nearzi * scale_for_mip * mipadjust)`. `views` is the face's polygon in
    /// view space (`x` right, `y` up, `z` forward — unclipped); `ti` its texinfo.
    pub(super) fn level_for_face(&mut self, views: &[super::vis::VView], ti: &crate::bsp::TexInfo) -> u32 {
        let nearzi = self.nearzi(views);
        self.level_for_scale(nearzi * self.scale_for_mip * mipadjust(ti))
    }

    /// `surf->nearzi`: the largest `1/z` among the vertices of the polygon
    /// clipped to the four side planes of the frustum, each `z` clamped to
    /// [`NEAR_CLIP`] — what `R_RenderFace` gathers as `R_EmitEdge` projects the
    /// clipped edges (the left clip edge, and the right one for its `1/z` only,
    /// included). 0 when nothing is left (the face covers no pixel).
    fn nearzi(&mut self, views: &[super::vis::VView]) -> f32 {
        let (tx, ty) = (self.tan_x, self.tan_y);
        self.a.clear();
        self.a.extend(views.iter().map(|v| [v.vx, v.vy, v.vz]));
        // The side planes, right, left, top and bottom, as (axis, sign, slope):
        // a point's distance inside is `slope * z + sign * p[axis]`.
        for (axis, sign, slope) in [(0, -1.0f32, tx), (0, 1.0, tx), (1, -1.0, ty), (1, 1.0, ty)] {
            let d = |p: &[f32; 3]| slope * p[2] + sign * p[axis];
            self.b.clear();
            let n = self.a.len();
            for i in 0..n {
                let (p, q) = (self.a[i], self.a[(i + 1) % n]);
                let (dp, dq) = (d(&p), d(&q));
                if dp >= 0.0 {
                    self.b.push(p);
                }
                if (dp >= 0.0) != (dq >= 0.0) {
                    let f = dp / (dp - dq);
                    self.b.push([p[0] + f * (q[0] - p[0]), p[1] + f * (q[1] - p[1]), p[2] + f * (q[2] - p[2])]);
                }
            }
            std::mem::swap(&mut self.a, &mut self.b);
            if self.a.is_empty() {
                return 0.0;
            }
        }
        self.a.iter().fold(0.0f32, |m, p| m.max(1.0 / p[2].max(NEAR_CLIP)))
    }
}

/// `mipadjust` (`Mod_LoadTexinfo`, `model.c`): how many texels a world unit
/// spans on this texinfo, in steps — the mean length of the two texture axes
/// below 0.32 gives 4, below 0.49 3, below 0.99 2, else 1. A texture scaled up
/// in the editor (short axes) drops to a coarser mip sooner.
pub(super) fn mipadjust(ti: &crate::bsp::TexInfo) -> f32 {
    let len = |k: usize| {
        let v = &ti.vecs[k];
        (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
    };
    let l = ((len(0) + len(1)) / 2.0) as f64;
    if l < 0.32 {
        4.0
    } else if l < 0.49 {
        3.0
    } else if l < 0.99 {
        2.0
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// The surface cache: D_CacheSurface (d_surf.c), R_DrawSurface (r_surf.c)
// ---------------------------------------------------------------------------

/// Maximum baked surface-cache block, in texels. A face larger than this stays on
/// the per-pixel lighting path, so one pathological giant surface can't allocate a
/// multi-MB block; id's `CalcSurfaceExtents` refuses any lightmapped face over
/// 256 texels a side, far below this.
const SURF_BLOCK_MAX: usize = 1 << 20;

/// A baked surface block ([`face_surf_block`]): the palette indices, `bw * bh`
/// row-major, at mip level `mip`, whose texel `(i, j)` is the surface's
/// `(texmins[0] + i, texmins[1] + j)` in that level's texels. The span walker
/// reads it through gradients scaled to the level
/// ([`PolyGrads::mip_scaled`](super::raster::PolyGrads::mip_scaled)), as
/// `D_CalcGradients` scales its steps by `mipscale`.
pub(super) struct SurfBlock {
    pub(super) block: std::rc::Rc<Vec<u8>>,
    pub(super) bw: usize,
    pub(super) bh: usize,
    pub(super) texmins: [f32; 2],
    pub(super) mip: u32,
}

/// Build (and cache) a face's lit+colormapped surface block at mip level
/// `miplevel`: `D_CacheSurface`. Returns the block, or `None` (the caller keeps
/// the per-pixel path) when there is no usable colormap, the texture is missing,
/// or the block would be empty or exceed [`SURF_BLOCK_MAX`]. A texture without
/// its levels 1..3 (the synthetic ones in tests) is baked at mip 0.
///
/// `tex_index` is the (animated) texture's index in `bsp.textures`, and `lm` the
/// face's lightmap for this frame, dynamic lights included when `dlit` (a light
/// reaches the face, [`any_dlight_reaches`]). A dynamically lit face is baked like
/// any other — `R_BuildLightMap` runs `R_AddDynamicLights`, then `R_DrawSurface`
/// — and its entry marked `dlight`, as the C marks `cache->dlight`: a dlit block
/// is never a hit, so the light is rebaked every frame it is live and the first
/// frame after it dies rebuilds the block without it. The hit test is the C's:
/// same texture, same style values, no dlight now or at the bake — per face and
/// mip level (`surface->cachespots[miplevel]`).
///
/// The bake is `R_DrawSurface`: the block is `extents >> miplevel` texels a side
/// (`surfwidth`), made of one `16 >> miplevel` square per pair of lightmap
/// columns and rows, each lit by `R_DrawSurfaceBlock8_mip0..3`'s integer
/// interpolation ([`draw_surface_block`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn face_surf_block(
    idx: usize,
    face: &crate::bsp::DFace,
    tex_index: usize,
    mt: &crate::bsp::MipTex,
    lm: &LightMap,
    colormap: &[u8],
    fp: WorldFingerprint,
    n_faces: usize,
    light_styles: &[f32; LIGHTSTYLES],
    dlit: bool,
    cache_surf: bool,
    miplevel: u32,
) -> Option<SurfBlock> {
    if colormap.len() < COLORMAP_LEN {
        return None;
    }
    let mip = if (miplevel as usize) < NUM_MIPS && mt.mip(miplevel as usize).is_some() { miplevel } else { 0 };
    let tex = mt.mip(mip as usize)?;
    let (smax, tmax) = ((mt.width as usize) >> mip, (mt.height as usize) >> mip);
    // `surfwidth = extents[0] >> miplevel`; `extents = (lmw - 1) * 16`.
    let bw = lm.lmw.saturating_sub(1).saturating_mul(16) >> mip;
    let bh = lm.lmh.saturating_sub(1).saturating_mul(16) >> mip;
    let total = bw.checked_mul(bh)?;
    if smax == 0 || tmax == 0 || bw == 0 || bh == 0 || total > SURF_BLOCK_MAX {
        return None;
    }
    // Cache key: the active styles' resolved scales (matching the lightmap cache).
    let mut scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
    let mut n_styles = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        if n_styles < scales.len() {
            scales[n_styles] = light_styles.get(style as usize).copied().unwrap_or(1.0);
        }
        n_styles += 1;
    }

    // texturemins are whole multiples of 16, so `>> mip` is exact.
    let texmins_i = [lm.texmins[0] as i32, lm.texmins[1] as i32];
    let texmins = [(texmins_i[0] >> mip) as f32, (texmins_i[1] >> mip) as f32];
    let bake = || -> std::rc::Rc<Vec<u8>> {
        stat(|s| s.surf_texels_baked += total as u64);
        let mut light = Vec::new();
        lm.blocklights_into(&mut light);
        let mut block = vec![0u8; total];
        draw_surface_block(tex, smax, tmax, texmins_i, mip, &light, lm.lmw, colormap, &mut block, bw, bh);
        std::rc::Rc::new(block)
    };
    let made = |block| SurfBlock { block, bw, bh, texmins, mip };

    // EXTERNAL brush models (the b_*.bsp ammo/health/explosive boxes) bypass the
    // cache. The game clones each item's `Bsp` per visible instance every frame, so
    // its `WorldFingerprint` (keyed on the `&Bsp` pointer) is different every frame
    // and every instance — it could never produce a cache hit, and routing it
    // through the shared slot would only evict the world's resident cache (the bug
    // this guard prevents). They are tiny (a 6-face box) so an unconditional bake is
    // cheap; bake fresh and return without touching SURF_CACHE.
    if !cache_surf {
        stat(|s| s.surf_bypass_baked += 1);
        return Some(made(bake()));
    }

    // CACHED path — the world model and its inline submodels (doors/plats/buttons)
    // all share the one world `Bsp`, so exactly one fingerprint is ever cached at a
    // time. A single slot therefore suffices: it self-invalidates on a changelevel
    // (the new world's `fp` / `n_faces` differ) and holds at most one world's worth
    // of baked blocks, so memory never accumulates across levels.
    SURF_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        let needs_reset = match slot.as_ref() {
            Some(sc) => sc.fingerprint != fp || sc.entries.len() != n_faces,
            None => true,
        };
        if needs_reset {
            *slot = Some(SurfCache {
                fingerprint: fp,
                entries: vec![Default::default(); n_faces],
            });
        }
        let sc = slot.as_mut().expect("just initialised");
        let spot = sc.entries.get_mut(idx).map(|spots| &mut spots[mip as usize]);
        // HIT (`D_CacheSurface`): no dynamic light now or in the bake, same
        // texture, same resolved style scales -> reuse the baked block.
        if let Some(Some(e)) = spot.as_deref() {
            if !dlit
                && !e.dlight
                && e.texture == tex_index
                && e.n_styles == n_styles
                && e.bw == bw
                && e.bh == bh
                && e.style_scales[..n_styles] == scales[..n_styles]
            {
                stat(|s| s.surf_cache_hits += 1);
                return Some(made(e.block.clone()));
            }
        }
        // MISS: bake and store, marked `dlight` when a light is folded in.
        stat(|s| s.surf_baked += 1);
        let block = bake();
        if let Some(e) = spot {
            *e = Some(SurfCacheEntry {
                style_scales: scales,
                n_styles,
                texture: tex_index,
                dlight: dlit,
                block: block.clone(),
                bw,
                bh,
            });
        }
        Some(made(block))
    })
}

/// `R_DrawSurface` with `R_DrawSurfaceBlock8_mip0..3` (`r_surf.c`): fill `out`
/// (`bw x bh`, the surface at mip level `mip`) from the level's texture (`tex`,
/// `smax x tmax`, tiled from `texturemins >> mip`) and the face's inverted
/// `blocklights` (`light`, `lmw` wide; [`LightMap::blocklights_into`]).
///
/// The surface is `bw >> (4 - mip)` by `bh >> (4 - mip)` blocks of `16 >> mip`
/// texels, one per lightmap cell. Down each block's left and right edges the
/// light steps from the top luxel toward the bottom one by `(bottom - top) >>
/// (4 - mip)` per row (`lightleftstep`, `lightrightstep`); along each row it
/// starts at the RIGHT edge's value and steps by `(left - right) >> (4 - mip)`
/// per texel toward the left (`lightstep`), all in integers with arithmetic
/// shifts. A texel is `colormap[(light & 0xFF00) + texel]`.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_surface_block(
    tex: &[u8],
    smax: usize,
    tmax: usize,
    texmins: [i32; 2],
    mip: u32,
    light: &[i32],
    lmw: usize,
    colormap: &[u8],
    out: &mut [u8],
    bw: usize,
    bh: usize,
) {
    let blocksize = 16usize >> mip;
    let shift = 4 - mip;
    let (nh, nv) = (bw >> shift, bh >> shift);
    if smax == 0 || tmax == 0 || tex.len() < smax * tmax || out.len() < bw * bh || colormap.len() < COLORMAP_LEN {
        return;
    }
    // `soffset`/`basetoffset`: where the surface's first texel falls in the tiled
    // texture ("+ (smax << 16)" in the C only keeps the % positive).
    let soffset = (texmins[0] >> mip).rem_euclid(smax as i32) as usize;
    let toffset = (texmins[1] >> mip).rem_euclid(tmax as i32) as usize;
    let lux = |x: usize, y: usize| light.get(y * lmw + x).copied().unwrap_or(1 << 6);
    for v in 0..nv {
        for u in 0..nh {
            // r_lightptr[0], r_lightptr[1], and the same one lightmap row down.
            let mut lightleft = lux(u, v);
            let mut lightright = lux(u + 1, v);
            let lightleftstep = (lux(u, v + 1) - lightleft) >> shift;
            let lightrightstep = (lux(u + 1, v + 1) - lightright) >> shift;
            // The block's first texture column (the C wraps `soffset` a block at a
            // time; id's textures are 16-aligned, so this is the same column).
            let s0 = (soffset + u * blocksize) % smax;
            for i in 0..blocksize {
                let y = v * blocksize + i;
                let trow = (toffset + y) % tmax * smax;
                let src = &tex[trow..trow + smax];
                let dst = &mut out[y * bw + u * blocksize..y * bw + (u + 1) * blocksize];
                let lightstep = (lightleft - lightright) >> shift;
                let mut l = lightright;
                // 0 < l <= 16320: the luxels are clamped to 64..=16320 and the
                // floor steps overshoot the lower one by less than 15 per edge,
                // so the index stays inside the 64 x 256 colormap.
                if s0 + blocksize <= smax {
                    let seg = &src[s0..s0 + blocksize];
                    for b in (0..blocksize).rev() {
                        dst[b] = colormap[(l & 0xFF00) as usize + seg[b] as usize];
                        l += lightstep;
                    }
                } else {
                    for b in (0..blocksize).rev() {
                        dst[b] = colormap[(l & 0xFF00) as usize + src[(s0 + b) % smax] as usize];
                        l += lightstep;
                    }
                }
                lightright += lightrightstep;
                lightleft += lightleftstep;
            }
        }
    }
}

/// Compute (and cache) a world-model face's static geometry. Returns a clone of
/// the cached [`FaceGeom`]. The cache is reset whenever the world fingerprint
/// changes (changelevel). `face` must be the corresponding `bsp.faces[idx]`.
pub(super) fn face_geom_cached(bsp: &Bsp, idx: usize, face: &crate::bsp::DFace) -> FaceGeom {
    let fp = WorldFingerprint::of(bsp);
    GEOM_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        // (Re)initialise the cache on first use or a world change.
        let needs_reset = match slot.as_ref() {
            Some(gc) => gc.fingerprint != fp || gc.geoms.len() != bsp.faces.len(),
            None => true,
        };
        if needs_reset {
            *slot = Some(GeomCache {
                fingerprint: fp,
                geoms: vec![None; bsp.faces.len()],
            });
        }
        let gc = slot.as_mut().expect("just initialised");
        if let Some(existing) = gc.geoms.get(idx).and_then(|g| g.clone()) {
            return existing;
        }
        // Build it once.
        let mut poly: Vec<Vec3> = Vec::new();
        let ok = face_world_poly(bsp, face, &mut poly);
        let geom = if !ok {
            FaceGeom {
                poly: std::rc::Rc::new(Vec::new()),
                normal: None,
                center: [0.0; 3],
                mins: [0.0; 3],
                maxs: [0.0; 3],
                bad: true,
            }
        } else {
            let normal = face_normal(bsp, face);
            // Centroid: identical accumulate-then-scale to the inline loop.
            let mut center = [0.0f32; 3];
            for v in &poly {
                for k in 0..3 {
                    center[k] += v[k];
                }
            }
            let inv_n = 1.0 / poly.len() as f32;
            for c in &mut center {
                *c *= inv_n;
            }
            // World AABB for the frustum cull.
            let mut mins = [f32::INFINITY; 3];
            let mut maxs = [f32::NEG_INFINITY; 3];
            for v in &poly {
                for k in 0..3 {
                    if v[k] < mins[k] {
                        mins[k] = v[k];
                    }
                    if v[k] > maxs[k] {
                        maxs[k] = v[k];
                    }
                }
            }
            FaceGeom {
                poly: std::rc::Rc::new(poly),
                normal,
                center,
                mins,
                maxs,
                bad: false,
            }
        };
        if let Some(g) = gc.geoms.get_mut(idx) {
            *g = Some(geom.clone());
        }
        geom
    })
}

/// The lightmap for a world-model face, going through the surface cache.
///
/// Behaviour, by case:
///  * **Static borrow** (`face_lightmap_dyn` returns `Luxels::Static`, the
///    common steady style-0-at-neutral case) — returned as-is, no caching: it
///    already borrows the BSP bytes and is byte-identical to before.
///  * **Dynamic light reaches the face** — rebuilt EVERY frame via
///    `face_lightmap_dyn` (dlights move). The result is NOT stored, and any
///    previously cached entry for this face is dropped, so the dlight is never
///    silently lost on a later frame.
///  * **Owned combine, no dlight** (animated styles) — keyed by the resolved
///    style scale values. On a hit with matching scales the cached `Vec<f32>` is
///    cloned into a fresh `LightMap` (bit-identical to a rebuild — the combine
///    is deterministic). On a miss it is rebuilt and stored.
///
/// The cache is reset on a world fingerprint change (changelevel), since it is
/// keyed by face index.
pub(super) fn face_lightmap_world_cached<'a>(
    bsp: &'a Bsp,
    idx: usize,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    // The face's `R_MarkLights` mask for this frame (see [`mark_dlights`]).
    dlightbits: u32,
) -> Option<LightMap<'a>> {
    // Resolve the active styles' SCALE values (cache key) once.
    let mut scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
    let mut n_styles = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        scales[n_styles] = light_styles.get(style as usize).copied().unwrap_or(1.0);
        n_styles += 1;
    }

    let dlit = any_dlight_reaches(bsp, face, dlights, dlightbits);

    let fp = WorldFingerprint::of(bsp);

    if dlit {
        // A dlight touches this face: rebuild every frame and DROP any cached
        // entry (so we never reuse a stale, dlight-free buffer next frame, and
        // never bake a moving dlight into the cache).
        LIGHT_CACHE.with(|c| {
            let mut slot = c.borrow_mut();
            if let Some(lc) = slot.as_mut() {
                if lc.fingerprint == fp {
                    if let Some(e) = lc.entries.get_mut(idx) {
                        *e = None;
                    }
                }
            }
        });
        return face_lightmap_dyn(bsp, face, world_poly, light_styles, dlights, dlightbits);
    }

    // No dlight: try the cache.
    let cached = LIGHT_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        let needs_reset = match slot.as_ref() {
            Some(lc) => lc.fingerprint != fp || lc.entries.len() != bsp.faces.len(),
            None => true,
        };
        if needs_reset {
            *slot = Some(LightCache {
                fingerprint: fp,
                entries: vec![None; bsp.faces.len()],
            });
        }
        let lc = slot.as_mut().expect("just initialised");
        match lc.entries.get(idx).and_then(|e| e.as_ref()) {
            Some(e)
                if e.n_styles == n_styles
                    && e.style_scales[..n_styles] == scales[..n_styles] =>
            {
                // HIT: clone the stored combined luxels (deterministic build ->
                // bit-identical to rebuilding).
                Some(LightCacheEntry {
                    style_scales: e.style_scales,
                    n_styles: e.n_styles,
                    luxels: e.luxels.clone(),
                    lmw: e.lmw,
                    lmh: e.lmh,
                    texmins: e.texmins,
                })
            }
            _ => None,
        }
    });

    if let Some(e) = cached {
        return Some(LightMap {
            luxels: Luxels::Owned(e.luxels),
            lmw: e.lmw,
            lmh: e.lmh,
            texmins: e.texmins,
        });
    }

    // MISS: build fresh (no dlights -> the result is the pure static/style
    // combine), then cache it if it is the owned combine.
    let built = face_lightmap_dyn(bsp, face, world_poly, light_styles, &[], 0)?;
    if let Luxels::Owned(ref v) = built.luxels {
        let mut style_scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
        style_scales[..n_styles].copy_from_slice(&scales[..n_styles]);
        let entry = LightCacheEntry {
            style_scales,
            n_styles,
            luxels: v.clone(),
            lmw: built.lmw,
            lmh: built.lmh,
            texmins: built.texmins,
        };
        LIGHT_CACHE.with(|c| {
            let mut slot = c.borrow_mut();
            if let Some(lc) = slot.as_mut() {
                if lc.fingerprint == fp {
                    if let Some(e) = lc.entries.get_mut(idx) {
                        *e = Some(entry);
                    }
                }
            }
        });
    }
    Some(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlight::DynamicLight;
    use crate::render::{demo_room, render_scene_ext, Camera};
    use crate::render::fixtures::{
        lightmapped_demo_room, one_face_bsp_zplane, reset_render_caches, two_style_face_bsp,
    };
    use crate::render::light::{ALL_DLIGHT_BITS, NEUTRAL_LIGHTSTYLE_SCALES};
    use crate::render::stats::{render_stats_begin, render_stats_end};
    use crate::render::world::ExternalBModel;

    #[test]
    fn texture_animation_selects_frame_by_time() {
        // FIX 1: a parsed BSP with a 2-frame `+`-animated cycle selects the frame
        // by (int)(time*10) % anim_total via R_TextureAnimation.
        use crate::bsp::{MipTex, TexAnim};
        let mut bsp = demo_room();
        let mk = |name: &str, anim: Option<TexAnim>| MipTex {
            name: name.into(),
            width: 16,
            height: 16,
            offsets: [0, 0, 0, 0],
            pixels: vec![0u8; 16 * 16],
            mips: Default::default(),
            anim,
        };
        // Two-frame primary cycle: frame 0 (idx 0) and frame 1 (idx 1), ANIM_CYCLE=2.
        // anim_total = 2*2 = 4 tenths. Frame 0 covers rel in [0,2), frame 1 [2,4).
        bsp.textures = vec![
            Some(mk("+0wat", Some(TexAnim { total: 4, min: 0, max: 2, next: 1, alternate: None }))),
            Some(mk("+1wat", Some(TexAnim { total: 4, min: 2, max: 4, next: 0, alternate: None }))),
        ];

        // time=0.0 -> rel=0 -> frame 0 (index 0).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.0), 0);
        // time=0.1 -> rel=1 -> still frame 0.
        assert_eq!(texture_animation(&bsp, 0, 0, 0.1), 0);
        // time=0.2 -> rel=2 -> frame 1 (index 1).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.2), 1);
        // time=0.4 -> rel=0 (wraps) -> frame 0.
        assert_eq!(texture_animation(&bsp, 0, 0, 0.4), 0);
        // Starting the walk from frame 1's index resolves the same frame for a
        // given time (R_TextureAnimation walks anim_next from any base).
        assert_eq!(texture_animation(&bsp, 1, 0, 0.0), 0);
        assert_eq!(texture_animation(&bsp, 1, 0, 0.2), 1);

        // A non-animated texture returns its own index unchanged.
        bsp.textures.push(Some(mk("brick", None)));
        assert_eq!(texture_animation(&bsp, 2, 0, 5.0), 2);
        // An out-of-range base index returns it unchanged (no panic).
        assert_eq!(texture_animation(&bsp, 99, 0, 1.0), 99);
    }

    #[test]
    fn texture_animation_entity_frame_selects_alternate() {
        // FIX 1: with the drawing entity's frame != 0, switch to the alternate
        // cycle (R_TextureAnimation `if (currententity->frame) base = alternate`).
        use crate::bsp::{MipTex, TexAnim};
        let mut bsp = demo_room();
        let mk = |name: &str, anim: TexAnim| MipTex {
            name: name.into(),
            width: 16,
            height: 16,
            offsets: [0, 0, 0, 0],
            pixels: vec![0u8; 16 * 16],
            mips: Default::default(),
            anim: Some(anim),
        };
        // Primary frame at index 0 with alternate -> index 1 (a single-frame alt).
        bsp.textures = vec![
            Some(mk("+0sw", TexAnim { total: 2, min: 0, max: 2, next: 0, alternate: Some(1) })),
            Some(mk("+asw", TexAnim { total: 2, min: 0, max: 2, next: 1, alternate: Some(0) })),
        ];
        // ent_frame 0 -> primary (index 0).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.0), 0);
        // ent_frame != 0 -> alternate cycle's first frame (index 1).
        assert_eq!(texture_animation(&bsp, 0, 1, 0.0), 1);
    }

    // -- Animated special surfaces: turbulent liquid warp + scrolling sky -----

    #[test]
    fn surface_classification_by_name() {
        // Liquids begin with '*'; sky begins with 'sky' (case-insensitive);
        // everything else is an ordinary wall.
        assert_eq!(classify_surface("*water1"), SurfKind::Turb);
        assert_eq!(classify_surface("*lava1"), SurfKind::Turb);
        assert_eq!(classify_surface("*slime"), SurfKind::Turb);
        assert_eq!(classify_surface("*teleport"), SurfKind::Turb);
        assert_eq!(classify_surface("sky1"), SurfKind::Sky);
        assert_eq!(classify_surface("sky4"), SurfKind::Sky);
        assert_eq!(classify_surface("SKY1"), SurfKind::Sky);
        assert_eq!(classify_surface("wall_brick"), SurfKind::Normal);
        assert_eq!(classify_surface("city4_7"), SurfKind::Normal);
        // Short / edge-case names never panic and default sensibly.
        assert_eq!(classify_surface(""), SurfKind::Normal);
        assert_eq!(classify_surface("sk"), SurfKind::Normal);
        assert_eq!(classify_surface("*"), SurfKind::Turb);
    }

    // -- Lightmap surface cache --------------------------------------------

    #[test]
    fn lightmap_cache_returns_bit_identical_luxels_for_same_style_key() {
        reset_render_caches();
        // A 2-style face: styles[0]=0 (steady), styles[1]=1 (animated). The owned
        // combine is cacheable (not a static borrow).
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;

        // First call: MISS -> builds + caches. Second call (same key): HIT.
        let first =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &scales, &[], 0).expect("present");
        let second =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &scales, &[], 0).expect("present");

        // The cached luxels must be BIT-identical to a fresh, cache-free build.
        let fresh = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
        let (lf, ls, lfresh) = match (&first.luxels, &second.luxels, &fresh.luxels) {
            (Luxels::Owned(a), Luxels::Owned(b), Luxels::Owned(c)) => (a, b, c),
            _ => panic!("a 2-style face must own the combined buffer"),
        };
        assert_eq!(lf.len(), lfresh.len());
        for i in 0..lf.len() {
            assert_eq!(lf[i].to_bits(), lfresh[i].to_bits(), "first build vs fresh luxel {i}");
            assert_eq!(ls[i].to_bits(), lfresh[i].to_bits(), "cached HIT vs fresh luxel {i}");
        }
        assert_eq!((second.lmw, second.lmh), (fresh.lmw, fresh.lmh));
        assert_eq!(second.texmins, fresh.texmins);
    }

    #[test]
    fn lightmap_cache_rebuilds_when_style_key_changes() {
        reset_render_caches();
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);

        // Build at scale 0.5, then again at scale 1.0 (a torch ticking). The
        // second result must reflect the NEW scale, not the stale cached one.
        let mut s_half = NEUTRAL_LIGHTSTYLE_SCALES;
        s_half[1] = 0.5;
        let half =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &s_half, &[], 0).expect("present");

        let mut s_full = NEUTRAL_LIGHTSTYLE_SCALES;
        s_full[1] = 1.0;
        let full =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &s_full, &[], 0).expect("present");

        // Compare against fresh builds at each scale.
        let fresh_full = face_lightmap_dyn(&bsp, &face, &poly, &s_full, &[], 0).expect("present");
        match (&half.luxels, &full.luxels, &fresh_full.luxels) {
            (Luxels::Owned(h), Luxels::Owned(f), Luxels::Owned(ff)) => {
                // block0=100, block1=200: half -> 100+0.5*200=200; full ->
                // 100+1.0*200=300. They must differ, and `full` must match a
                // fresh full build bit-for-bit.
                assert_ne!(h[0].to_bits(), f[0].to_bits(), "changed style key must rebuild");
                assert!((h[0] - 200.0).abs() < 1e-4, "half-scale luxel = 200, got {}", h[0]);
                assert!((f[0] - 300.0).abs() < 1e-4, "full-scale luxel = 300, got {}", f[0]);
                for i in 0..f.len() {
                    assert_eq!(f[i].to_bits(), ff[i].to_bits(), "rebuilt full vs fresh luxel {i}");
                }
            }
            _ => panic!("2-style face must own the combine"),
        }
    }

    #[test]
    fn lightmap_cache_invalidates_when_faces_len_changes() {
        reset_render_caches();
        // World A: a 2-style face, populate the cache for face 0.
        let (bsp_a, face_a, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;
        let _ = face_lightmap_world_cached(&bsp_a, 0, &face_a, &poly, &scales, &[], 0).expect("present");

        // World B: a DIFFERENT world with a different faces.len() and different
        // lightmap bytes at face 0. The fingerprint mismatch must clear the
        // (face-index-keyed) cache so face 0 is rebuilt from B's data, NOT served
        // from A's stale entry.
        let (mut bsp_b, face_b, poly_b) = two_style_face_bsp([0, 1, 255, 255], 40, 240);
        // Make faces.len() differ from A (A had 1 face) so the fingerprint flips
        // via the faces_len field as well as the &Bsp pointer.
        bsp_b.faces.push(face_b.clone());
        bsp_b.faces.push(face_b.clone());
        assert_ne!(bsp_a.faces.len(), bsp_b.faces.len());

        let got =
            face_lightmap_world_cached(&bsp_b, 0, &face_b, &poly_b, &scales, &[], 0).expect("present");
        let fresh_b = face_lightmap_dyn(&bsp_b, &face_b, &poly_b, &scales, &[], 0).expect("present");
        match (&got.luxels, &fresh_b.luxels) {
            (Luxels::Owned(g), Luxels::Owned(fb)) => {
                // B's block0=40, block1=240, scale 0.5 -> 40+120=160 (NOT A's 200).
                assert!((g[0] - 160.0).abs() < 1e-4, "world B luxel = 160, got {} (stale A?)", g[0]);
                for i in 0..g.len() {
                    assert_eq!(g[i].to_bits(), fb[i].to_bits(), "world B rebuilt vs fresh luxel {i}");
                }
            }
            _ => panic!("2-style face must own the combine"),
        }
    }

    #[test]
    fn lightmap_cache_dlit_face_rebuilds_each_frame_and_keeps_dlight() {
        reset_render_caches();
        // A single steady style-0 face at neutral scale -> normally a static
        // borrow (uncached). A reaching dlight must still produce the owned,
        // dlit buffer (NOT a cached static-only buffer) every call.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);

        // First, populate any cache via a dlight-free neutral call (static borrow,
        // not cached). Then a reaching dlight: must own the buffer and brighten.
        let _ = face_lightmap_world_cached(&bsp, 0, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0);
        let lit = face_lightmap_world_cached(
            &bsp, 0, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl),
            ALL_DLIGHT_BITS,
        )
        .expect("present");
        assert!(matches!(lit.luxels, Luxels::Owned(_)), "a dlit face must own the dlit buffer");
        // Must match the direct (cache-free) dlit build exactly.
        let fresh = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl),
            ALL_DLIGHT_BITS,
        )
        .expect("present");
        match (&lit.luxels, &fresh.luxels) {
            (Luxels::Owned(a), Luxels::Owned(b)) => {
                for i in 0..a.len() {
                    assert_eq!(a[i].to_bits(), b[i].to_bits(), "dlit cached-path vs fresh luxel {i}");
                }
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn world_render_is_pixel_identical_across_frames_with_lightmap_cache() {
        // This mirrors what the orchestrator diffs: render the SAME lightmapped,
        // style-animated world repeatedly and require byte-identical pixels. The
        // first render populates the geom + lightmap caches; the second hits
        // them. The cached combine must be bit-identical, so the images match.
        reset_render_caches();
        let bsp = lightmapped_demo_room(100, 200);
        let pal = [[180u8, 150, 90]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5; // non-neutral -> owned combine -> cache used

        let render = |b: &Bsp| {
            render_scene_ext(
                b, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles, None,
            )
        };

        let frame1 = render(&bsp); // populates caches
        let frame2 = render(&bsp); // cache hits
        assert_eq!(frame1.rgb, frame2.rgb, "cached frame must be pixel-identical to the first");

        // A change in the style scale must change the cache key AND the pixels
        // (proving the cache is keyed on the scale, not stale).
        let mut styles_b = NEUTRAL_LIGHTSTYLE_SCALES;
        styles_b[1] = 1.0;
        let frame_b = render_scene_ext(
            &bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles_b, None,
        );
        assert_ne!(
            frame1.rgb, frame_b.rgb,
            "a different style scale must rebuild and produce different pixels"
        );

        // Re-render at the ORIGINAL scale: must again equal frame1 (the cache
        // correctly rebuilt back to the 0.5 key).
        let frame3 = render(&bsp);
        assert_eq!(frame1.rgb, frame3.rgb, "returning to the original key reproduces frame1");
    }

    /// Give every face a plain (non-special) wall texture so the lit-surface cache
    /// (which needs a real miptex + colormap) is actually exercised.
    fn demo_room_with_walls(mut bsp: Bsp) -> Bsp {
        let n_tex = bsp.texinfo.len().max(1);
        bsp.textures = (0..n_tex)
            .map(|i| {
                Some(crate::bsp::MipTex {
                    name: format!("wall{i}"),
                    width: 16,
                    height: 16,
                    offsets: [0, 0, 0, 0],
                    pixels: vec![(i * 7) as u8; 16 * 16],
                    mips: Default::default(),
                    anim: None,
                })
            })
            .collect();
        bsp
    }

    #[test]
    fn external_models_bypass_and_dont_evict_world_surf_cache() {
        // REGRESSION (performance): external brush models (the b_*.bsp item boxes)
        // re-clone their Bsp every frame, so each has a different WorldFingerprint
        // every frame and every instance — they can NEVER hit the surface cache.
        // They must therefore BYPASS it: routing them through the shared world slot
        // would evict the world's resident cache and reintroduce the ~60ms/frame
        // re-bake-everything cost. Here we warm the world cache, then render a frame
        // containing MANY (26 > the old broken 24-slot LRU) distinct external models,
        // and assert (a) the externals baked (went through the bypass, not the cache)
        // and (b) the world cache is completely untouched afterwards.
        reset_render_caches();
        let world = demo_room_with_walls(lightmapped_demo_room(100, 200));
        // 26 distinct external "boxes" (each a separate Bsp -> distinct fingerprint),
        // standing at the world origin so their inward walls are in view and drawn.
        let ext_bsps: Vec<Bsp> = (0..26)
            .map(|k| demo_room_with_walls(lightmapped_demo_room(40 + k as u8, 220)))
            .collect();
        let externals: Vec<ExternalBModel> =
            ext_bsps.iter().map(|b| ExternalBModel { bsp: b, origin: [0.0, 0.0, 0.0] }).collect();

        let pal = [[180u8, 150, 90]; 256];
        let colormap = vec![0u8; COLORMAP_LEN]; // present -> the surf-cache path is active
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let render = |ext: &[ExternalBModel]| {
            render_scene_ext(
                &world, &cam, 160, 120, &pal, &[], &[], ext, None, 0.0, &[], &[], &styles,
                Some(&colormap),
            )
        };

        let _ = render(&[]); // bake the world's surface blocks into the cache

        // Sanity: the world scene hits its warm cache (else the asserts below are vacuous).
        render_stats_begin();
        let _ = render(&[]);
        let warm = render_stats_end();
        assert!(
            warm.surf_cache_hits > 0 && warm.surf_baked == 0,
            "world scene must hit the warm surf cache (got {} hits, {} bakes)",
            warm.surf_cache_hits, warm.surf_baked
        );
        let world_hits = warm.surf_cache_hits;

        // A frame with 26 distinct external models: the world still fully hits, and
        // the externals BAKE (proving they took the bypass path, not the cache).
        render_stats_begin();
        let _ = render(&externals);
        let with_ext = render_stats_end();
        assert_eq!(
            with_ext.surf_cache_hits, world_hits,
            "the world's faces must all still hit while externals draw (got {} vs {})",
            with_ext.surf_cache_hits, world_hits
        );
        assert_eq!(
            with_ext.surf_baked, 0,
            "the world must NOT re-bake while externals draw — got {} cached-path bakes",
            with_ext.surf_baked
        );
        assert!(
            with_ext.surf_bypass_baked > 0,
            "external models must bake via the bypass path — got {} bypass bakes",
            with_ext.surf_bypass_baked
        );

        // THE GUARD: after that external-laden frame, the world cache is untouched —
        // a subsequent world-only frame still hits everything, zero re-bakes. (With
        // the old shared/LRU cache the externals would have evicted it -> re-bakes.)
        render_stats_begin();
        let _ = render(&[]);
        let after = render_stats_end();
        assert_eq!(
            after.surf_baked, 0,
            "world cache was polluted/evicted by external models — {} faces re-baked \
             (the ~60ms/frame regression)",
            after.surf_baked
        );
        assert_eq!(after.surf_cache_hits, world_hits, "world cache must be fully intact");
    }

    /// A colormap whose rows differ (row `r` maps texel `c` to `(c + 3r) % 256`)
    /// and a grey palette, so a lighting change shows up as a pixel change.
    fn ramp_colormap() -> (Vec<u8>, [[u8; 3]; 256]) {
        let mut cm = vec![0u8; COLORMAP_LEN];
        for (i, v) in cm.iter_mut().enumerate() {
            *v = ((i % 256 + 3 * (i / 256)) % 256) as u8;
        }
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8; 3];
        }
        (cm, pal)
    }

    /// A2 (`D_CacheSurface`): a dynamically lit wall is baked into the surface
    /// cache with the light (no per-pixel fallback), rebaked every frame the
    /// light lives (`cache->dlight`), and rebuilt without it the first frame after
    /// — never lingering — and a cold cache draws the same lit frame.
    #[test]
    fn dlit_faces_bake_through_the_surface_cache_and_rebuild_when_the_light_dies() {
        reset_render_caches();
        let world = demo_room_with_walls(lightmapped_demo_room(40, 0));
        let (cm, pal) = ramp_colormap();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 300.0, f32::MAX, 0.0, 0.0, 0);
        let render = |dls: &[DynamicLight]| {
            render_stats_begin();
            let img = render_scene_ext(
                &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], dls,
                &NEUTRAL_LIGHTSTYLE_SCALES, Some(&cm),
            );
            (img, render_stats_end())
        };
        let (unlit, st0) = render(&[]);
        let (lit, st) = render(std::slice::from_ref(&dl));
        assert_ne!(lit.rgb, unlit.rgb, "the light must show");
        // demo_room's walls whose texinfo maps them to a line (zero extent) have
        // no block at all (id's `D_SCAlloc` would `Sys_Error` on them); the light
        // must not send any other face to the per-pixel path.
        assert_eq!(st.surf_misses, st0.surf_misses, "no dlit face may fall back to the per-pixel path");
        assert!(st.surf_hits > 0);
        assert!(st.surf_baked > 0, "dlit faces are baked with the light");
        // Lit again: a dlit block is never reused (the light may have moved).
        let (lit2, st2) = render(std::slice::from_ref(&dl));
        assert_eq!(lit2.rgb, lit.rgb);
        assert_eq!(st2.surf_baked, st.surf_baked, "every dlit face rebakes every lit frame");
        // The light dies: exactly those entries rebuild, and the frame is unlit.
        let (after, st3) = render(&[]);
        assert_eq!(after.rgb, unlit.rgb, "the light must not linger in the cache");
        assert_eq!(st3.surf_baked, st.surf_baked, "the dlit entries rebuild without the light");
        let (_, st4) = render(&[]);
        assert_eq!(st4.surf_baked, 0, "then the cache is warm again");
        // History-free: a cold cache draws the same lit frame.
        reset_render_caches();
        assert_eq!(render(std::slice::from_ref(&dl)).0.rgb, lit.rgb);
    }

    /// `D_CacheSurface` keys a block on its texture (`cache->texture`): an
    /// animated wall's next frame rebuilds the block instead of showing the
    /// first frame forever.
    #[test]
    fn animated_wall_texture_rebuilds_its_cached_block() {
        use crate::bsp::{MipTex, TexAnim};
        reset_render_caches();
        let mut world = lightmapped_demo_room(40, 0);
        let mk = |name: &str, texel: u8, anim: TexAnim| {
            Some(MipTex {
                name: name.into(),
                width: 16,
                height: 16,
                offsets: [0, 0, 0, 0],
                pixels: vec![texel; 16 * 16],
                mips: Default::default(),
                anim: Some(anim),
            })
        };
        world.textures = vec![
            mk("+0wall", 10, TexAnim { total: 4, min: 0, max: 2, next: 1, alternate: None }),
            mk("+1wall", 90, TexAnim { total: 4, min: 2, max: 4, next: 0, alternate: None }),
        ];
        for ti in world.texinfo.iter_mut() {
            ti.miptex = 0;
        }
        let (cm, pal) = ramp_colormap();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let render = |time: f32| {
            render_scene_ext(
                &world, &cam, 160, 120, &pal, &[], &[], &[], None, time, &[], &[],
                &NEUTRAL_LIGHTSTYLE_SCALES, Some(&cm),
            )
        };
        let f0 = render(0.0);
        let f1 = render(0.2);
        assert_ne!(f0.rgb, f1.rgb, "the animation's second frame must show");
        assert_eq!(render(0.0).rgb, f0.rgb, "and the first again");
        reset_render_caches();
        assert_eq!(render(0.2).rgb, f1.rgb, "a warm cache draws what a cold one does");
    }

    // -- Mip levels (D_MipLevelForScale, D_CacheSurface per miplevel) -------

    /// `D_MipLevelForScale`: `basemip` {1, 0.4, 0.2} times `d_mipscale`, and
    /// never finer than `d_mipcap`.
    #[test]
    fn mip_level_for_scale_is_d_mip_level_for_scale() {
        set_mip_cvars(MipCvars::DEFAULT);
        let mv = MipView::new(160.0, 100.0, 160.0, 160.0);
        let levels: Vec<u32> =
            [5.0, 1.0, 0.999, 0.4, 0.399, 0.2, 0.199, 0.0].iter().map(|&s| mv.level_for_scale(s)).collect();
        assert_eq!(levels, [0, 0, 1, 1, 2, 2, 3, 3]);
        // d_mipscale 0: every scale (>= 0) is mip 0.
        set_mip_cvars(MipCvars { mipscale: 0.0, mipcap: 0.0 });
        assert_eq!(MipView::new(160.0, 100.0, 160.0, 160.0).level_for_scale(0.0), 0);
        // d_mipcap 2 (and 9, clamped to 3): never finer than that.
        set_mip_cvars(MipCvars { mipscale: 1.0, mipcap: 2.0 });
        assert_eq!(MipView::new(160.0, 100.0, 160.0, 160.0).level_for_scale(5.0), 2);
        set_mip_cvars(MipCvars { mipscale: 1.0, mipcap: 9.0 });
        assert_eq!(MipView::new(160.0, 100.0, 160.0, 160.0).level_for_scale(5.0), 3);
        set_mip_cvars(MipCvars::DEFAULT);
    }

    /// `Mod_LoadTexinfo`'s `mipadjust` from the mean texture-axis length.
    #[test]
    fn mipadjust_follows_the_texture_scale() {
        let ti = |len: f32| crate::bsp::TexInfo {
            vecs: [[len, 0.0, 0.0, 7.0], [0.0, 0.0, len, 0.0]],
            miptex: 0,
            flags: 0,
        };
        assert_eq!(mipadjust(&ti(1.0)), 1.0);
        assert_eq!(mipadjust(&ti(2.0)), 1.0);
        assert_eq!(mipadjust(&ti(0.5)), 2.0);
        assert_eq!(mipadjust(&ti(0.4)), 3.0);
        assert_eq!(mipadjust(&ti(0.25)), 4.0);
    }

    /// `surf->nearzi` comes from the polygon clipped to the frustum's sides: a
    /// floor running from behind the eye to far ahead is nearest where the
    /// bottom of the view cuts it, not at its vertex behind the camera.
    #[test]
    fn nearzi_is_taken_from_the_frustum_clipped_polygon() {
        use super::super::vis::VView;
        let v = |vx: f32, vy: f32, vz: f32| VView { vx, vy, vz, s: 0.0, t: 0.0 };
        // 90 degrees both ways: |vx| <= vz, |vy| <= vz.
        let mut mv = MipView::new(100.0, 100.0, 100.0, 100.0);
        let floor = [v(-50.0, -20.0, -10.0), v(50.0, -20.0, -10.0), v(50.0, -20.0, 200.0), v(-50.0, -20.0, 200.0)];
        assert!((mv.nearzi(&floor) - 1.0 / 20.0).abs() < 1e-6, "got {}", mv.nearzi(&floor));
        // Wholly left of the view: nothing left, nearzi 0 (mip 3).
        let off = [v(-300.0, 0.0, 10.0), v(-200.0, 0.0, 10.0), v(-200.0, 5.0, 100.0)];
        assert_eq!(mv.nearzi(&off), 0.0);
        // Wholly inside: the nearest vertex, its z clamped to NEAR_CLIP.
        let inside = [v(0.0, 0.0, 40.0), v(1.0, 0.0, 30.0), v(0.0, 1.0, 50.0)];
        assert!((mv.nearzi(&inside) - 1.0 / 30.0).abs() < 1e-7);
        let ti = crate::bsp::TexInfo { vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]], miptex: 0, flags: 0 };
        // scale = (1/30) * 100 * 1 = 3.3 -> mip 0; at 10x the distance 0.33 -> mip 2.
        assert_eq!(mv.level_for_face(&inside, &ti), 0);
        let far: Vec<VView> = inside.iter().map(|p| v(p.vx * 10.0, p.vy * 10.0, p.vz * 10.0)).collect();
        assert_eq!(mv.level_for_face(&far, &ti), 2);
    }

    /// A 32x32 wall texture whose level `m` texels are all `10 * m + 1`.
    fn leveled_miptex() -> crate::bsp::MipTex {
        crate::bsp::MipTex {
            name: "wall".into(),
            width: 32,
            height: 32,
            offsets: [0, 0, 0, 0],
            pixels: vec![1; 32 * 32],
            mips: [vec![11; 16 * 16], vec![21; 8 * 8], vec![31; 4 * 4]],
            anim: None,
        }
    }

    /// `D_CacheSurface` at a mip level: the block is `extents >> miplevel` a side,
    /// baked from that level's texels, and each level has its own cache slot
    /// (`cachespots[miplevel]`) — going back to a level is a hit.
    #[test]
    fn surface_blocks_are_baked_and_cached_per_mip_level() {
        reset_render_caches();
        let (cm, _) = ramp_colormap();
        let mt = leveled_miptex();
        // A 64x48-texel surface: 5x4 luxels, texturemins (-16, 32).
        let luxels = vec![128u8; 5 * 4];
        let lm = LightMap { luxels: Luxels::Static(&luxels), lmw: 5, lmh: 4, texmins: [-16.0, 32.0] };
        let (_bsp, face, _) = one_face_bsp_zplane(128);
        let fp = WorldFingerprint::of(&_bsp);
        let styles = NEUTRAL_LIGHTSTYLE_SCALES;
        let get = |mip: u32| {
            face_surf_block(0, &face, 0, &mt, &lm, &cm, fp, 1, &styles, false, true, mip).expect("block")
        };
        render_stats_begin();
        for mip in 0..4u32 {
            let sb = get(mip);
            assert_eq!(sb.mip, mip);
            assert_eq!((sb.bw, sb.bh), (64 >> mip, 48 >> mip));
            assert_eq!(sb.texmins, [(-16 >> mip) as f32, (32 >> mip) as f32]);
            // Uniform luxel 128: blocklights 128*256, t = (65280 - 32768) >> 2 =
            // 8128, colormap row 31 of the level's texel (`ramp_colormap`: +3
            // per row).
            let want = (10 * mip + 1 + 3 * 31) as u8;
            assert!(sb.block.iter().all(|&p| p == want), "mip {mip}");
        }
        let cold = render_stats_end();
        assert_eq!((cold.surf_baked, cold.surf_cache_hits), (4, 0));
        assert_eq!(cold.surf_texels_baked, 64 * 48 + 32 * 24 + 16 * 12 + 8 * 6);
        render_stats_begin();
        for mip in [2u32, 0, 3, 1] {
            let _ = get(mip);
        }
        let warm = render_stats_end();
        assert_eq!((warm.surf_baked, warm.surf_cache_hits), (0, 4), "every level stays cached");
        let (bytes, blocks) = surface_cache_usage();
        assert_eq!((bytes, blocks), (64 * 48 + 32 * 24 + 16 * 12 + 8 * 6, 4));
        // A texture without levels 1..3 is baked at mip 0 whatever is asked.
        let mut flat = leveled_miptex();
        flat.mips = Default::default();
        let sb = face_surf_block(0, &face, 0, &flat, &lm, &cm, fp, 1, &styles, false, false, 2).expect("block");
        assert_eq!((sb.mip, sb.bw, sb.bh), (0, 64, 48));
    }

    // -- R_DrawSurfaceBlock8_mip0..3: id's integer light stepping ------------

    /// A colormap whose entry is its row (texel 0 everywhere), so a baked block
    /// reads back `light >> 8` per texel.
    fn row_colormap() -> Vec<u8> {
        (0..COLORMAP_LEN).map(|i| (i / 256) as u8).collect()
    }

    /// The C's stepping, hand-worked: one lightmap cell with (inverted) luxels
    /// 1000 (top left), 2000 (top right), 3000 (bottom left), 500 (bottom right).
    /// Each row starts at the right edge's value and steps left by
    /// `(left - right) >> 4`, flooring (-1000 >> 4 = -63), so texel 15 gets the
    /// right luxel exactly and texel 0 gets `right + 15 * step` — not the left
    /// luxel, which a bilinear sample (the port's old bake) gives it.
    #[test]
    fn surface_block_steps_light_like_r_draw_surface_block8() {
        let cm = row_colormap();
        let light = [1000, 2000, 3000, 500];
        let tex = vec![0u8; 16 * 16];
        let mut out = vec![0u8; 16 * 16];
        draw_surface_block(&tex, 16, 16, [0, 0], 0, &light, 2, &cm, &mut out, 16, 16);
        let at = |x: usize, y: usize| out[y * 16 + x] as i32;
        assert_eq!(at(15, 0), 2000 >> 8);
        assert_eq!(at(0, 0), (2000 - 15 * 63) >> 8, "1055: row 4, where the left luxel is row 3");
        // Down the edges: left += (3000-1000)>>4 = 125, right += (500-2000)>>4 = -94.
        assert_eq!(at(15, 15), (2000 - 15 * 94) >> 8);
        assert_eq!(at(0, 15), (590 + 15 * ((2875 - 590) >> 4)) >> 8);
        assert_eq!(at(8, 8), (1248 + 7 * ((2000 - 1248) >> 4)) >> 8);
        // Mip 1: 8-texel cells, shifts of 3: -1000 >> 3 = -125, -1500 >> 3 = -188.
        let tex1 = vec![0u8; 8 * 8];
        let mut out1 = vec![0u8; 8 * 8];
        draw_surface_block(&tex1, 8, 8, [0, 0], 1, &light, 2, &cm, &mut out1, 8, 8);
        let at1 = |x: usize, y: usize| out1[y * 8 + x] as i32;
        assert_eq!(at1(7, 0), 2000 >> 8);
        assert_eq!(at1(0, 0), (2000 - 7 * 125) >> 8);
        assert_eq!(at1(7, 7), (2000 - 7 * 188) >> 8);
        assert_eq!(at1(0, 7), (684 + 7 * ((2750 - 684) >> 3)) >> 8);
    }

    /// `R_DrawSurface`'s texture addressing at a mip level: the level tiled
    /// from `texturemins >> miplevel` (kept positive as the C's `+ (smax << 16)`
    /// does), across two cells.
    #[test]
    fn surface_block_tiles_the_level_from_texturemins() {
        // Row 0 of the colormap is the identity; every luxel is row 0.
        let cm: Vec<u8> = (0..COLORMAP_LEN).map(|i| (i % 256) as u8).collect();
        let level: Vec<u8> = (0..16 * 16).map(|i| i as u8).collect();
        let light = [64; 3 * 2];
        let mut out = vec![0u8; 16 * 8];
        // texturemins (48, -16) at mip 1: offsets 24 % 16 = 8 and -8 mod 16 = 8.
        draw_surface_block(&level, 16, 16, [48, -16], 1, &light, 3, &cm, &mut out, 16, 8);
        for (y, x) in [(0, 0), (0, 7), (0, 8), (3, 15), (7, 9)] {
            assert_eq!(out[y * 16 + x] as usize, (8 + y) % 16 * 16 + (8 + x) % 16, "texel ({x}, {y})");
        }
    }

    /// A dynamically lit face is baked at the level asked for, through the same
    /// `blocklights` and integer stepping as a static one (`R_BuildLightMap` +
    /// `R_AddDynamicLights`, then `R_DrawSurface` at `miplevel`), and never
    /// reused (`cache->dlight`).
    #[test]
    fn dlit_faces_bake_at_their_mip_level_through_the_same_stepping() {
        reset_render_caches();
        let (cm, _) = ramp_colormap();
        let mt = leveled_miptex();
        let (bsp, face, _) = one_face_bsp_zplane(128);
        let fp = WorldFingerprint::of(&bsp);
        // A 64x48 surface whose luxels a light has pushed up unevenly.
        let luxels: Vec<f32> = (0..5 * 4).map(|i| 100.0 + 7.0 * i as f32 + (i % 3) as f32 / 256.0).collect();
        let lm = LightMap { luxels: Luxels::Owned(luxels), lmw: 5, lmh: 4, texmins: [-16.0, 32.0] };
        let mut light = Vec::new();
        lm.blocklights_into(&mut light);
        render_stats_begin();
        for mip in 0..4u32 {
            let sb = face_surf_block(0, &face, 0, &mt, &lm, &cm, fp, 1, &NEUTRAL_LIGHTSTYLE_SCALES, true, true, mip)
                .expect("block");
            let (bw, bh) = (64 >> mip, 48 >> mip);
            let mut want = vec![0u8; bw * bh];
            let level = mt.mip(mip as usize).expect("level");
            draw_surface_block(level, 32 >> mip, 32 >> mip, [-16, 32], mip, &light, 5, &cm, &mut want, bw, bh);
            assert_eq!((sb.mip, sb.bw, sb.bh), (mip, bw, bh));
            assert_eq!(*sb.block, want, "mip {mip}");
            // Lit again: rebaked, never a hit.
            let _ = face_surf_block(0, &face, 0, &mt, &lm, &cm, fp, 1, &NEUTRAL_LIGHTSTYLE_SCALES, true, true, mip);
        }
        let st = render_stats_end();
        assert_eq!((st.surf_baked, st.surf_cache_hits), (8, 0));
    }

    #[test]
    fn world_render_unaffected_by_intervening_different_world() {
        // Render world A, then a DIFFERENT world B (different geometry + lighting,
        // which resets the face-index-keyed caches), then world A again. The two
        // renders of A must be byte-identical — proving the changelevel
        // invalidation never serves B's cached data for A's faces.
        reset_render_caches();
        let a = lightmapped_demo_room(100, 200);
        let b = lightmapped_demo_room(60, 240); // different lighting bytes
        let pal = [[180u8, 150, 90]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let render = |bsp: &Bsp| {
            render_scene_ext(
                bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles, None,
            )
        };

        let a1 = render(&a);
        let _b = render(&b); // resets caches to world B
        let a2 = render(&a); // must rebuild A's caches, not reuse B's
        assert_eq!(a1.rgb, a2.rgb, "world A renders identically before and after world B");
    }

    #[test]
    fn face_geom_cache_matches_uncached_build_and_invalidates() {
        reset_render_caches();
        let (bsp, face, _poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        // First call builds + caches; second returns the cached clone.
        let g1 = face_geom_cached(&bsp, 0, &face);
        let g2 = face_geom_cached(&bsp, 0, &face);
        assert!(!g1.bad);
        // The cached poly must equal a direct face_world_poly reconstruction
        // (face_world_poly walks the BSP edge tables, so this — not the helper's
        // literal `poly` used for lightmap math — is the geometry the loop sees).
        let mut direct = Vec::new();
        assert!(face_world_poly(&bsp, &face, &mut direct));
        assert_eq!(*g1.poly, direct);
        assert_eq!(*g2.poly, direct);
        // Normal + centroid match a direct compute.
        assert_eq!(g1.normal, face_normal(&bsp, &face));
        // AABB encloses every vertex.
        for v in &direct {
            for (k, &c) in v.iter().enumerate() {
                assert!(g1.mins[k] <= c && c <= g1.maxs[k]);
            }
        }
        // A different world (more faces) invalidates: still a correct rebuild.
        let (mut bsp_b, face_b, _polyb) = two_style_face_bsp([0, 255, 255, 255], 50, 50);
        bsp_b.faces.push(face_b.clone());
        let gb = face_geom_cached(&bsp_b, 0, &face_b);
        assert!(!gb.bad);
    }
}
