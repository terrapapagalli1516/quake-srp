//! Sound caching, ported from WinQuake's `snd_mem.c`: `GetWavinfo` (a
//! `.wav`'s header and `cue ` loop point), `S_LoadSound` and `ResampleSfx`
//! (a sample converted once, at load, to the mixer's rate and width), and
//! `snd_dma.c`'s `known_sfx` table (`S_FindName`), which holds them.
//!
//! The conversion is id's point resampling: each output sample repeats the
//! nearest earlier source sample, stepping through the source in 8.8 fixed
//! point (`fracstep`). At 11025, 22050 and 44100 Hz that step is exact; at
//! 48000 it is 58/256 of a source sample where 58.8/256 is right, so id's
//! sounds play 1.4% flat and lose their last 1.4%. [`LoadOptions::exact`]
//! steps exactly instead (still point resampling); Classic keeps id's step.
//!
//! id's `ResampleSfx` keeps `stepscale` in an x87 register (80 bits); the
//! port computes it in `f64`. The two give the same `length`, `loopstart`
//! and `fracstep` for every sample in the shareware pak at the four rates
//! the sound oracle checks (`oracle/sound.py`); where the exact quotient is a
//! whole number the last bit of the two precisions could in principle differ.

use crate::pak::Pak;

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

    Some(WavInfo { channels, rate, width, loop_start, samples, data_offset: data + 8 })
}

// ---------------------------------------------------------------------------
// S_LoadSound, ResampleSfx (snd_mem.c): a sample at the mixer's rate.
// ---------------------------------------------------------------------------

/// A sample's data after `ResampleSfx`: signed 8-bit, or signed 16-bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfxData {
    /// `sc->width == 1`: every Quake sample but two, and all of them under
    /// `loadas8bit`.
    Eight(Vec<i8>),
    /// `sc->width == 2`: the 16-bit sources (`weapons/lhit.wav` and
    /// `lstart.wav`, 22050 Hz) unless `loadas8bit` is set.
    Sixteen(Vec<i16>),
}

/// `sfxcache_t`: one mono sample converted at load to the mixer's rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfxCache {
    /// Samples at the mixer's rate (`sc->length`).
    pub length: i32,
    /// Where a loop restarts, at the mixer's rate; `None` for a one-shot
    /// (`sc->loopstart == -1`).
    pub loopstart: Option<i32>,
    /// The mixer's rate (`sc->speed`).
    pub speed: i32,
    /// The samples.
    pub data: SfxData,
}

/// What [`load_sound`] converts a sample to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadOptions {
    /// The mixer's rate (`shm->speed`).
    pub speed: i32,
    /// The `loadas8bit` cvar: keep 16-bit sources as 8-bit.
    pub loadas8bit: bool,
    /// Step through the source exactly instead of in id's 8.8 fixed point
    /// (see the module note).
    pub exact: bool,
}

/// `S_LoadSound` (snd_mem.c) for a `.wav` file's bytes: `GetWavinfo`, the
/// mono check, and `ResampleSfx`.
///
/// `None` where the C prints and returns NULL (not a PCM `.wav`, a stereo
/// sample), and also where its data chunk runs past the file, which the C
/// would read beyond its buffer.
pub fn load_sound(bytes: &[u8], opts: LoadOptions) -> Option<SfxCache> {
    let info = wav_info(bytes)?;
    if info.channels != 1 {
        return None; // "%s is a stereo sample"
    }
    let width = usize::from(info.width);
    let samples = usize::try_from(info.samples).ok()?;
    let data = bytes.get(info.data_offset..info.data_offset.checked_add(samples.checked_mul(width)?)?)?;
    Some(resample_sfx(&info, data, opts))
}

