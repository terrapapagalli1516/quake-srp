//! The Quake server: a QuakeC VM with the engine builtins installed over a
//! loaded map — entity spawning, physics, collision, monster movement, the
//! local player client, and the message side a front-end drains each frame.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! This file is the `server.h` part: the [`Server`] itself, the
//! [`WorldModel`] (the map as the [`Host`] the builtins reach it through), the
//! spawn/frame reports, [`UserCmd`], the shared `server.h` constants, and the
//! re-exports that keep every public `server::…` path flat. The rest is split
//! along id's own files, each `impl Server` block in the module of the C
//! function it ports:
//!
//! | module       | id's C                         | what it holds                              |
//! |--------------|--------------------------------|--------------------------------------------|
//! | `sv_main`    | `sv_main.c`, `cl_main.c`       | `SV_SpawnServer`, `SV_ConnectClient`,      |
//! |              |                                | `SV_CleanupEnts`, the `EF_*` dlights       |
//! | `pr_edict`   | `pr_edict.c`, `common.c`       | `ED_LoadFromFile` + parsers, `COM_Parse`   |
//! | `pr_cmds`    | `pr_cmds.c`                    | world-touching builtins, the table install |
//! | `sv_phys`    | `sv_phys.c`                    | `SV_Physics`, movetypes, pushers, the      |
//! |              |                                | player's `SV_WalkMove` / `SV_FlyMove`      |
//! | `sv_user`    | `sv_user.c`, `view.c`          | `SV_ClientThink`: friction, acceleration,  |
//! |              |                                | swimming, ideal pitch; `V_CalcRoll`        |
//! | `sv_world`   | `world.c`, `sv_phys.c`         | `SV_Move`, `SV_LinkEdict` bounds,          |
//! |              |                                | `SV_TouchLinks`, `SV_Impact`               |
//! | `sv_move`    | `sv_move.c`                    | monster stepping and chasing               |
//! | `msg`        | `sv_main.c`, `pr_cmds.c`,      | sound / particle / print queues, `Write*`  |
//! |              | `cl_tent.c`, `cl_parse.c`      | → temp entities and svc events             |
//! | `lightstyle` | `pr_cmds.c`, `r_light.c`       | `PF_lightstyle`, `R_AnimateLight`          |
//! | `host`       | `host_cmd.c`, `sv_main.c`      | skill, deferred changelevel / restart,     |
//! |              |                                | spawn parms, `kill`, signon frames         |
//!
//! Below the server sit the crate-level layers it builds on: `vm.rs`
//! (pr_exec.c and the edict runtime), `builtins.rs` (pr_cmds.c's
//! self-contained builtins), `world.rs` (the BSP hull traces) — and `save.rs`
//! (`Host_Savegame_f` / `Host_Loadgame_f`) above it.
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

use std::collections::HashMap;

use crate::bsp::Bsp;
use crate::math::Vec3;
use crate::stepping::Stepping;
use crate::vm::{Host, HostTrace, Vm};
use crate::{QError, Result};

mod host;
mod lightstyle;
mod msg;
mod pr_cmds;
mod pr_edict;
mod sv_main;
mod sv_move;
mod sv_phys;
mod sv_user;
mod sv_world;

pub use lightstyle::{lightstyle_scales_at, MAX_LIGHTSTYLES};
pub use msg::{
    te_consts, GameMessage, Outbox, ParticleBurst, SoundEvent, StaticSound, SvcEvent, TempEntityEvent,
};
pub use pr_cmds::install_engine_builtins;
pub use sv_main::{EntityDlight, EF_BRIGHTLIGHT, EF_DIMLIGHT, EF_MUZZLEFLASH};
pub use sv_move::{
    sv_check_bottom, sv_move_to_goal, sv_movestep, sv_new_chase_dir, sv_step_direction,
};
pub use sv_user::v_calc_roll;
pub use sv_world::{probe_point_contents, sv_impact, sv_move, touch_triggers, MoveTrace};


