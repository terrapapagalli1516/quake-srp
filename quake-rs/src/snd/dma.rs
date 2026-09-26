//! The mixer's control, ported from WinQuake's `snd_dma.c`: the channel
//! table and what fills it — `S_StartSound` with `SND_PickChannel` and
//! `SND_Spatialize`, `S_StaticSound`, `S_StopSound`, `S_StopAllSounds`,
//! `S_LocalSound` — and `S_Update`: the leaf ambients
//! (`S_UpdateAmbientSounds`), every channel re-spatialized, the static
//! sounds of one sample combined. The painting is [`super::mix`]'s.
//!
//! The C keeps all of this in globals (`channels[]`, `paintedtime`, the
//! listener, the cvars); here it is one value, the [`Mixer`], owned by
//! whoever owns the client session. The client hands it the calls a frame
//! made ([`Mixer::run`] over [`SoundCall`]s); the platform asks it for PCM
//! ([`Mixer::paint`]) as its device plays, the way `S_Update_` mixed ahead
//! of the DMA position ([`Mixer::samples_ahead`]).
//!
//! **Float precision.** id's WinQuake ran this code on the x87 FPU at full
//! (64-bit mantissa) precision, and `SND_Spatialize`'s volumes are
//! truncated to ints, so where the C keeps a value in a register and where
//! it stores it as a `float` decides some volumes by one step. The port
//! follows the C as id's gcc-built oracle compiles it: `f64` where the x87
//! keeps extended precision, `f32` where it stores a `float` (see
//! [`normalize_x87`] and [`Mixer::spatialize`]).
//!
//! **Classic and the fixes.** With [`Fixes::NONE`] the mixer is id's,
//! sample for sample (`oracle/sound.py` checks it against id's C). The
//! default, [`Fixes::ALL`], repairs four things id's code gets wrong; each
//! [`Fixes`] field says what and why.

use super::mem::{LoadOptions, SfxId, SfxTable};
use super::mix::PaintState;
use crate::bsp::NUM_AMBIENTS;
use crate::client::{Listener, SoundCall};
use crate::pak::Pak;
use crate::server::{SoundEvent, StaticSound};

/// `MAX_CHANNELS` (sound.h): the whole channel table — the ambients, the
/// dynamic channels, then the static sounds.
pub const MAX_CHANNELS: usize = 128;

/// `MAX_DYNAMIC_CHANNELS` (sound.h): the channels `S_StartSound` picks from,
/// right after the [`NUM_AMBIENTS`] ambient ones.
pub const MAX_DYNAMIC_CHANNELS: usize = 8;

/// The first static-sound channel: `S_StopAllSounds` resets
/// `total_channels` here.
const FIRST_STATIC: usize = NUM_AMBIENTS + MAX_DYNAMIC_CHANNELS;

/// `sound_nominal_clip_dist` (snd_dma.c): an attenuation of 1 falls silent
/// at 1000 units.
const SOUND_NOMINAL_CLIP_DIST: f32 = 1000.0;

/// Ambient channel indices (`bspfile.h`). WinQuake only ever loads sounds for
/// the first two (`S_Init`: `ambience/water1.wav` + `ambience/wind2.wav`);
/// slime and lava exist in the leaf data but have a NULL `ambient_sfx` and so
/// never sound.
pub const AMBIENT_WATER: usize = 0;
/// See [`AMBIENT_WATER`].
pub const AMBIENT_SKY: usize = 1;

/// The pak samples behind the audible ambient channels (`S_Init`,
/// snd_dma.c:237-238), indexed by channel. `None` = the C left
/// `ambient_sfx[ch]` NULL (slime/lava), so the channel stays silent forever.
pub const AMBIENT_SAMPLES: [Option<&str>; NUM_AMBIENTS] = [
    Some("ambience/water1.wav"), // AMBIENT_WATER
    Some("ambience/wind2.wav"),  // AMBIENT_SKY
    None,                        // AMBIENT_SLIME (never loaded by WinQuake)
    None,                        // AMBIENT_LAVA  (never loaded by WinQuake)
];

/// Default of the `ambient_level` cvar (snd_dma.c:78): scales the leaf's
/// `ambient_level[]` byte into the channel's target master volume.
pub const AMBIENT_LEVEL_DEFAULT: f32 = 0.3;
/// Default of the `ambient_fade` cvar (snd_dma.c:79): master-volume units
/// (0..255 scale) per second the channel may move toward its target.
pub const AMBIENT_FADE_DEFAULT: f32 = 100.0;

/// One ambient ramp step under [`Fixes::ambient_steps`]: a host frame at
/// id's 72 fps cap (`Host_FilterTime`).
const AMBIENT_STEP: f64 = 1.0 / 72.0;

/// The most time one call ramps the ambients under
/// [`Fixes::ambient_steps`]: `Host_FilterTime`'s 0.1 s frame clamp, so a
/// stall (a background tab) does not fast-forward the ramp.
const AMBIENT_MAX_FRAME: f64 = 0.1;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// What the default mixer repairs in id's. Classic is [`Fixes::NONE`]; the
/// 2026 default is [`Fixes::ALL`]. None of them changes the character of the
/// sound: the samples stay 8-bit and point-resampled, the spatialization and
/// attenuation stay id's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixes {
    /// Paint what follows a loop restart where it belongs in the paint
    /// buffer. id's `SND_PaintChannelFrom8` always writes from
    /// `paintbuffer[0]`, so in `S_PaintChannels` the samples after a restart
    /// land on top of the chunk's start, and the chunk's rest gets nothing
    /// from that channel: a click on every lap of an ambient, a torch or a
    /// door's hum. (Quake II passes the offset.)
    pub loop_seam: bool,
    /// Resample in exact steps. id's `ResampleSfx` steps in 8.8 fixed point,
    /// exact at 11025, 22050 and 44100 Hz; at 48000 every sound plays 1.4%
    /// flat and loses its end (see [`super::mem`]).
    pub exact_resample: bool,
    /// Ramp the ambients in fixed 1/72 s steps. `S_UpdateAmbientSounds` moves
    /// the int `master_vol` by `host_frametime * ambient_fade` once a frame,
    /// truncated: above about 100 fps the step is under 1 and the water and
    /// wind never fade in. The fixed steps are id's ramp at its 72 fps cap,
    /// at any frame rate.
    pub ambient_steps: bool,
    /// `S_StopSound` searches the eight dynamic channels. id's loop runs over
    /// channels 0..8: the four ambients and the first four dynamic channels,
    /// so a sound on the last four cannot be stopped.
    pub stop_range: bool,
}

