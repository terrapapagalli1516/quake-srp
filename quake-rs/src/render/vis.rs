//! Visibility: the PVS, the view frustum, and near-plane clipping.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources: `WinQuake/model.c` (`Mod_DecompressVis`, `Mod_PointInLeaf`),
//! `WinQuake/r_main.c` (`R_MarkLeaves`, the `R_SetFrustum` planes) and the
//! `R_CullBox` test.

use crate::bsp::{decompress_vis, Bsp};
use crate::math::{dot, Vec3};
use super::Camera;

// ---------------------------------------------------------------------------
// PVS culling (potentially-visible set)
// ---------------------------------------------------------------------------
//
// Ports three pieces of Quake's visibility pipeline:
//   * `Mod_DecompressVis` (model.c): run-length-decode the per-leaf PVS bitset
//     (it lives with the model loader, [`crate::bsp::decompress_vis`], since
//     the server's fat PVS needs it too).
//   * `Mod_PointInLeaf` (model.c): walk the BSP node tree to the leaf a point
//     falls in.
//   * the leaf-marking core of `R_MarkLeaves` (r_main.c): expand the PVS into a
//     per-face "visible" set via each visible leaf's marksurfaces.
//
// As everywhere in this module, every index into BSP-derived data is checked;
// malformed data degrades to "draw everything" (the safe, non-culling default)
// rather than panicking.

/// Walk the worldmodel's BSP node tree to find which leaf the world-space point
/// `p` falls in, porting `Mod_PointInLeaf`.
///
/// Starts at `models[0].headnode[0]` (a node index). At each node the point is
/// classified against the node's plane: `dot(normal, p) - dist > 0` takes
/// `children[0]` (front), otherwise `children[1]` (back; on-plane goes back, per
/// C `Mod_PointInLeaf`). A *negative* child
/// encodes a leaf as `-(child) - 1`; a non-negative child is the next node.
///
/// Returns the leaf index, or `None` if the model/headnode/plane/child indices
/// are malformed or out of range (every access is bounds-checked, so this never
/// panics on corrupt data). A bounded iteration guard prevents a cyclic/corrupt
/// node graph from looping forever.
///
/// `pub` because the sound front-end also needs the VIEW leaf each frame: the
/// four automatic ambient channels read `leaf.ambient_level[]` at the listener
/// position (`S_UpdateAmbientSounds` -> `Mod_PointInLeaf`); see [`crate::snd`].
pub fn point_in_leaf(bsp: &Bsp, p: Vec3) -> Option<usize> {
    let model = bsp.models.first()?;
    // headnode[0] is the rendering hull's root node index.
    let mut node_index: i32 = *model.headnode.first()?;

    // A valid descent visits at most `nodes.len()` nodes; cap iterations a bit
    // above that to defend against a malformed (cyclic) node graph.
    let max_steps = bsp.nodes.len().saturating_add(1);
    for _ in 0..=max_steps {
        if node_index < 0 {
            // Leaf: leaf index = -(node_index) - 1.
            let leaf = (-1 - node_index) as i64; // node_index < 0 => non-negative
            let leaf_index: usize = leaf.try_into().ok()?;
            // Confirm it is a real leaf so callers can index `bsp.leafs` safely.
            if leaf_index < bsp.leafs.len() {
                return Some(leaf_index);
            }
            return None;
        }

        let ni: usize = node_index.try_into().ok()?;
        let node = bsp.nodes.get(ni)?;
        let pi: usize = (node.planenum as i64).try_into().ok()?;
        let plane = bsp.planes.get(pi)?;

        let d = dot(plane.normal, p) - plane.dist;
        // front (child[0]) only when strictly in front; the exactly-on-plane case
        // (d == 0) goes to the back child, matching C `Mod_PointInLeaf` (`if (d > 0)`)
        // and the sibling recursive_light_point descent.
        let child = if d > 0.0 {
            *node.children.first()?
        } else {
            *node.children.get(1)?
        };
        node_index = child as i32;
    }

    // Exceeded the step guard: treat as malformed.
    None
}

