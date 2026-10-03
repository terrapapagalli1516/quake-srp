//! Surfaces: face geometry, texture animation, and the per-face caches.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_surf.c` (`R_TextureAnimation`) and `WinQuake/d_surf.c`
//! (the lit surface cache, `D_CacheSurface`); the port adds the static-geometry
//! and lightmap caches the world pass reuses between frames.

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::light::{
    any_dlight_reaches, face_lightmap_with, LightMap, Luxels, COLORMAP_LEN,
    LIGHTSTYLES, STYLE_NONE,
};
use super::stats::Profiler;
use super::torch::FaceTorches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

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

/// One cached lightmap (`R_BuildLightMap`'s blocklights for a face whose
/// styles combine into owned luxels): the multi-style or non-neutral case,
/// never touched by a dynamic light. A single steady style-0 face at neutral
/// scale borrows the map's bytes instead and needs no entry. The stored
/// `luxels` are the exact buffer `face_lightmap_dyn` produced, so reusing them
/// is bit-for-bit a rebuild.
#[derive(Clone)]
struct LightCacheEntry {
    /// The resolved per-active-style SCALE values at build time (in stored slot
    /// order). The entry is valid only while these are bit-identical — torches
    /// animate at 10 Hz, so at 60 fps the scales are unchanged ~5/6 frames.
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// The scales of the steady torches lighting the face at build time
    /// ([`FaceTorches::key`]; empty with `r_torchflicker` off): valid only
    /// while they are bit-identical too.
    torches: Box<[f32]>,
    /// The combined luxel grid (no dynamic light folded in: a dlit face is
    /// rebuilt every frame and never stored).
    luxels: Vec<f32>,
    lmw: usize,
    lmh: usize,
    texmins: [f32; 2],
}

/// One lit SURFACE block (`d_surf.c`'s `surfcache_t`): the face's texture at
/// one mip level with the lightmap shaded in AND resolved through the colormap
/// to a final palette index, one byte per texel of that level. The span routine
/// then reads a single byte per screen pixel (then one palette lookup) instead
/// of sampling the texture, the lightmap and the colormap per pixel — all of
/// that is a per-texel bake done ONCE and reused every frame.
#[derive(Clone)]
struct SurfCacheEntry {
    /// Active styles' resolved scale values at bake time (the cache key, same as the
    /// lightmap cache — a torch tick rebuilds the block).
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// The steady torches' scales at bake time, as the lightmap cache's.
    torches: Box<[f32]>,
    /// The texture baked in (`cache->texture`): an animated wall's frame index
    /// into `bsp.textures`, so the next animation frame rebuilds the block.
    texture: usize,
    /// Baked with dynamic light folded in (`cache->dlight`): never reused, so the
    /// first frame the light is gone rebuilds the block without it.
    dlight: bool,
    /// Baked palette indices, `bw * bh`, row-major, shared with the frames that
    /// draw it (an `Arc`, so handing a block to a pass is a refcount bump).
    block: Arc<Vec<u8>>,
    bw: usize,
    bh: usize,
    /// Asked for this frame and not baked yet: its job in the frame's bake
    /// list ([`SurfaceCaches::surface`]), the block to come
    /// ([`SurfaceCaches::baked`]).
    pending: Option<usize>,
}

/// The world's per-face caches, the [`Renderer`](super::Renderer)'s: each is
/// indexed by the world's face number, sized by [`SurfaceCaches::begin_map`]
/// (`R_NewMap`) and filled the first time a face is drawn.
///
/// - the face's polygon (`CalcSurfaceExtents`' vertices), built once: the
///   world never moves;
/// - its combined lightmap while its light styles hold still;
/// - its lit surface blocks, one per mip level (`surface->cachespots[miplevel]`,
///   `D_CacheSurface`).
///
/// The inline brush models share the world's faces, and so its caches; the
/// external `b_*.bsp` boxes bypass them. Unlike id's fixed-size surface cache
/// with its LRU rover (`D_SCAlloc`), nothing is evicted until the map changes:
/// eviction only ever costs id a rebake, never a different pixel.
#[derive(Default)]
pub(super) struct SurfaceCaches {
    geoms: Vec<Option<Vec<Vec3>>>,
    lights: Vec<Option<LightCacheEntry>>,
    blocks: Vec<[Option<SurfCacheEntry>; NUM_MIPS]>,
    /// The (face, mip level) entries this frame's lookups left pending.
    pending: Vec<(usize, usize)>,
    /// What a pending block reads as until it is baked: nothing.
    unbaked: Arc<Vec<u8>>,
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

/// What `D_DrawSurfaces` needs to pick a surface's mip level, set up once per
/// frame: `D_SetupFrame`'s `d_scalemip`/`d_minmip` and `D_ViewChanged`'s
/// `scale_for_mip`.
pub(super) struct MipView {
    scalemip: [f32; NUM_MIPS - 1],
    minmip: u32,
    scale_for_mip: f32,
}

impl MipView {
    /// For a view projected with `x = cx + xscale*vx/vz`, `y = cy -
    /// yscale*vy/vz`: `scale_for_mip` is the larger scale (`xscale`, or
    /// `yscale` when the pixels are taller than wide), under the cvars `cv`.
    pub(super) fn new(xscale: f32, yscale: f32, cv: MipCvars) -> MipView {
        // d_minmip = d_mipcap.value (float to int truncates), clamped to 0..3.
        let minmip = (cv.mipcap as i32).clamp(0, NUM_MIPS as i32 - 1) as u32;
        MipView {
            scalemip: BASEMIP.map(|b| b * cv.mipscale),
            minmip,
            scale_for_mip: if yscale > xscale { yscale } else { xscale },
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

    /// `D_DrawSurfaces`' `D_MipLevelForScale(s->nearzi * scale_for_mip *
    /// pface->texinfo->mipadjust)` for a surface whose edges gave `nearzi`
    /// (`R_EmitEdge`'s nearest `1/z` of the face as clipped to the view).
    pub(super) fn level_for_nearzi(&self, nearzi: f32, ti: &crate::bsp::TexInfo) -> u32 {
        self.level_for_scale(nearzi * self.scale_for_mip * mipadjust(ti))
    }
}

/// `mipadjust` (`Mod_LoadTexinfo`, `model.c`): about how many world units one
/// texel spans on this texinfo, in steps — the mean length of the two texture
/// axes (texels per unit) below 0.32 gives 4, below 0.49 3, below 0.99 2,
/// else 1. It multiplies the scale `D_MipLevelForScale` compares, so a
/// texture scaled up in the editor (short axes, big texels) keeps a finer mip
/// level further away.
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

/// A baked surface block ([`SurfaceCaches::surface`]): the palette indices,
/// `bw * bh` row-major, at mip level `mip`, whose texel `(i, j)` is the
/// surface's `(texmins[0] + i, texmins[1] + j)` in that level's texels. The
/// span walker reads it through gradients scaled to the level
/// ([`PolyGrads::mip_scaled`](super::raster::PolyGrads::mip_scaled)), as
/// `D_CalcGradients` scales its steps by `mipscale`.
pub(super) struct SurfBlock {
    pub(super) block: Arc<Vec<u8>>,
    pub(super) bw: usize,
    pub(super) bh: usize,
    pub(super) texmins: [f32; 2],
    pub(super) mip: u32,
}

/// A lit surface block to bake: `R_DrawSurface`'s inputs for one face at
/// one mip level, the frame's to own ([`SurfaceCaches::surface`] makes it),
/// baked by [`BakeJob::bake`] on whichever thread — a pure function of the
/// job, so the block is the same on any.
pub(super) struct BakeJob<'a> {
    /// The face's lightmap for this frame (`R_BuildLightMap`'s, dynamic and
    /// torch light in).
    lightmap: LightMap<'a>,
    /// The texture's level `mip`, `smax x tmax`.
    tex: &'a [u8],
    smax: usize,
    tmax: usize,
    /// `texturemins`, at mip 0.
    texmins: [i32; 2],
    mip: u32,
    bw: usize,
    bh: usize,
    colormap: &'a [u8],
}

impl BakeJob<'_> {
    /// The block's texels: the bake's work.
    pub(super) fn texels(&self) -> usize {
        self.bw * self.bh
    }