impl Fixes {
    /// id's mixer as written: Classic.
    pub const NONE: Fixes = Fixes { loop_seam: false, exact_resample: false, ambient_steps: false, stop_range: false };
    /// Every fix: the 2026 default.
    pub const ALL: Fixes = Fixes { loop_seam: true, exact_resample: true, ambient_steps: true, stop_range: true };
}

impl Default for Fixes {
    fn default() -> Self {
        Fixes::ALL
    }
}

/// Which mixer the player hears: the typed setting a platform (and the
/// settings' profiles) choose with. Each mode is a rate, a set of [`Fixes`]
/// and a mix-ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoundMode {
    /// id's mixer as WinQuake ran it: [`Fixes::NONE`], at id's
    /// `desired_speed` of 11025 Hz (the platform reconstructs that for its
    /// device, as a sound card's DAC and output filter did), mixing
    /// `_snd_mixahead`'s default 0.1 s ahead.
    Classic,
    /// The 2026 mixer: [`Fixes::ALL`], at the device's own rate (id's
    /// algorithms at `-sspeed` rate: point-resampled 8-bit samples, id's
    /// spatialization), mixing [`MODERN_MIXAHEAD`] ahead.
    #[default]
    Modern,
}

/// The 2026 mixer's `_snd_mixahead`: how far ahead of the device it mixes.
/// A sound starts this long after its frame at the least (plus the device's
/// own output latency). It must outlast the time between two host frames
/// plus the device's callback, or the device runs dry: 50 ms holds at 30 fps
/// and above (`web/verify_ambient.py` counts the device's underruns).
pub const MODERN_MIXAHEAD: f32 = 0.05;

/// id's `desired_speed` (snd_dma.c): the rate WinQuake asked its sound
/// device for.
pub const ID_RATE: u32 = 11025;

impl SoundMode {
    /// The fixes this mode mixes with.
    pub fn fixes(self) -> Fixes {
        match self {
            SoundMode::Classic => Fixes::NONE,
            SoundMode::Modern => Fixes::ALL,
        }
    }

    /// The rate to mix at for a device playing `device_rate` (0: unknown,
    /// taken as 48000).
    pub fn rate(self, device_rate: u32) -> u32 {
        match self {
            SoundMode::Classic => ID_RATE,
            SoundMode::Modern if device_rate == 0 => 48000,
            SoundMode::Modern => device_rate,
        }
    }

    /// `_snd_mixahead`, in seconds.
    pub fn mixahead(self) -> f32 {
        match self {
            SoundMode::Classic => 0.1,
            SoundMode::Modern => MODERN_MIXAHEAD,
        }
    }

    /// Its name, as a console or a menu prints it.
    pub fn name(self) -> &'static str {
        match self {
            SoundMode::Classic => "classic",
            SoundMode::Modern => "2026",
        }
    }

    /// The mode a console argument names (`classic`/`0`, `2026`/`1`).
    pub fn parse(s: &str) -> Option<SoundMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "classic" | "id" | "0" => Some(SoundMode::Classic),
            "2026" | "modern" | "1" => Some(SoundMode::Modern),
            _ => None,
        }
    }

    /// A mixer in this mode for a device at `device_rate`, its samples read
    /// from `pak`: `S_Init` with the mode's rate, fixes and `_snd_mixahead`.
    pub fn mixer(self, pak: &Pak, device_rate: u32) -> Mixer {
        let mut m = Mixer::new(pak, self.rate(device_rate), self.fixes());
        m.cvars.mixahead = self.mixahead();
        m
    }
}

/// `snd_dma.c`'s cvars that change what the mixer does (`bgmvolume`,
/// `bgmbuffer`, `precache` and `snd_show` have nothing to do here).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundCvars {
    /// `volume` (0.7): the master volume, applied as `volume * 256` in
    /// `S_TransferPaintBuffer`.
    pub volume: f32,
    /// `nosound` (0): `S_StartSound`, `S_LocalSound` and precaching do
    /// nothing. A level's static sounds that were registered before it was
    /// set play on, as in the C.
    pub nosound: bool,
    /// `loadas8bit` (0): load 16-bit samples as 8-bit. It applies to samples
    /// loaded after it changes, as in the C.
    pub loadas8bit: bool,
    /// `ambient_level` (0.3): see [`AMBIENT_LEVEL_DEFAULT`].
    pub ambient_level: f32,
    /// `ambient_fade` (100): see [`AMBIENT_FADE_DEFAULT`].
    pub ambient_fade: f32,
    /// `_snd_mixahead` (0.1): how far ahead of the device's play position
    /// [`Mixer::samples_ahead`] mixes, in seconds.
    pub mixahead: f32,
}

impl Default for SoundCvars {
    fn default() -> Self {
        SoundCvars {
            volume: 0.7,
            nosound: false,
            loadas8bit: false,
            ambient_level: AMBIENT_LEVEL_DEFAULT,
            ambient_fade: AMBIENT_FADE_DEFAULT,
            mixahead: 0.1,
        }
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// `channel_t` (sound.h): one sound playing, or a free slot (`sfx: None`).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Channel {
    /// The sample (`sfx`); `None` = free or stopped.
    pub(super) sfx: Option<SfxId>,
    /// 0..255 volume per side, after spatialization (static sounds combined
    /// in `S_Update` can sum past 255; the 8-bit painter clamps).
    pub(super) leftvol: i32,
    pub(super) rightvol: i32,
    /// When the sample runs out, in painted sample pairs.
    pub(super) end: i64,
    /// The next sample to paint.
    pub(super) pos: i32,
    /// The override key (`SND_PickChannel`).
    pub(super) entnum: i32,
    pub(super) entchannel: i32,
    /// Where the sound comes from.
    pub(super) origin: [f32; 3],
    /// `attenuation / sound_nominal_clip_dist`: 1.0 of distance is silence.
    pub(super) dist_mult: f32,
    /// 0..255 before spatialization.
    pub(super) master_vol: i32,
}

/// One sounding channel as [`Mixer::channels`] shows it: what the sound
/// oracle's trace prints for both mixers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelState<'a> {
    /// The channel's index in the table.
    pub index: usize,
    /// The sample's name, relative to `sound/`.
    pub sample: &'a str,
    pub leftvol: i32,
    pub rightvol: i32,
    pub master_vol: i32,
    pub pos: i32,
    pub end: i64,
    pub entnum: i32,
    pub entchannel: i32,
}

