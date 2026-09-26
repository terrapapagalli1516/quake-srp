//! The brush entities handed to the renderer, and a face's gradients.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! The brush half of `R_DrawBEntitiesOnList` (`r_main.c`) is in [`super::edge`],
//! which draws the world and these entities through one edge list; here are the
//! entities it takes ([`BModelInstance`], [`ExternalBModel`]) and
//! `D_CalcGradients`' planes for a face ([`face_grads`]).

use crate::bsp::Bsp;
use crate::math::Vec3;
use super::raster::{PolyGrads, ScreenProj};

/// A face's [`PolyGrads`] from its plane (`bsp.planes[face.planenum]`, not
/// side-flipped: flipping the normal and the distance together changes
/// nothing) and texinfo, seen from `eye` in the face's model space. `None` for
/// a missing plane or an eye on the plane (the face is edge-on).
pub(super) fn face_grads(
    bsp: &Bsp,
    face: &crate::bsp::DFace,
    view: &ScreenProj,
    eye: Vec3,
    ti: Option<&crate::bsp::TexInfo>,
) -> Option<PolyGrads> {
    let plane = usize::try_from(face.planenum).ok().and_then(|pi| bsp.planes.get(pi))?;
    PolyGrads::for_plane(view, eye, plane.normal, plane.dist, ti)
}

/// One brush submodel placed in the world: which inline model
/// (`bsp.models[model_index]`) to draw and where (`origin`).
///
/// Brush entities (doors, platforms, buttons, triggers with visible brushes)
/// reference an inline submodel through their `model` field `"*N"`, where `N`
/// indexes `bsp.models`. Submodel 0 is the worldspawn; `N >= 1` are the brush
/// entities, put into the world's edge list at this `origin`
/// (`R_DrawBEntitiesOnList`, `edge.rs`). See [`Scene::bmodels`](super::Scene::bmodels).
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
/// and always renders that bsp's model-0 faces. The renderer
/// ([`Scene::external`](super::Scene::external)) puts them in the world's
/// edge list like any brush entity, so the box sorts
/// with the world and the other brush models as id's instanced brush models do.
pub struct ExternalBModel<'a> {
    /// The parsed standalone brush BSP (its MODEL 0 is the visible box).
    pub bsp: &'a Bsp,
    /// World position to stand the box at (the item entity's `origin`).
    pub origin: Vec3,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{demo_room, Camera, Scene};
    use crate::render::fixtures::render_once;

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
    fn a_submodel_draws_into_the_scene() {
        // A submodel placed in front of the camera must add non-background
        // pixels relative to an empty bmodel list (the submodel becomes visible).
        let bsp = demo_room_with_submodel();
        let pal = crate::render::fixtures::ramp_palette();
        // Look down +X from near the west wall.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let bg = 2u8; // r_clearcolor, the background

        let without = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        // Place the quad between the camera (-200) and the centre, facing it.
        let bmodels = [BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }];
        let with = render_once(&Scene { bmodels: &bmodels, ..Scene::new(&bsp, cam, 160, 120, &pal) });

        let drawn_without = without.pixels.iter().filter(|&&p| p != bg).count();
        let drawn_with = with.pixels.iter().filter(|&&p| p != bg).count();
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
            .pixels
            .iter()
            .zip(with.pixels.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "submodel changed no pixels (not drawn / fully occluded)");
    }

    #[test]
    fn an_out_of_range_submodel_is_a_noop() {
        // An out-of-range model_index must draw nothing and not panic: the image
        // is byte-identical to passing an empty bmodel list.
        let bsp = demo_room_with_submodel();
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let empty = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        let oob = render_once(&Scene { bmodels: &[BModelInstance { model_index: 999, origin: [-120.0, 0.0, 0.0], frame: 0 }], ..Scene::new(&bsp, cam, 160, 120, &pal) });
        assert_eq!(
            empty.pixels, oob.pixels,
            "out-of-range submodel index must be a no-op"
        );
    }

    #[test]
    fn submodel_origin_shifts_geometry() {
        // The same submodel at two different origins must land in different
        // places: rendering it at one origin vs another changes pixels. This
        // proves the origin shift actually moves the geometry.
        let bsp = demo_room_with_submodel();
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_once(&Scene { bmodels: &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }], ..Scene::new(&bsp, cam, 160, 120, &pal) });
        // Shift the quad well off to one side (+Y) so it projects elsewhere.
        let shifted = render_once(&Scene { bmodels: &[BModelInstance { model_index: 1, origin: [-120.0, 120.0, 0.0], frame: 0 }], ..Scene::new(&bsp, cam, 160, 120, &pal) });
        let changed = centered
            .pixels
            .iter()
            .zip(shifted.pixels.iter())
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
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        // Must not panic; the corrupt face is simply skipped.
        let _img = render_once(&Scene { bmodels: &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }], ..Scene::new(&bsp, cam, 80, 60, &pal) });
    }

    // -- External brush models (standalone b_*.bsp item boxes) -----------------

    /// A standalone tiny brush BSP (version 29) whose **MODEL 0** is a single
    /// 64x64 YZ quad at local x = 0, facing -X (outward normal -X) — the smallest
    /// stand-in for Quake's `maps/b_*.bsp` item boxes. Built around the bsp's own
    /// local origin so [`ExternalBModel`] places it at a world
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
    fn an_external_brush_bsp_draws_into_the_scene() {
        // An external brush model placed in front of the camera must add /change
        // pixels relative to an empty `external` list — proving its MODEL-0 faces
        // are rasterised at the entity origin and depth-tested against the world.
        let world = demo_room();
        let box_bsp = tiny_brush_bsp();
        let pal = crate::render::fixtures::ramp_palette();
        // Look down +X from near the west wall; stand the box at world x = -120,
        // nearer than the +256 far wall, so it occludes geometry behind it.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let without = render_once(&Scene::new(&world, cam, 160, 120, &pal));
        let with = render_once(&Scene { external: &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }], ..Scene::new(&world, cam, 160, 120, &pal) });

        // The box is nearer than the far wall, so drawing it must CHANGE pixels.
        let changed = without
            .pixels
            .iter()
            .zip(with.pixels.iter())
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
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_once(&Scene { external: &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }], ..Scene::new(&world, cam, 160, 120, &pal) });
        let shifted = render_once(&Scene { external: &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 120.0, 0.0] }], ..Scene::new(&world, cam, 160, 120, &pal) });
        let changed = centered
            .pixels
            .iter()
            .zip(shifted.pixels.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "moving the external box origin should move its pixels");
    }

    #[test]
    fn external_brush_bsp_empty_or_malformed_is_safe() {
        // A missing box bsp (empty: no models/faces) and one with a corrupt face
        // must both draw nothing and never panic. The empty-bsp render must equal
        // the no-external render; the corrupt-face render must not panic.
        let world = demo_room();
        let pal = crate::render::fixtures::ramp_palette();
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
        let baseline = render_once(&Scene::new(&world, cam, 160, 120, &pal));
        let with_empty = render_once(&Scene { external: &[ExternalBModel { bsp: &empty, origin: [-120.0, 0.0, 0.0] }], ..Scene::new(&world, cam, 160, 120, &pal) });
        assert_eq!(
            baseline.pixels, with_empty.pixels,
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
        let _ = render_once(&Scene { external: &[ExternalBModel { bsp: &bad, origin: [-120.0, 0.0, 0.0] }], ..Scene::new(&world, cam, 80, 60, &pal) });
    }
}
