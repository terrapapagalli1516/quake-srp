//! The shot file: what `quaketool film` renders, as lines of `key value...`.
//!
//! The format is the command's own and has no dependencies: one setting a
//! line, a `#` that starts a word (at the line's start or after a space) to
//! the end of the line is a comment (`monster_army#0` is no comment), blank
//! lines are skipped, and a value with spaces (a label) runs to the end of
//! its line. A later line sets a setting again (a command-line override is
//! a line read last). Every key and its default:
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

/// Where a key looks: angles (degrees, Quake's: pitch + down, yaw, roll) or
/// a point.
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

/// What a camera follows or aims at, or a mark sticks to (`ENTITY` on a
/// shot line): `player`, an entity's number, or `CLASS[#N]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The player: a map's, or a demo's recorded one (its view entity).
    Player,
    /// An entity by number: a map's edict, a demo's `cl_entities` slot.
    Number(i32),
    /// The `N`th entity of a class to appear in the shot, from 0: those
    /// there at its first frame in entity order, then each as it appears (a
    /// nail fired later is a later one). The class is a classname
    /// (`monster_army`, `spike`), a model (`progs/soldier.mdl`) or a model's
    /// short name (`soldier`); a demo, which has no classnames, knows id1's
    /// by their models.
    Class(String, usize),
}

impl Target {
    pub fn parse(s: &str) -> Result<Target, String> {
        if s == "player" {
            return Ok(Target::Player);
        }
        if let Ok(n) = s.parse::<i32>() {
            return if n > 0 { Ok(Target::Number(n)) } else { Err(format!("no entity {n} (the world is 0)")) };
        }
        let (class, n) = match s.split_once('#') {
            Some((c, n)) => (c, n.parse::<usize>().map_err(|_| format!("`CLASS#N`, got {s:?}"))?),
            None => (s, 0),
        };
        if class.is_empty() || class.contains(',') {
            return Err(format!("an entity is `player`, a number or `CLASS[#N]`, got {s:?}"));
        }
        Ok(Target::Class(class.to_string(), n))
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Player => write!(f, "player"),
            Target::Number(n) => write!(f, "{n}"),
            Target::Class(c, n) => write!(f, "{c}#{n}"),
        }
    }
}

/// How a camera keeps to a moving target (`camera follow`, `aim`).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Follow {
    /// From the target to the eye (`camera follow`) or to the point aimed
    /// at (`aim`), in the world's axes.
    pub offset: [f64; 3],
    /// Game seconds the eye (an aim: the point aimed at) trails a target
    /// moving steadily, eased in and out (0: none).
    pub lag: f64,
    /// Game seconds ahead of the target the camera aims: where it will be.
    pub lookahead: f64,
}

/// The middle of an orbit.
#[derive(Clone, Debug, PartialEq)]
pub enum Centre {
    Point([f64; 3]),
    Target(Target),
}

/// `camera orbit`: the eye on a circle about a point or a target, looking
/// at it.
#[derive(Clone, Debug, PartialEq)]
pub struct Orbit {
    pub centre: Centre,
    pub radius: f64,
    /// The eye above the centre.
    pub height: f64,
    /// Degrees a film second, + anticlockwise seen from above (yaw rising).
    pub speed: f64,
    /// The eye's bearing from the centre at film second 0 (a yaw: 0 along +x).
    pub from: f64,
    /// A target centre's lag (see [`Follow::lag`]).
    pub lag: f64,
}

/// The camera.
#[derive(Clone, Debug, PartialEq)]
pub enum CameraSpec {
    Player,
    Demo,
    Path(Vec<Key>),
    /// `camera follow`: the eye rides with the target, looking at it; `key`
    /// lines after it are a path relative to it (their `X,Y,Z` the eye's
    /// offset from it, `at X,Y,Z` a point from the one aimed at).
    Follow {
        target: Target,
        follow: Follow,
        keys: Vec<Key>,
    },
    Orbit(Orbit),
}

impl CameraSpec {
    /// The target the camera's eye follows, if any.
    pub fn target(&self) -> Option<&Target> {
        match self {
            CameraSpec::Follow { target, .. } | CameraSpec::Orbit(Orbit { centre: Centre::Target(target), .. }) => {
                Some(target)
            }
            _ => None,
        }
    }
}

/// Where a mark is.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkAt {
    Point([f64; 3]),
    /// An entity where it is drawn, plus an offset.
    Entity(Target, [f64; 3]),
}

/// `mark`: a point whose place on the picture is written for each frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub name: String,
    pub at: MarkAt,
    /// How far behind what the frame drew there the point may lie and still
    /// be seen, in units (`None`: 8 for a point, 16 for an entity, whose
    /// middle is inside its model).
    pub radius: Option<f64>,
    /// `markdraw`: a tag drawn on the frames.
    pub draw: bool,
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
    Bakes,
    Luxels,
    Leaves,
    Lightmaps,
    Error,
    PixelsOff,
    Bands,
}

impl XrayBase {
    pub const NAMES: [(&'static str, XrayBase); 15] = [
        ("game", XrayBase::Game),
        ("black", XrayBase::Black),
        ("z", XrayBase::Z),
        ("surfaces", XrayBase::Surfaces),
        ("spans", XrayBase::Spans),
        ("segments", XrayBase::Segments),
        ("mip", XrayBase::Mip),
        ("cache", XrayBase::Cache),
        ("bakes", XrayBase::Bakes),
        ("luxels", XrayBase::Luxels),
        ("leaves", XrayBase::Leaves),
        ("lightmaps", XrayBase::Lightmaps),
        ("error", XrayBase::Error),
        ("pixelsoff", XrayBase::PixelsOff),
        ("bands", XrayBase::Bands),
    ];
}

/// A colour on a shot line: `RRGGBB` (hex, no `#`: that starts a comment)
/// or `pN`, Quake's palette entry N.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Rgb([u8; 3]),
    Palette(u8),
}

