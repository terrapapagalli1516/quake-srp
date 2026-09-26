//! `quaketool framerate <pak> [--rates LIST] [--only NAMES] [--markdown]
//! [--check]` — does the game play the same at every display rate?
//! `quaketool framerate <pak> --budget [--res WxH,...]` — what a frame of it
//! costs at 480 Hz.
//!
//! Each scenario is a scripted piece of play on the shareware maps — a jump,
//! a fall, a grenade, a lift, a damage flash, a demo — run through the same
//! client frame the browser runs ([`cl_main::walk_frame`],
//! [`cl_demo::demo_frame`]) with one host frame per display refresh, as the
//! uncapped host runs it. It runs at id's 72 Hz first (the reference: with
//! [`Stepping::Classic`] that is WinQuake), then at each other rate twice:
//! with id's per-frame code (Classic stepping; what the uncapped host did
//! before `FRAMERATE.md`) and with [`Stepping::Uncapped`]. Every quantity is
//! reported per rate, `id's → uncapped`.
//!
//! Rates: `60`, `72`, `144`, `240`, `480` (Hz, steady) and `jitter` (a 144 Hz
//! display whose refresh intervals wander ±40% and drop one in 50). Each run
//! restarts the random sequences (`server::reset_random`), so the rates
//! differ only by their frame times. With `--check` the command fails when
//! an uncapped value is further from the 72 Hz reference than the scenario's
//! stated tolerance.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::time::Instant;

use quake_rs::client::host::{host_filter_time_display, host_filter_time_uncapped};
use quake_rs::client::{cl_demo, cl_main, host_cmd, DemoPlay, Phase, SoundCall, Vid, Walk};
use quake_rs::pak::Pak;
use quake_rs::particles::ParticleKind;
use quake_rs::progs::OFS_PARM0;
use quake_rs::render;
use quake_rs::stepping::Stepping;
use quake_rs::vm::Vm;
use quake_rs::world;

/// The screen the scenarios draw (small: they measure the game, not pixels).
const VID: Vid = Vid { width: 320, height: 200, display_aspect: 4.0 / 3.0, exact_perspective: false };

// QuakeC constants (defs.qc).
const FL_GODMODE: i32 = 64;
const FL_NOTARGET: i32 = 128;
const FL_ONGROUND: i32 = 512;
const IT_SHOTGUN: i32 = 1;
const IT_NAILGUN: i32 = 4;
const IT_GRENADE_LAUNCHER: i32 = 16;
const IT_ROCKET_LAUNCHER: i32 = 32;
const IT_QUAD: i32 = 4_194_304;
const MOVETYPE_STEP: f32 = 4.0;
const MOVETYPE_NOCLIP: f32 = 8.0;
const MOVETYPE_BOUNCE: f32 = 10.0;
const SOLID_NOT: f32 = 0.0;
const SOLID_SLIDEBOX: f32 = 3.0;

// ---------------------------------------------------------------------------
// Rates and the host's clock
// ---------------------------------------------------------------------------

/// A display the harness drives the game at.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rate {
    /// A steady refresh rate.
    Hz(u32),
    /// A 144 Hz display whose refresh intervals wander ±40% and drop one
    /// refresh in 50 (a slow frame).
    Jitter,
}

impl Rate {
    fn parse(s: &str) -> Result<Rate, String> {
        match s {
            "jitter" => Ok(Rate::Jitter),
            n => n.parse().map(Rate::Hz).map_err(|_| format!("--rates: bad rate {n:?}")),
        }
    }

    fn label(self) -> String {
        match self {
            Rate::Hz(hz) => hz.to_string(),
            Rate::Jitter => "jitter".into(),
        }
    }
}

/// The host's clock at a [`Rate`]: `realtime` advanced by each refresh
/// interval, and the uncapped gate turning it into the next frame's
/// `host_frametime` — `Host_FilterTime` without its cap for id's per-frame
/// code (as the uncapped host ran it before), the display gate for
/// [`Stepping::Uncapped`] (the same at every rate here: none refreshes
/// faster than 1000 Hz).
struct FrameClock {
    rate: Rate,
    stepping: Stepping,
    realtime: f64,
    oldrealtime: f64,
    frames: u64,
    seed: u32,
}

impl FrameClock {
    fn new(rate: Rate, stepping: Stepping) -> FrameClock {
        FrameClock { rate, stepping, realtime: 0.0, oldrealtime: 0.0, frames: 0, seed: 0x5eed }
    }

    /// The next refresh interval.
    fn interval(&mut self) -> f64 {
        self.frames += 1;
        match self.rate {
            Rate::Hz(hz) => 1.0 / f64::from(hz),
            Rate::Jitter => {
                self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let u = f64::from(self.seed >> 8) / f64::from(1u32 << 24);
                let drop = if self.frames % 50 == 0 { 2.0 } else { 1.0 };
                drop * (0.6 + 0.8 * u) / 144.0
            }
        }
    }

