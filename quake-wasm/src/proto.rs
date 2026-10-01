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
//! | 6 | `AudioReady` | `ready u8`, `0 u8 ×3`, `rate u32` (the device's sample rate; 0 unknown) |
//! | 7 | `Call` | `id u32`, then a UTF-8 line `name arg...` (automation, [`crate::automation`]) |
//! | 8 | `End` | — (the host's "nothing more is queued", the answer to a polling `Sync`) |
//! | 9 | `Window` | `w u32`, `h u32`: the page's box for the picture in device pixels (CSS size x `devicePixelRatio`), which native resolution renders into |
//! | 10 | `AudioClock` | `pos u32`: the sample pairs the page's audio device has played (a wrapping count; the page sends it before each `Tick`) |
//! | 11 | `AudioWake` | `pos u32`: the same, from the host between ticks while the device plays: mix now (`S_ExtraUpdate`) |
//! | 12 | `Present` | `format u8`: how the page shows frames ([`FORMAT_RGBA8`] or [`FORMAT_INDEXED8`]; RGBA until it says) |
//! | 20 | `Gamepad` | `connected u8`, `standard u8`, `buttons u8`, `0 u8`, `pressed u32`, `axes f32×6`: the pad's state, polled each display refresh ([`quake_rs::client::in_win::Pad`]) |
//!
//! **Out** (program → host), an 8-byte header `[kind u8][0 u8 ×3][len u32]`
//! and `len` payload bytes:
//!
//! | kind | message | payload |
//! |---|---|---|
//! | 1 | `Frame` | `w u16`, `h u16`, `format u8`, `0 u8 ×3`, then for [`FORMAT_INDEXED8`] the palette (256 × RGBA), then the pixels |
//! | 2 | `Sync` | `seq u32` (the last tick consumed), `wait u8` (1: block for the next tick; 0: poll) |
//! | 3 | `State` | `flags u32` ([`STATE_MENU`] …), `menu_screen i32`, `pixel_size u32` (native: device pixels per picture pixel; 0 in the 4:3 box) |
//! | 4–11 | — | (retired: the sound records of the page's own mixing) |
//! | 12 | `Reply` | `id u32`, `value f64`, then UTF-8 text (the answer to a `Call`) |
//! | 13 | `Bench` | `f64` per value (`--features bench`: the frame's phase times) |
//! | 14 | `Pcm` | `start u32` (the pair it plays at, in the `AudioClock`'s count), `rate u32`, `flags u32` (1: silence what was mixed ahead first, `S_ClearBuffer`), then 16-bit stereo pairs: what the mixer painted this tick, for the page's ring |
//! | 15 | `Audio` | `rate u32`, `mode u32` (0 Classic, 1 2026), `starts u32`, `local u32`, `stops u32`, `clears u32`, `painted u32`: the sound device's counts |
//! | 16 | `Cd` | `serial u32` (a new value: play `track` from its top), `track u8`, `looping u8`, `mode u8` (0 stopped, 1 playing, 2 paused), `0 u8`, `volume f32` (0..1): the CD player's state, written when it changes (only with a disc: the player's music) |
//! | 17 | `FrameAt` | `w u16`, `h u16`, `format u8`, `slot u8`, `0 u16`, `pixels u32`, `palette u32`: a frame left in the program's shared memory, at those addresses ([`crate::present`]) |
//! | 18 | `Quit` | `registered u8`, `0 u8 ×3`, then (if the pak had the file) 4000 bytes: id's end screen (`end2.bin` registered, else `end1.bin` — 80x25 of (character, attribute) VGA text-mode pairs, the DOS build's version stamped into row 0 as `Sys_Quit` did). Written once, the game's last message: `Host_Quit_f`/`M_Quit_Key` decided to quit, the host should leave fullscreen and release the pointer, and the program ends right after (as id's `exit(0)` did) |
//! | 20 | `Rumble` | `strong f32`, `weak f32`, `ms u32`, `pad u32` (1: the pad is read, rumble it; 0: a phone's vibration): the 2026 `joy_rumble` |
//!
//! A `Sync` ends each turn of the program's loop: everything before it is
//! one turn's output, and the host publishes it then.

use std::io::{self, Read, Write};