impl Colour {
    pub fn parse(s: &str) -> Option<Colour> {
        if let Some(n) = s.strip_prefix('p') {
            return n.parse().ok().map(Colour::Palette);
        }
        let v = u32::from_str_radix(s, 16).ok().filter(|_| s.len() == 6)?;
        Some(Colour::Rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8]))
    }

    pub fn rgb(self, palette: &[[u8; 3]; 256]) -> [u8; 3] {
        match self {
            Colour::Rgb(c) => c,
            Colour::Palette(i) => palette[usize::from(i)],
        }
    }
}

/// `divides`: a mark at each perspective divide, over the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Divides {
    pub colour: Colour,
    /// The mark's width in output pixels (`None`: a 720th of the frame's height).
    pub width: Option<f64>,
    pub alpha: f64,
}

/// `pixelsoff`'s look: the colour the pixels off exact are tinted, and how
/// much the rest is darkened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub colour: Colour,
    pub alpha: f64,
    pub dim: f64,
}

impl Default for Tint {
    fn default() -> Tint {
        Tint { colour: Colour::Rgb([0xff, 0x3d, 0x8b]), alpha: 0.85, dim: 0.0 }
    }
}

/// Something the shot does to the game at a film second.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `impulse N`: the console's, sent with the next move.
    Impulse(i32),
    /// `+attack` (true) or `-attack`.
    Attack(bool),
    /// Every entity whose `targetname` is this, used as `SUB_UseTargets`
    /// uses a target (the player its activator).
    Fire(String),
    /// The player's view angles (Quake's: pitch + down, yaw).
    Look(f32, f32),
    /// A console command: a held button (`+jump`, `-forward`, ...) or one
    /// of the game's (`god`, `noclip`, `fly`, `give`, `kill`, `impulse`).
    Console(Vec<String>),
}

/// How a shot's game sound meets its picture (`sound on|game`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SoundClock {
    /// Each sound starts with the frame that shows its cause and plays at
    /// its own speed (in slow motion too).
    #[default]
    Film,
    /// The mixer runs on the game's clock and is stretched to the film's:
    /// slow motion slows the sound and lowers it, a frozen world is silent.
    Game,
}

/// How the two renders of an `ab` shot meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Split {
    /// One frame cut by a vertical line (`splitat`): A left of it, B right.
    #[default]
    Line,
    /// A in the left half, B in the right, each framed for its half.
    Side,
    /// A above, B below, each framed for its half.
    Stack,
    /// A, with each pixel that differs from B's tinted.
    Diff,
}

/// Whose sound an `ab` shot keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AbSound {
    A,
    #[default]
    B,
    /// B's as `sound.wav`, and each side's as `sound-a.wav` and `sound-b.wav`.
    Both,
}

/// The `ab` lines: one shot rendered twice, with lines that differ.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Ab {
    /// Each side's own shot lines, applied after the shot's.
    pub lines: [Vec<String>; 2],
    pub labels: [Option<String>; 2],
    pub split: Split,
    /// The line's place (0 the left edge, 1 the right) at film seconds,
    /// keys interpolated (default 0.5).
    pub at: Vec<(f64, f64)>,
    pub sound: AbSound,
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
    /// Only the world's edges outside the PVS.
    pub culled: bool,
}

/// Where a label goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Corner {
    TopLeft,
    TopRight,
    #[default]
    BottomLeft,
    BottomRight,
}

/// How long the warm-up runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Warmup {
    /// Game seconds.
    For(f64),
    /// Until the game's clock (`cl.time`) reaches this.
    Until(f64),
}

/// A whole shot.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub world: World,
    pub skill: u32,
    pub warmup: Warmup,
    /// Monsters to wake: `(the point the nearest is to, film second)`.
    pub wake: Vec<([f32; 3], f64)>,
    pub duration: f64,
    pub fps: f64,
    pub size: (usize, usize),
    /// `frames A..B`: only film frames `A..B` written (the game runs from 0).
    pub frames: Option<(usize, usize)>,
    /// `threads N`: the renderer's threads (`None`: every core).
    pub threads: Option<usize>,
    pub preset: Preset,
    /// `cvar NAME VALUE` lines, in order.
    pub cvars: Vec<(String, String)>,
    pub mode: Option<(usize, usize)>,
    pub pixel: usize,
    /// `None`: the preset's.
    pub display: Option<Option<f64>>,
    pub hud: bool,
    pub gun: bool,
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
    pub labelpos: Corner,
    pub sound: bool,
    pub sound_clock: SoundClock,
    /// Write `events.json`: the sounds started, muzzle flashes, monsters' poses.
    pub events: bool,
    /// What the shot does to the game, at film seconds, in line order.
    pub actions: Vec<(f64, Action)>,
    /// `divides`: marks at each perspective divide.
    pub divides: Option<Divides>,
    /// `xray pixelsoff`'s look.
    pub tint: Tint,
    /// `ab`: the shot rendered twice.
    pub ab: Option<Ab>,
    /// `stepping id|uncapped`: how a host frame steps the game, if not the
    /// clock's (`clock id` steps id's way, `clock free` the uncapped way).
    pub stepping: Option<bool>,
    /// `body on`: the player's own model is drawn (for a camera away from it).
    pub body: bool,
    /// `cvar ... at T`, `hud ... at T` and the other [`TIMED`] lines with
    /// T past 0: settings that change mid-shot, `(film second, the line
    /// without its "at T")`, by time, in line order at one time.
    pub timed: Vec<(f64, String)>,
    /// `bob on`: a path camera bobs as the game bobs a running player's view.
    pub bob: bool,
    /// `aim`: the film's camera looks at a target, not where its keys say.
    pub aim: Option<(Target, Follow)>,
    /// `mark` lines, in line order (a name given again is replaced).
    pub marks: Vec<Mark>,
}

