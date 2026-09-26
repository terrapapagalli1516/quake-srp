//! The page's sound bridge: what reaches a platform that mixes for itself —
//! the browser today, one Web Audio node per sound. It predates the engine's
//! own mixer ([`super::dma::Mixer`]) and goes when the page plays the mixer's
//! PCM instead.
//!
//! Here: the four automatic per-leaf ambient channels' ramp
//! (`S_UpdateAmbientSounds`, [`AmbientChannels`]), `S_StartSound`'s channel
//! choice (`SND_PickChannel`'s `(entity, channel)` override and
//! `SND_Spatialize`'s view-entity rule, [`queue_sounds`]) and `S_StaticSound`'s
//! gates ([`queue_static_sounds`]), each filling a queue a platform plays.
//!
//! The mixing is the platform's (the audit's recorded departure: the page
//! mixes with Web Audio). What is kept faithful here is the *control* data:
//! which ambient channels exist, how each frame's target volume is derived
//! from the view leaf's `ambient_level[]` bytes, the exact `ambient_fade` ramp
//! toward that target, and which samples are loopable (the `cue ` chunk loop
//! start the C refused to static-loop without).

use super::mem::wav_info;
use crate::bsp::NUM_AMBIENTS;
use crate::pak::Pak;
use crate::server::{SoundEvent, StaticSound};

/// One WinQuake host frame at the engine's frame cap. `Host_FilterTime`
/// (host.c:505) refuses to run a frame until at least 1/72 s of real time has
/// passed, so `S_UpdateAmbientSounds` never saw a shorter `host_frametime`;
/// the ambient ramp integrates in these fixed steps (see [`AmbientChannels`]).
const HOST_FRAME_STEP: f32 = 1.0 / 72.0;

/// `Host_FilterTime`'s "don't allow really long [...] frames" clamp
/// (host.c:515): one C frame never integrated more than 0.1 s of ambient fade
/// no matter how long the real stall was. Applied to the step accumulator for
/// the same reason (a backgrounded tab must not fast-forward the ramp on
/// return).
const HOST_FRAME_MAX: f32 = 0.1;

/// The state of the four automatic ambient channels — the `master_vol` of
/// `channels[0..NUM_AMBIENTS]` that `S_UpdateAmbientSounds` (snd_dma.c:664)
/// ramps every frame. A front-end keeps one of these alive, calls
/// [`update`](AmbientChannels::update) once per frame with the VIEW leaf's
/// `ambient_level[]` bytes, and drives its looping sources' gains from the
/// returned volumes.
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientChannels {
    /// `channels[ch].master_vol`, 0..=255 — an `int` in the C (sound.h:79:
    /// `int master_vol; // 0-255 master volume`), so every ramp assignment in
    /// `S_UpdateAmbientSounds` truncates toward zero. That integer math is
    /// load-bearing: at the default `ambient_fade` 100 a 72fps step is
    /// 100/72 ≈ 1.39, so the up-ramp moves `trunc(mv + 1.39) = mv + 1` per
    /// step (~72 units/s) while the down-ramp moves `trunc(mv - 1.39) =
    /// mv - 2` (~144 units/s — fade-out twice as fast as fade-in), and the
    /// clamp-onto-target stores `trunc(vol)` (76 for a full-level water
    /// leaf's 0.3*255 = 76.5).
    master_vol: [i32; NUM_AMBIENTS],
    /// Real seconds not yet consumed by whole [`HOST_FRAME_STEP`] ramp steps.
    /// WinQuake only ever ran the ramp at `host_frametime >= 1/72` (the
    /// `Host_FilterTime` frame cap), where the integer up-step above is >= 1;
    /// a rAF-driven front-end can hand us shorter frames (144 Hz: step 0.69)
    /// where the C's literal `trunc(mv + 0.69) = mv` would stall the up-ramp
    /// FOREVER — a regime the original could never enter. So the ramp runs on
    /// a fixed 1/72 s timestep fed by this accumulator: "WinQuake as
    /// compiled, at its frame cap" at any display refresh rate.
    accum: f32,
}

impl Default for AmbientChannels {
    fn default() -> Self {
        Self::new()
    }
}

