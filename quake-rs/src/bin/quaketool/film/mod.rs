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
//! its sample `n` is heard at film second `n / rate` and a sound starts with
//! the frame that shows its cause, at any speed (`sound game` runs the mixer
//! on the game's clock and stretches it instead). `events.json`
//! ([`events`]) logs what the frames did: the sounds started, the muzzle
//! flashes, the monsters' poses.
//!
//! An `ab` shot is two takes ([`Take`]): the shot with each side's lines,
//! stepped together film frame by film frame — each its own game, clock and
//! sound — and composed into one frame (`split`), so an option on and off is
//! one render.

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
pub mod events;
pub mod png;
pub mod shot;
pub mod text;
pub mod xray;

use events::{EventLog, Obj};
use shot::{Action, CameraSpec, Clock, Corner, Player, Preset, Shot, SoundClock, Warmup, World, XrayBase};

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
                // `--sound`, or the shot line's `--sound on|game|off`.
                match val.map(String::as_str) {
                    Some(v @ ("on" | "off" | "game" | "film")) => {
                        shot.set(&format!("sound {v}"))?;
                        i += 2;
                    }
                    _ => {
                        shot.sound = true;
                        i += 1;
                    }
                }
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
    shot_cvars_at(shot, 0.0)
}

/// [`shot_cvars`] at film second `t`: with the timed `cvar ... at T` lines
/// due by then.
fn shot_cvars_at(shot: &Shot, t: f64) -> Cvars {
    let mut c = match shot.preset {
        Preset::Classic => Cvars::classic(),
        Preset::Slop => Cvars::slop(),
    };
    let timed = shot.timed.iter().filter(|(k, ..)| *k <= t).map(|(_, n, v)| (n, v));
    for (name, value) in shot.cvars.iter().map(|(n, v)| (n, v)).chain(timed) {
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
            w.draw_player = shot.body;
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

/// The palette index the lightmaps view lights: a light grey, so the
/// colormap's 64 shades run from it to black.
const XRAY_GREY: u8 = 9;

/// One render of a shot: its game, clocks, picture, sound and events. A shot
/// is one take, or with `ab` two, stepped side by side.
struct Take {
    shot: Shot,
    /// `""`, or `"a"` / `"b"` for an `ab` shot's sides.
    name: &'static str,
    c: Cvars,
    vid: Vid,
    aspect: f64,
    clock: Clock,
    stepping: Stepping,
    gamma: [u8; 256],
    palette: [[u8; 3]; 256],
    game: Game,
    path: Option<camera::Path>,
    /// The take's output frame size (an `ab` side's half, or the shot's).
    out: (usize, usize),
    /// The x-ray base picture is drawn (not `game`), the renderer captures.
    wants_base: bool,
    mixer: Option<Mixer>,
    /// The sound on the film's clock (with `sound game`, stretched from
    /// `game_pcm`, the mixer's on the game's clock).
    pcm: Vec<i16>,
    game_pcm: Vec<i16>,
    sound_start: i64,
    /// clock id: the 1/72 s ticks of game time run since film second 0.
    ticks: u64,
    last: Option<Vec<u8>>,
    drawn: usize,
    held: usize,
    clock_at_start: f64,
    log: Option<EventLog>,
}

impl Take {
    /// Build `shot`'s game, warm it up (undrawn: the map's monsters settle,
    /// or the demo reaches `from`) and draw one paused frame from the first
    /// camera, so the surface cache is warm at frame 0. `sound`: keep its
    /// sound; `events`: keep its log.
    fn new(
        pak: &Pak,
        mut shot: Shot,
        name: &'static str,
        out: (usize, usize),
        threads: usize,
        sound: bool,
        events: bool,
    ) -> Result<Take, String> {
        shot.size = out;
        let c = shot_cvars(&shot);
        quake_rs::draw::set_scaled_2d(c.scaled_2d);
        let (vid, aspect) = shot_vid(&shot, &c);
        let clock = shot.clock();
        let uncapped = shot.stepping.unwrap_or(clock == Clock::Free);
        let stepping = if uncapped { Stepping::Uncapped } else { Stepping::Classic };
        let mut calls = Vec::new();
        let mut game = build(&shot, pak, &c, &mut calls)?;
        game.renderer().set_threads(threads);
        let path = match &shot.camera {
            Some(CameraSpec::Path(keys)) => Some(camera::Path::new(keys, shot.fov)),
            _ => None,
        };
        let wants_base = shot.xray != XrayBase::Game;
        let capture = wants_base || shot.wire.world || shot.wire.entities || shot.divides.is_some();
        game.renderer().set_xray(Some(XrayOptions {
            capture,
            lightmaps: (shot.xray == XrayBase::Lightmaps).then_some(XRAY_GREY),
            vis_from: shot.vis,
            exact: shot.xray == XrayBase::PixelsOff,
        }));
        let palette = game.palette();
        // The sound: the preset's mixer, painted to each frame's end.
        let mut mixer: Option<Mixer> = sound.then(|| {
            let mut m = c.sound.mixer(pak, 48000);
            m.cvars.volume = c.volume;
            m
        });
        if let Some(m) = mixer.as_mut() {
            m.run(pak, &calls);
        }
        let mut log = events.then(EventLog::default);
        if let Some(log) = log.as_mut() {
            for call in &calls {
                if let SoundCall::Static(loops) = call {
                    for l in loops {
                        let e = Obj::kind("loop").num("t", 0.0).str("sample", &l.sample).vec("origin", &l.origin);
                        let e = e.num("volume", f64::from(l.volume)).num("attenuation", f64::from(l.attenuation));
                        log.events.push(take_tag(e, name).end());
                    }
                }
            }
        }
        let mut take = Take {
            shot,
            name,
            c,
            vid,
            aspect,
            clock,
            stepping,
            gamma: render::build_gamma_table(1.0),
            palette,
            game,
            path,
            out,
            wants_base,
            mixer,
            pcm: Vec::new(),
            game_pcm: Vec::new(),
            sound_start: 0,
            ticks: 0,
            last: None,
            drawn: 0,
            held: 0,
            clock_at_start: 0.0,
            log,
        };
        take.gamma = render::build_gamma_table(take.c.gamma);
        take.warm_up(pak);
        Ok(take)
    }

    fn warm_up(&mut self, pak: &Pak) {
        let shot = &self.shot;
        apply_settings(&mut self.game, shot, &self.c, Stepping::Classic);
        let mut warmed = 0.0;
        let done = |game: &Game, warmed: f64| match (&shot.world, shot.warmup, game) {
            (World::Demo { from, .. }, ..) => warmed + 1e-9 >= *from,
            (World::Map(_), Warmup::For(s), _) => warmed + 1e-9 >= s,
            (World::Map(_), Warmup::Until(t), Game::Walk(w)) => f64::from(w.clock) + 1e-6 >= t || warmed > 600.0,
            (World::Map(_), Warmup::Until(_), Game::Demo(_)) => true,
        };
        let first = camera_at(shot, self.path.as_ref(), 0.0);
        while !done(&self.game, warmed) {
            if let (Game::Walk(w), Player::Camera, Some(pose)) = (&mut self.game, shot.player, first) {
                set_origin(w, [pose.pos[0], pose.pos[1], pose.pos[2] - VIEWHEIGHT].map(|v| v as f32));
            }
            let frame = self.game.frame(TICK, &self.vid, false);
            warmed += TICK;
            if let Some(m) = self.mixer.as_mut() {
                // Mixed and dropped: the warm-up's sound is not the shot's.
                m.run(pak, &frame.sound);
                paint_to(m, (warmed * f64::from(m.rate())).round() as i64, &mut Vec::new());
            }
            render::recycle_image(frame.image);
        }
        self.set_camera(first);
        let frame = self.game.frame(0.0, &self.vid, true);
        render::recycle_image(frame.image);
        self.sound_start = self.mixer.as_ref().map_or(0, |m| m.painted_time());
        self.clock_at_start = self.game_time();
    }

    /// The game's clock (`cl.time`).
    fn game_time(&self) -> f64 {
        match &self.game {
            Game::Walk(w) => f64::from(w.clock),
            Game::Demo(d) => d.time,
        }
    }

    fn set_camera(&mut self, pose: Option<camera::Pose>) {
        match &mut self.game {
            Game::Walk(w) => {
                w.camera = pose.map(|p| p.camera());
                if let (Player::Camera, Some(p)) = (self.shot.player, pose) {
                    set_origin(w, [p.pos[0], p.pos[1], p.pos[2] - VIEWHEIGHT].map(|v| v as f32));
                }
            }
            Game::Demo(d) => d.camera = pose.map(|p| p.camera()),
        }
    }

    /// Film frame `n`: the shot's actions due, the host frames it takes (its
    /// picture into [`Take::last`] when `wanted`), its sound to the frame's
    /// end and its events.
    fn frame(&mut self, pak: &Pak, n: usize, wanted: bool) {
        quake_rs::draw::set_scaled_2d(self.c.scaled_2d);
        let fps = self.shot.fps;
        let t = n as f64 / fps;
        let prev = t - 1.0 / fps;
        let dt = self.shot.game_time(t) - self.shot.game_time(prev);
        if let Game::Walk(w) = &mut self.game {
            for &(at, when) in &self.shot.wake {
                if when <= t && (when > prev || n == 0) {
                    wake_monster(w, at);
                }
            }
            for action in self.shot.actions_due(prev, t, n == 0) {
                run_action(w, action);
            }
        }
        // This film frame's host frames, each with the film second its camera
        // is at: one per film frame (free); or id's ticks of game time up to
        // now (id), the last drawn and none if no tick is due, the picture
        // held; a frozen world is one paused frame, redrawn from the camera.
        let mut steps: Vec<(f64, f64)> = Vec::new();
        match self.clock {
            Clock::Free => steps.push((dt.min(0.1), t)),
            Clock::Id if dt <= 0.0 => steps.push((0.0, t)),
            Clock::Id => {
                let g = self.shot.game_time(t);
                while (self.ticks + 1) as f64 * TICK <= g + 1e-9 {
                    self.ticks += 1;
                    steps.push((TICK, self.shot.film_time(self.ticks as f64 * TICK)));
                }
                if steps.is_empty() && self.last.is_none() {
                    steps.push((0.0, t));
                }
            }
        }
        if steps.is_empty() {
            self.held += 1;
        }
        let count = steps.len();
        for (i, (step, tc)) in steps.into_iter().enumerate() {
            let draw = wanted && i + 1 == count;
            if !self.shot.timed.is_empty() {
                // The settings as the timed `cvar` lines have them by now.
                self.c = shot_cvars_at(&self.shot, tc);
                (self.vid, self.aspect) = shot_vid(&self.shot, &self.c);
                quake_rs::draw::set_scaled_2d(self.c.scaled_2d);
            }
            let mut pose = camera_at(&self.shot, self.path.as_ref(), tc);
            if let (true, Some(p)) = (self.shot.bob, pose.as_mut()) {
                // V_CalcBob from the path's own horizontal speed (game units a
                // game second), on the game's clock.
                let h = 0.5 / self.shot.fps;
                let (a, b) = (
                    camera_at(&self.shot, self.path.as_ref(), tc - h),
                    camera_at(&self.shot, self.path.as_ref(), tc + h),
                );
                let speed = self.shot.speed_at(tc);
                if let (Some(a), Some(b), true) = (a, b, speed > 0.0) {
                    let v = (b.pos[0] - a.pos[0]).hypot(b.pos[1] - a.pos[1]) / (2.0 * h) / speed;
                    p.pos[2] += f64::from(render::view_bob(v as f32, self.game_time() as f32));
                }
            }
            apply_settings(&mut self.game, &self.shot, &self.c, self.stepping);
            self.set_camera(pose);
            let frame = self.game.frame(step, &self.vid, draw);
            if let Some(m) = self.mixer.as_mut() {
                m.run(pak, &frame.sound);
            }
            if self.log.is_some() {
                self.log_events(n, t, &frame.sound);
            }
            if draw {
                let mut rgb = present(&frame, &self.palette, &self.gamma);
                if self.wants_base {
                    self.xray_base(&mut rgb, tc);
                }
                let (ow, oh) = self.out;
                let mut out = fit(&rgb, frame.image.w, frame.image.h, self.aspect, ow, oh);
                self.overlays(&mut out, (frame.image.w, frame.image.h), tc);
                self.last = Some(out);
                self.drawn += 1;
            }
            render::recycle_image(frame.image);
        }
        self.sound_to(n);
    }

    /// Paint the sound to the end of film frame `n`: sample `k` of
    /// [`Take::pcm`] is heard at film second `k / rate`.
    fn sound_to(&mut self, n: usize) {
        let Some(m) = self.mixer.as_mut() else { return };
        let rate = f64::from(m.rate());
        let film_end = ((n + 1) as f64 / self.shot.fps * rate).round() as i64;
        match self.shot.sound_clock {
            SoundClock::Film => paint_to(m, self.sound_start + film_end, &mut self.pcm),
            SoundClock::Game => {
                // The mixer on the game's clock, then each film sample read
                // from where the game's clock was at it (linear between pairs).
                let g_end = (self.shot.game_time((n + 1) as f64 / self.shot.fps) * rate).round() as i64;
                paint_to(m, self.sound_start + g_end, &mut self.game_pcm);
                let pairs = self.game_pcm.len() / 2;
                let mut j = (self.pcm.len() / 2) as i64;
                while j < film_end {
                    let g = self.shot.game_time(j as f64 / rate) * rate;
                    let i0 = (g.floor().max(0.0) as usize).min(pairs.saturating_sub(1));
                    let i1 = (i0 + 1).min(pairs.saturating_sub(1));
                    let f = (g - g.floor()).clamp(0.0, 1.0);
                    for ch in 0..2 {
                        let (a, b) = (
                            f64::from(self.game_pcm.get(2 * i0 + ch).copied().unwrap_or(0)),
                            f64::from(self.game_pcm.get(2 * i1 + ch).copied().unwrap_or(0)),
                        );
                        self.pcm.push((a + (b - a) * f).round() as i16);
                    }
                    j += 1;
                }
            }
        }
    }

    /// The camera this frame was drawn from: the shot's, the player's eye,
    /// or the demo's recorded view (the renderer's convention: pitch + up).
    fn view_camera(&self) -> render::Camera {
        match &self.game {
            Game::Walk(w) => w.camera.unwrap_or_else(|| {
                let (eye, a) = w.server.player_view();
                render::Camera { pos: eye, yaw: a[1], pitch: -a[0], roll: 0.0, fov_deg: 90.0 }
            }),
            Game::Demo(d) => d.camera.unwrap_or(render::Camera {
                pos: d.view.view_origin,
                yaw: d.view.view_angles[1],
                pitch: -d.view.view_angles[0],
                roll: d.view.view_angles[2],
                fov_deg: 90.0,
            }),
        }
    }

    /// Where the view lies on the take's output frame.
    fn place(&self, (sw, sh): (usize, usize)) -> xray::Place {
        let viewsize = if self.shot.hud { self.c.viewsize } else { 120.0 };
        let v = render::calc_refdef(self.vid.width, self.vid.height, viewsize, false, self.c.sbar_layout).vrect;
        let (ow, oh) = self.out;
        let (x0, y0, rw, rh) = fit_rect(self.aspect, ow, oh);
        xray::Place {
            vx: v.x as f32,
            vy: v.y as f32,
            x0: x0 as f32,
            y0: y0 as f32,
            kx: rw as f32 / sw as f32,
            ky: rh as f32 / sh as f32,
        }
    }

    /// The x-ray base picture over the 3-D view of `rgb` (the screen).
    fn xray_base(&mut self, rgb: &mut [u8], t: f64) {
        let viewsize = if self.shot.hud { self.c.viewsize } else { 120.0 };
        let v = render::calc_refdef(self.vid.width, self.vid.height, viewsize, false, self.c.sbar_layout).vrect;
        let strength = self.shot.mix_at(t);
        let (bsp, x) = match &self.game {
            Game::Walk(w) => (&w.bsp, w.renderer.xray_frame()),
            Game::Demo(d) => (&d.bsp, d.renderer.xray_frame()),
        };
        let Some(x) = x else { return };
        let tint = self.shot.tint;
        let tint =
            xray::TintRgb { colour: tint.colour.rgb(&self.palette), alpha: tint.alpha as f32, dim: tint.dim as f32 };
        let mut screen = xray::Rgb { w: self.vid.width, h: self.vid.height, px: rgb };
        xray::composite(&mut screen, (v.x, v.y), x, bsp, &self.palette, self.shot.xray, &tint, strength);
    }

    /// The wireframe and the divide marks over the output frame `out` (the
    /// screen `sw x sh` fitted into it).
    fn overlays(&self, out: &mut [u8], screen: (usize, usize), t: f64) {
        let shot = &self.shot;
        if !(shot.wire.world || shot.wire.entities || shot.divides.is_some()) {
            return;
        }
        let (bsp, x) = match &self.game {
            Game::Walk(w) => (&w.bsp, w.renderer.xray_frame()),
            Game::Demo(d) => (&d.bsp, d.renderer.xray_frame()),
        };
        let Some(x) = x else { return };
        let place = self.place(screen);
        let strength = shot.mix_at(t);
        if shot.wire.world || shot.wire.entities {
            xray::wire_over(out, self.out, place, x, bsp, &self.palette, shot.wire, strength);
        }
        if let Some(d) = shot.divides {
            let width = d.width.unwrap_or(self.out.1 as f64 / 720.0).max(0.25) as f32;
            let marks = xray::Marks { colour: d.colour.rgb(&self.palette), width, alpha: (d.alpha * strength) as f32 };
            xray::divides_over(out, self.out, place, x, &marks);
        }
    }

    /// What this host frame did, into the log: the sounds it started, the
    /// muzzle flashes, the monsters' poses on screen.
    fn log_events(&mut self, n: usize, t: f64, sound: &[SoundCall]) {
        let game_t = self.game_time();
        let cam = self.view_camera();
        let proj = Projector::new(&cam, &self.vid, self.aspect, self.out, self.shot.hud, &self.c);
        let stamp = |kind: &str| Obj::kind(kind).num("t", t).int("frame", n as i64).num("game_t", game_t);
        let mut events = Vec::new();
        // The sounds: where, how loud, and the channel the mixer gave each.
        let mut taken: Vec<usize> = Vec::new();
        for call in sound {
            let SoundCall::Start { events: starts, .. } = call else { continue };
            for s in starts {
                let mut e = stamp("sound")
                    .str("sample", &s.sample)
                    .int("entity", i64::from(s.entity))
                    .int("channel", i64::from(s.channel))
                    .vec("origin", &s.origin)
                    .num("volume", f64::from(s.volume))
                    .num("attenuation", f64::from(s.attenuation))
                    .num("dist", f64::from(proj.dist(s.origin)))
                    .num("pan", f64::from(proj.pan(s.origin)))
                    .opt_vec("screen", proj.screen(s.origin).as_ref().map(|p| &p[..]));
                if let Some(m) = &self.mixer {
                    let ch = m.channels().find(|c| {
                        c.pos == 0
                            && !taken.contains(&c.index)
                            && c.entnum == s.entity
                            && c.entchannel == s.channel
                            && c.sample == s.sample
                    });
                    if let Some(c) = ch {
                        taken.push(c.index);
                        e = e.int("left", i64::from(c.leftvol)).int("right", i64::from(c.rightvol));
                    }
                }
                events.push(take_tag(e, self.name).end());
            }
        }
        let log = self.log.as_mut().expect("logging");
        match &self.game {
            Game::Walk(w) => {
                let vm = &w.server.vm;
                for l in w.server.lit_entities().filter(|l| l.effects & quake_rs::server::EF_MUZZLEFLASH != 0) {
                    let e = stamp("flash")
                        .int("entity", i64::from(l.key))
                        .bool("player", l.key == w.player)
                        .vec("origin", &l.origin)
                        .opt_vec("screen", proj.screen(l.origin).as_ref().map(|p| &p[..]));
                    events.push(take_tag(e, self.name).end());
                }
                let sent = w.server.entities_sent_to_eye(cam.pos);
                for m in 1..vm.num_edicts() as i32 {
                    // A monster: FL_MONSTER, or a monster_ that never sets it (Chthon).
                    let monster = vm.ent_get_float(m, "flags") as i32 & FL_MONSTER != 0
                        || vm.ent_string_ref(m, "classname").starts_with("monster_");
                    if vm.is_free_edict(m) || !monster {
                        continue;
                    }
                    let origin = vm.ent_get_vector(m, "origin");
                    let on_screen = sent.get(m as usize).copied().unwrap_or(false) && proj.screen(origin).is_some();
                    let pose = vm.ent_get_float(m, "frame") as i32;
                    let model = vm.ent_string_ref(m, "model");
                    let mdl = w.model_cache.get(model).and_then(Option::as_ref);
                    if let Some(e) = pose_event(log, m, pose, on_screen, mdl, &stamp) {
                        let e = e
                            .str("class", vm.ent_string_ref(m, "classname"))
                            .str("model", model)
                            .vec("origin", &origin)
                            .opt_vec("screen", proj.screen(origin).as_ref().map(|p| &p[..]));
                        events.push(take_tag(e, self.name).end());
                    }
                }
            }
            Game::Demo(d) => {
                // A message's flashes once, in the frame that reads it.
                if log.demo_idx != Some(d.idx) {
                    log.demo_idx = Some(d.idx);
                    let f = &d.demo.frames[d.idx];
                    if f.view_effects & quake_rs::server::EF_MUZZLEFLASH != 0 {
                        let e =
                            stamp("flash").int("entity", -1).bool("player", true).vec("origin", &d.view.view_origin);
                        events.push(take_tag(e.null("screen"), self.name).end());
                    }
                    for en in d.view.entities.iter().filter(|e| e.num >= 0) {
                        if en.effects & quake_rs::server::EF_MUZZLEFLASH != 0 {
                            let e = stamp("flash")
                                .int("entity", i64::from(en.num))
                                .bool("player", false)
                                .vec("origin", &en.origin)
                                .opt_vec("screen", proj.screen(en.origin).as_ref().map(|p| &p[..]));
                            events.push(take_tag(e, self.name).end());
                        }
                    }
                }
                for en in d.view.entities.iter().filter(|e| e.num >= 0) {
                    let model = d.demo.model_precache.get(en.modelindex).map_or("", String::as_str);
                    if !events::MONSTER_MODELS.contains(&model) {
                        continue;
                    }
                    let mdl = d.models.get(en.modelindex).and_then(Option::as_ref);
                    let on_screen = proj.screen(en.origin).is_some();
                    if let Some(e) = pose_event(log, en.num, en.frame, on_screen, mdl, &stamp) {
                        let e = e
                            .str("class", model.trim_start_matches("progs/").trim_end_matches(".mdl"))
                            .str("model", model)
                            .vec("origin", &en.origin)
                            .opt_vec("screen", proj.screen(en.origin).as_ref().map(|p| &p[..]));
                        events.push(take_tag(e, self.name).end());
                    }
                }
            }
        }
        log.events.extend(events);
    }
}

/// A monster's pose this frame against the log's: the event of a change
/// seen on screen (or of its first pose there), or `None`.
fn pose_event(
    log: &mut EventLog,
    entity: i32,
    pose: i32,
    on_screen: bool,
    mdl: Option<&quake_rs::mdl::Mdl>,
    stamp: &dyn Fn(&str) -> Obj,
) -> Option<Obj> {
    if !on_screen {
        log.poses.remove(&entity);
        return None;
    }
    let from = log.poses.insert(entity, pose);
    if from == Some(pose) {
        return None;
    }
    let name = |f: i32| mdl.and_then(|m| events::frame_name(m, f));
    Some(
        stamp("pose")
            .int("entity", i64::from(entity))
            .int("pose", i64::from(pose))
            .opt_str("name", name(pose).as_deref())
            .opt_int("from", from.map(i64::from))
            .opt_str("from_name", from.and_then(name).as_deref()),
    )
}

/// An event of an `ab` side carries its side.
fn take_tag(e: Obj, name: &str) -> Obj {
    if name.is_empty() { e } else { e.str("take", name) }
}

/// A world point on a take's output frame, as the frame's camera projected
/// it (`R_ViewChanged`'s projection of the view, then the fit).
struct Projector {
    pos: [f32; 3],
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    /// The view's centre and scales, in output pixels.
    cx: f64,
    cy: f64,
    xscale: f64,
    yscale: f64,
    /// The view's rectangle on the output.
    rect: [f64; 4],
}

impl Projector {
    fn new(cam: &render::Camera, vid: &Vid, aspect: f64, out: (usize, usize), hud: bool, c: &Cvars) -> Projector {
        let viewsize = if hud { c.viewsize } else { 120.0 };
        let v = render::calc_refdef(vid.width, vid.height, viewsize, false, c.sbar_layout).vrect;
        let pixel_aspect = render::vid_aspect(vid.width, vid.height, vid.display_aspect);
        let fov_x = vid.video.fov_mode.fov_x(cam.fov_deg, vid.width, vid.height, pixel_aspect);
        let (x0, y0, rw, rh) = fit_rect(aspect, out.0, out.1);
        let (kx, ky) = (rw as f64 / vid.width as f64, rh as f64 / vid.height as f64);
        let xscale = (v.w as f64 / 2.0) / (f64::from(fov_x) * 0.5).to_radians().tan();
        let (y, p, r) =
            (f64::from(cam.yaw).to_radians(), f64::from(cam.pitch).to_radians(), f64::from(cam.roll).to_radians());
        let forward = [p.cos() * y.cos(), p.cos() * y.sin(), p.sin()];
        let right0 = [y.sin(), -y.cos(), 0.0];
        let up0 = [
            right0[1] * forward[2] - right0[2] * forward[1],
            right0[2] * forward[0] - right0[0] * forward[2],
            right0[0] * forward[1] - right0[1] * forward[0],
        ];
        let (sr, cr) = (r.sin(), r.cos());
        let right = [0, 1, 2].map(|k| right0[k] * cr - up0[k] * sr);
        let up = [0, 1, 2].map(|k| right0[k] * sr + up0[k] * cr);
        Projector {
            pos: cam.pos,
            forward,
            right,
            up,
            cx: x0 as f64 + (v.x as f64 + v.w as f64 / 2.0) * kx,
            cy: y0 as f64 + (v.y as f64 + v.h as f64 / 2.0) * ky,
            xscale: xscale * kx,
            yscale: xscale * f64::from(pixel_aspect) * ky,
            rect: [
                x0 as f64 + v.x as f64 * kx,
                y0 as f64 + v.y as f64 * ky,
                x0 as f64 + (v.x + v.w) as f64 * kx,
                y0 as f64 + (v.y + v.h) as f64 * ky,
            ],
        }
    }

    fn rel(&self, p: [f32; 3]) -> [f64; 3] {
        [0, 1, 2].map(|k| f64::from(p[k]) - f64::from(self.pos[k]))
    }

    fn dist(&self, p: [f32; 3]) -> f32 {
        let d = self.rel(p);
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() as f32
    }

    /// -1 (left of the camera) to 1 (right).
    fn pan(&self, p: [f32; 3]) -> f32 {
        let d = self.rel(p);
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len < 1e-6 {
            return 0.0;
        }
        ((d[0] * self.right[0] + d[1] * self.right[1] + d[2] * self.right[2]) / len) as f32
    }

    /// The point in output pixels, if it is in front of the camera and in
    /// the view.
    fn screen(&self, p: [f32; 3]) -> Option<[f32; 2]> {
        let d = self.rel(p);
        let dot = |v: [f64; 3]| d[0] * v[0] + d[1] * v[1] + d[2] * v[2];
        let z = dot(self.forward);
        if z < 1.0 {
            return None;
        }
        let (x, y) = (self.cx + self.xscale * dot(self.right) / z, self.cy - self.yscale * dot(self.up) / z);
        let [l, t, r, b] = self.rect;
        (x >= l && x < r && y >= t && y < b).then_some([x as f32, y as f32])
    }
}

/// Do `action` to the game (`Shot::actions`).
fn run_action(w: &mut Walk, action: &Action) {
    match action {
        Action::Impulse(i) => w.next_impulse = *i,
        Action::Attack(on) => w.in_attack = *on,
        Action::Fire(name) => fire_target(w, name),
        Action::Look(pitch, yaw) => (w.pitch, w.yaw) = (*pitch, *yaw),
        Action::Console(argv) => {
            let name = argv[0].as_str();
            let on = name.starts_with('+');
            match name.trim_start_matches(['+', '-']) {
                "attack" if name != "attack" => w.in_attack = on,
                "jump" if name != "jump" => w.in_jump = on,
                "movedown" if name != "movedown" => w.in_down = on,
                "forward" if name != "forward" => w.in_fwd = if on { 1.0 } else { 0.0 },
                "back" if name != "back" => w.in_fwd = if on { -1.0 } else { 0.0 },
                "moveright" if name != "moveright" => w.in_side = if on { 1.0 } else { 0.0 },
                "moveleft" if name != "moveleft" => w.in_side = if on { -1.0 } else { 0.0 },
                _ => {
                    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
                    let (mut said, mut sound) = (Vec::new(), Vec::new());
                    host_cmd::run_game_command(w, name, &argv, &mut said, &mut sound);
                }
            }
        }
    }
}

/// `SUB_UseTargets` for one name: each entity whose `targetname` is `name`
/// has its `use` run, the player its `activator` and `other` (as a trigger
/// the player touched fires its targets).
fn fire_target(w: &mut Walk, name: &str) {
    let p = w.player;
    let vm = &mut w.server.vm;
    let t = vm.sv_time() as f32;
    let targets: Vec<i32> = (1..vm.num_edicts() as i32)
        .filter(|&e| !vm.is_free_edict(e) && vm.ent_string_ref(e, "targetname") == name)
        .collect();
    for e in targets {
        let f = vm.ent_get_int(e, "use");
        if f <= 0 {
            continue;
        }
        vm.gset_int("self", e);
        vm.gset_int("other", p);
        vm.gset_int("activator", p);
        vm.gset_float("time", t);
        if vm.execute(f as usize).is_err() {
            vm.reset_execution();
        }
    }
}

/// Render the shot (see the module doc). Returns the report.
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
    let (ow, oh) = shot.size;
    // The takes: the shot, or its two sides, each at its part of the frame.
    let shots = shot.takes()?;
    let mut takes = Vec::new();
    let split = shot.ab.as_ref().map(|ab| ab.split);
    for (k, s) in shots.into_iter().enumerate() {
        let (name, out) = match split {
            None => ("", (ow, oh)),
            Some(split) => {
                let out = match split {
                    shot::Split::Side => (if k == 0 { ow / 2 } else { ow - ow / 2 }, oh),
                    shot::Split::Stack => (ow, if k == 0 { oh / 2 } else { oh - oh / 2 }),
                    _ => (ow, oh),
                };
                (["a", "b"][k], out)
            }
        };
        let sound = shot.sound
            && match shot.ab.as_ref().map(|ab| ab.sound) {
                None | Some(shot::AbSound::Both) => true,
                Some(shot::AbSound::A) => k == 0,
                Some(shot::AbSound::B) => k == 1,
            };
        takes.push(Take::new(&pak, s, name, out, threads, sound, shot.events)?);
    }
    let palette = takes[0].palette;
    let frames = shot.frames();
    let (first, end) = range.map_or((0, frames), |(a, b)| (a.min(frames), b.min(frames)));
    let mut stdout = std::io::stdout().lock();
    // Frames waiting for their files, encoded a batch at a time on the threads.
    let mut pending: Vec<(usize, Vec<u8>)> = Vec::new();
    let glyphs = wad(&pak).map(|w| text::Glyphs::new(&w, text::Font::Gold)).transpose()?;
    let t_frames = Instant::now();
    for n in 0..end {
        let wanted = n >= first;
        for take in &mut takes {
            take.frame(&pak, n, wanted);
            if n < first {
                take.pcm.clear();
            }
        }
        if !wanted {
            continue;
        }
        let t = n as f64 / shot.fps;
        let mut out = match (&shot.ab, takes.as_slice()) {
            (Some(ab), [a, b]) => match (&a.last, &b.last) {
                (Some(pa), Some(pb)) => compose_ab(shot, ab, (pa, a.out), (pb, b.out), t, &palette, glyphs.as_ref()),
                _ => continue,
            },
            (_, [take, ..]) => match &take.last {
                Some(p) => p.clone(),
                None => continue,
            },
            _ => continue,
        };
        if let (Some(g), Some(label)) = (&glyphs, &shot.label) {
            draw_label(g, &mut out, (ow, oh), &palette, label, shot.labelpos);
        }
        match format {
            Format::Raw => {
                if let Err(e) = stdout.write_all(&out) {
                    return Err(format!("stdout: {e}"));
                }
            }
            Format::Png | Format::Ppm => {
                pending.push((n, out));
                if pending.len() >= threads {
                    write_frames(&mut pending, out_dir, format, (ow, oh))?;
                }
            }
        }
    }
    write_frames(&mut pending, out_dir, format, (ow, oh))?;
    let _ = stdout.flush();
    let frames_s = t_frames.elapsed().as_secs_f64();
    let mut report = String::new();
    for take in &takes {
        let side = if take.name.is_empty() { String::new() } else { format!("side {}: ", take.name) };
        report += &format!(
            "film: {side}{} frames ({first}..{end} of {frames}) at {}x{} ({}x{} drawn, {}, clock {}), {} drawn, {} held, cl.time {:.3} at frame 0",
            end - first,
            take.out.0,
            take.out.1,
            take.vid.width,
            take.vid.height,
            if take.shot.preset == Preset::Classic { "Classic" } else { "slop" },
            if take.clock == Clock::Id { "id" } else { "free" },
            take.drawn,
            take.held,
            take.clock_at_start,
        );
        report += "\n";
    }
    report += &format!(
        "film: {:.1} s ({:.0} ms a frame), {:.1} s in all",
        frames_s,
        frames_s * 1000.0 / (end - first).max(1) as f64,
        started.elapsed().as_secs_f64()
    );
    // The sound: the take's, or an `ab` shot's chosen side's (and with
    // `absound both`, each side's too).
    let keep = match shot.ab.as_ref().map(|ab| ab.sound) {
        Some(shot::AbSound::A) => 0,
        _ => takes.len() - 1,
    };
    let mut rate = None;
    for (k, take) in takes.iter().enumerate() {
        let Some(m) = take.mixer.as_ref() else { continue };
        let mut names = Vec::new();
        if k == keep {
            names.push("sound.wav".to_string());
            rate = Some(m.rate());
        }
        if !take.name.is_empty() && shot.ab.as_ref().is_some_and(|ab| ab.sound == shot::AbSound::Both) {
            names.push(format!("sound-{}.wav", take.name));
        }
        for name in names {
            let path = format!("{out_dir}/{name}");
            std::fs::write(&path, wav_bytes(&take.pcm, m.rate())).map_err(|e| format!("cannot write {path}: {e}"))?;
            report +=
                &format!("; {path} at {} Hz, {:.2} s", m.rate(), take.pcm.len() as f64 / 2.0 / f64::from(m.rate()));
        }
    }
    if shot.events {
        let events: Vec<String> =
            takes.iter_mut().flat_map(|t| t.log.take().map(|l| l.events).unwrap_or_default()).collect();
        let header = [
            ("fps", format!("{}", shot.fps)),
            ("frames", format!("{frames}")),
            ("first", format!("{first}")),
            ("size", format!("[{ow}, {oh}]")),
            ("sound_rate", rate.map_or("null".into(), |r| r.to_string())),
            ("sound_clock", events::quote(if shot.sound_clock == SoundClock::Game { "game" } else { "film" })),
        ];
        let path = format!("{out_dir}/events.json");
        std::fs::write(&path, events::file(&header, &events)).map_err(|e| format!("cannot write {path}: {e}"))?;
        report += &format!("; {path}: {} events", events.len());
    }
    // With --raw, stdout is the frames': the report goes to stderr.
    if format == Format::Raw {
        eprintln!("{report}");
        return Ok(String::new());
    }
    Ok(report + "\n")
}

/// An `ab` shot's two pictures as one frame (`split`): cut by the moving
/// line, side by side, stacked, or A with B's differences tinted; each side
/// labelled.
fn compose_ab(
    shot: &Shot,
    ab: &shot::Ab,
    (a, aout): (&[u8], (usize, usize)),
    (b, bout): (&[u8], (usize, usize)),
    t: f64,
    palette: &[[u8; 3]; 256],
    glyphs: Option<&text::Glyphs>,
) -> Vec<u8> {
    let (w, h) = shot.size;
    let mut out = vec![0u8; w * h * 3];
    let bronze = palette[106];
    let line_w = (h / 270).max(2);
    let mut regions = [(0, 0, w, h); 2];
    match ab.split {
        shot::Split::Line => {
            let x = (shot.split_at(t) * w as f64).round() as usize;
            for y in 0..h {
                let row = y * w * 3;
                out[row..row + x * 3].copy_from_slice(&a[row..row + x * 3]);
                out[row + x * 3..row + w * 3].copy_from_slice(&b[row + x * 3..row + w * 3]);
                if x > 0 && x < w {
                    for px in x.saturating_sub(line_w / 2)..(x + line_w - line_w / 2).min(w) {
                        out[row + px * 3..row + px * 3 + 3].copy_from_slice(&bronze);
                    }
                }
            }
            regions = [(0, 0, x, h), (x, 0, w - x, h)];
        }
        shot::Split::Side | shot::Split::Stack => {
            let side = ab.split == shot::Split::Side;
            for (k, (px, (pw, ph))) in [(a, aout), (b, bout)].into_iter().enumerate() {
                let (x0, y0) = if side { (k * aout.0, 0) } else { (0, k * aout.1) };
                for y in 0..ph.min(h - y0) {
                    let src = &px[y * pw * 3..(y * pw + pw.min(w - x0)) * 3];
                    let at = ((y0 + y) * w + x0) * 3;
                    out[at..at + src.len()].copy_from_slice(src);
                }
                regions[k] = (x0, y0, pw, ph);
            }
            // The seam between them.
            let half = line_w / 2;
            if side {
                for y in 0..h {
                    for x in aout.0.saturating_sub(half)..(aout.0 + half).min(w) {
                        out[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&bronze);
                    }
                }
            } else {
                for y in aout.1.saturating_sub(half)..(aout.1 + half).min(h) {
                    for x in 0..w {
                        out[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&bronze);
                    }
                }
            }
        }
        shot::Split::Diff => {
            let lava = palette[235];
            out.copy_from_slice(a);
            for (o, (pa, pb)) in out.chunks_exact_mut(3).zip(a.chunks_exact(3).zip(b.chunks_exact(3))) {
                if pa != pb {
                    for k in 0..3 {
                        o[k] = (f32::from(pa[k]) * 0.4 + f32::from(lava[k]) * 0.6) as u8;
                    }
                }
            }
            regions = [(0, 0, w, h), (w, 0, 0, h)];
        }
    }
    if let Some(g) = glyphs {
        let scale = (h / 360).max(1);
        let margin = 8 * scale;
        for (k, label) in ab.labels.iter().enumerate() {
            let (Some(label), (rx, ry, rw, rh)) = (label, regions[k]) else { continue };
            if rw == 0 || rh == 0 {
                continue;
            }
            let (tw, _) = g.measure(label, scale);
            // A on the left of its region, B on the right of its (a line
            // split's sides meet in the middle).
            let x = if k == 0 || ab.split != shot::Split::Line {
                rx + margin
            } else {
                (rx + rw).saturating_sub(tw + margin)
            };
            g.draw(&mut out, w, 3, palette, label, (x as i64, (ry + margin) as i64), scale, 1);
        }
    }
    out
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

#[cfg(test)]
mod v2_tests {
    use super::*;

    fn pak_path() -> Option<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }

    /// A test's render directory, under `target/`, removed when done.
    struct Dir(std::path::PathBuf);

    impl std::ops::Deref for Dir {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Render `shot` into a fresh directory under `target/`; returns it.
    fn render(pak: &str, shot: &str, tag: &str, extra: &[&str]) -> Dir {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("target/film-v2-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot_path = dir.join("t.shot");
        std::fs::write(&shot_path, shot).unwrap();
        let mut args: Vec<String> =
            [pak, &shot_path.to_string_lossy(), &dir.to_string_lossy(), "--format", "ppm", "--threads", "4"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        cmd_film(&args).expect("the shot renders");
        Dir(dir)
    }

    /// `events.json`'s sound events: (film second, sample).
    fn sounds(dir: &std::path::Path) -> Vec<(f64, String)> {
        let text = std::fs::read_to_string(dir.join("events.json")).unwrap();
        text.lines()
            .filter(|l| l.contains("\"kind\": \"sound\""))
            .map(|l| {
                let field = |k: &str| {
                    let at = l.find(&format!("\"{k}\": ")).unwrap() + k.len() + 4;
                    l[at..].split([',', '}']).next().unwrap().trim().trim_matches('"').to_string()
                };
                (field("t").parse().unwrap(), field("sample"))
            })
            .collect()
    }

    /// The WAV's interleaved samples and rate.
    fn wav(path: &std::path::Path) -> (Vec<i16>, f64) {
        let b = std::fs::read(path).unwrap();
        let rate = u32::from_le_bytes(b[24..28].try_into().unwrap());
        (b[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(), f64::from(rate))
    }

    /// The first sample at or after film second `t` (to `t + within`) above
    /// `level`, as a film second.
    fn onset(pcm: &[i16], rate: f64, t: f64, within: f64, level: i16) -> Option<f64> {
        let (a, b) = ((t * rate) as usize, (((t + within) * rate) as usize).min(pcm.len() / 2));
        (a..b)
            .find(|&i| pcm[2 * i].unsigned_abs().max(pcm[2 * i + 1].unsigned_abs()) > level as u16)
            .map(|i| i as f64 / rate)
    }

    const F13: &str = "map e1m1\nduration 1.2\nfps 30\nsize 320x180\npreset slop\ngun on\n\
                       player 480,-352,88,90\ncamera player\nimpulse 9\nimpulse 4 at 0.1\nattack on at 0.5\n\
                       sound on\nevents on\n";

    #[test]
    fn commands_fire_the_nailgun_and_its_sounds_start_with_their_frames() {
        let Some(pak) = pak_path() else { return };
        // impulse 9 (every weapon), impulse 4 (the nailgun), +attack at 0.5 s:
        // nails every 0.1 s of game time from then on, each sound in the WAV
        // from the film second of the frame that shows it.
        let dir = render(&pak, F13, "f13", &[]);
        let nails: Vec<f64> =
            sounds(&dir).into_iter().filter(|(_, s)| s == "weapons/rocket1i.wav").map(|(t, _)| t).collect();
        assert!(nails.len() >= 6, "nails: {nails:?}");
        assert!(nails[0] >= 0.5 && nails[0] < 0.5 + 1.5 / 30.0, "the first nail with +attack: {}", nails[0]);
        let (pcm, rate) = wav(&dir.join("sound.wav"));
        assert_eq!(pcm.len() / 2, (1.2 * rate) as usize, "the film's length");
        assert_eq!(onset(&pcm, rate, 0.0, nails[0], 200), None, "silent before the first nail");
        let heard = onset(&pcm, rate, nails[0], 0.01, 200).expect("the first nail is heard at its frame");
        assert!(heard - nails[0] < 0.002, "{heard} vs {}", nails[0]);
        let gaps: Vec<f64> = nails.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| (0.06..0.14).contains(g)), "a nail every 0.1 s: {gaps:?}");

        // Half speed: the nails twice as far apart on the film's clock, each
        // still heard at its frame; with `sound game`, the sound slowed too.
        for clock in ["on", "game"] {
            let dir = render(&pak, F13, &format!("f13-slow-{clock}"), &["--speed", "0.5", "--sound", clock]);
            let slow: Vec<f64> =
                sounds(&dir).into_iter().filter(|(_, s)| s == "weapons/rocket1i.wav").map(|(t, _)| t).collect();
            let gaps: Vec<f64> = slow.windows(2).map(|w| w[1] - w[0]).collect();
            assert!(slow.len() >= 3 && gaps.iter().all(|g| (0.15..0.25).contains(g)), "{clock}: {gaps:?}");
            let (pcm, rate) = wav(&dir.join("sound.wav"));
            assert_eq!(pcm.len() / 2, (1.2 * rate) as usize, "{clock}: the film's length");
            assert_eq!(onset(&pcm, rate, 0.0, slow[0], 200), None, "{clock}: silent before the first nail");
            let heard = onset(&pcm, rate, slow[0], 0.02, 200).expect("heard");
            assert!(heard - slow[0] < 0.004, "{clock}: {heard} vs {}", slow[0]);
        }
    }

    #[test]
    fn fire_wakes_chthon_and_the_log_follows_his_poses() {
        let Some(pak) = pak_path() else { return };
        let shot = "map e1m7\nduration 1.5\nfps 10\nsize 320x180\nplayer -600,64,56,0\nfire t4 at 0.5\n\
                    camera fixed -100,64,200 15,0\nevents on\n";
        let dir = render(&pak, shot, "chthon", &[]);
        let s = sounds(&dir);
        let rise = s.iter().find(|(_, n)| n == "boss1/out1.wav").expect("Chthon rises");
        assert!((0.5..0.7).contains(&rise.0), "{rise:?}");
        let log = std::fs::read_to_string(dir.join("events.json")).unwrap();
        assert!(log.contains("\"class\": \"monster_boss\"") && log.contains("\"name\": \"rise1\""), "his poses");
    }

    #[test]
    fn an_ab_split_at_an_edge_is_that_side_alone() {
        let Some(pak) = pak_path() else { return };
        let base = "map e1m2\nduration 0.2\nfps 10\nsize 320x180\ncamera fixed 1488,1240,296 0,270\n";
        let ab = format!("{base}ab preset classic | preset slop;cvar r_torchflicker 2\n");
        let frame = |dir: &std::path::Path| std::fs::read(dir.join("00001.ppm")).unwrap();
        let b_alone = frame(&render(&pak, &format!("{base}cvar r_torchflicker 2\n"), "b", &[]));
        let a_alone = frame(&render(&pak, &format!("{base}preset classic\n"), "a", &[]));
        assert_eq!(frame(&render(&pak, &format!("{ab}splitat 0 0\n"), "ab0", &[])), b_alone, "all B");
        assert_eq!(frame(&render(&pak, &format!("{ab}splitat 0 1\n"), "ab1", &[])), a_alone, "all A");
        let half = frame(&render(&pak, &ab, "ab-half", &[]));
        assert!(half != a_alone && half != b_alone, "half of each");
        assert!(Shot::parse(&format!("{base}ab fps 30 | fps 60\n")).is_err(), "the sides share the film's clock");
    }

    #[test]
    fn a_timed_cvar_changes_the_picture_from_its_time_and_bob_moves_the_eye() {
        let Some(pak) = pak_path() else { return };
        let base = "map e1m6\nduration 0.3\nfps 10\nsize 320x180\ncamera fixed 504,500,242 0,96\n";
        let frames = |dir: &std::path::Path| -> Vec<Vec<u8>> {
            (0..3).map(|n| std::fs::read(dir.join(format!("{n:05}.ppm"))).unwrap()).collect()
        };
        let flip =
            frames(&render(&pak, &format!("{base}cvar r_perspspan 16\ncvar r_perspspan 1 at 0.15\n"), "flip", &[]));
        let exact = frames(&render(&pak, &format!("{base}cvar r_perspspan 1\n"), "exact1", &[]));
        assert_ne!(flip[0], exact[0], "16 before 0.15");
        assert_eq!(flip[2], exact[2], "exact from 0.15");
        let path = "map e1m1\nduration 0.3\nfps 10\nsize 320x180\ncamera path\nkey 0 480,-352,110 0,90\nkey 0.3 480,-250,110 0,90\n";
        let (still, bobbed) =
            (frames(&render(&pak, path, "nobob", &[])), frames(&render(&pak, &format!("{path}bob on\n"), "bob", &[])));
        assert!(still.iter().zip(&bobbed).skip(1).any(|(a, b)| a != b), "a running eye bobs");
        let s = Shot::parse(&format!("{base}cvar r_perspspan 16\ncvar r_perspspan 1 at 0.15\n")).unwrap();
        assert_eq!(s.timed, vec![(0.15, "r_perspspan".to_string(), "1".to_string())]);
        assert_eq!(shot_cvars_at(&s, 0.1).persp_span, render::PerspSpan::Spans16);
        assert_eq!(shot_cvars_at(&s, 0.2).persp_span, render::PerspSpan::Exact);
    }

    #[test]
    fn divides_and_pixels_off_draw_over_the_picture() {
        let Some(pak) = pak_path() else { return };
        let base = "map e1m6\nduration 0.1\nfps 10\nsize 320x180\ncamera fixed 504,500,242 0,96\ncvar r_perspspan 16\n";
        let frame = |dir: &std::path::Path| std::fs::read(dir.join("00000.ppm")).unwrap();
        let plain = frame(&render(&pak, base, "plain", &[]));
        let marked = frame(&render(&pak, &format!("{base}divides on ff0000 width 1 alpha 1\n"), "div", &[]));
        let off = frame(&render(&pak, &format!("{base}xray pixelsoff 00ff00 alpha 1\n"), "off", &[]));
        let count = |img: &[u8], c: [u8; 3]| img[15..].chunks_exact(3).filter(|p| **p == c).count();
        let pixels = 320 * 180;
        // A sixteenth of the walls' pixels, or so, is a divide at 16.
        let red = count(&marked, [255, 0, 0]);
        assert!(red > pixels / 40 && red < pixels / 4, "divides: {red} of {pixels}");
        let green = count(&off, [0, 255, 0]);
        assert!(green > 0 && green < pixels / 2, "pixels off: {green}");
        assert_eq!(count(&plain, [255, 0, 0]), 0);
        // Exact: none off.
        let exact =
            frame(&render(&pak, &format!("{base}cvar r_perspspan 1\nxray pixelsoff 00ff00 alpha 1\n"), "ex", &[]));
        assert_eq!(count(&exact, [0, 255, 0]), 0, "exact is exact");
    }
}
