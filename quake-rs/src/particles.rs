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
/// * [`ParticleKind::Grav`] — `pt_grav`: in software WinQuake (no `QUAKE2`) this
///   is identical to `pt_slowgrav` (`vel.z -= grav`); the blood/slight-blood
///   rocket-trail types spawn it. No colour cycling.
/// * [`ParticleKind::Static`] — `pt_static`: never moves or decays on its own
///   (`R_DrawParticles` has an empty `case pt_static`); only its `die` time and
///   the per-frame `org += vel*dt` apply. The tracer and voor rocket-trail types
///   spawn it (with a constant velocity, no gravity).
/// * [`ParticleKind::Blob`] — `pt_blob`: velocity *grows* `(1 + dvel)` on all
///   three axes and falls under gravity, but does **not** cycle colour (its colour
///   is fixed at spawn). The tar/blob explosion's even half and the whole of
///   `R_ParticleExplosion2` spawn it.
/// * [`ParticleKind::Blob2`] — `pt_blob2`: velocity *shrinks* `(1 - dvel)` on the
///   X/Y axes only (Z is untouched by the shrink) and falls under gravity; no
///   colour cycling. The tar/blob explosion's odd half spawns it.
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
    /// `pt_grav`: gentle downward drift (`vel.z -= grav`), no colour cycling.
    /// Identical to `SlowGrav` in software WinQuake; kept distinct to match the C
    /// `ptype_t` exactly.
    Grav,
    /// `pt_static`: never decays or accelerates on its own — only `org += vel*dt`
    /// and the absolute `die` apply. Used by the tracer/voor trail particles.
    Static,
    /// `pt_blob`: velocity grows `(1 + dvel)` on all axes, falls under gravity, no
    /// colour cycling. Tar-explosion even half + `R_ParticleExplosion2`.
    Blob,
    /// `pt_blob2`: velocity shrinks `(1 - dvel)` on X/Y only, falls under gravity,
    /// no colour cycling. Tar-explosion odd half.
    Blob2,
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

