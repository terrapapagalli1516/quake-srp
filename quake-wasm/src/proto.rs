//! The wire between the program and its host: the page's events come in on
//! stdin, everything the program shows or plays goes out on stdout. Both are
//! streams of small framed records, little-endian throughout.
//! `web/PLATFORM.md` documents the same layout for the JS side; this file is
//! the Rust half, pure encoding and decoding with no I/O policy.
//!
//! **In** (host → program), a 4-byte header `[kind u8][0 u8][len u16]` and
//! `len` payload bytes:
//!
//! | kind | event | payload |
//! |---|---|---|
//! | 1 | `Tick` | `seq u32`, `dt f64` (seconds since the last tick: the page's display refresh) |
//! | 2 | `Key` | `keynum u8`, `down u8`, `0 u16`, `ch u32` (the character the layout typed, 0 none) |
//! | 3 | `Mouse` | `dx f32`, `dy f32` (raw `movementX`/`movementY` counts) |
//! | 4 | `ClearKeys` | — (the window lost the keyboard) |
//! | 5 | `PointerUnlocked` | — |
//! | 6 | `AudioReady` | `ready u8` |
//! | 7 | `Call` | `id u32`, then a UTF-8 line `name arg...` (automation, [`crate::automation`]) |
//! | 8 | `End` | — (the host's "nothing more is queued", the answer to a polling `Sync`) |
//! | 9 | `Window` | `w u32`, `h u32`: the page's box for the picture in device pixels (CSS size x `devicePixelRatio`), which native resolution renders into |
//!
//! **Out** (program → host), an 8-byte header `[kind u8][0 u8 ×3][len u32]`
//! and `len` payload bytes:
//!
//! | kind | message | payload |
//! |---|---|---|
//! | 1 | `Frame` | `w u16`, `h u16`, `format u8` (0 = RGBA8), `0 u8 ×3`, then the pixels |
//! | 2 | `Sync` | `seq u32` (the last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
//! | 3 | `State` | `flags u32` ([`STATE_MENU`] …), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel; 0 in the 4:3 box) |
//! | 4 | `Sample` | `id u32`, then the RIFF/WAV bytes (sent once per distinct sample) |
//! | 5 | `Sound` | `id u32`, `origin f32×3`, `volume f32`, `attenuation f32`, `entity i32`, `channel i32`, `view u32`, `loop_start f32`, `loop_end f32` |
//! | 6 | `StopSound` | `entity i32`, `channel i32` |
//! | 7 | `StaticSound` | `id u32`, `origin f32×3`, `volume f32`, `attenuation f32`, `loop_start f32`, `loop_end f32` |
//! | 8 | `Ambient` | `channel u32`, `id u32`, `loop_start f32`, `loop_end f32` |
//! | 9 | `Listener` | `origin f32×3`, `forward f32×3`, `right f32×3`, `ambient f32×4`, `volume f32` |
//! | 10 | `Generation` | `generation u32` (every looping and playing sound stops: `S_StopAllSounds`) |
//! | 11 | `LocalSound` | `id u32` (`S_LocalSound`: the menu's clicks) |
//! | 12 | `Reply` | `id u32`, `value f64`, then UTF-8 text (the answer to a `Call`) |
//! | 13 | `Bench` | `f64` per value (`--features bench`: the frame's phase times) |
//!
//! A `Sync` ends each turn of the program's loop: everything before it is
//! one turn's output, and the host publishes it then.

use std::io::{self, Read, Write};

/// Input record kinds.
const IN_TICK: u8 = 1;
const IN_KEY: u8 = 2;
const IN_MOUSE: u8 = 3;
const IN_CLEAR_KEYS: u8 = 4;
const IN_POINTER_UNLOCKED: u8 = 5;
const IN_AUDIO_READY: u8 = 6;
const IN_CALL: u8 = 7;
const IN_END: u8 = 8;
const IN_WINDOW: u8 = 9;

