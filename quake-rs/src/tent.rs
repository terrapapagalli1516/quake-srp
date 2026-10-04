//! Client-side beam temp entities: the lightning bolts and the grapple beam.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/cl_tent.c` — `CL_ParseBeam` (the per-entity beam slot list:
//!   `MAX_BEAMS` slots, a new beam keyed by the same entity REPLACES that
//!   entity's beam, `endtime = cl.time + 0.2`), `CL_ParseTEnt` (the
//!   `TE_LIGHTNING1/2/3` / `TE_BEAM` cases pick `progs/bolt.mdl` /
//!   `bolt2.mdl` / `bolt3.mdl` / `beam.mdl`), and `CL_UpdateTEnts` (each
//!   frame every live beam is expanded into one bolt-model entity every 30
//!   units along the segment, with the exact integer-truncated yaw/pitch
//!   math and a `rand()%360` roll per piece).
//! * `WinQuake/cl_main.c` — `CL_ClearState` (`memset(cl_beams, 0, ...)`)
//!   on a new server, mirrored by [`Beams::clear`].
//!
//! ## What this module is
//!
//! The C kept a fixed global `beam_t cl_beams[MAX_BEAMS]` array the client
//! refreshed from the net stream and expanded into `cl_temp_entities` visedicts
//! every frame. This headless port has no client, so a [`Beams`] store owns the
//! same fixed slot array; a front-end (the wasm walk / demo player) feeds it the
//! decoded beam [`crate::server::TempEntityEvent`]s via [`Beams::parse_beam`]
//! and calls [`Beams::update`] once per rendered frame to expand the live beams
//! into [`BeamSegment`]s — one alias-model instance each, which the caller maps
//! onto its loaded `progs/bolt*.mdl` models.
//!
//! ## Faithfulness and safety
//!
//! * The per-frame piece list is capped at [`MAX_TEMP_ENTITIES`] exactly like
//!   `CL_NewTempEntity` (which returns `NULL` once `num_temp_entities` hits the
//!   cap, making `CL_UpdateTEnts` `return` mid-walk — later beams are dropped
//!   for the frame, not truncated per-beam).
//! * The C's `CL_UpdateTEnts` reuses its outer loop index `i` for the inner
//!   `org[i] += dist[i]*30` step, so after any beam emits a piece the outer
//!   scan resumes from `i = 4` while `b` keeps advancing — walking `b` past
//!   `cl_beams[MAX_BEAMS]` into adjacent globals (undefined behaviour that is
//!   benign in practice because the garbage slots fail the `endtime` check).
//!   That bug cannot be ported into safe Rust; this port scans exactly the
//!   [`MAX_BEAMS`] real slots.
//! * The roll is `rand()%360` in the C; here the caller's deterministic
//!   [`Lcg`] supplies `next_range(360)` (same distribution, our own documented
//!   sequence — matching how [`crate::particles`] replaced libc `rand()`).

use crate::particles::Lcg;

/// `MAX_BEAMS` (`client.h`): the fixed number of simultaneous beam slots.
pub const MAX_BEAMS: usize = 24;

/// `MAX_TEMP_ENTITIES` (`client.h`): the per-frame cap on expanded beam pieces
/// (`CL_NewTempEntity` returns `NULL` at this count and `CL_UpdateTEnts`
/// `return`s, dropping the rest of the frame's beams).
pub const MAX_TEMP_ENTITIES: usize = 64;

/// Which bolt model a beam renders with — the `Mod_ForName` argument of the
/// matching `CL_ParseTEnt` case. The front-end maps each variant to its loaded
/// `.mdl` via [`BeamModel::model_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeamModel {
    /// `TE_LIGHTNING1` -> `progs/bolt.mdl` (the Shambler's attack).
    Bolt,
    /// `TE_LIGHTNING2` -> `progs/bolt2.mdl` (the player's thunderbolt).
    Bolt2,
    /// `TE_LIGHTNING3` -> `progs/bolt3.mdl` (the boss/Chthon lightning).
    Bolt3,
    /// `TE_BEAM` -> `progs/beam.mdl` (the grappling-hook beam; absent from the
    /// shareware pak — see [`BeamModel::model_name`]).
    Beam,
}

