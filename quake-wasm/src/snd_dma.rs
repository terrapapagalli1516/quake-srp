//! The platform's sound — what id's `snd_win.c` is to `snd_dma.c`: the
//! device behind the engine's own mixer. id's mixer ([`Mixer`]: `snd_dma.c`,
//! `snd_mix.c` and `snd_mem.c`) runs here, in the program; the page only
//! plays what it paints.
//!
//! - **The calls.** The sound layer's calls are collected as the game makes
//!   them: each client frame's and each level load's [`SoundCall`]s
//!   ([`play`]), and the `play` command's ([`s_play`]). Collected, because
//!   the game makes them from inside the host's borrow of the [`App`] while
//!   the mixer belongs to the loop.
//! - **The mix.** After every tick, a host frame or not (id's
//!   `S_ExtraUpdate` mixed between frames too), the loop ([`crate::sys`])
//!   runs [`Audio::frame`]: the calls, then the menu's clicks
//!   (`S_LocalSound`), then `S_Update_` — paint from where the mixer left
//!   off to the device's position plus `_snd_mixahead` — and sends the
//!   samples in a `Pcm` record. `web/wasi.js` writes them into a shared ring
//!   and the page's AudioWorklet plays it (`web/PLATFORM.md`, "Sound").
//! - **The clock.** The device's position is the pairs the worklet has
//!   played, which the page sends before each tick (`AudioClock`). While the
//!   page's audio is not running (before the first click, a hidden tab) the
//!   page moves it on in real time, so the mixer plays on unheard, as a DMA
//!   device runs whether a speaker is on or not: no backlog of sounds waits
//!   for the first click. With no clock at all (a native run on a pipe) the
//!   loop's own frame times move it.
//! - **Which mixer** is [`SoundMode`], the setting `snd_modern`
//!   ([`Cvars::sound`], on in the 2026 profile), read at every mix: Classic
//!   is id's mixer at id's
//!   11025 Hz, which the worklet reconstructs at the device's rate as a
//!   sound card's DAC and output filter did; the 2026 mixer runs at the
//!   device's rate with [`quake_rs::snd::Fixes::ALL`]. A new mode, or a
//!   device rate learned late, makes a new mixer; the level's placed sounds
//!   are registered with it again.
//! - **The CD** ([`CdAudio`], `cd_win.c`'s state) is fed by the same queue:
//!   the client's [`SoundCall::Cd`]s and the `cd` command go to it instead
//!   of the mixer, and every mix brings its level up to `bgmvolume`
//!   (`CDAudio_Update`). It plays beside the mix, as a drive did: the loop
//!   sends its state to the page ([`Audio::cd_state`], a `Cd` record), and
//!   the page plays the player's own file for the track. The disc is the
//!   tracks the page says it has (`-cdtracks`); without them there is no
//!   drive, as with id's `cd_null.c`.
//!
//! [`App`]: crate::app::App
//! [`Cvars::sound`]: quake_rs::cvar::Cvars::sound

use std::cell::RefCell;
use std::fmt::Write as _;

use quake_rs::cd_audio::{CdAudio, CdState, Disc};
use quake_rs::client::{Listener, SoundCall};
use quake_rs::pak::Pak;
use quake_rs::server::StaticSound;
use quake_rs::snd::{Mixer, SoundMode, MODERN_MIXAHEAD};

use crate::app::{ensure_app, APP};
use crate::common::pak;

/// The shared ring's size in sample pairs (`web/PLATFORM.md`, "Sound"): the
/// most the mixer paints ahead of the device.
pub(crate) const RING_PAIRS: usize = 32768;

/// A call into the sound layer, as the game made it.
#[derive(Debug, Clone)]
enum Request {
    /// A client frame's or a level load's call.
    Client(SoundCall),
    /// `S_Play`: the `play` command's samples.
    Play(Vec<String>),
    /// `CD_f`: the `cd` command's arguments (`cd` first).
    CdCommand(Vec<String>),
}

thread_local! {
    /// The calls since the loop last took them, in the order they were made.
    static PENDING: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    /// The listener as of the last `S_Update` (the automation's `listener_*`).
    static LISTENER: RefCell<Listener> = const { RefCell::new(Listener::zero()) };
    /// `S_StopAllSounds` so far (the automation's `sound_generation`).
    static SOUND_GENERATION: RefCell<i32> = const { RefCell::new(0) };
}

/// Carry out the calls a client frame or a level load made into the sound
/// layer ([`SoundCall`]), in the order it made them: they wait for the loop's
/// next [`Audio::frame`]. The samples load from the program's pak, which is
/// the client's.
pub(crate) fn play(_pak: &Pak, calls: Vec<SoundCall>) {
    for call in &calls {
        match call {
            SoundCall::Update { listener, .. } => LISTENER.with(|l| *l.borrow_mut() = *listener),
            SoundCall::StopAll => SOUND_GENERATION.with(|g| *g.borrow_mut() += 1),
            _ => {}
        }
    }
    PENDING.with(|p| p.borrow_mut().extend(calls.into_iter().map(Request::Client)));
}

/// `S_Play` (snd_dma.c, the `play` command): each named sample from the
/// listener, at full volume. The page's sound button plays
/// `items/r_item1.wav` with it.
pub(crate) fn s_play(names: &[&str]) {
    let names = names.iter().map(|n| n.to_string()).collect();
    PENDING.with(|p| p.borrow_mut().push(Request::Play(names)));
}

/// `CD_f`, the `cd` command (`argv[0]` is `cd`): carried out by the CD at the
/// loop's next mix, its lines on the console.
pub(crate) fn cd_command(argv: &[&str]) {
    let argv = argv.iter().map(|a| a.to_string()).collect();
    PENDING.with(|p| p.borrow_mut().push(Request::CdCommand(argv)));
}

/// The listener pose as of the last client frame's `S_Update`.
pub(crate) fn listener() -> Listener {
    LISTENER.with(|l| *l.borrow())
}

