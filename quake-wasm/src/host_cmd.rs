//! Console commands — host_cmd.c (plus `Cmd_ExecuteString`'s dispatch):
//! god/noclip/fly/kill/give/impulse/map, the viewsize cvar commands, and the
//! level swaps `changelevel` and `restart` (`Host_Changelevel_f` /
//! `Host_Restart_f`), which rebuild the server under the live [`Walk`].

use quake_rs::bsp::Bsp;
use quake_rs::dlight::DynamicLights;
use quake_rs::particles::ParticleSystem;
use quake_rs::progs::Progs;
use quake_rs::server::Server;

use crate::savegame::{do_load_command, do_save_command};
use crate::snd_dma::{bump_sound_generation, queue_static_sounds};
use crate::{build_walk_map, ensure_app, player_start, Walk};

// --- console command execution -------------------------------------------

/// Sane upper bounds the `give` command clamps to, mirroring Quake's pickup
/// caps (the player can't carry more than these).
const MAX_HEALTH: f32 = 250.0;
const MAX_ARMOR: f32 = 200.0;
const MAX_SHELLS: f32 = 100.0;
const MAX_NAILS: f32 = 200.0;
const MAX_ROCKETS: f32 = 100.0;
const MAX_CELLS: f32 = 100.0;

// QuakeC `items` weapon bits (quakedef.h): the weapon for digit `d` (1..8) is
// IT_SHOTGUN << (d-1), and IT_AXE is a separate high bit.
const IT_SHOTGUN: i32 = 1; // bit for weapon 2 base; weapon d>=2 is IT_SHOTGUN<<(d-2)
const IT_AXE: i32 = 4096;
pub(crate) const IT_INVISIBILITY: i32 = 1 << 19; // Ring of Shadows (524288)
const FL_GODMODE: i32 = 64;
pub(crate) const FL_ONGROUND: i32 = 512;
const MOVETYPE_WALK: f32 = 3.0;
const MOVETYPE_FLY: f32 = 5.0;
const MOVETYPE_NOCLIP: f32 = 8.0;

/// Parse `line` into whitespace argv and run the matching console command
/// against the live [`Walk`], appending any output to the console scrollback.
/// An empty line does nothing; an unknown command prints
/// `"unknown command: <cmd>"`. Commands that touch the player edict guard on a
/// live walk and print `"no active game"` when there is none. Nothing here
/// panics on a bad/missing argument (all parsing uses `.ok()`/defaults).
pub(crate) fn execute_console_command(line: &str) {
    let argv: Vec<&str> = line.split_whitespace().collect();
    let Some(&cmd) = argv.first() else { return };
    let cmd_lower = cmd.to_ascii_lowercase();

    // Commands that don't need the walk: echo / clear / help / cmdlist.
    match cmd_lower.as_str() {
        "clear" => {
            ensure_app(|a| a.console.clear());
            return;
        }
        "echo" => {
            let text = if argv.len() > 1 {
                argv[1..].join(" ")
            } else {
                String::new()
            };
            ensure_app(|a| a.console.println(text));
            return;
        }
        "help" | "cmdlist" => {
            ensure_app(|a| {
                a.console.println("commands:");
                a.console.println("  god noclip fly kill");
                a.console.println("  give <h|a|s|n|r|c|1-8> [n]");
                a.console.println("  impulse <n>   map <name>");
                a.console.println("  save <name>   load <name>");
                a.console.println("  sizeup  sizedown  viewsize [n]");
                a.console.println("  echo <text>   clear   help");
            });
            return;
        }
        // SCR_SizeUp_f / SCR_SizeDown_f: viewsize +/- 10 (SCR_CalcRefdef bounds
        // it to 30..120 on the next frame).
        "sizeup" => {
            ensure_app(|a| a.menu.size_up());
            return;
        }
        "sizedown" => {
            ensure_app(|a| a.menu.size_down());
            return;
        }
        // The `viewsize` cvar (Cvar_Command): no argument prints it the C's way,
        // one argument sets it (bounded like SCR_CalcRefdef).
        "viewsize" => {
            ensure_app(|a| match argv.get(1) {
                None => {
                    let v = a.menu.viewsize();
                    a.console.println(format!("\"viewsize\" is \"{}\"", cvar_string(v)));
                }
                Some(arg) => a.menu.set_viewsize(arg.parse::<f32>().unwrap_or(0.0)),
            });
            return;
        }
        _ => {}
    }

    // `map <name>` rebuilds the walk on a new level; handle it specially because
    // it replaces the whole Walk (can't be done while holding a &mut to it).
    if cmd_lower == "map" {
        run_map_command(argv.get(1).copied());
        return;
    }

    // `save`/`load` (Host_Savegame_f / Host_Loadgame_f): handled at this level
    // because load replaces the whole Walk (via the page round-trip) and save
    // runs guards that need the App, not just the walk. The C's Cmd_Argc()!=2
    // check covers extra args too, so pass None unless exactly one argument.
    if cmd_lower == "save" {
        do_save_command(if argv.len() == 2 { Some(argv[1]) } else { None });
        return;
    }
    if cmd_lower == "load" {
        do_load_command(if argv.len() == 2 { Some(argv[1]) } else { None });
        return;
    }

    // The remaining commands act on the live player edict. Run them under a
    // single borrow; guard a missing walk with "no active game".
    ensure_app(|a| {
        let has_walk = a.walk.is_some();
        if !has_walk {
            a.console.println("no active game");
            return;
        }
        // Split the borrow: the walk (player edict + vm) and the console output.
        // Take the player index + a raw pointer-free reference via the App.
        let mut out: Vec<String> = Vec::new();
        if let Some(w) = a.walk.as_mut() {
            run_game_command(w, &cmd_lower, &argv, &mut out);
        }
        for line in out {
            a.console.println(line);
        }
    });

    // An unrecognised command: report it. (Handled here so the borrow above can
    // finish first; run_game_command pushes nothing for an unknown verb.)
    let known = matches!(
        cmd_lower.as_str(),
        "god" | "noclip" | "fly" | "kill" | "give" | "impulse"
    );
    if !known {
        ensure_app(|a| a.console.println(format!("unknown command: {cmd}")));
    }
}

