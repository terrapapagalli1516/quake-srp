//! `quaketool timedemo <pak> <demo> [--res WxH[,WxH...]] [--profile 1]
//! [--lerpframe 1] [video options]` — id's `timedemo` (`CL_TimeDemo_f`), natively: the
//! recorded demo played as fast as the client can draw it, one message per
//! host frame with no 72 fps cap, then `CL_FinishTimeDemo`'s line,
//! `"%i frames %5.1f seconds %5.1f fps"`.
//!
//! The same [`quake_rs::client`] code the page runs for the console's
//! `timedemo` ([`cl_demo::build_timedemo`], [`cl_demo::timedemo_frame`],
//! [`cl_demo::TimeDemoClock`]), with the page's Classic screen (its 4:3
//! display, id's video) and the host's defaults (viewsize 100, the menu and console
//! closed — `key_dest == key_game`, as `quake +timedemo demo1` runs it).
//! `realtime` is the wall clock (`Sys_FloatTime`), read at the top of every
//! host frame. A host frame is what the page's `step` does for a frame: the
//! demo message, the 3-D view, the status bar and text, and the finished
//! frame packed through the palette-shift ramps into RGBA as the page's 2-D
//! canvas takes it (its WebGL2 path skips the pack); the sound calls are dropped (id's oracle runs with
//! `snd_null`). It prints the numbers id's C prints for `timedemo demo1` run
//! the same way (`oracle/build/quake-oracle -oracle_realtime -width W
//! -height H +timedemo demo1`, see PERF_PLAN.md).
//!
//! The video options (`video.rs`: `--video modern`, `--hires 1`, `--fov-mode
//! horplus`, `--display W:H`, `--scaled2d 1`) run it at 2026 sizes; the
//! display defaults to the page's 4:3. `--profile 1` then plays it a second
//! time with the client's frame timers and the render profiler on, and
//! prints where the time went, per frame: the host frame's phases (the
//! client's `lap`s: demo message and entities, 3-D view, post-3-D, 2-D, and
//! the RGBA pack) and the 3-D view's (the world walk to edges, the edge
//! scan, `D_DrawSurfaces` with the surface cache, alias models, particles,
//! sprites, the gun; with `--threads` above 1 these add every thread's time,
//! and the bands' wall time is printed beside them). The profiled run is a
//! little slower than the timed one.
//!
//! `--lerpframe 1` blends animation frames ([`LerpModels::Smooth`],
//! `r_lerpmodels`) instead of this command's own default, Classic (a
//! timedemo stays id's measure otherwise: `cl_demo::timedemo_frame`'s own
//! doc) — for measuring the extra's own cost (FRAMERATE.md, "Animation
//! frames blended").

use std::cell::RefCell;
use std::fmt::Write as _;
use std::time::Instant;

use quake_rs::client::cl_demo::{self, TimeDemoClock};
use quake_rs::client::host::host_filter_time_uncapped;
use quake_rs::client::lerpmodels::LerpModels;
use quake_rs::client::{set_lap_hook, Phase, Vid};
use quake_rs::pak::Pak;
use quake_rs::render;

use super::video::VideoArgs;

/// The width:height ratio the browser page displays the frame at
/// (quake-wasm's `vid::DISPLAY_ASPECT`).
const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

/// The client's frame phases this command times (`Phase` as an index).
const PHASES: usize = Phase::Pack as usize + 1;

thread_local! {
    /// The frame timer's state: the last lap, and the seconds per phase.
    static LAPS: RefCell<(Option<Instant>, [f64; PHASES])> = const { RefCell::new((None, [0.0; PHASES])) };
}

/// The lap hook: the time since the last lap belongs to `phase`.
fn lap_hook(phase: Phase) {
    LAPS.with(|l| {
        let mut l = l.borrow_mut();
        let now = Instant::now();
        if let Some(t) = l.0 {
            l.1[phase as usize] += (now - t).as_secs_f64();
        }
        l.0 = Some(now);
    });
}

