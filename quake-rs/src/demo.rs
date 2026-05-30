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
#[derive(Clone, Copy)]
pub struct EntSnapshot {
    pub modelindex: usize,
    pub frame: i32,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
}

/// One playback frame: the camera plus every visible entity.
pub struct DemoFrame {
    pub time: f32,
    pub view_origin: [f32; 3],
    pub view_angles: [f32; 3],
    pub entities: Vec<EntSnapshot>,
}

/// A fully parsed demo: level metadata, precache tables, and all frames.
pub struct Demo {
    pub level_name: String,
    pub model_precache: Vec<String>,
    pub sound_precache: Vec<String>,
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
    origin: [f32; 3],
    angles: [f32; 3],
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
    view_angles: [f32; 3],
    time: f32,
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
            time: 0.0,
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
        // view_angles and time are not reset by CL_ClearState in a way that
        // matters before serverinfo; leave them.
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
/// Framing (`CL_PlayDemo_f` + `CL_GetMessage`): an ASCII CD-track integer
/// followed by `'\n'`, then a sequence of blocks, each
/// `[i32 length][3 × f32 view angles][length bytes of message]`. Parsing stops
/// at end of file, on `svc_disconnect`, or when a block would overrun the file.
///
/// A malformed or truncated demo produces a short/empty frame list or an
/// `Err`, never a panic.
pub fn parse_demo(bytes: &[u8]) -> Result<Demo> {
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

        // CL_GetMessage records the block's view angles into cl.mviewangles[0],
        // which CL_ParseServerMessage / the view code uses as the camera angle
        // for this frame.
        cl.view_angles = block_angles;

        // Parse the message, updating client state.
        let flow = parse_server_message(&mut cl, msg)?;

        // Snapshot AFTER the block (only once we have a world).
        if cl.have_serverinfo {
            frames.push(snapshot(&cl));
        }

        if let ParseFlow::Stop = flow {
            break;
        }
    }

    Ok(Demo {
        level_name: cl.level_name,
        model_precache: cl.model_precache,
        sound_precache: cl.sound_precache,
        frames,
    })
}

/// Build a [`DemoFrame`] from the current client state.
///
/// `view_origin = entities[viewentity].origin` with `+viewheight` on Z; the
/// entity list is every regular entity with `modelindex > 0` plus every static.
fn snapshot(cl: &ClientState) -> DemoFrame {
    let mut view_origin = [0.0f32; 3];
    if let Some(ve) = cl.entities.get(cl.viewentity) {
        view_origin = ve.origin;
    }
    view_origin[2] += cl.viewheight;

    let mut entities: Vec<EntSnapshot> = Vec::new();
    for (i, e) in cl.entities.iter().enumerate() {
        // Skip the view entity: Quake hides the local player's own model in
        // first person (otherwise it fills the screen at the camera origin).
        if i == cl.viewentity {
            continue;
        }
        if e.modelindex > 0 {
            entities.push(EntSnapshot {
                modelindex: e.modelindex as usize,
                frame: e.frame,
                origin: e.origin,
                angles: e.angles,
            });
        }
    }
    for e in &cl.statics {
        // Statics are always emitted (they were spawned with a model).
        entities.push(EntSnapshot {
            modelindex: e.modelindex.max(0) as usize,
            frame: e.frame,
            origin: e.origin,
            angles: e.angles,
        });
    }

    DemoFrame {
        time: cl.time,
        view_origin,
        view_angles: cl.view_angles,
        entities,
    }
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
                cl.time = r.read_float();
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
                for a in 0..3 {
                    cl.view_angles[a] = r.read_angle();
                }
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
                // R_ParseParticleEffect: 3 coords, 3 chars (dir), byte count,
                // byte color.
                let _ = r.read_coord();
                let _ = r.read_coord();
                let _ = r.read_coord();
                let _ = r.read_char();
                let _ = r.read_char();
                let _ = r.read_char();
                let _ = r.read_byte();
                let _ = r.read_byte();
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
                // CL_ParseStatic: parse a baseline into a brand-new static.
                let mut ent = Entity::default();
                parse_baseline(&mut r, &mut ent);
                cl.statics.push(ent);
            }

            SVC_TEMP_ENTITY => {
                parse_temp_entity(&mut r)?;
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
                // CL_ParseStaticSound: 3 coords, 3 bytes (sample, vol, atten).
                let _ = r.read_coord();
                let _ = r.read_coord();
                let _ = r.read_coord();
                let _ = r.read_byte();
                let _ = r.read_byte();
                let _ = r.read_byte();
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

    // Order matches CL_ParseUpdate EXACTLY: model, frame, colormap, skin,
    // effects, then origin1, angle1, origin2, angle2, origin3, angle3.
    ent.modelindex = if bits & U_MODEL != 0 {
        r.read_byte()
    } else {
        ent.base_modelindex
    };

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

    if bits & U_EFFECTS != 0 {
        let _ = r.read_byte(); // effects — consumed
    }

    ent.origin[0] = if bits & U_ORIGIN1 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[0]
    };
    ent.angles[0] = if bits & U_ANGLE1 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[0]
    };

    ent.origin[1] = if bits & U_ORIGIN2 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[1]
    };
    ent.angles[1] = if bits & U_ANGLE2 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[1]
    };

    ent.origin[2] = if bits & U_ORIGIN3 != 0 {
        r.read_coord()
    } else {
        ent.base_origin[2]
    };
    ent.angles[2] = if bits & U_ANGLE3 != 0 {
        r.read_angle()
    } else {
        ent.base_angles[2]
    };

    // U_NOLERP carries no bytes.
    let _ = U_NOLERP;

    cl.entities[idx] = ent;
    Ok(())
}

