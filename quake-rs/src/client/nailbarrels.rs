//! `r_nailbarrels`: the nailgun's nails are drawn leaving its barrels — the
//! port's 2026 extra. No id file ports here: id's client draws a nail where
//! the server has it ([`NailBarrels::Classic`]).
//!
//! **Why.** id's QuakeC launches the player's nail from
//! `self.origin + '0 0 16' + v_right*ox` (`W_FireSpikes`, weapons.qc; `ox` is
//! 4, then -4, as the two barrels alternate) along the aim at 1000 units a
//! second: 6 units below the eye (`view_ofs` is `'0 0 22'`, client.qc) and 4
//! to the side. The gun is drawn where `V_CalcRefdef` (view.c) puts
//! `cl.viewent`, at the eye plus the viewsize fudge, and `v_nail.mdl`'s
//! barrels end 30 units ahead, 3.8 to each side and 13.7 below the gun's
//! origin ([`BARREL_MUZZLE`]): 12.7 below the eye at Screen size 110. So
//! every nail flies on a line 6.7 units above the barrel it leaves from, and
//! is drawn there from its first frame. At 72 Hz that first frame is 14
//! units out, beside the barrel's flank, and the next one is above the
//! muzzle; at 320x200 that is a few grey pixels. At 2026's sizes it is a
//! pyramid thousands of pixels big, and an uncapped frame rate adds frames
//! nearer still, beside and partly behind the gun (4 units out at 240 Hz, 2 at
//! 480), so the nails read as two lines starting wide of the gun and above
//! it. The super nailgun's nail flies on the centre line above its barrel
//! cluster, where the gun hides its first 20 units; it is left as id drew it.
//!
//! **What [`NailBarrels::Barrels`] draws.** Each of the player's own nails
//! ([`NailLaunches::draw`]), while the nailgun is drawn: while its tail is
//! still inside the barrel it fires from (behind the muzzle along its
//! flight), on that barrel's axis, where the gun hides it — carried by the gun
//! as it moves, so it leaves the muzzle the gun has then; past the muzzle it
//! eases back onto its own line over [`EASE`] units (or by where that line
//! meets the world, if sooner, so that it is drawn into its impact), the
//! offset held from the frame it left and fading on a smoothstep, so the
//! drawn path has no corner where it joins the line. Only where the
//! model is drawn changes: the nail itself, its collisions, its touch, its
//! sound and its impact stay the server's. A nail that hits something while
//! inside the barrel's length is gone from the server's list that frame, as
//! in id's game, so it is never drawn after; and the offset is cut short at
//! the world's surfaces ([`NailLaunches::draw`]'s `clip`), so a nail is never
//! drawn inside a wall.
//!
//! Live play only. In demo playback a nail is first seen where a recorded
//! message put it, 50 to 125 units out at a demo's ~13 messages a second
//! (`quaketool play demoN --trace`): never beside the eye, nearly on the
//! line from the muzzle already, and a recording carries neither a nail's
//! owner nor its launch to tell the player's nails by. The demos are drawn as
//! id's.

use std::collections::HashMap;

use crate::math::{angle_vectors, dot, mul_add, normalize, sub, Vec3};

/// How the client draws the player's nails (`r_nailbarrels`): set by the
/// host each frame, like [`crate::client::lerpmove::LerpMove`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NailBarrels {
    /// id's: a nail is drawn where the server has it.
    #[default]
    Classic,
    /// The player's nails leave the nailgun's barrels (the module doc).
    Barrels,
}

/// The nailgun's view model, whose barrels the nails leave.
pub const NAILGUN: &str = "progs/v_nail.mdl";

/// The nailgun's nail (`launch_spike`'s `setmodel`, weapons.qc); the super
/// nailgun's is `progs/s_spike.mdl`.
pub const NAIL: &str = "progs/spike.mdl";

/// Where the left barrel of `v_nail.mdl` (id1's pak0) ends, in the model's
/// space (x forward, y left, z up, units): the middle of its front face. Its
/// vertices' bytes times the header's `scale` plus `scale_origin`: across the
/// nine frames each barrel's front face spans x 29.2..30.7 and z -15.5..-12.0
/// (centre -13.7), y 1.5..5.9 on the left and -6.0..-1.7 on the right, and the
/// firing frames' muzzle flash points along it at y ±3.8. The right barrel
/// is the mirror image (y -3.8). quake-wasm's tests read the shareware pak's
/// model and hold these to it.
pub const BARREL_MUZZLE: Vec3 = [30.0, 3.8, -13.7];

