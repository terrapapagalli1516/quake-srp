//! `quaketool play <pak> <workload[,workload...]> [frames] [--res WxH[,WxH...]]
//! [--hash-every N] [--ppm PREFIX] [video options]` — the browser's game
//! client, run natively. The video options are `shot`'s (`video.rs`: `--video
//! modern` and friends; the display stays the page's 4:3).
//!
//! The same [`quake_rs::client`] frames the page runs (`walk_frame`,
//! `demo_frame`), driven the way `web/bench.py` drives the page, one host
//! frame per 1/72 s through `Host_FilterTime`, with the page's screen (its 4:3
//! display) and the Classic profile's settings (id's bindings and viewsize,
//! every departure off, the menu and console closed); each finished frame is presented
//! as the page presents it — through the `cl.cshifts` + gamma ramps into RGBA
//! (`VID_ShiftPalette`). Workloads, as in `web/bench.py`:
//!
//! | workload     | what |
//! |--------------|------|
//! | `walk_<map>` | the live walk on `maps/<map>.bsp`, `bench.py`'s scripted look-around + run |
//! | `fire_<map>` | the same with +attack held |
//! | `quad_<map>` | the same after `impulse 255` (id's QuadCheat): the Quad's cshift on |
//! | `demo1`..`demo3` | recorded-demo playback (the attract loop's next demo follows) |
//!
//! `--hash-every N` prints the FNV-1a hash `bench.py --hash-every N` prints for
//! the frames it runs (warmup included: frame 0, N, 2N, ...; 660 frames by
//! default, bench.py's 60 + 600), so the native client's frames can be
//! compared with the browser's byte for byte; `--ppm PREFIX` also writes those
//! frames as `PREFIX-<workload>-<W>x<H>-<frame>.ppm`. Each run ends with a
//! tally of the sound calls its frames made. `--trace PATH` writes, for each
//! demo frame, what `CL_RelinkEntities` drew — the clocks, the view angles,
//! the view entity, `cl.velocity` and every relinked entity — as the records
//! of the oracle's `oracle_trace` (`oracle/demo_lerp.py` compares the two).
//!
//! Several workloads and resolutions run in `bench.py`'s order (each workload
//! at each resolution) in one host, as the page runs them: the host hands
//! every server it runs its one [`QRand`], as id's servers all draw from one
//! libc `rand()`, so a run's shots and monsters depend on what ran before it.
//! Between runs the host takes `bench.py`'s realigning `step(0.2)` on the
//! previous game, and a walk workload boots e1m1 before the console's `map`
//! (the page's `boot()`). A demo falls its particles by the `sv_gravity` the
//! last game left, as the page's do.

use std::fmt::Write as _;
use std::rc::Rc;

use quake_rs::client::host::host_filter_time;
use quake_rs::client::{cl_demo, cl_input, cl_main, host_cmd, ClientFrame, DemoPlay, SoundCall, Vid, Walk};
use quake_rs::pak::Pak;
use quake_rs::qrand::QRand;
use quake_rs::render;
use quake_rs::settings::{Profile, Settings};

use super::video::VideoArgs;

/// The width:height ratio the browser page displays the frame at
/// (quake-wasm's `vid::DISPLAY_ASPECT`).
const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

/// Quake's frame cadence (`host_maxfps` 72): the step `bench.py` drives.
const DT: f32 = 1.0 / 72.0;

/// `bench.py`'s live-walk input for frame `f` (`walkInput` there, `walk_input`
/// in quake-wasm's bench.rs): a 720-frame cycle — a slow 360 degree sweep in
/// place, run forward 1 s, about-face, run back, about-face, idle. Returns
/// (forward fraction, yaw degrees to turn RIGHT this frame).
fn walk_input(f: u32) -> (f32, f32) {
    match f % 720 {
        0..=359 => (0.0, 1.0),
        360..=431 => (1.0, 0.0),
        432..=503 => (0.0, 2.5),
        504..=575 => (1.0, 0.0),
        576..=647 => (0.0, 2.5),
        _ => (0.0, 0.0),
    }
}

