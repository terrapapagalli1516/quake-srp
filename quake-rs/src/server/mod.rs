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
//!   through the handles resolved when the progs loaded ([`Vm::fo`], [`Vm::go`];
//!   `movetype`/`solid`/`flags` as [`MoveType`]/[`Solid`]/[`EntFlags`]) and the
//!   bounds-checked [`Vm`] helpers; missing definitions are no-ops rather than
//!   crashes, and a bad entity index simply does nothing.
//! * The tokenizer ([`Tokenizer`]) is a faithful transcription of `COM_Parse`
//!   working over `&str` byte positions, so a malformed entity blob yields fewer
//!   tokens rather than reading out of bounds.
//! * A QuakeC runtime error is id's `PR_RunError` → `Host_Error`: the first one
//!   halts the VM ([`Vm::execute`]) and ends whatever the server was doing — a
//!   frame, a level load — with that [`crate::QError::Program`] error, for the
//!   front-end to end the game on.
//! * The borrow discipline from `vm.rs` is respected: the host is only held out
//!   of the VM for the duration of a single trace / contents query, never across
//!   an [`Vm::execute`] call (which itself reaches the host via `with_host`).

use std::collections::HashMap;

use crate::bsp::Bsp;
use crate::math::Vec3;
use crate::stepping::Stepping;
use crate::vm::{Glb, GlobalOfs, Host, HostTrace, Vm};
use crate::Result;

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

pub use lightstyle::{lightstyle_scales_at, LerpLightStyles, GLIDE_STEP, MAX_LIGHTSTYLES};
pub use msg::{
    te_consts, wire_angle, wire_coord, GameMessage, Outbox, ParticleBurst, SoundEvent, StaticEntity,
    StaticSound, SvcEvent, TempEntityEvent,
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
pub(crate) mod testutil;

// ---------------------------------------------------------------------------
// Quake constants used by the server (server.h / sv_phys.c / pr_cmds.c).
// ---------------------------------------------------------------------------

/// Declares a server.h constant set QuakeC keeps in a float field as an enum
/// with an `Other` arm, decoded as id's `(int)` casts read the field.
macro_rules! qc_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident = $code:literal,)* }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vdoc])* $variant,)*
            /// A value no server.h constant has (QuakeC may store anything).
            Other(i32),
        }

        impl $name {
            /// The constant `code` names, or `Other(code)`.
            pub const fn from_code(code: i32) -> $name {
                match code {
                    $($code => $name::$variant,)*
                    other => $name::Other(other),
                }
            }

            /// The number QuakeC sees.
            pub const fn code(self) -> i32 {
                match self {
                    $($name::$variant => $code,)*
                    $name::Other(code) => code,
                }
            }
        }
    };
}

qc_enum!(
    /// An edict's `movetype` (server.h `MOVETYPE_*`): which of `SV_Physics`'
    /// movers runs it.
    ///
    /// QuakeC keeps it in a float. It decodes as `(int)ent->v.movetype`, the
    /// cast `SV_Physics_Client`'s switch makes; id's other tests compare the
    /// float with the constant (`ent->v.movetype == MOVETYPE_PUSH`), and the
    /// two agree on every value QuakeC stores, the constants themselves (a
    /// fractional movetype would read as its truncation here).
    MoveType {
        /// `MOVETYPE_NONE`: never moves (thinks only).
        None = 0,
        /// `MOVETYPE_ANGLENOCLIP`: defined by server.h, used by nothing.
        AngleNoClip = 1,
        /// `MOVETYPE_ANGLECLIP`: defined by server.h, used by nothing.
        AngleClip = 2,
        /// `MOVETYPE_WALK`: a player on foot (gravity, friction, steps).
        Walk = 3,
        /// `MOVETYPE_STEP`: a monster, moved by its AI in steps (gravity only
        /// when off the ground).
        Step = 4,
        /// `MOVETYPE_FLY`: no gravity.
        Fly = 5,
        /// `MOVETYPE_TOSS`: gravity, stops dead on a floor (gibs, the dead).
        Toss = 6,
        /// `MOVETYPE_PUSH`: a brush mover (doors, plats) that pushes others.
        Push = 7,
        /// `MOVETYPE_NOCLIP`: flies through everything.
        NoClip = 8,
        /// `MOVETYPE_FLYMISSILE`: `FLY`, clipping against monsters with a
        /// fattened box (`MOVE_MISSILE`).
        FlyMissile = 9,
        /// `MOVETYPE_BOUNCE`: `TOSS` that bounces (grenades).
        Bounce = 10,
    }
);