/// How far past the muzzle a nail eases onto its own line, in units along
/// its flight (0.1 s at a nail's 1000 units a second).
pub const EASE: f32 = 100.0;

/// The gun as the frame draws it: `cl.viewent`'s origin and the basis of its
/// angles, the transform `R_AliasSetUpTransform` gives a view model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GunPose {
    pub origin: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
}

impl GunPose {
    /// The gun at `origin` turned by `angles`, a
    /// [`Viewmodel`](crate::render::Viewmodel)'s (pitch +up, yaw, roll).
    pub fn new(origin: Vec3, angles: Vec3) -> GunPose {
        // R_AliasSetUpTransform: the entity's pitch is stored "backward".
        let (forward, right, up) = angle_vectors([-angles[0], angles[1], angles[2]]);
        GunPose { origin, forward, right, up }
    }

    /// A point of the model (x forward, y left, z up) in the world.
    pub fn point(&self, m: Vec3) -> Vec3 {
        let p = mul_add(self.origin, m[0], self.forward);
        let p = mul_add(p, -m[1], self.right);
        mul_add(p, m[2], self.up)
    }

    /// The muzzle of the barrel on the `left` (else the right), and the
    /// barrel's axis.
    fn barrel(&self, left: bool) -> (Vec3, Vec3) {
        let [x, y, z] = BARREL_MUZZLE;
        (self.point([x, if left { y } else { -y }, z]), self.forward)
    }
}

/// How much of a nail's offset is left `s` units past the muzzle, for an
/// ease `len` units long: all of it inside the barrel, none from `len` on, a
/// smoothstep between (so the drawn path leaves the barrel's line and joins
/// its own without a corner).
fn ease(s: f32, len: f32) -> f32 {
    if s <= 0.0 {
        1.0
    } else if s >= len {
        0.0
    } else {
        let u = s / len;
        1.0 - u * u * (3.0 - 2.0 * u)
    }
}

/// One nail's launch, as the gun drew it.
#[derive(Clone, Copy, Debug)]
struct Launch {
    /// Its `nextthink`: `launch_spike` sets `time + 6`, so it tells a new
    /// nail in a reused edict from the one before.
    id: f32,
    /// Its flight's direction.
    dir: Vec3,
    /// Which barrel: the side of the gun it was launched on.
    left: bool,
    /// The muzzle it leaves and the barrel's axis: followed while the nail
    /// is in the barrel, held from the frame it is out.
    muzzle: Vec3,
    axis: Vec3,
    /// Out of the barrel: the offset from its own line to the muzzle's
    /// (square to its flight), held from the frame it left.
    out: Option<Vec3>,
    /// How far past the muzzle it eases: [`EASE`], or less, to where its
    /// line meets the world (set as it leaves the barrel).
    ease: f32,
    /// [`NailLaunches::frame`] when it was last drawn.
    seen: u32,
}

/// The player's nails in flight, keyed by edict.
#[derive(Clone, Debug, Default)]
pub struct NailLaunches {
    launches: HashMap<i32, Launch>,
    /// Counts [`NailLaunches::end_frame`]s: a launch not drawn since the
    /// last one is dropped.
    frame: u32,
}

