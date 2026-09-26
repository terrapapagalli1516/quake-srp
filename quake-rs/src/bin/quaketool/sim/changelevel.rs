//! `quaketool changelevel <pak> <map.bsp>` — a level's exit taken end to
//! end, and the player's inventory carried into the next map.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::pak::Pak;
use quake_rs::progs::Progs;
use quake_rs::server::{Server, UserCmd};

use crate::entities::{player_start, trigger_map_key};
use crate::{CmdResult, Out};

/// `changelevel <pak> <map.bsp>`: boot the map, find its `trigger_changelevel`
/// exit, drive the player onto it until the QuakeC `changelevel()` builtin fires
/// (recording the next map), then perform the engine-side swap — save the
/// player's spawn parms, load the next BSP + a fresh progs, spawn the next
/// level, and reconnect the client carrying its inventory. Reports the next map,
/// the entities spawned there, and the player's weapon/items/health/armor BEFORE
/// vs AFTER the swap to prove the inventory carried across.
pub fn cmd_changelevel(pak_path: &str, map_name: &str) -> CmdResult {
    let pak = Pak::open(pak_path)?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_render = Bsp::parse(&read(map_name)?)?;
    let bsp_sim = Bsp::parse(&read(map_name)?)?;
    let progs = Progs::parse(&read("progs.dat")?)?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone()))?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities()?;
    let player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "changelevel {map_name}: {} entities spawned; player = edict {player}",
        rep.spawned
    );

    // A small snapshot of the player's persistent state for the BEFORE/AFTER
    // comparison: weapon, items bitfield, health, armor, and the three ammo
    // counters DecodeLevelParms restores.
    let snapshot = |s: &Server| -> [i64; 7] {
        let g = |f: &str| s.vm.ent_get_float(player.max(0), f) as i64;
        [
            g("weapon"),
            g("items"),
            g("health"),
            g("armorvalue"),
            g("ammo_shells"),
            g("ammo_nails"),
            g("ammo_rockets"),
        ]
    };
    let fmt = |v: &[i64; 7]| {
        format!(
            "weapon={} items={:#x} health={} armor={} shells={} nails={} rockets={}",
            v[0], v[1], v[2], v[3], v[4], v[5], v[6]
        )
    };

    // Locate the trigger_changelevel exit. The brush trigger has no `origin`; it
    // occupies a brush volume, so read the absmin/absmax setmodel + link gave it
    // and aim for the box centre. Also pull its `map` key (the expected next map).
    let mut trigger: Option<(i32, [f32; 3])> = None;
    for e in 0..server.vm.num_edicts() {
        let ent = e as i32;
        if server.vm.is_free_edict(e as i32) {
            continue;
        }
        if server.vm.ent_get_string(ent, "classname") == "trigger_changelevel" {
            let amin = server.vm.ent_get_vector(ent, "absmin");
            let amax = server.vm.ent_get_vector(ent, "absmax");
            let centre = [
                0.5 * (amin[0] + amax[0]),
                0.5 * (amin[1] + amax[1]),
                0.5 * (amin[2] + amax[2]),
            ];
            trigger = Some((ent, centre));
            break;
        }
    }
    let Some((trig, centre)) = trigger else {
        return Err(format!("{map_name} has no trigger_changelevel").into());
    };
    let trig_map = trigger_map_key(&bsp_render.entities);
    let _ = writeln!(
        o,
        "  found trigger_changelevel (edict {trig}) targeting map \"{}\"; volume centre {centre:?}",
        trig_map.as_deref().unwrap_or("?")
    );

    // Drive the player into the trigger volume: teleport onto the centre, then
    // tick frames (the touch fires during SV_Physics_Client) until the deferred
    // changelevel() request appears. A small jiggle of forward motion settles the
    // box so its absmin/absmax overlaps the trigger and SV_TouchLinks fires.
    // Drive the player into the trigger volume: hold it on the centre for a few
    // frames so SV_TouchLinks fires the trigger's `changelevel_touch`. In stock
    // single-player that touch sets the `nextmap` global and starts the
    // end-of-level intermission, whose exit (a player button press in-game) runs
    // `GotoNextMap` -> `changelevel(nextmap)`. We drive the touch by ticking
    // frames, then — if the deferred builtin has not fired yet — run the canonical
    // `GotoNextMap` exit function (self = player) to issue the changelevel, the
    // same call the intermission exit makes. Either way the deferred request
    // appears for `take_pending_changelevel`.
    let spawn_yaw = player_start(&bsp_render.entities).map(|(_, a)| a).unwrap_or(0.0);
    let cmd = UserCmd { yaw: spawn_yaw, ..Default::default() };
    let mut requested: Option<String> = None;
    let mut frames_driven = 0u32;
    for f in 0..40 {
        frames_driven = f + 1;
        // Pin the player in the volume so its box overlaps the trigger and the
        // touch fires (SV_TouchLinks runs during SV_Physics_Client).
        server.vm.ent_set_vector(player, "origin", centre);
        server.vm.ent_set_vector(player, "velocity", [0.0, 0.0, 0.0]);
        server.client_frame_f64(&cmd, 0.1).map_err(|e| format!("client_frame: {e}"))?;
        if let Some(m) = server.take_pending_changelevel() {
            requested = Some(m);
            break;
        }
        // Once the touch has set `nextmap` (the intermission is now armed), run the
        // intermission-exit function the same way a player button press would,
        // which calls the changelevel() builtin.
        if requested.is_none() && server.vm.gget_int("nextmap") != 0 {
            if let Some(goto) = server.vm.progs().find_function("GotoNextMap") {
                server.vm.gset_int("self", player);
                server.vm.gset_int("other", 0);
                if server.vm.execute(goto).is_err() {
                    server.vm.reset_execution();
                }
            }
            if let Some(m) = server.take_pending_changelevel() {
                requested = Some(m);
                break;
            }
        }
    }
    let Some(next_map_name) = requested else {
        return Err(format!(
            "player never triggered changelevel() (no request after {frames_driven} frames; nextmap global = {})",
            server.vm.gget_int("nextmap")
        )
        .into());
    };
    let _ = writeln!(o, "  drove player into the exit ({frames_driven} frames)");
    // QuakeC stores the bare map name ("e1m2"); the BSP lives at "maps/<name>.bsp".
    let _ = writeln!(o, "  changelevel() fired -> next map \"{next_map_name}\"");

    // Snapshot the inventory BEFORE the swap, then save the spawn parms (this runs
    // the QuakeC SetChangeParms, marshalling the player's state into parm1..16).
    let before = snapshot(&server);
    let parms = server.save_spawn_parms()?;
    let _ = writeln!(o, "  BEFORE swap: {}", fmt(&before));
    let _ = writeln!(
        o,
        "  saved spawn parms: {}",
        parms.iter().map(|p| format!("{p:.0}")).collect::<Vec<_>>().join(",")
    );

    // Build the next server from the next BSP + a fresh progs, spawn its entities,
    // and reconnect the client carrying the saved parms (DecodeLevelParms restores
    // the inventory inside PutClientInServer).
    let next_bsp_path = format!("maps/{next_map_name}.bsp");
    let next_bytes = read(&next_bsp_path)
        .map_err(|e| format!("loading next map {next_bsp_path}: {e}"))?;
    let next_bsp = Bsp::parse(&next_bytes)?;
    let next_progs = Progs::parse(&read("progs.dat")?)?;
    // Carry the chosen difficulty across (a new server starts at skill 1) and
    // the session's random streams, as the client's changelevel does.
    let carry_skill = server.skill();
    let mut next_server = Server::with_pak(next_bsp, next_progs, Some(pak.clone()))?;
    next_server.set_rand(std::rc::Rc::clone(server.rand()));
    next_server.set_map_name(&next_map_name); // SV_SpawnServer for the swapped-to level
    next_server.set_skill(carry_skill as f32);
    let next_rep = next_server.spawn_entities()?;
    let next_player = next_server
        .connect_client_with_parms(parms)
        .map_err(|e| format!("connect_client_with_parms: {e}"))?;
    let _ = writeln!(
        o,
        "  swapped to {next_bsp_path}: {} entities spawned; player = edict {next_player}",
        next_rep.spawned
    );

    let after_player = next_player;
    let after = {
        let g = |f: &str| next_server.vm.ent_get_float(after_player, f) as i64;
        [
            g("weapon"),
            g("items"),
            g("health"),
            g("armorvalue"),
            g("ammo_shells"),
            g("ammo_nails"),
            g("ammo_rockets"),
        ]
    };
    let _ = writeln!(o, "  AFTER swap:  {}", fmt(&after));
    let carried = after[0] == before[0]
        && after[1] == before[1]
        && after[4] == before[4]
        && after[5] == before[5]
        && after[6] == before[6];
    let _ = writeln!(
        o,
        "  inventory carried: {}  (weapon/items/ammo match across the swap)",
        if carried { "YES" } else { "NO" }
    );
    Ok(Out::Text(o))
}

