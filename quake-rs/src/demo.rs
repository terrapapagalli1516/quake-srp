//! Quake client demo (`.dem`) parser and network-message decoder.
//!
//! Ported from Quake (GPLv2). This module faithfully reproduces the byte-level
//! decoding of the Quake network protocol (version 15) and the demo file
//! framing, derived from id Software's original C sources:
//!
//! * `cl_demo.c`   — demo file framing (`CL_GetMessage`, `CL_PlayDemo_f`).
//! * `cl_parse.c`  — server message demux (`CL_ParseServerMessage`),
//!   `CL_ParseServerInfo`, `CL_ParseUpdate`, `CL_ParseBaseline`,
//!   `CL_ParseClientdata`, `CL_ParseStartSoundPacket`, `CL_ParseStatic`,
//!   `CL_ParseStaticSound`.
//! * `cl_tent.c`   — temporary-entity sizes (`CL_ParseTEnt`, `CL_ParseBeam`).
//! * `protocol.h`  — `svc_*`, `U_*`, `SU_*`, `SND_*` defines,
//!   `PROTOCOL_VERSION = 15`, `DEFAULT_VIEWHEIGHT = 22`.
//! * `common.c`    — the `MSG_Read*` reader encodings (~lines 600-725).
//!
//! Copyright (C) 1996-1997 Id Software, Inc. (original C).
//! Ported to memory-safe Rust under the GNU General Public License v2.
//!
//! Design notes
//! ------------
//! The original engine called `Sys_Error`/`Host_Error` (a `longjmp`) on any
//! malformed input. Here we instead return [`QError`] and *never* panic, unwrap,
//! index out of bounds, or use `unsafe`. The MSG readers mirror the C behavior
//! exactly: reading past the end of a message yields `-1` (or NaN/empty) and
//! sets an internal "bad read" flag, just like `msg_badread`.

#![forbid(unsafe_code)]

use crate::error::{QError, Result};
use crate::server::{ParticleBurst, StaticSound, TempEntityEvent};

// ---------------------------------------------------------------------------
// protocol.h constants
// ---------------------------------------------------------------------------

/// Network protocol version this parser understands (`PROTOCOL_VERSION`).
pub const PROTOCOL_VERSION: i32 = 15;

/// Default player view height when `SU_VIEWHEIGHT` is absent (`DEFAULT_VIEWHEIGHT`).
const DEFAULT_VIEWHEIGHT: f32 = 22.0;

// Fast-update field bits (`U_*`). The low byte arrives with the command byte;
// the high byte arrives via `U_MOREBITS`.
const U_MOREBITS: i32 = 1 << 0;
const U_ORIGIN1: i32 = 1 << 1;
const U_ORIGIN2: i32 = 1 << 2;
const U_ORIGIN3: i32 = 1 << 3;
const U_ANGLE2: i32 = 1 << 4;
const U_NOLERP: i32 = 1 << 5;
const U_FRAME: i32 = 1 << 6;
// (1<<7) is U_SIGNAL — folded into the command byte's high bit; never read here.
const U_ANGLE1: i32 = 1 << 8;
const U_ANGLE3: i32 = 1 << 9;
const U_MODEL: i32 = 1 << 10;
const U_COLORMAP: i32 = 1 << 11;
const U_SKIN: i32 = 1 << 12;
const U_EFFECTS: i32 = 1 << 13;
const U_LONGENTITY: i32 = 1 << 14;

// Clientdata field bits (`SU_*`).
const SU_VIEWHEIGHT: i32 = 1 << 0;
const SU_IDEALPITCH: i32 = 1 << 1;
const SU_PUNCH1: i32 = 1 << 2;
// SU_PUNCH2 / SU_PUNCH3 / SU_VELOCITY{1,2,3} are derived via (SU_PUNCH1 << i)
// and (SU_VELOCITY1 << i) below, exactly as in CL_ParseClientdata.
const SU_VELOCITY1: i32 = 1 << 5;
// (1<<8) is unused (was SU_AIMENT).
// SU_ITEMS / SU_ONGROUND / SU_INWATER carry no extra bytes that we must skip
// beyond the always-present long; only the data-bearing bits below matter.
const SU_WEAPONFRAME: i32 = 1 << 12;
const SU_ARMOR: i32 = 1 << 13;
const SU_WEAPON: i32 = 1 << 14;

// Sound field bits (`SND_*`) for CL_ParseStartSoundPacket.
const SND_VOLUME: i32 = 1 << 0;
const SND_ATTENUATION: i32 = 1 << 1;
// SND_LOOPING (1<<2) is never emitted to clients in protocol 15.

// Server-to-client command bytes (`svc_*`). svc_bad (0) and the obsolete
// svc_spawnbinary (21) are intentionally absent: they fall through the demux's
// catch-all `Err` arm (illegible message), exactly as the C engine treated them.
const SVC_NOP: i32 = 1;
const SVC_DISCONNECT: i32 = 2;
const SVC_UPDATESTAT: i32 = 3;
const SVC_VERSION: i32 = 4;
const SVC_SETVIEW: i32 = 5;
const SVC_SOUND: i32 = 6;
const SVC_TIME: i32 = 7;
const SVC_PRINT: i32 = 8;
const SVC_STUFFTEXT: i32 = 9;
const SVC_SETANGLE: i32 = 10;
const SVC_SERVERINFO: i32 = 11;
const SVC_LIGHTSTYLE: i32 = 12;
const SVC_UPDATENAME: i32 = 13;
const SVC_UPDATEFRAGS: i32 = 14;
const SVC_CLIENTDATA: i32 = 15;
const SVC_STOPSOUND: i32 = 16;
const SVC_UPDATECOLORS: i32 = 17;
const SVC_PARTICLE: i32 = 18;
const SVC_DAMAGE: i32 = 19;
const SVC_SPAWNSTATIC: i32 = 20;
// 21 == OBSOLETE svc_spawnbinary (never valid).
const SVC_SPAWNBASELINE: i32 = 22;
const SVC_TEMP_ENTITY: i32 = 23;
const SVC_SETPAUSE: i32 = 24;
const SVC_SIGNONNUM: i32 = 25;
const SVC_CENTERPRINT: i32 = 26;
const SVC_KILLEDMONSTER: i32 = 27;
const SVC_FOUNDSECRET: i32 = 28;
const SVC_SPAWNSTATICSOUND: i32 = 29;
const SVC_INTERMISSION: i32 = 30;
const SVC_FINALE: i32 = 31;
const SVC_CDTRACK: i32 = 32;
const SVC_SELLSCREEN: i32 = 33;
const SVC_CUTSCENE: i32 = 34;

// Model header flags (`model.h`). `EF_ROTATE` is a *model* flag (the MDL file
// header's `flags` field), NOT one of the entity-effects bits in
// `CL_ParseUpdate`'s `U_EFFECTS` byte. It marks bonus pickups that spin in
// place; CL_RelinkEntities forces their yaw to `anglemod(100*cl.time)` every
// frame regardless of the recorded angles. Numerically it equals 8 — the same
// value as the unrelated entity-effects bit `EF_DIMLIGHT` — but the two live in
// different namespaces (model->flags vs entity->effects) and never interact.
pub const EF_ROTATE: i32 = 8;

// Temp-entity types (`TE_*`).
const TE_SPIKE: i32 = 0;
const TE_SUPERSPIKE: i32 = 1;
const TE_GUNSHOT: i32 = 2;
const TE_EXPLOSION: i32 = 3;
const TE_TAREXPLOSION: i32 = 4;
const TE_LIGHTNING1: i32 = 5;
const TE_LIGHTNING2: i32 = 6;
const TE_WIZSPIKE: i32 = 7;
const TE_KNIGHTSPIKE: i32 = 8;
const TE_LIGHTNING3: i32 = 9;
const TE_LAVASPLASH: i32 = 10;
const TE_TELEPORT: i32 = 11;
const TE_EXPLOSION2: i32 = 12;
const TE_BEAM: i32 = 13;

// Defensive limits mirroring the C engine's fixed arrays, used only to refuse
// absurd allocations from a hostile demo (never to silently truncate valid
// data). The C engine used MAX_EDICTS=600 but cl.protocol demos can legally
// reference larger numbers in modded content; we use a generous cap.
const MAX_ENTITIES: usize = 1 << 20;
const MAX_PRECACHE: usize = 1 << 16;

// ---------------------------------------------------------------------------
// MSG reader (common.c)
// ---------------------------------------------------------------------------

/// A cursor over one server message, mirroring `common.c`'s `MSG_Read*`.
///
/// Reading past the end of the buffer sets [`bad`](NetReader::bad) and returns
/// a sentinel (`-1` for char/byte/short/long, `0.0`/NaN-safe for float, an
/// empty fragment for strings), exactly like the original `msg_badread` flag.
/// Callers in the demux loop terminate when `ReadByte` returns `-1`.
struct NetReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Set once any read runs off the end of the buffer (`msg_badread`).
    bad: bool,
}

impl<'a> NetReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        NetReader {
            data,
            pos: 0,
            bad: false,
        }
    }

    /// `MSG_ReadChar`: next byte sign-extended to i32, or -1 at end (+ bad flag).
    fn read_char(&mut self) -> i32 {
        if self.pos + 1 > self.data.len() {
            self.bad = true;
            return -1;
        }
        let c = self.data[self.pos] as i8 as i32;
        self.pos += 1;
        c
    }

    /// `MSG_ReadByte`: next byte 0..=255, or -1 at end (+ bad flag).
    fn read_byte(&mut self) -> i32 {
        if self.pos + 1 > self.data.len() {
            self.bad = true;
            return -1;
        }
        let c = self.data[self.pos] as i32;
        self.pos += 1;
        c
    }

    /// `MSG_ReadShort`: little-endian i16 sign-extended to i32, or -1 at end.
    fn read_short(&mut self) -> i32 {
        if self.pos + 2 > self.data.len() {
            self.bad = true;
            return -1;
        }
        let lo = self.data[self.pos] as i32;
        let hi = self.data[self.pos + 1] as i32;
        // (short)(lo + (hi<<8)): build u16 then sign-extend through i16.
        let c = ((lo + (hi << 8)) as u16) as i16 as i32;
        self.pos += 2;
        c
    }

    /// `MSG_ReadLong`: little-endian i32, or -1 at end (+ bad flag).
    fn read_long(&mut self) -> i32 {
        if self.pos + 4 > self.data.len() {
            self.bad = true;
            return -1;
        }
        let b0 = self.data[self.pos] as u32;
        let b1 = self.data[self.pos + 1] as u32;
        let b2 = self.data[self.pos + 2] as u32;
        let b3 = self.data[self.pos + 3] as u32;
        let c = (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) as i32;
        self.pos += 4;
        c
    }

    /// `MSG_ReadFloat`: little-endian f32, or 0.0 at end (+ bad flag).
    ///
    /// The C reader read uninitialized bytes off the end without bounds
    /// checking; we instead flag the bad read and return 0.0 so we never read
    /// out of bounds.
    fn read_float(&mut self) -> f32 {
        if self.pos + 4 > self.data.len() {
            self.bad = true;
            return 0.0;
        }
        let bytes = [
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ];
        self.pos += 4;
        f32::from_le_bytes(bytes)
    }

    /// `MSG_ReadString`: bytes until a 0 byte or end-of-buffer.
    ///
    /// Mirrors the C loop, which reads via `MSG_ReadChar` (stopping on -1 or 0)
    /// and caps at 2047 characters. Non-UTF-8 bytes are mapped lossily so we
    /// always produce a `String` without panicking.
    fn read_string(&mut self) -> String {
        let mut bytes: Vec<u8> = Vec::new();
        // The C buffer is `char string[2048]`, room for 2047 chars + NUL.
        while bytes.len() < 2047 {
            let c = self.read_char();
            if c == -1 || c == 0 {
                break;
            }
            bytes.push(c as u8);
        }
        // Quake strings are Latin-1-ish; lossy conversion never panics.
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// `MSG_ReadCoord`: short / 8.0.
    fn read_coord(&mut self) -> f32 {
        self.read_short() as f32 * (1.0 / 8.0)
    }

    /// `MSG_ReadAngle`: char * 360/256.
    fn read_angle(&mut self) -> f32 {
        self.read_char() as f32 * (360.0 / 256.0)
    }
}

