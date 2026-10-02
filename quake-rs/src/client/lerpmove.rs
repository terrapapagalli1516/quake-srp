//! `r_lerpmove`: monsters glide between their steps — the port's 2026 extra,
//! after QuakeSpasm's `r_lerpmove` (`R_SetupEntityTransform`, r_alias.c).
//! No id file ports here: id's client draws a step mover where its last step
//! put it ([`LerpMove::Classic`]).
//!
//! id's server moves a monster (`MOVETYPE_STEP`) in its think, every 0.1 s
//! (`walkmove`/`movetogoal` in id1's `ai_*` frames), and the client draws it
//! where the last step put it: a jump ten times a second. At 1996 frame rates
//! that was invisible; at 144–480 Hz, beside a camera that moves every frame,
//! it reads as stutter. With [`LerpMove::Smooth`] the client draws a step
//! mover gliding from where it was drawn when a step came to where the step
//! put it, turning the short way round. It changes only where the model is
//! drawn: the entity's origin for everything else (trails, lights, sound,
//! collisions) stays the server's. The animation frames are a separate
//! extra, [`crate::client::lerpmodels::LerpModels::Smooth`] (QuakeSpasm's
//! `r_lerpmodels`): off here, the stepped animation is part of the look;
//! on (2026's default, beside this one), it blends too.
//!
//! **How long a glide lasts: 0.1 s, id1's think interval (and QuakeSpasm's
//! glide), from where the entity is drawn when the step comes.** A step that
//! comes early — a demo's messages (~13 Hz) beat against the monsters' 10 Hz
//! thinks, and a 144 Hz display observes thinks 0.097 or 0.104 s apart —
//! glides on from where it is drawn instead of popping to the last step's
//! end first, as QuakeSpasm's does (by up to 4 units a frame in demo1, even
//! at 480 Hz); one that comes late holds for the difference. The exception is
//! a mover stepped again within half that, 0.05 s: the server moves it every
//! frame (falling, knocked back, riding a lift, `SV_Physics_Step` and the
//! pushers), and a 0.1 s glide would trail it by a tenth of a second of its
//! speed (13.7 units behind a thrown grunt); it glides for the time since
//! its last step, one frame, and is drawn a frame behind, as in QuakeSpasm.
//! Measured against the alternatives (`quaketool framerate --lerpmove`):
//! QuakeSpasm's rule (the pops), a fixed 0.1 s for every mover (the trail),
//! and a glide as long as the time since the last step (more held frames,
//! as early and late steps alternate). Not tried: ending the glide at the
//! server's `nextthink`, which a demo does not record.
//!
//! A glide snaps instead — the entity drawn where it is — when the entity is
//! first seen (or seen again after a frame without it), when its model
//! changes, when it moves more than 100 units on an axis (id's teleport test,
//! `CL_RelinkEntities`), and when the clock goes back (a new level, a demo
//! played again). `quaketool framerate --lerpmove` measures the result.

use std::collections::HashMap;

/// How the client draws step movers (`r_lerpmove`): set by the host each
/// frame, like [`crate::stepping::Stepping`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LerpMove {
    /// id's: a step mover is drawn where its last step put it.
    #[default]
    Classic,
    /// A step mover glides from step to step (see the module doc).
    Smooth,
}

/// A glide's length: id1's monsters step every 0.1 s (their think).
pub const GLIDE: f32 = 0.1;

/// id's teleport test (`CL_RelinkEntities`): a move of more than this on
/// any axis is a jump to a new place, not a step.
const TELEPORT: f32 = 100.0;

/// Where an entity is drawn: origin and angles.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub origin: [f32; 3],
    pub angles: [f32; 3],
}

/// One step mover's glide.
#[derive(Clone, Copy, Debug)]
struct Glide {
    /// The model it was drawn with (a new one snaps).
    model: usize,
    /// Where the glide starts (drawn when the step came) and ends (the step).
    from: Pose,
    to: Pose,
    /// When the step came, and how long the glide lasts.
    start: f64,
    length: f32,
    /// [`StepGlides::frame`] when it was last drawn, and where.
    seen: u32,
    drawn: Pose,
}

