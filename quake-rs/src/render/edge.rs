//! The world and brush models as id draws them: edges, sorted spans, no z test.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_bsp.c` (`R_RecursiveWorldNode`, `R_DrawSubmodelPolygons`,
//! `R_DrawSolidClippedSubmodelPolygons`, `R_RecursiveClipBPoly`), `r_draw.c`
//! (`R_EmitEdge`, `R_ClipEdge`, `R_EmitCachedEdge`, `R_RenderFace`,
//! `R_RenderBmodelFace`), `r_edge.c` (`R_BeginEdgeFrame`, `R_InsertNewEdges`,
//! `R_RemoveEdges`, `R_StepActiveU`, `R_CleanupSpan`, `R_LeadingEdge`,
//! `R_TrailingEdge`, `R_GenerateSpans`, `R_ScanEdges`), `r_main.c`
//! (`R_ViewChanged`'s clip planes, `R_MarkLeaves`, `R_BmodelCheckBBox`,
//! `R_DrawBEntitiesOnList`, `R_EdgeDrawing`), `r_misc.c` (`R_TransformFrustum`,
//! `R_SetUpFrustumIndexes`), `r_efrag.c` (`R_SplitEntityOnNode2`), `d_edge.c`
//! (`D_DrawSurfaces`) and `d_scan.c` (`D_DrawZSpans`).
//!
//! The frame: the world's BSP is walked front to back (`R_RecursiveWorldNode`);
//! every visible face is clipped to the four sides of the view and its edges
//! are projected into a per-scanline list, each edge carrying the surface on
//! its left or right and the surface a key that orders it front to back (the
//! walk's order). Brush entities join the same list: a door or an item box
//! that spans several world leaves is cut into fragments by the world's planes,
//! each keyed like the leaf it is in; one in a single leaf keeps whole faces
//! and sorts against the other brush models there on 1/z. `R_ScanEdges` then
//! walks the scanlines with an active edge table and a stack of the surfaces
//! under the current pixel, and emits a span wherever the top surface changes.
//! `D_DrawSurfaces` draws each surface's spans once — so each pixel of the view
//! is written exactly once, with no z test — and writes the 16-bit `1/z` of
//! every span into the z-buffer (`D_DrawZSpans`), which only the entities read.
//!
//! id's fixed pools — `MAXEDGES`, `MAXSURFS` (`r_maxedges`/`r_maxsurfs`, 2400
//! and 800 by default) and `MAXSPANS` (3000, flushed through `D_DrawSurfaces`
//! when full) — are growable buffers here. Running out of spans only changes
//! when id draws, never what; running out of edges or surfaces drops faces
//! ("Short %d surfaces"), which id's shareware maps do not do at the view
//! sizes it supports (the counters in [`super::RenderStats`] show the peak).

use super::band::Band;
use super::light::{LightMap, any_dlight_reaches, face_lightmap_with, mark_dlights, mark_dlights_more};
use super::raster::PolyGrads;
use super::raster::{BlockFixed, ScreenProj, hash_index, shade_index, span_at, span_cached, span_tex, span_turb};
use super::sky::{SkyView, draw_sky_span, sky_dome_scale, sky_texture};
use super::stats::Profiler;
use super::surf::{
    BakeJob, Bakes, MipView, SurfBlock, SurfKind, Surface, SurfaceCaches, SurfaceRequest, classify_surface,
    face_world_poly, texture_animation,
};
use super::torch::FaceTorches;
use super::vis::point_in_leaf;
use super::world::{self, face_grads};
use super::{Frame, Projection, ViewGeom};
use crate::bsp::MipTex;
use crate::bsp::{Bsp, CONTENTS_SOLID, DFace, TexInfo};
use crate::math::{Vec3, dot, normalize, sub};

/// "No edge / no span / no surface" in the index links.
const NONE: u32 = u32::MAX;

/// The four sentinel edges, at the head of the edge buffer (`edge_head`,
/// `edge_tail`, `edge_aftertail`, `edge_sentinel`); the frame's edges follow
/// from [`FIRST_EDGE`] (`r_edges`).
const EDGE_HEAD: u32 = 0;
const EDGE_TAIL: u32 = 1;
const EDGE_AFTERTAIL: u32 = 2;
const EDGE_SENTINEL: u32 = 3;
const FIRST_EDGE: u32 = 4;

/// `surfaces[1]`, the background: behind everything, drawn in `r_clearcolor`.
const BACKGROUND: u32 = 1;

/// `msurface_t` flags (`model.h`).
const SURF_PLANEBACK: u8 = 2;
const SURF_DRAWSKY: u8 = 4;
const SURF_DRAWTURB: u8 = 0x10;
const SURF_DRAWBACKGROUND: u8 = 0x40;

/// `BACKFACE_EPSILON` (`r_local.h`).
const BACKFACE_EPSILON: f64 = 0.01;
/// `NEAR_CLIP` (`r_local.h`): `R_EmitEdge` clamps a vertex's depth to it.
const NEAR_CLIP: f32 = 0.01;
/// `r_draw.c`'s edge-cache codes in `medge_t.cachededgeoffset`.
const FULLY_CLIPPED_CACHED: u32 = 0x8000_0000;
const FRAMECOUNT_MASK: u32 = 0x7FFF_FFFF;
const NOT_CACHEABLE: u32 = 0x7FFF_FFFF;
/// `R_BmodelCheckBBox`'s "not in view".
const BMODEL_FULLY_CLIPPED: u32 = 0x10;
/// `r_clearcolor` (default 2): the background surface's palette index.
const R_CLEARCOLOR: u8 = 2;
/// `D_DrawSurfaces`' background gradient: "effectively at infinity".
const BACKGROUND_ZI: f32 = -0.9;
/// A BSP deeper than this is malformed (id's maps are a few dozen deep).
const MAX_DEPTH: u32 = 1024;

/// `edge_t` (`r_shared.h`). `u` is 20-bit fixed point, `ceil`-biased
/// (`u * 0x100000 + 0xFFFFF`), so `u >> 20` is the first pixel right of it.
/// id's is a 12.20 `int`, which wraps from 2048 pixels wide (the right edge
/// is `(vid.width << 20) + 0xFFFFF`); here it is 44.20 in an `i64`, so any
/// view width is drawn. The values are id's: up to 2047 wide nothing
/// overflows an `int` (an edge that is stepped spans more than one row, so
/// `|u_step|` is under the view's width), and the output is the same pixels.
#[derive(Clone, Copy)]
struct Edge {
    u: i64,
    u_step: i64,
    prev: u32,
    next: u32,
    /// The surface on the edge's left (it is that surface's trailing edge) and
    /// on its right (leading); 0 = none.
    surfs: [u32; 2],
    /// The last scanline it is active on (id links the edges that end on a
    /// scanline into `removeedges[v2]`, `nextremove`; here each knows its
    /// own: [`EdgeState::step_edge`]).
    last: i32,
    nearzi: f32,
    /// The world `medge_t` it was made from (the edge cache's owner test), or
    /// [`NONE`] for a brush model's.
    owner: u32,
}

impl Edge {
    const ZERO: Edge =
        Edge { u: 0, u_step: 0, prev: NONE, next: NONE, surfs: [0, 0], last: i32::MAX, nearzi: 0.0, owner: NONE };
}

/// `surf_t` (`r_shared.h`).
#[derive(Clone, Copy)]
struct Surf {
    next: u32,
    prev: u32,
    /// Whether the scan gave it a span (id's `surf_t.spans` list is not
    /// kept: [`ESpan`]).
    has_spans: bool,
    /// Front-to-back order: smaller is nearer.
    key: i32,
    last_u: i32,
    spanstate: i32,
    flags: u8,
    insubmodel: bool,
    nearzi: f32,
    d_zistepu: f32,
    d_zistepv: f32,
    d_ziorigin: f32,
    /// Which model of the frame ([`Ent`]) and which of its bsp's faces.
    ent: u32,
    face: u32,
}

impl Surf {
    const ZERO: Surf = Surf {
        next: 0,
        prev: 0,
        has_spans: false,
        key: 0,
        last_u: 0,
        spanstate: 0,
        flags: 0,
        insubmodel: false,
        nearzi: 0.0,
        d_zistepu: 0.0,
        d_zistepv: 0.0,
        d_ziorigin: 0.0,
        ent: 0,
        face: 0,
    };
}

/// `espan_t`: `count` pixels from `u` of its row, of surface `surf`. id links
/// each surface's spans into a list (`pnext`) and `D_DrawSurfaces` draws
/// surface after surface; here the spans stay in the order the scan makes
/// them, row after row and left to right, each naming its surface, and
/// [`WorldDraw::rows`] says where each row's begin — so a band's spans are
/// one run of the list, and its pixels are written in memory order. Every
/// pixel is in exactly one span, so the order they are drawn in changes
/// nothing.
#[derive(Clone, Copy)]
struct ESpan {
    u: i32,
    count: i32,
    surf: u32,
}

/// `bedge_t` (`r_bsp.c`): a brush-model edge being clipped into the world's
/// leaves; the vertices by value (id points at `mvertex_t`s).
#[derive(Clone, Copy)]
struct BEdge {
    v: [Vec3; 2],
    pnext: u32,
}

/// `clipplane_t`: one side of the view (`view_clipplanes`).
#[derive(Clone, Copy)]
struct ClipPlane {
    normal: Vec3,
    dist: f32,
    leftedge: bool,
    rightedge: bool,
}

/// One model in the frame's edge list: the world (entity 0), an inline brush
/// entity of the world's bsp, or an external `b_*.bsp` item box.
struct Ent<'a> {
    bsp: &'a Bsp,
    /// `bsp.models[model]`.
    model: usize,
    origin: Vec3,
    /// The entity's `frame` (`R_TextureAnimation`'s alternate cycle).
    frame: i32,
    /// Whether its faces are the world bsp's (so the surface cache and the
    /// dlight mask are keyed by their face index).
    world_bsp: bool,
    /// The frame's dynamic lights, in world space for the world and the inline
    /// brush models alike (`R_DrawBEntitiesOnList` marks a moved door with
    /// `cl_dlights` as they are, and `R_AddDynamicLights` tests them against
    /// the model's own planes); none for the external boxes, which id never
    /// marks.
    dlights: &'a [crate::dlight::DynamicLight],
    /// `entity_rotation` for this entity (`world::entity_rotation_matrix` of
    /// its `angles`): [`world::IDENTITY_ROTATION`] for the world and the
    /// external boxes (neither ever rotates), the inline bmodel's own matrix
    /// for a brush entity (identity too, unless the mission packs turned it).
    rotation: [[f32; 3]; 3],
}

/// The edge renderer's state, the [`Renderer`](super::Renderer)'s: what id
/// keeps in globals and in the model (the edge cache in `medge_t`, leaf keys,
/// visframes), kept across frames as the C keeps it, and the frame's buffers
/// (reused).
pub(super) struct EdgeState {
    /// `r_framecount`.
    framecount: u32,
    /// `r_visframecount` and the leaf it was marked for (`r_oldviewleaf`).
    visframecount: u32,
    oldviewleaf: Option<usize>,
    /// `mnode_t.visframe`, `mleaf_t.visframe`, `mnode_t.parent`, `mleaf_t.parent`
    /// (`Mod_SetParent`), `mleaf_t.key`, `msurface_t.visframe`, and
    /// `medge_t.cachededgeoffset`, all of the world model.
    node_visframe: Vec<u32>,
    leaf_visframe: Vec<u32>,
    node_parent: Vec<i32>,
    leaf_parent: Vec<i32>,
    leaf_key: Vec<i32>,
    face_visframe: Vec<u32>,
    cachededgeoffset: Vec<u32>,
    /// `r_draw.c`'s statics: where the last clipped edges left and entered the
    /// view's left and right sides. Never reset — a face whose edge that would
    /// set one is skipped (cached) reuses an earlier face's (id's quirk).
    leftenter: Vec3,
    leftexit: Vec3,
    rightenter: Vec3,
    rightexit: Vec3,
    /// `r_bsp.c`'s statics `pfrontenter`/`pfrontexit`.
    pfrontenter: Vec3,
    pfrontexit: Vec3,
    /// The frame's view (`R_ViewChanged`, `R_SetupFrame`).
    w: usize,
    h: usize,
    xcenter: f32,
    ycenter: f32,
    xscale: f32,
    yscale: f32,
    xscaleinv: f32,
    yscaleinv: f32,
    fvrectx_adj: f32,
    fvrecty_adj: f32,
    fvrectright_adj: f32,
    fvrectbottom_adj: f32,
    vrect_x_adj_shift20: i64,
    vrectright_adj_shift20: i64,
    vpn: Vec3,
    vright: Vec3,
    vup: Vec3,
    r_origin: Vec3,
    /// The eye in the current model's frame (`modelorg`).
    modelorg: Vec3,
    screenedge: [Vec3; 4],
    clip: [ClipPlane; 4],
    frustum_indexes: [[usize; 6]; 4],
    /// `r_draw.c`'s per-face globals.
    insubmodel: bool,
    currententity: u32,
    cacheoffset: u32,
    r_leftclipped: bool,
    r_rightclipped: bool,
    makeleftedge: bool,
    makerightedge: bool,
    r_nearzionly: bool,
    r_emitted: bool,
    r_nearzi: f32,
    r_u1: f32,
    r_v1: f32,
    r_lzi1: f32,
    r_ceilv1: i32,
    r_lastvertvalid: bool,
    /// `r_pedge`, as the owner written into the edges it makes.
    r_pedge_owner: u32,
    r_currentkey: i32,
    r_currentbkey: i32,
    r_clipflags: u32,
    /// `r_edge.c`'s scan state.
    current_iv: i32,
    fv: f32,
    edge_head_u_shift20: i32,
    edge_tail_u_shift20: i32,
    /// The frame's buffers.
    edges: Vec<Edge>,
    surfs: Vec<Surf>,
    /// The scan's spans, and the index of each row's first (one more entry
    /// than rows: the end): the frame's [`WorldDraw`] takes both.
    spans: Vec<ESpan>,
    row_spans: Vec<u32>,
    /// Span buffers handed back by drawn frames ([`EdgeState::recycle`]).
    spare_spans: Vec<(Vec<ESpan>, Vec<u32>)>,
    newedges: Vec<u32>,
    bedges: Vec<BEdge>,
    dlight_bits: Vec<u32>,
    poly: Vec<Vec3>,
}

