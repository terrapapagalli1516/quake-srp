//! The simulation commands: the server (`quake_rs::server`) run headless.
//!
//! - `sim` (here): a map's QuakeC entities spawned and their physics ticked;
//! - `simbench` (here): the game-logic tick timed, no rendering;
//! - `walk` and `demo` (here): a walk from the start and a recorded demo,
//!   drawn frame by frame into PPMs by the tool itself, not through the game
//!   client (`quake_rs::client`) that `play` runs;
//! - `playtest` ([`playtest`]): a player spawned, walked, sent into a fight,
//!   and its view rendered;
//! - `changelevel` ([`changelevel`]): a level's exit taken, and the
//!   inventory carried into the next map.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera};
use quake_rs::server::{Server, UserCmd};

use crate::entities::player_start;
use crate::render::color_for_name;
use crate::{read, CmdResult, Out};

pub mod changelevel;
pub mod playtest;

/// `sim <progs.dat> <bsp> [frames]` — the server without a client: a
/// player-sized trace straight down from the start, the map's QuakeC entities
/// spawned, then `frames` physics frames of 0.1 s.
pub fn cmd_sim(progs_path: &str, bsp_path: &str, frames: u32) -> CmdResult {
    let pbytes = read(progs_path)?;
    let progs = quake_rs::progs::Progs::parse(&pbytes)?;
    let bbytes = read(bsp_path)?;
    let bsp = Bsp::parse(&bbytes)?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "map {bsp_path}: {} faces, {} models; progs {progs_path}: {} functions, {} entityfields",
        bsp.faces.len(),
        bsp.models.len(),
        progs.functions.len(),
        progs.entityfields
    );

    // --- collision demo: trace straight down from the player spawn ---
    if let Some((spawn, _ang)) = player_start(&bsp.entities) {
        let from = [spawn[0], spawn[1], spawn[2] + 24.0];
        let to = [from[0], from[1], from[2] - 4096.0];
        let (mins, maxs) = ([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]);
        let tr = quake_rs::world::trace_world(&bsp, from, to, mins, maxs);
        let _ = writeln!(o, "\ncollision trace (player box, straight down from spawn):");
        let _ = writeln!(o, "  start {from:?}  contents={}", contents_name(quake_rs::world::point_contents(&bsp, from)));
        if tr.fraction < 1.0 {
            let _ = writeln!(
                o,
                "  hit floor at {:?}  ({:.1} units below)  normal {:?}",
                tr.endpos,
                from[2] - tr.endpos[2],
                tr.plane_normal
            );
        } else {
            let _ = writeln!(o, "  no hit within 4096 units (fraction {:.3})", tr.fraction);
        }
    }

    // --- spawn the map's QuakeC entities ---
    let mut server = Server::new(bsp, progs)?;
    // SV_SpawnServer set sv.modelname/world.model/mapname before loading
    // entities; derive the bare name from the bsp file stem.
    if let Some(stem) = std::path::Path::new(bsp_path).file_stem().and_then(|s| s.to_str()) {
        server.set_map_name(stem);
    }
    let rep = server.spawn_entities()?;
    let _ = writeln!(
        o,
        "\nspawn: {} entity blocks -> {} spawned, {} inhibited (skill), {} no-spawn-fn",
        rep.total, rep.spawned, rep.inhibited, rep.no_spawn_function
    );
    let _ = writeln!(o, "  live edicts: {}", server.live_entities());
    let _ = writeln!(o, "  top classnames spawned:");
    for (name, n) in rep.classnames.iter().take(15) {
        let _ = writeln!(o, "    {n:>4}  {name}");
    }

    // --- tick physics ---
    if frames > 0 {
        let mut total = 0usize;
        for _ in 0..frames {
            let fr = server.run_frame_f64(0.1)?;
            total += fr.thinks_fired;
        }
        let _ = writeln!(
            o,
            "\nphysics: {frames} frames @ dt=0.1 -> time {:.1}s, {total} think calls fired",
            server.time()
        );
    }
    Ok(Out::Text(o))
}

