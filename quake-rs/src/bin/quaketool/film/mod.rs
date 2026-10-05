//! `quaketool film <pak> <shot-file> <out-dir> [overrides]` — a camera for
//! making films inside the engine: a shot ([`shot`]: the map or demo, the
//! settings, the camera, the x-ray views) rendered to numbered frames, and
//! its game sound if asked.
//!
//! The world runs as the game runs it: the page's own client
//! ([`quake_rs::client`]: `walk_frame`, `demo_frame`), its server ticking
//! the map's monsters, doors, lifts, lights and torches, with the preset's
//! settings as the page applies them. Only the camera is the film's: the
//! client draws from [`Walk::camera`] (an extra the client has for this:
//! the view, the sound's listener and the underwater test follow it, and
//! the game goes on around it). How the game's clock meets the film's:
//!
//! - `clock free` (slop's): a host frame for every film frame, the game
//!   stepped by the film frame's share of game time ([`Stepping::Uncapped`],
//!   the uncapped page's) — monsters glide, lights glide, as the slop preset
//!   draws at any refresh rate.
//! - `clock id` (Classic's): id's 72 Hz — a host frame every 1/72 s of game
//!   time, each picture (the camera's too) held until the next, as id's
//!   renderer only drew on a host frame. A film frame shows the last tick at
//!   or before it: at 60 a second, one tick in six is never seen and the
//!   motion steps unevenly; in slow motion every tick shows, held.
//!
//! `speed` slows the game (the film's seconds stay seconds) and 0 freezes it
//! while the camera goes on: a frozen frame is a paused one (the server
//! stops, as behind id's menu) redrawn from the new camera.
//!
//! The overrides are the shot file's lines given last, as `--KEY VALUE`
//! (`--preset classic`, `--fps 30`, `--size 640x360`, `--cvar "r_perspspan 1"`,
//! `--xray z`), and the command's own:
//!
//! ```text
//! --threads N        the renderer's threads (default: every core); the pixels are the same for any N
//! --format png|ppm   the frames' files (default png: <out-dir>/00000.png ...)
//! --raw              the frames as raw RGB24 to stdout instead, for `ffmpeg -f rawvideo`
//!                    (out-dir `-`: none, when there is no sound to write)
//! --frames A..B      only film frames A to B-1 (the game still runs from the start)
//! --sound            write <out-dir>/sound.wav too (as `sound on`)
//! ```
//!
//! The frame size the shot gives (`size`) is the output's: the game's
//! picture (its `mode`) is scaled into it by whole pixels where it fits
//! (nearest neighbour, never smoothed) at its display's shape, centred, black
//! around it. `sound.wav` is the engine's mixer (the preset's: id's at 11025
//! Hz in Classic, the slop mixer at 48000 Hz), mixed to each frame's end, so
//! its sample `n` is heard at film second `n / rate`.

use std::io::Write as _;
use std::rc::Rc;
use std::time::Instant;

use quake_rs::client::{ClientFrame, DemoPlay, SoundCall, Vid, Walk, cl_demo, cl_main, host_cmd};
use quake_rs::console::ConNotify;
use quake_rs::cvar::Cvars;
use quake_rs::pak::Pak;
use quake_rs::progs::OFS_PARM0;
use quake_rs::qrand::QRand;
use quake_rs::render::xray::XrayOptions;
use quake_rs::render::{self, FovMode, MipCvars, VideoCvars};
use quake_rs::snd::Mixer;
use quake_rs::stepping::Stepping;

pub mod camera;
pub mod png;
pub mod shot;
pub mod text;
pub mod xray;

use shot::{CameraSpec, Clock, Corner, Player, Preset, Shot, Warmup, World, XrayBase};

/// QuakeC's `flags` bits (defs.qc).
const FL_GODMODE: f32 = 64.0;
const FL_NOTARGET: f32 = 128.0;
/// `DEFAULT_VIEWHEIGHT`: the eye above a player's origin.
const VIEWHEIGHT: f64 = 22.0;
/// id's frame: the warm-up's step.
const TICK: f64 = 1.0 / 72.0;

/// The output's file format.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Ppm,
    Raw,
}

/// The game a shot runs.
enum Game {
    Walk(Box<Walk>),
    Demo(Box<DemoPlay>),
}

impl Game {
    fn renderer(&mut self) -> &mut render::Renderer {
        match self {
            Game::Walk(w) => &mut w.renderer,
            Game::Demo(d) => &mut d.renderer,
        }
    }

    fn palette(&self) -> [[u8; 3]; 256] {
        match self {
            Game::Walk(w) => w.palette,
            Game::Demo(d) => d.palette,
        }
    }

