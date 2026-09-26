//! Census tests (CENSUS.md): each test asserts what id's WinQuake does, on the
//! real shareware data, through the live browser path (`build_walk_map` +
//! `step_walk`). Each was written `#[ignore]`d as the evidence for one
//! CENSUS.md finding and un-ignored by the commit that fixed it; all of them
//! run now, as those fixes' regression tests. A new finding's test starts
//! `#[ignore]`d the same way (`cargo test --release census -- --ignored`
//! lists what is still open).

use quake_rs::progs::OFS_PARM0;
use quake_rs::render;

use crate::app::{build_walk, build_walk_map, Walk};
use crate::cl_walk::step_walk;

/// `PF_setorigin` through the engine's own builtin (origin + SV_LinkEdict), so
/// the absmin/absmax the trigger touches read are right.
fn set_origin(w: &mut Walk, e: i32, org: [f32; 3]) {
    let vm = &mut w.server.vm;
    vm.set_gi(OFS_PARM0, e);
    vm.set_gv(OFS_PARM0 + 3, org);
    vm.call_builtin(2, 2).expect("setorigin");
}

fn live(w: &Walk, e: i32) -> bool {
    !w.server.vm.is_free_edict(e)
}

fn find(w: &Walk, pred: impl Fn(&Walk, i32) -> bool) -> Option<i32> {
    (0..w.server.vm.num_edicts() as i32).find(|&e| live(w, e) && pred(w, e))
}

fn class(w: &Walk, e: i32) -> String {
    w.server.vm.ent_get_string(e, "classname")
}

fn centre(w: &Walk, e: i32) -> [f32; 3] {
    let a = w.server.vm.ent_get_vector(e, "absmin");
    let b = w.server.vm.ent_get_vector(e, "absmax");
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5]
}