fn contents_name(c: i32) -> &'static str {
    match c {
        -1 => "empty",
        -2 => "solid",
        -3 => "water",
        -4 => "slime",
        -5 => "lava",
        -6 => "sky",
        _ => "?",
    }
}

/// `simbench <pak> <map.bsp> [frames]` — benchmark the GAME-LOGIC tick (no
/// rendering). Spawns the real map, connects a player, then runs `frames` server
/// frames of deterministic forward-walking input and reports the steady-state
/// per-frame cost of the simulation: `SV_Physics` (walk/push/toss), the QuakeC VM
/// (monster AI, item/door/trigger thinks), and BSP collision (`SV_Move`). This is
/// the sim counterpart to the `QUAKE_BENCH` render benchmark — together they cover
/// "not just rendering but the logic the game runs".
///
/// The first frames (cold field-offset cache, first monster sightings) are a
/// warm-up and excluded; the reported figure is the average over the timed window.
/// VM-statement and BSP-trace counts come from free-running counters
/// ([`quake_rs::vm::Vm::stmt_count`], [`quake_rs::world::trace_count`]) so the breakdown is
/// exact, not sampled.
pub fn cmd_simbench(pak_path: &str, map_name: &str, frames: u32) -> CmdResult {
    use std::time::Instant;
    let frames = frames.max(1);
    let pak = Pak::open(pak_path)?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_sim = Bsp::parse(&read(map_name)?)?;
    let entities = bsp_sim.entities.clone();
    let progs = Progs::parse(&read("progs.dat")?)?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities()?;
    let _player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    // Deterministic input: walk forward along the spawn yaw at full speed, the
    // same as a player holding W. dt = 0.1s is Quake's canonical server frame (the
    // monster think interval), so every frame fully exercises the AI tick — the
    // heaviest realistic per-frame logic load.
    let spawn_yaw = player_start(&entities).map(|(_, a)| a).unwrap_or(0.0);
    let cmd = UserCmd { forwardmove: 400.0, yaw: spawn_yaw, ..Default::default() };
    const DT: f64 = 0.1;

    // Warm-up: a handful of frames to populate the VM field-offset cache and let
    // the player settle onto the ground / nearby monsters notice it, so the timed
    // window measures steady state, not first-touch costs.
    let warmup = 20u32.min(frames);
    for _ in 0..warmup {
        let _ = server.client_frame_f64(&cmd, DT).map_err(|e| format!("client_frame: {e}"))?;
    }

    // Timed window.
    let stmt0 = server.vm.stmt_count;
    quake_rs::world::reset_trace_count();
    let mut thinks = 0usize;
    let start = Instant::now();
    for _ in 0..frames {
        let fr = server.client_frame_f64(&cmd, DT).map_err(|e| format!("client_frame: {e}"))?;
        thinks += fr.thinks_fired;
    }
    let elapsed = start.elapsed();
    let stmts = server.vm.stmt_count.wrapping_sub(stmt0);
    let traces = quake_rs::world::trace_count();

    // Live (non-free) edicts as a load proxy.
    let mut alive = 0usize;
    for e in 0..server.vm.num_edicts() {
        if !server.vm.edict_free.get(e).copied().unwrap_or(true) {
            alive += 1;
        }
    }

    let per_ms = elapsed.as_secs_f64() * 1000.0 / frames as f64;
    let game_secs = frames as f64 * DT;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "simbench {map_name}: {frames} frames @ dt={DT}s ({game_secs:.1}s game time), {} edicts spawned, {alive} alive",
        rep.spawned
    );
    let _ = writeln!(
        o,
        "  -> {per_ms:.4} ms/frame  ({:.0} sim-frames/sec)  | walked from info_player_start (yaw {spawn_yaw:.0})",
        1000.0 / per_ms
    );
    let _ = writeln!(
        o,
        "  per frame: {:.0} VM statements, {:.1} BSP traces, {:.1} thinks",
        stmts as f64 / frames as f64,
        traces as f64 / frames as f64,
        thinks as f64 / frames as f64,
    );
    let _ = writeln!(
        o,
        "  totals: {stmts} VM statements, {traces} traces, {thinks} thinks over the window",
    );
    Ok(Out::Text(o))
}

