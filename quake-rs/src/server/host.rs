//! Host-side state around the server: the `skill` cvar, the deferred
//! `changelevel` / `restart` commands, spawn parms and serverflags across a
//! level change, `kill`, the signon settle frames, and the savegame loader's
//! rollback of the per-thread transports.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/host_cmd.c` — `Host_Changelevel_f` / `Host_Restart_f` (the
//!   console commands `PF_changelevel` / `PF_localcmd` defer through
//!   `Cbuf_AddText`), `Host_Kill_f`, `Host_Spawn_f` / `Host_Begin_f` (the
//!   signon frames), `Host_Loadgame_f` (what a failed load must leave intact).
//! * `WinQuake/sv_main.c` — `SV_SaveSpawnparms`, and `SV_SpawnServer`'s
//!   `skill` → `current_skill` rounding.
//! * `WinQuake/pr_cmds.c` — `PF_changelevel`, `PF_localcmd`.
//!
//! This port has no console, command buffer or cvar registry, so the pieces of
//! host state QuakeC can reach live in per-thread cells here, and the
//! front-end (wasm shell, quaketool) plays `Host_Frame`'s part through the
//! `Server` methods below.

use super::lightstyle::{snapshot_lightstyles, LIGHTSTYLES, MAX_LIGHTSTYLES};
use super::msg::{
    reset_svc_recognizer, reset_temp_entity_decoder, take_messages, take_particle_bursts,
    take_sound_events, take_static_sounds, take_svc_events, take_temp_entities,
};
use super::{parm_global_name, Server, UserCmd, NUM_SPAWN_PARMS, SETTLE_FRAMETIME};
use crate::vm::Vm;
use crate::Result;

// ---------------------------------------------------------------------------
// The `skill` cvar (host_cmd.c / sv_main.c `current_skill`).
//
// The original engine kept `skill` in the console-cvar registry and derived an
// integer `current_skill = (int)(skill.value + 0.5)` clamped to 0..3 in
// `SV_SpawnServer`. This headless port has no cvar subsystem and the `Vm` field
// set is fixed (we must not extend it), so — exactly like the changelevel /
// lightstyle transports — we hold the live skill value in a per-thread cell.
// `PF_cvar("skill")` reads it, `PF_cvar_set("skill", N)` writes it (clamped),
// and `ED_LoadFromFile` reads it to filter monsters/items by difficulty. The
// difficulty portals in the start map are `trigger_setskill` entities whose
// QuakeC `touch` calls `cvar_set("skill", N)`, so honouring `cvar_set` here is
// what makes those portals actually change which entities spawn.
//
// THREAD-LOCAL (not a process-global atomic): a server session runs all its
// QuakeC on one thread, so a `thread_local` is the correct scope AND keeps each
// test thread isolated (the cell is the same shape as the sound/lightstyle/
// changelevel queues above).
// ---------------------------------------------------------------------------

thread_local! {
    /// Live integer skill level (0=easy, 1=medium, 2=hard, 3=nightmare),
    /// defaulting to 1 (single-player medium, matching the stock `skill` "1").
    static SKILL: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}

/// The `current_skill` value: the live [`SKILL`] read back as an int. Used by
/// the spawn filter and by `cvar("skill")`.
pub(super) fn skill_value() -> i32 {
    SKILL.with(|s| s.get())
}

/// Set the skill level, clamped to `0..=3` exactly as `SV_SpawnServer` does
/// (`current_skill = (int)(value + 0.5)`, then clamp). The input is the raw
/// float a `cvar_set("skill", N)` would pass; we round it the way the C does.
pub(super) fn set_skill_value(v: f32) {
    // SV_SpawnServer: current_skill = (int)(skill.value + 0.5); clamp 0..3.
    let s = ((v + 0.5) as i32).clamp(0, 3);
    SKILL.with(|cell| cell.set(s));
}

