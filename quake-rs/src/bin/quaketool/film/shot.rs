//! The shot file: what `quaketool film` renders, as lines of `key value...`.
//!
//! The format is the command's own and has no dependencies: one setting a
//! line, `#` to the end of a line is a comment, blank lines are skipped, and
//! a value with spaces (a label) runs to the end of its line. A later line
//! sets a setting again (a command-line override is a line read last). Every
//! key and its default:
//!
//! ```text
#![doc = include_str!("shot_format.txt")]
//! ```

use std::fmt;

/// The format, as `quaketool film --help` prints it (the module doc's).
pub const FORMAT: &str = include_str!("shot_format.txt");

/// A parse error: the line it is on and what is wrong.
#[derive(Debug, PartialEq)]
pub struct ShotError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ShotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "shot: {}", self.message)
        } else {
            write!(f, "shot line {}: {}", self.line, self.message)
        }
    }
}

/// The world a shot runs.
#[derive(Clone, Debug, PartialEq)]
pub enum World {
    /// `maps/<name>.bsp`, live.
    Map(String),
    /// A demo (`demo1`..`demo3`, or a `.dem` in the pak), from `from` seconds in.
    Demo { name: String, from: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Classic,
    Slop,
}

/// How host frames are run (see `clock` in the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Id,
    Free,
}

/// Where the player is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Player {
    Spawn,
    Camera,
    At([f32; 3], Option<f32>),
}

/// Easing for a camera segment (applied to its time, 0..1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ease {
    #[default]
    Linear,
    In,
    Out,
    InOut,
}

impl Ease {
    /// `u` (0..1) through the ease: smooth cubic in, out, or both.
    pub fn apply(self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Ease::Linear => u,
            Ease::In => u * u * u,
            Ease::Out => 1.0 - (1.0 - u).powi(3),
            Ease::InOut => u * u * (3.0 - 2.0 * u),
        }
    }
}

/// Where a key looks: angles (pitch + up, yaw, roll, degrees) or a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    Angles([f64; 3]),
    At([f64; 3]),
}

/// One camera keyframe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub t: f64,
    pub pos: [f64; 3],
    pub look: Look,
    pub fov: Option<f64>,
    pub ease: Ease,
}

/// The camera.
#[derive(Clone, Debug, PartialEq)]
pub enum CameraSpec {
    Player,
    Demo,
    Path(Vec<Key>),
}

/// The base picture of an x-ray view (`xray.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum XrayBase {
    #[default]
    Game,
    Black,
    Z,
    Surfaces,
    Spans,
    Segments,
    Mip,
    Cache,
    Leaves,
    Lightmaps,
    Error,
}

impl XrayBase {
    pub const NAMES: [(&'static str, XrayBase); 11] = [
        ("game", XrayBase::Game),
        ("black", XrayBase::Black),
        ("z", XrayBase::Z),
        ("surfaces", XrayBase::Surfaces),
        ("spans", XrayBase::Spans),
        ("segments", XrayBase::Segments),
        ("mip", XrayBase::Mip),
        ("cache", XrayBase::Cache),
        ("leaves", XrayBase::Leaves),
        ("lightmaps", XrayBase::Lightmaps),
        ("error", XrayBase::Error),
    ];
}

/// Which polygon edges `wire` draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Wire {
    pub world: bool,
    pub entities: bool,
    /// Edges behind the picture's surfaces are drawn too (x-ray), not hidden.
    pub through: bool,
    /// The world's edges coloured by the PVS: drawn, in the PVS, culled.
    pub pvs: bool,
}

