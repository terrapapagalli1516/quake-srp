//! The player's own movement: `SV_ClientThink` with its friction /
//! acceleration / swimming helpers, the usercmd copy, and the ideal pitch.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/sv_user.c` — `SV_SetIdealPitch`, `SV_ReadClientMove`,
//!   `SV_ClientThink` + `SV_AirMove`, `SV_WaterMove`, `SV_WaterJump`,
//!   `SV_UserFriction`, `SV_Accelerate`, `SV_AirAccelerate`, `DropPunchAngle`.
//! * `WinQuake/view.c` — `V_CalcRoll`, the strafe lean `SV_ClientThink` puts
//!   on the body (the client view uses it too).
//!
//! These set the player's angles and velocity. The position move that follows
//! (`SV_Physics_Client` → `SV_WalkMove` / `SV_FlyMove`) is sv_phys.c's, in
//! `sv_phys.rs`.

use super::sv_world::sv_move;
use super::{EntFlags, MoveType, Server, UserCmd};
use crate::math::{angle_vectors, Vec3};

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

impl Server {
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
    pub(super) fn set_ideal_pitch(&mut self, ent: i32) {
        const MAX_FORWARD: usize = 6;
        const ON_EPSILON: f32 = 0.1;
        /// `sv_idealpitchscale` default ("0.8").
        const SV_IDEALPITCHSCALE: f32 = 0.8;

        if !self.vm.flags(ent).contains(EntFlags::ONGROUND) {
            return;
        }

        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let view_ofs = self.vm.ent_vec(ent, self.vm.fo().view_ofs);
        let yaw = self.vm.ent_vec(ent, self.vm.fo().angles)[crate::math::YAW];
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
            self.vm.set_ent_float(ent, self.vm.fo().idealpitch, 0.0);
            return;
        }
        if steps < 2 {
            return;
        }
        self.vm
            .set_ent_float(ent, self.vm.fo().idealpitch, -dir * SV_IDEALPITCHSCALE);
    }

    /// `SV_ReadClientMove` (sv_user.c): copy this frame's [`UserCmd`] onto the
    /// player edict before pre-think. Faithfully:
    /// * `v_angle = [pitch, yaw, 0]` (the look angles the netcode delivered);
    /// * `button0 = buttons & 1` (attack);
    /// * `button2 = (buttons & 2) >> 1` (jump);
    /// * `impulse = cmd.impulse` only when non-zero, like the C; the QuakeC
    ///   (ImpulseCommands) clears it once it has acted on it.
    pub(super) fn apply_usercmd_to_edict(&mut self, ent: i32, cmd: &UserCmd) {
        // v_angle before PreThink so weapon aim is correct (client_think later
        // re-derives it from the same cmd during the move).
        self.vm
            .set_ent_vec(ent, self.vm.fo().v_angle, [cmd.pitch, cmd.yaw, 0.0]);
        self.vm
            .set_ent_float(ent, self.vm.fo().button0, (cmd.buttons & 1) as f32);
        self.vm
            .set_ent_float(ent, self.vm.fo().button2, ((cmd.buttons & 2) >> 1) as f32);
        // The C only assigns impulse when the byte is non-zero (a 0 impulse means
        // "no command this frame"); the QuakeC clears it when it runs it.
        if cmd.impulse != 0 {
            self.vm.set_ent_float(ent, self.vm.fo().impulse, cmd.impulse as f32);
        }
    }

    /// `SV_ClientThink` + `SV_AirMove` (sv_user.c): apply the usercmd angles to
    /// `v_angle`/`angles`, build the wish velocity from the move axes and angle
    /// vectors, then friction + acceleration toward `wishdir` (ground) or air
    /// acceleration (airborne). This sets `velocity`; the actual position move
    /// happens afterward in [`Self::walk_move`] / [`Self::player_fly_move`].
    pub(super) fn client_think(&mut self, ent: i32, cmd: &UserCmd, dt: f32) {
        if self.vm.movetype(ent) == MoveType::None {
            return;
        }

        let on_ground = self.vm.flags(ent).contains(EntFlags::ONGROUND);

        // DropPunchAngle: decay the view kick toward zero.
        self.drop_punch_angle(ent, dt);

        // if dead, behave differently (no movement)
        if self.vm.ent_float(ent, self.vm.fo().health) <= 0.0 {
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
        let fixangle = self.vm.ent_float(ent, self.vm.fo().fixangle);

        // v_angle field = [pitch, yaw, roll] from the incoming command (the C
        // SV_ReadClientMove writes this before SV_ClientThink).
        self.vm
            .set_ent_vec(ent, self.vm.fo().v_angle, [cmd.pitch, cmd.yaw, 0.0]);

        // Local v_angle including the punch kick (the C `VectorAdd` into a temp;
        // the stored v_angle field is NOT modified by the punch).
        let punchangle = self.vm.ent_vec(ent, self.vm.fo().punchangle);
        let v_angle_kick = [
            cmd.pitch + punchangle[crate::math::PITCH],
            cmd.yaw + punchangle[crate::math::YAW],
            punchangle[crate::math::ROLL],
        ];

        // angles[ROLL] = V_CalcRoll(current angles, velocity) * 4 — read the
        // PRE-update angles + velocity, exactly as the C does before assigning
        // pitch/yaw.
        let cur_angles = self.vm.ent_vec(ent, self.vm.fo().angles);
        let velocity = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let roll = v_calc_roll(cur_angles, velocity) * 4.0;

        if fixangle == 0.0 {
            self.vm.set_ent_vec(
                ent,
                self.vm.fo().angles,
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
                .set_ent_vec(ent, self.vm.fo().angles, [cur_angles[0], cur_angles[1], roll]);
            self.vm.set_ent_float(ent, self.vm.fo().fixangle, 0.0);
        }

        // SV_ClientThink: waist-deep in water (and not noclip) -> swim, then
        // return — mirrors sv_user.c dispatching SV_WaterMove ahead of SV_AirMove.
        // `waterlevel` is the prior frame's check_water value (the WALK arm runs
        // check_water AFTER client_think), matching id's two-pass phasing where
        // SV_RunClients precedes SV_Physics.
        let movetype = self.vm.movetype(ent);
        // SV_WaterJump (sv_user.c:414): a QuakeC-set climb-out (FL_WATERJUMP, from
        // CheckWaterJump) forces a horizontal launch toward movedir until the
        // timer expires or you leave the water — checked before the swim/air move.
        let flags = self.vm.flags(ent);
        if flags.contains(EntFlags::WATERJUMP) {
            self.water_jump(ent);
            return;
        }
        let waterlevel = self.vm.ent_float(ent, self.vm.fo().waterlevel) as i32;
        if movetype != MoveType::NoClip && waterlevel >= 2 {
            self.water_move(ent, cmd, dt);
            return;
        }

        // SV_AirMove: wishvel from forward/side and the look angles.
        let angles = self.vm.ent_vec(ent, self.vm.fo().angles);
        let (forward, right, _up) = crate::math::angle_vectors(angles);
        let mut fmove = cmd.forwardmove;
        let smove = cmd.sidemove;

        // hack to not let you back into the teleporter you just left.
        let teleport_time = self.vm.ent_float(ent, self.vm.fo().teleport_time);
        // sv.time < sv_player->v.teleport_time: a double against a float.
        if self.sv_time() < f64::from(teleport_time) && fmove < 0.0 {
            fmove = 0.0;
        }

        let mut wishvel = [
            forward[0] * fmove + right[0] * smove,
            forward[1] * fmove + right[1] * smove,
            forward[2] * fmove + right[2] * smove,
        ];

        // (movetype was read above for the water-move dispatch.)
        if movetype != MoveType::Walk {
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

        if movetype == MoveType::NoClip {
            // noclip: velocity follows the wish directly.
            self.vm.set_ent_vec(ent, self.vm.fo().velocity, wishvel);
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
        let v_angle = self.vm.ent_vec(ent, self.vm.fo().v_angle);
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
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let speed = crate::math::length(vel);
        let newspeed = if speed != 0.0 {
            let ns = (speed - dt * speed * SV_FRICTION).max(0.0);
            vel = crate::math::scale(vel, ns / speed);
            self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
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
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// `SV_WaterJump` (sv_user.c:307): while FL_WATERJUMP is set (QuakeC's
    /// CheckWaterJump flagged a ledge climb-out), force horizontal velocity to
    /// `movedir` so the player is thrown up onto the ledge; clear the flag once
    /// the timer expires (`sv.time > teleport_time`) or the player left the water.
    fn water_jump(&mut self, ent: i32) {
        let teleport_time = self.vm.ent_float(ent, self.vm.fo().teleport_time);
        let waterlevel = self.vm.ent_float(ent, self.vm.fo().waterlevel) as i32;
        if self.sv_time() > f64::from(teleport_time) || waterlevel == 0 {
            let flags = self.vm.flags(ent);
            self.vm.set_flags(ent, flags.without(EntFlags::WATERJUMP));
            self.vm.set_ent_float(ent, self.vm.fo().teleport_time, 0.0);
        }
        let movedir = self.vm.ent_vec(ent, self.vm.fo().movedir);
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        vel[0] = movedir[0];
        vel[1] = movedir[1];
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// `SV_UserFriction` (sv_user.c): bleed off horizontal speed, with extra
    /// friction (`edgefriction`) when the leading edge hangs over a dropoff.
    fn user_friction(&mut self, ent: i32, dt: f32) {
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
        let speed = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        if speed == 0.0 {
            return;
        }

        // If the leading edge is over a dropoff, increase friction. The C traces
        // a *point* (mins=maxs=0) 34 units down, 16 units ahead, from the bottom
        // of the player box, ignoring the player.
        let origin = self.vm.ent_vec(ent, self.vm.fo().origin);
        let pmins = self.vm.ent_vec(ent, self.vm.fo().mins);
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
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// `SV_Accelerate` (sv_user.c): push velocity toward `wishdir` up to
    /// `wishspeed` by at most `sv_accelerate * dt * wishspeed` this tick.
    fn accelerate(&mut self, ent: i32, wishdir: Vec3, wishspeed: f32, dt: f32) {
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
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
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// `SV_AirAccelerate` (sv_user.c): like `SV_Accelerate` but the *target*
    /// speed is capped at 30, while the acceleration is scaled by the original
    /// (un-capped) `wishspeed` — a faithful transcription of id's exact code,
    /// `wishvel` normalized in place to give both `wishspeed` and the direction.
    fn air_accelerate(&mut self, ent: i32, wishveloc: Vec3, dt: f32) {
        let (dir, wishspeed) = crate::math::normalize(wishveloc);
        let wishspd = if wishspeed > 30.0 { 30.0 } else { wishspeed };
        let mut vel = self.vm.ent_vec(ent, self.vm.fo().velocity);
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
        self.vm.set_ent_vec(ent, self.vm.fo().velocity, vel);
    }

    /// `DropPunchAngle` (sv_user.c): decay the view-kick vector by `10*dt` units
    /// of length toward zero.
    fn drop_punch_angle(&mut self, ent: i32, dt: f32) {
        let punch = self.vm.ent_vec(ent, self.vm.fo().punchangle);
        let (dir, mut len) = crate::math::normalize(punch);
        len -= 10.0 * dt;
        if len < 0.0 {
            len = 0.0;
        }
        self.vm
            .set_ent_vec(ent, self.vm.fo().punchangle, crate::math::scale(dir, len));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Op, Progs, Statement};
    use crate::server::testutil::*;

    // -------------------------------------------------------------- the player

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
        let flags = server.vm.flags(p);
        server
            .vm.set_flags(p, flags.with(EntFlags::ONGROUND));

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
        let flags = server.vm.flags(p);
        server
            .vm.set_flags(p, flags.with(EntFlags::ONGROUND));
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
        let flags = server.vm.flags(p);
        server
            .vm.set_flags(p, flags.with(EntFlags::ONGROUND));
        server.client_frame(&cmd, 0.1).expect("frame");
        let v1 = server.vm.ent_get_vector(p, "velocity");
        assert!(
            v1[0] > 0.0,
            "velocity moved toward +X wishdir, got {v1:?}"
        );

        // Many ticks: horizontal speed never exceeds sv_maxspeed.
        for _ in 0..40 {
            let flags = server.vm.flags(p);
            server
                .vm.set_flags(p, flags.with(EntFlags::ONGROUND));
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
            op: Op::Done,
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
                    op: Op::LoadF,
                    a: SELF as i16,
                    b: g_fimpulse as i16,
                    c: g_tmp as i16,
                },
                Statement {
                    op: Op::StoreF,
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
}