/// Reset the skill to the default (1, medium). Called when a fresh server is
/// built so a prior level's `cvar_set("skill", …)` cannot leak into the next
/// (mirrors the per-thread reset of the changelevel / lightstyle transports).
pub(super) fn reset_skill() {
    SKILL.with(|s| s.set(1));
}

// ---------------------------------------------------------------------------
// Deferred level change (PF_changelevel).
//
// The C `PF_changelevel` (pr_cmds.c, non-`QUAKE2` build) does NOT swap the map
// inline — the VM is mid-execution and the entity/global memory the builtin
// would tear down is exactly what the rest of the calling frame is still using.
// It guards against a double issue (`svs.changelevel_issued`) and merely defers:
// `Cbuf_AddText("changelevel <map>")`, which `Host_Frame` processes AFTER the
// current frame finishes. We mirror this precisely: the builtin only *records*
// the requested map name in a thread-local; the front-end takes it after
// `client_frame` returns (via [`Server::take_pending_changelevel`]) and performs
// the swap itself, never inside the builtin call.
//
// The `thread_local!` choice is identical to the sound/particle/temp-entity
// queues above: builtins are `fn(&mut Vm)` and cannot see the `Server`, and
// `vm.rs` is off-limits, so the deferred request cannot hang off either. Server
// methods run on the same thread as the builtins, so a request a frame's QuakeC
// fired is visible to `take_pending_changelevel` right after the frame.
// ---------------------------------------------------------------------------

thread_local! {
    /// The map name requested by a deferred `changelevel()` this frame, or `None`.
    /// First-writer-wins within a frame, mirroring the C `svs.changelevel_issued`
    /// guard that drops a second `PF_changelevel` until the swap completes. Taken
    /// (and cleared) by [`Server::take_pending_changelevel`]; reset in
    /// [`Server::new`] so a stale request can never leak across servers.
    static CHANGELEVEL_REQUEST: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Record a deferred level change to `map` (first-writer-wins this frame).
fn push_changelevel(map: String) {
    CHANGELEVEL_REQUEST.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(map);
        }
    });
}

/// Take and clear the deferred level-change request, if any.
fn take_changelevel() -> Option<String> {
    CHANGELEVEL_REQUEST.with(|c| c.borrow_mut().take())
}

/// Clear any pending level-change request (called from [`Server::new`] so a
/// stale request from a prior server cannot leak into a fresh one).
pub(super) fn reset_changelevel() {
    CHANGELEVEL_REQUEST.with(|c| *c.borrow_mut() = None);
}

/// `PF_changelevel` (#70): `void(string s) changelevel`. The C looked up its
/// string argument, guarded against a double issue, and deferred the actual swap
/// via `Cbuf_AddText("changelevel <s>")`. We faithfully *only* record the map
/// name here (PARM0, the `string_t` of the destination map, e.g. `"e1m2"`); the
/// front-end performs the swap after the frame. Never swaps inline.
pub(super) fn bi_changelevel(vm: &mut Vm) -> Result<()> {
    let map = vm.arg_string(0);
    push_changelevel(map);
    Ok(())
}

thread_local! {
    /// Set when QuakeC issues `localcmd("restart\n")` — the single-player respawn
    /// path (a dead player who presses a button runs `client.qc`'s
    /// `localcmd("restart\n")` to reload the current level with fresh entry parms).
    /// Drained by [`Server::take_pending_restart`]; reset in [`Server::new`].
    static RESTART_REQUEST: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
}

pub(super) fn reset_restart() {
    RESTART_REQUEST.with(|c| *c.borrow_mut() = false);
}