/// Build a per-face visibility mask for the camera at `cam_pos`, porting the
/// leaf-marking core of `R_MarkLeaves`.
///
/// Returns `None` (meaning "draw everything, no culling") when there is no
/// usable PVS for the camera: an empty visibility lump, no leafs, the camera
/// resolving to leaf 0 (the solid/outside leaf), or a malformed BSP. Otherwise
/// returns a `Vec<bool>` of length `faces.len()` where `true` marks a face that
/// must be drawn.
///
/// Faces reached through visible leaves' `marksurfaces` are marked visible. Any
/// face *not* referenced by some leaf's marksurfaces (e.g. submodel faces, which
/// belong to brush entities rather than the worldmodel's leaves) is left visible
/// too, so submodels always draw. Out-of-range marksurface/leaf indices are
/// skipped harmlessly (they simply fail to mark, never panic).
pub(super) fn compute_visible_faces(bsp: &Bsp, cam_pos: Vec3) -> Option<Vec<bool>> {
    if bsp.visibility.is_empty() || bsp.leafs.is_empty() || bsp.faces.is_empty() {
        return None;
    }

    let view_leaf = point_in_leaf(bsp, cam_pos)?;
    // Leaf 0 is the solid/outside leaf (no PVS) — draw everything.
    if view_leaf == 0 {
        return None;
    }
    let leaf = bsp.leafs.get(view_leaf)?;
    if leaf.visofs < 0 {
        // This leaf carries no vis info — draw everything.
        return None;
    }

    // numleafs for the PVS is the visible-leaf count (leaves 1..=numleafs).
    let numleafs = bsp.leafs.len().saturating_sub(1);
    let vis = decompress_vis(&bsp.visibility, leaf.visofs, numleafs);

    // Start by marking every face that no leaf claims (submodels etc.) visible,
    // and every leaf-owned face not-visible; then re-mark the PVS-visible ones.
    // We discover "leaf-owned" faces in the same pass: a face becomes leaf-owned
    // the first time any leaf's marksurfaces references it.
    let nfaces = bsp.faces.len();
    let mut leaf_owned = vec![false; nfaces];
    let mut visible = vec![false; nfaces];

    for (li, lf) in bsp.leafs.iter().enumerate() {
        let first = lf.firstmarksurface as usize;
        let count = lf.nummarksurfaces as usize;
        let end = match first.checked_add(count) {
            Some(e) => e,
            None => continue,
        };
        // Slice the marksurfaces span for this leaf; out-of-range spans are
        // skipped (the leaf simply contributes no marks).
        let marks = match bsp.marksurfaces.get(first..end) {
            Some(m) => m,
            None => continue,
        };
        // Is this leaf in the PVS of the view leaf? (Leaf 0 / out-of-range -> no.)
        let leaf_visible = vis.get(li).copied().unwrap_or(false);
        for &ms in marks {
            let fi = ms as usize;
            if let Some(owned) = leaf_owned.get_mut(fi) {
                *owned = true;
            }
            if leaf_visible {
                if let Some(v) = visible.get_mut(fi) {
                    *v = true;
                }
            }
        }
    }

    // Any face never owned by a leaf (submodel faces) draws unconditionally.
    for fi in 0..nfaces {
        if !leaf_owned.get(fi).copied().unwrap_or(true) {
            if let Some(v) = visible.get_mut(fi) {
                *v = true;
            }
        }
    }

    Some(visible)
}

/// A brush face vertex in *view space* (`vx`/`vy`/`vz` along the camera's
/// right/up/forward axes) carrying the per-vertex texture coordinates `(s, t)`.
///
/// All five fields are *affine* functions of the world-space position, so along
/// a straight polygon edge they interpolate linearly with the *same* parameter.
/// That is what makes near-plane clipping a plain componentwise lerp: the
/// clipped vertex's `(vx, vy, vz, s, t)` is the lerp of the edge endpoints, and
/// projecting it afterwards yields the perspective-correct screen point.
#[derive(Clone, Copy)]
pub(super) struct VView {
    pub(super) vx: f32,
    pub(super) vy: f32,
    pub(super) vz: f32,
    pub(super) s: f32,
    pub(super) t: f32,
}

/// The near-clip plane, `vz == NEAR`; a vertex is *inside* iff `vz > NEAR`. Must
/// match the `NEAR` used by the draw passes (1.0).
const NEAR_PLANE: f32 = 1.0;

/// Componentwise lerp of two view-space vertices by `alpha` in `[0, 1]`
/// (`a` at 0, `b` at 1). Because every field is affine in world position, this
/// is the exact value of the attribute at the lerped world point.
fn vview_lerp(a: &VView, b: &VView, alpha: f32) -> VView {
    VView {
        vx: a.vx + (b.vx - a.vx) * alpha,
        vy: a.vy + (b.vy - a.vy) * alpha,
        vz: a.vz + (b.vz - a.vz) * alpha,
        s: a.s + (b.s - a.s) * alpha,
        t: a.t + (b.t - a.t) * alpha,
    }
}

/// Sutherland–Hodgman clip of a single convex/planar polygon (given in view
/// space) against the one near plane `vz >= NEAR_PLANE`.
///
/// A vertex is *inside* iff `vz > NEAR_PLANE`. Walking each edge `(cur, next)`
/// (with `next` wrapping to the first vertex), the output keeps `cur` when it is
/// inside and emits the near-plane crossing vertex whenever `cur` and `next` lie
/// on opposite sides of `vz == NEAR_PLANE`. The crossing parameter for an edge
/// `A -> B` is `alpha = (NEAR_PLANE - A.vz) / (B.vz - A.vz)`, and the new vertex
/// is the [`vview_lerp`] of `A`/`B` by `alpha` — so its `vz` becomes exactly
/// `NEAR_PLANE` and its `(vx, vy, s, t)` are the matching linear interpolations.
///
/// Behaviour at the extremes (important for *no* regression on the common case):
///  * A polygon **fully inside** (every `vz > NEAR_PLANE`) is returned with its
///    vertices **unchanged and in the same order** — no crossing is ever emitted,
///    so the result is byte-identical to the unclipped input.
///  * A polygon **fully behind** (every `vz <= NEAR_PLANE`) yields no inside
///    vertices and no crossings, so an empty (`< 3`) result is returned and the
///    caller skips the face.
///
/// Convenience wrapper: clip against the near plane into a fresh `Vec` (for cold
/// paths and tests). The per-face hot paths call [`clip_poly_near_into`] with a
/// reused scratch buffer instead.
#[cfg(test)]
fn clip_poly_near(input: &[VView]) -> Vec<VView> {
    let mut out = Vec::new();
    clip_poly_near_into(input, &mut out);
    out
}

