//! Alias (MDL) models, the weapon included.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_alias.c` (`R_AliasDrawModel` and its setup, bbox and
//! lighting), `WinQuake/r_aclip.c` (`R_AliasClipTriangle`), `WinQuake/anorms.h`,
//! and `R_DrawViewModel` (`r_main.c`).

use super::light::{COLORMAP_LEN, LIGHTSTYLES, r_light_point_hit};
use super::polyse::{PolyFramebuffer, screen_box};
use super::stats::Profiler;
use super::{Camera, Frame, ViewGeom, nearest_index};
use crate::bsp::Bsp;
use crate::math::{Vec3, dot};

// ---------------------------------------------------------------------------
// Alias (MDL) models rendered into the world scene
// ---------------------------------------------------------------------------

/// One alias model placed in the world: the parsed [`Mdl`](crate::mdl::Mdl) plus its world
/// `origin`, `yaw` (degrees, rotation about `+Z`), the animation `frame` to
/// pose, and a flat base `color`.
///
/// Borrows the model so a single parsed `Mdl` (e.g. cached by name) can back
/// many instances without cloning. Rendered by `draw_alias_model`
/// ([`Scene::models`](super::Scene::models)) sharing the world's z-buffer, so
/// models occlude — and are occluded by — BSP geometry correctly.
///
/// `frame` selects which pose to draw (see `mdl_frame_verts`); an out-of-range
/// frame resets to 0 (matching `R_AliasSetupFrame`), so any value is safe.
///
/// `skinnum` is the per-entity skin index (`currententity->skinnum`): a model
/// with multiple skins (or a skin group) uses it to pick / animate its skin, so
/// e.g. a damaged or team-coloured variant renders. Out-of-range resets to 0.
/// Defaults to `0` via [`ModelInstance::with_frame`] for callers that do not yet
/// track per-entity skins.
///
/// Group-frame (`ALIAS_GROUP`) and group-skin (`ALIAS_SKIN_GROUP`) animation is
/// driven by the **scene `time`** ([`Scene::time`](super::Scene::time), not a
/// per-instance field), so existing callers animate for free as game time
/// advances. `R_AliasSetupFrame` / `R_AliasSetupSkin` select the sub-frame /
/// sub-skin whose interval window contains that time.
pub struct ModelInstance<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub origin: Vec3,
    pub yaw: f32,
    /// Entity pitch (`angles[PITCH]`, degrees, +down as QuakeC stores it). Drives
    /// flying projectiles (rockets/grenades/spikes point along their flight path).
    /// `0.0` keeps the model upright and renders bit-identically to yaw-only.
    pub pitch: f32,
    /// Entity roll (`angles[ROLL]`, degrees). `0.0` for upright models.
    pub roll: f32,
    pub frame: usize,
    /// The 2026 extra `r_lerpmodels` ([`crate::client::lerpmodels`]):
    /// `Some((prev_frame, frac))` blends `prev_frame`'s vertices toward
    /// `frame`'s by `frac` (0: `prev_frame` alone, 1: `frame` alone — the
    /// caller skips the work and passes `None` instead once a blend
    /// reaches 1). `None` (and the default, [`ModelInstance::with_frame`] /
    /// [`ModelInstance::new`]) draws `frame` alone, byte for byte as
    /// Classic always has.
    pub blend: Option<(usize, f32)>,
    /// The flat colour of a model without a usable skin (never one of id's:
    /// `Mod_LoadAliasModel` requires a skin), drawn as the nearest palette entry.
    pub color: [u8; 3],
    /// Per-entity skin index (`currententity->skinnum`); out-of-range -> 0.
    ///
    /// NOTE: this is a `Default`-able field; callers that build a [`ModelInstance`]
    /// with a struct literal must set it (use `skinnum: 0` when the entity's skin
    /// is not tracked). The [`ModelInstance::with_frame`] / [`ModelInstance::new`]
    /// constructors default it to 0.
    pub skinnum: i32,
}

impl<'a> ModelInstance<'a> {
    /// Build a [`ModelInstance`] with `skinnum`/`pitch`/`roll` defaulted to 0 — the
    /// common case for callers that only track yaw. Equivalent to the old literal.
    pub fn with_frame(
        mdl: &'a crate::mdl::Mdl,
        origin: Vec3,
        yaw: f32,
        frame: usize,
        color: [u8; 3],
    ) -> ModelInstance<'a> {
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, blend: None, color, skinnum: 0 }
    }

    /// Build a [`ModelInstance`] specifying the per-entity `skinnum` (pitch/roll 0).
    pub fn new(
        mdl: &'a crate::mdl::Mdl,
        origin: Vec3,
        yaw: f32,
        frame: usize,
        color: [u8; 3],
        skinnum: i32,
    ) -> ModelInstance<'a> {
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, blend: None, color, skinnum }
    }
}

/// Resolve the vertices of pose `frame` at game `time` for an [`Mdl`](crate::mdl::Mdl), delegating
/// to [`crate::mdl::Mdl::frame_pose`] (the `R_AliasSetupFrame` port).
///
/// `frame` (a `usize` here) is range-checked there: an out-of-range frame **resets
/// to 0** (matching the C, which does NOT clamp to the last frame). A
/// [`crate::mdl::Frame::Group`] now ANIMATES — its sub-pose is selected by `time`
/// against the group's intervals — so monster/torch group-frame models cycle
/// instead of freezing on the first sub-pose.
///
/// Returns `None` only when the model has no frames at all (or a group is empty).
fn mdl_frame_verts(mdl: &crate::mdl::Mdl, frame: usize, time: f32) -> Option<&[crate::mdl::TriVertex]> {
    // `usize -> i32`: a frame beyond `i32::MAX` is treated as out of range (-> 0),
    // exactly as `frame_pose` would do for any out-of-range index.
    let f = i32::try_from(frame).unwrap_or(-1);
    mdl.frame_pose(f, time)
}

/// `ALIAS_ONSEAM` flag (`modelgen.h`): the stvert lies on the texture seam that
/// separates the model skin's front half from its back half.
pub(super) const ALIAS_ONSEAM: i32 = 0x0020;

/// A usable model skin: its palette-index pixels plus dimensions, borrowed from
/// the [`Mdl`](crate::mdl::Mdl). Resolved by [`mdl_skin`].
struct ModelSkin<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
}

/// Resolve the texturing skin for an alias model at entity skin index `skinnum`
/// and game `time`, ported from `R_AliasSetupSkin` (`r_alias.c`): the skin is
/// chosen by the entity's `skinnum` (out of range -> 0), and an
/// `ALIAS_SKIN_GROUP` skin ANIMATES — the image is selected by `time` against
/// the group's intervals (via [`crate::mdl::Mdl::skin_image`]). The dimensions
/// are always the header's `skinwidth`/`skinheight`.
///
/// Returns `None` — so the caller falls back to the flat-colour path for the
/// whole model — when there is no skin, the dimensions are non-positive, the
/// pixel/dimension product overflows, or the pixel buffer is shorter than
/// `skinwidth * skinheight`.
fn mdl_skin(mdl: &crate::mdl::Mdl, skinnum: i32, time: f32) -> Option<ModelSkin<'_>> {
    let pixels: &[u8] = mdl.skin_image(skinnum, time)?;
    // Dimensions must be strictly positive to index a real grid.
    let width: usize = (mdl.header.skinwidth as i64).try_into().ok()?;
    let height: usize = (mdl.header.skinheight as i64).try_into().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let needed = width.checked_mul(height)?;
    if pixels.len() < needed {
        return None;
    }
    Some(ModelSkin { pixels, width, height })
}

// ---------------------------------------------------------------------------
// Alias models as id's software renderer draws them: R_AliasDrawModel and its
// setup (r_alias.c), R_AliasClipTriangle (r_aclip.c), D_PolysetDraw (d_polyse.c)
// ---------------------------------------------------------------------------
//
// Every alias model — monsters, items, torches and the weapon — goes through
// the same pipeline as in WinQuake: the vertices are transformed and projected
// to INTEGER screen coordinates with a per-vertex light level
// (`R_AliasTransformFinalVert`: the ambient light, minus the shade light times
// the vertex normal's cosine to a fixed light vector), triangles that cross the
// view edges or come nearer than `ALIAS_Z_CLIP_PLANE` are clipped
// (`R_AliasClipTriangle`), and each triangle is filled by `D_PolysetDraw`'s
// fixed-point edge walker: AFFINE texture mapping, Gouraud-stepped light, 1/z
// stepped for the z-buffer, and every pixel written through the colormap —
// `acolormap[texel + (light & 0xFF00)]` — so the output is always a palette
// index and fullbright texels stay fullbright.

/// `ALIAS_*_CLIP` (r_shared.h): a final vertex's out-codes.
const ALIAS_LEFT_CLIP: i32 = 0x0001;
const ALIAS_TOP_CLIP: i32 = 0x0002;
const ALIAS_RIGHT_CLIP: i32 = 0x0004;
const ALIAS_BOTTOM_CLIP: i32 = 0x0008;
const ALIAS_Z_CLIP: i32 = 0x0010;
const ALIAS_XY_CLIP_MASK: i32 = 0x000F;
/// `ALIAS_Z_CLIP_PLANE` (r_local.h): alias triangles are clipped 5 units in
/// front of the eye.
const ALIAS_Z_CLIP_PLANE: f32 = 5.0;
/// `LIGHT_MIN` (r_alias.c): no vertex is lit below this.
const LIGHT_MIN: i32 = 5;
/// `VID_CBITS` / `VID_GRADES` (vid.h): 64 light levels.
const VID_CBITS: i32 = 6;
const VID_GRADES: i32 = 1 << VID_CBITS;
/// The `r_aliastransbase` / `r_aliastransadj` cvar defaults (r_main.c): beyond
/// this distance a whole-on-screen model is drawn by recursive subdivision.
const R_ALIASTRANSBASE: f32 = 200.0;
const R_ALIASTRANSADJ: f32 = 100.0;
/// The light vector `R_DrawEntitiesOnList` and `R_DrawViewModel` give every
/// alias model (`{-1, 0, 0}`, "FIXME: remove and do real lighting").
const ALIAS_LIGHTVEC: Vec3 = [-1.0, 0.0, 0.0];
/// `(float)0x8000 * 0x10000`: the 1/z scale of alias z (`ziscale`), 2^31.
pub(super) const ALIAS_ZISCALE: f64 = 2_147_483_648.0;

/// `finalvert_t` (r_shared.h): `v` = screen u, v, skin s, t (16.16), light
/// (colormap offset, 8.8 rows), 1/z (scaled by 2^31); plus the out-codes /
/// `ALIAS_ONSEAM` flag.
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct FinalVert {
    pub(super) v: [i32; 6],
    pub(super) flags: i32,
}

/// The view state R_ViewChanged / R_SetupFrame leave for the alias renderer:
/// `vpn`/`vright`/`vup`, `r_origin`, `aliasxcenter`/`aliasycenter`,
/// `aliasxscale`/`aliasyscale` (`r_aliasuvscale` 1), the `aliasvrect` (the
/// whole image: the port renders the view rectangle into its own image), and
/// `r_aliastransition`/`r_resfudge`.
pub(super) struct AliasView {
    vpn: Vec3,
    vright: Vec3,
    vup: Vec3,
    origin: Vec3,
    xcenter: f32,
    ycenter: f32,
    xscale: f32,
    yscale: f32,
    pub(super) right: i32,
    pub(super) bottom: i32,
    transition: f32,
    resfudge: f32,
}