impl EdgeState {
    const EMPTY: EdgeState = EdgeState {
        framecount: 1,
        visframecount: 0,
        oldviewleaf: None,
        node_visframe: Vec::new(),
        leaf_visframe: Vec::new(),
        node_parent: Vec::new(),
        leaf_parent: Vec::new(),
        leaf_key: Vec::new(),
        face_visframe: Vec::new(),
        cachededgeoffset: Vec::new(),
        leftenter: [0.0; 3],
        leftexit: [0.0; 3],
        rightenter: [0.0; 3],
        rightexit: [0.0; 3],
        pfrontenter: [0.0; 3],
        pfrontexit: [0.0; 3],
        w: 0,
        h: 0,
        xcenter: 0.0,
        ycenter: 0.0,
        xscale: 1.0,
        yscale: 1.0,
        xscaleinv: 1.0,
        yscaleinv: 1.0,
        fvrectx_adj: 0.0,
        fvrecty_adj: 0.0,
        fvrectright_adj: 0.0,
        fvrectbottom_adj: 0.0,
        vrect_x_adj_shift20: 0,
        vrectright_adj_shift20: 0,
        vpn: [0.0; 3],
        vright: [0.0; 3],
        vup: [0.0; 3],
        r_origin: [0.0; 3],
        modelorg: [0.0; 3],
        screenedge: [[0.0; 3]; 4],
        clip: [ClipPlane { normal: [0.0; 3], dist: 0.0, leftedge: false, rightedge: false }; 4],
        frustum_indexes: [[0; 6]; 4],
        insubmodel: false,
        currententity: 0,
        cacheoffset: 0,
        r_leftclipped: false,
        r_rightclipped: false,
        makeleftedge: false,
        makerightedge: false,
        r_nearzionly: false,
        r_emitted: false,
        r_nearzi: 0.0,
        r_u1: 0.0,
        r_v1: 0.0,
        r_lzi1: 0.0,
        r_ceilv1: 0,
        r_lastvertvalid: false,
        r_pedge_owner: NONE,
        r_currentkey: 0,
        r_currentbkey: 0,
        r_clipflags: 0,
        current_iv: 0,
        fv: 0.0,
        edge_head_u_shift20: 0,
        edge_tail_u_shift20: 0,
        edges: Vec::new(),
        surfs: Vec::new(),
        spans: Vec::new(),
        row_spans: Vec::new(),
        spare_spans: Vec::new(),
        newedges: Vec::new(),
        bedges: Vec::new(),
        dlight_bits: Vec::new(),
        poly: Vec::new(),
    };
}

/// `(int)` of a float as the x86 does it: truncation, and 0x80000000 for a
/// NaN or a value out of range (Rust's `as` saturates).
#[inline]
fn c_ftoi(x: f64) -> i32 {
    if x > -2_147_483_649.0 && x < 2_147_483_648.0 { x as i32 } else { i32::MIN }
}

/// [`c_ftoi`] widened for the edges' 44.20 `u` ([`Edge`]): truncation, the
/// same value wherever id's `int` holds it, and id's 0x80000000 for a NaN.
#[inline]
fn c_ftoi64(x: f64) -> i64 {
    if x.is_nan() { i64::from(i32::MIN) } else { x as i64 }
}

/// `VectorNormalize` (mathlib.c), in floats.
fn vector_normalize(v: Vec3) -> Vec3 {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length != 0.0 {
        let ilength = 1.0 / length;
        [v[0] * ilength, v[1] * ilength, v[2] * ilength]
    } else {
        v
    }
}

/// Whether `bsp` has the node tree and leaves id's world walk needs (every
/// map qbsp writes does; the synthetic test rooms may not).
fn has_tree(bsp: &Bsp) -> bool {
    !bsp.nodes.is_empty() && !bsp.leafs.is_empty()
}

/// A child reference of a `dnode_t`: `>= 0` a node, `< 0` the leaf `-1 - c`.
#[inline]
fn child_ref(c: i16) -> i32 {
    c as i32
}

/// `R_ViewChanged`'s `screenedge`: the normals, in view space (right, up,
/// forward), of the planes through the eye and the view's left, right, top
/// and bottom sides, from the fields of view the projection implies
/// (`horizontalFieldOfView` = width / xscale, `verticalFieldOfView` = height
/// / yscale).
pub(super) fn screen_edges(w: usize, h: usize, xscale: f32, yscale: f32) -> [Vec3; 4] {
    let hfov = w as f32 / xscale;
    let vfov = h as f32 / yscale;
    [
        vector_normalize([-1.0 / (0.5 * hfov), 0.0, 1.0]),
        vector_normalize([1.0 / (0.5 * hfov), 0.0, 1.0]),
        vector_normalize([0.0, -1.0 / (0.5 * vfov), 1.0]),
        vector_normalize([0.0, 1.0 / (0.5 * vfov), 1.0]),
    ]
}

/// [`screen_edges`] for a frame's image wherever it lies on its view
/// ([`ViewGeom`]): id's for a whole view; for a window the planes through the
/// eye and the window's own four sides, which need not straddle the centre
/// (the overlay's windows lie wholly below it, and to one side). A side `a`
/// (the window's edge from the centre, over the scale, positive outwards)
/// keeps `[-1, 0, a]` (left; `[1, 0, a]` right, `[0, -1, a]` top, `[0, 1,
/// a]` bottom): id's `[-1/a, 0, 1]` times `a`, the same plane facing the
/// same way while `a > 0`, and still the side's plane, facing in, when `a`
/// is 0 or negative (a side on or past the centre), where id's has none.
pub(super) fn view_edges(geom: &ViewGeom, p: &Projection) -> [Vec3; 4] {
    if geom.is_whole() {
        return screen_edges(geom.w, geom.h, p.xscale, p.yscale);
    }
    let left = p.cx / p.xscale;
    let right = (geom.w as f32 - p.cx) / p.xscale;
    let top = p.cy / p.yscale;
    let bottom = (geom.h as f32 - p.cy) / p.yscale;
    [
        vector_normalize([-1.0, 0.0, left]),
        vector_normalize([1.0, 0.0, right]),
        vector_normalize([0.0, -1.0, top]),
        vector_normalize([0.0, 1.0, bottom]),
    ]
}

/// `R_TransformFrustum`: the view's four sides (`screenedge`) as planes
/// `(normal, dist)` in the frame whose axes are `vpn`, `vright`, `vup` and
/// whose eye is `modelorg` — `view_clipplanes`. A point is inside a side
/// where `dot(normal, p) - dist >= 0`.
pub(super) fn frustum_planes(
    screenedge: &[Vec3; 4],
    vpn: Vec3,
    vright: Vec3,
    vup: Vec3,
    modelorg: Vec3,
) -> [(Vec3, f32); 4] {
    screenedge.map(|se| {
        let v = [se[2], -se[0], se[1]];
        let v2 = [
            v[1] * vright[0] + v[2] * vup[0] + v[0] * vpn[0],
            v[1] * vright[1] + v[2] * vup[1] + v[0] * vpn[1],
            v[1] * vright[2] + v[2] * vup[2] + v[0] * vpn[2],
        ];
        (v2, dot(modelorg, v2))
    })
}

impl EdgeState {
    /// A renderer's edge state before any map: everything at zero.
    pub(super) fn new() -> EdgeState {
        EdgeState::EMPTY
    }

