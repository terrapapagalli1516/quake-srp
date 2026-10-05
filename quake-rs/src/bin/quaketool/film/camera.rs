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

use super::shot::{Ease, Key, Look};

/// A camera at one moment: the renderer's [`quake_rs::render::Camera`] in
/// `f64`, angles in degrees (pitch + up).
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
            pitch: self.pitch as f32,
            roll: self.roll as f32,
            fov_deg: self.fov as f32,
        }
    }
}

/// `(pitch + up, yaw)` in degrees looking from `from` at `to`.
pub fn look_at(from: [f64; 3], to: [f64; 3]) -> (f64, f64) {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let horiz = d[0].hypot(d[1]);
    let yaw = if horiz == 0.0 { 0.0 } else { d[1].atan2(d[0]).to_degrees() };
    (d[2].atan2(horiz).to_degrees(), yaw)
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
    pub fn at(&self, t: f64) -> Pose {
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
        let pos = [v[0], v[1], v[2]];
        let (pitch, yaw, roll) = if self.looks_at {
            let (pitch, yaw) = look_at(pos, [v[3], v[4], v[5]]);
            (pitch, yaw, 0.0)
        } else {
            (v[3], v[4], v[5])
        };
        Pose { pos, pitch, yaw, roll, fov: v[6] }
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
        assert!((p.yaw - 90.0).abs() < 1e-9 && (p.pitch - 45.0).abs() < 1e-9, "{p:?}");
        assert_eq!(look_at([0.0; 3], [0.0, 0.0, 10.0]), (90.0, 0.0));
    }
}
