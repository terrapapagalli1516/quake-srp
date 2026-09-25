//! Bringing a level up: `SV_SpawnServer` (building the [`Server`]), the local
//! client's `SV_ConnectClient`, `SV_CleanupEnts`, which entities the client is
//! sent (`SV_FatPVS`, `SV_WriteEntitiesToClient`'s test), and the entity
//! dynamic lights a client derives from each edict's `effects` bits.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/sv_main.c` — `SV_SpawnServer` (VM, builtins, globals, map name;
//!   the entity load it calls is `ED_LoadFromFile`, in `pr_edict.rs`),
//!   `SV_ConnectClient`, `SV_CleanupEnts`, `SV_AddToFatPVS` / `SV_FatPVS`,
//!   and the entity filter of `SV_WriteEntitiesToClient`.
//! * `WinQuake/host_cmd.c` — `Host_Spawn_f` (`ClientConnect` +
//!   `PutClientInServer`, folded into the connect).
//! * `WinQuake/cl_main.c` — `CL_RelinkEntities`' `EF_*` dlights
//!   ([`EntityDlight`]); the `EF_*` bits are `quakedef.h`'s.
//!
//! sv_main.c's message senders (`SV_StartSound`, `SV_StartParticle`) live with
//! the rest of the message side in `msg.rs`.

use super::host::{reset_changelevel, reset_restart, reset_skill};
use super::lightstyle::reset_lightstyles;
use super::msg::{reset_message_parsers, take_svc_events};
use super::pr_cmds::install_engine_builtins;
use super::sv_world::link_edict;
use super::{
    parm_global_name, Server, WorldModel, FL_CLIENT, MOVETYPE_NONE, MOVETYPE_PUSH,
    MOVETYPE_WALK, NUM_SPAWN_PARMS, SOLID_BSP, SOLID_NOT, SOLID_SLIDEBOX,
};
use crate::bsp::{Bsp, CONTENTS_SOLID};
use crate::math::{angle_vectors, dot, Vec3};
use crate::progs::Progs;
use crate::vm::Vm;
use crate::Result;

/// `EF_MUZZLEFLASH` (`quakedef.h`): the firing entity emits a brief, bright
/// forward-offset light (`CL_RelinkEntities`).
pub const EF_MUZZLEFLASH: i32 = 2;
/// `EF_BRIGHTLIGHT`: a large light at the entity (+16 z).
pub const EF_BRIGHTLIGHT: i32 = 4;
/// `EF_DIMLIGHT`: a medium light at the entity origin (e.g. the player while
/// quad-damage or with the lightning gun warming).
pub const EF_DIMLIGHT: i32 = 8;

