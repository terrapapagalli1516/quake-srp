//! The view in row bands, drawn on several threads.
//!
//! The port's own: id's renderer ran on one CPU. Its frame splits cleanly
//! after the edge scan. `R_ScanEdges` leaves every pixel of the view in
//! exactly one span, and after it every pass writes pixels through the
//! z-buffer: the world's spans (`D_DrawSurfaces`, `D_DrawZSpans`), then the
//! alias models, sprites, particles and gun, each testing and writing
//! `d_pzbuffer`. So the view is cut into bands of whole rows, and each band
//! runs the same passes, in id's order, on the pixels in its rows only. Every
//! pixel then sees the same writes in the same order as in one pass over the
//! whole view, whichever thread draws its band: the frame is byte-identical
//! for any thread count.
//!
//! What must be decided for the whole frame first — the edge scan, the
//! surface cache (`D_CacheSurface`), the alias models' vertices and clipping,
//! the particles' projection — is done once, before the bands, and handed to
//! them read-only. One thread is the same code with one band, drawn on the
//! calling thread.
//!
//! The threads are `std::thread::scope`'s, spawned for the bands of a frame
//! and joined before [`Renderer::render`](super::Renderer::render) returns:
//! they borrow the frame, and nothing outlives it.

use std::ops::Range;
use std::sync::{Mutex, PoisonError};

/// Rows `y0..y0 + rows` of a `w`-wide view: their pixels and their 16-bit
/// `1/z`, borrowed from the frame so that bands can be drawn at once.
pub(super) struct Band<'a> {
    w: usize,
    y0: usize,
    rgb: &'a mut [[u8; 3]],
    z: &'a mut [i16],
}

impl<'a> Band<'a> {
    /// The whole of a `w`-wide view as one band.
    pub(super) fn whole(w: usize, rgb: &'a mut [[u8; 3]], z: &'a mut [i16]) -> Band<'a> {
        let n = rgb.len().min(z.len());
        Band { w, y0: 0, rgb: &mut rgb[..n], z: &mut z[..n] }
    }

    /// `self` cut into bands of `rows` rows (the last one shorter).
    pub(super) fn split(self, rows: usize) -> Vec<Band<'a>> {
        let (w, y0) = (self.w, self.y0);
        let chunk = rows.max(1) * w.max(1);
        self.rgb
            .chunks_mut(chunk)
            .zip(self.z.chunks_mut(chunk))
            .enumerate()
            .map(|(i, (rgb, z))| Band { w, y0: y0 + i * rows.max(1), rgb, z })
            .collect()
    }

    /// The view's width.
    #[inline]
    pub(super) fn width(&self) -> usize {
        self.w
    }

    /// The view rows this band holds.
    #[inline]
    pub(super) fn rows(&self) -> Range<usize> {
        self.y0..self.y0 + self.rgb.len() / self.w.max(1)
    }

    /// The view-linear pixel indices (`v * w + u`) this band holds.
    #[inline]
    pub(super) fn indices(&self) -> Range<usize> {
        let first = self.y0 * self.w;
        first..first + self.rgb.len()
    }

    /// Row `v`'s pixels `u..u + n` and their `1/z`, if `v` is in the band
    /// (`u + n` at most the width).
    #[inline]
    pub(super) fn span(&mut self, u: usize, v: usize, n: usize) -> Option<(&mut [[u8; 3]], &mut [i16])> {
        let row = v.checked_sub(self.y0)?;
        let start = row * self.w + u;
        let end = start + n;
        if end > self.rgb.len() || u + n > self.w {
            return None;
        }
        Some((&mut self.rgb[start..end], &mut self.z[start..end]))
    }

    /// The pixel and `1/z` at view-linear index `idx` (`v * w + u`), if the
    /// band holds it.
    #[inline]
    pub(super) fn at(&mut self, idx: usize) -> Option<(&mut [u8; 3], &mut i16)> {
        let i = idx.checked_sub(self.y0 * self.w)?;
        Some((self.rgb.get_mut(i)?, self.z.get_mut(i)?))
    }
}

/// How many threads draw a frame: 1 draws on the calling thread alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Workers {
    threads: usize,
}

impl Default for Workers {
    fn default() -> Workers {
        Workers { threads: 1 }
    }
}

/// Bands per thread: more bands than threads, handed out as threads come
/// free, so a band that is all gun or all sky does not hold the frame up.
const BANDS_PER_THREAD: usize = 4;

impl Workers {
    /// `threads` workers (at least 1).
    pub(super) fn new(threads: usize) -> Workers {
        Workers { threads: threads.max(1) }
    }

    pub(super) fn threads(self) -> usize {
        self.threads
    }