qc_enum!(
    /// An edict's `solid` (server.h `SOLID_*`): what it is to a trace. It
    /// decodes as `(int)ent->v.solid`, like [`MoveType`] (id compares the
    /// float, which agrees on every constant).
    Solid {
        /// `SOLID_NOT`: no interaction with other objects.
        Not = 0,
        /// `SOLID_TRIGGER`: touch on edge, but not blocking.
        Trigger = 1,
        /// `SOLID_BBOX`: touch on edge, block.
        BBox = 2,
        /// `SOLID_SLIDEBOX`: touch on edge, but not an onground (monsters,
        /// the player).
        SlideBox = 3,
        /// `SOLID_BSP`: bsp clip, touch on edge, block.
        Bsp = 4,
    }
);

/// An edict's `flags` (server.h `FL_*`): a bit-set QuakeC keeps in a float,
/// which id's C reads through `(int)ent->v.flags` and writes back as the
/// float of the new int.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntFlags(i32);

impl EntFlags {
    /// `FL_FLY`: a flying monster (no gravity, no steps).
    pub const FLY: EntFlags = EntFlags(1);
    /// `FL_SWIM`: a swimming monster (stays in water).
    pub const SWIM: EntFlags = EntFlags(2);
    /// `FL_CONVEYOR`: defined by server.h, used by nothing.
    pub const CONVEYOR: EntFlags = EntFlags(4);
    /// `FL_CLIENT`: a player's edict (what `checkclient` and the monsters'
    /// `FindTarget` look for).
    pub const CLIENT: EntFlags = EntFlags(8);
    /// `FL_INWATER`: a monster's box is in water.
    pub const INWATER: EntFlags = EntFlags(16);
    /// `FL_MONSTER`: AI-driven; missiles clip it with a fattened box.
    pub const MONSTER: EntFlags = EntFlags(32);
    /// `FL_GODMODE`: `god` is on.
    pub const GODMODE: EntFlags = EntFlags(64);
    /// `FL_NOTARGET`: `notarget` is on.
    pub const NOTARGET: EntFlags = EntFlags(128);
    /// `FL_ITEM`: an item (gets an extra-large bbox to be touched).
    pub const ITEM: EntFlags = EntFlags(256);
    /// `FL_ONGROUND`: standing on something.
    pub const ONGROUND: EntFlags = EntFlags(512);
    /// `FL_PARTIALGROUND`: not all corners are valid (`SV_FixCheckBottom`), so
    /// [`sv_movestep`] lets it move or fall instead of refusing every step.
    pub const PARTIALGROUND: EntFlags = EntFlags(1024);
    /// `FL_WATERJUMP`: a player jumping out of water.
    pub const WATERJUMP: EntFlags = EntFlags(2048);
    /// `FL_JUMPRELEASED`: the player let go of jump (for jump debouncing).
    pub const JUMPRELEASED: EntFlags = EntFlags(4096);

    /// The flags of the int `bits`.
    pub const fn from_bits(bits: i32) -> EntFlags {
        EntFlags(bits)
    }

    /// The int QuakeC's float holds.
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// Whether every flag of `other` is set.
    pub const fn contains(self, other: EntFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any flag of `other` is set.
    pub const fn intersects(self, other: EntFlags) -> bool {
        self.0 & other.0 != 0
    }

    /// These flags and `other`'s (`flags | FL_X`).
    pub const fn with(self, other: EntFlags) -> EntFlags {
        EntFlags(self.0 | other.0)
    }

    /// These flags less `other`'s (`flags & ~FL_X`).
    pub const fn without(self, other: EntFlags) -> EntFlags {
        EntFlags(self.0 & !other.0)
    }

    /// These flags with `other`'s flipped (`flags ^ FL_X`).
    pub const fn toggled(self, other: EntFlags) -> EntFlags {
        EntFlags(self.0 ^ other.0)
    }
}

impl std::ops::BitOr for EntFlags {
    type Output = EntFlags;

    fn bitor(self, other: EntFlags) -> EntFlags {
        self.with(other)
    }
}

/// The entity fields the engine reads as server.h types, through the handles
/// resolved at load ([`Vm::fo`]).
impl Vm {
    /// Edict `e`'s `movetype`.
    pub fn movetype(&self, e: i32) -> MoveType {
        MoveType::from_code(self.ent_float(e, self.fo().movetype) as i32)
    }

