//! host_cmd.c's client-state commands: the level loads — `Host_Map_f`
//! (`SV_SpawnServer` + the client's connect), `Host_Changelevel_f`,
//! `Host_Restart_f` and `Host_Loadgame_f`'s rebuild of the world from a
//! savegame — and the cheats that act on the player edict (god, noclip, fly,
//! kill, give, impulse). Each load makes its sound calls ([`SoundCall`]) into
//! the caller's `Vec`. (Parsing a console line, `Cmd_ExecuteString`, is the
//! host's.)
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/host_cmd.c`.

use std::rc::Rc;

use crate::bsp::Bsp;
use crate::cd_audio::CdCall;
use crate::dlight::DynamicLights;
use crate::pak::Pak;
use crate::particles::ParticleSystem;
use crate::progs::Progs;
use crate::qrand::QRand;
use crate::server::{EntFlags, MoveType, Server};
use crate::vm::Fld;

use super::cl_input::clamp_pitch;
use super::cl_main::client_items;
use super::host::host_error;
use super::{SoundCall, Walk, assemble_walk, spawn_view_angles};
use crate::QError;

/// Sane upper bounds the `give` command clamps to, mirroring Quake's pickup
/// caps (the player can't carry more than these).
pub const MAX_HEALTH: f32 = 250.0;
pub const MAX_ARMOR: f32 = 200.0;
pub const MAX_SHELLS: f32 = 100.0;
pub const MAX_NAILS: f32 = 200.0;
pub const MAX_ROCKETS: f32 = 100.0;
pub const MAX_CELLS: f32 = 100.0;

// QuakeC `items` weapon bits (quakedef.h): the weapon for digit `d` (1..8) is
// IT_SHOTGUN << (d-1), and IT_AXE is a separate high bit.
pub const IT_SHOTGUN: i32 = 1; // bit for weapon 2 base; weapon d>=2 is IT_SHOTGUN<<(d-2)
pub const IT_AXE: i32 = 4096;
pub const IT_INVISIBILITY: i32 = 1 << 19; // Ring of Shadows (524288)

/// Run a command that acts on the live player edict, pushing any output lines
/// into `out`. `cmd` is already lowercased; `argv[0]` is the command itself.
/// Unknown verbs push nothing (the caller reports them). A `kill` that
/// restarts the level makes its sound calls into `sound`.
pub fn run_game_command(w: &mut Walk, cmd: &str, argv: &[&str], out: &mut Vec<String>, sound: &mut Vec<SoundCall>) {
    let player = w.player;
    match cmd {
        // Host_God_f: flags ^= FL_GODMODE.
        "god" => {
            let flags = w.server.vm.flags(player).toggled(EntFlags::GODMODE);
            w.server.vm.set_flags(player, flags);
            out.push(if flags.contains(EntFlags::GODMODE) { "godmode ON".into() } else { "godmode OFF".into() });
        }
        // Host_Noclip_f: movetype toggles WALK <-> NOCLIP.
        "noclip" => {
            if w.server.vm.movetype(player) != MoveType::NoClip {
                w.server.vm.set_movetype(player, MoveType::NoClip);
                out.push("noclip ON".into());
            } else {
                w.server.vm.set_movetype(player, MoveType::Walk);
                out.push("noclip OFF".into());
            }
        }
        // Host_Fly_f: movetype toggles WALK <-> FLY.
        "fly" => {
            if w.server.vm.movetype(player) != MoveType::Fly {
                w.server.vm.set_movetype(player, MoveType::Fly);
                out.push("flymode ON".into());
            } else {
                w.server.vm.set_movetype(player, MoveType::Walk);
                out.push("flymode OFF".into());
            }
        }
        // Host_Kill_f: suicide through the QuakeC `ClientKill` entry point (the
        // REAL chain: suicide frame, frag penalty, respawn() — which in single
        // player issues localcmd("restart\n")). NOT a health hack: the QuakeC
        // owns the death. The pending restart is honoured HERE, not left for
        // walk_frame, because client_frame clears stale requests at the top of
        // each frame — and it matches the C, where the queued "restart" Cbuf
        // text executes right after the kill command itself.
        "kill" => match w.server.client_kill() {
            Ok(true) => {
                if w.server.take_pending_restart() {
                    try_restart(w, sound);
                }
                // No success line of its own: the QuakeC bprints
                // "<netname> suicides" (drained into notify next frame).
            }
            Ok(false) => out.push("Can't suicide -- allready dead!".into()),
            Err(e) => host_error(w, &e, sound), // ClientKill failed: Host_Error
        },
        // Queue a one-shot impulse (impulse 9 = the QuakeC give-all cheat).
        "impulse" => {
            let n = argv.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            w.next_impulse = n;
            out.push(format!("impulse {n}"));
        }
        // Host_Give_f (faithful-ish): a letter/digit selects what to give.
        "give" => run_give_command(w, argv, out),
        _ => {}
    }
}

/// Port of `Host_Give_f` for the single-player items we support: a letter
/// argument grants ammo/health/armour; a digit 1..8 grants that weapon (setting
/// the `items` bit and selecting it). `n` (`argv[2]`) is the amount; ammo/health
/// default to a full amount when omitted. All values clamp to sane caps.
fn run_give_command(w: &mut Walk, argv: &[&str], out: &mut Vec<String>) {
    let what = match argv.get(1) {
        Some(s) if !s.is_empty() => *s,
        _ => {
            out.push("give what? (h a s n r c 1-8)".into());
            return;
        }
    };
    let c0 = what.as_bytes()[0] as char;
    let player = w.player;
    // The amount (argv[2]); None => use the per-field default below.
    let amount = argv.get(2).and_then(|s| s.parse::<f32>().ok());

    let fo = *w.server.vm.fo();
    let mut set = |field: Fld, val: f32, cap: f32, label: &str, out: &mut Vec<String>| {
        let v = val.clamp(0.0, cap);
        w.server.vm.set_ent_float(player, field, v);
        out.push(format!("gave {label} {v:.0}"));
    };

    match c0 {
        'h' => set(fo.health, amount.unwrap_or(MAX_HEALTH), MAX_HEALTH, "health", out),
        'a' => set(fo.armorvalue, amount.unwrap_or(MAX_ARMOR), MAX_ARMOR, "armor", out),
        's' => set(fo.ammo_shells, amount.unwrap_or(MAX_SHELLS), MAX_SHELLS, "shells", out),
        'n' => set(fo.ammo_nails, amount.unwrap_or(MAX_NAILS), MAX_NAILS, "nails", out),
        'r' => set(fo.ammo_rockets, amount.unwrap_or(MAX_ROCKETS), MAX_ROCKETS, "rockets", out),
        'c' => set(fo.ammo_cells, amount.unwrap_or(MAX_CELLS), MAX_CELLS, "cells", out),
        '1'..='8' => {
            // Weapon select/grant. Weapon 1 = axe (its own high bit); weapons
            // 2..8 are IT_SHOTGUN << (d-2), exactly as Host_Give_f does
            // (sv_player->v.items |= IT_SHOTGUN << (t[0]-'2')).
            let d = (c0 as u8 - b'0') as i32; // 1..8
            let bit = if d == 1 { IT_AXE } else { IT_SHOTGUN << (d - 2) };
            let items = w.server.vm.ent_float(player, w.server.vm.fo().items) as i32 | bit;
            w.server.vm.set_ent_float(player, w.server.vm.fo().items, items as f32);
            // Select it: the QuakeC `weapon` field is the active weapon bit.
            w.server.vm.set_ent_float(player, w.server.vm.fo().weapon, bit as f32);
            out.push(format!("gave weapon {d}"));
        }
        _ => out.push(format!("give: unknown item '{what}'")),
    }
}

/// Build a live walk on `map` (a `maps/*.bsp` path in `pak`) — `Host_Map_f`'s
/// `SV_SpawnServer` and the client's connect: the browser boots e1m1; New Game
/// uses [`crate::render::NEW_GAME_MAP`] (the `start` hub). Its server draws
/// from the host session's `rand` ([`Server::set_rand`]), and so do the level
/// changes it makes. The level's sounds start through `sound`
/// ([`SoundCall::StopAll`], then its placed loops). `max_edicts` is the live
/// `sv_max_edicts` cvar (pass [`crate::vm::MAX_EDICTS`] for Classic-equivalent
/// behaviour); it is set on the new server before [`Server::spawn_entities`]
/// runs, exactly where `SV_SpawnServer` would size `sv.edicts`.
pub fn build_walk_map(
    pak: Pak,
    map: &str,
    rand: &Rc<QRand>,
    sound: &mut Vec<SoundCall>,
    max_edicts: usize,
) -> Option<Walk> {
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(map)?).ok()?;
    let bsp_sim = Bsp::parse(&read(map)?).ok()?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;

    // A live server: spawn the map's entities, then connect the local player.
    // Pass the pak so external brush-model item boxes (b_*.bsp) collide + take
    // damage (the explosive box becomes shootable).
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).ok()?;
    server.set_rand(Rc::clone(rand));
    // SV_SpawnServer sizing sv.edicts: before spawn_entities, like every
    // other setting a fresh server needs applied before it (skill, gravity).
    server.set_max_edicts(max_edicts);
    // SV_SpawnServer set world.model + the mapname global before loading the
    // entities (the QuakeC episode-end finale check reads world.model).
    server.set_map_name(map);
    // A QuakeC error from here on is Host_Error; the walk does not come up
    // (the host's console gets no report yet: an open item).
    server.spawn_entities().ok()?;
    let player = server.connect_client().ok()?;
    let (yaw, pitch) = spawn_view_angles(&server, player);
    // Capture the level-entry spawn parms (the just-connected, full-state player) so
    // a single-player respawn can reload THIS level with them.
    let entry_parms = server.save_spawn_parms().ok()?;
    // The signon-sequence physics frames the C runs between PutClientInServer and
    // the first rendered frame (Host_Spawn_f/Host_Begin_f each precede an
    // SV_Physics tick before signon 4 re-enables drawing). Without them the
    // just-spawned player — placed at spot.origin + '0 0 1', ~5 units up on the
    // start map — falls to the floor ON SCREEN over the first frames: the
    // reported one-time texture/lighting "pop" (every surface resamples as the
    // eye drops). Frame 0 must render the settled WinQuake pose.
    server.run_signon_frames().ok()?;

    // The level is committed past this point (nothing below fails). Tear down
    // the previous level/mode's looping audio and register this level's placed
    // `ambientsound()` loops (torches, wind, hums) for the sound layer to start —
    // PF_ambientsound wrote these into the signon ONCE; the QuakeC registered
    // them all during spawn_entities, so one drain captures them all.
    sound.push(SoundCall::StopAll);
    let statics = server.drain_static_sounds();
    sound.push(SoundCall::Static(statics));
    // SV_SendServerinfo's svc_cdtrack: the level's CD track.
    sound.push(SoundCall::Cd(CdCall::cdtrack(server.cd_track())));
    // Drop one-shot events the spawn + settle ticks queued, like the
    // changelevel/restart paths do (in the C the client misses signon-era
    // datagram sounds while not yet `spawned`; tick 2's are technically
    // deliverable there — dropping both is a deliberate, inaudible-on-id-maps
    // simplification, kept identical across all three walk-building paths).
    let _ = server.drain_sounds();
    let _ = server.drain_particles();
    let _ = server.drain_temp_entities();
    let _ = server.drain_messages();
    let _ = server.drain_svc_events();
    let _ = server.drain_stufftext();

    assemble_walk(pak, map.to_string(), server, player, entry_parms, bsp, yaw, pitch)
}

/// Perform a deferred level transition: save the current player's spawn parms,
/// load `next_map` and a fresh `progs.dat` from the open pak, spawn the new
/// level's entities, and reconnect the client carrying its inventory. On a
/// parse failure the current level is left untouched (the guards below all
/// early-`return` rather than panic), so a missing or corrupt next map is
/// non-fatal — the player keeps playing the level they are on. A QuakeC error
/// in either level's code is id's `Host_Error`: the game ends ([`host_error`]).
pub fn try_changelevel(w: &mut Walk, next_map: &str, sound: &mut Vec<SoundCall>) {
    // Save the outgoing player's inventory into parm1..parm16 (SV_SaveSpawnparms
    // -> SetChangeParms). Done before we touch the old server's world.
    let parms = match w.server.save_spawn_parms() {
        Ok(parms) => parms,
        Err(e) => return host_error(w, &e, sound),
    };
    // Capture serverflags (the episode rune SERVERFLAG_* bits) from the OUTGOING
    // server. The C keeps these alive across SV_SpawnServer (svs.serverflags);
    // building a brand-new Server would reset the global to 0 and lose the
    // collected runes, so we carry it forward onto the new level below.
    let serverflags = w.server.serverflags();
    // Carry the chosen difficulty (and sv_gravity, which id's cvar keeps too)
    // across the level change. A new server starts with `skill` 1, so capture
    // it from the OUTGOING server now and restore it on the new one below (the
    // start hub's skill portal set it via cvar_set; without this the jump to
    // e1m1 would silently revert to Normal). Mirrors how serverflags is carried.
    let skill = w.server.skill();

    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    // The QuakeC `changelevel(map)` carries the BARE map name (e.g. "e1m1", from
    // the trigger's `map` key), but the pak stores it as "maps/e1m1.bsp". Build
    // the pak path (tolerating an already-qualified name). Without this the read
    // missed and the swap silently aborted — the start-hub episode-1 slipgate
    // (and every in-game changelevel) "did nothing".
    let map_file = if next_map.ends_with(".bsp") { next_map.to_string() } else { format!("maps/{next_map}.bsp") };
    // Two BSP copies (one for the sim/collision world the server owns, one for
    // rendering) plus a fresh progs.dat for the new server. Any failure aborts
    // the swap, leaving the live level running.
    let Some(map_bytes) = read(&map_file) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
    // The host session's random streams continue into the new level.
    ns.set_rand(Rc::clone(w.server.rand()));
    // SV_SpawnServer: world.model + the mapname global, before the entities load.
    ns.set_map_name(&map_file);
    // Restore the carried serverflags onto the new server BEFORE spawning its
    // entities, mirroring the C (SV_SpawnServer restores svs.serverflags before
    // ED_LoadFromFile), so the new level's worldspawn — which reads serverflags
    // to light up the runes the player already holds — and the reconnecting
    // client both observe the carried bits. A no-op if the progs lacks the
    // global.
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
    ns.set_sv_gravity(w.server.sv_gravity());
    // The edict ceiling (sv_max_edicts) is host-session state like skill and
    // gravity, not per-map: carry it forward rather than resetting to id's
    // 600 on every level change.
    ns.set_max_edicts(w.server.max_edicts());
    let up = (|| {
        ns.spawn_entities()?;
        // Capture the new level's placed ambient loops now (registered during
        // spawn_entities); committed to the sound layer only once the swap
        // succeeds below.
        let statics = ns.drain_static_sounds();
        let player = ns.connect_client_with_parms(parms)?;
        let (yaw, pitch) = spawn_view_angles(&ns, player);
        // The carried inventory at the start of the NEW level becomes its entry
        // parms, so a respawn on this level restores the state the player
        // arrived with.
        let entry_parms = ns.save_spawn_parms()?;
        // The C's signon physics frames (see build_walk_map): settle the
        // arriving player onto the floor before the new level's frame 0
        // renders. Any events these ticks queue are dropped by the post-swap
        // drains below (the C's client misses tick 1's datagram sounds while
        // not yet `spawned`; tick 2's are technically deliverable there —
        // dropping both is a deliberate, inaudible-on-id-maps simplification,
        // identical across all three paths).
        ns.run_signon_frames()?;
        Ok((statics, player, entry_parms, yaw, pitch))
    })();
    let (statics, player, entry_parms, yaw, pitch) = match up {
        Ok(up) => up,
        Err(e) => return load_failed(w, &e, sound),
    };

    // Commit the swap. From here nothing can fail.
    w.server = ns;
    w.bsp = render_bsp;
    w.renderer.begin_map(&w.bsp);
    w.player = player;
    w.entry_parms = entry_parms;
    w.map_name = map_file;
    w.yaw = yaw;
    w.pitch = pitch;

    // New level, clean slate: drop the old level's particles / dynamic lights /
    // beams (CL_ClearState memsets cl_beams); cl.time is the new server's.
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.glides.clear();
    w.beams.clear();
    // Clear the on-screen text overlay on level load (SCR_BeginLoadingPlaque calls
    // Con_ClearNotify + scr_centertime_off=0): drop the half-built line AND the
    // already-flushed notify lines + the centerprint (their expiry is an
    // absolute time).
    w.notify.clear();
    w.centerprint = None;
    // cl.time: the new server's sv.time (SV_SpawnServer's 1.0 + the signon
    // frames), which CL_LerpPoint snaps the client clock to.
    w.clock = w.server.time();
    // CL_ClearState zeroes cl.cshifts and cl.faceanimtime (view.c's static
    // v_dmg_* kick is not in `cl` and runs out on its own).
    w.damage_blend = 0.0;
    w.bonus_blend = 0.0;
    w.faceanimtime = 0.0;
    // CL_ClearState + the signon's clientdata: the carried items, unflashed
    // (`stamp_item_gettime`).
    w.cl_items = client_items(w);
    w.item_gettime = [0.0; 32];
    // Reset stair-step view smoothing so the new spawn doesn't glide from old Z.
    w.oldz = f32::NAN;
    // CL_ClearState: the new level starts OUT of intermission (cl.intermission=0)
    // with no stale finale text or sellscreen request.
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    w.pending_menu_credits = false;
    // Drop any events the *outgoing* server queued (the new server starts fresh).
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Looping audio: stop the OLD level's loops (S_StopAllSounds on changelevel)
    // and hand the sound layer the NEW level's placed ambient loops + a fresh ambient
    // ramp, captured above right after spawn_entities.
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(statics));
    sound.push(SoundCall::Cd(CdCall::cdtrack(w.server.cd_track())));
    let _ = w.server.drain_svc_events();
    let _ = w.server.drain_stufftext();
}

/// A level change's new server failed with `e`: a QuakeC error is id's
/// `Host_Error`, which ends the game ([`host_error`]); anything else (a map
/// whose entities do not parse) leaves the current level running, the port's
/// degrade for a bad map.
fn load_failed(w: &mut Walk, e: &QError, sound: &mut Vec<SoundCall>) {
    if matches!(e, QError::Program(_)) {
        host_error(w, e, sound);
    }
}

/// Single-player respawn: reload the CURRENT level fresh and reconnect the player
/// with the level-ENTRY spawn parms (the state they arrived with). Ported from the
/// engine running QuakeC's `localcmd("restart\n")` — a dead player who presses a
/// button restarts the map. A dead player's own state is useless (health 0, dropped
/// inventory), so `entry_parms` (captured at level entry) is what's restored,
/// matching how `restart` works in id's single-player. A read/parse failure leaves
/// the (dead) level running rather than crashing; a QuakeC error is
/// `Host_Error` ([`host_error`]).
pub fn try_restart(w: &mut Walk, sound: &mut Vec<SoundCall>) {
    // Host_Restart_f -> SV_SpawnServer with NO SV_SaveSpawnparms: the level is
    // respawned with svs.serverflags, the runes held on ENTRY — a rune taken
    // on this level before dying is lost, as in id's game.
    let serverflags = w.server.level_entry_serverflags();
    let skill = w.server.skill();
    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    let Some(map_bytes) = read(&w.map_name) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
    ns.set_rand(Rc::clone(w.server.rand()));
    // SV_SpawnServer: world.model + the mapname global, before the entities load.
    ns.set_map_name(&w.map_name);
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
    ns.set_sv_gravity(w.server.sv_gravity());
    // See try_changelevel: host-session state, carried across the reload.
    ns.set_max_edicts(w.server.max_edicts());
    let entry_parms = w.entry_parms;
    let up = (|| {
        // Static loops as in try_changelevel: spawn, then capture this (re)load's.
        ns.spawn_entities()?;
        let statics = ns.drain_static_sounds();
        let player = ns.connect_client_with_parms(entry_parms)?;
        let (yaw, pitch) = spawn_view_angles(&ns, player);
        // The C's signon physics frames (see build_walk_map): settle the
        // respawned player onto the floor before the restarted level's frame 0
        // renders.
        ns.run_signon_frames()?;
        Ok((statics, player, yaw, pitch))
    })();
    let (statics, player, yaw, pitch) = match up {
        Ok(up) => up,
        Err(e) => return load_failed(w, &e, sound),
    };

    // Commit the reload (nothing below can fail).
    w.server = ns;
    w.bsp = render_bsp;
    w.renderer.begin_map(&w.bsp);
    w.player = player;
    w.yaw = yaw;
    w.pitch = pitch;
    // Same clean-slate reset as a changelevel (the map restarted from scratch).
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.glides.clear();
    w.beams.clear();
    w.notify.clear();
    w.centerprint = None;
    // cl.time: the new server's sv.time (SV_SpawnServer's 1.0 + the signon
    // frames), which CL_LerpPoint snaps the client clock to.
    w.clock = w.server.time();
    w.damage_blend = 0.0;
    w.bonus_blend = 0.0;
    w.faceanimtime = 0.0;
    w.cl_items = client_items(w); // unflashed, as at a changelevel
    w.item_gettime = [0.0; 32];
    w.oldz = f32::NAN;
    // Same intermission/finale reset as a changelevel (CL_ClearState).
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    w.pending_menu_credits = false;
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Stop the dead run's loops; restart the fresh level's (see try_changelevel).
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(statics));
    sound.push(SoundCall::Cd(CdCall::cdtrack(w.server.cd_track())));
    let _ = w.server.drain_svc_events();
    let _ = w.server.drain_stufftext();
}

/// `Host_Loadgame_f`'s post-fopen half: parse the header, spawn the named map,
/// and rebuild a [`Walk`] around [`Server::load_savegame`]'s reconstructed
/// world. Errors return the console message to print (the C's where it has
/// one); the caller leaves the current game untouched on `Err`. The loaded
/// server draws from the host session's `rand`, like [`build_walk_map`]'s.
/// `max_edicts` is the live `sv_max_edicts` cvar, exactly as
/// [`build_walk_map`] takes it — a save written with the extra on needs it
/// raised to load back (see [`Server::load_savegame`]).
pub fn build_walk_savegame(
    pak: Pak,
    text: &str,
    rand: &Rc<QRand>,
    sound: &mut Vec<SoundCall>,
    max_edicts: usize,
) -> Result<Walk, String> {
    use crate::save::{SAVEGAME_VERSION, parse_savegame};

    let sg = parse_savegame(text).map_err(|e| e.to_string())?;
    if sg.version != SAVEGAME_VERSION {
        // Con_Printf ("Savegame is version %i, not %i\n", ...)
        return Err(format!("Savegame is version {}, not {}", sg.version, SAVEGAME_VERSION));
    }
    let couldnt = || "Couldn't load map".to_string(); // SV_SpawnServer failure
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let map = format!("maps/{}.bsp", sg.map_name);
    let map_bytes = read(&map).ok_or_else(couldnt)?;
    let sim_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let render_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let progs_bytes = read("progs.dat").ok_or_else(couldnt)?;
    let progs = Progs::parse(&progs_bytes).map_err(|e| e.to_string())?;

    // The engine-side load: header -> SV_SpawnServer (map spawn functions DO
    // run, rebuilding precaches; see save.rs) -> lightstyles -> globals ->
    // edicts -> sv.time/spawn_parms. No entrance script, no signon settle.
    let mut server =
        Server::load_savegame(sim_bsp, progs, Some(pak.clone()), rand, text, max_edicts).map_err(|e| e.to_string())?;
    let player = server.player_edict().ok_or_else(|| "savegame has no player edict".to_string())?;
    // Host_Spawn_f names the client edict (`netname = host_client->name`) only
    // for a fresh spawn: a loaded game keeps the save's. Saves the port wrote
    // before it did that (2026-09-25) carry an empty netname, which read
    // "  was shot by a Grunt" until the next level; give them the name
    // Host_Spawn_f would have (cl_name's "player").
    if server.vm.ent_str(player, server.vm.fo().netname).to_string().is_empty() {
        server.vm.set_ent_string(player, server.vm.fo().netname, "player");
    }

    // The save's spawn parms are the level-ENTRY parms (svs.clients->
    // spawn_parms): a respawn on the loaded level restores the state the
    // player entered it with, exactly like an uninterrupted session.
    let entry_parms = sg.spawn_parms;

    // View angles from the loaded player's v_angle. DEVIATION: the C's
    // Host_Spawn_f sends an svc_setangle built from ent->v.angles (the model
    // angles, whose pitch is the C's -v_angle/3 quirk, roll forced 0 — "never
    // send a roll angle, because savegames can catch the server expecting the
    // client to correct it"); restoring v_angle directly gives back the exact
    // view the player saved with, roll-free here too (this shell has no
    // persistent roll state).
    let v_angle = server.vm.ent_vec(player, server.vm.fo().v_angle);
    let yaw = v_angle[1];
    let pitch = clamp_pitch(v_angle[0]);

    // Capture this load's placed ambient loops (registered while the map's
    // spawn functions re-ran inside load_savegame) BEFORE the server moves
    // into the Walk; committed to the sound layer only after assembly succeeds.
    let statics = server.drain_static_sounds();

    let mut w = assemble_walk(pak, map, server, player, entry_parms, render_bsp, yaw, pitch).ok_or_else(couldnt)?;

    // Committed: tear down the previous level/mode's looping audio, start this
    // level's, and drop one-shot events the load's spawn + settle ticks queued
    // (same treatment as every other walk-building path).
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(statics));
    sound.push(SoundCall::Cd(CdCall::cdtrack(w.server.cd_track())));
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    let _ = w.server.drain_svc_events();
    let _ = w.server.drain_stufftext();
    Ok(w)
}