/// `ResampleSfx` (snd_mem.c): `data` (the source's `info.samples` samples of
/// `info.width` bytes) point-resampled to `opts.speed` and converted to the
/// output width. Unsigned 8-bit sources become signed (`- 128`).
fn resample_sfx(info: &WavInfo, data: &[u8], opts: LoadOptions) -> SfxCache {
    // "this is usually 0.5, 1, or 2". The C holds it in an x87 register.
    let stepscale = f64::from(info.rate) / f64::from(opts.speed.max(1));
    let outcount = (f64::from(info.samples) / stepscale) as i32;
    let loopstart = info.loop_start.and_then(|ls| i32::try_from(ls).ok()).map(|ls| (f64::from(ls) / stepscale) as i32);
    let fracstep = (stepscale * 256.0) as i64;

    // The source sample behind output sample `i`: id's 8.8 fixed-point walk
    // (`samplefrac += fracstep`), or the exact one.
    let source_index = |i: usize| -> usize {
        if opts.exact {
            (i as u64 * u64::from(info.rate) / opts.speed.max(1) as u64) as usize
        } else {
            ((i as i64 * fracstep) >> 8) as usize
        }
    };
    // The source sample as 16 bits: `LittleShort`, or `(byte - 128) << 8`.
    let source = |s: usize| -> i32 {
        if info.width == 2 {
            data.get(2 * s..2 * s + 2).map_or(0, |b| i32::from(i16::from_le_bytes([b[0], b[1]])))
        } else {
            data.get(s).map_or(0, |&b| (i32::from(b) - 128) << 8)
        }
    };

    let n = usize::try_from(outcount).unwrap_or(0);
    let data = if opts.loadas8bit || info.width == 1 {
        SfxData::Eight((0..n).map(|i| (source(source_index(i)) >> 8) as i8).collect())
    } else {
        SfxData::Sixteen((0..n).map(|i| source(source_index(i)) as i16).collect())
    };
    SfxCache { length: outcount, loopstart, speed: opts.speed, data }
}

// ---------------------------------------------------------------------------
// known_sfx (snd_dma.c): S_FindName and the samples it holds.
// ---------------------------------------------------------------------------

/// `MAX_SFX` (snd_dma.c): the most sound names a session can know.
pub const MAX_SFX: usize = 512;

/// `MAX_QPATH` (quakedef.h): a sound name must be shorter.
const MAX_QPATH: usize = 64;

/// A handle to one entry of the [`SfxTable`] (the C's `sfx_t *`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SfxId(u16);

/// Whether an entry's sample has been loaded (the C's `sfx->cache`).
#[derive(Debug)]
enum Cached {
    NotYet,
    Loaded(SfxCache),
    /// `S_LoadSound` found no such file or could not use it. The C tries
    /// again at every use; the pak does not change, so the port remembers.
    Failed,
}

/// `known_sfx`: every sound name the mixer has been asked for, with its
/// sample once loaded. Entries are never removed, as in the C.
#[derive(Debug, Default)]
pub struct SfxTable {
    known: Vec<(String, Cached)>,
}

impl SfxTable {
    /// `S_FindName`: the entry for `name`, added if new. `None` where the C
    /// `Sys_Error`s: a name of `MAX_QPATH` or more, or all [`MAX_SFX`] entries
    /// taken.
    pub fn find_name(&mut self, name: &str) -> Option<SfxId> {
        if name.len() >= MAX_QPATH {
            return None;
        }
        if let Some(i) = self.known.iter().position(|(n, _)| n == name) {
            return Some(SfxId(i as u16));
        }
        if self.known.len() == MAX_SFX {
            return None;
        }
        self.known.push((name.to_string(), Cached::NotYet));
        Some(SfxId((self.known.len() - 1) as u16))
    }

    /// `S_LoadSound`: `id`'s sample, read from `pak` (as `sound/<name>`) and
    /// converted on first use.
    pub fn load(&mut self, id: SfxId, pak: &Pak, opts: LoadOptions) -> Option<&SfxCache> {
        let (name, cached) = self.known.get_mut(usize::from(id.0))?;
        if matches!(cached, Cached::NotYet) {
            let bytes = pak.read_file(&format!("sound/{name}")).ok().flatten();
            *cached = match bytes.and_then(|b| load_sound(&b, opts)) {
                Some(sc) => Cached::Loaded(sc),
                None => Cached::Failed,
            };
        }
        self.cached(id)
    }

    /// `id`'s sample if it has been loaded (`Cache_Check`).
    pub fn cached(&self, id: SfxId) -> Option<&SfxCache> {
        match self.known.get(usize::from(id.0)) {
            Some((_, Cached::Loaded(sc))) => Some(sc),
            _ => None,
        }
    }

