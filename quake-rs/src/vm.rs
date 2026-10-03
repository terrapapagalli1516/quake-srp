//! The QuakeC virtual machine — the bytecode interpreter and its runtime.
//!
//! Ported from Quake (GPLv2): `pr_exec.c` (`PR_ExecuteProgram`,
//! `PR_EnterFunction`, `PR_LeaveFunction`, `PR_RunError`) and the edict /
//! string runtime in `pr_edict.c` (`ED_Alloc`, `ED_Free`, `ED_ClearEdict`,
//! `type_size[]`). Copyright (C) 1996-1997 Id Software, Inc.
//!
//! The original engine cast the raw global block and the edict array straight
//! to C pointers and `longjmp`ed out of `PR_RunError` on any fault. This port
//! is `#![forbid(unsafe_code)]`: every operand offset, function number, jump
//! target, builtin index and edict/field access is bounds-checked, and any
//! fault returns `Err(QError)` via [`Vm::run_error`] instead of aborting.
//!
//! ## The flat memory model
//!
//! * **Globals** live in `globals: Vec<u32>`, one *cell* per `u32`. A float is
//!   `f32::from_bits(cell)`; an int is `cell as i32`; a vector is three
//!   consecutive cells. This mirrors the C `pr_globals` union array.
//! * **Edicts** are stored flat in `edict_fields: Vec<u32>` of length
//!   `num_edicts * entityfields`. Edict `i`, field `f` is index
//!   `i*entityfields + f`. A parallel `edict_free: Vec<bool>` tracks free slots.
//!   Edict 0 is the world.
//! * An **entity value** (`ev_entity`, what `.entity` fields hold and what
//!   `OP_LOAD_*` reads as its entity operand) is the edict *index* (`i32`);
//!   `world == 0`.
//! * An **entity field pointer** (`ev_pointer`, produced by `OP_ADDRESS` and
//!   consumed by `OP_STOREP_*`) is the *flat cell index*
//!   `ent_index*entityfields + field_ofs`. This re-encodes the C's byte-offset
//!   pointer (`(byte*)&ed->v + ofs - (byte*)sv.edicts`) and round-trips
//!   identically under our layout.
//! * **Strings** are a `Vec<u8>` heap initialised from `progs.strings`.
//!   [`Vm::intern`] appends `"s\0"` and returns the byte offset (a `string_t`),
//!   once per distinct text; [`Vm::string`] reads a NUL-terminated string from
//!   an offset.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::{ProgramError, QError, Result};
use crate::math::Vec3;
use crate::progs::{string_in, Op, Progs, Statement, MAX_PARMS, OFS_PARM0, OFS_RETURN};
use crate::qrand::QRand;

mod print;

/// A native builtin. Reads its arguments and writes its return value through
/// the `Vm` helpers (`arg_*` / `ret_*`), exactly like the C `builtin_t`.
pub type Builtin = fn(&mut Vm) -> Result<()>;

/// The result of a world collision trace (the engine `trace_t`).
#[derive(Debug, Clone, Copy)]
pub struct HostTrace {
    /// Never left a solid region for the whole move.
    pub allsolid: bool,
    /// The move began inside a solid.
    pub startsolid: bool,
    /// Passed through empty (non-water) space.
    pub inopen: bool,
    /// Passed through water/slime/lava.
    pub inwater: bool,
    /// Fraction of the move completed before hitting something (`1.0` = clear).
    pub fraction: f32,
    /// Final position reached.
    pub endpos: Vec3,
    /// Surface normal at the impact (valid when `fraction < 1`).
    pub plane_normal: Vec3,
    /// Plane distance at the impact.
    pub plane_dist: f32,
}

impl Default for HostTrace {
    fn default() -> Self {
        HostTrace {
            allsolid: true,
            startsolid: false,
            inopen: false,
            inwater: false,
            fraction: 1.0,
            endpos: [0.0; 3],
            plane_normal: [0.0; 3],
            plane_dist: 0.0,
        }
    }
}

/// Engine world services the QuakeC builtins need but the pure VM lacks
/// (collision against the map, point contents, asset precaching, model bounds).
/// Implemented by the `server` module's world model; absent (`None`) for a bare
/// VM, in which case the engine builtins fault cleanly.
///
/// Geometry methods take `&self` (the map is immutable); precache methods take
/// `&mut self` to record names. Reached from builtins via [`Vm::with_host`].
pub trait Host {
    /// Register a model name, returning its model index (≥ 1).
    fn precache_model(&mut self, name: &str) -> i32;
    /// Register a sound name, returning its sound index (≥ 1).
    fn precache_sound(&mut self, name: &str) -> i32;
    /// Read-only scan of the sound precache table for `name`'s slot — the C
    /// `PF_ambientsound`'s "check to see if samp was properly precached" walk
    /// over `sv.sound_precache` (pr_cmds.c). `None` = not precached; never
    /// registers.
    fn find_sound(&self, name: &str) -> Option<i32>;
    /// `SV_ModelIndex` without its error: the precache index of a model
    /// already precached, or `None` (id's errors, "model not precached").
    /// It is what `STAT_WEAPON` carries for the view weapon.
    fn find_model(&self, name: &str) -> Option<i32>;
    /// Bounding box `(mins, maxs)` for a model name. Brush submodels (`"*N"`)
    /// return the BSP submodel bounds; unknown models return `None`.
    fn model_bbox(&self, name: &str) -> Option<(Vec3, Vec3)>;
    /// Box-trace `mins`/`maxs` from `start` to `end` against the world.
    fn trace(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> HostTrace;
    /// Contents value (`CONTENTS_*`) at point `p`.
    fn point_contents(&self, p: Vec3) -> i32;
    /// The world BSP, so entity-aware moves can clip against both the map and
    /// (via the caller's edict data) other entities in one place.
    fn bsp(&self) -> &crate::bsp::Bsp;
    /// What the builtins send out of the server — the message buffers and the
    /// host requests id's C wrote into `sv.datagram` and friends — for the
    /// server to hand to its host after the frame ([`crate::server::Outbox`]).
    fn outbox(&mut self) -> &mut crate::server::Outbox;
    /// The engine cvars `PF_cvar` reads (`Cvar_VariableValue`) and
    /// `PF_cvar_set` sets ([`crate::server::ServerCvars`]).
    fn cvars(&self) -> &crate::server::ServerCvars;
    /// [`Host::cvars`], to set.
    fn cvars_mut(&mut self) -> &mut crate::server::ServerCvars;
}

/// An entity field resolved once by name: its cell offset within an edict, or
/// `None` when the progs declares no such field. Reads of a missing field give
/// 0 and writes are dropped, exactly as the by-name accessors
/// ([`Vm::ent_get_float`] and friends) behave; they resolve the name and call
/// the same code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fld(Option<u16>);

/// A global resolved once by name (see [`Fld`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Glb(Option<u16>);

impl Fld {
    fn ofs(self) -> Option<usize> {
        self.0.map(usize::from)
    }

    /// Whether the progs declares this field: `GetEdictFieldValue` finding it,
    /// for the engine paths that do one thing when a field exists and another
    /// when it does not (`items2`).
    pub fn is_declared(self) -> bool {
        self.0.is_some()
    }
}

impl Glb {
    fn ofs(self) -> Option<usize> {
        self.0.map(usize::from)
    }
}

/// The QuakeC name of a [`FieldOfs`] or [`GlobalOfs`] member: its
/// identifier, or the literal given for one whose QuakeC name is not a Rust
/// field name (a keyword, a capital).
macro_rules! qc_name {
    ($name:ident) => {
        stringify!($name)
    };
    ($name:ident $qc:literal) => {
        $qc
    };
}

/// Declares a table of handles resolved by name when the progs loads: one
/// `$handle` per member, looked up with `Progs::$lookup`.
macro_rules! resolved_by_name {
    ($(#[$doc:meta])* $table:ident of $handle:ident by $lookup:ident:
     $($name:ident $(= $qc:literal)?),* $(,)?) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $table {
            $(pub $name: $handle,)*
        }

        impl $table {
            /// Resolve every member in `progs` by name.
            pub fn resolve(progs: &Progs) -> $table {
                $table { $($name: $handle(progs.$lookup(qc_name!($name $($qc)?))),)* }
            }
        }
    };
}

resolved_by_name!(
    /// `entvars_t` (progdefs.h): every engine-visible entity field, resolved by
    /// name once, when the progs is loaded ([`Vm::new`]), plus the two id's
    /// engine finds with `GetEdictFieldValue`: `gravity` (`SV_AddGravity`) and
    /// `items2` (`SV_WriteClientdataToMessage`; only the mission packs' progs
    /// have it, so it resolves to nothing for id1's). id's engine reads these
    /// as struct members at offsets fixed by the id1 progs; the port looks them
    /// up by name so any progs works, and this table spares the hot paths a hash
    /// of the name on every access (PERF_PLAN D2).
    FieldOfs of Fld by field_offset:
    modelindex, absmin, absmax, ltime, movetype, solid, origin, oldorigin, velocity, angles,
    avelocity, punchangle, classname, model, frame, skin, effects, mins, maxs, size, touch,
    use_ = "use", think, blocked, nextthink, groundentity, health, frags, weapon, weaponmodel,
    weaponframe, currentammo, ammo_shells, ammo_nails, ammo_rockets, ammo_cells, items,
    takedamage, chain, deadflag, view_ofs, button0, button1, button2, impulse, fixangle, v_angle,
    idealpitch, netname, enemy, flags, colormap, team, max_health, teleport_time, armortype,
    armorvalue, waterlevel, watertype, ideal_yaw, yaw_speed, aiment, goalentity, spawnflags,
    target, targetname, dmg_take, dmg_save, dmg_inflictor, owner, movedir, message, sounds,
    noise, noise1, noise2, noise3, gravity, items2,
);

resolved_by_name!(
    /// `globalvars_t` (progdefs.h): every global the engine reads or writes,
    /// resolved by name at progs load like [`FieldOfs`] — the parameter
    /// block's `self`/`other`/`time`, the level counters, the spawn parms,
    /// `makevectors`' and `traceline`'s results, and the functions the
    /// engine calls (`StartFrame`, `PlayerPreThink`, ...).
    GlobalOfs of Glb by global_offset:
    self_ = "self", other, world, time, frametime, force_retouch, mapname, deathmatch, coop,
    teamplay, serverflags, total_secrets, total_monsters, found_secrets, killed_monsters,
    parm1, parm2, parm3, parm4, parm5, parm6, parm7, parm8, parm9, parm10, parm11, parm12,
    parm13, parm14, parm15, parm16, v_forward, v_up, v_right, trace_allsolid, trace_startsolid,
    trace_fraction, trace_endpos, trace_plane_normal, trace_plane_dist, trace_ent, trace_inopen,
    trace_inwater, msg_entity, main, start_frame = "StartFrame",
    player_pre_think = "PlayerPreThink", player_post_think = "PlayerPostThink",
    client_kill = "ClientKill", client_connect = "ClientConnect",
    put_client_in_server = "PutClientInServer", client_disconnect = "ClientDisconnect",
    set_new_parms = "SetNewParms", set_change_parms = "SetChangeParms",
);

impl GlobalOfs {
    /// The spawn parms `parm1`..`parm16`, in order (`NUM_SPAWN_PARMS`).
    pub fn parms(&self) -> [Glb; 16] {
        [
            self.parm1, self.parm2, self.parm3, self.parm4, self.parm5, self.parm6, self.parm7,
            self.parm8, self.parm9, self.parm10, self.parm11, self.parm12, self.parm13,
            self.parm14, self.parm15, self.parm16,
        ]
    }
}

/// Maximum interpreter call depth (`MAX_STACK_DEPTH`).
const MAX_STACK_DEPTH: usize = 32;
/// Size of the saved-locals stack (`LOCALSTACK_SIZE`).
const LOCALSTACK_SIZE: usize = 2048;
/// Statement budget before declaring a runaway loop (matches the C constant).
const RUNAWAY: u32 = 100_000;
/// Hard edict ceiling (`quakedef.h MAX_EDICTS`): the C `ED_Alloc` calls
/// `Sys_Error("ED_Alloc: no free edicts")` when this is exceeded. We surface it as
/// a `run_error` from the QuakeC-reachable `PF_Spawn` (via [`Vm::spawn_checked`])
/// so a runaway `spawn()` loop fails cleanly instead of growing memory unbounded.
/// id's own number, and [`Vm::classic`][Vm]'s ceiling — in Classic this is
/// still the only ceiling there is. In 2026 the `sv_max_edicts` cvar
/// ([`Vm::set_max_edicts`]) can raise the live ceiling past it (never below:
/// see [`MAX_EDICTS_LIMIT`]).
pub const MAX_EDICTS: usize = 600;

/// The `sv_max_edicts` cvar's own ceiling: not id's (there is no real
/// engine's own number to match here — the real, never-open-sourced Rogue
/// engine that shipped `r2m6` is undocumented; see `AUDIT.md`), so this is
/// the port's own choice, well clear of where an edict number stops being
/// representable at all: `svc_spawnbaseline`/`svc_update`'s entity number is
/// a signed 16-bit field end to end (`common.c`'s `MSG_ReadShort` casts
/// through `(short)`), so 32768 would read back as -32768 — and every
/// parser here already rejects a negative entity number, demo.rs's
/// `svc_spawnbaseline` included. 32000 leaves headroom under that 32767
/// wall, the same margin QuakeSpasm's own `max_edicts` cvar keeps.
///
/// One corner stays short of 32767: `svc_sound`'s entity+channel field packs
/// `(ent << 3) | channel` into a signed 16-bit short (`sv_main.c`), so a
/// sound AT an entity above 4095 would corrupt that pack — but this port's
/// live single-player client never serializes sound through that encoding
/// (`server::msg::start_sound` carries `ent`/`channel` as plain fields); only
/// a hypothetical future demo *recorder* would need to mind it. No shipped
/// map needs anywhere near 4095 edicts, let alone 32000.
pub const MAX_EDICTS_LIMIT: usize = 32_000;

/// `MAX_ENT_LEAFS` (progs.h): how many BSP leaves `SV_FindTouchedLeafs`
/// records for one edict. An entity touching more is known by its first 16
/// only, so the server can miss it (a large door can vanish, as in id's game).
pub const MAX_ENT_LEAFS: usize = 16;

/// `edict_t.num_leafs` / `leafnums[]` (progs.h): the world leaves an edict
/// touched when `SV_LinkEdict` last linked it (see
/// [`crate::bsp::Bsp::touched_leafs`]). `SV_WriteEntitiesToClient` sends an
/// entity only when one of them is in the client's fat PVS. Leaf numbers are
/// `bsp.leafs` indices (the C stores them less one; the PVS bit is the same).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdictLeafs {
    num: u8,
    leafnums: [u16; MAX_ENT_LEAFS],
}