impl AmbientChannels {
    /// All channels silent — the post-`S_StopAllSounds` state (it memsets every
    /// channel, ambients included, on level change).
    pub const fn new() -> Self {
        AmbientChannels { master_vol: [0; NUM_AMBIENTS], accum: 0.0 }
    }

    /// One frame of `S_UpdateAmbientSounds` (snd_dma.c:664-711), run on a
    /// fixed 1/72 s timestep (see [`AmbientChannels::accum`]): `frametime` is
    /// the frame's REAL seconds, banked and consumed in whole
    /// [`HOST_FRAME_STEP`] steps, each applying the C's literal
    /// integer-`master_vol` math with `host_frametime = 1/72` — WinQuake at
    /// its `Host_FilterTime` frame cap.
    ///
    /// `leaf_levels` is the view leaf's `ambient_level[]` (None when the
    /// listener is outside the world — the C's `!l` case); `ambient_level` /
    /// `ambient_fade` are the cvar values
    /// ([`super::AMBIENT_LEVEL_DEFAULT`]/[`super::AMBIENT_FADE_DEFAULT`]).
    ///
    /// Returns each channel's volume for this frame on the C's 0..=255 scale
    /// (`chan->leftvol = chan->rightvol = chan->master_vol` — ambients are
    /// centred, never panned), whether or not a step fired this call. Per the
    /// C, each step:
    ///
    /// * targets `vol = ambient_level * leaf_levels[ch]`, floored to 0 when
    ///   below 8;
    /// * moves `master_vol` toward the target by `(1/72) * ambient_fade`
    ///   ("don't adjust volume too fast"), truncating into the int channel on
    ///   every store, and clamps onto `trunc(vol)`;
    /// * with no leaf (or `ambient_level` 0) every channel's sfx is unhooked —
    ///   silence NOW — but `master_vol` is NOT reset (the C leaves it; a
    ///   re-entered world resumes ramping from the old value).
    pub fn update(
        &mut self,
        leaf_levels: Option<&[u8; NUM_AMBIENTS]>,
        frametime: f32,
        ambient_level: f32,
        ambient_fade: f32,
    ) -> [f32; NUM_AMBIENTS] {
        // `if (!l || !ambient_level.value)`: kill the channels' sfx (silent
        // this frame) without touching master_vol (or the accumulator — these
        // C frames ramped nothing).
        let Some(levels) = leaf_levels else {
            return [0.0; NUM_AMBIENTS];
        };
        if ambient_level == 0.0 {
            return [0.0; NUM_AMBIENTS];
        }

        // Bank this frame's real time, bounded by Host_FilterTime's 0.1 s
        // frame clamp, then ramp once per whole 1/72 s step. At WinQuake's own
        // frame cap this fires exactly once per frame; at 144 Hz every other
        // frame; never more than 7 steps (0.1 s) at once.
        self.accum = (self.accum + frametime.max(0.0)).min(HOST_FRAME_MAX);
        while self.accum >= HOST_FRAME_STEP {
            self.accum -= HOST_FRAME_STEP;
            for (ch, mv) in self.master_vol.iter_mut().enumerate() {
                let mut vol = ambient_level * levels[ch] as f32;
                if vol < 8.0 {
                    vol = 0.0;
                }

                // don't adjust volume too fast
                //
                // The C's literal int math: `master_vol` is an int, so
                // `chan->master_vol += host_frametime * ambient_fade.value`
                // (and the clamp store `= vol`) truncate toward zero on every
                // assignment — `as i32` is exactly that truncation.
                if (*mv as f32) < vol {
                    *mv = (*mv as f32 + HOST_FRAME_STEP * ambient_fade) as i32;
                    if *mv as f32 > vol {
                        *mv = vol as i32;
                    }
                } else if *mv as f32 > vol {
                    *mv = (*mv as f32 - HOST_FRAME_STEP * ambient_fade) as i32;
                    if (*mv as f32) < vol {
                        *mv = vol as i32;
                    }
                }
            }
        }

        let mut out = [0.0; NUM_AMBIENTS];
        for (ch, mv) in self.master_vol.iter().enumerate() {
            out[ch] = *mv as f32;
        }
        out
    }

    /// A channel's current `master_vol` (0..=255, integral like the C's int
    /// field); 0.0 for an out-of-range channel.
    pub fn master_vol(&self, ch: usize) -> f32 {
        self.master_vol.get(ch).copied().unwrap_or(0) as f32
    }
}