// ---------------------------------------------------------------------------
// Public data types consumed by the renderer
// ---------------------------------------------------------------------------

/// A renderable snapshot of one entity at one demo frame.
///
/// `origin`/`angles` are the *interpolated* render values (`ent->origin` /
/// `ent->angles` after `CL_RelinkEntities`), already lerped between the two most
/// recent server snapshots by the message-time fraction. `effects` is the
/// entity-effects byte (`ent->effects`, the `U_EFFECTS` field) so a front-end
/// can drive dynamic lights / brightfield particles exactly like the live walk.
#[derive(Clone, Copy)]
pub struct EntSnapshot {
    pub modelindex: usize,
    pub frame: i32,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    /// `ent->effects` (the `U_EFFECTS` byte). 0 when the update omitted it.
    pub effects: i32,
}

/// One playback frame: the camera, every visible entity, and the one-shot
/// effect events the server multiplexed into this block.
///
/// `particles` are the `svc_particle` ([`SVC_PARTICLE`]) bursts decoded from
/// `R_ParseParticleEffect` (one per message); `temp_entities` are the
/// `svc_temp_entity` ([`SVC_TEMP_ENTITY`]) effects (gunshot/explosion/spike
/// impacts) decoded from `CL_ParseTEnt`. A front-end replays each ONCE, on the
/// step that advances playback onto this frame, through the same
/// [`crate::particles::ParticleSystem`] the live walk uses — so the recorded
/// demo shows blood, gunshot puffs and explosions exactly like live play.
///
/// Beam temp entities (`TE_LIGHTNING1/2/3`, `TE_BEAM`) carry their owning
/// entity + start/end points on the [`TempEntityEvent`]; a front-end routes
/// them into a [`crate::tent::Beams`] store (the `CL_ParseBeam` slot list) and
/// expands the live beams into bolt-model instances each frame
/// (`CL_UpdateTEnts`).
pub struct DemoFrame {
    pub time: f32,
    pub view_origin: [f32; 3],
    pub view_angles: [f32; 3],
    /// The view entity's raw origin (`cl_entities[cl.viewentity].origin` — the
    /// pre-`viewheight` base of `view_origin`). `CL_UpdateTEnts` re-anchors a
    /// beam owned by the view entity to THIS each frame, so the recorded
    /// player's thunderbolt tracks them between beam refreshes.
    pub view_entity_origin: [f32; 3],
    pub entities: Vec<EntSnapshot>,
    /// `svc_particle` bursts fired during this frame's message block.
    pub particles: Vec<ParticleBurst>,
    /// `svc_temp_entity` effects fired during this frame's message block.
    pub temp_entities: Vec<TempEntityEvent>,
}

/// A fully parsed demo: level metadata, precache tables, and all frames.
pub struct Demo {
    pub level_name: String,
    pub model_precache: Vec<String>,
    pub sound_precache: Vec<String>,
    /// `cl.viewentity` (the `svc_setview` entity number, the recording
    /// player): the beam store's view-entity key for `CL_UpdateTEnts`'s
    /// start-position tracking. `0` if the demo never set a view.
    pub viewentity: usize,
    /// The placed looping ambient sounds (`svc_spawnstaticsound`,
    /// `CL_ParseStaticSound`) recorded in the demo's signon — torch crackles,
    /// wind, hums. These are persistent loops, not per-frame events: a
    /// front-end starts them once at playback start, exactly as the live
    /// client did on connect. Volume/attenuation are decoded back to the
    /// QuakeC domain (`byte/255`, `byte/64`) like `S_StaticSound` does.
    pub static_sounds: Vec<StaticSound>,
    pub frames: Vec<DemoFrame>,
}

impl Demo {
    /// The world map name, i.e. `model_precache[1]` (e.g. `"maps/e1m1.bsp"`).
    pub fn map_name(&self) -> Option<&str> {
        self.model_precache.get(1).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Internal client state (the parts of client_state_t we replay)
// ---------------------------------------------------------------------------

/// An entity baseline + current state, mirroring the fields of `entity_t`
/// that affect rendering. (`CL_ParseBaseline` / `CL_ParseUpdate`.)
#[derive(Clone, Copy, Default)]
struct Entity {
    // Baseline (default state, restored when an update omits a field).
    base_modelindex: i32,
    base_frame: i32,
    base_origin: [f32; 3],
    base_angles: [f32; 3],
    // Current state. A slot grown by `CL_EntityNum` but never spawned/updated
    // keeps `modelindex == 0` and is therefore invisible in the snapshot.
    modelindex: i32,
    frame: i32,
    effects: i32,
    // Interpolation history (CL_ParseUpdate / CL_RelinkEntities):
    // `msg_origins[0]`/`msg_angles[0]` is the most recent server snapshot,
    // `[1]` the one before it. The render `origin`/`angles` are lerped between
    // them by the message-time fraction. We DON'T keep a separate `origin`
    // field — the snapshot computes it via [`lerp_origin`]/[`lerp_angles`].
    msg_origins: [[f32; 3]; 2],
    msg_angles: [[f32; 3]; 2],
    /// `ent->msgtime` — the `cl.mtime[0]` at which this entity was last updated.
    /// An entity whose `msgtime` falls behind `mtime[0]` went silent and is
    /// culled (its model dropped), matching CL_RelinkEntities.
    msgtime: f32,
    /// `ent->forcelink` — set when there was no previous frame to lerp from
    /// (first sighting, null-model hack, or `U_NOLERP`). Forces the render
    /// state straight to `msg_origins[0]` with no interpolation.
    forcelink: bool,
    /// True once this slot has been spawned (baseline) or updated at least
    /// once. A grown-but-never-touched slot stays invisible.
    active: bool,
}

/// The replayed client state (`client_state_t` subset).
struct ClientState {
    have_serverinfo: bool,
    level_name: String,
    model_precache: Vec<String>,
    sound_precache: Vec<String>,
    entities: Vec<Entity>,
    statics: Vec<Entity>,
    viewentity: usize,
    viewheight: f32,
    /// `cl.viewangles` — the camera angles. In demo playback these are driven
    /// by the recorded block angles ([`mviewangles`](ClientState::mviewangles)),
    /// lerped by the message-time fraction in CL_RelinkEntities; `svc_setangle`
    /// is recorded into `mviewangles` but must NOT clobber the lerped result.
    view_angles: [f32; 3],
    /// `cl.mviewangles[0]`/`[1]` — the camera angles of the two most recent
    /// demo blocks (`CL_GetMessage` shifts `[0]`→`[1]` and reads the new `[0]`
    /// from the block header). The render camera lerps between them.
    mviewangles: [[f32; 3]; 2],
    time: f32,
    /// `cl.mtime[0]`/`[1]` — the server times of the two most recent
    /// `svc_time` messages. `svc_time` shifts `[0]`→`[1]` then reads the new
    /// `[0]`; the interpolation fraction is `(t - mtime[1])/(mtime[0]-mtime[1])`.
    mtime: [f32; 2],
    /// `svc_particle` bursts decoded since the last [`snapshot`], drained into
    /// the [`DemoFrame`] for the block they arrived in and then cleared.
    pending_particles: Vec<ParticleBurst>,
    /// `svc_temp_entity` effects decoded since the last [`snapshot`], drained
    /// into the [`DemoFrame`] and then cleared (one block == one frame).
    pending_tents: Vec<TempEntityEvent>,
    /// `svc_spawnstaticsound` registrations (`CL_ParseStaticSound` ->
    /// `S_StaticSound` persistent loops), accumulated for [`Demo::static_sounds`].
    static_sounds: Vec<StaticSound>,
}

impl ClientState {
    /// `CL_ClearState` — reset everything on a fresh serverinfo.
    fn new() -> Self {
        ClientState {
            have_serverinfo: false,
            level_name: String::new(),
            model_precache: Vec::new(),
            sound_precache: Vec::new(),
            entities: Vec::new(),
            statics: Vec::new(),
            viewentity: 0,
            viewheight: DEFAULT_VIEWHEIGHT,
            view_angles: [0.0; 3],
            mviewangles: [[0.0; 3]; 2],
            time: 0.0,
            mtime: [0.0; 2],
            pending_particles: Vec::new(),
            pending_tents: Vec::new(),
            static_sounds: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.level_name.clear();
        self.model_precache.clear();
        self.sound_precache.clear();
        self.entities.clear();
        self.statics.clear();
        self.viewentity = 0;
        self.viewheight = DEFAULT_VIEWHEIGHT;
        // CL_ClearState memsets the whole cl struct: the interpolation history
        // (server times and camera angles) restarts from zero on a new level.
        self.mtime = [0.0; 2];
        self.mviewangles = [[0.0; 3]; 2];
        self.view_angles = [0.0; 3];
        self.time = 0.0;
        // A fresh server clears any effects half-collected for the previous
        // level (CL_ClearState wipes the client-side effect pools too).
        self.pending_particles.clear();
        self.pending_tents.clear();
        // S_StopAllSounds on the new serverinfo drops the old level's static
        // loop channels; the new signon re-registers its own.
        self.static_sounds.clear();
    }

    /// `CL_EntityNum` — grow the entity array up to and including `num`.
    /// Returns `None` if `num` is absurdly large (hostile demo), to refuse
    /// runaway allocation rather than crash.
    fn entity_num(&mut self, num: usize) -> Option<usize> {
        if num >= MAX_ENTITIES {
            return None;
        }
        if num >= self.entities.len() {
            self.entities.resize(num + 1, Entity::default());
        }
        Some(num)
    }
}

/// Result of parsing one server message: did the stream ask us to stop?
enum ParseFlow {
    /// Continue reading further demo blocks.
    Continue,
    /// `svc_disconnect` — stop playback cleanly (not an error).
    Stop,
}

// ---------------------------------------------------------------------------
// Demo entry point (cl_demo.c framing + replay loop)
// ---------------------------------------------------------------------------

/// Parse a Quake `.dem` file from memory and replay it into [`Demo`].
///
/// This is the *keyframe* stream: exactly one [`DemoFrame`] per server message
/// block, each holding the authoritative post-message state (interpolation
/// fraction `frac == 1`, i.e. every entity at its newest snapshot) with the
/// stale-entity cull from `CL_RelinkEntities` applied — entities that went
/// silent in the latest message stop rendering instead of lingering. For smooth
/// (non-choppy) playback that lerps between snapshots, use
/// [`parse_demo_interpolated`].
///
/// Framing (`CL_PlayDemo_f` + `CL_GetMessage`): an ASCII CD-track integer
/// followed by `'\n'`, then a sequence of blocks, each
/// `[i32 length][3 × f32 view angles][length bytes of message]`. Parsing stops
/// at end of file, on `svc_disconnect`, or when a block would overrun the file.
///
/// A malformed or truncated demo produces a short/empty frame list or an
/// `Err`, never a panic.
pub fn parse_demo(bytes: &[u8]) -> Result<Demo> {
    // Keyframe emission: one frame per block at frac == 1 (cl.time == mtime[0]),
    // with no model-flag knowledge (no EF_ROTATE spin in the raw keyframe).
    parse_demo_with(bytes, |cl, _prev_mtime0, frames| {
        cl.time = cl.mtime[0];
        frames.push(snapshot(cl, 1.0, &|_| false));
    })
}

/// Parse a `.dem` file into a *smooth* frame stream by interpolating between the
/// recorded 10 Hz server snapshots, exactly like `CL_RelinkEntities` does every
/// display frame: entity origins/angles and the demo camera are lerped between
/// the two most recent snapshots by the message-time fraction
/// (`CL_LerpPoint`), with teleport detection (>100 unit jumps snap) and
/// shortest-arc angle wrap. Roughly `fps` frames are produced per real second
/// of demo time (clamped to at least one per block), turning the choppy 10 Hz
/// capture into fluid motion.
///
/// `rotating_models` is the set of `modelindex` values whose loaded MDL carries
/// the `EF_ROTATE` header flag (bonus pickups). For each such entity the yaw is
/// forced to `anglemod(100*time)` so it spins during playback. The demo parser
/// cannot read MDL headers itself (it has no model loader), so the caller
/// supplies this set after precaching the models; pass an empty slice to skip
/// the spin.
pub fn parse_demo_interpolated(
    bytes: &[u8],
    fps: f32,
    rotating_models: &[usize],
) -> Result<Demo> {
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 60.0 };
    let is_rotating = |m: usize| rotating_models.contains(&m);

    parse_demo_with(bytes, |cl, prev_mtime0, frames| {
        let mtime = cl.mtime;
        let interval = mtime[0] - prev_mtime0;
        // A zero or negative interval (first frame, or no svc_time advance this
        // block) yields a single snapshot at frac == 1.
        if interval <= 0.0 {
            cl.time = mtime[0];
            frames.push(snapshot(cl, 1.0, &is_rotating));
            return;
        }
        // CL_LerpPoint clamps gaps > 0.1s (dropped packet / start of demo) to a
        // 0.1s window, so the engine only ever interpolates over the last 0.1s
        // before mtime[0]. We sweep cl.time across exactly that window
        // [mtime[0]-window, mtime[0]] to avoid emitting a flood of degenerate
        // frac==0 frames on a big gap, while still producing smooth motion.
        let window = interval.min(0.1);
        let steps = ((window * fps).round() as i32).max(1);
        // The effect events (particles/tents) live on the FIRST sub-frame only
        // (snapshot drains them), so later sub-frames of this block are empty —
        // matching one effect-burst per server message.
        for s in 1..=steps {
            let t = mtime[0] - window + window * (s as f32 / steps as f32);
            cl.time = t;
            let frac = lerp_point(mtime, t);
            frames.push(snapshot(cl, frac, &is_rotating));
        }
    })
}

/// Shared demo replay core: parse the framing + every message block, calling
/// `emit` once per block (after a world exists) to turn the current client
/// state into zero or more [`DemoFrame`]s. `emit` receives the client state,
/// the `mtime[0]` value from *before* this block (the start of the
/// interpolation interval), and the output frame list.
fn parse_demo_with(
    bytes: &[u8],
    mut emit: impl FnMut(&mut ClientState, f32, &mut Vec<DemoFrame>),
) -> Result<Demo> {
    // --- Skip the CD-track header line: digits/'-' up to and including '\n'.
    // CL_PlayDemo_f reads bytes until '\n'. If there is no newline at all the
    // file is not a demo.
    let mut pos = 0usize;
    let mut found_newline = false;
    while pos < bytes.len() {
        let b = bytes[pos];
        pos += 1;
        if b == b'\n' {
            found_newline = true;
            break;
        }
    }
    if !found_newline {
        return Err(QError::invalid("demo: missing CD-track header newline"));
    }

    let mut cl = ClientState::new();
    let mut frames: Vec<DemoFrame> = Vec::new();

    // --- Block loop (CL_GetMessage demo branch).
    loop {
        // Need 4 (length) + 12 (3 floats) = 16 bytes of framing.
        if pos + 4 > bytes.len() {
            break; // clean EOF (or trailing garbage shorter than a header)
        }
        let len = i32::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
        ]);
        pos += 4;

        // A negative or nonsensical length means a corrupt stream: stop cleanly.
        if len < 0 {
            break;
        }
        let len = len as usize;

        // Three little-endian view angles.
        if pos + 12 > bytes.len() {
            break; // truncated framing
        }
        let mut block_angles = [0.0f32; 3];
        for a in block_angles.iter_mut() {
            *a = f32::from_le_bytes([
                bytes[pos],
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
            ]);
            pos += 4;
        }

        // The message body. If it overruns the file (or the addition would
        // overflow on a 32-bit host), stop cleanly (CL_StopPlayback). Using
        // checked arithmetic keeps us panic-free even in debug builds.
        let end = match pos.checked_add(len) {
            Some(e) if e <= bytes.len() => e,
            _ => break,
        };
        let msg = &bytes[pos..end];
        pos = end;

        // CL_GetMessage shifts the previous block's camera angles into
        // mviewangles[1] and records this block's angles into mviewangles[0],
        // so CL_RelinkEntities can lerp the demo camera between them. (The
        // shift happens BEFORE parsing the message body, exactly as in C.)
        cl.mviewangles[1] = cl.mviewangles[0];
        cl.mviewangles[0] = block_angles;

        // Remember the previous server time so the interpolation path can sweep
        // cl.time across this block's [mtime[1], mtime[0]] interval.
        let prev_mtime0 = cl.mtime[0];

        // Parse the message, updating client state (this may advance mtime via
        // svc_time, set up entity msg history, etc.).
        let flow = parse_server_message(&mut cl, msg)?;

        // Snapshot AFTER the block (only once we have a world). `snapshot`
        // drains this block's pending effect events into the frame and clears
        // them, so the next block starts collecting from empty.
        if cl.have_serverinfo {
            emit(&mut cl, prev_mtime0, &mut frames);
        } else {
            // Before the world exists no frame is emitted, so any stray effect
            // events parsed in a pre-serverinfo block would otherwise leak into
            // the first real frame. Drop them to keep frame N's lists == the
            // events of block N.
            cl.pending_particles.clear();
            cl.pending_tents.clear();
        }

        if let ParseFlow::Stop = flow {
            break;
        }
    }

