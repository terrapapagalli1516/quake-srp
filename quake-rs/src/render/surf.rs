//! Surfaces: face geometry, texture animation, and the per-face caches.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_surf.c` (`R_TextureAnimation`) and `WinQuake/d_surf.c`
//! (the lit surface cache, `D_CacheSurface`); the port adds the static-geometry
//! and lightmap caches the world pass reuses between frames.

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::light::{
    any_dlight_reaches, colormap_row, face_lightmap_dyn, LightMap, Luxels, COLORMAP_LEN,
    LIGHTSTYLES, STYLE_NONE,
};
use super::stats::stat;

/// Reconstruct a face's world-space polygon into `out`. Returns false if any
/// index is out of range (the caller then skips the face). Mirrors the
/// surfedge/edge/vertex walk in [`render_bsp`].
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

/// One cached lit SURFACE block (Quake's `d_surf.c` surface cache entry, mip 0):
/// the face's texture with the lightmap shaded in AND resolved through the colormap
/// to a final palette index, one byte per surface texel. The rasteriser then reads
/// a single byte per screen pixel (then one palette lookup) instead of sampling the
/// texture, bilinear-interpolating the lightmap, and indexing the colormap per
/// pixel — moving all of that to a per-texel bake done ONCE and reused every frame.
#[derive(Clone)]
struct SurfCacheEntry {
    /// Active styles' resolved scale values at bake time (the cache key, same as the
    /// lightmap cache — a torch tick rebuilds the block).
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// Baked palette indices, `bw * bh`, row-major. `Rc` so a frame's draw clones
    /// the handle (a refcount bump), not the (possibly large) buffer.
    block: std::rc::Rc<Vec<u8>>,
    bw: usize,
    bh: usize,
    /// Surface-space origin of the block (`s = texmins[0] + i`, `t = texmins[1] + j`).
    texmins: [f32; 2],
}

/// The world model's lit-surface cache: its [`WorldFingerprint`] identity plus a
/// per-face slot (rebuilt when an animated style ticks). Held as a single
/// [`SURF_CACHE`] slot — only the world `Bsp` is ever cached (external brush
/// models bypass it), so it self-invalidates on a changelevel via the
/// fingerprint/`n_faces` check; see [`face_surf_block`].
pub(super) struct SurfCache {
    fingerprint: WorldFingerprint,
    entries: Vec<Option<SurfCacheEntry>>,
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
}

/// Maximum baked surface-cache block, in texels. A face larger than this stays on
/// the per-pixel lighting path, so one pathological giant surface can't allocate a
/// multi-MB block; virtually every real id1 face is far smaller.
const SURF_BLOCK_MAX: usize = 1 << 20;

/// A baked surface block handle: `(texels, width, height, surface origin)`.
type SurfBlockRef = (std::rc::Rc<Vec<u8>>, usize, usize, [f32; 2]);