/// What the client is running.
enum Mode {
    Walk(Box<Walk>),
    Demo(Box<DemoPlay>),
}

/// A tally of the sound calls the frames made.
#[derive(Default)]
struct SoundTally {
    events: usize,
    stops: usize,
    stop_all: usize,
    static_loops: usize,
    updates: usize,
}

impl SoundTally {
    fn add(&mut self, calls: &[SoundCall]) {
        for c in calls {
            match c {
                SoundCall::Start { events, .. } => self.events += events.len(),
                SoundCall::Stop(s) => self.stops += s.len(),
                SoundCall::StopAll => self.stop_all += 1,
                SoundCall::Static(s) => self.static_loops += s.len(),
                SoundCall::Update { .. } => self.updates += 1,
                // The CD's calls are not the mixer's: the tally stays the
                // sound layer's.
                SoundCall::Cd(_) => {}
            }
        }
    }
}

/// FNV-1a over the little-endian 32-bit words of an RGBA buffer — `fnv` in
/// `web/bench.py`.
fn fnv(rgba: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for w in rgba.chunks_exact(4) {
        h = (h ^ u32::from_le_bytes([w[0], w[1], w[2], w[3]])).wrapping_mul(0x0100_0193);
    }
    h
}

/// The page's host, for as long as the runs last: its clocks, its settings
/// (Classic's) and the game it is running.
struct Host {
    pak: Pak,
    settings: Settings,
    keys: [bool; 256],
    /// `host_basepal`, the palette `VID_SetPalette` loads (`gfx/palette.lmp`).
    palette: render::Palette,
    gamma: [u8; 256],
    realtime: f64,
    oldrealtime: f64,
    mode: Option<Mode>,
    vid: Vid,
    /// The session's random streams, handed to every server it runs.
    rand: Rc<QRand>,
    /// The `sv_gravity` cvar as the last game left it, for the demos.
    sv_gravity: f32,
    /// The renderer threads each game draws with.
    threads: usize,
}

impl Host {
    /// One `step(raw_dt)`: `Host_FilterTime`'s gate, the bindings and cvars
    /// handed to the game, `CL_NextDemo` at a demo's end, the mode's frame.
    fn step(&mut self, raw_dt: f32, sound: &mut Vec<SoundCall>) -> Option<ClientFrame> {
        self.realtime += raw_dt as f64;
        let dt = host_filter_time(self.realtime, &mut self.oldrealtime)?;
        let s = &self.settings;
        let km = cl_input::derive_key_move(&s.cvars, &s.binds, &self.keys);
        let viewsize = s.cvars.viewsize;
        Some(match self.mode.as_mut()? {
            Mode::Walk(wk) => {
                wk.key_move = km;
                wk.viewsize = viewsize;
                cl_main::walk_frame(wk, dt, false, &self.vid)
            }
            Mode::Demo(d) => {
                d.viewsize = viewsize;
                d.show_scores = km.showscores;
                if d.at_end() {
                    if let Some(next) = cl_demo::build_demo_n(self.pak.clone(), d.demonum + 1, sound) {
                        let sv_gravity = d.sv_gravity;
                        **d = next;
                        d.renderer.set_threads(self.threads);
                        d.viewsize = viewsize;
                        d.show_scores = km.showscores;
                        d.sv_gravity = sv_gravity;
                    }
                }
                cl_demo::demo_frame(d, dt as f32, false, &self.vid)
            }
        })
    }

