//! The sound bridge — the client half of WinQuake's `snd_dma.c`, re-homed for
//! Web Audio: the one-shot queue (`S_StartSound` with `SND_PickChannel`'s
//! `(entity, channel)` override and `SND_Spatialize`'s view-entity rule),
//! `svc_stopsound`, the placed static loops (`S_StaticSound`), the automatic
//! leaf ambients (`S_UpdateAmbientSounds`), the menu's `S_LocalSound`s, and
//! the exports the page polls to play them. The page does the mixing.

use std::cell::RefCell;

use quake_rs::bsp::{Bsp, NUM_AMBIENTS};
use quake_rs::pak::Pak;
use quake_rs::render::{self, MenuSound};
use quake_rs::server::StaticSound;
use quake_rs::snd::{
    wav_info, AmbientChannels, AMBIENT_FADE_DEFAULT, AMBIENT_LEVEL_DEFAULT, AMBIENT_SAMPLES,
};

use crate::app::{ensure_app, pak, APP};

// --- sound: hand real Quake .wav bytes out of the pak for the page to play ---

/// Spatial parameters for one queued sound: its world emission point, volume
/// (`0.0..=1.0`) and attenuation (`0.0..=4.0`, where 0 = audible everywhere).
#[derive(Clone, Copy)]
pub(crate) struct SndParams {
    origin: [f32; 3],
    volume: f32,
    attenuation: f32,
    /// True when this sound came from the listener's own view entity (the
    /// player edict). The C `SND_Spatialize` (snd_dma.c:407-412) forces such
    /// sounds to full master volume on both channels with NO distance falloff
    /// or pan; the page reads this via `sound_is_view_entity` to skip its
    /// spatial attenuation for player-local sounds (weapon fire, pain, etc.).
    is_view_entity: bool,
    /// The emitting entity + channel (`SND_PickChannel`'s override key). The
    /// page reads these via `sound_entity`/`sound_channel` to keep a registry
    /// of PLAYING sources per `(entity, channel)`, so a NEW sound on a
    /// non-zero channel STOPS the source it overrides (the C "always override
    /// sound from same entity" — channel 0 never overrides), and an
    /// `svc_stopsound` can stop the keyed source (S_StopSound).
    entity: i32,
    channel: i32,
    /// The sample's `cue ` loop point in SECONDS (`GetWavinfo`'s `loopstart`
    /// over the rate), or -1.0 for a one-shot (`loopstart == -1`). The C mixer
    /// loops ANY channel whose sample has one (`SND_PaintChannels`: at the end
    /// `if (sc->loopstart >= 0) ch->pos = sc->loopstart`) until another sound
    /// takes the same (entity, channel) or all sounds stop: the door, lift and
    /// train "moving" hums.
    loop_start: f32,
    /// Loop end in seconds (`info.samples` over the rate).
    loop_end: f32,
}

/// Fill `p`'s loop window from the sample's `cue ` chunk (`GetWavinfo`).
fn set_loop_window(p: &mut SndParams, bytes: &[u8]) {
    let info = wav_info(bytes);
    (p.loop_start, p.loop_end) = match info {
        Some(i) if i.loop_start.is_some() => {
            let rate = i.rate.max(1) as f32;
            (i.loop_start.unwrap_or(0) as f32 / rate, i.samples as f32 / rate)
        }
        _ => (-1.0, 0.0),
    };
}

impl SndParams {
    /// The emitting entity (tests read what `sound_entity` hands the page).
    #[cfg(test)]
    pub(crate) fn entity(&self) -> i32 {
        self.entity
    }

    const fn zero() -> Self {
        SndParams {
            origin: [0.0; 3],
            volume: 0.0,
            attenuation: 0.0,
            is_view_entity: false,
            entity: 0,
            channel: 0,
            loop_start: -1.0,
            loop_end: 0.0,
        }
    }
}

