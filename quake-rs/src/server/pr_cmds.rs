//! The engine builtins that reach into the world, and the `pr_builtin[]`
//! table install.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/pr_cmds.c` — `PF_setorigin`, `PF_setsize` /
//! `SetMinMaxSize`, `PF_setmodel`, `PF_precache_sound` / `_model` / `_file`,
//! `PF_makevectors`, `PF_traceline`, `PF_pointcontents`, `PF_droptofloor`,
//! `PF_cvar`, `PF_cvar_set`, `PF_changeyaw`, `PF_aim`, `PF_checkclient`,
//! `PF_findradius`, and the `pr_builtin[]` table (non-`QUAKE2` build) that
//! [`install_engine_builtins`] fills.
//!
//! pr_cmds.c's self-contained builtins (`ftos`, `vlen`, `spawn`, `find`, …)
//! are the crate's `builtins.rs`, whose table this one starts from. The other
//! engine builtins live beside the state they drive: `sound` / `particle` /
//! the prints / `Write*` in `msg.rs`, `lightstyle` in `lightstyle.rs`,
//! `changelevel` / `localcmd` in `host.rs`, `walkmove` / `movetogoal` /
//! `checkbottom` in `sv_move.rs`.

use super::host::{ServerCvars, bi_changelevel, bi_localcmd};
use super::lightstyle::bi_lightstyle;
use super::msg::{
    bi_ambientsound, bi_bprint, bi_centerprint, bi_particle, bi_sound, bi_sprint, bi_stuffcmd, bi_writeangle,
    bi_writebyte, bi_writechar, bi_writecoord, bi_writeentity, bi_writelong, bi_writeshort, bi_writestring,
};
use super::pr_edict::parse_float;
use super::sv_move::{bi_checkbottom, bi_movetogoal, bi_walkmove};
use super::sv_world::{link_edict, sv_move};
use super::{EntFlags, SV_MAXVELOCITY, Solid, StaticEntity};
use crate::Result;
use crate::math::{Vec3, add as v_add, angle_vectors, sub as v_sub};
use crate::vm::{Builtin, Vm};

// ---------------------------------------------------------------------------
// Engine builtins. Each is an `fn(&mut Vm) -> Result<()>`.
//
// Fields and globals are read through the handles resolved at load (`vm.fo()`,
// `vm.go()`). World services are reached via `vm.with_host(...)`, which must
// NOT be held across `vm.execute`.
// ---------------------------------------------------------------------------

/// `PF_setorigin` (#2): `void(entity e, vector o) setorigin`. Sets the origin
/// and recomputes `absmin`/`absmax` from `origin + mins` / `origin + maxs`
/// (the part of `SV_LinkEdict` that matters without the area grid).
fn bi_setorigin(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let o = vm.arg_vector(1);
    vm.set_ent_vec(e, vm.fo().origin, o);
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
    vm.set_ent_vec(e, vm.fo().mins, min);
    vm.set_ent_vec(e, vm.fo().maxs, max);
    vm.set_ent_vec(e, vm.fo().size, v_sub(max, min));
    link_edict(vm, e);
}