    /// Boot `workload` as `bench.py`'s `startWorkload` does.
    fn start(&mut self, workload: &str, sound: &mut Vec<SoundCall>) -> Result<(), String> {
        let is_walk = ["walk_", "fire_", "quad_"].iter().any(|p| workload.starts_with(p));
        if let Some(Mode::Walk(wk)) = &self.mode {
            self.sv_gravity = wk.server.sv_gravity();
        }
        self.mode = Some(if is_walk {
            // boot(): e1m1; then the console's `map <map>` for another map.
            let e1m1 = "maps/e1m1.bsp";
            let max_edicts = self.settings.cvars.max_edicts as usize;
            let mut walk = host_cmd::build_walk_map(self.pak.clone(), e1m1, &self.rand, sound, max_edicts)
                .ok_or_else(|| format!("{e1m1} would not load"))?;
            let map = format!("maps/{}.bsp", &workload[5..]);
            if map != e1m1 {
                walk = host_cmd::build_walk_map(self.pak.clone(), &map, &self.rand, sound, max_edicts)
                    .ok_or_else(|| format!("{map} would not load"))?;
            }
            if workload.starts_with("quad_") {
                // The console's `impulse 255` (id's QuadCheat).
                let mut out = Vec::new();
                host_cmd::run_game_command(&mut walk, "impulse", &["impulse", "255"], &mut out, sound);
            }
            walk.renderer.set_threads(self.threads);
            Mode::Walk(Box::new(walk))
        } else if let Some(n) = workload.strip_prefix("demo").and_then(|n| n.parse::<usize>().ok()) {
            let mut demo = cl_demo::build_demo_n(self.pak.clone(), n.max(1) - 1, sound)
                .ok_or_else(|| format!("{workload} would not load"))?;
            demo.sv_gravity = self.sv_gravity;
            demo.renderer.set_threads(self.threads);
            Mode::Demo(Box::new(demo))
        } else {
            return Err(format!("unknown workload {workload:?} (walk_<map>, fire_<map>, quad_<map>, demo1..3)"));
        });
        Ok(())
    }
}

