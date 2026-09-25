//! Census tests (CENSUS.md): each test asserts what id's WinQuake does, on the
//! real shareware data, through the live browser path (`build_walk_map` +
//! `step_walk`). They are `#[ignore]`d because the port does not do it yet —
//! each is the evidence for one CENSUS.md finding and the acceptance test for
//! its fix: `cargo test --release census -- --ignored` lists what is still open.
//! Un-ignore a test in the commit that fixes its finding.

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
    vm.argc = 2;
    let f = vm.builtins[2];
    f(vm).expect("setorigin");
}

fn live(w: &Walk, e: i32) -> bool {
    !w.server.vm.edict_free.get(e as usize).copied().unwrap_or(true)
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

fn step(w: &mut Walk, dt: f32) -> ([u8; 3], f32) {
    let (_, c, a) = step_walk(w, dt, false, 320, 200);
    (c, a)
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
        let _ = step_walk(&mut w, 0.1, true, 320, 200); // menu up
    }
    assert_eq!(w.server.time(), t0, "sv.time must not advance while key_dest != key_game");
}

/// CENSUS F3 (HIGH). e1m8 is low gravity: QC `worldspawn` does
/// `cvar_set("sv_gravity", "100")` on maps/e1m8.bsp, and SV_AddGravity uses the
/// cvar (`velocity[2] -= ent_gravity * sv_gravity.value * host_frametime`). The
/// port's `bi_cvar_set` drops it and physics uses a constant 800.
#[test]
#[ignore = "census F3: e1m8's sv_gravity 100 is ignored"]
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
}

/// CENSUS F5 (MED). External brush-model items (`maps/b_*.bsp`) get the
/// Mod_LoadSubmodels pixel spread ONCE: b_explob.bsp's raw (1,1,1)-(31,31,63)
/// becomes (0,0,0)-(32,32,64). The port spreads twice (Bsp::parse, then the
/// server's precache_model), so the boxes are 34 units wide — traces use hull2
/// instead of hull1 — and droptofloor fails near walls/monsters: e1m1 loses the
/// 10-health box at (1224,2464,-304) ("Bonus item fell out of level"; id's
/// oracle keeps it), the e1m1 explosive box floats 2 units up.
#[test]
#[ignore = "census F5: b_*.bsp item bounds are pixel-spread twice"]
fn census_bmodel_item_bounds_are_spread_once() {
    let w = build_walk().expect("e1m1 boots");
    let bx = find(&w, |w, e| class(w, e) == "misc_explobox").expect("e1m1 has an explosive box");
    assert_eq!(w.server.vm.ent_get_vector(bx, "mins"), [0.0, 0.0, 0.0]);
    assert_eq!(w.server.vm.ent_get_vector(bx, "maxs"), [32.0, 32.0, 64.0]);
    assert_eq!(
        w.server.vm.ent_get_vector(bx, "origin")[2],
        -208.0,
        "droptofloor settles the box on the floor, as in id's game"
    );
    let health = find(&w, |w, e| {
        let o = w.server.vm.ent_get_vector(e, "origin");
        class(w, e) == "item_health" && (o[0] - 1224.0).abs() < 1.0 && (o[1] - 2464.0).abs() < 1.0
    });
    assert!(health.is_some(), "the 10-health box at (1224, 2464) survives PlaceItem's droptofloor");
}

/// CENSUS F8 (MED). The level start relinks every entity with touches: the
/// player's PutClientInServer -> spawn_tdeath sets `force_retouch = 2`, and
/// SV_Physics does `if (pr_global_struct->force_retouch) SV_LinkEdict (ent,
/// true)` for every edict for two frames. An ogre standing in e1m6's door *31
/// trigger field therefore opens it at once (id's oracle: the door has moved 30
/// units by sv.time 1.7). The port has no force_retouch; the door stays shut.
#[test]
#[ignore = "census F8: force_retouch is not modelled (level-start doors stay shut)"]
fn census_force_retouch_opens_e1m6_start_door() {
    let mut w = build_walk_map("maps/e1m6.bsp").expect("e1m6 boots");
    while w.server.time() < 1.7 {
        step(&mut w, 0.1);
    }
    let door = find(&w, |w, e| w.server.vm.ent_get_string(e, "model") == "*31").expect("door *31");
    let o = w.server.vm.ent_get_vector(door, "origin");
    assert!(o[0].abs() > 1.0, "door *31 is opening by t=1.7 (id: x=-30), port origin {o:?}");
}

