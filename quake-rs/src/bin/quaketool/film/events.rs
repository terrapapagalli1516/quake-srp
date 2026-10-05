//! `events.json`: what a shot's game did, frame by frame — each sound it
//! started, each muzzle flash, each monster's change of pose — so that sound
//! designed outside the game can be laid on the picture.
//!
//! ```text
//! { "fps": 60, "frames": 240, "size": [1920, 1080], "sound_rate": 48000, "sound_clock": "film",
//!   "events": [
//!     { "kind": "loop",  "t": 0, "sample": "ambience/fire1.wav", "origin": [..], "volume": 0.5, "attenuation": 3 },
//!     { "kind": "sound", "t": 0.5167, "frame": 31, "game_t": 3.43, "sample": "weapons/rocket1i.wav",
//!       "entity": 1, "channel": 1, "origin": [..], "volume": 1, "attenuation": 1,
//!       "dist": 12.0, "pan": 0.02, "screen": [960.0, 540.0], "left": 255, "right": 255 },
//!     { "kind": "flash", "t": .., "frame": .., "game_t": .., "entity": 1, "player": true, "origin": [..], "screen": null },
//!     { "kind": "pose",  "t": .., "frame": .., "game_t": .., "entity": 87, "class": "monster_army",
//!       "model": "progs/soldier.mdl", "pose": 12, "name": "run3", "from": 11, "from_name": "run2",
//!       "origin": [..], "screen": [..] } ] }
//! ```
//!
//! `t` is the film second of the frame that first shows the event — where
//! its sound starts in `sound.wav` — and `frame` that frame's number;
//! `game_t` is the game's clock (`cl.time`) then. `screen` is where the
//! point lands in the output frame, in pixels, or `null` behind the camera or
//! outside the picture; `pan` is -1 (left) to 1 (right) of the camera, `dist`
//! its distance in Quake units. `left`/`right` are the mixer's own volumes for
//! the channel the sound started on (0..255, with `sound on`). A pose is
//! logged when a monster on screen takes a new animation frame (`name` is the
//! model's own name for it: `run3`, `pain2`, `death5`); a monster's first
//! pose on screen has `from` null. With `ab`, each event has `"take": "a"`
//! or `"b"`.

use std::collections::HashMap;
use std::fmt::Write as _;

/// One JSON object, built field by field.
pub struct Obj(String);

impl Obj {
    pub fn kind(kind: &str) -> Obj {
        let mut s = String::from("{");
        let _ = write!(s, "\"kind\": {}", quote(kind));
        Obj(s)
    }

    fn key(&mut self, k: &str) {
        let _ = write!(self.0, ", {}: ", quote(k));
    }

    pub fn num(mut self, k: &str, v: f64) -> Obj {
        self.key(k);
        self.0 += &number(v);
        self
    }

    pub fn int(mut self, k: &str, v: i64) -> Obj {
        self.key(k);
        let _ = write!(self.0, "{v}");
        self
    }

    pub fn str(mut self, k: &str, v: &str) -> Obj {
        self.key(k);
        self.0 += &quote(v);
        self
    }

    pub fn opt_str(self, k: &str, v: Option<&str>) -> Obj {
        match v {
            Some(v) => self.str(k, v),
            None => self.null(k),
        }
    }

    pub fn opt_int(self, k: &str, v: Option<i64>) -> Obj {
        match v {
            Some(v) => self.int(k, v),
            None => self.null(k),
        }
    }

    pub fn bool(mut self, k: &str, v: bool) -> Obj {
        self.key(k);
        self.0 += if v { "true" } else { "false" };
        self
    }

    pub fn null(mut self, k: &str) -> Obj {
        self.key(k);
        self.0 += "null";
        self
    }

    pub fn vec(mut self, k: &str, v: &[f32]) -> Obj {
        self.key(k);
        self.0 += "[";
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.0 += ", ";
            }
            self.0 += &number(f64::from(*x));
        }
        self.0 += "]";
        self
    }

    pub fn opt_vec(self, k: &str, v: Option<&[f32]>) -> Obj {
        match v {
            Some(v) => self.vec(k, v),
            None => self.null(k),
        }
    }

    pub fn end(mut self) -> String {
        self.0 += "}";
        self.0
    }
}

/// A finite number, shortest form to 4 decimals (`null` for NaN).
fn number(v: f64) -> String {
    if !v.is_finite() {
        return "null".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// A JSON string.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A take's log: the events, and what it remembers to tell a change.
#[derive(Default)]
pub struct EventLog {
    pub events: Vec<String>,
    /// Each monster's pose as last seen on screen, by entity number.
    pub poses: HashMap<i32, i32>,
    /// A demo's message whose effects were logged last.
    pub demo_idx: Option<usize>,
}

/// The whole file.
pub fn file(header: &[(&str, String)], events: &[String]) -> String {
    let mut s = String::from("{\n");
    for (k, v) in header {
        let _ = writeln!(s, "  {}: {v},", quote(k));
    }
    s += "  \"events\": [\n";
    for (i, e) in events.iter().enumerate() {
        s += "    ";
        s += e;
        s += if i + 1 < events.len() { ",\n" } else { "\n" };
    }
    s += "  ]\n}\n";
    s
}

/// The monsters' models (id1's): an entity drawn with one of these is a
/// monster whose poses a demo's log follows.
pub const MONSTER_MODELS: [&str; 15] = [
    "progs/soldier.mdl",
    "progs/dog.mdl",
    "progs/ogre.mdl",
    "progs/knight.mdl",
    "progs/hknight.mdl",
    "progs/demon.mdl",
    "progs/shambler.mdl",
    "progs/zombie.mdl",
    "progs/wizard.mdl",
    "progs/enforcer.mdl",
    "progs/fish.mdl",
    "progs/shalrath.mdl",
    "progs/tarbaby.mdl",
    "progs/boss.mdl",
    "progs/oldone.mdl",
];

/// The model's name for animation frame `frame` (a group's first pose's).
pub fn frame_name(mdl: &quake_rs::mdl::Mdl, frame: i32) -> Option<String> {
    let f = mdl.frames.get(usize::try_from(frame).ok()?)?;
    Some(match f {
        quake_rs::mdl::Frame::Single(a) => a.name.clone(),
        quake_rs::mdl::Frame::Group { frames, .. } => frames.first()?.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_are_json() {
        let o = Obj::kind("sound")
            .num("t", 0.5)
            .int("frame", 30)
            .str("sample", "weapons/\"x\".wav")
            .vec("origin", &[1.0, -2.5, 0.0])
            .opt_vec("screen", None)
            .bool("player", true)
            .end();
        assert_eq!(
            o,
            r#"{"kind": "sound", "t": 0.5, "frame": 30, "sample": "weapons/\"x\".wav", "origin": [1, -2.5, 0], "screen": null, "player": true}"#
        );
        assert_eq!(number(1.0 / 3.0), "0.3333");
        assert_eq!(number(-0.00001), "0");
        let f = file(&[("fps", "60".into())], &[o.clone(), o]);
        assert!(f.starts_with("{\n  \"fps\": 60,\n  \"events\": [\n    {") && f.ends_with("}\n  ]\n}\n"));
    }
}
