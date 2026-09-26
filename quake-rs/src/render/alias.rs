//! Alias (MDL) models, the weapon included.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/r_alias.c` (`R_AliasDrawModel` and its setup, bbox and
//! lighting), `WinQuake/r_aclip.c` (`R_AliasClipTriangle`), `WinQuake/anorms.h`,
//! and `R_DrawViewModel` (`r_main.c`).

use crate::bsp::Bsp;
use crate::math::{dot, Vec3};
use super::{Camera, Image, RenderOptions};
use super::light::{r_light_point, COLORMAP_LEN, LIGHTSTYLES};
use super::polyse::PolyFramebuffer;

// ---------------------------------------------------------------------------
// Alias (MDL) models rendered into the world scene
// ---------------------------------------------------------------------------

/// One alias model placed in the world: the parsed [`Mdl`] plus its world
/// `origin`, `yaw` (degrees, rotation about `+Z`), the animation `frame` to
/// pose, and a flat base `color`.
///
/// Borrows the model so a single parsed `Mdl` (e.g. cached by name) can back
/// many instances without cloning. Rendered by [`draw_alias_model`] /
/// [`render_scene`](super::render_scene) sharing the world's z-buffer, so models occlude — and are
/// occluded by — BSP geometry correctly.
///
/// `frame` selects which pose to draw (see [`mdl_frame_verts`]); an out-of-range
/// frame resets to 0 (matching `R_AliasSetupFrame`), so any value is safe.
///
/// `skinnum` is the per-entity skin index (`currententity->skinnum`): a model
/// with multiple skins (or a skin group) uses it to pick / animate its skin, so
/// e.g. a damaged or team-coloured variant renders. Out-of-range resets to 0.
/// Defaults to `0` via [`ModelInstance::with_frame`] for callers that do not yet
/// track per-entity skins.
///
/// Group-frame (`ALIAS_GROUP`) and group-skin (`ALIAS_SKIN_GROUP`) animation is
/// driven by the **scene `time`** passed to [`render_scene_ext`](super::render_scene_ext) (not a
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
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, color, skinnum: 0 }
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
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, color, skinnum }
    }
}

/// Resolve the vertices of pose `frame` at game `time` for an [`Mdl`], delegating
/// to [`crate::mdl::Mdl::frame_pose`] (the `R_AliasSetupFrame` port).
///
/// `frame` (a `usize` here) is range-checked there: an out-of-range frame **resets
/// to 0** (matching the C, which does NOT clamp to the last frame). A
/// [`crate::mdl::Frame::Group`] now ANIMATES — its sub-pose is selected by `time`
/// against the group's intervals — so monster/torch group-frame models cycle
/// instead of freezing on the first sub-pose.
///
/// Returns `None` only when the model has no frames at all (or a group is empty).
fn mdl_frame_verts(
    mdl: &crate::mdl::Mdl,
    frame: usize,
    time: f32,
) -> Option<&[crate::mdl::TriVertex]> {
    // `usize -> i32`: a frame beyond `i32::MAX` is treated as out of range (-> 0),
    // exactly as `frame_pose` would do for any out-of-range index.
    let f = i32::try_from(frame).unwrap_or(-1);
    mdl.frame_pose(f, time)
}

/// `ALIAS_ONSEAM` flag (`modelgen.h`): the stvert lies on the texture seam that
/// separates the model skin's front half from its back half.
pub(super) const ALIAS_ONSEAM: i32 = 0x0020;

