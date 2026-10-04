//! Monster locomotion: the step and chase movement QuakeC's `ai.qc` drives.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/sv_move.c` — `SV_CheckBottom`, `SV_movestep`,
//!   `SV_StepDirection`, `SV_FixCheckBottom`, `SV_NewChaseDir`,
//!   `SV_CloseEnough`, `SV_MoveToGoal` (itself the `movetogoal` builtin).
//! * `WinQuake/pr_cmds.c` — `PF_walkmove`, `PF_checkbottom`, the builtins that
//!   front them.
//!
//! As in id's tree, the trace these steps are made of — `SV_Move` — is not
//! here but in world.c's port, [`super::sv_world`].

use super::pr_cmds::bi_changeyaw;
use super::sv_world::{link_edict, sv_move, touch_triggers};
use super::{CONTENTS_EMPTY, CONTENTS_SOLID, EntFlags};
use crate::Result;
use crate::math::{Vec3, add as v_add};
use crate::vm::Vm;
use crate::world;

/// `DI_NODIR` (sv_move.c): the "no preferred direction" sentinel used by
/// [`sv_new_chase_dir`]'s axis-direction picks.
const DI_NODIR: f32 = -1.0;

// ---------------------------------------------------------------------------
// Monster movement: the AI walk/chase steps.
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
    let origin = vm.ent_vec(ent, vm.fo().origin);
    let ent_mins = vm.ent_vec(ent, vm.fo().mins);
    let ent_maxs = vm.ent_vec(ent, vm.fo().maxs);
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
    let oldorg = vm.ent_vec(ent, vm.fo().origin);
    let ent_mins = vm.ent_vec(ent, vm.fo().mins);
    let ent_maxs = vm.ent_vec(ent, vm.fo().maxs);
    let flags = vm.flags(ent);

    // Flying / swimming monsters don't step up.
    if flags.intersects(EntFlags::SWIM | EntFlags::FLY) {
        let enemy = vm.ent_int(ent, vm.fo().enemy);
        // Try one move with vertical motion, then one without.
        for i in 0..2 {
            let mut neworg = v_add(oldorg, mov);
            if i == 0 && enemy > 0 {
                let enemy_org = vm.ent_vec(enemy, vm.fo().origin);
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
                if flags.contains(EntFlags::SWIM) {
                    let c = vm.with_host(|_vm, h| h.point_contents(tr.endpos)).unwrap_or(CONTENTS_SOLID);
                    if c == CONTENTS_EMPTY {
                        return false; // swim monster left water
                    }
                }
                vm.set_ent_vec(ent, vm.fo().origin, tr.endpos);
                if relink {
                    link_edict(vm, ent);
                    // Reached through movetogoal/walkmove DURING a monster's
                    // think; sv.time is not threaded here, so preserve the
                    // pre-fix behaviour (the prior NO-OP set `time` to its own
                    // current value). See FIX-1 notes: the physics paths get the
                    // true start-of-frame time; this monster path keeps `time`.
                    let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
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
        if flags.contains(EntFlags::PARTIALGROUND) {
            // The monster had the ground pulled out; let it fall.
            vm.set_ent_vec(ent, vm.fo().origin, v_add(oldorg, mov));
            if relink {
                link_edict(vm, ent);
                // Monster think path: keep the live `time` global (see FIX-1).
                let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
                touch_triggers(vm, ent, time);
            }
            let flags = vm.flags(ent);
            vm.set_flags(ent, flags.without(EntFlags::ONGROUND));
            return true;
        }
        return false; // walked off an edge
    }

    // Landed on something: provisionally take the new origin, then verify the
    // whole box has floor under it (dangling-corner check).
    vm.set_ent_vec(ent, vm.fo().origin, tr.endpos);

    if !sv_check_bottom(vm, ent) {
        if flags.contains(EntFlags::PARTIALGROUND) {
            // Floor mostly pulled out: keep correcting (accept the move).
            if relink {
                link_edict(vm, ent);
                // Monster think path: keep the live `time` global (see FIX-1).
                let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
                touch_triggers(vm, ent, time);
            }
            return true;
        }
        // Revert: no clean standing position.
        vm.set_ent_vec(ent, vm.fo().origin, oldorg);
        return false;
    }

    if flags.contains(EntFlags::PARTIALGROUND) {
        // Back on solid ground: clear the partial-ground flag.
        let flags = vm.flags(ent);
        vm.set_flags(ent, flags.without(EntFlags::PARTIALGROUND));
    }
    // groundentity = the edict we landed on (world = 0, an entity = its index;
    // a clear-but-landed trace resolves ent to 0/the world via MoveTrace).
    let ground = if tr.ent < 0 { 0 } else { tr.ent };
    vm.set_ent_int(ent, vm.fo().groundentity, ground);

    if relink {
        link_edict(vm, ent);
        // Monster think path: keep the live `time` global (see FIX-1).
        let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
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
    vm.set_ent_float(ent, vm.fo().ideal_yaw, yaw);
    // PF_changeyaw() turns angles[1] toward ideal_yaw by at most yaw_speed. It
    // reads `self` (as the C does), so point `self` at `ent` for the turn and
    // restore it afterwards (the C's chase chain runs with self == the monster,
    // but restoring keeps us robust if a caller drives a non-self actor).
    let oldself = vm.glob_int(vm.go().self_);
    vm.set_glob_int(vm.go().self_, ent);
    let _ = bi_changeyaw(vm);
    vm.set_glob_int(vm.go().self_, oldself);

    let rad = yaw * std::f32::consts::PI / 180.0;
    let mov: Vec3 = [rad.cos() * dist, rad.sin() * dist, 0.0];

    let oldorigin = vm.ent_vec(ent, vm.fo().origin);
    if sv_movestep(vm, ent, mov, false) {
        let angles = vm.ent_vec(ent, vm.fo().angles);
        let delta = angles[1] - vm.ent_float(ent, vm.fo().ideal_yaw);
        if delta > 45.0 && delta < 315.0 {
            // Not turned far enough: don't take the step (but report success).
            vm.set_ent_vec(ent, vm.fo().origin, oldorigin);
        }
        link_edict(vm, ent);
        // Monster think path: keep the live `time` global (see FIX-1).
        let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
        touch_triggers(vm, ent, time);
        return true;
    }
    link_edict(vm, ent);
    // Monster think path: keep the live `time` global (see FIX-1).
    let time = vm.sv_time() as f32; // SV_TouchLinks uses sv.time, not the per-think time global
    touch_triggers(vm, ent, time);
    false
}

/// `SV_FixCheckBottom` (sv_move.c ~267): mark `ent` `FL_PARTIALGROUND` so the
/// next [`sv_movestep`] tolerates a missing standing position.
fn sv_fix_check_bottom(vm: &mut Vm, ent: i32) {
    let flags = vm.flags(ent);
    vm.set_flags(ent, flags.with(EntFlags::PARTIALGROUND));
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
/// when it has no floor (via `sv_fix_check_bottom`). `rand()&n` is the VM's
/// deterministic LCG so tests are reproducible.
pub fn sv_new_chase_dir(vm: &mut Vm, actor: i32, enemy: i32, dist: f32) {
    let ideal_yaw = vm.ent_float(actor, vm.fo().ideal_yaw);
    let olddir = crate::math::anglemod(((ideal_yaw / 45.0) as i32 as f32) * 45.0);
    let turnaround = crate::math::anglemod(olddir - 180.0);

    let actor_org = vm.ent_vec(actor, vm.fo().origin);
    let enemy_org = vm.ent_vec(enemy, vm.fo().origin);
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
    if (vm.rand().chase() & 3) & 1 != 0 || deltay.abs() > deltax.abs() {
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
    if vm.rand().chase() & 1 != 0 {
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
    vm.set_ent_float(actor, vm.fo().ideal_yaw, olddir);
    if !sv_check_bottom(vm, actor) {
        sv_fix_check_bottom(vm, actor);
    }
}

/// `SV_CloseEnough` (sv_move.c ~371): is `goal`'s box within `dist` of `ent`'s
/// box on every axis? (Used by [`sv_move_to_goal`] to stop when adjacent.)
fn sv_close_enough(vm: &mut Vm, ent: i32, goal: i32, dist: f32) -> bool {
    let ent_absmin = vm.ent_vec(ent, vm.fo().absmin);
    let ent_absmax = vm.ent_vec(ent, vm.fo().absmax);
    let goal_absmin = vm.ent_vec(goal, vm.fo().absmin);
    let goal_absmax = vm.ent_vec(goal, vm.fo().absmax);
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
    let ent = vm.glob_int(vm.go().self_);
    let goal = vm.ent_int(ent, vm.fo().goalentity);

    let flags = vm.flags(ent);
    if !flags.intersects(EntFlags::ONGROUND | EntFlags::FLY | EntFlags::SWIM) {
        vm.ret_float(0.0);
        return;
    }

    // If the next step would reach the enemy goal, stop here.
    let enemy = vm.ent_int(ent, vm.fo().enemy);
    if enemy > 0 && sv_close_enough(vm, ent, goal, dist) {
        return;
    }

    // Bump around: occasionally force a fresh chase direction.
    let ideal_yaw = vm.ent_float(ent, vm.fo().ideal_yaw);
    if (vm.rand().chase() & 3) == 1 || !sv_step_direction(vm, ent, ideal_yaw, dist) {
        sv_new_chase_dir(vm, ent, goal, dist);
    }
}

// ---------------------------------------------------------------------------
// The monster-movement engine builtins (pr_cmds.c).
// ---------------------------------------------------------------------------

/// `PF_walkmove` (#32): `float(float yaw, float dist) walkmove`. Steps `self`
/// `dist` units along `yaw` via [`sv_movestep`] (with relink), returning 1 on a
/// successful move and 0 otherwise. Like the C, it only moves a monster that is
/// on ground / flying / swimming and saves/restores `self` around the step
/// (`sv_movestep` may run touch progs that change `self`).
pub(super) fn bi_walkmove(vm: &mut Vm) -> Result<()> {
    let ent = vm.glob_int(vm.go().self_);
    let yaw = vm.arg_float(0);
    let dist = vm.arg_float(1);

    let flags = vm.flags(ent);
    if !flags.intersects(EntFlags::ONGROUND | EntFlags::FLY | EntFlags::SWIM) {
        vm.ret_float(0.0);
        return Ok(());
    }

    let rad = yaw * std::f32::consts::PI / 180.0;
    let mov: Vec3 = [rad.cos() * dist, rad.sin() * dist, 0.0];

    // Save program state (self), because sv_movestep may run other progs.
    let oldself = vm.glob_int(vm.go().self_);
    let ok = sv_movestep(vm, ent, mov, true);
    vm.set_glob_int(vm.go().self_, oldself);

    vm.ret_float(if ok { 1.0 } else { 0.0 });
    Ok(())
}

/// `PF_movetogoal` (#67): `void(float step) movetogoal` — calls
/// [`sv_move_to_goal`] with the step distance. Wired over the old `bi_ret_zero`
/// stub.
pub(super) fn bi_movetogoal(vm: &mut Vm) -> Result<()> {
    let dist = vm.arg_float(0);
    sv_move_to_goal(vm, dist);
    Ok(())
}

/// `PF_checkbottom` (#40): `float(entity e) checkbottom` — returns 1 when `e`
/// has floor under its whole box ([`sv_check_bottom`]), else 0.
pub(super) fn bi_checkbottom(vm: &mut Vm) -> Result<()> {
    let ent = vm.arg_entity(0);
    let ok = sv_check_bottom(vm, ent);
    vm.ret_float(if ok { 1.0 } else { 0.0 });
    Ok(())
}