thread_local! {
    /// Scratch buffer the page reads via `sound_ptr` (for both the demo button
    /// and the per-frame queue below).
    static SND: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// WAV byte payloads for sounds fired this frame, awaiting playback, each
    /// paired with the spatial params the page reads to position it.
    pub(crate) static SND_QUEUE: RefCell<Vec<(Vec<u8>, SndParams)>> = const { RefCell::new(Vec::new()) };
    /// Spatial params of the entry the most recent `poll_sound` popped — the
    /// page reads these via the `sound_origin_*`/`sound_volume`/`sound_attenuation`
    /// exports after each non-zero `poll_sound`.
    static SND_CUR: RefCell<SndParams> = const { RefCell::new(SndParams::zero()) };
    /// The current listener pose, refreshed every walk `step`: eye position plus
    /// the forward and right unit vectors derived from the player's yaw. The page
    /// reads these via `listener_*` exports to spatialize each sound.
    pub(crate) static LISTENER: RefCell<Listener> = const { RefCell::new(Listener::zero()) };
    /// Whether the page's `AudioContext` is running yet. The page calls
    /// `set_audio_ready(1)` once the context resumes (it starts suspended until a
    /// user gesture). Until then `queue_sounds` drops sounds on the floor instead
    /// of appending them every frame up to the 12-cap — otherwise a backlog of
    /// stale sounds from before audio started would all play at once when it does.
    static AUDIO_READY: RefCell<bool> = const { RefCell::new(false) };
    /// Pending menu `S_LocalSound`s (menu1/menu2/menu3), drained from the menu
    /// by [`poll_menu_sound`]. Only fills while audio is ready (same
    /// no-backlog rule as `queue_sounds`).
    static MENU_SND_QUEUE: RefCell<Vec<MenuSound>> = const { RefCell::new(Vec::new()) };
    /// The three menu WAV payloads, loaded from the pak once on first use and
    /// cached (keyed [menu1, menu2, menu3]); `None` = not yet tried.
    static MENU_WAVS: RefCell<[Option<Vec<u8>>; 3]> = const { RefCell::new([None, None, None]) };
}

/// Page hook (LOW-10): set once the browser `AudioContext` has resumed to the
/// `running` state. While this is `0` the per-frame sound queue is not filled,
/// so no pre-audio backlog accumulates to flush when playback finally starts.
#[no_mangle]
pub extern "C" fn set_audio_ready(ready: i32) {
    AUDIO_READY.with(|r| *r.borrow_mut() = ready != 0);
}

/// Listener pose the page reads to spatialize queued sounds.
#[derive(Clone, Copy)]
pub(crate) struct Listener {
    pub(crate) pos: [f32; 3],
    pub(crate) forward: [f32; 3],
    pub(crate) right: [f32; 3],
}

impl Listener {
    const fn zero() -> Self {
        Listener { pos: [0.0; 3], forward: [0.0; 3], right: [0.0; 3] }
    }
}