// ---------------------------------------------------------------------------
// What reaches the mixer: S_StartSound's channels and S_StaticSound's loops
// ---------------------------------------------------------------------------

/// Spatial parameters for one queued sound: its world emission point, volume
/// (`0.0..=1.0`) and attenuation (`0.0..=4.0`, where 0 = audible everywhere).
#[derive(Clone, Copy, Debug)]
pub struct SndParams {
    pub origin: [f32; 3],
    pub volume: f32,
    pub attenuation: f32,
    /// True when this sound came from the listener's own view entity (the
    /// player edict). The C `SND_Spatialize` (snd_dma.c:407-412) forces such
    /// sounds to full master volume on both channels with NO distance falloff
    /// or pan; the page reads this via `sound_is_view_entity` to skip its
    /// spatial attenuation for player-local sounds (weapon fire, pain, etc.).
    pub is_view_entity: bool,
    /// The emitting entity + channel (`SND_PickChannel`'s override key). The
    /// page reads these via `sound_entity`/`sound_channel` to keep a registry
    /// of PLAYING sources per `(entity, channel)`, so a NEW sound on a
    /// non-zero channel STOPS the source it overrides (the C "always override
    /// sound from same entity" — channel 0 never overrides), and an
    /// `svc_stopsound` can stop the keyed source (S_StopSound).
    pub entity: i32,
    pub channel: i32,
    /// The sample's `cue ` loop point in SECONDS (`GetWavinfo`'s `loopstart`
    /// over the rate), or -1.0 for a one-shot (`loopstart == -1`). The C mixer
    /// loops ANY channel whose sample has one (`SND_PaintChannels`: at the end
    /// `if (sc->loopstart >= 0) ch->pos = sc->loopstart`) until another sound
    /// takes the same (entity, channel) or all sounds stop: the door, lift and
    /// train "moving" hums.
    pub loop_start: f32,
    /// Loop end in seconds (`info.samples` over the rate).
    pub loop_end: f32,
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
    /// The emitting entity (what the page's `sound_entity` reads).
    pub fn entity(&self) -> i32 {
        self.entity
    }

