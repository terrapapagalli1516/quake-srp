//! `quaketool scene <pak> <map.bsp> <out.ppm> [--threads N]` — the golden
//! renders.
//!
//! The map is spawned through its QuakeC, the camera stands at
//! `info_player_start` re-aimed at the nearest monster (else the nearest
//! model), and the world is drawn with every spawned entity's model — alias
//! models, brush submodels and the `b_*.bsp` item boxes — in one z-buffer.
//!
//! **The pixel path is kept verbatim.** The sha256 of `scene` on e1m1..e1m3
//! are the goldens (`oracle/classic_check.py`), and `scene` makes them through
//! its own entity gathering: straight from the server's edicts and statics,
//! every model at frame 0 and skin 0, not through the game client
//! (`quake_rs::client`) that the page, `play` and `shot` run. Any change here
//! that reaches the pixels means new goldens.
//!
//! Debug knobs, from the environment (the goldens set none):
//! `QUAKE_AIM_DOOR` frames the nearest door instead; `QUAKE_DLIGHT` injects a
//! dynamic light; `QUAKE_BENCH=<iters>` (with `QUAKE_RES=WxH`) times the
//! render instead of writing it.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera};
use quake_rs::server::Server;

use super::{camera_for_bsp, color_for_name};
use crate::{CmdResult, Out, parse_res};

