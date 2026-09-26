//! `quaketool playtest <pak> <map.bsp> [out.ppm]` — a player on a real map,
//! end to end through the server: spawned with id's loadout, walked forward
//! for 4 s, every door opened through its `use`, then set in front of the
//! nearest monster to watch its AI wake and to fire on it (the particles,
//! temp entities, dynamic lights and sounds tallied), and last its point of
//! view rendered with the gun and the status bar.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::dlight::DynamicLights;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::particles::{Lcg, ParticleSystem};
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera};
use quake_rs::server::{Server, TempEntityEvent, UserCmd};
use quake_rs::wad::Wad2;

use crate::entities::player_start;
use crate::render::color_for_name;
use crate::{CmdResult, Out};

/// The explosion sound a rocket/grenade/tarbaby temp entity plays (the C
/// `cl_sfx_r_exp3` = `weapons/r_exp3.wav`).
const TE_EXPLOSION_SOUND: &str = "weapons/r_exp3.wav";

/// Realise one decoded [`TempEntityEvent`] into `particles`, porting the
/// effect-mapping half of `CL_ParseTEnt`:
///
/// * explosion types (`TE_EXPLOSION`=3, `TE_TAREXPLOSION`=4, `TE_EXPLOSION2`=12)
///   spawn a 1024-particle [`ParticleSystem::spawn_explosion`] and return the
///   `weapons/r_exp3.wav` sound name to play;
/// * impact types spawn a `R_RunParticleEffect`-style burst (the existing
///   [`ParticleSystem::spawn_burst`]) with the matching colour/count:
///   `TE_SPIKE`=0 -> (0,10), `TE_SUPERSPIKE`=1 / `TE_GUNSHOT`=2 -> (0,20),
///   `TE_WIZSPIKE`=7 -> (20,30), `TE_KNIGHTSPIKE`=8 -> (226,20);
/// * splashes (`TE_LAVASPLASH`=10, `TE_TELEPORT`=11) get a small upward burst;
/// * beams (`TE_LIGHTNING1/2/3`=5/6/9, `TE_BEAM`=13) are skipped (consumed only).
///
/// Returns `Some(sound_name)` for the types that play a sound, else `None`.
fn spawn_temp_entity(
    particles: &mut ParticleSystem,
    ev: &TempEntityEvent,
    now: f32,
    rng: &mut Lcg,
) -> Option<&'static str> {
    use quake_rs::server::te_consts::*;
    match ev.te_type {
        TE_EXPLOSION | TE_TAREXPLOSION | TE_EXPLOSION2 => {
            particles.spawn_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        TE_SPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 10, now, rng);
            None
        }
        TE_SUPERSPIKE | TE_GUNSHOT => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 20, now, rng);
            None
        }
        TE_WIZSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 20, 30, now, rng);
            None
        }
        TE_KNIGHTSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 226, 20, now, rng);
            None
        }
        TE_LAVASPLASH | TE_TELEPORT => {
            // Approximate the splash as a small upward burst (dir = +Z).
            particles.spawn_burst(ev.pos, [0.0, 0.0, 1.0], 232, 20, now, rng);
            None
        }
        // Beam/lightning types carry no effect here.
        _ => None,
    }
}

