//! Brush models: the world, inline submodels and external `b_*.bsp` items.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_bsp.c` (`R_RecursiveWorldNode`, `R_DrawSubmodelPolygons`)
//! and the brush branch of `R_DrawBEntitiesOnList` (`r_main.c`).

use crate::bsp::Bsp;
use crate::math::{dot, normalize, sub, Vec3};
use super::{Camera, Image};
use super::light::{
    any_dlight_reaches, face_lightmap_dyn, mark_dlights, DLIGHT_BITS_SCRATCH, LIGHTSTYLES,
};
use super::raster::{
    hash_color, raster_triangle, raster_triangle_cached, raster_triangle_tex, ProjT, Projected,
    SurfaceMode,
};
use super::sky::{SkySpans, SkyView, SKY_SPANS_SCRATCH};
use super::stats::{stat, stats_on, StatInstant};
use super::surf::{
    classify_surface, face_geom_cached, face_lightmap_world_cached, face_normal, face_surf_block,
    face_world_poly, texture_animation, SurfKind, WorldFingerprint,
};
use super::vis::{clip_poly_near_into, compute_visible_faces, Frustum, VView};
use super::warp::TurbTable;

/// The textured world pass, factored out of [`render_bsp_textured`](super::render_bsp_textured) so it can
/// share an image + z-buffer with the alias-model pass (see [`render_scene`](super::render_scene)).
///
/// Rasterises every visible BSP face into `image`/`zbuf` exactly as the original
/// `render_bsp_textured` body did: same camera basis, focal length, projection,
/// near clip, backface cull, perspective-correct texturing, and flat fallback.
/// The caller owns the framebuffers, so models drawn afterward occlude (and are
/// occluded by) the world through the shared depth buffer.
///
/// Before the per-face loop it computes the camera's PVS via
/// [`compute_visible_faces`]: when the map has visibility data and the camera is
/// in a real (non-solid) leaf, faces outside the potentially-visible set are
/// skipped. Maps with no visibility lump (e.g. [`demo_room`](super::demo_room)) get the full draw,
/// so existing behaviour is unchanged there.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_world_textured(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
    turb: &TurbTable,
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    colormap: Option<&[u8]>,
) {
    // The near plane lives in `clip_poly_near` (`NEAR_PLANE`); this pass clips the
    // polygon to it rather than dropping any face that touches it.
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 {
        return;
    }
    let (forward, right, up) = cam.basis();
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
    let tan_half = half_fov.tan();
    let focal = if tan_half.abs() < 1e-6 {
        cx
    } else {
        (cx as f64 / tan_half) as f32
    };
    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);

    // Sub-phase profiling: accumulate ns into locals (cheap), flush to RenderStats
    // at function end. Only the `prof` path pays the Instant cost; the live game
    // (stats off) skips all of it.
    let prof = stats_on();
    let mut t_sort: u64 = 0;
    let mut t_light: u64 = 0;
    let mut t_surf: u64 = 0;
    let _t_pvs = prof.then(StatInstant::now);

    // PVS culling: a per-face visibility mask for the camera's leaf, or `None`
    // when there is no usable PVS (no vis lump, solid/outside leaf, malformed) —
    // in which case every face is drawn (the pre-PVS behaviour).
    let visible_face = compute_visible_faces(bsp, cam.pos);

    // View frustum (4 sides + near) for `R_CullBox`-style AABB rejection. Built
    // once per frame from the camera + aspect; see [`Frustum::from_camera`] for
    // the exact match to the rasteriser's screen rectangle (so it never culls a
    // face that could draw a pixel).
    let frustum = Frustum::from_camera(cam, w, h);
    let t_pvs = _t_pvs.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);

    // The world pass draws ONLY model 0's faces. Brush submodels (doors, plats,
    // buttons) own the remaining faces and are drawn by `draw_submodel` at their
    // entity origin — otherwise they'd render here at their local (closed)
    // position AND again, doubled, at the entity origin. Fall back to "all faces"
    // when models[0] is absent (the pre-submodel behaviour / demo maps).
    let (world_first, world_end) = match bsp.models.first() {
        Some(m) => {
            let f0 = m.firstface.max(0) as usize;
            let n = m.numfaces.max(0) as usize;
            (f0, f0.saturating_add(n).min(bsp.faces.len()))
        }
        None => (0, bsp.faces.len()),
    };

    let mut views: Vec<VView> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();
    // Reused near-clip output buffer (cleared per face) so the per-face clip
    // allocates nothing on the common in-front-of-near fast path.
    let mut clipped: Vec<VView> = Vec::new();

    // World fingerprint + face count for the lit-surface cache (keyed per face).
    let fp = WorldFingerprint::of(bsp);
    let n_faces = bsp.faces.len();

    // `R_PushDlights` (`r_light.c`): mark every face each live dynamic light
    // reaches by recursing the BSP from the world model's root node. A face's
    // mask gates which lights `add_dynamic_lights` may fold into it, so a light
    // whose sphere never crosses into a region (e.g. the far side of a wall)
    // cannot brighten faces there. The scratch is thread-local and reused; with
    // no live dlights it stays empty and every face reads mask 0.
    let mut dlight_bits = DLIGHT_BITS_SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()));
    // The sky is drawn span by span once the brush passes are done
    // (`resolve_sky_spans`); this frame's sky pixels are recorded here.
    let sky_view = SkyView::new(forward, right, up, w, h, time);
    let sky_spans = std::cell::RefCell::new(
        SKY_SPANS_SCRATCH.with(|b| std::mem::replace(&mut *b.borrow_mut(), SkySpans::EMPTY)),
    );
    sky_spans.borrow_mut().reset(w, h);
    sky_spans.borrow_mut().view = Some(sky_view);
    let world_headnode = bsp
        .models
        .first()
        .and_then(|m| m.headnode.first().copied())
        .unwrap_or(0);
    mark_dlights(bsp, world_headnode, dlights, &mut dlight_bits);

    // CULL, THEN FRONT-TO-BACK ORDER. With a z-buffer the final image is identical
    // for ANY draw order (the nearest surface always wins the depth test), but
    // drawing near faces FIRST lets the z-test reject occluded pixels BEFORE the
    // per-pixel shading (block read + framebuffer write) — cutting the ~1.9x world
    // overdraw the profiler measured. Only the faces that survive the PVS and
    // frustum culls are keyed and sorted (~700-860 of e1m3's 5,059), by squared
    // centroid distance, ascending. `sort_by` is stable, so the survivors come out
    // in exactly the order the old sort-everything-then-cull pass drew them: a
    // stable sort's order is (key, original index), and dropping faces from its
    // input does not reorder the rest. (A true BSP front-to-back walk would be
    // marginally better; the centroid sort captures the bulk of the win.)
    let _t_sort = prof.then(StatInstant::now);
    let mut world_order: Vec<(f32, usize)> = Vec::new();
    for fi in world_first..world_end {
        let face = match bsp.faces.get(fi) {
            Some(f) => f,
            None => continue,
        };
        stat(|s| s.faces_total += 1);
        // Skip faces outside the potentially-visible set. A missing mask entry
        // (or no mask at all) means "draw" — culling never removes a face it is
        // unsure about.
        if let Some(mask) = &visible_face {
            if !mask.get(fi).copied().unwrap_or(true) {
                stat(|s| s.faces_pvs_culled += 1);
                continue;
            }
        }
        // Static per-face geometry (poly / normal / centroid / AABB), built once
        // for the world model and reused every frame. `bad` reproduces the
        // original `face_world_poly` early-out exactly.
        let g = face_geom_cached(bsp, fi, face);
        if g.bad {
            continue;
        }
        // FRUSTUM CULL (R_CullBox): reject faces whose static world AABB is fully
        // outside the view, BEFORE projection / lightmap / raster. Conservative —
        // a face touching the view survives. A culled face draws nothing.
        if frustum.culls(g.mins, g.maxs) {
            stat(|s| s.faces_frustum_culled += 1);
            continue;
        }
        let d = sub(g.center, cam.pos);
        world_order.push((dot(d, d), fi));
    }
    world_order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(t) = _t_sort { t_sort += t.elapsed().as_nanos() as u64; }

    let _t_body = prof.then(StatInstant::now);
    for &(_, face_index) in &world_order {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        // Already fetched (and culled) above: an `Rc` refcount bump.
        let geom = face_geom_cached(bsp, face_index, face);

        let world_poly: &[Vec3] = &geom.poly;
        let normal = match geom.normal {
            Some(n) => n,
            None => continue,
        };

        // Face center for the backface cull (cached centroid).
        let center = geom.center;
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture. For a `+`-prefixed animated
        // texture, the miptex actually sampled is chosen by time via
        // `R_TextureAnimation` (the world entity's frame is 0, so it never takes
        // the alternate cycle). The (s,t) axes still come from the texinfo, which
        // are shared by every frame of the animation.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            let anim_mi = texture_animation(bsp, mi, 0, time);
            bsp.textures.get(anim_mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name so the
        // animated special surfaces route to the warp/scroll sampler. Liquids
        // and sky are fullbright and NOT lightmapped, so only walls compute a
        // baked static lightmap.
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        // This face's `R_MarkLights` mask (0 = no dynamic light reaches it).
        let face_dlightbits = dlight_bits.get(face_index).copied().unwrap_or(0);
        let _t_l = prof.then(StatInstant::now);
        let lightmap = if kind == SurfKind::Normal {
            // Goes through the lightmap SURFACE CACHE (R_BuildLightMap cache):
            // reuses the combined luxel buffer while the resolved style scales
            // are unchanged and no dynamic light touches the face. The cached
            // buffer is bit-identical to a fresh build.
            face_lightmap_world_cached(bsp, face_index, face, world_poly, light_styles, dlights, face_dlightbits)
        } else {
            None
        };
        if let Some(t) = _t_l { t_light += t.elapsed().as_nanos() as u64; }
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky { view: sky_view, defer: Some((&sky_spans, face_index as u32 + 1)) },
        };

        // Build the view-space polygon (vx,vy,vz,s,t per world vertex), then clip
        // it against the near plane. A face fully in front is returned unchanged
        // (no regression); a face fully behind yields < 3 verts and is skipped;
        // a straddling face is clipped to `vz == NEAR` and rasterised normally.
        views.clear();
        for v in world_poly {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            let (s, t) = match ti {
                Some(ti) => (
                    v[0] * ti.vecs[0][0] + v[1] * ti.vecs[0][1] + v[2] * ti.vecs[0][2] + ti.vecs[0][3],
                    v[0] * ti.vecs[1][0] + v[1] * ti.vecs[1][1] + v[2] * ti.vecs[1][2] + ti.vecs[1][3],
                ),
                None => (0.0, 0.0),
            };
            views.push(VView { vx, vy, vz, s, t });
        }
        clip_poly_near_into(&views, &mut clipped);
        if clipped.len() < 3 {
            continue;
        }
        proj.clear();
        for vv in &clipped {
            proj.push(ProjT {
                x: cx + focal * vv.vx / vv.vz,
                y: cy - focal * vv.vy / vv.vz,
                vz: vv.vz,
                s: vv.s,
                t: vv.t,
            });
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                // Lit SURFACE CACHE (Quake d_surf.c): a lightmapped wall with a
                // colormap and no reaching dynamic light bakes texture*lightmap*
                // colormap into a per-surface block ONCE, then reads one byte per
                // pixel. Turb/sky/dynamically-lit/colormap-less surfaces keep the
                // per-pixel path (raster_triangle_tex).
                stat(|s| { s.faces_drawn += 1; s.world_tris += (proj.len() - 2) as u64; });
                let _t_s = prof.then(StatInstant::now);
                let surf = if matches!(mode, SurfaceMode::Normal) {
                    match (lightmap.as_ref(), colormap) {
                        (Some(lm), Some(cm)) => {
                            let dlit = any_dlight_reaches(bsp, face, dlights, face_dlightbits);
                            // World model: cacheable (stable `Bsp` across frames).
                            face_surf_block(
                                face_index, face, mt, lm, cm, fp, n_faces, light_styles, dlit, true,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(t) = _t_s { t_surf += t.elapsed().as_nanos() as u64; }
                match surf {
                    Some((block, bw, bh, tmins)) => {
                        stat(|s| s.surf_hits += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_cached(
                                image, zbuf, v0, proj[i], proj[i + 1], &block, bw, bh, tmins,
                                palette,
                            );
                        }
                    }
                    None => {
                        stat(|s| s.surf_misses += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                                colormap,
                            );
                        }
                    }
                }
            }
            _ => {
                // Flat hashed colour for textureless faces (e.g. sky/animated).
                // When the face has a lightmap, route through the textured path
                // with a 1x1 single-colour texture so the per-pixel lightmap
                // factor is applied; otherwise keep the original flat shading.
                let key = ti.map(|t| t.miptex as i64).unwrap_or(face.texinfo as i64);
                let base = hash_color(key);
                match lightmap {
                    Some(lm) => {
                        // A 1x1 texture whose single index maps to the hashed
                        // base colour (before any shade); brightness comes from
                        // the lightmap inside the rasteriser.
                        let mut pal1 = [[0u8; 3]; 256];
                        pal1[0] = [
                            (base[0] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let one = [0u8];
                        let v0 = proj[0];
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &one, 1, 1, &pal1, shade, Some(&lm), SurfaceMode::Normal,
                                colormap,
                            );
                        }
                    }
                    None => {
                        let color = [
                            (base[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let p0 = Projected { x: proj[0].x, y: proj[0].y, depth: proj[0].vz };
                        for i in 1..proj.len() - 1 {
                            let p1 = Projected { x: proj[i].x, y: proj[i].y, depth: proj[i].vz };
                            let p2 =
                                Projected { x: proj[i + 1].x, y: proj[i + 1].y, depth: proj[i + 1].vz };
                            raster_triangle(image, zbuf, p0, p1, p2, color);
                        }
                    }
                }
            }
        }
    }

    // Return the marking scratch for the next pass/frame (keeps its capacity).
    DLIGHT_BITS_SCRATCH.with(|b| *b.borrow_mut() = dlight_bits);
    SKY_SPANS_SCRATCH.with(|b| *b.borrow_mut() = sky_spans.into_inner());

    // Flush sub-phase timers. `setup` = whole draw-loop body minus the measured
    // lightmap + surf-block calls (so it captures geom-cache fetch, the culls,
    // texinfo/classify, near-clip + projection, and — at high res — the raster).
    if prof {
        let body = _t_body.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);
        let t_setup = body.saturating_sub(t_light).saturating_sub(t_surf);
        stat(|s| {
            s.world_pvs_ns += t_pvs;
            s.world_sort_ns += t_sort;
            s.world_setup_ns += t_setup;
            s.world_light_ns += t_light;
            s.world_surf_ns += t_surf;
        });
    }
}

// ---------------------------------------------------------------------------
// Brush submodels (doors, platforms, buttons — Quake's inline `*N` bmodels)
// ---------------------------------------------------------------------------

/// Draw one brush submodel (`bsp.models[model_index]`) at world `origin`, sharing
/// `image`/`zbuf` so it occludes — and is occluded by — the world and other
/// geometry through the one depth buffer.
///
/// Submodels (doors/plats/buttons referenced by brush entities whose `model`
/// field is `"*N"`) live in the same vertex/edge/surfedge arrays as the
/// worldmodel; [`face_world_poly`] therefore reconstructs each submodel face in
/// the *same* model-local coordinate space as the world. To place the submodel
/// we add `origin` to every reconstructed vertex.
///
/// This is a faithful variant of [`draw_world_textured`]'s per-face loop with
/// three deliberate differences:
///  * **No PVS cull.** Submodel faces are not referenced by any worldmodel
///    leaf's marksurfaces, so they have no PVS bit; they always draw (matching
///    `compute_visible_faces`, which leaves unowned faces visible).
///  * **Origin shift.** Each vertex is projected from its *origin-shifted* world
///    position, and the backface cull uses the *shifted* face center against
///    `cam.pos`. The plane normal is a direction (origin-independent), so
///    [`face_normal`] is used directly. Quake also backface-culls bmodel polys,
///    so a door not visible from a side is faithful.
///  * **Local ST / lightmap.** Quake computes a bmodel's surface `(s,t)` from the
///    model-space vertex (i.e. *before* the origin shift), and lightmap extents
///    come from the origin-independent texinfo `(s,t)` of the *local* polygon.
///    So `(s,t)` and the lightmap are derived from the LOCAL vertices while only
///    projection uses the shifted ones.
///
/// Every index into BSP-derived data is bounds-checked; a malformed face (or an
/// out-of-range `model_index`) is skipped, never panicked on.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_submodel(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
    model_index: usize,
    origin: Vec3,
    turb: &TurbTable,
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    colormap: Option<&[u8]>,
    ent_frame: i32,
    // Whether this submodel's `bsp` is the stable world `Bsp` (inline submodels —
    // doors/plats/buttons — share it, so their baked surface blocks are worth
    // caching) or an ephemeral per-instance clone (external b_*.bsp item boxes,
    // which re-clone every frame and must bypass the surface cache; see
    // [`face_surf_block`]).
    cache_surf: bool,
) {
    // The near plane lives in `clip_poly_near` (`NEAR_PLANE`); this pass clips the
    // polygon to it rather than dropping any face that touches it.
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 {
        return;
    }
    // Out-of-range submodel index is a silent no-op.
    let m = match bsp.models.get(model_index) {
        Some(m) => m,
        None => return,
    };

    // The submodel's lightmap is built from the LOCAL (pre-shift) polygon, so the
    // dynamic lights — which live in world space — are translated into the
    // submodel's local frame by subtracting `origin` (Quake's `R_DrawBrushModel`
    // does the same `VectorSubtract(dlight.origin, ent->origin, lightorigin)`).
    // An empty input stays empty, so a submodel with no dlights is unchanged.
    let local_dlights: Vec<crate::dlight::DynamicLight> = dlights
        .iter()
        .map(|dl| {
            let mut d = *dl;
            d.origin = [
                dl.origin[0] - origin[0],
                dl.origin[1] - origin[1],
                dl.origin[2] - origin[2],
            ];
            d
        })
        .collect();

    // Same camera basis / focal length / projection as the world pass.
    let (forward, right, up) = cam.basis();
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
    let tan_half = half_fov.tan();
    let focal = if tan_half.abs() < 1e-6 {
        cx
    } else {
        (cx as f64 / tan_half) as f32
    };
    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);

    // Submodel face range: [firstface, firstface + numfaces). Negative counts
    // clamp to 0 so the range is empty rather than wrapping.
    let f0 = m.firstface.max(0) as usize;
    let count = m.numfaces.max(0) as usize;
    let end = match f0.checked_add(count) {
        Some(e) => e,
        None => return,
    };

    // World fingerprint + face count for the lit-surface cache. A submodel's (s,t)
    // and lightmap come from its LOCAL (origin-independent) polygon, so the baked
    // texture*lightmap*colormap block is identical wherever the door/plat sits —
    // cacheable by GLOBAL face index just like the world pass (the world's and the
    // submodels' face ranges are disjoint, so there is no key collision).
    let fp = WorldFingerprint::of(bsp);
    let n_faces = bsp.faces.len();

    // `R_MarkLights` over the submodel's OWN subtree, as the C marks bmodels from
    // `clmodel->nodes + clmodel->hulls[0].firstclipnode` (`R_DrawBEntitiesOnList`,
    // `r_main.c`) — the submodel's `headnode[0]`. The subtree's node face lists
    // only reference this submodel's faces, so the mask is per-face like the world
    // pass. Marked with the entity-LOCAL light origins, consistent with the local
    // per-luxel lighting above (the subtree planes are in model-local space).
    let mut dlight_bits = DLIGHT_BITS_SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let sub_headnode = m.headnode.first().copied().unwrap_or(0);
    mark_dlights(bsp, sub_headnode, &local_dlights, &mut dlight_bits);

    // `world_poly` holds origin-SHIFTED vertices (for projection); we keep the
    // LOCAL vertices separately for (s,t) and the lightmap.
    let mut local_poly: Vec<Vec3> = Vec::new();
    let mut world_poly: Vec<Vec3> = Vec::new();
    let mut views: Vec<VView> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();
    // Reused near-clip output buffer (cleared per face); zero per-face alloc.
    let mut clipped: Vec<VView> = Vec::new();

    for face_index in f0..end {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        // Every submodel face VISITED this frame (before backface/near cull). The
        // gap between this and `sub_faces_drawn` is the per-frame setup cost
        // (face_world_poly reconstruction etc.) paid on faces that never draw.
        stat(|s| s.sub_faces_visited += 1);

        // Reconstruct the LOCAL polygon (same space as the worldmodel).
        if !face_world_poly(bsp, face, &mut local_poly) {
            continue;
        }
        // Origin-shifted copy used for the cull center and projection.
        world_poly.clear();
        for v in &local_poly {
            world_poly.push([v[0] + origin[0], v[1] + origin[1], v[2] + origin[2]]);
        }

        let normal = match face_normal(bsp, face) {
            Some(n) => n,
            None => continue,
        };

        // Backface cull against the SHIFTED face center.
        let mut center = [0.0f32; 3];
        for v in &world_poly {
            for k in 0..3 {
                center[k] += v[k];
            }
        }
        let inv_n = 1.0 / world_poly.len() as f32;
        for c in &mut center {
            *c *= inv_n;
        }
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture. Animated (`+`-prefixed) textures
        // on a brush submodel cycle by time exactly as the world does
        // (`R_TextureAnimation`); `ent_frame` selects the `+a..+j` alternate
        // (switch) cycle when the brush entity is activated (e.g. a pressed button
        // turning green), else the primary `+0..+9` cycle.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            let anim_mi = texture_animation(bsp, mi, ent_frame, time);
            bsp.textures.get(anim_mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name. Liquids
        // and sky are fullbright and NOT lightmapped; only walls compute a
        // lightmap (from the LOCAL polygon — texinfo extents are origin-independent).
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        // This face's `R_MarkLights` mask (0 = no dynamic light reaches it).
        let face_dlightbits = dlight_bits.get(face_index).copied().unwrap_or(0);
        let lightmap = if kind == SurfKind::Normal {
            stat(|s| s.sub_lm_builds += 1);
            face_lightmap_dyn(bsp, face, &local_poly, light_styles, &local_dlights, face_dlightbits)
        } else {
            None
        };
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky { view: SkyView::new(forward, right, up, w, h, time), defer: None },
        };

        // Build the view-space polygon from the SHIFTED vertices (for vx/vy/vz)
        // but with (s,t) from the LOCAL (pre-shift) vertex, then near-clip it.
        // A length mismatch between the shifted and local polygons is treated as
        // a malformed face and skips it (matching the old defensive break).
        views.clear();
        let mut bad = false;
        for (vi, vw) in world_poly.iter().enumerate() {
            let rel = sub(*vw, cam.pos);
            let vz = dot(rel, forward);
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            // (s,t) from the local (pre-shift) vertex coordinate.
            let vl = match local_poly.get(vi) {
                Some(v) => *v,
                None => {
                    bad = true;
                    break;
                }
            };
            let (s, t) = match ti {
                Some(ti) => (
                    vl[0] * ti.vecs[0][0] + vl[1] * ti.vecs[0][1] + vl[2] * ti.vecs[0][2] + ti.vecs[0][3],
                    vl[0] * ti.vecs[1][0] + vl[1] * ti.vecs[1][1] + vl[2] * ti.vecs[1][2] + ti.vecs[1][3],
                ),
                None => (0.0, 0.0),
            };
            views.push(VView { vx, vy, vz, s, t });
        }
        if bad {
            continue;
        }
        clip_poly_near_into(&views, &mut clipped);
        if clipped.len() < 3 {
            continue;
        }
        proj.clear();
        for vv in &clipped {
            proj.push(ProjT {
                x: cx + focal * vv.vx / vv.vz,
                y: cy - focal * vv.vy / vv.vz,
                vz: vv.vz,
                s: vv.s,
                t: vv.t,
            });
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                stat(|s| { s.sub_faces_drawn += 1; s.sub_tris += (proj.len() - 2) as u64; });
                // Lit SURFACE CACHE for submodels (doors/plats/buttons) — same as the
                // world pass. Gated on `ent_frame == 0`: an ACTIVATED brush entity
                // (frame != 0) samples the alternate (+a..+j) texture cycle — a
                // different miptex than the baked block — so it falls back to the
                // per-pixel path. Turb/sky/dlit/colormap-less faces also fall back.
                let surf = if ent_frame == 0 && matches!(mode, SurfaceMode::Normal) {
                    match (lightmap.as_ref(), colormap) {
                        (Some(lm), Some(cm)) => {
                            let dlit = any_dlight_reaches(bsp, face, &local_dlights, face_dlightbits);
                            face_surf_block(
                                face_index, face, mt, lm, cm, fp, n_faces, light_styles, dlit,
                                cache_surf,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                match surf {
                    Some((block, bw, bh, tmins)) => {
                        stat(|s| s.sub_surf_hits += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_cached(
                                image, zbuf, v0, proj[i], proj[i + 1], &block, bw, bh, tmins,
                                palette,
                            );
                        }
                    }
                    None => {
                        stat(|s| s.sub_surf_misses += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                                colormap,
                            );
                        }
                    }
                }
            }
            _ => {
                // Flat hashed colour for textureless faces; route through the
                // textured path with a 1x1 colour when a lightmap is present so
                // the per-pixel lightmap factor still applies (matching the world
                // pass exactly).
                let key = ti.map(|t| t.miptex as i64).unwrap_or(face.texinfo as i64);
                let base = hash_color(key);
                match lightmap {
                    Some(lm) => {
                        let mut pal1 = [[0u8; 3]; 256];
                        pal1[0] = [
                            (base[0] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let one = [0u8];
                        let v0 = proj[0];
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &one, 1, 1, &pal1, shade, Some(&lm), SurfaceMode::Normal,
                                colormap,
                            );
                        }
                    }
                    None => {
                        let color = [
                            (base[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let p0 = Projected { x: proj[0].x, y: proj[0].y, depth: proj[0].vz };
                        for i in 1..proj.len() - 1 {
                            let p1 = Projected { x: proj[i].x, y: proj[i].y, depth: proj[i].vz };
                            let p2 =
                                Projected { x: proj[i + 1].x, y: proj[i + 1].y, depth: proj[i + 1].vz };
                            raster_triangle(image, zbuf, p0, p1, p2, color);
                        }
                    }
                }
            }
        }
    }

    // Return the marking scratch for the next pass/frame (keeps its capacity).
    DLIGHT_BITS_SCRATCH.with(|b| *b.borrow_mut() = dlight_bits);
}

/// One brush submodel placed in the world: which inline model
/// (`bsp.models[model_index]`) to draw and where (`origin`).
///
/// Brush entities (doors, platforms, buttons, triggers with visible brushes)
/// reference an inline submodel through their `model` field `"*N"`, where `N`
/// indexes `bsp.models`. Submodel 0 is the worldspawn (drawn by
/// [`draw_world_textured`]); `N >= 1` are the brush entities, drawn by
/// [`draw_submodel`] at this `origin`. See [`render_scene_ext`](super::render_scene_ext).
pub struct BModelInstance {
    pub model_index: usize,
    pub origin: Vec3,
    /// The brush entity's `frame` field. func_button / func_door / func_plat set
    /// `self.frame = 1` when activated; a non-zero frame selects the `+a..+j`
    /// ALTERNATE animated-texture cycle (`R_TextureAnimation`), so e.g. a pressed
    /// button shows its lit/green face. 0 = the primary `+0..+9` cycle.
    pub frame: i32,
}

/// One *external* brush model placed in the world: an entire standalone BSP
/// (e.g. `maps/b_explob.bsp`, `maps/b_shell0.bsp`) drawn as a single item,
/// placed at `origin`.
///
/// In Quake many pickup/item entities (`misc_explobox`, `item_health`,
/// `item_shells`, `item_rockets`, `item_spikes`, `item_cells`, the armor) are
/// not alias models — their QuakeC `precache_model`/`setmodel` points at a tiny
/// brush BSP that ships in the pak (`maps/b_*.bsp`). Each such BSP is a complete
/// `version 29` map whose **MODEL 0** is the little box brush, defined around the
/// bsp's own local origin. The item entity then stands that box at its world
/// `origin`.
///
/// This differs from [`BModelInstance`], which references an *inline* submodel of
/// the **world** bsp by index. An `ExternalBModel` borrows a *separate*, already
/// parsed [`Bsp`] (so one cached parse can back many instances without cloning)
/// and always renders that bsp's model-0 faces. It is drawn by [`draw_brush_bsp`]
/// / [`render_scene_ext`](super::render_scene_ext), sharing the world z-buffer so the box occludes — and
/// is occluded by — the world and every other model correctly.
pub struct ExternalBModel<'a> {
    /// The parsed standalone brush BSP (its MODEL 0 is the visible box).
    pub bsp: &'a Bsp,
    /// World position to stand the box at (the item entity's `origin`).
    pub origin: Vec3,
}

/// Draw an **external** brush model's MODEL-0 faces into `image`, translated to
/// `origin` and z-tested against the shared `zbuf` — the render path for Quake's
/// `b_*.bsp` item boxes (explosive box, ammo/health boxes; see [`ExternalBModel`]).
///
/// This is the standalone-bsp counterpart to the world-submodel path. It reuses
/// the exact same brush-face machinery as [`draw_submodel`] (texinfo (s,t) build,
/// near-clip via [`clip_poly_near`], perspective projection, fan rasterise with
/// the bsp's **own** miptextures, the static/multi-style lightmap via
/// [`face_lightmap_dyn`], and the per-pixel z-test) — only the source bsp differs,
/// so there is no duplicated rasteriser. Because these little boxes carry their
/// own textures and a baked lightmap (and most are effectively fullbright), a
/// face with a lightmap uses it and a face without one renders fullbright, exactly
/// as [`draw_submodel`] already handles.
///
/// MODEL index 0 (the whole box brush) is always the one drawn; the function is a
/// thin wrapper over the shared [`draw_submodel`] implementation with
/// `model_index = 0`.
///
/// SAFETY: every bsp-array access in the shared path goes through `.get()`, so a
/// malformed or empty external bsp (no models, an out-of-range face/edge/plane/
/// texinfo) simply draws nothing rather than panicking.
#[allow(clippy::too_many_arguments)]
pub fn draw_brush_bsp(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    bsp: &Bsp,
    origin: Vec3,
    palette: &[[u8; 3]; 256],
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
) {
    // The little box bsps are not dynamically lit in the original game; pass no
    // dynamic lights (so the static/multi-style lightmap, or fullbright, is used).
    // A fresh turbulent SIN table is built per call — these boxes have no liquid
    // surfaces in practice, but the shared path needs a table to satisfy the type;
    // it is cheap (a 256-entry `[f32]`, no global state). Callers that draw many
    // boxes per frame go through `render_scene_ext`, which builds ONE table and
    // calls the shared `draw_submodel` directly, so this per-call table only costs
    // when `draw_brush_bsp` is used standalone (e.g. tests).
    let turb = TurbTable::new();
    draw_submodel(
        image,
        zbuf,
        bsp,
        cam,
        palette,
        0,
        origin,
        &turb,
        time,
        light_styles,
        &[],
        // Standalone box draw keeps the legacy linear shade (no colormap),
        // byte-identical to before; the colormap is a render_scene_ext concern.
        None,
        0,     // ent_frame: standalone box has no activated/alternate state
        false, // external box bsp -> bypass the surface cache (no colormap anyway)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{demo_room, render_scene, render_scene_ext};
    use crate::render::alias::ModelInstance;
    use crate::render::fixtures::tiny_mdl;
    use crate::render::light::NEUTRAL_LIGHTSTYLE_SCALES;

    // -- Brush submodels (inline `*N` bmodels: doors / plats / buttons) -----

    /// Extend [`demo_room`] with a second inline model (model index 1): a single
    /// 64x64 quad whose face plane faces `-X`. The quad's vertices live in the
    /// shared vertex array in MODEL-LOCAL space (a YZ square at local `x = 0`),
    /// exactly as Quake stores bmodel geometry, so a caller places it by adding
    /// an `origin`.
    ///
    /// `model 0` is left covering only the original world faces (its `numfaces`
    /// is unchanged); the new quad is owned solely by `model 1`.
    fn demo_room_with_submodel() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DPlane};

        let mut bsp = demo_room();

        // Record where the world's faces end; the submodel face starts here.
        let submodel_firstface = bsp.faces.len() as i32;

        // Four corners of a 64x64 YZ quad at local x = 0, ordered CCW as seen
        // from -X. Pushed as fresh vertexes in the SHARED vertex array.
        let base_vtx = bsp.vertexes.len() as u16;
        let corners: [[f32; 3]; 4] = [
            [0.0, -32.0, -32.0],
            [0.0, 32.0, -32.0],
            [0.0, 32.0, 32.0],
            [0.0, -32.0, 32.0],
        ];
        for c in corners {
            bsp.vertexes.push(crate::bsp::DVertex { point: c });
        }

        // Four edges around the quad, referenced by four positive surfedges.
        let first_edge = bsp.surfedges.len() as i32;
        for k in 0..4u16 {
            let a = base_vtx + k;
            let b = base_vtx + ((k + 1) % 4);
            let edge_index = bsp.edges.len() as i32;
            bsp.edges.push(DEdge { v: [a, b] });
            bsp.surfedges.push(edge_index);
        }

        // Plane: outward normal -X (faces toward a camera on the -X side).
        let planenum = bsp.planes.len() as i16;
        bsp.planes.push(DPlane {
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            ptype: crate::bsp::PLANE_X,
        });

        // Reuse texinfo 0 (axis-aligned; demo_room has no inline textures so the
        // submodel takes the flat-colour fallback, same as the world walls).
        bsp.faces.push(DFace {
            planenum,
            side: 0,
            firstedge: first_edge,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        });

        // Model 1: just the new quad face.
        bsp.models.push(DModel {
            mins: [-1.0, -32.0, -32.0],
            maxs: [1.0, 32.0, 32.0],
            origin: [0.0, 0.0, 0.0],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: submodel_firstface,
            numfaces: 1,
        });

        bsp
    }

    #[test]
    fn render_scene_ext_draws_submodel() {
        // A submodel placed in front of the camera must add non-background
        // pixels relative to an empty bmodel list (the submodel becomes visible).
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        // Look down +X from near the west wall.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let bg = [10u8, 10, 14];

        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            // Place the quad between the camera (-200) and the centre, facing it.
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        let drawn_without = without.rgb.iter().filter(|&&p| p != bg).count();
        let drawn_with = with.rgb.iter().filter(|&&p| p != bg).count();
        // The submodel quad sits at world x = -120, nearer than the world walls
        // behind it, so it must paint at least as many non-background pixels as
        // without it (it can only add coverage, never remove it). With near-plane
        // clipping the surrounding world walls now also fill the frame, so this is
        // an `>=` rather than a strict `>` — the strong check below is occlusion.
        assert!(
            drawn_with >= drawn_without,
            "submodel must not reduce coverage: {drawn_without} -> {drawn_with}"
        );

        // The decisive check: the submodel is nearer than the geometry behind it,
        // so adding it must CHANGE the framebuffer (it occludes the far wall). This
        // proves the submodel is rasterised and depth-tested, independent of how
        // much background the world fills.
        let changed = without
            .rgb
            .iter()
            .zip(with.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "submodel changed no pixels (not drawn / fully occluded)");
    }

    #[test]
    fn render_scene_ext_out_of_range_submodel_is_noop() {
        // An out-of-range model_index must draw nothing and not panic: the image
        // is byte-identical to passing an empty bmodel list.
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let oob = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 999, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        assert_eq!(
            empty.rgb, oob.rgb,
            "out-of-range submodel index must be a no-op"
        );
    }

    #[test]
    fn render_scene_matches_ext_empty_on_demo_room() {
        // render_scene must equal render_scene_ext(.., &[]) on demo_room: the
        // wrapper preserves the existing world+alias behaviour exactly.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);

        // No alias models, no bmodels.
        let a = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(a.rgb, b.rgb, "render_scene must equal render_scene_ext(.., &[])");

        // Also holds with an alias instance present (the model path is shared).
        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let a2 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));
        let b2 = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            std::slice::from_ref(&inst),
            &[],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        assert_eq!(
            a2.rgb, b2.rgb,
            "render_scene must equal render_scene_ext with the same alias models and no bmodels"
        );
    }

    #[test]
    fn submodel_origin_shifts_geometry() {
        // The same submodel at two different origins must land in different
        // places: rendering it at one origin vs another changes pixels. This
        // proves the origin shift actually moves the geometry.
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        // Shift the quad well off to one side (+Y) so it projects elsewhere.
        let shifted = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 120.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let changed = centered
            .rgb
            .iter()
            .zip(shifted.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "moving the submodel origin should move its pixels");
    }

    #[test]
    fn submodel_tolerates_malformed_faces() {
        // A submodel whose faces reference out-of-range edges/planes/texinfo must
        // be skipped without panicking (mirrors render_tolerates_malformed_faces
        // for the world path).
        let mut bsp = demo_room_with_submodel();
        // Corrupt the submodel's single face (the last face in the array).
        if let Some(f) = bsp.faces.last_mut() {
            f.firstedge = 1_000_000; // past surfedges
            f.planenum = 30_000; // past planes
            f.texinfo = 30_000; // past texinfo
        }
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        // Must not panic; the corrupt face is simply skipped.
        let _img = render_scene_ext(
            &bsp,
            &cam,
            80,
            60,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    // -- External brush models (standalone b_*.bsp item boxes) -----------------

    /// A standalone tiny brush BSP (version 29) whose **MODEL 0** is a single
    /// 64x64 YZ quad at local x = 0, facing -X (outward normal -X) — the smallest
    /// stand-in for Quake's `maps/b_*.bsp` item boxes. Built around the bsp's own
    /// local origin so [`ExternalBModel`] / [`draw_brush_bsp`] place it at a world
    /// origin via the same origin shift the submodel path uses. No inline textures
    /// (so it takes the flat-colour fallback) and no lighting lump (fullbright).
    fn tiny_brush_bsp() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DPlane, DVertex, TexInfo, PLANE_X};

        // Four corners of a 64x64 YZ quad at local x = 0, ordered CCW as seen from
        // -X (so with the -X plane normal the face is visible from a -X camera).
        let vertexes = vec![
            DVertex { point: [0.0, -32.0, -32.0] },
            DVertex { point: [0.0, 32.0, -32.0] },
            DVertex { point: [0.0, 32.0, 32.0] },
            DVertex { point: [0.0, -32.0, 32.0] },
        ];
        // Edge 0 is conventionally unused; reserve a dummy then four real edges.
        let mut edges = vec![DEdge { v: [0, 0] }];
        let mut surfedges: Vec<i32> = Vec::new();
        for k in 0..4u16 {
            let a = k;
            let b = (k + 1) % 4;
            let edge_index = edges.len() as i32;
            edges.push(DEdge { v: [a, b] });
            surfedges.push(edge_index);
        }
        let planes = vec![DPlane { normal: [-1.0, 0.0, 0.0], dist: 0.0, ptype: PLANE_X }];
        let texinfo = vec![TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }];
        let faces = vec![DFace {
            planenum: 0,
            side: 0,
            firstedge: 0,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        }];
        // MODEL 0 covers the single face — the whole little box brush.
        let models = vec![DModel {
            mins: [-1.0, -32.0, -32.0],
            maxs: [1.0, 32.0, 32.0],
            origin: [0.0, 0.0, 0.0],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: 0,
            numfaces: 1,
        }];

        Bsp {
            version: 29,
            entities: String::new(),
            planes,
            vertexes,
            edges,
            faces,
            nodes: Vec::new(),
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo,
            models,
            marksurfaces: Vec::new(),
            surfedges,
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: Vec::new(),
        }
    }

    #[test]
    fn render_scene_ext_draws_external_brush_bsp() {
        // An external brush model placed in front of the camera must add /change
        // pixels relative to an empty `external` list — proving its MODEL-0 faces
        // are rasterised at the entity origin and depth-tested against the world.
        let world = demo_room();
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        // Look down +X from near the west wall; stand the box at world x = -120,
        // nearer than the +256 far wall, so it occludes geometry behind it.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let without = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        let with = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // The box is nearer than the far wall, so drawing it must CHANGE pixels.
        let changed = without
            .rgb
            .iter()
            .zip(with.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "external brush bsp changed no pixels (not drawn)");
    }

    #[test]
    fn external_brush_bsp_origin_shifts_geometry() {
        // The same external box at two different origins must land in different
        // places: rendering it centred vs shifted +Y changes pixels.
        let world = demo_room();
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let shifted = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 120.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let changed = centered
            .rgb
            .iter()
            .zip(shifted.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "moving the external box origin should move its pixels");
    }

    #[test]
    fn empty_external_slice_matches_no_external() {
        // Passing an empty `external` slice must be byte-identical to the prior
        // behaviour (so render_scene and every legacy caller are unchanged).
        let world = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let a = render_scene(&world, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        assert_eq!(a.rgb, b.rgb, "empty external slice must equal render_scene");
    }

    #[test]
    fn external_brush_bsp_empty_or_malformed_is_safe() {
        // A missing box bsp (empty: no models/faces) and one with a corrupt face
        // must both draw nothing and never panic. The empty-bsp render must equal
        // the no-external render; the corrupt-face render must not panic.
        let world = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        // An empty bsp (no models, no faces) — like an unparseable box.
        let empty = Bsp {
            version: 29,
            entities: String::new(),
            planes: Vec::new(),
            vertexes: Vec::new(),
            edges: Vec::new(),
            faces: Vec::new(),
            nodes: Vec::new(),
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo: Vec::new(),
            models: Vec::new(),
            marksurfaces: Vec::new(),
            surfedges: Vec::new(),
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: Vec::new(),
        };
        let baseline = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        let with_empty = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &empty, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        assert_eq!(
            baseline.rgb, with_empty.rgb,
            "an empty/missing external box must draw nothing"
        );

        // A box whose single face references out-of-range edges/planes/texinfo:
        // the face is skipped, no panic.
        let mut bad = tiny_brush_bsp();
        if let Some(f) = bad.faces.last_mut() {
            f.firstedge = 1_000_000;
            f.planenum = 30_000;
            f.texinfo = 30_000;
        }
        let _ = render_scene_ext(
            &world,
            &cam,
            80,
            60,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &bad, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    #[test]
    fn draw_brush_bsp_paints_at_projected_location() {
        // The standalone `draw_brush_bsp` entry must paint into a caller-owned
        // image + z-buffer at the box's projected location, and a missing/empty
        // bsp must leave both untouched (no panic).
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (160usize, 120usize);
        let bg = [10u8, 10, 14];

        let mut image = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        draw_brush_bsp(
            &mut image,
            &mut zbuf,
            &cam,
            &box_bsp,
            [-120.0, 0.0, 0.0],
            &pal,
            0.0,
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        let painted = image.rgb.iter().filter(|&&p| p != bg).count();
        assert!(painted > 0, "draw_brush_bsp painted nothing at the box location");
        // Some z-buffer cells must now be finite (depth was written).
        assert!(zbuf.iter().any(|z| z.is_finite()), "draw_brush_bsp wrote no depth");

        // A missing/empty bsp leaves a fresh image untouched and does not panic.
        let empty = {
            let mut e = tiny_brush_bsp();
            e.models.clear();
            e
        };
        let mut image2 = Image::new(w, h, bg);
        let mut zbuf2 = vec![f32::INFINITY; w * h];
        draw_brush_bsp(
            &mut image2,
            &mut zbuf2,
            &cam,
            &empty,
            [-120.0, 0.0, 0.0],
            &pal,
            0.0,
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        assert!(
            image2.rgb.iter().all(|&p| p == bg),
            "an empty external box must leave the image untouched"
        );
    }
}