impl BeamModel {
    /// Map a `TE_*` type byte to its beam model (the `CL_ParseTEnt` beam
    /// cases), or `None` for the non-beam types.
    pub fn from_te_type(te_type: u8) -> Option<BeamModel> {
        use crate::server::te_consts::*;
        match te_type {
            TE_LIGHTNING1 => Some(BeamModel::Bolt),
            TE_LIGHTNING2 => Some(BeamModel::Bolt2),
            TE_LIGHTNING3 => Some(BeamModel::Bolt3),
            TE_BEAM => Some(BeamModel::Beam),
            _ => None,
        }
    }

    /// The in-pak model path (`CL_ParseTEnt`'s `Mod_ForName` argument).
    ///
    /// The C loaded these with `Mod_ForName(name, crash=true)` — a missing
    /// model was a `Sys_Error`. `progs/beam.mdl` is NOT in the shareware
    /// `pak0.pak` (and vanilla progs never emits `TE_BEAM`, so id never hit
    /// the crash); this port's front-ends instead skip the pieces of a beam
    /// whose model is absent, consistent with their handling of every other
    /// missing model.
    pub fn model_name(self) -> &'static str {
        match self {
            BeamModel::Bolt => "progs/bolt.mdl",
            BeamModel::Bolt2 => "progs/bolt2.mdl",
            BeamModel::Bolt3 => "progs/bolt3.mdl",
            BeamModel::Beam => "progs/beam.mdl",
        }
    }
}

/// One beam slot (`beam_t`, `client.h`): the owning entity, the bolt model, the
/// expiry time and the segment endpoints. A default slot (`entity: 0`,
/// `model: None`, `endtime: 0.0`) mirrors the C's zero-initialised globals.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Beam {
    /// The owning server entity number (the net short before the coords); the
    /// slot-reuse key, and the view-entity start-tracking key.
    entity: i32,
    /// The bolt model, or `None` for a free slot (the C's `!b->model`).
    model: Option<BeamModel>,
    /// `cl.time + 0.2` at the last refresh; the slot is dead once `< cl.time`.
    endtime: f32,
    start: [f32; 3],
    end: [f32; 3],
}

const FREE_BEAM: Beam = Beam { entity: 0, model: None, endtime: 0.0, start: [0.0; 3], end: [0.0; 3] };

/// One expanded beam piece for the current frame: an alias-model instance the
/// front-end draws (the `CL_NewTempEntity` visedict `CL_UpdateTEnts` filled).
/// `pitch`/`yaw`/`roll` are the entity `angles[0..2]` in degrees, exactly as
/// the C stored them (the renderer's `ModelInstance` takes them unchanged).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamSegment {
    pub model: BeamModel,
    pub origin: [f32; 3],
    /// `angles[PITCH]`: the integer-truncated segment elevation, `0..360`.
    pub pitch: f32,
    /// `angles[YAW]`: the integer-truncated segment heading, `0..360`.
    pub yaw: f32,
    /// `angles[ROLL]`: `rand()%360` — a fresh random roll per piece per frame
    /// (what makes a bolt shimmer while it lasts).
    pub roll: f32,
}

