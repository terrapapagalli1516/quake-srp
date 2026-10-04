//! Bringing a level up: `SV_SpawnServer` (building the [`Server`]), the local
//! client's `SV_ConnectClient`, `SV_CleanupEnts`, which entities the client is
//! sent (`SV_FatPVS`, `SV_WriteEntitiesToClient`'s test), and the entities with
//! light effects (their `effects` bits) a client lights.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/sv_main.c` — `SV_SpawnServer` (VM, builtins, globals, map name;
//!   the entity load it calls is `ED_LoadFromFile`, in `pr_edict.rs`),
//!   `SV_ConnectClient`, `SV_CleanupEnts`, `SV_AddToFatPVS` / `SV_FatPVS`,
//!   and the entity filter of `SV_WriteEntitiesToClient`.
//! * `WinQuake/host_cmd.c` — `Host_Spawn_f` (`ClientConnect` +
//!   `PutClientInServer`, folded into the connect).
//! * `WinQuake/cl_main.c` — the entities `CL_RelinkEntities` gives `EF_*` lights
//!   ([`LitEntity`]); the `EF_*` bits are `quakedef.h`'s.
//!
//! sv_main.c's message senders (`SV_StartSound`, `SV_StartParticle`) live with
//! the rest of the message side in `msg.rs`.

use super::pr_cmds::install_engine_builtins;
use super::sv_world::link_edict;
use super::{EntFlags, GameMode, MoveType, NUM_SPAWN_PARMS, Server, Solid, SysFn, WorldModel};
use std::collections::HashMap;

use crate::Result;
use crate::bsp::{Bsp, CONTENTS_SOLID};
use crate::math::{Vec3, dot};
use crate::progs::Progs;
use crate::stepping::Stepping;
use crate::vm::Vm;
use std::rc::Rc;

/// `EF_MUZZLEFLASH` (`quakedef.h`): the firing entity emits a brief, bright
/// forward-offset light (`CL_RelinkEntities`).
pub const EF_MUZZLEFLASH: i32 = 2;
/// `EF_BRIGHTLIGHT`: a large light at the entity (+16 z).
pub const EF_BRIGHTLIGHT: i32 = 4;
/// `EF_DIMLIGHT`: a medium light at the entity origin (e.g. the player while
/// quad-damage or with the lightning gun warming).
pub const EF_DIMLIGHT: i32 = 8;

/// An entity with light effects, as [`Server::lit_entities`] finds it: the
/// number `CL_AllocDlight` keys its light by, where it is and which way it
/// faces, and its `effects` bits (`EF_MUZZLEFLASH` and friends).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LitEntity {
    /// The entity's number (`cl_entities[key]`).
    pub key: i32,
    pub origin: Vec3,
    pub angles: Vec3,
    pub effects: i32,
}

impl Server {
    /// Build a server from a parsed map and program: create the VM, install the
    /// engine builtins, attach the [`WorldModel`] host, and initialise the
    /// well-known globals (`time = 1.0`).
    pub fn new(bsp: Bsp, progs: Progs) -> Result<Server> {
        Server::with_pak(bsp, progs, None)
    }

