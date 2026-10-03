//! The view in row bands, drawn on several threads.
//!
//! The port's own: id's renderer ran on one CPU. Its frame splits cleanly
//! after the edge scan. `R_ScanEdges` leaves every pixel of the view in
//! exactly one span, and after it every pass writes pixels through the
//! z-buffer: the world's spans (`D_DrawSurfaces`, `D_DrawZSpans`), then the
//! alias models and sprites, particles and gun, each testing and writing
//! `d_pzbuffer`. So the view is cut into bands of whole rows, and each band
//! runs the same passes, in id's order, on the pixels in its rows only. Every
//! pixel then sees the same writes in the same order as in one pass over the
//! whole view, whichever thread draws its band: the frame is byte-identical
//! for any thread count.
//!
//! What must be decided for the whole frame first — the edge scan, the
//! surface cache's lookups (`D_CacheSurface`), the alias models' vertices and
//! clipping, the particles' projection — is done once, before the bands, and
//! handed to them read-only. The surface cache's bakes, the blocks the frame
//! finds stale, are independent of one another and run on the threads too,
//! in the same round: each thread takes bakes until none is left and then
//! bands (`surf::Bakes`; what a thread does first is [`Workers::run`]'s
//! `start`). One thread is the same code with one band, drawn on the calling
//! thread.
//! A band's pixels are the view's own rows or, drawn straight into the
//! screen, the screen's rows under the view ([`Band::placed`]).
//!
//! The threads are `std::thread::scope`'s, spawned for the bands of a frame
//! and joined before [`Renderer::render`](super::Renderer::render) returns:
//! they borrow the frame, and nothing outlives it. That is the one way safe
//! Rust lends a frame's buffers to other threads; a pool of threads kept
//! across frames would need the frame's data owned or `'static`. A round —
//! spawn, run, join — of 1, 3 and 7 helper threads costs about 32, 61 and
//! 77 µs natively when threads ran a moment before, and 145–415 µs after 14
//! ms idle (a frame's first round at 72 Hz); in the page, where a spawn
//! wakes a pooled worker, 10–25 µs hot and 160–265 µs cold in Chromium,
//! 20–40 and 180–240 in Firefox (the review of the bakes, 2026-10-03) —
//! against milliseconds of pixels at the sizes where threads pay.

use std::ops::Range;
use std::sync::{Mutex, PoisonError};

/// Rows `y0..` of a `w`-wide view: their pixels and their 16-bit `1/z`,
/// borrowed from the frame so that bands can be drawn at once. The pixels
/// are the view's own rows, or the rows of the screen it is drawn straight
/// into (`stride` pixels a row, the view's the `x0..x0 + w` of each).
pub(super) struct Band<'a> {
    w: usize,
    y0: usize,
    pixels: &'a mut [u8],
    stride: usize,
    x0: usize,
    z: &'a mut [i16],
}

