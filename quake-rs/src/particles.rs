//! Engine particles: the `particle()` builtin's visible effect.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/r_part.c` — `R_RunParticleEffect` (the spawn: per-axis position
//!   jitter, velocity from `dir`, the `(color&~7)+(rand()&7)` colour ramp, and
//!   the short `0.1*(rand()%5)` lifetime) and the per-particle integration in
//!   `R_DrawParticles` (`org += vel*frametime`, then `vel[2] -= grav` for the
//!   `pt_slowgrav` type the builtin spawns, with `grav = frametime*sv_gravity*0.05`).
//! * `WinQuake/pr_cmds.c` — `PF_particle` (builtin #48): the QuakeC entry point
//!   that forwards `(org, dir, color, count)` to `SV_StartParticle`.
//!
//! ## What this module is
//!
//! The C kept a fixed free-list of `particle_t` (`r_numparticles`, default 2048)
//! and rendered them in the client's `d_*` span pipeline. This headless port has
//! no client, so a [`ParticleSystem`] owns a plain `Vec<Particle>` that a
//! front-end (the wasm/quaketool playtest harness) advances each frame and hands
//! to [`crate::render`] to draw into the 3-D scene's z-buffer.
//!
//! ## Faithfulness and safety
//!
//! The crate is `#![forbid(unsafe_code)]` and this module never panics on data
//! derived from the QuakeC program:
//!
//! * A runaway `count` (QuakeC can ask for any number) is clamped against
//!   [`MAX_PARTICLES`], exactly as the C silently stopped spawning once its
//!   `free_particles` list was exhausted — excess particles are dropped, never
//!   allocated unbounded.
//! * The random number generator is a tiny deterministic LCG ([`Lcg`]) instead
//!   of libc `rand()`, so a given `(seed, burst)` is bit-for-bit reproducible
//!   (the C output depended on libc RNG state and was not reproducible). No
//!   external `rand` crate is pulled in.
//! * All arithmetic is plain `f32`; there is no indexing that could be out of
//!   bounds.

/// A live particle: a coloured point with a world position, a velocity, the
/// palette index it draws with, and the absolute game time it expires at.
///
/// `color` is a palette index (0..=255) into Quake's `gfx/palette.lmp`, matching
/// the C `particle_t::color`. `die` is an *absolute* time (game seconds), so the
/// per-frame [`ParticleSystem::advance`] can drop it with a single `die <= now`
/// test (the C stored `p->die = cl.time + lifetime` the same way).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Particle {
    /// World-space position (Quake units): `+X` east, `+Y` north, `+Z` up.
    pub origin: [f32; 3],
    /// World-space velocity (units/sec).
    pub velocity: [f32; 3],
    /// Palette index (0..=255) this particle draws with.
    pub color: u8,
    /// Absolute game time (seconds) at which this particle expires.
    pub die: f32,
}

/// The hard cap on simultaneously-live particles. Quake's `R_DrawParticles`
/// drew from a fixed `r_numparticles` free-list (default `MAX_PARTICLES = 2048`
/// in software WinQuake); once exhausted `R_RunParticleEffect` stopped spawning.
/// We mirror that with a `Vec` capped here, dropping any excess so a malicious /
/// buggy QuakeC `count` cannot grow memory without bound.
pub const MAX_PARTICLES: usize = 2048;

/// A tiny deterministic linear-congruential generator (the Numerical Recipes /
/// glibc `TYPE_0` constants `a = 1103515245`, `c = 12345`, modulo `2^31` via the
/// 32-bit wrap). Used in place of libc `rand()` so particle bursts are
/// reproducible and dependency-free (`std`-only).
///
/// The C `R_RunParticleEffect` used `rand()&15`, `rand()&7`, and `rand()%5`; this
/// LCG returns the same kinds of small bounded integers via [`Lcg::next_u32`] and
/// [`Lcg::next_range`], just with our own (documented) sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lcg(u32);

impl Lcg {
    /// Seed the generator. Any `u32` is a valid seed; the sequence is fully
    /// determined by it.
    pub fn new(seed: u32) -> Lcg {
        Lcg(seed)
    }

    /// Advance the state and return the next pseudo-random `u32`. Uses the
    /// classic glibc LCG recurrence with a 32-bit wrapping accumulator.
    pub fn next_u32(&mut self) -> u32 {
        // x = a*x + c (mod 2^32, via wrapping). The high bits are the most
        // "random"; callers that want a small range take them via next_range.
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.0
    }

    /// A pseudo-random integer in `0..bound`. `bound == 0` yields `0` (no
    /// modulo-by-zero), so callers never need to special-case it. Uses the upper
    /// bits (which mix better than the low bits of an LCG).
    pub fn next_range(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        // Take the high 16 bits and reduce — avoids the LCG's weak low bit.
        (self.next_u32() >> 16) % bound
    }
}