    /// One host frame of `dt` game seconds (0: paused, the camera's view of
    /// a frozen world), drawn or not.
    fn frame(&mut self, dt: f64, vid: &Vid, draw: bool) -> ClientFrame {
        let frozen = dt <= 0.0;
        match self {
            Game::Walk(w) => match (frozen, draw) {
                // A frozen frame is a paused one: the server stops, as behind id's menu.
                (_, true) => cl_main::walk_frame(w, dt.max(0.0), frozen, vid),
                (_, false) => cl_main::walk_frame_undrawn(w, dt.max(0.0), frozen, vid),
            },
            Game::Demo(d) => {
                if draw {
                    cl_demo::demo_frame(d, dt.max(0.0) as f32, false, vid)
                } else {
                    cl_demo::demo_frame_undrawn(d, dt.max(0.0) as f32, false, vid)
                }
            }
        }
    }
}

/// `quaketool film`: see the module doc.
pub fn cmd_film(args: &[String]) -> Result<String, String> {
    let (pak_path, shot_path, out_dir) = (&args[0], &args[1], &args[2]);
    let text = std::fs::read_to_string(shot_path).map_err(|e| format!("cannot read {shot_path}: {e}"))?;
    let mut shot = Shot::parse(&text).map_err(|e| e.to_string())?;
    let mut threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut format = Format::Png;
    let mut range: Option<(usize, usize)> = None;
    let rest = &args[3..];
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let val = rest.get(i + 1);
        let need = || val.ok_or_else(|| format!("{flag} needs a value"));
        match flag {
            "--raw" => {
                format = Format::Raw;
                i += 1;
                continue;
            }
            "--sound" => {
                shot.sound = true;
                i += 1;
                continue;
            }
            "--threads" => threads = need()?.parse().ok().filter(|&n| n > 0).ok_or("--threads N")?,
            "--format" => {
                format = match need()?.as_str() {
                    "png" => Format::Png,
                    "ppm" => Format::Ppm,
                    "raw" => Format::Raw,
                    v => return Err(format!("--format png|ppm|raw, got {v:?}")),
                }
            }
            "--frames" => {
                let v = need()?;
                let (a, b) = v.split_once("..").ok_or("--frames A..B")?;
                range = Some((a.parse().map_err(|_| "--frames A..B")?, b.parse().map_err(|_| "--frames A..B")?));
            }
            _ => {
                let key = flag.strip_prefix("--").ok_or_else(|| format!("film: unknown argument {flag:?}"))?;
                shot.set(&format!("{key} {}", need()?))?;
            }
        }
        i += 2;
    }
    shot.check()?;
    if out_dir == "-" {
        if format != Format::Raw || shot.sound {
            return Err("film: out-dir `-` is for --raw without sound".into());
        }
    } else {
        std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot make {out_dir}: {e}"))?;
    }
    let report = run(pak_path, &shot, out_dir, threads, format, range)?;
    Ok(report)
}

/// The game's settings for the shot's preset and cvars.
fn shot_cvars(shot: &Shot) -> Cvars {
    let mut c = match shot.preset {
        Preset::Classic => Cvars::classic(),
        Preset::Slop => Cvars::slop(),
    };
    for (name, value) in &shot.cvars {
        if let Some(cv) = quake_rs::cvar::find(name) {
            cv.set(&mut c, value);
        }
    }
    c
}

/// The picture's size and the [`Vid`] the frames are drawn with, as the page
/// draws the cvars `c` (quake-wasm's `vid::vid`), and the shape the
/// picture is shown at.
fn shot_vid(shot: &Shot, c: &Cvars) -> (Vid, f64) {
    let native = c.native;
    let video = VideoCvars {
        fov_mode: if c.fov_adapt { FovMode::HorPlus } else { FovMode::Classic },
        hires: native,
        sky: c.sky,
        lightstyles: c.lightstyles,
        torches: c.torches,
    };
    let (w, h) = shot.mode.unwrap_or(if native {
        (shot.size.0 / shot.pixel.max(1), shot.size.1 / shot.pixel.max(1))
    } else {
        (usize::from(c.vid_resolution.0), usize::from(c.vid_resolution.1))
    });
    let (w, h) = video.clamp_to_max(w.max(1), h.max(1));
    let display = shot.display.unwrap_or(if native { None } else { Some(4.0 / 3.0) });
    let display_aspect = display.unwrap_or(w as f64 / h as f64);
    let vid = Vid {
        width: w,
        height: h,
        display_aspect,
        persp_span: c.persp_span,
        video,
        mip: MipCvars { mipscale: c.d_mipscale, mipcap: c.d_mipcap },
    };
    (vid, display_aspect)
}

/// `PF_setorigin`, through the engine's builtin (the origin, then
/// `SV_LinkEdict`).
fn set_origin(w: &mut Walk, org: [f32; 3]) {
    let (vm, p) = (&mut w.server.vm, w.player);
    vm.set_gi(OFS_PARM0, p);
    vm.set_gv(OFS_PARM0 + 3, org);
    let _ = vm.call_builtin(2, 2);
    vm.ent_set_vector(p, "velocity", [0.0; 3]);
}

