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
//!   [`Vm::intern`] appends `"s\0"` and returns the byte offset (a `string_t`);
//!   [`Vm::get_string`] reads a NUL-terminated string from an offset.

use crate::error::{QError, Result};
use crate::math::Vec3;
use crate::progs::{string_in, Op, Progs, Statement, MAX_PARMS, OFS_PARM0, OFS_RETURN};

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
pub const MAX_EDICTS: usize = 600;

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
pub struct Vm {
    pub progs: Progs,
    /// The mutable global block, one cell per `u32`.
    pub globals: Vec<u32>,
    /// Flat edict field storage: `num_edicts * entityfields` cells.
    pub edict_fields: Vec<u32>,
    /// Free flag per edict (parallel to the edict array).
    pub edict_free: Vec<bool>,
    /// The string heap (`pr_strings`), initialised from `progs.strings`.
    pub strings: Vec<u8>,
    /// Native builtin table; index 0 is reserved (`PF_Fixme`).
    pub builtins: Vec<Builtin>,
    /// Captured console output (`print`/`dprint`/`bprint` append here).
    pub output: String,
    /// `pr_argc` — number of arguments to the builtin currently running.
    pub argc: usize,
    /// When set, the interpreter records a per-statement trace into `output`.
    pub trace: bool,
    /// Optional engine host providing world services to the engine builtins.
    /// Taken out and restored around each use via [`Vm::with_host`] so a builtin
    /// can mutate both the host and the rest of the VM without a borrow clash.
    pub host: Option<Box<dyn Host>>,
    /// Frame-start server time (`sv.time`), set once per frame by the Server. The
    /// monster-locomotion builtins (`walkmove`/`movetogoal` -> `sv_movestep` /
    /// `sv_step_direction`) need it for the relink trigger touches: world.c
    /// SV_TouchLinks sets `time = sv.time` before each touch, but during a monster
    /// think the `time` global holds the clamped thinktime, so reading it here
    /// would skew touch timers up to one frame. This field carries the true sv.time.
    pub sv_time: f32,

    /// Monotonic count of QuakeC statements executed across this VM's lifetime
    /// (one per `execute` loop iteration). A free running total used by the sim
    /// benchmark to report VM workload per frame; not gameplay state.
    pub stmt_count: u64,

    // --- private execution state ---
    /// Call stack of saved caller frames (`pr_stack` / `pr_depth`).
    stack: Vec<Frame>,
    /// Saved-locals stack (`localstack` / `localstack_used`).
    localstack: Vec<u32>,
    /// Current function index (`pr_xfunction`).
    xfunction: usize,
    /// Current statement index (`pr_xstatement`).
    xstatement: usize,

    // --- cached well-known defs for OP_STATE (resolved once in `new`) ---
    /// Global cell holding the current `self` entity.
    g_self: Option<usize>,
    /// Global cell holding the current `time` float.
    g_time: Option<usize>,
    /// Entity field offset of `frame`.
    f_frame: Option<usize>,
    /// Entity field offset of `think`.
    f_think: Option<usize>,
    /// Entity field offset of `nextthink`.
    f_nextthink: Option<usize>,
}