/// `PF_setmodel` (#3): `void(entity e, string m) setmodel`. Sets `model`,
/// resolves `modelindex` via the host precache, and copies the model's
/// bounds into `mins`/`maxs`/`size` (`SetMinMaxSize (e, mod->mins,
/// mod->maxs, true)`): a `"*N"` submodel's, or what `Mod_LoadModel` gave the
/// model file (±16 for an alias model, ±maxwidth/2 for a sprite).
fn bi_setmodel(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let m = vm.arg_string(1);

    // model field = the string_t of the argument (the C did `m - pr_strings`,
    // i.e. it kept the same string_t). Re-intern to be safe across heaps.
    vm.set_ent_string(e, vm.fo().model, &m);

    // modelindex = host.precache_model(m); also fetch its bounds if it's a
    // brush submodel. Take the host only briefly (no execute() inside).
    let (idx, bbox) = vm
        .with_host(|_vm, h| {
            let idx = h.precache_model(&m);
            let bbox = h.model_bbox(&m);
            (idx, bbox)
        })
        .unwrap_or((0, None));

    vm.set_ent_float(e, vm.fo().modelindex, idx as f32);

    // SetMinMaxSize(e, mod->mins, mod->maxs); `if (!mod)` the C used a zero
    // box (here also a model the host could not resolve: no pak in tests).
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
    vm.set_glob_vec(vm.go().v_forward, forward);
    vm.set_glob_vec(vm.go().v_right, right);
    vm.set_glob_vec(vm.go().v_up, up);
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

    vm.set_glob_float(vm.go().trace_allsolid, tr.allsolid as i32 as f32);
    vm.set_glob_float(vm.go().trace_startsolid, tr.startsolid as i32 as f32);
    vm.set_glob_float(vm.go().trace_fraction, tr.fraction);
    vm.set_glob_float(vm.go().trace_inwater, tr.inwater as i32 as f32);
    vm.set_glob_float(vm.go().trace_inopen, tr.inopen as i32 as f32);
    vm.set_glob_vec(vm.go().trace_endpos, tr.endpos);
    vm.set_glob_vec(vm.go().trace_plane_normal, tr.plane_normal);
    vm.set_glob_float(vm.go().trace_plane_dist, tr.plane_dist);
    // trace_ent = the hit edict; a clear move (ent == -1) resolves to the world.
    vm.set_glob_int(vm.go().trace_ent, if tr.ent < 0 { 0 } else { tr.ent });
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
    let ent = vm.glob_int(vm.go().self_);

    let origin = vm.ent_vec(ent, vm.fo().origin);
    let mins = vm.ent_vec(ent, vm.fo().mins);
    let maxs = vm.ent_vec(ent, vm.fo().maxs);
    let end: Vec3 = [origin[0], origin[1], origin[2] - 256.0];

    // PF_droptofloor (pr_cmds.c) uses the ENTITY-AWARE SV_Move (not the
    // world-only host trace), so the entity can come to rest on a door/plat or
    // another solid edict, and sets groundentity to whatever it landed on.
    let tr = sv_move(vm, origin, end, mins, maxs, ent, false, false);

    if tr.fraction == 1.0 || tr.allsolid {
        vm.ret_float(0.0);
    } else {
        vm.set_ent_vec(ent, vm.fo().origin, tr.endpos);
        link_edict(vm, ent);
        let flags = vm.flags(ent);
        vm.set_flags(ent, flags.with(EntFlags::ONGROUND));
        // groundentity = EDICT_TO_PROG(trace.ent): the resolved edict it rests
        // on (0 = world, >0 = that edict). The hit branch only runs when
        // fraction < 1, so trace.ent is never the "nothing hit" sentinel (-1).
        vm.set_ent_int(ent, vm.fo().groundentity, tr.ent.max(0));
        vm.ret_float(1.0);
    }
    Ok(())
}

/// `PF_cvar` (#45): `float(string name) cvar`. Returns the known server-cvar
/// defaults; everything else is 0 (the C looked these up in the cvar registry).
fn bi_cvar(vm: &mut Vm) -> Result<()> {
    let name = vm.arg_string(0);
    let cvars = vm.host().map(|h| *h.cvars()).unwrap_or_default();
    vm.ret_float(cvar_value(&cvars, &name));
    Ok(())
}

/// `finaleFinished` (#79, the mission packs' 2021 re-release only): whether
/// the end-of-pack finale/credits text has been fully shown and the player
/// has pressed a button since ([`ServerCvars::finale_finished`], latched by
/// [`super::Server::set_finale_finished`] — a front-end sets it from
/// client-side state, `client::screen::finale_text_fully_revealed`, since
/// single-player keeps server and client in one process). `client.qc`'s
/// `finale_check` polls this every 0.1s and, once true, runs `menu_credits`
/// then `disconnect` five seconds later. `id1`'s progs never declares this
/// builtin, so it is never called for it (AUDIT.md "The mission packs'
/// paths", P7/B4).
fn bi_finale_finished(vm: &mut Vm) -> Result<()> {
    let finished = vm.host().map(|h| h.cvars().finale_finished).unwrap_or(false);
    vm.ret_float(if finished { 1.0 } else { 0.0 });
    Ok(())
}

/// The handful of cvar defaults the spawn/think code reads. Values match the
/// stock `*.c` declarations (`deathmatch` "0"). `skill` and `sv_gravity` are
/// the server's *live* values ([`ServerCvars`]): `cvar_set("skill", N)` from a
/// difficulty portal updates it and `cvar("skill")` reads it back, so the
/// QuakeC sees the difficulty it selected. `registered` is the search path's
/// (`COM_CheckRegistered`).
pub(super) fn cvar_value(cvars: &ServerCvars, name: &str) -> f32 {
    match name {
        "sv_gravity" => cvars.sv_gravity,
        "sv_maxvelocity" => SV_MAXVELOCITY,
        "deathmatch" | "coop" | "teamplay" => 0.0,
        "skill" => cvars.skill as f32,
        "registered" => f32::from(u8::from(cvars.registered)),
        _ => 0.0,
    }
}

