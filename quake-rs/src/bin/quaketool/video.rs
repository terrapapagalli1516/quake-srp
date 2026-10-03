//! The video options `shot`, `view`, `play` and `timedemo` share: the port's video
//! cvars ([`VideoCvars`]: Hor+, hires, the fluid sky and the gliding light
//! styles), the perspective span (`r_perspspan`; a renderer option, not one of
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
//! --perspspan 64|32|16|8|4|1  walls and liquids exact every 16 pixels (id's
//!                          `D_DrawSpans16`), 64, 32, 8 (id's C `D_DrawSpans8`; 64,
//!                          32 and 4 its arithmetic), or at every pixel (`r_perspspan`)
//! --exactpersp 0|1         the same as --perspspan 16 or 1 (the option before the span)
//! --display W:H|square     the display's width:height (square: the mode's own,
//!                          square pixels); the default is the command's
//! --scaled2d 0|1           the status bar, menus and console blown up from 320x200
//! --threads N              draw each frame's 3-D view on N threads (default 1;
//!                          the pixels are the same for any N)
//! ```

use quake_rs::render::{FovMode, PerspSpan, SkyScroll, TorchFlicker, VideoCvars};
use quake_rs::server::LerpLightStyles;

/// The parsed video options (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct VideoArgs {
    pub cvars: VideoCvars,
    /// `--perspspan`: [`RenderOptions::persp_span`](quake_rs::render::RenderOptions::persp_span),
    /// which sits beside [`VideoCvars`] in the frame's [`Vid`](quake_rs::client::Vid).
    pub persp_span: PerspSpan,
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
                (self.cvars, self.persp_span) = match val {
                    "classic" => (VideoCvars::CLASSIC, PerspSpan::Spans16),
                    "modern" => (VideoCvars::MODERN, PerspSpan::Exact),
                    _ => return Err(format!("--video: expected classic or modern, got {val:?}")),
                }
            }
            "--perspspan" => self.persp_span = parse_span(val)?,
            "--exactpersp" => self.persp_span = if bit(val)? { PerspSpan::Exact } else { PerspSpan::Spans16 },
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
    /// (a preset with another perspective says so: `modern-spans` for id's
    /// 16, `modern-span8`, `classic-span64`, `classic-exactpersp`).
    pub fn tag(&self) -> String {
        let span = format!("-span{}", self.persp_span.pixels());
        let exact = match self.persp_span {
            PerspSpan::Spans16 => "",
            PerspSpan::Exact => "-exactpersp",
            _ => &span,
        };
        match self.cvars {
            VideoCvars::CLASSIC => format!("classic{exact}"),
            VideoCvars::MODERN => match self.persp_span {
                PerspSpan::Exact => "modern".into(),
                PerspSpan::Spans16 => "modern-spans".into(),
                _ => format!("modern{exact}"),
            },
            v => format!(
                "{}{}{}{exact}",
                if v.fov_mode == FovMode::HorPlus { "horplus" } else { "classicfov" },
                if v.hires { "-hires" } else { "" },
                if v.sky == SkyScroll::Fluid { "-fluidsky" } else { "" }
            ),
        }
    }
}

/// `--perspspan`'s value: exactly 64, 32, 16, 8, 4 or 1.
pub fn parse_span(val: &str) -> Result<PerspSpan, String> {
    PerspSpan::ALL
        .into_iter()
        .find(|p| val == p.pixels().to_string())
        .ok_or_else(|| format!("--perspspan: expected 64, 32, 16, 8, 4 or 1, got {val:?}"))
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
    ("--perspspan 64|32|16|8|4|1", "walls and liquids exact every 16 pixels (id's), 64, 32, 8 (id's C), 4, or every pixel (`r_perspspan`)"),
    ("--exactpersp 0|1", "the same as --perspspan 16 or 1"),
    ("--scaled2d 0|1", "the status bar, menus and console blown up from 320x200"),
    ("--threads N", "draw each frame's 3-D view on N threads (default 1; the pixels are the same for any N)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// `--video modern` is the whole 2026 set, exact perspective with the
    /// rest (it was id's spans until the user turned it on in 2026);
    /// `--video classic` is id's; `--perspspan` (or the older `--exactpersp`,
    /// its two ends) moves it alone, and a later `--video` sets it again with
    /// the rest, as it does every video option.
    #[test]
    fn video_modern_carries_exact_perspective() {
        let mut v = VideoArgs::default();
        assert_eq!(v.persp_span, PerspSpan::Spans16, "id's spans by default");
        assert_eq!(v.parse("--video", "modern"), Ok(true));
        assert_eq!((v.cvars, v.persp_span, v.tag().as_str()), (VideoCvars::MODERN, PerspSpan::Exact, "modern"));
        assert_eq!(v.parse("--exactpersp", "0"), Ok(true));
        assert_eq!((v.cvars, v.persp_span, v.tag().as_str()), (VideoCvars::MODERN, PerspSpan::Spans16, "modern-spans"));
        assert_eq!(v.parse("--perspspan", "8"), Ok(true));
        assert_eq!((v.persp_span, v.tag().as_str()), (PerspSpan::Spans8, "modern-span8"));
        assert_eq!(v.parse("--video", "classic"), Ok(true));
        assert_eq!((v.cvars, v.persp_span, v.tag().as_str()), (VideoCvars::CLASSIC, PerspSpan::Spans16, "classic"));
        assert_eq!(v.parse("--exactpersp", "1"), Ok(true));
        assert_eq!(v.tag(), "classic-exactpersp");
        assert_eq!(v.parse("--perspspan", "4"), Ok(true));
        assert_eq!(v.tag(), "classic-span4");
        assert_eq!(v.parse("--perspspan", "64"), Ok(true));
        assert_eq!((v.persp_span, v.tag().as_str()), (PerspSpan::Spans64, "classic-span64"));
        assert!(v.parse("--exactpersp", "2").is_err());
        assert!(v.parse("--perspspan", "2").is_err() && v.parse("--perspspan", "0").is_err());
    }
}