/// The live-particle pool a front-end advances and renders each frame.
///
/// Spawned by the engine `particle()` builtin (via the server's drained
/// [`crate::server::ParticleBurst`] queue), aged by gravity + their lifetime,
/// and drawn into the scene's shared z-buffer by [`crate::render`].
#[derive(Debug, Clone, Default)]
pub struct ParticleSystem {
    particles: Vec<Particle>,
}

impl ParticleSystem {
    /// An empty system (no live particles).
    pub fn new() -> ParticleSystem {
        ParticleSystem {
            particles: Vec::new(),
        }
    }

    /// Spawn one `particle()` burst, porting `R_RunParticleEffect`'s non-rocket
    /// path: for each of `count` particles, the origin is `org` plus a per-axis
    /// jitter in `[-8, 8)` (`(rand()&15)-8`), the velocity is `dir*15` (the C
    /// scaled the direction by 15), the colour is `(color & ~7) + (rand()&7)` (a
    /// small ramp around the base palette index), and the particle dies after a
    /// short `0.1*(rand()%5)` seconds (so `now + 0.0..=0.4` s, absolute).
    ///
    /// `now` is the current absolute game time (used to set the absolute `die`).
    /// `rng` is the caller's deterministic [`Lcg`]; advancing it here keeps the
    /// whole burst reproducible.
    ///
    /// SAFETY/FAITHFULNESS: total live particles are capped at [`MAX_PARTICLES`].
    /// Once the pool is full this stops adding (dropping the rest of `count`),
    /// exactly as the C bailed when its `free_particles` list was exhausted, so a
    /// runaway `count` cannot grow memory unbounded. A non-positive `count` adds
    /// nothing.
    pub fn spawn_burst(
        &mut self,
        org: [f32; 3],
        dir: [f32; 3],
        color: u8,
        count: i32,
        now: f32,
        rng: &mut Lcg,
    ) {
        if count <= 0 {
            return;
        }
        // Clamp the requested count to the remaining pool capacity so we never
        // exceed MAX_PARTICLES (the C's free-list-exhausted bail).
        let remaining = MAX_PARTICLES.saturating_sub(self.particles.len());
        let want = count as usize;
        let to_spawn = want.min(remaining);

        // color & ~7 == color & 0xF8: the base of the 8-entry colour ramp.
        let base = color & 0xF8;

        for _ in 0..to_spawn {
            // Per-axis position jitter in [-8, 8): the C `(rand()&15)-8`.
            let jitter = |rng: &mut Lcg| (rng.next_range(16) as i32 - 8) as f32;
            let origin = [
                org[0] + jitter(rng),
                org[1] + jitter(rng),
                org[2] + jitter(rng),
            ];
            // Velocity = dir*15 (the C `p->vel[j] = dir[j]*15`).
            let velocity = [dir[0] * 15.0, dir[1] * 15.0, dir[2] * 15.0];
            // Colour ramp: (color & ~7) + (rand()&7). next_range(8) is already 0..=7.
            let color = base | (rng.next_range(8) as u8);
            // Lifetime: 0.1 * (rand()%5)  =>  one of {0.0, 0.1, 0.2, 0.3, 0.4}s.
            let life = 0.1 * (rng.next_range(5) as f32);
            self.particles.push(Particle {
                origin,
                velocity,
                color,
                die: now + life,
            });
        }
    }

    /// Advance every particle one frame and retire the expired ones, porting the
    /// `pt_slowgrav` case of `R_DrawParticles`:
    ///
    /// * `origin += velocity * dt` (`p->org[j] += p->vel[j]*frametime`),
    /// * `velocity.z -= gravity * dt` (`pt_slowgrav` only nudges the Z velocity),
    /// * particles with `die <= now` are removed.
    ///
    /// `gravity` is the per-second downward acceleration the caller supplies. The
    /// C used `grav = frametime * sv_gravity * 0.05` *as the per-frame delta*, so
    /// a caller wanting C-faithful behaviour passes `gravity = sv_gravity * 0.05`
    /// (≈ 40 units/s² for the default `sv_gravity = 800`); this method multiplies
    /// by `dt` itself, so `gravity` is a proper acceleration.
    ///
    /// The C dropped expired particles at the *top* of the draw loop (before
    /// integrating); doing the integrate-then-retire here is equivalent for the
    /// visible result and keeps `die <= now` the single retirement test.
    pub fn advance(&mut self, dt: f32, now: f32, gravity: f32) {
        let dv = gravity * dt;
        for p in &mut self.particles {
            p.origin[0] += p.velocity[0] * dt;
            p.origin[1] += p.velocity[1] * dt;
            p.origin[2] += p.velocity[2] * dt;
            p.velocity[2] -= dv;
        }
        // Retire expired particles (die <= now). retain keeps the live ones.
        self.particles.retain(|p| p.die > now);
    }

