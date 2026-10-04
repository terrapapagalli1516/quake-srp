//! Regression coverage for the user's report, round 2: Scourge of Armagon's
//! real `maps/start.bsp` (the hipnotic start map), whose exit is a rotating
//! door: a `rotate_object` (purely visual, SOLID_NOT) swung by a
//! `func_rotate_door` controller, whose real collision is a group of
//! `func_movewall` entities sharing one targetname, opened by a floor-plate
//! `func_button`.
//!
//! Root cause (startdoor brief, round 2, fleet/startdoor, 2026-10-02):
//! `quake-rs/src/server/sv_world.rs`'s `sv_move` (`SV_ClipToLinks`) resolved
//! a `SOLID_BSP` entity's hull by re-parsing its *live* `model` string field
//! (`"*N"` -> submodel `N`) on every clip, instead of from `modelindex` (set
//! once by `setmodel`, independent of the entity's own field afterwards) —
//! id's real `SV_HullForEntity` always uses `sv.models[(int)
//! ent->v.modelindex]`, never the `model` string. Hipnotic's own
//! `func_movewall` (`quakec_hipnotic/hiprot.qc`) calls `setmodel` and then,
//! by design, unconditionally blanks `self.model` right after — a common,
//! legitimate QuakeC idiom — which silently made these entities uncollidable
//! in the port the instant they spawned, even though `solid` stayed
//! `SOLID_BSP` and the real hull (`modelindex`) was perfectly valid: a
//! closed, "solid"-looking door the player walked straight through. Fixed by
//! resolving the submodel through `Host::model_name(modelindex)`, a new
//! trait method backed by the engine's own (immutable, set-once) precache
//! table — see `Host::model_name`'s doc and the synthetic regression
//! `server::sv_world::tests::sv_move_stops_at_solid_bsp_entity_even_once_its_model_field_is_cleared`.
//!
//! Verified against id's own C oracle too: a real, untouched walk from
//! `start.bsp`'s true spawn straight at the closed door blocks at y=-337.83
//! in id's C (`census/oracle_run.py --hipnotic-pak`, `oracle_walk`) and at
//! y=-368.03 in this port post-fix (both well short of the movewalls'
//! y=-353..-235 span) — both engines now agree the closed door blocks; pre-fix
//! the port sailed through to y=+559.97.
//!
//! Gated on `QUAKE_HIP1M1_PAK` pointing at hipnotic's own `pak0.pak`
//! (self-contained: has `progs.dat` and `maps/start.bsp` both), mirroring
//! `pr_edict.rs`'s `QUAKE_R2M6_DIR` test.

use quake_rs::bsp::Bsp;
use quake_rs::pak::Pak;
use quake_rs::progs::Progs;
use quake_rs::server::{Server, UserCmd};

fn find(server: &Server, pred: impl Fn(i32) -> bool) -> Option<i32> {
    (0..server.vm.num_edicts() as i32).find(|&e| !server.vm.is_free_edict(e) && pred(e))
}
fn find_all(server: &Server, pred: impl Fn(i32) -> bool) -> Vec<i32> {
    (0..server.vm.num_edicts() as i32).filter(|&e| !server.vm.is_free_edict(e) && pred(e)).collect()
}