    /// The next host frame's `host_frametime`.
    fn next(&mut self) -> f64 {
        loop {
            self.realtime += self.interval();
            match self.stepping {
                Stepping::Classic => return host_filter_time_uncapped(self.realtime, &mut self.oldrealtime),
                Stepping::Uncapped => {
                    if let Some(dt) = host_filter_time_display(self.realtime, &mut self.oldrealtime) {
                        return dt;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A run: the live game at one rate
// ---------------------------------------------------------------------------

/// One scenario's game at one rate: the live client ([`Walk`]) and the host's
/// clock driving it, and what the frames made that the scenarios read.
struct Sim {
    w: Walk,
    clock: FrameClock,
    /// Game time since the run started: the sum of the frames'
    /// `host_frametime`.
    t: f64,
    /// The last frame's `host_frametime`.
    dt: f64,
    /// Every sound the frames started, with the time of the frame's end.
    sounds: Vec<(f64, String)>,
}

impl Sim {
    /// A fresh level: `maps/<map>.bsp` loaded as `map` does, the random
    /// sequences restarted, the player in god mode and unseen by monsters.
    fn new(pak: &Pak, map: &str, rate: Rate, stepping: Stepping) -> Sim {
        quake_rs::server::reset_random();
        let path = format!("maps/{map}.bsp");
        let mut w = host_cmd::build_walk_map(pak.clone(), &path, &mut Vec::new())
            .unwrap_or_else(|| panic!("{path} would not load"));
        w.stepping = stepping;
        let mut s = Sim { w, clock: FrameClock::new(rate, stepping), t: 0.0, dt: 0.0, sounds: Vec::new() };
        s.set_flag(FL_GODMODE | FL_NOTARGET, true);
        s
    }

    fn player(&self) -> i32 {
        self.w.player
    }

    fn vm(&mut self) -> &mut Vm {
        &mut self.w.server.vm
    }

    /// One host frame; returns its `host_frametime`.
    fn frame(&mut self) -> f64 {
        let dt = self.clock.next();
        let frame = cl_main::walk_frame(&mut self.w, dt, false, &VID);
        self.t += dt;
        self.dt = dt;
        for call in &frame.sound {
            if let SoundCall::Start { events, .. } = call {
                self.sounds.extend(events.iter().map(|e| (self.t, e.sample.clone())));
            }
        }
        render::recycle_image(frame.image);
        dt
    }

    /// Run frames for `secs` of game time, calling `each` after every one.
    fn run(&mut self, secs: f64, mut each: impl FnMut(&mut Sim)) {
        let end = self.t + secs;
        while self.t < end - 1e-9 {
            self.frame();
            each(self);
        }
    }

    /// Run frames until `done` says so (checked after each), at most `secs`.
    fn run_until(&mut self, secs: f64, mut done: impl FnMut(&mut Sim) -> bool) {
        let end = self.t + secs;
        while self.t < end - 1e-9 {
            self.frame();
            if done(self) {
                return;
            }
        }
    }

    fn origin(&self) -> [f32; 3] {
        self.w.server.vm.ent_get_vector(self.w.player, "origin")
    }

    fn velocity(&self) -> [f32; 3] {
        self.w.server.vm.ent_get_vector(self.w.player, "velocity")
    }

    fn speed_xy(&self) -> f32 {
        let v = self.velocity();
        (v[0] * v[0] + v[1] * v[1]).sqrt()
    }

    fn flags(&self) -> i32 {
        self.w.server.vm.ent_get_float(self.w.player, "flags") as i32
    }

    fn on_ground(&self) -> bool {
        self.flags() & FL_ONGROUND != 0
    }

    fn set_flag(&mut self, flag: i32, on: bool) {
        let p = self.player();
        let f = self.flags();
        let f = if on { f | flag } else { f & !flag };
        self.vm().ent_set_float(p, "flags", f as f32);
    }

    /// Stand the player at `origin`, at rest, looking along `yaw` (level),
    /// and let 0.5 s of game time settle it.
    fn place(&mut self, origin: [f32; 3], yaw: f32) {
        self.teleport(origin, yaw);
        self.run(0.5, |_| {});
    }

    /// Put the player at `origin`, at rest, looking along `yaw` (level), the
    /// inputs let go. The spot must fit the player's box: `SV_CheckStuck`
    /// would send a stuck player back to where it last stood.
    fn teleport(&mut self, origin: [f32; 3], yaw: f32) {
        let fits = !world::trace_world(&self.w.bsp, origin, origin, [-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]).startsolid;
        assert!(fits, "{origin:?} does not fit the player's box");
        let p = self.player();
        set_origin(self.vm(), p, origin);
        let vm = self.vm();
        vm.ent_set_vector(p, "oldorigin", origin);
        vm.ent_set_vector(p, "velocity", [0.0; 3]);
        vm.ent_set_vector(p, "angles", [0.0, yaw, 0.0]);
        vm.ent_set_vector(p, "v_angle", [0.0, yaw, 0.0]);
        vm.ent_set_float(p, "jump_flag", 0.0);
        self.set_flag(FL_ONGROUND, false);
        (self.w.yaw, self.w.pitch) = (yaw, 0.0);
        self.release();
        self.w.oldz = f32::NAN;
    }

    /// Let go of every input.
    fn release(&mut self) {
        let w = &mut self.w;
        (w.in_fwd, w.in_side, w.in_jump, w.in_attack, w.in_down) = (0.0, 0.0, false, false, false);
    }

    /// Hand the player `weapon` (an `IT_*` bit) with plenty of ammo, selected.
    fn arm(&mut self, weapon: i32) {
        let p = self.player();
        let vm = self.vm();
        let items = vm.ent_get_float(p, "items") as i32 | weapon;
        vm.ent_set_float(p, "items", items as f32);
        for ammo in ["ammo_shells", "ammo_nails", "ammo_rockets", "ammo_cells"] {
            vm.ent_set_float(p, ammo, 100.0);
        }
        vm.ent_set_float(p, "weapon", weapon as f32);
        call_qc(vm, "W_SetCurrentAmmo", p);
    }

    /// God mode off and a deep well of health, for scenarios that count
    /// damage (`T_Damage` stops at god mode; armour would absorb some).
    fn mortal(&mut self) {
        self.set_flag(FL_GODMODE, false);
        let p = self.player();
        self.vm().ent_set_float(p, "health", 100_000.0);
        self.vm().ent_set_float(p, "armorvalue", 0.0);
    }

    /// The live edicts whose `classname` is `name`.
    fn find(&self, name: &str) -> Vec<i32> {
        let vm = &self.w.server.vm;
        (0..vm.num_edicts() as i32)
            .filter(|&e| !vm.is_free_edict(e) && vm.ent_string_ref(e, "classname") == name)
            .collect()
    }

    fn count_sounds(&self, sample: &str, from: f64) -> usize {
        self.sounds.iter().filter(|(t, s)| *t > from && s == sample).count()
    }
}

/// `PF_setorigin` through the engine's builtin (origin + `SV_LinkEdict`).
fn set_origin(vm: &mut Vm, e: i32, org: [f32; 3]) {
    vm.set_gi(OFS_PARM0, e);
    vm.set_gv(OFS_PARM0 + 3, org);
    vm.argc = 2;
    let f = vm.builtins[2];
    let _ = f(vm);
}

/// `PF_setmodel` through the engine's builtin.
fn set_model(vm: &mut Vm, e: i32, model: &str) {
    let s = vm.intern(model);
    vm.set_gi(OFS_PARM0, e);
    vm.set_gi(OFS_PARM0 + 3, s);
    vm.argc = 2;
    let f = vm.builtins[3];
    let _ = f(vm);
}

/// `PF_setsize` through the engine's builtin.
fn set_size(vm: &mut Vm, e: i32, mins: [f32; 3], maxs: [f32; 3]) {
    vm.set_gi(OFS_PARM0, e);
    vm.set_gv(OFS_PARM0 + 3, mins);
    vm.set_gv(OFS_PARM0 + 6, maxs);
    vm.argc = 3;
    let f = vm.builtins[4];
    let _ = f(vm);
}

/// `stuffcmd(e, text)` through the engine's builtin.
fn stuffcmd(vm: &mut Vm, e: i32, text: &str) {
    let s = vm.intern(text);
    vm.set_gi(OFS_PARM0, e);
    vm.set_gi(OFS_PARM0 + 3, s);
    vm.argc = 2;
    let f = vm.builtins[21];
    let _ = f(vm);
}

/// The trigger field a `func_door` spawned for itself (its `owner`).
fn door_trigger(s: &Sim, door: i32) -> i32 {
    let vm = &s.w.server.vm;
    (0..vm.num_edicts() as i32)
        .find(|&e| !vm.is_free_edict(e) && vm.ent_get_int(e, "owner") == door && vm.ent_string_ref(e, "classname") != "door")
        .expect("the door has its trigger field")
}

/// Draw one `random()` through the engine's builtin.
fn random(vm: &mut Vm) {
    vm.argc = 0;
    let f = vm.builtins[7];
    let _ = f(vm);
}

/// Call the QuakeC function `name` with `self = e`, `other = world`,
/// `activator = e`.
fn call_qc(vm: &mut Vm, name: &str, e: i32) {
    call_qc_with(vm, name, e, 0, e);
}

/// Call the QuakeC function `name` with `self`, `other` and `activator` set
/// and `time = sv.time`, as the engine's callers set them.
fn call_qc_with(vm: &mut Vm, name: &str, self_e: i32, other: i32, activator: i32) {
    let Some(f) = vm.progs.find_function(name) else { panic!("progs.dat has no {name}") };
    let t = vm.sv_time as f32;
    vm.gset_int("self", self_e);
    vm.gset_int("other", other);
    vm.gset_int("activator", activator);
    vm.gset_float("time", t);
    vm.argc = 0;
    if vm.execute(f).is_err() {
        vm.reset_execution();
    }
}

/// A spawned edict of `movetype` and `solid`, box `mins`..`maxs`, at
/// `origin` moving at `velocity`: a test body for the toss and step
/// integrators.
fn spawn_mover(s: &mut Sim, movetype: f32, solid: f32, mins: [f32; 3], maxs: [f32; 3], origin: [f32; 3], velocity: [f32; 3]) -> i32 {
    let vm = s.vm();
    let e = vm.spawn();
    let cn = vm.intern("framerate_mover");
    vm.ent_set_int(e, "classname", cn);
    vm.ent_set_float(e, "movetype", movetype);
    vm.ent_set_float(e, "solid", solid);
    set_size(vm, e, mins, maxs);
    set_origin(vm, e, origin);
    vm.ent_set_vector(e, "velocity", velocity);
    e
}

// ---------------------------------------------------------------------------
// Series: a quantity sampled once a frame
// ---------------------------------------------------------------------------

/// `(time, value)` once a frame, read between the samples by straight lines,
/// so a quantity can be compared at the same times across frame rates.
#[derive(Clone, Default)]
struct Series(Vec<(f64, f64)>);

impl Series {
    fn push(&mut self, t: f64, v: f64) {
        self.0.push((t, v));
    }

    /// The value at `t` (linear between the samples around it).
    fn at(&self, t: f64) -> f64 {
        let s = &self.0;
        match s.iter().position(|&(ti, _)| ti >= t) {
            None => s.last().map_or(f64::NAN, |p| p.1),
            Some(0) => s[0].1,
            Some(i) => {
                let ((t0, v0), (t1, v1)) = (s[i - 1], s[i]);
                v0 + (v1 - v0) * (t - t0) / (t1 - t0)
            }
        }
    }

    /// When the value first reaches `level` from below (linear between the
    /// samples around the crossing); NaN if it never does.
    fn rises_to(&self, level: f64) -> f64 {
        let s = &self.0;
        match s.iter().position(|&(_, v)| v >= level) {
            None => f64::NAN,
            Some(0) => s[0].0,
            Some(i) => {
                let ((t0, v0), (t1, v1)) = (s[i - 1], s[i]);
                t0 + (t1 - t0) * (level - v0) / (v1 - v0)
            }
        }
    }

    /// When the value first falls to `level` from above.
    fn falls_to(&self, level: f64) -> f64 {
        Series(self.0.iter().map(|&(t, v)| (t, -v)).collect()).rises_to(-level)
    }

    /// The samples from time `t` on.
    fn after(&self, t: f64) -> Series {
        Series(self.0.iter().filter(|p| p.0 > t).copied().collect())
    }

    fn max(&self) -> f64 {
        self.0.iter().map(|p| p.1).fold(f64::NAN, f64::max)
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// One measured quantity: its name, unit, value and how close the uncapped
/// path must stay to the 72 Hz reference (`--check`; NaN = reported only).
struct Measure {
    name: &'static str,
    unit: &'static str,
    value: f64,
    tolerance: f64,
}

fn m(name: &'static str, unit: &'static str, value: f64, tolerance: f64) -> Measure {
    Measure { name, unit, value, tolerance }
}

/// What a scenario runs with: the pak, the rate and the stepping.
struct Ctx<'a> {
    pak: &'a Pak,
    rate: Rate,
    stepping: Stepping,
}

impl Ctx<'_> {
    fn sim(&self, map: &str) -> Sim {
        Sim::new(self.pak, map, self.rate, self.stepping)
    }
}

/// A scenario: a name, a line saying what it does, and its script.
struct Scenario {
    name: &'static str,
    what: &'static str,
    run: fn(&Ctx) -> Vec<Measure>,
}

/// e1m1's start corridor, north of the stairs: flat floor from y = 0 to 672
/// at the player's standing height z = 24.03 (x = 480, facing north).
const RUNWAY: [f32; 3] = [480.0, 60.0, 24.03125];

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "jump", what: "a standing jump on e1m1's flat floor", run: jump },
    Scenario { name: "runjump", what: "a running jump (full speed) on e1m1's flat floor", run: run_jump },
    Scenario { name: "accel", what: "from rest, forward held, on flat floor; the view bob while running", run: accel },
    Scenario { name: "friction", what: "at full speed, forward let go: the slide to a stop", run: friction },
    Scenario { name: "stairs", what: "running up and down e1m1's first stairs (6 steps of 8 units, then 16)", run: stairs },
    Scenario { name: "fall", what: "the drop height (feet above floor) that makes the landing sound, and fall damage", run: fall },
    Scenario { name: "swim", what: "e1m4's deep water: sinking idle, swimming down, swimming up", run: swim },
    Scenario { name: "lava", what: "standing waist-deep in e1m7's lava for 5 s", run: lava },
    Scenario { name: "plat", what: "riding e1m1's lift up (func_plat, 150 u/s)", run: plat },
    Scenario { name: "door", what: "e1m1's first door: opens, waits 3 s, closes", run: door },
    Scenario { name: "toss", what: "a bouncing projectile (MOVETYPE_BOUNCE, the grenade's) thrown up the runway", run: toss },
    Scenario { name: "leap", what: "a monster-sized MOVETYPE_STEP leap (a dog's jump)", run: leap },
    Scenario { name: "grenade", what: "a grenade fired level up the runway", run: grenade },
    Scenario { name: "rocket", what: "a rocket fired into the wall at the runway's end: flight, explosion, light, particles", run: rocket },
    Scenario { name: "rocketjump", what: "a rocket at the feet (pitch 80) fired with a jump", run: rocket_jump },
    Scenario { name: "weapons", what: "fire held: shotgun for 3 s, nailgun for 2 s", run: weapons },
    Scenario { name: "trails", what: "trail particles per 100 units behind a gib, a grenade and a rocket", run: trails },
    Scenario { name: "grunt", what: "an e1m1 grunt woken 192 units away: its first shot, shots and damage in 8 s", run: grunt },
    Scenario { name: "quad", what: "impulse 255: how long the Quad Damage lasts", run: quad },
    Scenario { name: "flash", what: "a hit's damage flash and view kick, and a pickup's bonus flash", run: flash },
    Scenario { name: "clocks", what: "long sessions: the client's host clock and a door's ltime, one hour in", run: clocks },
    Scenario { name: "demo", what: "demo1's first 20 s: how often the recorded view moves on screen", run: demo },
];

fn jump(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place(RUNWAY, 90.0);
    let z0 = f64::from(s.origin()[2]);
    // The first frame with the key down jumps (PlayerJump) and moves.
    s.w.in_jump = true;
    let (mut z, mut t_down, mut v_land, mut airborne) = (Series::default(), f64::NAN, 0.0, false);
    let t0 = s.t;
    s.run_until(2.0, |s| {
        let t = s.t - t0;
        z.push(t, f64::from(s.origin()[2]) - z0);
        if !s.on_ground() {
            airborne = true;
            v_land = f64::from(s.velocity()[2]);
            false
        } else if airborne {
            t_down = t;
            true
        } else {
            false
        }
    });
    vec![
        m("apex", "u", z.max(), 0.1),
        m("air time", "s", t_down, 1.0 / 72.0),
        m("last airborne speed", "u/s", -v_land, 12.0),
    ]
}

fn run_jump(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let z0 = f64::from(s.origin()[2]);
    s.w.in_fwd = 1.0;
    let (mut y_up, mut y_down, mut apex, mut phase) = (0.0, 0.0, 0.0f64, 0);
    let mut y_prev = f64::from(s.origin()[1]);
    s.run_until(3.0, |s| {
        let o = s.origin();
        match phase {
            0 if o[1] >= 200.0 => {
                s.w.in_jump = true;
                phase = 1;
            }
            1 if !s.on_ground() => {
                // The frame that jumped started where the one before ended.
                y_up = y_prev;
                phase = 2;
            }
            2 => {
                apex = apex.max(f64::from(o[2]) - z0);
                if s.on_ground() {
                    y_down = f64::from(o[1]);
                    return true;
                }
            }
            _ => {}
        }
        y_prev = f64::from(o[1]);
        false
    });
    vec![m("apex", "u", apex, 0.1), m("jump length", "u", y_down - y_up, 6.0)]
}

fn accel(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let (y0, t0) = (f64::from(s.origin()[1]), s.t);
    s.w.in_fwd = 1.0;
    let (mut speed, mut y, mut bob) = (Series::default(), Series::default(), (f64::MAX, f64::MIN));
    s.run(1.5, |s| {
        let t = s.t - t0;
        speed.push(t, f64::from(s.speed_xy()));
        y.push(t, f64::from(s.origin()[1]) - y0);
        if t > 0.5 {
            let b = f64::from(render::view_bob(s.speed_xy(), s.w.clock));
            bob = (bob.0.min(b), bob.1.max(b));
        }
    });
    // Accepted: id's 72 Hz frame moves at the speed it ends with, so a 72 Hz
    // start is about half a frame ahead of a 480 Hz one (FRAMERATE.md).
    vec![
        m("time to 200 u/s", "s", speed.rises_to(200.0), 1.0 / 72.0),
        m("time to 319 u/s", "s", speed.rises_to(319.0), 1.0 / 72.0),
        m("distance at 0.25 s", "u", y.at(0.25), 3.0),
        m("distance at 1 s", "u", y.at(1.0), 3.0),
        m("top speed", "u/s", speed.max(), 0.1),
        m("view bob, peak to peak", "u", bob.1 - bob.0, 0.1),
    ]
}

fn friction(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    s.w.in_fwd = 1.0;
    s.run_until(2.0, |s| s.origin()[1] >= 250.0);
    s.w.in_fwd = 0.0;
    let (y0, t0, v0) = (f64::from(s.origin()[1]), s.t, f64::from(s.speed_xy()));
    let mut speed = Series::default();
    speed.push(0.0, v0);
    s.run_until(2.0, |s| {
        speed.push(s.t - t0, f64::from(s.speed_xy()));
        s.speed_xy() == 0.0
    });
    // Accepted: the explicit friction decay (a 480 Hz slide is 5% longer).
    vec![
        m("time to 100 u/s", "s", speed.falls_to(100.0), 1.0 / 72.0),
        m("time to stop", "s", speed.falls_to(0.0), 1.0 / 72.0),
        m("slide", "u", f64::from(s.origin()[1]) - y0, 4.0),
    ]
}

fn stairs(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    // Up: from the corridor floor (z 24) south, up six 8-unit steps to 72,
    // then one of 16 to the start room (88).
    s.place([RUNWAY[0], 120.0, RUNWAY[2]], 270.0);
    s.w.in_fwd = 1.0;
    let t0 = s.t;
    let (mut y, mut lag) = (Series::default(), 0.0f32);
    s.run_until(3.0, |s| {
        let o = s.origin();
        y.push(s.t - t0, -f64::from(o[1]));
        if s.w.oldz.is_finite() {
            lag = lag.max(o[2] - s.w.oldz);
        }
        o[1] < -330.0
    });
    let up = y.rises_to(300.0) - y.rises_to(-100.0);
    // Down: back north from the top.
    s.place([RUNWAY[0], -300.0, 88.03125], 90.0);
    s.w.in_fwd = 1.0;
    let t0 = s.t;
    let (mut y, mut fall) = (Series::default(), 0.0f32);
    s.run_until(3.0, |s| {
        let o = s.origin();
        y.push(s.t - t0, f64::from(o[1]));
        fall = fall.min(s.velocity()[2]);
        o[1] > 150.0
    });
    let down = y.rises_to(100.0) - y.rises_to(-300.0);
    vec![
        // A run down stairs is a string of short falls and landings.
        m("up 400 units", "s", up, 0.03),
        m("view lag on the steps, max", "u", f64::from(lag), 1.0),
        m("down 400 units", "s", down, 0.03),
        m("fastest fall going down", "u/s", -f64::from(fall), 12.0),
    ]
}

/// e1m1's tall room: 440 units clear above the floor (origin z -376 standing).
const DROP_SPOT: [f32; 3] = [704.0, 2160.0, -375.96875];

fn fall(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.mortal();
    // One drop from `h` units above the floor: (landing sound, damage).
    let drop = |s: &mut Sim, h: f32| -> (bool, bool) {
        let p = s.player();
        s.vm().ent_set_float(p, "health", 100.0);
        s.place(DROP_SPOT, 0.0);
        let t0 = s.t;
        set_origin(s.vm(), p, [DROP_SPOT[0], DROP_SPOT[1], DROP_SPOT[2] + h]);
        s.set_flag(FL_ONGROUND, false);
        s.run_until(3.0, |s| s.on_ground());
        s.run(0.05, |_| {});
        let land = s.count_sounds("player/land.wav", t0) + s.count_sounds("player/land2.wav", t0) > 0;
        (land, s.vm().ent_get_float(p, "health") < 100.0)
    };
    // The smallest height in [lo, hi] for which the landing sound (or the
    // damage) happens.
    let threshold = |s: &mut Sim, lo: f32, hi: f32, damage: bool| -> f64 {
        let (mut lo, mut hi) = (lo, hi);
        while hi - lo > 0.05 {
            let mid = 0.5 * (lo + hi);
            let (land, dmg) = drop(s, mid);
            if if damage { dmg } else { land } {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        f64::from(hi)
    };
    let sound = threshold(&mut s, 20.0, 120.0, false);
    let damage = threshold(&mut s, 180.0, 400.0, true);
    // QuakeC judges a landing by `jump_flag`, the speed at the end of the last
    // frame in the air: up to a frame of gravity short of the impact, 11 u/s
    // at 72 Hz — which moves the thresholds by up to 9 units with the phase.
    vec![m("landing sound from", "u", sound, 4.5), m("fall damage from", "u", damage, 9.0)]
}

/// e1m4's flooded cavern: water from the floor (the player standing at z
/// 344) to 852 over (320, 1284).
const WATER: [f32; 3] = [320.0, 1284.0, 600.0];

fn swim(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m4");
    // One second from rest at WATER, looking `pitch` down, with forward
    // and/or up held: the height gained.
    let dive = |s: &mut Sim, pitch: f32, fwd: bool, up: bool| -> Series {
        s.teleport(WATER, 0.0);
        s.w.pitch = pitch;
        s.w.in_fwd = if fwd { 1.0 } else { 0.0 };
        s.w.in_jump = up;
        let (t0, z0) = (s.t, s.origin()[2]);
        let mut z = Series::default();
        s.run(1.0, |s| z.push(s.t - t0, f64::from(s.origin()[2] - z0)));
        s.release();
        z
    };
    let sink = dive(&mut s, 0.0, false, false);
    let down = dive(&mut s, 80.0, true, false);
    let up = dive(&mut s, 0.0, false, true);
    // Accepted: QuakeC's WaterMove drag and the water friction are explicit
    // decays (2-3% further in a second at 480 Hz).
    vec![
        m("idle sink in 0.9 s", "u", -sink.at(0.9), 1.5),
        m("swim down in 0.25 s", "u", -down.at(0.25), 1.5),
        m("swim down in 0.9 s", "u", -down.at(0.9), 4.0),
        m("swim up in 0.9 s", "u", up.at(0.9), 1.5),
    ]
}

fn lava(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m7");
    s.place([640.0, 64.0, -47.96875], 0.0);
    s.mortal();
    let p = s.player();
    let h0 = s.vm().ent_get_float(p, "health");
    let (mut hits, mut last) = (0, h0);
    s.run(5.0, |s| {
        let h = s.w.server.vm.ent_get_float(p, "health");
        if h < last {
            hits += 1;
        }
        last = h;
    });
    // Accepted: QuakeC burns once `time` passes `dmgtime = time + 0.2`, which
    // a frame's end rounds up: every 15th 72 Hz frame (0.208 s), every 97th
    // at 480 Hz (0.202 s) — one burn more in 5 s.
    vec![m("damage in 5 s", "hp", f64::from(h0 - last), 20.0), m("burns in 5 s", "", f64::from(hits), 1.0)]
}

fn plat(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    // The first lift, down: its top at z -120 over (-593..-495, 2623..2689).
    let lift = *s.find("plat").first().expect("e1m1 has lifts");
    let top0 = s.vm().ent_get_vector(lift, "absmax")[2] - 1.0;
    let lz0 = s.vm().ent_get_vector(lift, "origin")[2];
    s.teleport([-544.0, 2656.0, top0 + 24.03125], 0.0);
    let (t0, pz0) = (s.t, s.origin()[2]);
    let (mut lz, mut gap, mut air) = (Series::default(), 0.0f32, 0);
    s.run(2.0, |s| {
        let l = s.w.server.vm.ent_get_vector(lift, "origin")[2];
        lz.push(s.t - t0, f64::from(l - lz0));
        gap = gap.max(((s.origin()[2] - pz0) - (l - lz0)).abs());
        if !s.on_ground() {
            air += 1;
        }
    });
    let rise = lz.max();
    vec![
        m("lift reaches the top", "s", lz.rises_to(rise - 0.01), 1.0 / 72.0),
        m("rider off the floor, max", "u", f64::from(gap), 0.5),
        m("rider frames airborne", "", f64::from(air), f64::NAN),
    ]
}

fn door(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    // The first door pair (speed 400, wait 3), opened the way walking into
    // its trigger field does.
    let door = s.find("door").into_iter().min().expect("e1m1 has doors");
    let trigger = door_trigger(&s, door);
    let p = s.player();
    let o0 = s.vm().ent_get_vector(door, "origin");
    call_qc_with(s.vm(), "door_trigger_touch", trigger, p, p);
    let t0 = s.t;
    let mut d = Series::default();
    s.run(5.5, |s| {
        let o = s.w.server.vm.ent_get_vector(door, "origin");
        d.push(s.t - t0, f64::from(((o[0] - o0[0]).powi(2) + (o[1] - o0[1]).powi(2) + (o[2] - o0[2]).powi(2)).sqrt()));
    });
    let open = d.max();
    let opened = d.rises_to(open - 0.01);
    let later = d.after(opened + 1.0);
    // A pusher's think ends its frame's move (SV_Physics_Pusher stops at
    // `nextthink`), so each of the door's thinks (open, wait over, closed)
    // delays what follows by up to a frame: up to 1/72 s each in id's game.
    vec![
        m("open", "s", opened, 1.0 / 72.0),
        m("starts closing", "s", later.falls_to(open - 0.01), 2.0 / 72.0),
        m("closed", "s", later.falls_to(0.01), 3.0 / 72.0),
    ]
}

fn toss(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let e = spawn_mover(&mut s, MOVETYPE_BOUNCE, SOLID_NOT, [0.0; 3], [0.0; 3], [RUNWAY[0], 20.0, 40.0], [0.0, 150.0, 350.0]);
    let t0 = s.t;
    let (mut z, mut y) = (Series::default(), Series::default());
    let (mut rest, mut bounces, mut vz_prev) = (f64::NAN, 0, 350.0f32);
    s.run(3.0, |s| {
        let vm = &s.w.server.vm;
        let (o, v) = (vm.ent_get_vector(e, "origin"), vm.ent_get_vector(e, "velocity"));
        z.push(s.t - t0, f64::from(o[2]));
        y.push(s.t - t0, f64::from(o[1]));
        if v[2] > 0.0 && vz_prev <= 0.0 {
            bounces += 1;
        }
        vz_prev = v[2];
        if rest.is_nan() && v == [0.0; 3] {
            rest = s.t - t0;
        }
    });
    vec![
        m("apex", "u", z.max() - 40.0, 0.2),
        m("first landing", "s", z.falls_to(0.5), 1.0 / 72.0),
        m("height at 0.5 s", "u", z.at(0.5), 0.3),
        m("bounces", "", f64::from(bounces), 0.0),
        m("comes to rest", "s", rest, 2.0 / 72.0),
        m("rests at", "u", y.at(3.0) - 20.0, 3.0),
    ]
}

fn leap(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    // A dog-sized box launched as dog_leap does: `v_forward * 300 + '0 0 200'`.
    let e = spawn_mover(&mut s, MOVETYPE_STEP, SOLID_SLIDEBOX, [-16.0, -16.0, -24.0], [16.0, 16.0, 40.0], [RUNWAY[0], 150.0, 25.0], [0.0, 300.0, 200.0]);
    let t0 = s.t;
    let (mut z, mut land) = (Series::default(), (f64::NAN, 0.0));
    s.run_until(2.0, |s| {
        let vm = &s.w.server.vm;
        let o = vm.ent_get_vector(e, "origin");
        z.push(s.t - t0, f64::from(o[2] - 25.0));
        if vm.ent_get_float(e, "flags") as i32 & FL_ONGROUND != 0 {
            land = (s.t - t0, f64::from(o[1] - 150.0));
            return true;
        }
        false
    });
    vec![m("apex", "u", z.max(), 0.1), m("air time", "s", land.0, 1.0 / 72.0), m("leap length", "u", land.1, 4.5)]
}

fn grenade(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    s.arm(IT_GRENADE_LAUNCHER);
    s.w.in_attack = true;
    let t0 = s.t;
    s.run_until(0.5, |s| !s.find("grenade").is_empty());
    s.w.in_attack = false;
    let g = *s.find("grenade").first().expect("a grenade");
    let (mut y, mut z, mut boom) = (Series::default(), Series::default(), (f64::NAN, [0.0f32; 3]));
    let mut last = s.vm().ent_get_vector(g, "origin");
    s.run_until(3.5, |s| {
        let vm = &s.w.server.vm;
        last = vm.ent_get_vector(g, "origin");
        if vm.ent_string_ref(g, "model") == "progs/s_explod.spr" {
            boom = (s.t - t0, last);
            return true;
        }
        y.push(s.t - t0, f64::from(last[1]));
        z.push(s.t - t0, f64::from(last[2]));
        false
    });
    vec![
        m("height at 0.25 s", "u", z.at(0.25), 0.5),
        m("distance at 0.5 s", "u", y.at(0.5), 1.0),
        m("bounces", "", s.count_sounds("weapons/bounce.wav", t0) as f64, 0.0),
        m("explodes after", "s", boom.0, 1.0 / 72.0),
        // Where it stops depends on how fast each bounce leaves the floor, which at
        // 72 Hz varies by up to half a frame of gravity with the impact's phase.
        m("explodes at y", "u", f64::from(boom.1[1]), 8.5),
        m("explodes at z", "u", f64::from(boom.1[2]), 2.0),
    ]
}

/// The explosion particles alive: how many, and their mean distance from
/// `center`.
fn explosion_cloud(w: &Walk, center: [f32; 3]) -> (f64, f64) {
    let ps: Vec<_> = w
        .particles
        .particles()
        .iter()
        .filter(|p| matches!(p.kind, ParticleKind::Explode | ParticleKind::Explode2))
        .collect();
    let r: f64 = ps
        .iter()
        .map(|p| f64::from(((p.origin[0] - center[0]).powi(2) + (p.origin[1] - center[1]).powi(2) + (p.origin[2] - center[2]).powi(2)).sqrt()))
        .sum();
    (ps.len() as f64, r / ps.len().max(1) as f64)
}

fn rocket(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 300.0, RUNWAY[2]], 90.0);
    s.arm(IT_ROCKET_LAUNCHER);
    s.w.in_attack = true;
    s.run_until(0.5, |s| !s.find("missile").is_empty());
    s.w.in_attack = false;
    let t0 = s.t;
    let r = *s.find("missile").first().expect("a rocket");
    let mut boom = (f64::NAN, [0.0f32; 3]);
    s.run_until(2.0, |s| {
        let vm = &s.w.server.vm;
        if vm.ent_string_ref(r, "model") == "progs/s_explod.spr" {
            boom = (s.t - t0, vm.ent_get_vector(r, "origin"));
            return true;
        }
        false
    });
    // The explosion's frame drew its particles where they were born and then
    // moved them a frame: after a frame ending at `t` they have moved for
    // `t - (tb - dt)`.
    let (tb, born) = (s.t, s.t - s.dt);
    let (mut light, mut count, mut spread) = (Series::default(), Series::default(), Series::default());
    s.run(0.6, |s| {
        let now = s.w.clock;
        let radius = s.w.dlights.active().iter().filter(|d| d.die >= now).map(|d| d.radius).fold(0.0f32, f32::max);
        light.push(s.t - tb, f64::from(radius));
        let (n, r) = explosion_cloud(&s.w, boom.1);
        count.push(s.t - born, n);
        spread.push(s.t - born, r);
    });
    let cloud = (count.at(0.3), spread.at(0.3));
    vec![
        m("hits the wall after", "s", boom.0, 1.0 / 72.0),
        m("explodes at y", "u", f64::from(boom.1[1]), 0.5),
        m("light radius at 0.2 s", "u", light.at(0.2), 5.0),
        m("light gone after", "s", light.falls_to(0.5), 1.0 / 72.0),
        // The explosion's random directions come from the client's random
        // sequence, which the trails before it draw from: a few percent.
        m("explosion particles at 0.3 s", "", cloud.0, 20.0),
        m("their mean distance", "u", cloud.1, 6.0),
    ]
}

fn rocket_jump(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 200.0, RUNWAY[2]], 90.0);
    s.arm(IT_ROCKET_LAUNCHER);
    let z0 = f64::from(s.origin()[2]);
    s.w.pitch = 80.0;
    s.w.in_attack = true;
    s.w.in_jump = true;
    let t0 = s.t;
    let mut z = Series::default();
    s.run(1.5, |s| {
        z.push(s.t - t0, f64::from(s.origin()[2]) - z0);
        if !s.find("missile").is_empty() {
            s.w.in_attack = false;
        }
    });
    vec![m("apex", "u", z.max(), 2.0)]
}

fn weapons(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let p = s.player();
    s.arm(IT_SHOTGUN);
    s.run(0.6, |_| {});
    s.w.in_attack = true;
    let t0 = s.t;
    let (mut shots, mut last) = (Vec::new(), s.vm().ent_get_float(p, "ammo_shells"));
    s.run(3.0, |s| {
        let a = s.w.server.vm.ent_get_float(p, "ammo_shells");
        if a < last {
            shots.push(s.t - t0);
        }
        last = a;
    });
    s.w.in_attack = false;
    let shotgun_gap = (shots[shots.len() - 1] - shots[0]) / (shots.len() - 1) as f64;
    s.arm(IT_NAILGUN);
    s.run(1.0, |_| {});
    s.w.in_attack = true;
    let n0 = s.vm().ent_get_float(p, "ammo_nails");
    s.run(2.0, |_| {});
    s.w.in_attack = false;
    let nails = n0 - s.vm().ent_get_float(p, "ammo_nails");
    vec![
        m("shotgun shots in 3 s", "", shots.len() as f64, 0.0),
        m("shotgun: time between shots", "s", shotgun_gap, 1.0 / 72.0),
        m("nails in 2 s", "", f64::from(nails), 1.0),
    ]
}

fn trails(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    // A model flying level up the runway at `speed`: the client trails it
    // from its previous origin every frame (CL_RelinkEntities). Counts the
    // particles each frame spawned (a fresh trail particle dies 2 s on).
    let per_100 = |s: &mut Sim, model: &str, speed: f32, kind: fn(&ParticleKind) -> bool| -> f64 {
        let e = spawn_mover(s, MOVETYPE_NOCLIP, SOLID_NOT, [0.0; 3], [0.0; 3], [RUNWAY[0], 40.0, 40.0], [0.0, speed, 0.0]);
        set_model(s.vm(), e, model);
        let (mut spawned, mut dist) = (0usize, 0.0f64);
        s.run(0.4, |s| {
            let now = s.w.clock;
            spawned += s.w.particles.particles().iter().filter(|p| kind(&p.kind) && p.die > now + 1.999).count();
            dist = f64::from(s.w.server.vm.ent_get_vector(e, "origin")[1] - 40.0);
        });
        s.vm().free_edict(e);
        s.run(2.1, |_| {});
        100.0 * spawned as f64 / dist
    };
    let blood = per_100(&mut s, "progs/gib1.mdl", 150.0, |k| matches!(k, ParticleKind::Grav));
    let smoke = per_100(&mut s, "progs/grenade.mdl", 600.0, |k| matches!(k, ParticleKind::Fire));
    let fire = per_100(&mut s, "progs/missile.mdl", 1000.0, |k| matches!(k, ParticleKind::Fire));
    vec![
        m("gib blood (150 u/s)", "/100u", blood, 3.0),
        m("grenade smoke (600 u/s)", "/100u", smoke, 3.0),
        m("rocket fire (1000 u/s)", "/100u", fire, 3.0),
    ]
}

fn grunt(c: &Ctx) -> Vec<Measure> {
    // Five fights, each after a different number of `random()` draws: the
    // grunt's choices are random, so compare the averages.
    const FIGHTS: usize = 5;
    let (mut first, mut shots, mut damage, mut frames) = (0.0, 0.0, 0.0, 0.0);
    for fight in 0..FIGHTS {
        let mut s = c.sim("e1m1");
        s.mortal();
        let p = s.player();
        // e1m1's grunt in the room north of the start corridor.
        let g = s
            .find("monster_army")
            .into_iter()
            .find(|&g| s.w.server.vm.ent_get_vector(g, "origin")[..2] == [0.0, 576.0])
            .expect("e1m1's first-room grunt");
        let go = s.vm().ent_get_vector(g, "origin");
        s.place([go[0] + 192.0, go[1], go[2]], 180.0);
        s.set_flag(FL_NOTARGET, false);
        for _ in 0..fight * 7 {
            random(s.vm());
        }
        let h0 = s.vm().ent_get_float(p, "health");
        {
            let vm = s.vm();
            vm.ent_set_int(g, "enemy", p);
            call_qc_with(vm, "FoundTarget", g, 0, p);
        }
        let t0 = s.t;
        let mut last = s.vm().ent_get_float(g, "frame");
        s.run(8.0, |s| {
            let f = s.w.server.vm.ent_get_float(g, "frame");
            if f != last {
                frames += 1.0;
            }
            last = f;
        });
        let fired: Vec<f64> = s.sounds.iter().filter(|(t, n)| *t > t0 && n == "soldier/sattck1.wav").map(|(t, _)| t - t0).collect();
        first += fired.first().copied().unwrap_or(8.0);
        shots += fired.len() as f64;
        damage += f64::from(h0 - s.vm().ent_get_float(p, "health"));
    }
    let n = FIGHTS as f64;
    vec![
        m("first shot, mean", "s", first / n, f64::NAN),
        m("shots in 8 s, mean", "", shots / n, f64::NAN),
        m("damage in 8 s, mean", "hp", damage / n, f64::NAN),
        m("animation frames in 8 s, mean", "", frames / n, 1.0),
    ]
}

fn quad(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let p = s.player();
    s.w.next_impulse = 255;
    s.frame();
    let t0 = s.t;
    s.run_until(35.0, |s| s.w.server.vm.ent_get_float(p, "items") as i32 & IT_QUAD == 0);
    vec![m("quad lasts", "s", s.t - t0, 1.0 / 72.0)]
}

fn flash(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    let p = s.player();
    // A 25-point hit from the world (svc_damage: V_ParseDamage).
    s.vm().ent_set_float(p, "dmg_take", 25.0);
    s.vm().ent_set_int(p, "dmg_inflictor", 0);
    s.frame();
    let t0 = s.t;
    let (mut pct, mut kick) = (Series::default(), Series::default());
    pct.push(0.0, f64::from(s.w.damage_blend));
    kick.push(0.0, f64::from(s.w.v_dmg_time));
    s.run(1.0, |s| {
        pct.push(s.t - t0, f64::from(s.w.damage_blend));
        kick.push(s.t - t0, f64::from(s.w.v_dmg_time));
    });
    // A pickup's bonus flash (the server stuffs "bf").
    stuffcmd(s.vm(), p, "bf\n");
    s.frame();
    let t0 = s.t;
    let mut bonus = Series::default();
    bonus.push(0.0, f64::from(s.w.bonus_blend));
    s.run(1.0, |s| bonus.push(s.t - t0, f64::from(s.w.bonus_blend)));
    vec![
        m("damage flash, first frame", "%", pct.0[0].1, 3.0),
        m("damage flash gone after", "s", pct.falls_to(0.5), 0.02),
        m("view kick gone after", "s", kick.falls_to(0.0), 1.0 / 72.0),
        m("bonus flash gone after", "s", bonus.falls_to(0.5), 0.02),
    ]
}

fn clocks(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    s.place([RUNWAY[0], 0.0, RUNWAY[2]], 90.0);
    // An hour into the level. The host clock times the notify lines and
    // centre prints; it runs 10 s.
    s.w.host_time = 3600.0;
    let (t0, h0) = (s.t, s.w.host_time);
    s.run(10.0, |_| {});
    let host = f64::from(s.w.host_time - h0) / (s.t - t0) * 10.0;
    // A door whose `ltime` (its own clock, advanced only while a move is
    // under way) has reached an hour: SUB_CalcMove ends its move when
    // `ltime` reaches `nextthink`, then snaps it onto the end.
    let door = s.find("door").into_iter().min().expect("e1m1 has doors");
    let trigger = door_trigger(&s, door);
    s.vm().ent_set_float(door, "ltime", 3600.0);
    let (p, o0) = (s.player(), s.vm().ent_get_vector(door, "origin"));
    call_qc_with(s.vm(), "door_trigger_touch", trigger, p, p);
    let t0 = s.t;
    let (mut d, mut prev, mut snap) = (Series::default(), 0.0f64, 0.0f64);
    s.run(0.5, |s| {
        let o = s.w.server.vm.ent_get_vector(door, "origin");
        let dist = f64::from(((o[0] - o0[0]).powi(2) + (o[1] - o0[1]).powi(2) + (o[2] - o0[2]).powi(2)).sqrt());
        d.push(s.t - t0, dist);
        // The door's speed is 400: a step past what a frame covers is the snap.
        snap = snap.max(dist - prev - 400.0 * s.dt);
        prev = dist;
    });
    vec![
        m("host clock, 10 s at 1 h", "s", host, 0.02),
        m("door opens in, at 1 h", "s", d.rises_to(d.max() - 0.01), 1.0 / 72.0),
        m("door's snap at the end, at 1 h", "u", snap.max(0.0), 0.5),
    ]
}

fn demo(c: &Ctx) -> Vec<Measure> {
    quake_rs::server::reset_random();
    let mut d: DemoPlay = cl_demo::build_demo_n(c.pak.clone(), 0, &mut Vec::new()).expect("demo1");
    d.stepping = c.stepping;
    let lerp = c.stepping == Stepping::Uncapped;
    let mut clock = FrameClock::new(c.rate, c.stepping);
    let (mut t, mut moves, mut last) = (0.0f64, 0usize, ([0.0f32; 3], [0.0f32; 3]));
    while t < 20.0 {
        let dt = clock.next();
        t += dt;
        let frame = cl_demo::demo_frame(&mut d, dt as f32, false, &VID);
        render::recycle_image(frame.image);
        // The POV the frame drew.
        let v = cl_demo::demo_view(&d.demo.frames, d.idx, d.elapsed, lerp);
        let pov = (v.view_origin, v.view_angles);
        if pov != last {
            moves += 1;
        }
        last = pov;
    }
    vec![m("camera moves a second", "/s", moves as f64 / t, f64::NAN), m("demo message at 20 s", "", d.idx as f64, 2.0)]
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// One quantity's results: the 72 Hz reference, and per rate (id's
/// per-frame code, uncapped).
struct Row {
    name: &'static str,
    unit: &'static str,
    tolerance: f64,
    reference: f64,
    cells: Vec<(f64, f64)>,
}

fn fmt(v: f64) -> String {
    if v.is_nan() {
        "—".into()
    } else if v.abs() >= 100.0 || v.fract() == 0.0 {
        format!("{v:.1}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

pub fn cmd_framerate(pak_path: &str, rest: &[String]) -> Result<String, String> {
    let mut rates = vec![Rate::Hz(60), Rate::Hz(144), Rate::Hz(240), Rate::Hz(480), Rate::Jitter];
    let (mut only, mut markdown, mut check) = (None::<Vec<String>>, false, false);
    let (mut budget, mut res) = (false, "1280x800,1280x1024".to_string());
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--rates" => {
                let v = rest.get(i + 1).ok_or("--rates needs a list")?;
                rates = v.split(',').map(Rate::parse).collect::<Result<_, _>>()?;
                rates.retain(|&r| r != Rate::Hz(72));
                i += 1;
            }
            "--only" => {
                only = Some(rest.get(i + 1).ok_or("--only needs names")?.split(',').map(String::from).collect());
                i += 1;
            }
            "--markdown" => markdown = true,
            "--check" => check = true,
            "--budget" => budget = true,
            "--res" => {
                res = rest.get(i + 1).ok_or("--res needs WxH[,WxH...]")?.clone();
                i += 1;
            }
            a => return Err(format!("unknown argument {a:?}")),
        }
        i += 1;
    }
    let bytes = std::fs::read(pak_path).map_err(|e| format!("cannot read {pak_path}: {e}"))?;
    let pak = Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())?;
    if budget {
        let sizes = res.split(',').map(super::parse_res).collect::<Result<Vec<_>, _>>()?;
        return Ok(frame_budget(&pak, &sizes));
    }

    let mut o = String::new();
    let mut failures = Vec::new();
    for sc in SCENARIOS {
        if only.as_ref().is_some_and(|names| !names.iter().any(|n| n == sc.name)) {
            continue;
        }
        let run = |rate, stepping| (sc.run)(&Ctx { pak: &pak, rate, stepping });
        let reference = run(Rate::Hz(72), Stepping::Classic);
        let mut rows: Vec<Row> = reference
            .iter()
            .map(|r| Row { name: r.name, unit: r.unit, tolerance: r.tolerance, reference: r.value, cells: Vec::new() })
            .collect();
        for &rate in &rates {
            let id = run(rate, Stepping::Classic);
            let uncapped = run(rate, Stepping::Uncapped);
            for (row, (a, b)) in rows.iter_mut().zip(id.iter().zip(&uncapped)) {
                row.cells.push((a.value, b.value));
                // A NaN (a quantity that did not happen) is outside any tolerance.
                let within = (b.value - row.reference).abs() <= row.tolerance + 1e-9;
                if check && !row.tolerance.is_nan() && !within {
                    failures.push(format!(
                        "{} / {} at {}: {} vs {} at 72 (tolerance ±{})",
                        sc.name,
                        row.name,
                        rate.label(),
                        fmt(b.value),
                        fmt(row.reference),
                        fmt(row.tolerance)
                    ));
                }
            }
        }
        report(&mut o, sc, &rates, &rows, markdown);
    }
    if check {
        if failures.is_empty() {
            let _ = writeln!(o, "check: every uncapped value is within its tolerance of 72 Hz");
        } else {
            return Err(format!("{o}check failed:\n  {}", failures.join("\n  ")));
        }
    }
    Ok(o)
}

fn report(o: &mut String, sc: &Scenario, rates: &[Rate], rows: &[Row], markdown: bool) {
    if markdown {
        let _ = writeln!(o, "**{}** — {}\n", sc.name, sc.what);
        let head: Vec<String> = rates.iter().map(|r| r.label()).collect();
        let _ = writeln!(o, "| quantity | 72 (id) | {} | tolerance |", head.join(" | "));
        let _ = writeln!(o, "|---|---|{}---|", "---|".repeat(rates.len()));
        for r in rows {
            let unit = if r.unit.is_empty() { String::new() } else { format!(" ({})", r.unit) };
            let delta = |v: f64| match v - r.reference {
                d if d.is_nan() => String::new(),
                d if d.abs() < 5e-4 => " (0)".into(),
                d => format!(" ({}{})", if d > 0.0 { "+" } else { "−" }, fmt(d.abs())),
            };
            let cells: Vec<String> = r.cells.iter().map(|&(a, b)| format!("{} → {}{}", fmt(a), fmt(b), delta(b))).collect();
            let tol = if r.tolerance.is_nan() { "—".into() } else { format!("±{}", fmt(r.tolerance)) };
            let _ = writeln!(o, "| {}{unit} | {} | {} | {tol} |", r.name, fmt(r.reference), cells.join(" | "));
        }
        let _ = writeln!(o);
    } else {
        let _ = writeln!(o, "{} — {}", sc.name, sc.what);
        let head: String = rates.iter().map(|r| format!(" {:>17}", r.label())).collect();
        let _ = writeln!(o, "  {:<34} {:>9}{head}", "quantity (id's → uncapped)", "72 (id)");
        for r in rows {
            let name = if r.unit.is_empty() { r.name.to_string() } else { format!("{} ({})", r.name, r.unit) };
            let cells: String = r.cells.iter().map(|&(a, b)| format!(" {:>17}", format!("{} → {}", fmt(a), fmt(b)))).collect();
            let _ = writeln!(o, "  {name:<34} {:>9}{cells}", fmt(r.reference));
        }
    }
}

// ---------------------------------------------------------------------------
// Budget: what a frame costs at 480 Hz
// ---------------------------------------------------------------------------

thread_local! {
    /// The frame timer's state: the last lap's instant and each phase's
    /// time this frame (s).
    static LAPS: RefCell<(Option<Instant>, [f64; 4])> = const { RefCell::new((None, [0.0; 4])) };
}

/// The frame timer installed on the client (`client::set_lap_hook`): the
/// client frames lap `Sim` (input, the server frame, the client side of its
/// messages), `Render3d`, `Post3d` (warp, blends, compose) and `Hud2d`.
fn lap(phase: Phase) {
    let slot = match phase {
        Phase::Sim => 0,
        Phase::Render3d => 1,
        Phase::Post3d => 2,
        Phase::Hud2d => 3,
        _ => return,
    };
    LAPS.with(|l| {
        let mut l = l.borrow_mut();
        let now = Instant::now();
        if let Some(last) = l.0 {
            l.1[slot] += (now - last).as_secs_f64();
        }
        l.0 = Some(now);
    });
}

/// Start a frame's laps.
fn lap_start() {
    LAPS.with(|l| *l.borrow_mut() = (Some(Instant::now()), [0.0; 4]));
}

fn lap_times() -> [f64; 4] {
    LAPS.with(|l| l.borrow().1)
}

/// The median and 95th percentile of `xs` (ms).
fn median_p95(xs: &mut [f64]) -> (f64, f64) {
    xs.sort_by(f64::total_cmp);
    let at = |q: f64| xs[((xs.len() - 1) as f64 * q).round() as usize] * 1000.0;
    (at(0.5), at(0.95))
}

/// `--budget [--res WxH,...]`: the native cost of a frame of the uncapped
/// client at 480 Hz — the live game on e1m1 (bench.py's walk: a turn in
/// place, runs, about-faces; the level's monsters awake) and demo1's
/// playback — per phase, against the frame budgets of 480, 240 and 144 Hz.
fn frame_budget(pak: &Pak, sizes: &[(usize, usize)]) -> String {
    const FRAMES: usize = 2400;
    let mut o = String::new();
    let _ = writeln!(o, "per-frame cost at 480 Hz (Stepping::Uncapped), median / p95 ms over {FRAMES} frames");
    let _ = writeln!(o, "  {:<22} {:>13} {:>13} {:>13} {:>13}   headroom at 480 / 240 / 144 Hz", "", "sim", "3-D view", "post + 2-D", "total");
    quake_rs::client::set_lap_hook(Some(lap));
    for &(w, h) in sizes {
        let vid = Vid { width: w, height: h, ..VID };
        for demo in [false, true] {
            let mut phases: [Vec<f64>; 4] = Default::default();
            let mut total = Vec::with_capacity(FRAMES);
            let mut clock = FrameClock::new(Rate::Hz(480), Stepping::Uncapped);
            let mut sim = (!demo).then(|| {
                let mut s = Sim::new(pak, "e1m1", Rate::Hz(480), Stepping::Uncapped);
                s.set_flag(FL_GODMODE, true);
                s.set_flag(FL_NOTARGET, false);
                s
            });
            let mut playback = demo.then(|| {
                let mut d = cl_demo::build_demo_n(pak.clone(), 0, &mut Vec::new()).expect("demo1");
                d.stepping = Stepping::Uncapped;
                d
            });
            for f in 0..FRAMES {
                let dt = clock.next();
                lap_start();
                let t0 = Instant::now();
                let frame = if let Some(s) = sim.as_mut() {
                    // bench.py's walk_input, at 480 Hz: a 10 s cycle.
                    let (fwd, turn) = match (f * 72 / 480) % 720 {
                        0..=359 => (0.0, 1.0),
                        360..=431 | 504..=575 => (1.0, 0.0),
                        432..=503 | 576..=647 => (0.0, 2.5),
                        _ => (0.0, 0.0),
                    };
                    s.w.in_fwd = fwd;
                    s.w.yaw -= turn * 72.0 / 480.0;
                    cl_main::walk_frame(&mut s.w, dt, false, &vid)
                } else {
                    cl_demo::demo_frame(playback.as_mut().expect("demo"), dt as f32, false, &vid)
                };
                total.push(t0.elapsed().as_secs_f64());
                for (p, t) in phases.iter_mut().zip(lap_times()) {
                    p.push(t);
                }
                render::recycle_image(frame.image);
            }
            let cell = |xs: &mut Vec<f64>| {
                let (m, p) = median_p95(xs);
                format!("{m:5.2} / {p:5.2}")
            };
            let (m, _) = median_p95(&mut total.clone());
            let what = format!("{w}x{h} {}", if demo { "demo1" } else { "e1m1 live" });
            let _ = writeln!(
                o,
                "  {what:<22} {:>13} {:>13} {:>13} {:>13}   {:+.2} / {:+.2} / {:+.2}",
                cell(&mut phases[0]),
                cell(&mut phases[1]),
                cell(&mut { phases[2].iter().zip(&phases[3]).map(|(a, b)| a + b).collect() }),
                cell(&mut total),
                1000.0 / 480.0 - m,
                1000.0 / 240.0 - m,
                1000.0 / 144.0 - m,
            );
        }
    }
    quake_rs::client::set_lap_hook(None);
    o
}