/// The exact `CL_UpdateTEnts` pitch/yaw derivation for a beam direction
/// `dist = end - start`, including its special vertical cases. Returns
/// `(pitch, yaw)` in degrees, each in `0..360`.
///
/// The C truncates both angles through `(int)` BEFORE the negative wrap
/// (`if (yaw < 0) yaw += 360`), so bolts snap to whole degrees — preserved
/// here (the truncation is observable: a -0.5 deg heading becomes 360, not
/// 359.5). `atan2` runs in `f64` like the C's `double` math, and `forward`
/// is the `f32` hypotenuse sum the C computed in `float`.
pub fn beam_pitch_yaw(dist: [f32; 3]) -> (f32, f32) {
    if dist[1] == 0.0 && dist[0] == 0.0 {
        // Perfectly vertical: yaw 0, pitch straight up (90) or straight
        // down (270 — including the degenerate zero-length beam).
        let pitch = if dist[2] > 0.0 { 90.0 } else { 270.0 };
        (pitch, 0.0)
    } else {
        // yaw = (int)(atan2(dist[1], dist[0]) * 180 / M_PI); if (yaw < 0) yaw += 360;
        let mut yaw = (f64::atan2(f64::from(dist[1]), f64::from(dist[0])) * 180.0 / std::f64::consts::PI) as i32 as f32;
        if yaw < 0.0 {
            yaw += 360.0;
        }
        // forward = sqrt(dist[0]*dist[0] + dist[1]*dist[1])  (float math);
        // pitch = (int)(atan2(dist[2], forward) * 180 / M_PI); wrap like yaw.
        let forward = dist[0] * dist[0] + dist[1] * dist[1];
        let forward = f64::from(forward).sqrt() as f32;
        let mut pitch =
            (f64::atan2(f64::from(dist[2]), f64::from(forward)) * 180.0 / std::f64::consts::PI) as i32 as f32;
        if pitch < 0.0 {
            pitch += 360.0;
        }
        (pitch, yaw)
    }
}

/// The client beam store: the C's `beam_t cl_beams[MAX_BEAMS]` global, owned by
/// a front-end's walk/demo state instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Beams {
    beams: [Beam; MAX_BEAMS],
}

impl Default for Beams {
    fn default() -> Beams {
        Beams::new()
    }
}

impl Beams {
    /// All slots free (the zero-initialised C globals).
    pub fn new() -> Beams {
        Beams { beams: [FREE_BEAM; MAX_BEAMS] }
    }

    /// `CL_ClearState`'s `memset(cl_beams, 0, sizeof(cl_beams))`: drop every
    /// beam. Call on a new server / level change / demo loop restart.
    pub fn clear(&mut self) {
        self.beams = [FREE_BEAM; MAX_BEAMS];
    }

    /// `CL_ParseBeam`: record one decoded beam temp entity.
    ///
    /// First pass: a beam keyed by the SAME entity replaces that entity's slot
    /// (what keeps a sustained thunderbolt ONE refreshing beam instead of
    /// stacked copies). Note the C matches `b->entity == ent` even on a
    /// never-used slot (`entity` 0), faithfully kept. Second pass: take the
    /// first free slot (`model` none, or expired). Both full: the beam is
    /// dropped (the C printed "beam list overflow!").
    ///
    /// `now` is `cl.time`; the beam lives until `now + 0.2`.
    pub fn parse_beam(&mut self, entity: i32, model: BeamModel, start: [f32; 3], end: [f32; 3], now: f32) {
        // override any beam with the same entity
        for b in self.beams.iter_mut() {
            if b.entity == entity {
                *b = Beam { entity, model: Some(model), endtime: now + 0.2, start, end };
                return;
            }
        }
        // find a free beam
        for b in self.beams.iter_mut() {
            if b.model.is_none() || b.endtime < now {
                *b = Beam { entity, model: Some(model), endtime: now + 0.2, start, end };
                return;
            }
        }
        // Con_Printf ("beam list overflow!") — dropped.
    }