#[test]
#[ignore]
fn closed_rotating_door_blocks_the_player_and_opens_once_triggered() {
    let Ok(pak_path) = std::env::var("QUAKE_HIP1M1_PAK") else {
        eprintln!("QUAKE_HIP1M1_PAK not set; skipping (see this test's doc comment)");
        return;
    };
    let pak = Pak::open(&pak_path).expect("open hipnotic's pak0.pak");
    let progs_bytes = pak.read_file("progs.dat").expect("read").expect("progs.dat present");
    let bsp_bytes = pak.read_file("maps/start.bsp").expect("read").expect("start.bsp present");
    let progs = Progs::parse(&progs_bytes).expect("parse progs.dat");
    let bsp = Bsp::parse(&bsp_bytes).expect("parse start.bsp");

    let mut server = Server::with_pak(bsp, progs, Some(pak)).expect("server");
    server.set_map_name("maps/start.bsp");
    server.spawn_entities().expect("spawn");
    let player = server.connect_client().expect("connect");
    server.run_signon_frames().expect("signon");

    let rotate_obj = find(&server, |e| server.vm.ent_get_string(e, "classname") == "rotate_object")
        .expect("the rotate_object (*65)");
    let controller = find(&server, |e| server.vm.ent_get_string(e, "classname") == "func_rotate_door")
        .expect("the func_rotate_door controller (damndoor)");
    let movewalls = find_all(&server, |e| server.vm.ent_get_string(e, "classname") == "func_movewall");
    assert!(movewalls.len() >= 20, "expected a few dozen func_movewalls, found {}", movewalls.len());

    const SOLID_BSP: f32 = 4.0;
    for &e in &movewalls {
        assert_eq!(
            server.vm.ent_get_vector(e, "origin"),
            [0.0, 0.0, 0.0],
            "a movewall must start at its closed origin"
        );
        assert_eq!(server.vm.ent_get_float(e, "solid"), SOLID_BSP, "a movewall must start solid");
        // The exact bug this round found: `setmodel` already ran (real
        // bounds below prove it), but hiprot.qc's own func_movewall blanks
        // the display string right after -- the port must still collide
        // using `modelindex`, not this field.
        assert_eq!(server.vm.ent_get_string(e, "model"), "", "func_movewall blanks its own `model` string by design");
        let (mn, mx) = (server.vm.ent_get_vector(e, "absmin"), server.vm.ent_get_vector(e, "absmax"));
        assert!(mx[0] > mn[0] && mx[1] > mn[1] && mx[2] > mn[2], "a real submodel bounding box (from modelindex)");
    }

    // The rotate_object is purely visual (SOLID_NOT): it never blocked
    // anything, in the port or in id's C (both show its angles never move
    // from a stationary plate-touch alone -- a faithful, not a port, detail).
    assert_ne!(
        server.vm.ent_get_float(rotate_obj, "solid"),
        SOLID_BSP,
        "the rotate_object is decorative, not collision"
    );

    // (1) Closed-door collision: walk the real player, from the map's own
    // info_player_start, straight at the door. It must be BLOCKED well
    // short of the movewalls (y=-353..-235) -- id's C oracle blocks the same
    // walk at y=-337.83 (this test's doc comment).
    let cmd = UserCmd { forwardmove: 320.0, yaw: 90.0, ..Default::default() }; // +Y, toward the door
    let mut last_y = f32::MIN;
    let mut blocked = false;
    for i in 0..80 {
        server.client_frame_f64(&cmd, 0.1).expect("frame");
        let y = server.vm.ent_get_vector(player, "origin")[1];
        if i > 20 && (y - last_y).abs() < 0.01 {
            blocked = true;
        }
        last_y = y;
    }
    assert!(blocked, "the player must be blocked by the closed movewalls, stopped at y={last_y}");
    assert!(last_y < -200.0, "blocked well short of the door's own span (-353..-235), got y={last_y}");

    // (2) Trigger the door (the plate's func_button touch -> SUB_UseTargets
    // -> damndoor.use; calling the controller's own `use` directly has the
    // same effect without scripting a walk onto the exact plate box) and run
    // it out. At least the movewalls must move off their closed origin --
    // the real collision clearing the user's report of walking through
    // it depends on.
    let use_fn = server.vm.progs().find_function("rotate_door_use").expect("rotate_door_use exists");
    server.vm.set_glob_int(server.vm.go().self_, controller);
    server.vm.set_glob_int(server.vm.go().other, controller);
    server.vm.execute(use_fn).expect("rotate_door_use");

    for _ in 0..50 {
        server.client_frame_f64(&UserCmd::default(), 0.1).expect("frame");
    }
    let moved = movewalls.iter().filter(|&&e| server.vm.ent_get_vector(e, "origin") != [0.0, 0.0, 0.0]).count();
    assert!(moved > 0, "at least one movewall must have moved off its closed origin once triggered");
    for &e in &movewalls {
        assert_eq!(
            server.vm.ent_get_float(e, "solid"),
            SOLID_BSP,
            "a movewall stays solid while it moves (collision follows it)"
        );
    }
}