impl<'a> Band<'a> {
    /// The whole of a `w`-wide view as one band: its pixels (palette indices)
    /// and its `1/z` `z`, row after row.
    pub(super) fn whole(w: usize, pixels: &'a mut [u8], z: &'a mut [i16]) -> Band<'a> {
        let n = pixels.len().min(z.len()) / w.max(1) * w;
        Band { w, y0: 0, pixels: &mut pixels[..n], stride: w, x0: 0, z: &mut z[..n] }
    }

    /// The whole of a `w`-wide view drawn into its place on a screen: `rows`
    /// are the screen's rows the view covers, `stride` pixels each, the view
    /// at column `x0` of them (`x0 + w <= stride`); `z` its `1/z`.
    pub(super) fn placed(w: usize, rows: &'a mut [u8], stride: usize, x0: usize, z: &'a mut [i16]) -> Band<'a> {
        let h = (rows.len() / stride.max(1)).min(z.len() / w.max(1));
        Band { w, y0: 0, pixels: &mut rows[..h * stride], stride, x0, z: &mut z[..h * w] }
    }

    /// `self` cut into bands of `rows` rows (the last one shorter).
    pub(super) fn split(self, rows: usize) -> Vec<Band<'a>> {
        let (w, y0, stride, x0) = (self.w, self.y0, self.stride, self.x0);
        let rows = rows.max(1);
        self.pixels
            .chunks_mut(rows * stride.max(1))
            .zip(self.z.chunks_mut(rows * w.max(1)))
            .enumerate()
            .map(|(i, (pixels, z))| Band { w, y0: y0 + i * rows, pixels, stride, x0, z })
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
        self.y0..self.y0 + self.z.len() / self.w.max(1)
    }

    /// The view-linear pixel indices (`v * w + u`) this band holds.
    #[inline]
    pub(super) fn indices(&self) -> Range<usize> {
        let first = self.y0 * self.w;
        first..first + self.z.len()
    }

    /// Row `v`'s pixels `u..u + n` and their `1/z`, if `v` is in the band
    /// (`u + n` at most the width).
    #[inline]
    pub(super) fn span(&mut self, u: usize, v: usize, n: usize) -> Option<(&mut [u8], &mut [i16])> {
        let row = v.checked_sub(self.y0)?;
        let z = row * self.w + u;
        if z + n > self.z.len() || u + n > self.w {
            return None;
        }
        let p = row * self.stride + self.x0 + u;
        Some((self.pixels.get_mut(p..p + n)?, &mut self.z[z..z + n]))
    }

    /// The pixel and `1/z` at view-linear index `idx` (`v * w + u`), if the
    /// band holds it.
    #[inline]
    pub(super) fn at(&mut self, idx: usize) -> Option<(&mut u8, &mut i16)> {
        let i = idx.checked_sub(self.y0 * self.w)?;
        let z = self.z.get_mut(i)?;
        let p = if self.stride == self.w && self.x0 == 0 {
            i
        } else {
            i / self.w * self.stride + self.x0 + i % self.w
        };
        Some((self.pixels.get_mut(p)?, z))
    }
}

/// The renderer's thread count as a setting: [`Threads::Auto`] takes what
/// the platform offers, [`Threads::Count`] exactly that many. The frame is
/// the same either way; only its time changes. A host resolves it against
/// what it has — `std::thread::available_parallelism` natively, the page's
/// worker pool in the browser — and hands the count to
/// [`Renderer::set_threads`](super::Renderer::set_threads) each frame. As a
/// cvar (`r_threads`), 0 is `Auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Threads {
    /// As many as the platform offers.
    #[default]
    Auto,
    /// This many (1: the calling thread alone).
    Count(usize),
}

impl Threads {
    /// The count to draw with on a platform offering `available` threads (at
    /// least 1 either way).
    #[must_use]
    pub fn resolve(self, available: usize) -> usize {
        match self {
            Threads::Auto => available.max(1),
            Threads::Count(n) => n.max(1),
        }
    }

    /// The setting a cvar value names: 0 (or anything below 1) is `Auto`.
    #[must_use]
    pub fn from_cvar(value: f32) -> Threads {
        if value >= 1.0 { Threads::Count(value as usize) } else { Threads::Auto }
    }

    /// The cvar value naming this setting (0 for `Auto`).
    #[must_use]
    pub fn cvar(self) -> usize {
        match self {
            Threads::Auto => 0,
            Threads::Count(n) => n,
        }
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
    /// Each thread begins with `start()` — the frame's work that is not
    /// rows, shared out between the threads as they arrive (the bakes), and
    /// the thread's own state (its counters) — and the states come back,
    /// the calling thread's first. With one thread, one band: `start` and
    /// `draw` on the calling thread. The threads take the bands from one
    /// queue, so a thread the system will not start (no threads on this
    /// target, say) only leaves its share to the others: the frame is drawn
    /// whatever the count.
    pub(super) fn run<T, I, D>(self, whole: Band, h: usize, start: I, draw: D) -> Vec<T>
    where
        T: Send,
        I: Fn() -> T + Sync,
        D: Fn(&mut Band, &mut T) + Sync,
    {
        let threads = self.threads.min(h.max(1));
        if threads <= 1 {
            let (mut band, mut t) = (whole, start());
            draw(&mut band, &mut t);
            return vec![t];
        }
        let bands = threads * BANDS_PER_THREAD;
        let queue = Mutex::new(whole.split(h.div_ceil(bands)).into_iter());
        let next = || queue.lock().unwrap_or_else(PoisonError::into_inner).next();
        let work = || {
            let mut t = start();
            while let Some(mut band) = next() {
                draw(&mut band, &mut t);
            }
            t
        };
        std::thread::scope(|s| {
            let helpers: Vec<_> =
                (1..threads).filter_map(|_| std::thread::Builder::new().spawn_scoped(s, work).ok()).collect();
            let mut out = vec![work()];
            // A worker that panicked panics the frame, as one thread would.
            out.extend(helpers.into_iter().map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))));
            out
        })
    }
}

