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
use crate::{QError, Result};

mod host;
mod lightstyle;
mod msg;
mod pr_edict;
mod sv_move;
mod sv_phys;
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

pub(crate) use host::{capture_transports, restore_transports};
pub(crate) use lightstyle::{push_lightstyle, snapshot_lightstyles};
pub(crate) use pr_edict::{ed_new_string, parse_float, parse_int, parse_vector, Tokenizer};
pub(crate) use sv_world::link_edict;

use host::{
    bi_changelevel, bi_localcmd, reset_changelevel, reset_restart, reset_skill, set_skill_value,
    skill_value,
};
use lightstyle::{bi_lightstyle, reset_lightstyles};
use msg::{
    bi_ambientsound, bi_bprint, bi_centerprint, bi_particle, bi_sound, bi_sprint, bi_writeangle,
    bi_writebyte, bi_writechar, bi_writecoord, bi_writeentity, bi_writelong, bi_writeshort,
    bi_writestring, reset_svc_recognizer, take_svc_events,
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
    use crate::progs::{Op, Statement, OFS_RETURN};

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

    // ---------------------------------------------------- skill + water tests

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
