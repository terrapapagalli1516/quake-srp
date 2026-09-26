//! The sound bridge — the client half of WinQuake's `snd_dma.c`, re-homed for
//! Web Audio: the one-shot queue (`S_StartSound` with `SND_PickChannel`'s
//! `(entity, channel)` override and `SND_Spatialize`'s view-entity rule),
//! `svc_stopsound`, the placed static loops (`S_StaticSound`), the automatic
//! leaf ambients (`S_UpdateAmbientSounds`) and the menu's `S_LocalSound`s,
//! queued here each frame and taken by the program's loop ([`crate::sys`]),
//! which sends them to the page as protocol messages. The page does the
//! mixing.

use std::cell::RefCell;

use quake_rs::bsp::NUM_AMBIENTS;
use quake_rs::client::{Listener, SoundCall};
use quake_rs::pak::Pak;
use quake_rs::render::MenuSound;
use quake_rs::server::StaticSound;
use quake_rs::snd::{
    self, wav_info, AmbientChannels, SndParams, StaticLoop, AMBIENT_FADE_DEFAULT,
    AMBIENT_LEVEL_DEFAULT, AMBIENT_SAMPLES,
};

use crate::app::{ensure_app, APP};
use crate::common::pak;

// --- sound: real Quake .wav bytes out of the pak, for the page to play ---

thread_local! {
    /// WAV byte payloads for sounds fired this frame, awaiting playback, each
    /// paired with the spatial params the page positions it with.
    pub(crate) static SND_QUEUE: RefCell<Vec<(Vec<u8>, SndParams)>> = const { RefCell::new(Vec::new()) };
    /// The current listener pose, refreshed by every client frame's `S_Update`: eye position plus
    /// the forward and right unit vectors derived from the player's yaw. The
    /// page spatializes each sound against it (the `Listener` record).
    static LISTENER: RefCell<Listener> = const { RefCell::new(Listener::zero()) };
    /// Whether the page's `AudioContext` is running yet (the protocol's
    /// `AudioReady`; it starts suspended until a user gesture). Until then
    /// `queue_sounds` drops sounds on the floor instead
    /// of appending them every frame up to the 12-cap — otherwise a backlog of
    /// stale sounds from before audio started would all play at once when it does.
    static AUDIO_READY: RefCell<bool> = const { RefCell::new(false) };
    /// Pending menu `S_LocalSound`s (menu1/menu2/menu3), drained from the menu
    /// by [`take_menu_sounds`]. Only fills while audio is ready (same
    /// no-backlog rule as `queue_sounds`).
    static MENU_SND_QUEUE: RefCell<Vec<MenuSound>> = const { RefCell::new(Vec::new()) };
    /// The three menu WAV payloads, loaded from the pak once on first use and
    /// cached (keyed [menu1, menu2, menu3]); `None` = not yet tried.
    static MENU_WAVS: RefCell<[Option<Vec<u8>>; 3]> = const { RefCell::new([None, None, None]) };
    /// The `play` command's samples (`S_Play`), for this frame.
    static PLAY_QUEUE: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// The page's audio state (LOW-10): set once the browser `AudioContext` has
/// resumed to `running` (the protocol's `AudioReady`). While it is `0` the
/// per-frame sound queue is not filled, so no pre-audio backlog accumulates
/// to flush when playback finally starts.
pub(crate) fn set_audio_ready(ready: i32) {
    AUDIO_READY.with(|r| *r.borrow_mut() = ready != 0);
}

/// Carry out the calls a client frame or a level load made into the sound
/// layer ([`SoundCall`]), in the order it made them: the one-shot queue, the
/// stops, a level change's loop teardown and new placed loops, and
/// `S_Update`'s listener pose and ambient ramp. `pak` is the client's, which
/// the samples load from.
pub(crate) fn play(pak: &Pak, calls: Vec<SoundCall>) {
    for call in calls {
        match call {
            SoundCall::Start { events, view_entity } => queue_sounds(pak, &events, view_entity),
            SoundCall::Stop(stops) => push_stop_sounds(&stops),
            SoundCall::StopAll => bump_sound_generation(),
            SoundCall::Static(statics) => queue_static_sounds(pak, &statics),
            SoundCall::Update { listener, leaf_ambient, frametime } => {
                LISTENER.with(|l| *l.borrow_mut() = listener);
                ramp_ambient_channels(leaf_ambient.as_ref(), frametime);
            }
        }
    }
}

/// `S_StartSound` for `events` into the page's one-shot queue
/// ([`snd::queue_sounds`]: the sample, its loop window, `SND_PickChannel`'s
/// `(entity, channel)` override, the 12 cap), once the page's audio runs.
fn queue_sounds(pak: &Pak, events: &[quake_rs::server::SoundEvent], view_entity: i32) {
    if events.is_empty() {
        return;
    }
    // LOW-10: don't accumulate a backlog before audio starts. `drainGameSounds`
    // early-returns while the AudioContext is suspended, but the queue would
    // keep growing to its cap every frame and then dump stale sounds the moment
    // audio resumes. Skip enqueuing entirely until the page reports the context
    // running via `set_audio_ready(1)`.
    if !AUDIO_READY.with(|r| *r.borrow()) {
        return;
    }
    SND_QUEUE.with(|q| snd::queue_sounds(&mut q.borrow_mut(), pak, events, view_entity));
}

#[cfg(test)]
thread_local! {
    /// (Tests.) The pull interface the page used before the protocol: the
    /// last popped sample's bytes, its params, and its loop window.
    static SND: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static SND_CUR: RefCell<SndParams> = const { RefCell::new(SndParams::zero()) };
    static SND_LOOP: RefCell<(f32, f32)> = const { RefCell::new((0.0, 0.0)) };
}

/// This frame's one-shots, in the order they were started: each sample's WAV
/// bytes with its placement, `(entity, channel)` key and loop window (start
/// -1.0 = a one-shot; otherwise the page loops the source from there).
pub(crate) fn take_sounds() -> Vec<(Vec<u8>, SndParams)> {
    SND_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// (Tests.) Pop the next queued sound into the scratch buffer and return its
/// byte length (0 when the queue is empty), its params stashed for the
/// `sound_*` getters — the pull interface the page used before the protocol.
#[cfg(test)]
pub(crate) fn poll_sound() -> i32 {
    let next = SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    match next {
        Some((bytes, params)) => {
            let len = bytes.len() as i32;
            SND.with(|s| *s.borrow_mut() = bytes);
            SND_LOOP.with(|l| *l.borrow_mut() = (params.loop_start, params.loop_end));
            SND_CUR.with(|p| *p.borrow_mut() = params);
            len
        }
        None => 0,
    }
}

/// (Tests.) Pop the next queued MENU sound (menu.c's `S_LocalSound`
/// triggers: menu1 on cursor moves, menu2 on enter/select, menu3 on slider
/// adjusts) into the scratch and return its WAV byte length (0 when none).
/// The page plays each (`LocalSound`) per `S_LocalSound` semantics (snd_dma.c: `S_StartSound(cl.viewentity, -1, sfx,
/// vec3_origin, 1, 1)` — full volume, centred, no distance falloff; the page's
/// master volume still scales it, like the C mixer's `volume.value`). Works in
/// EVERY mode (the menu overlays the attract demo too). While the page hasn't
/// reported audio running ([`set_audio_ready`]), queued menu sounds are
/// discarded instead — the same no-backlog rule as `queue_sounds`.
#[cfg(test)]
pub(crate) fn poll_menu_sound() -> i32 {
    match next_menu_sound() {
        Some(b) => {
            let len = b.len() as i32;
            SND.with(|s| *s.borrow_mut() = b);
            len
        }
        None => 0,
    }
}

/// This frame's local sounds, each sample's WAV bytes: the menu's
/// `S_LocalSound`s, then the `play` command's ([`s_play`]).
pub(crate) fn take_menu_sounds() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = std::iter::from_fn(next_menu_sound).collect();
    out.extend(PLAY_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut())));
    out
}