    /// The live particles (read-only). A front-end maps these to
    /// `(origin, color)` and passes them to the renderer.
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// How many particles are live. Convenience for callers/tests.
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcg_is_deterministic() {
        let mut a = Lcg::new(12345);
        let mut b = Lcg::new(12345);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
        // A different seed gives a different stream.
        let mut c = Lcg::new(99);
        let mut d = Lcg::new(100);
        assert_ne!(c.next_u32(), d.next_u32());
    }

    #[test]
    fn next_range_is_bounded_and_zero_safe() {
        let mut rng = Lcg::new(7);
        for _ in 0..1000 {
            let r = rng.next_range(5);
            assert!(r < 5, "next_range(5) must be in 0..5, got {r}");
        }
        // bound == 0 must not divide by zero.
        assert_eq!(rng.next_range(0), 0);
        // bound == 1 always yields 0.
        for _ in 0..10 {
            assert_eq!(rng.next_range(1), 0);
        }
    }

    #[test]
    fn spawn_burst_adds_count_particles_in_the_color_ramp() {
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(1);
        let base: u8 = 0x90; // some palette index; ramp base = 0x90 & 0xF8 = 0x90
        sys.spawn_burst([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], base, 10, 5.0, &mut rng);
        assert_eq!(sys.len(), 10, "spawn_burst adds exactly `count` particles");
        for p in sys.particles() {
            // Colour must be in the 8-entry ramp [base&0xF8 .. base&0xF8 + 7].
            let ramp_base = base & 0xF8;
            assert!(
                p.color >= ramp_base && p.color <= ramp_base + 7,
                "color {} not in ramp [{}, {}]",
                p.color,
                ramp_base,
                ramp_base + 7
            );
            // Velocity = dir*15 on the spawned axis.
            assert_eq!(p.velocity, [15.0, 0.0, 0.0]);
            // Position jitter within [-8, 8) of the origin per axis.
            for axis in 0..3 {
                assert!(
                    p.origin[axis] >= -8.0 && p.origin[axis] < 8.0,
                    "axis {axis} jitter {} out of [-8, 8)",
                    p.origin[axis]
                );
            }
            // die is now + a short lifetime in [0, 0.4].
            assert!(p.die >= 5.0 && p.die <= 5.4, "die {} out of [5.0, 5.4]", p.die);
        }
    }

    #[test]
    fn spawn_burst_clamps_to_the_cap() {
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(2);
        // Ask for far more than the cap; must clamp to MAX_PARTICLES.
        sys.spawn_burst([0.0; 3], [0.0; 3], 0, (MAX_PARTICLES as i32) + 5000, 0.0, &mut rng);
        assert_eq!(sys.len(), MAX_PARTICLES, "spawn clamps to MAX_PARTICLES");
        // A second burst adds nothing (pool already full).
        sys.spawn_burst([0.0; 3], [0.0; 3], 0, 100, 0.0, &mut rng);
        assert_eq!(sys.len(), MAX_PARTICLES, "full pool drops further spawns");
    }

    #[test]
    fn spawn_burst_nonpositive_count_adds_nothing() {
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(3);
        sys.spawn_burst([0.0; 3], [1.0, 1.0, 1.0], 0, 0, 0.0, &mut rng);
        sys.spawn_burst([0.0; 3], [1.0, 1.0, 1.0], 0, -5, 0.0, &mut rng);
        assert!(sys.is_empty());
    }

    #[test]
    fn advance_moves_by_velocity_and_applies_gravity() {
        let mut sys = ParticleSystem::new();
        // Hand-build a single particle so the motion is exactly predictable.
        sys.particles.push(Particle {
            origin: [0.0, 0.0, 0.0],
            velocity: [10.0, 0.0, 20.0],
            color: 5,
            die: 100.0,
        });
        // dt = 0.5, gravity = 40 units/s^2 => dv = 20.
        sys.advance(0.5, 1.0, 40.0);
        let p = sys.particles()[0];
        // origin += velocity*dt: [5, 0, 10].
        assert_eq!(p.origin, [5.0, 0.0, 10.0]);
        // velocity.z -= gravity*dt: 20 - 20 = 0; x/y velocity unchanged.
        assert_eq!(p.velocity, [10.0, 0.0, 0.0]);
    }

    #[test]
    fn advance_removes_expired_particles() {
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [0.0; 3],
            color: 1,
            die: 2.0, // expires at t=2.0
        });
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [0.0; 3],
            color: 2,
            die: 10.0, // lives past t=3.0
        });
        // Advance to now = 3.0: the die=2.0 particle (die <= now) is removed.
        sys.advance(0.1, 3.0, 0.0);
        assert_eq!(sys.len(), 1, "expired particle (die <= now) removed");
        assert_eq!(sys.particles()[0].color, 2, "the live particle survives");
    }
}
