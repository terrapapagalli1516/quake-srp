//! `quaketool view <pak>[,<pak>...] <map.bsp> <out.ppm> [options]` — render ONE exactly specified view,
//! `<pak>` is usually one file, but a comma-separated list layers like a mod's
//! own game directory (last one listed searched first) — a mission pack's map
//! needs its own pak for the bsp/progs over id1's for the shared palette.
//! so the C oracle (`oracle/`: id's own software renderer, headless) can be diffed
//! against this port pixel for pixel. The camera is given in Quake's convention —
//! `r_refdef.vieworg` and `r_refdef.viewangles` (pitch positive looks DOWN) — and
//! the clock is explicit, so light styles, sky scroll, liquid turb and texture
//! animation sit at the same phase as the C frame.
//!
//! ```text
//! --res WxH          framebuffer size (default 320x200)
//! --origin x,y,z     eye position (default info_player_start + 22, the view height)
//! --angles p,y,r     view angles in degrees (default 0,<start angle>,0)
//! --time T           the render clock, cl.time (default: the server clock after spawn)
//! --fov F            horizontal field of view (default 90)
//! --aspect A         vid.aspect, R_ViewChanged's pixelAspect (default 1, square
//!                    pixels, as the oracle's vid_null; id's DOS/Win 320x200 on a
//!                    4:3 monitor is 0.8333 — the oracle's -oracle_aspect)
//! --exactpersp 0|1   1: exact perspective at every pixel, the port's extra (default
//!                    0: id's 16-pixel segments, D_DrawSpans16 / Turbulent8)
//! --vrect x,y,w,h    render only the view r_refdef.vrect: a w x h image, placed at
//!                    (x, y) of the --res screen (the sky is centred on the screen,
//!                    D_Sky_uv_To_st); the output is the w x h view (default: the
//!                    view is the whole screen, viewsize 120)
//! --ents FILE        draw these entities: the oracle's `.ents` list, one per line,
//!                    `model ox oy oz pitch yaw roll frame skin syncbase effects kind`
//!                    (without it: the world only, as r_drawentities 0)
//! --viewmodel M:F    also draw weapon model M at frame F
//! --viewent x,y,z,p,y,r  the weapon's origin and angles, `cl.viewent` (default:
//!                    V_CalcRefdef's for a still player at viewsize 120)
//! --bench N          then render the same view N more times, report warm ms/frame
//! --particles FILE   draw these particles: the oracle's `.parts` list, one per line,
//!                    `x y z color`, in id's draw order (without it: none)
//! --dlight x,y,z,radius[,minlight]  a live dynamic light (repeatable; the oracle
//!                    passes id's `cl_dlights`, in slot order)
//! --d-mipscale X     the `d_mipscale` cvar (default 1; 0 = every surface at mip 0)
//! --d-mipcap N       the `d_mipcap` cvar (default 0; the finest mip level allowed)
//! --video, --fov-mode, --hires, --sky  the port's video cvars (`video.rs`; default classic)
//! ```
//!
//! The map's entities are still spawned (worldspawn's QuakeC sets the light-style
//! strings), but only `--ents` decides what is drawn. No existing command's output
//! depends on this one. With the eye in water, slime or lava the view is id's
//! `r_waterwarp` one: rendered into the (at most 320x200) warp buffer and
//! stretched over the frame by `D_WarpScreen` at `--time`, as `R_RenderView`
//! does before the oracle's shot.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera};
use quake_rs::server::Server;
use quake_rs::spr::Sprite;

use super::color_for_name;
use crate::entities::player_start;
use crate::video::VideoArgs;
use crate::{parse_res, CmdResult, Out};