/// What one timedemo run gives.
struct Run {
    /// `CL_FinishTimeDemo`'s line.
    line: String,
    frames: i64,
    /// What the render profiler counted, when it was on.
    stats: Option<render::RenderStats>,
    /// The surface cache at the end (bytes, blocks).
    cache: (usize, usize),
}

/// One timedemo of `name` at `vid`, drawn on `threads` threads, with the
/// render profiler on if `profile`.
#[allow(clippy::too_many_arguments)]
fn run(
    pak: &Pak,
    name: &str,
    vid: &Vid,
    threads: usize,
    clock: &mut TimeDemoClock,
    rgba: &mut Vec<u8>,
    profile: bool,
    lerpmodels: LerpModels,
) -> Option<Run> {
    let gamma = render::build_gamma_table(1.0);
    let mut sound = Vec::new();
    let mut d = cl_demo::build_timedemo(pak.clone(), name, &mut sound)?;
    d.renderer.set_threads(threads);
    if profile {
        d.renderer.stats_begin();
    }
    let mut host_framecount: i64 = 0;
    clock.start(host_framecount);
    let t0 = Instant::now();
    let mut oldrealtime = 0.0f64;
    let line = loop {
        // Host_FilterTime: realtime is the wall clock; no cap in a timedemo.
        let realtime = t0.elapsed().as_secs_f64();
        let frametime = host_filter_time_uncapped(realtime, &mut oldrealtime) as f32;
        clock.message(host_framecount, realtime);
        lap_hook(Phase::Input);
        // `--lerpframe` (off by default: a timedemo stays id's measure,
        // `cl_demo::timedemo_frame`'s own doc) lets this command measure
        // `r_lerpmodels`' own cost instead.
        let Some(frame) = cl_demo::timedemo_frame_lerpmodels(&mut d, frametime, false, vid, lerpmodels) else {
            break clock.finish(host_framecount, realtime);
        };
        // V_UpdatePalette + VID_ShiftPalette: the frame into RGBA through
        // its palette (the cshifts, then gamma), as the page's 2-D canvas
        // presents it.
        let palette = render::FramePalette::new(&d.palette, &frame.cshifts, &gamma);
        render::pack_rgba(&frame.image, &palette, rgba, threads);
        render::recycle_image(frame.image);
        lap_hook(Phase::Pack);
        host_framecount += 1;
    };
    let stats = profile.then(|| d.renderer.stats_end());
    Some(Run { line, frames: host_framecount, stats, cache: d.renderer.surface_cache_usage() })
}

