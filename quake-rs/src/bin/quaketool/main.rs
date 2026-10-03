//! `quaketool` — the `quake_rs` engine from the command line: id's files read
//! and printed, maps rendered, the game run headless, and the port's side of
//! the checks against id's C (`oracle/`, `census/`).
//!
//! Every command is one row of [`COMMANDS`]: its name, its arguments, a line
//! about it, and the function that runs it. The table drives both the
//! dispatch and `--help`, so a new command is one new row. The commands live
//! in modules by area:
//!
//! - [`assets`]: id's files (PAK, WAD2, BSP, MDL, SPR, `progs.dat`) printed;
//! - [`render`]: the software renderer into PPMs, the goldens (`scene`) and
//!   the oracle's views (`view`) among them;
//! - [`sim`]: the server run headless;
//! - [`census`], [`play`], [`timedemo`], [`sound`], [`framerate`]: the game as
//!   the page runs it, played headless and measured.
//!
//! [`video`] parses the video options several commands share, and
//! [`entities`] reads a map's entity lump without spawning it.
//!
//! A command returns what it printed ([`Out`]) or an error, boxed so that `?`
//! takes a [`QError`](quake_rs::QError) from the engine and a plain message
//! alike; `main` prints the error as `quaketool: <error>` and exits with 1.
//! Output is buffered and written once, so piping into `head`/`less` (which
//! closes the pipe early) exits cleanly instead of panicking on `BrokenPipe`.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::exit;
use std::str::FromStr;

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

/// A command's output, or why it failed.
type CmdResult = Result<Out, Box<dyn Error>>;

/// One command: a row of [`COMMANDS`].
struct Command {
    /// The name on the command line: `quaketool <name> ...`.
    name: &'static str,
    /// Its arguments as `--help` shows them: the `<required>` ones first,
    /// then the `[optional]` ones and the options.
    usage: &'static str,
    /// What it does, in a line.
    about: &'static str,
    /// Run it on the arguments after its name. There are at least
    /// [`Command::required_args`] of them (`main` refuses fewer), so it may
    /// index those without a check.
    run: fn(&[String]) -> CmdResult,
}

impl Command {
    /// How many arguments the command cannot run without: the `<...>` in its
    /// usage before the first `[`.
    fn required_args(&self) -> usize {
        let required = self.usage.split_once('[').map_or(self.usage, |(head, _)| head);
        required.matches('<').count()
    }
}

