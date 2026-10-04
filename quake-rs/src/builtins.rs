//! Native QuakeC builtins (`builtin_t`).
//!
//! Ported from Quake (GPLv2): the `PF_*` functions and the `pr_builtin[]`
//! dispatch table in `WinQuake/pr_cmds.c`. Copyright (C) 1996-1997 Id Software,
//! Inc.
//!
//! Only the *self-contained* builtins are implemented here: the ones that need
//! nothing beyond the QuakeC VM state (console output, math, string formatting,
//! entity spawn/remove/find). Builtins that talk to the server world, the
//! network, the filesystem, the sound system or cvars (`setorigin`, `setmodel`,
//! `sound`, `traceline`, `precache_*`, `cvar`, the `Write*` network ops, …) are
//! stubbed with [`pf_fixme`], which faults via [`Vm::run_error`] rather than
//! aborting — the same shape as the C `PF_Fixme` (which calls `PR_RunError`).
//!
//! ## Numbering
//!
//! Builtin number `n` maps to `pr_builtin[n]`; index 0 is reserved
//! (`PF_Fixme`), so [`default_builtins`] returns a table whose index aligns with
//! the QuakeC `= #n` numbering. The order below is a faithful transcription of
//! the `pr_builtin[]` array in `pr_cmds.c` (the non-`QUAKE2` build), so e.g.
//! `ftos == #26`, `vtos == #27`, `rint == #36`, `fabs == #43`,
//! `nextent == #47`, `vectoangles == #51`.
//!
//! `pr_cmds.c`'s own table ends at `#78` (`setspawnparms`): id's engine, and
//! this port until now, never had a `#79` or `#80` to dispatch — a progs
//! that declared one and called it would hit the VM's own bad-builtin-number
//! check ([`crate::vm::Vm::call_builtin`]), never this table. The mission
//! packs' 2021 re-release `progs.dat` declares exactly two more,
//! `finaleFinished` (#79) and `localsound` (#80) — table slots 79/80 below
//! exist only so that declaration loads without the table itself being
//! "past the end"; `install_engine_builtins`
//! overwrites #79 with the real implementation (it reaches server/host
//! state no self-contained builtin here can), and leaves #80 as
//! [`pf_fixme`] (no pack ever calls it — see AUDIT.md "The mission packs'
//! paths", P7/B4).

use crate::error::{ProgramError, Result};
use crate::vm::{Builtin, Vm};

/// `PF_Fixme`: an unimplemented builtin. The C version calls `PR_RunError`
/// (which `longjmp`s out of the interpreter); we return the equivalent `Err`.
pub fn pf_fixme(vm: &mut Vm) -> Result<()> {
    Err(vm.run_error("unimplemented builtin"))
}

/// A benign no-op builtin that consumes its arguments and returns nothing.
///
/// Used for the *debug / developer* builtins whose only effect in the original
/// engine is console diagnostics or a host-side command-buffer push — things a
/// headless server has no business aborting over:
///
/// * `PF_coredump` (#28) — `ED_PrintEdicts()` (dumps every edict to the console)
/// * `PF_traceon`  (#29) — sets the `pr_trace` per-statement trace flag
/// * `PF_traceoff` (#30) — clears it
/// * `PF_eprint`   (#31) — `ED_PrintNum()` (dumps one edict)
/// * `PF_localcmd` (#46) — `Cbuf_AddText()` (queues console text)
///
/// The stock progs call these for diagnostics (e.g. an `eprint(self)` in a
/// debugging spawn path); the C runs them and continues, so they must NOT fault
/// out of the interpreter the way [`pf_fixme`] does. They have no world effect,
/// so a no-op is faithful — matching how `cvar_set` is handled in the server's
/// engine builtin table.
fn pf_debug_noop(_vm: &mut Vm) -> Result<()> {
    Ok(())
}


/// `PF_VarString(first)`: the string arguments from `first` to `pr_argc`,
/// joined. id's C (and this port, originally) just concatenated them — a
/// fixed 256-byte buffer there, a `String` here. The mission packs' 2021
/// re-release progs instead pass a `"$key"` localization immediate as the
/// first of those arguments, with the rest as its `{0}`/`{1}`/… arguments
/// (AUDIT P6/B3); [`crate::localization::format`] reads the VM's loaded
/// table ([`Vm::loc_table`]) and does that lookup+substitution, falling back
/// to the old plain concatenation when there is no table, no leading `$`,
/// or no such key — so a plain id1 string (and any key the table doesn't
/// carry) prints exactly as before.
pub(crate) fn var_string(vm: &Vm, first: usize) -> String {
    let args: Vec<String> = (first..vm.argc()).map(|i| vm.arg_string(i)).collect();
    crate::localization::format(vm.loc_table(), &args)
}

// ---------------------------------------------------------------- #1 makevectors

/// `PF_makevectors` (#1): `void(vector angles) makevectors`. Computes the
/// forward/right/up basis from `angles` and writes them into the globals
/// `v_forward`, `v_right`, `v_up`.
///
/// The C unconditionally dereferences `pr_global_struct->v_forward` etc.; we
/// write through the handles resolved at load ([`Vm::go`]). If a build's
/// program does not define one of them, that component is silently skipped
/// (a no-op), since there is nowhere to write it. The math matches
/// `AngleVectors` exactly via [`crate::math::angle_vectors`].
fn pf_makevectors(vm: &mut Vm) -> Result<()> {
    let angles = vm.arg_vector(0);
    let (forward, right, up) = crate::math::angle_vectors(angles);
    let go = *vm.go();
    vm.set_glob_vec(go.v_forward, forward);
    vm.set_glob_vec(go.v_right, right);
    vm.set_glob_vec(go.v_up, up);
    Ok(())
}

