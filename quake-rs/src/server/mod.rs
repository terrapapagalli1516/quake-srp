//! The Quake server: the world model, the engine builtins, entity spawning, and
//! a minimal physics frame.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/pr_cmds.c` — the `PF_*` engine builtins (`PF_setorigin`,
//!   `PF_setmodel`, `PF_setsize`, `PF_precache_model`/`_sound`/`_file`,
//!   `PF_droptofloor`, `PF_traceline`, `PF_pointcontents`, `PF_makevectors`,
//!   `PF_cvar`, `PF_walkmove`, `PF_aim`, `PF_changeyaw`, …) and the
//!   `pr_builtin[]` dispatch table (the non-`QUAKE2` build).
//! * `WinQuake/pr_edict.c` — `ED_LoadFromFile`, `ED_ParseEdict`,
//!   `ED_ParseEpair`, `ED_NewString`, and the `SetMinMaxSize` helper.
//! * `WinQuake/common.c` — `COM_Parse` (the tokenizer).
//! * `WinQuake/sv_phys.c` — `SV_RunThink`, `SV_Physics`, `SV_Physics_Toss`,
//!   `SV_Physics_None`/`_Noclip`/`_Step`, `SV_AddGravity`, `SV_PushEntity`,
//!   `SV_CheckVelocity`.
//!
//! ## Faithfulness and safety
//!
//! This module is `#![forbid(unsafe_code)]` (crate-wide) and never panics on
//! data derived from the BSP, the entity text, or the QuakeC program:
//!
//! * The C `PF_*` builtins read/write the global block and edict array through
//!   raw pointers and `longjmp`ed out of `PR_RunError` on a fault. Here every
//!   builtin reaches the world via [`Vm::with_host`] and accesses fields/globals
//!   *by name* through the bounds-checked [`Vm`] helpers; missing definitions are
//!   no-ops rather than crashes, and a bad entity index simply does nothing.
//! * The tokenizer ([`Tokenizer`]) is a faithful transcription of `COM_Parse`
//!   working over `&str` byte positions, so a malformed entity blob yields fewer
//!   tokens rather than reading out of bounds.
//! * Spawning catches a per-entity spawn-function error and continues, exactly
//!   as the spec requires (the C aborted the host on the first `Host_Error`).
//! * The borrow discipline from `vm.rs` is respected: the host is only held out
//!   of the VM for the duration of a single trace / contents query, never across
//!   an [`Vm::execute`] call (which itself reaches the host via `with_host`).

use crate::bsp::Bsp;
use crate::math::{add as v_add, angle_vectors, sub as v_sub, Vec3};
use crate::progs::Progs;
use crate::vm::{Builtin, Host, HostTrace, Vm};
use crate::world;
use crate::{QError, Result};

mod lightstyle;
mod msg;
mod pr_edict;
mod sv_move;
mod sv_user;
mod sv_world;

pub use lightstyle::{lightstyle_scales_at, MAX_LIGHTSTYLES};
pub use msg::{
    te_consts, GameMessage, ParticleBurst, SoundEvent, StaticSound, SvcEvent, TempEntityEvent,
};
pub use sv_move::{
    sv_check_bottom, sv_move_to_goal, sv_movestep, sv_new_chase_dir, sv_step_direction,
};
pub use sv_user::v_calc_roll;
pub use sv_world::{probe_point_contents, sv_impact, sv_move, touch_triggers, MoveTrace};

pub(crate) use lightstyle::{push_lightstyle, snapshot_lightstyles};
pub(crate) use pr_edict::{ed_new_string, parse_float, parse_int, parse_vector, Tokenizer};
pub(crate) use sv_world::link_edict;

use lightstyle::{bi_lightstyle, reset_lightstyles, LIGHTSTYLES};
use msg::{
    bi_ambientsound, bi_bprint, bi_centerprint, bi_particle, bi_sound, bi_sprint, bi_writeangle,
    bi_writebyte, bi_writechar, bi_writecoord, bi_writeentity, bi_writelong, bi_writeshort,
    bi_writestring, reset_svc_recognizer, reset_temp_entity_decoder, take_messages,
    take_particle_bursts, take_sound_events, take_static_sounds, take_svc_events,
    take_temp_entities,
};
use sv_move::{bi_checkbottom, bi_movetogoal, bi_walkmove};

#[cfg(test)]
mod testutil;

// ---------------------------------------------------------------------------
// Quake constants used by the server (server.h / sv_phys.c / pr_cmds.c).
// ---------------------------------------------------------------------------

// Movetypes (server.h).
const MOVETYPE_NONE: i32 = 0;
const MOVETYPE_WALK: i32 = 3;
const MOVETYPE_STEP: i32 = 4;
const MOVETYPE_FLY: i32 = 5;
const MOVETYPE_TOSS: i32 = 6;
const MOVETYPE_PUSH: i32 = 7;
const MOVETYPE_NOCLIP: i32 = 8;
const MOVETYPE_FLYMISSILE: i32 = 9;
const MOVETYPE_BOUNCE: i32 = 10;

// Entity flags (server.h).
const FL_ONGROUND: i32 = 512;
const FL_ITEM: i32 = 256;
const FL_CLIENT: i32 = 8;
const FL_FLY: i32 = 1;
const FL_SWIM: i32 = 2;
/// `FL_WATERJUMP` — set on a player climbing out of water (server.h). The
/// water-jump/water-move paths are out of scope here, but we honour the flag by
/// keeping the player out of the normal walk path (matching the C order).
const FL_WATERJUMP: i32 = 2048;
/// `FL_MONSTER` (server.h): set on AI-driven entities (grunts, dogs, …). Read by
/// the monster-movement builtins so non-monster callers are unaffected.
#[allow(dead_code)]
const FL_MONSTER: i32 = 32;
/// `FL_PARTIALGROUND` (server.h): set by `SV_FixCheckBottom` when a monster has
/// no clean standing position (e.g. a bridge pulled out underneath it). It lets
/// [`sv_movestep`] keep moving / fall instead of refusing every step.
const FL_PARTIALGROUND: i32 = 1024;
/// `FL_INWATER` (server.h): set while a monster's box is in water. Unused by the
/// walking path but defined for completeness with the C flag set.
#[allow(dead_code)]
const FL_INWATER: i32 = 16;

// Solid types (server.h). SOLID_NOT/SOLID_TRIGGER do not block a move.
const SOLID_NOT: i32 = 0;
const SOLID_TRIGGER: i32 = 1;
const SOLID_BBOX: i32 = 2;
const SOLID_SLIDEBOX: i32 = 3;
const SOLID_BSP: i32 = 4;

/// `CONTENTS_SOLID` / `CONTENTS_EMPTY` (bsp.h): the two point-contents values
/// [`sv_check_bottom`] and [`sv_movestep`] test against (re-stated here so the
/// movement code reads naturally without importing the whole bsp contents set).
const CONTENTS_SOLID: i32 = -2;
const CONTENTS_EMPTY: i32 = -1;

// ---------------------------------------------------------------------------
// The `skill` cvar (host_cmd.c / sv_main.c `current_skill`).
//
// The original engine kept `skill` in the console-cvar registry and derived an
// integer `current_skill = (int)(skill.value + 0.5)` clamped to 0..3 in
// `SV_SpawnServer`. This headless port has no cvar subsystem and the `Vm` field
// set is fixed (we must not extend it), so — exactly like the changelevel /
// lightstyle transports — we hold the live skill value in a per-thread cell.
// `PF_cvar("skill")` reads it, `PF_cvar_set("skill", N)` writes it (clamped),
// and `ED_LoadFromFile` reads it to filter monsters/items by difficulty. The
// difficulty portals in the start map are `trigger_setskill` entities whose
// QuakeC `touch` calls `cvar_set("skill", N)`, so honouring `cvar_set` here is
// what makes those portals actually change which entities spawn.
//
// THREAD-LOCAL (not a process-global atomic): a server session runs all its
// QuakeC on one thread, so a `thread_local` is the correct scope AND keeps each
// test thread isolated (the cell is the same shape as the sound/lightstyle/
// changelevel queues above).
// ---------------------------------------------------------------------------

thread_local! {
    /// Live integer skill level (0=easy, 1=medium, 2=hard, 3=nightmare),
    /// defaulting to 1 (single-player medium, matching the stock `skill` "1").
    static SKILL: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}

/// The `current_skill` value: the live [`SKILL`] read back as an int. Used by
/// the spawn filter and by `cvar("skill")`.
fn skill_value() -> i32 {
    SKILL.with(|s| s.get())
}

/// Set the skill level, clamped to `0..=3` exactly as `SV_SpawnServer` does
/// (`current_skill = (int)(value + 0.5)`, then clamp). The input is the raw
/// float a `cvar_set("skill", N)` would pass; we round it the way the C does.
fn set_skill_value(v: f32) {
    // SV_SpawnServer: current_skill = (int)(skill.value + 0.5); clamp 0..3.
    let s = ((v + 0.5) as i32).clamp(0, 3);
    SKILL.with(|cell| cell.set(s));
}

/// Reset the skill to the default (1, medium). Called when a fresh server is
/// built so a prior level's `cvar_set("skill", …)` cannot leak into the next
/// (mirrors the per-thread reset of the changelevel / lightstyle transports).
fn reset_skill() {
    SKILL.with(|s| s.set(1));
}

/// `host_frametime` used for the two post-spawn settle frames in
/// `SV_SpawnServer` ("run two frames to allow everything to settle").
const SETTLE_FRAMETIME: f32 = 0.1;

/// `sv_gravity` default ("800"), from `sv_phys.c`.
const SV_GRAVITY: f32 = 800.0;
/// `sv_maxvelocity` default ("2000"), from `sv_phys.c` (`SV_CheckVelocity`).
const SV_MAXVELOCITY: f32 = 2000.0;

/// `DEFAULT_VIEWHEIGHT` (quakedef.h): the eye sits 22 units above the origin
/// when the QuakeC has not set an explicit `view_ofs`.
const DEFAULT_VIEWHEIGHT: f32 = 22.0;

/// `NUM_SPAWN_PARMS` (quakedef.h): how many `parm1..parm16` spawn parameters
/// `SetNewParms` fills and `PutClientInServer` later consumes.
pub const NUM_SPAWN_PARMS: usize = 16;

/// The QuakeC global name for spawn parm index `i` (`0..NUM_SPAWN_PARMS`):
/// `parm1`..`parm16`. The C `pr_global_struct->parm1..16` are the 16 floats
/// `SetChangeParms` writes and `DecodeLevelParms` reads back across a level
/// change.
fn parm_global_name(i: usize) -> String {
    format!("parm{}", i + 1)
}

// ---------------------------------------------------------------------------
// (A) The world model: crate::vm::Host backed by a parsed BSP.
// ---------------------------------------------------------------------------

/// The server's view of the loaded map: the parsed BSP plus the precache name
/// tables the QuakeC builtins populate. Implements [`Host`] so the engine
/// builtins can trace, query point contents, precache, and look up submodel
/// bounds.
///
/// Index 0 of each precache table is the empty string `""` (the C reserved slot
/// 0 of `sv.model_precache` / `sv.sound_precache` for `NULL`); real names start
/// at index 1.
pub struct WorldModel {
    bsp: Bsp,
    precache_models: Vec<String>,
    precache_sounds: Vec<String>,
    /// Optional pak, used to resolve the collision bounds of external brush
    /// models (the `maps/b_*.bsp` item boxes — explosive/ammo/health boxes).
    /// `None` (e.g. in unit tests) just falls back to the historical zero box.
    pak: Option<crate::pak::Pak>,
    /// Cache of external brush-model MODEL-0 bounds, keyed by precache name
    /// (e.g. `"maps/b_explob.bsp"` -> `(0,0,0)..(32,32,64)`). Populated lazily
    /// by [`precache_model`] so `setmodel` can give the box a real bbox.
    external_bounds: std::collections::HashMap<String, (Vec3, Vec3)>,
}

impl WorldModel {
    /// Build a world model for `bsp`, reserving precache slot 0 for the empty
    /// string and precaching the world model name `"*0"` at index 1 (the C
    /// `SV_SpawnServer` precached `sv.worldmodel` as model 1).
    pub fn new(bsp: Bsp) -> WorldModel {
        WorldModel::with_pak(bsp, None)
    }

    /// Like [`new`], but with a pak so external brush-model (`b_*.bsp`) bounds
    /// can be resolved for collision/damage. The interactive engines (wasm,
    /// quaketool) pass `Some(pak)`; tests pass `None` and keep the zero-box
    /// fallback.
    pub fn with_pak(bsp: Bsp, pak: Option<crate::pak::Pak>) -> WorldModel {
        let mut w = WorldModel {
            bsp,
            precache_models: vec![String::new()],
            precache_sounds: vec![String::new()],
            pak,
            external_bounds: std::collections::HashMap::new(),
        };
        // Slot 1 is the world brush model. id used the map name; "*0" is the
        // submodel-0 (worldspawn) reference and is what setmodel resolves.
        let _ = w.precache_model("*0");
        w
    }

    /// The borrowed BSP (read-only).
    pub fn bsp(&self) -> &Bsp {
        &self.bsp
    }

    /// The precached model names (index 0 is `""`).
    pub fn model_names(&self) -> &[String] {
        &self.precache_models
    }

    /// The precached sound names (index 0 is `""`).
    pub fn sound_names(&self) -> &[String] {
        &self.precache_sounds
    }

    /// Resolve a `"*N"` brush-submodel reference to its model index `N`.
    fn submodel_index(name: &str) -> Option<usize> {
        let digits = name.strip_prefix('*')?;
        digits.parse::<usize>().ok()
    }
}

/// Push `name` onto `table` if absent, returning its index; if present, return
/// the existing index. (The C `PF_precache_*` linear-scanned `sv.*_precache`,
/// stopping at the first empty slot to append or the first matching slot to
/// reuse.)
fn precache_push(table: &mut Vec<String>, name: &str) -> i32 {
    if let Some(i) = table.iter().position(|s| s == name) {
        return i as i32;
    }
    let i = table.len() as i32;
    table.push(name.to_string());
    i
}

impl Host for WorldModel {
    fn precache_model(&mut self, name: &str) -> i32 {
        // External brush models — the `maps/b_*.bsp` item boxes (explosive box,
        // ammo/health boxes) — carry their real collision bounds in their own
        // BSP's MODEL-0. The C `PF_precache_model` loads every precached model
        // via `Mod_ForName`, so `setmodel` later finds `mod->mins/maxs`; without
        // it the box gets a zero bbox and hitscans/movement pass straight
        // through (visible but unshootable). Resolve the bounds once from the
        // pak and cache them. Any failure (no pak / missing file / parse error /
        // no models) silently leaves them unset -> the historical zero box.
        if name.ends_with(".bsp")
            && Self::submodel_index(name).is_none()
            && !self.external_bounds.contains_key(name)
        {
            if let Some(pak) = &self.pak {
                if let Ok(Some(bytes)) = pak.read_file(name) {
                    if let Ok(bsp) = crate::bsp::Bsp::parse(&bytes) {
                        if let Some(m) = bsp.models.first() {
                            // `Mod_LoadSubmodels` spreads the raw bounds out by a
                            // pixel (mins-1, maxs+1); b_explob.bsp's raw
                            // (1,1,1)..(31,31,63) becomes (0,0,0)..(32,32,64).
                            let mins = [m.mins[0] - 1.0, m.mins[1] - 1.0, m.mins[2] - 1.0];
                            let maxs = [m.maxs[0] + 1.0, m.maxs[1] + 1.0, m.maxs[2] + 1.0];
                            self.external_bounds.insert(name.to_string(), (mins, maxs));
                        }
                    }
                }
            }
        }
        precache_push(&mut self.precache_models, name)
    }

    fn precache_sound(&mut self, name: &str) -> i32 {
        precache_push(&mut self.precache_sounds, name)
    }

    fn find_sound(&self, name: &str) -> Option<i32> {
        // The C's scan starts at slot 0, which holds "" exactly like ours
        // (`sv.sound_precache[0] = pr_strings`), so an empty name "matches"
        // slot 0 there too — kept as-is; QuakeC never passes one.
        self.precache_sounds.iter().position(|s| s == name).map(|i| i as i32)
    }

    fn model_bbox(&self, name: &str) -> Option<(Vec3, Vec3)> {
        // Brush submodels ("*N") read bounds straight from the world BSP.
        if let Some(n) = Self::submodel_index(name) {
            let m = self.bsp.models.get(n)?;
            return Some((m.mins, m.maxs));
        }
        // External brush models (the b_*.bsp item boxes) get the bounds we
        // resolved + cached at precache time. Real ".mdl" alias models are not
        // in the cache, so they still fall back to a zero box (faithful: the C
        // `setmodel` also set a zero box for non-brush models, which the QuakeC
        // then `setsize`s).
        self.external_bounds.get(name).copied()
    }