pub use host::ServerCvars;
pub(crate) use pr_edict::{ed_new_string, parse_float, parse_int, parse_vector, Tokenizer};
pub(crate) use sv_world::link_edict;

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
const SETTLE_FRAMETIME: f64 = 0.1;

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
// The world model: crate::vm::Host backed by a parsed BSP.
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
    /// Optional pak, used to resolve the bounds of the model files (alias
    /// models, sprites and the external brush models, the `maps/b_*.bsp` item
    /// boxes). `None` (e.g. in unit tests) falls back to a zero box.
    pak: Option<crate::pak::Pak>,
    /// `mod->mins`/`maxs` of every model file precached, keyed by precache
    /// name, as `Mod_LoadModel` set them (see [`model_file_bounds`]): e.g.
    /// `"progs/player.mdl"` -> ±16, `"progs/s_explod.spr"` (56x56) -> ±28,
    /// `"maps/b_explob.bsp"` -> `(0,0,0)..(32,32,64)`. Filled by
    /// [`precache_model`] so `setmodel` finds them.
    model_bounds: std::collections::HashMap<String, (Vec3, Vec3)>,
    /// What this server's QuakeC sent out, until the server drains it (see
    /// [`Outbox`]). It lives here, on the [`Host`], because that is what a
    /// builtin can reach ([`Vm::with_host`]).
    outbox: Outbox,
    /// The `skill` and `sv_gravity` cvars (see [`ServerCvars`]).
    cvars: ServerCvars,
}

/// `mod->mins`/`maxs` of the model file `name` in `pak`, as `Mod_LoadModel`
/// (model.c) sets them, by the file's magic: an alias model (`IDPO`) gets
/// `Mod_LoadAliasModel`'s fixed ±16 ("FIXME: do this right"), a sprite
/// (`IDSP`) `Mod_LoadSpriteModel`'s `±maxwidth/2` across and `±maxheight/2`
/// up (C ints: `-psprite->maxwidth/2` truncates toward zero), anything else
/// is a brush model: submodel 0's bounds, as spread ONCE by
/// `Mod_LoadSubmodels` (mins-1, maxs+1) — which `Bsp::parse` already did:
/// b_explob.bsp's raw (1,1,1)..(31,31,63) is (0,0,0)..(32,32,64) here
/// (spreading again made the boxes 34 wide: hull2 traces, droptofloor
/// failures). `None` for a missing or unreadable file (id's `Sys_Error`) and
/// for bounds `SetMinMaxSize` would reject as backwards.
fn model_file_bounds(pak: &crate::pak::Pak, name: &str) -> Option<(Vec3, Vec3)> {
    let bytes = pak.read_file(name).ok().flatten()?;
    match bytes.get(..4)? {
        b"IDPO" => Some(([-16.0; 3], [16.0; 3])),
        b"IDSP" => {
            let h = crate::spr::Sprite::parse(&bytes).ok()?.header;
            if h.width < 0 || h.height < 0 {
                return None;
            }
            let (mw, mh) = (-h.width / 2, -h.height / 2);
            let (xw, xh) = (h.width / 2, h.height / 2);
            Some(([mw as f32, mw as f32, mh as f32], [xw as f32, xw as f32, xh as f32]))
        }
        _ => crate::bsp::Bsp::parse(&bytes).ok()?.models.first().map(|m| (m.mins, m.maxs)),
    }
}

impl WorldModel {
    /// Build a world model for `bsp`, reserving precache slot 0 for the empty
    /// string and precaching the world model name `"*0"` at index 1 (the C
    /// `SV_SpawnServer` precached `sv.worldmodel` as model 1).
    pub fn new(bsp: Bsp) -> WorldModel {
        WorldModel::with_pak(bsp, None)
    }

