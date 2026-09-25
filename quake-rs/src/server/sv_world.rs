//! Entity-aware collision: `SV_Move` against the world and every solid edict,
//! the bounds half of `SV_LinkEdict`, trigger touching and `SV_Impact`.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/world.c` — `SV_Move`, `SV_ClipToLinks`, `SV_ClipMoveToEntity`,
//!   `SV_LinkEdict` (bounds), `SV_TouchLinks`.
//! * `WinQuake/sv_phys.c` — `SV_Impact`.
//!
//! The BSP hull traces underneath (`SV_RecursiveHullCheck`, the box hull,
//! point contents) are the crate's `world.rs`; this module adds the edicts.

use super::{
    CONTENTS_SOLID, FL_ITEM, FL_MONSTER, SOLID_BBOX, SOLID_BSP, SOLID_NOT, SOLID_SLIDEBOX,
    SOLID_TRIGGER,
};
use crate::math::{add as v_add, Vec3};
use crate::vm::{EdictLeafs, HostTrace, Vm};

// ---------------------------------------------------------------------------
// Entity-aware move, impact, and trigger touching.
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

/// `SV_LinkEdict` without the area grid: the abs box (`absmin = origin +
/// mins`, `absmax = origin + maxs`, widened below) and the PVS leaves
/// (`SV_FindTouchedLeafs`, see [`find_touched_leafs`]). (The C also inserted
/// the edict into the area grid and touched triggers; neither is modelled
/// here.) `pub(crate)` so the savegame loader (`save.rs`) can relink loaded
/// edicts exactly as `Host_Loadgame_f` does (`SV_LinkEdict(ent, false)`).
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
    find_touched_leafs(vm, e, absmin, absmax);
}

/// The PVS half of `SV_LinkEdict` (world.c): `ent->num_leafs = 0; if
/// (ent->v.modelindex) SV_FindTouchedLeafs (ent, sv.worldmodel->nodes);` —
/// record up to [`MAX_ENT_LEAFS`] non-solid world leaves the abs box touches,
/// which `SV_WriteEntitiesToClient` tests against the client's fat PVS. The C
/// returns before this for the world and for a free edict, leaving their
/// leaves as they were. Without a host (unit-test VMs) nothing is recorded.
///
/// [`MAX_ENT_LEAFS`]: crate::vm::MAX_ENT_LEAFS
fn find_touched_leafs(vm: &mut Vm, e: i32, absmin: Vec3, absmax: Vec3) {
    if e <= 0 || vm.is_free_edict(e) {
        return;
    }
    let mut leafs = EdictLeafs::default();
    if vm.ent_get_float(e, "modelindex") != 0.0 {
        if let Some(host) = vm.host.as_deref() {
            host.bsp().touched_leafs(absmin, absmax, &mut |leaf| leafs.push(leaf));
        }
    }
    vm.set_edict_leafs(e, leafs);
}

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

/// Sample the world point-contents at `p` (the [`Host`](crate::vm::Host)-backed
/// `SV_PointContents` the builtins use): `CONTENTS_EMPTY` (-1), `SOLID` (-2), `WATER` (-3),
/// `SLIME` (-4), `LAVA` (-5), etc. Exposed for tooling/tests (e.g. probing where
/// a liquid is); `CONTENTS_SOLID` if there is no host.
pub fn probe_point_contents(vm: &mut Vm, p: Vec3) -> i32 {
    vm.with_host(|_vm, h| h.point_contents(p)).unwrap_or(CONTENTS_SOLID)
}

// Mirrors the C `SV_Move(start, mins, maxs, end, type, passedict)` signature (world.c).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Op, Progs, Statement};
    use crate::server::testutil::*;
    use crate::server::Server;

    // ----------------------------------------------- entity-aware move / touch

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
}
