//! The server→client message side, with no network: the [`Outbox`] the
//! builtins write into and the `Write*` recognisers a front-end drains every
//! frame.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/sv_main.c` — `SV_StartSound`, `SV_StartParticle`.
//! * `WinQuake/pr_cmds.c` — `PF_sound`, `PF_ambientsound`, `PF_particle`,
//!   `PF_bprint`/`PF_sprint`/`PF_centerprint`, and the `PF_Write*` family
//!   (`MSG_Write*` into `WriteDest()`).
//! * What the client made of those bytes: `WinQuake/cl_tent.c`
//!   (`CL_ParseTEnt`, the temp entities) and `WinQuake/cl_parse.c`
//!   (`CL_ParseServerMessage`, the `MSG_ALL` svc commands).
//!
//! In the C each of these became bytes in a client datagram, the signon or the
//! reliable buffer. This single-process server has no netcode, so each lands
//! in the server's [`Outbox`] instead — [`SoundEvent`], [`StaticSound`],
//! [`ParticleBurst`], [`GameMessage`], [`TempEntityEvent`], [`SvcEvent`] —
//! which the `Server::drain_*` methods below hand to the front-end.

use super::Server;
use crate::vm::Vm;
use crate::Result;

// ---------------------------------------------------------------------------
// The outbox.
// ---------------------------------------------------------------------------

/// Everything a server's QuakeC sent out of the server since the host last
/// looked: what id's C wrote into the client's buffers (`sv.datagram`,
/// `sv.reliable_datagram`, `sv.signon`, the client's `message`), and the
/// console text it queued for the host.
///
/// One per server, on its [`super::WorldModel`]: the builtins are
/// `fn(&mut Vm)` and reach it through [`Vm::with_host`] ([`Host::outbox`]);
/// the server's physics writes it the same way, and the `Server::drain_*`
/// methods empty it for the front-end. A new server starts with an empty one,
/// so nothing one level queued can reach the next.
#[derive(Debug, Default)]
pub struct Outbox {
    /// `svc_sound`s ([`Server::drain_sounds`]).
    sounds: Vec<SoundEvent>,
    /// The signon's `svc_spawnstaticsound`s ([`Server::drain_static_sounds`]).
    static_sounds: Vec<StaticSound>,
    /// `svc_particle`s ([`Server::drain_particles`]).
    particles: Vec<ParticleBurst>,
    /// `svc_print`s and `svc_centerprint`s ([`Server::drain_messages`]).
    messages: Vec<GameMessage>,
    /// Where the client's parse of each buffer the `Write*` builtins feed is
    /// (one per [`MsgBuf`]); reset at the top of every server frame.
    parsers: [MsgParse; 2],
    /// Temp entities those buffers completed ([`Server::drain_temp_entities`]).
    temp_entities: Vec<TempEntityEvent>,
    /// `svc_*` commands those buffers completed ([`Server::drain_svc_events`]).
    svc_events: Vec<SvcEvent>,
    /// `svc_stufftext`s, as `(client entity, text)` ([`Server::drain_stufftext`]).
    stufftext: Vec<(i32, String)>,
    /// A `changelevel <map>` the QuakeC queued for the host's command buffer
    /// (`PF_changelevel`, `localcmd`; [`Server::take_pending_changelevel`]).
    pub(super) changelevel: Option<String>,
    /// A `restart` the QuakeC queued for the host's command buffer
    /// (`localcmd("restart\n")`; [`Server::take_pending_restart`]).
    pub(super) restart: bool,
    /// `svc_lightstyle`s, `(style, pattern)`, not yet applied to the server's
    /// table (`Server::apply_lightstyles`).
    pub(super) lightstyles: Vec<(usize, String)>,
    /// A `menu_credits` the QuakeC queued for the host's command buffer
    /// (`localcmd("menu_credits\n")`; [`Server::take_pending_menu_credits`]).
    /// The mission packs' re-release-only end-of-game credits roll
    /// (`finale_transition`/`finale_6`, client.qc/ending.qc/oldone.qc), always
    /// immediately followed by `localcmd("disconnect\n")` in the same QuakeC
    /// frame — a front-end that ends the session on `menu_credits` (as the
    /// Quit menu does) need not separately recognise the `disconnect` that
    /// follows it; `id1` never calls either.
    pub(super) menu_credits: bool,
}

impl Server {
    /// This server's [`Outbox`]. Its world model always has one; `None` only
    /// if the VM's host were taken away.
    pub(super) fn outbox(&mut self) -> Option<&mut Outbox> {
        self.vm.host_mut().map(|h| h.outbox())
    }

    /// Take one of the outbox's queues, leaving it empty.
    pub(super) fn take_outbox<T: Default>(&mut self, queue: impl FnOnce(&mut Outbox) -> &mut T) -> T {
        self.outbox().map(|o| std::mem::take(queue(o))).unwrap_or_default()
    }
}

/// Put one more thing in the outbox of the server `vm` runs for, through its
/// host (a bare VM, with no server, has nowhere to send it).
fn send(vm: &mut Vm, put: impl FnOnce(&mut Outbox)) {
    vm.with_host(|_, h| put(h.outbox()));
}

// ---------------------------------------------------------------------------
// Sound events (PF_sound / SV_StartSound).
//
// The C `PF_sound` -> `SV_StartSound` wrote an `svc_sound` message into the
// per-client datagram for the network layer to flush. This headless server has
// no netcode, so instead each fired sound is captured as a [`SoundEvent`] in
// the outbox, which [`Server::drain_sounds`] hands to whatever audio front-end
// (or test) wants it.
// ---------------------------------------------------------------------------

/// One queued sound emission — the engine `SV_StartSound` payload, captured for
/// a front-end instead of being serialised into a client datagram.
///
/// `origin` is the entity's box centre (`origin + 0.5*(mins+maxs)`), matching
/// the coordinate `SV_StartSound` wrote. `sample` keeps the raw sound name;
/// `sound_index` is its precache slot (`>= 1`) or `-1` if it was never
/// precached (the C `Con_Printf("not precacheed")`-and-drop case — we still
/// queue the event so a caller can see what was attempted).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundEvent {
    /// The emitting edict index.
    pub entity: i32,
    /// Sound channel (0 = auto-allocate; 1..=7 override that entity/channel).
    pub channel: i32,
    /// Precache index of `sample`, or `-1` when it was not precached.
    pub sound_index: i32,
    /// The raw sound name (e.g. `"weapons/guncock.wav"`).
    pub sample: String,
    /// World-space emission point: `origin + 0.5*(mins + maxs)`.
    pub origin: [f32; 3],
    /// Volume in `0.0..=1.0` (the C scaled this by 255 for the packet byte).
    pub volume: f32,
    /// Attenuation in `0.0..=4.0` (0 = audible everywhere).
    pub attenuation: f32,
}

// ---------------------------------------------------------------------------
// Static (looping ambient) sound registry (PF_ambientsound).
//
// The C `PF_ambientsound` (pr_cmds.c) wrote an `svc_spawnstaticsound` into the
// level signon packet; the client's `CL_ParseStaticSound` -> `S_StaticSound`
// (snd_dma.c) then allocated a PERSISTENT looping channel re-spatialized every
// frame. These are the torch crackles / wind / hums placed by the QuakeC at
// level spawn. Like the one-shot queue above, this headless server has no
// netcode, so each `ambientsound()` is recorded as a [`StaticSound`] in the
// outbox, which [`Server::drain_static_sounds`] hands to the front-end ONCE
// (the front-end keeps the loops alive itself, mirroring how the signon packet
// was sent once at connect).
// ---------------------------------------------------------------------------

/// One placed looping ambient sound — the `svc_spawnstaticsound` payload the C
/// `PF_ambientsound` wrote into the signon, captured for a front-end.
///
/// `origin` is the position as the wire carried it ([`wire_coord`]), and
/// `volume`/`attenuation` are kept in the QuakeC domain (`0.0..=1.0` /
/// `0.0..=4.0`) but quantized through the same bytes the wire format used
/// (`vol*255` and `atten*64`, truncated), so a front-end hears exactly what the
/// original client was told. `sound_index` mirrors [`SoundEvent::sound_index`]:
/// the precache slot, or `-1` when no host resolved it.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticSound {
    /// World-space emission point: `PF_ambientsound`'s `pos` argument through
    /// `MSG_WriteCoord` ([`wire_coord`]) — static sounds are placed at a point,
    /// not on an entity.
    pub origin: [f32; 3],
    /// Precache index of `sample`, or `-1` when it was not resolved.
    pub sound_index: i32,
    /// The raw sound name (e.g. `"ambience/fire1.wav"`).
    pub sample: String,
    /// Volume in `0.0..=1.0`, quantized through the wire byte (`trunc(vol*255)/255`).
    pub volume: f32,
    /// Attenuation in `0.0..=4.0`, quantized through the wire byte
    /// (`trunc(atten*64)/64`; `ATTN_STATIC` = 3 survives exactly).
    pub attenuation: f32,
}

/// Box centre of an entity: `origin + 0.5*(mins + maxs)`, the point
/// `SV_StartSound`/`PF_ambientsound` wrote for the emission coordinate.
fn entity_sound_origin(vm: &Vm, e: i32) -> [f32; 3] {
    let origin = vm.ent_vec(e, vm.fo().origin);
    let mins = vm.ent_vec(e, vm.fo().mins);
    let maxs = vm.ent_vec(e, vm.fo().maxs);
    [
        origin[0] + 0.5 * (mins[0] + maxs[0]),
        origin[1] + 0.5 * (mins[1] + maxs[1]),
        origin[2] + 0.5 * (mins[2] + maxs[2]),
    ]
}

