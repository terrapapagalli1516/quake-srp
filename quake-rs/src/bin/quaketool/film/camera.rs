//! A camera path through keyframes: where the camera is and where it looks
//! at any film second.
//!
//! Each quantity — the position's three coordinates, the angles (or the
//! point looked at), the field of view — runs through a cubic Hermite curve
//! per segment, with the tangent at a key from its neighbours over their
//! times (Catmull-Rom for keys at any spacing: `(p[i+1] - p[i-1]) / (t[i+1] -
//! t[i-1])`) and at the first and last keys the one-sided difference, so a
//! path of two keys is a straight line at a steady speed. The curve passes
//! through every key and its velocity is continuous across them. A segment's
//! ease (its first key's) remaps the segment's time before the curve:
//! `inout` starts and stops it at rest. Angles go the shortest way round:
//! each key's angles are unwrapped against the key before, so a yaw from 170
//! to -170 turns 20 degrees through 180, not 340.

use super::shot::{CameraSpec, Centre, Ease, Follow, Key, Look, Shot, Target};

/// A camera at one moment, angles in degrees as Quake's are (`viewangles`,
/// `quaketool view --angles`): pitch + looks down, yaw 0 along +x and 90
/// along +y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: [f64; 3],
    pub pitch: f64,
    pub yaw: f64,
    pub roll: f64,
    pub fov: f64,
}

impl Pose {
    pub fn camera(&self) -> quake_rs::render::Camera {
        quake_rs::render::Camera {
            pos: self.pos.map(|v| v as f32),
            yaw: self.yaw as f32,
            // The renderer's pitch is + up.
            pitch: -self.pitch as f32,
            roll: self.roll as f32,
            fov_deg: self.fov as f32,
        }
    }
}

/// `(pitch, yaw)` in degrees (Quake's: pitch + down) looking from `from` at `to`.
pub fn look_at(from: [f64; 3], to: [f64; 3]) -> (f64, f64) {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let horiz = d[0].hypot(d[1]);
    let yaw = if horiz == 0.0 { 0.0 } else { d[1].atan2(d[0]).to_degrees() };
    (-d[2].atan2(horiz).to_degrees(), yaw)
}

/// `a` minus `b`, the shortest way round: in [-180, 180).
fn arc(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// A path, ready to evaluate.
pub struct Path {
    times: Vec<f64>,
    eases: Vec<Ease>,
    /// Per key: x, y, z, then either pitch, yaw, roll (unwrapped) or the
    /// looked-at point, then the fov.
    values: Vec<[f64; 7]>,
    /// Whether every key looks at a point (then the point is interpolated).
    looks_at: bool,
}

impl Path {
    /// The path through `keys` (at least one, times rising), with `fov`
    /// where a key gives none.
    pub fn new(keys: &[Key], fov: f64) -> Path {
        let looks_at = keys.iter().all(|k| matches!(k.look, Look::At(_)));
        let mut values: Vec<[f64; 7]> = Vec::with_capacity(keys.len());
        for k in keys {
            let look = match (looks_at, k.look) {
                (true, Look::At(p)) => p,
                (_, Look::Angles(a)) => a,
                (false, Look::At(p)) => {
                    let (pitch, yaw) = look_at(k.pos, p);
                    [pitch, yaw, 0.0]
                }
            };
            let mut v = [k.pos[0], k.pos[1], k.pos[2], look[0], look[1], look[2], k.fov.unwrap_or(fov)];
            if let (false, Some(prev)) = (looks_at, values.last()) {
                // The shortest arc from the key before.
                for c in 3..6 {
                    v[c] = prev[c] + arc(v[c], prev[c]);
                }
            }
            values.push(v);
        }
        Path {
            times: keys.iter().map(|k| k.t).collect(),
            eases: keys.iter().map(|k| k.ease).collect(),
            values,
            looks_at,
        }
    }

    /// The tangent (per second) of component `c` at key `i`.
    fn tangent(&self, i: usize, c: usize) -> f64 {
        let n = self.times.len();
        let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
        if a == b {
            return 0.0;
        }
        (self.values[b][c] - self.values[a][c]) / (self.times[b] - self.times[a])
    }

    /// The pose at film second `t` (before the first key, the first; after
    /// the last, the last).
    #[cfg(test)]
    pub fn at(&self, t: f64) -> Pose {
        let (pos, look, fov) = self.place(t);
        let (pitch, yaw, roll) = look.angles(pos);
        Pose { pos, pitch, yaw, roll, fov }
    }

    /// Where the path has the eye at film second `t`, where it looks, and
    /// its field of view.
    fn place(&self, t: f64) -> ([f64; 3], Aim, f64) {
        let n = self.times.len();
        let v = if n == 1 || t <= self.times[0] {
            self.values[0]
        } else if t >= self.times[n - 1] {
            self.values[n - 1]
        } else {
            let i = self.times.iter().rposition(|&k| k <= t).unwrap_or(0).min(n - 2);
            let h = self.times[i + 1] - self.times[i];
            let u = self.eases[i].apply((t - self.times[i]) / h);
            let (u2, u3) = (u * u, u * u * u);
            let (h00, h10, h01, h11) = (2.0 * u3 - 3.0 * u2 + 1.0, u3 - 2.0 * u2 + u, -2.0 * u3 + 3.0 * u2, u3 - u2);
            let mut v = [0.0; 7];
            for (c, out) in v.iter_mut().enumerate() {
                let (p0, p1) = (self.values[i][c], self.values[i + 1][c]);
                *out = h00 * p0 + h10 * h * self.tangent(i, c) + h01 * p1 + h11 * h * self.tangent(i + 1, c);
            }
            v
        };
        let look = if self.looks_at { Aim::At([v[3], v[4], v[5]]) } else { Aim::Angles([v[3], v[4], v[5]]) };
        ([v[0], v[1], v[2]], look, v[6])
    }
}

/// Where a camera looks: at a point, or along angles (pitch, yaw, roll).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Aim {
    At([f64; 3]),
    Angles([f64; 3]),
}

