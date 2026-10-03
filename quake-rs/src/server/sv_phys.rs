//! Per-frame physics: `SV_Physics` over every edict — thinks, the movetype
//! integrators, pushers, and the player's slide / stair-step move.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/sv_phys.c` — `SV_Physics` (as [`Server::run_frame`] and,
//! with the local client, [`Server::client_frame`]), `SV_RunThink`,
//! `SV_Physics_Pusher` / `SV_PushMove`, `SV_Physics_None` / `_Noclip` /
//! `_Step` / `_Toss`, `SV_Physics_Client`, `SV_CheckStuck`,
//! `SV_TestEntityPosition`, `SV_CheckWater`, `SV_CheckWaterTransition`,
//! `SV_AddGravity`, `SV_CheckVelocity`, `SV_PushEntity`, `SV_FlyMove`,
//! `SV_WalkMove`, `SV_WallFriction`, `SV_TryUnstick`, `ClipVelocity`.
//!
//! The collision queries are world.c's (`sv_world.rs`); the player's wish
//! velocity comes from sv_user.c's `SV_ClientThink` (`sv_user.rs`).

use super::sv_world::{link_edict, sv_impact, sv_move, touch_triggers, MoveTrace};
use super::{
    EntFlags, FrameReport, MoveType, Server, Solid, SysFn, UserCmd, CONTENTS_EMPTY, CONTENTS_SOLID,
    SV_MAXVELOCITY,
};
use crate::math::{add as v_add, angle_vectors, Vec3};
use crate::stepping::{advance_clock, Stepping};
use crate::world;
use crate::Result;

impl Server {
    /// One server frame (a stripped `SV_Physics`): advance `time` by `dt`, set
    /// `frametime`, then for each non-free edict run `SV_RunThink` and apply the
    /// minimal per-movetype physics. Returns how many thinks fired.
    ///
    /// The host is PRESENT throughout the loop (think functions reach it via
    /// `with_host`); only the brief `PushEntity` trace borrows it out.
    pub fn run_frame(&mut self, dt: f32) -> Result<FrameReport> {
        self.run_frame_f64(f64::from(dt))
    }

    /// [`Server::run_frame`] with id's `double host_frametime`: `sv.time`
    /// (a double) advances by exactly it, as `SV_Physics` does.
    pub fn run_frame_f64(&mut self, host_frametime: f64) -> Result<FrameReport> {
        let dt = host_frametime as f32;
        self.vm.set_host_frametime(host_frametime);
        // host_frametime = dt; sv.time advances at the END in the C, but the
        // think-time test compares against sv.time + host_frametime, so we set
        // frametime now and bump time after the loop.
        self.vm.set_glob_float(self.vm.go().frametime, dt);
        // Drop any half-collected temp-entity message from a prior (possibly
        // faulted) frame so this frame's Write* bursts parse cleanly.
        if let Some(o) = self.outbox() {
            o.reset_parsers();
        }
        // SV_CleanupEnts: clear last frame's one-frame EF_MUZZLEFLASH before thinks.
        self.cleanup_ents();
        // (float)sv.time, what every `pr_global_struct->time = sv.time` stores.
        let start_time = self.time();

        let mut thinks_fired = 0usize;
        // SV_Physics always starts with StartFrame (self/other = world, time =
        // sv.time) — the spawn settle frames included, so QC's `skill`,
        // `teamplay` and `framecount` globals are set from the first frame.
        // A program error anywhere in the frame ends it (id's Host_Error).
        self.vm.set_glob_float(self.vm.go().time, start_time);
        self.run_sys(SysFn::StartFrame, 0, 0)?;
        // `for (i=0 ; i<sv.num_edicts ; i++)`: the bound is re-read every
        // iteration, so an edict spawned by an earlier think this frame (a
        // missile, a gib) gets its physics on the frame it was spawned.
        let mut next = 0;
        while next < self.vm.num_edicts() {
            self.check_halted()?;
            let e = next;
            next += 1;
            // edict 0 is the world; process every non-free edict, as the C does.
            let free = self.vm.is_free_edict(e as i32);
            if free {
                continue;
            }
            let ent = e as i32;
            if !self.force_retouch_edict(ent, start_time) {
                continue; // a retouch freed it
            }
            let movetype = self.vm.movetype(ent);
            thinks_fired += self.process_entity(ent, movetype, start_time, dt)? as usize;
        }
        self.check_halted()?;

        self.decrement_force_retouch();
        self.end_physics_frame(host_frametime);

        // A think may have called lightstyle() (e.g. a trigger toggling a light);
        // apply those writes to the owned table (and any makestatic's).
        self.apply_lightstyles();
        self.apply_statics();

        Ok(FrameReport {
            thinks_fired,
            time: self.time(),
        })
    }

    /// The end of `SV_Physics`: `sv.time += host_frametime`, in double. (The
    /// port also leaves the `time` global at the new time's float between
    /// frames; in the C it keeps the frame's last value, but every C path that
    /// runs QuakeC outside `SV_Physics` sets it to `sv.time` first.)
    fn end_physics_frame(&mut self, host_frametime: f64) {
        self.vm.set_sv_time(self.vm.sv_time() + host_frametime);
        let t = self.time();
        self.vm.set_glob_float(self.vm.go().time, t);
    }

    /// Process one live edict for a frame: per-movetype physics plus
    /// `SV_RunThink`. Returns whether a think fired. `run_think` returns
    /// `(fired, alive)`; physics runs whenever the entity is still alive,
    /// independent of whether a think fired. A program error propagates: it
    /// ends the frame.
    fn process_entity(&mut self, ent: i32, movetype: MoveType, start_time: f32, dt: f32) -> Result<bool> {
        match movetype {
            MoveType::Push => self.physics_pusher(ent, start_time, dt),
            MoveType::None => {
                let (fired, _alive) = self.run_think(ent)?;
                Ok(fired)
            }
            MoveType::NoClip => {
                let (fired, alive) = self.run_think(ent)?;
                if alive {
                    self.integrate_noclip(ent, dt);
                }
                Ok(fired)
            }
            MoveType::Step => {
                // SV_Physics_Step: freefall (+ landing thud) if not on ground /
                // fly / swim, then SV_RunThink, then SV_CheckWaterTransition —
                // the C runs the water-transition check AFTER the think and
                // unconditionally (outside the freefall branch), so a step entity
                // resting on the floor still maintains watertype/waterlevel and
                // splashes when pushed into liquid.
                self.physics_step(ent, start_time, dt);
                let (fired, _alive) = self.run_think(ent)?;
                if !self.vm.is_free_edict(ent) {
                    self.check_water_transition(ent);
                }
                Ok(fired)
            }
            MoveType::Toss | MoveType::Bounce | MoveType::Fly | MoveType::FlyMissile => {
                // SV_Physics_Toss: think first; if alive, gravity + clipped move.
                let (fired, alive) = self.run_think(ent)?;
                if alive {
                    self.physics_toss(ent, movetype, start_time, dt);
                }
                Ok(fired)
            }
            MoveType::Walk | MoveType::AngleNoClip | MoveType::AngleClip | MoveType::Other(_) => {
                // MOVETYPE_WALK and any others: think only (no client AI).
                let (fired, _alive) = self.run_think(ent)?;
                Ok(fired)
            }
        }
    }