    /// Cut `whole` (an `h`-row view) into bands and run `draw` on each, on
    /// up to [`Workers::threads`] threads, the calling thread one of them.
    /// Each thread starts from `init()` (its own counters, say) and the
    /// values come back in thread order. With one thread, one band: `draw`
    /// on the calling thread.
    pub(super) fn run<T, I, D>(self, whole: Band, h: usize, init: I, draw: D) -> Vec<T>
    where
        T: Send,
        I: Fn() -> T + Sync,
        D: Fn(&mut Band, &mut T) + Sync,
    {
        let threads = self.threads.min(h.max(1));
        if threads <= 1 {
            let (mut band, mut t) = (whole, init());
            draw(&mut band, &mut t);
            return vec![t];
        }
        let bands = threads * BANDS_PER_THREAD;
        let queue = Mutex::new(whole.split(h.div_ceil(bands)).into_iter());
        let next = || queue.lock().unwrap_or_else(PoisonError::into_inner).next();
        let work = || {
            let mut t = init();
            while let Some(mut band) = next() {
                draw(&mut band, &mut t);
            }
            t
        };
        std::thread::scope(|s| {
            let helpers: Vec<_> = (1..threads).map(|_| s.spawn(work)).collect();
            let mut out = vec![work()];
            // A worker that panicked panics the frame, as one thread would.
            out.extend(helpers.into_iter().map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))));
            out
        })
    }
}

/// Run `f` on matching runs of rows of `dst` and `src` — `dst_row` and
/// `src_row` elements a row, `rows` rows in all — on up to `threads` threads,
/// the calling thread one of them: the per-row maps that end a frame (the
/// RGBA pack, the view's copy into the screen), each output row a function
/// of its input row alone, so any split gives the same bytes.
pub(crate) fn map_rows<D, S, F>(threads: usize, rows: usize, dst: &mut [D], dst_row: usize, src: &[S], src_row: usize, f: F)
where
    D: Send,
    S: Sync,
    F: Fn(&mut [D], &[S]) + Sync,
{
    let threads = threads.clamp(1, rows.max(1));
    let per = rows.div_ceil(threads).max(1);
    let (dst_chunk, src_chunk) = ((per * dst_row).max(1), (per * src_row).max(1));
    if threads == 1 {
        f(dst, src);
        return;
    }
    let mut runs = dst.chunks_mut(dst_chunk).zip(src.chunks(src_chunk));
    let first = runs.next();
    std::thread::scope(|s| {
        for (d, r) in runs {
            s.spawn(|| f(d, r));
        }
        if let Some((d, r)) = first {
            f(d, r);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_the_view_once_in_order() {
        let (w, h) = (7usize, 23usize);
        let mut rgb = vec![[0u8; 3]; w * h];
        let mut z = vec![0i16; w * h];
        let bands = Band::whole(w, &mut rgb, &mut z).split(5);
        let rows: Vec<Range<usize>> = bands.iter().map(Band::rows).collect();
        assert_eq!(rows, [0..5, 5..10, 10..15, 15..20, 20..23]);
        assert_eq!(bands[4].indices(), 140..161);
    }

    #[test]
    fn a_band_reaches_only_its_own_pixels() {
        let (w, h) = (4usize, 6usize);
        let mut rgb = vec![[0u8; 3]; w * h];
        let mut z = vec![0i16; w * h];
        let mut bands = Band::whole(w, &mut rgb, &mut z).split(2);
        let b = &mut bands[1]; // rows 2..4
        assert!(b.span(0, 1, 4).is_none() && b.span(0, 4, 1).is_none());
        assert!(b.span(3, 2, 2).is_none(), "past the row's end");
        let (px, zz) = b.span(1, 3, 2).expect("in the band");
        px.fill([9; 3]);
        zz.fill(9);
        assert!(b.at(7).is_none() && b.at(16).is_none());
        *b.at(8).expect("row 2, column 0").0 = [5; 3];
        drop(bands);
        assert_eq!(rgb[3 * w + 1], [9; 3]);
        assert_eq!(rgb[3 * w + 2], [9; 3]);
        assert_eq!(rgb[8], [5; 3]);
        assert_eq!(rgb.iter().filter(|p| **p != [0; 3]).count(), 3);
        assert_eq!(z.iter().filter(|v| **v == 9).count(), 2);
    }

    #[test]
    fn every_band_is_drawn_once_whatever_the_thread_count() {
        let (w, h) = (3usize, 37usize);
        for threads in [1, 2, 3, 8, 64] {
            let mut rgb = vec![[0u8; 3]; w * h];
            let mut z = vec![0i16; w * h];
            let counts = Workers::new(threads).run(
                Band::whole(w, &mut rgb, &mut z),
                h,
                || 0usize,
                |band, n| {
                    for v in band.rows() {
                        if let Some((px, _)) = band.span(0, v, w) {
                            for p in px {
                                p[0] += 1;
                            }
                        }
                    }
                    *n += band.rows().len();
                },
            );
            assert!(rgb.iter().all(|p| p[0] == 1), "{threads} threads");
            assert_eq!(counts.iter().sum::<usize>(), h);
            assert_eq!(counts.len(), threads.min(h));
        }
    }
}
