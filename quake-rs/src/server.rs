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
use crate::progs::{EType, Progs};
use crate::vm::{Builtin, Host, HostTrace, Vm};
use crate::world;
use crate::{QError, Result};

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

/// `DI_NODIR` (sv_move.c): the "no preferred direction" sentinel used by
/// [`sv_new_chase_dir`]'s axis-direction picks.
const DI_NODIR: f32 = -1.0;

/// `CONTENTS_SOLID` / `CONTENTS_EMPTY` (bsp.h): the two point-contents values
/// [`sv_check_bottom`] and [`sv_movestep`] test against (re-stated here so the
/// movement code reads naturally without importing the whole bsp contents set).
const CONTENTS_SOLID: i32 = -2;
const CONTENTS_EMPTY: i32 = -1;

// Skill spawnflags (server.h). These mark an entity as absent on a given
// difficulty (or in deathmatch); `ED_LoadFromFile` filters by the current skill.
const SPAWNFLAG_NOT_EASY: i32 = 256;
const SPAWNFLAG_NOT_MEDIUM: i32 = 512;
const SPAWNFLAG_NOT_HARD: i32 = 1024;
const SPAWNFLAG_NOT_DEATHMATCH: i32 = 2048;

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

// Player-movement cvars (sv_user.c defaults). This engine has no console-cvar
// subsystem, so the values are faithful constants instead of a registry.
/// `sv_friction` default ("4").
const SV_FRICTION: f32 = 4.0;
/// `sv_stopspeed` default ("100").
const SV_STOPSPEED: f32 = 100.0;
/// `sv_accelerate` default ("10").
const SV_ACCELERATE: f32 = 10.0;
/// `sv_maxspeed` default ("320").
const SV_MAXSPEED: f32 = 320.0;
/// `edgefriction` default ("2"): friction multiplier when the leading edge of
/// the player box hangs over a dropoff (`SV_UserFriction`).
const SV_EDGEFRICTION: f32 = 2.0;

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