impl EdictLeafs {
    /// Record `leaf` unless all [`MAX_ENT_LEAFS`] slots are taken; returns
    /// whether there is room for another (the walk stops when there is not).
    pub fn push(&mut self, leaf: usize) -> bool {
        let n = self.num as usize;
        if n < MAX_ENT_LEAFS {
            self.leafnums[n] = leaf.min(u16::MAX as usize) as u16;
            self.num += 1;
        }
        (self.num as usize) < MAX_ENT_LEAFS
    }

    /// The recorded leaf numbers.
    pub fn leafs(&self) -> &[u16] {
        &self.leafnums[..self.num as usize]
    }
}

/// One saved interpreter frame (`prstack_t`): where to resume and in which
/// function, so `PR_LeaveFunction` can restore them.
#[derive(Clone, Copy)]
struct Frame {
    /// Statement index to resume at in the caller (`prstack_t.s`).
    s: usize,
    /// Caller function index (`prstack_t.f`).
    f: usize,
}

/// The QuakeC virtual machine: the loaded program plus all mutable runtime
/// state (globals, edicts, the string heap, builtins and console output).
///
/// Its state is private: the engine reads and writes QuakeC values through
/// the typed accessors (entity fields by [`Fld`] handle from [`Vm::fo`],
/// globals by [`Glb`] from [`Vm::go`], builtin arguments with `arg_*` /
/// `ret_*`), and the few engine paths that need more (the savegame loader,
/// a tool calling one builtin) have a method each.
pub struct Vm {
    progs: Progs,
    /// The mutable global block, one cell per `u32`.
    globals: Vec<u32>,
    /// Flat edict field storage: `num_edicts * entityfields` cells.
    edict_fields: Vec<u32>,
    /// Free flag per edict (parallel to the edict array).
    edict_free: Vec<bool>,
    /// The string heap (`pr_strings`), initialised from `progs.strings`.
    strings: Vec<u8>,
    /// Every text [`Vm::intern`] has appended to `strings`, and where.
    interned: HashMap<Box<str>, i32>,
    /// Native builtin table; index 0 is reserved (`PF_Fixme`).
    builtins: Vec<Builtin>,
    /// What the QuakeC printed ([`Vm::print`]), until the host takes it
    /// ([`Vm::take_output`]).
    output: String,
    /// `pr_argc` — number of arguments to the builtin currently running.
    argc: usize,
    /// When set, the interpreter records a per-statement trace into `output`.
    trace: bool,
    /// The entity fields (`entvars_t`) resolved by name at load ([`Vm::fo`]).
    fo: FieldOfs,
    /// The globals (`globalvars_t`) resolved by name at load ([`Vm::go`]).
    go: GlobalOfs,
    /// Optional engine host providing world services to the engine builtins.
    /// Taken out and restored around each use via [`Vm::with_host`] so a builtin
    /// can mutate both the host and the rest of the VM without a borrow clash.
    host: Option<Box<dyn Host>>,
    /// `sv.time`, the server clock: a `double` in id's `server_t` (server.h),
    /// advanced by `SV_Physics`' `sv.time += host_frametime` at the end of each
    /// frame (1.0 at spawn, the save's time after a load). QuakeC sees it
    /// through the `time` global, a float: each `pr_global_struct->time =
    /// sv.time` stores `sv_time as f32`. It lives on the VM because
    /// `ED_Alloc`/`ED_Free` (freetime) and the monster-locomotion builtins
    /// (`walkmove`/`movetogoal`'s relink touches: world.c SV_TouchLinks sets
    /// `time = sv.time`, not the clamped thinktime the global holds during a
    /// monster think) read it. [`crate::server::Server::time`] is its float.
    sv_time: f64,
    /// `host_frametime` (host.c, a `double`) of the server frame running: the
    /// think-due and pusher tests compare floats against `sv.time +
    /// host_frametime` in double.
    host_frametime: f64,

    /// Monotonic count of QuakeC statements executed across this VM's lifetime
    /// (one per `execute` loop iteration). A free running total used by the sim
    /// benchmark to report VM workload per frame; not gameplay state.
    stmt_count: u64,

    /// `edict_t.freetime` per edict: the `sv_time` of its last `ED_Free`
    /// (missing entries read as 0). `ED_Alloc` leaves a slot alone for 0.5 s
    /// after it was freed, except in the first two seconds of server time.
    edict_freetime: Vec<f32>,
    /// `edict_t.num_leafs`/`leafnums` per edict, written by `SV_LinkEdict`
    /// ([`Vm::set_edict_leafs`]); missing entries read as no leaves.
    edict_leafs: Vec<EdictLeafs>,
    /// The edicts `makestatic` turned into client statics. The C writes one
    /// `svc_spawnstatic` into the signon and frees the edict, and the client
    /// draws it through efrags (`R_AddEfrags` / `R_StoreEfrags`) and never
    /// relinks it. The port keeps the edict (edict numbering and savegames
    /// follow it) and marks it here, so the client can treat it as a static.
    /// Missing entries read as not static; `ED_Alloc` and `ED_Free` clear it.
    edict_static: Vec<bool>,
    /// The host session's random streams ([`QRand`]) that `random()` and the
    /// monsters' chase directions draw from: a VM's own fresh ones until the
    /// host hands it its session's ([`Vm::set_rand`]).
    rand: Rc<QRand>,
    /// The mission packs' `$key` string table (AUDIT P6/B3), shared the same
    /// way `rand` is: `None` for a bare VM or a plain id1 game (its progs has
    /// no `$` string to look up), `Some` once a server built from a pak that
    /// carries `localization/loc_english.txt` loads it
    /// ([`Vm::set_loc_table`]). [`crate::builtins::var_string`] reads it
    /// through [`Vm::loc_table`]; the client reads it the same way to
    /// resolve `svc_finale`/`svc_cutscene`'s raw text.
    loc: Option<Rc<crate::localization::LocTable>>,
    /// The program error this VM halted on ([`Vm::execute`]): id's
    /// `PR_RunError` ends the game, so the VM runs no more QuakeC once one has
    /// happened, until a harness resumes it ([`Vm::reset_execution`]).
    halted: Option<ProgramError>,
    /// The live `ED_Alloc` ceiling [`Vm::spawn_checked`] enforces: id's
    /// [`MAX_EDICTS`] (600) unless a 2026-only extra raised it
    /// ([`Vm::set_max_edicts`], the `sv_max_edicts` cvar). Never below
    /// [`MAX_EDICTS`] — this is a departure that only ever gives QuakeC more
    /// room, never less, so Classic's ceiling is untouched.
    max_edicts: usize,

    // --- private execution state ---
    /// Call stack of saved caller frames (`pr_stack` / `pr_depth`).
    stack: Vec<Frame>,
    /// Saved-locals stack (`localstack` / `localstack_used`).
    localstack: Vec<u32>,
    /// Current function index (`pr_xfunction`).
    xfunction: usize,
    /// Current statement index (`pr_xstatement`).
    xstatement: usize,
}

impl Vm {
    /// Build a VM from an already-parsed program. Copies the initial globals
    /// and string heap, allocates edict 0 (the world) and installs the default
    /// builtin table.
    pub fn new(progs: Progs) -> Vm {
        let globals = progs.globals.clone();
        let strings = progs.strings.clone();

        let fo = FieldOfs::resolve(&progs);
        let go = GlobalOfs::resolve(&progs);
        let mut vm = Vm {
            fo,
            go,
            progs,
            globals,
            edict_fields: Vec::new(),
            edict_free: Vec::new(),
            strings,
            interned: HashMap::new(),
            builtins: crate::builtins::default_builtins(),
            output: String::new(),
            argc: 0,
            trace: false,
            host: None,
            sv_time: 0.0,
            host_frametime: 0.0,
            stmt_count: 0,
            edict_freetime: Vec::new(),
            edict_leafs: Vec::new(),
            edict_static: Vec::new(),
            rand: Rc::new(QRand::new()),
            loc: None,
            halted: None,
            stack: Vec::new(),
            localstack: Vec::new(),
            xfunction: 0,
            xstatement: 0,
            max_edicts: MAX_EDICTS,
        };

        // Edict 0 is the world. ED_ClearEdict zeroes its fields; it is not free.
        let ef = vm.entityfields();
        vm.edict_fields = vec![0u32; ef];
        vm.edict_free = vec![false];
        vm
    }

    /// Parse a `progs.dat` image and build a VM from it.
    pub fn load(bytes: &[u8]) -> Result<Vm> {
        Ok(Vm::new(Progs::parse(bytes)?))
    }

    /// Install the engine host (world services for the engine builtins).
    pub fn set_host(&mut self, host: Box<dyn Host>) {
        self.host = Some(host);
    }

    /// The engine host, if one is installed.
    pub fn host(&self) -> Option<&dyn Host> {
        self.host.as_deref()
    }

    /// The engine host, to change (its outbox, its cvars).
    pub fn host_mut(&mut self) -> Option<&mut (dyn Host + 'static)> {
        self.host.as_deref_mut()
    }

    /// The loaded program (read-only: the VM's copy of the globals and the
    /// string heap are the live ones).
    pub fn progs(&self) -> &Progs {
        &self.progs
    }

    /// The entity fields (`entvars_t`) resolved at load: read and write them
    /// with [`Vm::ent_float`] and friends.
    pub fn fo(&self) -> &FieldOfs {
        &self.fo
    }

    /// The globals (`globalvars_t`) resolved at load: read and write them
    /// with [`Vm::glob_float`] and friends.
    pub fn go(&self) -> &GlobalOfs {
        &self.go
    }

    /// `sv.time`, the server clock (see the field).
    pub fn sv_time(&self) -> f64 {
        self.sv_time
    }

    /// Set `sv.time` (the clock only; the QuakeC `time` global is the
    /// server's to set).
    pub fn set_sv_time(&mut self, t: f64) {
        self.sv_time = t;
    }

    /// `host_frametime` of the server frame running.
    pub fn host_frametime(&self) -> f64 {
        self.host_frametime
    }