/// A world coordinate as it crossed the wire: `MSG_WriteCoord` sent
/// `(int)(f*8)` as a short (the fraction truncated toward zero, the bits past
/// 16 dropped) and `MSG_ReadCoord` read back `short * (1.0/8)`. id's client
/// knew every position the server sent it to the 1/8 unit, so where a
/// position reaches the sound layer it goes through this first.
#[must_use]
pub fn wire_coord(f: f32) -> f32 {
    f32::from((f * 8.0) as i32 as i16) * (1.0 / 8.0)
}

// ---------------------------------------------------------------------------
// Particle-burst queue (PF_particle).
//
// The C `PF_particle` -> `SV_StartParticle` wrote an `svc_particle` message
// into the per-client datagram; the client's `R_RunParticleEffect` then spawned
// the actual particles into its `d_*` software renderer. This headless server
// has no client, so — exactly like the sound queue above — each fired
// `particle()` is captured as a [`ParticleBurst`] in the outbox, which
// [`Server::drain_particles`] hands to a front-end. The front-end
// (wasm/quaketool) owns the live [`crate::particles::ParticleSystem`] that turns
// a drained burst into spawned points, ages them, and draws them into the scene.
// ---------------------------------------------------------------------------

/// One queued `particle()` burst — the engine `SV_StartParticle` payload,
/// captured for a front-end instead of being serialised into a client datagram.
///
/// The fields mirror `PF_particle`'s arguments verbatim: `org` is the emission
/// origin, `dir` the direction/speed the C scaled into the velocity, `color` the
/// base palette index of the 8-entry colour ramp, and `count` the number of
/// particles to spawn. A front-end replays this through
/// [`crate::particles::ParticleSystem::spawn_burst`].
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleBurst {
    /// Emission origin (world space).
    pub org: [f32; 3],
    /// Direction/speed the renderer scales into each particle's velocity.
    pub dir: [f32; 3],
    /// Base palette index of the colour ramp (`color & ~7` selects the ramp).
    pub color: u8,
    /// How many particles to spawn (clamped against the pool cap on spawn).
    pub count: i32,
}

/// A text message QuakeC asked to show the player: a `centerprint` (drawn
/// centered for a couple of seconds — level intros, "you need the silver key")
/// or a `bprint`/`sprint` notify line (item pickups, etc.). Drained each frame by
/// the front-end, which renders + times them out.
#[derive(Debug, Clone, PartialEq)]
pub struct GameMessage {
    /// True for `centerprint` (centered, transient); false for a notify line.
    pub center: bool,
    /// The message text (may contain '\n').
    pub text: String,
}

impl Outbox {
    /// Queue a print for the player (`svc_centerprint`, or `svc_print`'s notify
    /// line); an empty one shows nothing, so it is dropped.
    fn print(&mut self, center: bool, text: String) {
        if !text.is_empty() {
            self.messages.push(GameMessage { center, text });
        }
    }
}

/// `PF_centerprint` (#73): show the (var-arg concatenated) message centered on
/// screen for a few seconds. The client arg (index 0) is ignored (single-player);
/// the message is args from index 1. Also mirrored into the dev `output` log.
pub(super) fn bi_centerprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 1);
    vm.print(&s);
    send(vm, |o| o.print(true, s));
    Ok(())
}

/// `PF_bprint` (#23): broadcast print — shown as a notify line.
pub(super) fn bi_bprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 0);
    vm.print(&s);
    send(vm, |o| o.print(false, s));
    Ok(())
}

/// `PF_sprint` (#24): single-client print — a notify line (client arg at index 0
/// ignored; message is args from index 1).
pub(super) fn bi_sprint(vm: &mut Vm) -> Result<()> {
    let s = crate::builtins::var_string(vm, 1);
    vm.print(&s);
    send(vm, |o| o.print(false, s));
    Ok(())
}

/// `PF_stuffcmd` (#21): `stuffcmd(client, text)` sends `svc_stufftext` to that
/// client, whose command buffer runs it (`Cbuf_AddText`). Queued as
/// `(entity, text)`; the front-end playing that client takes it with
/// [`Server::drain_stufftext`]. The C's "Parm 0 not a client" `PR_RunError`
/// for an entity outside `1..=maxclients` is left to the front-end, which
/// only executes text sent to its own player. (id1 stuffs only `"bf\n"`, the
/// bonus flash, from 16 sites: every item pickup and CheckPowerups.)
pub(super) fn bi_stuffcmd(vm: &mut Vm) -> Result<()> {
    let ent = vm.arg_entity(0);
    let text = vm.arg_string(1);
    send(vm, |o| o.stufftext.push((ent, text)));
    Ok(())
}

/// `PF_particle` (#48): `void(vector org, vector dir, float color, float count)
/// particle`. The C forwarded these straight to `SV_StartParticle`; here we
/// queue a [`ParticleBurst`] for the front-end's [`crate::particles::ParticleSystem`]
/// to realise. The base `color` and `count` are kept as the engine domain (a
/// palette index and a particle count); the colour is cast into a `u8` palette
/// index (the C `SV_StartParticle` itself wrote `color` as one packet byte).
///
/// FAITHFULNESS: the C `SV_StartParticle` early-returned when the network
/// datagram was nearly full; we have no datagram, so every fired burst is
/// queued. A negative/huge `count` is preserved as-is and clamped only when the
/// `ParticleSystem` spawns it, so the engine never allocates on program data.
pub(super) fn bi_particle(vm: &mut Vm) -> Result<()> {
    let org = vm.arg_vector(0);
    let dir = vm.arg_vector(1);
    // color is a float palette index; clamp into 0..=255 before the byte cast so
    // an out-of-range value can never wrap unexpectedly.
    let color = vm.arg_float(2).clamp(0.0, 255.0) as u8;
    // SV_StartParticle writes `count` through MSG_WriteByte, which TRUNCATES mod 256
    // (`buf[0] = c`), and the client's CL_ParseParticleEffect maps the byte value
    // EXACTLY 255 back to 1024 — the explosion sentinel (R_RunParticleEffect's fiery
    // pt_explode burst). So the trigger is `(count & 0xFF) == 255`, not `count >=
    // 255`: a stray count like 256 truncates to 0 (no explosion), and -1 wraps to
    // 255 -> 1024, exactly as the C and the demo parser (demo.rs) do. The
    // misc_explobox death does `particle(origin, '0 0 0', 75, 255)` -> 1024 -> burst.
    let sent = (vm.arg_float(3) as i32 & 0xFF) as u8;
    let count = if sent == 255 { 1024 } else { sent as i32 };

    let burst = ParticleBurst { org, dir, color, count };
    send(vm, |o| o.particles.push(burst));
    Ok(())
}

// ---------------------------------------------------------------------------
// The Write* builtins (#52..#59) and what the client made of them: one svc
// parser per message buffer.
//
// In the C, `PF_Write*` append to the buffer `WriteDest()` picks —
// MSG_BROADCAST -> `sv.datagram`, MSG_ONE -> one client's `message`, MSG_ALL
// -> `sv.reliable_datagram`, MSG_INIT -> `sv.signon` — and the client reads
// every buffer it is sent with the same `CL_ParseServerMessage` (cl_parse.c):
// a command byte, then that command's payload, `svc_temp_entity` handing its
// payload to `CL_ParseTEnt` (cl_tent.c). The QuakeC picks the buffer; the
// client parses them all alike. The id1 progs write their temp entities to
// MSG_BROADCAST and the level-end / stat commands to MSG_ALL — except boss.qc's
// `lightning_fire`, which writes Chthon's TE_LIGHTNING3 bolt to MSG_ALL, and
// nothing to MSG_ONE or MSG_INIT (`census/qcsym.py calls WriteByte`).
//
// This server has no buffers and no network, so each buffer the progs write
// gets its own small parser ([`MsgParse`], one per [`MsgBuf`]) that the Write*
// builtins feed one value at a time. A completed temp entity queues as a
// [`TempEntityEvent`] ([`Server::drain_temp_entities`]; the front-end maps it
// to the same effects `CL_ParseTEnt` made), a completed command as an
// [`SvcEvent`] ([`Server::drain_svc_events`]). Keeping one parser per buffer,
// like the C's separate buffers, means a message half-written into one can
// never swallow a write aimed at the other. The parsers are deliberately
// total: an unknown command or TE type, or a write of the wrong kind, drops
// the message in progress and goes back to reading command bytes rather than
// guessing a length or panicking. The parsers and what they complete live in
// the outbox, like the queues above.
// ---------------------------------------------------------------------------

/// `MSG_BROADCAST` (pr_cmds.c `WriteDest`): `sv.datagram`, the unreliable
/// broadcast every temp entity but Chthon's rides.
const MSG_BROADCAST: i32 = 0;

/// `MSG_ALL` (pr_cmds.c `WriteDest`): `sv.reliable_datagram`, the reliable
/// broadcast every client receives — the intermission/finale/stat commands and
/// Chthon's lightning.
const MSG_ALL: i32 = 2;

/// The client-bound message buffers this server realises — which [`MsgParse`]
/// a write feeds. `MSG_ONE` (a client's own `message`) and `MSG_INIT` (the
/// signon) are not modelled: the id1 progs never write to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgBuf {
    /// `MSG_BROADCAST` -> `sv.datagram`.
    Datagram = 0,
    /// `MSG_ALL` -> `sv.reliable_datagram`.
    Reliable = 1,
}

