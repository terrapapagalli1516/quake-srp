//! `quaketool shot <pak> <map.bsp> <out.ppm> [options]` — one frame of the
//! game as a player sees it, at any size, shape and video setting: the map
//! spawned by the page's game client ([`quake_rs::client`]), the player at
//! `info_player_start` (or where `--origin` puts them), a few host frames at
//! 1/72 s so the view and the gun settle, and the finished screen — 3-D
//! view, gun, status bar — through the palette shifts as the page presents
//! it. For reviewing the renderer at 2026 resolutions; the golden renders
//! stay `scene`'s.
//!
//! ```text
//! --res WxH          the mode rendered (default 1920x1080)
//! --zoom N           write each pixel as N x N (a phone's "2x pixels": --res 1170x540 --zoom 2)
//! --frames N         host frames before the shot (default 36, half a second)
//! --yaw Y --pitch P  the view angles, degrees (default: the start's yaw, level)
//! --origin x,y,z     put the player there first (noclip and god on, so walls and lava do not matter)
//! --in-liquid K      or in the middle of the map's largest water, slime or lava leaf (K = water|slime|lava):
//!                    the underwater warp and tint
//! --viewsize V       the `viewsize` cvar (default 100: the view above the full status bar)
//! --fire N           hold +attack for the last N frames (muzzle flash, particles)
//! plus the video options (`video.rs`): --video, --fov-mode, --hires, --display (default square), --scaled2d, --threads
//! ```

use std::fmt::Write as _;

use quake_rs::client::{cl_main, host_cmd, Vid};
use quake_rs::pak::Pak;
use quake_rs::render;

use super::video::VideoArgs;

/// Quake's frame cadence (`host_maxfps` 72).
const DT: f64 = 1.0 / 72.0;

pub fn cmd_shot(args: &[String]) -> Result<String, String> {
    let (pak_path, map, out) = (&args[0], &args[1], &args[2]);
    let mut video = VideoArgs::default();
    let mut res = "1920x1080".to_string();
    let (mut zoom, mut frames, mut fire) = (1usize, 36u32, 0u32);
    let (mut yaw, mut pitch, mut origin): (Option<f32>, f32, Option<[f32; 3]>) = (None, 0.0, None);
    let mut liquid: Option<i32> = None;
    let mut viewsize = render::VIEWSIZE_DEFAULT;
    let mut i = 3;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        let num = |what: &str| val.parse::<f32>().map_err(|_| format!("{what}: bad number {val:?}"));
        if !video.parse(flag, val)? {
            match flag {
                "--res" => res = val.clone(),
                "--zoom" => zoom = val.parse::<usize>().map_err(|_| format!("--zoom: bad count {val:?}"))?.clamp(1, 8),
                "--frames" => frames = val.parse().map_err(|_| format!("--frames: bad count {val:?}"))?,
                "--fire" => fire = val.parse().map_err(|_| format!("--fire: bad count {val:?}"))?,
                "--yaw" => yaw = Some(num(flag)?),
                "--pitch" => pitch = num(flag)?,
                "--viewsize" => viewsize = num(flag)?,
                "--in-liquid" => {
                    liquid = Some(match val.as_str() {
                        "water" => quake_rs::bsp::CONTENTS_WATER,
                        "slime" => quake_rs::bsp::CONTENTS_SLIME,
                        "lava" => quake_rs::bsp::CONTENTS_LAVA,
                        _ => return Err(format!("--in-liquid: expected water, slime or lava, got {val:?}")),
                    })
                }
                "--origin" => {
                    let v: Vec<f32> = val.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>()
                        .map_err(|_| format!("--origin: expected x,y,z, got {val:?}"))?;
                    origin = Some(v.try_into().map_err(|_| format!("--origin: expected 3 numbers, got {val:?}"))?);
                }
                other => return Err(format!("shot: unknown option {other:?}")),
            }
        }
        i += 2;
    }
    video.apply();
    let (w, h) = super::parse_res(&res, video.cvars)?;

    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let mut sound = Vec::new();
    let rand = std::rc::Rc::new(quake_rs::qrand::QRand::new());
    let mut wk =
        host_cmd::build_walk_map(pak, map, &rand, &mut sound).ok_or_else(|| format!("{map} would not load"))?;
    wk.viewsize = viewsize;
    wk.renderer.set_threads(video.threads());
    if let Some(contents) = liquid {
        origin = Some(largest_leaf_centre(&wk.bsp, contents).ok_or_else(|| format!("{map} has no leaf of contents {contents}"))?);
    }
    if let Some(o) = origin {
        let mut said = Vec::new();
        for cmd in ["god", "noclip"] {
            host_cmd::run_game_command(&mut wk, cmd, &[cmd], &mut said, &mut sound);
        }
        wk.server.vm.ent_set_vector(wk.player, "origin", o);
    }
    if let Some(y) = yaw {
        wk.yaw = y;
    }
    wk.pitch = pitch;
    let vid = Vid {
        width: w,
        height: h,
        display_aspect: video.display_aspect(w, h, None),
        exact_perspective: false,
        video: video.cvars,
    };
    let gamma = render::build_gamma_table(1.0);
    let mut last = None;
    for f in 0..frames.max(1) {
        wk.in_attack = f + fire >= frames.max(1);
        let frame = cl_main::walk_frame(&mut wk, DT, false, &vid);
        if let Some(prev) = last.replace(frame) {
            render::recycle_image(prev.image);
        }
    }
    let frame = last.ok_or("no frame")?;
    // V_UpdatePalette + VID_ShiftPalette, as the page presents the frame.
    let ramps = (!frame.cshifts.is_empty()).then(|| render::cshift_ramps(&frame.cshifts, &gamma));
    let img = &frame.image;
    let (ow, oh) = (img.w * zoom, img.h * zoom);
    let mut ppm = format!("P6\n{ow} {oh}\n255\n").into_bytes();
    ppm.reserve(ow * oh * 3);
    let mut row = Vec::with_capacity(ow * 3);
    for y in 0..img.h {
        row.clear();
        for px in &img.rgb[y * img.w..(y + 1) * img.w] {
            let c = match &ramps {
                Some([r, g, b]) => [r[px[0] as usize], g[px[1] as usize], b[px[2] as usize]],
                None => *px,
            };
            for _ in 0..zoom {
                row.extend_from_slice(&c);
            }
        }
        for _ in 0..zoom {
            ppm.extend_from_slice(&row);
        }
    }
    std::fs::write(out, ppm).map_err(|e| format!("cannot write {out}: {e}"))?;
    let eye = wk.server.vm.ent_get_vector(wk.player, "origin");
    let mut o = String::new();
    let _ = writeln!(
        o,
        "shot {map} {w}x{h} ({}, display {:.4}, vid.aspect {:.4}, fov_x {:.2}) at [{:.0} {:.0} {:.0}] yaw {:.1} pitch {:.1}, {frames} frames -> {out} ({ow}x{oh})",
        video.tag(),
        vid.display_aspect,
        render::vid_aspect(w, h, vid.display_aspect),
        video.cvars.fov_mode.fov_x(90.0, w, h, render::vid_aspect(w, h, vid.display_aspect)),
        eye[0], eye[1], eye[2], wk.yaw, wk.pitch,
    );
    Ok(o)
}