/// How many times every sound has stopped (`S_StopAllSounds`: a level or
/// mode change).
pub(crate) fn sound_generation() -> i32 {
    SOUND_GENERATION.with(|g| *g.borrow())
}

/// The Options "Volume" (`volume`, 0..1, default 0.7): the mixer's master
/// volume. 1.0 before the App exists.
pub(crate) fn volume() -> f32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.settings.cvars.volume).unwrap_or(1.0))
}

/// The mixer the player chose: the `snd_modern` setting ([`SoundMode`]).
fn sound_mode() -> SoundMode {
    APP.with(|c| c.borrow().as_ref().map(|a| a.settings.cvars.sound).unwrap_or_default())
}

/// The menu's `S_LocalSound`s since the last frame (its clicks).
fn menu_sounds() -> Vec<&'static str> {
    let mut out = Vec::new();
    ensure_app(|a| out = a.menu.take_sounds().into_iter().map(|s| s.sample()).collect());
    out
}

/// What the sound device has done so far: the page shows some of it, the
/// browser checks read it (the `Audio` record, and `snd_stats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioStats {
    /// `S_StartSound` calls (every event of the client's `Start` calls).
    pub(crate) starts: u32,
    /// `S_LocalSound`s: the menu's clicks.
    pub(crate) local: u32,
    /// `S_StopSound` calls (`svc_stopsound`).
    pub(crate) stops: u32,
    /// `S_ClearBuffer`s asked of the page (every `S_StopAllSounds`, and a new
    /// mixer).
    pub(crate) clears: u32,
    /// Sample pairs painted (wraps).
    pub(crate) painted: u32,
}

/// One tick's samples for the page's ring: 16-bit stereo, `start` being the
/// pair (in the mixer's count, which is the device's) the first of them
/// plays at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Pcm<'a> {
    pub(crate) start: u32,
    pub(crate) rate: u32,
    /// Silence what was mixed ahead before these (`S_ClearBuffer`).
    pub(crate) clear: bool,
    pub(crate) samples: &'a [i16],
}

/// The sound device: the mixer, the clock it paints against, and what a new
/// mixer needs to take over (the level's placed sounds, the view entity).
pub(crate) struct Audio {
    /// The pak the samples load from (the program's, opened at startup).
    pak: Option<Pak>,
    mixer: Option<Mixer>,
    /// The mode and device rate the mixer was made for.
    made_for: (SoundMode, u32),
    /// The page's audio: running, and its device's rate (0: not yet known).
    running: bool,
    device_rate: u32,
    /// The device's position in pairs: the page's clock, unwrapped.
    clock: i64,
    /// The page's last clock, as it sent it (a wrapping u32).
    last_pos: Option<u32>,
    /// A clock came from the page since the last frame.
    clocked: bool,
    /// Pairs of the self-kept clock not yet whole (no page clock).
    frac: f64,
    /// This level's `S_StaticSound`s, for a new mixer.
    statics: Vec<StaticSound>,
    /// `cl.viewentity`, for a new mixer.
    view_entity: i32,
    /// [`SoundMode::Modern`]'s lead now: [`MODERN_MIXAHEAD`] normally, grown
    /// by [`adapt_modern_ahead`] when a host frame ran long and eased back
    /// when frames are quick again. Classic's lead is id's fixed 0.1 s,
    /// never touched.
    modern_ahead: f32,
    pub(crate) stats: AudioStats,
    /// The last tick's samples: where they start, whether they clear the
    /// ring first, the pairs.
    start: u32,
    cleared: bool,
    pcm: Vec<i16>,
    /// The CD player, beside the mixer.
    cd: CdAudio,
}

impl Default for Audio {
    fn default() -> Self {
        Audio::new()
    }
}

impl Audio {
    /// No mixer yet: it is made at the first frame, when the pak is open and
    /// the mode is known.
    pub(crate) fn new() -> Audio {
        Audio {
            pak: None,
            mixer: None,
            made_for: (SoundMode::default(), 0),
            running: false,
            device_rate: 0,
            clock: 0,
            last_pos: None,
            clocked: false,
            frac: 0.0,
            statics: Vec::new(),
            view_entity: 0,
            modern_ahead: MODERN_MIXAHEAD,
            stats: AudioStats::default(),
            start: 0,
            cleared: false,
            pcm: Vec::new(),
            cd: CdAudio::new(None),
        }
    }

    /// `CDAudio_Init`: a drive with the player's music in it (`None`: no
    /// music, no drive).
    pub(crate) fn set_disc(&mut self, disc: Option<Disc>) {
        self.cd = CdAudio::new(disc);
    }

    /// What the CD plays, for the page; `None` without a drive.
    pub(crate) fn cd_state(&self) -> Option<CdState> {
        self.cd.has_drive().then(|| self.cd.state())
    }

    /// The page's audio started or stopped (`AudioReady`), on a device at
    /// `rate` Hz (0: unknown).
    pub(crate) fn device(&mut self, running: bool, rate: u32) {
        self.running = running;
        if rate != 0 {
            self.device_rate = rate;
        }
    }

    /// The mode the mixer was made for.
    pub(crate) fn mode(&self) -> SoundMode {
        self.made_for.0
    }

    /// The device's position, in pairs (`AudioClock`: a wrapping count).
    pub(crate) fn clock(&mut self, pos: u32) {
        match self.last_pos {
            // The page's count is the ring's: the first one sets the clock.
            None => self.clock = i64::from(pos),
            Some(last) => self.clock += i64::from((pos.wrapping_sub(last) as i32).max(0)),
        }
        self.last_pos = Some(pos);
        self.clocked = true;
    }