/// A usable model skin: its palette-index pixels plus dimensions, borrowed from
/// the [`Mdl`]. Resolved by [`mdl_skin`].
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
    Some(ModelSkin {
        pixels,
        width,
        height,
    })
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
    /// The alias view of a `w x h` view drawn by `cam` (its field of view
    /// the view's own) for the `fov` cvar `scr_fov`.
    fn new(cam: &Camera, scr_fov: f32, w: usize, h: usize, pixel_aspect: f32) -> AliasView {
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
            xcenter: w as f32 * 0.5 - 0.5,
            ycenter: h as f32 * 0.5 - 0.5,
            xscale,
            yscale: xscale * pixel_aspect,
            right: w as i32,
            bottom: h as i32,
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
    skinnum: i32,
    /// The flat colour for a model without a usable skin (port fallback).
    color: [u8; 3],
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
fn alias_entity_light(
    bsp: &Bsp,
    origin: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    viewmodel: bool,
) -> (i32, i32) {
    // R_LightPoint's integer (`r >>= 8` of the style-scaled sum; the float sum
    // here is exact, so its floor is that integer).
    let mut j = r_light_point(bsp, origin, light_styles).floor() as i32;
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
    pub(super) flat: [u8; 3],
}

impl AliasSetup<'_> {
    /// `R_AliasTransformFinalVert` (r_alias.c): view-space position (the
    /// auxvert), skin coordinates in 16.16, and the vertex light: the ambient
    /// offset, lowered by `shadelight * cos` where the normal faces the light.
    fn final_vert(&self, tv: &crate::mdl::TriVertex, st: &crate::mdl::StVert) -> ClipVert {
        let av = alias_transform_point(&self.transform, tv.v.map(f32::from));
        let mut fv = FinalVert { v: [0; 6], flags: st.onseam };
        fv.v[2] = st.s.wrapping_shl(16);
        fv.v[3] = st.t.wrapping_shl(16);
        fv.v[4] = self.vertex_light(tv.lightnormalindex);
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

/// Draw one alias entity: `R_AliasDrawModel` (r_alias.c). `trivial_accept`
/// comes from [`alias_check_bbox`] (always 0 for the gun, which is never
/// bbox-tested); the gun's 1/z is tripled (`ziscale * 3`) so it wins the depth
/// test against anything but a wall right against the eye.
#[allow(clippy::too_many_arguments)]
fn alias_draw_model(
    fb: &mut PolyFramebuffer,
    view: &AliasView,
    ent: &AliasEntity,
    trivial_accept: i32,
    light: (i32, i32),
    viewmodel: bool,
    time: f32,
    colormap: Option<&[u8]>,
) {
    let mdl = ent.mdl;
    let header = &mdl.header;
    // R_AliasSetupSkin: the entity's skin (a skin group animates by time).
    let skin = mdl_skin(mdl, ent.skinnum, time);
    let skinwidth = skin.as_ref().map_or(header.skinwidth.max(0), |s| s.width as i32);
    let (transform, axes) = alias_setup_transform(view, header, ent, trivial_accept);
    let (r_ambientlight, r_shadelight, plightvec) = alias_setup_lighting(light.0, light.1, &axes);
    // R_AliasSetupFrame
    let Some(verts) = mdl_frame_verts(mdl, ent.frame, time) else {
        return;
    };
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
    for (tv, st) in verts.iter().zip(mdl.stverts.iter()) {
        let (mut fv, av) = setup.final_vert(tv, st);
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
    if trivial_accept != 0 {
        // R_AliasPrepareUnclippedPoints
        if setup.subdiv {
            fb.draw_final_verts(&setup, &fverts, view);
        }
        for tri in &mdl.triangles {
            if let (Some(a), Some(b), Some(c)) = (vert(tri.vertindex[0]), vert(tri.vertindex[1]), vert(tri.vertindex[2])) {
                fb.polyset_draw(&setup, [fverts[a], fverts[b], fverts[c]], tri.facesfront != 0);
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
                fb.polyset_draw(&setup, pfv, tri.facesfront != 0);
            } else {
                alias_clip_triangle(fb, &setup, view, pfv, [aux[a], aux[b], aux[c]], tri.facesfront != 0);
            }
        }
    }
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
/// plane or a view edge, clamp the result into the view, and draw it as a fan.
fn alias_clip_triangle(
    fb: &mut PolyFramebuffer,
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
        fb.polyset_draw(setup, [poly[0].0, poly[i].0, poly[i + 1].0], facesfront);
    }
}

/// Draw one alias-model instance, as `R_DrawEntitiesOnList` does: the bounding
/// box test (`R_AliasCheckBBox`), the light at the origin plus dynamic lights,
/// then `R_AliasDrawModel`. The model shares the world's z-buffer.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_alias_model(
    image: &mut Image,
    zbuf: &mut [i16],
    bsp: &Bsp,
    cam: &Camera,
    scr_fov: f32,
    opts: &RenderOptions,
    inst: &ModelInstance,
    palette: &[[u8; 3]; 256],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
    time: f32,
    colormap: Option<&[u8]>,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let view = AliasView::new(cam, scr_fov, image.w, image.h, opts.aspect());
    let ent = AliasEntity {
        mdl: inst.mdl,
        origin: inst.origin,
        angles: [inst.pitch, inst.yaw, inst.roll],
        frame: inst.frame,
        skinnum: inst.skinnum,
        color: inst.color,
    };
    super::stats::stat(|s| s.alias_models += 1);
    let Some(trivial_accept) = alias_check_bbox(&view, &ent) else {
        return;
    };
    super::stats::stat(|s| {
        s.alias_accepted += 1;
        s.alias_tris += inst.mdl.header.numtris.max(0) as u64;
    });
    let light = alias_entity_light(bsp, inst.origin, light_styles, dlights, false);
    let mut fb = PolyFramebuffer::new(image, zbuf, palette);
    alias_draw_model(&mut fb, &view, &ent, trivial_accept, light, false, time, colormap);
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

/// The player's first-person weapon viewmodel: the parsed weapon [`Mdl`]
/// (`progs/v_shot.mdl` and friends) plus the animation `frame` to pose.
///
/// Unlike [`ModelInstance`], a viewmodel is placed relative to the camera:
/// Quake's `cl.viewent`, which V_CalcRefdef puts at the eye plus the bob and
/// viewsize fudge (`origin_ofs`) with CalcGunAngle's `angles`, drawn by
/// `R_DrawViewModel` through the ordinary alias pipeline. See
/// [`draw_viewmodel`].
///
/// `frame` selects the pose (an out-of-range frame draws frame 0, as
/// `R_AliasSetupFrame` does, see [`mdl_frame_verts`]). The model is borrowed
/// so a cached `Mdl` backs it without cloning.
pub struct Viewmodel<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub frame: usize,
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

/// Draw the first-person weapon — `R_DrawViewModel` (r_main.c): `cl.viewent`
/// posed at V_CalcRefdef's gun origin (the camera plus `origin_ofs`, see
/// [`viewmodel_origin_ofs`](super::view::viewmodel_origin_ofs)) facing along the view, lit by `R_LightPoint` at
/// that origin (at least 24) plus dynamic lights, and drawn by the same
/// `R_AliasDrawModel` as any alias model — never bbox-tested, so every
/// triangle takes the clipping path (the grip nearer than `ALIAS_Z_CLIP_PLANE`
/// is trimmed), and with its 1/z tripled so it wins the shared depth test
/// against everything but a wall right against the eye. No gun when the `fov`
/// cvar, `scr_fov`, is over 90 (`r_fov_greater_than_90`) — the cvar, not the
/// view's field of view, which Hor+ widens past 90 on a wide screen.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_viewmodel(
    image: &mut Image,
    zbuf: &mut [i16],
    bsp: &Bsp,
    cam: &Camera,
    scr_fov: f32,
    opts: &RenderOptions,
    vm: &Viewmodel,
    palette: &[[u8; 3]; 256],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
    time: f32,
    colormap: Option<&[u8]>,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    if scr_fov > 90.0 {
        return;
    }
    let view = AliasView::new(cam, scr_fov, image.w, image.h, opts.aspect());
    let origin = [cam.pos[0] + vm.origin_ofs[0], cam.pos[1] + vm.origin_ofs[1], cam.pos[2] + vm.origin_ofs[2]];
    // CalcGunAngle's angles (pitch stored "backward", i.e. +up like the camera).
    let ent = AliasEntity {
        mdl: vm.mdl,
        origin,
        angles: vm.angles,
        frame: vm.frame,
        skinnum: 0,
        color: [180, 180, 180],
    };
    let light = alias_entity_light(bsp, origin, light_styles, dlights, true);
    let mut fb = PolyFramebuffer::new(image, zbuf, palette);
    alias_draw_model(&mut fb, &view, &ent, 0, light, true, time, colormap);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{demo_room, render_scene, render_scene_ext};
    use crate::render::fixtures::tiny_mdl;
    use crate::render::light::NEUTRAL_LIGHTSTYLE_SCALES;

    #[test]
    fn render_scene_model_changes_pixels() {
        // A model placed in front of the camera must alter some pixels relative
        // to the world-only render.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        // Look from the west wall toward the centre (down +X).
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_scene(&bsp, &cam, 160, 120, &pal, &[]);

        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0], // between the camera and the centre
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let with_model = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));

        let changed = world_only
            .rgb
            .iter()
            .zip(with_model.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "model in front of camera changed no pixels");
    }

    /// Build a synthetic two-frame single-skin MDL. Frame 0 and frame 1 carry
    /// distinct vertex data so the selected pose is observable.
    fn two_frame_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
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
        grouped.skins = vec![Skin::Group {
            intervals: vec![0.1, 0.2],
            frames: vec![vec![5, 6], vec![7, 8]],
        }];
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
        mdl.stverts = vec![
            StVert { onseam: 0, s: 0, t: 0 },
            StVert { onseam: 0, s: 1, t: 0 },
            StVert { onseam: 0, s: 0, t: 1 },
        ];
        mdl
    }

    #[test]
    fn render_scene_skinned_differs_from_flat() {
        // A model with a real skin must render differently from the same model
        // forced down the flat-colour path (skin removed), proving the skin is
        // sampled rather than ignored.
        let bsp = demo_room();

        // A palette where the skin's texel indices map to vivid, distinct colours
        // unlikely to coincide with the flat instance colour after shading.
        let mut pal = [[0u8; 3]; 256];
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
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img_skin = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst_skin));

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
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img_flat = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst_flat));

        let changed = img_skin
            .rgb
            .iter()
            .zip(img_flat.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "skinned model should differ from flat-colour model");

        // The skinned render must actually show one of the skin's palette colours
        // somewhere (red/green/blue), confirming the skin pixels are sampled.
        let shows_skin_color = img_skin.rgb.iter().any(|&p| {
            // After Lambert shading the channel scales down, but a pure-channel
            // skin colour stays a pure channel (the other two channels stay 0).
            (p[0] > 0 && p[1] == 0 && p[2] == 0)
                || (p[1] > 0 && p[0] == 0 && p[2] == 0)
                || (p[2] > 0 && p[0] == 0 && p[1] == 0)
        });
        assert!(shows_skin_color, "expected a sampled skin colour in the skinned render");
    }

    #[test]
    fn render_scene_skinless_model_still_draws() {
        // A model without a skin must still draw (flat fallback), unchanged from
        // the pre-skin behaviour: placing it in front of the camera alters pixels.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_scene(&bsp, &cam, 160, 120, &pal, &[]);

        let mut mdl = tiny_mdl();
        mdl.skins.clear(); // no usable skin -> flat path
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
        let with_model = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));
        let changed = world_only
            .rgb
            .iter()
            .zip(with_model.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "skinless model must still draw via the flat fallback");
    }

    #[test]
    fn render_scene_frame_selection_changes_pixels() {
        // Two instances differing only in `frame` must render differently when
        // the two frames carry distinct geometry.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let mdl = two_frame_mdl();

        let inst0 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
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
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img0 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst0));
        let img1 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst1));
        let changed = img0
            .rgb
            .iter()
            .zip(img1.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "different frames should produce different images");
    }

    // -- First-person weapon viewmodel (camera-anchored, drawn on top) --------

    /// A small single-skin, single-frame MDL whose one triangle sits *forward*
    /// of the model origin (model `+X` is the gun's forward axis), so once the
    /// viewmodel anchors it to the camera basis every vertex lands in front of
    /// the near plane and the triangle actually rasterises. Coloured via a 1x1
    /// skin so it takes the textured path through `palette[7]`.
    fn viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
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

    /// True when a pixel looks like the viewmodel's skin: palette index 7 is set
    /// to pure yellow `[255, 255, 0]` in these tests, and the only per-pixel
    /// transform is a multiply by the (positive) Lambert `shade`. So a gun pixel
    /// keeps `B == 0` with `R > 0` and `G > 0`, whereas `hash_color` walls (HSV
    /// saturation 0.55) always have all three channels strictly positive and the
    /// background `[10,10,14]` has `B != 0`.
    fn is_gun_pixel(p: [u8; 3]) -> bool {
        p[2] == 0 && p[0] > 0 && p[1] > 0
    }

    /// The bounding box (min_x, min_y, max_x, max_y) of the pixels that differ
    /// from `bg`, plus their centroid. Returns `None` when nothing was drawn.
    fn drawn_bbox(img: &Image, bg: [u8; 3]) -> Option<(usize, usize, usize, usize, f32, f32)> {
        let (mut minx, mut miny, mut maxx, mut maxy) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let (mut sx, mut sy, mut n) = (0f64, 0f64, 0u64);
        for y in 0..img.h {
            for x in 0..img.w {
                if img.rgb[y * img.w + x] != bg {
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
        if n == 0 {
            None
        } else {
            Some((minx, miny, maxx, maxy, (sx / n as f64) as f32, (sy / n as f64) as f32))
        }
    }

    #[test]
    fn viewmodel_is_camera_anchored_not_world_anchored() {
        // Drawing the viewmodel at two very different camera yaws must place the
        // gun in roughly the SAME lower-centre screen region both times — proving
        // it is anchored to the view, not to a world position (which would swing
        // wildly across the frame, or vanish, as the camera turns).
        let bsp = demo_room();
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [255, 255, 0]; // the viewmodel's skin colour (index 7), pure yellow
        let bg = [10u8, 10, 14];
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Two cameras at the room centre, looking in very different directions.
        let cam_a = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let cam_b = Camera { pos: [0.0, 0.0, 0.0], yaw: 137.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img_a = render_scene_ext(
            &bsp, &cam_a, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam_a.pitch, cam_a.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let img_b = render_scene_ext(
            &bsp, &cam_b, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam_b.pitch, cam_b.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // Isolate the gun pixels (its unique skin colour) in each frame.
        let gun_only = |img: &Image| {
            let mut g = Image::new(img.w, img.h, bg);
            for i in 0..img.rgb.len() {
                if is_gun_pixel(img.rgb[i]) {
                    g.rgb[i] = [255, 255, 0];
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
            assert!(
                cy > h as f32 * 0.5,
                "gun should sit in the lower half of the frame (cy={cy}, h={h})"
            );
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
        let mut pal = [[80u8; 3]; 256]; // (walls use hash_color, not the palette)
        pal[7] = [255, 255, 0]; // distinctive pure-yellow gun colour
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Stand close to the east wall (x = 256) looking straight at it (+X), so
        // a wall is right in front and would occlude a depth-tested viewmodel.
        // The gun is anchored ~40-60 units ahead, i.e. world x ~240-260, AT the
        // wall plane — depth-tested it would lose, but it must still show.
        let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Sanity: the wall actually fills the view (without the gun).
        let world = render_scene_ext(&bsp, &cam, w, h, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let bg = [10u8, 10, 14];
        let wall_pixels = world.rgb.iter().filter(|&&p| p != bg).count();
        assert!(wall_pixels > w * h / 2, "expected the wall to fill most of the view");
        // The wall must NOT itself produce gun-coloured pixels (so the assert
        // below truly measures the gun, not the wall).
        assert!(
            !world.rgb.iter().any(|&p| is_gun_pixel(p)),
            "wall-only render must not contain gun-coloured pixels"
        );

        let with_gun = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam.pitch, cam.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // The gun's pure-yellow skin (B == 0) must appear, proving it drew on top
        // of the wall rather than being depth-occluded by it.
        let shows_gun = with_gun.rgb.iter().any(|&p| is_gun_pixel(p));
        assert!(shows_gun, "weapon viewmodel must draw on top of the wall directly ahead");

        // And it changed pixels relative to the wall-only render.
        let changed = world
            .rgb
            .iter()
            .zip(with_gun.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "viewmodel changed no pixels over the wall");
    }

    #[test]
    fn viewmodel_none_matches_no_viewmodel() {
        // Passing `None` for the viewmodel must be byte-identical to the prior
        // behaviour (render_scene_ext with the trailing arg absent in spirit).
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let a = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(a.rgb, b.rgb, "None viewmodel must equal render_scene");
    }

    #[test]
    fn viewmodel_tolerates_malformed_model() {
        // A weapon model with out-of-range triangle indices and no frames must be
        // skipped without panicking and without altering the frame.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Frameless model -> draw_viewmodel returns early.
        let mut frameless = viewmodel_mdl();
        frameless.frames.clear();
        let img = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &frameless, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam.pitch, cam.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let baseline = render_scene_ext(&bsp, &cam, 80, 60, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(img.rgb, baseline.rgb, "frameless weapon must draw nothing");

        // Out-of-range triangle vertex index -> that triangle is skipped.
        let mut bad = viewmodel_mdl();
        bad.triangles = vec![crate::mdl::Triangle { facesfront: 1, vertindex: [0, 1, 9999] }];
        // Must not panic.
        let _ = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &bad, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam.pitch, cam.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    /// A viewmodel whose geometry deliberately *straddles* the alias clip plane:
    /// in model space its forward axis (`+X`) runs from well behind the eye to
    /// well in front of it, so the grip end is nearer than `ALIAS_Z_CLIP_PLANE`
    /// and the barrel end is beyond it — exactly the authentic held-gun layout
    /// that the clip must handle.
    fn straddling_viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
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
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },   // X=-20 (behind)
            TriVertex { v: [40, 0, 0], lightnormalindex: 0 },   // X=+20 (in front)
            TriVertex { v: [20, 8, 0], lightnormalindex: 0 },   // X=0 (on the eye)
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
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [255, 255, 0]; // the viewmodel's pure-yellow skin (B == 0)
        let bg = [10u8, 10, 14];
        let (w, h) = (160usize, 120usize);
        let gun = straddling_viewmodel_mdl();
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam.pitch, cam.yaw, 0.0] }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // (a) it drew SOME gun pixels (not all-dropped). With the old whole-tri
        // drop, every straddling triangle vanished and this would be zero.
        let gun_pixels = img.rgb.iter().filter(|&&p| is_gun_pixel(p)).count();
        assert!(
            gun_pixels > 0,
            "straddling viewmodel must be clipped and still draw pixels (got {gun_pixels})"
        );

        // (b) the drawn pixels stay on-screen within the frame (the rasteriser
        // clamps to the framebuffer; this just confirms a non-empty drawn bbox).
        assert!(
            drawn_bbox(&img, bg).is_some(),
            "straddling viewmodel produced a visible bounding box"
        );
    }

    #[test]
    fn alias_lighting_follows_r_drawentitiesonlist_and_setup_lighting() {
        // No lightdata: R_LightPoint is 255 -> ambient clamps to 128 and the
        // shade to 192 - 128.
        let bsp = demo_room();
        assert_eq!(alias_entity_light(&bsp, [0.0; 3], &NEUTRAL_LIGHTSTYLE_SCALES, &[], false), (128, 64));
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
            flat: [0; 3],
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
        use crate::render::{FovMode, VideoCvars, VideoGuard};
        let bsp = demo_room();
        let mut pal = [[80u8; 3]; 256];
        pal[7] = [255, 255, 0];
        let gun = viewmodel_mdl();
        let draw = |w: usize, h: usize, fov_deg: f32| {
            let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg };
            let vm = Viewmodel { mdl: &gun, frame: 0, origin_ofs: [0.0, 0.0, 2.0], angles: [cam.pitch, cam.yaw, 0.0] };
            let img = crate::render::render_scene_ext_sprited(
                &bsp, &cam, w, h, &pal, &[], &[], &[], Some(vm), 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
                &[], &RenderOptions::default(),
            );
            let gun_px: Vec<(usize, usize)> =
                (0..w * h).filter(|&i| is_gun_pixel(img.rgb[i])).map(|i| (i % w, i / w)).collect();
            let (x0, x1) = (gun_px.iter().map(|p| p.0).min(), gun_px.iter().map(|p| p.0).max());
            let (y0, y1) = (gun_px.iter().map(|p| p.1).min(), gun_px.iter().map(|p| p.1).max());
            Some((x0?, y0?, x1?, y1?))
        };
        let four_three = draw(144, 108, 90.0).expect("gun at 4:3");
        let wide = {
            let _g = VideoGuard::set(VideoCvars { fov_mode: FovMode::HorPlus, hires: false });
            draw(192, 108, 90.0).expect("gun under Hor+ at 16:9")
        };
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
        let (a, b) = (AliasView::new(&wide, 90.0, 1920, 1080, 1.0), AliasView::new(&cam, 90.0, 1440, 1080, 1.0));
        assert!((a.transition - b.transition).abs() < 1e-2 && (a.resfudge - b.resfudge).abs() < 1e-2);
        assert!((a.xscale - b.xscale).abs() < 1e-2);
        // id's own at 320x152: res_scale 1, transition 200.
        assert_eq!(AliasView::new(&cam, 90.0, 320, 152, 1.0).transition, 200.0);
    }
}