/// QuakeC's `FL_MONSTER`.
const FL_MONSTER: i32 = 32;

/// Wake the living monster nearest `at`: its enemy the player, and
/// QuakeC's `FoundTarget` (what `FindTarget` calls when it sees one).
fn wake_monster(w: &mut Walk, at: [f32; 3]) {
    let vm = &mut w.server.vm;
    let dist = |o: [f32; 3]| (0..3).map(|k| (o[k] - at[k]).powi(2)).sum::<f32>();
    let monster = (1..vm.num_edicts() as i32)
        .filter(|&e| !vm.is_free_edict(e))
        .filter(|&e| vm.ent_get_float(e, "flags") as i32 & FL_MONSTER != 0 && vm.ent_get_float(e, "health") > 0.0)
        .min_by(|&a, &b| dist(vm.ent_get_vector(a, "origin")).total_cmp(&dist(vm.ent_get_vector(b, "origin"))));
    let (Some(m), Some(f)) = (monster, vm.progs().find_function("FoundTarget")) else { return };
    let p = w.player;
    vm.ent_set_int(m, "enemy", p);
    let t = vm.sv_time() as f32;
    vm.gset_int("self", m);
    vm.gset_int("other", 0);
    vm.gset_int("activator", p);
    vm.gset_float("time", t);
    if vm.execute(f).is_err() {
        vm.reset_execution();
    }
}

/// A label in the frame's `corner`, in Quake's lettering, a font pixel to
/// each 360th of the frame's height (3x at 1080p).
fn draw_label(
    g: &text::Glyphs,
    out: &mut [u8],
    (w, h): (usize, usize),
    palette: &[[u8; 3]; 256],
    label: &str,
    corner: Corner,
) {
    let scale = (h / 360).max(1);
    let (tw, th) = g.measure(label, scale);
    let margin = 8 * scale as i64;
    let x = match corner {
        Corner::TopLeft | Corner::BottomLeft => margin,
        Corner::TopRight | Corner::BottomRight => w as i64 - tw as i64 - margin,
    };
    let y = match corner {
        Corner::TopLeft | Corner::TopRight => margin,
        Corner::BottomLeft | Corner::BottomRight => h as i64 - th as i64 - margin,
    };
    g.draw(out, w, 3, palette, label, (x, y), scale, 1);
}

/// Set or clear `flag` in the player's `flags`.
fn player_flag(w: &mut Walk, flag: f32, on: bool) {
    let p = w.player;
    let f = w.server.vm.ent_get_float(p, "flags") as i32;
    let f = if on { f | flag as i32 } else { f & !(flag as i32) };
    w.server.vm.ent_set_float(p, "flags", f as f32);
}

/// Build the shot's game.
fn build(shot: &Shot, pak: &Pak, c: &Cvars, sound: &mut Vec<SoundCall>) -> Result<Game, String> {
    match &shot.world {
        World::Map(name) => {
            let map = format!("maps/{name}.bsp");
            let rand = Rc::new(QRand::new());
            let mut w = host_cmd::build_walk_map(pak.clone(), &map, &rand, sound, c.max_edicts as usize)
                .ok_or_else(|| format!("{map} would not load"))?;
            if shot.skill != 1 {
                // The level again at that skill (a new server reads `skill` as it spawns).
                w.server.set_skill(shot.skill as f32);
                host_cmd::try_changelevel(&mut w, name, sound);
            }
            let mut said = Vec::new();
            host_cmd::run_game_command(&mut w, "god", &["god"], &mut said, sound);
            player_flag(&mut w, FL_GODMODE, true);
            player_flag(&mut w, FL_NOTARGET, shot.notarget);
            match shot.player {
                Player::Spawn => {}
                Player::At(org, yaw) => {
                    set_origin(&mut w, org);
                    if let Some(yaw) = yaw {
                        w.yaw = yaw;
                    }
                }
                Player::Camera => {
                    host_cmd::run_game_command(&mut w, "noclip", &["noclip"], &mut said, sound);
                    // The map's exits shut: a camera crossing one must not end the level.
                    let vm = &mut w.server.vm;
                    for e in 1..vm.num_edicts() as i32 {
                        if !vm.is_free_edict(e) && vm.ent_string_ref(e, "classname") == "trigger_changelevel" {
                            vm.ent_set_float(e, "solid", 0.0);
                        }
                    }
                }
            }
            Ok(Game::Walk(Box::new(w)))
        }
        World::Demo { name, .. } => {
            let file = cl_demo::default_extension(name, ".dem");
            let d = cl_demo::build_demo(pak.clone(), &file, sound).ok_or_else(|| format!("{file} would not load"))?;
            Ok(Game::Demo(Box::new(d)))
        }
    }
}