/// Run `f` on runs of rows of `dst` — `dst_row` elements a row, `rows` rows
/// in all — on up to `threads` threads, the calling thread one of them: the
/// per-row maps that end a frame (the RGBA pack, the view's copy into the
/// screen, the underwater warp), each output row a function of its place
/// alone, so any split gives the same bytes. `f` gets the run's first row.
pub(crate) fn for_rows<D, F>(threads: usize, rows: usize, dst: &mut [D], dst_row: usize, f: F)
where
    D: Send,
    F: Fn(usize, &mut [D]) + Sync,
{
    let threads = threads.clamp(1, rows.max(1));
    if threads == 1 || dst_row == 0 {
        f(0, dst);
        return;
    }
    // Several runs a thread, taken from a queue as threads come free (see
    // `Workers::run`): one run a thread waits on the slowest core, as a
    // phone's efficiency cores or a busy machine show (the underwater warp
    // took 70% longer with 2 of 8 cores shared). A thread that does not
    // start leaves its runs to the others.
    let per = rows.div_ceil(threads * BANDS_PER_THREAD);
    let queue = Mutex::new(dst.chunks_mut(per * dst_row).enumerate());
    let work = || loop {
        let Some((i, d)) = queue.lock().unwrap_or_else(PoisonError::into_inner).next() else { break };
        f(i * per, d);
    };
    std::thread::scope(|s| {
        for _ in 1..threads {
            // A thread that would not start is no loss: the queue is drained below.
            let _ = std::thread::Builder::new().spawn_scoped(s, work);
        }
        work();
    });
}

