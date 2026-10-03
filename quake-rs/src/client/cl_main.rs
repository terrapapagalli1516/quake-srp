//! The live client frame — [`walk_frame`], one frame of the player's game as
//! the C client runs it against a local server: `CL_SendMove` (cl_input.c)
//! into the server tick, the client side of `CL_ParseServerMessage`
//! (cl_parse.c: intermission/finale, sounds, prints, particles, temp
//! entities), `CL_RelinkEntities` (cl_main.c), `V_CalcRefdef` (view.c) and
//! `SCR_UpdateScreen`'s 3-D view, blends, status bar and overlays (screen.c).

//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_main.c`, `cl_parse.c`, `cl_input.c`, `view.c`, `screen.c`.

use crate::bsp::Bsp;
use crate::cd_audio::CdCall;
use crate::mdl::Mdl;
use crate::particles::{TrailHead, TrailStep};
use crate::stepping::advance_clock;
use crate::render::{self, Camera, ModelInstance, Viewmodel};
use crate::server::{wire_angle, wire_coord, EntFlags, MoveType, SoundEvent, StaticEntity, UserCmd};
use crate::vm::{Fld, Glb};
use crate::tent::BeamModel;

use super::cl_input::{
    clamp_pitch, KeyMove, CL_ANGLESPEEDKEY, CL_PITCHSPEED, CL_YAWSPEED, SPEED, V_CENTERSPEED,
};
use super::cl_tent::{rocket_trail_type, spawn_temp_entity};
use super::host::host_error;
use super::host_cmd::{try_changelevel, try_restart, IT_INVISIBILITY};
use super::lerpmodels::{self, LerpModels};
use super::lerpmove::LerpMove;
use super::view::{
    cshift_add, fade_cshifts, parse_damage, stamp_item_gettime, stufftext_bonus_flash, BONUS_COLOR,
    BONUS_PERCENT, FACE_ANIM_TIME, V_KICKTIME,
};
use super::{
    backtile_for, color_for_name, lap, render_options, s_update, view_hook, ClientFrame,
    Listener, Phase, SoundCall, Vid, Walk,
};

/// `SV_WriteClientdataToMessage`'s fixangle (sv_main.c) and the client's
/// `svc_setangle` (cl_parse.c): when the QuakeC forced the player's facing
/// (`fixangle = 1` — teleporters, the intermission camera, PutClientInServer),
/// the server sends the entity's `angles` and clears the flag, and the client
/// takes them as `cl.viewangles`. This shell has no view roll, so the roll the
/// message carries (0 at every id1 site) is dropped; the pitch is clamped as
/// CL_AdjustAngles clamps it on the next move.
fn apply_fixangle(w: &mut Walk) {
    let p = w.player;
    if p < 0 || w.server.vm.ent_float(p, w.server.vm.fo().fixangle) == 0.0 {
        return;
    }
    let a = w.server.vm.ent_vec(p, w.server.vm.fo().angles);
    w.pitch = clamp_pitch(wire_angle(a[0]));
    w.yaw = wire_angle(a[1]);
    w.server.vm.set_ent_float(p, w.server.vm.fo().fixangle, 0.0);
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
    let take = vm.ent_float(p, vm.fo().dmg_take);
    let save = vm.ent_float(p, vm.fo().dmg_save);
    if take == 0.0 && save == 0.0 {
        return;
    }
    let mut other = vm.ent_int(p, vm.fo().dmg_inflictor);
    if other < 0 || other as usize >= vm.num_edicts() {
        other = 0;
    }
    let (o, mins, maxs) = (
        vm.ent_vec(other, vm.fo().origin),
        vm.ent_vec(other, vm.fo().mins),
        vm.ent_vec(other, vm.fo().maxs),
    );
    let coord = |i: usize| wire_coord(o[i] + 0.5 * (mins[i] + maxs[i]));
    let from = [coord(0), coord(1), coord(2)];
    vm.set_ent_float(p, vm.fo().dmg_take, 0.0);
    vm.set_ent_float(p, vm.fo().dmg_save, 0.0);
    let byte = |f: f32| (f as i32) & 255;
    let pd = parse_damage(byte(save), byte(take), from, ent_origin, [w.pitch, w.yaw, 0.0]);
    w.damage_blend = cshift_add(w.damage_blend, pd.percent);
    w.damage_count += pd.percent / 3.0;
    w.damage_color = pd.color;
    w.v_dmg_roll = pd.roll;
    w.v_dmg_pitch = pd.pitch;
    w.v_dmg_time = V_KICKTIME;
    w.faceanimtime = w.server.time() + FACE_ANIM_TIME;
}

/// The dynamic lights R_PushDlights marks this frame: every slot with a
/// radius whose `die` has not passed (`die < cl.time || !radius` is skipped),
/// at its current — not yet decayed — radius.
pub fn pushed_dlights(
    dlights: &crate::dlight::DynamicLights,
    now: f32,
) -> Vec<crate::dlight::DynamicLight> {
    dlights.active().into_iter().filter(|dl| dl.die >= now).collect()
}

/// `cl.punchangle` as SV_WriteClientdataToMessage sends it: each component
/// through `MSG_WriteChar` — the float truncated to an int, kept as a signed
/// byte — so the shotgun's -2 kick reads -2, then -1 while DropPunchAngle eases
/// the server's value back, then 0: whole-degree steps, not a smooth ease.
pub fn client_punchangle(w: &Walk) -> [f32; 3] {
    let p = w.server.vm.ent_vec(w.player, w.server.vm.fo().punchangle);
    p.map(|v| (v as i32) as i8 as f32)
}

/// `r_lerpmodels`' "model identity" for the view weapon ([`lerpmodels::FrameLerps::blend`]):
/// the view weapon is not an edict, so it has no `modelindex` of its own; id's
/// client knows it by `cl.stats[STAT_WEAPON]`, `SV_ModelIndex` of the player's
/// `weaponmodel` — the same precache index a demo's view weapon is keyed by
/// (`cl_demo.rs`). 0 if the model was never precached (QuakeC precaches
/// every weapon it sets, so only a broken progs gets here).
fn weapon_model_index(w: &Walk, name: &str) -> usize {
    w.server.vm.host().and_then(|h| h.find_model(name)).unwrap_or(0) as usize
}

