//! SPR sprite-format loader.
//!
//! Ported from Quake (GPLv2) — id Software, 1996. The on-disk layout comes from
//! `WinQuake/spritegn.h` (`dsprite_t`, `dspriteframe_t`, `dspritegroup_t`,
//! `dspriteinterval_t`, `dspriteframetype_t`); the frame/group read order is
//! taken from `WinQuake/model.c` (`Mod_LoadSpriteModel`, `Mod_LoadSpriteFrame`,
//! `Mod_LoadSpriteGroup`).
//!
//! The C engine cast the raw file buffer straight onto these structs and walked
//! it with pointer arithmetic; here every field is decoded explicitly through a
//! bounds-checked [`crate::read::Reader`], so a truncated or malformed sprite
//! yields a [`QError`] rather than undefined behaviour. Where the C called
//! `Sys_Error` (bad version, negative counts, `interval <= 0`) we return
//! `Err(QError)`.

use crate::error::{QError, Result};
use crate::read::Reader;

/// `IDSPRITEHEADER` from `spritegn.h`:
/// `(('P'<<24)+('S'<<16)+('D'<<8)+'I')`, which is little-endian `"IDSP"`.
pub const IDSPRITEHEADER: [u8; 4] = *b"IDSP";

/// `SPRITE_VERSION` from `spritegn.h`.
pub const SPRITE_VERSION: i32 = 1;

// Orientation / type constants from `spritegn.h`.
pub const SPR_VP_PARALLEL_UPRIGHT: i32 = 0;
pub const SPR_FACING_UPRIGHT: i32 = 1;
pub const SPR_VP_PARALLEL: i32 = 2;
pub const SPR_ORIENTED: i32 = 3;
pub const SPR_VP_PARALLEL_ORIENTED: i32 = 4;

// Frame-type tags (`spriteframetype_t`).
const SPR_SINGLE: i32 = 0;
const SPR_GROUP: i32 = 1;

/// The fixed 36-byte `dsprite_t` file header.
///
/// Field order and types match the C struct exactly:
/// `ident i32, version i32, type i32, boundingradius f32, width i32,
///  height i32, numframes i32, beamlength f32, synctype i32`.
#[derive(Debug, Clone)]
pub struct SpriteHeader {
    /// `IDSPRITEHEADER` magic read as a little-endian i32.
    pub ident: i32,
    /// Format version; must equal [`SPRITE_VERSION`].
    pub version: i32,
    /// Orientation, one of the `SPR_*` constants.
    pub type_: i32,
    /// Bounding radius of the sprite.
    pub boundingradius: f32,
    /// Maximum frame width.
    pub width: i32,
    /// Maximum frame height.
    pub height: i32,
    /// Number of top-level frames.
    pub numframes: i32,
    /// Beam length (used by oriented/beam sprites).
    pub beamlength: f32,
    /// Sync type (`ST_SYNC=0`, `ST_RAND=1`).
    pub synctype: i32,
}

impl SpriteHeader {
    /// On-disk size of `dsprite_t`, in bytes.
    pub const SIZE: usize = 36;
}

/// A single sprite frame: the `dspriteframe_t` header plus its 8-bit pixel
/// bitmap (`width * height` palette indices, row-major).
#[derive(Debug, Clone)]
pub struct SpriteFrame {
    /// `origin[2]` from `dspriteframe_t` (left, up offsets in pixels).
    pub origin: [i32; 2],
    /// Frame width in pixels.
    pub width: i32,
    /// Frame height in pixels.
    pub height: i32,
    /// `width * height` palette indices, row-major.
    pub pixels: Vec<u8>,
}

/// One top-level entry in the sprite: either a single still frame or an
/// animated group with per-frame intervals.
#[derive(Debug, Clone)]
pub enum Frame {
    /// `SPR_SINGLE`: a lone [`SpriteFrame`].
    Single(SpriteFrame),
    /// `SPR_GROUP`: `intervals.len() == frames.len()` animated sub-frames.
    Group {
        /// Per-frame playback intervals (all `> 0`, per the C check).
        intervals: Vec<f32>,
        /// The group's sub-frames, in file order.
        frames: Vec<SpriteFrame>,
    },
}