/// Load the WAV bytes for the gameplay sounds in `events` and push them onto the
/// playback queue, capped at 12 so a noisy frame can't grow it without bound.
///
/// A sample with a `cue ` loop point carries its loop window
/// ([`SndParams::loop_start`]) and the page LOOPS it, as `SND_PaintChannels`
/// does, until a later sound on the same (entity, channel) overrides it: the
/// door/lift/train "moving" samples (`doors/doormv1`, `hydro1`, `stndr1`,
/// `plats/plat1`, `medplat1`, `train1`, ...) hum until their stop sound. The
/// silent `misc/null.wav` is queued like any other sample, since it too
/// overrides its (entity, channel).
///
/// `ambience/*` one-shots are real gameplay content and queue like any other
/// sample: trigger_push wind tunnels fire `sound (other, CHAN_AUTO,
/// "ambience/windfly.wav", 1, ATTN_NORM)` (QuakeC `trigger_push_touch`, heard
/// on E1M6); windfly has no cue chunk, so it plays once.
///
/// Mixing follows the C `SND_PickChannel` (snd_dma.c:354-390), keyed on the
/// `(entity, channel)` pair carried by each `SoundEvent` — NOT on the sample
/// name. A repeat `(entity, channel)` with a non-zero channel RESTARTS that
/// channel (the later event overrides the earlier queued one for that key,
/// matching "always override sound from same entity"); `channel == -1` matches
/// any channel of that entity. Channel 0 NEVER overrides (the C comment:
/// "channel 0 never overrides") so every channel-0 emitter queues separately.
/// This keeps two distinct emitters of the SAME sample (e.g. two doors, or a
/// gunshot and a footstep) from collapsing into one and losing the other's
/// origin/volume.
///
/// `view_entity` is the listener's own edict (the player): a sound from it is
/// flagged so the page plays it at full volume with no falloff (see
/// `SndParams::is_view_entity`).
pub(crate) fn queue_sounds(pak: &Pak, events: &[quake_rs::server::SoundEvent], view_entity: i32) {
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
    // Queue indices of entries already placed this call, keyed by their
    // (entity, channel) — only for non-zero channels (channel 0 never
    // overrides, so it is never recorded here and always appends).
    let mut placed: Vec<((i32, i32), usize)> = Vec::new();
    SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for ev in events {
            let name = ev.sample.as_str();
            if name.is_empty() {
                continue;
            }

            // QuakeC sample names are relative to the "sound/" directory (the C
            // `S_LoadSound` does sprintf(buf, "sound/%s", name)); the pak stores
            // them under that prefix. Without it every read_file misses and the
            // sound is silently dropped — the long-standing "no in-game sound".
            let path = format!("sound/{name}");

            let mut params = SndParams {
                origin: ev.origin,
                volume: ev.volume,
                attenuation: ev.attenuation,
                is_view_entity: ev.entity == view_entity,
                entity: ev.entity,
                channel: ev.channel,
                loop_start: -1.0,
                loop_end: 0.0,
            };

            // Channel restart (SND_PickChannel): a non-zero channel from the
            // same entity overrides that entity's prior queued entry on the same
            // channel, so the channel plays the latest sound, not a stale one.
            // The C wildcard is `entchannel == -1` on the NEW event only; QuakeC's
            // SV_StartSound emit path always carries a concrete channel 0..7
            // (the -1 "any" form is only used to STOP sounds, never emitted here),
            // so we match the C's predicate exactly: new-channel -1 is a wildcard.
            if ev.channel != 0 {
                let hit = placed.iter().position(|&((e, c), _)| {
                    e == ev.entity && (c == ev.channel || ev.channel == -1)
                });
                if let Some(pi) = hit {
                    let qi = placed[pi].1;
                    if let Ok(Some(bytes)) = pak.read_file(&path) {
                        set_loop_window(&mut params, &bytes);
                        q[qi] = (bytes, params);
                        // Re-key to this channel so a following -1 still matches.
                        placed[pi].0 = (ev.entity, ev.channel);
                    }
                    continue;
                }
            }

            if q.len() >= 12 {
                continue; // queue cap reached
            }
            if let Ok(Some(bytes)) = pak.read_file(&path) {
                set_loop_window(&mut params, &bytes);
                let qi = q.len();
                q.push((bytes, params));
                if ev.channel != 0 {
                    placed.push(((ev.entity, ev.channel), qi));
                }
            }
        }
    });
}

/// Pop the next queued sound into the scratch buffer and return its byte length
/// (0 when the queue is empty). The page calls this in a loop each frame, reads
/// `sound_ptr()` after each non-zero return, and plays it via Web Audio. The
/// popped entry's spatial params are stashed for the `sound_origin_*` /
/// `sound_volume` / `sound_attenuation` exports to read alongside the bytes, and
/// its loop window for `sound_loop_start`/`sound_loop_end` (start -1.0 = a
/// one-shot; otherwise the page loops the source from there).
#[no_mangle]
pub extern "C" fn poll_sound() -> i32 {
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

/// Pop the next queued MENU sound (menu.c's `S_LocalSound` triggers: menu1 on
/// cursor moves, menu2 on enter/select, menu3 on slider adjusts) into the
/// shared sound scratch and return its WAV byte length (0 when none). The page
/// polls this each frame alongside [`poll_sound`] and plays the bytes per
/// `S_LocalSound` semantics (snd_dma.c: `S_StartSound(cl.viewentity, -1, sfx,
/// vec3_origin, 1, 1)` — full volume, centred, no distance falloff; the page's
/// master volume still scales it, like the C mixer's `volume.value`). Works in
/// EVERY mode (the menu overlays the attract demo too). While the page hasn't
/// reported audio running ([`set_audio_ready`]), queued menu sounds are
/// discarded instead — the same no-backlog rule as `queue_sounds`.
#[no_mangle]
pub extern "C" fn poll_menu_sound() -> i32 {
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
        return 0;
    }
    let next = MENU_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    let Some(snd) = next else { return 0 };
    let slot = match snd {
        MenuSound::Menu1 => 0,
        MenuSound::Menu2 => 1,
        MenuSound::Menu3 => 2,
    };
    // Load-once cache: the C's S_PrecacheSound holds these three resident.
    let bytes = MENU_WAVS.with(|w| {
        let mut w = w.borrow_mut();
        if w[slot].is_none() {
            // S_LoadSound: sprintf(namebuffer, "sound/%s", s->name).
            w[slot] = pak()
                .and_then(|p| p.read_file(&format!("sound/{}", snd.sample())).ok().flatten());
        }
        w[slot].clone()
    });
    match bytes {
        Some(b) => {
            let len = b.len() as i32;
            SND.with(|s| *s.borrow_mut() = b);
            len
        }
        None => 0,
    }
}