impl Glide {
    /// Where the glide has got to at `time`.
    fn at(&self, time: f64) -> Pose {
        if self.length <= 0.0 {
            return self.to;
        }
        let blend = (((time - self.start) / f64::from(self.length)) as f32).clamp(0.0, 1.0);
        Pose {
            origin: std::array::from_fn(|i| self.from.origin[i] + blend * (self.to.origin[i] - self.from.origin[i])),
            angles: std::array::from_fn(|i| self.from.angles[i] + blend * short_way(self.to.angles[i] - self.from.angles[i])),
        }
    }
}

/// A turn of `d` degrees, the short way round (-180..180).
fn short_way(d: f32) -> f32 {
    if d > 180.0 {
        d - 360.0
    } else if d < -180.0 {
        d + 360.0
    } else {
        d
    }
}

/// Every step mover's glide, keyed by entity number (QuakeSpasm's
/// `previousorigin`/`currentorigin`/`movelerpstart` on each entity).
#[derive(Clone, Debug, Default)]
pub struct StepGlides {
    glides: HashMap<i32, Glide>,
    /// Counts [`StepGlides::end_frame`]s: a glide not drawn since the last
    /// one is dropped.
    frame: u32,
}

impl StepGlides {
    /// Where to draw step mover `num` (drawn with model `model`) at client
    /// time `time`, the server having put it at `origin`, `angles`.
    pub fn draw(&mut self, num: i32, model: usize, origin: [f32; 3], angles: [f32; 3], time: f64) -> Pose {
        let pose = Pose { origin, angles };
        let frame = self.frame;
        let snap = Glide { model, from: pose, to: pose, start: time, length: 0.0, seen: frame, drawn: pose };
        let glide = self.glides.entry(num).or_insert(snap);
        let jumped = (0..3).any(|i| (origin[i] - glide.to.origin[i]).abs() > TELEPORT);
        if glide.model != model || time < glide.start || jumped {
            *glide = snap;
        } else if pose != glide.to {
            // A step: glide from where it is drawn now, for GLIDE — unless it
            // comes this soon after another (the server moves it every
            // frame): then for the time since, one frame. (A snap has not
            // stepped yet: its first step glides for GLIDE.)
            let since = (time - glide.start) as f32;
            let stepped = glide.length > 0.0 || glide.from != glide.to;
            let length = if stepped && since < GLIDE / 2.0 { since } else { GLIDE };
            *glide = Glide {
                model,
                from: glide.at(time),
                to: pose,
                start: time,
                length,
                seen: frame,
                drawn: pose,
            };
        }
        glide.seen = frame;
        glide.drawn = glide.at(time);
        glide.drawn
    }

    /// Where step mover `num` was drawn by the last [`StepGlides::draw`], if
    /// it has a glide.
    pub fn drawn(&self, num: i32) -> Option<Pose> {
        self.glides.get(&num).map(|g| g.drawn)
    }