impl Vm {
    /// Build a VM from an already-parsed program. Copies the initial globals
    /// and string heap, allocates edict 0 (the world) and installs the default
    /// builtin table.
    pub fn new(progs: Progs) -> Vm {
        let globals = progs.globals.clone();
        let strings = progs.strings.clone();

        // Resolve the OP_STATE well-known globals/fields once, by name.
        let g_self = progs.find_global("self").map(|d| d.ofs as usize);
        let g_time = progs.find_global("time").map(|d| d.ofs as usize);
        let f_frame = progs.find_field("frame").map(|d| d.ofs as usize);
        let f_think = progs.find_field("think").map(|d| d.ofs as usize);
        let f_nextthink = progs.find_field("nextthink").map(|d| d.ofs as usize);

        let mut vm = Vm {
            progs,
            globals,
            edict_fields: Vec::new(),
            edict_free: Vec::new(),
            strings,
            builtins: crate::builtins::default_builtins(),
            output: String::new(),
            argc: 0,
            trace: false,
            host: None,
            sv_time: 0.0,
            stmt_count: 0,
            stack: Vec::new(),
            localstack: Vec::new(),
            xfunction: 0,
            xstatement: 0,
            g_self,
            g_time,
            f_frame,
            f_think,
            f_nextthink,
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

    /// Entity-field cell offset for `name`. O(1) via the progs' cached name->ofs
    /// map (this is on the hot path of every `ent_get_*`/`ent_set_*`).
    pub fn field_ofs(&self, name: &str) -> Option<usize> {
        self.progs.field_offset(name).map(|o| o as usize)
    }
    /// Global cell offset for `name`, O(1) via the progs' cached map.
    pub fn global_ofs(&self, name: &str) -> Option<usize> {
        self.progs.global_offset(name).map(|o| o as usize)
    }

    /// Read entity field `name` as a float (0.0 if the field is unknown).
    pub fn ent_get_float(&self, e: i32, name: &str) -> f32 {
        self.field_ofs(name).map(|o| self.ef(e, o)).unwrap_or(0.0)
    }
    /// Write entity field `name` as a float (no-op if the field is unknown).
    pub fn ent_set_float(&mut self, e: i32, name: &str, v: f32) {
        if let Some(o) = self.field_ofs(name) {
            self.set_ef(e, o, v);
        }
    }
    /// Read entity field `name` as an int.
    pub fn ent_get_int(&self, e: i32, name: &str) -> i32 {
        self.field_ofs(name).map(|o| self.ei(e, o)).unwrap_or(0)
    }
    /// Write entity field `name` as an int.
    pub fn ent_set_int(&mut self, e: i32, name: &str, v: i32) {
        if let Some(o) = self.field_ofs(name) {
            self.set_ei(e, o, v);
        }
    }
    /// Read entity field `name` as a vector.
    pub fn ent_get_vector(&self, e: i32, name: &str) -> Vec3 {
        self.field_ofs(name).map(|o| self.ev(e, o)).unwrap_or([0.0; 3])
    }
    /// Write entity field `name` as a vector.
    pub fn ent_set_vector(&mut self, e: i32, name: &str, v: Vec3) {
        if let Some(o) = self.field_ofs(name) {
            self.set_ev(e, o, v);
        }
    }
    /// Resolve entity field `name` (a `string_t`) to an owned string.
    pub fn ent_get_string(&self, e: i32, name: &str) -> String {
        let s = self.ent_get_int(e, name);
        self.get_string(s)
    }
    /// Borrow entity field `name` as a `&str` (no owned-String allocation). For
    /// hot paths that only read the value (e.g. parsing a `"*N"` submodel name in
    /// the per-tick collision loop). Returns `""` for a null/out-of-range string.
    pub fn ent_string_ref(&self, e: i32, name: &str) -> &str {
        let s = self.ent_get_int(e, name);
        string_in(&self.strings, s)
    }
    /// Intern `value` and store its `string_t` in entity field `name`.
    pub fn ent_set_string(&mut self, e: i32, name: &str, value: &str) {
        let s = self.intern(value);
        self.ent_set_int(e, name, s);
    }

    /// Read global `name` as a float.
    pub fn gget_float(&self, name: &str) -> f32 {
        self.global_ofs(name).map(|o| self.gf(o)).unwrap_or(0.0)
    }
    /// Write global `name` as a float.
    pub fn gset_float(&mut self, name: &str, v: f32) {
        if let Some(o) = self.global_ofs(name) {
            self.set_gf(o, v);
        }
    }
    /// Read global `name` as an int.
    pub fn gget_int(&self, name: &str) -> i32 {
        self.global_ofs(name).map(|o| self.gi(o)).unwrap_or(0)
    }
    /// Write global `name` as an int (also used for `.entity`/`.function` globals).
    pub fn gset_int(&mut self, name: &str, v: i32) {
        if let Some(o) = self.global_ofs(name) {
            self.set_gi(o, v);
        }
    }
    /// Read global `name` as a vector.
    pub fn gget_vector(&self, name: &str) -> Vec3 {
        self.global_ofs(name).map(|o| self.gv(o)).unwrap_or([0.0; 3])
    }
    /// Write global `name` as a vector.
    pub fn gset_vector(&mut self, name: &str, v: Vec3) {
        if let Some(o) = self.global_ofs(name) {
            self.set_gv(o, v);
        }
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

    /// Intern a Rust string: append `"s\0"` to the heap and return its byte
    /// offset (a `string_t`).
    pub fn intern(&mut self, s: &str) -> i32 {
        let ofs = self.strings.len() as i32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        ofs
    }

    /// Resolve a `string_t` (byte offset) to an owned, NUL-terminated `String`.
    /// Out-of-range offsets yield `""`.
    pub fn get_string(&self, s: i32) -> String {
        string_in(&self.strings, s).to_string()
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
    }

    /// `ED_Alloc`: reuse the first free edict, or grow the array. The reused or
    /// new edict is cleared and marked not-free. Returns its index.
    ///
    /// (The C `ED_Alloc` skips client slots and applies a `freetime` relaxation
    /// policy that depends on `sv.time`; with no server time tracked here we use
    /// the simpler "reuse any free slot, else grow" behaviour, which is the same
    /// reuse-or-grow contract.)
    pub fn spawn(&mut self) -> i32 {
        // Skip the world (edict 0) when looking for a free slot.
        for i in 1..self.edict_free.len() {
            if self.edict_free[i] {
                self.clear_edict(i);
                return i as i32;
            }
        }
        let i = self.edict_free.len();
        let ef = self.entityfields();
        self.edict_fields.resize(self.edict_fields.len() + ef, 0);
        self.edict_free.push(false);
        // Newly grown fields are already zero; mark not-free (already false).
        i as i32
    }

    /// `ED_Alloc` with id's hard [`MAX_EDICTS`] ceiling (the C
    /// `Sys_Error("ED_Alloc: no free edicts")`). Reuses a free slot, else grows —
    /// but returns `None` once every slot is in use AND the array is already at the
    /// ceiling, so the QuakeC-reachable `PF_Spawn` surfaces a `run_error` instead of
    /// growing memory without bound on a runaway `spawn()` loop. Engine-internal
    /// spawns (the player, temp entities, the explosive box) use the infallible
    /// [`spawn`](Self::spawn).
    pub fn spawn_checked(&mut self) -> Option<i32> {
        for i in 1..self.edict_free.len() {
            if self.edict_free[i] {
                self.clear_edict(i);
                return Some(i as i32);
            }
        }
        if self.edict_free.len() >= MAX_EDICTS {
            return None;
        }
        let i = self.edict_free.len();
        let ef = self.entityfields();
        self.edict_fields.resize(self.edict_fields.len() + ef, 0);
        self.edict_free.push(false);
        Some(i as i32)
    }

    /// `ED_Free`: zero the edict's fields and mark it free. The world (edict 0)
    /// and out-of-range indices are left untouched.
    pub fn free_edict(&mut self, e: i32) {
        if e <= 0 {
            return; // never free the world
        }
        let e = e as usize;
        if e >= self.edict_free.len() {
            return;
        }
        self.clear_edict(e);
        self.edict_free[e] = true;
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

    /// Build a `QError::Invalid` carrying the message plus the current function
    /// and statement, mirroring `PR_RunError`'s diagnostics (without aborting).
    pub fn run_error(&self, msg: impl Into<String>) -> QError {
        let msg = msg.into();
        let fname = self
            .progs
            .functions
            .get(self.xfunction)
            .map(|f| self.progs.string(f.s_name).to_string())
            .unwrap_or_else(|| "<no function>".to_string());
        QError::invalid(format!(
            "program error in {fname}() @ statement {}: {msg}",
            self.xstatement
        ))
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

    /// `PR_ExecuteProgram`: run function `fnum` to completion, executing nested
    /// calls and builtins, until it returns past the entry frame.
    /// Reset the interpreter call/locals stacks to empty. The C `PR_RunError`
    /// sets `pr_depth = 0` to abandon a faulted call chain; callers that catch a
    /// top-level [`execute`](Self::execute) error and continue (e.g. the server
    /// running per-entity thinks) must call this so the next call starts clean.
    pub fn reset_execution(&mut self) {
        self.stack.clear();
        self.localstack.clear();
        self.xfunction = 0;
        self.xstatement = 0;
    }

    pub fn execute(&mut self, fnum: usize) -> Result<()> {
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
                let mn = Op::from_u16(st.op).map(|o| o.mnemonic()).unwrap_or("<bad>");
                self.output
                    .push_str(&format!("{s:5}: {mn} a={} b={} c={}\n", st.a, st.b, st.c));
            }

            let op = Op::from_u16(st.op)
                .ok_or_else(|| self.run_error(format!("bad opcode {}", st.op)))?;

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
                        s = jump(s, st.b, self).0;
                        // jump returns the new s already accounting for the loop
                        // s++ (we apply b-1 and let s++ re-add). Set 'first' so
                        // the loop does NOT pre-increment again.
                        first = true;
                    }
                }
                Op::If => {
                    if self.cell_i(a)? != 0 {
                        s = jump(s, st.b, self).0;
                        first = true;
                    }
                }
                Op::Goto => {
                    s = jump(s, st.a, self).0;
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
            }
        }
    }

    /// `OP_STATE`: set `self.nextthink = time + 0.1`, `self.frame = a`,
    /// `self.think = b`. Best-effort: if any of the well-known globals/fields is
    /// missing from this program, treat STATE as a no-op (the C unconditionally
    /// dereferences them).
    fn do_state(&mut self, st: Statement) -> Result<()> {
        let (Some(g_self), Some(g_time), Some(f_frame), Some(f_think), Some(f_nextthink)) = (
            self.g_self,
            self.g_time,
            self.f_frame,
            self.f_think,
            self.f_nextthink,
        ) else {
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

/// Compute the new statement index for a branch. The C does `s += offset - 1`
/// and then `s++` at the top of the loop; we return the target index
/// `s + offset` directly (callers set `first = true` so the loop's pre-increment
/// is skipped). The target is range-checked against the statement table.
fn jump(s: usize, offset: i16, vm: &Vm) -> (usize, ()) {
    // s as i64 + offset. Negative offsets jump backward (loops).
    let target = s as i64 + offset as i64;
    if target < 0 {
        // Will fail the bounds check on the next fetch; clamp to a sentinel that
        // is guaranteed out of range so the loop reports a clean run_error.
        return (vm.progs.statements.len(), ());
    }
    (target as usize, ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Def, EType, Function, OFS_NULL, RESERVED_OFS};

    // ---- progs.dat image builder (mirrors progs.rs's test serializer) ----

    const HEADER_SIZE: usize = 60;

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
            Statement { op: Op::AddF as u16, a: g_a as i16, b: g_b as i16, c: g_add as i16 },
            Statement { op: Op::SubF as u16, a: g_a as i16, b: g_b as i16, c: g_sub as i16 },
            Statement { op: Op::MulF as u16, a: g_a as i16, b: g_b as i16, c: g_mul as i16 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
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
            Statement { op: Op::Le as u16, a: g_i as i16, b: g_five as i16, c: g_cmp as i16 },
            Statement { op: Op::Ifnot as u16, a: g_cmp as i16, b: 4, c: 0 },
            Statement { op: Op::AddF as u16, a: g_sum as i16, b: g_i as i16, c: g_sum as i16 },
            Statement { op: Op::AddF as u16, a: g_i as i16, b: g_one as i16, c: g_i as i16 },
            Statement { op: Op::Goto as u16, a: -4, b: 0, c: 0 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
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
            op: Op::AddF as u16,
            a: RESERVED_OFS as i16,
            b: RESERVED_OFS as i16,
            c: OFS_RETURN as i16,
        });
        b.statements.push(Statement { op: Op::Done as u16, a: OFS_RETURN as i16, b: 0, c: 0 });
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
            op: Op::StoreF as u16,
            a: g_input as i16,
            b: OFS_PARM0 as i16,
            c: 0,
        });
        b.statements.push(Statement {
            // CALL1 with function operand at g_func
            op: Op::Call1 as u16,
            a: g_func as i16,
            b: 0,
            c: 0,
        });
        b.statements.push(Statement {
            // STORE_F OFS_RETURN -> g_result
            op: Op::StoreF as u16,
            a: OFS_RETURN as i16,
            b: g_result as i16,
            c: 0,
        });
        b.statements.push(Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 });
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
            Statement { op: Op::Address as u16, a: g_ent as i16, b: g_field as i16, c: g_ptr as i16 },
            Statement { op: Op::StorepF as u16, a: g_val as i16, b: g_ptr as i16, c: 0 },
            Statement { op: Op::LoadF as u16, a: g_ent as i16, b: g_field as i16, c: g_loaded as i16 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
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
        let _main = add_function(&mut b, "main", vec![Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 }]);
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
    fn test_runaway_loop_errors_not_panics() {
        // goto self forever -> runaway loop guard fires as an Err.
        let mut b = Builder::new();
        let stmts = vec![Statement { op: Op::Goto as u16, a: 0, b: 0, c: 0 }];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let err = vm.execute(main).unwrap_err();
        match err {
            QError::Invalid(msg) => assert!(msg.contains("runaway"), "got: {msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn test_bad_opcode_errors() {
        let mut b = Builder::new();
        // opcode 9999 is invalid.
        let stmts = vec![Statement { op: 9999, a: 0, b: 0, c: 0 }];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let err = vm.execute(main).unwrap_err();
        assert!(matches!(err, QError::Invalid(_)));
    }

    #[test]
    fn test_storep_out_of_range_errors() {
        // A STOREP through a pointer past the edict array must error, not panic.
        let mut b = Builder::new();
        b.entityfields = 2;
        let (g_ptr, g_val) = (10usize, 11usize);
        let stmts = vec![
            Statement { op: Op::StorepF as u16, a: g_val as i16, b: g_ptr as i16, c: 0 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
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
        let _ = add_function(&mut b, "main", vec![Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let s = vm.intern("hello");
        assert_eq!(vm.get_string(s), "hello");
        // out-of-range string_t -> empty
        assert_eq!(vm.get_string(-5), "");
        assert_eq!(vm.get_string(1_000_000), "");
    }

    #[test]
    fn test_eq_s_compares_string_contents() {
        let mut b = Builder::new();
        let (g_a, g_b, g_c) = (10usize, 11usize, 12usize);
        let stmts = vec![
            Statement { op: Op::EqS as u16, a: g_a as i16, b: g_b as i16, c: g_c as i16 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
        ];
        let main = add_function(&mut b, "main", stmts);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let sa = vm.intern("foo");
        let sb = vm.intern("foo"); // distinct offset, same contents
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
