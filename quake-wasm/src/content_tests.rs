//! The player's own files, end to end on the shareware data: the registered
//! game through a search path whose `pak1.pak` is synthetic (id's `pop.lmp`
//! from common.c's table, a shareware map copied as `maps/e2m1.bsp`: no
//! registered data in the repo), and the CD's records on the wire.

use std::rc::Rc;

use quake_rs::cd_audio::CdCall;
use quake_rs::client::{host_cmd, SoundCall, Walk};
use quake_rs::common::pop_lmp;
use quake_rs::pak::{write_pack, Pak};
use quake_rs::qrand::QRand;

use crate::cl_walk::step_walk;
use crate::proto::{encode, Record};

/// The shareware pak with a synthetic registered `pak1.pak` in front of it:
/// id's `gfx/pop.lmp`, and e1m1 again as `maps/e2m1.bsp`.
fn registered_path() -> Pak {
    let pak0 = crate::common::pak().expect("the shareware pak");
    let e1m1 = pak0.read_file("maps/e1m1.bsp").unwrap().unwrap();
    let pak1 = write_pack(&[("gfx/pop.lmp", &pop_lmp()), ("maps/e2m1.bsp", &e1m1)]);
    Pak::from_bytes("id1/pak1.pak".into(), pak1).unwrap().over(pak0)
}

/// A walk on `map` through `path`, and the sound calls its load made.
fn walk(path: Pak, map: &str) -> (Option<Walk>, Vec<SoundCall>) {
    let mut sound = Vec::new();
    let w = host_cmd::build_walk_map(path, map, &Rc::new(QRand::new()), &mut sound);
    (w, sound)
}

fn cd_calls(sound: &[SoundCall]) -> Vec<CdCall> {
    sound.iter().filter_map(|c| if let SoundCall::Cd(cd) = c { Some(*cd) } else { None }).collect()
}

#[test]
fn e2m1_loads_from_pak1_and_the_level_asks_for_its_track() {
    let shareware = crate::common::pak().unwrap();
    assert!(walk(shareware, "maps/e2m1.bsp").0.is_none(), "shareware has no e2m1");
    let (w, sound) = walk(registered_path(), "maps/e2m1.bsp");
    let w = w.expect("e2m1 loads from pak1");
    assert_eq!(w.map_name, "maps/e2m1.bsp");
    // SV_SendServerinfo's svc_cdtrack: e1m1's data, so e1m1's `sounds` 6.
    assert_eq!(cd_calls(&sound), [CdCall::Play { track: 6, looping: true }]);
}

/// The start map's episode gates are `trigger_onlyregistered`: shareware
/// centerprints "For registered users only!" and stays; registered, the
/// trigger fires its targets and removes itself (triggers.qc, on
/// `cvar("registered")`).
#[test]
fn the_start_maps_episode_gate_opens_only_when_registered() {
    let touch_gate = |path: Pak| {
        let mut w = walk(path, "maps/start.bsp").0.expect("start");
        let vm = &w.server.vm;
        let gate = (0..vm.num_edicts() as i32)
            .find(|&e| {
                !vm.is_free_edict(e)
                    && vm.ent_get_string(e, "classname") == "trigger_onlyregistered"
                    && vm.ent_get_string(e, "message").starts_with("For registered users only!")
            })
            .expect("start has the gate");
        let (a, b) = (vm.ent_get_vector(gate, "absmin"), vm.ent_get_vector(gate, "absmax"));
        let centre = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5];
        // PF_setorigin, so the touch sees the player in the trigger.
        let player = w.player;
        let vm = &mut w.server.vm;
        vm.set_gi(quake_rs::progs::OFS_PARM0, player);
        vm.set_gv(quake_rs::progs::OFS_PARM0 + 3, centre);
        vm.call_builtin(2, 2).expect("setorigin");
        for _ in 0..3 {
            let _ = step_walk(&mut w, 0.1, false, &crate::vid::mode_vid(320, 200));
        }
        let gone = w.server.vm.is_free_edict(gate);
        (gone, w.centerprint.map(|(t, _)| t).unwrap_or_default())
    };
    let (gone, text) = touch_gate(crate::common::pak().unwrap());
    assert!(!gone && text.starts_with("For registered users only!"), "shareware: {gone} {text:?}");
    let (gone, text) = touch_gate(registered_path());
    assert!(gone && text.is_empty(), "registered: the gate is gone, silently ({text:?})");
}