    /// Nothing queued yet: no sound, a one-shot.
    pub const fn zero() -> Self {
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

/// The one-shot queue's cap: the new sounds a frame may add (a non-zero
/// channel's go past it; see [`queue_sounds`]).
pub const QUEUE_CAP: usize = 12;

/// Load the WAV bytes for the gameplay sounds in `events` and push them onto the
/// playback queue `q`, capped at 12 so a noisy frame can't grow it without bound.
/// A sound on a non-zero channel still goes out past the cap (it overrides its
/// `(entity, channel)`, which the C never drops).
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
pub fn queue_sounds(q: &mut Vec<(Vec<u8>, SndParams)>, pak: &Pak, events: &[SoundEvent], view_entity: i32) {
    // Queue indices of entries already placed this call, keyed by their
    // (entity, channel) — only for non-zero channels (channel 0 never
    // overrides, so it is never recorded here and always appends).
    let mut placed: Vec<((i32, i32), usize)> = Vec::new();
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
            let hit = placed.iter().position(|&((e, c), _)| e == ev.entity && (c == ev.channel || ev.channel == -1));
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

        if q.len() >= QUEUE_CAP {
            // The cap drops a channel-0 sound, but not one on a non-zero
            // channel: SND_PickChannel's same-(entity, channel) override comes
            // first and always wins (snd_dma.c:365, "allways override sound
            // from same entity"), so the C never loses it, and the page needs
            // it to stop what that key plays — a door, lift or train's stop
            // sound (CHAN_VOICE) dropped in a busy frame left its "moving" hum
            // looping forever. It replaces an undrained entry of the same key
            // if there is one, so the queue stays bounded (the cap plus one
            // entry per key).
            if ev.channel == 0 {
                continue;
            }
            if let Ok(Some(bytes)) = pak.read_file(&path) {
                set_loop_window(&mut params, &bytes);
                let old = q.iter().position(|(_, p)| {
                    p.channel != 0 && p.entity == ev.entity && (p.channel == ev.channel || ev.channel == -1)
                });
                let qi = match old {
                    Some(qi) => {
                        q[qi] = (bytes, params);
                        qi
                    }
                    None => {
                        q.push((bytes, params));
                        q.len() - 1
                    }
                };
                placed.push(((ev.entity, ev.channel), qi));
            }
            continue;
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
}

/// One registered static (looping) sound awaiting pickup by the mixer: the WAV
/// bytes, its spatial params, and the loop window `GetWavinfo` found (seconds;
/// `loop_end` 0.0 = loop to the buffer's end, Web Audio's `loopEnd` default).
#[derive(Clone, Debug)]
pub struct StaticLoop {
    pub bytes: Vec<u8>,
    pub params: SndParams,
    pub loop_start: f32,
    pub loop_end: f32,
}

/// The C's effective static-sound budget. `S_StaticSound` (snd_dma.c) refuses
/// at `total_channels == MAX_CHANNELS` (128), but `total_channels` starts at
/// `MAX_DYNAMIC_CHANNELS + NUM_AMBIENTS` = 8 + 4 = 12 (`S_Init`), so at most
/// 116 statics ever fit — and `total_channels++` happens BEFORE the
/// load/loop checks, so a registration that then FAILS (missing or unlooped
/// sample) still burns its slot (see [`queue_static_sounds`]).
pub const MAX_STATIC_SOUNDS: usize = 128 - (8 + 4);

/// Load the WAV bytes for each placed static sound and queue them in `q` for
/// the mixer's loop pickup — the `S_StaticSound` (snd_dma.c:620) gate: a sample the
/// pak lacks is dropped, and so is one with no loop point (`sc->loopstart ==
/// -1` -> "Sound %s not looped"). Quake's ambient samples all carry a `cue `
/// loop chunk; one-shots don't, and the C refuses to static-loop them.
pub fn queue_static_sounds(q: &mut Vec<StaticLoop>, pak: &Pak, statics: &[StaticSound]) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snd::dma::{AMBIENT_FADE_DEFAULT, AMBIENT_LEVEL_DEFAULT, AMBIENT_SKY, AMBIENT_WATER};

    // ------------------------------------------------ ambient channel ramp

    /// Step `amb` by `n` whole 72fps host frames toward `levels` with the
    /// default cvars, returning the last frame's volumes.
    fn step_n(amb: &mut AmbientChannels, levels: &[u8; NUM_AMBIENTS], n: usize) -> [f32; NUM_AMBIENTS] {
        let mut v = [0.0; NUM_AMBIENTS];
        for _ in 0..n {
            v = amb.update(Some(levels), HOST_FRAME_STEP, AMBIENT_LEVEL_DEFAULT, AMBIENT_FADE_DEFAULT);
        }
        v
    }

    #[test]
    fn ambient_ramp_is_integer_and_asymmetric_like_the_c() {
        // The C's master_vol is an int (sound.h:79), so at the 72fps host cap
        // the default fade's per-frame step 100/72 ≈ 1.39 truncates: up-ramp
        // trunc(mv + 1.39) = mv + 1 (72 u/s), down-ramp trunc(mv - 1.39) =
        // mv - 2 (144 u/s) — fade-out twice as fast as fade-in.
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 0, 0, 0];
        let v = step_n(&mut amb, &levels, 1);
        assert_eq!(v[AMBIENT_WATER], 1.0, "up: trunc(0 + 1.39) = 1, not 1.39");
        assert_eq!(v[AMBIENT_SKY], 0.0, "sky leaf level 0 stays silent");
        step_n(&mut amb, &levels, 49);
        assert_eq!(amb.master_vol(AMBIENT_WATER), 50.0, "+1/step = 72 u/s up");
        // Empty leaf -> target 0: the down-ramp drops 2 per step.
        let quiet = [0u8; NUM_AMBIENTS];
        let v = step_n(&mut amb, &quiet, 1);
        assert_eq!(v[AMBIENT_WATER], 48.0, "down: trunc(50 - 1.39) = 48");
        step_n(&mut amb, &quiet, 24);
        assert_eq!(amb.master_vol(AMBIENT_WATER), 0.0, "reaches 0 exactly");
    }

    #[test]
    fn ambient_clamp_lands_on_truncated_target() {
        // A full-level water leaf targets 0.3*255 = 76.5, but the C stores the
        // clamp into the int channel: trunc(76.5) = 76. The ramp settles on
        // that whole int — never the fractional target — and holds it.
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 0, 0, 0];
        step_n(&mut amb, &levels, 200);
        assert_eq!(amb.master_vol(AMBIENT_WATER), 76.0, "trunc(76.5), an int");
        let v = step_n(&mut amb, &levels, 1);
        assert_eq!(v[AMBIENT_WATER], 76.0, "steady state holds");
    }