    /// Set `host_frametime` for the server frame about to run.
    pub fn set_host_frametime(&mut self, t: f64) {
        self.host_frametime = t;
    }

    /// QuakeC statements executed over this VM's life (the sim benchmark's
    /// workload count; not gameplay state).
    pub fn stmt_count(&self) -> u64 {
        self.stmt_count
    }

    /// Record a per-statement trace into the output log (a debugging aid,
    /// id's `pr_trace`; the port's `traceon` builtin leaves it alone).
    pub fn set_trace(&mut self, on: bool) {
        self.trace = on;
    }

    // --- builtins ---------------------------------------------------------------

    /// The builtin table (`pr_builtins`), by QuakeC builtin number.
    pub fn builtins(&self) -> &[Builtin] {
        &self.builtins
    }

    /// Install `f` as builtin number `n` (a number past the table's end is
    /// ignored: the table is id's `pr_builtin[]`, whose size is fixed).
    pub fn set_builtin(&mut self, n: usize, f: Builtin) {
        if let Some(slot) = self.builtins.get_mut(n) {
            *slot = f;
        }
    }

    /// Call builtin `n` as an `OP_CALLn` with `argc` arguments would (`pr_argc
    /// = argc`, then `pr_builtins[n]()`), its arguments already in the parm
    /// globals: for a tool or a test that pokes one builtin, since a builtin
    /// cannot be an entry function ([`Vm::execute`]).
    pub fn call_builtin(&mut self, n: usize, argc: usize) -> Result<()> {
        let Some(&f) = self.builtins.get(n).filter(|_| n != 0) else {
            return Err(self.run_error(format!("bad builtin call number {n}")));
        };
        self.argc = argc;
        f(self)
    }

    /// `pr_argc`: how many arguments the running builtin was called with.
    pub fn argc(&self) -> usize {
        self.argc
    }

    // --- the output log -------------------------------------------------------

    /// Print `text` to the VM's output log: what QuakeC printed (`dprint`,
    /// `bprint`, `sprint`, `centerprint`, the `error`/`objerror` reports)
    /// and the console lines id's server printed while running it.
    pub fn print(&mut self, text: &str) {
        self.output.push_str(text);
    }

    /// The output log since it was last taken.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Take the output log, leaving it empty.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    /// The random streams this VM draws from ([`QRand`]).
    pub fn rand(&self) -> &Rc<QRand> {
        &self.rand
    }

    /// Draw from `rand` from now on: the host session's streams, which it
    /// hands to each new server so they continue across level loads.
    pub fn set_rand(&mut self, rand: Rc<QRand>) {
        self.rand = rand;
    }

    /// The mission packs' `$key` string table, if one was loaded
    /// ([`Vm::set_loc_table`]).
    pub fn loc_table(&self) -> Option<&crate::localization::LocTable> {
        self.loc.as_deref()
    }

    /// Resolve `$key` strings against `loc` from now on (AUDIT P6/B3):
    /// [`crate::server::Server::with_pak`] calls this once, right after
    /// loading the pak's `localization/loc_english.txt`, for every game —
    /// id1's has none, so this is simply never called for it and
    /// [`Vm::loc_table`] stays `None`.
    pub fn set_loc_table(&mut self, loc: Rc<crate::localization::LocTable>) {
        self.loc = Some(loc);
    }

    /// Run `f` with both `self` and the engine host borrowed mutably, by taking
    /// the host out of `self` for the duration and restoring it afterward. This
    /// lets an engine builtin touch both the world (host) and the VM (globals,
    /// edicts, strings). Returns `None` (and does nothing) if no host is set.
    pub fn with_host<R>(&mut self, f: impl FnOnce(&mut Vm, &mut dyn Host) -> R) -> Option<R> {
        let mut host = self.host.take()?;
        let r = f(self, host.as_mut());
        self.host = Some(host);
        Some(r)
    }

    // --- name-resolved access (for the engine builtins and the spawner) ------

    /// Entity-field cell offset for `name`: a hash of the name into the progs'
    /// name->ofs map. Per-frame code holds a resolved [`Fld`] from [`Vm::fo`].
    pub fn field_ofs(&self, name: &str) -> Option<usize> {
        self.progs.field_offset(name).map(|o| o as usize)
    }
    /// Global cell offset for `name`, through the progs' name->ofs map.
    pub fn global_ofs(&self, name: &str) -> Option<usize> {
        self.progs.global_offset(name).map(|o| o as usize)
    }

    /// Resolve entity field `name` to a handle (see [`Fld`]).
    pub fn fld(&self, name: &str) -> Fld {
        Fld(self.progs.field_offset(name))
    }
    /// Resolve global `name` to a handle (see [`Glb`]).
    pub fn glb(&self, name: &str) -> Glb {
        Glb(self.progs.global_offset(name))
    }

    // --- resolved access: the hot paths hold a Fld/Glb from `fo`/`go` --------

    /// Read field `f` of edict `e` as a float (0.0 for a missing field).
    pub fn ent_float(&self, e: i32, f: Fld) -> f32 {
        f.ofs().map(|o| self.ef(e, o)).unwrap_or(0.0)
    }
    /// Write field `f` of edict `e` as a float (dropped for a missing field).
    pub fn set_ent_float(&mut self, e: i32, f: Fld, v: f32) {
        if let Some(o) = f.ofs() {
            self.set_ef(e, o, v);
        }
    }
    /// Read field `f` of edict `e` as an int.
    pub fn ent_int(&self, e: i32, f: Fld) -> i32 {
        f.ofs().map(|o| self.ei(e, o)).unwrap_or(0)
    }
    /// Write field `f` of edict `e` as an int.
    pub fn set_ent_int(&mut self, e: i32, f: Fld, v: i32) {
        if let Some(o) = f.ofs() {
            self.set_ei(e, o, v);
        }
    }
    /// Read field `f` of edict `e` as a vector.
    pub fn ent_vec(&self, e: i32, f: Fld) -> Vec3 {
        f.ofs().map(|o| self.ev(e, o)).unwrap_or([0.0; 3])
    }
    /// Write field `f` of edict `e` as a vector.
    pub fn set_ent_vec(&mut self, e: i32, f: Fld, v: Vec3) {
        if let Some(o) = f.ofs() {
            self.set_ev(e, o, v);
        }
    }
    /// Borrow string field `f` of edict `e` (`""` for a null or bad string).
    pub fn ent_str(&self, e: i32, f: Fld) -> &str {
        string_in(&self.strings, self.ent_int(e, f))
    }
    /// Intern `value` and store its `string_t` in string field `f` of edict
    /// `e` (dropped for a missing field).
    pub fn set_ent_string(&mut self, e: i32, f: Fld, value: &str) {
        let s = self.intern(value);
        self.set_ent_int(e, f, s);
    }
    /// Read global `g` as a float.
    pub fn glob_float(&self, g: Glb) -> f32 {
        g.ofs().map(|o| self.gf(o)).unwrap_or(0.0)
    }
    /// Write global `g` as a float.
    pub fn set_glob_float(&mut self, g: Glb, v: f32) {
        if let Some(o) = g.ofs() {
            self.set_gf(o, v);
        }
    }
    /// Read global `g` as an int.
    pub fn glob_int(&self, g: Glb) -> i32 {
        g.ofs().map(|o| self.gi(o)).unwrap_or(0)
    }
    /// Write global `g` as an int (also `.entity`/`.function` globals).
    pub fn set_glob_int(&mut self, g: Glb, v: i32) {
        if let Some(o) = g.ofs() {
            self.set_gi(o, v);
        }
    }
    /// Read global `g` as a vector.
    pub fn glob_vec(&self, g: Glb) -> Vec3 {
        g.ofs().map(|o| self.gv(o)).unwrap_or([0.0; 3])
    }
    /// Write global `g` as a vector.
    pub fn set_glob_vec(&mut self, g: Glb, v: Vec3) {
        if let Some(o) = g.ofs() {
            self.set_gv(o, v);
        }
    }

    // --- by-name access: resolve the name, then the same code ----------------
    // For tools and tests, and for names that come from data; the engine holds
    // resolved handles ([`Vm::fo`], [`Vm::go`]).

    /// Read entity field `name` as a float (0.0 if the field is unknown).
    pub fn ent_get_float(&self, e: i32, name: &str) -> f32 {
        self.ent_float(e, self.fld(name))
    }
    /// Write entity field `name` as a float (no-op if the field is unknown).
    pub fn ent_set_float(&mut self, e: i32, name: &str, v: f32) {
        self.set_ent_float(e, self.fld(name), v);
    }
    /// Read entity field `name` as an int.
    pub fn ent_get_int(&self, e: i32, name: &str) -> i32 {
        self.ent_int(e, self.fld(name))
    }
    /// Write entity field `name` as an int.
    pub fn ent_set_int(&mut self, e: i32, name: &str, v: i32) {
        self.set_ent_int(e, self.fld(name), v);
    }
    /// Read entity field `name` as a vector.
    pub fn ent_get_vector(&self, e: i32, name: &str) -> Vec3 {
        self.ent_vec(e, self.fld(name))
    }
    /// Write entity field `name` as a vector.
    pub fn ent_set_vector(&mut self, e: i32, name: &str, v: Vec3) {
        self.set_ent_vec(e, self.fld(name), v);
    }
    /// Resolve entity field `name` (a `string_t`) to an owned string.
    pub fn ent_get_string(&self, e: i32, name: &str) -> String {
        self.ent_string_ref(e, name).to_string()
    }
    /// Borrow entity field `name` as a `&str` (no owned-String allocation).
    /// Returns `""` for a null/out-of-range string.
    pub fn ent_string_ref(&self, e: i32, name: &str) -> &str {
        self.ent_str(e, self.fld(name))
    }
    /// Intern `value` and store its `string_t` in entity field `name`.
    pub fn ent_set_string(&mut self, e: i32, name: &str, value: &str) {
        self.set_ent_string(e, self.fld(name), value);
    }

    /// Read global `name` as a float.
    pub fn gget_float(&self, name: &str) -> f32 {
        self.glob_float(self.glb(name))
    }
    /// Write global `name` as a float.
    pub fn gset_float(&mut self, name: &str, v: f32) {
        self.set_glob_float(self.glb(name), v);
    }
    /// Read global `name` as an int.
    pub fn gget_int(&self, name: &str) -> i32 {
        self.glob_int(self.glb(name))
    }
    /// Write global `name` as an int (also used for `.entity`/`.function` globals).
    pub fn gset_int(&mut self, name: &str, v: i32) {
        self.set_glob_int(self.glb(name), v);
    }
    /// Read global `name` as a vector.
    pub fn gget_vector(&self, name: &str) -> Vec3 {
        self.glob_vec(self.glb(name))
    }
    /// Write global `name` as a vector.
    pub fn gset_vector(&mut self, name: &str, v: Vec3) {
        self.set_glob_vec(self.glb(name), v);
    }

    /// Number of 32-bit fields per entity (`progs->entityfields`). Always at
    /// least 1 so edict storage is never zero-strided.
    pub fn entityfields(&self) -> usize {
        (self.progs.entityfields.max(0) as usize).max(1)
    }

    // ----------------------------------------------------------------- globals

    /// Read a float global. Out-of-range offsets read as `0.0` (saturating).
    pub fn gf(&self, ofs: usize) -> f32 {
        f32::from_bits(self.globals.get(ofs).copied().unwrap_or(0))
    }
    /// Write a float global. Out-of-range offsets are ignored (no panic).
    pub fn set_gf(&mut self, ofs: usize, v: f32) {
        if let Some(c) = self.globals.get_mut(ofs) {
            *c = v.to_bits();
        }
    }
    /// Read an int global. Out-of-range offsets read as `0`.
    pub fn gi(&self, ofs: usize) -> i32 {
        self.globals.get(ofs).copied().unwrap_or(0) as i32
    }
    /// Write an int global. Out-of-range offsets are ignored.
    pub fn set_gi(&mut self, ofs: usize, v: i32) {
        if let Some(c) = self.globals.get_mut(ofs) {
            *c = v as u32;
        }
    }
    /// Read a vector global (3 cells). Missing cells read as `0.0`.
    pub fn gv(&self, ofs: usize) -> [f32; 3] {
        [self.gf(ofs), self.gf(ofs + 1), self.gf(ofs + 2)]
    }
    /// Write a vector global (3 cells). Out-of-range cells are ignored.
    pub fn set_gv(&mut self, ofs: usize, v: [f32; 3]) {
        self.set_gf(ofs, v[0]);
        self.set_gf(ofs + 1, v[1]);
        self.set_gf(ofs + 2, v[2]);
    }

