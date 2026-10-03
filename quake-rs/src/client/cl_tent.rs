//! Temp-entity effects — the effect half of cl_tent.c's `CL_ParseTEnt`
//! (explosion/impact particles and their client-side sounds, r_part.c's
//! spawners) and cl_main.c's `CL_RelinkEntities` trail flags, shared by the
//! live and the demo client frames. (The beam half, `CL_ParseBeam` /
//! `CL_UpdateTEnts`, is [`crate::tent`].)
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_tent.c`, `WinQuake/cl_main.c`.

use crate::dlight::DynamicLights;
use crate::particles::{Lcg, ParticleSystem};
use crate::server::TempEntityEvent;

/// The explosion sound a rocket/grenade/tarbaby temp entity plays
/// (the C `cl_sfx_r_exp3` = `weapons/r_exp3.wav`).
const TE_EXPLOSION_SOUND: &str = "weapons/r_exp3.wav";

/// Quake alias-model header flags (the mdl `flags` field, distinct from an
/// entity's `effects`): the projectile/gib trail bits CL_RelinkEntities reads to
/// spawn R_RocketTrail behind a moving model.
const MF_ROCKET: i32 = 1;
const MF_GRENADE: i32 = 2;
const MF_GIB: i32 = 4;
const MF_TRACER: i32 = 16;
const MF_ZOMGIB: i32 = 32;
const MF_TRACER2: i32 = 64;
const MF_TRACER3: i32 = 128;

/// `R_RocketTrail`'s type for a model flagged `EF_ROCKET` (rocket and lavaball
/// fire): the one trail whose entity `CL_RelinkEntities` also lights
/// ([`crate::dlight::DynamicLights::relink_rocket`]).
pub const TRAIL_ROCKET: i32 = 0;

/// Map a model's header flags to its R_RocketTrail type (0 rocket, 1 grenade,
/// 2 gib-blood, 3 tracer, 4 zombie-gib, 5 tracer2, 6 voor), or `None` if the
/// model leaves no trail. Order matches CL_RelinkEntities' if/else chain.
pub fn rocket_trail_type(model_flags: i32) -> Option<i32> {
    if model_flags & MF_ROCKET != 0 {
        Some(TRAIL_ROCKET)
    } else if model_flags & MF_GRENADE != 0 {
        Some(1)
    } else if model_flags & MF_GIB != 0 {
        Some(2)
    } else if model_flags & MF_ZOMGIB != 0 {
        Some(4)
    } else if model_flags & MF_TRACER != 0 {
        Some(3)
    } else if model_flags & MF_TRACER2 != 0 {
        Some(5)
    } else if model_flags & MF_TRACER3 != 0 {
        Some(6)
    } else {
        None
    }
}

/// Realise one decoded [`TempEntityEvent`] into `particles` and `dlights`,
/// porting the effect-mapping half of `CL_ParseTEnt`: explosion types spawn a
/// 1024-particle [`ParticleSystem::spawn_explosion`] and the explosion's
/// dynamic light ([`DynamicLights::explosion`]), impact types a
/// `R_RunParticleEffect`-style burst with the matching colour/count, splashes a
/// small upward burst, and beams nothing. Returns `Some(sound_name)` for the types
/// that play a sound (explosions -> r_exp3; spike/super-spike -> tink1/ric*; wizard
/// -> wizard/hit; knight -> hknight/hit), else `None` (gunshot/splashes/beams).
///
/// The live walk and a recorded demo's playback both call it, with the client
/// time (`cl.time`) the message was read at as `now`.
pub fn spawn_temp_entity(
    particles: &mut ParticleSystem,
    dlights: &mut DynamicLights,
    ev: &TempEntityEvent,
    now: f32,
    rng: &mut Lcg,
) -> Option<&'static str> {
    use crate::server::te_consts::*;
    match ev.te_type {
        // Rocket explosion: R_ParticleExplosion, its light, and r_exp3
        // (CL_ParseTEnt).
        TE_EXPLOSION => {
            particles.spawn_explosion(ev.pos, now, rng);
            dlights.explosion(ev.pos, now);
            Some(TE_EXPLOSION_SOUND)
        }
        // Tar/blob explosion (Scrag/Vore): R_BlobExplosion — distinct two-ramp
        // effect, NOT the rocket explosion: no light (CL_ParseTEnt allocates
        // none for it).
        TE_TAREXPLOSION => {
            particles.spawn_blob_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        // Coloured explosion: R_ParticleExplosion2 honouring the colour ramp
        // (color_start/color_length carried on the temp-entity event), and the
        // same light as the rocket's.
        TE_EXPLOSION2 => {
            particles.spawn_explosion2(
                ev.pos,
                ev.color_start as i32,
                ev.color_length as i32,
                now,
                rng,
            );
            dlights.explosion(ev.pos, now);
            Some(TE_EXPLOSION_SOUND)
        }
        // Spike/super-spike (nailgun, Ogre/Knight nails) wall impact: the dust burst
        // then a ricochet sound — tink1 4/5 of the time, else ric1/ric2/ric3
        // (CL_ParseTEnt). The rng draw follows spawn_burst to keep C's ordering.
        TE_SPIKE | TE_SUPERSPIKE => {
            let count = if ev.te_type == TE_SPIKE { 10 } else { 20 };
            particles.spawn_burst(ev.pos, [0.0; 3], 0, count, now, rng);
            Some(if rng.next_range(5) != 0 {
                "weapons/tink1.wav"
            } else {
                match rng.next_range(4) {
                    1 => "weapons/ric1.wav",
                    2 => "weapons/ric2.wav",
                    _ => "weapons/ric3.wav",
                }
            })
        }
        // Bullet impact: dust only, NO sound (CL_ParseTEnt plays nothing for TE_GUNSHOT).
        TE_GUNSHOT => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 20, now, rng);
            None
        }
        // Scrag (wizard) spike impact -> wizard/hit.wav.
        TE_WIZSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 20, 30, now, rng);
            Some("wizard/hit.wav")
        }
        // Hell-knight spike impact -> hknight/hit.wav.
        TE_KNIGHTSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 226, 20, now, rng);
            Some("hknight/hit.wav")
        }
        // The real lava-burst spiral (R_LavaSplash), not a 20-particle puff.
        TE_LAVASPLASH => {
            particles.spawn_lava_splash(ev.pos, now, rng);
            None
        }
        // The teleport-fog column (R_TeleportSplash).
        TE_TELEPORT => {
            particles.spawn_teleport_splash(ev.pos, now, rng);
            None
        }
        _ => None, // beam/lightning types: no effect here.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::te_consts::*;

    /// `CL_ParseTEnt`'s dynamic lights: only `TE_EXPLOSION` and `TE_EXPLOSION2`
    /// call `CL_AllocDlight` (350, 0.5 s, decay 300); the tarbaby's blob, the
    /// impacts, the splashes and the beams light nothing.
    #[test]
    fn only_the_two_explosions_make_a_light() {
        let event = |te_type| TempEntityEvent {
            te_type,
            pos: [10.0, 20.0, 30.0],
            end: [10.0, 20.0, 30.0],
            entity: 0,
            color_start: 224,
            color_length: 8,
        };
        for (te, lit) in [
            (TE_EXPLOSION, true),
            (TE_EXPLOSION2, true),
            (TE_TAREXPLOSION, false),
            (TE_GUNSHOT, false),
            (TE_SPIKE, false),
            (TE_SUPERSPIKE, false),
            (TE_WIZSPIKE, false),
            (TE_KNIGHTSPIKE, false),
            (TE_LAVASPLASH, false),
            (TE_TELEPORT, false),
            (TE_LIGHTNING1, false),
            (TE_LIGHTNING2, false),
            (TE_LIGHTNING3, false),
            (TE_BEAM, false),
        ] {
            let (mut particles, mut dlights, mut rng) = (ParticleSystem::new(), DynamicLights::new(), Lcg::new(7));
            spawn_temp_entity(&mut particles, &mut dlights, &event(te), 2.0, &mut rng);
            let drawn = dlights.active(2.0);
            assert_eq!(drawn.len(), usize::from(lit), "type {te}: {drawn:?}");
            if let Some(l) = drawn.first() {
                assert_eq!((l.origin, l.radius, l.minlight, l.decay, l.key()), ([10.0, 20.0, 30.0], 350.0, 0.0, 300.0, 0));
                assert!((l.die - 2.5).abs() < 1e-6, "die = cl.time + 0.5");
            }
        }
        // Two explosions in a frame are two lights, not one.
        let (mut particles, mut dlights, mut rng) = (ParticleSystem::new(), DynamicLights::new(), Lcg::new(7));
        for _ in 0..2 {
            spawn_temp_entity(&mut particles, &mut dlights, &event(TE_EXPLOSION), 2.0, &mut rng);
        }
        assert_eq!(dlights.active_count(2.0), 2);
    }
}