/// One walk frame; returns its colour shifts (`cl.cshifts`, in order).
fn step(w: &mut Walk, dt: f64) -> Vec<([u8; 3], f32)> {
    step_walk(w, dt, false, &crate::vid::mode_vid(320, 200)).1
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// CENSUS F1 (HIGH). A teleporter turns the view to the destination's facing:
/// QC `teleport_touch` sets `other.angles = t.mangle; other.fixangle = 1`, and
/// SV_WriteClientdataToMessage sends svc_setangle, so the client's
/// `cl.viewangles` become the destination's angles. The port's view angles
/// (`w.yaw`/`w.pitch`) are only ever written by input.
#[test]
fn census_teleport_turns_the_view_to_the_destination() {
    let mut w = build_walk_map("maps/start.bsp").expect("start boots");
    for _ in 0..5 {
        step(&mut w, 0.1);
    }
    // The first always-armed teleporter (no targetname) and its destination.
    let tele = find(&w, |w, e| {
        class(w, e) == "trigger_teleport" && w.server.vm.ent_get_string(e, "targetname").is_empty()
    })
    .expect("start has a trigger_teleport");
    let target = w.server.vm.ent_get_string(tele, "target");
    let dest = find(&w, |w, e| {
        class(w, e) == "info_teleport_destination" && w.server.vm.ent_get_string(e, "targetname") == target
    })
    .expect("its destination");
    let dest_yaw = w.server.vm.ent_get_vector(dest, "mangle")[1];
    // Face 90 degrees away from the destination's facing, then step in.
    w.yaw = dest_yaw + 90.0;
    w.pitch = 20.0;
    let p = w.player;
    let c = centre(&w, tele);
    set_origin(&mut w, p, c);
    step(&mut w, 0.1);
    step(&mut w, 0.1);
    let moved = w.server.vm.ent_get_vector(p, "origin");
    assert!(
        (moved[0] - c[0]).abs() + (moved[1] - c[1]).abs() > 64.0,
        "the teleporter moved the player (origin {moved:?})"
    );
    assert!(
        angle_diff(w.yaw, dest_yaw) < 1.0 && w.pitch.abs() < 1.0,
        "after the teleport the view faces the destination (yaw {dest_yaw}, pitch 0); the port kept yaw {} pitch {}",
        w.yaw,
        w.pitch
    );
}

/// CENSUS F2 (HIGH). Single player pauses behind the menu and console:
/// Host_ServerFrame runs SV_Physics only `if (!sv.paused && (svs.maxclients > 1
/// || key_dest == key_game))`. The port ticks the server with a zeroed usercmd
/// ("the world still TICKS"), so monsters keep attacking behind the menu.
#[test]
fn census_single_player_pauses_behind_the_menu() {
    let mut w = build_walk().expect("e1m1 boots");
    for _ in 0..5 {
        step(&mut w, 0.1);
    }
    let t0 = w.server.time();
    for _ in 0..10 {
        let _ = step_walk(&mut w, 0.1, true, &crate::vid::mode_vid(320, 200)); // menu up
    }
    assert_eq!(w.server.time(), t0, "sv.time must not advance while key_dest != key_game");
}

/// CENSUS F3 (HIGH). e1m8 is low gravity: QC `worldspawn` does
/// `cvar_set("sv_gravity", "100")` on maps/e1m8.bsp, and SV_AddGravity uses the
/// cvar (`velocity[2] -= ent_gravity * sv_gravity.value * host_frametime`). The
/// port's `bi_cvar_set` dropped it and physics used a constant 800. The client's
/// `R_DrawParticles` reads the same cvar (`grav = frametime * sv_gravity * 0.05`),
/// and every other map's worldspawn sets it back to 800.
#[test]
fn census_e1m8_has_low_gravity() {
    let mut w = build_walk_map("maps/e1m8.bsp").expect("e1m8 boots");
    for _ in 0..3 {
        step(&mut w, 0.1);
    }
    let p = w.player;
    let o = w.server.vm.ent_get_vector(p, "origin");
    // Lift the player into open air above the spawn and let one frame run.
    set_origin(&mut w, p, [o[0], o[1], o[2] + 64.0]);
    w.server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
    let flags = w.server.vm.ent_get_float(p, "flags") as i32;
    w.server.vm.ent_set_float(p, "flags", (flags & !512) as f32); // clear FL_ONGROUND
    step(&mut w, 0.1);
    let vz = w.server.vm.ent_get_vector(p, "velocity")[2];
    assert!((vz + 10.0).abs() < 1.0, "one 0.1 s frame of sv_gravity 100 gives vz -10, got {vz}");

    // The particles fall at sv_gravity too: a teleport splash is pt_slowgrav
    // (vel[2] -= grav), so one 0.1 s frame takes 0.1 * 100 * 0.05 = 0.5 off.
    w.particles = quake_rs::particles::ParticleSystem::new();
    let o = w.server.vm.ent_get_vector(p, "origin");
    let now = w.clock;
    w.particles.spawn_teleport_splash(o, now, &mut w.prng);
    let before: Vec<f32> = w.particles.particles().iter().map(|q| q.velocity[2]).collect();
    step(&mut w, 0.1);
    let after = &w.particles.particles()[..before.len()];
    for (b, a) in before.iter().zip(after) {
        assert!((b - a.velocity[2] - 0.5).abs() < 1e-3, "particle vz {b} -> {}", a.velocity[2]);
    }

    // The next map's worldspawn does cvar_set("sv_gravity", "800").
    let w = build_walk_map("maps/e1m5.bsp").expect("e1m5 boots");
    assert_eq!(w.server.sv_gravity(), 800.0);
}

/// CENSUS F5 (MED). External brush-model items (`maps/b_*.bsp`) get the
/// Mod_LoadSubmodels pixel spread ONCE: b_explob.bsp's raw (1,1,1)-(31,31,63)
/// becomes (0,0,0)-(32,32,64). The port spread twice (Bsp::parse, then the
/// server's precache_model), so the explosive box was 34 units wide — traces
/// used hull2 instead of hull1 — and floated 2 units up. Two health boxes
/// (setsize '0 0 0' '32 32 56', not affected by the spread) vanished for a
/// second reason: PlaceItem's droptofloor started "inside" a monster whose box
/// they only touch — e1m1's 10-health box at (1224,2464) against a grunt,
/// e1m6's 25-health box at (-672,832) against an ogre — where id's box hull is
/// half-open (a point on a max face is outside), so id keeps both ("Bonus item
/// fell out of level" never prints in id's log). Positions from id's oracle
/// edict dump (sv.time 1.7): box z -207.969, the health boxes at z -303.969
/// and 0.031 (droptofloor stops DIST_EPSILON above the floor).
#[test]
fn census_bmodel_item_bounds_are_spread_once() {
    let w = build_walk().expect("e1m1 boots");
    let bx = find(&w, |w, e| class(w, e) == "misc_explobox").expect("e1m1 has an explosive box");
    assert_eq!(w.server.vm.ent_get_vector(bx, "mins"), [0.0, 0.0, 0.0]);
    assert_eq!(w.server.vm.ent_get_vector(bx, "maxs"), [32.0, 32.0, 64.0]);
    assert_eq!(
        w.server.vm.ent_get_vector(bx, "origin")[2],
        -207.96875,
        "droptofloor settles the box on the floor, as in id's game"
    );
    let health_at = |w: &Walk, x: f32, y: f32| {
        find(w, |w, e| {
            let o = w.server.vm.ent_get_vector(e, "origin");
            class(w, e) == "item_health" && (o[0] - x).abs() < 1.0 && (o[1] - y).abs() < 1.0
        })
        .map(|e| w.server.vm.ent_get_vector(e, "origin")[2])
    };
    assert_eq!(health_at(&w, 1224.0, 2464.0), Some(-303.96875), "e1m1's 10-health box beside the grunt");
    let w = build_walk_map("maps/e1m6.bsp").expect("e1m6 boots");
    assert_eq!(health_at(&w, -672.0, 832.0), Some(0.03125), "e1m6's 25-health box beside the ogre");
}

/// CENSUS F8 (MED). The level start relinks every entity with touches: the
/// player's PutClientInServer -> spawn_tdeath sets `force_retouch = 2`, and
/// SV_Physics does `if (pr_global_struct->force_retouch) SV_LinkEdict (ent,
/// true)` for every edict for two frames. An ogre standing in e1m6's door *31
/// (and *76) trigger field therefore opens it at once, and one in e1m8's *6;
/// id's oracle has them fully open by sv.time 4.7 (*31 x -56, *76 x 56, *6
/// y -72). The port had no force_retouch; the doors stayed shut.
#[test]
fn census_force_retouch_opens_e1m6_start_door() {
    let door_at = |map: &str, model: &str| {
        let mut w = build_walk_map(map).expect("map boots");
        while w.server.time() < 4.7 {
            step(&mut w, 0.1);
        }
        let door = find(&w, |w, e| w.server.vm.ent_get_string(e, "model") == model).expect("the door");
        let o = w.server.vm.ent_get_vector(door, "origin");
        o.map(|v| (v * 100.0).round() / 100.0 + 0.0) // movedir float noise; -0 -> 0
    };
    assert_eq!(door_at("maps/e1m6.bsp", "*31"), [-56.0, 0.0, 0.0], "e1m6 door *31 open");
    assert_eq!(door_at("maps/e1m6.bsp", "*76"), [56.0, 0.0, 0.0], "e1m6 door *76 open");
    assert_eq!(door_at("maps/e1m8.bsp", "*6"), [0.0, -72.0, 0.0], "e1m8 door *6 open");
}

/// CENSUS F6 (MED). Every pickup flashes the screen gold: the QC item touch
/// functions `stuffcmd(other, "bf\n")`, V_BonusFlash_f sets the bonus cshift to
/// (215,186,69) at 50%, and V_UpdatePalette folds it into the palette shift.
/// The port's `stuffcmd` is a no-op.
#[test]
fn census_pickup_flashes_the_screen_gold() {
    let mut w = build_walk().expect("e1m1 boots");
    for _ in 0..5 {
        step(&mut w, 0.1);
    }
    let item = find(&w, |w, e| class(w, e) == "item_armor1").expect("e1m1 has green armour");
    let p = w.player;
    let c = centre(&w, item);
    set_origin(&mut w, p, c);
    let cshifts = step(&mut w, 0.05);
    assert!(
        cshifts.iter().any(|&(color, percent)| color == [215, 186, 69] && percent > 0.0),
        "a gold (215,186,69) bonus shift is on after the pickup; got {cshifts:?}"
    );
}

/// CENSUS F7 (MED). The player's `netname` is "player": Host_Spawn_f does
/// `ent->v.netname = host_client->name` (cl_name defaults to "player"), and the
/// QC prints it in "player entered the game" and every obituary ("player was
/// shot by a Grunt"). The port never set it. Host_Spawn_f also sets `team =
/// (cl_color & 15) + 1` and `colormap = NUM_FOR_EDICT(ent)`.
#[test]
fn census_player_netname_is_player() {
    let w = build_walk().expect("e1m1 boots");
    assert_eq!(w.server.vm.ent_get_string(w.player, "netname"), "player");
    assert_eq!(w.server.vm.ent_get_float(w.player, "team"), 1.0);
    assert_eq!(w.server.vm.ent_get_float(w.player, "colormap"), w.player as f32);
}

/// CENSUS F4 (HIGH). A weapon key pressed while the weapon is cooling down is
/// honoured when the cooldown ends: SV_ReadClientMove only ever SETS
/// `v.impulse` (`if (i) host_client->edict->v.impulse = i;`), QC W_WeaponFrame
/// returns early `if (time < self.attack_finished)`, and ImpulseCommands clears
/// the impulse only once it runs. The port clears `impulse` after every
/// PlayerPostThink (`physics_client`), so the switch is silently dropped —
/// e.g. switching away from the rocket launcher (or while holding fire with
/// the nailgun/thunderbolt).
#[test]
fn census_weapon_switch_survives_the_cooldown() {
    let mut w = build_walk().expect("e1m1 boots");
    for _ in 0..5 {
        step(&mut w, 0.05);
    }
    w.next_impulse = 9; // all weapons + ammo
    step(&mut w, 0.05);
    w.next_impulse = 7; // rocket launcher
    for _ in 0..4 {
        step(&mut w, 0.05);
    }
    assert_eq!(w.server.vm.ent_get_float(w.player, "weapon"), 32.0, "IT_ROCKET_LAUNCHER selected");
    w.in_attack = true;
    step(&mut w, 0.05); // fire: attack_finished = time + 0.8
    w.in_attack = false;
    step(&mut w, 0.05);
    let p = w.player;
    assert!(
        w.server.time() < w.server.vm.ent_get_float(p, "attack_finished"),
        "the launcher is cooling down"
    );
    w.next_impulse = 2; // shotgun, pressed during the cooldown
    step(&mut w, 0.05);
    for _ in 0..20 {
        step(&mut w, 0.05); // 1 s: the cooldown ends
    }
    assert_eq!(
        w.server.vm.ent_get_float(p, "weapon"),
        1.0,
        "the shotgun (IT_SHOTGUN) is selected once the cooldown ends"
    );
}

/// CENSUS F10 (MED). The runes show on the status bar: SV_WriteClientdataToMessage
/// sends `items = ent->v.items | (serverflags << 28)` and Sbar_DrawInventory
/// draws sigil i when `cl.items & (1<<(28+i))`. QC `sigil_touch` only ORs
/// `serverflags`. The live HUD is built from the bare `items` field.
#[test]
fn census_rune_icons_reach_the_status_bar() {
    let mut w = build_walk_map("maps/e1m7.bsp").expect("e1m7 boots");
    for _ in 0..3 {
        step(&mut w, 0.1);
    }
    let rune = find(&w, |w, e| class(w, e) == "item_sigil").expect("e1m7 has the rune");
    // Render the status bar before and after the pickup; the rune slot
    // (Sbar_DrawInventory: x = 320-32+i*8, y = -8 above the sbar, i.e. the
    // inventory strip's right end) changes when the icon is drawn.
    let before = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(320, 200)).0;
    let p = w.player;
    let c = centre(&w, rune);
    set_origin(&mut w, p, c);
    step(&mut w, 0.1);
    assert_eq!(w.server.serverflags() as i32 & 1, 1, "sigil_touch set serverflags bit 0");
    let after = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(320, 200)).0;
    // Sbar_DrawInventory draws sigil i with Sbar_DrawPic (320-32 + i*8, -16):
    // x 288.., y 200-24-16 = 160.. at 320x200 (viewsize 100: sb_lines 48, the
    // inventory strip is drawn). The rune-1 cell is 8x16.
    let cell = |img: &render::Image| -> Vec<u8> {
        let mut v = Vec::new();
        for y in 160..176 {
            for x in 288..296 {
                v.push(img.pixels[y * img.w + x]);
            }
        }
        v
    };
    assert_ne!(cell(&before), cell(&after), "the rune 1 icon is drawn after the pickup");
}