/// `walk <pak> <map.bsp> <out-prefix> [steps]` — spawn the map, then walk a
/// player box forward from `info_player_start` along its facing angle, writing
/// one rendered PPM frame per step (`<prefix>_000.ppm`, …). Movement uses the
/// world slide-move ([`quake_rs::world::walk_move`]) so the player follows walls
/// and stops at them; the spawned `.mdl` entities are drawn into every frame.
pub fn cmd_walk(pak_path: &str, map_name: &str, out_prefix: &str, steps: u32) -> CmdResult {
    let pak = Pak::open(pak_path)?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };
    let bsp_bytes = read_pak(map_name)?;
    let bsp = Bsp::parse(&bsp_bytes)?; // render + collision
    let bsp_sim = Bsp::parse(&bsp_bytes)?; // moved into the server
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;
    let progs = Progs::parse(&read_pak("progs.dat")?)?;

    let (spawn, ang) =
        player_start(&bsp.entities).ok_or_else(|| "map has no info_player_start".to_string())?;

    // Spawn entities and gather their MDL models (drawn at fixed positions).
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    server.spawn_entities()?;
    let mut model_cache: std::collections::HashMap<String, Option<Mdl>> =
        std::collections::HashMap::new();
    let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
    for e in 0..server.vm.num_edicts() {
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let ent = e as i32;
        let model = server.vm.ent_get_string(ent, "model");
        if model.is_empty()
            || model.starts_with('*')
            || model.starts_with("maps/")
            || !model.ends_with(".mdl")
        {
            continue;
        }
        if !model_cache.contains_key(&model) {
            let parsed = match pak.read_file(&model) {
                Ok(Some(b)) => Mdl::parse(&b).ok(),
                _ => None,
            };
            model_cache.insert(model.clone(), parsed);
        }
        if let Some(Some(mdl)) = model_cache.get(&model) {
            let origin = server.vm.ent_get_vector(ent, "origin");
            let yaw = server.vm.ent_get_vector(ent, "angles")[1];
            owned.push((mdl.clone(), origin, yaw, color_for_name(&model)));
        }
    }
    let instances: Vec<render::ModelInstance> = owned
        .iter()
        .map(|(mdl, origin, yaw, color)| render::ModelInstance {
            mdl,
            origin: *origin,
            yaw: *yaw, pitch: 0.0, roll: 0.0,
            color: *color,
            frame: 0,
            skinnum: 0,
        })
        .collect();

    // Walk forward from the spawn along its facing yaw.
    let (mins, maxs) = ([-16.0f32, -16.0, -24.0], [16.0f32, 16.0, 32.0]);
    let yr = ang.to_radians();
    let forward = [yr.cos(), yr.sin(), 0.0f32];
    let (w, h) = (480usize, 300usize);
    let (dt, speed) = (0.1f32, 180.0f32);

    // Settle onto the floor before the first step.
    let mut origin = quake_rs::world::walk_move(&bsp, spawn, mins, maxs, [0.0, 0.0, 0.0], dt);
    let start = origin;
    let mut frames = 0u32;
    let mut renderer = render::Renderer::new();
    for i in 0..steps {
        let wishvel = [forward[0] * speed, forward[1] * speed, 0.0];
        origin = quake_rs::world::walk_move(&bsp, origin, mins, maxs, wishvel, dt);
        let eye = [origin[0], origin[1], origin[2] + 22.0];
        let cam = Camera::looking_at(eye, [eye[0] + forward[0], eye[1] + forward[1], eye[2]], 90.0);
        let img = renderer.render(&render::Scene { models: &instances, ..render::Scene::new(&bsp, cam, w, h, &palette) });
        let path = format!("{out_prefix}_{i:03}.ppm");
        img.to_rgb(&palette).write_ppm(&path).map_err(|e| format!("cannot write {path}: {e}"))?;
        frames += 1;
    }

    let dist = {
        let (dx, dy) = (origin[0] - start[0], origin[1] - start[1]);
        (dx * dx + dy * dy).sqrt()
    };
    let mut o = String::new();
    let _ = writeln!(
        o,
        "walk {map_name}: {} model instances, {frames} frames @ {w}x{h}",
        instances.len()
    );
    let _ = writeln!(
        o,
        "  spawn {start:?} -> end {origin:?}  (advanced {dist:.0} units; the slide stops at walls)"
    );
    let _ = writeln!(o, "  wrote {out_prefix}_000.ppm .. {out_prefix}_{:03}.ppm", frames.saturating_sub(1));
    Ok(Out::Text(o))
}