/// A fully parsed `.spr` sprite model.
#[derive(Debug, Clone)]
pub struct Sprite {
    /// The decoded 36-byte header.
    pub header: SpriteHeader,
    /// `header.numframes` top-level frames.
    pub frames: Vec<Frame>,
}

/// Read one `dspriteframe_t` (`origin[2]` i32, `width` i32, `height` i32) followed by
/// `width * height` pixel bytes. Mirrors `Mod_LoadSpriteFrame` in `model.c`.
fn read_sprite_frame(r: &mut Reader) -> Result<SpriteFrame> {
    // dspriteframe_t { int origin[2]; int width; int height; }
    let origin = [r.i32()?, r.i32()?];
    let width = r.i32()?;
    let height = r.i32()?;

    if width < 0 {
        return Err(QError::invalid("sprite frame has negative width"));
    }
    if height < 0 {
        return Err(QError::invalid("sprite frame has negative height"));
    }

    // size = width * height; the bitmap immediately follows the header.
    let size = (width as i64)
        .checked_mul(height as i64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| QError::invalid("sprite frame size overflow"))?;

    let pixels = r.take(size)?.to_vec();

    Ok(SpriteFrame {
        origin,
        width,
        height,
        pixels,
    })
}

/// Read a `SPR_GROUP`: `dspritegroup_t { int numframes }`, then `numframes`
/// `dspriteinterval_t { float interval }`, then `numframes` sprite frames.
/// Mirrors `Mod_LoadSpriteGroup` in `model.c`.
// `!(interval > 0.0)` is deliberate: unlike the C's `<= 0.0` (Mod_LoadSpriteGroup),
// it also rejects a NaN interval; rewriting per clippy would re-admit NaN.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn read_sprite_group(r: &mut Reader) -> Result<Frame> {
    let numframes = r.i32()?;
    if numframes < 0 {
        return Err(QError::invalid("sprite group has negative numframes"));
    }
    let numframes = numframes as usize;

    // All intervals come first, contiguously, then all frames.
    // Cap reservations by the bytes left so an untrusted `numframes` cannot
    // force a huge up-front allocation; the reads below still bound the count.
    let mut intervals = Vec::with_capacity(numframes.min(r.remaining() / 4));
    for _ in 0..numframes {
        let interval = r.f32()?;
        // C: if (*poutintervals <= 0.0) Sys_Error("interval<=0").
        if !(interval > 0.0) {
            return Err(QError::invalid("sprite group interval <= 0"));
        }
        intervals.push(interval);
    }

    let mut frames = Vec::with_capacity(numframes.min(r.remaining()));
    for _ in 0..numframes {
        frames.push(read_sprite_frame(r)?);
    }

    Ok(Frame::Group { intervals, frames })
}