    /// `SV_Physics_Pusher` (sv_phys.c): advance a `MOVETYPE_PUSH` bmodel
    /// (`func_door`, `func_plat`, `func_button`, trains) by its velocity over the
    /// frame, carrying riders and respecting blockers, then fire its `think` when
    /// the local time `ltime` reaches `nextthink`.
    ///
    /// Faithful to the C: the move time is clamped so the pusher never steps past
    /// its scheduled think, [`Self::push_move`] advances `ltime` (unless blocked),
    /// and the think runs with `self = ent`, `other = world`. Returns whether
    /// the think fired, or its program error.
    fn physics_pusher(&mut self, ent: i32, start_time: f32, dt: f32) -> Result<bool> {
        let oldltime = self.vm.ent_float(ent, self.vm.fo().ltime);
        let thinktime = self.vm.ent_float(ent, self.vm.fo().nextthink);

        // thinktime < ent->v.ltime + host_frametime: float + double.
        let movetime = if f64::from(thinktime) < f64::from(oldltime) + self.vm.host_frametime() {
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

        let ltime = self.vm.ent_float(ent, self.vm.fo().ltime);
        let mut fired = false;
        if thinktime > oldltime && thinktime <= ltime {
            self.vm.set_ent_float(ent, self.vm.fo().nextthink, 0.0);
            self.vm.set_glob_float(self.vm.go().time, start_time);
            self.vm.set_glob_int(self.vm.go().self_, ent);
            self.vm.set_glob_int(self.vm.go().other, 0); // world
            let think = self.vm.ent_int(ent, self.vm.fo().think);
            if think > 0 {
                fired = true;
                self.vm.execute(think as usize)?;
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
    /// function is invoked. Otherwise the move stands and `ltime` is advanced.
    fn push_move(&mut self, pusher: i32, movetime: f32, sv_time: f32) -> Result<()> {
        let velocity = self.vm.ent_vec(pusher, self.vm.fo().velocity);
        if velocity[0] == 0.0 && velocity[1] == 0.0 && velocity[2] == 0.0 {
            self.advance_ltime(pusher, movetime);
            return Ok(());
        }

        let mut mov = [0.0f32; 3];
        for i in 0..3 {
            mov[i] = velocity[i] * movetime;
        }

        // Swept AABB of the pusher's move: start from absmin/absmax, then for each
        // axis extend the leading edge in the direction of travel.
        let absmin = self.vm.ent_vec(pusher, self.vm.fo().absmin);
        let absmax = self.vm.ent_vec(pusher, self.vm.fo().absmax);
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
        let pushorig = self.vm.ent_vec(pusher, self.vm.fo().origin);
        self.vm
            .set_ent_vec(pusher, self.vm.fo().origin, v_add(pushorig, mov));
        self.advance_ltime(pusher, movetime);
        link_edict(&mut self.vm, pusher);

        // Collect entities to drag, moving each as we go (origin, saved-origin).
        let mut moved: Vec<(i32, Vec3)> = Vec::new();
        let num = self.vm.num_edicts() as i32;
        let mut blocker: Option<i32> = None;

        let mut check: i32 = 1;
        while check < num {
            if self.vm.is_free_edict(check) {
                check += 1;
                continue;
            }
            if check == pusher {
                check += 1;
                continue;
            }
            let ck_movetype = self.vm.movetype(check);
            // SV_PushMove skips PUSH, NONE, and NOCLIP entities (sv_phys.c:478).
            if ck_movetype == MoveType::Push
                || ck_movetype == MoveType::None
                || ck_movetype == MoveType::NoClip
            {
                check += 1;
                continue;
            }

            // The check entity must be standing on the pusher, or its box must
            // intersect the pusher's swept box; otherwise it is unaffected.
            let flags = self.vm.flags(check);
            let ground = self.vm.ent_int(check, self.vm.fo().groundentity);
            let riding = flags.contains(EntFlags::ONGROUND) && ground == pusher;
            if !riding {
                let ck_absmin = self.vm.ent_vec(check, self.vm.fo().absmin);
                let ck_absmax = self.vm.ent_vec(check, self.vm.fo().absmax);
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
            if ck_movetype != MoveType::Walk {
                let f = self.vm.flags(check);
                self.vm.set_flags(check, f.without(EntFlags::ONGROUND));
            }

            // Drag the check along with the pusher and record it for rollback.
            // SV_PushMove (sv_phys.c:509-512) moves the rider/pushed entity via
            // SV_PushEntity (a CLIPPED move), temporarily making the pusher
            // SOLID_NOT so the rider does not clip on the pusher itself, then
            // restoring it. A clipped push lets a door push a rider against a
            // wall (so the door later blocks/crushes) instead of teleporting the
            // rider through solid geometry by an unclipped origin += mov.
            let entorig = self.vm.ent_vec(check, self.vm.fo().origin);
            moved.push((check, entorig));

            let pusher_solid = self.vm.solid(pusher);
            self.vm.set_solid(pusher, Solid::Not);
            self.push_entity(check, mov, sv_time);
            self.vm.set_solid(pusher, pusher_solid);
            // push_entity already linked `check` (SV_PushEntity -> SV_LinkEdict).

            // If the check is now stuck in solid geometry, the move is blocked.
            if self.push_test_position(check) {
                // A zero-thickness box (e.g. a flattened corpse) cannot block.
                let cmins = self.vm.ent_vec(check, self.vm.fo().mins);
                let cmaxs = self.vm.ent_vec(check, self.vm.fo().maxs);
                if cmins[0] == cmaxs[0] {
                    check += 1;
                    continue;
                }
                let csolid = self.vm.solid(check);
                if csolid == Solid::Not || csolid == Solid::Trigger {
                    // Corpse: squish its box flat so it stops blocking.
                    let mut m = self.vm.ent_vec(check, self.vm.fo().mins);
                    m[0] = 0.0;
                    m[1] = 0.0;
                    self.vm.set_ent_vec(check, self.vm.fo().mins, m);
                    self.vm.set_ent_vec(check, self.vm.fo().maxs, m);
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
                self.vm.set_ent_vec(block, self.vm.fo().origin, saved);
                link_edict(&mut self.vm, block);
            }

            // 2. Restore the pusher (origin, relink, roll back ltime).
            self.vm.set_ent_vec(pusher, self.vm.fo().origin, pushorig);
            link_edict(&mut self.vm, pusher);
            self.advance_ltime(pusher, -movetime);

            // 3. If the pusher has a "blocked" function, call it (self=pusher,
            //    other=blocker).
            let blocked = self.vm.ent_int(pusher, self.vm.fo().blocked);
            if blocked > 0 {
                self.vm.set_glob_int(self.vm.go().self_, pusher);
                self.vm.set_glob_int(self.vm.go().other, block);
                self.vm.execute(blocked as usize)?;
            }

            // 4. Move back every entity we already dragged (including the blocker
            //    again — harmless, matches the C loop).
            for &(e, saved) in &moved {
                self.vm.set_ent_vec(e, self.vm.fo().origin, saved);
                link_edict(&mut self.vm, e);
            }
        }

        Ok(())
    }

    /// `pusher->v.ltime += by` (SV_PushMove), kept exact by the uncapped step
    /// ([`advance_clock`]): a train's `ltime` runs with the level's clock, and
    /// in f32 at 480 Hz it would be 5.5% fast an hour in, its moves ending
    /// early and snapping onto their marks.
    fn advance_ltime(&mut self, pusher: i32, by: f32) {
        let mut ltime = self.vm.ent_float(pusher, self.vm.fo().ltime);
        let exact = self.ltime_exact.entry(pusher).or_insert(0.0);
        advance_clock(self.stepping, &mut ltime, exact, by);
        self.vm.set_ent_float(pusher, self.vm.fo().ltime, ltime);
    }

    /// `SV_TestEntityPosition` (sv_phys.c): true when `ent`'s box overlaps solid
    /// geometry at its current origin. Implemented, as in the C, by tracing the
    /// entity's own box from its origin to its origin and reporting `startsolid`.
    fn push_test_position(&mut self, ent: i32) -> bool {
        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let mins = self.vm.ent_vec(ent, self.vm.fo().mins);
        let maxs = self.vm.ent_vec(ent, self.vm.fo().maxs);
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
            let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
            self.vm.set_ent_vec(ent, self.vm.fo().oldorigin, origin);
            return;
        }

        let org = self.vm.ent_vec(ent, self.vm.fo().origin);
        let oldorigin = self.vm.ent_vec(ent, self.vm.fo().oldorigin);
        self.vm.set_ent_vec(ent, self.vm.fo().origin, oldorigin);
        if !self.push_test_position(ent) {
            link_edict(&mut self.vm, ent);
            return;
        }

        for z in 0..18 {
            for i in -1..=1 {
                for j in -1..=1 {
                    let cand = [org[0] + i as f32, org[1] + j as f32, org[2] + z as f32];
                    self.vm.set_ent_vec(ent, self.vm.fo().origin, cand);
                    if !self.push_test_position(ent) {
                        link_edict(&mut self.vm, ent);
                        return;
                    }
                }
            }
        }

        // still stuck: restore the original origin.
        self.vm.set_ent_vec(ent, self.vm.fo().origin, org);
    }

    /// `SV_RunThink` (sv_phys.c): if the edict's `nextthink` is in `(0, time+dt]`,
    /// clear it, set the `time`/`self`/`other` globals, and execute its `think`.
    /// Returns `(fired, alive)`: `fired` = a think executed this frame; `alive`
    /// is `SV_RunThink`'s own bool (the edict was not removed). When no think is
    /// due it returns `(false, true)` — nothing ran, the entity lives on, and
    /// the caller still runs per-movetype physics. Errors from the think
    /// propagate (the caller decides whether to abort the frame).
    fn run_think(&mut self, ent: i32) -> Result<(bool, bool)> {
        let thinktime = self.vm.ent_float(ent, self.vm.fo().nextthink);
        // `thinktime > sv.time + host_frametime`: the float promoted, the sum
        // in double (sv.time a double since server.h).
        let sv_time = self.vm.sv_time();
        if thinktime <= 0.0 || f64::from(thinktime) > sv_time + self.vm.host_frametime() {
            // Not due: SV_RunThink returns true (alive); nothing fired.
            return Ok((false, true));
        }
        // Don't let things stay in the past (thinktime = sv.time: a float).
        let thinktime = if f64::from(thinktime) < sv_time { sv_time as f32 } else { thinktime };

        self.vm.set_ent_float(ent, self.vm.fo().nextthink, 0.0);
        self.vm.set_glob_float(self.vm.go().time, thinktime);
        self.vm.set_glob_int(self.vm.go().self_, ent);
        self.vm.set_glob_int(self.vm.go().other, 0);

        let think = self.vm.ent_int(ent, self.vm.fo().think);
        let fnum = think as usize;
        if think <= 0 || fnum >= self.vm.progs().functions.len() {
            // nextthink consumed (as the C did), but no valid think to run;
            // the entity is still alive.
            return Ok((false, true));
        }
        // The C leaves pr_global_struct->time at thinktime afterward; we mirror that.
        self.vm.execute(fnum)?;

        // alive = !ent->free.
        let free = self.vm.is_free_edict(ent);
        Ok((true, !free))
    }

    /// `SV_Physics_Noclip` integration: `angles += dt*avelocity`,
    /// `origin += dt*velocity` (no clipping), then relink bounds.
    fn integrate_noclip(&mut self, ent: i32, dt: f32) {
        let angles = self.vm.ent_vec(ent, self.vm.fo().angles);
        let avel = self.vm.ent_vec(ent, self.vm.fo().avelocity);
        self.vm
            .set_ent_vec(ent, self.vm.fo().angles, crate::math::mul_add(angles, dt, avel));

        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        self.vm
            .set_ent_vec(ent, self.vm.fo().origin, crate::math::mul_add(origin, dt, vel));

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
        let flags = self.vm.flags(ent);
        if !flags.intersects(EntFlags::ONGROUND | EntFlags::FLY | EntFlags::SWIM) {
            // hitsound = velocity[2] < sv_gravity * -0.1, sampled BEFORE gravity.
            let vel_z = self.vm.ent_vec(ent, self.vm.fo().velocity)[2];
            let hitsound = vel_z < self.sv_gravity() * -0.1;

            // SV_Physics_Step freefall: AddGravity; CheckVelocity; SV_FlyMove;
            // SV_LinkEdict(ent, true). The C runs the full slide move (NOT a
            // single PushEntity), so a freefalling MOVETYPE_STEP entity latches
            // FL_ONGROUND on a floor contact and clips/slides its velocity
            // instead of accumulating downward speed forever.
            self.add_gravity(ent, dt);
            self.check_velocity(ent);
            let lead = self.gravity_lead(ent, dt);
            self.move_with_lead(ent, lead, |s| {
                let mut steptrace: Option<MoveTrace> = None;
                let _ = s.fly_move_core(ent, dt, sv_time, &mut steptrace);
            });

            // SV_LinkEdict(ent, true) ends the freefall branch: it recomputes
            // absmin/absmax from the NEW origin AND trips triggers/pickups for the
            // moved entity. This is INSIDE the branch in the C (the on-ground /
            // flying / swimming path returns before it), so an entity that skipped
            // the move does not re-link here. Skip if a touch impact during the move
            // already removed the entity.
            if !self.vm.is_free_edict(ent) {
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
                    self.vm.flags(ent).contains(EntFlags::ONGROUND);
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
    fn physics_toss(&mut self, ent: i32, movetype: MoveType, sv_time: f32, dt: f32) {
        let flags = self.vm.flags(ent);
        if flags.contains(EntFlags::ONGROUND) {
            return; // resting on the ground (C returns before CheckWaterTransition)
        }
        self.check_velocity(ent);

        // add gravity (not for FLY / FLYMISSILE)
        let falls = movetype != MoveType::Fly && movetype != MoveType::FlyMissile;
        if falls {
            self.add_gravity(ent, dt);
        }

        // move angles
        let angles = self.vm.ent_vec(ent, self.vm.fo().angles);
        let avel = self.vm.ent_vec(ent, self.vm.fo().avelocity);
        self.vm
            .set_ent_vec(ent, self.vm.fo().angles, crate::math::mul_add(angles, dt, avel));

        // move origin (the uncapped step leads the fall: `gravity_lead`)
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let lead = if falls { self.gravity_lead(ent, dt) } else { 0.0 };
        if lead != 0.0 {
            vel[2] += lead;
        }
        let move_ = crate::math::scale(vel, dt);
        let tr = self.push_entity(ent, move_, sv_time);

        // SV_PushEntity ends with SV_LinkEdict(ent, true): trip triggers/pickups
        // for the moved entity (unless a touch impact already removed it).
        if !self.vm.is_free_edict(ent) {
            touch_triggers(&mut self.vm, ent, sv_time);
        }

        if tr.fraction == 1.0 {
            return; // clear move
        }
        let free = self.vm.is_free_edict(ent);
        if free {
            return;
        }

        let backoff = if movetype == MoveType::Bounce { 1.5 } else { 1.0 };
        // The bounce takes the velocity the move was made with, lead and all,
        // as the walk and step moves clip theirs: a 72 Hz frame that hits
        // the floor mid-frame bounces with the speed of its end.
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        if lead != 0.0 {
            vel[2] += lead;
        }
        let new_vel = clip_velocity(vel, tr.plane_normal, backoff);
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, new_vel);

        // stop if on ground (nested ifs in the C, collapsed here — no elses)
        if tr.plane_normal[2] > 0.7 && (new_vel[2] < 60.0 || movetype != MoveType::Bounce) {
            let flags = self.vm.flags(ent);
            self.vm.set_flags(ent, flags.with(EntFlags::ONGROUND));
            // groundentity = EDICT_TO_PROG(trace.ent): the edict actually
            // landed on (0 = world, >0 = a plat/door/other solid), not a
            // hardcoded world. `tr.ent` is `-1` only when nothing was hit,
            // but this branch runs only when fraction < 1 (something WAS hit),
            // so clamp the "nothing" sentinel to the world (0) defensively.
            self.vm.set_ent_int(ent, self.vm.fo().groundentity, tr.ent.max(0));
            self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0; 3]);
            self.vm.set_ent_vec(ent, self.vm.fo().avelocity, [0.0; 3]);
        }

        // check for in water (SV_CheckWaterTransition). The C reaches this only
        // when the move was NOT clear (the `fraction == 1` / freed early returns
        // above skip it), so a grenade/gib that just struck something updates its
        // watertype here and splashes on an air/liquid crossing.
        if !self.vm.is_free_edict(ent) {
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
        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let mins = self.vm.ent_vec(ent, self.vm.fo().mins);
        let maxs = self.vm.ent_vec(ent, self.vm.fo().maxs);
        let view_ofs = self.vm.ent_vec(ent, self.vm.fo().view_ofs);
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
        self.vm.set_ent_float(ent, self.vm.fo().waterlevel, waterlevel as f32);
        self.vm.set_ent_float(ent, self.vm.fo().watertype, watertype as f32);
        waterlevel > 1
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
        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let cont = self
            .vm
            .with_host(|_vm, h| h.point_contents(origin))
            .unwrap_or(CONTENTS_SOLID);

        let watertype = self.vm.ent_float(ent, self.vm.fo().watertype) as i32;
        if watertype == 0 {
            // just spawned here
            self.vm.set_ent_float(ent, self.vm.fo().watertype, cont as f32);
            self.vm.set_ent_float(ent, self.vm.fo().waterlevel, 1.0);
            return;
        }

        if cont <= CONTENTS_WATER {
            if watertype == CONTENTS_EMPTY {
                // just crossed into water
                self.start_sound(ent, 0, "misc/h2ohit1.wav", 255, 1.0);
            }
            self.vm.set_ent_float(ent, self.vm.fo().watertype, cont as f32);
            self.vm.set_ent_float(ent, self.vm.fo().waterlevel, 1.0);
        } else {
            if watertype != CONTENTS_EMPTY {
                // just crossed out of water
                self.start_sound(ent, 0, "misc/h2ohit1.wav", 255, 1.0);
            }
            self.vm.set_ent_float(ent, self.vm.fo().watertype, CONTENTS_EMPTY as f32);
            self.vm.set_ent_float(ent, self.vm.fo().waterlevel, cont as f32);
        }
    }

    /// `SV_Physics`' `if (pr_global_struct->force_retouch) SV_LinkEdict (ent,
    /// true); // force retouch even for stationary`, run on each live edict
    /// before its physics. QC sets the global to 2 in `spawn_tdeath` (every
    /// `PutClientInServer` and teleport) and `teleport_use`, so for two frames
    /// everything relinks and touches the triggers it overlaps: a monster
    /// standing in a door's trigger field opens it at the level start (e1m6,
    /// e1m8), one standing on a teleport destination is telefragged, and a
    /// monster in a just-enabled teleporter is sent through. SV_LinkEdict skips
    /// the world, and a SOLID_NOT edict is relinked but touches nothing.
    /// Returns whether `ent` is still live (a touch may free it).
    fn force_retouch_edict(&mut self, ent: i32, sv_time: f32) -> bool {
        if ent == 0 || self.vm.glob_float(self.vm.go().force_retouch) == 0.0 {
            return true;
        }
        link_edict(&mut self.vm, ent);
        if self.vm.solid(ent) != Solid::Not {
            touch_triggers(&mut self.vm, ent, sv_time);
        }
        !self.vm.is_free_edict(ent)
    }

    /// The end of `SV_Physics`: `if (pr_global_struct->force_retouch)
    /// pr_global_struct->force_retouch--;`.
    fn decrement_force_retouch(&mut self) {
        let n = self.vm.glob_float(self.vm.go().force_retouch);
        if n != 0.0 {
            self.vm.set_glob_float(self.vm.go().force_retouch, n - 1.0);
        }
    }

    /// `SV_AddGravity` (sv_phys.c): `velocity[2] -= gravity * sv_gravity * dt`,
    /// where the per-entity `gravity` field defaults to 1.0 when unset/zero.
    fn add_gravity(&mut self, ent: i32, dt: f32) {
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        vel[2] -= self.gravity_of(ent) * dt;
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// The pull `SV_AddGravity` applies to `ent`: `ent.gravity` (1 when unset)
    /// times `sv_gravity`, in units/s².
    fn gravity_of(&self, ent: i32) -> f32 {
        let g = self.vm.ent_float(ent, self.vm.fo().gravity);
        let ent_gravity = if g != 0.0 { g } else { 1.0 };
        ent_gravity * self.sv_gravity()
    }

    /// How far the frame's move leads `ent`'s fall ([`Stepping::gravity_lead`]):
    /// 0 in Classic, where the move is id's.
    fn gravity_lead(&self, ent: i32, dt: f32) -> f32 {
        self.stepping.gravity_lead(self.gravity_of(ent), dt)
    }

    /// Run `mv`, a frame's move after `SV_AddGravity`, with `lead` added to
    /// the vertical velocity it moves by ([`Stepping::gravity_lead`]), then
    /// take the lead back off — unless the move replaced the vertical
    /// velocity (a floor, ceiling or slope clipped it, a stair step zeroed
    /// it), in which case the move's velocity stands, as in id's.
    fn move_with_lead(&mut self, ent: i32, lead: f32, mv: impl FnOnce(&mut Self)) {
        if lead == 0.0 {
            return mv(self);
        }
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        vel[2] += lead;
        let led = vel[2];
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
        mv(self);
        if !self.vm.is_free_edict(ent) {
            let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
            if vel[2] == led {
                vel[2] -= lead;
                self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
            }
        }
    }

    /// `SV_CheckVelocity` (sv_phys.c): clamp each velocity component to
    /// `±sv_maxvelocity` and scrub NaNs from velocity/origin.
    fn check_velocity(&mut self, ent: i32) {
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let mut origin = self.vm.ent_vec(ent, self.vm.fo().origin);
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
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
        self.vm.set_ent_vec(ent, self.vm.fo().origin, origin);
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
        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let mins = self.vm.ent_vec(ent, self.vm.fo().mins);
        let maxs = self.vm.ent_vec(ent, self.vm.fo().maxs);
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
        let movetype = self.vm.movetype(ent);
        let solid = self.vm.solid(ent);
        let missile = movetype == MoveType::FlyMissile;
        let nomonsters = !missile && (solid == Solid::Trigger || solid == Solid::Not);

        // Entity-aware move: clips world + all solid edicts; `ent` ignores
        // itself (the C `passedict`).
        let mt = sv_move(&mut self.vm, origin, end, mins, maxs, ent, nomonsters, missile);

        self.vm.set_ent_vec(ent, self.vm.fo().origin, mt.endpos);
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

    /// One server frame driven by the local player's input.
    ///
    /// Mirrors `Host_Frame` -> `SV_Physics`: advance `time`/`frametime`, run the
    /// `StartFrame` system function (self/other = world), then `SV_Physics` over
    /// every live edict — the player edict via `SV_Physics_Client`
    /// (`PlayerPreThink` -> movement -> `PlayerPostThink`), all others via the
    /// generic [`Self::process_entity`] path. `dt` is the frame time.
    pub fn client_frame(&mut self, cmd: &UserCmd, dt: f32) -> Result<FrameReport> {
        self.client_frame_f64(cmd, f64::from(dt))
    }

    /// [`Server::client_frame`] with id's `double host_frametime`: `sv.time`
    /// (a double) advances by exactly it, as `SV_Physics` does.
    pub fn client_frame_f64(&mut self, cmd: &UserCmd, host_frametime: f64) -> Result<FrameReport> {
        self.client_frame_stepped(cmd, host_frametime, Stepping::Classic)
    }

    /// [`Server::client_frame_f64`], its integrators stepped as `stepping`
    /// says: [`Stepping::Uncapped`] is the uncapped host's frame, which plays
    /// as a run of id's 1/72 s frames would.
    pub fn client_frame_stepped(
        &mut self,
        cmd: &UserCmd,
        host_frametime: f64,
        stepping: Stepping,
    ) -> Result<FrameReport> {
        self.stepping = stepping;
        let report = self.client_frame_inner(cmd, host_frametime);
        self.stepping = Stepping::Classic;
        report
    }

    fn client_frame_inner(&mut self, cmd: &UserCmd, host_frametime: f64) -> Result<FrameReport> {
        let dt = host_frametime as f32;
        self.vm.set_host_frametime(host_frametime);
        // host_frametime = dt; sv.time advances at the END of SV_Physics in the
        // C, but the think-due test compares against sv.time + host_frametime, so
        // (as run_frame does) we set frametime now and bump time after the loop.
        self.vm.set_glob_float(self.vm.go().frametime, dt);
        // Drop any half-collected temp-entity message from a prior (possibly
        // faulted) frame so this frame's Write* bursts parse cleanly.
        if let Some(o) = self.outbox() {
            o.reset_parsers();
        }
        // Drop any changelevel() / restart request a *prior* frame left untaken.
        if let Some(o) = self.outbox() {
            o.clear_requests();
        }
        // SV_CleanupEnts: clear last frame's one-frame EF_MUZZLEFLASH before this
        // frame's thinks (the host already consumed it via entity_dlights()).
        self.cleanup_ents();
        // (float)sv.time, what every `pr_global_struct->time = sv.time` stores.
        let start_time = self.time();

        // Host_ServerFrame runs SV_RunClients BEFORE SV_Physics: SV_ReadClientMove
        // copies the usercmd onto the client edict (v_angle, buttons, impulse),
        // then SV_ClientThink applies the look angles, the punch decay, friction
        // and acceleration (or the swim / water-jump move) to its velocity. Only
        // then does SV_Physics run StartFrame and every edict, the client's
        // PlayerPreThink (WaterMove's drag, PlayerJump) acting on the
        // ALREADY-accelerated velocity.
        self.vm.set_glob_float(self.vm.go().time, start_time);
        if let Some(player) = self.live_player() {
            self.apply_usercmd_to_edict(player, cmd);
            self.client_think(player, cmd, dt);
        }

        // Let the progs know a new frame has started (self/other = world,
        // time = sv.time).
        // A program error anywhere in the frame ends it (id's Host_Error).
        let mut thinks_fired = 0usize;
        self.vm.set_glob_float(self.vm.go().time, start_time);
        self.run_sys(SysFn::StartFrame, 0, 0)?;

        // The bound is re-read every iteration, as in SV_Physics (see run_frame).
        let mut next = 0;
        while next < self.vm.num_edicts() {
            self.check_halted()?;
            let e = next;
            next += 1;
            let free = self.vm.is_free_edict(e as i32);
            if free {
                continue;
            }
            let ent = e as i32;
            if !self.force_retouch_edict(ent, start_time) {
                continue; // a retouch freed it
            }

            let fired = if Some(ent) == self.player {
                self.physics_client(ent, start_time, dt)?
            } else {
                let movetype = self.vm.movetype(ent);
                self.process_entity(ent, movetype, start_time, dt)?
            };
            thinks_fired += fired as usize;
        }
        self.check_halted()?;

        self.decrement_force_retouch();

        // SV_WriteClientdataToMessage (sv_main.c) runs SV_SetIdealPitch once per
        // client per frame, after physics: compute the slope-following auto-pitch
        // the QuakeC view code centres toward when you walk up/down stairs.
        if let Some(player) = self.live_player() {
            self.set_ideal_pitch(player);
        }

        self.end_physics_frame(host_frametime);

        // A think may have called lightstyle() (e.g. a trigger toggling a light);
        // apply those writes to the owned table (and any makestatic's).
        self.apply_lightstyles();
        self.apply_statics();

        Ok(FrameReport {
            thinks_fired,
            time: self.time(),
        })
    }

    /// `SV_Physics_Client` (sv_phys.c ~1059): `PlayerPreThink` -> the movement
    /// path chosen by movetype -> `touch_triggers` -> relink -> `PlayerPostThink`.
    /// Returns whether a think fired (for the frame report). A removed player
    /// (`free`) short-circuits the rest, like the C `SV_RunThink` guards.
    fn physics_client(&mut self, ent: i32, start_time: f32, dt: f32) -> Result<bool> {
        // (The usercmd and SV_ClientThink were applied by client_frame before
        // SV_Physics began, as SV_RunClients does.)

        // call standard client pre-think (self = player). SV_Physics_Client sets
        // `pr_global_struct->time = sv.time` first: without it PreThink reads the
        // `time` a preceding think left (its clamped thinktime), so its timers
        // (air_finished, lava damage, IntermissionThink) could fire a frame early.
        self.vm.set_glob_float(self.vm.go().time, start_time);
        self.run_sys(SysFn::PlayerPreThink, ent, 0)?;
        if self.vm.is_free_edict(ent) {
            return Ok(false);
        }

        // SV_CheckVelocity clamps before the move (the slide clamps implicitly,
        // but mirror the NaN/maxvelocity scrub the C does first).
        self.check_velocity(ent);

        let movetype = self.vm.movetype(ent);
        // Each arm assigns `fired`; the initial value is just to satisfy the
        // borrow checker on the early-return paths.
        #[allow(unused_assignments)]
        let mut fired = false;
        match movetype {
            MoveType::None => {
                let (f, alive) = self.run_think(ent)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
            }
            MoveType::Walk => {
                let (f, alive) = self.run_think(ent)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                // (SV_ClientThink already ran, in SV_RunClients.) Gravity (unless
                // in water or water-jumping) and the step-up walk move. check_water
                // sets waterlevel/watertype so the QuakeC WaterMove (PlayerPreThink)
                // can deal lava/slime damage.
                let in_water = self.check_water(ent);
                let flags = self.vm.flags(ent);
                let falls = !in_water && !flags.contains(EntFlags::WATERJUMP);
                if falls {
                    self.add_gravity(ent, dt);
                }
                // SV_CheckStuck: free the player from the clipping hull (and
                // latch `oldorigin`) right before the walk move, as the C does.
                self.check_stuck(ent);
                let lead = if falls { self.gravity_lead(ent, dt) } else { 0.0 };
                self.move_with_lead(ent, lead, |s| s.walk_move(ent, start_time, dt));
            }
            MoveType::Fly => {
                let (f, alive) = self.run_think(ent)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                self.check_water(ent); // keep waterlevel/watertype live while flying
                self.player_fly_move(ent, start_time, dt);
            }
            MoveType::NoClip => {
                let (f, alive) = self.run_think(ent)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                // origin += frametime * velocity (no clipping).
                let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
                let vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
                self.vm
                    .set_ent_vec(ent, self.vm.fo().origin, crate::math::mul_add(origin, dt, vel));
            }
            MoveType::Toss | MoveType::Bounce => {
                // SV_Physics_Client `case MOVETYPE_TOSS/BOUNCE: SV_Physics_Toss`:
                // think first; if still alive, gravity + the clipped toss move. A
                // client is MOVETYPE_TOSS exactly while DEAD (client.qc PlayerDie),
                // so this is the corpse physics — the death pop (PlayerDie's
                // `velocity_z += random()*300`) and the fall back to the floor.
                // Previously this fell into the think-only fallback arm and a
                // corpse killed mid-air froze in place.
                let (f, alive) = self.run_think(ent)?;
                fired = f;
                if !alive {
                    return Ok(fired);
                }
                self.physics_toss(ent, movetype, start_time, dt);
            }
            _ => {
                // Any other movetype on a client: think only (no movement).
                let (f, _alive) = self.run_think(ent)?;
                fired = f;
            }
        }

        if self.vm.is_free_edict(ent) {
            return Ok(fired);
        }

        // After moving, trip triggers so the player can pick up items / fire
        // trigger fields (the C does this inside SV_LinkEdict during the move;
        // here the move's link is bounds-only, so we touch triggers explicitly).
        touch_triggers(&mut self.vm, ent, start_time);
        if self.vm.is_free_edict(ent) {
            return Ok(fired);
        }

        // call standard player post-think (relink first, like SV_Physics_Client).
        // SV_Physics_Client (sv_phys.c:1128) sets pr_global_struct->time = sv.time
        // before PlayerPostThink; without this the global is left at the move's
        // touch time (or a think's clamped thinktime), so PostThink would read a
        // stale `time`. run_sys sets self/other but never time.
        link_edict(&mut self.vm, ent);
        self.vm.set_glob_float(self.vm.go().time, start_time);
        self.run_sys(SysFn::PlayerPostThink, ent, 0)?;

        // No engine clear of `impulse` (census F4): the C never clears it —
        // QuakeC's ImpulseCommands does, once W_WeaponFrame gets past the
        // weapon cooldown, so a switch pressed mid-cooldown waits for it.
        Ok(fired)
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
        let original_velocity = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let primal_velocity = original_velocity;
        let mut original = original_velocity;
        // Clip planes are capped at 5 (the `>= 5` guard below), so a fixed array
        // + count avoids a per-call heap Vec — identical plane set, same order.
        let mut planes: [Vec3; 5] = [[0.0; 3]; 5];
        let mut nplanes = 0usize;
        let mut time_left = dt;

        for _bump in 0..num_bumps {
            let velocity = self.vm.ent_vec(ent, self.vm.fo().velocity);
            if velocity == [0.0, 0.0, 0.0] {
                break;
            }
            let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
            let end = [
                origin[0] + time_left * velocity[0],
                origin[1] + time_left * velocity[1],
                origin[2] + time_left * velocity[2],
            ];
            let mins = self.vm.ent_vec(ent, self.vm.fo().mins);
            let maxs = self.vm.ent_vec(ent, self.vm.fo().maxs);
            let trace = sv_move(&mut self.vm, origin, end, mins, maxs, ent, false, false);

            if trace.allsolid {
                // entity is trapped in another solid: stop dead.
                self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0; 3]);
                return 3;
            }

            if trace.fraction > 0.0 {
                // actually covered some distance
                self.vm.set_ent_vec(ent, self.vm.fo().origin, trace.endpos);
                original = self.vm.ent_vec(ent, self.vm.fo().velocity);
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
                        && self.vm.solid(trace.ent) == Solid::Bsp);
                if on_bsp {
                    let flags = self.vm.flags(ent);
                    self.vm.set_flags(ent, flags.with(EntFlags::ONGROUND));
                    self.vm.set_ent_int(ent, self.vm.fo().groundentity, trace.ent.max(0));
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
                if self.vm.is_free_edict(ent) {
                    break; // removed by the impact function
                }
            }

            time_left -= time_left * trace.fraction;

            // clipped to another plane
            if nplanes >= 5 {
                // this shouldn't really happen
                self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0; 3]);
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
                self.vm.set_ent_vec(ent, self.vm.fo().velocity, new_velocity);
            } else {
                // go along the crease (two planes)
                if nplanes != 2 {
                    self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0; 3]);
                    return 7;
                }
                let dir = crate::math::cross(planes[0], planes[1]);
                let cur = self.vm.ent_vec(ent, self.vm.fo().velocity);
                let d = crate::math::dot(dir, cur);
                self.vm
                    .set_ent_vec(ent, self.vm.fo().velocity, crate::math::scale(dir, d));
            }

            // if velocity is against the original velocity, stop dead to avoid
            // tiny oscillations in sloping corners.
            let cur = self.vm.ent_vec(ent, self.vm.fo().velocity);
            if crate::math::dot(cur, primal_velocity) <= 0.0 {
                self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0; 3]);
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
        let oldonground = self.vm.flags(ent).contains(EntFlags::ONGROUND);
        // Clear ONGROUND; fly_move / the down move below will re-set it.
        let flags0 = self.vm.flags(ent);
        self.vm.set_flags(ent, flags0.without(EntFlags::ONGROUND));

        let oldorg = self.vm.ent_vec(ent, self.vm.fo().origin);
        let oldvel = self.vm.ent_vec(ent, self.vm.fo().velocity);

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
        let waterlevel = self.vm.ent_float(ent, self.vm.fo().waterlevel) as i32;
        if !oldonground && waterlevel == 0 {
            link_edict(&mut self.vm, ent);
            return;
        }
        if self.vm.movetype(ent) != MoveType::Walk {
            link_edict(&mut self.vm, ent); // gibbed by a trigger
            return;
        }
        if self.vm.flags(ent).contains(EntFlags::WATERJUMP) {
            link_edict(&mut self.vm, ent);
            return;
        }

        // remember the no-step result.
        let nosteporg = self.vm.ent_vec(ent, self.vm.fo().origin);
        let nostepvel = self.vm.ent_vec(ent, self.vm.fo().velocity);

        // try moving up and forward to go up a step.
        self.vm.set_ent_vec(ent, self.vm.fo().origin, oldorg); // back to start pos

        // move up
        let upmove = [0.0, 0.0, world::STEPSIZE];
        self.push_entity(ent, upmove, sv_time);

        // move forward (no vertical wish in velocity).
        self.vm
            .set_ent_vec(ent, self.vm.fo().velocity, [oldvel[0], oldvel[1], 0.0]);
        let mut steptrace2 = None;
        let mut clip2 = self.fly_move_core(ent, dt, sv_time, &mut steptrace2);

        // Stuck check (sv_phys.c SV_WalkMove ~1015): if the step-up forward move
        // made essentially no horizontal progress (< 1/32 unit on BOTH axes) but
        // still blocked, the player is wedged at a BSP hull angle-join — try the
        // SV_TryUnstick nudge dance to free them, adopting its resulting clip.
        if clip2 != 0 {
            let neworg = self.vm.ent_vec(ent, self.vm.fo().origin);
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
            if self.vm.solid(ent) == Solid::Bsp {
                let flags = self.vm.flags(ent);
                self.vm.set_flags(ent, flags.with(EntFlags::ONGROUND));
                // groundentity = EDICT_TO_PROG(downtrace.ent): the edict we
                // stepped down onto (0 = world, >0 = a plat/door). `downtrace.ent`
                // is `-1` only when the down-push was clear, but plane_normal[2] >
                // 0.7 implies a floor contact, so clamp the sentinel to world (0).
                self.vm.set_ent_int(ent, self.vm.fo().groundentity, downtrace.ent.max(0));
            }
        } else {
            // the push down didn't reach good ground: use the no-step move.
            self.vm.set_ent_vec(ent, self.vm.fo().origin, nosteporg);
            self.vm.set_ent_vec(ent, self.vm.fo().velocity, nostepvel);
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
        let v_angle = self.vm.ent_vec(ent, self.vm.fo().v_angle);
        let (forward, _right, _up) = angle_vectors(v_angle);
        let d = crate::math::dot(normal, forward) + 0.5;
        if d >= 0.0 {
            return;
        }
        let vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let i = crate::math::dot(normal, vel);
        let into = [normal[0] * i, normal[1] * i, normal[2] * i];
        let side = [vel[0] - into[0], vel[1] - into[1], vel[2] - into[2]];
        let new_vel = [side[0] * (1.0 + d), side[1] * (1.0 + d), vel[2]];
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, new_vel);
    }

    /// `SV_TryUnstick` (sv_phys.c ~901): the player is wedged at a BSP hull
    /// angle-join where float precision pins the step-up move. Nudge the player 2
    /// units in each of 8 axial/diagonal directions, retry the original horizontal
    /// move, and accept the first direction that frees > 4 units of progress on X or
    /// Y; otherwise restore the position and try the next. If none work, zero the
    /// velocity ("don't stick") and report a full block (7).
    fn sv_try_unstick(&mut self, ent: i32, oldvel: Vec3, sv_time: f32) -> i32 {
        let oldorg = self.vm.ent_vec(ent, self.vm.fo().origin);
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
                .set_ent_vec(ent, self.vm.fo().velocity, [oldvel[0], oldvel[1], 0.0]);
            let mut steptrace = None;
            let clip = self.fly_move_core(ent, 0.1, sv_time, &mut steptrace);
            let neworg = self.vm.ent_vec(ent, self.vm.fo().origin);
            if (oldorg[1] - neworg[1]).abs() > 4.0 || (oldorg[0] - neworg[0]).abs() > 4.0 {
                return clip; // freed
            }
            // go back to the original (stuck) pos and try the next direction.
            self.vm.set_ent_vec(ent, self.vm.fo().origin, oldorg);
        }
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, [0.0, 0.0, 0.0]); // don't stick
        7 // still not moving
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::Bsp;
    use crate::progs::{Op, Progs, Statement};
    use crate::server::testutil::*;
    use crate::server::{EntFlags, MoveType, Solid};

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
                    op: Op::StoreF,
                    a: g_one as i16,
                    b: g_flag as i16,
                    c: 0,
                },
                Statement {
                    op: Op::Done,
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
        server.vm.set_movetype(e, MoveType::None);
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
        let (mut server, e) = toss_server();
        server.run_frame(0.1).expect("frame");

        // velocity.z should be negative (gravity pulled it down): -1*800*0.1 = -80.
        let vel = server.vm.ent_get_vector(e, "velocity");
        assert!(vel[2] < 0.0, "gravity should make velocity.z negative, got {vel:?}");
        assert!((vel[2] - (-80.0)).abs() < 1e-3, "expected -80, got {}", vel[2]);
    }