/// One event from the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    /// A display refresh: run a host frame `dt` seconds after the last one.
    /// `seq` is echoed in the next [`Msg::Sync`] so the page knows which
    /// refresh the frame answers.
    Tick { seq: u32, dt: f64 },
    /// A key by Quake keynum (keys.c `Key_Event`), with the character it
    /// typed on the player's layout.
    Key { keynum: u8, down: bool, ch: u32 },
    /// Raw mouse motion (in_win.c `IN_MouseMove`).
    Mouse { dx: f32, dy: f32 },
    /// vid_win.c `ClearAllStates`: the window lost the keyboard.
    ClearKeys,
    /// The pointer lock ended (the port's `+mlook` release).
    PointerUnlocked,
    /// The page's audio output is running (or stopped).
    AudioReady(bool),
    /// An automation call, answered by a [`Msg::Reply`] with the same `id`.
    Call { id: u32, line: String },
    /// The host has nothing more queued (the answer to a polling sync).
    End,
    /// The page's box for the picture, in device pixels.
    Window { w: u32, h: u32 },
    /// A record this program does not know (skipped, for forward
    /// compatibility with a newer page).
    Unknown(u8),
}

/// Read one event; `Ok(None)` at a clean end of the stream (the host closed
/// stdin between records: the program quits).
pub(crate) fn read_event(r: &mut impl Read) -> io::Result<Option<Event>> {
    let mut head = [0u8; 4];
    match r.read_exact(&mut head[..1]) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    r.read_exact(&mut head[1..])?;
    let len = u16::from_le_bytes([head[2], head[3]]) as usize;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let mut p = Payload(&body);
    let ev = match head[0] {
        IN_TICK => Event::Tick { seq: p.u32(), dt: p.f64() },
        IN_KEY => {
            let keynum = p.u8();
            let down = p.u8() != 0;
            p.skip(2);
            Event::Key { keynum, down, ch: p.u32() }
        }
        IN_MOUSE => Event::Mouse { dx: p.f32(), dy: p.f32() },
        IN_CLEAR_KEYS => Event::ClearKeys,
        IN_POINTER_UNLOCKED => Event::PointerUnlocked,
        IN_AUDIO_READY => Event::AudioReady(p.u8() != 0),
        IN_CALL => {
            let id = p.u32();
            Event::Call { id, line: String::from_utf8_lossy(p.rest()).into_owned() }
        }
        IN_END => Event::End,
        IN_WINDOW => Event::Window { w: p.u32(), h: p.u32() },
        other => Event::Unknown(other),
    };
    Ok(Some(ev))
}

/// A little-endian reader over one record's payload. A short payload reads
/// as zeros rather than failing: a record is only as long as its sender
/// made it, and the fields a newer sender appended are simply absent here.
struct Payload<'a>(&'a [u8]);

impl Payload<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        let n = N.min(self.0.len());
        out[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        out
    }
    fn skip(&mut self, n: usize) {
        self.0 = &self.0[n.min(self.0.len())..];
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take())
    }
    fn rest(&mut self) -> &[u8] {
        std::mem::take(&mut self.0)
    }
}

/// Output record kinds.
const OUT_FRAME: u8 = 1;
const OUT_SYNC: u8 = 2;
const OUT_STATE: u8 = 3;
const OUT_SAMPLE: u8 = 4;
const OUT_SOUND: u8 = 5;
const OUT_STOP_SOUND: u8 = 6;
const OUT_STATIC_SOUND: u8 = 7;
const OUT_AMBIENT: u8 = 8;
const OUT_LISTENER: u8 = 9;
const OUT_GENERATION: u8 = 10;
const OUT_LOCAL_SOUND: u8 = 11;
const OUT_REPLY: u8 = 12;
#[cfg(feature = "bench")]
const OUT_BENCH: u8 = 13;

/// `Frame` pixel formats. Only RGBA8 exists today: the engine composes the
/// screen in RGB (PERF_PLAN B5). An 8-bit indexed format plus its palette is
/// the natural next one, when the renderer writes palette indices.
pub(crate) const FORMAT_RGBA8: u8 = 0;