    /// The world and the brush entities of `frame` as id sorts them
    /// (`R_EdgeDrawing`: `R_RenderWorld`, `R_DrawBEntitiesOnList`,
    /// `R_ScanEdges`), and `D_DrawSurfaces`' choice for each surface that
    /// owns a span — the surface cache consulted, the bake of any block it
    /// does not have added to the frame's `jobs` — so that
    /// [`WorldDraw::draw_band`] can draw any rows of the view from them. The
    /// world is the one [`EdgeState::begin_map`] was last called for. `None`
    /// for a view larger than any setting allows (the caller clamps to the
    /// cvars' limit; this refuses the 8K limit's past).
    pub(super) fn build<'a>(
        &mut self,
        frame: &Frame<'_, 'a>,
        caches: &mut SurfaceCaches,
        jobs: &mut Vec<BakeJob<'a>>,
        prof: &mut Profiler,
    ) -> Option<WorldDraw<'a>> {
        let (w, h) = (frame.w, frame.h);
        if w == 0 || h == 0 || w > super::HIRES_MAXWIDTH || h > super::HIRES_MAXHEIGHT {
            return None;
        }
        let scene = frame.scene;
        let bsp = scene.world;
        let dlights = scene.dlights;
        let t0 = prof.now();
        self.setup_frame(frame);
        self.mark_leaves(bsp);

        // The frame's models: the world, then the brush entities in list order
        // (inline submodels, then the external boxes).
        let mut ents: Vec<Ent> = Vec::with_capacity(1 + scene.bmodels.len() + scene.external.len());
        ents.push(Ent {
            bsp,
            model: 0,
            origin: [0.0; 3],
            frame: 0,
            world_bsp: true,
            dlights,
            rotation: world::IDENTITY_ROTATION,
        });
        for bm in scene.bmodels {
            if bm.model_index == 0 || bm.model_index >= bsp.models.len() {
                continue;
            }
            // `R_DrawBEntitiesOnList` hands `R_MarkLights` `&cl_dlights[k]`
            // untranslated, and `R_AddDynamicLights` measures
            // `cl_dlights[lnum].origin` against the surface's plane and
            // texinfo, which are the model's own (a door's faces where the
            // map put it): a light is not moved into a moved model's frame,
            // so a moved door or lift is lit as if it had not moved, as in id.
            // Its *rotation* is the same story (R_MarkLights never saw
            // `entity_rotation` either): unaffected by this round.
            ents.push(Ent {
                bsp,
                model: bm.model_index,
                origin: bm.origin,
                frame: bm.frame,
                world_bsp: true,
                dlights,
                rotation: world::entity_rotation_matrix(bm.angles),
            });
        }
        for ext in scene.external {
            if ext.bsp.models.is_empty() {
                continue;
            }
            // Instanced item boxes (`maps/b_*.bsp`) never rotate in id either
            // (out of this round's scope: see `BModelInstance::angles`).
            ents.push(Ent {
                bsp: ext.bsp,
                model: 0,
                origin: ext.origin,
                frame: 0,
                world_bsp: false,
                dlights: &[],
                rotation: world::IDENTITY_ROTATION,
            });
        }

        // `R_PushDlights` over the world, and `R_MarkLights` over each inline
        // brush model's own subtree (`R_DrawBEntitiesOnList`); the external
        // boxes are instanced models, which id never marks.
        let mut bits = std::mem::take(&mut self.dlight_bits);
        let world_head = bsp.models.first().and_then(|m| m.headnode.first().copied()).unwrap_or(0);
        mark_dlights(bsp, world_head, dlights, &mut bits);
        for e in ents.iter().skip(1).filter(|e| e.world_bsp) {
            let head = e.bsp.models[e.model].headnode.first().copied().unwrap_or(0);
            mark_dlights_more(bsp, head, e.dlights, &mut bits);
        }

        // Phase times as offsets from `t0` (only while profiling).
        let lap = || t0.map_or(0, |t| t.elapsed().as_nanos() as u64);
        self.begin_edge_frame();
        self.render_world(bsp);
        let t1 = lap();
        self.draw_bentities(bsp, &ents);
        let t2 = lap();
        self.scan_edges();
        let t3 = lap();
        let world = self.prepare_surfaces(frame, caches, jobs, prof, &ents, &bits);
        self.dlight_bits = bits;
        if prof.on() {
            let t4 = lap();
            let (edges, surfs, spans) =
                (self.edges.len() as u64 - FIRST_EDGE as u64, self.surfs.len() as u64 - 2, world.spans.len() as u64);
            prof.add(|s| {
                // world = the whole pass but the brush entities' edge setup,
                // which goes to `submodel` (their spans are drawn with the
                // world's); `sort` = the world walk to edges, `setup` = the scan,
                // `surf` = D_DrawSurfaces (here its per-surface setup; the
                // bands add their bakes and spans).
                s.world_ns += t1 + (t4 - t2);
                s.submodel_ns += t2 - t1;
                s.world_sort_ns += t1;
                s.world_setup_ns += t3 - t2;
                s.world_surf_ns += t4 - t3;
                s.edges_emitted += edges;
                s.surfs_emitted += surfs;
                s.spans_emitted += spans;
                s.edges_peak = s.edges_peak.max(edges);
                s.surfs_peak = s.surfs_peak.max(surfs);
            });
        }
        Some(world)
    }

    /// `R_NewMap` for the edge renderer (`Mod_LoadBrushModel`,
    /// `Mod_SetParent`): size the per-model arrays for `bsp`, everything id
    /// keeps in the model at zero, and forget the last view leaf.
    pub(super) fn begin_map(&mut self, bsp: &Bsp) {
        self.oldviewleaf = None;
        self.node_visframe = vec![0; bsp.nodes.len()];
        self.leaf_visframe = vec![0; bsp.leafs.len()];
        self.node_parent = vec![-1; bsp.nodes.len()];
        self.leaf_parent = vec![-1; bsp.leafs.len()];
        self.leaf_key = vec![0; bsp.leafs.len()];
        self.face_visframe = vec![0; bsp.faces.len()];
        self.cachededgeoffset = vec![0; bsp.edges.len()];
        // Mod_SetParent (loadmodel->nodes, NULL), iteratively.
        let root = bsp.models.first().and_then(|m| m.headnode.first().copied()).unwrap_or(0);
        let mut stack: Vec<(i32, i32)> = vec![(root, -1)];
        // Every node is visited once and pushes two children (a malformed,
        // cyclic tree stops here).
        let mut budget = 2 * bsp.nodes.len() + 2;
        while let Some((node, parent)) = stack.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if node < 0 {
                if let Some(p) = self.leaf_parent.get_mut((-1 - node) as usize) {
                    *p = parent;
                }
                continue;
            }
            let Some(n) = bsp.nodes.get(node as usize) else { continue };
            self.node_parent[node as usize] = parent;
            stack.push((child_ref(n.children[0]), node));
            stack.push((child_ref(n.children[1]), node));
        }
    }

    /// `R_ViewChanged`'s projection and clip planes, `R_SetupFrame`'s view and
    /// `R_TransformFrustum` / `R_SetUpFrustumIndexes`, for a `w x h` view.
    fn setup_frame(&mut self, frame: &Frame) {
        let (cam, w, h) = (&frame.cam, frame.w, frame.h);
        self.framecount = self.framecount.wrapping_add(1);
        let proj = Projection::new(cam, &frame.geom, frame.scene.options.aspect());
        let Projection { cx, cy, xscale, yscale } = proj;
        let (vpn, vright, vup) = cam.basis();
        self.w = w;
        self.h = h;
        // id's pixel centres are on the integers: xcenter = width/2 - 0.5.
        self.xcenter = cx - 0.5;
        self.ycenter = cy - 0.5;
        self.xscale = xscale;
        self.yscale = yscale;
        self.xscaleinv = 1.0 / xscale;
        self.yscaleinv = 1.0 / yscale;
        let (wf, hf) = (w as f32, h as f32);
        self.fvrectx_adj = -0.5;
        self.fvrecty_adj = -0.5;
        self.fvrectright_adj = wf - 0.5;
        self.fvrectbottom_adj = hf - 0.5;
        self.vrect_x_adj_shift20 = (1 << 19) - 1;
        self.vrectright_adj_shift20 = ((w as i64) << 20) + (1 << 19) - 1;
        self.vpn = vpn;
        self.vright = vright;
        self.vup = vup;
        self.r_origin = cam.pos;
        self.modelorg = cam.pos;
        self.screenedge = view_edges(&frame.geom, &proj);
        for (i, c) in self.clip.iter_mut().enumerate() {
            c.leftedge = i == 0;
            c.rightedge = i == 1;
        }
        self.transform_frustum();
        // R_SetUpFrustumIndexes
        for i in 0..4 {
            for j in 0..3 {
                if self.clip[i].normal[j] < 0.0 {
                    self.frustum_indexes[i][j] = j;
                    self.frustum_indexes[i][j + 3] = j + 3;
                } else {
                    self.frustum_indexes[i][j] = j + 3;
                    self.frustum_indexes[i][j + 3] = j;
                }
            }
        }
    }

    /// `R_TransformFrustum`: the view's sides in the current model's frame.
    fn transform_frustum(&mut self) {
        let planes = frustum_planes(&self.screenedge, self.vpn, self.vright, self.vup, self.modelorg);
        for (c, (normal, dist)) in self.clip.iter_mut().zip(planes) {
            c.normal = normal;
            c.dist = dist;
        }
    }

    /// `R_MarkLeaves`: mark every leaf in the view leaf's PVS and the nodes
    /// above it with a new `r_visframecount`, once per view leaf. No PVS (no
    /// vis data, leaf 0, a leaf without vis info) marks every leaf.
    fn mark_leaves(&mut self, bsp: &Bsp) {
        let viewleaf = point_in_leaf(bsp, self.r_origin).unwrap_or(0);
        if self.oldviewleaf == Some(viewleaf) {
            return;
        }
        self.visframecount = self.visframecount.wrapping_add(1);
        self.oldviewleaf = Some(viewleaf);
        let numleafs =
            bsp.models.first().map_or(0, |m| m.visleafs.max(0) as usize).min(bsp.leafs.len().saturating_sub(1));
        let vis = match bsp.leafs.get(viewleaf) {
            Some(leaf) if viewleaf != 0 && !bsp.visibility.is_empty() => {
                crate::bsp::decompress_vis(&bsp.visibility, leaf.visofs, numleafs)
            }
            _ => vec![true; numleafs + 1],
        };
        let frame = self.visframecount;
        for i in 0..numleafs {
            if !vis.get(i + 1).copied().unwrap_or(false) {
                continue;
            }
            let leaf = i + 1;
            self.leaf_visframe[leaf] = frame;
            let mut node = self.leaf_parent[leaf];
            let mut budget = bsp.nodes.len();
            while node >= 0 && budget > 0 {
                budget -= 1;
                let n = node as usize;
                if self.node_visframe[n] == frame {
                    break;
                }
                self.node_visframe[n] = frame;
                node = self.node_parent[n];
            }
        }
    }

    #[inline]
    fn visframe_of(&self, node: i32) -> u32 {
        if node >= 0 {
            self.node_visframe.get(node as usize).copied().unwrap_or(0)
        } else {
            self.leaf_visframe.get((-1 - node) as usize).copied().unwrap_or(0)
        }
    }

    /// `R_BeginEdgeFrame` (r_draw_order 0: `R_GenerateSpans`, the background
    /// at key 0x7FFFFFFF).
    fn begin_edge_frame(&mut self) {
        self.edges.clear();
        self.edges.resize(FIRST_EDGE as usize, Edge::ZERO);
        self.surfs.clear();
        self.surfs.push(Surf::ZERO); // surface 0: the "no surface" dummy
        self.surfs.push(Surf { flags: SURF_DRAWBACKGROUND, key: 0x7FFF_FFFF, ..Surf::ZERO });
        self.r_currentkey = 0;
        self.newedges.clear();
        self.newedges.resize(self.h, NONE);
        if let Some((spans, rows)) = self.spare_spans.pop() {
            (self.spans, self.row_spans) = (spans, rows);
        }
        self.spans.clear();
        self.row_spans.clear();
    }

    /// Take back a drawn frame's span buffers for the next one.
    pub(super) fn recycle(&mut self, world: WorldDraw) {
        self.spare_spans.push((world.spans, world.rows));
    }

    // -----------------------------------------------------------------------
    // r_bsp.c: the world walk
    // -----------------------------------------------------------------------

    /// `R_RenderWorld`.
    fn render_world(&mut self, bsp: &Bsp) {
        self.currententity = 0;
        self.insubmodel = false;
        self.modelorg = self.r_origin;
        let root = bsp.models.first().and_then(|m| m.headnode.first().copied()).unwrap_or(0);
        if !has_tree(bsp) {
            // No node tree (the synthetic test rooms).
            self.render_world_flat(bsp);
            return;
        }
        self.recursive_world_node(bsp, root, 15, 0);
    }

    /// A bsp with no nodes (the synthetic test rooms; never id's) has no
    /// front-to-back order: its faces go in as one leaf's brush model would —
    /// one key, sorted on 1/z at their edges (`R_DrawSubmodelPolygons`).
    fn render_world_flat(&mut self, bsp: &Bsp) {
        let Some(m) = bsp.models.first() else { return };
        let f0 = m.firstface.max(0) as usize;
        let end = (f0 + m.numfaces.max(0) as usize).min(bsp.faces.len());
        self.insubmodel = true;
        for fi in f0..end {
            let face = &bsp.faces[fi];
            let Some(plane) = usize::try_from(face.planenum).ok().and_then(|p| bsp.planes.get(p)) else { continue };
            let d = (dot(self.modelorg, plane.normal) - plane.dist) as f64;
            let back = face.side != 0;
            if (back && d < -BACKFACE_EPSILON) || (!back && d > BACKFACE_EPSILON) {
                self.r_currentkey = 0;
                self.render_face(bsp, fi, 15);
            }
        }
        self.insubmodel = false;
    }

    /// `R_RecursiveWorldNode`.
    fn recursive_world_node(&mut self, bsp: &Bsp, node: i32, mut clipflags: u32, depth: u32) {
        if depth > MAX_DEPTH || self.visframe_of(node) != self.visframecount {
            return;
        }
        let (minmaxs, leaf) = if node < 0 {
            let li = (-1 - node) as usize;
            let Some(l) = bsp.leafs.get(li) else { return };
            if l.contents == CONTENTS_SOLID {
                return;
            }
            ([l.mins[0], l.mins[1], l.mins[2], l.maxs[0], l.maxs[1], l.maxs[2]].map(|v| v as f32), Some(li))
        } else {
            let Some(n) = bsp.nodes.get(node as usize) else { return };
            ([n.mins[0], n.mins[1], n.mins[2], n.maxs[0], n.maxs[1], n.maxs[2]].map(|v| v as f32), None)
        };
        // Cull against the clip planes unless trivially accepted.
        if clipflags != 0 {
            for i in 0..4 {
                if clipflags & (1 << i) == 0 {
                    continue;
                }
                let pindex = self.frustum_indexes[i];
                let c = self.clip[i];
                let rejectpt = [minmaxs[pindex[0]], minmaxs[pindex[1]], minmaxs[pindex[2]]];
                let d = dot(rejectpt, c.normal) as f64 - c.dist as f64;
                if d <= 0.0 {
                    return;
                }
                let acceptpt = [minmaxs[pindex[3]], minmaxs[pindex[4]], minmaxs[pindex[5]]];
                let d = dot(acceptpt, c.normal) as f64 - c.dist as f64;
                if d >= 0.0 {
                    clipflags &= !(1 << i); // node is entirely on screen
                }
            }
        }
        if let Some(li) = leaf {
            let l = &bsp.leafs[li];
            let first = l.firstmarksurface as usize;
            let count = l.nummarksurfaces as usize;
            if let Some(marks) = bsp.marksurfaces.get(first..first + count) {
                for &m in marks {
                    if let Some(v) = self.face_visframe.get_mut(m as usize) {
                        *v = self.framecount;
                    }
                }
            }
            self.leaf_key[li] = self.r_currentkey;
            self.r_currentkey += 1; // all bmodels in a leaf share the same key
            return;
        }
        let n = &bsp.nodes[node as usize];
        let Some(plane) = usize::try_from(n.planenum).ok().and_then(|p| bsp.planes.get(p)) else { return };
        let dotv: f64 = match plane.ptype {
            0..=2 => (self.modelorg[plane.ptype as usize] - plane.dist) as f64,
            _ => (dot(self.modelorg, plane.normal) - plane.dist) as f64,
        };
        let side = if dotv >= 0.0 { 0 } else { 1 };
        let (front, back) = (child_ref(n.children[side]), child_ref(n.children[side ^ 1]));
        self.recursive_world_node(bsp, front, clipflags, depth + 1);
        let c = n.numfaces as usize;
        if c != 0 {
            let first = n.firstface as usize;
            let want_back = if dotv < -BACKFACE_EPSILON {
                Some(true)
            } else if dotv > BACKFACE_EPSILON {
                Some(false)
            } else {
                None
            };
            if let Some(want_back) = want_back {
                for fi in first..(first + c).min(bsp.faces.len()) {
                    let back_face = bsp.faces[fi].side != 0;
                    if back_face == want_back && self.face_visframe[fi] == self.framecount {
                        self.render_face(bsp, fi, clipflags);
                    }
                }
            }
            // all surfaces on the same node share the same sequence number
            self.r_currentkey += 1;
        }
        self.recursive_world_node(bsp, back, clipflags, depth + 1);
    }

    // -----------------------------------------------------------------------
    // r_draw.c: faces to edges
    // -----------------------------------------------------------------------

    /// The clip-plane chain for `clipflags` (`view_clipplanes[i].next`): the
    /// set planes, lowest first.
    fn clip_chain(clipflags: u32) -> ([u8; 4], usize) {
        let mut chain = [0u8; 4];
        let mut n = 0;
        for i in 0..4u8 {
            if clipflags & (1 << i) != 0 {
                chain[n] = i;
                n += 1;
            }
        }
        (chain, n)
    }

    /// `view_clipplanes[1].next`: the chain after the right plane.
    fn after_right(chain: &[u8]) -> &[u8] {
        match chain.iter().position(|&p| p == 1) {
            Some(k) => &chain[k + 1..],
            None => &[],
        }
    }

    /// Transform and project a vertex as `R_EmitEdge` does: `(u, v, 1/z)`.
    #[inline]
    fn project(&self, p: Vec3) -> (f32, f32, f32) {
        let local = [p[0] - self.modelorg[0], p[1] - self.modelorg[1], p[2] - self.modelorg[2]];
        let t0 = dot(local, self.vright);
        let t1 = dot(local, self.vup);
        let mut t2 = dot(local, self.vpn);
        if t2 < NEAR_CLIP {
            t2 = NEAR_CLIP;
        }
        let lzi = 1.0 / t2;
        let scale = self.xscale * lzi;
        let mut u = self.xcenter + scale * t0;
        if u < self.fvrectx_adj {
            u = self.fvrectx_adj;
        }
        if u > self.fvrectright_adj {
            u = self.fvrectright_adj;
        }
        let scale = self.yscale * lzi;
        let mut v = self.ycenter - scale * t1;
        if v < self.fvrecty_adj {
            v = self.fvrecty_adj;
        }
        if v > self.fvrectbottom_adj {
            v = self.fvrectbottom_adj;
        }
        (u, v, lzi)
    }

    /// `R_EmitEdge`.
    fn emit_edge(&mut self, pv0: Vec3, pv1: Vec3) {
        let (u0, v0, mut lzi0, ceilv0);
        if self.r_lastvertvalid {
            (u0, v0, lzi0, ceilv0) = (self.r_u1, self.r_v1, self.r_lzi1, self.r_ceilv1);
        } else {
            let (u, v, lzi) = self.project(pv0);
            (u0, v0, lzi0, ceilv0) = (u, v, lzi, (v as f64).ceil() as i32);
        }
        let (u1, v1, lzi1) = self.project(pv1);
        (self.r_u1, self.r_v1, self.r_lzi1) = (u1, v1, lzi1);
        if lzi1 > lzi0 {
            lzi0 = lzi1;
        }
        if lzi0 > self.r_nearzi {
            self.r_nearzi = lzi0; // for mipmap finding
        }
        // for right edges, all we want is the effect on 1/z
        if self.r_nearzionly {
            return;
        }
        self.r_emitted = true;
        self.r_ceilv1 = (v1 as f64).ceil() as i32;
        if ceilv0 == self.r_ceilv1 {
            // we cache unclipped horizontal edges as fully clipped
            if self.cacheoffset != NOT_CACHEABLE {
                self.cacheoffset = FULLY_CLIPPED_CACHED | (self.framecount & FRAMECOUNT_MASK);
            }
            return; // horizontal edge
        }
        let surf = self.surfs.len() as u32;
        let (v, v2, u_step, u, surfs);
        if ceilv0 < self.r_ceilv1 {
            // trailing edge (go from p1 to p2)
            v = ceilv0;
            v2 = self.r_ceilv1 - 1;
            surfs = [surf, 0];
            u_step = (self.r_u1 - u0) / (self.r_v1 - v0);
            u = u0 + (v as f32 - v0) * u_step;
        } else {
            // leading edge (go from p2 to p1)
            v2 = ceilv0 - 1;
            v = self.r_ceilv1;
            surfs = [0, surf];
            u_step = (u0 - self.r_u1) / (v0 - self.r_v1);
            u = self.r_u1 + (v as f32 - self.r_v1) * u_step;
        }
        if v < 0 || v2 < v || v2 as usize >= self.h {
            return; // cannot happen with a clamped projection; a NaN guard
        }
        let mut eu = c_ftoi64((u * 1_048_576.0 + 1_048_575.0) as f64);
        // avoid stepping off the edges of the screen
        if eu < self.vrect_x_adj_shift20 {
            eu = self.vrect_x_adj_shift20;
        }
        if eu > self.vrectright_adj_shift20 {
            eu = self.vrectright_adj_shift20;
        }
        let e = self.edges.len() as u32;
        self.edges.push(Edge {
            u: eu,
            u_step: c_ftoi64((u_step * 1_048_576.0) as f64),
            prev: NONE,
            next: NONE,
            surfs,
            last: v2,
            nearzi: lzi0,
            owner: self.r_pedge_owner,
        });
        // sort the edge in normally
        let mut u_check = eu;
        if surfs[0] != 0 {
            u_check = u_check.wrapping_add(1); // sort trailers after leaders
        }
        let v = v as usize;
        let head = self.newedges[v];
        if head == NONE || self.edges[head as usize].u >= u_check {
            self.edges[e as usize].next = head;
            self.newedges[v] = e;
        } else {
            let mut pcheck = head;
            loop {
                let nx = self.edges[pcheck as usize].next;
                if nx == NONE || self.edges[nx as usize].u >= u_check {
                    break;
                }
                pcheck = nx;
            }
            self.edges[e as usize].next = self.edges[pcheck as usize].next;
            self.edges[pcheck as usize].next = e;
        }
    }

    /// `R_ClipEdge`: clip `pv0 -> pv1` against the planes of `chain`, noting
    /// where it leaves and enters the left and right sides, then emit it.
    fn clip_edge(&mut self, pv0: Vec3, pv1: Vec3, chain: &[u8]) {
        for (k, &pi) in chain.iter().enumerate() {
            let clip = self.clip[pi as usize];
            let d0 = dot(pv0, clip.normal) - clip.dist;
            let d1 = dot(pv1, clip.normal) - clip.dist;
            let lerp = |f: f32| {
                [pv0[0] + f * (pv1[0] - pv0[0]), pv0[1] + f * (pv1[1] - pv0[1]), pv0[2] + f * (pv1[2] - pv0[2])]
            };
            if d0 >= 0.0 {
                // point 0 is unclipped
                if d1 >= 0.0 {
                    continue; // both points are unclipped
                }
                // only point 1 is clipped; we don't cache clipped edges
                self.cacheoffset = NOT_CACHEABLE;
                let clipvert = lerp(d0 / (d0 - d1));
                if clip.leftedge {
                    self.r_leftclipped = true;
                    self.leftexit = clipvert;
                } else if clip.rightedge {
                    self.r_rightclipped = true;
                    self.rightexit = clipvert;
                }
                self.clip_edge(pv0, clipvert, &chain[k + 1..]);
                return;
            }
            // point 0 is clipped
            if d1 < 0.0 {
                // both points are clipped; we do cache fully clipped edges
                if !self.r_leftclipped {
                    self.cacheoffset = FULLY_CLIPPED_CACHED | (self.framecount & FRAMECOUNT_MASK);
                }
                return;
            }
            // only point 0 is clipped
            self.r_lastvertvalid = false;
            // we don't cache partially clipped edges
            self.cacheoffset = NOT_CACHEABLE;
            let clipvert = lerp(d0 / (d0 - d1));
            if clip.leftedge {
                self.r_leftclipped = true;
                self.leftenter = clipvert;
            } else if clip.rightedge {
                self.r_rightclipped = true;
                self.rightenter = clipvert;
            }
            self.clip_edge(clipvert, pv1, &chain[k + 1..]);
            return;
        }
        // add the edge
        self.emit_edge(pv0, pv1);
    }

    /// `R_EmitCachedEdge`: this face shares an edge an earlier face made.
    fn emit_cached_edge(&mut self, cached: u32) {
        let surf = self.surfs.len() as u32;
        let e = &mut self.edges[(FIRST_EDGE + cached) as usize];
        if e.surfs[0] == 0 {
            e.surfs[0] = surf;
        } else {
            e.surfs[1] = surf;
        }
        if e.nearzi > self.r_nearzi {
            self.r_nearzi = e.nearzi; // for mipmap finding
        }
        self.r_emitted = true;
    }

    /// The model the current entity draws from (`currententity->model`).
    fn vertex(bsp: &Bsp, i: u16) -> Option<Vec3> {
        bsp.vertexes.get(i as usize).map(|v| v.point)
    }

    /// `R_RenderFace`: push face `fi` of `bsp` (the current entity's model)
    /// through the clip planes into the edge list and post its surface.
    ///
    /// A face whose plane index is out of range (a malformed map; id would
    /// read past `mplane_t`) is skipped whole: its edges would name a surface
    /// [`Self::post_surface`] cannot post.
    fn render_face(&mut self, bsp: &Bsp, fi: usize, clipflags: u32) {
        let face = &bsp.faces[fi];
        if usize::try_from(face.planenum).ok().and_then(|p| bsp.planes.get(p)).is_none() {
            return;
        }
        let (chain, nchain) = Self::clip_chain(clipflags);
        let chain = &chain[..nchain];
        self.r_emitted = false;
        self.r_nearzi = 0.0;
        self.r_nearzionly = false;
        self.makeleftedge = false;
        self.makerightedge = false;
        self.r_lastvertvalid = false;
        // Only the world's own edges are ever looked up in the edge cache (a
        // brush model's edges are its own: qbsp shares edges within a model),
        // so a brush model's edges get no owner and write no cache entry.
        let world_edges = !self.insubmodel;
        for i in 0..face.numedges.max(0) as usize {
            let Some(&lindex) = usize::try_from(face.firstedge).ok().and_then(|f| bsp.surfedges.get(f + i)) else {
                continue;
            };
            let (ei, a, b) = if lindex > 0 { (lindex as usize, 0, 1) } else { ((-(lindex as i64)) as usize, 1, 0) };
            let Some(medge) = bsp.edges.get(ei) else { continue };
            self.r_pedge_owner = if world_edges { ei as u32 } else { NONE };
            // if the edge is cached, we can just reuse the edge
            if !self.insubmodel {
                let off = self.cachededgeoffset[ei];
                if off & FULLY_CLIPPED_CACHED != 0 {
                    if off & FRAMECOUNT_MASK == self.framecount & FRAMECOUNT_MASK {
                        self.r_lastvertvalid = false;
                        continue;
                    }
                } else {
                    let made = self.edges.len() as u32 - FIRST_EDGE;
                    if made > off && self.edges[(FIRST_EDGE + off) as usize].owner == ei as u32 {
                        self.emit_cached_edge(off);
                        self.r_lastvertvalid = false;
                        continue;
                    }
                }
            }
            let (Some(p0), Some(p1)) = (Self::vertex(bsp, medge.v[a]), Self::vertex(bsp, medge.v[b])) else {
                continue;
            };
            // assume it's cacheable
            self.cacheoffset = self.edges.len() as u32 - FIRST_EDGE;
            self.r_leftclipped = false;
            self.r_rightclipped = false;
            self.clip_edge(p0, p1, chain);
            if world_edges {
                self.cachededgeoffset[ei] = self.cacheoffset;
            }
            self.makeleftedge |= self.r_leftclipped;
            self.makerightedge |= self.r_rightclipped;
            self.r_lastvertvalid = true;
        }
        self.finish_face(chain);
        if !self.r_emitted {
            return;
        }
        let key = self.r_currentkey;
        self.r_currentkey += 1;
        self.post_surface(bsp, fi, key, self.insubmodel);
    }

    /// The end of `R_RenderFace` / `R_RenderBmodelFace`: the extra edge along
    /// the left side, and the right side's `1/z`.
    fn finish_face(&mut self, chain: &[u8]) {
        self.r_pedge_owner = NONE; // the dummy `tedge`
        if self.makeleftedge {
            self.r_lastvertvalid = false;
            let (a, b) = (self.leftexit, self.leftenter);
            self.clip_edge(a, b, chain.get(1..).unwrap_or(&[]));
        }
        if self.makerightedge {
            self.r_lastvertvalid = false;
            self.r_nearzionly = true;
            let (a, b) = (self.rightexit, self.rightenter);
            self.clip_edge(a, b, Self::after_right(chain));
        }
    }

    /// Post the current face as a surface (the end of `R_RenderFace`): its
    /// `1/z` plane in screen space, `D_DrawZSpans`' and the 1/z sort's.
    fn post_surface(&mut self, bsp: &Bsp, fi: usize, key: i32, insubmodel: bool) {
        let face = &bsp.faces[fi];
        let Some(plane) = usize::try_from(face.planenum).ok().and_then(|p| bsp.planes.get(p)) else { return };
        let p_normal = [dot(plane.normal, self.vright), dot(plane.normal, self.vup), dot(plane.normal, self.vpn)];
        let distinv = 1.0 / (plane.dist - dot(self.modelorg, plane.normal));
        let d_zistepu = p_normal[0] * self.xscaleinv * distinv;
        let d_zistepv = -p_normal[1] * self.yscaleinv * distinv;
        let d_ziorigin = p_normal[2] * distinv - self.xcenter * d_zistepu - self.ycenter * d_zistepv;
        let flags = face_flags(bsp, face);
        self.surfs.push(Surf {
            next: 0,
            prev: 0,
            has_spans: false,
            key,
            last_u: 0,
            spanstate: 0,
            flags,
            insubmodel,
            nearzi: self.r_nearzi,
            d_zistepu,
            d_zistepv,
            d_ziorigin,
            ent: self.currententity,
            face: fi as u32,
        });
    }
}