/// A whole shot.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub world: World,
    pub skill: u32,
    pub warmup: f64,
    pub duration: f64,
    pub fps: f64,
    pub size: (usize, usize),
    pub preset: Preset,
    /// `cvar NAME VALUE` lines, in order.
    pub cvars: Vec<(String, String)>,
    pub mode: Option<(usize, usize)>,
    pub pixel: usize,
    /// `None`: the preset's.
    pub display: Option<Option<f64>>,
    pub hud: bool,
    pub gun: bool,
    pub crosshair: f32,
    pub messages: bool,
    pub notarget: bool,
    pub player: Player,
    pub clock: Option<Clock>,
    /// `(from film second, speed)`, sorted by time.
    pub speed: Vec<(f64, f64)>,
    pub camera: Option<CameraSpec>,
    pub fov: f64,
    pub xray: XrayBase,
    pub wire: Wire,
    pub vis: Option<[f32; 3]>,
    /// `(film second, strength)`, sorted.
    pub mix: Vec<(f64, f64)>,
    pub label: Option<String>,
    pub sound: bool,
}

impl Default for Shot {
    fn default() -> Shot {
        Shot {
            world: World::Map(String::new()),
            skill: 1,
            warmup: 1.0,
            duration: 0.0,
            fps: 60.0,
            size: (1920, 1080),
            preset: Preset::Slop,
            cvars: Vec::new(),
            mode: None,
            pixel: 1,
            display: None,
            hud: false,
            gun: false,
            crosshair: 0.0,
            messages: false,
            notarget: true,
            player: Player::Spawn,
            clock: None,
            speed: Vec::new(),
            camera: None,
            fov: 90.0,
            xray: XrayBase::Game,
            wire: Wire::default(),
            vis: None,
            mix: Vec::new(),
            label: None,
            sound: false,
        }
    }
}