// -------------------------------------------------------------------- #7 random

/// `PF_random` (#7): `float() random`, a value in the closed `[0, 1]`
/// (`(rand() & 0x7fff) / (float)0x7fff`), drawn from the host session's
/// streams ([`crate::qrand::QRand::random`]) in place of libc's `rand()`.
fn pf_random(vm: &mut Vm) -> Result<()> {
    let num = vm.rand().random();
    vm.ret_float(num);
    Ok(())
}

// ----------------------------------------------------------------- #9 normalize

/// `PF_normalize` (#9): `vector(vector v) normalize`. A zero vector maps to the
/// zero vector, exactly as the C special-cases `new == 0`.
fn pf_normalize(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_vector(0);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let out = if len == 0.0 {
        [0.0, 0.0, 0.0]
    } else {
        let inv = 1.0 / len;
        [v[0] * inv, v[1] * inv, v[2] * inv]
    };
    vm.ret_vector(out);
    Ok(())
}

// ------------------------------------------------------------ #10/#11 error/objerror

/// `PF_error` (#10): `void(string s, ...) error`. id prints
/// `======SERVER ERROR in <function>:` and the text, dumps `self`
/// (`ED_Print`), and calls `Host_Error ("Program error")` — directly, not
/// through `PR_RunError`, so with no statement or stack trace. The program
/// error returned halts the VM like any other ([`Vm::execute`]).
fn pf_error(vm: &mut Vm) -> Result<()> {
    Err(error_report(vm, "SERVER ERROR").into())
}

/// `PF_objerror` (#11): as [`pf_error`], under `======OBJECT ERROR`, and
/// `self` is freed (`ED_Free`) after its dump, before `Host_Error`.
fn pf_objerror(vm: &mut Vm) -> Result<()> {
    let report = error_report(vm, "OBJECT ERROR");
    let ent = vm.glob_int(vm.go().self_);
    vm.free_edict(ent);
    Err(report.into())
}

/// What `PF_error`/`PF_objerror` print on the way to `Host_Error`:
/// `======<kind> in <function>:`, the message, and `ED_Print (self)`. The
/// text also goes to the output log ([`Vm::print`]), like `dprint`'s.
fn error_report(vm: &mut Vm, kind: &str) -> ProgramError {
    let message = var_string(vm, 0);
    let function = vm.running_function().to_string();
    let dump = vm.ed_print(vm.glob_int(vm.go().self_));
    let console = format!("======{kind} in {function}:\n{message}\n{dump}");
    vm.print(&console);
    ProgramError { function, message, console }
}

// ------------------------------------------------------------------- #12 vlen