impl Aim {
    /// The angles from an eye at `pos`.
    fn angles(self, pos: [f64; 3]) -> (f64, f64, f64) {
        match self {
            Aim::At(p) => {
                let (pitch, yaw) = look_at(pos, p);
                (pitch, yaw, 0.0)
            }
            Aim::Angles([p, y, r]) => (p, y, r),
        }
    }
}

/// How long a monster stands between its steps, in game seconds: QuakeC's
/// monsters think ten times a second (`nextthink = time + 0.1`), and each
/// think is a step (`ai_walk`, `ai_run`). id's client draws each where its
/// step put it (Classic); slop's glides it there over the same time.
pub const STEP: f64 = 0.1;

/// A target's place along the game's clock, as a shot's rehearsal saw it:
/// one sample a host frame, `(game second, point)`, straight between them
/// and held before the first and after the last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    samples: Vec<(f64, [f64; 3])>,
    /// The integral of the place from the first sample to each sample.
    sums: Vec<[f64; 3]>,
}

impl Track {
    /// One more sample, at or after the last (one at the same time replaces it).
    pub fn push(&mut self, g: f64, p: [f64; 3]) {
        match self.samples.last() {
            Some(&(last, _)) if g < last => {}
            Some(&(last, _)) if g == last => {
                let n = self.samples.len();
                self.samples[n - 1].1 = p;
                // The last piece's area changes with its end.
                if n >= 2 {
                    let ((g0, a), s0) = (self.samples[n - 2], self.sums[n - 2]);
                    self.sums[n - 1] = [0, 1, 2].map(|k| s0[k] + (g - g0) * (a[k] + p[k]) / 2.0);
                }
            }
            Some(&(last, a)) => {
                let s0 = *self.sums.last().unwrap_or(&[0.0; 3]);
                self.sums.push([0, 1, 2].map(|k| s0[k] + (g - last) * (a[k] + p[k]) / 2.0));
                self.samples.push((g, p));
            }
            None => {
                self.samples.push((g, p));
                self.sums.push([0.0; 3]);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn samples(&self) -> &[(f64, [f64; 3])] {
        &self.samples
    }

    /// The place at game second `g` (the origin with no samples).
    pub fn raw(&self, g: f64) -> [f64; 3] {
        let s = &self.samples;
        let Some(&(first, p0)) = s.first() else { return [0.0; 3] };
        if g <= first {
            return p0;
        }
        let i = s.partition_point(|&(k, _)| k <= g);
        if i >= s.len() {
            return s[s.len() - 1].1;
        }
        let ((g0, a), (g1, b)) = (s[i - 1], s[i]);
        let f = (g - g0) / (g1 - g0);
        [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * f)
    }

    /// The integral of the place from the first sample to `g` (held places
    /// past either end).
    fn integral(&self, g: f64) -> [f64; 3] {
        let s = &self.samples;
        let Some(&(first, p0)) = s.first() else { return [0.0; 3] };
        if g <= first {
            return p0.map(|v| v * (g - first));
        }
        let i = s.partition_point(|&(k, _)| k <= g) - 1;
        let (gi, a) = s[i];
        let p = self.raw(g);
        [0, 1, 2].map(|k| self.sums[i][k] + (g - gi) * (a[k] + p[k]) / 2.0)
    }

    /// The place averaged over the [`STEP`] centred on `g`.
    fn boxed(&self, g: f64) -> [f64; 3] {
        let (a, b) = (self.integral(g - STEP / 2.0), self.integral(g + STEP / 2.0));
        [0, 1, 2].map(|k| (b[k] - a[k]) / STEP)
    }

    /// The place at `g` for a camera to keep to: averaged over a box as long
    /// as a monster's step, then over a Gaussian of half a step's spread (to
    /// three spreads either side). The box turns id's steps — a staircase —
    /// into the line through their middles, but its treads are not even (a
    /// run's steps are 8 to 15 units, and a think lands on id's 72 Hz frames,
    /// 7 or 8 ticks apart), so that line still turns at each step: the
    /// Gaussian rounds its corners, and the camera's speed changes smoothly.
    /// A steady motion is left as it is; a monster's own gait (its 8 frames
    /// of a run, 0.8 s) mostly is.
    pub fn steady(&self, g: f64) -> [f64; 3] {
        const TAPS: i32 = 12;
        let sigma = STEP / 2.0;
        let h = 3.0 * sigma / f64::from(TAPS);
        let (mut sum, mut weights) = ([0.0; 3], 0.0);
        for k in -TAPS..=TAPS {
            let u = f64::from(k) * h;
            let w = (-u * u / (2.0 * sigma * sigma)).exp();
            let p = self.boxed(g + u);
            for c in 0..3 {
                sum[c] += w * p[c];
            }
            weights += w;
        }
        sum.map(|s| s / weights)
    }
}

/// A camera's place trailing a [`Track`]: a critically damped spring pulled
/// toward its [`Track::steady`] place, whose stiffness makes a steadily
/// moving target lead it by `lag` seconds (`2 / lag` its natural frequency),
/// so the camera eases after a target that starts or stops and never
/// overshoots it. It starts as if it had been following all along.
#[derive(Clone, Debug)]
pub struct Lagged {
    g0: f64,
    values: Vec<[f64; 3]>,
}

impl Lagged {
    /// The spring's step, game seconds.
    const H: f64 = 1.0 / 960.0;

    pub fn new(track: &Track, lag: f64) -> Lagged {
        let (g0, g1) = match (track.samples.first(), track.samples.last()) {
            (Some(&(a, _)), Some(&(b, _))) => (a, b),
            _ => return Lagged { g0: 0.0, values: vec![[0.0; 3]] },
        };
        // Past the last sample, until it has settled.
        let steps = ((g1 - g0 + 4.0 * lag) / Self::H).ceil() as usize + 1;
        let w = 2.0 / lag;
        let y0 = track.steady(g0);
        let v0 = [0, 1, 2].map(|k| (track.steady(g0 + STEP)[k] - y0[k]) / STEP);
        // The steady state behind a target at a steady speed v: lag * v behind.
        let mut x = [0, 1, 2].map(|k| y0[k] - lag * v0[k]);
        let mut v = v0;
        let mut values = Vec::with_capacity(steps);
        for n in 0..steps {
            values.push(x);
            let y = track.steady(g0 + (n + 1) as f64 * Self::H);
            for k in 0..3 {
                // Semi-implicit Euler: the speed first, then the place.
                v[k] += Self::H * (w * w * (y[k] - x[k]) - 2.0 * w * v[k]);
                x[k] += Self::H * v[k];
            }
        }
        Lagged { g0, values }
    }

    pub fn at(&self, g: f64) -> [f64; 3] {
        let f = ((g - self.g0) / Self::H).max(0.0);
        let i = f.floor() as usize;
        match (self.values.get(i), self.values.get(i + 1)) {
            (Some(a), Some(b)) => {
                let u = f - i as f64;
                [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * u)
            }
            (Some(a), None) => *a,
            _ => self.values[self.values.len() - 1],
        }
    }
}

/// A followed target: its track, steadied and, with a lag, trailed.
#[derive(Clone, Debug)]
pub struct Followed {
    track: Track,
    lagged: Option<Lagged>,
}

impl Followed {
    pub fn new(track: Track, lag: f64) -> Followed {
        let lagged = (lag > 1e-3 && !track.is_empty()).then(|| Lagged::new(&track, lag));
        Followed { track, lagged }
    }

    /// Where the camera's eye takes the target to be at game second `g`:
    /// trailing it by the lag.
    pub fn eye(&self, g: f64) -> [f64; 3] {
        self.lagged.as_ref().map_or_else(|| self.track.steady(g), |l| l.at(g))
    }

    /// Where the camera aims: the target's steady place at `g`.
    pub fn aim(&self, g: f64) -> [f64; 3] {
        self.track.steady(g)
    }
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// How near a wall a camera that keeps out of walls may come, in units.
const WALL_MARGIN: f64 = 8.0;

/// How far a camera that follows is pulled in toward what it is fastened to
/// (the target, the orbit's centre) so that no wall comes between: sampled
/// over the film's seconds, already widened and smoothed ([`Rig::keep_out`]).
#[derive(Clone, Debug)]
struct Pull {
    t0: f64,
    values: Vec<f64>,
}

impl Pull {
    /// Its samples' spacing, film seconds.
    const H: f64 = 1.0 / 240.0;

    fn at(&self, t: f64) -> f64 {
        let f = ((t - self.t0) / Self::H).clamp(0.0, (self.values.len() - 1) as f64);
        let i = (f.floor() as usize).min(self.values.len() - 1);
        let b = self.values.get(i + 1).copied().unwrap_or(self.values[i]);
        self.values[i] + (b - self.values[i]) * (f - i as f64)
    }
}

/// Where a [`Rig`] has the camera, before any pull: the eye, what it is
/// fastened to (a follow's target, an orbit's centre: it keeps out of
/// walls), where it looks, and its field of view.
struct Placed {
    pos: [f64; 3],
    base: Option<[f64; 3]>,
    look: Aim,
    fov: f64,
}

/// The film's camera: the shot's `camera` and `aim`, and the places of the
/// targets they follow once a rehearsal has found them.
pub struct Rig {
    spec: Option<CameraSpec>,
    /// A path's keys (`camera path`, `fixed`), or a follow's (relative).
    path: Option<Path>,
    fov: f64,
    aim: Option<(Target, Follow)>,
    /// The followed targets, in [`Shot::followed`]'s order (none before a
    /// rehearsal).
    followed: Vec<(Target, Followed)>,
    pull: Option<Pull>,
}

impl Rig {
    /// `shot`'s camera, following `tracks` (the rehearsal's, one for each of
    /// [`Shot::followed`]; none in the rehearsal itself).
    pub fn new(shot: &Shot, tracks: Option<Vec<Track>>) -> Rig {
        let path = match &shot.camera {
            Some(CameraSpec::Path(keys)) => Some(Path::new(keys, shot.fov)),
            Some(CameraSpec::Follow { keys, .. }) if !keys.is_empty() => Some(Path::new(keys, shot.fov)),
            _ => None,
        };
        // Each target's lag: its eye's (the camera's), else its aim's.
        let eye = shot.camera.as_ref().and_then(CameraSpec::target);
        let lag = |t: &Target| match (&shot.camera, &shot.aim) {
            (Some(CameraSpec::Follow { follow, .. }), _) if eye == Some(t) => follow.lag,
            (Some(CameraSpec::Orbit(o)), _) if eye == Some(t) => o.lag,
            (_, Some((target, follow))) if target == t => follow.lag,
            _ => 0.0,
        };
        let tracks = tracks.unwrap_or_default();
        let followed = shot.followed().into_iter().zip(tracks).map(|(t, track)| {
            let lag = lag(&t);
            (t, Followed::new(track, lag))
        });
        let followed = followed.collect();
        Rig { spec: shot.camera.clone(), path, fov: shot.fov, aim: shot.aim.clone(), followed, pull: None }
    }

    fn find(&self, t: &Target) -> Option<&Followed> {
        self.followed.iter().find(|(k, _)| k == t).map(|(_, f)| f)
    }

    /// Where the camera is at film second `t`, game second `g`, before any
    /// pull: the eye, what it is fastened to (a follow's or an orbit's: it
    /// keeps out of walls), where it looks and its field of view.
    fn place(&self, t: f64, g: f64) -> Option<Placed> {
        let (pos, base, look, fov) = match self.spec.as_ref()? {
            CameraSpec::Player | CameraSpec::Demo | CameraSpec::Walk(_) => return None,
            CameraSpec::Path(_) => {
                let (pos, look, fov) = self.path.as_ref()?.place(t);
                (pos, None, look, fov)
            }
            CameraSpec::Follow { target, follow, .. } => {
                let f = self.find(target)?;
                let (base, aim) = (f.eye(g), f.aim(g + follow.lookahead));
                match &self.path {
                    // The keys, relative: their places from the target, their
                    // points from the one aimed at.
                    Some(path) => {
                        let (pos, look, fov) = path.place(t);
                        let look = match look {
                            Aim::At(p) => Aim::At(add(p, aim)),
                            angles => angles,
                        };
                        (add(pos, base), Some(base), look, fov)
                    }
                    None => (add(base, follow.offset), Some(base), Aim::At(aim), self.fov),
                }
            }
            CameraSpec::Orbit(o) => {
                let (centre, aim) = match &o.centre {
                    Centre::Point(p) => (*p, *p),
                    Centre::Target(target) => {
                        let f = self.find(target)?;
                        (f.eye(g), f.aim(g))
                    }
                };
                let a = (o.from + o.speed * t).to_radians();
                let pos = add(centre, [o.radius * a.cos(), o.radius * a.sin(), o.height]);
                (pos, Some(centre), Aim::At(aim), self.fov)
            }
        };
        Some(Placed { pos, base, look, fov })
    }

    /// The camera at film second `t`, game second `g` (`Shot::game_time(t)`),
    /// if the film places one (`None`: the player's eye or the demo's; and a
    /// camera that follows before its rehearsal).
    pub fn at(&self, t: f64, g: f64) -> Option<Pose> {
        let Placed { mut pos, base, look, fov } = self.place(t, g)?;
        if let (Some(base), Some(pull)) = (base, &self.pull) {
            // Pulled in along the line from what it is fastened to.
            let d = [0, 1, 2].map(|k| pos[k] - base[k]);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len > 1e-6 {
                let keep = ((len - pull.at(t)) / len).max(0.0);
                pos = [0, 1, 2].map(|k| base[k] + d[k] * keep);
            }
        }
        let (mut pitch, mut yaw, roll) = look.angles(pos);
        // `aim`: the same eye, turned to the target (before a rehearsal, as
        // its keys say).
        if let Some((target, follow)) = &self.aim {
            if let Some(f) = self.find(target) {
                let g = g + follow.lookahead;
                let p = if follow.lag > 1e-3 { f.eye(g) } else { f.aim(g) };
                (pitch, yaw) = look_at(pos, add(p, follow.offset));
            }
        }
        Some(Pose { pos, pitch, yaw, roll, fov })
    }

    /// Keep a camera fastened to a target or an orbit's centre out of the
    /// walls, over film seconds 0 to `duration`: where `clear(from, to)` (how
    /// far a point gets from `from` toward `to` before the world stops it)
    /// finds a wall between the eye and what it is fastened to, the eye is
    /// pulled in along that line to [`WALL_MARGIN`] short of the wall. (QuakeSpasm's
    /// chase camera does as much, a frame at a time; id's `Chase_Update`
    /// goes through walls.) So that the pull is never sudden, each moment's
    /// need is widened to the [`Track::steady`] smoothing's reach either side
    /// (the most needed nearby), then smoothed by the same Gaussian: the
    /// camera eases in before a wall comes between and out after it, and is
    /// never pulled in less than the moment needs.
    pub fn keep_out(&mut self, shot: &Shot, clear: impl Fn([f64; 3], [f64; 3]) -> [f64; 3]) {
        if !matches!(self.spec, Some(CameraSpec::Follow { .. } | CameraSpec::Orbit(_))) {
            return;
        }
        let sigma = STEP / 2.0;
        let reach = (3.0 * sigma / Pull::H).ceil() as usize;
        let t0 = -((2 * reach) as f64) * Pull::H;
        let n = ((shot.duration - t0) / Pull::H).ceil() as usize + 2 * reach + 1;
        let need: Vec<f64> = (0..n)
            .map(|i| {
                let t = t0 + i as f64 * Pull::H;
                let Some(Placed { pos, base: Some(base), .. }) = self.place(t, shot.game_time(t)) else { return 0.0 };
                let dist = |a: [f64; 3], b: [f64; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt();
                let (want, got) = (dist(pos, base), dist(clear(base, pos), base));
                if got + 1e-3 < want { (want - got + WALL_MARGIN).min(want) } else { 0.0 }
            })
            .collect();
        let wide: Vec<f64> = (0..n)
            .map(|i| need[i.saturating_sub(reach)..(i + reach + 1).min(n)].iter().fold(0.0f64, |a, &b| a.max(b)))
            .collect();
        let weights: Vec<f64> = (0..=2 * reach)
            .map(|j| (-((j as f64 - reach as f64) * Pull::H).powi(2) / (2.0 * sigma * sigma)).exp())
            .collect();
        let total: f64 = weights.iter().sum();
        let values = (0..n)
            .map(|i| {
                let sum: f64 =
                    (0..=2 * reach).map(|j| weights[j] * wide[(i + j).saturating_sub(reach).min(n - 1)]).sum();
                sum / total
            })
            .collect();
        self.pull = Some(Pull { t0, values });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, pos: [f64; 3], look: Look, ease: Ease) -> Key {
        Key { t, pos, look, fov: None, ease }
    }

    fn keys() -> Vec<Key> {
        vec![
            key(0.0, [480.0, -352.0, 110.0], Look::Angles([0.0, 170.0, 0.0]), Ease::Linear),
            key(2.0, [544.0, 288.0, 80.0], Look::Angles([-10.0, -170.0, 5.0]), Ease::InOut),
            key(5.0, [600.0, 140.0, 88.0], Look::Angles([20.0, -100.0, 0.0]), Ease::In),
            key(6.0, [610.0, 150.0, 90.0], Look::Angles([20.0, -90.0, 0.0]), Ease::Out),
        ]
    }

    #[test]
    fn the_path_passes_through_its_keyframes() {
        let ks = keys();
        let path = Path::new(&ks, 90.0);
        for k in &ks {
            let p = path.at(k.t);
            assert_eq!(p.pos, k.pos, "at {}", k.t);
            let Look::Angles(a) = k.look else { unreachable!() };
            for (got, want) in [(p.pitch, a[0]), (p.yaw, a[1]), (p.roll, a[2])] {
                assert!(arc(got, want).abs() < 1e-9, "at {}: {got} vs {want}", k.t);
            }
            assert_eq!(p.fov, 90.0);
        }
        // Before and after: held.
        assert_eq!(path.at(-1.0).pos, ks[0].pos);
        assert_eq!(path.at(60.0).pos, ks[3].pos);
    }

    #[test]
    fn yaw_takes_the_shortest_arc() {
        let path = Path::new(&keys(), 90.0);
        // 170 -> -170 (= 190): through 180, never back through 0.
        for i in 0..=20 {
            let yaw = path.at(i as f64 * 0.1).yaw.rem_euclid(360.0);
            assert!((165.0..=195.0).contains(&yaw), "t {}: yaw {yaw}", i as f64 * 0.1);
        }
    }

    #[test]
    fn the_motion_is_smooth_through_a_linear_key() {
        // Velocity is continuous across key 1 of an all-linear path: the
        // difference quotients either side agree.
        let mut ks = keys();
        for k in &mut ks {
            k.ease = Ease::Linear;
        }
        let path = Path::new(&ks, 90.0);
        let e = 1e-5;
        for c in 0..3 {
            let before = (path.at(2.0).pos[c] - path.at(2.0 - e).pos[c]) / e;
            let after = (path.at(2.0 + e).pos[c] - path.at(2.0).pos[c]) / e;
            assert!((before - after).abs() < 1e-2 * (1.0 + before.abs()), "axis {c}: {before} vs {after}");
        }
        // Two keys: a straight line at a steady speed.
        let two = Path::new(&ks[..2], 90.0);
        let mid = two.at(1.0).pos;
        for (c, m) in mid.iter().enumerate() {
            assert!((m - (ks[0].pos[c] + ks[1].pos[c]) / 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn an_eased_segment_starts_and_stops_at_rest() {
        let path = Path::new(&keys(), 90.0);
        // Segment 1 (2..5) is `inout`: no movement just after 2 or before 5.
        let e = 1e-4;
        for t in [2.0, 5.0 - e] {
            let d: f64 = (0..3).map(|c| (path.at(t + e).pos[c] - path.at(t).pos[c]).abs()).sum();
            assert!(d < 1e-3, "t {t}: moved {d}");
        }
    }

    #[test]
    fn a_path_of_points_looks_at_them() {
        let ks = vec![
            key(0.0, [0.0, 0.0, 0.0], Look::At([100.0, 0.0, 0.0]), Ease::Linear),
            key(1.0, [0.0, 0.0, 0.0], Look::At([0.0, 100.0, 100.0]), Ease::Linear),
        ];
        let path = Path::new(&ks, 90.0);
        let p = path.at(0.0);
        assert!((p.yaw, p.pitch) == (0.0, 0.0), "{p:?}");
        let p = path.at(1.0);
        assert!((p.yaw - 90.0).abs() < 1e-9 && (p.pitch + 45.0).abs() < 1e-9, "up is pitch -45: {p:?}");
        assert_eq!(look_at([0.0; 3], [0.0, 0.0, -10.0]), (90.0, 0.0), "straight down is pitch 90");
        assert_eq!(p.camera().pitch, 45.0, "the renderer's pitch is + up");
    }

    /// A grunt's run as id draws it: a step at each think, 8 to 15 units
    /// (`ai_run`'s), 7 or 8 of id's 72 Hz ticks apart, seen at 60 host
    /// frames a second; and the time it stops.
    fn run() -> (Track, f64) {
        let (lengths, ticks) = ([11.0, 15.0, 10.0, 10.0, 8.0, 15.0, 10.0, 8.0], [7, 8, 7, 7, 8, 7, 8, 7]);
        let mut steps = vec![(-1.0, 0.0)];
        let (mut at, mut x) = (0.5, 0.0);
        for i in 0..24 {
            x += lengths[i % 8];
            steps.push((at, x));
            at += f64::from(ticks[i % 8]) / 72.0;
        }
        let stop = at;
        let mut track = Track::default();
        for n in -60..=360 {
            let g = f64::from(n) / 60.0;
            let x = steps.iter().rev().find(|&&(t, _)| t <= g).map_or(0.0, |&(_, x)| x);
            track.push(g, [x, 0.0, 0.0]);
        }
        (track, stop)
    }

    /// The largest change of speed between film frames at 60 a second
    /// (units a second), over `g` from `a` to `b`.
    fn jolt(f: impl Fn(f64) -> f64, a: f64, b: f64) -> f64 {
        let h = 1.0 / 60.0;
        let n = ((b - a) / h) as usize;
        (1..n).map(|i| a + i as f64 * h).map(|g| ((f(g + h) - 2.0 * f(g) + f(g - h)) / h).abs()).fold(0.0, f64::max)
    }

    #[test]
    fn a_steady_track_smooths_id_s_steps_and_keeps_a_steady_motion() {
        let (track, stop) = run();
        let raw = jolt(|g| track.raw(g)[0], 1.0, stop - 0.5);
        let boxed = jolt(|g| track.boxed(g)[0], 1.0, stop - 0.5);
        let steady = jolt(|g| track.steady(g)[0], 1.0, stop - 0.5);
        // The staircase jumps a step (900 units a second in a frame); the box
        // alone still turns at each one (its speed jumps by some 75); the
        // steady place's speed changes by a sixth of that from frame to frame.
        assert!(raw > 400.0 && boxed > 50.0 && steady < boxed / 6.0, "raw {raw}, boxed {boxed}, steady {steady}");
        // And it stays with the grunt: within a step of where it stands.
        for g in [1.0, 2.0, 3.0] {
            assert!((track.steady(g)[0] - track.raw(g)[0]).abs() < 15.0, "at {g}");
        }
        // A steady motion is left as it is.
        let mut line = Track::default();
        for n in 0..=120 {
            let g = f64::from(n) / 60.0;
            line.push(g, [100.0 * g, 5.0, -2.0 * g]);
        }
        let p = line.steady(1.0);
        assert!((p[0] - 100.0).abs() < 1e-9 && (p[1] - 5.0).abs() < 1e-9 && (p[2] + 2.0).abs() < 1e-9, "{p:?}");
        assert_eq!(line.raw(-1.0), [0.0, 5.0, 0.0], "held before the first");
    }

    #[test]
    fn a_lagged_eye_trails_by_its_lag_and_settles_without_overshoot() {
        // 100 units a second for 2 s, then still at 200.
        let mut t = Track::default();
        for n in -60..=240 {
            let g = f64::from(n) / 60.0;
            t.push(g, [100.0 * g.min(2.0), 0.0, 0.0]);
        }
        let lagged = Lagged::new(&t, 0.5);
        // It starts as if it had been following: half a second behind soon after.
        assert!((lagged.at(0.0)[0] + 50.0).abs() < 2.0, "{:?}", lagged.at(0.0));
        assert!((lagged.at(1.2)[0] - 70.0).abs() < 0.5, "the lag behind a steady target: {:?}", lagged.at(1.2));
        // It stops after the target, never past it, and comes to rest there.
        let mut last = f64::MIN;
        for n in 0..=300 {
            let x = lagged.at(1.5 + f64::from(n) / 100.0)[0];
            assert!(x >= last - 1e-9 && x <= 200.0 + 1e-6, "at {}: {x}", 1.5 + f64::from(n) / 100.0);
            last = x;
        }
        assert!((last - 200.0).abs() < 0.5, "settled: {last}");
    }

    fn rig(lines: &str, track: Track) -> Rig {
        let shot = Shot::parse(&format!("map e1m1\nduration 4\n{lines}")).expect("parses");
        Rig::new(&shot, Some(vec![track]))
    }

    /// A target moving along +x at 100 units a second from (0, 0, 24).
    fn walker() -> Track {
        let mut t = Track::default();
        for n in -60..=300 {
            let g = f64::from(n) / 60.0;
            t.push(g, [100.0 * g, 0.0, 24.0]);
        }
        t
    }

    #[test]
    fn a_follow_rides_with_its_target_and_looks_ahead_of_it() {
        let r = rig("camera follow monster_army#1 offset -100,0,40 lookahead 0.5\nfov 70\n", walker());
        let p = r.at(1.0, 1.0).expect("a camera");
        assert!(p.pos.iter().zip([0.0, 0.0, 64.0]).all(|(a, b)| (a - b).abs() < 1e-6), "{p:?}");
        // Looking at where it will be in half a second: (150, 0, 24).
        let (pitch, yaw) = look_at(p.pos, [150.0, 0.0, 24.0]);
        assert!((p.pitch - pitch).abs() < 1e-6 && (p.yaw - yaw).abs() < 1e-6 && p.fov == 70.0, "{p:?}");
        // Its keys are a path about the target, their points about the aim.
        let r = rig("camera follow 87\nkey 0 0,-80,0 at 0,0,0\nkey 4 0,80,0 at 0,0,0\n", walker());
        let p = r.at(2.0, 2.0).unwrap();
        assert!((p.pos[0] - 200.0).abs() < 1e-6 && p.pos[1].abs() < 1e-6, "halfway round: {p:?}");
        assert!(r.at(0.0, 0.0).unwrap().yaw.abs() > 89.0, "from the side, looking across");
        // Before its rehearsal: no camera.
        let shot = Shot::parse("map e1m1\nduration 4\ncamera follow 87\n").unwrap();
        assert_eq!(Rig::new(&shot, None).at(1.0, 1.0), None);
    }

    #[test]
    fn an_orbit_circles_its_centre_and_an_aim_turns_a_path() {
        let r = rig("camera orbit 10,20,30 radius 100 height 50 speed 90 from 0\n", Track::default());
        let p = r.at(1.0, 1.0).unwrap();
        assert!((p.pos[0] - 10.0).abs() < 1e-9 && (p.pos[1] - 120.0).abs() < 1e-9 && p.pos[2] == 80.0, "{p:?}");
        assert!((p.yaw + 90.0).abs() < 1e-9 && p.pitch > 0.0, "looking back down at the centre: {p:?}");
        let r = rig("camera fixed 0,-200,24 0,0\naim 87 offset 0,0,10\n", walker());
        let p = r.at(2.0, 2.0).unwrap();
        let (pitch, yaw) = look_at([0.0, -200.0, 24.0], [200.0, 0.0, 34.0]);
        assert!((p.pitch - pitch).abs() < 1e-6 && (p.yaw - yaw).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn a_follow_keeps_out_of_a_wall_and_eases_in_and_out() {
        // A wall at x = 120 between t = 1 and 2 only (the world, as a trace
        // from the target to the eye finds it).
        let mut r = rig("camera follow 87 offset 100,0,0\n", walker());
        let shot = Shot::parse("map e1m1\nduration 4\ncamera follow 87 offset 100,0,0\n").unwrap();
        let wall = |t: f64| (1.0..2.0).contains(&t);
        let clear = |from: [f64; 3], to: [f64; 3]| {
            // The film second the target is at `from` (it walks 100 a second).
            if wall(from[0] / 100.0) && to[0] > 120.0 && from[0] < 120.0 { [120.0, to[1], to[2]] } else { to }
        };
        r.keep_out(&shot, clear);
        for n in 0..=240 {
            let t = f64::from(n) / 60.0;
            let x = r.at(t, t).unwrap().pos[0];
            if wall(t) && 100.0 * t < 120.0 {
                // Short of the wall by its margin, or at the target (no nearer).
                assert!(x <= (120.0 - WALL_MARGIN).max(100.0 * t) + 1e-6, "at {t}: {x}, in the wall");
            }
            if !(0.6..2.4).contains(&t) {
                assert!((x - (100.0 * t + 100.0)).abs() < 1e-6, "at {t}: {x}, free of it");
            }
        }
        // Eased: its speed never jumps.
        let j = jolt(|t| r.at(t, t).unwrap().pos[0], 0.1, 3.9);
        assert!(j < 400.0, "the pull's jolt: {j}");
    }
}