impl AliasView {
    /// The alias view of an image drawn by `cam` (its field of view the
    /// view's own) for the `fov` cvar `scr_fov`: projected as its view
    /// (`proj_w x proj_h`, the image's own unless it is a window onto a larger
    /// one, [`ViewGeom`]) and clipped to the image's `w x h`.
    fn new(cam: &Camera, scr_fov: f32, geom: &ViewGeom, pixel_aspect: f32) -> AliasView {
        let (w, h) = (geom.proj_w, geom.proj_h);
        let (vpn, vright, vup) = cam.basis();
        // R_ViewChanged: horizontalFieldOfView = 2*tan(fov_x/360*M_PI),
        // aliasxscale = vrect.width / it, aliasyscale = aliasxscale * pixelAspect.
        let hfov = (2.0 * (cam.fov_deg as f64 / 360.0 * std::f64::consts::PI).tan()) as f32;
        let hfov = if hfov.abs() > 1e-6 { hfov } else { 2.0 };
        let xscale = w as f32 / hfov;
        // r_aliastransition's res_scale: sqrt(width*height / (320*152)) *
        // (2 / horizontalFieldOfView). When Hor+ has widened the view
        // (`fov_x` over `scr_fov`) it is the 4:3 view's it widens, the width
        // `w * hfov(scr_fov) / hfov`: models are drawn at that view's size,
        // so they change drawing path at the same distance.
        let res_scale = if cam.fov_deg == scr_fov {
            ((w * h) as f64 / (320.0 * 152.0)).sqrt() * (2.0 / hfov as f64)
        } else {
            let hfov_ref = 2.0 * (scr_fov as f64 / 360.0 * std::f64::consts::PI).tan();
            let w_ref = w as f64 * hfov_ref / hfov as f64;
            (w_ref * h as f64 / (320.0 * 152.0)).sqrt() * (2.0 / hfov_ref)
        };
        AliasView {
            vpn,
            vright,
            vup,
            origin: cam.pos,
            // (A whole view's offsets are 0: id's centre to the bit.)
            xcenter: w as f32 * 0.5 - 0.5 - geom.ox as f32,
            ycenter: h as f32 * 0.5 - 0.5 - geom.oy as f32,
            xscale,
            yscale: xscale * pixel_aspect,
            right: geom.w as i32,
            bottom: geom.h as i32,
            transition: (R_ALIASTRANSBASE as f64 * res_scale) as f32,
            resfudge: (R_ALIASTRANSADJ as f64 * res_scale) as f32,
        }
    }

    /// The `ALIAS_*_CLIP` out-codes of a projected vertex against `aliasvrect`.
    fn xy_flags(&self, u: i32, v: i32) -> i32 {
        let mut f = 0;
        if u < 0 {
            f |= ALIAS_LEFT_CLIP;
        }
        if v < 0 {
            f |= ALIAS_TOP_CLIP;
        }
        if u > self.right {
            f |= ALIAS_RIGHT_CLIP;
        }
        if v > self.bottom {
            f |= ALIAS_BOTTOM_CLIP;
        }
        f
    }
}

/// One alias entity to draw: the fields of `entity_t` `R_AliasDrawModel` reads.
struct AliasEntity<'a> {
    mdl: &'a crate::mdl::Mdl,
    origin: Vec3,
    /// `angles` as the entity stores them (pitch, yaw, roll; pitch "backward").
    angles: Vec3,
    frame: usize,
    /// `r_lerpmodels`: see [`ModelInstance::blend`].
    blend: Option<(usize, f32)>,
    skinnum: i32,
    /// The flat palette index for a model without a usable skin (port fallback).
    color: u8,
}

/// `R_ConcatTransforms` (mathlib.c).
fn concat_transforms(a: &[[f32; 4]; 3], b: &[[f32; 4]; 3]) -> [[f32; 4]; 3] {
    let mut o = [[0.0f32; 4]; 3];
    for i in 0..3 {
        for j in 0..4 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
        o[i][3] += a[i][3];
    }
    o
}

/// `R_AliasSetUpTransform` (r_alias.c): model bytes -> view space (x right,
/// y down, z forward) in one 3x4 matrix, `viewmatrix * t2matrix * tmatrix`.
/// With `trivial_accept` the rows are pre-scaled for the unclipped projection
/// (x and y to screen units, all three by 2^-31 so 1/z comes out scaled).
/// Returns the matrix and the entity's `alias_forward/right/up`.
fn alias_setup_transform(
    view: &AliasView,
    header: &crate::mdl::MdlHeader,
    ent: &AliasEntity,
    trivial_accept: i32,
) -> ([[f32; 4]; 3], [Vec3; 3]) {
    let angles = [-ent.angles[0], ent.angles[1], ent.angles[2]];
    let (fwd, right, up) = crate::math::angle_vectors(angles);
    let mut tmatrix = [[0.0f32; 4]; 3];
    for (i, row) in tmatrix.iter_mut().enumerate() {
        row[i] = header.scale[i];
        row[3] = header.scale_origin[i];
    }
    let mut t2 = [[0.0f32; 4]; 3];
    for i in 0..3 {
        t2[i][0] = fwd[i];
        t2[i][1] = -right[i];
        t2[i][2] = up[i];
        // -modelorg, modelorg = r_origin - r_entorigin
        t2[i][3] = -(view.origin[i] - ent.origin[i]);
    }
    let rotation = concat_transforms(&t2, &tmatrix);
    let viewmatrix = [
        [view.vright[0], view.vright[1], view.vright[2], 0.0],
        [-view.vup[0], -view.vup[1], -view.vup[2], 0.0],
        [view.vpn[0], view.vpn[1], view.vpn[2], 0.0],
    ];
    let mut m = concat_transforms(&viewmatrix, &rotation);
    if trivial_accept != 0 {
        let k = 1.0 / ALIAS_ZISCALE;
        for (row, scale) in m.iter_mut().zip([view.xscale as f64 * k, view.yscale as f64 * k, k]) {
            for e in row.iter_mut() {
                *e = (*e as f64 * scale) as f32;
            }
        }
    }
    (m, [fwd, right, up])
}

/// `R_AliasTransformVector` / the transform half of `R_AliasTransformFinalVert`.
#[inline]
fn alias_transform_point(m: &[[f32; 4]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        p[0] * m[0][0] + p[1] * m[0][1] + p[2] * m[0][2] + m[0][3],
        p[0] * m[1][0] + p[1] * m[1][1] + p[2] * m[1][2] + m[1][3],
        p[0] * m[2][0] + p[1] * m[2][1] + p[2] * m[2][2] + m[2][3],
    ]
}

/// The bounding box `R_AliasCheckBBox` tests: the frame's (a group's own box
/// for a group frame), with an out-of-range frame read as 0.
fn alias_frame_bbox(mdl: &crate::mdl::Mdl, frame: usize) -> Option<([u8; 3], [u8; 3])> {
    let f = mdl.frames.get(frame).or_else(|| mdl.frames.first())?;
    Some(match f {
        crate::mdl::Frame::Single(af) => (af.bboxmin.v, af.bboxmax.v),
        crate::mdl::Frame::Group { bboxmin, bboxmax, .. } => (bboxmin.v, bboxmax.v),
    })
}

/// `R_AliasCheckBBox` (r_alias.c): transform the frame's bounding box; reject
/// the model when it is wholly nearer than the z-clip plane or wholly off one
/// side of the view, else return `trivial_accept`: 1 when no corner needs any
/// clipping, | 2 when also farther than `r_aliastransition + size*r_resfudge`
/// (drawn by recursive subdivision), 0 when triangles may need clipping.
fn alias_check_bbox(view: &AliasView, ent: &AliasEntity) -> Option<i32> {
    let header = &ent.mdl.header;
    let (m, _) = alias_setup_transform(view, header, ent, 0);
    let (lo, hi) = alias_frame_bbox(ent.mdl, ent.frame)?;
    let (lo, hi) = (lo.map(f32::from), hi.map(f32::from));
    // basepts: x from min for 0..3 and max for 4..7; y and z as the C lists them.
    let xs = [lo[0], lo[0], lo[0], lo[0], hi[0], hi[0], hi[0], hi[0]];
    let ys = [lo[1], hi[1], hi[1], lo[1], hi[1], lo[1], lo[1], hi[1]];
    let zs = [lo[2], lo[2], hi[2], hi[2], lo[2], lo[2], hi[2], hi[2]];
    let mut aux = [[0.0f32; 3]; 16];
    let mut zflag = [false; 16];
    let mut zclipped = false;
    let mut zfullyclipped = true;
    let mut minz: i32 = 9999;
    for i in 0..8 {
        aux[i] = alias_transform_point(&m, [xs[i], ys[i], zs[i]]);
        if aux[i][2] < ALIAS_Z_CLIP_PLANE {
            zflag[i] = true;
            zclipped = true;
        } else {
            if aux[i][2] < minz as f32 {
                minz = aux[i][2] as i32;
            }
            zfullyclipped = false;
        }
    }
    if zfullyclipped {
        return None;
    }
    let mut numv = 8;
    if zclipped {
        // aedges: the box's 12 edges; a crossing edge contributes its point on
        // the clip plane.
        const AEDGES: [(usize, usize); 12] =
            [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 5), (1, 4), (2, 7), (3, 6)];
        for &(a, b) in &AEDGES {
            if zflag[a] != zflag[b] {
                let (pa, pb) = (aux[a], aux[b]);
                let frac = (ALIAS_Z_CLIP_PLANE - pa[2]) / (pb[2] - pa[2]);
                aux[numv] = [pa[0] + (pb[0] - pa[0]) * frac, pa[1] + (pb[1] - pa[1]) * frac, ALIAS_Z_CLIP_PLANE];
                zflag[numv] = false;
                numv += 1;
            }
        }
    }
    let mut anyclip = 0;
    let mut allclip = ALIAS_XY_CLIP_MASK;
    for i in 0..numv {
        if zflag[i] {
            continue;
        }
        let zi = 1.0 / aux[i][2];
        let v0 = aux[i][0] * view.xscale * zi + view.xcenter;
        let v1 = aux[i][1] * view.yscale * zi + view.ycenter;
        let mut flags = 0;
        if v0 < 0.0 {
            flags |= ALIAS_LEFT_CLIP;
        }
        if v1 < 0.0 {
            flags |= ALIAS_TOP_CLIP;
        }
        if v0 > view.right as f32 {
            flags |= ALIAS_RIGHT_CLIP;
        }
        if v1 > view.bottom as f32 {
            flags |= ALIAS_BOTTOM_CLIP;
        }
        anyclip |= flags;
        allclip &= flags;
    }
    if allclip != 0 {
        return None;
    }
    let mut trivial_accept = i32::from(anyclip == 0 && !zclipped);
    // Mod_LoadAliasModel keeps `size * ALIAS_BASE_SIZE_RATIO` (1/11).
    let size = (header.size as f64 * (1.0 / 11.0)) as f32;
    if trivial_accept != 0 && minz as f32 > view.transition + size * view.resfudge {
        trivial_accept |= 2;
    }
    Some(trivial_accept)
}