/// The next queued menu sound's WAV bytes (see [`poll_menu_sound`]).
fn next_menu_sound() -> Option<Vec<u8>> {
    // Drain the menu's queue into the local one (or the bin, pre-audio).
    let ready = AUDIO_READY.with(|r| *r.borrow());
    ensure_app(|a| {
        let queued = a.menu.take_sounds();
        if ready && !queued.is_empty() {
            MENU_SND_QUEUE.with(|q| {
                let mut q = q.borrow_mut();
                for s in queued {
                    if q.len() < 16 {
                        q.push(s);
                    }
                }
            });
        }
    });
    if !ready {
        return None;
    }
    let snd = MENU_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    })?;
    let slot = match snd {
        MenuSound::Menu1 => 0,
        MenuSound::Menu2 => 1,
        MenuSound::Menu3 => 2,
    };
    // Load-once cache: the C's S_PrecacheSound holds these three resident.
    MENU_WAVS.with(|w| {
        let mut w = w.borrow_mut();
        if w[slot].is_none() {
            // S_LoadSound: sprintf(namebuffer, "sound/%s", s->name).
            w[slot] = pak()
                .and_then(|p| p.read_file(&format!("sound/{}", snd.sample())).ok().flatten());
        }
        w[slot].clone()
    })
}

/// Spatial params of the entry the most recent `poll_sound` popped. `origin_*`
/// are the world emission point; `volume` is `0.0..=1.0`; `attenuation` is
/// `0.0..=4.0` (0 = no falloff, audible everywhere): what the page computes
/// distance gain and stereo pan from (tests read them after `poll_sound`).
#[cfg(test)]
pub(crate) fn sound_origin_x() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[0])
}
#[cfg(test)]
pub(crate) fn sound_origin_y() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[1])
}
#[cfg(test)]
pub(crate) fn sound_origin_z() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[2])
}
#[cfg(test)]
pub(crate) fn sound_volume() -> f32 {
    SND_CUR.with(|p| p.borrow().volume)
}
#[cfg(test)]
pub(crate) fn sound_attenuation() -> f32 {
    SND_CUR.with(|p| p.borrow().attenuation)
}

/// `1` when the entry the most recent `poll_sound` popped came from the
/// listener's own view entity (the player edict), else `0`. The C
/// `SND_Spatialize` (snd_dma.c:407-412) forces view-entity sounds to full
/// master volume on both channels with no distance falloff or pan; the page
/// reads this to take the same full-volume / centred path for the player's own
/// sounds (weapon fire, pain) instead of attenuating them with distance.
#[cfg(test)]
pub(crate) fn sound_is_view_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().is_view_entity as i32)
}

/// The emitting entity of the most recent `poll_sound` pop. With
/// [`sound_channel`] this is the `SND_PickChannel` override key: the page
/// keeps its playing one-shot sources in a registry keyed `(entity, channel)`
/// so a later sound on the same non-zero channel STOPS the source it replaces
/// (snd_dma.c: "always override sound from same entity"), and an
/// `svc_stopsound` can stop it (S_StopSound).
#[cfg(test)]
pub(crate) fn sound_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().entity)
}
/// The channel (0..=7) of the most recent `poll_sound` pop; 0 = CHAN_AUTO,
/// which never overrides and is never stopped by key.
#[cfg(test)]
pub(crate) fn sound_channel() -> i32 {
    SND_CUR.with(|p| p.borrow().channel)
}

