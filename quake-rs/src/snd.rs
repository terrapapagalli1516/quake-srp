//! Client-side sound bookkeeping ported from WinQuake's `snd_dma.c` /
//! `snd_mem.c`: the four automatic per-leaf ambient channels
//! (`S_UpdateAmbientSounds`) and the `.wav` header/loop-point parser
//! (`GetWavinfo`).
//!
//! The actual MIXING is out of scope (the audit's explicit deviation: the web
//! front-end mixes with Web Audio). What is kept faithful here is the *control*
//! data: which ambient channels exist, how each frame's target volume is
//! derived from the view leaf's `ambient_level[]` bytes, the exact
//! `ambient_fade` ramp toward that target, and which samples are loopable (the
//! `cue ` chunk loop start the C refused to static-loop without).

use crate::bsp::NUM_AMBIENTS;

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
        AmbientChannels {
            master_vol: [0; NUM_AMBIENTS],
            accum: 0.0,
        }
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
    /// ([`AMBIENT_LEVEL_DEFAULT`]/[`AMBIENT_FADE_DEFAULT`]).
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
// GetWavinfo (snd_mem.c): RIFF/WAVE header + cue-chunk loop point.
// ---------------------------------------------------------------------------

/// The fields `GetWavinfo` (snd_mem.c:247) extracts from a `.wav`: PCM format
/// info, the total sample count, and — the part that matters for ambient
/// sounds — the `cue ` chunk loop start. `S_StaticSound` REFUSES a sample with
/// no loop point ("Sound %s not looped"), so a front-end uses
/// [`loop_start`](WavInfo::loop_start) both to decide whether a static sound
/// may loop at all and where the loop restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    /// Channel count (1 = mono; every Quake SFX is mono).
    pub channels: u16,
    /// Sample rate in Hz (Quake's are 11025).
    pub rate: u32,
    /// Bytes per sample (1 or 2).
    pub width: u16,
    /// Loop start in samples from the `cue ` chunk, or `None` when the file
    /// has no cue chunk (a non-looping one-shot; the C's `loopstart = -1`).
    pub loop_start: Option<u32>,
    /// Total playable samples. When a `LIST`/`mark` chunk gives a loop length
    /// the C truncates to `loopstart + loop_len` (the loop END); otherwise the
    /// full `data` chunk length in samples.
    pub samples: u32,
    /// Byte offset of the PCM data within the file.
    pub data_offset: usize,
}

/// Find a RIFF chunk by tag, scanning forward from byte offset `from`
/// (`FindNextChunk`/`FindChunk`, snd_mem.c). Chunks are `[4-byte tag][4-byte
/// LE length][length bytes, padded to even]`. Returns the offset of the
/// chunk's TAG, like the C leaves `data_p` at the chunk start. A negative
/// (`>i32::MAX`) length or running off `end` returns None, exactly the C's
/// NULL cases — never a panic.
fn find_chunk(bytes: &[u8], tag: &[u8; 4], mut from: usize, end: usize) -> Option<usize> {
    loop {
        if from >= end || from + 8 > bytes.len() {
            return None;
        }
        let len = u32::from_le_bytes(bytes[from + 4..from + 8].try_into().ok()?);
        if len > i32::MAX as u32 {
            return None; // the C's `iff_chunk_len < 0` reject
        }
        if &bytes[from..from + 4] == tag {
            return Some(from);
        }
        // last_chunk = data_p + 8 + ((len + 1) & ~1)
        from = from.checked_add(8 + ((len as usize + 1) & !1))?;
    }
}