/// `PF_cvar_set` (#72): `void(string var, string val) cvar_set`. The C calls
/// `Cvar_Set(var, val)`. This headless port has no cvar registry; the cvars the
/// id1 progs set have live backing stores in `host.rs`: `skill` (the start-map
/// difficulty portals, `trigger_setskill` -> `cvar_set("skill", N)`) and
/// `sv_gravity` (world.qc `worldspawn`: 100 on e1m8, 800 elsewhere). Any other
/// name is a benign no-op.
fn bi_cvar_set(vm: &mut Vm) -> Result<()> {
    let name = vm.arg_string(0);
    let value = parse_float(&vm.arg_string(1));
    vm.with_host(|_, h| {
        let cvars = h.cvars_mut();
        match name.as_str() {
            "skill" => cvars.set_skill(value),
            "sv_gravity" => cvars.sv_gravity = value,
            _ => {}
        }
    });
    Ok(())
}

/// `PF_changeyaw` (#49): turn `self.angles[1]` toward `ideal_yaw` by at most
/// `yaw_speed`. A faithful port of the C (which converted this from QuakeC for
/// speed); harmless for non-monster entities (`yaw_speed == 0` => no turn).
pub(super) fn bi_changeyaw(vm: &mut Vm) -> Result<()> {
    let ent = vm.glob_int(vm.go().self_);
    let angles = vm.ent_vec(ent, vm.fo().angles);
    let current = crate::math::anglemod(angles[1]);
    let ideal = vm.ent_float(ent, vm.fo().ideal_yaw);
    let speed = vm.ent_float(ent, vm.fo().yaw_speed);

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
    vm.set_ent_vec(ent, vm.fo().angles, new_angles);
    Ok(())
}

/// A benign no-op builtin: consumes its arguments and returns nothing. Used for
/// the remaining network / client-routing builtins that have no world effect in
/// this headless server (`setspawnparms`). (`makestatic` sends the edict's
/// `svc_spawnstatic` via [`bi_makestatic`]; `stuffcmd` queues
/// its text via [`bi_stuffcmd`]; `sound` queues a
/// [`SoundEvent`] via [`bi_sound`]; `ambientsound` records a [`StaticSound`]
/// via [`bi_ambientsound`]; `particle` queues a [`ParticleBurst`] via
/// [`bi_particle`]; the `Write*` family (#52..#59) feeds the per-buffer svc
/// parsers in `msg.rs`.)
fn bi_noop(_vm: &mut Vm) -> Result<()> {
    Ok(())
}

/// `PF_makestatic` (#69): `void(entity e) makestatic`. Writes the edict's
/// `svc_spawnstatic` (model, frame, skin, origin, angles: a
/// [`StaticEntity`], as id's client read it) into the signon and frees the
/// edict — "throw the entity away now" — so the next `spawn()` reuses its
/// slot, as id's `ED_Alloc` does. The client draws the record through
/// efrags, never relinked: no trail, no `EF_*` light, no spin.
/// (`SV_ModelIndex`'s `Sys_Error` for a model never precached is not
/// modelled: this port's `setmodel` precaches what it is given.)
fn bi_makestatic(vm: &mut Vm) -> Result<()> {
    let e = vm.arg_entity(0);
    let st = StaticEntity::of_edict(vm, e);
    vm.with_host(|_, h| h.outbox().statics.push(st));
    vm.free_edict(e);
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

    let v_forward = vm.glob_vec(vm.go().v_forward);
    let origin = vm.ent_vec(ent, vm.fo().origin);
    let mut start = origin;
    start[2] += 20.0;

    // Try a straight trace first; a direct DAMAGE_AIM hit needs no assist.
    let end = [start[0] + 2048.0 * v_forward[0], start[1] + 2048.0 * v_forward[1], start[2] + 2048.0 * v_forward[2]];
    let tr = sv_move(vm, start, end, [0.0; 3], [0.0; 3], ent, false, false);
    if tr.ent > 0 && vm.ent_float(tr.ent, vm.fo().takedamage) == DAMAGE_AIM {
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
        if vm.ent_float(check, vm.fo().takedamage) != DAMAGE_AIM {
            continue;
        }
        let c_org = vm.ent_vec(check, vm.fo().origin);
        let c_min = vm.ent_vec(check, vm.fo().mins);
        let c_max = vm.ent_vec(check, vm.fo().maxs);
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
        let b_org = vm.ent_vec(bestent, vm.fo().origin);
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
    let self_e = vm.glob_int(vm.go().self_);

    // Find a live client edict by its FL_CLIENT flag (the C scanned svs.clients;
    // real progs.dat has no `viewentity` global, so we can't rely on that). A
    // dead client (health <= 0) is not a valid target, matching the C.
    let mut player = 0i32;
    for e in 1..vm.num_edicts() {
        let ent = e as i32;
        if vm.is_free_edict(ent) {
            continue;
        }
        if vm.flags(ent).contains(EntFlags::CLIENT) && vm.ent_float(ent, vm.fo().health) > 0.0 {
            player = ent;
            break;
        }
    }
    if player <= 0 {
        vm.ret_entity(0);
        return Ok(());
    }

    // Eyes: origin + view_ofs for both ends of the sight line.
    let self_org = vm.ent_vec(self_e, vm.fo().origin);
    let self_ofs = vm.ent_vec(self_e, vm.fo().view_ofs);
    let view = v_add(self_org, self_ofs);

    let pl_org = vm.ent_vec(player, vm.fo().origin);
    let pl_ofs = vm.ent_vec(player, vm.fo().view_ofs);
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
        if vm.is_free_edict(ei) {
            continue;
        }
        if vm.solid(ei) == Solid::Not {
            continue;
        }
        let origin = vm.ent_vec(ei, vm.fo().origin);
        let mins = vm.ent_vec(ei, vm.fo().mins);
        let maxs = vm.ent_vec(ei, vm.fo().maxs);
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
        vm.set_ent_int(ei, vm.fo().chain, chain);
        chain = ei;
    }

    vm.ret_entity(chain);
    Ok(())
}