/// The light `R_DrawEntitiesOnList` / `R_DrawViewModel` hand `R_AliasDrawModel`
/// (`alight_t`): `R_LightPoint` at the origin (at least 24 for the gun), plus
/// every dynamic light reaching it (`radius - distance`) into the ambient, then
/// ambient clamped to 128 and ambient + shade to 192. Returns (ambient, shade).
///
/// EXTRA (`r_torchflicker`, 2026): with `torches`, the luxel `R_LightPoint`
/// reads moves as the wall and floor luxels do — by its steady torches'
/// change this frame — so a model by a torch flickers with the floor it
/// stands on. `None` is id's.
fn alias_entity_light(
    bsp: &Bsp,
    origin: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
    torches: Option<&super::torch::TorchSet>,
    dlights: &[crate::dlight::DynamicLight],
    viewmodel: bool,
) -> (i32, i32) {
    // R_LightPoint's integer (`r >>= 8` of the style-scaled sum; the float sum
    // here is exact, so its floor is that integer).
    let (mut r, hit) = r_light_point_hit(bsp, origin, light_styles);
    if let (Some(t), Some((face, luxel))) = (torches, hit) {
        r = (r + t.face(face).at(luxel) * light_styles[0]).max(0.0);
    }
    let mut j = r.floor() as i32;
    if viewmodel && j < 24 {
        j = 24; // "allways give some light on gun"
    }
    let mut ambient = j;
    let mut shade = j;
    for dl in dlights {
        if viewmodel && dl.radius == 0.0 {
            continue;
        }
        let d = [origin[0] - dl.origin[0], origin[1] - dl.origin[1], origin[2] - dl.origin[2]];
        let add = dl.radius - (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if add > 0.0 {
            ambient = (ambient as f32 + add) as i32; // int += float
        }
    }
    // "clamp lighting so it doesn't overbright as much"
    if ambient > 128 {
        ambient = 128;
    }
    if ambient + shade > 192 {
        shade = 192 - ambient;
    }
    (ambient, shade)
}

/// `R_AliasSetupLighting` (r_alias.c): the ambient light as an inverted
/// colormap offset (`(255 - ambient) << VID_CBITS`, never below `LIGHT_MIN`),
/// the shade light in the same units, and the light vector rotated into the
/// model's frame.
fn alias_setup_lighting(ambient: i32, shade: i32, axes: &[Vec3; 3]) -> (i32, f32, Vec3) {
    let mut r_ambientlight = ambient.max(LIGHT_MIN);
    r_ambientlight = (255 - r_ambientlight) << VID_CBITS;
    if r_ambientlight < LIGHT_MIN {
        r_ambientlight = LIGHT_MIN;
    }
    let r_shadelight = (shade.max(0) as f32) * VID_GRADES as f32;
    let lv = ALIAS_LIGHTVEC;
    let plightvec = [dot(lv, axes[0]), -dot(lv, axes[1]), dot(lv, axes[2])];
    (r_ambientlight, r_shadelight, plightvec)
}

/// Everything `R_AliasDrawModel` sets up for one model and `D_PolysetDraw`
/// reads: the skin (`r_affinetridesc`), the colormap, the lighting, and the
/// transform.
pub(super) struct AliasSetup<'a> {
    pub(super) transform: [[f32; 4]; 3],
    pub(super) r_ambientlight: i32,
    pub(super) r_shadelight: f32,
    pub(super) plightvec: Vec3,
    pub(super) ziscale: f64,
    /// `r_affinetridesc.drawtype`: recursive subdivision instead of the edge walker.
    pub(super) subdiv: bool,
    pub(super) skin: Option<&'a [u8]>,
    pub(super) skinwidth: i32,
    pub(super) seamfixup: i32,
    pub(super) colormap: Option<&'a [u8]>,
    pub(super) flat: u8,
}

impl AliasSetup<'_> {
    /// `R_AliasTransformFinalVert` (r_alias.c): view-space position (the
    /// auxvert), skin coordinates in 16.16, and the vertex light: the ambient
    /// offset, lowered by `shadelight * cos` where the normal faces the
    /// light. With `prev` — `r_lerpmodels`, the smooth-animations extra —
    /// the position and the light are each blended with `prev`'s vertex
    /// first, by `frac` (0: `prev` alone, 1: `tv` alone): blending the
    /// position before the transform (not the two transformed positions
    /// after) is exactly the vertex a model whose *frame data* moved there
    /// would have, so it transforms, clips and shades like any other frame.
    /// The light is blended too, not just picked from the heavier side's
    /// normal: the one normal index a vertex carries changes in a single
    /// step between poses, so reading it alone would pop the shading at the
    /// 50% crossover; blending the two shades costs one extra dot product
    /// (self.vertex_light already paid for `tv`'s) and is never seen to pop.
    fn final_vert(
        &self,
        tv: &crate::mdl::TriVertex,
        prev: Option<(&crate::mdl::TriVertex, f32)>,
        st: &crate::mdl::StVert,
    ) -> ClipVert {
        let pos = match prev {
            Some((pv, frac)) => std::array::from_fn(|i| {
                let (p, c) = (f32::from(pv.v[i]), f32::from(tv.v[i]));
                p + (c - p) * frac
            }),
            None => tv.v.map(f32::from),
        };
        let av = alias_transform_point(&self.transform, pos);
        let mut fv = FinalVert { v: [0; 6], flags: st.onseam };
        fv.v[2] = st.s.wrapping_shl(16);
        fv.v[3] = st.t.wrapping_shl(16);
        fv.v[4] = match prev {
            Some((pv, frac)) => {
                let (a, b) = (self.vertex_light(pv.lightnormalindex), self.vertex_light(tv.lightnormalindex));
                a + (((b - a) as f32) * frac) as i32
            }
            None => self.vertex_light(tv.lightnormalindex),
        };
        (fv, av)
    }

    #[inline]
    fn vertex_light(&self, lightnormalindex: u8) -> i32 {
        let n = R_AVERTEXNORMALS.get(lightnormalindex as usize).copied().unwrap_or([0.0; 3]);
        let lightcos = dot(n, self.plightvec);
        let mut temp = self.r_ambientlight;
        if lightcos < 0.0 {
            temp += (self.r_shadelight * lightcos) as i32;
            if temp < 0 {
                temp = 0;
            }
        }
        temp
    }
}

/// `R_AliasProjectFinalVert` (r_alias.c): project a view-space point (z at
/// least `ALIAS_Z_CLIP_PLANE`) to integer screen coordinates and scaled 1/z.
fn alias_project(fv: &mut FinalVert, av: [f32; 3], view: &AliasView, ziscale: f64) {
    let zi = 1.0 / av[2];
    fv.v[5] = (zi as f64 * ziscale) as i32;
    fv.v[0] = ((av[0] as f64 * view.xscale as f64 * zi as f64) + view.xcenter as f64) as i32;
    fv.v[1] = ((av[1] as f64 * view.yscale as f64 * zi as f64) + view.ycenter as f64) as i32;
}

/// One triangle for `D_PolysetDraw`: its screen vertices, and whether it
/// faces front (a back-facing seam vertex takes the skin's back half).
#[derive(Clone, Copy)]
struct PolyTri {
    v: [FinalVert; 3],
    facesfront: bool,
}

/// An alias model ready for the bands: `R_AliasDrawModel` up to the
/// rasteriser — the skin, light and transform set up, every vertex
/// transformed and projected, every triangle clipped to the view — so that a
/// band only rasterises (`D_PolysetDraw`) what falls in its rows.
pub(super) struct AliasDraw<'a> {
    setup: AliasSetup<'a>,
    /// The vertices `D_PolysetDrawFinalVerts` plots first (a subdivided model's;
    /// those inside the view).
    points: Vec<FinalVert>,
    /// The triangles in id's order, a clipped one as its fan.
    tris: Vec<PolyTri>,
    /// The box every point and triangle lies in ([`screen_box`]; `None`
    /// with nothing to draw): a band it does not reach skips the model
    /// without asking each triangle.
    reach: Option<[i64; 4]>,
}

impl AliasDraw<'_> {
    /// EXTRA, debug only: the triangles as the rasteriser gets them, each
    /// vertex `(x, y, 1/z)` with `1/z` in the z-buffer's units (`>> 16` of
    /// the finalvert's), for an x-ray capture.
    pub(super) fn xray_triangles(&self) -> impl Iterator<Item = [[f32; 3]; 3]> + '_ {
        self.tris.iter().map(|t| t.v.map(|fv| [fv.v[0] as f32, fv.v[1] as f32, fv.v[5] as f32 / 65536.0]))
    }

    /// `D_PolysetDraw` of the model into `fb`'s rows: the points, then the
    /// triangles (those that can reach the rows).
    pub(super) fn draw(&self, fb: &mut PolyFramebuffer) {
        if !self.reach.is_some_and(|reach| fb.touches_box(&reach)) {
            return;
        }
        fb.draw_final_verts(&self.setup, &self.points);
        for t in &self.tris {
            if fb.touches(&t.v) {
                fb.polyset_draw(&self.setup, t.v, t.facesfront);
            }
        }
    }
}