/// What [`EdgeState::prepare_face`] reads besides the surface: the frame
/// and what `D_DrawSurfaces` sets up for it once.
#[derive(Clone, Copy)]
struct FacePass<'p, 's, 'a> {
    frame: &'p Frame<'s, 'a>,
    sview: &'p ScreenProj,
    mipview: &'p MipView,
    ents: &'p [Ent<'a>],
    bits: &'p [u32],
    /// The port's flat shading direction, for a face with no lightmap.
    light_dir: Vec3,
    /// `r_clearcolor`'s palette index.
    clear: u8,
}

/// `Mod_LoadFaces`' flags for a face: `SURF_PLANEBACK`, and `SURF_DRAWSKY` /
/// `SURF_DRAWTURB` from its texture's name.
fn face_flags(bsp: &Bsp, face: &DFace) -> u8 {
    let mut flags = if face.side != 0 { SURF_PLANEBACK } else { 0 };
    let kind = usize::try_from(face.texinfo)
        .ok()
        .and_then(|t| bsp.texinfo.get(t))
        .and_then(|ti| usize::try_from(ti.miptex).ok())
        .and_then(|m| bsp.textures.get(m))
        .and_then(|t| t.as_ref())
        .map(|mt| classify_surface(&mt.name));
    match kind {
        Some(SurfKind::Sky) => flags |= SURF_DRAWSKY,
        Some(SurfKind::Turb) => flags |= SURF_DRAWTURB,
        _ => {}
    }
    flags
}

impl EdgeState {
    // -----------------------------------------------------------------------
    // r_main.c / r_bsp.c: brush entities into the same edge list
    // -----------------------------------------------------------------------

    /// `R_BmodelCheckBBox` (an unrotated model): the clip flags its box needs
    /// against the view's sides, or [`BMODEL_FULLY_CLIPPED`].
    fn bmodel_check_bbox(&self, minmaxs: &[f32; 6]) -> u32 {
        let mut clipflags = 0;
        for i in 0..4 {
            let pindex = self.frustum_indexes[i];
            let c = self.clip[i];
            let rejectpt = [minmaxs[pindex[0]], minmaxs[pindex[1]], minmaxs[pindex[2]]];
            let d = dot(rejectpt, c.normal) as f64 - c.dist as f64;
            if d <= 0.0 {
                return BMODEL_FULLY_CLIPPED;
            }
            let acceptpt = [minmaxs[pindex[3]], minmaxs[pindex[4]], minmaxs[pindex[5]]];
            let d = dot(acceptpt, c.normal) as f64 - c.dist as f64;
            if d <= 0.0 {
                clipflags |= 1 << i;
            }
        }
        clipflags
    }