/// CENSUS F6 (MED). Every pickup flashes the screen gold: the QC item touch
/// functions `stuffcmd(other, "bf\n")`, V_BonusFlash_f sets the bonus cshift to
/// (215,186,69) at 50%, and V_CalcBlend folds it into the palette shift. The
/// port's `stuffcmd` is a no-op.
#[test]
#[ignore = "census F6: the 'bf' bonus flash never fires"]
fn census_pickup_flashes_the_screen_gold() {
    let mut w = build_walk().expect("e1m1 boots");
    for _ in 0..5 {
        step(&mut w, 0.1);
    }
    let item = find(&w, |w, e| class(w, e) == "item_armor1").expect("e1m1 has green armour");
    let p = w.player;
    let c = centre(&w, item);
    set_origin(&mut w, p, c);
    let (color, alpha) = step(&mut w, 0.05);
    assert!(
        alpha > 0.0 && color[0] > color[2],
        "a gold (215,186,69) bonus shift is in the blend after the pickup; got {color:?} @ {alpha}"
    );
}

/// CENSUS F7 (MED). The player's `netname` is "player": Host_Spawn_f does
/// `ent->v.netname = host_client->name` (cl_name defaults to "player"), and the
/// QC prints it in "player entered the game" and every obituary ("player was
/// shot by a Grunt"). The port never sets it.
#[test]
#[ignore = "census F7: the player's netname is never set (obituaries lose their subject)"]
fn census_player_netname_is_player() {
    let w = build_walk().expect("e1m1 boots");
    assert_eq!(w.server.vm.ent_get_string(w.player, "netname"), "player");
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
#[ignore = "census F4: impulses pressed during a weapon cooldown are dropped"]
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
#[ignore = "census F10: rune (sigil) icons never reach the live status bar"]
fn census_rune_icons_reach_the_status_bar() {
    let mut w = build_walk_map("maps/e1m7.bsp").expect("e1m7 boots");
    for _ in 0..3 {
        step(&mut w, 0.1);
    }
    let rune = find(&w, |w, e| class(w, e) == "item_sigil").expect("e1m7 has the rune");
    // Render the status bar before and after the pickup; the rune slot
    // (Sbar_DrawInventory: x = 320-32+i*8, y = -8 above the sbar, i.e. the
    // inventory strip's right end) changes when the icon is drawn.
    let before = step_walk(&mut w, 0.0, false, 320, 200).0;
    let p = w.player;
    let c = centre(&w, rune);
    set_origin(&mut w, p, c);
    step(&mut w, 0.1);
    assert_eq!(w.server.serverflags() as i32 & 1, 1, "sigil_touch set serverflags bit 0");
    let after = step_walk(&mut w, 0.0, false, 320, 200).0;
    // Sbar_DrawInventory draws sigil i with Sbar_DrawPic (320-32 + i*8, -16):
    // x 288.., y 200-24-16 = 160.. at 320x200 (viewsize 100: sb_lines 48, the
    // inventory strip is drawn). The rune-1 cell is 8x16.
    let cell = |img: &render::Image| -> Vec<[u8; 3]> {
        let mut v = Vec::new();
        for y in 160..176 {
            for x in 288..296 {
                v.push(img.rgb[y * img.w + x]);
            }
        }
        v
    };
    assert_ne!(cell(&before), cell(&after), "the rune 1 icon is drawn after the pickup");
}