// ---------------------------------------------------------------------------
// CL_ParseBaseline
// ---------------------------------------------------------------------------

fn parse_baseline(r: &mut NetReader, ent: &mut Entity) {
    ent.base_modelindex = r.read_byte();
    ent.base_frame = r.read_byte();
    let _colormap = r.read_byte();
    let _skin = r.read_byte();
    for i in 0..3 {
        ent.base_origin[i] = r.read_coord();
        ent.base_angles[i] = r.read_angle();
    }

    // Initialise current state from the baseline (CL_ParseStatic does this
    // explicitly; CL_EntityNum-spawned baselines also become the current
    // state until the first update overrides them).
    ent.modelindex = ent.base_modelindex;
    ent.frame = ent.base_frame;
    ent.origin = ent.base_origin;
    ent.angles = ent.base_angles;
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

fn parse_temp_entity(r: &mut NetReader) -> Result<()> {
    let te = r.read_byte();
    match te {
        // Beams: short entity + start coord3 + end coord3 (CL_ParseBeam).
        TE_LIGHTNING1 | TE_LIGHTNING2 | TE_LIGHTNING3 | TE_BEAM => {
            let _ = r.read_short();
            for _ in 0..6 {
                let _ = r.read_coord();
            }
        }

        // Color-mapped explosion: coord3 + 2 bytes (colorStart, colorLength).
        TE_EXPLOSION2 => {
            let _ = r.read_coord();
            let _ = r.read_coord();
            let _ = r.read_coord();
            let _ = r.read_byte();
            let _ = r.read_byte();
        }

        // Everything else: a single coord3 position.
        TE_SPIKE | TE_SUPERSPIKE | TE_GUNSHOT | TE_EXPLOSION | TE_TAREXPLOSION
        | TE_WIZSPIKE | TE_KNIGHTSPIKE | TE_LAVASPLASH | TE_TELEPORT => {
            let _ = r.read_coord();
            let _ = r.read_coord();
            let _ = r.read_coord();
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

    // -----------------------------------------------------------------------
    // (2) Tiny synthetic demo.
    // -----------------------------------------------------------------------
    #[test]
    fn parse_tiny_demo() {
        let mut msg = Vec::new();

        // svc_time(float)
        w_byte(&mut msg, SVC_TIME);
        w_float(&mut msg, 1.5);

        // svc_serverinfo: protocol 15, maxclients 1, gametype 0,
        // level "test", models ["maps/x.bsp"], no sounds. NOTE: serverinfo
        // resets client state (CL_ClearState), so svc_setview must come AFTER it.
        w_byte(&mut msg, SVC_SERVERINFO);
        w_long(&mut msg, PROTOCOL_VERSION);
        w_byte(&mut msg, 1); // maxclients
        w_byte(&mut msg, 0); // gametype
        w_string(&mut msg, "test"); // level name
        w_string(&mut msg, "maps/x.bsp"); // model_precache[1]
        w_string(&mut msg, ""); // end of models
        w_string(&mut msg, ""); // end of sounds (empty list)

        // svc_setview(short) -> entity 1 is the camera (after the serverinfo clear).
        w_byte(&mut msg, SVC_SETVIEW);
        w_short(&mut msg, 1);

        // svc_spawnbaseline for entity 1: model 1, frame 0, colormap 0, skin 0,
        // origin (0,0,0), angles (0,0,0).
        w_byte(&mut msg, SVC_SPAWNBASELINE);
        w_short(&mut msg, 1); // entity number
        w_byte(&mut msg, 1); // modelindex
        w_byte(&mut msg, 0); // frame
        w_byte(&mut msg, 0); // colormap
        w_byte(&mut msg, 0); // skin
        for _ in 0..3 {
            w_coord(&mut msg, 0.0);
            w_angle(&mut msg, 0.0);
        }

        // Fast update for entity 1 changing origin to (16,32,64).
        // bits low byte: U_ORIGIN1|U_ORIGIN2|U_ORIGIN3, plus high bit (0x80).
        let bits = U_ORIGIN1 | U_ORIGIN2 | U_ORIGIN3;
        w_byte(&mut msg, 0x80 | bits); // command byte: fast update
        w_byte(&mut msg, 1); // entity number (short-entity not set)
        w_coord(&mut msg, 16.0);
        w_coord(&mut msg, 32.0);
        w_coord(&mut msg, 64.0);

        let file = demo_with_message(&msg);
        let demo = parse_demo(&file).expect("tiny demo must parse");

        assert_eq!(demo.level_name, "test");
        assert_eq!(demo.map_name(), Some("maps/x.bsp"));
        assert_eq!(demo.model_precache.get(1).map(|s| s.as_str()), Some("maps/x.bsp"));
        assert!(!demo.frames.is_empty(), "expected at least one frame");

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
        // The block's recorded view angles became the frame's view angles.
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
}