    /// `CL_UpdateTEnts`: expand every live beam into bolt-model pieces for this
    /// frame, clearing and filling `out` (the C reset `num_temp_entities = 0`
    /// then filled `cl_temp_entities`).
    ///
    /// `view_entity`/`view_entity_origin` port the "if coming from the player,
    /// update the start position" block: a beam owned by the view entity has
    /// its **stored** start moved to the entity's current origin each frame, so
    /// the player's thunderbolt stays anchored to the muzzle while they move.
    ///
    /// One piece is emitted every 30 units from start towards end (`d -= 30`
    /// per step, so a non-multiple tail still gets a piece), each with the
    /// shared integer pitch/yaw of the segment and a fresh `rand()%360` roll
    /// from `rng`. Hitting [`MAX_TEMP_ENTITIES`] aborts the whole walk
    /// (`CL_NewTempEntity` returned `NULL` -> `return`).
    pub fn update(
        &mut self,
        now: f32,
        view_entity: i32,
        view_entity_origin: [f32; 3],
        rng: &mut Lcg,
        out: &mut Vec<BeamSegment>,
    ) {
        out.clear(); // num_temp_entities = 0
        for b in self.beams.iter_mut() {
            let Some(model) = b.model else { continue };
            if b.endtime < now {
                continue;
            }

            // if coming from the player, update the start position
            if b.entity == view_entity {
                b.start = view_entity_origin;
            }

            // calculate pitch and yaw
            let dist = crate::math::sub(b.end, b.start);
            let (pitch, yaw) = beam_pitch_yaw(dist);

            // add new entities for the lightning
            let mut org = b.start;
            let (dir, mut d) = crate::math::normalize(dist);
            while d > 0.0 {
                if out.len() == MAX_TEMP_ENTITIES {
                    return; // CL_NewTempEntity returned NULL
                }
                out.push(BeamSegment { model, origin: org, pitch, yaw, roll: rng.next_range(360) as f32 });
                org = crate::math::vector_ma(org, 30.0, dir);
                d -= 30.0;
            }
        }
    }

    /// Whether any slot holds a beam still alive at `now` — lets a front-end
    /// skip the per-frame [`Beams::update`] walk entirely on the (overwhelmingly
    /// common) frames with no lightning on screen.
    pub fn any_live(&self, now: f32) -> bool {
        self.beams.iter().any(|b| b.model.is_some() && b.endtime >= now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `end - start` of unit length 1 emits exactly one piece at the start.
    #[test]
    fn one_segment_per_30_units_with_tail_piece() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(1);
        let mut out = Vec::new();

        // 90 units along +x => exactly 3 pieces (at 0, 30, 60).
        beams.parse_beam(5, BeamModel::Bolt2, [0.0; 3], [90.0, 0.0, 0.0], 1.0);
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), 3);
        for (i, seg) in out.iter().enumerate() {
            assert_eq!(seg.origin, [30.0 * i as f32, 0.0, 0.0]);
            assert_eq!(seg.model, BeamModel::Bolt2);
            assert_eq!(seg.yaw, 0.0, "+x heading is yaw 0");
            assert_eq!(seg.pitch, 0.0, "level beam is pitch 0");
            assert!((0.0..360.0).contains(&seg.roll), "roll is rand()%360");
        }

