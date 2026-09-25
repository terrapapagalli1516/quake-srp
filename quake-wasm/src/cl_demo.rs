//! The demo client frame — cl_demo.c playback of a recorded `.dem` stream
//! rendered like live play: per-frame effect replay (svc_particle, temp
//! entities, svc_sound/stopsound, svc_damage, prints — cl_parse.c /
//! cl_tent.c / view.c `V_ParseDamage`) and `step_demo`, the recorded-POV
//! `V_CalcRefdef` + `SCR_UpdateScreen` for one frame.

use quake_rs::particles::ParticleSystem;
use quake_rs::render::{self, Camera, ModelInstance, Viewmodel};
use quake_rs::tent::BeamModel;

use crate::app::DemoPlay;
use crate::bench::{self, Phase};
use crate::cl_tent::spawn_temp_entity;
use crate::host_cmd::IT_INVISIBILITY;
use crate::snd_dma::{
    push_stop_sounds, queue_sounds, update_ambient_channels, Listener, LISTENER,
};
use crate::vid::backtile_for;

/// Spawn the recorded effects of demo frame `idx` into the live particle pool
/// exactly ONCE: a frame rendered across several steps (small `dt`) must not
/// re-spawn its bursts each step. `d.last_spawned_idx` records the most recently
/// spawned frame; this is a no-op when it already equals `idx`.
///
/// Each `svc_particle` burst replays through [`ParticleSystem::spawn_burst`],
/// except the explosion sentinel (`count >= 1024`, the demo parser's mapping of
/// the net `count == 255`) which routes to [`ParticleSystem::spawn_explosion`]
/// for the 1024-particle fiery burst. Each temp entity replays through the same
/// [`spawn_temp_entity`] mapping the live walk uses (explosion / impact / splash).
/// The frame's recorded server `time` is the absolute clock for particle
/// lifetimes (`spawn_*` set `die = now + life`).
fn spawn_demo_frame_effects(d: &mut DemoPlay, idx: usize) {
    if d.last_spawned_idx == idx {
        return; // already spawned this frame's effects; don't double-spawn
    }
    d.last_spawned_idx = idx;
    let Some(frame) = d.demo.frames.get(idx) else { return };
    let now = frame.time;
    // The frame borrows `d.demo`; copy the small effect records out so we can
    // call &mut self spawn methods on `d.particles` without aliasing `d`.
    let bursts = frame.particles.clone();
    let tents = frame.temp_entities.clone();
    let sounds = frame.sounds.clone();
    let stops = frame.stop_sounds.clone();
    let damage = frame.damage.clone();
    let prints = frame.prints.clone();
    let centerprints = frame.centerprints.clone();
    let view_entity_origin = frame.view_entity_origin;
    let view_angles = frame.view_angles;
    for b in &bursts {
        // svc_particle is always R_RunParticleEffect (spawn_burst) in id's
        // CL_ParseParticleEffect — the net count==255 sentinel just means 1024
        // particles (the demo parser already maps it), NOT the rocket
        // R_ParticleExplosion. Route every burst through spawn_burst.
        d.particles
            .spawn_burst(b.org, b.dir, b.color, b.count, now, &mut d.prng);
    }
    // CLIENT-SIDE temp-entity impact sounds (CL_ParseTEnt: tink/ric for
    // spikes, wizard/hit, hknight/hit, r_exp3 for explosions) — the C plays
    // these during demo playback too; they are NOT in the recorded svc_sound
    // stream. Collected here and queued through the same path as the recorded
    // sounds below.
    let mut te_sounds: Vec<quake_rs::server::SoundEvent> = Vec::new();
    for ev in &tents {
        // Beam types refresh the entity's beam slot (CL_ParseBeam) with the
        // frame's recorded server time; step_demo expands the live beams into
        // bolt-model instances every render (CL_UpdateTEnts), exactly like the
        // live walk.
        if let Some(bm) = BeamModel::from_te_type(ev.te_type) {
            d.beams.parse_beam(ev.entity, bm, ev.pos, ev.end, now);
            continue;
        }
        // Reuse the live-walk mapping (explosion/impact/splash) — including
        // its client-side impact sound, exactly like step_walk's te_sounds.
        if let Some(name) = spawn_temp_entity(&mut d.particles, ev, now, &mut d.prng) {
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
    // The RECORDED svc_sound one-shots (CL_ParseStartSoundPacket ->
    // S_StartSound): queue through the SAME spatialized path live play uses.
    // The listener is the recorded camera pose, which step_demo refreshes
    // every frame; the recorded view entity's own sounds (weapon fire, pain
    // grunts) get the full-volume centred treatment via the view_entity key.
    if !sounds.is_empty() {
        queue_sounds(&d.pak, &sounds, d.demo.viewentity as i32);
    }
    if !te_sounds.is_empty() {
        queue_sounds(&d.pak, &te_sounds, d.demo.viewentity as i32);
    }
    // svc_stopsound: hand the (entity, channel) stops to the page, which
    // stop()s its registered source for that key (S_StopSound).
    push_stop_sounds(&stops);
    // svc_damage (V_ParseDamage, view.c): bump the damage cshift and compute
    // the directional view kick from the recorded attack origin.
    for dmg in &damage {
        // count = blood*0.5 + armor*0.5, floored at 10; percent += 3*count,
        // clamped 0..150.
        let count = (dmg.blood as f32 * 0.5 + dmg.armor as f32 * 0.5).max(10.0);
        d.damage_blend = (d.damage_blend + 3.0 * count).clamp(0.0, 150.0);
        d.damage_color = if dmg.armor > dmg.blood {
            [200, 100, 100] // armour absorbed most -> pinkish
        } else if dmg.armor > 0 {
            [220, 50, 50] // some armour -> orange-red
        } else {
            [255, 0, 0] // pure blood -> red
        };
        // from = normalize(from - ent->origin); AngleVectors(ent->angles) with
        // the angles V_CalcRefdef maintains on the view entity: YAW =
        // cl.viewangles[YAW], PITCH = -cl.viewangles[PITCH], ROLL untouched
        // (~0 for the player).
        let delta = [
            dmg.from[0] - view_entity_origin[0],
            dmg.from[1] - view_entity_origin[1],
            dmg.from[2] - view_entity_origin[2],
        ];
        let (from_dir, _len) = quake_rs::math::normalize(delta);
        let (forward, right, _up) =
            quake_rs::math::angle_vectors([-view_angles[0], view_angles[1], 0.0]);
        // v_kickroll 0.6 / v_kickpitch 0.6 / v_kicktime 0.5 (stock cvars).
        d.v_dmg_roll = count * quake_rs::math::dot(from_dir, right) * V_KICKROLL;
        d.v_dmg_pitch = count * quake_rs::math::dot(from_dir, forward) * V_KICKPITCH;
        d.v_dmg_time = V_KICKTIME;
    }
    // svc_print fragments accumulate Con_Print-style (a notify line breaks
    // only on '\n' — pickups arrive as several fragments) with Quake's
    // con_notifytime expiry on the demo's recorded clock; svc_centerprint
    // replaces the current centered message (SCR_CenterPrint, ~2 s).
    for p in &prints {
        d.notify_pending.push_str(p);
    }
    while let Some(nl) = d.notify_pending.find('\n') {
        let line: String = d.notify_pending.drain(..=nl).collect();
        let line = line.trim_end_matches(['\n', '\r']).to_string();
        if !line.trim().is_empty() {
            d.notify.push((line, now + 3.0));
            while d.notify.len() > 4 {
                d.notify.remove(0);
            }
        }
    }
    if let Some(text) = centerprints.into_iter().next_back() {
        d.centerprint = Some((text, now + 2.0));
    }
}

/// `v_kicktime` (view.c, default "0.5"): how long an svc_damage view kick lasts.
const V_KICKTIME: f32 = 0.5;
/// `v_kickroll` (view.c, default "0.6"): roll degrees per damage count*side.
const V_KICKROLL: f32 = 0.6;
/// `v_kickpitch` (view.c, default "0.6"): pitch degrees per damage count*side.
const V_KICKPITCH: f32 = 0.6;

pub(crate) fn step_demo(
    d: &mut DemoPlay,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> (render::Image, [u8; 3], f32) {
    let n = d.demo.frames.len();
    let t0 = d.demo.frames[0].time;
    d.elapsed += dt;
    // Wrap BEFORE advancing: only loop back to frame 0 once we were already
    // sitting on the last frame on a prior step and time has run past it. This
    // defers the reset by one step so frames[n-1] is rendered (displayed for its
    // dt) before we snap back to the start — the previous code reset to 0 the
    // instant `idx` reached n-1, so the final frame was never shown.
    if d.idx + 1 >= n {
        d.idx = 0;
        d.elapsed = 0.0;
        // Looping restarts the recorded effect stream: drop every live particle
        // and beam and forget what was spawned so the replay from frame 0 is
        // identical to the first pass (no stale explosions/bolts carried across
        // the wrap). The per-POV view state resets too: damage flash/kick,
        // notify + centerprint text, and the stair-smoothing accumulator (their
        // expiries live on the recorded clock, which just jumped back to t0).
        d.particles = ParticleSystem::new();
        d.beams.clear();
        d.last_spawned_idx = usize::MAX;
        d.damage_blend = 0.0;
        d.v_dmg_time = 0.0;
        d.centerprint = None;
        d.notify.clear();
        d.notify_pending.clear();
        d.oldz = f32::NAN;
    }
    // Advance to the frame matching the recorded server time. Stop at the last
    // frame (n-1); the wrap above handles looping on the FOLLOWING step. Spawn
    // the recorded effects of EACH frame we newly advance onto (a large dt can
    // step over several frames at once; missing one would drop its explosion).
    while d.idx + 1 < n && (d.demo.frames[d.idx + 1].time - t0) <= d.elapsed {
        d.idx += 1;
        spawn_demo_frame_effects(d, d.idx);
    }
    // Also spawn the landing frame's effects when we first arrive on it without
    // the while-loop running (e.g. the very first step lands on frame 0, or a
    // tiny dt holds us on the same frame the wrap reset us to). `last_spawned_idx`
    // guards against re-spawning while a frame lingers across several steps.
    spawn_demo_frame_effects(d, d.idx);

    // Age the live particle pool one frame under the same gentle gravity the
    // live walk uses (sv_gravity * 0.05 with the default sv_gravity = 800), then
    // retire the expired ones. Guarded against a non-finite/negative dt.
    if dt.is_finite() && dt > 0.0 {
        let now = d.demo.frames[d.idx].time;
        d.particles.advance(dt, now, 800.0 * 0.05);
    }

    let f = &d.demo.frames[d.idx];

    let mut owned: Vec<ModelInstance> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut sprite_insts: Vec<render::SpriteInstance> = Vec::new();
    for e in &f.entities {
        if let Some(Some(mdl)) = d.models.get(e.modelindex) {
            owned.push(ModelInstance {
                mdl,
                origin: e.origin,
                yaw: e.angles[1],
                // Demo entities carry full angles; orient projectiles (pitch/roll)
                // as the recorded stream did (R_AliasSetUpTransform).
                pitch: e.angles[0],
                roll: e.angles[2],
                color: d.colors.get(e.modelindex).copied().unwrap_or([200, 200, 200]),
                // Demo entities carry their current animation frame from the net
                // stream — use it so monsters in the demo are actually posed.
                frame: e.frame.max(0) as usize,
                skinnum: 0,
            });
        } else if let Some(num) = d
            .demo
            .model_precache
            .get(e.modelindex)
            .and_then(|name| name.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            // Brush submodels (doors, platforms, ELEVATORS, buttons) are "*N"
            // precache names with no alias Mdl; they render at the entity origin
            // from world submodel N. The live walk passes these; the demo path used
            // to drop them entirely, so moving level geometry vanished behind the
            // boot-demo menu. `frame` picks the activated (+a..+j) texture cycle.
            bmodels.push(render::BModelInstance {
                model_index: num,
                origin: e.origin,
                frame: e.frame.max(0),
            });
        } else if let Some(Some(spr)) = d.sprites.get(e.modelindex) {
            // Sprite-model entity (the boot demo's s_explod.spr explosion flashes).
            sprite_insts.push(render::SpriteInstance {
                sprite: spr,
                origin: e.origin,
                frame: e.frame.max(0) as usize,
            });
        }
    }
    // CL_UpdateTEnts: expand the recorded lightning beams into bolt-model
    // pieces, exactly like the live walk. The bolt models resolve through the
    // demo's PRECACHE table (the .dem signon lists progs/bolt*.mdl); a beam
    // owned by the recorded view entity tracks its per-frame origin.
    if d.beams.any_live(f.time) {
        d.beams.update(
            f.time,
            d.demo.viewentity as i32,
            f.view_entity_origin,
            &mut d.prng,
            &mut d.beam_scratch,
        );
        for seg in &d.beam_scratch {
            let name = seg.model.model_name();
            let Some(idx) = d.demo.model_precache.iter().position(|n| n == name) else {
                continue; // model not precached (e.g. beam.mdl in shareware)
            };
            if let Some(Some(mdl)) = d.models.get(idx) {
                owned.push(ModelInstance {
                    mdl,
                    origin: seg.origin,
                    yaw: seg.yaw,
                    pitch: seg.pitch,
                    roll: seg.roll,
                    color: d.colors.get(idx).copied().unwrap_or([200, 200, 200]),
                    frame: 0,
                    skinnum: 0,
                });
            }
        }
    }
    // The recorded per-client state (svc_clientdata) drives V_CalcRefdef.
    let client = f.client;
    let cam = if f.intermission != 0 {
        // V_CalcIntermissionRefdef (view.c): a recorded intermission renders
        // with the forced v_idlescale=1 idle sway (V_AddIdle, stock
        // cycle/level cvars) applied LIVE on top of the recorded (QC-placed)
        // camera angles — no bob, no punch, no kick, no stair smoothing.
        Camera {
            pos: f.view_origin,
            yaw: f.view_angles[1] + (f.time * 2.0).sin() * 0.3,
            pitch: -(f.view_angles[0] + (f.time * 1.0).sin() * 0.3),
            roll: f.view_angles[2] + (f.time * 0.5).sin() * 0.1,
            fov_deg: 90.0,
        }
    } else {
        // V_CalcRefdef (view.c) on the RECORDED stream, exactly like the C's
        // demo playback: head-bob from the recorded SU_VELOCITY (visible in
        // id's demo1 — the player runs), stair-step smoothing from the
        // recorded SU_ONGROUND, the strafe/damage/dead view roll
        // (V_CalcViewRoll) and the recorded punchangle added LAST. V_AddIdle
        // is a no-op here (v_idlescale defaults to 0 outside intermission);
        // the 1/32 anti-node-line epsilon is omitted, matching this port's
        // live walk. The listener pose below deliberately stays UNbobbed
        // (audio panning must not jitter with the head-bob), like step_walk.
        let vel = client.velocity;
        let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        let bob = render::view_bob(speed_xy, f.time);
        let mut eye = f.view_origin; // view entity origin + recorded viewheight
        eye[2] += bob;
        // Stair-step smoothing (V_CalcRefdef ~960): the same port as
        // step_walk's, driven by the recorded onground flag + the raw view
        // entity origin z.
        let origin_z = f.view_entity_origin[2];
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        if d.oldz.is_finite() && client.onground && origin_z - d.oldz > 0.0 {
            d.oldz += sdt * 80.0;
            if d.oldz > origin_z {
                d.oldz = origin_z;
            }
            if origin_z - d.oldz > 12.0 {
                d.oldz = origin_z - 12.0;
            }
            eye[2] += d.oldz - origin_z;
        } else {
            d.oldz = origin_z;
        }
        // V_CalcViewRoll: strafe lean from the recorded velocity (the C reads
        // the view entity's angles, which V_CalcRefdef keeps at YAW =
        // viewangles[YAW], PITCH = -viewangles[PITCH]), plus the decaying
        // svc_damage kick; a dead POV (health <= 0) REPLACES the whole roll
        // with the 80-degree dead view (the recorded viewangles[ROLL] and the
        // lean/kick are wiped — the C assigns viewangles[ROLL] = 80). The
        // punchangle adds AFTER, per the C's VectorAdd ordering.
        let basis = [-f.view_angles[0], f.view_angles[1], 0.0];
        let mut roll_angle =
            f.view_angles[2] + quake_rs::server::v_calc_roll(basis, vel);
        let mut dmg_pitch = 0.0;
        if d.v_dmg_time > 0.0 {
            roll_angle += d.v_dmg_time / V_KICKTIME * d.v_dmg_roll;
            dmg_pitch = d.v_dmg_time / V_KICKTIME * d.v_dmg_pitch;
            d.v_dmg_time -= sdt; // v_dmg_time -= host_frametime
        }
        if client.health <= 0 {
            roll_angle = 80.0; // dead view angle (replaces lean + kick + bank)
        }
        Camera {
            pos: eye,
            yaw: f.view_angles[1] + client.punchangle[1],
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(f.view_angles[0] + dmg_pitch + client.punchangle[0]),
            roll: roll_angle + client.punchangle[2],
            fov_deg: 90.0,
        }
    };
    // Sound listener pose + the per-leaf ambient channels follow the demo
    // camera (the C's S_Update runs in demo playback too — the recorded e1m3
    // run drifts past water and open sky, and its placed torch loops pan with
    // the recorded view). Forward/right are the level yaw basis like step_walk.
    {
        let yaw_rad = (f.view_angles[1] as f64).to_radians();
        let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
        LISTENER.with(|l| {
            *l.borrow_mut() = Listener {
                pos: f.view_origin,
                forward: [cy, sy, 0.0],
                right: [sy, -cy, 0.0],
            };
        });
        update_ambient_channels(&d.bsp, f.view_origin, dt);
    }
    // The recorded server time animates the demo's liquids/sky too. The live
    // particle pool (replayed from the recorded svc_particle / temp-entity
    // stream) is passed as (world pos, palette index) so blood/puffs/explosions
    // draw into the scene sharing its z-buffer. Demos carry no dynamic lights
    // here (empty; a deferred LOW).
    let parts: Vec<([f32; 3], u8)> =
        d.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    // The RECORDED svc_lightstyle table drives the world lighting through the
    // same R_AnimateLight 10 Hz logic the live walk uses (lightstyle_scales_at)
    // — the demo's torch flicker matches the recording exactly. A synthetic
    // demo without a table (tests) falls back to the previous seeded default:
    // style 0 = 'm' (264/256, id's steady-world brightness), the rest neutral.
    let demo_styles = if f.lightstyles.is_empty() {
        let mut s = render::NEUTRAL_LIGHTSTYLE_SCALES;
        s[0] = 264.0 / 256.0;
        s
    } else {
        quake_rs::server::lightstyle_scales_at(&f.lightstyles, f.time)
    };
    // The first-person weapon viewmodel: SU_WEAPON is the model PRECACHE index
    // (`view->model = cl.model_precache[cl.stats[STAT_WEAPON]]`, V_CalcRefdef),
    // SU_WEAPONFRAME its animation frame. Hidden exactly like R_DrawViewModel
    // (r_main.c ~606): invisible POV (Ring of Shadows), dead POV, or an
    // intermission (V_CalcIntermissionRefdef sets `view->model = NULL`).
    let hide_gun = f.intermission != 0
        || client.health <= 0
        || client.items & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match d.models.get(client.weapon_model.max(0) as usize) {
            Some(Some(mdl)) => {
                // V_CalcRefdef's gun origin from the recorded velocity's bob.
                let vel = client.velocity;
                let bob = render::view_bob((vel[0] * vel[0] + vel[1] * vel[1]).sqrt(), f.time);
                // CalcGunAngle: the recorded view (with the damage kick's
                // pitch) before the punch, and the recorded roll.
                let angles = render::viewmodel_angles(&cam, client.punchangle, f.view_angles[2]);
                Some(Viewmodel {
                    mdl,
                    frame: client.weaponframe.max(0) as usize,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, d.viewsize),
                    angles,
                })
            }
            _ => None,
        }
    };
    // SCR_CalcRefdef: the same viewsize framing as live play (the C's demo IS
    // the client rendering a recorded stream).
    bench::lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, d.viewsize, f.intermission != 0);
    let vrect = refdef.vrect;
    let mut view = render::render_scene_ext_sprited(&d.bsp, &cam, vrect.w, vrect.h, &d.palette, &owned, &bmodels, &[], viewmodel, f.time, &parts, &[], &demo_styles, d.colormap.as_deref(), &sprite_insts);
    bench::lap(Phase::Render3d);
    // D_WarpScreen: a submerged recorded POV ripples exactly like live play —
    // the warp applies to the 3-D view FIRST; the content tint joins the
    // deferred whole-screen blend below (V_CalcBlend order).
    let eye_contents = quake_rs::world::point_contents(&d.bsp, cam.pos);
    if eye_contents <= quake_rs::bsp::CONTENTS_WATER {
        render::apply_warp(&mut view, f.time);
    }
    let backtile = backtile_for(&vrect, render_w, render_h, d.gfx_wad.as_ref());
    let mut img =
        render::compose_view(view, vrect, render_w, render_h, backtile.as_ref(), &d.palette);
    bench::lap(Phase::Post3d);
    // A recorded intermission/finale frame draws its overlay exactly like the
    // live walk (SCR_UpdateScreen's cl.intermission branches), gated on the game
    // owning the screen (`key_dest == key_game` — i.e. no menu/console up).
    if f.intermission != 0 && !menu_up {
        match f.intermission {
            1 => {
                if let Some(wad) = d.gfx_wad.as_ref() {
                    let stats = render::IntermissionStats {
                        completed_time: f.completed_time as i32,
                        secrets: f.stats.secrets,
                        total_secrets: f.stats.total_secrets,
                        monsters: f.stats.monsters,
                        total_monsters: f.stats.total_monsters,
                    };
                    render::draw_intermission_overlay(
                        &mut img,
                        wad,
                        &d.palette,
                        d.pic_complete.as_ref(),
                        d.pic_inter.as_ref(),
                        &stats,
                    );
                }
            }
            2 => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                d.pic_finale.as_ref(),
                &f.finale_text,
                f.time - f.finale_start,
            ),
            _ => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                None,
                &f.finale_text,
                f.time - f.finale_start,
            ),
        }
    } else if let Some(wad) = d.gfx_wad.as_ref() {
        // Status bar from the RECORDED cl.stats (svc_clientdata) — the C's
        // Sbar_Draw runs identically during demo playback, so the attract loop
        // shows the recorded player's health/ammo/armour/items exactly like
        // live play. Drawn under the menu/console like step_walk's HUD (the C
        // draws the sbar regardless of key_dest; overlays paint on top).
        // draw_hud_into's dead-player branch shows the solo scoreboard when
        // the recorded health hits 0, like Sbar_Draw's scoreboard flip.
        let hud = render::Hud {
            wad,
            palette: &d.palette,
            health: client.health,
            ammo: client.ammo,
            armor: client.armor,
            items: client.items,
            weapon: client.active_weapon,
            ammo_shells: client.shells,
            ammo_nails: client.nails,
            ammo_rockets: client.rockets,
            ammo_cells: client.cells,
            // The recorded server clock (cl.time) drives the weapon-flash
            // cycle + face animation, exactly what sbar.c reads.
            time: f.time,
            monsters: f.stats.monsters,
            total_monsters: f.stats.total_monsters,
            secrets: f.stats.secrets,
            total_secrets: f.stats.total_secrets,
            level_name: &d.demo.level_name,
            show_scores: d.show_scores,
            sb_lines: refdef.sb_lines,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // On-screen messages from the recorded svc_print / svc_centerprint stream,
    // drawn through the same overlays live play uses, with the same key_dest +
    // intermission gating as step_walk. Expiries live on the recorded clock.
    if let Some((_, exp)) = &d.centerprint {
        if f.time >= *exp {
            d.centerprint = None;
        }
    }
    let ftime = f.time;
    d.notify.retain(|(_, exp)| ftime < *exp);
    if !menu_up && f.intermission == 0 {
        if let Some(cc) = d.conchars.as_ref() {
            if let Some((text, _)) = &d.centerprint {
                render::draw_centerprint(&mut img, cc, &d.palette, text);
            }
            if !d.notify.is_empty() {
                let lines: Vec<&str> = d.notify.iter().map(|(t, _)| t.as_str()).collect();
                render::draw_notify(&mut img, cc, &d.palette, &lines);
            }
        }
    }

    // Screen blends (V_CalcBlend order: CONTENTS -> DAMAGE -> POWERUP), all
    // from the RECORDED stream: the eye-contents tint, the svc_damage flash
    // (faded dt*150 per frame like V_UpdatePalette), and the powerup tint from
    // the recorded cl.items. DEFERRED to the dispatcher so it tints the whole
    // composited frame (HUD + menu + console), like the live walk.
    {
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        d.damage_blend = (d.damage_blend - sdt * 150.0).max(0.0);
    }
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if let Some(cs) = render::content_cshift(eye_contents) {
        shifts.push(cs);
    }
    if d.damage_blend > 0.0 {
        shifts.push((d.damage_color, d.damage_blend));
    }
    if let Some(cs) = render::powerup_cshift(client.items) {
        shifts.push(cs);
    }
    let blend = if shifts.is_empty() {
        ([0u8, 0, 0], 0.0f32)
    } else {
        render::combine_cshifts(&shifts)
    };
    bench::lap(Phase::Hud2d);
    (img, blend.0, blend.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::demo::parse_demo;
    use quake_rs::mdl::Mdl;
    use quake_rs::particles::Lcg;
    use quake_rs::server::{SoundEvent, TempEntityEvent};

    use crate::app::{build_demo, pak};
    use crate::snd_dma::{
        poll_sound, set_audio_ready, sound_channel, sound_entity, sound_is_view_entity,
        sound_volume, SND_QUEUE,
    };
    use crate::test_util::*;
    use crate::vid::{DEFAULT_H, DEFAULT_W};

    #[test]
    fn step_demo_shows_the_last_frame_before_looping() {
        // FIX-7: the wrap must be DEFERRED so frames[n-1] is rendered for one
        // step before looping back to frame 0. The old code reset to 0 the
        // instant `idx` reached n-1, so the final frame was never displayed.
        use quake_rs::demo::{Demo, DemoFrame};

        let frame = |t: f32| DemoFrame { time: t, ..Default::default() };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            // map_name() reads model_precache[1]; unused by step_demo's indexing.
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Three frames at t = 0, 1, 2.
            frames: vec![frame(0.0), frame(1.0), frame(2.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);
        let n = d.demo.frames.len();

        // Drive several 1.0s steps and record which frame index is RENDERED
        // (i.e. the value of `idx` chosen by step_demo for that frame).
        let mut shown = Vec::new();
        for _ in 0..5 {
            let _img = step_demo(&mut d, 1.0, false, DEFAULT_W, DEFAULT_H);
            shown.push(d.idx);
        }

        // The last frame (index n-1) must appear in the shown sequence, and it
        // must be displayed BEFORE the wrap-back-to-0 that follows it.
        let last = n - 1;
        let pos = shown
            .iter()
            .position(|&i| i == last)
            .expect("the final frame index must be rendered at least once");
        assert_eq!(
            shown.get(pos + 1).copied(),
            Some(0),
            "after the last frame is shown, the very next step wraps to frame 0; shown={shown:?}"
        );
        // Concretely: 1.0s steps over t={0,1,2} render [1, 2, 0, 1, 2] — frame 2
        // (the last) is shown, then it loops to 0.
        assert_eq!(shown, vec![1, 2, 0, 1, 2], "deferred-wrap playback order");
    }

    #[test]
    fn step_demo_spawns_recorded_effects_into_the_particle_pool() {
        // A frame carrying an svc_particle burst + a TE_EXPLOSION temp entity
        // must fill the live particle pool when playback advances onto it, and
        // must NOT re-spawn while the same frame lingers, and must reset on wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::{te_consts, ParticleBurst, TempEntityEvent};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        // Frame 1 (t=0.05) carries the effects; frames 0 and 2 are empty. Frame
        // times are one ~Quake tick apart so a 0.05s step advances exactly one
        // frame and the explosion's ramp ages by a realistic amount (not all the
        // way through its 8-frame life in a single huge step).
        let effect_frame = DemoFrame {
            time: 0.05,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: vec![ParticleBurst {
                org: [0.0, 0.0, 0.0],
                dir: [0.0, 0.0, 0.0],
                color: 73,
                count: 20,
            }],
            temp_entities: vec![TempEntityEvent {
                te_type: te_consts::TE_EXPLOSION,
                pos: [10.0, 0.0, 0.0],
                end: [10.0, 0.0, 0.0],
                entity: 0,
                color_start: 0,
                color_length: 0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![plain(0.0), effect_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        // Step 0.05s: lands on frame 1 (the effect frame). The burst (20) +
        // explosion (1024) particles populate the pool; after one tick of aging
        // the bulk of the 1024-particle explosion is still alive.
        let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "advanced onto the effect frame");
        let after_first = d.particles.len();
        assert!(
            after_first > 500,
            "the burst + 1024-particle explosion populate the pool (got {after_first})"
        );

        // A tiny step that holds us on frame 1 must NOT re-spawn the explosion
        // (the pool only shrinks as particles age — it never jumps back up).
        let _ = step_demo(&mut d, 0.001, false, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "still on the effect frame");
        assert!(
            d.particles.len() <= after_first,
            "no double-spawn: pool did not grow while the frame lingered"
        );

        // Drive 0.05s steps until playback wraps back to frame 0. After landing
        // on the last frame the very NEXT step wraps (deferred-wrap, as the
        // dedicated test above verifies); the wrap resets the pool, and frame 0
        // carries no effects, so the pool is empty afterwards.
        let mut wrapped = false;
        for _ in 0..6 {
            let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
            if d.idx == 0 {
                wrapped = true;
                break;
            }
        }
        assert!(wrapped, "playback looped back to the first frame within a cycle");
        assert_eq!(d.idx, 0, "looped back to the first frame");
        assert!(
            d.particles.is_empty(),
            "wrap reset the particle pool (no stale explosion across the loop)"
        );
    }

    #[test]
    fn demo_playback_replays_recorded_lightning_beams() {
        // The DEMO path: a synthetic frame carrying a recorded TE_LIGHTNING1
        // must refresh the beam store when playback advances onto it, expand
        // into bolt.mdl pieces on render, and clear on the loop wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::te_consts;

        let plain = |t: f32| DemoFrame {
            time: t,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: Vec::new(),
            temp_entities: Vec::new(),
            ..Default::default()
        };
        let mut bolt_frame = plain(0.05);
        bolt_frame.temp_entities = vec![TempEntityEvent {
            te_type: te_consts::TE_LIGHTNING1,
            pos: [0.0, 0.0, 0.0],
            end: [75.0, 0.0, 0.0],
            entity: 9,
            color_start: 0,
            color_length: 0,
        }];
        // The real bolt model from the embedded pak, at precache index 2 (the
        // .dem signon precaches progs/bolt.mdl; index 1 is the world).
        let bolt_mdl = pak()
            .and_then(|p| p.read_file("progs/bolt.mdl").ok().flatten())
            .and_then(|b| Mdl::parse(&b).ok())
            .expect("progs/bolt.mdl parses from the embedded pak");
        let demo = Demo {
            level_name: "test".into(),
            model_precache: vec![
                String::new(),
                "maps/test.bsp".into(),
                "progs/bolt.mdl".into(),
            ],
            sound_precache: Vec::new(),
            viewentity: 1,
            static_sounds: Vec::new(),
            frames: vec![plain(0.0), bolt_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.models = vec![None, None, Some(bolt_mdl)];
        d.sprites = vec![None, None, None];
        d.colors = vec![[200; 3]; 3];
        d.prng = Lcg::new(1);

        // Advance onto the bolt frame: the recorded beam lands in the store and
        // the render expands it (75 units => 3 pieces at 0/30/60 along +x).
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the bolt frame");
        assert!(d.beams.any_live(0.05), "recorded TE_LIGHTNING1 refreshed a beam");
        assert_eq!(d.beam_scratch.len(), 3, "75 units expand to 3 pieces");
        assert!(d.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt));

        // A lingering step does NOT re-parse (last_spawned_idx guard) but the
        // beam stays live until its 0.2 s endtime.
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert!(d.beams.any_live(0.05));

        // Advance to the LAST frame: at t=0.10 the beam (endtime 0.25) still
        // rides across frames — it is a client effect, not a per-frame one.
        let mut guard = 0;
        while d.idx != 2 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
            guard += 1;
            assert!(guard < 10, "playback reaches the last frame");
        }
        assert!(
            d.beams.any_live(d.demo.frames[2].time),
            "beam still live on the last frame (t=0.10 < endtime 0.25)"
        );
        // The NEXT (tiny) step triggers the deferred loop wrap: back to frame 0
        // with the beam store cleared (no stale bolts carried into the replay;
        // the tiny dt keeps playback ON frame 0, before the bolt re-spawns).
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert_eq!(d.idx, 0, "playback wrapped");
        assert!(!d.beams.any_live(0.0), "the wrap cleared the beam store");
    }

    // =======================================================================

    // Demo parity: the recorded stream drives sound / sbar / viewmodel /
    // lightstyles / damage exactly like live play.
    // =======================================================================

    /// The REAL embedded demo1.dem decodes the full recorded stream the demo
    /// path previously discarded: hundreds of svc_sound one-shots, the
    /// clientdata stats (sbar source), the SU_WEAPON viewmodel index, the
    /// signon lightstyle table, and — via the signon gate — an in-world first
    /// frame (no void-camera intro).
    #[test]
    fn demo1_recorded_stream_carries_sounds_stats_styles_and_viewmodel() {
        let pak = pak().expect("embedded pak");
        let bytes = pak.read_file("demo1.dem").unwrap().expect("demo1.dem in pak");
        let demo = parse_demo(&bytes).expect("demo1 parses");

        // (a) recorded svc_sound events decoded: id's demo1 carries ~595
        // one-shots (gunshots, doors, monster barks). Assert a robust floor.
        let sounds: usize = demo.frames.iter().map(|f| f.sounds.len()).sum();
        assert!(sounds >= 500, "demo1 carries ~595 svc_sound events, got {sounds}");
        // Every event resolved its precache name (S_StartSound's sfx lookup).
        assert!(
            demo.frames.iter().flat_map(|f| &f.sounds).all(|s| !s.sample.is_empty()),
            "every recorded sound resolves a precache name"
        );

        // (d/e) clientdata: the sbar stats are present from the FIRST frame.
        let f0 = &demo.frames[0];
        assert_eq!(f0.client.health, 100, "fresh recorded player");
        assert_eq!(f0.client.ammo, 25);
        assert_eq!(f0.client.shells, 25);
        assert_eq!(f0.client.active_weapon, 1, "IT_SHOTGUN");
        assert_ne!(f0.client.items, 0, "recorded cl.items bits present");

        // (f) viewmodel: STAT_WEAPON resolves through the demo's precache to
        // the shotgun viewmodel.
        assert_eq!(
            demo.model_precache
                .get(f0.client.weapon_model.max(0) as usize)
                .map(|s| s.as_str()),
            Some("progs/v_shot.mdl"),
            "SU_WEAPON -> model_precache -> v_shot.mdl"
        );

        // (c) the recorded lightstyle table: style 0 = 'm' (the steady world)
        // plus the torch-flicker set from the signon.
        assert_eq!(f0.lightstyles.first().map(|s| s.as_str()), Some("m"));
        let nonempty = f0.lightstyles.iter().filter(|s| !s.is_empty()).count();
        assert!(nonempty >= 10, "signon carries the style table, got {nonempty}");

        // (i) no void-camera intro: the signon gate makes frame 0 in-world.
        assert!(f0.entities.len() > 10, "frame 0 renders the level's entities");
        assert_ne!(f0.view_origin, [0.0; 3], "frame 0 camera is in-world");

        // (j) the recorded SU_VELOCITY drives V_CalcBob: the run reaches real
        // ground speed (id's demo1 visibly bobs).
        let maxv = demo
            .frames
            .iter()
            .map(|f| (f.client.velocity[0].powi(2) + f.client.velocity[1].powi(2)).sqrt())
            .fold(0.0f32, f32::max);
        assert!(maxv > 200.0, "recorded velocity shows the player running ({maxv})");
    }

    /// (b) svc_stopsound census: id's shipped demos never send it — the
    /// (entity, channel) stop registry is protocol completeness, exercised by
    /// the synthetic decode test in quake-rs. If a future demo carries stops,
    /// the page's keyed-source registry honours them.
    #[test]
    fn id_demos_never_send_stopsound() {
        let pak = pak().expect("embedded pak");
        for name in ["demo1.dem", "demo2.dem", "demo3.dem"] {
            let bytes = pak.read_file(name).unwrap().expect("demo in pak");
            let demo = parse_demo(&bytes).expect("demo parses");
            let stops: usize = demo.frames.iter().map(|f| f.stop_sounds.len()).sum();
            assert_eq!(stops, 0, "{name} sends no svc_stopsound");
        }
    }

    /// A recorded svc_sound event queues through the SAME `queue_sounds` path
    /// live play uses — once per frame advance (the spawn guard), carrying its
    /// (entity, channel) override key for the page registry.
    #[test]
    fn step_demo_queues_recorded_sounds_through_the_live_path() {
        use quake_rs::demo::{Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let sound_frame = DemoFrame {
            time: 0.05,
            sounds: vec![SoundEvent {
                entity: 5,
                channel: 2,
                sound_index: 1,
                sample: "doors/x.wav".to_string(),
                origin: [64.0, 0.0, 0.0],
                volume: 0.5,
                attenuation: 1.0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 1,
            frames: vec![plain(0.0), sound_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[("sound/doors/x.wav", b"WAVE")]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        reset_queue(); // clears SND_QUEUE + marks audio ready
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the sound frame");
        assert_eq!(
            SND_QUEUE.with(|q| q.borrow().len()),
            1,
            "the recorded svc_sound queued exactly once"
        );
        // Lingering on the same frame must not re-queue it.
        let _ = step_demo(&mut d, 0.0001, false, 160, 100);
        assert_eq!(SND_QUEUE.with(|q| q.borrow().len()), 1, "no re-queue while lingering");

        // The pop carries the spatial params + the (entity, channel) key.
        let len = poll_sound();
        assert!(len > 0, "WAV bytes loaded from the pak");
        assert_eq!(sound_volume(), 0.5);
        assert_eq!(sound_entity(), 5, "override key entity");
        assert_eq!(sound_channel(), 2, "override key channel");
        assert_eq!(
            sound_is_view_entity(),
            0,
            "entity 5 is not the recorded view entity (1)"
        );
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0);
    }

    /// A recorded svc_damage drives the SAME flash + view-kick math live play
    /// uses (V_ParseDamage): the deferred blend returned by step_demo carries
    /// the red cshift, and the kick state arms + decays.
    #[test]
    fn step_demo_damage_event_drives_flash_and_kick() {
        use quake_rs::demo::{DamageEvent, Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let dmg_frame = DemoFrame {
            time: 0.05,
            // Attack from straight ahead (+x of a yaw-0 view at the origin).
            damage: vec![DamageEvent { armor: 0, blood: 20, from: [128.0, 0.0, 0.0] }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Trailing frames keep the fade-out steps below from wrapping the
            // loop (a wrap re-spawns the damage frame's events).
            frames: vec![plain(0.0), dmg_frame, plain(0.10), plain(1.0), plain(2.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.damage_color = [0, 0, 0];
        d.prng = Lcg::new(1);

        let (_img, color, alpha) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the damage frame");
        // count = max(10, blood*0.5) = 10 -> percent 30, faded by 0.05*150 =
        // 7.5 within the same step (V_UpdatePalette) -> 22.5.
        assert!(
            (d.damage_blend - 22.5).abs() < 1e-3,
            "V_ParseDamage percent 3*count then dt*150 fade, got {}",
            d.damage_blend
        );
        assert_eq!(d.damage_color, [255, 0, 0], "pure-blood red tint");
        assert_eq!(color, [255, 0, 0], "the deferred blend carries the flash");
        assert!(alpha > 0.0, "non-zero blend returned to the dispatcher");
        // The directional kick armed (forward hit -> pitch kick, no roll) and
        // already decayed one step (v_dmg_time -= host_frametime).
        assert!(
            (d.v_dmg_time - (V_KICKTIME - 0.05)).abs() < 1e-3,
            "kick timer armed then decayed by dt"
        );
        assert!(d.v_dmg_roll.abs() < 1e-3, "head-on hit has no roll component");
        assert!(
            (d.v_dmg_pitch - 10.0 * V_KICKPITCH).abs() < 1e-3,
            "pitch kick = count * dot(from, forward) * v_kickpitch"
        );

        // The flash fades out over the following steps and the blend clears.
        for _ in 0..4 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
        }
        assert_eq!(d.damage_blend, 0.0, "flash fully faded");
    }

    /// The real boot demo draws the recorded status bar (sbar pixels differ
    /// from the bare scene) and resolves the recorded viewmodel + lightstyles.
    #[test]
    fn build_demo_resolves_viewmodel_hud_and_recorded_styles() {
        let mut d = build_demo().expect("the embedded demo boots");

        // The recorded SU_WEAPON viewmodel parsed (v_shot.mdl).
        let f0 = &d.demo.frames[0];
        let wm = f0.client.weapon_model.max(0) as usize;
        assert!(
            matches!(d.models.get(wm), Some(Some(_))),
            "the recorded viewmodel's Mdl parsed from the pak"
        );
        // The recorded style table reaches the renderer's scale law: style 0
        // is the steady 'm' world (264/256, what the seeded default used to
        // hardcode) and the flicker styles are present.
        let scales = quake_rs::server::lightstyle_scales_at(&f0.lightstyles, f0.time);
        assert!((scales[0] - 264.0 / 256.0).abs() < 1e-6, "style 0 'm'");
        assert!(d.gfx_wad.is_some(), "sbar pics available for the demo HUD");

        // Status bar A/B: one step with the wad, then re-render the SAME frame
        // without it (dt == 0 holds the frame) — the sbar region must differ.
        let (with_hud, _, _) = step_demo(&mut d, 0.016, false, 320, 200);
        d.gfx_wad = None;
        let (without, _, _) = step_demo(&mut d, 0.0, false, 320, 200);
        assert_eq!(with_hud.rgb.len(), without.rgb.len());
        // Quake's sbar is the bottom 24 rows of the 320x200 virtual screen.
        let bar_rows = 24usize;
        let diff = (0..320 * bar_rows)
            .filter(|i| {
                let a = with_hud.rgb[(200 - bar_rows) * 320 + i];
                let b = without.rgb[(200 - bar_rows) * 320 + i];
                a != b
            })
            .count();
        assert!(diff > 500, "the drawn sbar changes the bar region ({diff} px)");
    }

    /// The loop wrap is seam-clean on the REAL demo: fast-forward to the last
    /// frame, take one more step, and the playback lands back on frame 0 —
    /// which, thanks to the parser's signon gate, is an IN-WORLD frame (the
    /// old stream emitted ~1.2 s of void-camera signon frames here).
    #[test]
    fn demo_loop_wrap_lands_on_the_in_world_first_frame() {
        let mut d = build_demo().expect("the embedded demo boots");
        let n = d.demo.frames.len();
        let _ = step_demo(&mut d, 1.0e6, false, 160, 100);
        assert_eq!(d.idx, n - 1, "fast-forwarded to the last frame");
        let (img, _, _) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 0, "the wrap landed back on frame 0");
        assert!(
            !d.demo.frames[0].entities.is_empty(),
            "frame 0 is the post-signon in-world frame"
        );
        let lit = img
            .rgb
            .iter()
            .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            .count();
        assert!(
            lit * 2 > img.rgb.len(),
            "the post-wrap frame renders a real scene ({lit}/{} lit)",
            img.rgb.len()
        );
    }
}