/// The C runtime's `rand()` as WinQuake.exe had it, Microsoft's
/// (`holdrand * 214013 + 2531011`, bits 16..30), seeded 1 as a program that
/// never calls `srand`. id's `rand()` was one sequence for the whole engine;
/// the mixer, which draws only when one sound starts twice in a frame, has
/// its own.
#[derive(Debug, Clone)]
struct CRand(u32);

impl CRand {
    fn next(&mut self) -> i32 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((self.0 >> 16) & 0x7fff) as i32
    }
}

// ---------------------------------------------------------------------------
// The mixer
// ---------------------------------------------------------------------------

/// id's sound engine: `snd_dma.c`'s channel table and listener, `snd_mix.c`'s
/// paint buffer, `snd_mem.c`'s samples. See the module note.
pub struct Mixer {
    /// The cvars, read at each use.
    pub cvars: SoundCvars,
    pub(super) fixes: Fixes,
    /// The output rate (`shm->speed`).
    pub(super) speed: i32,
    pub(super) sfx: SfxTable,
    ambient_sfx: [Option<SfxId>; NUM_AMBIENTS],
    /// `channels[MAX_CHANNELS]`: the ambients, the dynamic channels, then the
    /// static sounds up to `total_channels`.
    pub(super) channels: Vec<Channel>,
    pub(super) total_channels: usize,
    /// Sample pairs painted so far (`paintedtime`); the C's is an `int` that
    /// `GetSoundtime` chops back after 2^30, this one never wraps.
    pub(super) paintedtime: i64,
    listener_origin: [f32; 3],
    listener_right: [f32; 3],
    /// `cl.viewentity`: its sounds play at full volume, unpanned.
    view_entity: i32,
    rand: CRand,
    pub(super) paint: PaintState,
    /// Seconds not yet ramped under [`Fixes::ambient_steps`].
    ambient_clock: f64,
    /// `S_StopAllSounds (true)`'s `S_ClearBuffer` is due: the platform's
    /// output buffer, whose mixed-ahead samples should fall silent
    /// ([`Mixer::take_clear`]).
    clear: bool,
    /// `S_Play`'s `static int hash`: the entity number of the next `play`.
    play_hash: i32,
}

impl Mixer {
    /// `S_Init` + `S_Startup` for a device at `rate` Hz: the scale table, the
    /// two ambient loops loaded from `pak`, every channel clear.
    pub fn new(pak: &Pak, rate: u32, fixes: Fixes) -> Mixer {
        let mut m = Mixer {
            cvars: SoundCvars::default(),
            fixes,
            speed: i32::try_from(rate.max(1)).unwrap_or(i32::MAX),
            sfx: SfxTable::default(),
            ambient_sfx: [None; NUM_AMBIENTS],
            channels: vec![Channel::default(); MAX_CHANNELS],
            total_channels: FIRST_STATIC,
            paintedtime: 0,
            listener_origin: [0.0; 3],
            listener_right: [0.0; 3],
            view_entity: 0,
            rand: CRand(1),
            paint: PaintState::new(),
            ambient_clock: 0.0,
            clear: false,
            play_hash: 345,
        };
        for (ch, name) in AMBIENT_SAMPLES.iter().enumerate() {
            if let Some(name) = name {
                m.ambient_sfx[ch] = m.precache(pak, name);
            }
        }
        m.stop_all_sounds();
        m
    }

    /// The output rate, in sample pairs a second.
    pub fn rate(&self) -> u32 {
        self.speed as u32
    }

    /// Sample pairs painted so far.
    pub fn painted_time(&self) -> i64 {
        self.paintedtime
    }

    /// The channel table's used length (`total_channels`): the ambients, the
    /// dynamic channels, and every static sound registered since the last
    /// [`Mixer::stop_all_sounds`] (a failed one too).
    pub fn total_channels(&self) -> usize {
        self.total_channels
    }

    /// Carry out the calls a client frame or a level load made, in order
    /// (see [`SoundCall`]). `pak` is the client's; samples load from it on
    /// first use.
    pub fn run(&mut self, pak: &Pak, calls: &[SoundCall]) {
        for call in calls {
            match call {
                SoundCall::Start { events, view_entity } => {
                    self.view_entity = *view_entity;
                    for ev in events {
                        self.start_sound(pak, ev);
                    }
                }
                SoundCall::Stop(stops) => {
                    for &(entnum, entchannel) in stops {
                        self.stop_sound(entnum, entchannel);
                    }
                }
                SoundCall::StopAll => self.stop_all_sounds(),
                SoundCall::Static(statics) => {
                    for s in statics {
                        self.static_sound(pak, s);
                    }
                }
                SoundCall::Update { listener, leaf_ambient, frametime } => {
                    self.update(listener, *leaf_ambient, f64::from(*frametime));
                }
                // The CD plays beside the mix, never through it.
                SoundCall::Cd(_) => {}
            }
        }
    }

    /// `cl.viewentity`, whose sounds `SND_Spatialize` plays at full volume
    /// and `SND_PickChannel` protects from monsters. [`Mixer::run`] takes it
    /// from each [`SoundCall::Start`].
    pub fn set_view_entity(&mut self, entity: i32) {
        self.view_entity = entity;
    }

    /// `S_PrecacheSound`: the table entry for `name`, its sample loaded.
    /// `None` under `nosound`, as the C returns NULL.
    fn precache(&mut self, pak: &Pak, name: &str) -> Option<SfxId> {
        if self.cvars.nosound {
            return None;
        }
        let id = self.sfx.find_name(name)?;
        let opts = self.load_options();
        self.sfx.load(id, pak, opts);
        Some(id)
    }

    fn load_options(&self) -> LoadOptions {
        LoadOptions { speed: self.speed, loadas8bit: self.cvars.loadas8bit, exact: self.fixes.exact_resample }
    }

    // -----------------------------------------------------------------------
    // Starting and stopping
    // -----------------------------------------------------------------------