    /// `id`'s name, as it was asked for (relative to `sound/`).
    pub fn name(&self, id: SfxId) -> &str {
        self.known.get(usize::from(id.0)).map_or("", |(n, _)| n.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ------------------------------------------------ ResampleSfx port

    /// A 16-bit mono PCM `.wav` of `samples` at `rate`.
    fn wav16(rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend(b"RIFF");
        b.extend(0u32.to_le_bytes());
        b.extend(b"WAVE");
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend((samples.len() as u32 * 2).to_le_bytes());
        for s in samples {
            b.extend(s.to_le_bytes());
        }
        b
    }

    fn opts(speed: i32) -> LoadOptions {
        LoadOptions { speed, loadas8bit: false, exact: false }
    }

    #[test]
    fn eight_bit_samples_become_signed_at_their_own_rate() {
        let mut w = wav(Some(3), 4);
        let at = w.len() - 4;
        w[at..].copy_from_slice(&[0x00, 0x80, 0xff, 0x7f]);
        let sc = load_sound(&w, opts(11025)).expect("load");
        assert_eq!(sc.data, SfxData::Eight(vec![-128, 0, 127, -1]), "byte - 128");
        assert_eq!((sc.length, sc.loopstart, sc.speed), (4, Some(3), 11025));
    }

    #[test]
    fn doubling_the_rate_repeats_each_sample_and_scales_the_loop() {
        let mut w = wav(Some(1), 3);
        let at = w.len() - 3;
        w[at..].copy_from_slice(&[0x81, 0x82, 0x83]);
        let sc = load_sound(&w, opts(22050)).expect("load");
        assert_eq!(sc.data, SfxData::Eight(vec![1, 1, 2, 2, 3, 3]));
        assert_eq!((sc.length, sc.loopstart), (6, Some(2)), "loopstart / 0.5");
    }

    #[test]
    fn at_48k_id_steps_58_of_256_where_exact_steps_58_8() {
        // 11025 -> 48000: stepscale 0.2296875, fracstep trunc(58.8) = 58.
        // Output sample i reads source (i * 58) >> 8 in id's walk and
        // i * 11025 / 48000 in the exact one: id's falls behind by 0.8/256
        // of a source sample per output sample.
        let n = 441; // 0.04 s
        let mut w = wav(None, n as u32);
        let at = w.len() - n;
        for (i, b) in w[at..].iter_mut().enumerate() {
            *b = i as u8; // source sample i, modulo 256
        }
        let id = load_sound(&w, opts(48000)).expect("load");
        let exact = load_sound(&w, LoadOptions { exact: true, ..opts(48000) }).expect("load");
        assert_eq!(id.length, 1920, "441 / 0.2296875");
        assert_eq!(exact.length, 1920);
        let (SfxData::Eight(a), SfxData::Eight(b)) = (&id.data, &exact.data) else { panic!("8-bit") };
        let src = |v: i8| (i32::from(v) + 128) as usize;
        assert_eq!(src(a[1919]), ((1919 * 58) >> 8) % 256, "id's walk ends at 434, not 440");
        assert_eq!(src(b[1919]), (1919 * 11025 / 48000) % 256, "exact: 440, the last source sample");
        assert_eq!((0..1920).find(|&i| a[i] != b[i]), Some(22), "they part within 2 ms");
    }

    #[test]
    fn sixteen_bit_sources_stay_16_bit_unless_loadas8bit() {
        let w = wav16(22050, &[1000, -1000, 32767, -32768]);
        let sc = load_sound(&w, opts(11025)).expect("load");
        assert_eq!(sc.data, SfxData::Sixteen(vec![1000, 32767]), "every other sample at half the rate");
        let sc8 = load_sound(&w, LoadOptions { loadas8bit: true, ..opts(11025) }).expect("load");
        assert_eq!(sc8.data, SfxData::Eight(vec![3, 127]), "sample >> 8");
    }

    #[test]
    fn stereo_and_truncated_samples_do_not_load() {
        let mut w = wav(None, 4);
        w[22] = 2; // channels = 2
        assert_eq!(load_sound(&w, opts(11025)), None, "a stereo sample");
        let w = wav(None, 100);
        assert_eq!(load_sound(&w[..w.len() - 1], opts(11025)), None, "data past the end");
    }

    #[test]
    fn find_name_reuses_entries_and_refuses_what_id_sys_errors_on() {
        let mut t = SfxTable::default();
        let a = t.find_name("misc/null.wav").expect("slot");
        assert_eq!(t.find_name("misc/null.wav"), Some(a));
        assert_ne!(t.find_name("misc/talk.wav"), Some(a));
        assert_eq!(t.name(a), "misc/null.wav");
        assert_eq!(t.find_name(&"x".repeat(64)), None, "MAX_QPATH");
        for i in 2..MAX_SFX {
            t.find_name(&format!("s{i}")).expect("slot");
        }
        assert_eq!(t.find_name("one/too/many.wav"), None, "MAX_SFX");
    }
}
