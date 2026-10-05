//! `camera walk`: the player walks a route, and the camera is its own eye.
//!
//! A path camera glides at a fixed height: no steps, no bob, a hovering gun.
//! A walk moves the player itself. Each host frame the film sends the game
//! what a player at the keyboard and mouse sends — the view's angles, the
//! forward and side speeds, the jump button — through the client's own input
//! (`Walk::yaw`, `pitch` and `key_move`, which the page fills from the keys),
//! and the game's own movement code moves the player: `SV_ClientThink`'s
//! friction and acceleration, `SV_WalkMove`'s 18-unit steps and slides,
//! gravity. So the stairs, the walls, the view's bob and its stair smoothing
//! are the game's, drawn as id's client draws its player's eye. Nothing in
//! the engine changes for it.
//!
//! - **The route** is the shot's keys, waypoints `key T X,Y,Z`: the eye's
//!   place (the origin plus the 22-unit view height) the player should reach
//!   at film second `T`. The player starts standing at the first key. It
//!   steers toward a point [`LOOKAHEAD`] units ahead of itself along the
//!   segment it walks, never past the key it has not reached yet, so it keeps
//!   to the route's lines and turns at its corners. A key is reached when the
//!   eye comes within the walk's `within` (16 units) across and [`REACH_Z`]
//!   up or down; a key at the place of the key before is a pause, left at
//!   its time.
//! - **The speed** is what reaches the next key at its time: the distance
//!   left over the time left, at most the walk's top speed (320, the run;
//!   `walk`, 200), and slowing in time for a stop (the last key, a pause, a
//!   turn back), as the ground's friction brakes. Below [`SLOWEST`] the
//!   player stands: Quake's friction (its 100-unit stop speed) holds a slower
//!   wish at rest.
//! - **The look** turns, over each segment, from the look of the key it
//!   leaves to the look of the key it nears (`P,Y`, `at X,Y,Z`; a key without
//!   one looks along the route a little ahead, [`VIEW_AHEAD`]), as far as the
//!   player has come along it, then through a critically damped turn ([`TURN_RATE`]): a hand on a
//!   mouse, never a snap. The forward and side speeds are what move the
//!   player along the route whatever the view: turning a corner it strafes a
//!   little, as a player does.
//! - **A stuck player** (no progress toward the next key for
//!   [`STUCK_AFTER`] game seconds while it means to move: a wall, a ledge
//!   higher than a step, a key off the floor) ends the walk with an error,
//!   before anything is drawn.
//! - **The timing** is honest: the walk takes as long as it takes. The report
//!   gives the film second each key was reached at, against its `T`, and the
//!   end of the walk against the shot's.
//!
//! One walk, every take: a rehearsal of the shot walks the route (closed
//! loop: each move from where the player is), and records the moves by game
//! second ([`Plan`]). Each take sends the moves recorded for its own host
//! frames' times (open loop), so the two sides of an `ab` shot — Classic and
//! slop, 72 Hz and 240 — send the same commands, and their players walk the
//! same route each by its own physics. The report says how far apart.

use super::camera::{Track, look_at};
use super::shot::{Ease, Look, Walk};

/// The run: `sv_maxspeed`, what the always-run player reaches.
pub const RUN: f64 = 320.0;
/// The walk: `cl_forwardspeed`, the player who does not run.
pub const WALK: f64 = 200.0;
/// The slowest the player is asked to walk; a slower wish is a stand. On
/// the ground `SV_UserFriction` takes 4 × 100 units a second off a speed
/// below 100 every second, and `SV_Accelerate` adds 10 × the wish: a wish
/// below 40 never builds a speed, one above holds it. 50 keeps clear.
pub const SLOWEST: f64 = 50.0;
/// How near a key the eye passes, across (units): the player's half-width.
pub const WITHIN: f64 = 16.0;
/// How far above or below a key the eye may be and reach it: more than a
/// step (18) and the stair smoothing's lag (12), less than a storey.
pub const REACH_Z: f64 = 40.0;
/// How far ahead along its segment the player steers (units).
pub const LOOKAHEAD: f64 = 40.0;
/// How far ahead along the route a player looks where it goes (units): at a
/// run, a fifth of a second.
pub const VIEW_AHEAD: f64 = 64.0;
/// Game seconds without progress toward the next key that make a stuck player.
pub const STUCK_AFTER: f64 = 1.0;
/// What counts as progress: units nearer the key than the nearest yet.
const PROGRESS: f64 = 2.0;
/// The view's turn: a critically damped spring of this rate (per second): a
/// right angle's turn peaks near 330 degrees a second and is done in half a
/// second.
pub const TURN_RATE: f64 = 10.0;
/// The jump button is held at least this long (game seconds), so that a take
/// on a coarser clock than the plan's still sees it, and let go once the
/// player is off the ground (or after [`JUMP_MAX`]): QuakeC jumps once a
/// press (`FL_JUMPRELEASED`).
const JUMP_MIN: f64 = 0.1;
const JUMP_MAX: f64 = 0.3;
/// `sv_friction` and `sv_stopspeed`, for the braking distance.
const FRICTION: f64 = 4.0;
const STOPSPEED: f64 = 100.0;
/// A turn at a key sharper than this (degrees) stops the player there first.
const SHARP_TURN: f64 = 100.0;