/// `scene`: parse a map from a PAK, spawn its QuakeC entities, and software-
/// render the world plus every spawned entity's `.mdl` alias model at its world
/// position, all sharing one z-buffer so models occlude correctly.
///
/// These pixels are the goldens: keep the path to them verbatim (the module
/// docs say why).
pub fn cmd_scene(pak_path: &str, map_name: &str, out: &str, opts: &[String]) -> CmdResult {
    use std::collections::HashMap;
    // `[--threads N]`: the renderer's threads (the pixels are the same).
    let threads = match opts {
        [] => 1,
        [flag, n] if flag == "--threads" => n.parse().ok().filter(|&n| n > 0).ok_or("--threads: expected a count")?,
        _ => return Err(format!("scene: unknown options {opts:?}").into()),
    };

    let pak = Pak::open(pak_path)?;

    // --- map BSP bytes from the pak (parsed twice: render + sim) ---
    let bsp_bytes = pak.read_file(map_name)?.ok_or_else(|| format!("{map_name:?} not found in {pak_path}"))?;
    let bsp_for_render = Bsp::parse(&bsp_bytes)?;
    let bsp_for_sim = Bsp::parse(&bsp_bytes)?;

    // --- palette + progs from the pak (graceful errors, never panic) ---
    let pal_bytes =
        pak.read_file("gfx/palette.lmp")?.ok_or_else(|| format!("gfx/palette.lmp not found in {pak_path}"))?;
    let palette =
        render::parse_palette(&pal_bytes).ok_or_else(|| format!("bad palette in {pak_path} (need >= 768 bytes)"))?;

    let progs_bytes = pak.read_file("progs.dat")?.ok_or_else(|| format!("progs.dat not found in {pak_path}"))?;
    let progs = Progs::parse(&progs_bytes)?;

    // Camera from the player start (derived from the render BSP before sim).
    // Eye at the player spawn; we re-aim it at the nearest model once we know
    // where the entities are, so a monster/item is framed instead of a wall.
    let base_cam = camera_for_bsp(&bsp_for_render);
    let eye = base_cam.pos;

    // --- spawn the map's entities ---
    let mut server = Server::with_pak(bsp_for_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let report = server.spawn_entities()?;

    // --- gather MDL instances from live edicts ---
    // Cache parsed models by in-pak name so each loads at most once.
    let mut model_cache: HashMap<String, Option<Mdl>> = HashMap::new();
    // Owned model data outlives the borrowing ModelInstances below.
    let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
    let mut monster_origins: Vec<[f32; 3]> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    // External brush-model item boxes (maps/b_*.bsp): parsed once per name and
    // stood at the entity origin. Owned so the borrowing `ExternalBModel` list can
    // be built after the loop (same pattern as `owned` for MDLs).
    let mut ext_cache: HashMap<String, Option<Bsp>> = HashMap::new();
    let mut ext_owned: Vec<(Bsp, [f32; 3])> = Vec::new();
    let mut skipped_load = 0usize;

    // Every model to draw, as (model, origin, angles, frame, is a monster):
    // each live edict whose QuakeC spawn actually setmodel'd (modelindex != 0;
    // a passable func_episodegate keeps its "*N" map key but no modelindex and
    // is invisible in Quake), then the signon's statics (the torches and
    // flames, whose edicts makestatic freed).
    let vm = &server.vm;
    let live = (0..vm.num_edicts() as i32)
        .filter(|&e| !vm.is_free_edict(e) && vm.ent_get_float(e, "modelindex") != 0.0)
        .map(|e| {
            let monster = vm.ent_get_string(e, "classname").starts_with("monster");
            let frame = vm.ent_get_float(e, "frame") as i32;
            (
                vm.ent_get_string(e, "model"),
                vm.ent_get_vector(e, "origin"),
                vm.ent_get_vector(e, "angles"),
                frame,
                monster,
            )
        });
    let statics =
        server.statics().iter().map(|st| (st.model.clone(), st.origin, st.angles, i32::from(st.frame), false));
    for (model, origin, angles, frame, monster) in live.chain(statics) {
        if model.is_empty() {
            continue;
        }
        // Brush submodels ("*N") — doors, platforms, buttons — draw as bmodels at
        // the entity origin (the world pass only draws model 0).
        if let Some(num) = model.strip_prefix('*') {
            if let Ok(idx) = num.parse::<usize>() {
                bmodels.push(render::BModelInstance { model_index: idx, origin, frame, angles });
            }
            continue;
        }
        // External brush-model item box (maps/b_*.bsp): a standalone bsp the item
        // set as its model (explosive box, ammo/health boxes). The world map path
        // itself is never an entity model here (worldspawn is the `*0` branch), but
        // guard against it explicitly so the world is never re-drawn as a box.
        if model.ends_with(".bsp") {
            if model != map_name {
                if !ext_cache.contains_key(&model) {
                    let parsed = match pak.read_file(&model) {
                        Ok(Some(bytes)) => Bsp::parse(&bytes).ok(),
                        _ => None,
                    };
                    ext_cache.insert(model.clone(), parsed);
                }
                if let Some(Some(bsp)) = ext_cache.get(&model) {
                    ext_owned.push((bsp.clone(), origin));
                }
            }
            continue;
        }
        if model.starts_with("maps/") {
            continue;
        }
        if !model.ends_with(".mdl") {
            continue;
        }

        // Load (and cache) the parsed model.
        if !model_cache.contains_key(&model) {
            let parsed = match pak.read_file(&model) {
                Ok(Some(bytes)) => Mdl::parse(&bytes).ok(),
                _ => None,
            };
            model_cache.insert(model.clone(), parsed);
        }
        let Some(Some(mdl)) = model_cache.get(&model) else {
            skipped_load += 1;
            continue;
        };

        let yaw = angles[1]; // angles = [pitch, yaw, roll]
        let color = color_for_name(&model);
        if monster {
            monster_origins.push(origin);
        }
        owned.push((mdl.clone(), origin, yaw, color));
    }

    let instances: Vec<render::ModelInstance> = owned
        .iter()
        .map(|(mdl, origin, yaw, color)| render::ModelInstance {
            syncbase: 0.0,
            mdl,
            origin: *origin,
            yaw: *yaw,
            pitch: 0.0,
            roll: 0.0,
            color: *color,
            frame: 0,
            blend: None,
            skinnum: 0,
        })
        .collect();
    let external: Vec<render::ExternalBModel> =
        ext_owned.iter().map(|(bsp, origin)| render::ExternalBModel { bsp, origin: *origin }).collect();

    // Aim the camera from the spawn eye at the nearest model that is not almost
    // on top of us (so it's framed, not degenerate); fall back to the spawn view.
    let d2 = |a: [f32; 3], b: [f32; 3]| {
        let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
        dx * dx + dy * dy + dz * dz
    };
    // Prefer the nearest monster (bigger, more recognisable); else nearest model.
    let pick = |pts: &[[f32; 3]]| {
        pts.iter().copied().filter(|o| d2(*o, eye) > 64.0 * 64.0).min_by(|a, b| d2(*a, eye).total_cmp(&d2(*b, eye)))
    };
    let model_pts: Vec<[f32; 3]> = owned.iter().map(|(_, o, _, _)| *o).collect();
    // When QUAKE_AIM_DOOR is set, frame the nearest brush submodel (door/plat) so
    // the bmodel render can be eyeballed; otherwise frame the nearest monster.
    // A door's entity origin is usually [0,0,0] (the brush geometry carries the
    // position), so aim at the centre of model N's bounds + the entity origin.
    let door_pts: Vec<[f32; 3]> = bmodels
        .iter()
        .filter_map(|b| {
            let m = bsp_for_render.models.get(b.model_index)?;
            Some([
                b.origin[0] + (m.mins[0] + m.maxs[0]) * 0.5,
                b.origin[1] + (m.mins[1] + m.maxs[1]) * 0.5,
                b.origin[2] + (m.mins[2] + m.maxs[2]) * 0.5,
            ])
        })
        .collect();
    let target = if std::env::var("QUAKE_AIM_DOOR").is_ok() {
        pick(&door_pts).or_else(|| pick(&monster_origins)).or_else(|| pick(&model_pts))
    } else {
        pick(&monster_origins).or_else(|| pick(&model_pts))
    };
    let cam = match target {
        Some(t) => {
            // Stand ~110 units in front of the target (between it and the eye),
            // at its mid-height, looking at it — so it fills a good part of frame.
            let dist = d2(t, eye).sqrt();
            let f = if dist > 130.0 { (dist - 110.0) / dist } else { 0.0 };
            let pos = [eye[0] + (t[0] - eye[0]) * f, eye[1] + (t[1] - eye[1]) * f, eye[2] + (t[2] - eye[2]) * f];
            Camera::looking_at(pos, [t[0], t[1], t[2] + 16.0], 90.0)
        }
        None => base_cam,
    };

    // Pass the server clock so liquid/sky surfaces are animated for this frame,
    // plus the animated light-style scales (torch flicker / light pulse). No live
    // particles in this single-shot `scene` command (no per-frame loop), so that
    // slice is empty; dynamic lights are empty too unless explicitly injected
    // below for A/B debugging.
    let light_styles = server.lightstyle_scales(server.sv_time(), quake_rs::server::LerpLightStyles::Classic);

    // Optional injected dynamic light, for eyeballing / A-B-diffing the dlight
    // path (e.g. the R_MarkLights BSP gating) on a real map:
    //   QUAKE_DLIGHT="x,y,z,radius" quaketool scene <pak> <map> <out>
    //   QUAKE_DLIGHT="eye"          (at the camera, radius 350 — explosion-sized)
    //   QUAKE_DLIGHT="eye:250"      (at the camera, radius 250)
    // Unset (the normal case, and all golden renders) leaves the dlight slice
    // empty — byte-identical to before this knob existed.
    let injected_dlights: Vec<quake_rs::dlight::DynamicLight> = std::env::var("QUAKE_DLIGHT")
        .ok()
        .and_then(|s| {
            let s = s.trim().to_string();
            let (origin, radius) = if let Some(rest) = s.strip_prefix("eye") {
                let r = rest.strip_prefix(':').and_then(|r| r.parse().ok()).unwrap_or(350.0);
                (cam.pos, r)
            } else {
                let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if v.len() != 4 {
                    return None;
                }
                ([v[0], v[1], v[2]], v[3])
            };
            // die far in the future / no decay: the light is fully live for this
            // single frame. key 0 = unowned (explosion-style).
            Some(vec![quake_rs::dlight::DynamicLight::new(origin, radius, f32::MAX, 0.0, 0.0, 0)])
        })
        .unwrap_or_default();
    // Read the colormap from the PAK (not the filesystem), matching the live game,
    // so this single-shot render uses id's 64-row colormap-LUT shading (and the lit
    // surface cache) exactly like step_walk does.
    let colormap = pak.read_file("gfx/colormap.lmp").ok().flatten();
    let scene = render::Scene {
        colormap: colormap.as_deref(),
        time: f64::from(server.time()),
        light_styles: &light_styles,
        dlights: &injected_dlights,
        bmodels: &bmodels,
        external: &external,
        models: &instances,
        ..render::Scene::new(&bsp_for_render, cam, 640, 400, &palette)
    };

    // Optional render benchmark, reusing this command's full scene setup:
    //   QUAKE_BENCH=<iters> [QUAKE_RES=<WxH>] quaketool scene <pak> <map> <out>
    // Renders the scene `iters` times at the given resolution and reports the WARM
    // per-frame cost (the first, cache-cold frame is excluded). This exercises the
    // exact rasteriser the live game uses, so it measures the real render cost.
    if let Ok(iters) = std::env::var("QUAKE_BENCH") {
        let iters: u32 = iters.parse().unwrap_or(60).max(1);
        let (bw, bh) = std::env::var("QUAKE_RES")
            .ok()
            .and_then(|s| parse_res(&s, render::VideoCvars::CLASSIC).ok())
            .unwrap_or((640usize, 400usize));
        let scene = render::Scene { width: bw, height: bh, ..scene };
        let mut renderer = render::Renderer::new();
        renderer.set_threads(threads);
        let cold = std::time::Instant::now();
        let _ = std::hint::black_box(renderer.render(&scene)); // warm the per-face caches
        let cold = cold.elapsed().as_secs_f64() * 1000.0;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(renderer.render(&scene));
        }
        let per = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // One profiled frame (caches already warm) for the per-phase breakdown.
        renderer.stats_begin();
        let _ = std::hint::black_box(renderer.render(&scene));
        let st = renderer.stats_end();
        let ms = |ns: u64| ns as f64 / 1_000_000.0;
        let mut o = String::new();
        use std::fmt::Write as _;
        let _ = writeln!(
            o,
            "bench {map_name} {bw}x{bh} on {threads} thread(s): {iters} warm frames -> {per:.2} ms/frame ({:.1} fps); the first, cold (every surface baked): {cold:.2} ms",
            1000.0 / per
        );
        let _ = writeln!(
            o,
            "  phases (ms): world {:.2}  submodel {:.2}  external {:.2}  alias {:.2}  particle {:.2}  sprite {:.2}  viewmodel {:.2}",
            ms(st.world_ns),
            ms(st.submodel_ns),
            ms(st.external_ns),
            ms(st.alias_ns),
            ms(st.particle_ns),
            ms(st.sprite_ns),
            ms(st.viewmodel_ns),
        );
        let _ = writeln!(
            o,
            "  world: {} faces ({} pvs-cull, {} frustum-cull, {} drawn), {} tris, {} px, surf {}/{} hit/miss",
            st.faces_total,
            st.faces_pvs_culled,
            st.faces_frustum_culled,
            st.faces_drawn,
            st.world_tris,
            st.world_pixels,
            st.surf_hits,
            st.surf_misses,
        );
        let _ = writeln!(
            o,
            "  submodel: {} faces visited, {} drawn ({}/{} surf hit/miss), {} tris, {} lightmap rebuilds",
            st.sub_faces_visited,
            st.sub_faces_drawn,
            st.sub_surf_hits,
            st.sub_surf_misses,
            st.sub_tris,
            st.sub_lm_builds,
        );
        let _ = writeln!(
            o,
            "  world sub-phases (ms): pvs {:.2}  sort {:.2}  setup+raster {:.2}  lightmap {:.2}  surf {:.2}",
            ms(st.world_pvs_ns),
            ms(st.world_sort_ns),
            ms(st.world_setup_ns),
            ms(st.world_light_ns),
            ms(st.world_surf_ns),
        );
        let _ = writeln!(
            o,
            "  surf cache: {} true-hits, {} cached-rebakes (warm: should be ~0), {} external-bypass-bakes (expected, cheap)",
            st.surf_cache_hits, st.surf_baked, st.surf_bypass_baked,
        );
        let _ = writeln!(
            o,
            "  edge renderer: {} edges, {} surfaces, {} spans (id's pools: r_maxedges 2400, r_maxsurfs 800)",
            st.edges_emitted, st.surfs_emitted, st.spans_emitted,
        );
        return Ok(Out::Text(o));
    }

    let mut renderer = render::Renderer::new();
    renderer.set_threads(threads);
    let img = renderer.render(&scene);
    img.to_rgb(&palette).write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "scene {map_name} from {pak_path}: {} faces, {} entities spawned",
        bsp_for_render.faces.len(),
        report.spawned
    );
    if let Some(dl) = injected_dlights.first() {
        let _ = writeln!(
            o,
            "  injected dlight at [{:.0} {:.0} {:.0}] radius {:.0} (camera at [{:.0} {:.0} {:.0}])",
            dl.origin[0], dl.origin[1], dl.origin[2], dl.radius, cam.pos[0], cam.pos[1], cam.pos[2]
        );
    }
    let _ = writeln!(
        o,
        "  {} MDL instances drawn ({} unique models, {} failed to load)",
        instances.len(),
        model_cache.values().filter(|v| v.is_some()).count(),
        skipped_load
    );
    let _ = writeln!(o, "  -> {out} ({}x{} PPM)", img.w, img.h);
    Ok(Out::Text(o))
}
