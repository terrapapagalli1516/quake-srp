//! `r_lerpmodels`: an alias model's animation blends between its frames —
//! the port's 2026 extra, after QuakeSpasm's `r_lerpmodels`
//! (`R_SetupAliasFrame`, r_alias.c). No id file ports here: id's client
//! always draws the pose the entity's `frame` field names, vertex for vertex
//! ([`LerpModels::Classic`]).
//!
//! id's QuakeC steps an animated model's `frame` field every 0.1 s (a
//! monster's walk cycle, the view weapon's fire animation): on a 1996
//! monitor id's own frame rate (72 fps at most) could not show much more
//! than the ten poses a second this picks between, so the jump from pose to
//! pose was never far apart in time. At 144-480 Hz, beside a camera that
//! moves every frame, the model visibly holds a pose for several frames and
//! then jumps to the next. With [`LerpModels::Smooth`] the renderer blends
//! the current pose's vertices with the previous one's, by how far through
//! the 0.1 s the clock is — [`crate::render::ModelInstance::blend`] /
//! [`crate::render::Viewmodel::blend`]: the vertex pass this costs is
//! read in [`crate::mdl::Mdl::frame_pose`] and transformed in
//! `AliasSetup::final_vert` (`quake_rs::render::alias`, not public). Only the
//! vertex positions and their shading move; everything else about the entity (its
//! origin, its collision box, [`crate::client::lerpmove`]'s own glide
//! between steps) is unaffected — this extra only changes which vertices
//! the alias pipeline reads.
//!
//! **Which frame changes blend.** [`Frame::Group`](crate::mdl::Frame::Group)
//! poses (a torch's flicker, a flame) are not a motion between two named
//! poses: id already animates them by the clock inside one `frame` index
//! ([`crate::mdl::Mdl::frame_is_group`]), so a blend there would mix two
//! unrelated sub-poses QuakeC never asked to move between (QuakeSpasm keeps
//! a `nolerp` list for the same models). [`FrameLerps::blend`] snaps instead
//! — draws the resolved pose alone, byte for byte as Classic reads it —
//! whenever either side of the change is a group.
//!
//! **How long a blend lasts.** Unlike [`crate::client::lerpmove::StepGlides`],
//! which keeps gliding from wherever the model was last *drawn* (a
//! continuous position, so a new step can continue the motion smoothly),
//! a blend here always runs from the frame that was showing right before
//! the change to the new one: there is no cheap way to "continue" a vertex
//! array that is itself already a blend of two others without storing and
//! re-blending a whole extra pose every frame, for a difference no one
//! would see (QuakeSpasm does the same simpler restart). The length is
//! [`GLIDE`](crate::client::lerpmove::GLIDE), id1's animation tick (0.1 s) —
//! same reasoning as `StepGlides`' own `GLIDE`, and the same fix for the
//! same problem: a frame that changes sooner than half of that (an
//! animation that legitimately ticks faster than 10 Hz, or a demo's ~13 Hz
//! messages beating against a 10 Hz one) would otherwise still be blending
//! the previous change when the next one lands, which trails a fast source
//! more and more; blending only for the time since the last change (as
//! `StepGlides` does for a mover stepped every frame) keeps it caught up.
//!
//! **Snaps instead of blending**: first sighting (or seen again after a
//! frame without it), a model change, either side of the change a group
//! frame (above), the clock going back (a new level, a demo played again),
//! and — like `StepGlides` — a move of more than 100 units on an axis
//! between calls (`StepGlides`' own teleport test, `TELEPORT`): a blend has
//! no view of the world beyond the entity it is given, so it keeps its own
//! copy of that same test rather than trust the position glide to share one.

use std::collections::HashMap;

use crate::math::Vec3;

/// How the renderer draws an alias model's animation (`r_lerpmodels`): set
/// by the host each frame, like [`crate::stepping::Stepping`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LerpModels {
    /// id's: the vertices of the entity's current `frame` alone.
    #[default]
    Classic,
    /// Blend with the previous frame's vertices (see the module doc).
    Smooth,
}

/// id1's monsters and the view weapon animate at 10 Hz: the longest a blend
/// runs, same reasoning as [`crate::client::lerpmove::GLIDE`].
pub const GLIDE_FRAME: f32 = crate::client::lerpmove::GLIDE;

/// Id's teleport test ([`crate::client::lerpmove`]'s own `TELEPORT`): a move
/// of more than this on any axis is a jump, not a step.
const TELEPORT: f32 = 100.0;