    Ok(Demo {
        viewentity: cl.viewentity,
        level_name: cl.level_name,
        model_precache: cl.model_precache,
        sound_precache: cl.sound_precache,
        static_sounds: cl.static_sounds,
        frames,
    })
}

/// Build a [`DemoFrame`] from the current client state.
///
/// `view_origin = entities[viewentity].origin` with `+viewheight` on Z; the
/// entity list is every regular entity with `modelindex > 0` plus every static.
///
/// The per-frame effect events (`svc_particle` / `svc_temp_entity`) collected
/// since the last snapshot are *moved* out of the client state into the frame
/// (leaving the pending lists empty), so each [`DemoFrame`] owns exactly the
/// effects of its own message block and the next block starts fresh.
///
/// `frac` is the message-time interpolation fraction (`CL_LerpPoint`'s result,
/// 0..=1): 0 puts every entity at the *older* snapshot, 1 at the most recent.
/// `is_rotating` answers, for a given `modelindex`, whether the model carries
/// the `EF_ROTATE` header flag (bonus pickups that spin); only the front-end
/// knows the loaded MDL's flags, so this is supplied by the caller. The
/// authoritative keyframe stream (`parse_demo`) passes `frac == 1` and a
/// closure that always returns `false`.
fn snapshot(cl: &mut ClientState, frac: f32, is_rotating: &dyn Fn(usize) -> bool) -> DemoFrame {
    // bobjrotate = anglemod(100*cl.time): the spin angle shared by every
    // EF_ROTATE model this frame.
    let bobjrotate = crate::math::anglemod(100.0 * cl.time);

    // --- Camera: in demo playback the recorded angles drive the view, lerped
    // between mviewangles[1] and mviewangles[0] (CL_RelinkEntities). The view
    // origin tracks the (lerped) view entity origin + the view height.
    let mut view_origin = [0.0f32; 3];
    if let Some(ve) = cl.entities.get(cl.viewentity) {
        // The view entity is interpolated like any other entity.
        view_origin = if ve.forcelink {
            ve.msg_origins[0]
        } else {
            lerp_origin(ve.msg_origins[1], ve.msg_origins[0], frac)
        };
    }
    // The raw (pre-viewheight) view entity origin — what CL_UpdateTEnts
    // re-anchors the view entity's own beam start to each frame.
    let view_entity_origin = view_origin;
    view_origin[2] += cl.viewheight;
    let view_angles = lerp_angles(cl.mviewangles[1], cl.mviewangles[0], frac);

    let mut entities: Vec<EntSnapshot> = Vec::new();
    for (i, e) in cl.entities.iter().enumerate() {
        // Skip the view entity: Quake hides the local player's own model in
        // first person (otherwise it fills the screen at the camera origin).
        if i == cl.viewentity {
            continue;
        }
        // CL_RelinkEntities: empty slots (no model) are skipped, and any entity
        // that was NOT included in the most recent server message (its msgtime
        // fell behind mtime[0]) is removed — it went silent and must stop
        // rendering instead of lingering at its last position forever.
        if !e.active || e.modelindex <= 0 {
            continue;
        }
        if e.msgtime != cl.mtime[0] {
            continue;
        }

        let (origin, mut angles) = relink_lerp(e, frac);

        // Rotate binary objects (bonus pickups) locally: EF_ROTATE forces the
        // yaw to anglemod(100*time) every frame, overriding the interpolated
        // value. The model flag is resolved by the caller from the loaded MDL.
        if is_rotating(e.modelindex as usize) {
            angles[1] = bobjrotate;
        }

        entities.push(EntSnapshot {
            modelindex: e.modelindex as usize,
            frame: e.frame,
            origin,
            angles,
            effects: e.effects,
        });
    }
    for e in &cl.statics {
        // Statics are always emitted (they were spawned with a model) and never
        // move, so their lerp is a no-op; still honour EF_ROTATE for rotating
        // static pickups.
        let mut angles = e.msg_angles[0];
        if is_rotating(e.modelindex.max(0) as usize) {
            angles[1] = bobjrotate;
        }
        entities.push(EntSnapshot {
            modelindex: e.modelindex.max(0) as usize,
            frame: e.frame,
            origin: e.msg_origins[0],
            angles,
            effects: e.effects,
        });
    }

    // Move this block's accumulated effect events into the frame and leave the
    // client's pending lists empty for the next block (std::mem::take swaps in
    // a fresh empty Vec without cloning).
    let particles = std::mem::take(&mut cl.pending_particles);
    let temp_entities = std::mem::take(&mut cl.pending_tents);

    DemoFrame {
        time: cl.time,
        view_origin,
        view_entity_origin,
        view_angles,
        entities,
        particles,
        temp_entities,
    }
}

/// `CL_LerpPoint` — the fraction of the way `cl.time` lies between the two most
/// recent server message times (`mtime[1]` → `mtime[0]`), clamped to 0..=1.
///
/// Mirrors `cl_main.c`: a zero interval (or a back-end that disables lerp)
/// returns 1 with `cl.time` snapped to `mtime[0]`; a gap larger than 0.1 s
/// (dropped packet / start of demo) is treated as a 0.1 s interval. We do NOT
/// model `cl_nolerp`/`timedemo`/`sv.active` (no live back-end here); the keyframe
/// path forces `frac == 1` explicitly instead.
fn lerp_point(mtime: [f32; 2], time: f32) -> f32 {
    let mut f = mtime[0] - mtime[1];
    if f == 0.0 {
        return 1.0;
    }
    let m1 = if f > 0.1 {
        // Dropped packet or start of demo: clamp the interval to 0.1 s.
        f = 0.1;
        mtime[0] - 0.1
    } else {
        mtime[1]
    };
    let frac = (time - m1) / f;
    frac.clamp(0.0, 1.0)
}

