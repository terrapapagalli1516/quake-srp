//! `quaketool timedemo <pak> <demo> [--res WxH[,WxH...]] [--profile 1]
//! [video options]` — id's `timedemo` (`CL_TimeDemo_f`), natively: the
//! recorded demo played as fast as the client can draw it, one message per
//! host frame with no 72 fps cap, then `CL_FinishTimeDemo`'s line,
//! `"%i frames %5.1f seconds %5.1f fps"`.
//!
//! The same [`quake_rs::client`] code the page runs for the console's
//! `timedemo` ([`cl_demo::build_timedemo`], [`cl_demo::timedemo_frame`],
//! [`cl_demo::TimeDemoClock`]), with the page's screen (its 4:3 display, no
//! Web extras) and the host's defaults (viewsize 100, the menu and console
//! closed — `key_dest == key_game`, as `quake +timedemo demo1` runs it).
//! `realtime` is the wall clock (`Sys_FloatTime`), read at the top of every
//! host frame. A host frame is what the page's `step` does for a frame: the
//! demo message, the 3-D view, the status bar and text, and the finished
//! frame packed through the palette-shift ramps into RGBA as the page
//! presents it; the sound calls are dropped (id's oracle runs with
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
//! sprites, the gun). The profiled run is a little slower than the timed one.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::time::Instant;

use quake_rs::client::cl_demo::{self, TimeDemoClock};
use quake_rs::client::host::host_filter_time_uncapped;
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

/// One timedemo of `name` at `vid`: `CL_FinishTimeDemo`'s line and the frame count.
fn run(pak: &Pak, name: &str, vid: &Vid, clock: &mut TimeDemoClock, rgba: &mut Vec<u8>) -> Option<(String, i64)> {
    let gamma = render::build_gamma_table(1.0);
    let mut sound = Vec::new();
    let mut d = cl_demo::build_timedemo(pak.clone(), name, &mut sound)?;
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
        let Some(frame) = cl_demo::timedemo_frame(&mut d, frametime, false, vid) else {
            break clock.finish(host_framecount, realtime);
        };
        // V_UpdatePalette + VID_ShiftPalette: the frame into RGBA through
        // the cshift ramps (a plain copy without any), as the page packs it.
        let ramps = (!frame.cshifts.is_empty()).then(|| render::cshift_ramps(&frame.cshifts, &gamma));
        rgba.resize(frame.image.rgb.len() * 4, 255);
        for (out, px) in rgba.chunks_exact_mut(4).zip(&frame.image.rgb) {
            let c = match &ramps {
                Some([r, g, b]) => [r[px[0] as usize], g[px[1] as usize], b[px[2] as usize]],
                None => *px,
            };
            out[..3].copy_from_slice(&c);
        }
        render::recycle_image(frame.image);
        lap_hook(Phase::Pack);
        host_framecount += 1;
    };
    Some((line, host_framecount))
}

pub fn cmd_timedemo(pak_path: &str, demo: &str, rest: &[String]) -> Result<String, String> {
    let mut res = "320x200".to_string();
    let mut video = VideoArgs::default();
    let mut profile = false;
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let val = rest.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        if !video.parse(flag, val)? {
            match flag {
                "--res" => res = val.clone(),
                "--profile" => profile = val == "1",
                a => return Err(format!("unknown argument {a:?}")),
            }
        }
        i += 2;
    }
    video.apply();
    let mut sizes = Vec::new();
    for r in res.split(',') {
        sizes.push(super::parse_res(r)?);
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
        let vid = Vid { width, height, display_aspect, exact_perspective: false };
        let Some((line, _)) = run(&pak, &name, &vid, &mut clock, &mut rgba) else {
            let _ = writeln!(o, "ERROR: couldn't open.");
            return Ok(o);
        };
        let _ = writeln!(o, "{width}x{height}: {line}");
        if profile {
            LAPS.with(|l| *l.borrow_mut() = (None, [0.0; PHASES]));
            set_lap_hook(Some(lap_hook));
            render::render_stats_begin();
            let t0 = Instant::now();
            let frames = run(&pak, &name, &vid, &mut clock, &mut rgba).map_or(1, |(_, n)| n.max(1));
            let total = t0.elapsed().as_secs_f64();
            let st = render::render_stats_end();
            set_lap_hook(None);
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
            let (bytes, blocks) = render::surface_cache_usage();
            let _ = writeln!(o, "  surface cache at the end: {:.1} MB in {blocks} blocks", bytes as f64 / 1e6);
        }
    }
    Ok(o)
}