    /// `S_StartSound` for one event, as `CL_ParseStartSoundPacket` calls it:
    /// `ev.volume` is the C's `fvol` (0..1), `ev.attenuation` its
    /// `attenuation`. The sample is looked up (and loaded) by name, which is
    /// what the C's `cl.sound_precache[]` did at signon.
    pub fn start_sound(&mut self, pak: &Pak, ev: &SoundEvent) {
        if ev.sample.is_empty() || self.cvars.nosound {
            return;
        }
        let Some(sfx) = self.precache(pak, &ev.sample) else { return };
        // `vol = fvol*255`: the x87 multiplies exactly, then truncates.
        let vol = (f64::from(ev.volume) * 255.0) as i32;

        // pick a channel to play on
        let Some(t) = self.pick_channel(ev.entity, ev.channel) else { return };

        // spatialize
        self.channels[t] = Channel {
            origin: ev.origin,
            dist_mult: ev.attenuation / SOUND_NOMINAL_CLIP_DIST,
            master_vol: vol,
            entnum: ev.entity,
            entchannel: ev.channel,
            ..Channel::default()
        };
        self.spatialize(t);
        if self.channels[t].leftvol == 0 && self.channels[t].rightvol == 0 {
            return; // not audible at all
        }

        // new channel
        let opts = self.load_options();
        let Some(length) = self.sfx.load(sfx, pak, opts).map(|sc| sc.length) else {
            self.channels[t].sfx = None;
            return; // couldn't load the sound's data
        };
        let ch = &mut self.channels[t];
        ch.sfx = Some(sfx);
        ch.pos = 0;
        ch.end = self.paintedtime + i64::from(length);
        self.offset_repeat(t, sfx);
    }

    /// The end of `S_StartSound`: "if an identical sound has also been
    /// started this frame, offset the pos a bit to keep it from just making
    /// the first one louder" — by up to 0.1 s, drawn from `rand()`.
    fn offset_repeat(&mut self, t: usize, sfx: SfxId) {
        let twin = (NUM_AMBIENTS..FIRST_STATIC)
            .filter(|&i| i != t)
            .any(|i| self.channels[i].sfx == Some(sfx) && self.channels[i].pos == 0);
        if !twin {
            return;
        }
        let range = ((0.1 * f64::from(self.speed)) as i32).max(1);
        let mut skip = i64::from(self.rand.next() % range);
        let ch = &mut self.channels[t];
        // id compares with `end`, a time, where the length was meant; kept.
        if skip >= ch.end {
            skip = ch.end - 1;
        }
        ch.pos += skip as i32;
        ch.end -= skip;
    }

    /// `S_LocalSound`: `name` from the listener itself, on channel -1 (which
    /// overrides any of its sounds), at full volume.
    pub fn local_sound(&mut self, pak: &Pak, name: &str) {
        let ev = SoundEvent {
            entity: self.view_entity,
            channel: -1,
            sound_index: -1,
            sample: name.to_string(),
            origin: [0.0; 3],
            volume: 1.0,
            attenuation: 1.0,
        };
        self.start_sound(pak, &ev);
    }

    /// `S_Play` (the `play` command): `name` (`.wav` added when it has no
    /// extension) from the listener's position on a fresh entity number, at
    /// full volume and attenuation 1: centred and unattenuated there.
    pub fn play(&mut self, pak: &Pak, name: &str) {
        let sample = if name.contains('.') { name.to_string() } else { format!("{name}.wav") };
        let ev = SoundEvent {
            entity: self.play_hash,
            channel: 0,
            sound_index: -1,
            sample,
            origin: self.listener_origin,
            volume: 1.0,
            attenuation: 1.0,
        };
        self.play_hash += 1;
        self.start_sound(pak, &ev);
    }

    /// `SND_PickChannel`: the dynamic channel a new sound takes. The same
    /// entity's sound on the same channel is always replaced (channel 0
    /// never overrides; -1 matches any); otherwise the sound closest to its
    /// end goes, but a monster never cuts off the player's own.
    fn pick_channel(&mut self, entnum: i32, entchannel: i32) -> Option<usize> {
        let mut first_to_die = None;
        let mut life_left = i64::from(i32::MAX);
        for i in NUM_AMBIENTS..FIRST_STATIC {
            let ch = &self.channels[i];
            if entchannel != 0 && ch.entnum == entnum && (ch.entchannel == entchannel || entchannel == -1) {
                // always override sound from same entity
                first_to_die = Some(i);
                break;
            }
            // don't let monster sounds override player sounds
            if ch.entnum == self.view_entity && entnum != self.view_entity && ch.sfx.is_some() {
                continue;
            }
            if ch.end - self.paintedtime < life_left {
                life_left = ch.end - self.paintedtime;
                first_to_die = Some(i);
            }
        }
        let t = first_to_die?;
        self.channels[t].sfx = None;
        Some(t)
    }

    /// `S_StaticSound`, as `CL_ParseStaticSound` calls it: a looping sound
    /// at a point for the rest of the level. `s.volume` and `s.attenuation`
    /// are the wire bytes over 255 and 64 ([`StaticSound`]); the C gets the
    /// bytes. A slot is spent even when the sample then fails to load or has
    /// no loop point ("Sound %s not looped").
    pub fn static_sound(&mut self, pak: &Pak, s: &StaticSound) {
        if s.sample.is_empty() {
            return;
        }
        let Some(sfx) = self.precache(pak, &s.sample) else { return };
        if self.total_channels == MAX_CHANNELS {
            return; // "total_channels == MAX_CHANNELS"
        }
        let i = self.total_channels;
        self.total_channels += 1;

        let opts = self.load_options();
        let Some(sc) = self.sfx.load(sfx, pak, opts) else { return };
        if sc.loopstart.is_none() {
            return; // "Sound %s not looped"
        }
        let length = sc.length;
        let vol = (s.volume * 255.0).round() as i32;
        let atten = (s.attenuation * 64.0).round();
        let ch = &mut self.channels[i];
        ch.sfx = Some(sfx);
        ch.origin = s.origin;
        ch.master_vol = vol;
        ch.dist_mult = (atten / 64.0) / SOUND_NOMINAL_CLIP_DIST;
        ch.end = self.paintedtime + i64::from(length);
        self.spatialize(i);
    }

    /// `S_StopSound`: end the sound `entnum` plays on `entchannel`. id's
    /// search covers channels 0..8, the ambients among them;
    /// [`Fixes::stop_range`] searches the dynamic channels.
    pub fn stop_sound(&mut self, entnum: i32, entchannel: i32) {
        let range = if self.fixes.stop_range { NUM_AMBIENTS..FIRST_STATIC } else { 0..MAX_DYNAMIC_CHANNELS };
        if let Some(ch) = self.channels[range].iter_mut().find(|c| c.entnum == entnum && c.entchannel == entchannel) {
            ch.end = 0;
            ch.sfx = None;
        }
    }

