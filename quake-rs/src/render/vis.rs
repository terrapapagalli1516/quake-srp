//! The view leaf: `Mod_PointInLeaf`.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/model.c` (`Mod_PointInLeaf`; `Mod_DecompressVis` lives with
//! the model loader, [`crate::bsp::decompress_vis`], since the server's fat PVS
//! needs it too). The PVS marking (`R_MarkLeaves`) and the view's clip planes
//! are the edge renderer's ([`super::edge`]).
//!
//! Every index into BSP-derived data is checked; malformed data gives `None`
//! rather than a panic.

use crate::bsp::Bsp;
use crate::math::{Vec3, dot};

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
        let child = if d > 0.0 { *node.children.first()? } else { *node.children.get(1)? };
        node_index = child as i32;
    }

    // Exceeded the step guard: treat as malformed.
    None
}

/// `r_viewleaf->contents`: `R_SetupFrame` (r_misc.c) finds the view's leaf
/// with `Mod_PointInLeaf` and reads its contents, for `r_dowarp` (`<=
/// CONTENTS_WATER`: water, slime, lava and the currents) and for
/// `V_SetContentsColor`'s tint. That is the render tree's rule, not the
/// server's: a point exactly on a plane is in the back child here (the front
/// only when `d > 0`) and in the front one for `SV_HullPointContents` (the
/// back only when `d < 0`, [`crate::world::point_contents`]). So an eye on a
/// pool's surface plane is under water to id's client, warped and tinted,
/// and in the air to its server. A malformed tree reads as solid: no warp,
/// no tint.
pub fn view_contents(bsp: &Bsp, p: Vec3) -> i32 {
    point_in_leaf(bsp, p).and_then(|leaf| bsp.leafs.get(leaf)).map_or(crate::bsp::CONTENTS_SOLID, |l| l.contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::decompress_vis;
    use crate::render::demo_room;

    // -- decompress_vis / point_in_leaf ---------------------------------------

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
        bsp.planes = vec![DPlane { normal: [1.0, 0.0, 0.0], dist: 0.0, ptype: crate::bsp::PLANE_X }];
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
    fn the_view_on_a_plane_is_behind_it_where_the_server_is_in_front() {
        // Water behind the plane (x < 0), air in front: on the plane the
        // client's view is in the water and the server's point in the air.
        let mut bsp = two_leaf_bsp();
        bsp.leafs[2].contents = crate::bsp::CONTENTS_WATER;
        let on = [0.0, 5.0, -3.0];
        assert_eq!(view_contents(&bsp, on), crate::bsp::CONTENTS_WATER);
        assert_eq!(crate::world::point_contents(&bsp, on), crate::bsp::CONTENTS_EMPTY);
        assert_eq!(view_contents(&bsp, [1.0 / 32.0, 5.0, -3.0]), crate::bsp::CONTENTS_EMPTY, "V_CalcRefdef's nudge");
        // A malformed tree is solid to the view: no warp, no tint.
        bsp.models[0].headnode = [999, 0, 0, 0];
        assert_eq!(view_contents(&bsp, on), crate::bsp::CONTENTS_SOLID);
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
}
