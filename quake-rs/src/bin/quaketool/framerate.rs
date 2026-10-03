//! `quaketool framerate <pak> [--rates LIST] [--only NAMES] [--markdown]
//! [--check]` — does the game play the same at every display rate?
//! `quaketool framerate <pak> --budget [--res WxH,...]` — what a frame of it
//! costs at 480 Hz.
//! `quaketool framerate <pak> --lerpmove [--rates LIST] [--strip DIR]` — how
//! smoothly the monsters' steps are drawn, Classic and with `r_lerpmove`
//! ([`LerpMove`]; "Monsters between their steps" below).
//! `quaketool framerate <pak>[,<pak>...] --lightstyles [--rates LIST] [--res WxH]
//! [--threads N] [--reps N] [--secs S] [--view NAME=MAP:X,Y,Z:YAW]...` — what
//! the gliding light styles (`r_lerplightstyles`) cost: surfaces rebaked
//! and the 3-D view's time per frame, standing where lights animate.
//! `quaketool framerate <pak>[,<pak>...] --bake [--threads LIST] [--paced]
//! [the same options]` — what the frame's lit-surface bakes cost on 1 to 16
//! threads: the 3-D view's time and its serial part where many blocks
//! rebake; `--paced` keeps the display's real time between frames.
//! `quaketool framerate <pak>[,<pak>...] --serial [--threads LIST] [--paced]
//! [the same options]` — the page's 2026 frame (exact perspective, the
//! status bar overlay with the world in its corners, the scaled 2-D layer)
//! at the same views: the whole frame's time, and what it does on the
//! calling thread alone, piece by piece (`--overlay 0` after it: id's
//! status bar, one view a frame).
//! `quaketool framerate <pak>[,<pak>...] --torchflicker S [the same options]
//! [--dump DIR [--strengths LIST]]` — the same for the steady torches'
//! flicker (`r_torchflicker` at strength S), standing by torches; with
//! `--dump`, every frame of each view at the first rate as raw RGB instead.
//! `quaketool framerate <pak> --perspspan [--spans 16,8,4,1] [--rates LIST]
//! [--res WxH] [--threads N] [--reps N] [--secs S]
//! [--view NAME=MAP:X,Y,Z:YAW[:PITCH]]...` — what each perspective span
//! (`r_perspspan`) costs: the 3-D view's time per frame with the walls and
//! liquids exact every 16 pixels (id's), 8, 4 or at every pixel, the rest the
//! 2026 profile's (`--exactpersp`: the same with `--spans 16,1`); with
//! `--dump DIR [--turn DEG_S] [--strafe UNITS_S] [--crop X,Y,W,H]`, every
//! frame of each view at each span at the first rate as raw RGB instead, the
//! camera turning or strafing (a clip of the four side by side).
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
//! starts from a fresh session's random streams (`QRand::new`), so the rates
//! differ only by their frame times. With `--check` the command fails when
//! an uncapped value is further from the 72 Hz reference than the scenario's
//! stated tolerance.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::time::Instant;

use quake_rs::client::host::{host_filter_time_display, host_filter_time_uncapped};
use quake_rs::client::lerpmove::LerpMove;
use quake_rs::client::{cl_demo, cl_main, host_cmd, DemoPlay, Phase, SoundCall, Vid, Walk};
use quake_rs::pak::Pak;
use quake_rs::particles::ParticleKind;
use quake_rs::progs::OFS_PARM0;
use quake_rs::render;
use quake_rs::stepping::Stepping;
use quake_rs::vm::Vm;
use quake_rs::world;

/// The screen the scenarios draw (small: they measure the game, not pixels).
const VID: Vid = Vid { width: 320, height: 200, display_aspect: 4.0 / 3.0, persp_span: render::PerspSpan::Spans16, video: render::VideoCvars::CLASSIC, mip: render::MipCvars::DEFAULT };

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
    /// A fresh level: `maps/<map>.bsp` loaded as `map` does, with a fresh
    /// session's random streams, the player in god mode and unseen by monsters.
    fn new(pak: &Pak, map: &str, rate: Rate, stepping: Stepping) -> Sim {
        let path = format!("maps/{map}.bsp");
        let rand = std::rc::Rc::new(quake_rs::qrand::QRand::new());
        let mut w = host_cmd::build_walk_map(pak.clone(), &path, &rand, &mut Vec::new(), quake_rs::vm::MAX_EDICTS)
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
    let _ = vm.call_builtin(2, 2);
}

/// `PF_setmodel` through the engine's builtin.
fn set_model(vm: &mut Vm, e: i32, model: &str) {
    let s = vm.intern(model);
    vm.set_gi(OFS_PARM0, e);
    vm.set_gi(OFS_PARM0 + 3, s);
    let _ = vm.call_builtin(3, 2);
}

/// `PF_setsize` through the engine's builtin.
fn set_size(vm: &mut Vm, e: i32, mins: [f32; 3], maxs: [f32; 3]) {
    vm.set_gi(OFS_PARM0, e);
    vm.set_gv(OFS_PARM0 + 3, mins);
    vm.set_gv(OFS_PARM0 + 6, maxs);
    let _ = vm.call_builtin(4, 3);
}

/// `stuffcmd(e, text)` through the engine's builtin.
fn stuffcmd(vm: &mut Vm, e: i32, text: &str) {
    let s = vm.intern(text);
    vm.set_gi(OFS_PARM0, e);
    vm.set_gi(OFS_PARM0 + 3, s);
    let _ = vm.call_builtin(21, 2);
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
    let _ = vm.call_builtin(7, 0);
}

/// Call the QuakeC function `name` with `self = e`, `other = world`,
/// `activator = e`.
fn call_qc(vm: &mut Vm, name: &str, e: i32) {
    call_qc_with(vm, name, e, 0, e);
}