    /// The mixer, made (again) when the mode or the device's rate asks for
    /// another: `S_Init` at the mode's rate, then the level's placed sounds.
    fn mixer(&mut self, pak: &Pak) -> &mut Mixer {
        let want = (sound_mode(), self.device_rate);
        let rate_now = self.mixer.as_ref().map(Mixer::rate);
        if rate_now.is_none() || want.0 != self.made_for.0 || Some(want.0.rate(want.1)) != rate_now {
            let mut m = want.0.mixer(pak, want.1);
            m.set_view_entity(self.view_entity);
            for s in &self.statics {
                m.static_sound(pak, s);
            }
            self.mixer = Some(m);
            self.made_for = want;
        }
        self.mixer.as_mut().expect("made above")
    }

    /// One tick: the calls since the last, the menu's clicks, and `S_Update_`
    /// — the samples from where the mixer left off to the device's position
    /// plus `_snd_mixahead` (`dt` moves the position on when the page sent
    /// none). `host_elapsed` is the wall-clock time the host frame just took
    /// to compute (`None` for a wake between ticks, which ran no frame): a
    /// tick that took long feeds [`adapt_modern_ahead`], in [`SoundMode::
    /// Modern`] only — Classic's `_snd_mixahead` is id's fixed 0.1 s.
    /// `None` without a pak.
    pub(crate) fn frame(&mut self, dt: f64, host_elapsed: Option<f64>) -> Option<Pcm<'_>> {
        if self.pak.is_none() {
            self.pak = pak();
        }
        let pak = self.pak.take()?;
        let pcm = self.mix(&pak, dt, host_elapsed);
        self.pak = Some(pak);
        pcm.then(|| {
            let mixer = self.mixer.as_ref().expect("mixed");
            Pcm { start: self.start, rate: mixer.rate(), clear: self.cleared, samples: &self.pcm }
        })
    }

    /// [`Audio::frame`]'s work: true when it painted.
    fn mix(&mut self, pak: &Pak, dt: f64, host_elapsed: Option<f64>) -> bool {
        self.mixer(pak);
        let requests = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
        for r in requests {
            self.request(pak, r);
        }
        // CDAudio_Update: the drive's level follows the slider.
        ensure_app(|a| self.cd.update(&mut a.settings.cvars.bgmvolume));
        let mixer = self.mixer.as_mut().expect("made above");
        for sample in menu_sounds() {
            mixer.local_sound(pak, sample);
            self.stats.local = self.stats.local.wrapping_add(1);
        }
        mixer.cvars.volume = volume();

        // 2026 only: id's own `_snd_mixahead` (Classic, [`SoundMode::
        // mixahead`]) is never touched. A host frame that took
        // `host_elapsed` left the worker unable to mix for that long (it is
        // one thread: busy rendering, it cannot answer the page's AudioWake
        // between ticks either), which can run the ring dry; this grows the
        // lead at once to cover a repeat, then eases it back towards
        // `MODERN_MIXAHEAD` as frames come in quick (`web/PLATFORM.md`,
        // "Sound").
        if self.made_for.0 == SoundMode::Modern {
            if let Some(elapsed) = host_elapsed {
                self.modern_ahead = adapt_modern_ahead(self.modern_ahead, elapsed as f32, dt as f32);
            }
            mixer.cvars.mixahead = self.modern_ahead;
        }

        if !self.clocked {
            // No page clock this tick: the device ran for `dt`.
            self.frac += dt.clamp(0.0, 1.0) * f64::from(mixer.rate());
            let whole = self.frac.floor();
            self.frac -= whole;
            self.clock += whole as i64;
        }
        self.clocked = false;

        // S_Update_: mix ahead of the device.
        let n = mixer.samples_ahead(self.clock, RING_PAIRS);
        self.start = mixer.painted_time() as u32;
        self.cleared = mixer.take_clear();
        self.pcm.clear();
        self.pcm.resize(2 * n, 0);
        mixer.paint(&mut self.pcm);
        self.stats.painted = self.stats.painted.wrapping_add(n as u32);
        if self.cleared {
            self.stats.clears = self.stats.clears.wrapping_add(1);
        }
        true
    }

    /// Carry out one call, keeping what a new mixer would need.
    fn request(&mut self, pak: &Pak, r: Request) {
        let Some(mixer) = self.mixer.as_mut() else { return };
        match r {
            Request::Client(call) => {
                match &call {
                    SoundCall::StopAll => self.statics.clear(),
                    SoundCall::Static(s) => self.statics.extend_from_slice(s),
                    SoundCall::Start { events, view_entity } => {
                        self.view_entity = *view_entity;
                        self.stats.starts = self.stats.starts.wrapping_add(events.len() as u32);
                    }
                    SoundCall::Stop(s) => self.stats.stops = self.stats.stops.wrapping_add(s.len() as u32),
                    SoundCall::Update { .. } => {}
                    SoundCall::Cd(c) => {
                        let mut con = Vec::new();
                        self.cd.call(*c, &mut con);
                        print_lines(con);
                        return;
                    }
                }
                mixer.run(pak, std::slice::from_ref(&call));
            }
            Request::Play(names) => {
                for name in names {
                    mixer.play(pak, &name);
                }
            }
            Request::CdCommand(argv) => {
                let mut con = Vec::new();
                self.cd.command(&argv.iter().map(String::as_str).collect::<Vec<_>>(), &mut con);
                print_lines(con);
            }
        }
    }

    /// The sound's own automation calls (`snd_stats`, `snd_channels`), or
    /// `None` for a call that is not one.
    pub(crate) fn call(&self, line: &str) -> Option<(f64, String)> {
        let name = line.split_whitespace().next()?;
        let mixer = self.mixer.as_ref();
        let mut t = String::new();
        match name {
            // The device and its counts, `name=value` separated by spaces.
            "snd_stats" => {
                let s = self.stats;
                let _ = write!(
                    t,
                    "rate={} mode={} running={} device_rate={} clock={} painted_time={} starts={} local={} \
                     stops={} clears={} painted={} statics={} sounding={} mixahead_ms={:.1}",
                    mixer.map_or(0, Mixer::rate),
                    self.made_for.0.name(),
                    u8::from(self.running),
                    self.device_rate,
                    self.clock,
                    mixer.map_or(0, Mixer::painted_time),
                    s.starts,
                    s.local,
                    s.stops,
                    s.clears,
                    s.painted,
                    mixer.map_or(0, |m| m.total_channels() - quake_rs::snd::MAX_DYNAMIC_CHANNELS - 4),
                    mixer.map_or(0, |m| m.channels().filter(|c| c.leftvol > 0 || c.rightvol > 0).count()),
                    f64::from(mixer.map_or(0.0, |m| m.cvars.mixahead)) * 1000.0,
                );
                Some((0.0, t))
            }
            // Every channel with a sound: `index sample left right master pos
            // end entity channel`, a line each.
            "snd_channels" => {
                for c in mixer.into_iter().flat_map(Mixer::channels) {
                    let _ = writeln!(
                        t,
                        "{} {} {} {} {} {} {} {} {}",
                        c.index, c.sample, c.leftvol, c.rightvol, c.master_vol, c.pos, c.end, c.entnum, c.entchannel
                    );
                }
                Some((f64::from(mixer.map_or(0, |m| m.channels().count() as u32)), t))
            }
            _ => None,
        }
    }
}