    /// Like [`new`], but threads a `pak` through to the [`WorldModel`] host so
    /// `setmodel` gives every model file the box `Mod_LoadModel` gave it (the
    /// `maps/b_*.bsp` item boxes collide and take damage; alias models ±16,
    /// sprites ±half their size). The interactive engines (wasm shell,
    /// quaketool) pass `Some(pak)`; the test suite uses [`new`] (`None`) and
    /// keeps a zero box for them.
    pub fn with_pak(bsp: Bsp, progs: Progs, pak: Option<crate::pak::Pak>) -> Result<Server> {
        // Capture the entity text before the BSP moves into the host.
        let entities = bsp.entities.clone();

        let mode = GameMode::detect(&progs);
        let mut vm = Vm::new(progs);
        install_engine_builtins(&mut vm);
        // AUDIT P6/B3: a mission pack's own pak0.pak carries its
        // localization/loc_english.txt (mission_paks.py); read it before
        // `pak` moves into the host. id1 carries none (its progs has no `$`
        // string), so this is `None` for a plain shareware/registered game.
        let loc = pak.as_ref().and_then(crate::localization::load);
        vm.set_host(Box::new(WorldModel::with_pak(bsp, pak)));
        if let Some(loc) = loc {
            vm.set_loc_table(Rc::new(loc));
        }

        // The world model starts with the cvars' defaults (skill 1, sv_gravity
        // 800, `ServerCvars`). A front-end that persists the player's chosen
        // difficulty across a changelevel re-applies it with
        // [`Server::set_skill`] after construction (the same way it carries
        // `serverflags`); id1's worldspawn sets sv_gravity on every map.

        // Init globals available in this program. The C `SV_SpawnServer` set
        // sv.time = 1.0 before loading entities.
        vm.set_sv_time(1.0);
        vm.set_glob_float(vm.go().time, 1.0);
        // mapname / world entity defaults are best-effort: only set if present.
        vm.set_glob_int(vm.go().world, 0);
        vm.set_glob_int(vm.go().self_, 0);
        vm.set_glob_int(vm.go().other, 0);

        Ok(Server {
            vm,
            mode,
            entities,
            player: None,
            lightstyles: std::array::from_fn(|_| String::new()),
            statics: Vec::new(),
            map_name: String::new(),
            client_spawn_parms: [0.0; NUM_SPAWN_PARMS],
            svs_serverflags: 0.0, // Host_Map_f: "haven't completed an episode yet"
            paused: false,        // SV_SpawnServer: sv.paused = false
            stepping: Stepping::Classic,
            ltime_exact: HashMap::new(),
        })
    }

    /// `SV_SpawnServer` (sv_main.c): the parts of the world-edict/globals setup
    /// that need the MAP NAME, which the constructor never sees — the world
    /// edict's `model` field (`"maps/<name>.bsp"`; the QuakeC episode-end check
    /// `world.model == "maps/e1m7.bsp"` in `ExitIntermission` reads it) and the
    /// `mapname` global (the bare name; `samelevel`/`noexit`/`NextLevel` read
    /// it). Call after construction and BEFORE [`Server::spawn_entities`],
    /// exactly where the C set them (worldspawn's entity-lump keys never include
    /// `model`, so the value survives the parse). `name` may be bare (`"e1m7"`)
    /// or a pak path (`"maps/e1m7.bsp"`); both derive the same pair.
    ///
    /// The world edict also gets the rest of SV_SpawnServer's setup:
    /// `modelindex = 1`, `solid = SOLID_BSP`, `movetype = MOVETYPE_PUSH`. The
    /// collision and pusher paths skip edict 0 as the C's do (it is never
    /// linked; SV_PushMove starts at edict 1), and with `nextthink` 0 its
    /// SV_Physics_Pusher pass moves nothing, as in id.
    pub fn set_map_name(&mut self, name: &str) {
        let bare = name.trim_start_matches("maps/").trim_end_matches(".bsp").to_string();
        let full = format!("maps/{bare}.bsp");
        self.vm.set_ent_string(0, self.vm.fo().model, &full);
        self.vm.set_ent_float(0, self.vm.fo().modelindex, 1.0); // the world model
        self.vm.set_solid(0, Solid::Bsp);
        self.vm.set_movetype(0, MoveType::Push);
        let s = self.vm.intern(&bare);
        self.vm.set_glob_int(self.vm.go().mapname, s);
        // sv.name (strcpy(sv.name, server) in SV_SpawnServer): kept for the
        // savegame header's mapname line (Host_Savegame_f writes sv.name).
        self.map_name = bare;
    }

    /// `sv.edicts->v.sounds`: the worldspawn's `sounds` key, the CD track
    /// `SV_SendServerinfo` sends every client that connects (`svc_cdtrack`,
    /// the track twice), as `MSG_WriteByte` stores a float — its `(int)`, as
    /// a byte.
    pub fn cd_track(&self) -> u8 {
        self.vm.ent_float(0, self.vm.fo().sounds) as i32 as u8
    }