    // ----------------------------------------------------- builtin arg / return

    /// Cell offset of builtin argument `n` (`OFS_PARM0 + n*3`).
    fn parm_ofs(n: usize) -> usize {
        OFS_PARM0 + n * 3
    }

    /// Read builtin argument `n` as a float (`G_FLOAT(OFS_PARMn)`).
    pub fn arg_float(&self, n: usize) -> f32 {
        self.gf(Self::parm_ofs(n))
    }
    /// Read builtin argument `n` as an int (`G_INT(OFS_PARMn)`).
    pub fn arg_int(&self, n: usize) -> i32 {
        self.gi(Self::parm_ofs(n))
    }
    /// Read builtin argument `n` as a vector (`G_VECTOR(OFS_PARMn)`).
    pub fn arg_vector(&self, n: usize) -> [f32; 3] {
        self.gv(Self::parm_ofs(n))
    }
    /// Read builtin argument `n` as an entity index (`G_EDICTNUM`).
    pub fn arg_entity(&self, n: usize) -> i32 {
        self.gi(Self::parm_ofs(n))
    }
    /// Read builtin argument `n` as a string (`G_STRING(OFS_PARMn)`), resolving
    /// the `string_t` to an owned `String`.
    pub fn arg_string(&self, n: usize) -> String {
        self.get_string(self.gi(Self::parm_ofs(n)))
    }

    /// Write the builtin return value as a float (`G_FLOAT(OFS_RETURN)`).
    pub fn ret_float(&mut self, v: f32) {
        self.set_gf(OFS_RETURN, v);
    }
    /// Write the builtin return value as an int (`G_INT(OFS_RETURN)`).
    pub fn ret_int(&mut self, v: i32) {
        self.set_gi(OFS_RETURN, v);
    }
    /// Write the builtin return value as a vector (`G_VECTOR(OFS_RETURN)`).
    pub fn ret_vector(&mut self, v: [f32; 3]) {
        self.set_gv(OFS_RETURN, v);
    }
    /// Write the builtin return value as an entity index (`RETURN_EDICT`).
    pub fn ret_entity(&mut self, e: i32) {
        self.set_gi(OFS_RETURN, e);
    }
    /// Write the builtin return value as a `string_t` (`G_INT(OFS_RETURN)`).
    pub fn ret_string(&mut self, s: i32) {
        self.set_gi(OFS_RETURN, s);
    }

    // ----------------------------------------------------------------- strings

    /// Intern a Rust string: the `string_t` (byte offset) of `"s\0"` in the
    /// heap, appended the first time this text is interned and shared after.
    ///
    /// id's C has no such heap: `ED_NewString` allocates on the hunk once per
    /// entity-text value, and `ftos`/`vtos` write one static temp buffer, so a
    /// level never grows it. Appending on every call, the port's heap grew by
    /// every `ftos`, `vtos` and `setmodel` for a level's life; sharing the text
    /// bounds it by the distinct strings. QuakeC compares strings by content
    /// (`EQ_S`/`NE_S`/`NOT_S`) and the heap is never written in place, so a
    /// shared offset reads exactly as a fresh copy would.
    pub fn intern(&mut self, s: &str) -> i32 {
        if let Some(&ofs) = self.interned.get(s) {
            return ofs;
        }
        let ofs = self.strings.len() as i32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        self.interned.insert(s.into(), ofs);
        ofs
    }

    /// Resolve a `string_t` (byte offset) to an owned, NUL-terminated `String`.
    /// Out-of-range offsets yield `""`.
    pub fn get_string(&self, s: i32) -> String {
        self.string(s).to_string()
    }

    /// Borrow the string a `string_t` names (`pr_strings + s`, up to its
    /// NUL; `""` out of range).
    pub fn string(&self, s: i32) -> &str {
        string_in(&self.strings, s)
    }

    // ----------------------------------------------------------------- edicts

    /// Number of edicts currently allocated (free or not).
    pub fn num_edicts(&self) -> usize {
        self.edict_free.len()
    }

    /// Whether edict index `e` is free (unallocated) or out of range. The
    /// out-of-range case returns `true` (treat as free) so callers iterating
    /// `0..num_edicts()` are safe.
    pub fn is_free_edict(&self, e: i32) -> bool {
        if e < 0 {
            return true;
        }
        self.edict_free.get(e as usize).copied().unwrap_or(true)
    }

