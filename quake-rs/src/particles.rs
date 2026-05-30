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

/// How a particle is animated each frame (the C `particle_t::type`,
/// `ptype_t`). Only the four kinds this port spawns are modelled; the C had a
/// couple more (`pt_static`, `pt_blob`, `pt_blob2`, `pt_grav`) that no spawn
/// path here produces.
///
/// * [`ParticleKind::SlowGrav`] — the `particle()` builtin's wall/blood burst
///   ([`ParticleSystem::spawn_burst`]): drifts with a gentle downward pull, no
///   colour cycling. The pre-existing default behaviour.
/// * [`ParticleKind::Fire`] — `pt_fire`: rises (gravity *adds* to Z), cycles
///   through `ramp3`, dies when `ramp >= 6`. (No spawn path emits this yet, but
///   the per-frame update is faithful so a future trail/rocket emitter can use
///   it.)
/// * [`ParticleKind::Explode`] — `pt_explode`: the even half of a rocket
///   explosion. Velocity *grows* `(1 + dvel)` per axis, falls under gravity, and
///   cycles through `ramp1` (yellow -> dark), dying at `ramp >= 8`.
/// * [`ParticleKind::Explode2`] — `pt_explode2`: the odd half. Velocity *shrinks*
///   `(1 - dt)` per axis, falls under gravity, cycles `ramp2`, dies at `ramp >= 8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticleKind {
    /// `pt_slowgrav`: gentle downward drift, no colour cycling (`spawn_burst`).
    SlowGrav,
    /// `pt_fire`: rises and cycles `ramp3`; dies at `ramp >= 6`.
    Fire,
    /// `pt_explode`: velocity grows, falls, cycles `ramp1`; dies at `ramp >= 8`.
    Explode,
    /// `pt_explode2`: velocity shrinks, falls, cycles `ramp2`; dies at `ramp >= 8`.
    Explode2,
}

/// A live particle: a coloured point with a world position, a velocity, the
/// palette index it draws with, and the absolute game time it expires at.
///
/// `color` is a palette index (0..=255) into Quake's `gfx/palette.lmp`, matching
/// the C `particle_t::color`. `die` is an *absolute* time (game seconds), so the
/// per-frame [`ParticleSystem::advance`] can drop it with a single `die <= now`
/// test (the C stored `p->die = cl.time + lifetime` the same way).
///
/// `kind` and `ramp` mirror the C `particle_t::type` / `particle_t::ramp`: the
/// animation rule applied each frame, and the floating colour-ramp cursor the
/// fire/explosion kinds advance and index into their ramp table.
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
    /// How this particle is animated each frame (the C `particle_t::type`).
    pub kind: ParticleKind,
    /// The colour-ramp cursor (the C `particle_t::ramp`): advanced by the
    /// fire/explosion kinds and floored to index their ramp table.
    pub ramp: f32,
}

/// `ramp1` (r_part.c): the `pt_explode` colour ramp — bright yellow fading to a
/// dark red. Indexed by `floor(ramp)` while `ramp < 8`.
const RAMP1: [u8; 8] = [0x6f, 0x6d, 0x6b, 0x69, 0x67, 0x65, 0x63, 0x61];
/// `ramp2` (r_part.c): the `pt_explode2` colour ramp (a slightly different fade).
const RAMP2: [u8; 8] = [0x6f, 0x6e, 0x6d, 0x6c, 0x6b, 0x6a, 0x68, 0x66];
/// `ramp3` (r_part.c): the `pt_fire` colour ramp. Six entries; `pt_fire` dies
/// once `ramp >= 6` so indices 6/7 are never read (the array is sized 8 in the C
/// but only the first six are initialised — we keep six and bound-check anyway).
const RAMP3: [u8; 6] = [0x6d, 0x6b, 0x06, 0x05, 0x04, 0x03];