    /// Spawn the local player and run the connect/spawn entrance script.
    ///
    /// Mirrors `SV_ConnectClient` + `Host_Spawn_f`: reserve a fresh edict, make
    /// it `self`, run `SetNewParms` (fills `parm1..parm16` — the fresh-game
    /// loadout), then `ClientConnect`, then `PutClientInServer` (the QuakeC sets
    /// `origin` from `info_player_start`, plus `health`/`model`/`items`/
    /// `view_ofs`). Marks the edict a walking client (`MOVETYPE_WALK`,
    /// `SOLID_SLIDEBOX`), links it into the world, and returns its index.
    /// QuakeC faults are caught and surfaced, not panicked.
    /// (The C's `svc_setview` names the client's view entity; the port's client
    /// knows its player edict directly.)
    ///
    /// SINGLE-CLIENT SIMPLIFICATION: the C copies the parm globals into the
    /// `client_t.spawn_parms` after `SetNewParms` and copies them back before
    /// `PutClientInServer`. With exactly one client and no save/load round-trip
    /// that copy is the identity, so we run `SetNewParms` immediately before the
    /// connect/spawn pair and let the parm globals carry straight through.
    pub fn connect_client(&mut self) -> Result<i32> {
        // Fresh game: SetNewParms fills parm1..parm16 with the new-game loadout
        // (shotgun + axe, 100 health), then they pass straight to
        // PutClientInServer (single-client identity copy).
        self.connect_client_inner(|s, ent| {
            s.run_sys(SysFn::SetNewParms, ent, 0)?;
            Ok(())
        })
    }

    /// Spawn the local player carrying *saved* spawn parameters across a level
    /// change. Like [`Self::connect_client`] but, instead of `SetNewParms`
    /// (which would reset the loadout to the fresh-game default), it writes the
    /// 16 saved `parm1..parm16` values into the globals first, then runs
    /// `ClientConnect` + `PutClientInServer` — whose QuakeC `DecodeLevelParms`
    /// reads them back into the player's `items`/`health`/`ammo_*`/`weapon`/
    /// `armorvalue` fields. NET EFFECT: the inventory carries to the new map.
    ///
    /// Mirrors `SV_SpawnServer`'s reconnect path: the engine copies the client's
    /// saved `spawn_parms` back into `pr_global_struct->parm1..16` before calling
    /// `PutClientInServer`. Returns the player edict index. QuakeC faults are
    /// caught and surfaced, not panicked; a missing parm global is a silent
    /// no-op ([`Vm::set_glob_float`]).
    pub fn connect_client_with_parms(&mut self, parms: [f32; NUM_SPAWN_PARMS]) -> Result<i32> {
        self.connect_client_inner(move |s, _ent| {
            for (g, v) in s.vm.go().parms().into_iter().zip(parms) {
                s.vm.set_glob_float(g, v);
            }
            Ok(())
        })
    }

    /// Shared body of [`Self::connect_client`] / [`Self::connect_client_with_parms`]:
    /// reserve the player edict, default its physics fields, run `setup_parms`
    /// (the only step that differs — fresh `SetNewParms` vs. restoring saved
    /// parms), then `ClientConnect` + `PutClientInServer`, re-assert physics,
    /// mark `FL_CLIENT`, and link into the world.
    fn connect_client_inner(&mut self, setup_parms: impl FnOnce(&mut Self, i32) -> Result<()>) -> Result<i32> {
        // Reserve a fresh edict (the first free slot after spawn_entities).
        let ent = self.vm.spawn();
        self.player = Some(ent);
        // Host_Spawn_f sets up the cleared client edict before ClientConnect:
        // `colormap = NUM_FOR_EDICT(ent)`, `team = (colors & 15) + 1` (cl_color
        // "0") and `netname = host_client->name` (cl_name "player") — the
        // subject of "player entered the game" and every obituary.
        self.vm.set_ent_float(ent, self.vm.fo().colormap, ent as f32);
        self.vm.set_ent_float(ent, self.vm.fo().team, 1.0);
        self.vm.set_ent_string(ent, self.vm.fo().netname, "player");

        // Default the engine-managed physics fields before the script runs, so a
        // minimal mod that only sets health/origin still yields a walking client
        // (the C `SV_SpawnServer` set up the client slot likewise). The QuakeC
        // PutClientInServer normally sets these too.
        self.vm.set_movetype(ent, MoveType::Walk);
        self.vm.set_solid(ent, Solid::SlideBox);

        // Establish parm1..parm16 (fresh loadout, or restored saved parms).
        setup_parms(self, ent)?;

        // SV_ConnectClient (sv_main.c): copy the parm globals into the client's
        // spawn_parms right after SetNewParms (or the restored carried set).
        // These are the level-ENTRY parms `Host_Savegame_f` writes into a save.
        self.client_spawn_parms = self.vm.go().parms().map(|g| self.vm.glob_float(g));

        // ClientConnect then PutClientInServer (the C runs both with self=player).
        self.run_sys(SysFn::ClientConnect, ent, 0)?;
        self.run_sys(SysFn::PutClientInServer, ent, 0)?;

        // Re-assert the engine-managed physics fields if the mod cleared them.
        if self.vm.movetype(ent) == MoveType::None {
            self.vm.set_movetype(ent, MoveType::Walk);
        }
        if self.vm.solid(ent) == Solid::Not {
            self.vm.set_solid(ent, Solid::SlideBox);
        }

        // Mark the edict a client (FL_CLIENT). The C engine sets this when a
        // client connects; monster AI's FindTarget / checkclient look for it.
        self.vm.set_flags(ent, self.vm.flags(ent).with(EntFlags::CLIENT));

        // Link into the collision world so absmin/absmax are valid.
        link_edict(&mut self.vm, ent);

        Ok(ent)
    }

