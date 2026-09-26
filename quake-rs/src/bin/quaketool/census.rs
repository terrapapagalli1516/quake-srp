//! `quaketool census <pak> [map ...]` — a headless faithfulness playthrough.
//!
//! For each map (default: `start` + e1m1..e1m8) it loads the real progs.dat the
//! way the browser build does (spawn, connect, the two signon frames), then
//! drives a fixed script through the *real* QuakeC so every code path a player
//! would exercise runs at least once:
//!
//! 1. idle 3 s (trains start, lights animate, monsters idle);
//! 2. god mode + `impulse 9` (all weapons/ammo, like the console cheat);
//! 3. a duel with one monster of each kind: the player on the floor 96 units
//!    in front of it, in its line of sight, for 8 s — every shareware attack
//!    runs (the duellist is killed afterwards so it does not follow);
//! 4. wake every other monster (`enemy = player; FoundTarget()`), 20 s;
//! 5. kill every monster through QuakeC `T_Damage` — odd ones exactly
//!    (normal death), even ones by 1000 (gibs);
//! 6. touch every item/weapon/key/powerup (the `touch` SV_TouchLinks would
//!    call; the keys `impulse 9` gave are taken back first);
//! 7. e1m7 only: Chthon — raise both electrodes, press the lightning button,
//!    three times;
//! 8. three passes over every trigger / button / door: call its `touch` with
//!    the player as `other`, damage every shootable, then let the world run
//!    3 s — enough for multi-step sequences (counters, relays, secret doors);
//! 9. finally the exit: touch `trigger_changelevel`, hold fire after 6 s so
//!    `ExitIntermission` runs (twice at an episode end, for the finale).
//!
//! It records what a faithfulness census needs: spawn-function coverage,
//! QuakeC faults (think/spawn errors, `error`/`objerror`/`dprint` text), every
//! builtin actually executed (with the arguments of the interesting ones:
//! `stuffcmd`, `localcmd`, `cvar_set`, `lightstyle`, `makestatic`,
//! `changelevel`, precaches, `setmodel`), every sound played (and whether the
//! sample exists in the pak / was precached), every print, every MSG_ALL
//! command and temp entity, and which MOVETYPE_PUSH brush models never moved.
//!
//! The wrappers are installed over `vm.builtins` (the table the engine
//! dispatches through) — no engine code is changed. The report is text; it is
//! the evidence behind `CENSUS.md`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::rc::Rc;

use quake_rs::bsp::Bsp;
use quake_rs::pak::Pak;
use quake_rs::progs::{Progs, OFS_PARM0};
use quake_rs::qrand::QRand;
use quake_rs::server::{Server, SvcEvent, UserCmd};
use quake_rs::vm::{Builtin, Vm};

const FL_GODMODE: i32 = 64;
const FL_MONSTER: i32 = 32;
const SOLID_TRIGGER: i32 = 1;
const SOLID_BSP: i32 = 4;
const MOVETYPE_PUSH: i32 = 7;
/// id's frame at the oracle's pace: `host_frametime` exactly 0.1 (a double).
const DT: f64 = 0.1;

/// The builtin names by number (pr_cmds.c `pr_builtin[]`), for the report.
fn builtin_name(n: usize) -> &'static str {
    const NAMES: [&str; 80] = [
        "#0", "makevectors", "setorigin", "setmodel", "setsize", "#5", "break", "random", "sound",
        "normalize", "error", "objerror", "vlen", "vectoyaw", "spawn", "remove", "traceline",
        "checkclient", "find", "precache_sound", "precache_model", "stuffcmd", "findradius",
        "bprint", "sprint", "dprint", "ftos", "vtos", "coredump", "traceon", "traceoff", "eprint",
        "walkmove", "#33", "droptofloor", "lightstyle", "rint", "floor", "ceil", "#39",
        "checkbottom", "pointcontents", "#42", "fabs", "aim", "cvar", "localcmd", "nextent",
        "particle", "ChangeYaw", "#50", "vectoangles", "WriteByte", "WriteChar", "WriteShort",
        "WriteLong", "WriteCoord", "WriteAngle", "WriteString", "WriteEntity", "#60", "#61",
        "#62", "#63", "#64", "#65", "#66", "movetogoal", "precache_file", "makestatic",
        "changelevel", "#71", "cvar_set", "centerprint", "ambientsound", "precache_model2",
        "precache_sound2", "precache_file2", "setspawnparms", "#79",
    ];
    NAMES.get(n).copied().unwrap_or("#?")
}