/// Every command, in `--help`'s order.
const COMMANDS: &[Command] = &[
    // assets.rs
    Command {
        name: "info",
        usage: "<file>",
        about: "auto-detect format (PAK, WAD2, BSP, MDL, SPR) and summarize",
        run: |a| assets::cmd_info(&a[0]),
    },
    Command {
        name: "ls",
        usage: "<pak>",
        about: "list a PAK directory",
        run: |a| assets::cmd_ls(&a[0]),
    },
    Command {
        name: "cat",
        usage: "<pak> <name>",
        about: "extract one PAK file to stdout",
        run: |a| assets::cmd_cat(&a[0], &a[1]),
    },
    Command {
        name: "bsp",
        usage: "<file.bsp>",
        about: "dump BSP lump counts / models / entities",
        run: |a| assets::cmd_bsp(&a[0]),
    },
    Command {
        name: "map",
        usage: "<file.bsp>",
        about: "top-down ASCII minimap from BSP geometry",
        run: |a| assets::cmd_map(&a[0]),
    },
    Command {
        name: "mdl",
        usage: "<file.mdl>",
        about: "dump alias-model header/skins/frames",
        run: |a| assets::cmd_mdl(&a[0]),
    },
    Command {
        name: "spr",
        usage: "<file.spr>",
        about: "dump sprite header/frames",
        run: |a| assets::cmd_spr(&a[0]),
    },
    Command {
        name: "wad",
        usage: "<file.wad>",
        about: "list WAD2 lumps",
        run: |a| assets::cmd_wad(&a[0]),
    },
    Command {
        name: "dis",
        usage: "<progs.dat>",
        about: "disassemble QuakeC bytecode",
        run: |a| assets::cmd_dis(&a[0]),
    },
    Command {
        name: "run",
        usage: "<progs.dat> <fn>",
        about: "execute a QuakeC function (no engine builtins), show output + return",
        run: |a| assets::cmd_run(&a[0], &a[1]),
    },
    // render.rs, render/
    Command {
        name: "render",
        usage: "<bsp> <out.ppm> [palette.lmp]",
        about: "software-render a BSP to a PPM image (flat-shaded; textured with a palette)",
        run: |a| render::cmd_render(&a[0], &a[1], a.get(2).map(String::as_str)),
    },
    Command {
        name: "render-demo",
        usage: "<out.ppm>",
        about: "render the built-in demo room",
        run: |a| render::cmd_render_demo(&a[0]),
    },
    Command {
        name: "menu",
        usage: "<pak> <out.ppm>",
        about: "draw the MAIN menu over the e1m1 POV",
        run: |a| render::cmd_menu(&a[0], &a[1]),
    },
    Command {
        name: "scene",
        usage: "<pak> <map.bsp> <out.ppm> [--threads N]",
        about: "render a map + its spawned MDL entities (the goldens)",
        run: |a| render::scene::cmd_scene(&a[0], &a[1], &a[2], &a[3..]),
    },
    Command {
        name: "view",
        usage: "<pak> <map.bsp> <out.ppm> [--res WxH] [--origin x,y,z] [--angles p,y,r] [--time T] [--fov F] \
                [--aspect A] [--vrect x,y,w,h] [--ents FILE] [--particles FILE] \
                [--dlight x,y,z,radius[,minlight]]... [--viewmodel M:F] [--viewent x,y,z,p,y,r] [--bench N] \
                [--d-mipscale X] [--d-mipcap N] [video options]",
        about: "render one exact view (Quake camera convention), for the C oracle diff",
        run: render::view::cmd_view,
    },
    Command {
        name: "shot",
        usage: "<pak> <map.bsp> <out.ppm> [--res WxH] [--zoom N] [--frames N] [--yaw Y] [--pitch P] \
                [--origin x,y,z | --in-liquid water|slime|lava] [--viewsize V] [--fire N] [--crosshair N] \
                [--sbaroverlay 0|1] [video options]",
        about: "the game screen as a player sees it (view, gun, status bar) at any size and video setting",
        run: |a| Ok(Out::Text(render::shot::cmd_shot(a)?)),
    },
    // sim.rs, sim/
    Command {
        name: "sim",
        usage: "<progs.dat> <bsp> [frames]",
        about: "spawn a map's QuakeC entities + tick physics",
        run: |a| sim::cmd_sim(&a[0], &a[1], parsed_or(a, 2, 5)),
    },
    Command {
        name: "simbench",
        usage: "<pak> <map.bsp> [frames]",
        about: "benchmark the game-logic tick (physics/VM/AI/collision), no rendering",
        run: |a| sim::cmd_simbench(&a[0], &a[1], parsed_or(a, 2, 600)),
    },
    Command {
        name: "playtest",
        usage: "<pak> <map.bsp> [out.ppm]",
        about: "spawn a player, walk forward, report state + render POV",
        run: |a| sim::playtest::cmd_playtest(&a[0], &a[1], a.get(2).map(String::as_str)),
    },
    Command {
        name: "changelevel",
        usage: "<pak> <map.bsp>",
        about: "drive a player into the map's exit, swap to the next level, prove inventory carries",
        run: |a| sim::changelevel::cmd_changelevel(&a[0], &a[1]),
    },
    Command {
        name: "walk",
        usage: "<pak> <map.bsp> <out-prefix> [steps]",
        about: "walk forward from spawn; one PPM frame per step",
        run: |a| sim::cmd_walk(&a[0], &a[1], &a[2], parsed_or(a, 3, 40)),
    },
    Command {
        name: "demo",
        usage: "<pak> <demo.dem> <out-prefix> [stride]",
        about: "replay + render a recorded demo",
        run: |a| sim::cmd_demo(&a[0], &a[1], &a[2], parsed_or(a, 3, 0)),
    },
    // census.rs
    Command {
        name: "census",
        usage: "<pak> [map ...]",
        about: "headless faithfulness playthrough (start, e1m1..e1m8 by default); <pak> may be a,b,c (the last searched first)",
        run: |a| Ok(Out::Text(census::cmd_census(&a[0], &a[1..])?)),
    },
    Command {
        name: "census-edicts",
        usage: "<pak> <map> <t1,t2,..>",
        about: "dump live edicts at server times (oracle_edicts format); <pak> may be a,b,c (the last searched first)",
        run: |a| Ok(Out::Text(census::cmd_census_edicts(&a[0], &a[1], &a[2])?)),
    },
    // play.rs, timedemo.rs
    Command {
        name: "play",
        usage: "<pak> <walk_MAP|fire_MAP|quad_MAP|demoN>[,...] [frames] [--res WxH[,WxH...]] [--hash-every N] \
                [--ppm PREFIX] [--trace PATH] [video options]",
        about: "run the browser's game client natively (quake_rs::client), frame hashes as web/bench.py",
        run: |a| Ok(Out::Text(play::cmd_play(&a[0], &a[1], &a[2..])?)),
    },
    Command {
        name: "timedemo",
        usage: "<pak> <demo> [--res WxH[,WxH...]] [--profile 1] [--lerpframe 1] [video options]",
        about: "id's `timedemo`: the demo one message a frame, uncapped; prints CL_FinishTimeDemo's line",
        run: |a| Ok(Out::Text(timedemo::cmd_timedemo(&a[0], &a[1], &a[2..])?)),
    },
    // sound.rs
    Command {
        name: "sound",
        usage: "<pak> <demo> <out.wav> [--rate HZ] [--classic] [--fps F] [--trace FILE]",
        about: "a demo's sound through the engine's mixer, as a .wav",
        run: |a| Ok(Out::Text(sound::cmd_sound(&a[0], &a[1], &a[2], &a[3..])?)),
    },
    Command {
        name: "sndscript",
        usage: "<pak> <script> <out.raw> --rate HZ [--fixes] [--trace FILE]",
        about: "a sound-oracle script through the port (oracle/sound.py)",
        run: |a| Ok(Out::Text(sound::cmd_sndscript(&a[0], &a[1], &a[2], &a[3..])?)),
    },
    Command {
        name: "sndwalk",
        usage: "<pak> <map> <script> <log> [--wav out.wav]",
        about: "a scripted walk through the Classic client, its sound calls logged (oracle/sound_walk.py)",
        run: |a| Ok(Out::Text(sound::cmd_sndwalk(&a[0], &a[1], &a[2], &a[3], &a[4..])?)),
    },
    // framerate.rs
    Command {
        name: "framerate",
        usage: "<pak> [--rates 60,144,240,480,jitter] [--only NAMES] [--markdown] [--check] \
                | --budget [--res WxH,...] | --lerpmove [--rates LIST] [--strip DIR] \
                | --lightstyles [--rates LIST] [--res WxH] [--threads N] [--reps N] [--secs S] [--view NAME=MAP:X,Y,Z:YAW]... \
                | --torchflicker S [the same] [--dump DIR [--strengths LIST]] \
                | --bake [--threads LIST] [the same]",
        about: "play scripted scenarios at 72 Hz and each rate; how each quantity differs (FRAMERATE.md)",
        run: |a| Ok(Out::Text(framerate::cmd_framerate(&a[0], &a[1..])?)),
    },
];