/// What a player sends a host frame (`UserCmd`): the view's angles (Quake's:
/// pitch + down), the forward and side speeds (units a second, side + right),
/// the jump button.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Command {
    pub pitch: f64,
    pub yaw: f64,
    pub fwd: f64,
    pub side: f64,
    pub jump: bool,
}

/// What the walker reads of the player before a host frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub origin: [f64; 3],
    pub velocity: [f64; 3],
    pub onground: bool,
    /// The body's roll (`angles[ROLL]`: `SV_ClientThink`'s strafe lean), which
    /// tilts the side axis the side speed moves along.
    pub roll: f64,
}

impl Body {
    /// The eye, unbobbed: the origin plus the view height.
    pub fn eye(&self) -> [f64; 3] {
        [self.origin[0], self.origin[1], self.origin[2] + super::VIEWHEIGHT]
    }
}

/// A waypoint on the game's clock.
#[derive(Clone, Copy, Debug)]
struct Waypoint {
    /// The game second it should be reached at.
    g: f64,
    pos: [f64; 3],
    look: Option<Look>,
    jump: bool,
    ease: Ease,
}

/// Why a walk stopped: the key it could not reach and where the player stood.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stuck {
    pub key: usize,
    /// The game second it last made progress.
    pub since: f64,
    /// The eye then.
    pub eye: [f64; 3],
    /// Across, and up (+) or down from the eye to the key.
    pub across: f64,
    pub dz: f64,
}

/// The player at the keyboard: from where the player is, the move toward
/// the next key (see the module doc).
pub struct Walker {
    keys: Vec<Waypoint>,
    top: f64,
    within: f64,
    /// The key being walked to (`keys.len()`: the walk is over).
    target: usize,
    /// The game second each key was reached.
    reached: Vec<Option<f64>>,
    /// The view (pitch, yaw) and how fast it turns (degrees a second).
    look: [f64; 2],
    turn: [f64; 2],
    last: Option<f64>,
    /// The way the player last walked (a yaw), for a key that looks there.
    travel: f64,
    /// The nearest the player has come to the target, and when it last came
    /// nearer.
    best: f64,
    since: f64,
    /// When the jump button went down.
    jump: Option<f64>,
    stuck: Option<Stuck>,
}

/// Units across from `a` to `b`.
fn across(a: [f64; 3], b: [f64; 3]) -> f64 {
    (b[0] - a[0]).hypot(b[1] - a[1])
}