    /// Set edict `e`'s `movetype`.
    pub fn set_movetype(&mut self, e: i32, movetype: MoveType) {
        self.set_ent_float(e, self.fo().movetype, movetype.code() as f32);
    }

    /// Edict `e`'s `solid`.
    pub fn solid(&self, e: i32) -> Solid {
        Solid::from_code(self.ent_float(e, self.fo().solid) as i32)
    }

    /// Set edict `e`'s `solid`.
    pub fn set_solid(&mut self, e: i32, solid: Solid) {
        self.set_ent_float(e, self.fo().solid, solid.code() as f32);
    }

    /// Edict `e`'s `flags`.
    pub fn flags(&self, e: i32) -> EntFlags {
        EntFlags::from_bits(self.ent_float(e, self.fo().flags) as i32)
    }

    /// Set edict `e`'s `flags`.
    pub fn set_flags(&mut self, e: i32, flags: EntFlags) {
        self.set_ent_float(e, self.fo().flags, flags.bits() as f32);
    }
}

/// The QuakeC functions the engine calls, each named by a global
/// (`pr_global_struct->StartFrame`, progdefs.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SysFn {
    StartFrame,
    PlayerPreThink,
    PlayerPostThink,
    ClientKill,
    ClientConnect,
    PutClientInServer,
    SetNewParms,
    SetChangeParms,
}