/// The settings a line can change mid-shot (`... at T`); [`Shot::at`].
pub const TIMED: [&str; 6] = ["cvar", "hud", "gun", "crosshair", "messages", "body"];

impl Default for Shot {
    fn default() -> Shot {
        Shot {
            world: World::Map(String::new()),
            skill: 1,
            warmup: Warmup::For(1.0),
            wake: Vec::new(),
            duration: 0.0,
            fps: 60.0,
            size: (1920, 1080),
            frames: None,
            threads: None,
            preset: Preset::Slop,
            cvars: Vec::new(),
            mode: None,
            pixel: 1,
            display: None,
            hud: false,
            gun: false,
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
            labelpos: Corner::BottomLeft,
            sound: false,
            sound_clock: SoundClock::Film,
            events: false,
            actions: Vec::new(),
            divides: None,
            tint: Tint::default(),
            ab: None,
            stepping: None,
            body: false,
            timed: Vec::new(),
            bob: false,
            aim: None,
            marks: Vec::new(),
        }
    }
}

impl Shot {
    /// Parse a shot file's text.
    pub fn parse(text: &str) -> Result<Shot, ShotError> {
        let mut shot = Shot::default();
        let mut has_world = false;
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
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
        // The game's own cameras: the player's eye, the recording's.
        let games = matches!(self.camera, None | Some(CameraSpec::Player | CameraSpec::Demo));
        if games && self.fov != 90.0 {
            return Err("`fov` is the film's cameras' (path, fixed, follow, orbit): the player's eye and a \
                        demo's see id's 90"
                .into());
        }
        if self.aim.is_some() && (games || matches!(self.camera, Some(CameraSpec::Follow { .. }))) {
            return Err("`aim` turns a path, fixed or orbit camera (`camera follow` aims already)".into());
        }
        let follows = self.camera.as_ref().and_then(CameraSpec::target).is_some();
        if follows && self.player == Player::Camera {
            return Err("`player camera` cannot ride a camera that follows the game: the player would move \
                        what the camera follows"
                .into());
        }
        if self.ab.is_some() {
            self.takes()?;
        }
        Ok(())
    }

    /// The shot as it stands at film second `t`: with the timed lines due
    /// by then applied, in time order.
    pub fn at(&self, t: f64) -> Shot {
        let mut s = self.clone();
        for (_, line) in self.timed.iter().take_while(|(k, _)| *k <= t) {
            // Each was applied once as it was read: it applies again.
            let _ = s.apply(line);
        }
        s
    }

