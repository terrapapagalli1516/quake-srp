//! `quaketool timedemo <pak> <demo> [--res WxH[,WxH...]]` — id's `timedemo`
//! (`CL_TimeDemo_f`), natively: the recorded demo played as fast as the
//! client can draw it, one message per host frame with no 72 fps cap, then
//! `CL_FinishTimeDemo`'s line, `"%i frames %5.1f seconds %5.1f fps"`.
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

use std::fmt::Write as _;
use std::time::Instant;

use quake_rs::client::cl_demo::{self, TimeDemoClock};
use quake_rs::client::host::host_filter_time_uncapped;
use quake_rs::client::Vid;
use quake_rs::pak::Pak;
use quake_rs::render;

/// The width:height ratio the browser page displays the frame at
/// (quake-wasm's `vid::DISPLAY_ASPECT`).
const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

pub fn cmd_timedemo(pak_path: &str, demo: &str, rest: &[String]) -> Result<String, String> {
    let mut res = "320x200".to_string();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--res" => {
                res = rest.get(i + 1).ok_or("--res needs a value")?.clone();
                i += 1;
            }
            a => return Err(format!("unknown argument {a:?}")),
        }
        i += 1;
    }
    let mut sizes = Vec::new();
    for r in res.split(',') {
        let (a, b) = r.split_once('x').ok_or("--res WxH")?;
        let (w, h): (usize, usize) = (a.parse().map_err(|_| "--res WxH")?, b.parse().map_err(|_| "--res WxH")?);
        if w == 0 || h == 0 {
            return Err("--res: a zero-sized screen".into());
        }
        sizes.push((w, h));
    }

    // The archive in memory, as the page embeds it.
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    let gamma = render::build_gamma_table(1.0);
    let name = cl_demo::default_extension(demo, ".dem");
    let mut clock = TimeDemoClock::default();
    let mut o = String::new();
    let mut rgba: Vec<u8> = Vec::new();
    for (width, height) in sizes {
        // CL_PlayDemo_f, then CL_TimeDemo_f in host frame 0.
        let _ = writeln!(o, "Playing demo from {name}.");
        let mut sound = Vec::new();
        let Some(mut d) = cl_demo::build_timedemo(pak.clone(), &name, &mut sound) else {
            let _ = writeln!(o, "ERROR: couldn't open.");
            return Ok(o);
        };
        let vid = Vid { width, height, display_aspect: DISPLAY_ASPECT, exact_perspective: false };
        let mut host_framecount: i64 = 0;
        clock.start(host_framecount);
        let t0 = Instant::now();
        let mut oldrealtime = 0.0f64;
        let line = loop {
            // Host_FilterTime: realtime is the wall clock; no cap in a timedemo.
            let realtime = t0.elapsed().as_secs_f64();
            let frametime = host_filter_time_uncapped(realtime, &mut oldrealtime) as f32;
            clock.message(host_framecount, realtime);
            let Some(frame) = cl_demo::timedemo_frame(&mut d, frametime, false, &vid) else {
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
            host_framecount += 1;
        };
        let _ = writeln!(o, "{width}x{height}: {line}");
    }
    Ok(o)
}