impl Audio {
    /// The CD's automation calls — `cd_state`, and `cd_ended <serial>`, the
    /// page's word that a track played to its end (MCI's notify) — or `None`
    /// for a call that is not one.
    pub(crate) fn cd_call(&mut self, line: &str) -> Option<(f64, String)> {
        match line.split_whitespace().next()? {
            "cd_state" => {
                let s = self.cd.state();
                let tracks: Vec<String> = self.cd.tracks().iter().map(u8::to_string).collect();
                let text = format!(
                    "drive={} serial={} track={} looping={} mode={:?} volume={:.3} tracks={}",
                    u8::from(self.cd.has_drive()),
                    s.serial,
                    s.track,
                    u8::from(s.looping),
                    s.mode,
                    s.volume,
                    tracks.join(",")
                );
                Some((f64::from(s.track), text))
            }
            "cd_ended" => {
                let serial = line.split_whitespace().nth(1).and_then(|n| n.parse().ok()).unwrap_or(u32::MAX);
                let mut con = Vec::new();
                self.cd.track_ended(serial, &mut con);
                print_lines(con);
                Some((0.0, String::new()))
            }
            _ => None,
        }
    }
}

/// Lines for the console (`Con_Printf` from the sound's side of the loop).
fn print_lines(lines: Vec<String>) {
    if !lines.is_empty() {
        ensure_app(|a| lines.into_iter().for_each(|l| a.console.println(l)));
    }
}

/// 2026's `_snd_mixahead`, adapted: host plumbing around id's mixer, not a
/// change to it ([`Mixer::samples_ahead`] is untouched; this only picks the
/// value [`Audio::mix`] hands it for [`SoundMode::Modern`] — Classic's lead
/// is id's fixed 0.1 s, always, [`SoundMode::mixahead`]).
///
/// The worker that runs the program is the same one that mixes: a host
/// frame busy rendering cannot answer the page's `AudioWake` either, so a
/// frame that takes `host_elapsed_s` leaves the ring unfed for that whole
/// stretch, and the newly mixed PCM that finally ends the stall is itself
/// painted from a clock reading taken *before* it (the device's position at
/// the top of the frame, like id's own `S_Update_`) — so by the time it
/// lands, that same `host_elapsed_s` has already passed again in real time.
/// A lead of [`MODERN_MIXAHEAD`] (50 ms) only outlasts frames up to about
/// that long; a slower one (a phone's slow pass underwater, a GC pause, a
/// tab stealing the core) runs it dry and the worklet plays silence —
/// `web/PLATFORM.md`, "Sound", measured it holding "at 30 fps and above".
///
/// Surviving a *run* of frames that long (not just one) needs the lead at
/// a little over twice the frame, not once: one to cover the stall just
/// measured, a second to leave over for the next one just like it (nothing
/// mixes again until this frame's samples are already delivered late), plus
/// a margin for the next one running a little longer than this one did —
/// without it, the lead would land exactly on the line and a sustained,
/// slightly uneven stall (never perfectly the same length twice) would keep
/// catching it there. `current` jumps up at once to `SAFETY` the frame just
/// seen (2.5x: 2x plus that margin) — at or above `current`, holding steady
/// rather than easing down the moment it catches up is what keeps a
/// *sustained* run from reopening the gap it just closed — clamped to
/// [`CEILING`] (comfortably short of the ring's cap — `RING_PAIRS` holds
/// 682 ms at 48 kHz, 743 ms at 44.1 kHz — so `samples_ahead` is never
/// clamped by it instead, which would quietly undersize the lead this
/// function asked for). Short of that, it eases back towards
/// `MODERN_MIXAHEAD` a fixed fraction of the gap a second (`dt_s` the
/// elapsed real time, so the rate is the same at 60 Hz or 480), so a fast
/// desktop's latency recovers within a few seconds of the game running well
/// again — reactive, not predictive: the first frame of a new stall still
/// glitches once, as nothing can see it coming, and a *sustained* stall
/// longer than `CEILING / SAFETY` (220 ms/frame — under 4.5 fps, worse
/// than anything measured so far) still underruns periodically, bounded by
/// how far past it the frame runs — there is only so much a bounded ring
/// can buy.
fn adapt_modern_ahead(current: f32, host_elapsed_s: f32, dt_s: f32) -> f32 {
    /// However long the slow frame just measured was, mix this many times
    /// that far ahead: one to cover it, one more so a repeat of it lands
    /// inside the buffer this bought rather than draining it to nothing
    /// again (see the function's docs for the derivation).
    const SAFETY: f32 = 2.5;
    /// Never past this: comfortably under the ring's cap (`RING_PAIRS` /
    /// the device rate — 682 ms at 48 kHz, 743 ms at 44.1 kHz).
    const CEILING: f32 = 0.55;
    /// Fraction of the gap above `MODERN_MIXAHEAD` that remains after a
    /// second of quick frames (continuous decay, so splitting a second into
    /// more or fewer frames gives the same result): a 350 ms lead is back
    /// under 75 ms within a second.
    const DECAY_PER_SECOND: f32 = 0.07;

    let wanted = (host_elapsed_s * SAFETY).clamp(MODERN_MIXAHEAD, CEILING);
    if wanted >= current {
        // At or above: jump up, or (`==`) hold — a *sustained* run of
        // frames this long must not ease down the moment it has just
        // caught up, or the next one like it reopens the gap this bought.
        wanted
    } else {
        let decay = DECAY_PER_SECOND.powf(dt_s.clamp(0.0, 1.0));
        MODERN_MIXAHEAD + (current - MODERN_MIXAHEAD) * decay
    }
}