impl Sprite {
    /// Parse a `.spr` sprite from an in-memory buffer.
    ///
    /// Verifies `ident == "IDSP"` and `version == SPRITE_VERSION`, then decodes
    /// `numframes` frames in file order. Negative counts and non-positive group
    /// intervals are rejected (the C engine called `Sys_Error` here).
    pub fn parse(bytes: &[u8]) -> Result<Sprite> {
        let mut r = Reader::new(bytes);

        // --- dsprite_t header (36 bytes) ---
        let ident_bytes = r.bytes::<4>()?;
        if ident_bytes != IDSPRITEHEADER {
            return Err(QError::BadMagic {
                context: "spr header",
                found: ident_bytes,
                expected: "IDSP",
            });
        }
        // Keep the raw little-endian i32 value of the magic for the struct.
        let ident = i32::from_le_bytes(ident_bytes);

        let version = r.i32()?;
        if version != SPRITE_VERSION {
            return Err(QError::invalid(format!(
                "spr has wrong version number ({} should be {})",
                version, SPRITE_VERSION
            )));
        }

        let type_ = r.i32()?;
        let boundingradius = r.f32()?;
        let width = r.i32()?;
        let height = r.i32()?;
        let numframes = r.i32()?;
        let beamlength = r.f32()?;
        let synctype = r.i32()?;

        // C: if (numframes < 1) Sys_Error("Invalid # of frames").
        if numframes < 1 {
            return Err(QError::invalid(format!(
                "spr has invalid # of frames: {}",
                numframes
            )));
        }

        let header = SpriteHeader {
            ident,
            version,
            type_,
            boundingradius,
            width,
            height,
            numframes,
            beamlength,
            synctype,
        };

        // --- frames ---
        // pframetype = (dspriteframetype_t *)(pin + 1); each frame begins with
        // an int `type`, then the SINGLE or GROUP body.
        // Cap by bytes remaining: `numframes` is from the (validated >=1) header
        // but is still untrusted, so never reserve more slots than could exist.
        let mut frames = Vec::with_capacity((numframes as usize).min(r.remaining()));
        for _ in 0..numframes {
            let frametype = r.i32()?;
            let frame = match frametype {
                SPR_SINGLE => Frame::Single(read_sprite_frame(&mut r)?),
                SPR_GROUP => read_sprite_group(&mut r)?,
                other => {
                    return Err(QError::invalid(format!(
                        "spr has unknown frame type: {}",
                        other
                    )));
                }
            };
            frames.push(frame);
        }

        Ok(Sprite { header, frames })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Append a little-endian i32.
    fn push_i32(buf: &mut Vec<u8>, v: i32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Append a little-endian f32.
    fn push_f32(buf: &mut Vec<u8>, v: f32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Build a minimal valid SPR: one SPR_SINGLE 2x2 frame.
    fn synthetic_single_2x2() -> Vec<u8> {
        let mut buf = Vec::new();
        // dsprite_t header.
        buf.extend_from_slice(b"IDSP"); // ident
        push_i32(&mut buf, 1); // version
        push_i32(&mut buf, SPR_VP_PARALLEL); // type
        push_f32(&mut buf, 12.5); // boundingradius
        push_i32(&mut buf, 2); // width
        push_i32(&mut buf, 2); // height
        push_i32(&mut buf, 1); // numframes
        push_f32(&mut buf, 0.0); // beamlength
        push_i32(&mut buf, 1); // synctype (ST_RAND)
        assert_eq!(buf.len(), SpriteHeader::SIZE);

        // frame[0]: type SPR_SINGLE, dspriteframe header, then 2*2 pixels.
        push_i32(&mut buf, SPR_SINGLE); // frametype
        push_i32(&mut buf, -1); // origin[0]
        push_i32(&mut buf, 3); // origin[1]
        push_i32(&mut buf, 2); // width
        push_i32(&mut buf, 2); // height
        buf.extend_from_slice(&[10, 20, 30, 40]); // 4 pixels
        buf
    }

    #[test]
    fn parses_single_2x2() {
        let buf = synthetic_single_2x2();
        let spr = Sprite::parse(&buf).unwrap();

        // Header fields.
        assert_eq!(spr.header.ident, i32::from_le_bytes(*b"IDSP"));
        assert_eq!(spr.header.version, 1);
        assert_eq!(spr.header.type_, SPR_VP_PARALLEL);
        assert_eq!(spr.header.boundingradius, 12.5);
        assert_eq!(spr.header.width, 2);
        assert_eq!(spr.header.height, 2);
        assert_eq!(spr.header.numframes, 1);
        assert_eq!(spr.header.beamlength, 0.0);
        assert_eq!(spr.header.synctype, 1);

        // One single frame, 2x2, 4 pixels.
        assert_eq!(spr.frames.len(), 1);
        match &spr.frames[0] {
            Frame::Single(f) => {
                assert_eq!(f.origin, [-1, 3]);
                assert_eq!(f.width, 2);
                assert_eq!(f.height, 2);
                assert_eq!(f.pixels.len(), 4);
                assert_eq!(f.pixels, vec![10, 20, 30, 40]);
            }
            Frame::Group { .. } => panic!("expected a single frame"),
        }
    }

    #[test]
    fn parses_group_frame() {
        let mut buf = Vec::new();
        // Header with one top-level (group) frame, max dims 1x1.
        buf.extend_from_slice(b"IDSP");
        push_i32(&mut buf, 1); // version
        push_i32(&mut buf, SPR_ORIENTED); // type
        push_f32(&mut buf, 1.0); // boundingradius
        push_i32(&mut buf, 1); // width
        push_i32(&mut buf, 1); // height
        push_i32(&mut buf, 1); // numframes (1 top-level entry)
        push_f32(&mut buf, 0.0); // beamlength
        push_i32(&mut buf, 0); // synctype

        // frame[0]: SPR_GROUP with 2 sub-frames.
        push_i32(&mut buf, SPR_GROUP); // frametype
        push_i32(&mut buf, 2); // groupcount
        push_f32(&mut buf, 0.1); // interval[0]
        push_f32(&mut buf, 0.2); // interval[1]
        // sub-frame 0: 1x1.
        push_i32(&mut buf, 0); // origin[0]
        push_i32(&mut buf, 0); // origin[1]
        push_i32(&mut buf, 1); // width
        push_i32(&mut buf, 1); // height
        buf.push(7); // 1 pixel
        // sub-frame 1: 1x1.
        push_i32(&mut buf, 5); // origin[0]
        push_i32(&mut buf, 6); // origin[1]
        push_i32(&mut buf, 1); // width
        push_i32(&mut buf, 1); // height
        buf.push(9); // 1 pixel

        let spr = Sprite::parse(&buf).unwrap();
        assert_eq!(spr.frames.len(), 1);
        match &spr.frames[0] {
            Frame::Group { intervals, frames } => {
                assert_eq!(intervals, &vec![0.1, 0.2]);
                assert_eq!(frames.len(), 2);
                assert_eq!(frames[0].pixels, vec![7]);
                assert_eq!(frames[1].origin, [5, 6]);
                assert_eq!(frames[1].pixels, vec![9]);
            }
            Frame::Single(_) => panic!("expected a group frame"),
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = synthetic_single_2x2();
        buf[0] = b'X';
        match Sprite::parse(&buf) {
            Err(QError::BadMagic { expected, .. }) => assert_eq!(expected, "IDSP"),
            other => panic!("expected BadMagic, got {:?}", other),
        }
    }

    #[test]
    fn rejects_wrong_version() {
        let mut buf = synthetic_single_2x2();
        // version is the second i32, bytes 4..8.
        buf[4..8].copy_from_slice(&2i32.to_le_bytes());
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn rejects_zero_numframes() {
        let mut buf = synthetic_single_2x2();
        // numframes is the 7th i32: offset 24..28.
        buf[24..28].copy_from_slice(&0i32.to_le_bytes());
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn rejects_negative_numframes() {
        let mut buf = synthetic_single_2x2();
        buf[24..28].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn rejects_negative_frame_dimensions() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"IDSP");
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 0);
        push_f32(&mut buf, 0.0);
        push_i32(&mut buf, 4);
        push_i32(&mut buf, 4);
        push_i32(&mut buf, 1); // numframes
        push_f32(&mut buf, 0.0);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, SPR_SINGLE); // frametype
        push_i32(&mut buf, 0); // origin[0]
        push_i32(&mut buf, 0); // origin[1]
        push_i32(&mut buf, -1); // negative width
        push_i32(&mut buf, 2); // height
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn rejects_nonpositive_group_interval() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"IDSP");
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 0);
        push_f32(&mut buf, 0.0);
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 1); // numframes
        push_f32(&mut buf, 0.0);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, SPR_GROUP); // frametype
        push_i32(&mut buf, 1); // groupcount
        push_f32(&mut buf, 0.0); // interval <= 0 -> error
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn truncated_pixels_is_error() {
        let mut buf = synthetic_single_2x2();
        // Drop the last pixel byte; the 2x2 frame now lacks one of its 4 pixels.
        buf.pop();
        assert!(Sprite::parse(&buf).is_err());
    }

    #[test]
    fn rejects_unknown_frame_type() {
        let mut buf = synthetic_single_2x2();
        // frametype is the i32 right after the 36-byte header.
        buf[SpriteHeader::SIZE..SpriteHeader::SIZE + 4]
            .copy_from_slice(&99i32.to_le_bytes());
        assert!(Sprite::parse(&buf).is_err());
    }
}
