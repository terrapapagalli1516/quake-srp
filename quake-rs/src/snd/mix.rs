//! Painting, ported from WinQuake's `snd_mix.c`: `S_PaintChannels` sums
//! every channel into the paint buffer 512 sample pairs at a time
//! (`SND_PaintChannelFrom8`, `SND_PaintChannelFrom16`, with
//! `SND_InitScaletable`'s volume table), loops or ends each sample, and
//! `S_TransferStereo16` scales the sum by the master volume and clamps it to
//! 16 bits.
//!
//! In the C the output is the DMA buffer; here it is the caller's slice
//! ([`Mixer::paint`]), so a platform writes it wherever its device reads.
//! The inner loops keep id's shape: they are the point.

use super::dma::{Channel, Mixer};
use super::mem::{SfxCache, SfxData};

/// `PAINTBUFFER_SIZE` (snd_mix.c): the most sample pairs one pass paints.
const PAINTBUFFER_SIZE: usize = 512;

/// `portable_samplepair_t`: one pair of the paint buffer, wide enough for
/// every channel's sum.
#[derive(Debug, Clone, Copy, Default)]
struct SamplePair {
    left: i32,
    right: i32,
}

/// `snd_mix.c`'s buffers: `paintbuffer` and `snd_scaletable`.
#[derive(Debug)]
pub(super) struct PaintState {
    buffer: Vec<SamplePair>,
    /// `snd_scaletable[32][256]`: an 8-bit sample (as its byte) times a
    /// volume in steps of 8 — 32 volumes, so a channel's 0..255 volume
    /// loses its low 3 bits.
    scaletable: Vec<[i32; 256]>,
}

impl PaintState {
    /// `SND_InitScaletable`: `scaletable[i][j] = (signed char)j * i * 8`.
    pub(super) fn new() -> PaintState {
        let scaletable = (0..32)
            .map(|i| {
                let mut row = [0; 256];
                for (j, v) in row.iter_mut().enumerate() {
                    *v = i32::from(j as u8 as i8) * i * 8;
                }
                row
            })
            .collect();
        PaintState { buffer: vec![SamplePair::default(); PAINTBUFFER_SIZE], scaletable }
    }

    /// `SND_PaintChannelFrom8` / `SND_PaintChannelFrom16`: add `count` of
    /// `ch`'s samples into the paint buffer from pair `offset` (id's is
    /// always 0; see [`super::Fixes::loop_seam`]).
    fn paint_from(&mut self, ch: &mut Channel, sc: &SfxCache, count: usize, offset: usize) {
        let pos = usize::try_from(ch.pos).unwrap_or(0);
        let dest = &mut self.buffer[offset..offset + count];
        match &sc.data {
            SfxData::Eight(data) => {
                // The 8-bit painter clamps (and keeps) volumes past 255,
                // which combined static sounds reach.
                ch.leftvol = ch.leftvol.clamp(0, 255);
                ch.rightvol = ch.rightvol.clamp(0, 255);
                let lscale = &self.scaletable[(ch.leftvol >> 3) as usize];
                let rscale = &self.scaletable[(ch.rightvol >> 3) as usize];
                for (p, &s) in dest.iter_mut().zip(data.get(pos..).unwrap_or_default()) {
                    p.left = p.left.wrapping_add(lscale[usize::from(s as u8)]);
                    p.right = p.right.wrapping_add(rscale[usize::from(s as u8)]);
                }
            }
            SfxData::Sixteen(data) => {
                let (leftvol, rightvol) = (ch.leftvol, ch.rightvol);
                for (p, &s) in dest.iter_mut().zip(data.get(pos..).unwrap_or_default()) {
                    p.left = p.left.wrapping_add(i32::from(s).wrapping_mul(leftvol) >> 8);
                    p.right = p.right.wrapping_add(i32::from(s).wrapping_mul(rightvol) >> 8);
                }
            }
        }
        ch.pos += count as i32;
    }
}

impl Mixer {
    /// Paint the next `out.len() / 2` sample pairs into `out` as interleaved
    /// left/right 16-bit samples: `S_PaintChannels` up to `paintedtime` plus
    /// that many, with `S_TransferStereo16` writing each pass.
    ///
    /// id's `S_Update_` called this once a frame for everything between the
    /// last painted pair and the DMA position plus `_snd_mixahead`
    /// ([`Mixer::samples_ahead`]). A platform that paints the same counts at
    /// the same points between [`Mixer::update`]s gets id's samples exactly:
    /// the 512-pair passes start where each call starts.
    pub fn paint(&mut self, out: &mut [i16]) {
        let endtime = self.paintedtime + (out.len() / 2) as i64;
        let snd_vol = (f64::from(self.cvars.volume) * 256.0) as i32;
        let mut done = 0;
        while self.paintedtime < endtime {
            // if paintbuffer is smaller than DMA buffer
            let end = endtime.min(self.paintedtime + PAINTBUFFER_SIZE as i64);
            let count = (end - self.paintedtime) as usize;

            // clear the paint buffer
            self.paint.buffer[..count].fill(SamplePair::default());

            // paint in the channels.
            for i in 0..self.total_channels {
                self.paint_channel(i, end);
            }

            // transfer out according to DMA format
            transfer_stereo16(&self.paint.buffer[..count], snd_vol, &mut out[2 * done..2 * (done + count)]);
            done += count;
            self.paintedtime = end;
        }
    }

    /// One channel's part of an `S_PaintChannels` pass, from `paintedtime` to
    /// `end`: paint up to the sample's end, then loop it from its
    /// `loopstart` or stop the channel.
    fn paint_channel(&mut self, i: usize, end: i64) {
        let Mixer { channels, sfx, paint, paintedtime, fixes, .. } = self;
        let ch = &mut channels[i];
        let Some(id) = ch.sfx else { return };
        if ch.leftvol == 0 && ch.rightvol == 0 {
            return;
        }
        let Some(sc) = sfx.cached(id) else { return };

        let mut ltime = *paintedtime;
        while ltime < end {
            // paint up to end
            let count = ch.end.min(end) - ltime;
            if count > 0 {
                let offset = if fixes.loop_seam { (ltime - *paintedtime) as usize } else { 0 };
                paint.paint_from(ch, sc, count as usize, offset);
                ltime += count;
            }

            // if at end of loop, restart
            if ltime >= ch.end {
                match sc.loopstart {
                    // A loop point at or past the end would restart forever
                    // without painting (the C hangs); such a sample stops.
                    Some(loopstart) if loopstart < sc.length => {
                        ch.pos = loopstart;
                        ch.end = ltime + i64::from(sc.length - loopstart);
                    }
                    _ => {
                        // channel just stopped
                        ch.sfx = None;
                        break;
                    }
                }
            }
        }
    }
}

/// `S_TransferStereo16` (`Snd_WriteLinearBlastStereo16`): each pair times
/// the master volume (`volume * 256`), shifted back, clamped to 16 bits.
fn transfer_stereo16(src: &[SamplePair], snd_vol: i32, out: &mut [i16]) {
    let clamp = |v: i32| (v.wrapping_mul(snd_vol) >> 8).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    for (p, o) in src.iter().zip(out.chunks_exact_mut(2)) {
        o[0] = clamp(p.left);
        o[1] = clamp(p.right);
    }
}