    /// `R_DrawSurface`: the block, `bw * bh` palette indices.
    pub(super) fn bake(&self) -> Arc<Vec<u8>> {
        let lm = &self.lightmap;
        let mut light = Vec::new();
        lm.blocklights_into(&mut light);
        let mut block = vec![0u8; self.texels()];
        draw_surface_block(self.tex, self.smax, self.tmax, self.texmins, self.mip, &light, lm.lmw, self.colormap, &mut block, self.bw, self.bh);
        Arc::new(block)
    }
}

/// A frame's bakes, shared by the threads that draw it: the jobs, and each
/// one's block once a thread has baked it.
///
/// The bakes have no round of threads of their own. Each thread of the
/// frame's one round — the bands' — first takes jobs from here until none
/// is left ([`Bakes::work`]), the largest first so the threads end
/// together, and then draws bands; a span reads its block with
/// [`Bakes::block`], which bakes it on the spot if no thread has yet and
/// waits if one is at it, so a band never draws from a block that is not
/// there. A block is its job's alone ([`BakeJob::bake`]), the same
/// whichever thread bakes it and whenever: the frame and the cache are the
/// same for any thread count. One thread bakes them all before its one band.
///
/// (A round of threads — spawn, run, join — costs 30–80 µs natively when
/// threads ran a moment before and 150–400 µs after an idle gap; on a phone
/// each one wakes workers that slept since the last frame. A round for the
/// bakes and another for the bands was two of them a frame; PERF_PLAN.md §14.)
pub(super) struct Bakes<'a> {
    jobs: Vec<BakeJob<'a>>,
    /// The jobs' indices, the largest block first: the order they are taken in.
    order: Vec<usize>,
    /// How many of `order` have been taken.
    taken: AtomicUsize,
    blocks: Vec<OnceLock<Arc<Vec<u8>>>>,
}

impl<'a> Bakes<'a> {
    /// The frame's `jobs`, none baked yet.
    pub(super) fn new(jobs: Vec<BakeJob<'a>>) -> Bakes<'a> {
        let mut order: Vec<usize> = (0..jobs.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].texels()));
        let blocks = jobs.iter().map(|_| OnceLock::new()).collect();
        Bakes { jobs, order, taken: AtomicUsize::new(0), blocks }
    }

    /// Bake jobs, one at a time, until every one is taken: what each thread
    /// of the frame does before its first band.
    pub(super) fn work(&self) {
        // (A thread that finds none left has still counted one: the count
        // only ever passes the jobs by a thread's worth.)
        while let Some(&job) = self.order.get(self.taken.fetch_add(1, Ordering::Relaxed)) {
            self.block(job);
        }
    }

    /// Job `job`'s block (empty for a job the frame does not have): baked
    /// here and now if no thread has baked it; if one is baking it, once
    /// that is done.
    #[inline]
    pub(super) fn block(&self, job: usize) -> &[u8] {
        match (self.blocks.get(job), self.jobs.get(job)) {
            (Some(block), Some(j)) => block.get_or_init(|| j.bake()),
            _ => &[],
        }
    }

    /// Every job's block, in the jobs' order, for the cache
    /// ([`SurfaceCaches::baked`]).
    pub(super) fn finish(self) -> Vec<Arc<Vec<u8>>> {
        let Bakes { jobs, blocks, .. } = self;
        blocks.into_iter().zip(&jobs).map(|(block, job)| block.into_inner().unwrap_or_else(|| job.bake())).collect()
    }
}

/// What [`SurfaceCaches::surface`] found for a face.
pub(super) enum Surface<'a> {
    /// A lit block: from the cache, or — with the index of its job in the
    /// frame's bake list — baked as the frame is drawn (its `block` is
    /// empty: the frame's [`Bakes`] has it).
    Block(SurfBlock, Option<usize>),
    /// No block (no usable colormap, no texture, an empty or oversized
    /// block): the face is lit per pixel, with its lightmap given back.
    PerPixel(Option<LightMap<'a>>),
}

/// What `D_CacheSurface` is asked for: one face's texture, lit, at one mip
/// level — what a bake reads borrowed for the frame (`'a`), the rest only
/// for the lookup (`'r`).
pub(super) struct SurfaceRequest<'r, 'a> {
    /// The face's number in the world, whose cache slot the block goes in, or
    /// `None` for an external brush model's face (baked fresh, never cached:
    /// its numbers are its own `b_*.bsp`'s).
    pub(super) slot: Option<usize>,
    pub(super) face: &'r crate::bsp::DFace,
    /// The (animated) texture's index in `bsp.textures`, and the texture.
    pub(super) texture: usize,
    pub(super) mt: &'a crate::bsp::MipTex,
    /// The face's lightmap for this frame, dynamic light included when `dlit`
    /// (the bake's, if it comes to one).
    pub(super) lightmap: LightMap<'a>,
    pub(super) colormap: &'a [u8],
    pub(super) light_styles: &'r [f32; LIGHTSTYLES],
    /// The steady torches lighting the face, at this frame's scales (the
    /// 2026 `r_torchflicker`; [`FaceTorches::NONE`] with it off).
    pub(super) torches: FaceTorches<'r>,
    /// A dynamic light reaches the face ([`any_dlight_reaches`]).
    pub(super) dlit: bool,
    pub(super) mip: u32,
}

impl SurfaceCaches {
    /// `R_NewMap`: every cache empty, with a slot for each of the world's
    /// `n_faces` faces.
    pub(super) fn begin_map(&mut self, n_faces: usize) {
        self.geoms.clear();
        self.geoms.resize(n_faces, None);
        self.lights.clear();
        self.lights.resize(n_faces, None);
        self.blocks.clear();
        self.blocks.resize(n_faces, Default::default());
        self.pending.clear();
    }

    /// The bytes the lit-surface cache holds (every baked block of every face
    /// at every mip level), and the number of blocks.
    pub(super) fn usage(&self) -> (usize, usize) {
        self.blocks.iter().flatten().flatten().fold((0, 0), |(bytes, n), e| (bytes + e.block.len(), n + 1))
    }