/// The view weapon's key in [`FrameLerps`] (`cl_main::walk_frame`,
/// `cl_demo::render_demo_frame`): never a real entity number, which is
/// always non-negative.
pub const VIEWMODEL: i32 = -1;

/// One entity's animation blend.
#[derive(Clone, Copy, Debug)]
struct Lerp {
    /// Identifies the model drawn (the entity's modelindex, or the demo's
    /// precache index): a change snaps.
    model: usize,
    /// The origin last seen, for this module's own teleport test.
    origin: Vec3,
    /// The frame blended from and the frame blended to (equal: nothing to
    /// blend, the entry is a fresh snap).
    prev_frame: usize,
    cur_frame: usize,
    /// When `cur_frame` arrived, and how long the blend to it runs.
    start: f64,
    length: f32,
    /// `cur_frame` is a group frame: the next change snaps too (a group's
    /// pose is the clock's, not one the next frame can be blended from).
    group: bool,
    /// [`FrameLerps::frame`] when this entry was last asked for, so a frame
    /// it is not asked for in is forgotten (the next sighting snaps).
    seen: u32,
}

/// Every alias entity's animation blend, keyed by entity number (-1 for the
/// view weapon, which has no entity of its own: see
/// [`crate::client::cl_main::walk_frame`]).
#[derive(Clone, Debug, Default)]
pub struct FrameLerps {
    lerps: HashMap<i32, Lerp>,
    /// Counts [`FrameLerps::end_frame`]s.
    frame: u32,
}

impl FrameLerps {
    /// The blend to draw entity `num` (model `model`, at server/demo `frame`,
    /// drawn at `origin`) with at client time `time`: `Some((prev_frame,
    /// frac))` to blend `prev_frame`'s vertices toward `frame`'s by `frac`
    /// (0: `prev_frame` alone, 1: `frame` alone — the caller skips the work
    /// at 1), `None` to draw `frame` alone (nothing changed to blend from,
    /// or this call snapped: see the module doc). `is_group` is whether
    /// `frame` resolves to a [`crate::mdl::Frame::Group`]
    /// ([`crate::mdl::Mdl::frame_is_group`]): a group frame always snaps.
    pub fn blend(&mut self, num: i32, model: usize, frame: usize, is_group: bool, origin: Vec3, time: f64) -> Option<(usize, f32)> {
        let seen = self.frame;
        let snap = Lerp { model, origin, prev_frame: frame, cur_frame: frame, start: time, length: 0.0, group: is_group, seen };
        let entry = self.lerps.entry(num).or_insert(snap);
        let jumped = (0..3).any(|i| (origin[i] - entry.origin[i]).abs() > TELEPORT);
        if is_group || entry.group || entry.model != model || time < entry.start || jumped {
            *entry = snap;
        } else if frame != entry.cur_frame {
            // A frame change: blend for GLIDE_FRAME from the frame that was
            // showing — unless this entity already changed frame once
            // (`changed_before`, `StepGlides::draw`'s own `stepped`) and the
            // one before THIS one came less than half of that ago (ticking
            // faster than 10 Hz): then for the time since, so the blend
            // never trails a source that updates faster than it. A freshly
            // snapped entity's first change always gets the full
            // GLIDE_FRAME: `since` measures from an arbitrary sighting, not
            // the animation's own tick, so it says nothing about its rate.
            let changed_before = entry.length > 0.0 || entry.prev_frame != entry.cur_frame;
            let since = (time - entry.start) as f32;
            let length = if changed_before && since < GLIDE_FRAME / 2.0 { since.max(0.0) } else { GLIDE_FRAME };
            *entry = Lerp { model, origin, prev_frame: entry.cur_frame, cur_frame: frame, start: time, length, group: false, seen };
        } else {
            entry.origin = origin;
        }
        entry.seen = seen;
        if entry.prev_frame == entry.cur_frame || entry.length <= 0.0 {
            return None;
        }
        let frac = (((time - entry.start) / f64::from(entry.length)) as f32).clamp(0.0, 1.0);
        if frac >= 1.0 {
            return None;
        }
        Some((entry.prev_frame, frac))
    }