thread_local! {
    /// Pending `svc_stopsound` stops, packed as the wire short `(entity << 3)
    /// | channel` (S_StopSound's arguments). Pushed by demo playback (the only
    /// current producer — live play's QuakeC stops loops by playing
    /// `misc/null.wav` on the same channel, which the override path handles);
    /// sent to the page each frame ([`take_stop_sounds`]). NOTE: id's own
    /// demo1/2/3 never send svc_stopsound (asserted by a wasm test), so for
    /// the shipped attract loop this stays empty — the plumbing exists for
    /// protocol completeness.
    pub(crate) static STOP_SND_QUEUE: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// Queue `(entity, channel)` stops for the page (see [`STOP_SND_QUEUE`]).
fn push_stop_sounds(stops: &[(i32, i32)]) {
    if stops.is_empty() {
        return;
    }
    STOP_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for &(ent, chan) in stops {
            q.push((ent << 3) | (chan & 7));
        }
    });
}

/// This frame's `svc_stopsound`s, as `(entity, channel)` (`S_StopSound`).
pub(crate) fn take_stop_sounds() -> Vec<(i32, i32)> {
    let packed = STOP_SND_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
    packed.into_iter().map(|v| (v >> 3, v & 7)).collect()
}

// ---------------------------------------------------------------------------
// Looping ambient audio: placed static sounds (PF_ambientsound ->
// S_StaticSound) + the four automatic per-leaf ambient channels
// (S_UpdateAmbientSounds). The page keeps a looping Web Audio source per
// static sound (re-spatialized every frame from the listener pose, same
// distance/pan law as the one-shots) and one looping source per audible
// ambient channel (gain driven by `ambient_gain`, centred — the C sets
// leftvol = rightvol). See quake_rs::snd for the faithful control logic.
// ---------------------------------------------------------------------------

thread_local! {
    /// Static sounds registered by the CURRENT level, to be sent to the page
    /// ([`take_static_sounds`]). NOT gated on `AUDIO_READY` (unlike the
    /// one-shot queue): these are persistent registrations, not a backlog —
    /// the page starts the loops whenever its AudioContext comes up.
    static STATIC_QUEUE: RefCell<Vec<StaticLoop>> = const { RefCell::new(Vec::new()) };
    /// Bumped on every level/mode transition (boot, demo boot, New Game, `map`,
    /// changelevel, restart). The page compares it each frame and, on a change,
    /// stops + drops every looping source — the S_StopAllSounds half of a level
    /// change; the new level's registrations then restart them.
    static SOUND_GENERATION: RefCell<i32> = const { RefCell::new(0) };
    /// The four automatic ambient channels' ramp state (S_UpdateAmbientSounds).
    static AMBIENT: RefCell<AmbientChannels> = const { RefCell::new(AmbientChannels::new()) };
    /// The four channels' CURRENT frame volumes (0..=255) as returned by the
    /// last [`AmbientChannels::update`] — the C's per-frame `chan->leftvol =
    /// chan->rightvol = chan->master_vol` (or the silenced `!l` /
    /// ambient-off frames' nothing-at-all). [`ambient_gain`] serves THESE,
    /// not the raw ramp state, so an out-of-world listener actually goes
    /// quiet on the page while `master_vol` is preserved for re-entry.
    static AMBIENT_VOLS: RefCell<[f32; NUM_AMBIENTS]> =
        const { RefCell::new([0.0; NUM_AMBIENTS]) };
}

/// A level/mode transition happened: invalidate every looping source. Mirrors
/// `S_StopAllSounds` (snd_dma.c), which memsets ALL channels — statics and the
/// ambient ramps included — on every server (re)connect. The page notices the
/// new generation and tears its loop nodes down; the engine-side static queue
/// is dropped (a not-yet-picked-up loop from the old level must never start
/// over the new one) and the ambient master_vols restart from silence.
fn bump_sound_generation() {
    SOUND_GENERATION.with(|g| {
        let mut g = g.borrow_mut();
        *g = g.wrapping_add(1);
    });
    // Every channel: a sound started earlier in the same frame dies too, so
    // everything still queued after a bump belongs to the new generation.
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
    STATIC_QUEUE.with(|q| q.borrow_mut().clear());
    AMBIENT.with(|a| *a.borrow_mut() = AmbientChannels::new());
    AMBIENT_VOLS.with(|v| *v.borrow_mut() = [0.0; NUM_AMBIENTS]);
}

/// `S_StaticSound` for a level's placed loops into the page's loop queue
/// ([`snd::queue_static_sounds`]: the slot budget, missing and unlooped
/// samples dropped).
fn queue_static_sounds(pak: &Pak, statics: &[StaticSound]) {
    STATIC_QUEUE.with(|q| snd::queue_static_sounds(&mut q.borrow_mut(), pak, statics));
}

/// The current sound generation. The loop sends it when it changes (the
/// `Generation` record), and the page then stops every looping and playing
/// source and rebuilds the loops from the new level's registrations (see
/// [`bump_sound_generation`]).
pub(crate) fn sound_generation() -> i32 {
    SOUND_GENERATION.with(|g| *g.borrow())
}