    /// Find a face's lit+colormapped surface block at mip level `req.mip` —
    /// `D_CacheSurface` — or make the job that bakes it: [`Surface::Block`],
    /// with the job's index in `jobs` when it is to be baked. The frame's
    /// jobs are baked by the threads that draw it, before their bands
    /// ([`Bakes`]), and handed back with [`SurfaceCaches::baked`] when the
    /// frame is drawn. [`Surface::PerPixel`]
    /// (the caller keeps the per-pixel path) when there is no usable colormap,
    /// the texture is missing, or the block would be empty or exceed
    /// [`SURF_BLOCK_MAX`]. A texture without its levels 1..3 (the synthetic
    /// ones in tests) is baked at mip 0.
    ///
    /// A dynamically lit face is baked like any other — `R_BuildLightMap` runs
    /// `R_AddDynamicLights`, then `R_DrawSurface` — and its entry marked
    /// `dlight`, as the C marks `cache->dlight`: a dlit block is never a hit
    /// in a later frame, so the light is rebaked every frame it is live and
    /// the first frame after it dies rebuilds the block without it. The hit
    /// test is the C's: same texture, same style values, no dlight now or at
    /// the bake — per face and mip level (`surface->cachespots[miplevel]`).
    /// A face asked for twice in a frame (an inline model drawn by two
    /// entities) finds the first ask's pending entry and shares its job,
    /// dynamic light or not: the same face in the same frame is the same
    /// block. A miss replaces the entry at once, as the C's did, so a frame's
    /// lookups see what one-by-one baking would have left.
    ///
    /// The bake is `R_DrawSurface`: the block is `extents >> miplevel` texels a
    /// side (`surfwidth`), made of one `16 >> miplevel` square per pair of
    /// lightmap columns and rows, each lit by `R_DrawSurfaceBlock8_mip0..3`'s
    /// integer interpolation ([`draw_surface_block`]).
    pub(super) fn surface<'a>(&mut self, req: SurfaceRequest<'_, 'a>, jobs: &mut Vec<BakeJob<'a>>, prof: &mut Profiler) -> Surface<'a> {
        let mt = req.mt;
        let lm = req.lightmap;
        if req.colormap.len() < COLORMAP_LEN {
            return Surface::PerPixel(Some(lm));
        }
        let mip = if (req.mip as usize) < NUM_MIPS && mt.mip(req.mip as usize).is_some() { req.mip } else { 0 };
        let Some(tex) = mt.mip(mip as usize) else { return Surface::PerPixel(Some(lm)) };
        let (smax, tmax) = ((mt.width as usize) >> mip, (mt.height as usize) >> mip);
        // `surfwidth = extents[0] >> miplevel`; `extents = (lmw - 1) * 16`.
        let bw = lm.lmw.saturating_sub(1).saturating_mul(16) >> mip;
        let bh = lm.lmh.saturating_sub(1).saturating_mul(16) >> mip;
        let Some(total) = bw.checked_mul(bh) else { return Surface::PerPixel(Some(lm)) };
        if smax == 0 || tmax == 0 || bw == 0 || bh == 0 || total > SURF_BLOCK_MAX {
            return Surface::PerPixel(Some(lm));
        }
        let (scales, n_styles) = style_scales(req.face, req.light_styles);
        // A style past 0: an animated (or switched) light, which rebakes the
        // block whenever its value changes.
        prof.add(|s| s.surf_styled += u64::from(req.face.styles.iter().take_while(|&&st| st != STYLE_NONE).any(|&st| st != 0)));
        // A steady torch flickers on it (`r_torchflicker`): the same.
        prof.add(|s| s.surf_torchlit += u64::from(!req.torches.is_empty()));
        // texturemins are whole multiples of 16, so `>> mip` is exact.
        let texmins_i = [lm.texmins[0] as i32, lm.texmins[1] as i32];
        let texmins = [(texmins_i[0] >> mip) as f32, (texmins_i[1] >> mip) as f32];
        let made = |block| SurfBlock { block, bw, bh, texmins, mip };
        // What a block to be baked reads as until then (cloned only on the
        // paths that bake or share a bake: a hit takes the cache's block).
        let unbaked = &self.unbaked;
        let bake = |prof: &mut Profiler| -> usize {
            prof.add(|s| s.surf_texels_baked += total as u64);
            jobs.push(BakeJob { lightmap: lm, tex, smax, tmax, texmins: texmins_i, mip, bw, bh, colormap: req.colormap });
            jobs.len() - 1
        };

        // An external brush model's face (a `b_*.bsp` box): baked fresh every
        // frame, never cached — its face numbers are its own bsp's. They are
        // tiny (a 6-face box), so an unconditional bake is cheap.
        let Some(slot) = req.slot else {
            prof.add(|s| s.surf_bypass_baked += 1);
            let job = bake(prof);
            return Surface::Block(made(unbaked.clone()), Some(job));
        };
        let spot = self.blocks.get_mut(slot).map(|spots| &mut spots[mip as usize]);
        if let Some(Some(e)) = spot.as_deref() {
            let same = e.texture == req.texture
                && e.n_styles == n_styles
                && e.bw == bw
                && e.bh == bh
                && e.style_scales[..n_styles] == scales[..n_styles]
                && req.torches.key_is(&e.torches);
            // This frame's own ask for the same block: share its bake.
            if let (true, Some(job)) = (same && e.dlight == req.dlit, e.pending) {
                prof.add(|s| s.surf_cache_hits += 1);
                return Surface::Block(made(unbaked.clone()), Some(job));
            }
            // HIT (`D_CacheSurface`): no dynamic light now or in the bake,
            // same texture, same resolved style scales.
            if same && !req.dlit && !e.dlight && e.pending.is_none() {
                prof.add(|s| s.surf_cache_hits += 1);
                return Surface::Block(made(e.block.clone()), None);
            }
        }
        // MISS: a bake, its entry stored now (marked `dlight` when a light is
        // folded in) and its block when the frame's bakes are done.
        prof.add(|s| s.surf_baked += 1);
        let job = bake(prof);
        if let Some(e) = spot {
            *e = Some(SurfCacheEntry {
                style_scales: scales,
                n_styles,
                torches: req.torches.key(),
                texture: req.texture,
                dlight: req.dlit,
                block: unbaked.clone(),
                bw,
                bh,
                pending: Some(job),
            });
            self.pending.push((slot, mip as usize));
        }
        Surface::Block(made(unbaked.clone()), Some(job))
    }

    /// The frame's bakes are done: `blocks[job]` is job `job`'s block. Every
    /// entry this frame left pending takes its job's.
    pub(super) fn baked(&mut self, blocks: &[Arc<Vec<u8>>]) {
        for (slot, mip) in self.pending.drain(..) {
            if let Some(e) = self.blocks.get_mut(slot).and_then(|spots| spots[mip].as_mut()) {
                if let Some(job) = e.pending.take() {
                    e.block = blocks[job].clone();
                }
            }
        }
    }

    /// A frame that ended before its bakes (it never does but by a panic)
    /// leaves no pending entry to the next: forget them.
    pub(super) fn begin_frame(&mut self) {
        for (slot, mip) in self.pending.drain(..) {
            if let Some(spot) = self.blocks.get_mut(slot).map(|spots| &mut spots[mip]) {
                if spot.as_ref().is_some_and(|e| e.pending.is_some()) {
                    *spot = None;
                }
            }
        }
    }

    /// Every lit block the cache holds: (face, mip level, texture, whether a
    /// dynamic light is in it, its bytes), in face order (the tests').
    #[cfg(test)]
    pub(super) fn block_entries(&self) -> Vec<(usize, usize, usize, bool, Vec<u8>)> {
        let mut out = Vec::new();
        for (slot, spots) in self.blocks.iter().enumerate() {
            for (mip, e) in spots.iter().enumerate() {
                if let Some(e) = e {
                    assert!(e.pending.is_none(), "face {slot} mip {mip} left pending");
                    out.push((slot, mip, e.texture, e.dlight, e.block.to_vec()));
                }
            }
        }
        out
    }

    /// [`SurfaceCaches::surface`] as one frame of one face: looked up, baked
    /// and handed back (the tests').
    #[cfg(test)]
    pub(super) fn surface_now(&mut self, req: SurfaceRequest<'_, '_>, prof: &mut Profiler) -> Option<SurfBlock> {
        self.begin_frame();
        let mut jobs = Vec::new();
        let Surface::Block(mut block, job) = self.surface(req, &mut jobs, prof) else { return None };
        let blocks: Vec<_> = jobs.iter().map(BakeJob::bake).collect();
        self.baked(&blocks);
        if let Some(job) = job {
            block.block = blocks[job].clone();
        }
        Some(block)
    }
}

/// The resolved scales of `face`'s active styles, in slot order, and how many
/// there are: the surface and lightmap caches' key.
fn style_scales(
    face: &crate::bsp::DFace,
    light_styles: &[f32; LIGHTSTYLES],
) -> ([f32; crate::bsp::MAXLIGHTMAPS], usize) {
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
    (scales, n_styles)
}

/// A face's polygon ([`face_world_poly`]), empty when it cannot be built.
fn face_poly_or_empty(bsp: &Bsp, face: &crate::bsp::DFace) -> Vec<Vec3> {
    let mut poly = Vec::new();
    if !face_world_poly(bsp, face, &mut poly) {
        poly.clear();
    }
    poly
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
    let Some(colormap) = colormap.get(..COLORMAP_LEN).and_then(|c| <&[u8; COLORMAP_LEN]>::try_from(c).ok()) else { return };
    if smax == 0 || tmax == 0 || tex.len() < smax * tmax || out.len() < bw * bh {
        return;
    }
    let level = Level { tex, smax, tmax, texmins, light, lmw, colormap };
    // id has a routine per mip level too: the block size is each one's constant.
    match mip {
        0 => level.draw_blocks::<16>(out, bw, bh),
        1 => level.draw_blocks::<8>(out, bw, bh),
        2 => level.draw_blocks::<4>(out, bw, bh),
        _ => level.draw_blocks::<2>(out, bw, bh),
    }
}

/// What a surface block is baked from ([`draw_surface_block`]): a mip
/// level's texels, the face's `blocklights` and the colormap.
struct Level<'a> {
    tex: &'a [u8],
    smax: usize,
    tmax: usize,
    /// `texturemins`, at mip 0.
    texmins: [i32; 2],
    light: &'a [i32],
    lmw: usize,
    colormap: &'a [u8; COLORMAP_LEN],
}