    /// `S_StopAllSounds`: a level or mode change clears every channel, the
    /// static sounds and the ambients' ramps included. (`S_ClearBuffer`'s
    /// half, silencing what was already mixed ahead, is the platform's: the
    /// buffer is its.)
    pub fn stop_all_sounds(&mut self) {
        self.total_channels = FIRST_STATIC;
        self.channels.fill(Channel::default());
        self.ambient_clock = 0.0;
        self.clear = true;
    }

    /// Whether `S_StopAllSounds` has asked, since the last call, for what was
    /// already mixed ahead to be silenced (`S_ClearBuffer`: id zeroes its DMA
    /// buffer; here the buffer is the platform's).
    pub fn take_clear(&mut self) -> bool {
        std::mem::take(&mut self.clear)
    }

    // -----------------------------------------------------------------------
    // S_Update
    // -----------------------------------------------------------------------

    /// `S_Update`, the frame's half: the listener's pose, the leaf ambients
    /// ramped over `frametime` (`host_frametime`) toward `leaf` (the
    /// listener leaf's `ambient_sound_level[]`; `None` outside the world),
    /// every sounding channel re-spatialized, and the static sounds of one
    /// sample combined. The mixing that followed in the C (`S_Update_`) is
    /// [`Mixer::paint`], which the platform calls as its device needs.
    pub fn update(&mut self, listener: &Listener, leaf: Option<[u8; NUM_AMBIENTS]>, frametime: f64) {
        self.listener_origin = listener.pos;
        self.listener_right = listener.right;
        self.update_ambient_sounds(leaf, frametime);
        self.respatialize();
    }

    /// `S_UpdateAmbientSounds`: the water and wind channels follow the
    /// listener leaf's levels, `ambient_level` times the leaf's byte (silent
    /// under 8), moving at most `ambient_fade` a second. With no leaf, or
    /// `ambient_level` 0, they fall silent at once without losing their ramp.
    fn update_ambient_sounds(&mut self, leaf: Option<[u8; NUM_AMBIENTS]>, frametime: f64) {
        let level = f64::from(self.cvars.ambient_level);
        let Some(levels) = leaf.filter(|_| level != 0.0) else {
            for ch in &mut self.channels[..NUM_AMBIENTS] {
                ch.sfx = None;
            }
            return;
        };
        let (steps, step) = if self.fixes.ambient_steps {
            self.ambient_clock = (self.ambient_clock + frametime.max(0.0)).min(AMBIENT_MAX_FRAME);
            let n = (self.ambient_clock / AMBIENT_STEP) as u32;
            self.ambient_clock -= f64::from(n) * AMBIENT_STEP;
            (n, AMBIENT_STEP)
        } else {
            (1, frametime)
        };
        let delta = step * f64::from(self.cvars.ambient_fade);
        for (i, ch) in self.channels[..NUM_AMBIENTS].iter_mut().enumerate() {
            ch.sfx = self.ambient_sfx[i];
            let mut vol = level * f64::from(levels[i]);
            if vol < 8.0 {
                vol = 0.0;
            }
            for _ in 0..steps {
                ramp(&mut ch.master_vol, vol, delta);
            }
            ch.leftvol = ch.master_vol;
            ch.rightvol = ch.master_vol;
        }
    }

    /// `S_Update`'s loop: re-spatialize every dynamic and static channel, and
    /// "try to combine static sounds with a previous channel of the same
    /// sound effect so we don't mix five torches every frame": a static
    /// sound's volumes join the first earlier static of its sample (or the
    /// one it joined last), which then plays for both.
    fn respatialize(&mut self) {
        let mut combine: Option<usize> = None;
        for i in NUM_AMBIENTS..self.total_channels {
            let Some(sfx) = self.channels[i].sfx else { continue };
            self.spatialize(i);
            if self.channels[i].leftvol == 0 && self.channels[i].rightvol == 0 {
                continue;
            }
            if i < FIRST_STATIC {
                continue;
            }
            let into = match combine {
                // see if it can just use the last one
                Some(c) if self.channels[c].sfx == Some(sfx) => c,
                // search for one. (id's `j == total_channels` test after the
                // search cannot hold, so a static with no earlier twin
                // becomes the next one's candidate.)
                _ => {
                    let j = (FIRST_STATIC..i).find(|&j| self.channels[j].sfx == Some(sfx)).unwrap_or(i);
                    combine = Some(j);
                    j
                }
            };
            if into != i {
                let (l, r) = (self.channels[i].leftvol, self.channels[i].rightvol);
                self.channels[into].leftvol += l;
                self.channels[into].rightvol += r;
                self.channels[i].leftvol = 0;
                self.channels[i].rightvol = 0;
            }
        }
    }

    /// `SND_Spatialize`: channel `i`'s left and right volumes from its
    /// distance and direction. The view entity's own sounds play at full
    /// volume, unpanned. Otherwise the volume falls linearly to silence at
    /// `1 / dist_mult` units and pans by the dot product with the listener's
    /// right: `(1 - dist) * (1 ± dot) * master_vol`, up to twice the master
    /// volume on the near side.
    ///
    /// The precision is id's x87 build's: the source vector is stored as
    /// `float`s, the length, the dot product and the scale are carried in
    /// extended precision, each scale is stored as a `float` before its
    /// multiply, and the result is truncated.
    fn spatialize(&mut self, i: usize) {
        let (origin, right, view) = (self.listener_origin, self.listener_right, self.view_entity);
        let ch = &mut self.channels[i];

        // anything coming from the view entity will always be full volume
        if ch.entnum == view {
            ch.leftvol = ch.master_vol;
            ch.rightvol = ch.master_vol;
            return;
        }

        // calculate stereo separation and distance attenuation
        let mut v = [ch.origin[0] - origin[0], ch.origin[1] - origin[1], ch.origin[2] - origin[2]];
        let dist = normalize_x87(&mut v) * f64::from(ch.dist_mult);
        let dot = (f64::from(right[0]) * f64::from(v[0]) + f64::from(right[1]) * f64::from(v[1]))
            + f64::from(right[2]) * f64::from(v[2]);
        let (rscale, lscale) = (1.0 + dot, 1.0 - dot);

        // add in distance effect
        let volume = |scale: f64| {
            let scale = scale as f32;
            ((f64::from(scale) * f64::from(ch.master_vol)) as i32).max(0)
        };
        ch.rightvol = volume((1.0 - dist) * rscale);
        ch.leftvol = volume((1.0 - dist) * lscale);
    }