thread_local! {
    static ORIG: RefCell<Vec<Builtin>> = const { RefCell::new(Vec::new()) };
    static CALLS: RefCell<BTreeMap<usize, u64>> = const { RefCell::new(BTreeMap::new()) };
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static MODELS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    static PRECACHE: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

fn log(s: String) {
    LOG.with(|l| l.borrow_mut().push(s));
}

/// One wrapper per builtin number: count it, record the arguments of the
/// interesting ones, then run the engine's own builtin.
fn wrap<const N: usize>(vm: &mut Vm) -> quake_rs::Result<()> {
    CALLS.with(|c| *c.borrow_mut().entry(N).or_insert(0) += 1);
    match N {
        21 => log(format!("stuffcmd({}, {:?})", vm.arg_entity(0), vm.arg_string(1))),
        46 => log(format!("localcmd({:?})", vm.arg_string(0))),
        72 => log(format!("cvar_set({:?}, {:?})", vm.arg_string(0), vm.arg_string(1))),
        35 => log(format!("lightstyle({}, {:?})", vm.arg_float(0), vm.arg_string(1))),
        70 => log(format!("changelevel({:?})", vm.arg_string(0))),
        78 => log(format!("setspawnparms({})", vm.arg_entity(0))),
        69 => {
            let e = vm.arg_entity(0);
            log(format!(
                "makestatic({} {:?} model={:?})",
                e,
                vm.ent_get_string(e, "classname"),
                vm.ent_get_string(e, "model")
            ))
        }
        3 => {
            let m = vm.arg_string(1);
            MODELS.with(|s| s.borrow_mut().insert(m));
        }
        19 | 20 | 75 | 76 => {
            let m = vm.arg_string(0);
            PRECACHE.with(|s| s.borrow_mut().insert(m));
        }
        _ => {}
    }
    let f = ORIG.with(|o| o.borrow()[N]);
    f(vm)
}

macro_rules! wrappers {
    ($($n:literal)*) => { [$(wrap::<$n> as Builtin),*] };
}

fn install_wrappers(vm: &mut Vm) {
    let table: [Builtin; 80] = wrappers!(
        0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29
        30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59
        60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79
    );
    ORIG.with(|o| *o.borrow_mut() = vm.builtins.clone());
    for (i, slot) in vm.builtins.iter_mut().enumerate() {
        if i < table.len() {
            *slot = table[i];
        }
    }
}

fn reset_logs() {
    CALLS.with(|c| c.borrow_mut().clear());
    LOG.with(|l| l.borrow_mut().clear());
    MODELS.with(|s| s.borrow_mut().clear());
    PRECACHE.with(|s| s.borrow_mut().clear());
}

/// Call a QuakeC (or builtin) function by name with entity/float args in the
/// parm slots, `self`/`other` set, and `time` = sv.time (what SV_Impact /
/// SV_TouchLinks / the spawn loop set before calling into QC).
fn call_qc(server: &mut Server, name: &str, self_e: i32, other: i32, args: &[Arg]) -> Result<(), String> {
    let f = server.vm.progs.find_function(name).ok_or_else(|| format!("no function {name}"))?;
    call_fnum(server, f, self_e, other, args)
}

#[derive(Clone, Copy)]
enum Arg {
    Ent(i32),
    F(f32),
}

fn call_fnum(server: &mut Server, f: usize, self_e: i32, other: i32, args: &[Arg]) -> Result<(), String> {
    let t = server.time();
    let vm = &mut server.vm;
    vm.gset_int("self", self_e);
    vm.gset_int("other", other);
    vm.gset_float("time", t);
    for (i, a) in args.iter().enumerate() {
        let ofs = OFS_PARM0 + 3 * i;
        match *a {
            Arg::Ent(e) => vm.set_gi(ofs, e),
            Arg::F(x) => vm.set_gf(ofs, x),
        }
    }
    vm.argc = args.len();
    let r = vm.execute(f).map_err(|e| e.to_string());
    if r.is_err() {
        vm.reset_execution();
    }
    r
}

fn live(server: &Server, e: usize) -> bool {
    !server.vm.edict_free.get(e).copied().unwrap_or(true)
}

fn center(server: &Server, e: i32) -> [f32; 3] {
    let vm = &server.vm;
    let (a, b) = (vm.ent_get_vector(e, "absmin"), vm.ent_get_vector(e, "absmax"));
    if a == [0.0; 3] && b == [0.0; 3] {
        let o = vm.ent_get_vector(e, "origin");
        let (mi, ma) = (vm.ent_get_vector(e, "mins"), vm.ent_get_vector(e, "maxs"));
        return [o[0] + (mi[0] + ma[0]) * 0.5, o[1] + (mi[1] + ma[1]) * 0.5, o[2] + (mi[2] + ma[2]) * 0.5];
    }
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5]
}

/// Everything one map's run collected.
#[derive(Default)]
struct Run {
    frames: usize,
    think_errors: Vec<String>,
    sounds: BTreeMap<String, (usize, bool, bool)>, // sample -> (plays, in pak, precached)
    prints: BTreeMap<String, usize>,
    centers: BTreeMap<String, usize>,
    svc: Vec<String>,
    te: BTreeMap<u8, usize>,
    particles: usize,
    output: String,
    // pusher entity -> (classname, targetname, max displacement)
    pushers: BTreeMap<i32, (String, String, [f32; 3], f32)>,
    teleports: Vec<String>,
    fixangle_seen: usize,
}

fn track_pushers(server: &Server, run: &mut Run) {
    for (&e, rec) in run.pushers.iter_mut() {
        if !live(server, e as usize) {
            continue;
        }
        let o = server.vm.ent_get_vector(e, "origin");
        let a = server.vm.ent_get_vector(e, "angles");
        let d = ((o[0] - rec.2[0]).powi(2) + (o[1] - rec.2[1]).powi(2) + (o[2] - rec.2[2]).powi(2)).sqrt()
            + a[0].abs() + a[1].abs() + a[2].abs() * 0.0;
        if d > rec.3 {
            rec.3 = d;
        }
    }
}