/// `PF_localcmd` (#46): `void(string s) localcmd` — `Cbuf_AddText(s)`, i.e. QuakeC
/// pushing a console command. Most are host/diagnostic and irrelevant to this port,
/// but single-player gameplay issues a few level-control commands we MUST honour:
///   * `restart` — reload the current level (the death-respawn path, `client.qc`).
///   * `changelevel <map>` / `map <map>` — defer a level swap (same as PF_changelevel).
///
/// Everything else is a benign no-op (matching the old behaviour). The token parse
/// is whitespace-split and case-insensitive on the command word.
pub(super) fn bi_localcmd(vm: &mut Vm) -> Result<()> {
    let cmd = vm.arg_string(0);
    let mut it = cmd.split_whitespace();
    match it.next().map(|w| w.to_ascii_lowercase()).as_deref() {
        Some("restart") => {
            RESTART_REQUEST.with(|c| *c.borrow_mut() = true);
        }
        Some("changelevel") | Some("map") => {
            if let Some(map) = it.next() {
                push_changelevel(map.to_string());
            }
        }
        _ => {} // other console text: benign no-op, as before.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-thread transport capture/restore for the savegame loader.
//
// `Server::load_savegame` builds a THROWAWAY server (`with_pak` +
// `spawn_entities`) before the `.sav` blocks have proven parseable, and that
// build resets/repopulates the per-thread transports (the lightstyle table,
// the skill cell) and queues spawn-time events (sounds, particles, svc
// commands). On success the new server owns all of it; on FAILURE the caller
// keeps its old `Server` — whose next `run_frame` re-syncs `lightstyles` from
// the shared transport and whose `skill()` reads the shared cell — so a
// rejected save would otherwise leak its lightstyles/skill into the running
// game it was supposed to leave intact (save.rs's documented deviation from
// the C's Sys_Error). The loader captures the persistent transports up front
// and, on any error, restores them and discards the transient queues (the
// same drop-the-spawn's-one-shots treatment every SUCCESSFUL build applies).
// ---------------------------------------------------------------------------

/// The persistent per-thread transports a savegame load clobbers, captured by
/// [`crate::save`]'s loader before it spawns the throwaway server and handed
/// back through [`restore_transports`] when the load fails.
pub(crate) struct TransportSnapshot {
    lightstyles: [String; MAX_LIGHTSTYLES],
    skill: i32,
}

/// Capture the caller's per-thread transport state (see [`TransportSnapshot`]).
pub(crate) fn capture_transports() -> TransportSnapshot {
    TransportSnapshot {
        lightstyles: snapshot_lightstyles(),
        skill: skill_value(),
    }
}

/// Put the captured persistent transports back and discard everything the
/// failed build queued, so the still-running game's next frame sees exactly
/// the state it left behind. The transient queues are cleared rather than
/// captured: the caller drains them at the end of every frame (and a load
/// runs between frames), so "empty" IS the caller's state — replaying the
/// failed spawn's one-shot sounds/particles/svc commands into the surviving
/// game would be its own leak.
pub(crate) fn restore_transports(snap: TransportSnapshot) {
    LIGHTSTYLES.with(|t| *t.borrow_mut() = snap.lightstyles);
    SKILL.with(|s| s.set(snap.skill));
    reset_changelevel();
    reset_restart();
    reset_svc_recognizer();
    reset_temp_entity_decoder();
    let _ = take_sound_events();
    let _ = take_static_sounds();
    let _ = take_particle_bursts();
    let _ = take_messages();
    let _ = take_temp_entities();
    let _ = take_svc_events();
}

impl Server {
    /// The signon-sequence physics frames between `PutClientInServer` and the
    /// first rendered frame. Call once after [`Self::connect_client`] /
    /// [`Self::connect_client_with_parms`], BEFORE rendering frame 0.
    ///
    /// In WinQuake a connecting client's signon spans several host frames: the
    /// "spawn" client command (`Host_Spawn_f`, host_cmd.c — runs QuakeC
    /// `PutClientInServer`) and the "begin" command (`Host_Begin_f`) execute in
    /// `SV_RunClients` on consecutive frames, and each of those frames then runs
    /// `SV_Physics` (host.c `Host_ServerFrame`) before the client reaches
    /// signon 4 and `SCR_EndLoadingPlaque` re-enables drawing (cl_parse.c
    /// `CL_ParseUpdate`: "first update is the final signon stage"). During those
    /// ticks `SV_Physics_Client` runs full player physics — the client is
    /// `active`, just not yet `spawned`, so its movement cmd stays zeroed
    /// (sv_user.c `SV_RunClients`: `if (!host_client->spawned) memset(&cmd...)`)
    /// — which matters because QuakeC `PutClientInServer` places the player at
    /// `spot.origin + '0 0 1'` and some spawn spots float well above the floor
    /// (the start map's `info_player_start` is ~5 units up): the player falls to
    /// the ground DURING the signon, before the first visible frame.
    ///
    /// This port's `connect_client` compresses the whole signon round-trip into
    /// one call, so a front-end that rendered immediately after it would show
    /// the settle on screen — a one-time whole-view shift over the first frames
    /// (the reported texture/lighting "pop"). The C ticks run at
    /// `Host_FilterTime`'s real frame duration, clamped to at most 0.1 s — and
    /// 0.1 is also exactly the `host_frametime` id hard-codes for
    /// `SV_SpawnServer`'s own two "let everything settle" frames — so we run the
    /// two ticks at [`SETTLE_FRAMETIME`], which settles any spawn drop up to
    /// ~24 units deterministically. The cmd carries the player's current view
    /// angles (the C never touches `v_angle` during signon) with zero
    /// moves/buttons. Think faults are isolated by `client_frame`; a hard fault
    /// is swallowed (a boot must not fail over a settle tick), matching
    /// `spawn_entities`' own settle-frame handling. The golden `scene` tool
    /// never connects a client, so this does not affect golden renders.
    pub fn run_signon_frames(&mut self) {
        let (yaw, pitch) = if self.player >= 0 {
            let ang = self.vm.ent_get_vector(self.player, "angles");
            let vang = self.vm.ent_get_vector(self.player, "v_angle");
            (ang[1], vang[0])
        } else {
            (0.0, 0.0)
        };
        let cmd = UserCmd {
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            yaw,
            pitch,
            buttons: 0,
            impulse: 0,
        };
        let _ = self.client_frame(&cmd, SETTLE_FRAMETIME);
        let _ = self.client_frame(&cmd, SETTLE_FRAMETIME);
    }

    /// `SV_SaveSpawnparms` for the local client: set the QuakeC `self` global to
    /// the player edict, run the progs `SetChangeParms` (which writes the
    /// player's persistent state — items/health/ammo/weapon/armor — into the 16
    /// `parm1..parm16` globals), then read those globals back into an array the
    /// caller can hand to a new server's [`Self::connect_client_with_parms`].
    ///
    /// Mirrors `SV_SaveSpawnparms` (host.c): `pr_global_struct->self = client`,
    /// `PR_ExecuteProgram(SetChangeParms)`, then copy `parm1..16` into
    /// `client->spawn_parms`. If the progs lacks `SetChangeParms` (a minimal mod)
    /// the run is a no-op and the *current* parm globals are returned unchanged;
    /// a missing individual parm global reads as `0.0` (`gget_float`), so this
    /// never panics. Returns `[0.0; 16]` when no client has connected.
    pub fn save_spawn_parms(&mut self) -> [f32; NUM_SPAWN_PARMS] {
        let mut parms = [0.0f32; NUM_SPAWN_PARMS];
        if self.player < 0 {
            return parms;
        }
        // SetChangeParms writes parm1..parm16 from the player's live fields
        // (self = the player edict, other = world). A fault is caught by run_sys.
        let _ = self.run_sys("SetChangeParms", self.player, 0);
        for (i, p) in parms.iter_mut().enumerate() {
            *p = self.vm.gget_float(&parm_global_name(i));
        }
        parms
    }

    /// Read the `serverflags` QuakeC global (the episode rune `SERVERFLAG_*`
    /// bits the player carries between levels). Returns `0.0` if the progs has
    /// no such global. The C keeps `pr_global_struct->serverflags` alive across
    /// `SV_SpawnServer`; a front-end driving a changelevel reads it from the
    /// outgoing server and writes it into the incoming one with
    /// [`Self::set_serverflags`] so the runes are not lost each level.
    pub fn serverflags(&self) -> f32 {
        self.vm.gget_float("serverflags")
    }

    /// Write the `serverflags` QuakeC global. A no-op if the progs lacks the
    /// global (the loader guards the offset), so calling it on a progs without
    /// runes is harmless. See [`Self::serverflags`].
    pub fn set_serverflags(&mut self, flags: f32) {
        self.vm.gset_float("serverflags", flags);
    }

    /// The current integer skill level (0=easy, 1=medium, 2=hard, 3=nightmare).
    ///
    /// This is the `current_skill` the spawn filter uses and the value
    /// `cvar("skill")` returns to the QuakeC. The difficulty portals in the start
    /// map (`trigger_setskill`) change it at runtime via `cvar_set("skill", N)`;
    /// a front-end reads it here to persist the player's choice across a
    /// changelevel (the constructor resets it to the medium default).
    pub fn skill(&self) -> i32 {
        skill_value()
    }

    /// Set the skill level from a raw value, normalised exactly as
    /// `SV_SpawnServer` does (`current_skill = (int)(value + 0.5)`, clamped to
    /// `0..=3`). A front-end calls this after construction to apply the menu's /
    /// the persisted difficulty before [`Self::spawn_entities`], so the spawn
    /// filter inhibits the right monsters/items. See [`Self::skill`].
    pub fn set_skill(&mut self, value: f32) {
        set_skill_value(value);
    }

    /// Take (and clear) the deferred level-change request a `changelevel()`
    /// builtin recorded this frame, or `None` if none was issued. A front-end
    /// calls this once after [`Self::client_frame`]: when it returns `Some(map)`,
    /// the front-end saves the spawn parms, loads `map`, and reconnects the
    /// client carrying its inventory. Mirrors the engine processing the deferred
    /// `changelevel <map>` console command after the frame.
    pub fn take_pending_changelevel(&mut self) -> Option<String> {
        take_changelevel()
    }

    /// Take and clear a pending single-player respawn (`localcmd("restart")`). The
    /// front-end calls this once after [`Self::client_frame`]: when it returns
    /// `true`, it reloads the CURRENT level (carrying the level-entry spawn parms,
    /// not the dead player's state). Mirrors the engine running the deferred
    /// `restart` console command after the frame.
    pub fn take_pending_restart(&mut self) -> bool {
        RESTART_REQUEST.with(|c| std::mem::replace(&mut *c.borrow_mut(), false))
    }

    /// `Host_Kill_f` (host_cmd.c): the `kill` console command — suicide via the
    /// QuakeC `ClientKill` entry point, NOT a health hack. Faithfully:
    /// * an already-dead player is refused (the C prints `"Can't suicide --
    ///   allready dead!\n"`; we return `Ok(false)` and the front-end prints it);
    /// * otherwise set `pr_global_struct->time = sv.time`, `self = sv_player`,
    ///   and execute `ClientKill` — whose QuakeC (client.qc) plays the suicide
    ///   frame, docks two frags, and calls `respawn()`, which in single player
    ///   issues `localcmd("restart\n")`, surfaced via
    ///   [`Server::take_pending_restart`] for the front-end to reload the level.
    ///
    /// Returns `Ok(true)` when `ClientKill` ran, `Ok(false)` when refused (dead,
    /// or no connected client). A QuakeC fault surfaces as `Err` (interpreter
    /// already reset), matching the other system entry points.
    pub fn client_kill(&mut self) -> Result<bool> {
        let player = self.player;
        if player < 0 || self.is_free(player) {
            return Ok(false);
        }
        if self.vm.ent_get_float(player, "health") <= 0.0 {
            return Ok(false); // "Can't suicide -- allready dead!"
        }
        // pr_global_struct->time = sv.time; self = sv_player; run ClientKill.
        let t = self.time();
        self.vm.gset_float("time", t);
        self.run_sys("ClientKill", player, 0)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Op, Progs, Statement, OFS_PARM0};
    use crate::server::testutil::*;
    use crate::server::FL_ONGROUND;

    #[test]
    fn run_signon_frames_settles_the_spawned_player_before_frame_zero() {
        // Regression: the first frames of live play showed a one-time whole-view
        // texture/lighting "pop" — the just-connected player (QuakeC
        // PutClientInServer places it at spot.origin + '0 0 1'; the start map's
        // spawn floats ~5 units up) fell to the floor ON SCREEN because the port
        // rendered frame 0 with zero physics frames after PutClientInServer. The
        // C runs two SV_Physics ticks during the signon (Host_Spawn_f /
        // Host_Begin_f frames) before SCR_EndLoadingPlaque re-enables drawing,
        // so WinQuake's first visible frame shows a settled player.
        // `run_signon_frames` ports those ticks.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        // The synthetic PutClientInServer sets origin=(0,0,40) but no size; give
        // it the player box (the real progs calls setsize inside the spawn).
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        assert_eq!(
            server.vm.ent_get_vector(p, "origin")[2],
            40.0,
            "spawn floats above the floor (box bottom at z=16, floor at z=0)"
        );

        server.run_signon_frames();

        // Settled BEFORE the front-end's frame 0: on the ground, no residual
        // fall velocity, box bottom resting on the floor (origin.z ~ 24).
        let org = server.vm.ent_get_vector(p, "origin");
        let vel = server.vm.ent_get_vector(p, "velocity");
        let flags = server.vm.ent_get_float(p, "flags") as i32;
        assert!(flags & FL_ONGROUND != 0, "player is on the ground at frame 0");
        assert_eq!(vel[2], 0.0, "no residual fall velocity at frame 0");
        assert!(
            (23.0..=25.0).contains(&org[2]),
            "box bottom rests on the z=0 floor (origin.z ~ 24), got {}",
            org[2]
        );

        // ...and frame 0 == frame N for a static, zero-input camera: subsequent
        // frames must not move the player AT ALL (the pop was exactly this
        // motion leaking into the first rendered frames).
        for i in 0..10 {
            server
                .client_frame(&UserCmd::default(), 1.0 / 60.0)
                .expect("static frame");
            let now = server.vm.ent_get_vector(p, "origin");
            assert_eq!(now, org, "origin is bit-identical on static frame {i}");
        }
    }

    // -------------------------------------------------------- level transitions

    #[test]
    fn bi_changelevel_records_map_and_drain_returns_once() {
        // bi_changelevel (PF_changelevel, #70) must record its string argument and
        // take_pending_changelevel must return it exactly once, then None.
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // A fresh server has no pending request.
        assert_eq!(
            server.take_pending_changelevel(),
            None,
            "fresh server has no pending changelevel"
        );

        // Drive the builtin directly: place the map name's string_t in PARM0.
        let map_t = server.vm.intern("e1m2");
        server.vm.set_gi(OFS_PARM0, map_t);
        bi_changelevel(&mut server.vm).expect("bi_changelevel");

        // take_pending_changelevel returns it once, then drains to None.
        assert_eq!(server.take_pending_changelevel().as_deref(), Some("e1m2"));
        assert_eq!(
            server.take_pending_changelevel(),
            None,
            "second take drains to None"
        );
    }

    #[test]
    fn bi_localcmd_restart_sets_pending_respawn() {
        // localcmd("restart\n") (PF_localcmd #46) is the single-player death-respawn
        // path: it must set the pending-restart flag, which take_pending_restart
        // returns exactly once. Other console text is a benign no-op, and a
        // `changelevel <map>` localcmd routes to the changelevel queue.
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        assert!(!server.take_pending_restart(), "fresh server: no pending restart");

        // "restart\n" -> pending restart (trailing newline + case-insensitive word).
        let t = server.vm.intern("restart\n");
        server.vm.set_gi(OFS_PARM0, t);
        bi_localcmd(&mut server.vm).expect("bi_localcmd restart");
        assert!(server.take_pending_restart(), "restart sets the pending flag");
        assert!(!server.take_pending_restart(), "second take drains to false");

        // An unrelated console command does nothing.
        let t2 = server.vm.intern("echo hi");
        server.vm.set_gi(OFS_PARM0, t2);
        bi_localcmd(&mut server.vm).expect("bi_localcmd echo");
        assert!(!server.take_pending_restart(), "unrelated localcmd is a no-op");

        // A `changelevel e1m2` localcmd routes to the changelevel queue, not restart.
        let t3 = server.vm.intern("changelevel e1m2");
        server.vm.set_gi(OFS_PARM0, t3);
        bi_localcmd(&mut server.vm).expect("bi_localcmd changelevel");
        assert!(!server.take_pending_restart(), "changelevel localcmd is not a restart");
        assert_eq!(server.take_pending_changelevel().as_deref(), Some("e1m2"));
    }

    /// Synthetic progs for [`Server::client_kill`] (`Host_Kill_f`): a `ClientKill`
    /// QuakeC function that calls the `localcmd` builtin (#46) with a
    /// `"restart\n"` string — the single-player suicide chain (`ClientKill` ->
    /// `respawn()` -> `localcmd("restart\n")`, client.qc) compressed to its
    /// engine-visible effect. Returns `(image, g_str, g_fn, localcmd_index)`;
    /// the test fills global `g_str` with the interned string and `g_fn` with
    /// the localcmd function value after load.
    fn client_kill_progs() -> (Vec<u8>, usize, usize, usize) {
        let mut b = Builder::new();
        b.entityfields = 24;

        b.add_global("self", EV_ENTITY, 31);
        b.add_global("other", EV_ENTITY, 32);
        b.add_global("time", EV_FLOAT, 33);
        b.add_global("world", EV_ENTITY, 34);
        b.add_global("frametime", EV_FLOAT, 35);
        b.add_global("viewentity", EV_FLOAT, 36);

        // Minimal field set so spawn()/link/connect work.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2);
        b.add_field("mins", EV_VECTOR, 5);
        b.add_field("maxs", EV_VECTOR, 8);
        b.add_field("absmin", EV_VECTOR, 11);
        b.add_field("absmax", EV_VECTOR, 14);
        b.add_field("flags", EV_FLOAT, 17);
        b.add_field("movetype", EV_FLOAT, 18);
        b.add_field("solid", EV_FLOAT, 19);
        b.add_field("size", EV_VECTOR, 20);
        b.add_field("health", EV_FLOAT, 23);

        let done = || Statement {
            op: Op::Done as u16,
            a: 0,
            b: 0,
            c: 0,
        };
        b.add_function("ClientConnect", vec![done()]);
        b.add_function("PutClientInServer", vec![done()]);

        let localcmd = b.add_builtin("localcmd", 46);

        // Cells the test fills after load: the "restart\n" string_t and the
        // localcmd function value the CALL1 dereferences.
        let g_str = 40u16;
        let g_fn = 41u16;
        // ClientKill: localcmd("restart\n");
        b.add_function(
            "ClientKill",
            vec![
                Statement {
                    op: Op::StoreS as u16,
                    a: g_str as i16,
                    b: OFS_PARM0 as i16,
                    c: 0,
                },
                Statement {
                    op: Op::Call1 as u16,
                    a: g_fn as i16,
                    b: 0,
                    c: 0,
                },
                done(),
            ],
        );

        (b.build(), g_str as usize, g_fn as usize, localcmd)
    }

    #[test]
    fn client_kill_runs_clientkill_and_refuses_when_dead() {
        // Host_Kill_f (host_cmd.c): `kill` must route through the QuakeC
        // ClientKill entry point (the REAL suicide chain, ending in respawn() ->
        // localcmd("restart\n") in single player), and must refuse an
        // already-dead player WITHOUT running ClientKill.
        let (img, g_str, g_fn, localcmd) = client_kill_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // No client connected yet: refused, no QuakeC runs.
        assert!(
            !server.client_kill().expect("kill w/o client"),
            "no connected client -> refused"
        );

        let player = server.connect_client().expect("connect");
        // Fill ClientKill's constants: the "restart\n" string + localcmd fn value.
        let s = server.vm.intern("restart\n");
        server.vm.set_gi(g_str, s);
        server.vm.set_gi(g_fn, localcmd as i32);

        // Alive player: ClientKill runs; its localcmd("restart\n") queues the
        // single-player respawn exactly once.
        server.vm.ent_set_float(player, "health", 100.0);
        assert!(
            server.client_kill().expect("kill alive"),
            "alive player -> ClientKill ran"
        );
        assert!(
            server.take_pending_restart(),
            "ClientKill -> localcmd(restart) -> pending respawn"
        );
        assert!(!server.take_pending_restart(), "second take drains to false");

        // Dead player: refused (the C prints "Can't suicide -- allready dead!"),
        // and ClientKill must NOT have run — nothing queued.
        server.vm.ent_set_float(player, "health", 0.0);
        assert!(!server.client_kill().expect("kill dead"), "dead -> refused");
        assert!(
            !server.take_pending_restart(),
            "a refused kill queues no respawn"
        );
    }

    #[test]
    fn bi_changelevel_first_writer_wins_within_a_frame() {
        // Two changelevel() calls before a drain: the first wins (mirrors the C
        // svs.changelevel_issued guard).
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        let a = server.vm.intern("e1m2");
        server.vm.set_gi(OFS_PARM0, a);
        bi_changelevel(&mut server.vm).expect("first");
        let bm = server.vm.intern("e1m3");
        server.vm.set_gi(OFS_PARM0, bm);
        bi_changelevel(&mut server.vm).expect("second");

        assert_eq!(
            server.take_pending_changelevel().as_deref(),
            Some("e1m2"),
            "first writer wins"
        );
    }

    #[test]
    fn fresh_server_clears_stale_changelevel_request() {
        // A request left in the thread-local must not leak into a freshly built
        // server (Server::new calls reset_changelevel).
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        // Issue a request against one server...
        let mut s1 = Server::new(empty_bsp(), progs).expect("server");
        let t = s1.vm.intern("e1m9");
        s1.vm.set_gi(OFS_PARM0, t);
        bi_changelevel(&mut s1.vm).expect("bi");
        // ...then a new server clears it before the old one ever drained.
        let progs2 = Progs::parse(&img).expect("parse");
        let mut s2 = Server::new(empty_bsp(), progs2).expect("server");
        assert_eq!(
            s2.take_pending_changelevel(),
            None,
            "new server starts with no pending changelevel"
        );
    }

    #[test]
    fn save_spawn_parms_returns_sixteen_floats_via_setchangeparms() {
        // save_spawn_parms runs SetChangeParms (which writes parm1 = g_const) and
        // returns the 16 parm globals. With no client connected it returns zeros
        // and never panics.
        let (img, g_const, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // No client yet -> all zeros, no panic.
        assert_eq!(server.save_spawn_parms(), [0.0; NUM_SPAWN_PARMS]);

        // Connect the player, set the constant SetChangeParms marshals into parm1.
        server.connect_client().expect("connect");
        server.vm.set_gf(g_const, 42.0);
        let parms = server.save_spawn_parms();
        assert_eq!(parms.len(), NUM_SPAWN_PARMS);
        assert_eq!(parms[0], 42.0, "SetChangeParms wrote parm1");
        assert_eq!(&parms[1..], &[0.0; NUM_SPAWN_PARMS - 1]);
    }
}
