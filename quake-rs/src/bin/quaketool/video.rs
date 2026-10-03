//! The video options `shot`, `view`, `play` and `timedemo` share: the port's video
//! cvars ([`VideoCvars`]: Hor+, hires and the fluid sky), the display the frame
//! is shown on (which with the mode's size gives `vid.aspect`), and the scaled
//! 2-D layer.
//!
//! ```text
//! --video classic|modern   every cvar at once: id's, or Hor+, hires and the fluid sky (default classic)
//! --fov-mode classic|horplus  how `fov` meets the display's shape
//! --hires 0|1              views past 1280x1024, particles and the warp at 320x200 proportions
//! --sky classic|fluid      the clouds in id's whole-texel steps, or gliding (`r_fluidsky`)
//! --display W:H|square     the display's width:height (square: the mode's own,
//!                          square pixels); the default is the command's
//! --scaled2d 0|1           the status bar, menus and console blown up from 320x200
//! --threads N              draw each frame's 3-D view on N threads (default 1;
//!                          the pixels are the same for any N)
//! ```

use quake_rs::render::{FovMode, SkyScroll, VideoCvars};

/// The parsed video options (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct VideoArgs {
    pub cvars: VideoCvars,
    /// `--display`: `Some(None)` for `square`, `Some(Some(a))` for `W:H`.
    display: Option<Option<f64>>,
    scaled_2d: Option<bool>,
    /// `--threads`: the renderer's thread count (0: not given, 1).
    threads: usize,
}

impl VideoArgs {
    /// Take `flag val` if it is a video option: `Ok(true)` when it was.
    pub fn parse(&mut self, flag: &str, val: &str) -> Result<bool, String> {
        let bit = |v: &str| match v {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(format!("{flag}: expected 0 or 1, got {v:?}")),
        };
        match flag {
            "--video" => {
                self.cvars = match val {
                    "classic" => VideoCvars::CLASSIC,
                    "modern" => VideoCvars::MODERN,
                    _ => return Err(format!("--video: expected classic or modern, got {val:?}")),
                }
            }
            "--fov-mode" => {
                self.cvars.fov_mode = match val {
                    "classic" => FovMode::Classic,
                    "horplus" | "hor+" => FovMode::HorPlus,
                    _ => return Err(format!("--fov-mode: expected classic or horplus, got {val:?}")),
                }
            }
            "--hires" => self.cvars.hires = bit(val)?,
            "--sky" => {
                self.cvars.sky = match val {
                    "classic" => SkyScroll::Classic,
                    "fluid" => SkyScroll::Fluid,
                    _ => return Err(format!("--sky: expected classic or fluid, got {val:?}")),
                }
            }
            "--threads" => {
                self.threads = val.parse().ok().filter(|&n| n > 0).ok_or_else(|| format!("--threads: expected a count, got {val:?}"))?;
            }
            "--scaled2d" => self.scaled_2d = Some(bit(val)?),
            "--display" => {
                self.display = Some(if val == "square" {
                    None
                } else {
                    let (a, b) = val.split_once(':').ok_or_else(|| format!("--display: expected W:H or square, got {val:?}"))?;
                    let (a, b): (f64, f64) = (
                        a.trim().parse().map_err(|_| format!("--display: bad width {a:?}"))?,
                        b.trim().parse().map_err(|_| format!("--display: bad height {b:?}"))?,
                    );
                    if !(a > 0.0 && b > 0.0 && (a / b).is_finite()) {
                        return Err(format!("--display: {val:?} is not a shape"));
                    }
                    Some(a / b)
                })
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Set the scaled 2-D layer, if given, for this thread's frames. (The
    /// video cvars go to the frames themselves: [`Vid::video`](quake_rs::client::Vid::video),
    /// [`RenderOptions::video`](quake_rs::render::RenderOptions::video).)
    pub fn apply(&self) {
        if let Some(on) = self.scaled_2d {
            quake_rs::draw::set_scaled_2d(on);
        }
    }

    /// How many threads draw a frame (`--threads`, default 1).
    pub fn threads(&self) -> usize {
        self.threads.max(1)
    }

    /// The display aspect a `w x h` mode is shown at: `--display`, else
    /// `default` (`None`: square pixels, the mode's own shape).
    pub fn display_aspect(&self, w: usize, h: usize, default: Option<f64>) -> f64 {
        match self.display.unwrap_or(default) {
            Some(a) => a,
            None => w as f64 / h.max(1) as f64,
        }
    }

    /// A short tag for file names and reports: `classic`, `modern`, or the mix.
    pub fn tag(&self) -> String {
        match self.cvars {
            VideoCvars::CLASSIC => "classic".into(),
            VideoCvars::MODERN => "modern".into(),
            v => format!(
                "{}{}{}",
                if v.fov_mode == FovMode::HorPlus { "horplus" } else { "classicfov" },
                if v.hires { "-hires" } else { "" },
                if v.sky == SkyScroll::Fluid { "-fluidsky" } else { "" }
            ),
        }
    }
}

/// The options as `quaketool --help` lists them (the module docs say more).
pub const HELP: &[(&str, &str)] = &[
    ("--video classic|modern", "every cvar at once: id's, or Hor+, hires and the fluid sky (default classic)"),
    ("--fov-mode classic|horplus", "how `fov` meets the display's shape"),
    ("--hires 0|1", "views past 1280x1024, particles and the warp at 320x200 proportions"),
    ("--sky classic|fluid", "the clouds in id's whole-texel steps, or gliding (`r_fluidsky`)"),
    ("--display W:H|square", "the display's width:height (square: the mode's own); the default is the command's"),
    ("--scaled2d 0|1", "the status bar, menus and console blown up from 320x200"),
    ("--threads N", "draw each frame's 3-D view on N threads (default 1; the pixels are the same for any N)"),
];