/// A cvar value as the console prints it: `%f` with the trailing zeros (and a
/// bare trailing point) trimmed — `100`, `55.5`. (The C prints the cvar's
/// STRING, which is whatever set it last: "100" from default.cfg, "55" typed,
/// but "110.000000" after `Cvar_SetValue`'s `%f`. This port keeps no cvar
/// strings, so it always prints the short form.)
fn cvar_string(v: f32) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Run a command that acts on the live player edict, pushing any output lines
/// into `out`. `cmd` is already lowercased; `argv[0]` is the command itself.
/// Unknown verbs push nothing (the caller reports them).
fn run_game_command(w: &mut Walk, cmd: &str, argv: &[&str], out: &mut Vec<String>) {
    let player = w.player;
    match cmd {
        // Host_God_f: flags ^= FL_GODMODE.
        "god" => {
            let flags = w.server.vm.ent_get_float(player, "flags") as i32 ^ FL_GODMODE;
            w.server.vm.ent_set_float(player, "flags", flags as f32);
            out.push(if flags & FL_GODMODE != 0 {
                "godmode ON".into()
            } else {
                "godmode OFF".into()
            });
        }
        // Host_Noclip_f: movetype toggles WALK <-> NOCLIP.
        "noclip" => {
            let mt = w.server.vm.ent_get_float(player, "movetype");
            if mt != MOVETYPE_NOCLIP {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_NOCLIP);
                out.push("noclip ON".into());
            } else {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_WALK);
                out.push("noclip OFF".into());
            }
        }
        // Host_Fly_f: movetype toggles WALK <-> FLY.
        "fly" => {
            let mt = w.server.vm.ent_get_float(player, "movetype");
            if mt != MOVETYPE_FLY {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_FLY);
                out.push("flymode ON".into());
            } else {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_WALK);
                out.push("flymode OFF".into());
            }
        }
        // Host_Kill_f: suicide through the QuakeC `ClientKill` entry point (the
        // REAL chain: suicide frame, frag penalty, respawn() — which in single
        // player issues localcmd("restart\n")). NOT a health hack: the QuakeC
        // owns the death. The pending restart is honoured HERE, not left for
        // step_walk, because client_frame clears stale requests at the top of
        // each frame — and it matches the C, where the queued "restart" Cbuf
        // text executes right after the kill command itself.
        "kill" => match w.server.client_kill() {
            Ok(true) => {
                if w.server.take_pending_restart() {
                    try_restart(w);
                }
                // No success line of its own: the QuakeC bprints
                // "<netname> suicides" (drained into notify next frame).
            }
            Ok(false) => out.push("Can't suicide -- allready dead!".into()),
            Err(e) => out.push(format!("kill failed: {e}")),
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
/// the `items` bit and selecting it). `n` (argv[2]) is the amount; ammo/health
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

    let mut set = |field: &str, val: f32, cap: f32, label: &str, out: &mut Vec<String>| {
        let v = val.clamp(0.0, cap);
        w.server.vm.ent_set_float(player, field, v);
        out.push(format!("gave {label} {v:.0}"));
    };

    match c0 {
        'h' => set("health", amount.unwrap_or(MAX_HEALTH), MAX_HEALTH, "health", out),
        'a' => set("armorvalue", amount.unwrap_or(MAX_ARMOR), MAX_ARMOR, "armor", out),
        's' => set("ammo_shells", amount.unwrap_or(MAX_SHELLS), MAX_SHELLS, "shells", out),
        'n' => set("ammo_nails", amount.unwrap_or(MAX_NAILS), MAX_NAILS, "nails", out),
        'r' => set("ammo_rockets", amount.unwrap_or(MAX_ROCKETS), MAX_ROCKETS, "rockets", out),
        'c' => set("ammo_cells", amount.unwrap_or(MAX_CELLS), MAX_CELLS, "cells", out),
        '1'..='8' => {
            // Weapon select/grant. Weapon 1 = axe (its own high bit); weapons
            // 2..8 are IT_SHOTGUN << (d-2), exactly as Host_Give_f does
            // (sv_player->v.items |= IT_SHOTGUN << (t[0]-'2')).
            let d = (c0 as u8 - b'0') as i32; // 1..8
            let bit = if d == 1 {
                IT_AXE
            } else {
                IT_SHOTGUN << (d - 2)
            };
            let items = w.server.vm.ent_get_float(player, "items") as i32 | bit;
            w.server.vm.ent_set_float(player, "items", items as f32);
            // Select it: the QuakeC `weapon` field is the active weapon bit.
            w.server.vm.ent_set_float(player, "weapon", bit as f32);
            out.push(format!("gave weapon {d}"));
        }
        _ => out.push(format!("give: unknown item '{what}'")),
    }
}