    #[test]
    fn ambient_floors_target_below_8_to_zero() {
        let mut amb = AmbientChannels::new();
        let loud = [0u8, 255, 0, 0];
        step_n(&mut amb, &loud, 200);
        assert_eq!(amb.master_vol(AMBIENT_SKY), 76.0);
        // Leaf level 20 -> 0.3*20 = 6 < 8 -> target 0 (the C's "if (vol < 8)
        // vol = 0"), so the channel ramps DOWN even though the leaf is nonzero.
        let faint = [0u8, 20, 0, 0];
        let v = step_n(&mut amb, &faint, 1);
        assert_eq!(v[AMBIENT_SKY], 74.0, "ramping down toward the 0 floor");
        step_n(&mut amb, &faint, 50);
        assert_eq!(amb.master_vol(AMBIENT_SKY), 0.0, "clamped onto the 0 floor");
    }

    #[test]
    fn ambient_no_leaf_silences_now_but_preserves_master_vol() {
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 0, 0, 0];
        step_n(&mut amb, &levels, 10);
        assert_eq!(amb.master_vol(AMBIENT_WATER), 10.0);
        // The C's `!l` case NULLs the sfx (silent this frame) without touching
        // master_vol — so re-entering the world resumes from 10, not 0.
        let v = amb.update(None, HOST_FRAME_STEP, AMBIENT_LEVEL_DEFAULT, AMBIENT_FADE_DEFAULT);
        assert_eq!(v, [0.0; NUM_AMBIENTS], "outside the world = silent");
        assert_eq!(amb.master_vol(AMBIENT_WATER), 10.0, "master_vol preserved");
        let v = step_n(&mut amb, &levels, 1);
        assert_eq!(v[AMBIENT_WATER], 11.0, "resumes ramping from the old value");
    }

    #[test]
    fn ambient_level_zero_cvar_silences_all() {
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 255, 255, 255];
        step_n(&mut amb, &levels, 10);
        let v = amb.update(Some(&levels), HOST_FRAME_STEP, 0.0, AMBIENT_FADE_DEFAULT);
        assert_eq!(v, [0.0; NUM_AMBIENTS], "ambient_level 0 = ambients off");
    }

    #[test]
    fn ambient_ramp_does_not_stall_at_144hz() {
        // At 144 Hz the C's literal int math would stall the up-ramp forever:
        // trunc(mv + (1/144)*100) = trunc(mv + 0.69) = mv. WinQuake never saw
        // such a frametime (Host_FilterTime's 72fps cap); the fixed-timestep
        // accumulator banks the half-steps so one whole 1/72 s step fires
        // every other frame — the same 72 ramp-steps/second as WinQuake.
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 0, 0, 0];
        for _ in 0..144 {
            amb.update(Some(&levels), 1.0 / 144.0, AMBIENT_LEVEL_DEFAULT, AMBIENT_FADE_DEFAULT);
        }
        // One real second = 72 steps = +72 units (not stalled at 0).
        assert_eq!(amb.master_vol(AMBIENT_WATER), 72.0);
    }

    #[test]
    fn ambient_long_stall_integrates_at_most_the_host_frame_clamp() {
        // Host_FilterTime clamps a long frame to 0.1 s ("don't allow really
        // long frames", host.c:515), so the C never integrated more than
        // 0.1 s of fade per frame. A 5 s stall (backgrounded tab) banks only
        // 0.1 s = 7 whole steps -> +7 units, no fast-forward.
        let mut amb = AmbientChannels::new();
        let levels = [255u8, 0, 0, 0];
        let v = amb.update(Some(&levels), 5.0, AMBIENT_LEVEL_DEFAULT, AMBIENT_FADE_DEFAULT);
        assert_eq!(v[AMBIENT_WATER], 7.0, "trunc(0.1 / (1/72)) = 7 steps");
    }
}