/// Hand the game the settings the page hands it every frame.
fn apply_settings(game: &mut Game, shot: &Shot, c: &Cvars, stepping: Stepping) {
    let preset_viewsize = c.viewsize;
    let viewsize = if shot.hud { preset_viewsize } else { 120.0 };
    let crosshair = render::Crosshair::from_cvar(shot.crosshair);
    match game {
        Game::Walk(w) => {
            w.viewsize = viewsize;
            w.crosshair = crosshair;
            w.sbar_layout = c.sbar_layout;
            w.stepping = stepping;
            w.lerpmove = c.lerpmove;
            w.lerpmodels = c.lerpmodels;
            w.nailbarrels = c.nailbarrels;
            w.draw_viewmodel = shot.gun;
            if !shot.messages {
                w.centerprint = None;
                w.notify = ConNotify::default();
            }
        }
        Game::Demo(d) => {
            d.viewsize = viewsize;
            d.crosshair = crosshair;
            d.sbar_layout = c.sbar_layout;
            d.stepping = stepping;
            d.lerpmove = c.lerpmove;
            d.lerpmodels = c.lerpmodels;
            d.draw_viewmodel = shot.gun;
            if !shot.messages {
                d.centerprint = None;
                d.notify = ConNotify::default();
            }
        }
    }
}

/// The 8-bit screen through its frame's palette, as RGB.
fn present(frame: &ClientFrame, palette: &[[u8; 3]; 256], gamma: &[u8; 256]) -> Vec<u8> {
    let pal = render::FramePalette::new(palette, &frame.cshifts, gamma);
    let mut rgb = Vec::with_capacity(frame.image.pixels.len() * 3);
    for &p in &frame.image.pixels {
        rgb.extend_from_slice(&pal.0[usize::from(p)][..3]);
    }
    rgb
}

/// Where [`fit`] puts a picture shown at `aspect` in a `w x h` frame: `(x0,
/// y0, width, height)`.
pub fn fit_rect(aspect: f64, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let (rw, rh) = if (w as f64) / (h as f64) > aspect {
        (((h as f64) * aspect).round() as usize, h)
    } else {
        (w, ((w as f64) / aspect).round() as usize)
    };
    let (rw, rh) = (rw.clamp(1, w), rh.clamp(1, h));
    ((w - rw) / 2, (h - rh) / 2, rw, rh)
}

/// `src` (`sw x sh` RGB, shown at `aspect`) into a `w x h` frame: as large
/// as fits at that shape, centred, each output pixel the source pixel under
/// it (nearest, never smoothed), black around.
pub fn fit(src: &[u8], sw: usize, sh: usize, aspect: f64, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3];
    let (x0, y0, rw, rh) = fit_rect(aspect, w, h);
    let cols: Vec<usize> = (0..rw).map(|x| (x * sw / rw).min(sw - 1)).collect();
    for y in 0..rh {
        let sy = (y * sh / rh).min(sh - 1);
        let row = &src[sy * sw * 3..(sy + 1) * sw * 3];
        let dst = &mut out[((y0 + y) * w + x0) * 3..((y0 + y) * w + x0 + rw) * 3];
        for (d, &sx) in dst.chunks_exact_mut(3).zip(&cols) {
            d.copy_from_slice(&row[sx * 3..sx * 3 + 3]);
        }
    }
    out
}