/// Run `map <name>`: build a fresh walk on `maps/<name>.bsp`. On success swap the
/// walk, close the console, and print `"loading <name>"`; on failure print
/// `"map not found: <name>"` and keep the current level.
fn run_map_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("usage: map <name>"));
        return;
    };
    let path = format!("maps/{name}.bsp");
    // Build the new walk OUTSIDE the borrow (it reads the pak + parses a BSP).
    let new_walk = build_walk_map(&path);
    ensure_app(|a| match new_walk {
        Some(nw) => {
            a.walk = Some(nw);
            a.mode = 0;
            a.console.println(format!("loading {name}"));
            // The level loaded: close the console so the player sees the new map.
            a.console.open = false;
            // Keep the menu closed too (a `map` from the console starts play).
            // Navigation-only reset: the C's `map` command never resets cvars or
            // keybindings, so the player's options and rebinds survive here too.
            a.menu.reset_nav();
            // Preserve the player's chosen render resolution across a `map` (the C
            // keeps the video mode): the framebuffer is untouched, and we eagerly
            // point the menu's current video mode at it — same as every other
            // re-boot site — so the Video Options list is correct the instant the
            // player opens it (not relying on the per-frame sync in step()).
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
        None => a.console.println(format!("map not found: {name}")),
    });
}

