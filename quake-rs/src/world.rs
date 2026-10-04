//! World query: point-contents and line/box collision against the BSP.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/world.c` — `SV_HullPointContents`, `SV_PointContents`,
//! `SV_RecursiveHullCheck`, `SV_InitBoxHull`/`SV_HullForBox`, and the hull
//! selection of `SV_HullForEntity` / point/box offsetting of
//! `SV_ClipMoveToEntity`.
//!
//! ## Faithfulness and safety
//!
//! The C engine kept three precomputed clipping hulls per brush model and a
//! singleton "box hull" rebuilt per query; collision walked raw `dclipnode_t`
//! arrays whose children encoded either another clipnode index (≥ 0) or a
//! `CONTENTS_*` value (< 0), and walked the rendering `mnode_t`/`mleaf_t` tree
//! for the point hull (hull 0). Out-of-range node numbers triggered
//! `Sys_Error`.
//!
//! This port is `#![forbid(unsafe_code)]` and never panics on bad map data:
//!
//! * A single uniform [`Hull`] type is synthesized for all three world hulls
//!   ([`build_hull`]). Hull 0 is rebuilt from `bsp.nodes`, replacing each leaf
//!   child with that leaf's `contents`; hulls 1 and 2 copy `bsp.clipnodes`
//!   (whose negative children already *are* contents). Any out-of-range index
//!   encountered during the rebuild is replaced by `CONTENTS_SOLID`, so the
//!   synthesized tree always terminates.
//! * The trace and point walks index every clipnode and plane through
//!   `.get(..)`; an out-of-range node number is treated as solid contents
//!   (a conservative, terminating choice) rather than aborting.
//! * The recursion in [`recursive_hull_check`] is bounded by an explicit depth
//!   guard so a malformed (cyclic) tree cannot recurse forever.

use crate::bsp::{
    Bsp, CONTENTS_CURRENT_0, CONTENTS_CURRENT_DOWN, CONTENTS_EMPTY, CONTENTS_SOLID, CONTENTS_WATER, DPlane,
};
use crate::math::{Vec3, dot};
use crate::vm::HostTrace;

/// Re-export the VM's trace struct as the world-collision result type
/// (the engine `trace_t`: `allsolid`/`startsolid`/`inopen`/`inwater`/
/// `fraction`/`endpos`/`plane_normal`/`plane_dist`).
pub use crate::vm::HostTrace as Trace;

/// `DIST_EPSILON` — "1/32 epsilon to keep floating point happy" (world.c).
pub const DIST_EPSILON: f32 = 0.03125;

thread_local! {
    /// Monotonic count of BSP box/line traces ([`trace_world`] + [`trace_submodel`]).
    /// A free-running diagnostic the sim benchmark samples to report collision
    /// workload per frame; not used by gameplay. Single-threaded `Cell`.
    static TRACE_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Current total BSP traces performed (since process start or the last
/// [`reset_trace_count`]). Used by the sim benchmark; not gameplay state.
pub fn trace_count() -> u64 {
    TRACE_COUNT.with(|c| c.get())
}

/// Reset the [`trace_count`] running total to zero.
pub fn reset_trace_count() {
    TRACE_COUNT.with(|c| c.set(0));
}

#[inline]
fn bump_trace_count() {
    TRACE_COUNT.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Recursion-depth ceiling for [`recursive_hull_check`]. The C recursion is
/// bounded by the BSP depth; a malformed (cyclic) hull could otherwise recurse
/// without end. This generously exceeds any real Quake BSP depth.
const MAX_HULL_DEPTH: u32 = 16384;

/// One collision node, mirroring `dclipnode_t` but with the leaf/contents
/// distinction already resolved into `children`:
///
/// * `children[k] >= 0` — index of another [`ClipNode`] within the same hull.
/// * `children[k] <  0` — a `CONTENTS_*` value (the region's contents).
///
/// `planenum` indexes the shared plane slice ([`Hull::planes`]).
#[derive(Debug, Clone, Copy)]
struct ClipNode {
    planenum: u32,
    children: [i32; 2],
}

/// A self-contained clipping hull: the synthesized collision nodes, a borrow of
/// the map's plane table, the head node to start traces from, and the hull's
/// clip box (the size of the object this hull models, used to offset box
/// traces into point traces).
pub struct Hull<'a> {
    /// The synthesized clip-node table (hull 0 from `bsp.nodes`, hulls 1/2 from
    /// `bsp.clipnodes`). Behind an `Rc` and served from a per-world cache
    /// ([`HULL_CACHE`]) so the (potentially thousands-of-entry) table is built
    /// ONCE per loaded map instead of rebuilt on every trace — the sim does
    /// hundreds of traces per tick. The table is a pure, immutable function of the
    /// `Bsp`, so sharing it is byte-identical.
    clipnodes: std::rc::Rc<Vec<ClipNode>>,
    planes: &'a [DPlane],
    headnode: i32,
    clip_mins: Vec3,
    clip_maxs: Vec3,
}

impl<'a> Hull<'a> {
    /// The clip box minimum (the `clip_mins` of the C `hull_t`).
    pub fn clip_mins(&self) -> Vec3 {
        self.clip_mins
    }

    /// The clip box maximum (the `clip_maxs` of the C `hull_t`).
    pub fn clip_maxs(&self) -> Vec3 {
        self.clip_maxs
    }

    /// The head node index that traces and point queries start from.
    pub fn headnode(&self) -> i32 {
        self.headnode
    }

    /// Signed plane distance of point `p` against the plane referenced by clip
    /// node `node`. Mirrors the `plane->type < 3` axial fast path of
    /// `SV_HullPointContents` / `SV_RecursiveHullCheck`.
    ///
    /// The slanted case is `DotProduct (plane->normal, p) - plane->dist`, which
    /// id's x87 code evaluates in its 80-bit registers: three products of
    /// floats (each exact in a double) summed, then the plane's distance
    /// taken off, rounded once. Done in `f32`, every product and sum rounds,
    /// and a point lying on the plane (a 45-degree wall through a box corner)
    /// can come out on the other side: id's `droptofloor` then finds the
    /// floor where the port finds solid, or the other way round (the mission
    /// packs' hip1m1, hip2m6 and hip3m1 each have an item that hits it). So it
    /// is summed in `f64`, which gives the sign id's registers give.
    fn plane_distance(&self, plane: &DPlane, p: Vec3) -> f32 {
        // type < 3 => axial: d = p[type] - dist; else dot(normal, p) - dist.
        if plane.ptype >= 0 && plane.ptype < 3 {
            // ptype is 0,1,2 here; index is in-bounds for a Vec3. One
            // subtraction of two floats rounds without changing its sign.
            let i = plane.ptype as usize;
            p.get(i).copied().unwrap_or(0.0) - plane.dist
        } else {
            let n = plane.normal.map(f64::from);
            let p = p.map(f64::from);
            (n[0] * p[0] + n[1] * p[1] + n[2] * p[2] - f64::from(plane.dist)) as f32
        }
    }
}

/// A cheap identity for a loaded world's collision tables: the `&Bsp` address
/// plus the lengths of the lumps the hull node tables derive from. A changelevel
/// reparses the BSP (fresh address + lengths), so a mismatch reliably means the
/// cached tables are stale and must be rebuilt — the same scheme the renderer's
/// face caches use.
#[derive(Clone, Copy, PartialEq, Eq)]
struct HullFingerprint {
    ptr: usize,
    nodes_len: usize,
    clipnodes_len: usize,
    leafs_len: usize,
    planes_len: usize,
}

impl HullFingerprint {
    fn of(bsp: &Bsp) -> HullFingerprint {
        HullFingerprint {
            ptr: bsp as *const Bsp as usize,
            nodes_len: bsp.nodes.len(),
            clipnodes_len: bsp.clipnodes.len(),
            leafs_len: bsp.leafs.len(),
            planes_len: bsp.planes.len(),
        }
    }
}

/// The cached clip-node tables for one loaded world: hull 0 (synthesized from
/// `bsp.nodes`) and the brush hull (from `bsp.clipnodes`, shared by hulls 1/2 and
/// every SOLID_BSP submodel — only the head node differs per trace).
struct HullTables {
    fp: HullFingerprint,
    hull0: std::rc::Rc<Vec<ClipNode>>,
    brush: std::rc::Rc<Vec<ClipNode>>,
}

thread_local! {
    /// Per-world cached clip-node tables, rebuilt only on a world change
    /// (changelevel). Without this, every trace rebuilt the full (thousands of
    /// entries) table from scratch — and the sim does hundreds of traces per tick.
    static HULL_CACHE: std::cell::RefCell<Option<HullTables>> =
        const { std::cell::RefCell::new(None) };
}

/// Get the cached `Rc` to the clip-node table for size class `which` (0 = hull-0
/// world nodes; 1/2 = brush clipnodes), building + caching BOTH tables on the
/// first call after a world change. The tables are pure, deterministic functions
/// of `bsp`, so the shared `Rc` is byte-identical to a fresh per-call build.
fn cached_clipnodes(bsp: &Bsp, which: usize) -> std::rc::Rc<Vec<ClipNode>> {
    HULL_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        let fp = HullFingerprint::of(bsp);
        let stale = match slot.as_ref() {
            Some(t) => t.fp != fp,
            None => true,
        };
        if stale {
            *slot = Some(HullTables {
                fp,
                hull0: std::rc::Rc::new(build_hull0_clipnodes(bsp)),
                brush: std::rc::Rc::new(build_brush_clipnodes(bsp)),
            });
        }
        let t = slot.as_ref().expect("just initialised");
        if which == 0 { t.hull0.clone() } else { t.brush.clone() }
    })
}