/// Linear interpolation of an origin vector by `frac`, with the teleport guard
/// from `CL_RelinkEntities`: if any axis moved more than 100 units between the
/// two snapshots, snap (`frac == 1`) instead of lerping (assume a teleport).
fn lerp_origin(from: [f32; 3], to: [f32; 3], frac: f32) -> [f32; 3] {
    let mut f = frac;
    for j in 0..3 {
        let delta = to[j] - from[j];
        // Kept as the literal C expression `delta > 100 || delta < -100` from
        // CL_RelinkEntities for faithfulness (not the equivalent range form).
        #[allow(clippy::manual_range_contains)]
        if delta > 100.0 || delta < -100.0 {
            f = 1.0; // teleport, not motion
        }
    }
    [
        from[0] + f * (to[0] - from[0]),
        from[1] + f * (to[1] - from[1]),
        from[2] + f * (to[2] - from[2]),
    ]
}

/// Angular interpolation by `frac` with the shortest-arc wraparound from
/// `CL_RelinkEntities`: each axis delta is folded into (-180, 180] before
/// lerping so the spin takes the short way around.
fn lerp_angles(from: [f32; 3], to: [f32; 3], frac: f32) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for j in 0..3 {
        let mut d = to[j] - from[j];
        if d > 180.0 {
            d -= 360.0;
        } else if d < -180.0 {
            d += 360.0;
        }
        out[j] = from[j] + frac * d;
    }
    out
}

/// Compute one entity's interpolated `(origin, angles)` exactly like
/// `CL_RelinkEntities`: a `forcelink` entity snaps to its newest snapshot
/// (`msg_origins[0]`/`msg_angles[0]`), otherwise it lerps from `[1]` to `[0]`
/// by `frac` with the teleport guard and the shortest-arc angle wrap.
fn relink_lerp(e: &Entity, frac: f32) -> ([f32; 3], [f32; 3]) {
    if e.forcelink {
        (e.msg_origins[0], e.msg_angles[0])
    } else {
        (
            lerp_origin(e.msg_origins[1], e.msg_origins[0], frac),
            lerp_angles(e.msg_angles[1], e.msg_angles[0], frac),
        )
    }
}

/// `bobjrotate = anglemod(100*time)` — the yaw of every `EF_ROTATE` bonus
/// pickup at the given demo `time`, exactly as `CL_RelinkEntities` computes it.
///
/// A front-end that loads the MDL headers can apply this directly to any entity
/// whose model carries the `EF_ROTATE` flag (see [`EF_ROTATE`]); the
/// [`parse_demo_interpolated`] path applies it automatically given the set of
/// rotating `modelindex`es.
pub fn rotate_yaw(time: f32) -> f32 {
    crate::math::anglemod(100.0 * time)
}

// ---------------------------------------------------------------------------
// CL_ParseServerMessage — the command demux
// ---------------------------------------------------------------------------

fn parse_server_message(cl: &mut ClientState, msg: &[u8]) -> Result<ParseFlow> {
    let mut r = NetReader::new(msg);

    loop {
        // CL_ParseServerMessage checks msg_badread at the top of each iteration.
        if r.bad {
            return Err(QError::invalid("server message: bad read (desync)"));
        }

        let cmd = r.read_byte();
        if cmd == -1 {
            // End of message (clean).
            return Ok(ParseFlow::Continue);
        }

        // High bit set => fast entity update with bits == cmd & 127.
        if cmd & 128 != 0 {
            parse_update(cl, &mut r, cmd & 127)?;
            continue;
        }

        match cmd {
            SVC_NOP | SVC_KILLEDMONSTER | SVC_FOUNDSECRET | SVC_INTERMISSION
            | SVC_SELLSCREEN => {
                // No payload bytes.
            }

            SVC_TIME => {
                // CL_ParseServerMessage: shift mtime[0]->mtime[1], then read
                // the new server time into mtime[0]. CL_LerpPoint later derives
                // the interpolation fraction from this pair.
                cl.mtime[1] = cl.mtime[0];
                cl.mtime[0] = r.read_float();
            }

            SVC_CLIENTDATA => {
                let bits = r.read_short();
                cl.viewheight = parse_clientdata(&mut r, bits)?;
            }

            SVC_VERSION => {
                let v = r.read_long();
                if v != PROTOCOL_VERSION {
                    return Err(QError::invalid(format!(
                        "svc_version: server protocol {v}, expected {PROTOCOL_VERSION}"
                    )));
                }
            }

            SVC_DISCONNECT => {
                // Host_EndGame — stop playback cleanly.
                return Ok(ParseFlow::Stop);
            }

            SVC_PRINT | SVC_CENTERPRINT | SVC_STUFFTEXT => {
                let _ = r.read_string();
            }

            SVC_DAMAGE => {
                // V_ParseDamage: byte armor, byte blood, 3 coords from[].
                let _ = r.read_byte();
                let _ = r.read_byte();
                let _ = r.read_coord();
                let _ = r.read_coord();
                let _ = r.read_coord();
            }

            SVC_SERVERINFO => {
                parse_serverinfo(cl, &mut r)?;
            }

            SVC_SETANGLE => {
                // svc_setangle writes cl.viewangles in live play, but during
                // DEMO PLAYBACK CL_RelinkEntities recomputes cl.viewangles from
                // the recorded mviewangles every frame (see CL_RelinkEntities's
                // `if (cls.demoplayback)` block), so the setangle value is
                // immediately overwritten and never affects the rendered
                // camera. We therefore consume the three angle bytes purely to
                // keep the stream aligned and DISCARD them — matching the demo
                // path. (Previously this clobbered the recorded camera angles,
                // snapping the view away from the demo's intended viewpoint.)
                let _ = r.read_angle();
                let _ = r.read_angle();
                let _ = r.read_angle();
            }

            SVC_SETVIEW => {
                let v = r.read_short();
                // Negative or absurd view entity: clamp to 0 (the world), which
                // is harmless and avoids an out-of-range index later.
                cl.viewentity = if v >= 0 { v as usize } else { 0 };
            }

            SVC_LIGHTSTYLE => {
                let _ = r.read_byte();
                let _ = r.read_string();
            }

            SVC_SOUND => {
                parse_start_sound(&mut r)?;
            }

            SVC_STOPSOUND => {
                let _ = r.read_short();
            }

            SVC_UPDATENAME => {
                let _ = r.read_byte();
                let _ = r.read_string();
            }

            SVC_UPDATEFRAGS => {
                let _ = r.read_byte();
                let _ = r.read_short();
            }

            SVC_UPDATECOLORS => {
                let _ = r.read_byte();
                let _ = r.read_byte();
            }

            SVC_PARTICLE => {
                parse_particle(cl, &mut r);
            }

            SVC_SPAWNBASELINE => {
                let num = r.read_short();
                if num < 0 {
                    return Err(QError::invalid("svc_spawnbaseline: negative entity"));
                }
                let idx = cl
                    .entity_num(num as usize)
                    .ok_or_else(|| QError::invalid("svc_spawnbaseline: entity number too large"))?;
                let mut ent = cl.entities[idx];
                parse_baseline(&mut r, &mut ent);
                cl.entities[idx] = ent;
            }

            SVC_SPAWNSTATIC => {
                // CL_ParseStatic: parse a baseline into a brand-new static and
                // seed its current/interpolation state from that baseline.
                let ent = spawn_static(&mut r);
                cl.statics.push(ent);
            }

            SVC_TEMP_ENTITY => {
                parse_temp_entity(cl, &mut r)?;
            }

            SVC_SETPAUSE => {
                let _ = r.read_byte();
            }

            SVC_SIGNONNUM => {
                let _ = r.read_byte();
            }

            SVC_UPDATESTAT => {
                let _ = r.read_byte();
                let _ = r.read_long();
            }

            SVC_SPAWNSTATICSOUND => {
                // CL_ParseStaticSound: 3 coords, 3 bytes (sample, vol, atten);
                // hands them to S_StaticSound, which keeps a PERSISTENT looping
                // channel at that point. Recorded into Demo::static_sounds for
                // the front-end to loop. Volume/attenuation come back from the
                // wire bytes to the QuakeC domain the same way S_StaticSound
                // consumed them (master_vol = byte; dist_mult = byte/64/1000).
                let origin = [r.read_coord(), r.read_coord(), r.read_coord()];
                let sound_num = r.read_byte();
                let vol = r.read_byte();
                let atten = r.read_byte();
                let sample = cl
                    .sound_precache
                    .get(sound_num as usize)
                    .cloned()
                    .unwrap_or_default();
                cl.static_sounds.push(StaticSound {
                    origin,
                    sound_index: sound_num,
                    sample,
                    volume: vol as f32 / 255.0,
                    attenuation: atten as f32 / 64.0,
                });
            }

            SVC_CDTRACK => {
                let _ = r.read_byte();
                let _ = r.read_byte();
            }

            SVC_FINALE | SVC_CUTSCENE => {
                let _ = r.read_string();
            }

            // svc_bad (0), OBSOLETE svc_spawnbinary (21), and anything else
            // are illegible — the original called Host_Error.
            _ => {
                return Err(QError::invalid(format!(
                    "illegible server message: unknown command {cmd}"
                )));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CL_ParseUpdate
// ---------------------------------------------------------------------------

fn parse_update(cl: &mut ClientState, r: &mut NetReader, mut bits: i32) -> Result<()> {
    if bits & U_MOREBITS != 0 {
        let i = r.read_byte();
        bits |= i << 8;
    }

    let num = if bits & U_LONGENTITY != 0 {
        r.read_short()
    } else {
        r.read_byte()
    };
    if num < 0 {
        return Err(QError::invalid("entity update: bad entity number"));
    }
    let idx = cl
        .entity_num(num as usize)
        .ok_or_else(|| QError::invalid("entity update: entity number too large"))?;

    // Work on a copy of the entity, then store it back. (Borrow-checker
    // friendly and avoids holding a &mut across NetReader calls.)
    let mut ent = cl.entities[idx];

    // CL_ParseUpdate: forcelink is set when this entity had no update in the
    // *previous* message (its msgtime is stale relative to mtime[1]) — there is
    // no prior snapshot to interpolate from, so we must snap, not lerp. We then
    // stamp the entity with the current message time so the relink-cull can see
    // it was touched this frame.
    let prev_modelindex = ent.modelindex;
    let mut forcelink = ent.msgtime != cl.mtime[1];
    ent.msgtime = cl.mtime[0];
    ent.active = true;

    // Order matches CL_ParseUpdate EXACTLY: model, frame, colormap, skin,
    // effects, then origin1, angle1, origin2, angle2, origin3, angle3.
    ent.modelindex = if bits & U_MODEL != 0 {
        r.read_byte()
    } else {
        ent.base_modelindex
    };
    // C: when the model changes to NULL, force a relink ("hack to make null
    // model players work"). modelindex 0 is the empty/world model here.
    if ent.modelindex != prev_modelindex && ent.modelindex == 0 {
        forcelink = true;
    }

    ent.frame = if bits & U_FRAME != 0 {
        r.read_byte()
    } else {
        ent.base_frame
    };

    if bits & U_COLORMAP != 0 {
        let _ = r.read_byte(); // colormap — consumed, not rendered here
    }

    if bits & U_SKIN != 0 {
        let _ = r.read_byte(); // skin — consumed
    }

    // CL_ParseUpdate stores effects (or restores the baseline value); we keep
    // it so a front-end can drive dynamic lights / brightfield particles.
    // CL_ParseBaseline never reads an effects byte, so baseline.effects is
    // always 0 (the zeroed entity_state_t), hence the fallback is 0.
    ent.effects = if bits & U_EFFECTS != 0 {
        r.read_byte()
    } else {
        0
    };

    // "shift the known values for interpolation": the previous most-recent
    // snapshot ([0]) becomes the older one ([1]) before we read the new [0].
    ent.msg_origins[1] = ent.msg_origins[0];
    ent.msg_angles[1] = ent.msg_angles[0];

    ent.msg_origins[0][0] = if bits & U_ORIGIN1 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[0]
    };
    ent.msg_angles[0][0] = if bits & U_ANGLE1 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[0]
    };

    ent.msg_origins[0][1] = if bits & U_ORIGIN2 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[1]
    };
    ent.msg_angles[0][1] = if bits & U_ANGLE2 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[1]
    };

    ent.msg_origins[0][2] = if bits & U_ORIGIN3 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[2]
    };
    ent.msg_angles[0][2] = if bits & U_ANGLE3 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[2]
    };

    // U_NOLERP forces the relink (no bytes consumed): the entity teleported and
    // must not be lerped from its previous position.
    if bits & U_NOLERP != 0 {
        forcelink = true;
    }

    if forcelink {
        // No update last message: copy the new snapshot into BOTH history
        // slots so the lerp is a no-op (the entity simply appears at [0]).
        ent.msg_origins[1] = ent.msg_origins[0];
        ent.msg_angles[1] = ent.msg_angles[0];
    }
    ent.forcelink = forcelink;

    cl.entities[idx] = ent;
    Ok(())
}

