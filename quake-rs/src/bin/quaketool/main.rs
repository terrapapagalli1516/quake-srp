//! `quaketool` — inspect Quake assets using the `quake_rs` loaders.
//!
//! ```text
//! quaketool info <file>          auto-detect format and print a summary
//! quaketool ls   <pak>           list a PAK archive's directory
//! quaketool cat  <pak> <name>    write one file from a PAK to stdout
//! quaketool bsp  <file.bsp>      dump BSP lump counts, model bounds, entity keys
//! quaketool map  <file.bsp>      render a top-down ASCII minimap from BSP geometry
//! quaketool mdl  <file.mdl>      dump an alias model's header, skins, frames
//! quaketool spr  <file.spr>      dump a sprite's header and frames
//! quaketool wad  <file.wad>      list a WAD2 archive's lumps
//! ```
//!
//! Output is buffered and written once, so piping into `head`/`less` (which
//! closes the pipe early) exits cleanly instead of panicking on `BrokenPipe`.

use std::io::Write as _;
use std::process::exit;

use quake_rs::render::VideoCvars;

mod assets;
mod census;
mod entities;
mod framerate;
mod play;
mod render;
mod sim;
mod sound;
mod timedemo;
mod video;

/// What a command produced: text to print, or raw bytes (for `cat`).
enum Out {
    Text(String),
    Bytes(Vec<u8>),
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
        exit(2);
    }
    let cmd = args[1].as_str();
    let rest = &args[2..];

    let result = match cmd {
        "info" => need(rest, 1, cmd).and_then(|a| assets::cmd_info(&a[0])),
        "ls" => need(rest, 1, cmd).and_then(|a| assets::cmd_ls(&a[0])),
        "cat" => need(rest, 2, cmd).and_then(|a| assets::cmd_cat(&a[0], &a[1])),
        "bsp" => need(rest, 1, cmd).and_then(|a| assets::cmd_bsp(&a[0])),
        "map" => need(rest, 1, cmd).and_then(|a| assets::cmd_map(&a[0])),
        "mdl" => need(rest, 1, cmd).and_then(|a| assets::cmd_mdl(&a[0])),
        "spr" => need(rest, 1, cmd).and_then(|a| assets::cmd_spr(&a[0])),
        "wad" => need(rest, 1, cmd).and_then(|a| assets::cmd_wad(&a[0])),
        "dis" => need(rest, 1, cmd).and_then(|a| assets::cmd_dis(&a[0])),
        "run" => need(rest, 2, cmd).and_then(|a| assets::cmd_run(&a[0], &a[1])),
        "render" => need(rest, 2, cmd).and_then(|a| render::cmd_render(&a[0], &a[1], a.get(2).map(|s| s.as_str()))),
        "render-demo" => need(rest, 1, cmd).and_then(|a| render::cmd_render_demo(&a[0])),
        "menu" => need(rest, 2, cmd).and_then(|a| render::cmd_menu(&a[0], &a[1])),
        "scene" => need(rest, 3, cmd).and_then(|a| render::scene::cmd_scene(&a[0], &a[1], &a[2], &a[3..])),
        "view" => need(rest, 3, cmd).and_then(render::view::cmd_view),
        "shot" => need(rest, 3, cmd).and_then(|a| render::shot::cmd_shot(a).map(Out::Text)),
        "walk" => need(rest, 3, cmd).and_then(|a| {
            sim::cmd_walk(&a[0], &a[1], &a[2], a.get(3).and_then(|s| s.parse().ok()).unwrap_or(40))
        }),
        "demo" => need(rest, 3, cmd).and_then(|a| {
            sim::cmd_demo(&a[0], &a[1], &a[2], a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0))
        }),
        "playtest" => need(rest, 2, cmd).and_then(|a| sim::playtest::cmd_playtest(&a[0], &a[1], a.get(2).map(|s| s.as_str()))),
        "simbench" => need(rest, 2, cmd).and_then(|a| {
            sim::cmd_simbench(&a[0], &a[1], a.get(2).and_then(|s| s.parse().ok()).unwrap_or(600))
        }),
        "changelevel" => need(rest, 2, cmd).and_then(|a| sim::changelevel::cmd_changelevel(&a[0], &a[1])),
        "census-edicts" => need(rest, 3, cmd).and_then(|a| census::cmd_census_edicts(&a[0], &a[1], &a[2]).map(Out::Text)),
        "census" => need(rest, 1, cmd).and_then(|a| census::cmd_census(&a[0], &a[1..]).map(Out::Text)),
        "play" => need(rest, 2, cmd).and_then(|a| play::cmd_play(&a[0], &a[1], &a[2..]).map(Out::Text)),
        "timedemo" => need(rest, 2, cmd).and_then(|a| timedemo::cmd_timedemo(&a[0], &a[1], &a[2..]).map(Out::Text)),
        "sound" => need(rest, 3, cmd).and_then(|a| sound::cmd_sound(&a[0], &a[1], &a[2], &a[3..]).map(Out::Text)),
        "sndscript" => need(rest, 3, cmd).and_then(|a| sound::cmd_sndscript(&a[0], &a[1], &a[2], &a[3..]).map(Out::Text)),
        "framerate" => need(rest, 1, cmd).and_then(|a| framerate::cmd_framerate(&a[0], &a[1..]).map(Out::Text)),
        "sim" => need(rest, 2, cmd).and_then(|a| {
            sim::cmd_sim(&a[0], &a[1], a.get(2).and_then(|s| s.parse().ok()).unwrap_or(5))
        }),
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        other => Err(format!("unknown command {other:?}")),
    };

    match result {
        Ok(out) => emit(out),
        Err(e) => {
            eprintln!("quaketool: {e}");
            exit(1);
        }
    }
}