/// Build one of the three world hulls (`which` in `0..=2`; any other value is
/// clamped to `0`, the point hull). Mirrors how Quake precomputed
/// `model->hulls[0..2]`:
///
/// * **Hull 0** (point size, clip box `(0,0,0)`): synthesized from `bsp.nodes`.
///   At node `i`, child `nodes[i].children[k]` (an `i16`) is another node index
///   when `>= 0`, or a leaf when `< 0` (leaf index `-(child) - 1`); the leaf's
///   child becomes that leaf's `contents`. Head node is `models[0].headnode[0]`.
/// * **Hull 1** (player, clip box `(-16,-16,-24)..(16,16,32)`): copies
///   `bsp.clipnodes` (children already encode contents when negative). Head node
///   is `models[0].headnode[1]`.
/// * **Hull 2** (big monster, clip box `(-32,-32,-24)..(32,32,64)`): same nodes
///   as hull 1, head node `models[0].headnode[2]`.
///
/// Every index is bounds-checked; an out-of-range index becomes
/// `CONTENTS_SOLID` so the synthesized tree always terminates.
pub fn build_hull<'a>(bsp: &'a Bsp, which: usize) -> Hull<'a> {
    let which = if which <= 2 { which } else { 0 };

    // headnode comes from model 0 (the worldspawn); default to 0 when absent.
    let headnode = bsp.models.first().and_then(|m| m.headnode.get(which).copied()).unwrap_or(0);

    // (clip box + node synthesis are handled by build_hull_for_model)
    build_hull_for_model(bsp, which, headnode)
}

/// Build a clip hull with the given `which` (clip-box size class, `0..=2`) and
/// an explicit `headnode`. This is the shared body of [`build_hull`] and the
/// SOLID_BSP submodel path ([`trace_submodel`]): hull 0 synthesizes its nodes
/// from `bsp.nodes`, hulls 1/2 copy `bsp.clipnodes`, and the clip box is the
/// fixed player/monster size. [`build_hull`] is the model-0 specialization
/// (`headnode = models[0].headnode[which]`); submodel traces pass
/// `models[N].headnode[which]` instead. Node *contents* are identical between
/// submodels (they share `bsp.clipnodes`); only the head node differs.
fn build_hull_for_model<'a>(bsp: &'a Bsp, which: usize, headnode: i32) -> Hull<'a> {
    let which = if which <= 2 { which } else { 0 };

    let (clip_mins, clip_maxs): (Vec3, Vec3) = match which {
        1 => ([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]),
        2 => ([-32.0, -32.0, -24.0], [32.0, 32.0, 64.0]),
        _ => ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
    };

    // The (large) node table is cached per-world and shared by every trace; this
    // is an O(1) refcount bump after the first build, not a full rebuild.
    let clipnodes = cached_clipnodes(bsp, which);

    Hull { clipnodes, planes: &bsp.planes, headnode, clip_mins, clip_maxs }
}

/// Synthesize hull-0 clip nodes from `bsp.nodes`. Each `i16` child that names a
/// leaf (`< 0`, leaf index `-(child) - 1`) is replaced by that leaf's contents;
/// a child naming another node (`>= 0`) is kept as a node index. Out-of-range
/// references collapse to `CONTENTS_SOLID`.
fn build_hull0_clipnodes(bsp: &Bsp) -> Vec<ClipNode> {
    let mut out = Vec::with_capacity(bsp.nodes.len());
    for node in &bsp.nodes {
        let planenum = if node.planenum >= 0 { node.planenum as u32 } else { 0 };
        let mut children = [CONTENTS_SOLID; 2];
        for (k, slot) in children.iter_mut().enumerate() {
            // node.children is [i16; 2]; index k in 0..2 is always in range.
            let c = node.children.get(k).copied().unwrap_or(-1);
            *slot = if c >= 0 {
                // Another node — keep as a node index (validated at walk time).
                i32::from(c)
            } else {
                // A leaf: leaf index = -(c) - 1; child becomes its contents.
                let leafidx = (-i32::from(c) - 1) as usize;
                bsp.leafs.get(leafidx).map(|l| l.contents).unwrap_or(CONTENTS_SOLID)
            };
        }
        out.push(ClipNode { planenum, children });
    }
    out
}

/// Copy `bsp.clipnodes` into the uniform [`ClipNode`] form. A non-negative child
/// is a clipnode index; a negative child is already a `CONTENTS_*` value and is
/// preserved verbatim (sign-extended from the on-disk `i16`).
fn build_brush_clipnodes(bsp: &Bsp) -> Vec<ClipNode> {
    let mut out = Vec::with_capacity(bsp.clipnodes.len());
    for cn in &bsp.clipnodes {
        let planenum = if cn.planenum >= 0 { cn.planenum as u32 } else { 0 };
        let children = [i32::from(cn.children[0]), i32::from(cn.children[1])];
        out.push(ClipNode { planenum, children });
    }
    out
}

/// Look up clip node `num` and resolve its plane; returns `None` (treat as
/// solid) if either index is out of range.
fn node_and_plane<'h>(hull: &'h Hull, num: i32) -> Option<(&'h ClipNode, &'h DPlane)> {
    if num < 0 {
        return None;
    }
    let node = hull.clipnodes.get(num as usize)?;
    let plane = hull.planes.get(node.planenum as usize)?;
    Some((node, plane))
}

