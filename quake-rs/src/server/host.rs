//! Host-side state around the server: the `skill` and `sv_gravity` cvars
//! ([`ServerCvars`]), the deferred `changelevel` / `restart` commands, spawn
//! parms and serverflags across a level change, `kill`, and the signon settle
//! frames.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/host_cmd.c` — `Host_Changelevel_f` / `Host_Restart_f` (the
//!   console commands `PF_changelevel` / `PF_localcmd` defer through
//!   `Cbuf_AddText`), `Host_Kill_f`, `Host_Spawn_f` / `Host_Begin_f` (the
//!   signon frames).
//! * `WinQuake/sv_main.c` — `SV_SaveSpawnparms`, and `SV_SpawnServer`'s
//!   `skill` → `current_skill` rounding.
//! * `WinQuake/pr_cmds.c` — `PF_changelevel`, `PF_localcmd`.
//!
//! This port has no console, command buffer or cvar registry, so the pieces of
//! host state QuakeC can reach live on the server — the cvars on its world
//! model, the commands in its [`Outbox`] — and the front-end (wasm shell,
//! quaketool) plays `Host_Frame`'s part through the `Server` methods below.

use std::rc::Rc;

use super::{Outbox, Server, SysFn, UserCmd, NUM_SPAWN_PARMS, SETTLE_FRAMETIME, SV_GRAVITY};
use crate::qrand::QRand;
use crate::vm::Vm;
use crate::Result;

// ---------------------------------------------------------------------------
// The server's cvars.
// ---------------------------------------------------------------------------

/// The engine cvars the server's QuakeC reads (`PF_cvar`) and sets
/// (`PF_cvar_set`) that the port gives a live value: `skill`, `sv_gravity`
/// and `registered`.
///
/// id kept them in the console's cvar registry, which outlives a server. The
/// port has no registry yet (CODE_PLAN R4's typed `Cvars` will be it), so each
/// server keeps its own on its world model, where the builtins reach them
/// ([`crate::vm::Host::cvars`]); a new server starts from the defaults, and the
/// front-end carries a value across a level change where it matters (the
/// difficulty, [`Server::skill`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServerCvars {
    /// `current_skill` (sv_main.c): 0 easy, 1 medium, 2 hard, 3 nightmare. The
    /// start map's difficulty portals (`trigger_setskill`) set it with
    /// `cvar_set("skill", N)`, the spawn filter reads it, and `cvar("skill")`
    /// returns it.
    pub skill: i32,
    /// `sv_gravity` (sv_phys.c, "800"). world.qc's `worldspawn` sets 100 on
    /// e1m8 (Ziggurat Vertigo) and 800 on every other map; `SV_AddGravity`
    /// and `SV_Physics_Step`'s landing-sound threshold read it.
    pub sv_gravity: f32,
    /// `registered` (common.c, "0"): `COM_CheckRegistered` set it at startup
    /// from the search path, so a server takes it from the path it reads
    /// ([`crate::common::is_registered`]). The QuakeC reads it with
    /// `cvar("registered")`: `trigger_onlyregistered` (the start map's
    /// episode gates) and `ExitIntermission` (episode 1's end: the next
    /// episode, or the order screen).
    pub registered: bool,
    /// NOT a QuakeC-visible cvar — `cvar()`/`cvar_set()` never reach this
    /// one, unlike the three above. It is engine state for the mission
    /// packs' re-release-only builtin `finaleFinished` (#79,
    /// `server::pr_cmds::bi_finale_finished`), kept here only because this is
    /// what a builtin can already reach ([`crate::vm::Host::cvars`]). A
    /// front-end sets it once the end-of-pack finale/credits text is fully
    /// shown (`screen::finale_text_fully_revealed`) and the player has
    /// pressed a button since (`client/cl_main.rs`'s `walk_frame`, via
    /// [`Server::set_finale_finished`]); it only ever latches true — see
    /// that method — so a stray poll before the press keeps seeing false.
    /// `id1`'s progs never declares the builtin, so this never matters there.
    pub finale_finished: bool,
}

impl Default for ServerCvars {
    /// The cvars' defaults: `skill` "1" (single-player medium), `sv_gravity`
    /// "800", `registered` "0", `finale_finished` false (a fresh level has
    /// not shown, let alone finished, any finale text).
    fn default() -> Self {
        ServerCvars { skill: 1, sv_gravity: SV_GRAVITY, registered: false, finale_finished: false }
    }
}