impl Level<'_> {
    /// `R_DrawSurfaceBlock8_mipN` over the whole surface, for the level whose
    /// blocks are `N = 16 >> mip` texels a side (a constant, so the row of
    /// `N` texels is a loop the compiler unrolls, as id wrote four routines).
    fn draw_blocks<const N: usize>(&self, out: &mut [u8], bw: usize, bh: usize) {
        let Level { tex, smax, tmax, texmins, light, lmw, colormap } = *self;
        let mip = 4 - N.trailing_zeros();
        let shift = 4 - mip;
        let (nh, nv) = (bw / N, bh / N);
        // `soffset`/`basetoffset`: where the surface's first texel falls in the tiled
        // texture ("+ (smax << 16)" in the C only keeps the % positive).
        let soffset = (texmins[0] >> mip).rem_euclid(smax as i32) as usize;
        let toffset = (texmins[1] >> mip).rem_euclid(tmax as i32) as usize;
        let lux = |x: usize, y: usize| light.get(y * lmw + x).copied().unwrap_or(1 << 6);
        for v in 0..nv {
            // The texture row under each of the blocks' rows, once for the
            // whole row of blocks.
            let trow: [usize; N] = std::array::from_fn(|i| (toffset + v * N + i) % tmax * smax);
            for u in 0..nh {
                // r_lightptr[0], r_lightptr[1], and the same one lightmap row down.
                let mut lightleft = lux(u, v);
                let mut lightright = lux(u + 1, v);
                let lightleftstep = (lux(u, v + 1) - lightleft) >> shift;
                let lightrightstep = (lux(u + 1, v + 1) - lightright) >> shift;
                // The block's first texture column (the C wraps `soffset` a block at a
                // time; id's textures are 16-aligned, so this is the same column).
                let s0 = (soffset + u * N) % smax;
                for (i, &trow) in trow.iter().enumerate() {
                    let y = v * N + i;
                    let src = &tex[trow..trow + smax];
                    let at = y * bw + u * N;
                    let Some(dst) = out.get_mut(at..at + N).and_then(|o| <&mut [u8; N]>::try_from(o).ok()) else { return };
                    let lightstep = (lightleft - lightright) >> shift;
                    let mut l = lightright;
                    // 0 < l <= 16320: the luxels are clamped to 64..=16320 and the
                    // floor steps overshoot the lower one by less than 15 per edge,
                    // so `l & 0xFF00` is one of the colormap's 64 rows — and
                    // masked to them (`ROWS`) the index is in the colormap for
                    // the compiler too, with no test at every texel.
                    const ROWS: usize = COLORMAP_LEN - 256;
                    if let Some(seg) = src.get(s0..s0 + N).and_then(|s| <&[u8; N]>::try_from(s).ok()) {
                        for b in (0..N).rev() {
                            dst[b] = colormap[((l & 0xFF00) as usize & ROWS) | seg[b] as usize];
                            l += lightstep;
                        }
                    } else {
                        for b in (0..N).rev() {
                            dst[b] = colormap[((l & 0xFF00) as usize & ROWS) | src[(s0 + b) % smax] as usize];
                            l += lightstep;
                        }
                    }
                    lightright += lightrightstep;
                    lightleft += lightleftstep;
                }
            }
        }
    }
}

impl SurfaceCaches {
    /// A world face's polygon (`face_world_poly`, empty when that fails),
    /// built the first time it is asked for: the world never moves. `face`
    /// must be `bsp.faces[idx]`; `None` for a face number past the map begun.
    /// ([`SurfaceCaches::world_lightmap`] reads the same slots.)
    #[cfg(test)]
    pub(super) fn geom(&mut self, bsp: &Bsp, idx: usize, face: &crate::bsp::DFace) -> Option<&[Vec3]> {
        let slot = self.geoms.get_mut(idx)?;
        Some(slot.get_or_insert_with(|| face_poly_or_empty(bsp, face)))
    }