pub fn cmd_play(pak_path: &str, workloads: &str, rest: &[String]) -> Result<String, String> {
    // Options: [frames] [--res WxH[,WxH...]] [--hash-every N] [--ppm PREFIX]
    // [--trace PATH] [video options].
    let (mut frames, mut res, mut every, mut ppm) = (660u32, "320x200".to_string(), 30u32, None);
    let mut trace: Option<String> = None;
    let mut video = VideoArgs::default();
    let mut i = 0;
    while i < rest.len() {
        let val = |i: usize| rest.get(i + 1).ok_or_else(|| format!("{} needs a value", rest[i]));
        if video.parse(&rest[i], val(i).map(String::as_str).unwrap_or(""))? {
            i += 2;
            continue;
        }
        match rest[i].as_str() {
            "--res" => {
                res = val(i)?.clone();
                i += 1;
            }
            "--hash-every" => {
                every = val(i)?.parse().map_err(|_| "--hash-every N")?;
                i += 1;
            }
            "--ppm" => {
                ppm = Some(val(i)?.clone());
                i += 1;
            }
            "--trace" => {
                trace = Some(val(i)?.clone());
                i += 1;
            }
            n => frames = n.parse().map_err(|_| format!("unknown argument {n:?}"))?,
        }
        i += 1;
    }
    let mut sizes = Vec::new();
    for r in res.split(',') {
        sizes.push(super::parse_res(r, video.cvars)?);
    }

    // The archive in memory, as the page embeds it.
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let palette = pak
        .read_file("gfx/palette.lmp")
        .ok()
        .flatten()
        .and_then(|b| render::parse_palette(&b))
        .ok_or("gfx/palette.lmp is missing or short")?;
    let vid = Vid { width: 0, height: 0, display_aspect: DISPLAY_ASPECT, exact_perspective: video.exact_persp, video: video.cvars, mip: render::MipCvars::DEFAULT };
    let mut host = Host {
        pak,
        settings: Settings::id(Profile::Classic), // id's controls by name: the oracle pins against them explicitly
        keys: [false; 256],
        palette,
        gamma: render::build_gamma_table(1.0),
        realtime: 0.0,
        oldrealtime: 0.0,
        mode: None,
        vid,
        rand: Rc::new(QRand::new()),
        sv_gravity: quake_rs::server::ServerCvars::default().sv_gravity,
        threads: video.threads(),
    };
    let mut rgba: Vec<u8> = Vec::new();
    let mut o = String::new();
    let (mut trace_out, mut traced) = (String::new(), 0usize);

    for workload in workloads.split(',') {
        let _ = writeln!(o, "{workload}");
        for &(w, h) in &sizes {
            let mut tally = SoundTally::default();
            let mut sound = Vec::new();
            // bench.py's realigning step(0.2), on the game the last run left.
            if host.mode.is_some() {
                if let Some(frame) = host.step(0.2, &mut sound) {
                    render::recycle_image(frame.image);
                }
                sound.clear();
            }
            host.start(workload, &mut sound)?;
            tally.add(&sound);
            host.vid = Vid { width: w, height: h, ..host.vid };
            let mut hashes = Vec::new();
            for f in 0..frames {
                // The page's exports for this frame: set_move / look / set_attack.
                if let Some(Mode::Walk(wk)) = host.mode.as_mut() {
                    let (fwd, turn) = walk_input(f);
                    (wk.in_fwd, wk.in_side) = (fwd, 0.0);
                    let (dyaw, dpitch) = (-turn, 0.0);
                    wk.yaw += dyaw;
                    wk.pitch = cl_input::clamp_pitch(wk.pitch + dpitch);
                    wk.in_attack = workload.starts_with("fire_");
                }
                let mut sound = Vec::new();
                let Some(frame) = host.step(DT, &mut sound) else { continue };
                if let (Some(_), Some(Mode::Demo(d))) = (&trace, host.mode.as_ref()) {
                    trace_frame(&mut trace_out, &mut traced, d);
                }
                tally.add(&sound);
                tally.add(&frame.sound);
                // V_UpdatePalette + VID_ShiftPalette: the frame's palette (its
                // cshifts, then gamma), which the 8-bit frame is shown through.
                let palette = render::FramePalette::new(&host.palette, &frame.cshifts, &host.gamma);
                render::pack_rgba(&frame.image, &palette, &mut rgba, host.threads);
                if every > 0 && f % every == 0 {
                    hashes.push(format!("{:08x}", fnv(&rgba)));
                    if let Some(prefix) = &ppm {
                        let mut out = format!("P6\n{} {}\n255\n", frame.image.w, frame.image.h).into_bytes();
                        for px in rgba.chunks_exact(4) {
                            out.extend_from_slice(&px[..3]);
                        }
                        let path = format!("{prefix}-{workload}-{w}x{h}-{f:04}.ppm");
                        std::fs::write(&path, out).map_err(|e| format!("cannot write {path}: {e}"))?;
                    }
                }
                render::recycle_image(frame.image);
            }
            if !hashes.is_empty() {
                let _ = writeln!(o, "  hashes {w}x{h}: {}", hashes.join(" "));
            }
            let t = &tally;
            let _ = writeln!(
                o,
                "  {w}x{h}: {frames} frames at 1/72 s; sound calls: {} S_StartSound, {} S_StopSound, {} S_StopAllSounds, {} S_StaticSound, {} S_Update",
                t.events, t.stops, t.stop_all, t.static_loops, t.updates
            );
        }
    }
    if let Some(path) = &trace {
        std::fs::write(path, &trace_out).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    Ok(o)
}

/// One `--trace` record, the `traced`-th: the frame's clocks, what
/// `CL_RelinkEntities` drew for the camera, and each relinked entity
/// (statics are never relinked).
fn trace_frame(out: &mut String, traced: &mut usize, d: &DemoPlay) {
    let f = &d.demo.frames[d.idx];
    let v = &d.view;
    let _ = writeln!(
        out,
        "F {} t={:.17} old={:.17} m0={} m1={} ang={} {} {} vorg={} {} {} vel={} {} {}",
        *traced, d.time, d.oldtime, f.time, f.prev_time, v.view_angles[0], v.view_angles[1], v.view_angles[2],
        v.view_entity_origin[0], v.view_entity_origin[1], v.view_entity_origin[2],
        v.velocity[0], v.velocity[1], v.velocity[2],
    );
    *traced += 1;
    for e in v.entities.iter().filter(|e| e.num >= 0) {
        let model = d.demo.model_precache.get(e.modelindex).map_or("", String::as_str);
        let _ = writeln!(
            out,
            "E {} {model} {} {} {} {} {} {} {}",
            e.num, e.origin[0], e.origin[1], e.origin[2], e.angles[0], e.angles[1], e.angles[2], e.frame
        );
    }
}