/// `R_AliasDrawModel` (r_alias.c) up to the rasteriser: one alias entity's
/// [`AliasDraw`]. `trivial_accept` comes from [`alias_check_bbox`] (always 0
/// for the gun, which is never bbox-tested); the gun's 1/z is tripled
/// (`ziscale * 3`) so it wins the depth test against anything but a wall
/// right against the eye.
fn alias_prepare<'a>(
    view: &AliasView,
    ent: &AliasEntity<'a>,
    trivial_accept: i32,
    light: (i32, i32),
    viewmodel: bool,
    time: f32,
    colormap: Option<&'a [u8]>,
) -> Option<AliasDraw<'a>> {
    let mdl = ent.mdl;
    let header = &mdl.header;
    // R_AliasSetupSkin: the entity's skin (a skin group animates by time).
    let skin = mdl_skin(mdl, ent.skinnum, time);
    let skinwidth = skin.as_ref().map_or(header.skinwidth.max(0), |s| s.width as i32);
    let (transform, axes) = alias_setup_transform(view, header, ent, trivial_accept);
    let (r_ambientlight, r_shadelight, plightvec) = alias_setup_lighting(light.0, light.1, &axes);
    // R_AliasSetupFrame
    let verts = mdl_frame_verts(mdl, ent.frame, time)?;
    // r_lerpmodels (the 2026 extra): blend `verts` with the previous frame's
    // vertices by `ent.blend`'s fraction (see `ModelInstance::blend` /
    // `Viewmodel::blend`). The caller already ruled out a group frame on
    // either side before setting `ent.blend`, so a `Some` here always means
    // an ordinary two-pose blend; a failed read (an out-of-range `prev` the
    // caller somehow passed) just draws `verts` alone, same as `None`.
    let blend = ent.blend.and_then(|(prev_frame, frac)| {
        let prev_verts = mdl_frame_verts(mdl, prev_frame, time)?;
        Some((prev_verts, frac))
    });
    let setup = AliasSetup {
        transform,
        r_ambientlight,
        r_shadelight,
        plightvec,
        ziscale: if viewmodel { ALIAS_ZISCALE * 3.0 } else { ALIAS_ZISCALE },
        subdiv: trivial_accept == 3,
        skin: skin.as_ref().map(|s| &s.pixels[..s.width * s.height]),
        skinwidth,
        seamfixup: (skinwidth >> 1) << 16,
        colormap: colormap.filter(|cm| cm.len() >= COLORMAP_LEN),
        flat: ent.color,
    };
    let n = verts.len().min(mdl.stverts.len());
    let mut fverts: Vec<FinalVert> = Vec::with_capacity(n);
    let mut aux: Vec<[f32; 3]> = Vec::with_capacity(n);
    for (i, (tv, st)) in verts.iter().zip(mdl.stverts.iter()).enumerate() {
        let prev = blend.as_ref().and_then(|(pv, frac)| pv.get(i).map(|p| (p, *frac)));
        let (mut fv, av) = setup.final_vert(tv, prev, st);
        if trivial_accept != 0 {
            // R_AliasTransformAndProjectFinalVerts: the transform is prescaled,
            // so 1/z comes out times 2^31 and x, y in screen units.
            let zi = 1.0 / av[2];
            fv.v[5] = zi as i32;
            fv.v[0] = ((av[0] * zi) as f64 + view.xcenter as f64) as i32;
            fv.v[1] = ((av[1] * zi) as f64 + view.ycenter as f64) as i32;
        } else if av[2] < ALIAS_Z_CLIP_PLANE {
            fv.flags |= ALIAS_Z_CLIP;
        } else {
            alias_project(&mut fv, av, view, setup.ziscale);
            fv.flags |= view.xy_flags(fv.v[0], fv.v[1]);
        }
        fverts.push(fv);
        aux.push(av);
    }
    let vert = |i: i32| usize::try_from(i).ok().filter(|&i| i < fverts.len());
    let mut points = Vec::new();
    let mut tris = Vec::with_capacity(mdl.triangles.len());
    if trivial_accept != 0 {
        // R_AliasPrepareUnclippedPoints: D_PolysetDrawFinalVerts' points
        // (those inside the view), then the triangles.
        if setup.subdiv {
            points.extend(
                fverts
                    .iter()
                    .filter(|fv| fv.v[0] < view.right && fv.v[1] < view.bottom && fv.v[0] >= 0 && fv.v[1] >= 0),
            );
        }
        for tri in &mdl.triangles {
            if let (Some(a), Some(b), Some(c)) =
                (vert(tri.vertindex[0]), vert(tri.vertindex[1]), vert(tri.vertindex[2]))
            {
                tris.push(PolyTri { v: [fverts[a], fverts[b], fverts[c]], facesfront: tri.facesfront != 0 });
            }
        }
    } else {
        // R_AliasPreparePoints: clip and draw each triangle.
        for tri in &mdl.triangles {
            let (Some(a), Some(b), Some(c)) = (vert(tri.vertindex[0]), vert(tri.vertindex[1]), vert(tri.vertindex[2]))
            else {
                continue;
            };
            let pfv = [fverts[a], fverts[b], fverts[c]];
            let all = pfv[0].flags & pfv[1].flags & pfv[2].flags;
            let any = pfv[0].flags | pfv[1].flags | pfv[2].flags;
            if all & (ALIAS_XY_CLIP_MASK | ALIAS_Z_CLIP) != 0 {
                continue; // completely clipped
            }
            if any & (ALIAS_XY_CLIP_MASK | ALIAS_Z_CLIP) == 0 {
                tris.push(PolyTri { v: pfv, facesfront: tri.facesfront != 0 });
            } else {
                alias_clip_triangle(&mut tris, &setup, view, pfv, [aux[a], aux[b], aux[c]], tri.facesfront != 0);
            }
        }
    }
    let reach = screen_box(points.iter().chain(tris.iter().flat_map(|t| &t.v)));
    Some(AliasDraw { setup, points, tris, reach })
}

/// A clipped-polygon vertex: the final vertex and its view-space position.
type ClipVert = (FinalVert, [f32; 3]);

/// `R_AliasClip` (r_aclip.c): clip the polygon `input` against one out-code
/// `flag`, the new vertices from `clip`, each given fresh xy out-codes.
fn alias_clip(
    input: &[ClipVert],
    flag: i32,
    view: &AliasView,
    clip: &dyn Fn(&ClipVert, &ClipVert) -> FinalVert,
) -> Vec<ClipVert> {
    let mut out = Vec::with_capacity(input.len() + 2);
    let count = input.len();
    let mut j = count.wrapping_sub(1);
    for i in 0..count {
        let oldflags = input[j].0.flags & flag;
        let flags = input[i].0.flags & flag;
        if flags != 0 && oldflags != 0 {
            j = i;
            continue;
        }
        if (oldflags ^ flags) != 0 {
            let mut v = clip(&input[j], &input[i]);
            v.flags = view.xy_flags(v.v[0], v.v[1]);
            out.push((v, [0.0; 3]));
        }
        if flags == 0 {
            out.push(input[i]);
        }
        j = i;
    }
    out
}

/// The screen-edge clips of r_aclip.c (`R_Alias_clip_left` and friends): the
/// crossing point at `bound` along axis `axis` (0 = u, 1 = v), every field
/// interpolated and rounded (`+ 0.5`), always from the vertex lower on screen.
fn alias_clip_screen(a: &FinalVert, b: &FinalVert, axis: usize, bound: i32) -> FinalVert {
    let (p0, p1) = if a.v[1] >= b.v[1] { (a, b) } else { (b, a) };
    let scale = (bound - p0.v[axis]) as f32 / (p1.v[axis] - p0.v[axis]) as f32;
    let mut out = FinalVert::default();
    for i in 0..6 {
        out.v[i] = (p0.v[i] as f64 + ((p1.v[i] - p0.v[i]) as f32 * scale) as f64 + 0.5) as i32;
    }
    out
}

/// `R_AliasClipTriangle` (r_aclip.c): clip a triangle that crosses the z
/// plane or a view edge, clamp the result into the view, and add it to `tris`
/// as a fan.
fn alias_clip_triangle(
    tris: &mut Vec<PolyTri>,
    setup: &AliasSetup,
    view: &AliasView,
    pfv: [FinalVert; 3],
    aux: [[f32; 3]; 3],
    facesfront: bool,
) {
    // copy vertexes and fix seam texture coordinates
    let mut poly: Vec<ClipVert> = (0..3)
        .map(|i| {
            let mut v = pfv[i];
            if !facesfront && (v.flags & ALIAS_ONSEAM) != 0 {
                v.v[2] += setup.seamfixup;
            }
            (v, aux[i])
        })
        .collect();
    let mut clipflags = pfv[0].flags | pfv[1].flags | pfv[2].flags;
    if clipflags & ALIAS_Z_CLIP != 0 {
        // R_Alias_clip_z: interpolate in view space to the plane, then project.
        let clip_z = |a: &ClipVert, b: &ClipVert| {
            let (p0, p1) = if a.0.v[1] >= b.0.v[1] { (a, b) } else { (b, a) };
            let (av0, av1) = (p0.1, p1.1);
            let scale = (ALIAS_Z_CLIP_PLANE - av0[2]) / (av1[2] - av0[2]);
            let avout = [av0[0] + (av1[0] - av0[0]) * scale, av0[1] + (av1[1] - av0[1]) * scale, ALIAS_Z_CLIP_PLANE];
            let mut out = FinalVert::default();
            for i in 2..5 {
                out.v[i] = (p0.0.v[i] as f32 + (p1.0.v[i] - p0.0.v[i]) as f32 * scale) as i32;
            }
            alias_project(&mut out, avout, view, setup.ziscale);
            out
        };
        poly = alias_clip(&poly, ALIAS_Z_CLIP, view, &clip_z);
        if poly.is_empty() {
            return;
        }
        clipflags = poly.iter().take(3).fold(0, |f, v| f | v.0.flags);
    }
    let edges: [(i32, usize, i32); 4] = [
        (ALIAS_LEFT_CLIP, 0, 0),
        (ALIAS_RIGHT_CLIP, 0, view.right),
        (ALIAS_BOTTOM_CLIP, 1, view.bottom),
        (ALIAS_TOP_CLIP, 1, 0),
    ];
    for (flag, axis, bound) in edges {
        if clipflags & flag != 0 {
            let clip = |a: &ClipVert, b: &ClipVert| alias_clip_screen(&a.0, &b.0, axis, bound);
            poly = alias_clip(&poly, flag, view, &clip);
            if poly.is_empty() {
                return;
            }
        }
    }
    for (v, _) in poly.iter_mut() {
        v.v[0] = v.v[0].clamp(0, view.right);
        v.v[1] = v.v[1].clamp(0, view.bottom);
        v.flags = 0;
    }
    for i in 1..poly.len().saturating_sub(1) {
        tris.push(PolyTri { v: [poly[0].0, poly[i].0, poly[i + 1].0], facesfront });
    }
}

/// One alias-model instance as `R_DrawEntitiesOnList` draws it, up to the
/// rasteriser: the bounding box test (`R_AliasCheckBBox`), the light at the
/// origin plus dynamic lights, then `R_AliasDrawModel`'s setup
/// ([`alias_prepare`]). `None` when it draws nothing. It shares the world's
/// z-buffer.
pub(super) fn prepare_alias_model<'a>(
    frame: &Frame<'_, 'a>,
    inst: &ModelInstance<'a>,
    prof: &mut Profiler,
) -> Option<AliasDraw<'a>> {
    if frame.w == 0 || frame.h == 0 {
        return None;
    }
    let scene = frame.scene;
    let view = AliasView::new(&frame.cam, frame.scr_fov(), &frame.geom, scene.options.aspect());
    let ent = AliasEntity {
        mdl: inst.mdl,
        origin: inst.origin,
        angles: [inst.pitch, inst.yaw, inst.roll],
        frame: inst.frame,
        blend: inst.blend,
        skinnum: inst.skinnum,
        color: nearest_index(scene.palette, inst.color),
    };
    prof.add(|s| s.alias_models += 1);
    let trivial_accept = alias_check_bbox(&view, &ent)?;
    prof.add(|s| {
        s.alias_accepted += 1;
        s.alias_tris += inst.mdl.header.numtris.max(0) as u64;
    });
    let light = alias_entity_light(scene.world, inst.origin, scene.light_styles, frame.torches, scene.dlights, false);
    alias_prepare(&view, &ent, trivial_accept, light, false, scene.time, scene.colormap)
}