/// Normalise a 3-vector, porting the C `VectorNormalize`.
///
/// The C divided each component by the length (computing `1/length` and
/// multiplying), and for a zero-length input the divide produced `inf`/`nan` that
/// then got multiplied to leave the vector at its (zero) input. To keep the
/// `#![forbid(unsafe_code)]` crate panic- and NaN-free, a zero (or non-finite)
/// length returns the zero vector — the only zero-`dir` cell is the teleport
/// splash's `(i,j)=(0,0)`, where the C velocity was effectively `(0,0,0)` too, so
/// this matches the visible behaviour.
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 && len.is_finite() {
        let inv = 1.0 / len;
        [v[0] * inv, v[1] * inv, v[2] * inv]
    } else {
        [0.0, 0.0, 0.0]
    }
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
        // R_RunParticleEffect's `count == 1024` branch (r_part.c) is identical to
        // R_ParticleExplosion (pt_explode/pt_explode2 alternating, colour ramp1[0],
        // die + 5, the wide org/vel jitter) — the `dir`/`color` are ignored. svc_
        // particle's net `count == 255` sentinel maps to 1024, so a burst of 1024
        // must render as the fiery explosion, not the gentle SlowGrav dust.
        if count == 1024 {
            self.spawn_explosion(org, now, rng);
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

    /// Spawn a colour-mapped explosion, porting `R_ParticleExplosion2(org,
    /// colorStart, colorLength)` (the `TE_EXPLOSION2` temp entity).
    ///
    /// Spawns up to 512 [`ParticleKind::Blob`] particles centred on `org`. The
    /// colour walks `colorStart + (colorMod % colorLength)` as `colorMod`
    /// increments each particle (the C `p->color = colorStart + (colorMod %
    /// colorLength); colorMod++`). Each has a per-axis position jitter in
    /// `[-16, 16)` (`(rand()%32)-16`), a per-axis velocity in `[-256, 256)`
    /// (`(rand()%512)-256`), and a 0.3-second lifetime (`die = now + 0.3`).
    ///
    /// `color_length` of 0 would divide by zero in the C; here it is treated as 1
    /// (every particle gets `colorStart`) so the `#![forbid(unsafe_code)]` crate
    /// cannot panic on a hostile server message. The palette index wraps modulo
    /// 256 (`as u8`) exactly as the C `int -> byte` store did.
    ///
    /// SAFETY/FAITHFULNESS: the spawn count is clamped against the remaining pool
    /// capacity, exactly as the C bailed once `free_particles` was exhausted.
    pub fn spawn_explosion2(
        &mut self,
        org: [f32; 3],
        color_start: i32,
        color_length: i32,
        now: f32,
        rng: &mut Lcg,
    ) {
        let remaining = MAX_PARTICLES.saturating_sub(self.particles.len());
        let to_spawn = 512usize.min(remaining);
        // The C divides by colorLength; guard a 0/negative length to avoid a
        // panic (treat as a single-colour ramp).
        let len = if color_length <= 0 { 1 } else { color_length };
        for color_mod in 0..to_spawn {
            let jx = rng.next_range(32) as i32 - 16;
            let jy = rng.next_range(32) as i32 - 16;
            let jz = rng.next_range(32) as i32 - 16;
            let vx = (rng.next_range(512) as i32 - 256) as f32;
            let vy = (rng.next_range(512) as i32 - 256) as f32;
            let vz = (rng.next_range(512) as i32 - 256) as f32;
            // color = colorStart + (colorMod % colorLength), stored as a byte.
            let color = (color_start + (color_mod as i32 % len)) as u8;
            self.particles.push(Particle {
                origin: [org[0] + jx as f32, org[1] + jy as f32, org[2] + jz as f32],
                velocity: [vx, vy, vz],
                color,
                die: now + 0.3,
                kind: ParticleKind::Blob,
                ramp: 0.0,
            });
        }
    }

    /// Spawn a tar/blob explosion, porting `R_BlobExplosion(org)` (the
    /// `TE_TAREXPLOSION` temp entity — the tarbaby/spawn monster's death blast).
    ///
    /// Spawns up to 1024 particles centred on `org`, alternating by index:
    /// * even `i` -> [`ParticleKind::Blob2`], colour `150 + rand()%6` (a purple
    ///   ramp);
    /// * odd `i`  -> [`ParticleKind::Blob`], colour `66 + rand()%6` (a blue ramp).
    ///
    /// Each has a per-axis position jitter in `[-16, 16)`, a per-axis velocity in
    /// `[-256, 256)`, and a lifetime of `1 + (rand()&8)*0.05` seconds — note the C
    /// uses `rand()&8` (a *bit mask*, yielding only 0 or 8), so the lifetime is
    /// either `1.0` or `1.4` seconds, not a uniform spread.
    ///
    /// This is distinct from [`ParticleSystem::spawn_explosion`] (the rocket
    /// `R_ParticleExplosion`, which uses the `ramp1`/`ramp2` fire ramps and a
    /// 5-second life). The tar explosion does **not** cycle colour and does **not**
    /// allocate a dynamic light — the C `TE_TAREXPLOSION` case calls only
    /// `R_BlobExplosion` + a sound, no `CL_AllocDlight` (unlike `TE_EXPLOSION`).
    ///
    /// SAFETY/FAITHFULNESS: clamped against the remaining pool capacity.
    pub fn spawn_blob_explosion(&mut self, org: [f32; 3], now: f32, rng: &mut Lcg) {
        let remaining = MAX_PARTICLES.saturating_sub(self.particles.len());
        let to_spawn = 1024usize.min(remaining);
        for i in 0..to_spawn {
            // Lifetime: 1 + (rand()&8)*0.05 -> 1.0 (bit clear) or 1.4 (bit set).
            let life = 1.0 + (rng.next_u32() & 8) as f32 * 0.05;
            let (kind, color) = if i & 1 != 0 {
                // odd: pt_blob, color 66 + rand()%6
                (ParticleKind::Blob, (66 + rng.next_range(6)) as u8)
            } else {
                // even: pt_blob2, color 150 + rand()%6
                (ParticleKind::Blob2, (150 + rng.next_range(6)) as u8)
            };
            let jx = rng.next_range(32) as i32 - 16;
            let jy = rng.next_range(32) as i32 - 16;
            let jz = rng.next_range(32) as i32 - 16;
            let vx = (rng.next_range(512) as i32 - 256) as f32;
            let vy = (rng.next_range(512) as i32 - 256) as f32;
            let vz = (rng.next_range(512) as i32 - 256) as f32;
            self.particles.push(Particle {
                origin: [org[0] + jx as f32, org[1] + jy as f32, org[2] + jz as f32],
                velocity: [vx, vy, vz],
                color,
                die: now + life,
                kind,
                ramp: 0.0,
            });
        }
    }

    /// Spawn a lava splash, porting `R_LavaSplash(org)` (the `TE_LAVASPLASH` temp
    /// entity — Chthon rising, the lava-pool ambient burst).
    ///
    /// Iterates a 32x32 grid (`i`,`j` each from -16..16, the C's `for (i=-16;
    /// i<16; i++)` double loop with a degenerate `k<1` inner loop), spawning **one**
    /// [`ParticleKind::SlowGrav`] particle per cell — up to 1024 total. Each:
    ///
    /// * lifetime `2 + (rand()&31)*0.02` s (so `now + 2.0..=2.62`);
    /// * colour `224 + (rand()&7)` (a red/orange ramp);
    /// * a direction `dir = (j*8 + rand()&7, i*8 + rand()&7, 256)` that is
    ///   *normalised* and scaled by `vel = 50 + (rand()&63)` for the velocity;
    /// * an origin offset by the **un-normalised** `dir` X/Y and `rand()&63` in Z.
    ///
    /// The result is the characteristic upward-fanning spiral, not a 20-particle
    /// burst. Because each cell consumes several RNG draws *before* the early
    /// pool-cap bail, the draw order matches the C exactly.
    ///
    /// SAFETY/FAITHFULNESS: clamped against the remaining pool capacity (the C
    /// `if (!free_particles) return;` mid-loop).
    pub fn spawn_lava_splash(&mut self, org: [f32; 3], now: f32, rng: &mut Lcg) {
        for i in -16..16 {
            for j in -16..16 {
                if self.particles.len() >= MAX_PARTICLES {
                    return;
                }
                // Lifetime: 2 + (rand()&31)*0.02.
                let life = 2.0 + (rng.next_u32() & 31) as f32 * 0.02;
                // Color: 224 + (rand()&7).
                let color = (224 + (rng.next_u32() & 7)) as u8;
                // dir = (j*8 + rand()&7, i*8 + rand()&7, 256).
                let dir = [
                    (j * 8) as f32 + (rng.next_u32() & 7) as f32,
                    (i * 8) as f32 + (rng.next_u32() & 7) as f32,
                    256.0,
                ];
                // Origin offset by the (un-normalised) dir X/Y; Z by rand()&63.
                let origin = [
                    org[0] + dir[0],
                    org[1] + dir[1],
                    org[2] + (rng.next_u32() & 63) as f32,
                ];
                // VectorNormalize(dir); vel = 50 + (rand()&63); vel = dir*vel.
                let n = normalize(dir);
                let speed = 50.0 + (rng.next_u32() & 63) as f32;
                let velocity = [n[0] * speed, n[1] * speed, n[2] * speed];
                self.particles.push(Particle {
                    origin,
                    velocity,
                    color,
                    die: now + life,
                    kind: ParticleKind::SlowGrav,
                    ramp: 0.0,
                });
            }
        }
    }

    /// Spawn a teleport splash, porting `R_TeleportSplash(org)` (the `TE_TELEPORT`
    /// temp entity).
    ///
    /// Iterates a 3-D grid (`i`,`j` from -16..16 step 4; `k` from -24..32 step 4 —
    /// the C `for(i=-16;i<16;i+=4) for(j...) for(k=-24;k<32;k+=4)`), spawning one
    /// [`ParticleKind::SlowGrav`] particle per cell (8*8*14 = 896 cells, up to
    /// 1024). Each:
    ///
    /// * lifetime `0.2 + (rand()&7)*0.02` s (so `now + 0.2..=0.34`);
    /// * colour `7 + (rand()&7)` (a white/grey ramp);
    /// * a direction `dir = (j*8, i*8, k*8)` that is *normalised* and scaled by
    ///   `vel = 50 + (rand()&63)` for the velocity;
    /// * an origin at `org + (i,j,k)` plus a small `rand()&3` per-axis jitter.
    ///
    /// The result is the upward-and-outward column, not a generic burst.
    ///
    /// SAFETY/FAITHFULNESS: clamped against the remaining pool capacity. Note the
    /// degenerate cell at `(i,j)=(0,0)` produces a zero `dir` whose normalisation
    /// is the zero vector (the C `VectorNormalize` returns 0 length and leaves the
    /// vector at 0,0,0 after the divide-by-`1/length`); [`normalize`] returns the
    /// zero vector for a zero input, matching that velocity of 0.
    pub fn spawn_teleport_splash(&mut self, org: [f32; 3], now: f32, rng: &mut Lcg) {
        let mut i = -16;
        while i < 16 {
            let mut j = -16;
            while j < 16 {
                let mut k = -24;
                while k < 32 {
                    if self.particles.len() >= MAX_PARTICLES {
                        return;
                    }
                    // Lifetime: 0.2 + (rand()&7)*0.02.
                    let life = 0.2 + (rng.next_u32() & 7) as f32 * 0.02;
                    // Color: 7 + (rand()&7).
                    let color = (7 + (rng.next_u32() & 7)) as u8;
                    // dir = (j*8, i*8, k*8).
                    let dir = [(j * 8) as f32, (i * 8) as f32, (k * 8) as f32];
                    // Origin: org + (i,j,k) + rand()&3 per axis.
                    let origin = [
                        org[0] + i as f32 + (rng.next_u32() & 3) as f32,
                        org[1] + j as f32 + (rng.next_u32() & 3) as f32,
                        org[2] + k as f32 + (rng.next_u32() & 3) as f32,
                    ];
                    // VectorNormalize(dir); vel = 50 + (rand()&63); vel = dir*vel.
                    let n = normalize(dir);
                    let speed = 50.0 + (rng.next_u32() & 63) as f32;
                    let velocity = [n[0] * speed, n[1] * speed, n[2] * speed];
                    self.particles.push(Particle {
                        origin,
                        velocity,
                        color,
                        die: now + life,
                        kind: ParticleKind::SlowGrav,
                        ramp: 0.0,
                    });
                    k += 4;
                }
                j += 4;
            }
            i += 4;
        }
    }

    /// Spawn a rocket/projectile trail, porting `R_RocketTrail(start, end, type)`.
    ///
    /// Walks from `start` toward `end` in steps along the unit direction, spawning
    /// one particle per step. The step size `dec` is 3 units for `type < 128`; a
    /// `type >= 128` (the `0+128` the `TE_RAILTRAIL`/demo "explosion-trail" path
    /// uses) steps 1 unit at a time after subtracting 128 from `type`.
    ///
    /// The six trail types (after the `-128` adjust):
    /// * `0` rocket trail: [`ParticleKind::Fire`], `ramp = rand()&3`,
    ///   `color = ramp3[ramp]`, `org = start + (rand()%6 - 3)` per axis, `die +2 s`,
    ///   zero velocity.
    /// * `1` smoke: [`ParticleKind::Fire`], `ramp = (rand()&3) + 2`, otherwise as
    ///   type 0.
    /// * `2` blood: [`ParticleKind::Grav`], `color = 67 + (rand()&3)`, `org` jitter
    ///   `rand()%6 - 3`, `die +2 s`, zero velocity.
    /// * `3` tracer1: [`ParticleKind::Static`], `die +0.5 s`,
    ///   `color = 52 + ((tracercount&4)<<1)`, velocity perpendicular to the trail
    ///   (`±30*vec` swapped X/Y) alternating by `tracercount` parity. `org = start`.
    /// * `4` slight blood: like type 2 but advances an **extra** 3 units per step
    ///   (`len -= 3`), so half as dense.
    /// * `5` tracer2: like type 3 but `color = 230 + ((tracercount&4)<<1)`.
    /// * `6` voor trail: [`ParticleKind::Static`], `color = 9*16 + 8 + (rand()&3)`,
    ///   `die +0.3 s`, `org = start + (rand()&15 - 8)` per axis, zero velocity.
    ///
    /// `tracercount` is the C's `static int` shared across calls — it must persist
    /// between trail spawns to alternate the tracer velocity direction, so the
    /// caller passes a `&mut u32` they keep on the particle system / client.
    ///
    /// SAFETY/FAITHFULNESS: each step checks the pool cap before spawning (the C
    /// `if (!free_particles) return;`), so a long trail can never exceed
    /// [`MAX_PARTICLES`]. A `start == end` (zero-length) trail spawns nothing
    /// (`len <= 0`), matching the C `while (len > 0)`.
    pub fn spawn_rocket_trail(
        &mut self,
        start: [f32; 3],
        end: [f32; 3],
        ttype: i32,
        tracercount: &mut u32,
        now: f32,
        rng: &mut Lcg,
    ) {
        // vec = end - start; len = |vec|; vec normalised.
        let mut vec = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        let mut len = (vec[0] * vec[0] + vec[1] * vec[1] + vec[2] * vec[2]).sqrt();
        if len > 0.0 {
            vec = [vec[0] / len, vec[1] / len, vec[2] / len];
        }
        // The C `R_RocketTrail` walks `start` itself forward by `vec` each step;
        // we keep a local copy so the caller's `start` is untouched.
        let mut cur = start;

        let (dec, ttype) = if ttype < 128 {
            (3.0f32, ttype)
        } else {
            (1.0f32, ttype - 128)
        };

        while len > 0.0 {
            len -= dec;

            if self.particles.len() >= MAX_PARTICLES {
                return;
            }

            // Defaults shared by most cases: zero velocity, die = now + 2.
            let mut velocity = [0.0f32, 0.0, 0.0];
            let mut die = now + 2.0;
            let color;
            let kind;
            let mut ramp = 0.0f32;

            // Per-axis jitter helpers matching the C rand() expressions.
            let jit6 = |rng: &mut Lcg| (rng.next_range(6) as i32 - 3) as f32; // rand()%6 - 3
            let jit16 = |rng: &mut Lcg| ((rng.next_u32() & 15) as i32 - 8) as f32; // rand()&15 - 8

            let mut origin = cur;

            match ttype {
                0 => {
                    // rocket trail
                    ramp = (rng.next_u32() & 3) as f32;
                    color = RAMP3[(ramp as usize).min(RAMP3.len() - 1)];
                    kind = ParticleKind::Fire;
                    origin = [cur[0] + jit6(rng), cur[1] + jit6(rng), cur[2] + jit6(rng)];
                }
                1 => {
                    // smoke
                    ramp = ((rng.next_u32() & 3) + 2) as f32;
                    color = RAMP3[(ramp as usize).min(RAMP3.len() - 1)];
                    kind = ParticleKind::Fire;
                    origin = [cur[0] + jit6(rng), cur[1] + jit6(rng), cur[2] + jit6(rng)];
                }
                2 => {
                    // blood
                    kind = ParticleKind::Grav;
                    color = (67 + (rng.next_u32() & 3)) as u8;
                    origin = [cur[0] + jit6(rng), cur[1] + jit6(rng), cur[2] + jit6(rng)];
                }
                3 | 5 => {
                    // tracer1 / tracer2
                    die = now + 0.5;
                    kind = ParticleKind::Static;
                    color = if ttype == 3 {
                        (52 + ((*tracercount & 4) << 1)) as u8
                    } else {
                        (230 + ((*tracercount & 4) << 1)) as u8
                    };
                    let odd = *tracercount & 1;
                    *tracercount = tracercount.wrapping_add(1);
                    origin = cur;
                    if odd != 0 {
                        velocity = [30.0 * vec[1], 30.0 * -vec[0], 0.0];
                    } else {
                        velocity = [30.0 * -vec[1], 30.0 * vec[0], 0.0];
                    }
                }
                4 => {
                    // slight blood: like type 2 but advance an extra 3 units.
                    kind = ParticleKind::Grav;
                    color = (67 + (rng.next_u32() & 3)) as u8;
                    origin = [cur[0] + jit6(rng), cur[1] + jit6(rng), cur[2] + jit6(rng)];
                    len -= 3.0;
                }
                6 => {
                    // voor trail
                    color = (9 * 16 + 8 + (rng.next_u32() & 3)) as u8;
                    kind = ParticleKind::Static;
                    die = now + 0.3;
                    origin = [cur[0] + jit16(rng), cur[1] + jit16(rng), cur[2] + jit16(rng)];
                }
                _ => {
                    // Unknown type: the C `switch` would fall through with the
                    // particle left at its defaults (vel 0, die +2) and color 0.
                    // We mirror that as a harmless static particle so the pool
                    // accounting (and RNG draw count) is unchanged.
                    color = 0;
                    kind = ParticleKind::Static;
                }
            }

            self.particles.push(Particle {
                origin,
                velocity,
                color,
                die,
                kind,
                ramp,
            });

            // VectorAdd(start, vec, start): step the walk position forward.
            cur = [cur[0] + vec[0], cur[1] + vec[1], cur[2] + vec[2]];
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
                ParticleKind::Grav => {
                    // C `case pt_grav` (non-QUAKE2 falls through to pt_slowgrav):
                    // vel[2] -= grav. No colour cycling.
                    p.velocity[2] -= grav;
                }
                ParticleKind::Static => {
                    // C `case pt_static: break;` — no acceleration, no decay; only
                    // the org += vel*dt above and the absolute `die` apply.
                }
                ParticleKind::Blob => {
                    // C `case pt_blob`: vel[i] += vel[i]*dvel for all 3 axes,
                    // then vel[2] -= grav. No colour cycling.
                    let s = 1.0 + dvel;
                    p.velocity[0] *= s;
                    p.velocity[1] *= s;
                    p.velocity[2] *= s;
                    p.velocity[2] -= grav;
                }
                ParticleKind::Blob2 => {
                    // C `case pt_blob2`: vel[i] -= vel[i]*dvel for i<2 (X/Y ONLY;
                    // Z is NOT scaled), then vel[2] -= grav. No colour cycling.
                    let s = 1.0 - dvel;
                    p.velocity[0] *= s;
                    p.velocity[1] *= s;
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
    fn spawn_explosion2_honors_color_ramp_args() {
        // R_ParticleExplosion2: up to 512 pt_blob particles; color walks
        // colorStart + (colorMod % colorLength); die = now + 0.3.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(7);
        let color_start = 100;
        let color_length = 4;
        sys.spawn_explosion2([0.0, 0.0, 0.0], color_start, color_length, 2.0, &mut rng);
        assert_eq!(sys.len(), 512, "R_ParticleExplosion2 spawns 512 particles");
        for (idx, p) in sys.particles().iter().enumerate() {
            // Color cycles colorStart + (colorMod % colorLength).
            let expected = (color_start + (idx as i32 % color_length)) as u8;
            assert_eq!(p.color, expected, "particle {idx} color follows the ramp");
            assert_eq!(p.die, 2.3, "die = now + 0.3");
            assert_eq!(p.kind, ParticleKind::Blob, "explosion2 spawns pt_blob");
            assert_eq!(p.ramp, 0.0, "no ramp cycling for blob");
            // Position jitter [-16, 16), velocity [-256, 256).
            for axis in 0..3 {
                assert!(p.origin[axis] >= -16.0 && p.origin[axis] < 16.0);
                assert!(p.velocity[axis] >= -256.0 && p.velocity[axis] < 256.0);
            }
        }
        // The colour really does cycle: first four are 100,101,102,103 then wraps.
        assert_eq!(sys.particles()[0].color, 100);
        assert_eq!(sys.particles()[3].color, 103);
        assert_eq!(sys.particles()[4].color, 100, "ramp wraps modulo colorLength");
    }

    #[test]
    fn spawn_explosion2_zero_color_length_is_safe() {
        // colorLength == 0 would be a div-by-zero in the C; here every particle
        // takes colorStart and nothing panics.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(1);
        sys.spawn_explosion2([0.0; 3], 200, 0, 0.0, &mut rng);
        assert_eq!(sys.len(), 512);
        for p in sys.particles() {
            assert_eq!(p.color, 200, "zero length -> single colour");
        }
    }

    #[test]
    fn spawn_blob_explosion_two_ramps_and_no_dlight() {
        // R_BlobExplosion: 1024 particles, even -> pt_blob2 (color 150..=155),
        // odd -> pt_blob (color 66..=71); die = now + (1.0 or 1.4).
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(99);
        sys.spawn_blob_explosion([10.0, 20.0, 30.0], 5.0, &mut rng);
        assert_eq!(sys.len(), 1024, "R_BlobExplosion spawns 1024 particles");

        let mut blob = 0; // odd index, blue ramp
        let mut blob2 = 0; // even index, purple ramp
        for (idx, p) in sys.particles().iter().enumerate() {
            // Lifetime is exactly 1.0 or 1.4 (the C `(rand()&8)*0.05` bit mask).
            assert!(
                (p.die - 6.0).abs() < 1e-4 || (p.die - 6.4).abs() < 1e-4,
                "blob die {} must be now+1.0 or now+1.4",
                p.die
            );
            match p.kind {
                ParticleKind::Blob => {
                    // odd index, color 66 + rand()%6 -> 66..=71
                    assert_eq!(idx & 1, 1, "pt_blob is the odd half");
                    assert!(p.color >= 66 && p.color <= 71, "blue ramp 66..=71, got {}", p.color);
                    blob += 1;
                }
                ParticleKind::Blob2 => {
                    // even index, color 150 + rand()%6 -> 150..=155
                    assert_eq!(idx & 1, 0, "pt_blob2 is the even half");
                    assert!(p.color >= 150 && p.color <= 155, "purple ramp 150..=155, got {}", p.color);
                    blob2 += 1;
                }
                other => panic!("unexpected blob kind {other:?}"),
            }
            assert_eq!(p.ramp, 0.0, "blob explosion does not cycle colour");
        }
        assert_eq!(blob, 512, "half are pt_blob");
        assert_eq!(blob2, 512, "half are pt_blob2");
        // The blob explosion is purely particles — it never allocates a dlight
        // (the ParticleSystem has no dlight pool at all), matching the C
        // TE_TAREXPLOSION case which omits CL_AllocDlight.
    }

    #[test]
    fn spawn_lava_splash_is_the_full_grid() {
        // R_LavaSplash: a 32x32 grid -> 1024 slowgrav particles; color 224..=231,
        // die = now + 2.0..=2.62, velocity = normalize(dir)*speed (so |vel| is the
        // speed 50..=113).
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(123);
        sys.spawn_lava_splash([0.0, 0.0, 0.0], 1.0, &mut rng);
        assert_eq!(sys.len(), 1024, "lava splash is the full 32x32 grid, not a burst");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::SlowGrav);
            assert!(p.color >= 224 && p.color <= 231, "lava color 224..=231, got {}", p.color);
            assert!(p.die >= 3.0 && p.die <= 3.62, "lava die {} out of [3.0, 3.62]", p.die);
            // Velocity magnitude is the scaled speed in [50, 113] (dir is non-zero
            // because dir[2]=256 always).
            let speed = (p.velocity[0].powi(2) + p.velocity[1].powi(2) + p.velocity[2].powi(2)).sqrt();
            assert!(speed >= 49.9 && speed <= 113.1, "lava speed {speed} out of [50, 113]");
            // dir[2] = 256 dominates, so velocity Z is always positive (upward fan).
            assert!(p.velocity[2] > 0.0, "lava particles fan upward");
        }
    }

    #[test]
    fn spawn_teleport_splash_is_the_column() {
        // R_TeleportSplash: 8 * 8 * 14 = 896 slowgrav particles; color 7..=14,
        // die = now + 0.2..=0.34.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(55);
        sys.spawn_teleport_splash([0.0, 0.0, 0.0], 1.0, &mut rng);
        // i: -16..16 step 4 = 8; j: same = 8; k: -24..32 step 4 = 14.
        assert_eq!(sys.len(), 8 * 8 * 14, "teleport splash is the full 3-D grid");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::SlowGrav);
            assert!(p.color >= 7 && p.color <= 14, "teleport color 7..=14, got {}", p.color);
            assert!(p.die >= 1.2 && p.die <= 1.34, "teleport die {} out of [1.2, 1.34]", p.die);
        }
    }

    #[test]
    fn spawn_rocket_trail_type0_fire_steps_every_3_units() {
        // type 0 rocket trail: pt_fire, color from ramp3, ramp = rand()&3,
        // die = now + 2, zero velocity. Steps every 3 units along a 30-unit trail
        // => 10 particles.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(2);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0, 0.0, 0.0], [30.0, 0.0, 0.0], 0, &mut tc, 5.0, &mut rng);
        assert_eq!(sys.len(), 10, "30-unit trail / 3-unit step = 10 particles");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Fire);
            assert_eq!(p.die, 7.0, "die = now + 2");
            assert_eq!(p.velocity, [0.0, 0.0, 0.0], "fire trail has zero velocity");
            // ramp in 0..=3, color is the matching ramp3 entry.
            assert!(p.ramp >= 0.0 && p.ramp <= 3.0);
            assert_eq!(p.color, RAMP3[p.ramp as usize], "color = ramp3[ramp]");
            // Position jitter rand()%6 - 3 -> [-3, 3) around the step point on X.
            assert!(p.origin[1] >= -3.0 && p.origin[1] < 3.0);
        }
    }

    #[test]
    fn spawn_rocket_trail_type1_smoke_ramp_offset() {
        // type 1 smoke: pt_fire, ramp = (rand()&3) + 2 -> 2..=5.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(3);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0; 3], [9.0, 0.0, 0.0], 1, &mut tc, 1.0, &mut rng);
        assert_eq!(sys.len(), 3, "9-unit trail / 3 = 3 particles");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Fire);
            assert!(p.ramp >= 2.0 && p.ramp <= 5.0, "smoke ramp 2..=5, got {}", p.ramp);
            assert_eq!(p.color, RAMP3[p.ramp as usize]);
        }
    }

    #[test]
    fn spawn_rocket_trail_type2_blood() {
        // type 2 blood: pt_grav, color 67..=70, die = now + 2.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(4);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0; 3], [12.0, 0.0, 0.0], 2, &mut tc, 0.0, &mut rng);
        assert_eq!(sys.len(), 4, "12-unit trail / 3 = 4 particles");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Grav);
            assert!(p.color >= 67 && p.color <= 70, "blood color 67..=70, got {}", p.color);
            assert_eq!(p.die, 2.0);
            assert_eq!(p.velocity, [0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn spawn_rocket_trail_type4_slight_blood_is_half_density() {
        // type 4 slight blood: like type 2 but an extra `len -= 3` per step, so a
        // 12-unit trail makes only 2 particles (each step consumes 6 units).
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(5);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0; 3], [12.0, 0.0, 0.0], 4, &mut tc, 0.0, &mut rng);
        assert_eq!(sys.len(), 2, "slight blood advances 6 units/step -> half density");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Grav);
            assert!(p.color >= 67 && p.color <= 70);
        }
    }

    #[test]
    fn spawn_rocket_trail_type3_tracer_alternates_velocity_and_color() {
        // type 3 tracer1: pt_static, die = now + 0.5, perpendicular velocity that
        // alternates direction by tracercount parity; color 52 + ((tracercount&4)<<1).
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(6);
        let mut tc = 0u32;
        // Trail along +X; the perpendicular velocity is in the X/Y plane.
        sys.spawn_rocket_trail([0.0; 3], [9.0, 0.0, 0.0], 3, &mut tc, 1.0, &mut rng);
        assert_eq!(sys.len(), 3, "9-unit trail / 3 = 3 tracer particles");
        // tracercount advanced once per spawned particle.
        assert_eq!(tc, 3, "tracercount incremented per particle");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Static);
            assert_eq!(p.die, 1.5, "tracer die = now + 0.5");
            // vec = (1,0,0): velocity is +/- (0, 30, 0) alternating; |vel| = 30.
            assert_eq!(p.velocity[2], 0.0);
            let speed = (p.velocity[0].powi(2) + p.velocity[1].powi(2)).sqrt();
            assert!((speed - 30.0).abs() < 1e-4, "tracer speed 30, got {speed}");
            // color = 52 + ((tracercount&4)<<1): with tc 0,1,2 the &4 bit is 0 so
            // all three are color 52.
            assert_eq!(p.color, 52, "tracer1 color base 52 while tracercount&4 == 0");
        }
        // tracer2 (type 5) uses base 230.
        let mut sys2 = ParticleSystem::new();
        let mut rng2 = Lcg::new(6);
        let mut tc2 = 0u32;
        sys2.spawn_rocket_trail([0.0; 3], [3.0, 0.0, 0.0], 5, &mut tc2, 1.0, &mut rng2);
        assert_eq!(sys2.len(), 1);
        assert_eq!(sys2.particles()[0].color, 230, "tracer2 color base 230");
    }

    #[test]
    fn spawn_rocket_trail_type6_voor() {
        // type 6 voor: pt_static, color 9*16+8 + (rand()&3) = 152..=155, die = now+0.3,
        // origin jitter rand()&15 - 8 -> [-8, 8).
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(8);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0; 3], [6.0, 0.0, 0.0], 6, &mut tc, 2.0, &mut rng);
        assert_eq!(sys.len(), 2, "6-unit trail / 3 = 2 particles");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Static);
            assert!(p.color >= 152 && p.color <= 155, "voor color 152..=155, got {}", p.color);
            assert_eq!(p.die, 2.3, "voor die = now + 0.3");
            assert_eq!(p.velocity, [0.0, 0.0, 0.0]);
            assert!(p.origin[1] >= -8.0 && p.origin[1] < 8.0, "voor jitter [-8, 8)");
        }
    }

    #[test]
    fn spawn_rocket_trail_high_type_steps_every_unit() {
        // type >= 128 steps 1 unit at a time (dec = 1) after subtracting 128. The
        // 0+128 the rail/demo path uses is therefore a dense type-0 fire trail.
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(9);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([0.0; 3], [5.0, 0.0, 0.0], 0 + 128, &mut tc, 0.0, &mut rng);
        assert_eq!(sys.len(), 5, "5-unit trail / 1-unit step = 5 particles");
        for p in sys.particles() {
            assert_eq!(p.kind, ParticleKind::Fire, "0+128 is still a type-0 fire trail");
        }
    }

    #[test]
    fn spawn_rocket_trail_zero_length_spawns_nothing() {
        let mut sys = ParticleSystem::new();
        let mut rng = Lcg::new(10);
        let mut tc = 0u32;
        sys.spawn_rocket_trail([4.0, 5.0, 6.0], [4.0, 5.0, 6.0], 0, &mut tc, 0.0, &mut rng);
        assert!(sys.is_empty(), "a zero-length trail (start == end) spawns nothing");
    }

    #[test]
    fn blob_velocity_grows_blob2_shrinks_xy_only() {
        // pt_blob: all 3 axes scale by (1 + dvel). pt_blob2: X/Y by (1 - dvel),
        // Z untouched by the scale (then both subtract grav from Z).
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [100.0, 100.0, 100.0],
            color: 66,
            die: 1000.0,
            kind: ParticleKind::Blob,
            ramp: 0.0,
        });
        sys.particles.push(Particle {
            origin: [0.0; 3],
            velocity: [100.0, 100.0, 100.0],
            color: 150,
            die: 1000.0,
            kind: ParticleKind::Blob2,
            ramp: 0.0,
        });
        // dt = 0.1 => dvel = 0.4; gravity 0 to isolate the scaling on Z.
        sys.advance(0.1, 1.0, 0.0);
        let blob = sys.particles()[0];
        let blob2 = sys.particles()[1];
        // Blob: every axis 100 * 1.4 = 140.
        for a in 0..3 {
            assert!((blob.velocity[a] - 140.0).abs() < 1e-3, "blob axis {a}");
        }
        // Blob2: X/Y 100 * 0.6 = 60; Z UNCHANGED (the C scales only i<2).
        assert!((blob2.velocity[0] - 60.0).abs() < 1e-3);
        assert!((blob2.velocity[1] - 60.0).abs() < 1e-3);
        assert!((blob2.velocity[2] - 100.0).abs() < 1e-3, "blob2 Z is not scaled");
        // Colours do not cycle for blobs.
        assert_eq!(blob.color, 66);
        assert_eq!(blob2.color, 150);
    }

    #[test]
    fn static_particle_does_not_move_or_decay() {
        // pt_static integrates org += vel*dt but never accelerates/cycles, and only
        // its absolute `die` retires it.
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0, 0.0, 0.0],
            velocity: [10.0, 0.0, 0.0],
            color: 52,
            die: 1000.0,
            kind: ParticleKind::Static,
            ramp: 0.0,
        });
        sys.advance(0.5, 1.0, 40.0);
        let p = sys.particles()[0];
        assert_eq!(p.origin, [5.0, 0.0, 0.0], "static moves by velocity");
        assert_eq!(p.velocity, [10.0, 0.0, 0.0], "static velocity is constant (no gravity)");
        assert_eq!(p.color, 52, "static colour never changes");
    }

    #[test]
    fn grav_particle_matches_slowgrav() {
        // pt_grav is identical to pt_slowgrav in software WinQuake: vel.z -= grav.
        let mut sys = ParticleSystem::new();
        sys.particles.push(Particle {
            origin: [0.0, 0.0, 0.0],
            velocity: [10.0, 0.0, 20.0],
            color: 67,
            die: 1000.0,
            kind: ParticleKind::Grav,
            ramp: 0.0,
        });
        sys.advance(0.5, 1.0, 40.0);
        let p = sys.particles()[0];
        assert_eq!(p.origin, [5.0, 0.0, 10.0]);
        assert_eq!(p.velocity, [10.0, 0.0, 0.0], "grav pulls Z down by grav*dt");
    }

    #[test]
    fn normalize_zero_vector_is_zero() {
        assert_eq!(normalize([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        let n = normalize([3.0, 4.0, 0.0]);
        assert!((n[0] - 0.6).abs() < 1e-6 && (n[1] - 0.8).abs() < 1e-6);
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
