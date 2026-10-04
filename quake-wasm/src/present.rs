//! `VID_Update` and `VID_ShiftPalette` (vid_win.c) for a page: the finished
//! 8-bit frame handed to the host the way the page shows it.
//!
//! Quake draws into `vid.buffer`, a palette index a pixel, and the display's
//! DAC turns the indices into colours through the palette `V_UpdatePalette`
//! set that frame (`VID_ShiftPalette`: the cshifts, then gamma). The page is
//! that DAC. With WebGL2 it takes the frame as drawn plus its 256 colours
//! ([`FORMAT_INDEXED8`]) and looks every pixel up on the GPU; with a 2-D
//! canvas it takes RGBA ([`FORMAT_RGBA8`]), which the program packs on the
//! renderer's threads ([`render::pack_rgba`]). The page says which (the
//! `Present` event); until it does, frames are RGBA.
//!
//! A frame reaches the page one of two ways:
//! - **copied:** the `Frame` record carries its bytes, which the host copies
//!   into a frame slot of its own (the `wasm32-wasip1` build);
//! - **in place:** when the program's memory is shared with the page (the
//!   threads build; the host passes `-sharedframes`), the `FrameAt` record
//!   says where the frame lies and the page reads it there. The program keeps
//!   its last [`RING`] frames, and the host keeps the page off the slot the
//!   next frame takes: a `FrameAt` returns only once the page is not reading
//!   the ring's next slot (web/PLATFORM.md, "Shared memory"). So a frame's
//!   buffer is written, or handed back to the frame pool, only while no one
//!   reads it.

use quake_rs::render::{self, FramePalette, Image};

use crate::proto::{FORMAT_INDEXED8, FORMAT_RGBA8, Msg};

/// The frames the program keeps: the page may be reading the newest or the
/// one before while the next one is drawn.
const RING: usize = 3;

/// The palette's size in a frame: 256 colours, RGBA each.
const PALETTE_BYTES: usize = 256 * 4;

/// One frame as the page is handed it.
#[derive(Default)]
struct Held {
    w: usize,
    h: usize,
    format: u8,
    /// The frame's own buffer of palette indices ([`FORMAT_INDEXED8`]), or
    /// its RGBA.
    pixels: Vec<u8>,
    /// The frame's palette, 256 RGBA colours; allocated once, so where it
    /// lies does not move.
    palette: Vec<u8>,
}

impl Held {
    /// The page's view of the frame: indexed through its palette, or its RGBA.
    fn rgba(&self) -> Vec<u8> {
        if self.format != FORMAT_INDEXED8 {
            return self.pixels.clone();
        }
        let colour = |i: u8| &self.palette[usize::from(i) * 4..usize::from(i) * 4 + 4];
        self.pixels.iter().flat_map(|&i| colour(i).iter().copied()).collect()
    }
}

/// The frames handed to the page: what it asked for, how they reach it, and
/// the last [`RING`] of them.
pub(crate) struct Present {
    /// [`FORMAT_RGBA8`] or [`FORMAT_INDEXED8`], as the page asked.
    format: u8,
    /// The page reads the frames in the program's memory (`FrameAt`).
    in_place: bool,
    ring: [Held; RING],
    /// The ring slot of the newest frame; `None` before the first.
    newest: Option<usize>,
}

impl Present {
    /// Frames copied out as RGBA until the page says otherwise; `in_place`
    /// when the page can read the program's memory.
    pub(crate) fn new(in_place: bool) -> Present {
        Present { format: FORMAT_RGBA8, in_place, ring: Default::default(), newest: None }
    }

    /// The `Present` event: the frames from the next one on in `format`
    /// (anything but [`FORMAT_INDEXED8`] is RGBA).
    pub(crate) fn set_format(&mut self, format: u8) {
        self.format = if format == FORMAT_INDEXED8 { FORMAT_INDEXED8 } else { FORMAT_RGBA8 };
    }

    /// `VID_Update`: the finished `image` becomes the newest frame, shown
    /// through `palette` — the frame's own buffer when the page takes it
    /// indexed, else packed to RGBA on up to `threads` threads. The ring's
    /// oldest buffer goes back to the frame pool (or holds the RGBA).
    pub(crate) fn frame(&mut self, image: Image, palette: &FramePalette, threads: usize) {
        let slot = self.newest.map_or(0, |s| (s + 1) % RING);
        let held = &mut self.ring[slot];
        let spare = std::mem::take(&mut held.pixels);
        (held.w, held.h, held.format) = (image.w, image.h, self.format);
        held.palette.resize(PALETTE_BYTES, 0);
        held.palette.copy_from_slice(&palette.to_bytes());
        if self.format == FORMAT_INDEXED8 {
            held.pixels = image.pixels;
            render::recycle_pixels(spare);
        } else {
            held.pixels = spare;
            render::pack_rgba(&image, palette, &mut held.pixels, threads);
            render::recycle_image(image);
        }
        self.newest = Some(slot);
    }