/// `a` minus `b`, the shortest way round: in [-180, 180).
fn arc(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// The speed from which the ground's friction stops the player within `d`
/// units (`SV_UserFriction`: 4 × the speed off a second, but at least 4 × 100).
pub fn braking(d: f64) -> f64 {
    let tail = STOPSPEED * STOPSPEED / (2.0 * FRICTION * STOPSPEED);
    if d <= 0.0 {
        0.0
    } else if d <= tail {
        (2.0 * FRICTION * STOPSPEED * d).sqrt()
    } else {
        STOPSPEED + FRICTION * (d - tail)
    }
}

impl Walker {
    /// A walker for `walk`, its keys' film seconds put on the game's clock by
    /// `game_time`.
    pub fn new(walk: &Walk, game_time: impl Fn(f64) -> f64) -> Walker {
        let keys: Vec<Waypoint> = walk
            .keys
            .iter()
            .map(|k| Waypoint { g: game_time(k.t), pos: k.pos, look: k.look, jump: k.jump, ease: k.ease })
            .collect();
        let travel = match keys.as_slice() {
            [a, b, ..] if across(a.pos, b.pos) > 1e-6 => (b.pos[1] - a.pos[1]).atan2(b.pos[0] - a.pos[0]).to_degrees(),
            _ => 0.0,
        };
        let mut w = Walker {
            reached: vec![None; keys.len()],
            keys,
            top: walk.top,
            within: walk.within,
            target: 0,
            look: [0.0; 2],
            turn: [0.0; 2],
            last: None,
            travel,
            best: f64::INFINITY,
            since: f64::NEG_INFINITY,
            jump: None,
            stuck: None,
        };
        w.look = w.look_of(0, w.keys[0].pos);
        w
    }

    /// Where the player stands at the start (the first key, its eye) and its
    /// view's (pitch, yaw) there.
    pub fn start(&self) -> ([f64; 3], [f64; 2]) {
        (self.keys[0].pos, self.look)
    }

    /// Every key reached.
    pub fn done(&self) -> bool {
        self.target >= self.keys.len()
    }

    pub fn stuck(&self) -> Option<&Stuck> {
        self.stuck.as_ref()
    }

    /// The game second each key was reached at.
    pub fn reached(&self) -> &[Option<f64>] {
        &self.reached
    }

    /// The key walked to (the number of keys: none, the walk is over).
    pub fn target(&self) -> usize {
        self.target
    }

    /// Whether the eye at `eye` reaches key `k` at game second `g`.
    fn reaches(&self, k: usize, eye: [f64; 3], g: f64) -> bool {
        let key = &self.keys[k];
        let near = across(eye, key.pos) <= self.within && (key.pos[2] - eye[2]).abs() <= REACH_Z;
        // A pause (a key where the one before is) is left at its time.
        let pause = k > 0 && across(self.keys[k - 1].pos, key.pos) <= self.within;
        near && (!pause || g >= key.g)
    }

    /// Key `k`'s look, from an eye at `eye`: its angles, the point it looks
    /// at, or where the player walks (level).
    fn look_of(&self, k: usize, eye: [f64; 3]) -> [f64; 2] {
        match self.keys[k].look {
            Some(Look::Angles([p, y, _])) => [p, y],
            Some(Look::At(p)) => {
                let (pitch, yaw) = look_at(eye, p);
                [pitch, yaw]
            }
            None => [0.0, self.travel],
        }
    }

    /// Whether the player should be at rest when it reaches key `k`: the
    /// last, before a pause, or before a turn back.
    fn stops_at(&self, k: usize) -> bool {
        let Some(next) = self.keys.get(k + 1) else { return true };
        let here = self.keys[k].pos;
        if across(here, next.pos) <= self.within {
            return true;
        }
        let Some(prev) = k.checked_sub(1).map(|p| self.keys[p].pos) else { return false };
        let a = (here[1] - prev[1]).atan2(here[0] - prev[0]).to_degrees();
        let b = (next.pos[1] - here[1]).atan2(next.pos[0] - here[0]).to_degrees();
        arc(b, a).abs() > SHARP_TURN
    }

    /// The heading (a yaw) of the route [`VIEW_AHEAD`] units on from the
    /// eye's place on the segment it walks, past the keys ahead (pauses
    /// skipped; past the last key, the last segment's): where a player looking
    /// where it goes looks, turning into a corner as it comes to it. The
    /// route's heading, not the way to the steering point, so that a small
    /// correction near a key does not swing the view.
    fn heading_ahead(&self, eye: [f64; 3]) -> Option<f64> {
        let n = self.keys.len();
        let mut k = self.target.clamp(1, n.max(2) - 1);
        let mut ahead = f64::INFINITY;
        let mut heading = None;
        while k < n {
            let (from, to) = (self.keys[k - 1].pos, self.keys[k].pos);
            let len = across(from, to);
            if len > 1e-6 {
                if ahead == f64::INFINITY {
                    // The first segment: from the eye's place on it.
                    let s = ((eye[0] - from[0]) * (to[0] - from[0]) + (eye[1] - from[1]) * (to[1] - from[1])) / len;
                    ahead = s.max(0.0) + VIEW_AHEAD;
                    if self.target >= n {
                        ahead = f64::MAX;
                    }
                }
                heading = Some((to[1] - from[1]).atan2(to[0] - from[0]).to_degrees());
                if ahead <= len {
                    break;
                }
                ahead -= len;
            }
            k += 1;
        }
        heading
    }

    /// How far along the segment into the target the eye is (0..1), and the
    /// point the player steers at.
    fn along(&self, eye: [f64; 3]) -> (f64, [f64; 3]) {
        let to = self.keys[self.target].pos;
        let Some(from) = self.target.checked_sub(1).map(|k| self.keys[k].pos) else { return (1.0, to) };
        let seg = [to[0] - from[0], to[1] - from[1]];
        let len = seg[0].hypot(seg[1]);
        if len <= 1e-6 {
            return (1.0, to);
        }
        let s = ((eye[0] - from[0]) * seg[0] + (eye[1] - from[1]) * seg[1]) / len;
        let ahead = (s.max(0.0) + LOOKAHEAD).min(len) / len;
        let carrot = [0, 1, 2].map(|c| from[c] + (to[c] - from[c]) * ahead);
        ((s / len).clamp(0.0, 1.0), carrot)
    }

    /// The command for the host frame that starts at game second `g`, the
    /// player as `body` has it.
    pub fn step(&mut self, g: f64, body: &Body) -> Command {
        let dt = self.last.map_or(0.0, |l| (g - l).max(0.0));
        self.last = Some(g);
        let eye = body.eye();
        while self.target < self.keys.len() && self.reaches(self.target, eye, g) {
            self.reached[self.target] = Some(g);
            if self.keys[self.target].jump {
                self.jump = Some(g);
            }
            self.target += 1;
            (self.best, self.since) = (f64::INFINITY, g);
        }
        if let Some(h) = self.heading_ahead(eye) {
            self.travel = h;
        }
        // The move: toward the steering point, at the speed that keeps time.
        let mut wish = [0.0; 2];
        let mut speed = 0.0;
        let mut progress = 1.0;
        if self.target < self.keys.len() {
            let key = self.keys[self.target];
            let (u, carrot) = self.along(eye);
            progress = u;
            let d = across(eye, key.pos);
            let left = key.g - g;
            let paced = if left > 1e-3 { d / left } else { self.top };
            let mut v = paced.min(self.top);
            if self.stops_at(self.target) {
                // On a curve a fifth under friction's own, so that a player
                // coming out of a turn fast (a turn adds speed across) still stops.
                v = v.min(0.8 * braking(d - 0.5 * self.within));
            }
            let pause = self.target > 0 && across(self.keys[self.target - 1].pos, key.pos) <= self.within;
            // Waiting: early (the time left is long for the way left), or
            // in a pause.
            let waiting = paced < SLOWEST || (pause && d <= self.within);
            if waiting || v < SLOWEST {
                v = 0.0;
            }
            let (dx, dy) = (carrot[0] - eye[0], carrot[1] - eye[1]);
            let len = dx.hypot(dy);
            if v > 0.0 && len > 1e-3 {
                wish = [dx / len, dy / len];
                speed = v;
            }
            // Stuck: not waiting, and no nearer for a while (a player braked
            // to a stop across from a key it does not reach is stuck too: the
            // key is above or below it).
            let gap = d + ((key.pos[2] - eye[2]).abs() - REACH_Z).max(0.0);
            if waiting || gap < self.best - PROGRESS {
                self.best = self.best.min(gap);
                self.since = g;
            } else if g - self.since > STUCK_AFTER && self.stuck.is_none() {
                self.stuck =
                    Some(Stuck { key: self.target, since: self.since, eye, across: d, dz: key.pos[2] - eye[2] });
            }
        }
        // The look: from the key left to the key neared, as far as the
        // player has come, then the hand's damped turn.
        let want = match self.target {
            0 => self.look_of(0, eye),
            t if t >= self.keys.len() => self.look_of(t - 1, eye),
            t => {
                let u = self.keys[t - 1].ease.apply(progress);
                let (a, b) = (self.look_of(t - 1, eye), self.look_of(t, eye));
                [a[0] + (b[0] - a[0]) * u, a[1] + arc(b[1], a[1]) * u]
            }
        };
        // The exact step of x'' = -2w x' - w² x (x the angle from the one wanted,
        // held over the frame).
        let w = TURN_RATE;
        let e = (-w * dt).exp();
        for ((look, turn), want) in self.look.iter_mut().zip(&mut self.turn).zip(want) {
            let (x, v) = (arc(*look, want), *turn);
            let k = (v + w * x) * dt;
            *look = want + (x + k) * e;
            *turn = (v - w * k) * e;
        }
        self.look[0] = self.look[0].clamp(-70.0, 80.0);
        self.look[1] = (self.look[1] + 180.0).rem_euclid(360.0) - 180.0;
        // The jump button.
        let jump = match self.jump {
            Some(down) => {
                let held = g - down;
                let up = held >= JUMP_MAX || (held >= JUMP_MIN && !body.onground);
                if up {
                    self.jump = None;
                }
                !up
            }
            None => false,
        };
        // The forward and side speeds that move the player along `wish`:
        // `SV_AirMove` moves it along the body's forward and right axes
        // (`angles`: a third of the view's pitch, the strafe lean's roll),
        // their level parts.
        let (fwd, side) = if speed > 0.0 {
            let angles = [(-self.look[0] / 3.0) as f32, self.look[1] as f32, body.roll as f32];
            let (f, r, _) = quake_rs::math::angle_vectors(angles);
            let (f, r) = ([f64::from(f[0]), f64::from(f[1])], [f64::from(r[0]), f64::from(r[1])]);
            let det = f[0] * r[1] - f[1] * r[0];
            let (wx, wy) = (speed * wish[0], speed * wish[1]);
            ((wx * r[1] - wy * r[0]) / det, (f[0] * wy - f[1] * wx) / det)
        } else {
            (0.0, 0.0)
        };
        Command { pitch: self.look[0], yaw: self.look[1], fwd, side, jump }
    }
}

/// A walk as its rehearsal walked it: the moves by game second, where the
/// player went, and when it reached each key.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// `(game second a host frame starts, its command)`, rising.
    pub moves: Vec<(f64, Command)>,
    /// The player's origin after each host frame, by the game second it ends.
    pub track: Track,
    /// The game second each key was reached (`None`: never).
    pub reached: Vec<Option<f64>>,
}