/// `r_avertexnormals` (anorms.h): the 162 precomputed vertex normals an MDL
/// vertex's `lightnormalindex` selects.
#[rustfmt::skip]
static R_AVERTEXNORMALS: [[f32; 3]; 162] = [
    [-0.525731, 0.000000, 0.850651], [-0.442863, 0.238856, 0.864188], [-0.295242, 0.000000, 0.955423],
    [-0.309017, 0.500000, 0.809017], [-0.162460, 0.262866, 0.951056], [0.000000, 0.000000, 1.000000],
    [0.000000, 0.850651, 0.525731], [-0.147621, 0.716567, 0.681718], [0.147621, 0.716567, 0.681718],
    [0.000000, 0.525731, 0.850651], [0.309017, 0.500000, 0.809017], [0.525731, 0.000000, 0.850651],
    [0.295242, 0.000000, 0.955423], [0.442863, 0.238856, 0.864188], [0.162460, 0.262866, 0.951056],
    [-0.681718, 0.147621, 0.716567], [-0.809017, 0.309017, 0.500000], [-0.587785, 0.425325, 0.688191],
    [-0.850651, 0.525731, 0.000000], [-0.864188, 0.442863, 0.238856], [-0.716567, 0.681718, 0.147621],
    [-0.688191, 0.587785, 0.425325], [-0.500000, 0.809017, 0.309017], [-0.238856, 0.864188, 0.442863],
    [-0.425325, 0.688191, 0.587785], [-0.716567, 0.681718, -0.147621], [-0.500000, 0.809017, -0.309017],
    [-0.525731, 0.850651, 0.000000], [0.000000, 0.850651, -0.525731], [-0.238856, 0.864188, -0.442863],
    [0.000000, 0.955423, -0.295242], [-0.262866, 0.951056, -0.162460], [0.000000, 1.000000, 0.000000],
    [0.000000, 0.955423, 0.295242], [-0.262866, 0.951056, 0.162460], [0.238856, 0.864188, 0.442863],
    [0.262866, 0.951056, 0.162460], [0.500000, 0.809017, 0.309017], [0.238856, 0.864188, -0.442863],
    [0.262866, 0.951056, -0.162460], [0.500000, 0.809017, -0.309017], [0.850651, 0.525731, 0.000000],
    [0.716567, 0.681718, 0.147621], [0.716567, 0.681718, -0.147621], [0.525731, 0.850651, 0.000000],
    [0.425325, 0.688191, 0.587785], [0.864188, 0.442863, 0.238856], [0.688191, 0.587785, 0.425325],
    [0.809017, 0.309017, 0.500000], [0.681718, 0.147621, 0.716567], [0.587785, 0.425325, 0.688191],
    [0.955423, 0.295242, 0.000000], [1.000000, 0.000000, 0.000000], [0.951056, 0.162460, 0.262866],
    [0.850651, -0.525731, 0.000000], [0.955423, -0.295242, 0.000000], [0.864188, -0.442863, 0.238856],
    [0.951056, -0.162460, 0.262866], [0.809017, -0.309017, 0.500000], [0.681718, -0.147621, 0.716567],
    [0.850651, 0.000000, 0.525731], [0.864188, 0.442863, -0.238856], [0.809017, 0.309017, -0.500000],
    [0.951056, 0.162460, -0.262866], [0.525731, 0.000000, -0.850651], [0.681718, 0.147621, -0.716567],
    [0.681718, -0.147621, -0.716567], [0.850651, 0.000000, -0.525731], [0.809017, -0.309017, -0.500000],
    [0.864188, -0.442863, -0.238856], [0.951056, -0.162460, -0.262866], [0.147621, 0.716567, -0.681718],
    [0.309017, 0.500000, -0.809017], [0.425325, 0.688191, -0.587785], [0.442863, 0.238856, -0.864188],
    [0.587785, 0.425325, -0.688191], [0.688191, 0.587785, -0.425325], [-0.147621, 0.716567, -0.681718],
    [-0.309017, 0.500000, -0.809017], [0.000000, 0.525731, -0.850651], [-0.525731, 0.000000, -0.850651],
    [-0.442863, 0.238856, -0.864188], [-0.295242, 0.000000, -0.955423], [-0.162460, 0.262866, -0.951056],
    [0.000000, 0.000000, -1.000000], [0.295242, 0.000000, -0.955423], [0.162460, 0.262866, -0.951056],
    [-0.442863, -0.238856, -0.864188], [-0.309017, -0.500000, -0.809017], [-0.162460, -0.262866, -0.951056],
    [0.000000, -0.850651, -0.525731], [-0.147621, -0.716567, -0.681718], [0.147621, -0.716567, -0.681718],
    [0.000000, -0.525731, -0.850651], [0.309017, -0.500000, -0.809017], [0.442863, -0.238856, -0.864188],
    [0.162460, -0.262866, -0.951056], [0.238856, -0.864188, -0.442863], [0.500000, -0.809017, -0.309017],
    [0.425325, -0.688191, -0.587785], [0.716567, -0.681718, -0.147621], [0.688191, -0.587785, -0.425325],
    [0.587785, -0.425325, -0.688191], [0.000000, -0.955423, -0.295242], [0.000000, -1.000000, 0.000000],
    [0.262866, -0.951056, -0.162460], [0.000000, -0.850651, 0.525731], [0.000000, -0.955423, 0.295242],
    [0.238856, -0.864188, 0.442863], [0.262866, -0.951056, 0.162460], [0.500000, -0.809017, 0.309017],
    [0.716567, -0.681718, 0.147621], [0.525731, -0.850651, 0.000000], [-0.238856, -0.864188, -0.442863],
    [-0.500000, -0.809017, -0.309017], [-0.262866, -0.951056, -0.162460], [-0.850651, -0.525731, 0.000000],
    [-0.716567, -0.681718, -0.147621], [-0.716567, -0.681718, 0.147621], [-0.525731, -0.850651, 0.000000],
    [-0.500000, -0.809017, 0.309017], [-0.238856, -0.864188, 0.442863], [-0.262866, -0.951056, 0.162460],
    [-0.864188, -0.442863, 0.238856], [-0.809017, -0.309017, 0.500000], [-0.688191, -0.587785, 0.425325],
    [-0.681718, -0.147621, 0.716567], [-0.442863, -0.238856, 0.864188], [-0.587785, -0.425325, 0.688191],
    [-0.309017, -0.500000, 0.809017], [-0.147621, -0.716567, 0.681718], [-0.425325, -0.688191, 0.587785],
    [-0.162460, -0.262866, 0.951056], [0.442863, -0.238856, 0.864188], [0.162460, -0.262866, 0.951056],
    [0.309017, -0.500000, 0.809017], [0.147621, -0.716567, 0.681718], [0.000000, -0.525731, 0.850651],
    [0.425325, -0.688191, 0.587785], [0.587785, -0.425325, 0.688191], [0.688191, -0.587785, 0.425325],
    [-0.955423, 0.295242, 0.000000], [-0.951056, 0.162460, 0.262866], [-1.000000, 0.000000, 0.000000],
    [-0.850651, 0.000000, 0.525731], [-0.955423, -0.295242, 0.000000], [-0.951056, -0.162460, 0.262866],
    [-0.864188, 0.442863, -0.238856], [-0.951056, 0.162460, -0.262866], [-0.809017, 0.309017, -0.500000],
    [-0.864188, -0.442863, -0.238856], [-0.951056, -0.162460, -0.262866], [-0.809017, -0.309017, -0.500000],
    [-0.681718, 0.147621, -0.716567], [-0.681718, -0.147621, -0.716567], [-0.850651, 0.000000, -0.525731],
    [-0.688191, 0.587785, -0.425325], [-0.587785, 0.425325, -0.688191], [-0.425325, 0.688191, -0.587785],
    [-0.425325, -0.688191, -0.587785], [-0.587785, -0.425325, -0.688191], [-0.688191, -0.587785, -0.425325],
];

/// The player's first-person weapon viewmodel: the parsed weapon [`Mdl`](crate::mdl::Mdl)
/// (`progs/v_shot.mdl` and friends) plus the animation `frame` to pose.
///
/// Unlike [`ModelInstance`], a viewmodel is placed relative to the camera:
/// Quake's `cl.viewent`, which V_CalcRefdef puts at the eye plus the bob and
/// viewsize fudge (`origin_ofs`) with CalcGunAngle's `angles`, drawn by
/// `R_DrawViewModel` through the ordinary alias pipeline. See
/// [`Scene::viewmodel`](super::Scene::viewmodel).
///
/// `frame` selects the pose (an out-of-range frame draws frame 0, as
/// `R_AliasSetupFrame` does, see `mdl_frame_verts`). The model is borrowed
/// so a cached `Mdl` backs it without cloning.
#[derive(Clone, Copy)]
pub struct Viewmodel<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub frame: usize,
    /// The 2026 extra `r_lerpmodels` ([`ModelInstance::blend`]): the gun's
    /// animation blends too (QuakeSpasm does the same), a weapon switch
    /// (the model changes) snapping like any other model change. Computed
    /// by the same [`crate::client::lerpmodels::FrameLerps`] the entities
    /// share, under a sentinel key of its own (`cl_main::walk_frame`,
    /// `cl_demo::render_demo_frame`): the view weapon is never an edict.
    pub blend: Option<(usize, f32)>,
    /// Where V_CalcRefdef puts the gun (`view->origin`) relative to the
    /// camera (`r_refdef.vieworg`), in world units: see
    /// [`viewmodel_origin_ofs`](super::view::viewmodel_origin_ofs).
    pub origin_ofs: Vec3,
    /// The gun's orientation, `cl.viewent.angles` as CalcGunAngle leaves
    /// them — pitch (+up, like [`Camera::pitch`]), yaw, roll: the view angles
    /// BEFORE `cl.punchangle` is added and without V_CalcViewRoll's roll (only
    /// `cl.viewangles[ROLL]`), so the weapon kick and the strafe lean move the
    /// view but not the gun. See [`viewmodel_angles`](super::view::viewmodel_angles).
    pub angles: Vec3,
}