/// Write the `pending` frames' files (`<out_dir>/NNNNN.png` or `.ppm`), the
/// PNGs encoded one a thread at once.
fn write_frames(
    pending: &mut Vec<(usize, Vec<u8>)>,
    out_dir: &str,
    format: Format,
    (w, h): (usize, usize),
) -> Result<(), String> {
    let results: Vec<Result<(), String>> = std::thread::scope(|s| {
        let jobs: Vec<_> = pending
            .iter()
            .map(|(n, px)| {
                s.spawn(move || {
                    let (path, bytes) = if format == Format::Png {
                        (format!("{out_dir}/{n:05}.png"), png::encode(w, h, 3, px))
                    } else {
                        let mut b = format!("P6\n{w} {h}\n255\n").into_bytes();
                        b.extend_from_slice(px);
                        (format!("{out_dir}/{n:05}.ppm"), b)
                    };
                    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {path}: {e}"))
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err("a frame's encoder panicked".into()))).collect()
    });
    pending.clear();
    results.into_iter().collect()
}

/// `S_Update_` to a fixed clock: paint the mixer up to sample pair `end`
/// (none if it is there already), onto `pcm`.
fn paint_to(m: &mut Mixer, end: i64, pcm: &mut Vec<i16>) {
    let k = usize::try_from(end - m.painted_time()).unwrap_or(0);
    let at = pcm.len();
    pcm.resize(at + 2 * k, 0);
    m.paint(&mut pcm[at..]);
}

/// A 16-bit stereo PCM `.wav`.
fn wav_bytes(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + pcm.len() * 2);
    b.extend(b"RIFF");
    b.extend((36 + data_len).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(rate.to_le_bytes());
    b.extend((rate * 4).to_le_bytes());
    b.extend(4u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data_len.to_le_bytes());
    for s in pcm {
        b.extend(s.to_le_bytes());
    }
    b
}

/// The camera's pose at film second `t`, if the shot places one.
fn camera_at(shot: &Shot, path: Option<&camera::Path>, t: f64) -> Option<camera::Pose> {
    match &shot.camera {
        Some(CameraSpec::Path(_)) => path.map(|p| p.at(t)),
        _ => None,
    }
}

/// Render the shot (see the module doc). Returns the report line.
fn run(
    pak_path: &str,
    shot: &Shot,
    out_dir: &str,
    threads: usize,
    format: Format,
    range: Option<(usize, usize)>,
) -> Result<String, String> {
    let started = Instant::now();
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let c = shot_cvars(shot);
    quake_rs::draw::set_scaled_2d(c.scaled_2d);
    let (vid, aspect) = shot_vid(shot, &c);
    let clock = shot.clock();
    let stepping = if clock == Clock::Free { Stepping::Uncapped } else { Stepping::Classic };
    let gamma = render::build_gamma_table(c.gamma);
    let mut calls = Vec::new();
    let mut game = build(shot, &pak, &c, &mut calls)?;
    game.renderer().set_threads(threads);
    let path = match &shot.camera {
        Some(CameraSpec::Path(keys)) => Some(camera::Path::new(keys, shot.fov)),
        _ => None,
    };
    let wants_xray = shot.xray != XrayBase::Game || shot.wire.world || shot.wire.entities;
    let wants_base = shot.xray != XrayBase::Game;
    let xray_options = XrayOptions {
        capture: wants_xray,
        lightmaps: (shot.xray == XrayBase::Lightmaps).then_some(XRAY_GREY),
        vis_from: shot.vis,
    };
    game.renderer().set_xray(Some(xray_options));
    let palette = game.palette();

    // The sound: the preset's mixer, mixing exactly to each frame's end.
    let mut mixer: Option<Mixer> = shot.sound.then(|| {
        let mut m = c.sound.mixer(&pak, 48000);
        m.cvars.volume = c.volume;
        m
    });
    if let Some(m) = mixer.as_mut() {
        m.run(&pak, &calls);
    }
    let mut pcm: Vec<i16> = Vec::new();

    // Warm up (undrawn): the map's monsters settle, or the demo reaches `from`.
    apply_settings(&mut game, shot, &c, Stepping::Classic);
    let mut warmed = 0.0;
    let warm_done = |game: &Game, warmed: f64| match (&shot.world, shot.warmup, game) {
        (World::Demo { from, .. }, ..) => warmed + 1e-9 >= *from,
        (World::Map(_), Warmup::For(s), _) => warmed + 1e-9 >= s,
        (World::Map(_), Warmup::Until(t), Game::Walk(w)) => f64::from(w.clock) + 1e-6 >= t || warmed > 600.0,
        (World::Map(_), Warmup::Until(_), Game::Demo(_)) => true,
    };
    while !warm_done(&game, warmed) {
        if let Game::Walk(w) = &mut game {
            if let (Player::Camera, Some(pose)) = (shot.player, camera_at(shot, path.as_ref(), 0.0)) {
                set_origin(w, [pose.pos[0], pose.pos[1], pose.pos[2] - VIEWHEIGHT].map(|v| v as f32));
            }
        }
        let frame = game.frame(TICK, &vid, false);
        warmed += TICK;
        if let Some(m) = mixer.as_mut() {
            // Mixed and dropped: the warm-up's sound is not the shot's.
            m.run(&pak, &frame.sound);
            paint_to(m, (warmed * f64::from(m.rate())).round() as i64, &mut Vec::new());
        }
        render::recycle_image(frame.image);
    }
    // One paused frame from the first camera, drawn and dropped: the surface
    // cache is warm at frame 0, as a running game's is.
    if let Game::Walk(w) = &mut game {
        w.camera = camera_at(shot, path.as_ref(), 0.0).map(|p| p.camera());
    } else if let Game::Demo(d) = &mut game {
        d.camera = camera_at(shot, path.as_ref(), 0.0).map(|p| p.camera());
    }
    let frame = game.frame(0.0, &vid, true);
    render::recycle_image(frame.image);
    let sound_start = mixer.as_ref().map_or(0, |m| m.painted_time());
    let clock_at_start = match &game {
        Game::Walk(w) => f64::from(w.clock),
        Game::Demo(d) => d.time,
    };

    let frames = shot.frames();
    let (first, end) = range.map_or((0, frames), |(a, b)| (a.min(frames), b.min(frames)));
    let (ow, oh) = shot.size;
    let mut last: Option<Vec<u8>> = None;
    // clock id: the 1/72 s ticks of game time run since film second 0.
    let mut ticks = 0u64;
    let mut stdout = std::io::stdout().lock();
    // Frames waiting for their files, encoded a batch at a time on the threads.
    let mut pending: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut drawn = 0usize;
    let mut held = 0usize;
    let label_glyphs = match (&shot.label, wad(&pak)) {
        (Some(_), Some(w)) => Some(text::Glyphs::new(&w, text::Font::Gold)?),
        _ => None,
    };
    let t_frames = Instant::now();
    for n in 0..end {
        let t = n as f64 / shot.fps;
        let prev = t - 1.0 / shot.fps;
        let dt = shot.game_time(t) - shot.game_time(prev);
        let wanted = n >= first;
        // The monsters to wake by now.
        if let Game::Walk(w) = &mut game {
            for &(at, when) in &shot.wake {
                if when <= t && (when > prev || n == 0) {
                    wake_monster(w, at);
                }
            }
        }
        // This film frame's host frames, each with the film second its camera
        // is at: one per film frame (free); or id's ticks of game time up to
        // now (id), the last drawn and none if no tick is due, the picture
        // held; a frozen world is one paused frame, redrawn from the camera.
        let mut steps: Vec<(f64, f64)> = Vec::new();
        match clock {
            Clock::Free => steps.push((dt.min(0.1), t)),
            Clock::Id if dt <= 0.0 => steps.push((0.0, t)),
            Clock::Id => {
                let g = shot.game_time(t);
                while (ticks + 1) as f64 * TICK <= g + 1e-9 {
                    ticks += 1;
                    steps.push((TICK, shot.film_time(ticks as f64 * TICK)));
                }
                if steps.is_empty() && last.is_none() {
                    steps.push((0.0, t));
                }
            }
        }
        if steps.is_empty() {
            held += 1;
        }
        let count = steps.len();
        for (i, (step, tc)) in steps.into_iter().enumerate() {
            let draw = wanted && i + 1 == count;
            let pose = camera_at(shot, path.as_ref(), tc);
            apply_settings(&mut game, shot, &c, stepping);
            if let Game::Walk(w) = &mut game {
                w.camera = pose.map(|p| p.camera());
                if let (Player::Camera, Some(p)) = (shot.player, pose) {
                    set_origin(w, [p.pos[0], p.pos[1], p.pos[2] - VIEWHEIGHT].map(|v| v as f32));
                }
            } else if let Game::Demo(d) = &mut game {
                d.camera = pose.map(|p| p.camera());
            }
            let frame = game.frame(step, &vid, draw);
            if let Some(m) = mixer.as_mut() {
                m.run(&pak, &frame.sound);
            }
            if draw {
                let mut rgb = present(&frame, &palette, &gamma);
                if wants_base {
                    composite_xray(&mut game, shot, &vid, &c, &palette, &mut rgb, tc);
                }
                let mut out = fit(&rgb, frame.image.w, frame.image.h, aspect, ow, oh);
                if shot.wire.world || shot.wire.entities {
                    wire_xray(&game, shot, &vid, &c, &palette, &mut out, (frame.image.w, frame.image.h), aspect, tc);
                }
                if let (Some(g), Some(label)) = (&label_glyphs, &shot.label) {
                    draw_label(g, &mut out, (ow, oh), &palette, label, shot.labelpos);
                }
                last = Some(out);
                drawn += 1;
            }
            render::recycle_image(frame.image);
        }
        if let Some(m) = mixer.as_mut() {
            // Mixed to this frame's end: sample `k` is heard at film second `k / rate`.
            let end = sound_start + ((n + 1) as f64 / shot.fps * f64::from(m.rate())).round() as i64;
            paint_to(m, end, &mut pcm);
            if n < first {
                pcm.clear();
            }
        }
        if !wanted {
            continue;
        }
        let Some(out) = last.as_ref() else { continue };
        match format {
            Format::Raw => {
                if let Err(e) = stdout.write_all(out) {
                    return Err(format!("stdout: {e}"));
                }
            }
            Format::Png | Format::Ppm => {
                pending.push((n, out.clone()));
                if pending.len() >= threads {
                    write_frames(&mut pending, out_dir, format, (ow, oh))?;
                }
            }
        }
    }
    write_frames(&mut pending, out_dir, format, (ow, oh))?;
    let _ = stdout.flush();
    let frames_s = t_frames.elapsed().as_secs_f64();
    let mut report = format!(
        "film: {} frames ({}..{} of {frames}) at {}x{} ({}x{} drawn, {}, clock {}), {drawn} drawn, {held} held, cl.time {clock_at_start:.3} at frame 0; {:.1} s ({:.0} ms a frame), {:.1} s in all",
        end - first,
        first,
        end,
        ow,
        oh,
        vid.width,
        vid.height,
        if shot.preset == Preset::Classic { "Classic" } else { "slop" },
        if clock == Clock::Id { "id" } else { "free" },
        frames_s,
        frames_s * 1000.0 / (end - first).max(1) as f64,
        started.elapsed().as_secs_f64(),
    );
    if let Some(m) = mixer.as_ref() {
        let path = format!("{out_dir}/sound.wav");
        std::fs::write(&path, wav_bytes(&pcm, m.rate())).map_err(|e| format!("cannot write {path}: {e}"))?;
        report += &format!("; {path} at {} Hz, {:.2} s", m.rate(), pcm.len() as f64 / 2.0 / f64::from(m.rate()));
    }
    // With --raw, stdout is the frames': the report goes to stderr.
    if format == Format::Raw {
        eprintln!("{report}");
        return Ok(String::new());
    }
    Ok(report + "\n")
}

/// The palette index the lightmaps view lights: a light grey, so the
/// colormap's 64 shades run from it to black.
const XRAY_GREY: u8 = 9;

/// The shot's x-ray views over the 3-D view of `rgb` (the screen).
fn composite_xray(
    game: &mut Game,
    shot: &Shot,
    vid: &Vid,
    c: &Cvars,
    palette: &[[u8; 3]; 256],
    rgb: &mut [u8],
    t: f64,
) {
    let viewsize = if shot.hud { c.viewsize } else { 120.0 };
    let refdef = render::calc_refdef(vid.width, vid.height, viewsize, false, c.sbar_layout);
    let v = refdef.vrect;
    let strength = shot.mix_at(t);
    let (bsp, x) = match game {
        Game::Walk(w) => (&w.bsp, w.renderer.xray_frame()),
        Game::Demo(d) => (&d.bsp, d.renderer.xray_frame()),
    };
    let Some(x) = x else { return };
    let mut screen = xray::Rgb { w: vid.width, h: vid.height, px: rgb };
    xray::composite(&mut screen, (v.x, v.y), x, bsp, palette, shot.xray, strength);
}

/// The shot's wireframe over the output frame `out` (the screen `sw x sh`
/// fitted into it at `aspect`).
#[allow(clippy::too_many_arguments)]
fn wire_xray(
    game: &Game,
    shot: &Shot,
    vid: &Vid,
    c: &Cvars,
    palette: &[[u8; 3]; 256],
    out: &mut [u8],
    (sw, sh): (usize, usize),
    aspect: f64,
    t: f64,
) {
    let viewsize = if shot.hud { c.viewsize } else { 120.0 };
    let v = render::calc_refdef(vid.width, vid.height, viewsize, false, c.sbar_layout).vrect;
    let (bsp, x) = match game {
        Game::Walk(w) => (&w.bsp, w.renderer.xray_frame()),
        Game::Demo(d) => (&d.bsp, d.renderer.xray_frame()),
    };
    let Some(x) = x else { return };
    let (ow, oh) = shot.size;
    let (x0, y0, rw, rh) = fit_rect(aspect, ow, oh);
    let place = xray::Place {
        vx: v.x as f32,
        vy: v.y as f32,
        x0: x0 as f32,
        y0: y0 as f32,
        kx: rw as f32 / sw as f32,
        ky: rh as f32 / sh as f32,
    };
    xray::wire_over(out, (ow, oh), place, x, bsp, palette, shot.wire, shot.mix_at(t));
}

/// The pak's `gfx.wad`.
fn wad(pak: &Pak) -> Option<quake_rs::wad::Wad2> {
    pak.read_file("gfx.wad").ok().flatten().and_then(|b| quake_rs::wad::Wad2::parse(b).ok())
}

/// `quaketool filmtext <pak> <out.png> <text> [--font white|gold|num|anum]
/// [--scale N] [--shadow N] [--size WxH] [--at X,Y]`: `text` (`\n` or `|`
/// between lines) in Quake's lettering on a transparent PNG — as large as
/// the text, or `--size` with the text at `--at` (default centred).
pub fn cmd_filmtext(args: &[String]) -> Result<String, String> {
    let (pak_path, out, text) = (&args[0], &args[1], args[2].replace("\\n", "\n").replace('|', "\n"));
    let (mut font, mut scale, mut shadow, mut size, mut at) = (text::Font::White, 4usize, 1usize, None, None);
    let rest = &args[3..];
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let val = rest.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            "--font" => font = text::Font::parse(val).ok_or("--font white|gold|num|anum")?,
            "--scale" => scale = val.parse::<usize>().map_err(|_| "--scale N")?.clamp(1, 64),
            "--shadow" => shadow = val.parse().map_err(|_| "--shadow N")?,
            "--size" => {
                let (w, h) = val.split_once('x').ok_or("--size WxH")?;
                size = Some((
                    w.parse::<usize>().map_err(|_| "--size WxH")?,
                    h.parse::<usize>().map_err(|_| "--size WxH")?,
                ));
            }
            "--at" => {
                let (x, y) = val.split_once(',').ok_or("--at X,Y")?;
                at = Some((x.parse::<i64>().map_err(|_| "--at X,Y")?, y.parse::<i64>().map_err(|_| "--at X,Y")?));
            }
            _ => return Err(format!("filmtext: unknown option {flag:?}")),
        }
        i += 2;
    }
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let wad = wad(&pak).ok_or("the pak has no gfx.wad")?;
    let palette = pak
        .read_file("gfx/palette.lmp")
        .ok()
        .flatten()
        .and_then(|b| render::parse_palette(&b))
        .ok_or("gfx/palette.lmp is missing")?;
    let glyphs = text::Glyphs::new(&wad, font)?;
    let (tw, th) = glyphs.measure(&text, scale);
    let pad = shadow * scale;
    let (w, h) = size.unwrap_or((tw + pad, th + pad));
    let at = at.unwrap_or((((w as i64) - tw as i64) / 2, ((h as i64) - th as i64) / 2));
    let mut px = vec![0u8; w * h * 4];
    glyphs.draw(&mut px, w, 4, &palette, &text, at, scale, shadow);
    std::fs::write(out, png::encode(w, h, 4, &px)).map_err(|e| format!("cannot write {out}: {e}"))?;
    Ok(format!("filmtext: {w}x{h} -> {out}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_scales_by_whole_pixels_and_centres() {
        // 2x1 red|blue into 8x2 at 2:1: exactly 4x, no bars.
        let src = [255, 0, 0, 0, 0, 255];
        let out = fit(&src, 2, 1, 2.0, 8, 4);
        assert_eq!(&out[..3], &[255, 0, 0]);
        assert_eq!(&out[3 * 3..3 * 4], &[255, 0, 0]);
        assert_eq!(&out[3 * 4..3 * 5], &[0, 0, 255]);
        // At 1:1 into 8x4: a 4x4 box, pillarboxed.
        let out = fit(&src, 2, 1, 1.0, 8, 4);
        assert_eq!(&out[..3], &[0, 0, 0]);
        assert_eq!(&out[3 * 2..3 * 3], &[255, 0, 0]);
        assert_eq!(&out[3 * 5..3 * 6], &[0, 0, 255]);
        assert_eq!(&out[3 * 7..3 * 8], &[0, 0, 0]);
    }

    /// The shareware pak, when it is there.
    fn pak_path() -> Option<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    }

    /// The frames' hashes of a shot rendered on `threads` threads.
    fn hashes(pak: &str, shot: &str, threads: usize, tag: &str) -> Vec<u64> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/film-test-{}-{tag}-{threads}", std::process::id()));
        let shot_path = dir.with_extension("shot");
        std::fs::write(&shot_path, shot).unwrap();
        let args: Vec<String> = [
            pak,
            &shot_path.to_string_lossy(),
            &dir.to_string_lossy(),
            "--threads",
            &threads.to_string(),
            "--format",
            "ppm",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        cmd_film(&args).expect("the shot renders");
        let mut out = Vec::new();
        for n in 0.. {
            let Ok(b) = std::fs::read(dir.join(format!("{n:05}.ppm"))) else { break };
            out.push(fnv(&b));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&shot_path);
        out
    }

    #[test]
    fn a_shot_is_the_same_on_one_thread_and_eight() {
        let Some(pak) = pak_path() else { return };
        // A short flight on e1m1 with the slop preset, a monster room's
        // torches and lights running, and a demo's camera, at a small size.
        let fly = "map e1m1\nduration 0.25\nfps 24\nsize 320x180\nwarmup 0.2\ncamera path\n\
                   key 0 480,-352,110 0,90\nkey 0.25 520,-100,100 -5,80\n";
        let a = hashes(&pak, fly, 1, "fly");
        assert_eq!(a.len(), 6);
        assert_eq!(a, hashes(&pak, fly, 8, "fly"), "the same frames on 1 and 8 threads");
        assert!(a.windows(2).any(|w| w[0] != w[1]), "the camera moves");
        let demo = "demo demo1 from 2\nduration 0.2\nfps 30\nsize 320x200\npreset classic\nmode 320x200\n";
        assert_eq!(hashes(&pak, demo, 1, "demo"), hashes(&pak, demo, 8, "demo"));
        let xray = "map e1m1\nduration 0.1\nfps 20\nsize 320x180\nwarmup 0.1\nxray segments\nwire all\n\
                    camera fixed 480,-352,110 0,90\n";
        assert_eq!(hashes(&pak, xray, 1, "xray"), hashes(&pak, xray, 8, "xray"));
    }
}