    /// `R_SplitEntityOnNode2`: the first world node whose plane splits the
    /// box, or the (non-solid) leaf holding all of it; `None` when the box is
    /// only in solid space or outside the PVS.
    fn split_entity_on_node(&self, world: &Bsp, mut node: i32, emins: Vec3, emaxs: Vec3) -> Option<i32> {
        for _ in 0..MAX_DEPTH {
            if self.visframe_of(node) != self.visframecount {
                return None;
            }
            if node < 0 {
                let leaf = world.leafs.get((-1 - node) as usize)?;
                return (leaf.contents != CONTENTS_SOLID).then_some(node);
            }
            let n = world.nodes.get(node as usize)?;
            let p = usize::try_from(n.planenum).ok().and_then(|i| world.planes.get(i))?;
            // BOX_ON_PLANE_SIDE: the axial shortcut, else BoxOnPlaneSide.
            let sides = if (0..3).contains(&p.ptype) {
                let t = p.ptype as usize;
                if p.dist <= emins[t] {
                    1
                } else if p.dist >= emaxs[t] {
                    2
                } else {
                    3
                }
            } else {
                crate::math::box_on_plane_side(emins, emaxs, &crate::math::Plane::new(p.normal, p.dist))
            };
            if sides == 3 {
                return Some(node); // remember first splitter
            }
            // not split yet; recurse down the contacted side
            node = child_ref(if sides & 1 != 0 { n.children[0] } else { n.children[1] });
        }
        None
    }

    /// `R_DrawBEntitiesOnList`'s brush half: every brush entity's faces into
    /// the edge list, clipped to the world's leaves when it spans several.
    fn draw_bentities(&mut self, world: &Bsp, ents: &[Ent]) {
        let oldorigin = self.modelorg;
        let (base_vpn, base_vright, base_vup) = (self.vpn, self.vright, self.vup);
        let world_root = world.models.first().and_then(|m| m.headnode.first().copied()).unwrap_or(0);
        self.insubmodel = true;
        for (ei, e) in ents.iter().enumerate().skip(1) {
            let m = &e.bsp.models[e.model];
            // R_BmodelCheckBBox / R_SplitEntityOnNode2 both use the model's
            // own (unrotated) bounding box translated by `origin`, exactly as
            // id does even for a rotated bmodel (a known looseness of id's
            // own check, not something to tighten here: see R_RotateBmodel's
            // call site in `r_main.c`, which rotates *after* this box test).
            let emins = [e.origin[0] + m.mins[0], e.origin[1] + m.mins[1], e.origin[2] + m.mins[2]];
            let emaxs = [e.origin[0] + m.maxs[0], e.origin[1] + m.maxs[1], e.origin[2] + m.maxs[2]];
            let minmaxs = [emins[0], emins[1], emins[2], emaxs[0], emaxs[1], emaxs[2]];
            let clipflags = self.bmodel_check_bbox(&minmaxs);
            if clipflags == BMODEL_FULLY_CLIPPED {
                continue;
            }
            self.currententity = ei as u32;
            self.modelorg = sub(self.r_origin, e.origin);
            // R_RotateBmodel: rotate modelorg and the view axes into the
            // entity's rest frame (identity when `e.rotation` is — every
            // entity in the shareware, so Classic takes the exact bytes the
            // pre-rotation code did).
            self.modelorg = world::entity_rotate(&e.rotation, self.modelorg);
            self.vpn = world::entity_rotate(&e.rotation, base_vpn);
            self.vright = world::entity_rotate(&e.rotation, base_vright);
            self.vup = world::entity_rotate(&e.rotation, base_vup);
            self.transform_frustum();
            let top = if !has_tree(world) { None } else { self.split_entity_on_node(world, world_root, emins, emaxs) };
            match top {
                // Not a leaf: clipped to the world BSP.
                Some(node) if node >= 0 => {
                    self.r_clipflags = clipflags;
                    self.draw_solid_clipped_submodel_polygons(world, e, node);
                }
                // In one leaf: whole faces, sorted on 1/z against the leaf's others.
                Some(leaf) => self.draw_submodel_polygons(e, clipflags, self.leaf_key[(-1 - leaf) as usize]),
                // No world tree (synthetic rooms): whole faces, sorted on 1/z
                // with the world's (`render_world_flat`).
                None if !has_tree(world) => self.draw_submodel_polygons(e, clipflags, 0),
                None => {}
            }
            // put back world rotation and frustum clipping
            self.modelorg = oldorigin;
            self.vpn = base_vpn;
            self.vright = base_vright;
            self.vup = base_vup;
            self.transform_frustum();
        }
        self.insubmodel = false;
        self.currententity = 0;
    }