impl NailLaunches {
    /// Where to draw the player's nail `ent`, whose `nextthink` is `id`, at
    /// the server's `origin` moving at `velocity`, with the nailgun drawn at
    /// `gun` this frame (`None`: no nailgun drawn — another weapon, dead,
    /// invisible, the intermission). `clip(from, to)` is how far a point can
    /// move from `from` toward `to` before the world stops it. A nail first
    /// seen without the nailgun is drawn where it is.
    pub fn draw(
        &mut self,
        ent: i32,
        id: f32,
        origin: Vec3,
        velocity: Vec3,
        gun: Option<&GunPose>,
        clip: impl Fn(Vec3, Vec3) -> Vec3,
    ) -> Vec3 {
        let frame = self.frame;
        let (dir, speed) = normalize(velocity);
        let known = self.launches.get(&ent).is_some_and(|l| l.id == id);
        if !known {
            self.launches.remove(&ent);
            let Some(g) = gun else { return origin };
            if speed == 0.0 {
                return origin;
            }
            let left = dot(sub(origin, g.origin), g.right) < 0.0;
            let (muzzle, axis) = g.barrel(left);
            let launch = Launch { id, dir, left, muzzle, axis, out: None, ease: EASE, seen: frame };
            self.launches.insert(ent, launch);
        }
        let Some(l) = self.launches.get_mut(&ent) else { return origin };
        l.seen = frame;
        let offset = match l.out {
            Some(offset) => offset,
            None => {
                // In the barrel: follow the gun (or hold its last pose without it).
                if let Some(g) = gun {
                    (l.muzzle, l.axis) = g.barrel(l.left);
                }
                // How far past the muzzle it is, along its flight.
                let s = dot(sub(origin, l.muzzle), l.dir);
                if s <= 0.0 {
                    // On the barrel's axis, where the gun hides it. (The
                    // axis, not its own line: autoaim may have pitched its
                    // flight off the gun's by up to 22 degrees.)
                    return clip(origin, mul_add(l.muzzle, s, l.axis));
                }
                // Out: from its own line abeam the muzzle, to the muzzle.
                let offset = sub(l.muzzle, mul_add(origin, -s, l.dir));
                l.out = Some(offset);
                // On its own line EASE past the muzzle, or where that line
                // meets the world, if sooner: a nail at a near wall is drawn
                // into its impact, not under it.
                let reach = clip(origin, mul_add(origin, EASE, l.dir));
                l.ease = dot(sub(reach, l.muzzle), l.dir).clamp(1.0, EASE);
                offset
            }
        };
        let k = ease(dot(sub(origin, l.muzzle), l.dir), l.ease);
        if k == 0.0 {
            return origin;
        }
        clip(origin, mul_add(origin, k, offset))
    }

