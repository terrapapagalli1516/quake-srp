//! The live client frame — `step_walk`, one frame of the player's game as
//! the C client runs it against a local server: `CL_SendMove` (cl_input.c)
//! into the server tick, the client side of `CL_ParseServerMessage`
//! (cl_parse.c: intermission/finale, sounds, prints, particles, temp
//! entities), `CL_RelinkEntities` (cl_main.c), `V_CalcRefdef` (view.c) and
//! `SCR_UpdateScreen`'s 3-D view, blends, status bar and overlays (screen.c).

use quake_rs::bsp::Bsp;
use quake_rs::mdl::Mdl;
use quake_rs::render::{self, Camera, ModelInstance, Viewmodel};
use quake_rs::server::UserCmd;
use quake_rs::tent::BeamModel;

use crate::app::{color_for_name, Walk};
use crate::bench::{self, Phase};
use crate::cl_tent::{rocket_trail_type, spawn_temp_entity};
use crate::host_cmd::{try_changelevel, try_restart, FL_ONGROUND, IT_INVISIBILITY};
use crate::input::{
    clamp_pitch, KeyMove, CL_ANGLESPEEDKEY, CL_PITCHSPEED, CL_YAWSPEED, SPEED, V_CENTERSPEED,
};
use crate::snd_dma::{queue_sounds, update_ambient_channels, Listener, LISTENER};
use crate::vid::backtile_for;
use crate::view::{
    cshift_add, cshift_drop, parse_damage, stamp_item_gettime, stufftext_bonus_flash, BONUS_COLOR,
    BONUS_FADE, BONUS_PERCENT, DAMAGE_FADE, FACE_ANIM_TIME, V_KICKTIME,
};

/// An angle as it crosses the wire in `svc_setangle`: `MSG_WriteAngle`
/// (`((int)f*256/360) & 255`) then `MSG_ReadAngle` (`MSG_ReadChar() *
/// (360.0/256)`) — whole degrees truncated, then 256 steps, signed.
pub(crate) fn net_angle(f: f32) -> f32 {
    let b = ((f as i32).wrapping_mul(256) / 360) & 255;
    (b as u8 as i8) as f32 * (360.0 / 256.0)
}

/// `SV_WriteClientdataToMessage`'s fixangle (sv_main.c) and the client's
/// `svc_setangle` (cl_parse.c): when the QuakeC forced the player's facing
/// (`fixangle = 1` — teleporters, the intermission camera, PutClientInServer),
/// the server sends the entity's `angles` and clears the flag, and the client
/// takes them as `cl.viewangles`. This shell has no view roll, so the roll the
/// message carries (0 at every id1 site) is dropped; the pitch is clamped as
/// CL_AdjustAngles clamps it on the next move.
fn apply_fixangle(w: &mut Walk) {
    let p = w.player;
    if p < 0 || w.server.vm.ent_get_float(p, "fixangle") == 0.0 {
        return;
    }
    let a = w.server.vm.ent_get_vector(p, "angles");
    w.pitch = clamp_pitch(net_angle(a[0]));
    w.yaw = net_angle(a[1]);
    w.server.vm.ent_set_float(p, "fixangle", 0.0);
}

/// `SV_WriteClientdataToMessage`'s svc_damage (sv_main.c) read by the client's
/// `V_ParseDamage` (view.c). QC `T_Damage` adds to the client's `dmg_take` /
/// `dmg_save` (and sets `dmg_inflictor`) BEFORE its god-mode and Pentagram
/// returns, so a hit that costs no health still flashes, kicks and shows the
/// pain face. The server sends them whenever either is non-zero and zeroes
/// them: `MSG_WriteByte` of each (float truncated, then a byte) and the
/// inflictor's box centre through `MSG_WriteCoord` (1/8 unit). `ent_origin`
/// is the view entity's origin as the client has it when the message is
/// parsed — last frame's (clientdata precedes the entity updates).
fn parse_client_damage(w: &mut Walk, ent_origin: [f32; 3]) {
    let p = w.player;
    let vm = &mut w.server.vm;
    let take = vm.ent_get_float(p, "dmg_take");
    let save = vm.ent_get_float(p, "dmg_save");
    if take == 0.0 && save == 0.0 {
        return;
    }
    let mut other = vm.ent_get_int(p, "dmg_inflictor");
    if other < 0 || other as usize >= vm.num_edicts() {
        other = 0;
    }
    let (o, mins, maxs) = (
        vm.ent_get_vector(other, "origin"),
        vm.ent_get_vector(other, "mins"),
        vm.ent_get_vector(other, "maxs"),
    );
    let coord = |i: usize| ((o[i] + 0.5 * (mins[i] + maxs[i])) * 8.0) as i32 as i16 as f32 / 8.0;
    let from = [coord(0), coord(1), coord(2)];
    vm.ent_set_float(p, "dmg_take", 0.0);
    vm.ent_set_float(p, "dmg_save", 0.0);
    let byte = |f: f32| (f as i32) & 255;
    let pd = parse_damage(byte(save), byte(take), from, ent_origin, [w.pitch, w.yaw, 0.0]);
    w.damage_blend = cshift_add(w.damage_blend, pd.percent);
    w.damage_color = pd.color;
    w.v_dmg_roll = pd.roll;
    w.v_dmg_pitch = pd.pitch;
    w.v_dmg_time = V_KICKTIME;
    w.faceanimtime = w.server.time() + FACE_ANIM_TIME;
}

/// The dynamic lights R_PushDlights marks this frame: every slot with a
/// radius whose `die` has not passed (`die < cl.time || !radius` is skipped),
/// at its current — not yet decayed — radius.
fn pushed_dlights(
    dlights: &quake_rs::dlight::DynamicLights,
    now: f32,
) -> Vec<quake_rs::dlight::DynamicLight> {
    dlights.active().into_iter().filter(|dl| dl.die >= now).collect()
}

/// `cl.punchangle` as SV_WriteClientdataToMessage sends it: each component
/// through `MSG_WriteChar` — the float truncated to an int, kept as a signed
/// byte — so the shotgun's -2 kick reads -2, then -1 while DropPunchAngle eases
/// the server's value back, then 0: whole-degree steps, not a smooth ease.
pub(crate) fn client_punchangle(w: &Walk) -> [f32; 3] {
    let p = w.server.vm.ent_get_vector(w.player, "punchangle");
    p.map(|v| (v as i32) as i8 as f32)
}

/// `cl.items` as SV_WriteClientdataToMessage sends it: the player's `items`
/// with the rune bits "stuffed into the high bits of items for sbar" —
/// `(int)ent->v.items | ((int)pr_global_struct->serverflags << 28)`. QC
/// `sigil_touch` only sets `serverflags`, so this is how a rune reaches the
/// status bar.
pub(crate) fn client_items(w: &Walk) -> i32 {
    (w.server.vm.ent_get_float(w.player, "items") as i32) | ((w.server.serverflags() as i32) << 28)
}

/// Owned visible-entity descriptor gathered from the server before rendering:
/// `(model name, origin, angles, frame, shirt/pants colour, skin)`.
type EntityDesc = (String, [f32; 3], [f32; 3], usize, [u8; 3], i32);