    /// The lightmap of world face `idx` (`face` is `bsp.faces[idx]`) for this
    /// frame's light styles, steady torches (`torches`, the 2026
    /// `r_torchflicker`) and dynamic lights, through the lightmap cache.
    ///
    /// Behaviour, by case:
    ///  * **Static borrow** (`face_lightmap_dyn` returns `Luxels::Static`, the
    ///    common steady style-0-at-neutral case) — returned as-is, no caching:
    ///    it already borrows the BSP bytes.
    ///  * **Dynamic light reaches the face** — rebuilt EVERY frame (dlights
    ///    move). The result is NOT stored, and any cached entry for this face
    ///    is dropped, so the dlight is never silently lost on a later frame.
    ///  * **Owned combine, no dlight** (animated styles, flickering torches)
    ///    — keyed by the resolved style scale values and the torches' scales.
    ///    On a hit the cached luxels are cloned into a fresh `LightMap`
    ///    (bit-identical to a rebuild: the combine is deterministic). On a miss
    ///    it is rebuilt and stored.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn world_lightmap<'a>(
        &mut self,
        bsp: &'a Bsp,
        idx: usize,
        face: &crate::bsp::DFace,
        light_styles: &[f32; LIGHTSTYLES],
        torches: FaceTorches,
        dlights: &[crate::dlight::DynamicLight],
        // The face's `R_MarkLights` mask for this frame (see [`mark_dlights`]).
        dlightbits: u32,
    ) -> Option<LightMap<'a>> {
        let (scales, n_styles) = style_scales(face, light_styles);
        let dlit = any_dlight_reaches(bsp, face, dlights, dlightbits);
        let unkept;
        let poly: &[Vec3] = match self.geoms.get_mut(idx) {
            Some(slot) => slot.get_or_insert_with(|| face_poly_or_empty(bsp, face)),
            None => {
                unkept = face_poly_or_empty(bsp, face);
                &unkept
            }
        };
        let entry = self.lights.get_mut(idx);
        if dlit {
            // A dlight touches this face: rebuild every frame and DROP any cached
            // entry (so we never reuse a stale, dlight-free buffer next frame, and
            // never bake a moving dlight into the cache).
            if let Some(e) = entry {
                *e = None;
            }
            return face_lightmap_with(bsp, face, poly, light_styles, torches, dlights, dlightbits);
        }
        // No dlight: try the cache.
        let entry = match entry {
            Some(Some(e))
                if e.n_styles == n_styles && e.style_scales[..n_styles] == scales[..n_styles] && torches.key_is(&e.torches) =>
            {
                // HIT: clone the stored combined luxels (deterministic build ->
                // bit-identical to rebuilding).
                return Some(LightMap {
                    luxels: Luxels::Owned(e.luxels.clone()),
                    lmw: e.lmw,
                    lmh: e.lmh,
                    texmins: e.texmins,
                });
            }
            e => e,
        };
        // MISS: build fresh (no dlights -> the pure static/style combine), then
        // cache it if it is the owned combine.
        let built = face_lightmap_with(bsp, face, poly, light_styles, torches, &[], 0)?;
        if let (Luxels::Owned(v), Some(e)) = (&built.luxels, entry) {
            *e = Some(LightCacheEntry {
                style_scales: scales,
                n_styles,
                torches: torches.key(),
                luxels: v.clone(),
                lmw: built.lmw,
                lmh: built.lmh,
                texmins: built.texmins,
            });
        }
        Some(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlight::DynamicLight;
    use crate::render::{demo_room, Camera, Palette, Renderer, Scene};
    use super::super::light::face_lightmap_dyn;
    use crate::render::fixtures::render_once;
    use crate::render::fixtures::{lightmapped_demo_room, one_face_bsp_zplane, two_style_face_bsp};
    use crate::render::light::{ALL_DLIGHT_BITS, NEUTRAL_LIGHTSTYLE_SCALES};
    use crate::render::world::ExternalBModel;

    /// Caches for `bsp` whose face 0 has the polygon `poly`: the fixtures'
    /// faces are lit by a literal polygon, not their bsp's edges.
    fn caches_with_poly(bsp: &Bsp, poly: &[Vec3]) -> SurfaceCaches {
        let mut c = SurfaceCaches::default();
        c.begin_map(bsp.faces.len());
        c.geoms[0] = Some(poly.to_vec());
        c
    }

    /// A profiler that is on.
    fn counting() -> Profiler {
        let mut p = Profiler::default();
        p.begin();
        p
    }

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
        // A 2-style face: styles[0]=0 (steady), styles[1]=1 (animated). The owned
        // combine is cacheable (not a static borrow).
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;

        // First call: MISS -> builds + caches. Second call (same key): HIT.
        let mut caches = caches_with_poly(&bsp, &poly);
        let first = caches.world_lightmap(&bsp, 0, &face, &scales, FaceTorches::NONE, &[], 0).expect("present");
        let second = caches.world_lightmap(&bsp, 0, &face, &scales, FaceTorches::NONE, &[], 0).expect("present");

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
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut caches = caches_with_poly(&bsp, &poly);

        // Build at scale 0.5, then again at scale 1.0 (a torch ticking). The
        // second result must reflect the NEW scale, not the stale cached one.
        let mut s_half = NEUTRAL_LIGHTSTYLE_SCALES;
        s_half[1] = 0.5;
        let half = caches.world_lightmap(&bsp, 0, &face, &s_half, FaceTorches::NONE, &[], 0).expect("present");

        let mut s_full = NEUTRAL_LIGHTSTYLE_SCALES;
        s_full[1] = 1.0;
        let full = caches.world_lightmap(&bsp, 0, &face, &s_full, FaceTorches::NONE, &[], 0).expect("present");

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
    fn lightmap_cache_is_emptied_by_begin_map() {
        // World A: a 2-style face, populate the cache for face 0.
        let (bsp_a, face_a, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;
        let mut caches = caches_with_poly(&bsp_a, &poly);
        let _ = caches.world_lightmap(&bsp_a, 0, &face_a, &scales, FaceTorches::NONE, &[], 0).expect("present");

        // World B: a DIFFERENT world with different lightmap bytes at face 0.
        // `R_NewMap` empties the (face-index-keyed) cache, so face 0 is rebuilt
        // from B's data, NOT served from A's stale entry.
        let (mut bsp_b, face_b, poly_b) = two_style_face_bsp([0, 1, 255, 255], 40, 240);
        bsp_b.faces.push(face_b.clone());
        bsp_b.faces.push(face_b.clone());
        caches.begin_map(bsp_b.faces.len());
        caches.geoms[0] = Some(poly_b.clone());

        let got = caches.world_lightmap(&bsp_b, 0, &face_b, &scales, FaceTorches::NONE, &[], 0).expect("present");
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
        // A single steady style-0 face at neutral scale -> normally a static
        // borrow (uncached). A reaching dlight must still produce the owned,
        // dlit buffer (NOT a cached static-only buffer) every call.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);

        // First, populate any cache via a dlight-free neutral call (static borrow,
        // not cached). Then a reaching dlight: must own the buffer and brighten.
        let mut caches = caches_with_poly(&bsp, &poly);
        let _ = caches.world_lightmap(&bsp, 0, &face, &NEUTRAL_LIGHTSTYLE_SCALES, FaceTorches::NONE, &[], 0);
        let lit = caches
            .world_lightmap(&bsp, 0, &face, &NEUTRAL_LIGHTSTYLE_SCALES, FaceTorches::NONE, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
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
        let bsp = lightmapped_demo_room(100, 200);
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5; // non-neutral -> owned combine -> cache used
        let mut styles_b = NEUTRAL_LIGHTSTYLE_SCALES;
        styles_b[1] = 1.0;

        let mut r = Renderer::new();
        let mut render = |styles: &[f32; LIGHTSTYLES]| {
            r.render(&Scene { light_styles: styles, ..Scene::new(&bsp, cam, 160, 120, &pal) })
        };

        let frame1 = render(&styles); // populates caches
        let frame2 = render(&styles); // cache hits
        assert_eq!(frame1.pixels, frame2.pixels, "cached frame must be pixel-identical to the first");

        // A change in the style scale must change the cache key AND the pixels
        // (proving the cache is keyed on the scale, not stale).
        let frame_b = render(&styles_b);
        assert_ne!(
            frame1.pixels, frame_b.pixels,
            "a different style scale must rebuild and produce different pixels"
        );

        // Re-render at the ORIGINAL scale: must again equal frame1 (the cache
        // correctly rebuilt back to the 0.5 key).
        let frame3 = render(&styles);
        assert_eq!(frame1.pixels, frame3.pixels, "returning to the original key reproduces frame1");
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

    // -- The frame's bakes on the render threads (Bakes) --

    /// The bake test's scene at frame `k` of a run: the lightmapped room seen
    /// from a corner, its second style stepping, a light moving across it.
    fn baking_scene<'a>(world: &'a Bsp, pal: &'a Palette, cm: &'a [u8], styles: &'a [f32; LIGHTSTYLES], dls: &'a [DynamicLight]) -> Scene<'a> {
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        Scene { light_styles: styles, dlights: dls, colormap: Some(cm), ..Scene::new(world, cam, 211, 157, pal) }
    }

    fn baking_styles(k: usize) -> [f32; LIGHTSTYLES] {
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = [0.25, 0.25, 0.5, 1.0, 1.5, 1.5][k % 6];
        styles
    }

    fn baking_lights(k: usize) -> Vec<DynamicLight> {
        if k % 3 == 2 {
            return Vec::new();
        }
        vec![DynamicLight::new([-150.0 + 60.0 * k as f32, 0.0, -60.0], 250.0, f32::MAX, 0.0, 0.0, 0)]
    }

    /// The frame is the same on any number of threads — 1 bakes every block
    /// on the calling thread, more share the bakes out — and so is what the
    /// cache keeps; the frames bake enough to go to the threads.
    #[test]
    fn the_bakes_give_the_same_frame_and_cache_on_any_thread_count() {
        let world = demo_room_with_walls(lightmapped_demo_room(60, 90));
        let (cm, pal) = ramp_colormap();
        let run = |threads: usize| {
            let mut r = Renderer::new();
            r.set_threads(threads);
            let mut frames = Vec::new();
            let mut most = 0;
            for k in 0..8 {
                let (styles, dls) = (baking_styles(k), baking_lights(k));
                r.stats_begin();
                frames.push(r.render(&baking_scene(&world, &pal, &cm, &styles, &dls)).pixels);
                most = most.max(r.stats_end().surf_texels_baked as usize);
            }
            (frames, r.surfaces.block_entries(), most)
        };
        let (one, cache, most) = run(1);
        assert!(most > 100_000, "a frame bakes enough to share out ({most} texels)");
        for threads in [2, 3, 8, 16] {
            let (frames, entries, _) = run(threads);
            for (k, (a, b)) in frames.iter().zip(&one).enumerate() {
                assert!(a == b, "{threads} threads, frame {k}");
            }
            assert!(entries == cache, "{threads} threads: the cache");
        }
    }

    /// Warm or cold, the frame and the blocks are the same: a renderer that
    /// drew the run's earlier frames holds, for every block a fresh one bakes
    /// for the last frame, the same block.
    #[test]
    fn the_bakes_are_the_same_cold_or_warm() {
        let world = demo_room_with_walls(lightmapped_demo_room(60, 90));
        let (cm, pal) = ramp_colormap();
        let (mut warm, mut cold) = (Renderer::new(), Renderer::new());
        warm.set_threads(8);
        cold.set_threads(8);
        let last = 7;
        for k in 0..=last {
            let (styles, dls) = (baking_styles(k), baking_lights(k));
            let w = warm.render(&baking_scene(&world, &pal, &cm, &styles, &dls)).pixels;
            if k == last {
                assert!(w == cold.render(&baking_scene(&world, &pal, &cm, &styles, &dls)).pixels, "frame {k}");
            }
        }
        let held = warm.surfaces.block_entries();
        let fresh = cold.surfaces.block_entries();
        assert!(!fresh.is_empty());
        for e in &fresh {
            assert!(held.contains(e), "face {} mip {}: warm and cold differ", e.0, e.1);
        }
    }

    /// An inline model drawn by two entities in one frame is one face in the
    /// cache: e1m2's model 52 (the bars by the start) twice, lit by a
    /// dynamic light or not — a cold renderer's first frame shares bakes
    /// (its hits are the second copy's) and bakes each block it keeps once;
    /// on any thread count (when id's pak is here).
    #[test]
    fn a_face_two_entities_draw_is_baked_once() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let read = |n: &str| pak.read_file(n).expect("read").expect(n);
        let world = Bsp::parse(&read("maps/e1m2.bsp")).expect("e1m2");
        let pal = crate::render::parse_palette(&read("gfx/palette.lmp")).expect("palette");
        let cm = read("gfx/colormap.lmp");
        let cam = Camera { pos: [1496.0, 1664.0, 288.0], yaw: 270.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let at = |z: f32| crate::render::BModelInstance { model_index: 52, origin: [0.0, -200.0, z], frame: 0, angles: [0.0; 3] };
        let (one, two) = ([at(40.0)], [at(40.0), at(-40.0)]);
        // Every surface at mip 0, so the copies ask for the same blocks.
        let options = crate::render::RenderOptions { mip: MipCvars { mipscale: 0.0, mipcap: 0.0 }, ..Default::default() };
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        for dls in [Vec::new(), vec![DynamicLight::new([1496.0, 1464.0, 288.0], 300.0, f32::MAX, 0.0, 0.0, 0)]] {
            let frame = |bm: &[crate::render::BModelInstance], threads| {
                let mut r = Renderer::new();
                r.set_threads(threads);
                r.stats_begin();
                let scene = Scene { bmodels: bm, light_styles: &styles, dlights: &dls, colormap: Some(&cm), options, ..Scene::new(&world, cam, 320, 200, &pal) };
                let img = r.render(&scene).pixels;
                (img, r.stats_end(), r.surfaces.block_entries().len())
            };
            let (a, _, _) = frame(&one, 1);
            let (b, st, kept) = frame(&two, 1);
            assert!(a != b, "the second copy is drawn");
            assert!(st.surf_cache_hits > 0, "the copies share blocks");
            assert_eq!(st.surf_baked as usize, kept, "lit {}: every block baked once", !dls.is_empty());
            let (b8, st8, _) = frame(&two, 8);
            assert!(b8 == b && st8.surf_baked == st.surf_baked, "8 threads");
        }
    }

    /// The 2026 frame on a real map — e1m3's torch-lit flames (the torches
    /// flickering, every lit block rebaked each frame), a rocket's light
    /// moving through them — is the same on 1 to 16 threads, frame after
    /// frame, and its bakes went to the threads (when id's pak is here).
    #[test]
    fn a_torch_lit_view_is_the_same_on_any_thread_count() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let read = |n: &str| pak.read_file(n).expect("read").expect(n);
        let world = Bsp::parse(&read("maps/e1m3.bsp")).expect("e1m3");
        let pal = crate::render::parse_palette(&read("gfx/palette.lmp")).expect("palette");
        let cm = read("gfx/colormap.lmp");
        let cam = Camera { pos: [-1352.0, -720.0, -50.0], yaw: 90.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let options = crate::render::RenderOptions { video: crate::render::VideoCvars::MODERN, ..Default::default() };
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        let run = |threads: usize| {
            let mut r = Renderer::new();
            r.set_threads(threads);
            let mut frames = Vec::new();
            let mut most = 0u64;
            for k in 0..12 {
                let dl = [DynamicLight::new([-1352.0, -600.0 + 40.0 * k as f32, -40.0], 250.0, f32::MAX, 0.0, 0.0, 0)];
                let scene = Scene { time: 3.0 + k as f32 / 144.0, light_styles: &styles, dlights: &dl, colormap: Some(&cm), options, ..Scene::new(&world, cam, 480, 270, &pal) };
                r.stats_begin();
                frames.push(r.render(&scene).pixels);
                most = most.max(r.stats_end().surf_texels_baked);
            }
            (frames, most)
        };
        let (one, most) = run(1);
        assert!(most > 100_000, "the frames bake enough to share out ({most} texels)");
        for threads in [2, 3, 8, 16] {
            let (frames, _) = run(threads);
            for (k, (a, b)) in frames.iter().zip(&one).enumerate() {
                assert!(a == b, "{threads} threads, frame {k}");
            }
        }
    }

    /// A frame bakes only what it draws: looking down at the room's floor,
    /// the floor is baked and the ceiling behind the eye is not.
    #[test]
    fn nothing_undrawn_is_baked() {
        let world = demo_room_with_walls(lightmapped_demo_room(60, 90));
        let (cm, pal) = ramp_colormap();
        let cam = Camera::looking_at([0.0, -10.0, 120.0], [0.0, 0.0, -128.0], 90.0);
        let mut r = Renderer::new();
        r.render(&Scene { colormap: Some(&cm), ..Scene::new(&world, cam, 160, 120, &pal) });
        let baked: Vec<usize> = r.surfaces.block_entries().iter().map(|e| e.0).collect();
        assert!(baked.contains(&0) && !baked.contains(&1), "{baked:?}");
    }

    /// A frame's bakes shared by its threads: every job's block is its own
    /// bake, in the jobs' order, whether the threads took the jobs
    /// ([`Bakes::work`]), a span asked for one first ([`Bakes::block`]), or
    /// nobody did; the largest are taken first; a job the frame does not
    /// have reads as nothing.
    #[test]
    fn the_frames_bakes_are_each_jobs_own_whoever_bakes_them() {
        let (cm, _) = ramp_colormap();
        let tex: Vec<u8> = (0..64 * 64).map(|i| (i * 7 % 251) as u8).collect();
        let luxels: Vec<u8> = (0..9 * 9).map(|i| (i * 3) as u8).collect();
        let jobs = || -> Vec<BakeJob> {
            (1..=8usize)
                .map(|k| BakeJob {
                    lightmap: LightMap { luxels: Luxels::Static(&luxels), lmw: k + 1, lmh: 9 - k + 1, texmins: [0.0; 2] },
                    tex: &tex,
                    smax: 64,
                    tmax: 64,
                    texmins: [16 * k as i32, 0],
                    mip: 0,
                    bw: 16 * k,
                    bh: 16 * (9 - k),
                    colormap: &cm,
                })
                .collect()
        };
        let want: Vec<Arc<Vec<u8>>> = jobs().iter().map(BakeJob::bake).collect();
        assert!(want.iter().all(|b| !b.is_empty()) && want[0] != want[1]);
        // Nobody baked: `finish` does.
        assert!(Bakes::new(jobs()).finish() == want);
        // One thread takes them all, the largest first.
        let alone = Bakes::new(jobs());
        assert_eq!(alone.order[..2], [3, 4], "8 x 5 and 5 x 4 lightmap cells: 4 x 5 blocks of 16 first");
        alone.work();
        assert!(alone.blocks.iter().all(|b| b.get().is_some()));
        assert!(alone.block(2) == &want[2][..] && alone.block(8).is_empty());
        assert!(alone.finish() == want);
        // Several threads, some asking for blocks before the jobs are taken.
        for threads in [2, 3, 8] {
            let shared = Bakes::new(jobs());
            std::thread::scope(|s| {
                for t in 0..threads {
                    let (shared, want) = (&shared, &want);
                    s.spawn(move || {
                        assert!(shared.block(t) == &want[t][..], "asked for first");
                        shared.work();
                        assert!((0..8).all(|j| shared.block(j) == &want[j][..]));
                    });
                }
            });
            assert!(shared.finish() == want, "{threads} threads");
        }
    }

    #[test]
    fn external_models_bypass_and_dont_evict_world_surf_cache() {
        // REGRESSION (performance): external brush models (the b_*.bsp item boxes)
        // have face numbers of their own bsp, not the world's — they can NEVER
        // hit the world's surface cache. They must therefore BYPASS it: routing
        // them through the world's slots would evict the world's resident blocks
        // and reintroduce the ~60ms/frame re-bake-everything cost. Here we warm
        // the world cache, then render a frame containing MANY (26 > the old
        // broken 24-slot LRU) distinct external models, and assert (a) the
        // externals baked (went through the bypass, not the cache) and (b) the
        // world cache is completely untouched afterwards.
        let world = demo_room_with_walls(lightmapped_demo_room(100, 200));
        // 26 distinct external "boxes" (each a separate Bsp -> distinct fingerprint),
        // standing 2 units toward the camera's corner and 2 up from the world's
        // origin, so their inward floor and far walls are just in front of the
        // world's and are drawn (the edge renderer draws only the nearest).
        let ext_bsps: Vec<Bsp> = (0..26)
            .map(|k| demo_room_with_walls(lightmapped_demo_room(40 + k as u8, 220)))
            .collect();
        let externals: Vec<ExternalBModel> =
            ext_bsps.iter().map(|b| ExternalBModel { bsp: b, origin: [-2.0, -2.0, 2.0] }).collect();

        let pal = crate::render::fixtures::ramp_palette();
        let colormap = vec![0u8; COLORMAP_LEN]; // present -> the surf-cache path is active
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let mut r = Renderer::new();
        let mut render = |ext: &[ExternalBModel]| {
            r.stats_begin();
            let scene = Scene { external: ext, light_styles: &styles, colormap: Some(&colormap), ..Scene::new(&world, cam, 160, 120, &pal) };
            let _ = r.render(&scene);
            r.stats_end()
        };

        let _ = render(&[]); // bake the world's surface blocks into the cache

        // Sanity: the world scene hits its warm cache (else the asserts below are vacuous).
        let warm = render(&[]);
        assert!(
            warm.surf_cache_hits > 0 && warm.surf_baked == 0,
            "world scene must hit the warm surf cache (got {} hits, {} bakes)",
            warm.surf_cache_hits, warm.surf_baked
        );
        let world_hits = warm.surf_cache_hits;

        // A frame with 26 distinct external models: every world face drawn still
        // hits (the edge renderer draws only the surfaces that own a span, and
        // the boxes stand over the world's own walls, so some world faces are not
        // drawn at all), and the externals BAKE (proving they took the bypass
        // path, not the cache).
        let with_ext = render(&externals);
        assert!(
            with_ext.surf_cache_hits > 0 && with_ext.surf_cache_hits <= world_hits,
            "the world's drawn faces must still hit while externals draw (got {} of {})",
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
        let after = render(&[]);
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
        let world = demo_room_with_walls(lightmapped_demo_room(40, 0));
        let (cm, pal) = ramp_colormap();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 300.0, f32::MAX, 0.0, 0.0, 0);
        fn scene<'a>(world: &'a Bsp, cam: Camera, pal: &'a Palette, cm: &'a [u8], dls: &'a [DynamicLight]) -> Scene<'a> {
            Scene { dlights: dls, colormap: Some(cm), ..Scene::new(world, cam, 160, 120, pal) }
        }
        let mut r = Renderer::new();
        let mut render = |dls: &[DynamicLight]| {
            r.stats_begin();
            let img = r.render(&scene(&world, cam, &pal, &cm, dls));
            (img, r.stats_end())
        };
        let (unlit, st0) = render(&[]);
        let (lit, st) = render(std::slice::from_ref(&dl));
        assert_ne!(lit.pixels, unlit.pixels, "the light must show");
        // demo_room's walls whose texinfo maps them to a line (zero extent) have
        // no block at all (id's `D_SCAlloc` would `Sys_Error` on them); the light
        // must not send any other face to the per-pixel path.
        assert_eq!(st.surf_misses, st0.surf_misses, "no dlit face may fall back to the per-pixel path");
        assert!(st.surf_hits > 0);
        assert!(st.surf_baked > 0, "dlit faces are baked with the light");
        // Lit again: a dlit block is never reused (the light may have moved).
        let (lit2, st2) = render(std::slice::from_ref(&dl));
        assert_eq!(lit2.pixels, lit.pixels);
        assert_eq!(st2.surf_baked, st.surf_baked, "every dlit face rebakes every lit frame");
        // The light dies: exactly those entries rebuild, and the frame is unlit.
        let (after, st3) = render(&[]);
        assert_eq!(after.pixels, unlit.pixels, "the light must not linger in the cache");
        assert_eq!(st3.surf_baked, st.surf_baked, "the dlit entries rebuild without the light");
        let (_, st4) = render(&[]);
        assert_eq!(st4.surf_baked, 0, "then the cache is warm again");
        // History-free: a cold cache draws the same lit frame.
        assert_eq!(render_once(&scene(&world, cam, &pal, &cm, std::slice::from_ref(&dl))).pixels, lit.pixels);
    }

    /// `D_CacheSurface` keys a block on its texture (`cache->texture`): an
    /// animated wall's next frame rebuilds the block instead of showing the
    /// first frame forever.
    #[test]
    fn animated_wall_texture_rebuilds_its_cached_block() {
        use crate::bsp::{MipTex, TexAnim};
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
        let scene = |time| Scene { time, colormap: Some(&cm), ..Scene::new(&world, cam, 160, 120, &pal) };
        let mut r = Renderer::new();
        let f0 = r.render(&scene(0.0));
        let f1 = r.render(&scene(0.2));
        assert_ne!(f0.pixels, f1.pixels, "the animation's second frame must show");
        assert_eq!(r.render(&scene(0.0)).pixels, f0.pixels, "and the first again");
        assert_eq!(render_once(&scene(0.2)).pixels, f1.pixels, "a warm cache draws what a cold one does");
    }

    // -- Mip levels (D_MipLevelForScale, D_CacheSurface per miplevel) -------

    /// `D_MipLevelForScale`: `basemip` {1, 0.4, 0.2} times `d_mipscale`, and
    /// never finer than `d_mipcap`.
    #[test]
    fn mip_level_for_scale_is_d_mip_level_for_scale() {
        let mv = MipView::new(160.0, 160.0, MipCvars::DEFAULT);
        let levels: Vec<u32> =
            [5.0, 1.0, 0.999, 0.4, 0.399, 0.2, 0.199, 0.0].iter().map(|&s| mv.level_for_scale(s)).collect();
        assert_eq!(levels, [0, 0, 1, 1, 2, 2, 3, 3]);
        // d_mipscale 0: every scale (>= 0) is mip 0.
        let level = |mipscale, mipcap, scale| MipView::new(160.0, 160.0, MipCvars { mipscale, mipcap }).level_for_scale(scale);
        assert_eq!(level(0.0, 0.0, 0.0), 0);
        // d_mipcap 2 (and 9, clamped to 3): never finer than that.
        assert_eq!(level(1.0, 2.0, 5.0), 2);
        assert_eq!(level(1.0, 9.0, 5.0), 3);
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

    /// `D_DrawSurfaces`' level for a surface's `nearzi`: `nearzi *
    /// scale_for_mip * mipadjust` through `D_MipLevelForScale`.
    #[test]
    fn the_level_comes_from_nearzi_scale_for_mip_and_mipadjust() {
        let mv = MipView::new(100.0, 100.0, MipCvars::DEFAULT);
        let ti = crate::bsp::TexInfo { vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]], miptex: 0, flags: 0 };
        // scale = (1/30) * 100 * 1 = 3.3 -> mip 0; at 10x the distance 0.33 -> mip 2.
        assert_eq!(mv.level_for_nearzi(1.0 / 30.0, &ti), 0);
        assert_eq!(mv.level_for_nearzi(1.0 / 300.0, &ti), 2);
        // Nothing of the face in view: nearzi 0, the coarsest level.
        assert_eq!(mv.level_for_nearzi(0.0, &ti), 3);
        // Short texture axes (mipadjust 4) scale it up: 1.33 -> mip 0.
        let big = crate::bsp::TexInfo { vecs: [[0.25, 0.0, 0.0, 0.0], [0.0, 0.25, 0.0, 0.0]], miptex: 0, flags: 0 };
        assert_eq!(mv.level_for_nearzi(1.0 / 300.0, &big), 0);
        // The larger of the two scales is scale_for_mip (pixels taller than wide).
        assert_eq!(MipView::new(100.0, 300.0, MipCvars::DEFAULT).level_for_nearzi(1.0 / 300.0, &ti), 0);
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
        let (cm, _) = ramp_colormap();
        let mt = leveled_miptex();
        // A 64x48-texel surface: 5x4 luxels, texturemins (-16, 32).
        let luxels = vec![128u8; 5 * 4];
        let lm = LightMap { luxels: Luxels::Static(&luxels), lmw: 5, lmh: 4, texmins: [-16.0, 32.0] };
        let (_bsp, face, _) = one_face_bsp_zplane(128);
        let styles = NEUTRAL_LIGHTSTYLE_SCALES;
        let req = |slot, mt, mip| SurfaceRequest {
            slot,
            face: &face,
            texture: 0,
            mt,
            lightmap: lm.clone(),
            colormap: &cm,
            light_styles: &styles,
            torches: FaceTorches::NONE,
            dlit: false,
            mip,
        };
        let mut caches = SurfaceCaches::default();
        caches.begin_map(1);
        let mut prof = counting();
        let mut get = |mip: u32, prof: &mut Profiler| caches.surface_now(req(Some(0), &mt, mip), prof).expect("block");
        for mip in 0..4u32 {
            let sb = get(mip, &mut prof);
            assert_eq!(sb.mip, mip);
            assert_eq!((sb.bw, sb.bh), (64 >> mip, 48 >> mip));
            assert_eq!(sb.texmins, [(-16 >> mip) as f32, (32 >> mip) as f32]);
            // Uniform luxel 128: blocklights 128*256, t = (65280 - 32768) >> 2 =
            // 8128, colormap row 31 of the level's texel (`ramp_colormap`: +3
            // per row).
            let want = (10 * mip + 1 + 3 * 31) as u8;
            assert!(sb.block.iter().all(|&p| p == want), "mip {mip}");
        }
        let cold = prof.end();
        assert_eq!((cold.surf_baked, cold.surf_cache_hits), (4, 0));
        assert_eq!(cold.surf_texels_baked, 64 * 48 + 32 * 24 + 16 * 12 + 8 * 6);
        let mut prof = counting();
        for mip in [2u32, 0, 3, 1] {
            let _ = get(mip, &mut prof);
        }
        let warm = prof.end();
        assert_eq!((warm.surf_baked, warm.surf_cache_hits), (0, 4), "every level stays cached");
        let (bytes, blocks) = caches.usage();
        assert_eq!((bytes, blocks), (64 * 48 + 32 * 24 + 16 * 12 + 8 * 6, 4));
        // A texture without levels 1..3 is baked at mip 0 whatever is asked.
        let mut flat = leveled_miptex();
        flat.mips = Default::default();
        let sb = caches.surface_now(req(None, &flat, 2), &mut prof).expect("block");
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
    /// [`draw_surface_block`] as it was first written, the block size a
    /// variable and every row finding its texture row: the reference the
    /// unrolled one is held to.
    #[allow(clippy::too_many_arguments)]
    fn draw_surface_block_as_written(
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

    #[test]
    fn the_unrolled_block_bake_is_the_one_as_written() {
        // Every mip level, textures that are and are not 16-aligned (the
        // wrap inside a block), texturemins either side of zero, lights
        // across their whole range (64..=16320) and blocks at the tile's
        // seams: the same bytes.
        let (cm, _) = ramp_colormap();
        let mut seed = 12345u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 8
        };
        let mut compared = 0;
        for &(smax0, tmax0) in &[(64usize, 64usize), (16, 32), (128, 16), (24, 40), (48, 24)] {
            for mip in 0..4u32 {
                let (smax, tmax) = (smax0 >> mip, tmax0 >> mip);
                let tex: Vec<u8> = (0..smax * tmax).map(|_| next() as u8).collect();
                for &(lmw, lmh) in &[(2usize, 2usize), (5, 3), (9, 17)] {
                    for &texmins in &[[0, 0], [-48, 16], [112, -160], [16 * 7, 16 * 3]] {
                        let light: Vec<i32> = (0..lmw * lmh).map(|_| 64 + (next() % (16320 - 64 + 1)) as i32).collect();
                        let (bw, bh) = (((lmw - 1) * 16) >> mip, ((lmh - 1) * 16) >> mip);
                        let (mut want, mut got) = (vec![7u8; bw * bh], vec![7u8; bw * bh]);
                        draw_surface_block_as_written(&tex, smax, tmax, texmins, mip, &light, lmw, &cm, &mut want, bw, bh);
                        draw_surface_block(&tex, smax, tmax, texmins, mip, &light, lmw, &cm, &mut got, bw, bh);
                        assert!(got == want, "{smax}x{tmax} mip {mip}, lightmap {lmw}x{lmh}, texturemins {texmins:?}");
                        compared += bw * bh;
                    }
                }
            }
        }
        assert!(compared > 500_000);
    }

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
        let (cm, _) = ramp_colormap();
        let mt = leveled_miptex();
        let (_bsp, face, _) = one_face_bsp_zplane(128);
        // A 64x48 surface whose luxels a light has pushed up unevenly.
        let luxels: Vec<f32> = (0..5 * 4).map(|i| 100.0 + 7.0 * i as f32 + (i % 3) as f32 / 256.0).collect();
        let lm = LightMap { luxels: Luxels::Owned(luxels), lmw: 5, lmh: 4, texmins: [-16.0, 32.0] };
        let mut light = Vec::new();
        lm.blocklights_into(&mut light);
        let req = |mip| SurfaceRequest {
            slot: Some(0),
            face: &face,
            texture: 0,
            mt: &mt,
            lightmap: lm.clone(),
            colormap: &cm,
            light_styles: &NEUTRAL_LIGHTSTYLE_SCALES,
            torches: FaceTorches::NONE,
            dlit: true,
            mip,
        };
        let mut caches = SurfaceCaches::default();
        caches.begin_map(1);
        let mut prof = counting();
        for mip in 0..4u32 {
            let sb = caches.surface_now(req(mip), &mut prof).expect("block");
            let (bw, bh) = (64 >> mip, 48 >> mip);
            let mut want = vec![0u8; bw * bh];
            let level = mt.mip(mip as usize).expect("level");
            draw_surface_block(level, 32 >> mip, 32 >> mip, [-16, 32], mip, &light, 5, &cm, &mut want, bw, bh);
            assert_eq!((sb.mip, sb.bw, sb.bh), (mip, bw, bh));
            assert_eq!(*sb.block, want, "mip {mip}");
            // Lit again: rebaked, never a hit.
            let _ = caches.surface_now(req(mip), &mut prof);
        }
        let st = prof.end();
        assert_eq!((st.surf_baked, st.surf_cache_hits), (8, 0));
    }

    #[test]
    fn world_render_unaffected_by_intervening_different_world() {
        // Render world A, then a DIFFERENT world B (the same shape, different
        // lighting: a changelevel to it calls `begin_map`), then world A again.
        // The two renders of A must be byte-identical — proving the renderer
        // never serves B's cached data for A's faces.
        let a = lightmapped_demo_room(100, 200);
        let b = lightmapped_demo_room(60, 240); // different lighting bytes
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let mut r = Renderer::new();
        let mut render = |bsp: &Bsp| {
            r.begin_map(bsp);
            r.render(&Scene { light_styles: &styles, ..Scene::new(bsp, cam, 160, 120, &pal) })
        };

        let a1 = render(&a);
        let b1 = render(&b); // R_NewMap for world B
        let a2 = render(&a); // must rebuild A's caches, not reuse B's
        assert_eq!(a1.pixels, a2.pixels, "world A renders identically before and after world B");
        assert_ne!(a1.pixels, b1.pixels, "and B is not A");
    }

    #[test]
    fn face_geom_cache_matches_uncached_build_and_is_emptied_by_begin_map() {
        let (bsp, face, _poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut caches = SurfaceCaches::default();
        caches.begin_map(bsp.faces.len());
        // The cached poly must equal a direct face_world_poly reconstruction
        // (face_world_poly walks the BSP edge tables, so this — not the helper's
        // literal `poly` used for lightmap math — is the geometry the loop sees).
        let mut direct = Vec::new();
        assert!(face_world_poly(&bsp, &face, &mut direct));
        assert_eq!(caches.geom(&bsp, 0, &face), Some(&direct[..]));
        assert_eq!(caches.geom(&bsp, 0, &face), Some(&direct[..]), "the cached one");
        assert_eq!(caches.geom(&bsp, bsp.faces.len(), &face), None, "no slot past the map's faces");
        // A different world: begin_map empties it, and face 0 is B's.
        let (mut bsp_b, face_b, _polyb) = two_style_face_bsp([0, 255, 255, 255], 50, 50);
        bsp_b.faces.push(face_b.clone());
        caches.begin_map(bsp_b.faces.len());
        let mut direct_b = Vec::new();
        assert!(face_world_poly(&bsp_b, &face_b, &mut direct_b));
        assert_eq!(caches.geom(&bsp_b, 0, &face_b), Some(&direct_b[..]));
    }
}