/// `SV_HullPointContents` — return the `CONTENTS_*` value for point `p`,
/// starting the descent at clip node `num`.
///
/// Iterative (the C is a `while (num >= 0)` loop): at each node, take child 1
/// when the point is on the back side (`d < 0`) and child 0 otherwise; when
/// `num` goes negative it is the contents. A bad node/plane index is reported
/// as `CONTENTS_SOLID` rather than aborting (the C `Sys_Error`).
pub fn hull_point_contents(hull: &Hull, num: i32, p: Vec3) -> i32 {
    let mut num = num;
    // The hull has a finite number of nodes; cap iterations to that count so a
    // cyclic (malformed) tree cannot loop forever.
    let mut budget = hull.clipnodes.len().saturating_add(1);
    while num >= 0 {
        if budget == 0 {
            return CONTENTS_SOLID;
        }
        budget -= 1;

        let (node, plane) = match node_and_plane(hull, num) {
            Some(np) => np,
            None => return CONTENTS_SOLID, // bad node number -> solid (was Sys_Error)
        };

        let d = hull.plane_distance(plane, p);
        num = if d < 0.0 { node.children[1] } else { node.children[0] };
    }
    num
}

/// `SV_RecursiveHullCheck` — trace the segment `p1`..`p2` (parameterized by the
/// fractions `p1f`..`p2f` of the overall move) through the hull subtree rooted
/// at `num`, accumulating the result into `trace`. Returns `true` while the
/// move is still clear and `false` once it has been clipped (an impact or a
/// solid start).
///
/// This is a faithful port of the `#if 1` branch of the C function, including
/// the `DIST_EPSILON` near-side bias and the "backup past solid" fixup loop.
/// The C recursion is mirrored, with an explicit depth guard against malformed
/// (cyclic) trees.
pub fn recursive_hull_check(hull: &Hull, num: i32, p1f: f32, p2f: f32, p1: Vec3, p2: Vec3, trace: &mut Trace) -> bool {
    recursive_hull_check_depth(hull, num, p1f, p2f, p1, p2, trace, 0)
}

#[allow(clippy::too_many_arguments)]
fn recursive_hull_check_depth(
    hull: &Hull,
    num: i32,
    p1f: f32,
    p2f: f32,
    p1: Vec3,
    p2: Vec3,
    trace: &mut Trace,
    depth: u32,
) -> bool {
    // Guard against unbounded recursion on a malformed (cyclic) hull. Treat a
    // too-deep descent as "blocked" without writing an impact plane.
    if depth >= MAX_HULL_DEPTH {
        return false;
    }

    // ---- check for empty (leaf / contents) ----
    if num < 0 {
        if num != CONTENTS_SOLID {
            trace.allsolid = false;
            if num == CONTENTS_EMPTY {
                trace.inopen = true;
            } else {
                trace.inwater = true;
            }
        } else {
            trace.startsolid = true;
        }
        return true; // empty
    }

    // ---- find the point distances ----
    let (node, plane) = match node_and_plane(hull, num) {
        Some(np) => np,
        // C: Sys_Error("bad node number"). Stay total: treat as blocked.
        None => return false,
    };
    let children = node.children;
    let plane_normal = plane.normal;
    let plane_dist = plane.dist;

    let t1 = hull.plane_distance(plane, p1);
    let t2 = hull.plane_distance(plane, p2);

    // Both endpoints on the front side -> recurse child 0.
    if t1 >= 0.0 && t2 >= 0.0 {
        return recursive_hull_check_depth(hull, children[0], p1f, p2f, p1, p2, trace, depth + 1);
    }
    // Both endpoints on the back side -> recurse child 1.
    if t1 < 0.0 && t2 < 0.0 {
        return recursive_hull_check_depth(hull, children[1], p1f, p2f, p1, p2, trace, depth + 1);
    }

    // ---- the segment crosses the plane: split it ----
    // Put the crosspoint DIST_EPSILON on the near side.
    let denom = t1 - t2;
    let mut frac = if denom != 0.0 {
        if t1 < 0.0 { (t1 + DIST_EPSILON) / denom } else { (t1 - DIST_EPSILON) / denom }
    } else {
        // t1 == t2 cannot reach here (signs would match), but stay total.
        0.0
    };
    // C: `if (frac < 0) frac = 0; if (frac > 1) frac = 1;` — clamp is identical
    // (NaN passes through both forms unchanged).
    frac = frac.clamp(0.0, 1.0);

    let mut midf = p1f + (p2f - p1f) * frac;
    let mut mid: Vec3 =
        [p1[0] + frac * (p2[0] - p1[0]), p1[1] + frac * (p2[1] - p1[1]), p1[2] + frac * (p2[2] - p1[2])];

    // side = (t1 < 0): the side p1 is on. We descend the near side first.
    let side = (t1 < 0.0) as usize;
    let near = children[side];
    let far = children[side ^ 1];

    // Move up to the node (near side, p1..mid).
    if !recursive_hull_check_depth(hull, near, p1f, midf, p1, mid, trace, depth + 1) {
        return false;
    }

    // If the far side isn't solid, continue the trace past the node.
    if hull_point_contents(hull, far, mid) != CONTENTS_SOLID {
        return recursive_hull_check_depth(hull, far, midf, p2f, mid, p2, trace, depth + 1);
    }

    // Never got out of the solid area.
    if trace.allsolid {
        return false;
    }

    // ---- the far side of the node is solid: this is the impact point ----
    if side == 0 {
        trace.plane_normal = plane_normal;
        trace.plane_dist = plane_dist;
    } else {
        trace.plane_normal = [-plane_normal[0], -plane_normal[1], -plane_normal[2]];
        trace.plane_dist = -plane_dist;
    }

    // "shouldn't really happen, but does occasionally": back the impact point
    // out of any residual solid by stepping the fraction back 0.1 at a time.
    while hull_point_contents(hull, hull.headnode, mid) == CONTENTS_SOLID {
        frac -= 0.1;
        if frac < 0.0 {
            trace.fraction = midf;
            trace.endpos = mid;
            // C: Con_DPrintf("backup past 0\n");
            return false;
        }
        midf = p1f + (p2f - p1f) * frac;
        mid = [p1[0] + frac * (p2[0] - p1[0]), p1[1] + frac * (p2[1] - p1[1]), p1[2] + frac * (p2[2] - p1[2])];
    }

    trace.fraction = midf;
    trace.endpos = mid;

    false
}

/// `SV_PointContents` — the contents of point `p` in the world, walking hull 0
/// from `models[0].headnode[0]`. Current-carrying water variants
/// (`CONTENTS_CURRENT_0..CONTENTS_CURRENT_DOWN`) are normalized to
/// `CONTENTS_WATER`, exactly as the C did.
pub fn point_contents(bsp: &Bsp, p: Vec3) -> i32 {
    let hull = build_hull(bsp, 0);
    let cont = hull_point_contents(&hull, hull.headnode, p);
    if (CONTENTS_CURRENT_DOWN..=CONTENTS_CURRENT_0).contains(&cont) { CONTENTS_WATER } else { cont }
}

/// Initialize a trace prior to walking the hull, matching the
/// `memset(&trace,0)` + `fraction=1; allsolid=true; endpos=end` of
/// `SV_ClipMoveToEntity` (everything else false).
fn init_trace(end_local: Vec3) -> Trace {
    HostTrace {
        allsolid: true,
        startsolid: false,
        inopen: false,
        inwater: false,
        fraction: 1.0,
        endpos: end_local,
        plane_normal: [0.0, 0.0, 0.0],
        plane_dist: 0.0,
    }
}