/// `cl.items` as SV_WriteClientdataToMessage sends it: the player's `items`
/// with more bits above them. For id1's progs those are the rune bits
/// "stuffed into the high bits of items for sbar" — `(int)ent->v.items |
/// ((int)pr_global_struct->serverflags << 28)`; QC `sigil_touch` only sets
/// `serverflags`, so this is how a rune reaches the status bar. A progs that
/// declares the field `items2` (`GetEdictFieldValue(ent, "items2")`: the
/// mission packs') gets `items | ((int)items2 << 23)` instead, and no runes:
/// that is where Hipnotic keeps its wetsuit and empathy shields and Rogue its
/// armour types, ammo types, shield and anti-grav belt, the bits `sbar.c`
/// reads at 23 and up.
pub fn client_items(w: &Walk) -> i32 {
    server_items(&w.server, w.player)
}

/// [`client_items`] for a `server` and `player` not yet in a [`Walk`] (a
/// level being assembled).
pub fn server_items(server: &crate::server::Server, player: i32) -> i32 {
    let vm = &server.vm;
    let items = vm.ent_float(player, vm.fo().items) as i32;
    let high = if vm.fo().items2.is_declared() {
        (vm.ent_float(player, vm.fo().items2) as i32) << 23
    } else {
        (server.serverflags() as i32) << 28
    };
    items | high
}

/// Owned visible-entity descriptor gathered from the server before rendering:
/// `(model name, origin, angles, frame, shirt/pants colour, skin, blend)`.
/// `blend` is `r_lerpmodels`' ([`ModelInstance::blend`]) — `None` for a
/// static (its frame never changes).
type EntityDesc = (String, [f32; 3], [f32; 3], usize, [u8; 3], i32, Option<(usize, f32)>);

/// What a static entity draws as, gathered before the camera is known.
enum StaticDraw {
    Alias(EntityDesc),
    Brush(render::BModelInstance),
    External(String, [f32; 3]),
    /// Model, origin, angles, frame.
    Sprite(String, [f32; 3], [f32; 3], usize),
}

/// A static entity waiting for [`static_is_visible`]: what it draws as, and
/// the box `R_AddEfrags` splits into leaves — `origin + model->mins` ..
/// `origin + model->maxs`.
struct StaticDesc {
    draw: StaticDraw,
    emins: [f32; 3],
    emaxs: [f32; 3],
}

/// `model->mins`/`maxs` of an alias model: `Mod_LoadAliasModel` sets a fixed
/// ±16 box ("FIXME: do this right").
pub const ALIAS_MODEL_HALF: f32 = 16.0;

/// Whether `R_StoreEfrags` reaches a static entity this frame: one of the
/// non-solid leaves its box touches (`R_SplitEntityOnNode`) is in `view_pvs`,
/// the PVS `R_MarkLeaves` marks from the view leaf. (The C also skips a leaf
/// whose node box `R_RecursiveWorldNode` rejects against the frustum;
/// `R_AliasCheckBBox` and the z-buffer hide whatever that would have hidden,
/// barring a mesh that reaches past its efrag box.)
pub fn static_is_visible(bsp: &Bsp, view_pvs: &[bool], emins: [f32; 3], emaxs: [f32; 3]) -> bool {
    let mut seen = false;
    bsp.touched_leafs(emins, emaxs, &mut |leaf| {
        seen = view_pvs.get(leaf).copied().unwrap_or(false);
        !seen
    });
    seen
}

/// `CL_ParseStatic` + `R_AddEfrags` for one of the signon's statics: what it
/// draws as, at the record's origin, angles, frame and skin, and its efrag box
/// (`origin + model->mins` .. `origin + model->maxs`). `None` for no model
/// (`R_AddEfrags`' `if (!ent->model) return;`) or one the client could not load.
fn static_desc(w: &Walk, st: &StaticEntity) -> Option<StaticDesc> {
    let StaticEntity { model: m, origin, angles, .. } = st;
    let (origin, angles, frame) = (*origin, *angles, usize::from(st.frame));
    let (draw, mins, maxs) = if let Some(num) = m.strip_prefix('*') {
        // model->mins/maxs of "*N": the submodel's spread bounds.
        let model_index = num.parse::<usize>().ok()?;
        let sub = w.bsp.models.get(model_index)?;
        let inst = render::BModelInstance { model_index, origin, frame: frame as i32, angles };
        (StaticDraw::Brush(inst), sub.mins, sub.maxs)
    } else if m.ends_with(".bsp") && *m != w.map_name {
        // model->mins/maxs: the box's own model 0 bounds.
        let bm = w.bmodel_cache.get(m)?.as_ref()?.models.first()?;
        (StaticDraw::External(m.clone(), origin), bm.mins, bm.maxs)
    } else if m.ends_with(".spr") {
        // Mod_LoadSpriteModel: ±maxwidth/2 across, ±maxheight/2 up (integer
        // halves).
        let spr = w.sprite_cache.get(m)?.as_ref()?;
        let (hw, hh) = ((spr.header.width / 2) as f32, (spr.header.height / 2) as f32);
        (StaticDraw::Sprite(m.clone(), origin, angles, frame), [-hw, -hw, -hh], [hw, hw, hh])
    } else if m.ends_with(".mdl") {
        // A static's frame never changes: no blend.
        let desc = (m.clone(), origin, angles, frame, color_for_name(m), i32::from(st.skin), None);
        let h = ALIAS_MODEL_HALF;
        (StaticDraw::Alias(desc), [-h; 3], [h; 3])
    } else {
        return None;
    };
    let (emins, emaxs) = offset_box(origin, mins, maxs);
    Some(StaticDesc { draw, emins, emaxs })
}

/// Box `origin + mins` .. `origin + maxs`.
pub fn offset_box(origin: [f32; 3], mins: [f32; 3], maxs: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    (
        [origin[0] + mins[0], origin[1] + mins[1], origin[2] + mins[2]],
        [origin[0] + maxs[0], origin[1] + maxs[1], origin[2] + maxs[2]],
    )
}

/// The frame of a game `Host_Error` has ended: nothing — the client is
/// disconnected, and id's console covers the screen (`con_forcedup`) — and
/// what it said to the sound layer.
fn disconnected_frame(vid: &Vid, sound: Vec<SoundCall>) -> ClientFrame {
    ClientFrame { image: render::Image::new(vid.width, vid.height, 0), cshifts: Vec::new(), sound }
}