/// Perform a deferred level transition: save the current player's spawn parms,
/// load `next_map` and a fresh `progs.dat` from the open pak, spawn the new
/// level's entities, and reconnect the client carrying its inventory. On any
/// parse/spawn/connect failure the current level is left untouched (the guards
/// below all early-`return` rather than panic), so a missing or corrupt next map
/// is non-fatal — the player keeps playing the level they are on.
pub(crate) fn try_changelevel(w: &mut Walk, next_map: &str) {
    // Save the outgoing player's inventory into parm1..parm16 (SV_SaveSpawnparms
    // -> SetChangeParms). Done before we touch the old server's world.
    let parms = w.server.save_spawn_parms();
    // Capture serverflags (the episode rune SERVERFLAG_* bits) from the OUTGOING
    // server. The C keeps these alive across SV_SpawnServer (svs.serverflags);
    // building a brand-new Server would reset the global to 0 and lose the
    // collected runes, so we carry it forward onto the new level below.
    let serverflags = w.server.serverflags();
    // Carry the chosen difficulty across the level change. `skill` is a
    // thread-local that Server::with_pak resets to 1, so capture it from the
    // OUTGOING server now and restore it on the new one below (the start hub's
    // skill portal set it via cvar_set; without this the jump to e1m1 would
    // silently revert to Normal). Mirrors how serverflags is carried.
    let skill = w.server.skill();

    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    // The QuakeC `changelevel(map)` carries the BARE map name (e.g. "e1m1", from
    // the trigger's `map` key), but the pak stores it as "maps/e1m1.bsp". Build
    // the pak path (tolerating an already-qualified name). Without this the read
    // missed and the swap silently aborted — the start-hub episode-1 slipgate
    // (and every in-game changelevel) "did nothing".
    let map_file = if next_map.ends_with(".bsp") {
        next_map.to_string()
    } else {
        format!("maps/{next_map}.bsp")
    };
    // Two BSP copies (one for the sim/collision world the server owns, one for
    // rendering) plus a fresh progs.dat for the new server. Any failure aborts
    // the swap, leaving the live level running.
    let Some(map_bytes) = read(&map_file) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
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
    // Discard stale static-sound registrations (a previously failed spawn's)
    // so the drain after spawn_entities is exactly this level's.
    let _ = ns.drain_static_sounds();
    if ns.spawn_entities().is_err() {
        return;
    }
    // Capture the new level's placed ambient loops now (registered during
    // spawn_entities); committed to the page only once the swap succeeds below.
    let statics = ns.drain_static_sounds();
    let Ok(player) = ns.connect_client_with_parms(parms) else { return };
    // The carried inventory at the start of the NEW level becomes its entry parms,
    // so a respawn on this level restores the state the player arrived with.
    let entry_parms = ns.save_spawn_parms();
    // The C's signon physics frames (see build_walk_map): settle the arriving
    // player onto the floor before the new level's frame 0 renders. Any events
    // these ticks queue are dropped by the post-swap drains below (the C's
    // client misses tick 1's datagram sounds while not yet `spawned`; tick 2's
    // are technically deliverable there — dropping both is a deliberate,
    // inaudible-on-id-maps simplification, identical across all three paths).
    ns.run_signon_frames();

    // Commit the swap. From here nothing can fail.
    let (_spawn, yaw) =
        player_start(&render_bsp.entities).unwrap_or(([0.0, 0.0, 0.0], w.yaw));
    w.server = ns;
    w.bsp = render_bsp;
    w.player = player;
    w.entry_parms = entry_parms;
    w.map_name = map_file;
    w.yaw = yaw;
    w.pitch = 0.0;

    // New level, clean slate: drop the old level's particles / dynamic lights /
    // beams (CL_ClearState memsets cl_beams) and reset the animation clock so
    // liquids/sky restart from zero.
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.beams.clear();
    // Clear the on-screen text overlay on level load (SCR_BeginLoadingPlaque calls
    // Con_ClearNotify + scr_centertime_off=0): drop the half-built line AND the
    // already-flushed notify lines + the centerprint. Their expiry is an ABSOLUTE
    // clock value, and the clock resets to 0 below, so a stale "You got the Quad!"
    // would otherwise linger over the new level for old-clock seconds.
    w.notify_pending.clear();
    w.notify.clear();
    w.centerprint = None;
    w.clock = 0.0;
    // Reset the screen-blend state so the level change does not flash red.
    w.damage_blend = 0.0;
    w.last_health = f32::NAN;
    w.last_armor = f32::NAN;
    // Reset stair-step view smoothing so the new spawn doesn't glide from old Z.
    w.oldz = f32::NAN;
    // CL_ClearState: the new level starts OUT of intermission (cl.intermission=0)
    // with no stale finale text or sellscreen request.
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    // Drop any events the *outgoing* server queued (the new server starts fresh).
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Looping audio: stop the OLD level's loops (S_StopAllSounds on changelevel)
    // and hand the page the NEW level's placed ambient loops + a fresh ambient
    // ramp, captured above right after spawn_entities.
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_svc_events();
}