pub fn cmd_timedemo(pak_path: &str, demo: &str, rest: &[String]) -> Result<String, String> {
    let mut res = "320x200".to_string();
    let mut video = VideoArgs::default();
    let mut profile = false;
    let mut lerpmodels = LerpModels::Classic;
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let val = rest.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        if !video.parse(flag, val)? {
            match flag {
                "--res" => res = val.clone(),
                "--profile" => profile = val == "1",
                "--lerpframe" => lerpmodels = if val == "1" { LerpModels::Smooth } else { LerpModels::Classic },
                a => return Err(format!("unknown argument {a:?}")),
            }
        }
        i += 2;
    }
    video.apply();
    let mut sizes = Vec::new();
    for r in res.split(',') {
        sizes.push(super::parse_res(r, video.cvars)?);
    }

    // The archive in memory, as the page embeds it.
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let name = cl_demo::default_extension(demo, ".dem");
    let mut clock = TimeDemoClock::default();
    let mut o = String::new();
    let mut rgba: Vec<u8> = Vec::new();
    for (width, height) in sizes {
        // CL_PlayDemo_f, then CL_TimeDemo_f in host frame 0.
        let _ = writeln!(o, "Playing demo from {name}.");
        let display_aspect = video.display_aspect(width, height, Some(DISPLAY_ASPECT));
        let vid = Vid { width, height, display_aspect, exact_perspective: false, video: video.cvars, mip: render::MipCvars::DEFAULT };
        let Some(timed) = run(&pak, &name, &vid, video.threads(), &mut clock, &mut rgba, false, lerpmodels) else {
            let _ = writeln!(o, "ERROR: couldn't open.");
            return Ok(o);
        };
        let _ = writeln!(o, "{width}x{height}: {}", timed.line);
        if profile {
            LAPS.with(|l| *l.borrow_mut() = (None, [0.0; PHASES]));
            set_lap_hook(Some(lap_hook));
            let t0 = Instant::now();
            let profiled = run(&pak, &name, &vid, video.threads(), &mut clock, &mut rgba, true, lerpmodels);
            let total = t0.elapsed().as_secs_f64();
            set_lap_hook(None);
            let (frames, st, (bytes, blocks)) = match profiled {
                Some(r) => (r.frames.max(1), r.stats.unwrap_or_default(), r.cache),
                None => (1, render::RenderStats::default(), (0, 0)),
            };
            let laps = LAPS.with(|l| l.borrow().1);
            let ms = |s: f64| s * 1000.0 / frames as f64;
            let ns = |n: u64| n as f64 / 1e6 / frames as f64;
            let _ = writeln!(
                o,
                "  profiled: {frames} frames, {:.3} ms/frame; host frame (ms): message {:.3}  sim {:.3}  3-D {:.3}  post-3-D {:.3}  2-D {:.3}  pack {:.3}",
                ms(total),
                ms(laps[Phase::Input as usize]),
                ms(laps[Phase::Sim as usize]),
                ms(laps[Phase::Render3d as usize]),
                ms(laps[Phase::Post3d as usize]),
                ms(laps[Phase::Hud2d as usize]),
                ms(laps[Phase::Pack as usize]),
            );
            let _ = writeln!(
                o,
                "  3-D (ms): walk {:.3}  brush ents {:.3}  scan {:.3}  surfaces {:.3}  alias {:.3}  particles {:.3}  sprites {:.3}  gun {:.3}",
                ns(st.world_sort_ns),
                ns(st.submodel_ns),
                ns(st.world_setup_ns),
                ns(st.world_surf_ns),
                ns(st.alias_ns),
                ns(st.particle_ns),
                ns(st.sprite_ns),
                ns(st.viewmodel_ns),
            );
            let _ = writeln!(
                o,
                "  bands: {:.3} ms wall per frame on {} thread(s) (the 3-D times above add every thread's)",
                ns(st.bands_ns),
                st.band_threads / frames as u64,
            );
            let per = |n: u64| n / frames as u64;
            let _ = writeln!(
                o,
                "  per frame: {} px, {} surfaces drawn, {} edges (peak {}), {} surfs (peak {}), {} spans; {} texels baked into the surface cache",
                per(st.world_pixels),
                per(st.faces_drawn),
                per(st.edges_emitted),
                st.edges_peak,
                per(st.surfs_emitted),
                st.surfs_peak,
                per(st.spans_emitted),
                per(st.surf_texels_baked),
            );
            // The background (r_clearcolor) where no surface covers the view:
            // cracks and sparkles, or the eye outside the world. (demo1 has no
            // intermission and no underwater frame, so every frame's view is
            // the viewsize-100 rectangle, in id's layout: this command does not
            // put the status bar over the view.)
            let vrect = render::calc_refdef(width, height, render::VIEWSIZE_DEFAULT, false, render::SbarLayout::Classic).vrect;
            let background = (frames as u64 * (vrect.w * vrect.h) as u64).saturating_sub(st.world_pixels);
            let _ = writeln!(o, "  background pixels in all {frames} frames (if every frame is the viewsize-100 view): {background}");
            let _ = writeln!(o, "  surface cache at the end: {:.1} MB in {blocks} blocks", bytes as f64 / 1e6);
        }
    }
    Ok(o)
}