/// Spatial params of the entry the most recent `poll_sound` popped. `origin_*`
/// are the world emission point; `volume` is `0.0..=1.0`; `attenuation` is
/// `0.0..=4.0` (0 = no falloff, audible everywhere). The page reads these after
/// each non-zero `poll_sound` to compute distance gain and stereo pan.
#[no_mangle]
pub extern "C" fn sound_origin_x() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[0])
}
#[no_mangle]
pub extern "C" fn sound_origin_y() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[1])
}
#[no_mangle]
pub extern "C" fn sound_origin_z() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[2])
}
#[no_mangle]
pub extern "C" fn sound_volume() -> f32 {
    SND_CUR.with(|p| p.borrow().volume)
}
#[no_mangle]
pub extern "C" fn sound_attenuation() -> f32 {
    SND_CUR.with(|p| p.borrow().attenuation)
}

/// `1` when the entry the most recent `poll_sound` popped came from the
/// listener's own view entity (the player edict), else `0`. The C
/// `SND_Spatialize` (snd_dma.c:407-412) forces view-entity sounds to full
/// master volume on both channels with no distance falloff or pan; the page
/// reads this to take the same full-volume / centred path for the player's own
/// sounds (weapon fire, pain) instead of attenuating them with distance.
#[no_mangle]
pub extern "C" fn sound_is_view_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().is_view_entity as i32)
}

/// The emitting entity of the most recent `poll_sound` pop. With
/// [`sound_channel`] this is the `SND_PickChannel` override key: the page
/// keeps its playing one-shot sources in a registry keyed `(entity, channel)`
/// so a later sound on the same non-zero channel STOPS the source it replaces
/// (snd_dma.c: "always override sound from same entity"), and an
/// `svc_stopsound` can stop it (S_StopSound).
#[no_mangle]
pub extern "C" fn sound_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().entity)
}
/// The channel (0..=7) of the most recent `poll_sound` pop; 0 = CHAN_AUTO,
/// which never overrides and is never stopped by key.
#[no_mangle]
pub extern "C" fn sound_channel() -> i32 {
    SND_CUR.with(|p| p.borrow().channel)
}

thread_local! {
    /// Pending `svc_stopsound` stops, packed as the wire short `(entity << 3)
    /// | channel` (S_StopSound's arguments). Pushed by demo playback (the only
    /// current producer — live play's QuakeC stops loops by playing
    /// `misc/null.wav` on the same channel, which the override path handles);
    /// drained by the page via [`poll_stop_sound`] each frame. NOTE: id's own
    /// demo1/2/3 never send svc_stopsound (asserted by a wasm test), so for
    /// the shipped attract loop this stays empty — the plumbing exists for
    /// protocol completeness.
    pub(crate) static STOP_SND_QUEUE: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// Queue `(entity, channel)` stops for the page (see [`STOP_SND_QUEUE`]).
pub(crate) fn push_stop_sounds(stops: &[(i32, i32)]) {
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

/// Pop the next pending sound STOP as the packed `(entity << 3) | channel`
/// short (`S_StopSound(i >> 3, i & 7)`), or `-1` when none are pending. The
/// page calls this in a loop each frame and `stop()`s the registered source
/// for that `(entity, channel)` key — the Web Audio equivalent of the C
/// zeroing the channel's sfx.
#[no_mangle]
pub extern "C" fn poll_stop_sound() -> i32 {
    STOP_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            -1
        } else {
            q.remove(0)
        }
    })
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