/// Choose the clipping hull for a moving box of size `maxs - mins`, mirroring
/// `SV_HullForEntity`: `size[0] < 3` -> hull 0, `<= 32` -> hull 1, else hull 2.
fn hull_index_for_size(mins: Vec3, maxs: Vec3) -> usize {
    let sx = maxs[0] - mins[0];
    if sx < 3.0 {
        0
    } else if sx <= 32.0 {
        1
    } else {
        2
    }
}

/// Box-trace `mins`/`maxs` from `start` to `end` against the world, returning a
/// [`HostTrace`] in **world space**.
///
/// Faithful to `SV_ClipMoveToEntity` against the world model (no rotation; the
/// world's origin is `(0,0,0)`): a hull is chosen by box size, the move is
/// shifted by `offset = hull.clip_mins - mins` so the box becomes a point in the
/// hull's frame, [`recursive_hull_check`] traces that point, and the resulting
/// `endpos` is shifted back by `offset` (only when the move was clipped, exactly
/// as the C's `if (trace.fraction != 1)`).
pub fn trace_world(bsp: &Bsp, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> HostTrace {
    bump_trace_count();
    let which = hull_index_for_size(mins, maxs);
    let hull = build_hull(bsp, which);

    // offset = hull.clip_mins - mins (world origin is (0,0,0) for the world).
    let offset: Vec3 = [hull.clip_mins[0] - mins[0], hull.clip_mins[1] - mins[1], hull.clip_mins[2] - mins[2]];

    let start_l: Vec3 = [start[0] - offset[0], start[1] - offset[1], start[2] - offset[2]];
    let end_l: Vec3 = [end[0] - offset[0], end[1] - offset[1], end[2] - offset[2]];

    // Default trace: fraction 1, allsolid true, endpos = end (local frame).
    let mut trace = init_trace(end_l);

    recursive_hull_check(&hull, hull.headnode, 0.0, 1.0, start_l, end_l, &mut trace);

    // Fix the endpoint up by the offset when the move was clipped.
    if trace.fraction != 1.0 {
        trace.endpos = [trace.endpos[0] + offset[0], trace.endpos[1] + offset[1], trace.endpos[2] + offset[2]];
    } else {
        // fraction == 1: the C leaves endpos as the (already world-space) end.
        trace.endpos = end;
    }

    trace
}

// ---------------------------------------------------------------------------
// Entity-vs-entity collision: swept box-vs-box and box-vs-submodel.
//
// These are the two non-world clip paths of `SV_ClipMoveToEntity`
// (world.c ~722) plus the hull selection of `SV_HullForEntity` (world.c ~129).
// The world path stays in `trace_world`; here we add the SOLID_BBOX /
// SOLID_SLIDEBOX box path (which the C built via `SV_HullForBox` /
// `SV_InitBoxHull`) and the SOLID_BSP submodel path.
// ---------------------------------------------------------------------------

/// `SV_ClipMoveToEntity` against a **SOLID_BBOX / SOLID_SLIDEBOX** entity: a
/// swept-box-vs-box trace.
///
/// In the C engine this went through `SV_HullForBox`, whose six axial planes
/// describe the *Minkowski-expanded* target box (`SV_HullForEntity`'s else
/// branch computes `hullmins = ent.mins - maxs`, `hullmaxs = ent.maxs - mins`
/// and offsets by the target origin, so the moving box collapses to a point
/// against the grown box). The recursive box-hull walk is exactly a
/// segment-vs-AABB slab test, which we do directly here.
///
/// * `start`/`end` — the moving box's centre path (world space).
/// * `move_mins`/`move_maxs` — the moving box size (the trace's mins/maxs).
/// * `ent_mins`/`ent_maxs`/`ent_origin` — the target entity's box.
///
/// The expanded box is
/// `bmin = ent_origin + ent_mins - move_maxs`,
/// `bmax = ent_origin + ent_maxs - move_mins`. We slab-clip the segment to it,
/// using a `DIST_EPSILON` pullback on the entry plane (matching the engine's
/// near-side bias so the mover stops just shy). Starting inside the box sets
/// `startsolid` (and `allsolid`); a clear pass returns `fraction == 1`.
///
/// The box is HALF-OPEN, `bmin <= p < bmax` on each axis, as the C box hull
/// is: `SV_InitBoxHull` puts CONTENTS_EMPTY on the front (`d >= 0`) of each
/// max plane and on the back (`d < 0`) of each min plane, so a point exactly
/// on a max face is outside and one exactly on a min face is inside. Boxes
/// that merely touch on the target's max side do not collide — e1m1's
/// 10-health box dropped beside a grunt, e1m6's 25-health box beside an ogre,
/// reach the floor as in id's game instead of "falling out of the level".
pub fn clip_box(
    start: Vec3,
    end: Vec3,
    move_mins: Vec3,
    move_maxs: Vec3,
    ent_mins: Vec3,
    ent_maxs: Vec3,
    ent_origin: Vec3,
) -> HostTrace {
    // The Minkowski-expanded target box (SV_HullForEntity's box branch).
    let bmin: Vec3 = [
        ent_origin[0] + ent_mins[0] - move_maxs[0],
        ent_origin[1] + ent_mins[1] - move_maxs[1],
        ent_origin[2] + ent_mins[2] - move_maxs[2],
    ];
    let bmax: Vec3 = [
        ent_origin[0] + ent_maxs[0] - move_mins[0],
        ent_origin[1] + ent_maxs[1] - move_mins[1],
        ent_origin[2] + ent_maxs[2] - move_mins[2],
    ];

    // Default: a clear move (fraction 1, allsolid false, endpos = end).
    let mut tr = HostTrace {
        allsolid: false,
        startsolid: false,
        inopen: false,
        inwater: false,
        fraction: 1.0,
        endpos: end,
        plane_normal: [0.0, 0.0, 0.0],
        plane_dist: 0.0,
    };

    let d: Vec3 = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];

    // Slab clip. `tenter` rises as each axis pushes the entry forward; `texit`
    // falls as each axis pulls the exit back. We track the entry axis/sign so
    // the entry face becomes the hit normal.
    let mut tenter = 0.0f32;
    let mut texit = 1.0f32;
    let mut enter_axis: i32 = -1;
    let mut enter_sign = 0.0f32;

    // Is the start point inside the expanded box on every axis? (startsolid)
    let mut inside = true;

    for i in 0..3 {
        let outside = start[i] < bmin[i] || start[i] >= bmax[i]; // half-open
        if outside {
            inside = false;
        }
        if d[i] == 0.0 {
            // Parallel to this slab: if outside it, the segment can never enter.
            if outside {
                return tr; // misses entirely -> clear
            }
            continue;
        }
        let inv = 1.0 / d[i];
        let mut t1 = (bmin[i] - start[i]) * inv; // crossing of the -face
        let mut t2 = (bmax[i] - start[i]) * inv; // crossing of the +face
        // sign of the entry face normal on this axis: the near plane we cross.
        // Moving in +d we enter through the -face (normal -axis) and exit the
        // +face; moving in -d it is the reverse.
        let mut sign = -1.0f32;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
            sign = 1.0;
        }
        // `t1 == 0` with no entry yet: the start sits exactly on a max face
        // (outside, half-open) moving in; the C crosses that plane at once
        // and stops the mover against it at fraction 0.
        if t1 > tenter || (t1 == 0.0 && enter_axis < 0) {
            tenter = t1;
            enter_axis = i as i32;
            enter_sign = sign;
        }
        if t2 < texit {
            texit = t2;
        }
        if tenter > texit {
            return tr; // entered after exiting -> never overlaps -> clear
        }
    }

    if inside {
        // The mover began inside the (expanded) target box. Mirror the C
        // SV_RecursiveHullCheck box hull: the trace keeps fraction=1.0 / endpos=end
        // (the move COMPLETES — exiting a box is not an impact, SV_FlyMove breaks on
        // fraction==1). `allsolid` is set only when the segment never leaves the box
        // (texit stays >= 1.0, so the end is inside too) — that is the only case the
        // C treats as trapped (SV_FlyMove zeroes velocity). When the move exits
        // (texit < 1.0), allsolid stays false so a mover that merely brushes into a
        // box slides out instead of freezing. (Previously this returned allsolid +
        // fraction=0, locking any two entities whose AABBs overlapped.)
        tr.startsolid = true;
        tr.allsolid = texit >= 1.0;
        tr.fraction = 1.0;
        tr.endpos = end;
        return tr;
    }

    if enter_axis < 0 {
        // No axis produced an entry plane (e.g. zero-length move not inside):
        // treat as a clear move.
        return tr;
    }

    // Pull the entry back by DIST_EPSILON on the ENTRY PLANE so the mover stops
    // just shy of the surface. SV_RecursiveHullCheck biases per-plane:
    // `frac = (t1 - DIST_EPSILON) / (t1 - t2)`, i.e. in fraction terms the shift is
    // DIST_EPSILON / |t1 - t2| = DIST_EPSILON / |d[entry_axis]| (the entry plane is
    // axis-aligned, so the distance changes by |d[ax]| per unit fraction). The
    // earlier 3D-length pullback (DIST_EPSILON / |d|) under-shifted diagonal moves,
    // letting box-vs-box stops land up to a few units too deep.
    let ax = enter_axis as usize;
    let mut frac = tenter;
    if d[ax] != 0.0 {
        frac = tenter - DIST_EPSILON / d[ax].abs();
    }
    // Two-if clamp in the C; .clamp is identical (NaN passes through both forms).
    frac = frac.clamp(0.0, 1.0);

    tr.fraction = frac;
    tr.endpos = [start[0] + frac * d[0], start[1] + frac * d[1], start[2] + frac * d[2]];
    let mut normal: Vec3 = [0.0, 0.0, 0.0];
    if let Some(slot) = normal.get_mut(enter_axis as usize) {
        *slot = enter_sign;
    }
    tr.plane_normal = normal;
    // plane_dist: the box face position projected onto the normal.
    tr.plane_dist = if enter_sign > 0.0 {
        bmax.get(enter_axis as usize).copied().unwrap_or(0.0)
    } else {
        -bmin.get(enter_axis as usize).copied().unwrap_or(0.0)
    };
    tr
}