/// The column `--help` starts each command's `about` at: past the usage of
/// the short ones (`quaketool render-demo <out.ppm>`).
const ABOUT_COLUMN: usize = 33;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(name) = args.get(1) else {
        eprint!("{}", help());
        exit(2);
    };
    if matches!(name.as_str(), "-h" | "--help" | "help") {
        eprint!("{}", help());
        return;
    }
    match run(name, &args[2..]) {
        Ok(out) => emit(out),
        Err(e) => {
            eprintln!("quaketool: {e}");
            exit(1);
        }
    }
}

/// Run the command called `name` on `args`, the arguments after its name.
fn run(name: &str, args: &[String]) -> CmdResult {
    let cmd = COMMANDS.iter().find(|c| c.name == name).ok_or_else(|| format!("unknown command {name:?}"))?;
    let n = cmd.required_args();
    if args.len() < n {
        return Err(format!("`{name}` needs {n} argument(s)").into());
    }
    (cmd.run)(args)
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

/// `--help`: every row of [`COMMANDS`], then the video options.
fn help() -> String {
    let mut s = String::from("quaketool — Quake's files, renderer and game, from the command line\n\nUSAGE:\n");
    for c in COMMANDS {
        help_row(&mut s, &format!("quaketool {} {}", c.name, c.usage), c.about);
    }
    s.push_str("\nVIDEO OPTIONS (`[video options]` above):\n");
    for (option, about) in video::HELP {
        help_row(&mut s, option, about);
    }
    s
}

/// One entry of `--help`: `left`, then `about` at [`ABOUT_COLUMN`] — or on the
/// next line, when `left` runs past it.
fn help_row(s: &mut String, left: &str, about: &str) {
    if left.len() + 2 <= ABOUT_COLUMN {
        let _ = writeln!(s, "\t{left:ABOUT_COLUMN$}{about}");
    } else {
        let _ = writeln!(s, "\t{left}\n\t{:ABOUT_COLUMN$}{about}", "");
    }
}

/// The optional argument `a[i]` parsed, or `default` when it is missing or
/// does not parse (as `a.get(i)` then `parse().ok()` would have it).
fn parsed_or<T: FromStr>(a: &[String], i: usize, default: T) -> T {
    a.get(i).and_then(|s| s.parse().ok()).unwrap_or(default)
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

/// A whole file from disk.
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

    #[test]
    fn command_names_are_unique() {
        for (i, c) in COMMANDS.iter().enumerate() {
            assert!(COMMANDS[..i].iter().all(|d| d.name != c.name), "{} twice", c.name);
        }
    }

    #[test]
    fn required_args_are_the_usages_placeholders() {
        // What each command could not run without before the table (its
        // hand-written `need(rest, n, cmd)`), now read from its usage.
        let expected = [
            ("info", 1), ("ls", 1), ("cat", 2), ("bsp", 1), ("map", 1), ("mdl", 1), ("spr", 1), ("wad", 1),
            ("dis", 1), ("run", 2), ("render", 2), ("render-demo", 1), ("menu", 2), ("scene", 3), ("view", 3),
            ("shot", 3), ("sim", 2), ("simbench", 2), ("playtest", 2), ("changelevel", 2), ("walk", 3),
            ("demo", 3), ("census", 1), ("census-edicts", 3), ("play", 2), ("timedemo", 2), ("sound", 3),
            ("sndscript", 3), ("sndwalk", 4), ("framerate", 1),
        ];
        assert_eq!(COMMANDS.len(), expected.len());
        for (name, n) in expected {
            let c = COMMANDS.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no command {name}"));
            assert_eq!(c.required_args(), n, "{name}: {}", c.usage);
        }
    }

    #[test]
    fn a_missing_argument_is_an_error_not_a_panic() {
        let err = run("cat", &["pak0.pak".to_string()]).err().map(|e| e.to_string());
        assert_eq!(err.as_deref(), Some("`cat` needs 2 argument(s)"));
        let err = run("nope", &[]).err().map(|e| e.to_string());
        assert_eq!(err.as_deref(), Some("unknown command \"nope\""));
    }

    #[test]
    fn help_lists_every_command() {
        let help = help();
        for c in COMMANDS {
            assert!(help.contains(&format!("\tquaketool {} {}", c.name, c.usage)), "{}", c.name);
        }
    }
}