/// `demo <pak> <demo.dem> <out-prefix> [stride]` — replay a recorded Quake demo
/// (id's attract-mode `.dem`) and render it: parse the net-protocol stream into
/// per-frame entity snapshots, then draw the map + each entity's `.mdl` from the
/// recorded viewpoint, one PPM per sampled server frame. `stride` 0 = auto-pick
/// to emit ~120 frames.
pub fn cmd_demo(pak_path: &str, demo_name: &str, out_prefix: &str, stride_arg: usize) -> CmdResult {
    let pak = Pak::open(pak_path)?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };

    let demo = quake_rs::demo::parse_demo(&read_pak(demo_name)?)?;
    let map = demo
        .map_name()
        .ok_or_else(|| "demo has no world model (never received serverinfo)".to_string())?
        .to_string();
    let bsp = Bsp::parse(&read_pak(&map)?)?;
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;

    let total = demo.frames.len();
    let stride = if stride_arg == 0 { (total / 120).max(1) } else { stride_arg };
    let (w, h) = (480usize, 300usize);

    let mut model_cache: std::collections::HashMap<String, Option<Mdl>> =
        std::collections::HashMap::new();
    let mut written = 0u32;
    let mut renderer = render::Renderer::new();
    for f in demo.frames.iter().step_by(stride) {
        // Build the alias-model instances visible this frame.
        let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
        for e in &f.entities {
            let name = match demo.model_precache.get(e.modelindex) {
                Some(n) if n.ends_with(".mdl") => n.clone(),
                _ => continue,
            };
            if !model_cache.contains_key(&name) {
                let parsed = match pak.read_file(&name) {
                    Ok(Some(b)) => Mdl::parse(&b).ok(),
                    _ => None,
                };
                model_cache.insert(name.clone(), parsed);
            }
            if let Some(Some(mdl)) = model_cache.get(&name) {
                owned.push((mdl.clone(), e.origin, e.angles[1], color_for_name(&name)));
            }
        }
        let instances: Vec<render::ModelInstance> = owned
            .iter()
            .map(|(mdl, origin, yaw, color)| render::ModelInstance {
                mdl,
                origin: *origin,
                yaw: *yaw, pitch: 0.0, roll: 0.0,
                color: *color,
                frame: 0,
                skinnum: 0,
            })
            .collect();

        // Demo view angles are [pitch, yaw, roll]; the renderer's pitch is +up,
        // while Quake's is +down, so negate it. view_origin already includes the
        // view height. Build the camera directly from the recorded angles.
        let cam = Camera {
            pos: f.view_origin,
            yaw: f.view_angles[1],
            pitch: -f.view_angles[0],
            // Demos record viewangles[ROLL] (the engine's V_CalcViewRoll bank); use
            // it so demo playback leans/tilts exactly as the original did.
            roll: f.view_angles[2],
            fov_deg: 90.0,
        };
        let img = renderer.render(&render::Scene { models: &instances, ..render::Scene::new(&bsp, cam, w, h, &palette) });
        let path = format!("{out_prefix}_{written:04}.ppm");
        img.to_rgb(&palette).write_ppm(&path).map_err(|e| format!("cannot write {path}: {e}"))?;
        written += 1;
    }

    let mut o = String::new();
    let _ = writeln!(o, "demo {demo_name}: map {map}, {total} server frames", );
    let _ = writeln!(
        o,
        "  {} models precached, {} unique .mdl loaded; rendered {written} frames (stride {stride}) @ {w}x{h}",
        demo.model_precache.len().saturating_sub(1),
        model_cache.values().filter(|v| v.is_some()).count()
    );
    let _ = writeln!(o, "  wrote {out_prefix}_0000.ppm .. {out_prefix}_{:04}.ppm", written.saturating_sub(1));
    Ok(Out::Text(o))
}
