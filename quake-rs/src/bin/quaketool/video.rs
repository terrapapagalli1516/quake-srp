//! The video options `shot`, `view`, `play` and `timedemo` share: the port's video
//! cvars ([`VideoCvars`]: Hor+, hires, the fluid sky and the gliding light
//! styles), exact perspective (`wasm_exactpersp`; a renderer option, not one of
//! [`VideoCvars`]), the display the frame is shown on (which with the mode's
//! size gives `vid.aspect`), and the scaled 2-D layer.
//!
//! ```text
//! --video classic|modern   every cvar at once: id's, or the 2026 profile's: Hor+, hires,
//!                          the fluid sky, the gliding light styles, the flickering
//!                          torches and exact perspective (default classic)
//! --fov-mode classic|horplus  how `fov` meets the display's shape
//! --hires 0|1              views past 1280x1024, particles and the warp at 320x200 proportions
//! --sky classic|fluid      the clouds in id's whole-texel steps, or gliding (`r_fluidsky`)
//! --lightstyles classic|smooth  the animated lights in id's ten steps a second, or
//!                          gliding between them (`r_lerplightstyles`)
//! --torchflicker S         the steady torches flicker at strength S, 0 (id's) to 2
//!                          (`r_torchflicker`; 1 the flicker style's own swing)
//! --exactpersp 0|1         walls and liquids exact at every pixel, or id's 16-pixel
//!                          spans (`wasm_exactpersp`)
//! --display W:H|square     the display's width:height (square: the mode's own,
//!                          square pixels); the default is the command's
//! --scaled2d 0|1           the status bar, menus and console blown up from 320x200
//! --threads N              draw each frame's 3-D view on N threads (default 1;
//!                          the pixels are the same for any N)
//! ```

use quake_rs::render::{FovMode, SkyScroll, TorchFlicker, VideoCvars};
use quake_rs::server::LerpLightStyles;

/// The parsed video options (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct VideoArgs {
    pub cvars: VideoCvars,
    /// `--exactpersp`: [`RenderOptions::exact_perspective`](quake_rs::render::RenderOptions::exact_perspective),
    /// which sits beside [`VideoCvars`] in the frame's [`Vid`](quake_rs::client::Vid).
    pub exact_persp: bool,
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
                (self.cvars, self.exact_persp) = match val {
                    "classic" => (VideoCvars::CLASSIC, false),
                    "modern" => (VideoCvars::MODERN, true),
                    _ => return Err(format!("--video: expected classic or modern, got {val:?}")),
                }
            }
            "--exactpersp" => self.exact_persp = bit(val)?,
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
            "--lightstyles" => {
                self.cvars.lightstyles = match val {
                    "classic" => LerpLightStyles::Classic,
                    "smooth" => LerpLightStyles::Smooth,
                    _ => return Err(format!("--lightstyles: expected classic or smooth, got {val:?}")),
                }
            }
            "--torchflicker" => {
                let v: f32 = val.parse().map_err(|_| format!("--torchflicker: expected a strength 0..2, got {val:?}"))?;
                self.cvars.torches = TorchFlicker::from_value(v);
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

    /// A short tag for file names and reports: `classic`, `modern`, or the mix
    /// (a preset with the other perspective says so: `modern-spans`,
    /// `classic-exactpersp`).
    pub fn tag(&self) -> String {
        let exact = if self.exact_persp { "-exactpersp" } else { "" };
        match self.cvars {
            VideoCvars::CLASSIC => format!("classic{exact}"),
            VideoCvars::MODERN if self.exact_persp => "modern".into(),
            VideoCvars::MODERN => "modern-spans".into(),
            v => format!(
                "{}{}{}{exact}",
                if v.fov_mode == FovMode::HorPlus { "horplus" } else { "classicfov" },
                if v.hires { "-hires" } else { "" },
                if v.sky == SkyScroll::Fluid { "-fluidsky" } else { "" }
            ),
        }
    }
}

/// The options as `quaketool --help` lists them (the module docs say more).
pub const HELP: &[(&str, &str)] = &[
    ("--video classic|modern", "every cvar at once: id's, or the 2026 profile's: Hor+, hires, fluid sky, gliding lights, torches, exact perspective (default classic)"),
    ("--fov-mode classic|horplus", "how `fov` meets the display's shape"),
    ("--hires 0|1", "views past 1280x1024, particles and the warp at 320x200 proportions"),
    ("--sky classic|fluid", "the clouds in id's whole-texel steps, or gliding (`r_fluidsky`)"),
    ("--lightstyles classic|smooth", "the animated lights in id's ten steps a second, or gliding (`r_lerplightstyles`)"),
    ("--torchflicker S", "the steady torches flicker at strength S, 0 (id's) to 2 (`r_torchflicker`)"),
    ("--display W:H|square", "the display's width:height (square: the mode's own); the default is the command's"),
    ("--exactpersp 0|1", "walls and liquids exact at every pixel, or id's 16-pixel spans (`wasm_exactpersp`)"),
    ("--scaled2d 0|1", "the status bar, menus and console blown up from 320x200"),
    ("--threads N", "draw each frame's 3-D view on N threads (default 1; the pixels are the same for any N)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// `--video modern` is the whole 2026 set, exact perspective with the
    /// rest (it was id's spans until the user turned it on in 2026);
    /// `--video classic` is id's; `--exactpersp` moves it alone, in either
    /// order with `--video` the way every video option does.
    #[test]
    fn video_modern_carries_exact_perspective() {
        let mut v = VideoArgs::default();
        assert!(!v.exact_persp, "id's spans by default");
        assert_eq!(v.parse("--video", "modern"), Ok(true));
        assert_eq!((v.cvars, v.exact_persp, v.tag().as_str()), (VideoCvars::MODERN, true, "modern"));
        assert_eq!(v.parse("--exactpersp", "0"), Ok(true));
        assert_eq!((v.cvars, v.exact_persp, v.tag().as_str()), (VideoCvars::MODERN, false, "modern-spans"));
        assert_eq!(v.parse("--video", "classic"), Ok(true));
        assert_eq!((v.cvars, v.exact_persp, v.tag().as_str()), (VideoCvars::CLASSIC, false, "classic"));
        assert_eq!(v.parse("--exactpersp", "1"), Ok(true));
        assert_eq!(v.tag(), "classic-exactpersp");
        assert!(v.parse("--exactpersp", "2").is_err());
    }
}