/// Single-player respawn: reload the CURRENT level fresh and reconnect the player
/// with the level-ENTRY spawn parms (the state they arrived with). Ported from the
/// engine running QuakeC's `localcmd("restart\n")` — a dead player who presses a
/// button restarts the map. A dead player's own state is useless (health 0, dropped
/// inventory), so `entry_parms` (captured at level entry) is what's restored,
/// matching how `restart` works in id's single-player. A read/parse failure leaves
/// the (dead) level running rather than crashing.
pub(crate) fn try_restart(w: &mut Walk) {
    let serverflags = w.server.serverflags();
    let skill = w.server.skill();
    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    let Some(map_bytes) = read(&w.map_name) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
    // SV_SpawnServer: world.model + the mapname global, before the entities load.
    ns.set_map_name(&w.map_name);
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
    // Static-loop bookkeeping mirrors try_changelevel: discard stale
    // registrations, spawn, capture this (re)load's own.
    let _ = ns.drain_static_sounds();
    if ns.spawn_entities().is_err() {
        return;
    }
    let statics = ns.drain_static_sounds();
    let Ok(player) = ns.connect_client_with_parms(w.entry_parms) else { return };
    // The C's signon physics frames (see build_walk_map): settle the respawned
    // player onto the floor before the restarted level's frame 0 renders.
    ns.run_signon_frames();

    // Commit the reload (nothing below can fail).
    let (_spawn, yaw) =
        player_start(&render_bsp.entities).unwrap_or(([0.0, 0.0, 0.0], w.yaw));
    w.server = ns;
    w.bsp = render_bsp;
    w.player = player;
    w.yaw = yaw;
    w.pitch = 0.0;
    // Same clean-slate reset as a changelevel (the map restarted from scratch).
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.beams.clear();
    w.notify_pending.clear();
    w.notify.clear();
    w.centerprint = None;
    w.clock = 0.0;
    w.damage_blend = 0.0;
    w.last_health = f32::NAN;
    w.last_armor = f32::NAN;
    w.oldz = f32::NAN;
    // Same intermission/finale reset as a changelevel (CL_ClearState).
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Stop the dead run's loops; restart the fresh level's (see try_changelevel).
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_svc_events();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use crate::console::{console_toggle, console_visible};
    use crate::menu::menu_cancel;
    use crate::input::{key_down, key_up, set_attack};
    use crate::vid::{set_resolution, viewsize};
    use crate::{boot, step, APP};

    #[test]
    fn sizeup_sizedown_console_commands_and_default_binds() {
        assert_eq!(boot(), 1);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        console_toggle();
        run_console_line("sizeup");
        assert_eq!(viewsize(), 110.0);
        run_console_line("sizeup");
        run_console_line("sizeup");
        assert_eq!(viewsize(), 120.0, "bounded at 120");
        run_console_line("sizedown");
        assert_eq!(viewsize(), 110.0);
        run_console_line("viewsize");
        let printed = APP.with(|c| {
            c.borrow().as_ref().unwrap().console.lines().last().map(str::to_string)
        });
        assert_eq!(printed.as_deref(), Some("\"viewsize\" is \"110\""), "Cvar_Command print");
        run_console_line("viewsize 5");
        assert_eq!(viewsize(), 30.0, "bounded at 30");
        run_console_line("viewsize 100");
        console_toggle();
        // default.cfg: `-` sizedown, `=` / `+` sizeup — in the game only
        // (key_dest == key_game), like every binding.
        key_down(i32::from(b'-'));
        key_up(i32::from(b'-'));
        assert_eq!(viewsize(), 90.0, "'-' is sizedown");
        key_down(i32::from(b'='));
        key_up(i32::from(b'='));
        key_down(i32::from(b'+'));
        key_up(i32::from(b'+'));
        assert_eq!(viewsize(), 110.0, "'=' and '+' are sizeup");
        menu_cancel(); // open the menu: keys no longer reach the bindings
        key_down(i32::from(b'-'));
        key_up(i32::from(b'-'));
        assert_eq!(viewsize(), 110.0, "no binding runs while the menu is up");
    }

    // -----------------------------------------------------------------------
    // Drop-down console
    // -----------------------------------------------------------------------

    /// Read the player edict's `flags` field from the live walk (0.0 if no walk).
    fn player_flags() -> i32 {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|a| a.walk.as_ref())
                .map(|w| w.server.vm.ent_get_float(w.player, "flags") as i32)
                .unwrap_or(0)
        })
    }

    fn console_scrollback() -> usize {
        APP.with(|c| {
            c.borrow().as_ref().map(|a| a.console.line_count()).unwrap_or(0)
        })
    }

    #[test]
    fn console_god_toggles_the_player_flags_bit() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        console_toggle(); // open
        assert_eq!(console_visible(), 1);

        let before = player_flags();
        assert_eq!(before & FL_GODMODE, 0, "godmode starts off");
        run_console_line("god");
        let after = player_flags();
        assert_ne!(after & FL_GODMODE, 0, "the god command set FL_GODMODE on the player");
        // The command echoed the input line + its result into the scrollback.
        assert!(console_scrollback() >= 2, "god echoed the line and a result");

        // A second `god` toggles it back off.
        run_console_line("god");
        assert_eq!(player_flags() & FL_GODMODE, 0, "a second god clears FL_GODMODE");
    }

    #[test]
    fn console_unknown_command_prints_an_error() {
        assert_eq!(boot(), 1);
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.clear());
        run_console_line("frobnicate now");
        // Exactly two lines: the echoed "]frobnicate now" and the error message.
        // (A known command would echo the line + its own variable output; an
        // unknown one always produces precisely the echo + one error line.)
        let lines = console_scrollback();
        assert_eq!(lines, 2, "unknown command echoes the line and one error line");
    }

    #[test]
    fn console_give_changes_the_field() {
        assert_eq!(boot(), 1);
        console_toggle();
        // give h 100 sets the player's health field.
        run_console_line("give h 100");
        assert_eq!(player_field("health"), 100.0, "give h set health");
        // give s 50 sets ammo_shells.
        run_console_line("give s 50");
        assert_eq!(player_field("ammo_shells"), 50.0, "give s set ammo_shells");
        // give 2 grants + selects the shotgun (items bit 1, weapon bit 1).
        run_console_line("give 2");
        let items = player_field("items") as i32;
        assert_ne!(items & IT_SHOTGUN, 0, "give 2 set the shotgun items bit");
        assert_eq!(player_field("weapon") as i32, IT_SHOTGUN, "give 2 selected the shotgun");
    }

    #[test]
    fn console_impulse_queues_next_impulse() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("impulse 9");
        let n = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().next_impulse
        });
        assert_eq!(n, 9, "impulse 9 queued the give-all cheat impulse");
    }

    #[test]
    fn console_clear_and_echo_and_noclip_fly_kill() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("echo hello world");
        assert!(console_scrollback() >= 2, "echo printed text");
        run_console_line("clear");
        // After clear, only the echoed "]clear" line (pushed before exec) remains
        // — clear empties everything that came before it.
        assert_eq!(console_scrollback(), 0, "clear empties the scrollback");

        // noclip toggles movetype WALK <-> NOCLIP.
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_NOCLIP, "noclip set NOCLIP");
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "noclip toggled back to WALK");
        // fly toggles WALK <-> FLY.
        run_console_line("fly");
        assert_eq!(player_field("movetype"), MOVETYPE_FLY, "fly set FLY");

        // `kill` routes through the QuakeC ClientKill (Host_Kill_f), whose
        // respawn() issues localcmd("restart\n") in single player: the level
        // reloads and the player comes back ALIVE with the level-ENTRY loadout
        // — wiping the cheats above and the marker rockets we set here.
        run_console_line("give r 5"); // marker: not part of the entry parms
        assert_eq!(player_field("ammo_rockets"), 5.0);

        // An already-dead player is refused (Host_Kill_f's guard): no QuakeC
        // runs, no restart — the live state is untouched.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 0.0);
        });
        run_console_line("kill");
        assert_eq!(
            player_field("ammo_rockets"),
            5.0,
            "a refused kill must not reload the level"
        );
        assert_eq!(player_field("health"), 0.0, "a refused kill leaves the player as-is");

        // Alive again: kill -> ClientKill -> respawn() -> localcmd("restart")
        // -> the level restarts. Fresh player: alive, walking, marker wiped.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 70.0);
        });
        run_console_line("kill");
        assert_eq!(player_field("health"), 100.0, "suicide restarted the level: alive");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(
            player_field("movetype"),
            MOVETYPE_WALK,
            "fresh player walks (the fly cheat did not survive the restart)"
        );
        assert_eq!(
            player_field("ammo_rockets"),
            0.0,
            "restart restored the level-ENTRY parms (the marker is gone)"
        );
    }

    #[test]
    fn console_map_failure_keeps_level_and_prints_not_found() {
        assert_eq!(boot(), 1);
        console_toggle();
        let had_walk = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(had_walk);
        run_console_line("map nosuchmap");
        // The walk is unchanged and the console stays OPEN (failure path).
        let still = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(still, "a missing map leaves the current walk in place");
        assert_eq!(console_visible(), 1, "a failed map keeps the console open");
    }

    #[test]
    fn console_command_guards_missing_walk() {
        // A fresh app with NO walk: a game command prints "no active game", no panic.
        ensure_app(|a| {
            a.walk = None;
            a.console.open = true;
            a.console.clear();
        });
        run_console_line("god");
        // Echoed line + "no active game".
        assert!(console_scrollback() >= 2, "god with no walk prints a guard message");
    }

    /// QuakeC deadflag values (client.qc / defs.qc).
    const DEAD_DYING: f32 = 1.0;
    const DEAD_DEAD: f32 = 2.0;
    const DEAD_RESPAWNABLE: f32 = 3.0;

    /// End-to-end proof of the single-player death -> respawn chain on the REAL
    /// embedded e1m1 + progs.dat, through the exact path the browser uses
    /// (`boot()` / `step()`):
    ///
    ///   self-fired rocket -> T_RadiusDamage -> T_Damage -> Killed -> PlayerDie
    ///   (deadflag = DEAD_DYING, movetype = TOSS: corpse physics) -> the
    ///   death-anim THINKS play out while health < 0 (PlayerDead -> deadflag =
    ///   DEAD_DEAD) -> PlayerDeathThink (run from PlayerPreThink while dead)
    ///   sees all buttons released (deadflag = DEAD_RESPAWNABLE) -> a +attack
    ///   press reaches the QuakeC `button0` field while dead -> respawn() ->
    ///   localcmd("restart\n") -> take_pending_restart -> try_restart reloads
    ///   the level with the level-ENTRY parms: the player is alive at the spawn
    ///   point in a reset world.
    #[test]
    fn real_death_chain_respawns_via_restart() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        set_resolution(320, 200); // keep the per-step debug render cheap
        // boot() opens the main menu, which gates gameplay input; close it.
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Arm the rocket launcher and aim straight down (test SETUP only — the
        // kill itself travels the real QuakeC damage chain). Health 30: one
        // self-rocket deals ~55 (radius 120 minus distance falloff, halved for
        // attacker == target), leaving ~-25 — dead, but above the -40 gib line,
        // so the longer death-ANIM think chain runs.
        let spawn_org = APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.pitch = 80.0; // straight down (the +80 clamp)
            w.server.vm.ent_set_float(p, "health", 30.0);
            let items = w.server.vm.ent_get_float(p, "items") as i32 | IT_RL;
            w.server.vm.ent_set_float(p, "items", items as f32);
            w.server.vm.ent_set_float(p, "ammo_rockets", 5.0);
            player_start(&w.bsp.entities).expect("e1m1 has info_player_start").0
        });

        // Select the RL through the REAL impulse path (PlayerPostThink ->
        // W_WeaponFrame -> ImpulseCommands -> W_ChangeWeapon -> W_SetCurrentAmmo).
        APP.with(|c| {
            c.borrow_mut().as_mut().unwrap().walk.as_mut().unwrap().next_impulse = 7
        });
        step(0.05);
        assert_eq!(
            player_field("weapon") as i32,
            IT_RL,
            "impulse 7 selected the rocket launcher"
        );

        // Settle on the floor, then FIRE for one frame and release.
        for _ in 0..4 {
            step(0.05);
        }
        let mut trace: Vec<(usize, f32, f32)> = Vec::new(); // (frame, health, deadflag)
        trace.push((0, player_field("health"), player_field("deadflag")));
        set_attack(1);
        step(0.05);
        set_attack(0);

        // Ride the death out with all buttons released, tracing deadflag per
        // frame. Once DYING, hold +attack for ONE frame to prove UserCmd buttons
        // reach the QuakeC button0 field while dead (nothing consumes it during
        // DEAD_DYING: PlayerPreThink returns early and W_WeaponFrame is
        // deadflag-gated), then release well before the DEAD_DEAD button-free
        // wait.
        let mut probed_button_while_dead = false;
        for i in 1..=120 {
            step(0.05);
            let (h, df) = (player_field("health"), player_field("deadflag"));
            trace.push((i, h, df));
            if df == DEAD_DYING && !probed_button_while_dead {
                set_attack(1);
                step(0.05);
                assert_eq!(
                    player_field("button0"),
                    1.0,
                    "the attack button reaches QuakeC button0 while dead"
                );
                assert!(player_field("health") < 0.0, "the probe ran while dead");
                set_attack(0);
                step(0.05); // settle the release
                probed_button_while_dead = true;
            }
            if df == DEAD_RESPAWNABLE {
                break;
            }
        }
        assert!(probed_button_while_dead, "the DEAD_DYING phase was observed");

        // The chain, in order: alive -> DYING (PlayerDie, via the real
        // T_Damage) -> DEAD (the death-anim thinks ran out while health < 0 —
        // client thinks RUN while dead) -> RESPAWNABLE (PlayerDeathThink, run
        // from PlayerPreThink while dead, saw every button released).
        let mut seq: Vec<f32> = Vec::new();
        for &(i, h, df) in &trace {
            if seq.last() != Some(&df) {
                seq.push(df);
                // Evidence of the chain as it executed (visible with --nocapture).
                eprintln!("deadflag -> {df} at frame {i} (health {h})");
            }
        }
        assert_eq!(
            seq,
            vec![0.0, DEAD_DYING, DEAD_DEAD, DEAD_RESPAWNABLE],
            "deadflag progression; full trace: {trace:?}"
        );

        // DEAD_RESPAWNABLE: the dead player waits for a button. Press +attack:
        // PlayerDeathThink consumes it and calls respawn() ->
        // localcmd("restart\n"); step_walk drains take_pending_restart() and
        // try_restart() reloads the level inside this same step.
        let t_before = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().server.time()
        });
        set_attack(1);
        step(0.05);
        set_attack(0);

        assert_eq!(player_field("health"), 100.0, "respawned alive (entry health)");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "fresh player walks");
        let items = player_field("items") as i32;
        assert_eq!(
            items & IT_RL,
            0,
            "the cheat rocket launcher did NOT survive (level-ENTRY parms restored)"
        );
        assert_ne!(items & IT_SHOTGUN, 0, "the entry loadout (shotgun) is back");
        assert_eq!(player_field("ammo_rockets"), 0.0, "cheat rockets wiped");
        assert_eq!(player_field("ammo_shells"), 25.0, "entry shells restored");

        // Back at the spawn point, in a rebuilt world (server time restarted).
        let (org, t_after) = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            (w.server.vm.ent_get_vector(w.player, "origin"), w.server.time())
        });
        assert!(
            (org[0] - spawn_org[0]).abs() < 16.0
                && (org[1] - spawn_org[1]).abs() < 16.0
                && (org[2] - spawn_org[2]).abs() < 64.0,
            "respawned at the spawn point: {org:?} vs {spawn_org:?}"
        );
        assert!(
            t_after < t_before,
            "the world was rebuilt: server time restarted ({t_after} < {t_before})"
        );
    }

    /// An ENVIRONMENT kill reaches the same chain: slime damage is dealt by
    /// client.qc `WaterMove` (run from PlayerPreThink) -> `T_Damage(self, world,
    /// world, 4*waterlevel)` -> Killed -> PlayerDie — the attacker==world branch
    /// (no knockback), unlike the rocket. Teleporting the player into e1m1's
    /// slime pool is test setup; the damage itself travels the real QuakeC path,
    /// and the death rides the same anim -> DEAD_RESPAWNABLE -> button ->
    /// restart tail.
    #[test]
    fn environment_slime_kill_enters_the_same_death_chain() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Find a submerged spot: scan the world bounds on a coarse grid for
        // CONTENTS_SLIME that is still slime 48 units higher, so a player with
        // origin 24 above the probe has the eye (origin + 22) under the surface
        // (waterlevel 3 -> 12 damage per slime tick).
        let slime: Option<[f32; 3]> = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            let world = &w.bsp.models[0];
            let (mins, maxs) = (world.mins, world.maxs);
            let mut z = mins[2] + 16.0;
            while z < maxs[2] {
                let mut x = mins[0] + 16.0;
                while x < maxs[0] {
                    let mut y = mins[1] + 16.0;
                    while y < maxs[1] {
                        if quake_rs::world::point_contents(&w.bsp, [x, y, z])
                            == quake_rs::bsp::CONTENTS_SLIME
                            && quake_rs::world::point_contents(&w.bsp, [x, y, z + 48.0])
                                == quake_rs::bsp::CONTENTS_SLIME
                        {
                            return Some([x, y, z]);
                        }
                        y += 64.0;
                    }
                    x += 64.0;
                }
                z += 64.0;
            }
            None
        });
        let p = slime.expect("e1m1 has a slime pool deep enough to submerge in");

        // Drop the player in with 5 health: the first WaterMove slime tick
        // (4 * waterlevel) kills through the real chain. waterlevel is sensed
        // during the move phase, so the kill lands a couple of frames in.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let pl = w.player;
            w.server.vm.ent_set_vector(pl, "origin", [p[0], p[1], p[2] + 24.0]);
            w.server.vm.ent_set_vector(pl, "velocity", [0.0, 0.0, 0.0]);
            w.server.vm.ent_set_float(pl, "health", 5.0);
        });
        let mut died = false;
        for _ in 0..20 {
            step(0.05);
            if player_field("deadflag") >= DEAD_DYING {
                died = true;
                break;
            }
        }
        assert!(died, "slime damage killed through PlayerDie (deadflag set)");
        assert!(player_field("health") < 0.0, "the slime tick took health below zero");

        // Same tail as the rocket death: anim out, button, restart, alive.
        let mut respawnable = false;
        for _ in 0..120 {
            step(0.05);
            if player_field("deadflag") == DEAD_RESPAWNABLE {
                respawnable = true;
                break;
            }
        }
        assert!(respawnable, "the death anim ran out to DEAD_RESPAWNABLE");
        set_attack(1);
        step(0.05);
        set_attack(0);
        assert_eq!(
            player_field("health"),
            100.0,
            "respawned alive after the environment kill"
        );
        assert_eq!(player_field("deadflag"), 0.0);
    }
}