    /// `SV_CleanupEnts` (sv_main.c:557): clear the one-frame `EF_MUZZLEFLASH` bit on
    /// every edict. QuakeC's `W_Attack` sets `self.effects |= EF_MUZZLEFLASH` on each
    /// discharge and relies on the engine clearing it the same frame, so the muzzle
    /// dynamic light lasts exactly one frame. The C clears at the END of the frame
    /// (after the client read the bit); this single-process port clears at the START
    /// of the next frame instead — equivalent, since nothing reads `effects` between
    /// the host's `lit_entities()` (end of this frame) and the next frame's thinks.
    /// Without this the muzzle light, once lit, tracked the shooter forever.
    pub(super) fn cleanup_ents(&mut self) {
        let n = self.vm.num_edicts();
        for e in 1..n {
            if self.vm.is_free_edict(e as i32) {
                continue;
            }
            let ei = e as i32;
            let eff = self.vm.ent_float(ei, self.vm.fo().effects) as i32;
            if eff & EF_MUZZLEFLASH != 0 {
                self.vm.set_ent_float(ei, self.vm.fo().effects, (eff & !EF_MUZZLEFLASH) as f32);
            }
        }
    }

    /// `SV_FatPVS` (sv_main.c): the union of the PVS of every leaf within 8
    /// units of `org` (`SV_AddToFatPVS`: descend both sides of any node plane
    /// closer than 8; a solid leaf adds nothing), indexed by leaf number. `None`
    /// without a host.
    pub fn fat_pvs(&self, org: Vec3) -> Option<Vec<bool>> {
        let bsp = self.vm.host()?.bsp();
        let mut fat = vec![false; bsp.leafs.len()];
        if !bsp.nodes.is_empty() {
            let mut budget = bsp.nodes.len() + bsp.leafs.len();
            add_to_fat_pvs(bsp, org, 0, 0, &mut budget, &mut fat);
        }
        Some(fat)
    }

    /// Which edicts `SV_WriteEntitiesToClient` (sv_main.c) would send the local
    /// client this frame, indexed by edict: every entity except the client
    /// itself needs a `modelindex`, a non-empty `model`, and one of the leaves
    /// it touched at its last link (`ent->leafnums`) in the fat PVS at the
    /// client's eye (`origin + view_ofs`); the client is always sent. The world
    /// (edict 0) and free edicts are `false` (a freed edict's `modelindex` is 0).
    ///
    /// On the client, `CL_RelinkEntities` then relinks exactly these, minus any
    /// whose model is null (`modelindex` 0 — possible only for the client
    /// itself): the caller applies that. Without a host nothing is culled
    /// (every in-use edict with a model is sent).
    pub fn entities_sent_to_client(&self) -> Vec<bool> {
        let vm = &self.vm;
        let n = vm.num_edicts();
        let mut sent = vec![false; n];
        let clent = self.player;
        let pvs = clent.and_then(|clent| {
            let org = vm.ent_vec(clent, vm.fo().origin);
            let ofs = vm.ent_vec(clent, vm.fo().view_ofs);
            self.fat_pvs([org[0] + ofs[0], org[1] + ofs[1], org[2] + ofs[2]])
        });
        for (e, slot) in sent.iter_mut().enumerate().skip(1) {
            let ent = e as i32;
            if vm.is_free_edict(ent) {
                continue;
            }
            if Some(ent) == clent {
                *slot = true;
                continue;
            }
            // "ignore ents without visible models"
            if vm.ent_float(ent, vm.fo().modelindex) == 0.0 || vm.ent_str(ent, vm.fo().model).is_empty() {
                continue;
            }
            *slot = match &pvs {
                Some(pvs) => vm.edict_leafs(ent).iter().any(|&l| pvs.get(usize::from(l)).copied().unwrap_or(false)),
                None => true,
            };
        }
        sent
    }