/// `WriteDest()`: the buffer a `Write*`'s destination (its `PARM0`) names, or
/// `None` for one that is not modelled (the C `PR_RunError`ed on a bad one).
fn write_dest(dest: i32) -> Option<MsgBuf> {
    match dest {
        MSG_BROADCAST => Some(MsgBuf::Datagram),
        MSG_ALL => Some(MsgBuf::Reliable),
        _ => None,
    }
}

/// One `MSG_Write*` call as the client's parser reads it back: which kind of
/// field, and its value.
#[derive(Debug, Clone, PartialEq)]
enum MsgWrite {
    /// An 8-bit field: `WriteByte`, `WriteChar`, and `WriteAngle` (one byte on
    /// the wire) — svc command bytes, TE type bytes, `TE_EXPLOSION2`'s colours.
    Byte(f32),
    /// An integer field: `WriteShort`, `WriteLong`, and `WriteEntity` (the C's
    /// `MSG_WriteShort(G_EDICTNUM)`) — the beam types' owning entity.
    Short(f32),
    /// `WriteCoord`: a world coordinate. The C round-tripped it through a lossy
    /// `*8` short; the float is kept as written.
    Coord(f32),
    /// `WriteString`: the finale/cutscene text.
    Str(String),
}

// The `svc_*` command bytes (protocol.h) the id1 progs write.
const SVC_TEMP_ENTITY: u8 = 23;
const SVC_KILLEDMONSTER: u8 = 27;
const SVC_FOUNDSECRET: u8 = 28;
const SVC_INTERMISSION: u8 = 30;
const SVC_FINALE: u8 = 31;
const SVC_CDTRACK: u8 = 32;
const SVC_SELLSCREEN: u8 = 33;
const SVC_CUTSCENE: u8 = 34;
/// The 2021 re-release's `svc_achievement` (not in protocol.h): a string
/// naming a Steam/console achievement. The mission packs' re-release progs
/// write it when a monster kills another (`Killed`), at a secret
/// (`multi_trigger`), at a pack's last level.
const SVC_ACHIEVEMENT: u8 = 52;

// TE_* type bytes (protocol.h), as written after the svc_temp_entity byte.
const TE_SPIKE: u8 = 0;
const TE_SUPERSPIKE: u8 = 1;
const TE_GUNSHOT: u8 = 2;
const TE_EXPLOSION: u8 = 3;
const TE_TAREXPLOSION: u8 = 4;
const TE_LIGHTNING1: u8 = 5;
const TE_LIGHTNING2: u8 = 6;
const TE_WIZSPIKE: u8 = 7;
const TE_KNIGHTSPIKE: u8 = 8;
const TE_LIGHTNING3: u8 = 9;
const TE_LAVASPLASH: u8 = 10;
const TE_TELEPORT: u8 = 11;
const TE_EXPLOSION2: u8 = 12;
const TE_BEAM: u8 = 13;

/// The `TE_*` type bytes (protocol.h), re-exported for front-ends that map a
/// [`TempEntityEvent::te_type`] to an effect (the playtest/wasm callers). These
/// are the same byte values the QuakeC writes after `svc_temp_entity`.
pub mod te_consts {
    /// Spike hitting a wall (nail impact): a small `R_RunParticleEffect` burst.
    pub const TE_SPIKE: u8 = super::TE_SPIKE;
    /// Super-spike (super-nail) wall impact: a larger burst.
    pub const TE_SUPERSPIKE: u8 = super::TE_SUPERSPIKE;
    /// Bullet hitting a wall: a medium burst.
    pub const TE_GUNSHOT: u8 = super::TE_GUNSHOT;
    /// Rocket/grenade explosion: a 1024-particle fiery explosion + sound.
    pub const TE_EXPLOSION: u8 = super::TE_EXPLOSION;
    /// Tarbaby explosion: treated as an explosion + sound.
    pub const TE_TAREXPLOSION: u8 = super::TE_TAREXPLOSION;
    /// Lightning bolt beam (bolt.mdl): the Shambler's attack.
    pub const TE_LIGHTNING1: u8 = super::TE_LIGHTNING1;
    /// Lightning bolt beam (bolt2.mdl): the player's thunderbolt.
    pub const TE_LIGHTNING2: u8 = super::TE_LIGHTNING2;
    /// Wizard spike wall impact: a green-ish burst.
    pub const TE_WIZSPIKE: u8 = super::TE_WIZSPIKE;
    /// Knight spike wall impact.
    pub const TE_KNIGHTSPIKE: u8 = super::TE_KNIGHTSPIKE;
    /// Lightning bolt beam (bolt3.mdl): Chthon's electrodes (on MSG_ALL).
    pub const TE_LIGHTNING3: u8 = super::TE_LIGHTNING3;
    /// Lava splash (a Chthon attack): approximated as an upward burst.
    pub const TE_LAVASPLASH: u8 = super::TE_LAVASPLASH;
    /// Teleport splash: approximated as an upward burst.
    pub const TE_TELEPORT: u8 = super::TE_TELEPORT;
    /// Colour-mapped explosion: a 1024-particle explosion + sound.
    pub const TE_EXPLOSION2: u8 = super::TE_EXPLOSION2;
    /// Grappling-hook beam (beam.mdl).
    pub const TE_BEAM: u8 = super::TE_BEAM;
}

/// One decoded temp entity (the `CL_ParseTEnt` payload), captured for a
/// front-end instead of spawning a client-side particle effect directly —
/// whichever buffer ([`MsgBuf`]) it was written into.
///
/// `pos` is the effect origin (the three `WriteCoord`s). For [`TE_EXPLOSION2`]
/// (`te_type == 12`) `color_start`/`color_length` carry the two trailing colour
/// bytes; for every other type they are `0`. Beam types (`TE_LIGHTNING1/2/3`,
/// `TE_BEAM`) carry the owning entity number in `entity`, the *start* point in
/// `pos` and the *end* point in `end` — a front-end feeds those three to
/// [`crate::tent::Beams::parse_beam`] (the `CL_ParseBeam` slot store) to render
/// the bolt; for every non-beam type `entity` is `0` and `end` equals `pos`.
#[derive(Debug, Clone, PartialEq)]
pub struct TempEntityEvent {
    /// The `TE_*` type byte (e.g. `3` = explosion, `2` = gunshot).
    pub te_type: u8,
    /// The effect origin (the three `WriteCoord` values; the beam START point).
    pub pos: [f32; 3],
    /// Beam types: the END point (the trailing three `WriteCoord`s). Non-beam
    /// types carry no end point — set equal to `pos`.
    pub end: [f32; 3],
    /// Beam types: the owning entity number (the `WriteEntity` short before the
    /// coords) — `CL_ParseBeam`'s slot-reuse key; `0` (the world) for Chthon's
    /// bolt. `0` for non-beam types.
    pub entity: i32,
    /// `TE_EXPLOSION2` colour-ramp start index; `0` for other types.
    pub color_start: u8,
    /// `TE_EXPLOSION2` colour-ramp length; `0` for other types.
    pub color_length: u8,
}

/// What payload shape a recognised `TE_*` type expects, so the parser consumes
/// exactly the right fields and stays synchronised with the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TePayload {
    /// Three `WriteCoord`s (spikes, gunshots, explosions, splashes).
    Coords3,
    /// Three `WriteCoord`s then two colour bytes (`TE_EXPLOSION2`).
    Coords3ThenTwoBytes,
    /// A `short` entity index then six `WriteCoord`s, start and end (the
    /// beam/lightning types).
    Beam,
}

/// Map a `TE_*` type byte to its payload shape, or `None` for an unknown type
/// (the parser then drops the message rather than guessing a length and
/// corrupting every later write).
fn te_payload(te_type: u8) -> Option<TePayload> {
    match te_type {
        TE_SPIKE | TE_SUPERSPIKE | TE_GUNSHOT | TE_EXPLOSION | TE_TAREXPLOSION
        | TE_WIZSPIKE | TE_KNIGHTSPIKE | TE_LAVASPLASH | TE_TELEPORT => Some(TePayload::Coords3),
        TE_EXPLOSION2 => Some(TePayload::Coords3ThenTwoBytes),
        TE_LIGHTNING1 | TE_LIGHTNING2 | TE_LIGHTNING3 | TE_BEAM => Some(TePayload::Beam),
        _ => None,
    }
}

/// The largest number of `WriteCoord` fields any temp entity carries (the beam
/// types: 6). A hard cap so a malformed stream can never grow a message without
/// bound.
const TE_MAX_COORDS: usize = 6;

/// A temp entity being read back (`CL_ParseTEnt`), from its `svc_temp_entity`
/// byte to its last payload field.
#[derive(Debug, Clone, PartialEq, Default)]
struct TeMsg {
    /// The `TE_*` type byte once read (`None` while awaiting it), with its
    /// payload shape.
    ty: Option<(u8, TePayload)>,
    /// `WriteCoord` values collected so far (at most [`TE_MAX_COORDS`]).
    coords: Vec<f32>,
    /// `TE_EXPLOSION2`'s trailing colour bytes collected so far (at most 2).
    bytes: Vec<u8>,
    /// For [`TePayload::Beam`], the leading short (the owning entity) once read.
    beam_entity: Option<i32>,
}

/// What one more write did to a [`TeMsg`].
enum TeStep {
    /// The message needs more fields.
    More,
    /// The message is complete.
    Done(TempEntityEvent),
    /// The write cannot belong to this message (an unknown type, a string, a
    /// non-byte where the type byte goes): the message is dropped.
    Drop,
}