/// Write the output once. A closed downstream pipe (`head`, `less`) is a clean
/// exit, not an error.
fn emit(out: Out) {
    let bytes = match &out {
        Out::Text(s) => s.as_bytes(),
        Out::Bytes(b) => b.as_slice(),
    };
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match lock.write_all(bytes).and_then(|_| lock.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => exit(0),
        Err(e) => {
            eprintln!("quaketool: write error: {e}");
            exit(1);
        }
    }
}

fn usage() {
    eprint!(
        "quaketool — inspect Quake assets\n\n\
         USAGE:\n\
         \tquaketool info <file>          auto-detect format and summarize\n\
         \tquaketool ls   <pak>           list a PAK directory\n\
         \tquaketool cat  <pak> <name>    extract one PAK file to stdout\n\
         \tquaketool bsp  <file.bsp>      dump BSP lump counts / models / entities\n\
         \tquaketool map  <file.bsp>      top-down ASCII minimap from BSP geometry\n\
         \tquaketool mdl  <file.mdl>      dump alias-model header/skins/frames\n\
         \tquaketool spr  <file.spr>      dump sprite header/frames\n\
         \tquaketool wad  <file.wad>      list WAD2 lumps\n\
         \tquaketool dis  <progs.dat>     disassemble QuakeC bytecode\n\
         \tquaketool run  <progs.dat> <fn>  execute a QuakeC function, show output + return\n\
         \tquaketool render <bsp> <out.ppm>  software-render a BSP to a PPM image\n\
         \tquaketool render-demo <out.ppm>   render the built-in demo room\n\
         \tquaketool menu <pak> <out.ppm>    draw the MAIN menu over the e1m1 POV\n\
         \tquaketool sim <progs.dat> <bsp> [frames]  spawn a map's QuakeC entities + tick physics\n\
         \tquaketool scene <pak> <map.bsp> <out.ppm> [--threads N]  render a map + its spawned MDL entities\n\
         \tquaketool view <pak> <map.bsp> <out.ppm> [--res WxH] [--origin x,y,z] [--angles p,y,r] [--time T] [--fov F] [--aspect A] [--exactpersp 0|1] [--vrect x,y,w,h] [--ents FILE] [--particles FILE] [--viewmodel M:F] [--viewent x,y,z,p,y,r] [--bench N] [--threads N]\n\
         \t                               render one exact view (Quake camera convention), for the C oracle diff\n\
         \tquaketool shot <pak> <map.bsp> <out.ppm> [--res WxH] [--zoom N] [--frames N] [--yaw Y] [--pitch P] [--origin x,y,z] [--viewsize V] [--fire N] [--video classic|modern] [--fov-mode classic|horplus] [--hires 0|1] [--display W:H|square] [--scaled2d 0|1] [--threads N]\n\
         \t                               the game screen as a player sees it (view, gun, status bar) at any size and video setting\n\
         \tquaketool walk <pak> <map.bsp> <out-prefix> [steps]  walk forward from spawn; one PPM frame per step\n\
         \tquaketool demo <pak> <demo.dem> <out-prefix> [stride]  replay + render a recorded demo\n\
         \tquaketool playtest <pak> <map.bsp> [out.ppm]  spawn a player, walk forward, report state + render POV\n\
         \tquaketool simbench <pak> <map.bsp> [frames]  benchmark the game-logic tick (physics/VM/AI/collision), no rendering\n\
         \tquaketool changelevel <pak> <map.bsp>  drive a player into the map's exit, swap to the next level, prove inventory carries\n\
         \tquaketool census <pak> [map ...]  headless faithfulness playthrough (start, e1m1..e1m8 by default)\n\
         \tquaketool census-edicts <pak> <map> <t1,t2,..>  dump live edicts at server times (oracle_edicts format)\n\
         \tquaketool play <pak> <walk_MAP|fire_MAP|quad_MAP|demoN> [frames] [--res WxH] [--hash-every N] [--ppm PREFIX] [--trace PATH] [video options] [--threads N]\n\
         \t                               run the browser's game client natively (quake_rs::client), frame hashes as web/bench.py\n\
         \tquaketool timedemo <pak> <demo> [--res WxH[,WxH...]] [--profile 1] [--video classic|modern] [--hires 0|1] [--fov-mode M] [--display W:H] [--scaled2d 0|1] [--threads N]  id's `timedemo`: the demo one message a frame, uncapped; prints CL_FinishTimeDemo's line\n\
         \tquaketool sound <pak> <demo> <out.wav> [--rate HZ] [--classic] [--fps F] [--trace FILE]  a demo's sound through the engine's mixer, as a .wav\n\
         \tquaketool sndscript <pak> <script> <out.raw> --rate HZ [--fixes] [--trace FILE]  a sound-oracle script through the port (oracle/sound.py)\n\
         \tquaketool framerate <pak> [--rates 60,144,240,480,jitter] [--only NAMES] [--markdown] [--check]\n\t                               play scripted scenarios at 72 Hz and each rate; how each quantity differs (FRAMERATE.md)\n"
    );
}