/// CENSUS L5 (LOW). Host_ServerFrame runs SV_RunClients (SV_ReadClientMove +
/// SV_ClientThink: friction and acceleration) BEFORE SV_Physics, whose
/// PlayerPreThink then runs PlayerJump / WaterMove on the accelerated
/// velocity. A standing jump with forward held: SV_ClientThink still sees the
/// player on the ground and accelerates it to the full wish speed, then
/// PlayerJump adds 270 up; the other way round the jump clears FL_ONGROUND
/// first and the air move caps the gain at 30 u/s. Ground truth from id's
/// oracle (e1m1, `+forward` `+jump` from rest, 0.1 s frames; the press frame
/// is CL_KeyState's half step, forwardmove 100): the player moves 10 units
/// forward every frame and rises 19, 11, 3, then falls 5.
#[test]
fn census_client_think_runs_before_player_prethink() {
    let mut w = build_walk().expect("e1m1 boots");
    for _ in 0..10 {
        step(&mut w, 0.1); // settle on the floor, FL_JUMPRELEASED set
    }
    let p = w.player;
    assert!(w.server.vm.ent_get_float(p, "flags") as i32 & 512 != 0, "on the ground");
    let mut o = w.server.vm.ent_get_vector(p, "origin");
    assert_eq!(o, [480.0, -352.0, 88.03125], "id's resting spot");
    for (frame, dz) in [19.0, 11.0, 3.0, -5.0].into_iter().enumerate() {
        let cmd = quake_rs::server::UserCmd {
            forwardmove: if frame == 0 { 100.0 } else { 200.0 },
            yaw: 90.0,
            buttons: 2, // +jump held
            ..Default::default()
        };
        w.server.client_frame(&cmd, 0.1).expect("frame");
        let n = w.server.vm.ent_get_vector(p, "origin");
        assert!((n[1] - o[1] - 10.0).abs() < 0.01, "frame {frame}: 10 units forward, got {}", n[1] - o[1]);
        assert!((n[2] - o[2] - dz).abs() < 0.01, "frame {frame}: dz {dz}, got {}", n[2] - o[2]);
        o = n;
    }
}