/// `State` flag bits: what the page's own UI needs to know every turn.
pub(crate) const STATE_MENU: u32 = 1;
/// The console has the keyboard (open, or forced up with nothing playing).
pub(crate) const STATE_CONSOLE: u32 = 2;
/// The live game (not a demo) is the active mode: the mouse may be captured.
pub(crate) const STATE_WALK: u32 = 4;
/// Customize controls is waiting for a key to bind.
pub(crate) const STATE_BIND_GRAB: u32 = 8;
/// A `timedemo` is running: frames come back to back, not per refresh.
pub(crate) const STATE_TIMEDEMO: u32 = 16;
/// Native resolution (`vid_native`): the page fills its box with the frame,
/// `pixel_size` device pixels to a frame pixel, instead of a 4:3 box.
pub(crate) const STATE_NATIVE: u32 = 32;
/// `vid_fkey`: the page's `f` toggles fullscreen.
pub(crate) const STATE_FKEY: u32 = 64;

/// A sound's placement, as `S_StartSound` gave it (the fields the page's
/// `SND_Spatialize` needs every frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub(crate) origin: [f32; 3],
    pub(crate) volume: f32,
    pub(crate) attenuation: f32,
}

/// The loop window of a sample, in seconds (`cue` chunk); `start < 0` for a
/// one-shot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LoopWindow {
    pub(crate) start: f32,
    pub(crate) end: f32,
}