    /// The frame's nails are drawn: forget any not drawn in it.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.launches.retain(|_, l| l.seen == frame);
        self.frame = frame.wrapping_add(1);
    }

    /// Forget every launch (the extra is off).
    pub fn clear(&mut self) {
        self.launches.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_WALLS: fn(Vec3, Vec3) -> Vec3 = |_, to| to;

    /// A gun at the eye's height minus 1 (Screen size 110's fudge) at
    /// `eye`, facing `yaw`, `pitch` (+up).
    fn gun(eye: Vec3, pitch: f32, yaw: f32) -> GunPose {
        GunPose::new([eye[0], eye[1], eye[2] + 1.0], [pitch, yaw, 0.0])
    }

    /// id's launch from a player whose eye is `eye`, facing `yaw` (level):
    /// the nail `t` seconds into its flight, `side` 4 (right) or -4.
    fn nail(eye: Vec3, yaw: f32, side: f32, t: f32) -> (Vec3, Vec3) {
        let (fwd, right, _) = angle_vectors([0.0, yaw, 0.0]);
        let spawn = mul_add([eye[0], eye[1], eye[2] - 6.0], side, right);
        (mul_add(spawn, 1000.0 * t, fwd), crate::math::scale(fwd, 1000.0))
    }

    /// Where a model point is in front of the eye: (forward, right, up).
    fn view(eye: Vec3, yaw: f32, p: Vec3) -> Vec3 {
        let (f, r, u) = angle_vectors([0.0, yaw, 0.0]);
        let d = sub(p, eye);
        [dot(d, f), dot(d, r), dot(d, u)]
    }

    #[test]
    fn a_nail_is_drawn_on_its_barrels_axis_then_eases_onto_its_line() {
        let (eye, yaw) = ([100.0, 200.0, 50.0], 30.0);
        let g = gun(eye, 0.0, yaw);
        for side in [4.0f32, -4.0] {
            let mut l = NailLaunches::default();
            let mut last_up = f32::NEG_INFINITY;
            for f in 1..=40 {
                let t = f as f32 / 240.0;
                let (o, v) = nail(eye, yaw, side, t);
                let d = view(eye, yaw, l.draw(7, 6.5, o, v, Some(&g), NO_WALLS));
                l.end_frame();
                let s = 1000.0 * t;
                // id's line: 6 below the eye, 4 to the side, s ahead (the
                // spawn is along the right at eye height - 6).
                assert!((d[0] - s).abs() < 1e-3, "{side} at {s}: forward {d:?}");
                assert!((d[1] - side * 0.95).abs() < 0.25, "{side} at {s}: the barrel's side, {d:?}");
                if s <= BARREL_MUZZLE[0] {
                    assert!((d[2] - (1.0 + BARREL_MUZZLE[2])).abs() < 1e-3, "in the barrel at {s}: {d:?}");
                } else if s >= BARREL_MUZZLE[0] + EASE {
                    assert!((d[2] + 6.0).abs() < 1e-3, "on its line at {s}: {d:?}");
                }
                assert!(d[2] >= last_up - 1e-4, "never back down: {} after {last_up}", d[2]);
                last_up = d[2];
            }
        }
    }

    #[test]
    fn the_ease_has_no_corner() {
        // A smoothstep: no step at either end, a gentle slope between.
        assert_eq!((ease(-5.0, EASE), ease(0.0, EASE), ease(EASE, EASE), ease(EASE + 1.0, EASE)), (1.0, 1.0, 0.0, 0.0));
        let slope = |s: f32| (ease(s + 0.01, EASE) - ease(s, EASE)) / 0.01;
        assert!(slope(0.0).abs() < 1e-3 && slope(EASE - 0.02).abs() < 1e-3);
        assert!((slope(EASE / 2.0) + 1.5 / EASE).abs() < 1e-3);
    }

    #[test]
    fn a_nail_in_the_barrel_moves_with_the_gun_and_keeps_its_exit_offset() {
        let (eye, yaw) = ([0.0, 0.0, 0.0], 0.0);
        let mut l = NailLaunches::default();
        let (o, v) = nail(eye, yaw, 4.0, 1.0 / 120.0);
        l.draw(3, 6.5, o, v, Some(&gun(eye, 0.0, yaw)), NO_WALLS);
        l.end_frame();
        // The player strafed 5 units left: still in the barrel, the nail is
        // drawn in it, where the gun is now.
        let eye2 = [0.0, 5.0, 0.0];
        let (o, v) = nail(eye, yaw, 4.0, 2.0 / 120.0);
        let d = l.draw(3, 6.5, o, v, Some(&gun(eye2, 0.0, yaw)), NO_WALLS);
        l.end_frame();
        assert!((d[1] - (5.0 - 3.8)).abs() < 1e-3, "in the barrel where the gun is: {d:?}");
        // Out of the barrel (s 3.3 past it), the offset is held: the gun
        // moving on no longer moves the nail.
        let (o, v) = nail(eye, yaw, 4.0, 4.0 / 120.0);
        let a = l.draw(3, 6.5, o, v, Some(&gun([0.0, 10.0, 0.0], 0.0, yaw)), NO_WALLS);
        l.end_frame();
        let (o2, v2) = nail(eye, yaw, 4.0, 5.0 / 120.0);
        let b = l.draw(3, 6.5, o2, v2, Some(&gun([0.0, 40.0, 0.0], 0.0, 90.0)), NO_WALLS);
        l.end_frame();
        let off = |d: Vec3, o: Vec3| sub(d, o);
        let (ka, kb) = (ease(4.0 / 120.0 * 1000.0 - 30.0, EASE), ease(5.0 / 120.0 * 1000.0 - 30.0, EASE));
        for i in 0..3 {
            assert!((off(a, o)[i] * kb - off(b, o2)[i] * ka).abs() < 1e-3, "the same offset, eased: {a:?} {b:?}");
        }
    }

    #[test]
    fn a_new_nail_in_a_reused_edict_starts_again_and_unseen_ones_are_forgotten() {
        let (eye, yaw) = ([0.0, 0.0, 0.0], 0.0);
        let g = gun(eye, 0.0, yaw);
        let mut l = NailLaunches::default();
        let (far, v) = nail(eye, yaw, 4.0, 0.5);
        assert_eq!(l.draw(3, 6.5, far, v, None, NO_WALLS), far, "first seen without the nailgun: where it is");
        l.end_frame();
        assert_eq!(l.draw(3, 6.5, far, v, Some(&g), NO_WALLS), far, "500 units out, no offset left");
        l.end_frame();
        // The edict now holds a nail fired later (another nextthink).
        let (near, v) = nail(eye, yaw, -4.0, 1.0 / 240.0);
        assert_ne!(l.draw(3, 7.0, near, v, Some(&g), NO_WALLS), near, "a new launch, in the left barrel");
        l.end_frame();
        l.end_frame();
        assert!(l.launches.is_empty(), "not drawn for a frame: forgotten");
    }

    #[test]
    fn the_world_stops_the_offset() {
        let (eye, yaw) = ([0.0, 0.0, 0.0], 0.0);
        let g = gun(eye, 0.0, yaw);
        let mut l = NailLaunches::default();
        // A floor 3 units under the nail's line.
        let floor = |_: Vec3, to: Vec3| [to[0], to[1], to[2].max(-9.0)];
        let (o, v) = nail(eye, yaw, 4.0, 1.0 / 72.0);
        let d = l.draw(3, 6.5, o, v, Some(&g), floor);
        assert_eq!(d[2], -9.0, "{d:?}");
    }

    #[test]
    fn an_autoaimed_nail_stays_in_the_barrel_then_leaves_the_muzzle_on_its_own_heading() {
        // aim() pitched the flight 15 degrees up at a monster; the gun is level.
        let (eye, g) = ([0.0, 0.0, 0.0], gun([0.0; 3], 0.0, 0.0));
        let fwd = angle_vectors([-15.0, 0.0, 0.0]).0;
        let spawn = mul_add([0.0, 0.0, -6.0], 4.0, angle_vectors([0.0; 3]).1);
        let mut l = NailLaunches::default();
        let mut out = Vec::new();
        for f in 1..=12 {
            let o = mul_add(spawn, 1000.0 * f as f32 / 240.0, fwd);
            let d = l.draw(3, 6.5, o, crate::math::scale(fwd, 1000.0), Some(&g), NO_WALLS);
            l.end_frame();
            let v = view(eye, 0.0, d);
            if v[0] < BARREL_MUZZLE[0] - 1.0 {
                assert!((v[2] - (1.0 + BARREL_MUZZLE[2])).abs() < 1e-3 && (v[1] - 3.8).abs() < 1e-3, "on the level axis: {v:?}");
            } else {
                out.push(d);
            }
        }
        // Out of the barrel it heads along its own flight, from the muzzle.
        let muzzle = g.barrel(false).0;
        let a = sub(out[0], muzzle);
        let sine = crate::math::length(crate::math::cross(a, fwd)) / crate::math::length(a);
        assert!(sine < 0.05, "leaving along its heading: {a:?}");
    }

    #[test]
    fn a_nail_meeting_a_wall_is_on_its_line_when_it_hits() {
        // A wall square across the flight 70 units ahead of the eye.
        let (eye, yaw) = ([0.0, 0.0, 0.0], 0.0);
        let g = gun(eye, 0.0, yaw);
        let wall = |from: Vec3, to: Vec3| if to[0] > 70.0 { mul_add(from, (70.0 - from[0]) / (to[0] - from[0]), sub(to, from)) } else { to };
        let mut l = NailLaunches::default();
        let mut last = None;
        for f in 1..=16 {
            let (o, v) = nail(eye, yaw, 4.0, f as f32 / 240.0);
            if o[0] > 70.0 {
                break; // the server's nail has hit the wall and is gone
            }
            last = Some((o, l.draw(3, 6.5, o, v, Some(&g), wall)));
            l.end_frame();
        }
        let (o, d) = last.expect("drawn");
        // (Without the wall, 37 units past the muzzle it would still be
        // drawn 4.7 units under its line.)
        assert!(o[0] > 62.0 && crate::math::length(sub(d, o)) < 0.25, "at {o:?} drawn {d:?}: on its line");
    }

    #[test]
    fn a_gun_pose_is_the_view_models_transform() {
        // Facing +x level, the model's left is +y, its up +z; pitched up 90
        // degrees, its forward is up.
        let g = GunPose::new([1.0, 2.0, 3.0], [0.0, 0.0, 0.0]);
        let p = g.point([10.0, 2.0, -1.0]);
        assert!(sub(p, [11.0, 4.0, 2.0]).iter().all(|c| c.abs() < 1e-5), "{p:?}");
        let g = GunPose::new([0.0; 3], [90.0, 0.0, 0.0]);
        let p = g.point([10.0, 0.0, 0.0]);
        assert!(sub(p, [0.0, 0.0, 10.0]).iter().all(|c| c.abs() < 1e-4), "{p:?}");
    }
}