// ---------------------------------------------------------------------------
// CL_ParseBaseline
// ---------------------------------------------------------------------------

/// `CL_ParseBaseline` — fill ONLY the entity's `baseline` (default state).
///
/// In the C engine `CL_ParseBaseline` does not touch the current render state:
/// a `svc_spawnbaseline` entity stays invisible (`ent->model == NULL`) until
/// its first `CL_ParseUpdate`. The current state is therefore left untouched
/// here; statics get it copied by [`spawn_static`].
fn parse_baseline(r: &mut NetReader, ent: &mut Entity) {
    ent.base_modelindex = r.read_byte();
    ent.base_frame = r.read_byte();
    let _colormap = r.read_byte();
    let _skin = r.read_byte();
    for i in 0..3 {
        ent.base_origin[i] = r.read_coord();
        ent.base_angles[i] = r.read_angle();
    }
}

/// `CL_ParseStatic` — parse a baseline into a brand-new static and copy it to
/// the current state. Statics never move and are never culled, so both
/// interpolation history slots are seeded from the baseline (the lerp is a
/// no-op) and the static renders immediately.
fn spawn_static(r: &mut NetReader) -> Entity {
    let mut ent = Entity::default();
    parse_baseline(r, &mut ent);
    ent.modelindex = ent.base_modelindex;
    ent.frame = ent.base_frame;
    ent.effects = 0; // baseline.effects is always 0 (never read from stream)
    ent.msg_origins = [ent.base_origin, ent.base_origin];
    ent.msg_angles = [ent.base_angles, ent.base_angles];
    ent.active = true;
    ent.forcelink = true;
    ent
}

// ---------------------------------------------------------------------------
// CL_ParseClientdata
// ---------------------------------------------------------------------------

/// Parse the per-client data block, consuming every field in `CL_ParseClientdata`
/// order so the stream stays aligned.
///
/// Returns the parsed view height (`SU_VIEWHEIGHT` value or `DEFAULT_VIEWHEIGHT`),
/// which the caller stores in client state for the camera snapshot. All other
/// fields are consumed for alignment but otherwise ignored.
fn parse_clientdata(r: &mut NetReader, bits: i32) -> Result<f32> {
    let viewheight = if bits & SU_VIEWHEIGHT != 0 {
        r.read_char() as f32
    } else {
        DEFAULT_VIEWHEIGHT
    };

    if bits & SU_IDEALPITCH != 0 {
        let _ = r.read_char();
    }

    // punchangle[i] / velocity[i], interleaved, for i in 0..3.
    for i in 0..3 {
        if bits & (SU_PUNCH1 << i) != 0 {
            let _ = r.read_char();
        }
        if bits & (SU_VELOCITY1 << i) != 0 {
            let _ = r.read_char();
        }
    }

    // items — always a long.
    let _ = r.read_long();

    if bits & SU_WEAPONFRAME != 0 {
        let _ = r.read_byte();
    }
    if bits & SU_ARMOR != 0 {
        let _ = r.read_byte();
    }
    if bits & SU_WEAPON != 0 {
        let _ = r.read_byte();
    }

    let _health = r.read_short(); // always
    let _ammo = r.read_byte(); // always
    // shells / nails / rockets / cells — always 4 bytes.
    for _ in 0..4 {
        let _ = r.read_byte();
    }
    // active weapon — always 1 byte.
    let _ = r.read_byte();

    Ok(viewheight)
}

// ---------------------------------------------------------------------------
// CL_ParseStartSoundPacket
// ---------------------------------------------------------------------------

fn parse_start_sound(r: &mut NetReader) -> Result<()> {
    let field_mask = r.read_byte();

    if field_mask & SND_VOLUME != 0 {
        let _ = r.read_byte();
    }
    if field_mask & SND_ATTENUATION != 0 {
        let _ = r.read_byte();
    }

    let _channel = r.read_short();
    let _sound_num = r.read_byte();

    // 3 coords for position.
    let _ = r.read_coord();
    let _ = r.read_coord();
    let _ = r.read_coord();
    Ok(())
}

// ---------------------------------------------------------------------------
// CL_ParseTEnt — per-type byte sizes, ported from cl_tent.c EXACTLY.
// ---------------------------------------------------------------------------

/// `R_ParseParticleEffect` (`cl_parse.c`): the `svc_particle` payload — 3
/// coords (origin), 3 chars (the direction, each `char / 16.0` per axis), one
/// byte count and one byte colour. Records a [`ParticleBurst`] onto the client's
/// pending list for the current block's [`DemoFrame`].
///
/// The C `R_ParseParticleEffect` maps the net `count == 255` sentinel to `1024`
/// and then calls `R_RunParticleEffect(org, dir, color, 1024)`. Every count
/// (including 1024) flows through `R_RunParticleEffect`; its internal
/// `count == 1024` branch happens to spawn the same particles as
/// `R_ParticleExplosion` (which the `spawn_burst` 1024 fast-path delegates to). We
/// record the mapped count so the front-end routes the burst through `spawn_burst`.
///
/// Reads the SAME 8 fields the original discarded so the byte stream stays in
/// sync; never panics (a short read flags `r.bad`, which the demux's top-of-loop
/// `bad` check turns into a clean desync `Err` on the next command).
fn parse_particle(cl: &mut ClientState, r: &mut NetReader) {
    let org = [r.read_coord(), r.read_coord(), r.read_coord()];
    // Net dir is a signed byte per axis, scaled by 1/16 (the C `dir[i] =
    // MSG_ReadChar()*(1.0/16)`).
    let dir = [
        r.read_char() as f32 * (1.0 / 16.0),
        r.read_char() as f32 * (1.0 / 16.0),
        r.read_char() as f32 * (1.0 / 16.0),
    ];
    let msg_count = r.read_byte();
    let color = r.read_byte();

    // 255 is the sentinel -> 1024 particles (R_RunParticleEffect's count==1024
    // branch, which matches R_ParticleExplosion).
    let count = if msg_count == 255 { 1024 } else { msg_count };
    // A short read leaves `r.bad` set; the demux refuses the next command, so
    // we still record (with sentinel values) without desyncing silently.
    cl.pending_particles.push(ParticleBurst {
        org,
        dir,
        // color is a byte 0..=255 here; a bad read returns -1, clamp to a u8.
        color: color.clamp(0, 255) as u8,
        count,
    });
}