/// The bounds half of `SV_LinkEdict`: `absmin = origin + mins`,
/// `absmax = origin + maxs`. (The C also inserted the edict into the area grid
/// and touched triggers; neither is modelled here.) `pub(crate)` so the
/// savegame loader (`save.rs`) can relink loaded edicts exactly as
/// `Host_Loadgame_f` does (`SV_LinkEdict(ent, false)`).
pub(crate) fn link_edict(vm: &mut Vm, e: i32) {
    let origin = vm.ent_get_vector(e, "origin");
    let mins = vm.ent_get_vector(e, "mins");
    let maxs = vm.ent_get_vector(e, "maxs");
    let mut absmin = v_add(origin, mins);
    let mut absmax = v_add(origin, maxs);
    // SV_LinkEdict expands the abs box so tangent boxes still register as
    // touching: items get a generous ±15 on X/Y (easier pickups), everything
    // else ±1 on all axes (movement is clipped an epsilon shy of the surface).
    let flags = vm.ent_get_float(e, "flags") as i32;
    if flags & FL_ITEM != 0 {
        absmin[0] -= 15.0;
        absmin[1] -= 15.0;
        absmax[0] += 15.0;
        absmax[1] += 15.0;
    } else {
        for i in 0..3 {
            absmin[i] -= 1.0;
            absmax[i] += 1.0;
        }
    }
    vm.ent_set_vector(e, "absmin", absmin);
    vm.ent_set_vector(e, "absmax", absmax);
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
// Animated light styles (PF_lightstyle / R_AnimateLight).
//
// `lightstyle(style, val)` (pr_cmds.c PF_lightstyle, #35) stores a pattern
// string per style index into `sv.lightstyles[64]`. The QuakeC worldspawn calls
// it for styles 0..11 with the classic patterns (steady "m", torch flicker
// "mmnmmommommnonmmonqnmmo", slow pulse "abcdefghijklmnopqrstuvwxyz…", …), so the
// table is populated automatically while `spawn_entities` runs worldspawn.
//
// This is *persistent map state* — the renderer reads it every frame to animate
// lightmaps — so unlike the per-frame sound/particle/temp-entity queues it is
// OWNED by the [`Server`] (the `lightstyles` field), not drained-and-discarded.
// The thread-local below is only the *write transport*: builtins are
// `fn(&mut Vm)` and cannot see the `Server`, and `vm.rs` (the `Host` trait) is
// off-limits, so the builtin has no other place to write. The Server syncs the
// transport into its owned table after each QuakeC execution window
// (`spawn_entities` / `run_frame`) and the getter reads the owned table. The
// transport is reset in [`Server::new`] so a changelevel re-populates cleanly.
// ---------------------------------------------------------------------------

/// `MAX_LIGHTSTYLES` (quakedef.h): the size of `sv.lightstyles[]`.
pub const MAX_LIGHTSTYLES: usize = 64;

thread_local! {
    /// Write transport for [`bi_lightstyle`]: the latest pattern string per style
    /// index. The [`Server`] owns the authoritative copy and syncs from here; this
    /// is reset in [`Server::new`] so a fresh level starts empty. See the module
    /// note above for why a thread-local transport (not a field) is unavoidable
    /// for an engine builtin.
    static LIGHTSTYLES: std::cell::RefCell<[String; MAX_LIGHTSTYLES]> =
        std::cell::RefCell::new(std::array::from_fn(|_| String::new()));
}

/// Store `val` at style index `style` in the thread-local transport. An
/// out-of-range index is ignored (no panic), matching the C's silent clamp
/// (`if (style >= MAX_LIGHTSTYLES) ...`). `pub(crate)` so the savegame loader
/// can restore the saved styles into the transport (a later frame's
/// `snapshot_lightstyles` sync must not revert them to the fresh-spawn set).
pub(crate) fn push_lightstyle(style: usize, val: String) {
    if style >= MAX_LIGHTSTYLES {
        return;
    }
    LIGHTSTYLES.with(|t| {
        if let Some(slot) = t.borrow_mut().get_mut(style) {
            *slot = val;
        }
    });
}

/// Snapshot the current transport table (the latest pattern per style).
pub(crate) fn snapshot_lightstyles() -> [String; MAX_LIGHTSTYLES] {
    LIGHTSTYLES.with(|t| t.borrow().clone())
}

/// Clear the transport table (called from [`Server::new`] so a stale level's
/// styles cannot leak into a fresh server before its worldspawn repopulates).
fn reset_lightstyles() {
    LIGHTSTYLES.with(|t| {
        *t.borrow_mut() = std::array::from_fn(|_| String::new());
    });
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

/// `PF_lightstyle` (#35): `void(float style, string value) lightstyle`. The C
/// `PF_lightstyle` stored `value` into `sv.lightstyles[style]` and, for live
/// clients, broadcast an `svc_lightstyle` update. This headless server has no
/// netcode, so we only store the pattern (PARM0 = style index, PARM1 = the
/// pattern string). An out-of-range style index is ignored without panicking.
fn bi_lightstyle(vm: &mut Vm) -> Result<()> {
    let style = vm.arg_float(0);
    let val = vm.arg_string(1);
    // The C truncates the float to an int index; negatives / NaN clamp out of
    // range and are dropped by `push_lightstyle`.
    let idx = if style.is_finite() && style >= 0.0 {
        style as usize
    } else {
        usize::MAX
    };
    push_lightstyle(idx, val);
    Ok(())
}

/// `R_AnimateLight` letter scale: map a pattern character to its
/// `d_lightstylevalue` (the C `(c - 'a') * 22`). Non-letters fold modulo 26 onto
/// the `a..z` range like the C's byte arithmetic, never reading out of bounds.
fn lightstyle_letter_value(ch: u8) -> f32 {
    // The C indexes `lightstyles[j].map[k]` (an ASCII byte) and computes
    // `(map[k]-'a')*22`. Authored patterns are always `a..z`; for robustness we
    // wrap any other byte into `0..=25` rather than producing a wild value.
    let v = (ch.wrapping_sub(b'a')) % 26;
    v as f32 * 22.0
}

/// `R_AnimateLight` (r_light.c) over an arbitrary style table: the per-style
/// brightness scale at game `time`, one entry per [`MAX_LIGHTSTYLES`] index.
/// Shared by [`Server::lightstyle_scales`] (the live walk's `sv.lightstyles`)
/// and demo playback (the RECORDED `svc_lightstyle` table a `.dem` carries),
/// so both paths animate through the identical 10 Hz logic.
///
/// `styles` shorter than [`MAX_LIGHTSTYLES`] treats the missing tail as unset
/// (scale `1.0`), so a demo-frame table can be passed directly.
///
/// For style `j` with pattern string of length `L`:
/// * `L == 0` (unset) → scale `1.0` (the C `d_lightstylevalue = 256`, i.e.
///   "normal"). Treating a missing style as normal keeps faces that reference
///   an unset style at full brightness rather than going dark.
/// * else the string animates at 10 chars/sec: `k = floor(time*10) mod L`,
///   `ch = string[k]`, and the C `d_lightstylevalue[j] = (ch - 'a') * 22`
///   (so `'a'` → 0 = dark, `'m'` → 264 = normal, `'z'` → 550 ≈ double-bright).
pub fn lightstyle_scales_at(styles: &[String], time: f32) -> [f32; MAX_LIGHTSTYLES] {
    // Normalise by 256 — id's white point — NOT by 'm' (264). R_AnimateLight
    // sets d_lightstylevalue[j] = (letter-'a')*22 (so worldspawn's lightstyle
    // (0,"m") gives style 0 = 264), and R_BuildLightMap renders luxel*scale
    // against the constant 255*256 white point. So a steady 'm' world is
    // luxel*264/256 = 1.03125x — slightly brighter than a literal luxel*256.
    // Normalising by 'm' (264) made style 0 exactly 1.0, rendering the entire
    // static-lit world ~1 colormap row too dark; /256 matches id. An UNSET style
    // still maps to 1.0 below (R_AnimateLight's length==0 default of 256).
    const NORMAL: f32 = 256.0;
    // Animation phase in characters; floor(time*10), guarded against a
    // non-finite/huge time so the modulo index never overflows or panics.
    let phase: i64 = if time.is_finite() {
        (time * 10.0).floor() as i64
    } else {
        0
    };
    std::array::from_fn(|j| {
        let s = styles.get(j).map(|s| s.as_bytes()).unwrap_or(b"");
        if s.is_empty() {
            return 1.0; // unset style -> normal (256/264 ~ never; treat as 1.0)
        }
        let len = s.len() as i64;
        // Positive modulo: ((phase % len) + len) % len keeps k in 0..len even
        // for a negative phase (a time before 0).
        let k = (((phase % len) + len) % len) as usize;
        lightstyle_letter_value(s[k]) / NORMAL
    })
}

// ---------------------------------------------------------------------------
// Sound-event queue (PF_sound / PF_ambientsound).
//
// The C `PF_sound` -> `SV_StartSound` wrote an `svc_sound` message into the
// per-client datagram for the network layer to flush. This headless server has
// no netcode, so instead each fired sound is captured as a [`SoundEvent`] in a
// process-wide queue that [`Server::drain_sounds`] hands to whatever audio
// front-end (or test) wants it.
//
// Builtins are `fn(&mut Vm)` and cannot see the `Server`, and the `Vm` type
// lives in `vm.rs` (which this task may not edit), so the queue cannot hang off
// either. A `thread_local!` `RefCell<Vec<SoundEvent>>` reached from `bi_sound`
// is the cleanest spot that keeps the builtin signature intact. Server methods
// run on the same thread as the builtins they invoke, so the events a frame's
// QuakeC fires are visible to `drain_sounds` immediately afterward.
// ---------------------------------------------------------------------------

/// One queued sound emission — the engine `SV_StartSound` payload, captured for
/// a front-end instead of being serialised into a client datagram.
///
/// `origin` is the entity's box centre (`origin + 0.5*(mins+maxs)`), matching
/// the coordinate `SV_StartSound` wrote. `sample` keeps the raw sound name;
/// `sound_index` is its precache slot (`>= 1`) or `-1` if it was never
/// precached (the C `Con_Printf("not precacheed")`-and-drop case — we still
/// queue the event so a caller can see what was attempted).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundEvent {
    /// The emitting edict index.
    pub entity: i32,
    /// Sound channel (0 = auto-allocate; 1..=7 override that entity/channel).
    pub channel: i32,
    /// Precache index of `sample`, or `-1` when it was not precached.
    pub sound_index: i32,
    /// The raw sound name (e.g. `"weapons/guncock.wav"`).
    pub sample: String,
    /// World-space emission point: `origin + 0.5*(mins + maxs)`.
    pub origin: [f32; 3],
    /// Volume in `0.0..=1.0` (the C scaled this by 255 for the packet byte).
    pub volume: f32,
    /// Attenuation in `0.0..=4.0` (0 = audible everywhere).
    pub attenuation: f32,
}

thread_local! {
    /// Process-wide (per-thread) queue the sound builtins push to and
    /// [`Server::drain_sounds`] takes. See the module note above.
    static SOUND_EVENTS: std::cell::RefCell<Vec<SoundEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Push a fired sound onto the thread-local queue.
fn push_sound_event(ev: SoundEvent) {
    SOUND_EVENTS.with(|q| q.borrow_mut().push(ev));
}

/// Take and clear every queued sound event.
fn take_sound_events() -> Vec<SoundEvent> {
    SOUND_EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

// ---------------------------------------------------------------------------
// Static (looping ambient) sound registry (PF_ambientsound).
//
// The C `PF_ambientsound` (pr_cmds.c) wrote an `svc_spawnstaticsound` into the
// level signon packet; the client's `CL_ParseStaticSound` -> `S_StaticSound`
// (snd_dma.c) then allocated a PERSISTENT looping channel re-spatialized every
// frame. These are the torch crackles / wind / hums placed by the QuakeC at
// level spawn. Like the one-shot queue above, this headless server has no
// netcode, so each `ambientsound()` is recorded as a [`StaticSound`] in a
// process-wide thread-local list that [`Server::drain_static_sounds`] hands to
// the front-end ONCE (the front-end keeps the loops alive itself, mirroring how
// the signon packet was sent once at connect).
// ---------------------------------------------------------------------------

/// One placed looping ambient sound — the `svc_spawnstaticsound` payload the C
/// `PF_ambientsound` wrote into the signon, captured for a front-end.
///
/// `volume`/`attenuation` are kept in the QuakeC domain (`0.0..=1.0` /
/// `0.0..=4.0`) but quantized through the same bytes the wire format used
/// (`vol*255` and `atten*64`, truncated), so a front-end hears exactly what the
/// original client was told. `sound_index` mirrors [`SoundEvent::sound_index`]:
/// the precache slot, or `-1` when no host resolved it.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticSound {
    /// World-space emission point (`PF_ambientsound`'s literal `pos` argument —
    /// static sounds are placed at a point, not on an entity).
    pub origin: [f32; 3],
    /// Precache index of `sample`, or `-1` when it was not resolved.
    pub sound_index: i32,
    /// The raw sound name (e.g. `"ambience/fire1.wav"`).
    pub sample: String,
    /// Volume in `0.0..=1.0`, quantized through the wire byte (`trunc(vol*255)/255`).
    pub volume: f32,
    /// Attenuation in `0.0..=4.0`, quantized through the wire byte
    /// (`trunc(atten*64)/64`; `ATTN_STATIC` = 3 survives exactly).
    pub attenuation: f32,
}

thread_local! {
    /// Process-wide (per-thread) registry [`bi_ambientsound`] pushes to and
    /// [`Server::drain_static_sounds`] takes. See the module note above; the
    /// thread-local reasoning mirrors [`SOUND_EVENTS`] exactly.
    static STATIC_SOUNDS: std::cell::RefCell<Vec<StaticSound>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Push a placed static sound onto the thread-local registry.
fn push_static_sound(ev: StaticSound) {
    STATIC_SOUNDS.with(|q| q.borrow_mut().push(ev));
}

/// Take and clear every registered static sound.
fn take_static_sounds() -> Vec<StaticSound> {
    STATIC_SOUNDS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Box centre of an entity: `origin + 0.5*(mins + maxs)`, the point
/// `SV_StartSound`/`PF_ambientsound` wrote for the emission coordinate.
fn entity_sound_origin(vm: &Vm, e: i32) -> [f32; 3] {
    let origin = vm.ent_get_vector(e, "origin");
    let mins = vm.ent_get_vector(e, "mins");
    let maxs = vm.ent_get_vector(e, "maxs");
    [
        origin[0] + 0.5 * (mins[0] + maxs[0]),
        origin[1] + 0.5 * (mins[1] + maxs[1]),
        origin[2] + 0.5 * (mins[2] + maxs[2]),
    ]
}

// ---------------------------------------------------------------------------
// Particle-burst queue (PF_particle).
//
// The C `PF_particle` -> `SV_StartParticle` wrote an `svc_particle` message
// into the per-client datagram; the client's `R_RunParticleEffect` then spawned
// the actual particles into its `d_*` software renderer. This headless server
// has no client, so — exactly like the sound queue above — each fired
// `particle()` is captured as a [`ParticleBurst`] in a process-wide thread-local
// queue that [`Server::drain_particles`] hands to a front-end. The front-end
// (wasm/quaketool) owns the live [`crate::particles::ParticleSystem`] that turns
// a drained burst into spawned points, ages them, and draws them into the scene.
//
// The reasoning for a `thread_local!` (rather than a field on `Server` or `Vm`)
// is identical to the sound queue's: builtins are `fn(&mut Vm)` and cannot see
// the `Server`, and `vm.rs` is off-limits, so the queue cannot hang off either.
// ---------------------------------------------------------------------------

/// One queued `particle()` burst — the engine `SV_StartParticle` payload,
/// captured for a front-end instead of being serialised into a client datagram.
///
/// The fields mirror `PF_particle`'s arguments verbatim: `org` is the emission
/// origin, `dir` the direction/speed the C scaled into the velocity, `color` the
/// base palette index of the 8-entry colour ramp, and `count` the number of
/// particles to spawn. A front-end replays this through
/// [`crate::particles::ParticleSystem::spawn_burst`].
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleBurst {
    /// Emission origin (world space).
    pub org: [f32; 3],
    /// Direction/speed the renderer scales into each particle's velocity.
    pub dir: [f32; 3],
    /// Base palette index of the colour ramp (`color & ~7` selects the ramp).
    pub color: u8,
    /// How many particles to spawn (clamped against the pool cap on spawn).
    pub count: i32,
}

thread_local! {
    /// Process-wide (per-thread) queue [`bi_particle`] pushes to and
    /// [`Server::drain_particles`] takes. See the module note above; mirrors the
    /// [`SOUND_EVENTS`] queue exactly.
    static PARTICLE_BURSTS: std::cell::RefCell<Vec<ParticleBurst>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Push a fired particle burst onto the thread-local queue.
fn push_particle_burst(ev: ParticleBurst) {
    PARTICLE_BURSTS.with(|q| q.borrow_mut().push(ev));
}

/// Take and clear every queued particle burst.
fn take_particle_bursts() -> Vec<ParticleBurst> {
    PARTICLE_BURSTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// A text message QuakeC asked to show the player: a `centerprint` (drawn
/// centered for a couple of seconds — level intros, "you need the silver key")
/// or a `bprint`/`sprint` notify line (item pickups, etc.). Drained each frame by
/// the front-end, which renders + times them out.
pub struct GameMessage {
    /// True for `centerprint` (centered, transient); false for a notify line.
    pub center: bool,
    /// The message text (may contain '\n').
    pub text: String,
}

thread_local! {
    // QuakeC print routing the front-end displays. Same single-threaded-VM
    // rationale as the sound/particle/temp-entity queues above.
    static MESSAGES: std::cell::RefCell<Vec<GameMessage>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn push_message(center: bool, text: String) {
    if text.is_empty() {
        return;
    }
    MESSAGES.with(|q| q.borrow_mut().push(GameMessage { center, text }));
}

fn take_messages() -> Vec<GameMessage> {
    MESSAGES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// `PF_centerprint` (#73): show the (var-arg concatenated) message centered on
/// screen for a few seconds. The client arg (index 0) is ignored (single-player);
/// the message is args from index 1. Also mirrored into the dev `output` log.
fn bi_centerprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 1);
    vm.output.push_str(&s);
    push_message(true, s);
    Ok(())
}

/// `PF_bprint` (#23): broadcast print — shown as a notify line.
fn bi_bprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 0);
    vm.output.push_str(&s);
    push_message(false, s);
    Ok(())
}

/// `PF_sprint` (#24): single-client print — a notify line (client arg at index 0
/// ignored; message is args from index 1).
fn bi_sprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 1);
    vm.output.push_str(&s);
    push_message(false, s);
    Ok(())
}

/// `PF_particle` (#48): `void(vector org, vector dir, float color, float count)
/// particle`. The C forwarded these straight to `SV_StartParticle`; here we
/// queue a [`ParticleBurst`] for the front-end's [`crate::particles::ParticleSystem`]
/// to realise. The base `color` and `count` are kept as the engine domain (a
/// palette index and a particle count); the colour is cast into a `u8` palette
/// index (the C `SV_StartParticle` itself wrote `color` as one packet byte).
///
/// FAITHFULNESS: the C `SV_StartParticle` early-returned when the network
/// datagram was nearly full; we have no datagram, so every fired burst is
/// queued. A negative/huge `count` is preserved as-is and clamped only when the
/// `ParticleSystem` spawns it, so the engine never allocates on program data.
fn bi_particle(vm: &mut Vm) -> Result<()> {
    let org = vm.arg_vector(0);
    let dir = vm.arg_vector(1);
    // color is a float palette index; clamp into 0..=255 before the byte cast so
    // an out-of-range value can never wrap unexpectedly.
    let color = vm.arg_float(2).clamp(0.0, 255.0) as u8;
    // SV_StartParticle writes `count` through MSG_WriteByte, which TRUNCATES mod 256
    // (`buf[0] = c`), and the client's CL_ParseParticleEffect maps the byte value
    // EXACTLY 255 back to 1024 — the explosion sentinel (R_RunParticleEffect's fiery
    // pt_explode burst). So the trigger is `(count & 0xFF) == 255`, not `count >=
    // 255`: a stray count like 256 truncates to 0 (no explosion), and -1 wraps to
    // 255 -> 1024, exactly as the C and the demo parser (demo.rs) do. The
    // misc_explobox death does `particle(origin, '0 0 0', 75, 255)` -> 1024 -> burst.
    let sent = (vm.arg_float(3) as i32 & 0xFF) as u8;
    let count = if sent == 255 { 1024 } else { sent as i32 };

    push_particle_burst(ParticleBurst {
        org,
        dir,
        color,
        count,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Temp-entity decoder + queue (the network Write* family, #52..#59).
//
// The C produced a temp entity by writing a short ordered burst into the
// broadcast datagram from QuakeC: WriteByte(MSG_BROADCAST, svc_temp_entity=23),
// WriteByte(MSG_BROADCAST, TE_type), then the per-type payload (coords / bytes).
// The client's `CL_ParseTEnt` (cl_tent.c) read that burst back and turned each
// temp entity into a particle effect (R_ParticleExplosion / R_RunParticleEffect)
// plus, for explosions, a dynamic light and the `weapons/r_exp3.wav` sound.
//
// This headless server has no client and no datagram, so the Write* builtins
// instead feed a small decoder state machine that recognises a broadcast temp
// entity and, when its payload is complete, emits a [`TempEntityEvent`] onto a
// thread-local queue that [`Server::drain_temp_entities`] hands to a front-end
// (which maps it to the same [`crate::particles::ParticleSystem`] effects).
//
// As with the sound/particle queues above, a `thread_local!` is the only place
// the state can live: builtins are `fn(&mut Vm)` and cannot see the `Server`,
// and `vm.rs` is off-limits. Server methods run on the same thread as the
// builtins, so a frame's events are visible to `drain_temp_entities` right after.
// ---------------------------------------------------------------------------

/// `svc_temp_entity` (protocol.h): the server-command byte a broadcast temp
/// entity begins with. A `WriteByte(MSG_BROADCAST, 23)` opens the burst.
const SVC_TEMP_ENTITY: u8 = 23;

/// `MSG_BROADCAST` (pr_cmds.c `WriteDest`): the only message destination this
/// headless server realises (the unreliable broadcast datagram all temp
/// entities use). Writes to `MSG_ONE`/`MSG_ALL`/`MSG_INIT` are ignored.
const MSG_BROADCAST: i32 = 0;

// TE_* type bytes (protocol.h), as written after the svc_temp_entity byte.
const TE_SPIKE: u8 = 0;
const TE_SUPERSPIKE: u8 = 1;
const TE_GUNSHOT: u8 = 2;
const TE_EXPLOSION: u8 = 3;
const TE_TAREXPLOSION: u8 = 4;
const TE_LIGHTNING1: u8 = 5;
const TE_LIGHTNING2: u8 = 6;
const TE_WIZSPIKE: u8 = 7;
const TE_KNIGHTSPIKE: u8 = 8;
const TE_LIGHTNING3: u8 = 9;
const TE_LAVASPLASH: u8 = 10;
const TE_TELEPORT: u8 = 11;
const TE_EXPLOSION2: u8 = 12;
const TE_BEAM: u8 = 13;

/// `EF_MUZZLEFLASH` (`quakedef.h`): the firing entity emits a brief, bright
/// forward-offset light (`CL_RelinkEntities`).
pub const EF_MUZZLEFLASH: i32 = 2;
/// `EF_BRIGHTLIGHT`: a large light at the entity (+16 z).
pub const EF_BRIGHTLIGHT: i32 = 4;
/// `EF_DIMLIGHT`: a medium light at the entity origin (e.g. the player while
/// quad-damage or with the lightning gun warming).
pub const EF_DIMLIGHT: i32 = 8;

/// The `TE_*` type bytes (protocol.h), re-exported for front-ends that map a
/// [`TempEntityEvent::te_type`] to an effect (the playtest/wasm callers). These
/// are the same byte values the QuakeC writes after `svc_temp_entity`.
pub mod te_consts {
    /// Spike hitting a wall (nail impact): a small `R_RunParticleEffect` burst.
    pub const TE_SPIKE: u8 = super::TE_SPIKE;
    /// Super-spike (super-nail) wall impact: a larger burst.
    pub const TE_SUPERSPIKE: u8 = super::TE_SUPERSPIKE;
    /// Bullet hitting a wall: a medium burst.
    pub const TE_GUNSHOT: u8 = super::TE_GUNSHOT;
    /// Rocket/grenade explosion: a 1024-particle fiery explosion + sound.
    pub const TE_EXPLOSION: u8 = super::TE_EXPLOSION;
    /// Tarbaby explosion: treated as an explosion + sound.
    pub const TE_TAREXPLOSION: u8 = super::TE_TAREXPLOSION;
    /// Lightning bolt beam (bolt.mdl).
    pub const TE_LIGHTNING1: u8 = super::TE_LIGHTNING1;
    /// Lightning bolt beam (bolt2.mdl).
    pub const TE_LIGHTNING2: u8 = super::TE_LIGHTNING2;
    /// Wizard spike wall impact: a green-ish burst.
    pub const TE_WIZSPIKE: u8 = super::TE_WIZSPIKE;
    /// Knight spike wall impact.
    pub const TE_KNIGHTSPIKE: u8 = super::TE_KNIGHTSPIKE;
    /// Lightning bolt beam (bolt3.mdl).
    pub const TE_LIGHTNING3: u8 = super::TE_LIGHTNING3;
    /// Lava splash (a Chthon attack): approximated as an upward burst.
    pub const TE_LAVASPLASH: u8 = super::TE_LAVASPLASH;
    /// Teleport splash: approximated as an upward burst.
    pub const TE_TELEPORT: u8 = super::TE_TELEPORT;
    /// Colour-mapped explosion: a 1024-particle explosion + sound.
    pub const TE_EXPLOSION2: u8 = super::TE_EXPLOSION2;
    /// Grappling-hook beam (beam.mdl).
    pub const TE_BEAM: u8 = super::TE_BEAM;
}

/// One decoded broadcast temp entity (the `CL_ParseTEnt` payload), captured for
/// a front-end instead of spawning a client-side particle effect directly.
///
/// `pos` is the effect origin (the three `WriteCoord`s). For [`TE_EXPLOSION2`]
/// (`te_type == 12`) `color_start`/`color_length` carry the two trailing colour
/// bytes; for every other type they are `0`. Beam types (`TE_LIGHTNING1/2/3`,
/// `TE_BEAM`) carry the owning entity number in `entity`, the *start* point in
/// `pos` and the *end* point in `end` — a front-end feeds those three to
/// [`crate::tent::Beams::parse_beam`] (the `CL_ParseBeam` slot store) to render
/// the bolt; for every non-beam type `entity` is `0` and `end` equals `pos`.
#[derive(Debug, Clone, PartialEq)]
pub struct TempEntityEvent {
    /// The `TE_*` type byte (e.g. `3` = explosion, `2` = gunshot).
    pub te_type: u8,
    /// The effect origin (the three `WriteCoord` values; the beam START point).
    pub pos: [f32; 3],
    /// Beam types: the END point (the trailing three `WriteCoord`s). Non-beam
    /// types carry no end point — set equal to `pos`.
    pub end: [f32; 3],
    /// Beam types: the owning entity number (the `WriteEntity` short before the
    /// coords) — `CL_ParseBeam`'s slot-reuse key. `0` for non-beam types.
    pub entity: i32,
    /// `TE_EXPLOSION2` colour-ramp start index; `0` for other types.
    pub color_start: u8,
    /// `TE_EXPLOSION2` colour-ramp length; `0` for other types.
    pub color_length: u8,
}

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

/// What payload shape a recognised `TE_*` type expects, so the decoder consumes
/// exactly the right fields and stays byte-synchronised with the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TePayload {
    /// Three `WriteCoord`s, then emit (spikes, gunshots, explosions, splashes).
    Coords3,
    /// Three `WriteCoord`s then two colour bytes, then emit (`TE_EXPLOSION2`).
    Coords3ThenTwoBytes,
    /// A `short` entity index then six `WriteCoord`s (start+end), then emit with
    /// no effect mapping (the beam/lightning types — consumed to stay in sync).
    Beam,
}

/// Map a `TE_*` type byte to its payload shape, or `None` for an unknown type
/// (the decoder then resets to `Idle`, dropping the in-progress message rather
/// than guessing a length and corrupting every later write).
fn te_payload(te_type: u8) -> Option<TePayload> {
    match te_type {
        TE_SPIKE | TE_SUPERSPIKE | TE_GUNSHOT | TE_EXPLOSION | TE_TAREXPLOSION
        | TE_WIZSPIKE | TE_KNIGHTSPIKE | TE_LAVASPLASH | TE_TELEPORT => Some(TePayload::Coords3),
        TE_EXPLOSION2 => Some(TePayload::Coords3ThenTwoBytes),
        TE_LIGHTNING1 | TE_LIGHTNING2 | TE_LIGHTNING3 | TE_BEAM => Some(TePayload::Beam),
        _ => None,
    }
}

/// The temp-entity decoder state. `Idle` between messages; `InMessage` while
/// collecting a recognised temp entity's payload.
#[derive(Debug, Clone, PartialEq)]
enum TeState {
    /// Not inside a temp-entity message. The next `WriteByte(MSG_BROADCAST, 23)`
    /// opens one; any other broadcast write is ignored here.
    Idle,
    /// Inside a temp entity: the `svc_temp_entity` byte was seen.
    InMessage {
        /// The `TE_*` type byte once read (`None` while awaiting it), with its
        /// resolved payload shape.
        ty: Option<(u8, TePayload)>,
        /// `WriteCoord` values collected so far (bounded — see the decoder).
        coords: Vec<f32>,
        /// Trailing colour bytes collected so far (`TE_EXPLOSION2`, bounded).
        bytes: Vec<u8>,
        /// For [`TePayload::Beam`], the leading `short` (the beam's owning
        /// entity index) once consumed (`None` while awaiting it).
        beam_entity: Option<i32>,
    },
}

thread_local! {
    /// The temp-entity decoder state machine (per-thread). Driven by the Write*
    /// builtins; reset to [`TeState::Idle`] whenever a message completes, an
    /// unknown type is seen, or a new frame begins ([`Server::run_frame`] /
    /// [`Server::client_frame`] reset it via [`reset_temp_entity_decoder`]).
    static TE_STATE: std::cell::RefCell<TeState> =
        const { std::cell::RefCell::new(TeState::Idle) };
    /// Completed temp-entity events awaiting a [`Server::drain_temp_entities`].
    /// Mirrors the [`SOUND_EVENTS`]/[`PARTICLE_BURSTS`] queues exactly.
    static TEMP_ENTITIES: std::cell::RefCell<Vec<TempEntityEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The largest number of `WriteCoord`/colour-byte fields any modelled temp
/// entity carries (the beam types: 6 coords). A hard cap on the collected vecs
/// so a malformed stream can never grow them without bound.
const TE_MAX_COORDS: usize = 6;

/// Push a completed temp-entity event onto the thread-local queue.
fn push_temp_entity(ev: TempEntityEvent) {
    TEMP_ENTITIES.with(|q| q.borrow_mut().push(ev));
}

/// Take and clear every queued temp-entity event.
fn take_temp_entities() -> Vec<TempEntityEvent> {
    TEMP_ENTITIES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Reset the decoder to [`TeState::Idle`], dropping any half-collected message.
/// Called at the start of each server frame so a partial temp entity left by an
/// errored think never bleeds into the next frame's writes.
fn reset_temp_entity_decoder() {
    TE_STATE.with(|s| *s.borrow_mut() = TeState::Idle);
}

/// Feed one `WriteByte`/`WriteShort`/`WriteCoord`/… value to the decoder.
///
/// `dest` is the message destination (`PARM0`); only `MSG_BROADCAST` is decoded,
/// every other destination is ignored (the C routed those to a specific client's
/// reliable buffer, which this headless server has no client for). `field` says
/// which kind of write this is so the decoder treats a coord as a coordinate, a
/// byte/short as an integer payload field, etc.
///
/// The state machine is deliberately total: an unrecognised `TE_*` type, an
/// over-long field run, or any unexpected write simply resets to `Idle` (drops
/// the in-progress message) rather than panicking or guessing.
fn te_feed(dest: i32, field: TeField, value: f32) {
    if dest != MSG_BROADCAST {
        return; // not a broadcast temp entity — ignore (no client buffers here).
    }
    TE_STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        match &mut *st {
            // --- between messages: only a WriteByte of svc_temp_entity opens one.
            TeState::Idle => {
                if matches!(field, TeField::Byte) && (value as i32) == SVC_TEMP_ENTITY as i32 {
                    *st = TeState::InMessage {
                        ty: None,
                        coords: Vec::new(),
                        bytes: Vec::new(),
                        beam_entity: None,
                    };
                }
                // Any other broadcast write outside a message is ignored.
            }

            // --- inside a temp entity.
            TeState::InMessage {
                ty,
                coords,
                bytes,
                beam_entity,
            } => {
                // Awaiting the TE_* type byte (the second WriteByte).
                if ty.is_none() {
                    if matches!(field, TeField::Byte) {
                        let tb = value as i32;
                        // Out-of-byte-range or unknown type => drop the message.
                        let te_type = if (0..=255).contains(&tb) { tb as u8 } else { 255 };
                        match te_payload(te_type) {
                            Some(shape) => *ty = Some((te_type, shape)),
                            None => *st = TeState::Idle,
                        }
                    } else {
                        // A non-byte write where the type was expected: desync; reset.
                        *st = TeState::Idle;
                    }
                    return;
                }

                let (te_type, shape) = ty.expect("ty is Some here");
                match shape {
                    TePayload::Coords3 => {
                        if matches!(field, TeField::Coord) && coords.len() < TE_MAX_COORDS {
                            coords.push(value);
                        }
                        if coords.len() == 3 {
                            let pos = [coords[0], coords[1], coords[2]];
                            push_temp_entity(TempEntityEvent {
                                te_type,
                                pos,
                                end: pos,
                                entity: 0,
                                color_start: 0,
                                color_length: 0,
                            });
                            *st = TeState::Idle;
                        }
                    }
                    TePayload::Coords3ThenTwoBytes => {
                        if coords.len() < 3 {
                            if matches!(field, TeField::Coord) && coords.len() < TE_MAX_COORDS {
                                coords.push(value);
                            }
                        } else if matches!(field, TeField::Byte) && bytes.len() < 2 {
                            bytes.push((value as i32).clamp(0, 255) as u8);
                        }
                        if coords.len() == 3 && bytes.len() == 2 {
                            let pos = [coords[0], coords[1], coords[2]];
                            push_temp_entity(TempEntityEvent {
                                te_type,
                                pos,
                                end: pos,
                                entity: 0,
                                color_start: bytes[0],
                                color_length: bytes[1],
                            });
                            *st = TeState::Idle;
                        }
                    }
                    TePayload::Beam => {
                        // short (entity index) first, then 6 coords (start+end).
                        if beam_entity.is_none() {
                            if matches!(field, TeField::Short | TeField::Entity) {
                                *beam_entity = Some(value as i32);
                            }
                            // (A stray coord before the short is ignored; the
                            // writer always emits the short first.)
                        } else if matches!(field, TeField::Coord) && coords.len() < TE_MAX_COORDS {
                            coords.push(value);
                        }
                        if let (Some(entity), true) = (*beam_entity, coords.len() == 6) {
                            // START point in pos, END point in end, plus the owning
                            // entity — everything CL_ParseBeam needs for its slot
                            // store (crate::tent::Beams).
                            push_temp_entity(TempEntityEvent {
                                te_type,
                                pos: [coords[0], coords[1], coords[2]],
                                end: [coords[3], coords[4], coords[5]],
                                entity,
                                color_start: 0,
                                color_length: 0,
                            });
                            *st = TeState::Idle;
                        }
                    }
                }
            }
        }
    });
}

/// Which `Write*` builtin produced a decoder field — lets [`te_feed`] tell a
/// coordinate from an integer payload byte/short so it consumes the right shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeField {
    /// `WriteByte`/`WriteChar` — an 8-bit integer field (svc byte, TE type byte,
    /// or `TE_EXPLOSION2`'s two colour bytes).
    Byte,
    /// `WriteShort`/`WriteLong` — a 16/32-bit integer field (the beam entity index).
    Short,
    /// `WriteCoord`/`WriteAngle` — a world coordinate (captured as the raw float).
    Coord,
    /// `WriteEntity` — an entity index short (treated like [`TeField::Short`]).
    Entity,
}

// ---------------------------------------------------------------------------
// MSG_ALL server-command recognizer (svc_intermission / svc_finale / ...).
//
// The C `PF_WriteByte`/`PF_WriteString` route a `MSG_ALL` destination into
// `sv.reliable_datagram`, which every client's `CL_ParseServerMessage`
// (cl_parse.c) later reads back as `svc_*` commands. The vanilla progs writes
// exactly these commands to MSG_ALL: `svc_killedmonster`/`svc_foundsecret`
// (one byte, no payload — the engine reads the kill/secret counts from the
// QuakeC globals directly, so these are recognised but not surfaced),
// `svc_intermission` (no payload), `svc_finale` (+ one `WriteString`),
// `svc_cdtrack` (+ two payload bytes: track, looptrack) and `svc_sellscreen`
// (no payload); mission packs add `svc_cutscene` (+ string). This headless
// server has no datagram, so — exactly like the temp-entity decoder above —
// the Write* builtins feed a tiny recognizer whose completed commands queue as
// [`SvcEvent`]s until [`Server::drain_svc_events`] hands them to the front-end
// (which plays the client role: intermission camera, finale text, stats overlay).
// ---------------------------------------------------------------------------

/// `MSG_ALL` (pr_cmds.c `WriteDest`): the reliable broadcast message every
/// client receives — the destination of the intermission/finale/stat commands.
const MSG_ALL: i32 = 2;

// The `svc_*` command bytes (protocol.h) the vanilla progs writes to MSG_ALL.
const SVC_KILLEDMONSTER: u8 = 27;
const SVC_FOUNDSECRET: u8 = 28;
const SVC_INTERMISSION: u8 = 30;
const SVC_FINALE: u8 = 31;
const SVC_CDTRACK: u8 = 32;
const SVC_SELLSCREEN: u8 = 33;
const SVC_CUTSCENE: u8 = 34;

/// One recognised MSG_ALL server command, surfaced to the front-end the way the
/// client's `CL_ParseServerMessage` (cl_parse.c) would have acted on it.
#[derive(Debug, Clone, PartialEq)]
pub enum SvcEvent {
    /// `svc_intermission` (30): the level ended — the C set `cl.intermission = 1`,
    /// latched `cl.completed_time = cl.time` and went full-screen for the
    /// intermission camera + `Sbar_IntermissionOverlay` stats.
    Intermission,
    /// `svc_finale` (31) + its `WriteString` payload: episode-end text — the C set
    /// `cl.intermission = 2` and `SCR_CenterPrint`ed the string (slow char reveal).
    Finale(String),
    /// `svc_cutscene` (34) + its string: `cl.intermission = 3` (text only, no
    /// plaque). Unused by the vanilla progs (mission packs use it).
    Cutscene(String),
    /// `svc_sellscreen` (33): the shareware "order the full game" pitch — the C ran
    /// `Cmd_ExecuteString("help")`, i.e. opened the Help/Ordering pages.
    SellScreen,
}

/// The MSG_ALL recognizer state. Like [`TeState`], deliberately total: any
/// unexpected write resets to `Idle` rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SvcAllState {
    /// Between commands: the next `WriteByte` is an `svc_*` command byte.
    Idle,
    /// `svc_finale`/`svc_cutscene` seen; awaiting the `WriteString` payload.
    /// `cutscene` distinguishes which event to emit.
    AwaitString { cutscene: bool },
    /// `svc_cdtrack` seen; the next N `WriteByte`s (track, looptrack) are payload
    /// and must be consumed so they are not mistaken for command bytes.
    SkipBytes(u8),
}

thread_local! {
    /// The MSG_ALL recognizer state (per-thread, like [`TE_STATE`]). Reset at the
    /// top of each server frame and in [`Server::with_pak`].
    static SVC_ALL_STATE: std::cell::RefCell<SvcAllState> =
        const { std::cell::RefCell::new(SvcAllState::Idle) };
    /// Completed MSG_ALL commands awaiting a [`Server::drain_svc_events`].
    static SVC_EVENTS: std::cell::RefCell<Vec<SvcEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Push a completed MSG_ALL command onto the thread-local queue.
fn push_svc_event(ev: SvcEvent) {
    SVC_EVENTS.with(|q| q.borrow_mut().push(ev));
}

/// Take and clear every queued MSG_ALL command.
fn take_svc_events() -> Vec<SvcEvent> {
    SVC_EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Reset the recognizer to `Idle`, dropping any half-collected command. Called
/// at the start of each server frame (next to [`reset_temp_entity_decoder`]) so
/// a partial command left by an errored think never bleeds into the next frame.
fn reset_svc_recognizer() {
    SVC_ALL_STATE.with(|s| *s.borrow_mut() = SvcAllState::Idle);
}

/// Feed one `WriteByte`/`WriteChar` value to the MSG_ALL recognizer (a no-op for
/// any other destination). Unknown command bytes are ignored where the real
/// stream would carry their payload too — the vanilla progs only ever writes the
/// commands modelled here, so anything else is simply dropped, never panicking.
fn svc_all_feed_byte(dest: i32, value: f32) {
    if dest != MSG_ALL {
        return;
    }
    SVC_ALL_STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        match *st {
            SvcAllState::Idle => {
                let b = value as i32;
                match if (0..=255).contains(&b) { b as u8 } else { 0 } {
                    SVC_INTERMISSION => push_svc_event(SvcEvent::Intermission),
                    SVC_FINALE => *st = SvcAllState::AwaitString { cutscene: false },
                    SVC_CUTSCENE => *st = SvcAllState::AwaitString { cutscene: true },
                    SVC_CDTRACK => *st = SvcAllState::SkipBytes(2),
                    SVC_SELLSCREEN => push_svc_event(SvcEvent::SellScreen),
                    // Stat ticks: the front-end reads killed_monsters /
                    // found_secrets from the QuakeC globals (like the Tab
                    // scoreboard), so these single-byte commands need no event.
                    SVC_KILLEDMONSTER | SVC_FOUNDSECRET => {}
                    _ => {} // unknown command byte: ignore (stay Idle).
                }
            }
            SvcAllState::AwaitString { .. } => {
                // A byte where the string was expected: desync; drop the command.
                *st = SvcAllState::Idle;
            }
            SvcAllState::SkipBytes(n) => {
                *st = if n <= 1 { SvcAllState::Idle } else { SvcAllState::SkipBytes(n - 1) };
            }
        }
    });
}

/// Feed one `WriteString` value to the MSG_ALL recognizer (a no-op for any other
/// destination): completes a pending `svc_finale`/`svc_cutscene`.
fn svc_all_feed_string(dest: i32, text: String) {
    if dest != MSG_ALL {
        return;
    }
    SVC_ALL_STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let SvcAllState::AwaitString { cutscene } = *st {
            push_svc_event(if cutscene {
                SvcEvent::Cutscene(text)
            } else {
                SvcEvent::Finale(text)
            });
        }
        // A string outside AwaitString is not part of any modelled command; either
        // way the recognizer returns to Idle.
        *st = SvcAllState::Idle;
    });
}

/// Feed a non-byte, non-string write to the recognizer: no modelled MSG_ALL
/// command carries one, so it can only mean desync — reset to `Idle`.
fn svc_all_feed_other(dest: i32) {
    if dest != MSG_ALL {
        return;
    }
    reset_svc_recognizer();
}

/// `PF_WriteByte` (#52): `void(float to, float value)`. Feeds the decoder an
/// 8-bit field. The C did `MSG_WriteByte(WriteDest(), G_FLOAT(PARM1))`; here the
/// destination is `PARM0` and the value `PARM1`.
fn bi_writebyte(vm: &mut Vm) -> Result<()> {
    te_feed(vm.arg_float(0) as i32, TeField::Byte, vm.arg_float(1));
    svc_all_feed_byte(vm.arg_float(0) as i32, vm.arg_float(1));
    Ok(())
}

/// `PF_WriteChar` (#53): like [`bi_writebyte`] (an 8-bit field). No temp entity
/// uses a char field, but it is decoded as a byte so the stream stays in sync.
fn bi_writechar(vm: &mut Vm) -> Result<()> {
    te_feed(vm.arg_float(0) as i32, TeField::Byte, vm.arg_float(1));
    svc_all_feed_byte(vm.arg_float(0) as i32, vm.arg_float(1));
    Ok(())
}

/// `PF_WriteShort` (#54): a 16-bit integer field (the beam types' entity index).
fn bi_writeshort(vm: &mut Vm) -> Result<()> {
    te_feed(vm.arg_float(0) as i32, TeField::Short, vm.arg_float(1));
    svc_all_feed_other(vm.arg_float(0) as i32);
    Ok(())
}

/// `PF_WriteLong` (#55): a 32-bit integer field. Decoded like a short (no temp
/// entity carries a long, but it keeps the stream synchronised if one appears).
fn bi_writelong(vm: &mut Vm) -> Result<()> {
    te_feed(vm.arg_float(0) as i32, TeField::Short, vm.arg_float(1));
    svc_all_feed_other(vm.arg_float(0) as i32);
    Ok(())
}

/// `PF_WriteCoord` (#56): a world coordinate. The C round-tripped through a lossy
/// `*8` short; we capture the float value directly (the task notes this is fine).
fn bi_writecoord(vm: &mut Vm) -> Result<()> {
    te_feed(vm.arg_float(0) as i32, TeField::Coord, vm.arg_float(1));
    svc_all_feed_other(vm.arg_float(0) as i32);
    Ok(())
}

/// `PF_WriteAngle` (#57): an angle byte. No modelled temp entity carries one;
/// decoded as a coordinate field would be wrong, so it is treated as a [`TeField::Coord`]
/// only for the (unused-by-temp-entities) angle slot — in practice angles never
/// appear inside a temp-entity burst, so this just stays benign.
fn bi_writeangle(vm: &mut Vm) -> Result<()> {
    // Angles are not part of any temp-entity payload; feed as a byte-like field
    // so it cannot be mistaken for a coordinate (keeps Coords3 in sync if a
    // writer ever interleaved one, which the stock game never does).
    te_feed(vm.arg_float(0) as i32, TeField::Byte, vm.arg_float(1));
    svc_all_feed_other(vm.arg_float(0) as i32);
    Ok(())
}

/// `PF_WriteString` (#58): a string field. Temp entities carry no strings (the
/// broadcast decoder ignores it), but a MSG_ALL string completes a pending
/// `svc_finale`/`svc_cutscene` — the episode-end text the client's
/// `CL_ParseServerMessage` read with `MSG_ReadString` and `SCR_CenterPrint`ed.
fn bi_writestring(vm: &mut Vm) -> Result<()> {
    let dest = vm.arg_float(0) as i32;
    if dest == MSG_ALL {
        svc_all_feed_string(dest, vm.arg_string(1));
    }
    Ok(())
}

/// `PF_WriteEntity` (#59): an entity-index short — the C wrote
/// `G_EDICTNUM(OFS_PARM1)`. The beam types' leading field: their owning
/// entity number (the `Beams` slot-reuse / view-entity key). NOTE the arg is
/// an entity reference (an INT global, `arg_entity`), not a float — reading it
/// as a float would yield the f32 bit-reinterpretation of the edict index
/// (~0.0 for every real entity), collapsing all beams onto one slot.
fn bi_writeentity(vm: &mut Vm) -> Result<()> {
    te_feed(
        vm.arg_float(0) as i32,
        TeField::Entity,
        vm.arg_entity(1) as f32,
    );
    svc_all_feed_other(vm.arg_float(0) as i32);
    Ok(())
}

/// `PF_sound` (#8): `void(entity e, float chan, string sample, float vol,
/// float atten) sound`. The arg layout mirrors `PF_sound`/`SV_StartSound`:
/// `entity = PARM0`, `channel = PARM1`, `sample = PARM2`, `volume = PARM3`,
/// `attenuation = PARM4`; the emission point is the entity's box centre.
///
/// FAITHFULNESS: the C `Sys_Error`s on out-of-range volume/attenuation/channel
/// and silently drops an un-precached sample. We never abort the host on
/// program data, so instead we keep the values as given (a front-end can clamp)
/// and still queue the event even when the sample was not precached, recording
/// `sound_index = -1` so the caller can tell. The C scaled volume by 255 into a
/// packet byte; we keep the QuakeC-domain `0.0..=1.0` float for the front-end.
fn bi_sound(vm: &mut Vm) -> Result<()> {
    let entity = vm.arg_entity(0);
    let channel = vm.arg_float(1) as i32;
    let sample = vm.arg_string(2);
    let volume = vm.arg_float(3);
    let attenuation = vm.arg_float(4);

    let origin = entity_sound_origin(vm, entity);
    // Resolve the precache slot without registering a new name: SV_StartSound
    // only *looks up* an already-precached sample, dropping (here: marking -1)
    // when absent.
    let sound_index = lookup_sound_index(vm, &sample);

    push_sound_event(SoundEvent {
        entity,
        channel,
        sound_index,
        sample,
        origin,
        volume,
        attenuation,
    });
    Ok(())
}

/// `PF_ambientsound` (#74, pr_cmds.c): `void(vector pos, string sample, float
/// vol, float atten) ambientsound`. The C emitted an `svc_spawnstaticsound`
/// into the level signon at an explicit world position; the client's
/// `S_StaticSound` then ran it as a PERSISTENT looping channel. We record it as
/// a [`StaticSound`] for [`Server::drain_static_sounds`] — NOT as a one-shot
/// [`SoundEvent`] (a loop is state, not an event).
///
/// Unlike `SV_StartSound`'s path (see [`lookup_sound_index`]'s DEVIATION),
/// the precache gate here matches the C exactly: `PF_ambientsound` scans
/// `sv.sound_precache` read-only and REFUSES an un-precached sample —
/// `Con_Printf ("no precache: %s\n", samp); return;` — registering nothing.
/// The message routes to [`Vm::output`] like the `print`/`dprint` builtins.
///
/// The wire format quantized volume and attenuation into bytes
/// (`MSG_WriteByte(vol*255)` / `MSG_WriteByte(attenuation*64)`, C float→int
/// truncation); `CL_ParseStaticSound` handed those bytes to `S_StaticSound`,
/// which divided the attenuation byte back by 64. We apply the same round-trip
/// (clamped to the byte range instead of wrapping, defensively) so a front-end
/// hears exactly what the original client was told.
fn bi_ambientsound(vm: &mut Vm) -> Result<()> {
    let pos = vm.arg_vector(0);
    let sample = vm.arg_string(1);
    let volume = vm.arg_float(2);
    let attenuation = vm.arg_float(3);

    // "check to see if samp was properly precached" (pr_cmds.c:519-528).
    let Some(sound_index) = vm.with_host(|_vm, h| h.find_sound(&sample)).flatten() else {
        vm.output.push_str("no precache: ");
        vm.output.push_str(&sample);
        vm.output.push('\n');
        return Ok(());
    };

    let vol_byte = (volume * 255.0).clamp(0.0, 255.0) as u8;
    let atten_byte = (attenuation * 64.0).clamp(0.0, 255.0) as u8;
    push_static_sound(StaticSound {
        origin: pos,
        sound_index,
        sample,
        volume: vol_byte as f32 / 255.0,
        attenuation: atten_byte as f32 / 64.0,
    });
    Ok(())
}

/// Resolve `sample`'s precache slot for the one-shot [`SoundEvent`] paths
/// (`bi_sound` / the physics `start_sound`). The C `SV_StartSound` only
/// *searched* `sv.sound_precache` and dropped an un-precached sample. In
/// practice QuakeC precaches every sound during `worldspawn` before any
/// `sound()` fires, so `precache_sound` returns the existing stable slot
/// (`>= 1`) without appending. Returns `-1` only when there is no host at all.
///
/// DEVIATION: an un-precached name is registered here (and so gets a real
/// slot) rather than being dropped with a warning. The captured
/// [`SoundEvent`] still carries the raw `sample`, so a front-end is never
/// misled about what played. (`bi_ambientsound` does NOT use this: it matches
/// the C's read-only check via [`Host::find_sound`] and drops.)
fn lookup_sound_index(vm: &mut Vm, sample: &str) -> i32 {
    vm.with_host(|_vm, h| h.precache_sound(sample)).unwrap_or(-1)
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
    put(t, 21, crate::builtins::pf_stuffcmd); // stuffcmd -> svc_stufftext queue
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
// COM_Parse tokenizer.
// ---------------------------------------------------------------------------

/// A faithful port of `COM_Parse` (common.c) over `&str` bytes: skips
/// whitespace and `//` line comments, returns `"quoted strings"`, the single
/// characters `{ } ( ) ' :`, or a run of non-whitespace as one token.
/// `pub(crate)`: the savegame loader (`save.rs`) parses the `.sav` brace blocks
/// with the same tokenizer, exactly as the C shares `COM_Parse`.
pub(crate) struct Tokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(s: &'a str) -> Tokenizer<'a> {
        Tokenizer {
            data: s.as_bytes(),
            pos: 0,
        }
    }

    /// Return the next token, or `None` at end of input. Mirrors `COM_Parse`,
    /// including the `c <= ' '` whitespace test and the special single chars.
    pub(crate) fn next_token(&mut self) -> Option<String> {
        let mut token = Vec::new();

        // skip whitespace (and // comments), looping like the C `goto skipwhite`.
        loop {
            // skip whitespace
            loop {
                let c = *self.data.get(self.pos)?;
                if c > b' ' {
                    break;
                }
                self.pos += 1;
            }
            // skip // comments
            if self.data.get(self.pos) == Some(&b'/')
                && self.data.get(self.pos + 1) == Some(&b'/')
            {
                while let Some(&c) = self.data.get(self.pos) {
                    if c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }

        let c = *self.data.get(self.pos)?;

        // quoted string
        if c == b'"' {
            self.pos += 1;
            loop {
                match self.data.get(self.pos) {
                    None => break,            // EOF inside a quote: stop cleanly
                    Some(&b'"') => {
                        self.pos += 1; // consume closing quote
                        break;
                    }
                    Some(&ch) => {
                        token.push(ch);
                        self.pos += 1;
                    }
                }
            }
            return Some(String::from_utf8_lossy(&token).into_owned());
        }

        // single-character tokens
        if is_single(c) {
            self.pos += 1;
            return Some(String::from_utf8_lossy(&[c]).into_owned());
        }

        // a regular word: run of chars > 32 that aren't a single-char token.
        while let Some(&ch) = self.data.get(self.pos) {
            token.push(ch);
            self.pos += 1;
            match self.data.get(self.pos) {
                Some(&nc) if is_single(nc) => break,
                Some(&nc) if nc > 32 => continue,
                _ => break,
            }
        }
        Some(String::from_utf8_lossy(&token).into_owned())
    }
}

/// The C `COM_Parse` single-character set: `{ } ( ) ' :`.
fn is_single(c: u8) -> bool {
    matches!(c, b'{' | b'}' | b')' | b'(' | b'\'' | b':')
}

/// `ED_NewString` (pr_edict.c): copy the raw value, translating `\n` to a
/// newline and any other `\x` escape to a literal backslash. `pub(crate)` for
/// the savegame loader's `ED_ParseEpair` (the C shares this helper too).
pub(crate) fn ed_new_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i < bytes.len() - 1 {
            i += 1;
            if bytes[i] == b'n' {
                out.push('\n');
            } else {
                out.push('\\');
            }
        } else {
            out.push(c as char);
        }
        i += 1;
    }
    out
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

    /// The raw light-style pattern string at index `style`, or `""` for an unset
    /// or out-of-range index. (Mostly for inspection / tests; the renderer wants
    /// [`Self::lightstyle_scales`].)
    pub fn lightstyle(&self, style: usize) -> &str {
        self.lightstyles.get(style).map(String::as_str).unwrap_or("")
    }

    /// `R_AnimateLight` (r_light.c): the per-style brightness scale at game `time`,
    /// one entry per `MAX_LIGHTSTYLES` style index, ready to pass to
    /// [`crate::render::render_scene_ext`].
    ///
    /// For style `j` with pattern string of length `L`:
    /// * `L == 0` (unset) → scale `1.0` (the C `d_lightstylevalue = 256`, i.e.
    ///   "normal"). Treating a missing style as normal keeps faces that reference
    ///   an unset style at full brightness rather than going dark.
    /// * else the string animates at 10 chars/sec: `k = floor(time*10) mod L`,
    ///   `ch = string[k]`, and the C `d_lightstylevalue[j] = (ch - 'a') * 22`
    ///   (so `'a'` → 0 = dark, `'m'` → 264 = normal, `'z'` → 550 ≈ double-bright).
    ///
    /// The C renders `luxel * d_lightstylevalue` against a constant `255 * 256`
    /// white point. This renderer stores luxels in `0..=255` and applies a
    /// multiplicative factor, so we normalise the style value by id's `256` white
    /// point: `scale = (ch - 'a') * 22 / 256`. Then `'m'` → `264/256 = 1.03125`
    /// (exactly id's steady-world brightness — normalising by `'m'` itself made the
    /// whole static-lit world ~1 colormap row too dark). An UNSET style still maps
    /// to `1.0` (R_AnimateLight's `length == 0` default of 256).
    pub fn lightstyle_scales(&self, time: f32) -> [f32; MAX_LIGHTSTYLES] {
        // Delegates to the shared table-driven helper so demo playback (the
        // recorded svc_lightstyle table) animates through the IDENTICAL logic.
        lightstyle_scales_at(&self.lightstyles, time)
    }

    /// The number of live (not-free) edicts, including the world (edict 0).
    pub fn live_entities(&self) -> usize {
        self.vm
            .edict_free
            .iter()
            .filter(|&&free| !free)
            .count()
    }

    /// `ED_LoadFromFile` (pr_edict.c): tokenize `bsp.entities`, spawn each
    /// entity, set its fields by name, and call its spawn function (named by
    /// `classname`). Faithful to the C control flow, but a per-entity spawn
    /// error is caught and counted rather than aborting the whole load.
    pub fn spawn_entities(&mut self) -> Result<SpawnReport> {
        // SV_SpawnServer: current_skill = (int)(skill.value + 0.5), clamped to
        // 0..3, then Cvar_SetValue("skill", current_skill). Re-normalise the live
        // skill the same way before loading entities so a fractional value a
        // front-end set (or a portal's cvar_set) is rounded to the integer the
        // spawn filter compares against, and cvar("skill") reads back the
        // canonical value.
        set_skill_value(skill_value() as f32);

        // The entity text was captured at construction (the host has no accessor
        // and we never downcast). Clone it so the tokenizer borrow does not pin
        // `&self`, leaving the VM free to mutate during spawning.
        let entities = self.entities.clone();

        let mut report = SpawnReport::default();
        let mut classname_counts: Vec<(String, usize)> = Vec::new();
        let time = self.time();

        let mut tok = Tokenizer::new(&entities);
        let mut first = true;

        // Loop over entity blocks until EOF (next_token() returns None).
        while let Some(open) = tok.next_token() {
            if open != "{" {
                // C: Sys_Error("found %s when expecting {"). Stay total: stop.
                break;
            }

            report.total += 1;

            // First entity is the world (edict 0); subsequent are ED_Alloc'd.
            let ent = if first {
                first = false;
                0
            } else {
                self.vm.spawn()
            };

            // Parse the key/value pairs into this edict.
            self.parse_edict(&mut tok, ent)?;

            // Skill / deathmatch filtering (ED_LoadFromFile, pr_edict.c). In
            // deathmatch, drop NOT_DEATHMATCH entities; otherwise drop the entity
            // whose NOT_<difficulty> flag matches the current skill (easy=0,
            // medium=1, hard/nightmare>=2). `current_skill` is the live `skill`
            // cvar (see [`SKILL`]) — a difficulty portal's `cvar_set("skill", N)`
            // changes which monsters/items this filter keeps.
            let spawnflags = self.vm.ent_get_float(ent, "spawnflags") as i32;
            let deathmatch = cvar_value("deathmatch") != 0.0;
            let current_skill = skill_value();
            let inhibited = if deathmatch {
                spawnflags & SPAWNFLAG_NOT_DEATHMATCH != 0
            } else {
                (current_skill == 0 && spawnflags & SPAWNFLAG_NOT_EASY != 0)
                    || (current_skill == 1 && spawnflags & SPAWNFLAG_NOT_MEDIUM != 0)
                    || (current_skill >= 2 && spawnflags & SPAWNFLAG_NOT_HARD != 0)
            };
            if inhibited {
                self.vm.free_edict(ent);
                report.inhibited += 1;
                continue;
            }

            // classname -> spawn function.
            let classname = self.vm.ent_get_string(ent, "classname");
            if classname.is_empty() {
                // C: "No classname" -> free and continue.
                self.vm.free_edict(ent);
                continue;
            }
            bump_classname(&mut classname_counts, &classname);

            let func = self.vm.progs.find_function(&classname);
            let Some(func) = func else {
                report.no_spawn_function += 1;
                self.vm.free_edict(ent);
                continue;
            };

            // self = ent, other = world, time = current; then execute. The host
            // is PRESENT here (we are not inside with_host).
            self.vm.gset_int("self", ent);
            self.vm.gset_int("other", 0);
            self.vm.gset_float("time", time);

            match self.vm.execute(func) {
                Ok(()) => report.spawned += 1,
                Err(_) => {
                    // C aborted via Host_Error; we keep loading the rest, but
                    // must reset the interpreter so the faulted call chain does
                    // not corrupt the next spawn.
                    report.spawn_errors += 1;
                    self.vm.reset_execution();
                }
            }
        }

        // Worldspawn (and any other spawn function) may have called lightstyle();
        // pull those patterns out of the write transport into the owned table.
        self.lightstyles = snapshot_lightstyles();

        // SV_SpawnServer: "run two frames to allow everything to settle" with
        // host_frametime = 0.1. The first frame fires each entity's spawn-set
        // `nextthink` (e.g. monsters droptofloor / set their first animation
        // frame, items settle onto the floor) so the world is in its resting
        // initial state before play begins. A per-entity think fault is isolated
        // by run_frame (it never aborts the load).
        let _ = self.run_frame(SETTLE_FRAMETIME);
        let _ = self.run_frame(SETTLE_FRAMETIME);

        // classnames sorted by count desc, then name asc for determinism.
        classname_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        report.classnames = classname_counts;
        Ok(report)
    }

    /// `ED_ParseEdict` (pr_edict.c): read key/value pairs until `}`, applying the
    /// `angle`/`light`/leading-`_` key hacks, and set each field by its def type.
    fn parse_edict(&mut self, tok: &mut Tokenizer, ent: i32) -> Result<()> {
        // Parse key (or closing brace); EOF here is C's Sys_Error, we stay total.
        while let Some(key) = tok.next_token() {
            if key == "}" {
                break;
            }

            // anglehack: "angle" -> key "angles", value rewritten "0 <v> 0".
            let anglehack = key == "angle";
            // "light" -> "light_lev"; trailing-space trim on the key name.
            let mut keyname = if key == "angle" {
                "angles".to_string()
            } else if key == "light" {
                "light_lev".to_string()
            } else {
                key
            };
            while keyname.ends_with(' ') {
                keyname.pop();
            }

            // parse value
            let value = match tok.next_token() {
                Some(t) => t,
                None => break,
            };
            if value == "}" {
                // C: Sys_Error("closing brace without data"). Stay total.
                break;
            }

            // leading underscore keys are utility comments, discarded.
            if keyname.starts_with('_') {
                continue;
            }

            let value = if anglehack {
                format!("0 {value} 0")
            } else {
                value
            };

            // Set the field by name using its def TYPE; unknown keys are skipped
            // (the C printed "'%s' is not a field" and continued).
            self.set_field(ent, &keyname, &value);
        }
        Ok(())
    }

    /// `ED_ParseEpair` (pr_edict.c): write `value` into entity `ent`'s field
    /// `keyname` according to the field def's type. Unknown fields are silently
    /// skipped (the C continued past them).
    fn set_field(&mut self, ent: i32, keyname: &str, value: &str) {
        // Copy the def's ofs/type out so the immutable `progs` borrow ends before
        // we mutate the VM (intern / set_e*). A missing field is skipped.
        let (ofs, etype) = match self.vm.progs.find_field(keyname) {
            Some(def) => (def.ofs as usize, def.etype()),
            None => return, // not a field — skip (C: "is not a field")
        };
        match etype {
            EType::String => {
                let interned = ed_new_string(value);
                let s_t = self.vm.intern(&interned);
                self.vm.set_ei(ent, ofs, s_t);
            }
            EType::Float => {
                let f = parse_float(value);
                self.vm.set_ef(ent, ofs, f);
            }
            EType::Vector => {
                let v = parse_vector(value);
                self.vm.set_ev(ent, ofs, v);
            }
            EType::Entity => {
                let n = parse_int(value);
                self.vm.set_ei(ent, ofs, n);
            }
            EType::Field => {
                // ev_field: store the ofs of the named field (G_INT(def->ofs)).
                let target_ofs = self.vm.progs.find_field(value).map(|d| d.ofs as i32);
                if let Some(target_ofs) = target_ofs {
                    self.vm.set_ei(ent, ofs, target_ofs);
                }
            }
            EType::Function => {
                // ev_function: store the function index found by name.
                let fnum = self.vm.progs.find_function(value);
                if let Some(fnum) = fnum {
                    self.vm.set_ei(ent, ofs, fnum as i32);
                }
            }
            // ev_void / ev_pointer: nothing to store.
            EType::Void | EType::Pointer => {}
        }
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

    /// `SV_StartSound` (sv_phys.c helper, via `world.c`): queue a sound emitted by
    /// `ent` on `channel` with the named `sample`. `volume_byte` is the C 0..255
    /// byte (255 = full); we store it back in the QuakeC `0.0..=1.0` domain the
    /// [`SoundEvent`] queue uses. Used by the toss/step physics for the
    /// water-entry splash and the landing thud (the C calls these directly, not
    /// through the QuakeC `sound` builtin).
    fn start_sound(&mut self, ent: i32, channel: i32, sample: &str, volume_byte: i32, attenuation: f32) {
        let origin = entity_sound_origin(&self.vm, ent);
        let sound_index = lookup_sound_index(&mut self.vm, sample);
        push_sound_event(SoundEvent {
            entity: ent,
            channel,
            sound_index,
            sample: sample.to_string(),
            origin,
            volume: (volume_byte as f32) / 255.0,
            attenuation,
        });
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

    /// `SV_SetIdealPitch` (sv_user.c:53): trace six 12-unit forward steps along
    /// the player's yaw, sampling the floor height under each; if the steps form
    /// a consistent staircase (a single, sign-stable height delta over at least
    /// two steps), set `idealpitch = -dir * sv_idealpitchscale` so the QuakeC view
    /// code can auto-centre the pitch to look up/down the stairs. Only runs while
    /// the player is `FL_ONGROUND` (the C returns early otherwise).
    ///
    /// Faithful transcription over the entity-aware [`sv_move`] (the C
    /// `SV_Move(top, 0, 0, bottom, MOVE_NOMONSTERS, sv_player)`). `idealpitch` is
    /// left untouched on a wall / dropoff (the C returns without clearing it),
    /// zeroed on flat ground, and set to the scaled slope otherwise.
    fn set_ideal_pitch(&mut self, ent: i32) {
        const MAX_FORWARD: usize = 6;
        const ON_EPSILON: f32 = 0.1;
        /// `sv_idealpitchscale` default ("0.8").
        const SV_IDEALPITCHSCALE: f32 = 0.8;

        if (self.vm.ent_get_float(ent, "flags") as i32) & FL_ONGROUND == 0 {
            return;
        }

        let origin = self.vm.ent_get_vector(ent, "origin");
        let view_ofs = self.vm.ent_get_vector(ent, "view_ofs");
        let yaw = self.vm.ent_get_vector(ent, "angles")[crate::math::YAW];
        let angleval = f64::from(yaw) * std::f64::consts::PI * 2.0 / 360.0;
        let sinval = angleval.sin() as f32;
        let cosval = angleval.cos() as f32;

        let mut z = [0.0f32; MAX_FORWARD];
        for (i, zi) in z.iter_mut().enumerate() {
            let top = [
                origin[0] + cosval * ((i + 3) as f32) * 12.0,
                origin[1] + sinval * ((i + 3) as f32) * 12.0,
                origin[2] + view_ofs[2],
            ];
            let bottom = [top[0], top[1], top[2] - 160.0];

            // SV_Move(top, 0, 0, bottom, MOVE_NOMONSTERS, sv_player).
            let tr = sv_move(&mut self.vm, top, bottom, [0.0; 3], [0.0; 3], ent, true, false);
            if tr.allsolid {
                return; // looking at a wall, leave ideal the way it was
            }
            if tr.fraction == 1.0 {
                return; // near a dropoff
            }
            *zi = top[2] + tr.fraction * (bottom[2] - top[2]);
        }

        let mut dir = 0.0f32;
        let mut steps = 0i32;
        for j in 1..MAX_FORWARD {
            let step = z[j] - z[j - 1];
            if step > -ON_EPSILON && step < ON_EPSILON {
                continue;
            }
            if dir != 0.0 && (step - dir > ON_EPSILON || step - dir < -ON_EPSILON) {
                return; // mixed changes
            }
            steps += 1;
            dir = step;
        }

        if dir == 0.0 {
            self.vm.ent_set_float(ent, "idealpitch", 0.0);
            return;
        }
        if steps < 2 {
            return;
        }
        self.vm
            .ent_set_float(ent, "idealpitch", -dir * SV_IDEALPITCHSCALE);
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

        // No engine clear of `impulse` (census F4): the C never clears it —
        // QuakeC's ImpulseCommands does, once W_WeaponFrame gets past the
        // weapon cooldown, so a switch pressed mid-cooldown waits for it.
        Ok(fired)
    }

    /// `SV_ReadClientMove` (sv_user.c): copy this frame's [`UserCmd`] onto the
    /// player edict before pre-think. Faithfully:
    /// * `v_angle = [pitch, yaw, 0]` (the look angles the netcode delivered);
    /// * `button0 = buttons & 1` (attack);
    /// * `button2 = (buttons & 2) >> 1` (jump);
    /// * `impulse = cmd.impulse` only when non-zero, like the C; the QuakeC
    ///   (ImpulseCommands) clears it once it has acted on it.
    fn apply_usercmd_to_edict(&mut self, ent: i32, cmd: &UserCmd) {
        // v_angle before PreThink so weapon aim is correct (client_think later
        // re-derives it from the same cmd during the move).
        self.vm
            .ent_set_vector(ent, "v_angle", [cmd.pitch, cmd.yaw, 0.0]);
        self.vm
            .ent_set_float(ent, "button0", (cmd.buttons & 1) as f32);
        self.vm
            .ent_set_float(ent, "button2", ((cmd.buttons & 2) >> 1) as f32);
        // The C only assigns impulse when the byte is non-zero (a 0 impulse means
        // "no command this frame"); the QuakeC clears it when it runs it.
        if cmd.impulse != 0 {
            self.vm.ent_set_float(ent, "impulse", cmd.impulse as f32);
        }
    }

    /// Take and clear the queued sound events fired by the QuakeC since the last
    /// drain (`PF_sound`/`PF_ambientsound` pushes; see [`SoundEvent`]). A
    /// front-end calls this once per frame to play them; tests use it to assert
    /// a weapon actually fired. The queue is process-/thread-local, so call this
    /// on the same thread that drove the frame.
    pub fn drain_sounds(&mut self) -> Vec<SoundEvent> {
        take_sound_events()
    }

    /// Take and clear the placed looping ambient sounds the QuakeC registered
    /// via `ambientsound()` since the last drain (see [`StaticSound`]). The
    /// level's worldspawn registers them all during `spawn_entities`, so a
    /// front-end drains ONCE after the level builds and keeps the loops alive
    /// itself — mirroring how the C wrote them once into the signon packet and
    /// `S_StaticSound` kept a persistent channel. Thread-local like
    /// [`Server::drain_sounds`]: call on the thread that spawned the level.
    pub fn drain_static_sounds(&mut self) -> Vec<StaticSound> {
        take_static_sounds()
    }

    /// Take and clear the queued on-screen messages (`centerprint`/`sprint`/
    /// `bprint`) the QuakeC emitted since the last drain. The front-end shows
    /// centered ones transiently and notify lines fading at the top.
    pub fn drain_messages(&mut self) -> Vec<GameMessage> {
        take_messages()
    }

    /// Take and clear the queued particle bursts fired by the QuakeC since the
    /// last drain (`PF_particle` pushes; see [`ParticleBurst`]). A front-end
    /// calls this once per frame and replays each burst into its
    /// [`crate::particles::ParticleSystem`]; tests use it to assert an
    /// explosion/spawn actually emitted particles. The queue is
    /// process-/thread-local, so call this on the same thread that drove the
    /// frame (mirrors [`Server::drain_sounds`]).
    pub fn drain_particles(&mut self) -> Vec<ParticleBurst> {
        take_particle_bursts()
    }

    /// Take and clear the queued temp-entity events decoded from the QuakeC's
    /// broadcast `Write*` bursts since the last drain (rocket/grenade explosions,
    /// bullet wall-impacts, nail/spike impacts). A front-end calls this once per
    /// frame and maps each [`TempEntityEvent`] to the matching
    /// [`crate::particles::ParticleSystem`] effect (and an explosion sound); tests
    /// use it to assert a temp entity actually fired. The queue is
    /// process-/thread-local, so call this on the same thread that drove the frame
    /// (mirrors [`Server::drain_sounds`]/[`Server::drain_particles`]).
    pub fn drain_temp_entities(&mut self) -> Vec<TempEntityEvent> {
        take_temp_entities()
    }

    /// Take and clear the queued MSG_ALL server commands recognised from the
    /// QuakeC's `WriteByte(MSG_ALL, ...)` bursts since the last drain
    /// (`svc_intermission` / `svc_finale` / `svc_cutscene` / `svc_sellscreen`).
    /// A front-end calls this once per frame and plays the client role of
    /// `CL_ParseServerMessage` (cl_parse.c): enter intermission mode, latch the
    /// completed time, start the finale text reveal. Thread-local like
    /// [`Server::drain_temp_entities`] — call it on the thread that drove the frame.
    pub fn drain_svc_events(&mut self) -> Vec<SvcEvent> {
        take_svc_events()
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

    /// `SV_ClientThink` + `SV_AirMove` (sv_user.c): apply the usercmd angles to
    /// `v_angle`/`angles`, build the wish velocity from the move axes and angle
    /// vectors, then friction + acceleration toward `wishdir` (ground) or air
    /// acceleration (airborne). This sets `velocity`; the actual position move
    /// happens afterward in [`Self::walk_move`] / [`Self::player_fly_move`].
    fn client_think(&mut self, ent: i32, cmd: &UserCmd, dt: f32) {
        if self.vm.ent_get_float(ent, "movetype") as i32 == MOVETYPE_NONE {
            return;
        }

        let on_ground = (self.vm.ent_get_float(ent, "flags") as i32) & FL_ONGROUND != 0;

        // DropPunchAngle: decay the view kick toward zero.
        self.drop_punch_angle(ent, dt);

        // if dead, behave differently (no movement)
        if self.vm.ent_get_float(ent, "health") <= 0.0 {
            return;
        }

        // Angles (SV_ClientThink, sv_user.c:400-412): the engine derives the
        // body angles from the look + the weapon kick + the strafe lean:
        //   v_angle      = v_angle + punchangle          (weapon recoil kick)
        //   angles[ROLL] = V_CalcRoll(angles, velocity)*4 (strafe lean — set
        //                  UNCONDITIONALLY, even when fixangle forces the look)
        //   if (!fixangle) { angles[PITCH] = -v_angle[PITCH]/3; angles[YAW] = v_angle[YAW]; }
        // angles show 1/3 the (punch-adjusted) pitch and all the yaw; ROLL leans
        // into a sidestep so the model/view banks. A QuakeC-forced `fixangle`
        // (e.g. after a teleport) overrides only the pitch/yaw, not the roll.
        let fixangle = self.vm.ent_get_float(ent, "fixangle");

        // v_angle field = [pitch, yaw, roll] from the incoming command (the C
        // SV_ReadClientMove writes this before SV_ClientThink).
        self.vm
            .ent_set_vector(ent, "v_angle", [cmd.pitch, cmd.yaw, 0.0]);

        // Local v_angle including the punch kick (the C `VectorAdd` into a temp;
        // the stored v_angle field is NOT modified by the punch).
        let punchangle = self.vm.ent_get_vector(ent, "punchangle");
        let v_angle_kick = [
            cmd.pitch + punchangle[crate::math::PITCH],
            cmd.yaw + punchangle[crate::math::YAW],
            punchangle[crate::math::ROLL],
        ];

        // angles[ROLL] = V_CalcRoll(current angles, velocity) * 4 — read the
        // PRE-update angles + velocity, exactly as the C does before assigning
        // pitch/yaw.
        let cur_angles = self.vm.ent_get_vector(ent, "angles");
        let velocity = self.vm.ent_get_vector(ent, "velocity");
        let roll = v_calc_roll(cur_angles, velocity) * 4.0;

        if fixangle == 0.0 {
            self.vm.ent_set_vector(
                ent,
                "angles",
                [
                    -v_angle_kick[crate::math::PITCH] / 3.0,
                    v_angle_kick[crate::math::YAW],
                    roll,
                ],
            );
        } else {
            // Honour the forced pitch/yaw but still bank the roll, then clear the
            // flag (SV_WriteClientdata).
            self.vm
                .ent_set_vector(ent, "angles", [cur_angles[0], cur_angles[1], roll]);
            self.vm.ent_set_float(ent, "fixangle", 0.0);
        }

        // SV_ClientThink: waist-deep in water (and not noclip) -> swim, then
        // return — mirrors sv_user.c dispatching SV_WaterMove ahead of SV_AirMove.
        // `waterlevel` is the prior frame's check_water value (the WALK arm runs
        // check_water AFTER client_think), matching id's two-pass phasing where
        // SV_RunClients precedes SV_Physics.
        let movetype = self.vm.ent_get_float(ent, "movetype") as i32;
        // SV_WaterJump (sv_user.c:414): a QuakeC-set climb-out (FL_WATERJUMP, from
        // CheckWaterJump) forces a horizontal launch toward movedir until the
        // timer expires or you leave the water — checked before the swim/air move.
        let flags = self.vm.ent_get_float(ent, "flags") as i32;
        if flags & FL_WATERJUMP != 0 {
            self.water_jump(ent);
            return;
        }
        let waterlevel = self.vm.ent_get_float(ent, "waterlevel") as i32;
        if movetype != MOVETYPE_NOCLIP && waterlevel >= 2 {
            self.water_move(ent, cmd, dt);
            return;
        }

        // SV_AirMove: wishvel from forward/side and the look angles.
        let angles = self.vm.ent_get_vector(ent, "angles");
        let (forward, right, _up) = crate::math::angle_vectors(angles);
        let mut fmove = cmd.forwardmove;
        let smove = cmd.sidemove;

        // hack to not let you back into the teleporter you just left.
        let teleport_time = self.vm.ent_get_float(ent, "teleport_time");
        let time = self.time();
        if time < teleport_time && fmove < 0.0 {
            fmove = 0.0;
        }

        let mut wishvel = [
            forward[0] * fmove + right[0] * smove,
            forward[1] * fmove + right[1] * smove,
            forward[2] * fmove + right[2] * smove,
        ];

        // (movetype was read above for the water-move dispatch.)
        if movetype != MOVETYPE_WALK {
            wishvel[2] = cmd.upmove;
        } else {
            wishvel[2] = 0.0;
        }

        // wishdir / wishspeed = normalize(wishvel), clamped to sv_maxspeed.
        let (wishdir, mut wishspeed) = crate::math::normalize(wishvel);
        if wishspeed > SV_MAXSPEED {
            let scale = SV_MAXSPEED / wishspeed;
            wishvel = crate::math::scale(wishvel, scale);
            wishspeed = SV_MAXSPEED;
        }

        if movetype == MOVETYPE_NOCLIP {
            // noclip: velocity follows the wish directly.
            self.vm.ent_set_vector(ent, "velocity", wishvel);
        } else if on_ground {
            self.user_friction(ent, dt);
            self.accelerate(ent, wishdir, wishspeed, dt);
        } else {
            // not on ground, so little effect on velocity (air control).
            self.air_accelerate(ent, wishvel, dt);
        }
    }

    /// `SV_WaterMove` (sv_user.c:247): swimming. Faithful transcription —
    ///  * wishvel is built from the FULL view angles (`v_angle`, includes pitch)
    ///    so you swim along your look; the air path uses `angles` (1/3 pitch).
    ///  * when fully idle (no forward/side/up) the player drifts down at 60 u/s,
    ///    otherwise `cmd.upmove` adds vertical intent.
    ///  * wishspeed clamps to `sv_maxspeed`, then scales 0.7 (water is slower).
    ///  * water friction bleeds the full 3-D speed (`sv_friction`, NO edgefriction
    ///    dropoff trace — unlike `user_friction`).
    ///  * water-acceleration nudges velocity toward the normalised wish.
    ///
    /// Gravity is suppressed by the WALK arm while waist-deep (waterlevel > 1),
    /// so this buoyant motion survives the frame.
    fn water_move(&mut self, ent: i32, cmd: &UserCmd, dt: f32) {
        // AngleVectors(v_angle) — NOTE v_angle, not the AirMove `angles`.
        let v_angle = self.vm.ent_get_vector(ent, "v_angle");
        let (forward, right, _up) = angle_vectors(v_angle);
        let mut wishvel = [
            forward[0] * cmd.forwardmove + right[0] * cmd.sidemove,
            forward[1] * cmd.forwardmove + right[1] * cmd.sidemove,
            forward[2] * cmd.forwardmove + right[2] * cmd.sidemove,
        ];
        if cmd.forwardmove == 0.0 && cmd.sidemove == 0.0 && cmd.upmove == 0.0 {
            wishvel[2] -= 60.0; // drift towards the bottom
        } else {
            wishvel[2] += cmd.upmove;
        }

        let mut wishspeed = crate::math::length(wishvel);
        if wishspeed > SV_MAXSPEED {
            wishvel = crate::math::scale(wishvel, SV_MAXSPEED / wishspeed);
            wishspeed = SV_MAXSPEED;
        }
        wishspeed *= 0.7;

        // Water friction: full 3-D speed, sv_friction, no edgefriction trace.
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        let speed = crate::math::length(vel);
        let newspeed = if speed != 0.0 {
            let ns = (speed - dt * speed * SV_FRICTION).max(0.0);
            vel = crate::math::scale(vel, ns / speed);
            self.vm.ent_set_vector(ent, "velocity", vel);
            ns
        } else {
            0.0
        };

        // Water acceleration toward normalize(wishvel).
        if wishspeed == 0.0 {
            return;
        }
        let addspeed = wishspeed - newspeed;
        if addspeed <= 0.0 {
            return;
        }
        let (wishdir, _) = crate::math::normalize(wishvel);
        let mut accelspeed = SV_ACCELERATE * wishspeed * dt;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            vel[i] += accelspeed * wishdir[i];
        }
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `SV_WaterJump` (sv_user.c:307): while FL_WATERJUMP is set (QuakeC's
    /// CheckWaterJump flagged a ledge climb-out), force horizontal velocity to
    /// `movedir` so the player is thrown up onto the ledge; clear the flag once
    /// the timer expires (`sv.time > teleport_time`) or the player left the water.
    fn water_jump(&mut self, ent: i32) {
        let teleport_time = self.vm.ent_get_float(ent, "teleport_time");
        let waterlevel = self.vm.ent_get_float(ent, "waterlevel") as i32;
        if self.time() > teleport_time || waterlevel == 0 {
            let flags = self.vm.ent_get_float(ent, "flags") as i32;
            self.vm.ent_set_float(ent, "flags", (flags & !FL_WATERJUMP) as f32);
            self.vm.ent_set_float(ent, "teleport_time", 0.0);
        }
        let movedir = self.vm.ent_get_vector(ent, "movedir");
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        vel[0] = movedir[0];
        vel[1] = movedir[1];
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `SV_UserFriction` (sv_user.c): bleed off horizontal speed, with extra
    /// friction (`edgefriction`) when the leading edge hangs over a dropoff.
    fn user_friction(&mut self, ent: i32, dt: f32) {
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        let speed = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        if speed == 0.0 {
            return;
        }

        // If the leading edge is over a dropoff, increase friction. The C traces
        // a *point* (mins=maxs=0) 34 units down, 16 units ahead, from the bottom
        // of the player box, ignoring the player.
        let origin = self.vm.ent_get_vector(ent, "origin");
        let pmins = self.vm.ent_get_vector(ent, "mins");
        let start = [
            origin[0] + vel[0] / speed * 16.0,
            origin[1] + vel[1] / speed * 16.0,
            origin[2] + pmins[2],
        ];
        let stop = [start[0], start[1], start[2] - 34.0];
        // SV_UserFriction (sv_user.c) uses SV_Move(..., true, ent): the edge
        // dropoff probe is MOVE_NOMONSTERS, so a box entity below the leading
        // edge can't spuriously suppress edge friction (world geometry only).
        let trace = sv_move(&mut self.vm, start, stop, [0.0; 3], [0.0; 3], ent, true, false);
        let friction = if trace.fraction == 1.0 {
            SV_FRICTION * SV_EDGEFRICTION
        } else {
            SV_FRICTION
        };

        // apply friction
        let control = if speed < SV_STOPSPEED {
            SV_STOPSPEED
        } else {
            speed
        };
        let mut newspeed = speed - dt * control * friction;
        if newspeed < 0.0 {
            newspeed = 0.0;
        }
        newspeed /= speed;

        vel = crate::math::scale(vel, newspeed);
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `SV_Accelerate` (sv_user.c): push velocity toward `wishdir` up to
    /// `wishspeed` by at most `sv_accelerate * dt * wishspeed` this tick.
    fn accelerate(&mut self, ent: i32, wishdir: Vec3, wishspeed: f32, dt: f32) {
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        let currentspeed = crate::math::dot(vel, wishdir);
        let addspeed = wishspeed - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        let mut accelspeed = SV_ACCELERATE * dt * wishspeed;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            vel[i] += accelspeed * wishdir[i];
        }
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `SV_AirAccelerate` (sv_user.c): like `SV_Accelerate` but the *target*
    /// speed is capped at 30, while the acceleration is scaled by the original
    /// (un-capped) `wishspeed` — a faithful transcription of id's exact code,
    /// `wishvel` normalized in place to give both `wishspeed` and the direction.
    fn air_accelerate(&mut self, ent: i32, wishveloc: Vec3, dt: f32) {
        let (dir, wishspeed) = crate::math::normalize(wishveloc);
        let wishspd = if wishspeed > 30.0 { 30.0 } else { wishspeed };
        let mut vel = self.vm.ent_get_vector(ent, "velocity");
        // The C uses `wishveloc` (the normalized vector, since VectorNormalize
        // wrote it in place) for the dot and the add.
        let currentspeed = crate::math::dot(vel, dir);
        let addspeed = wishspd - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        // NOTE: id scales by the ORIGINAL wishspeed, not the capped wishspd.
        let mut accelspeed = SV_ACCELERATE * wishspeed * dt;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            vel[i] += accelspeed * dir[i];
        }
        self.vm.ent_set_vector(ent, "velocity", vel);
    }

    /// `DropPunchAngle` (sv_user.c): decay the view-kick vector by `10*dt` units
    /// of length toward zero.
    fn drop_punch_angle(&mut self, ent: i32, dt: f32) {
        let punch = self.vm.ent_get_vector(ent, "punchangle");
        let (dir, mut len) = crate::math::normalize(punch);
        len -= 10.0 * dt;
        if len < 0.0 {
            len = 0.0;
        }
        self.vm
            .ent_set_vector(ent, "punchangle", crate::math::scale(dir, len));
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

// ---------------------------------------------------------------------------
// (D) Entity-aware move, impact, and trigger touching.
//
// Ported from world.c (`SV_Move` ~923, `SV_ClipMoveToEntity` ~722,
// `SV_TouchLinks` ~258) and sv_phys.c (`SV_Impact` ~153, `SV_PushEntity`
// ~408). SIMPLIFICATION: the C walked an areanode tree (SV_ClipToLinks /
// SV_TouchLinks recursing children) for O(log n) culling; here we LINEAR-SCAN
// every edict but apply the C's per-entity abs-box broadphase reject first (see
// `sv_move`), so a dense map's far entities are dismissed in O(1) without a hull
// trace — result-identical to the tree (which was only an acceleration
// structure). Brush-model rotation (the `QUAKE2` branch) is out of scope.
// ---------------------------------------------------------------------------

// Solid types (server.h). SOLID_NOT/SOLID_TRIGGER do not block a move.
const SOLID_NOT: i32 = 0;
const SOLID_TRIGGER: i32 = 1;
const SOLID_BBOX: i32 = 2;
const SOLID_SLIDEBOX: i32 = 3;
const SOLID_BSP: i32 = 4;

/// The result of [`sv_move`]: a world-collision trace plus the edict that was
/// hit (`SV_Move`'s `clip.trace` with `trace.ent` resolved to an edict index).
///
/// `ent` semantics: `0` = the world model (a world impact, like the C
/// `clip.trace.ent = sv.edicts`), a positive index = that edict, and `-1` =
/// nothing was hit (a fully clear move). This mirrors `trace_t.ent`, which the
/// C set to `sv.edicts` (edict 0) for a world hit, the touched edict for an
/// entity hit, and left `NULL` for a clear move (here `-1`, since `0` is the
/// valid world edict).
#[derive(Debug, Clone, Copy)]
pub struct MoveTrace {
    pub allsolid: bool,
    pub startsolid: bool,
    pub inopen: bool,
    pub inwater: bool,
    pub fraction: f32,
    pub endpos: Vec3,
    pub plane_normal: Vec3,
    pub plane_dist: f32,
    /// Edict hit: `0` = world, `>0` = that edict, `-1` = nothing.
    pub ent: i32,
}

impl MoveTrace {
    /// Build a `MoveTrace` from a world [`HostTrace`], resolving `ent` to `0`
    /// (the world) when the move was clipped and `-1` when it ran clear.
    fn from_world(tr: HostTrace) -> MoveTrace {
        let hit = tr.fraction < 1.0 || tr.startsolid;
        MoveTrace {
            allsolid: tr.allsolid,
            startsolid: tr.startsolid,
            inopen: tr.inopen,
            inwater: tr.inwater,
            fraction: tr.fraction,
            endpos: tr.endpos,
            plane_normal: tr.plane_normal,
            plane_dist: tr.plane_dist,
            ent: if hit { 0 } else { -1 },
        }
    }
}

/// `SV_Move` (world.c ~923): box-trace `mins`/`maxs` from `start` to `end`
/// against the world **and** every solid entity, returning the closest impact.
///
/// This is the linear-scan replacement for `SV_ClipToLinks`: after clipping to
/// the world (via [`crate::world::trace_world`]), it walks every live edict,
/// clips against each blocking solid (`SOLID_BSP` brush submodels via
/// [`crate::world::trace_submodel`], `SOLID_BBOX`/`SOLID_SLIDEBOX` boxes via
/// [`crate::world::clip_box`]), and keeps whichever trace stops earliest.
/// `SOLID_NOT` and `SOLID_TRIGGER` entities never block (triggers fire via
/// [`touch_triggers`], not here). The `ignore` edict (the mover itself) is
/// skipped, exactly as the C skipped `clip->passedict`.
///
/// The whole scan runs inside one [`Vm::with_host`] so the host is borrowed out
/// exactly once; no QuakeC executes here (touch functions run later, with the
/// host present).
///
/// `nomonsters` implements the C `MOVE_NOMONSTERS` flag (world.c
/// `SV_ClipToLinks`: `if (type == MOVE_NOMONSTERS && touch->v.solid !=
/// SOLID_BSP) continue;`). When set, the scan clips only against the world and
/// `SOLID_BSP` brush submodels (doors/plats), skipping every `SOLID_BBOX` /
/// `SOLID_SLIDEBOX` box (monsters, items, and the player). This is what the QC
/// `visible()` helper relies on: its sight-line `traceline(..., TRUE, self)`
/// must pass *through* the player's own box and reach `trace_fraction == 1.0`,
/// otherwise FindTarget's `if (!visible(client)) return;` bails and the monster
/// never latches `self.enemy`.
///
/// `missile` implements the C `MOVE_MISSILE` flag (world.c `SV_Move` sets
/// `clip.mins2/maxs2 = +-15`, and `SV_ClipToLinks` clips `FL_MONSTER` touch
/// entities against that expanded box: `trace = SV_ClipMoveToEntity(touch,
/// clip->start, clip->mins2, clip->maxs2, clip->end)`). When set, any candidate
/// carrying `FL_MONSTER` is clipped against a +-15 moving box instead of the
/// move's own `mins`/`maxs`, so a rocket detonates when it lands *near* a
/// monster (not only on a direct hit). Non-monster entities and the world keep
/// the move's own box. When `missile == false` (every caller except the
/// `MOVETYPE_FLYMISSILE` branch of `push_entity`), behaviour is identical to a
/// plain `MOVE_NORMAL` clip.
/// Sample the world point-contents at `p` (the [`Host`]-backed `SV_PointContents`
/// the builtins use): `CONTENTS_EMPTY` (-1), `SOLID` (-2), `WATER` (-3),
/// `SLIME` (-4), `LAVA` (-5), etc. Exposed for tooling/tests (e.g. probing where
/// a liquid is); `CONTENTS_SOLID` if there is no host.
pub fn probe_point_contents(vm: &mut Vm, p: Vec3) -> i32 {
    vm.with_host(|_vm, h| h.point_contents(p)).unwrap_or(CONTENTS_SOLID)
}

// Mirrors the C `SV_Move(start, mins, maxs, end, type, passedict)` signature (world.c).
#[allow(clippy::too_many_arguments)]
pub fn sv_move(
    vm: &mut Vm,
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
    ignore: i32,
    nomonsters: bool,
    missile: bool,
) -> MoveTrace {
    vm.with_host(|vm, host| {
        let bsp = host.bsp();

        // 1) Clip to the world (edict 0).
        let world = crate::world::trace_world(bsp, start, end, mins, maxs);
        let mut best = MoveTrace::from_world(world);

        // Broadphase bounds of the whole move (SV_MoveBounds, world.c ~1004): the
        // C's SV_ClipToLinks rejects any touch entity whose linked abs box can't
        // overlap this box BEFORE the expensive per-entity hull trace. We still scan
        // linearly (no areanode tree), but this O(1) reject is what makes dense maps
        // (e1m3: many monsters/items, so the old all-pairs clip was O(moves*edicts))
        // tractable — and it's RESULT-IDENTICAL, since a non-overlapping box can
        // never be hit. For a missile the box uses the +-15 `mins2/maxs2` expansion
        // so nearby FL_MONSTER touches are still considered.
        let (m2_mins, m2_maxs) = if missile {
            ([-15.0, -15.0, -15.0], [15.0, 15.0, 15.0])
        } else {
            (mins, maxs)
        };
        let mut box_mins = [0.0f32; 3];
        let mut box_maxs = [0.0f32; 3];
        for i in 0..3 {
            if end[i] > start[i] {
                box_mins[i] = start[i] + m2_mins[i] - 1.0;
                box_maxs[i] = end[i] + m2_maxs[i] + 1.0;
            } else {
                box_mins[i] = end[i] + m2_mins[i] - 1.0;
                box_maxs[i] = start[i] + m2_maxs[i] + 1.0;
            }
        }

        // 2) Clip to every solid entity (linear scan + the broadphase reject above;
        //    the C used the areanode tree purely as an acceleration structure).
        let n = vm.num_edicts();
        for e in 0..n {
            let ei = e as i32;
            if ei == 0 {
                continue; // world already clipped
            }
            if ei == ignore {
                continue; // don't clip against the mover itself (passedict)
            }
            if vm.edict_free.get(e).copied().unwrap_or(true) {
                continue; // free slot
            }

            // SV_ClipToLinks allsolid early-out (world.c ~847): once the move is
            // wholly trapped in solid there is nothing further to clip, and a later
            // entity's trace must not clobber `allsolid` back to false. The C does
            // `if (clip->trace.allsolid) return;` at the top of the touch loop.
            if best.allsolid {
                break;
            }

            let solid = vm.ent_get_float(ei, "solid") as i32;

            // MOVE_NOMONSTERS: clip only against the world + SOLID_BSP bmodels;
            // skip every box entity (monsters, items, and the player itself).
            if nomonsters && solid != SOLID_BSP {
                continue;
            }

            // SV_ClipToLinks owner skip (world.c ~849-855): when a real
            // passedict is set, never clip a missile against its owner or an
            // owner against its own missile. Without this a rocket/grenade/nail
            // spawned inside the shooter's box traces against the shooter and
            // detonates immediately. Only world (edict 0) is never a passedict,
            // so the gate is for ignore > 0 (`clip->passedict` set).
            if ignore > 0 {
                if vm.ent_get_int(ei, "owner") == ignore {
                    continue; // don't clip against own missiles
                }
                if vm.ent_get_int(ignore, "owner") == ei {
                    continue; // don't clip against owner
                }

                // SV_ClipToLinks points-never-interact skip (world.c
                // ~843-844): `if (clip->passedict && clip->passedict->v.size[0]
                // && !touch->v.size[0]) continue;`. A box-sized passedict (the
                // mover, `ignore`) must not clip against a point-sized
                // (size[0]==0) touch entity (e.g. a player/monster vs a
                // point trigger/item). size = maxs - mins, so size[0]==0 iff the
                // box has zero extent on x. Only the world (edict 0) is never a
                // passedict, so this is gated on `ignore > 0`.
                let pass_size_x = {
                    let pmins = vm.ent_get_vector(ignore, "mins");
                    let pmaxs = vm.ent_get_vector(ignore, "maxs");
                    pmaxs[0] - pmins[0]
                };
                if pass_size_x != 0.0 {
                    let tmins = vm.ent_get_vector(ei, "mins");
                    let tmaxs = vm.ent_get_vector(ei, "maxs");
                    if tmaxs[0] - tmins[0] == 0.0 {
                        continue; // points never interact
                    }
                }
            }

            // SV_ClipToLinks broadphase reject (world.c ~857): skip any entity whose
            // linked abs box does not overlap the move box. `absmin`/`absmax` are kept
            // current by link_edict (origin+mins-exp .. origin+maxs-exp) on every
            // setorigin/setsize/move, exactly as SV_LinkEdict maintains them, so this
            // is the same test the C runs — and result-identical (a box that can't
            // overlap the move can't be hit by the precise clip below).
            let absmin = vm.ent_get_vector(ei, "absmin");
            let absmax = vm.ent_get_vector(ei, "absmax");
            if absmin[0] > box_maxs[0]
                || absmin[1] > box_maxs[1]
                || absmin[2] > box_maxs[2]
                || absmax[0] < box_mins[0]
                || absmax[1] < box_mins[1]
                || absmax[2] < box_mins[2]
            {
                continue;
            }

            let origin = vm.ent_get_vector(ei, "origin");

            // MOVE_MISSILE FL_MONSTER expansion (world.c SV_ClipToLinks): a
            // missile move clips FL_MONSTER touch entities against the +-15
            // `mins2`/`maxs2` box instead of the move's own box, so rockets
            // detonate when they land NEAR a monster. Non-monster entities and
            // the world keep the move's own `mins`/`maxs`.
            let (clip_mins, clip_maxs) =
                if missile && (vm.ent_get_float(ei, "flags") as i32) & FL_MONSTER != 0 {
                    ([-15.0, -15.0, -15.0], [15.0, 15.0, 15.0])
                } else {
                    (mins, maxs)
                };

            let tr = match solid {
                SOLID_BSP => {
                    // model "*N" -> submodel index N. Borrow the name (no per-clip
                    // String allocation); the &str borrow ends with this expression.
                    let idx = vm
                        .ent_string_ref(ei, "model")
                        .strip_prefix('*')
                        .and_then(|d| d.parse::<usize>().ok());
                    match idx {
                        Some(idx) => crate::world::trace_submodel(
                            bsp, idx, origin, start, end, clip_mins, clip_maxs,
                        ),
                        None => continue, // SOLID_BSP without a valid "*N" model
                    }
                }
                SOLID_BBOX | SOLID_SLIDEBOX => {
                    let ent_mins = vm.ent_get_vector(ei, "mins");
                    let ent_maxs = vm.ent_get_vector(ei, "maxs");
                    crate::world::clip_box(
                        start, end, clip_mins, clip_maxs, ent_mins, ent_maxs, origin,
                    )
                }
                // SOLID_NOT and SOLID_TRIGGER do not block a move.
                _ => continue,
            };

            // Adopt this entity's trace when it stops earlier, is all-solid, or
            // started solid (the C's `trace.allsolid || trace.startsolid ||
            // trace.fraction < clip->trace.fraction` test).
            if tr.allsolid || tr.startsolid || tr.fraction < best.fraction {
                let was_startsolid = best.startsolid;
                best.allsolid = tr.allsolid;
                best.startsolid = tr.startsolid || was_startsolid;
                best.inopen = tr.inopen;
                best.inwater = tr.inwater;
                best.fraction = tr.fraction;
                best.endpos = tr.endpos;
                best.plane_normal = tr.plane_normal;
                best.plane_dist = tr.plane_dist;
                best.ent = ei;
            } else if tr.startsolid {
                best.startsolid = true;
            }
        }

        best
    })
    .unwrap_or(MoveTrace {
        // No host: a clear move (the engine builtins fault cleanly anyway).
        allsolid: false,
        startsolid: false,
        inopen: false,
        inwater: false,
        fraction: 1.0,
        endpos: end,
        plane_normal: [0.0; 3],
        plane_dist: 0.0,
        ent: -1,
    })
}

/// `SV_Impact` (sv_phys.c ~153): two entities have touched, so run each one's
/// `touch` function with `self`/`other` set appropriately.
///
/// Saves the `self`/`other` globals, sets `time = current`, runs `e1.touch`
/// (self=e1, other=e2) then `e2.touch` (self=e2, other=e1) — each only if that
/// edict has a non-null `touch` and is not `SOLID_NOT` — then restores
/// `self`/`other`. A faulting touch is isolated (the interpreter is reset) so
/// one bad touch does not abort the caller, mirroring the per-entity
/// robustness elsewhere in the server. The host must be PRESENT (this calls
/// `execute`); never invoke it from inside `with_host`.
pub fn sv_impact(vm: &mut Vm, e1: i32, e2: i32, sv_time: f32) {
    let old_self = vm.gget_int("self");
    let old_other = vm.gget_int("other");
    // SV_Impact (sv_phys.c:160) sets pr_global_struct->time = sv.time before the
    // touch functions run, so a touch sees the frame's start time — not a stale
    // value left in the `time` global by the entity's own think (which
    // SV_RunThink clamps to [sv.time, sv.time+frametime] and is usually past
    // sv.time). The C does not restore `time` afterward, matching the order here.
    vm.gset_float("time", sv_time);

    run_touch(vm, e1, e2);
    run_touch(vm, e2, e1);

    vm.gset_int("self", old_self);
    vm.gset_int("other", old_other);
}

/// Run `toucher`'s `touch` function with `self = toucher`, `other = with`, when
/// `toucher` has a valid `touch` function and is not `SOLID_NOT`. A fault is
/// caught and the interpreter reset (the entity's bad touch is isolated).
fn run_touch(vm: &mut Vm, toucher: i32, with: i32) {
    let touch = vm.ent_get_int(toucher, "touch");
    if touch <= 0 || (touch as usize) >= vm.progs.functions.len() {
        return; // no touch function (the C `if (e->v.touch ...)`)
    }
    if vm.ent_get_float(toucher, "solid") as i32 == SOLID_NOT {
        return;
    }
    vm.gset_int("self", toucher);
    vm.gset_int("other", with);
    if vm.execute(touch as usize).is_err() {
        vm.reset_execution();
    }
}

/// `SV_TouchLinks` for trigger fields (world.c ~258, the trigger half of
/// `SV_LinkEdict`'s relink): after `mover` moves, fire every `SOLID_TRIGGER`
/// edict whose `touch` function exists and whose `absmin`/`absmax` box overlaps
/// the mover's. This is how items get picked up and trigger fields fire.
///
/// SIMPLIFICATION: linear scan instead of the areanode `trigger_edicts` lists.
/// The overlap test reads the `absmin`/`absmax` fields the `setorigin`/`setsize`
/// builtins maintain. Triggers to run are gathered into a `Vec` first (so the
/// borrow of the edict array ends before any `execute`), then each is run with
/// `self = trigger`, `other = mover`. A faulting trigger is isolated.
pub fn touch_triggers(vm: &mut Vm, mover: i32, sv_time: f32) {
    // Gather first: collect the trigger edicts to fire so we don't execute
    // QuakeC while iterating (the touch could spawn/free edicts).
    let mover_absmin = vm.ent_get_vector(mover, "absmin");
    let mover_absmax = vm.ent_get_vector(mover, "absmax");

    let mut to_fire: Vec<i32> = Vec::new();
    let n = vm.num_edicts();
    for e in 0..n {
        let ei = e as i32;
        if ei == mover {
            continue; // the C `if (touch == ent) continue;`
        }
        if vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        if vm.ent_get_float(ei, "solid") as i32 != SOLID_TRIGGER {
            continue;
        }
        let touch = vm.ent_get_int(ei, "touch");
        if touch <= 0 || (touch as usize) >= vm.progs.functions.len() {
            continue; // no touch function
        }
        let t_absmin = vm.ent_get_vector(ei, "absmin");
        let t_absmax = vm.ent_get_vector(ei, "absmax");
        // AABB overlap (the C's six-way reject test, inverted).
        if mover_absmin[0] > t_absmax[0]
            || mover_absmin[1] > t_absmax[1]
            || mover_absmin[2] > t_absmax[2]
            || mover_absmax[0] < t_absmin[0]
            || mover_absmax[1] < t_absmin[1]
            || mover_absmax[2] < t_absmin[2]
        {
            continue;
        }
        to_fire.push(ei);
    }

    // Now run each trigger's touch (host is present here).
    let old_self = vm.gget_int("self");
    let old_other = vm.gget_int("other");
    for t in to_fire {
        // Re-check the edict is still live and a trigger (a prior touch may have
        // freed or changed it).
        if vm.edict_free.get(t as usize).copied().unwrap_or(true) {
            continue;
        }
        if vm.ent_get_float(t, "solid") as i32 != SOLID_TRIGGER {
            continue;
        }
        let touch = vm.ent_get_int(t, "touch");
        if touch <= 0 || (touch as usize) >= vm.progs.functions.len() {
            continue;
        }
        vm.gset_int("self", t);
        vm.gset_int("other", mover);
        // SV_TouchLinks (world.c:304) sets pr_global_struct->time = sv.time
        // before EACH trigger touch, so the touch sees the frame's start time
        // rather than a stale think-time left in the `time` global.
        vm.gset_float("time", sv_time);
        if vm.execute(touch as usize).is_err() {
            vm.reset_execution();
        }
    }
    vm.gset_int("self", old_self);
    vm.gset_int("other", old_other);
}

// ---------------------------------------------------------------------------
// (D2) Monster movement: the AI walk/chase steps.
//
// Ported from sv_move.c (`SV_CheckBottom` ~36, `SV_movestep` ~110,
// `SV_StepDirection` ~232, `SV_FixCheckBottom` ~267, `SV_NewChaseDir` ~283,
// `SV_CloseEnough` ~371, `SV_MoveToGoal` ~391) and pr_cmds.c (`PF_walkmove`
// ~541, `PF_checkbottom`, `PF_checkclient` ~714, `PF_findradius` ~788).
//
// These drive ai.qc / fight.qc so grunts and dogs walk, chase and target the
// player. Every collision query is the ENTITY-AWARE [`sv_move`] (so monsters
// collide with the world, the player, and each other), exactly as the C's
// `SV_Move(..., ent)` passed the monster as the ignored passedict.
//
// QUAKE2-branch omissions: id's `QUAKE2` build added water-current handling and
// an alternate `SV_movestep` fly/swim path; that branch is not compiled here
// (this is the stock WinQuake non-`QUAKE2` build). `SV_CheckBottom`'s `c_yes`/
// `c_no` debug counters are dropped (diagnostics only).
// ---------------------------------------------------------------------------

/// `SV_CheckBottom` (sv_move.c ~36): is there floor under the whole box?
///
/// Returns `false` if any part of the bottom of `ent`'s box hangs over an edge
/// that is not a staircase (so [`sv_movestep`] refuses to walk off a ledge).
///
/// Fast path: if all four bottom corners sit directly over solid world, accept
/// immediately. Otherwise the slow path traces a point straight down from the
/// midpoint and each corner (using the world-only trace — the C passed
/// `vec3_origin` mins/maxs, i.e. a point move), and rejects if the midpoint
/// found no floor or any corner is more than `STEPSIZE` below the midpoint.
pub fn sv_check_bottom(vm: &mut Vm, ent: i32) -> bool {
    let origin = vm.ent_get_vector(ent, "origin");
    let ent_mins = vm.ent_get_vector(ent, "mins");
    let ent_maxs = vm.ent_get_vector(ent, "maxs");
    let mins = v_add(origin, ent_mins);
    let maxs = v_add(origin, ent_maxs);

    // Fast path: if all four corners under the box are solid world, accept.
    // (The corners are sampled 1 unit below the box bottom.)
    let mut easy = true;
    let z = mins[2] - 1.0;
    'corners: for &x in &[mins[0], maxs[0]] {
        for &y in &[mins[1], maxs[1]] {
            let p: Vec3 = [x, y, z];
            let c = vm.with_host(|_vm, h| h.point_contents(p)).unwrap_or(CONTENTS_SOLID);
            if c != CONTENTS_SOLID {
                easy = false;
                break 'corners;
            }
        }
    }
    if easy {
        return true; // we got out easy
    }

    // Slow path: trace point moves straight down from the box bottom.
    let start_z = mins[2];
    let stop_z = start_z - 2.0 * world::STEPSIZE;

    // The midpoint must find a floor within 2*STEPSIZE.
    let mid_x = (mins[0] + maxs[0]) * 0.5;
    let mid_y = (mins[1] + maxs[1]) * 0.5;
    let mid_start: Vec3 = [mid_x, mid_y, start_z];
    let mid_stop: Vec3 = [mid_x, mid_y, stop_z];
    // SV_Move(start, vec3_origin, vec3_origin, stop, true, ent): a *point* move
    // (mins=maxs=0) with MOVE_NOMONSTERS (the trailing `true`) so the floor
    // probe clips only against world geometry, not monster/item/player boxes.
    let tr = sv_move(vm, mid_start, mid_stop, [0.0; 3], [0.0; 3], ent, true, false);
    if tr.fraction == 1.0 {
        return false; // no floor under the midpoint
    }
    let mid = tr.endpos[2];
    let mut bottom = mid;

    // Each corner must be within STEPSIZE of the midpoint floor height.
    for &x in &[mins[0], maxs[0]] {
        for &y in &[mins[1], maxs[1]] {
            let cstart: Vec3 = [x, y, start_z];
            let cstop: Vec3 = [x, y, stop_z];
            // MOVE_NOMONSTERS (trailing `true`): world geometry only.
            let tr = sv_move(vm, cstart, cstop, [0.0; 3], [0.0; 3], ent, true, false);
            if tr.fraction != 1.0 && tr.endpos[2] > bottom {
                bottom = tr.endpos[2];
            }
            if tr.fraction == 1.0 || mid - tr.endpos[2] > world::STEPSIZE {
                return false; // corner dangles over an edge
            }
        }
    }
    true
}

/// `SV_movestep` (sv_move.c ~110): try to move `ent` by `move`, adjusting for
/// slopes and stairs. Returns `true` and commits the new origin on success;
/// returns `false` and leaves the origin untouched if the move isn't possible.
///
/// Walking monsters: trace from `origin + STEPSIZE` down to
/// `origin + move - STEPSIZE` (the step-up/step-down envelope) via the
/// entity-aware [`sv_move`], requiring solid ground and [`sv_check_bottom`].
/// Flying/swimming monsters (`FL_FLY`/`FL_SWIM`) use the direct two-try path
/// (with the enemy-height nudge) and never step up. `FL_PARTIALGROUND` monsters
/// fall through / keep correcting instead of refusing. When `relink` is set the
/// box is re-linked and its triggers fired, exactly as the C `SV_LinkEdict(ent,
/// true)`.
pub fn sv_movestep(vm: &mut Vm, ent: i32, mov: Vec3, relink: bool) -> bool {
    let oldorg = vm.ent_get_vector(ent, "origin");
    let ent_mins = vm.ent_get_vector(ent, "mins");
    let ent_maxs = vm.ent_get_vector(ent, "maxs");
    let flags = vm.ent_get_float(ent, "flags") as i32;

    // Flying / swimming monsters don't step up.
    if flags & (FL_SWIM | FL_FLY) != 0 {
        let enemy = vm.ent_get_int(ent, "enemy");
        // Try one move with vertical motion, then one without.
        for i in 0..2 {
            let mut neworg = v_add(oldorg, mov);
            if i == 0 && enemy > 0 {
                let enemy_org = vm.ent_get_vector(enemy, "origin");
                let dz = oldorg[2] - enemy_org[2];
                if dz > 40.0 {
                    neworg[2] -= 8.0;
                }
                if dz < 30.0 {
                    neworg[2] += 8.0;
                }
            }
            let tr = sv_move(vm, oldorg, neworg, ent_mins, ent_maxs, ent, false, false);
            if tr.fraction == 1.0 {
                // A swim monster that would leave water cannot make this move.
                if flags & FL_SWIM != 0 {
                    let c = vm
                        .with_host(|_vm, h| h.point_contents(tr.endpos))
                        .unwrap_or(CONTENTS_SOLID);
                    if c == CONTENTS_EMPTY {
                        return false; // swim monster left water
                    }
                }
                vm.ent_set_vector(ent, "origin", tr.endpos);
                if relink {
                    link_edict(vm, ent);
                    // Reached through movetogoal/walkmove DURING a monster's
                    // think; sv.time is not threaded here, so preserve the
                    // pre-fix behaviour (the prior NO-OP set `time` to its own
                    // current value). See FIX-1 notes: the physics paths get the
                    // true start-of-frame time; this monster path keeps `time`.
                    let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
                    touch_triggers(vm, ent, time);
                }
                return true;
            }
            if enemy <= 0 {
                break; // no enemy: only one try
            }
        }
        return false;
    }

    // Walking monster: push down from a step height above the wished position.
    let mut neworg = v_add(oldorg, mov);
    neworg[2] += world::STEPSIZE;
    let mut end = neworg;
    end[2] -= world::STEPSIZE * 2.0;

    let mut tr = sv_move(vm, neworg, end, ent_mins, ent_maxs, ent, false, false);

    if tr.allsolid {
        return false;
    }
    if tr.startsolid {
        // Back the start down a step and retry (the C's startsolid retry).
        neworg[2] -= world::STEPSIZE;
        tr = sv_move(vm, neworg, end, ent_mins, ent_maxs, ent, false, false);
        if tr.allsolid || tr.startsolid {
            return false;
        }
    }
    if tr.fraction == 1.0 {
        // No floor in the step envelope.
        if flags & FL_PARTIALGROUND != 0 {
            // The monster had the ground pulled out; let it fall.
            vm.ent_set_vector(ent, "origin", v_add(oldorg, mov));
            if relink {
                link_edict(vm, ent);
                // Monster think path: keep the live `time` global (see FIX-1).
                let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
                touch_triggers(vm, ent, time);
            }
            let flags = vm.ent_get_float(ent, "flags") as i32;
            vm.ent_set_float(ent, "flags", (flags & !FL_ONGROUND) as f32);
            return true;
        }
        return false; // walked off an edge
    }

    // Landed on something: provisionally take the new origin, then verify the
    // whole box has floor under it (dangling-corner check).
    vm.ent_set_vector(ent, "origin", tr.endpos);

    if !sv_check_bottom(vm, ent) {
        if flags & FL_PARTIALGROUND != 0 {
            // Floor mostly pulled out: keep correcting (accept the move).
            if relink {
                link_edict(vm, ent);
                // Monster think path: keep the live `time` global (see FIX-1).
                let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
                touch_triggers(vm, ent, time);
            }
            return true;
        }
        // Revert: no clean standing position.
        vm.ent_set_vector(ent, "origin", oldorg);
        return false;
    }

    if flags & FL_PARTIALGROUND != 0 {
        // Back on solid ground: clear the partial-ground flag.
        let flags = vm.ent_get_float(ent, "flags") as i32;
        vm.ent_set_float(ent, "flags", (flags & !FL_PARTIALGROUND) as f32);
    }
    // groundentity = the edict we landed on (world = 0, an entity = its index;
    // a clear-but-landed trace resolves ent to 0/the world via MoveTrace).
    let ground = if tr.ent < 0 { 0 } else { tr.ent };
    vm.ent_set_int(ent, "groundentity", ground);

    if relink {
        link_edict(vm, ent);
        // Monster think path: keep the live `time` global (see FIX-1).
        let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
        touch_triggers(vm, ent, time);
    }
    true
}

/// `SV_StepDirection` (sv_move.c ~232): turn `ent` toward `yaw` and walk `dist`
/// in that direction if now roughly facing it.
///
/// Sets `ideal_yaw`, turns via the real `changeyaw` builtin logic, builds the
/// horizontal move `(cos,sin,0)*dist`, and calls [`sv_movestep`]. On success,
/// if the monster has not yet turned within 45 degrees of the move it reverts
/// the origin (it still counts as a successful "step" so the caller stops
/// hunting for a direction — matching the C). Always relinks at the end.
pub fn sv_step_direction(vm: &mut Vm, ent: i32, yaw: f32, dist: f32) -> bool {
    vm.ent_set_float(ent, "ideal_yaw", yaw);
    // PF_changeyaw() turns angles[1] toward ideal_yaw by at most yaw_speed. It
    // reads `self` (as the C does), so point `self` at `ent` for the turn and
    // restore it afterwards (the C's chase chain runs with self == the monster,
    // but restoring keeps us robust if a caller drives a non-self actor).
    let oldself = vm.gget_int("self");
    vm.gset_int("self", ent);
    let _ = bi_changeyaw(vm);
    vm.gset_int("self", oldself);

    let rad = yaw * std::f32::consts::PI / 180.0;
    let mov: Vec3 = [rad.cos() * dist, rad.sin() * dist, 0.0];

    let oldorigin = vm.ent_get_vector(ent, "origin");
    if sv_movestep(vm, ent, mov, false) {
        let angles = vm.ent_get_vector(ent, "angles");
        let delta = angles[1] - vm.ent_get_float(ent, "ideal_yaw");
        if delta > 45.0 && delta < 315.0 {
            // Not turned far enough: don't take the step (but report success).
            vm.ent_set_vector(ent, "origin", oldorigin);
        }
        link_edict(vm, ent);
        // Monster think path: keep the live `time` global (see FIX-1).
        let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
        touch_triggers(vm, ent, time);
        return true;
    }
    link_edict(vm, ent);
    // Monster think path: keep the live `time` global (see FIX-1).
    let time = vm.sv_time; // SV_TouchLinks uses sv.time, not the per-think time global
    touch_triggers(vm, ent, time);
    false
}

/// `SV_FixCheckBottom` (sv_move.c ~267): mark `ent` `FL_PARTIALGROUND` so the
/// next [`sv_movestep`] tolerates a missing standing position.
fn sv_fix_check_bottom(vm: &mut Vm, ent: i32) {
    let flags = vm.ent_get_float(ent, "flags") as i32;
    vm.ent_set_float(ent, "flags", (flags | FL_PARTIALGROUND) as f32);
}

/// `SV_NewChaseDir` (sv_move.c ~283): pick a new movement direction for `actor`
/// toward `enemy` and step that way.
///
/// Faithful to id's heuristic: derive the preferred X (`d[1]`) and Y (`d[2]`)
/// directions from the signed deltas to the enemy, try the diagonal when both
/// axes have a preference, then the individual axes (optionally swapped at
/// random or when the Y delta dominates), then the old direction, then a full
/// 45-degree sweep (forward or backward at random), and finally the turnaround.
/// If nothing works the actor keeps its old yaw and `FL_PARTIALGROUND` is set
/// when it has no floor (via [`sv_fix_check_bottom`]). `rand()&n` is the VM's
/// deterministic LCG so tests are reproducible.
/// A small deterministic LCG for the AI's `rand()&n` symmetry-breaking in
/// chase-direction selection. The C used libc `rand()`; here a process-global
/// LCG keeps chase behaviour varied yet reproducible across runs/tests.
fn ai_rand() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEED: AtomicU32 = AtomicU32::new(0x1234_5678);
    let next = SEED
        .load(Ordering::Relaxed)
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12_345);
    SEED.store(next, Ordering::Relaxed);
    (next >> 16) & 0x7fff
}

pub fn sv_new_chase_dir(vm: &mut Vm, actor: i32, enemy: i32, dist: f32) {
    let ideal_yaw = vm.ent_get_float(actor, "ideal_yaw");
    let olddir = crate::math::anglemod(((ideal_yaw / 45.0) as i32 as f32) * 45.0);
    let turnaround = crate::math::anglemod(olddir - 180.0);

    let actor_org = vm.ent_get_vector(actor, "origin");
    let enemy_org = vm.ent_get_vector(enemy, "origin");
    let deltax = enemy_org[0] - actor_org[0];
    let deltay = enemy_org[1] - actor_org[1];

    // d[1] = preferred X-axis yaw, d[2] = preferred Y-axis yaw (DI_NODIR = none).
    let mut d1 = if deltax > 10.0 {
        0.0
    } else if deltax < -10.0 {
        180.0
    } else {
        DI_NODIR
    };
    let mut d2 = if deltay < -10.0 {
        270.0
    } else if deltay > 10.0 {
        90.0
    } else {
        DI_NODIR
    };

    // Try the direct diagonal route when both axes have a preference.
    if d1 != DI_NODIR && d2 != DI_NODIR {
        let tdir = if d1 == 0.0 {
            if d2 == 90.0 { 45.0 } else { 315.0 }
        } else if d2 == 90.0 {
            135.0
        } else {
            215.0
        };
        if tdir != turnaround && sv_step_direction(vm, actor, tdir, dist) {
            return;
        }
    }

    // Try the other directions; randomly (or when Y dominates) swap the axes.
    if (ai_rand() & 3) & 1 != 0 || deltay.abs() > deltax.abs() {
        std::mem::swap(&mut d1, &mut d2);
    }

    if d1 != DI_NODIR && d1 != turnaround && sv_step_direction(vm, actor, d1, dist) {
        return;
    }
    if d2 != DI_NODIR && d2 != turnaround && sv_step_direction(vm, actor, d2, dist) {
        return;
    }

    // No direct path: try the old direction.
    if olddir != DI_NODIR && sv_step_direction(vm, actor, olddir, dist) {
        return;
    }

    // Sweep every 45 degrees, in a randomly chosen order.
    if ai_rand() & 1 != 0 {
        let mut tdir = 0.0;
        while tdir <= 315.0 {
            if tdir != turnaround && sv_step_direction(vm, actor, tdir, dist) {
                return;
            }
            tdir += 45.0;
        }
    } else {
        let mut tdir = 315.0;
        while tdir >= 0.0 {
            if tdir != turnaround && sv_step_direction(vm, actor, tdir, dist) {
                return;
            }
            tdir -= 45.0;
        }
    }

    if turnaround != DI_NODIR && sv_step_direction(vm, actor, turnaround, dist) {
        return;
    }

    // Can't move: keep the old yaw and, if no floor, mark partial ground.
    vm.ent_set_float(actor, "ideal_yaw", olddir);
    if !sv_check_bottom(vm, actor) {
        sv_fix_check_bottom(vm, actor);
    }
}

/// `SV_CloseEnough` (sv_move.c ~371): is `goal`'s box within `dist` of `ent`'s
/// box on every axis? (Used by [`sv_move_to_goal`] to stop when adjacent.)
fn sv_close_enough(vm: &mut Vm, ent: i32, goal: i32, dist: f32) -> bool {
    let ent_absmin = vm.ent_get_vector(ent, "absmin");
    let ent_absmax = vm.ent_get_vector(ent, "absmax");
    let goal_absmin = vm.ent_get_vector(goal, "absmin");
    let goal_absmax = vm.ent_get_vector(goal, "absmax");
    for i in 0..3 {
        if goal_absmin[i] > ent_absmax[i] + dist {
            return false;
        }
        if goal_absmax[i] < ent_absmin[i] - dist {
            return false;
        }
    }
    true
}

/// `SV_MoveToGoal` (sv_move.c ~391): the QuakeC `movetogoal(dist)` builtin body.
///
/// For the `self` monster: do nothing (return 0) unless it is on ground / flying
/// / swimming. If it has an enemy and is already close enough to its goal, stop.
/// Otherwise step toward `ideal_yaw` (occasionally bumping to a fresh direction
/// at random), and on failure pick a [`sv_new_chase_dir`] toward the goal.
pub fn sv_move_to_goal(vm: &mut Vm, dist: f32) {
    let ent = vm.gget_int("self");
    let goal = vm.ent_get_int(ent, "goalentity");

    let flags = vm.ent_get_float(ent, "flags") as i32;
    if flags & (FL_ONGROUND | FL_FLY | FL_SWIM) == 0 {
        vm.ret_float(0.0);
        return;
    }

    // If the next step would reach the enemy goal, stop here.
    let enemy = vm.ent_get_int(ent, "enemy");
    if enemy > 0 && sv_close_enough(vm, ent, goal, dist) {
        return;
    }

    // Bump around: occasionally force a fresh chase direction.
    let ideal_yaw = vm.ent_get_float(ent, "ideal_yaw");
    if (ai_rand() & 3) == 1 || !sv_step_direction(vm, ent, ideal_yaw, dist) {
        sv_new_chase_dir(vm, ent, goal, dist);
    }
}

// ---------------------------------------------------------------------------
// (D3) The monster-movement engine builtins (pr_cmds.c).
// ---------------------------------------------------------------------------

/// `PF_walkmove` (#32): `float(float yaw, float dist) walkmove`. Steps `self`
/// `dist` units along `yaw` via [`sv_movestep`] (with relink), returning 1 on a
/// successful move and 0 otherwise. Like the C, it only moves a monster that is
/// on ground / flying / swimming and saves/restores `self` around the step
/// (`sv_movestep` may run touch progs that change `self`).
fn bi_walkmove(vm: &mut Vm) -> Result<()> {
    let ent = vm.gget_int("self");
    let yaw = vm.arg_float(0);
    let dist = vm.arg_float(1);

    let flags = vm.ent_get_float(ent, "flags") as i32;
    if flags & (FL_ONGROUND | FL_FLY | FL_SWIM) == 0 {
        vm.ret_float(0.0);
        return Ok(());
    }

    let rad = yaw * std::f32::consts::PI / 180.0;
    let mov: Vec3 = [rad.cos() * dist, rad.sin() * dist, 0.0];

    // Save program state (self), because sv_movestep may run other progs.
    let oldself = vm.gget_int("self");
    let ok = sv_movestep(vm, ent, mov, true);
    vm.gset_int("self", oldself);

    vm.ret_float(if ok { 1.0 } else { 0.0 });
    Ok(())
}

/// `PF_movetogoal` (#67): `void(float step) movetogoal` — calls
/// [`sv_move_to_goal`] with the step distance. Wired over the old `bi_ret_zero`
/// stub.
fn bi_movetogoal(vm: &mut Vm) -> Result<()> {
    let dist = vm.arg_float(0);
    sv_move_to_goal(vm, dist);
    Ok(())
}

/// `PF_checkbottom` (#40): `float(entity e) checkbottom` — returns 1 when `e`
/// has floor under its whole box ([`sv_check_bottom`]), else 0.
fn bi_checkbottom(vm: &mut Vm) -> Result<()> {
    let ent = vm.arg_entity(0);
    let ok = sv_check_bottom(vm, ent);
    vm.ret_float(if ok { 1.0 } else { 0.0 });
    Ok(())
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

/// `V_CalcRoll` (view.c:81): how far the view/body banks (rolls) when strafing.
///
/// Used by both the client view and `SV_ClientThink` (which multiplies the
/// result by 4 to set the player body's `angles[ROLL]`). The sign follows the
/// strafe direction (the dot of velocity with the right vector), the magnitude
/// ramps from 0 up to `cl_rollangle` (2.0 deg) as the sideways speed climbs to
/// `cl_rollspeed` (200 u/s), then clamps. We carry id's stock cvar defaults as
/// constants — this headless server has no cvar registry, but these are the
/// values a default config uses.
///
/// ```text
/// AngleVectors(angles) -> right
/// side = DotProduct(velocity, right)
/// sign = side < 0 ? -1 : 1
/// side = |side|
/// side = side < rollspeed ? side*rollangle/rollspeed : rollangle
/// return side * sign
/// ```
pub fn v_calc_roll(angles: Vec3, velocity: Vec3) -> f32 {
    /// `cl_rollangle` default ("2.0").
    const CL_ROLLANGLE: f32 = 2.0;
    /// `cl_rollspeed` default ("200").
    const CL_ROLLSPEED: f32 = 200.0;

    let (_forward, right, _up) = angle_vectors(angles);
    let raw = crate::math::dot(velocity, right);
    let sign = if raw < 0.0 { -1.0 } else { 1.0 };
    let side = raw.abs();
    let side = if side < CL_ROLLSPEED {
        side * CL_ROLLANGLE / CL_ROLLSPEED
    } else {
        CL_ROLLANGLE
    };
    side * sign
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

// ---------------------------------------------------------------------------
// value parsers (atof / atoi semantics).
// ---------------------------------------------------------------------------

/// `atof`-like float parse: take the leading numeric prefix, default 0.0. C's
/// `atof` stops at the first non-numeric char and never errors. `pub(crate)`
/// for the savegame loader's `ED_ParseEpair` / header floats.
pub(crate) fn parse_float(s: &str) -> f32 {
    let t = s.trim_start();
    // Find the longest leading prefix that parses; fall back to 0.0.
    let bytes = t.as_bytes();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < bytes.len() {
        let c = bytes[end];
        let ok = match c {
            b'0'..=b'9' => true,
            b'+' | b'-' => end == 0 || bytes[end - 1] == b'e' || bytes[end - 1] == b'E',
            b'.' if !seen_dot && !seen_e => {
                seen_dot = true;
                true
            }
            b'e' | b'E' if !seen_e && end > 0 => {
                seen_e = true;
                true
            }
            _ => false,
        };
        if !ok {
            break;
        }
        end += 1;
    }
    t.get(..end).and_then(|p| p.parse::<f32>().ok()).unwrap_or(0.0)
}

/// `atoi`-like int parse: leading optional sign then digits, default 0.
/// `pub(crate)` for the savegame loader's `ev_entity` epair.
pub(crate) fn parse_int(s: &str) -> i32 {
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let c = bytes[end];
        let ok = c.is_ascii_digit() || ((c == b'+' || c == b'-') && end == 0);
        if !ok {
            break;
        }
        end += 1;
    }
    t.get(..end).and_then(|p| p.parse::<i32>().ok()).unwrap_or(0)
}

/// Parse a "x y z" vector, `atof`-style on each of the first three
/// space-separated fields (missing fields are 0.0), matching `ED_ParseEpair`'s
/// `ev_vector` loop. `pub(crate)` for the savegame loader's `ev_vector` epair.
pub(crate) fn parse_vector(s: &str) -> Vec3 {
    let mut out = [0.0f32; 3];
    for (i, field) in s.split_whitespace().take(3).enumerate() {
        out[i] = parse_float(field);
    }
    out
}

/// Increment the running count for `classname`.
fn bump_classname(counts: &mut Vec<(String, usize)>, classname: &str) {
    if let Some(entry) = counts.iter_mut().find(|(c, _)| c == classname) {
        entry.1 += 1;
    } else {
        counts.push((classname.to_string(), 1));
    }
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
    use crate::progs::{
        Def, Function, Op, Statement, MAX_PARMS, OFS_PARM0, OFS_RETURN, PROG_VERSION, RESERVED_OFS,
    };

    const HEADER_SIZE: usize = 60;

    // ---- synthetic progs.dat builder (mirrors vm.rs's test serializer) ----

    fn ser_stmt(s: &Statement) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&s.op.to_le_bytes());
        v.extend_from_slice(&s.a.to_le_bytes());
        v.extend_from_slice(&s.b.to_le_bytes());
        v.extend_from_slice(&s.c.to_le_bytes());
        v
    }
    fn ser_def(d: &Def) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&d.type_.to_le_bytes());
        v.extend_from_slice(&d.ofs.to_le_bytes());
        v.extend_from_slice(&d.s_name.to_le_bytes());
        v
    }
    fn ser_func(f: &Function) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [
            f.first_statement,
            f.parm_start,
            f.locals,
            f.profile,
            f.s_name,
            f.s_file,
            f.numparms,
        ] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&f.parm_size);
        v
    }

    struct Builder {
        strings: Vec<u8>,
        statements: Vec<Statement>,
        globaldefs: Vec<Def>,
        fielddefs: Vec<Def>,
        functions: Vec<Function>,
        nglobals: usize,
        entityfields: i32,
    }

    impl Builder {
        fn new() -> Builder {
            Builder {
                strings: vec![0u8],
                statements: Vec::new(),
                globaldefs: Vec::new(),
                fielddefs: Vec::new(),
                functions: vec![Function {
                    first_statement: 0,
                    parm_start: 0,
                    locals: 0,
                    profile: 0,
                    s_name: 0,
                    s_file: 0,
                    numparms: 0,
                    parm_size: [0; MAX_PARMS],
                }],
                nglobals: 128,
                entityfields: 0,
            }
        }
        fn intern(&mut self, s: &str) -> i32 {
            let ofs = self.strings.len() as i32;
            self.strings.extend_from_slice(s.as_bytes());
            self.strings.push(0);
            ofs
        }
        /// Add a global def of `type_` at `ofs` named `name`.
        fn add_global(&mut self, name: &str, type_: u16, ofs: u16) {
            let s = self.intern(name);
            self.globaldefs.push(Def {
                type_,
                ofs,
                s_name: s,
            });
        }
        /// Add a field def of `type_` at `ofs` named `name`.
        fn add_field(&mut self, name: &str, type_: u16, ofs: u16) {
            let s = self.intern(name);
            self.fielddefs.push(Def {
                type_,
                ofs,
                s_name: s,
            });
        }
        /// Add a bytecode function `name` with `stmts`; returns its index.
        fn add_function(&mut self, name: &str, stmts: Vec<Statement>) -> usize {
            let first = self.statements.len() as i32;
            let s_name = self.intern(name);
            self.statements.extend(stmts);
            self.functions.push(Function {
                first_statement: first,
                parm_start: RESERVED_OFS as i32,
                locals: 0,
                profile: 0,
                s_name,
                s_file: 0,
                numparms: 0,
                parm_size: [0; MAX_PARMS],
            });
            self.functions.len() - 1
        }
        /// Add a builtin function record (`first_statement = -builtin_num`) named
        /// `name`, so QuakeC can `CALL` into the engine builtin table; returns its
        /// function index.
        fn add_builtin(&mut self, name: &str, builtin_num: i32) -> usize {
            let s_name = self.intern(name);
            self.functions.push(Function {
                first_statement: -builtin_num,
                parm_start: RESERVED_OFS as i32,
                locals: 0,
                profile: 0,
                s_name,
                s_file: 0,
                numparms: 0,
                parm_size: [0; MAX_PARMS],
            });
            self.functions.len() - 1
        }
        fn build(&self) -> Vec<u8> {
            let globals: Vec<u32> = vec![0u32; self.nglobals];
            let mut body = Vec::new();
            let ofs_statements = HEADER_SIZE + body.len();
            for s in &self.statements {
                body.extend_from_slice(&ser_stmt(s));
            }
            let ofs_globaldefs = HEADER_SIZE + body.len();
            for d in &self.globaldefs {
                body.extend_from_slice(&ser_def(d));
            }
            let ofs_fielddefs = HEADER_SIZE + body.len();
            for d in &self.fielddefs {
                body.extend_from_slice(&ser_def(d));
            }
            let ofs_functions = HEADER_SIZE + body.len();
            for f in &self.functions {
                body.extend_from_slice(&ser_func(f));
            }
            let ofs_strings = HEADER_SIZE + body.len();
            body.extend_from_slice(&self.strings);
            let ofs_globals = HEADER_SIZE + body.len();
            for g in &globals {
                body.extend_from_slice(&g.to_le_bytes());
            }
            let header: [i32; 15] = [
                PROG_VERSION,
                0,
                ofs_statements as i32,
                self.statements.len() as i32,
                ofs_globaldefs as i32,
                self.globaldefs.len() as i32,
                ofs_fielddefs as i32,
                self.fielddefs.len() as i32,
                ofs_functions as i32,
                self.functions.len() as i32,
                ofs_strings as i32,
                self.strings.len() as i32,
                ofs_globals as i32,
                globals.len() as i32,
                self.entityfields,
            ];
            let mut out = Vec::new();
            for x in header {
                out.extend_from_slice(&x.to_le_bytes());
            }
            out.extend_from_slice(&body);
            out
        }
    }

    /// An empty BSP (no geometry); world queries are total and report SOLID.
    fn empty_bsp() -> Bsp {
        Bsp {
            version: crate::bsp::BSPVERSION,
            entities: String::new(),
            planes: Vec::new(),
            vertexes: Vec::new(),
            edges: Vec::new(),
            faces: Vec::new(),
            nodes: Vec::new(),
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo: Vec::new(),
            models: Vec::new(),
            marksurfaces: Vec::new(),
            surfedges: Vec::new(),
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: Vec::new(),
        }
    }

    /// A BSP carrying a specific entity text blob.
    fn bsp_with_entities(text: &str) -> Bsp {
        let mut b = empty_bsp();
        b.entities = text.to_string();
        b
    }

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

    // ev_* type codes (etype_t ordinals; see progs::EType).
    const EV_STRING: u16 = 1;
    const EV_FLOAT: u16 = 2;
    const EV_FUNCTION: u16 = 6;

    // -------------------------------------------------------------- tokenizer

    #[test]
    fn tokenizer_basic_pairs_and_braces() {
        let mut t = Tokenizer::new("{ \"classname\" \"worldspawn\" }");
        assert_eq!(t.next_token().as_deref(), Some("{"));
        assert_eq!(t.next_token().as_deref(), Some("classname"));
        assert_eq!(t.next_token().as_deref(), Some("worldspawn"));
        assert_eq!(t.next_token().as_deref(), Some("}"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn tokenizer_skips_line_comments_and_words() {
        let mut t = Tokenizer::new("// a comment\nword1   word2\n{ }");
        assert_eq!(t.next_token().as_deref(), Some("word1"));
        assert_eq!(t.next_token().as_deref(), Some("word2"));
        assert_eq!(t.next_token().as_deref(), Some("{"));
        assert_eq!(t.next_token().as_deref(), Some("}"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn tokenizer_unterminated_quote_is_total() {
        // A quote with no closing " must end the token at EOF, not loop/panic.
        let mut t = Tokenizer::new("\"unterminated");
        assert_eq!(t.next_token().as_deref(), Some("unterminated"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn ed_new_string_handles_escapes() {
        assert_eq!(ed_new_string("a\\nb"), "a\nb");
        assert_eq!(ed_new_string("a\\tb"), "a\\b"); // non-n escape -> backslash
        assert_eq!(ed_new_string("plain"), "plain");
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_float("3.5 abc"), 3.5);
        assert_eq!(parse_float("notanumber"), 0.0);
        assert_eq!(parse_int("-42 x"), -42);
        assert_eq!(parse_vector("1 2 3"), [1.0, 2.0, 3.0]);
        assert_eq!(parse_vector("1 2"), [1.0, 2.0, 0.0]);
    }

    // ------------------------------------------------------------ spawn flow

    /// Build a progs whose "marker" classname spawn function sets a global float
    /// `spawned_flag` to 1.0, so we can prove the spawner executed it. Also adds
    /// a "classname" string field and "spawnflags"/"think"/"nextthink" fields.
    fn marker_progs() -> (Vec<u8>, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 8;

        // Globals: a float "spawned_flag" at offset 30, plus the well-known
        // self/other/time/world globals the server sets.
        let g_flag = 30u16;
        b.add_global("spawned_flag", EV_FLOAT, g_flag);
        b.add_global("self", 4 /*ev_entity*/, 31);
        b.add_global("other", 4, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", 4, 34);
        b.add_global("frametime", EV_FLOAT, 35);

        // Fields: classname(string)@1, spawnflags(float)@2, think(function)@3,
        // nextthink(float)@4, frame(float)@5, origin(vector)@5? keep simple.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("spawnflags", EV_FLOAT, 2);
        b.add_field("think", EV_FUNCTION, 3);
        b.add_field("nextthink", EV_FLOAT, 4);

        // Spawn function "marker": STORE_F const(1.0) -> spawned_flag; DONE.
        // We need a global holding 1.0; put it at offset 40 and set it after load.
        let g_one = 40u16;
        let marker = b.add_function(
            "marker",
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
        (img, marker, g_one as usize)
    }

    #[test]
    fn spawn_entities_runs_spawn_function() {
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        let ents = "{ \"classname\" \"worldspawn\" }\n{ \"classname\" \"marker\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        // Set the constant 1.0 the marker spawn stores into spawned_flag.
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");

        // Two entity blocks; "marker" has a spawn function, "worldspawn" does
        // not (no function named worldspawn) -> no_spawn_function = 1, but it is
        // edict 0 (world) and freeing it is a no-op.
        assert_eq!(report.total, 2);
        assert_eq!(report.spawned, 1, "marker spawn function ran");
        assert_eq!(report.no_spawn_function, 1, "worldspawn has no spawn fn");

        // The spawn function set the global flag to 1.0.
        assert_eq!(
            server.vm.gget_float("spawned_flag"),
            1.0,
            "marker's spawn function executed and set the global"
        );

        // classnames report contains both, sorted by count desc then name.
        assert!(report
            .classnames
            .iter()
            .any(|(c, n)| c == "marker" && *n == 1));
    }

    #[test]
    fn spawn_entities_inhibits_not_medium() {
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        // Entity with spawnflags NOT_MEDIUM (512) must be inhibited at skill 1.
        let ents = "{ \"classname\" \"marker\" \"spawnflags\" \"512\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");
        assert_eq!(report.total, 1);
        assert_eq!(report.inhibited, 1, "NOT_MEDIUM entity inhibited at skill 1");
        assert_eq!(report.spawned, 0);
        // The spawn function must NOT have run.
        assert_eq!(server.vm.gget_float("spawned_flag"), 0.0);
    }

    #[test]
    fn spawn_entities_bad_entity_does_not_abort() {
        // An entity whose classname has no spawn function is counted, not fatal,
        // and the following good entity still spawns.
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        let ents = "{ \"classname\" \"unknown_thing\" }\n{ \"classname\" \"marker\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");
        assert_eq!(report.total, 2);
        assert_eq!(report.no_spawn_function, 1);
        assert_eq!(report.spawned, 1);
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
    fn skill_filter_honours_each_difficulty_flag() {
        // FIX-1: the spawn filter must honour NOT_EASY (skill 0), NOT_MEDIUM
        // (skill 1) and NOT_HARD (skill>=2), not just NOT_MEDIUM. An entity
        // flagged NOT_HARD must spawn at skill 0/1 and be inhibited at skill 2/3.
        let make = |skill: f32, spawnflags: i32| -> SpawnReport {
            let (img, _marker, g_one) = marker_progs();
            let progs = Progs::parse(&img).expect("parse");
            let ents = format!(
                "{{ \"classname\" \"marker\" \"spawnflags\" \"{spawnflags}\" }}\n"
            );
            let mut server = Server::new(bsp_with_entities(&ents), progs).expect("server");
            server.vm.set_gf(g_one, 1.0);
            server.set_skill(skill);
            server.spawn_entities().expect("spawn")
        };

        // NOT_HARD (1024): kept on easy/medium, dropped on hard/nightmare.
        assert_eq!(make(0.0, 1024).inhibited, 0, "NOT_HARD spawns at easy");
        assert_eq!(make(1.0, 1024).inhibited, 0, "NOT_HARD spawns at medium");
        assert_eq!(make(2.0, 1024).inhibited, 1, "NOT_HARD inhibited at hard");
        assert_eq!(make(3.0, 1024).inhibited, 1, "NOT_HARD inhibited at nightmare");

        // NOT_EASY (256): dropped only on easy.
        assert_eq!(make(0.0, 256).inhibited, 1, "NOT_EASY inhibited at easy");
        assert_eq!(make(1.0, 256).inhibited, 0, "NOT_EASY spawns at medium");
        assert_eq!(make(2.0, 256).inhibited, 0, "NOT_EASY spawns at hard");

        // NOT_MEDIUM (512): dropped only on medium (the prior behaviour, intact).
        assert_eq!(make(0.0, 512).inhibited, 0, "NOT_MEDIUM spawns at easy");
        assert_eq!(make(1.0, 512).inhibited, 1, "NOT_MEDIUM inhibited at medium");
        assert_eq!(make(2.0, 512).inhibited, 0, "NOT_MEDIUM spawns at hard");

        // A monster with no skill flags always spawns.
        assert_eq!(make(2.0, 0).spawned, 1, "unflagged entity spawns at any skill");
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

    // ----------------------------------------------- entity-aware move / touch

    const EV_VECTOR: u16 = 3;
    const EV_ENTITY: u16 = 4;

    /// Build a progs whose "do_touch" function stores the constant at `g_one`
    /// into the global `touched_flag` (offset `g_flag`). Field/global layout is
    /// shared by the sv_move and touch_triggers tests so a synthetic Server can
    /// place SOLID_BBOX / SOLID_TRIGGER edicts and run them. Returns
    /// `(image, touch_fn_index, g_one_offset, g_flag_offset)`.
    fn touch_progs() -> (Vec<u8>, usize, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 32;

        // Globals.
        let g_flag = 30u16;
        b.add_global("touched_flag", EV_FLOAT, g_flag);
        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        let g_one = 40u16;

        // Fields the move/touch code reads.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("solid", EV_FLOAT, 2);
        b.add_field("touch", EV_FUNCTION, 3);
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
        b.add_field("owner", EV_ENTITY, 30); // SV_ClipToLinks owner-skip tests

        let touch_fn = b.add_function(
            "do_touch",
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
        (img, touch_fn, g_one as usize, g_flag as usize)
    }

    /// Like [`touch_progs`], but the touch function copies the current `time`
    /// global into `touched_flag`, so a test can observe exactly which `time`
    /// value was live when the touch ran. Returns `(img, touch_fn, g_flag)`.
    fn time_recording_touch_progs() -> (Vec<u8>, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 32;

        let g_flag = 30u16;
        b.add_global("touched_flag", EV_FLOAT, g_flag);
        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        let g_time = 33u16;
        b.add_global("time", EV_FLOAT, g_time);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);

        b.add_field("classname", EV_STRING, 1);
        b.add_field("solid", EV_FLOAT, 2);
        b.add_field("touch", EV_FUNCTION, 3);
        b.add_field("origin", EV_VECTOR, 4);
        b.add_field("mins", EV_VECTOR, 7);
        b.add_field("maxs", EV_VECTOR, 10);
        b.add_field("absmin", EV_VECTOR, 13);
        b.add_field("absmax", EV_VECTOR, 16);
        b.add_field("model", EV_STRING, 19);
        b.add_field("movetype", EV_FLOAT, 20);
        b.add_field("nextthink", EV_FLOAT, 21);
        b.add_field("flags", EV_FLOAT, 22);
        b.add_field("velocity", EV_VECTOR, 23);
        b.add_field("size", EV_VECTOR, 26);
        b.add_field("groundentity", EV_ENTITY, 29);
        b.add_field("owner", EV_ENTITY, 30);

        // touched_flag = time;  (record the live time global, then DONE)
        let touch_fn = b.add_function(
            "record_time",
            vec![
                Statement { op: Op::StoreF as u16, a: g_time as i16, b: g_flag as i16, c: 0 },
                Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
            ],
        );

        (b.build(), touch_fn, g_flag as usize)
    }

    #[test]
    fn touch_triggers_resets_time_global_to_sv_time_before_each_touch() {
        // FIX-1: SV_TouchLinks (world.c:304) sets pr_global_struct->time = sv.time
        // before each trigger touch. Seed the `time` global with a STALE value (a
        // prior entity's clamped thinktime) and confirm the touch sees the
        // start-of-frame sv.time we pass, not the stale value.
        let (img, touch_fn, g_flag) = time_recording_touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        // A stale time left in the global (e.g. a think clamped to time+frametime).
        server.vm.gset_float("time", 99.0);

        let mover = server.vm.spawn();
        server.vm.ent_set_vector(mover, "absmin", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(mover, "absmax", [16.0, 16.0, 16.0]);

        let trigger = server.vm.spawn();
        server.vm.ent_set_float(trigger, "solid", SOLID_TRIGGER as f32);
        server.vm.ent_set_int(trigger, "touch", touch_fn as i32);
        server.vm.ent_set_vector(trigger, "absmin", [-8.0, -8.0, -8.0]);
        server.vm.ent_set_vector(trigger, "absmax", [8.0, 8.0, 8.0]);

        let sv_time = 5.0;
        touch_triggers(&mut server.vm, mover, sv_time);
        assert_eq!(
            server.vm.gf(g_flag),
            sv_time,
            "the trigger touch ran with sv.time, not the stale 99.0"
        );
    }

    #[test]
    fn sv_impact_resets_time_global_to_sv_time_before_touch() {
        // FIX-1: SV_Impact (sv_phys.c:160) sets pr_global_struct->time = sv.time
        // before running the touch functions. With a stale `time` global, the
        // impacted entities' touch must still observe the passed sv.time.
        let (img, touch_fn, g_flag) = time_recording_touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        server.vm.gset_float("time", 42.0); // stale

        let e1 = server.vm.spawn();
        server.vm.ent_set_float(e1, "solid", SOLID_BBOX as f32);
        let e2 = server.vm.spawn();
        server.vm.ent_set_float(e2, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_int(e2, "touch", touch_fn as i32);

        let sv_time = 7.25;
        sv_impact(&mut server.vm, e1, e2, sv_time);
        assert_eq!(
            server.vm.gf(g_flag),
            sv_time,
            "sv_impact ran the touch with sv.time, not the stale 42.0"
        );
    }

    #[test]
    fn sv_move_stops_at_solid_bbox_entity() {
        // Place a SOLID_BBOX edict ahead at x=100 (a 32-cube) and trace a point
        // through it from the origin to x=200. The move must stop on the box,
        // and the trace's ent must be that edict.
        let (img, _touch, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        // An *empty* BSP has headnode 0 out of range -> trace_world reports the
        // whole world solid at fraction 0, which would mask the entity. Use a
        // world with a real empty leaf so the world trace runs clear, letting
        // the entity collision show through.
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        // The blocker entity.
        let blocker = server.vm.spawn();
        server.vm.ent_set_float(blocker, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(blocker, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(blocker, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(blocker, "absmin", [84.0, -16.0, -16.0]);
        server.vm.ent_set_vector(blocker, "absmax", [116.0, 16.0, 16.0]);

        let tr = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            -1,    // ignore nothing
            false, // clip all solids
            false, // not a missile move
        );

        assert!(tr.fraction < 1.0, "the move was clipped, got {}", tr.fraction);
        assert_eq!(tr.ent, blocker, "the SOLID_BBOX edict was the blocker");
        assert!(tr.endpos[0] < 84.0, "stopped before the box, got {}", tr.endpos[0]);
    }

    #[test]
    fn sv_move_nomonsters_passes_through_box_entity() {
        // MOVE_NOMONSTERS must skip SOLID_BBOX / SOLID_SLIDEBOX boxes: the same
        // box that blocks an ordinary move is transparent to a nomonsters
        // trace, so the move reaches the far end at fraction 1.0. This is the
        // semantics QC visible() relies on for passive sight acquisition.
        let (img, _touch, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let blocker = server.vm.spawn();
        server.vm.ent_set_float(blocker, "solid", SOLID_SLIDEBOX as f32);
        server.vm.ent_set_vector(blocker, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(blocker, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(blocker, "absmin", [84.0, -16.0, -16.0]);
        server.vm.ent_set_vector(blocker, "absmax", [116.0, 16.0, 16.0]);

        let tr = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            -1,   // ignore nothing
            true, // MOVE_NOMONSTERS: skip box entities
            false, // not a missile move
        );

        assert_eq!(tr.fraction, 1.0, "nomonsters trace passed through the box");
        assert_eq!(tr.ent, -1, "clear move hit nothing");
    }

    #[test]
    fn sv_move_ignores_passedict() {
        // The blocker is the same edict we pass as `ignore` -> not clipped.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        let blocker = server.vm.spawn();
        server.vm.ent_set_float(blocker, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(blocker, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(blocker, "maxs", [16.0, 16.0, 16.0]);

        let tr = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            blocker, // ignore the blocker
            false,   // clip all solids
            false,   // not a missile move
        );
        assert_eq!(tr.fraction, 1.0, "ignored edict did not block");
        assert_eq!(tr.ent, -1, "clear move hit nothing");
    }

    #[test]
    fn sv_move_skips_owner_and_own_missile() {
        // SV_ClipToLinks owner skip: a missile (passedict = shooter via owner)
        // must not clip against its owner, and the owner must not clip against
        // its own missile. We model the projectile trace as the missile moving
        // forward with the shooter (a SOLID_BBOX box) sitting at the start.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");

        // Case A: trace ignores the missile; the blocker IS the missile's owner.
        // ent_get_int(blocker, "owner") == ignore (missile) is false here; the
        // relevant predicate is ent_get_int(ignore, "owner") == blocker.
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        let shooter = server.vm.spawn();
        server.vm.ent_set_float(shooter, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(shooter, "origin", [50.0, 0.0, 0.0]);
        server.vm.ent_set_vector(shooter, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(shooter, "maxs", [16.0, 16.0, 16.0]);

        let missile = server.vm.spawn();
        server.vm.ent_set_float(missile, "solid", SOLID_BBOX as f32);
        // The missile's owner is the shooter: don't clip against the owner.
        server.vm.ent_set_int(missile, "owner", shooter);

        let tr = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            missile, // passedict = the missile
            false,
            false,
        );
        assert_eq!(tr.fraction, 1.0, "missile passed through its owner");
        assert_eq!(tr.ent, -1, "owner did not block the missile");

        // Case B: the owner traces and the blocker is its OWN missile (the
        // missile's owner == the passedict). The owner must not clip against it.
        let (img2, _t2, _g2, _gf2) = touch_progs();
        let progs2 = Progs::parse(&img2).expect("parse");
        let mut server2 = Server::new(world_open_bsp(), progs2).expect("server");
        let shooter2 = server2.vm.spawn();
        let missile2 = server2.vm.spawn();
        server2.vm.ent_set_float(missile2, "solid", SOLID_BBOX as f32);
        server2.vm.ent_set_vector(missile2, "origin", [100.0, 0.0, 0.0]);
        server2.vm.ent_set_vector(missile2, "mins", [-16.0, -16.0, -16.0]);
        server2.vm.ent_set_vector(missile2, "maxs", [16.0, 16.0, 16.0]);
        // missile2.owner == shooter2 (the passedict): skip own missile.
        server2.vm.ent_set_int(missile2, "owner", shooter2);

        let tr2 = sv_move(
            &mut server2.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            shooter2, // passedict = the owner
            false,
            false,
        );
        assert_eq!(tr2.fraction, 1.0, "owner passed through its own missile");
        assert_eq!(tr2.ent, -1, "own missile did not block the owner");

        // Control: an unrelated SOLID_BBOX (no owner relationship) DOES block.
        let (img3, _t3, _g3, _gf3) = touch_progs();
        let progs3 = Progs::parse(&img3).expect("parse");
        let mut server3 = Server::new(world_open_bsp(), progs3).expect("server");
        let shooter3 = server3.vm.spawn();
        let other = server3.vm.spawn();
        server3.vm.ent_set_float(other, "solid", SOLID_BBOX as f32);
        server3.vm.ent_set_vector(other, "origin", [100.0, 0.0, 0.0]);
        server3.vm.ent_set_vector(other, "mins", [-16.0, -16.0, -16.0]);
        server3.vm.ent_set_vector(other, "maxs", [16.0, 16.0, 16.0]);
        // No owner relationship between shooter3 and other.

        let tr3 = sv_move(
            &mut server3.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            shooter3,
            false,
            false,
        );
        assert!(tr3.fraction < 1.0, "an unrelated box still blocks the move");
        assert_eq!(tr3.ent, other, "the unrelated box was the blocker");
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
    fn sv_move_missile_expands_box_for_monsters() {
        // MOVE_MISSILE clips FL_MONSTER touch entities against a +-15 box
        // (world.c SV_Move sets clip.mins2/maxs2 = +-15, SV_ClipToLinks uses it
        // for the FL_MONSTER branch). A point missile traced 16 units to the
        // side of a *point* monster would miss with the move's own zero box, but
        // the +-15 expansion makes it clip — a rocket detonates NEAR a monster.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");

        // The monster is a tiny box centred at y=+20; a point trace along x at
        // y=0 stays 20 units away on y, outside the monster's own box but inside
        // the +-15 expanded box (20 - 15 = 5 <= the monster's own +5 half-width).
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        let monster = server.vm.spawn();
        server.vm.ent_set_float(monster, "solid", SOLID_SLIDEBOX as f32);
        server.vm.ent_set_float(monster, "flags", FL_MONSTER as f32);
        server.vm.ent_set_vector(monster, "origin", [100.0, 20.0, 0.0]);
        server.vm.ent_set_vector(monster, "mins", [-5.0, -5.0, -5.0]);
        server.vm.ent_set_vector(monster, "maxs", [5.0, 5.0, 5.0]);

        // Non-missile point trace at y=0: misses the monster (gap is 15 on y;
        // monster's own box only reaches y=15, the point path is at y=0... it is
        // 5 units clear). Confirm a normal move passes through.
        let normal = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            -1,
            false,
            false, // MOVE_NORMAL
        );
        assert_eq!(normal.fraction, 1.0, "normal point trace misses the monster");
        assert_eq!(normal.ent, -1);

        // Missile trace at the same y=0: the +-15 expansion reaches the monster
        // box (expanded bmin y = origin.y + (-5) - 15 = 0, the path is at y=0),
        // so it clips and stops short.
        let missile = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            -1,
            false,
            true, // MOVE_MISSILE
        );
        assert!(
            missile.fraction < 1.0,
            "missile expanded box clips the nearby monster, got {}",
            missile.fraction
        );
        assert_eq!(missile.ent, monster, "the monster was the blocker");
    }

    #[test]
    fn sv_move_missile_does_not_expand_box_for_non_monsters() {
        // A non-monster box (no FL_MONSTER flag) keeps the move's own box even
        // in missile mode: a point trace that misses it without expansion still
        // misses it, so we never over-detonate against items / gibs.
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        let item = server.vm.spawn();
        server.vm.ent_set_float(item, "solid", SOLID_BBOX as f32);
        // No FL_MONSTER flag set.
        server.vm.ent_set_vector(item, "origin", [100.0, 20.0, 0.0]);
        server.vm.ent_set_vector(item, "mins", [-5.0, -5.0, -5.0]);
        server.vm.ent_set_vector(item, "maxs", [5.0, 5.0, 5.0]);

        let missile = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            -1,
            false,
            true, // MOVE_MISSILE
        );
        assert_eq!(
            missile.fraction, 1.0,
            "non-monster keeps its own box, missile passes by"
        );
        assert_eq!(missile.ent, -1);
    }

    #[test]
    fn sv_move_box_passedict_skips_point_touch() {
        // SV_ClipToLinks points-never-interact skip (world.c ~843-844): a
        // box-sized passedict must not clip against a point-sized (size[0]==0)
        // touch entity. We give the mover (passedict) a real box, place a
        // zero-size SOLID_BBOX point in its path, and confirm the point is
        // skipped. A *box*-sized blocker in the same spot still blocks (control).
        let (img, _t, _g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");

        // The passedict / mover: a real box (size[0] = 32 != 0).
        let mover = server.vm.spawn();
        server.vm.ent_set_float(mover, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(mover, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(mover, "maxs", [16.0, 16.0, 16.0]);

        // A point-sized blocker (mins == maxs, so size[0] == 0) in the path.
        let point = server.vm.spawn();
        server.vm.ent_set_float(point, "solid", SOLID_BBOX as f32);
        server.vm.ent_set_vector(point, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(point, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(point, "maxs", [0.0, 0.0, 0.0]);

        let tr = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            mover, // box-sized passedict
            false,
            false,
        );
        assert_eq!(tr.fraction, 1.0, "point touch is skipped by the box passedict");
        assert_eq!(tr.ent, -1);

        // Control: give the same blocker a real box -> it blocks again.
        server.vm.ent_set_vector(point, "mins", [-8.0, -8.0, -8.0]);
        server.vm.ent_set_vector(point, "maxs", [8.0, 8.0, 8.0]);
        let tr2 = sv_move(
            &mut server.vm,
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [-16.0, -16.0, -16.0],
            [16.0, 16.0, 16.0],
            mover,
            false,
            false,
        );
        assert!(tr2.fraction < 1.0, "a box-sized blocker still blocks");
        assert_eq!(tr2.ent, point);
    }

    #[test]
    fn touch_triggers_fires_overlapping_trigger() {
        // A SOLID_TRIGGER edict with a touch function, overlapping the mover's
        // abs box, must have its touch run by touch_triggers.
        let (img, touch_fn, g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        // The mover (e.g. the player) at the origin, abs box [-16,16]^3.
        let mover = server.vm.spawn();
        server.vm.ent_set_vector(mover, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(mover, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(mover, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(mover, "absmin", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(mover, "absmax", [16.0, 16.0, 16.0]);

        // The trigger, overlapping the mover.
        let trigger = server.vm.spawn();
        server.vm.ent_set_float(trigger, "solid", SOLID_TRIGGER as f32);
        server.vm.ent_set_int(trigger, "touch", touch_fn as i32);
        server.vm.ent_set_vector(trigger, "absmin", [-8.0, -8.0, -8.0]);
        server.vm.ent_set_vector(trigger, "absmax", [8.0, 8.0, 8.0]);

        assert_eq!(server.vm.gget_float("touched_flag"), 0.0, "not yet touched");
        touch_triggers(&mut server.vm, mover, 0.0);
        assert_eq!(
            server.vm.gget_float("touched_flag"),
            1.0,
            "the overlapping trigger's touch function ran"
        );
    }

    #[test]
    fn touch_triggers_skips_non_overlapping() {
        // A trigger far away must NOT fire.
        let (img, touch_fn, g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        let mover = server.vm.spawn();
        server.vm.ent_set_vector(mover, "absmin", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(mover, "absmax", [16.0, 16.0, 16.0]);

        let trigger = server.vm.spawn();
        server.vm.ent_set_float(trigger, "solid", SOLID_TRIGGER as f32);
        server.vm.ent_set_int(trigger, "touch", touch_fn as i32);
        server.vm.ent_set_vector(trigger, "absmin", [500.0, 500.0, 500.0]);
        server.vm.ent_set_vector(trigger, "absmax", [532.0, 532.0, 532.0]);

        touch_triggers(&mut server.vm, mover, 0.0);
        assert_eq!(server.vm.gget_float("touched_flag"), 0.0, "far trigger did not fire");
    }

    /// A BSP whose world model is a single empty leaf, so a world box-trace runs
    /// clear (fraction 1) instead of the empty-BSP "everything solid". Hull 0's
    /// headnode (0) names node 0, whose children are the empty leaf -> CONTENTS
    /// EMPTY. This lets the entity-clip tests see entity collisions instead of a
    /// world block at fraction 0.
    fn world_open_bsp() -> Bsp {
        use crate::bsp::{DClipNode, DLeaf, DModel, DNode, DPlane, CONTENTS_EMPTY, CONTENTS_SOLID};
        let mut b = empty_bsp();
        // One axial plane at x = -100000 (far away), so every test point is on
        // its front side -> child 0 -> the empty leaf.
        b.planes = vec![DPlane {
            normal: [1.0, 0.0, 0.0],
            dist: -100000.0,
            ptype: 0,
        }];
        // node 0: both children name leaf 1 (index -(- ( -2)) ...). Children are
        // i16: a negative child -(leaf)-1. Leaf 1 -> child = -(1)-1 = -2.
        b.nodes = vec![DNode {
            planenum: 0,
            children: [-2, -2], // both sides -> leaf 1 (CONTENTS_EMPTY)
            mins: [0; 3],
            maxs: [0; 3],
            firstface: 0,
            numfaces: 0,
        }];
        // leaf 0 is the solid leaf; leaf 1 is empty open space.
        b.leafs = vec![
            DLeaf {
                contents: CONTENTS_SOLID,
                visofs: -1,
                mins: [0; 3],
                maxs: [0; 3],
                firstmarksurface: 0,
                nummarksurfaces: 0,
                ambient_level: [0; 4],
            },
            DLeaf {
                contents: CONTENTS_EMPTY,
                visofs: -1,
                mins: [0; 3],
                maxs: [0; 3],
                firstmarksurface: 0,
                nummarksurfaces: 0,
                ambient_level: [0; 4],
            },
        ];
        // Clip hulls 1/2: a single clipnode that is empty on both sides.
        b.clipnodes = vec![DClipNode {
            planenum: 0,
            children: [CONTENTS_EMPTY as i16, CONTENTS_EMPTY as i16],
        }];
        b.models = vec![DModel {
            mins: [-4096.0; 3],
            maxs: [4096.0; 3],
            origin: [0.0; 3],
            headnode: [0, 0, 0, 0],
            visleafs: 1,
            firstface: 0,
            numfaces: 0,
        }];
        b
    }

    // -------------------------------------------------------------- the player

    /// A BSP with a flat floor at `z = 0`: the half-space `z >= 0` is open
    /// (`CONTENTS_EMPTY`) and `z < 0` is solid (`CONTENTS_SOLID`), in every hull.
    /// A player box dropped onto it lands on `z = 0` and cannot tunnel through.
    /// The split plane is the axial +Z plane at `dist = 0` (`ptype = 2`).
    fn floor_bsp() -> Bsp {
        use crate::bsp::{DClipNode, DLeaf, DModel, DNode, DPlane, CONTENTS_EMPTY, CONTENTS_SOLID};
        let mut b = empty_bsp();
        // plane 0: +Z at z = 0 (the point hull, hull 0). plane 1: +Z at z = 24,
        // which models how the BSP compiler bakes the player box (mins.z = -24)
        // into hull 1 — so a *point* traced against hull 1 stops with the box
        // bottom resting on the real floor at z = 0 (origin.z = 24).
        b.planes = vec![
            DPlane { normal: [0.0, 0.0, 1.0], dist: 0.0, ptype: 2 },
            DPlane { normal: [0.0, 0.0, 1.0], dist: 24.0, ptype: 2 },
        ];
        // Hull-0 node: front side (z >= 0, child 0) -> empty leaf 1;
        // back side (z < 0, child 1) -> solid leaf 0. Negative child -(leaf)-1:
        // leaf 1 -> -2 (empty), leaf 0 -> -1 (solid).
        b.nodes = vec![DNode {
            planenum: 0,
            children: [-2, -1],
            mins: [0; 3],
            maxs: [0; 3],
            firstface: 0,
            numfaces: 0,
        }];
        b.leafs = vec![
            DLeaf {
                contents: CONTENTS_SOLID,
                visofs: -1,
                mins: [0; 3],
                maxs: [0; 3],
                firstmarksurface: 0,
                nummarksurfaces: 0,
                ambient_level: [0; 4],
            },
            DLeaf {
                contents: CONTENTS_EMPTY,
                visofs: -1,
                mins: [0; 3],
                maxs: [0; 3],
                firstmarksurface: 0,
                nummarksurfaces: 0,
                ambient_level: [0; 4],
            },
        ];
        // Clip hulls 1/2 split on plane 1 (z = 24): above empty, below solid —
        // the player-expanded floor.
        b.clipnodes = vec![DClipNode {
            planenum: 1,
            children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16],
        }];
        b.models = vec![DModel {
            mins: [-4096.0; 3],
            maxs: [4096.0; 3],
            origin: [0.0; 3],
            headnode: [0, 0, 0, 0],
            visleafs: 1,
            firstface: 0,
            numfaces: 0,
        }];
        b
    }

    /// Build a progs for the player-physics tests. It declares every field the
    /// client movement code reads/writes and the engine globals it sets, plus a
    /// `PutClientInServer` function that sets `health = 100` and `origin =
    /// (0, 0, 40)` (above the floor) by storing two prepared global constants.
    /// `SetNewParms` / `ClientConnect` / `PlayerPreThink` / `PlayerPostThink` /
    /// `StartFrame` are empty (just `DONE`) so the connect/frame paths run. The
    /// system functions are resolved by NAME (no need to wire the like-named
    /// globals — `connect_client`'s `sys_function` falls back to `find_function`).
    ///
    /// Returns `(image, g_const100_ofs, g_origin_vec_ofs)` so the test can place
    /// the `100.0` float and the `(0,0,40)` vector the spawn function stores.
    fn player_progs() -> (Vec<u8>, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 48;

        // Engine globals the server sets/reads.
        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);
        b.add_global("v_forward", EV_VECTOR, 60);
        b.add_global("v_right", EV_VECTOR, 63);
        b.add_global("v_up", EV_VECTOR, 66);

        // Constants the spawn function stores: 100.0 (health) and (0,0,40)
        // (origin). Placed in free global cells; the test fills them after load.
        let g_const100 = 40u16;
        let g_origin = 44u16; // 44,45,46

        // Fields the client physics touches.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2); // 2,3,4
        b.add_field("velocity", EV_VECTOR, 5); // 5,6,7
        b.add_field("mins", EV_VECTOR, 8); // 8,9,10
        b.add_field("maxs", EV_VECTOR, 11); // 11,12,13
        b.add_field("absmin", EV_VECTOR, 14); // 14,15,16
        b.add_field("absmax", EV_VECTOR, 17); // 17,18,19
        b.add_field("angles", EV_VECTOR, 20); // 20,21,22
        b.add_field("v_angle", EV_VECTOR, 23); // 23,24,25
        b.add_field("punchangle", EV_VECTOR, 26); // 26,27,28
        b.add_field("size", EV_VECTOR, 29); // 29,30,31
        b.add_field("flags", EV_FLOAT, 32);
        b.add_field("health", EV_FLOAT, 33);
        b.add_field("movetype", EV_FLOAT, 34);
        b.add_field("solid", EV_FLOAT, 35);
        b.add_field("fixangle", EV_FLOAT, 36);
        b.add_field("teleport_time", EV_FLOAT, 37);
        b.add_field("groundentity", EV_ENTITY, 38);
        b.add_field("view_ofs", EV_VECTOR, 39); // 39,40,41
        b.add_field("model", EV_STRING, 42);
        b.add_field("modelindex", EV_FLOAT, 43);
        b.add_field("think", EV_FUNCTION, 44);
        b.add_field("nextthink", EV_FLOAT, 45);
        b.add_field("touch", EV_FUNCTION, 46);
        b.add_field("gravity", EV_FLOAT, 47);

        // Field offsets for the spawn function's stores (health=33, origin=2..4).
        let f_health = 33u16;
        let f_origin = 2u16;

        // Empty system functions (DONE only).
        let done = || Statement {
            op: Op::Done as u16,
            a: 0,
            b: 0,
            c: 0,
        };
        b.add_function("SetNewParms", vec![done()]);
        b.add_function("ClientConnect", vec![done()]);
        b.add_function("StartFrame", vec![done()]);
        b.add_function("PlayerPreThink", vec![done()]);
        b.add_function("PlayerPostThink", vec![done()]);

        // PutClientInServer: STOREP_F const100 -> self.health;
        //                    STOREP_V origin_const -> self.origin; DONE.
        // We compute the field pointer with ADDRESS(self, field) -> a temp global,
        // then STOREP into it. Use temp globals 48 (ptr) and the self entity in
        // global 31. Field-number globals: we need a global holding the field ofs.
        // Simpler: ADDRESS takes (entity, field) where both are globals; place the
        // field numbers in globals 50 (health) and 51 (origin).
        let g_fhealth = 50u16;
        let g_forigin = 51u16;
        let g_ptr = 52u16;
        let put = b.add_function(
            "PutClientInServer",
            vec![
                // ptr = ADDRESS(self, f_health)
                Statement {
                    op: Op::Address as u16,
                    a: 31, // self entity global
                    b: g_fhealth as i16,
                    c: g_ptr as i16,
                },
                // *ptr = const100
                Statement {
                    op: Op::StorepF as u16,
                    a: g_const100 as i16,
                    b: g_ptr as i16,
                    c: 0,
                },
                // ptr = ADDRESS(self, f_origin)
                Statement {
                    op: Op::Address as u16,
                    a: 31,
                    b: g_forigin as i16,
                    c: g_ptr as i16,
                },
                // *ptr = origin_const (vector)
                Statement {
                    op: Op::StorepV as u16,
                    a: g_origin as i16,
                    b: g_ptr as i16,
                    c: 0,
                },
                done(),
            ],
        );
        let _ = put;
        // The ADDRESS field-number globals (g_fhealth=50, g_forigin=51) and the
        // value constants are filled by `prime_player_globals` after the test
        // builds its Server, keeping these offsets in one documented place.
        let _ = (g_fhealth, g_forigin, g_ptr, f_health, f_origin);

        let img = b.build();
        (img, g_const100 as usize, g_origin as usize)
    }

    /// Set up the field-number constants a freshly-loaded player progs needs for
    /// its `PutClientInServer` ADDRESS ops, plus the value constants. Mirrors the
    /// offsets chosen in [`player_progs`].
    fn prime_player_globals(server: &mut Server, g_const100: usize, g_origin: usize) {
        // Field numbers for ADDRESS (health field ofs 33, origin field ofs 2).
        server.vm.set_gi(50, 33);
        server.vm.set_gi(51, 2);
        // Value constants: health 100, origin (0,0,40).
        server.vm.set_gf(g_const100, 100.0);
        server.vm.set_gv(g_origin, [0.0, 0.0, 40.0]);
    }

    #[test]
    fn user_friction_reduces_player_speed() {
        // A player gliding on the ground with no input must lose horizontal
        // speed each tick (SV_UserFriction), trending toward zero.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        // Stand the player on the floor with a player-sized box and give it a
        // forward velocity, on the ground.
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        server.vm.ent_set_vector(p, "velocity", [200.0, 0.0, 0.0]);
        let flags = server.vm.ent_get_float(p, "flags") as i32;
        server
            .vm
            .ent_set_float(p, "flags", (flags | FL_ONGROUND) as f32);

        // No movement input -> friction only.
        let cmd = UserCmd::default();
        let speed0 = {
            let v = server.vm.ent_get_vector(p, "velocity");
            (v[0] * v[0] + v[1] * v[1]).sqrt()
        };
        server.client_frame(&cmd, 0.1).expect("frame1");
        let speed1 = {
            let v = server.vm.ent_get_vector(p, "velocity");
            (v[0] * v[0] + v[1] * v[1]).sqrt()
        };
        // Re-plant on the ground (the move may clear ONGROUND) and tick again.
        let flags = server.vm.ent_get_float(p, "flags") as i32;
        server
            .vm
            .ent_set_float(p, "flags", (flags | FL_ONGROUND) as f32);
        server.client_frame(&cmd, 0.1).expect("frame2");
        let speed2 = {
            let v = server.vm.ent_get_vector(p, "velocity");
            (v[0] * v[0] + v[1] * v[1]).sqrt()
        };

        assert!(
            speed1 < speed0,
            "friction reduced speed: {speed0} -> {speed1}"
        );
        assert!(
            speed2 < speed1,
            "friction kept reducing speed: {speed1} -> {speed2}"
        );
    }

    // SV_WaterMove: waist-deep the player swims instead of walking. Idle, you
    // drift down ~60 u/s; pressing the swim-down key (upmove < 0) descends
    // faster. (Tests water_move directly — the synthetic test progs don't define
    // a `waterlevel` field for the client_think dispatch, but the swim math is
    // what matters; the real progs.dat has waterlevel and exercises the branch.)
    #[test]
    fn water_move_sinks_when_idle_and_descends_faster_with_movedown() {
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);
        let p = server.connect_client().expect("connect");

        // Idle, at rest: SV_WaterMove drifts down (wishvel.z -= 60).
        server.vm.ent_set_vector(p, "v_angle", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
        server.water_move(p, &UserCmd::default(), 0.1);
        let idle_z = server.vm.ent_get_vector(p, "velocity")[2];
        assert!(idle_z < 0.0, "idle swimmer drifts down: vel.z = {idle_z}");

        // Pressing swim-down (the `c` key -> upmove < 0) sinks faster.
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
        let down = UserCmd { upmove: -320.0, ..UserCmd::default() };
        server.water_move(p, &down, 0.1);
        let down_z = server.vm.ent_get_vector(p, "velocity")[2];
        assert!(down_z < idle_z, "swim-down descends faster: {down_z} < {idle_z}");
    }

    // SV_WaterMove builds its wish from the FULL view angles (v_angle), so you
    // swim along your look pitch — unlike the air move (which uses the 1/3-pitch
    // `angles`). Looking up (pitch < 0) and swimming forward must rise. This is
    // the regression guard for the v_angle-vs-angles faithfulness point.
    #[test]
    fn water_move_swims_up_when_looking_up_and_pressing_forward() {
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);
        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
        // v_angle pitch = -45 (look up in Quake's convention): forward.z =
        // -sin(-45) > 0, so swimming forward rises.
        server.vm.ent_set_vector(p, "v_angle", [-45.0, 0.0, 0.0]);

        let fwd = UserCmd { forwardmove: 320.0, ..UserCmd::default() };
        server.water_move(p, &fwd, 0.1);
        let z = server.vm.ent_get_vector(p, "velocity")[2];
        assert!(z > 0.0, "swimming forward while looking up rises: vel.z = {z}");
    }

    #[test]
    fn accelerate_moves_toward_wishdir_clamped_at_maxspeed() {
        // A stationary on-ground player given a sustained forward command must
        // build up forward velocity, capped at sv_maxspeed (320).
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        server.vm.ent_set_vector(p, "velocity", [0.0; 3]);

        // Look straight along +X (yaw 0) and push full forward.
        let cmd = UserCmd {
            forwardmove: 800.0, // exceeds maxspeed so the clamp is exercised
            yaw: 0.0,
            ..UserCmd::default()
        };

        // First tick: velocity gains a +X component (accelerate toward wishdir).
        let flags = server.vm.ent_get_float(p, "flags") as i32;
        server
            .vm
            .ent_set_float(p, "flags", (flags | FL_ONGROUND) as f32);
        server.client_frame(&cmd, 0.1).expect("frame");
        let v1 = server.vm.ent_get_vector(p, "velocity");
        assert!(
            v1[0] > 0.0,
            "velocity moved toward +X wishdir, got {v1:?}"
        );

        // Many ticks: horizontal speed never exceeds sv_maxspeed.
        for _ in 0..40 {
            let flags = server.vm.ent_get_float(p, "flags") as i32;
            server
                .vm
                .ent_set_float(p, "flags", (flags | FL_ONGROUND) as f32);
            server.client_frame(&cmd, 0.1).expect("frame");
            let v = server.vm.ent_get_vector(p, "velocity");
            let hspeed = (v[0] * v[0] + v[1] * v[1]).sqrt();
            assert!(
                hspeed <= SV_MAXSPEED + 1.0,
                "horizontal speed clamped at maxspeed, got {hspeed}"
            );
        }
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
    fn v_calc_roll_leans_into_a_strafe() {
        // FIX-7 unit: V_CalcRoll signs with the strafe direction and ramps with
        // sideways speed up to cl_rollangle (2.0) at cl_rollspeed (200).
        // Facing +X (yaw 0): right vector is -Y, so a +Y velocity gives a negative
        // dot (lean one way), a -Y velocity the opposite sign.
        let facing = [0.0, 0.0, 0.0];
        let slow = v_calc_roll(facing, [0.0, 100.0, 0.0]); // half rollspeed
        let fast = v_calc_roll(facing, [0.0, 400.0, 0.0]); // past rollspeed -> clamp
        assert!(slow != 0.0, "a sideways velocity produces a non-zero roll");
        // 100 u/s is half of rollspeed -> magnitude = 2.0 * 100/200 = 1.0.
        assert!((slow.abs() - 1.0).abs() < 1e-4, "ramped roll magnitude, got {slow}");
        // Clamped at cl_rollangle = 2.0 beyond rollspeed.
        assert!((fast.abs() - 2.0).abs() < 1e-4, "clamped roll magnitude, got {fast}");
        // Opposite strafe -> opposite sign.
        let other = v_calc_roll(facing, [0.0, -100.0, 0.0]);
        assert!(slow * other < 0.0, "strafe direction flips the roll sign");
        // No sideways component -> no roll.
        assert_eq!(v_calc_roll(facing, [200.0, 0.0, 0.0]), 0.0, "pure forward = no lean");
    }

    #[test]
    fn client_think_applies_punchangle_and_roll_to_body_angles() {
        // FIX-7: SV_ClientThink adds punchangle to the view (so the body pitch =
        // -(v_angle+punch).pitch/3) and sets angles[ROLL] = V_CalcRoll*4. The old
        // port wrote angles = [-pitch/3, yaw, 0] with no punch and no roll.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_float(p, "health", 100.0);
        // Pin it on the ground so it strafes (the WALK ground path).
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        for _ in 0..5 {
            server.client_frame(&UserCmd::default(), 0.1).expect("settle");
        }

        // Strafe right (sidemove > 0) while facing yaw 0; look pitch 0. Roll is
        // computed from the velocity at the TOP of SV_ClientThink (before this
        // frame's acceleration), so build up sideways speed over a few frames
        // first — exactly id's one-frame-lagged lean.
        let cmd = UserCmd {
            sidemove: 320.0,
            yaw: 0.0,
            pitch: 0.0,
            ..UserCmd::default()
        };
        for _ in 0..4 {
            server.client_frame(&cmd, 0.1).expect("build strafe speed");
        }

        // Set a fresh weapon kick (punchangle pitch = -6) just before the final
        // frame so the decay maths is predictable (one frame of 10*dt decay).
        server.vm.ent_set_vector(p, "punchangle", [-6.0, 0.0, 0.0]);
        server.client_frame(&cmd, 0.1).expect("strafe frame");

        let angles = server.vm.ent_get_vector(p, "angles");
        // ROLL is non-zero: the body leans into the strafe (V_CalcRoll * 4).
        assert!(
            angles[crate::math::ROLL].abs() > 0.01,
            "strafing player banks: angles[ROLL] = {}",
            angles[crate::math::ROLL]
        );
        // PITCH reflects the punch. SV_ClientThink runs DropPunchAngle FIRST
        // (decays the kick by 10*dt = 1.0 unit of length, so -6 -> -5), THEN adds
        // it: angles[PITCH] = -(v_angle.pitch + decayed_punch)/3 = -(0 + -5)/3.
        assert!(
            (angles[crate::math::PITCH] - 5.0 / 3.0).abs() < 1e-3,
            "decayed punchangle feeds the body pitch: expected ~1.667, got {}",
            angles[crate::math::PITCH]
        );
        // Without the punch the body pitch would be 0 (look pitch is 0), so a
        // non-zero pitch proves the punch was applied.
        assert!(
            angles[crate::math::PITCH] > 0.5,
            "punch must move the body pitch off zero, got {}",
            angles[crate::math::PITCH]
        );
    }

    // ----------------------------------------------------- attack / sound wiring

    /// Field/global offsets the attack progs uses (kept in one place so the test
    /// can fill the constants after load).
    mod attack_ofs {
        // Globals.
        pub const SELF: u16 = 31;
        pub const G_FIRED: u16 = 40; // float flag PostThink sets when attacking
        pub const G_BTN: u16 = 41; // temp: loaded self.button0
        pub const G_FBUTTON0: u16 = 42; // holds the button0 field offset (for LOAD)
        pub const G_ONE: u16 = 43; // const 1.0
        pub const G_SNDFUNC: u16 = 44; // const: function index of the sound builtin
        pub const G_CHAN: u16 = 45; // const channel
        pub const G_VOL: u16 = 46; // const volume
        pub const G_ATTEN: u16 = 47; // const attenuation
        pub const G_SAMPLE: u16 = 48; // const string_t of the sample name
        // Field offsets.
        pub const F_BUTTON0: u16 = 48; // button0 field cell
    }

    /// Build a progs whose `PlayerPostThink` reads `self.button0` and, when it is
    /// set, both sets a global flag (`g_fired = 1`) and fires `sound(self, CHAN,
    /// SAMPLE, VOL, ATTEN)` through the engine `PF_sound` builtin (#8). When
    /// `button0` is clear it does nothing. Returns `(image, sound_fn_index)`; the
    /// caller fills the constant globals via [`prime_attack_globals`].
    fn attack_progs() -> (Vec<u8>, usize) {
        use attack_ofs::*;
        let mut b = Builder::new();
        b.entityfields = 56;

        b.add_global("self", EV_ENTITY, SELF);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);
        b.add_global("v_forward", EV_VECTOR, 60);
        b.add_global("v_right", EV_VECTOR, 63);
        b.add_global("v_up", EV_VECTOR, 66);
        // A named global for the flag so the test can read it by name.
        b.add_global("fired_flag", EV_FLOAT, G_FIRED);

        // Fields the client physics touches (mirrors player_progs' broad set so
        // the movement path never faults), plus button0/weapon/ammo_shells.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2); // 2,3,4
        b.add_field("velocity", EV_VECTOR, 5); // 5,6,7
        b.add_field("mins", EV_VECTOR, 8); // 8,9,10
        b.add_field("maxs", EV_VECTOR, 11); // 11,12,13
        b.add_field("absmin", EV_VECTOR, 14); // 14,15,16
        b.add_field("absmax", EV_VECTOR, 17); // 17,18,19
        b.add_field("angles", EV_VECTOR, 20); // 20,21,22
        b.add_field("v_angle", EV_VECTOR, 23); // 23,24,25
        b.add_field("punchangle", EV_VECTOR, 26); // 26,27,28
        b.add_field("size", EV_VECTOR, 29); // 29,30,31
        b.add_field("flags", EV_FLOAT, 32);
        b.add_field("health", EV_FLOAT, 33);
        b.add_field("movetype", EV_FLOAT, 34);
        b.add_field("solid", EV_FLOAT, 35);
        b.add_field("fixangle", EV_FLOAT, 36);
        b.add_field("teleport_time", EV_FLOAT, 37);
        b.add_field("groundentity", EV_ENTITY, 38);
        b.add_field("view_ofs", EV_VECTOR, 39); // 39,40,41
        b.add_field("think", EV_FUNCTION, 44);
        b.add_field("nextthink", EV_FLOAT, 45);
        b.add_field("touch", EV_FUNCTION, 46);
        b.add_field("gravity", EV_FLOAT, 47);
        b.add_field("button0", EV_FLOAT, F_BUTTON0);
        b.add_field("button2", EV_FLOAT, 49);
        b.add_field("impulse", EV_FLOAT, 50);
        b.add_field("weapon", EV_FLOAT, 51);
        b.add_field("ammo_shells", EV_FLOAT, 52);
        b.add_field("effects", EV_FLOAT, 53);

        // The sound builtin (PF_sound, #8) as a callable QuakeC function.
        let sound_fn = b.add_builtin("sound", 8);

        // Empty connect/frame system functions.
        let done = || Statement {
            op: Op::Done as u16,
            a: 0,
            b: 0,
            c: 0,
        };
        b.add_function("SetNewParms", vec![done()]);
        b.add_function("ClientConnect", vec![done()]);
        b.add_function("PutClientInServer", vec![done()]);
        b.add_function("StartFrame", vec![done()]);
        b.add_function("PlayerPreThink", vec![done()]);

        // PlayerPostThink: read self.button0; if set, fire the sound + set flag.
        // Statement layout (relative indices used for the IFNOT branch offset):
        //   0 LoadF  self.button0 -> G_BTN
        //   1 IFNOT  G_BTN -> (skip to DONE at rel index 9)  => offset 8
        //   2 StoreF G_ONE -> fired_flag
        //   3 StoreEnt self -> PARM0
        //   4 StoreF G_CHAN -> PARM1
        //   5 StoreS G_SAMPLE -> PARM2
        //   6 StoreF G_VOL -> PARM3
        //   7 StoreF G_ATTEN -> PARM4
        //   8 CALL5  G_SNDFUNC
        //   9 DONE
        let parm0 = OFS_PARM0 as i16; // 4
        let parm1 = (OFS_PARM0 + 3) as i16; // 7
        let parm2 = (OFS_PARM0 + 6) as i16; // 10
        let parm3 = (OFS_PARM0 + 9) as i16; // 13
        let parm4 = (OFS_PARM0 + 12) as i16; // 16
        b.add_function(
            "PlayerPostThink",
            vec![
                Statement {
                    op: Op::LoadF as u16,
                    a: SELF as i16,
                    b: G_FBUTTON0 as i16,
                    c: G_BTN as i16,
                },
                Statement {
                    op: Op::Ifnot as u16,
                    a: G_BTN as i16,
                    b: 8,
                    c: 0,
                },
                Statement {
                    op: Op::StoreF as u16,
                    a: G_ONE as i16,
                    b: G_FIRED as i16,
                    c: 0,
                },
                Statement {
                    op: Op::StoreEnt as u16,
                    a: SELF as i16,
                    b: parm0,
                    c: 0,
                },
                Statement {
                    op: Op::StoreF as u16,
                    a: G_CHAN as i16,
                    b: parm1,
                    c: 0,
                },
                Statement {
                    op: Op::StoreS as u16,
                    a: G_SAMPLE as i16,
                    b: parm2,
                    c: 0,
                },
                Statement {
                    op: Op::StoreF as u16,
                    a: G_VOL as i16,
                    b: parm3,
                    c: 0,
                },
                Statement {
                    op: Op::StoreF as u16,
                    a: G_ATTEN as i16,
                    b: parm4,
                    c: 0,
                },
                Statement {
                    op: Op::Call5 as u16,
                    a: G_SNDFUNC as i16,
                    b: 0,
                    c: 0,
                },
                done(),
            ],
        );

        (b.build(), sound_fn)
    }

    /// Fill the constant globals the attack progs reads (after the Server is
    /// built so the sample string is interned into the live VM heap).
    fn prime_attack_globals(server: &mut Server, sound_fn: usize, sample: &str) -> i32 {
        use attack_ofs::*;
        server.vm.set_gi(G_FBUTTON0 as usize, F_BUTTON0 as i32);
        server.vm.set_gf(G_ONE as usize, 1.0);
        server.vm.set_gi(G_SNDFUNC as usize, sound_fn as i32);
        server.vm.set_gf(G_CHAN as usize, 1.0); // CHAN_WEAPON
        server.vm.set_gf(G_VOL as usize, 1.0);
        server.vm.set_gf(G_ATTEN as usize, 1.0); // ATTN_NORM
        let s_t = server.vm.intern(sample);
        server.vm.set_gi(G_SAMPLE as usize, s_t);
        s_t
    }

    #[test]
    fn attack_button_drives_quakec_and_fires_sound() {
        // Pressing attack (buttons bit 0) must make PlayerPostThink see
        // self.button0 != 0 and run its firing code (set the flag + emit a
        // sound); releasing it must not.
        let sample = "weapons/guncock.wav";
        let (img, sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let s_t = prime_attack_globals(&mut server, sound_fn, sample);

        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        server.vm.ent_set_float(p, "health", 100.0);

        // --- Frame 1: attack released (buttons = 0) ---
        let release = UserCmd {
            buttons: 0,
            ..UserCmd::default()
        };
        server.client_frame(&release, 0.1).expect("frame");
        assert_eq!(
            server.vm.gget_float("fired_flag"),
            0.0,
            "no attack -> PostThink did not fire"
        );
        let (b0, _, _) = server.player_attack_state();
        assert_eq!(b0, 0.0, "button0 cleared on the edict when not pressed");
        assert!(
            server.drain_sounds().is_empty(),
            "no sound queued when not attacking"
        );

        // --- Frame 2: attack pressed (buttons = 1) ---
        let attack = UserCmd {
            buttons: 1,
            ..UserCmd::default()
        };
        server.client_frame(&attack, 0.1).expect("frame");
        assert_eq!(
            server.vm.gget_float("fired_flag"),
            1.0,
            "attack -> button0 reached QuakeC PostThink and fired"
        );
        let (b0, _, _) = server.player_attack_state();
        assert_eq!(b0, 1.0, "button0 set on the edict while attack held");

        let sounds = server.drain_sounds();
        assert_eq!(sounds.len(), 1, "exactly one sound fired");
        let ev = &sounds[0];
        assert_eq!(ev.entity, p, "sound emitted by the player edict");
        assert_eq!(ev.channel, 1, "CHAN_WEAPON");
        assert_eq!(ev.sample, sample);
        assert_eq!(ev.volume, 1.0);
        assert_eq!(ev.attenuation, 1.0);
        // origin = player origin + 0.5*(mins+maxs) = (0,0,24)+0.5*((-16,-16,-24)+(16,16,32))
        //        = (0,0,24)+(0,0,4) = (0,0,28).
        assert_eq!(ev.origin, [0.0, 0.0, 28.0], "box-centre emission point");
        assert!(ev.sound_index >= 1, "sample resolved to a precache slot");
        // drain cleared the queue.
        assert!(server.drain_sounds().is_empty(), "drain cleared the queue");

        let _ = s_t; // (interned handle; asserted indirectly via ev.sample)
    }

    #[test]
    fn bi_sound_queues_event_and_drain_clears() {
        // bi_sound (PF_sound) must push a SoundEvent with the faithful fields and
        // drain_sounds must return then clear it. Drive the builtin directly by
        // placing its args in the PARM globals and calling it.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // Give an entity a box so the centre offset is non-trivial.
        let e = server.vm.spawn();
        server.vm.ent_set_vector(e, "origin", [10.0, 20.0, 30.0]);
        server.vm.ent_set_vector(e, "mins", [-2.0, -4.0, -6.0]);
        server.vm.ent_set_vector(e, "maxs", [2.0, 4.0, 16.0]);

        // Precache the sample so it resolves to a real slot, then set up PARMs.
        let sample = "ambience/wind2.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        // PARM0=entity, PARM1=channel(2), PARM2=sample, PARM3=vol(0.5), PARM4=atten(2)
        server.vm.set_gi(OFS_PARM0, e);
        server.vm.set_gf(OFS_PARM0 + 3, 2.0);
        server.vm.set_gi(OFS_PARM0 + 6, s_t);
        server.vm.set_gf(OFS_PARM0 + 9, 0.5);
        server.vm.set_gf(OFS_PARM0 + 12, 2.0);

        bi_sound(&mut server.vm).expect("bi_sound");

        let sounds = server.drain_sounds();
        assert_eq!(sounds.len(), 1);
        let ev = &sounds[0];
        assert_eq!(ev.entity, e);
        assert_eq!(ev.channel, 2);
        assert_eq!(ev.sample, sample);
        assert_eq!(ev.volume, 0.5);
        assert_eq!(ev.attenuation, 2.0);
        // centre = (10,20,30) + 0.5*((-2,-4,-6)+(2,4,16)) = (10,20,30)+(0,0,5) = (10,20,35)
        assert_eq!(ev.origin, [10.0, 20.0, 35.0]);
        assert!(ev.sound_index >= 1, "precached sample resolved");

        // The queue is empty after draining.
        assert!(
            server.drain_sounds().is_empty(),
            "drain_sounds cleared the queue"
        );
    }

    #[test]
    fn bi_ambientsound_records_static_sound_and_drain_clears() {
        // bi_ambientsound (PF_ambientsound, #74) must record a StaticSound (a
        // persistent loop, NOT a one-shot SoundEvent) carrying the placed
        // position and the byte-quantized volume/attenuation the wire format
        // (`svc_spawnstaticsound`) carried, and drain_static_sounds must return
        // then clear it. Drive the builtin directly via the PARM globals.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds(); // clear any startup registrations

        // Precache the sample so it resolves to a real slot, then set up PARMs:
        // PARM0=pos(vec), PARM1=sample, PARM2=vol(0.5), PARM3=atten(3=ATTN_STATIC)
        // — FireAmbient's exact call for the e1m1 torches.
        let sample = "ambience/fire1.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        server.vm.set_gv(OFS_PARM0, [100.0, -50.0, 24.0]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 0.5);
        server.vm.set_gf(OFS_PARM0 + 9, 3.0);

        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        let statics = server.drain_static_sounds();
        assert_eq!(statics.len(), 1, "one static sound recorded");
        let s = &statics[0];
        assert_eq!(s.origin, [100.0, -50.0, 24.0], "placed at the literal pos");
        assert_eq!(s.sample, sample);
        // vol 0.5 -> byte trunc(127.5)=127 -> 127/255 (the wire round-trip).
        assert_eq!(s.volume, 127.0 / 255.0);
        // atten 3 -> byte 192 -> 192/64 = 3.0 exactly (ATTN_STATIC survives).
        assert_eq!(s.attenuation, 3.0);
        assert!(s.sound_index >= 1, "precached sample resolved");

        // It is a loop registration, not a one-shot: the SoundEvent queue is
        // untouched, and the static registry is empty after draining.
        assert!(
            server.drain_sounds().is_empty(),
            "no one-shot SoundEvent queued by ambientsound"
        );
        assert!(
            server.drain_static_sounds().is_empty(),
            "drain_static_sounds cleared the registry"
        );
    }

    #[test]
    fn bi_ambientsound_clamps_out_of_range_bytes() {
        // The C MSG_WriteByte would wrap out-of-range values; we clamp
        // defensively (QuakeC only ever passes sane 0..1 / 0..4 values).
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds();

        let sample = "ambience/wind2.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 9.0); // vol byte clamps to 255
        server.vm.set_gf(OFS_PARM0 + 9, 9.0); // atten byte clamps to 255
        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        let statics = server.drain_static_sounds();
        assert_eq!(statics[0].volume, 1.0, "volume byte clamps to 255");
        assert_eq!(
            statics[0].attenuation,
            255.0 / 64.0,
            "attenuation byte clamps to 255"
        );
    }

    #[test]
    fn bi_ambientsound_drops_unprecached_sample_like_the_c() {
        // PF_ambientsound scans sv.sound_precache READ-ONLY: an un-precached
        // sample is refused with `Con_Printf ("no precache: %s\n", samp)` and
        // nothing is registered — and the check must not grow the precache
        // table either (unlike the one-shot path's lookup_sound_index).
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds();

        let s_t = server.vm.intern("ambience/notthere.wav");
        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 0.5);
        server.vm.set_gf(OFS_PARM0 + 9, 3.0);
        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        assert!(
            server.drain_static_sounds().is_empty(),
            "un-precached ambientsound registers nothing"
        );
        assert!(
            server.vm.output.contains("no precache: ambience/notthere.wav\n"),
            "the C's console message, routed to vm.output: {:?}",
            server.vm.output
        );
        assert_eq!(
            server.vm.with_host(|_vm, h| h.find_sound("ambience/notthere.wav")),
            Some(None),
            "the read-only check must not register the name as a side effect"
        );
    }

    #[test]
    fn bi_particle_queues_burst_and_drain_clears() {
        // bi_particle (PF_particle, #48) must push a ParticleBurst carrying its
        // (org, dir, color, count) arguments verbatim, and drain_particles must
        // return then clear it. Drive the builtin directly by placing its args in
        // the PARM globals, mirroring bi_sound_queues_event_and_drain_clears.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // particle(org, dir, color, count): PARM0=org(vec), PARM1=dir(vec),
        // PARM2=color(float), PARM3=count(float).
        server.vm.set_gv(OFS_PARM0, [10.0, 20.0, 30.0]);
        server.vm.set_gv(OFS_PARM0 + 3, [0.0, 0.0, 1.0]);
        server.vm.set_gf(OFS_PARM0 + 6, 73.0); // base palette index
        server.vm.set_gf(OFS_PARM0 + 9, 12.0); // count

        bi_particle(&mut server.vm).expect("bi_particle");

        let bursts = server.drain_particles();
        assert_eq!(bursts.len(), 1, "one burst queued");
        let b = &bursts[0];
        assert_eq!(b.org, [10.0, 20.0, 30.0]);
        assert_eq!(b.dir, [0.0, 0.0, 1.0]);
        assert_eq!(b.color, 73);
        assert_eq!(b.count, 12);

        // The queue is empty after draining.
        assert!(
            server.drain_particles().is_empty(),
            "drain_particles cleared the queue"
        );
    }

    #[test]
    fn bi_particle_clamps_out_of_range_color_to_byte() {
        // A float color outside 0..=255 must clamp into the palette-index byte
        // range rather than wrapping unexpectedly when cast.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gv(OFS_PARM0 + 3, [0.0; 3]);
        server.vm.set_gf(OFS_PARM0 + 6, 99999.0); // absurd color -> clamps to 255
        server.vm.set_gf(OFS_PARM0 + 9, 1.0);
        bi_particle(&mut server.vm).expect("bi_particle");
        let b = server.drain_particles();
        assert_eq!(b[0].color, 255, "out-of-range color clamps to 255");

        server.vm.set_gf(OFS_PARM0 + 6, -10.0); // negative -> clamps to 0
        bi_particle(&mut server.vm).expect("bi_particle");
        let b = server.drain_particles();
        assert_eq!(b[0].color, 0, "negative color clamps to 0");
    }

    // ------------------------------------------------ temp-entity decoder

    /// Drive a `WriteByte(dest, value)` builtin: dest in PARM0, value in PARM1.
    fn write_byte(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writebyte(&mut server.vm).expect("bi_writebyte");
    }
    /// Drive a `WriteCoord(dest, value)` builtin.
    fn write_coord(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writecoord(&mut server.vm).expect("bi_writecoord");
    }
    /// Drive a `WriteShort(dest, value)` builtin.
    fn write_short(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writeshort(&mut server.vm).expect("bi_writeshort");
    }
    /// A fresh server plus a cleared decoder/queue (the thread-locals persist
    /// across tests on the same thread, so reset before each scenario).
    fn te_server() -> Server {
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let server = Server::new(floor_bsp(), progs).expect("server");
        reset_temp_entity_decoder();
        let _ = take_temp_entities(); // clear any residue from a prior test
        server
    }

    #[test]
    fn te_explosion_burst_yields_one_event_with_pos() {
        // WriteByte(0,23) WriteByte(0,3) WriteCoord(0,x/y/z) -> one TE_EXPLOSION.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32); // svc_temp_entity
        write_byte(&mut server, 0, TE_EXPLOSION as f32); // type 3
        write_coord(&mut server, 0, 16.0);
        write_coord(&mut server, 0, -32.0);
        write_coord(&mut server, 0, 48.5);

        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "exactly one temp entity emitted");
        assert_eq!(evs[0].te_type, TE_EXPLOSION);
        assert_eq!(evs[0].pos, [16.0, -32.0, 48.5]);
        assert_eq!(evs[0].color_start, 0);
        assert_eq!(evs[0].color_length, 0);
        // drain cleared the queue (mirrors drain_sounds).
        assert!(
            server.drain_temp_entities().is_empty(),
            "drain_temp_entities cleared the queue"
        );
    }

    /// Drive a `WriteString(dest, text)` builtin (interning the text first, as
    /// the progs loader would have).
    fn write_string(server: &mut Server, dest: i32, text: &str) {
        let ofs = server.vm.intern(text);
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gi(OFS_PARM0 + 3, ofs);
        bi_writestring(&mut server.vm).expect("bi_writestring");
    }
    /// A fresh server plus a cleared MSG_ALL recognizer/queue (thread-locals
    /// persist across tests on one thread, so reset before each scenario).
    fn svc_server() -> Server {
        let server = te_server();
        reset_svc_recognizer();
        let _ = take_svc_events();
        server
    }

    #[test]
    fn svc_intermission_byte_on_msg_all_yields_event() {
        // execute_changelevel (client.qc): WriteByte(MSG_ALL, SVC_INTERMISSION).
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
        assert!(server.drain_svc_events().is_empty(), "drain cleared the queue");
    }

    #[test]
    fn svc_finale_byte_plus_string_yields_finale_text() {
        // ExitIntermission (client.qc): WriteByte(MSG_ALL, SVC_FINALE) then
        // WriteString(MSG_ALL, <episode text>).
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_FINALE as f32);
        assert!(server.drain_svc_events().is_empty(), "no event until the string lands");
        write_string(&mut server, MSG_ALL, "the Rune of Earth Magic");
        assert_eq!(
            server.drain_svc_events(),
            vec![SvcEvent::Finale("the Rune of Earth Magic".into())]
        );
    }

    #[test]
    fn svc_cdtrack_payload_bytes_do_not_desync_the_stream() {
        // ExitIntermission writes cdtrack THEN the finale: WriteByte(MSG_ALL, 32),
        // WriteByte(MSG_ALL, 2), WriteByte(MSG_ALL, 3) — the two payload bytes must
        // be consumed, not read as commands — then the intermission/finale follows.
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_CDTRACK as f32);
        write_byte(&mut server, MSG_ALL, 2.0);
        write_byte(&mut server, MSG_ALL, 3.0);
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
    }

    #[test]
    fn svc_stat_ticks_and_other_destinations_yield_no_events() {
        let mut server = svc_server();
        // killed_monsters/found_secrets arrive as bare MSG_ALL bytes; the engine
        // reads the counts from the QuakeC globals, so no event surfaces.
        write_byte(&mut server, MSG_ALL, SVC_KILLEDMONSTER as f32);
        write_byte(&mut server, MSG_ALL, SVC_FOUNDSECRET as f32);
        // A broadcast (MSG_BROADCAST=0) temp-entity burst must not feed the
        // MSG_ALL recognizer even though 30 is svc_intermission.
        write_byte(&mut server, 0, SVC_INTERMISSION as f32);
        // MSG_ONE / MSG_INIT are likewise ignored.
        write_byte(&mut server, 1, SVC_INTERMISSION as f32);
        write_byte(&mut server, 3, SVC_INTERMISSION as f32);
        assert!(server.drain_svc_events().is_empty());
    }

    #[test]
    fn svc_sellscreen_and_cutscene_recognised() {
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_SELLSCREEN as f32);
        write_byte(&mut server, MSG_ALL, SVC_CUTSCENE as f32);
        write_string(&mut server, MSG_ALL, "cut");
        assert_eq!(
            server.drain_svc_events(),
            vec![SvcEvent::SellScreen, SvcEvent::Cutscene("cut".into())]
        );
    }

    #[test]
    fn svc_recognizer_resets_on_unexpected_write_and_new_server() {
        let mut server = svc_server();
        // A non-byte/string MSG_ALL write mid-command means desync: drop it.
        write_byte(&mut server, MSG_ALL, SVC_FINALE as f32);
        write_short(&mut server, MSG_ALL, 7.0);
        write_string(&mut server, MSG_ALL, "late text");
        assert!(
            server.drain_svc_events().is_empty(),
            "desynced finale dropped, stray string ignored"
        );
        // A queued event from the OLD level must not leak across a new server
        // (with_pak clears state + queue, mirroring reset_changelevel).
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        let fresh = svc_server();
        drop(fresh);
        assert!(
            take_svc_events().is_empty(),
            "a fresh server cleared the queued events"
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
    fn te_gunshot_burst_carries_its_type() {
        // A TE_GUNSHOT (type 2) sequence yields te_type == 2.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_GUNSHOT as f32);
        write_coord(&mut server, 0, 1.0);
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_GUNSHOT);
        assert_eq!(evs[0].pos, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn te_explosion2_consumes_three_coords_then_two_bytes() {
        // EXPLOSION2 (type 12): 3 coords + colorStart + colorLength byte.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_EXPLOSION2 as f32);
        write_coord(&mut server, 0, 10.0);
        write_coord(&mut server, 0, 20.0);
        write_coord(&mut server, 0, 30.0);
        write_byte(&mut server, 0, 105.0); // colorStart
        write_byte(&mut server, 0, 8.0); // colorLength
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_EXPLOSION2);
        assert_eq!(evs[0].pos, [10.0, 20.0, 30.0]);
        assert_eq!(evs[0].color_start, 105);
        assert_eq!(evs[0].color_length, 8);
    }

    #[test]
    fn te_beam_consumes_short_and_six_coords() {
        // A beam (TE_BEAM=13): short entity index + 6 coords (start+end), all
        // captured for CL_ParseBeam (crate::tent): entity = slot key, pos =
        // start point, end = end point.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_BEAM as f32);
        write_short(&mut server, 0, 7.0); // entity index
        write_coord(&mut server, 0, 1.0); // start
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        write_coord(&mut server, 0, 4.0); // end
        write_coord(&mut server, 0, 5.0);
        write_coord(&mut server, 0, 6.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "beam emits exactly one event");
        assert_eq!(evs[0].te_type, TE_BEAM);
        assert_eq!(evs[0].entity, 7, "beam entity = the WriteShort slot key");
        assert_eq!(evs[0].pos, [1.0, 2.0, 3.0], "beam pos = start point");
        assert_eq!(evs[0].end, [4.0, 5.0, 6.0], "beam end = end point");
    }

    #[test]
    fn te_lightning_write_entity_captures_the_edict_number() {
        // The REAL beam writers (W_FireLightning etc.) pass the owner through
        // WriteEntity, whose parm is an entity reference — an INT global
        // (G_EDICTNUM), not a float. Reading it as a float yields the f32
        // bit-reinterpretation of the index (~0.0 for every edict), which would
        // collapse all beams onto one slot and break the view-entity tracking.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_LIGHTNING2 as f32);
        // WriteEntity(MSG_BROADCAST, self): an int edict number in PARM1.
        server.vm.set_gf(OFS_PARM0, 0.0);
        server.vm.set_gi(OFS_PARM0 + 3, 1); // the player edict
        bi_writeentity(&mut server.vm).expect("bi_writeentity");
        for v in [10.0, 20.0, 30.0, 40.0, 50.0, 60.0] {
            write_coord(&mut server, 0, v);
        }
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_LIGHTNING2);
        assert_eq!(
            evs[0].entity, 1,
            "WriteEntity's int edict number survives the decode"
        );
        assert_eq!(evs[0].pos, [10.0, 20.0, 30.0]);
        assert_eq!(evs[0].end, [40.0, 50.0, 60.0]);
    }

    #[test]
    fn te_non_broadcast_dest_produces_no_event() {
        // Writes on MSG_ONE (dest 1) are ignored: no temp entity is decoded.
        let mut server = te_server();
        write_byte(&mut server, 1, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 1, TE_EXPLOSION as f32);
        write_coord(&mut server, 1, 16.0);
        write_coord(&mut server, 1, 32.0);
        write_coord(&mut server, 1, 48.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "MSG_ONE writes produce no broadcast temp entity"
        );
    }

    #[test]
    fn te_unknown_type_resets_and_next_message_still_parses() {
        // An unknown type byte resets the decoder cleanly (no event), and a
        // following valid message must still parse.
        let mut server = te_server();
        // Unknown type 200 -> reset, drop.
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, 200.0); // not a known TE_*
        // These stray coords land in Idle and are ignored.
        write_coord(&mut server, 0, 9.0);
        write_coord(&mut server, 0, 9.0);
        write_coord(&mut server, 0, 9.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "unknown type emits nothing"
        );

        // A clean, valid message right after still decodes.
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_SPIKE as f32);
        write_coord(&mut server, 0, 7.0);
        write_coord(&mut server, 0, 8.0);
        write_coord(&mut server, 0, 9.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "the next valid message parses after a reset");
        assert_eq!(evs[0].te_type, TE_SPIKE);
        assert_eq!(evs[0].pos, [7.0, 8.0, 9.0]);
    }

    #[test]
    fn te_decoder_reset_drops_partial_message() {
        // A half-collected message (svc + type + one coord) is dropped by a
        // frame reset; a fresh message after the reset parses cleanly.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_EXPLOSION as f32);
        write_coord(&mut server, 0, 1.0); // only one of three coords
        reset_temp_entity_decoder(); // frame boundary
        // Continuing the old coords now must NOT complete a stale message.
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "reset dropped the partial temp entity"
        );
    }

    #[test]
    fn impulse_is_set_only_by_a_nonzero_cmd_and_kept_for_the_progs() {
        // SV_ReadClientMove: `if (i) host_client->edict->v.impulse = i;` — the
        // engine only ever SETS the impulse; the QuakeC's ImpulseCommands clears
        // it once it runs (this synthetic progs has none, so it stays).
        let (img, sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_attack_globals(&mut server, sound_fn, "weapons/guncock.wav");

        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        server.vm.ent_set_float(p, "health", 100.0);

        // Frame with impulse 7 (e.g. a weapon-switch command).
        let cmd = UserCmd {
            impulse: 7,
            ..UserCmd::default()
        };
        server.client_frame(&cmd, 0.1).expect("frame");
        assert_eq!(server.vm.ent_get_float(p, "impulse"), 7.0, "the engine does not clear it");

        // A frame with no impulse leaves it alone (a 0 byte means "none").
        let none = UserCmd::default();
        server.client_frame(&none, 0.1).expect("frame");
        assert_eq!(server.vm.ent_get_float(p, "impulse"), 7.0, "a 0 impulse does not overwrite");

        // A new non-zero impulse replaces it.
        let three = UserCmd { impulse: 3, ..UserCmd::default() };
        server.client_frame(&three, 0.1).expect("frame");
        assert_eq!(server.vm.ent_get_float(p, "impulse"), 3.0);
    }

    /// Prove the impulse is actually *present on the edict* mid-frame, by having
    /// PlayerPreThink copy `self.impulse` into a flag.
    #[test]
    fn impulse_visible_to_prethink() {
        use attack_ofs::*;
        let mut b = Builder::new();
        b.entityfields = 56;
        b.add_global("self", EV_ENTITY, SELF);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);
        b.add_global("v_forward", EV_VECTOR, 60);
        b.add_global("v_right", EV_VECTOR, 63);
        b.add_global("v_up", EV_VECTOR, 66);
        b.add_global("seen_impulse", EV_FLOAT, 40); // PreThink copies impulse here

        // Minimal field set for the client physics, plus impulse.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2);
        b.add_field("velocity", EV_VECTOR, 5);
        b.add_field("mins", EV_VECTOR, 8);
        b.add_field("maxs", EV_VECTOR, 11);
        b.add_field("absmin", EV_VECTOR, 14);
        b.add_field("absmax", EV_VECTOR, 17);
        b.add_field("angles", EV_VECTOR, 20);
        b.add_field("v_angle", EV_VECTOR, 23);
        b.add_field("punchangle", EV_VECTOR, 26);
        b.add_field("size", EV_VECTOR, 29);
        b.add_field("flags", EV_FLOAT, 32);
        b.add_field("health", EV_FLOAT, 33);
        b.add_field("movetype", EV_FLOAT, 34);
        b.add_field("solid", EV_FLOAT, 35);
        b.add_field("fixangle", EV_FLOAT, 36);
        b.add_field("teleport_time", EV_FLOAT, 37);
        b.add_field("groundentity", EV_ENTITY, 38);
        b.add_field("view_ofs", EV_VECTOR, 39);
        b.add_field("think", EV_FUNCTION, 44);
        b.add_field("nextthink", EV_FLOAT, 45);
        b.add_field("touch", EV_FUNCTION, 46);
        b.add_field("gravity", EV_FLOAT, 47);
        b.add_field("button0", EV_FLOAT, 48);
        b.add_field("impulse", EV_FLOAT, 50);

        let g_seen = 40u16;
        let g_fimpulse = 41u16; // holds the impulse field offset for LOAD
        let g_tmp = 42u16;

        let done = || Statement {
            op: Op::Done as u16,
            a: 0,
            b: 0,
            c: 0,
        };
        b.add_function("SetNewParms", vec![done()]);
        b.add_function("ClientConnect", vec![done()]);
        b.add_function("PutClientInServer", vec![done()]);
        b.add_function("StartFrame", vec![done()]);
        // PreThink: seen_impulse = self.impulse.
        b.add_function(
            "PlayerPreThink",
            vec![
                Statement {
                    op: Op::LoadF as u16,
                    a: SELF as i16,
                    b: g_fimpulse as i16,
                    c: g_tmp as i16,
                },
                Statement {
                    op: Op::StoreF as u16,
                    a: g_tmp as i16,
                    b: g_seen as i16,
                    c: 0,
                },
                done(),
            ],
        );
        b.add_function("PlayerPostThink", vec![done()]);

        let img = b.build();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        server.vm.set_gi(g_fimpulse as usize, 50); // impulse field ofs

        let p = server.connect_client().expect("connect");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
        server.vm.ent_set_float(p, "health", 100.0);

        let cmd = UserCmd {
            impulse: 3,
            ..UserCmd::default()
        };
        server.client_frame(&cmd, 0.1).expect("frame");

        // PreThink saw the impulse the engine wrote on the edict this frame...
        assert_eq!(
            server.vm.gget_float("seen_impulse"),
            3.0,
            "impulse was on the edict before PreThink ran"
        );
        // ...and the engine left it there (only the QuakeC clears it).
        assert_eq!(server.vm.ent_get_float(p, "impulse"), 3.0, "impulse kept after the frame");
    }

    // -------------------------------------------------------- level transitions

    /// Build a minimal progs for the changelevel parm-marshaling tests. It
    /// declares the engine globals plus `parm1..parm16`, a `classname`/`origin`
    /// field set, and three system functions:
    ///   * `SetChangeParms`: copies a test-filled constant into `parm1` (the
    ///     QuakeC `SetChangeParms` marshals the player's state into the parm
    ///     globals; here we just write a recognisable value so the test can prove
    ///     `save_spawn_parms` ran it and read it back).
    ///   * `ClientConnect`: empty (DONE).
    ///   * `PutClientInServer`: copies `parm1` back into a global `decoded` so a
    ///     test can prove the restored parm was visible to the spawn script
    ///     (mirrors `DecodeLevelParms` reading parm1 into a player field).
    ///
    /// Returns `(image, g_const_ofs, g_decoded_ofs)` so the test can place the
    /// value `SetChangeParms` stores and read what `PutClientInServer` decoded.
    fn changelevel_progs() -> (Vec<u8>, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 8;

        // Engine globals + the 16 spawn parms. Globals 31..36 are the well-known
        // self/other/time/world/frametime/viewentity (matching player_progs).
        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);
        // parm1..parm16 at globals 70..85.
        for i in 0..NUM_SPAWN_PARMS {
            b.add_global(&parm_global_name(i), EV_FLOAT, 70 + i as u16);
        }
        // A constant SetChangeParms stores into parm1, and a global
        // PutClientInServer decodes parm1 into. Filled by the test after load.
        let g_const = 40u16;
        let g_decoded = 41u16;

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
        // SetChangeParms: parm1 = g_const.
        b.add_function(
            "SetChangeParms",
            vec![
                Statement {
                    op: Op::StoreF as u16,
                    a: g_const as i16,
                    b: 70, // parm1 global ofs
                    c: 0,
                },
                done(),
            ],
        );
        b.add_function("ClientConnect", vec![done()]);
        // PutClientInServer: g_decoded = parm1 (DecodeLevelParms stand-in).
        b.add_function(
            "PutClientInServer",
            vec![
                Statement {
                    op: Op::StoreF as u16,
                    a: 70, // parm1 global ofs
                    b: g_decoded as i16,
                    c: 0,
                },
                done(),
            ],
        );

        let img = b.build();
        (img, g_const as usize, g_decoded as usize)
    }

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

    // ------------------------------------------------ animated light styles (#35)

    /// Drive `bi_lightstyle(style, val)` directly: PARM0 = style float, PARM1 =
    /// the interned pattern string. Returns nothing; the write lands in the
    /// thread-local transport (`snapshot_lightstyles` / a frame sync reads it).
    fn call_lightstyle(server: &mut Server, style: f32, val: &str) {
        let s = server.vm.intern(val);
        server.vm.set_gf(OFS_PARM0, style);
        // PARM1 is a string_t (an int handle), at OFS_PARM0 + 3.
        server.vm.set_gi(OFS_PARM0 + 3, s);
        bi_lightstyle(&mut server.vm).expect("bi_lightstyle");
    }

    #[test]
    fn bi_lightstyle_stores_pattern_and_getter_reflects_it() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Fresh server: all styles empty.
        assert_eq!(server.lightstyle(0), "");
        assert_eq!(server.lightstyle(3), "");

        // Store a steady style 0 and a torch flicker at slot 3.
        call_lightstyle(&mut server, 0.0, "m");
        call_lightstyle(&mut server, 3.0, "mmnmmommommnonmmonqnmmo");
        // A frame syncs the transport into the owned table (the production path).
        server.run_frame(0.1).expect("frame");

        assert_eq!(server.lightstyle(0), "m");
        assert_eq!(server.lightstyle(3), "mmnmmommommnonmmonqnmmo");
    }

    #[test]
    fn lightstyle_scales_maps_letters_to_brightness() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Letter value is (c-'a')*22, normalised by id's 256 white point (NOT 'm'):
        // 'a' -> 0 (dark), 'm' -> 264/256 = 1.03125 (id's steady-world brightness),
        // 'z' -> 550/256 ~ 2.148, and an UNSET style -> 1.0 (R_AnimateLight's 256
        // default, so untouched faces stay neutral).
        call_lightstyle(&mut server, 0.0, "a");
        call_lightstyle(&mut server, 1.0, "m");
        call_lightstyle(&mut server, 2.0, "z");
        // style 4 left empty.
        server.run_frame(0.1).expect("frame");

        let sc = server.lightstyle_scales(0.0);
        assert!((sc[0] - 0.0).abs() < 1e-6, "'a' -> 0.0, got {}", sc[0]);
        // 'm' = (12*22)/256 = 264/256 = 1.03125 (id renders the steady world here).
        assert!((sc[1] - (264.0 / 256.0)).abs() < 1e-6, "'m' -> 1.03125, got {}", sc[1]);
        // 'z' = (25*22)/256 = 550/256 ~ 2.1484.
        assert!((sc[2] - (550.0 / 256.0)).abs() < 1e-5, "'z' -> ~2.148, got {}", sc[2]);
        assert!((sc[4] - 1.0).abs() < 1e-6, "unset style -> 1.0 (normal), got {}", sc[4]);
    }

    #[test]
    fn lightstyle_scales_animate_at_ten_per_second_with_modulo() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // A two-char flicker: 'a' (dark) then 'z' (bright). At 10 chars/sec it
        // toggles every 0.1s.
        call_lightstyle(&mut server, 1.0, "az");
        server.run_frame(0.1).expect("frame");

        let s_t0 = server.lightstyle_scales(0.00); // k = floor(0)=0 -> 'a' -> 0.0
        let s_t1 = server.lightstyle_scales(0.10); // k = floor(1)=1 -> 'z' -> ~2.08
        let s_t2 = server.lightstyle_scales(0.20); // k = floor(2)=0 (mod 2) -> 'a'
        assert!((s_t0[1] - 0.0).abs() < 1e-6, "t=0 -> 'a' 0.0, got {}", s_t0[1]);
        assert!(s_t1[1] > 2.0, "t=0.1 -> 'z' ~2.08, got {}", s_t1[1]);
        // Cycling: the index wraps modulo the string length, so t=0.2 == t=0.0.
        assert!((s_t2[1] - s_t0[1]).abs() < 1e-6, "modulo cycle: t=0.2 == t=0.0");
        // Across time the same style yields DIFFERENT scales (animation).
        assert!((s_t0[1] - s_t1[1]).abs() > 1e-3, "style must animate over time");
    }

    #[test]
    fn bi_lightstyle_out_of_range_index_is_ignored_without_panic() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Index 64 (== MAX_LIGHTSTYLES) is out of range -> dropped, no panic.
        call_lightstyle(&mut server, MAX_LIGHTSTYLES as f32, "z");
        // A wild / negative / non-finite index is also clamped out, no panic.
        call_lightstyle(&mut server, -5.0, "z");
        call_lightstyle(&mut server, 1.0e30, "z");
        call_lightstyle(&mut server, f32::NAN, "z");
        server.run_frame(0.1).expect("frame");

        // Nothing was stored; every scale is the unset normal 1.0.
        let sc = server.lightstyle_scales(0.0);
        assert!(sc.iter().all(|&s| (s - 1.0).abs() < 1e-6), "no style stored");
        // The valid last in-range index (63) still works as a sanity anchor.
        call_lightstyle(&mut server, 63.0, "a");
        server.run_frame(0.1).expect("frame");
        assert!((server.lightstyle_scales(0.0)[63] - 0.0).abs() < 1e-6, "index 63 valid");
    }

    #[test]
    fn fresh_server_clears_stale_lightstyles() {
        // A pattern left in the transport must not leak into a freshly built
        // server (Server::new calls reset_lightstyles).
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut s1 = Server::new(empty_bsp(), progs).expect("server");
        call_lightstyle(&mut s1, 1.0, "z"); // leaves "z" in the transport
        // A new server resets the transport; its first frame syncs an empty table.
        let progs2 = Progs::parse(&img).expect("parse");
        let mut s2 = Server::new(empty_bsp(), progs2).expect("server");
        s2.run_frame(0.1).expect("frame");
        assert_eq!(s2.lightstyle(1), "", "stale style must not leak into a new server");
        assert!((s2.lightstyle_scales(0.0)[1] - 1.0).abs() < 1e-6, "new server style 1 normal");
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