/// (Tests.) The `S_StartSound` events waiting for the next frame, with the
/// view entity of their call.
#[cfg(test)]
pub(crate) fn pending_starts() -> Vec<(quake_rs::server::SoundEvent, i32)> {
    PENDING.with(|p| {
        let mut out = Vec::new();
        for r in p.borrow().iter() {
            if let Request::Client(SoundCall::Start { events, view_entity }) = r {
                out.extend(events.iter().map(|e| (e.clone(), *view_entity)));
            }
        }
        out
    })
}

/// (Tests.) The `S_StaticSound`s waiting for the next frame.
#[cfg(test)]
pub(crate) fn pending_statics() -> Vec<StaticSound> {
    PENDING.with(|p| {
        let mut out = Vec::new();
        for r in p.borrow().iter() {
            if let Request::Client(SoundCall::Static(s)) = r {
                out.extend(s.iter().cloned());
            }
        }
        out
    })
}

/// (Tests.) Forget the calls waiting for the next frame.
#[cfg(test)]
pub(crate) fn clear_pending() {
    PENDING.with(|p| p.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::bsp::Bsp;
    use quake_rs::progs::Progs;
    use quake_rs::server::Server;

    use crate::app::{boot, boot_attract, boot_demo};
    use crate::host::step;
    use crate::menu::menu_down;
    use crate::test_util::{close_menu, walk_mut};

    /// The 2026 mixer (the tests start in the Classic profile).
    fn modern() {
        crate::host_cmd::execute_console_command("snd_modern 1");
    }

    /// Split `snd_stats`'s text into (name, value) pairs.
    fn stats(audio: &Audio) -> std::collections::HashMap<String, String> {
        let (_, text) = audio.call("snd_stats").expect("a sound call");
        text.split_whitespace()
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect()
    }

    /// Frames of `dt` on the page's clock, as the page sends it: the device
    /// played `dt` of the mixer's rate before each.
    fn run_frames(audio: &mut Audio, frames: usize, dt: f64) -> Vec<i16> {
        let mut out = Vec::new();
        for _ in 0..frames {
            step(dt as f32);
            if let Some(pcm) = audio.frame(dt, None) {
                out.extend_from_slice(pcm.samples);
            }
        }
        out
    }

    /// Frames of `dt`, each measured as having taken `host_elapsed` of real
    /// time to compute — [`adapt_modern_ahead`]'s input, as `Sys::frame`
    /// feeds it from an `Instant` around `step`.
    fn run_frames_timed(audio: &mut Audio, frames: usize, dt: f64, host_elapsed: f64) -> Vec<i16> {
        let mut out = Vec::new();
        for _ in 0..frames {
            step(dt as f32);
            if let Some(pcm) = audio.frame(dt, Some(host_elapsed)) {
                out.extend_from_slice(pcm.samples);
            }
        }
        out
    }

    #[test]
    fn a_level_load_hands_the_mixer_its_placed_loops_and_the_mixer_plays_them() {
        clear_pending();
        assert_eq!(boot(), 1);
        close_menu();
        modern();
        let statics = pending_statics();
        assert!(statics.len() >= 5, "e1m1's placed loops wait for the mixer: {}", statics.len());
        let mut audio = Audio::new();
        let pcm = run_frames(&mut audio, 72, 1.0 / 72.0);
        let s = stats(&audio);
        assert_eq!(s["statics"], statics.len().to_string(), "every one registered");
        assert_eq!(s["rate"], "48000", "the 2026 mixer at an unknown device's 48 kHz");
        assert!(pcm.iter().any(|&v| v != 0), "the hums and ambients are painted");
        // The device's second from the first frame on, plus the mix-ahead.
        let want = 48000 * 71 / 72 + (MODERN_MIXAHEAD as f64 * 48000.0) as usize;
        assert!((pcm.len() / 2).abs_diff(want) <= 2, "{} pairs for {want}", pcm.len() / 2);
    }

    #[test]
    fn the_page_clock_drives_the_mix_and_an_overtaken_mixer_skips_ahead() {
        assert_eq!(boot(), 1);
        close_menu();
        modern();
        let mut audio = Audio::new();
        audio.clock(1000);
        let first = audio.frame(0.0, None).map(|p| (p.start, p.samples.len() / 2)).unwrap();
        assert_eq!(first, (1000, 2400), "from the page's position, 50 ms ahead of it");
        audio.clock(1000 + 800);
        let next = audio.frame(0.0, None).map(|p| (p.start, p.samples.len() / 2)).unwrap();
        assert_eq!(next, (3400, 800), "the device played 800 pairs: 800 more");
        audio.clock(1000 + 800 + 10_000);
        let late = audio.frame(0.0, None).map(|p| (p.start, p.samples.len() / 2)).unwrap();
        assert_eq!(late, (11_800, 2400), "overtaken: skip to the device, then 50 ms again");
    }

    #[test]
    fn stop_all_clears_the_ring_and_forgets_the_levels_loops() {
        assert_eq!(boot(), 1);
        close_menu();
        let mut audio = Audio::new();
        let first = audio.frame(1.0 / 72.0, None).map(|p| p.clear).unwrap();
        assert!(first, "a new mixer: S_Init's S_StopAllSounds (true)");
        assert!(!audio.frame(1.0 / 72.0, None).unwrap().clear);
        assert_eq!(boot_demo(), 1);
        let pcm = audio.frame(1.0 / 72.0, None).unwrap();
        assert!(pcm.clear, "the demo's S_StopAllSounds clears what was mixed ahead");
        assert_eq!(stats(&audio)["clears"], "2");
        assert!(sound_generation() >= 2, "the automation's generation counts them");
    }

    #[test]
    fn classic_mixes_ids_mixer_at_11025_and_a_new_mode_keeps_the_levels_loops() {
        assert_eq!(boot(), 1);
        close_menu();
        modern();
        let mut audio = Audio::new();
        audio.device(true, 44100);
        run_frames(&mut audio, 10, 1.0 / 72.0);
        let s = stats(&audio);
        assert_eq!((s["rate"].as_str(), s["mode"].as_str()), ("44100", "2026"), "the device's rate");
        let statics = s["statics"].clone();
        crate::host_cmd::execute_console_command("snd_modern 0");
        let pcm = audio.frame(1.0 / 72.0, None).map(|p| (p.rate, p.clear)).unwrap();
        assert_eq!(pcm, (11025, true), "id's rate; the ring cleared for the new mixer");
        assert_eq!(sound_mode(), SoundMode::Classic, "snd_modern 0 is the Classic mixer");
        let s = stats(&audio);
        assert_eq!((s["mode"].as_str(), s["statics"].clone()), ("classic", statics), "the loops came along");
        crate::host_cmd::execute_console_command("snd_modern 1");
    }

    #[test]
    fn the_attract_demos_sounds_start_in_the_mixer_before_any_click() {
        clear_pending();
        crate::vid::set_resolution(320, 200);
        assert_eq!(boot_attract(), 1);
        let mut audio = Audio::new();
        // No page, no clock: the loop's frame times move the device.
        run_frames(&mut audio, 72 * 8, 1.0 / 72.0);
        let s = stats(&audio);
        assert!(s["starts"].parse::<u32>().unwrap() > 0, "demo1's recorded sounds: {s:?}");
        let (n, channels) = audio.call("snd_channels").unwrap();
        assert!(n > 0.0 && channels.lines().count() as f64 == n, "{channels}");
    }

    #[test]
    fn menu_clicks_and_play_go_through_the_mixer_centred_at_full_volume() {
        assert_eq!(boot(), 1);
        let mut audio = Audio::new();
        run_frames(&mut audio, 2, 1.0 / 72.0);
        let before: u32 = stats(&audio)["local"].parse().unwrap();
        menu_down();
        s_play(&["items/r_item1"]);
        audio.frame(1.0 / 72.0, None);
        let s = stats(&audio);
        assert_eq!(s["local"], (before + 1).to_string(), "menu1.wav for the cursor");
        let (_, channels) = audio.call("snd_channels").unwrap();
        let line = |sample: &str| channels.lines().find(|l| l.contains(sample)).map(str::to_string);
        let click = line("misc/menu1.wav").expect("the click is on a channel");
        let item = line("items/r_item1.wav").expect("`play`'s sample is on a channel");
        for l in [click, item] {
            let f: Vec<&str> = l.split(' ').collect();
            assert_eq!((f[2], f[3]), ("255", "255"), "centred, full: {l}");
        }
    }

    #[test]
    fn neither_the_menu_nor_pause_stops_the_sound() {
        // id's S_Update runs every host frame whatever key_dest is and
        // whether the server is paused: under the menu (which stops a
        // single-player game) and over `pause`, the torches, hums and
        // ambients play on. The listener stands on one of e1m1's hums.
        clear_pending();
        assert_eq!(boot(), 1);
        let hum = pending_statics()[0].origin;
        crate::host_cmd::execute_console_command("noclip");
        walk_mut(|w| w.server.vm.ent_set_vector(w.player, "origin", hum));
        let mut audio = Audio::new();
        // Placed loops (channels 12 on) with a volume.
        let sounding = |audio: &Audio| {
            let (_, text) = audio.call("snd_channels").unwrap();
            text.lines()
                .map(|l| l.split(' ').map(|f| f.parse::<i64>().unwrap_or(0)).collect::<Vec<_>>())
                .filter(|f| f[0] >= 12 && (f[2] > 0 || f[3] > 0))
                .count()
        };
        run_frames(&mut audio, 36, 1.0 / 72.0);
        assert_eq!(crate::menu::menu_visible(), 1, "boot opens the menu over e1m1");
        assert!(sounding(&audio) > 0, "the hum sounds under the menu");
        close_menu();
        crate::host_cmd::execute_console_command("pause");
        assert!(walk_mut(|w| w.server.paused));
        let before = stats(&audio)["painted"].parse::<u32>().unwrap();
        let pcm = run_frames(&mut audio, 36, 1.0 / 72.0);
        assert!(sounding(&audio) > 0 && pcm.iter().any(|&v| v != 0), "and over a paused game");
        assert!(stats(&audio)["painted"].parse::<u32>().unwrap() > before);
    }

    // The level data the mixer is handed: what the server registers and what
    // the leafs carry (checked on the shareware maps).

    /// Spawn a real map's entities on a live server and return the static
    /// sounds its QuakeC registered (ambientsound() during spawn).
    fn spawn_map_statics(map: &str) -> Vec<StaticSound> {
        let pak = pak().expect("embedded pak");
        let read = |n: &str| pak.read_file(n).ok().flatten();
        let bsp = Bsp::parse(&read(map).expect("bsp")).expect("parse");
        let progs = Progs::parse(&read("progs.dat").expect("progs")).expect("parse");
        let mut server = Server::with_pak(bsp, progs, Some(pak.clone())).expect("server");
        let _ = server.drain_static_sounds();
        server.spawn_entities().expect("spawn");
        let statics = server.drain_static_sounds();
        // The registry drains once: a second drain is empty.
        assert!(server.drain_static_sounds().is_empty());
        statics
    }

    #[test]
    fn e1m2_spawn_registers_fire_torch_static_sounds() {
        // Ground truth: the medieval maps' wall torches run QuakeC's
        // FireAmbient — `ambientsound(self.origin, "ambience/fire1.wav", 0.5,
        // ATTN_STATIC)` — during spawn. e1m2 places 24 of them.
        let statics = spawn_map_statics("maps/e1m2.bsp");
        let fires: Vec<&StaticSound> = statics
            .iter()
            .filter(|s| s.sample == "ambience/fire1.wav")
            .collect();
        assert!(
            fires.len() >= 20,
            "e1m2's torches register ambience/fire1.wav loops (got {})",
            fires.len()
        );
        for f in &fires {
            // FireAmbient's exact arguments through the wire bytes:
            // vol 0.5 -> 127/255, ATTN_STATIC 3 -> 192/64 = 3.0.
            assert_eq!(f.volume, 127.0 / 255.0, "torch volume 0.5 (quantized)");
            assert_eq!(f.attenuation, 3.0, "ATTN_STATIC");
            assert!(
                f.origin.iter().all(|c| c.abs() < 10000.0),
                "plausible world position {:?}",
                f.origin
            );
            assert!(f.sound_index >= 1, "fire1.wav resolved to a precache slot");
        }
        // The torches sit at DISTINCT places (each wall torch registers its own).
        let mut pts: Vec<[i32; 3]> = fires
            .iter()
            .map(|f| [f.origin[0] as i32, f.origin[1] as i32, f.origin[2] as i32])
            .collect();
        pts.sort_unstable();
        pts.dedup();
        assert!(
            pts.len() >= 20,
            "torch loops are at distinct world positions (got {})",
            pts.len()
        );
    }

    #[test]
    fn e1m1_spawn_registers_base_ambience_static_sounds() {
        // e1m1 is a BASE map: no torches, but its light_fluoro fixtures hum
        // (ambience/fl_hum1.wav) and its computers drone (ambience/comp1.wav),
        // all at ATTN_STATIC — the level's actual placed soundscape.
        let statics = spawn_map_statics("maps/e1m1.bsp");
        assert!(
            statics.iter().any(|s| s.sample == "ambience/fl_hum1.wav"),
            "fluorescent hum registered"
        );
        assert!(
            statics.iter().any(|s| s.sample == "ambience/comp1.wav"),
            "computer drone registered"
        );
        assert!(statics.len() >= 10, "a full soundscape (got {})", statics.len());
        for s in &statics {
            assert_eq!(s.attenuation, 3.0, "every e1m1 ambient is ATTN_STATIC");
            assert!(s.volume > 0.0 && s.volume <= 1.0);
        }
    }

    #[test]
    fn e1m1_leafs_carry_ambient_levels() {
        // The dleaf_t ambient_level[NUM_AMBIENTS] bytes must survive the real
        // map's parse: e1m1 has both water (slime pools) and open-sky areas, so
        // SOME leafs carry non-zero water and sky levels for
        // S_UpdateAmbientSounds to ramp toward.
        let pak = pak().expect("embedded pak");
        let bsp = Bsp::parse(
            &pak.read_file("maps/e1m1.bsp").ok().flatten().expect("bsp"),
        )
        .expect("parse");
        let water = bsp
            .leafs
            .iter()
            .filter(|l| l.ambient_level[quake_rs::snd::AMBIENT_WATER] > 0)
            .count();
        let sky = bsp
            .leafs
            .iter()
            .filter(|l| l.ambient_level[quake_rs::snd::AMBIENT_SKY] > 0)
            .count();
        assert!(water > 0, "some e1m1 leafs hear water ambience");
        assert!(sky > 0, "some e1m1 leafs hear sky/wind ambience");
    }

    // --- adapt_modern_ahead: the late-frame fix ---------------------------

    #[test]
    fn a_slow_frame_grows_the_lead_at_once_and_a_quick_one_does_not() {
        // A 150 ms frame (a phone's slow underwater pass) jumps the lead to
        // 2.5x it, clamped to the floor and the ceiling.
        let grown = adapt_modern_ahead(MODERN_MIXAHEAD, 0.150, 1.0 / 72.0);
        assert!((grown - 0.375).abs() < 1e-5, "{grown}");
        // A frame well inside the current lead does not grow it.
        assert!(adapt_modern_ahead(grown, 0.010, 1.0 / 72.0) <= grown);
        // However long the stall, the lead never passes the ring's cap
        // (`RING_PAIRS` / 48 kHz = 682 ms; 44.1 kHz = 743 ms).
        assert_eq!(adapt_modern_ahead(MODERN_MIXAHEAD, 10.0, 1.0 / 72.0), 0.55);
        // However short the frame, the lead never drops below the baseline.
        assert_eq!(adapt_modern_ahead(MODERN_MIXAHEAD, 0.0, 1.0 / 72.0), MODERN_MIXAHEAD);
    }

    #[test]
    fn a_sustained_stall_at_the_same_level_holds_steady_instead_of_oscillating() {
        // The bug this guards: `wanted > current` (strict) eased the lead
        // back down the instant it caught up to a sustained stall, so the
        // very next frame just like it found it under-grown again — forever
        // (caught by `a_sustained_stall_at_the_derived_safe_limit_never_
        // gaps_once_converged` actually running the ring dry in headless
        // Chromium before this fix, not by this pure-function test alone).
        // Once the lead matches `SAFETY * s`, a run of more frames taking
        // exactly `s` must hold it there, not ease it down and jump back up
        // every other frame.
        let s = 0.15;
        let mut ahead = adapt_modern_ahead(MODERN_MIXAHEAD, s, s);
        let target = ahead;
        for _ in 0..20 {
            ahead = adapt_modern_ahead(ahead, s, s);
            assert_eq!(ahead, target, "holds steady, not oscillating, at a sustained {s}s/frame");
        }
    }

    #[test]
    fn the_grown_lead_eases_back_to_the_floor_over_quick_seconds() {
        let mut ahead = adapt_modern_ahead(MODERN_MIXAHEAD, 0.150, 0.0);
        assert!((ahead - 0.375).abs() < 1e-5, "the jump: {ahead}");
        let jump = ahead;
        for _ in 0..72 {
            ahead = adapt_modern_ahead(ahead, 0.0, 1.0 / 72.0);
        }
        assert!((ahead - (MODERN_MIXAHEAD + (jump - MODERN_MIXAHEAD) * 0.07)).abs() < 1e-5,
            "one second of quick frames sheds 93% of the gap: {ahead}");
        for _ in 0..72 * 4 {
            ahead = adapt_modern_ahead(ahead, 0.0, 1.0 / 72.0);
        }
        assert!(ahead - MODERN_MIXAHEAD < 0.0005, "back near the floor within a few seconds: {ahead}");
    }

    /// `samples_ahead`'s own bookkeeping (`quake-rs/src/snd/dma.rs`), without
    /// a live `Mixer`: each cycle paints up to `clock + ahead`, but
    /// `painted_time` only ever grows (`Mixer::paint` advances it by
    /// `samples_ahead`'s return, which is 0 — not negative — once a lower
    /// target falls under the high-water mark a bigger lead already bought).
    /// Simulates a host frame that always takes exactly `s`, forever, and
    /// checks the real condition for no underrun: the buffer already built
    /// *before* a cycle starts must reach past that cycle's *own* clock plus
    /// its own `s` — it is the only buffer there is for the ring to draw on
    /// while that cycle's stall runs, since nothing mixes again until it
    /// ends. This is why `adapt_modern_ahead`'s docs derive a lead of a
    /// little over *twice* the frame, not once. Returns how many cycles from
    /// `warmup` on failed that.
    fn simulate_sustained_stall_gaps(s: f32, cycles: u32, warmup: u32) -> u32 {
        let (mut ahead, mut clock, mut painted, mut gaps) = (MODERN_MIXAHEAD, 0.0_f32, 0.0_f32, 0);
        for cycle in 0..cycles {
            let underran_this_cycle = painted < clock + s;
            ahead = adapt_modern_ahead(ahead, s, s);
            painted = painted.max(clock + ahead);
            clock += s;
            if cycle >= warmup && underran_this_cycle {
                gaps += 1;
            }
        }
        gaps
    }

    #[test]
    fn a_sustained_stall_at_the_derived_safe_limit_never_gaps_once_converged() {
        // Comfortably under CEILING / SAFETY (0.55 / 2.5 = 0.22 s): the lead
        // converges to a stable `SAFETY * s` and every later cycle's
        // already-built buffer reaches past that cycle's own stall.
        assert_eq!(simulate_sustained_stall_gaps(0.15, 200, 5), 0, "150 ms/frame, forever: no gap once converged");
        assert_eq!(simulate_sustained_stall_gaps(0.18, 200, 5), 0, "180 ms/frame: still comfortably covered");
        assert_eq!(simulate_sustained_stall_gaps(0.05, 200, 5), 0, "50 ms/frame: comfortably covered");
    }

    #[test]
    fn a_sustained_stall_past_the_derived_limit_keeps_gapping_by_a_bounded_amount() {
        // Past CEILING / SAFETY (~227 ms/frame, under 4.5 fps): the ring
        // (RING_PAIRS: 682 ms at 48 kHz) is not big enough to buy a lead of
        // twice a frame this long forever, so a *perfectly steady* stall
        // this far past it gaps every cycle once the lead settles at the
        // ceiling — a real architectural limit, not a bug (a real, jittery
        // stall this severe gaps less often, as some frames run under the
        // local average; `web/PLATFORM.md`, "Sound").
        let gaps_50 = simulate_sustained_stall_gaps(0.3, 50, 5);
        assert_eq!(gaps_50, 45, "every cycle past warmup gaps at this sustained a stall");
        // Bounded, not runaway: twice the cycles gaps (about) twice as
        // often, not faster — each cycle's shortfall is the same fixed
        // amount (CEILING short of `2 * s`), never compounding.
        let gaps_400 = simulate_sustained_stall_gaps(0.3, 400, 5);
        assert_eq!(gaps_400, 395, "one more gap a cycle, the whole way: no runaway");
    }

    #[test]
    fn a_run_of_slow_frames_grows_the_mixers_own_mixahead_and_classic_never_moves() {
        assert_eq!(boot(), 1);
        close_menu();
        modern();
        let mut audio = Audio::new();
        audio.device(true, 48000);
        // A steady run of 150 ms frames, as a sustained slow pass underwater
        // would be: the lead converges above the floor.
        run_frames_timed(&mut audio, 30, 1.0 / 72.0, 0.150);
        let grown = audio.mixer.as_ref().unwrap().cvars.mixahead;
        assert!(grown > MODERN_MIXAHEAD * 2.0, "grown past the baseline: {grown}");
        assert!(grown <= 0.55, "never past the ceiling: {grown}");
        assert_eq!(stats(&audio)["mixahead_ms"], format!("{:.1}", grown * 1000.0), "snd_stats shows it");
        // Quick frames again: it eases back down.
        run_frames_timed(&mut audio, 72 * 5, 1.0 / 72.0, 0.0);
        let eased = audio.mixer.as_ref().unwrap().cvars.mixahead;
        assert!(eased < grown, "eased back down: {grown} -> {eased}");
        assert!(eased - MODERN_MIXAHEAD < 0.002, "close to the floor after 5 quick seconds: {eased}");

        // Classic's `_snd_mixahead` is id's fixed 0.1 s, whatever the host
        // frame took — never adapted.
        crate::host_cmd::execute_console_command("snd_modern 0");
        run_frames_timed(&mut audio, 10, 1.0 / 72.0, 0.300);
        assert_eq!(audio.mixer.as_ref().unwrap().cvars.mixahead, 0.1, "Classic is untouched by slow frames");
        crate::host_cmd::execute_console_command("snd_modern 1");
    }
}