/// Spawn a real player on a map, report its loadout, walk it forward, and render
/// its point of view — the first-person gameplay milestone (#3).
pub fn cmd_playtest(pak_path: &str, map_name: &str, out: Option<&str>) -> CmdResult {
    let pak = Pak::open(pak_path)?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_render = Bsp::parse(&read(map_name)?)?;
    let bsp_sim = Bsp::parse(&read(map_name)?)?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?).ok_or("bad palette")?;
    let progs = Progs::parse(&read("progs.dat")?)?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities()?;
    let player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    // Live engine particles (the `particle()` builtin's effect) for this
    // playtest. Bursts fired by the QuakeC during the frame loops below are
    // drained, spawned into this pool with a deterministic RNG + the current
    // game time, aged under gravity, and finally drawn into the POV render so an
    // explosion/gun-impact shows up as coloured points occluded by walls.
    let mut particles = ParticleSystem::new();
    let mut prng = Lcg::new(0x1234_5678);
    // sv_gravity (800) * the R_DrawParticles particle factor (0.05) as an
    // acceleration; ParticleSystem::advance multiplies by dt itself.
    const PARTICLE_GRAVITY: f32 = 800.0 * 0.05;

    let mut o = String::new();
    let _ = writeln!(o, "playtest {map_name}: {} entities spawned; player = edict {player}", rep.spawned);
    let _ = writeln!(
        o,
        "  loadout after PutClientInServer:  health={}  items={:#x}  weapon={}  shells={}",
        server.player_health(),
        server.vm.ent_get_float(player, "items") as i64,
        server.vm.ent_get_float(player, "weapon") as i64,
        server.vm.ent_get_float(player, "ammo_shells") as i64
    );
    let (eye0, ang) = server.player_view();
    let _ = writeln!(o, "  spawn eye {eye0:?}  angles {ang:?}");

    // Walk forward along the spawn yaw for a few seconds of game time. The spawn
    // angle lives on info_player_start in the map entities, so read it from the
    // BSP rather than the (zero) player angle.
    let spawn_yaw = player_start(&bsp_render.entities).map(|(_, a)| a).unwrap_or(ang[1]);
    let cmd = UserCmd { forwardmove: 400.0, yaw: spawn_yaw, ..Default::default() };
    let start = server.vm.ent_get_vector(player, "origin");
    let mut thinks = 0usize;
    for _ in 0..40 {
        let fr = server.client_frame_f64(&cmd, 0.1).map_err(|e| format!("client_frame: {e}"))?;
        thinks += fr.thinks_fired;
    }
    let end = server.vm.ent_get_vector(player, "origin");
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let _ = writeln!(
        o,
        "  walked 40 frames (4.0s): moved {:.0} units, eye now {:?}, health {}",
        (dx * dx + dy * dy).sqrt(),
        server.player_view().0,
        server.player_health()
    );
    let _ = writeln!(o, "  {thinks} entity/monster thinks fired during play");

    // --- doors: did any func_door (MOVETYPE_PUSH) physically move? ---
    {
        // Record every door's origin, then trigger each by calling its `use`
        // function (the QuakeC door_use that buttons/triggers invoke), tick a
        // couple seconds, and report which ones moved. This proves the pusher
        // physics carry the bmodel, independent of whether the player reached
        // the door's specific trigger field.
        let mut doors: Vec<(i32, [f32; 3])> = Vec::new();
        for e in 0..server.vm.num_edicts() {
            let ent = e as i32;
            if server.vm.is_free_edict(e as i32) {
                continue;
            }
            // func_door's spawn reassigns classname to "door"; movetype PUSH (7).
            if server.vm.ent_get_string(ent, "classname") == "door" {
                doors.push((ent, server.vm.ent_get_vector(ent, "origin")));
            }
        }
        let _ = writeln!(o, "\n  {} doors (func_door, classname \"door\", MOVETYPE_PUSH)", doors.len());
        // Fire each door's `use` (self=door, other=player) to open it.
        for &(d, _) in &doors {
            let usefn = server.vm.ent_get_int(d, "use");
            if usefn > 0 {
                server.vm.gset_int("self", d);
                server.vm.gset_int("other", player);
                let _ = server.vm.execute(usefn as usize);
            }
        }
        // Tick ~2s so the doors slide and reach their open state.
        let still = UserCmd { yaw: spawn_yaw, ..Default::default() };
        for _ in 0..20 {
            let _ = server.client_frame_f64(&still, 0.1);
        }
        let mut moved = 0;
        let mut max_disp = 0.0f32;
        for &(d, o0) in &doors {
            if server.vm.is_free_edict(d) {
                continue;
            }
            let o1 = server.vm.ent_get_vector(d, "origin");
            let disp = ((o1[0]-o0[0]).powi(2) + (o1[1]-o0[1]).powi(2) + (o1[2]-o0[2]).powi(2)).sqrt();
            if disp > 1.0 {
                moved += 1;
                max_disp = max_disp.max(disp);
            }
        }
        let _ = writeln!(o, "  after `use` + 2s tick: {moved} doors moved (max displacement {max_disp:.0} units)");
        for s in server.drain_sounds() {
            if s.sample.contains("door") {
                let _ = writeln!(o, "    door sound: {}", s.sample);
                break;
            }
        }
    }

    // --- combat: aim at the nearest monster and pull the trigger ---
    let pe = server.player_view().0;
    let mut nearest: Option<(i32, f32, [f32; 3])> = None;
    for e in 0..server.vm.num_edicts() {
        let ent = e as i32;
        if server.vm.is_free_edict(e as i32) {
            continue;
        }
        if !server.vm.ent_get_string(ent, "classname").starts_with("monster") {
            continue;
        }
        let mo = server.vm.ent_get_vector(ent, "origin");
        let d2 = (mo[0] - pe[0]).powi(2) + (mo[1] - pe[1]).powi(2) + (mo[2] - pe[2]).powi(2);
        if nearest.map_or(true, |(_, bd, _)| d2 < bd) {
            nearest = Some((ent, d2, mo));
        }
    }
    // Snapshot of the particle pool at the frame where the most particles are
    // alive during combat — used for the POV action shot below (declared out
    // here so it outlives the combat block).
    let mut peak_parts: Vec<([f32; 3], u8)> = Vec::new();
    // Dynamic lights (explosions, muzzle flashes, EF_* lights). Driven each
    // combat frame from the drained temp entities + entity_dlights, decayed, and
    // snapshotted at peak so the POV render below lights up the walls.
    let mut dlights = DynamicLights::new();
    let mut peak_dlights: Vec<quake_rs::dlight::DynamicLight> = Vec::new();
    let mut max_active_dlights = 0usize;
    if let Some((mon, d2, mo)) = nearest {
        let mname = server.vm.ent_get_string(mon, "classname");
        let hp_before = server.vm.ent_get_float(mon, "health");
        // Teleport the player ~80 units in front of the monster with clear line
        // of sight, so the shot demonstrably connects (the spawn-walk leaves the
        // player far down the hall behind geometry). Point-blank, same height.
        let approach = [mo[0] - 80.0, mo[1], mo[2] + 24.0];
        server.vm.ent_set_vector(player, "origin", approach);
        let pe = server.player_view().0;
        // Aim the player at the monster (yaw + pitch toward its centre).
        let dir = [mo[0] - pe[0], mo[1] - pe[1], mo[2] - pe[2]];
        let yaw = dir[1].atan2(dir[0]).to_degrees();
        let horiz = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
        let pitch = -dir[2].atan2(horiz).to_degrees(); // QuakeC pitch is +down
        let _ = writeln!(
            o,
            "\n  nearest monster: {mname} (edict {mon}) at {:.0} units, health {hp_before}",
            d2.sqrt()
        );
        // Diagnostic: trace a bullet from the eye toward the monster centre and
        // report what the engine's own collision says it hits (the monster, the
        // world, or nothing) — this distinguishes an aim/LOS miss from a damage bug.
        {
            let aim = [pe[0] + dir[0] * 4.0, pe[1] + dir[1] * 4.0, pe[2] + dir[2] * 4.0];
            let tr = quake_rs::server::sv_move(&mut server.vm, pe, aim, [0.0; 3], [0.0; 3], player, false, false);
            let hit = if tr.ent == mon { format!("the monster (edict {mon}) ✓") }
                      else if tr.ent == 0 { "the world (wall) — no LOS".into() }
                      else if tr.ent < 0 { "nothing (clear)".into() }
                      else { format!("another edict {}", tr.ent) };
            let _ = writeln!(o, "  eye {pe:?} -> bullet trace hits {hit} at fraction {:.2}", tr.fraction);
        }
        // AI PROBE: stand still in front of the monster (no firing) and watch
        // whether it acquires the player as its enemy and changes think frames —
        // i.e. whether FindTarget + ai_stand->ai_run transitions fire.
        {
            let still = UserCmd { yaw, pitch, ..Default::default() };
            let _ = writeln!(o, "  AI probe (10 frames, player standing in view):");
            for f in 0..10 {
                server.client_frame_f64(&still, 0.1).map_err(|e| format!("probe: {e}"))?;
                if !server.vm.is_free_edict(mon) {
                    let enemy = server.vm.ent_get_int(mon, "enemy");
                    let frame = server.vm.ent_get_float(mon, "frame");
                    let nextthink = server.vm.ent_get_float(mon, "nextthink");
                    let estate = server.vm.ent_get_float(mon, "enemy"); // raw
                    let _ = estate;
                    if f == 0 || f == 4 || f == 9 {
                        let _ = writeln!(o, "    f{f}: enemy={enemy} frame={frame} nextthink={nextthink:.2}");
                    }
                }
            }
        }
        // Hold attack (buttons bit 0) for ~1.5s of game time; collect sounds.
        let fire = UserCmd { yaw, pitch, buttons: 1, ..Default::default() };
        let mut sounds: Vec<String> = Vec::new();
        let mut total_bursts = 0usize;
        let mut total_burst_particles = 0i64;
        // Temp-entity tallies (rocket/grenade explosions, gunshots, spikes) the
        // QuakeC fires via the Write* network builtins, decoded into events.
        let mut te_total = 0usize;
        let mut te_explosions = 0usize;
        let mut te_gunshots = 0usize;
        for _ in 0..15 {
            server.client_frame_f64(&fire, 0.1).map_err(|e| format!("fire frame: {e}"))?;
            for s in server.drain_sounds() {
                sounds.push(s.sample);
            }
            // Realise any particle() bursts the QuakeC fired (gun impacts, blood,
            // gibs), age the pool, and retire the expired ones — exactly the
            // per-frame cycle the renderer front-end runs.
            let now = server.time();
            for b in server.drain_particles() {
                total_bursts += 1;
                total_burst_particles += b.count.max(0) as i64;
                particles.spawn_burst(b.org, b.dir, b.color, b.count, now, &mut prng);
            }
            // Realise the temp entities (explosions, wall impacts) the QuakeC
            // fired via the Write* builtins. Explosions also queue their sound
            // AND spawn a decaying dynamic light (CL_ParseTEnt: radius 350, die
            // now+0.5, decay 300, minlight 0, key 0 -> a fresh slot each one).
            for ev in server.drain_temp_entities() {
                te_total += 1;
                use quake_rs::server::te_consts::*;
                match ev.te_type {
                    TE_EXPLOSION | TE_TAREXPLOSION | TE_EXPLOSION2 => {
                        te_explosions += 1;
                        dlights.alloc(0, ev.pos, 350.0, now + 0.5, 300.0, 0.0, now);
                    }
                    TE_GUNSHOT => te_gunshots += 1,
                    _ => {}
                }
                if let Some(snd) = spawn_temp_entity(&mut particles, &ev, now, &mut prng) {
                    sounds.push(snd.to_string());
                }
            }
            // Entity light effects (EF_MUZZLEFLASH / BRIGHTLIGHT / DIMLIGHT) from
            // the in-use edicts; add the deterministic rand()&31 radius jitter
            // here (entity_dlights keeps the base radius so the query is pure).
            for ed in server.entity_dlights() {
                let jitter = (prng.next_range(32)) as f32;
                dlights.alloc(
                    ed.key,
                    ed.origin,
                    ed.radius_base + jitter,
                    now + ed.life,
                    0.0,
                    ed.minlight,
                    now,
                );
            }
            particles.advance(0.1, now, PARTICLE_GRAVITY);
            // Decay + retire dynamic lights, then track the peak set for the POV.
            dlights.advance(0.1, now);
            let active = dlights.active();
            if active.len() > max_active_dlights {
                max_active_dlights = active.len();
            }
            if !active.is_empty() && active.len() >= peak_dlights.len() {
                peak_dlights = active;
            }
            if particles.len() > peak_parts.len() {
                peak_parts =
                    particles.particles().iter().map(|p| (p.origin, p.color)).collect();
            }
        }
        let _ = writeln!(
            o,
            "    particle() bursts during combat: {total_bursts} ({total_burst_particles} points); live at end: {}",
            particles.len()
        );
        let _ = writeln!(
            o,
            "    temp entities: {te_total} ({te_explosions} explosions, {te_gunshots} gunshots)"
        );
        let _ = writeln!(
            o,
            "    dynamic lights: peak {max_active_dlights} active during combat (explosions + EF_* muzzle/bright/dim lights)"
        );
        let hp_after = server.vm.ent_get_float(mon, "health");
        let alive = !server.vm.is_free_edict(mon);
        let (b0, weapon, shells) = server.player_attack_state();
        let _ = writeln!(
            o,
            "  fired 15 frames (attack={b0}, weapon={weapon}, shells {} -> {}):",
            25, shells as i64
        );
        let _ = writeln!(
            o,
            "    monster health {hp_before} -> {hp_after}{}",
            if !alive { " (REMOVED — killed)" } else { "" }
        );
        if sounds.is_empty() {
            let _ = writeln!(o, "    (no sound events)");
        } else {
            let _ = writeln!(o, "    sound events: {}", sounds.join(", "));
        }
    } else {
        let _ = writeln!(o, "\n  (no monster in range to attack)");
    }

    // Render the player's POV (world + spawned models).
    if let Some(path) = out {
        let mut model_cache: std::collections::HashMap<String, Option<Mdl>> = std::collections::HashMap::new();
        let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
        let mut bmodels: Vec<render::BModelInstance> = Vec::new();
        // External brush-model item boxes (maps/b_*.bsp): each item's box bsp is
        // parsed once (cached by name) and stood at the entity origin. Owned here
        // so the borrowing `ExternalBModel` list can be built after the loop.
        let mut ext_cache: std::collections::HashMap<String, Option<Bsp>> = std::collections::HashMap::new();
        let mut ext_owned: Vec<(Bsp, [f32; 3])> = Vec::new();
        for e in 0..server.vm.num_edicts() {
            if server.vm.is_free_edict(e as i32) || e as i32 == player {
                continue;
            }
            let ent = e as i32;
            // Only render entities with a real modelindex (setmodel ran). An edict
            // that early-returns before setmodel (e.g. a passable func_episodegate)
            // keeps its raw "*N" map key but no modelindex -> invisible in Quake.
            if server.vm.ent_get_float(ent, "modelindex") == 0.0 {
                continue;
            }
            let m = server.vm.ent_get_string(ent, "model");
            // Brush submodels (doors/plats/buttons) draw at the entity origin.
            if let Some(num) = m.strip_prefix('*') {
                if let Ok(idx) = num.parse::<usize>() {
                    let origin = server.vm.ent_get_vector(ent, "origin");
                    bmodels.push(render::BModelInstance { model_index: idx, origin, frame: server.vm.ent_get_float(ent, "frame") as i32 });
                }
                continue;
            }
            // External brush-model item box: a standalone b_*.bsp the item set as
            // its model (explosive box, ammo/health boxes), never the world map.
            if m.ends_with(".bsp") {
                if m != map_name {
                    if !ext_cache.contains_key(&m) {
                        ext_cache.insert(m.clone(), pak.read_file(&m).ok().flatten().and_then(|b| Bsp::parse(&b).ok()));
                    }
                    if let Some(Some(bsp)) = ext_cache.get(&m) {
                        let origin = server.vm.ent_get_vector(ent, "origin");
                        ext_owned.push((bsp.clone(), origin));
                    }
                }
                continue;
            }
            if !m.ends_with(".mdl") {
                continue;
            }
            if !model_cache.contains_key(&m) {
                model_cache.insert(m.clone(), pak.read_file(&m).ok().flatten().and_then(|b| Mdl::parse(&b).ok()));
            }
            if let Some(Some(mdl)) = model_cache.get(&m) {
                owned.push((
                    mdl.clone(),
                    server.vm.ent_get_vector(ent, "origin"),
                    server.vm.ent_get_vector(ent, "angles")[1],
                    color_for_name(&m),
                ));
            }
        }
        let inst: Vec<render::ModelInstance> = owned
            .iter()
            .map(|(mdl, origin, yaw, color)| render::ModelInstance { mdl, origin: *origin, yaw: *yaw, pitch: 0.0, roll: 0.0, color: *color, frame: 0, skinnum: 0 })
            .collect();
        let external: Vec<render::ExternalBModel> = ext_owned
            .iter()
            .map(|(bsp, origin)| render::ExternalBModel { bsp, origin: *origin })
            .collect();

        // The first-person weapon viewmodel: the player edict's `weaponmodel`
        // (e.g. "progs/v_shot.mdl") posed at its `weaponframe`. Loaded from the
        // pak like any other MDL and cached. Drawn anchored to the camera.
        let weapon_name = server.vm.ent_get_string(player, "weaponmodel");
        let weapon_frame = server.vm.ent_get_float(player, "weaponframe").max(0.0) as usize;
        let weapon_mdl: Option<Mdl> = if weapon_name.ends_with(".mdl") {
            pak.read_file(&weapon_name).ok().flatten().and_then(|b| Mdl::parse(&b).ok())
        } else {
            None
        };

        let (eye, a) = server.player_view();
        let cam = Camera { pos: eye, yaw: a[1], pitch: -a[0], roll: 0.0, fov_deg: 90.0 };
        let (vid_w, vid_h) = (640, 400);
        let (viewsize, refdef) = pov_screen(vid_w, vid_h);
        let vrect = refdef.vrect;
        let viewmodel = weapon_mdl
            .as_ref()
            .map(|mdl| render::Viewmodel {
                mdl,
                frame: weapon_frame,
                // No bob in this still.
                origin_ofs: render::viewmodel_origin_ofs([cam.pitch, cam.yaw, 0.0], 0.0, viewsize),
                angles: [cam.pitch, cam.yaw, 0.0],
            });
        // The live particles as (world pos, palette index) for the renderer; they
        // share the scene z-buffer so any behind a wall are hidden. Use the
        // peak-combat snapshot so the action shot actually shows the blood burst
        // (the end-of-combat pool is empty — the monster is dead by then).
        let parts: Vec<([f32; 3], u8)> = if peak_parts.is_empty() {
            particles.particles().iter().map(|p| (p.origin, p.color)).collect()
        } else {
            peak_parts.clone()
        };
        // The peak-combat dynamic lights so the action shot lights up the walls
        // near explosions / muzzle flashes (the end-of-combat pool is empty).
        // Pass the server clock so liquids warp and sky scrolls in the POV shot,
        // and the animated light-style scales so torches flicker and lights pulse.
        let light_styles = server.lightstyle_scales(server.time());
        let colormap = read("gfx/colormap.lmp").ok();
        let scene = render::Scene {
            colormap: colormap.as_deref(),
            time: server.time(),
            light_styles: &light_styles,
            dlights: &peak_dlights,
            bmodels: &bmodels,
            external: &external,
            models: &inst,
            particles: &parts,
            viewmodel,
            ..render::Scene::new(&bsp_render, cam, vrect.w, vrect.h, &palette)
        };
        let view = render::Renderer::new().render(&scene);
        let gfx_wad = read("gfx.wad").ok().and_then(|b| Wad2::parse(b).ok());
        let backtile = gfx_wad.as_ref().and_then(|w| w.qpic("backtile").ok());
        let mut img = render::compose_view(view, vrect, vid_w, vid_h, backtile.as_ref(), 1);

        // Status bar (HUD) overlay: build a Hud from the player's stats and the
        // game's gfx.wad, then blit it on top of the composed screen. If
        // gfx.wad is missing or unparseable we just skip the overlay (the POV
        // shot still renders) rather than failing the whole command.
        if let Some(wad) = gfx_wad.as_ref() {
            let stat = |f: &str| server.vm.ent_get_float(player, f) as i32;
            let hud = render::Hud {
                wad,
                health: stat("health"),
                ammo: stat("currentammo"),
                armor: stat("armorvalue"),
                items: stat("items"),
                weapon: stat("weapon"),
                ammo_shells: stat("ammo_shells"),
                ammo_nails: stat("ammo_nails"),
                ammo_rockets: stat("ammo_rockets"),
                ammo_cells: stat("ammo_cells"),
                time: server.time(),
                item_gettime: None,
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                sb_lines: refdef.sb_lines,
                face_pain: false,
            };
            render::draw_hud_into(&mut img, &hud);
        }

        img.to_rgb(&palette).write_ppm(path).map_err(|e| format!("write {path}: {e}"))?;
        let _ = writeln!(
            o,
            "  rendered player POV -> {path}{}",
            if weapon_mdl.is_some() {
                format!(" (weapon viewmodel {weapon_name} frame {weapon_frame})")
            } else {
                String::new()
            }
        );
    }
    Ok(Out::Text(o))
}