/// `SV_ClipMoveToEntity` against a **SOLID_BSP** entity (a brush submodel): pick
/// the clip hull by mover size, build it from `bsp.clipnodes` rooted at the
/// submodel's `headnode[which]`, offset the move into the hull's point frame,
/// and run [`recursive_hull_check`].
///
/// * `model_index` indexes `bsp.models`; the submodel's `headnode[which]` is
///   the hull root. An out-of-range model index (or model with no clip hull)
///   yields a clear trace.
/// * `ent_origin` is the submodel entity's origin (brush models translate, no
///   rotation here).
/// * `start`/`end` are the moving box's path, `move_mins`/`move_maxs` its size.
///
/// `offset = hull.clip_mins - move_mins + ent_origin`; the trace runs on
/// `(start - offset) -> (end - offset)` and the resulting `endpos` is shifted
/// back by `offset` when the move was clipped (`fraction != 1`), mirroring the
/// C `if (trace.fraction != 1) VectorAdd(...)`.
pub fn trace_submodel(
    bsp: &Bsp,
    model_index: usize,
    ent_origin: Vec3,
    start: Vec3,
    end: Vec3,
    move_mins: Vec3,
    move_maxs: Vec3,
) -> HostTrace {
    bump_trace_count();
    // A clear (unclipped) trace, used when this submodel has no usable hull.
    let clear = HostTrace {
        allsolid: false,
        startsolid: false,
        inopen: false,
        inwater: false,
        fraction: 1.0,
        endpos: end,
        plane_normal: [0.0, 0.0, 0.0],
        plane_dist: 0.0,
    };

    // Out-of-range submodel -> nothing to clip against.
    let model = match bsp.models.get(model_index) {
        Some(m) => m,
        None => return clear,
    };

    let which = hull_index_for_size(move_mins, move_maxs);
    let headnode = match model.headnode.get(which).copied() {
        Some(h) => h,
        None => return clear,
    };

    let hull = build_hull_for_model(bsp, which, headnode);

    // offset = hull.clip_mins - move_mins + ent_origin.
    let offset: Vec3 = [
        hull.clip_mins[0] - move_mins[0] + ent_origin[0],
        hull.clip_mins[1] - move_mins[1] + ent_origin[1],
        hull.clip_mins[2] - move_mins[2] + ent_origin[2],
    ];

    let start_l: Vec3 = [start[0] - offset[0], start[1] - offset[1], start[2] - offset[2]];
    let end_l: Vec3 = [end[0] - offset[0], end[1] - offset[1], end[2] - offset[2]];

    let mut trace = init_trace(end_l);
    recursive_hull_check(&hull, hull.headnode, 0.0, 1.0, start_l, end_l, &mut trace);

    if trace.fraction != 1.0 {
        trace.endpos = [trace.endpos[0] + offset[0], trace.endpos[1] + offset[1], trace.endpos[2] + offset[2]];
    } else {
        trace.endpos = end;
    }
    trace
}

// ---------------------------------------------------------------------------
// Movement: slide-move against the world (SV_FlyMove)
// ---------------------------------------------------------------------------

const STOP_EPSILON: f32 = 0.1;
/// Maximum stair height a walker steps up (`STEPSIZE` in the C).
pub const STEPSIZE: f32 = 18.0;

/// `ClipVelocity` (sv_phys.c): slide velocity `v` off a surface with `normal`,
/// returning the clipped velocity and blocked flags (`1` = floor, `2` = wall).
pub fn clip_velocity(v: Vec3, normal: Vec3, overbounce: f32) -> (Vec3, i32) {
    let mut blocked = 0;
    if normal[2] > 0.0 {
        blocked |= 1; // floor
    }
    if normal[2] == 0.0 {
        blocked |= 2; // wall / step
    }
    let backoff = dot(v, normal) * overbounce;
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        out[i] = v[i] - normal[i] * backoff;
        if out[i] > -STOP_EPSILON && out[i] < STOP_EPSILON {
            out[i] = 0.0;
        }
    }
    (out, blocked)
}

/// The outcome of [`fly_move`].
#[derive(Debug, Clone, Copy)]
pub struct MoveResult {
    pub origin: Vec3,
    pub velocity: Vec3,
    /// `SV_FlyMove` blocked flags: `1` = hit a floor, `2` = hit a wall/step.
    pub blocked: i32,
    /// A floor (`normal.z > 0.7`) was contacted during the move.
    pub on_ground: bool,
}