/// Call the QuakeC function `name` with `self`, `other` and `activator` set
/// and `time = sv.time`, as the engine's callers set them.
fn call_qc_with(vm: &mut Vm, name: &str, self_e: i32, other: i32, activator: i32) {
    let Some(f) = vm.progs().find_function(name) else { panic!("progs.dat has no {name}") };
    let t = vm.sv_time() as f32;
    vm.gset_int("self", self_e);
    vm.gset_int("other", other);
    vm.gset_int("activator", activator);
    vm.gset_float("time", t);
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
    Scenario { name: "step", what: "the view over a 16-unit step: running up it (id's stair smoothing, 80 u/s) and off it", run: step },
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

/// e1m1's last landing on the stairs (origin z 72 standing) and the start
/// room's floor 16 units above it, the riser between them at y = -304.
const STEP_FOOT: [f32; 3] = [480.0, -200.0, 72.03125];
const STEP_TOP: [f32; 3] = [480.0, -340.0, 88.03125];

/// The view over one stair step. The server steps the player up at once
/// (`SV_WalkMove`); the client's `V_CalcRefdef` lowers the eye by the rise,
/// at most 12 units, and lifts it back at 80 u/s while the player stands on
/// the ground (`cl.onground`). `Walk::oldz` is that smoothed height (the
/// eye less its view offset and bob).
fn step(c: &Ctx) -> Vec<Measure> {
    let mut s = c.sim("e1m1");
    // Up: run south from the landing up the riser.
    s.place(STEP_FOOT, 270.0);
    s.w.in_fwd = 1.0;
    let (mut lag, mut t_step, mut level, mut fastest) = (Series::default(), f64::NAN, f64::NAN, 0.0f64);
    let (mut z, mut eye) = (s.origin()[2], f64::from(s.w.oldz));
    s.run_until(1.5, |s| {
        let (o, e) = (s.origin()[2], f64::from(s.w.oldz));
        if t_step.is_nan() && o > z + 8.0 {
            t_step = s.t; // the frame that stepped up
        } else if !t_step.is_nan() {
            fastest = fastest.max((e - eye) / s.dt);
        }
        if !t_step.is_nan() {
            lag.push(s.t - t_step, f64::from(o) - e);
            if level.is_nan() && e >= f64::from(o) {
                level = s.t - t_step;
            }
        }
        (z, eye) = (o, e);
        !level.is_nan()
    });
    // Down: run north off the riser; id smooths only steps up.
    s.place(STEP_TOP, 90.0);
    s.w.in_fwd = 1.0;
    let mut off = 0.0f32;
    s.run_until(1.0, |s| {
        let o = s.origin();
        off = off.max((o[2] - s.w.oldz).abs());
        o[1] > STEP_FOOT[1]
    });
    // At 72 Hz the eye is a frame's rise (80/72 u) from the straight line at
    // most; at the frame ends it is on it at every rate.
    vec![
        m("eye below the body as it steps", "u", lag.at(0.0), 0.1),
        m("eye below the body at 0.05 s", "u", lag.at(0.05), 80.0 / 72.0),
        m("eye below the body at 0.1 s", "u", lag.at(0.1), 80.0 / 72.0),
        m("eye level again after", "s", level, 1.0 / 72.0),
        m("eye's fastest rise", "u/s", fastest, 0.5),
        m("going down: eye off the body, max", "u", f64::from(off), 0.01),
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
    // The rider's view: stair smoothing holds the eye up to 12 units under a
    // rising lift (V_CalcRefdef), so it rises at the lift's speed.
    let (mut eye, mut fastest) = (f64::NAN, 0.0f64);
    s.run(2.0, |s| {
        let l = s.w.server.vm.ent_get_vector(lift, "origin")[2];
        lz.push(s.t - t0, f64::from(l - lz0));
        gap = gap.max(((s.origin()[2] - pz0) - (l - lz0)).abs());
        if !s.on_ground() {
            air += 1;
        }
        let e = f64::from(s.w.oldz);
        fastest = fastest.max((e - eye) / s.dt);
        eye = e;
    });
    let rise = lz.max();
    vec![
        m("lift reaches the top", "s", lz.rises_to(rise - 0.01), 1.0 / 72.0),
        m("rider off the floor, max", "u", f64::from(gap), 0.5),
        m("rider frames airborne", "", f64::from(air), f64::NAN),
        m("rider's eye: fastest rise", "u/s", fastest, 1.0),
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
    let mut d: DemoPlay = cl_demo::build_demo_n(c.pak.clone(), 0, &mut Vec::new()).expect("demo1");
    d.stepping = c.stepping;
    let mut clock = FrameClock::new(c.rate, c.stepping);
    let (mut t, mut moves, mut last) = (0.0f64, 0usize, ([0.0f32; 3], [0.0f32; 3]));
    while t < 20.0 {
        let dt = clock.next();
        t += dt;
        let frame = cl_demo::demo_frame(&mut d, dt as f32, false, &VID);
        render::recycle_image(frame.image);
        // The POV the frame drew.
        let pov = (d.view.view_origin, d.view.view_angles);
        if pov != last {
            moves += 1;
        }
        last = pov;
    }
    vec![m("camera moves a second", "/s", moves as f64 / t, f64::NAN), m("demo message at 20 s", "", d.idx as f64, 2.0)]
}

// ---------------------------------------------------------------------------
// Monsters between their steps: Classic and r_lerpmove
// ---------------------------------------------------------------------------

/// One step mover over a run: per frame, where the server (or the newest
/// message) has it, and where the frame drew it.
#[derive(Default)]
struct StepTrack {
    server: Vec<[f32; 3]>,
    drawn: Vec<[f32; 3]>,
}

/// How smoothly a set of step movers was drawn, over the frames in which
/// each was walking (it moved within 0.1 s before the frame and within 0.1 s
/// after).
struct Motion {
    /// Of those frames, the share in which the drawn monster moved (%).
    moving: f64,
    /// The per-frame drawn move's spread: standard deviation over mean (0:
    /// the same move every frame; Classic's one step in N frames: sqrt(N-1)).
    spread: f64,
    /// The largest drawn move in one frame (units).
    largest: f64,
    /// How far behind the server's position it was drawn, on average (units).
    lag: f64,
}

/// A row of the `--lerpmove` table: its name and what it reads off a [`Motion`].
type MotionRow = (&'static str, fn(&Motion) -> f64);

fn dist(a: [f32; 3], b: [f32; 3]) -> f64 {
    f64::from((0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum::<f32>().sqrt())
}

fn motion(tracks: &[StepTrack], hz: f64) -> Motion {
    let k = (0.1 * hz).ceil() as usize + 1;
    let (mut n, mut moved, mut lag, mut largest) = (0usize, 0usize, 0.0, 0.0f64);
    let mut moves = Vec::new();
    for t in tracks {
        for f in k..t.server.len().saturating_sub(k) {
            let walking = t.server[f - k] != t.server[f] && t.server[f] != t.server[f + k];
            let step = dist(t.drawn[f], t.drawn[f - 1]);
            if !walking || step > 100.0 {
                continue;
            }
            n += 1;
            moved += usize::from(step > 1e-3);
            largest = largest.max(step);
            lag += dist(t.drawn[f], t.server[f]);
            moves.push(step);
        }
    }
    let mean = moves.iter().sum::<f64>() / moves.len().max(1) as f64;
    let var = moves.iter().map(|m| (m - mean) * (m - mean)).sum::<f64>() / moves.len().max(1) as f64;
    let n = n.max(1) as f64;
    Motion { moving: 100.0 * moved as f64 / n, spread: var.sqrt() / mean.max(1e-9), largest, lag: lag / n }
}

/// e1m1's patrolling grunt (on `t16`/`t17`), or its first-room grunt woken
/// and charging the player: `secs` of the live game at `rate` drawn with
/// `lerpmove`, each frame's server origin and drawn origin of every step
/// mover drawn (with the extra off, the two are the same). `each` sees
/// every frame's image (the strip).
fn live_steps(pak: &Pak, rate: Rate, workload: &str, secs: f64, vid: Vid, lerpmove: LerpMove, mut each: impl FnMut(&render::Image)) -> Vec<StepTrack> {
    let stepping = if rate == Rate::Hz(72) { Stepping::Classic } else { Stepping::Uncapped };
    let mut s = Sim::new(pak, "e1m1", rate, stepping);
    s.w.lerpmove = lerpmove;
    if workload == "knock" {
        // The first-room grunt, asleep, thrown up and sideways as a rocket
        // would: SV_Physics_Step moves it every frame until it lands.
        let g = s
            .find("monster_army")
            .into_iter()
            .find(|&g| s.w.server.vm.ent_get_vector(g, "origin")[..2] == [0.0, 576.0])
            .expect("e1m1's first-room grunt");
        let go = s.vm().ent_get_vector(g, "origin");
        s.place([go[0] + 192.0, go[1], go[2]], 180.0);
        let vm = s.vm();
        vm.ent_set_vector(g, "velocity", [0.0, -150.0, 300.0]);
        let flags = vm.ent_get_float(g, "flags") as i32 & !FL_ONGROUND;
        vm.ent_set_float(g, "flags", flags as f32);
    } else if workload == "charge" {
        let g = s
            .find("monster_army")
            .into_iter()
            .find(|&g| s.w.server.vm.ent_get_vector(g, "origin")[..2] == [0.0, 576.0])
            .expect("e1m1's first-room grunt");
        let go = s.vm().ent_get_vector(g, "origin");
        // Off its axis, so it runs across the view (SV_NewChaseDir goes
        // diagonally first).
        s.place([go[0] + 224.0, go[1] + 40.0, go[2]], 190.0);
        let p = s.player();
        let vm = s.vm();
        vm.ent_set_int(g, "enemy", p);
        call_qc_with(vm, "FoundTarget", g, 0, p);
    } else {
        // Watch the patrol (along y = 2048, x 1232 to 880) from the north.
        s.place([1164.0, 2236.0, -210.0], 270.0);
    }
    let mut tracks: std::collections::BTreeMap<i32, StepTrack> = std::collections::BTreeMap::new();
    let end = s.t + secs;
    let mut frames = 0usize;
    while s.t < end - 1e-9 {
        let dt = s.clock.next();
        let frame = cl_main::walk_frame(&mut s.w, dt, false, &vid);
        s.t += dt;
        each(&frame.image);
        render::recycle_image(frame.image);
        let vm = &s.w.server.vm;
        for e in 0..vm.num_edicts() as i32 {
            if vm.is_free_edict(e) || vm.ent_get_float(e, "movetype") != MOVETYPE_STEP {
                continue;
            }
            let origin = vm.ent_get_vector(e, "origin");
            let drawn = match lerpmove {
                LerpMove::Smooth => match s.w.glides.drawn(e) {
                    Some(p) => p.origin,
                    None => continue, // not drawn this frame
                },
                LerpMove::Classic => origin,
            };
            let t = tracks.entry(e).or_default();
            // A track is one unbroken run of frames.
            if t.server.len() + 1 < frames {
                *t = StepTrack::default();
            }
            t.server.resize(frames, origin);
            t.drawn.resize(frames, drawn);
            t.server.push(origin);
            t.drawn.push(drawn);
        }
        frames += 1;
    }
    tracks.into_values().filter(|t| t.server.len() == frames).collect()
}

/// demo1's first `secs` at `rate`, drawn with `lerpmove`: each frame's newest
/// message position and drawn origin of every recorded step mover.
fn demo_steps(pak: &Pak, rate: Rate, lerpmove: LerpMove, secs: f64) -> Vec<StepTrack> {
    let mut d: DemoPlay = cl_demo::build_demo_n(pak.clone(), 0, &mut Vec::new()).expect("demo1");
    d.lerpmove = lerpmove;
    let mut clock = FrameClock::new(rate, Stepping::Uncapped);
    let mut tracks: std::collections::BTreeMap<i32, StepTrack> = std::collections::BTreeMap::new();
    let (mut t, mut frames) = (0.0, 0usize);
    while t < secs {
        let dt = clock.next();
        t += dt;
        render::recycle_image(cl_demo::demo_frame(&mut d, dt as f32, false, &VID).image);
        let msg = &d.demo.frames[d.idx];
        for e in d.view.entities.iter().filter(|e| e.step && e.num >= 0) {
            let Some(m) = msg.entities.iter().find(|m| m.num == e.num) else { continue };
            let tr = tracks.entry(e.num).or_default();
            if tr.server.len() + 1 < frames {
                *tr = StepTrack::default();
            }
            tr.server.resize(frames, m.origin);
            tr.drawn.resize(frames, e.origin);
            tr.server.push(m.origin);
            tr.drawn.push(e.origin);
        }
        frames += 1;
    }
    tracks.into_values().filter(|t| t.server.len() >= frames / 4).collect()
}

/// `--lerpmove`: per workload and rate, how the monsters' steps are drawn —
/// Classic (where the server or the newest message has them; in a demo,
/// id's relink with its `U_NOLERP` jump) → with `r_lerpmove`; `--strip DIR`
/// also writes frames ([`write_strip`]).
fn lerpmove_report(pak: &Pak, rates: &[Rate], strip: Option<&str>) -> Result<String, String> {
    let mut o = String::new();
    let hz = |r: Rate| match r {
        Rate::Hz(h) => f64::from(h),
        Rate::Jitter => 144.0,
    };
    let workloads = ["patrol", "charge", "knock", "demo1"];
    for w in workloads {
        let what = match w {
            "patrol" => "e1m1's patrolling grunt walking its path (6 s)",
            "charge" => "e1m1's first-room grunt woken 228 units away: runs, fights (6 s)",
            "knock" => "the same grunt asleep, thrown up and sideways: moved every frame in the air (1.5 s)",
            _ => "demo1's first 20 s: every recorded monster (U_NOLERP)",
        };
        let _ = writeln!(o, "{w} — {what}");
        let _ = write!(o, "  {:<44}", "quantity (Classic → r_lerpmove)");
        for &r in rates {
            let _ = write!(o, " {:>17}", r.label());
        }
        let _ = writeln!(o);
        let cells: Vec<(Motion, Motion)> = rates
            .iter()
            .map(|&r| {
                if w == "demo1" {
                    (motion(&demo_steps(pak, r, LerpMove::Classic, 20.0), hz(r)), motion(&demo_steps(pak, r, LerpMove::Smooth, 20.0), hz(r)))
                } else {
                    let secs = if w == "knock" { 1.5 } else { 6.0 };
                    let tracks = live_steps(pak, r, w, secs, VID, LerpMove::Smooth, |_| {});
                    let classic: Vec<StepTrack> =
                        tracks.iter().map(|t| StepTrack { server: t.server.clone(), drawn: t.server.clone() }).collect();
                    (motion(&classic, hz(r)), motion(&tracks, hz(r)))
                }
            })
            .collect();
        let rows: [MotionRow; 4] = [
            ("frames it is drawn moving, walking (%)", |m| m.moving),
            ("spread of the per-frame move (sd/mean)", |m| m.spread),
            ("largest move in one frame (u)", |m| m.largest),
            ("drawn behind the server, mean (u)", |m| m.lag),
        ];
        for (name, get) in rows {
            let _ = write!(o, "  {name:<44}");
            for (c, sm) in &cells {
                let _ = write!(o, " {:>17}", format!("{} → {}", fmt(get(c)), fmt(get(sm))));
            }
            let _ = writeln!(o);
        }
        let _ = writeln!(o);
    }
    if let Some(dir) = strip {
        write_strip(pak, dir)?;
        let _ = writeln!(o, "wrote {dir}/charge-{{classic,lerpmove}}-00..23.ppm");
    }
    Ok(o)
}

/// `--strip DIR`: one step's worth of frames at 240 Hz — 24, 0.1 s — of the
/// charging grunt crossing the doorway, 0.4 s after it wakes, drawn Classic
/// and with `r_lerpmove`, as 640x400 PPMs `DIR/charge-<classic|lerpmove>-NN.ppm`.
fn write_strip(pak: &Pak, dir: &str) -> Result<(), String> {
    const FRAMES: usize = 24;
    let vid = Vid { width: 640, height: 400, ..VID };
    let palette = pak
        .read_file("gfx/palette.lmp")
        .ok()
        .flatten()
        .and_then(|b| render::parse_palette(&b))
        .ok_or("gfx/palette.lmp is missing or short")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (workload, from) in [("charge", 0.4)] {
        let first = (from * 240.0) as usize;
        for (mode, lerpmove) in [("classic", LerpMove::Classic), ("lerpmove", LerpMove::Smooth)] {
            let (mut n, mut written) = (0usize, Ok(()));
            live_steps(pak, Rate::Hz(240), workload, from + 0.11, vid, lerpmove, |img| {
                if (first..first + FRAMES).contains(&n) && written.is_ok() {
                    written = img.to_rgb(&palette).write_ppm(&format!("{dir}/{workload}-{mode}-{:02}.ppm", n - first));
                }
                n += 1;
            });
            written.map_err(|e| format!("cannot write the strip: {e}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Light styles: what `r_lerplightstyles` costs
// ---------------------------------------------------------------------------

/// A view `--lightstyles` and `--torchflicker` measure: the player standing
/// at `origin` on `maps/<map>.bsp`, looking along `yaw`, `pitch` degrees
/// down — and with `fire`, firing rockets all the while (god mode, the
/// rocket launcher in hand): explosions and muzzle flashes, the dynamic
/// lights that rebake what they touch.
#[derive(Clone, Debug)]
struct StyleView {
    name: String,
    map: String,
    origin: [f32; 3],
    yaw: f32,
    pitch: f32,
    fire: bool,
}

impl StyleView {
    /// `NAME=MAP:X,Y,Z:YAW[:PITCH[:fire]]` (`--view`).
    fn parse(s: &str) -> Result<StyleView, String> {
        let bad = || format!("--view: expected NAME=MAP:X,Y,Z:YAW[:PITCH[:fire]], got {s:?}");
        let (name, rest) = s.split_once('=').ok_or_else(bad)?;
        let mut parts = rest.split(':');
        let (Some(map), Some(xyz), Some(yaw), pitch, fire, None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let v: Vec<f32> = xyz.split(',').map(|p| p.trim().parse()).collect::<Result<_, _>>().map_err(|_| bad())?;
        let origin: [f32; 3] = v.try_into().map_err(|_| bad())?;
        let pitch = pitch.map_or(Ok(0.0), str::parse).map_err(|_| bad())?;
        let fire = match fire {
            None => false,
            Some("fire") => true,
            Some(_) => return Err(bad()),
        };
        Ok(StyleView { name: name.into(), map: map.into(), origin, yaw: yaw.parse().map_err(|_| bad())?, pitch, fire })
    }
}

/// The views `--lightstyles` measures without `--view`: where the shareware
/// maps animate a light — e1m1's start (the fluorescent flicker, style 10,
/// in the room ahead) and the flickering corridor itself, e1m5's slow
/// pulse (style 2) — and, with the registered `pak1.pak` layered on,
/// episode 2's flickering wall torches (styles 1 and 6): e2m2's start, and
/// the most torch-lit surfaces found in a view, by e2m5's start. The
/// shareware maps' torches are steady (style 0).
const STYLE_VIEWS: &[&str] = &[
    "e1m1-start=e1m1:480,-352,88:90",
    "e1m1-flicker=e1m1:600,140,88:270",
    "e1m5-pulse=e1m5:-544,1880,-192:270",
    "e2m2-torches=e2m2:-256,-1952,280:0",
    "e2m5-torches=e2m5:-864,-1100,-142:225",
];

/// The views `--torchflicker` measures without `--view`: on the maps with
/// the most steady torches, the views where the most drawn surfaces are
/// torch-lit (of standing 160 and 288 units from each torch, facing it) —
/// e1m2's start besides (two wall torches on the far wall), e1m3's flames,
/// e1m4's, and with the registered `pak1.pak` layered on, e2m6's (109 wall
/// torches) and e4m5's (48 large flames). A torch's light reaches 300
/// units: four in five of the surfaces drawn there are torch-lit.
const TORCH_VIEWS: &[&str] = &[
    "e1m2-start=e1m2:1496,1664,288:270",
    "e1m3-flames=e1m3:-1352,-720,-72:90",
    "e1m4-torches=e1m4:998,2246,944:90",
    "e2m6-torches=e2m6:542,1002,-488:-45",
    "e4m5-flames=e4m5:-854,-1046,-264:-135",
];

/// The views `--bake` measures without `--view`: where a frame rebakes the
/// most lit blocks — the torch-lit views (e1m2's start, e1m3's flames and,
/// with `pak1.pak`, e4m5's), the light-style glide's worst (e2m5 by the
/// start), and a rocket fight: e1m1's start, firing rockets into the room
/// ahead.
const BAKE_VIEWS: &[&str] = &[
    "e1m2-start=e1m2:1496,1664,288:270",
    "e1m3-flames=e1m3:-1352,-720,-72:90",
    "e4m5-flames=e4m5:-854,-1046,-264:-135",
    "e2m5-glide=e2m5:-864,-1100,-142:225",
    "e1m1-rockets=e1m1:480,-352,88:90:0:fire",
];

/// One run's frames: per frame, the surfaces whose lightmap carries a style
/// past 0 and those a steady torch flickers on, the blocks the surface cache
/// baked and their texels, and the 3-D view's time less its bands' (the
/// frame's serial part: the edge scan, the surface cache, the models'
/// setup), with the renderer's counters on; or the 3-D view's time in
/// seconds, with them off.
#[derive(Default)]
struct StyleRun {
    styled: Vec<f64>,
    torchlit: Vec<f64>,
    serial_s: Vec<f64>,
    baked: Vec<f64>,
    texels: Vec<f64>,
    view_s: Vec<f64>,
    /// The whole client frame, in seconds, with the counters off, and its
    /// parts beside the 3-D view: the game before it, and the 2-D layer
    /// after it.
    frame_s: Vec<f64>,
    game_s: Vec<f64>,
    layer2d_s: Vec<f64>,
    /// The renderer's counters, frame by frame, with them on.
    stats: Vec<render::RenderStats>,
}

/// `secs` of the live game at `rate` standing at `view`, drawn at `vid`,
/// after a second to settle and warm the caches.
/// `--paced`: the runs keep the display's real time, each frame started on
/// the rate's tick (a sleep between), as a game shown on that display runs:
/// the render threads idle between frames, and a frame's first round of
/// threads starts cold. Without it the frames run back to back.
static PACED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--serial`: the runs draw the page's 2026 frame — the status bar overlay
/// with the world in the corners beside it ([`render::SbarLayout::Overlay`]),
/// over the video settings the caller hands in.
static OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn style_run(pak: &Pak, view: &StyleView, rate: Rate, vid: Vid, threads: usize, secs: f64, counters: bool) -> StyleRun {
    let stepping = if rate == Rate::Hz(72) { Stepping::Classic } else { Stepping::Uncapped };
    let tick = match rate {
        Rate::Hz(hz) if PACED.load(std::sync::atomic::Ordering::Relaxed) => Some(std::time::Duration::from_secs_f64(1.0 / f64::from(hz))),
        _ => None,
    };
    let mut next = Instant::now();
    let mut s = Sim::new(pak, &view.map, rate, stepping);
    s.w.renderer.set_threads(threads);
    s.teleport(view.origin, view.yaw);
    s.w.pitch = view.pitch;
    if view.fire {
        s.arm(IT_ROCKET_LAUNCHER);
        s.w.in_attack = true;
    }
    if OVERLAY.load(std::sync::atomic::Ordering::Relaxed) {
        s.w.sbar_layout = render::SbarLayout::Overlay;
    }
    let mut run = StyleRun::default();
    let (warm, end) = (s.t + 1.0, s.t + 1.0 + secs);
    while s.t < end - 1e-9 {
        let dt = s.clock.next();
        let measuring = s.t >= warm;
        if measuring && counters {
            s.w.renderer.stats_begin();
        }
        if let Some(tick) = tick {
            next += tick;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        lap_start();
        let frame = cl_main::walk_frame(&mut s.w, dt, false, &vid);
        s.t += dt;
        render::recycle_image(frame.image);
        if !measuring {
            continue;
        }
        if counters {
            let st = s.w.renderer.stats_end();
            run.styled.push(st.surf_styled as f64);
            run.torchlit.push(st.surf_torchlit as f64);
            run.serial_s.push((lap_times()[1] - st.bands_ns as f64 * 1e-9).max(0.0));
            run.baked.push(st.surf_baked as f64);
            run.texels.push(st.surf_texels_baked as f64);
            run.stats.push(st);
        } else {
            let [game, view, post, hud] = lap_times();
            run.view_s.push(view);
            run.frame_s.push(game + view + post + hud);
            run.game_s.push(game);
            run.layer2d_s.push(post + hud);
        }
    }
    run
}

/// `--lightstyles [--rates LIST] [--res WxH] [--threads N] [--reps N]
/// [--secs S] [--view NAME=MAP:X,Y,Z:YAW]...`: per view and rate, id's
/// stepped light styles → `r_lerplightstyles`' glide: the styled surfaces
/// drawn, the blocks rebaked and their texels per frame (the renderer's
/// counters, one run each), and the 3-D view's time per frame (median, mean,
/// p95 over `reps` runs of each, interleaved, the counters off). The video
/// cvars are the 2026 profile's but for the light styles.
fn lightstyles_report(pak: &Pak, rates: &[Rate], views: &[StyleView], res: (usize, usize), threads: usize, reps: usize, secs: f64) -> String {
    use quake_rs::server::LerpLightStyles;
    let modes = [LerpLightStyles::Classic, LerpLightStyles::Smooth]
        .map(|lightstyles| render::VideoCvars { lightstyles, ..render::VideoCvars::MODERN });
    let title = format!("r_lerplightstyles at {}x{}, {threads} thread(s), {secs} s a run; Classic → Smooth", res.0, res.1);
    ab_report(pak, &title, modes, rates, views, res, threads, reps, secs)
}

/// `--torchflicker S [--rates LIST] [--res WxH] [--threads N] [--reps N]
/// [--secs S] [--view NAME=MAP:X,Y,Z:YAW[:PITCH]]...`: as `--lightstyles`,
/// the steady torches as id's → flickering at strength S (`r_torchflicker`),
/// the rest of the 2026 profile's video cvars on in both.
#[allow(clippy::too_many_arguments)]
fn torches_report(pak: &Pak, rates: &[Rate], views: &[StyleView], res: (usize, usize), threads: usize, reps: usize, secs: f64, strength: render::TorchFlicker) -> String {
    let modes = [render::TorchFlicker::OFF, strength].map(|torches| render::VideoCvars { torches, ..render::VideoCvars::MODERN });
    let title = format!("r_torchflicker at {}x{}, {threads} thread(s), {secs} s a run; 0 → {}", res.0, res.1, strength.value());
    ab_report(pak, &title, modes, rates, views, res, threads, reps, secs)
}

/// `--bake [--rates LIST] [--res WxH] [--threads LIST] [--reps N] [--secs S]
/// [--view NAME=MAP:X,Y,Z:YAW[:PITCH[:fire]]]...`: the 2026 profile's frame
/// (the torches flickering, the light styles gliding) at each view, rate and
/// thread count: the blocks rebaked and their texels per frame, the 3-D
/// view's median and p95 ms (over `reps` runs of each thread count,
/// interleaved, the counters off) and its serial part — the view's time less
/// its bands' wall time, median, from a run with the counters on — as ms and
/// as a share of the view. What the lit-surface bakes cost on 1 to 16
/// threads.
#[allow(clippy::too_many_arguments)]
fn bake_report(pak: &Pak, rates: &[Rate], views: &[StyleView], res: (usize, usize), threads: &[usize], reps: usize, secs: f64) -> String {
    let mut o = String::new();
    let vid = Vid { width: res.0, height: res.1, display_aspect: res.0 as f64 / res.1 as f64, video: render::VideoCvars::MODERN, ..VID };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
    let _ = writeln!(o, "lit-surface bakes at {}x{}, the 2026 frame, {secs} s a run, threads {threads:?}", res.0, res.1);
    quake_rs::client::set_lap_hook(Some(lap));
    for view in views {
        if pak.read_file(&format!("maps/{}.bsp", view.map)).ok().flatten().is_none() {
            let _ = writeln!(o, "{}: maps/{}.bsp is not in the pak (skipped)", view.name, view.map);
            continue;
        }
        let _ = writeln!(o, "{} — maps/{}.bsp at {:?} looking {}{}", view.name, view.map, view.origin, view.yaw, if view.fire { ", firing rockets" } else { "" });
        for &rate in rates {
            let counts: Vec<StyleRun> = threads.iter().map(|&t| style_run(pak, view, rate, vid, t, secs, true)).collect();
            let mut times: Vec<Vec<f64>> = vec![Vec::new(); threads.len()];
            for _ in 0..reps {
                for (k, &t) in threads.iter().enumerate() {
                    times[k].extend(style_run(pak, view, rate, vid, t, secs, false).view_s);
                }
            }
            let _ = writeln!(o, "  {:>6} Hz: blocks rebaked/frame {}; texels baked/frame {}", rate.label(), fmt(mean(&counts[0].baked)), fmt(mean(&counts[0].texels)));
            for (k, &t) in threads.iter().enumerate() {
                let (med, p95) = median_p95(&mut times[k]);
                let (serial, _) = median_p95(&mut counts[k].serial_s.clone());
                let _ = writeln!(o, "    {t:>2} threads: 3-D view ms/frame median {med:.3}, p95 {p95:.3}; serial {serial:.3} ms ({:.0}%)  ({} frames)",
                    100.0 * serial / med.max(1e-9), times[k].len());
            }
        }
    }
    quake_rs::client::set_lap_hook(None);
    o
}

/// `--serial [--rates LIST] [--res WxH] [--threads LIST] [--paced] [--reps N]
/// [--secs S] [--view ...]`: the page's 2026 frame — the 2026 video
/// settings with exact perspective, the status bar overlay (the world drawn
/// on in the corners beside the bar: two more views a frame) and the scaled
/// 2-D layer — at each view, rate and thread count: the whole client frame's
/// and the 3-D view's median ms (over `reps` runs of each thread count,
/// interleaved, the counters off), and from a run with the counters on the
/// mean ms a frame of each piece the calling thread does alone, beside the
/// bands' wall time. Where a native-resolution frame's serial time goes.
fn serial_report(pak: &Pak, rates: &[Rate], views: &[StyleView], res: (usize, usize), threads: &[usize], reps: usize, secs: f64) -> String {
    let mut o = String::new();
    let vid = Vid {
        width: res.0,
        height: res.1,
        display_aspect: res.0 as f64 / res.1 as f64,
        persp_span: render::PerspSpan::Exact,
        video: render::VideoCvars::MODERN,
        ..VID
    };
    quake_rs::draw::set_scaled_2d(true);
    let bar = if OVERLAY.load(std::sync::atomic::Ordering::Relaxed) { "the status bar overlay" } else { "id's status bar (--overlay 0)" };
    let _ = writeln!(o, "the page's 2026 frame at {}x{}, {bar}, {secs} s a run, threads {threads:?}", res.0, res.1);
    quake_rs::client::set_lap_hook(Some(lap));
    for view in views {
        if pak.read_file(&format!("maps/{}.bsp", view.map)).ok().flatten().is_none() {
            let _ = writeln!(o, "{}: maps/{}.bsp is not in the pak (skipped)", view.name, view.map);
            continue;
        }
        let _ = writeln!(o, "{} — maps/{}.bsp at {:?} looking {}{}", view.name, view.map, view.origin, view.yaw, if view.fire { ", firing rockets" } else { "" });
        for &rate in rates {
            let counts: Vec<StyleRun> = threads.iter().map(|&t| style_run(pak, view, rate, vid, t, secs, true)).collect();
            let mut times: Vec<[Vec<f64>; 4]> = vec![Default::default(); threads.len()];
            for _ in 0..reps {
                for (k, &t) in threads.iter().enumerate() {
                    let run = style_run(pak, view, rate, vid, t, secs, false);
                    for (all, new) in times[k].iter_mut().zip([run.frame_s, run.view_s, run.game_s, run.layer2d_s]) {
                        all.extend(new);
                    }
                }
            }
            let _ = writeln!(o, "  {:>6} Hz", rate.label());
            for (k, &t) in threads.iter().enumerate() {
                let [(frame, frame95), (view3d, _), (game, _), (layer2d, _)] = [0, 1, 2, 3].map(|i| median_p95(&mut times[k][i]));
                let st = &counts[k].stats;
                let ms = |f: &dyn Fn(&render::RenderStats) -> u64| st.iter().map(|s| f(s) as f64).sum::<f64>() / 1e6 / st.len().max(1) as f64;
                let _ = writeln!(
                    o,
                    "    {t:>2} threads: frame {frame:.3} ms (p95 {frame95:.3}) = game {game:.3} + 3-D {view3d:.3} + 2-D {layer2d:.3}; counted: {:.1} views, {:.3} ms, alone {:.3} = setup {:.3} + walk {:.3} + brush {:.3} + scan {:.3} + lookups {:.3} + entities {:.3}; bands {:.3} (their bakes {:.3}, every thread's)",
                    ms(&|s| s.views) * 1e6,
                    ms(&|s| s.view_ns),
                    ms(&|s| s.view_ns.saturating_sub(s.bands_ns)),
                    ms(&|s| s.view_setup_ns),
                    ms(&|s| s.world_sort_ns),
                    ms(&|s| s.submodel_ns),
                    ms(&|s| s.world_setup_ns),
                    ms(&|s| s.surf_lookup_ns),
                    ms(&|s| s.entity_setup_ns),
                    ms(&|s| s.bands_ns),
                    ms(&|s| s.surf_bake_ns),
                );
            }
        }
    }
    quake_rs::client::set_lap_hook(None);
    o
}

/// Per view and rate, the two video settings `modes`, A → B: the styled and
/// torch-lit surfaces drawn, the blocks rebaked and their texels per frame
/// (the renderer's counters, one run each), and the 3-D view's time per
/// frame (median, mean, p95 over `reps` runs of each, interleaved, the
/// counters off).
#[allow(clippy::too_many_arguments)]
fn ab_report(pak: &Pak, title: &str, modes: [render::VideoCvars; 2], rates: &[Rate], views: &[StyleView], res: (usize, usize), threads: usize, reps: usize, secs: f64) -> String {
    let mut o = String::new();
    let vid_for = |video| Vid { width: res.0, height: res.1, display_aspect: res.0 as f64 / res.1 as f64, video, ..VID };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len().max(1) as f64;
    let _ = writeln!(o, "{title}");
    quake_rs::client::set_lap_hook(Some(lap));
    for view in views {
        if pak.read_file(&format!("maps/{}.bsp", view.map)).ok().flatten().is_none() {
            let _ = writeln!(o, "{}: maps/{}.bsp is not in the pak (skipped)", view.name, view.map);
            continue;
        }
        let _ = writeln!(o, "{} — maps/{}.bsp at {:?} looking {}", view.name, view.map, view.origin, view.yaw);
        for &rate in rates {
            let counts = modes.map(|m| style_run(pak, view, rate, vid_for(m), threads, secs, true));
            let mut times: [Vec<f64>; 2] = Default::default();
            for _ in 0..reps {
                for (k, &m) in modes.iter().enumerate() {
                    times[k].extend(style_run(pak, view, rate, vid_for(m), threads, secs, false).view_s);
                }
            }
            let cell = |f: &dyn Fn(&StyleRun) -> f64| format!("{} → {}", fmt(f(&counts[0])), fmt(f(&counts[1])));
            let _ = writeln!(o, "  {:>6} Hz: styled surfaces/frame {}; torch-lit {}; blocks rebaked/frame {}; texels baked/frame {}",
                rate.label(), cell(&|r| mean(&r.styled)), cell(&|r| mean(&r.torchlit)), cell(&|r| mean(&r.baked)), cell(&|r| mean(&r.texels)));
            // (median, mean, p95) in ms.
            let stat = |xs: &mut Vec<f64>| {
                let m = mean(xs) * 1000.0;
                let (med, p95) = median_p95(xs);
                (med, m, p95)
            };
            let (a, b) = (stat(&mut times[0]), stat(&mut times[1]));
            let _ = writeln!(
                o,
                "          3-D view ms/frame median {:.3} → {:.3}, mean {:.3} → {:.3} ({:+.1}%), p95 {:.3} → {:.3}  ({} frames each)",
                a.0, b.0, a.1, b.1, (b.1 / a.1 - 1.0) * 100.0, a.2, b.2, times[0].len()
            );
        }
    }
    quake_rs::client::set_lap_hook(None);
    o
}

/// `--torchflicker S --dump DIR [--strengths LIST]`: `secs` of each view at
/// `rate` (after a second to settle), the whole screen at viewsize 120 (the
/// view and the gun, no status bar), once for each strength — `0` is id's —
/// as `DIR/<view>-<strength>.rgb`, the frames' RGB one after another, and
/// `DIR/<view>.txt` saying `W H RATE FRAMES`: what the strips, the stills
/// and the side-by-side clip are cut from.
fn torches_dump(pak: &Pak, views: &[StyleView], rate: Rate, res: (usize, usize), secs: f64, strengths: &[f32], dir: &str) -> Result<String, String> {
    use std::io::Write as _;
    let palette = pak
        .read_file("gfx/palette.lmp")
        .ok()
        .flatten()
        .and_then(|b| render::parse_palette(&b))
        .ok_or("gfx/palette.lmp is missing or short")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stepping = if rate == Rate::Hz(72) { Stepping::Classic } else { Stepping::Uncapped };
    let mut o = String::new();
    for view in views {
        let mut frames = 0;
        for &strength in strengths {
            let torches = render::TorchFlicker::from_value(strength);
            let video = render::VideoCvars { torches, ..render::VideoCvars::MODERN };
            let vid = Vid { width: res.0, height: res.1, display_aspect: res.0 as f64 / res.1 as f64, video, ..VID };
            let mut s = Sim::new(pak, &view.map, rate, stepping);
            s.teleport(view.origin, view.yaw);
            s.w.pitch = view.pitch;
            s.w.viewsize = 120.0;
            let path = format!("{dir}/{}-{}.rgb", view.name, torches.value());
            let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?);
            let (warm, end) = (s.t + 1.0, s.t + 1.0 + secs);
            frames = 0;
            while s.t < end - 1e-9 {
                let dt = s.clock.next();
                let frame = cl_main::walk_frame(&mut s.w, dt, false, &vid);
                s.t += dt;
                if s.t > warm {
                    let rgb = frame.image.to_rgb(&palette);
                    let bytes: Vec<u8> = rgb.pixels.iter().flatten().copied().collect();
                    out.write_all(&bytes).map_err(|e| format!("{path}: {e}"))?;
                    frames += 1;
                }
                render::recycle_image(frame.image);
            }
            let _ = writeln!(o, "{path}: {frames} frames");
        }
        let meta = format!("{dir}/{}.txt", view.name);
        std::fs::write(&meta, format!("{} {} {} {frames}\n", res.0, res.1, rate.label())).map_err(|e| format!("{meta}: {e}"))?;
    }
    Ok(o)
}

// ---------------------------------------------------------------------------
// The perspective span: what `r_perspspan` costs
// ---------------------------------------------------------------------------

/// The views `--perspspan` measures without `--view`: e1m1's start (a lit
/// corridor, the first thing a player sees), e1m1's flickering corridor, a
/// wall-heavy view in e1m6 (the walls of the Door to Chthon's courtyard, where
/// the two perspectives differ most of the legal views searched: 3% of the
/// frame at 1080p), and a liquid-heavy one, e1m4's lake from a ledge above it,
/// looking down (the liquids take the exact path too: `Turbulent8`'s
/// segments). The player's origin, not the eye's, as `--view` gives it.
const PERSP_VIEWS: &[&str] = &[
    "e1m1-start=e1m1:480,-352,88:90",
    "e1m1-corridor=e1m1:600,140,88:270",
    "e1m6-walls=e1m6:504,500,220:100",
    "e1m4-lake=e1m4:320,1284,928:0:35",
];

/// `secs` of the live game at `rate` with the camera at `view`, drawn at
/// `vid`, after a second to settle: the 3-D view's time per frame in seconds.
/// The player floats (noclip) so that a view over a lake stays where it is
/// put; [`style_run`] is otherwise the same loop.
fn persp_run(pak: &Pak, view: &StyleView, rate: Rate, vid: Vid, threads: usize, secs: f64) -> Vec<f64> {
    let stepping = if rate == Rate::Hz(72) { Stepping::Classic } else { Stepping::Uncapped };
    let mut s = Sim::new(pak, &view.map, rate, stepping);
    s.w.renderer.set_threads(threads);
    s.teleport(view.origin, view.yaw);
    let player = s.player();
    s.vm().ent_set_float(player, "movetype", MOVETYPE_NOCLIP);
    s.w.pitch = view.pitch;
    let mut view_s = Vec::new();
    let (warm, end) = (s.t + 1.0, s.t + 1.0 + secs);
    while s.t < end - 1e-9 {
        let dt = s.clock.next();
        lap_start();
        let frame = cl_main::walk_frame(&mut s.w, dt, false, &vid);
        render::recycle_image(frame.image);
        if s.t >= warm {
            view_s.push(lap_times()[1]);
        }
        s.t += dt;
    }
    view_s
}

/// `--perspspan`: per view and rate, the 3-D view's time per frame (median,
/// mean, p95 over `reps` runs of each, interleaved) at each of `spans`, the
/// first the reference the others are put against (id's 16 by default),
/// every other video setting the 2026 profile's (the torches flicker, so the
/// numbers are today's).
#[allow(clippy::too_many_arguments)]
fn persp_report(pak: &Pak, rates: &[Rate], views: &[StyleView], res: (usize, usize), spans: &[render::PerspSpan], threads: usize, reps: usize, secs: f64) -> String {
    let mut o = String::new();
    let vid_for = |persp_span| Vid { width: res.0, height: res.1, display_aspect: res.0 as f64 / res.1 as f64, persp_span, video: render::VideoCvars::MODERN, ..VID };
    let names: Vec<String> = spans.iter().map(|p| p.pixels().to_string()).collect();
    let _ = writeln!(o, "r_perspspan at {}x{}, {threads} thread(s), {secs} s a run; spans {}", res.0, res.1, names.join(" → "));
    quake_rs::client::set_lap_hook(Some(lap));
    for view in views {
        if pak.read_file(&format!("maps/{}.bsp", view.map)).ok().flatten().is_none() {
            let _ = writeln!(o, "{}: maps/{}.bsp is not in the pak (skipped)", view.name, view.map);
            continue;
        }
        let _ = writeln!(o, "{} — maps/{}.bsp at {:?} looking {} pitch {}", view.name, view.map, view.origin, view.yaw, view.pitch);
        for &rate in rates {
            let mut times: Vec<Vec<f64>> = vec![Vec::new(); spans.len()];
            for _ in 0..reps {
                for (k, &span) in spans.iter().enumerate() {
                    times[k].extend(persp_run(pak, view, rate, vid_for(span), threads, secs));
                }
            }
            // (median, mean, p95) in ms.
            let stat = |xs: &mut Vec<f64>| {
                let m = xs.iter().sum::<f64>() / xs.len().max(1) as f64 * 1000.0;
                let (med, p95) = median_p95(xs);
                (med, m, p95)
            };
            let st: Vec<(f64, f64, f64)> = times.iter_mut().map(stat).collect();
            let pct = |b: f64, a: f64| (b / a - 1.0) * 100.0;
            let cells = |f: &dyn Fn(&(f64, f64, f64)) -> f64| {
                st.iter().zip(&names).enumerate().map(|(k, (x, n))| {
                    if k == 0 { format!("{n}: {:.3}", f(x)) } else { format!("{n}: {:.3} ({:+.1}%)", f(x), pct(f(x), f(&st[0]))) }
                }).collect::<Vec<_>>().join(", ")
            };
            let _ = writeln!(o, "  {:>6} Hz: 3-D view ms/frame median {}  ({} frames each)", rate.label(), cells(&|x| x.0), times[0].len());
            let _ = writeln!(o, "  {:>6}     mean {}", "", cells(&|x| x.1));
            let _ = writeln!(o, "  {:>6}     p95 {}", "", cells(&|x| x.2));
        }
    }
    quake_rs::client::set_lap_hook(None);
    o
}

/// How `--perspspan --dump` moves the camera: `turn` degrees a second to the
/// left (the yaw grows), and `strafe` units a second to the right (noclip:
/// the player floats at that speed, the view bobbing as a player's does).
#[derive(Clone, Copy, Debug, Default)]
struct PerspMotion {
    turn: f32,
    strafe: f32,
}

/// `--perspspan --dump DIR`: `secs` of each view at each span, the camera
/// moving as `motion` says, every frame (after a second to settle, the
/// camera already moving) as raw RGB in `DIR/<view>-<span>.rgb` — the
/// `crop` rectangle of each, or the whole frame — and `DIR/<view>.txt`, the
/// size, rate and frame count. Every span's run is the same game frame for
/// frame (a fresh session's random streams, the same clock), so the four
/// files are one clip at four settings.
#[allow(clippy::too_many_arguments)]
fn persp_dump(
    pak: &Pak,
    views: &[StyleView],
    rate: Rate,
    res: (usize, usize),
    secs: f64,
    spans: &[render::PerspSpan],
    motion: PerspMotion,
    crop: Option<(usize, usize, usize, usize)>,
    dir: &str,
) -> Result<String, String> {
    use std::io::Write as _;
    let palette = pak
        .read_file("gfx/palette.lmp")
        .ok()
        .flatten()
        .and_then(|b| render::parse_palette(&b))
        .ok_or("gfx/palette.lmp is missing or short")?;
    let (cx, cy, cw, ch) = crop.unwrap_or((0, 0, res.0, res.1));
    if cx + cw > res.0 || cy + ch > res.1 || cw == 0 || ch == 0 {
        return Err(format!("--crop {cx},{cy},{cw},{ch} is not inside {}x{}", res.0, res.1));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stepping = if rate == Rate::Hz(72) { Stepping::Classic } else { Stepping::Uncapped };
    let mut o = String::new();
    for view in views {
        let mut frames = 0;
        for &span in spans {
            let vid = Vid { width: res.0, height: res.1, display_aspect: res.0 as f64 / res.1 as f64, persp_span: span, video: render::VideoCvars::MODERN, ..VID };
            let mut s = Sim::new(pak, &view.map, rate, stepping);
            s.teleport(view.origin, view.yaw);
            let player = s.player();
            s.vm().ent_set_float(player, "movetype", MOVETYPE_NOCLIP);
            s.w.pitch = view.pitch;
            s.w.viewsize = 120.0;
            s.w.in_side = motion.strafe / 320.0;
            let path = format!("{dir}/{}-{}.rgb", view.name, span.pixels());
            let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?);
            let (start, warm, end) = (s.t, s.t + 1.0, s.t + 1.0 + secs);
            frames = 0;
            while s.t < end - 1e-9 {
                let dt = s.clock.next();
                s.w.yaw = view.yaw + motion.turn * (s.t - start) as f32;
                let frame = cl_main::walk_frame(&mut s.w, dt, false, &vid);
                s.t += dt;
                if s.t > warm {
                    let rgb = frame.image.to_rgb(&palette);
                    let mut bytes = Vec::with_capacity(cw * ch * 3);
                    for row in rgb.pixels.chunks(res.0).skip(cy).take(ch) {
                        bytes.extend(row[cx..cx + cw].iter().flatten());
                    }
                    out.write_all(&bytes).map_err(|e| format!("{path}: {e}"))?;
                    frames += 1;
                }
                render::recycle_image(frame.image);
            }
            let _ = writeln!(o, "{path}: {frames} frames");
        }
        let meta = format!("{dir}/{}.txt", view.name);
        std::fs::write(&meta, format!("{cw} {ch} {} {frames}\n", rate.label())).map_err(|e| format!("{meta}: {e}"))?;
    }
    Ok(o)
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
    let (mut lerpmove, mut strip) = (false, None::<String>);
    // `--lightstyles`' own: 72 Hz is a rate like any other there.
    let (mut lightstyles, mut views, mut style_rates) = (false, Vec::new(), vec![Rate::Hz(72), Rate::Hz(480)]);
    // `--bake`'s: the thread counts.
    let (mut bake, mut serial, mut thread_list) = (false, false, vec![1usize, 2, 4, 8, 16]);
    // `--torchflicker`'s: the strength, and `--dump`'s directory and strengths.
    let (mut torchflicker, mut dump, mut strengths) = (None::<f32>, None::<String>, vec![0.0, 0.5, 1.0]);
    // `--perspspan`'s: the spans compared, and `--dump`'s camera motion and
    // crop (`--exactpersp` is `--perspspan --spans 16,1`).
    let (mut perspspan, mut spans) = (false, render::PerspSpan::ALL.to_vec());
    let (mut motion, mut crop) = (PerspMotion::default(), None);
    let (mut threads, mut reps, mut secs) = (1usize, 3usize, 4.6f64);
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--rates" => {
                let v = rest.get(i + 1).ok_or("--rates needs a list")?;
                rates = v.split(',').map(Rate::parse).collect::<Result<_, _>>()?;
                style_rates = rates.clone();
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
            "--lerpmove" => lerpmove = true,
            "--strip" => {
                strip = Some(rest.get(i + 1).ok_or("--strip needs a directory")?.clone());
                i += 1;
            }
            "--res" => {
                res = rest.get(i + 1).ok_or("--res needs WxH[,WxH...]")?.clone();
                i += 1;
            }
            "--lightstyles" => lightstyles = true,
            "--perspspan" => perspspan = true,
            "--exactpersp" => (perspspan, spans) = (true, vec![render::PerspSpan::Spans16, render::PerspSpan::Exact]),
            "--spans" => {
                let v = rest.get(i + 1).ok_or("--spans needs a list")?;
                spans = v.split(',').map(crate::video::parse_span).collect::<Result<_, _>>()?;
                i += 1;
            }
            "--turn" | "--strafe" => {
                let v = rest.get(i + 1).ok_or_else(|| format!("{} needs a speed", rest[i]))?;
                let speed: f32 = v.parse().map_err(|_| format!("{}: bad speed {v:?}", rest[i]))?;
                if rest[i] == "--turn" { motion.turn = speed } else { motion.strafe = speed }
                i += 1;
            }
            "--crop" => {
                let v = rest.get(i + 1).ok_or("--crop needs X,Y,W,H")?;
                let n: Vec<usize> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().map_err(|_| format!("--crop: bad X,Y,W,H {v:?}"))?;
                let [x, y, w, h] = n[..] else { return Err(format!("--crop: expected X,Y,W,H, got {v:?}")) };
                crop = Some((x, y, w, h));
                i += 1;
            }
            "--bake" => bake = true,
            "--serial" => {
                serial = true;
                OVERLAY.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            "--overlay" => {
                let on = rest.get(i + 1).ok_or("--overlay needs 0 or 1")? != "0";
                OVERLAY.store(on, std::sync::atomic::Ordering::Relaxed);
                i += 1;
            }
            "--paced" => PACED.store(true, std::sync::atomic::Ordering::Relaxed),
            "--torchflicker" => {
                let v = rest.get(i + 1).ok_or("--torchflicker needs a strength")?;
                torchflicker = Some(v.parse().map_err(|_| format!("--torchflicker: bad strength {v:?}"))?);
                i += 1;
            }
            "--dump" => {
                dump = Some(rest.get(i + 1).ok_or("--dump needs a directory")?.clone());
                i += 1;
            }
            "--strengths" => {
                let v = rest.get(i + 1).ok_or("--strengths needs a list")?;
                strengths = v.split(',').map(|x| x.parse().map_err(|_| format!("--strengths: bad strength {x:?}"))).collect::<Result<_, _>>()?;
                i += 1;
            }
            "--view" => {
                views.push(StyleView::parse(rest.get(i + 1).ok_or("--view needs NAME=MAP:X,Y,Z:YAW")?)?);
                i += 1;
            }
            "--threads" | "--reps" | "--secs" => {
                let v = rest.get(i + 1).ok_or_else(|| format!("{} needs a number", rest[i]))?;
                let bad = || format!("{}: bad number {v:?}", rest[i]);
                match rest[i].as_str() {
                    "--threads" => {
                        thread_list = v.split(',').map(|t| t.parse().ok().filter(|&n| n > 0).ok_or_else(bad)).collect::<Result<_, _>>()?;
                        threads = thread_list[0];
                    }
                    "--reps" => reps = v.parse().ok().filter(|&n| n > 0).ok_or_else(bad)?,
                    _ => secs = v.parse().ok().filter(|&s: &f64| s > 0.0).ok_or_else(bad)?,
                }
                i += 1;
            }
            a => return Err(format!("unknown argument {a:?}")),
        }
        i += 1;
    }
    // A comma-separated list layers like `view`'s (the last searched first):
    // the registered maps need `pak1.pak` over `pak0.pak`.
    let mut pak: Option<Pak> = None;
    for path in pak_path.split(',') {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let name = std::path::Path::new(path).file_name().map_or("pak0.pak".into(), |n| n.to_string_lossy().to_lowercase());
        let over = Pak::from_bytes(name, bytes).map_err(|e| e.to_string())?;
        pak = Some(match pak {
            Some(under) => over.over(under),
            None => over,
        });
    }
    let pak = pak.ok_or("no pak")?;
    if perspspan {
        style_rates.retain(|r| matches!(r, Rate::Hz(_)));
        if views.is_empty() {
            views = PERSP_VIEWS.iter().map(|v| StyleView::parse(v)).collect::<Result<_, _>>()?;
        }
        if spans.is_empty() {
            return Err("--spans: no span".into());
        }
        let size = super::parse_res(res.split(',').next().unwrap_or("1920x1080"), render::VideoCvars::MODERN)?;
        if let Some(dir) = dump {
            let rate = style_rates.first().copied().unwrap_or(Rate::Hz(60));
            return persp_dump(&pak, &views, rate, size, secs, &spans, motion, crop, &dir);
        }
        return Ok(persp_report(&pak, &style_rates, &views, size, &spans, threads, reps, secs));
    }
    if serial {
        style_rates.retain(|r| matches!(r, Rate::Hz(_)));
        if views.is_empty() {
            views = BAKE_VIEWS.iter().map(|v| StyleView::parse(v)).collect::<Result<_, _>>()?;
        }
        let size = super::parse_res(res.split(',').next().unwrap_or("1920x1080"), render::VideoCvars::MODERN)?;
        return Ok(serial_report(&pak, &style_rates, &views, size, &thread_list, reps, secs));
    }
    if bake {
        style_rates.retain(|r| matches!(r, Rate::Hz(_)));
        if views.is_empty() {
            views = BAKE_VIEWS.iter().map(|v| StyleView::parse(v)).collect::<Result<_, _>>()?;
        }
        let size = super::parse_res(res.split(',').next().unwrap_or("1920x1080"), render::VideoCvars::MODERN)?;
        return Ok(bake_report(&pak, &style_rates, &views, size, &thread_list, reps, secs));
    }
    if let Some(strength) = torchflicker {
        style_rates.retain(|r| matches!(r, Rate::Hz(_)));
        if views.is_empty() {
            views = TORCH_VIEWS.iter().map(|v| StyleView::parse(v)).collect::<Result<_, _>>()?;
        }
        views.retain(|v| pak.read_file(&format!("maps/{}.bsp", v.map)).ok().flatten().is_some());
        let size = super::parse_res(res.split(',').next().unwrap_or("1920x1080"), render::VideoCvars::MODERN)?;
        if let Some(dir) = dump {
            let rate = style_rates.first().copied().unwrap_or(Rate::Hz(240));
            return torches_dump(&pak, &views, rate, size, secs, &strengths, &dir);
        }
        return Ok(torches_report(&pak, &style_rates, &views, size, threads, reps, secs, render::TorchFlicker::from_value(strength)));
    }
    if lightstyles {
        style_rates.retain(|r| matches!(r, Rate::Hz(_)));
        if views.is_empty() {
            views = STYLE_VIEWS.iter().map(|v| StyleView::parse(v)).collect::<Result<_, _>>()?;
        }
        let size = super::parse_res(res.split(',').next().unwrap_or("1920x1080"), render::VideoCvars::MODERN)?;
        return Ok(lightstyles_report(&pak, &style_rates, &views, size, threads, reps, secs));
    }
    if budget {
        let sizes = res.split(',').map(|r| super::parse_res(r, render::VideoCvars::CLASSIC)).collect::<Result<Vec<_>, _>>()?;
        return Ok(frame_budget(&pak, &sizes));
    }
    if lerpmove {
        rates.retain(|r| matches!(r, Rate::Hz(_)));
        rates.insert(0, Rate::Hz(72));
        return lerpmove_report(&pak, &rates, strip.as_deref());
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