/// With a disc (`-cdtracks`), the attract loop's demo1 plays its forced
/// track 2 and a level its own; the level follows `bgmvolume`; `pause`
/// pauses it. Without one, no `Cd` record is ever written.
#[test]
fn the_cd_plays_beside_the_mix_in_cd_records() {
    let cd_records = |input: &[u8], args: &[&str]| {
        let mut out = Vec::new();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        crate::sys::run(input, &mut out, &args).expect("runs");
        crate::app::APP.with(|c| *c.borrow_mut() = None);
        Record::split(&out)
            .into_iter()
            .filter(|r| r.kind == Record::CD)
            .map(|r| (r.u32_at(0), r.payload[4], r.payload[5], r.payload[6], f32::from_le_bytes(r.payload[8..12].try_into().unwrap())))
            .collect::<Vec<_>>()
    };
    let mut input = Vec::new();
    input.extend(encode::tick(1, 1.0 / 60.0));
    input.extend(encode::call(1, "exec bgmvolume 0.5"));
    input.extend(encode::tick(2, 1.0 / 60.0));
    input.extend(encode::call(2, "exec map e1m1"));
    input.extend(encode::tick(3, 1.0 / 60.0));
    input.extend(encode::call(3, "exec pause"));
    input.extend(encode::tick(4, 1.0 / 60.0));
    let recs = cd_records(&input, &["-cdtracks", "2,3,4,5,6,7,8,9,10,11"]);
    let full = 1.0;
    let half = 127.0 / 255.0;
    assert_eq!(
        recs,
        [
            (1, 2, 1, 1, full),  // demo1: its header's forced track 2, looping
            (1, 2, 1, 1, half),  // bgmvolume 0.5: (int)(0.5 * 255) = 127
            (2, 6, 1, 1, half),  // e1m1's `sounds` 6, from its top
            (2, 6, 1, 2, half),  // paused where it was
        ]
    );
    assert!(cd_records(&input, &[]).is_empty(), "no music, no drive, no records");
}

/// Episode 1's end, registered: e1m7's exit plays the intermission's CD
/// track 3, `ExitIntermission` (on `cvar("registered")`) shows the registered
/// finale text with the finale's track 2, and the next press goes on to
/// `start` where shareware gets the sell screen (`cl_walk`'s
/// `e1m7_exit_reaches_the_shareware_finale_and_sellscreen`).
#[test]
fn e1m7_registered_goes_on_after_the_finale_with_the_cds_tracks() {
    use quake_rs::client::cl_main::walk_frame;
    let mut w = walk(registered_path(), "maps/e1m7.bsp").0.expect("e1m7");
    let vid = crate::vid::mode_vid(320, 200);
    let mut cds = Vec::new();
    let mut frame = |w: &mut Walk| {
        let f = walk_frame(w, 0.1, false, &vid);
        cds.extend(cd_calls(&f.sound));
        quake_rs::render::recycle_image(f.image);
    };
    let vm = &w.server.vm;
    let exit = (0..vm.num_edicts() as i32)
        .find(|&e| !vm.is_free_edict(e) && vm.ent_get_string(e, "classname") == "trigger_changelevel")
        .expect("e1m7's exit");
    let (a, b) = (vm.ent_get_vector(exit, "absmin"), vm.ent_get_vector(exit, "absmax"));
    let centre = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5];
    for _ in 0..40 {
        let p = w.player;
        w.server.vm.ent_set_vector(p, "origin", centre);
        w.server.vm.ent_set_vector(p, "velocity", [0.0; 3]);
        frame(&mut w);
        if w.intermission != 0 {
            break;
        }
    }
    assert_eq!(w.intermission, 1);
    w.in_attack = true;
    for _ in 0..30 {
        frame(&mut w);
        if w.intermission == 2 {
            break;
        }
    }
    assert_eq!(w.intermission, 2);
    assert!(w.finale_text.contains("A Rune of magic\npower lies at the end of each haunted"), "{:?}", w.finale_text);
    w.in_attack = false;
    for _ in 0..15 {
        frame(&mut w);
    }
    w.in_attack = true;
    for _ in 0..30 {
        frame(&mut w);
        if w.map_name != "maps/e1m7.bsp" {
            break;
        }
    }
    assert_eq!(w.map_name, "maps/start.bsp", "registered: on to the start map");
    assert!(!w.pending_sellscreen, "no sell screen");
    // The intermission's 3, the finale's 2, then start's own 4.
    let tracks: Vec<u8> = cds.iter().map(|c| if let CdCall::Play { track, .. } = c { *track } else { 0 }).collect();
    assert_eq!(tracks, [3, 2, 4]);
}