    /// The model's front faces (`psurf` loop with `BACKFACE_EPSILON`).
    fn front_faces<'b>(&self, e: &'b Ent) -> impl Iterator<Item = usize> + 'b {
        let m = &e.bsp.models[e.model];
        let f0 = m.firstface.max(0) as usize;
        let end = (f0 + m.numfaces.max(0) as usize).min(e.bsp.faces.len());
        let modelorg = self.modelorg;
        (f0..end).filter(move |&fi| {
            let face = &e.bsp.faces[fi];
            let Some(plane) = usize::try_from(face.planenum).ok().and_then(|p| e.bsp.planes.get(p)) else {
                return false;
            };
            let d = (dot(modelorg, plane.normal) - plane.dist) as f64;
            if face.side != 0 { d < -BACKFACE_EPSILON } else { d > BACKFACE_EPSILON }
        })
    }

    /// `R_DrawSubmodelPolygons`: a model in one leaf, every front face keyed
    /// as that leaf (`R_RenderFace` with `insubmodel`).
    fn draw_submodel_polygons(&mut self, e: &Ent, clipflags: u32, key: i32) {
        let faces: Vec<usize> = self.front_faces(e).collect();
        for fi in faces {
            self.r_currentkey = key;
            self.render_face(e.bsp, fi, clipflags);
        }
    }

    /// `R_DrawSolidClippedSubmodelPolygons`: each front face's edges, clipped
    /// down the world BSP from `topnode` into its leaves.
    fn draw_solid_clipped_submodel_polygons(&mut self, world: &Bsp, e: &Ent, topnode: i32) {
        let faces: Vec<usize> = self.front_faces(e).collect();
        for fi in faces {
            let face = &e.bsp.faces[fi];
            // copy the edges to bedges, flipping if necessary so always
            // clockwise winding
            self.bedges.clear();
            let n = face.numedges.max(0) as usize;
            let mut ok = n > 0;
            for j in 0..n {
                let se = usize::try_from(face.firstedge).ok().and_then(|f| e.bsp.surfedges.get(f + j));
                let Some(&lindex) = se else {
                    ok = false;
                    break;
                };
                let (ei, a, b) = if lindex > 0 { (lindex as usize, 0, 1) } else { ((-(lindex as i64)) as usize, 1, 0) };
                let verts = e
                    .bsp
                    .edges
                    .get(ei)
                    .and_then(|m| Some((Self::vertex(e.bsp, m.v[a])?, Self::vertex(e.bsp, m.v[b])?)));
                let Some((v0, v1)) = verts else {
                    ok = false;
                    break;
                };
                let next = if j + 1 < n { j as u32 + 1 } else { NONE };
                self.bedges.push(BEdge { v: [v0, v1], pnext: next });
            }
            if ok {
                self.recursive_clip_bpoly(world, e.bsp, e.origin, e.rotation, 0, topnode, fi, 0);
            }
        }
    }

    /// `R_RecursiveClipBPoly`: split the edge list `pedges` by `node`'s plane
    /// (in the model's frame), close each side along the plane, and send each
    /// side down its child — to `R_RenderBmodelFace` at a non-solid leaf in the
    /// PVS, keyed as that leaf.
    #[allow(clippy::too_many_arguments)]
    fn recursive_clip_bpoly(
        &mut self,
        world: &Bsp,
        model: &Bsp,
        entorigin: Vec3,
        rotation: [[f32; 3]; 3],
        pedges: u32,
        node: i32,
        fi: usize,
        depth: u32,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(n) = world.nodes.get(node as usize) else { return };
        let Some(splitplane) = usize::try_from(n.planenum).ok().and_then(|p| world.planes.get(p)) else { return };
        let mut psideedges = [NONE, NONE];
        let mut makeclippededge = false;
        // transform the BSP plane into model space: the world-space split
        // plane's normal rotates the same way `modelorg` and the view axes
        // did (R_RotateBmodel); its distance only needs the translation
        // (`entorigin`), since that's taken before the rotation (a plane's
        // distance from the entity's own origin doesn't change by turning
        // the entity in place around that same origin).
        let tdist = splitplane.dist - dot(entorigin, splitplane.normal);
        let tnormal = world::entity_rotate(&rotation, splitplane.normal);
        // clip edges to BSP plane
        let mut p = pedges;
        while p != NONE {
            let pe = self.bedges[p as usize];
            let pnextedge = pe.pnext;
            let (plastvert, pvert) = (pe.v[0], pe.v[1]);
            let lastdist = dot(plastvert, tnormal) - tdist;
            let lastside = if lastdist > 0.0 { 0 } else { 1 };
            let dist = dot(pvert, tnormal) - tdist;
            let side = if dist > 0.0 { 0 } else { 1 };
            if side != lastside {
                // clipped: generate the clipped vertex, split into two edges
                let frac = lastdist / (lastdist - dist);
                let ptvert = [
                    plastvert[0] + frac * (pvert[0] - plastvert[0]),
                    plastvert[1] + frac * (pvert[1] - plastvert[1]),
                    plastvert[2] + frac * (pvert[2] - plastvert[2]),
                ];
                let e1 = self.bedges.len() as u32;
                self.bedges.push(BEdge { v: [plastvert, ptvert], pnext: psideedges[lastside] });
                psideedges[lastside] = e1;
                self.bedges.push(BEdge { v: [ptvert, pvert], pnext: psideedges[side] });
                psideedges[side] = e1 + 1;
                if side == 0 {
                    // entering for front, exiting for back
                    self.pfrontenter = ptvert;
                } else {
                    self.pfrontexit = ptvert;
                }
                makeclippededge = true;
            } else {
                // add the edge to the appropriate side
                self.bedges[p as usize].pnext = psideedges[side];
                psideedges[side] = p;
            }
            p = pnextedge;
        }
        // if anything was clipped, reconstitute and add the edges along the
        // clip plane to both sides (but in opposite directions)
        if makeclippededge {
            let e1 = self.bedges.len() as u32;
            self.bedges.push(BEdge { v: [self.pfrontexit, self.pfrontenter], pnext: psideedges[0] });
            psideedges[0] = e1;
            self.bedges.push(BEdge { v: [self.pfrontenter, self.pfrontexit], pnext: psideedges[1] });
            psideedges[1] = e1 + 1;
        }
        // draw or recurse further
        for (i, &edges) in psideedges.iter().enumerate() {
            if edges == NONE {
                continue;
            }
            let pn = child_ref(n.children[i]);
            if self.visframe_of(pn) != self.visframecount {
                continue; // not in the PVS
            }
            if pn < 0 {
                let li = (-1 - pn) as usize;
                if world.leafs.get(li).is_some_and(|l| l.contents != CONTENTS_SOLID) {
                    self.r_currentbkey = self.leaf_key[li];
                    self.render_bmodel_face(model, edges, fi);
                }
            } else {
                self.recursive_clip_bpoly(world, model, entorigin, rotation, edges, pn, fi, depth + 1);
            }
        }
    }

    /// `R_RenderBmodelFace`: a brush-model fragment's edges (a `bedge_t` list)
    /// through the clip planes; its surface keyed as its leaf. (A face with a
    /// bad plane index never gets here: [`Self::front_faces`] drops it.)
    fn render_bmodel_face(&mut self, bsp: &Bsp, pedges: u32, fi: usize) {
        self.r_pedge_owner = NONE; // the dummy `tedge`
        let (chain, nchain) = Self::clip_chain(self.r_clipflags);
        let chain = &chain[..nchain];
        self.r_emitted = false;
        self.r_nearzi = 0.0;
        self.r_nearzionly = false;
        self.makeleftedge = false;
        self.makerightedge = false;
        self.r_lastvertvalid = false;
        let mut p = pedges;
        while p != NONE {
            let e = self.bedges[p as usize];
            self.r_leftclipped = false;
            self.r_rightclipped = false;
            self.clip_edge(e.v[0], e.v[1], chain);
            self.makeleftedge |= self.r_leftclipped;
            self.makerightedge |= self.r_rightclipped;
            p = e.pnext;
        }
        self.finish_face(chain);
        if !self.r_emitted {
            return;
        }
        self.post_surface(bsp, fi, self.r_currentbkey, true);
    }

    // -----------------------------------------------------------------------
    // r_edge.c: the scan
    // -----------------------------------------------------------------------

    /// `R_ScanEdges` (without its span-pool flush: the pool grows): every
    /// scanline's spans, from the edges `newedges` holds.
    ///
    /// id walks the active edges three times a scanline: `R_GenerateSpans`,
    /// then `R_RemoveEdges` (those that end on it), then `R_StepActiveU`
    /// (step each to the next scanline and move back any that passed the
    /// one before it). Here it is one walk: an edge is removed or stepped
    /// ([`EdgeState::step_edge`]) as soon as its spans are generated. The
    /// spans come out the same: an edge's spans read its own `u` and the
    /// surfaces, never another edge's `u`, and the walk goes on from the
    /// edge that followed this one before it was stepped. The table comes
    /// out the same too. Stepping an edge only looks at, and moves it
    /// among, the edges before it, which id's third walk had stepped by
    /// then and this one has too; the edges before it that end here are
    /// already out, as id's second walk had them (taking a set of edges out
    /// of a list leaves the same list in any order); and what is left of
    /// the scanline's own walk lies after it, untouched.
    ///
    /// One thing id's third walk does and this one does not: it steps
    /// `edge_tail` as well, and would move the tail back before an edge
    /// that had run past it. None does. `R_EmitEdge`'s clamps start every
    /// edge half a pixel or more left of the tail — 2^19 of `u`'s units —
    /// and an edge's steps carry it a few thousand units past its clamp at
    /// the most, so the tail stays last in id's walk as it does here. (The
    /// tests hold this walk to id's three, written out, line by line over
    /// random tables and the maps' own; the scan is the largest thing a
    /// frame does on one thread: PERF_PLAN.md §14.)
    fn scan_edges(&mut self) {
        self.begin_scan();
        let bottom = self.h as i32 - 1;
        for iv in 0..bottom {
            self.scan_line(iv, true);
        }
        // the last scan (no need to step or sort or remove on the last scan)
        self.scan_line(bottom, false);
        self.row_spans.push(self.spans.len() as u32);
    }

    /// `R_ScanEdges`' sentinels: the active edges cleared to just the
    /// background edges around the screen.
    fn begin_scan(&mut self) {
        let head_u = 0i64;
        self.edges[EDGE_HEAD as usize] =
            Edge { u: head_u, u_step: 0, prev: NONE, next: EDGE_TAIL, surfs: [0, BACKGROUND], ..Edge::ZERO };
        self.edge_head_u_shift20 = (head_u >> 20) as i32;
        let tail_u = ((self.w as i64) << 20) + 0xFFFFF;
        self.edges[EDGE_TAIL as usize] =
            Edge { u: tail_u, u_step: 0, prev: EDGE_HEAD, next: EDGE_AFTERTAIL, surfs: [BACKGROUND, 0], ..Edge::ZERO };
        self.edge_tail_u_shift20 = (tail_u >> 20) as i32;
        // force a move
        self.edges[EDGE_AFTERTAIL as usize] =
            Edge { u: -1, u_step: 0, prev: EDGE_TAIL, next: EDGE_SENTINEL, ..Edge::ZERO };
        // "make sure nothing sorts past this": id's `2000 << 24` (which wraps
        // its int negative; neither value is ever compared, the tail stops
        // every walk first)
        self.edges[EDGE_SENTINEL as usize] = Edge { u: i64::MAX, u_step: 0, prev: EDGE_AFTERTAIL, ..Edge::ZERO };
    }

    /// One scanline of `R_ScanEdges`: add the new edges, generate the spans
    /// and, with `step`, leave the active edges as the next scanline's.
    fn scan_line(&mut self, iv: i32, step: bool) {
        self.current_iv = iv;
        self.fv = iv as f32;
        self.row_spans.push(self.spans.len() as u32);
        // mark that the head (background start) span is pre-included
        self.surfs[BACKGROUND as usize].spanstate = 1;
        let ne = self.newedges[iv as usize];
        if ne != NONE {
            let first = self.edges[EDGE_HEAD as usize].next;
            self.insert_new_edges(ne, first);
        }
        self.generate_spans(step);
    }

    /// `R_InsertNewEdges`: merge the u-sorted list `toadd` into the active
    /// edge table from `edgelist`.
    fn insert_new_edges(&mut self, mut toadd: u32, mut edgelist: u32) {
        while toadd != NONE {
            let next_edge = self.edges[toadd as usize].next;
            let u = self.edges[toadd as usize].u;
            while self.edges[edgelist as usize].u < u {
                edgelist = self.edges[edgelist as usize].next;
            }
            // insert toadd before edgelist
            let prev = self.edges[edgelist as usize].prev;
            self.edges[toadd as usize].next = edgelist;
            self.edges[toadd as usize].prev = prev;
            self.edges[prev as usize].next = toadd;
            self.edges[edgelist as usize].prev = toadd;
            toadd = next_edge;
        }
    }

    /// `R_RemoveEdges` and `R_StepActiveU` for one active edge, its spans on
    /// the current scanline generated: out of the table if this is its last
    /// scanline; else stepped to the next one, and moved back if it passed
    /// the edge before it.
    #[inline]
    fn step_edge(&mut self, pedge: u32) {
        let e = &mut self.edges[pedge as usize];
        let (prev, next) = (e.prev, e.next);
        if e.last == self.current_iv {
            self.edges[next as usize].prev = prev;
            self.edges[prev as usize].next = next;
            return;
        }
        e.u = e.u.wrapping_add(e.u_step);
        let u = e.u;
        if u >= self.edges[prev as usize].u {
            return;
        }
        // push it back to keep it sorted: pull the edge out of the edge list
        self.edges[next as usize].prev = prev;
        self.edges[prev as usize].next = next;
        // find out where the edge goes in the edge list (id would walk
        // past `edge_head` for an edge left of the screen, which its
        // clamps never make; stop there)
        let mut pwedge = self.edges[prev as usize].prev;
        if pwedge == NONE {
            pwedge = EDGE_HEAD;
        }
        while pwedge != EDGE_HEAD && self.edges[pwedge as usize].u > u {
            pwedge = self.edges[pwedge as usize].prev;
        }
        // put the edge back into the edge list
        let after = self.edges[pwedge as usize].next;
        self.edges[pedge as usize].next = after;
        self.edges[pedge as usize].prev = pwedge;
        self.edges[after as usize].prev = pedge;
        self.edges[pwedge as usize].next = pedge;
    }

    /// `R_ScanEdges` as id walks it — the spans, then `R_RemoveEdges`, then
    /// `R_StepActiveU`, three walks a scanline: what [`EdgeState::scan_edges`]
    /// is held to.
    #[cfg(test)]
    fn scan_edges_in_ids_walks(&mut self) {
        self.begin_scan();
        let bottom = self.h as i32 - 1;
        for iv in 0..bottom {
            self.scan_line(iv, false);
            // R_RemoveEdges (id's list of them is the `last` of each here).
            let mut pedge = self.edges[EDGE_HEAD as usize].next;
            while pedge != EDGE_TAIL {
                let Edge { prev, next, last, .. } = self.edges[pedge as usize];
                if last == iv {
                    self.edges[next as usize].prev = prev;
                    self.edges[prev as usize].next = next;
                }
                pedge = next;
            }
            let first = self.edges[EDGE_HEAD as usize].next;
            if first != EDGE_TAIL {
                self.step_active_u(first);
            }
        }
        self.scan_line(bottom, false);
        self.row_spans.push(self.spans.len() as u32);
    }

    /// `R_StepActiveU`: step every active edge to the next scanline, moving
    /// back any that passed the one before it.
    #[cfg(test)]
    fn step_active_u(&mut self, mut pedge: u32) {
        let mut budget = self.edges.len() * 2 + 8;
        loop {
            if budget == 0 {
                return;
            }
            budget -= 1;
            let e = &mut self.edges[pedge as usize];
            e.u = e.u.wrapping_add(e.u_step);
            let (u, prev) = (e.u, e.prev);
            if u >= self.edges[prev as usize].u {
                pedge = self.edges[pedge as usize].next;
                continue;
            }
            // pushback:
            if pedge == EDGE_AFTERTAIL {
                return;
            }
            // push it back to keep it sorted
            let pnext_edge = self.edges[pedge as usize].next;
            // pull the edge out of the edge list
            let next = pnext_edge;
            self.edges[next as usize].prev = prev;
            self.edges[prev as usize].next = next;
            let mut pwedge = self.edges[prev as usize].prev;
            if pwedge == NONE {
                pwedge = EDGE_HEAD;
            }
            while pwedge != EDGE_HEAD && self.edges[pwedge as usize].u > u {
                pwedge = self.edges[pwedge as usize].prev;
            }
            // put the edge back into the edge list
            let after = self.edges[pwedge as usize].next;
            self.edges[pedge as usize].next = after;
            self.edges[pedge as usize].prev = pwedge;
            self.edges[after as usize].prev = pedge;
            self.edges[pwedge as usize].next = pedge;
            pedge = pnext_edge;
            if pedge == EDGE_TAIL {
                return;
            }
        }
    }

    /// Add a span of `surf`: `count` pixels from `u` on the current scanline.
    #[inline]
    fn emit_span(&mut self, surf: u32, u: i32, count: i32) {
        self.surfs[surf as usize].has_spans = true;
        self.spans.push(ESpan { u, count, surf });
    }

    /// `R_GenerateSpans`: walk the active edges left to right, keeping the
    /// stack of surfaces under the pixel, and emit a span wherever the top
    /// changes. With `step`, each edge is then removed or stepped for the
    /// next scanline ([`EdgeState::step_edge`]), in the same walk.
    fn generate_spans(&mut self, step: bool) {
        // clear active surfaces to just the background surface
        let bg = &mut self.surfs[BACKGROUND as usize];
        bg.next = BACKGROUND;
        bg.prev = BACKGROUND;
        bg.last_u = self.edge_head_u_shift20;
        let mut edge = self.edges[EDGE_HEAD as usize].next;
        while edge != EDGE_TAIL {
            let Edge { surfs, next, .. } = self.edges[edge as usize];
            if surfs[0] != 0 {
                // it has a left surface, so a surface is going away for this span
                self.trailing_edge(surfs[0], edge);
            }
            if surfs[0] == 0 || surfs[1] != 0 {
                self.leading_edge(edge);
            }
            if step {
                self.step_edge(edge);
            }
            edge = next;
        }
        self.cleanup_span();
    }

    /// `R_CleanupSpan`: at the right edge of the screen, emit a span for
    /// whatever is on top and reset the stack's span states.
    fn cleanup_span(&mut self) {
        let surf = self.surfs[BACKGROUND as usize].next;
        let iu = self.edge_tail_u_shift20;
        let last_u = self.surfs[surf as usize].last_u;
        if iu > last_u {
            self.emit_span(surf, last_u, iu - last_u);
        }
        let mut s = surf;
        let mut budget = self.surfs.len() + 1;
        loop {
            self.surfs[s as usize].spanstate = 0;
            s = self.surfs[s as usize].next;
            budget -= 1;
            if s == BACKGROUND || budget == 0 {
                break;
            }
        }
    }

    /// `R_TrailingEdge`: `surf` ends at `edge`.
    fn trailing_edge(&mut self, surf: u32, edge: u32) {
        let s = &mut self.surfs[surf as usize];
        s.spanstate -= 1;
        // don't generate a span if this is an inverted span, with the end edge
        // preceding the start edge (that is, we haven't seen the start edge yet)
        if s.spanstate != 0 {
            return;
        }
        if surf == self.surfs[BACKGROUND as usize].next {
            // emit a span (current top going away)
            let iu = (self.edges[edge as usize].u >> 20) as i32;
            let last_u = self.surfs[surf as usize].last_u;
            if iu > last_u {
                self.emit_span(surf, last_u, iu - last_u);
            }
            // set last_u on the surface below
            let below = self.surfs[surf as usize].next;
            self.surfs[below as usize].last_u = iu;
        }
        let Surf { prev, next, .. } = self.surfs[surf as usize];
        self.surfs[prev as usize].next = next;
        self.surfs[next as usize].prev = prev;
    }

    /// `R_LeadingEdge` (`R_LeadingEdge`'s sort: by key, and brush models in
    /// the same leaf by their `1/z` at the edge).
    fn leading_edge(&mut self, edge: u32) {
        let surf = self.edges[edge as usize].surfs[1];
        if surf == 0 {
            return;
        }
        // it's adding a new surface in, so find the correct place
        self.surfs[surf as usize].spanstate += 1;
        // don't start a span if this is an inverted span, with the end edge
        // preceding the start edge (that is, we've already seen the end edge)
        if self.surfs[surf as usize].spanstate != 1 {
            return;
        }
        let s = self.surfs[surf as usize];
        let edge_u = self.edges[edge as usize].u;
        // 1/z at the edge, for two brush models in the same leaf
        let fu = (edge_u.wrapping_sub(0xFFFFF) as f32) as f64 * (1.0 / 1_048_576.0);
        let fv = self.fv;
        let zi_at = |t: &Surf| (t.d_ziorigin + fv * t.d_zistepv) as f64 + fu * t.d_zistepu as f64;
        // `true` when `s` sorts in front of `t`, which has the same key.
        let in_front = |t: &Surf| {
            let newzi = zi_at(&s);
            let testzi = zi_at(t);
            if newzi * 0.99 >= testzi {
                return true;
            }
            newzi * 1.01 >= testzi && s.d_zistepu >= t.d_zistepu
        };
        let mut surf2 = self.surfs[BACKGROUND as usize].next;
        let mut newtop = false;
        let s2 = self.surfs[surf2 as usize];
        if s.key < s2.key || (s.insubmodel && s.key == s2.key && in_front(&s2)) {
            newtop = true;
        } else {
            // continue_search
            let mut budget = self.surfs.len() + 1;
            loop {
                loop {
                    surf2 = self.surfs[surf2 as usize].next;
                    budget -= 1;
                    if s.key <= self.surfs[surf2 as usize].key || budget == 0 {
                        break;
                    }
                }
                let t = self.surfs[surf2 as usize];
                if s.key == t.key && budget > 0 {
                    // two surfaces on the same plane: the one already active is
                    // in front, unless they are brush models, sorted on 1/z
                    if s.insubmodel && in_front(&t) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        if newtop {
            // emit a span (obscures current top)
            let iu = (edge_u >> 20) as i32;
            let last_u = self.surfs[surf2 as usize].last_u;
            if iu > last_u {
                self.emit_span(surf2, last_u, iu - last_u);
            }
            // set last_u on the new span
            self.surfs[surf as usize].last_u = iu;
        }
        // gotposition: insert before surf2
        let prev = self.surfs[surf2 as usize].prev;
        self.surfs[surf as usize].next = surf2;
        self.surfs[surf as usize].prev = prev;
        self.surfs[prev as usize].next = surf;
        self.surfs[surf2 as usize].prev = surf;
    }

    // -----------------------------------------------------------------------
    // d_edge.c: the surfaces, span by span
    // -----------------------------------------------------------------------

    /// `D_DrawSurfaces`' per-surface setup: for each surface that owns a
    /// span, how its spans are painted and its `1/z` plane, the surface cache
    /// consulted (`D_CacheSurface`) and a block it does not have added to the
    /// frame's bakes, `jobs` — everything the bands need, decided once.
    #[allow(clippy::too_many_arguments)]
    fn prepare_surfaces<'a>(
        &mut self,
        frame: &Frame<'_, 'a>,
        caches: &mut SurfaceCaches,
        jobs: &mut Vec<BakeJob<'a>>,
        prof: &mut Profiler,
        ents: &[Ent<'a>],
        bits: &[u32],
    ) -> WorldDraw<'a> {
        let (cam, opts) = (&frame.cam, &frame.scene.options);
        let Projection { cx, cy, xscale, yscale } = Projection::new(cam, &frame.geom, opts.aspect());
        let (vpn, vright, vup) = (self.vpn, self.vright, self.vup);
        let sview = ScreenProj { forward: vpn, right: vright, up: vup, cx, cy, xscale, yscale };
        let mipview = MipView::new(xscale, yscale, opts.mip);
        // The dome's scale is the projected view's (a window's is the view it
        // opens onto, so its sky meets the view's).
        let (proj_w, proj_h) = (frame.geom.proj_w, frame.geom.proj_h);
        let sky = SkyView::new(
            vpn,
            vright,
            vup,
            sky_dome_scale(proj_w, proj_h, frame.scr_fov(), cam.fov_deg),
            opts.sky_centre(&frame.geom),
            frame.scene.time,
            opts.video.sky,
        );
        let sky_tex = sky_texture(ents[0].bsp);
        let (light_dir, _) = normalize([0.3, 0.5, 1.0]);
        let clear = R_CLEARCOLOR;
        let pass = FacePass { frame, sview: &sview, mipview: &mipview, ents, bits, light_dir, clear };
        let mut faces = 0u64;
        let mut surfs = Vec::with_capacity(self.surfs.len());
        let t_lookup = prof.now();
        for si in 0..self.surfs.len() {
            let s = self.surfs[si];
            if si == 0 || !s.has_spans {
                surfs.push(None);
                continue;
            }
            let background = s.flags & SURF_DRAWBACKGROUND != 0;
            let (paint, zi) = if background {
                // the background: effectively at infinity
                (Paint::Fill(clear), [BACKGROUND_ZI, 0.0, 0.0])
            } else {
                faces += 1;
                let paint = if s.flags & SURF_DRAWSKY != 0 {
                    sky_tex.map_or(Paint::Fill(clear), Paint::Sky)
                } else {
                    self.prepare_face(&s, &pass, caches, jobs, prof)
                };
                (paint, [s.d_ziorigin, s.d_zistepu, s.d_zistepv])
            };
            // D_DrawZSpans' step along a row, the plane's for every span.
            let izistep = c_ftoi((zi[1] * 32768.0 * 65536.0) as f64);
            surfs.push(Some(SurfDraw { paint, zi, izistep, background }));
        }
        prof.add(|st| st.faces_drawn += faces);
        if let Some(t) = t_lookup {
            prof.add(|st| st.surf_lookup_ns += t.elapsed().as_nanos() as u64);
        }
        WorldDraw {
            surfs,
            spans: std::mem::take(&mut self.spans),
            rows: std::mem::take(&mut self.row_spans),
            w: self.w,
            sky,
            persp: opts.persp_span,
        }
    }

    /// How one wall or liquid surface is painted (`D_DrawSurfaces`' turbulent
    /// and cached branches, and the port's fallbacks for textureless or unlit
    /// faces), its lightmap built and its block found in the cache — or its
    /// bake added to `jobs`, the paint naming the job, whose block the
    /// frame's [`Bakes`] has when the spans are drawn.
    fn prepare_face<'a>(
        &mut self,
        s: &Surf,
        pass: &FacePass<'_, '_, 'a>,
        caches: &mut SurfaceCaches,
        jobs: &mut Vec<BakeJob<'a>>,
        prof: &mut Profiler,
    ) -> Paint<'a> {
        let FacePass { frame, sview, mipview, ents, bits, light_dir, clear } = *pass;
        let scene = frame.scene;
        let (light_styles, colormap, time) = (scene.light_styles, scene.colormap, scene.time);
        let e = &ents[s.ent as usize];
        let bsp = e.bsp;
        let fi = s.face as usize;
        let face = &bsp.faces[fi];
        let ti: Option<&TexInfo> = usize::try_from(face.texinfo).ok().and_then(|i| bsp.texinfo.get(i));
        // R_TextureAnimation: the frame drawn, with the entity's alternate cycle.
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            let anim_mi = texture_animation(bsp, mi, e.frame, time);
            bsp.textures.get(anim_mi).and_then(|o| o.as_ref()).map(|mt| (anim_mi, mt))
        });
        // The face's gradients from the eye in the model's frame: `D_CalcGradients`'
        // own R_RotateBmodel re-do, by rotating the eye and the (shared, world)
        // view axes into this entity's rest frame together — identity for the
        // world and for an unrotated bmodel (`e.rotation` is then exactly
        // `IDENTITY_ROTATION`, so this is byte-for-byte the plain translation
        // below it used to be), id's own rotated-door math otherwise.
        let eye = world::entity_rotate(&e.rotation, sub(frame.cam.pos, e.origin));
        let local_sview = ScreenProj {
            forward: world::entity_rotate(&e.rotation, sview.forward),
            right: world::entity_rotate(&e.rotation, sview.right),
            up: world::entity_rotate(&e.rotation, sview.up),
            ..*sview
        };
        let Some(grads) = face_grads(bsp, face, &local_sview, eye, ti) else {
            return Paint::Fill(clear);
        };
        let face_bits = if e.world_bsp { bits.get(fi).copied().unwrap_or(0) } else { 0 };
        // The steady torches lighting it (`r_torchflicker`): the world's
        // faces and its brush models', lit in place as LIGHT.EXE lit them.
        let torches = match frame.torches {
            Some(t) if e.world_bsp => t.face(fi),
            _ => FaceTorches::NONE,
        };
        // The port's own flat-shading fallback (id has none: an unlit face is
        // simply fullbright). `light_dir` is a world-space constant, so a
        // rotated entity's rest-frame normal has to make the same trip back
        // out to world space the eye and the view axes made in — the inverse
        // rotation (identity for the world/an unrotated bmodel, same as above).
        let normal = super::surf::face_normal(bsp, face).unwrap_or([0.0, 0.0, 1.0]);
        let normal = world::entity_rotate_transpose(&e.rotation, normal);
        let shade = (0.5 + 0.5 * dot(normal, light_dir).max(0.0)).min(1.0);
        let turbulent = s.flags & SURF_DRAWTURB != 0;
        // Only walls are lightmapped (sky and liquids are TEX_SPECIAL).
        let lightmap: Option<LightMap> = if turbulent {
            None
        } else if s.ent == 0 {
            caches.world_lightmap(bsp, fi, face, light_styles, torches, e.dlights, face_bits)
        } else if face_world_poly(bsp, face, &mut self.poly) {
            prof.add(|st| st.sub_lm_builds += 1);
            face_lightmap_with(bsp, face, &self.poly, light_styles, torches, e.dlights, face_bits)
        } else {
            None
        };
        match tex {
            Some((tex_index, mt)) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                if turbulent {
                    return Paint::Turb { grads, mt };
                }
                let found = match (lightmap, colormap) {
                    (Some(lightmap), Some(colormap)) => caches.surface(
                        SurfaceRequest {
                            slot: e.world_bsp.then_some(fi),
                            face,
                            texture: tex_index,
                            mt,
                            lightmap,
                            colormap,
                            light_styles,
                            torches,
                            dlit: any_dlight_reaches(bsp, face, e.dlights, face_bits),
                            // D_MipLevelForScale on the surface's nearest 1/z
                            mip: ti.map_or(0, |t| mipview.level_for_nearzi(s.nearzi, t)),
                        },
                        jobs,
                        prof,
                    ),
                    (lightmap, _) => Surface::PerPixel(lightmap),
                };
                match found {
                    Surface::Block(block, job) => {
                        prof.add(|st| st.surf_hits += 1);
                        let grads = grads.mip_scaled(block.mip);
                        let fixed = BlockFixed::new(&grads, block.texmins, block.bw, block.bh);
                        Paint::Cached { grads, fixed, block, job }
                    }
                    Surface::PerPixel(lightmap) => {
                        prof.add(|st| st.surf_misses += 1);
                        Paint::Texels { grads, texture: Some(mt), shade, lightmap }
                    }
                }
            }
            _ => {
                // The port's flat colour for a face without a texture (test
                // maps): a palette index hashed from the texture, as a 1x1
                // texture lit by the lightmap when there is one.
                let key = ti.map(|t| t.miptex as i64).unwrap_or(face.texinfo as i64);
                let colour = hash_index(key);
                match lightmap {
                    Some(lm) => Paint::Flat { grads, colour, shade, lightmap: lm },
                    None => Paint::Fill(shade_index(scene.palette, colour, shade)),
                }
            }
        }
    }
}