/// The first-person weapon up to the rasteriser — `R_DrawViewModel` (r_main.c): `cl.viewent`
/// posed at V_CalcRefdef's gun origin (the camera plus `origin_ofs`, see
/// [`viewmodel_origin_ofs`](super::view::viewmodel_origin_ofs)) facing along the view, lit by `R_LightPoint` at
/// that origin (at least 24) plus dynamic lights, and drawn by the same
/// `R_AliasDrawModel` as any alias model — never bbox-tested, so every
/// triangle takes the clipping path (the grip nearer than `ALIAS_Z_CLIP_PLANE`
/// is trimmed), and with its 1/z tripled so it wins the shared depth test
/// against everything but a wall right against the eye. No gun when the `fov`
/// cvar, `scr_fov`, is over 90 (`r_fov_greater_than_90`) — the cvar, not the
/// view's field of view, which Hor+ widens past 90 on a wide screen.
pub(super) fn prepare_viewmodel<'a>(frame: &Frame<'_, 'a>, vm: &Viewmodel<'a>) -> Option<AliasDraw<'a>> {
    let (scene, cam, scr_fov) = (frame.scene, &frame.cam, frame.scr_fov());
    if frame.w == 0 || frame.h == 0 || scr_fov > 90.0 {
        return None;
    }
    let view = AliasView::new(cam, scr_fov, &frame.geom, scene.options.aspect());
    let origin = [cam.pos[0] + vm.origin_ofs[0], cam.pos[1] + vm.origin_ofs[1], cam.pos[2] + vm.origin_ofs[2]];
    // CalcGunAngle's angles (pitch stored "backward", i.e. +up like the camera).
    let ent = AliasEntity {
        mdl: vm.mdl,
        origin,
        angles: vm.angles,
        frame: vm.frame,
        blend: vm.blend,
        skinnum: 0,
        color: nearest_index(scene.palette, [180, 180, 180]),
    };
    let light = alias_entity_light(scene.world, origin, scene.light_styles, frame.torches, scene.dlights, true);
    alias_prepare(&view, &ent, 0, light, true, scene.time, scene.colormap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::render_once;
    use crate::render::fixtures::tiny_mdl;
    use crate::render::light::NEUTRAL_LIGHTSTYLE_SCALES;
    use crate::render::{Image, Palette, Scene, demo_room, fixtures};

    #[test]
    fn render_scene_model_changes_pixels() {
        // A model placed in front of the camera must alter some pixels relative
        // to the world-only render.
        let bsp = demo_room();
        let pal = fixtures::ramp_palette();
        // Look from the west wall toward the centre (down +X).
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));

        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0], // between the camera and the centre
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let with_model =
            render_once(&Scene { models: std::slice::from_ref(&inst), ..Scene::new(&bsp, cam, 160, 120, &pal) });

        let changed = world_only.pixels.iter().zip(with_model.pixels.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "model in front of camera changed no pixels");
    }

    /// Build a synthetic two-frame single-skin MDL. Frame 0 and frame 1 carry
    /// distinct vertex data so the selected pose is observable.
    fn two_frame_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, TriVertex, Triangle};
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            // 1 unit of v -> 1 world unit; origin centres the box (matches the
            // proven-visible geometry of `tiny_mdl`).
            scale: [1.0, 1.0, 1.0],
            scale_origin: [-16.0, -16.0, -16.0],
            boundingradius: 32.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 1,
            numframes: 2,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Frame 0: the same triangle `tiny_mdl` uses (known to rasterise here).
        let frame0 = AliasFrame {
            name: "pose0".into(),
            bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            bboxmax: TriVertex { v: [32, 0, 32], lightnormalindex: 0 },
            verts: vec![
                TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                TriVertex { v: [32, 0, 0], lightnormalindex: 0 },
                TriVertex { v: [0, 0, 32], lightnormalindex: 0 },
            ],
        };
        // Frame 1: a clearly different pose — the triangle spread along +Y so it
        // faces the camera differently and covers a different screen region.
        let frame1 = AliasFrame {
            name: "pose1".into(),
            bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            bboxmax: TriVertex { v: [255, 255, 255], lightnormalindex: 0 },
            verts: vec![
                TriVertex { v: [255, 0, 255], lightnormalindex: 0 },
                TriVertex { v: [255, 255, 0], lightnormalindex: 0 },
                TriVertex { v: [0, 255, 255], lightnormalindex: 0 },
            ],
        };
        Mdl {
            header,
            skins: vec![Skin::Single(vec![0])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            triangles: vec![Triangle { facesfront: 1, vertindex: [0, 1, 2] }],
            frames: vec![Frame::Single(frame0), Frame::Single(frame1)],
        }
    }

    #[test]
    fn mdl_frame_verts_selects_and_resets_out_of_range() {
        use crate::mdl::TriVertex;
        let mdl = two_frame_mdl();

        // Frame 0 -> first pose.
        let f0 = mdl_frame_verts(&mdl, 0, 0.0).expect("frame 0 present");
        assert_eq!(f0.len(), 3);
        assert_eq!(f0[0], TriVertex { v: [0, 0, 0], lightnormalindex: 0 });
        assert_eq!(f0[1], TriVertex { v: [32, 0, 0], lightnormalindex: 0 });

        // Frame 1 -> second, distinct pose.
        let f1 = mdl_frame_verts(&mdl, 1, 0.0).expect("frame 1 present");
        assert_eq!(f1[0], TriVertex { v: [255, 0, 255], lightnormalindex: 0 });
        assert_eq!(f1[2], TriVertex { v: [0, 255, 255], lightnormalindex: 0 });

        // FAITHFULNESS: an out-of-range frame RESETS to 0 (R_AliasSetupFrame), it
        // does NOT clamp to the last frame.
        let reset = mdl_frame_verts(&mdl, 999, 0.0).expect("reset frame present");
        assert_eq!(reset, f0, "out-of-range frame should reset to frame 0");

        // A frameless model yields None (no pose to draw).
        let mut empty = two_frame_mdl();
        empty.frames.clear();
        assert!(mdl_frame_verts(&empty, 0, 0.0).is_none());

        // A group frame resolves to its first sub-pose at time 0 and to the next
        // sub-pose as time advances past the first interval.
        {
            use crate::mdl::{AliasFrame, Frame, TriVertex as TV};
            let mut grouped = two_frame_mdl();
            let sub0 = AliasFrame {
                name: "g0".into(),
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                verts: vec![
                    TV { v: [7, 0, 0], lightnormalindex: 0 },
                    TV { v: [8, 0, 0], lightnormalindex: 0 },
                    TV { v: [9, 0, 0], lightnormalindex: 0 },
                ],
            };
            let sub1 = AliasFrame {
                name: "g1".into(),
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                verts: vec![
                    TV { v: [50, 0, 0], lightnormalindex: 0 },
                    TV { v: [51, 0, 0], lightnormalindex: 0 },
                    TV { v: [52, 0, 0], lightnormalindex: 0 },
                ],
            };
            grouped.frames = vec![Frame::Group {
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                intervals: vec![0.1, 0.2],
                frames: vec![sub0, sub1],
            }];
            // time in [0,0.1) -> sub-pose 0; time in [0.1,0.2) -> sub-pose 1.
            let g0 = mdl_frame_verts(&grouped, 0, 0.05).expect("group sub-pose 0");
            assert_eq!(g0[0], TriVertex { v: [7, 0, 0], lightnormalindex: 0 });
            let g1 = mdl_frame_verts(&grouped, 0, 0.15).expect("group sub-pose 1");
            assert_eq!(g1[0], TriVertex { v: [50, 0, 0], lightnormalindex: 0 });
        }
    }

    // -- Alias-model skin texturing: skin resolution + onseam texcoord math --

    #[test]
    fn mdl_skin_resolves_single_and_rejects_bad() {
        use crate::mdl::Skin;

        // tiny_mdl has skinwidth=1, skinheight=1 and a 1-byte single skin.
        let mut mdl = tiny_mdl();
        let sk = mdl_skin(&mdl, 0, 0.0).expect("1x1 single skin resolves");
        assert_eq!((sk.width, sk.height), (1, 1));
        assert_eq!(sk.pixels.len(), 1);

        // A 2x2 single skin with the right number of pixels resolves.
        mdl.header.skinwidth = 2;
        mdl.header.skinheight = 2;
        mdl.skins = vec![Skin::Single(vec![1, 2, 3, 4])];
        let sk = mdl_skin(&mdl, 0, 0.0).expect("2x2 single skin resolves");
        assert_eq!((sk.width, sk.height), (2, 2));
        assert_eq!(sk.pixels, &[1, 2, 3, 4]);
        // An out-of-range skinnum resets to 0 (it does not reject the model).
        let sk_oob = mdl_skin(&mdl, 99, 0.0).expect("out-of-range skinnum resets to 0");
        assert_eq!(sk_oob.pixels, &[1, 2, 3, 4]);

        // Too few pixels for the claimed dimensions -> None (flat fallback).
        mdl.skins = vec![Skin::Single(vec![1, 2, 3])];
        assert!(mdl_skin(&mdl, 0, 0.0).is_none(), "short pixel buffer must be rejected");

        // Non-positive dimensions -> None.
        let mut zero = tiny_mdl();
        zero.header.skinwidth = 0;
        assert!(mdl_skin(&zero, 0, 0.0).is_none(), "zero skinwidth must be rejected");

        // No skins at all -> None.
        let mut noskin = tiny_mdl();
        noskin.skins.clear();
        assert!(mdl_skin(&noskin, 0, 0.0).is_none(), "skinless model must be rejected");

        // A skin GROUP animates by time: sub-skin 0 for t<0.1, sub-skin 1 after.
        let mut grouped = tiny_mdl();
        grouped.header.skinwidth = 2;
        grouped.header.skinheight = 1;
        grouped.skins = vec![Skin::Group { intervals: vec![0.1, 0.2], frames: vec![vec![5, 6], vec![7, 8]] }];
        let sk = mdl_skin(&grouped, 0, 0.05).expect("skin group resolves at t=0.05");
        assert_eq!((sk.width, sk.height), (2, 1));
        assert_eq!(sk.pixels, &[5, 6], "group sub-skin 0 at t=0.05");
        let sk2 = mdl_skin(&grouped, 0, 0.15).expect("skin group resolves at t=0.15");
        assert_eq!(sk2.pixels, &[7, 8], "group sub-skin 1 at t=0.15 (animates)");
    }

    /// A `tiny_mdl` variant carrying a 2x2 skin whose four texels map to four
    /// distinct, vivid palette colours, with stverts spread across the skin so
    /// the rasteriser actually samples more than one texel.
    fn skinned_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{Skin, StVert};
        let mut mdl = tiny_mdl();
        mdl.header.skinwidth = 2;
        mdl.header.skinheight = 2;
        // Texel indices 1,2,3 (palette entries set vividly in the test).
        mdl.skins = vec![Skin::Single(vec![1, 2, 3, 1])];
        // Spread the three triangle vertices to three corners of the 2x2 skin.
        mdl.stverts =
            vec![StVert { onseam: 0, s: 0, t: 0 }, StVert { onseam: 0, s: 1, t: 0 }, StVert { onseam: 0, s: 0, t: 1 }];
        mdl
    }

    #[test]
    fn render_scene_skinned_differs_from_flat() {
        // A model with a real skin must render differently from the same model
        // forced down the flat-colour path (skin removed), proving the skin is
        // sampled rather than ignored.
        let bsp = demo_room();

        // Greys (the flat walls), and vivid, distinct colours for the skin's
        // texel indices.
        let mut pal = fixtures::ramp_palette();
        pal[1] = [255, 0, 0];
        pal[2] = [0, 255, 0];
        pal[3] = [0, 0, 255];

        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        // Skinned model.
        let skinned = skinned_mdl();
        let inst_skin = ModelInstance {
            mdl: &skinned,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img_skin =
            render_once(&Scene { models: std::slice::from_ref(&inst_skin), ..Scene::new(&bsp, cam, 160, 120, &pal) });

        // Same model/instance but with the skin stripped -> flat fallback path.
        let mut flat = skinned_mdl();
        flat.skins.clear();
        let inst_flat = ModelInstance {
            mdl: &flat,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img_flat =
            render_once(&Scene { models: std::slice::from_ref(&inst_flat), ..Scene::new(&bsp, cam, 160, 120, &pal) });

        let changed = img_skin.pixels.iter().zip(img_flat.pixels.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "skinned model should differ from flat-colour model");

        // The skinned render must actually show one of the skin's texels (no
        // colormap: drawn raw), confirming the skin pixels are sampled.
        let shows_skin_color = img_skin.pixels.iter().any(|p| [1, 2, 3].contains(p));
        assert!(shows_skin_color, "expected a sampled skin colour in the skinned render");
    }

    #[test]
    fn render_scene_skinless_model_still_draws() {
        // A model without a skin must still draw (flat fallback), unchanged from
        // the pre-skin behaviour: placing it in front of the camera alters pixels.
        let bsp = demo_room();
        let pal = fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));

        let mut mdl = tiny_mdl();
        mdl.skins.clear(); // no usable skin -> flat path
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let with_model =
            render_once(&Scene { models: std::slice::from_ref(&inst), ..Scene::new(&bsp, cam, 160, 120, &pal) });
        let changed = world_only.pixels.iter().zip(with_model.pixels.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "skinless model must still draw via the flat fallback");
    }

    #[test]
    fn render_scene_frame_selection_changes_pixels() {
        // Two instances differing only in `frame` must render differently when
        // the two frames carry distinct geometry.
        let bsp = demo_room();
        let pal = fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let mdl = two_frame_mdl();

        let inst0 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let inst1 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 1,
            blend: None,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img0 =
            render_once(&Scene { models: std::slice::from_ref(&inst0), ..Scene::new(&bsp, cam, 160, 120, &pal) });
        let img1 =
            render_once(&Scene { models: std::slice::from_ref(&inst1), ..Scene::new(&bsp, cam, 160, 120, &pal) });
        let changed = img0.pixels.iter().zip(img1.pixels.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "different frames should produce different images");
    }

    /// `r_lerpmodels`: `AliasSetup::final_vert` with a blend, checked at the
    /// vertex level (no rasteriser involved) against hand-computed values —
    /// the Proof the brief asks for ("at half the interval the vertices are
    /// the midpoint").
    #[test]
    fn final_vert_blend_interpolates_position_and_light() {
        use crate::mdl::{StVert, TriVertex};
        // The identity transform (no rotation, no translation): the "view
        // space" position a final vert reads out is the blended model-space
        // position unchanged, so the midpoint is exact to compare.
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let setup = AliasSetup {
            transform: identity,
            r_ambientlight: 100,
            r_shadelight: 80.0,
            plightvec: [0.0, 0.0, 1.0],
            ziscale: 1.0,
            subdiv: false,
            skin: None,
            skinwidth: 0,
            seamfixup: 0,
            colormap: None,
            flat: 0,
        };
        // Normal 5 faces the light vector exactly ([0,0,1]): ambient, unshaded.
        // Normal 84 faces exactly away from it ([0,0,-1]): fully shaded, so
        // the two give genuinely different light levels to blend between.
        let prev = TriVertex { v: [10, 20, 30], lightnormalindex: 5 };
        let cur = TriVertex { v: [50, 60, 70], lightnormalindex: 84 };
        let st = StVert { onseam: 0, s: 0, t: 0 };
        let light_at = |n: u8| setup.vertex_light(n);

        // frac 0: prev alone, byte for byte.
        let (fv0, av0) = setup.final_vert(&cur, Some((&prev, 0.0)), &st);
        assert_eq!(av0, [10.0, 20.0, 30.0]);
        assert_eq!(fv0.v[4], light_at(5));
        // frac 1: cur alone.
        let (fv1, av1) = setup.final_vert(&cur, Some((&prev, 1.0)), &st);
        assert_eq!(av1, [50.0, 60.0, 70.0]);
        assert_eq!(fv1.v[4], light_at(84));
        // frac 0.5: the midpoint of both the position and the light (not
        // whichever side's normal is "heavier" — see the module doc).
        let (fv_mid, av_mid) = setup.final_vert(&cur, Some((&prev, 0.5)), &st);
        assert_eq!(av_mid, [30.0, 40.0, 50.0], "the position at the midpoint");
        let want_light = light_at(5) + ((light_at(84) - light_at(5)) as f32 * 0.5) as i32;
        assert_eq!(fv_mid.v[4], want_light, "the light at the midpoint");
        assert_ne!(fv_mid.v[4], light_at(5));
        assert_ne!(fv_mid.v[4], light_at(84));
        // No blend (`None`, Classic or a snapped frame): `cur` alone, same as frac 1.
        let (fv_none, av_none) = setup.final_vert(&cur, None, &st);
        assert_eq!((av_none, fv_none.v[4]), (av1, fv1.v[4]));
    }

    /// `r_lerpmodels` through the real pipeline: a blended render differs
    /// from both its endpoints (an actual blend, not a snap to either one).
    #[test]
    fn render_scene_blend_differs_from_both_endpoint_frames() {
        let bsp = demo_room();
        let pal = fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let mdl = two_frame_mdl();
        let inst = |frame, blend| ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame,
            blend,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let scene = |inst: &ModelInstance| {
            render_once(&Scene { models: std::slice::from_ref(inst), ..Scene::new(&bsp, cam, 160, 120, &pal) })
        };
        let img0 = scene(&inst(0, None));
        let img1 = scene(&inst(1, None));
        let mid = scene(&inst(1, Some((0, 0.5))));
        let differs = |a: &Image, b: &Image| a.pixels.iter().zip(b.pixels.iter()).any(|(x, y)| x != y);
        assert!(differs(&mid, &img0), "a blend at 0.5 must not just redraw the old frame");
        assert!(differs(&mid, &img1), "a blend at 0.5 must not just redraw the new frame");
        // At each end the blend reproduces the plain render exactly.
        let end0 = scene(&inst(1, Some((0, 0.0))));
        let end1 = scene(&inst(1, Some((0, 1.0))));
        assert_eq!(end0.pixels, img0.pixels, "frac 0 is the old frame");
        assert_eq!(end1.pixels, img1.pixels, "frac 1 is the new frame");
    }

    // -- First-person weapon viewmodel (camera-anchored, drawn on top) --------

    /// A small single-skin, single-frame MDL whose one triangle sits *forward*
    /// of the model origin (model `+X` is the gun's forward axis), so once the
    /// viewmodel anchors it to the camera basis every vertex lands in front of
    /// the near plane and the triangle actually rasterises. Coloured via a 1x1
    /// skin so it takes the textured path through `palette[7]`.
    fn viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, TriVertex, Triangle};
        // Mirror the real `v_*` weapon layout: forward along model `+X`, and
        // sitting *below* the eye (model `Z < 0`, via `scale_origin`), so posed
        // at the gun origin it lands in the lower half of the frame, like the
        // shipping weapon models. The triangle lies flat (a gun's top face): a
        // vertical one through the eye's own column would be seen edge-on.
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            scale: [1.0, 1.0, 1.0],
            scale_origin: [10.0, -4.0, -10.0],
            boundingradius: 64.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 2,
            numframes: 1,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Decoded model space: X in [10, 26] (forward, clear of the 5-unit
        // alias clip plane), Y in [-4, 4], Z = -10 (below the eye), so the
        // triangle always rasterises.
        let verts = vec![
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [16, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [8, 8, 0], lightnormalindex: 0 },
        ];
        Mdl {
            header,
            skins: vec![Skin::Single(vec![7])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            // Two oppositely-wound triangles over the same three verts so the
            // fixture is double-sided. Real weapon models are closed solids that
            // always present a FRONT-facing triangle toward the eye; a lone
            // one-sided triangle is not, and the FIX-3 screen-space backface cull
            // would (correctly) drop it whenever its single winding faces away.
            // The reverse winding keeps the fixture visible from either side,
            // mirroring a real model, so these view-anchoring tests still probe
            // placement rather than an artefact of one-sided test geometry.
            triangles: vec![
                Triangle { facesfront: 1, vertindex: [0, 1, 2] },
                Triangle { facesfront: 1, vertindex: [0, 2, 1] },
            ],
            frames: vec![Frame::Single(AliasFrame {
                name: "v0".into(),
                bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TriVertex { v: [16, 8, 0], lightnormalindex: 0 },
                verts,
            })],
        }
    }

    /// The viewmodel's skin texel ([`viewmodel_mdl`]): drawn raw (no colormap).
    const GUN: u8 = 7;

    /// The tests' palette: greys (index `i` is `[i, i, i]`, so the flat walls
    /// shade to greys) with the gun's index a pure yellow no grey is near.
    fn gun_palette() -> Palette {
        let mut pal = fixtures::ramp_palette();
        pal[usize::from(GUN)] = [255, 255, 0];
        pal
    }

    /// True for a pixel of the viewmodel's skin.
    fn is_gun_pixel(p: u8) -> bool {
        p == GUN
    }

    /// The bounding box (min_x, min_y, max_x, max_y) of the pixels that differ
    /// from `bg`, plus their centroid. Returns `None` when nothing was drawn.
    fn drawn_bbox(img: &Image, bg: u8) -> Option<(usize, usize, usize, usize, f32, f32)> {
        let (mut minx, mut miny, mut maxx, mut maxy) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let (mut sx, mut sy, mut n) = (0f64, 0f64, 0u64);
        for y in 0..img.h {
            for x in 0..img.w {
                if img.pixels[y * img.w + x] != bg {
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                    sx += x as f64;
                    sy += y as f64;
                    n += 1;
                }
            }
        }
        if n == 0 { None } else { Some((minx, miny, maxx, maxy, (sx / n as f64) as f32, (sy / n as f64) as f32)) }
    }

    #[test]
    fn viewmodel_is_camera_anchored_not_world_anchored() {
        // Drawing the viewmodel at two very different camera yaws must place the
        // gun in roughly the SAME lower-centre screen region both times — proving
        // it is anchored to the view, not to a world position (which would swing
        // wildly across the frame, or vanish, as the camera turns).
        let bsp = demo_room();
        let pal = gun_palette();
        let bg = 0u8;
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Two cameras at the room centre, looking in very different directions.
        let cam_a = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let cam_b = Camera { pos: [0.0, 0.0, 0.0], yaw: 137.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img_a = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &gun,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam_a.pitch, cam_a.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam_a, w, h, &pal)
        });
        let img_b = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &gun,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam_b.pitch, cam_b.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam_b, w, h, &pal)
        });

        // Isolate the gun pixels (its unique skin colour) in each frame.
        let gun_only = |img: &Image| {
            let mut g = Image::new(img.w, img.h, bg);
            for i in 0..img.pixels.len() {
                if is_gun_pixel(img.pixels[i]) {
                    g.pixels[i] = GUN;
                }
            }
            g
        };
        let ga = gun_only(&img_a);
        let gb = gun_only(&img_b);
        let (_, _, _, _, cax, cay) = drawn_bbox(&ga, bg).expect("gun visible at yaw A");
        let (_, _, _, _, cbx, cby) = drawn_bbox(&gb, bg).expect("gun visible at yaw B");

        // The gun centroid must land in the lower-centre band in BOTH frames and
        // move only a little between the two wildly different yaws.
        let cxf = w as f32 / 2.0;
        for (cx, cy) in [(cax, cay), (cbx, cby)] {
            assert!(
                (cx - cxf).abs() < w as f32 * 0.30,
                "gun should be roughly horizontally centred (cx={cx}, centre={cxf})"
            );
            assert!(cy > h as f32 * 0.5, "gun should sit in the lower half of the frame (cy={cy}, h={h})");
        }
        assert!(
            (cax - cbx).abs() < w as f32 * 0.15 && (cay - cby).abs() < h as f32 * 0.15,
            "view-anchored gun should barely move between yaws: A=({cax},{cay}) B=({cbx},{cby})"
        );
    }

    #[test]
    fn viewmodel_draws_on_top_of_a_wall() {
        // With a wall directly in front of the camera, the viewmodel must still
        // be visible: its skin colour appears in the frame even though world
        // geometry fills the same pixels. (A depth-tested-against-world gun would
        // be hidden by the near wall.)
        let bsp = demo_room();
        let pal = gun_palette();
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Stand close to the east wall (x = 256) looking straight at it (+X), so
        // a wall is right in front and would occlude a depth-tested viewmodel.
        // The gun is anchored ~40-60 units ahead, i.e. world x ~240-260, AT the
        // wall plane — depth-tested it would lose, but it must still show.
        let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Sanity: the wall actually fills the view (without the gun).
        let world = render_once(&Scene::new(&bsp, cam, w, h, &pal));
        let bg = 2u8; // r_clearcolor, the background
        let wall_pixels = world.pixels.iter().filter(|&&p| p != bg).count();
        assert!(wall_pixels > w * h / 2, "expected the wall to fill most of the view");
        // The wall must NOT itself produce gun-coloured pixels (so the assert
        // below truly measures the gun, not the wall).
        assert!(
            !world.pixels.iter().any(|&p| is_gun_pixel(p)),
            "wall-only render must not contain gun-coloured pixels"
        );

        let with_gun = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &gun,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam.pitch, cam.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam, w, h, &pal)
        });

        // The gun's pure-yellow skin (B == 0) must appear, proving it drew on top
        // of the wall rather than being depth-occluded by it.
        let shows_gun = with_gun.pixels.iter().any(|&p| is_gun_pixel(p));
        assert!(shows_gun, "weapon viewmodel must draw on top of the wall directly ahead");

        // And it changed pixels relative to the wall-only render.
        let changed = world.pixels.iter().zip(with_gun.pixels.iter()).filter(|(a, b)| a != b).count();
        assert!(changed > 0, "viewmodel changed no pixels over the wall");
    }

    #[test]
    fn viewmodel_tolerates_malformed_model() {
        // A weapon model with out-of-range triangle indices and no frames must be
        // skipped without panicking and without altering the frame.
        let bsp = demo_room();
        let pal = fixtures::ramp_palette();
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Frameless model -> draw_viewmodel returns early.
        let mut frameless = viewmodel_mdl();
        frameless.frames.clear();
        let img = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &frameless,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam.pitch, cam.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam, 80, 60, &pal)
        });
        let baseline = render_once(&Scene::new(&bsp, cam, 80, 60, &pal));
        assert_eq!(img.pixels, baseline.pixels, "frameless weapon must draw nothing");

        // Out-of-range triangle vertex index -> that triangle is skipped.
        let mut bad = viewmodel_mdl();
        bad.triangles = vec![crate::mdl::Triangle { facesfront: 1, vertindex: [0, 1, 9999] }];
        // Must not panic.
        let _ = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &bad,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam.pitch, cam.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam, 80, 60, &pal)
        });
    }

    /// A viewmodel whose geometry deliberately *straddles* the alias clip plane:
    /// in model space its forward axis (`+X`) runs from well behind the eye to
    /// well in front of it, so the grip end is nearer than `ALIAS_Z_CLIP_PLANE`
    /// and the barrel end is beyond it — exactly the authentic held-gun layout
    /// that the clip must handle.
    fn straddling_viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, TriVertex, Triangle};
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            scale: [1.0, 1.0, 1.0],
            // Model-X (forward) runs from -20 (grip, behind the eye) to +20
            // (barrel, in front). Model-Z < 0 keeps it below the eye, like a real
            // weapon; flat, like `viewmodel_mdl`.
            scale_origin: [-20.0, -4.0, -8.0],
            boundingradius: 64.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 2,
            numframes: 1,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Decoded model space: X in [-20, +20] (straddles the eye and the
        // 5-unit clip plane), Y in [-4, 4], Z = -8.
        let verts = vec![
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },  // X=-20 (behind)
            TriVertex { v: [40, 0, 0], lightnormalindex: 0 }, // X=+20 (in front)
            TriVertex { v: [20, 8, 0], lightnormalindex: 0 }, // X=0 (on the eye)
        ];
        Mdl {
            header,
            skins: vec![Skin::Single(vec![7])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            // Double-sided (both windings) so a FRONT face always greets the eye,
            // exactly as `viewmodel_mdl`.
            triangles: vec![
                Triangle { facesfront: 1, vertindex: [0, 1, 2] },
                Triangle { facesfront: 1, vertindex: [0, 2, 1] },
            ],
            frames: vec![Frame::Single(AliasFrame {
                name: "v0".into(),
                bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TriVertex { v: [40, 8, 0], lightnormalindex: 0 },
                verts,
            })],
        }
    }

    #[test]
    fn viewmodel_straddling_near_plane_is_clipped_not_dropped() {
        // A viewmodel that crosses the near plane (part behind the eye, part in
        // front) must be CLIPPED — its front part still draws SOME pixels — rather
        // than having every crossing triangle dropped whole (an old behaviour
        // that once made this port shove the gun far away). The render must not
        // panic.
        let bsp = demo_room();
        let pal = gun_palette();
        let bg = 2u8; // r_clearcolor, the background
        let (w, h) = (160usize, 120usize);
        let gun = straddling_viewmodel_mdl();
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img = render_once(&Scene {
            viewmodel: Some(Viewmodel {
                mdl: &gun,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam.pitch, cam.yaw, 0.0],
            }),
            ..Scene::new(&bsp, cam, w, h, &pal)
        });

        // (a) it drew SOME gun pixels (not all-dropped). With the old whole-tri
        // drop, every straddling triangle vanished and this would be zero.
        let gun_pixels = img.pixels.iter().filter(|&&p| is_gun_pixel(p)).count();
        assert!(gun_pixels > 0, "straddling viewmodel must be clipped and still draw pixels (got {gun_pixels})");

        // (b) the drawn pixels stay on-screen within the frame (the rasteriser
        // clamps to the framebuffer; this just confirms a non-empty drawn bbox).
        assert!(drawn_bbox(&img, bg).is_some(), "straddling viewmodel produced a visible bounding box");
    }

    /// `r_torchflicker`: a model standing by a steady torch is lit by the
    /// luxel under it as it moves — id's light with the extra off or the
    /// torch still, the floor's change with it on — on e1m2's floor before
    /// the arch's two flames (when id's pak is here).
    #[test]
    fn a_model_by_a_torch_flickers_with_the_floor() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let bsp = Bsp::parse(&pak.read_file("maps/e1m2.bsp").expect("read").expect("e1m2")).expect("parse");
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        let mut torches = super::super::torch::TorchSet::build(&bsp, 1);
        let origin = [1488.0, 1100.0, 296.0];
        let (r, hit) = super::super::light::r_light_point_hit(&bsp, origin, &styles);
        let (face, luxel) = hit.expect("a lit floor");
        let id = alias_entity_light(&bsp, origin, &styles, None, &[], false);
        assert_eq!(id.0, (r.floor() as i32).min(128));
        torches.animate(0.0, crate::server::LerpLightStyles::Smooth, crate::render::TorchFlicker::OFF);
        assert_eq!(alias_entity_light(&bsp, origin, &styles, Some(&torches), &[], false), id, "still: id's");
        let mut seen = std::collections::BTreeSet::new();
        for f in 0..400 {
            torches.animate(
                f as f32 / 40.0,
                crate::server::LerpLightStyles::Smooth,
                crate::render::TorchFlicker::STYLE,
            );
            let delta = torches.face(face).at(luxel) * styles[0];
            let (ambient, _) = alias_entity_light(&bsp, origin, &styles, Some(&torches), &[], false);
            assert_eq!(ambient, ((r + delta).floor() as i32).min(128));
            seen.insert(ambient);
        }
        assert!(
            seen.len() >= 4 && seen.iter().any(|&a| a > id.0) && seen.iter().any(|&a| a < id.0),
            "{seen:?} about {}",
            id.0
        );
    }

    #[test]
    fn alias_lighting_follows_r_drawentitiesonlist_and_setup_lighting() {
        // No lightdata: R_LightPoint is 255 -> ambient clamps to 128 and the
        // shade to 192 - 128.
        let bsp = demo_room();
        assert_eq!(alias_entity_light(&bsp, [0.0; 3], &NEUTRAL_LIGHTSTYLE_SCALES, None, &[], false), (128, 64));
        // R_AliasSetupLighting: (255 - 128) << 6, shade * 64, and the light
        // vector {-1,0,0} in the model's frame (identity here).
        let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let (amb, shade, lv) = alias_setup_lighting(128, 64, &axes);
        assert_eq!((amb, shade, lv), (8128, 4096.0, [-1.0, -0.0, 0.0]));
        // A dark model is still lit at LIGHT_MIN: (255 - 5) << 6.
        assert_eq!(alias_setup_lighting(0, 0, &axes).0, 16000);
        let setup = AliasSetup {
            transform: [[0.0; 4]; 3],
            r_ambientlight: amb,
            r_shadelight: shade,
            plightvec: lv,
            ziscale: ALIAS_ZISCALE,
            subdiv: false,
            skin: None,
            skinwidth: 0,
            seamfixup: 0,
            colormap: None,
            flat: 0,
        };
        // Normal 52 is +x, straight into the light: ambient - shadelight.
        assert_eq!(setup.vertex_light(52), 8128 - 4096);
        // Normal 0 faces away (cos > 0): just the ambient.
        assert_eq!(setup.vertex_light(0), 8128);
    }

    #[test]
    fn hor_plus_keeps_the_gun_at_the_4_3_size() {
        // R_DrawViewModel tests the fov CVAR (r_fov_greater_than_90): Hor+
        // widens a 16:9 view to 106 degrees and the gun stays, drawn as a 4:3
        // screen of the same height draws it — the same size, and 24 columns
        // further right in a view 48 wider. A Classic fov over 90 has no gun.
        use crate::render::{FovMode, RenderOptions, VideoCvars};
        let bsp = demo_room();
        let pal = gun_palette();
        let gun = viewmodel_mdl();
        let draw_as = |w: usize, h: usize, fov_deg: f32, video: VideoCvars| {
            let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg };
            let vm = Viewmodel {
                mdl: &gun,
                frame: 0,
                blend: None,
                origin_ofs: [0.0, 0.0, 2.0],
                angles: [cam.pitch, cam.yaw, 0.0],
            };
            let options = RenderOptions { video, ..RenderOptions::default() };
            let img = render_once(&Scene { viewmodel: Some(vm), options, ..Scene::new(&bsp, cam, w, h, &pal) });
            let gun_px: Vec<(usize, usize)> =
                (0..w * h).filter(|&i| is_gun_pixel(img.pixels[i])).map(|i| (i % w, i / w)).collect();
            let (x0, x1) = (gun_px.iter().map(|p| p.0).min(), gun_px.iter().map(|p| p.0).max());
            let (y0, y1) = (gun_px.iter().map(|p| p.1).min(), gun_px.iter().map(|p| p.1).max());
            Some((x0?, y0?, x1?, y1?))
        };
        let draw = |w, h, fov_deg| draw_as(w, h, fov_deg, VideoCvars::CLASSIC);
        let four_three = draw(144, 108, 90.0).expect("gun at 4:3");
        let hor_plus = VideoCvars { fov_mode: FovMode::HorPlus, ..VideoCvars::CLASSIC };
        let wide = draw_as(192, 108, 90.0, hor_plus).expect("gun under Hor+ at 16:9");
        assert_eq!(
            (wide.0, wide.1, wide.2, wide.3),
            (four_three.0 + 24, four_three.1, four_three.2 + 24, four_three.3),
            "4:3 {four_three:?}, 16:9 {wide:?}"
        );
        assert_eq!(draw(192, 108, 100.0), None, "fov 100: no gun");
    }

    #[test]
    fn hor_plus_keeps_the_4_3_alias_transition() {
        // r_aliastransition scales with sqrt(width*height)/fov: under Hor+ a
        // model changes drawing path at the distance the 4:3 view of the same
        // height has, as it is drawn at that view's size.
        let cam = Camera { pos: [0.0; 3], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let wide = Camera { fov_deg: crate::render::FovMode::HorPlus.fov_x(90.0, 1920, 1080, 1.0), ..cam };
        let (a, b) = (
            AliasView::new(&wide, 90.0, &ViewGeom::whole(1920, 1080), 1.0),
            AliasView::new(&cam, 90.0, &ViewGeom::whole(1440, 1080), 1.0),
        );
        assert!((a.transition - b.transition).abs() < 1e-2 && (a.resfudge - b.resfudge).abs() < 1e-2);
        assert!((a.xscale - b.xscale).abs() < 1e-2);
        // id's own at 320x152: res_scale 1, transition 200.
        assert_eq!(AliasView::new(&cam, 90.0, &ViewGeom::whole(320, 152), 1.0).transition, 200.0);
    }
}