/// Read a little-endian u32 at `at`, bounds-checked.
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Read a little-endian u16 at `at`, bounds-checked.
fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// Parse a `.wav`'s header the way `GetWavinfo` (snd_mem.c:247) does: locate
/// `RIFF`/`WAVE`, require Microsoft PCM (`format == 1`), read channel
/// count/rate/width from `fmt `, the loop start from `cue ` (and the loop
/// length from a following `LIST`+`mark`, "not a proper parse, but it works
/// with cooledit"), and the sample count from `data`.
///
/// Returns `None` wherever the C printed an error and bailed with a zeroed
/// info (missing chunks, non-PCM format) — and also for the C's
/// `Sys_Error("bad loop length")` case (data shorter than the declared loop),
/// where crashing would be unhelpful. Every access is bounds-checked; corrupt
/// input can never panic.
pub fn wav_info(bytes: &[u8]) -> Option<WavInfo> {
    // find "RIFF" chunk + "WAVE" id.
    let riff = find_chunk(bytes, b"RIFF", 0, bytes.len())?;
    if bytes.get(riff + 8..riff + 12)? != b"WAVE" {
        return None;
    }

    // Chunks scan from just after the RIFF header ("RIFF" + len + "WAVE").
    let iff_data = riff + 12;

    // "fmt " chunk: format(u16) channels(u16) rate(u32) byterate(u32)
    // blockalign(u16) bits(u16).
    let fmt = find_chunk(bytes, b"fmt ", iff_data, bytes.len())?;
    let p = fmt + 8;
    let format = le_u16(bytes, p)?;
    if format != 1 {
        return None; // Microsoft PCM format only
    }
    let channels = le_u16(bytes, p + 2)?;
    let rate = le_u32(bytes, p + 4)?;
    // The C skips byte-rate (4) + block-align (2) before bits-per-sample.
    let width = le_u16(bytes, p + 14)? / 8;
    if width == 0 || channels == 0 {
        return None; // malformed; also guards the divisions below
    }

    // "cue " chunk: the loop start is the first cue point's sample offset,
    // 32 bytes past the chunk tag (tag 4 + len 4 + count 4 + cuepoint{id,
    // position, chunkid, chunkstart, blockstart, sampleoffset} = 20, of which
    // sampleoffset is last).
    let mut loop_start: Option<u32> = None;
    let mut loop_samples: u32 = 0; // info.samples from the LIST mark, 0 = unset
    if let Some(cue) = find_chunk(bytes, b"cue ", iff_data, bytes.len()) {
        let ls = le_u32(bytes, cue + 32)?;
        loop_start = Some(ls);
        // A following LIST chunk whose tag+28 reads "mark" carries the loop
        // length at tag+24 (the C: strncmp(data_p+28,"mark",4) then reads at
        // data_p+24). The scan resumes AFTER the cue chunk, like last_chunk.
        let cue_len = le_u32(bytes, cue + 4)? as usize;
        let next = cue + 8 + ((cue_len + 1) & !1);
        if let Some(list) = find_chunk(bytes, b"LIST", next, bytes.len()) {
            if bytes.get(list + 28..list + 32) == Some(b"mark") {
                // "this is not a proper parse, but it works with cooledit..."
                let len = le_u32(bytes, list + 24)?;
                loop_samples = ls.saturating_add(len);
            }
        }
    }

    // "data" chunk: total samples = byte length / sample width.
    let data = find_chunk(bytes, b"data", iff_data, bytes.len())?;
    let data_len = le_u32(bytes, data + 4)?;
    let samples = data_len / width as u32;

    let samples = if loop_samples != 0 {
        if samples < loop_samples {
            return None; // the C Sys_Error's "Sound %s has a bad loop length"
        }
        loop_samples
    } else {
        samples
    };

    Some(WavInfo {
        channels,
        rate,
        width,
        loop_start,
        samples,
        data_offset: data + 8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------ ambient channel ramp

    /// Step `amb` by `n` whole 72fps host frames toward `levels` with the
    /// default cvars, returning the last frame's volumes.
    fn step_n(
        amb: &mut AmbientChannels,
        levels: &[u8; NUM_AMBIENTS],
        n: usize,
    ) -> [f32; NUM_AMBIENTS] {
        let mut v = [0.0; NUM_AMBIENTS];
        for _ in 0..n {
            v = amb.update(
                Some(levels),
                HOST_FRAME_STEP,
                AMBIENT_LEVEL_DEFAULT,
                AMBIENT_FADE_DEFAULT,
            );
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

    // ------------------------------------------------ GetWavinfo port

    /// Build a minimal RIFF/WAVE: fmt (PCM mono 11025 8-bit), optional cue
    /// (loop start), then `data_samples` bytes of data.
    fn wav(cue: Option<u32>, data_samples: u32) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend(b"RIFF");
        b.extend(0u32.to_le_bytes()); // RIFF length (unchecked by the parser)
        b.extend(b"WAVE");
        // fmt chunk: 16 bytes of PCM header.
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // format = PCM
        b.extend(1u16.to_le_bytes()); // channels = 1
        b.extend(11025u32.to_le_bytes()); // rate
        b.extend(11025u32.to_le_bytes()); // byte rate
        b.extend(1u16.to_le_bytes()); // block align
        b.extend(8u16.to_le_bytes()); // bits per sample
        if let Some(ls) = cue {
            // cue chunk: count = 1, one cue point whose final field is the
            // sample offset (the loop start, 32 bytes past the tag).
            b.extend(b"cue ");
            b.extend(28u32.to_le_bytes());
            b.extend(1u32.to_le_bytes()); // cue point count
            b.extend(0u32.to_le_bytes()); // id
            b.extend(0u32.to_le_bytes()); // position
            b.extend(b"data"); // chunk id
            b.extend(0u32.to_le_bytes()); // chunk start
            b.extend(0u32.to_le_bytes()); // block start
            b.extend(ls.to_le_bytes()); // sample offset = loop start
        }
        b.extend(b"data");
        b.extend(data_samples.to_le_bytes());
        b.extend(std::iter::repeat_n(0x80u8, data_samples as usize));
        b
    }

    #[test]
    fn wav_info_parses_pcm_header_and_data() {
        let info = wav_info(&wav(None, 100)).expect("parse");
        assert_eq!(info.channels, 1);
        assert_eq!(info.rate, 11025);
        assert_eq!(info.width, 1);
        assert_eq!(info.samples, 100);
        assert_eq!(info.loop_start, None, "no cue chunk = not loopable");
        // Data offset points at the PCM bytes.
        assert_eq!(info.data_offset, wav(None, 100).len() - 100);
    }

    #[test]
    fn wav_info_reads_cue_loop_start() {
        let info = wav_info(&wav(Some(42), 100)).expect("parse");
        assert_eq!(info.loop_start, Some(42), "cue chunk sample offset");
        assert_eq!(info.samples, 100, "no LIST mark: full data length");
    }

    #[test]
    fn wav_info_rejects_non_pcm_and_corrupt() {
        let mut nonpcm = wav(None, 4);
        nonpcm[20] = 2; // format = 2 (ADPCM) -> "Microsoft PCM format only"
        assert_eq!(wav_info(&nonpcm), None);
        assert_eq!(wav_info(b"not a wav"), None);
        assert_eq!(wav_info(&[]), None);
        // Truncated mid-header must not panic.
        let w = wav(Some(10), 100);
        for cut in 0..w.len() {
            let _ = wav_info(&w[..cut]);
        }
    }

    #[test]
    fn wav_info_list_mark_truncates_to_loop_end() {
        // cue (loop start 10) + LIST whose +28 is "mark" and +32 the loop
        // length (20) -> samples = 10 + 20 = 30 even though data has 100.
        let mut b = wav(Some(10), 100);
        // Splice a LIST chunk between cue and data: find "data" tag from the
        // chunk scan region and insert before it.
        let data_at = b.windows(4).rposition(|w| w == b"data").unwrap();
        let mut list: Vec<u8> = Vec::new();
        list.extend(b"LIST");
        list.extend(28u32.to_le_bytes());
        list.extend([0u8; 16]); // adtl header etc (opaque to the parser)
        list.extend(20u32.to_le_bytes()); // +24: samples in loop
        list.extend(b"mark"); // +28: the cooledit marker
        list.extend([0u8; 4]);
        b.splice(data_at..data_at, list);
        let info = wav_info(&b).expect("parse");
        assert_eq!(info.loop_start, Some(10));
        assert_eq!(info.samples, 30, "truncated to loopstart + loop length");

        // A declared loop PAST the data is the C's Sys_Error case -> None.
        let mut bad = b.clone();
        let data_at = bad.windows(4).rposition(|w| w == b"data").unwrap();
        bad[data_at + 4..data_at + 8].copy_from_slice(&8u32.to_le_bytes());
        bad.truncate(data_at + 8 + 8);
        assert_eq!(wav_info(&bad), None, "data shorter than the loop");
    }
}