    /// A server holding one airborne MOVETYPE_TOSS point entity at z 100.
    fn toss_server() -> (Server, i32) {
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
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.set_movetype(e, MoveType::Toss);
        server.vm.ent_set_float(e, "nextthink", 0.0); // no think
        server.vm.ent_set_float(e, "flags", 0.0); // not on ground
        server.vm.ent_set_vector(e, "velocity", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "origin", [0.0, 0.0, 100.0]);
        // tiny point box so trace uses hull 0.
        server.vm.ent_set_vector(e, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(e, "maxs", [0.0, 0.0, 0.0]);
        (server, e)
    }

    #[test]
    fn ed_alloc_waits_half_a_second_before_reusing_a_freed_slot() {
        // CENSUS L6: ED_Alloc takes a free slot only if it was freed in the
        // first two seconds of server time or more than 0.5 s ago, so a missile
        // spawned the frame another is removed never inherits its slot (and
        // the client never draws a trail from the old one to the new).
        let (img, _touch_fn, _g_one, _g_flag) = touch_progs();
        let mut server = Server::new(world_open_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        server.vm.set_sv_time(1.5); // the relaxed first two seconds
        let a = server.vm.spawn();
        server.vm.free_edict(a);
        assert_eq!(server.vm.spawn(), a, "freed at t 1.5: reused at once");
        server.vm.set_sv_time(10.0);
        server.vm.free_edict(a);
        let b = server.vm.spawn();
        assert_ne!(b, a, "freed this frame: not reused");
        server.vm.set_sv_time(10.4);
        assert_ne!(server.vm.spawn(), a, "0.4 s later: still not");
        server.vm.set_sv_time(10.6);
        assert_eq!(server.vm.spawn(), a, "0.6 s later: reused");
    }

    #[test]
    fn ed_free_clears_only_the_fields_the_c_clears() {
        // ED_Free zeroes model/takedamage/modelindex/colormap/skin/frame/origin/
        // angles/solid, sets nextthink -1 and freetime; everything else stays
        // (QuakeC holding a reference to a removed entity still reads it).
        let (img, _touch_fn, _g_one, _g_flag) = touch_progs();
        let mut server = Server::new(world_open_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        let e = server.vm.spawn();
        server.vm.ent_set_string(e, "classname", "missile");
        server.vm.ent_set_string(e, "model", "progs/missile.mdl");
        server.vm.ent_set_vector(e, "origin", [1.0, 2.0, 3.0]);
        server.vm.ent_set_vector(e, "velocity", [100.0, 0.0, 0.0]);
        server.vm.set_solid(e, Solid::BBox);
        server.vm.ent_set_float(e, "nextthink", 5.0);
        server.vm.free_edict(e);
        assert!(server.vm.is_free_edict(e));
        assert_eq!(server.vm.ent_get_string(e, "model"), "");
        assert_eq!(server.vm.ent_get_vector(e, "origin"), [0.0; 3]);
        assert_eq!(server.vm.ent_get_float(e, "solid"), 0.0);
        assert_eq!(server.vm.ent_get_float(e, "nextthink"), -1.0);
        assert_eq!(server.vm.ent_get_string(e, "classname"), "missile", "kept");
        assert_eq!(server.vm.ent_get_vector(e, "velocity"), [100.0, 0.0, 0.0], "kept");
    }

    #[test]
    fn an_edict_spawned_by_a_think_moves_on_its_spawn_frame() {
        // CENSUS L25: SV_Physics' loop re-reads sv.num_edicts every iteration,
        // so a missile spawned by an earlier think gets its physics the same
        // frame. The thinker's QC: e = spawn(); e.movetype = MOVETYPE_FLYMISSILE;
        // e.velocity = '100 0 0'. After one 0.1 s frame it has moved 10 units.
        let mut b = Builder::new();
        b.entityfields = 32;
        for (name, ty, ofs) in [("self", 4, 31), ("other", 4, 32), ("time", EV_FLOAT, 33), ("frametime", EV_FLOAT, 35)] {
            b.add_global(name, ty, ofs);
        }
        for (name, ty, ofs) in [
            ("classname", EV_STRING, 1), ("movetype", EV_FLOAT, 2), ("nextthink", EV_FLOAT, 3),
            ("flags", EV_FLOAT, 4), ("velocity", 3, 5), ("origin", 3, 8), ("mins", 3, 11),
            ("maxs", 3, 14), ("think", EV_FUNCTION, 17), ("solid", EV_FLOAT, 18),
            ("absmin", 3, 19), ("absmax", 3, 22), ("size", 3, 25), ("groundentity", 4, 28),
            ("owner", 4, 29),
        ] {
            b.add_field(name, ty, ofs);
        }
        let spawn = b.add_builtin("spawn", 14);
        let (g_spawn, g_fmove, g_fvel, g_ptr, g_nine, g_vel) = (40i16, 41i16, 42i16, 43i16, 44i16, 45i16);
        let st = |op: Op, a: i16, b: i16, c: i16| Statement { op, a, b, c };
        let spawner = b.add_function(
            "spawner",
            vec![
                st(Op::Call0, g_spawn, 0, 0),
                st(Op::Address, crate::progs::OFS_RETURN as i16, g_fmove, g_ptr),
                st(Op::StorepF, g_nine, g_ptr, 0),
                st(Op::Address, crate::progs::OFS_RETURN as i16, g_fvel, g_ptr),
                st(Op::StorepV, g_vel, g_ptr, 0),
                st(Op::Done, 0, 0, 0),
            ],
        );
        let progs = Progs::parse(&b.build()).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        server.vm.set_gi(g_spawn as usize, spawn as i32);
        server.vm.set_gi(g_fmove as usize, 2);
        server.vm.set_gi(g_fvel as usize, 5);
        server.vm.set_gf(g_nine as usize, MoveType::FlyMissile.code() as f32);
        server.vm.set_gv(g_vel as usize, [100.0, 0.0, 0.0]);
        let thinker = server.vm.spawn();
        server.vm.ent_set_int(thinker, "think", spawner as i32);
        server.vm.ent_set_float(thinker, "nextthink", server.time());
        let n0 = server.vm.num_edicts();
        server.run_frame(0.1).expect("frame");
        let missile = n0 as i32; // appended by the think
        assert_eq!(server.vm.movetype(missile), MoveType::FlyMissile);
        let x = server.vm.ent_get_vector(missile, "origin")[0];
        assert!((x - 10.0).abs() < 1e-3, "moved on its spawn frame: x = {x}");
    }

    #[test]
    fn run_frame_starts_with_startframe_like_sv_physics() {
        // CENSUS L20: SV_Physics runs StartFrame (time = sv.time) every frame,
        // the spawn settle frames too; run_frame (those frames) skipped it.
        let mut b = Builder::new();
        b.add_global("self", 4, 31);
        b.add_global("other", 4, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("startframe_time", EV_FLOAT, 40);
        b.add_function(
            "StartFrame",
            vec![
                Statement { op: Op::StoreF, a: 33, b: 40, c: 0 },
                Statement { op: Op::Done, a: 0, b: 0, c: 0 },
            ],
        );
        let progs = Progs::parse(&b.build()).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");
        server.vm.gset_float("startframe_time", -1.0);
        let t0 = server.time();
        server.run_frame(0.1).expect("frame");
        assert_eq!(server.vm.gget_float("startframe_time"), t0, "StartFrame ran at sv.time");
        server.run_frame(0.1).expect("frame");
        assert!((server.vm.gget_float("startframe_time") - (t0 + 0.1)).abs() < 1e-6);
    }

    /// A server whose StartFrame records `time` in `startframe_time` (40) and
    /// whose function `record` stores `time` in `think_time` (41).
    fn time_recording_server() -> Server {
        let mut b = Builder::new();
        b.entityfields = 8;
        b.add_global("self", 4, 31);
        b.add_global("other", 4, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("startframe_time", EV_FLOAT, 40);
        b.add_global("think_time", EV_FLOAT, 41);
        b.add_field("nextthink", EV_FLOAT, 1);
        b.add_field("think", EV_FUNCTION, 2);
        b.add_field("movetype", EV_FLOAT, 3);
        for (name, dst) in [("StartFrame", 40i16), ("record", 41)] {
            b.add_function(
                name,
                vec![
                    Statement { op: Op::StoreF, a: 33, b: dst, c: 0 },
                    Statement { op: Op::Done, a: 0, b: 0, c: 0 },
                ],
            );
        }
        let progs = Progs::parse(&b.build()).expect("parse");
        Server::new(empty_bsp(), progs).expect("server")
    }

    #[test]
    fn sv_time_is_a_double_and_the_qc_time_global_its_float() {
        // server.h: `double time`; SV_Physics ends with sv.time +=
        // host_frametime and each pr_global_struct->time = sv.time stores a
        // float. The port added the QC float global up in f32: after 63 frames
        // of 0.1 s from 1.0 that is 7.2999954, a frame's flash early in
        // (int)((cl.time - item_gettime)*10).
        let mut server = time_recording_server();
        assert_eq!(server.sv_time(), 1.0, "SV_SpawnServer: sv.time = 1.0");
        let mut double = 1.0f64;
        let mut float = 1.0f32;
        for _ in 0..63 {
            server.run_frame_f64(0.1).expect("frame");
            double += 0.1;
            float += 0.1;
        }
        assert_eq!(server.sv_time(), double, "the clock adds host_frametime in double");
        assert_eq!(server.time(), 7.3f32);
        assert_eq!(server.vm.gget_float("time"), 7.3f32, "the QC global holds (float)sv.time");
        assert_eq!(server.vm.gget_float("startframe_time"), 7.2f32, "StartFrame got (float)sv.time");
        assert_ne!(float, 7.3f32, "the old f32 sum ({float}) is a float step off");
        // The f32 entry point widens its dt: 0.1f32 is 1.5e-9 over 0.1, well
        // under a float step here.
        let mut server = time_recording_server();
        for _ in 0..63 {
            server.run_frame(0.1).expect("frame");
        }
        assert_eq!(server.time(), 7.3f32);
    }

    #[test]
    fn a_think_is_due_by_sv_time_plus_host_frametime_in_double() {
        // SV_RunThink: `if (thinktime <= 0 || thinktime > sv.time +
        // host_frametime) return true;` compares the float nextthink with a
        // double. QuakeC's `self.nextthink = time + 0.1` at sv.time 7.3 stores
        // (float)(7.3f + 0.1f) = 7.4f = 7.40000010: above 7.3 + 0.1 in double,
        // so the think waits a frame, and then runs with time = 7.4f (not
        // raised to sv.time 7.3999999999999995). In f32 7.3f + 0.1f is 7.4f
        // itself and the think ran a frame early.
        let mut server = time_recording_server();
        let record = server.vm.progs().find_function("record").expect("record") as i32;
        server.set_sv_time(7.3);
        let thinker = server.vm.spawn();
        server.vm.ent_set_int(thinker, "think", record);
        server.vm.ent_set_float(thinker, "nextthink", 7.3f32 + 0.1f32);
        server.vm.gset_float("think_time", -1.0);
        server.run_frame_f64(0.1).expect("frame");
        assert_eq!(server.vm.gget_float("think_time"), -1.0, "not due at sv.time 7.3");
        server.run_frame_f64(0.1).expect("frame");
        assert_eq!(server.vm.gget_float("think_time"), 7.4f32, "due at 7.4, time = the thinktime");
        assert_eq!(server.vm.ent_get_float(thinker, "nextthink"), 0.0);
    }

    #[test]
    fn player_prethink_sees_sv_time_not_a_preceding_thinktime() {
        // CENSUS L4: SV_Physics_Client sets pr_global_struct->time = sv.time
        // before PlayerPreThink. An edict thinking earlier in the frame at
        // nextthink = sv.time + 0.05 leaves `time` = its thinktime (SV_RunThink);
        // PreThink must still read sv.time.
        let record_time = vec![
            Statement { op: Op::StoreF, a: 33, b: 56, c: 0 }, // prethink_time = time
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        let (img, c100, org) = player_progs_with_prethink(record_time);
        let mut server = Server::new(floor_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        prime_player_globals(&mut server, c100, org);
        let thinker = server.vm.spawn();
        let noop = server.vm.progs().find_function("StartFrame").expect("a DONE-only function");
        server.vm.ent_set_int(thinker, "think", noop as i32);
        let t0 = server.time();
        server.vm.ent_set_float(thinker, "nextthink", t0 + 0.05);
        let player = server.connect_client().expect("connect");
        assert!(thinker < player, "the thinker runs before the player in the edict loop");
        server.client_frame(&UserCmd::default(), 0.1).expect("frame");
        assert_eq!(server.vm.gget_float("prethink_time"), t0, "PreThink saw sv.time");
    }

    #[test]
    fn force_retouch_relinks_stationary_edicts_for_two_frames() {
        // CENSUS F8: SV_Physics does SV_LinkEdict(ent, true) on every live edict
        // while the QC force_retouch global is set, then decrements it. A
        // stationary box inside a trigger is touched on exactly the two frames
        // after spawn_tdeath's force_retouch = 2 — never without it — and a
        // SOLID_NOT edict is relinked but touches nothing.
        let (img, touch_fn, g_one, _g_flag) = touch_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(world_open_bsp(), progs).expect("server");
        server.vm.set_gf(g_one, 1.0);
        let still = server.vm.spawn();
        server.vm.set_solid(still, Solid::BBox);
        server.vm.ent_set_vector(still, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(still, "mins", [-16.0; 3]);
        server.vm.ent_set_vector(still, "maxs", [16.0; 3]);
        let trigger = server.vm.spawn();
        server.vm.set_solid(trigger, Solid::Trigger);
        server.vm.ent_set_int(trigger, "touch", touch_fn as i32);
        server.vm.ent_set_vector(trigger, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(trigger, "mins", [-8.0; 3]);
        server.vm.ent_set_vector(trigger, "maxs", [8.0; 3]);
        link_edict(&mut server.vm, trigger);
        let frame = |server: &mut Server| {
            server.vm.gset_float("touched_flag", 0.0);
            server.run_frame(0.1).expect("frame");
            server.vm.gget_float("touched_flag")
        };
        assert_eq!(frame(&mut server), 0.0, "no force_retouch: a stationary edict touches nothing");
        server.vm.gset_float("force_retouch", 2.0);
        assert_eq!(frame(&mut server), 1.0, "first force_retouch frame");
        assert_eq!(server.vm.gget_float("force_retouch"), 1.0);
        assert_eq!(server.vm.ent_get_vector(still, "absmin"), [83.0, -17.0, -17.0], "relinked");
        assert_eq!(frame(&mut server), 1.0, "second force_retouch frame");
        assert_eq!(server.vm.gget_float("force_retouch"), 0.0);
        assert_eq!(frame(&mut server), 0.0, "and then no more");
        server.vm.set_solid(still, Solid::Not);
        server.vm.gset_float("force_retouch", 1.0);
        assert_eq!(frame(&mut server), 0.0, "Solid::Not: SV_LinkEdict returns before SV_TouchLinks");
    }

    /// id's `PR_RunError` longjmps out of the whole frame to `Host_Error`: a
    /// QuakeC error in a touch the physics runs (here a trigger `force_retouch`
    /// relinks, `SV_TouchLinks`) ends the frame with that error, and the server
    /// runs no more QuakeC — the next frame fails at once with the same error.
    #[test]
    fn a_touch_error_in_the_physics_ends_the_frame() {
        let mut b = Builder::new();
        b.entityfields = 32;
        for (name, ty, ofs) in [
            ("self", EV_ENTITY, 31),
            ("other", EV_ENTITY, 32),
            ("time", EV_FLOAT, 33),
            ("world", EV_ENTITY, 34),
            ("frametime", EV_FLOAT, 35),
            ("force_retouch", EV_FLOAT, 36),
        ] {
            b.add_global(name, ty, ofs);
        }
        for (name, ty, ofs) in [
            ("solid", EV_FLOAT, 2),
            ("touch", EV_FUNCTION, 3),
            ("origin", EV_VECTOR, 4),
            ("mins", EV_VECTOR, 7),
            ("maxs", EV_VECTOR, 10),
            ("absmin", EV_VECTOR, 13),
            ("absmax", EV_VECTOR, 16),
            ("movetype", EV_FLOAT, 20),
            ("size", EV_VECTOR, 26),
        ] {
            b.add_field(name, ty, ofs);
        }
        // bad_touch calls the function in global 40, which holds 0.
        let call_null = Statement { op: Op::Call0, a: 40, b: 0, c: 0 };
        let done = Statement { op: Op::Done, a: 0, b: 0, c: 0 };
        let bad_touch = b.add_function("bad_touch", vec![call_null, done]);
        let mut server = Server::new(world_open_bsp(), Progs::parse(&b.build()).expect("parse")).expect("server");

        let mover = server.vm.spawn();
        server.vm.set_solid(mover, Solid::BBox);
        server.vm.ent_set_vector(mover, "maxs", [16.0; 3]);
        server.vm.ent_set_vector(mover, "mins", [-16.0; 3]);
        let trigger = server.vm.spawn();
        server.vm.set_solid(trigger, Solid::Trigger);
        server.vm.ent_set_int(trigger, "touch", bad_touch as i32);
        server.vm.ent_set_vector(trigger, "mins", [-8.0; 3]);
        server.vm.ent_set_vector(trigger, "maxs", [8.0; 3]);
        link_edict(&mut server.vm, trigger);
        server.vm.gset_float("force_retouch", 1.0);

        let t0 = server.sv_time();
        let Err(crate::QError::Program(e)) = server.run_frame(0.1) else { panic!("the touch's error ends the frame") };
        assert_eq!((e.function.as_str(), e.message.as_str()), ("bad_touch", "NULL function"));
        assert_eq!(server.sv_time(), t0, "the frame never finished: sv.time did not advance");
        let Err(crate::QError::Program(again)) = server.run_frame(0.1) else { panic!("a halted server runs nothing") };
        assert_eq!(again, e);
    }

    #[test]
    fn sv_gravity_cvar_drives_add_gravity() {
        // CENSUS F3: world.qc worldspawn does cvar_set("sv_gravity", "100") on
        // e1m8, and SV_AddGravity reads sv_gravity.value: one 0.1 s frame at
        // 100 gives -10, not -80.
        let (mut server, e) = toss_server();
        assert_eq!(server.sv_gravity(), 800.0, "the cvar's default");
        server.set_sv_gravity(100.0);
        assert_eq!(server.sv_gravity(), 100.0);
        server.run_frame(0.1).expect("frame");
        let vz = server.vm.ent_get_vector(e, "velocity")[2];
        assert!((vz + 10.0).abs() < 1e-3, "sv_gravity 100: expected -10, got {vz}");
        // The cvar outlives the map (SV_SpawnServer never touches it): the
        // front-end hands it to the next server, which keeps 100 until its
        // worldspawn sets it, as id1's does on every map.
        let (mut next, e2) = toss_server();
        assert_eq!(next.sv_gravity(), 800.0, "a server starts from the default");
        next.set_sv_gravity(server.sv_gravity());
        assert_eq!(next.sv_gravity(), 100.0, "the handed-over cvar");
        let name = next.vm.intern("sv_gravity");
        let val = next.vm.intern("800");
        next.vm.set_gi(crate::progs::OFS_PARM0, name);
        next.vm.set_gi(crate::progs::OFS_PARM1, val);
        next.vm.call_builtin(72, 2).expect("cvar_set"); // worldspawn's
        next.run_frame(0.1).expect("frame");
        let vz = next.vm.ent_get_vector(e2, "velocity")[2];
        assert!((vz + 80.0).abs() < 1e-3, "cvar_set(\"sv_gravity\", \"800\"): expected -80, got {vz}");
    }

    // ------------------------------------------------------ water + toss

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
    fn water_transition_sets_watertype_and_splashes() {
        // FIX-3: a stepped entity that crosses from air into water gets its
        // watertype/waterlevel set and plays the misc/h2ohit1.wav splash.
        let progs = Progs::parse(&step_physics_progs()).expect("parse");
        let mut server = Server::new(water_world_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.set_movetype(e, MoveType::Step);
        server.vm.ent_set_float(e, "nextthink", 0.0); // no think
        // ON_GROUND so the freefall block is skipped but CheckWaterTransition
        // still runs unconditionally at the end of SV_Physics_Step.
        server.vm.set_flags(e, EntFlags::ONGROUND);
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
        server.vm.set_movetype(e, MoveType::Step);
        server.vm.set_flags(e, EntFlags::ONGROUND);
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
        server.vm.set_solid(plat, Solid::Bsp);
        server.vm.set_movetype(plat, MoveType::Push);
        server.vm.ent_set_vector(plat, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(plat, "mins", [-64.0, -64.0, -8.0]);
        server.vm.ent_set_vector(plat, "maxs", [64.0, 64.0, 0.0]);
        link_edict(&mut server.vm, plat);

        // A grenade-like toss entity just above the platform, falling.
        let g = server.vm.spawn();
        server.vm.set_movetype(g, MoveType::Toss);
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
        let on_ground = server.vm.flags(g).contains(EntFlags::ONGROUND);
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
        server.vm.set_movetype(p, MoveType::Push);
        server.vm.set_solid(p, Solid::Bsp);
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
        server.vm.set_movetype(p, MoveType::Push);
        server.vm.set_solid(p, Solid::Bsp);
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
        server.vm.set_movetype(r, MoveType::Walk);
        server.vm.set_solid(r, Solid::BBox);
        server.vm.ent_set_vector(r, "origin", [0.0, 0.0, 32.0]);
        server.vm.ent_set_vector(r, "mins", [-8.0, -8.0, -8.0]);
        server.vm.ent_set_vector(r, "maxs", [8.0, 8.0, 8.0]);
        server.vm.set_flags(r, EntFlags::ONGROUND);
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
        server.vm.set_movetype(p, MoveType::Push);
        server.vm.set_solid(p, Solid::Bsp);
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
        server.vm.set_solid(monster, Solid::SlideBox);
        server.vm.set_flags(monster, EntFlags::MONSTER);
        server.vm.ent_set_vector(monster, "origin", [100.0, 0.0, 0.0]);
        server.vm.ent_set_vector(monster, "mins", [-16.0, -16.0, -16.0]);
        server.vm.ent_set_vector(monster, "maxs", [16.0, 16.0, 16.0]);
        server.vm.ent_set_vector(monster, "absmin", [84.0, -16.0, -16.0]);
        server.vm.ent_set_vector(monster, "absmax", [116.0, 16.0, 16.0]);

        // SOLID_BBOX mover (normal): stops on the monster box.
        let blocker = server.vm.spawn();
        server.vm.set_solid(blocker, Solid::BBox);
        server.vm.set_movetype(blocker, MoveType::Bounce);
        server.vm.ent_set_vector(blocker, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(blocker, "maxs", [0.0, 0.0, 0.0]);
        let tr_normal = server.push_entity(blocker, [200.0, 0.0, 0.0], 0.0);
        assert!(
            tr_normal.fraction < 1.0,
            "a Solid::BBox mover (MOVE_NORMAL) is stopped by the monster"
        );

        // SOLID_NOT mover (e.g. a gib): MOVE_NOMONSTERS, passes through.
        let gib = server.vm.spawn();
        server.vm.set_solid(gib, Solid::Not);
        server.vm.set_movetype(gib, MoveType::Bounce);
        server.vm.ent_set_vector(gib, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(gib, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(gib, "maxs", [0.0, 0.0, 0.0]);
        let tr_not = server.push_entity(gib, [200.0, 0.0, 0.0], 0.0);
        assert_eq!(
            tr_not.fraction, 1.0,
            "a Solid::Not mover (MOVE_NOMONSTERS) passes through the monster"
        );

        // SOLID_TRIGGER mover: also MOVE_NOMONSTERS, passes through.
        let trig = server.vm.spawn();
        server.vm.set_solid(trig, Solid::Trigger);
        server.vm.set_movetype(trig, MoveType::Bounce);
        server.vm.ent_set_vector(trig, "origin", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(trig, "mins", [0.0, 0.0, 0.0]);
        server.vm.ent_set_vector(trig, "maxs", [0.0, 0.0, 0.0]);
        let tr_trig = server.push_entity(trig, [200.0, 0.0, 0.0], 0.0);
        assert_eq!(
            tr_trig.fraction, 1.0,
            "a Solid::Trigger mover (MOVE_NOMONSTERS) passes through the monster"
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
        server.vm.set_solid(monster, Solid::SlideBox);
        server.vm.set_flags(monster, EntFlags::MONSTER);
        server.vm.ent_set_vector(monster, "origin", [100.0, 20.0, 0.0]);
        server.vm.ent_set_vector(monster, "mins", [-5.0, -5.0, -5.0]);
        server.vm.ent_set_vector(monster, "maxs", [5.0, 5.0, 5.0]);

        // The rocket: a point box, MOVETYPE_FLYMISSILE, path at y=0.
        let rocket = server.vm.spawn();
        server.vm.set_solid(rocket, Solid::BBox);
        server.vm.set_movetype(rocket, MoveType::FlyMissile);
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
        server.vm.set_movetype(p, MoveType::Toss);
        server.vm.set_flags(p, server.vm.flags(p).without(EntFlags::ONGROUND));

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

    // -------------------------------------------- stepping: plays like 72 Hz

    /// A player on `floor_bsp` launched at 270 u/s (PlayerJump's impulse),
    /// or a bouncing point thrown up at (100, 0, 300), stepped at `hz` until
    /// it lands (or for 2 s): (apex, landing time, where it rests).
    fn launch(hz: f64, stepping: Stepping, bounce: bool) -> (f32, f64, f32) {
        let (img, g_const100, g_origin) = player_progs();
        let mut server = Server::new(floor_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);
        let p = server.connect_client().expect("connect");
        let (e, z0) = if bounce {
            let e = server.vm.spawn();
            server.vm.set_movetype(e, MoveType::Bounce);
            server.vm.ent_set_vector(e, "origin", [0.0, 0.0, 50.0]);
            server.vm.ent_set_vector(e, "velocity", [100.0, 0.0, 300.0]);
            server.vm.ent_set_vector(p, "origin", [-500.0, 0.0, 24.0]);
            (e, 50.0)
        } else {
            server.vm.set_movetype(p, MoveType::Walk);
            server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
            server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
            server.vm.ent_set_vector(p, "origin", [0.0, 0.0, 24.0]);
            server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 270.0]);
            (p, 24.0)
        };
        let (mut apex, mut landed, mut t, mut vz) = (0.0f32, f64::NAN, 0.0f64, 1.0f32);
        while t < 2.0 {
            server.client_frame_stepped(&UserCmd::default(), 1.0 / hz, stepping).expect("frame");
            t += 1.0 / hz;
            apex = apex.max(server.vm.ent_get_vector(e, "origin")[2] - z0);
            let on_ground = server.vm.flags(e).contains(EntFlags::ONGROUND);
            // A bounce turns the fall around within the frame.
            let (was, now) = (vz, server.vm.ent_get_vector(e, "velocity")[2]);
            vz = now;
            if landed.is_nan() && (on_ground || (was < 0.0 && now > 0.0)) {
                landed = t;
                if !bounce {
                    break;
                }
            }
        }
        (apex, landed, server.vm.ent_get_vector(e, "origin")[0])
    }

    /// The uncapped step (`Stepping::Uncapped`) flies a jump and a bounce as
    /// id's 72 Hz frames do, at 60, 144 and 480 Hz, where id's own code
    /// stepped at those rates does not. Tolerances: the apex to 0.05 units;
    /// the landing, sampled at frame ends, to one 72 Hz frame; where the
    /// bouncer comes to rest to 3 units (it bounces off the floor at the
    /// speed its landing frame ends with, which varies with the frame's
    /// phase at any rate). FRAMERATE.md; `quaketool framerate --check` runs
    /// the same comparison through the whole game on the shareware maps.
    #[test]
    fn uncapped_frames_jump_and_bounce_like_72_hz() {
        for bounce in [false, true] {
            let (apex72, land72, rest72) = launch(72.0, Stepping::Classic, bounce);
            // v²/2g less the 72 Hz frame's half-step, v/144: 270 -> 43.70, 300 -> 54.17.
            let expect = if bounce { 54.17 } else { 43.70 };
            assert!((apex72 - expect).abs() < 0.05, "id's apex at 72 Hz: {apex72}");
            for hz in [60.0, 144.0, 480.0] {
                let (apex, land, rest) = launch(hz, Stepping::Uncapped, bounce);
                assert!((apex - apex72).abs() < 0.05, "{hz} Hz apex {apex} vs {apex72}");
                assert!((land - land72).abs() <= 1.0 / 72.0 + 1e-6, "{hz} Hz lands {land} vs {land72}");
                assert!((rest - rest72).abs() < 3.0, "{hz} Hz rests at {rest} vs {rest72}");
                let (classic, _, _) = launch(hz, Stepping::Classic, bounce);
                assert!((classic - apex72).abs() > 0.3, "{hz} Hz: id's per-frame code drifts ({classic})");
            }
        }
    }
}