pub(crate) fn step_walk(
    w: &mut Walk,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> (render::Image, Vec<([u8; 3], f32)>) {
    // Con_CheckResize: the notify lines are laid out con_linewidth wide.
    w.notify.check_resize(render_w, render_h);
    // Host_ServerFrame (host.c): "always pause in single player if in console
    // or menus" — `if (!sv.paused && (svs.maxclients > 1 || key_dest ==
    // key_game)) SV_Physics ();`, and SV_RunClients gates SV_ClientThink the
    // same way. Nothing on the server runs while the menu or console is up, so
    // sv.time stands still, and with it cl.time (on a local server CL_LerpPoint
    // snaps cl.time to the server's message time): particles, dlight decay,
    // light styles, sky and liquids, rotating pickups and the status-bar
    // animations freeze. The client frame itself still runs — V_RenderView
    // draws the view, and whatever the C drives off host_frametime / realtime
    // keeps going: the palette-shift fades (V_UpdatePalette), the centerprint
    // countdown and notify expiry, the ambient-sound ramps.
    let paused = menu_up;
    // Guard against a non-finite/negative dt so the clock only moves forward.
    // (cl.time, `w.clock`, follows the server's clock below.)
    if dt.is_finite() && dt > 0.0 {
        w.host_time += dt;
    }

    // 1. Tick the live server with this frame's input. forwardmove/sidemove are
    //    Quake run speeds; the server's SV_ClientThink turns them into motion and
    //    runs every entity's think (so monsters animate and move).
    // The bindings-driven keyboard input `step` derived this frame; zeroed while
    // the menu/console gate gameplay (key_dest != key_game).
    let km = if menu_up { KeyMove::default() } else { w.key_move };

    // CL_AdjustAngles (cl_input.c), run before CL_BaseMove builds the cmd like
    // CL_SendMove does: the keyboard turn/look keys move the view angles at
    // cl_yawspeed/cl_pitchspeed deg/sec (x cl_anglespeedkey with +speed held).
    if dt.is_finite() && dt > 0.0 {
        let aspeed = dt * if km.speed { CL_ANGLESPEEDKEY } else { 1.0 };
        w.yaw += aspeed * CL_YAWSPEED * km.turn;
        if km.look != 0.0 {
            // PITCH -= speed*cl_pitchspeed*up (look up = pitch down numerically);
            // "if (up || down) V_StopPitchDrift()"; clamp 80/-70 (clamp_pitch).
            w.pitch = clamp_pitch(w.pitch - aspeed * CL_PITCHSPEED * km.look);
            w.pitch_drift = false;
            w.pitch_vel = 0.0;
        }
        // V_DriftPitch (view.c), the active-drift arm: centerview / a lookspring
        // pointer-unlock seeded pitch_vel and the view re-levels toward the
        // ideal pitch. SIMPLIFICATION: cl.idealpitch is fixed at 0 here (the C
        // computes a walking idealpitch from ground slope; this port never
        // does, so level is the only ideal) and the C's nodrift/driftmove
        // re-arm bookkeeping is unneeded — drifting starts only at the two
        // explicit triggers. The velocity integration matches the C: move =
        // frametime*pitchvel, pitchvel += frametime*v_centerspeed, overshoot
        // clamps to the target and stops.
        if w.pitch_drift && !menu_up {
            let delta = -w.pitch; // idealpitch (0) - viewangles[PITCH]
            if delta == 0.0 {
                w.pitch_vel = 0.0;
                w.pitch_drift = false;
            } else {
                let mut mv = dt * w.pitch_vel;
                w.pitch_vel += dt * V_CENTERSPEED;
                if mv > delta.abs() {
                    mv = delta.abs();
                    w.pitch_vel = 0.0;
                    w.pitch_drift = false;
                }
                w.pitch += mv * delta.signum();
            }
        }
    }

    // The legacy analog set_move fractions (tests/automation), normalised so a
    // diagonal can't exceed 1, at the old fixed 320 scale.
    let (mut fwd, mut side) = if menu_up { (0.0, 0.0) } else { (w.in_fwd, w.in_side) };
    let mag = (fwd * fwd + side * side).sqrt();
    if mag > 1.0 {
        fwd /= mag;
        side /= mag;
    }
    // IN_MouseMove's accumulated sidemove/forwardmove contributions (lookstrafe
    // / +strafe routing). Take them even when gated so a stale accumulation
    // can't fire after the menu closes (mouse_move is gated too, so these are
    // zero behind an overlay anyway).
    let mouse_side = std::mem::take(&mut w.mouse_side);
    let mouse_fwd = std::mem::take(&mut w.mouse_fwd);
    let cmd = UserCmd {
        // CL_BaseMove composition: keyboard (km, real cl_* cvar speeds) +
        // mouse strafe units + the legacy analog path. The server's
        // SV_AirMove clamps wishspeed to sv_maxspeed exactly like the C.
        forwardmove: fwd * SPEED + km.fwd + if menu_up { 0.0 } else { mouse_fwd },
        sidemove: side * SPEED + km.side + if menu_up { 0.0 } else { mouse_side },
        // Vertical swim intent: Space (jump) = up, c (movedown) = down. Quake's
        // SV_WaterMove consumes upmove while waist-deep; the ground/air move
        // ignores it, so on land Space still just jumps and c does nothing.
        // The legacy set_jump/set_movedown booleans keep their old 320 scale;
        // the key path contributes at cl_upspeed via km.up.
        upmove: km.up
            + if menu_up {
                0.0
            } else {
                ((if w.in_jump { 1.0 } else { 0.0 }) - (if w.in_down { 1.0 } else { 0.0 }))
                    * SPEED
            },
        yaw: w.yaw,
        pitch: w.pitch,
        buttons: if menu_up {
            0
        } else {
            (if w.in_attack || km.attack { 1 } else { 0 })
                | (if w.in_jump || km.jump { 2 } else { 0 })
        },
        impulse: if menu_up { 0 } else { w.next_impulse },
    };
    if !paused {
        // A queued impulse is sent once (CL_SendMove: `in_impulse = 0`). While
        // paused it waits: the C's SV_ReadClientMove still stores it on the
        // edict behind the menu, and it runs when the server does.
        w.next_impulse = 0;
        let before = w.server.vm.ent_get_vector(w.player, "origin");
        let _ = w.server.client_frame(&cmd, dt);
        // CL_LerpPoint on a local server: cl.time = the message time, sv.time
        // after this frame's physics.
        w.clock = w.server.time();
        apply_fixangle(w);
        parse_client_damage(w, before);
        // svc_stufftext to this client (PF_stuffcmd): the bonus flash.
        for (ent, text) in quake_rs::builtins::take_stufftext() {
            if ent == w.player && stufftext_bonus_flash(&text) {
                w.bonus_blend = BONUS_PERCENT;
            }
        }
    }

    // 1a. MSG_ALL server commands (CL_ParseServerMessage, cl_parse.c): the QuakeC
    //     end-of-level chain WriteBytes svc_intermission / svc_finale (+ text) /
    //     svc_sellscreen to every client; play the client role here — enter
    //     intermission mode, latch cl.completed_time, start the finale reveal.
    for ev in w.server.drain_svc_events() {
        match ev {
            quake_rs::server::SvcEvent::Intermission => {
                // cl.intermission = 1; cl.completed_time = cl.time (cl_parse.c:939).
                // On a local server cl.time is sv.time (`Walk::clock`), which
                // SV_SpawnServer starts at 1.0, so the overlay's minutes:seconds
                // shows exactly what vanilla shows. (The demo parser latches
                // mtime[0], also server time — the paths agree.)
                w.intermission = 1;
                w.completed_time = w.server.time();
            }
            quake_rs::server::SvcEvent::Finale(text) => {
                // cl.intermission = 2 + SCR_CenterPrint (scr_centertime_start).
                // completed_time = cl.time = sv.time, as above; the reveal uses
                // w.clock - finale_start (cl.time - scr_centertime_start in the C).
                w.intermission = 2;
                w.completed_time = w.server.time();
                w.finale_text = text;
                w.finale_start = w.clock;
            }
            quake_rs::server::SvcEvent::Cutscene(text) => {
                // cl.intermission = 3 (text only, no plaque); times as per Finale.
                w.intermission = 3;
                w.completed_time = w.server.time();
                w.finale_text = text;
                w.finale_start = w.clock;
            }
            quake_rs::server::SvcEvent::SellScreen => {
                // Cmd_ExecuteString("help"): the dispatcher opens the Help menu.
                w.pending_sellscreen = true;
            }
        }
    }

    // 1b. Level transition: a trigger_changelevel the player crossed this frame
    //     ran the QuakeC changelevel() builtin, which only *recorded* the next
    //     map (it cannot swap mid-frame). Now that the frame has finished, save
    //     the player's spawn parms (inventory) and swap to the new level,
    //     reconnecting the client so DecodeLevelParms restores the carried
    //     inventory. A missing/bad map leaves the current level running.
    if let Some(next_map) = w.server.take_pending_changelevel() {
        try_changelevel(w, &next_map);
        // The swap reset the world; render this frame from the *new* level so the
        // player never sees a frame straddling two maps.
    } else if w.server.take_pending_restart() {
        // Single-player respawn: QuakeC ran localcmd("restart") (a dead player who
        // pressed a button). Reload the current level with the entry inventory.
        // `else if` so a changelevel this frame takes precedence over a restart.
        try_restart(w);
    }

    // CL_ParseClientdata's item get-times (the new-weapon icon flash), on the
    // server clock the HUD reads — after any level swap above, whose
    // CL_ClearState zeroed cl.items.
    let items = client_items(w);
    let now_sv = w.server.time();
    stamp_item_gettime(&mut w.cl_items, &mut w.item_gettime, items, now_sv);

    // 2. Surface the sounds the world fired this frame (gunshots, doors, monster
    //    voices) to the page's audio queue.
    let events = w.server.drain_sounds();
    queue_sounds(&w.pak, &events, w.player);

    // 2a. Drain QuakeC's on-screen messages (centerprint / sprint / bprint) into
    //     the timed display state, and expire old ones on the host clock
    //     (the C times both off realtime / host_frametime, paused or not).
    for m in w.server.drain_messages() {
        if m.center {
            w.centerprint = Some((m.text, w.host_time + 2.0));
        } else {
            // Con_Print: pickups print via several sprint() calls ("You receive
            // ", "25", " health\n") that land on one console line.
            w.notify.print(&m.text, w.host_time);
        }
    }
    if let Some((_, exp)) = &w.centerprint {
        if w.host_time >= *exp {
            w.centerprint = None;
        }
    }

    // 2b. Realise the particle() bursts the world fired this frame (explosions,
    //     blood, gibs) into the live pool, then age it under gravity and retire
    //     expired particles. Spawn uses the current game clock for absolute
    //     lifetimes; advance uses sv_gravity*0.05 as the particle gravity factor.
    let now = w.clock;
    for b in w.server.drain_particles() {
        w.particles.spawn_burst(b.org, b.dir, b.color, b.count, now, &mut w.prng);
    }
    // 2c. Realise the temp entities (rocket/grenade explosions, bullet/spike wall
    //     impacts) the world fired via the Write* builtins. Explosions also queue
    //     their `weapons/r_exp3.wav` sound through the SAME spatial-audio path the
    //     other sounds use, with the explosion's world position as its origin.
    let tents = w.server.drain_temp_entities();
    let mut te_sounds: Vec<quake_rs::server::SoundEvent> = Vec::new();
    for ev in &tents {
        // Beam types (CL_ParseTEnt's TE_LIGHTNING1/2/3 + TE_BEAM cases): refresh
        // the entity's beam slot (CL_ParseBeam) and load its bolt model now —
        // the C's `CL_ParseBeam(Mod_ForName("progs/bolt*.mdl", true))` loads at
        // parse time too. A missing model (shareware lacks beam.mdl) caches
        // `None` and the expansion below skips its pieces (the C Sys_Error'd;
        // vanilla progs never emits TE_BEAM, so the path was never live).
        if let Some(bm) = BeamModel::from_te_type(ev.te_type) {
            w.beams.parse_beam(ev.entity, bm, ev.pos, ev.end, now);
            let name = bm.model_name();
            if !w.model_cache.contains_key(name) {
                let parsed =
                    w.pak.read_file(name).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
                w.model_cache.insert(name.to_string(), parsed);
            }
            continue; // beams spawn no particles / sounds / dlights here
        }
        // Explosions spawn a decaying dynamic light (CL_ParseTEnt): radius 350,
        // die now+0.5, decay 300, minlight 0, key 0 -> a fresh slot each one.
        {
            use quake_rs::server::te_consts::*;
            // Only TE_EXPLOSION and TE_EXPLOSION2 flash a dynamic light in id's
            // CL_ParseTEnt; TE_TAREXPLOSION (blob) does NOT.
            if matches!(ev.te_type, TE_EXPLOSION | TE_EXPLOSION2) {
                w.dlights.alloc(0, ev.pos, 350.0, now + 0.5, 300.0, 0.0, now);
            }
        }
        if let Some(name) = spawn_temp_entity(&mut w.particles, ev, now, &mut w.prng) {
            te_sounds.push(quake_rs::server::SoundEvent {
                entity: 0,
                channel: 0,
                sound_index: -1,
                sample: name.to_string(),
                origin: ev.pos,
                volume: 1.0,
                attenuation: 1.0,
            });
        }
    }
    if !te_sounds.is_empty() {
        // Temp-entity sounds (explosions, wall impacts) carry entity=0,
        // channel=0 -> never the view entity, never channel-restarted, so each
        // distinct explosion queues separately at its own origin.
        queue_sounds(&w.pak, &te_sounds, w.player);
    }
    // 2d. Entity light effects (EF_MUZZLEFLASH / BRIGHTLIGHT / DIMLIGHT) from the
    //     in-use edicts. The rand()&31 radius jitter is added here (entity_dlights
    //     stays a pure query). Then decay + retire the whole pool for this frame.
    for ed in w.server.entity_dlights() {
        let jitter = w.prng.next_range(32) as f32;
        w.dlights.alloc(
            ed.key,
            ed.origin,
            ed.radius_base + jitter,
            now + ed.life,
            0.0,
            ed.minlight,
            now,
        );
    }
    // 3. Make sure every live entity's alias model is cached (runtime-spawned
    //    entities — gibs, projectiles — can appear after boot).
    let n = w.server.vm.num_edicts();
    for e in 0..n {
        if w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let m = w.server.vm.ent_get_string(e as i32, "model");
        if m.ends_with(".mdl") && !w.model_cache.contains_key(&m) {
            let parsed = w.pak.read_file(&m).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
            w.model_cache.insert(m, parsed);
        } else if m.ends_with(".bsp") && m != w.map_name && !w.bmodel_cache.contains_key(&m) {
            // An external brush-model item box (maps/b_*.bsp). Parse once and cache;
            // a missing/unparseable box stores `None` so we never re-read or panic.
            let parsed = w.pak.read_file(&m).ok().flatten().and_then(|b| Bsp::parse(&b).ok());
            w.bmodel_cache.insert(m, parsed);
        } else if m.ends_with(".spr") && !w.sprite_cache.contains_key(&m) {
            // A sprite-model entity (progs/s_explod.spr explosion flash, bubbles).
            // Parse once and cache; None on missing/unparseable.
            let parsed =
                w.pak.read_file(&m).ok().flatten().and_then(|b| quake_rs::spr::Sprite::parse(&b).ok());
            w.sprite_cache.insert(m, parsed);
        }
    }

    // The player's first-person weapon viewmodel ("progs/v_shot.mdl" etc.) lives
    // on the `weaponmodel` field (separate from `model`); cache it like any MDL.
    let weapon_name = w.server.vm.ent_get_string(w.player, "weaponmodel");
    if weapon_name.ends_with(".mdl") && !w.model_cache.contains_key(&weapon_name) {
        let parsed = w.pak.read_file(&weapon_name).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
        w.model_cache.insert(weapon_name.clone(), parsed);
    }
    let weapon_frame = w.server.vm.ent_get_float(w.player, "weaponframe").max(0.0) as usize;

    // 4. Gather the visible entities (owned descriptors, so the cache borrow for
    //    rendering doesn't clash with reading the server). Skip the player's own
    //    edict — its model would fill the screen in first person.
    // (model name, origin, angles, frame, shirt/pants colour, skin) per entity.
    let mut descs: Vec<EntityDesc> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    // Projectile/gib trails to spawn this frame, collected here and emitted after
    // the loop (so we don't borrow w.particles/dlights while reading the server):
    // (entity, old origin, new origin, R_RocketTrail type).
    let mut trail_spawns: Vec<(i32, [f32; 3], [f32; 3], i32)> = Vec::new();
    // External brush-model items (maps/b_*.bsp) as owned (name, origin) pairs; the
    // borrowing `ExternalBModel` list is built below, after the cache is final, so
    // the immutable cache borrow does not clash with reading the server here.
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    // Sprite-model entities (name, origin, frame): the explosion flash, bubbles.
    // Resolved against the sprite cache after the loop (disjoint borrows).
    let mut sprite_descs: Vec<(String, [f32; 3], usize)> = Vec::new();
    // Drop trail history for any edict that is currently free. When `ED_Free`
    // recycles a slot for a new trailed entity (rocket/grenade/gib), a stale
    // `trail_org[ent]` from the previous occupant would make R_RocketTrail draw a
    // spurious streak from the old entity's last origin to the new spawn point.
    // Pruning here lets a reused slot start fresh (oldorg defaults to its own
    // origin below, so no trail on the first frame). Disjoint field borrows.
    {
        let vm = &w.server.vm;
        w.trail_org
            .retain(|&e, _| !vm.edict_free.get(e as usize).copied().unwrap_or(true));
    }
    for e in 0..n {
        let ent = e as i32;
        if ent == w.player || w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        // Render an entity only when it has a real modelindex — i.e. its QuakeC
        // spawn actually called setmodel. An edict that early-returns before
        // setmodel (e.g. func_episodegate in shareware: serverflags=0 so the gate
        // stays passable) keeps its raw map `model` key like "*41" but never gets
        // a modelindex, and Quake leaves it invisible. Without this guard those
        // gates draw as phantom walls the player walks through — and mask the real
        // slipgate behind them, so episode/level selection *looks* broken.
        if w.server.vm.ent_get_float(ent, "modelindex") == 0.0 {
            continue;
        }
        let m = w.server.vm.ent_get_string(ent, "model");
        // Brush submodels (doors, platforms, buttons) draw at the entity origin —
        // their origin tracks the door's open/close motion, so they animate live.
        if let Some(num) = m.strip_prefix('*') {
            if let Ok(idx) = num.parse::<usize>() {
                let origin = w.server.vm.ent_get_vector(ent, "origin");
                // The entity's `frame` selects the alternate (+a..+j) texture cycle
                // for activated buttons/doors (a pressed button shows its lit face).
                let frame = w.server.vm.ent_get_float(ent, "frame") as i32;
                bmodels.push(render::BModelInstance { model_index: idx, origin, frame });
            }
            continue;
        }
        // External brush-model item boxes: a standalone b_*.bsp the item set as its
        // model (explosive box, ammo/health boxes). Not the world map itself.
        if m.ends_with(".bsp") {
            if m != w.map_name {
                let origin = w.server.vm.ent_get_vector(ent, "origin");
                ext_descs.push((m, origin));
            }
            continue;
        }
        // Sprite-model entities (s_explod.spr explosion flash, bubbles): a camera-
        // facing billboard at the entity origin, current `frame` for the animation.
        if m.ends_with(".spr") {
            let origin = w.server.vm.ent_get_vector(ent, "origin");
            let frame = w.server.vm.ent_get_float(ent, "frame").max(0.0) as usize;
            sprite_descs.push((m, origin, frame));
            continue;
        }
        if !m.ends_with(".mdl") {
            continue;
        }
        let origin = w.server.vm.ent_get_vector(ent, "origin");
        let frame = w.server.vm.ent_get_float(ent, "frame").max(0.0) as usize;
        let color = color_for_name(&m);
        // The model header flags (rocket/grenade/gib/tracer trails + EF_ROTATE).
        let mflags = w
            .model_cache
            .get(&m)
            .and_then(|o| o.as_ref())
            .map(|md| md.header.flags)
            .unwrap_or(0);
        // CL_RelinkEntities (cl_main.c:531): a model carrying EF_ROTATE (bonus
        // pickups — ammo/health/armour boxes, weapons, keys, runes, powerups) has
        // its yaw overwritten with `anglemod(100*cl.time)` every frame so it spins.
        // Otherwise use the entity's own yaw. Without this every pickup sat frozen.
        let ent_angles = w.server.vm.ent_get_vector(ent, "angles");
        let yaw = if mflags & quake_rs::demo::EF_ROTATE != 0 {
            quake_rs::demo::rotate_yaw(w.clock)
        } else {
            ent_angles[1]
        };
        // [pitch, yaw, roll]: EF_ROTATE overrides yaw only; pitch/roll come straight
        // from the entity so flying projectiles point along their flight path
        // (r_alias.c R_AliasSetUpTransform), not just spin about Z.
        let angles = [ent_angles[0], yaw, ent_angles[2]];
        // Per-entity skin index (R_AliasSetupSkin: `skinnum = currententity->skinnum`).
        // Drives e.g. armor.mdl's 3 skins (green/yellow/red); was hardcoded to 0.
        let skin = w.server.vm.ent_get_float(ent, "skin").max(0.0) as i32;
        // R_RocketTrail: a model with a rocket/grenade/gib/tracer header flag
        // trails particles from its previous origin to here (CL_RelinkEntities).
        if let Some(ttype) = rocket_trail_type(mflags) {
            let oldorg = *w.trail_org.get(&ent).unwrap_or(&origin);
            trail_spawns.push((ent, oldorg, origin, ttype));
            w.trail_org.insert(ent, origin);
        }
        descs.push((m, origin, angles, frame, color, skin));
    }

    // Emit the collected trails (after the entity loop to keep the borrows
    // disjoint). spawn_rocket_trail steps from old->new origin; EF_ROCKET also
    // flashes a small dynamic light at the rocket head.
    for (ent, oldorg, neworg, ttype) in trail_spawns.drain(..) {
        w.particles
            .spawn_rocket_trail(oldorg, neworg, ttype, &mut w.tracercount, now, &mut w.prng);
        if ttype == 0 {
            w.dlights.alloc(ent, neworg, 200.0, now + 0.01, 0.0, 0.0, now);
        }
    }

    // 5. Render from the player's eye, with Quake's head-bob added to the eye
    //    height (V_CalcBob) so the view rocks as the player moves. During an
    //    intermission the refdef is V_CalcIntermissionRefdef (view.c) instead:
    //    the QuakeC moved the player entity to the info_intermission spot, so
    //    the camera is the RAW entity origin + angles — no view_ofs, no bob, no
    //    punch, no strafe/death roll — plus the forced v_idlescale=1 sway of
    //    V_AddIdle (the gentle drift id's intermission camera has).
    let intermission = w.intermission != 0;
    let (mut eye, ang) = if intermission {
        // ent->origin / ent->angles: the QC set `angles = pos.mangle` (fixangle)
        // and froze the player MOVETYPE_NONE, which SV_ClientThink early-outs on,
        // so the spot's angles survive the per-frame mouse v_angle updates.
        (
            w.server.vm.ent_get_vector(w.player, "origin"),
            w.server.vm.ent_get_vector(w.player, "angles"),
        )
    } else {
        w.server.player_view()
    };
    let vel = w.server.vm.ent_get_vector(w.player, "velocity");
    let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
    let bob = render::view_bob(speed_xy, w.clock);

    // Record the listener pose so the page can spatialize this frame's queued
    // sounds. Forward/right are the level (no-pitch) yaw basis, matching the
    // renderer's `Camera::basis`: yaw rotates in XY about +Z, right is forward
    // turned -90 deg. Panning only needs the horizontal plane.
    let yaw_rad = (ang[1] as f64).to_radians();
    let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
    LISTENER.with(|l| {
        *l.borrow_mut() = Listener {
            pos: eye,
            forward: [cy, sy, 0.0],
            right: [sy, -cy, 0.0],
        };
    });

    // S_UpdateAmbientSounds: ramp the four automatic ambient channels toward
    // the VIEW leaf's ambient_level[] targets (water wash / sky wind). Uses the
    // same steady (un-bobbed) eye as the listener pose above.
    update_ambient_channels(&w.bsp, eye, dt);

    // Bob the rendered eye only (the listener pose above stays steady so audio
    // panning does not jitter with the head-bob). Skipped during intermission
    // (V_CalcIntermissionRefdef has no bob and no stair smoothing).
    if !intermission {
        eye[2] += bob;
        // Stair-step view smoothing (view.c V_CalcRefdef ~960): while on the ground
        // and the player's origin Z rose this frame, lag the eye Z behind by up to 12
        // units and catch up at 80 u/s, so climbing stairs glides instead of jolting
        // up each 16/18-unit step. The delta is relative to the raw origin Z (bob
        // layered on top); on first frame / not-climbing, oldz tracks origin exactly.
        let origin_z = w.server.vm.ent_get_vector(w.player, "origin")[2];
        let onground = (w.server.vm.ent_get_float(w.player, "flags") as i32) & FL_ONGROUND != 0;
        if w.oldz.is_finite() && onground && origin_z - w.oldz > 0.0 {
            w.oldz += dt.max(0.0) * 80.0;
            if w.oldz > origin_z {
                w.oldz = origin_z;
            }
            if origin_z - w.oldz > 12.0 {
                w.oldz = origin_z - 12.0;
            }
            eye[2] += w.oldz - origin_z;
        } else {
            w.oldz = origin_z;
        }
    }
    let cam = if intermission {
        // V_AddIdle with v_idlescale forced to 1 (view.c V_CalcIntermissionRefdef):
        // angle += sin(cl.time * v_i*_cycle) * v_i*_level, with the stock cvar
        // defaults — roll 0.5/0.1, pitch 1/0.3, yaw 2/0.3.
        Camera {
            pos: eye,
            yaw: ang[1] + (w.clock * 2.0).sin() * 0.3,
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(ang[0] + (w.clock * 1.0).sin() * 0.3),
            roll: ang[2] + (w.clock * 0.5).sin() * 0.1,
            fov_deg: 90.0,
        }
    } else {
        // Add the weapon-fire view kick (cl.punchangle, view.c:957); the engine's
        // drop_punch_angle already decays it back to zero each frame.
        let punch = client_punchangle(w);
        // View bank (V_CalcViewRoll, view.c:808): strafe lean from side-velocity
        // plus the svc_damage kick (decaying over v_kicktime by host_frametime),
        // plus the punchangle's roll component; the dead-view tilt (80°)
        // overrides when the player is dead.
        let body_angles = w.server.vm.ent_get_vector(w.player, "angles");
        let mut roll = quake_rs::server::v_calc_roll(body_angles, vel) + punch[2];
        let mut kick_pitch = 0.0;
        if w.v_dmg_time > 0.0 {
            roll += w.v_dmg_time / V_KICKTIME * w.v_dmg_roll;
            kick_pitch = w.v_dmg_time / V_KICKTIME * w.v_dmg_pitch;
            w.v_dmg_time -= if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        }
        if w.server.vm.ent_get_float(w.player, "health") <= 0.0 {
            roll = 80.0; // dead view angle (replaces, per V_CalcViewRoll)
        }
        Camera {
            pos: eye,
            yaw: ang[1] + punch[1],
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(ang[0] + kick_pitch + punch[0]),
            roll,
            fov_deg: 90.0,
        }
    };
    let mut instances: Vec<ModelInstance> = descs
        .iter()
        .filter_map(|(name, origin, angles, frame, color, skin)| match w.model_cache.get(name) {
            Some(Some(mdl)) => Some(ModelInstance {
                mdl,
                origin: *origin,
                yaw: angles[1],
                pitch: angles[0],
                roll: angles[2],
                color: *color,
                frame: *frame,
                skinnum: *skin,
            }),
            _ => None,
        })
        .collect();
    // CL_UpdateTEnts (cl_tent.c): expand every live lightning beam into one
    // bolt-model piece every 30 units (shared integer pitch/yaw, random roll
    // per piece per frame). A beam owned by the view entity (the player's
    // thunderbolt) is re-anchored to the player's CURRENT origin first. Gated
    // on any_live so the common no-beam frame pays one boolean scan.
    if w.beams.any_live(now) {
        let player_org = w.server.vm.ent_get_vector(w.player, "origin");
        w.beams.update(now, w.player, player_org, &mut w.prng, &mut w.beam_scratch);
        for seg in &w.beam_scratch {
            // CL_NewTempEntity memsets the entity: frame 0, skin 0. A `None`
            // cache entry (model absent from the pak) skips the piece.
            if let Some(Some(mdl)) = w.model_cache.get(seg.model.model_name()) {
                instances.push(ModelInstance {
                    mdl,
                    origin: seg.origin,
                    yaw: seg.yaw,
                    pitch: seg.pitch,
                    roll: seg.roll,
                    color: color_for_name(seg.model.model_name()),
                    frame: 0,
                    skinnum: 0,
                });
            }
        }
    }
    // External brush-model item boxes: resolve each (name, origin) against the
    // bmodel cache, dropping any box whose bsp was missing/unparseable (`None`).
    let external: Vec<render::ExternalBModel> = ext_descs
        .iter()
        .filter_map(|(name, origin)| match w.bmodel_cache.get(name) {
            Some(Some(bsp)) => Some(render::ExternalBModel { bsp, origin: *origin }),
            _ => None,
        })
        .collect();
    // Sprite-model entities: resolve each (name, origin, frame) against the sprite
    // cache, dropping any whose .spr was missing/unparseable.
    let sprites: Vec<render::SpriteInstance> = sprite_descs
        .iter()
        .filter_map(|(name, origin, frame)| match w.sprite_cache.get(name) {
            Some(Some(spr)) => Some(render::SpriteInstance { sprite: spr, origin: *origin, frame: *frame }),
            _ => None,
        })
        .collect();
    // Anchor the weapon viewmodel to the camera (drawn last, on top of the world).
    // R_DrawViewModel (r_main.c ~622) returns early — drawing NO gun — when the
    // player is dead (STAT_HEALTH <= 0) or carrying the Ring of Shadows
    // (IT_INVISIBILITY). Without this the gun hovers, frozen, on the rolled
    // death-cam, and stays visible while invisible. The intermission camera also
    // hides it (V_CalcIntermissionRefdef: `view->model = NULL`).
    let hide_gun = intermission
        || w.server.vm.ent_get_float(w.player, "health") <= 0.0
        || (w.server.vm.ent_get_float(w.player, "items") as i32) & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match w.model_cache.get(&weapon_name) {
            // V_CalcRefdef's gun origin (the forward bob + the viewsize fudge)
            // and CalcGunAngle's angles (the view before the punch, no lean).
            Some(Some(mdl)) => {
                let punch = client_punchangle(w);
                let angles = render::viewmodel_angles(&cam, punch, ang[2]);
                Some(Viewmodel {
                    mdl,
                    frame: weapon_frame,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, w.viewsize),
                    angles,
                })
            }
            _ => None,
        }
    };
    // R_DrawParticles: free the particles whose `die < cl.time`, draw the rest
    // as (world pos, palette index) — they share the scene z-buffer, so any
    // behind a wall are hidden — and only then move each one and step its
    // ramp by `cl.time - cl.oldtime` (0 while paused). A particle is drawn
    // where and as it was spawned on its first frame, and on its last.
    w.particles.retire(now);
    let parts: Vec<([f32; 3], u8)> =
        w.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    if dt.is_finite() && dt > 0.0 && !paused {
        // grav = frametime * sv_gravity.value * 0.05 (100 on e1m8).
        w.particles.integrate(dt, now, w.server.sv_gravity() * 0.05);
    }
    // The live dynamic lights (explosions / muzzle flashes) light up nearby
    // walls: R_PushDlights skips `die < cl.time || !radius`. A light is drawn
    // at the radius it was allocated with; CL_DecayLights shrinks it after the
    // frame (below).
    let active_dlights = pushed_dlights(&w.dlights, now);
    // The animated light-style scales (torch flicker, pulsing lights) at the
    // current server clock; the worldspawn populated the styles at spawn time.
    let light_styles = w.server.lightstyle_scales(w.clock);
    // SCR_CalcRefdef / R_SetVrect: the viewsize picks the 3-D view rectangle
    // (the view sits ABOVE the status bar, projected about its own centre) and
    // how much status bar shows; an intermission is always full screen.
    bench::lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, w.viewsize, intermission);
    let vrect = refdef.vrect;
    // R_SetupFrame's r_dowarp (r_waterwarp 1): with the eye's leaf in water,
    // slime or lava the view is rendered into the (at most 320x200) warp
    // buffer, and D_WarpScreen stretches it over `vrect` below.
    let eye_contents = quake_rs::world::point_contents(&w.bsp, eye);
    let dowarp = eye_contents <= quake_rs::bsp::CONTENTS_WATER;
    let rvrect = if dowarp {
        quake_rs::screen::warp_vrect(render_w, render_h, w.viewsize, intermission)
    } else {
        vrect
    };
    let view =
        render::render_scene_ext_sprited(&w.bsp, &cam, rvrect.w, rvrect.h, &w.palette, &instances, &bmodels, &external, viewmodel, w.clock, &parts, &active_dlights, &light_styles, w.colormap.as_deref(), &sprites);
    bench::lap(Phase::Render3d);
    // Host_Frame runs CL_DecayLights after SCR_UpdateScreen: `radius -=
    // (cl.time - cl.oldtime)*decay` — 0 while paused, nothing fades or dies.
    if dt.is_finite() && dt > 0.0 && !paused {
        w.dlights.advance(dt, now);
    }

    // 5b. Colour shifts (V_UpdatePalette, the software build's palette shift):
    //     drop the damage and bonus flashes (after this frame's svc_damage was
    //     parsed) and tint the view when the eye is under water / in lava or
    //     slime. The shifts are DEFERRED (returned to the dispatcher) and the
    //     finished screen goes through `render::cshift_ramps` last, so they
    //     tint the HUD, menu and console too, as the palette shift does.
    let frametime = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    w.damage_blend = cshift_drop(w.damage_blend, frametime, DAMAGE_FADE);
    w.bonus_blend = cshift_drop(w.bonus_blend, frametime, BONUS_FADE);
    // Underwater sine wobble (D_WarpScreen): the warp buffer's view, stretched
    // over the screen's view rectangle while it wobbles, BEFORE the content
    // tint so the screen ripples, not just darkens.
    let view = if dowarp { render::apply_warp(view, vrect.w, vrect.h, w.clock) } else { view };
    // The 2-D oracle harness paints the view one flat colour (the C oracle's
    // `oracle_blank`), so a shot measures the 2-D layer alone. Tests only.
    #[cfg(test)]
    let view = crate::oracle_screen::blank_view(view, &w.palette);
    // The screen: the view at its rectangle, backtile around it
    // (SCR_UpdateScreen's Draw_TileClear), the status bar drawn over below.
    let backtile = backtile_for(&vrect, render_w, render_h, w.gfx_wad.as_ref());
    let mut img =
        render::compose_view(view, vrect, render_w, render_h, backtile.as_ref(), &w.palette);
    // cl.cshifts order: CONTENTS (bottom) -> DAMAGE -> BONUS -> POWERUP (top).
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if let Some(cs) = render::content_cshift(eye_contents) {
        shifts.push(cs);
    }
    if w.damage_blend > 0.0 {
        shifts.push((w.damage_color, w.damage_blend));
    }
    if w.bonus_blend > 0.0 {
        shifts.push((BONUS_COLOR, w.bonus_blend));
    }
    // Powerup tint (Quad=blue, Biosuit=green, Ring=gray, Pentagram=yellow).
    if let Some(cs) = render::powerup_cshift(w.server.vm.ent_get_float(w.player, "items") as i32) {
        shifts.push(cs);
    }
    // V_UpdatePalette (software view.c): the cshift is a whole-PALETTE shift run
    // LAST in SCR_UpdateScreen, so it tints the ENTIRE screen — 3D view, status bar,
    // centerprint, menu, console — not just the 3D viewport (that 3D-only scope is
    // the GLQuake R_PolyBlend look). We DEFER the shifts: draw the HUD/messages on
    // the untinted frame and return them so the dispatcher applies them to the
    // fully composited frame (after the menu/console overlay too).
    bench::lap(Phase::Post3d);

    // 6. Status bar (HUD) overlay: blit the bottom bar with the player's live
    //    health/ammo/armour on top of the finished 3-D frame. Skipped silently
    //    when gfx.wad was absent (the world still renders).
    //
    //    During an intermission SCR_UpdateScreen (screen.c) draws the matching
    //    overlay INSTEAD of the status bar — Sbar_IntermissionOverlay for
    //    cl.intermission == 1, Sbar_FinaleOverlay + the revealed center string
    //    for == 2, the center string alone for == 3 — and only while the game
    //    owns the screen (`key_dest == key_game`; with the menu/console up
    //    neither the bar nor the overlay paints, the view is full-screen).
    if w.intermission != 0 {
        if !menu_up {
            match w.intermission {
                1 => {
                    if let Some(wad) = w.gfx_wad.as_ref() {
                        // Counts from the QuakeC globals the engine's
                        // SV_UpdateStats reads (same source as the Tab scoreboard).
                        let gcount = |g: &str| w.server.vm.gget_float(g) as i32;
                        let stats = render::IntermissionStats {
                            // cl.completed_time is an int in the C: whole seconds.
                            completed_time: w.completed_time as i32,
                            secrets: gcount("found_secrets"),
                            total_secrets: gcount("total_secrets"),
                            monsters: gcount("killed_monsters"),
                            total_monsters: gcount("total_monsters"),
                        };
                        render::draw_intermission_overlay(
                            &mut img,
                            wad,
                            &w.palette,
                            w.pic_complete.as_ref(),
                            w.pic_inter.as_ref(),
                            &stats,
                        );
                    }
                }
                2 => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    &w.palette,
                    w.pic_finale.as_ref(),
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
                // svc_cutscene: the centered text alone, no plaque.
                _ => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    &w.palette,
                    None,
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
            }
        }
    } else if let Some(wad) = w.gfx_wad.as_ref() {
        let stat = |f: &str| w.server.vm.ent_get_float(w.player, f) as i32;
        // Solo-scoreboard counts come from the QuakeC globals the engine's
        // SV_UpdateStats reads; the level name is worldspawn's `message` (edict 0).
        let gcount = |g: &str| w.server.vm.gget_float(g) as i32;
        let level_name = w.server.vm.ent_get_string(0, "message");
        let hud = render::Hud {
            wad,
            palette: &w.palette,
            health: stat("health"),
            // The active weapon's ammo (W_SetCurrentAmmo keeps `currentammo` in
            // sync with the weapon), not always shells — sbar.c draws currentammo.
            ammo: stat("currentammo"),
            armor: stat("armorvalue"),
            items: client_items(w),
            weapon: stat("weapon"),
            ammo_shells: stat("ammo_shells"),
            ammo_nails: stat("ammo_nails"),
            ammo_rockets: stat("ammo_rockets"),
            ammo_cells: stat("ammo_cells"),
            // Sbar_SoloScoreboard shows cl.time — the SERVER clock (epoch 1.0,
            // SV_SpawnServer), not this walk's 0-based clock, matching what the
            // intermission overlay's completed_time latches.
            time: w.server.time(),
            item_gettime: Some(&w.item_gettime),
            monsters: gcount("killed_monsters"),
            total_monsters: gcount("total_monsters"),
            secrets: gcount("found_secrets"),
            total_secrets: gcount("total_secrets"),
            level_name: &level_name,
            // `+showscores` held (Tab); the dead-player branch (health <= 0)
            // inside draw_hud_into handles the death scoreboard.
            show_scores: km.showscores,
            face_pain: w.server.time() <= w.faceanimtime,
            sb_lines: refdef.sb_lines,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // On-screen messages the QuakeC printed (drained above): the current
    // centerprint drawn centered, the notify lines stacked top-left. Both time
    // out via their stored expiry; drawn over the HUD. Suppressed while the menu or
    // console owns the screen (Quake draws the notify/centerprint only for
    // key_dest == key_game), so they don't paint through the menu/console overlay.
    // Also suppressed during intermission: SCR_UpdateScreen's intermission
    // branches draw neither SCR_CheckDrawCenterString (the finale text above is
    // its own path) nor the console notify lines.
    if !menu_up && w.intermission == 0 {
        if let Some(cc) = w.conchars.as_ref() {
            if let Some((text, _)) = &w.centerprint {
                render::draw_centerprint(&mut img, cc, &w.palette, text);
            }
            let lines = w.notify.visible(w.host_time);
            if !lines.is_empty() {
                render::draw_notify(&mut img, cc, &w.palette, &lines);
            }
        }
    }

    // The main-menu overlay is drawn by the `step` dispatcher (the menu lives at
    // the App level now so it can overlay walk OR the attract demo); step_walk no
    // longer draws it. The deferred cshifts ride out with the frame so the
    // dispatcher tints the whole composited image (HUD + menu + console included).
    bench::lap(Phase::Hud2d);
    (img, shifts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, boot_attract, build_walk, APP};
    use crate::console::{console_toggle, console_visible};
    use crate::host::step;
    use crate::menu::menu_select;
    use crate::test_util::*;
    use crate::vid::set_resolution;

    /// Regression for the one-time texture/lighting "pops" in the first second of
    /// live play (two distinct root causes, both whole-view shimmers):
    ///
    /// 1. **Spawn settle on screen.** QuakeC `PutClientInServer` places the player
    ///    at `spot.origin + '0 0 1'` (the start map's spawn floats ~5 units up),
    ///    and the port used to render frame 0 with ZERO physics frames after the
    ///    spawn — the player fell to the floor ON SCREEN over the first 2-3 frames
    ///    and every textured surface resampled (17.8-45.9% of pixels/frame).
    ///    WinQuake runs two SV_Physics ticks during the signon (the Host_Spawn_f
    ///    and Host_Begin_f frames) before SCR_EndLoadingPlaque re-enables drawing;
    ///    `Server::run_signon_frames` ports those, and every walk-building path
    ///    (boot / New Game / changelevel / restart) must call it.
    /// 2. **Plane-only dlight gating.** ~0.55s in (sv.time ~1.98 with the current
    ///    deterministic PRNG), the start map's distant `misc_fireball` lavaball
    ///    spawns with a rocket-trail dynamic light ~2000 units away behind walls;
    ///    `any_dlight_reaches`' old plane-distance-only test marked every
    ///    near-coplanar face in the VIEW as dynamically lit, kicking them off the
    ///    baked surface cache onto the per-pixel path (13.3% of pixels shifted in
    ///    one frame, then back when the light died). The gate now also tests the
    ///    face's texture-space extent (WinQuake's R_MarkLights is spatially
    ///    bounded by the BSP recursion).
    ///
    /// The 45-frame window covers both: settle would hit frames 0-2, the fireball
    /// ~frame 34.
    #[test]
    fn new_game_first_frames_render_a_settled_player_no_pop() {
        // The browser path: attract demo + menu, then Single Player > New Game.
        assert_eq!(boot_attract(), 1);
        step(1.0 / 60.0); // an attract-demo frame, like the live page
        menu_select(); // Main: Single Player
        menu_select(); // SP: New Game -> builds the start-map walk, closes menu

        // BEFORE the first frame renders the player must already be settled:
        // on the ground, no residual fall velocity (it spawns ~5 units up).
        let eye0 = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let w = a.walk.as_ref().unwrap();
            let flags = w.server.vm.ent_get_float(w.player, "flags") as i32;
            let vel = w.server.vm.ent_get_vector(w.player, "velocity");
            assert!(flags & FL_ONGROUND != 0, "player on the ground at frame 0");
            assert_eq!(vel[2], 0.0, "no residual fall velocity at frame 0");
            w.server.player_view().0
        });

        // Frame 0, then 44 more static zero-input frames: the eye must stay
        // bit-identical (the settle pop was exactly this eye motion leaking into
        // the first rendered frames)...
        step(1.0 / 60.0);
        let mut prev: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.render_w, a.render_h)
        });
        for i in 1..45 {
            step(1.0 / 60.0);
            let eye = APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                a.walk.as_ref().unwrap().server.player_view().0
            });
            assert_eq!(eye, eye0, "static eye is bit-identical on frame {i}");
            // ...and consecutive frames stay near-identical. Faithful animation
            // in the static spawn view (scrolling sky, flame group-frames, the
            // 10 Hz lightstyle flicker) touches <= ~0.4% of pixels per frame;
            // the settle pop touched 17.8%-45.9% and the fireball-dlight pop
            // 13.3%. A 2% ceiling separates bug from animation with a wide
            // margin in both directions.
            let fb: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
            let nd = prev
                .chunks_exact(4)
                .zip(fb.chunks_exact(4))
                .filter(|(a4, b4)| a4[..3] != b4[..3])
                .count();
            assert!(
                nd <= w * h / 50,
                "frame {i} vs {}: {nd} px differ ({:.2}%) — a one-time view shift leaked into the first frames",
                i - 1,
                100.0 * nd as f64 / (w * h) as f64
            );
            prev = fb;
        }
    }

    #[test]
    fn thunderbolt_beam_renders_bolt_pixels_on_e1m1() {
        // End-to-end through the LIVE path: boot the e1m1 walk, cheat in the
        // thunderbolt (impulse 9 = all weapons + ammo, impulse 8 = lightning
        // gun), hold fire, and verify (a) the QuakeC's TE_LIGHTNING2 broadcast
        // landed in the beam store, (b) bolt2.mdl was loaded on demand at parse
        // time (Mod_ForName in CL_ParseTEnt), (c) the per-frame CL_UpdateTEnts
        // expansion produced pieces anchored at the muzzle (origin + '0 0 16',
        // W_FireLightning), and (d) the bolt actually changes rendered pixels —
        // an identical re-render (same rng, dt=0) with the beams cleared differs.
        let mut w = build_walk().expect("e1m1 walk boots from the embedded pak");
        // Let the spawn settle (telefrag effects, initial thinks).
        for _ in 0..10 {
            let _ = step_walk(&mut w, 0.05, false, 320, 200);
        }
        w.next_impulse = 9; // CheatCommand: all weapons + full cells
        let _ = step_walk(&mut w, 0.05, false, 320, 200);
        w.next_impulse = 8; // select the thunderbolt
        let _ = step_walk(&mut w, 0.05, false, 320, 200);
        // Hold fire across several frames (W_FireLightning re-broadcasts the
        // beam each weapon frame, exercising the same-entity slot REPLACEMENT).
        w.in_attack = true;
        for _ in 0..6 {
            let _ = step_walk(&mut w, 0.05, false, 320, 200);
        }
        assert!(
            w.beams.any_live(w.clock),
            "firing the thunderbolt put a live beam in the store"
        );
        assert!(
            matches!(w.model_cache.get("progs/bolt2.mdl"), Some(Some(_))),
            "TE_LIGHTNING2 loaded progs/bolt2.mdl on demand"
        );
        // The last frame's expansion is retained in the scratch buffer: the
        // thunderbolt is ONE beam (slot replacement, never stacked). The QuakeC
        // fires from origin + '0 0 16' with a 600-unit traceline, but
        // CL_UpdateTEnts re-anchors the VIEW entity's beam to the player's raw
        // ORIGIN every frame (the C quirk — the visible bolt hangs 16 units
        // below the muzzle), so the segment can run a hair over 600 units:
        // 1..=21 Bolt2 pieces.
        assert!(
            !w.beam_scratch.is_empty() && w.beam_scratch.len() <= 21,
            "one ~600-unit beam expands to 1..=21 pieces (got {})",
            w.beam_scratch.len()
        );
        assert!(
            w.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt2),
            "thunderbolt pieces use bolt2.mdl"
        );
        // The first piece sits at the player ORIGIN: the WriteEntity short
        // carried the player edict number through the decoder (an int global —
        // a float read would have yielded ~0 and never matched w.player), and
        // the view-entity re-anchor replaced the broadcast start (origin+16).
        let player_origin = w.server.vm.ent_get_vector(w.player, "origin");
        let first = w.beam_scratch[0].origin;
        for i in 0..3 {
            assert!(
                (first[i] - player_origin[i]).abs() < 1.0,
                "first piece re-anchored to the player origin (axis {i}: {} vs {})",
                first[i],
                player_origin[i]
            );
        }

        // Pixel evidence: render the SAME state twice (dt = 0 -> no time passes,
        // restored rng -> identical dlight jitter draws), once with the live
        // beam and once with the store cleared. The ONLY difference is the bolt
        // model pieces, so differing pixels prove the bolt drew into the scene.
        let rng = w.prng;
        let (with_bolt, _) = step_walk(&mut w, 0.0, false, 320, 200);
        w.prng = rng;
        w.beams.clear();
        let (without_bolt, _) = step_walk(&mut w, 0.0, false, 320, 200);
        let diff = with_bolt
            .rgb
            .iter()
            .zip(without_bolt.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff > 0,
            "the rendered thunderbolt changes pixels vs the beam-less frame"
        );
        // Optional visual evidence: QUAKE_DUMP_BEAM=/some/dir dumps the two
        // frames as PPMs for eyeballing (never set in CI; the asserts above are
        // the real check).
        if let Ok(dir) = std::env::var("QUAKE_DUMP_BEAM") {
            for (img, name) in [(&with_bolt, "with-bolt"), (&without_bolt, "without-bolt")] {
                let mut buf = format!("P6\n{} {}\n255\n", img.w, img.h).into_bytes();
                for px in &img.rgb {
                    buf.extend_from_slice(px);
                }
                let _ = std::fs::write(format!("{dir}/beam-{name}.ppm"), buf);
            }
        }
    }

    // -------------------------------------------------------------------
    // Intermission + finale (end-to-end against the real progs.dat)
    // -------------------------------------------------------------------

    /// The centre of the live map's `trigger_changelevel` brush volume (from the
    /// absmin/absmax its setmodel+link produced), to pin the player onto.
    fn changelevel_trigger_center() -> [f32; 3] {
        walk_mut(|w| {
            for e in 0..w.server.vm.num_edicts() {
                let ent = e as i32;
                if w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
                    continue;
                }
                if w.server.vm.ent_get_string(ent, "classname") == "trigger_changelevel" {
                    let amin = w.server.vm.ent_get_vector(ent, "absmin");
                    let amax = w.server.vm.ent_get_vector(ent, "absmax");
                    return [
                        0.5 * (amin[0] + amax[0]),
                        0.5 * (amin[1] + amax[1]),
                        0.5 * (amin[2] + amax[2]),
                    ];
                }
            }
            panic!("no trigger_changelevel in the live map");
        })
    }

    /// Pin the player onto the exit trigger and step until the QuakeC's
    /// `execute_changelevel` think fires `svc_intermission` (touch at frame N,
    /// the scheduled think 0.1s later). Panics if it never arrives.
    fn drive_into_exit() {
        let centre = changelevel_trigger_center();
        for _ in 0..40 {
            walk_mut(|w| {
                let p = w.player;
                w.server.vm.ent_set_vector(p, "origin", centre);
                w.server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
            });
            step(0.1);
            if walk_mut(|w| w.intermission) != 0 {
                return;
            }
        }
        panic!("svc_intermission never arrived after 40 frames on the exit trigger");
    }

    /// Write the current RGBA framebuffer as a binary PPM into `$QUAKE_DUMP_DIR`
    /// (the feature-evidence dumps); a no-op when the variable is unset.
    fn dump_frame(name: &str) {
        let Ok(dir) = std::env::var("QUAKE_DUMP_DIR") else { return };
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let (w, h) = (a.render_w, a.render_h);
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in a.fb.chunks(4).take(w * h) {
                out.extend_from_slice(&px[..3]);
            }
            let _ = std::fs::write(format!("{dir}/{name}.ppm"), out);
        });
    }

    /// Run QC `T_Damage(targ, inflictor, attacker, damage)` on the live server.
    fn qc_damage(w: &mut Walk, targ: i32, inflictor: i32, damage: f32) {
        use quake_rs::progs::OFS_PARM0;
        let f = w.server.vm.progs.find_function("T_Damage").expect("progs has T_Damage");
        let vm = &mut w.server.vm;
        vm.set_gi(OFS_PARM0, targ);
        vm.set_gi(OFS_PARM0 + 3, inflictor);
        vm.set_gi(OFS_PARM0 + 6, inflictor);
        vm.set_gf(OFS_PARM0 + 9, damage);
        vm.execute(f).expect("T_Damage runs");
    }

    /// CENSUS F16: QC T_Damage adds to `dmg_take`/`dmg_save` before its god-mode
    /// return, and SV_WriteClientdataToMessage sends svc_damage whenever they
    /// are non-zero — so a god-mode (or Pentagram) hit still flashes red, kicks
    /// the view and shows the pain face; the fields are zeroed once sent.
    #[test]
    fn damage_in_god_mode_still_flashes_and_kicks() {
        let mut w = build_walk().expect("e1m1 boots");
        for _ in 0..3 {
            step_walk(&mut w, 0.05, false, 320, 200);
        }
        let p = w.player;
        let flags = w.server.vm.ent_get_float(p, "flags") as i32;
        w.server.vm.ent_set_float(p, "flags", (flags | 64) as f32); // FL_GODMODE
        // The inflictor: a spot 100 units straight ahead (yaw 0 -> +x).
        w.yaw = 0.0;
        w.pitch = 0.0;
        let src = w.server.vm.spawn();
        let o = w.server.vm.ent_get_vector(p, "origin");
        w.server.vm.ent_set_vector(src, "origin", [o[0] + 100.0, o[1], o[2]]);
        qc_damage(&mut w, p, src, 20.0);
        assert_eq!(w.server.vm.ent_get_float(p, "health"), 100.0, "god mode: no health lost");
        assert_eq!(w.server.vm.ent_get_float(p, "dmg_take"), 20.0, "T_Damage counted the hit");
        let (_, cshifts) = step_walk(&mut w, 0.05, false, 320, 200);
        assert_eq!(w.server.vm.ent_get_float(p, "dmg_take"), 0.0, "sent and zeroed");
        assert!(
            cshifts.iter().any(|&(c, pct)| c == [255, 0, 0] && pct > 0.0),
            "a red damage cshift: {cshifts:?}"
        );
        // count = max(20*0.5, 10) = 10: percent 30, then one 0.05 s drop of 7.5,
        // truncated like the C's int percent.
        assert_eq!(w.damage_blend, 22.0);
        assert!(w.v_dmg_pitch > 5.0, "hit from the front pitches the view: {}", w.v_dmg_pitch);
        assert!(w.v_dmg_time > 0.0 && w.v_dmg_time < crate::view::V_KICKTIME, "kick running");
        assert!(w.server.time() <= w.faceanimtime, "the pain face shows");
    }

    /// CENSUS L9: an explosion's dlight is drawn at its full 350 radius on the
    /// frame CL_ParseTEnt allocated it (CL_DecayLights runs after
    /// SCR_UpdateScreen), a light past its `die` is not drawn, and the frame
    /// still decays the pool once.
    #[test]
    fn dlights_are_drawn_before_they_decay() {
        let mut w = build_walk().expect("e1m1 boots");
        step_walk(&mut w, 0.05, false, 320, 200);
        let now = w.clock;
        w.dlights.alloc(0, [0.0; 3], 350.0, now + 0.5, 300.0, 0.0, now);
        w.dlights.alloc(0, [64.0, 0.0, 0.0], 200.0, now - 0.01, 0.0, 0.0, now);
        let drawn = pushed_dlights(&w.dlights, now);
        assert_eq!(drawn.len(), 1, "the dead light is not pushed");
        assert_eq!(drawn[0].radius, 350.0, "full radius on its first frame");
        let before = w.dlights.active().iter().map(|d| d.radius).fold(0.0, f32::max);
        step_walk(&mut w, 0.05, false, 320, 200);
        let after = w.dlights.active().iter().map(|d| d.radius).fold(0.0, f32::max);
        assert!((before - after - 0.05 * 300.0).abs() < 1e-3, "{before} -> {after}");
    }

    /// CENSUS L1: the client's punchangle is MSG_WriteChar'd — truncated to
    /// whole degrees — so the shotgun kick steps -2, -1, 0.
    #[test]
    fn punchangle_reaches_the_view_in_whole_degrees() {
        let mut w = build_walk().expect("e1m1 boots");
        let p = w.player;
        w.server.vm.ent_set_vector(p, "punchangle", [-1.9, 0.5, -2.0]);
        assert_eq!(client_punchangle(&w), [-1.0, 0.0, -2.0]);
        w.server.vm.ent_set_vector(p, "punchangle", [-4.0, 0.0, 0.0]);
        assert_eq!(client_punchangle(&w), [-4.0, 0.0, 0.0]);
    }

    /// CENSUS F18: CL_ParseClientdata stamps `cl.item_gettime` for every newly
    /// set items bit (and CL_ClearState's zeroed cl.items makes the level start
    /// stamp what the player carries); the HUD then cycles the new weapon's
    /// `inva1..5` icons for a second.
    #[test]
    fn new_items_are_stamped_for_the_icon_flash() {
        let mut w = build_walk().expect("e1m1 boots");
        step_walk(&mut w, 0.05, false, 320, 200);
        let t0 = w.server.time();
        assert_eq!(w.item_gettime[0], t0, "the shotgun (bit 0) flashes at level start");
        assert_eq!(w.cl_items, client_items(&w));
        for _ in 0..30 {
            step_walk(&mut w, 0.05, false, 320, 200);
        }
        w.next_impulse = 9; // every weapon
        step_walk(&mut w, 0.05, false, 320, 200);
        let t1 = w.server.time();
        assert_eq!(w.item_gettime[4], t1, "the rocket launcher (bit 4) was just got");
        assert_eq!(w.item_gettime[0], t0, "the shotgun keeps its old stamp");
    }

    /// CENSUS F1: svc_setangle carries MSG_WriteAngle's byte — whole degrees,
    /// 256 steps, read back signed — and a new level starts facing the angles
    /// PutClientInServer gave the player (Host_Spawn_f's setangle).
    #[test]
    fn setangle_quantises_like_the_wire_and_spawns_face_the_spot() {
        assert_eq!(net_angle(90.0), 90.0);
        assert_eq!(net_angle(45.0), 45.0);
        assert_eq!(net_angle(270.0), -90.0, "byte 192 reads back as char -64");
        assert_eq!(net_angle(10.9), 7.0 * 360.0 / 256.0, "(int)10.9*256/360 = 7");
        assert_eq!(net_angle(-90.0), -90.0);
        let w = build_walk().expect("e1m1 boots");
        let spot = crate::app::player_start(&w.bsp.entities).expect("e1m1 has a start").1;
        assert_eq!(w.yaw, net_angle(spot), "the view faces info_player_start's angle");
        assert_eq!(w.pitch, 0.0);
        assert_eq!(w.server.vm.ent_get_float(w.player, "fixangle"), 0.0);
    }

    /// CENSUS F2: behind the menu/console single player is paused
    /// (Host_ServerFrame skips SV_Physics, SV_RunClients skips SV_ClientThink):
    /// sv.time and cl.time stand still, the particles neither move nor die,
    /// and a queued impulse waits for the server. What the C runs off
    /// host_frametime / realtime keeps going: the damage fade and the
    /// centerprint countdown.
    #[test]
    fn single_player_pause_freezes_the_world_but_not_the_host_clock() {
        let mut w = build_walk().expect("e1m1 boots");
        for _ in 0..3 {
            step_walk(&mut w, 0.1, false, 320, 200);
        }
        w.particles.spawn_burst([0.0; 3], [0.0; 3], 73, 20, w.clock, &mut w.prng);
        w.centerprint = Some(("paused".into(), w.host_time + 2.0));
        w.damage_blend = 100.0;
        w.next_impulse = 2;
        let (sv0, cl0, parts0) = (w.server.time(), w.clock, w.particles.particles().len());
        let org0 = w.particles.particles()[0].origin;
        for _ in 0..25 {
            step_walk(&mut w, 0.1, true, 320, 200); // menu up for 2.5 s
        }
        assert_eq!(w.server.time(), sv0, "sv.time stands still");
        assert_eq!(w.clock, cl0, "cl.time stands still");
        assert_eq!(w.particles.particles().len(), parts0, "no particle dies");
        assert_eq!(w.particles.particles()[0].origin, org0, "no particle moves");
        assert_eq!(w.next_impulse, 2, "the impulse waits for the server");
        assert!(w.centerprint.is_none(), "the centerprint timed out behind the menu");
        assert_eq!(w.damage_blend, 0.0, "the damage flash faded behind the menu");
        step_walk(&mut w, 0.1, false, 320, 200);
        assert!(w.server.time() > sv0, "the game resumes when the menu closes");
        assert_eq!(w.next_impulse, 0, "and the waiting impulse is sent");
    }

    #[test]
    fn level_exit_runs_the_intermission_then_changelevel() {
        // The faithful end-of-level flow, driven end-to-end through the REAL
        // progs.dat: touching trigger_changelevel runs execute_changelevel (the
        // QC freezes the player on the info_intermission spot and WriteBytes
        // svc_intermission to MSG_ALL), the engine enters intermission mode and
        // draws the stats overlay, and only a button press AFTER
        // intermission_exittime (time+2) runs GotoNextMap -> changelevel(e1m2).
        assert_eq!(boot(), 1);
        set_resolution(320, 200); // debug-build render speed; clamps to the min preset
        close_menu(); // boot opens the menu; buttons are gated while it is up
        let before_shells = player_field("ammo_shells") as i32;

        drive_into_exit();

        // --- The engine is in intermission: camera frozen on the QC-moved
        // player, stats overlay up, status bar hidden.
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "svc_intermission set cl.intermission = 1");
            // cl.completed_time = cl.time = sv.time (cl_parse.c:939), and
            // SV_SpawnServer starts sv.time at 1.0 — the walk clock (starting at
            // 0) would read at least 1 second LOW here. drive_into_exit returned
            // the moment the latch happened, so the latched value IS the server's
            // current clock.
            assert!(w.completed_time >= 1.0, "completed_time latches sv.time (epoch 1.0)");
            assert_eq!(
                w.completed_time,
                w.server.time(),
                "completed_time = sv.time at the latch (no steps ran since)"
            );
            // execute_changelevel froze the player: MOVETYPE_NONE, modelindex 0,
            // view_ofs zeroed, moved to the info_intermission spot.
            assert_eq!(
                w.server.vm.ent_get_float(w.player, "movetype") as i32,
                0,
                "player frozen MOVETYPE_NONE"
            );
            assert_eq!(
                w.server.vm.ent_get_vector(w.player, "view_ofs"),
                [0.0, 0.0, 0.0],
                "view_ofs zeroed for the intermission camera"
            );
            // The stats the overlay shows come from the QC globals and are sane.
            assert!(
                w.server.vm.gget_float("total_monsters") > 0.0,
                "e1m1 reports a monster total"
            );
        });
        // The QC moved the player to the info_intermission spot (e1m1 has one);
        // its angles came from the spot's mangle via fixangle.
        let pinned = changelevel_trigger_center();
        walk_mut(|w| {
            let org = w.server.vm.ent_get_vector(w.player, "origin");
            assert_ne!(org, pinned, "player moved OFF the exit to the intermission spot");
        });

        // --- Overlay pixels: render the same frozen frame with and without the
        // intermission flag; the plaque/number region (virtual x>=160, y 56..160)
        // is 3-D view in one and Sbar_IntermissionOverlay in the other.
        let (with_overlay, without_overlay) = walk_mut(|w| {
            let a = step_walk(w, 0.0, false, 320, 200).0;
            w.intermission = 0;
            let b = step_walk(w, 0.0, false, 320, 200).0;
            w.intermission = 1;
            (a, b)
        });
        let region_differs = (56..160).any(|y| {
            (160..320).any(|x| with_overlay.rgb[y * 320 + x] != without_overlay.rgb[y * 320 + x])
        });
        assert!(region_differs, "the intermission overlay painted the stats region");
        dump_frame("intermission-e1m1");

        // --- No button: the intermission HOLDS even long past exittime.
        for _ in 0..25 {
            step(0.1);
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "no button => still at the intermission");
            assert_eq!(w.map_name, "maps/e1m1.bsp", "no level change without a button");
        });

        // --- Attack pressed: IntermissionThink (time >= exittime, button down)
        // runs ExitIntermission -> GotoNextMap -> changelevel("e1m2"); the host
        // drains the pending request and swaps, carrying the inventory parms.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..5 {
            step(0.1);
            if walk_mut(|w| w.map_name.clone()) != "maps/e1m1.bsp" {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.map_name, "maps/e1m2.bsp", "the exit leads to e1m2");
            assert_eq!(w.intermission, 0, "the new level starts out of intermission");
            w.in_attack = false;
        });
        assert_eq!(
            player_field("ammo_shells") as i32,
            before_shells,
            "spawn parms carried the inventory across the swap"
        );
    }

    #[test]
    fn e1m7_exit_reaches_the_shareware_finale_and_sellscreen() {
        // Episode end: e1m7's exit runs the same intermission, but the SECOND
        // button press (ExitIntermission with intermission_running == 2 and
        // world.model == "maps/e1m7.bsp", cvar("registered") == 0) emits
        // svc_finale + the shareware episode text, and the THIRD press
        // (running == 3, shareware) emits svc_sellscreen — which pops the
        // Help/Ordering menu exactly like Cmd_ExecuteString("help").
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        close_menu();
        console_toggle();
        run_console_line("map e1m7");
        walk_mut(|w| assert_eq!(w.map_name, "maps/e1m7.bsp", "console map swap"));
        assert_eq!(console_visible(), 0, "a successful map command closed the console");
        close_menu(); // the fresh-walk path must not leave the menu gating input

        drive_into_exit();
        walk_mut(|w| assert_eq!(w.intermission, 1));

        // Hold attack: IntermissionThink exits as soon as time passes exittime
        // (time+2), then svc_finale arrives with the episode-end text.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            if walk_mut(|w| w.intermission) == 2 {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 2, "svc_finale set cl.intermission = 2");
            assert!(
                w.finale_text.starts_with("As the corpse of the monstrous entity"),
                "the shareware episode-1 finale text arrived; got {:?}",
                &w.finale_text[..w.finale_text.len().min(60)]
            );
            assert_eq!(w.map_name, "maps/e1m7.bsp", "the finale shows BEFORE any map change");
        });
        // Let ~1.5s of the slow text reveal pass, then dump the evidence frame.
        walk_mut(|w| w.in_attack = false);
        for _ in 0..15 {
            step(0.1);
        }
        dump_frame("finale-e1m7");

        // Third press (after the finale's exittime = time+1): shareware emits
        // svc_sellscreen; the dispatcher opens the menu on the Help screen.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            let open = APP.with(|c| c.borrow().as_ref().unwrap().menu.visible);
            if open {
                break;
            }
        }
        APP.with(|c| {
            let b = c.borrow();
            let menu = &b.as_ref().unwrap().menu;
            assert!(menu.visible, "svc_sellscreen popped the menu");
            assert_eq!(
                menu.screen(),
                render::MenuScreen::Help,
                "the sell screen is the Help/Ordering pages"
            );
        });
        walk_mut(|w| w.in_attack = false);
    }

    /// R_SetupFrame's r_dowarp: with the eye in water the view is rendered
    /// into the (at most) 320x200 warp buffer, and D_WarpScreen stretches it
    /// over the screen's view rectangle. So underwater a 640x400 screen and a
    /// 320x200 one render the same 320x152 view (the world pass writes the
    /// same pixels), while above water 640x400 draws four times as many.
    #[test]
    fn underwater_view_renders_into_the_warp_buffer() {
        use quake_rs::progs::OFS_PARM0;
        let mut w = build_walk().expect("e1m1 boots");
        let _ = step_walk(&mut w, 0.05, false, 320, 200);
        let p = w.player;
        w.server.vm.ent_set_float(p, "movetype", 8.0); // MOVETYPE_NOCLIP: stays put
        let frame_at = |w: &mut Walk, org: [f32; 3], rw: usize, rh: usize| {
            let vm = &mut w.server.vm;
            vm.set_gi(OFS_PARM0, p);
            vm.set_gv(OFS_PARM0 + 3, org);
            vm.argc = 2;
            let setorigin = vm.builtins[2];
            setorigin(vm).expect("setorigin");
            vm.ent_set_vector(p, "velocity", [0.0; 3]);
            render::render_stats_begin();
            let (img, _) = step_walk(w, 0.0, false, rw, rh);
            let px = render::render_stats_end().world_pixels;
            let eye = [org[0], org[1], org[2] + 22.0];
            assert_eq!((img.w, img.h), (rw, rh));
            (px, quake_rs::world::point_contents(&w.bsp, eye))
        };
        // e1m1's start pool, eye at the review's oracle view (750, 898, -332),
        // then lifted above the water in the same room.
        let (under, above) = ([750.0, 898.0, -354.0], [750.0, 898.0, -272.0]);
        let (u640, c) = frame_at(&mut w, under, 640, 400);
        assert_eq!(c, quake_rs::bsp::CONTENTS_WATER);
        let (u320, _) = frame_at(&mut w, under, 320, 200);
        // id's 48-row bar is (int)(48 * 200/400) = 24 rows of the warp buffer
        // at 640x400: a 320x176 render, taller than 320x200's 320x152.
        assert!(u640 > u320, "underwater: 320x176 at 640x400 ({u640} vs {u320})");
        {
            // The "scaled 2-D" extra's bar is 48 rows of the 320x200 screen.
            let _extra = Scaled2dGuard::set(true);
            let (s640, _) = frame_at(&mut w, under, 640, 400);
            assert_eq!(s640, u320, "underwater, scaled 2-D: the same 320x152 render at both sizes");
        }
        let (a640, c) = frame_at(&mut w, above, 640, 400);
        assert_eq!(c, quake_rs::bsp::CONTENTS_EMPTY);
        let (a320, _) = frame_at(&mut w, above, 320, 200);
        assert!(a640 > 3 * a320, "above water: 640x400 renders at full size ({a640} vs {a320})");
    }

    /// cl.time is the server's clock: on a local server CL_LerpPoint snaps it
    /// to the message time, sv.time after the frame's physics. So the sky,
    /// liquids, the underwater warp and R_AnimateLight's `(int)(cl.time*10)`
    /// start at SV_SpawnServer's 1.0 plus the signon frames, not at 0, stop
    /// with the server behind the menu, and follow it across a restart and a
    /// changelevel (a load: `save_load_round_trips_the_world_digest`).
    #[test]
    fn client_clock_is_the_server_clock() {
        let mut w = build_walk().expect("e1m1 boots");
        let t0 = w.server.time();
        assert!(t0 > 1.0, "sv.time at spawn: 1.0 + the signon frames ({t0})");
        assert_eq!(w.clock, t0, "the first frame draws at cl.time = sv.time");
        for _ in 0..5 {
            let _ = step_walk(&mut w, 0.05, false, 320, 200);
            assert_eq!(w.clock, w.server.time());
        }
        let t = w.clock;
        let _ = step_walk(&mut w, 0.05, true, 320, 200); // paused behind the menu
        assert_eq!((w.clock, w.server.time()), (t, t));
        try_restart(&mut w);
        assert_eq!(w.clock, w.server.time());
        assert!(w.clock < t, "a restarted level's clock starts over");
        try_changelevel(&mut w, "e1m2");
        assert_eq!(w.map_name, "maps/e1m2.bsp");
        assert_eq!(w.clock, w.server.time());
        assert!(w.clock > 1.0 && w.clock < 2.0, "{}", w.clock);
    }
}