    /// `S_Update_`'s arithmetic: how many sample pairs to [`Mixer::paint`]
    /// now, for a device whose play position is `soundtime` pairs (counted
    /// from the mixer's start, `GetSoundtime`) and whose buffer holds
    /// `buffer_pairs`. It mixes `_snd_mixahead` seconds ahead of the play
    /// position; if the device has overtaken the mixer, the mixer first
    /// skips to it ("check to make sure that we haven't overshot").
    pub fn samples_ahead(&mut self, soundtime: i64, buffer_pairs: usize) -> usize {
        if self.paintedtime < soundtime {
            self.paintedtime = soundtime;
        }
        let ahead = f64::from(self.cvars.mixahead) * f64::from(self.speed);
        let mut endtime = (soundtime as f64 + ahead) as i64;
        let cap = i64::try_from(buffer_pairs).unwrap_or(i64::MAX);
        if endtime - soundtime > cap {
            endtime = soundtime + cap;
        }
        usize::try_from(endtime - self.paintedtime).unwrap_or(0)
    }

    /// Every sounding channel (with a sample), in table order.
    pub fn channels(&self) -> impl Iterator<Item = ChannelState<'_>> {
        self.channels[..self.total_channels].iter().enumerate().filter_map(|(index, ch)| {
            Some(ChannelState {
                index,
                sample: self.sfx.name(ch.sfx?),
                leftvol: ch.leftvol,
                rightvol: ch.rightvol,
                master_vol: ch.master_vol,
                pos: ch.pos,
                end: ch.end,
                entnum: ch.entnum,
                entchannel: ch.entchannel,
            })
        })
    }
}

/// One step of `S_UpdateAmbientSounds`' "don't adjust volume too fast":
/// move the int `master_vol` toward `vol` by `delta`, clamping onto it. Each
/// store truncates, as into the C's int: at the default fade and 72 fps the
/// step is 1.39, so the ramp rises 1 a frame and falls 2.
fn ramp(master_vol: &mut i32, vol: f64, delta: f64) {
    let mv = f64::from(*master_vol);
    if mv < vol {
        *master_vol = (mv + delta) as i32;
        if f64::from(*master_vol) > vol {
            *master_vol = vol as i32;
        }
    } else if mv > vol {
        *master_vol = (mv - delta) as i32;
        if f64::from(*master_vol) < vol {
            *master_vol = vol as i32;
        }
    }
}