use quake_rs::client::in_win::{Pad, Rumble, JOY_MAX_AXES};

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
const IN_AUDIO_CLOCK: u8 = 10;
const IN_AUDIO_WAKE: u8 = 11;
const IN_PRESENT: u8 = 12;
const IN_GAMEPAD: u8 = 20;

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
    /// The page's audio output is running (or stopped), on a device at `rate`
    /// Hz (0: not known yet).
    AudioReady { running: bool, rate: u32 },
    /// The sample pairs the page's audio device has played (wrapping).
    AudioClock(u32),
    /// The same between ticks, from the host while the device plays: time
    /// to mix again.
    AudioWake(u32),
    /// An automation call, answered by a [`Msg::Reply`] with the same `id`.
    Call { id: u32, line: String },
    /// The host has nothing more queued (the answer to a polling sync).
    End,
    /// The page's box for the picture, in device pixels.
    Window { w: u32, h: u32 },
    /// How the page shows the frames from now on: [`FORMAT_RGBA8`] or
    /// [`FORMAT_INDEXED8`] (another value is RGBA).
    Present(u8),
    /// The pad's state this refresh; `None`: no pad is connected.
    Gamepad(Option<Pad>),
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
        IN_AUDIO_READY => {
            let running = p.u8() != 0;
            p.skip(3);
            Event::AudioReady { running, rate: p.u32() }
        }
        IN_AUDIO_CLOCK => Event::AudioClock(p.u32()),
        IN_AUDIO_WAKE => Event::AudioWake(p.u32()),
        IN_CALL => {
            let id = p.u32();
            Event::Call { id, line: String::from_utf8_lossy(p.rest()).into_owned() }
        }
        IN_END => Event::End,
        IN_WINDOW => Event::Window { w: p.u32(), h: p.u32() },
        IN_PRESENT => Event::Present(p.u8()),
        IN_GAMEPAD => Event::Gamepad(read_pad(&mut p)),
        other => Event::Unknown(other),
    };
    Ok(Some(ev))
}

/// A `Gamepad` record's pad: `None` when it says none is connected.
fn read_pad(p: &mut Payload) -> Option<Pad> {
    let connected = p.u8() != 0;
    let standard = p.u8() != 0;
    let num_buttons = p.u8().min(32);
    p.skip(1);
    let pressed = p.u32();
    let mut axes = [0.0; JOY_MAX_AXES];
    for a in &mut axes {
        *a = p.f32();
    }
    connected.then_some(Pad { standard, num_buttons, pressed, axes })
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
const OUT_REPLY: u8 = 12;
#[cfg(feature = "bench")]
const OUT_BENCH: u8 = 13;
const OUT_PCM: u8 = 14;
const OUT_AUDIO: u8 = 15;
const OUT_CD: u8 = 16;
const OUT_FRAME_AT: u8 = 17;
const OUT_QUIT: u8 = 18;
const OUT_RUMBLE: u8 = 20;

/// `Frame` pixel formats. RGBA8: four bytes a pixel, what a 2-D canvas
/// takes. INDEXED8: the engine's own frame, a palette index a pixel, with the
/// 256 colours (RGBA each) it is shown through — a quarter of the bytes, and
/// the page's GPU is the DAC ([`crate::present`]).
pub(crate) const FORMAT_RGBA8: u8 = 0;
pub(crate) const FORMAT_INDEXED8: u8 = 1;

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
/// `in_touch`: on a touch screen, the page shows its touch controls for play.
pub(crate) const STATE_TOUCH: u32 = 128;
/// The menu waits for y or n (the Quit prompt, New Game's question): a touch
/// screen offers them as buttons.
pub(crate) const STATE_ASK: u32 = 256;
/// The live game is paused (`pause`, `sv.paused`).
pub(crate) const STATE_PAUSED: u32 = 512;

/// `Pcm` flags: silence what was mixed ahead before these samples
/// (`S_ClearBuffer`).
pub(crate) const PCM_CLEAR: u32 = 1;

/// The sound device's counts (the `Audio` record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioCounts {
    pub(crate) rate: u32,
    /// 0 Classic, 1 2026.
    pub(crate) mode: u32,
    pub(crate) starts: u32,
    pub(crate) local: u32,
    pub(crate) stops: u32,
    pub(crate) clears: u32,
    pub(crate) painted: u32,
}