/// Install the engine builtins over the pure-builtin table from
/// [`crate::builtins::default_builtins`], keeping the self-contained ones
/// (`ftos`/`vtos`/`vlen`/`normalize`/`rint`/`floor`/`ceil`/`fabs`/`random`/
/// `spawn`/`remove`/`find`/`nextent`/`error`/`objerror`/`print`/`dprint`/
/// `vectoyaw`/`vectoangles`) intact.
///
/// Numbers are the `pr_builtin[]` indices from `pr_cmds.c` (non-`QUAKE2` build).
pub fn install_engine_builtins(vm: &mut Vm) {
    let mut put = |n: usize, f: Builtin| vm.set_builtin(n, f);

    put(1, bi_makevectors); // makevectors
    put(2, bi_setorigin); // setorigin
    put(3, bi_setmodel); // setmodel
    put(4, bi_setsize); // setsize
    put(8, bi_sound); // sound (queues a SoundEvent)
    put(16, bi_traceline); // traceline
    put(17, bi_checkclient); // checkclient (line-of-sight to the player)
    put(19, bi_precache_sound); // precache_sound
    put(20, bi_precache_model); // precache_model
    put(21, bi_stuffcmd); // stuffcmd -> svc_stufftext queue
    put(22, bi_findradius); // findradius (chain of edicts within rad)
    put(23, bi_bprint); // bprint -> on-screen notify line
    put(24, bi_sprint); // sprint -> on-screen notify line
    put(73, bi_centerprint); // centerprint -> centered transient message
    put(32, bi_walkmove); // walkmove (SV_movestep)
    put(34, bi_droptofloor); // droptofloor
    put(35, bi_lightstyle); // lightstyle (stores sv.lightstyles[style])
    put(40, bi_checkbottom); // checkbottom (SV_CheckBottom)
    put(41, bi_pointcontents); // pointcontents
    put(44, bi_aim); // aim
    put(45, bi_cvar); // cvar
    put(48, bi_particle); // particle (queues a ParticleBurst)
    put(49, bi_changeyaw); // changeyaw

    // #52..#59: the network Write* family. These feed one svc parser per message
    // buffer (MSG_BROADCAST, MSG_ALL) -> TempEntityEvents + SvcEvents; see msg.rs.
    put(52, bi_writebyte); // WriteByte
    put(53, bi_writechar); // WriteChar
    put(54, bi_writeshort); // WriteShort
    put(55, bi_writelong); // WriteLong
    put(56, bi_writecoord); // WriteCoord
    put(57, bi_writeangle); // WriteAngle
    put(58, bi_writestring); // WriteString
    put(59, bi_writeentity); // WriteEntity

    put(46, bi_localcmd); // localcmd (honours restart / changelevel / map; else no-op)
    put(67, bi_movetogoal); // movetogoal (SV_MoveToGoal)
    put(68, bi_precache_file); // precache_file
    put(69, bi_makestatic); // makestatic (an svc_spawnstatic; frees the edict)
    put(70, bi_changelevel); // changelevel (records the deferred map swap)
    put(72, bi_cvar_set); // cvar_set (honours "skill"; else benign no-op)
    put(74, bi_ambientsound); // ambientsound (records a StaticSound loop)
    put(75, bi_precache_model); // precache_model (alias)
    put(76, bi_precache_sound); // precache_sound (alias)
    put(77, bi_precache_file); // precache_file (alias)
    put(78, bi_noop); // setspawnparms
    put(79, bi_finale_finished); // finaleFinished (mission packs' re-release only)
    // #80 localsound stays pf_fixme: declared by both packs, called by
    // neither (AUDIT.md "The mission packs' paths", P7/B4) — left failing at
    // the call, like id's own PR_RunError would.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{OFS_RETURN, Op, Progs, Statement};
    use crate::server::testutil::*;
    use crate::server::{Server, WorldModel};

    // ---------------------------------------------------------- makestatic

    /// PF_makestatic writes the edict's svc_spawnstatic — the bytes, and the
    /// origin and angles as MSG_WriteCoord/MSG_WriteAngle sent them — and
    /// frees the edict ("throw the entity away now"), so the next ED_Alloc
    /// takes the slot at once (freed at sv.time 1, inside the first two
    /// seconds' relaxed policy).
    #[test]
    fn makestatic_records_the_wire_static_and_frees_the_edict() {
        let mut b = Builder::new();
        b.add_field("model", EV_STRING, 1);
        b.add_field("frame", EV_FLOAT, 2);
        b.add_field("skin", EV_FLOAT, 3);
        b.add_field("origin", EV_VECTOR, 4);
        b.add_field("angles", EV_VECTOR, 7);
        b.entityfields = 10;
        let mut server = Server::new(empty_bsp(), Progs::parse(&b.build()).expect("parse")).expect("server");
        let e = server.vm.spawn();
        server.vm.ent_set_string(e, "model", "progs/flame.mdl");
        server.vm.ent_set_float(e, "frame", 3.7);
        server.vm.ent_set_float(e, "skin", 1.0);
        server.vm.ent_set_vector(e, "origin", [100.3, -50.06, 24.0]);
        server.vm.ent_set_vector(e, "angles", [0.0, 270.0, 10.9]);
        server.vm.set_gi(crate::progs::OFS_PARM0, e);
        server.vm.call_builtin(69, 1).expect("makestatic");
        assert!(server.vm.is_free_edict(e), "ED_Free (ent)");
        server.apply_statics();
        let wire = StaticEntity {
            model: "progs/flame.mdl".into(),
            frame: 3,
            skin: 1,
            origin: [100.25, -50.0, 24.0],
            angles: [0.0, -90.0, 7.0 * 360.0 / 256.0],
        };
        assert_eq!(server.statics(), [wire]);
        assert_eq!(server.vm.spawn(), e, "the next spawn takes the freed slot");
    }

    // ------------------------------------------------------------ objerror

    /// PF_objerror (pr_cmds.c) prints `======OBJECT ERROR in <function>:`, the
    /// text and `ED_Print (self)`, frees `self`, and calls Host_Error — the
    /// error start.bsp's unreachable teleporter raises in the census (L16).
    #[test]
    fn objerror_dumps_and_frees_self_then_is_host_error() {
        let (img, _touch_fn, _g_one, _g_flag) = touch_progs();
        let mut server = Server::new(empty_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        let e = server.vm.spawn();
        // (classname is this progs' field def 0, which ED_Print's loop skips
        // as id's does: `for (i=1 ; i<progs->numfielddefs ; i++)`.)
        server.vm.ent_set_string(e, "classname", "trigger_teleport");
        server.vm.ent_set_string(e, "model", "*9");
        server.vm.ent_set_vector(e, "origin", [10.0, -20.5, 0.0]);
        server.vm.gset_int("self", e);
        let text = server.vm.intern("couldn't find target");
        server.vm.set_gi(crate::progs::OFS_PARM0, text);
        let Err(crate::QError::Program(err)) = server.vm.call_builtin(11, 1) else { panic!("objerror is Host_Error") };
        assert_eq!(
            err.console,
            format!(
                "======OBJECT ERROR in :\ncouldn't find target\n\nEDICT {e}:\n\
                 origin         ' 10.0 -20.5   0.0'\nmodel          *9\n"
            )
        );
        assert!(server.vm.is_free_edict(e), "ED_Free (ed) before Host_Error");
    }

    // ------------------------------------------------------------ cvar / skill

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
        server.vm.set_gi(crate::progs::OFS_PARM0, name);
        server.vm.set_gi(crate::progs::OFS_PARM1, val);
        server.vm.call_builtin(72, 2).expect("cvar_set");
        assert_eq!(server.skill(), 2, "cvar_set('skill','2') stored 2");

        // cvar("skill") reads the live value back.
        let name = server.vm.intern("skill");
        server.vm.set_gi(crate::progs::OFS_PARM0, name);
        server.vm.call_builtin(45, 1).expect("cvar");
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

    /// `cvar("registered")`: the search path the server reads through is
    /// registered — `gfx/pop.lmp` is id's (`COM_CheckRegistered`) — or not.
    #[test]
    fn cvar_registered_is_the_search_paths() {
        let cvar = |pak: Option<crate::pak::Pak>| {
            let (img, _marker, _g_one) = marker_progs();
            let progs = Progs::parse(&img).expect("parse");
            let mut server = Server::with_pak(bsp_with_entities("{ }"), progs, pak).expect("server");
            let name = server.vm.intern("registered");
            server.vm.set_gi(crate::progs::OFS_PARM0, name);
            server.vm.call_builtin(45, 1).expect("cvar");
            server.vm.gf(crate::progs::OFS_RETURN)
        };
        let pak = |files: &[(&str, &[u8])]| crate::pak::Pak::from_bytes("t".into(), crate::pak::write_pack(files)).ok();
        assert_eq!(cvar(None), 0.0);
        assert_eq!(cvar(pak(&[("maps/e1m1.bsp", b"")])), 0.0, "shareware");
        let pop = crate::common::pop_lmp();
        let pak0 = pak(&[("maps/e1m1.bsp", b"")]).unwrap();
        let pak1 = pak(&[("gfx/pop.lmp", &pop)]).unwrap();
        assert_eq!(cvar(Some(pak1.over(pak0))), 1.0, "pak1 with id's pop.lmp in front of pak0");
    }

    /// Helper: invoke the engine `cvar_set` builtin with two string args.
    fn bi_cvar_set_via(server: &mut Server, var: &str, val: &str) {
        let n = server.vm.intern(var);
        let v = server.vm.intern(val);
        server.vm.set_gi(crate::progs::OFS_PARM0, n);
        server.vm.set_gi(crate::progs::OFS_PARM1, v);
        server.vm.call_builtin(72, 2).expect("cvar_set");
    }

    #[test]
    fn install_engine_builtins_overwrites_world_keeps_pure() {
        // Build a trivial progs just to get a Vm.
        let mut b = Builder::new();
        let _ = b.add_function("main", vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        let len_before = vm.builtins().len();
        install_engine_builtins(&mut vm);
        // Table length is unchanged (we only overwrite slots).
        assert_eq!(vm.builtins().len(), len_before);
        // #45 (cvar) now returns sv_gravity default for "sv_gravity".
        vm.set_host(Box::new(WorldModel::new(empty_bsp())));
        let s = vm.intern("sv_gravity");
        vm.set_gi(crate::progs::OFS_PARM0, s);
        vm.call_builtin(45, 1).expect("cvar");
        assert_eq!(vm.gf(OFS_RETURN), 800.0);
    }

    #[test]
    fn traceline_writes_globals_without_host_panic() {
        // traceline against the empty world should write the trace_* globals and
        // never panic, even with the headnode-out-of-range "everything solid".
        let mut b = Builder::new();
        b.add_global("trace_fraction", EV_FLOAT, 50);
        b.add_global("trace_allsolid", EV_FLOAT, 51);
        b.add_global("trace_endpos", 3, 52);
        let _ = b.add_function("main", vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }]);
        let img = b.build();
        let mut vm = Vm::load(&img).expect("load");
        install_engine_builtins(&mut vm);
        vm.set_host(Box::new(WorldModel::new(empty_bsp())));
        vm.set_gv(crate::progs::OFS_PARM0, [0.0, 0.0, 0.0]);
        vm.set_gv(crate::progs::OFS_PARM0 + 3, [100.0, 0.0, 0.0]);
        vm.call_builtin(16, 4).expect("traceline");
        // fraction in [0,1].
        let frac = vm.gget_float("trace_fraction");
        assert!((0.0..=1.0).contains(&frac));
    }

    /// `finaleFinished` (#79): false on a fresh server, latches true once
    /// [`Server::set_finale_finished`] sees it, and stays latched — a later
    /// `false` (what a frame where the condition no longer holds would pass)
    /// is not a reset, matching `finale_check`'s own one-shot need (AUDIT.md
    /// "The mission packs' paths", P7/B4).
    #[test]
    fn finale_finished_builtin_reads_the_latch() {
        let img = Builder::new().build();
        let mut server = Server::new(empty_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        assert!(!server.finale_finished(), "a fresh server has not latched it");
        server.vm.call_builtin(79, 0).expect("builtin #79 is installed");
        assert_eq!(server.vm.gf(OFS_RETURN), 0.0, "false by default");

        server.set_finale_finished(true);
        assert!(server.finale_finished());
        server.vm.call_builtin(79, 0).expect("builtin #79");
        assert_eq!(server.vm.gf(OFS_RETURN), 1.0, "latched true");

        // A `false` input (what a frame where the reveal/keypress condition
        // no longer holds would pass) is not a reset -- only a fresh server
        // (a changelevel/restart) clears it.
        server.set_finale_finished(false);
        assert!(server.finale_finished(), "a false input does not clear the latch");
    }

    /// Hipnotic's hipend ending, on real data: `#[ignore]`d (needs the
    /// mission pack's own `progs.dat`/`hipend.bsp`, which this repo does not
    /// carry — see `AUDIT.md` "The mission packs' paths", P7/B4, and
    /// `pr_edict.rs`'s `r2m6_needs_more_than_ids_600_edicts` for the same
    /// pattern). Point `QUAKE_HIPNOTIC_DIR` at a *basedir* laid out as
    /// `census/packs.py --data` wants it (`id1/pak0.pak`, `id1/pak1.pak`,
    /// `hipnotic/pak0.pak`) and run `cargo test --release hipend_ending --
    /// --ignored`. Goes through the real `-hipnotic` file layering
    /// ([`crate::common::init_filesystem`]), not a bare `progs.dat`/`.bsp`
    /// pair, so `cvar("registered")` reads true exactly as real play would
    /// (id1's `pak1.pak` carries `gfx/pop.lmp`) — the hipend branch at
    /// `intermission_running == 3` needs it, else it takes the shareware
    /// sell-screen branch instead and never reaches `finale_check`.
    ///
    /// Plays the real chain end to end exactly as the map's own QuakeC does
    /// (no shortcuts past it): hipend's ending runs through its one
    /// `info_startendtext` (hipmisc.qc's `info_startendtext_use`, reached in
    /// play once the player sits through its `effect_finale` camera chain —
    /// out of this brief's scope, P5/the oriented-sprite and camera work),
    /// whose `.use` sets `intermission_running = 1` and calls
    /// `ExitIntermission()` directly — already `intermission_running == 2`
    /// by the time it returns (`ExitIntermission` increments it again on
    /// entry), hipend's own finale text written. From there tick real
    /// frames with `+attack` held, as a player mashing "continue" would, so
    /// `IntermissionThink` (run every frame by the real `PlayerPreThink`)
    /// drives one more `ExitIntermission` call once its `intermission_exittime`
    /// gate opens — `intermission_running` 2->3, hipend's own branch, which
    /// spawns the `finale_check` timer. Before #79 this point was a QuakeC
    /// "bad builtin call number 79" error, ending the game; confirm instead
    /// it idles cleanly (`finaleFinished()` answering false) until
    /// [`Server::set_finale_finished`] (what `client/cl_main.rs`'s
    /// `walk_frame` does once the finale text is fully shown and a button
    /// pressed) lets `finale_check` -> `finale_transition` queue
    /// `menu_credits` ([`Server::take_pending_menu_credits`]).
    #[test]
    #[ignore]
    fn hipend_ending_runs_finale_check_to_menu_credits() {
        let Ok(dir) = std::env::var("QUAKE_HIPNOTIC_DIR") else {
            eprintln!("QUAKE_HIPNOTIC_DIR not set; skipping (see this test's doc comment)");
            return;
        };
        let fs = crate::common::init_filesystem(std::path::Path::new(&dir), &["hipnotic"], false)
            .expect("basedir laid out as id1/ + hipnotic/, each with its pak0.pak (+ id1/pak1.pak)");
        assert!(fs.registered, "id1's pak1.pak must be present for cvar(\"registered\") to read true");
        let read = |name: &str| -> Vec<u8> {
            fs.files.read_file(name).expect("read").unwrap_or_else(|| panic!("{name} not found"))
        };
        let progs = Progs::parse(&read("progs.dat")).expect("parse progs.dat");
        let bsp = crate::bsp::Bsp::parse(&read("maps/hipend.bsp")).expect("parse hipend.bsp");

        let mut server = Server::with_pak(bsp, progs, Some(fs.files)).expect("server");
        server.set_map_name("hipend");
        server.spawn_entities().expect("hipend spawns clean");
        let _player = server.connect_client().expect("connect");
        server.run_signon_frames().expect("signon settles");

        // The map's one info_startendtext: its `.use` is info_startendtext_use.
        let starter = (0..server.vm.num_edicts() as i32)
            .find(|&e| !server.vm.is_free_edict(e) && server.vm.ent_get_string(e, "classname") == "info_startendtext")
            .expect("hipend has an info_startendtext");

        server.vm.gset_int("self", starter);
        server.vm.call_by_name("info_startendtext_use").expect("info_startendtext_use");
        assert_eq!(server.vm.gget_float("intermission_running"), 2.0, "ExitIntermission ran once already");

        let cmd = crate::server::UserCmd {
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            buttons: 1, // +attack: IntermissionThink's "a button": self.button0.
            impulse: 0,
        };
        // intermission_exittime is `time + 1` (ExitIntermission's own TRUE,
        // non-deathmatch); held attack then drives IntermissionThink's one
        // more ExitIntermission call (running 2->3) the instant the gate
        // opens. 8 sim-seconds is generous slack.
        for _ in 0..80 {
            server.client_frame_f64(&cmd, 0.1).expect("frame (pre-latch)");
        }
        assert_eq!(server.vm.gget_float("intermission_running"), 3.0, "hipend's own branch spawned finale_check");
        assert!(!server.finale_finished(), "nothing has latched it yet");
        assert!(!server.take_pending_menu_credits(), "finale_check is still polling false");

        // Keep ticking with #79 still false: this is exactly the point that
        // used to error "bad builtin call number 79" -- a clean `.expect`
        // across many more polls (every 0.1s) is the regression proof.
        for _ in 0..30 {
            server.client_frame_f64(&cmd, 0.1).expect("finale_check polls #79 cleanly");
        }
        assert!(!server.take_pending_menu_credits(), "still not latched");

        // The player has "seen the text and pressed a key" (what
        // `walk_frame` computes from `finale_text_fully_revealed` + a
        // button): latch it. finale_check sees it true within 0.1s and
        // schedules finale_transition 5s later.
        server.set_finale_finished(true);
        for _ in 0..70 {
            // 7 sim-seconds: >= the 5s wait, plus slack for when exactly
            // finale_check's own think next lands.
            if server.take_pending_menu_credits() {
                return; // menu_credits queued -- the chain completed.
            }
            server.client_frame_f64(&cmd, 0.1).expect("frame (post-latch)");
        }
        assert!(server.take_pending_menu_credits(), "finale_transition queued menu_credits");
    }

    /// Rogue's own ending chain (`oldone.qc`'s `finale_5`/`finale_6`,
    /// reached in play once the final boss (`th_die = finale_1`) is killed
    /// and its cutscene chain -- `finale_1..4`'s teleport-train choreography,
    /// out of this brief's scope -- finishes) on real data: `#[ignore]`d
    /// (needs the mission pack's own `progs.dat`; see `AUDIT.md` "The
    /// mission packs' paths", P7/B4). Point `QUAKE_ROGUE_DIR` at a directory
    /// holding it and run `cargo test --release r2m8_ending -- --ignored`.
    ///
    /// `finale_5`/`finale_6` need no map state at all (unlike Hipnotic's
    /// `ending.qc`'s `finale_check`, no `cvar("registered")` check), so this
    /// calls `finale_5` directly on a throwaway edict standing in for
    /// `finale_4`'s own `timer`, then ticks real frames
    /// (`MoveType::None`'s generic `SV_RunThink`) to drive its 0.1s poll --
    /// otherwise the same proof as `hipend_ending_runs_finale_check_to_menu_credits`,
    /// cross-checking the same builtin/outbox plumbing against Rogue's own
    /// compiled progs.dat, not just Hipnotic's.
    #[test]
    #[ignore]
    fn r2m8_ending_runs_finale_5_to_menu_credits() {
        let Ok(dir) = std::env::var("QUAKE_ROGUE_DIR") else {
            eprintln!("QUAKE_ROGUE_DIR not set; skipping (see this test's doc comment)");
            return;
        };
        let progs_bytes = std::fs::read(format!("{dir}/progs.dat")).expect("progs.dat");
        let progs = Progs::parse(&progs_bytes).expect("parse progs.dat");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        let timer = server.vm.spawn(); // stands in for finale_4's own `timer`
        // finale_4's real setup (`timer.think = finale_5`) before the first
        // call, since finale_5's own "poll false" branch only ever touches
        // `nextthink`, trusting `think` is already itself.
        let finale_5_idx = server.vm.progs().find_function("finale_5").expect("finale_5 declared");
        server.vm.set_ent_int(timer, server.vm.fo().think, finale_5_idx as i32);
        server.vm.gset_int("self", timer);
        server.vm.call_by_name("finale_5").expect("finale_5");

        let cmd = crate::server::UserCmd {
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            buttons: 0,
            impulse: 0,
        };
        for _ in 0..20 {
            server.client_frame_f64(&cmd, 0.1).expect("finale_5 polls #79 cleanly (pre-latch)");
        }
        assert!(!server.take_pending_menu_credits(), "not yet -- finaleFinished() hasn't latched");

        // "Seen the text and pressed a key": latch it, as `walk_frame` would.
        server.set_finale_finished(true);
        for _ in 0..70 {
            if server.take_pending_menu_credits() {
                return; // menu_credits queued -- the chain completed.
            }
            server.client_frame_f64(&cmd, 0.1).expect("frame (post-latch)");
        }
        assert!(server.take_pending_menu_credits(), "finale_6 queued menu_credits");
    }
}