/// One registered static (looping) sound awaiting pickup by the page: the WAV
/// bytes, its spatial params, and the loop window `GetWavinfo` found (seconds;
/// `loop_end` 0.0 = loop to the buffer's end, Web Audio's `loopEnd` default).
struct StaticLoop {
    bytes: Vec<u8>,
    params: SndParams,
    loop_start: f32,
    loop_end: f32,
}

/// The C's effective static-sound budget. `S_StaticSound` (snd_dma.c) refuses
/// at `total_channels == MAX_CHANNELS` (128), but `total_channels` starts at
/// `MAX_DYNAMIC_CHANNELS + NUM_AMBIENTS` = 8 + 4 = 12 (`S_Init`), so at most
/// 116 statics ever fit — and `total_channels++` happens BEFORE the
/// load/loop checks, so a registration that then FAILS (missing or unlooped
/// sample) still burns its slot (see [`queue_static_sounds`]).
const MAX_STATIC_SOUNDS: usize = 128 - (8 + 4);

thread_local! {
    /// Static sounds registered by the CURRENT level, awaiting page pickup via
    /// `poll_static_sound`. NOT gated on `AUDIO_READY` (unlike the one-shot
    /// queue): these are persistent registrations, not a backlog — the page
    /// starts the loops whenever its AudioContext comes up.
    static STATIC_QUEUE: RefCell<Vec<StaticLoop>> = const { RefCell::new(Vec::new()) };
    /// Loop window of the entry the most recent `poll_static_sound` popped (or
    /// `load_ambient_sound` loaded), for the `sound_loop_start`/`sound_loop_end`
    /// exports. `(0, 0)` = loop the whole buffer.
    static SND_LOOP: RefCell<(f32, f32)> = const { RefCell::new((0.0, 0.0)) };
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
pub(crate) fn bump_sound_generation() {
    SOUND_GENERATION.with(|g| {
        let mut g = g.borrow_mut();
        *g = g.wrapping_add(1);
    });
    STATIC_QUEUE.with(|q| q.borrow_mut().clear());
    AMBIENT.with(|a| *a.borrow_mut() = AmbientChannels::new());
    AMBIENT_VOLS.with(|v| *v.borrow_mut() = [0.0; NUM_AMBIENTS]);
}

/// Load the WAV bytes for each placed static sound and queue them for the
/// page's loop pickup — the `S_StaticSound` (snd_dma.c:620) gate: a sample the
/// pak lacks is dropped, and so is one with no loop point (`sc->loopstart ==
/// -1` -> "Sound %s not looped"). Quake's ambient samples all carry a `cue `
/// loop chunk; one-shots don't, and the C refuses to static-loop them.
pub(crate) fn queue_static_sounds(pak: &Pak, statics: &[StaticSound]) {
    STATIC_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        // The statics' share of the C channel table (`total_channels - 12`).
        // Local to the call: `S_StopAllSounds` resets `total_channels` on
        // every level change, and a level registers its statics exactly once
        // (one drain -> one call, right after the generation bump).
        let mut slots = 0usize;
        for s in statics {
            if slots >= MAX_STATIC_SOUNDS {
                break; // the C's "total_channels == MAX_CHANNELS" refusal
            }
            if s.sample.is_empty() {
                continue; // the C's `if (!sfx) return` — before the slot grab
            }
            // `total_channels++` precedes S_LoadSound and the loop check in
            // the C, so each of the drops below still burns its slot.
            slots += 1;
            // Sample names are relative to "sound/" (S_LoadSound's sprintf),
            // exactly like the one-shot path in `queue_sounds`.
            let path = format!("sound/{}", s.sample);
            let Ok(Some(bytes)) = pak.read_file(&path) else {
                continue;
            };
            let Some(info) = wav_info(&bytes) else {
                continue;
            };
            let Some(loop_start) = info.loop_start else {
                continue; // "Sound %s not looped" — never static-loop a one-shot
            };
            let rate = info.rate.max(1) as f32;
            let (loop_start, loop_end) = (loop_start as f32 / rate, info.samples as f32 / rate);
            q.push(StaticLoop {
                bytes,
                params: SndParams {
                    origin: s.origin,
                    volume: s.volume,
                    attenuation: s.attenuation,
                    is_view_entity: false, // statics are placed in the world
                    entity: 0,             // statics carry no override key
                    channel: 0,
                    loop_start,
                    loop_end,
                },
                loop_start,
                loop_end,
            });
        }
    });
}