    /// The newest frame's record: its bytes (copied), or where it lies (in
    /// place). `None` before the first frame.
    pub(crate) fn msg(&self) -> Option<Msg<'_>> {
        let slot = self.newest?;
        let held = &self.ring[slot];
        let (w, h, format) = (held.w as u16, held.h as u16, held.format);
        Some(if self.in_place {
            Msg::FrameAt {
                w,
                h,
                format,
                slot: slot as u8,
                pixels: address(&held.pixels),
                palette: address(&held.palette),
            }
        } else {
            let palette = if format == FORMAT_INDEXED8 { &held.palette[..] } else { &[] };
            Msg::Frame { w, h, format, palette, pixels: &held.pixels }
        })
    }

    /// The newest frame as the page shows it, RGBA (the checks' view; empty
    /// before the first frame).
    pub(crate) fn rgba(&self) -> Vec<u8> {
        self.newest.map(|s| self.ring[s].rgba()).unwrap_or_default()
    }
}

/// Where `bytes` lie in the program's memory: the offset the page reads a
/// `FrameAt` frame at (wasm32's addresses are 32-bit; natively, where
/// nothing reads it, the low bits).
fn address(bytes: &[u8]) -> u32 {
    bytes.as_ptr().addr() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Record;

    /// A 4x2 frame of indices 0..8 and a palette whose entry `i` is
    /// `[i, 2i, 3i]`, with shifts none and gamma the identity.
    fn frame() -> (Image, FramePalette) {
        let image = Image { w: 4, h: 2, pixels: (0..8).collect() };
        let base: [[u8; 3]; 256] = std::array::from_fn(|i| [i as u8, (2 * i) as u8, (3 * i) as u8]);
        (image, FramePalette::new(&base, &[], &render::build_gamma_table(1.0)))
    }

    fn record(p: &Present) -> Record {
        let mut out = Vec::new();
        p.msg().expect("a frame").write_to(&mut out).unwrap();
        Record::split(&out).remove(0)
    }

    #[test]
    fn rgba_until_the_page_asks_for_the_indexed_frame() {
        let mut p = Present::new(false);
        assert!(p.msg().is_none() && p.rgba().is_empty(), "nothing before the first frame");
        let (image, palette) = frame();
        p.frame(image, &palette, 1);
        let rgba = record(&p);
        assert_eq!((rgba.kind, rgba.payload[4], rgba.payload.len()), (Record::FRAME, FORMAT_RGBA8, 8 + 32));
        assert_eq!(&rgba.payload[8 + 4 * 5..8 + 4 * 6], &[5, 10, 15, 255], "pixel 5 through the palette");
        p.set_format(FORMAT_INDEXED8);
        let (image, palette) = frame();
        p.frame(image, &palette, 1);
        let indexed = record(&p);
        assert_eq!(
            (indexed.kind, indexed.payload[4], indexed.payload.len()),
            (Record::FRAME, FORMAT_INDEXED8, 8 + 1024 + 8)
        );
        assert_eq!(&indexed.payload[8..8 + 1024], &palette.to_bytes()[..]);
        assert_eq!(&indexed.payload[8 + 1024..], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(p.rgba(), rgba.payload[8..], "either way the page shows the same colours");
    }

    #[test]
    fn in_place_frames_say_where_they_lie_and_rotate_through_the_ring() {
        let mut p = Present::new(true);
        p.set_format(FORMAT_INDEXED8);
        let mut slots = Vec::new();
        for _ in 0..4 {
            let (image, palette) = frame();
            p.frame(image, &palette, 1);
            let r = record(&p);
            assert_eq!((r.kind, r.payload.len()), (Record::FRAME_AT, 16));
            let held = &p.ring[usize::from(r.payload[5])];
            assert_eq!((r.u32_at(8), r.u32_at(12)), (address(&held.pixels), address(&held.palette)));
            slots.push(r.payload[5]);
        }
        assert_eq!(slots, [0, 1, 2, 0]);
    }
}