    /// The targets the camera follows (its eye's, then an `aim`'s), each
    /// once: what a rehearsal of the shot records.
    pub fn followed(&self) -> Vec<Target> {
        let mut out: Vec<Target> = Vec::new();
        let eye = self.camera.as_ref().and_then(CameraSpec::target);
        for t in eye.into_iter().chain(self.aim.as_ref().map(|(t, _)| t)) {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
        out
    }

    /// The renders the shot makes: itself, or with `ab` its two sides — the
    /// shot with each side's lines applied — which must share the film's
    /// clock and size.
    pub fn takes(&self) -> Result<Vec<Shot>, String> {
        let Some(ab) = &self.ab else { return Ok(vec![self.clone()]) };
        let mut takes = Vec::new();
        for (side, lines) in ["a", "b"].iter().zip(&ab.lines) {
            let mut s = Shot { ab: None, ..self.clone() };
            for line in lines {
                s.apply(line).map_err(|e| format!("`ab` side {side}: {e}"))?;
            }
            if s.ab.is_some() {
                return Err("`ab` inside `ab`".into());
            }
            if (s.duration, s.fps) != (self.duration, self.fps) {
                return Err(format!("`ab` side {side} changes the duration or fps: both sides share the film's"));
            }
            takes.push(s);
        }
        Ok(takes)
    }

    /// Where an `ab` line split stands at film second `t` (0..1).
    pub fn split_at(&self, t: f64) -> f64 {
        let Some(ab) = &self.ab else { return 0.5 };
        interpolate(&ab.at, t, 0.5)
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
        if let (true, [w @ .., "at", t]) = (TIMED.contains(&key), words.as_slice()) {
            // A setting from film second T on: kept for [`Shot::at`] (and
            // tried now, so a bad line fails where it is read).
            let (t, line) = (num(t)?, format!("{key} {}", w.join(" ")));
            if t <= 0.0 {
                return self.apply(&line);
            }
            self.clone().apply(&line)?;
            let at = self.timed.partition_point(|(k, _)| *k <= t);
            self.timed.insert(at, (t, line));
            return Ok(());
        }
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
            "warmup" => {
                self.warmup = match words.as_slice() {
                    [s] => Warmup::For(num(s)?.max(0.0)),
                    ["until", t] => Warmup::Until(num(t)?),
                    _ => return Err(format!("`warmup S | until T`, got {rest:?}")),
                }
            }
            "wake" => {
                let (p, t) = match words.as_slice() {
                    [p] => (p, 0.0),
                    [p, "at", t] => (p, num(t)?),
                    _ => return Err(format!("`wake X,Y,Z [at T]`, got {rest:?}")),
                };
                let v = numbers(p)?;
                let [x, y, z] = v.as_slice() else { return Err("`wake X,Y,Z`".into()) };
                self.wake.push(([*x as f32, *y as f32, *z as f32], t));
            }
            "duration" => self.duration = num(one()?)?,
            "fps" => self.fps = num(one()?)?,
            "size" => self.size = parse_size(one()?)?,
            "frames" => {
                let bad = || format!("`frames A..B` (film frames A to B-1), got {rest:?}");
                let (a, b) = one()?.split_once("..").ok_or_else(bad)?;
                let (a, b) = (a.parse::<usize>().map_err(|_| bad())?, b.parse::<usize>().map_err(|_| bad())?);
                if a >= b {
                    return Err(bad());
                }
                self.frames = Some((a, b));
            }
            "threads" => {
                let n = one()?.parse::<usize>().ok().filter(|&n| n > 0);
                self.threads = Some(n.ok_or_else(|| format!("`threads N`, N at least 1, got {rest:?}"))?);
            }
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
                _ => return Err(format!("`cvar NAME VALUE [at T]`, got {rest:?}")),
            },
            "bob" => self.bob = on_off(one()?)?,
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
            // The `crosshair` cvar, so that a `cvar crosshair` line (or a
            // timed one) and this one are one setting.
            "crosshair" => {
                let v = match one()? {
                    "on" => "1",
                    "off" => "0",
                    v => {
                        num(v)?;
                        v
                    }
                };
                self.cvars.push(("crosshair".into(), v.into()));
            }
            "messages" => self.messages = on_off(one()?)?,
            "notarget" => self.notarget = on_off(one()?)?,
            "sound" => {
                (self.sound, self.sound_clock) = match one()? {
                    "game" => (true, SoundClock::Game),
                    "film" => (true, SoundClock::Film),
                    v => (on_off(v)?, SoundClock::Film),
                }
            }
            "events" => self.events = on_off(one()?)?,
            "body" => self.body = on_off(one()?)?,
            "stepping" => {
                self.stepping = Some(match one()? {
                    "uncapped" | "slop" => true,
                    "id" | "classic" => false,
                    v => return Err(format!("`stepping id|uncapped`, got {v:?}")),
                })
            }
            "impulse" => {
                let (w, t) = at_time(&words)?;
                let [n] = w else { return Err(format!("`impulse N [at T]`, got {rest:?}")) };
                self.act(t, Action::Impulse(num(n)? as i32));
            }
            "attack" => {
                let (w, t) = at_time(&words)?;
                let [v] = w else { return Err(format!("`attack on|off [at T]`, got {rest:?}")) };
                self.act(t, Action::Attack(on_off(v)?));
            }
            "fire" => {
                let (w, t) = at_time(&words)?;
                let [name] = w else { return Err(format!("`fire TARGETNAME [at T]`, got {rest:?}")) };
                self.act(t, Action::Fire(name.to_string()));
            }
            "look" => {
                let (w, t) = at_time(&words)?;
                let v = match w {
                    [a] => numbers(a)?,
                    _ => return Err(format!("`look P,Y [at T]`, got {rest:?}")),
                };
                let [p, y] = v.as_slice() else { return Err("`look P,Y`: pitch (+ down) and yaw".into()) };
                self.act(t, Action::Look(*p as f32, *y as f32));
            }
            "cmd" => {
                let (w, t) = at_time(&words)?;
                let argv: Vec<String> = w.iter().map(|s| s.to_string()).collect();
                check_console(&argv)?;
                self.act(t, Action::Console(argv));
            }
            "divides" => {
                let mut d = Divides { colour: Colour::Rgb([0x39, 0xc2, 0xff]), width: None, alpha: 0.85 };
                let mut it = words.iter();
                match it.next() {
                    Some(&"off") => {
                        self.divides = None;
                        return Ok(());
                    }
                    Some(&"on") => {}
                    _ => return Err(format!("`divides on|off [COLOUR] [width W] [alpha A]`, got {rest:?}")),
                }
                while let Some(w) = it.next() {
                    match *w {
                        "width" => d.width = Some(num(it.next().ok_or("`width W`")?)?.clamp(0.25, 64.0)),
                        "alpha" => d.alpha = num(it.next().ok_or("`alpha A`")?)?.clamp(0.0, 1.0),
                        c => {
                            d.colour = Colour::parse(c).ok_or_else(|| format!("a colour is RRGGBB or pN, got {c:?}"))?
                        }
                    }
                }
                self.divides = Some(d);
            }
            "ab" => {
                let (a, b) = rest.split_once('|').ok_or("`ab LINES | LINES` (`;` between a side's lines)")?;
                let side = |s: &str| -> Vec<String> {
                    s.split(';').map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
                };
                let ab = self.ab.get_or_insert_with(Ab::default);
                ab.lines = [side(a), side(b)];
            }
            "ablabels" => {
                let (a, b) = rest.split_once('|').ok_or("`ablabels TEXT | TEXT`")?;
                let label = |s: &str| Some(s.trim().to_string()).filter(|l| !l.is_empty());
                self.ab.get_or_insert_with(Ab::default).labels = [label(a), label(b)];
            }
            "split" => {
                let split = match one()? {
                    "line" | "wipe" => Split::Line,
                    "side" => Split::Side,
                    "stack" => Split::Stack,
                    "diff" => Split::Diff,
                    v => return Err(format!("`split line|side|stack|diff`, got {v:?}")),
                };
                self.ab.get_or_insert_with(Ab::default).split = split;
            }
            "splitat" => {
                let [t, x] = words.as_slice() else { return Err(format!("`splitat T X`, got {rest:?}")) };
                let (t, x) = (num(t)?, num(x)?.clamp(0.0, 1.0));
                let at = &mut self.ab.get_or_insert_with(Ab::default).at;
                at.retain(|&(k, _)| k != t);
                at.push((t, x));
                at.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            "absound" => {
                self.ab.get_or_insert_with(Ab::default).sound = match one()? {
                    "a" => AbSound::A,
                    "b" => AbSound::B,
                    "both" => AbSound::Both,
                    v => return Err(format!("`absound a|b|both`, got {v:?}")),
                }
            }
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
                    ["follow", target, opts @ ..] => {
                        let target = Target::parse(target)?;
                        CameraSpec::Follow { target, follow: parse_follow(opts)?, keys: Vec::new() }
                    }
                    ["orbit", centre, opts @ ..] => CameraSpec::Orbit(parse_orbit(centre, opts)?),
                    _ => return Err(format!("`camera player|demo|path|fixed|follow|orbit ...`, got {rest:?}")),
                })
            }
            "aim" => {
                let [target, opts @ ..] = words.as_slice() else {
                    return Err("`aim ENTITY [offset X,Y,Z] [lag S] [lookahead S]`".into());
                };
                self.aim = Some((Target::parse(target)?, parse_follow(opts)?));
            }
            "mark" => {
                let [name, place @ ..] = words.as_slice() else {
                    return Err("`mark NAME X,Y,Z | entity ENTITY`".into());
                };
                if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    return Err(format!("a mark's name is letters, digits, `_` and `-`, got {name:?}"));
                }
                let (at, mut opts) = match place {
                    ["entity", target, opts @ ..] => (MarkAt::Entity(Target::parse(target)?, [0.0; 3]), opts.iter()),
                    [p, opts @ ..] => (MarkAt::Point(xyz(p)?), opts.iter()),
                    [] => return Err("`mark NAME X,Y,Z | entity ENTITY`".into()),
                };
                let mut mark = Mark { name: name.to_string(), at, radius: None, draw: false };
                while let Some(w) = opts.next() {
                    let v = opts.next().ok_or_else(|| format!("`{w}` needs a value"))?;
                    match (*w, &mut mark.at) {
                        ("radius", _) => mark.radius = Some(num(v)?.max(0.0)),
                        ("offset", MarkAt::Entity(_, o)) => *o = xyz(v)?,
                        _ => return Err(format!("`mark` takes `offset X,Y,Z` (an entity) and `radius R`, not {w:?}")),
                    }
                }
                match self.marks.iter_mut().find(|m| m.name == mark.name) {
                    Some(m) => *m = mark,
                    None => self.marks.push(mark),
                }
            }
            "markdraw" => {
                let (name, on) = match words.as_slice() {
                    [name] => (*name, true),
                    [name, v] => (*name, on_off(v)?),
                    _ => return Err(format!("`markdraw NAME [on|off]`, got {rest:?}")),
                };
                let m = self.marks.iter_mut().find(|m| m.name == name);
                m.ok_or_else(|| format!("`markdraw {name}` before its `mark` line"))?.draw = on;
            }
            "key" => {
                let (Some(CameraSpec::Path(keys)) | Some(CameraSpec::Follow { keys, .. })) = &mut self.camera else {
                    return Err("`key` lines follow `camera path` (or `camera follow`)".into());
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
                let [v, opts @ ..] = words.as_slice() else { return Err("`xray MODE`".into()) };
                self.xray = XrayBase::NAMES
                    .iter()
                    .find(|(n, _)| n == v)
                    .map(|&(_, b)| b)
                    .ok_or_else(|| format!("`xray` modes: {}", XrayBase::NAMES.map(|(n, _)| n).join(", ")))?;
                // pixelsoff's look: [COLOUR] [alpha A] [dim D].
                let mut it = opts.iter();
                while let Some(w) = it.next() {
                    match *w {
                        "alpha" => self.tint.alpha = num(it.next().ok_or("`alpha A`")?)?.clamp(0.0, 1.0),
                        "dim" => self.tint.dim = num(it.next().ok_or("`dim D`")?)?.clamp(0.0, 1.0),
                        c if self.xray == XrayBase::PixelsOff => {
                            self.tint.colour =
                                Colour::parse(c).ok_or_else(|| format!("a colour is RRGGBB or pN, got {c:?}"))?
                        }
                        _ => return Err(format!("`xray {v}` takes no {w:?}")),
                    }
                }
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
                        "culled" => wire.culled = true,
                        _ => {
                            return Err(format!(
                                "`wire world|entities|all|off [hidden|through] [pvs|culled]`, got {w:?}"
                            ));
                        }
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
            "labelpos" => {
                self.labelpos = match one()? {
                    "tl" => Corner::TopLeft,
                    "tr" => Corner::TopRight,
                    "bl" => Corner::BottomLeft,
                    "br" => Corner::BottomRight,
                    v => return Err(format!("`labelpos tl|tr|bl|br`, got {v:?}")),
                }
            }
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

    /// The first film second at which [`Shot::game_time`] reaches `g` (the
    /// game time is never falling: a frozen stretch gives its start).
    pub fn film_time(&self, g: f64) -> f64 {
        let (mut lo, mut hi) = (-1.0, self.duration + 1.0);
        if self.game_time(hi) < g {
            return hi;
        }
        for _ in 0..64 {
            let mid = 0.5 * (lo + hi);
            if self.game_time(mid) >= g {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    /// The x-ray's strength at film second `t` (linear between `mix` keys).
    pub fn mix_at(&self, t: f64) -> f64 {
        interpolate(&self.mix, t, 1.0)
    }

    /// Add `action` at film second `t`, after any earlier at the same time.
    fn act(&mut self, t: f64, action: Action) {
        let at = self.actions.partition_point(|&(k, _)| k <= t);
        self.actions.insert(at, (t, action));
    }

    /// The actions due in the film frame at `t` (the one after `prev`): those
    /// at or before `t` and after `prev` — every one at or before `t` on the
    /// first frame (`first`).
    pub fn actions_due(&self, prev: f64, t: f64, first: bool) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(move |&&(k, _)| k <= t && (k > prev || first)).map(|(_, a)| a)
    }

    /// The number of frames.
    pub fn frames(&self) -> usize {
        (self.duration * self.fps).round().max(1.0) as usize
    }
}

/// Keys `(t, value)`, sorted, linear between them and held past the ends;
/// `default` with none.
fn interpolate(keys: &[(f64, f64)], t: f64, default: f64) -> f64 {
    match keys {
        [] => default,
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

/// A line's words less a trailing `at T`, and `T` (0 without one).
fn at_time<'a, 'b>(words: &'b [&'a str]) -> Result<(&'b [&'a str], f64), String> {
    match words {
        [w @ .., "at", t] => {
            let t = t.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("bad time {t:?}"))?;
            Ok((w, t))
        }
        w => Ok((w, 0.0)),
    }
}

/// The held buttons `cmd` takes (`+NAME` / `-NAME`).
pub const BUTTONS: [&str; 7] = ["attack", "jump", "forward", "back", "moveleft", "moveright", "movedown"];

/// The game's console commands `cmd` takes (`client::host_cmd::run_game_command`'s).
pub const GAME_COMMANDS: [&str; 6] = ["god", "noclip", "fly", "kill", "impulse", "give"];

/// Whether `cmd` can run `argv`.
fn check_console(argv: &[String]) -> Result<(), String> {
    let Some(name) = argv.first() else { return Err("`cmd COMMAND [ARGS] [at T]`".into()) };
    let button = name.strip_prefix(['+', '-']).is_some_and(|b| BUTTONS.contains(&b));
    if button || GAME_COMMANDS.contains(&name.as_str()) {
        Ok(())
    } else {
        Err(format!("`cmd` runs +/-{} and {}, not {name:?}", BUTTONS.join(", +/-"), GAME_COMMANDS.join(", ")))
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

/// A line less its comment: from a `#` that starts a word to the end.
fn strip_comment(raw: &str) -> &str {
    let mut prev = ' ';
    for (i, c) in raw.char_indices() {
        if c == '#' && prev.is_whitespace() {
            return &raw[..i];
        }
        prev = c;
    }
    raw
}

/// `X,Y,Z`.
fn xyz(v: &str) -> Result<[f64; 3], String> {
    let n = numbers(v)?;
    n.as_slice().try_into().map_err(|_| format!("expected X,Y,Z, got {v:?}"))
}

/// A seconds value that cannot be negative.
fn seconds(v: &str) -> Result<f64, String> {
    v.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).ok_or_else(|| format!("bad seconds {v:?}"))
}

/// `[offset X,Y,Z] [lag S] [lookahead S]`, in any order.
fn parse_follow(opts: &[&str]) -> Result<Follow, String> {
    let mut f = Follow::default();
    let mut it = opts.iter();
    while let Some(w) = it.next() {
        let v = it.next().ok_or_else(|| format!("`{w}` needs a value"))?;
        match *w {
            "offset" => f.offset = xyz(v)?,
            "lag" => f.lag = seconds(v)?,
            "lookahead" => f.lookahead = seconds(v)?,
            _ => return Err(format!("`[offset X,Y,Z] [lag S] [lookahead S]`, got {w:?}")),
        }
    }
    Ok(f)
}

/// `X,Y,Z|ENTITY radius R [height H] [speed DEG/S] [from YAW] [lag S]`.
fn parse_orbit(centre: &str, opts: &[&str]) -> Result<Orbit, String> {
    let centre =
        if centre.contains(',') { Centre::Point(xyz(centre)?) } else { Centre::Target(Target::parse(centre)?) };
    let mut o = Orbit { centre, radius: 128.0, height: 0.0, speed: 20.0, from: 0.0, lag: 0.0 };
    let mut it = opts.iter();
    while let Some(w) = it.next() {
        let v = it.next().ok_or_else(|| format!("`{w}` needs a value"))?;
        let n = v.parse::<f64>().ok().filter(|x| x.is_finite()).ok_or_else(|| format!("bad number {v:?}"));
        match *w {
            "radius" => o.radius = n?.max(1.0),
            "height" => o.height = n?,
            "speed" => o.speed = n?,
            "from" => o.from = n?,
            "lag" => o.lag = seconds(v)?,
            _ => return Err(format!("`camera orbit` takes radius, height, speed, from and lag, not {w:?}")),
        }
    }
    Ok(o)
}

/// `X,Y,Z P,Y[,R]` or `X,Y,Z at X,Y,Z`.
fn parse_place(words: &[&str]) -> Result<([f64; 3], Look), String> {
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
        // And back: the film second a game time is reached at.
        assert!((s.film_time(3.0) - 4.0).abs() < 1e-9);
        assert!((s.film_time(1.0) - 1.0).abs() < 1e-9);
        assert!((s.film_time(4.0) - 6.0).abs() < 1e-9, "the frozen stretch's start");
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
    fn a_window_and_the_threads_are_lines_too() {
        // What `--frames A..B` and `--threads N` say, a shot file can say: a
        // window of a longer shot, and the threads `xray bands` shows.
        let mut s = Shot::parse("map e1m1\nduration 4\nframes 60..180\nthreads 8\n").unwrap();
        assert_eq!((s.frames, s.threads), (Some((60, 180)), Some(8)));
        assert_eq!(s.frames(), 240, "the window does not change the shot's clock");
        s.set("frames 0..1").unwrap();
        assert_eq!(s.frames, Some((0, 1)), "an override is a line read last");
        assert_eq!(Shot::parse("map e1m1\nduration 4\n").unwrap().frames, None);
        for bad in ["frames 60", "frames 180..60", "frames 60..60", "frames a..b", "threads 0", "threads all"] {
            assert!(Shot::parse(&format!("map e1m1\nduration 4\n{bad}\n")).is_err(), "{bad}");
        }
    }

    #[test]
    fn warmup_wake_and_labels() {
        let s = Shot::parse(
            "map e1m7\nduration 2\nwarmup until 1.6\nwake 0,576,24\nwake 10,20,30 at 1.5\n\
             label id's 16\nlabelpos tr\nwire world through culled\nxray luxels\n",
        )
        .unwrap();
        assert_eq!(s.warmup, Warmup::Until(1.6));
        assert_eq!(s.wake, vec![([0.0, 576.0, 24.0], 0.0), ([10.0, 20.0, 30.0], 1.5)]);
        assert_eq!((s.label.as_deref(), s.labelpos), (Some("id's 16"), Corner::TopRight));
        assert!(s.wire.world && s.wire.through && s.wire.culled && !s.wire.entities);
        assert_eq!(s.xray, XrayBase::Luxels);
    }

    #[test]
    fn actions_happen_at_their_times_in_line_order() {
        let s = Shot::parse(
            "map e1m1\nduration 2\nimpulse 9\nimpulse 4 at 0.1\nattack on at 0.5\nfire t4 at 1.9\n\
             cmd give s 50 at 0.1\ncmd +jump at 1\nlook 10,45 at 1\nattack off at 1.5\n",
        )
        .unwrap();
        let due = |prev, t, first| s.actions_due(prev, t, first).cloned().collect::<Vec<_>>();
        assert_eq!(due(-0.1, 0.0, true), vec![Action::Impulse(9)]);
        assert_eq!(
            due(0.0, 0.1, false),
            vec![Action::Impulse(4), Action::Console(vec!["give".into(), "s".into(), "50".into()])],
            "line order at one time"
        );
        assert_eq!(due(0.9, 1.0, false), vec![Action::Console(vec!["+jump".into()]), Action::Look(10.0, 45.0)]);
        assert_eq!(due(1.8, 1.95, false), vec![Action::Fire("t4".into())]);
        assert_eq!(due(1.0, 1.4, false), vec![]);
        assert!(Shot::parse("map e1m1\nduration 2\ncmd quit\n").is_err(), "not every command");
        assert!(Shot::parse("map e1m1\nduration 2\nattack maybe\n").is_err());
    }

    #[test]
    fn divides_pixelsoff_sound_events_and_ab_lines() {
        let s = Shot::parse(
            "map e1m6\nduration 3\ndivides on 39c2ff width 2 alpha 0.5\nxray pixelsoff p251 dim 0.3\n\
             sound game\nevents on\nab cvar r_perspspan 16 | cvar r_perspspan 8; cvar r_torchflicker 0\n\
             ablabels id's 16 | slop's 8\nsplit line\nsplitat 1 1\nsplitat 3 0\nabsound both\n",
        )
        .unwrap();
        let d = s.divides.unwrap();
        assert_eq!((d.colour, d.width, d.alpha), (Colour::Rgb([0x39, 0xc2, 0xff]), Some(2.0), 0.5));
        assert_eq!((s.xray, s.tint.colour, s.tint.dim), (XrayBase::PixelsOff, Colour::Palette(251), 0.3));
        assert_eq!((s.sound, s.sound_clock, s.events), (true, SoundClock::Game, true));
        let ab = s.ab.as_ref().unwrap();
        assert_eq!(ab.lines[1], vec!["cvar r_perspspan 8".to_string(), "cvar r_torchflicker 0".to_string()]);
        assert_eq!(ab.labels[0].as_deref(), Some("id's 16"));
        assert_eq!((s.split_at(0.0), s.split_at(2.0), s.split_at(9.0)), (1.0, 0.5, 0.0));
        let takes = s.takes().unwrap();
        assert_eq!(takes.len(), 2);
        assert_eq!(takes[0].cvars.last().unwrap().1, "16");
        assert_eq!(takes[1].cvars.len(), 2);
        assert!(takes.iter().all(|t| t.ab.is_none()));
        assert!(Shot::parse("map e1m1\nduration 2\nab cvar no_such 1 | fps 30\n").is_err());
        let hashed = Shot::parse("map e1m1\nduration 2\ndivides on #ff0000\n").unwrap();
        assert_eq!(hashed.divides.unwrap().colour, Colour::Rgb([0x39, 0xc2, 0xff]), "# starts a comment");
    }

    #[test]
    fn mix_keys_interpolate() {
        let s = Shot::parse("map e1m1\nduration 4\nxray z\nmix 1 0\nmix 3 1\n").unwrap();
        assert_eq!(s.xray, XrayBase::Z);
        assert_eq!(s.mix_at(0.0), 0.0);
        assert_eq!(s.mix_at(2.0), 0.5);
        assert_eq!(s.mix_at(9.0), 1.0);
    }

    #[test]
    fn follow_orbit_aim_and_marks_parse() {
        let s = Shot::parse(
            "map e1m1\nduration 3   # a comment\ncamera follow monster_army#1 offset -70,-50,48 lag 0.4 lookahead 0.2\n\
             mark torch 1360,936,314 radius 4 # the flame\nmark grunt entity monster_army#1 offset 0,0,20\n\
             markdraw grunt\nkey 0 0,-80,0 at 0,0,0\n",
        )
        .expect("parses");
        let Some(CameraSpec::Follow { target, follow, keys }) = &s.camera else { panic!("a follow: {:?}", s.camera) };
        assert_eq!(*target, Target::Class("monster_army".into(), 1), "a # inside a word is no comment");
        assert_eq!(*follow, Follow { offset: [-70.0, -50.0, 48.0], lag: 0.4, lookahead: 0.2 });
        assert_eq!(keys.len(), 1, "keys about the target");
        assert_eq!(s.followed(), vec![Target::Class("monster_army".into(), 1)]);
        assert_eq!(s.marks.len(), 2);
        assert_eq!(
            (s.marks[0].at.clone(), s.marks[0].radius, s.marks[0].draw),
            (MarkAt::Point([1360.0, 936.0, 314.0]), Some(4.0), false)
        );
        assert_eq!(s.marks[1].at, MarkAt::Entity(Target::Class("monster_army".into(), 1), [0.0, 0.0, 20.0]));
        assert!(s.marks[1].draw);
        let o =
            Shot::parse("demo demo1\nduration 2\ncamera orbit player radius 200 height 40 speed -30 from 90 lag 0.3\n")
                .unwrap();
        let Some(CameraSpec::Orbit(o)) = &o.camera else { panic!("an orbit") };
        assert_eq!(
            (o.centre.clone(), o.radius, o.height, o.speed, o.from, o.lag),
            (Centre::Target(Target::Player), 200.0, 40.0, -30.0, 90.0, 0.3)
        );
        let a = Shot::parse("map e1m1\nduration 2\ncamera fixed 0,0,0 0,0\naim 87 lookahead 0.5\n").unwrap();
        assert_eq!(a.aim, Some((Target::Number(87), Follow { lookahead: 0.5, ..Follow::default() })));
        for (t, s) in
            [(Target::Player, "player"), (Target::Number(87), "87"), (Target::Class("spike".into(), 2), "spike#2")]
        {
            assert_eq!((Target::parse(s), t.to_string()), (Ok(t), s.to_string()));
        }
        assert_eq!(Target::parse("zombie"), Ok(Target::Class("zombie".into(), 0)));
        assert!(Target::parse("0").is_err() && Target::parse("ogre#x").is_err() && Target::parse("1,2,3").is_err());
    }

    #[test]
    fn what_a_shot_cannot_do_is_an_error() {
        let bad = |lines: &str| Shot::parse(&format!("map e1m1\nduration 2\n{lines}")).unwrap_err().message;
        assert!(bad("player camera\ncamera follow 87\n").contains("player camera"));
        assert!(bad("camera orbit monster_army#1\nplayer camera\n").contains("player camera"));
        assert!(
            Shot::parse("map e1m1\nduration 2\nplayer camera\ncamera orbit 0,0,0\n").is_ok(),
            "a point is no target"
        );
        assert!(bad("fov 110\n").contains("fov"), "the player's eye sees id's 90");
        assert!(Shot::parse("demo demo1\nduration 2\nfov 110\n").is_err());
        assert!(bad("camera follow 87\naim 87\n").contains("aim"));
        assert!(bad("aim 87\n").contains("aim"), "the player's eye is the player's");
        assert!(bad("markdraw torch\nmark torch 0,0,0\n").contains("before its `mark`"));
        assert!(bad("mark a,b 0,0,0\n").contains("name"));
        assert!(bad("mark t 0,0,0 offset 1,2,3\n").contains("offset"), "a point has no offset");
        assert!(bad("camera follow 87 lag -1\n").contains("seconds"));
    }

    #[test]
    fn settings_change_mid_shot_as_their_lines_say() {
        let s = Shot::parse(
            "map e1m1\nduration 2\nhud on at 0.5\ncrosshair 1\ncvar crosshair 2 at 1\ncrosshair off at 1.5\n\
             gun on at 1\nmessages on at 0\nbody on at 1.2\n",
        )
        .unwrap();
        assert_eq!(s.timed.len(), 5, "{:?}", s.timed);
        assert!(s.messages, "at 0: from the start");
        let at = |t: f64| s.at(t);
        assert!(!at(0.4).hud && at(0.5).hud && at(2.0).hud);
        assert!(!at(0.9).gun && at(1.0).gun);
        assert!(!at(1.1).body && at(1.2).body);
        // The crosshair is the cvar: the line, then the timed lines in time order.
        let last = |t: f64| at(t).cvars.iter().rev().find(|(n, _)| n == "crosshair").map(|(_, v)| v.clone());
        assert_eq!((last(0.0), last(1.0), last(1.6)), (Some("1".into()), Some("2".into()), Some("0".into())));
        assert!(Shot::parse("map e1m1\nduration 2\ngun maybe at 1\n").is_err(), "checked where it is read");
        assert!(Shot::parse("map e1m1\nduration 2\nfps 30 at 1\n").is_err(), "not every setting");
    }
}