    /// Like [`new`], but with a pak so the model files' bounds (alias models,
    /// sprites, the `b_*.bsp` boxes) can be resolved for `setmodel`. The
    /// interactive engines (wasm, quaketool) pass `Some(pak)`; tests pass
    /// `None` and keep the zero-box fallback.
    pub fn with_pak(bsp: Bsp, pak: Option<crate::pak::Pak>) -> WorldModel {
        let mut w = WorldModel {
            bsp,
            precache_models: vec![String::new()],
            precache_sounds: vec![String::new()],
            pak,
            model_bounds: std::collections::HashMap::new(),
            outbox: Outbox::default(),
            cvars: ServerCvars::default(),
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
        // `PF_precache_model` loads every model it precaches (`sv.models[i] =
        // Mod_ForName (s, true)`), so `PF_setmodel` later finds
        // `mod->mins/maxs` for alias models, sprites and the external brush
        // models (the `maps/b_*.bsp` item boxes, which without it were visible
        // but unshootable). Resolve them once from the pak; any failure (no
        // pak, a missing file, a parse error) leaves them unset -> a zero box.
        if Self::submodel_index(name).is_none() && !self.model_bounds.contains_key(name) {
            if let Some(b) = self.pak.as_ref().and_then(|pak| model_file_bounds(pak, name)) {
                self.model_bounds.insert(name.to_string(), b);
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
        // Every other model file gets the bounds `Mod_LoadModel` gave it,
        // resolved at precache time: ±16 for an alias model, ±maxwidth/2 and
        // ±maxheight/2 for a sprite, submodel 0's for a b_*.bsp box
        // (`PF_setmodel` -> `SetMinMaxSize (e, mod->mins, mod->maxs, true)`).
        // What QuakeC `setsize`s afterwards overrides it; what it does not
        // (an explosion's `s_explod.spr`, the flames) keeps it.
        self.model_bounds.get(name).copied()
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

    fn outbox(&mut self) -> &mut Outbox {
        &mut self.outbox
    }

    fn cvars(&self) -> &ServerCvars {
        &self.cvars
    }

    fn cvars_mut(&mut self) -> &mut ServerCvars {
        &mut self.cvars
    }
}

// ---------------------------------------------------------------------------
// The Server.
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
    /// the server. The `lightstyle()` builtin sends its writes to the outbox;
    /// the server applies them to this field after each QuakeC execution window
    /// (`spawn_entities` / `run_frame`). `lightstyle_scales` reads it to produce
    /// the per-style brightness scales the renderer applies each frame. Empty in
    /// a new server, so a changelevel re-populates it from the new worldspawn.
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
    /// `svs.serverflags` (server.h): the rune bits kept across levels.
    /// `SV_SpawnServer` writes it into the QC global before the entities load;
    /// only `SV_SaveSpawnparms` (a changelevel) reads the live global back. A
    /// `restart` therefore respawns with this LEVEL-ENTRY value: die on e1m7
    /// after taking the rune and id's game loses it. Set by
    /// [`Server::set_serverflags`].
    svs_serverflags: f32,
    /// `sv.paused` (server.h): the `pause` command stopped the world
    /// ([`Server::pause`]). While set, `Host_ServerFrame` runs neither
    /// `SV_ClientThink` nor `SV_Physics`, so `sv.time` stands still; the
    /// client follows it (`svc_setpause` sets `cl.paused` in the same host
    /// frame on a local server). `SV_SpawnServer` clears it.
    pub paused: bool,
    /// How the frame running steps its integrators ([`Stepping`]): set by
    /// [`Server::client_frame_stepped`] for the frame, as `host_frametime`
    /// is; Classic (id's per-frame code) otherwise.
    pub(crate) stepping: Stepping,
    /// Each pusher's `ltime` to double precision, which the uncapped step
    /// keeps it by (`sv_phys`'s `advance_ltime`).
    pub(crate) ltime_exact: HashMap<i32, f64>,
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

// The accessors and QuakeC entry-point helpers every part of the server uses;
// the rest of `impl Server` is spread over the submodules (see the table in
// the module header).
//
// The local player is a real edict the QuakeC game logic owns (health / items
// / weapons). The engine connects it (`sv_main`: SV_ConnectClient), turns its
// usercmd into velocity (`sv_user`: SV_ClientThink) and moves it with
// friction, acceleration and ENTITY-AWARE collision (`sv_phys`:
// SV_Physics_Client / SV_WalkMove over [`sv_move`]), so it collides with
// monsters, doors and items.
impl Server {
    /// `sv.time` as a float: what `pr_global_struct->time = sv.time` stores for
    /// QuakeC and `svc_time`'s `MSG_WriteFloat(sv.time)` sends the client (so a
    /// local client's `cl.time`). The clock itself is [`Server::sv_time`].
    pub fn time(&self) -> f32 {
        self.vm.sv_time as f32
    }

    /// `sv.time`, the server clock (a `double` in id's `server_t`).
    pub fn sv_time(&self) -> f64 {
        self.vm.sv_time
    }

    /// Set `sv.time` (and, as the C's next `pr_global_struct->time = sv.time`
    /// would, the QuakeC `time` global to its float).
    pub fn set_sv_time(&mut self, t: f64) {
        self.vm.sv_time = t;
        self.vm.gset_float("time", t as f32);
    }

    /// The number of live (not-free) edicts, including the world (edict 0).
    pub fn live_entities(&self) -> usize {
        self.vm
            .edict_free
            .iter()
            .filter(|&&free| !free)
            .count()
    }

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
        wm3.model_bounds
            .insert("maps/b_explob.bsp".into(), ([0.0, 0.0, 0.0], [32.0, 32.0, 64.0]));
        assert_eq!(
            wm3.model_bbox("maps/b_explob.bsp"),
            Some(([0.0, 0.0, 0.0], [32.0, 32.0, 64.0]))
        );
    }

    /// CENSUS L10: `PF_setmodel` -> `SetMinMaxSize (e, mod->mins, mod->maxs,
    /// true)` with the bounds `Mod_LoadModel` gave the file: ±16 for any alias
    /// model (`Mod_LoadAliasModel`), `±maxwidth/2` across and `±maxheight/2`
    /// up for a sprite (`Mod_LoadSpriteModel`, C ints), not a zero box.
    #[test]
    fn alias_and_sprite_models_get_mod_load_model_bounds() {
        let mut spr = b"IDSP".to_vec();
        for v in [1i32, 0] {
            spr.extend_from_slice(&v.to_le_bytes()); // version, type
        }
        spr.extend_from_slice(&0f32.to_le_bytes()); // boundingradius
        for v in [33i32, 21, 1] {
            spr.extend_from_slice(&v.to_le_bytes()); // width, height, numframes
        }
        spr.extend_from_slice(&0f32.to_le_bytes()); // beamlength
        for v in [0i32, 0, -16, 10, 33, 21] {
            spr.extend_from_slice(&v.to_le_bytes()); // synctype; SPR_SINGLE, origin, size
        }
        spr.resize(spr.len() + 33 * 21, 0);
        let mut mdl = b"IDPO".to_vec();
        mdl.extend_from_slice(&6i32.to_le_bytes());
        let files: [(&str, &[u8]); 2] = [("progs/s.spr", &spr), ("progs/m.mdl", &mdl)];
        // PACK: header, the files, then the 64-byte directory entries.
        let mut img = b"PACK".to_vec();
        let body: usize = files.iter().map(|f| f.1.len()).sum();
        img.extend_from_slice(&(12 + body as i32).to_le_bytes());
        img.extend_from_slice(&(64 * files.len() as i32).to_le_bytes());
        let mut dir = Vec::new();
        for (name, bytes) in files {
            let mut n = [0u8; 56];
            n[..name.len()].copy_from_slice(name.as_bytes());
            dir.extend_from_slice(&n);
            dir.extend_from_slice(&(img.len() as i32).to_le_bytes());
            dir.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
            img.extend_from_slice(bytes);
        }
        img.extend_from_slice(&dir);
        let pak = crate::pak::Pak::from_bytes("t".into(), img).expect("pak");
        let mut wm = WorldModel::with_pak(empty_bsp(), Some(pak));
        for name in ["progs/s.spr", "progs/m.mdl", "progs/missing.mdl"] {
            wm.precache_model(name);
        }
        // -33/2 and 33/2 truncate toward zero: 16; 21/2: 10.
        assert_eq!(wm.model_bbox("progs/s.spr"), Some(([-16.0, -16.0, -10.0], [16.0, 16.0, 10.0])));
        assert_eq!(wm.model_bbox("progs/m.mdl"), Some(([-16.0; 3], [16.0; 3])));
        assert_eq!(wm.model_bbox("progs/missing.mdl"), None, "id would Sys_Error; a zero box");
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
}