    /// The entities `CL_RelinkEntities` gives light effects to, as the server's
    /// edicts have them: every in-use edict whose `effects` field is not 0
    /// (edict 0 is the world). The client turns each into its lights with
    /// [`crate::dlight::DynamicLights::relink_effects`], the same call a
    /// recorded demo's entities make. A pure query: it never mutates the
    /// server.
    pub fn lit_entities(&self) -> impl Iterator<Item = LitEntity> + '_ {
        (1..self.vm.num_edicts() as i32).filter_map(|key| {
            if self.vm.is_free_edict(key) {
                return None;
            }
            let effects = self.vm.ent_float(key, self.vm.fo().effects) as i32;
            (effects != 0).then(|| LitEntity {
                key,
                origin: self.vm.ent_vec(key, self.vm.fo().origin),
                angles: self.vm.ent_vec(key, self.vm.fo().angles),
                effects,
            })
        })
    }
}

/// `SV_AddToFatPVS` (sv_main.c): OR into `fat` the PVS of every non-solid leaf
/// within 8 units of `org`, from node or leaf `child` down. The plane distance
/// is the full dot product, as the C computes it here (no axial shortcut).
/// Bad indices end the branch. A malformed tree whose nodes loop cannot hang
/// it: `budget` (the caller's `nodes + leafs`, which a real tree never
/// exhausts — each node and leaf is reached once) counts every node and leaf
/// visited, the one-sided descents of the loop included, and the depth bound
/// keeps the recursion off the end of the stack.
fn add_to_fat_pvs(bsp: &Bsp, org: Vec3, mut child: i32, depth: usize, budget: &mut usize, fat: &mut [bool]) {
    if depth > crate::bsp::TOUCHED_LEAFS_MAX_DEPTH {
        return;
    }
    loop {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if child < 0 {
            let leaf = (-1 - child) as usize;
            if bsp.leafs.get(leaf).is_some_and(|l| l.contents != CONTENTS_SOLID) {
                for (f, v) in fat.iter_mut().zip(bsp.leaf_pvs(leaf)) {
                    *f |= v;
                }
            }
            return;
        }
        let Some(node) = bsp.nodes.get(child as usize) else { return };
        let Some(plane) = usize::try_from(node.planenum).ok().and_then(|p| bsp.planes.get(p)) else {
            return;
        };
        let d = dot(org, plane.normal) - plane.dist;
        if d > 8.0 {
            child = i32::from(node.children[0]);
        } else if d < -8.0 {
            child = i32::from(node.children[1]);
        } else {
            // go down both
            add_to_fat_pvs(bsp, org, i32::from(node.children[0]), depth + 1, budget, fat);
            child = i32::from(node.children[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::UserCmd;
    use crate::server::testutil::*;

    #[test]
    fn set_map_name_sets_up_the_world_edict_like_sv_spawnserver() {
        // CENSUS L18: SV_SpawnServer gives edict 0 model = the map,
        // modelindex 1, SOLID_BSP, MOVETYPE_PUSH (id's oracle dump: worldspawn
        // movetype 7, solid 4).
        let (img, c100, org) = player_progs();
        let mut server = Server::new(floor_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        prime_player_globals(&mut server, c100, org);
        server.set_map_name("e1m1");
        assert_eq!(server.vm.ent_get_string(0, "model"), "maps/e1m1.bsp");
        assert_eq!(server.vm.ent_get_float(0, "modelindex"), 1.0);
        assert_eq!(server.vm.solid(0), Solid::Bsp);
        assert_eq!(server.vm.movetype(0), MoveType::Push);
        // Its SV_Physics_Pusher pass is inert, and a player still stands on it.
        let p = server.connect_client().expect("connect");
        for _ in 0..10 {
            server.client_frame(&UserCmd::default(), 0.1).expect("frame");
        }
        assert_eq!(server.vm.ent_get_vector(0, "origin"), [0.0; 3]);
        assert!(server.vm.flags(p).contains(EntFlags::ONGROUND));
    }

    #[test]
    fn connect_client_spawns_player_and_walks_without_tunnelling() {
        // The minimal progs sets health=100 and origin=(0,0,40) in
        // PutClientInServer. After connect the player has health 100 and a sane
        // origin; a forward usercmd advances it in XY and never sinks through the
        // floor at z = 0.
        let (img, g_const100, g_origin) = player_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);

        let p = server.connect_client().expect("connect");
        assert_eq!(server.player_edict(), Some(p));
        assert_eq!(server.player_health(), 100.0, "PutClientInServer set health");

        // The QuakeC spawn set origin to (0,0,40); give it a player box.
        let origin0 = server.vm.ent_get_vector(p, "origin");
        assert_eq!(origin0, [0.0, 0.0, 40.0], "spawn origin applied");
        server.vm.ent_set_vector(p, "mins", [-16.0, -16.0, -24.0]);
        server.vm.ent_set_vector(p, "maxs", [16.0, 16.0, 32.0]);
        // Settle onto the floor: a few empty frames let gravity + walk_move drop
        // it until the box bottom rests on z = 0 (origin.z ~ 24).
        let still = UserCmd::default();
        for _ in 0..20 {
            server.client_frame(&still, 0.1).expect("settle");
        }
        let settled = server.vm.ent_get_vector(p, "origin");
        assert!(settled[2] >= 24.0 - 1.0, "player rests on the floor (origin.z ~ 24), got {}", settled[2]);
        assert!(settled[2] <= 40.0 + 0.1, "player did not rise above spawn, got {}", settled[2]);

        // Drive forward (yaw 0 = +X) and confirm XY advance + no tunnelling.
        let cmd = UserCmd { forwardmove: 320.0, yaw: 0.0, ..UserCmd::default() };
        let before = server.vm.ent_get_vector(p, "origin");
        for _ in 0..10 {
            server.client_frame(&cmd, 0.1).expect("walk");
            let o = server.vm.ent_get_vector(p, "origin");
            // The box bottom is origin.z - 24; it must stay at/above the floor.
            assert!(o[2] - 24.0 >= -1.0, "player did not tunnel through the floor, origin.z = {}", o[2]);
        }
        let after = server.vm.ent_get_vector(p, "origin");
        assert!(after[0] > before[0] + 1.0, "player advanced forward in +X: {} -> {}", before[0], after[0]);
    }

    #[test]
    fn lit_entities_are_the_in_use_edicts_with_effects() {
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let flash = server.vm.spawn();
        server.vm.ent_set_vector(flash, "origin", [100.0, 200.0, 50.0]);
        server.vm.ent_set_vector(flash, "angles", [10.0, 20.0, 0.0]);
        server.vm.ent_set_float(flash, "effects", (EF_MUZZLEFLASH | EF_DIMLIGHT) as f32);

        // No effects: not lit.
        let plain = server.vm.spawn();
        server.vm.ent_set_vector(plain, "origin", [1.0, 2.0, 3.0]);
        server.vm.ent_set_float(plain, "effects", 0.0);

        // A freed edict with effects set must be ignored.
        let gone = server.vm.spawn();
        server.vm.ent_set_float(gone, "effects", EF_DIMLIGHT as f32);
        server.vm.free_edict(gone);

        let lit: Vec<_> = server.lit_entities().collect();
        assert_eq!(
            lit,
            vec![LitEntity {
                key: flash,
                origin: [100.0, 200.0, 50.0],
                angles: [10.0, 20.0, 0.0],
                effects: EF_MUZZLEFLASH | EF_DIMLIGHT,
            }]
        );
    }

    #[test]
    fn connect_client_with_parms_writes_parm_globals_before_spawn() {
        // connect_client_with_parms must write the supplied parms into the
        // parm1..parm16 globals BEFORE running PutClientInServer (so
        // DecodeLevelParms sees them). The progs' PutClientInServer copies parm1
        // into `decoded`, proving the parm was live when the spawn script ran.
        let (img, _gc, g_decoded) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let mut parms = [0.0f32; NUM_SPAWN_PARMS];
        parms[0] = 7.5; // parm1
        parms[3] = 25.0; // parm4 (e.g. shells)
        let p = server.connect_client_with_parms(parms).expect("connect with parms");
        assert_eq!(server.player_edict(), Some(p));

        // The parm1 global holds the value we passed in...
        assert_eq!(server.vm.gget_float("parm1"), 7.5, "connect_client_with_parms wrote parm1");
        assert_eq!(server.vm.gget_float("parm4"), 25.0, "and parm4");
        // ...and PutClientInServer (the spawn script) saw it (decoded parm1).
        assert_eq!(server.vm.gf(g_decoded), 7.5, "PutClientInServer ran AFTER the parm globals were set");
    }

    /// SV_AddToFatPVS: the PVS of the leaf holding the point, OR'd with every
    /// leaf whose plane is within 8 units; a solid leaf adds nothing.
    #[test]
    fn fat_pvs_unions_the_leaves_within_8_units() {
        use crate::bsp::{CONTENTS_EMPTY, DLeaf, DNode, DPlane, NUM_AMBIENTS};
        let leaf = |contents, visofs| DLeaf {
            contents,
            visofs,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; NUM_AMBIENTS],
        };
        // x < 0: leaf 1 (sees 1); 0 <= x < 100: leaf 2 (sees 2, 3);
        // x >= 100: leaf 3 (sees 2, 3); x >= 200: solid leaf 4.
        let mut b = empty_bsp();
        b.planes = [0.0, 100.0, 200.0].iter().map(|&dist| DPlane { normal: [1.0, 0.0, 0.0], dist, ptype: 0 }).collect();
        let node =
            |planenum, children| DNode { planenum, children, mins: [0; 3], maxs: [0; 3], firstface: 0, numfaces: 0 };
        b.nodes = vec![node(0, [1, -2]), node(1, [2, -3]), node(2, [-5, -4])];
        b.leafs = vec![
            leaf(CONTENTS_SOLID, -1),
            leaf(CONTENTS_EMPTY, 0),
            leaf(CONTENTS_EMPTY, 1),
            leaf(CONTENTS_EMPTY, 1),
            leaf(CONTENTS_SOLID, 2),
        ];
        b.visibility = vec![0b0001, 0b0110, 0b1111];
        let fat = |b: &Bsp, x: f32| {
            let mut f = vec![false; b.leafs.len()];
            let mut budget = b.nodes.len() + b.leafs.len();
            add_to_fat_pvs(b, [x, 0.0, 0.0], 0, 0, &mut budget, &mut f);
            (0..f.len()).filter(|&l| f[l]).collect::<Vec<_>>()
        };
        assert_eq!(fat(&b, -50.0), vec![1]);
        assert_eq!(fat(&b, -8.5), vec![1], "just over 8 units away");
        assert_eq!(fat(&b, -8.0), vec![1, 2, 3], "within 8: both sides");
        assert_eq!(fat(&b, 50.0), vec![2, 3]);
        assert_eq!(fat(&b, 196.0), vec![2, 3], "the solid leaf beyond adds nothing");
        // A malformed tree whose nodes loop ends (bad maps only): node 1's
        // front child is node 0, which sends x = 150 back to node 1 forever
        // (one-sided, the loop's own step), and node 2 at x = 200 recurses
        // into itself on both sides.
        let mut cyclic = b.clone();
        cyclic.nodes[1].children = [0, -3];
        assert_eq!(fat(&cyclic, 150.0), Vec::<usize>::new(), "gives up, nothing added");
        let mut both = b.clone();
        both.nodes[2].children = [2, 2];
        assert_eq!(fat(&both, 200.0), Vec::<usize>::new());
    }
}