impl SysFn {
    /// Its name, which is also its global's.
    fn name(self) -> &'static str {
        match self {
            SysFn::StartFrame => "StartFrame",
            SysFn::PlayerPreThink => "PlayerPreThink",
            SysFn::PlayerPostThink => "PlayerPostThink",
            SysFn::ClientKill => "ClientKill",
            SysFn::ClientConnect => "ClientConnect",
            SysFn::PutClientInServer => "PutClientInServer",
            SysFn::SetNewParms => "SetNewParms",
            SysFn::SetChangeParms => "SetChangeParms",
        }
    }

    /// The global holding it.
    fn global(self, go: &GlobalOfs) -> Glb {
        match self {
            SysFn::StartFrame => go.start_frame,
            SysFn::PlayerPreThink => go.player_pre_think,
            SysFn::PlayerPostThink => go.player_post_think,
            SysFn::ClientKill => go.client_kill,
            SysFn::ClientConnect => go.client_connect,
            SysFn::PutClientInServer => go.put_client_in_server,
            SysFn::SetNewParms => go.set_new_parms,
            SysFn::SetChangeParms => go.set_change_parms,
        }
    }
}

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
/// `parm1`..`parm16`, for the tests' progs (the engine reads them through
/// [`GlobalOfs::parms`]).
#[cfg(test)]
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
    /// The `skill`, `sv_gravity` and `registered` cvars (see [`ServerCvars`]).
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
        let registered = pak.as_ref().is_some_and(crate::common::is_registered);
        let mut w = WorldModel {
            bsp,
            precache_models: vec![String::new()],
            precache_sounds: vec![String::new()],
            pak,
            model_bounds: std::collections::HashMap::new(),
            outbox: Outbox::default(),
            cvars: ServerCvars { registered, ..ServerCvars::default() },
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

    fn find_model(&self, name: &str) -> Option<i32> {
        self.precache_models.iter().position(|s| s == name).map(|i| i as i32)
    }

    fn model_name(&self, idx: i32) -> Option<&str> {
        usize::try_from(idx).ok().and_then(|i| self.precache_models.get(i)).map(|s| s.as_str())
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

/// `standard_quake`/`rogue`/`hipnotic` (common.c): which game this `progs.dat`
/// is. id's engine sets these from `-rogue`/`-hipnotic` on the command line
/// (the search path, `common.rs`); the port instead detects the loaded
/// `progs.dat` itself — [`GameMode::detect`] — so every caller that builds a
/// [`Server`] (the browser's level loads, `quaketool`, the tests) gets the
/// right mode for free, with no extra argument to thread through. The outcome
/// is the same: a basedir laid out with `-hipnotic`'s directory always loads
/// `hipnotic`'s own `progs.dat`, never id's or `rogue`'s.
///
/// Read by the status bar ([`crate::sbar::Hud::mode`]) to draw the mission
/// packs' own weapons/items and remapped armour/ammo-type bits
/// (`Sbar_DrawInventory`/`Sbar_Draw`, sbar.c) the way `hipnotic`/`rogue` make
/// id's C draw them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameMode {
    /// id's 1996 game (or any progs that isn't recognised as one of the two
    /// mission packs below): `standard_quake = true`.
    #[default]
    Id1,
    /// Scourge of Armagon (`-hipnotic`).
    Hipnotic,
    /// Dissolution of Eternity (`-rogue`).
    Rogue,
}

impl GameMode {
    /// Detect which game `progs` is from content only it declares — never id's
    /// 1996 `progs.dat`, and the two mission packs don't declare each other's:
    /// Rogue's `give` cheat reads/writes the field `ammo_lava_nails`
    /// (`host_cmd.c`'s `Host_Give_f`, the `rogue` arm); Hipnotic's empathy
    /// shields cheat is the function `EmpathyShieldsCheat` (`weapons.qc`, dead
    /// code in the shipped game but still declared). Checked against the 2021
    /// re-release's `hipnotic`/`rogue` `progs.dat` (`quaketool dis`); id's
    /// shareware `progs.dat` has neither.
    pub fn detect(progs: &crate::progs::Progs) -> GameMode {
        if progs.find_field("ammo_lava_nails").is_some() {
            GameMode::Rogue
        } else if progs.find_function("EmpathyShieldsCheat").is_some() {
            GameMode::Hipnotic
        } else {
            GameMode::Id1
        }
    }
}

/// The headless Quake server: a QuakeC VM with the engine builtins installed and
/// a [`WorldModel`] host. Drives entity spawning and a minimal physics frame.
pub struct Server {
    pub vm: Vm,
    /// `standard_quake`/`rogue`/`hipnotic`, detected once at construction from
    /// the loaded `progs.dat` ([`GameMode::detect`]); never changes across a
    /// changelevel (a mission pack's levels all share its `progs.dat`).
    pub mode: GameMode,
    /// The map's entity description text (`bsp.entities`), captured before the
    /// BSP is moved into the host. `spawn_entities` tokenizes this. (The
    /// [`Host`] trait has no entity-text accessor and we never `unsafe`-downcast,
    /// so the server keeps its own copy.)
    entities: String,
    /// The local client's edict index, `None` until a client connects.
    ///
    /// SIMPLIFICATION (documented): canonical Quake reserves edict 1 for the
    /// first client in `SV_SpawnServer` (`sv.num_edicts = maxclients+1`) and
    /// keys `SV_Physics_Client` off the slot index `i <= svs.maxclients`. Here we
    /// instead reserve the *first free edict after `spawn_entities`* and remember
    /// it in this field (our stand-in for the single-element `svs.clients` table).
    /// Single-player QuakeC keys off `self`, not a hardcoded edict number, so the
    /// game logic is unaffected. Single client only; no netcode.
    /// (`pub(crate)`: the savegame loader re-identifies the player edict.)
    pub(crate) player: Option<i32>,
    /// The map's animated light-style patterns (`sv.lightstyles[64]`), owned by
    /// the server. The `lightstyle()` builtin sends its writes to the outbox;
    /// the server applies them to this field after each QuakeC execution window
    /// (`spawn_entities` / `run_frame`). `lightstyle_scales` reads it to produce
    /// the per-style brightness scales the renderer applies each frame. Empty in
    /// a new server, so a changelevel re-populates it from the new worldspawn.
    /// (`pub(crate)`: `Host_Loadgame_f` overwrites all 64 from the savegame.)
    pub(crate) lightstyles: [String; MAX_LIGHTSTYLES],
    /// The signon's static entities (`svc_spawnstatic`), owned like the
    /// light styles and filled the same way ([`Server::statics`]). Empty in a
    /// new server; a savegame's load rebuilds it by re-running the map's
    /// spawn functions, as id's did (a save holds no statics).
    statics: Vec<StaticEntity>,
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
    /// `(classname, count)` pairs, sorted by count descending then name.
    pub classnames: Vec<(String, usize)>,
}

/// The result of [`Server::run_frame`].
#[derive(Debug, Clone, Copy)]
pub struct FrameReport {
    /// Number of think functions that fired this frame.
    pub thinks_fired: usize,
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
        self.vm.sv_time() as f32
    }

    /// `sv.time`, the server clock (a `double` in id's `server_t`).
    pub fn sv_time(&self) -> f64 {
        self.vm.sv_time()
    }

    /// Set `sv.time` (and, as the C's next `pr_global_struct->time = sv.time`
    /// would, the QuakeC `time` global to its float).
    pub fn set_sv_time(&mut self, t: f64) {
        self.vm.set_sv_time(t);
        self.vm.set_glob_float(self.vm.go().time, t as f32);
    }

    /// The number of live (not-free) edicts, including the world (edict 0).
    pub fn live_entities(&self) -> usize {
        self.vm.live_edicts().count()
    }

    /// The local player's edict index, `None` until a client connects.
    pub fn player_edict(&self) -> Option<i32> {
        self.player
    }

    /// The player's edict while it is in use (not freed).
    fn live_player(&self) -> Option<i32> {
        self.player.filter(|&p| !self.vm.is_free_edict(p))
    }

    /// Convenience: the player's `health` field (for a HUD / verification). 0.0
    /// when no client is connected.
    pub fn player_health(&self) -> f32 {
        self.player.map_or(0.0, |p| self.vm.ent_float(p, self.vm.fo().health))
    }

    /// The player's view: `(eye, v_angle)` where `eye = origin + view_ofs`
    /// (defaulting `view_ofs` to `(0,0,22)` when the QuakeC left it unset) and
    /// `v_angle` is `[pitch, yaw, roll]`. Both are zero when no client exists.
    pub fn player_view(&self) -> ([f32; 3], [f32; 3]) {
        let Some(p) = self.player else {
            return ([0.0; 3], [0.0; 3]);
        };
        let origin = self.vm.ent_vec(p, self.vm.fo().origin);
        let mut ofs = self.vm.ent_vec(p, self.vm.fo().view_ofs);
        if ofs == [0.0, 0.0, 0.0] {
            ofs = [0.0, 0.0, DEFAULT_VIEWHEIGHT];
        }
        let eye = [origin[0] + ofs[0], origin[1] + ofs[1], origin[2] + ofs[2]];
        let v_angle = self.vm.ent_vec(p, self.vm.fo().v_angle);
        (eye, v_angle)
    }

    /// Resolve a *system* QuakeC function (`StartFrame`, `PlayerPreThink`, …):
    /// the function number in its like-named global (`pr_global_struct->X`),
    /// read at the call as id's does; for a progs that leaves the global unset
    /// (the tests'), the function of that name. `None` when there is neither.
    fn sys_function(&self, f: SysFn) -> Option<usize> {
        let g = self.vm.glob_int(f.global(self.vm.go()));
        if g > 0 && (g as usize) < self.vm.progs().functions.len() {
            return Some(g as usize);
        }
        self.vm.progs().find_function(f.name())
    }

    /// Execute a system QuakeC function with `self = self_e`, `other = other_e`.
    /// Returns `Ok(true)` if it existed and ran, `Ok(false)` if absent, and the
    /// program error if it failed (the VM has halted: see [`Vm::execute`]).
    fn run_sys(&mut self, sys: SysFn, self_e: i32, other_e: i32) -> Result<bool> {
        let Some(f) = self.sys_function(sys) else {
            return Ok(false);
        };
        self.vm.set_glob_int(self.vm.go().self_, self_e);
        self.vm.set_glob_int(self.vm.go().other, other_e);
        self.vm.execute(f)?;
        Ok(true)
    }

    /// The program error the VM halted on, as an `Err`, if it has: QuakeC
    /// the physics ran outside a think (a touch, a pusher's `blocked`) failed,
    /// and id's longjmp would have left the frame there.
    fn check_halted(&self) -> Result<()> {
        self.vm.halted().map_or(Ok(()), |e| Err(e.clone().into()))
    }

    /// The player's attack-relevant state for verification: `(button0, weapon,
    /// ammo_shells)`. All zero when no client is connected. `button0` is the
    /// attack bit copied from the last usercmd; `weapon`/`ammo_shells` are the
    /// QuakeC inventory fields the shotgun path reads/decrements.
    pub fn player_attack_state(&self) -> (f32, f32, f32) {
        let Some(p) = self.player else {
            return (0.0, 0.0, 0.0);
        };
        let button0 = self.vm.ent_float(p, self.vm.fo().button0);
        let weapon = self.vm.ent_float(p, self.vm.fo().weapon);
        let ammo_shells = self.vm.ent_float(p, self.vm.fo().ammo_shells);
        (button0, weapon, ammo_shells)
    }
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::testutil::*;
    use crate::progs::Progs;

    /// [`GameMode::detect`] on synthetic progs shaped like each game's real
    /// `progs.dat` (built once from the actual files, `quaketool dis`):
    /// id's has neither marker; Rogue's declares the field `ammo_lava_nails`
    /// (its `give` cheat); Hipnotic's declares the function
    /// `EmpathyShieldsCheat` (dead in the shipped game, but still there).
    #[test]
    fn game_mode_detects_from_the_progs_the_mission_packs_actually_declare() {
        let mut id1 = Builder::new();
        id1.add_field("health", 1, 0);
        assert_eq!(GameMode::detect(&Progs::parse(&id1.build()).unwrap()), GameMode::Id1);

        let mut rogue = Builder::new();
        rogue.add_field("ammo_lava_nails", 1, 0);
        assert_eq!(GameMode::detect(&Progs::parse(&rogue.build()).unwrap()), GameMode::Rogue);

        let mut hipnotic = Builder::new();
        hipnotic.add_function("EmpathyShieldsCheat", vec![]);
        assert_eq!(GameMode::detect(&Progs::parse(&hipnotic.build()).unwrap()), GameMode::Hipnotic);
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

    // ------------------------------------------------------ typed entity fields

    /// server.h's numbers, both ways; a number no constant has is `Other`.
    #[test]
    fn movetype_and_solid_are_server_h_numbers() {
        assert_eq!(MoveType::from_code(4), MoveType::Step);
        assert_eq!(MoveType::from_code(10), MoveType::Bounce);
        assert_eq!(MoveType::from_code(11), MoveType::Other(11));
        assert_eq!(Solid::from_code(4), Solid::Bsp);
        assert_eq!(Solid::from_code(-1), Solid::Other(-1));
        for code in -2..14 {
            assert_eq!(MoveType::from_code(code).code(), code);
            assert_eq!(Solid::from_code(code).code(), code);
        }
    }

    #[test]
    fn entflags_are_ids_bit_arithmetic() {
        let f = EntFlags::ONGROUND | EntFlags::CLIENT;
        assert_eq!(f.bits(), 512 | 8);
        assert!(f.contains(EntFlags::ONGROUND) && !f.contains(EntFlags::ONGROUND | EntFlags::FLY));
        assert!(f.intersects(EntFlags::ONGROUND | EntFlags::FLY));
        assert_eq!(f.without(EntFlags::ONGROUND), EntFlags::CLIENT); // flags & ~FL_ONGROUND
        assert_eq!(f.with(EntFlags::GODMODE).bits(), 512 | 8 | 64); // flags | FL_GODMODE
        assert_eq!(f.toggled(EntFlags::CLIENT), EntFlags::ONGROUND); // flags ^ FL_CLIENT
    }

    /// The typed accessors read the float field as id's `(int)` cast and
    /// write back the float of the number, through the resolved handles.
    #[test]
    fn typed_fields_are_the_float_fields_through_ids_int_cast() {
        let mut b = Builder::new();
        b.entityfields = 4;
        b.add_field("movetype", EV_FLOAT, 1);
        b.add_field("solid", EV_FLOAT, 2);
        b.add_field("flags", EV_FLOAT, 3);
        let progs = crate::progs::Progs::parse(&b.build()).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");
        let vm = &mut server.vm;
        let e = vm.spawn();
        vm.ent_set_float(e, "movetype", 4.0);
        vm.ent_set_float(e, "solid", 3.9); // (int)3.9 == SOLID_SLIDEBOX
        vm.ent_set_float(e, "flags", 520.0);
        assert_eq!(vm.movetype(e), MoveType::Step);
        assert_eq!(vm.solid(e), Solid::SlideBox);
        assert_eq!(vm.flags(e), EntFlags::ONGROUND | EntFlags::CLIENT);
        vm.set_movetype(e, MoveType::Other(12));
        vm.set_solid(e, Solid::Trigger);
        vm.set_flags(e, vm.flags(e).without(EntFlags::ONGROUND));
        assert_eq!(
            (vm.ent_get_float(e, "movetype"), vm.ent_get_float(e, "solid"), vm.ent_get_float(e, "flags")),
            (12.0, 1.0, 8.0)
        );
    }

    /// `Server::drain_output` hands over what the QuakeC printed, once.
    #[test]
    fn drain_output_takes_the_vm_output_log() {
        let progs = crate::progs::Progs::parse(&Builder::new().build()).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");
        server.vm.print("You got the shotgun\n");
        assert_eq!(server.drain_output(), "You got the shotgun\n");
        assert_eq!(server.drain_output(), "");
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