    fn trace(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> HostTrace {
        crate::world::trace_world(&self.bsp, start, end, mins, maxs)
    }

    fn point_contents(&self, p: Vec3) -> i32 {
        crate::world::point_contents(&self.bsp, p)
    }

    fn bsp(&self) -> &Bsp {
        &self.bsp
    }
}

// ---------------------------------------------------------------------------
// (B) Engine builtins. Each is an `fn(&mut Vm) -> Result<()>`.
//
// Field/global access is by name through the Vm helpers. World services are
// reached via `vm.with_host(...)`, which must NOT be held across `vm.execute`.
// ---------------------------------------------------------------------------

/// `PF_setorigin` (#2): `void(entity e, vector o) setorigin`. Sets the origin
/// and recomputes `absmin`/`absmax` from `origin + mins` / `origin + maxs`
/// (the part of `SV_LinkEdict` that matters without the area grid).
fn bi_setorigin(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let o = vm.arg_vector(1);
    vm.ent_set_vector(e, "origin", o);
    link_edict(vm, e);
    Ok(())
}

/// `PF_setsize` (#4): `void(entity e, vector min, vector max) setsize`. Mirrors
/// `SetMinMaxSize`: stores `mins`, `maxs`, and `size = max - min`. The C
/// `PR_RunError("backwards mins/maxs")` is downgraded to a clean early return so
/// a malformed map can't abort the load.
fn bi_setsize(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let min = vm.arg_vector(1);
    let max = vm.arg_vector(2);
    set_min_max_size(vm, e, min, max);
    Ok(())
}

/// `SetMinMaxSize` (pr_cmds.c): set `mins`/`maxs`/`size` then relink. Rotation
/// is disabled in the C (`rotate = false; // FIXME`), so we copy directly.
fn set_min_max_size(vm: &mut Vm, e: i32, min: Vec3, max: Vec3) {
    vm.ent_set_vector(e, "mins", min);
    vm.ent_set_vector(e, "maxs", max);
    vm.ent_set_vector(e, "size", v_sub(max, min));
    link_edict(vm, e);
}

/// `PF_setmodel` (#3): `void(entity e, string m) setmodel`. Sets `model`,
/// resolves `modelindex` via the host precache, and — for a `"*N"` brush
/// submodel — copies the BSP submodel bounds into `mins`/`maxs`/`size`.
fn bi_setmodel(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let m = vm.arg_string(1);

    // model field = the string_t of the argument (the C did `m - pr_strings`,
    // i.e. it kept the same string_t). Re-intern to be safe across heaps.
    vm.ent_set_string(e, "model", &m);

    // modelindex = host.precache_model(m); also fetch its bounds if it's a
    // brush submodel. Take the host only briefly (no execute() inside).
    let (idx, bbox) = vm
        .with_host(|_vm, h| {
            let idx = h.precache_model(&m);
            let bbox = h.model_bbox(&m);
            (idx, bbox)
        })
        .unwrap_or((0, None));

    vm.ent_set_float(e, "modelindex", idx as f32);

    // SetMinMaxSize(e, mod->mins, mod->maxs) for a brush model; the C used a
    // zero box when the model had no bounds (mod == NULL or a non-brush model).
    let (min, max) = bbox.unwrap_or(([0.0; 3], [0.0; 3]));
    set_min_max_size(vm, e, min, max);
    Ok(())
}

/// `PF_precache_sound` (#19): registers the sound and returns the argument's
/// `string_t` unchanged (QuakeC assigns it back to a field). The C
/// `ss_loading`-state and overflow `PR_RunError`s are not modelled.
fn bi_precache_sound(vm: &mut Vm) -> Result<()> {
    let s = vm.arg_string(0);
    vm.with_host(|_vm, h| h.precache_sound(&s));
    // Return the argument's string_t unchanged: G_INT(OFS_RETURN)=G_INT(OFS_PARM0).
    let s_t = vm.arg_int(0);
    vm.ret_string(s_t);
    Ok(())
}

/// `PF_precache_model` (#20): registers the model and returns the argument's
/// `string_t` unchanged.
fn bi_precache_model(vm: &mut Vm) -> Result<()> {
    let s = vm.arg_string(0);
    vm.with_host(|_vm, h| h.precache_model(&s));
    let s_t = vm.arg_int(0);
    vm.ret_string(s_t);
    Ok(())
}

/// `PF_precache_file` (#68/#77): a qcc-only copy hint — does nothing but return
/// its argument (`G_INT(OFS_RETURN) = G_INT(OFS_PARM0)`).
fn bi_precache_file(vm: &mut Vm) -> Result<()> {
    let s_t = vm.arg_int(0);
    vm.ret_string(s_t);
    Ok(())
}

/// `PF_makevectors` (#1): set the `v_forward`/`v_right`/`v_up` globals from the
/// argument angles. Identical to the pure builtin, restated here so installing
/// the engine table is self-contained.
fn bi_makevectors(vm: &mut Vm) -> Result<()> {
    let angles = vm.arg_vector(0);
    let (forward, right, up) = angle_vectors(angles);
    vm.gset_vector("v_forward", forward);
    vm.gset_vector("v_right", right);
    vm.gset_vector("v_up", up);
    Ok(())
}

/// `PF_traceline` (#16): `float(vector v1, vector v2, float nomonsters, entity
/// ignore) traceline`. Traces a *point* (`mins=maxs=0`) through the world AND
/// every solid entity via [`sv_move`], writes the `trace_*` globals, and sets
/// `trace_ent` to the edict that was hit (world = 0, nothing = the world too,
/// matching the C which left `trace.ent` as `sv.edicts` for a clear move only
/// implicitly — here a clear move leaves `trace_ent = 0`).
fn bi_traceline(vm: &mut Vm) -> Result<()> {
    let v1 = vm.arg_vector(0);
    let v2 = vm.arg_vector(1);
    // arg 2 = nomonsters (MOVE_NOMONSTERS): when nonzero, the trace must skip
    // every box entity (monsters/player) and clip only the world + SOLID_BSP
    // bmodels. QC visible() passes TRUE here so its sight line reaches the
    // player without stopping on the player's own SOLID_SLIDEBOX box.
    let nomonsters = vm.arg_float(2) != 0.0;
    let ignore = vm.arg_entity(3); // the "ignore" passedict.

    // Entity-aware move. sv_move borrows the host internally; this builtin must
    // not be inside with_host.
    let tr = sv_move(vm, v1, v2, [0.0; 3], [0.0; 3], ignore, nomonsters, false);

    vm.gset_float("trace_allsolid", tr.allsolid as i32 as f32);
    vm.gset_float("trace_startsolid", tr.startsolid as i32 as f32);
    vm.gset_float("trace_fraction", tr.fraction);
    vm.gset_float("trace_inwater", tr.inwater as i32 as f32);
    vm.gset_float("trace_inopen", tr.inopen as i32 as f32);
    vm.gset_vector("trace_endpos", tr.endpos);
    vm.gset_vector("trace_plane_normal", tr.plane_normal);
    vm.gset_float("trace_plane_dist", tr.plane_dist);
    // trace_ent = the hit edict; a clear move (ent == -1) resolves to the world.
    vm.gset_int("trace_ent", if tr.ent < 0 { 0 } else { tr.ent });
    Ok(())
}

/// `PF_pointcontents` (#41): `float(vector v) pointcontents`. Returns the
/// `CONTENTS_*` value at the point.
fn bi_pointcontents(vm: &mut Vm) -> Result<()> {
    let p = vm.arg_vector(0);
    let c = vm.with_host(|_vm, h| h.point_contents(p)).unwrap_or(-2); // CONTENTS_SOLID
    vm.ret_float(c as f32);
    Ok(())
}

/// `PF_droptofloor` (#34): `float() droptofloor`. Box-traces `self` straight
/// down 256 units (entity-aware `SV_Move`); on a clean landing snaps `origin` to
/// the floor, sets `FL_ONGROUND` and `groundentity` to whatever edict it landed
/// on (the world or a solid bmodel such as a platform), and returns 1; otherwise
/// returns 0.
fn bi_droptofloor(vm: &mut Vm) -> Result<()> {
    let ent = vm.gget_int("self");

    let origin = vm.ent_get_vector(ent, "origin");
    let mins = vm.ent_get_vector(ent, "mins");
    let maxs = vm.ent_get_vector(ent, "maxs");
    let end: Vec3 = [origin[0], origin[1], origin[2] - 256.0];

    // PF_droptofloor (pr_cmds.c) uses the ENTITY-AWARE SV_Move (not the
    // world-only host trace), so the entity can come to rest on a door/plat or
    // another solid edict, and sets groundentity to whatever it landed on.
    let tr = sv_move(vm, origin, end, mins, maxs, ent, false, false);

    if tr.fraction == 1.0 || tr.allsolid {
        vm.ret_float(0.0);
    } else {
        vm.ent_set_vector(ent, "origin", tr.endpos);
        link_edict(vm, ent);
        let flags = vm.ent_get_float(ent, "flags") as i32;
        vm.ent_set_float(ent, "flags", (flags | FL_ONGROUND) as f32);
        // groundentity = EDICT_TO_PROG(trace.ent): the resolved edict it rests
        // on (0 = world, >0 = that edict). The hit branch only runs when
        // fraction < 1, so trace.ent is never the "nothing hit" sentinel (-1).
        vm.ent_set_int(ent, "groundentity", tr.ent.max(0));
        vm.ret_float(1.0);
    }
    Ok(())
}

/// `PF_cvar` (#45): `float(string name) cvar`. Returns the known server-cvar
/// defaults; everything else is 0 (the C looked these up in the cvar registry).
fn bi_cvar(vm: &mut Vm) -> Result<()> {
    let name = vm.arg_string(0);
    let v = cvar_value(&name);
    vm.ret_float(v);
    Ok(())
}

/// The handful of cvar defaults the spawn/think code reads. Values match the
/// stock `*.c` declarations (`sv_gravity` "800", `deathmatch` "0"). `skill` is
/// the *live* value (see [`SKILL`]): `cvar_set("skill", N)` from a difficulty
/// portal updates it and `cvar("skill")` reads it back, so the QuakeC sees the
/// difficulty it selected (the old stub returned a constant 1.0 unconditionally).
fn cvar_value(name: &str) -> f32 {
    match name {
        "sv_gravity" => SV_GRAVITY,
        "sv_maxvelocity" => SV_MAXVELOCITY,
        "deathmatch" | "coop" | "teamplay" => 0.0,
        "skill" => skill_value() as f32,
        _ => 0.0,
    }
}

/// `PF_cvar_set` (#72): `void(string var, string val) cvar_set`. The C calls
/// `Cvar_Set(var, val)`. This headless port has no cvar registry, so the only
/// cvar with a live backing store is `skill` (see [`SKILL`]); setting it is what
/// makes the start-map difficulty portals (`trigger_setskill` -> `cvar_set
/// ("skill", N)`) actually change which monsters/items spawn. Any other cvar
/// name is a benign no-op (the value is parsed but has nowhere to land), exactly
/// as the old `bi_noop` behaved — but `skill` now persists.
fn bi_cvar_set(vm: &mut Vm) -> Result<()> {
    let name = vm.arg_string(0);
    if name == "skill" {
        let val = parse_float(&vm.arg_string(1));
        set_skill_value(val);
    }
    Ok(())
}

/// `PF_changeyaw` (#49): turn `self.angles[1]` toward `ideal_yaw` by at most
/// `yaw_speed`. A faithful port of the C (which converted this from QuakeC for
/// speed); harmless for non-monster entities (`yaw_speed == 0` => no turn).
fn bi_changeyaw(vm: &mut Vm) -> Result<()> {
    let ent = vm.gget_int("self");
    let angles = vm.ent_get_vector(ent, "angles");
    let current = crate::math::anglemod(angles[1]);
    let ideal = vm.ent_get_float(ent, "ideal_yaw");
    let speed = vm.ent_get_float(ent, "yaw_speed");

    if current == ideal {
        return Ok(());
    }
    let mut move_ = ideal - current;
    if ideal > current {
        if move_ >= 180.0 {
            move_ -= 360.0;
        }
    } else if move_ <= -180.0 {
        move_ += 360.0;
    }
    if move_ > 0.0 {
        if move_ > speed {
            move_ = speed;
        }
    } else if move_ < -speed {
        move_ = -speed;
    }

    let new_yaw = crate::math::anglemod(current + move_);
    let new_angles = [angles[0], new_yaw, angles[2]];
    vm.ent_set_vector(ent, "angles", new_angles);
    Ok(())
}

/// A benign no-op builtin: consumes its arguments and returns nothing. Used for
/// the remaining network / client-routing builtins that have no world effect in
/// this headless server (`stuffcmd`, `makestatic`, `lightstyle`, `changelevel`,
/// `setspawnparms`, the print routers, `cvar_set`). (`sound` queues a
/// [`SoundEvent`] via [`bi_sound`]; `ambientsound` records a [`StaticSound`]
/// via [`bi_ambientsound`]; `particle` queues a [`ParticleBurst`] via
/// [`bi_particle`]; the `Write*` family (#52..#59) drives the temp-entity
/// decoder via [`te_feed`].)
fn bi_noop(_vm: &mut Vm) -> Result<()> {
    Ok(())
}

/// `PF_aim` (#44): `vector(entity e, float speed) aim`. Quake's auto-aim, ON by
/// default in single-player (`sv_aim` 0.93). Faithful port of pr_cmds.c PF_aim:
/// first trace straight along `v_forward`; if that doesn't immediately hit a
/// DAMAGE_AIM target, scan every damageable entity and snap toward the best one
/// whose direction is within the `sv_aim` cone AND that a clear `SV_Move` can
/// actually reach — preserving the vertical component so off-pitch targets
/// (above/below) get hit. This is what lets keyboard / imperfect-pitch aiming
/// connect; the old stub returned `v_forward` unconditionally (no assist).
/// (The teamplay exclusions are dropped — this is a single-player server.)
fn bi_aim(vm: &mut Vm) -> Result<()> {
    // takedamage flags (defs.qc): DAMAGE_NO 0, DAMAGE_YES 1, DAMAGE_AIM 2.
    const DAMAGE_AIM: f32 = 2.0;
    // `sv_aim` cvar default ("0.93"): the minimum forward-dot to assist toward.
    const SV_AIM: f32 = 0.93;

    let ent = vm.arg_entity(0);
    // arg 1 (speed) is read but unused by PF_aim — the QC applies it to the shot.

    let v_forward = vm.gget_vector("v_forward");
    let origin = vm.ent_get_vector(ent, "origin");
    let mut start = origin;
    start[2] += 20.0;

    // Try a straight trace first; a direct DAMAGE_AIM hit needs no assist.
    let end = [
        start[0] + 2048.0 * v_forward[0],
        start[1] + 2048.0 * v_forward[1],
        start[2] + 2048.0 * v_forward[2],
    ];
    let tr = sv_move(vm, start, end, [0.0; 3], [0.0; 3], ent, false, false);
    if tr.ent > 0 && vm.ent_get_float(tr.ent, "takedamage") == DAMAGE_AIM {
        vm.ret_vector(v_forward);
        return Ok(());
    }

    // Otherwise scan all damageable entities for the best in-cone, reachable one.
    let bestdir = v_forward;
    let mut bestdist = SV_AIM;
    let mut bestent: i32 = -1;

    let n = vm.num_edicts() as i32;
    for check in 1..n {
        if check == ent {
            continue;
        }
        if vm.ent_get_float(check, "takedamage") != DAMAGE_AIM {
            continue;
        }
        let c_org = vm.ent_get_vector(check, "origin");
        let c_min = vm.ent_get_vector(check, "mins");
        let c_max = vm.ent_get_vector(check, "maxs");
        // Aim at the centre of the target's bounding box.
        let target = [
            c_org[0] + 0.5 * (c_min[0] + c_max[0]),
            c_org[1] + 0.5 * (c_min[1] + c_max[1]),
            c_org[2] + 0.5 * (c_min[2] + c_max[2]),
        ];
        let dir = [target[0] - start[0], target[1] - start[1], target[2] - start[2]];
        let (dirn, _) = crate::math::normalize(dir);
        let dist = dirn[0] * v_forward[0] + dirn[1] * v_forward[1] + dirn[2] * v_forward[2];
        if dist < bestdist {
            continue; // outside the cone — too far to turn
        }
        let tr = sv_move(vm, start, target, [0.0; 3], [0.0; 3], ent, false, false);
        if tr.ent == check {
            // Clear line to this target — it's the new best.
            bestdist = dist;
            bestent = check;
        }
    }

    if bestent >= 0 {
        let b_org = vm.ent_get_vector(bestent, "origin");
        let dir = [b_org[0] - origin[0], b_org[1] - origin[1], b_org[2] - origin[2]];
        let dist = dir[0] * v_forward[0] + dir[1] * v_forward[1] + dir[2] * v_forward[2];
        // Snap horizontally to v_forward*dist but keep the true vertical (dir.z).
        let endv = [v_forward[0] * dist, v_forward[1] * dist, dir[2]];
        let (endn, _) = crate::math::normalize(endv);
        vm.ret_vector(endn);
    } else {
        vm.ret_vector(bestdir);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Deferred level change (PF_changelevel).
//
// The C `PF_changelevel` (pr_cmds.c, non-`QUAKE2` build) does NOT swap the map
// inline — the VM is mid-execution and the entity/global memory the builtin
// would tear down is exactly what the rest of the calling frame is still using.
// It guards against a double issue (`svs.changelevel_issued`) and merely defers:
// `Cbuf_AddText("changelevel <map>")`, which `Host_Frame` processes AFTER the
// current frame finishes. We mirror this precisely: the builtin only *records*
// the requested map name in a thread-local; the front-end takes it after
// `client_frame` returns (via [`Server::take_pending_changelevel`]) and performs
// the swap itself, never inside the builtin call.
//
// The `thread_local!` choice is identical to the sound/particle/temp-entity
// queues above: builtins are `fn(&mut Vm)` and cannot see the `Server`, and
// `vm.rs` is off-limits, so the deferred request cannot hang off either. Server
// methods run on the same thread as the builtins, so a request a frame's QuakeC
// fired is visible to `take_pending_changelevel` right after the frame.
// ---------------------------------------------------------------------------

thread_local! {
    /// The map name requested by a deferred `changelevel()` this frame, or `None`.
    /// First-writer-wins within a frame, mirroring the C `svs.changelevel_issued`
    /// guard that drops a second `PF_changelevel` until the swap completes. Taken
    /// (and cleared) by [`Server::take_pending_changelevel`]; reset in
    /// [`Server::new`] so a stale request can never leak across servers.
    static CHANGELEVEL_REQUEST: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Record a deferred level change to `map` (first-writer-wins this frame).
fn push_changelevel(map: String) {
    CHANGELEVEL_REQUEST.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(map);
        }
    });
}

/// Take and clear the deferred level-change request, if any.
fn take_changelevel() -> Option<String> {
    CHANGELEVEL_REQUEST.with(|c| c.borrow_mut().take())
}

/// Clear any pending level-change request (called from [`Server::new`] so a
/// stale request from a prior server cannot leak into a fresh one).
fn reset_changelevel() {
    CHANGELEVEL_REQUEST.with(|c| *c.borrow_mut() = None);
}

/// `PF_changelevel` (#70): `void(string s) changelevel`. The C looked up its
/// string argument, guarded against a double issue, and deferred the actual swap
/// via `Cbuf_AddText("changelevel <s>")`. We faithfully *only* record the map
/// name here (PARM0, the `string_t` of the destination map, e.g. `"e1m2"`); the
/// front-end performs the swap after the frame. Never swaps inline.
fn bi_changelevel(vm: &mut Vm) -> Result<()> {
    let map = vm.arg_string(0);
    push_changelevel(map);
    Ok(())
}

thread_local! {
    /// Set when QuakeC issues `localcmd("restart\n")` — the single-player respawn
    /// path (a dead player who presses a button runs `client.qc`'s
    /// `localcmd("restart\n")` to reload the current level with fresh entry parms).
    /// Drained by [`Server::take_pending_restart`]; reset in [`Server::new`].
    static RESTART_REQUEST: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
}

fn reset_restart() {
    RESTART_REQUEST.with(|c| *c.borrow_mut() = false);
}

/// `PF_localcmd` (#46): `void(string s) localcmd` — `Cbuf_AddText(s)`, i.e. QuakeC
/// pushing a console command. Most are host/diagnostic and irrelevant to this port,
/// but single-player gameplay issues a few level-control commands we MUST honour:
///   * `restart` — reload the current level (the death-respawn path, `client.qc`).
///   * `changelevel <map>` / `map <map>` — defer a level swap (same as PF_changelevel).
///
/// Everything else is a benign no-op (matching the old behaviour). The token parse
/// is whitespace-split and case-insensitive on the command word.
fn bi_localcmd(vm: &mut Vm) -> Result<()> {
    let cmd = vm.arg_string(0);
    let mut it = cmd.split_whitespace();
    match it.next().map(|w| w.to_ascii_lowercase()).as_deref() {
        Some("restart") => {
            RESTART_REQUEST.with(|c| *c.borrow_mut() = true);
        }
        Some("changelevel") | Some("map") => {
            if let Some(map) = it.next() {
                push_changelevel(map.to_string());
            }
        }
        _ => {} // other console text: benign no-op, as before.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-thread transport capture/restore for the savegame loader.
//
// `Server::load_savegame` builds a THROWAWAY server (`with_pak` +
// `spawn_entities`) before the `.sav` blocks have proven parseable, and that
// build resets/repopulates the per-thread transports (the lightstyle table,
// the skill cell) and queues spawn-time events (sounds, particles, svc
// commands). On success the new server owns all of it; on FAILURE the caller
// keeps its old `Server` — whose next `run_frame` re-syncs `lightstyles` from
// the shared transport and whose `skill()` reads the shared cell — so a
// rejected save would otherwise leak its lightstyles/skill into the running
// game it was supposed to leave intact (save.rs's documented deviation from
// the C's Sys_Error). The loader captures the persistent transports up front
// and, on any error, restores them and discards the transient queues (the
// same drop-the-spawn's-one-shots treatment every SUCCESSFUL build applies).
// ---------------------------------------------------------------------------

/// The persistent per-thread transports a savegame load clobbers, captured by
/// [`crate::save`]'s loader before it spawns the throwaway server and handed
/// back through [`restore_transports`] when the load fails.
pub(crate) struct TransportSnapshot {
    lightstyles: [String; MAX_LIGHTSTYLES],
    skill: i32,
}

/// Capture the caller's per-thread transport state (see [`TransportSnapshot`]).
pub(crate) fn capture_transports() -> TransportSnapshot {
    TransportSnapshot {
        lightstyles: snapshot_lightstyles(),
        skill: skill_value(),
    }
}

/// Put the captured persistent transports back and discard everything the
/// failed build queued, so the still-running game's next frame sees exactly
/// the state it left behind. The transient queues are cleared rather than
/// captured: the caller drains them at the end of every frame (and a load
/// runs between frames), so "empty" IS the caller's state — replaying the
/// failed spawn's one-shot sounds/particles/svc commands into the surviving
/// game would be its own leak.
pub(crate) fn restore_transports(snap: TransportSnapshot) {
    LIGHTSTYLES.with(|t| *t.borrow_mut() = snap.lightstyles);
    SKILL.with(|s| s.set(snap.skill));
    reset_changelevel();
    reset_restart();
    reset_svc_recognizer();
    reset_temp_entity_decoder();
    let _ = take_sound_events();
    let _ = take_static_sounds();
    let _ = take_particle_bursts();
    let _ = take_messages();
    let _ = take_temp_entities();
    let _ = take_svc_events();
}

/// `EF_MUZZLEFLASH` (`quakedef.h`): the firing entity emits a brief, bright
/// forward-offset light (`CL_RelinkEntities`).
pub const EF_MUZZLEFLASH: i32 = 2;
/// `EF_BRIGHTLIGHT`: a large light at the entity (+16 z).
pub const EF_BRIGHTLIGHT: i32 = 4;
/// `EF_DIMLIGHT`: a medium light at the entity origin (e.g. the player while
/// quad-damage or with the lightning gun warming).
pub const EF_DIMLIGHT: i32 = 8;

/// One entity dynamic-light contribution for a frame, as enumerated by
/// [`Server::entity_dlights`] (the `EF_*` dlight spawns of `CL_RelinkEntities`).
///
/// A front-end turns each into a [`crate::dlight::DynamicLights::alloc`] call:
/// `alloc(key, origin, radius_base + (rng & 31), now + life, decay=0, minlight,
/// now)`. The `radius_base` excludes the `rand()&31` jitter so this struct stays
/// deterministic; the caller adds the jitter with its own RNG. `decay` is 0 for
/// these lights — they simply expire at `die` (Quake set no decay for the `EF_*`
/// lights; only explosions decay).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityDlight {
    /// Owning entity number; used as the `CL_AllocDlight` reuse key so the light
    /// tracks the entity instead of filling the pool.
    pub key: i32,
    /// World-space light position (already offset for the muzzle / bright cases).
    pub origin: [f32; 3],
    /// Radius in light units *before* the `rand()&31` jitter the caller adds.
    pub radius_base: f32,
    /// Ambient floor (32 for the muzzle flash, 0 otherwise).
    pub minlight: f32,
    /// Seconds until the light dies (`die = now + life`).
    pub life: f32,
}

/// Install the engine builtins over the pure-builtin table from
/// [`crate::builtins::default_builtins`], keeping the self-contained ones
/// (`ftos`/`vtos`/`vlen`/`normalize`/`rint`/`floor`/`ceil`/`fabs`/`random`/
/// `spawn`/`remove`/`find`/`nextent`/`error`/`objerror`/`print`/`dprint`/
/// `vectoyaw`/`vectoangles`) intact.
///
/// Numbers are the `pr_builtin[]` indices from `pr_cmds.c` (non-`QUAKE2` build).
pub fn install_engine_builtins(vm: &mut Vm) {
    // A small helper that only writes if the index is in range, so a short
    // table can never panic here.
    fn put(table: &mut [Builtin], n: usize, f: Builtin) {
        if let Some(slot) = table.get_mut(n) {
            *slot = f;
        }
    }
    let t = &mut vm.builtins;

    put(t, 1, bi_makevectors); // makevectors
    put(t, 2, bi_setorigin); // setorigin
    put(t, 3, bi_setmodel); // setmodel
    put(t, 4, bi_setsize); // setsize
    put(t, 8, bi_sound); // sound (queues a SoundEvent)
    put(t, 16, bi_traceline); // traceline
    put(t, 17, bi_checkclient); // checkclient (line-of-sight to the player)
    put(t, 19, bi_precache_sound); // precache_sound
    put(t, 20, bi_precache_model); // precache_model
    put(t, 21, bi_noop); // stuffcmd
    put(t, 22, bi_findradius); // findradius (chain of edicts within rad)
    put(t, 23, bi_bprint); // bprint -> on-screen notify line
    put(t, 24, bi_sprint); // sprint -> on-screen notify line
    put(t, 73, bi_centerprint); // centerprint -> centered transient message
    put(t, 32, bi_walkmove); // walkmove (SV_movestep)
    put(t, 34, bi_droptofloor); // droptofloor
    put(t, 35, bi_lightstyle); // lightstyle (stores sv.lightstyles[style])
    put(t, 40, bi_checkbottom); // checkbottom (SV_CheckBottom)
    put(t, 41, bi_pointcontents); // pointcontents
    put(t, 44, bi_aim); // aim
    put(t, 45, bi_cvar); // cvar
    put(t, 48, bi_particle); // particle (queues a ParticleBurst)
    put(t, 49, bi_changeyaw); // changeyaw

    // #52..#59: the network Write* family. These drive the temp-entity decoder
    // (broadcast Write* bursts -> TempEntityEvents); see te_feed.
    put(t, 52, bi_writebyte); // WriteByte
    put(t, 53, bi_writechar); // WriteChar
    put(t, 54, bi_writeshort); // WriteShort
    put(t, 55, bi_writelong); // WriteLong
    put(t, 56, bi_writecoord); // WriteCoord
    put(t, 57, bi_writeangle); // WriteAngle
    put(t, 58, bi_writestring); // WriteString
    put(t, 59, bi_writeentity); // WriteEntity

    put(t, 46, bi_localcmd); // localcmd (honours restart / changelevel / map; else no-op)
    put(t, 67, bi_movetogoal); // movetogoal (SV_MoveToGoal)
    put(t, 68, bi_precache_file); // precache_file
    put(t, 69, bi_noop); // makestatic
    put(t, 70, bi_changelevel); // changelevel (records the deferred map swap)
    put(t, 72, bi_cvar_set); // cvar_set (honours "skill"; else benign no-op)
    put(t, 74, bi_ambientsound); // ambientsound (records a StaticSound loop)
    put(t, 75, bi_precache_model); // precache_model (alias)
    put(t, 76, bi_precache_sound); // precache_sound (alias)
    put(t, 77, bi_precache_file); // precache_file (alias)
    put(t, 78, bi_noop); // setspawnparms
}

// ---------------------------------------------------------------------------
// (C) The Server.
// ---------------------------------------------------------------------------

/// The headless Quake server: a QuakeC VM with the engine builtins installed and
/// a [`WorldModel`] host. Drives entity spawning and a minimal physics frame.
pub struct Server {
    pub vm: Vm,
    /// The map's entity description text (`bsp.entities`), captured before the
    /// BSP is moved into the host. `spawn_entities` tokenizes this. (The
    /// [`Host`] trait has no entity-text accessor and we never `unsafe`-downcast,
    /// so the server keeps its own copy.)
    entities: String,
    /// The local client's edict index, or `-1` if no client has connected.
    ///
    /// SIMPLIFICATION (documented): canonical Quake reserves edict 1 for the
    /// first client in `SV_SpawnServer` (`sv.num_edicts = maxclients+1`) and
    /// keys `SV_Physics_Client` off the slot index `i <= svs.maxclients`. Here we
    /// instead reserve the *first free edict after `spawn_entities`* and remember
    /// it in this field (our stand-in for the single-element `svs.clients` table).
    /// Single-player QuakeC keys off `self`, not a hardcoded edict number, so the
    /// game logic is unaffected. Single client only; no netcode.
    /// (`pub(crate)`: the savegame loader re-identifies the player edict.)
    pub(crate) player: i32,
    /// The map's animated light-style patterns (`sv.lightstyles[64]`), owned by
    /// the server. The `lightstyle()` builtin writes a thread-local transport;
    /// the server syncs that into this field after each QuakeC execution window
    /// (`spawn_entities` / `run_frame`). `lightstyle_scales` reads it to produce
    /// the per-style brightness scales the renderer applies each frame. Cleared in
    /// [`Server::new`] so a changelevel re-populates it from the new worldspawn.
    /// (`pub(crate)`: `Host_Loadgame_f` overwrites all 64 from the savegame.)
    pub(crate) lightstyles: [String; MAX_LIGHTSTYLES],
    /// `sv.name` (server.h): the bare map name (`"e1m1"`), recorded by
    /// [`Server::set_map_name`]. `Host_Savegame_f` writes it into the `.sav`
    /// header so a load knows which map to spawn.
    pub(crate) map_name: String,
    /// `svs.clients[0].spawn_parms` (server.h `client_t`): the 16 spawn
    /// parameters captured when the local client connected — the level-ENTRY
    /// inventory, NOT the live one. `SV_ConnectClient` copies them out of the
    /// parm globals right after `SetNewParms` (fresh game) or restores the
    /// carried/saved set; `Host_Savegame_f` writes exactly these into the
    /// `.sav` header, and `Host_Loadgame_f` restores them from it.
    pub(crate) client_spawn_parms: [f32; NUM_SPAWN_PARMS],
}

/// The result of [`Server::spawn_entities`].
#[derive(Debug, Clone, Default)]
pub struct SpawnReport {
    /// Total `{ ... }` entity blocks parsed.
    pub total: usize,
    /// Entities whose spawn function ran without error.
    pub spawned: usize,
    /// Entities skipped by skill/deathmatch filtering.
    pub inhibited: usize,
    /// Entities with a classname but no matching spawn function.
    pub no_spawn_function: usize,
    /// Entities whose spawn function returned an error (caught, not fatal).
    pub spawn_errors: usize,
    /// `(classname, count)` pairs, sorted by count descending then name.
    pub classnames: Vec<(String, usize)>,
}

/// The result of [`Server::run_frame`].
#[derive(Debug, Clone, Copy)]
pub struct FrameReport {
    /// Number of think functions that fired this frame.
    pub thinks_fired: usize,
    /// Thinks that faulted (e.g. hit an unimplemented engine builtin). The
    /// offending entity is isolated and the frame continues, mirroring the
    /// per-entity robustness of the spawn loop.
    pub think_errors: usize,
    /// The `time` global after the frame.
    pub time: f32,
}

/// One frame of player input — the engine's `usercmd_t` (`forwardmove`,
/// `sidemove`, `upmove`, `buttons`, `impulse`) plus the look angles the engine
/// derives from the client's mouse/keyboard and copies into `v_angle`.
///
/// Units match id's: the move axes are intended speeds in pixels/sec; `yaw` and
/// `pitch` are absolute view angles in degrees (pitch positive = looking down,
/// as the network protocol delivers them).
#[derive(Debug, Clone, Copy, Default)]
pub struct UserCmd {
    pub forwardmove: f32,
    pub sidemove: f32,
    pub upmove: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub buttons: i32,
    pub impulse: i32,
}

impl Server {
    /// Build a server from a parsed map and program: create the VM, install the
    /// engine builtins, attach the [`WorldModel`] host, and initialise the
    /// well-known globals (`time = 1.0`).
    pub fn new(bsp: Bsp, progs: Progs) -> Result<Server> {
        Server::with_pak(bsp, progs, None)
    }

    /// Like [`new`], but threads a `pak` through to the [`WorldModel`] host so
    /// external brush-model item boxes (`maps/b_*.bsp`) collide and take damage.
    /// The interactive engines (wasm shell, quaketool) pass `Some(pak)`; the
    /// test suite uses [`new`] (`None`) and keeps the prior zero-box behaviour.
    pub fn with_pak(bsp: Bsp, progs: Progs, pak: Option<crate::pak::Pak>) -> Result<Server> {
        // Capture the entity text before the BSP moves into the host.
        let entities = bsp.entities.clone();

        let mut vm = Vm::new(progs);
        install_engine_builtins(&mut vm);
        vm.set_host(Box::new(WorldModel::with_pak(bsp, pak)));

        // A deferred changelevel() request is per-thread and outlives a server;
        // clear it so a request issued against a prior level can never leak into
        // this fresh one (mirrors `svs.changelevel_issued = false` in
        // SV_SpawnServer).
        reset_changelevel();
        // Likewise a pending localcmd("restart") respawn must not survive into a
        // freshly spawned server.
        reset_restart();
        // And a half-recognised / queued MSG_ALL command (an intermission fired on
        // the OLD level must never start one on this fresh server).
        reset_svc_recognizer();
        let _ = take_svc_events();
        // The light-style transport is also per-thread and outlives a server;
        // clear it so a prior level's patterns cannot leak before this level's
        // worldspawn calls `lightstyle()` (mirrors `SV_SpawnServer` memset of
        // sv.lightstyles).
        reset_lightstyles();
        // The `skill` cvar is process-global (we have no cvar registry); reset it
        // to the single-player default (1, medium) for each fresh server so the
        // spawn filter is deterministic and a prior level's `cvar_set("skill", …)`
        // cannot leak in unexpectedly. A front-end that persists the player's
        // chosen difficulty across a changelevel re-applies it with
        // [`Server::set_skill`] after construction (the same way it carries
        // `serverflags`).
        reset_skill();

        // Init globals available in this program. The C `SV_SpawnServer` set
        // sv.time = 1.0 before loading entities.
        vm.gset_float("time", 1.0);
        // mapname / world entity defaults are best-effort: only set if present.
        vm.gset_int("world", 0);
        vm.gset_int("self", 0);
        vm.gset_int("other", 0);

        Ok(Server {
            vm,
            entities,
            player: -1,
            lightstyles: std::array::from_fn(|_| String::new()),
            map_name: String::new(),
            client_spawn_parms: [0.0; NUM_SPAWN_PARMS],
        })
    }

    /// The current `time` global.
    pub fn time(&self) -> f32 {
        self.vm.gget_float("time")
    }

    /// `SV_SpawnServer` (sv_main.c): the parts of the world-edict/globals setup
    /// that need the MAP NAME, which the constructor never sees — the world
    /// edict's `model` field (`"maps/<name>.bsp"`; the QuakeC episode-end check
    /// `world.model == "maps/e1m7.bsp"` in `ExitIntermission` reads it) and the
    /// `mapname` global (the bare name; `samelevel`/`noexit`/`NextLevel` read
    /// it). Call after construction and BEFORE [`Server::spawn_entities`],
    /// exactly where the C set them (worldspawn's entity-lump keys never include
    /// `model`, so the value survives the parse). `name` may be bare (`"e1m7"`)
    /// or a pak path (`"maps/e1m7.bsp"`); both derive the same pair.
    ///
    /// DEVIATION: the C also set the world edict's `modelindex`/`solid`/
    /// `movetype` here; this port's collision and physics special-case edict 0
    /// everywhere instead, and the stock QuakeC never reads those world fields,
    /// so they stay unset to keep the world edict out of the mover paths.
    pub fn set_map_name(&mut self, name: &str) {
        let bare = name.trim_start_matches("maps/").trim_end_matches(".bsp").to_string();
        let full = format!("maps/{bare}.bsp");
        self.vm.ent_set_string(0, "model", &full);
        let s = self.vm.intern(&bare);
        self.vm.gset_int("mapname", s);
        // sv.name (strcpy(sv.name, server) in SV_SpawnServer): kept for the
        // savegame header's mapname line (Host_Savegame_f writes sv.name).
        self.map_name = bare;
    }

    /// The number of live (not-free) edicts, including the world (edict 0).
    pub fn live_entities(&self) -> usize {
        self.vm
            .edict_free
            .iter()
            .filter(|&&free| !free)
            .count()
    }

    /// One server frame (a stripped `SV_Physics`): advance `time` by `dt`, set
    /// `frametime`, then for each non-free edict run `SV_RunThink` and apply the
    /// minimal per-movetype physics. Returns how many thinks fired.
    ///
    /// The host is PRESENT throughout the loop (think functions reach it via
    /// `with_host`); only the brief `PushEntity` trace borrows it out.
    pub fn run_frame(&mut self, dt: f32) -> Result<FrameReport> {
        // host_frametime = dt; sv.time advances at the END in the C, but the
        // think-time test compares against sv.time + host_frametime, so we set
        // frametime now and bump time after the loop.
        self.vm.gset_float("frametime", dt);
        // Drop any half-collected temp-entity message from a prior (possibly
        // faulted) frame so this frame's Write* bursts parse cleanly.
        reset_temp_entity_decoder();
        reset_svc_recognizer();
        // SV_CleanupEnts: clear last frame's one-frame EF_MUZZLEFLASH before thinks.
        self.cleanup_ents();
        let start_time = self.time();
        // Record sv.time for the monster-locomotion relink touches (SV_TouchLinks
        // uses sv.time, not the clamped per-think `time` global).
        self.vm.sv_time = start_time;

        let mut thinks_fired = 0usize;
        let mut think_errors = 0usize;
        let n = self.vm.num_edicts();

        for e in 0..n {
            // edict 0 is the world; process every non-free edict, as the C does.
            let free = self.vm.edict_free.get(e).copied().unwrap_or(true);
            if free {
                continue;
            }
            let ent = e as i32;
            let movetype = self.vm.ent_get_float(ent, "movetype") as i32;

            // Isolate per-entity faults (e.g. a think hitting an unimplemented
            // builtin): count it, reset the interpreter, and carry on — one bad
            // entity must not abort the whole frame.
            match self.process_entity(ent, movetype, start_time, dt) {
                Ok(fired) => thinks_fired += fired as usize,
                Err(_) => {
                    think_errors += 1;
                    self.vm.reset_execution();
                }
            }
        }

        // sv.time += host_frametime (end of SV_Physics).
        self.vm.gset_float("time", start_time + dt);

        // A think may have called lightstyle() (e.g. a trigger toggling a light);
        // sync any updates from the write transport into the owned table.
        self.lightstyles = snapshot_lightstyles();

        Ok(FrameReport {
            thinks_fired,
            think_errors,
            time: self.time(),
        })
    }

    /// Process one live edict for a frame: per-movetype physics plus
    /// `SV_RunThink`. Returns whether a think fired. `run_think` returns
    /// `(fired, alive)`; physics runs whenever the entity is still alive,
    /// independent of whether a think fired. Errors propagate so the caller can
    /// isolate a faulting entity.
    fn process_entity(&mut self, ent: i32, movetype: i32, start_time: f32, dt: f32) -> Result<bool> {
        match movetype {
            MOVETYPE_PUSH => self.physics_pusher(ent, start_time, dt),
            MOVETYPE_NONE => {
                let (fired, _alive) = self.run_think(ent, start_time, dt)?;
                Ok(fired)
            }
            MOVETYPE_NOCLIP => {
                let (fired, alive) = self.run_think(ent, start_time, dt)?;
                if alive {
                    self.integrate_noclip(ent, dt);
                }
                Ok(fired)
            }
            MOVETYPE_STEP => {
                // SV_Physics_Step: freefall (+ landing thud) if not on ground /
                // fly / swim, then SV_RunThink, then SV_CheckWaterTransition —
                // the C runs the water-transition check AFTER the think and
                // unconditionally (outside the freefall branch), so a step entity
                // resting on the floor still maintains watertype/waterlevel and
                // splashes when pushed into liquid.
                self.physics_step(ent, start_time, dt);
                let (fired, _alive) = self.run_think(ent, start_time, dt)?;
                if !self.vm.edict_free.get(ent as usize).copied().unwrap_or(true) {
                    self.check_water_transition(ent);
                }
                Ok(fired)
            }
            MOVETYPE_TOSS | MOVETYPE_BOUNCE | MOVETYPE_FLY | MOVETYPE_FLYMISSILE => {
                // SV_Physics_Toss: think first; if alive, gravity + clipped move.
                let (fired, alive) = self.run_think(ent, start_time, dt)?;
                if alive {
                    self.physics_toss(ent, movetype, start_time, dt);
                }
                Ok(fired)
            }
            _ => {
                // MOVETYPE_WALK and any others: think only (no client AI).
                let _ = MOVETYPE_WALK;
                let (fired, _alive) = self.run_think(ent, start_time, dt)?;
                Ok(fired)
            }
        }
    }

    /// `SV_RunThink` (sv_phys.c): if the edict's `nextthink` is in `(0, time+dt]`,
    /// clear it, set the `time`/`self`/`other` globals, and execute its `think`.
    /// Returns `(fired, alive)`: `fired` = a think executed this frame; `alive`
    /// is `SV_RunThink`'s own bool (the edict was not removed). When no think is
    /// due it returns `(false, true)` — nothing ran, the entity lives on, and
    /// the caller still runs per-movetype physics. Errors from the think
    /// propagate (the caller decides whether to abort the frame).
    /// `SV_Physics_Pusher` (sv_phys.c): advance a `MOVETYPE_PUSH` bmodel
    /// (`func_door`, `func_plat`, `func_button`, trains) by its velocity over the
    /// frame, carrying riders and respecting blockers, then fire its `think` when
    /// the local time `ltime` reaches `nextthink`.
    ///
    /// Faithful to the C: the move time is clamped so the pusher never steps past
    /// its scheduled think, [`Self::push_move`] advances `ltime` (unless blocked),
    /// and the think runs with `self = ent`, `other = world`. A QuakeC error in
    /// the think is caught via `reset_execution` rather than aborting the host.
    /// Returns whether the think fired.
    fn physics_pusher(&mut self, ent: i32, start_time: f32, dt: f32) -> Result<bool> {
        let oldltime = self.vm.ent_get_float(ent, "ltime");
        let thinktime = self.vm.ent_get_float(ent, "nextthink");

        let movetime = if thinktime < oldltime + dt {
            let m = thinktime - oldltime;
            if m < 0.0 {
                0.0
            } else {
                m
            }
        } else {
            dt
        };

        if movetime != 0.0 {
            // SV_PushMove advances ent.ltime if it is not blocked.
            self.push_move(ent, movetime, start_time)?;
        }

        let ltime = self.vm.ent_get_float(ent, "ltime");
        let mut fired = false;
        if thinktime > oldltime && thinktime <= ltime {
            self.vm.ent_set_float(ent, "nextthink", 0.0);
            self.vm.gset_float("time", start_time);
            self.vm.gset_int("self", ent);
            self.vm.gset_int("other", 0); // world
            let think = self.vm.ent_get_int(ent, "think");
            if think > 0 {
                fired = true;
                if self.vm.execute(think as usize).is_err() {
                    self.vm.reset_execution();
                }
            }
        }
        Ok(fired)
    }

    /// `SV_PushMove` (sv_phys.c): translate a pusher by `velocity * movetime`,
    /// dragging every entity that is either riding it (`FL_ONGROUND` with
    /// `groundentity == pusher`) or whose box intersects the pusher's swept AABB.
    ///
    /// If a dragged entity ends up stuck (its box overlaps solid geometry after
    /// moving), the whole move is reverted — the pusher and every already-moved
    /// entity are restored to their saved origins — and the pusher's `blocked`
    /// function is invoked (caught, never fatal). Otherwise the move stands and
    /// `ltime` is advanced.
    fn push_move(&mut self, pusher: i32, movetime: f32, sv_time: f32) -> Result<()> {
        let velocity = self.vm.ent_get_vector(pusher, "velocity");
        if velocity[0] == 0.0 && velocity[1] == 0.0 && velocity[2] == 0.0 {
            let lt = self.vm.ent_get_float(pusher, "ltime");
            self.vm.ent_set_float(pusher, "ltime", lt + movetime);
            return Ok(());
        }

        let mut mov = [0.0f32; 3];
        for i in 0..3 {
            mov[i] = velocity[i] * movetime;
        }

        // Swept AABB of the pusher's move: start from absmin/absmax, then for each
        // axis extend the leading edge in the direction of travel.
        let absmin = self.vm.ent_get_vector(pusher, "absmin");
        let absmax = self.vm.ent_get_vector(pusher, "absmax");
        let mut mins = absmin;
        let mut maxs = absmax;
        for i in 0..3 {
            if mov[i] < 0.0 {
                mins[i] += mov[i];
            } else {
                maxs[i] += mov[i];
            }
        }

        // Save and apply the pusher move.
        let pushorig = self.vm.ent_get_vector(pusher, "origin");
        self.vm
            .ent_set_vector(pusher, "origin", v_add(pushorig, mov));
        let lt = self.vm.ent_get_float(pusher, "ltime");
        self.vm.ent_set_float(pusher, "ltime", lt + movetime);
        link_edict(&mut self.vm, pusher);

        // Collect entities to drag, moving each as we go (origin, saved-origin).
        let mut moved: Vec<(i32, Vec3)> = Vec::new();
        let num = self.vm.num_edicts() as i32;
        let mut blocker: Option<i32> = None;

        let mut check: i32 = 1;
        while check < num {
            if self.vm.edict_free.get(check as usize).copied().unwrap_or(true) {
                check += 1;
                continue;
            }
            if check == pusher {
                check += 1;
                continue;
            }
            let ck_movetype = self.vm.ent_get_float(check, "movetype") as i32;
            // SV_PushMove skips PUSH, NONE, and NOCLIP entities (sv_phys.c:478).
            if ck_movetype == MOVETYPE_PUSH
                || ck_movetype == MOVETYPE_NONE
                || ck_movetype == MOVETYPE_NOCLIP
            {
                check += 1;
                continue;
            }

            // The check entity must be standing on the pusher, or its box must
            // intersect the pusher's swept box; otherwise it is unaffected.
            let flags = self.vm.ent_get_float(check, "flags") as i32;
            let ground = self.vm.ent_get_int(check, "groundentity");
            let riding = (flags & FL_ONGROUND) != 0 && ground == pusher;
            if !riding {
                let ck_absmin = self.vm.ent_get_vector(check, "absmin");
                let ck_absmax = self.vm.ent_get_vector(check, "absmax");
                if ck_absmin[0] >= maxs[0]
                    || ck_absmin[1] >= maxs[1]
                    || ck_absmin[2] >= maxs[2]
                    || ck_absmax[0] <= mins[0]
                    || ck_absmax[1] <= mins[1]
                    || ck_absmax[2] <= mins[2]
                {
                    check += 1;
                    continue;
                }
                // SV_PushMove (sv_phys.c): after the swept-box overlap test, a
                // non-rider is only dragged if its bbox is actually inside the
                // pusher's FINAL position (`if (!SV_TestEntityPosition(check))
                // continue;`). The pusher origin was already advanced above, so
                // this tests the (un-moved) check against the moved pusher and
                // skips entities that merely brush the swept box without
                // penetrating — no spurious pushing.
                if !self.push_test_position(check) {
                    check += 1;
                    continue;
                }
            }

            // Remove the onground flag for non-players (it is re-derived below).
            if ck_movetype != MOVETYPE_WALK {
                let f = self.vm.ent_get_float(check, "flags") as i32;
                self.vm
                    .ent_set_float(check, "flags", (f & !FL_ONGROUND) as f32);
            }

            // Drag the check along with the pusher and record it for rollback.
            // SV_PushMove (sv_phys.c:509-512) moves the rider/pushed entity via
            // SV_PushEntity (a CLIPPED move), temporarily making the pusher
            // SOLID_NOT so the rider does not clip on the pusher itself, then
            // restoring it. A clipped push lets a door push a rider against a
            // wall (so the door later blocks/crushes) instead of teleporting the
            // rider through solid geometry by an unclipped origin += mov.
            let entorig = self.vm.ent_get_vector(check, "origin");
            moved.push((check, entorig));

            let pusher_solid = self.vm.ent_get_float(pusher, "solid");
            self.vm.ent_set_float(pusher, "solid", SOLID_NOT as f32);
            self.push_entity(check, mov, sv_time);
            self.vm.ent_set_float(pusher, "solid", pusher_solid);
            // push_entity already linked `check` (SV_PushEntity -> SV_LinkEdict).

            // If the check is now stuck in solid geometry, the move is blocked.
            if self.push_test_position(check) {
                // A zero-thickness box (e.g. a flattened corpse) cannot block.
                let cmins = self.vm.ent_get_vector(check, "mins");
                let cmaxs = self.vm.ent_get_vector(check, "maxs");
                if cmins[0] == cmaxs[0] {
                    check += 1;
                    continue;
                }
                let csolid = self.vm.ent_get_float(check, "solid") as i32;
                if csolid == SOLID_NOT || csolid == SOLID_TRIGGER {
                    // Corpse: squish its box flat so it stops blocking.
                    let mut m = self.vm.ent_get_vector(check, "mins");
                    m[0] = 0.0;
                    m[1] = 0.0;
                    self.vm.ent_set_vector(check, "mins", m);
                    self.vm.ent_set_vector(check, "maxs", m);
                    check += 1;
                    continue;
                }
                blocker = Some(check);
                break;
            }

            check += 1;
        }

        if let Some(block) = blocker {
            // SV_PushMove (sv_phys.c:530-552) restores in a SPECIFIC order so the
            // pusher's `blocked` function sees the right world state:
            //   1. restore the BLOCKER (the stuck entity) and relink it,
            //   2. restore the PUSHER (origin + ltime) and relink it,
            //   3. run `blocked` (self=pusher, other=blocker),
            //   4. ONLY THEN move back the other already-dragged riders.
            // So when `blocked` runs, the OTHER riders are still at their pushed
            // positions — restoring them all up-front (as the prior code did)
            // changed what `blocked` observes.

            // 1. Restore the blocker. `block` is also the last entry in `moved`
            //    (pushed before its SV_PushEntity), so step 4's loop restores it
            //    again harmlessly — exactly as the C re-restores moved_edict.
            let block_saved = moved
                .iter()
                .rev()
                .find(|&&(e, _)| e == block)
                .map(|&(_, saved)| saved);
            if let Some(saved) = block_saved {
                self.vm.ent_set_vector(block, "origin", saved);
                link_edict(&mut self.vm, block);
            }

            // 2. Restore the pusher (origin, relink, roll back ltime).
            self.vm.ent_set_vector(pusher, "origin", pushorig);
            link_edict(&mut self.vm, pusher);
            let lt = self.vm.ent_get_float(pusher, "ltime");
            self.vm.ent_set_float(pusher, "ltime", lt - movetime);

            // 3. If the pusher has a "blocked" function, call it (self=pusher,
            //    other=blocker). Caught, never fatal.
            let blocked = self.vm.ent_get_int(pusher, "blocked");
            if blocked > 0 {
                self.vm.gset_int("self", pusher);
                self.vm.gset_int("other", block);
                if self.vm.execute(blocked as usize).is_err() {
                    self.vm.reset_execution();
                }
            }

            // 4. Move back every entity we already dragged (including the blocker
            //    again — harmless, matches the C loop).
            for &(e, saved) in &moved {
                self.vm.ent_set_vector(e, "origin", saved);
                link_edict(&mut self.vm, e);
            }
        }

        Ok(())
    }

    /// `SV_TestEntityPosition` (sv_phys.c): true when `ent`'s box overlaps solid
    /// geometry at its current origin. Implemented, as in the C, by tracing the
    /// entity's own box from its origin to its origin and reporting `startsolid`.
    fn push_test_position(&mut self, ent: i32) -> bool {
        let origin = self.vm.ent_get_vector(ent, "origin");
        let mins = self.vm.ent_get_vector(ent, "mins");
        let maxs = self.vm.ent_get_vector(ent, "maxs");
        let trace = sv_move(&mut self.vm, origin, origin, mins, maxs, ent, false, false);
        trace.startsolid
    }

    /// `SV_CheckStuck` (sv_phys.c:762): the "big hack" that frees a player wedged
    /// in the clipping hull. If the box is clear, snapshot `oldorigin` and return.
    /// Otherwise try `oldorigin`, then a 1-unit grid (`±1` in x/y, `0..18` up); the
    /// first clear spot wins (relink there). If nothing is clear, restore the
    /// original origin (the C `player is stuck`).
    ///
    /// Faithful transcription over [`Self::push_test_position`]
    /// (`SV_TestEntityPosition`). The console diagnostics are dropped (headless).
    fn check_stuck(&mut self, ent: i32) {
        if !self.push_test_position(ent) {
            // not stuck: remember this good spot.
            let origin = self.vm.ent_get_vector(ent, "origin");
            self.vm.ent_set_vector(ent, "oldorigin", origin);
            return;
        }

        let org = self.vm.ent_get_vector(ent, "origin");
        let oldorigin = self.vm.ent_get_vector(ent, "oldorigin");
        self.vm.ent_set_vector(ent, "origin", oldorigin);
        if !self.push_test_position(ent) {
            link_edict(&mut self.vm, ent);
            return;
        }

        for z in 0..18 {
            for i in -1..=1 {
                for j in -1..=1 {
                    let cand = [org[0] + i as f32, org[1] + j as f32, org[2] + z as f32];
                    self.vm.ent_set_vector(ent, "origin", cand);
                    if !self.push_test_position(ent) {
                        link_edict(&mut self.vm, ent);
                        return;
                    }
                }
            }
        }

        // still stuck: restore the original origin.
        self.vm.ent_set_vector(ent, "origin", org);
    }

    fn run_think(&mut self, ent: i32, sv_time: f32, dt: f32) -> Result<(bool, bool)> {
        let thinktime = self.vm.ent_get_float(ent, "nextthink");
        if thinktime <= 0.0 || thinktime > sv_time + dt {
            // Not due: SV_RunThink returns true (alive); nothing fired.
            return Ok((false, true));
        }
        // Don't let things stay in the past.
        let thinktime = if thinktime < sv_time { sv_time } else { thinktime };

        self.vm.ent_set_float(ent, "nextthink", 0.0);
        self.vm.gset_float("time", thinktime);
        self.vm.gset_int("self", ent);
        self.vm.gset_int("other", 0);

        let think = self.vm.ent_get_int(ent, "think");
        let fnum = think as usize;
        if think <= 0 || fnum >= self.vm.progs.functions.len() {
            // nextthink consumed (as the C did), but no valid think to run;
            // the entity is still alive.
            return Ok((false, true));
        }
        // The C leaves pr_global_struct->time at thinktime afterward; we mirror that.
        self.vm.execute(fnum)?;

        // alive = !ent->free.
        let free = self.vm.edict_free.get(ent as usize).copied().unwrap_or(true);
        Ok((true, !free))
    }

    /// `SV_Physics_Noclip` integration: `angles += dt*avelocity`,
    /// `origin += dt*velocity` (no clipping), then relink bounds.
    fn integrate_noclip(&mut self, ent: i32, dt: f32) {
        let angles = self.vm.ent_get_vector(ent, "angles");
        let avel = self.vm.ent_get_vector(ent, "avelocity");
        self.vm
            .ent_set_vector(ent, "angles", crate::math::mul_add(angles, dt, avel));

        let origin = self.vm.ent_get_vector(ent, "origin");
        let vel = self.vm.ent_get_vector(ent, "velocity");
        self.vm
            .ent_set_vector(ent, "origin", crate::math::mul_add(origin, dt, vel));

        link_edict(&mut self.vm, ent);
    }

    /// `SV_Physics_Step` (non-`QUAKE2`): freefall when the edict is not on
    /// ground / flying / swimming — `SV_AddGravity`, `SV_CheckVelocity`,
    /// `SV_FlyMove` (the full slide move via [`Self::fly_move_core`], which
    /// latches `FL_ONGROUND` on a floor contact and clips/slides velocity), then
    /// `SV_LinkEdict(ent, true)` to trip triggers. Touch impacts during the
    /// slide are handled inside [`Self::fly_move_core`] (which calls
    /// [`sv_impact`]).
    ///
    /// A falling step entity that lands this frame (was airborne, now
    /// `FL_ONGROUND`) plays `demon/dland2.wav` when it hit the ground hard
    /// (downward speed exceeded `sv_gravity * 0.1` before gravity was applied) —
    /// the landing thud. The caller ([`Self::process_entity`]) runs
    /// `SV_CheckWaterTransition` afterward (after the think), matching the C
    /// order, so even a step entity resting on the floor maintains
    /// `watertype`/`waterlevel` and splashes when pushed into liquid.
    fn physics_step(&mut self, ent: i32, sv_time: f32, dt: f32) {
        let flags = self.vm.ent_get_float(ent, "flags") as i32;
        if flags & (FL_ONGROUND | FL_FLY | FL_SWIM) == 0 {
            // hitsound = velocity[2] < sv_gravity * -0.1, sampled BEFORE gravity.
            let vel_z = self.vm.ent_get_vector(ent, "velocity")[2];
            let hitsound = vel_z < SV_GRAVITY * -0.1;

            // SV_Physics_Step freefall: AddGravity; CheckVelocity; SV_FlyMove;
            // SV_LinkEdict(ent, true). The C runs the full slide move (NOT a
            // single PushEntity), so a freefalling MOVETYPE_STEP entity latches
            // FL_ONGROUND on a floor contact and clips/slides its velocity
            // instead of accumulating downward speed forever.
            self.add_gravity(ent, dt);
            self.check_velocity(ent);
            let mut steptrace: Option<MoveTrace> = None;
            let _ = self.fly_move_core(ent, dt, sv_time, &mut steptrace);

            // SV_LinkEdict(ent, true) ends the freefall branch: it recomputes
            // absmin/absmax from the NEW origin AND trips triggers/pickups for the
            // moved entity. This is INSIDE the branch in the C (the on-ground /
            // flying / swimming path returns before it), so an entity that skipped
            // the move does not re-link here. Skip if a touch impact during the move
            // already removed the entity.
            if !self.vm.edict_free.get(ent as usize).copied().unwrap_or(true) {
                // Recompute absmin/absmax FIRST (C SV_LinkEdict order), so the
                // trigger overlap test — and, crucially, the sv_move abs-box
                // broadphase on later moves — see the fresh box. Without this a
                // fast-falling MOVETYPE_STEP monster kept a stale box and could be
                // wrongly broadphase-rejected (a missed collision).
                link_edict(&mut self.vm, ent);
                touch_triggers(&mut self.vm, ent, sv_time);

                // "just hit ground": FL_ONGROUND newly latched by the slide move
                // -> the landing thud, gated on the pre-gravity downward speed.
                let now_on_ground =
                    (self.vm.ent_get_float(ent, "flags") as i32) & FL_ONGROUND != 0;
                if now_on_ground && hitsound {
                    self.start_sound(ent, 0, "demon/dland2.wav", 255, 1.0);
                }
            }
        }
        // SV_CheckWaterTransition runs AFTER SV_RunThink in the C; the caller
        // (process_entity's MOVETYPE_STEP arm) invokes it post-think.
    }

    /// `SV_Physics_Toss` (sv_phys.c, non-`QUAKE2`): if on ground, do nothing;
    /// else add gravity (except FLY/FLYMISSILE), integrate angles, and move the
    /// origin via a clipped `PushEntity`. The bounce/stop fixups after an impact
    /// are applied via [`Self::clip_velocity`].
    fn physics_toss(&mut self, ent: i32, movetype: i32, sv_time: f32, dt: f32) {
        let flags = self.vm.ent_get_float(ent, "flags") as i32;
        if flags & FL_ONGROUND != 0 {
            return; // resting on the ground (C returns before CheckWaterTransition)
        }
        self.check_velocity(ent);

        // add gravity (not for FLY / FLYMISSILE)
        if movetype != MOVETYPE_FLY && movetype != MOVETYPE_FLYMISSILE {
            self.add_gravity(ent, dt);
        }

        // move angles
        let angles = self.vm.ent_get_vector(ent, "angles");
        let avel = self.vm.ent_get_vector(ent, "avelocity");
        self.vm
            .ent_set_vector(ent, "angles", crate::math::mul_add(angles, dt, avel));

        // move origin
        let vel = self.vm.ent_get_vector(ent, "velocity");
        let move_ = crate::math::scale(vel, dt);
        let tr = self.push_entity(ent, move_, sv_time);

        // SV_PushEntity ends with SV_LinkEdict(ent, true): trip triggers/pickups
        // for the moved entity (unless a touch impact already removed it).
        if !self.vm.edict_free.get(ent as usize).copied().unwrap_or(true) {
            touch_triggers(&mut self.vm, ent, sv_time);
        }

        if tr.fraction == 1.0 {
            return; // clear move
        }
        let free = self.vm.edict_free.get(ent as usize).copied().unwrap_or(true);
        if free {
            return;
        }

        let backoff = if movetype == MOVETYPE_BOUNCE { 1.5 } else { 1.0 };
        let vel = self.vm.ent_get_vector(ent, "velocity");
        let new_vel = clip_velocity(vel, tr.plane_normal, backoff);
        self.vm.ent_set_vector(ent, "velocity", new_vel);

        // stop if on ground (nested ifs in the C, collapsed here — no elses)
        if tr.plane_normal[2] > 0.7 && (new_vel[2] < 60.0 || movetype != MOVETYPE_BOUNCE) {
            let flags = self.vm.ent_get_float(ent, "flags") as i32;
            self.vm
                .ent_set_float(ent, "flags", (flags | FL_ONGROUND) as f32);
            // groundentity = EDICT_TO_PROG(trace.ent): the edict actually
            // landed on (0 = world, >0 = a plat/door/other solid), not a
            // hardcoded world. `tr.ent` is `-1` only when nothing was hit,
            // but this branch runs only when fraction < 1 (something WAS hit),
            // so clamp the "nothing" sentinel to the world (0) defensively.
            self.vm.ent_set_int(ent, "groundentity", tr.ent.max(0));
            self.vm.ent_set_vector(ent, "velocity", [0.0; 3]);
            self.vm.ent_set_vector(ent, "avelocity", [0.0; 3]);
        }

        // check for in water (SV_CheckWaterTransition). The C reaches this only
        // when the move was NOT clear (the `fraction == 1` / freed early returns
        // above skip it), so a grenade/gib that just struck something updates its
        // watertype here and splashes on an air/liquid crossing.
        if !self.vm.edict_free.get(ent as usize).copied().unwrap_or(true) {
            self.check_water_transition(ent);
        }
    }

    /// `SV_CheckWater` (sv_phys.c:808): sample the world contents at the entity's
    /// feet, waist and eyes and set its `waterlevel` (0..3) + `watertype`
    /// (`CONTENTS_WATER`/`SLIME`/`LAVA`). Without this the QuakeC `WaterMove`
    /// (run from `PlayerPostThink`) sees `waterlevel == 0` forever and never deals
    /// lava/slime drowning damage. Returns `true` when at least waist-deep
    /// (`waterlevel > 1`), which the caller uses to suppress gravity. A content
    /// `<= CONTENTS_WATER` (-3) is liquid (LAVA -5 < SLIME -4 < WATER -3).
    fn check_water(&mut self, ent: i32) -> bool {
        const CONTENTS_WATER: i32 = -3;
        let origin = self.vm.ent_get_vector(ent, "origin");
        let mins = self.vm.ent_get_vector(ent, "mins");
        let maxs = self.vm.ent_get_vector(ent, "maxs");
        let view_ofs = self.vm.ent_get_vector(ent, "view_ofs");
        let contents_at = |s: &mut Self, z: f32| -> i32 {
            let p = [origin[0], origin[1], z];
            s.vm.with_host(|_vm, h| h.point_contents(p)).unwrap_or(CONTENTS_SOLID)
        };

        let mut waterlevel = 0i32;
        let mut watertype = CONTENTS_EMPTY;
        // Feet: origin.z + mins.z + 1.
        if contents_at(self, origin[2] + mins[2] + 1.0) <= CONTENTS_WATER {
            watertype = contents_at(self, origin[2] + mins[2] + 1.0);
            waterlevel = 1;
            // Waist: midpoint of the box.
            if contents_at(self, origin[2] + (mins[2] + maxs[2]) * 0.5) <= CONTENTS_WATER {
                waterlevel = 2;
                // Eyes: origin.z + view_ofs.z.
                if contents_at(self, origin[2] + view_ofs[2]) <= CONTENTS_WATER {
                    waterlevel = 3;
                }
            }
        }
        self.vm.ent_set_float(ent, "waterlevel", waterlevel as f32);
        self.vm.ent_set_float(ent, "watertype", watertype as f32);
        waterlevel > 1
    }

    /// `SV_CheckWaterTransition` (sv_phys.c, non-`QUAKE2`): sample the world
    /// contents at the entity's origin and maintain its `watertype` / `waterlevel`
    /// fields, playing the `misc/h2ohit1.wav` splash whenever the entity crosses
    /// the air/liquid boundary in either direction.
    ///
    /// Faithful to the C:
    /// * `watertype == 0` (never set, i.e. just spawned) -> adopt the current
    ///   contents and `waterlevel = 1` with no sound.
    /// * contents is liquid (`<= CONTENTS_WATER`): if we were in `CONTENTS_EMPTY`
    ///   we just splashed in -> play the sound; set `watertype = cont`,
    ///   `waterlevel = 1`.
    /// * contents is not liquid: if `watertype` was not already `CONTENTS_EMPTY`
    ///   we just surfaced -> play the sound; set `watertype = CONTENTS_EMPTY`,
    ///   `waterlevel = cont` (the C stores the raw contents value here).
    ///
    /// Without this, `MOVETYPE_TOSS`/`BOUNCE`/`STEP` entities (grenades, gibs,
    /// dropped weapons, falling monsters) never get `watertype`/`waterlevel` and
    /// emit no entry splash.
    fn check_water_transition(&mut self, ent: i32) {
        const CONTENTS_WATER: i32 = -3;
        let origin = self.vm.ent_get_vector(ent, "origin");
        let cont = self
            .vm
            .with_host(|_vm, h| h.point_contents(origin))
            .unwrap_or(CONTENTS_SOLID);

        let watertype = self.vm.ent_get_float(ent, "watertype") as i32;
        if watertype == 0 {
            // just spawned here
            self.vm.ent_set_float(ent, "watertype", cont as f32);
            self.vm.ent_set_float(ent, "waterlevel", 1.0);
            return;
        }

        if cont <= CONTENTS_WATER {
            if watertype == CONTENTS_EMPTY {
                // just crossed into water
                self.start_sound(ent, 0, "misc/h2ohit1.wav", 255, 1.0);
            }
            self.vm.ent_set_float(ent, "watertype", cont as f32);
            self.vm.ent_set_float(ent, "waterlevel", 1.0);
        } else {
            if watertype != CONTENTS_EMPTY {
                // just crossed out of water
                self.start_sound(ent, 0, "misc/h2ohit1.wav", 255, 1.0);
            }
            self.vm.ent_set_float(ent, "watertype", CONTENTS_EMPTY as f32);
            self.vm.ent_set_float(ent, "waterlevel", cont as f32);
        }
    }

    /// `SV_AddGravity` (sv_phys.c): `velocity[2] -= gravity * sv_gravity * dt`,
    /// where the per-entity `gravity` field defaults to 1.0 when unset/zero.
    fn add_gravity(&mut self, ent: i32, dt: f32) {
        let ent_gravity = {
            let g = self.vm.ent_get_float(ent, "gravity");
            if g != 0.0 {
                g
            } else {
                1.0
            }
        };
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        vel[2] -= ent_gravity * SV_GRAVITY * dt;
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `SV_CheckVelocity` (sv_phys.c): clamp each velocity component to
    /// `±sv_maxvelocity` and scrub NaNs from velocity/origin.
    fn check_velocity(&mut self, ent: i32) {
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        let mut origin = self.vm.ent_get_vector(ent, "origin");
        for i in 0..3 {
            if vel[i].is_nan() {
                vel[i] = 0.0;
            }
            if origin[i].is_nan() {
                origin[i] = 0.0;
            }
            // vel[i] is NaN-scrubbed above, so .clamp matches the C's if/else if.
            vel[i] = vel[i].clamp(-SV_MAXVELOCITY, SV_MAXVELOCITY);
        }
        self.vm.ent_set_vector(ent, "velocity", vel);
        self.vm.ent_set_vector(ent, "origin", origin);
    }

    /// `SV_PushEntity` (sv_phys.c ~408): move `ent` by `push` via the
    /// entity-aware [`sv_move`] (clipping against the world AND every solid
    /// edict), set `origin = trace.endpos`, relink, and — when the move hit
    /// another entity — run [`sv_impact`] so both touch functions fire. The
    /// returned [`MoveTrace`] carries `fraction`/`plane_normal` for the toss/step
    /// physics' bounce/stop fixups AND the hit `ent` index, which the toss-rest
    /// and stair-step-down paths store as `groundentity` (`EDICT_TO_PROG(trace
    /// .ent)`) so an entity resting on a plat/door records what it stands on.
    ///
    /// `sv_move` borrows the host internally and `sv_impact` executes QuakeC, so
    /// neither is called while the host is held out.
    fn push_entity(&mut self, ent: i32, push: Vec3, sv_time: f32) -> MoveTrace {
        let origin = self.vm.ent_get_vector(ent, "origin");
        let mins = self.vm.ent_get_vector(ent, "mins");
        let maxs = self.vm.ent_get_vector(ent, "maxs");
        let end = v_add(origin, push);

        // SV_PushEntity (sv_phys.c:408-421) selects the move type from the
        // MOVING entity:
        //   * MOVETYPE_FLYMISSILE  -> MOVE_MISSILE   (FL_MONSTER touch entities
        //     are clipped against a +-15 box so a rocket detonates NEAR a
        //     monster, not only on a direct hit).
        //   * SOLID_TRIGGER / SOLID_NOT -> MOVE_NOMONSTERS (dropped backpacks /
        //     gibs / corpses pass THROUGH monster+player boxes instead of
        //     hanging on them; only bmodels block).
        //   * otherwise -> MOVE_NORMAL.
        let movetype = self.vm.ent_get_float(ent, "movetype") as i32;
        let solid = self.vm.ent_get_float(ent, "solid") as i32;
        let missile = movetype == MOVETYPE_FLYMISSILE;
        let nomonsters = !missile && (solid == SOLID_TRIGGER || solid == SOLID_NOT);

        // Entity-aware move: clips world + all solid edicts; `ent` ignores
        // itself (the C `passedict`).
        let mt = sv_move(&mut self.vm, origin, end, mins, maxs, ent, nomonsters, missile);

        self.vm.ent_set_vector(ent, "origin", mt.endpos);
        link_edict(&mut self.vm, ent);

        // SV_Impact (sv_phys.c SV_PushEntity ~426): `if (trace.ent) SV_Impact(...)`.
        // trace.ent is the WORLD edict (index 0, a non-NULL pointer) on any world
        // clip, so the mover's touch fires on world contact too — that is what makes
        // a rocket fired into a wall DETONATE and a grenade clang (GrenadeTouch plays
        // bounce.wav vs world). MoveTrace yields ent==-1 only for a clear move, ==0
        // for a world hit, >0 for an entity, so `>= 0` includes the world and
        // excludes only the no-hit case. (Previously `> 0` skipped every world hit,
        // so wall-struck rockets/nails never exploded.)
        if mt.ent >= 0 {
            sv_impact(&mut self.vm, ent, mt.ent, sv_time);
        }

        // Return the full MoveTrace (carries `ent` for groundentity in addition
        // to the fraction/plane the bounce-and-stop fixups need).
        mt
    }
}

// ---------------------------------------------------------------------------
// (C2) The local player / client.
//
// Ported from sv_user.c (`SV_ClientThink`, `SV_AirMove`, `SV_UserFriction`,
// `SV_Accelerate`, `SV_AirAccelerate`, `DropPunchAngle`), sv_phys.c
// (`SV_Physics_Client`, `SV_WalkMove`, `SV_FlyMove`, `SV_Physics`), host_cmd.c
// (`Host_Spawn_f`) and sv_main.c (`SV_ConnectClient`). The player is a real
// edict the QuakeC game logic owns (health/items/weapons); the engine drives
// its per-frame physics with friction, acceleration and ENTITY-AWARE collision
// (via [`sv_move`]) so it collides with monsters, doors and items.
// ---------------------------------------------------------------------------

impl Server {
    /// The local player's edict index, or `-1` if no client has connected.
    pub fn player_edict(&self) -> i32 {
        self.player
    }

    /// Convenience: the player's `health` field (for a HUD / verification). 0.0
    /// when no client is connected.
    pub fn player_health(&self) -> f32 {
        if self.player < 0 {
            0.0
        } else {
            self.vm.ent_get_float(self.player, "health")
        }
    }

    /// The player's view: `(eye, v_angle)` where `eye = origin + view_ofs`
    /// (defaulting `view_ofs` to `(0,0,22)` when the QuakeC left it unset) and
    /// `v_angle` is `[pitch, yaw, roll]`. Both are zero when no client exists.
    pub fn player_view(&self) -> ([f32; 3], [f32; 3]) {
        if self.player < 0 {
            return ([0.0; 3], [0.0; 3]);
        }
        let origin = self.vm.ent_get_vector(self.player, "origin");
        let mut ofs = self.vm.ent_get_vector(self.player, "view_ofs");
        if ofs == [0.0, 0.0, 0.0] {
            ofs = [0.0, 0.0, DEFAULT_VIEWHEIGHT];
        }
        let eye = [origin[0] + ofs[0], origin[1] + ofs[1], origin[2] + ofs[2]];
        let v_angle = self.vm.ent_get_vector(self.player, "v_angle");
        (eye, v_angle)
    }

    /// Resolve a named *system* QuakeC function (`StartFrame`, `PlayerPreThink`,
    /// …). These are stored in like-named globals (`pr_global_struct->X`); prefer
    /// the function index in that global, fall back to a by-name lookup. Returns
    /// `None` when the program defines neither.
    fn sys_function(&self, name: &str) -> Option<usize> {
        let g = self.vm.gget_int(name);
        if g > 0 && (g as usize) < self.vm.progs.functions.len() {
            return Some(g as usize);
        }
        self.vm.progs.find_function(name)
    }

    /// Execute a system QuakeC function with `self = self_e`, `other = other_e`.
    /// Returns `Ok(true)` if it existed and ran, `Ok(false)` if absent. A program
    /// fault is caught (`reset_execution`) and surfaced as `Err`, never panicked.
    fn run_sys(&mut self, name: &str, self_e: i32, other_e: i32) -> Result<bool> {
        let Some(f) = self.sys_function(name) else {
            return Ok(false);
        };
        self.vm.gset_int("self", self_e);
        self.vm.gset_int("other", other_e);
        if let Err(e) = self.vm.execute(f) {
            self.vm.reset_execution();
            return Err(crate::QError::invalid(format!(
                "QuakeC error in {name}(): {e}"
            )));
        }
        Ok(true)
    }

    /// Spawn the local player and run the connect/spawn entrance script.
    ///
    /// Mirrors `SV_ConnectClient` + `Host_Spawn_f`: reserve a fresh edict, make
    /// it `self`, run `SetNewParms` (fills `parm1..parm16` — the fresh-game
    /// loadout), then `ClientConnect`, then `PutClientInServer` (the QuakeC sets
    /// `origin` from `info_player_start`, plus `health`/`model`/`items`/
    /// `view_ofs`). Marks the edict a walking client (`MOVETYPE_WALK`,
    /// `SOLID_SLIDEBOX`), records the view entity, links it into the world, and
    /// returns its index. QuakeC faults are caught and surfaced, not panicked.
    ///
    /// SINGLE-CLIENT SIMPLIFICATION: the C copies the parm globals into the
    /// `client_t.spawn_parms` after `SetNewParms` and copies them back before
    /// `PutClientInServer`. With exactly one client and no save/load round-trip
    /// that copy is the identity, so we run `SetNewParms` immediately before the
    /// connect/spawn pair and let the parm globals carry straight through.
    pub fn connect_client(&mut self) -> Result<i32> {
        // Fresh game: SetNewParms fills parm1..parm16 with the new-game loadout
        // (shotgun + axe, 100 health), then they pass straight to
        // PutClientInServer (single-client identity copy).
        self.connect_client_inner(|s, ent| {
            s.run_sys("SetNewParms", ent, 0)?;
            Ok(())
        })
    }

    /// Spawn the local player carrying *saved* spawn parameters across a level
    /// change. Like [`Self::connect_client`] but, instead of `SetNewParms`
    /// (which would reset the loadout to the fresh-game default), it writes the
    /// 16 saved `parm1..parm16` values into the globals first, then runs
    /// `ClientConnect` + `PutClientInServer` — whose QuakeC `DecodeLevelParms`
    /// reads them back into the player's `items`/`health`/`ammo_*`/`weapon`/
    /// `armorvalue` fields. NET EFFECT: the inventory carries to the new map.
    ///
    /// Mirrors `SV_SpawnServer`'s reconnect path: the engine copies the client's
    /// saved `spawn_parms` back into `pr_global_struct->parm1..16` before calling
    /// `PutClientInServer`. Returns the player edict index. QuakeC faults are
    /// caught and surfaced, not panicked; a missing parm global is a silent
    /// no-op (`gset_float`).
    pub fn connect_client_with_parms(&mut self, parms: [f32; NUM_SPAWN_PARMS]) -> Result<i32> {
        self.connect_client_inner(move |s, _ent| {
            for (i, v) in parms.iter().enumerate() {
                s.vm.gset_float(&parm_global_name(i), *v);
            }
            Ok(())
        })
    }

    /// Shared body of [`Self::connect_client`] / [`Self::connect_client_with_parms`]:
    /// reserve the player edict, default its physics fields, run `setup_parms`
    /// (the only step that differs — fresh `SetNewParms` vs. restoring saved
    /// parms), then `ClientConnect` + `PutClientInServer`, re-assert physics,
    /// record the view entity, mark `FL_CLIENT`, and link into the world.
    fn connect_client_inner(
        &mut self,
        setup_parms: impl FnOnce(&mut Self, i32) -> Result<()>,
    ) -> Result<i32> {
        // Reserve a fresh edict (the first free slot after spawn_entities).
        let ent = self.vm.spawn();
        self.player = ent;

        // Default the engine-managed physics fields before the script runs, so a
        // minimal mod that only sets health/origin still yields a walking client
        // (the C `SV_SpawnServer` set up the client slot likewise). The QuakeC
        // PutClientInServer normally sets these too.
        self.vm
            .ent_set_float(ent, "movetype", MOVETYPE_WALK as f32);
        self.vm
            .ent_set_float(ent, "solid", SOLID_SLIDEBOX as f32);

        // Establish parm1..parm16 (fresh loadout, or restored saved parms).
        setup_parms(self, ent)?;

        // SV_ConnectClient (sv_main.c): copy the parm globals into the client's
        // spawn_parms right after SetNewParms (or the restored carried set).
        // These are the level-ENTRY parms `Host_Savegame_f` writes into a save.
        for (i, p) in self.client_spawn_parms.iter_mut().enumerate() {
            *p = self.vm.gget_float(&parm_global_name(i));
        }

        // ClientConnect then PutClientInServer (the C runs both with self=player).
        self.run_sys("ClientConnect", ent, 0)?;
        self.run_sys("PutClientInServer", ent, 0)?;

        // Re-assert the engine-managed physics fields if the mod cleared them.
        if self.vm.ent_get_float(ent, "movetype") as i32 == MOVETYPE_NONE {
            self.vm
                .ent_set_float(ent, "movetype", MOVETYPE_WALK as f32);
        }
        if self.vm.ent_get_float(ent, "solid") as i32 == SOLID_NOT {
            self.vm
                .ent_set_float(ent, "solid", SOLID_SLIDEBOX as f32);
        }

        // Record the view entity (what the client looks through). NOTE: real
        // progs.dat has no `viewentity` global, so this write is a no-op there;
        // client identity is carried by the FL_CLIENT flag below instead.
        self.vm.gset_float("viewentity", ent as f32);

        // Mark the edict a client (FL_CLIENT). The C engine sets this when a
        // client connects; monster AI's FindTarget / checkclient look for it.
        let flags = self.vm.ent_get_float(ent, "flags") as i32;
        self.vm
            .ent_set_float(ent, "flags", (flags | FL_CLIENT) as f32);

        // Link into the collision world so absmin/absmax are valid.
        link_edict(&mut self.vm, ent);

        Ok(ent)
    }

    /// The signon-sequence physics frames between `PutClientInServer` and the
    /// first rendered frame. Call once after [`Self::connect_client`] /
    /// [`Self::connect_client_with_parms`], BEFORE rendering frame 0.
    ///
    /// In WinQuake a connecting client's signon spans several host frames: the
    /// "spawn" client command (`Host_Spawn_f`, host_cmd.c — runs QuakeC
    /// `PutClientInServer`) and the "begin" command (`Host_Begin_f`) execute in
    /// `SV_RunClients` on consecutive frames, and each of those frames then runs
    /// `SV_Physics` (host.c `Host_ServerFrame`) before the client reaches
    /// signon 4 and `SCR_EndLoadingPlaque` re-enables drawing (cl_parse.c
    /// `CL_ParseUpdate`: "first update is the final signon stage"). During those
    /// ticks `SV_Physics_Client` runs full player physics — the client is
    /// `active`, just not yet `spawned`, so its movement cmd stays zeroed
    /// (sv_user.c `SV_RunClients`: `if (!host_client->spawned) memset(&cmd...)`)
    /// — which matters because QuakeC `PutClientInServer` places the player at
    /// `spot.origin + '0 0 1'` and some spawn spots float well above the floor
    /// (the start map's `info_player_start` is ~5 units up): the player falls to
    /// the ground DURING the signon, before the first visible frame.
    ///
    /// This port's `connect_client` compresses the whole signon round-trip into
    /// one call, so a front-end that rendered immediately after it would show
    /// the settle on screen — a one-time whole-view shift over the first frames
    /// (the reported texture/lighting "pop"). The C ticks run at
    /// `Host_FilterTime`'s real frame duration, clamped to at most 0.1 s — and
    /// 0.1 is also exactly the `host_frametime` id hard-codes for
    /// `SV_SpawnServer`'s own two "let everything settle" frames — so we run the
    /// two ticks at [`SETTLE_FRAMETIME`], which settles any spawn drop up to
    /// ~24 units deterministically. The cmd carries the player's current view
    /// angles (the C never touches `v_angle` during signon) with zero
    /// moves/buttons. Think faults are isolated by `client_frame`; a hard fault
    /// is swallowed (a boot must not fail over a settle tick), matching
    /// `spawn_entities`' own settle-frame handling. The golden `scene` tool
    /// never connects a client, so this does not affect golden renders.
    pub fn run_signon_frames(&mut self) {
        let (yaw, pitch) = if self.player >= 0 {
            let ang = self.vm.ent_get_vector(self.player, "angles");
            let vang = self.vm.ent_get_vector(self.player, "v_angle");
            (ang[1], vang[0])
        } else {
            (0.0, 0.0)
        };
        let cmd = UserCmd {
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            yaw,
            pitch,
            buttons: 0,
            impulse: 0,
        };
        let _ = self.client_frame(&cmd, SETTLE_FRAMETIME);
        let _ = self.client_frame(&cmd, SETTLE_FRAMETIME);
    }

    /// `SV_SaveSpawnparms` for the local client: set the QuakeC `self` global to
    /// the player edict, run the progs `SetChangeParms` (which writes the
    /// player's persistent state — items/health/ammo/weapon/armor — into the 16
    /// `parm1..parm16` globals), then read those globals back into an array the
    /// caller can hand to a new server's [`Self::connect_client_with_parms`].
    ///
    /// Mirrors `SV_SaveSpawnparms` (host.c): `pr_global_struct->self = client`,
    /// `PR_ExecuteProgram(SetChangeParms)`, then copy `parm1..16` into
    /// `client->spawn_parms`. If the progs lacks `SetChangeParms` (a minimal mod)
    /// the run is a no-op and the *current* parm globals are returned unchanged;
    /// a missing individual parm global reads as `0.0` (`gget_float`), so this
    /// never panics. Returns `[0.0; 16]` when no client has connected.
    pub fn save_spawn_parms(&mut self) -> [f32; NUM_SPAWN_PARMS] {
        let mut parms = [0.0f32; NUM_SPAWN_PARMS];
        if self.player < 0 {
            return parms;
        }
        // SetChangeParms writes parm1..parm16 from the player's live fields
        // (self = the player edict, other = world). A fault is caught by run_sys.
        let _ = self.run_sys("SetChangeParms", self.player, 0);
        for (i, p) in parms.iter_mut().enumerate() {
            *p = self.vm.gget_float(&parm_global_name(i));
        }
        parms
    }

    /// Read the `serverflags` QuakeC global (the episode rune `SERVERFLAG_*`
    /// bits the player carries between levels). Returns `0.0` if the progs has
    /// no such global. The C keeps `pr_global_struct->serverflags` alive across
    /// `SV_SpawnServer`; a front-end driving a changelevel reads it from the
    /// outgoing server and writes it into the incoming one with
    /// [`Self::set_serverflags`] so the runes are not lost each level.
    pub fn serverflags(&self) -> f32 {
        self.vm.gget_float("serverflags")
    }

    /// Write the `serverflags` QuakeC global. A no-op if the progs lacks the
    /// global (the loader guards the offset), so calling it on a progs without
    /// runes is harmless. See [`Self::serverflags`].
    pub fn set_serverflags(&mut self, flags: f32) {
        self.vm.gset_float("serverflags", flags);
    }

    /// The current integer skill level (0=easy, 1=medium, 2=hard, 3=nightmare).
    ///
    /// This is the `current_skill` the spawn filter uses and the value
    /// `cvar("skill")` returns to the QuakeC. The difficulty portals in the start
    /// map (`trigger_setskill`) change it at runtime via `cvar_set("skill", N)`;
    /// a front-end reads it here to persist the player's choice across a
    /// changelevel (the constructor resets it to the medium default).
    pub fn skill(&self) -> i32 {
        skill_value()
    }

    /// Set the skill level from a raw value, normalised exactly as
    /// `SV_SpawnServer` does (`current_skill = (int)(value + 0.5)`, clamped to
    /// `0..=3`). A front-end calls this after construction to apply the menu's /
    /// the persisted difficulty before [`Self::spawn_entities`], so the spawn
    /// filter inhibits the right monsters/items. See [`Self::skill`].
    pub fn set_skill(&mut self, value: f32) {
        set_skill_value(value);
    }

    /// Take (and clear) the deferred level-change request a `changelevel()`
    /// builtin recorded this frame, or `None` if none was issued. A front-end
    /// calls this once after [`Self::client_frame`]: when it returns `Some(map)`,
    /// the front-end saves the spawn parms, loads `map`, and reconnects the
    /// client carrying its inventory. Mirrors the engine processing the deferred
    /// `changelevel <map>` console command after the frame.
    pub fn take_pending_changelevel(&mut self) -> Option<String> {
        take_changelevel()
    }

    /// Take and clear a pending single-player respawn (`localcmd("restart")`). The
    /// front-end calls this once after [`Self::client_frame`]: when it returns
    /// `true`, it reloads the CURRENT level (carrying the level-entry spawn parms,
    /// not the dead player's state). Mirrors the engine running the deferred
    /// `restart` console command after the frame.
    pub fn take_pending_restart(&mut self) -> bool {
        RESTART_REQUEST.with(|c| std::mem::replace(&mut *c.borrow_mut(), false))
    }

    /// `Host_Kill_f` (host_cmd.c): the `kill` console command — suicide via the
    /// QuakeC `ClientKill` entry point, NOT a health hack. Faithfully:
    /// * an already-dead player is refused (the C prints `"Can't suicide --
    ///   allready dead!\n"`; we return `Ok(false)` and the front-end prints it);
    /// * otherwise set `pr_global_struct->time = sv.time`, `self = sv_player`,
    ///   and execute `ClientKill` — whose QuakeC (client.qc) plays the suicide
    ///   frame, docks two frags, and calls `respawn()`, which in single player
    ///   issues `localcmd("restart\n")`, surfaced via
    ///   [`Server::take_pending_restart`] for the front-end to reload the level.
    ///
    /// Returns `Ok(true)` when `ClientKill` ran, `Ok(false)` when refused (dead,
    /// or no connected client). A QuakeC fault surfaces as `Err` (interpreter
    /// already reset), matching the other system entry points.
    pub fn client_kill(&mut self) -> Result<bool> {
        let player = self.player;
        if player < 0 || self.is_free(player) {
            return Ok(false);
        }
        if self.vm.ent_get_float(player, "health") <= 0.0 {
            return Ok(false); // "Can't suicide -- allready dead!"
        }
        // pr_global_struct->time = sv.time; self = sv_player; run ClientKill.
        let t = self.time();
        self.vm.gset_float("time", t);
        self.run_sys("ClientKill", player, 0)?;
        Ok(true)
    }

    /// One server frame driven by the local player's input.
    ///
    /// Mirrors `Host_Frame` -> `SV_Physics`: advance `time`/`frametime`, run the
    /// `StartFrame` system function (self/other = world), then `SV_Physics` over
    /// every live edict — the player edict via `SV_Physics_Client`
    /// (`PlayerPreThink` -> movement -> `PlayerPostThink`), all others via the
    /// generic [`Self::process_entity`] path. `dt` is the frame time.
    pub fn client_frame(&mut self, cmd: &UserCmd, dt: f32) -> Result<FrameReport> {
        // host_frametime = dt; sv.time advances at the END of SV_Physics in the
        // C, but the think-due test compares against sv.time + host_frametime, so
        // (as run_frame does) we set frametime now and bump time after the loop.
        self.vm.gset_float("frametime", dt);
        // Drop any half-collected temp-entity message from a prior (possibly
        // faulted) frame so this frame's Write* bursts parse cleanly.
        reset_temp_entity_decoder();
        reset_svc_recognizer();
        // Drop any changelevel() / restart request a *prior* frame left unconsumed
        // (a well-behaved front-end drains it immediately, but a stale request must
        // never trigger a swap/respawn a frame late or against the wrong level).
        reset_changelevel();
        reset_restart();
        // SV_CleanupEnts: clear last frame's one-frame EF_MUZZLEFLASH before this
        // frame's thinks (the host already consumed it via entity_dlights()).
        self.cleanup_ents();
        let start_time = self.time();
        // Record sv.time for the monster-locomotion relink touches (SV_TouchLinks
        // uses sv.time, not the clamped per-think `time` global).
        self.vm.sv_time = start_time;

        // Let the progs know a new frame has started (self/other = world).
        let mut thinks_fired = 0usize;
        let mut think_errors = 0usize;
        match self.run_sys("StartFrame", 0, 0) {
            Ok(_) => {}
            Err(_) => think_errors += 1, // isolated; the interpreter was reset
        }

        let n = self.vm.num_edicts();
        for e in 0..n {
            let free = self.vm.edict_free.get(e).copied().unwrap_or(true);
            if free {
                continue;
            }
            let ent = e as i32;

            let result = if ent == self.player {
                self.physics_client(ent, cmd, start_time, dt)
            } else {
                let movetype = self.vm.ent_get_float(ent, "movetype") as i32;
                self.process_entity(ent, movetype, start_time, dt)
            };
            match result {
                Ok(fired) => thinks_fired += fired as usize,
                Err(_) => {
                    think_errors += 1;
                    self.vm.reset_execution();
                }
            }
        }

        // SV_WriteClientdataToMessage (sv_main.c) runs SV_SetIdealPitch once per
        // client per frame, after physics: compute the slope-following auto-pitch
        // the QuakeC view code centres toward when you walk up/down stairs.
        if self.player >= 0
            && !self
                .vm
                .edict_free
                .get(self.player as usize)
                .copied()
                .unwrap_or(true)
        {
            self.set_ideal_pitch(self.player);
        }

        // sv.time += host_frametime (end of SV_Physics).
        self.vm.gset_float("time", start_time + dt);

        // A think may have called lightstyle() (e.g. a trigger toggling a light);
        // sync any updates from the write transport into the owned table.
        self.lightstyles = snapshot_lightstyles();

        Ok(FrameReport {
            thinks_fired,
            think_errors,
            time: self.time(),
        })
    }

    /// `SV_Physics_Client` (sv_phys.c ~1059): `PlayerPreThink` -> the movement
    /// path chosen by movetype -> `touch_triggers` -> relink -> `PlayerPostThink`.
    /// Returns whether a think fired (for the frame report). A removed player
    /// (`free`) short-circuits the rest, like the C `SV_RunThink` guards.
    fn physics_client(&mut self, ent: i32, cmd: &UserCmd, start_time: f32, dt: f32) -> Result<bool> {
        // SV_ReadClientMove (sv_user.c) copies the usercmd onto the client edict
        // BEFORE the physics frame: v_angle from the look angles, then the button
        // bits and impulse. We do it here, immediately before PlayerPreThink, so
        // the weapon code that runs inside PreThink/PostThink (W_WeaponFrame ->
        // W_Attack reads `self.button0` and aims off `self.v_angle`) sees the
        // current frame's input. (client_think later re-derives v_angle/angles
        // during the move, but PreThink runs first and must see it set.)
        self.apply_usercmd_to_edict(ent, cmd);

        // call standard client pre-think (self = player)
        self.run_sys("PlayerPreThink", ent, 0)?;
        if self.is_free(ent) {
            return Ok(false);
        }

        // SV_CheckVelocity clamps before the move (the slide clamps implicitly,
        // but mirror the NaN/maxvelocity scrub the C does first).
        self.check_velocity(ent);

        let movetype = self.vm.ent_get_float(ent, "movetype") as i32;
        // Each arm assigns `fired`; the initial value is just to satisfy the
        // borrow checker on the early-return paths.
        #[allow(unused_assignments)]
        let mut fired = false;
        match movetype {
            MOVETYPE_NONE => {
                let (f, alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
            }
            MOVETYPE_WALK => {
                let (f, alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                // SV_ClientThink does friction/acceleration toward wishdir; then
                // gravity (unless in water or water-jumping) and the step-up walk
                // move. check_water sets waterlevel/watertype so the QuakeC
                // WaterMove (PlayerPostThink) can deal lava/slime damage.
                self.client_think(ent, cmd, dt);
                let in_water = self.check_water(ent);
                let flags = self.vm.ent_get_float(ent, "flags") as i32;
                if !in_water && flags & FL_WATERJUMP == 0 {
                    self.add_gravity(ent, dt);
                }
                // SV_CheckStuck: free the player from the clipping hull (and
                // latch `oldorigin`) right before the walk move, as the C does.
                self.check_stuck(ent);
                self.walk_move(ent, start_time, dt);
            }
            MOVETYPE_FLY => {
                let (f, alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                self.client_think(ent, cmd, dt);
                self.check_water(ent); // keep waterlevel/watertype live while flying
                self.player_fly_move(ent, start_time, dt);
            }
            MOVETYPE_NOCLIP => {
                let (f, alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                self.client_think(ent, cmd, dt);
                // origin += frametime * velocity (no clipping).
                let origin = self.vm.ent_get_vector(ent, "origin");
                let vel = self.vm.ent_get_vector(ent, "velocity");
                self.vm
                    .ent_set_vector(ent, "origin", crate::math::mul_add(origin, dt, vel));
            }
            MOVETYPE_TOSS | MOVETYPE_BOUNCE => {
                // SV_Physics_Client `case MOVETYPE_TOSS/BOUNCE: SV_Physics_Toss`:
                // think first; if still alive, gravity + the clipped toss move. A
                // client is MOVETYPE_TOSS exactly while DEAD (client.qc PlayerDie),
                // so this is the corpse physics — the death pop (PlayerDie's
                // `velocity_z += random()*300`) and the fall back to the floor.
                // Previously this fell into the think-only fallback arm and a
                // corpse killed mid-air froze in place.
                let (f, alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                self.physics_toss(ent, movetype, start_time, dt);
            }
            _ => {
                // Any other movetype on a client: think only (no movement).
                let (f, _alive) = self.run_think(ent, start_time, dt)?;
                fired = f;
            }
        }

        if self.is_free(ent) {
            return Ok(fired);
        }

        // After moving, trip triggers so the player can pick up items / fire
        // trigger fields (the C does this inside SV_LinkEdict during the move;
        // here the move's link is bounds-only, so we touch triggers explicitly).
        touch_triggers(&mut self.vm, ent, start_time);
        if self.is_free(ent) {
            return Ok(fired);
        }

        // call standard player post-think (relink first, like SV_Physics_Client).
        // SV_Physics_Client (sv_phys.c:1128) sets pr_global_struct->time = sv.time
        // before PlayerPostThink; without this the global is left at the move's
        // touch time (or a think's clamped thinktime), so PostThink would read a
        // stale `time`. run_sys sets self/other but never time.
        link_edict(&mut self.vm, ent);
        self.vm.gset_float("time", start_time);
        self.run_sys("PlayerPostThink", ent, 0)?;

        // The impulse is a one-shot: a usercmd carries it for a single frame.
        // Stock QuakeC's ImpulseCommands() clears `self.impulse` after handling
        // it; the engine likewise treats it as edge-triggered (SV_ReadClientMove
        // only overwrites it when a fresh non-zero impulse arrives). Clear it
        // here so a held impulse fires once even if the mod's QuakeC forgot to.
        if !self.is_free(ent) {
            self.vm.ent_set_float(ent, "impulse", 0.0);
        }

        Ok(fired)
    }

    /// Enumerate the per-frame entity dynamic-light contributions, porting the
    /// `EF_*` dlight spawns of `CL_RelinkEntities` (`cl_main.c`).
    ///
    /// Scans every in-use edict whose `effects` float field is non-zero and, for
    /// each `EF_MUZZLEFLASH` / `EF_BRIGHTLIGHT` / `EF_DIMLIGHT` bit set, yields an
    /// [`EntityDlight`] describing the light to allocate:
    ///  * `key` = the entity number (so the flash reuses one slot per entity via
    ///    `CL_AllocDlight`),
    ///  * `origin` = the light position (muzzle: `origin.z += 16` then `+ 18 *
    ///    forward(angles)`; brightlight: `origin.z += 16`; dimlight: `origin`),
    ///  * `radius_base` = the radius *before* the `rand()&31` jitter (the caller
    ///    adds it deterministically, keeping this query side-effect-free),
    ///  * `minlight` = the ambient floor (32 for the muzzle flash, else 0),
    ///  * `life` = seconds until the light dies (`die = now + life`).
    ///
    /// The forward vector for the muzzle offset is [`crate::math::angle_vectors`]
    /// (Quake's `AngleVectors`), reusing the same helper the VM `makevectors`
    /// builtin uses. This is a pure query: it never mutates the server, and the
    /// `rand()&31` radius jitter is deliberately left to the caller so the result
    /// is reproducible.
    ///
    /// If one entity has several light bits set, it yields several entries — but
    /// they share the entity's `key`, so `CL_AllocDlight` collapses them into one
    /// slot (the last wins), exactly as the C overwrote the same slot in sequence.
    /// `SV_CleanupEnts` (sv_main.c:557): clear the one-frame `EF_MUZZLEFLASH` bit on
    /// every edict. QuakeC's `W_Attack` sets `self.effects |= EF_MUZZLEFLASH` on each
    /// discharge and relies on the engine clearing it the same frame, so the muzzle
    /// dynamic light lasts exactly one frame. The C clears at the END of the frame
    /// (after the client read the bit); this single-process port clears at the START
    /// of the next frame instead — equivalent, since nothing reads `effects` between
    /// the host's `entity_dlights()` (end of this frame) and the next frame's thinks.
    /// Without this the muzzle light, once lit, tracked the shooter forever.
    fn cleanup_ents(&mut self) {
        let n = self.vm.num_edicts();
        for e in 1..n {
            if self.vm.edict_free.get(e).copied().unwrap_or(true) {
                continue;
            }
            let ei = e as i32;
            let eff = self.vm.ent_get_float(ei, "effects") as i32;
            if eff & EF_MUZZLEFLASH != 0 {
                self.vm
                    .ent_set_float(ei, "effects", (eff & !EF_MUZZLEFLASH) as f32);
            }
        }
    }

    pub fn entity_dlights(&self) -> Vec<EntityDlight> {
        let mut out = Vec::new();
        let n = self.vm.num_edicts();
        for e in 1..n {
            // edict 0 is the world; skip free edicts.
            if self.vm.edict_free.get(e).copied().unwrap_or(true) {
                continue;
            }
            let ent = e as i32;
            let effects = self.vm.ent_get_float(ent, "effects") as i32;
            if effects == 0 {
                continue;
            }
            let origin = self.vm.ent_get_vector(ent, "origin");
            let angles = self.vm.ent_get_vector(ent, "angles");

            if effects & EF_MUZZLEFLASH != 0 {
                let (forward, _r, _u) = angle_vectors(angles);
                let muzzle = [
                    origin[0] + forward[0] * 18.0,
                    origin[1] + forward[1] * 18.0,
                    origin[2] + 16.0 + forward[2] * 18.0,
                ];
                out.push(EntityDlight {
                    key: ent,
                    origin: muzzle,
                    radius_base: 200.0,
                    minlight: 32.0,
                    life: 0.1,
                });
            }
            if effects & EF_BRIGHTLIGHT != 0 {
                out.push(EntityDlight {
                    key: ent,
                    origin: [origin[0], origin[1], origin[2] + 16.0],
                    radius_base: 400.0,
                    minlight: 0.0,
                    life: 0.001,
                });
            }
            if effects & EF_DIMLIGHT != 0 {
                out.push(EntityDlight {
                    key: ent,
                    origin,
                    radius_base: 200.0,
                    minlight: 0.0,
                    life: 0.001,
                });
            }
        }
        out
    }

    /// The player's attack-relevant state for verification: `(button0, weapon,
    /// ammo_shells)`. All zero when no client is connected. `button0` is the
    /// attack bit copied from the last usercmd; `weapon`/`ammo_shells` are the
    /// QuakeC inventory fields the shotgun path reads/decrements.
    pub fn player_attack_state(&self) -> (f32, f32, f32) {
        if self.player < 0 {
            return (0.0, 0.0, 0.0);
        }
        let button0 = self.vm.ent_get_float(self.player, "button0");
        let weapon = self.vm.ent_get_float(self.player, "weapon");
        let ammo_shells = self.vm.ent_get_float(self.player, "ammo_shells");
        (button0, weapon, ammo_shells)
    }

    /// True if edict `e` is free (removed) or out of range.
    fn is_free(&self, e: i32) -> bool {
        if e < 0 {
            return true;
        }
        self.vm.edict_free.get(e as usize).copied().unwrap_or(true)
    }

    /// `SV_FlyMove` (sv_phys.c ~229) against the ENTITY-AWARE [`sv_move`]: slide
    /// the player box along the surfaces it hits over `dt`, sliding along walls
    /// and creases instead of stopping dead. Returns the blocked bitmask
    /// (1 = floor, 2 = wall/step, plus 4-ish dead-stop returns), sets
    /// `FL_ONGROUND` on a floor contact, and runs the touch functions of any
    /// entity it bumps via [`sv_impact`]. `out_steptrace` receives the trace of
    /// the wall hit that triggers stair-stepping.
    fn fly_move_core(
        &mut self,
        ent: i32,
        dt: f32,
        sv_time: f32,
        out_steptrace: &mut Option<MoveTrace>,
    ) -> i32 {
        let num_bumps = 4;
        let mut blocked = 0;
        let original_velocity = self.vm.ent_get_vector(ent, "velocity");
        let primal_velocity = original_velocity;
        let mut original = original_velocity;
        // Clip planes are capped at 5 (the `>= 5` guard below), so a fixed array
        // + count avoids a per-call heap Vec — identical plane set, same order.
        let mut planes: [Vec3; 5] = [[0.0; 3]; 5];
        let mut nplanes = 0usize;
        let mut time_left = dt;

        for _bump in 0..num_bumps {
            let velocity = self.vm.ent_get_vector(ent, "velocity");
            if velocity == [0.0, 0.0, 0.0] {
                break;
            }
            let origin = self.vm.ent_get_vector(ent, "origin");
            let end = [
                origin[0] + time_left * velocity[0],
                origin[1] + time_left * velocity[1],
                origin[2] + time_left * velocity[2],
            ];
            let mins = self.vm.ent_get_vector(ent, "mins");
            let maxs = self.vm.ent_get_vector(ent, "maxs");
            let trace = sv_move(&mut self.vm, origin, end, mins, maxs, ent, false, false);

            if trace.allsolid {
                // entity is trapped in another solid: stop dead.
                self.vm.ent_set_vector(ent, "velocity", [0.0; 3]);
                return 3;
            }

            if trace.fraction > 0.0 {
                // actually covered some distance
                self.vm.ent_set_vector(ent, "origin", trace.endpos);
                original = self.vm.ent_get_vector(ent, "velocity");
                nplanes = 0;
            }

            if trace.fraction == 1.0 {
                break; // moved the entire distance
            }

            if trace.plane_normal[2] > 0.7 {
                blocked |= 1; // floor
                // SV_FlyMove only latches FL_ONGROUND when the contacted floor
                // is a SOLID_BSP edict (`trace.ent->v.solid == SOLID_BSP`). The
                // world (edict 0) is SOLID_BSP and must still count; a
                // SOLID_BBOX/SOLID_SLIDEBOX box (monster/item/player) must NOT
                // become "ground" even when its top faces up.
                let on_bsp = trace.ent == 0
                    || (trace.ent > 0
                        && self.vm.ent_get_float(trace.ent, "solid") as i32 == SOLID_BSP);
                if on_bsp {
                    let flags = self.vm.ent_get_float(ent, "flags") as i32;
                    self.vm
                        .ent_set_float(ent, "flags", (flags | FL_ONGROUND) as f32);
                    self.vm.ent_set_int(ent, "groundentity", trace.ent.max(0));
                }
            }
            if trace.plane_normal[2] == 0.0 {
                blocked |= 2; // step / wall
                *out_steptrace = Some(trace);
            }

            // run the impact function (host present; not inside with_host). C
            // SV_FlyMove calls SV_Impact(ent, trace.ent) for EVERY clipped bump,
            // world or entity — trace.ent is the world edict on a world hit. The
            // loop already broke on fraction==1.0, so here trace.ent is 0 (world) or
            // >0 (entity), never the -1 no-hit sentinel; `>= 0` fires the mover's
            // touch on world contact too (mover with no touch is a no-op).
            if trace.ent >= 0 {
                sv_impact(&mut self.vm, ent, trace.ent, sv_time);
                if self.is_free(ent) {
                    break; // removed by the impact function
                }
            }

            time_left -= time_left * trace.fraction;

            // clipped to another plane
            if nplanes >= 5 {
                // this shouldn't really happen
                self.vm.ent_set_vector(ent, "velocity", [0.0; 3]);
                return 3;
            }
            planes[nplanes] = trace.plane_normal;
            nplanes += 1;

            // modify original_velocity so it parallels all of the clip planes.
            let mut new_velocity = [0.0f32; 3];
            let mut i = 0usize;
            while i < nplanes {
                new_velocity = clip_velocity(original, planes[i], 1.0);
                let mut ok = true;
                let mut j = 0usize;
                while j < nplanes {
                    if j != i && crate::math::dot(new_velocity, planes[j]) < 0.0 {
                        ok = false;
                        break;
                    }
                    j += 1;
                }
                if ok {
                    break;
                }
                i += 1;
            }

            if i != nplanes {
                // go along this plane
                self.vm.ent_set_vector(ent, "velocity", new_velocity);
            } else {
                // go along the crease (two planes)
                if nplanes != 2 {
                    self.vm.ent_set_vector(ent, "velocity", [0.0; 3]);
                    return 7;
                }
                let dir = crate::math::cross(planes[0], planes[1]);
                let cur = self.vm.ent_get_vector(ent, "velocity");
                let d = crate::math::dot(dir, cur);
                self.vm
                    .ent_set_vector(ent, "velocity", crate::math::scale(dir, d));
            }

            // if velocity is against the original velocity, stop dead to avoid
            // tiny oscillations in sloping corners.
            let cur = self.vm.ent_get_vector(ent, "velocity");
            if crate::math::dot(cur, primal_velocity) <= 0.0 {
                self.vm.ent_set_vector(ent, "velocity", [0.0; 3]);
                return blocked;
            }
        }

        blocked
    }

    /// Plain fly move for `MOVETYPE_FLY` clients (no stair step-up), then relink.
    fn player_fly_move(&mut self, ent: i32, sv_time: f32, dt: f32) {
        let mut steptrace = None;
        let _ = self.fly_move_core(ent, dt, sv_time, &mut steptrace);
        link_edict(&mut self.vm, ent);
    }

    /// `SV_WalkMove` (sv_phys.c ~958): a slide move with a stair step-up of up to
    /// [`world::STEPSIZE`] when the flat move is blocked by a wall, so the player
    /// climbs small ledges. Faithful to id's algorithm over the entity-aware
    /// [`Self::fly_move_core`]. Updates `FL_ONGROUND` from the down move and
    /// relinks at the end.
    fn walk_move(&mut self, ent: i32, sv_time: f32, dt: f32) {
        // do a regular slide move unless it looks like you ran into a step.
        let oldonground = (self.vm.ent_get_float(ent, "flags") as i32) & FL_ONGROUND != 0;
        // Clear ONGROUND; fly_move / the down move below will re-set it.
        let flags0 = self.vm.ent_get_float(ent, "flags") as i32;
        self.vm
            .ent_set_float(ent, "flags", (flags0 & !FL_ONGROUND) as f32);

        let oldorg = self.vm.ent_get_vector(ent, "origin");
        let oldvel = self.vm.ent_get_vector(ent, "velocity");

        let mut steptrace: Option<MoveTrace> = None;
        let clip = self.fly_move_core(ent, dt, sv_time, &mut steptrace);

        if clip & 2 == 0 {
            // move didn't block on a step.
            link_edict(&mut self.vm, ent);
            return;
        }
        // don't stair up while jumping — UNLESS swimming. The C gate is
        // `if (!oldonground && ent->v.waterlevel == 0) return;`, so a player in
        // water (waterlevel > 0) can still step up onto a ledge even mid-air.
        let waterlevel = self.vm.ent_get_float(ent, "waterlevel") as i32;
        if !oldonground && waterlevel == 0 {
            link_edict(&mut self.vm, ent);
            return;
        }
        if self.vm.ent_get_float(ent, "movetype") as i32 != MOVETYPE_WALK {
            link_edict(&mut self.vm, ent); // gibbed by a trigger
            return;
        }
        if (self.vm.ent_get_float(ent, "flags") as i32) & FL_WATERJUMP != 0 {
            link_edict(&mut self.vm, ent);
            return;
        }

        // remember the no-step result.
        let nosteporg = self.vm.ent_get_vector(ent, "origin");
        let nostepvel = self.vm.ent_get_vector(ent, "velocity");

        // try moving up and forward to go up a step.
        self.vm.ent_set_vector(ent, "origin", oldorg); // back to start pos

        // move up
        let upmove = [0.0, 0.0, world::STEPSIZE];
        self.push_entity(ent, upmove, sv_time);

        // move forward (no vertical wish in velocity).
        self.vm
            .ent_set_vector(ent, "velocity", [oldvel[0], oldvel[1], 0.0]);
        let mut steptrace2 = None;
        let mut clip2 = self.fly_move_core(ent, dt, sv_time, &mut steptrace2);

        // Stuck check (sv_phys.c SV_WalkMove ~1015): if the step-up forward move
        // made essentially no horizontal progress (< 1/32 unit on BOTH axes) but
        // still blocked, the player is wedged at a BSP hull angle-join — try the
        // SV_TryUnstick nudge dance to free them, adopting its resulting clip.
        if clip2 != 0 {
            let neworg = self.vm.ent_get_vector(ent, "origin");
            if (oldorg[0] - neworg[0]).abs() < 0.03125 && (oldorg[1] - neworg[1]).abs() < 0.03125 {
                clip2 = self.sv_try_unstick(ent, oldvel, sv_time);
            }
        }

        // Extra friction based on view angle (sv_phys.c ~1027): when the (possibly
        // unstick-updated) forward move blocked on a wall, SV_WallFriction bleeds the
        // tangential velocity using the wall normal from the forward move's trace.
        if clip2 & 2 != 0 {
            if let Some(tr) = &steptrace2 {
                let normal = tr.plane_normal;
                self.sv_wall_friction(ent, normal);
            }
        }

        // move down by STEPSIZE - the vertical the original move would have done.
        let downmove = [0.0, 0.0, -world::STEPSIZE + oldvel[2] * dt];
        let downtrace = self.push_entity(ent, downmove, sv_time);

        if downtrace.plane_normal[2] > 0.7 {
            // Landed on a walkable floor: keep the stepped result. The C
            // (sv_phys.c SV_WalkMove ~390) only latches FL_ONGROUND /
            // groundentity HERE when the mover is a brush model
            // (`ent->v.solid == SOLID_BSP`). A player (SOLID_SLIDEBOX) keeps the
            // stepped origin but does NOT latch ground in the step-down branch —
            // it already got FL_ONGROUND from the regular slide move
            // (`fly_move_core` / SV_FlyMove, which gates on the contacted floor
            // being SOLID_BSP). Unconditionally setting it here let players latch
            // ground onto a step they only grazed; the gate restores the C.
            if self.vm.ent_get_float(ent, "solid") as i32 == SOLID_BSP {
                let flags = self.vm.ent_get_float(ent, "flags") as i32;
                self.vm
                    .ent_set_float(ent, "flags", (flags | FL_ONGROUND) as f32);
                // groundentity = EDICT_TO_PROG(downtrace.ent): the edict we
                // stepped down onto (0 = world, >0 = a plat/door). `downtrace.ent`
                // is `-1` only when the down-push was clear, but plane_normal[2] >
                // 0.7 implies a floor contact, so clamp the sentinel to world (0).
                self.vm.ent_set_int(ent, "groundentity", downtrace.ent.max(0));
            }
        } else {
            // the push down didn't reach good ground: use the no-step move.
            self.vm.ent_set_vector(ent, "origin", nosteporg);
            self.vm.ent_set_vector(ent, "velocity", nostepvel);
        }

        link_edict(&mut self.vm, ent);
    }

    /// `SV_WallFriction` (sv_phys.c ~867): when the player walks into a wall while
    /// facing toward it, bleed off the tangential velocity. `d = dot(normal,
    /// forward(v_angle)) + 0.5`; if `d < 0` the into-wall component is removed and
    /// the side component is scaled by `(1+d)`, so head-on contact loses the most
    /// speed. Only X/Y are scaled (Z is left to gravity/step logic). Uses the
    /// player's VIEW angles (`v_angle`), not the body `angles`.
    fn sv_wall_friction(&mut self, ent: i32, normal: Vec3) {
        let v_angle = self.vm.ent_get_vector(ent, "v_angle");
        let (forward, _right, _up) = angle_vectors(v_angle);
        let d = crate::math::dot(normal, forward) + 0.5;
        if d >= 0.0 {
            return;
        }
        let vel = self.vm.ent_get_vector(ent, "velocity");
        let i = crate::math::dot(normal, vel);
        let into = [normal[0] * i, normal[1] * i, normal[2] * i];
        let side = [vel[0] - into[0], vel[1] - into[1], vel[2] - into[2]];
        self.vm
            .ent_set_vector(ent, "velocity", [side[0] * (1.0 + d), side[1] * (1.0 + d), vel[2]]);
    }

    /// `SV_TryUnstick` (sv_phys.c ~901): the player is wedged at a BSP hull
    /// angle-join where float precision pins the step-up move. Nudge the player 2
    /// units in each of 8 axial/diagonal directions, retry the original horizontal
    /// move, and accept the first direction that frees > 4 units of progress on X or
    /// Y; otherwise restore the position and try the next. If none work, zero the
    /// velocity ("don't stick") and report a full block (7).
    fn sv_try_unstick(&mut self, ent: i32, oldvel: Vec3, sv_time: f32) -> i32 {
        let oldorg = self.vm.ent_get_vector(ent, "origin");
        const DIRS: [[f32; 3]; 8] = [
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [-2.0, 0.0, 0.0],
            [0.0, -2.0, 0.0],
            [2.0, 2.0, 0.0],
            [-2.0, 2.0, 0.0],
            [2.0, -2.0, 0.0],
            [-2.0, -2.0, 0.0],
        ];
        for dir in DIRS {
            // try pushing a little in an axial direction (from the stuck origin).
            self.push_entity(ent, dir, sv_time);
            // retry the original move (horizontal only).
            self.vm
                .ent_set_vector(ent, "velocity", [oldvel[0], oldvel[1], 0.0]);
            let mut steptrace = None;
            let clip = self.fly_move_core(ent, 0.1, sv_time, &mut steptrace);
            let neworg = self.vm.ent_get_vector(ent, "origin");
            if (oldorg[1] - neworg[1]).abs() > 4.0 || (oldorg[0] - neworg[0]).abs() > 4.0 {
                return clip; // freed
            }
            // go back to the original (stuck) pos and try the next direction.
            self.vm.ent_set_vector(ent, "origin", oldorg);
        }
        self.vm.ent_set_vector(ent, "velocity", [0.0, 0.0, 0.0]); // don't stick
        7 // still not moving
    }
}

/// `PF_checkclient` (#17): `entity() checkclient` — return a client visible to
/// `self`, used by `FindTarget` to wake monsters.
///
/// SIMPLIFICATION (documented): the C `PF_checkclient` cached `sv.lastcheck`,
/// re-picked the candidate client only every 0.1s (`PF_newcheckclient`), and
/// tested PVS bits before tracing. With a single, always-present player and no
/// PVS subsystem, we test line of sight directly: a world-only traceline from
/// `self`'s eyes (`origin + view_ofs`) to the player's eyes. If it is
/// unobstructed (`fraction == 1`, or it hit only the player), return the player
/// edict; otherwise return the world (0), matching the C's "can't see -> world".
/// The per-frame caching is dropped (it was a CPU optimization, not a behaviour
/// change); the visibility result is identical for one client.
fn bi_checkclient(vm: &mut Vm) -> Result<()> {
    let self_e = vm.gget_int("self");

    // Find a live client edict by its FL_CLIENT flag (the C scanned svs.clients;
    // real progs.dat has no `viewentity` global, so we can't rely on that). A
    // dead client (health <= 0) is not a valid target, matching the C.
    let mut player = 0i32;
    for e in 1..vm.num_edicts() {
        let ent = e as i32;
        if vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        if (vm.ent_get_float(ent, "flags") as i32) & FL_CLIENT != 0
            && vm.ent_get_float(ent, "health") > 0.0
        {
            player = ent;
            break;
        }
    }
    if player <= 0 {
        vm.ret_entity(0);
        return Ok(());
    }

    // Eyes: origin + view_ofs for both ends of the sight line.
    let self_org = vm.ent_get_vector(self_e, "origin");
    let self_ofs = vm.ent_get_vector(self_e, "view_ofs");
    let view = v_add(self_org, self_ofs);

    let pl_org = vm.ent_get_vector(player, "origin");
    let pl_ofs = vm.ent_get_vector(player, "view_ofs");
    let target = v_add(pl_org, pl_ofs);

    // World-only line of sight (MOVE_NOMONSTERS, matching C PF_checkclient):
    // ignore the monster itself and skip every box entity, so the trace clips
    // only the world + SOLID_BSP bmodels. A clear trace (fraction == 1) means
    // the player is visible.
    let tr = sv_move(vm, view, target, [0.0; 3], [0.0; 3], self_e, true, false);
    let visible = tr.fraction == 1.0 || tr.ent == player;

    vm.ret_entity(if visible { player } else { 0 });
    Ok(())
}

/// `PF_findradius` (#22): `entity(vector org, float rad) findradius` — return a
/// `chain` of every non-free, blocking (`solid != SOLID_NOT`) edict whose box
/// centre is within `rad` of `org`.
///
/// Each found edict's `chain` field points at the previous one (the head is
/// returned, the tail terminated by the world edict 0), exactly as the C built
/// the linked list. Used by explosion radius damage and some triggers. The
/// distance is measured from `org` to the entity's box centre
/// (`origin + (mins+maxs)/2`), matching the C.
fn bi_findradius(vm: &mut Vm) -> Result<()> {
    let org = vm.arg_vector(0);
    let rad = vm.arg_float(1);

    // chain starts at the world (0): the terminator of the list.
    let mut chain = 0i32;
    let n = vm.num_edicts();
    // Scan edicts 1..num_edicts (the C started at NEXT_EDICT(sv.edicts)).
    for e in 1..n {
        let ei = e as i32;
        if vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        if vm.ent_get_float(ei, "solid") as i32 == SOLID_NOT {
            continue;
        }
        let origin = vm.ent_get_vector(ei, "origin");
        let mins = vm.ent_get_vector(ei, "mins");
        let maxs = vm.ent_get_vector(ei, "maxs");
        // eorg = org - (origin + (mins+maxs)/2): distance from the box centre.
        let eorg: Vec3 = [
            org[0] - (origin[0] + (mins[0] + maxs[0]) * 0.5),
            org[1] - (origin[1] + (mins[1] + maxs[1]) * 0.5),
            org[2] - (origin[2] + (mins[2] + maxs[2]) * 0.5),
        ];
        if crate::math::length(eorg) > rad {
            continue;
        }
        // Link: this edict's chain points at the previous head; it becomes head.
        vm.ent_set_int(ei, "chain", chain);
        chain = ei;
    }

    vm.ret_entity(chain);
    Ok(())
}

/// Standalone `ClipVelocity` (so the physics methods can call it without
/// borrowing `self`). `STOP_EPSILON = 0.1` matches the C.
fn clip_velocity(vel: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    const STOP_EPSILON: f32 = 0.1;
    let backoff = crate::math::dot(vel, normal) * overbounce;
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        let change = normal[i] * backoff;
        out[i] = vel[i] - change;
        if out[i] > -STOP_EPSILON && out[i] < STOP_EPSILON {
            out[i] = 0.0;
        }
    }
    out
}

/// Build a [`QError`] for an unexpected server condition. (Currently unused on
/// the happy path; kept so callers can surface a structured error if needed.)
#[allow(dead_code)]
fn server_error(msg: impl Into<String>) -> QError {
    QError::invalid(msg.into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::testutil::*;
    use crate::progs::{Op, Statement, OFS_PARM0, OFS_RETURN};

    // The explosive-box fix: external brush models (the `maps/b_*.bsp` item
    // boxes) must resolve real collision bounds at setmodel time, or a hitscan
    // passes straight through the box — it renders but can't be shot. The bug
    // was `model_bbox` returning `None` for external models (zero box). This
    // locks the lookup `setmodel` depends on.
    #[test]
    fn external_brush_model_bounds_feed_setmodel() {
        use crate::bsp::DModel;

        // No pak -> external b_*.bsp keeps the historical zero-box fallback
        // (None). Data-free tests and the pure-disk paths behave as before.
        let wm = WorldModel::with_pak(empty_bsp(), None);
        assert_eq!(wm.model_bbox("maps/b_explob.bsp"), None);

        // The "*N" world-submodel path is unchanged (reads the BSP model bounds).
        let mut bsp = empty_bsp();
        bsp.models.push(DModel {
            mins: [-16.0, -16.0, -24.0],
            maxs: [16.0, 16.0, 32.0],
            origin: [0.0; 3],
            headnode: [0; crate::bsp::MAX_MAP_HULLS],
            visleafs: 0,
            firstface: 0,
            numfaces: 0,
        });
        let wm2 = WorldModel::with_pak(bsp, None);
        assert_eq!(wm2.model_bbox("*0"), Some(([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0])));

        // Once `precache_model` has resolved the box's MODEL-0 bounds from the
        // pak (populated directly here, since this unit test is data-free),
        // `setmodel`'s `model_bbox` returns them — the box gets a real bbox
        // (e.g. b_explob.bsp -> (0,0,0)..(32,32,64)) instead of (0,0,0).
        let mut wm3 = WorldModel::with_pak(empty_bsp(), None);
        wm3.external_bounds
            .insert("maps/b_explob.bsp".into(), ([0.0, 0.0, 0.0], [32.0, 32.0, 64.0]));
        assert_eq!(
            wm3.model_bbox("maps/b_explob.bsp"),
            Some(([0.0, 0.0, 0.0], [32.0, 32.0, 64.0]))
        );
    }

    // ------------------------------------------------------------- run_frame

    #[test]
    fn run_frame_fires_due_thinks() {
        // Build a progs with a think function that sets a flag, and an entity
        // (movetype NONE) whose nextthink is due.
        let mut b = Builder::new();
        b.entityfields = 8;
        let g_flag = 30u16;
        b.add_global("spawned_flag", EV_FLOAT, g_flag);
        b.add_global("self", 4, 31);
        b.add_global("other", 4, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("frametime", EV_FLOAT, 35);
        let g_one = 40u16;

        b.add_field("classname", EV_STRING, 1);
        b.add_field("spawnflags", EV_FLOAT, 2);
        b.add_field("think", EV_FUNCTION, 3);
        b.add_field("nextthink", EV_FLOAT, 4);
        b.add_field("movetype", EV_FLOAT, 5);

        let think_fn = b.add_function(
            "do_think",
            vec![
                Statement {
                    op: Op::StoreF as u16,
                    a: g_one as i16,
                    b: g_flag as i16,
                    c: 0,
                },
                Statement {
                    op: Op::Done as u16,
                    a: 0,
                    b: 0,
                    c: 0,
                },
            ],
        );

        let img = b.build();
        let progs = Progs::parse(&img).expect("parse");
        let bsp = empty_bsp();
        let mut server = Server::new(bsp, progs).expect("server");
        server.vm.set_gf(usize::from(g_one), 1.0);

        // Spawn an entity, set movetype NONE, think=do_think, nextthink in past.
        let e = server.vm.spawn();
        server.vm.ent_set_float(e, "movetype", MOVETYPE_NONE as f32);
        server.vm.ent_set_int(e, "think", think_fn as i32);
        server.vm.ent_set_float(e, "nextthink", 0.5); // <= time(1.0)+dt

        let before = server.time();
        let report = server.run_frame(0.1).expect("frame");

        assert_eq!(report.thinks_fired, 1, "the due think fired");
        assert_eq!(server.vm.gget_float("spawned_flag"), 1.0);
        // time advanced by dt.
        assert!((report.time - (before + 0.1)).abs() < 1e-6);
        // nextthink was consumed (set to 0).
        assert_eq!(server.vm.ent_get_float(e, "nextthink"), 0.0);
    }

    #[test]
    fn run_frame_toss_adds_gravity_and_moves() {
        // A MOVETYPE_TOSS entity with no due think falls under gravity. The empty
        // world traces as blocked at fraction 0 (headnode out of range -> solid),
        // so origin won't move, but velocity must gain downward speed.
        let mut b = Builder::new();
        b.entityfields = 16;
        b.add_global("self", 4, 31);
        b.add_global("other", 4, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_field("classname", EV_STRING, 1);
        b.add_field("movetype", EV_FLOAT, 2);
        b.add_field("nextthink", EV_FLOAT, 3);
        b.add_field("flags", EV_FLOAT, 4);
        b.add_field("velocity", 3 /*vector*/, 5); // 5,6,7
        b.add_field("origin", 3, 8); // 8,9,10
        b.add_field("mins", 3, 11);
        b.add_field("maxs", 3, 14);

        let img = b.build();
        let progs = Progs::parse(&img).expect("parse");
        let bsp = empty_bsp();
        let mut server = Server::new(bsp, progs).expect("server");

        let e = server.vm.spawn();
        server.vm.ent_set_float(e, "movetype", MOVETYPE_TOSS as f32);
        server.vm.ent_set_float(e, "nextthink", 0.0); // no think
        server.vm.ent_set_float(e, "flags", 0.0); // not on ground
        server.vm.ent_set_vector(e, "velocity", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "origin", [0.0, 0.0, 100.0]);
        // tiny point box so trace uses hull 0.
        server.vm.ent_set_vector(e, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "maxs", [0.0, 0.0, 0.0]);

        server.run_frame(0.1).expect("frame");

        // velocity.z should be negative (gravity pulled it down): -1*800*0.1 = -80.
        let vel = server.vm.ent_get_vector(e, "velocity");
        assert!(vel[2] < 0.0, "gravity should make velocity.z negative, got {vel:?}");
        assert!((vel[2] - (-80.0)).abs() < 1e-3, "expected -80, got {}", vel[2]);
    }

    // ---------------------------------------------------- skill + water tests

    /// A copy of [`world_open_bsp`] whose open leaf is `CONTENTS_WATER`, so
    /// `point_contents(anywhere)` returns water. Used to exercise
    /// `SV_CheckWaterTransition` (the entity reports it is submerged).
    fn water_world_bsp() -> Bsp {
        use crate::bsp::{CONTENTS_WATER, DLeaf};
        let mut b = world_open_bsp();
        // leaf 1 (the side both node children point at) becomes water.
        b.leafs[1] = DLeaf {
            contents: CONTENTS_WATER,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        };
        b
    }

    /// A progs with the fields the toss/step physics touch by name, including
    /// `watertype`/`waterlevel` so `SV_CheckWaterTransition` can write them.
    fn step_physics_progs() -> Vec<u8> {
        let mut b = Builder::new();
        b.entityfields = 40;
        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);

        b.add_field("classname", EV_STRING, 1);
        b.add_field("movetype", EV_FLOAT, 2);
        b.add_field("nextthink", EV_FLOAT, 3);
        b.add_field("flags", EV_FLOAT, 4);
        b.add_field("velocity", EV_VECTOR, 5); // 5,6,7
        b.add_field("origin", EV_VECTOR, 8); // 8,9,10
        b.add_field("mins", EV_VECTOR, 11); // 11,12,13
        b.add_field("maxs", EV_VECTOR, 14); // 14,15,16
        b.add_field("angles", EV_VECTOR, 17); // 17,18,19
        b.add_field("avelocity", EV_VECTOR, 20); // 20,21,22
        b.add_field("watertype", EV_FLOAT, 23);
        b.add_field("waterlevel", EV_FLOAT, 24);
        b.add_field("solid", EV_FLOAT, 25);
        b.add_field("groundentity", EV_ENTITY, 26);
        b.build()
    }

    #[test]
    fn cvar_set_skill_round_trips_through_cvar() {
        // FIX-1: cvar_set("skill", N) must update a real, readable skill value
        // (clamped 0..3, rounded like SV_SpawnServer), and cvar("skill") reads it
        // back. The old stub made cvar always return 1 and cvar_set a no-op.
        let (img, _marker, _g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(bsp_with_entities("{ }"), progs).expect("server");

        // Constructor resets skill to the medium default.
        assert_eq!(server.skill(), 1, "fresh server defaults to skill 1");

        // Drive the engine cvar_set builtin directly (#72): cvar_set("skill","2").
        let name = server.vm.intern("skill");
        let val = server.vm.intern("2");
        server.vm.argc = 2;
        server.vm.set_gi(crate::progs::OFS_PARM0, name);
        server.vm.set_gi(crate::progs::OFS_PARM1, val);
        bi_cvar_set(&mut server.vm).expect("cvar_set");
        assert_eq!(server.skill(), 2, "cvar_set('skill','2') stored 2");

        // cvar("skill") reads the live value back.
        server.vm.argc = 1;
        let name = server.vm.intern("skill");
        server.vm.set_gi(crate::progs::OFS_PARM0, name);
        bi_cvar(&mut server.vm).expect("cvar");
        assert_eq!(
            server.vm.gf(crate::progs::OFS_RETURN),
            2.0,
            "cvar('skill') returns the live 2, not the old constant 1"
        );

        // Out-of-range is clamped (nightmare cap at 3); a non-integer rounds.
        bi_cvar_set_via(&mut server, "skill", "9");
        assert_eq!(server.skill(), 3, "skill clamps to 3");
        bi_cvar_set_via(&mut server, "skill", "-4");
        assert_eq!(server.skill(), 0, "skill clamps to 0");
        bi_cvar_set_via(&mut server, "skill", "1.6");
        assert_eq!(server.skill(), 2, "1.6 -> (int)(1.6+0.5) = 2");

        // A non-skill cvar_set is a benign no-op (does not touch skill).
        bi_cvar_set_via(&mut server, "fraglimit", "20");
        assert_eq!(server.skill(), 2, "setting another cvar leaves skill alone");
    }

    /// Helper: invoke the engine `cvar_set` builtin with two string args.
    fn bi_cvar_set_via(server: &mut Server, var: &str, val: &str) {
        let n = server.vm.intern(var);
        let v = server.vm.intern(val);
        server.vm.argc = 2;
        server.vm.set_gi(crate::progs::OFS_PARM0, n);
        server.vm.set_gi(crate::progs::OFS_PARM1, v);
        bi_cvar_set(&mut server.vm).expect("cvar_set");
    }

    #[test]
    fn water_transition_sets_watertype_and_splashes() {
        // FIX-3: a stepped entity that crosses from air into water gets its
        // watertype/waterlevel set and plays the misc/h2ohit1.wav splash.
        let progs = Progs::parse(&step_physics_progs()).expect("parse");
        let mut server = Server::new(water_world_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.ent_set_float(e, "movetype", MOVETYPE_STEP as f32);
        server.vm.ent_set_float(e, "nextthink", 0.0); // no think
        // ON_GROUND so the freefall block is skipped but CheckWaterTransition
        // still runs unconditionally at the end of SV_Physics_Step.
        server.vm.ent_set_float(e, "flags", FL_ONGROUND as f32);
        server.vm.ent_set_vector(e, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "maxs", [0.0, 0.0, 0.0]);
        // Pretend it was previously in AIR so this frame is an air->water crossing
        // (watertype != 0 avoids the silent "just spawned" path).
        server.vm.ent_set_float(e, "watertype", CONTENTS_EMPTY as f32);
        server.vm.ent_set_float(e, "waterlevel", 0.0);
        let _ = server.drain_sounds(); // clear any startup queue

        server.run_frame(0.1).expect("frame");

        // watertype is now the liquid contents and waterlevel == 1.
        assert_eq!(
            server.vm.ent_get_float(e, "watertype") as i32,
            crate::bsp::CONTENTS_WATER,
            "watertype updated to the water contents"
        );
        assert_eq!(
            server.vm.ent_get_float(e, "waterlevel"),
            1.0,
            "waterlevel set to 1 on entry"
        );
        // The air->water crossing queued the splash.
        let sounds = server.drain_sounds();
        assert!(
            sounds.iter().any(|s| s.sample == "misc/h2ohit1.wav"),
            "entering water plays misc/h2ohit1.wav, got {sounds:?}"
        );
    }

    #[test]
    fn water_transition_just_spawned_is_silent() {
        // The "just spawned here" path (watertype == 0) adopts the current
        // contents with waterlevel 1 and NO sound — faithful to the C early-out.
        let progs = Progs::parse(&step_physics_progs()).expect("parse");
        let mut server = Server::new(water_world_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.ent_set_float(e, "movetype", MOVETYPE_STEP as f32);
        server.vm.ent_set_float(e, "flags", FL_ONGROUND as f32);
        server.vm.ent_set_vector(e, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "maxs", [0.0, 0.0, 0.0]);
        server.vm.ent_set_float(e, "watertype", 0.0); // never set -> just spawned
        let _ = server.drain_sounds();

        server.run_frame(0.1).expect("frame");

        assert_eq!(server.vm.ent_get_float(e, "waterlevel"), 1.0);
        assert_eq!(
            server.vm.ent_get_float(e, "watertype") as i32,
            crate::bsp::CONTENTS_WATER
        );
        assert!(
            server.drain_sounds().is_empty(),
            "the just-spawned water adoption must be silent"
        );
    }

    #[test]
    fn toss_rest_records_groundentity_landed_on() {
        // FIX-6: a MOVETYPE_TOSS entity that comes to rest sets groundentity to
        // the bmodel it landed on (trace.ent), not a hardcoded world. Here it
        // lands on a SOLID_BSP platform edict, so groundentity must be that edict.
        let progs = Progs::parse(&step_physics_progs()).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        // A solid bmodel platform at the floor.
        let plat = server.vm.spawn();
        server.vm.ent_set_float(plat, "solid", SOLID_BSP as f32);
        server.vm.ent_set_float(plat, "movetype", MOVETYPE_PUSH as f32);
        server.vm.ent_set_vector(plat, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(plat, "mins", [-64.0, -64.0, -8.0]);
        server.vm.ent_set_vector(plat, "maxs", [64.0, 64.0, 0.0]);
        link_edict(&mut server.vm, plat);

        // A grenade-like toss entity just above the platform, falling.
        let g = server.vm.spawn();
        server.vm.ent_set_float(g, "movetype", MOVETYPE_TOSS as f32);
        server.vm.ent_set_float(g, "flags", 0.0); // airborne
        server.vm.ent_set_vector(g, "origin", [0.0, 0.0, 4.0]);
        server.vm.ent_set_vector(g, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(g, "maxs", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(g, "velocity", [0.0, 0.0, -50.0]);
        // groundentity starts as world (0); the rest path must overwrite it.
        server.vm.ent_set_int(g, "groundentity", 0);
        link_edict(&mut server.vm, g);

        server.run_frame(0.1).expect("frame");

        // It should have come to rest on the platform (FL_ONGROUND) and recorded
        // the platform edict as its groundentity.
        let on_ground = (server.vm.ent_get_float(g, "flags") as i32) & FL_ONGROUND != 0;
        if on_ground {
            assert_eq!(
                server.vm.ent_get_int(g, "groundentity"),
                plat,
                "toss-rest groundentity is the platform it landed on"
            );
        }
    }

    // -------------------------------------------------- MOVETYPE_PUSH physics

    /// Field/global layout for the `MOVETYPE_PUSH` tests: everything the pusher
    /// physics reads/writes by name, including `ltime` (the bmodel's local time)
    /// and `groundentity` (so a rider can be tied to its pusher). `think`/
    /// `blocked` are present so the engine can find them, but the tests leave them
    /// null so no QuakeC runs.
    fn pusher_progs() -> Vec<u8> {
        let mut b = Builder::new();
        b.entityfields = 40;

        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);

        b.add_field("classname", EV_STRING, 1);
        b.add_field("solid", EV_FLOAT, 2);
        b.add_field("origin", EV_VECTOR, 4); // 4,5,6
        b.add_field("mins", EV_VECTOR, 7); // 7,8,9
        b.add_field("maxs", EV_VECTOR, 10); // 10,11,12
        b.add_field("absmin", EV_VECTOR, 13); // 13,14,15
        b.add_field("absmax", EV_VECTOR, 16); // 16,17,18
        b.add_field("model", EV_STRING, 19);
        b.add_field("movetype", EV_FLOAT, 20);
        b.add_field("nextthink", EV_FLOAT, 21);
        b.add_field("flags", EV_FLOAT, 22);
        b.add_field("velocity", EV_VECTOR, 23); // 23,24,25
        b.add_field("size", EV_VECTOR, 26); // 26,27,28
        b.add_field("groundentity", EV_ENTITY, 29);
        b.add_field("ltime", EV_FLOAT, 30);
        b.add_field("think", EV_FUNCTION, 31);
        b.add_field("blocked", EV_FUNCTION, 32);

        b.build()
    }

    /// `SV_Physics_Pusher`/`SV_PushMove`: a `MOVETYPE_PUSH` bmodel given a
    /// constant velocity and a future `nextthink` translates its origin by
    /// `velocity * dt` over a frame, and its local time `ltime` advances by `dt`.
    /// Uses the open world so `push_test_position` never reports the pusher stuck.
    #[test]
    fn run_frame_pusher_moves_by_velocity_and_advances_ltime() {
        let progs = Progs::parse(&pusher_progs()).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let p = server.vm.spawn();
        server.vm.ent_set_float(p, "movetype", MOVETYPE_PUSH as f32);
        server.vm.ent_set_float(p, "solid", SOLID_BSP as f32);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(p, "velocity", [10.0, 0.0, 0.0]);
        server.vm.ent_set_float(p, "ltime", 0.0);
        // nextthink in the future so movetime = dt (not clamped) and no think fires.
        server.vm.ent_set_float(p, "nextthink", 100.0);
        link_edict(&mut server.vm, p);

        let dt = 0.1;
        let report = server.run_frame(dt).expect("frame");
        assert_eq!(report.thinks_fired, 0, "future think must not fire");

        let after = server.vm.ent_get_vector(p, "origin");
        assert!(
            (after[0] - 1.0).abs() < 1e-5,
            "pusher origin x moved by velocity*dt (10*0.1=1.0), got {}",
            after[0]
        );
        assert!(after[1].abs() < 1e-5 && after[2].abs() < 1e-5);
        // ltime advanced by dt (SV_PushMove advances it when not blocked).
        let ltime = server.vm.ent_get_float(p, "ltime");
        assert!((ltime - dt).abs() < 1e-6, "ltime advanced by dt, got {ltime}");
    }

    /// A rider standing on the pusher (`FL_ONGROUND`, `groundentity == pusher`)
    /// is carried by the same delta as the pusher.
    #[test]
    fn run_frame_pusher_carries_rider() {
        let progs = Progs::parse(&pusher_progs()).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let p = server.vm.spawn();
        server.vm.ent_set_float(p, "movetype", MOVETYPE_PUSH as f32);
        server.vm.ent_set_float(p, "solid", SOLID_BSP as f32);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(p, "mins", [-64.0, -64.0, -16.0]);
        server.vm.ent_set_vector(p, "maxs", [64.0, 64.0, 16.0]);
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 10.0]);
        server.vm.ent_set_float(p, "ltime", 0.0);
        server.vm.ent_set_float(p, "nextthink", 100.0);
        link_edict(&mut server.vm, p);

        // Rider resting on top of the pusher: a small bbox, onground, ground=pusher.
        // Use MOVETYPE_WALK so the C keeps its FL_ONGROUND through the push (the
        // `movetype != MOVETYPE_WALK` guard) and it runs no gravity of its own this
        // frame, isolating the carry delta.
        let r = server.vm.spawn();
        server.vm.ent_set_float(r, "movetype", MOVETYPE_WALK as f32);
        server.vm.ent_set_float(r, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(r, "origin", [0.0, 0.0, 32.0]);
        server.vm.ent_set_vector(r, "mins", [-8.0, -8.0, -8.0]);
        server.vm.ent_set_vector(r, "maxs", [8.0, 8.0, 8.0]);
        server.vm.ent_set_float(r, "flags", FL_ONGROUND as f32);
        server.vm.ent_set_int(r, "groundentity", p);
        link_edict(&mut server.vm, r);

        let dt = 0.1;
        let _ = server.run_frame(dt).expect("frame");

        // Both moved up by velocity*dt = 1.0.
        let pafter = server.vm.ent_get_vector(p, "origin");
        let rafter = server.vm.ent_get_vector(r, "origin");
        assert!((pafter[2] - 1.0).abs() < 1e-5, "pusher z moved 1.0");
        assert!(
            (rafter[2] - 33.0).abs() < 1e-5,
            "rider carried the same delta (32+1.0), got {}",
            rafter[2]
        );
    }

    /// A zero-velocity pusher only advances `ltime`; its origin does not change.
    #[test]
    fn run_frame_pusher_zero_velocity_only_advances_ltime() {
        let progs = Progs::parse(&pusher_progs()).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let p = server.vm.spawn();
        server.vm.ent_set_float(p, "movetype", MOVETYPE_PUSH as f32);
        server.vm.ent_set_float(p, "solid", SOLID_BSP as f32);
        server.vm.ent_set_vector(p, "origin", [5.0, 6.0, 7.0]);
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
        server.vm.ent_set_float(p, "ltime", 0.0);
        server.vm.ent_set_float(p, "nextthink", 100.0);
        link_edict(&mut server.vm, p);

        let dt = 0.1;
        let _ = server.run_frame(dt).expect("frame");

        let after = server.vm.ent_get_vector(p, "origin");
        assert_eq!(after, [5.0, 6.0, 7.0], "zero-velocity pusher did not move");
        let ltime = server.vm.ent_get_float(p, "ltime");
        assert!(
            (ltime - dt).abs() < 1e-6,
            "ltime still advances by dt for a zero-velocity pusher, got {ltime}"
        );
    }

    // -------------------------------------------------------- world / builtins

    #[test]
    fn worldmodel_precache_dedup_and_index() {
        let mut w = WorldModel::new(empty_bsp());
        // index 0 is "", index 1 is "*0" (world model from new()).
        assert_eq!(w.model_names()[0], "");
        assert_eq!(w.model_names()[1], "*0");

        let a = w.precache_model("progs/player.mdl");
        let b = w.precache_model("progs/player.mdl"); // dedup
        assert_eq!(a, b, "same name returns same index");
        assert_eq!(a, 2, "first new model after world is index 2");

        let s1 = w.precache_sound("weapons/rocket.wav");
        assert_eq!(s1, 1, "first sound is index 1 (slot 0 is empty)");
    }

    #[test]
    fn install_engine_builtins_overwrites_world_keeps_pure() {
        // Build a trivial progs just to get a Vm.
        let mut b = Builder::new();
        let _ = b.add_function(
            "main",
            vec![Statement {
                op: Op::Done as u16,
                a: 0,
                b: 0,
                c: 0,
            }],
        );
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let len_before = vm.builtins.len();
        install_engine_builtins(&mut vm);
        // Table length is unchanged (we only overwrite slots).
        assert_eq!(vm.builtins.len(), len_before);
        // #45 (cvar) now returns sv_gravity default for "sv_gravity".
        vm.set_host(Box::new(WorldModel::new(empty_bsp())));
        let s = vm.intern("sv_gravity");
        vm.set_gi(crate::progs::OFS_PARM0, s);
        (vm.builtins[45])(&mut vm).expect("cvar");
        assert_eq!(vm.gf(OFS_RETURN), SV_GRAVITY);
    }

    #[test]
    fn traceline_writes_globals_without_host_panic() {
        // traceline against the empty world should write the trace_* globals and
        // never panic, even with the headnode-out-of-range "everything solid".
        let mut b = Builder::new();
        b.add_global("trace_fraction", EV_FLOAT, 50);
        b.add_global("trace_allsolid", EV_FLOAT, 51);
        b.add_global("trace_endpos", 3, 52);
        let _ = b.add_function(
            "main",
            vec![Statement {
                op: Op::Done as u16,
                a: 0,
                b: 0,
                c: 0,
            }],
        );
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        install_engine_builtins(&mut vm);
        vm.set_host(Box::new(WorldModel::new(empty_bsp())));
        vm.set_gv(crate::progs::OFS_PARM0, [0.0, 0.0, 0.0]);
        vm.set_gv(crate::progs::OFS_PARM0 + 3, [100.0, 0.0, 0.0]);
        (vm.builtins[16])(&mut vm).expect("traceline");
        // fraction in [0,1].
        let frac = vm.gget_float("trace_fraction");
        assert!((0.0..=1.0).contains(&frac));
    }

    #[test]
    fn push_entity_selects_nomonsters_for_trigger_and_not_solids() {
        // SV_PushEntity (sv_phys.c:408-421) chooses the move type from the
        // MOVING entity's solid: SOLID_TRIGGER / SOLID_NOT -> MOVE_NOMONSTERS
        // (the dropped backpack / gib / corpse passes THROUGH monster boxes),
        // anything else -> MOVE_NORMAL (it stops on the box).
        //
        // A monster box sits in the path. A SOLID_BBOX mover must stop short of
        // it; a SOLID_NOT mover must pass through and reach its endpoint.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let monster = server.vm.spawn();
        server.vm.ent_set_float(monster, "solid", SOLID_SLIDEBOX as f32);
        server.vm.ent_set_float(monster, "flags", FL_MONSTER as f32);
        server.vm.ent_set_vector(monster, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(monster, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(monster, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(monster, "absmin", [84.0, -16.0, -16.0]);
        server.vm.ent_set_vector(monster, "absmax", [116.0, 16.0, 16.0]);

        // SOLID_BBOX mover (normal): stops on the monster box.
        let blocker = server.vm.spawn();
        server.vm.ent_set_float(blocker, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_float(blocker, "movetype", MOVETYPE_BOUNCE as f32);
        server.vm.ent_set_vector(blocker, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "maxs", [0.0, 0.0, 0.0]);
        let tr_normal = server.push_entity(blocker, [200.0, 0.0, 0.0], 0.0);
        assert!(
            tr_normal.fraction < 1.0,
            "a SOLID_BBOX mover (MOVE_NORMAL) is stopped by the monster"
        );

        // SOLID_NOT mover (e.g. a gib): MOVE_NOMONSTERS, passes through.
        let gib = server.vm.spawn();
        server.vm.ent_set_float(gib, "solid", SOLID_NOT as f32);
        server.vm.ent_set_float(gib, "movetype", MOVETYPE_BOUNCE as f32);
        server.vm.ent_set_vector(gib, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(gib, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(gib, "maxs", [0.0, 0.0, 0.0]);
        let tr_not = server.push_entity(gib, [200.0, 0.0, 0.0], 0.0);
        assert_eq!(
            tr_not.fraction, 1.0,
            "a SOLID_NOT mover (MOVE_NOMONSTERS) passes through the monster"
        );

        // SOLID_TRIGGER mover: also MOVE_NOMONSTERS, passes through.
        let trig = server.vm.spawn();
        server.vm.ent_set_float(trig, "solid", SOLID_TRIGGER as f32);
        server.vm.ent_set_float(trig, "movetype", MOVETYPE_BOUNCE as f32);
        server.vm.ent_set_vector(trig, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(trig, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(trig, "maxs", [0.0, 0.0, 0.0]);
        let tr_trig = server.push_entity(trig, [200.0, 0.0, 0.0], 0.0);
        assert_eq!(
            tr_trig.fraction, 1.0,
            "a SOLID_TRIGGER mover (MOVE_NOMONSTERS) passes through the monster"
        );
    }

    #[test]
    fn push_entity_flymissile_expands_against_monsters() {
        // SV_PushEntity sends a MOVETYPE_FLYMISSILE mover through MOVE_MISSILE,
        // so a rocket whose centre path passes 20 units to the side of a small
        // monster still detonates (the +-15 expanded box reaches it). A
        // non-missile mover on the same path passes by.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let monster = server.vm.spawn();
        server.vm.ent_set_float(monster, "solid", SOLID_SLIDEBOX as f32);
        server.vm.ent_set_float(monster, "flags", FL_MONSTER as f32);
        server.vm.ent_set_vector(monster, "origin", [100.0, 20.0, 0.0]);
        server.vm.ent_set_vector(monster, "mins", [-5.0, -5.0, -5.0]);
        server.vm.ent_set_vector(monster, "maxs", [5.0, 5.0, 5.0]);

        // The rocket: a point box, MOVETYPE_FLYMISSILE, path at y=0.
        let rocket = server.vm.spawn();
        server.vm.ent_set_float(rocket, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_float(rocket, "movetype", MOVETYPE_FLYMISSILE as f32);
        server.vm.ent_set_vector(rocket, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(rocket, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(rocket, "maxs", [0.0, 0.0, 0.0]);
        let tr = server.push_entity(rocket, [200.0, 0.0, 0.0], 0.0);
        assert!(
            tr.fraction < 1.0,
            "a FLYMISSILE mover detonates NEAR the monster, got {}",
            tr.fraction
        );
    }

    #[test]
    fn connect_client_spawns_player_and_walks_without_tunnelling() {
        // The minimal progs sets health=100 and origin=(0,0,40) in
        // PutClientInServer. After connect the player has health 100 and a sane
        // origin; a forward usercmd advances it in XY and never sinks through the
        // floor at z = 0.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        assert_eq!(server.player_edict(), p);
        assert_eq!(server.player_health(), 100.0, "PutClientInServer set health");

        // The QuakeC spawn set origin to (0,0,40); give it a player box.
        let origin0 = server.vm.ent_get_vector(p, "origin");
        assert_eq!(origin0, [0.0, 0.0, 40.0], "spawn origin applied");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        // Settle onto the floor: a few empty frames let gravity + walk_move drop
        // it until the box bottom rests on z = 0 (origin.z ~ 24).
        let still = UserCmd::default();
        for _ in 0..20 {
            server.client_frame(&still, 0.1).expect("settle");
        }
        let settled = server.vm.ent_get_vector(p, "origin");
        assert!(
            settled[2] >= 24.0 - 1.0,
            "player rests on the floor (origin.z ~ 24), got {}",
            settled[2]
        );
        assert!(
            settled[2] <= 40.0 + 0.1,
            "player did not rise above spawn, got {}",
            settled[2]
        );

        // Drive forward (yaw 0 = +X) and confirm XY advance + no tunnelling.
        let cmd = UserCmd {
            forwardmove: 320.0,
            yaw: 0.0,
            ..UserCmd::default()
        };
        let before = server.vm.ent_get_vector(p, "origin");
        for _ in 0..10 {
            server.client_frame(&cmd, 0.1).expect("walk");
            let o = server.vm.ent_get_vector(p, "origin");
            // The box bottom is origin.z - 24; it must stay at/above the floor.
            assert!(
                o[2] - 24.0 >= -1.0,
                "player did not tunnel through the floor, origin.z = {}",
                o[2]
            );
        }
        let after = server.vm.ent_get_vector(p, "origin");
        assert!(
            after[0] > before[0] + 1.0,
            "player advanced forward in +X: {} -> {}",
            before[0],
            after[0]
        );
    }

    #[test]
    fn run_signon_frames_settles_the_spawned_player_before_frame_zero() {
        // Regression: the first frames of live play showed a one-time whole-view
        // texture/lighting "pop" — the just-connected player (QuakeC
        // PutClientInServer places it at spot.origin + '0 0 1'; the start map's
        // spawn floats ~5 units up) fell to the floor ON SCREEN because the port
        // rendered frame 0 with zero physics frames after PutClientInServer. The
        // C runs two SV_Physics ticks during the signon (Host_Spawn_f /
        // Host_Begin_f frames) before SCR_EndLoadingPlaque re-enables drawing,
        // so WinQuake's first visible frame shows a settled player.
        // `run_signon_frames` ports those ticks.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        // The synthetic PutClientInServer sets origin=(0,0,40) but no size; give
        // it the player box (the real progs calls setsize inside the spawn).
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        assert_eq!(
            server.vm.ent_get_vector(p, "origin")[2],
            40.0,
            "spawn floats above the floor (box bottom at z=16, floor at z=0)"
        );

        server.run_signon_frames();

        // Settled BEFORE the front-end's frame 0: on the ground, no residual
        // fall velocity, box bottom resting on the floor (origin.z ~ 24).
        let org = server.vm.ent_get_vector(p, "origin");
        let vel = server.vm.ent_get_vector(p, "velocity");
        let flags = server.vm.ent_get_float(p, "flags") as i32;
        assert!(flags & FL_ONGROUND != 0, "player is on the ground at frame 0");
        assert_eq!(vel[2], 0.0, "no residual fall velocity at frame 0");
        assert!(
            (23.0..=25.0).contains(&org[2]),
            "box bottom rests on the z=0 floor (origin.z ~ 24), got {}",
            org[2]
        );

        // ...and frame 0 == frame N for a static, zero-input camera: subsequent
        // frames must not move the player AT ALL (the pop was exactly this
        // motion leaking into the first rendered frames).
        for i in 0..10 {
            server
                .client_frame(&UserCmd::default(), 1.0 / 60.0)
                .expect("static frame");
            let now = server.vm.ent_get_vector(p, "origin");
            assert_eq!(now, org, "origin is bit-identical on static frame {i}");
        }
    }

    #[test]
    fn entity_dlights_muzzleflash_offsets_forward_and_up() {
        // An entity with EF_MUZZLEFLASH set yields one dlight keyed to the entity,
        // offset +16 z then +18 along its forward (angle) vector, minlight 32.
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.ent_set_vector(e, "origin", [100.0, 200.0, 50.0]);
        // Facing +x (yaw 0, pitch 0): forward = [1,0,0].
        server.vm.ent_set_vector(e, "angles", [0.0, 0.0, 0.0]);
        server.vm.ent_set_float(e, "effects", EF_MUZZLEFLASH as f32);

        let dls = server.entity_dlights();
        assert_eq!(dls.len(), 1, "one muzzleflash dlight");
        let d = dls[0];
        assert_eq!(d.key, e, "keyed to the firing entity");
        assert_eq!(d.minlight, 32.0);
        assert!((d.radius_base - 200.0).abs() < 1e-4, "base radius excludes jitter");
        assert!((d.life - 0.1).abs() < 1e-6);
        // origin + [18,0,0] + [0,0,16] = [118, 200, 66].
        assert!((d.origin[0] - 118.0).abs() < 1e-3, "forward x offset: {:?}", d.origin);
        assert!((d.origin[1] - 200.0).abs() < 1e-3);
        assert!((d.origin[2] - 66.0).abs() < 1e-3);
    }

    #[test]
    fn entity_dlights_brightlight_and_dimlight() {
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let bright = server.vm.spawn();
        server.vm.ent_set_vector(bright, "origin", [10.0, 20.0, 30.0]);
        server.vm.ent_set_float(bright, "effects", EF_BRIGHTLIGHT as f32);

        let dim = server.vm.spawn();
        server.vm.ent_set_vector(dim, "origin", [40.0, 50.0, 60.0]);
        server.vm.ent_set_float(dim, "effects", EF_DIMLIGHT as f32);

        let dls = server.entity_dlights();
        assert_eq!(dls.len(), 2);

        let b = dls.iter().find(|d| d.key == bright).expect("brightlight");
        assert!((b.radius_base - 400.0).abs() < 1e-4);
        assert_eq!(b.minlight, 0.0);
        assert_eq!(b.origin, [10.0, 20.0, 46.0]); // +16 z
        assert!((b.life - 0.001).abs() < 1e-7);

        let d = dls.iter().find(|d| d.key == dim).expect("dimlight");
        assert!((d.radius_base - 200.0).abs() < 1e-4);
        assert_eq!(d.minlight, 0.0);
        assert_eq!(d.origin, [40.0, 50.0, 60.0]); // origin unchanged
    }

    #[test]
    fn entity_dlights_skips_zero_effects_and_free_edicts() {
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // No effects -> no dlight.
        let plain = server.vm.spawn();
        server.vm.ent_set_vector(plain, "origin", [1.0, 2.0, 3.0]);
        server.vm.ent_set_float(plain, "effects", 0.0);

        // A freed edict with effects set must be ignored.
        let gone = server.vm.spawn();
        server.vm.ent_set_float(gone, "effects", EF_DIMLIGHT as f32);
        server.vm.free_edict(gone);

        assert!(server.entity_dlights().is_empty(), "no live lit entities");
    }

    // -------------------------------------------------------- level transitions

    #[test]
    fn bi_changelevel_records_map_and_drain_returns_once() {
        // bi_changelevel (PF_changelevel, #70) must record its string argument and
        // take_pending_changelevel must return it exactly once, then None.
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // A fresh server has no pending request.
        assert_eq!(
            server.take_pending_changelevel(),
            None,
            "fresh server has no pending changelevel"
        );

        // Drive the builtin directly: place the map name's string_t in PARM0.
        let map_t = server.vm.intern("e1m2");
        server.vm.set_gi(OFS_PARM0, map_t);
        bi_changelevel(&mut server.vm).expect("bi_changelevel");

        // take_pending_changelevel returns it once, then drains to None.
        assert_eq!(server.take_pending_changelevel().as_deref(), Some("e1m2"));
        assert_eq!(
            server.take_pending_changelevel(),
            None,
            "second take drains to None"
        );
    }

    #[test]
    fn bi_localcmd_restart_sets_pending_respawn() {
        // localcmd("restart\n") (PF_localcmd #46) is the single-player death-respawn
        // path: it must set the pending-restart flag, which take_pending_restart
        // returns exactly once. Other console text is a benign no-op, and a
        // `changelevel <map>` localcmd routes to the changelevel queue.
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        assert!(!server.take_pending_restart(), "fresh server: no pending restart");

        // "restart\n" -> pending restart (trailing newline + case-insensitive word).
        let t = server.vm.intern("restart\n");
        server.vm.set_gi(OFS_PARM0, t);
        bi_localcmd(&mut server.vm).expect("bi_localcmd restart");
        assert!(server.take_pending_restart(), "restart sets the pending flag");
        assert!(!server.take_pending_restart(), "second take drains to false");

        // An unrelated console command does nothing.
        let t2 = server.vm.intern("echo hi");
        server.vm.set_gi(OFS_PARM0, t2);
        bi_localcmd(&mut server.vm).expect("bi_localcmd echo");
        assert!(!server.take_pending_restart(), "unrelated localcmd is a no-op");

        // A `changelevel e1m2` localcmd routes to the changelevel queue, not restart.
        let t3 = server.vm.intern("changelevel e1m2");
        server.vm.set_gi(OFS_PARM0, t3);
        bi_localcmd(&mut server.vm).expect("bi_localcmd changelevel");
        assert!(!server.take_pending_restart(), "changelevel localcmd is not a restart");
        assert_eq!(server.take_pending_changelevel().as_deref(), Some("e1m2"));
    }

    /// Synthetic progs for [`Server::client_kill`] (`Host_Kill_f`): a `ClientKill`
    /// QuakeC function that calls the `localcmd` builtin (#46) with a
    /// `"restart\n"` string — the single-player suicide chain (`ClientKill` ->
    /// `respawn()` -> `localcmd("restart\n")`, client.qc) compressed to its
    /// engine-visible effect. Returns `(image, g_str, g_fn, localcmd_index)`;
    /// the test fills global `g_str` with the interned string and `g_fn` with
    /// the localcmd function value after load.
    fn client_kill_progs() -> (Vec<u8>, usize, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 24;

        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);

        // Minimal field set so spawn()/link/connect work.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2);
        b.add_field("mins", EV_VECTOR, 5);
        b.add_field("maxs", EV_VECTOR, 8);
        b.add_field("absmin", EV_VECTOR, 11);
        b.add_field("absmax", EV_VECTOR, 14);
        b.add_field("flags", EV_FLOAT, 17);
        b.add_field("movetype", EV_FLOAT, 18);
        b.add_field("solid", EV_FLOAT, 19);
        b.add_field("size", EV_VECTOR, 20);
        b.add_field("health", EV_FLOAT, 23);

        let done = || Statement {
            op: Op::Done as u16,
            a: 0,
            b: 0,
            c: 0,
        };
        b.add_function("ClientConnect", vec![done()]);
        b.add_function("PutClientInServer", vec![done()]);

        let localcmd = b.add_builtin("localcmd", 46);

        // Cells the test fills after load: the "restart\n" string_t and the
        // localcmd function value the CALL1 dereferences.
        let g_str = 40u16;
        let g_fn = 41u16;
        // ClientKill: localcmd("restart\n");
        b.add_function(
            "ClientKill",
            vec![
                Statement {
                    op: Op::StoreS as u16,
                    a: g_str as i16,
                    b: OFS_PARM0 as i16,
                    c: 0,
                },
                Statement {
                    op: Op::Call1 as u16,
                    a: g_fn as i16,
                    b: 0,
                    c: 0,
                },
                done(),
            ],
        );

        (b.build(), g_str as usize, g_fn as usize, localcmd)
    }

    #[test]
    fn client_kill_runs_clientkill_and_refuses_when_dead() {
        // Host_Kill_f (host_cmd.c): `kill` must route through the QuakeC
        // ClientKill entry point (the REAL suicide chain, ending in respawn() ->
        // localcmd("restart\n") in single player), and must refuse an
        // already-dead player WITHOUT running ClientKill.
        let (img, g_str, g_fn, localcmd) = client_kill_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // No client connected yet: refused, no QuakeC runs.
        assert!(
            !server.client_kill().expect("kill w/o client"),
            "no connected client -> refused"
        );

        let player = server.connect_client().expect("connect");
        // Fill ClientKill's constants: the "restart\n" string + localcmd fn value.
        let s = server.vm.intern("restart\n");
        server.vm.set_gi(g_str, s);
        server.vm.set_gi(g_fn, localcmd as i32);

        // Alive player: ClientKill runs; its localcmd("restart\n") queues the
        // single-player respawn exactly once.
        server.vm.ent_set_float(player, "health", 100.0);
        assert!(
            server.client_kill().expect("kill alive"),
            "alive player -> ClientKill ran"
        );
        assert!(
            server.take_pending_restart(),
            "ClientKill -> localcmd(restart) -> pending respawn"
        );
        assert!(!server.take_pending_restart(), "second take drains to false");

        // Dead player: refused (the C prints "Can't suicide -- allready dead!"),
        // and ClientKill must NOT have run — nothing queued.
        server.vm.ent_set_float(player, "health", 0.0);
        assert!(!server.client_kill().expect("kill dead"), "dead -> refused");
        assert!(
            !server.take_pending_restart(),
            "a refused kill queues no respawn"
        );
    }

    #[test]
    fn dead_client_movetype_toss_gets_corpse_physics() {
        // SV_Physics_Client (sv_phys.c) routes a MOVETYPE_TOSS/BOUNCE client
        // through SV_Physics_Toss — the dead player's corpse physics (client.qc
        // PlayerDie sets movetype TOSS + a velocity pop). The old fallback arm
        // ran the think only, freezing a mid-air corpse in place. The corpse
        // must gain downward velocity (gravity) and fall.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        // A dead player hovering above the floor: TOSS, not on ground, no velocity.
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 120.0]);
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
        server.vm.ent_set_float(p, "health", 0.0);
        server.vm.ent_set_float(p, "movetype", MOVETYPE_TOSS as f32);
        let flags = server.vm.ent_get_float(p, "flags") as i32 & !FL_ONGROUND;
        server.vm.ent_set_float(p, "flags", flags as f32);

        server.client_frame(&UserCmd::default(), 0.1).expect("frame");

        let vel = server.vm.ent_get_vector(p, "velocity");
        let org = server.vm.ent_get_vector(p, "origin");
        assert!(
            vel[2] < 0.0,
            "toss corpse gains downward velocity (gravity): vz = {}",
            vel[2]
        );
        assert!(
            org[2] < 120.0,
            "toss corpse falls instead of freezing mid-air: z = {}",
            org[2]
        );
    }

    #[test]
    fn bi_changelevel_first_writer_wins_within_a_frame() {
        // Two changelevel() calls before a drain: the first wins (mirrors the C
        // svs.changelevel_issued guard).
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        let a = server.vm.intern("e1m2");
        server.vm.set_gi(OFS_PARM0, a);
        bi_changelevel(&mut server.vm).expect("first");
        let bm = server.vm.intern("e1m3");
        server.vm.set_gi(OFS_PARM0, bm);
        bi_changelevel(&mut server.vm).expect("second");

        assert_eq!(
            server.take_pending_changelevel().as_deref(),
            Some("e1m2"),
            "first writer wins"
        );
    }

    #[test]
    fn fresh_server_clears_stale_changelevel_request() {
        // A request left in the thread-local must not leak into a freshly built
        // server (Server::new calls reset_changelevel).
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        // Issue a request against one server...
        let mut s1 = Server::new(empty_bsp(), progs).expect("server");
        let t = s1.vm.intern("e1m9");
        s1.vm.set_gi(OFS_PARM0, t);
        bi_changelevel(&mut s1.vm).expect("bi");
        // ...then a new server clears it before the old one ever drained.
        let progs2 = Progs::parse(&img).expect("parse");
        let mut s2 = Server::new(empty_bsp(), progs2).expect("server");
        assert_eq!(
            s2.take_pending_changelevel(),
            None,
            "new server starts with no pending changelevel"
        );
    }

    #[test]
    fn save_spawn_parms_returns_sixteen_floats_via_setchangeparms() {
        // save_spawn_parms runs SetChangeParms (which writes parm1 = g_const) and
        // returns the 16 parm globals. With no client connected it returns zeros
        // and never panics.
        let (img, g_const, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // No client yet -> all zeros, no panic.
        assert_eq!(server.save_spawn_parms(), [0.0; NUM_SPAWN_PARMS]);

        // Connect the player, set the constant SetChangeParms marshals into parm1.
        server.connect_client().expect("connect");
        server.vm.set_gf(g_const, 42.0);
        let parms = server.save_spawn_parms();
        assert_eq!(parms.len(), NUM_SPAWN_PARMS);
        assert_eq!(parms[0], 42.0, "SetChangeParms wrote parm1");
        assert_eq!(&parms[1..], &[0.0; NUM_SPAWN_PARMS - 1]);
    }

    #[test]
    fn connect_client_with_parms_writes_parm_globals_before_spawn() {
        // connect_client_with_parms must write the supplied parms into the
        // parm1..parm16 globals BEFORE running PutClientInServer (so
        // DecodeLevelParms sees them). The progs' PutClientInServer copies parm1
        // into `decoded`, proving the parm was live when the spawn script ran.
        let (img, _gc, g_decoded) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let mut parms = [0.0f32; NUM_SPAWN_PARMS];
        parms[0] = 7.5; // parm1
        parms[3] = 25.0; // parm4 (e.g. shells)
        let p = server
            .connect_client_with_parms(parms)
            .expect("connect with parms");
        assert_eq!(server.player_edict(), p);

        // The parm1 global holds the value we passed in...
        assert_eq!(
            server.vm.gget_float("parm1"),
            7.5,
            "connect_client_with_parms wrote parm1"
        );
        assert_eq!(server.vm.gget_float("parm4"), 25.0, "and parm4");
        // ...and PutClientInServer (the spawn script) saw it (decoded parm1).
        assert_eq!(
            server.vm.gf(g_decoded),
            7.5,
            "PutClientInServer ran AFTER the parm globals were set"
        );
    }
}
