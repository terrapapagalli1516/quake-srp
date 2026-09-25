//! Temp-entity effects — the effect half of cl_tent.c's `CL_ParseTEnt`
//! (explosion/impact particles and their client-side sounds, r_part.c's
//! spawners) and cl_main.c's `CL_RelinkEntities` trail flags, shared by the
//! live and the demo client frames. (The beam half, `CL_ParseBeam` /
//! `CL_UpdateTEnts`, is [`crate::tent`].)
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_tent.c`, `WinQuake/cl_main.c`.

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

/// Map a model's header flags to its R_RocketTrail type (0 rocket, 1 grenade,
/// 2 gib-blood, 3 tracer, 4 zombie-gib, 5 tracer2, 6 voor), or `None` if the
/// model leaves no trail. Order matches CL_RelinkEntities' if/else chain.
pub fn rocket_trail_type(model_flags: i32) -> Option<i32> {
    if model_flags & MF_ROCKET != 0 {
        Some(0)
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

/// Realise one decoded [`TempEntityEvent`] into `particles`, porting the
/// effect-mapping half of `CL_ParseTEnt`: explosion types spawn a
/// 1024-particle [`ParticleSystem::spawn_explosion`], impact types a
/// `R_RunParticleEffect`-style burst with the matching colour/count, splashes a
/// small upward burst, and beams nothing. Returns `Some(sound_name)` for the types
/// that play a sound (explosions -> r_exp3; spike/super-spike -> tink1/ric*; wizard
/// -> wizard/hit; knight -> hknight/hit), else `None` (gunshot/splashes/beams).
pub fn spawn_temp_entity(
    particles: &mut ParticleSystem,
    ev: &TempEntityEvent,
    now: f32,
    rng: &mut Lcg,
) -> Option<&'static str> {
    use crate::server::te_consts::*;
    match ev.te_type {
        // Rocket explosion: R_ParticleExplosion + r_exp3 (CL_ParseTEnt).
        TE_EXPLOSION => {
            particles.spawn_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        // Tar/blob explosion (Scrag/Vore): R_BlobExplosion — distinct two-ramp
        // effect, NOT the rocket explosion (the dlight is also dropped below).
        TE_TAREXPLOSION => {
            particles.spawn_blob_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        // Coloured explosion: R_ParticleExplosion2 honouring the colour ramp
        // (color_start/color_length carried on the temp-entity event).
        TE_EXPLOSION2 => {
            particles.spawn_explosion2(
                ev.pos,
                ev.color_start as i32,
                ev.color_length as i32,
                now,
                rng,
            );
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