/// `SV_FlyMove` (sv_phys.c), world-only: slide a box (`mins`/`maxs`) from
/// `origin` with `velocity` over `dt`, clipping against the world hull and
/// sliding along up to `MAX_CLIP_PLANES` surfaces (so the mover follows walls
/// and creases instead of stopping dead). Entity-vs-entity collision and touch
/// impacts are out of scope — the only solid is the map.
pub fn fly_move(bsp: &Bsp, origin: Vec3, mins: Vec3, maxs: Vec3, velocity: Vec3, dt: f32) -> MoveResult {
    const MAX_CLIP_PLANES: usize = 5;
    let mut origin = origin;
    let mut velocity = velocity;
    let primal = velocity;
    let mut original = velocity;
    let mut planes: Vec<Vec3> = Vec::with_capacity(MAX_CLIP_PLANES);
    let mut blocked = 0;
    let mut on_ground = false;
    let mut time_left = dt;

    for _bump in 0..4 {
        if velocity == [0.0, 0.0, 0.0] {
            break;
        }
        let end = [
            origin[0] + time_left * velocity[0],
            origin[1] + time_left * velocity[1],
            origin[2] + time_left * velocity[2],
        ];
        let trace = trace_world(bsp, origin, end, mins, maxs);

        if trace.allsolid {
            // trapped in solid: stop dead
            return MoveResult { origin, velocity: [0.0; 3], blocked: 3, on_ground };
        }
        if trace.fraction > 0.0 {
            origin = trace.endpos;
            original = velocity;
            planes.clear();
        }
        if trace.fraction == 1.0 {
            break; // moved the whole way
        }

        let n = trace.plane_normal;
        if n[2] > 0.7 {
            blocked |= 1;
            on_ground = true;
        }
        if n[2] == 0.0 {
            blocked |= 2;
        }

        time_left -= time_left * trace.fraction;

        if planes.len() >= MAX_CLIP_PLANES {
            return MoveResult { origin, velocity: [0.0; 3], blocked: 3, on_ground };
        }
        planes.push(n);

        // Find a velocity that parallels every clip plane.
        let mut chosen: Option<Vec3> = None;
        for i in 0..planes.len() {
            let (nv, _b) = clip_velocity(original, planes[i], 1.0);
            let ok = planes.iter().enumerate().all(|(j, p)| j == i || dot(nv, *p) >= 0.0);
            if ok {
                chosen = Some(nv);
                break;
            }
        }
        match chosen {
            Some(nv) => velocity = nv,
            None => {
                // Slide along the crease of two planes; bail otherwise.
                if planes.len() != 2 {
                    return MoveResult { origin, velocity: [0.0; 3], blocked: 7, on_ground };
                }
                let dir = crate::math::cross(planes[0], planes[1]);
                let d = dot(dir, velocity);
                velocity = crate::math::scale(dir, d);
            }
        }

        // Stop dead if we've reversed into the original direction (corner jitter).
        if dot(velocity, primal) <= 0.0 {
            return MoveResult { origin, velocity: [0.0; 3], blocked, on_ground };
        }
    }

    MoveResult { origin, velocity, blocked, on_ground }
}