/// The placed loops the current level registered since the last call
/// (`S_StaticSound`), each with its sample, placement and loop window.
pub(crate) fn take_static_sounds() -> Vec<StaticLoop> {
    STATIC_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// (Tests.) Pop the next registered static (looping) sound into the scratch
/// buffer and return its byte length (0 when none are pending), its params
/// and loop window stashed like a one-shot pop's. The page starts a LOOPING
/// source for each (`StaticSound`) and re-spatializes it every frame.
#[cfg(test)]
pub(crate) fn poll_static_sound() -> i32 {
    let next = STATIC_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    match next {
        Some(sl) => {
            let len = sl.bytes.len() as i32;
            SND.with(|s| *s.borrow_mut() = sl.bytes);
            SND_CUR.with(|p| *p.borrow_mut() = sl.params);
            SND_LOOP.with(|l| *l.borrow_mut() = (sl.loop_start, sl.loop_end));
            len
        }
        None => 0,
    }
}

/// Loop start of the most recent `poll_sound`/`poll_static_sound`/`load_ambient_sound`, in
/// SECONDS (the `cue ` chunk's sample offset over the WAV rate — sample-rate
/// independent, so the page can hand it straight to `AudioBufferSourceNode.
/// loopStart` no matter what rate `decodeAudioData` resampled to).
#[cfg(test)]
pub(crate) fn sound_loop_start() -> f32 {
    SND_LOOP.with(|l| l.borrow().0)
}

/// Loop end in seconds of the most recent `poll_sound`/`poll_static_sound`/
/// `load_ambient_sound` (`GetWavinfo`'s `info.samples` over the rate; this is
/// the full data length unless a `LIST`/`mark` chunk declared a shorter loop).
/// 0.0 means "to the buffer's end" — Web Audio's `loopEnd` default.
#[cfg(test)]
pub(crate) fn sound_loop_end() -> f32 {
    SND_LOOP.with(|l| l.borrow().1)
}

/// Load ambient channel `ch`'s sample (`S_Init`: 0 = `ambience/water1.wav`,
/// 1 = `ambience/wind2.wav`) into the scratch buffer, returning its byte
/// length; the loop window lands in `sound_loop_start`/`sound_loop_end` like a
/// static pop. Returns 0 for channels the C never loaded (2 = slime, 3 = lava
/// have a NULL `ambient_sfx`) and for out-of-range/missing samples. The page
/// calls this once per audible channel, starts a centred looping source at
/// gain 0, and drives the gain from `ambient_gain` every frame.
#[cfg(test)]
pub(crate) fn load_ambient_sound(ch: i32) -> i32 {
    let Some((bytes, window)) = usize::try_from(ch).ok().and_then(ambient_sample) else {
        return 0;
    };
    SND_LOOP.with(|l| *l.borrow_mut() = window);
    let len = bytes.len() as i32;
    SND.with(|s| *s.borrow_mut() = bytes);
    len
}

/// Ambient channel `ch`'s sample (`S_Init`'s `ambient_sfx`) and its loop
/// window in seconds, or `None` for the channels the C never loaded.
pub(crate) fn ambient_sample(ch: usize) -> Option<(Vec<u8>, (f32, f32))> {
    let name = (*AMBIENT_SAMPLES.get(ch)?)?;
    let bytes = pak()?.read_file(&format!("sound/{name}")).ok()??;
    let info = wav_info(&bytes)?;
    let rate = info.rate.max(1) as f32;
    let window = (info.loop_start.unwrap_or(0) as f32 / rate, info.samples as f32 / rate);
    Some((bytes, window))
}

/// Ambient channel `ch`'s volume THIS frame in `0.0..=1.0` (the value
/// [`AmbientChannels::update`] returned, over the C's 255 scale — see
/// [`AMBIENT_VOLS`]). The page multiplies by its master volume and writes it
/// to the channel's gain node every frame — both sides of the C's
/// `chan->leftvol = chan->rightvol = chan->master_vol` (ambients are centred,
/// never panned or distance-attenuated). 0.0 on the frames the C silenced
/// outright (listener outside the world, `ambient_level` 0).
#[cfg(test)]
pub(crate) fn ambient_gain(ch: i32) -> f32 {
    let Ok(c) = usize::try_from(ch) else { return 0.0 };
    AMBIENT_VOLS.with(|v| v.borrow().get(c).copied().unwrap_or(0.0)) / 255.0
}

/// The four ambient channels' volumes this frame, `0.0..=1.0` (see
/// [`ambient_gain`]).
pub(crate) fn ambient_gains() -> [f32; NUM_AMBIENTS] {
    AMBIENT_VOLS.with(|v| v.borrow().map(|x| x / 255.0))
}

/// The listener pose as of the last client frame's `S_Update`.
pub(crate) fn listener() -> Listener {
    LISTENER.with(|l| *l.borrow())
}

/// Ramp the four ambient channels toward `leaf_levels` and publish the frame's
/// returned volumes for [`ambient_gain`] — `update`'s return is the ONLY place
/// the C's silenced `!l`/ambient-off frames differ from the ramp state, so it
/// must be what the page hears.
fn ramp_ambient_channels(leaf_levels: Option<&[u8; NUM_AMBIENTS]>, frametime: f32) {
    let vols = AMBIENT.with(|a| {
        a.borrow_mut().update(
            leaf_levels,
            frametime,
            AMBIENT_LEVEL_DEFAULT,
            AMBIENT_FADE_DEFAULT,
        )
    });
    AMBIENT_VOLS.with(|v| *v.borrow_mut() = vols);
}

/// `S_Play` (snd_dma.c, the `play` command): each named sample (`.wav`
/// added when the name has no extension) at the listener, full volume, like
/// a menu click — `S_StartSound` at `listener_origin` is unattenuated and
/// centred. Dropped while the page's audio is not running, as every sound
/// is. The page's sound button plays `items/r_item1.wav` with it.
pub(crate) fn s_play(names: &[&str]) {
    if !AUDIO_READY.with(|r| *r.borrow()) {
        return;
    }
    let Some(p) = pak() else { return };
    for name in names {
        let name = if name.contains('.') { name.to_string() } else { format!("{name}.wav") };
        if let Ok(Some(bytes)) = p.read_file(&format!("sound/{name}")) {
            PLAY_QUEUE.with(|q| q.borrow_mut().push(bytes));
        }
    }
}

/// The Options "Volume" as a `0.0..=1.0` master gain (default 0.7). The page
/// scales its sound gains by this. Reads from the App-level menu; 1.0 when the app
/// has not been created yet (so audio is never accidentally silenced before then).
pub(crate) fn volume() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.volume())
            .unwrap_or(1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::snd::MAX_STATIC_SOUNDS;
    use quake_rs::bsp::Bsp;
    use quake_rs::progs::Progs;
    use quake_rs::server::{Server, SoundEvent};

    use crate::app::{boot, boot_attract, boot_demo};
    use crate::menu::{menu_down, menu_select, menu_up};
    use crate::test_util::*;

    fn ev(entity: i32, channel: i32, sample: &str, vol: f32) -> SoundEvent {
        SoundEvent {
            entity,
            channel,
            sound_index: -1,
            sample: sample.to_string(),
            origin: [vol, 0.0, 0.0], // stash a tag in origin.x so we can identify it
            volume: vol,
            attenuation: 1.0,
        }
    }

    /// Drain the whole queue into a list of (volume, is_view_entity) via the same
    /// poll path the page uses.
    fn drain_queue() -> Vec<(f32, bool)> {
        let mut out = Vec::new();
        loop {
            let len = poll_sound();
            if len == 0 {
                break;
            }
            out.push((sound_volume(), sound_is_view_entity() != 0));
        }
        out
    }

    #[test]
    fn queue_sounds_keys_by_entity_channel_not_sample_name() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA"), ("sound/b.wav", b"BBBB")]);

        // Two DISTINCT emitters of the SAME sample (different entities, channel 0
        // each) must BOTH queue — the old by-name dedup would have dropped one.
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 0, "a.wav", 0.3), ev(5, 0, "a.wav", 0.7)],
            /*view_entity*/ -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 2, "distinct emitters of the same sample both queue");
        assert_eq!(got[0].0, 0.3);
        assert_eq!(got[1].0, 0.7);

        // Same (entity, channel) with a NON-ZERO channel RESTARTS that channel:
        // the later event overrides the earlier queued entry (one slot, latest
        // params).
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 1, "a.wav", 0.2), ev(2, 1, "b.wav", 0.9)],
            -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 1, "same (entity,channel>0) collapses to one slot");
        assert_eq!(got[0].0, 0.9, "the restart keeps the LATER sound's params");

        // Channel 0 NEVER overrides: the same entity firing twice on channel 0
        // queues twice (Quake's auto-channel allocates fresh each time).
        reset_queue();
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.1), ev(2, 0, "a.wav", 0.4)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 2, "channel 0 never overrides; both queue");

        // Different non-zero channels of the SAME entity are independent.
        reset_queue();
        queue_sounds(&pak, &[ev(2, 1, "a.wav", 0.5), ev(2, 2, "b.wav", 0.6)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 2, "distinct channels of one entity stay separate");
    }

    #[test]
    fn queue_sounds_flags_view_entity() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA"), ("sound/b.wav", b"BBBB")]);
        reset_queue();
        // entity 7 is the player (view entity); entity 3 is a monster.
        queue_sounds(&pak, &[ev(7, 0, "a.wav", 0.5), ev(3, 0, "b.wav", 0.5)], 7);
        let got = drain_queue();
        assert_eq!(got.len(), 2);
        assert!(got[0].1, "the view entity's sound is flagged");
        assert!(!got[1].1, "the monster's sound is not flagged");
    }

    #[test]
    fn queue_sounds_plays_ambience_one_shots() {
        // `ambience/*` is NOT a loop marker on the one-shot path: E1M6's
        // trigger_push wind tunnels fire `sound (other, CHAN_AUTO,
        // "ambience/windfly.wav", 1, ATTN_NORM)` (QuakeC trigger_push_touch)
        // as genuine gameplay one-shots the C plays like any other sample.
        // The silent misc/null.wav queues too: it overrides its (entity,
        // channel) like any sound, which is what ends some movers' loops.
        let pak = build_test_pak(&[
            ("sound/ambience/windfly.wav", b"WIND"),
            ("sound/misc/null.wav", b"NULL"),
        ]);
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 0, "ambience/windfly.wav", 0.8), ev(3, 0, "misc/null.wav", 0.5)],
            -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 2, "windfly and null.wav both queued");
        assert_eq!(got[0].0, 0.8, "the ambience one-shot's own params");
    }

    #[test]
    fn queue_sounds_carries_the_cue_loop_so_movers_hum_until_their_stop_sound() {
        // CENSUS F9: GetWavinfo reads the `cue ` loop point and SND_PaintChannels
        // loops the channel from it until another sound takes the same (entity,
        // channel). A door (entity 5) plays its moving sound on CHAN_VOICE (2),
        // then its stop sound on the same channel: the page gets the first with
        // a loop window (and loops it) and the second as a one-shot on the same
        // key (its registry stops the loop).
        let mv = test_wav(Some(8), 64);
        let stop = test_wav(None, 32);
        let pak = build_test_pak(&[("sound/doors/doormv1.wav", &mv), ("sound/doors/drclos4.wav", &stop)]);
        reset_queue();
        queue_sounds(&pak, &[ev(5, 2, "doors/doormv1.wav", 1.0)], -1);
        assert!(poll_sound() > 0);
        assert_eq!((sound_entity(), sound_channel()), (5, 2));
        assert!((sound_loop_start() - 8.0 / 11025.0).abs() < 1e-7, "loops from the cue point");
        assert!((sound_loop_end() - 64.0 / 11025.0).abs() < 1e-7, "to the end of the data");
        queue_sounds(&pak, &[ev(5, 2, "doors/drclos4.wav", 1.0)], -1);
        assert!(poll_sound() > 0);
        assert_eq!((sound_entity(), sound_channel()), (5, 2), "the stop sound's key");
        assert_eq!(sound_loop_start(), -1.0, "no cue chunk: a one-shot");
        // The real movers carry cue chunks; their stop sounds and null.wav don't.
        let pak = crate::common::pak().expect("embedded pak");
        let loops = |name: &str| {
            let bytes = pak.read_file(&format!("sound/{name}")).ok().flatten().expect(name);
            wav_info(&bytes).expect(name).loop_start.is_some()
        };
        for name in ["doors/doormv1.wav", "doors/hydro1.wav", "plats/plat1.wav", "plats/train1.wav"] {
            assert!(loops(name), "{name} loops");
        }
        for name in ["doors/drclos4.wav", "plats/plat2.wav", "plats/train2.wav", "misc/null.wav"] {
            assert!(!loops(name), "{name} is a one-shot");
        }
    }

    /// Second review: the 12-sound cap ran before the (entity, channel)
    /// override, so a mover's stop sound (CHAN_VOICE) in a busy frame was
    /// dropped and its "moving" hum looped forever. SND_PickChannel's
    /// same-key override comes before anything else and always wins
    /// (snd_dma.c:365), so the C never loses it: past the cap a non-zero
    /// channel's sound still goes out, replacing an undrained entry of its key.
    #[test]
    fn queue_cap_never_drops_a_channel_override() {
        let mv = test_wav(Some(8), 64);
        let stop = test_wav(None, 32);
        let pak = build_test_pak(&[
            ("sound/a.wav", b"AAAA"),
            ("sound/doors/doormv1.wav", &mv),
            ("sound/doors/drclos4.wav", &stop),
        ]);
        reset_queue();
        queue_sounds(&pak, &[ev(5, 2, "doors/doormv1.wav", 1.0)], -1);
        assert!(poll_sound() > 0 && sound_loop_start() > 0.0, "the door hums (a loop)");
        // A busy frame: twelve channel-0 sounds, then the door stops, then a
        // thirteenth channel-0 sound and a second override of the same key.
        let mut frame: Vec<SoundEvent> = (0..12).map(|i| ev(100 + i, 0, "a.wav", 0.5)).collect();
        frame.push(ev(5, 2, "doors/drclos4.wav", 1.0));
        frame.push(ev(113, 0, "a.wav", 0.5));
        frame.push(ev(6, 1, "a.wav", 0.25));
        frame.push(ev(6, 1, "a.wav", 0.75));
        queue_sounds(&pak, &frame, -1);
        let mut got = Vec::new();
        while poll_sound() > 0 {
            got.push((sound_entity(), sound_channel(), sound_volume(), sound_loop_start()));
        }
        assert_eq!(got.len(), 14, "12 channel-0 sounds + one per overridden key: {got:?}");
        assert!(got.contains(&(5, 2, 1.0, -1.0)), "the door's stop sound reaches its key");
        assert!(got.contains(&(6, 1, 0.75, -1.0)), "the later override of (6, 1) wins");
        assert!(!got.iter().any(|g| g.0 == 113), "a channel-0 sound past the cap is dropped");
        // An undrained keyed entry from an earlier frame is replaced, not doubled.
        reset_queue();
        let full: Vec<SoundEvent> = (0..12).map(|i| ev(100 + i, 0, "a.wav", 0.5)).collect();
        queue_sounds(&pak, &full, -1);
        queue_sounds(&pak, &[ev(5, 2, "doors/doormv1.wav", 1.0)], -1);
        queue_sounds(&pak, &[ev(5, 2, "doors/drclos4.wav", 1.0)], -1);
        let n = SND_QUEUE.with(|q| q.borrow().len());
        assert_eq!(n, 13);
        reset_queue();
        set_audio_ready(0);
    }

    #[test]
    fn queue_sounds_skips_until_audio_ready() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA")]);
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0); // audio not running yet
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.5)], -1);
        assert!(
            SND_QUEUE.with(|q| q.borrow().is_empty()),
            "no sounds accumulate before audio is ready"
        );
        // Once ready, the same call enqueues.
        set_audio_ready(1);
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.5)], -1);
        assert_eq!(SND_QUEUE.with(|q| q.borrow().len()), 1);
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0); // restore default for other tests
    }

    #[test]
    fn queue_sounds_resolves_bare_sample_to_sound_dir() {
        // A QuakeC sample name is bare ("weapons/guncock.wav"); the pak stores it
        // under "sound/". queue_sounds must prepend "sound/" or the lookup misses
        // and the sound is dropped (the long-standing "no in-game sound" bug).
        let pak = build_test_pak(&[("sound/weapons/guncock.wav", b"GUNC")]);
        reset_queue();
        queue_sounds(&pak, &[ev(7, 1, "weapons/guncock.wav", 0.8)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 1, "bare sample name must resolve under sound/");
        assert_eq!(got[0].0, 0.8);
        // A name with NO matching pak entry (even under sound/) queues nothing.
        reset_queue();
        queue_sounds(&pak, &[ev(7, 1, "weapons/nope.wav", 0.5)], -1);
        assert!(drain_queue().is_empty(), "missing sample is dropped, no panic");
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0);
    }

    // ------------------------------------------------ ambient sounds (H11)

    /// A minimal PCM mono 11025 Hz 8-bit WAV; `loop_start = Some(n)` adds the
    /// `cue ` chunk the ambience samples carry (the loop gate `S_StaticSound`
    /// checks before agreeing to static-loop a sound).
    fn test_wav(loop_start: Option<u32>, data_samples: u32) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend(b"RIFF");
        b.extend(0u32.to_le_bytes());
        b.extend(b"WAVE");
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(1u16.to_le_bytes()); // mono
        b.extend(11025u32.to_le_bytes()); // rate
        b.extend(11025u32.to_le_bytes()); // byte rate
        b.extend(1u16.to_le_bytes()); // block align
        b.extend(8u16.to_le_bytes()); // bits
        if let Some(ls) = loop_start {
            b.extend(b"cue ");
            b.extend(28u32.to_le_bytes());
            b.extend(1u32.to_le_bytes()); // one cue point
            b.extend([0u8; 16]); // id/position/chunkid/chunkstart
            b.extend(0u32.to_le_bytes()); // block start
            b.extend(ls.to_le_bytes()); // sample offset = loop start
        }
        b.extend(b"data");
        b.extend(data_samples.to_le_bytes());
        b.extend(std::iter::repeat_n(0x80u8, data_samples as usize));
        b
    }

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

    #[test]
    fn boot_walk_queues_static_loops_and_demo_boot_bumps_generation() {
        // boot() (live e1m1) must hand the page the level's static loops via
        // poll_static_sound, each with sane spatial params + loop window; then
        // boot_demo() must bump the generation (the page's stop-all signal) and
        // replace the queue with the DEMO's own signon statics.
        assert_eq!(boot(), 1);
        let g0 = sound_generation();
        let mut n = 0;
        loop {
            let len = poll_static_sound();
            if len == 0 {
                break;
            }
            n += 1;
            let (ls, le) = (sound_loop_start(), sound_loop_end());
            assert!(ls >= 0.0, "loop start is a real offset");
            assert!(le > ls, "loop end past loop start");
            assert!(sound_volume() > 0.0 && sound_volume() <= 1.0);
            assert!(sound_attenuation() > 0.0 && sound_attenuation() <= 4.0);
            assert_eq!(sound_is_view_entity(), 0, "statics are world-placed");
        }
        assert!(n >= 5, "e1m1 queues its torch/ambience loops (got {n})");

        // Switching to the attract demo stops the walk's loops (generation
        // bump) and registers the demo signon's own statics.
        assert_eq!(boot_demo(), 1);
        assert_ne!(sound_generation(), g0, "mode transition bumps generation");
        assert!(
            poll_static_sound() > 0,
            "the demo's svc_spawnstaticsound loops are queued"
        );
    }

    #[test]
    fn load_ambient_sound_loads_only_water_and_wind() {
        // S_Init loads exactly ambience/water1.wav (ch 0) + ambience/wind2.wav
        // (ch 1); slime/lava keep a NULL sfx. Both real samples carry cue loop
        // chunks, so the loop window must come back usable.
        let l0 = load_ambient_sound(0);
        assert!(l0 > 0, "water1.wav loaded from the pak");
        assert!(sound_loop_end() > 0.0, "water1 loop window parsed");
        let l1 = load_ambient_sound(1);
        assert!(l1 > 0, "wind2.wav loaded from the pak");
        assert!(sound_loop_end() > 0.0, "wind2 loop window parsed");
        assert_eq!(load_ambient_sound(2), 0, "slime: WinQuake never loads one");
        assert_eq!(load_ambient_sound(3), 0, "lava: WinQuake never loads one");
        assert_eq!(load_ambient_sound(4), 0, "out of range");
        assert_eq!(load_ambient_sound(-1), 0, "negative channel");
    }

    #[test]
    fn ambient_gain_serves_frame_volumes_and_resets_on_generation_bump() {
        AMBIENT.with(|a| *a.borrow_mut() = AmbientChannels::new());
        // 36 host frames (1/72 s each) toward a full-water leaf: the C's
        // integer ramp climbs +1 per step -> master_vol 36.
        for _ in 0..36 {
            ramp_ambient_channels(Some(&[255, 0, 0, 0]), 1.0 / 72.0);
        }
        assert!((ambient_gain(0) - 36.0 / 255.0).abs() < 1e-6, "ramped gain");
        assert_eq!(ambient_gain(1), 0.0, "silent channel");
        assert_eq!(ambient_gain(99), 0.0, "out of range");
        assert_eq!(ambient_gain(-1), 0.0, "negative channel");
        // Outside the world (the C's `!l` branch): this frame is SILENT on the
        // page even though master_vol is preserved — ambient_gain must serve
        // update()'s returned frame volumes, not the raw ramp state.
        ramp_ambient_channels(None, 1.0 / 72.0);
        assert_eq!(ambient_gain(0), 0.0, "no leaf = silence NOW");
        // Re-entering the world resumes the ramp from the preserved value.
        ramp_ambient_channels(Some(&[255, 0, 0, 0]), 1.0 / 72.0);
        assert!((ambient_gain(0) - 37.0 / 255.0).abs() < 1e-6, "ramp resumed");
        // A level change (S_StopAllSounds) resets ramp AND frame volumes.
        bump_sound_generation();
        assert_eq!(ambient_gain(0), 0.0, "generation bump resets the ramp");
    }

    #[test]
    fn queue_static_sounds_drops_unlooped_and_missing_samples() {
        // S_StaticSound's gates: a sample with no cue loop point is REFUSED
        // ("Sound %s not looped"), a missing file is dropped, and names resolve
        // under the pak's "sound/" directory. Only the looped one queues.
        let looped = test_wav(Some(8), 64);
        let oneshot = test_wav(None, 64);
        let pak = build_test_pak(&[
            ("sound/amb/loopy.wav", looped.as_slice()),
            ("sound/amb/shot.wav", oneshot.as_slice()),
        ]);
        STATIC_QUEUE.with(|q| q.borrow_mut().clear());
        let mk = |sample: &str| StaticSound {
            origin: [1.0, 2.0, 3.0],
            sound_index: 1,
            sample: sample.to_string(),
            volume: 0.5,
            attenuation: 3.0,
        };
        queue_static_sounds(
            &pak,
            &[mk("amb/loopy.wav"), mk("amb/shot.wav"), mk("amb/missing.wav"), mk("")],
        );
        let len = poll_static_sound();
        assert_eq!(len, looped.len() as i32, "the looped sample queued");
        assert_eq!(sound_origin_x(), 1.0);
        assert_eq!(sound_origin_y(), 2.0);
        assert_eq!(sound_origin_z(), 3.0);
        assert_eq!(sound_volume(), 0.5);
        assert_eq!(sound_attenuation(), 3.0);
        // Loop window in seconds: start 8/11025, end 64/11025.
        assert!((sound_loop_start() - 8.0 / 11025.0).abs() < 1e-7);
        assert!((sound_loop_end() - 64.0 / 11025.0).abs() < 1e-7);
        assert_eq!(
            poll_static_sound(),
            0,
            "unlooped / missing / empty-name samples all dropped"
        );
    }

    #[test]
    fn queue_static_sounds_cap_and_slot_burning_match_the_c() {
        // S_StaticSound's budget is 116 (MAX_CHANNELS 128 minus the 12
        // channels total_channels starts at), and `total_channels++` happens
        // BEFORE the load/loop checks — a registration that then fails burns
        // its slot. The C's `if (!sfx) return` (empty name) precedes the slot
        // grab and burns nothing.
        let looped = test_wav(Some(8), 64);
        let oneshot = test_wav(None, 64);
        let pak = build_test_pak(&[
            ("sound/amb/loopy.wav", looped.as_slice()),
            ("sound/amb/shot.wav", oneshot.as_slice()),
        ]);
        let mk = |sample: &str| StaticSound {
            origin: [0.0; 3],
            sound_index: 1,
            sample: sample.to_string(),
            volume: 0.5,
            attenuation: 3.0,
        };
        // 3 empty names (no slot), 4 unlooped (slot burned, dropped), then 116
        // looped: only 112 slots remain for them.
        let mut statics = vec![mk(""), mk(""), mk("")];
        statics.extend(std::iter::repeat_with(|| mk("amb/shot.wav")).take(4));
        statics.extend(std::iter::repeat_with(|| mk("amb/loopy.wav")).take(116));
        STATIC_QUEUE.with(|q| q.borrow_mut().clear());
        queue_static_sounds(&pak, &statics);
        let mut queued = 0;
        while poll_static_sound() > 0 {
            queued += 1;
        }
        assert_eq!(
            queued,
            MAX_STATIC_SOUNDS - 4,
            "4 burned slots leave 112 of the 116 for real loops"
        );
    }

    /// Drain the engine-side menu-sound queue completely (returns the drained
    /// WAV payload lengths, in order).
    fn drain_menu_sounds() -> Vec<i32> {
        let mut out = Vec::new();
        loop {
            let len = poll_menu_sound();
            if len <= 0 {
                break;
            }
            out.push(len);
        }
        out
    }

    #[test]
    fn poll_menu_sound_serves_the_real_wavs_and_respects_audio_gate() {
        assert_eq!(boot_attract(), 1);
        crate::menu::menu_cancel(); // Escape: the menu over the demo
        set_audio_ready(1);
        MENU_SND_QUEUE.with(|q| q.borrow_mut().clear());
        drain_menu_sounds(); // flush whatever the open queued (menu2)

        // A cursor move queues misc/menu1.wav — the exact pak bytes.
        menu_down();
        let len = poll_menu_sound();
        assert!(len > 0, "menu navigation queues a local sound");
        let served = SND.with(|s| s.borrow().clone());
        let pak = pak().unwrap();
        let menu1 = pak.read_file("sound/misc/menu1.wav").unwrap().unwrap();
        assert_eq!(served, menu1, "cursor move serves misc/menu1.wav byte-for-byte");
        assert_eq!(poll_menu_sound(), 0, "queue drained");

        // Entering a submenu queues misc/menu2.wav (m_entersound).
        menu_up(); // back onto item 0
        drain_menu_sounds();
        menu_select(); // -> SinglePlayer
        let len = poll_menu_sound();
        assert!(len > 0);
        let served = SND.with(|s| s.borrow().clone());
        let menu2 = pak.read_file("sound/misc/menu2.wav").unwrap().unwrap();
        assert_eq!(served, menu2, "Enter serves misc/menu2.wav");
        drain_menu_sounds();

        // While audio isn't ready, queued menu sounds are DISCARDED (the
        // queue_sounds no-backlog rule), not saved up.
        set_audio_ready(0);
        menu_down();
        assert_eq!(poll_menu_sound(), 0, "no sound while audio is down");
        set_audio_ready(1);
        assert_eq!(poll_menu_sound(), 0, "pre-audio sounds were dropped, not queued");
    }
}