fn frame(server: &mut Server, pak: &Pak, run: &mut Run, cmd: &UserCmd) {
    let player = server.player_edict();
    let before = server.vm.ent_get_vector(player, "origin");
    let fix = server.vm.ent_get_float(player, "fixangle");
    if fix != 0.0 {
        run.fixangle_seen += 1;
    }
    match server.client_frame_f64(cmd, DT) {
        Ok(fr) => {
            if fr.think_errors > 0 {
                let tail: String = server.vm.output.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
                run.think_errors.push(format!(
                    "frame {} t={:.1}: {} think error(s); vm.output tail: {:?}",
                    run.frames, fr.time, fr.think_errors, tail
                ));
            }
        }
        Err(e) => run.think_errors.push(format!("frame {}: client_frame Err {e}", run.frames)),
    }
    run.frames += 1;
    let after = server.vm.ent_get_vector(player, "origin");
    let jump = ((after[0] - before[0]).powi(2) + (after[1] - before[1]).powi(2) + (after[2] - before[2]).powi(2)).sqrt();
    // A teleport: a big move that set fixangle (teleport_touch does both).
    if jump > 64.0 && server.vm.ent_get_float(player, "fixangle") != 0.0 {
        let ang = server.vm.ent_get_vector(player, "angles");
        let fixangle = server.vm.ent_get_float(player, "fixangle");
        run.teleports.push(format!(
            "t={:.1} player jumped {:.0}u to {:?}, entity angles {:?}, fixangle now {}",
            server.time(), jump, after, ang, fixangle
        ));
    }
    drain(server, pak, run);
    track_pushers(server, run);
}

fn drain(server: &mut Server, pak: &Pak, run: &mut Run) {
    for s in server.drain_sounds() {
        let e = run.sounds.entry(s.sample.clone()).or_insert((0, false, false));
        e.0 += 1;
        e.1 = matches!(pak.read_file(&format!("sound/{}", s.sample)), Ok(Some(_)));
        e.2 = s.sound_index >= 0;
    }
    for m in server.drain_messages() {
        let map = if m.center { &mut run.centers } else { &mut run.prints };
        *map.entry(m.text).or_insert(0) += 1;
    }
    for ev in server.drain_svc_events() {
        run.svc.push(match ev {
            SvcEvent::Intermission => format!("t={:.1} svc_intermission", server.time()),
            SvcEvent::Finale(t) => format!("t={:.1} svc_finale {:?}", server.time(), t.chars().take(40).collect::<String>()),
            SvcEvent::Cutscene(t) => format!("t={:.1} svc_cutscene {t:?}", server.time()),
            SvcEvent::SellScreen => format!("t={:.1} svc_sellscreen", server.time()),
        });
    }
    for te in server.drain_temp_entities() {
        *run.te.entry(te.te_type).or_insert(0) += 1;
    }
    run.particles += server.drain_particles().len();
    let _ = server.drain_static_sounds();
    if !server.vm.output.is_empty() {
        run.output.push_str(&std::mem::take(&mut server.vm.output));
    }
}

fn idle(server: &mut Server, pak: &Pak, run: &mut Run, secs: f32, buttons: i32) {
    let player = server.player_edict();
    let va = server.vm.ent_get_vector(player, "v_angle");
    let cmd = UserCmd { yaw: va[1], pitch: va[0], buttons, ..Default::default() };
    for _ in 0..(f64::from(secs) / DT).round() as usize {
        frame(server, pak, run, &cmd);
    }
}

/// `PF_setorigin` through the engine's own builtin (origin + SV_LinkEdict).
/// (A builtin cannot be the VM's entry function, so call the table slot.)
fn set_origin(server: &mut Server, e: i32, org: [f32; 3]) {
    let vm = &mut server.vm;
    vm.set_gi(OFS_PARM0, e);
    vm.set_gv(OFS_PARM0 + 3, org);
    vm.argc = 2;
    let f = vm.builtins[2];
    let _ = f(vm);
}


fn point_contents(server: &mut Server, p: [f32; 3]) -> f32 {
    let vm = &mut server.vm;
    vm.set_gv(OFS_PARM0, p);
    vm.argc = 1;
    let f = vm.builtins[41];
    let _ = f(vm);
    vm.gf(quake_rs::progs::OFS_RETURN)
}

/// `traceline(a, b, TRUE, ignore)` through the engine's builtin; true when
/// nothing solid lies between (trace_fraction == 1).
fn trace_clear(server: &mut Server, a: [f32; 3], b: [f32; 3], ignore: i32) -> bool {
    let vm = &mut server.vm;
    vm.set_gv(OFS_PARM0, a);
    vm.set_gv(OFS_PARM0 + 3, b);
    vm.set_gf(OFS_PARM0 + 6, 1.0);
    vm.set_gi(OFS_PARM0 + 9, ignore);
    vm.argc = 4;
    let f = vm.builtins[16];
    let _ = f(vm);
    vm.gget_float("trace_fraction") >= 1.0
}

/// `droptofloor()` for `e` through the engine's builtin; true on success.
fn drop_to_floor(server: &mut Server, e: i32) -> bool {
    let vm = &mut server.vm;
    vm.gset_int("self", e);
    vm.argc = 0;
    let f = vm.builtins[34];
    let _ = f(vm);
    vm.gf(quake_rs::progs::OFS_RETURN) != 0.0
}