impl Shot {
    /// Parse a shot file's text.
    pub fn parse(text: &str) -> Result<Shot, ShotError> {
        let mut shot = Shot::default();
        let mut has_world = false;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            shot.apply(line).map_err(|message| ShotError { line: n + 1, message })?;
            has_world |= line.starts_with("map ") || line.starts_with("demo ");
        }
        if !has_world {
            return Err(ShotError { line: 0, message: "no `map` or `demo` line".into() });
        }
        shot.check().map_err(|message| ShotError { line: 0, message })?;
        Ok(shot)
    }

    /// Apply one more line (a command-line override); [`Shot::check`] the
    /// whole after the last.
    pub fn set(&mut self, line: &str) -> Result<(), String> {
        self.apply(line.trim())
    }

    /// What must hold of the whole shot.
    pub fn check(&self) -> Result<(), String> {
        if !(self.duration > 0.0 && self.duration.is_finite()) {
            return Err("`duration` must be a positive number of seconds".into());
        }
        if !(self.fps > 0.0 && self.fps <= 1000.0) {
            return Err("`fps` must be between 0 and 1000".into());
        }
        if let Some(CameraSpec::Path(keys)) = &self.camera {
            if keys.is_empty() {
                return Err("`camera path` has no `key` lines".into());
            }
        }
        if matches!(self.camera, Some(CameraSpec::Demo)) && !matches!(self.world, World::Demo { .. }) {
            return Err("`camera demo` needs a `demo` world".into());
        }
        Ok(())
    }

    fn apply(&mut self, line: &str) -> Result<(), String> {
        let (key, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(k, r)| (k, r.trim()));
        let words: Vec<&str> = rest.split_whitespace().collect();
        let one = || -> Result<&str, String> {
            match words.as_slice() {
                [w] => Ok(*w),
                _ => Err(format!("`{key}` takes one value, got {rest:?}")),
            }
        };
        let num = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("bad number {s:?}"));
        match key {
            "map" => self.world = World::Map(one()?.trim_start_matches("maps/").trim_end_matches(".bsp").into()),
            "demo" => {
                let from = match words.as_slice() {
                    [_] => 0.0,
                    [_, "from", s] => num(s)?.max(0.0),
                    _ => return Err(format!("`demo NAME [from S]`, got {rest:?}")),
                };
                self.world = World::Demo { name: words[0].into(), from };
                if self.camera.is_none() || matches!(self.camera, Some(CameraSpec::Player)) {
                    self.camera = Some(CameraSpec::Demo);
                }
            }
            "skill" => self.skill = num(one()?)?.clamp(0.0, 3.0) as u32,
            "warmup" => self.warmup = num(one()?)?.max(0.0),
            "duration" => self.duration = num(one()?)?,
            "fps" => self.fps = num(one()?)?,
            "size" => self.size = parse_size(one()?)?,
            "preset" => {
                self.preset = match one()? {
                    "slop" | "modern" | "2026" => Preset::Slop,
                    "classic" | "id" => Preset::Classic,
                    v => return Err(format!("`preset slop|classic`, got {v:?}")),
                }
            }
            "cvar" => match words.as_slice() {
                [name, value @ ..] if !value.is_empty() => {
                    if quake_rs::cvar::find(name).is_none() {
                        return Err(format!("no cvar {name:?}"));
                    }
                    self.cvars.push((name.to_string(), value.join(" ")));
                }
                _ => return Err(format!("`cvar NAME VALUE`, got {rest:?}")),
            },
            "mode" => self.mode = Some(parse_size(one()?)?),
            "pixel" => self.pixel = (num(one()?)? as usize).clamp(1, 8),
            "display" => {
                self.display = Some(match one()? {
                    "square" => None,
                    v => {
                        let (a, b) = v.split_once(':').ok_or_else(|| format!("`display W:H|square`, got {v:?}"))?;
                        let (a, b) = (num(a)?, num(b)?);
                        if !(a > 0.0 && b > 0.0) {
                            return Err(format!("`display`: {v:?} is not a shape"));
                        }
                        Some(a / b)
                    }
                })
            }
            "hud" => self.hud = on_off(one()?)?,
            "gun" => self.gun = on_off(one()?)?,
            "crosshair" => self.crosshair = num(one()?)? as f32,
            "messages" => self.messages = on_off(one()?)?,
            "notarget" => self.notarget = on_off(one()?)?,
            "sound" => self.sound = on_off(one()?)?,
            "player" => {
                self.player = match one()? {
                    "spawn" => Player::Spawn,
                    "camera" => Player::Camera,
                    v => {
                        let n = numbers(v)?;
                        match n.as_slice() {
                            [x, y, z] => Player::At([*x as f32, *y as f32, *z as f32], None),
                            [x, y, z, yaw] => Player::At([*x as f32, *y as f32, *z as f32], Some(*yaw as f32)),
                            _ => return Err(format!("`player spawn|camera|X,Y,Z[,YAW]`, got {v:?}")),
                        }
                    }
                }
            }
            "clock" => {
                self.clock = Some(match one()? {
                    "id" | "72" => Clock::Id,
                    "free" => Clock::Free,
                    v => return Err(format!("`clock id|free`, got {v:?}")),
                })
            }
            "speed" => {
                let (s, t) = match words.as_slice() {
                    [s] => (num(s)?, 0.0),
                    [s, "at", t] => (num(s)?, num(t)?),
                    _ => return Err(format!("`speed S [at T]`, got {rest:?}")),
                };
                if s < 0.0 {
                    return Err("`speed` cannot run the game backwards".into());
                }
                self.speed.retain(|&(at, _)| at != t);
                self.speed.push((t, s));
                self.speed.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            "camera" => {
                self.camera = Some(match words.as_slice() {
                    ["player"] => CameraSpec::Player,
                    ["demo"] => CameraSpec::Demo,
                    ["path"] => CameraSpec::Path(Vec::new()),
                    ["fixed", rest @ ..] => {
                        let (pos, look) = parse_place(rest)?;
                        CameraSpec::Path(vec![Key { t: 0.0, pos, look, fov: None, ease: Ease::Linear }])
                    }
                    _ => return Err(format!("`camera player|demo|path|fixed ...`, got {rest:?}")),
                })
            }
            "key" => {
                let Some(CameraSpec::Path(keys)) = &mut self.camera else {
                    return Err("`key` lines follow `camera path`".into());
                };
                let [t, rest @ ..] = words.as_slice() else { return Err("`key T X,Y,Z ...`".into()) };
                let t = num(t)?;
                // The place, then the options.
                let opts_at = rest.iter().position(|w| *w == "fov" || *w == "ease").unwrap_or(rest.len());
                let (pos, look) = parse_place(&rest[..opts_at])?;
                let (mut fov, mut ease) = (None, Ease::Linear);
                let mut o = rest[opts_at..].iter();
                while let Some(w) = o.next() {
                    let v = o.next().ok_or_else(|| format!("`{w}` needs a value"))?;
                    match *w {
                        "fov" => fov = Some(num(v)?),
                        _ => ease = parse_ease(v)?,
                    }
                }
                if keys.last().is_some_and(|k| k.t >= t) {
                    return Err(format!("key times must rise: {t} after {}", keys[keys.len() - 1].t));
                }
                keys.push(Key { t, pos, look, fov, ease });
            }
            "fov" => self.fov = num(one()?)?.clamp(10.0, 170.0),
            "xray" => {
                let v = one()?;
                self.xray = XrayBase::NAMES
                    .iter()
                    .find(|(n, _)| *n == v)
                    .map(|&(_, b)| b)
                    .ok_or_else(|| format!("`xray` modes: {}", XrayBase::NAMES.map(|(n, _)| n).join(", ")))?;
            }
            "wire" => {
                let mut wire = Wire::default();
                for w in &words {
                    match *w {
                        "world" => wire.world = true,
                        "entities" => wire.entities = true,
                        "all" => (wire.world, wire.entities) = (true, true),
                        "off" => wire = Wire::default(),
                        "hidden" => wire.through = false,
                        "through" => wire.through = true,
                        "pvs" => wire.pvs = true,
                        _ => return Err(format!("`wire world|entities|all|off [hidden|through] [pvs]`, got {w:?}")),
                    }
                }
                self.wire = wire;
            }
            "vis" => {
                let v = numbers(one()?)?;
                let [x, y, z] = v.as_slice() else { return Err("`vis X,Y,Z`".into()) };
                self.vis = Some([*x as f32, *y as f32, *z as f32]);
            }
            "mix" => {
                let [t, v] = words.as_slice() else { return Err(format!("`mix T VALUE`, got {rest:?}")) };
                let (t, v) = (num(t)?, num(v)?.clamp(0.0, 1.0));
                self.mix.retain(|&(at, _)| at != t);
                self.mix.push((t, v));
                self.mix.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            "label" => self.label = (!rest.is_empty()).then(|| rest.to_string()),
            _ => return Err(format!("unknown setting {key:?}")),
        }
        Ok(())
    }

    /// The preset's clock unless the shot says.
    pub fn clock(&self) -> Clock {
        self.clock.unwrap_or(match self.preset {
            Preset::Classic => Clock::Id,
            Preset::Slop => Clock::Free,
        })
    }

    /// The game's speed at film second `t`.
    pub fn speed_at(&self, t: f64) -> f64 {
        self.speed.iter().rev().find(|&&(at, _)| at <= t).map_or(1.0, |&(_, s)| s)
    }

    /// Game seconds from film second 0 to `t` (the integral of the speed;
    /// before 0, the speed at 0).
    pub fn game_time(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return t * self.speed_at(0.0);
        }
        let mut g = 0.0;
        let mut from = 0.0;
        let mut s = self.speed_at(0.0);
        for &(at, next) in self.speed.iter().filter(|&&(at, _)| at > 0.0) {
            if at >= t {
                break;
            }
            g += (at - from) * s;
            (from, s) = (at, next);
        }
        g + (t - from) * s
    }

    /// The x-ray's strength at film second `t` (linear between `mix` keys).
    pub fn mix_at(&self, t: f64) -> f64 {
        match self.mix.as_slice() {
            [] => 1.0,
            [(_, v)] => *v,
            keys => {
                let i = keys.iter().position(|&(at, _)| at > t).unwrap_or(keys.len());
                if i == 0 {
                    keys[0].1
                } else if i == keys.len() {
                    keys[i - 1].1
                } else {
                    let ((t0, a), (t1, b)) = (keys[i - 1], keys[i]);
                    a + (b - a) * ((t - t0) / (t1 - t0))
                }
            }
        }
    }

    /// The number of frames.
    pub fn frames(&self) -> usize {
        (self.duration * self.fps).round().max(1.0) as usize
    }
}