impl WorldDraw<'_> {
    /// `D_DrawSurfaces` and `D_DrawZSpans` for the rows of `band`: the spans
    /// of those rows, each painted as [`EdgeState::build`] decided for its
    /// surface — a block the frame bakes from `bakes` — and their `1/z`.
    /// Returns the pixels drawn (the background's not counted).
    pub(super) fn draw_band(&self, band: &mut Band, frame: &Frame, bakes: &Bakes) -> u64 {
        let w = self.w as i32;
        let scene = frame.scene;
        let (palette, colormap, persp) = (scene.palette, scene.colormap, self.persp);
        let mut drawn = 0u64;
        for v in band.rows() {
            let (Some(&first), Some(&end)) = (self.rows.get(v), self.rows.get(v + 1)) else { break };
            for sp in self.spans.get(first as usize..end as usize).unwrap_or(&[]) {
                let Some(Some(SurfDraw { paint, zi: [ziorigin, zistepu, zistepv], izistep, background })) =
                    self.surfs.get(sp.surf as usize)
                else {
                    continue;
                };
                // clamp to the row (id's stepping keeps it there)
                let u = sp.u.clamp(0, w) as usize;
                let n = ((sp.u + sp.count).clamp(0, w) as usize).saturating_sub(u);
                let Some((row, zrow)) = band.span(u, v, n).filter(|_| n > 0) else { continue };
                match paint {
                    Paint::Fill(c) => row.fill(*c),
                    Paint::Sky(mt) => {
                        draw_sky_span(row, u as i32, v as i32, n as i32, &mt.pixels, mt.width as usize, &self.sky);
                    }
                    Paint::Turb { grads, mt } => {
                        let (tw, th) = (mt.width as usize, mt.height as usize);
                        span_turb(
                            row,
                            &span_at(grads, u, v),
                            grads,
                            &mt.pixels,
                            tw,
                            th,
                            &frame.turb,
                            scene.time,
                            persp,
                        );
                    }
                    Paint::Cached { grads, fixed, block, job } => {
                        let texels = job.map_or(&block.block[..], |job| bakes.block(job));
                        span_cached(row, &span_at(grads, u, v), fixed, texels, block.bw, block.bh, persp);
                    }
                    Paint::Texels { grads, texture, shade, lightmap } => {
                        let (pixels, tw, th) = texture
                            .map_or((&[][..], 0, 0), |mt| (&mt.pixels[..], mt.width as usize, mt.height as usize));
                        span_tex(
                            row,
                            &span_at(grads, u, v),
                            grads,
                            pixels,
                            tw,
                            th,
                            palette,
                            *shade,
                            lightmap.as_ref(),
                            colormap,
                        );
                    }
                    Paint::Flat { grads, colour, shade, lightmap } => {
                        span_tex(
                            row,
                            &span_at(grads, u, v),
                            grads,
                            std::slice::from_ref(colour),
                            1,
                            1,
                            palette,
                            *shade,
                            Some(lightmap),
                            colormap,
                        );
                    }
                }
                if !background {
                    drawn += n as u64;
                }
                // D_DrawZSpans. The step is copied out of the surface first:
                // read through its reference, it is read again after every
                // store (the compiler cannot tell the z row from it), and
                // the loop is not vectorized (the browser builds' SIMD).
                let zi = (ziorigin + v as f32 * zistepv + u as f32 * zistepu) as f64;
                let (mut izi, izistep) = (c_ftoi(zi * 32768.0 * 65536.0), *izistep);
                for z in zrow {
                    *z = (izi >> 16) as i16;
                    izi = izi.wrapping_add(izistep);
                }
            }
        }
        drawn
    }
}