/// One message to the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Msg<'a> {
    Frame { w: u16, h: u16, format: u8, pixels: &'a [u8] },
    Sync { seq: u32, wait: bool },
    State { flags: u32, menu_screen: i32, pixel_size: u32 },
    Sample { id: u32, wav: &'a [u8] },
    Sound { id: u32, at: Placement, entity: i32, channel: i32, view: bool, window: LoopWindow },
    StopSound { entity: i32, channel: i32 },
    StaticSound { id: u32, at: Placement, window: LoopWindow },
    Ambient { channel: u32, id: u32, window: LoopWindow },
    Listener { origin: [f32; 3], forward: [f32; 3], right: [f32; 3], ambient: [f32; 4], volume: f32 },
    Generation(u32),
    LocalSound { id: u32 },
    Reply { id: u32, value: f64, text: &'a str },
    #[cfg(feature = "bench")]
    Bench(&'a [f64]),
}

/// Little-endian field writer for a message's fixed part.
#[derive(Default)]
struct Fields(Vec<u8>);

impl Fields {
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i32(mut self, v: i32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32s(self, vs: &[f32]) -> Self {
        vs.iter().fold(self, |s, &v| s.f32(v))
    }
    fn f64(mut self, v: f64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn at(self, p: Placement) -> Self {
        self.f32s(&p.origin).f32(p.volume).f32(p.attenuation)
    }
    fn window(self, w: LoopWindow) -> Self {
        self.f32(w.start).f32(w.end)
    }
}

impl Msg<'_> {
    /// The record's kind, fixed fields, and trailing bytes (pixels, WAV,
    /// text), written after the fields without an intermediate copy.
    fn parts(&self) -> (u8, Vec<u8>, &[u8]) {
        let f = Fields::default();
        match *self {
            Msg::Frame { w, h, format, pixels } => {
                (OUT_FRAME, f.u16(w).u16(h).u8(format).u8(0).u16(0).0, pixels)
            }
            Msg::Sync { seq, wait } => (OUT_SYNC, f.u32(seq).u8(wait as u8).0, &[]),
            Msg::State { flags, menu_screen, pixel_size } => {
                (OUT_STATE, f.u32(flags).i32(menu_screen).u32(pixel_size).0, &[])
            }
            Msg::Sample { id, wav } => (OUT_SAMPLE, f.u32(id).0, wav),
            Msg::Sound { id, at, entity, channel, view, window } => (
                OUT_SOUND,
                f.u32(id).at(at).i32(entity).i32(channel).u32(view as u32).window(window).0,
                &[],
            ),
            Msg::StopSound { entity, channel } => (OUT_STOP_SOUND, f.i32(entity).i32(channel).0, &[]),
            Msg::StaticSound { id, at, window } => (OUT_STATIC_SOUND, f.u32(id).at(at).window(window).0, &[]),
            Msg::Ambient { channel, id, window } => (OUT_AMBIENT, f.u32(channel).u32(id).window(window).0, &[]),
            Msg::Listener { origin, forward, right, ambient, volume } => (
                OUT_LISTENER,
                f.f32s(&origin).f32s(&forward).f32s(&right).f32s(&ambient).f32(volume).0,
                &[],
            ),
            Msg::Generation(g) => (OUT_GENERATION, f.u32(g).0, &[]),
            Msg::LocalSound { id } => (OUT_LOCAL_SOUND, f.u32(id).0, &[]),
            Msg::Reply { id, value, text } => (OUT_REPLY, f.u32(id).f64(value).0, text.as_bytes()),
            #[cfg(feature = "bench")]
            Msg::Bench(values) => (OUT_BENCH, values.iter().fold(f, |f, &v| f.f64(v)).0, &[]),
        }
    }

    /// Write the record: its header, fixed fields and trailing bytes.
    pub(crate) fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let (kind, fields, tail) = self.parts();
        let len = u32::try_from(fields.len() + tail.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record over 4 GB"))?;
        let mut head = [0u8; 8];
        head[0] = kind;
        head[4..].copy_from_slice(&len.to_le_bytes());
        w.write_all(&head)?;
        w.write_all(&fields)?;
        w.write_all(tail)
    }
}

/// Encoders for the input side, for the tests and the native twin (the page
/// writes these records in JS).
#[cfg(test)]
pub(crate) mod encode {
    fn record(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![kind, 0];
        v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }
    pub(crate) fn tick(seq: u32, dt: f64) -> Vec<u8> {
        let mut p = seq.to_le_bytes().to_vec();
        p.extend_from_slice(&dt.to_le_bytes());
        record(super::IN_TICK, &p)
    }
    pub(crate) fn key(keynum: u8, down: bool, ch: u32) -> Vec<u8> {
        let mut p = vec![keynum, down as u8, 0, 0];
        p.extend_from_slice(&ch.to_le_bytes());
        record(super::IN_KEY, &p)
    }
    pub(crate) fn window(w: u32, h: u32) -> Vec<u8> {
        let mut p = w.to_le_bytes().to_vec();
        p.extend_from_slice(&h.to_le_bytes());
        record(super::IN_WINDOW, &p)
    }
    pub(crate) fn mouse(dx: f32, dy: f32) -> Vec<u8> {
        let mut p = dx.to_le_bytes().to_vec();
        p.extend_from_slice(&dy.to_le_bytes());
        record(super::IN_MOUSE, &p)
    }
    pub(crate) fn call(id: u32, line: &str) -> Vec<u8> {
        let mut p = id.to_le_bytes().to_vec();
        p.extend_from_slice(line.as_bytes());
        record(super::IN_CALL, &p)
    }
    pub(crate) fn end() -> Vec<u8> {
        record(super::IN_END, &[])
    }
    pub(crate) fn audio_ready(on: bool) -> Vec<u8> {
        record(super::IN_AUDIO_READY, &[on as u8])
    }
}

/// A decoded output record (tests and the native twin read the program's
/// stdout with it).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub(crate) kind: u8,
    pub(crate) payload: Vec<u8>,
}

#[cfg(test)]
impl Record {
    pub(crate) const FRAME: u8 = OUT_FRAME;
    pub(crate) const SYNC: u8 = OUT_SYNC;
    pub(crate) const STATE: u8 = OUT_STATE;
    pub(crate) const SAMPLE: u8 = OUT_SAMPLE;
    pub(crate) const REPLY: u8 = OUT_REPLY;
    pub(crate) const GENERATION: u8 = OUT_GENERATION;
    pub(crate) const AMBIENT: u8 = OUT_AMBIENT;
    pub(crate) const LISTENER: u8 = OUT_LISTENER;

    /// Split a stdout byte stream into records.
    pub(crate) fn split(mut bytes: &[u8]) -> Vec<Record> {
        let mut out = Vec::new();
        while bytes.len() >= 8 {
            let len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            out.push(Record { kind: bytes[0], payload: bytes[8..8 + len].to_vec() });
            bytes = &bytes[8 + len..];
        }
        assert!(bytes.is_empty(), "a truncated record");
        out
    }

    pub(crate) fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.payload[off..off + 4].try_into().unwrap())
    }

    pub(crate) fn f64_at(&self, off: usize) -> f64 {
        f64::from_le_bytes(self.payload[off..off + 8].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip_through_their_records() {
        let mut stream = Vec::new();
        stream.extend(encode::tick(7, 1.0 / 144.0));
        stream.extend(encode::key(b'w', true, 'w' as u32));
        stream.extend(encode::mouse(-3.0, 2.5));
        stream.extend(encode::call(9, "menu_visible"));
        stream.extend(encode::audio_ready(true));
        stream.extend(encode::end());
        let mut r = &stream[..];
        let mut got = Vec::new();
        while let Some(ev) = read_event(&mut r).unwrap() {
            got.push(ev);
        }
        assert_eq!(
            got,
            [
                Event::Tick { seq: 7, dt: 1.0 / 144.0 },
                Event::Key { keynum: b'w', down: true, ch: 'w' as u32 },
                Event::Mouse { dx: -3.0, dy: 2.5 },
                Event::Call { id: 9, line: "menu_visible".into() },
                Event::AudioReady(true),
                Event::End,
            ]
        );
    }

    #[test]
    fn a_short_record_reads_as_zeros_and_an_unknown_kind_is_skipped() {
        // A tick with only its seq: dt reads 0 (a frozen frame), not an error.
        let mut stream = vec![IN_TICK, 0, 4, 0, 5, 0, 0, 0];
        stream.extend([99, 0, 2, 0, 0xAA, 0xBB]);
        stream.extend(encode::end());
        let mut r = &stream[..];
        assert_eq!(read_event(&mut r).unwrap(), Some(Event::Tick { seq: 5, dt: 0.0 }));
        assert_eq!(read_event(&mut r).unwrap(), Some(Event::Unknown(99)));
        assert_eq!(read_event(&mut r).unwrap(), Some(Event::End));
        assert_eq!(read_event(&mut r).unwrap(), None, "a clean end of the stream");
    }

    #[test]
    fn a_stream_cut_inside_a_record_is_an_error() {
        let stream = encode::tick(1, 0.5);
        let mut r = &stream[..6];
        assert!(read_event(&mut r).is_err());
    }

    #[test]
    fn messages_carry_their_length_and_trailing_bytes() {
        let px = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let mut out = Vec::new();
        Msg::Frame { w: 2, h: 1, format: FORMAT_RGBA8, pixels: &px }.write_to(&mut out).unwrap();
        Msg::Reply { id: 3, value: 1.5, text: "ok" }.write_to(&mut out).unwrap();
        Msg::Sync { seq: 42, wait: true }.write_to(&mut out).unwrap();
        let recs = Record::split(&out);
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].kind, Record::FRAME);
        assert_eq!(&recs[0].payload[..4], &[2, 0, 1, 0]);
        assert_eq!(&recs[0].payload[8..], &px);
        assert_eq!((recs[1].u32_at(0), recs[1].f64_at(4)), (3, 1.5));
        assert_eq!(&recs[1].payload[12..], b"ok");
        assert_eq!((recs[2].kind, recs[2].u32_at(0), recs[2].payload[4]), (Record::SYNC, 42, 1));
    }

    #[test]
    fn sound_records_have_the_documented_sizes() {
        let at = Placement { origin: [1.0, 2.0, 3.0], volume: 1.0, attenuation: 1.0 };
        let window = LoopWindow { start: -1.0, end: 0.0 };
        let size = |m: Msg| {
            let mut out = Vec::new();
            m.write_to(&mut out).unwrap();
            out.len() - 8
        };
        assert_eq!(size(Msg::Sound { id: 1, at, entity: 2, channel: 3, view: false, window }), 44);
        assert_eq!(size(Msg::StaticSound { id: 1, at, window }), 32);
        assert_eq!(size(Msg::Ambient { channel: 0, id: 1, window }), 16);
        assert_eq!(
            size(Msg::Listener {
                origin: [0.0; 3],
                forward: [0.0; 3],
                right: [0.0; 3],
                ambient: [0.0; 4],
                volume: 0.7
            }),
            56
        );
        assert_eq!(size(Msg::State { flags: 0, menu_screen: 0, pixel_size: 0 }), 12);
    }
}