fn need<'a>(rest: &'a [String], n: usize, cmd: &str) -> Result<&'a [String], String> {
    if rest.len() < n {
        Err(format!("`{cmd}` needs {n} argument(s)"))
    } else {
        Ok(rest)
    }
}

/// A `--res WxH` screen size (`x` or `X`), never zero, at most the largest
/// view `video` allows — in Classic id's largest mode, `render::MAXWIDTH` x
/// `render::MAXHEIGHT` (1280 x 1024): a larger one is clamped with a note on
/// stderr, as id's drivers offer no such mode.
fn parse_res(r: &str, video: VideoCvars) -> Result<(usize, usize), String> {
    let (a, b) = r.split_once(['x', 'X']).ok_or_else(|| format!("--res: expected WxH, got {r:?}"))?;
    let w: usize = a.trim().parse().map_err(|_| format!("--res: bad width {a:?}"))?;
    let h: usize = b.trim().parse().map_err(|_| format!("--res: bad height {b:?}"))?;
    if w == 0 || h == 0 {
        return Err("--res: a zero-sized screen".into());
    }
    let (cw, ch) = video.clamp_to_max(w, h);
    if (cw, ch) != (w, h) {
        let (mw, mh) = video.max_view_size();
        eprintln!("--res {w}x{h}: the largest view is {mw}x{mh}; using {cw}x{ch}");
    }
    Ok((cw, ch))
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn res_is_at_most_id_largest_mode() {
        // r_shared.h's MAXWIDTH x MAXHEIGHT: `view --res 2048x400` panicked in
        // the edge renderer (its 12.20 u wraps from 2048 wide).
        let classic = |r| parse_res(r, VideoCvars::CLASSIC);
        assert_eq!(classic("640x400"), Ok((640, 400)));
        assert_eq!(classic("2048X400"), Ok((1280, 400)));
        assert_eq!(classic("320x2000"), Ok((320, 1024)));
        assert!(classic("0x200").is_err() && classic("320").is_err());
        // The hires extra lifts it to 8K.
        assert_eq!(parse_res("3840x2160", VideoCvars::MODERN), Ok((3840, 2160)));
    }
}