fn census_map(pak: &Pak, progs_bytes: &[u8], map: &str, rand: &Rc<QRand>, o: &mut String) -> Result<(), String> {
    let path = format!("maps/{map}.bsp");
    let bytes = pak.read_file(&path).map_err(|e| e.to_string())?.ok_or(format!("{path} not in pak"))?;
    let bsp = Bsp::parse(&bytes).map_err(|e| e.to_string())?;
    let progs = Progs::parse(progs_bytes).map_err(|e| e.to_string())?;

    // Spawn-function coverage straight from the entity lump (ED_LoadFromFile
    // prints "No spawn function for:" and frees the entity).
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for block in bsp.entities.split('}') {
        let toks: Vec<&str> = block.split('"').collect();
        let mut i = 1;
        while i + 2 < toks.len() {
            if toks[i] == "classname" {
                *classes.entry(toks[i + 2].to_string()).or_insert(0) += 1;
            }
            i += 4;
        }
    }
    let nofunc: Vec<String> = classes
        .iter()
        .filter(|(c, _)| progs.find_function(c).is_none())
        .map(|(c, n)| format!("{c} x{n}"))
        .collect();

    reset_logs();
    let mut server = Server::with_pak(bsp, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_rand(Rc::clone(rand));
    install_wrappers(&mut server.vm);
    server.set_map_name(&path);
    let rep = server.spawn_entities().map_err(|e| e.to_string())?;
    // Every brush pusher (doors, plats, buttons, trains, secret doors, walls);
    // not the world, which SV_SpawnServer also makes MOVETYPE_PUSH. Their
    // baselines are taken before the player connects: PutClientInServer's
    // force_retouch opens doors whose trigger field holds a monster during
    // the signon frames (e1m8's *6), and a door already open at the baseline
    // would read "never moved".
    let mut pushers = BTreeMap::new();
    for e in 1..server.vm.num_edicts() {
        if live(&server, e) && server.vm.ent_get_float(e as i32, "movetype") as i32 == MOVETYPE_PUSH {
            let ei = e as i32;
            pushers.insert(
                ei,
                (
                    server.vm.ent_get_string(ei, "classname"),
                    server.vm.ent_get_string(ei, "targetname"),
                    server.vm.ent_get_vector(ei, "origin"),
                    0.0,
                ),
            );
        }
    }
    let player = server.connect_client().map_err(|e| e.to_string())?;
    server.run_signon_frames();
    let spawn_log: Vec<String> = LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));

    let mut run = Run { pushers, ..Run::default() };
    track_pushers(&server, &mut run); // the signon frames' moves
    drain(&mut server, pak, &mut run);

    let _ = writeln!(o, "\n=== {map} ===");
    let _ = writeln!(
        o,
        "spawn: {} blocks, {} spawned, {} inhibited (skill), {} no spawn function, {} spawn errors",
        rep.total, rep.spawned, rep.inhibited, rep.no_spawn_function, rep.spawn_errors
    );
    if !nofunc.is_empty() {
        let _ = writeln!(o, "  classnames with no spawn function: {}", nofunc.join(", "));
    }
    for l in spawn_log.iter().filter(|l| !l.starts_with("lightstyle(") && !l.starts_with("makestatic(")) {
        let _ = writeln!(o, "  spawn-time: {l}");
    }
    let statics = spawn_log.iter().filter(|l| l.starts_with("makestatic(")).count();
    let _ = writeln!(o, "  spawn-time makestatic calls: {statics} (the C frees these edicts; the port keeps them)");

    // 1. idle
    idle(&mut server, pak, &mut run, 3.0, 0);
    let idle_movers: Vec<String> = run
        .pushers
        .iter()
        .filter(|(_, r)| r.3 > 1.0)
        .map(|(e, r)| format!("{}#{e}", r.0))
        .collect();
    let _ = writeln!(o, "idle 3s: pushers already moving: {}", if idle_movers.is_empty() { "none".into() } else { idle_movers.join(" ") });

    // 2. god + impulse 9
    let flags = server.vm.ent_get_float(player, "flags") as i32;
    server.vm.ent_set_float(player, "flags", (flags | FL_GODMODE) as f32);
    let va = server.vm.ent_get_vector(player, "v_angle");
    let cmd = UserCmd { yaw: va[1], pitch: va[0], impulse: 9, ..Default::default() };
    frame(&mut server, pak, &mut run, &cmd);
    // id1's CheatCommand also ORs in IT_KEY1|IT_KEY2; take the keys back so the
    // key pickups below still fire their targets (key_touch returns early when
    // the player already holds the key).
    let items = server.vm.ent_get_float(player, "items") as i32;
    server.vm.ent_set_float(player, "items", (items & !(131072 | 262144)) as f32);

    // 3. wake every monster
    let monsters: Vec<i32> = (0..server.vm.num_edicts())
        .filter(|&e| live(&server, e) && (server.vm.ent_get_float(e as i32, "flags") as i32) & FL_MONSTER != 0)
        .map(|e| e as i32)
        .collect();
    // 3b. a duel with one monster of each kind: put the player 96 units in
    // front of it (the first direction whose spot is open), face it, and let
    // it attack for 8 s — every shareware attack (grunt shots, dog bite, ogre
    // grenades/chainsaw, zombie gib throws, scrag spit, knight sword, fiend
    // leap, shambler lightning/claws) runs through QuakeC at least once.
    let mut kinds: BTreeMap<String, i32> = BTreeMap::new();
    for &m in &monsters {
        if live(&server, m as usize) && server.vm.ent_get_float(m, "health") > 0.0 {
            kinds.entry(server.vm.ent_get_string(m, "classname")).or_insert(m);
        }
    }
    for (kind, m) in &kinds {
        let m = *m;
        if !live(&server, m as usize) || server.vm.ent_get_float(m, "health") <= 0.0 {
            continue;
        }
        let mo = server.vm.ent_get_vector(m, "origin");
        let mut spot = None;
        for k in 0..8 {
            let a = (k as f32 * 45.0).to_radians();
            let p = [mo[0] + 96.0 * a.cos(), mo[1] + 96.0 * a.sin(), mo[2] + 8.0];
            let eye = [mo[0], mo[1], mo[2] + 16.0];
            if point_contents(&mut server, p) == -1.0 && trace_clear(&mut server, eye, p, m) {
                // Settle the player's hull onto the floor there (PF_droptofloor
                // fails if the hull starts solid, which SV_CheckStuck would
                // otherwise undo by snapping back to oldorigin).
                set_origin(&mut server, player, p);
                if drop_to_floor(&mut server, player) {
                    let q = server.vm.ent_get_vector(player, "origin");
                    server.vm.ent_set_vector(player, "oldorigin", q);
                    spot = Some(q);
                    break;
                }
            }
        }
        let Some(p) = spot else {
            let _ = writeln!(o, "duel {kind}: no open spot next to it");
            continue;
        };
        set_origin(&mut server, player, p);
        let yaw = (mo[1] - p[1]).atan2(mo[0] - p[0]).to_degrees();
        server.vm.ent_set_int(m, "enemy", player);
        let _ = call_qc(&mut server, "FoundTarget", m, player, &[]);
        let te_before: BTreeMap<u8, usize> = run.te.clone();
        let snd_before: BTreeMap<String, usize> = run.sounds.iter().map(|(k, v)| (k.clone(), v.0)).collect();
        let cmd = UserCmd { yaw, ..Default::default() };
        for _i in 0..80 {
            set_origin(&mut server, player, p); // stand still where we were put
            frame(&mut server, pak, &mut run, &cmd);
            if std::env::var("CENSUS_DUEL").as_deref() == Ok(kind.as_str()) {
                let th = server.vm.ent_get_int(m, "think");
                let fname = server.vm.progs.functions.get(th as usize).map(|f| server.vm.progs.string(f.s_name).to_string());
                eprintln!("{_i} {:?} org {:?} enemy {} health {} player {:?} psolid {} pmove {} p {:?}", fname, server.vm.ent_get_vector(m, "origin"), server.vm.ent_get_int(m, "enemy"), server.vm.ent_get_float(m, "health"), server.vm.ent_get_vector(player, "origin"), server.vm.ent_get_float(player, "solid"), server.vm.ent_get_float(player, "movetype"), p);
            }
        }
        let te: Vec<String> = run
            .te
            .iter()
            .filter(|(k, v)| te_before.get(k).copied().unwrap_or(0) < **v)
            .map(|(k, v)| format!("TE{k}x{}", v - te_before.get(k).copied().unwrap_or(0)))
            .collect();
        let snd: Vec<String> = run
            .sounds
            .iter()
            .filter(|(k, v)| snd_before.get(*k).copied().unwrap_or(0) < v.0 && !k.starts_with("player/"))
            .map(|(k, _)| k.clone())
            .collect();
        // Kill it so it does not follow the player into the next duel.
        if live(&server, m as usize) && server.vm.ent_get_float(m, "health") > 0.0 {
            let _ = call_qc(&mut server, "T_Damage", m, player, &[Arg::Ent(m), Arg::Ent(player), Arg::Ent(player), Arg::F(1000.0)]);
        }
        let _ = writeln!(
            o,
            "duel {kind}#{m}: faults so far {}, temp ents [{}], sounds [{}]",
            run.think_errors.len(),
            te.join(" "),
            snd.join(" ")
        );
    }

    let mut wake_err = 0;
    for &m in &monsters {
        server.vm.ent_set_int(m, "enemy", player);
        if call_qc(&mut server, "FoundTarget", m, player, &[]).is_err() {
            wake_err += 1;
        }
    }
    idle(&mut server, pak, &mut run, 20.0, 0);
    let _ = writeln!(o, "woke {} monsters ({} FoundTarget errors), fought 20s", monsters.len(), wake_err);

    // 4. kill every monster
    let mut killed = 0;
    let mut kill_err = 0;
    for (i, &m) in monsters.iter().enumerate() {
        if !live(&server, m as usize) || server.vm.ent_get_float(m, "health") <= 0.0 {
            continue;
        }
        let dmg = if i % 2 == 1 { server.vm.ent_get_float(m, "health") } else { 1000.0 };
        match call_qc(&mut server, "T_Damage", m, player, &[Arg::Ent(m), Arg::Ent(player), Arg::Ent(player), Arg::F(dmg)]) {
            Ok(()) => killed += 1,
            Err(e) => {
                kill_err += 1;
                run.think_errors.push(format!("T_Damage on {}: {e}", server.vm.ent_get_string(m, "classname")));
            }
        }
        frame(&mut server, pak, &mut run, &UserCmd::default());
    }
    idle(&mut server, pak, &mut run, 5.0, 0);
    let alive: Vec<String> = monsters
        .iter()
        .filter(|&&m| live(&server, m as usize) && server.vm.ent_get_float(m, "health") > 0.0 && (server.vm.ent_get_float(m, "flags") as i32) & FL_MONSTER != 0)
        .map(|&m| server.vm.ent_get_string(m, "classname"))
        .collect();
    let _ = writeln!(
        o,
        "killed {killed} ({kill_err} errors); still alive: {}; killed_monsters={} total_monsters={}",
        if alive.is_empty() { "none".into() } else { alive.join(" ") },
        server.vm.gget_float("killed_monsters"),
        server.vm.gget_float("total_monsters")
    );

    // 5. touch every item
    let items: Vec<i32> = (0..server.vm.num_edicts())
        .filter(|&e| {
            live(&server, e) && {
                let c = server.vm.ent_get_string(e as i32, "classname");
                (c.starts_with("item_") || c.starts_with("weapon_")) && server.vm.ent_get_int(e as i32, "touch") > 0
            }
        })
        .map(|e| e as i32)
        .collect();
    let mut item_err = 0;
    for &it in &items {
        // Call the touch SV_TouchLinks would call. The player is NOT moved
        // there: a trigger/item centre is often inside a mover's path, where
        // the pusher would be blocked by the player (plat_crush) and
        // SV_CheckStuck would snap the player back anyway.
        let f = server.vm.ent_get_int(it, "touch") as usize;
        if call_fnum(&mut server, f, it, player, &[]).is_err() {
            item_err += 1;
        }
        frame(&mut server, pak, &mut run, &UserCmd::default());
    }
    let _ = writeln!(
        o,
        "touched {} items ({} errors); player items={:#x} health={} armor={}",
        items.len(),
        item_err,
        server.vm.ent_get_float(player, "items") as i64,
        server.vm.ent_get_float(player, "health"),
        server.vm.ent_get_float(player, "armorvalue")
    );

    // 5b. e1m7: Chthon. The sigil pickup above woke him (item_sigil -> t4 ->
    // boss_awake). Kill him the way a player does: raise both electrode doors
    // (buttons -> t12/t13, func_doors whose target is "lightning"), then press
    // the lightning button (-> t14 -> event_lightning -> lightning_use, which
    // only hurts him when both electrodes are at STATE_TOP); re-press the
    // electrode buttons so their 20 s wait re-arms and they come back down.
    if map == "e1m7" {
        let boss = (0..server.vm.num_edicts() as i32)
            .find(|&e| live(&server, e as usize) && server.vm.ent_get_string(e, "classname") == "monster_boss");
        let buttons = |server: &Server, target: &str| -> Vec<i32> {
            (0..server.vm.num_edicts() as i32)
                .filter(|&e| {
                    live(server, e as usize)
                        && server.vm.ent_get_string(e, "classname") == "func_button"
                        && server.vm.ent_get_string(e, "target") == target
                })
                .collect()
        };
        let press = |server: &mut Server, run: &mut Run, b: i32| {
            let f = server.vm.ent_get_int(b, "touch") as usize;
            let c = center(server, b);
            let p = server.player_edict();
            set_origin(server, p, c);
            let _ = call_fnum(server, f, b, p, &[]);
            frame(server, pak, run, &UserCmd::default());
        };
        idle(&mut server, pak, &mut run, 6.0, 0); // he rises out of the lava
        for round in 0..4 {
            let Some(b) = boss else { break };
            if !live(&server, b as usize) {
                break;
            }
            for t in ["t12", "t13"] {
                for btn in buttons(&server, t) {
                    press(&mut server, &mut run, btn);
                }
            }
            idle(&mut server, pak, &mut run, 4.0, 0);
            let doors: Vec<(String, f32)> = (0..server.vm.num_edicts() as i32)
                .filter(|&e| live(&server, e as usize) && server.vm.ent_get_string(e, "target") == "lightning")
                .map(|e| (server.vm.ent_get_string(e, "targetname"), server.vm.ent_get_float(e, "state")))
                .collect();
            for btn in buttons(&server, "t14") {
                press(&mut server, &mut run, btn);
            }
            idle(&mut server, pak, &mut run, 3.0, 0);
            let (h, alive) = if live(&server, b as usize) {
                (server.vm.ent_get_float(b, "health"), server.vm.ent_get_string(b, "classname"))
            } else {
                (0.0, "freed".into())
            };
            let _ = writeln!(o, "chthon round {round}: electrodes {doors:?} (STATE_TOP=0) -> boss health {h} ({alive})");
            for t in ["t12", "t13"] {
                for btn in buttons(&server, t) {
                    press(&mut server, &mut run, btn);
                }
            }
            idle(&mut server, pak, &mut run, 24.0, 0);
        }
        idle(&mut server, pak, &mut run, 10.0, 0);
        let _ = writeln!(
            o,
            "chthon: boss {} ; killed_monsters={} total_monsters={}",
            match boss {
                Some(b) if live(&server, b as usize) => format!("still there, health {}", server.vm.ent_get_float(b, "health")),
                _ => "removed".into(),
            },
            server.vm.gget_float("killed_monsters"),
            server.vm.gget_float("total_monsters")
        );
    }

    // 6. three trigger passes
    let mut exits: Vec<i32> = Vec::new();
    let mut trig_err = 0;
    let mut touched = 0;
    for pass in 0..3 {
        let targets: Vec<i32> = (0..server.vm.num_edicts())
            .filter(|&e| live(&server, e) && e as i32 != player)
            .map(|e| e as i32)
            .collect();
        for t in targets {
            if !live(&server, t as usize) {
                continue;
            }
            let class = server.vm.ent_get_string(t, "classname");
            let solid = server.vm.ent_get_float(t, "solid") as i32;
            let touch = server.vm.ent_get_int(t, "touch");
            let shootable = server.vm.ent_get_float(t, "takedamage") > 0.0
                && (server.vm.ent_get_float(t, "flags") as i32) & FL_MONSTER == 0
                && !class.starts_with("misc_explobox");
            if class == "trigger_changelevel" {
                if pass == 0 {
                    exits.push(t);
                }
                continue;
            }
            if class.starts_with("item_") || class.starts_with("weapon_") {
                continue;
            }
            if touch > 0 && (solid == SOLID_TRIGGER || solid == SOLID_BSP) {
                if call_fnum(&mut server, touch as usize, t, player, &[]).is_err() {
                    trig_err += 1;
                    run.think_errors.push(format!("touch of {class}#{t} faulted"));
                }
                touched += 1;
                frame(&mut server, pak, &mut run, &UserCmd::default());
            }
            if shootable && live(&server, t as usize) {
                let _ = call_qc(&mut server, "T_Damage", t, player, &[Arg::Ent(t), Arg::Ent(player), Arg::Ent(player), Arg::F(200.0)]);
                frame(&mut server, pak, &mut run, &UserCmd::default());
            }
        }
        idle(&mut server, pak, &mut run, 3.0, 0);
    }
    idle(&mut server, pak, &mut run, 8.0, 0);
    let _ = writeln!(o, "trigger passes: {touched} touches ({trig_err} faults)");
    let mut still: Vec<String> = run
        .pushers
        .iter()
        .filter(|(_, r)| r.3 <= 1.0)
        .map(|(e, r)| {
            format!(
                "{}#{e}({}){}",
                r.0,
                server.vm.ent_get_string(*e, "model"),
                if r.1.is_empty() { String::new() } else { format!("[{}]", r.1) }
            )
        })
        .collect();
    still.sort();
    let moved = run.pushers.values().filter(|r| r.3 > 1.0).count();
    let _ = writeln!(o, "pushers: {} total, {} moved, never moved: {}", run.pushers.len(), moved, still.join(" "));

    // 7. the exit
    if let Some(&x) = exits.first() {
        let f = server.vm.ent_get_int(x, "touch") as usize;
        let c = center(&server, x);
        set_origin(&mut server, player, c);
        let _ = call_fnum(&mut server, f, x, player, &[]);
        idle(&mut server, pak, &mut run, 6.0, 0);
        idle(&mut server, pak, &mut run, 0.3, 1); // ExitIntermission on fire
        idle(&mut server, pak, &mut run, 2.0, 0);
        // The C executes a changelevel on the next frame; only an episode end
        // (no changelevel issued: the finale) takes a second press.
        if !LOG.with(|l| l.borrow().iter().any(|x| x.starts_with("changelevel("))) {
            idle(&mut server, pak, &mut run, 0.3, 1);
            idle(&mut server, pak, &mut run, 2.0, 0);
        }
    }
    let _ = writeln!(o, "exits: {} trigger_changelevel; svc events: {}", exits.len(), if run.svc.is_empty() { "none".into() } else { run.svc.join(" | ") });

    // Report.
    let _ = writeln!(o, "frames run: {} (sim t={:.1})", run.frames, server.time());
    let _ = writeln!(o, "QC faults: {}", run.think_errors.len());
    for e in run.think_errors.iter().take(12) {
        let _ = writeln!(o, "  {e}");
    }
    let _ = writeln!(o, "fixangle set on the player in {} frames; teleports (big moves that set fixangle):", run.fixangle_seen);
    for t in run.teleports.iter().take(12) {
        let _ = writeln!(o, "  {t}");
    }
    let runtime_log: Vec<String> = LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));
    let mut seen = BTreeMap::new();
    for l in &runtime_log {
        *seen.entry(l.clone()).or_insert(0usize) += 1;
    }
    let _ = writeln!(o, "runtime builtin log (stuffcmd/localcmd/cvar_set/lightstyle/changelevel/...):");
    for (l, n) in &seen {
        let _ = writeln!(o, "  {l} x{n}");
    }
    let calls = CALLS.with(|c| c.borrow().clone());
    let _ = writeln!(
        o,
        "builtins executed: {}",
        calls.iter().map(|(n, c)| format!("{}={c}", builtin_name(*n))).collect::<Vec<_>>().join(" ")
    );
    let missing_models: Vec<String> = MODELS
        .with(|s| s.borrow().clone())
        .into_iter()
        .chain(PRECACHE.with(|s| s.borrow().clone()).into_iter().filter(|m| !m.ends_with(".wav")))
        .filter(|m| !m.is_empty() && !m.starts_with('*'))
        .filter(|m| !matches!(pak.read_file(m), Ok(Some(_))))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let _ = writeln!(o, "models set/precached but not in the pak: {}", if missing_models.is_empty() { "none".into() } else { missing_models.join(" ") });
    let missing_snd: Vec<String> = PRECACHE
        .with(|s| s.borrow().clone())
        .into_iter()
        .filter(|m| m.ends_with(".wav") && !matches!(pak.read_file(&format!("sound/{m}")), Ok(Some(_))))
        .collect();
    let _ = writeln!(o, "sounds precached but not in the pak: {}", if missing_snd.is_empty() { "none".into() } else { missing_snd.join(" ") });
    let _ = writeln!(o, "sounds played ({} distinct):", run.sounds.len());
    for (s, (n, inpak, pre)) in &run.sounds {
        let _ = writeln!(o, "  {s} x{n}{}{}", if *inpak { "" } else { "  NOT IN PAK" }, if *pre { "" } else { "  NOT PRECACHED" });
    }
    let _ = writeln!(o, "centerprints: {:?}", run.centers);
    let _ = writeln!(o, "prints: {:?}", run.prints.keys().take(40).collect::<Vec<_>>());
    let _ = writeln!(o, "temp entities by TE_ type: {:?}; particle() bursts: {}", run.te, run.particles);
    if !run.output.is_empty() {
        let out: String = run.output.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
        let _ = writeln!(o, "vm.output (dprint/error/objerror):\n{out}");
    }
    Ok(())
}