/// How `D_DrawSurfaces` paints one surface's spans, decided once a frame
/// ([`EdgeState::build`]) so that a band only reads.
enum Paint<'a> {
    /// One colour: the background (`r_clearcolor`), a face seen edge-on, a
    /// sky with no sky texture, or the port's flat colour for a textureless
    /// face with no lightmap.
    Fill(u8),
    /// The two-layer sky (`D_DrawSkyScans8`).
    Sky(&'a MipTex),
    /// A liquid (`Turbulent8`), the raw texel.
    Turb { grads: PolyGrads, mt: &'a MipTex },
    /// A wall from its lit surface-cache block (`D_DrawSpans16`); the
    /// gradients are the block's mip level's. With `job`, the block is one
    /// the frame bakes ([`Bakes::block`]), and `block` has only its shape.
    Cached { grads: PolyGrads, fixed: BlockFixed, block: SurfBlock, job: Option<usize> },
    /// A wall with no block, lit per pixel (no colormap, or a block past the
    /// size cap — never in id's maps).
    Texels { grads: PolyGrads, texture: Option<&'a MipTex>, shade: f32, lightmap: Option<LightMap<'a>> },
    /// The port's flat colour for a textureless face with a lightmap (test
    /// maps): a 1x1 texture of that colour, lit.
    Flat { grads: PolyGrads, colour: u8, shade: f32, lightmap: LightMap<'a> },
}

/// One surface ready for the bands: its paint, its `1/z` plane
/// (`d_ziorigin`, `d_zistepu`, `d_zistepv`) with `D_DrawZSpans`' step along
/// a row, and whether it is the background.
struct SurfDraw<'a> {
    paint: Paint<'a>,
    zi: [f32; 3],
    izistep: i32,
    background: bool,
}

/// The world as the bands draw it ([`EdgeState::build`]), the frame's own:
/// the scan's spans row after row, and the surfaces they name (indexed like
/// the edge state's, `None` for one without a span).
pub(super) struct WorldDraw<'a> {
    surfs: Vec<Option<SurfDraw<'a>>>,
    spans: Vec<ESpan>,
    /// The index in `spans` of each row's first span, and their count last.
    rows: Vec<u32>,
    /// The view's width.
    w: usize,
    sky: SkyView,
    persp: super::raster::PerspSpan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::cross;
    use crate::render::fixtures::render_once;
    use crate::render::{Camera, Image, Renderer, Scene, VideoCvars, demo_room, recycle_image};

    fn palette() -> [[u8; 3]; 256] {
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, (i * 7) as u8, (i * 13) as u8];
        }
        pal
    }

    fn render(cam: &Camera, w: usize, h: usize) -> Image {
        render_once(&Scene::new(&demo_room(), *cam, w, h, &palette()))
    }

    /// [`render`], and the z-buffer the frame left.
    fn render_z(cam: &Camera, w: usize, h: usize) -> (Image, Vec<i16>) {
        let mut r = Renderer::new();
        let img = r.render(&Scene::new(&demo_room(), *cam, w, h, &palette()));
        (img, r.zbuf()[..w * h].to_vec())
    }

    #[test]
    fn the_pillar_sorts_in_front_of_the_far_wall() {
        // A synthetic room without a node tree sorts its faces on 1/z at their
        // edges: straight at the pillar, its -X face covers the centre and the
        // far wall shows on either side of it on the same row, all at depths
        // D_DrawZSpans put there (pillar 168 units ahead, far wall 456).
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (160usize, 100usize);
        let (img, z) = render_z(&cam, w, h);
        let row = h / 2;
        let (centre, side) = (img.pixels[row * w + w / 2], img.pixels[row * w + w / 2 + 40]);
        assert_ne!(centre, side, "pillar and far wall are different surfaces");
        assert!(![centre, side].contains(&R_CLEARCOLOR), "both drawn");
        assert_eq!(z[row * w + w / 2], (32768.0f64 / 168.0) as i16);
        assert_eq!(z[row * w + w / 2 + 40], (32768.0f64 / 456.0) as i16);
    }

    #[test]
    fn every_pixel_is_drawn_so_the_frame_needs_no_clear() {
        // The spans cover the view: a recycled buffer full of garbage is
        // overwritten everywhere.
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let fresh = render(&cam, 96, 64);
        let garbage = (0..=255u8).find(|v| !fresh.pixels.contains(v)).expect("an index the frame does not use");
        recycle_image(Image { w: 96, h: 64, pixels: vec![garbage; 96 * 64] });
        let again = render(&cam, 96, 64);
        assert_eq!(fresh.pixels, again.pixels);
        assert!(!again.pixels.contains(&garbage));
    }

    #[test]
    fn the_background_is_r_clearcolor_at_infinity() {
        // Outside the room looking away from it: one background span per row,
        // palette[2], and D_DrawZSpans' -0.9 "at infinity" 1/z.
        let cam = Camera::looking_at([-400.0, 0.0, 0.0], [-800.0, 0.0, 0.0], 90.0);
        let (img, z) = render_z(&cam, 64, 40);
        assert!(img.pixels.iter().all(|&p| p == R_CLEARCOLOR));
        let bg = ((-0.9f32 as f64 * 32768.0 * 65536.0) as i32 >> 16) as i16;
        assert!(z.iter().all(|&v| v == bg));
    }

    #[test]
    fn the_zbuffer_holds_the_16_bit_1_over_z_of_the_nearest_surface() {
        // Straight at the pillar's -X face, 168 units ahead: `(int)(zi * 0x8000
        // * 0x10000) >> 16` = 32768/168 = 195 at the centre pixel.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let (_, z) = render_z(&cam, 64, 40);
        assert_eq!(z[20 * 64 + 32], (32768.0f64 / 168.0) as i16);
    }

    #[test]
    fn the_demo_room_is_wound_as_qbsp_winds_faces() {
        // Clockwise seen from the front — `R_EmitEdge` reads a face's leading
        // and trailing edges from that order, so a face wound the other way
        // makes only inverted spans and draws nothing.
        let bsp = demo_room();
        for f in &bsp.faces {
            let mut poly = Vec::new();
            assert!(face_world_poly(&bsp, f, &mut poly));
            let n = super::super::surf::face_normal(&bsp, f).expect("plane");
            let c = cross(sub(poly[1], poly[0]), sub(poly[2], poly[1]));
            assert!(dot(c, n) < 0.0, "face {f:?} is wound counter-clockwise");
        }
    }

    #[test]
    fn classic_views_are_at_most_id_maxwidth_by_maxheight() {
        // r_shared.h's MAXWIDTH x MAXHEIGHT (1280 x 1024): Classic clamps to it,
        // as id's drivers never set a larger mode.
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let wide = render(&cam, 2048, 400);
        assert_eq!((wide.w, wide.h), (1280, 400));
        assert_eq!(wide.pixels, render(&cam, 1280, 400).pixels, "drawn as id's widest mode");
        let tall = render(&cam, 320, 1100);
        assert_eq!((tall.w, tall.h), (320, 1024));
        // The edge renderer itself refuses only what no setting allows.
        let (w, h) = (crate::render::HIRES_MAXWIDTH + 8, 2);
        let (world, pal) = (demo_room(), palette());
        let scene = Scene::new(&world, cam, w, h, &pal);
        let mut edge = EdgeState::new();
        edge.begin_map(&world);
        let (mut caches, mut prof) = (SurfaceCaches::default(), Profiler::default());
        assert!(edge.build(&Frame::new(&scene, w, h), &mut caches, &mut Vec::new(), &mut prof).is_none());
    }

    #[test]
    fn hires_draws_past_2048_wide_where_id_u_wraps() {
        // With the hires extra the view is not clamped. From 2048 wide id's
        // 12.20 `int` u wraps (the right edge `(w << 20) + 0xFFFFF`; the scan
        // indexed past its edges, a release panic); the 44.20 u draws it. A
        // view 4x as wide and tall as another shows the same picture at 4x:
        // every 4x4 block of the big frame holds the small frame's pixel
        // there, but for the pixels along an edge between two surfaces.
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let (world, pal) = (demo_room(), palette());
        let options = crate::render::RenderOptions {
            video: VideoCvars { hires: true, ..VideoCvars::CLASSIC },
            ..Default::default()
        };
        let render = |w, h| render_once(&Scene { options, ..Scene::new(&world, cam, w, h, &pal) });
        let (w, h) = (640usize, 150usize);
        let small = render(w, h);
        let big = render(4 * w, 4 * h);
        assert_eq!((big.w, big.h), (2560, 600));
        assert!(!big.pixels.contains(&R_CLEARCOLOR), "the room covers the view: no background");
        let differ =
            (0..h * w).filter(|&i| big.pixels[(4 * (i / w) + 2) * 4 * w + 4 * (i % w) + 2] != small.pixels[i]).count();
        assert!(differ * 100 < w * h, "{differ} of {} pixels differ", w * h);
    }

    #[test]
    fn a_face_with_a_bad_plane_index_is_skipped() {
        // demo_room under one node whose two leaves see every face: the world
        // walk (R_RecursiveWorldNode) reaches the faces through the node. A
        // face naming a plane past the lump had its edges emitted but no
        // surface posted, and R_LeadingEdge indexed past the surfaces.
        use crate::bsp::{CONTENTS_EMPTY, DLeaf, DNode, DPlane, NUM_AMBIENTS};
        let mut bsp = demo_room();
        let n = bsp.faces.len();
        bsp.planes.push(DPlane { normal: [1.0, 0.0, 0.0], dist: -1000.0, ptype: 0 });
        let node_plane = (bsp.planes.len() - 1) as i32;
        bsp.nodes = vec![DNode {
            planenum: node_plane,
            children: [-2, -3],
            mins: [-300; 3],
            maxs: [300; 3],
            firstface: 0,
            numfaces: n as u16,
        }];
        let leaf = |contents| DLeaf {
            contents,
            visofs: -1,
            mins: [-300; 3],
            maxs: [300; 3],
            firstmarksurface: 0,
            nummarksurfaces: n as u16,
            ambient_level: [0; NUM_AMBIENTS],
        };
        bsp.leafs = vec![leaf(CONTENTS_SOLID), leaf(CONTENTS_EMPTY), leaf(CONTENTS_EMPTY)];
        bsp.marksurfaces = (0..n as u16).collect();
        for f in &mut bsp.faces {
            f.side = 0;
        }
        bsp.models[0].headnode = [0; crate::bsp::MAX_MAP_HULLS];
        bsp.models[0].visleafs = 2;
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let pal = palette();
        let draw = |bsp: &Bsp| render_once(&Scene::new(bsp, cam, 96, 64, &pal));
        for fi in 0..n {
            let mut bad = bsp.clone();
            bad.faces[fi].planenum = 9999;
            let mut gone = bsp.clone();
            gone.faces[fi].numedges = 0;
            assert_eq!(draw(&bad).pixels, draw(&gone).pixels, "face {fi} is left out, the rest drawn");
        }
    }

    /// The world of `scene` up to the scan, then the scan in one walk a
    /// scanline or in id's three: the spans (column, count, surface) and
    /// each row's first.
    fn scan(scene: &Scene, w: usize, h: usize, ids_walks: bool) -> (Vec<(i32, i32, u32)>, Vec<u32>) {
        let mut edge = EdgeState::new();
        edge.begin_map(scene.world);
        edge.setup_frame(&Frame::new(scene, w, h));
        edge.mark_leaves(scene.world);
        edge.begin_edge_frame();
        edge.render_world(scene.world);
        if ids_walks {
            edge.scan_edges_in_ids_walks();
        } else {
            edge.scan_edges();
        }
        (edge.spans.iter().map(|sp| (sp.u, sp.count, sp.surf)).collect(), edge.row_spans)
    }

    #[test]
    fn the_scan_in_one_walk_a_scanline_is_ids_three() {
        // R_GenerateSpans with each edge removed or stepped as it goes
        // against R_GenerateSpans, R_RemoveEdges, R_StepActiveU: the same
        // spans in the same order — the demo room from all round, and (when
        // id's pak is here) four maps from many eyes, level and tilted and
        // rolled, at three sizes.
        let pal = palette();
        let room = demo_room();
        let mut compared = 0;
        let mut check = |world: &Bsp, cam: Camera, w: usize, h: usize| {
            let scene = Scene::new(world, cam, w, h, &pal);
            let (one, ids) = (scan(&scene, w, h, false), scan(&scene, w, h, true));
            assert!(one == ids, "{cam:?} at {w}x{h}");
            assert_eq!(one.1.len(), h + 1);
            compared += one.0.len();
        };
        for k in 0..48 {
            let a = k as f32 * 0.37;
            let eye = [200.0 * a.cos(), 200.0 * a.sin(), -100.0 + 4.0 * k as f32];
            let cam = Camera { roll: (k % 5) as f32 * 3.0 - 6.0, ..Camera::looking_at(eye, [0.0, 0.0, 0.0], 90.0) };
            check(&room, cam, 97 + k, 61 + k % 7);
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        if let Ok(pak) = crate::pak::Pak::open(&path) {
            for (map, eyes) in [
                ("e1m1", [[480.0, -352.0, 110.0], [544.0, 288.0, 50.0], [600.0, 140.0, 88.0]]),
                ("e1m2", [[1496.0, 1664.0, 288.0], [1788.0, 296.0, 180.0], [1488.0, 1240.0, 296.0]]),
                ("e1m3", [[-1352.0, -720.0, -50.0], [-1352.0, -600.0, -40.0], [-1300.0, -400.0, -40.0]]),
                ("e1m4", [[998.0, 2246.0, 944.0], [320.0, 1284.0, 950.0], [320.0, 1284.0, 700.0]]),
            ] {
                let world = Bsp::parse(&pak.read_file(&format!("maps/{map}.bsp")).expect("read").expect("the map"))
                    .expect("a bsp");
                for (k, pos) in eyes.into_iter().enumerate() {
                    for (j, (w, h)) in [(320, 200), (701, 397), (1315, 535)].into_iter().enumerate() {
                        for turn in 0..6 {
                            let cam = Camera {
                                pos,
                                yaw: 60.0 * turn as f32 + 7.0 * k as f32,
                                pitch: [0.0, -25.0, 40.0][(turn + j) % 3],
                                roll: [0.0, 2.0, -80.0][(turn + k) % 3],
                                fov_deg: 90.0,
                            };
                            check(&world, cam, w, h);
                        }
                    }
                }
            }
            assert!(compared > 500_000, "{compared} spans compared");
        } else {
            eprintln!("id's maps skipped: no shareware pak at {}", path.display());
        }
        assert!(compared > 5_000, "{compared} spans compared");
    }

    #[test]
    fn c_ftoi_is_the_x86_conversion() {
        assert_eq!(c_ftoi(1.9), 1);
        assert_eq!(c_ftoi(-1.9), -1);
        assert_eq!(c_ftoi(3.0e9), i32::MIN);
        assert_eq!(c_ftoi(-3.0e9), i32::MIN);
        assert_eq!(c_ftoi(f64::NAN), i32::MIN);
    }
}

// The scan held to a transcription of id's three walks, table by table.
#[cfg(test)]
#[path = "edge_scan_tests.rs"]
mod scan_tests;