        // 91 units => the 1-unit tail still gets a 4th piece (d -= 30 loop).
        beams.parse_beam(5, BeamModel::Bolt2, [0.0; 3], [91.0, 0.0, 0.0], 1.0);
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), 4, "ceil(91/30) pieces");

        // A zero-length beam emits nothing (d = 0 fails `d > 0`).
        beams.parse_beam(5, BeamModel::Bolt2, [3.0; 3], [3.0; 3], 1.0);
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert!(out.is_empty());
    }

    /// The TE type -> bolt model mapping of CL_ParseTEnt's beam cases.
    #[test]
    fn te_type_selects_the_bolt_model() {
        use crate::server::te_consts::*;
        assert_eq!(BeamModel::from_te_type(TE_LIGHTNING1), Some(BeamModel::Bolt));
        assert_eq!(BeamModel::from_te_type(TE_LIGHTNING2), Some(BeamModel::Bolt2));
        assert_eq!(BeamModel::from_te_type(TE_LIGHTNING3), Some(BeamModel::Bolt3));
        assert_eq!(BeamModel::from_te_type(TE_BEAM), Some(BeamModel::Beam));
        assert_eq!(BeamModel::from_te_type(TE_EXPLOSION), None);
        assert_eq!(BeamModel::Bolt.model_name(), "progs/bolt.mdl");
        assert_eq!(BeamModel::Bolt2.model_name(), "progs/bolt2.mdl");
        assert_eq!(BeamModel::Bolt3.model_name(), "progs/bolt3.mdl");
        assert_eq!(BeamModel::Beam.model_name(), "progs/beam.mdl");
    }

    /// endtime = now + 0.2: alive until then, dead strictly after.
    #[test]
    fn beam_expires_after_point_two_seconds() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(7);
        let mut out = Vec::new();
        beams.parse_beam(3, BeamModel::Bolt, [0.0; 3], [60.0, 0.0, 0.0], 10.0);

        beams.update(10.19, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), 2, "still alive just before endtime");
        // endtime == now is NOT expired (the C check is endtime < cl.time).
        beams.update(10.2, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), 2, "alive at exactly endtime");
        beams.update(10.21, -1, [0.0; 3], &mut rng, &mut out);
        assert!(out.is_empty(), "dead past endtime");
        assert!(!beams.any_live(10.21));
        assert!(beams.any_live(10.2));
    }

    /// CL_ParseBeam's first loop: a beam keyed by the same entity REPLACES that
    /// entity's slot; different entities take separate slots.
    #[test]
    fn same_entity_replaces_its_beam_slot() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(3);
        let mut out = Vec::new();

        beams.parse_beam(7, BeamModel::Bolt2, [0.0; 3], [30.0, 0.0, 0.0], 1.0);
        beams.parse_beam(8, BeamModel::Bolt, [0.0, 100.0, 0.0], [0.0, 130.0, 0.0], 1.0);
        // Refresh entity 7's beam with a NEW end: replaces, never stacks.
        beams.parse_beam(7, BeamModel::Bolt2, [0.0; 3], [60.0, 0.0, 0.0], 1.05);

        beams.update(1.05, -1, [0.0; 3], &mut rng, &mut out);
        // Entity 7 contributes 2 pieces (60 units, the refreshed end), entity 8
        // exactly 1 (30 units) — 3 total, NOT 1+1+2 stacked copies.
        assert_eq!(out.len(), 3);
        let bolt2 = out.iter().filter(|s| s.model == BeamModel::Bolt2).count();
        assert_eq!(bolt2, 2, "entity 7's beam was replaced, not stacked");
    }

    /// The exact CL_UpdateTEnts vectoangles math: integer truncation, the
    /// negative wrap, and the two vertical special cases.
    #[test]
    fn beam_pitch_yaw_matches_the_c_math() {
        // Vertical up / down (dist[0] == dist[1] == 0).
        assert_eq!(beam_pitch_yaw([0.0, 0.0, 50.0]), (90.0, 0.0));
        assert_eq!(beam_pitch_yaw([0.0, 0.0, -50.0]), (270.0, 0.0));
        // The degenerate zero vector takes the `dist[2] > 0` else-branch: 270.
        assert_eq!(beam_pitch_yaw([0.0, 0.0, 0.0]), (270.0, 0.0));

        // Cardinal headings, level: yaw 0/90/180/270, pitch 0.
        assert_eq!(beam_pitch_yaw([10.0, 0.0, 0.0]), (0.0, 0.0));
        assert_eq!(beam_pitch_yaw([0.0, 10.0, 0.0]), (0.0, 90.0));
        assert_eq!(beam_pitch_yaw([-10.0, 0.0, 0.0]), (0.0, 180.0));
        // -y: atan2 = -90 -> (int) -90 -> +360 = 270.
        assert_eq!(beam_pitch_yaw([0.0, -10.0, 0.0]), (0.0, 270.0));

        // 45 deg up at 45 deg heading: atan2(10,10)=45.0, forward=sqrt(200),
        // atan2(sqrt(200), sqrt(200)) = 45.0.
        assert_eq!(beam_pitch_yaw([10.0, 10.0, 14.142136]), (45.0, 45.0));

        // Integer truncation BEFORE the wrap: atan2(-1, 100) deg = -0.57...,
        // (int) = 0 -> stays 0 (not 359.42 and not 360).
        assert_eq!(beam_pitch_yaw([100.0, -1.0, 0.0]), (0.0, 0.0));
        // atan2(-10, 5) deg = -63.43, (int) = -63 -> 297 (truncation toward 0,
        // NOT floor — floor would give -64 -> 296).
        assert_eq!(beam_pitch_yaw([5.0, -10.0, 0.0]).1, 297.0);
        // Same for pitch: 63.43 down -> (int)(-63.43) = -63 -> 297.
        assert_eq!(beam_pitch_yaw([5.0, 0.0, -10.0]).0, 297.0);
    }

    /// A beam owned by the view entity re-anchors its STORED start to the view
    /// entity's current origin every update (the thunderbolt tracks the player).
    #[test]
    fn view_entity_beam_start_tracks_the_player() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(11);
        let mut out = Vec::new();
        beams.parse_beam(1, BeamModel::Bolt2, [0.0; 3], [0.0, 0.0, 90.0], 2.0);

        // The player (view entity 1) moved; the beam start follows.
        beams.update(2.05, 1, [40.0, 0.0, 0.0], &mut rng, &mut out);
        assert_eq!(out[0].origin, [40.0, 0.0, 0.0], "start re-anchored");
        // And it PERSISTS (the C mutates b->start): the next update without
        // further movement still starts from the moved origin.
        beams.update(2.1, 999, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out[0].origin, [40.0, 0.0, 0.0]);
        // A beam from a DIFFERENT entity is not re-anchored.
        beams.parse_beam(6, BeamModel::Bolt, [7.0, 0.0, 0.0], [7.0, 0.0, 60.0], 2.1);
        beams.update(2.1, 1, [40.0, 0.0, 0.0], &mut rng, &mut out);
        let bolt = out.iter().find(|s| s.model == BeamModel::Bolt).unwrap();
        assert_eq!(bolt.origin, [7.0, 0.0, 0.0]);
    }

    /// The MAX_TEMP_ENTITIES cap aborts the whole expansion mid-walk, exactly
    /// like CL_NewTempEntity returning NULL.
    #[test]
    fn expansion_caps_at_max_temp_entities() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(5);
        let mut out = Vec::new();
        // Two beams of 1500 units = 50 pieces each; the second is cut short at
        // the 64-piece cap (50 + 14), not truncated per-beam.
        beams.parse_beam(2, BeamModel::Bolt, [0.0; 3], [1500.0, 0.0, 0.0], 1.0);
        beams.parse_beam(3, BeamModel::Bolt, [0.0; 3], [0.0, 1500.0, 0.0], 1.0);
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), MAX_TEMP_ENTITIES);
    }

    /// CL_ParseBeam's second loop reuses free/expired slots; 24 live beams from
    /// distinct entities fill the table and the 25th is dropped.
    #[test]
    fn slot_reuse_and_overflow() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(9);
        let mut out = Vec::new();
        // NOTE: entity numbers start at 1 — the C's zero-initialised slots all
        // carry entity 0, so an entity-0 beam would match slot 0's key.
        for e in 1..=(MAX_BEAMS as i32) {
            beams.parse_beam(e, BeamModel::Bolt, [e as f32, 0.0, 0.0], [e as f32, 10.0, 0.0], 1.0);
        }
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), MAX_BEAMS, "all 24 slots live");
        // The 25th distinct entity finds no slot: dropped (beam list overflow).
        beams.parse_beam(99, BeamModel::Bolt3, [500.0, 0.0, 0.0], [500.0, 10.0, 0.0], 1.0);
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), MAX_BEAMS);
        assert!(out.iter().all(|s| s.model == BeamModel::Bolt), "no Bolt3 made it in");
        // Once expired, the slots free up for a new beam.
        beams.parse_beam(99, BeamModel::Bolt3, [500.0, 0.0, 0.0], [500.0, 10.0, 0.0], 2.0);
        beams.update(2.0, -1, [0.0; 3], &mut rng, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].model, BeamModel::Bolt3);
    }

    /// clear() drops everything (CL_ClearState on a new server).
    #[test]
    fn clear_drops_all_beams() {
        let mut beams = Beams::new();
        let mut rng = Lcg::new(2);
        let mut out = Vec::new();
        beams.parse_beam(4, BeamModel::Bolt, [0.0; 3], [60.0, 0.0, 0.0], 1.0);
        assert!(beams.any_live(1.0));
        beams.clear();
        assert!(!beams.any_live(1.0));
        beams.update(1.0, -1, [0.0; 3], &mut rng, &mut out);
        assert!(out.is_empty());
        assert_eq!(beams, Beams::new());
    }
}