/// [`for_rows`] with a source row for each output row: `f` gets matching
/// runs of `dst` and `src` (`src_row` elements a row).
pub(crate) fn map_rows<D, S, F>(threads: usize, rows: usize, dst: &mut [D], dst_row: usize, src: &[S], src_row: usize, f: F)
where
    D: Send,
    S: Sync,
    F: Fn(&mut [D], &[S]) + Sync,
{
    for_rows(threads, rows, dst, dst_row, |row0, d| {
        let n = d.len() / dst_row.max(1);
        let start = (row0 * src_row).min(src.len());
        let end = ((row0 + n) * src_row).min(src.len());
        f(d, &src[start..end]);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threads_setting_resolves_against_what_the_platform_offers() {
        assert_eq!(Threads::default(), Threads::Auto);
        assert_eq!(Threads::Auto.resolve(8), 8);
        assert_eq!(Threads::Auto.resolve(0), 1, "no threads offered: the calling thread");
        assert_eq!(Threads::Count(3).resolve(8), 3, "a count is taken as it is");
        assert_eq!(Threads::Count(0).resolve(8), 1);
        assert_eq!((Threads::from_cvar(0.0), Threads::from_cvar(-2.0)), (Threads::Auto, Threads::Auto));
        assert_eq!(Threads::from_cvar(4.0), Threads::Count(4));
        assert_eq!((Threads::Auto.cvar(), Threads::Count(4).cvar()), (0, 4));
    }

    #[test]
    fn bands_cover_the_view_once_in_order() {
        let (w, h) = (7usize, 23usize);
        let mut pixels = vec![0u8; w * h];
        let mut z = vec![0i16; w * h];
        let bands = Band::whole(w, &mut pixels, &mut z).split(5);
        let rows: Vec<Range<usize>> = bands.iter().map(Band::rows).collect();
        assert_eq!(rows, [0..5, 5..10, 10..15, 15..20, 20..23]);
        assert_eq!(bands[4].indices(), 140..161);
    }

    #[test]
    fn a_band_reaches_only_its_own_pixels() {
        let (w, h) = (4usize, 6usize);
        let mut pixels = vec![0u8; w * h];
        let mut z = vec![0i16; w * h];
        let mut bands = Band::whole(w, &mut pixels, &mut z).split(2);
        let b = &mut bands[1]; // rows 2..4
        assert!(b.span(0, 1, 4).is_none() && b.span(0, 4, 1).is_none());
        assert!(b.span(3, 2, 2).is_none(), "past the row's end");
        let (px, zz) = b.span(1, 3, 2).expect("in the band");
        px.fill(9);
        zz.fill(9);
        assert!(b.at(7).is_none() && b.at(16).is_none());
        *b.at(8).expect("row 2, column 0").0 = 5;
        drop(bands);
        assert_eq!(pixels[3 * w + 1], 9);
        assert_eq!(pixels[3 * w + 2], 9);
        assert_eq!(pixels[8], 5);
        assert_eq!(pixels.iter().filter(|p| **p != 0).count(), 3);
        assert_eq!(z.iter().filter(|v| **v == 9).count(), 2);
    }

    #[test]
    fn a_placed_band_writes_the_view_into_its_place_on_the_screen() {
        // A 3x4 view at column 2 of a 7-wide screen, from screen row 1.
        let (w, h, stride) = (3usize, 4usize, 7usize);
        let mut screen = vec![0u8; stride * 6];
        let mut z = vec![0i16; w * h];
        let bands = Band::placed(w, &mut screen[stride..(1 + h) * stride], stride, 2, &mut z).split(3);
        assert_eq!(bands.iter().map(Band::rows).collect::<Vec<_>>(), [0..3, 3..4]);
        for mut band in bands {
            for v in band.rows() {
                let (px, _) = band.span(0, v, w).expect("the row");
                px.fill(1 + v as u8);
            }
            let (px, zz) = band.at(band.indices().end - 1).expect("the band's last pixel");
            *px = 9;
            *zz = 9;
        }
        for (i, p) in screen.iter().enumerate() {
            let (row, col) = (i / stride, i % stride);
            let want = match (row, col) {
                (3 | 4, 4) => 9, // each band's last pixel
                (1..=4, 2..=4) => row as u8,
                _ => 0,
            };
            assert_eq!(*p, want, "screen ({col}, {row})");
        }
        assert_eq!(z.iter().filter(|v| **v == 9).count(), 2);
    }

    #[test]
    fn rows_are_mapped_once_whatever_the_thread_count() {
        let (w, h) = (5usize, 23usize);
        let src: Vec<u16> = (0..w * h).map(|i| i as u16).collect();
        for threads in [1, 2, 4, 30] {
            let mut dst = vec![0u32; w * 2 * h];
            map_rows(threads, h, &mut dst, 2 * w, &src, w, |d, s| {
                for (o, x) in d.chunks_mut(2).zip(s) {
                    o.fill(u32::from(*x) + 1);
                }
            });
            assert!(dst.iter().enumerate().all(|(i, v)| *v as usize == i / 2 + 1), "{threads} threads");
        }
    }

    #[test]
    fn every_band_is_drawn_once_whatever_the_thread_count() {
        let (w, h) = (3usize, 37usize);
        for threads in [1, 2, 3, 8, 64] {
            let mut pixels = vec![0u8; w * h];
            let mut z = vec![0i16; w * h];
            let counts = Workers::new(threads).run(
                Band::whole(w, &mut pixels, &mut z),
                h,
                || 0usize,
                |band, n| {
                    for v in band.rows() {
                        if let Some((px, _)) = band.span(0, v, w) {
                            for p in px {
                                *p += 1;
                            }
                        }
                    }
                    *n += band.rows().len();
                },
            );
            assert!(pixels.iter().all(|&p| p == 1), "{threads} threads");
            assert_eq!(counts.iter().sum::<usize>(), h);
            assert_eq!(counts.len(), threads.min(h));
        }
    }
}