impl TeMsg {
    /// Feed one write to the temp entity being read. A field of the wrong kind
    /// for the slot being read is skipped, except where the type byte is
    /// expected and for strings (no temp entity carries one), which drop it.
    fn feed(&mut self, w: &MsgWrite) -> TeStep {
        if matches!(w, MsgWrite::Str(_)) {
            return TeStep::Drop;
        }
        // Awaiting the TE_* type byte (the write after svc_temp_entity).
        let Some((te_type, shape)) = self.ty else {
            let MsgWrite::Byte(v) = *w else { return TeStep::Drop };
            let tb = v as i32;
            // Out-of-byte-range or unknown type => drop the message.
            let te_type = if (0..=255).contains(&tb) { tb as u8 } else { 255 };
            return match te_payload(te_type) {
                Some(shape) => {
                    self.ty = Some((te_type, shape));
                    TeStep::More
                }
                None => TeStep::Drop,
            };
        };
        let point = |c: &[f32]| [c[0], c[1], c[2]];
        match shape {
            TePayload::Coords3 => {
                if let MsgWrite::Coord(v) = *w {
                    self.coords.push(v);
                }
                if self.coords.len() < 3 {
                    return TeStep::More;
                }
                let pos = point(&self.coords);
                TeStep::Done(TempEntityEvent { te_type, pos, end: pos, entity: 0, color_start: 0, color_length: 0 })
            }
            TePayload::Coords3ThenTwoBytes => {
                match *w {
                    MsgWrite::Coord(v) if self.coords.len() < 3 => self.coords.push(v),
                    MsgWrite::Byte(v) if self.coords.len() == 3 => {
                        self.bytes.push((v as i32).clamp(0, 255) as u8)
                    }
                    _ => {}
                }
                if self.bytes.len() < 2 {
                    return TeStep::More;
                }
                let pos = point(&self.coords);
                TeStep::Done(TempEntityEvent {
                    te_type,
                    pos,
                    end: pos,
                    entity: 0,
                    color_start: self.bytes[0],
                    color_length: self.bytes[1],
                })
            }
            TePayload::Beam => {
                // The short (owning entity) first, then 6 coords (start, end).
                match *w {
                    MsgWrite::Short(v) if self.beam_entity.is_none() => self.beam_entity = Some(v as i32),
                    MsgWrite::Coord(v) if self.beam_entity.is_some() && self.coords.len() < TE_MAX_COORDS => {
                        self.coords.push(v)
                    }
                    _ => {}
                }
                match self.beam_entity {
                    // Everything CL_ParseBeam needs for its slot store
                    // (crate::tent::Beams): the owner, START, END.
                    Some(entity) if self.coords.len() == 6 => TeStep::Done(TempEntityEvent {
                        te_type,
                        pos: point(&self.coords),
                        end: point(&self.coords[3..]),
                        entity,
                        color_start: 0,
                        color_length: 0,
                    }),
                    _ => TeStep::More,
                }
            }
        }
    }
}

/// `CL_ParseServerMessage` over one buffer: where the parser is between writes.
#[derive(Debug, Clone, PartialEq, Default)]
enum MsgParse {
    /// Between commands: the next `WriteByte` is an `svc_*` command byte; any
    /// other write here is out of step and skipped.
    #[default]
    Command,
    /// `svc_temp_entity` read: the writes are `CL_ParseTEnt`'s payload.
    TempEntity(TeMsg),
    /// `svc_finale`/`svc_cutscene` read; awaiting the `WriteString` payload.
    /// `cutscene` says which event to emit.
    AwaitString { cutscene: bool },
    /// `svc_cdtrack` read; the next two `WriteByte`s are its payload (the
    /// track, once read, then `looptrack`), not command bytes.
    CdTrack(Option<u8>),
}

/// One recognised server command, surfaced to the front-end the way the
/// client's `CL_ParseServerMessage` (cl_parse.c) would have acted on it.
#[derive(Debug, Clone, PartialEq)]
pub enum SvcEvent {
    /// `svc_intermission` (30): the level ended — the C set `cl.intermission = 1`,
    /// latched `cl.completed_time = cl.time` and went full-screen for the
    /// intermission camera + `Sbar_IntermissionOverlay` stats.
    Intermission,
    /// `svc_finale` (31) + its `WriteString` payload: episode-end text — the C set
    /// `cl.intermission = 2` and `SCR_CenterPrint`ed the string (slow char reveal).
    Finale(String),
    /// `svc_cutscene` (34) + its string: `cl.intermission = 3` (text only, no
    /// plaque). Unused by the vanilla progs (mission packs use it).
    Cutscene(String),
    /// `svc_sellscreen` (33): the shareware "order the full game" pitch — the C ran
    /// `Cmd_ExecuteString("help")`, i.e. opened the Help/Ordering pages.
    SellScreen,
    /// `svc_cdtrack` (32) + two bytes: `cl.cdtrack`, `cl.looptrack` — the
    /// client's `CDAudio_Play (cdtrack, true)`. The QuakeC sends the
    /// intermission's track 3 (`execute_changelevel`) and the finale's 2
    /// (`ExitIntermission`); a level's own comes with its signon.
    CdTrack { track: u8, looptrack: u8 },
}

impl Outbox {
    /// Put every buffer's parser back between commands, dropping any half-read
    /// message. Called at the start of each server frame (the C cleared its
    /// buffers once they were sent) so a partial message left by an errored
    /// think never bleeds into the next frame.
    pub(super) fn reset_parsers(&mut self) {
        self.parsers = Default::default();
    }

    /// A command byte read between commands: start its payload, or act on it.
    /// Unknown commands are skipped — the id1 progs only write the ones here.
    fn parse_command(&mut self, value: f32) -> MsgParse {
        let b = value as i32;
        match if (0..=255).contains(&b) { b as u8 } else { 0 } {
            SVC_TEMP_ENTITY => MsgParse::TempEntity(TeMsg::default()),
            SVC_INTERMISSION => {
                self.svc_events.push(SvcEvent::Intermission);
                MsgParse::Command
            }
            SVC_FINALE => MsgParse::AwaitString { cutscene: false },
            SVC_CUTSCENE => MsgParse::AwaitString { cutscene: true },
            SVC_CDTRACK => MsgParse::CdTrack(None),
            SVC_SELLSCREEN => {
                self.svc_events.push(SvcEvent::SellScreen);
                MsgParse::Command
            }
            // Stat ticks: the front-end reads killed_monsters / found_secrets from
            // the QuakeC globals (like the Tab scoreboard), so these single-byte
            // commands need no event.
            SVC_KILLEDMONSTER | SVC_FOUNDSECRET => MsgParse::Command,
            // id's client knows no 52: CL_ParseServerMessage's `default`
            // Host_Errors ("Illegible server message"), so WinQuake cannot
            // play the re-release's mission packs past their first secret.
            // The re-release's own engine records the achievement; here the
            // command and its string drop out of the parse, as any unknown
            // command does, and the game goes on.
            SVC_ACHIEVEMENT => MsgParse::Command,
            _ => MsgParse::Command,
        }
    }

    /// `MSG_Write*(WriteDest(), value)`: hand one write to the parser of the
    /// buffer `dest` names (a no-op for an unmodelled destination).
    fn write(&mut self, dest: i32, w: MsgWrite) {
        let Some(buf) = write_dest(dest) else { return };
        let st = std::mem::take(&mut self.parsers[buf as usize]);
        self.parsers[buf as usize] = match st {
            MsgParse::Command => match w {
                MsgWrite::Byte(v) => self.parse_command(v),
                _ => MsgParse::Command,
            },
            MsgParse::TempEntity(mut te) => match te.feed(&w) {
                TeStep::More => MsgParse::TempEntity(te),
                TeStep::Done(ev) => {
                    self.temp_entities.push(ev);
                    MsgParse::Command
                }
                TeStep::Drop => MsgParse::Command,
            },
            MsgParse::AwaitString { cutscene } => {
                // Anything but the string is out of step: drop the command.
                if let MsgWrite::Str(text) = w {
                    self.svc_events.push(if cutscene { SvcEvent::Cutscene(text) } else { SvcEvent::Finale(text) });
                }
                MsgParse::Command
            }
            MsgParse::CdTrack(track) => match (w, track) {
                // MSG_WriteByte stores the float's (int) as a byte.
                (MsgWrite::Byte(v), None) => MsgParse::CdTrack(Some(v as i32 as u8)),
                (MsgWrite::Byte(v), Some(track)) => {
                    self.svc_events.push(SvcEvent::CdTrack { track, looptrack: v as i32 as u8 });
                    MsgParse::Command
                }
                _ => MsgParse::Command,
            },
        };
    }
}

/// `MSG_Write*(WriteDest(), value)` from a builtin, into its server's outbox.
fn msg_write(vm: &mut Vm, dest: i32, w: MsgWrite) {
    send(vm, |o| o.write(dest, w));
}

