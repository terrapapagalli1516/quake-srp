//! How a host frame of any length steps the game — the port's own, for the
//! uncapped host (id's engine never ran a frame shorter than 1/72 s).
//!
//! WinQuake's `Host_FilterTime` runs at most 72 host frames a second, and id
//! tuned the game there: the physics integrate once per frame, and several
//! client effects count in whole units per frame. Uncapped, a frame is one
//! display refresh — 1/480 s on a 480 Hz monitor — and some of those
//! per-frame steps drift with the rate: a jump rises higher, a damage flash
//! fades faster, a slow gib trails more blood (`FRAMERATE.md` measures every
//! one, and says which drifts are left and why).
//!
//! [`Stepping::Uncapped`] keeps the frame at the display rate — no fixed tick,
//! no interpolation of the simulation — and steps each integrator that drifts
//! so that a frame of any length does what a run of 1/72 s frames covering
//! the same time does in id's game:
//!
//! - **Gravity** ([`Stepping::gravity_lead`], used by `sv_phys`): the closed
//!   form of id's 72 Hz steps, exact at any frame length.
//! - **Integer fades** ([`Tick72`], used by `client::view::fade_cshifts`):
//!   id's palette-shift percents are `int`s that lose a truncation every
//!   frame, so they step in whole 1/72 s ticks.
//! - **Long-running `f32` clocks** ([`advance_clock`]: the client's
//!   `host_time`, pushers' `ltime`), kept exact beside a double.
//! - **Trails** (`particles::ParticleSystem::spawn_trail`): laid as densely
//!   as id's are at 72 Hz.
//!
//! (Demo playback needs nothing here: id's client draws it between the
//! recorded messages at any frame rate, `client::cl_demo::demo_frame`.)
//!
//! [`Stepping::Classic`], the default, is id's per-frame code unchanged: with
//! the 72 fps gate on it is WinQuake.

/// id's frame time: `Host_FilterTime`'s cap of 72 host frames a second. The
/// uncapped host steps the game as a run of these.
pub const ID_FRAMETIME: f32 = 1.0 / 72.0;

/// How the game steps a host frame (see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stepping {
    /// id's per-frame code, whatever the frame's length.
    #[default]
    Classic,
    /// A frame of any length plays as a run of 1/72 s frames would.
    Uncapped,
}

impl Stepping {
    /// What a frame of `dt` adds to an entity's vertical velocity for its
    /// move, after `SV_AddGravity` took `gravity * dt` off it. id's
    /// integrator is semi-implicit (velocity first, then `origin +=
    /// velocity * dt`), so a run of 72 Hz frames follows the true parabola
    /// lowered by `gravity * t / 144` — a jump peaks 1.6 units lower than one
    /// stepped at 480 Hz. A frame of `dt` moving with `velocity + gravity *
    /// (dt - 1/72) / 2` lands on that same curve at every rate, and on id's
    /// own points at 72 Hz. `gravity` is the entity's pull (`ent.gravity *
    /// sv_gravity`, units/s²). Classic: 0.
    pub fn gravity_lead(self, gravity: f32, dt: f32) -> f32 {
        match self {
            Stepping::Classic => 0.0,
            Stepping::Uncapped => gravity * (dt - ID_FRAMETIME) * 0.5,
        }
    }
}

/// Advance an `f32` clock `value` that goes up by every frame's time (the
/// client's `host_time`, a pusher's `ltime`) by `by`, stepped as `stepping`
/// says. Classic adds in f32, as id's code does. In f32 thousands of short
/// frames round away: at 480 Hz a clock near an hour runs 5.5% fast, and one
/// past 18 hours stops. Uncapped adds to `exact`, a double kept beside the
/// clock, and stores its rounding, so the clock stays right however long it
/// runs (a clock set from elsewhere since the last call restarts `exact`).
pub fn advance_clock(stepping: Stepping, value: &mut f32, exact: &mut f64, by: f32) {
    match stepping {
        Stepping::Classic => *value += by,
        Stepping::Uncapped => {
            if *exact as f32 != *value {
                *exact = f64::from(*value);
            }
            *exact += f64::from(by);
            *value = *exact as f32;
        }
    }
}