    /// The frame's entities are drawn: forget any not asked for in it (the
    /// next sighting snaps).
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.lerps.retain(|_, l| l.seen == frame);
        self.frame = frame.wrapping_add(1);
    }

    /// Forget every blend (the extra is off, or a new level).
    pub fn clear(&mut self) {
        self.lerps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_half_the_interval_the_blend_is_the_midpoint() {
        let mut l = FrameLerps::default();
        l.blend(1, 7, 0, false, [0.0; 3], 0.0); // first sighting: snap
        // The change itself: frac 0 (the blend starts here, the frame before
        // it arrives any).
        let start = l.blend(1, 7, 1, false, [0.0; 3], 0.05).expect("blending");
        assert_eq!(start.0, 0, "blending from the old frame");
        assert!(start.1.abs() < 1e-6, "{start:?}");
        // Half of GLIDE_FRAME (0.1 s) later: the midpoint.
        let b = l.blend(1, 7, 1, false, [0.0; 3], 0.1).expect("still blending");
        assert_eq!(b.0, 0, "blending from the old frame");
        assert!((b.1 - 0.5).abs() < 1e-6, "{b:?}");
        // Fully arrived: nothing left to blend.
        assert_eq!(l.blend(1, 7, 1, false, [0.0; 3], 0.2), None);
    }

    #[test]
    fn a_group_frame_never_blends() {
        let mut l = FrameLerps::default();
        l.blend(1, 7, 0, false, [0.0; 3], 0.0);
        // The new frame is a group: snaps instead of blending from frame 0.
        assert_eq!(l.blend(1, 7, 1, true, [0.0; 3], 0.05), None);
        // And FROM a group: the pose a group frame showed was the clock's
        // (`mdl_frame_verts` at the time of the next draw would resolve it
        // afresh), so the next ordinary frame snaps too, and only the one
        // after that blends, from a named pose.
        assert_eq!(l.blend(1, 7, 2, false, [0.0; 3], 0.1), None, "from a group: snaps");
        let b = l.blend(1, 7, 3, false, [0.0; 3], 0.2).expect("blending from 2");
        assert_eq!(b.0, 2);
    }

    #[test]
    fn first_sighting_model_change_and_rewind_snap() {
        let mut l = FrameLerps::default();
        assert_eq!(l.blend(1, 7, 3, false, [0.0; 3], 1.0), None, "first sighting");
        assert_eq!(l.blend(1, 7, 3, false, [0.0; 3], 1.01), None, "frame unchanged");
        assert_eq!(l.blend(1, 8, 5, false, [0.0; 3], 1.02), None, "a model change snaps");
        assert_eq!(l.blend(1, 8, 9, false, [0.0; 3], 0.5), None, "the clock went back snaps");
    }

    #[test]
    fn a_teleport_snaps_even_with_the_same_frame() {
        let mut l = FrameLerps::default();
        l.blend(1, 7, 4, false, [0.0; 3], 1.0);
        l.blend(1, 7, 5, false, [0.0; 3], 1.01); // an ordinary change, blending
        // A jump of over 100 units, same frame: still snaps the blend (no
        // motion to show across a teleport).
        assert_eq!(l.blend(1, 7, 5, false, [200.0, 0.0, 0.0], 1.02), None);
    }

    #[test]
    fn seen_again_after_a_dropped_frame_snaps() {
        let mut l = FrameLerps::default();
        l.blend(1, 7, 0, false, [0.0; 3], 0.0);
        l.blend(1, 7, 1, false, [0.0; 3], 0.01);
        l.end_frame();
        l.end_frame(); // a frame without this entity: forgotten
        assert_eq!(l.blend(1, 7, 1, false, [0.0; 3], 0.5), None, "forgotten, so a fresh snap");
    }

    #[test]
    fn a_change_sooner_than_half_glide_blends_only_for_the_time_since() {
        // Like StepGlides' every-frame mover: frames changing faster than
        // 20 Hz (half of GLIDE_FRAME) blend for the elapsed time, not the
        // full 0.1 s, so the blend does not trail the source.
        let mut l = FrameLerps::default();
        l.blend(1, 7, 0, false, [0.0; 3], 0.0);
        l.blend(1, 7, 1, false, [0.0; 3], 0.02); // a change at 0.02s: blends over 0.1 (first change)
        // The next change comes only 0.02s later (well under half of 0.1):
        // its blend should run only 0.02s, so by +0.02s it is fully arrived.
        let b = l.blend(1, 7, 2, false, [0.0; 3], 0.04).expect("still blending just after the change");
        assert!(b.1 < 1.0);
        assert_eq!(l.blend(1, 7, 2, false, [0.0; 3], 0.06), None, "arrived after only 0.02s, not 0.1s");
    }
}