/// `PF_WriteByte` (#52): `void(float to, float value)` —
/// `MSG_WriteByte(WriteDest(), G_FLOAT(OFS_PARM1))`.
pub(super) fn bi_writebyte(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Byte(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteChar` (#53): one byte, like [`bi_writebyte`].
pub(super) fn bi_writechar(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Byte(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteShort` (#54): a 16-bit integer field.
pub(super) fn bi_writeshort(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Short(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteLong` (#55): a 32-bit integer field (no message the progs write
/// carries one; read like a short so the parser stays in step if one appears).
pub(super) fn bi_writelong(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Short(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteCoord` (#56): a world coordinate.
pub(super) fn bi_writecoord(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Coord(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteAngle` (#57): `MSG_WriteAngle` writes one byte, so the parser
/// reads it as one (no message the progs write carries an angle).
pub(super) fn bi_writeangle(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Byte(vm.arg_float(1)));
    Ok(())
}

/// `PF_WriteString` (#58): a string field — the text of a pending
/// `svc_finale`/`svc_cutscene`, which the client read with `MSG_ReadString`
/// and `SCR_CenterPrint`ed.
pub(super) fn bi_writestring(vm: &mut Vm) -> Result<()> {
    let dest = vm.arg_float(0) as i32;
    if write_dest(dest).is_some() {
        let text = vm.arg_string(1);
        msg_write(vm, dest, MsgWrite::Str(text));
    }
    Ok(())
}

/// `PF_WriteEntity` (#59): `MSG_WriteShort(WriteDest(), G_EDICTNUM(OFS_PARM1))`
/// — the beam types' owning entity (the `Beams` slot-reuse / view-entity key).
/// NOTE the arg is an entity reference (an INT global, `arg_entity`), not a
/// float — reading it as a float would yield the f32 bit-reinterpretation of
/// the edict index (~0.0 for every real entity), collapsing all beams onto one
/// slot.
pub(super) fn bi_writeentity(vm: &mut Vm) -> Result<()> {
    msg_write(vm, vm.arg_float(0) as i32, MsgWrite::Short(vm.arg_entity(1) as f32));
    Ok(())
}

/// `PF_sound` (#8): `void(entity e, float chan, string sample, float vol,
/// float atten) sound`. The arg layout mirrors `PF_sound`/`SV_StartSound`:
/// `entity = PARM0`, `channel = PARM1`, `sample = PARM2`, `volume = PARM3`,
/// `attenuation = PARM4`; the emission point is the entity's box centre.
///
/// FAITHFULNESS: the C `Sys_Error`s on out-of-range volume/attenuation/channel
/// and silently drops an un-precached sample. We never abort the host on
/// program data, so instead we keep the values as given (a front-end can clamp)
/// and still queue the event even when the sample was not precached, recording
/// `sound_index = -1` so the caller can tell. The C scaled volume by 255 into a
/// packet byte; we keep the QuakeC-domain `0.0..=1.0` float for the front-end.
pub(super) fn bi_sound(vm: &mut Vm) -> Result<()> {
    let entity = vm.arg_entity(0);
    let channel = vm.arg_float(1) as i32;
    let sample = vm.arg_string(2);
    let volume = vm.arg_float(3);
    let attenuation = vm.arg_float(4);

    let origin = entity_sound_origin(vm, entity);
    // Resolve the precache slot without registering a new name: SV_StartSound
    // only *looks up* an already-precached sample, dropping (here: marking -1)
    // when absent.
    let sound_index = lookup_sound_index(vm, &sample);

    let ev = SoundEvent { entity, channel, sound_index, sample, origin, volume, attenuation };
    send(vm, |o| o.sounds.push(ev));
    Ok(())
}

/// `PF_ambientsound` (#74, pr_cmds.c): `void(vector pos, string sample, float
/// vol, float atten) ambientsound`. The C emitted an `svc_spawnstaticsound`
/// into the level signon at an explicit world position; the client's
/// `S_StaticSound` then ran it as a PERSISTENT looping channel. We record it as
/// a [`StaticSound`] for [`Server::drain_static_sounds`] — NOT as a one-shot
/// [`SoundEvent`] (a loop is state, not an event).
///
/// Unlike `SV_StartSound`'s path (see [`lookup_sound_index`]'s DEVIATION),
/// the precache gate here matches the C exactly: `PF_ambientsound` scans
/// `sv.sound_precache` read-only and REFUSES an un-precached sample —
/// `Con_Printf ("no precache: %s\n", samp); return;` — registering nothing.
/// The message routes to [`Vm::output`] like the `print`/`dprint` builtins.
///
/// The wire format carried the position as three coordinates ([`wire_coord`])
/// and quantized volume and attenuation into bytes
/// (`MSG_WriteByte(vol*255)` / `MSG_WriteByte(attenuation*64)`, C float→int
/// truncation); `CL_ParseStaticSound` handed those bytes to `S_StaticSound`,
/// which divided the attenuation byte back by 64. We apply the same round-trip
/// (clamped to the byte range instead of wrapping, defensively) so a front-end
/// hears exactly what the original client was told.
pub(super) fn bi_ambientsound(vm: &mut Vm) -> Result<()> {
    let pos = vm.arg_vector(0);
    let sample = vm.arg_string(1);
    let volume = vm.arg_float(2);
    let attenuation = vm.arg_float(3);

    // "check to see if samp was properly precached" (pr_cmds.c:519-528).
    let Some(sound_index) = vm.with_host(|_vm, h| h.find_sound(&sample)).flatten() else {
        vm.print("no precache: ");
        vm.print(&sample);
        vm.print("\n");
        return Ok(());
    };

    let vol_byte = (volume * 255.0).clamp(0.0, 255.0) as u8;
    let atten_byte = (attenuation * 64.0).clamp(0.0, 255.0) as u8;
    let ev = StaticSound {
        origin: pos.map(wire_coord),
        sound_index,
        sample,
        volume: vol_byte as f32 / 255.0,
        attenuation: atten_byte as f32 / 64.0,
    };
    send(vm, |o| o.static_sounds.push(ev));
    Ok(())
}

/// Resolve `sample`'s precache slot for the one-shot [`SoundEvent`] paths
/// (`bi_sound` / the physics `start_sound`). The C `SV_StartSound` only
/// *searched* `sv.sound_precache` and dropped an un-precached sample. In
/// practice QuakeC precaches every sound during `worldspawn` before any
/// `sound()` fires, so `precache_sound` returns the existing stable slot
/// (`>= 1`) without appending. Returns `-1` only when there is no host at all.
///
/// DEVIATION: an un-precached name is registered here (and so gets a real
/// slot) rather than being dropped with a warning. The captured
/// [`SoundEvent`] still carries the raw `sample`, so a front-end is never
/// misled about what played. (`bi_ambientsound` does NOT use this: it matches
/// the C's read-only check via [`Host::find_sound`] and drops.)
fn lookup_sound_index(vm: &mut Vm, sample: &str) -> i32 {
    vm.with_host(|_vm, h| h.precache_sound(sample)).unwrap_or(-1)
}

impl Server {
    /// `Host_Pause_f` as the server runs it for the client that sent `pause`
    /// (the console command is `Cmd_ForwardToServer`ed; `pausable` is 1, id's
    /// default): toggle `sv.paused` and `SV_BroadcastPrintf` "<netname> paused
    /// the game" / "... unpaused the game" — the notify line and console text
    /// every client gets. The `svc_setpause` that follows is
    /// [`Server::paused`] itself: the local client reads it the same frame.
    pub fn pause(&mut self) {
        self.paused = !self.paused;
        let name = self.player.map(|p| self.vm.ent_str(p, self.vm.fo().netname).to_string()).unwrap_or_default();
        let what = if self.paused { "paused" } else { "unpaused" };
        if let Some(o) = self.outbox() {
            o.print(false, format!("{name} {what} the game\n"));
        }
    }

    /// Take and clear the queued sound events fired by the QuakeC since the last
    /// drain (`PF_sound`/`PF_ambientsound` pushes; see [`SoundEvent`]). A
    /// front-end calls this once per frame to play them; tests use it to assert
    /// a weapon actually fired.
    pub fn drain_sounds(&mut self) -> Vec<SoundEvent> {
        self.take_outbox(|o| &mut o.sounds)
    }

    /// Take and clear the placed looping ambient sounds the QuakeC registered
    /// via `ambientsound()` since the last drain (see [`StaticSound`]). The
    /// level's worldspawn registers them all during `spawn_entities`, so a
    /// front-end drains ONCE after the level builds and keeps the loops alive
    /// itself — mirroring how the C wrote them once into the signon packet and
    /// `S_StaticSound` kept a persistent channel.
    pub fn drain_static_sounds(&mut self) -> Vec<StaticSound> {
        self.take_outbox(|o| &mut o.static_sounds)
    }

    /// Take and clear the queued on-screen messages (`centerprint`/`sprint`/
    /// `bprint`) the QuakeC emitted since the last drain. The front-end shows
    /// centered ones transiently and notify lines fading at the top.
    pub fn drain_messages(&mut self) -> Vec<GameMessage> {
        self.take_outbox(|o| &mut o.messages)
    }

    /// Take the VM's output log ([`Vm::take_output`]): everything the QuakeC
    /// printed since the last drain — the `centerprint`/`sprint`/`bprint`
    /// text [`Server::drain_messages`] also hands over, `dprint`'s developer
    /// text, the `error`/`objerror` reports — and the console lines the
    /// server printed while running it (`no precache: …`). The game shows
    /// the messages and drops the rest, as id's does with `developer 0`;
    /// the tools keep it. Whoever runs a server for long drains it, or it
    /// grows for the level's life.
    pub fn drain_output(&mut self) -> String {
        self.vm.take_output()
    }

    /// Take and clear the queued particle bursts fired by the QuakeC since the
    /// last drain (`PF_particle` pushes; see [`ParticleBurst`]). A front-end
    /// calls this once per frame and replays each burst into its
    /// [`crate::particles::ParticleSystem`]; tests use it to assert an
    /// explosion/spawn actually emitted particles.
    pub fn drain_particles(&mut self) -> Vec<ParticleBurst> {
        self.take_outbox(|o| &mut o.particles)
    }

    /// Take and clear the queued temp-entity events decoded from the QuakeC's
    /// `Write*` bursts since the last drain — MSG_BROADCAST (rocket/grenade
    /// explosions, wall impacts, the Shambler's and the thunderbolt's beams) and
    /// MSG_ALL (Chthon's lightning), in the order they were written. A front-end
    /// calls this once per frame and maps each [`TempEntityEvent`] to the
    /// matching [`crate::particles::ParticleSystem`] effect or beam; tests
    /// use it to assert a temp entity actually fired.
    pub fn drain_temp_entities(&mut self) -> Vec<TempEntityEvent> {
        self.take_outbox(|o| &mut o.temp_entities)
    }

    /// Take and clear the queued MSG_ALL server commands recognised from the
    /// QuakeC's `WriteByte(MSG_ALL, ...)` bursts since the last drain
    /// (`svc_intermission` / `svc_finale` / `svc_cutscene` / `svc_sellscreen`).
    /// A front-end calls this once per frame and plays the client role of
    /// `CL_ParseServerMessage` (cl_parse.c): enter intermission mode, latch the
    /// completed time, start the finale text reveal.
    pub fn drain_svc_events(&mut self) -> Vec<SvcEvent> {
        self.take_outbox(|o| &mut o.svc_events)
    }

    /// Take and clear the `stuffcmd` text the QuakeC sent since the last
    /// drain, as `(client entity, text)`: the reliable `svc_stufftext`
    /// messages whose text the client's command buffer runs (see
    /// [`bi_stuffcmd`]).
    pub fn drain_stufftext(&mut self) -> Vec<(i32, String)> {
        self.take_outbox(|o| &mut o.stufftext)
    }

    /// `SV_StartSound` (sv_phys.c helper, via `world.c`): queue a sound emitted by
    /// `ent` on `channel` with the named `sample`. `volume_byte` is the C 0..255
    /// byte (255 = full); we store it back in the QuakeC `0.0..=1.0` domain the
    /// [`SoundEvent`] queue uses. Used by the toss/step physics for the
    /// water-entry splash and the landing thud (the C calls these directly, not
    /// through the QuakeC `sound` builtin).
    pub(super) fn start_sound(&mut self, ent: i32, channel: i32, sample: &str, volume_byte: i32, attenuation: f32) {
        let origin = entity_sound_origin(&self.vm, ent);
        let sound_index = lookup_sound_index(&mut self.vm, sample);
        let volume = (volume_byte as f32) / 255.0;
        let ev = SoundEvent { entity: ent, channel, sound_index, sample: sample.to_string(), origin, volume, attenuation };
        if let Some(o) = self.outbox() {
            o.sounds.push(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Progs, OFS_PARM0};
    use crate::server::testutil::*;

    #[test]
    fn bi_sound_queues_event_and_drain_clears() {
        // bi_sound (PF_sound) must push a SoundEvent with the faithful fields and
        // drain_sounds must return then clear it. Drive the builtin directly by
        // placing its args in the PARM globals and calling it.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // Give an entity a box so the centre offset is non-trivial.
        let e = server.vm.spawn();
        server.vm.ent_set_vector(e, "origin", [10.0, 20.0, 30.0]);
        server.vm.ent_set_vector(e, "mins", [-2.0, -4.0, -6.0]);
        server.vm.ent_set_vector(e, "maxs", [2.0, 4.0, 16.0]);

        // Precache the sample so it resolves to a real slot, then set up PARMs.
        let sample = "ambience/wind2.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        // PARM0=entity, PARM1=channel(2), PARM2=sample, PARM3=vol(0.5), PARM4=atten(2)
        server.vm.set_gi(OFS_PARM0, e);
        server.vm.set_gf(OFS_PARM0 + 3, 2.0);
        server.vm.set_gi(OFS_PARM0 + 6, s_t);
        server.vm.set_gf(OFS_PARM0 + 9, 0.5);
        server.vm.set_gf(OFS_PARM0 + 12, 2.0);

        bi_sound(&mut server.vm).expect("bi_sound");

        let sounds = server.drain_sounds();
        assert_eq!(sounds.len(), 1);
        let ev = &sounds[0];
        assert_eq!(ev.entity, e);
        assert_eq!(ev.channel, 2);
        assert_eq!(ev.sample, sample);
        assert_eq!(ev.volume, 0.5);
        assert_eq!(ev.attenuation, 2.0);
        // centre = (10,20,30) + 0.5*((-2,-4,-6)+(2,4,16)) = (10,20,30)+(0,0,5) = (10,20,35)
        assert_eq!(ev.origin, [10.0, 20.0, 35.0]);
        assert!(ev.sound_index >= 1, "precached sample resolved");

        // The queue is empty after draining.
        assert!(
            server.drain_sounds().is_empty(),
            "drain_sounds cleared the queue"
        );
    }

    #[test]
    fn bi_ambientsound_records_static_sound_and_drain_clears() {
        // bi_ambientsound (PF_ambientsound, #74) must record a StaticSound (a
        // persistent loop, NOT a one-shot SoundEvent) carrying the placed
        // position and the byte-quantized volume/attenuation the wire format
        // (`svc_spawnstaticsound`) carried, and drain_static_sounds must return
        // then clear it. Drive the builtin directly via the PARM globals.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds(); // clear any startup registrations

        // Precache the sample so it resolves to a real slot, then set up PARMs:
        // PARM0=pos(vec), PARM1=sample, PARM2=vol(0.5), PARM3=atten(3=ATTN_STATIC)
        // — FireAmbient's exact call for the e1m1 torches.
        let sample = "ambience/fire1.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        server.vm.set_gv(OFS_PARM0, [100.0, -50.0, 24.0]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 0.5);
        server.vm.set_gf(OFS_PARM0 + 9, 3.0);

        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        let statics = server.drain_static_sounds();
        assert_eq!(statics.len(), 1, "one static sound recorded");
        let s = &statics[0];
        assert_eq!(s.origin, [100.0, -50.0, 24.0], "placed at the literal pos");
        assert_eq!(s.sample, sample);
        // vol 0.5 -> byte trunc(127.5)=127 -> 127/255 (the wire round-trip).
        assert_eq!(s.volume, 127.0 / 255.0);
        // atten 3 -> byte 192 -> 192/64 = 3.0 exactly (ATTN_STATIC survives).
        assert_eq!(s.attenuation, 3.0);
        assert!(s.sound_index >= 1, "precached sample resolved");

        // It is a loop registration, not a one-shot: the SoundEvent queue is
        // untouched, and the static registry is empty after draining.
        assert!(
            server.drain_sounds().is_empty(),
            "no one-shot SoundEvent queued by ambientsound"
        );
        assert!(
            server.drain_static_sounds().is_empty(),
            "drain_static_sounds cleared the registry"
        );
    }

    #[test]
    fn wire_coord_truncates_to_the_eighth_and_wraps_at_a_short() {
        // MSG_WriteCoord's (int)(f*8): toward zero, both signs.
        assert_eq!(wire_coord(1352.469), 1352.375);
        assert_eq!(wire_coord(-7.969), -7.875);
        assert_eq!(wire_coord(0.124), 0.0);
        assert_eq!(wire_coord(-0.124), 0.0);
        // Eighths cross unchanged.
        assert_eq!(wire_coord(43.0), 43.0);
        assert_eq!(wire_coord(-4095.875), -4095.875);
        // MSG_WriteShort keeps 16 bits: 4096 comes back as -4096.
        assert_eq!(wire_coord(4096.0), -4096.0);
    }

    #[test]
    fn bi_ambientsound_sends_its_position_through_the_wire() {
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds();
        let sample = "ambience/fire1.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        server.vm.set_gv(OFS_PARM0, [100.3, -50.06, 24.2]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 1.0);
        server.vm.set_gf(OFS_PARM0 + 9, 3.0);
        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");
        // CL_ParseStaticSound's three MSG_ReadCoords.
        assert_eq!(server.drain_static_sounds()[0].origin, [100.25, -50.0, 24.125]);
    }

    #[test]
    fn bi_ambientsound_clamps_out_of_range_bytes() {
        // The C MSG_WriteByte would wrap out-of-range values; we clamp
        // defensively (QuakeC only ever passes sane 0..1 / 0..4 values).
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds();

        let sample = "ambience/wind2.wav";
        server.vm.with_host(|_vm, h| h.precache_sound(sample));
        let s_t = server.vm.intern(sample);
        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 9.0); // vol byte clamps to 255
        server.vm.set_gf(OFS_PARM0 + 9, 9.0); // atten byte clamps to 255
        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        let statics = server.drain_static_sounds();
        assert_eq!(statics[0].volume, 1.0, "volume byte clamps to 255");
        assert_eq!(
            statics[0].attenuation,
            255.0 / 64.0,
            "attenuation byte clamps to 255"
        );
    }

    #[test]
    fn bi_ambientsound_drops_unprecached_sample_like_the_c() {
        // PF_ambientsound scans sv.sound_precache READ-ONLY: an un-precached
        // sample is refused with `Con_Printf ("no precache: %s\n", samp)` and
        // nothing is registered — and the check must not grow the precache
        // table either (unlike the one-shot path's lookup_sound_index).
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");
        let _ = server.drain_static_sounds();

        let s_t = server.vm.intern("ambience/notthere.wav");
        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gi(OFS_PARM0 + 3, s_t);
        server.vm.set_gf(OFS_PARM0 + 6, 0.5);
        server.vm.set_gf(OFS_PARM0 + 9, 3.0);
        bi_ambientsound(&mut server.vm).expect("bi_ambientsound");

        assert!(
            server.drain_static_sounds().is_empty(),
            "un-precached ambientsound registers nothing"
        );
        assert!(
            server.vm.output().contains("no precache: ambience/notthere.wav\n"),
            "the C's console message, routed to vm.output: {:?}",
            server.vm.output()
        );
        assert_eq!(
            server.vm.with_host(|_vm, h| h.find_sound("ambience/notthere.wav")),
            Some(None),
            "the read-only check must not register the name as a side effect"
        );
    }

    #[test]
    fn bi_particle_queues_burst_and_drain_clears() {
        // bi_particle (PF_particle, #48) must push a ParticleBurst carrying its
        // (org, dir, color, count) arguments verbatim, and drain_particles must
        // return then clear it. Drive the builtin directly by placing its args in
        // the PARM globals, mirroring bi_sound_queues_event_and_drain_clears.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        // particle(org, dir, color, count): PARM0=org(vec), PARM1=dir(vec),
        // PARM2=color(float), PARM3=count(float).
        server.vm.set_gv(OFS_PARM0, [10.0, 20.0, 30.0]);
        server.vm.set_gv(OFS_PARM0 + 3, [0.0, 0.0, 1.0]);
        server.vm.set_gf(OFS_PARM0 + 6, 73.0); // base palette index
        server.vm.set_gf(OFS_PARM0 + 9, 12.0); // count

        bi_particle(&mut server.vm).expect("bi_particle");

        let bursts = server.drain_particles();
        assert_eq!(bursts.len(), 1, "one burst queued");
        let b = &bursts[0];
        assert_eq!(b.org, [10.0, 20.0, 30.0]);
        assert_eq!(b.dir, [0.0, 0.0, 1.0]);
        assert_eq!(b.color, 73);
        assert_eq!(b.count, 12);

        // The queue is empty after draining.
        assert!(
            server.drain_particles().is_empty(),
            "drain_particles cleared the queue"
        );
    }

    #[test]
    fn bi_particle_clamps_out_of_range_color_to_byte() {
        // A float color outside 0..=255 must clamp into the palette-index byte
        // range rather than wrapping unexpectedly when cast.
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(floor_bsp(), progs).expect("server");

        server.vm.set_gv(OFS_PARM0, [0.0; 3]);
        server.vm.set_gv(OFS_PARM0 + 3, [0.0; 3]);
        server.vm.set_gf(OFS_PARM0 + 6, 99999.0); // absurd color -> clamps to 255
        server.vm.set_gf(OFS_PARM0 + 9, 1.0);
        bi_particle(&mut server.vm).expect("bi_particle");
        let b = server.drain_particles();
        assert_eq!(b[0].color, 255, "out-of-range color clamps to 255");

        server.vm.set_gf(OFS_PARM0 + 6, -10.0); // negative -> clamps to 0
        bi_particle(&mut server.vm).expect("bi_particle");
        let b = server.drain_particles();
        assert_eq!(b[0].color, 0, "negative color clamps to 0");
    }

    // ------------------------------------------------ temp-entity decoder

    /// Drive a `WriteByte(dest, value)` builtin: dest in PARM0, value in PARM1.
    fn write_byte(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writebyte(&mut server.vm).expect("bi_writebyte");
    }
    /// Drive a `WriteCoord(dest, value)` builtin.
    fn write_coord(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writecoord(&mut server.vm).expect("bi_writecoord");
    }
    /// Drive a `WriteShort(dest, value)` builtin.
    fn write_short(server: &mut Server, dest: i32, value: f32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gf(OFS_PARM0 + 3, value);
        bi_writeshort(&mut server.vm).expect("bi_writeshort");
    }
    /// A fresh server (with a fresh outbox) for a message scenario.
    fn te_server() -> Server {
        let (img, _sound_fn) = attack_progs();
        let progs = Progs::parse(&img).expect("parse");
        Server::new(floor_bsp(), progs).expect("server")
    }

    #[test]
    fn te_explosion_burst_yields_one_event_with_pos() {
        // WriteByte(0,23) WriteByte(0,3) WriteCoord(0,x/y/z) -> one TE_EXPLOSION.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32); // svc_temp_entity
        write_byte(&mut server, 0, TE_EXPLOSION as f32); // type 3
        write_coord(&mut server, 0, 16.0);
        write_coord(&mut server, 0, -32.0);
        write_coord(&mut server, 0, 48.5);

        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "exactly one temp entity emitted");
        assert_eq!(evs[0].te_type, TE_EXPLOSION);
        assert_eq!(evs[0].pos, [16.0, -32.0, 48.5]);
        assert_eq!(evs[0].color_start, 0);
        assert_eq!(evs[0].color_length, 0);
        // drain cleared the queue (mirrors drain_sounds).
        assert!(
            server.drain_temp_entities().is_empty(),
            "drain_temp_entities cleared the queue"
        );
    }

    /// Drive a `WriteString(dest, text)` builtin (interning the text first, as
    /// the progs loader would have).
    fn write_string(server: &mut Server, dest: i32, text: &str) {
        let ofs = server.vm.intern(text);
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gi(OFS_PARM0 + 3, ofs);
        bi_writestring(&mut server.vm).expect("bi_writestring");
    }
    /// A fresh server for a MSG_ALL scenario.
    fn svc_server() -> Server {
        te_server()
    }

    #[test]
    fn svc_intermission_byte_on_msg_all_yields_event() {
        // execute_changelevel (client.qc): WriteByte(MSG_ALL, SVC_INTERMISSION).
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
        assert!(server.drain_svc_events().is_empty(), "drain cleared the queue");
    }

    #[test]
    fn svc_finale_byte_plus_string_yields_finale_text() {
        // ExitIntermission (client.qc): WriteByte(MSG_ALL, SVC_FINALE) then
        // WriteString(MSG_ALL, <episode text>).
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_FINALE as f32);
        assert!(server.drain_svc_events().is_empty(), "no event until the string lands");
        write_string(&mut server, MSG_ALL, "the Rune of Earth Magic");
        assert_eq!(
            server.drain_svc_events(),
            vec![SvcEvent::Finale("the Rune of Earth Magic".into())]
        );
    }

    /// The re-release progs' `svc_achievement` (`Killed`: WriteByte(MSG_ALL,
    /// 52), WriteString(MSG_ALL, "ACH_FRIENDLY_FIRE")) between two of id's
    /// commands: nothing comes of it, and the stream stays in step.
    #[test]
    fn svc_achievement_and_its_string_are_skipped() {
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_KILLEDMONSTER as f32);
        write_byte(&mut server, MSG_ALL, SVC_ACHIEVEMENT as f32);
        write_string(&mut server, MSG_ALL, "ACH_FRIENDLY_FIRE");
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
    }

    #[test]
    fn svc_cdtrack_payload_bytes_do_not_desync_the_stream() {
        // ExitIntermission writes cdtrack THEN the finale: WriteByte(MSG_ALL, 32),
        // WriteByte(MSG_ALL, 2), WriteByte(MSG_ALL, 3) — the two payload bytes must
        // be consumed, not read as commands — then the intermission/finale follows.
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_CDTRACK as f32);
        write_byte(&mut server, MSG_ALL, 2.0);
        write_byte(&mut server, MSG_ALL, 3.0);
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(
            server.drain_svc_events(),
            vec![SvcEvent::CdTrack { track: 2, looptrack: 3 }, SvcEvent::Intermission]
        );
    }

    #[test]
    fn svc_stat_ticks_and_other_destinations_yield_no_events() {
        let mut server = svc_server();
        // killed_monsters/found_secrets arrive as bare MSG_ALL bytes; the engine
        // reads the counts from the QuakeC globals, so no event surfaces.
        write_byte(&mut server, MSG_ALL, SVC_KILLEDMONSTER as f32);
        write_byte(&mut server, MSG_ALL, SVC_FOUNDSECRET as f32);
        // MSG_ONE / MSG_INIT are not modelled (the id1 progs never write them).
        write_byte(&mut server, 1, SVC_INTERMISSION as f32);
        write_byte(&mut server, 3, SVC_INTERMISSION as f32);
        assert!(server.drain_svc_events().is_empty());
    }

    /// Drive a `WriteEntity(dest, edict)` builtin (an INT entity global).
    fn write_entity(server: &mut Server, dest: i32, e: i32) {
        server.vm.set_gf(OFS_PARM0, dest as f32);
        server.vm.set_gi(OFS_PARM0 + 3, e);
        bi_writeentity(&mut server.vm).expect("bi_writeentity");
    }

    /// boss.qc `lightning_fire`, write for write (`qcsym.py func
    /// lightning_fire`): svc_temp_entity, TE_LIGHTNING3, `WriteEntity(world)`,
    /// then p1 and p2 — all to MSG_ALL.
    fn write_chthon_bolt(server: &mut Server, p1: [f32; 3], p2: [f32; 3]) {
        write_byte(server, MSG_ALL, SVC_TEMP_ENTITY as f32);
        write_byte(server, MSG_ALL, TE_LIGHTNING3 as f32);
        write_entity(server, MSG_ALL, 0);
        for v in p1.into_iter().chain(p2) {
            write_coord(server, MSG_ALL, v);
        }
    }

    #[test]
    fn chthon_msg_all_lightning_decodes_like_a_broadcast_temp_entity() {
        // The Chthon bug: lightning_fire writes its TE_LIGHTNING3 to MSG_ALL
        // (sv.reliable_datagram), which the client's CL_ParseServerMessage
        // parses exactly like the datagram. Only MSG_BROADCAST temp entities
        // were decoded, so the bolt never reached the client.
        let mut server = svc_server();
        write_chthon_bolt(&mut server, [-128.0, 64.0, -40.0], [960.0, 64.0, -40.0]);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "one temp entity from the reliable buffer");
        assert_eq!(evs[0].te_type, TE_LIGHTNING3);
        assert_eq!(evs[0].entity, 0, "the world owns Chthon's bolt");
        assert_eq!(evs[0].pos, [-128.0, 64.0, -40.0]);
        assert_eq!(evs[0].end, [960.0, 64.0, -40.0]);
        assert!(server.drain_svc_events().is_empty(), "a temp entity is not a command");
        // The next MSG_ALL byte is a command again.
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
    }

    #[test]
    fn each_buffer_has_its_own_parser() {
        // A broadcast explosion half-written when a MSG_ALL bolt and a stat tick
        // arrive: neither message swallows the other's writes (the C's
        // sv.datagram and sv.reliable_datagram are separate buffers).
        let mut server = svc_server();
        write_byte(&mut server, MSG_BROADCAST, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, MSG_BROADCAST, TE_EXPLOSION as f32);
        write_coord(&mut server, MSG_BROADCAST, 1.0);
        write_byte(&mut server, MSG_ALL, SVC_KILLEDMONSTER as f32);
        write_chthon_bolt(&mut server, [0.0; 3], [100.0, 0.0, 0.0]);
        write_coord(&mut server, MSG_BROADCAST, 2.0);
        write_coord(&mut server, MSG_BROADCAST, 3.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 2);
        assert_eq!((evs[0].te_type, evs[0].end), (TE_LIGHTNING3, [100.0, 0.0, 0.0]));
        assert_eq!((evs[1].te_type, evs[1].pos), (TE_EXPLOSION, [1.0, 2.0, 3.0]));
        assert!(server.drain_svc_events().is_empty());
    }

    #[test]
    fn temp_entity_payload_bytes_are_not_commands() {
        // TE_EXPLOSION2's colour bytes 30/33 on MSG_ALL are payload, not
        // svc_intermission/svc_sellscreen; and a bare svc_intermission on the
        // datagram is one (the client parses both buffers alike).
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, MSG_ALL, TE_EXPLOSION2 as f32);
        for v in [1.0, 2.0, 3.0] {
            write_coord(&mut server, MSG_ALL, v);
        }
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        write_byte(&mut server, MSG_ALL, SVC_SELLSCREEN as f32);
        let evs = server.drain_temp_entities();
        assert_eq!((evs.len(), evs[0].color_start, evs[0].color_length), (1, 30, 33));
        assert!(server.drain_svc_events().is_empty());
        write_byte(&mut server, MSG_BROADCAST, SVC_INTERMISSION as f32);
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
    }

    #[test]
    fn svc_sellscreen_and_cutscene_recognised() {
        let mut server = svc_server();
        write_byte(&mut server, MSG_ALL, SVC_SELLSCREEN as f32);
        write_byte(&mut server, MSG_ALL, SVC_CUTSCENE as f32);
        write_string(&mut server, MSG_ALL, "cut");
        assert_eq!(
            server.drain_svc_events(),
            vec![SvcEvent::SellScreen, SvcEvent::Cutscene("cut".into())]
        );
    }

    #[test]
    fn svc_recognizer_resets_on_unexpected_write_and_new_server() {
        let mut server = svc_server();
        // A non-byte/string MSG_ALL write mid-command means desync: drop it.
        write_byte(&mut server, MSG_ALL, SVC_FINALE as f32);
        write_short(&mut server, MSG_ALL, 7.0);
        write_string(&mut server, MSG_ALL, "late text");
        assert!(
            server.drain_svc_events().is_empty(),
            "desynced finale dropped, stray string ignored"
        );
        // A queued event from the OLD level cannot reach a new server: each
        // has its own outbox.
        write_byte(&mut server, MSG_ALL, SVC_INTERMISSION as f32);
        let mut fresh = svc_server();
        assert!(fresh.drain_svc_events().is_empty(), "a fresh server has no queued events");
        assert_eq!(server.drain_svc_events(), vec![SvcEvent::Intermission]);
    }

    #[test]
    fn te_gunshot_burst_carries_its_type() {
        // A TE_GUNSHOT (type 2) sequence yields te_type == 2.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_GUNSHOT as f32);
        write_coord(&mut server, 0, 1.0);
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_GUNSHOT);
        assert_eq!(evs[0].pos, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn te_explosion2_consumes_three_coords_then_two_bytes() {
        // EXPLOSION2 (type 12): 3 coords + colorStart + colorLength byte.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_EXPLOSION2 as f32);
        write_coord(&mut server, 0, 10.0);
        write_coord(&mut server, 0, 20.0);
        write_coord(&mut server, 0, 30.0);
        write_byte(&mut server, 0, 105.0); // colorStart
        write_byte(&mut server, 0, 8.0); // colorLength
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_EXPLOSION2);
        assert_eq!(evs[0].pos, [10.0, 20.0, 30.0]);
        assert_eq!(evs[0].color_start, 105);
        assert_eq!(evs[0].color_length, 8);
    }

    #[test]
    fn te_beam_consumes_short_and_six_coords() {
        // A beam (TE_BEAM=13): short entity index + 6 coords (start+end), all
        // captured for CL_ParseBeam (crate::tent): entity = slot key, pos =
        // start point, end = end point.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_BEAM as f32);
        write_short(&mut server, 0, 7.0); // entity index
        write_coord(&mut server, 0, 1.0); // start
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        write_coord(&mut server, 0, 4.0); // end
        write_coord(&mut server, 0, 5.0);
        write_coord(&mut server, 0, 6.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "beam emits exactly one event");
        assert_eq!(evs[0].te_type, TE_BEAM);
        assert_eq!(evs[0].entity, 7, "beam entity = the WriteShort slot key");
        assert_eq!(evs[0].pos, [1.0, 2.0, 3.0], "beam pos = start point");
        assert_eq!(evs[0].end, [4.0, 5.0, 6.0], "beam end = end point");
    }

    #[test]
    fn te_lightning_write_entity_captures_the_edict_number() {
        // The REAL beam writers (W_FireLightning etc.) pass the owner through
        // WriteEntity, whose parm is an entity reference — an INT global
        // (G_EDICTNUM), not a float. Reading it as a float yields the f32
        // bit-reinterpretation of the index (~0.0 for every edict), which would
        // collapse all beams onto one slot and break the view-entity tracking.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_LIGHTNING2 as f32);
        // WriteEntity(MSG_BROADCAST, self): an int edict number in PARM1.
        server.vm.set_gf(OFS_PARM0, 0.0);
        server.vm.set_gi(OFS_PARM0 + 3, 1); // the player edict
        bi_writeentity(&mut server.vm).expect("bi_writeentity");
        for v in [10.0, 20.0, 30.0, 40.0, 50.0, 60.0] {
            write_coord(&mut server, 0, v);
        }
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].te_type, TE_LIGHTNING2);
        assert_eq!(
            evs[0].entity, 1,
            "WriteEntity's int edict number survives the decode"
        );
        assert_eq!(evs[0].pos, [10.0, 20.0, 30.0]);
        assert_eq!(evs[0].end, [40.0, 50.0, 60.0]);
    }

    #[test]
    fn te_non_broadcast_dest_produces_no_event() {
        // Writes on MSG_ONE (dest 1) are ignored: no temp entity is decoded.
        let mut server = te_server();
        write_byte(&mut server, 1, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 1, TE_EXPLOSION as f32);
        write_coord(&mut server, 1, 16.0);
        write_coord(&mut server, 1, 32.0);
        write_coord(&mut server, 1, 48.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "MSG_ONE writes produce no broadcast temp entity"
        );
    }

    #[test]
    fn te_unknown_type_resets_and_next_message_still_parses() {
        // An unknown type byte resets the decoder cleanly (no event), and a
        // following valid message must still parse.
        let mut server = te_server();
        // Unknown type 200 -> reset, drop.
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, 200.0); // not a known TE_*
        // These stray coords land in Idle and are ignored.
        write_coord(&mut server, 0, 9.0);
        write_coord(&mut server, 0, 9.0);
        write_coord(&mut server, 0, 9.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "unknown type emits nothing"
        );

        // A clean, valid message right after still decodes.
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_SPIKE as f32);
        write_coord(&mut server, 0, 7.0);
        write_coord(&mut server, 0, 8.0);
        write_coord(&mut server, 0, 9.0);
        let evs = server.drain_temp_entities();
        assert_eq!(evs.len(), 1, "the next valid message parses after a reset");
        assert_eq!(evs[0].te_type, TE_SPIKE);
        assert_eq!(evs[0].pos, [7.0, 8.0, 9.0]);
    }

    #[test]
    fn te_decoder_reset_drops_partial_message() {
        // A half-collected message (svc + type + one coord) is dropped by a
        // frame reset; a fresh message after the reset parses cleanly.
        let mut server = te_server();
        write_byte(&mut server, 0, SVC_TEMP_ENTITY as f32);
        write_byte(&mut server, 0, TE_EXPLOSION as f32);
        write_coord(&mut server, 0, 1.0); // only one of three coords
        server.outbox().expect("outbox").reset_parsers(); // frame boundary
        // Continuing the old coords now must NOT complete a stale message.
        write_coord(&mut server, 0, 2.0);
        write_coord(&mut server, 0, 3.0);
        assert!(
            server.drain_temp_entities().is_empty(),
            "reset dropped the partial temp entity"
        );
    }
}