/// CENSUS L12 (the pause half). default.cfg binds PAUSE to `pause`
/// (Host_Pause_f, forwarded to the server): single player stops — sv.paused,
/// so neither SV_ClientThink nor SV_Physics runs and cl.time stands, while
/// cl.paused keeps V_CalcRefdef from moving the view — the server broadcasts
/// "player paused the game", and SCR_DrawPause puts gfx/pause.lmp at
/// ((w - 128)/2, (h - 48 - 24)/2). PAUSE is no console key, so it works
/// with the console down; the menu does not bind it. Pressed again, it all
/// resumes ("player unpaused the game").
#[test]
fn census_pause_stops_the_game_and_shows_the_plaque() {
    use crate::app::{boot, APP};
    use crate::common::pak;
    use crate::console::console_toggle;
    use crate::host::step as host_step;
    use crate::input::{key_down, key_up};
    assert_eq!(boot(), 1);
    crate::test_util::close_menu();
    crate::vid::set_resolution(320, 200);
    for _ in 0..10 {
        host_step(0.05);
    }
    let state = || {
        APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            (w.server.time(), w.server.vm.ent_get_vector(w.player, "origin"), w.clock, w.server.paused)
        })
    };
    let fb = || APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
    let pause_key = || {
        key_down(255); // K_PAUSE
        key_up(255);
    };
    pause_key();
    host_step(0.05);
    let s0 = state();
    assert!(s0.3, "PAUSE: sv.paused");
    // +forward held for a second: nothing moves and no clock runs.
    key_down(i32::from(quake_rs::keys::K_UPARROW));
    for _ in 0..20 {
        host_step(0.05);
    }
    assert_eq!(state(), s0, "the world stands still");
    // The plaque, texel for texel, at id's place on a 320x200 screen.
    let lmp = pak().unwrap().read_file("gfx/pause.lmp").unwrap().unwrap();
    let pic = quake_rs::wad::Qpic::parse(&lmp).unwrap();
    let pal = render::parse_palette(&pak().unwrap().read_file("gfx/palette.lmp").unwrap().unwrap()).unwrap();
    assert_eq!((pic.width, pic.height), (128, 24));
    let shot = fb();
    for y in 0..24 {
        for x in 0..128 {
            let i = ((64 + y) * 320 + 96 + x) * 4;
            let want = pal[pic.data[y * 128 + x] as usize];
            assert_eq!(shot[i..i + 3], want, "plaque texel ({x},{y})");
        }
    }
    // Once the notify line has gone (con_notifytime 3 s of realtime), the
    // paused frames are identical: sky, liquids, lights, particles all stand.
    for _ in 0..70 {
        host_step(0.05);
    }
    let a = fb();
    host_step(0.05);
    assert!(a == fb(), "paused frames are the same frame");
    // The menu does not bind PAUSE; the console does not take it.
    crate::menu::menu_cancel(); // open the menu
    pause_key();
    assert!(state().3, "PAUSE in the menu does nothing");
    crate::menu::menu_cancel(); // close it
    console_toggle();
    pause_key();
    assert!(!state().3, "PAUSE with the console down: unpaused");
    console_toggle();
    for _ in 0..10 {
        host_step(0.05);
    }
    let s1 = state();
    assert!(s1.0 > s0.0 && s1.2 > s0.2, "the clocks run again");
    assert!(s1.1 != s0.1, "+forward moves the player again");
    key_up(i32::from(quake_rs::keys::K_UPARROW));
    let text: Vec<String> =
        APP.with(|c| c.borrow().as_ref().unwrap().console.lines().map(str::to_string).collect());
    let said: Vec<&String> = text.iter().filter(|l| l.contains("the game")).collect();
    assert_eq!(said, ["player paused the game", "player unpaused the game"], "SV_BroadcastPrintf");
}