/// One live client frame (see the module doc) of `host_frametime` seconds —
/// `Host_FilterTime`'s double, which the server's `sv.time` advances by
/// exactly (`client_frame_f64`); the client's own timing takes it as an `f32`.
/// A QuakeC error in the server's frame (or in a level change it makes) is
/// `Host_Error`: [`host_error`] ends the game, and this frame and every later
/// one is the disconnected screen, until the host drops the walk.
pub fn walk_frame(w: &mut Walk, host_frametime: f64, menu_up: bool, vid: &Vid) -> ClientFrame {
    if w.host_error.is_some() {
        return disconnected_frame(vid, Vec::new());
    }
    let dt = host_frametime as f32;
    let (render_w, render_h) = (vid.width, vid.height);
    let mut sound = Vec::new();
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
    // countdown and notify expiry, the ambient-sound ramps. The `pause`
    // command stops the server the same way (`!sv.paused` in the same tests)
    // for as long as it is on, and the client with it: `svc_setpause` sets
    // `cl.paused` in the same host frame on a local server, and while it is
    // set V_RenderView does not run V_CalcRefdef — the view keeps its last
    // angles, kick and stair smoothing — and SCR_DrawPause shows the plaque.
    let cl_paused = w.server.paused;
    let paused = menu_up || cl_paused;
    // Guard against a non-finite/negative dt so the clock only moves forward.
    // (cl.time, `w.clock`, follows the server's clock below.)
    if dt.is_finite() && dt > 0.0 {
        advance_clock(w.stepping, &mut w.host_time, &mut w.host_clock, dt);
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
        // The mission packs' re-release `finaleFinished` builtin (#79,
        // server::pr_cmds::bi_finale_finished): latch it once the end-of-pack
        // finale/credits text — cl.intermission 2 (the plaque) or 3 (text
        // alone) — has been fully typewriter-revealed
        // (`screen::finale_text_fully_revealed`, the same budget math the
        // draw uses) AND the player has pressed a button THIS frame (the
        // simplification: `cmd.buttons`, i.e. +attack or +jump — the two
        // keys/clicks this port already turns into a UserCmd bit; a bare
        // movement key does not count as "pressing a key" here). Single
        // player keeps server and client state in the same `Walk`, so this
        // is the only place able to feed the server's builtin what only the
        // client knows. id1 never declares the builtin, so this is inert
        // for it (AUDIT.md "The mission packs' paths", P7/B4).
        if w.intermission >= 2
            && cmd.buttons != 0
            && crate::screen::finale_text_fully_revealed(&w.finale_text, w.clock - w.finale_start)
        {
            w.server.set_finale_finished(true);
        }
        // A queued impulse is sent once (CL_SendMove: `in_impulse = 0`). While
        // paused it waits: the C's SV_ReadClientMove still stores it on the
        // edict behind the menu, and it runs when the server does.
        w.next_impulse = 0;
        let before = w.server.vm.ent_vec(w.player, w.server.vm.fo().origin);
        if let Err(e) = w.server.client_frame_stepped(&cmd, host_frametime, w.stepping) {
            // Host_Error longjmps out of the host frame: none of this frame's
            // messages reach the client.
            host_error(w, &e, &mut sound);
            return disconnected_frame(vid, sound);
        }
        // CL_LerpPoint on a local server: cl.time = the message time, sv.time
        // after this frame's physics.
        w.clock = w.server.time();
        apply_fixangle(w);
        parse_client_damage(w, before);
        // svc_stufftext to this client (PF_stuffcmd): the bonus flash.
        for (ent, text) in w.server.drain_stufftext() {
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
            crate::server::SvcEvent::Intermission => {
                // cl.intermission = 1; cl.completed_time = cl.time (cl_parse.c:939).
                // On a local server cl.time is sv.time (`Walk::clock`), which
                // SV_SpawnServer starts at 1.0, so the overlay's minutes:seconds
                // shows exactly what vanilla shows. (The demo parser latches
                // mtime[0], also server time — the paths agree.)
                w.intermission = 1;
                w.completed_time = w.server.time();
            }
            crate::server::SvcEvent::Finale(text) => {
                // cl.intermission = 2 + SCR_CenterPrint (scr_centertime_start).
                // completed_time = cl.time = sv.time, as above; the reveal uses
                // w.clock - finale_start (cl.time - scr_centertime_start in the C).
                //
                // AUDIT P6/B3: WriteString carries no var-args for
                // PF_VarString/var_string to resolve, so a mission pack's
                // finale text (e.g. "$qc_finale_hip1") arrives as the raw
                // key; look it up here, at the client, right where it is
                // about to be shown (id1's text is never a key, so this is a
                // no-op for it — crate::localization::resolve falls back to
                // the text itself).
                w.intermission = 2;
                w.completed_time = w.server.time();
                w.finale_text = crate::localization::resolve(w.server.vm.loc_table(), &text);
                w.finale_start = w.clock;
            }
            crate::server::SvcEvent::Cutscene(text) => {
                // cl.intermission = 3 (text only, no plaque); times as per Finale.
                w.intermission = 3;
                w.completed_time = w.server.time();
                w.finale_text = crate::localization::resolve(w.server.vm.loc_table(), &text);
                w.finale_start = w.clock;
            }
            crate::server::SvcEvent::SellScreen => {
                // Cmd_ExecuteString("help"): the dispatcher opens the Help menu.
                w.pending_sellscreen = true;
            }
            crate::server::SvcEvent::CdTrack { track, .. } => {
                // CDAudio_Play ((byte)cl.cdtrack, true).
                sound.push(SoundCall::Cd(CdCall::cdtrack(track)));
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
        try_changelevel(w, &next_map, &mut sound);
        // The swap reset the world; render this frame from the *new* level so the
        // player never sees a frame straddling two maps.
    } else if w.server.take_pending_restart() {
        // Single-player respawn: QuakeC ran localcmd("restart") (a dead player who
        // pressed a button). Reload the current level with the entry inventory.
        // `else if` so a changelevel this frame takes precedence over a restart.
        try_restart(w, &mut sound);
    }
    if w.host_error.is_some() {
        return disconnected_frame(vid, sound); // the new level's QuakeC failed
    }
    // The mission packs' re-release-only end-of-game credits roll
    // (localcmd("menu_credits\n"), builtin #79 finally true): surface it for
    // whatever owns the session ([`Walk::pending_menu_credits`]) — a plain
    // flag like `pending_sellscreen`, since quake-rs itself has no notion of
    // a Quit screen to show. `id1` never sets this.
    if w.server.take_pending_menu_credits() {
        w.pending_menu_credits = true;
    }

    // CL_ParseClientdata's item get-times (the new-weapon icon flash), on the
    // server clock the HUD reads — after any level swap above, which seeded
    // cl.items with the spawn items (see `stamp_item_gettime`).
    let items = client_items(w);
    let now_sv = w.server.time();
    stamp_item_gettime(&mut w.cl_items, &mut w.item_gettime, items, now_sv);

    // 2. Surface the sounds the world fired this frame (gunshots, doors, monster
    //    voices) to the sound layer, each where CL_ParseStartSoundPacket's
    //    MSG_ReadCoords put it: to the 1/8 unit.
    let events: Vec<SoundEvent> = w
        .server
        .drain_sounds()
        .into_iter()
        .map(|e| SoundEvent { origin: e.origin.map(wire_coord), ..e })
        .collect();
    sound.push(SoundCall::Start { events, view_entity: w.player });

    // 2a. Drain QuakeC's on-screen messages (centerprint / sprint / bprint) into
    //     the timed display state, and expire old ones on the host clock
    //     (the C times both off realtime / host_frametime, paused or not).
    // The VM's output log repeats these and adds dprint's developer text:
    // dropped, as id's `developer 0` does (Server::drain_output).
    let _ = w.server.drain_output();
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
    let mut te_sounds: Vec<SoundEvent> = Vec::new();
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
            use crate::server::te_consts::*;
            // Only TE_EXPLOSION and TE_EXPLOSION2 flash a dynamic light in id's
            // CL_ParseTEnt; TE_TAREXPLOSION (blob) does NOT.
            if matches!(ev.te_type, TE_EXPLOSION | TE_EXPLOSION2) {
                w.dlights.alloc(0, ev.pos, 350.0, now + 0.5, 300.0, 0.0, now);
            }
        }
        if let Some(name) = spawn_temp_entity(&mut w.particles, ev, now, &mut w.prng) {
            // CL_ParseTEnt read the position with MSG_ReadCoord, so the sound
            // starts to the 1/8 unit (the particles above still start at the
            // unrounded position: an open item, in the particles' code).
            te_sounds.push(SoundEvent {
                entity: 0,
                channel: 0,
                sound_index: -1,
                sample: name.to_string(),
                origin: ev.pos.map(wire_coord),
                volume: 1.0,
                attenuation: 1.0,
            });
        }
    }
    if !te_sounds.is_empty() {
        // Temp-entity sounds (explosions, wall impacts) carry entity=0,
        // channel=0 -> never the view entity, never channel-restarted, so each
        // distinct explosion queues separately at its own origin.
        sound.push(SoundCall::Start { events: te_sounds, view_entity: w.player });
    }
    // CL_RelinkEntities (cl_main.c) relinks only what the server sent this
    // frame — SV_WriteEntitiesToClient's test: a model, and a leaf from the
    // entity's last SV_LinkEdict in the fat PVS at the player's eye (the player
    // is always sent) — and skips a slot whose model is null, which only the
    // player's can be. Everything the client does with an entity (its EF_*
    // lights, trails, spin and drawing) is gated on this; an entity out of the
    // PVS cannot light the far side of a wall.
    let mut relinked = w.server.entities_sent_to_client();
    if w.server.vm.ent_float(w.player, w.server.vm.fo().modelindex) == 0.0 {
        if let Some(r) = usize::try_from(w.player).ok().and_then(|p| relinked.get_mut(p)) {
            *r = false;
        }
    }
    let is_relinked =
        |e: i32| usize::try_from(e).ok().and_then(|e| relinked.get(e)).copied() == Some(true);

    // 2d. Entity light effects (EF_MUZZLEFLASH / BRIGHTLIGHT / DIMLIGHT) from the
    //     relinked edicts. The rand()&31 radius jitter is added here (entity_dlights
    //     stays a pure query). Then decay + retire the whole pool for this frame.
    for ed in w.server.entity_dlights() {
        if !is_relinked(ed.key) {
            continue;
        }
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
    // 3. Make sure every live entity's model is cached (runtime-spawned
    //    entities — gibs, projectiles — can appear after boot), and every
    //    static's.
    // (The name is borrowed from the string heap; only a miss allocates.)
    let n = w.server.vm.num_edicts();
    let f_model = w.server.vm.fo().model;
    let vm = &w.server.vm;
    let live = (0..n as i32).filter(|&e| !vm.is_free_edict(e)).map(|e| vm.ent_str(e, f_model));
    for m in live.chain(w.server.statics().iter().map(|st| st.model.as_str())) {
        if m.ends_with(".mdl") && !w.model_cache.contains_key(m) {
            let parsed = w.pak.read_file(m).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
            w.model_cache.insert(m.to_string(), parsed);
        } else if m.ends_with(".bsp") && m != w.map_name && !w.bmodel_cache.contains_key(m) {
            // An external brush-model item box (maps/b_*.bsp). Parse once and cache;
            // a missing/unparseable box stores `None` so we never re-read or panic.
            let parsed = w.pak.read_file(m).ok().flatten().and_then(|b| Bsp::parse(&b).ok());
            w.bmodel_cache.insert(m.to_string(), parsed);
        } else if m.ends_with(".spr") && !w.sprite_cache.contains_key(m) {
            // A sprite-model entity (progs/s_explod.spr explosion flash, bubbles).
            // Parse once and cache; None on missing/unparseable.
            let parsed =
                w.pak.read_file(m).ok().flatten().and_then(|b| crate::spr::Sprite::parse(&b).ok());
            w.sprite_cache.insert(m.to_string(), parsed);
        }
    }

    // The player's first-person weapon viewmodel ("progs/v_shot.mdl" etc.) lives
    // on the `weaponmodel` field (separate from `model`); cache it like any MDL.
    let weapon_name = w.server.vm.ent_str(w.player, w.server.vm.fo().weaponmodel).to_string();
    if weapon_name.ends_with(".mdl") && !w.model_cache.contains_key(&weapon_name) {
        let parsed = w.pak.read_file(&weapon_name).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
        w.model_cache.insert(weapon_name.clone(), parsed);
    }
    let weapon_frame = w.server.vm.ent_float(w.player, w.server.vm.fo().weaponframe).max(0.0) as usize;

    // 4. Gather the visible entities (owned descriptors, so the cache borrow for
    //    rendering doesn't clash with reading the server). Skip the player's own
    //    edict — its model would fill the screen in first person.
    // (model name, origin, angles, frame, shirt/pants colour, skin) per entity.
    let mut descs: Vec<EntityDesc> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    // Projectile/gib trails to spawn this frame, collected here and emitted after
    // the loop (so we don't borrow w.particles/dlights while reading the server):
    // (entity, new origin, R_RocketTrail type); the old origin is its trail head.
    let mut trail_spawns: Vec<(i32, [f32; 3], i32)> = Vec::new();
    // External brush-model items (maps/b_*.bsp) as owned (name, origin) pairs; the
    // borrowing `ExternalBModel` list is built below, after the cache is final, so
    // the immutable cache borrow does not clash with reading the server here.
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    // Sprite-model entities (name, origin, angles, frame, and how many alias
    // entries came before it: R_DrawEntitiesOnList draws both kinds in one
    // list): the explosion flash, bubbles. Resolved against the sprite cache
    // after the loop (disjoint borrows).
    type SpriteDesc = (String, [f32; 3], [f32; 3], usize, usize);
    let mut sprite_descs: Vec<SpriteDesc> = Vec::new();
    // Drop trail history for any edict that is currently free. When `ED_Free`
    // recycles a slot for a new trailed entity (rocket/grenade/gib), a stale
    // `trail_org[ent]` from the previous occupant would make R_RocketTrail draw a
    // spurious streak from the old entity's last origin to the new spawn point.
    // Pruning here lets a reused slot start fresh (oldorg defaults to its own
    // origin below, so no trail on the first frame). Disjoint field borrows.
    {
        let vm = &w.server.vm;
        w.trail_org
            .retain(|&e, _| !vm.is_free_edict(e));
    }
    let smooth = w.lerpmove == LerpMove::Smooth;
    let smooth_frames = w.lerpmodels == LerpModels::Smooth;
    for e in 0..n {
        let ent = e as i32;
        if ent == w.player || w.server.vm.is_free_edict(ent) {
            continue;
        }
        if !is_relinked(ent) {
            // Not sent this frame: CL_RelinkEntities nulls its model. When it
            // is sent again CL_ParseUpdate forcelinks it to the new origin, so
            // its trail restarts there.
            w.trail_org.remove(&ent);
            continue;
        }
        // Render an entity only when it has a real modelindex — i.e. its QuakeC
        // spawn actually called setmodel. An edict that early-returns before
        // setmodel (e.g. func_episodegate in shareware: serverflags=0 so the gate
        // stays passable) keeps its raw map `model` key like "*41" but never gets
        // a modelindex, and Quake leaves it invisible. Without this guard those
        // gates draw as phantom walls the player walks through — and mask the real
        // slipgate behind them, so episode/level selection *looks* broken.
        if w.server.vm.ent_float(ent, w.server.vm.fo().modelindex) == 0.0 {
            continue;
        }
        let m = w.server.vm.ent_str(ent, w.server.vm.fo().model).to_owned();
        // Brush submodels (doors, platforms, buttons) draw at the entity origin —
        // their origin tracks the door's open/close motion, so they animate live.
        if let Some(num) = m.strip_prefix('*') {
            if let Ok(idx) = num.parse::<usize>() {
                let origin = w.server.vm.ent_vec(ent, w.server.vm.fo().origin);
                // The entity's `frame` selects the alternate (+a..+j) texture cycle
                // for activated buttons/doors (a pressed button shows its lit face).
                let frame = w.server.vm.ent_float(ent, w.server.vm.fo().frame) as i32;
                // The entity's current `angles`: zero for the shareware's
                // doors/plats/buttons, turning live for the mission packs'
                // func_rotate_door/func_rotate_train/func_rotate_entity.
                let angles = w.server.vm.ent_vec(ent, w.server.vm.fo().angles);
                bmodels.push(render::BModelInstance { model_index: idx, origin, frame, angles });
            }
            continue;
        }
        // External brush-model item boxes: a standalone b_*.bsp the item set as its
        // model (explosive box, ammo/health boxes). Not the world map itself.
        if m.ends_with(".bsp") {
            if m != w.map_name {
                let origin = w.server.vm.ent_vec(ent, w.server.vm.fo().origin);
                ext_descs.push((m, origin));
            }
            continue;
        }
        // Sprite-model entities (s_explod.spr explosion flash, bubbles, the
        // mission packs' bullet holes): a poster at the entity origin, turned
        // by its angles when the sprite is SPR_ORIENTED (render/sprite.rs),
        // current `frame` for the animation.
        if m.ends_with(".spr") {
            let origin = w.server.vm.ent_vec(ent, w.server.vm.fo().origin);
            let angles = w.server.vm.ent_vec(ent, w.server.vm.fo().angles);
            let frame = w.server.vm.ent_float(ent, w.server.vm.fo().frame).max(0.0) as usize;
            sprite_descs.push((m, origin, angles, frame, descs.len()));
            continue;
        }
        if !m.ends_with(".mdl") {
            continue;
        }
        let origin = w.server.vm.ent_vec(ent, w.server.vm.fo().origin);
        let frame = w.server.vm.ent_float(ent, w.server.vm.fo().frame).max(0.0) as usize;
        let color = color_for_name(&m);
        // The model header flags (rocket/grenade/gib/tracer trails + EF_ROTATE).
        let cached_mdl = w.model_cache.get(&m).and_then(|o| o.as_ref());
        let mflags = cached_mdl.map(|md| md.header.flags).unwrap_or(0);
        // r_lerpmodels (the 2026 extra): a group frame (a torch's flicker) is
        // not a motion between two poses — [`lerpmodels::FrameLerps::blend`]
        // snaps instead of blending across one.
        let frame_is_group = cached_mdl.is_some_and(|md| md.frame_is_group(frame as i32));
        // CL_RelinkEntities (cl_main.c:531): a model carrying EF_ROTATE (bonus
        // pickups — ammo/health/armour boxes, weapons, keys, runes, powerups) has
        // its yaw overwritten with `anglemod(100*cl.time)` every frame so it spins.
        // Otherwise use the entity's own yaw. Without this every pickup sat frozen.
        let ent_angles = w.server.vm.ent_vec(ent, w.server.vm.fo().angles);
        let yaw = if mflags & crate::demo::EF_ROTATE != 0 {
            crate::demo::rotate_yaw(w.clock)
        } else {
            ent_angles[1]
        };
        // [pitch, yaw, roll]: EF_ROTATE overrides yaw only; pitch/roll come straight
        // from the entity so flying projectiles point along their flight path
        // (r_alias.c R_AliasSetUpTransform), not just spin about Z.
        let angles = [ent_angles[0], yaw, ent_angles[2]];
        // Per-entity skin index (R_AliasSetupSkin: `skinnum = currententity->skinnum`).
        // Drives e.g. armor.mdl's 3 skins (green/yellow/red); was hardcoded to 0.
        let skin = w.server.vm.ent_float(ent, w.server.vm.fo().skin).max(0.0) as i32;
        // R_RocketTrail: a model with a rocket/grenade/gib/tracer header flag
        // trails particles from its previous origin to here (CL_RelinkEntities).
        if let Some(ttype) = rocket_trail_type(mflags) {
            w.trail_org.entry(ent).or_insert(TrailHead::at(origin));
            trail_spawns.push((ent, origin, ttype));
        }
        let model_index = w.server.vm.ent_float(ent, w.server.vm.fo().modelindex) as usize;
        // r_lerpmove (the 2026 extra): a monster glides between its steps
        // where it is drawn; its trail and everything else keep the server's
        // origin.
        let (origin, angles) = if smooth && w.server.vm.movetype(ent) == MoveType::Step {
            let drawn = w.glides.draw(ent, model_index, origin, angles, f64::from(w.clock));
            (drawn.origin, drawn.angles)
        } else {
            (origin, angles)
        };
        // r_lerpmodels (the 2026 extra): blend this entity's animation
        // toward `frame` from whatever frame it was at a moment ago.
        let blend = if smooth_frames {
            w.frame_lerps.blend(ent, model_index, frame, frame_is_group, origin, f64::from(w.clock))
        } else {
            None
        };
        descs.push((m, origin, angles, frame, color, skin, blend));
    }
    if smooth {
        w.glides.end_frame();
    } else {
        w.glides.clear();
    }
    if smooth_frames {
        w.frame_lerps.end_frame();
    } else {
        w.frame_lerps.clear();
    }

    // The signon's statics (cl_static_entities), each hung on the leaves its
    // box touches (R_AddEfrags); they wait for the camera: R_StoreEfrags draws
    // one when a leaf it touches is in the view's PVS (after the camera, below).
    let statics: Vec<StaticDesc> = w.server.statics().iter().filter_map(|st| static_desc(w, st)).collect();

    // Emit the collected trails (after the entity loop to keep the borrows
    // disjoint). Each trails from its head to its new origin; EF_ROCKET also
    // flashes a small dynamic light at the rocket head.
    let step = TrailStep { stepping: w.stepping, dt, now };
    for (ent, neworg, ttype) in trail_spawns.drain(..) {
        if let Some(head) = w.trail_org.get_mut(&ent) {
            w.particles.spawn_trail(head, neworg, ttype, step, &mut w.tracercount, &mut w.prng);
        }
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
            w.server.vm.ent_vec(w.player, w.server.vm.fo().origin),
            w.server.vm.ent_vec(w.player, w.server.vm.fo().angles),
        )
    } else {
        w.server.player_view()
    };
    let vel = w.server.vm.ent_vec(w.player, w.server.vm.fo().velocity);
    let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
    let bob = render::view_bob(speed_xy, w.clock);

    // Record the listener pose so the sound layer can spatialize this frame's
    // sounds. Forward/right are the level (no-pitch) yaw basis, matching the
    // renderer's `Camera::basis`: yaw rotates in XY about +Z, right is forward
    // turned -90 deg. Panning only needs the horizontal plane.
    let yaw_rad = (ang[1] as f64).to_radians();
    let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
    let listener = Listener {
        pos: eye,
        forward: [cy, sy, 0.0],
        right: [sy, -cy, 0.0],
    };

    // S_UpdateAmbientSounds: ramp the four automatic ambient channels toward
    // the VIEW leaf's ambient_level[] targets (water wash / sky wind). Uses the
    // same steady (un-bobbed) eye as the listener pose above.
    sound.push(s_update(&w.bsp, listener, dt));

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
        let origin_z = w.server.vm.ent_vec(w.player, w.server.vm.fo().origin)[2];
        let onground = w.server.vm.flags(w.player).contains(EntFlags::ONGROUND);
        // `steptime = cl.time - cl.oldtime`: nothing while the server is
        // paused (behind the menu, or by `pause`), so the eye stays put.
        let steptime = if paused { 0.0 } else { dt.max(0.0) };
        if w.oldz.is_finite() && onground && origin_z - w.oldz > 0.0 {
            w.oldz += steptime * 80.0;
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
        let body_angles = w.server.vm.ent_vec(w.player, w.server.vm.fo().angles);
        let mut roll = crate::server::v_calc_roll(body_angles, vel) + punch[2];
        let mut kick_pitch = 0.0;
        if w.v_dmg_time > 0.0 {
            roll += w.v_dmg_time / V_KICKTIME * w.v_dmg_roll;
            kick_pitch = w.v_dmg_time / V_KICKTIME * w.v_dmg_pitch;
            if !cl_paused {
                w.v_dmg_time -= if dt.is_finite() { dt.max(0.0) } else { 0.0 };
            }
        }
        if w.server.vm.ent_float(w.player, w.server.vm.fo().health) <= 0.0 {
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
    // R_MarkLeaves / R_StoreEfrags: the statics whose leaves the view's PVS
    // (from the leaf holding r_refdef.vieworg, not fattened) reaches join the
    // frame after the relinked entities, as they join cl_visedicts in the C.
    if !statics.is_empty() {
        let view_leaf = render::point_in_leaf(&w.bsp, cam.pos).unwrap_or(0);
        let view_pvs = w.bsp.leaf_pvs(view_leaf);
        for st in statics {
            if !static_is_visible(&w.bsp, &view_pvs, st.emins, st.emaxs) {
                continue;
            }
            match st.draw {
                StaticDraw::Alias(d) => descs.push(d),
                StaticDraw::Brush(b) => bmodels.push(b),
                StaticDraw::External(name, origin) => ext_descs.push((name, origin)),
                StaticDraw::Sprite(name, origin, angles, frame) => {
                    sprite_descs.push((name, origin, angles, frame, descs.len()))
                }
            }
        }
    }
    let mut instances: Vec<ModelInstance> = descs
        .iter()
        .filter_map(|(name, origin, angles, frame, color, skin, blend)| match w.model_cache.get(name) {
            Some(Some(mdl)) => Some(ModelInstance {
                mdl,
                origin: *origin,
                yaw: angles[1],
                pitch: angles[0],
                roll: angles[2],
                color: *color,
                frame: *frame,
                blend: *blend,
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
        let player_org = w.server.vm.ent_vec(w.player, w.server.vm.fo().origin);
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
                    blend: None,
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
    // Sprite-model entities: resolve each (name, origin, angles, frame) against
    // the sprite cache, dropping any whose .spr was missing/unparseable, each
    // after the resolved models that came before it on the list.
    let resolved_before: Vec<usize> = std::iter::once(0)
        .chain(descs.iter().scan(0, |n, (name, ..)| {
            *n += usize::from(matches!(w.model_cache.get(name), Some(Some(_))));
            Some(*n)
        }))
        .collect();
    let sprites: Vec<render::SpriteInstance> = sprite_descs
        .iter()
        .filter_map(|(name, origin, angles, frame, k)| match w.sprite_cache.get(name) {
            Some(Some(spr)) => Some(render::SpriteInstance {
                sprite: spr,
                origin: *origin,
                angles: *angles,
                frame: *frame,
                models_before: resolved_before[*k],
            }),
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
        || w.server.vm.ent_float(w.player, w.server.vm.fo().health) <= 0.0
        || (w.server.vm.ent_float(w.player, w.server.vm.fo().items) as i32) & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match w.model_cache.get(&weapon_name) {
            // V_CalcRefdef's gun origin (the forward bob + the viewsize fudge)
            // and CalcGunAngle's angles (the view before the punch, no lean).
            Some(Some(mdl)) => {
                let punch = client_punchangle(w);
                let angles = render::viewmodel_angles(&cam, punch, ang[2]);
                // r_lerpmodels: the gun blends too (a weapon switch is a model
                // change, which snaps like any other). The view weapon is not
                // an edict, so its "model identity" is its precache index
                // (STAT_WEAPON's), under lerpmodels::VIEWMODEL's sentinel key.
                let blend = if smooth_frames {
                    let model_id = weapon_model_index(w, &weapon_name);
                    let is_group = mdl.frame_is_group(weapon_frame as i32);
                    w.frame_lerps.blend(lerpmodels::VIEWMODEL, model_id, weapon_frame, is_group, cam.pos, f64::from(w.clock))
                } else {
                    None
                };
                Some(Viewmodel {
                    mdl,
                    frame: weapon_frame,
                    blend,
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
    // (in id's layout the view sits ABOVE the status bar; with the 2026 bar
    // over the view it takes the bar's rows too; either way it is projected
    // about its own centre) and how much status bar shows; an intermission
    // is always full screen.
    lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, w.viewsize, intermission, w.sbar_layout);
    let vrect = refdef.vrect;
    // R_SetupFrame's r_dowarp (r_waterwarp 1): with the eye's leaf in water,
    // slime or lava the view is rendered into the (at most 320x200) warp
    // buffer, and D_WarpScreen stretches it over `vrect` below.
    let eye_contents = crate::world::point_contents(&w.bsp, eye);
    let dowarp = eye_contents <= crate::bsp::CONTENTS_WATER;
    let rvrect = if dowarp {
        crate::screen::warp_vrect(render_w, render_h, w.viewsize, intermission, w.sbar_layout, vid.video.hires)
    } else {
        vrect
    };
    let scene = render::Scene {
        colormap: w.colormap.as_deref(),
        time: w.clock,
        light_styles: &light_styles,
        dlights: &active_dlights,
        bmodels: &bmodels,
        external: &external,
        models: &instances,
        sprites: &sprites,
        particles: &parts,
        viewmodel,
        options: render_options(&rvrect, vid),
        ..render::Scene::new(&w.bsp, cam, rvrect.w, rvrect.h, &w.palette)
    };
    // The screen: backtile around the view rectangle (SCR_UpdateScreen's
    // Draw_TileClear) and the view drawn straight into it, or, underwater,
    // into the warp buffer for D_WarpScreen below. The status bar is drawn
    // over it later.
    let backtile = backtile_for(&vrect, render_w, render_h, w.gfx_wad.as_ref());
    let mut img = render::screen_with_backtile(vrect, render_w, render_h, backtile.as_ref());
    let warp_view = if dowarp {
        Some(w.renderer.render(&scene))
    } else {
        w.renderer.render_into(&scene, &mut img);
        None
    };
    lap(Phase::Render3d);
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
    fade_cshifts(&mut w.damage_blend, &mut w.bonus_blend, frametime, w.stepping, &mut w.fade_clock);
    // Underwater sine wobble (D_WarpScreen): the warp buffer's view, stretched
    // over the screen's view rectangle while it wobbles, BEFORE the content
    // tint so the screen ripples, not just darkens.
    if let Some(view) = warp_view {
        w.renderer.warp_into(view, &mut img, vrect, w.clock, vid.video.hires);
    }
    // The 2-D oracle harness paints the view one flat colour (the C oracle's
    // `oracle_blank`), so a shot measures the 2-D layer alone (`set_view_hook`).
    view_hook(&mut img, vrect);
    // V_RenderView: the crosshair over the view, before the 2-D layer — but
    // not over an intermission or finale, which id's GLQuake leaves it off
    // (gl_screen.c's SCR_UpdateScreen draws it only outside them): WinQuake
    // draws it there too, over the level's stats, with nothing to aim at.
    // (`crosshair` is a 2026 setting; Classic draws none.)
    if let Some(cc) = w.conchars.as_ref().filter(|_| w.crosshair && w.intermission == 0) {
        render::draw_crosshair(&mut img, cc, &vrect);
    }
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
    if let Some(cs) = render::powerup_cshift(w.server.vm.ent_float(w.player, w.server.vm.fo().items) as i32) {
        shifts.push(cs);
    }
    // V_UpdatePalette (software view.c): the cshift is a whole-PALETTE shift run
    // LAST in SCR_UpdateScreen, so it tints the ENTIRE screen — 3D view, status bar,
    // centerprint, menu, console — not just the 3D viewport (that 3D-only scope is
    // the GLQuake R_PolyBlend look). We DEFER the shifts: draw the HUD/messages on
    // the untinted frame and return them so the dispatcher applies them to the
    // fully composited frame (after the menu/console overlay too).
    lap(Phase::Post3d);

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
                        let (vm, go) = (&w.server.vm, w.server.vm.go());
                        let gcount = |g: Glb| vm.glob_float(g) as i32;
                        let stats = render::IntermissionStats {
                            // cl.completed_time is an int in the C: whole seconds.
                            completed_time: w.completed_time as i32,
                            secrets: gcount(go.found_secrets),
                            total_secrets: gcount(go.total_secrets),
                            monsters: gcount(go.killed_monsters),
                            total_monsters: gcount(go.total_monsters),
                        };
                        render::draw_intermission_overlay(
                            &mut img,
                            wad,
                            w.pic_complete.as_ref(),
                            w.pic_inter.as_ref(),
                            &stats,
                        );
                    }
                }
                2 => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    w.pic_finale.as_ref(),
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
                // svc_cutscene: the centered text alone, no plaque.
                _ => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    None,
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
            }
        }
    } else if let Some(wad) = w.gfx_wad.as_ref() {
        let (vm, fo, go) = (&w.server.vm, w.server.vm.fo(), w.server.vm.go());
        let stat = |f: Fld| vm.ent_float(w.player, f) as i32;
        // Solo-scoreboard counts come from the QuakeC globals the engine's
        // SV_UpdateStats reads; the level name is worldspawn's `message` (edict 0).
        let gcount = |g: Glb| vm.glob_float(g) as i32;
        let level_name = vm.ent_str(0, fo.message).to_string();
        let hud = render::Hud {
            wad,
            mode: w.server.mode,
            health: stat(fo.health),
            // The active weapon's ammo (W_SetCurrentAmmo keeps `currentammo` in
            // sync with the weapon), not always shells — sbar.c draws currentammo.
            ammo: stat(fo.currentammo),
            armor: stat(fo.armorvalue),
            items: client_items(w),
            weapon: stat(fo.weapon),
            ammo_shells: stat(fo.ammo_shells),
            ammo_nails: stat(fo.ammo_nails),
            ammo_rockets: stat(fo.ammo_rockets),
            ammo_cells: stat(fo.ammo_cells),
            // Sbar_SoloScoreboard shows cl.time — the SERVER clock (epoch 1.0,
            // SV_SpawnServer), not this walk's 0-based clock, matching what the
            // intermission overlay's completed_time latches.
            time: w.server.time(),
            item_gettime: Some(&w.item_gettime),
            monsters: gcount(go.killed_monsters),
            total_monsters: gcount(go.total_monsters),
            secrets: gcount(go.found_secrets),
            total_secrets: gcount(go.total_secrets),
            level_name: &level_name,
            // `+showscores` held (Tab); the dead-player branch (health <= 0)
            // inside draw_hud_into handles the death scoreboard.
            show_scores: km.showscores,
            face_pain: w.server.time() <= w.faceanimtime,
            sb_lines: refdef.sb_lines,
            sbar_layout: w.sbar_layout,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // SCR_DrawPause: the plaque while cl.paused, outside an intermission and
    // whatever key_dest is (the menu draws over it).
    if cl_paused && w.intermission == 0 {
        if let Some(pic) = w.pic_pause.as_ref() {
            render::draw_pause(&mut img, pic);
        }
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
                render::draw_centerprint(&mut img, cc, text);
            }
            let lines = w.notify.visible(w.host_time);
            if !lines.is_empty() {
                render::draw_notify(&mut img, cc, &lines, render::notify_top(w.show_fps));
            }
        }
    }

    // The main-menu overlay is drawn by the `step` dispatcher (the menu lives at
    // the App level now so it can overlay walk OR the attract demo); walk_frame no
    // longer draws it. The deferred cshifts ride out with the frame so the
    // dispatcher tints the whole composited image (HUD + menu + console included).
    lap(Phase::Hud2d);
    ClientFrame { image: img, cshifts: shifts, sound }
}


#[cfg(test)]
mod tests {
    use super::server_items;
    use crate::progs::Progs;
    use crate::server::testutil::{empty_bsp, Builder};
    use crate::server::Server;

    /// A server on a progs with the fields `items` (cell 0) and, if
    /// `with_items2`, `items2` (cell 1), and the global `serverflags`; edict 0
    /// stands in for the player.
    fn server(with_items2: bool) -> Server {
        let mut b = Builder::new();
        b.add_field("items", 2, 0);
        if with_items2 {
            b.add_field("items2", 2, 1);
        }
        b.entityfields = 2;
        b.add_global("serverflags", 2, 40);
        Server::new(empty_bsp(), Progs::parse(&b.build()).expect("progs")).expect("server")
    }

    /// `SV_WriteClientdataToMessage`: id1's progs (no `items2`) get the rune
    /// bits of `serverflags` at 28 and up; a progs that declares `items2` (the
    /// mission packs') gets `items2 << 23` instead and no runes — Hipnotic's
    /// wetsuit (`items2` 2) at bit 24, Rogue's shield (64) at 29.
    #[test]
    fn items2_replaces_the_runes_when_the_progs_declares_it() {
        let mut id1 = server(false);
        id1.vm.ent_set_float(0, "items", 4097.0);
        id1.vm.set_glob_float(id1.vm.go().serverflags, 3.0);
        assert_eq!(server_items(&id1, 0), 4097 | (3 << 28));

        let mut pack = server(true);
        pack.vm.ent_set_float(0, "items", 4097.0);
        pack.vm.set_glob_float(pack.vm.go().serverflags, 3.0);
        assert_eq!(server_items(&pack, 0), 4097, "items2 0: no runes, nothing above");
        pack.vm.ent_set_float(0, "items2", 2.0);
        assert_eq!(server_items(&pack, 0), 4097 | (1 << 24), "Hipnotic's wetsuit");
        pack.vm.ent_set_float(0, "items2", 64.0 + 128.0);
        assert_eq!(server_items(&pack, 0), 4097 | (1 << 29) | (1 << 30), "Rogue's shield and belt");
    }
}