fn on_off(v: &str) -> Result<bool, String> {
    match v {
        "on" | "1" | "yes" => Ok(true),
        "off" | "0" | "no" => Ok(false),
        _ => Err(format!("expected on or off, got {v:?}")),
    }
}

fn parse_size(v: &str) -> Result<(usize, usize), String> {
    let (a, b) = v.split_once(['x', 'X']).ok_or_else(|| format!("expected WxH, got {v:?}"))?;
    let (w, h): (usize, usize) =
        (a.parse().map_err(|_| format!("bad width {a:?}"))?, b.parse().map_err(|_| format!("bad height {b:?}"))?);
    if w == 0 || h == 0 || w > 7680 || h > 4320 {
        return Err(format!("{v:?}: a size from 1x1 to 7680x4320"));
    }
    Ok((w, h))
}

fn numbers(v: &str) -> Result<Vec<f64>, String> {
    v.split(',')
        .map(|p| p.trim().parse::<f64>().ok().filter(|x| x.is_finite()).ok_or_else(|| format!("bad numbers {v:?}")))
        .collect()
}

fn parse_ease(v: &str) -> Result<Ease, String> {
    Ok(match v {
        "linear" => Ease::Linear,
        "in" => Ease::In,
        "out" => Ease::Out,
        "inout" | "in-out" => Ease::InOut,
        _ => return Err(format!("`ease linear|in|out|inout`, got {v:?}")),
    })
}