/// `PF_vlen` (#12): `float(vector v) vlen` — the Euclidean length.
fn pf_vlen(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_vector(0);
    vm.ret_float((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
    Ok(())
}

// ----------------------------------------------------------------- #13 vectoyaw

/// `PF_vectoyaw` (#13): `float(vector v) vectoyaw`. Returns the compass yaw in
/// degrees `[0, 360)`; `0` when the vector has no horizontal component.
///
/// The C computes `(int)(atan2(y, x) * 180 / M_PI)` in `double` (truncating
/// toward zero), then adds 360 if negative. We mirror the `double` trig and the
/// `(int)` truncation.
fn pf_vectoyaw(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_vector(0);
    let yaw = if v[1] == 0.0 && v[0] == 0.0 {
        0.0
    } else {
        let deg = (f64::from(v[1])).atan2(f64::from(v[0])) * 180.0 / std::f64::consts::PI;
        let mut y = deg as i32 as f32; // (int) truncation toward zero, like C
        if y < 0.0 {
            y += 360.0;
        }
        y
    };
    vm.ret_float(yaw);
    Ok(())
}

// -------------------------------------------------------------- #51 vectoangles

/// `PF_vectoangles` (#51): `vector(vector v) vectoangles`. Returns
/// `(pitch, yaw, 0)` in degrees, matching the C `double`-precision trig and
/// `(int)` truncation. The straight-up / straight-down special case yields
/// `pitch = 90` / `pitch = 270`.
fn pf_vectoangles(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_vector(0);
    let (pitch, yaw);
    if v[1] == 0.0 && v[0] == 0.0 {
        yaw = 0.0;
        pitch = if v[2] > 0.0 { 90.0 } else { 270.0 };
    } else {
        let ydeg = (f64::from(v[1])).atan2(f64::from(v[0])) * 180.0 / std::f64::consts::PI;
        let mut y = ydeg as i32 as f32;
        if y < 0.0 {
            y += 360.0;
        }
        yaw = y;

        let forward = (f64::from(v[0]) * f64::from(v[0]) + f64::from(v[1]) * f64::from(v[1])).sqrt();
        let pdeg = (f64::from(v[2])).atan2(forward) * 180.0 / std::f64::consts::PI;
        let mut p = pdeg as i32 as f32;
        if p < 0.0 {
            p += 360.0;
        }
        pitch = p;
    }
    vm.ret_vector([pitch, yaw, 0.0]);
    Ok(())
}

// ------------------------------------------------------------- #14/#15 spawn/remove

/// `PF_Spawn` (#14): `entity() spawn` — allocate (or reuse) an edict. Enforces
/// the live `ED_Alloc` ceiling (`Vm::max_edicts`: id's `MAX_EDICTS` 600, or
/// higher where the slop-only `sv_max_edicts` cvar raised it — see
/// `Vm::set_max_edicts`); the C `ED_Alloc` `Sys_Error`s when full, so here a
/// spawn past the ceiling (a runaway QuakeC loop, or Classic's/an unraised
/// 600) fails with a `run_error` rather than exhausting memory.
fn pf_spawn(vm: &mut Vm) -> Result<()> {
    match vm.spawn_checked() {
        Some(e) => {
            vm.ret_entity(e);
            Ok(())
        }
        None => Err(vm.run_error("ED_Alloc: no free edicts")),
    }
}

/// `PF_Remove` (#15): `void(entity e) remove` — free the edict.
fn pf_remove(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    vm.free_edict(e);
    Ok(())
}

// ------------------------------------------------------ #23/#24/#25/#66 prints

/// `PF_bprint` (#23): `void(string s) bprint` — broadcast print. We append the
/// concatenated string args to the captured `output`.
fn pf_bprint(vm: &mut Vm) -> Result<()> {
    let s = var_string(vm, 0);
    vm.print(&s);
    Ok(())
}

/// `PF_sprint` (#24): `void(entity client, string s) sprint` — single-client
/// print. The client routing is server state we don't model, so the entity arg
/// (index 0) is ignored and the message portion (args from index 1) is appended
/// to `output`.
fn pf_sprint(vm: &mut Vm) -> Result<()> {
    let s = var_string(vm, 1);
    vm.print(&s);
    Ok(())
}

/// `PF_dprint` (#25): `void(string s) dprint` — developer print. Appended to
/// `output`.
fn pf_dprint(vm: &mut Vm) -> Result<()> {
    let s = var_string(vm, 0);
    vm.print(&s);
    Ok(())
}

/// `PF_centerprint` (#66): like `sprint`, the message portion (args from index
/// 1) is appended to `output`; the client routing is ignored.
fn pf_centerprint(vm: &mut Vm) -> Result<()> {
    let s = var_string(vm, 1);
    vm.print(&s);
    Ok(())
}

// ----------------------------------------------------------------- #26 ftos

/// `PF_ftos` (#26): `string(float) ftos`. The C prints `"%d"` when the value is
/// integral (`v == (int)v`) and `"%5.1f"` otherwise, then returns a `string_t`.
/// We intern the formatted text and return it.
///
/// `(int)v` truncates toward zero; matching that with `v as i32` reproduces the
/// integral test exactly (e.g. `3.0 -> "3"`, `2.5 -> "  2.5"`).
fn pf_ftos(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_float(0);
    let s = if v == (v as i32) as f32 {
        format!("{}", v as i32)
    } else {
        // C "%5.1f": width 5, 1 fractional digit, space-padded, right-justified.
        format!("{v:5.1}")
    };
    let st = vm.intern(&s);
    vm.ret_string(st);
    Ok(())
}

// ----------------------------------------------------------------- #27 vtos

/// `PF_vtos` (#27): `string(vector) vtos`. Formats as `'%5.1f %5.1f %5.1f'`
/// (single-quote wrapped), interns it and returns the `string_t`.
fn pf_vtos(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_vector(0);
    let s = format!("'{:5.1} {:5.1} {:5.1}'", v[0], v[1], v[2]);
    let st = vm.intern(&s);
    vm.ret_string(st);
    Ok(())
}

// ------------------------------------------------------ #36/#37/#38/#43 math

/// `PF_rint` (#36): round to nearest integer. The C does `(int)(f + 0.5)` for
/// `f > 0` and `(int)(f - 0.5)` for `f <= 0`; both truncate toward zero, which
/// `as i32` reproduces. We replicate the sign split exactly.
///
/// The C's `f` is a `float`, but `f + 0.5` promotes `f` to `double` and adds the
/// `double` literal `0.5` before the `(int)` truncation. Doing the add in `f32`
/// can round to a different integer on the half-way boundary (e.g. a float that
/// sits just under `x.5` rounds up in f32 but not in f64), so we promote to f64
/// before adding 0.5 to match PF_rint bit-for-bit.
fn pf_rint(vm: &mut Vm) -> Result<()> {
    let f = vm.arg_float(0);
    let r = if f > 0.0 {
        (f as f64 + 0.5) as i32
    } else {
        (f as f64 - 0.5) as i32
    };
    vm.ret_float(r as f32);
    Ok(())
}

/// `PF_floor` (#37): `float(float) floor`.
fn pf_floor(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_float(0);
    vm.ret_float(v.floor());
    Ok(())
}

/// `PF_ceil` (#38): `float(float) ceil`.
fn pf_ceil(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_float(0);
    vm.ret_float(v.ceil());
    Ok(())
}

/// `PF_fabs` (#43): `float(float) fabs`.
fn pf_fabs(vm: &mut Vm) -> Result<()> {
    let v = vm.arg_float(0);
    vm.ret_float(v.abs());
    Ok(())
}

// ----------------------------------------------------------- #18/#47 find/nextent

/// `PF_Find` (#18): `entity(entity start, .string field, string match) find`.
///
/// Scans edicts *after* `start` for the first non-free one whose string field
/// at offset `field` (compared by contents via the string heap) equals `match`,
/// returning that edict; returns the world (edict 0) if none matches. This is
/// the non-`QUAKE2` `PF_Find`.
///
/// The C reads `t = E_STRING(ed,f)` and compares it with `strcmp(t, s)`. Our
/// `field` value is a string field's cell offset (the QuakeC `.string` operand,
/// which is the field's `ofs`); an unset field reads string offset 0, which is
/// the empty string `""` — exactly the non-NULL `t` the C sees — so an empty
/// `match` matches an empty stored field (`strcmp("", "") == 0`), matching id.
fn pf_find(vm: &mut Vm) -> Result<()> {
    let start = vm.arg_entity(0);
    let field = vm.arg_int(1);
    let m = vm.arg_string(2);
    if field < 0 {
        return Err(vm.run_error(format!("find: bad field offset {field}")));
    }
    let field = field as usize;

    // Begin at start+1 (the C `for (e++ ; ...)`), guarding against overflow.
    let mut e = start.saturating_add(1);
    while e >= 0 && (e as usize) < vm.num_edicts() {
        let free = vm.is_free_edict(e);
        if !free {
            let s_t = vm.ei(e, field);
            let s = vm.string(s_t);
            // Match by contents, borrowed (no String per edict). In the C,
            // `t = E_STRING(ed,f)` is the empty string "" (string offset 0),
            // not NULL, so `strcmp(t, s)` matches an empty stored field against
            // an empty search string. Compare contents directly — an empty
            // `match` finds an empty field.
            if s == m {
                vm.ret_entity(e);
                return Ok(());
            }
        }
        e += 1;
    }
    vm.ret_entity(0);
    Ok(())
}

/// `PF_nextent` (#47): `entity(entity) nextent` — the next non-free edict after
/// the argument, or the world (edict 0) when there are no more.
fn pf_nextent(vm: &mut Vm) -> Result<()> {
    let mut e = vm.arg_entity(0).saturating_add(1);
    while e >= 0 && (e as usize) < vm.num_edicts() {
        let free = vm.is_free_edict(e);
        if !free {
            vm.ret_entity(e);
            return Ok(());
        }
        e += 1;
    }
    vm.ret_entity(0);
    Ok(())
}

/// The default builtin table, indexed by QuakeC builtin number. Index 0 is the
/// reserved `PF_Fixme` slot; thereafter the order matches `pr_builtin[]` in
/// `pr_cmds.c` (non-`QUAKE2` build). Builtins that need server-world, network,
/// filesystem, sound or cvar state are filled with [`pf_fixme`].
pub fn default_builtins() -> Vec<Builtin> {
    vec![
        pf_fixme,       // 0   PF_Fixme (reserved)
        pf_makevectors, // 1   makevectors
        pf_fixme,       // 2   setorigin     (server world)
        pf_fixme,       // 3   setmodel      (server world)
        pf_fixme,       // 4   setsize       (server world)
        pf_fixme,       // 5   setabssize    (PF_Fixme in C)
        pf_fixme,       // 6   break
        pf_random,      // 7   random
        pf_fixme,       // 8   sound         (sound system)
        pf_normalize,   // 9   normalize
        pf_error,       // 10  error
        pf_objerror,    // 11  objerror
        pf_vlen,        // 12  vlen
        pf_vectoyaw,    // 13  vectoyaw
        pf_spawn,       // 14  spawn
        pf_remove,      // 15  remove
        pf_fixme,       // 16  traceline     (server world)
        pf_fixme,       // 17  checkclient   (server world / PVS)
        pf_find,        // 18  find
        pf_fixme,       // 19  precache_sound
        pf_fixme,       // 20  precache_model
        pf_fixme,       // 21  stuffcmd      (network)
        pf_fixme,       // 22  findradius    (server world)
        pf_bprint,      // 23  bprint
        pf_sprint,      // 24  sprint
        pf_dprint,      // 25  dprint
        pf_ftos,        // 26  ftos
        pf_vtos,        // 27  vtos
        pf_debug_noop,  // 28  coredump  (ED_PrintEdicts -> benign no-op)
        pf_debug_noop,  // 29  traceon   (pr_trace = true -> benign no-op)
        pf_debug_noop,  // 30  traceoff  (pr_trace = false -> benign no-op)
        pf_debug_noop,  // 31  eprint    (ED_PrintNum -> benign no-op)
        pf_fixme,       // 32  walkmove      (server world)
        pf_fixme,       // 33  (PF_Fixme in C)
        pf_fixme,       // 34  droptofloor   (server world)
        pf_fixme,       // 35  lightstyle    (server/network)
        pf_rint,        // 36  rint
        pf_floor,       // 37  floor
        pf_ceil,        // 38  ceil
        pf_fixme,       // 39  (PF_Fixme in C)
        pf_fixme,       // 40  checkbottom   (server world)
        pf_fixme,       // 41  pointcontents (server world)
        pf_fixme,       // 42  (PF_Fixme in C)
        pf_fabs,        // 43  fabs
        pf_fixme,       // 44  aim           (server world)
        pf_fixme,       // 45  cvar
        pf_debug_noop,  // 46  localcmd  (Cbuf_AddText -> benign no-op)
        pf_nextent,     // 47  nextent
        pf_fixme,       // 48  particle      (server/network)
        pf_fixme,       // 49  changeyaw     (server world)
        pf_fixme,       // 50  (PF_Fixme in C)
        pf_vectoangles, // 51  vectoangles
        pf_fixme,       // 52  WriteByte     (network)
        pf_fixme,       // 53  WriteChar     (network)
        pf_fixme,       // 54  WriteShort    (network)
        pf_fixme,       // 55  WriteLong     (network)
        pf_fixme,       // 56  WriteCoord    (network)
        pf_fixme,       // 57  WriteAngle    (network)
        pf_fixme,       // 58  WriteString   (network)
        pf_fixme,       // 59  WriteEntity   (network)
        // #60-66: the 7 PF_Fixme stubs the non-QUAKE2 build inserts (the #else of
        // the QUAKE2 sin/cos/sqrt/changepitch/TraceToss/etos/WaterMove block).
        pf_fixme,       // 60
        pf_fixme,       // 61
        pf_fixme,       // 62
        pf_fixme,       // 63
        pf_fixme,       // 64
        pf_fixme,       // 65
        pf_fixme,       // 66
        pf_fixme,       // 67  SV_MoveToGoal (server world)
        pf_fixme,       // 68  precache_file
        pf_fixme,       // 69  makestatic    (network)
        pf_fixme,       // 70  changelevel   (command buffer)
        pf_fixme,       // 71  (PF_Fixme in C)
        pf_fixme,       // 72  cvar_set
        pf_centerprint, // 73  centerprint
        pf_fixme,       // 74  ambientsound  (network)
        pf_fixme,       // 75  precache_model
        pf_fixme,       // 76  precache_sound
        pf_fixme,       // 77  precache_file
        pf_fixme,       // 78  setspawnparms (server world)
        pf_fixme,       // 79  finaleFinished (mission packs' re-release only; server
                        //     world — install_engine_builtins puts bi_finale_finished)
        pf_fixme,       // 80  localsound (mission packs' re-release only; never
                        //     called by either pack — left unimplemented, see B4)
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Function, Op, Progs, Statement, OFS_PARM0, OFS_RETURN, RESERVED_OFS};

    const HEADER_SIZE: usize = 60;

    // Minimal progs builder: one bytecode "main" that CALLs a builtin record,
    // then DONEs preserving OFS_RETURN so the builtin's result survives.
    fn build_calling_builtin(builtin_no: i32, call_op: Op) -> (Vec<u8>, usize) {
        let mut strings = vec![0u8];
        let intern = |strings: &mut Vec<u8>, s: &str| -> i32 {
            let o = strings.len() as i32;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
            o
        };
        let n_main = intern(&mut strings, "main");
        let n_bi = intern(&mut strings, "thebuiltin");

        let func0 = Function {
            first_statement: 0,
            parm_start: 0,
            locals: 0,
            profile: 0,
            s_name: 0,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        };
        // main: CALLn at g_func (=60), then DONE returning OFS_RETURN unchanged.
        let g_func = 60i16;
        let stmts = [
            Statement { op: call_op, a: g_func, b: 0, c: 0 },
            Statement { op: Op::Done, a: OFS_RETURN as i16, b: 0, c: 0 },
        ];
        let main = Function {
            first_statement: 0,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name: n_main,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        };
        // A builtin record has a negative first_statement = -builtin_no.
        let bi = Function {
            first_statement: -builtin_no,
            parm_start: 0,
            locals: 0,
            profile: 0,
            s_name: n_bi,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        };
        let functions = [func0, main, bi];
        let bi_idx = 2usize;

        let globals: Vec<u32> = vec![0u32; 64];

        let ser_stmt = |s: &Statement| {
            let mut v = Vec::new();
            v.extend_from_slice(&s.op.code().to_le_bytes());
            v.extend_from_slice(&s.a.to_le_bytes());
            v.extend_from_slice(&s.b.to_le_bytes());
            v.extend_from_slice(&s.c.to_le_bytes());
            v
        };
        let ser_func = |f: &Function| {
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
        };

        let mut body = Vec::new();
        let ofs_statements = HEADER_SIZE + body.len();
        for s in &stmts {
            body.extend_from_slice(&ser_stmt(s));
        }
        let ofs_globaldefs = HEADER_SIZE + body.len(); // none
        let ofs_fielddefs = HEADER_SIZE + body.len(); // none
        let ofs_functions = HEADER_SIZE + body.len();
        for f in &functions {
            body.extend_from_slice(&ser_func(f));
        }
        let ofs_strings = HEADER_SIZE + body.len();
        body.extend_from_slice(&strings);
        let ofs_globals = HEADER_SIZE + body.len();
        for g in &globals {
            body.extend_from_slice(&g.to_le_bytes());
        }

        let header: [i32; 15] = [
            crate::progs::PROG_VERSION,
            0,
            ofs_statements as i32,
            stmts.len() as i32,
            ofs_globaldefs as i32,
            0,
            ofs_fielddefs as i32,
            0,
            ofs_functions as i32,
            functions.len() as i32,
            ofs_strings as i32,
            strings.len() as i32,
            ofs_globals as i32,
            globals.len() as i32,
            0,
        ];
        let mut out = Vec::new();
        for x in header {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.extend_from_slice(&body);
        (out, bi_idx)
    }

    /// Build a bare VM with no bytecode (just enough to call builtins directly).
    fn bare_vm() -> Vm {
        let (img, _bi) = build_calling_builtin(7, Op::Call0);
        Vm::load(&img).expect("load")
    }

    #[test]
    fn dprint_appends_to_output() {
        // builtin #25 = dprint, called CALL1 with a string arg.
        let (img, bi_idx) = build_calling_builtin(25, Op::Call1);
        let _ = Progs::parse(&img).expect("parse");
        let mut vm = Vm::load(&img).expect("load");
        let s = vm.intern("hello world");
        vm.set_gi(OFS_PARM0, s);
        vm.set_gi(60, bi_idx as i32);
        vm.call_by_name("main").expect("run");
        assert_eq!(vm.output(), "hello world");
    }

    #[test]
    fn ftos_integer_formats_as_d() {
        let mut vm = bare_vm();
        vm.set_gf(OFS_PARM0, 3.0);
        pf_ftos(&mut vm).expect("ftos");
        let s = vm.get_string(vm.gi(OFS_RETURN));
        assert_eq!(s, "3", "ftos(3.0) == \"3\"");
    }

    #[test]
    fn ftos_fraction_formats_as_5_1f() {
        let mut vm = bare_vm();
        vm.set_gf(OFS_PARM0, 2.5);
        pf_ftos(&mut vm).expect("ftos");
        let s = vm.get_string(vm.gi(OFS_RETURN));
        assert_eq!(s, "  2.5", "ftos(2.5) == \"  2.5\" (%5.1f)");
    }

    #[test]
    fn ftos_via_bytecode_call() {
        let (img, bi_idx) = build_calling_builtin(26, Op::Call1); // ftos
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gf(OFS_PARM0, 42.0);
        vm.set_gi(60, bi_idx as i32);
        vm.call_by_name("main").expect("run");
        let s = vm.get_string(vm.gi(OFS_RETURN));
        assert_eq!(s, "42");
    }

    #[test]
    fn vtos_formats_three_components() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [1.0, 2.0, 3.0]);
        pf_vtos(&mut vm).expect("vtos");
        let s = vm.get_string(vm.gi(OFS_RETURN));
        assert_eq!(s, "'  1.0   2.0   3.0'");
    }

    #[test]
    fn vlen_computes_length() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [3.0, 4.0, 0.0]);
        pf_vlen(&mut vm).expect("vlen");
        assert_eq!(vm.gf(OFS_RETURN), 5.0);
    }

    #[test]
    fn normalize_unit_vector() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [0.0, 3.0, 0.0]);
        pf_normalize(&mut vm).expect("normalize");
        assert_eq!(vm.gv(OFS_RETURN), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn normalize_zero_vector_stays_zero() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [0.0, 0.0, 0.0]);
        pf_normalize(&mut vm).expect("normalize");
        assert_eq!(vm.gv(OFS_RETURN), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn spawn_returns_fresh_index_then_remove_frees() {
        let mut vm = bare_vm();
        assert_eq!(vm.num_edicts(), 1); // world only

        pf_spawn(&mut vm).expect("spawn");
        let e1 = vm.gi(OFS_RETURN);
        assert_eq!(e1, 1, "first spawn is edict 1");
        assert!(!vm.is_free_edict(1));

        pf_spawn(&mut vm).expect("spawn");
        let e2 = vm.gi(OFS_RETURN);
        assert_eq!(e2, 2, "second spawn is edict 2");

        // remove(e1): mark it free.
        vm.set_gi(OFS_PARM0, e1);
        pf_remove(&mut vm).expect("remove");
        assert!(vm.is_free_edict(1));

        // The next spawn reuses the freed slot.
        pf_spawn(&mut vm).expect("spawn");
        assert_eq!(vm.gi(OFS_RETURN), 1, "spawn reuses freed edict 1");
    }

    #[test]
    fn dprint_appends_direct() {
        let mut vm = bare_vm();
        let s = vm.intern("xyzzy");
        vm.set_gi(OFS_PARM0, s);
        vm.call_builtin(25, 1).expect("dprint");
        assert_eq!(vm.output(), "xyzzy");
    }

    #[test]
    fn rint_rounds_half_away_for_sign() {
        let mut vm = bare_vm();
        // C: f>0 -> (int)(f+0.5); else (int)(f-0.5)
        vm.set_gf(OFS_PARM0, 2.4);
        pf_rint(&mut vm).expect("rint");
        assert_eq!(vm.gf(OFS_RETURN), 2.0);
        vm.set_gf(OFS_PARM0, 2.6);
        pf_rint(&mut vm).expect("rint");
        assert_eq!(vm.gf(OFS_RETURN), 3.0);
        vm.set_gf(OFS_PARM0, -2.6);
        pf_rint(&mut vm).expect("rint");
        assert_eq!(vm.gf(OFS_RETURN), -3.0);
    }

    #[test]
    fn rint_promotes_to_f64_at_the_half_boundary() {
        // FIX-4: PF_rint does `(int)(f + 0.5)` with `f` (a C float) PROMOTED to
        // double, so the +0.5 happens in double precision. f = 8388609 (an odd
        // integer in [2^23, 2^24), where the f32 ulp is exactly 1.0) is the
        // canonical divergence: in f32, `8388609 + 0.5` rounds half-to-even up
        // to 8388610, so an f32 add would (wrongly) yield 8388610. In f64 the
        // sum is exactly 8388609.5, truncating to 8388609 — what the C produces.
        let mut vm = bare_vm();
        let f = 8_388_609.0f32;
        vm.set_gf(OFS_PARM0, f);
        pf_rint(&mut vm).expect("rint");
        assert_eq!(
            vm.gf(OFS_RETURN),
            8_388_609.0,
            "rint must add 0.5 in f64 (got the f32-rounded 8388610 instead)"
        );
        // The symmetric negative case: (int)(f - 0.5) in double = -8388609.
        vm.set_gf(OFS_PARM0, -f);
        pf_rint(&mut vm).expect("rint");
        assert_eq!(
            vm.gf(OFS_RETURN),
            -8_388_609.0,
            "rint must subtract 0.5 in f64 for the negative boundary"
        );
    }

    #[test]
    fn floor_ceil_fabs() {
        let mut vm = bare_vm();
        vm.set_gf(OFS_PARM0, 2.7);
        pf_floor(&mut vm).expect("floor");
        assert_eq!(vm.gf(OFS_RETURN), 2.0);
        pf_ceil(&mut vm).expect("ceil");
        assert_eq!(vm.gf(OFS_RETURN), 3.0);
        vm.set_gf(OFS_PARM0, -4.5);
        pf_fabs(&mut vm).expect("fabs");
        assert_eq!(vm.gf(OFS_RETURN), 4.5);
    }

    #[test]
    fn vectoyaw_axes() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [0.0, 0.0, 0.0]);
        pf_vectoyaw(&mut vm).expect("vectoyaw");
        assert_eq!(vm.gf(OFS_RETURN), 0.0, "zero horizontal -> 0");

        vm.set_gv(OFS_PARM0, [1.0, 0.0, 0.0]);
        pf_vectoyaw(&mut vm).expect("vectoyaw");
        assert_eq!(vm.gf(OFS_RETURN), 0.0, "+x -> 0 deg");

        vm.set_gv(OFS_PARM0, [0.0, 1.0, 0.0]);
        pf_vectoyaw(&mut vm).expect("vectoyaw");
        assert_eq!(vm.gf(OFS_RETURN), 90.0, "+y -> 90 deg");

        vm.set_gv(OFS_PARM0, [-1.0, 0.0, 0.0]);
        pf_vectoyaw(&mut vm).expect("vectoyaw");
        assert_eq!(vm.gf(OFS_RETURN), 180.0, "-x -> 180 deg");
    }

    #[test]
    fn vectoangles_straight_up_down() {
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [0.0, 0.0, 1.0]);
        pf_vectoangles(&mut vm).expect("vectoangles");
        assert_eq!(vm.gv(OFS_RETURN), [90.0, 0.0, 0.0]);

        vm.set_gv(OFS_PARM0, [0.0, 0.0, -1.0]);
        pf_vectoangles(&mut vm).expect("vectoangles");
        assert_eq!(vm.gv(OFS_RETURN), [270.0, 0.0, 0.0]);
    }

    #[test]
    fn random_is_in_unit_interval_and_draws_from_the_vms_streams() {
        fn draw(vm: &mut Vm) -> f32 {
            pf_random(vm).expect("random");
            vm.gf(OFS_RETURN)
        }
        // Many draws all land in the CLOSED [0,1] (PF_random divides by 0x7fff,
        // so 1.0 is attainable), and they vary.
        let (mut a, mut b) = (bare_vm(), bare_vm());
        let seq: Vec<f32> = (0..1000).map(|_| draw(&mut a)).collect();
        assert!(seq.iter().all(|r| (0.0..=1.0).contains(r)), "random() out of [0,1]");
        assert!(seq.windows(2).any(|w| w[0] != w[1]), "random() should produce varied values");
        // Each VM has fresh streams of its own: another draws the same numbers,
        // whatever ran before it.
        let again: Vec<f32> = (0..1000).map(|_| draw(&mut b)).collect();
        assert_eq!(seq, again, "a fresh VM repeats the sequence");
        // Handed one session's streams, two VMs continue one sequence.
        let session = std::rc::Rc::new(crate::qrand::QRand::new());
        a.set_rand(std::rc::Rc::clone(&session));
        b.set_rand(session);
        assert_eq!([draw(&mut a), draw(&mut b)], [seq[0], seq[1]], "one shared sequence");
    }

    #[test]
    fn find_locates_matching_string_field() {
        // entityfields large enough that field offset 1 is valid.
        let mut vm = {
            let (img, _bi) = build_calling_builtin(18, Op::Call3);
            Vm::load(&img).expect("load")
        };
        // Give the world enough fields by spawning entities; entityfields()>=1.
        // Build three entities and set a string field on the second.
        let e1 = vm.spawn();
        let e2 = vm.spawn();
        let e3 = vm.spawn();
        assert_eq!((e1, e2, e3), (1, 2, 3));

        let field = 0usize; // works for any entityfields() >= 1
        let target = vm.intern("monster");
        vm.set_ei(e2, field, target);

        // find(start=world(0), field=0, match="monster") -> e2
        // (a distinct intern offset with the same text, to prove content match)
        let match_str = vm.intern("monster");
        vm.set_gi(OFS_PARM0, 0);
        vm.set_gi(OFS_PARM0 + 3, field as i32); // PARM1
        vm.set_gi(OFS_PARM0 + 6, match_str); // PARM2
        pf_find(&mut vm).expect("find");
        assert_eq!(vm.gi(OFS_RETURN), e2, "find returns the matching edict");

        // A search that matches nothing returns the world (0).
        let miss = vm.intern("nonexistent");
        vm.set_gi(OFS_PARM0, 0);
        vm.set_gi(OFS_PARM0 + 3, field as i32);
        vm.set_gi(OFS_PARM0 + 6, miss);
        pf_find(&mut vm).expect("find");
        assert_eq!(vm.gi(OFS_RETURN), 0, "no match -> world");
    }

    #[test]
    fn find_matches_empty_field_with_empty_search() {
        // PF_Find: an unset string field reads string offset 0 (the empty
        // string ""), which the C compares with strcmp(t, "") == 0. So an empty
        // search string must match an entity whose field is empty/unset.
        let mut vm = {
            let (img, _bi) = build_calling_builtin(18, Op::Call3);
            Vm::load(&img).expect("load")
        };
        let e1 = vm.spawn();
        let e2 = vm.spawn();
        assert_eq!((e1, e2), (1, 2));

        let field = 0usize;
        // e1 has a non-empty field; e2 is left unset (empty).
        let nonempty = vm.intern("monster");
        vm.set_ei(e1, field, nonempty);

        // find(start=world(0), field=0, match="") -> first entity with an empty
        // field, i.e. e2 (e1's field is "monster", not "").
        let empty = vm.intern(""); // string offset 0
        vm.set_gi(OFS_PARM0, 0);
        vm.set_gi(OFS_PARM0 + 3, field as i32);
        vm.set_gi(OFS_PARM0 + 6, empty);
        pf_find(&mut vm).expect("find");
        assert_eq!(
            vm.gi(OFS_RETURN),
            e2,
            "empty search matches the first empty field, not the non-empty one"
        );
    }

    #[test]
    fn nextent_skips_free_edicts() {
        let mut vm = bare_vm();
        let e1 = vm.spawn();
        let e2 = vm.spawn();
        let _e3 = vm.spawn();
        assert_eq!((e1, e2), (1, 2));
        // Free e2 so nextent(1) should skip it and land on 3.
        vm.free_edict(e2);

        vm.set_gi(OFS_PARM0, 1);
        pf_nextent(&mut vm).expect("nextent");
        assert_eq!(vm.gi(OFS_RETURN), 3, "nextent skips the freed edict");

        // No more after 3 -> world (0).
        vm.set_gi(OFS_PARM0, 3);
        pf_nextent(&mut vm).expect("nextent");
        assert_eq!(vm.gi(OFS_RETURN), 0);
    }

    #[test]
    fn error_and_objerror_fault() {
        let mut vm = bare_vm();
        let s = vm.intern("boom");
        vm.set_gi(OFS_PARM0, s);
        // PF_error's banner and ED_Print (self): here the world, all zeros.
        let banner = "======SERVER ERROR in :\nboom\n\nEDICT 0:\n";
        let Err(crate::QError::Program(e)) = vm.call_builtin(10, 1) else { panic!("error() must fault") };
        assert_eq!((e.message.as_str(), e.console.as_str()), ("boom", banner));
        assert!(vm.output().contains(banner));

        let mut vm = bare_vm();
        let s = vm.intern("kaboom");
        vm.set_gi(OFS_PARM0, s);
        let Err(crate::QError::Program(e)) = vm.call_builtin(11, 1) else { panic!("objerror() must fault") };
        assert!(e.console.starts_with("======OBJECT ERROR in :\nkaboom\n"), "{e:?}");
    }

    #[test]
    fn makevectors_no_globals_is_noop() {
        // Our bare program defines no v_forward/v_right/v_up globals, so
        // makevectors must not fault — it just has nowhere to write.
        let mut vm = bare_vm();
        vm.set_gv(OFS_PARM0, [0.0, 90.0, 0.0]);
        assert!(pf_makevectors(&mut vm).is_ok());
    }

    #[test]
    fn fixme_builtin_faults_not_panics() {
        let (img, bi_idx) = build_calling_builtin(2, Op::Call1); // setorigin -> fixme
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gi(60, bi_idx as i32);
        assert!(vm.call_by_name("main").is_err());
    }

    #[test]
    fn debug_builtins_are_inert_not_faulting() {
        // FIX-2: coredump(#28)/traceon(#29)/traceoff(#30)/eprint(#31) and
        // localcmd(#46) are diagnostic/host-side builtins. The C runs them and
        // continues; here they must be benign no-ops, NOT faults like pf_fixme.
        let mut vm = bare_vm();
        for n in [28usize, 29, 30, 31, 46] {
            assert!(
                vm.call_builtin(n, 1).is_ok(), // with an arg slot to consume
                "debug builtin #{n} must be an inert no-op, not a fault"
            );
        }
        // Sanity: a true engine-world stub (#16 traceline) is still a fixme fault
        // in the bare default table (only the engine server installs the real one).
        assert!(
            vm.call_builtin(16, 4).is_err(),
            "#16 traceline stays a fault in the default table"
        );
    }

    #[test]
    fn debug_builtins_run_through_bytecode_without_aborting() {
        // A progs that CALLs coredump (#28) must complete, not abort the program
        // (the old behaviour faulted via PF_Fixme).
        let (img, bi_idx) = build_calling_builtin(28, Op::Call0);
        let mut vm = Vm::load(&img).expect("load");
        vm.set_gi(60, bi_idx as i32);
        assert!(
            vm.call_by_name("main").is_ok(),
            "coredump() called from bytecode must not abort"
        );
    }

    #[test]
    fn table_is_correctly_indexed() {
        let table = default_builtins();
        // Index 0 is reserved (Fixme): calling it must error.
        let mut vm = bare_vm();
        assert!((table[0])(&mut vm).is_err());
        // The full non-QUAKE2 table is 79 entries (0..=78), plus the mission
        // packs' re-release #79/#80 (finaleFinished/localsound) — 81 total;
        // centerprint is #73.
        assert_eq!(table.len(), 81, "table must hold 0..=78 plus #79/#80");

        // #14 spawn writes a fresh entity index into OFS_RETURN.
        let before = vm.num_edicts();
        (table[14])(&mut vm).expect("spawn via table");
        assert_eq!(vm.num_edicts(), before + 1);
        assert_eq!(vm.gi(OFS_RETURN), before as i32);

        // #73 must be centerprint (the bug was the table being off-by-7 here).
        // It concatenates args from index 1 (arg 0 is the client entity it
        // ignores), per pr_argc, so put the message at parm 1 and set argc.
        let s = vm.intern("hello");
        vm.set_gi(OFS_PARM0 + 3, s); // parameter index 1
        let out_before = vm.output().len();
        vm.call_builtin(73, 2).expect("centerprint via table #73");
        assert!(vm.output().len() > out_before, "centerprint should write output");
        // #72 (cvar_set) is an inert fixme and must fault, not print.
        assert!((table[72])(&mut vm).is_err(), "#72 should be an inert fixme");
    }
}