/// The screen `playtest`'s POV shot is: the game's default viewsize (100),
/// the 3-D view framed ABOVE the status bar by SCR_CalcRefdef — the framing
/// V_CalcRefdef's gun fudge for that viewsize assumes, so the gun sits on the
/// bar as in the game.
fn pov_screen(vid_w: usize, vid_h: usize) -> (f32, quake_rs::screen::Refdef) {
    let viewsize = render::VIEWSIZE_DEFAULT;
    (viewsize, render::calc_refdef(vid_w, vid_h, viewsize, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playtest_frames_the_view_for_the_guns_viewsize() {
        // The POV shot used the viewsize-100 gun offset on a full-screen view
        // (viewsize 120's framing) with the bar pasted over it: the gun sat
        // 48 rows low. Now both come from one viewsize.
        let (viewsize, refdef) = pov_screen(640, 400);
        assert_eq!(viewsize, 100.0);
        assert_eq!(refdef.sb_lines, 48);
        // id's 48-row bar in every mode (the "scaled 2-D" extra would make it 96).
        assert_eq!(refdef.vrect, render::ViewRect { x: 0, y: 0, w: 640, h: 352 });
        assert_eq!(render::viewmodel_fudge(viewsize), 2.0, "V_CalcRefdef's fudge at 100");
    }
}