/// Build (and cache) a world face's lit+colormapped surface block (mip 0). Returns
/// the `Rc` handle + dimensions + surface origin, or `None` (caller keeps the
/// per-pixel path) when there is no usable colormap, the face is dynamically lit
/// (`dlit` — the moving dlight can't be baked), the texture is missing, or the
/// block would exceed [`SURF_BLOCK_MAX`].
///
/// FIDELITY: at mip 0 the block is 1:1 with surface texels, so the sampled texture
/// texel is identical to the per-pixel path; only the lighting is sampled at texel
/// centres (then nearest-read per pixel) rather than per screen pixel — which is
/// exactly what Quake's surface cache does (`R_BuildLightMap` + `D_DrawSurfaceBlock8`).
#[allow(clippy::too_many_arguments)]
pub(super) fn face_surf_block(
    idx: usize,
    face: &crate::bsp::DFace,
    mt: &crate::bsp::MipTex,
    lm: &LightMap,
    colormap: &[u8],
    fp: WorldFingerprint,
    n_faces: usize,
    light_styles: &[f32; LIGHTSTYLES],
    dlit: bool,
    cache_surf: bool,
) -> Option<SurfBlockRef> {
    if dlit || colormap.len() < COLORMAP_LEN {
        return None;
    }
    let (tw, th) = (mt.width as usize, mt.height as usize);
    if tw == 0 || th == 0 || mt.pixels.len() < tw.saturating_mul(th) {
        return None;
    }
    // The block spans the surface's texture-space extent at full resolution. lmw/lmh
    // are extent/16 + 1 luxels, so the texel extent is (lmw-1)*16 + 1.
    let bw = lm.lmw.saturating_sub(1).saturating_mul(16).saturating_add(1);
    let bh = lm.lmh.saturating_sub(1).saturating_mul(16).saturating_add(1);
    let total = bw.checked_mul(bh)?;
    if bw == 0 || bh == 0 || total > SURF_BLOCK_MAX {
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

    // The deterministic bake: for each surface texel (i,j) take the tiled base
    // texel, shade it by the lightmap factor at the texel centre, pick the colormap
    // row, and store the final palette index. Identical inputs -> identical bytes,
    // so a cached block is bit-for-bit equal to a fresh one.
    let texmins = lm.texmins;
    let bake = || -> std::rc::Rc<Vec<u8>> {
        let (tmi0, tmi1) = (texmins[0] as i64, texmins[1] as i64);
        let mut block = vec![0u8; total];
        for j in 0..bh {
            let ty = ((tmi1 + j as i64).rem_euclid(th as i64)) as usize;
            let tf = texmins[1] + j as f32;
            for i in 0..bw {
                let tx = ((tmi0 + i as i64).rem_euclid(tw as i64)) as usize;
                let texel = mt.pixels[ty * tw + tx] as usize;
                let bri = lm.factor_at(texmins[0] + i as f32, tf);
                let row = colormap_row(bri);
                block[j * bw + i] = colormap[row * 256 + texel];
            }
        }
        std::rc::Rc::new(block)
    };

    // EXTERNAL brush models (the b_*.bsp ammo/health/explosive boxes) bypass the
    // cache. The game clones each item's `Bsp` per visible instance every frame, so
    // its `WorldFingerprint` (keyed on the `&Bsp` pointer) is different every frame
    // and every instance — it could never produce a cache hit, and routing it
    // through the shared slot would only evict the world's resident cache (the bug
    // this guard prevents). They are tiny (a 6-face box) so an unconditional bake is
    // cheap; bake fresh and return without touching SURF_CACHE.
    if !cache_surf {
        stat(|s| s.surf_bypass_baked += 1);
        return Some((bake(), bw, bh, texmins));
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
                entries: vec![None; n_faces],
            });
        }
        let sc = slot.as_mut().expect("just initialised");
        // HIT: same face, same resolved style scales -> reuse the baked block.
        if let Some(e) = sc.entries.get(idx).and_then(|e| e.as_ref()) {
            if e.n_styles == n_styles
                && e.bw == bw
                && e.bh == bh
                && e.style_scales[..n_styles] == scales[..n_styles]
            {
                stat(|s| s.surf_cache_hits += 1);
                return Some((e.block.clone(), e.bw, e.bh, e.texmins));
            }
        }
        // MISS: bake once and store.
        stat(|s| s.surf_baked += 1);
        let block = bake();
        if let Some(e) = sc.entries.get_mut(idx) {
            *e = Some(SurfCacheEntry {
                style_scales: scales,
                n_styles,
                block: block.clone(),
                bw,
                bh,
                texmins,
            });
        }
        Some((block, bw, bh, texmins))
    })
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
    use crate::render::{demo_room, render_scene_ext, Camera, ExternalBModel};
    use crate::render::fixtures::{
        lightmapped_demo_room, one_face_bsp_zplane, reset_render_caches, two_style_face_bsp,
    };
    use crate::render::light::{ALL_DLIGHT_BITS, NEUTRAL_LIGHTSTYLE_SCALES};
    use crate::render::stats::{render_stats_begin, render_stats_end};

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