/// Walk a player box one frame: slide horizontally with [`fly_move`], attempt a
/// stair step-up of up to [`STEPSIZE`] when the flat move is blocked, then snap
/// down onto the floor so the walker hugs the ground. Returns the new origin.
///
/// This is a pragmatic blend of `SV_FlyMove` and `SV_movestep`'s stair handling
/// (world-only) — enough to walk a real map, slide along its walls, and climb
/// its small ledges. Full Quake player movement (friction, acceleration, air
/// control, entity clipping) is out of scope.
pub fn walk_move(bsp: &Bsp, origin: Vec3, mins: Vec3, maxs: Vec3, wishvel: Vec3, dt: f32) -> Vec3 {
    let horiz2 = |a: Vec3, b: Vec3| {
        let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
        dx * dx + dy * dy
    };

    // 1) Flat slide.
    let flat = fly_move(bsp, origin, mins, maxs, wishvel, dt);

    // 2) Stair step-up: rise STEPSIZE, slide from there, settle back down onto a
    //    floor. Only accepted if it actually landed on a floor.
    let up = trace_world(bsp, origin, [origin[0], origin[1], origin[2] + STEPSIZE], mins, maxs);
    let stepped: Option<Vec3> = {
        let s = fly_move(bsp, up.endpos, mins, maxs, wishvel, dt);
        let down = trace_world(bsp, s.origin, [s.origin[0], s.origin[1], s.origin[2] - STEPSIZE], mins, maxs);
        if down.fraction < 1.0 && down.plane_normal[2] >= 0.7 { Some(down.endpos) } else { None }
    };

    // 3) Keep whichever advanced farther horizontally.
    let mut result = match stepped {
        Some(sp) if horiz2(sp, origin) > horiz2(flat.origin, origin) + 0.01 => sp,
        _ => flat.origin,
    };

    // 4) Ground snap: drop onto the floor within STEPSIZE so the box stays put.
    let snap = trace_world(bsp, result, [result[0], result[1], result[2] - (STEPSIZE + 1.0)], mins, maxs);
    if snap.fraction < 1.0 && snap.plane_normal[2] >= 0.7 {
        result = snap.endpos;
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::{DPlane, PLANE_X};

    /// Build a one-plane hull by hand: clipnode 0 splits on an axial +X plane at
    /// `x == dist`. The front side (child 0, `x >= dist`) is `CONTENTS_EMPTY`;
    /// the back side (child 1, `x < dist`) is `CONTENTS_SOLID`. Planes are owned
    /// by the caller (so the borrow outlives the hull).
    fn one_plane_hull(planes: &[DPlane]) -> Hull<'_> {
        Hull {
            clipnodes: std::rc::Rc::new(vec![ClipNode { planenum: 0, children: [CONTENTS_EMPTY, CONTENTS_SOLID] }]),
            planes,
            headnode: 0,
            clip_mins: [0.0, 0.0, 0.0],
            clip_maxs: [0.0, 0.0, 0.0],
        }
    }

    fn x_plane(dist: f32) -> Vec<DPlane> {
        vec![DPlane { normal: [1.0, 0.0, 0.0], dist, ptype: PLANE_X }]
    }

    /// Two clip planes of Scourge of Armagon's maps that pass exactly
    /// through the hull-1 point of an item's `droptofloor` (`census/packs.py`,
    /// `oracle_move`): `DotProduct (normal, p) - dist` is a few millionths
    /// below zero in id's x87 registers, and 0 or a few millionths above it in
    /// `f32` arithmetic — id's `droptofloor` and the port's then disagree on
    /// whether the item is in solid (hip1m1's shells at 1184 -160 -176, which
    /// id's removes as "fell out of level"; hip3m1's rockets at -224 16 -448,
    /// which it keeps). Summed in `f64`, the point is on the back side, as in
    /// id's.
    #[test]
    fn a_point_on_a_slanted_plane_lands_on_ids_side() {
        let s = std::f32::consts::FRAC_1_SQRT_2; // 0.70710677, as the bsp stores it
        for (normal, dist, p) in [
            ([s, -s, 0.0], 950.3515_f32, [1200.0, -144.0, -146.0]), // hip1m1 plane 1204
            ([s, s, 0.0], -124.45079_f32, [-208.0, 32.0, -418.0]),  // hip3m1 plane 1526
        ] {
            let f32_sum = dot(normal, p) - dist;
            assert!(f32_sum >= 0.0, "f32 puts {p:?} in front ({f32_sum})");
            let planes = [DPlane { normal, dist, ptype: 3 }];
            let hull = one_plane_hull(&planes);
            assert!(hull.plane_distance(&planes[0], p) < 0.0);
            assert_eq!(hull_point_contents(&hull, 0, p), CONTENTS_SOLID, "{p:?} is behind the plane");
        }
    }

    #[test]
    fn point_contents_empty_and_solid_sides() {
        let planes = x_plane(0.0);
        let hull = one_plane_hull(&planes);

        // x > 0 => front side => EMPTY.
        assert_eq!(hull_point_contents(&hull, 0, [10.0, 0.0, 0.0]), CONTENTS_EMPTY);
        // x < 0 => back side => SOLID.
        assert_eq!(hull_point_contents(&hull, 0, [-10.0, 0.0, 0.0]), CONTENTS_SOLID);
        // Exactly on the plane (d == 0, not < 0) => front => EMPTY.
        assert_eq!(hull_point_contents(&hull, 0, [0.0, 0.0, 0.0]), CONTENTS_EMPTY);
    }

    #[test]
    fn trace_from_empty_into_solid_clips() {
        let planes = x_plane(0.0);
        let hull = one_plane_hull(&planes);

        // Start in empty (x=10) heading into solid (x=-10).
        let mut trace = init_trace([-10.0, 0.0, 0.0]);
        let blocked =
            recursive_hull_check(&hull, hull.headnode, 0.0, 1.0, [10.0, 0.0, 0.0], [-10.0, 0.0, 0.0], &mut trace);

        // The move was clipped (returns false once blocked).
        assert!(!blocked);
        // Fraction is in [0, 1): we did not reach the end.
        assert!(trace.fraction >= 0.0 && trace.fraction < 1.0);
        // Impact plane normal points along +X (the empty side's plane).
        assert_eq!(trace.plane_normal, [1.0, 0.0, 0.0]);
        // We started in open space, never solid at the start.
        assert!(!trace.startsolid);
        assert!(!trace.allsolid);
        assert!(trace.inopen);
        // The impact endpoint is on the empty side of the plane (x >= 0),
        // backed off by DIST_EPSILON.
        assert!(trace.endpos[0] >= 0.0);
    }

    #[test]
    fn fully_empty_trace_completes() {
        let planes = x_plane(0.0);
        let hull = one_plane_hull(&planes);

        // Whole move stays on the empty (front) side: x from 50 to 10.
        let mut trace = init_trace([10.0, 0.0, 0.0]);
        let blocked =
            recursive_hull_check(&hull, hull.headnode, 0.0, 1.0, [50.0, 0.0, 0.0], [10.0, 0.0, 0.0], &mut trace);

        // Reached the end: fraction stays 1.0, never blocked.
        assert!(blocked);
        assert_eq!(trace.fraction, 1.0);
        assert!(!trace.allsolid);
        assert!(trace.inopen);
        assert_eq!(trace.endpos, [10.0, 0.0, 0.0]);
    }

    #[test]
    fn trace_starting_in_solid_sets_startsolid() {
        let planes = x_plane(0.0);
        let hull = one_plane_hull(&planes);

        // Whole move stays in solid (back side): x from -50 to -10.
        let mut trace = init_trace([-10.0, 0.0, 0.0]);
        recursive_hull_check(&hull, hull.headnode, 0.0, 1.0, [-50.0, 0.0, 0.0], [-10.0, 0.0, 0.0], &mut trace);
        assert!(trace.startsolid);
        // allsolid stays true: never left the solid region.
        assert!(trace.allsolid);
    }

    #[test]
    fn bad_node_number_is_solid_not_panic() {
        let planes = x_plane(0.0);
        // Clipnode 0 points at a non-existent node index 5 on its front side.
        let hull = Hull {
            clipnodes: std::rc::Rc::new(vec![ClipNode { planenum: 0, children: [5, CONTENTS_SOLID] }]),
            planes: &planes,
            headnode: 0,
            clip_mins: [0.0, 0.0, 0.0],
            clip_maxs: [0.0, 0.0, 0.0],
        };
        // Walking onto the out-of-range index must not panic; it reports SOLID.
        assert_eq!(hull_point_contents(&hull, 0, [10.0, 0.0, 0.0]), CONTENTS_SOLID);
    }

    #[test]
    fn build_hull_clamps_bad_index() {
        // An empty BSP: building any hull index (including a bad one) must not
        // panic and yields an empty clipnode set with default headnode 0.
        let bsp = empty_bsp();
        let h0 = build_hull(&bsp, 0);
        assert_eq!(h0.clip_mins, [0.0, 0.0, 0.0]);
        let h_bad = build_hull(&bsp, 99); // clamps to 0
        assert_eq!(h_bad.clip_mins, [0.0, 0.0, 0.0]);
        let h1 = build_hull(&bsp, 1);
        assert_eq!(h1.clip_mins, [-16.0, -16.0, -24.0]);
        let h2 = build_hull(&bsp, 2);
        assert_eq!(h2.clip_maxs, [32.0, 32.0, 64.0]);
    }

    #[test]
    fn point_contents_empty_world_is_solid() {
        // With no nodes/leafs, hull 0's headnode (0) names node 0 which does not
        // exist -> treated as SOLID, but point_contents must not panic.
        let bsp = empty_bsp();
        let c = point_contents(&bsp, [0.0, 0.0, 0.0]);
        assert_eq!(c, CONTENTS_SOLID);
    }

    #[test]
    fn trace_world_offsets_box_to_point() {
        // Build a real one-plane hull-1 BSP via the loader path is heavy; here we
        // exercise the offsetting math directly through trace_world on an empty
        // world (which yields a SOLID/blocked trace) and assert no panic and a
        // sane fraction. The empty-world headnode 0 is out of range -> blocked.
        let bsp = empty_bsp();
        let tr = trace_world(&bsp, [0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]);
        assert!(tr.fraction >= 0.0 && tr.fraction <= 1.0);
    }

    /// A BSP with empty lumps, used to confirm the world walks never panic when
    /// the map has no geometry.
    fn empty_bsp() -> Bsp {
        Bsp {
            version: crate::bsp::BSPVERSION,
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
        }
    }

    /// A single solid half-space (`x < 0` is solid) expressed as a hull-1 clip
    /// BSP, for the movement tests.
    fn wall_bsp() -> Bsp {
        use crate::bsp::{DClipNode, DModel, DPlane};
        let mut b = empty_bsp();
        b.planes = vec![DPlane { normal: [1.0, 0.0, 0.0], dist: 0.0, ptype: 0 }];
        // clipnode 0: front (x >= 0) EMPTY, back (x < 0) SOLID.
        b.clipnodes = vec![DClipNode { planenum: 0, children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16] }];
        b.models = vec![DModel {
            mins: [-256.0; 3],
            maxs: [256.0; 3],
            origin: [0.0; 3],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: 0,
            numfaces: 0,
        }];
        b
    }

    #[test]
    fn clip_velocity_slides_along_wall() {
        // Into a +X wall: the X component cancels, Y is preserved.
        let (out, blocked) = clip_velocity([-100.0, -100.0, 0.0], [1.0, 0.0, 0.0], 1.0);
        assert!(out[0].abs() < 0.001, "x cancelled, got {}", out[0]);
        assert!((out[1] + 100.0).abs() < 0.001, "y preserved, got {}", out[1]);
        assert_eq!(blocked & 2, 2, "wall flag set");
        // Onto a floor: the floor flag is set.
        let (_o, b2) = clip_velocity([0.0, 0.0, -50.0], [0.0, 0.0, 1.0], 1.0);
        assert_eq!(b2 & 1, 1, "floor flag set");
    }

    #[test]
    fn fly_move_slides_along_wall_not_through() {
        let bsp = wall_bsp();
        let (mins, maxs) = ([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]);
        // Start in the empty half (x = 50), aim diagonally into the wall.
        let r = fly_move(&bsp, [50.0, 0.0, 0.0], mins, maxs, [-100.0, -100.0, 0.0], 1.0);
        assert!(r.origin[0] >= -1.0, "did not pass through the wall: x={}", r.origin[0]);
        assert!(r.origin[1] < -10.0, "slid along the wall in -y: y={}", r.origin[1]);
        assert_eq!(r.blocked & 2, 2, "registered a wall hit");
    }

    // -------------------------------------------------- clip_box (box vs box)

    #[test]
    fn clip_box_stops_moving_into_box() {
        // A point mover (mins=maxs=0) at the origin heading +X. The target box
        // is a 32-unit cube centred at x=100, so [84,116] on X. The mover should
        // stop at fraction in (0,1) just shy of x=84, with normal -X.
        let mover_mins = [0.0, 0.0, 0.0];
        let mover_maxs = [0.0, 0.0, 0.0];
        let ent_mins = [-16.0, -16.0, -16.0];
        let ent_maxs = [16.0, 16.0, 16.0];
        let tr =
            clip_box([0.0, 0.0, 0.0], [200.0, 0.0, 0.0], mover_mins, mover_maxs, ent_mins, ent_maxs, [100.0, 0.0, 0.0]);
        assert!(!tr.startsolid, "did not start inside");
        assert!(!tr.allsolid);
        assert!(tr.fraction > 0.0 && tr.fraction < 1.0, "stopped partway, got fraction {}", tr.fraction);
        assert_eq!(tr.plane_normal, [-1.0, 0.0, 0.0], "entry face is -X");
        // Stopped just shy of x = 84 (the near face), backed off by epsilon.
        assert!(tr.endpos[0] < 84.0, "stopped before the box face: {}", tr.endpos[0]);
        assert!(tr.endpos[0] > 80.0, "but close to it: {}", tr.endpos[0]);
    }

    #[test]
    fn clip_box_miss_is_clear() {
        // Same target box, but the mover travels +X far below it (z = -100), so
        // it never overlaps -> a clear move (fraction 1).
        let tr = clip_box(
            [0.0, 0.0, -100.0],
            [200.0, 0.0, -100.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            [100.0, 0.0, 0.0],
        );
        assert_eq!(tr.fraction, 1.0, "missed -> clear");
        assert!(!tr.startsolid);
        assert!(!tr.allsolid);
        assert_eq!(tr.endpos, [200.0, 0.0, -100.0]);
    }

    #[test]
    fn clip_box_start_inside_slides_out_or_traps() {
        // Mover (a point) starts at the box centre and moves OUT the +X face. Per the
        // C box hull, EXITING a box is not an impact: startsolid is set, but
        // allsolid=false and the move completes (fraction 1.0, endpos=end) so the
        // mover slides out instead of freezing.
        let tr = clip_box(
            [100.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            [100.0, 0.0, 0.0],
        );
        assert!(tr.startsolid, "started inside the box");
        assert!(!tr.allsolid, "exits the box -> not trapped (the freeze bug)");
        assert_eq!(tr.fraction, 1.0, "exiting completes the move");
        assert_eq!(tr.endpos, [200.0, 0.0, 0.0]);

        // A move that stays WHOLLY inside (start and end inside) is trapped: allsolid.
        let tr2 = clip_box(
            [100.0, 0.0, 0.0],
            [105.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            [100.0, 0.0, 0.0],
        );
        assert!(tr2.startsolid && tr2.allsolid, "stays inside -> trapped (allsolid)");
    }

    #[test]
    fn clip_box_is_half_open_like_the_c_box_hull() {
        // SV_InitBoxHull: CONTENTS_EMPTY in front of each max plane (d >= 0),
        // behind each min plane (d < 0). e1m1's 10-health box (0..32 x 0..32 x
        // 0..56, dropping from z -298) beside a grunt at (1232, 2448) (-16..16):
        // its y range starts exactly at the grunt's absmax.y 2464, so the drop is
        // clear of the grunt (id keeps the box; the port used to start "inside").
        let drop = |y: f32| {
            clip_box(
                [1224.0, y, -298.0],
                [1224.0, y, -554.0],
                [0.0, 0.0, 0.0],
                [32.0, 32.0, 56.0],
                [-16.0, -16.0, -24.0],
                [16.0, 16.0, 40.0],
                [1232.0, 2448.0, -280.0],
            )
        };
        let tr = drop(2464.0);
        assert!(!tr.startsolid && tr.fraction == 1.0, "touching the max face: no contact");
        // Touching the MIN face (the box's y max == the grunt's absmin.y) is
        // inside in the C, so it still starts solid.
        let tr = drop(2432.0 - 32.0);
        assert!(tr.startsolid, "touching the min face: inside");
        // Starting exactly on a max face and moving in stops at once (the C
        // crosses the max plane at fraction 0), rather than passing through.
        let tr = clip_box([16.0, 0.0, 0.0], [-100.0, 0.0, 0.0], [0.0; 3], [0.0; 3], [-16.0; 3], [16.0; 3], [0.0; 3]);
        assert!(!tr.startsolid, "on the max face is outside");
        assert_eq!((tr.fraction, tr.plane_normal), (0.0, [1.0, 0.0, 0.0]));
    }

    #[test]
    fn clip_box_expands_by_mover_size() {
        // A finite mover box (a 32-cube, mins/maxs = -16..16) heading +X toward
        // the same target. The expanded box near face is at
        // bmin.x = 100 + (-16) - 16 = 68, so the centre should stop near x=68,
        // proving the box was grown by the mover's +X extent.
        let tr = clip_box(
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            [100.0, 0.0, 0.0],
        );
        assert!(tr.fraction > 0.0 && tr.fraction < 1.0);
        assert_eq!(tr.plane_normal, [-1.0, 0.0, 0.0]);
        assert!(tr.endpos[0] < 68.0 && tr.endpos[0] > 64.0, "stopped near x=68, got {}", tr.endpos[0]);
    }

    // ------------------------------------------------ trace_submodel (vs BSP)

    #[test]
    fn trace_submodel_clips_against_brush() {
        // Model 1 is a brush submodel whose hull-1 is the +X half-space wall
        // (x < 0 solid). A player-sized box starting in the empty half and
        // moving -X should clip against it.
        let bsp = submodel_wall_bsp();
        let (mins, maxs) = ([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]);
        let tr = trace_submodel(
            &bsp,
            1,
            [0.0, 0.0, 0.0], // submodel origin
            [50.0, 0.0, 0.0],
            [-50.0, 0.0, 0.0],
            mins,
            maxs,
        );
        assert!(tr.fraction < 1.0, "clipped against the submodel brush");
        assert!(tr.endpos[0] >= -1.0, "stopped before passing through: {}", tr.endpos[0]);
    }

    #[test]
    fn trace_submodel_out_of_range_is_clear() {
        let bsp = submodel_wall_bsp();
        // Model index 9 does not exist -> a clear move.
        let tr = trace_submodel(
            &bsp,
            9,
            [0.0, 0.0, 0.0],
            [50.0, 0.0, 0.0],
            [-50.0, 0.0, 0.0],
            [-16.0, -16.0, -24.0],
            [16.0, 16.0, 32.0],
        );
        assert_eq!(tr.fraction, 1.0);
        assert_eq!(tr.endpos, [-50.0, 0.0, 0.0]);
    }

    /// A BSP with two models: model 0 (the world, headnodes 0) and model 1, a
    /// brush submodel whose hull-1 headnode (index 0) is the +X wall clipnode.
    fn submodel_wall_bsp() -> Bsp {
        use crate::bsp::{DClipNode, DModel};
        let mut b = empty_bsp();
        b.planes = vec![DPlane { normal: [1.0, 0.0, 0.0], dist: 0.0, ptype: 0 }];
        b.clipnodes = vec![DClipNode { planenum: 0, children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16] }];
        let model = DModel {
            mins: [-256.0; 3],
            maxs: [256.0; 3],
            origin: [0.0; 3],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: 0,
            numfaces: 0,
        };
        b.models = vec![model.clone(), model];
        b
    }
}