/// Resolve the colour for a ramp-cycling particle, or `None` when it should die.
///
/// Mirrors the C `if (p->ramp >= LIMIT) p->die = -1; else p->color = ramp[ramp]`:
/// returns `None` (caller kills the particle) once `ramp >= limit`, otherwise the
/// ramp entry at `floor(ramp)`. The index is additionally bounds-checked against
/// the table length so a non-finite or pathological `ramp` can never panic — the
/// `>= limit` guard already keeps it in range for every real `dt`, but the
/// `ramp.get(idx)` is the belt-and-braces the `#![forbid(unsafe_code)]` crate
/// promises.
fn ramp_color(ramp: &[u8], cursor: f32, limit: f32) -> Option<u8> {
    // Die once the cursor reaches the limit, or if it is NaN (a pathological
    // `dt` could in principle produce one): only a value strictly in `[0, limit)`
    // keeps the particle alive. Written as an explicit `is_nan` + `>=` rather
    // than `!(cursor < limit)` so the NaN intent is obvious.
    if cursor.is_nan() || cursor >= limit {
        return None;
    }
    // cursor is in [0, limit) and finite here; floor to a table index.
    let idx = cursor.max(0.0) as usize;
    ramp.get(idx).copied()
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
                // The `particle()` builtin's burst is always the gentle-gravity
                // kind with no colour cycling (the C set `p->type = pt_slowgrav`).
                kind: ParticleKind::SlowGrav,
                ramp: 0.0,
            });
        }
    }

    /// Spawn a rocket/grenade explosion, porting `R_ParticleExplosion`: up to
    /// 1024 particles centred on `org`, alternating [`ParticleKind::Explode`]
    /// (even index) and [`ParticleKind::Explode2`] (odd index). Each starts at
    /// the bright `ramp1[0]` colour (`0x6f`), with a random ramp cursor `rand()&3`,
    /// a per-axis position jitter in `[-16, 16)` (`(rand()%32)-16`), a per-axis
    /// velocity in `[-256, 256)` (`(rand()%512)-256`), and a 5-second lifetime
    /// (`die = now + 5`).
    ///
    /// `now` is the current absolute game time; `rng` is the caller's
    /// deterministic [`Lcg`] (advanced here so the whole explosion is reproducible
    /// and dependency-free).
    ///
    /// SAFETY/FAITHFULNESS: the spawn count is clamped against the remaining pool
    /// capacity (`MAX_PARTICLES - len`), exactly as the C bailed once its
    /// `free_particles` list was exhausted (`if (!free_particles) return;`), so an
    /// explosion can never grow the pool past [`MAX_PARTICLES`].
    pub fn spawn_explosion(&mut self, org: [f32; 3], now: f32, rng: &mut Lcg) {
        let remaining = MAX_PARTICLES.saturating_sub(self.particles.len());
        let to_spawn = 1024usize.min(remaining);
        for i in 0..to_spawn {
            // Per-axis position jitter in [-16, 16): the C `(rand()%32)-16`.
            let jx = rng.next_range(32) as i32 - 16;
            let jy = rng.next_range(32) as i32 - 16;
            let jz = rng.next_range(32) as i32 - 16;
            // Per-axis velocity in [-256, 256): the C `(rand()%512)-256`.
            let vx = (rng.next_range(512) as i32 - 256) as f32;
            let vy = (rng.next_range(512) as i32 - 256) as f32;
            let vz = (rng.next_range(512) as i32 - 256) as f32;
            // Random initial ramp cursor: the C `p->ramp = rand()&3`.
            let ramp = (rng.next_u32() & 3) as f32;
            // Even i -> pt_explode, odd i -> pt_explode2 (the C `if (i & 1)` set
            // pt_explode; we keep the same even/odd split — exact assignment is
            // cosmetic since both halves spawn the same way and only differ in
            // their per-frame update).
            let kind = if i & 1 == 0 {
                ParticleKind::Explode
            } else {
                ParticleKind::Explode2
            };
            self.particles.push(Particle {
                origin: [org[0] + jx as f32, org[1] + jy as f32, org[2] + jz as f32],
                velocity: [vx, vy, vz],
                color: RAMP1[0], // 0x6f
                die: now + 5.0,
                kind,
                ramp,
            });
        }
    }

    /// Advance every particle one frame and retire the expired ones, porting the
    /// per-`type` cases of `R_DrawParticles`.
    ///
    /// Every particle first integrates its position (`org += vel*dt`,
    /// `p->org[j] += p->vel[j]*frametime`). Then, by kind:
    ///
    /// * [`ParticleKind::SlowGrav`]: `vel.z -= grav` (the gentle downward pull the
    ///   `particle()` burst uses; the pre-existing behaviour).
    /// * [`ParticleKind::Fire`]: `ramp += dt*5`; if `ramp >= 6` the particle dies,
    ///   else `color = ramp3[ramp]`; `vel.z += grav` (rises).
    /// * [`ParticleKind::Explode`]: `ramp += dt*10`; if `ramp >= 8` dies, else
    ///   `color = ramp1[ramp]`; `vel *= (1 + 4*dt)` per axis; `vel.z -= grav`.
    /// * [`ParticleKind::Explode2`]: `ramp += dt*15`; if `ramp >= 8` dies, else
    ///   `color = ramp2[ramp]`; `vel *= (1 - dt)` per axis; `vel.z -= grav`.
    ///
    /// `gravity` is the per-second downward acceleration the caller supplies. The
    /// C used `grav = frametime * sv_gravity * 0.05` *as the per-frame delta*, so
    /// a caller wanting C-faithful behaviour passes `gravity = sv_gravity * 0.05`
    /// (≈ 40 units/s² for the default `sv_gravity = 800`); this method multiplies
    /// by `dt` itself, so `gravity` is a proper acceleration. The C's `dvel`
    /// (`4*frametime`) is derived here the same way.
    ///
    /// The C dropped expired particles at the *top* of the draw loop (before
    /// integrating); doing the integrate-then-retire here is equivalent for the
    /// visible result. A ramp-driven death sets `die = now - 1` (the C
    /// `p->die = -1`) so the single `die <= now` test still retires it this frame.
    ///
    /// FAITHFULNESS/SAFETY: the ramp index is floored to a `usize` and
    /// bounds-checked against the ramp table — a particle dies (per the C's
    /// `ramp >= N` guard) before its cursor could ever index past the array end,
    /// so no out-of-range access is possible even with a pathological `dt`.
    pub fn advance(&mut self, dt: f32, now: f32, gravity: f32) {
        let grav = gravity * dt;
        let time1 = dt * 5.0;
        let time2 = dt * 10.0;
        let time3 = dt * 15.0;
        let dvel = 4.0 * dt;
        for p in &mut self.particles {
            p.origin[0] += p.velocity[0] * dt;
            p.origin[1] += p.velocity[1] * dt;
            p.origin[2] += p.velocity[2] * dt;
            match p.kind {
                ParticleKind::SlowGrav => {
                    p.velocity[2] -= grav;
                }
                ParticleKind::Fire => {
                    p.ramp += time1;
                    match ramp_color(&RAMP3, p.ramp, 6.0) {
                        Some(c) => p.color = c,
                        None => p.die = now - 1.0, // C: p->die = -1
                    }
                    p.velocity[2] += grav;
                }
                ParticleKind::Explode => {
                    p.ramp += time2;
                    match ramp_color(&RAMP1, p.ramp, 8.0) {
                        Some(c) => p.color = c,
                        None => p.die = now - 1.0,
                    }
                    // vel[i] += vel[i]*dvel  =>  vel *= (1 + dvel).
                    let s = 1.0 + dvel;
                    p.velocity[0] *= s;
                    p.velocity[1] *= s;
                    p.velocity[2] *= s;
                    p.velocity[2] -= grav;
                }
                ParticleKind::Explode2 => {
                    p.ramp += time3;
                    match ramp_color(&RAMP2, p.ramp, 8.0) {
                        Some(c) => p.color = c,
                        None => p.die = now - 1.0,
                    }
                    // vel[i] -= vel[i]*frametime  =>  vel *= (1 - dt).
                    let s = 1.0 - dt;
                    p.velocity[0] *= s;
                    p.velocity[1] *= s;
                    p.velocity[2] *= s;
                    p.velocity[2] -= grav;
                }
            }
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
            // spawn_burst particles are always the gentle-gravity kind, no ramp.
            assert_eq!(p.kind, ParticleKind::SlowGrav, "burst kind is SlowGrav");
            assert_eq!(p.ramp, 0.0, "burst ramp starts at 0");
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
            kind: ParticleKind::SlowGrav,
            ramp: 0.0,
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
            kind: ParticleKind::SlowGrav,
            ramp: 0.0,
        });
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [0.0; 3],
            color: 2,
            die: 10.0, // lives past t=3.0
            kind: ParticleKind::SlowGrav,
            ramp: 0.0,
        });
        // Advance to now = 3.0: the die=2.0 particle (die <= now) is removed.
        sys.advance(0.1, 3.0, 0.0);
        assert_eq!(sys.len(), 1, "expired particle (die <= now) removed");
        assert_eq!(sys.particles()[0].color, 2, "the live particle survives");
    }

    #[test]
    fn spawn_explosion_adds_explode_particles_at_ramp1_zero() {
        // R_ParticleExplosion spawns up to 1024 particles, alternating
        // Explode/Explode2, all starting at color ramp1[0] = 0x6f, dying at now+5.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(42);
        sys.spawn_explosion([100.0, 200.0, 300.0], 10.0, &mut rng);
        assert_eq!(sys.len(), 1024, "explosion spawns 1024 particles");

        let mut explode = 0;
        let mut explode2 = 0;
        for p in sys.particles() {
            assert_eq!(p.color, 0x6f, "initial color is ramp1[0] = 0x6f");
            assert_eq!(p.die, 15.0, "die = now + 5");
            // Position within +/-16 of the origin per axis.
            for axis in 0..3 {
                let c = [100.0, 200.0, 300.0][axis];
                assert!(
                    p.origin[axis] >= c - 16.0 && p.origin[axis] < c + 16.0,
                    "axis {axis} jitter {} out of [{}, {})",
                    p.origin[axis],
                    c - 16.0,
                    c + 16.0
                );
                // Velocity per axis in [-256, 256).
                assert!(
                    p.velocity[axis] >= -256.0 && p.velocity[axis] < 256.0,
                    "axis {axis} velocity {} out of [-256, 256)",
                    p.velocity[axis]
                );
            }
            // ramp cursor seeded with rand()&3 -> 0..=3.
            assert!(p.ramp >= 0.0 && p.ramp <= 3.0, "ramp {} out of 0..=3", p.ramp);
            match p.kind {
                ParticleKind::Explode => explode += 1,
                ParticleKind::Explode2 => explode2 += 1,
                other => panic!("unexpected explosion kind {other:?}"),
            }
        }
        // Even/odd split: 512 of each across 1024 particles.
        assert_eq!(explode, 512, "half the particles are Explode");
        assert_eq!(explode2, 512, "half the particles are Explode2");
    }

    #[test]
    fn spawn_explosion_clamps_to_remaining_capacity() {
        // With the pool already near full, an explosion only takes the slots left.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(1);
        // Fill all but 10 slots with a burst.
        sys.spawn_burst([0.0; 3], [0.0; 3], 0, (MAX_PARTICLES as i32) - 10, 0.0, &mut rng);
        assert_eq!(sys.len(), MAX_PARTICLES - 10);
        sys.spawn_explosion([0.0; 3], 0.0, &mut rng);
        assert_eq!(sys.len(), MAX_PARTICLES, "explosion clamps to the cap");
        // A second explosion on a full pool adds nothing.
        sys.spawn_explosion([0.0; 3], 0.0, &mut rng);
        assert_eq!(sys.len(), MAX_PARTICLES, "full pool drops the explosion");
    }

    #[test]
    fn advance_cycles_explode_color_through_ramp1_then_retires() {
        // An Explode particle's color must walk ramp1 (0x6f, 0x6d, ...) as its
        // ramp cursor advances, and the particle must retire once ramp >= 8.
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [0.0; 3],
            color: 0x6f,
            die: 1000.0, // far future: only a ramp-driven death can retire it
            kind: ParticleKind::Explode,
            ramp: 0.0,
        });
        // dt = 0.1 => time2 = dt*10 = 1.0, so ramp climbs by 1.0 each frame.
        // Frame 1: ramp 0 -> 1.0, color = ramp1[1] = 0x6d.
        sys.advance(0.1, 1.0, 0.0);
        assert_eq!(sys.len(), 1, "still alive at ramp 1");
        assert_eq!(sys.particles()[0].color, 0x6d, "color stepped to ramp1[1]");
        // Frames 2..7 push ramp to 7.0 -> color ramp1[7] = 0x61 (last valid).
        for _ in 0..6 {
            sys.advance(0.1, 1.0, 0.0);
        }
        assert_eq!(sys.len(), 1, "alive at ramp 7 (last valid index)");
        assert_eq!(sys.particles()[0].color, 0x61, "color at ramp1[7]");
        // One more frame: ramp -> 8.0 >= 8 => particle dies (die set to now-1).
        sys.advance(0.1, 1.0, 0.0);
        assert!(sys.is_empty(), "Explode particle retires once ramp >= 8");
    }

    #[test]
    fn advance_explode_velocity_grows_and_explode2_shrinks() {
        // Explode velocity scales by (1+4*dt) per axis; Explode2 by (1-dt).
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [100.0, 0.0, 0.0],
            color: 0x6f,
            die: 1000.0,
            kind: ParticleKind::Explode,
            ramp: 0.0,
        });
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [100.0, 0.0, 0.0],
            color: 0x6f,
            die: 1000.0,
            kind: ParticleKind::Explode2,
            ramp: 0.0,
        });
        // dt = 0.1, gravity 0 so the Z nudge does not muddy the X scaling.
        sys.advance(0.1, 1.0, 0.0);
        // Explode: 100 * (1 + 4*0.1) = 140.
        assert!((sys.particles()[0].velocity[0] - 140.0).abs() < 1e-3);
        // Explode2: 100 * (1 - 0.1) = 90.
        assert!((sys.particles()[1].velocity[0] - 90.0).abs() < 1e-3);
    }

    #[test]
    fn ramp_color_is_bounds_safe() {
        // Below the limit returns the floored index; at/above the limit and NaN
        // return None (the caller then kills the particle) — never a panic.
        assert_eq!(ramp_color(&RAMP1, 0.0, 8.0), Some(0x6f));
        assert_eq!(ramp_color(&RAMP1, 7.9, 8.0), Some(0x61));
        assert_eq!(ramp_color(&RAMP1, 8.0, 8.0), None);
        assert_eq!(ramp_color(&RAMP3, 5.5, 6.0), Some(0x03));
        assert_eq!(ramp_color(&RAMP3, 6.0, 6.0), None);
        assert_eq!(ramp_color(&RAMP1, f32::NAN, 8.0), None);
        assert_eq!(ramp_color(&RAMP1, -1.0, 8.0), Some(0x6f)); // clamped to idx 0
    }
}