/// One message to the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Msg<'a> {
    /// A frame's bytes: the palette (empty for RGBA) and the pixels.
    Frame { w: u16, h: u16, format: u8, palette: &'a [u8], pixels: &'a [u8] },
    /// A frame left where it lies in the program's shared memory: ring slot
    /// `slot`, its pixels and palette at those addresses.
    FrameAt { w: u16, h: u16, format: u8, slot: u8, pixels: u32, palette: u32 },
    Sync { seq: u32, wait: bool },
    State { flags: u32, menu_screen: i32, pixel_size: u32 },
    Reply { id: u32, value: f64, text: &'a str },
    #[cfg(feature = "bench")]
    Bench(&'a [f64]),
    /// A tick's samples: `pairs` is the 16-bit stereo pairs as little-endian
    /// bytes.
    Pcm { start: u32, rate: u32, flags: u32, pairs: &'a [u8] },
    Audio(AudioCounts),
    /// The CD player's state ([`quake_rs::cd_audio::CdState`]): the page
    /// plays the player's file for the track, beside the sound ring.
    Cd(quake_rs::cd_audio::CdState),
    /// A rumble, and whether the pad is read (else a phone vibrates).
    Rumble { rumble: Rumble, pad: bool },
    /// The game just quit (`Sys_Quit`): `registered` says which end screen
    /// id would show, and `screen` is that file's 4000 raw bytes when the
    /// pak had it ([`crate::sys::end_screen`]) — the host draws it with no
    /// extra round trip. The program ends right after this message.
    Quit { registered: bool, screen: Option<&'a [u8]> },
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
    fn f64(mut self, v: f64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(mut self, v: &[u8]) -> Self {
        self.0.extend_from_slice(v);
        self
    }
}

impl Msg<'_> {
    /// The record's kind, fixed fields, and trailing bytes (pixels, WAV,
    /// text), written after the fields without an intermediate copy.
    fn parts(&self) -> (u8, Vec<u8>, &[u8]) {
        let f = Fields::default();
        match *self {
            Msg::Frame { w, h, format, palette, pixels } => {
                (OUT_FRAME, f.u16(w).u16(h).u8(format).u8(0).u16(0).bytes(palette).0, pixels)
            }
            Msg::FrameAt { w, h, format, slot, pixels, palette } => {
                (OUT_FRAME_AT, f.u16(w).u16(h).u8(format).u8(slot).u16(0).u32(pixels).u32(palette).0, &[])
            }
            Msg::Sync { seq, wait } => (OUT_SYNC, f.u32(seq).u8(wait as u8).0, &[]),
            Msg::State { flags, menu_screen, pixel_size } => {
                (OUT_STATE, f.u32(flags).i32(menu_screen).u32(pixel_size).0, &[])
            }
            Msg::Reply { id, value, text } => (OUT_REPLY, f.u32(id).f64(value).0, text.as_bytes()),
            #[cfg(feature = "bench")]
            Msg::Bench(values) => (OUT_BENCH, values.iter().fold(f, |f, &v| f.f64(v)).0, &[]),
            Msg::Pcm { start, rate, flags, pairs } => (OUT_PCM, f.u32(start).u32(rate).u32(flags).0, pairs),
            Msg::Audio(c) => (
                OUT_AUDIO,
                f.u32(c.rate).u32(c.mode).u32(c.starts).u32(c.local).u32(c.stops).u32(c.clears).u32(c.painted).0,
                &[],
            ),
            Msg::Cd(cd) => {
                use quake_rs::cd_audio::CdMode;
                let mode = match cd.mode {
                    CdMode::Stopped => 0,
                    CdMode::Playing => 1,
                    CdMode::Paused => 2,
                };
                (OUT_CD, f.u32(cd.serial).u8(cd.track).u8(u8::from(cd.looping)).u8(mode).u8(0).f32(cd.volume).0, &[])
            }
            Msg::Rumble { rumble: r, pad } => {
                (OUT_RUMBLE, f.f32(r.strong).f32(r.weak).u32(r.ms).u32(u32::from(pad)).0, &[])
            }
            Msg::Quit { registered, screen } => {
                (OUT_QUIT, f.u8(u8::from(registered)).u8(0).u16(0).0, screen.unwrap_or(&[]))
            }
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
    pub(crate) fn audio_ready(on: bool, rate: u32) -> Vec<u8> {
        let mut p = vec![on as u8, 0, 0, 0];
        p.extend_from_slice(&rate.to_le_bytes());
        record(super::IN_AUDIO_READY, &p)
    }
    pub(crate) fn audio_clock(pos: u32) -> Vec<u8> {
        record(super::IN_AUDIO_CLOCK, &pos.to_le_bytes())
    }
    pub(crate) fn audio_wake(pos: u32) -> Vec<u8> {
        record(super::IN_AUDIO_WAKE, &pos.to_le_bytes())
    }
    pub(crate) fn present(format: u8) -> Vec<u8> {
        record(super::IN_PRESENT, &[format])
    }
    /// A `Gamepad` record: `None` is "no pad connected".
    pub(crate) fn gamepad(pad: Option<quake_rs::client::in_win::Pad>) -> Vec<u8> {
        let p = pad.unwrap_or_default();
        let mut v = vec![pad.is_some() as u8, p.standard as u8, p.num_buttons, 0];
        v.extend_from_slice(&p.pressed.to_le_bytes());
        for a in p.axes {
            v.extend_from_slice(&a.to_le_bytes());
        }
        record(super::IN_GAMEPAD, &v)
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
    pub(crate) const FRAME_AT: u8 = OUT_FRAME_AT;
    pub(crate) const SYNC: u8 = OUT_SYNC;
    pub(crate) const STATE: u8 = OUT_STATE;
    pub(crate) const REPLY: u8 = OUT_REPLY;
    pub(crate) const PCM: u8 = OUT_PCM;
    pub(crate) const AUDIO: u8 = OUT_AUDIO;
    pub(crate) const CD: u8 = OUT_CD;
    pub(crate) const QUIT: u8 = OUT_QUIT;

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
        stream.extend(encode::audio_ready(true, 48000));
        stream.extend(encode::audio_clock(4096));
        stream.extend(encode::audio_wake(4200));
        stream.extend(encode::present(FORMAT_INDEXED8));
        let pad = Pad { standard: true, num_buttons: 17, pressed: 0b101, axes: [0.5, -1.0, 0.0, 0.25, 1.0, 0.0] };
        stream.extend(encode::gamepad(Some(pad)));
        stream.extend(encode::gamepad(None));
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
                Event::AudioReady { running: true, rate: 48000 },
                Event::AudioClock(4096),
                Event::AudioWake(4200),
                Event::Present(FORMAT_INDEXED8),
                Event::Gamepad(Some(pad)),
                Event::Gamepad(None),
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
        Msg::Frame { w: 2, h: 1, format: FORMAT_RGBA8, palette: &[], pixels: &px }.write_to(&mut out).unwrap();
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
    fn frames_carry_their_palette_or_where_they_lie() {
        let (palette, px) = ([7u8; 1024], [1u8, 2, 3, 4, 5, 6]);
        let mut out = Vec::new();
        Msg::Frame { w: 3, h: 2, format: FORMAT_INDEXED8, palette: &palette, pixels: &px }.write_to(&mut out).unwrap();
        Msg::FrameAt { w: 3, h: 2, format: FORMAT_INDEXED8, slot: 2, pixels: 0x1234, palette: 0x5678 }
            .write_to(&mut out)
            .unwrap();
        let recs = Record::split(&out);
        assert_eq!((recs[0].kind, recs[0].payload[4], recs[0].payload.len()), (Record::FRAME, 1, 8 + 1024 + 6));
        assert_eq!((&recs[0].payload[8..1032], &recs[0].payload[1032..]), (&palette[..], &px[..]));
        assert_eq!((recs[1].kind, recs[1].payload.len()), (Record::FRAME_AT, 16));
        assert_eq!(&recs[1].payload[..6], &[3, 0, 2, 0, 1, 2]);
        assert_eq!((recs[1].u32_at(8), recs[1].u32_at(12)), (0x1234, 0x5678));
    }

    #[test]
    fn sound_records_have_the_documented_sizes() {
        let size = |m: Msg| {
            let mut out = Vec::new();
            m.write_to(&mut out).unwrap();
            out.len() - 8
        };
        let pairs = [1u8, 0, 2, 0, 3, 0, 4, 0];
        assert_eq!(size(Msg::Pcm { start: 5, rate: 11025, flags: PCM_CLEAR, pairs: &pairs }), 12 + 8);
        let c = AudioCounts { rate: 48000, mode: 1, starts: 2, local: 3, stops: 0, clears: 1, painted: 7 };
        assert_eq!(size(Msg::Audio(c)), 28);
        assert_eq!(size(Msg::State { flags: 0, menu_screen: 0, pixel_size: 0 }), 12);
        let cd = quake_rs::cd_audio::CdState {
            serial: 3,
            track: 6,
            looping: true,
            mode: quake_rs::cd_audio::CdMode::Paused,
            volume: 0.5,
        };
        let mut out = Vec::new();
        Msg::Cd(cd).write_to(&mut out).unwrap();
        assert_eq!(&out[..8], &[16, 0, 0, 0, 12, 0, 0, 0]);
        assert_eq!(&out[8..], &[3, 0, 0, 0, 6, 1, 2, 0, 0, 0, 0, 0x3f]);
        assert_eq!(size(Msg::Rumble { rumble: Rumble { strong: 1.0, weak: 0.5, ms: 200 }, pad: true }), 16);
    }
}