pub fn cmd_view(args: &[String]) -> CmdResult {
    use std::collections::HashMap;

    let (pak_path, map_name, out) = (&args[0], &args[1], &args[2]);
    let parse_vec3 = |flag: &str, s: &str| -> Result<[f32; 3], String> {
        let v: Vec<f32> = s.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>()
            .map_err(|_| format!("{flag}: expected x,y,z, got {s:?}"))?;
        if v.len() != 3 {
            return Err(format!("{flag}: expected 3 comma-separated numbers, got {s:?}"));
        }
        Ok([v[0], v[1], v[2]])
    };
    let (mut w, mut h) = (320usize, 200usize);
    let (mut origin, mut angles, mut time, mut fov) = (None, None, None, 90.0f32);
    let mut opts = render::RenderOptions::default();
    let mut vrect: Option<(usize, usize, usize, usize)> = None;
    let (mut ents_path, mut viewmodel_arg): (Option<&str>, Option<&str>) = (None, None);
    let mut bench: Option<u32> = None;
    let mut viewent: Option<[f32; 6]> = None;
    let mut dlights: Vec<quake_rs::dlight::DynamicLight> = Vec::new();
    let mut particles: Vec<([f32; 3], u8)> = Vec::new();
    let mut video = VideoArgs::default();
    let mut res: Option<&str> = None;
    let mut i = 3;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        if video.parse(flag, val)? {
            i += 2;
            continue;
        }
        match flag {
            "--res" => res = Some(val),
            "--origin" => origin = Some(parse_vec3(flag, val)?),
            "--angles" => angles = Some(parse_vec3(flag, val)?),
            "--time" => time = Some(val.parse::<f32>().map_err(|_| format!("--time: bad number {val:?}"))?),
            "--fov" => fov = val.parse().map_err(|_| format!("--fov: bad number {val:?}"))?,
            "--vrect" => {
                let v: Vec<usize> = val.split(',').map(|p| p.trim().parse::<usize>()).collect::<Result<_, _>>()
                    .map_err(|_| format!("--vrect: expected x,y,w,h, got {val:?}"))?;
                let [x, y, vw, vh] = v[..] else {
                    return Err(format!("--vrect: expected 4 numbers, got {val:?}").into());
                };
                vrect = Some((x, y, vw, vh));
            }
            "--exactpersp" => {
                opts.exact_perspective = match val.as_str() {
                    "0" => false,
                    "1" => true,
                    _ => return Err(format!("--exactpersp: expected 0 or 1, got {val:?}").into()),
                }
            }
            "--aspect" => {
                opts.pixel_aspect = val.parse().map_err(|_| format!("--aspect: bad number {val:?}"))?;
                if !(opts.pixel_aspect.is_finite() && opts.pixel_aspect > 0.0) {
                    return Err(format!("--aspect: must be a positive number, got {val:?}").into());
                }
            }
            "--ents" => ents_path = Some(val.as_str()),
            "--particles" => {
                let text = std::fs::read_to_string(val).map_err(|e| format!("--particles {val}: {e}"))?;
                particles = parse_particles(&text).map_err(|e| format!("--particles {val}: {e}"))?;
            }
            "--dlight" => {
                let v: Vec<f32> = val.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>()
                    .map_err(|_| format!("--dlight: expected x,y,z,radius[,minlight], got {val:?}"))?;
                if v.len() != 4 && v.len() != 5 {
                    return Err(format!("--dlight: expected 4 or 5 numbers, got {val:?}").into());
                }
                // Live for this frame (die far away, no decay); unowned.
                let minlight = v.get(4).copied().unwrap_or(0.0);
                dlights.push(quake_rs::dlight::DynamicLight::new([v[0], v[1], v[2]], v[3], f32::MAX, minlight, 0.0, 0));
            }
            "--viewmodel" => viewmodel_arg = Some(val.as_str()),
            "--viewent" => {
                let v: Vec<f32> = val.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>()
                    .map_err(|_| format!("--viewent: expected x,y,z,p,y,r, got {val:?}"))?;
                viewent = Some(v.try_into().map_err(|_| format!("--viewent: expected 6 numbers, got {val:?}"))?);
            }
            "--bench" => bench = Some(val.parse::<u32>().map_err(|_| format!("--bench: bad count {val:?}"))?.max(1)),
            "--d-mipscale" | "--d-mipcap" => {
                let x: f32 = val.parse().map_err(|_| format!("{flag}: bad number {val:?}"))?;
                if flag == "--d-mipscale" { opts.mip.mipscale = x } else { opts.mip.mipcap = x }
            }
            other => return Err(format!("view: unknown option {other:?}").into()),
        }
        i += 2;
    }
    // The video cvars first: hires lifts --res's clamp. (`--display` is not
    // used here: `--aspect` gives vid.aspect itself.)
    video.apply();
    opts.video = video.cvars;
    if let Some(r) = res {
        (w, h) = parse_res(r, video.cvars)?;
    }

    // A comma-separated list layers like `-game`'s own mod_dirs (common.rs's
    // `init_filesystem`): each pak over the ones before it, so the last one
    // listed is searched first — e.g. "id1/pak0.pak,id1/pak1.pak,hipnotic/
    // pak0.pak" puts hip1m1.bsp's own hipnotic pak on top of id1's shared
    // gfx/palette.lmp and progs builtins, the way `-hipnotic` would. A bare
    // path (no comma) is unchanged from before this existed.
    let pak = pak_path
        .split(',')
        .map(Pak::open)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .reduce(|under, over| over.over(under))
        .ok_or_else(|| format!("{pak_path}: empty pak list"))?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };
    let bsp_bytes = read_pak(map_name)?;
    let bsp = Bsp::parse(&bsp_bytes)?;
    let bsp_sim = Bsp::parse(&bsp_bytes)?;
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;
    let colormap = pak.read_file("gfx/colormap.lmp").ok().flatten();
    let progs = Progs::parse(&read_pak("progs.dat")?)?;
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    server.spawn_entities()?;

    // Default camera: the player start at the view height (DEFAULT_VIEWHEIGHT 22).
    let start = player_start(&bsp.entities);
    let origin = match (origin, start) {
        (Some(o), _) => o,
        (None, Some((o, _))) => [o[0], o[1], o[2] + 22.0],
        (None, None) => return Err("map has no info_player_start; pass --origin".into()),
    };
    let angles = angles.unwrap_or([0.0, start.map_or(0.0, |(_, a)| a), 0.0]);
    let cam = Camera { pos: origin, yaw: angles[1], pitch: -angles[0], roll: angles[2], fov_deg: fov };
    let time = time.unwrap_or_else(|| server.time());
    let light_styles = server.lightstyle_scales(time);

    // The entity list, resolved against per-name model caches (each file parsed once).
    let mut mdls: HashMap<String, Option<Mdl>> = HashMap::new();
    let mut sprs: HashMap<String, Option<Sprite>> = HashMap::new();
    let mut ext: HashMap<String, Option<Bsp>> = HashMap::new();
    // (model, origin, angles, frame, skin) per alias entity.
    type AliasDesc = (String, [f32; 3], [f32; 3], usize, i32);
    let mut alias_descs: Vec<AliasDesc> = Vec::new();
    // (model, origin, angles, frame, alias entries before it) per sprite entity.
    type SpriteDesc = (String, [f32; 3], [f32; 3], usize, usize);
    let mut sprite_descs: Vec<SpriteDesc> = Vec::new();
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut skipped = 0usize;
    if let Some(p) = ents_path {
        let text = std::fs::read_to_string(p).map_err(|e| format!("cannot read {p}: {e}"))?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let num = |k: usize| f.get(k).and_then(|s| s.parse::<f32>().ok());
            let (Some(model), Some(ox), Some(oy), Some(oz), Some(ap), Some(ay), Some(ar), Some(fr), Some(sk)) =
                (f.first(), num(1), num(2), num(3), num(4), num(5), num(6), num(7), num(8))
            else {
                return Err(format!("{p}: malformed entity line {line:?}").into());
            };
            let (model, org, ang) = (model.to_string(), [ox, oy, oz], [ap, ay, ar]);
            if let Some(n) = model.strip_prefix('*') {
                let model_index = n.parse().map_err(|_| format!("{p}: bad submodel {model:?}"))?;
                // The `.ents` line carries this entity's angles too (oracle.c
                // writes every entity's, bmodel or not): a mission-pack door
                // caught mid-turn in id's own frame turns the same way here.
                bmodels.push(render::BModelInstance { model_index, origin: org, frame: fr as i32, angles: ang });
            } else if model.ends_with(".bsp") {
                ext.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Bsp::parse(&b).ok()));
                ext_descs.push((model, org));
            } else if model.ends_with(".spr") {
                sprs.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Sprite::parse(&b).ok()));
                sprite_descs.push((model, org, ang, fr as usize, alias_descs.len()));
            } else if model.ends_with(".mdl") {
                mdls.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Mdl::parse(&b).ok()));
                alias_descs.push((model, org, ang, fr as usize, sk as i32));
            } else {
                skipped += 1;
            }
        }
    }
    let instances: Vec<render::ModelInstance> = alias_descs
        .iter()
        .filter_map(|(name, org, ang, frame, skin)| match mdls.get(name) {
            Some(Some(mdl)) => Some(render::ModelInstance {
                mdl,
                origin: *org,
                yaw: ang[1],
                pitch: ang[0],
                roll: ang[2],
                color: color_for_name(name),
                frame: *frame,
                blend: None,
                skinnum: *skin,
            }),
            _ => None,
        })
        .collect();
    let externals: Vec<render::ExternalBModel> = ext_descs
        .iter()
        .filter_map(|(name, org)| match ext.get(name) {
            Some(Some(bsp)) => Some(render::ExternalBModel { bsp, origin: *org }),
            _ => None,
        })
        .collect();
    // The resolved models before each alias entry: where a sprite falls among
    // the models on id's list (an unresolved model is not drawn).
    let resolved_before: Vec<usize> = std::iter::once(0)
        .chain(alias_descs.iter().scan(0, |n, (name, ..)| {
            *n += usize::from(matches!(mdls.get(name), Some(Some(_))));
            Some(*n)
        }))
        .collect();
    let sprites: Vec<render::SpriteInstance> = sprite_descs
        .iter()
        .filter_map(|(name, org, ang, frame, k)| match sprs.get(name) {
            Some(Some(sprite)) => Some(render::SpriteInstance {
                sprite,
                origin: *org,
                angles: *ang,
                frame: *frame,
                models_before: resolved_before[*k],
            }),
            _ => None,
        })
        .collect();
    let vm_mdl = match viewmodel_arg {
        Some(arg) => {
            let (name, frame) = arg.rsplit_once(':').unwrap_or((arg, "0"));
            let frame: usize = frame.parse().map_err(|_| format!("--viewmodel: bad frame in {arg:?}"))?;
            Some((Mdl::parse(&read_pak(name)?)?, frame))
        }
        None => None,
    };
    let unresolved = alias_descs.len() + ext_descs.len() + sprite_descs.len()
        - instances.len() - externals.len() - sprites.len();
    // The view: the whole screen, or r_refdef.vrect placed on it.
    let (view_w, view_h) = match vrect {
        Some((x, y, vw, vh)) => {
            if vw == 0 || vh == 0 || x + vw > w || y + vh > h {
                return Err(format!("--vrect: {vw}x{vh} at ({x}, {y}) is not inside the {w}x{h} screen").into());
            }
            opts.screen = Some(render::ScreenPlace { x, y, vid_w: w, vid_h: h });
            (vw, vh)
        }
        None => (w, h),
    };

    let dowarp = quake_rs::world::point_contents(&bsp, cam.pos) <= quake_rs::bsp::CONTENTS_WATER;
    let mut renderer = render::Renderer::new();
    renderer.set_threads(video.threads());
    let mut render_once = || {
        // cl.viewent as given (the oracle's), else V_CalcRefdef's for a still
        // player in a full-frame view (id at viewsize 120: no fudge, no bob).
        let (origin_ofs, gun_angles) = match viewent {
            Some(v) => ([v[0] - cam.pos[0], v[1] - cam.pos[1], v[2] - cam.pos[2]], [v[3], v[4], v[5]]),
            None => {
                let a = [cam.pitch, cam.yaw, 0.0];
                (render::viewmodel_origin_ofs(a, 0.0, 120.0), a)
            }
        };
        let viewmodel = vm_mdl.as_ref().map(|(mdl, frame)| render::Viewmodel {
            mdl,
            frame: *frame,
            blend: None,
            origin_ofs,
            angles: gun_angles,
        });
        let scene = render::Scene {
            colormap: colormap.as_deref(),
            time,
            light_styles: &light_styles,
            dlights: &dlights,
            bmodels: &bmodels,
            external: &externals,
            models: &instances,
            sprites: &sprites,
            particles: &particles,
            viewmodel,
            options: opts,
            ..render::Scene::new(&bsp, cam, view_w, view_h, &palette)
        };
        // R_SetupFrame's r_dowarp, for the full-frame view (viewsize 120): the
        // warp buffer's view, stretched over the screen by D_WarpScreen. (With
        // --vrect the view is drawn unwarped.)
        if dowarp && vrect.is_none() {
            let r = quake_rs::screen::warp_vrect(w, h, 120.0, false, video.cvars.hires);
            let mut wopts = opts;
            wopts.screen = Some(render::ScreenPlace { x: r.x, y: r.y, vid_w: w, vid_h: h });
            let scene = render::Scene { width: r.w, height: r.h, options: wopts, ..scene };
            let view = renderer.render(&scene);
            let mut screen = render::Image::new(w, h, 0);
            let full = render::ViewRect { x: 0, y: 0, w, h };
            renderer.warp_into(view, &mut screen, full, 0, time, video.cvars.hires);
            return screen;
        }
        renderer.render(&scene)
    };
    let img = render_once();
    // Warm re-renders of the same view (the first, cold frame above is excluded),
    // the port side of the oracle's `oracle_bench`: renderer cost only.
    let bench = bench.map(|n| {
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(render_once());
        }
        (n, start.elapsed().as_secs_f64() * 1000.0 / n as f64)
    });
    img.to_rgb(&palette).write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "view {map_name} {w}x{h} origin [{} {} {}] angles [{} {} {}] fov {fov} time {time}{}{}{}{}",
        origin[0], origin[1], origin[2], angles[0], angles[1], angles[2],
        if opts.pixel_aspect != 1.0 { format!(" aspect {}", opts.pixel_aspect) } else { String::new() },
        if opts.exact_perspective { " exactpersp" } else { "" },
        vrect.map(|(x, y, vw, vh)| format!(" vrect {x},{y},{vw},{vh}")).unwrap_or_default(),
        if dowarp && vrect.is_none() { " (underwater: D_WarpScreen)" } else { "" }
    );
    let _ = writeln!(
        o,
        "  entities: {} alias, {} submodel, {} external, {} sprite ({} unresolved, {} unknown kind)",
        instances.len(), bmodels.len(), externals.len(), sprites.len(), unresolved, skipped
    );
    if let Some((n, per)) = bench {
        let _ = writeln!(o, "  bench {n} warm frames -> {per:.4} ms/frame ({:.1} fps)", 1000.0 / per);
    }
    let _ = writeln!(o, "  -> {out} ({}x{} PPM)", img.w, img.h);
    Ok(Out::Text(o))
}

/// The oracle's `.parts` file: `x y z color` per line (`#` comments), the
/// particles `R_DrawParticles` drew, in its order.
fn parse_particles(text: &str) -> Result<Vec<([f32; 3], u8)>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let bad = || format!("line {}: expected `x y z color`, got {line:?}", n + 1);
        let [x, y, z, c] = f[..] else { return Err(bad()) };
        let num = |s: &str| s.parse::<f32>().map_err(|_| bad());
        let color = c.parse::<i64>().map_err(|_| bad())?;
        // id's particle colour is an int indexing the 8-bit palette (`byte` on draw).
        out.push(([num(x)?, num(y)?, num(z)?], (color & 255) as u8));
    }
    Ok(out)
}