pub fn cmd_census(pak_path: &str, maps: &[String]) -> Result<String, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let progs = pak.read_file("progs.dat").map_err(|e| e.to_string())?.ok_or("no progs.dat")?;
    let default: Vec<String> =
        ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"].iter().map(|s| s.to_string()).collect();
    let maps = if maps.is_empty() { &default[..] } else { maps };
    let mut o = String::new();
    // One session: every map's server draws from the same random streams, as
    // id's all draw from one libc rand().
    let rand = Rc::new(QRand::new());
    for m in maps {
        if let Err(e) = census_map(&pak, &progs, m, &rand, &mut o) {
            let _ = writeln!(o, "\n=== {m} ===\nFAILED: {e}");
        }
    }
    Ok(o)
}

/// `quaketool census-edicts <pak> <map> <t1,t2,...> [--script S]` — load a map
/// the way the browser build does (spawn, connect, signon frames), let it run
/// idle (the player standing at info_player_start, no input), and at each
/// server time `t` dump every live edict in exactly the oracle's
/// `oracle_edicts` format, so `census/edict_diff.py` can diff the port's
/// simulation against id's.
pub fn cmd_census_edicts(pak_path: &str, map: &str, times: &str) -> Result<String, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let progs_bytes = pak.read_file("progs.dat").map_err(|e| e.to_string())?.ok_or("no progs.dat")?;
    let path = format!("maps/{map}.bsp");
    let bytes = pak.read_file(&path).map_err(|e| e.to_string())?.ok_or(format!("{path} not in pak"))?;
    let bsp = Bsp::parse(&bytes).map_err(|e| e.to_string())?;
    let progs = Progs::parse(&progs_bytes).map_err(|e| e.to_string())?;
    let mut server = Server::with_pak(bsp, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(&path);
    server.spawn_entities().map_err(|e| e.to_string())?;
    let player = server.connect_client().map_err(|e| e.to_string())?;
    server.run_signon_frames();
    let mut times: Vec<f32> = times.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let va = server.vm.ent_get_vector(player, "v_angle");
    let ang = server.vm.ent_get_vector(player, "angles");
    let cmd = UserCmd { yaw: ang[1], pitch: va[0], ..Default::default() };
    let mut o = String::new();
    for t in times {
        while server.time() + 0.05 < t {
            let _ = server.client_frame_f64(&cmd, DT);
            let _ = server.drain_sounds();
            let _ = server.drain_messages();
            let _ = server.drain_temp_entities();
            let _ = server.drain_particles();
            let _ = server.drain_svc_events();
            let _ = server.drain_static_sounds();
        }
        let vm = &server.vm;
        let n = vm.num_edicts();
        let _ = writeln!(o, "# t={:.3} num_edicts={}", server.time(), n);
        for e in 0..n {
            if !live(&server, e) {
                continue;
            }
            let ei = e as i32;
            let org = vm.ent_get_vector(ei, "origin");
            let an = vm.ent_get_vector(ei, "angles");
            let (mi, ma) = (vm.ent_get_vector(ei, "mins"), vm.ent_get_vector(ei, "maxs"));
            let _ = writeln!(
                o,
                "{e}\t{}\t{}\t{:.3} {:.3} {:.3}\t{:.3} {:.3} {:.3}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{:.3} {:.3} {:.3}\t{:.3} {:.3} {:.3}",
                vm.ent_get_string(ei, "classname"),
                vm.ent_get_string(ei, "model"),
                org[0], org[1], org[2], an[0], an[1], an[2],
                vm.ent_get_float(ei, "frame"),
                vm.ent_get_float(ei, "movetype"),
                vm.ent_get_float(ei, "solid"),
                vm.ent_get_float(ei, "flags"),
                vm.ent_get_float(ei, "health"),
                vm.ent_get_float(ei, "nextthink"),
                vm.ent_get_float(ei, "effects"),
                vm.ent_get_string(ei, "targetname"),
                mi[0], mi[1], mi[2], ma[0], ma[1], ma[2],
            );
        }
    }
    Ok(o)
}