impl ServerCvars {
    /// Set `skill` from a raw value the way `SV_SpawnServer` turns the cvar
    /// into `current_skill`: `(int)(value + 0.5)`, clamped to `0..=3`.
    pub fn set_skill(&mut self, value: f32) {
        self.skill = ((value + 0.5) as i32).clamp(0, 3);
    }
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
// the requested map name in the server's outbox; the front-end takes it after
// `client_frame` returns (via [`Server::take_pending_changelevel`]) and performs
// the swap itself, never inside the builtin call.
// ---------------------------------------------------------------------------

impl Outbox {
    /// Record a deferred level change to `map`. The first request wins until
    /// the host takes it, as the C's `svs.changelevel_issued` guard drops a
    /// second `PF_changelevel` until the swap completes.
    fn request_changelevel(&mut self, map: String) {
        self.changelevel.get_or_insert(map);
    }

    /// Drop a `changelevel` or `restart` a *prior* frame left untaken: a
    /// well-behaved front-end takes it at once, but a stale request must
    /// never swap or respawn a frame late or against the wrong level.
    pub(super) fn clear_requests(&mut self) {
        self.changelevel = None;
        self.restart = false;
    }
}

/// `PF_changelevel` (#70): `void(string s) changelevel`. The C looked up its
/// string argument, guarded against a double issue, and deferred the actual swap
/// via `Cbuf_AddText("changelevel <s>")`. We faithfully *only* record the map
/// name here (PARM0, the `string_t` of the destination map, e.g. `"e1m2"`); the
/// front-end performs the swap after the frame. Never swaps inline.
pub(super) fn bi_changelevel(vm: &mut Vm) -> Result<()> {
    let map = vm.arg_string(0);
    vm.with_host(|_, h| h.outbox().request_changelevel(map));
    Ok(())
}

/// `PF_localcmd` (#46): `void(string s) localcmd` — `Cbuf_AddText(s)`, i.e. QuakeC
/// pushing a console command. Most are host/diagnostic and irrelevant to this port,
/// but single-player gameplay issues a few level-control commands we MUST honour:
///   * `restart` — reload the current level (the death-respawn path, `client.qc`).
///   * `changelevel <map>` / `map <map>` — defer a level swap (same as PF_changelevel).
///   * `menu_credits` — the mission packs' re-release-only end-of-game credits
///     roll ([`Outbox::menu_credits`]); the `disconnect` that always follows
///     it in the same QuakeC frame needs no handler of its own (see there).
///
/// Everything else is a benign no-op (matching the old behaviour). The token parse
/// is whitespace-split and case-insensitive on the command word.
pub(super) fn bi_localcmd(vm: &mut Vm) -> Result<()> {
    let cmd = vm.arg_string(0);
    let mut it = cmd.split_whitespace();
    let word = it.next().map(str::to_ascii_lowercase);
    let map = it.next().map(str::to_string);
    vm.with_host(|_, h| {
        let outbox = h.outbox();
        match (word.as_deref(), map) {
            (Some("restart"), _) => outbox.restart = true,
            (Some("changelevel" | "map"), Some(map)) => outbox.request_changelevel(map),
            (Some("menu_credits"), _) => outbox.menu_credits = true,
            _ => {} // other console text (e.g. the paired "disconnect"): benign no-op.
        }
    });
    Ok(())
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
    /// moves/buttons. A program error in them is `Host_Error`, as in id's: it
    /// is returned, and the level does not come up. The golden `scene` tool
    /// never connects a client, so this does not affect golden renders.
    pub fn run_signon_frames(&mut self) -> Result<()> {
        let (yaw, pitch) = self.player.map_or((0.0, 0.0), |p| {
            let ang = self.vm.ent_vec(p, self.vm.fo().angles);
            let vang = self.vm.ent_vec(p, self.vm.fo().v_angle);
            (ang[1], vang[0])
        });
        let cmd = UserCmd {
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            yaw,
            pitch,
            buttons: 0,
            impulse: 0,
        };
        self.client_frame_f64(&cmd, SETTLE_FRAMETIME)?;
        self.client_frame_f64(&cmd, SETTLE_FRAMETIME)?;
        Ok(())
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
    /// a missing individual parm global reads as `0.0` ([`Vm::glob_float`]), so
    /// this never panics. Returns `[0.0; 16]` when no client has connected, and
    /// the program error if `SetChangeParms` fails (id's `Host_Error`).
    pub fn save_spawn_parms(&mut self) -> Result<[f32; NUM_SPAWN_PARMS]> {
        let Some(player) = self.player else {
            return Ok([0.0; NUM_SPAWN_PARMS]);
        };
        // SetChangeParms writes parm1..parm16 from the player's live fields
        // (self = the player edict, other = world).
        self.run_sys(SysFn::SetChangeParms, player, 0)?;
        Ok(self.vm.go().parms().map(|g| self.vm.glob_float(g)))
    }

    /// Read the `serverflags` QuakeC global (the episode rune `SERVERFLAG_*`
    /// bits the player carries between levels). Returns `0.0` if the progs has
    /// no such global. The C keeps `pr_global_struct->serverflags` alive across
    /// `SV_SpawnServer`; a front-end driving a changelevel reads it from the
    /// outgoing server and writes it into the incoming one with
    /// [`Self::set_serverflags`] so the runes are not lost each level.
    pub fn serverflags(&self) -> f32 {
        self.vm.glob_float(self.vm.go().serverflags)
    }

    /// `SV_SpawnServer`'s `pr_global_struct->serverflags = svs.serverflags`: set
    /// the carried rune bits (`svs.serverflags`) and write them into the QuakeC
    /// global. Call before [`Self::spawn_entities`]. A no-op on the global if the
    /// progs lacks it. See [`Self::serverflags`].
    pub fn set_serverflags(&mut self, flags: f32) {
        self.svs_serverflags = flags;
        self.vm.set_glob_float(self.vm.go().serverflags, flags);
    }

    /// `svs.serverflags`: the rune bits this level was entered with — what a
    /// `restart` (`Host_Restart_f` -> `SV_SpawnServer`, no `SV_SaveSpawnparms`)
    /// respawns with, whatever the live global says now.
    pub fn level_entry_serverflags(&self) -> f32 {
        self.svs_serverflags
    }

    /// The current integer skill level (0=easy, 1=medium, 2=hard, 3=nightmare).
    ///
    /// This is the `current_skill` the spawn filter uses and the value
    /// `cvar("skill")` returns to the QuakeC. The difficulty portals in the start
    /// map (`trigger_setskill`) change it at runtime via `cvar_set("skill", N)`;
    /// a front-end reads it here to persist the player's choice across a
    /// changelevel (the constructor resets it to the medium default).
    pub fn skill(&self) -> i32 {
        self.cvars().skill
    }

    /// Set the skill level from a raw value, normalised exactly as
    /// `SV_SpawnServer` does (`current_skill = (int)(value + 0.5)`, clamped to
    /// `0..=3`). A front-end calls this after construction to apply the menu's /
    /// the persisted difficulty before [`Self::spawn_entities`], so the spawn
    /// filter inhibits the right monsters/items. See [`Self::skill`].
    pub fn set_skill(&mut self, value: f32) {
        if let Some(c) = self.cvars_mut() {
            c.set_skill(value);
        }
    }

    /// The live `sv_gravity` cvar (800, or 100 on e1m8 — world.qc `worldspawn`).
    /// The client side reads it too: `R_DrawParticles`' particle gravity is
    /// `sv_gravity * 0.05`.
    pub fn sv_gravity(&self) -> f32 {
        self.cvars().sv_gravity
    }

    /// Set the `sv_gravity` cvar: `Cvar_Set`, which id's cvar outlives the
    /// map for — a front-end carries it to the next level's server as it
    /// carries [`Self::skill`] (id1's worldspawn sets it on every map anyway).
    pub fn set_sv_gravity(&mut self, value: f32) {
        if let Some(c) = self.cvars_mut() {
            c.sv_gravity = value;
        }
    }

    /// What the mission packs' re-release `finaleFinished` builtin (#79)
    /// returns — see [`ServerCvars::finale_finished`].
    pub fn finale_finished(&self) -> bool {
        self.cvars().finale_finished
    }

    /// Latch [`Self::finale_finished`] true. A front-end calls this every
    /// frame with its own freshly-computed condition (the finale text fully
    /// revealed AND a button pressed since); passing `false` is a no-op —
    /// once latched, only a changelevel/restart's fresh server (a new
    /// [`ServerCvars::default`]) clears it, matching `finale_check`'s think
    /// (client.qc) needing to see `true` only once, however its 0.1s polls
    /// happen to land against the player's one dismiss press.
    pub fn set_finale_finished(&mut self, finished: bool) {
        if finished {
            if let Some(c) = self.cvars_mut() {
                c.finale_finished = true;
            }
        }
    }

    /// The live `ED_Alloc` ceiling ([`crate::vm::Vm::max_edicts`]): id's
    /// `MAX_EDICTS` (600) unless [`Self::set_max_edicts`] raised it.
    pub fn max_edicts(&self) -> usize {
        self.vm.max_edicts()
    }

    /// Raise (or restore) the edict ceiling — the 2026-only `sv_max_edicts`
    /// cvar's engine side. Call before [`Self::spawn_entities`] (this is
    /// `SV_SpawnServer` sizing `sv.edicts`, just with a port whose edict
    /// storage already grows on demand — see `crate::vm::Vm::set_max_edicts`
    /// for the clamp and why it is a floor, not a ceiling, on id's 600). A
    /// front-end carries this to the next level's server exactly as it
    /// carries [`Self::skill`] and [`Self::sv_gravity`] — see `host_cmd.rs`'s
    /// `try_changelevel`/`try_restart`.
    pub fn set_max_edicts(&mut self, n: usize) {
        self.vm.set_max_edicts(n);
    }

    /// Draw from the host session's random streams ([`QRand`]) from now on.
    /// A front-end hands each server it builds its session's, before
    /// [`Self::spawn_entities`], so the streams continue across level loads
    /// as id's one libc `rand()` does; a server it is not handed to draws
    /// from fresh streams of its own.
    pub fn set_rand(&mut self, rand: Rc<QRand>) {
        self.vm.set_rand(rand);
    }

    /// The random streams this server draws from, to hand to the next one.
    pub fn rand(&self) -> &Rc<QRand> {
        self.vm.rand()
    }

    /// This server's [`ServerCvars`] (the defaults if its world model were
    /// taken away).
    pub(super) fn cvars(&self) -> ServerCvars {
        self.vm.host().map(|h| *h.cvars()).unwrap_or_default()
    }

    /// This server's [`ServerCvars`], to set.
    fn cvars_mut(&mut self) -> Option<&mut ServerCvars> {
        self.vm.host_mut().map(|h| h.cvars_mut())
    }

    /// Take (and clear) the deferred level-change request a `changelevel()`
    /// builtin recorded this frame, or `None` if none was issued. A front-end
    /// calls this once after [`Self::client_frame`]: when it returns `Some(map)`,
    /// the front-end saves the spawn parms, loads `map`, and reconnects the
    /// client carrying its inventory. Mirrors the engine processing the deferred
    /// `changelevel <map>` console command after the frame.
    pub fn take_pending_changelevel(&mut self) -> Option<String> {
        self.outbox()?.changelevel.take()
    }

    /// Take and clear a pending single-player respawn (`localcmd("restart")`). The
    /// front-end calls this once after [`Self::client_frame`]: when it returns
    /// `true`, it reloads the CURRENT level (carrying the level-entry spawn parms,
    /// not the dead player's state). Mirrors the engine running the deferred
    /// `restart` console command after the frame.
    pub fn take_pending_restart(&mut self) -> bool {
        self.take_outbox(|o| &mut o.restart)
    }

    /// Take and clear a pending `menu_credits` (the mission packs'
    /// re-release-only end-of-game credits roll, `localcmd("menu_credits\n")`
    /// — [`Outbox::menu_credits`]). A front-end that sees `true` ends the
    /// session the same way the Quit menu does (`id1` never calls this).
    pub fn take_pending_menu_credits(&mut self) -> bool {
        self.take_outbox(|o| &mut o.menu_credits)
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
        let Some(player) = self.live_player() else {
            return Ok(false);
        };
        if self.vm.ent_float(player, self.vm.fo().health) <= 0.0 {
            return Ok(false); // "Can't suicide -- allready dead!"
        }
        // pr_global_struct->time = sv.time; self = sv_player; run ClientKill.
        let t = self.time();
        self.vm.set_glob_float(self.vm.go().time, t);
        self.run_sys(SysFn::ClientKill, player, 0)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Op, Progs, Statement, OFS_PARM0};
    use crate::server::testutil::*;
    use crate::server::EntFlags;

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

        server.run_signon_frames().expect("signon frames");

        // Settled BEFORE the front-end's frame 0: on the ground, no residual
        // fall velocity, box bottom resting on the floor (origin.z ~ 24).
        let org = server.vm.ent_get_vector(p, "origin");
        let vel = server.vm.ent_get_vector(p, "velocity");
        let flags = server.vm.flags(p);
        assert!(flags.contains(EntFlags::ONGROUND), "player is on the ground at frame 0");
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
            op: Op::Done,
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
                    op: Op::StoreS,
                    a: g_str as i16,
                    b: OFS_PARM0 as i16,
                    c: 0,
                },
                Statement {
                    op: Op::Call1,
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
        // A request left in one server's outbox never reaches a freshly built
        // server.
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
        assert_eq!(server.save_spawn_parms().expect("no client"), [0.0; NUM_SPAWN_PARMS]);

        // Connect the player, set the constant SetChangeParms marshals into parm1.
        server.connect_client().expect("connect");
        server.vm.set_gf(g_const, 42.0);
        let parms = server.save_spawn_parms().expect("SetChangeParms");
        assert_eq!(parms.len(), NUM_SPAWN_PARMS);
        assert_eq!(parms[0], 42.0, "SetChangeParms wrote parm1");
        assert_eq!(&parms[1..], &[0.0; NUM_SPAWN_PARMS - 1]);
    }
}