/// Clip `input` against the near plane, writing the result into `out` (cleared
/// first). `out` is a caller-owned scratch buffer reused across faces so the
/// overwhelmingly common per-face call allocates nothing. The vertices written
/// are byte-identical to the previous return-a-fresh-`Vec` version.
pub(super) fn clip_poly_near_into(input: &[VView], out: &mut Vec<VView>) {
    clip_poly_plane_into(input, NEAR_PLANE, out);
}

/// [`clip_poly_near_into`] against an arbitrary view-space plane `vz >= near`
/// (the alias clip plane, `ALIAS_Z_CLIP_PLANE`, for the viewmodel). The same
/// arithmetic: with `near == NEAR_PLANE` the output is byte-identical.
fn clip_poly_plane_into(input: &[VView], near: f32, out: &mut Vec<VView>) {
    out.clear();
    let n = input.len();
    if n == 0 {
        return;
    }
    // Fast path: a polygon entirely in front of the near plane is copied
    // unchanged (same vertices, same order). This keeps the overwhelmingly
    // common case a verbatim copy, guaranteeing no rasteriser regression.
    if input.iter().all(|v| v.vz > near) {
        out.extend_from_slice(input);
        return;
    }
    out.reserve(n + 1);
    for i in 0..n {
        let cur = &input[i];
        let next = &input[(i + 1) % n];
        let cur_in = cur.vz > near;
        let next_in = next.vz > near;
        if cur_in {
            out.push(*cur);
        }
        // Emit a crossing vertex whenever the edge straddles the plane. The
        // denominator is non-zero precisely because the endpoints differ in
        // inside-ness, hence differ in `vz`. The crossing is always computed
        // from the INSIDE endpoint toward the outside one, whichever way the
        // polygon walks the edge: two faces sharing a clipped edge walk it in
        // opposite directions, and must get the same bits for the new vertex,
        // or the span raster's fill rule could leave a crack between them.
        if cur_in != next_in {
            let (inside, outside) = if cur_in { (cur, next) } else { (next, cur) };
            let denom = outside.vz - inside.vz;
            if denom != 0.0 {
                let alpha = (near - inside.vz) / denom;
                out.push(vview_lerp(inside, outside, alpha));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// View-frustum culling (Quake's R_CullBox) + per-face static caches
// ---------------------------------------------------------------------------
//
// Big maps (e1m3) spend most of their time in the per-face world loop:
// reconstructing each visible face's polygon, projecting it, and *rebuilding*
// its combined lightmap every frame. Two caches below cut that without changing
// a single output pixel:
//
//  * A **view frustum** (4 side planes + near) derived from the camera, used to
//    reject — *before* any polygon/projection/raster work — faces whose static
//    world-space AABB lies entirely outside the view. This is purely a SKIP
//    decision placed ahead of the existing PVS/backface/near-clip pipeline; a
//    culled face contributes zero drawn pixels, so the image is unchanged.
//  * A **per-face static-geometry cache** (world polygon, normal, centroid,
//    surface extents, AABB) computed once for the world model, plus a
//    **lightmap surface cache** (Quake's `R_BuildLightMap` cache) that reuses a
//    face's combined luxel buffer while its resolved style scales are unchanged.

/// A view frustum: four side planes (left/right/bottom/top) plus the near
/// plane, all with inward-pointing normals. A point `p` is inside the frustum
/// iff `dot(plane.normal, p) >= plane.dist` for every plane. A box is culled iff
/// it lies entirely on the *outside* (`< dist`) of some plane — i.e.
/// `box_on_plane_side(...) == 2` (the same predicate as Quake's `R_CullBox`).
pub(super) struct Frustum {
    planes: [crate::math::Plane; 5],
}

impl Frustum {
    /// Derive the frustum from the camera, the framebuffer size and the pixel
    /// aspect (`R_ViewChanged`'s `screenedge` planes, from `verticalFieldOfView`).
    ///
    /// The side planes are built to EXACTLY match the screen rectangle the
    /// rasteriser draws into. The rasteriser projects a view-space vertex
    /// `(vx, vy, vz)` (along `right`/`up`/`forward`) to
    /// `x = cx + xscale*vx/vz`, `y = cy - yscale*vy/vz` with `cx = w/2`,
    /// `cy = h/2`, `xscale = cx / tan(fov/2)`, `yscale = xscale * pixel_aspect`
    /// ([`Projection`](super::Projection)). On-screen means `0 <= x < w` and
    /// `0 <= y < h`, i.e. `|vx/vz| <= cx/xscale = tx` and `|vy/vz| <= cy/yscale =
    /// ty`. So the horizontal half-extent is `tx = tan(fov/2)` and the vertical
    /// is `ty = (cy/cx)*tx/pixel_aspect = (h/w)*tan(fov/2)/pixel_aspect`. The
    /// inward side-plane normals in VIEW coordinates are therefore:
    ///   left   `( 1, 0, tx)`  (inside: `vx + tx*vz >= 0`)
    ///   right  `(-1, 0, tx)`
    ///   bottom `( 0, 1, ty)`
    ///   top    `( 0,-1, ty)`
    /// each transformed to world space via `n = nx*right + ny*up + nz*forward`.
    /// Every plane passes through the camera origin, so `dist = dot(pos, n)`.
    /// The near plane has normal `forward`, `dist = dot(pos + forward*NEAR,
    /// forward)`, matching `clip_poly_near`'s `vz >= NEAR_PLANE` test.
    ///
    /// CONSERVATIVENESS: these planes bound precisely the angular region the
    /// rasteriser can draw to (the screen rectangle), so a face that produces
    /// any on-screen pixel has at least one vertex inside all five planes — its
    /// AABB then straddles or is inside every plane and is never culled. The
    /// normals are NOT normalised: the side/cull test only uses the SIGN of
    /// `dot(n, corner) - dist`, which a positive scale leaves unchanged, so
    /// skipping the normalise costs nothing and avoids a sqrt rounding step.
    pub(super) fn from_camera(cam: &Camera, w: usize, h: usize, pixel_aspect: f32) -> Frustum {
        let (forward, right, up) = cam.basis();
        let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
        let tan_half = half_fov.tan();
        let cxf = w as f32 / 2.0;
        let cyf = h as f32 / 2.0;
        // tx matches the rasteriser's cx/focal == tan(fov/2); guard the
        // degenerate focal (tan ~ 0) the draw path falls back on (focal = cx,
        // i.e. tx = 1.0) so the frustum stays consistent with what is drawn.
        let tx = if tan_half.abs() < 1e-6 { 1.0f32 } else { tan_half as f32 };
        // ty = (cy/cx)*tx/pixel_aspect (bit-exact at 1: x/1 == x); with cx==0
        // (zero-width) fall back to tx (the loop never runs for w==0 anyway).
        let ty = if cxf != 0.0 { (cyf / cxf) * tx / pixel_aspect } else { tx };

        // View-space inward normals (see doc comment), in (right, up, forward)
        // components.
        let view_normals: [[f32; 3]; 4] = [
            [1.0, 0.0, tx],  // left
            [-1.0, 0.0, tx], // right
            [0.0, 1.0, ty],  // bottom
            [0.0, -1.0, ty], // top
        ];

        let to_world = |n: [f32; 3]| -> Vec3 {
            [
                n[0] * right[0] + n[1] * up[0] + n[2] * forward[0],
                n[0] * right[1] + n[1] * up[1] + n[2] * forward[1],
                n[0] * right[2] + n[1] * up[2] + n[2] * forward[2],
            ]
        };

        // mplane_t-style planes (carry signbits so box_on_plane_side picks the
        // right corners). Each side plane passes through cam.pos.
        let mut planes: [crate::math::Plane; 5] = [
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
        ];
        for (i, vn) in view_normals.iter().enumerate() {
            let nw = to_world(*vn);
            planes[i] = crate::math::Plane::new(nw, dot(cam.pos, nw));
        }
        // Near plane: inward normal = forward, through `pos + forward*NEAR`.
        let near_dist = dot(cam.pos, forward) + NEAR_PLANE;
        planes[4] = crate::math::Plane::new(forward, near_dist);

        Frustum { planes }
    }

    /// `R_CullBox`: true (cull) iff the AABB `[mins, maxs]` is entirely on the
    /// outside of some frustum plane (`box_on_plane_side == 2`). Conservative:
    /// a box that straddles or is inside every plane is kept.
    #[inline]
    pub(super) fn culls(&self, mins: Vec3, maxs: Vec3) -> bool {
        for p in &self.planes {
            if crate::math::box_on_plane_side(mins, maxs, p) == 2 {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::sub;
    use crate::render::demo_room;
    use crate::render::fixtures::{lightmapped_demo_room, reset_render_caches};
    use crate::render::surf::face_geom_cached;

    // -- PVS culling: decompress_vis / point_in_leaf -----------------------

    #[test]
    // The literal leaf-index ranges ARE the assertion; iterators would obscure them.
    #[allow(clippy::needless_range_loop)]
    fn decompress_vis_rle_and_novis() {
        // Hand-built RLE stream for a map with numleafs = 20 (leaves 1..=20).
        //
        // Byte sequence:
        //   0xA5            -> leaves 1..8 from bits 1010_0101 (LSB=leaf1):
        //                      leaf1=1, leaf2=0, leaf3=1, leaf4=0,
        //                      leaf5=0, leaf6=1, leaf7=0, leaf8=1
        //   0x00 0x01       -> zero-run of 1 byte = 8 not-visible leaves (9..16)
        //   0xFF            -> leaves 17..20 all visible (only 4 consumed)
        let stream = [0xA5u8, 0x00, 0x01, 0xFF];
        let vis = decompress_vis(&stream, 0, 20);

        // Length is numleafs + 1, indexable by leaf number; leaf 0 always false.
        assert_eq!(vis.len(), 21);
        assert!(!vis[0], "leaf 0 (solid) is never visible");

        // First byte 0xA5 = 1010_0101.
        assert!(vis[1]);
        assert!(!vis[2]);
        assert!(vis[3]);
        assert!(!vis[4]);
        assert!(!vis[5]);
        assert!(vis[6]);
        assert!(!vis[7]);
        assert!(vis[8]);

        // Zero-run skipped leaves 9..=16.
        for l in 9..=16 {
            assert!(!vis[l], "leaf {l} should be in the zero-run (not visible)");
        }

        // Final 0xFF marks leaves 17..=20 visible.
        for l in 17..=20 {
            assert!(vis[l], "leaf {l} should be visible from the trailing 0xFF");
        }

        // visofs < 0 -> all leaves visible (Quake's mod_novis fallback).
        let all = decompress_vis(&stream, -1, 20);
        assert_eq!(all.len(), 21);
        assert!(all.iter().all(|&v| v), "no-vis fallback marks every leaf visible");
    }

    #[test]
    fn decompress_vis_truncated_is_safe() {
        // A zero byte with no following count byte must stop, not panic.
        let stream = [0x00u8];
        let vis = decompress_vis(&stream, 0, 16);
        assert_eq!(vis.len(), 17);
        // Nothing was marked visible; the decode simply stopped.
        assert!(vis.iter().all(|&v| !v));

        // An offset past the end of the lump also stops immediately (all false).
        let vis2 = decompress_vis(&stream, 99, 16);
        assert!(vis2.iter().all(|&v| !v));
    }

    /// Build a tiny BSP with exactly one splitting node and two leaves, so
    /// `point_in_leaf` has a well-defined front/back to resolve.
    ///
    /// Plane: normal +X, dist 0 (the YZ plane through the origin). `node.children`
    /// = `[-(leaf1)-1, -(leaf2)-1]` = `[-2, -3]`, so the front child (x >= 0) is
    /// leaf 1 and the back child (x < 0) is leaf 2. `models[0].headnode[0] = 0`.
    fn two_leaf_bsp() -> Bsp {
        use crate::bsp::{DLeaf, DNode, DPlane};
        let mut bsp = demo_room();
        bsp.planes = vec![DPlane {
            normal: [1.0, 0.0, 0.0],
            dist: 0.0,
            ptype: crate::bsp::PLANE_X,
        }];
        // children: front (x>=0) -> leaf index 1 => -(1)-1 = -2;
        //           back  (x<0)  -> leaf index 2 => -(2)-1 = -3.
        bsp.nodes = vec![DNode {
            planenum: 0,
            children: [-2, -3],
            mins: [0, 0, 0],
            maxs: [0, 0, 0],
            firstface: 0,
            numfaces: 0,
        }];
        // Three leaves: 0 = solid, 1 = front, 2 = back.
        let mk_leaf = |contents: i32| DLeaf {
            contents,
            visofs: -1,
            mins: [0, 0, 0],
            maxs: [0, 0, 0],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0, 0, 0, 0],
        };
        bsp.leafs = vec![
            mk_leaf(crate::bsp::CONTENTS_SOLID),
            mk_leaf(crate::bsp::CONTENTS_EMPTY),
            mk_leaf(crate::bsp::CONTENTS_EMPTY),
        ];
        if let Some(m) = bsp.models.first_mut() {
            m.headnode = [0, 0, 0, 0];
        }
        bsp
    }

    #[test]
    fn point_in_leaf_resolves_plane_sides() {
        let bsp = two_leaf_bsp();

        // A point with x > 0 is on the front side (normal +X, dist 0) -> leaf 1.
        assert_eq!(point_in_leaf(&bsp, [10.0, 0.0, 0.0]), Some(1));
        // A point with x < 0 is on the back side -> leaf 2.
        assert_eq!(point_in_leaf(&bsp, [-10.0, 0.0, 0.0]), Some(2));
        // Exactly on the plane (d == 0) goes to the BACK child (strict `d > 0`,
        // matching C Mod_PointInLeaf) -> leaf 2.
        assert_eq!(point_in_leaf(&bsp, [0.0, 5.0, -3.0]), Some(2));
    }

    #[test]
    fn point_in_leaf_malformed_is_none() {
        // headnode pointing at a non-existent node yields None, not a panic.
        let mut bsp = two_leaf_bsp();
        if let Some(m) = bsp.models.first_mut() {
            m.headnode = [999, 0, 0, 0];
        }
        assert!(point_in_leaf(&bsp, [10.0, 0.0, 0.0]).is_none());

        // A node whose child points past the leaf array also yields None.
        let mut bsp2 = two_leaf_bsp();
        if let Some(n) = bsp2.nodes.first_mut() {
            n.children = [-9999, -3]; // front child -> leaf 9998, out of range
        }
        assert!(point_in_leaf(&bsp2, [10.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn compute_visible_faces_culls_unmarked_leaves() {
        // Build on the two-leaf BSP: give the worldmodel three faces, mark face 0
        // to leaf 1 and face 1 to leaf 2, leave face 2 unowned (submodel). Vis
        // for leaf 1 sees only itself, so face 1 (leaf 2 only) must be culled,
        // while face 0 (visible leaf) and face 2 (submodel) draw.
        use crate::bsp::DFace;
        let mut bsp = two_leaf_bsp();

        // Three trivial faces (their content is irrelevant to the masking test).
        let mk_face = || DFace {
            planenum: 0,
            side: 0,
            firstedge: 0,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        };
        bsp.faces = vec![mk_face(), mk_face(), mk_face()];
        // marksurfaces: [face0, face1]; leaf1 -> {0}, leaf2 -> {1}; face2 unowned.
        bsp.marksurfaces = vec![0, 1];
        bsp.leafs[1].firstmarksurface = 0;
        bsp.leafs[1].nummarksurfaces = 1; // leaf1 owns face 0
        bsp.leafs[1].visofs = 0; // leaf1 has vis info at byte 0
        bsp.leafs[2].firstmarksurface = 1;
        bsp.leafs[2].nummarksurfaces = 1; // leaf2 owns face 1

        // PVS for leaf 1 (numleafs = 2): a single byte with only leaf 1's bit set
        // (bit 0 = leaf 1, bit 1 = leaf 2) -> 0b01 = 0x01: leaf1 visible, leaf2 not.
        bsp.visibility = vec![0x01];

        // Camera in the front half-space resolves to leaf 1.
        let mask = compute_visible_faces(&bsp, [10.0, 0.0, 0.0])
            .expect("a real leaf with vis should produce a culling mask");
        assert_eq!(mask.len(), 3);
        assert!(mask[0], "face 0 (in the visible view leaf) should draw");
        assert!(!mask[1], "face 1 (only in the culled leaf 2) should be culled");
        assert!(mask[2], "face 2 (unowned/submodel) should always draw");

        // No visibility lump -> no culling (None means draw everything).
        let mut novis = bsp.clone();
        novis.visibility = Vec::new();
        assert!(compute_visible_faces(&novis, [10.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn demo_room_pvs_is_noop() {
        // demo_room has no visibility lump, so PVS must not cull anything: the
        // textured render is identical with the culling code present.
        let bsp = demo_room();
        assert!(bsp.visibility.is_empty(), "demo_room has no vis lump");
        // compute_visible_faces returns None (no culling) for such a map.
        assert!(compute_visible_faces(&bsp, [0.0, 0.0, 0.0]).is_none());
    }

    // -----------------------------------------------------------------------
    // Near-plane polygon clipping (clip_poly_near)
    // -----------------------------------------------------------------------

    fn vv(vx: f32, vy: f32, vz: f32, s: f32, t: f32) -> VView {
        VView { vx, vy, vz, s, t }
    }

    /// (a) A polygon entirely in front of the near plane (every `vz > NEAR`) must
    /// come back UNCHANGED: same vertices, same order, byte-identical. This is the
    /// common case and any regression here would corrupt every visible wall.
    #[test]
    fn clip_poly_near_keeps_front_polygon_unchanged() {
        let poly = vec![
            vv(-2.0, -1.0, 5.0, 0.0, 0.0),
            vv(3.0, -1.0, 8.0, 64.0, 0.0),
            vv(3.0, 4.0, 8.0, 64.0, 64.0),
            vv(-2.0, 4.0, 5.0, 0.0, 64.0),
        ];
        let out = clip_poly_near(&poly);
        assert_eq!(out.len(), poly.len(), "front polygon must keep all vertices");
        for (o, p) in out.iter().zip(poly.iter()) {
            // Exact equality (no lerp should have run): bit-for-bit identical.
            assert_eq!(o.vx, p.vx);
            assert_eq!(o.vy, p.vy);
            assert_eq!(o.vz, p.vz);
            assert_eq!(o.s, p.s);
            assert_eq!(o.t, p.t);
        }
        // A vertex sitting exactly on the plane (vz == NEAR) counts as OUTSIDE
        // (inside is strictly vz > NEAR), so a polygon touching the plane is NOT
        // the trivial fast-path; but with all others in front it still clips to a
        // valid (>=3 vert) polygon.
        let touching = vec![
            vv(0.0, 0.0, NEAR_PLANE, 0.0, 0.0),
            vv(1.0, 0.0, 5.0, 10.0, 0.0),
            vv(1.0, 1.0, 5.0, 10.0, 10.0),
        ];
        let out = clip_poly_near(&touching);
        assert!(out.len() >= 3, "touching-plane triangle still clips to a polygon");
        for o in &out {
            assert!(o.vz >= NEAR_PLANE - 1e-4, "every output vertex is on/in front of NEAR");
        }
    }

    /// (b) A polygon entirely behind the near plane (every `vz <= NEAR`) yields
    /// fewer than 3 vertices, so the caller drops the face.
    #[test]
    fn clip_poly_near_drops_fully_behind_polygon() {
        let behind = vec![
            vv(-1.0, -1.0, -3.0, 0.0, 0.0),
            vv(1.0, -1.0, 0.0, 1.0, 0.0),
            vv(0.0, 1.0, NEAR_PLANE, 1.0, 1.0), // exactly on the plane = outside
        ];
        let out = clip_poly_near(&behind);
        assert!(out.len() < 3, "a fully-behind polygon must clip away (got {})", out.len());

        // An empty input is also handled (no panic, empty out).
        assert!(clip_poly_near(&[]).is_empty());
    }

    /// (c) A triangle straddling the near plane clips to a 4-vertex polygon: the
    /// two front vertices are kept verbatim and the two edges crossing the plane
    /// each contribute one new vertex with `vz == NEAR` and correctly-lerped
    /// `(vx, vy, s, t)`.
    #[test]
    fn clip_poly_near_shared_edge_gets_identical_crossing() {
        // Two faces sharing the edge P-Q (P behind the near plane, Q in front)
        // walk it in opposite directions; the crossing vertex each emits must be
        // bit-identical, or the span raster could crack along it.
        let p = vv(-3.7, 1.3, 0.21, 17.3, -4.1);
        let q = vv(5.9, -2.2, 7.9, -3.3, 11.7);
        let r = vv(9.1, 4.4, 6.3, 2.0, 2.0); // left face: P -> Q -> R
        let l = vv(-8.0, -6.1, 5.2, 1.0, 9.0); // right face: Q -> P -> L
        let a = clip_poly_near(&[p, q, r]);
        let b = clip_poly_near(&[q, p, l]);
        let on_near = |o: &[VView]| -> Vec<[u32; 5]> {
            o.iter()
                .filter(|v| v.vz == NEAR_PLANE)
                .map(|v| [v.vx.to_bits(), v.vy.to_bits(), v.vz.to_bits(), v.s.to_bits(), v.t.to_bits()])
                .collect()
        };
        // Each face's crossing on the shared edge P-Q: the near vertex whose
        // position is on segment P-Q (the other crossing is on the unshared edge).
        let shared = |o: &[VView]| {
            let alpha = (NEAR_PLANE - q.vz) / (p.vz - q.vz);
            let x = q.vx + (p.vx - q.vx) * alpha;
            on_near(o)
                .into_iter()
                .find(|bits| (f32::from_bits(bits[0]) - x).abs() < 1e-3)
                .expect("a crossing on P-Q")
        };
        assert_eq!(shared(&a), shared(&b), "shared clipped edge must give the same vertex bits");
    }

    #[test]
    fn clip_poly_near_straddling_triangle_lerps_correctly() {
        // Apex behind the plane, base in front. Numbers chosen so the crossings
        // land at simple parameters.
        //  A: behind   (vz = 0,  s=0,  t=0)
        //  B: in front (vz = 3,  s=30, t=0)
        //  C: in front (vz = 3,  s=30, t=30)
        let a = vv(0.0, 0.0, 0.0, 0.0, 0.0);
        let b = vv(6.0, 0.0, 3.0, 30.0, 0.0);
        let c = vv(6.0, 6.0, 3.0, 30.0, 30.0);
        let out = clip_poly_near(&[a, b, c]);
        assert_eq!(out.len(), 4, "an apex-behind triangle clips to a quad");

        // Sutherland–Hodgman walks edges A->B, B->C, C->A. With A outside and
        // B,C inside, the emitted ring is:
        //   edge A->B: A outside (skip A), crossing P (A->B), then keep B
        //   edge B->C: keep C
        //   edge C->A: crossing Q (C->A)
        // => [P, B, C, Q].
        //
        // Crossing on A->B at alpha = (NEAR - 0)/(3 - 0) = 1/3:
        //   vx = 0 + (6-0)*1/3 = 2, vz = NEAR = 1, s = 0 + 30/3 = 10, t = 0.
        // Crossing on C->A at alpha = (NEAR - 3)/(0 - 3) = 2/3:
        //   vx = 6 + (0-6)*2/3 = 2, vz = 1, s = 30 + (0-30)*2/3 = 10,
        //   t = 30 + (0-30)*2/3 = 10.
        let eps = 1e-5;
        // out[0] = P (A->B crossing)
        assert!((out[0].vz - NEAR_PLANE).abs() < eps, "P.vz must be NEAR");
        assert!((out[0].vx - 2.0).abs() < eps, "P.vx lerp");
        assert!((out[0].vy - 0.0).abs() < eps, "P.vy lerp");
        assert!((out[0].s - 10.0).abs() < eps, "P.s lerp");
        assert!((out[0].t - 0.0).abs() < eps, "P.t lerp");
        // out[1] = B (kept verbatim)
        assert_eq!(out[1].vx, b.vx);
        assert_eq!(out[1].vz, b.vz);
        assert_eq!(out[1].s, b.s);
        // out[2] = C (kept verbatim)
        assert_eq!(out[2].vx, c.vx);
        assert_eq!(out[2].vz, c.vz);
        assert_eq!(out[2].t, c.t);
        // out[3] = Q (C->A crossing)
        assert!((out[3].vz - NEAR_PLANE).abs() < eps, "Q.vz must be NEAR");
        assert!((out[3].vx - 2.0).abs() < eps, "Q.vx lerp");
        assert!((out[3].s - 10.0).abs() < eps, "Q.s lerp");
        assert!((out[3].t - 10.0).abs() < eps, "Q.t lerp");

        // Every output vertex is on or in front of the plane.
        for o in &out {
            assert!(o.vz >= NEAR_PLANE - eps, "clipped vertex behind NEAR: vz={}", o.vz);
        }
    }

    // -- Frustum culling (R_CullBox) ---------------------------------------

    #[test]
    fn frustum_culls_box_behind_camera_keeps_box_in_front() {
        // Camera at the origin looking down +X (yaw 0, pitch 0), 90-deg fov.
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, 320, 200, 1.0);

        // A box entirely BEHIND the camera (negative X): fully outside the near
        // plane -> culled.
        assert!(
            frustum.culls([-200.0, -10.0, -10.0], [-100.0, 10.0, 10.0]),
            "a box wholly behind the camera must be culled"
        );

        // A box straddling the near plane (spanning x = -10..50 around the eye):
        // touches the view -> NOT culled (conservative).
        assert!(
            !frustum.culls([-10.0, -10.0, -10.0], [50.0, 10.0, 10.0]),
            "a box straddling the near plane must NOT be culled"
        );

        // A box well in FRONT and centred on the view axis -> NOT culled.
        assert!(
            !frustum.culls([90.0, -10.0, -10.0], [110.0, 10.0, 10.0]),
            "a box in front of the camera must NOT be culled"
        );

        // A box far off to the LEFT (large +Y, beyond the 90-deg side at this
        // depth) is fully outside the left side plane -> culled. At x=100 the
        // left frustum edge is y=100 (tan45); a box at y in [500,600] is outside.
        assert!(
            frustum.culls([100.0, 500.0, -10.0], [110.0, 600.0, 10.0]),
            "a box outside the side frustum plane must be culled"
        );
    }

    #[test]
    fn frustum_top_and_bottom_follow_the_pixel_aspect() {
        // R_ViewChanged: verticalFieldOfView = horizontalFieldOfView /
        // screenAspect, screenAspect = width*pixelAspect/height. At 320x200 with
        // square pixels the view reaches z = 62.5 at x = 100; at id's 4:3-monitor
        // aspect 0.8333 it reaches z = 75. A box at z 66..70 is outside the first
        // and inside the second.
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let (lo, hi) = ([99.0, -5.0, 66.0], [100.0, 5.0, 70.0]);
        assert!(Frustum::from_camera(&cam, 320, 200, 1.0).culls(lo, hi));
        assert!(!Frustum::from_camera(&cam, 320, 200, 200.0 / 320.0 * 4.0 / 3.0).culls(lo, hi));
        // The sides do not move.
        let side = ([99.0, 101.0, -5.0], [100.0, 110.0, 5.0]);
        for aspect in [1.0, 0.8333333] {
            assert!(Frustum::from_camera(&cam, 320, 200, aspect).culls(side.0, side.1));
        }
    }

    #[test]
    fn frustum_never_culls_a_box_that_encloses_the_eye() {
        // A huge box around the camera straddles every plane -> never culled,
        // guaranteeing we never punch a hole when geometry surrounds the view.
        let cam = Camera { pos: [10.0, 20.0, 30.0], yaw: 35.0, pitch: -12.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, 640, 480, 1.0);
        assert!(
            !frustum.culls([-1000.0, -1000.0, -1000.0], [1000.0, 1000.0, 1000.0]),
            "a box enclosing the eye must never be culled"
        );
    }

    #[test]
    fn frustum_culled_faces_draw_no_onscreen_pixel() {
        // CONSERVATIVENESS on a concrete scene: for every world-model face the
        // frustum CULLS, confirm it could not have contributed any on-screen
        // pixel — i.e. all its (in-front-of-near) projected vertices fall outside
        // the framebuffer rectangle. This is the property that guarantees the
        // cull never punches a hole (removes a visible pixel) vs. the pre-cull
        // renderer.
        reset_render_caches();
        let bsp = lightmapped_demo_room(100, 200);
        let (w, h) = (160usize, 120usize);
        // A camera tucked in a corner looking along an axis so a good chunk of
        // the room's faces fall outside the view (some get culled).
        let cam = Camera { pos: [-240.0, -240.0, 20.0], yaw: 10.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, w, h, 1.0);

        let (forward, right, up) = cam.basis();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let tan_half = (cam.fov_deg as f64 * 0.5).to_radians().tan();
        let focal = if tan_half.abs() < 1e-6 { cx } else { (cx as f64 / tan_half) as f32 };

        let mut culled = 0usize;
        for (idx, face) in bsp.faces.iter().enumerate() {
            let geom = face_geom_cached(&bsp, idx, face);
            if geom.bad {
                continue;
            }
            if !frustum.culls(geom.mins, geom.maxs) {
                continue;
            }
            culled += 1;
            // A culled face must not project any vertex into the screen rect.
            // (Vertices behind the near plane never draw; the near plane is one of
            // the cull planes, and a face culled by a SIDE plane lies wholly to
            // that side, so every in-front vertex is off-screen on that side.)
            for v in geom.poly.iter() {
                let rel = sub(*v, cam.pos);
                let vz = dot(rel, forward);
                if vz <= NEAR_PLANE {
                    continue; // behind near -> never rasterised
                }
                let sx = cx + focal * dot(rel, right) / vz;
                let sy = cy - focal * dot(rel, up) / vz;
                let onscreen = sx >= 0.0 && sx < w as f32 && sy >= 0.0 && sy < h as f32;
                assert!(
                    !onscreen,
                    "culled face {idx} projects vertex on-screen at ({sx},{sy}) — would punch a hole"
                );
            }
        }
        assert!(culled > 0, "the test camera should cull at least one face to be meaningful");
    }
}