    /// The frame's step movers are drawn: forget any not drawn in it (the
    /// next sighting snaps).
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.glides.retain(|_, g| g.seen == frame);
        self.frame = frame.wrapping_add(1);
    }

    /// Forget every glide (the extra is off, or a new level).
    pub fn clear(&mut self) {
        self.glides.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mover stepping `step` units along x every `interval` s, drawn at
    /// `hz` for `secs`: the x it is drawn at in each frame.
    fn walk(glides: &mut StepGlides, hz: f64, interval: f64, step: f32, secs: f64) -> Vec<f32> {
        let mut drawn = Vec::new();
        let frames = (secs * hz).round() as usize;
        for f in 1..=frames {
            let t = f as f64 / hz;
            let x = step * (t / interval + 1e-9).floor() as f32;
            drawn.push(glides.draw(1, 7, [x, 0.0, 0.0], [0.0; 3], 10.0 + t).origin[0]);
            glides.end_frame();
        }
        drawn
    }

    #[test]
    fn a_monster_stepping_every_tenth_glides_at_its_speed() {
        // 8 units every 0.1 s at 240 Hz: after the first step, which glides
        // for GLIDE, every frame moves 8 / 24 units.
        let xs = walk(&mut StepGlides::default(), 240.0, 0.1, 8.0, 1.0);
        let moves: Vec<f32> = xs.windows(2).map(|w| w[1] - w[0]).collect();
        for (i, m) in moves.iter().enumerate().skip(24) {
            assert!((m - 8.0 / 24.0).abs() < 1e-3, "frame {i}: {m} ({xs:?})");
        }
        // Classic would have moved in 1 frame of 24.
    }

    #[test]
    fn steps_that_beat_against_the_frames_never_hold() {
        // A demo's view of a monster: a step every 0.078 s (its messages),
        // at 240 Hz. Every frame after the first step moves it.
        let xs = walk(&mut StepGlides::default(), 240.0, 0.078, 8.0, 1.0);
        let still = xs.windows(2).skip(20).filter(|w| w[1] == w[0]).count();
        assert_eq!(still, 0, "{xs:?}");
    }

    #[test]
    fn a_mover_stepped_every_frame_is_drawn_a_frame_behind() {
        // Falling or riding a lift: a new position every frame.
        let mut g = StepGlides::default();
        let xs = walk(&mut g, 144.0, 1.0 / 144.0, 2.0, 0.5);
        // (Frame 1 glides the first step for GLIDE; frame 2 cuts it short.)
        for (f, x) in xs.iter().enumerate().skip(3) {
            assert!((x - 2.0 * f as f32).abs() < 1e-3, "frame {f}: {x}, the true x {}", 2.0 * (f + 1) as f32);
        }
    }

    #[test]
    fn an_early_step_does_not_pop_back() {
        let mut g = StepGlides::default();
        g.draw(1, 7, [0.0; 3], [0.0; 3], 0.0);
        g.draw(1, 7, [8.0, 0.0, 0.0], [0.0; 3], 1.0); // after a pause: 0.1 s glide
        let mid = g.draw(1, 7, [8.0, 0.0, 0.0], [0.0; 3], 1.05).origin[0];
        assert!((mid - 4.0).abs() < 1e-3);
        // The next step comes before the glide ends: it starts from the 6 drawn.
        let next = g.draw(1, 7, [16.0, 0.0, 0.0], [0.0; 3], 1.075).origin[0];
        assert!((next - 6.0).abs() < 1e-3, "{next}");
    }

    #[test]
    fn angles_turn_the_short_way() {
        let mut g = StepGlides::default();
        g.draw(1, 7, [0.0; 3], [0.0, 350.0, 0.0], 0.0);
        g.draw(1, 7, [0.0; 3], [0.0, 10.0, 0.0], 1.0);
        let yaw = g.draw(1, 7, [0.0; 3], [0.0, 10.0, 0.0], 1.05).angles[1];
        assert!((yaw - 360.0).abs() < 1e-3, "{yaw}");
    }

    #[test]
    fn teleports_new_models_new_sightings_and_rewinds_snap() {
        let mut g = StepGlides::default();
        let x = |g: &mut StepGlides, model, x, t| g.draw(1, model, [x, 0.0, 0.0], [0.0; 3], t).origin[0];
        x(&mut g, 7, 0.0, 0.0);
        assert_eq!(x(&mut g, 7, 101.0, 1.0), 101.0, "a teleport");
        assert_eq!(x(&mut g, 8, 105.0, 1.01), 105.0, "a new model");
        assert_eq!(x(&mut g, 8, 110.0, 0.5), 110.0, "the clock went back");
        g.end_frame();
        g.end_frame(); // a frame without it: forgotten
        assert_eq!(x(&mut g, 8, 120.0, 0.6), 120.0, "seen again");
        assert!(x(&mut g, 8, 128.0, 0.65) < 128.0, "and then glides");
    }
}