    /// The edicts in use (not free), the world first, in index order.
    pub fn live_edicts(&self) -> impl Iterator<Item = i32> + '_ {
        self.edict_free.iter().enumerate().filter(|&(_, &free)| !free).map(|(e, _)| e as i32)
    }

    /// Zero every field of edict `e` and mark it not-free (`ED_ClearEdict`).
    fn clear_edict(&mut self, e: usize) {
        let ef = self.entityfields();
        let base = e * ef;
        if let Some(slot) = self.edict_fields.get_mut(base..base + ef) {
            for c in slot {
                *c = 0;
            }
        }
        if let Some(free) = self.edict_free.get_mut(e) {
            *free = false;
        }
        if let Some(st) = self.edict_static.get_mut(e) {
            *st = false;
        }
    }

    /// The first slot `ED_Alloc` may hand out: free, and either freed in the
    /// first two seconds of server time or more than 0.5 s ago ("the first
    /// couple seconds of server time can involve a lot of freeing and
    /// allocating, so relax the replacement policy"; otherwise wait "so the
    /// client doesn't think the entity morphed", which would also draw a trail
    /// from the old entity's spot to the new one). The world is never reused.
    /// (The C also skips the client slots 1..maxclients; this port allocates
    /// its player like any edict, see `Server::player`.)
    fn reusable_slot(&self) -> Option<usize> {
        (1..self.edict_free.len()).find(|&i| {
            let freetime = self.edict_freetime.get(i).copied().unwrap_or(0.0);
            // sv.time - e->freetime: a double minus a float.
            self.edict_free[i] && (freetime < 2.0 || self.sv_time - f64::from(freetime) > 0.5)
        })
    }

    /// `ED_Alloc`: reuse a free edict the replacement policy allows (see
    /// [`Self::reusable_slot`]), or grow the array. The reused or new edict is
    /// cleared and marked not-free (`ED_ClearEdict`). Returns its index.
    pub fn spawn(&mut self) -> i32 {
        if let Some(i) = self.reusable_slot() {
            self.clear_edict(i);
            return i as i32;
        }
        let i = self.edict_free.len();
        let ef = self.entityfields();
        self.edict_fields.resize(self.edict_fields.len() + ef, 0);
        self.edict_free.push(false);
        // Newly grown fields are already zero; mark not-free (already false).
        i as i32
    }

    /// `ED_Alloc` with id's hard [`MAX_EDICTS`] ceiling (the C
    /// `Sys_Error("ED_Alloc: no free edicts")`) — [`Self::max_edicts`] in
    /// 2026, where the `sv_max_edicts` cvar may have raised it past id's 600
    /// (never below: [`Self::set_max_edicts`]). Reuses a slot like
    /// [`Self::spawn`], else grows — but returns `None` once the array is
    /// already at the ceiling, so the QuakeC-reachable `PF_Spawn` surfaces a
    /// `run_error` instead of growing memory without bound on a runaway
    /// `spawn()` loop. Engine-internal spawns (the player, temp entities, the
    /// explosive box) use the infallible [`spawn`](Self::spawn).
    pub fn spawn_checked(&mut self) -> Option<i32> {
        if let Some(i) = self.reusable_slot() {
            self.clear_edict(i);
            return Some(i as i32);
        }
        if self.edict_free.len() >= self.max_edicts {
            return None;
        }
        let i = self.edict_free.len();
        let ef = self.entityfields();
        self.edict_fields.resize(self.edict_fields.len() + ef, 0);
        self.edict_free.push(false);
        Some(i as i32)
    }

    /// The live `ED_Alloc` ceiling ([`Self::spawn_checked`]): id's
    /// [`MAX_EDICTS`] unless [`Self::set_max_edicts`] raised it.
    pub fn max_edicts(&self) -> usize {
        self.max_edicts
    }

    /// Raise (or restore) the `ED_Alloc` ceiling — the `sv_max_edicts` cvar's
    /// engine side, called once per level load (`SV_SpawnServer` sizes
    /// `sv.edicts`; this port's edict storage already grows on demand, so
    /// "sizing" it is just moving this ceiling before
    /// `crate::server::Server::spawn_entities` runs). Clamped to
    /// [`MAX_EDICTS`]..=[`MAX_EDICTS_LIMIT`]: this is a departure that only
    /// ever gives QuakeC more room than id's 600, never less, so Classic
    /// (which never calls this) is untouched either way.
    pub fn set_max_edicts(&mut self, n: usize) {
        self.max_edicts = n.clamp(MAX_EDICTS, MAX_EDICTS_LIMIT);
    }

    /// `ED_Free` (pr_edict.c): mark the edict free and clear exactly the fields
    /// the C clears — `model`, `takedamage`, `modelindex`, `colormap`, `skin`,
    /// `frame`, `origin`, `angles`, `solid` zeroed, `nextthink` = -1 — and
    /// record `freetime = sv.time`. Every other field keeps its value, as in
    /// id's game (QuakeC holding a reference to a removed entity still reads
    /// its `classname`, `health`, ...). The world (edict 0) and out-of-range
    /// indices are left untouched.
    pub fn free_edict(&mut self, e: i32) {
        if e <= 0 || e as usize >= self.edict_free.len() {
            return; // never free the world
        }
        let fo = self.fo;
        for f in [fo.takedamage, fo.modelindex, fo.colormap, fo.skin, fo.frame, fo.solid] {
            self.set_ent_float(e, f, 0.0);
        }
        self.set_ent_int(e, fo.model, 0);
        self.set_ent_vec(e, fo.origin, [0.0; 3]);
        self.set_ent_vec(e, fo.angles, [0.0; 3]);
        self.set_ent_float(e, fo.nextthink, -1.0);
        let e = e as usize;
        self.edict_free[e] = true;
        if let Some(st) = self.edict_static.get_mut(e) {
            *st = false;
        }
        if self.edict_freetime.len() <= e {
            self.edict_freetime.resize(e + 1, 0.0);
        }
        self.edict_freetime[e] = self.sv_time as f32; // ed->freetime = sv.time (a float)
    }

    /// The world leaves edict `e` touched when it was last linked
    /// (`ent->leafnums[0..num_leafs]`); empty for an edict never linked with a
    /// model, or out of range.
    pub fn edict_leafs(&self, e: i32) -> &[u16] {
        usize::try_from(e).ok().and_then(|e| self.edict_leafs.get(e)).map_or(&[], |l| l.leafs())
    }

    /// Store edict `e`'s touched leaves (`SV_LinkEdict`). Negative `e` is ignored.
    pub fn set_edict_leafs(&mut self, e: i32, leafs: EdictLeafs) {
        let Ok(e) = usize::try_from(e) else { return };
        if self.edict_leafs.len() <= e {
            self.edict_leafs.resize(e + 1, EdictLeafs::default());
        }
        self.edict_leafs[e] = leafs;
    }

    /// `EDICT_NUM(e)` as `Host_Loadgame_f` takes a savegame's slot `e`: the
    /// array grows to hold it (the new slots free), then `memset (&ent->v, 0,
    /// …); ent->free = false`. Its other state (leaves, static mark) is left
    /// as the loader found it.
    pub(crate) fn load_edict(&mut self, e: usize) {
        let ef = self.entityfields();
        while self.edict_free.len() <= e {
            self.edict_fields.resize(self.edict_fields.len() + ef, 0);
            self.edict_free.push(true);
        }
        if let Some(slot) = self.edict_fields.get_mut(e * ef..(e + 1) * ef) {
            slot.fill(0);
        }
        self.edict_free[e] = false;
    }

    /// `ent->free = true` and nothing else: what `ED_ParseEdict` does with a
    /// savegame block that has no pairs (its fields are already zero).
    pub(crate) fn mark_edict_free(&mut self, e: usize) {
        if let Some(free) = self.edict_free.get_mut(e) {
            *free = true;
        }
    }

    /// `sv.num_edicts = n`: drop every edict slot from `n` on (a loaded game
    /// has exactly the savegame's slots).
    pub(crate) fn truncate_edicts(&mut self, n: usize) {
        if self.edict_free.len() > n {
            self.edict_free.truncate(n);
            self.edict_fields.truncate(n * self.entityfields());
        }
    }

    /// Whether `makestatic` turned edict `e` into a client static.
    pub fn is_static_edict(&self, e: i32) -> bool {
        usize::try_from(e).ok().and_then(|e| self.edict_static.get(e)).copied().unwrap_or(false)
    }

    /// Mark edict `e` a client static (`PF_makestatic`). The world and negative
    /// indices are ignored.
    pub fn make_static(&mut self, e: i32) {
        let Ok(e) = usize::try_from(e) else { return };
        if e == 0 {
            return;
        }
        if self.edict_static.len() <= e {
            self.edict_static.resize(e + 1, false);
        }
        self.edict_static[e] = true;
    }

    /// Flat cell index for edict `e`, field `ofs`, or `None` if out of range.
    fn edict_cell(&self, e: i32, ofs: usize) -> Option<usize> {
        if e < 0 {
            return None;
        }
        let ef = self.entityfields();
        let idx = (e as usize).checked_mul(ef)?.checked_add(ofs)?;
        if idx < self.edict_fields.len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Read entity field as a float (`E_FLOAT`). Out of range reads as `0.0`.
    pub fn ef(&self, e: i32, ofs: usize) -> f32 {
        match self.edict_cell(e, ofs) {
            Some(i) => f32::from_bits(self.edict_fields[i]),
            None => 0.0,
        }
    }
    /// Write entity field as a float. Out of range is ignored.
    pub fn set_ef(&mut self, e: i32, ofs: usize, v: f32) {
        if let Some(i) = self.edict_cell(e, ofs) {
            self.edict_fields[i] = v.to_bits();
        }
    }
    /// Read entity field as an int (`E_INT`). Out of range reads as `0`.
    pub fn ei(&self, e: i32, ofs: usize) -> i32 {
        match self.edict_cell(e, ofs) {
            Some(i) => self.edict_fields[i] as i32,
            None => 0,
        }
    }
    /// Write entity field as an int. Out of range is ignored.
    pub fn set_ei(&mut self, e: i32, ofs: usize, v: i32) {
        if let Some(i) = self.edict_cell(e, ofs) {
            self.edict_fields[i] = v as u32;
        }
    }
    /// Read entity field as a vector (`E_VECTOR`). Missing cells read as `0.0`.
    pub fn ev(&self, e: i32, ofs: usize) -> [f32; 3] {
        [self.ef(e, ofs), self.ef(e, ofs + 1), self.ef(e, ofs + 2)]
    }
    /// Write entity field as a vector. Out-of-range cells are ignored.
    pub fn set_ev(&mut self, e: i32, ofs: usize, v: [f32; 3]) {
        self.set_ef(e, ofs, v[0]);
        self.set_ef(e, ofs + 1, v[1]);
        self.set_ef(e, ofs + 2, v[2]);
    }

    // ------------------------------------------------------ pointer (STOREP/LOAD)

    /// Write a flat-pointer cell, bounds-checked.
    fn ptr_set(&mut self, ptr: i32, v: u32) -> Result<()> {
        if ptr < 0 {
            return Err(self.run_error(format!("bad pointer {ptr}")));
        }
        let p = ptr as usize;
        let len = self.edict_fields.len();
        match self.edict_fields.get_mut(p) {
            Some(c) => {
                *c = v;
                Ok(())
            }
            None => Err(self.run_error(format!(
                "pointer {ptr} out of range (len {len})"
            ))),
        }
    }

    // --------------------------------------------------------- raw global cells

    /// Read a raw global cell, bounds-checked (for the interpreter inner loop).
    fn cell(&self, ofs: usize) -> Result<u32> {
        self.globals
            .get(ofs)
            .copied()
            .ok_or_else(|| self.run_error(format!("global offset {ofs} out of range")))
    }
    /// Read a raw global cell as a float.
    fn cell_f(&self, ofs: usize) -> Result<f32> {
        Ok(f32::from_bits(self.cell(ofs)?))
    }
    /// Read a raw global cell as an int.
    fn cell_i(&self, ofs: usize) -> Result<i32> {
        Ok(self.cell(ofs)? as i32)
    }
    /// Read a raw global vector (3 cells), bounds-checked on all three.
    fn cell_v(&self, ofs: usize) -> Result<[f32; 3]> {
        Ok([self.cell_f(ofs)?, self.cell_f(ofs + 1)?, self.cell_f(ofs + 2)?])
    }
    /// Write a raw global cell, bounds-checked.
    fn set_cell(&mut self, ofs: usize, v: u32) -> Result<()> {
        let len = self.globals.len();
        match self.globals.get_mut(ofs) {
            Some(c) => {
                *c = v;
                Ok(())
            }
            None => Err(self.run_error(format!(
                "global offset {ofs} out of range (len {len})"
            ))),
        }
    }
    fn set_cell_f(&mut self, ofs: usize, v: f32) -> Result<()> {
        self.set_cell(ofs, v.to_bits())
    }
    fn set_cell_i(&mut self, ofs: usize, v: i32) -> Result<()> {
        self.set_cell(ofs, v as u32)
    }
    fn set_cell_v(&mut self, ofs: usize, v: [f32; 3]) -> Result<()> {
        self.set_cell_f(ofs, v[0])?;
        self.set_cell_f(ofs + 1, v[1])?;
        self.set_cell_f(ofs + 2, v[2])?;
        Ok(())
    }

    // ----------------------------------------------------------------- errors

    /// `PR_RunError`: a runtime error raised at the current statement, as a
    /// [`QError::Program`] carrying id's report (the statement, the stack
    /// trace, the message). Returning it from a builtin or the interpreter
    /// halts the VM ([`Vm::execute`]).
    pub fn run_error(&self, msg: impl Into<String>) -> QError {
        self.program_error(msg.into()).into()
    }

    /// The program error this VM halted on, if any (see [`Vm::execute`]).
    pub fn halted(&self) -> Option<&ProgramError> {
        self.halted.as_ref()
    }

    // ------------------------------------------------------- function frames

    /// `PR_EnterFunction`: save the caller frame, spill the new function's
    /// locals onto the locals stack, copy parameters into `parm_start`, and
    /// return the resume statement index (`first_statement - 1`, to offset the
    /// `s++` at the top of the loop). Faults overflow / bad layout via
    /// `run_error`.
    fn enter_function(&mut self, fnum: usize) -> Result<usize> {
        // Push the caller frame.
        if self.stack.len() + 1 >= MAX_STACK_DEPTH {
            return Err(self.run_error("stack overflow"));
        }
        self.stack.push(Frame {
            s: self.xstatement,
            f: self.xfunction,
        });

        let f = self
            .progs
            .functions
            .get(fnum)
            .ok_or_else(|| self.run_error(format!("bad function number {fnum}")))?
            .clone();

        let parm_start = if f.parm_start < 0 {
            return Err(self.run_error(format!("negative parm_start {}", f.parm_start)));
        } else {
            f.parm_start as usize
        };
        let locals = if f.locals < 0 {
            return Err(self.run_error(format!("negative locals {}", f.locals)));
        } else {
            f.locals as usize
        };

        // Save off any locals that the new function steps on.
        if self.localstack.len() + locals > LOCALSTACK_SIZE {
            return Err(self.run_error("locals stack overflow"));
        }
        for i in 0..locals {
            let v = self.cell(parm_start + i)?;
            self.localstack.push(v);
        }

        // Copy parameters from OFS_PARM0+i*3 into parm_start.
        let mut o = parm_start;
        let numparms = f.numparms.max(0) as usize;
        for i in 0..numparms.min(MAX_PARMS) {
            let psize = *f.parm_size.get(i).unwrap_or(&0) as usize;
            for j in 0..psize {
                let v = self.cell(OFS_PARM0 + i * 3 + j)?;
                self.set_cell(o, v)?;
                o += 1;
            }
        }

        self.xfunction = fnum;
        // first_statement-1 to offset the s++ at the top of the loop. Returned
        // as the resume index; the loop adds 1 before fetching.
        if f.first_statement < 0 {
            // Should never happen: callers check for builtins first.
            return Err(self.run_error("entered a builtin as bytecode"));
        }
        Ok(f.first_statement as usize)
    }

    /// `PR_LeaveFunction`: restore the current function's locals from the locals
    /// stack and pop the caller frame, returning the resume statement index.
    fn leave_function(&mut self) -> Result<usize> {
        let frame = self
            .stack
            .pop()
            .ok_or_else(|| self.run_error("prog stack underflow"))?;

        let f = self
            .progs
            .functions
            .get(self.xfunction)
            .ok_or_else(|| self.run_error("bad current function on leave"))?
            .clone();
        let parm_start = f.parm_start.max(0) as usize;
        let locals = f.locals.max(0) as usize;

        if self.localstack.len() < locals {
            return Err(self.run_error("locals stack underflow"));
        }
        // Restore locals in original order: they were pushed parm_start..+locals.
        let start = self.localstack.len() - locals;
        for i in 0..locals {
            let v = self.localstack[start + i];
            self.set_cell(parm_start + i, v)?;
        }
        self.localstack.truncate(start);

        self.xfunction = frame.f;
        Ok(frame.s)
    }

    // ----------------------------------------------------------- entry points

    /// Call the function named `name`. Errors if no such function exists.
    pub fn call_by_name(&mut self, name: &str) -> Result<()> {
        let fnum = self
            .progs
            .find_function(name)
            .ok_or_else(|| self.run_error(format!("no function named {name:?}")))?;
        self.execute(fnum)
    }

    /// Resume after a program error: empty the call and locals stacks (the C
    /// `PR_RunError` sets `pr_depth = 0`) and lift the halt. The game never
    /// does this — in id's a program error ends it — but a harness that runs
    /// QuakeC past an error (the census) does.
    pub fn reset_execution(&mut self) {
        self.stack.clear();
        self.localstack.clear();
        self.xfunction = 0;
        self.xstatement = 0;
        self.halted = None;
    }

    /// `PR_ExecuteProgram`: run function `fnum` to completion, executing nested
    /// calls and builtins, until it returns past the entry frame.
    ///
    /// A runtime error is id's `PR_RunError`, which prints its report and
    /// longjmps out of every running QuakeC function to `Host_Error`, ending
    /// the game. Here the error (a [`QError::Program`]) is returned and the VM
    /// halts on it: this call unwinds its frames and returns it, so does every
    /// call it is nested in (through the builtin that made it), and every
    /// later call returns it at once, as a shut-down server runs no more
    /// QuakeC. The first error is the one reported.
    pub fn execute(&mut self, fnum: usize) -> Result<()> {
        if let Some(e) = &self.halted {
            return Err(e.clone().into());
        }
        let (depth, locals) = (self.stack.len(), self.localstack.len());
        let Err(e) = self.run(fnum) else { return Ok(()) };
        let e = match e {
            QError::Program(e) => *e,
            other => self.program_error(other.to_string()),
        };
        self.stack.truncate(depth);
        self.localstack.truncate(locals);
        Err(self.halted.get_or_insert(e).clone().into())
    }

    /// [`Vm::execute`]'s interpreter loop (`PR_ExecuteProgram`'s body).
    fn run(&mut self, fnum: usize) -> Result<()> {
        if fnum == 0 || fnum >= self.progs.functions.len() {
            return Err(self.run_error(format!("NULL function (number {fnum})")));
        }

        let mut runaway = RUNAWAY;
        // exitdepth: the call depth we must return below to finish.
        let exitdepth = self.stack.len();

        // Builtins called directly as the entry point would loop forever in the
        // C (it indexes pr_statements[s] starting at a negative s); guard it.
        if let Some(f) = self.progs.functions.get(fnum) {
            if f.builtin().is_some() {
                return Err(self.run_error("cannot execute a builtin as the entry function"));
            }
        }

        // The C loop does `s++` *before* fetching, and `PR_EnterFunction`
        // returns `first_statement - 1` to offset it. `s` is a `usize` here, so
        // rather than store a -1 we keep `s = first_statement` and use a `first`
        // flag to skip the pre-increment on the very first fetch. Branches and
        // calls set `first = true` so their freshly-set `s` is fetched directly,
        // exactly reproducing the C's `s += offset - 1; ... s++` arithmetic.
        let mut s = self.enter_function(fnum)?;
        let mut first = true;

        loop {
            if !first {
                s += 1;
            }
            first = false;

            // Fetch the statement (bounds-checked).
            let st: Statement = *self
                .progs
                .statements
                .get(s)
                .ok_or_else(|| self.run_error(format!("statement index {s} out of range")))?;

            runaway -= 1;
            if runaway == 0 {
                return Err(self.run_error("runaway loop error"));
            }
            self.stmt_count = self.stmt_count.wrapping_add(1);

            // Profile + current statement (for error messages / STATE).
            if let Some(f) = self.progs.functions.get_mut(self.xfunction) {
                f.profile = f.profile.wrapping_add(1);
            }
            self.xstatement = s;

            let a = st.a as usize;
            let b = st.b as usize;
            let c = st.c as usize;

            if self.trace {
                let mn = st.op.mnemonic();
                self.output
                    .push_str(&format!("{s:5}: {mn} a={} b={} c={}\n", st.a, st.b, st.c));
            }

            let op = st.op;
            match op {
                // ------------------------------------------------- arithmetic
                Op::AddF => {
                    let v = self.cell_f(a)? + self.cell_f(b)?;
                    self.set_cell_f(c, v)?;
                }
                Op::AddV => {
                    let (x, y) = (self.cell_v(a)?, self.cell_v(b)?);
                    self.set_cell_v(c, [x[0] + y[0], x[1] + y[1], x[2] + y[2]])?;
                }
                Op::SubF => {
                    let v = self.cell_f(a)? - self.cell_f(b)?;
                    self.set_cell_f(c, v)?;
                }
                Op::SubV => {
                    let (x, y) = (self.cell_v(a)?, self.cell_v(b)?);
                    self.set_cell_v(c, [x[0] - y[0], x[1] - y[1], x[2] - y[2]])?;
                }
                Op::MulF => {
                    let v = self.cell_f(a)? * self.cell_f(b)?;
                    self.set_cell_f(c, v)?;
                }
                Op::MulV => {
                    // dot product -> scalar
                    let (x, y) = (self.cell_v(a)?, self.cell_v(b)?);
                    let v = x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
                    self.set_cell_f(c, v)?;
                }
                Op::MulFV => {
                    let f = self.cell_f(a)?;
                    let y = self.cell_v(b)?;
                    self.set_cell_v(c, [f * y[0], f * y[1], f * y[2]])?;
                }
                Op::MulVF => {
                    let x = self.cell_v(a)?;
                    let f = self.cell_f(b)?;
                    self.set_cell_v(c, [f * x[0], f * x[1], f * x[2]])?;
                }
                Op::DivF => {
                    // C divides with IEEE semantics (no trap); replicate exactly.
                    let v = self.cell_f(a)? / self.cell_f(b)?;
                    self.set_cell_f(c, v)?;
                }
                Op::BitAnd => {
                    let v = (self.cell_f(a)? as i32) & (self.cell_f(b)? as i32);
                    self.set_cell_f(c, v as f32)?;
                }
                Op::BitOr => {
                    let v = (self.cell_f(a)? as i32) | (self.cell_f(b)? as i32);
                    self.set_cell_f(c, v as f32)?;
                }

                // ------------------------------------------------ comparisons
                Op::Ge => {
                    let v = (self.cell_f(a)? >= self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::Le => {
                    let v = (self.cell_f(a)? <= self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::Gt => {
                    let v = (self.cell_f(a)? > self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::Lt => {
                    let v = (self.cell_f(a)? < self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::And => {
                    // C: a->_float && b->_float
                    let v = ((self.cell_f(a)? != 0.0) && (self.cell_f(b)? != 0.0)) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::Or => {
                    let v = ((self.cell_f(a)? != 0.0) || (self.cell_f(b)? != 0.0)) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }

                // ------------------------------------------------------- NOTs
                Op::NotF => {
                    let v = (self.cell_f(a)? == 0.0) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NotV => {
                    let x = self.cell_v(a)?;
                    let v = (x[0] == 0.0 && x[1] == 0.0 && x[2] == 0.0) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NotS => {
                    // !a->string || !pr_strings[a->string]: empty string_t.
                    // Borrow the string (string_in returns "" for 0/out-of-range)
                    // instead of allocating an owned String just to test emptiness.
                    let s_t = self.cell_i(a)?;
                    let empty = string_in(&self.strings, s_t).is_empty();
                    self.set_cell_f(c, empty as i32 as f32)?;
                }
                Op::NotFnc => {
                    let v = (self.cell_i(a)? == 0) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NotEnt => {
                    // true iff entity == world (edict 0)
                    let v = (self.cell_i(a)? == 0) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }

                // -------------------------------------------------- equality
                Op::EqF => {
                    let v = (self.cell_f(a)? == self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::EqV => {
                    let (x, y) = (self.cell_v(a)?, self.cell_v(b)?);
                    let v = (x[0] == y[0] && x[1] == y[1] && x[2] == y[2]) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::EqS => {
                    // Compare borrowed &str (no owned-String allocation per op).
                    let (ai, bi) = (self.cell_i(a)?, self.cell_i(b)?);
                    let eq = string_in(&self.strings, ai) == string_in(&self.strings, bi);
                    self.set_cell_f(c, eq as i32 as f32)?;
                }
                Op::EqE => {
                    let v = (self.cell_i(a)? == self.cell_i(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::EqFnc => {
                    let v = (self.cell_i(a)? == self.cell_i(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }

                // ------------------------------------------------ inequality
                Op::NeF => {
                    let v = (self.cell_f(a)? != self.cell_f(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NeV => {
                    let (x, y) = (self.cell_v(a)?, self.cell_v(b)?);
                    let v = (x[0] != y[0] || x[1] != y[1] || x[2] != y[2]) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NeS => {
                    let (ai, bi) = (self.cell_i(a)?, self.cell_i(b)?);
                    let ne = string_in(&self.strings, ai) != string_in(&self.strings, bi);
                    self.set_cell_f(c, ne as i32 as f32)?;
                }
                Op::NeE => {
                    let v = (self.cell_i(a)? != self.cell_i(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }
                Op::NeFnc => {
                    let v = (self.cell_i(a)? != self.cell_i(b)?) as i32 as f32;
                    self.set_cell_f(c, v)?;
                }

                // ----------------------------------------------- STORE_* (b=slot)
                Op::StoreF | Op::StoreEnt | Op::StoreFld | Op::StoreS | Op::StoreFnc => {
                    // copy a's single cell into b (b is a global slot).
                    let v = self.cell(a)?;
                    self.set_cell(b, v)?;
                }
                Op::StoreV => {
                    let v = self.cell_v(a)?;
                    self.set_cell_v(b, v)?;
                }

                // ---------------------------------------------- STOREP_* (b=ptr)
                Op::StorepF | Op::StorepEnt | Op::StorepFld | Op::StorepS | Op::StorepFnc => {
                    let ptr = self.cell_i(b)?;
                    let v = self.cell(a)?;
                    self.ptr_set(ptr, v)?;
                }
                Op::StorepV => {
                    let ptr = self.cell_i(b)?;
                    let x = self.cell(a)?;
                    let y = self.cell(a + 1)?;
                    let z = self.cell(a + 2)?;
                    // checked_add so a pathological ptr near i32::MAX can't panic
                    // (ptr_set would reject it anyway, but stay panic-free here too).
                    let p1 = ptr.checked_add(1).ok_or_else(|| self.run_error("STOREP_V pointer overflow"))?;
                    let p2 = ptr.checked_add(2).ok_or_else(|| self.run_error("STOREP_V pointer overflow"))?;
                    self.ptr_set(ptr, x)?;
                    self.ptr_set(p1, y)?;
                    self.ptr_set(p2, z)?;
                }

                // ------------------------------------------------------ ADDRESS
                Op::Address => {
                    let ent = self.cell_i(a)?;
                    let field = self.cell_i(b)?;
                    if ent < 0 || field < 0 {
                        return Err(self.run_error(format!(
                            "ADDRESS with bad ent={ent} field={field}"
                        )));
                    }
                    let ef = self.entityfields();
                    let flat = (ent as usize)
                        .checked_mul(ef)
                        .and_then(|x| x.checked_add(field as usize))
                        .ok_or_else(|| self.run_error("ADDRESS overflow"))?;
                    // It is legal to compute pointers; the bounds check happens
                    // at the eventual STOREP. But guard obvious overflow above.
                    self.set_cell_i(c, flat as i32)?;
                }

                // ----------------------------------------------- LOAD_* (a=ent,b=field)
                Op::LoadF | Op::LoadFld | Op::LoadEnt | Op::LoadS | Op::LoadFnc => {
                    let ent = self.cell_i(a)?;
                    let field = self.cell_i(b)?;
                    if field < 0 {
                        return Err(self.run_error(format!("LOAD bad field {field}")));
                    }
                    let cell = self
                        .edict_cell(ent, field as usize)
                        .ok_or_else(|| self.run_error(format!(
                            "LOAD ent={ent} field={field} out of range"
                        )))?;
                    let v = self.edict_fields[cell];
                    self.set_cell(c, v)?;
                }
                Op::LoadV => {
                    let ent = self.cell_i(a)?;
                    let field = self.cell_i(b)?;
                    if field < 0 {
                        return Err(self.run_error(format!("LOAD_V bad field {field}")));
                    }
                    let base = self
                        .edict_cell(ent, field as usize)
                        .ok_or_else(|| self.run_error(format!(
                            "LOAD_V ent={ent} field={field} out of range"
                        )))?;
                    // Bounds-check all three cells.
                    let x = *self.edict_fields.get(base).ok_or_else(|| self.run_error("LOAD_V oob"))?;
                    let y = *self.edict_fields.get(base + 1).ok_or_else(|| self.run_error("LOAD_V oob"))?;
                    let z = *self.edict_fields.get(base + 2).ok_or_else(|| self.run_error("LOAD_V oob"))?;
                    self.set_cell(c, x)?;
                    self.set_cell(c + 1, y)?;
                    self.set_cell(c + 2, z)?;
                }

                // ----------------------------------------------------- branches
                Op::Ifnot => {
                    if self.cell_i(a)? == 0 {
                        s = self.jump(s, st.b);
                        // jump returns the target itself (the C's `s += b - 1`
                        // then `s++`); set `first` so the loop does NOT
                        // pre-increment again.
                        first = true;
                    }
                }
                Op::If => {
                    if self.cell_i(a)? != 0 {
                        s = self.jump(s, st.b);
                        first = true;
                    }
                }
                Op::Goto => {
                    s = self.jump(s, st.a);
                    first = true;
                }

                // -------------------------------------------------------- calls
                Op::Call0
                | Op::Call1
                | Op::Call2
                | Op::Call3
                | Op::Call4
                | Op::Call5
                | Op::Call6
                | Op::Call7
                | Op::Call8 => {
                    self.argc = op.call_argc().unwrap_or(0);
                    let func = self.cell_i(a)?;
                    if func == 0 {
                        return Err(self.run_error("NULL function"));
                    }
                    if func < 0 {
                        return Err(self.run_error(format!("bad function number {func}")));
                    }
                    let fnum2 = func as usize;
                    let newf = self
                        .progs
                        .functions
                        .get(fnum2)
                        .ok_or_else(|| self.run_error(format!("bad function number {func}")))?;
                    if let Some(bi) = newf.builtin() {
                        // negative first_statement => builtin number `bi`
                        if bi == 0 || bi >= self.builtins.len() {
                            return Err(self.run_error(format!("bad builtin call number {bi}")));
                        }
                        let f = self.builtins[bi];
                        f(self)?;
                        // QuakeC the builtin ran (a touch a walkmove fired)
                        // failed: id's longjmp leaves this function too.
                        if let Some(e) = &self.halted {
                            return Err(e.clone().into());
                        }
                        // builtin returns; continue with s++ as normal.
                    } else {
                        s = self.enter_function(fnum2)?;
                        first = true; // resume at first_statement (no pre-inc)
                    }
                }

                // --------------------------------------------------- done/return
                Op::Done | Op::Return => {
                    let x = self.cell(a)?;
                    let y = self.cell(a + 1)?;
                    let z = self.cell(a + 2)?;
                    self.set_cell(OFS_RETURN, x)?;
                    self.set_cell(OFS_RETURN + 1, y)?;
                    self.set_cell(OFS_RETURN + 2, z)?;

                    s = self.leave_function()?;
                    if self.stack.len() == exitdepth {
                        return Ok(()); // all done
                    }
                    // resume in the caller at the saved s, then the loop's s++
                    // advances past the CALL. So leave 'first' false.
                }

                // --------------------------------------------------------- STATE
                Op::State => {
                    self.do_state(st)?;
                }

                Op::Invalid(code) => return Err(self.run_error(format!("bad opcode {code}"))),
            }
        }
    }

    /// The statement a branch at `s` lands on. The C does `s += offset - 1`
    /// and then `s++` at the top of the loop; this returns the target `s +
    /// offset` itself (the caller sets `first` so the loop's pre-increment is
    /// skipped). A target before statement 0 becomes one past the end, which
    /// the next fetch reports as a clean `run_error`.
    fn jump(&self, s: usize, offset: i16) -> usize {
        s.checked_add_signed(isize::from(offset)).unwrap_or(self.progs.statements.len())
    }

    /// `OP_STATE`: set `self.nextthink = time + 0.1`, `self.frame = a`,
    /// `self.think = b`. Best-effort: if any of the well-known globals/fields is
    /// missing from this program, treat STATE as a no-op (the C unconditionally
    /// dereferences them).
    fn do_state(&mut self, st: Statement) -> Result<()> {
        let (fo, go) = (&self.fo, &self.go);
        let (Some(g_self), Some(g_time), Some(f_frame), Some(f_think), Some(f_nextthink)) =
            (go.self_.ofs(), go.time.ofs(), fo.frame.ofs(), fo.think.ofs(), fo.nextthink.ofs())
        else {
            return Ok(()); // missing defs: no-op
        };

        let ent = self.gi(g_self);
        let time = self.gf(g_time);
        let frame = self.cell_f(st.a as usize)?;
        let think = self.cell_i(st.b as usize)?;

        self.set_ef(ent, f_nextthink, time + 0.1);
        self.set_ef(ent, f_frame, frame);
        self.set_ei(ent, f_think, think);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Def, EType, Function, OFS_NULL, RESERVED_OFS};

    // ---- progs.dat image builder (mirrors progs.rs's test serializer) ----

    const HEADER_SIZE: usize = 60;

    fn ser_stmt(s: &Statement) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&s.op.code().to_le_bytes());
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

    /// Assemble a full progs image from parts.
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
                strings: vec![0u8], // string 0 = ""
                statements: Vec::new(),
                globaldefs: Vec::new(),
                fielddefs: Vec::new(),
                functions: vec![Function {
                    // function 0 is the null function
                    first_statement: 0,
                    parm_start: 0,
                    locals: 0,
                    profile: 0,
                    s_name: 0,
                    s_file: 0,
                    numparms: 0,
                    parm_size: [0; 8],
                }],
                nglobals: 64,
                entityfields: 0,
            }
        }
        fn intern(&mut self, s: &str) -> i32 {
            let ofs = self.strings.len() as i32;
            self.strings.extend_from_slice(s.as_bytes());
            self.strings.push(0);
            ofs
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
                crate::progs::PROG_VERSION,
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

    /// A simple bytecode function with statements starting after fn 0.
    fn add_function(b: &mut Builder, name: &str, stmts: Vec<Statement>) -> usize {
        let first = b.statements.len() as i32;
        let s_name = b.intern(name);
        b.statements.extend(stmts);
        b.functions.push(Function {
            first_statement: first,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        });
        b.functions.len() - 1
    }

    // ----------------------------------------------------------------- tests

    #[test]
    fn test_arithmetic_add_sub_mul() {
        // Lay out globals: g10 = 3.0, g11 = 4.0, results in g12,g13,g14.
        let mut b = Builder::new();
        let g_a = 10usize;
        let g_b = 11usize;
        let g_add = 12usize;
        let g_sub = 13usize;
        let g_mul = 14usize;
        let stmts = vec![
            Statement { op: Op::AddF, a: g_a as i16, b: g_b as i16, c: g_add as i16 },
            Statement { op: Op::SubF, a: g_a as i16, b: g_b as i16, c: g_sub as i16 },
            Statement { op: Op::MulF, a: g_a as i16, b: g_b as i16, c: g_mul as i16 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gf(g_a, 3.0);
        vm.set_gf(g_b, 4.0);
        vm.execute(main).expect("execute");
        assert_eq!(vm.gf(g_add), 7.0);
        assert_eq!(vm.gf(g_sub), -1.0);
        assert_eq!(vm.gf(g_mul), 12.0);
    }

    #[test]
    fn test_if_goto_sum_loop() {
        // sum = 0; i = 1; while (i <= 5) { sum += i; i += 1; } -> sum == 15
        // globals:
        //   g_sum=10, g_i=11, g_one=12 (=1), g_five=13 (=5), g_cmp=14 (temp)
        let mut b = Builder::new();
        let (g_sum, g_i, g_one, g_five, g_cmp) = (10usize, 11usize, 12usize, 13usize, 14usize);
        // statements (indices are absolute since fn0 has none, this fn is first):
        // 0: cmp = (i <= five)      LE  a=i b=five c=cmp
        // 1: if (!cmp) goto +4 -> done   IFNOT a=cmp b=+4
        // 2: sum = sum + i          ADD_F a=sum b=i c=sum
        // 3: i = i + one            ADD_F a=i b=one c=i
        // 4: goto -4 (back to 0)    GOTO a=-4
        // 5: done                   DONE
        let stmts = vec![
            Statement { op: Op::Le, a: g_i as i16, b: g_five as i16, c: g_cmp as i16 },
            Statement { op: Op::Ifnot, a: g_cmp as i16, b: 4, c: 0 },
            Statement { op: Op::AddF, a: g_sum as i16, b: g_i as i16, c: g_sum as i16 },
            Statement { op: Op::AddF, a: g_i as i16, b: g_one as i16, c: g_i as i16 },
            Statement { op: Op::Goto, a: -4, b: 0, c: 0 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gf(g_sum, 0.0);
        vm.set_gf(g_i, 1.0);
        vm.set_gf(g_one, 1.0);
        vm.set_gf(g_five, 5.0);
        vm.execute(main).expect("execute");
        assert_eq!(vm.gf(g_sum), 15.0, "1+2+3+4+5 should be 15");
        assert_eq!(vm.gf(g_i), 6.0);
    }

    #[test]
    fn test_call_function_returns_value() {
        // main: parm0 = 21; call double(); store ret to g_result; done
        // double(): a0 = parm_start cell; ADD_F a0+a0 -> RETURN; done
        let mut b = Builder::new();
        let g_result = 40usize;

        // Build "dbl" first so we know its index. It reads its parameter from
        // its parm_start (RESERVED_OFS=28) and returns param+param.
        // parm_start for dbl = RESERVED_OFS (28). numparms=1, parm_size[0]=1.
        let dbl_first = b.statements.len() as i32;
        let dbl_name = b.intern("dbl");
        b.statements.push(Statement {
            op: Op::AddF,
            a: RESERVED_OFS as i16,
            b: RESERVED_OFS as i16,
            c: OFS_RETURN as i16,
        });
        b.statements.push(Statement { op: Op::Done, a: OFS_RETURN as i16, b: 0, c: 0 });
        b.functions.push(Function {
            first_statement: dbl_first,
            parm_start: RESERVED_OFS as i32,
            locals: 1,
            profile: 0,
            s_name: dbl_name,
            s_file: 0,
            numparms: 1,
            parm_size: { let mut p = [0u8; 8]; p[0] = 1; p },
        });
        let dbl_idx = b.functions.len() - 1;

        // main: store the constant into OFS_PARM0, store dbl's func number into
        // a global slot, CALL1 it, copy RETURN -> g_result, done.
        // We need a global holding the function number for the CALL operand.
        let g_input = 41usize; // holds 21.0
        let g_func = 42usize; // holds dbl_idx as an int
        let main_first = b.statements.len() as i32;
        let main_name = b.intern("main");
        b.statements.push(Statement {
            // STORE_F g_input -> OFS_PARM0
            op: Op::StoreF,
            a: g_input as i16,
            b: OFS_PARM0 as i16,
            c: 0,
        });
        b.statements.push(Statement {
            // CALL1 with function operand at g_func
            op: Op::Call1,
            a: g_func as i16,
            b: 0,
            c: 0,
        });
        b.statements.push(Statement {
            // STORE_F OFS_RETURN -> g_result
            op: Op::StoreF,
            a: OFS_RETURN as i16,
            b: g_result as i16,
            c: 0,
        });
        b.statements.push(Statement { op: Op::Done, a: 0, b: 0, c: 0 });
        b.functions.push(Function {
            first_statement: main_first,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name: main_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        });
        let main_idx = b.functions.len() - 1;

        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gf(g_input, 21.0);
        vm.set_gi(g_func, dbl_idx as i32);
        vm.execute(main_idx).expect("execute");
        assert_eq!(vm.gf(g_result), 42.0, "double(21) == 42");
        // sanity: the function index resolved
        assert_eq!(vm.progs.find_function("dbl"), Some(dbl_idx));
        let _ = OFS_NULL;
    }

    /// `PR_RunError("Bad builtin call number")`: a function whose
    /// `first_statement` names a builtin number past the table — as the
    /// mission packs' re-release `progs.dat` declares for `finaleFinished`
    /// (#79) and `localsound` (#80), neither ever called — loads fine and
    /// runs fine until something actually calls it; only then does the VM
    /// error, matching id's lazy behaviour (`common.rs`'s doc, which no
    /// longer scans for this eagerly).
    #[test]
    fn calling_an_unknown_builtin_errors_at_the_call_not_at_load() {
        let mut b = Builder::new();
        // "foreign": builtin #79, one past the port's 79-entry table
        // (0..=78) — declared here, but this test never calls it.
        let foreign_name = b.intern("foreign");
        b.functions.push(Function {
            first_statement: -79,
            parm_start: 0,
            locals: 0,
            profile: 0,
            s_name: foreign_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        });
        let foreign_idx = b.functions.len() - 1;

        // main: CALL0 through a global holding "foreign"'s index.
        let g_func = 40usize;
        let main_first = b.statements.len() as i32;
        let main_name = b.intern("main");
        b.statements.push(Statement { op: Op::Call0, a: g_func as i16, b: 0, c: 0 });
        b.statements.push(Statement { op: Op::Done, a: 0, b: 0, c: 0 });
        b.functions.push(Function {
            first_statement: main_first,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name: main_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        });
        let main_idx = b.functions.len() - 1;

        let img = b.build();
        // Loading never inspects a function it does not run: the
        // declaration alone, with nothing calling it, is not an error.
        let mut vm = Vm::load(&img).expect("a declared, uncalled foreign builtin loads fine");
        vm.set_gi(g_func, foreign_idx as i32);
        let err = vm.execute(main_idx).unwrap_err();
        assert!(err.to_string().contains("bad builtin call number 79"), "{err}");
    }

    /// D2: the handles in `fo`/`go` are the by-name lookups done once at load —
    /// same cells, and a field the progs lacks reads 0 and drops writes.
    #[test]
    fn resolved_fields_match_the_by_name_accessors() {
        let mut b = Builder::new();
        b.entityfields = 6;
        for (name, type_, ofs) in [("solid", 2u16, 0u16), ("origin", 3, 1), ("model", 1, 4)] {
            let s_name = b.intern(name);
            b.fielddefs.push(Def { type_, ofs, s_name });
        }
        for (name, ofs) in [("self", 30u16), ("time", 31)] {
            let s_name = b.intern(name);
            b.globaldefs.push(Def { type_: 2, ofs, s_name });
        }
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        assert_eq!(vm.fo().origin, vm.fld("origin"));
        assert_eq!(vm.fo().origin.ofs(), Some(1));
        assert_eq!(vm.fo().health, Fld::default(), "a field this progs lacks");
        assert_eq!(vm.go().time.ofs(), Some(31));
        assert_eq!(vm.go().force_retouch, Glb::default());

        let e = vm.spawn();
        vm.set_ent_vec(e, vm.fo().origin, [1.0, 2.0, 3.0]);
        vm.ent_set_float(e, "solid", 4.0);
        vm.ent_set_string(e, "model", "progs/player.mdl");
        assert_eq!(vm.ent_get_vector(e, "origin"), [1.0, 2.0, 3.0]);
        assert_eq!(vm.ent_float(e, vm.fo().solid), 4.0);
        assert_eq!(vm.ent_str(e, vm.fo().model), "progs/player.mdl");
        let cells = vm.edict_fields.clone();
        vm.set_ent_float(e, vm.fo().health, 9.0);
        vm.set_ent_vec(e, vm.fo().velocity, [9.0; 3]);
        assert_eq!(vm.edict_fields, cells, "writes to a missing field are dropped");
        assert_eq!(vm.ent_float(e, vm.fo().health), 0.0);
        assert_eq!(vm.ent_str(e, vm.fo().classname), "");
        vm.set_glob_float(vm.go().time, 2.5);
        assert_eq!(vm.gget_float("time"), 2.5);
        assert_eq!(vm.glob_float(vm.go().force_retouch), 0.0);
    }

    /// `GlobalOfs::parms` is `parm1`..`parm16` in order, and names that are
    /// not Rust fields (`self`, `StartFrame`) resolve by their QuakeC name.
    #[test]
    fn global_handles_resolve_parms_in_order_and_qc_names() {
        let mut b = Builder::new();
        for i in 0..16u16 {
            let s_name = b.intern(&format!("parm{}", i + 1));
            b.globaldefs.push(Def { type_: 2, ofs: 40 + i, s_name });
        }
        for (name, ofs) in [("self", 30u16), ("StartFrame", 31)] {
            let s_name = b.intern(name);
            b.globaldefs.push(Def { type_: 6, ofs, s_name });
        }
        let vm = Vm::load(&b.build()).expect("load");
        let parms: Vec<_> = vm.go().parms().iter().map(|g| g.ofs()).collect();
        assert_eq!(parms, (40..56).map(Some).collect::<Vec<_>>());
        assert_eq!((vm.go().self_.ofs(), vm.go().start_frame.ofs()), (Some(30), Some(31)));
    }

    #[test]
    fn test_edict_store_load_via_address_and_storep() {
        // entityfields = 4. main:
        //   ADDRESS (ent=g_ent, field=g_field) -> g_ptr
        //   STOREP_F g_val -> [g_ptr]
        //   LOAD_F (ent=g_ent, field=g_field) -> g_loaded
        //   done
        let mut b = Builder::new();
        b.entityfields = 4;
        let (g_ent, g_field, g_ptr, g_val, g_loaded) = (10usize, 11usize, 12usize, 13usize, 14usize);
        let stmts = vec![
            Statement { op: Op::Address, a: g_ent as i16, b: g_field as i16, c: g_ptr as i16 },
            Statement { op: Op::StorepF, a: g_val as i16, b: g_ptr as i16, c: 0 },
            Statement { op: Op::LoadF, a: g_ent as i16, b: g_field as i16, c: g_loaded as i16 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");

        // Spawn an entity (index 1) so we have somewhere to store.
        let ent = vm.spawn();
        assert_eq!(ent, 1);
        assert_eq!(vm.entityfields(), 4);

        vm.set_gi(g_ent, ent);
        vm.set_gi(g_field, 2); // field offset 2 within the entity
        vm.set_gf(g_val, 99.5);
        vm.execute(main).expect("execute");

        // The pointer must equal ent*entityfields + field = 1*4 + 2 = 6.
        assert_eq!(vm.gi(g_ptr), 6);
        // The LOAD_F should have read back the stored value.
        assert_eq!(vm.gf(g_loaded), 99.5);

        // Direct ef/set_ef agree with the bytecode-stored value.
        assert_eq!(vm.ef(ent, 2), 99.5);
        vm.set_ef(ent, 2, 7.25);
        assert_eq!(vm.ef(ent, 2), 7.25);
        assert_eq!(vm.ei(ent, 2), 7.25f32.to_bits() as i32);
    }

    #[test]
    fn test_spawn_free_reuse() {
        let mut b = Builder::new();
        b.entityfields = 3;
        // No statements needed; just exercise the edict runtime.
        let _main = add_function(&mut b, "main", vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");

        assert_eq!(vm.num_edicts(), 1); // world
        let e1 = vm.spawn();
        let e2 = vm.spawn();
        assert_eq!((e1, e2), (1, 2));
        vm.set_ef(e1, 0, 5.0);
        vm.free_edict(e1);
        // Reuse should hand back index 1 and have zeroed its fields.
        let e3 = vm.spawn();
        assert_eq!(e3, 1);
        assert_eq!(vm.ef(e3, 0), 0.0, "reused edict must be cleared");
        // Freeing the world is a no-op.
        vm.free_edict(0);
        assert!(!vm.edict_free[0]);
    }

    #[test]
    fn set_max_edicts_raises_or_restores_the_ed_alloc_ceiling() {
        // The 2026-only sv_max_edicts extra: spawn_checked (PF_Spawn) holds at
        // id's MAX_EDICTS until raised, then holds at the raised ceiling too —
        // this is what fixes Rogue's r2m6 ("ED_Alloc: no free edicts" past 600
        // edicts; AUDIT.md "the mission packs").
        let mut b = Builder::new();
        b.entityfields = 1;
        let _main = add_function(&mut b, "main", vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");

        assert_eq!(vm.max_edicts(), MAX_EDICTS, "id's 600 until something raises it");

        // Nothing is ever freed here, so spawn_checked always grows (never
        // reuses) until the ceiling — world (edict 0) plus MAX_EDICTS-1 more.
        for _ in 1..MAX_EDICTS {
            assert!(vm.spawn_checked().is_some());
        }
        assert_eq!(vm.num_edicts(), MAX_EDICTS);
        assert!(vm.spawn_checked().is_none(), "id's own ceiling: ED_Alloc: no free edicts");

        // Raised past 600 (the extra, on): the same VM keeps allocating up to
        // the new ceiling, then holds there too — never unbounded.
        vm.set_max_edicts(700);
        assert_eq!(vm.max_edicts(), 700);
        for _ in MAX_EDICTS..700 {
            assert!(vm.spawn_checked().is_some());
        }
        assert_eq!(vm.num_edicts(), 700);
        assert!(vm.spawn_checked().is_none(), "the raised ceiling holds just as id's did");

        // Clamped both ways: never below id's 600 (a departure only ever
        // gives QuakeC more room, never less — Classic, which never calls
        // this, is untouched either way) and never past the cvar's own limit.
        vm.set_max_edicts(0);
        assert_eq!(vm.max_edicts(), MAX_EDICTS, "never below id's own ceiling");
        vm.set_max_edicts(usize::MAX);
        assert_eq!(vm.max_edicts(), MAX_EDICTS_LIMIT, "never past the cvar's own ceiling");
    }

    #[test]
    fn test_runaway_loop_errors_not_panics() {
        // goto self forever -> runaway loop guard fires as an Err.
        let mut b = Builder::new();
        let stmts = vec![Statement { op: Op::Goto, a: 0, b: 0, c: 0 }];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let err = vm.execute(main).unwrap_err();
        match err {
            QError::Program(e) => assert_eq!(e.message, "runaway loop error"),
            other => panic!("expected a program error, got {other:?}"),
        }
    }

    #[test]
    fn a_program_error_is_pr_run_errors_report_and_halts_the_vm() {
        // main calls helper through global 30; helper calls the function in
        // global 31, which holds 0: PR_RunError ("NULL function").
        let mut b = Builder::new();
        let (g, f, file) = (b.intern("g"), b.intern("f"), b.intern("demo.qc"));
        b.globaldefs.push(Def { type_: EType::Function as u16, ofs: 30, s_name: g });
        b.globaldefs.push(Def { type_: EType::Function as u16, ofs: 31, s_name: f });
        let call = |a| Statement { op: Op::Call0, a, b: 0, c: 0 };
        let done = Statement { op: Op::Done, a: 0, b: 0, c: 0 };
        let main = add_function(&mut b, "main", vec![call(30), done]);
        let helper = add_function(&mut b, "helper", vec![call(31), done]);
        let ok = add_function(&mut b, "ok", vec![done]);
        for i in [main, helper] {
            b.functions[i].s_file = file;
        }
        let mut vm = Vm::load(&b.build()).expect("load");
        vm.set_gi(30, helper as i32);

        let Err(QError::Program(e)) = vm.execute(main) else { panic!("NULL function must fail") };
        // PR_PrintStatement (the opcode to 10 columns and a space, each operand
        // `ofs(name)value` to 20 and a space), PR_StackTrace (`%12s : %s`,
        // innermost first, down to the entry frame's <NO FUNCTION>), the message.
        assert_eq!(
            e.console,
            "CALL0      31(f)()              \n     demo.qc : helper\n     demo.qc : main\n<NO FUNCTION>\nNULL function\n"
        );
        assert_eq!((e.function.as_str(), e.message.as_str()), ("helper", "NULL function"));
        // Halted: id's game is over, so nothing runs, until a harness resumes.
        let Err(QError::Program(again)) = vm.execute(ok) else { panic!("a halted VM runs nothing") };
        assert_eq!(again.message, "NULL function", "the first error is the one reported");
        assert_eq!(vm.halted().map(|e| e.message.as_str()), Some("NULL function"));
        vm.reset_execution();
        vm.execute(ok).expect("resumed");
        assert!(vm.halted().is_none());
    }

    #[test]
    fn test_bad_opcode_errors() {
        let mut b = Builder::new();
        // opcode 9999 is invalid.
        let stmts = vec![Statement { op: Op::Invalid(9999), a: 0, b: 0, c: 0 }];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let err = vm.execute(main).unwrap_err();
        assert!(matches!(err, QError::Program(e) if e.message == "bad opcode 9999"));
    }

    /// The opcodes are decoded at load, but a bad one faults only when it
    /// runs (id's `default:` arm): a program that jumps over it is fine.
    #[test]
    fn a_bad_opcode_that_never_runs_is_harmless() {
        let mut b = Builder::new();
        let stmts = vec![
            Statement { op: Op::Goto, a: 2, b: 0, c: 0 },
            Statement { op: Op::Invalid(9999), a: 0, b: 0, c: 0 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let mut vm = Vm::load(&b.build()).expect("a bad opcode loads");
        assert_eq!(vm.progs.statements[1].op, Op::Invalid(9999));
        vm.execute(main).expect("jumped over");
    }

    #[test]
    fn test_storep_out_of_range_errors() {
        // A STOREP through a pointer past the edict array must error, not panic.
        let mut b = Builder::new();
        b.entityfields = 2;
        let (g_ptr, g_val) = (10usize, 11usize);
        let stmts = vec![
            Statement { op: Op::StorepF, a: g_val as i16, b: g_ptr as i16, c: 0 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gi(g_ptr, 999_999); // way out of range
        vm.set_gf(g_val, 1.0);
        assert!(vm.execute(main).is_err());
    }

    #[test]
    fn test_string_intern_and_get() {
        let mut b = Builder::new();
        let _ = add_function(&mut b, "main", vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let s = vm.intern("hello");
        assert_eq!(vm.get_string(s), "hello");
        // The same text again is the same string_t; the heap does not grow.
        let len = vm.strings.len();
        assert_eq!(vm.intern("hello"), s);
        assert_eq!(vm.strings.len(), len);
        assert_ne!(vm.intern("hello2"), s);
        // out-of-range string_t -> empty
        assert_eq!(vm.get_string(-5), "");
        assert_eq!(vm.get_string(1_000_000), "");
    }

    #[test]
    fn test_eq_s_compares_string_contents() {
        let mut b = Builder::new();
        let progs_foo = b.intern("foo"); // the progs' own "foo"
        let (g_a, g_b, g_c) = (10usize, 11usize, 12usize);
        let stmts = vec![
            Statement { op: Op::EqS, a: g_a as i16, b: g_b as i16, c: g_c as i16 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let (sa, sb) = (vm.intern("foo"), progs_foo);
        assert_ne!(sa, sb, "distinct offsets, same contents");
        vm.set_gi(g_a, sa);
        vm.set_gi(g_b, sb);
        vm.execute(main).expect("execute");
        assert_eq!(vm.gf(g_c), 1.0, "EQ_S compares contents, not offsets");
        // make them differ
        let sd = vm.intern("bar");
        vm.set_gi(g_b, sd);
        vm.execute(main).expect("execute");
        assert_eq!(vm.gf(g_c), 0.0);
    }

    #[test]
    fn test_type_cells() {
        assert_eq!(EType::Vector.cells(), 3);
        assert_eq!(EType::Float.cells(), 1);
    }
}