/// A clock that counts whole 1/72 s ticks, carrying the remainder from frame
/// to frame: the uncapped client's way to step an integrator that must move
/// in id's 72 Hz steps (see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tick72 {
    /// Ticks' worth of time not yet counted, in [0, 1). A double, so a
    /// frame's remainder is carried exactly however long the clock runs.
    carry: f64,
}

impl Tick72 {
    /// Add a frame of `dt` seconds; returns how many whole 1/72 s ticks have
    /// passed. A non-finite or negative `dt` adds nothing.
    pub fn ticks(&mut self, dt: f32) -> u32 {
        if !(dt.is_finite() && dt > 0.0) {
            return 0;
        }
        self.carry += f64::from(dt) * 72.0;
        let n = self.carry.floor();
        self.carry -= n;
        n as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `frames` frames of `dt` of id's semi-implicit gravity from rest
    /// with `v0` up; returns the height at the end.
    fn fall(stepping: Stepping, v0: f32, dt: f32, frames: usize) -> f32 {
        let g = 800.0;
        let (mut z, mut v) = (0.0f32, v0);
        for _ in 0..frames {
            v -= g * dt; // SV_AddGravity
            z += (v + stepping.gravity_lead(g, dt)) * dt;
        }
        z
    }

    #[test]
    fn classic_gravity_is_ids_and_uncapped_at_72_hz_is_too() {
        assert_eq!(Stepping::Classic.gravity_lead(800.0, 1.0 / 480.0), 0.0);
        assert_eq!(Stepping::Uncapped.gravity_lead(800.0, ID_FRAMETIME), 0.0);
    }

    #[test]
    fn gravity_lands_on_ids_72_hz_points_at_any_rate() {
        // One second of a 270 u/s jump: at 72 Hz, id's points; at 60..480 Hz
        // the uncapped step reaches the same height (Classic drifts ~1.5 u).
        let id = fall(Stepping::Classic, 270.0, ID_FRAMETIME, 72);
        for hz in [60usize, 144, 240, 480] {
            let z = fall(Stepping::Uncapped, 270.0, 1.0 / hz as f32, hz);
            assert!((z - id).abs() < 0.01, "{hz} Hz: {z} vs id's {id}");
            let classic = fall(Stepping::Classic, 270.0, 1.0 / hz as f32, hz);
            assert!((classic - id).abs() > 0.5, "{hz} Hz Classic drifts: {classic}");
        }
    }

    #[test]
    fn advance_clock_keeps_an_hour_old_clock_exact() {
        // An hour in, a second of 480 Hz frames: f32 addition gains 5.5%;
        // the uncapped clock gains nothing.
        let run = |stepping| {
            let (mut value, mut exact) = (3600.0f32, 0.0f64);
            for _ in 0..480 {
                advance_clock(stepping, &mut value, &mut exact, 1.0 / 480.0);
            }
            value - 3600.0
        };
        assert!((run(Stepping::Classic) - 1.055).abs() < 0.01, "{}", run(Stepping::Classic));
        assert!((run(Stepping::Uncapped) - 1.0).abs() < 0.001, "{}", run(Stepping::Uncapped));
        // Classic is f32 addition exactly.
        let (mut value, mut exact) = (1.5f32, 0.0f64);
        advance_clock(Stepping::Classic, &mut value, &mut exact, 0.1);
        assert_eq!(value, 1.5f32 + 0.1f32);
    }

    #[test]
    fn tick72_counts_whole_ticks_and_carries_the_rest() {
        let mut t = Tick72::default();
        let n: u32 = (0..480).map(|_| t.ticks(1.0 / 480.0)).sum();
        assert!((71..=72).contains(&n), "a second at 480 Hz is 72 ticks: {n}");
        let mut t = Tick72::default();
        let n: u32 = (0..480 * 3600).map(|_| t.ticks(1.0 / 480.0)).sum();
        assert!((72 * 3600 - 1..=72 * 3600).contains(&n), "an hour at 480 Hz: {n}");
        let mut t = Tick72::default();
        assert_eq!(t.ticks(0.1), 7);
        assert_eq!(t.ticks(0.0), 0);
        assert_eq!(t.ticks(f32::NAN), 0);
        assert_eq!(t.ticks(ID_FRAMETIME), 1);
    }
}