/// Where to stand for the eye to be well inside `contents` (water, slime or
/// lava): the first point of a 7x7x7 grid over the largest leaves of that
/// contents (by box volume; a leaf's box is looser than the leaf) whose
/// neighbourhood 24 units each way is all `contents` too — less the view
/// height (`DEFAULT_VIEWHEIGHT` 22), as the eye is above the origin.
fn largest_leaf_centre(bsp: &quake_rs::bsp::Bsp, contents: i32) -> Option<[f32; 3]> {
    let volume = |l: &quake_rs::bsp::DLeaf| (0..3).map(|k| (l.maxs[k] as f32 - l.mins[k] as f32).max(0.0)).product::<f32>();
    let mut leaves: Vec<&quake_rs::bsp::DLeaf> = bsp.leafs.iter().filter(|l| l.contents == contents).collect();
    leaves.sort_by(|a, b| volume(b).total_cmp(&volume(a)));
    let inside = |p: [f32; 3]| {
        [-24.0f32, 24.0].iter().all(|&d| {
            (0..3).all(|k| {
                let mut q = p;
                q[k] += d;
                quake_rs::world::point_contents(bsp, q) == contents
            })
        }) && quake_rs::world::point_contents(bsp, p) == contents
    };
    for leaf in leaves {
        let at = |k: usize, i: usize| leaf.mins[k] as f32 + (leaf.maxs[k] as f32 - leaf.mins[k] as f32) * (i as f32 + 0.5) / 7.0;
        for (i, j, n) in (0..343).map(|c| (3 + (c % 7 + 4) % 7, (c / 7 % 7 + 3) % 7, (c / 49 + 3) % 7)) {
            let p = [at(0, i), at(1, j), at(2, n)];
            if inside(p) {
                return Some([p[0], p[1], p[2] - 22.0]);
            }
        }
    }
    None
}