/// `X,Y,Z P,Y[,R]` or `X,Y,Z at X,Y,Z`.
fn parse_place(words: &[&str]) -> Result<([f64; 3], Look), String> {
    let xyz = |v: &str| -> Result<[f64; 3], String> {
        let n = numbers(v)?;
        n.as_slice().try_into().map_err(|_| format!("expected X,Y,Z, got {v:?}"))
    };
    match words {
        [pos, "at", target] => Ok((xyz(pos)?, Look::At(xyz(target)?))),
        [pos, angles] => {
            let a = numbers(angles)?;
            let look = match a.as_slice() {
                [p, y] => [*p, *y, 0.0],
                [p, y, r] => [*p, *y, *r],
                _ => return Err(format!("expected PITCH,YAW[,ROLL], got {angles:?}")),
            };
            Ok((xyz(pos)?, Look::Angles(look)))
        }
        _ => Err(format!("expected `X,Y,Z P,Y[,R]` or `X,Y,Z at X,Y,Z`, got {:?}", words.join(" "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLY: &str = "\
# e1m1's start, a slow fly-through
map e1m1
duration 8          # seconds
fps 30
size 1280x720
preset classic
cvar r_perspspan 1
mode 640x480
camera path
key 0   480,-352,110  0,90
key 4   544,288,80    -10,45   ease inout
key 8   600,140,88    at 700,140,60  fov 100
speed 0.5 at 2
speed 0 at 6
label Classic: id's 16-pixel spans
";

    #[test]
    fn a_shot_parses_every_kind_of_line() {
        let s = Shot::parse(FLY).expect("parses");
        assert_eq!(s.world, World::Map("e1m1".into()));
        assert_eq!((s.duration, s.fps, s.size), (8.0, 30.0, (1280, 720)));
        assert_eq!(s.preset, Preset::Classic);
        assert_eq!(s.clock(), Clock::Id, "Classic's clock is id's");
        assert_eq!(s.cvars, vec![("r_perspspan".to_string(), "1".to_string())]);
        assert_eq!(s.mode, Some((640, 480)));
        assert_eq!(s.label.as_deref(), Some("Classic: id's 16-pixel spans"));
        let Some(CameraSpec::Path(keys)) = &s.camera else { panic!("a path") };
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[1].ease, Ease::InOut);
        assert_eq!(keys[2].look, Look::At([700.0, 140.0, 60.0]));
        assert_eq!(keys[2].fov, Some(100.0));
        assert_eq!(s.frames(), 240);
    }

    #[test]
    fn the_speed_integrates_into_game_time() {
        let s = Shot::parse(FLY).unwrap();
        assert_eq!(s.speed_at(1.0), 1.0);
        assert_eq!(s.speed_at(3.0), 0.5);
        assert_eq!(s.speed_at(7.0), 0.0);
        assert!((s.game_time(2.0) - 2.0).abs() < 1e-12);
        assert!((s.game_time(4.0) - 3.0).abs() < 1e-12);
        assert!((s.game_time(8.0) - 4.0).abs() < 1e-12, "frozen from 6: {}", s.game_time(8.0));
        assert!((s.game_time(-0.5) + 0.5).abs() < 1e-12);
    }

    #[test]
    fn errors_name_their_line() {
        let e = Shot::parse("map e1m1\nduration 2\nfps fast\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(Shot::parse("duration 2\n").unwrap_err().message.contains("map"));
        assert!(Shot::parse("map e1m1\n").unwrap_err().message.contains("duration"));
        let e = Shot::parse("map e1m1\nduration 2\ncamera path\nkey 1 0,0,0 0,0\nkey 1 0,0,0 0,0\n").unwrap_err();
        assert_eq!(e.line, 5, "{e}");
        assert!(Shot::parse("map e1m1\nduration 2\ncvar no_such_cvar 1\n").is_err());
        assert!(Shot::parse("map e1m1\nduration 2\nwobble 3\n").is_err());
        assert!(Shot::parse("map e1m1\nduration 2\nkey 0 0,0,0 0,0\n").is_err(), "a key needs a path");
    }

    #[test]
    fn a_demo_follows_its_own_camera_and_overrides_apply_last() {
        let mut s = Shot::parse("demo demo1 from 12.5\nduration 3\n").unwrap();
        assert_eq!(s.world, World::Demo { name: "demo1".into(), from: 12.5 });
        assert_eq!(s.camera, Some(CameraSpec::Demo));
        assert_eq!(s.clock(), Clock::Free, "slop's clock by default");
        s.set("preset classic").unwrap();
        s.set("fps 72").unwrap();
        assert_eq!((s.preset, s.fps, s.clock()), (Preset::Classic, 72.0, Clock::Id));
        s.set("fps 0").unwrap();
        assert!(s.check().is_err());
        // A path given line by line: checked once it is whole.
        s.set("camera path").unwrap();
        assert!(s.check().is_err(), "no keys yet");
        s.set("key 0 0,0,0 0,90").unwrap();
        s.set("fps 30").unwrap();
        assert_eq!(s.check(), Ok(()));
    }

    #[test]
    fn mix_keys_interpolate() {
        let s = Shot::parse("map e1m1\nduration 4\nxray z\nmix 1 0\nmix 3 1\n").unwrap();
        assert_eq!(s.xray, XrayBase::Z);
        assert_eq!(s.mix_at(0.0), 0.0);
        assert_eq!(s.mix_at(2.0), 0.5);
        assert_eq!(s.mix_at(9.0), 1.0);
    }
}