/// The current sound generation. The page reads this every frame; when it
/// changes, every looping source (static + ambient) is stopped and rebuilt
/// from the new level's registrations (see [`bump_sound_generation`]).
#[no_mangle]
pub extern "C" fn sound_generation() -> i32 {
    SOUND_GENERATION.with(|g| *g.borrow())
}

/// Pop the next registered static (looping) sound into the scratch buffer and
/// return its byte length (0 when none are pending). Mirrors `poll_sound`: the
/// page reads the bytes via `sound_ptr()` and the spatial params via
/// `sound_origin_*`/`sound_volume`/`sound_attenuation` (stashed exactly like a
/// one-shot pop), plus the loop window via `sound_loop_start`/`sound_loop_end`
/// — then starts a LOOPING source it re-spatializes every frame.
#[no_mangle]
pub extern "C" fn poll_static_sound() -> i32 {
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
#[no_mangle]
pub extern "C" fn sound_loop_start() -> f32 {
    SND_LOOP.with(|l| l.borrow().0)
}

/// Loop end in seconds of the most recent `poll_sound`/`poll_static_sound`/
/// `load_ambient_sound` (`GetWavinfo`'s `info.samples` over the rate; this is
/// the full data length unless a `LIST`/`mark` chunk declared a shorter loop).
/// 0.0 means "to the buffer's end" — Web Audio's `loopEnd` default.
#[no_mangle]
pub extern "C" fn sound_loop_end() -> f32 {
    SND_LOOP.with(|l| l.borrow().1)
}

/// Load ambient channel `ch`'s sample (`S_Init`: 0 = `ambience/water1.wav`,
/// 1 = `ambience/wind2.wav`) into the scratch buffer, returning its byte
/// length; the loop window lands in `sound_loop_start`/`sound_loop_end` like a
/// static pop. Returns 0 for channels the C never loaded (2 = slime, 3 = lava
/// have a NULL `ambient_sfx`) and for out-of-range/missing samples. The page
/// calls this once per audible channel, starts a centred looping source at
/// gain 0, and drives the gain from `ambient_gain` every frame.
#[no_mangle]
pub extern "C" fn load_ambient_sound(ch: i32) -> i32 {
    let Some(Some(name)) = usize::try_from(ch)
        .ok()
        .and_then(|c| AMBIENT_SAMPLES.get(c).copied().map(Some))
    else {
        return 0;
    };
    let Some(name) = name else { return 0 };
    let Some(p) = pak() else { return 0 };
    let Ok(Some(bytes)) = p.read_file(&format!("sound/{name}")) else {
        return 0;
    };
    let Some(info) = wav_info(&bytes) else { return 0 };
    let rate = info.rate.max(1) as f32;
    SND_LOOP.with(|l| {
        *l.borrow_mut() = (
            info.loop_start.unwrap_or(0) as f32 / rate,
            info.samples as f32 / rate,
        )
    });
    let len = bytes.len() as i32;
    SND.with(|s| *s.borrow_mut() = bytes);
    len
}

/// Ambient channel `ch`'s volume THIS frame in `0.0..=1.0` (the value
/// [`AmbientChannels::update`] returned, over the C's 255 scale — see
/// [`AMBIENT_VOLS`]). The page multiplies by its master volume and writes it
/// to the channel's gain node every frame — both sides of the C's
/// `chan->leftvol = chan->rightvol = chan->master_vol` (ambients are centred,
/// never panned or distance-attenuated). 0.0 on the frames the C silenced
/// outright (listener outside the world, `ambient_level` 0).
#[no_mangle]
pub extern "C" fn ambient_gain(ch: i32) -> f32 {
    let Ok(c) = usize::try_from(ch) else { return 0.0 };
    AMBIENT_VOLS.with(|v| v.borrow().get(c).copied().unwrap_or(0.0)) / 255.0
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

/// One frame of `S_UpdateAmbientSounds` for the listener standing at `eye` in
/// `bsp`: look up the view leaf and ramp the four ambient channels toward its
/// `ambient_level[]` targets. Called from both `step_walk` and `step_demo`
/// (the C runs it from `S_Update` regardless of game/demo mode). A listener
/// outside the world (no leaf) silences the channels without resetting the
/// ramp, exactly like the C's `!l` branch.
pub(crate) fn update_ambient_channels(bsp: &Bsp, eye: [f32; 3], dt: f32) {
    let frametime = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let leaf_levels = render::point_in_leaf(bsp, eye)
        .and_then(|li| bsp.leafs.get(li))
        .map(|l| l.ambient_level);
    ramp_ambient_channels(leaf_levels.as_ref(), frametime);
}

/// The listener (player) pose as of the last walk `step`: eye position and the
/// forward/right unit vectors derived from the player's yaw. The page reads
/// these to spatialize each sound (distance from `pos`, pan via dot with right).
#[no_mangle]
pub extern "C" fn listener_x() -> f32 {
    LISTENER.with(|l| l.borrow().pos[0])
}
#[no_mangle]
pub extern "C" fn listener_y() -> f32 {
    LISTENER.with(|l| l.borrow().pos[1])
}
#[no_mangle]
pub extern "C" fn listener_z() -> f32 {
    LISTENER.with(|l| l.borrow().pos[2])
}
#[no_mangle]
pub extern "C" fn listener_fwd_x() -> f32 {
    LISTENER.with(|l| l.borrow().forward[0])
}
#[no_mangle]
pub extern "C" fn listener_fwd_y() -> f32 {
    LISTENER.with(|l| l.borrow().forward[1])
}
#[no_mangle]
pub extern "C" fn listener_fwd_z() -> f32 {
    LISTENER.with(|l| l.borrow().forward[2])
}
#[no_mangle]
pub extern "C" fn listener_right_x() -> f32 {
    LISTENER.with(|l| l.borrow().right[0])
}
#[no_mangle]
pub extern "C" fn listener_right_y() -> f32 {
    LISTENER.with(|l| l.borrow().right[1])
}
#[no_mangle]
pub extern "C" fn listener_right_z() -> f32 {
    LISTENER.with(|l| l.borrow().right[2])
}

/// Load a recognisable Quake SFX (item pickup) from the pak into a buffer once,
/// returning its byte length. The bytes are a standard RIFF/WAV the browser's
/// `decodeAudioData` understands — this is real id sound data, read by our pak
/// loader, played in the page via Web Audio.
#[no_mangle]
pub extern "C" fn load_sound() -> i32 {
    SND.with(|s| {
        if s.borrow().is_empty() {
            if let Some(p) = pak() {
                if let Ok(Some(b)) = p.read_file("sound/items/r_item1.wav") {
                    *s.borrow_mut() = b;
                }
            }
        }
        s.borrow().len() as i32
    })
}

/// Pointer to the loaded sound bytes in linear memory (the page reads them out).
#[no_mangle]
pub extern "C" fn sound_ptr() -> *const u8 {
    SND.with(|s| s.borrow().as_ptr())
}

/// The Options "Volume" as a `0.0..=1.0` master gain (default 0.7). The page
/// scales its sound gains by this. Reads from the App-level menu; 1.0 when the app
/// has not been created yet (so audio is never accidentally silenced before then).
#[no_mangle]
pub extern "C" fn volume() -> f32 {
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
        let pak = crate::app::pak().expect("embedded pak");
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
        set_audio_ready(1);
        MENU_SND_QUEUE.with(|q| q.borrow_mut().clear());
        drain_menu_sounds(); // flush whatever boot queued (the open's menu2)

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