impl Plan {
    /// The command for a host frame starting at game second `g`: the one
    /// recorded there, or between two recorded ones (angles the shortest way,
    /// speeds straight; the jump button as the earlier has it); before the
    /// first, the first; after the last, its angles, standing.
    pub fn command(&self, g: f64) -> Command {
        let m = &self.moves;
        let Some(&(first, c0)) = m.first() else { return Command::default() };
        if g <= first {
            return c0;
        }
        let i = m.partition_point(|&(k, _)| k <= g);
        if i >= m.len() {
            let last = m[m.len() - 1].1;
            return Command { fwd: 0.0, side: 0.0, jump: false, ..last };
        }
        let ((g0, a), (g1, b)) = (m[i - 1], m[i]);
        let f = (g - g0) / (g1 - g0);
        Command {
            pitch: a.pitch + (b.pitch - a.pitch) * f,
            yaw: a.yaw + arc(b.yaw, a.yaw) * f,
            fwd: a.fwd + (b.fwd - a.fwd) * f,
            side: a.side + (b.side - a.side) * f,
            jump: a.jump,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::shot::Shot;
    use super::*;

    /// A toy of Quake's ground move, enough to steer: `SV_UserFriction`,
    /// `SV_Accelerate` toward the command's wish (yaw and the speeds, level),
    /// then the move; `wall_x` stops the player crossing x (a wall).
    struct Toy {
        body: Body,
        wall_x: Option<f64>,
    }

    impl Toy {
        fn at(eye: [f64; 3]) -> Toy {
            let body =
                Body { origin: [eye[0], eye[1], eye[2] - super::super::VIEWHEIGHT], onground: true, ..Body::default() };
            Toy { body, wall_x: None }
        }

        fn step(&mut self, c: &Command, dt: f64) {
            let v = &mut self.body.velocity;
            let speed = v[0].hypot(v[1]);
            if speed > 0.0 {
                let drop = speed.max(STOPSPEED) * FRICTION * dt;
                let k = (speed - drop).max(0.0) / speed;
                v[0] *= k;
                v[1] *= k;
            }
            let pitch = (-c.pitch / 3.0).to_radians();
            let y = c.yaw.to_radians();
            let wish =
                [c.fwd * pitch.cos() * y.cos() + c.side * y.sin(), c.fwd * pitch.cos() * y.sin() - c.side * y.cos()];
            let ws = wish[0].hypot(wish[1]).min(RUN);
            if ws > 0.0 {
                let dir = [wish[0] / wish[0].hypot(wish[1]), wish[1] / wish[0].hypot(wish[1])];
                let add = ws - (v[0] * dir[0] + v[1] * dir[1]);
                if add > 0.0 {
                    let a = (10.0 * ws * dt).min(add);
                    v[0] += a * dir[0];
                    v[1] += a * dir[1];
                }
            }
            let o = &mut self.body.origin;
            let nx = o[0] + v[0] * dt;
            match self.wall_x {
                Some(x) if (o[0] < x) != (nx < x) => v[0] = 0.0,
                _ => o[0] = nx,
            }
            o[1] += v[1] * dt;
        }
    }

    fn walk(lines: &str) -> Walk {
        let s = Shot::parse(&format!("map e1m1\nduration 9\n{lines}")).expect("parses");
        match s.camera {
            Some(super::super::shot::CameraSpec::Walk(w)) => w,
            c => panic!("a walk: {c:?}"),
        }
    }

    /// Walk the toy at `hz` until done, stuck or `secs`; returns the walker,
    /// the eye each frame and the commands.
    fn run(w: &Walk, hz: f64, secs: f64, wall_x: Option<f64>) -> (Walker, Vec<(f64, [f64; 3])>, Plan) {
        let mut walker = Walker::new(w, |t| t);
        let mut toy = Toy::at(walker.start().0);
        toy.wall_x = wall_x;
        let mut plan = Plan::default();
        let mut eyes = Vec::new();
        let dt = 1.0 / hz;
        let mut g = 0.0;
        while g < secs && walker.stuck().is_none() {
            let c = walker.step(g, &toy.body);
            plan.moves.push((g, c));
            toy.step(&c, dt);
            g += dt;
            plan.track.push(g, toy.body.origin);
            eyes.push((g, toy.body.eye()));
        }
        plan.reached = walker.reached().to_vec();
        (walker, eyes, plan)
    }

    const ROUTE: &str =
        "camera walk\nkey 0 0,0,46\nkey 1.2 384,0,46\nkey 2.4 384,384,46 0,180\nkey 3 384,450,46 0,180\n";

    #[test]
    fn the_player_reaches_every_key_on_time_and_stops_at_the_last() {
        let w = walk(ROUTE);
        for hz in [72.0, 240.0] {
            let (walker, eyes, plan) = run(&w, hz, 6.0, None);
            assert!(walker.done(), "{hz} Hz: every key reached");
            let r = plan.reached;
            assert_eq!(r[0], Some(0.0), "standing at the first key");
            // Keys at a run's pace (320/s) are reached about on time: the
            // start's acceleration costs a little.
            assert!((r[1].unwrap() - 1.2).abs() < 0.12, "{hz} Hz: key 1 at {:?}", r[1]);
            assert!((r[2].unwrap() - 2.4).abs() < 0.12, "{hz} Hz: key 2 at {:?}", r[2]);
            // It stops within the tolerance of the last key and stays.
            let end = eyes.last().unwrap().1;
            assert!(across(end, [384.0, 450.0, 46.0]) <= WITHIN, "{hz} Hz: ends at {end:?}");
            // Every key was passed within the tolerance.
            for k in &w.keys[1..] {
                let nearest = eyes.iter().map(|(_, e)| across(*e, k.pos)).fold(f64::INFINITY, f64::min);
                assert!(nearest <= WITHIN, "{hz} Hz: {:?} passed at {nearest}", k.pos);
            }
        }
    }

    #[test]
    fn the_view_turns_smoothly_to_a_key_s_look_and_otherwise_looks_ahead() {
        let w = walk(ROUTE);
        let (_, _, plan) = run(&w, 240.0, 6.0, None);
        let at = |g: f64| plan.command(g);
        assert!(at(0.6).yaw.abs() < 1.0, "walking east, looking east: {}", at(0.6).yaw);
        // Nearing key 2 the view turns to its yaw of 180 (west) through north:
        // the shortest arc from the travel's 90 north.
        assert!((arc(at(2.9).yaw, 180.0)).abs() < 10.0, "{}", at(2.9).yaw);
        // Never a snap: no frame turns more than a mouse-hand's 600 degrees a second.
        let worst =
            plan.moves.windows(2).map(|m| arc(m[1].1.yaw, m[0].1.yaw).abs() / (m[1].0 - m[0].0)).fold(0.0, f64::max);
        assert!(worst < 600.0, "turns at most {worst} degrees a second");
        // While the view lags the turn north at key 1, the player strafes to
        // keep to the route.
        assert!(plan.moves.iter().any(|(_, c)| c.side.abs() > 50.0), "a strafe in the turn");
    }

    #[test]
    fn a_pause_holds_the_player_until_its_time_and_a_slow_key_slows_it() {
        let w = walk("camera walk\nkey 0 0,0,46\nkey 0.5 160,0,46\nkey 2 160,0,46\nkey 4 480,0,46\n");
        let (walker, eyes, plan) = run(&w, 72.0, 6.0, None);
        assert!(walker.done());
        let r = &plan.reached;
        assert!((r[2].unwrap() - 2.0).abs() < 0.02, "the pause is left at its time: {:?}", r[2]);
        let x = |g: f64| eyes.iter().find(|(t, _)| *t >= g).unwrap().1[0];
        assert!((x(1.0) - x(1.9)).abs() < 2.0, "standing through the pause");
        // 320 units in 2 s: 160 a second, not a run.
        assert!((x(3.0) - x(2.5) - 80.0).abs() < 15.0, "walked {} in half a second", x(3.0) - x(2.5));
        assert!((r[3].unwrap() - 4.0).abs() < 0.15, "{:?}", r[3]);
    }

    #[test]
    fn a_wall_is_a_stuck_player_named() {
        let w = walk("camera walk\nkey 0 0,0,46\nkey 1 300,0,46\n");
        let (walker, eyes, _) = run(&w, 72.0, 6.0, Some(150.0));
        let s = *walker.stuck().expect("stuck at the wall");
        assert_eq!(s.key, 1);
        assert!((s.eye[0] - 150.0).abs() < 20.0 && (s.across - 150.0).abs() < 20.0, "{s:?}");
        assert!(eyes.last().unwrap().0 - s.since <= STUCK_AFTER + 0.05, "found a second after its last progress");
        // A key the eye can reach across but not up: stuck too.
        let w = walk("camera walk\nkey 0 0,0,46\nkey 1 100,0,146\n");
        let (walker, ..) = run(&w, 72.0, 6.0, None);
        assert!(walker.stuck().is_some_and(|s| s.dz > 90.0), "a key off the floor");
    }

    #[test]
    fn a_jump_is_held_until_the_player_leaves_the_ground() {
        let w = walk("camera walk\nkey 0 0,0,46\nkey 0.5 160,0,46 jump\nkey 1.5 480,0,46\n");
        let (_, _, plan) = run(&w, 240.0, 3.0, None);
        let held: Vec<f64> = plan.moves.iter().filter(|(_, c)| c.jump).map(|(g, _)| *g).collect();
        // The toy never leaves the ground: held its longest.
        let span = held.last().unwrap() - held.first().unwrap();
        assert!((span - JUMP_MAX).abs() < 0.01, "{span}");
        assert!((held[0] - 0.5).abs() < 0.1, "at key 1: {}", held[0]);
    }

    #[test]
    fn a_plan_replays_its_moves_exactly_and_between_them_smoothly() {
        let w = walk(ROUTE);
        let (_, eyes, plan) = run(&w, 240.0, 6.0, None);
        for &(g, c) in &plan.moves {
            assert_eq!(plan.command(g), c, "at its own times, its own moves");
        }
        // At 72 Hz the same moves, by game second, walk the same route.
        let mut toy = Toy::at([0.0, 0.0, 46.0]);
        let mut g = 0.0;
        let mut worst = 0.0f64;
        while g < 3.0 {
            toy.step(&plan.command(g), 1.0 / 72.0);
            g += 1.0 / 72.0;
            let p = plan.track.raw(g);
            worst = worst.max(across(p, toy.body.origin));
        }
        assert!(worst < 4.0, "72 Hz replaying 240's moves keeps within {worst} units");
        let last = plan.command(99.0);
        assert_eq!((last.fwd, last.side, last.jump), (0.0, 0.0, false), "after the plan, standing");
        assert!(eyes.len() > 100);
    }

    #[test]
    fn braking_stops_the_player_within_its_distance() {
        for d in [5.0, 12.5, 40.0, 100.0] {
            let mut v = braking(d);
            let mut x = 0.0;
            let dt = 1.0 / 480.0;
            while v > 0.0 {
                v = (v - v.max(STOPSPEED) * FRICTION * dt).max(0.0);
                x += v * dt;
            }
            assert!(x <= d && x > d * 0.9, "from {} stops in {x} of {d}", braking(d));
        }
        assert_eq!(braking(-1.0), 0.0);
    }
}