/// `VectorNormalize` (mathlib.c) as id's x87 build runs it: the length is
/// computed and returned in extended precision (here `f64`), and only the
/// normalized components are stored back as `float`s.
fn normalize_x87(v: &mut [f32; 3]) -> f64 {
    let [x, y, z] = v.map(f64::from);
    let length = ((x * x + y * y) + z * z).sqrt();
    if length != 0.0 {
        let ilength = 1.0 / length;
        *v = [(x * ilength) as f32, (y * ilength) as f32, (z * ilength) as f32];
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 8-bit 11025 Hz `.wav` of `data` (unsigned bytes), looped from
    /// `cue` if given.
    fn wav(data: &[u8], cue: Option<u32>) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend(b"RIFF\0\0\0\0WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(11025u32.to_le_bytes());
        b.extend(11025u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(8u16.to_le_bytes());
        if let Some(ls) = cue {
            b.extend(b"cue ");
            b.extend(28u32.to_le_bytes());
            b.extend([0u8; 24]);
            b.extend(ls.to_le_bytes());
        }
        b.extend(b"data");
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(data);
        b
    }

    /// A pak of `sound/<name>` files.
    fn pak(files: &[(&str, Vec<u8>)]) -> Pak {
        let mut body: Vec<u8> = Vec::new();
        let mut dir: Vec<u8> = Vec::new();
        for (name, data) in files {
            let mut entry = [0u8; 64];
            let path = format!("sound/{name}");
            entry[..path.len()].copy_from_slice(path.as_bytes());
            entry[56..60].copy_from_slice(&(12 + body.len() as i32).to_le_bytes());
            entry[60..64].copy_from_slice(&(data.len() as i32).to_le_bytes());
            dir.extend(entry);
            body.extend(data);
        }
        let mut img = b"PACK".to_vec();
        img.extend((12 + body.len() as i32).to_le_bytes());
        img.extend((dir.len() as i32).to_le_bytes());
        img.extend(body);
        img.extend(dir);
        Pak::from_bytes("t.pak".into(), img).expect("pak")
    }

    /// The test pak: the two ambients, a 100-sample one-shot at +64, and a
    /// 50-sample hum of the values 1..=50 looped from its 11th sample.
    fn test_pak() -> Pak {
        let hum: Vec<u8> = (1..=50).map(|v| 128 + v).collect();
        pak(&[
            ("ambience/water1.wav", wav(&[0xa0; 64], Some(0))),
            ("ambience/wind2.wav", wav(&[0x60; 64], Some(0))),
            ("t/beep.wav", wav(&[0xc0; 100], None)),
            ("t/hum.wav", wav(&hum, Some(10))),
        ])
    }

    fn event(entity: i32, channel: i32, sample: &str, origin: [f32; 3]) -> SoundEvent {
        SoundEvent { entity, channel, sound_index: -1, sample: sample.into(), origin, volume: 1.0, attenuation: 1.0 }
    }

    /// A listener at the origin facing +x: its right is -y.
    fn listener() -> Listener {
        Listener { pos: [0.0; 3], forward: [1.0, 0.0, 0.0], right: [0.0, -1.0, 0.0] }
    }

    /// One sample `v` (signed) of one channel at volume 255, after the
    /// scale table and the default master volume: `v * 31 * 8 * 179 >> 8`.
    fn full(v: i32) -> i16 {
        ((v * 31 * 8 * 179) >> 8) as i16
    }

    fn mixer(fixes: Fixes) -> (Pak, Mixer) {
        let p = test_pak();
        let mut m = Mixer::new(&p, 11025, fixes);
        m.set_view_entity(1);
        m.update(&listener(), None, 1.0 / 72.0); // no leaf: the ambients stay off
        (p, m)
    }

    fn paint(m: &mut Mixer, pairs: usize) -> Vec<i16> {
        let mut out = vec![0; 2 * pairs];
        m.paint(&mut out);
        out
    }

    #[test]
    fn the_players_sound_plays_centred_at_full_volume_and_ends() {
        let (p, mut m) = mixer(Fixes::NONE);
        m.start_sound(&p, &event(1, 1, "t/beep.wav", [900.0, 0.0, 0.0]));
        let out = paint(&mut m, 120);
        assert_eq!(&out[..2], &[full(64), full(64)], "view entity: 255 both sides, no falloff");
        assert!(out[..200].iter().all(|&s| s == full(64)), "100 samples");
        assert!(out[200..].iter().all(|&s| s == 0), "then silence");
        assert_eq!(m.channels().count(), 0, "the channel stopped");
    }

    #[test]
    fn spatialize_pans_by_the_right_vector_and_falls_off_with_distance() {
        let (p, mut m) = mixer(Fixes::NONE);
        // 500 units to the right at attenuation 1: dist = 500 * 0.001f,
        // just over 0.5, so (1 - dist) * 2 rounds to a float under 1 and
        // the right side truncates to 254 — id's number.
        m.start_sound(&p, &event(7, 1, "t/beep.wav", [0.0, -500.0, 0.0]));
        let c = m.channels().next().expect("sounding");
        assert_eq!((c.leftvol, c.rightvol), (0, 254));
        // Straight ahead at 250: centred, 3/4 volume.
        m.start_sound(&p, &event(8, 1, "t/beep.wav", [250.0, 0.0, 0.0]));
        let c = m.channels().find(|c| c.entnum == 8).expect("sounding");
        assert_eq!((c.leftvol, c.rightvol), (191, 191));
        // Past 1000 units nothing starts at all.
        m.start_sound(&p, &event(9, 1, "t/beep.wav", [1001.0, 0.0, 0.0]));
        assert!(m.channels().all(|c| c.entnum != 9), "not audible at all");
    }

    #[test]
    fn a_sound_overrides_its_entity_channel_but_channel_0_never_does() {
        let (p, mut m) = mixer(Fixes::NONE);
        m.start_sound(&p, &event(5, 3, "t/beep.wav", [10.0, 0.0, 0.0]));
        m.start_sound(&p, &event(5, 3, "t/hum.wav", [10.0, 0.0, 0.0]));
        let busy: Vec<_> = m.channels().map(|c| c.sample.to_string()).collect();
        assert_eq!(busy, ["t/hum.wav"], "(5, 3) replaced");
        m.start_sound(&p, &event(5, 0, "t/beep.wav", [10.0, 0.0, 0.0]));
        m.start_sound(&p, &event(5, 0, "t/beep.wav", [10.0, 0.0, 0.0]));
        assert_eq!(m.channels().count(), 3, "channel 0 never overrides");
        m.start_sound(&p, &event(5, -1, "t/beep.wav", [10.0, 0.0, 0.0]));
        assert_eq!(m.channels().count(), 3, "-1 takes the first of entity 5's non-zero channels");
    }

    #[test]
    fn a_repeat_in_the_same_frame_starts_offset_by_rand() {
        let (p, mut m) = mixer(Fixes::NONE);
        m.start_sound(&p, &event(5, 0, "t/beep.wav", [10.0, 0.0, 0.0]));
        m.start_sound(&p, &event(6, 0, "t/beep.wav", [10.0, 0.0, 0.0]));
        let pos: Vec<i32> = m.channels().map(|c| c.pos).collect();
        // MSVC's first rand() is 41; 41 % 1102 = 41.
        assert_eq!(pos, [0, 41]);
    }

    #[test]
    fn monsters_never_take_the_players_channels() {
        let (p, mut m) = mixer(Fixes::NONE);
        for chan in 1..=8 {
            m.start_sound(&p, &event(1, chan, "t/hum.wav", [0.0; 3]));
        }
        m.start_sound(&p, &event(9, 1, "t/beep.wav", [10.0, 0.0, 0.0]));
        assert!(m.channels().all(|c| c.entnum == 1), "all eight are the player's: the monster's is dropped");
        m.start_sound(&p, &event(1, 0, "t/beep.wav", [0.0; 3]));
        assert_eq!(m.channels().filter(|c| c.sample == "t/beep.wav").count(), 1, "the player's own may");
    }

    #[test]
    fn static_sounds_of_one_sample_combine_and_a_one_shot_spends_its_slot() {
        let (p, mut m) = mixer(Fixes::NONE);
        let st = |sample: &str, x: f32| StaticSound {
            origin: [x, 0.0, 0.0],
            sound_index: -1,
            sample: sample.into(),
            volume: 1.0,
            attenuation: 3.0,
        };
        m.static_sound(&p, &st("t/hum.wav", 100.0));
        m.static_sound(&p, &st("t/beep.wav", 100.0)); // not looped: refused
        m.static_sound(&p, &st("t/hum.wav", 100.0));
        assert_eq!(m.total_channels(), FIRST_STATIC + 3, "the refused one spent a slot");
        m.update(&listener(), None, 1.0 / 72.0);
        let vols: Vec<(usize, i32)> = m.channels().map(|c| (c.index, c.leftvol)).collect();
        // ATTN_STATIC 3 at 100 units, straight ahead: (1 - 0.3) * 255 each.
        assert_eq!(vols, [(FIRST_STATIC, 2 * 178), (FIRST_STATIC + 2, 0)], "the second joined the first");
    }

    #[test]
    fn the_ambients_ramp_with_the_frame_classic_stalls_above_100_fps() {
        let water = Some([255, 0, 0, 0]);
        for (fixes, want) in [(Fixes::NONE, 0), (Fixes::ALL, 72)] {
            let (_p, mut m) = mixer(fixes);
            for _ in 0..144 {
                m.update(&listener(), water, 1.0 / 144.0);
            }
            let c = m.channels().find(|c| c.index == AMBIENT_WATER).expect("water sounding or held");
            assert_eq!(c.master_vol, want, "{fixes:?}: trunc(mv + 100/144) = mv in id's");
        }
        let (_p, mut m) = mixer(Fixes::NONE);
        for _ in 0..72 {
            m.update(&listener(), water, 1.0 / 72.0);
        }
        assert_eq!(m.channels().next().map(|c| c.master_vol), Some(72), "+1 a frame at 72 fps");
        m.update(&listener(), None, 1.0 / 72.0);
        assert!(m.channels().all(|c| c.index >= NUM_AMBIENTS), "no leaf: silent at once");
    }

    #[test]
    fn id_paints_a_loops_restart_at_the_passes_start_the_fix_where_it_belongs() {
        // The hum is 1..=50 looped from sample 10: one 100-pair pass holds
        // the first 50 and 40 of the loop.
        let expect: Vec<i16> = (0..100).map(|k| full(if k < 50 { k + 1 } else { 11 + (k - 50) % 40 })).collect();
        let (p, mut m) = mixer(Fixes::ALL);
        m.start_sound(&p, &event(1, 1, "t/hum.wav", [0.0; 3]));
        let out = paint(&mut m, 100);
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        assert_eq!(left, expect, "the fix: a seamless loop");

        let (p, mut m) = mixer(Fixes::NONE);
        m.start_sound(&p, &event(1, 1, "t/hum.wav", [0.0; 3]));
        let out = paint(&mut m, 100);
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        // id's: the loop's 40 samples and the next lap's first 10 all land
        // from pair 0, over the start, and pairs 50..100 get nothing.
        assert_eq!(left[0], full(1 + 11 + 11), "pair 0: samples 1, 11 and 11");
        assert_eq!(left[39], full(40 + 50), "pair 39: samples 40 and 50");
        assert!(left[50..].iter().all(|&s| s == 0), "and a gap where they belonged");
    }

    #[test]
    fn stop_sound_in_id_misses_the_last_four_dynamic_channels() {
        for (fixes, stopped) in [(Fixes::NONE, false), (Fixes::ALL, true)] {
            let (p, mut m) = mixer(fixes);
            for ent in 2..10 {
                m.start_sound(&p, &event(ent, 1, "t/hum.wav", [10.0, 0.0, 0.0]));
            }
            let last = m.channels().last().map(|c| (c.index, c.entnum)).expect("sounding");
            assert_eq!(last, (FIRST_STATIC - 1, 9));
            m.stop_sound(9, 1);
            assert_eq!(m.channels().all(|c| c.entnum != 9), stopped, "{fixes:?}");
        }
    }

    #[test]
    fn samples_ahead_mixes_ahead_of_the_device_and_skips_when_overtaken() {
        let (_p, mut m) = mixer(Fixes::NONE);
        assert_eq!(m.samples_ahead(0, 32768), 1102, "0.1 s at 11025");
        paint(&mut m, 1102);
        assert_eq!(m.samples_ahead(153, 32768), 153, "a frame later: one frame's worth");
        assert_eq!(m.samples_ahead(153, 500), 0, "never more than the device buffer ahead");
        paint(&mut m, 153);
        assert_eq!(m.samples_ahead(5000, 32768), 1102, "overtaken: skip to the device, then 0.1 s");
        assert_eq!(m.painted_time(), 5000);
    }

    #[test]
    fn nosound_drops_new_sounds_and_volume_clamps_to_16_bits() {
        let (p, mut m) = mixer(Fixes::NONE);
        m.cvars.nosound = true;
        m.start_sound(&p, &event(1, 1, "t/beep.wav", [0.0; 3]));
        assert_eq!(m.channels().count(), 0);
        m.cvars.nosound = false;
        m.cvars.volume = 1.0;
        for chan in 1..=3 {
            m.start_sound(&p, &event(1, chan, "t/beep.wav", [0.0; 3]));
        }
        assert_eq!(paint(&mut m, 1)[0], i16::MAX, "3 x 15872 clamps");
    }

    #[test]
    fn the_modes_are_ids_mixer_at_11025_and_the_2026_one_at_the_device_rate() {
        let p = test_pak();
        let classic = SoundMode::Classic.mixer(&p, 48000);
        assert_eq!((classic.rate(), classic.fixes, classic.cvars.mixahead), (11025, Fixes::NONE, 0.1));
        let modern = SoundMode::Modern.mixer(&p, 44100);
        assert_eq!((modern.rate(), modern.fixes, modern.cvars.mixahead), (44100, Fixes::ALL, MODERN_MIXAHEAD));
        assert_eq!(SoundMode::Modern.rate(0), 48000, "an unknown device");
        assert_eq!(SoundMode::default(), SoundMode::Modern);
        for m in [SoundMode::Classic, SoundMode::Modern] {
            assert_eq!(SoundMode::parse(m.name()), Some(m));
        }
        assert_eq!(SoundMode::parse("x"), None);
    }

    #[test]
    fn stop_all_asks_once_for_the_output_buffer_to_be_cleared() {
        let (_p, mut m) = mixer(Fixes::NONE);
        assert!(m.take_clear(), "S_Init's S_StopAllSounds (true)");
        assert!(!m.take_clear(), "once");
        m.stop_all_sounds();
        assert!(m.take_clear());
    }

    #[test]
    fn play_starts_a_sample_at_the_listener_on_a_fresh_entity_each_time() {
        let (p, mut m) = mixer(Fixes::NONE);
        let far = Listener { pos: [500.0, 0.0, 0.0], ..listener() };
        m.update(&far, None, 1.0 / 72.0);
        m.play(&p, "t/beep");
        m.play(&p, "t/beep.wav");
        let busy: Vec<_> = m.channels().map(|c| (c.entnum, c.leftvol, c.rightvol)).collect();
        assert_eq!(busy, [(345, 255, 255), (346, 255, 255)], "centred, full, on S_Play's hash");
    }

    #[test]
    fn run_carries_out_a_frames_calls_in_order() {
        let (p, mut m) = mixer(Fixes::NONE);
        let calls = [
            SoundCall::Start { events: vec![event(4, 2, "t/hum.wav", [10.0, 0.0, 0.0])], view_entity: 4 },
            SoundCall::Update { listener: listener(), leaf_ambient: Some([0, 255, 0, 0]), frametime: 1.0 / 72.0 },
            SoundCall::Stop(vec![(4, 2)]),
        ];
        m.run(&p, &calls);
        let busy: Vec<_> = m.channels().map(|c| (c.index, c.sample, c.leftvol)).collect();
        let want = [(AMBIENT_WATER, "ambience/water1.wav", 0), (AMBIENT_SKY, "ambience/wind2.wav", 1)];
        assert_eq!(busy, want, "the hum stopped; the wind rose by 1, the water (level 0) holds at 0");
        m.run(&p, &[SoundCall::StopAll]);
        assert_eq!((m.channels().count(), m.total_channels()), (0, FIRST_STATIC));
    }
}