/// One entity dynamic-light contribution for a frame, as enumerated by
/// [`Server::entity_dlights`] (the `EF_*` dlight spawns of `CL_RelinkEntities`).
///
/// A front-end turns each into a [`crate::dlight::DynamicLights::alloc`] call:
/// `alloc(key, origin, radius_base + (rng & 31), now + life, decay=0, minlight,
/// now)`. The `radius_base` excludes the `rand()&31` jitter so this struct stays
/// deterministic; the caller adds the jitter with its own RNG. `decay` is 0 for
/// these lights — they simply expire at `die` (Quake set no decay for the `EF_*`
/// lights; only explosions decay).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityDlight {
    /// Owning entity number; used as the `CL_AllocDlight` reuse key so the light
    /// tracks the entity instead of filling the pool.
    pub key: i32,
    /// World-space light position (already offset for the muzzle / bright cases).
    pub origin: [f32; 3],
    /// Radius in light units *before* the `rand()&31` jitter the caller adds.
    pub radius_base: f32,
    /// Ambient floor (32 for the muzzle flash, 0 otherwise).
    pub minlight: f32,
    /// Seconds until the light dies (`die = now + life`).
    pub life: f32,
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

        let mut vm = Vm::new(progs);
        install_engine_builtins(&mut vm);
        vm.set_host(Box::new(WorldModel::with_pak(bsp, pak)));

        // A deferred changelevel() request is per-thread and outlives a server;
        // clear it so a request issued against a prior level can never leak into
        // this fresh one (mirrors `svs.changelevel_issued = false` in
        // SV_SpawnServer).
        reset_changelevel();
        // Likewise a pending localcmd("restart") respawn must not survive into a
        // freshly spawned server.
        reset_restart();
        // And a half-parsed message / queued MSG_ALL command (an intermission fired
        // on the OLD level must never start one on this fresh server).
        reset_message_parsers();
        let _ = take_svc_events();
        // The light-style transport is also per-thread and outlives a server;
        // clear it so a prior level's patterns cannot leak before this level's
        // worldspawn calls `lightstyle()` (mirrors `SV_SpawnServer` memset of
        // sv.lightstyles).
        reset_lightstyles();
        // The `skill` cvar is process-global (we have no cvar registry); reset it
        // to the single-player default (1, medium) for each fresh server so the
        // spawn filter is deterministic and a prior level's `cvar_set("skill", …)`
        // cannot leak in unexpectedly. A front-end that persists the player's
        // chosen difficulty across a changelevel re-applies it with
        // [`Server::set_skill`] after construction (the same way it carries
        // `serverflags`).
        reset_skill();
        // `sv_gravity` is NOT reset: the C cvar outlives the map
        // (SV_SpawnServer never touches it), and id1's worldspawn sets it on
        // every map (100 on e1m8, 800 elsewhere).

        // Init globals available in this program. The C `SV_SpawnServer` set
        // sv.time = 1.0 before loading entities.
        vm.sv_time = 1.0;
        vm.gset_float("time", 1.0);
        // mapname / world entity defaults are best-effort: only set if present.
        vm.gset_int("world", 0);
        vm.gset_int("self", 0);
        vm.gset_int("other", 0);

        Ok(Server {
            vm,
            entities,
            player: -1,
            lightstyles: std::array::from_fn(|_| String::new()),
            map_name: String::new(),
            client_spawn_parms: [0.0; NUM_SPAWN_PARMS],
            svs_serverflags: 0.0, // Host_Map_f: "haven't completed an episode yet"
            paused: false,        // SV_SpawnServer: sv.paused = false
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
        self.vm.ent_set_string(0, "model", &full);
        self.vm.ent_set_float(0, "modelindex", 1.0); // the world model
        self.vm.ent_set_float(0, "solid", SOLID_BSP as f32);
        self.vm.ent_set_float(0, "movetype", MOVETYPE_PUSH as f32);
        let s = self.vm.intern(&bare);
        self.vm.gset_int("mapname", s);
        // sv.name (strcpy(sv.name, server) in SV_SpawnServer): kept for the
        // savegame header's mapname line (Host_Savegame_f writes sv.name).
        self.map_name = bare;
    }

    /// Spawn the local player and run the connect/spawn entrance script.
    ///
    /// Mirrors `SV_ConnectClient` + `Host_Spawn_f`: reserve a fresh edict, make
    /// it `self`, run `SetNewParms` (fills `parm1..parm16` — the fresh-game
    /// loadout), then `ClientConnect`, then `PutClientInServer` (the QuakeC sets
    /// `origin` from `info_player_start`, plus `health`/`model`/`items`/
    /// `view_ofs`). Marks the edict a walking client (`MOVETYPE_WALK`,
    /// `SOLID_SLIDEBOX`), records the view entity, links it into the world, and
    /// returns its index. QuakeC faults are caught and surfaced, not panicked.
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
            s.run_sys("SetNewParms", ent, 0)?;
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
    /// no-op (`gset_float`).
    pub fn connect_client_with_parms(&mut self, parms: [f32; NUM_SPAWN_PARMS]) -> Result<i32> {
        self.connect_client_inner(move |s, _ent| {
            for (i, v) in parms.iter().enumerate() {
                s.vm.gset_float(&parm_global_name(i), *v);
            }
            Ok(())
        })
    }

    /// Shared body of [`Self::connect_client`] / [`Self::connect_client_with_parms`]:
    /// reserve the player edict, default its physics fields, run `setup_parms`
    /// (the only step that differs — fresh `SetNewParms` vs. restoring saved
    /// parms), then `ClientConnect` + `PutClientInServer`, re-assert physics,
    /// record the view entity, mark `FL_CLIENT`, and link into the world.
    fn connect_client_inner(
        &mut self,
        setup_parms: impl FnOnce(&mut Self, i32) -> Result<()>,
    ) -> Result<i32> {
        // Reserve a fresh edict (the first free slot after spawn_entities).
        let ent = self.vm.spawn();
        self.player = ent;
        // Host_Spawn_f sets up the cleared client edict before ClientConnect:
        // `colormap = NUM_FOR_EDICT(ent)`, `team = (colors & 15) + 1` (cl_color
        // "0") and `netname = host_client->name` (cl_name "player") — the
        // subject of "player entered the game" and every obituary.
        self.vm.ent_set_float(ent, "colormap", ent as f32);
        self.vm.ent_set_float(ent, "team", 1.0);
        self.vm.ent_set_string(ent, "netname", "player");

        // Default the engine-managed physics fields before the script runs, so a
        // minimal mod that only sets health/origin still yields a walking client
        // (the C `SV_SpawnServer` set up the client slot likewise). The QuakeC
        // PutClientInServer normally sets these too.
        self.vm
            .ent_set_float(ent, "movetype", MOVETYPE_WALK as f32);
        self.vm
            .ent_set_float(ent, "solid", SOLID_SLIDEBOX as f32);

        // Establish parm1..parm16 (fresh loadout, or restored saved parms).
        setup_parms(self, ent)?;

        // SV_ConnectClient (sv_main.c): copy the parm globals into the client's
        // spawn_parms right after SetNewParms (or the restored carried set).
        // These are the level-ENTRY parms `Host_Savegame_f` writes into a save.
        for (i, p) in self.client_spawn_parms.iter_mut().enumerate() {
            *p = self.vm.gget_float(&parm_global_name(i));
        }

        // ClientConnect then PutClientInServer (the C runs both with self=player).
        self.run_sys("ClientConnect", ent, 0)?;
        self.run_sys("PutClientInServer", ent, 0)?;

        // Re-assert the engine-managed physics fields if the mod cleared them.
        if self.vm.ent_get_float(ent, "movetype") as i32 == MOVETYPE_NONE {
            self.vm
                .ent_set_float(ent, "movetype", MOVETYPE_WALK as f32);
        }
        if self.vm.ent_get_float(ent, "solid") as i32 == SOLID_NOT {
            self.vm
                .ent_set_float(ent, "solid", SOLID_SLIDEBOX as f32);
        }

        // Record the view entity (what the client looks through). NOTE: real
        // progs.dat has no `viewentity` global, so this write is a no-op there;
        // client identity is carried by the FL_CLIENT flag below instead.
        self.vm.gset_float("viewentity", ent as f32);

        // Mark the edict a client (FL_CLIENT). The C engine sets this when a
        // client connects; monster AI's FindTarget / checkclient look for it.
        let flags = self.vm.ent_get_float(ent, "flags") as i32;
        self.vm
            .ent_set_float(ent, "flags", (flags | FL_CLIENT) as f32);

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
    /// the host's `entity_dlights()` (end of this frame) and the next frame's thinks.
    /// Without this the muzzle light, once lit, tracked the shooter forever.
    pub(super) fn cleanup_ents(&mut self) {
        let n = self.vm.num_edicts();
        for e in 1..n {
            if self.vm.edict_free.get(e).copied().unwrap_or(true) {
                continue;
            }
            let ei = e as i32;
            let eff = self.vm.ent_float(ei, self.vm.fo.effects) as i32;
            if eff & EF_MUZZLEFLASH != 0 {
                self.vm
                    .set_ent_float(ei, self.vm.fo.effects, (eff & !EF_MUZZLEFLASH) as f32);
            }
        }
    }

    /// `SV_FatPVS` (sv_main.c): the union of the PVS of every leaf within 8
    /// units of `org` (`SV_AddToFatPVS`: descend both sides of any node plane
    /// closer than 8; a solid leaf adds nothing), indexed by leaf number. `None`
    /// without a host.
    pub fn fat_pvs(&self, org: Vec3) -> Option<Vec<bool>> {
        let bsp = self.vm.host.as_deref()?.bsp();
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
    /// (edict 0) and free edicts are `false` (a freed edict's `modelindex` is 0),
    /// and so are `makestatic` statics, which the C freed into the signon.
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
        let pvs = if clent > 0 {
            let org = vm.ent_vec(clent, vm.fo.origin);
            let ofs = vm.ent_vec(clent, vm.fo.view_ofs);
            self.fat_pvs([org[0] + ofs[0], org[1] + ofs[1], org[2] + ofs[2]])
        } else {
            None
        };
        for (e, slot) in sent.iter_mut().enumerate().skip(1) {
            let ent = e as i32;
            if vm.is_free_edict(ent) || vm.is_static_edict(ent) {
                continue;
            }
            if ent == clent {
                *slot = true;
                continue;
            }
            // "ignore ents without visible models"
            if vm.ent_float(ent, vm.fo.modelindex) == 0.0 || vm.ent_str(ent, vm.fo.model).is_empty() {
                continue;
            }
            *slot = match &pvs {
                Some(pvs) => vm
                    .edict_leafs(ent)
                    .iter()
                    .any(|&l| pvs.get(usize::from(l)).copied().unwrap_or(false)),
                None => true,
            };
        }
        sent
    }

    /// Enumerate the per-frame entity dynamic-light contributions, porting the
    /// `EF_*` dlight spawns of `CL_RelinkEntities` (`cl_main.c`).
    ///
    /// Scans every in-use edict whose `effects` float field is non-zero and, for
    /// each `EF_MUZZLEFLASH` / `EF_BRIGHTLIGHT` / `EF_DIMLIGHT` bit set, yields an
    /// [`EntityDlight`] describing the light to allocate:
    ///  * `key` = the entity number (so the flash reuses one slot per entity via
    ///    `CL_AllocDlight`),
    ///  * `origin` = the light position (muzzle: `origin.z += 16` then `+ 18 *
    ///    forward(angles)`; brightlight: `origin.z += 16`; dimlight: `origin`),
    ///  * `radius_base` = the radius *before* the `rand()&31` jitter (the caller
    ///    adds it deterministically, keeping this query side-effect-free),
    ///  * `minlight` = the ambient floor (32 for the muzzle flash, else 0),
    ///  * `life` = seconds until the light dies (`die = now + life`).
    ///
    /// The forward vector for the muzzle offset is [`crate::math::angle_vectors`]
    /// (Quake's `AngleVectors`), reusing the same helper the VM `makevectors`
    /// builtin uses. This is a pure query: it never mutates the server, and the
    /// `rand()&31` radius jitter is deliberately left to the caller so the result
    /// is reproducible.
    ///
    /// If one entity has several light bits set, it yields several entries — but
    /// they share the entity's `key`, so `CL_AllocDlight` collapses them into one
    /// slot (the last wins), exactly as the C overwrote the same slot in sequence.
    pub fn entity_dlights(&self) -> Vec<EntityDlight> {
        let mut out = Vec::new();
        let n = self.vm.num_edicts();
        for e in 1..n {
            // edict 0 is the world; skip free edicts.
            if self.vm.edict_free.get(e).copied().unwrap_or(true) {
                continue;
            }
            let ent = e as i32;
            let effects = self.vm.ent_float(ent, self.vm.fo.effects) as i32;
            if effects == 0 {
                continue;
            }
            let origin = self.vm.ent_vec(ent, self.vm.fo.origin);
            let angles = self.vm.ent_vec(ent, self.vm.fo.angles);

            if effects & EF_MUZZLEFLASH != 0 {
                let (forward, _r, _u) = angle_vectors(angles);
                let muzzle = [
                    origin[0] + forward[0] * 18.0,
                    origin[1] + forward[1] * 18.0,
                    origin[2] + 16.0 + forward[2] * 18.0,
                ];
                out.push(EntityDlight {
                    key: ent,
                    origin: muzzle,
                    radius_base: 200.0,
                    minlight: 32.0,
                    life: 0.1,
                });
            }
            if effects & EF_BRIGHTLIGHT != 0 {
                out.push(EntityDlight {
                    key: ent,
                    origin: [origin[0], origin[1], origin[2] + 16.0],
                    radius_base: 400.0,
                    minlight: 0.0,
                    life: 0.001,
                });
            }
            if effects & EF_DIMLIGHT != 0 {
                out.push(EntityDlight {
                    key: ent,
                    origin,
                    radius_base: 200.0,
                    minlight: 0.0,
                    life: 0.001,
                });
            }
        }
        out
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
    use crate::server::testutil::*;
    use crate::server::UserCmd;

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
        assert_eq!(server.vm.ent_get_float(0, "solid"), SOLID_BSP as f32);
        assert_eq!(server.vm.ent_get_float(0, "movetype"), MOVETYPE_PUSH as f32);
        // Its SV_Physics_Pusher pass is inert, and a player still stands on it.
        let p = server.connect_client().expect("connect");
        for _ in 0..10 {
            server.client_frame(&UserCmd::default(), 0.1).expect("frame");
        }
        assert_eq!(server.vm.ent_get_vector(0, "origin"), [0.0; 3]);
        assert!(server.vm.ent_get_float(p, "flags") as i32 & super::super::FL_ONGROUND != 0);
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
        assert_eq!(server.player_edict(), p);
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
        assert!(
            settled[2] >= 24.0 - 1.0,
            "player rests on the floor (origin.z ~ 24), got {}",
            settled[2]
        );
        assert!(
            settled[2] <= 40.0 + 0.1,
            "player did not rise above spawn, got {}",
            settled[2]
        );

        // Drive forward (yaw 0 = +X) and confirm XY advance + no tunnelling.
        let cmd = UserCmd {
            forwardmove: 320.0,
            yaw: 0.0,
            ..UserCmd::default()
        };
        let before = server.vm.ent_get_vector(p, "origin");
        for _ in 0..10 {
            server.client_frame(&cmd, 0.1).expect("walk");
            let o = server.vm.ent_get_vector(p, "origin");
            // The box bottom is origin.z - 24; it must stay at/above the floor.
            assert!(
                o[2] - 24.0 >= -1.0,
                "player did not tunnel through the floor, origin.z = {}",
                o[2]
            );
        }
        let after = server.vm.ent_get_vector(p, "origin");
        assert!(
            after[0] > before[0] + 1.0,
            "player advanced forward in +X: {} -> {}",
            before[0],
            after[0]
        );
    }

    #[test]
    fn entity_dlights_muzzleflash_offsets_forward_and_up() {
        // An entity with EF_MUZZLEFLASH set yields one dlight keyed to the entity,
        // offset +16 z then +18 along its forward (angle) vector, minlight 32.
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let e = server.vm.spawn();
        server.vm.ent_set_vector(e, "origin", [100.0, 200.0, 50.0]);
        // Facing +x (yaw 0, pitch 0): forward = [1,0,0].
        server.vm.ent_set_vector(e, "angles", [0.0, 0.0, 0.0]);
        server.vm.ent_set_float(e, "effects", EF_MUZZLEFLASH as f32);

        let dls = server.entity_dlights();
        assert_eq!(dls.len(), 1, "one muzzleflash dlight");
        let d = dls[0];
        assert_eq!(d.key, e, "keyed to the firing entity");
        assert_eq!(d.minlight, 32.0);
        assert!((d.radius_base - 200.0).abs() < 1e-4, "base radius excludes jitter");
        assert!((d.life - 0.1).abs() < 1e-6);
        // origin + [18,0,0] + [0,0,16] = [118, 200, 66].
        assert!((d.origin[0] - 118.0).abs() < 1e-3, "forward x offset: {:?}", d.origin);
        assert!((d.origin[1] - 200.0).abs() < 1e-3);
        assert!((d.origin[2] - 66.0).abs() < 1e-3);
    }

    #[test]
    fn entity_dlights_brightlight_and_dimlight() {
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        let bright = server.vm.spawn();
        server.vm.ent_set_vector(bright, "origin", [10.0, 20.0, 30.0]);
        server.vm.ent_set_float(bright, "effects", EF_BRIGHTLIGHT as f32);

        let dim = server.vm.spawn();
        server.vm.ent_set_vector(dim, "origin", [40.0, 50.0, 60.0]);
        server.vm.ent_set_float(dim, "effects", EF_DIMLIGHT as f32);

        let dls = server.entity_dlights();
        assert_eq!(dls.len(), 2);

        let b = dls.iter().find(|d| d.key == bright).expect("brightlight");
        assert!((b.radius_base - 400.0).abs() < 1e-4);
        assert_eq!(b.minlight, 0.0);
        assert_eq!(b.origin, [10.0, 20.0, 46.0]); // +16 z
        assert!((b.life - 0.001).abs() < 1e-7);

        let d = dls.iter().find(|d| d.key == dim).expect("dimlight");
        assert!((d.radius_base - 200.0).abs() < 1e-4);
        assert_eq!(d.minlight, 0.0);
        assert_eq!(d.origin, [40.0, 50.0, 60.0]); // origin unchanged
    }

    #[test]
    fn entity_dlights_skips_zero_effects_and_free_edicts() {
        let (img, _f) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // No effects -> no dlight.
        let plain = server.vm.spawn();
        server.vm.ent_set_vector(plain, "origin", [1.0, 2.0, 3.0]);
        server.vm.ent_set_float(plain, "effects", 0.0);

        // A freed edict with effects set must be ignored.
        let gone = server.vm.spawn();
        server.vm.ent_set_float(gone, "effects", EF_DIMLIGHT as f32);
        server.vm.free_edict(gone);

        assert!(server.entity_dlights().is_empty(), "no live lit entities");
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
        let p = server
            .connect_client_with_parms(parms)
            .expect("connect with parms");
        assert_eq!(server.player_edict(), p);

        // The parm1 global holds the value we passed in...
        assert_eq!(
            server.vm.gget_float("parm1"),
            7.5,
            "connect_client_with_parms wrote parm1"
        );
        assert_eq!(server.vm.gget_float("parm4"), 25.0, "and parm4");
        // ...and PutClientInServer (the spawn script) saw it (decoded parm1).
        assert_eq!(
            server.vm.gf(g_decoded),
            7.5,
            "PutClientInServer ran AFTER the parm globals were set"
        );
    }

    /// SV_AddToFatPVS: the PVS of the leaf holding the point, OR'd with every
    /// leaf whose plane is within 8 units; a solid leaf adds nothing.
    #[test]
    fn fat_pvs_unions_the_leaves_within_8_units() {
        use crate::bsp::{DLeaf, DNode, DPlane, CONTENTS_EMPTY, NUM_AMBIENTS};
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
        b.planes = [0.0, 100.0, 200.0]
            .iter()
            .map(|&dist| DPlane { normal: [1.0, 0.0, 0.0], dist, ptype: 0 })
            .collect();
        let node = |planenum, children| DNode { planenum, children, mins: [0; 3], maxs: [0; 3], firstface: 0, numfaces: 0 };
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