fn parse_temp_entity(cl: &mut ClientState, r: &mut NetReader) -> Result<()> {
    let te = r.read_byte();
    match te {
        // Beams: short entity + start coord3 + end coord3 (CL_ParseBeam). The
        // entity is the beam slot-reuse key, pos/end the segment endpoints —
        // the front-end feeds all three to `crate::tent::Beams::parse_beam`.
        TE_LIGHTNING1 | TE_LIGHTNING2 | TE_LIGHTNING3 | TE_BEAM => {
            let entity = r.read_short();
            let pos = [r.read_coord(), r.read_coord(), r.read_coord()];
            let end = [r.read_coord(), r.read_coord(), r.read_coord()];
            cl.pending_tents.push(TempEntityEvent {
                te_type: te as u8,
                pos,
                end,
                entity,
                color_start: 0,
                color_length: 0,
            });
        }

        // Color-mapped explosion: coord3 + 2 bytes (colorStart, colorLength).
        TE_EXPLOSION2 => {
            let pos = [r.read_coord(), r.read_coord(), r.read_coord()];
            let color_start = r.read_byte();
            let color_length = r.read_byte();
            cl.pending_tents.push(TempEntityEvent {
                te_type: te as u8,
                pos,
                end: pos,
                entity: 0,
                color_start: color_start.clamp(0, 255) as u8,
                color_length: color_length.clamp(0, 255) as u8,
            });
        }

        // Everything else: a single coord3 position.
        TE_SPIKE | TE_SUPERSPIKE | TE_GUNSHOT | TE_EXPLOSION | TE_TAREXPLOSION
        | TE_WIZSPIKE | TE_KNIGHTSPIKE | TE_LAVASPLASH | TE_TELEPORT => {
            let pos = [r.read_coord(), r.read_coord(), r.read_coord()];
            cl.pending_tents.push(TempEntityEvent {
                te_type: te as u8,
                pos,
                end: pos,
                entity: 0,
                color_start: 0,
                color_length: 0,
            });
        }

        // Sys_Error ("CL_ParseTEnt: bad type") — illegible, desyncs the stream.
        _ => {
            return Err(QError::invalid(format!(
                "svc_temp_entity: bad TE type {te}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CL_ParseServerInfo
// ---------------------------------------------------------------------------

fn parse_serverinfo(cl: &mut ClientState, r: &mut NetReader) -> Result<()> {
    // CL_ClearState — reset entities/statics/precaches on serverinfo.
    cl.clear();

    let proto = r.read_long();
    if proto != PROTOCOL_VERSION {
        return Err(QError::invalid(format!(
            "svc_serverinfo: protocol {proto}, expected {PROTOCOL_VERSION}"
        )));
    }

    let _maxclients = r.read_byte();
    let _gametype = r.read_byte();

    cl.level_name = r.read_string();

    // Model precache: index 0 is "" so model_precache[1] is the first real
    // model (the world map). Loop ReadString until "".
    cl.model_precache.push(String::new());
    loop {
        // Guard against a runaway / malformed stream allocating forever.
        if cl.model_precache.len() >= MAX_PRECACHE {
            return Err(QError::invalid("svc_serverinfo: too many model precaches"));
        }
        let s = r.read_string();
        if s.is_empty() {
            break;
        }
        // A bad read in the middle of the precache list means truncation.
        if r.bad {
            return Err(QError::invalid(
                "svc_serverinfo: truncated model precache list",
            ));
        }
        cl.model_precache.push(s);
    }

    // Sound precache: same structure.
    cl.sound_precache.push(String::new());
    loop {
        if cl.sound_precache.len() >= MAX_PRECACHE {
            return Err(QError::invalid("svc_serverinfo: too many sound precaches"));
        }
        let s = r.read_string();
        if s.is_empty() {
            break;
        }
        if r.bad {
            return Err(QError::invalid(
                "svc_serverinfo: truncated sound precache list",
            ));
        }
        cl.sound_precache.push(s);
    }

    // We have a world now; subsequent frames may be snapshotted.
    cl.have_serverinfo = true;
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Little-endian byte writers mirroring MSG_Write* for building demos.

    fn w_char(buf: &mut Vec<u8>, c: i32) {
        buf.push(c as i8 as u8);
    }
    fn w_byte(buf: &mut Vec<u8>, c: i32) {
        buf.push(c as u8);
    }
    fn w_short(buf: &mut Vec<u8>, c: i32) {
        let v = c as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    fn w_long(buf: &mut Vec<u8>, c: i32) {
        buf.extend_from_slice(&c.to_le_bytes());
    }
    fn w_float(buf: &mut Vec<u8>, f: f32) {
        buf.extend_from_slice(&f.to_le_bytes());
    }
    fn w_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }
    /// MSG_WriteCoord: round(f*8) as short.
    fn w_coord(buf: &mut Vec<u8>, f: f32) {
        w_short(buf, (f * 8.0).round() as i32);
    }
    /// MSG_WriteAngle: (f*256/360) & 255 as a byte.
    fn w_angle(buf: &mut Vec<u8>, f: f32) {
        w_byte(buf, ((f * 256.0 / 360.0) as i32) & 255);
    }

    // -----------------------------------------------------------------------
    // (1) NetReader round-trip.
    // -----------------------------------------------------------------------
    #[test]
    fn netreader_roundtrip() {
        let mut buf = Vec::new();
        w_char(&mut buf, -5);
        w_byte(&mut buf, 200);
        w_short(&mut buf, -1234);
        w_long(&mut buf, 0x12345678);
        w_float(&mut buf, 3.5);
        w_string(&mut buf, "hello");
        w_coord(&mut buf, 8.0); // -> short 64
        w_angle(&mut buf, 90.0); // -> char 64 -> 90.0

        // Sanity: the encodings the spec calls out explicitly.
        assert_eq!((8.0f32 * 8.0).round() as i32, 64);
        assert_eq!(((90.0f32 * 256.0 / 360.0) as i32) & 255, 64);

        let mut r = NetReader::new(&buf);
        assert_eq!(r.read_char(), -5);
        assert_eq!(r.read_byte(), 200);
        assert_eq!(r.read_short(), -1234);
        assert_eq!(r.read_long(), 0x12345678);
        assert_eq!(r.read_float(), 3.5);
        assert_eq!(r.read_string(), "hello");
        assert_eq!(r.read_coord(), 8.0);
        assert_eq!(r.read_angle(), 90.0);
        assert!(!r.bad, "reads within bounds must not set the bad flag");

        // Reading past the end yields the sentinels and sets bad.
        assert_eq!(r.read_byte(), -1);
        assert!(r.bad);
    }

    /// The coord/angle quantization is lossy but the spec values must be exact.
    #[test]
    fn coord_angle_exact_values() {
        let mut buf = Vec::new();
        w_short(&mut buf, 64);
        let mut r = NetReader::new(&buf);
        assert_eq!(r.read_coord(), 8.0);

        let mut buf2 = Vec::new();
        w_char(&mut buf2, 64);
        let mut r2 = NetReader::new(&buf2);
        assert_eq!(r2.read_angle(), 64.0 * (360.0 / 256.0));
    }

    // -----------------------------------------------------------------------
    // Helper: wrap a message body into a single demo block.
    // -----------------------------------------------------------------------
    fn demo_with_message(msg: &[u8]) -> Vec<u8> {
        let mut file = Vec::new();
        // CD-track header line.
        file.extend_from_slice(b"-1\n");
        // Block: length, 3 view-angle floats, body.
        file.extend_from_slice(&(msg.len() as i32).to_le_bytes());
        w_float(&mut file, 10.0);
        w_float(&mut file, 20.0);
        w_float(&mut file, 30.0);
        file.extend_from_slice(msg);
        file
    }

    /// Append a single demo block (length + 3 view-angle floats + body) to a
    /// file buffer, with explicit per-block camera angles.
    fn push_block(file: &mut Vec<u8>, angles: [f32; 3], msg: &[u8]) {
        file.extend_from_slice(&(msg.len() as i32).to_le_bytes());
        for a in angles {
            w_float(file, a);
        }
        file.extend_from_slice(msg);
    }

    // -----------------------------------------------------------------------
    // (2) Tiny synthetic demo.
    //
    // Two blocks, mirroring how real demos separate the signon (serverinfo)
    // block from gameplay: serverinfo calls CL_ClearState, which memsets the
    // whole `cl` struct — including the recorded camera angles (mviewangles).
    // So the *gameplay* block's angles, not the serverinfo block's, drive the
    // rendered camera. The view entity's updated origin reaches frac == 1 in
    // the keyframe stream, confirming the fast-update applied.
    // -----------------------------------------------------------------------
    #[test]
    fn parse_tiny_demo() {
        // --- Block 1 (signon): svc_time + serverinfo + setview + spawnbaseline.
        let mut b1 = Vec::new();
        w_byte(&mut b1, SVC_TIME);
        w_float(&mut b1, 1.4);

        // svc_serverinfo: protocol 15, maxclients 1, gametype 0,
        // level "test", models ["maps/x.bsp"], no sounds. NOTE: serverinfo
        // resets client state (CL_ClearState), so svc_setview must come AFTER it.
        w_byte(&mut b1, SVC_SERVERINFO);
        w_long(&mut b1, PROTOCOL_VERSION);
        w_byte(&mut b1, 1); // maxclients
        w_byte(&mut b1, 0); // gametype
        w_string(&mut b1, "test"); // level name
        w_string(&mut b1, "maps/x.bsp"); // model_precache[1]
        w_string(&mut b1, ""); // end of models
        w_string(&mut b1, ""); // end of sounds (empty list)

        // svc_setview(short) -> entity 1 is the camera (after the serverinfo clear).
        w_byte(&mut b1, SVC_SETVIEW);
        w_short(&mut b1, 1);

        // svc_spawnbaseline for entity 1: model 1, frame 0, colormap 0, skin 0,
        // origin (0,0,0), angles (0,0,0).
        w_byte(&mut b1, SVC_SPAWNBASELINE);
        w_short(&mut b1, 1); // entity number
        w_byte(&mut b1, 1); // modelindex
        w_byte(&mut b1, 0); // frame
        w_byte(&mut b1, 0); // colormap
        w_byte(&mut b1, 0); // skin
        for _ in 0..3 {
            w_coord(&mut b1, 0.0);
            w_angle(&mut b1, 0.0);
        }

        // --- Block 2 (gameplay): svc_time + fast update of entity 1.
        let mut b2 = Vec::new();
        w_byte(&mut b2, SVC_TIME);
        w_float(&mut b2, 1.5);
        // Fast update for entity 1 changing origin to (16,32,64).
        // bits low byte: U_ORIGIN1|U_ORIGIN2|U_ORIGIN3, plus high bit (0x80).
        let bits = U_ORIGIN1 | U_ORIGIN2 | U_ORIGIN3;
        w_byte(&mut b2, 0x80 | bits); // command byte: fast update
        w_byte(&mut b2, 1); // entity number (short-entity not set)
        w_coord(&mut b2, 16.0);
        w_coord(&mut b2, 32.0);
        w_coord(&mut b2, 64.0);

        let mut file = Vec::new();
        file.extend_from_slice(b"-1\n");
        push_block(&mut file, [1.0, 2.0, 3.0], &b1); // signon angles (wiped)
        push_block(&mut file, [10.0, 20.0, 30.0], &b2); // gameplay angles

        let demo = parse_demo(&file).expect("tiny demo must parse");

        assert_eq!(demo.level_name, "test");
        assert_eq!(demo.map_name(), Some("maps/x.bsp"));
        assert_eq!(demo.model_precache.get(1).map(|s| s.as_str()), Some("maps/x.bsp"));
        assert_eq!(demo.frames.len(), 2, "one keyframe per block");

        let frame = demo.frames.last().expect("a frame");

        // Entity 1 is the view entity, so it is deliberately excluded from the
        // rendered list (Quake hides the local player's own model). Its updated
        // origin is instead verified through `view_origin` below: the camera sits
        // at the (updated) entity origin + the default view height (22) on Z, so
        // [16,32,64] + [0,0,22] == [16,32,86] confirms the fast-update applied.
        assert!(
            !frame.entities.iter().any(|e| e.origin == frame.view_origin),
            "the view entity's own model must not be in the render list"
        );
        assert_eq!(frame.view_origin, [16.0, 32.0, 64.0 + DEFAULT_VIEWHEIGHT]);
        // The gameplay block's recorded view angles drive the camera at frac==1.
        assert_eq!(frame.view_angles, [10.0, 20.0, 30.0]);
        // svc_time set the frame time.
        assert_eq!(frame.time, 1.5);
    }

    // -----------------------------------------------------------------------
    // (3) Truncated demo: Err or short frame list, never a panic.
    // -----------------------------------------------------------------------
    #[test]
    fn truncated_demo_no_panic() {
        // A header that claims a 1000-byte block but provides only a few bytes.
        let mut file = Vec::new();
        file.extend_from_slice(b"0\n");
        file.extend_from_slice(&1000i32.to_le_bytes()); // length
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        file.extend_from_slice(&[1, 2, 3]); // far fewer than 1000 bytes

        let demo = parse_demo(&file).expect("truncated framing stops cleanly");
        assert!(demo.frames.is_empty(), "no complete frame should be produced");

        // A file with no header newline at all is an error, not a panic.
        let no_header = b"not a demo file with no newline";
        assert!(parse_demo(no_header).is_err());

        // Empty input: no newline -> Err.
        assert!(parse_demo(&[]).is_err());

        // A header followed by a truncated mid-message (bad read inside the
        // message body) should Err rather than panic. svc_time promises a
        // float but only provides 1 byte: the float read sets the bad flag,
        // and the next loop iteration detects it (mirroring the C engine's
        // `if (msg_badread) Host_Error(...)`).
        let mut msg = Vec::new();
        w_byte(&mut msg, SVC_TIME); // promises a float...
        msg.push(0x01); // ...but only 1 of 4 bytes follow
        let file2 = demo_with_message(&msg);
        assert!(parse_demo(&file2).is_err(), "truncated message must Err, not panic");

        // A message with an unknown command byte is illegible -> Err.
        let bad_cmd = vec![99u8]; // 99 is not a valid svc_ command
        let file3 = demo_with_message(&bad_cmd);
        assert!(parse_demo(&file3).is_err());
    }

    /// Static entities are emitted into every frame's snapshot.
    #[test]
    fn spawnstatic_is_rendered() {
        let mut msg = Vec::new();
        // Minimal serverinfo so we have a world.
        w_byte(&mut msg, SVC_SERVERINFO);
        w_long(&mut msg, PROTOCOL_VERSION);
        w_byte(&mut msg, 1);
        w_byte(&mut msg, 0);
        w_string(&mut msg, "lvl");
        w_string(&mut msg, "maps/y.bsp");
        w_string(&mut msg, "");
        w_string(&mut msg, "");

        // svc_spawnstatic: baseline with model 3 at (1,2,3).
        w_byte(&mut msg, SVC_SPAWNSTATIC);
        w_byte(&mut msg, 3); // modelindex
        w_byte(&mut msg, 0); // frame
        w_byte(&mut msg, 0); // colormap
        w_byte(&mut msg, 0); // skin
        w_coord(&mut msg, 1.0);
        w_angle(&mut msg, 0.0);
        w_coord(&mut msg, 2.0);
        w_angle(&mut msg, 0.0);
        w_coord(&mut msg, 3.0);
        w_angle(&mut msg, 0.0);

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("frame");
        assert!(frame
            .entities
            .iter()
            .any(|e| e.modelindex == 3 && e.origin == [1.0, 2.0, 3.0]));
    }

    /// `svc_spawnstaticsound` registrations are collected into
    /// `Demo::static_sounds` with the sample name resolved from the sound
    /// precache and volume/attenuation decoded from their wire bytes
    /// (CL_ParseStaticSound -> S_StaticSound).
    #[test]
    fn spawnstaticsound_is_collected() {
        let mut msg = Vec::new();
        // Serverinfo with one precached sound (slot 1; slot 0 is "").
        w_byte(&mut msg, SVC_SERVERINFO);
        w_long(&mut msg, PROTOCOL_VERSION);
        w_byte(&mut msg, 1);
        w_byte(&mut msg, 0);
        w_string(&mut msg, "lvl");
        w_string(&mut msg, "maps/y.bsp");
        w_string(&mut msg, ""); // end of models
        w_string(&mut msg, "ambience/fire1.wav");
        w_string(&mut msg, ""); // end of sounds

        // svc_spawnstaticsound: 3 coords, sound_num, vol byte, atten byte —
        // the exact PF_ambientsound signon payload (vol*255, atten*64).
        w_byte(&mut msg, SVC_SPAWNSTATICSOUND);
        w_coord(&mut msg, 100.0);
        w_coord(&mut msg, -50.0);
        w_coord(&mut msg, 24.0);
        w_byte(&mut msg, 1); // sound_precache[1]
        w_byte(&mut msg, 127); // trunc(0.5 * 255)
        w_byte(&mut msg, 192); // 3 (ATTN_STATIC) * 64

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("parse");
        assert_eq!(demo.static_sounds.len(), 1);
        let s = &demo.static_sounds[0];
        assert_eq!(s.origin, [100.0, -50.0, 24.0]);
        assert_eq!(s.sample, "ambience/fire1.wav");
        assert_eq!(s.sound_index, 1);
        assert_eq!(s.volume, 127.0 / 255.0, "vol byte / 255");
        assert_eq!(s.attenuation, 3.0, "atten byte / 64 (ATTN_STATIC)");
    }

    // -----------------------------------------------------------------------
    // (4) svc_particle + svc_temp_entity are surfaced onto the frame, decoded.
    // -----------------------------------------------------------------------

    /// Append a minimal serverinfo so a world exists and frames are snapshotted.
    fn write_serverinfo(msg: &mut Vec<u8>) {
        w_byte(msg, SVC_SERVERINFO);
        w_long(msg, PROTOCOL_VERSION);
        w_byte(msg, 1); // maxclients
        w_byte(msg, 0); // gametype
        w_string(msg, "lvl"); // level name
        w_string(msg, "maps/z.bsp"); // model_precache[1]
        w_string(msg, ""); // end of models
        w_string(msg, ""); // end of sounds
    }

    #[test]
    fn particle_and_temp_entity_are_surfaced_on_the_frame() {
        let mut msg = Vec::new();
        write_serverinfo(&mut msg);

        // svc_particle: org (4, -8, 16), dir bytes (16, -32, 0) -> /16 ==
        // (1, -2, 0), count 12, color 73.
        w_byte(&mut msg, SVC_PARTICLE);
        w_coord(&mut msg, 4.0);
        w_coord(&mut msg, -8.0);
        w_coord(&mut msg, 16.0);
        w_char(&mut msg, 16); // dir.x raw -> 1.0
        w_char(&mut msg, -32); // dir.y raw -> -2.0
        w_char(&mut msg, 0); // dir.z raw -> 0.0
        w_byte(&mut msg, 12); // count
        w_byte(&mut msg, 73); // color

        // svc_temp_entity: TE_EXPLOSION at (32, 64, -16).
        w_byte(&mut msg, SVC_TEMP_ENTITY);
        w_byte(&mut msg, TE_EXPLOSION);
        w_coord(&mut msg, 32.0);
        w_coord(&mut msg, 64.0);
        w_coord(&mut msg, -16.0);

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("a frame");

        assert_eq!(frame.particles.len(), 1, "one particle burst recorded");
        let p = &frame.particles[0];
        assert_eq!(p.org, [4.0, -8.0, 16.0]);
        assert_eq!(p.dir, [1.0, -2.0, 0.0], "net dir is char/16 per axis");
        assert_eq!(p.count, 12);
        assert_eq!(p.color, 73);

        assert_eq!(frame.temp_entities.len(), 1, "one temp entity recorded");
        let te = &frame.temp_entities[0];
        assert_eq!(te.te_type, TE_EXPLOSION as u8);
        assert_eq!(te.pos, [32.0, 64.0, -16.0]);
    }

    #[test]
    fn particle_count_255_is_the_explosion_sentinel() {
        // R_ParseParticleEffect: count == 255 is R_ParticleExplosion (1024).
        let mut msg = Vec::new();
        write_serverinfo(&mut msg);
        w_byte(&mut msg, SVC_PARTICLE);
        w_coord(&mut msg, 0.0);
        w_coord(&mut msg, 0.0);
        w_coord(&mut msg, 0.0);
        w_char(&mut msg, 0);
        w_char(&mut msg, 0);
        w_char(&mut msg, 0);
        w_byte(&mut msg, 255); // sentinel
        w_byte(&mut msg, 0);

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("a frame");
        assert_eq!(frame.particles.len(), 1);
        assert_eq!(
            frame.particles[0].count, 1024,
            "count 255 maps to the 1024-particle explosion"
        );
    }

    #[test]
    fn explosion2_records_its_two_colour_bytes() {
        // TE_EXPLOSION2 carries coord3 + colorStart + colorLength.
        let mut msg = Vec::new();
        write_serverinfo(&mut msg);
        w_byte(&mut msg, SVC_TEMP_ENTITY);
        w_byte(&mut msg, TE_EXPLOSION2);
        w_coord(&mut msg, 1.0);
        w_coord(&mut msg, 2.0);
        w_coord(&mut msg, 3.0);
        w_byte(&mut msg, 100); // colorStart
        w_byte(&mut msg, 8); // colorLength

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("parse");
        let te = &demo.frames.last().expect("a frame").temp_entities[0];
        assert_eq!(te.te_type, TE_EXPLOSION2 as u8);
        assert_eq!(te.pos, [1.0, 2.0, 3.0]);
        assert_eq!(te.color_start, 100);
        assert_eq!(te.color_length, 8);
    }

    #[test]
    fn effect_lists_are_cleared_between_frames() {
        // Block 1 carries an svc_particle; block 2 (after serverinfo) is empty.
        // The frame for block 1 has the burst; the frame for block 2 is empty —
        // proving snapshot drained-and-cleared the pending lists.
        let mut file = Vec::new();
        file.extend_from_slice(b"-1\n");

        // Block 1: serverinfo + one particle.
        let mut b1 = Vec::new();
        write_serverinfo(&mut b1);
        w_byte(&mut b1, SVC_PARTICLE);
        w_coord(&mut b1, 1.0);
        w_coord(&mut b1, 1.0);
        w_coord(&mut b1, 1.0);
        w_char(&mut b1, 0);
        w_char(&mut b1, 0);
        w_char(&mut b1, 0);
        w_byte(&mut b1, 5);
        w_byte(&mut b1, 0);
        file.extend_from_slice(&(b1.len() as i32).to_le_bytes());
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        file.extend_from_slice(&b1);

        // Block 2: a single svc_nop (no effects) -> an empty effect frame.
        let mut b2 = Vec::new();
        w_byte(&mut b2, SVC_NOP);
        file.extend_from_slice(&(b2.len() as i32).to_le_bytes());
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        w_float(&mut file, 0.0);
        file.extend_from_slice(&b2);

        let demo = parse_demo(&file).expect("parse");
        assert_eq!(demo.frames.len(), 2, "one frame per block");
        assert_eq!(demo.frames[0].particles.len(), 1, "block 1 frame has the burst");
        assert!(
            demo.frames[1].particles.is_empty() && demo.frames[1].temp_entities.is_empty(),
            "block 2 frame starts empty — pending lists were cleared after block 1"
        );
    }

    #[test]
    fn malformed_effect_errs_without_panic() {
        // A truncated svc_particle (origin coords promised, bytes missing) must
        // Err on the desync check, never panic. The reader flags `bad`; the
        // demux's top-of-loop guard turns it into an illegible-message Err.
        let mut msg = Vec::new();
        write_serverinfo(&mut msg);
        w_byte(&mut msg, SVC_PARTICLE);
        msg.push(0x01); // 1 byte where 8 fields (origin..color) were promised
        let file = demo_with_message(&msg);
        assert!(parse_demo(&file).is_err(), "truncated particle errs, not panics");

        // A truncated svc_temp_entity (type byte then nothing) likewise Errs.
        let mut msg2 = Vec::new();
        write_serverinfo(&mut msg2);
        w_byte(&mut msg2, SVC_TEMP_ENTITY);
        w_byte(&mut msg2, TE_GUNSHOT); // promises coord3, none follow
        let file2 = demo_with_message(&msg2);
        assert!(parse_demo(&file2).is_err(), "truncated temp entity errs, not panics");

        // A bad TE type is illegible (Sys_Error in the C) -> Err, no panic.
        let mut msg3 = Vec::new();
        write_serverinfo(&mut msg3);
        w_byte(&mut msg3, SVC_TEMP_ENTITY);
        w_byte(&mut msg3, 99); // not a valid TE_*
        let file3 = demo_with_message(&msg3);
        assert!(parse_demo(&file3).is_err(), "bad TE type errs, not panics");
    }

    // =======================================================================
    // CL_RelinkEntities faithfulness: stale cull, interpolation, setangle,
    // EF_ROTATE. These exercise the pure helpers directly and the end-to-end
    // demo paths.
    // =======================================================================

    // ---- low-level message builders for entity updates / svc_time -----------

    fn w_time(msg: &mut Vec<u8>, t: f32) {
        w_byte(msg, SVC_TIME);
        w_float(msg, t);
    }

    /// Fast-update entity `num` to origin `o` and yaw `yaw`, setting all three
    /// origin axes and angle2 (yaw). `effects` adds the U_EFFECTS byte when > 0.
    ///
    /// `U_EFFECTS` lives in the update's high byte, so when it is requested we
    /// also set `U_MOREBITS` (low byte) and emit the high byte after the
    /// command byte, exactly like MSG_WriteByte does in `SV_WriteEntitiesToClient`.
    fn w_update(msg: &mut Vec<u8>, num: i32, o: [f32; 3], yaw: f32, effects: i32) {
        let mut bits = U_ORIGIN1 | U_ORIGIN2 | U_ORIGIN3 | U_ANGLE2;
        if effects != 0 {
            bits |= U_EFFECTS | U_MOREBITS;
        }
        // The low 7 bits ride the command byte (high bit set). If any high-byte
        // bit is present, U_MOREBITS is set and the high byte follows.
        w_byte(msg, 0x80 | (bits & 0x7f));
        if bits & U_MOREBITS != 0 {
            w_byte(msg, (bits >> 8) & 0xff);
        }
        w_byte(msg, num);
        // CL_ParseUpdate reads effects BEFORE the origin/angle fields.
        if effects != 0 {
            w_byte(msg, effects);
        }
        w_coord(msg, o[0]);
        // angle1 omitted (not in bits); angle2 (yaw) present.
        w_coord(msg, o[1]);
        w_angle(msg, yaw);
        w_coord(msg, o[2]);
    }

    /// A baseline for entity `num`: model `model`, at origin (0,0,0).
    fn w_baseline(msg: &mut Vec<u8>, num: i32, model: i32) {
        w_byte(msg, SVC_SPAWNBASELINE);
        w_short(msg, num);
        w_byte(msg, model);
        w_byte(msg, 0); // frame
        w_byte(msg, 0); // colormap
        w_byte(msg, 0); // skin
        for _ in 0..3 {
            w_coord(msg, 0.0);
            w_angle(msg, 0.0);
        }
    }

    // ----------------------- pure helper tests -------------------------------

    #[test]
    fn lerp_point_matches_cl_lerppoint() {
        // Zero interval -> snap to mtime[0] (frac 1).
        assert_eq!(lerp_point([2.0, 2.0], 2.0), 1.0);

        // Normal 0.1s interval, cl.time exactly at the midpoint -> 0.5.
        let mt = [1.6, 1.5];
        assert!((lerp_point(mt, 1.55) - 0.5).abs() < 1e-6);
        // At the start of the interval -> 0; at the end -> 1.
        assert_eq!(lerp_point(mt, 1.5), 0.0);
        assert_eq!(lerp_point(mt, 1.6), 1.0);
        // Clamped below 0 and above 1.
        assert_eq!(lerp_point(mt, 1.4), 0.0);
        assert_eq!(lerp_point(mt, 1.7), 1.0);

        // A gap > 0.1s is treated as a 0.1s interval ending at mtime[0]
        // (dropped packet / start of demo): mtime[1] is effectively
        // mtime[0]-0.1, so cl.time at mtime[0]-0.05 is the midpoint.
        let big = [5.0, 4.0]; // 1.0s gap
        assert!((lerp_point(big, 4.95) - 0.5).abs() < 1e-4);
        assert_eq!(lerp_point(big, 4.90), 0.0); // clamps at the 0.1s window start
    }

    #[test]
    fn lerp_origin_interpolates_and_snaps_on_teleport() {
        // Small motion: linear interpolation by frac.
        let mid = lerp_origin([0.0, 0.0, 0.0], [10.0, 20.0, 40.0], 0.5);
        assert_eq!(mid, [5.0, 10.0, 20.0]);

        // A jump > 100 units on ANY axis is assumed a teleport: f forced to 1,
        // so the result snaps to the destination regardless of frac.
        let tele = lerp_origin([0.0, 0.0, 0.0], [0.0, 0.0, 200.0], 0.25);
        assert_eq!(tele, [0.0, 0.0, 200.0], "teleport snaps to destination");
        // Exactly 100 is NOT a teleport (C uses strict > 100).
        let edge = lerp_origin([0.0, 0.0, 0.0], [100.0, 0.0, 0.0], 0.5);
        assert_eq!(edge, [50.0, 0.0, 0.0]);
    }

    #[test]
    fn lerp_angles_takes_the_short_way_around() {
        // 350 -> 10 should go the short way (+20 through 360), not -340.
        let a = lerp_angles([350.0, 0.0, 0.0], [10.0, 0.0, 0.0], 0.5);
        assert!((a[0] - 360.0).abs() < 1e-4, "midpoint is 360==0, got {}", a[0]);
        // 10 -> 350 is the short way the other direction (-20).
        let b = lerp_angles([10.0, 0.0, 0.0], [350.0, 0.0, 0.0], 0.5);
        assert!((b[0] - 0.0).abs() < 1e-4, "midpoint is 0, got {}", b[0]);
    }

    #[test]
    fn rotate_yaw_is_anglemod_of_100t() {
        for &t in &[0.0f32, 0.37, 1.0, 12.5, 100.0] {
            assert_eq!(rotate_yaw(t), crate::math::anglemod(100.0 * t));
        }
    }

    // --------------------- end-to-end behaviour tests ------------------------

    /// Build a two-block demo: block 1 = signon (svc_time t0 + serverinfo +
    /// baselines via `setup`), block 2 = gameplay (svc_time t1 + `play`), with
    /// per-block camera angles.
    fn two_block_demo(
        t0: f32,
        a0: [f32; 3],
        setup: impl FnOnce(&mut Vec<u8>),
        t1: f32,
        a1: [f32; 3],
        play: impl FnOnce(&mut Vec<u8>),
    ) -> Vec<u8> {
        let mut b1 = Vec::new();
        w_time(&mut b1, t0);
        write_serverinfo(&mut b1);
        setup(&mut b1);

        let mut b2 = Vec::new();
        w_time(&mut b2, t1);
        play(&mut b2);

        let mut file = Vec::new();
        file.extend_from_slice(b"-1\n");
        push_block(&mut file, a0, &b1);
        push_block(&mut file, a1, &b2);
        file
    }

    #[test]
    fn stale_entity_is_culled_when_it_goes_silent() {
        // Two entities baselined in block 1. In block 2 ONLY entity 2 is
        // updated; entity 3 goes silent. CL_RelinkEntities drops entity 3
        // (its msgtime falls behind mtime[0]) so it must NOT render in the
        // block-2 keyframe, even though it lingered in the old code.
        let file = two_block_demo(
            1.4,
            [0.0; 3],
            |b| {
                w_baseline(b, 2, 5); // entity 2, model 5
                w_baseline(b, 3, 6); // entity 3, model 6
            },
            1.5,
            [0.0; 3],
            |b| {
                // Update only entity 2; entity 3 is omitted (silent).
                w_update(b, 2, [50.0, 0.0, 0.0], 0.0, 0);
            },
        );
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("a frame");

        assert!(
            frame.entities.iter().any(|e| e.modelindex == 5),
            "the entity updated this message still renders"
        );
        assert!(
            !frame.entities.iter().any(|e| e.modelindex == 6),
            "the entity that went silent this message must be culled"
        );
    }

    #[test]
    fn interpolation_produces_intermediate_positions() {
        // Entity 2 is at x=0 in block 1 and x=80 in block 2 (an 80-unit move,
        // under the 100-unit teleport threshold, so it lerps). The interpolated
        // stream emits sub-frames between the snapshots; the midpoint frame must
        // show the entity roughly halfway (x ~= 40), proving the lerp ran.
        let file = two_block_demo(
            1.4,
            [0.0; 3],
            |b| {
                // Baseline entity 2, then update it in block 1 so it has a real
                // previous snapshot (forcelink clears, enabling lerp in block 2).
                w_baseline(b, 2, 5);
                w_update(b, 2, [0.0, 0.0, 0.0], 0.0, 0);
            },
            1.5,
            [0.0; 3],
            |b| {
                w_update(b, 2, [80.0, 0.0, 0.0], 0.0, 0);
            },
        );

        // 20 fps over a 0.1s interval -> ~2 sub-frames for block 2.
        let demo = parse_demo_interpolated(&file, 20.0, &[]).expect("parse");
        // Collect the x-position of entity 2 (model 5) across all frames.
        let xs: Vec<f32> = demo
            .frames
            .iter()
            .filter_map(|f| f.entities.iter().find(|e| e.modelindex == 5))
            .map(|e| e.origin[0])
            .collect();
        assert!(!xs.is_empty(), "entity should render across the interpolation");
        // Some frame must land strictly between the two snapshots (0 < x < 80),
        // which only happens if interpolation occurred (a keyframe-only stream
        // would jump 0 -> 80 with nothing in between).
        assert!(
            xs.iter().any(|&x| x > 1.0 && x < 79.0),
            "expected an interpolated mid position, got xs = {xs:?}"
        );
        // And a frame must reach the final snapshot position.
        assert!(
            xs.iter().any(|&x| (x - 80.0).abs() < 1e-3),
            "expected the final snapshot position 80, got xs = {xs:?}"
        );
    }

    #[test]
    fn svc_setangle_does_not_override_recorded_camera() {
        // The gameplay block carries recorded camera angles [10,20,30] AND an
        // svc_setangle to a wildly different value. In demo playback the
        // recorded angles win (CL_RelinkEntities recomputes viewangles from
        // mviewangles every frame), so svc_setangle must be a no-op.
        let file = two_block_demo(
            1.4,
            [0.0; 3],
            |b| {
                w_baseline(b, 1, 1);
                w_byte(b, SVC_SETVIEW);
                w_short(b, 1);
            },
            1.5,
            [10.0, 20.0, 30.0],
            |b| {
                // svc_setangle to (90, 90, 90) — must be ignored for the camera.
                w_byte(b, SVC_SETANGLE);
                w_angle(b, 90.0);
                w_angle(b, 90.0);
                w_angle(b, 90.0);
                // Keep entity 1 alive so it isn't culled (the view entity).
                w_update(b, 1, [0.0, 0.0, 0.0], 0.0, 0);
            },
        );
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("a frame");
        assert_eq!(
            frame.view_angles, [10.0, 20.0, 30.0],
            "recorded camera angles drive the view; svc_setangle is ignored"
        );
    }

    #[test]
    fn ef_rotate_model_spins_during_interpolated_playback() {
        // A rotating pickup (model 7) should have its yaw forced to
        // anglemod(100*time) regardless of the recorded angle, when the caller
        // marks model 7 as EF_ROTATE.
        let file = two_block_demo(
            1.4,
            [0.0; 3],
            |b| {
                w_baseline(b, 2, 7);
                w_update(b, 2, [0.0, 0.0, 0.0], 0.0, 0);
            },
            1.5,
            [0.0; 3],
            |b| {
                // Recorded yaw is 0, but EF_ROTATE must override it with the spin.
                w_update(b, 2, [0.0, 0.0, 0.0], 0.0, 0);
            },
        );

        // With model 7 declared as a rotator, the final block-2 frame is at
        // cl.time == 1.5, so yaw == anglemod(100*1.5) == anglemod(150) == 150.
        let demo = parse_demo_interpolated(&file, 10.0, &[7]).expect("parse");
        let frame = demo.frames.last().expect("a frame");
        let ent = frame
            .entities
            .iter()
            .find(|e| e.modelindex == 7)
            .expect("rotating pickup renders");
        assert_eq!(
            ent.angles[1],
            crate::math::anglemod(100.0 * 1.5),
            "EF_ROTATE forces yaw to anglemod(100*time)"
        );

        // Without the EF_ROTATE declaration the recorded yaw (0) is kept.
        let demo2 = parse_demo_interpolated(&file, 10.0, &[]).expect("parse");
        let frame2 = demo2.frames.last().expect("a frame");
        let ent2 = frame2
            .entities
            .iter()
            .find(|e| e.modelindex == 7)
            .expect("pickup renders");
        assert_eq!(ent2.angles[1], 0.0, "no EF_ROTATE => recorded yaw kept");
    }

    #[test]
    fn effects_byte_is_surfaced_on_the_snapshot() {
        // The U_EFFECTS byte reaches EntSnapshot.effects so a front-end can
        // drive dynamic lights / brightfield particles like the live walk.
        let file = two_block_demo(
            1.4,
            [0.0; 3],
            |b| w_baseline(b, 2, 9),
            1.5,
            [0.0; 3],
            |b| {
                w_update(b, 2, [0.0, 0.0, 0.0], 0.0, EF_ROTATE /* any nonzero */);
            },
        );
        let demo = parse_demo(&file).expect("parse");
        let frame = demo.frames.last().expect("a frame");
        let ent = frame.entities.iter().find(|e| e.modelindex == 9).unwrap();
        assert_eq!(ent.effects, EF_ROTATE, "effects byte preserved on snapshot");
    }
}
