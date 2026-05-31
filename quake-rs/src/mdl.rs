//! MDL alias-model loader.
//!
//! Ports the on-disk MDL ("alias model") format used for Quake's animated
//! models (players, monsters, weapons, pickups). The struct layout comes from
//! `WinQuake/modelgen.h`; the load order of skins, base ST vertices, triangles,
//! and frames follows `Mod_LoadAliasModel` / `Mod_LoadAliasFrame` /
//! `Mod_LoadAliasGroup` / `Mod_LoadAllSkins` in `WinQuake/model.c`.
//!
//! Ported from Quake (GPLv2). Original C: Copyright (C) 1996-1997 Id Software, Inc.
//!
//! Faithfulness notes:
//! * The on-disk header `mdl_t` is exactly 84 bytes; fields are decoded in the
//!   exact C order through a bounds-checked [`Reader`].
//! * `trivertx_t` is 4 bytes, `stvert_t` is 12 bytes, `dtriangle_t` is 16 bytes,
//!   `daliasframe_t` header is 24 bytes, `daliasgroup_t` header is 12 bytes.
//! * Where the C engine called `Sys_Error` on bad input (wrong version/magic,
//!   `numverts <= 0`, `numtris <= 0`, `numskins < 1`, `numframes < 1`,
//!   `interval <= 0`) we return [`QError`] instead of aborting.
//! * Unlike the C loader we do *not* shift `stvert.s`/`stvert.t` into 16.16
//!   fixed point or scale `size`; this module decodes the raw on-disk values
//!   faithfully and leaves any rendering-time transforms to a higher layer.

use crate::error::{QError, Result};
use crate::math::Vec3;
use crate::read::Reader;

/// `ALIAS_VERSION` from `modelgen.h`.
pub const ALIAS_VERSION: i32 = 6;

/// `IDPOLYHEADER` from `modelgen.h`: little-endian "IDPO".
/// `('O'<<24)+('P'<<16)+('D'<<8)+'I'` as a 4-byte tag on disk.
pub const IDPOLYHEADER: &[u8; 4] = b"IDPO";

/// `ALIAS_SKIN_SINGLE` skin type tag.
pub const ALIAS_SKIN_SINGLE: i32 = 0;
/// `ALIAS_SKIN_GROUP` skin type tag.
pub const ALIAS_SKIN_GROUP: i32 = 1;

/// `ALIAS_SINGLE` frame type tag.
pub const ALIAS_SINGLE: i32 = 0;
/// `ALIAS_GROUP` frame type tag.
pub const ALIAS_GROUP: i32 = 1;

/// `trivertx_t` (4 bytes): a packed model-space vertex plus a lighting normal
/// index. `v` is in the model's quantized 0..255 space (scaled by the header's
/// `scale` / `scale_origin`); `lightnormalindex` indexes the precomputed
/// anorms table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriVertex {
    pub v: [u8; 3],
    pub lightnormalindex: u8,
}

/// `stvert_t` (12 bytes): base texture coordinate for one vertex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StVert {
    /// `ALIAS_ONSEAM` flag (whether the vertex lies on the texture seam).
    pub onseam: i32,
    pub s: i32,
    pub t: i32,
}

/// `dtriangle_t` (16 bytes): one model triangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triangle {
    /// `DT_FACES_FRONT` flag (front-facing vs. back-facing seam triangle).
    pub facesfront: i32,
    pub vertindex: [i32; 3],
}

/// A model skin. Either a single static image or an animated group.
// `Group` holds `Vec<f32>` intervals, so this cannot derive `Eq` (f32: !Eq).
#[derive(Debug, Clone, PartialEq)]
pub enum Skin {
    /// `ALIAS_SKIN_SINGLE`: `skinwidth * skinheight` palette indices.
    Single(Vec<u8>),
    /// `ALIAS_SKIN_GROUP`: per-frame intervals plus a stack of images, each
    /// `skinwidth * skinheight` palette indices.
    Group {
        intervals: Vec<f32>,
        frames: Vec<Vec<u8>>,
    },
}

/// `daliasframe_t` header plus the frame's `numverts` vertices. The bounding
/// box (`bboxmin`/`bboxmax`) uses `trivertx_t`'s `lightnormalindex` as padding,
/// matching the C comment "lightnormal isn't used".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasFrame {
    pub name: String,
    pub bboxmin: TriVertex,
    pub bboxmax: TriVertex,
    pub verts: Vec<TriVertex>,
}

/// A model animation frame. Either a single pose or an animated group.
// `Group` holds `Vec<f32>` intervals, so this cannot derive `Eq` (f32: !Eq).
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// `ALIAS_SINGLE`: one pose.
    Single(AliasFrame),
    /// `ALIAS_GROUP`: per-frame intervals plus a sequence of poses. The group's
    /// own `bboxmin`/`bboxmax` are kept alongside the per-frame ones.
    Group {
        bboxmin: TriVertex,
        bboxmax: TriVertex,
        intervals: Vec<f32>,
        frames: Vec<AliasFrame>,
    },
}

/// The 84-byte on-disk `mdl_t` header, decoded in C field order.
#[derive(Debug, Clone, PartialEq)]
pub struct MdlHeader {
    /// `ident` — must equal `IDPOLYHEADER` ("IDPO").
    pub ident: i32,
    /// `version` — must equal `ALIAS_VERSION` (6).
    pub version: i32,
    pub scale: Vec3,
    pub scale_origin: Vec3,
    pub boundingradius: f32,
    pub eyeposition: Vec3,
    pub numskins: i32,
    pub skinwidth: i32,
    pub skinheight: i32,
    pub numverts: i32,
    pub numtris: i32,
    pub numframes: i32,
    /// `synctype_t` (`ST_SYNC`=0, `ST_RAND`=1) stored as an `int`.
    pub synctype: i32,
    pub flags: i32,
    pub size: f32,
}

impl MdlHeader {
    /// On-disk size of `mdl_t` in bytes.
    pub const SIZE: usize = 84;

    /// Decode the header from the cursor's current position.
    fn read(r: &mut Reader) -> Result<MdlHeader> {
        let ident = r.i32()?;
        let version = r.i32()?;
        let scale = r.vec3()?;
        let scale_origin = r.vec3()?;
        let boundingradius = r.f32()?;
        let eyeposition = r.vec3()?;
        let numskins = r.i32()?;
        let skinwidth = r.i32()?;
        let skinheight = r.i32()?;
        let numverts = r.i32()?;
        let numtris = r.i32()?;
        let numframes = r.i32()?;
        let synctype = r.i32()?;
        let flags = r.i32()?;
        let size = r.f32()?;
        Ok(MdlHeader {
            ident,
            version,
            scale,
            scale_origin,
            boundingradius,
            eyeposition,
            numskins,
            skinwidth,
            skinheight,
            numverts,
            numtris,
            numframes,
            synctype,
            flags,
            size,
        })
    }
}

/// A fully parsed MDL alias model.
#[derive(Debug, Clone, PartialEq)]
pub struct Mdl {
    pub header: MdlHeader,
    pub skins: Vec<Skin>,
    pub stverts: Vec<StVert>,
    pub triangles: Vec<Triangle>,
    pub frames: Vec<Frame>,
}

/// Convert a non-negative `i32` count to `usize`, rejecting negatives.
fn checked_count(value: i32, what: &'static str) -> Result<usize> {
    if value < 0 {
        return Err(QError::invalid(format!("{what} is negative: {value}")));
    }
    Ok(value as usize)
}

/// A safe `Vec::with_capacity` hint for an untrusted `count`.
///
/// A malformed header can claim billions of elements; pre-reserving that much
/// would abort the process (capacity-overflow panic / OOM) before the cursor
/// ever runs off the end. We cap the reservation at `remaining` (an upper bound
/// on how many elements could possibly be decoded from the rest of the buffer),
/// so the eventual error is a clean `Truncated` from the `Reader`.
fn capacity_hint(count: usize, remaining: usize) -> usize {
    count.min(remaining)
}

/// Read a single `trivertx_t` (4 bytes).
fn read_trivertex(r: &mut Reader) -> Result<TriVertex> {
    let v = r.bytes::<3>()?;
    let lightnormalindex = r.u8()?;
    Ok(TriVertex {
        v,
        lightnormalindex,
    })
}

/// Read a single `stvert_t` (12 bytes).
fn read_stvert(r: &mut Reader) -> Result<StVert> {
    let onseam = r.i32()?;
    let s = r.i32()?;
    let t = r.i32()?;
    Ok(StVert { onseam, s, t })
}

/// Read a single `dtriangle_t` (16 bytes).
fn read_triangle(r: &mut Reader) -> Result<Triangle> {
    let facesfront = r.i32()?;
    let vertindex = [r.i32()?, r.i32()?, r.i32()?];
    Ok(Triangle {
        facesfront,
        vertindex,
    })
}

/// Read a `daliasframe_t` header (bboxmin, bboxmax, name[16]) plus `numverts`
/// `trivertx_t`. Mirrors `Mod_LoadAliasFrame`.
fn read_alias_frame(r: &mut Reader, numverts: usize) -> Result<AliasFrame> {
    let bboxmin = read_trivertex(r)?;
    let bboxmax = read_trivertex(r)?;
    let name = r.name(16)?;
    let mut verts = Vec::with_capacity(capacity_hint(numverts, r.remaining() / 4));
    for _ in 0..numverts {
        verts.push(read_trivertex(r)?);
    }
    Ok(AliasFrame {
        name,
        bboxmin,
        bboxmax,
        verts,
    })
}

impl Mdl {
    /// Parse an MDL alias model from an in-memory file image.
    ///
    /// Verifies `ident == IDPOLYHEADER` and `version == ALIAS_VERSION`, then
    /// decodes skins, base ST vertices, triangles, and frames in the on-disk
    /// order established by `Mod_LoadAliasModel`.
    pub fn parse(bytes: &[u8]) -> Result<Mdl> {
        let mut r = Reader::new(bytes);

        let header = MdlHeader::read(&mut r)?;

        // Verify magic. The C reads the first int and compares to
        // IDPOLYHEADER; here we compare the raw 4-byte tag for a clearer error.
        let ident_bytes = header.ident.to_le_bytes();
        if &ident_bytes != IDPOLYHEADER {
            return Err(QError::BadMagic {
                context: "mdl ident",
                found: ident_bytes,
                expected: "IDPO",
            });
        }

        if header.version != ALIAS_VERSION {
            return Err(QError::invalid(format!(
                "mdl has wrong version number ({} should be {ALIAS_VERSION})",
                header.version
            )));
        }

        // Validate counts the way the C loader does (Sys_Error -> Err).
        // numverts <= 0, numtris <= 0 are explicit Sys_Errors in the C.
        if header.numverts <= 0 {
            return Err(QError::invalid("mdl has no vertices"));
        }
        if header.numtris <= 0 {
            return Err(QError::invalid("mdl has no triangles"));
        }
        // numskins < 1 and numframes < 1 are explicit Sys_Errors in the C.
        if header.numskins < 1 {
            return Err(QError::invalid(format!(
                "mdl has invalid # of skins: {}",
                header.numskins
            )));
        }
        if header.numframes < 1 {
            return Err(QError::invalid(format!(
                "mdl has invalid # of frames: {}",
                header.numframes
            )));
        }
        if header.skinwidth < 0 {
            return Err(QError::invalid(format!(
                "mdl has negative skinwidth: {}",
                header.skinwidth
            )));
        }
        if header.skinheight < 0 {
            return Err(QError::invalid(format!(
                "mdl has negative skinheight: {}",
                header.skinheight
            )));
        }
        // NOTE (deliberate deviation): the C `Mod_LoadAliasModel` aborts when
        // `skinwidth & 0x03` (the software rasteriser wants 4-byte-aligned skin
        // rows) and when `skinheight > MAX_LBM_HEIGHT` / `numverts > MAXALIASVERTS`.
        // Those are engine-resource limits, not on-disk-format constraints — the
        // parse is byte-correct without them — so we do not enforce them here.

        let numskins = header.numskins as usize;
        let numverts = header.numverts as usize;
        let numtris = header.numtris as usize;
        let numframes = header.numframes as usize;
        let skinwidth = header.skinwidth as usize;
        let skinheight = header.skinheight as usize;

        // skinsize = skinheight * skinwidth (C: pmodel->skinheight * skinwidth).
        let skinsize = skinheight
            .checked_mul(skinwidth)
            .ok_or_else(|| QError::invalid("mdl skin size overflow"))?;

        //
        // load the skins
        //
        let mut skins = Vec::with_capacity(capacity_hint(numskins, r.remaining() / 4));
        for _ in 0..numskins {
            skins.push(read_skin(&mut r, skinsize)?);
        }

        //
        // base s and t vertices
        //
        let mut stverts = Vec::with_capacity(capacity_hint(numverts, r.remaining() / 12));
        for _ in 0..numverts {
            stverts.push(read_stvert(&mut r)?);
        }

        //
        // triangles
        //
        let mut triangles = Vec::with_capacity(capacity_hint(numtris, r.remaining() / 16));
        for _ in 0..numtris {
            triangles.push(read_triangle(&mut r)?);
        }

        //
        // frames
        //
        let mut frames = Vec::with_capacity(capacity_hint(numframes, r.remaining() / 4));
        for _ in 0..numframes {
            frames.push(read_frame(&mut r, numverts)?);
        }

        Ok(Mdl {
            header,
            skins,
            stverts,
            triangles,
            frames,
        })
    }
}

/// Read one skin entry. Mirrors the dispatch in `Mod_LoadAliasModel`'s skin
/// loop on `pskindesc[i].type`.
fn read_skin(r: &mut Reader, skinsize: usize) -> Result<Skin> {
    let skintype = r.i32()?;
    if skintype == ALIAS_SKIN_SINGLE {
        // Mod_LoadAliasSkin: copy skinsize bytes (r_pixbytes == 1 case).
        let data = r.take(skinsize)?.to_vec();
        Ok(Skin::Single(data))
    } else {
        // ALIAS_SKIN_GROUP / Mod_LoadAliasSkinGroup.
        // daliasskingroup_t: { int numskins }.
        let groupcount = checked_count(r.i32()?, "skin group count")?;
        // numskins f32 intervals.
        let mut intervals = Vec::with_capacity(capacity_hint(groupcount, r.remaining() / 4));
        for _ in 0..groupcount {
            let interval = r.f32()?;
            // C: Sys_Error if interval <= 0.
            if interval <= 0.0 {
                return Err(QError::invalid("mdl skin group interval <= 0"));
            }
            intervals.push(interval);
        }
        // numskins images of skinsize bytes.
        let cap = if skinsize == 0 {
            0
        } else {
            capacity_hint(groupcount, r.remaining() / skinsize)
        };
        let mut group_frames = Vec::with_capacity(cap);
        for _ in 0..groupcount {
            group_frames.push(r.take(skinsize)?.to_vec());
        }
        Ok(Skin::Group {
            intervals,
            frames: group_frames,
        })
    }
}

/// Read one animation frame. Mirrors the dispatch in `Mod_LoadAliasModel`'s
/// frame loop on `pheader->frames[i].type`.
fn read_frame(r: &mut Reader, numverts: usize) -> Result<Frame> {
    let frametype = r.i32()?;
    if frametype == ALIAS_SINGLE {
        // Mod_LoadAliasFrame.
        let frame = read_alias_frame(r, numverts)?;
        Ok(Frame::Single(frame))
    } else {
        // ALIAS_GROUP / Mod_LoadAliasGroup.
        // daliasgroup_t: { int numframes; trivertx bboxmin; trivertx bboxmax }.
        let groupcount = checked_count(r.i32()?, "frame group count")?;
        let bboxmin = read_trivertex(r)?;
        let bboxmax = read_trivertex(r)?;
        // numframes f32 intervals.
        let mut intervals = Vec::with_capacity(capacity_hint(groupcount, r.remaining() / 4));
        for _ in 0..groupcount {
            let interval = r.f32()?;
            // C: Sys_Error if interval <= 0.
            if interval <= 0.0 {
                return Err(QError::invalid("mdl frame group interval <= 0"));
            }
            intervals.push(interval);
        }
        // numframes of (daliasframe header + numverts trivertx).
        // Each sub-frame is at least the 24-byte daliasframe header.
        let mut group_frames = Vec::with_capacity(capacity_hint(groupcount, r.remaining() / 24));
        for _ in 0..groupcount {
            group_frames.push(read_alias_frame(r, numverts)?);
        }
        Ok(Frame::Group {
            bboxmin,
            bboxmax,
            intervals,
            frames: group_frames,
        })
    }
}

impl Mdl {
    /// Resolve which pose to draw for animation `frame` at game `time`, porting
    /// `R_AliasSetupFrame` (`r_alias.c`).
    ///
    /// First the requested `frame` is range-checked: the C resets an out-of-range
    /// frame (`frame >= numframes || frame < 0`) to **0** (it does NOT clamp to the
    /// last frame), so we do the same. Then:
    ///  * a [`Frame::Single`] returns its one pose's vertices;
    ///  * a [`Frame::Group`] selects the sub-pose whose interval window contains
    ///    `time` — the C computes `fullinterval = intervals[n-1]`,
    ///    `targettime = time - floor(time/fullinterval)*fullinterval`, then picks
    ///    the first `i` in `0..n-1` with `intervals[i] > targettime` (else the last
    ///    sub-frame). `syncbase` (a per-entity random phase) is folded into `time`
    ///    by the caller.
    ///
    /// Returns `None` only for a frameless model (or an empty group).
    pub fn frame_pose(&self, frame: i32, time: f32) -> Option<&[TriVertex]> {
        let idx = if frame < 0 || (frame as usize) >= self.frames.len() {
            0
        } else {
            frame as usize
        };
        match self.frames.get(idx)? {
            Frame::Single(af) => Some(&af.verts),
            Frame::Group { intervals, frames, .. } => {
                let sub = select_interval(intervals, frames.len(), time)?;
                frames.get(sub).map(|af| af.verts.as_slice())
            }
        }
    }

    /// Resolve which skin image to use for skin `skinnum` at game `time`, porting
    /// `R_AliasSetupSkin` (`r_alias.c`).
    ///
    /// `skinnum` is range-checked the same way as a frame (out of range -> 0). A
    /// single skin returns its pixels; a skin group selects the image whose
    /// interval window contains `time` (same rule as [`Self::frame_pose`]).
    ///
    /// Returns `None` only for a model with no skins (or an empty skin group).
    pub fn skin_image(&self, skinnum: i32, time: f32) -> Option<&[u8]> {
        let idx = if skinnum < 0 || (skinnum as usize) >= self.skins.len() {
            0
        } else {
            skinnum as usize
        };
        match self.skins.get(idx)? {
            Skin::Single(px) => Some(px),
            Skin::Group { intervals, frames } => {
                let sub = select_interval(intervals, frames.len(), time)?;
                frames.get(sub).map(|v| v.as_slice())
            }
        }
    }
}

/// Select the sub-index of a group (frame or skin) for game `time` from its
/// per-sub-frame `intervals`, porting the shared `R_AliasSetupFrame` /
/// `R_AliasSetupSkin` selection in `r_alias.c`.
///
/// `intervals[i]` is the cumulative time at which sub-frame `i` ends. The loader
/// stores the on-disk intervals VERBATIM (`Mod_LoadAliasGroup` copies each
/// `LittleFloat` value with no accumulation, as `read_frame`/`read_skin` do here);
/// the model tools author them as cumulative end-times, which is why the selection
/// below works. `fullinterval = intervals[n-1]` is the cycle length; `targettime =
/// time - floor(time/fullinterval)*fullinterval` wraps `time` into
/// `[0, fullinterval)`. The chosen sub-frame is the first `i` with
/// `intervals[i] > targettime` (else the last, `n-1`).
///
/// SAFETY: a non-finite or non-positive `fullinterval`, an empty group, or a
/// `len`/`intervals` mismatch all fall back to sub-frame 0, so a malformed model
/// never panics or divides by zero.
fn select_interval(intervals: &[f32], len: usize, time: f32) -> Option<usize> {
    if len == 0 {
        return None;
    }
    // Need a usable interval table; otherwise hold on the first sub-frame.
    let full = match intervals.last() {
        Some(&f) if f > 0.0 && f.is_finite() => f,
        _ => return Some(0),
    };
    let t = if time.is_finite() { time } else { 0.0 };
    let target = t - (t / full).floor() * full;
    let upper = len.saturating_sub(1).min(intervals.len());
    for i in 0..upper {
        if intervals[i] > target {
            return Some(i);
        }
    }
    Some(len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- byte-buffer builders -------------------------------------------

    fn push_i32(buf: &mut Vec<u8>, v: i32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    fn push_f32(buf: &mut Vec<u8>, v: f32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    fn push_vec3(buf: &mut Vec<u8>, v: [f32; 3]) {
        for c in v {
            push_f32(buf, c);
        }
    }
    fn push_trivertex(buf: &mut Vec<u8>, v: [u8; 3], lni: u8) {
        buf.extend_from_slice(&v);
        buf.push(lni);
    }
    fn push_name16(buf: &mut Vec<u8>, name: &str) {
        let mut field = [0u8; 16];
        let bytes = name.as_bytes();
        let n = bytes.len().min(15); // leave at least one NUL
        field[..n].copy_from_slice(&bytes[..n]);
        buf.extend_from_slice(&field);
    }

    /// Header constants used by the canonical synthetic model.
    const SW: i32 = 2;
    const SH: i32 = 2;
    const NV: i32 = 3;
    const NT: i32 = 1;

    /// Build the spec's canonical MDL: skinwidth=2, skinheight=2, numverts=3,
    /// numtris=1, numskins=1 (single skin), numframes=1 (single frame).
    fn build_canonical() -> Vec<u8> {
        let mut buf = Vec::new();

        // --- header (mdl_t, 84 bytes) ---
        buf.extend_from_slice(IDPOLYHEADER); // ident
        push_i32(&mut buf, ALIAS_VERSION); // version
        push_vec3(&mut buf, [1.0, 2.0, 3.0]); // scale
        push_vec3(&mut buf, [4.0, 5.0, 6.0]); // scale_origin
        push_f32(&mut buf, 7.5); // boundingradius
        push_vec3(&mut buf, [8.0, 9.0, 10.0]); // eyeposition
        push_i32(&mut buf, 1); // numskins
        push_i32(&mut buf, SW); // skinwidth
        push_i32(&mut buf, SH); // skinheight
        push_i32(&mut buf, NV); // numverts
        push_i32(&mut buf, NT); // numtris
        push_i32(&mut buf, 1); // numframes
        push_i32(&mut buf, 1); // synctype (ST_RAND)
        push_i32(&mut buf, 42); // flags
        push_f32(&mut buf, 0.5); // size
        assert_eq!(buf.len(), MdlHeader::SIZE);

        // --- skins (1 single skin: type=0, then sw*sh bytes) ---
        push_i32(&mut buf, ALIAS_SKIN_SINGLE);
        let skinsize = (SW * SH) as usize; // 4
        buf.extend_from_slice(&[10, 20, 30, 40][..skinsize]);

        // --- stverts (numverts * stvert_t) ---
        for i in 0..NV {
            push_i32(&mut buf, i); // onseam
            push_i32(&mut buf, 100 + i); // s
            push_i32(&mut buf, 200 + i); // t
        }

        // --- triangles (numtris * dtriangle_t) ---
        push_i32(&mut buf, 1); // facesfront
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 2);

        // --- frames (1 single frame: type=0, then daliasframe + numverts verts) ---
        push_i32(&mut buf, ALIAS_SINGLE);
        push_trivertex(&mut buf, [0, 0, 0], 0); // bboxmin
        push_trivertex(&mut buf, [255, 255, 255], 0); // bboxmax
        push_name16(&mut buf, "frame1");
        for i in 0..NV as u8 {
            push_trivertex(&mut buf, [i, i + 1, i + 2], i + 3);
        }

        buf
    }

    #[test]
    fn parses_canonical_single_skin_single_frame() {
        let buf = build_canonical();
        let mdl = Mdl::parse(&buf).expect("parse should succeed");

        // header fields
        assert_eq!(mdl.header.ident.to_le_bytes(), *IDPOLYHEADER);
        assert_eq!(mdl.header.version, ALIAS_VERSION);
        assert_eq!(mdl.header.scale, [1.0, 2.0, 3.0]);
        assert_eq!(mdl.header.scale_origin, [4.0, 5.0, 6.0]);
        assert_eq!(mdl.header.boundingradius, 7.5);
        assert_eq!(mdl.header.eyeposition, [8.0, 9.0, 10.0]);
        assert_eq!(mdl.header.numskins, 1);
        assert_eq!(mdl.header.skinwidth, 2);
        assert_eq!(mdl.header.skinheight, 2);
        assert_eq!(mdl.header.numverts, 3);
        assert_eq!(mdl.header.numtris, 1);
        assert_eq!(mdl.header.numframes, 1);
        assert_eq!(mdl.header.synctype, 1);
        assert_eq!(mdl.header.flags, 42);
        assert_eq!(mdl.header.size, 0.5);

        // skins[0] is a single 4-byte skin
        assert_eq!(mdl.skins.len(), 1);
        match &mdl.skins[0] {
            Skin::Single(data) => assert_eq!(data, &vec![10u8, 20, 30, 40]),
            other => panic!("expected single skin, got {other:?}"),
        }

        // stverts
        assert_eq!(mdl.stverts.len(), 3);
        assert_eq!(mdl.stverts[0], StVert { onseam: 0, s: 100, t: 200 });
        assert_eq!(mdl.stverts[2], StVert { onseam: 2, s: 102, t: 202 });

        // triangles
        assert_eq!(mdl.triangles.len(), 1);
        assert_eq!(
            mdl.triangles[0],
            Triangle { facesfront: 1, vertindex: [0, 1, 2] }
        );

        // frames[0] is a single frame with 3 verts
        assert_eq!(mdl.frames.len(), 1);
        match &mdl.frames[0] {
            Frame::Single(f) => {
                assert_eq!(f.name, "frame1");
                assert_eq!(f.verts.len(), 3);
                assert_eq!(f.bboxmin, TriVertex { v: [0, 0, 0], lightnormalindex: 0 });
                assert_eq!(
                    f.bboxmax,
                    TriVertex { v: [255, 255, 255], lightnormalindex: 0 }
                );
                assert_eq!(f.verts[0], TriVertex { v: [0, 1, 2], lightnormalindex: 3 });
                assert_eq!(f.verts[2], TriVertex { v: [2, 3, 4], lightnormalindex: 5 });
            }
            other => panic!("expected single frame, got {other:?}"),
        }
    }

    #[test]
    fn parses_skin_group_and_frame_group() {
        // numskins=1 (group of 2), numverts=2, numtris=1, numframes=1 (group of 2),
        // skinwidth=2, skinheight=1 -> skinsize=2.
        let mut buf = Vec::new();
        let (sw, sh, nv, nt) = (2i32, 1i32, 2i32, 1i32);

        buf.extend_from_slice(IDPOLYHEADER);
        push_i32(&mut buf, ALIAS_VERSION);
        push_vec3(&mut buf, [0.0, 0.0, 0.0]);
        push_vec3(&mut buf, [0.0, 0.0, 0.0]);
        push_f32(&mut buf, 0.0);
        push_vec3(&mut buf, [0.0, 0.0, 0.0]);
        push_i32(&mut buf, 1); // numskins
        push_i32(&mut buf, sw);
        push_i32(&mut buf, sh);
        push_i32(&mut buf, nv);
        push_i32(&mut buf, nt);
        push_i32(&mut buf, 1); // numframes
        push_i32(&mut buf, 0); // synctype
        push_i32(&mut buf, 0); // flags
        push_f32(&mut buf, 1.0); // size
        assert_eq!(buf.len(), MdlHeader::SIZE);

        // skin group: type=1, groupcount=2, 2 intervals, 2 images of skinsize=2
        push_i32(&mut buf, ALIAS_SKIN_GROUP);
        push_i32(&mut buf, 2); // groupcount
        push_f32(&mut buf, 0.1);
        push_f32(&mut buf, 0.2);
        buf.extend_from_slice(&[1, 2]); // image 0
        buf.extend_from_slice(&[3, 4]); // image 1

        // stverts (2)
        for i in 0..nv {
            push_i32(&mut buf, 0);
            push_i32(&mut buf, i);
            push_i32(&mut buf, i);
        }

        // triangles (1)
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 0);

        // frame group: type=1, groupcount=2, group bbox, 2 intervals, 2 frames
        push_i32(&mut buf, ALIAS_GROUP);
        push_i32(&mut buf, 2); // groupcount
        push_trivertex(&mut buf, [1, 1, 1], 9); // group bboxmin
        push_trivertex(&mut buf, [2, 2, 2], 9); // group bboxmax
        push_f32(&mut buf, 0.5);
        push_f32(&mut buf, 0.75);
        for fi in 0..2u8 {
            push_trivertex(&mut buf, [0, 0, 0], 0); // frame bboxmin
            push_trivertex(&mut buf, [9, 9, 9], 0); // frame bboxmax
            push_name16(&mut buf, &format!("g{fi}"));
            for vi in 0..nv as u8 {
                push_trivertex(&mut buf, [fi, vi, fi + vi], 0);
            }
        }

        let mdl = Mdl::parse(&buf).expect("parse should succeed");

        match &mdl.skins[0] {
            Skin::Group { intervals, frames } => {
                assert_eq!(intervals, &vec![0.1f32, 0.2]);
                assert_eq!(frames.len(), 2);
                assert_eq!(frames[0], vec![1u8, 2]);
                assert_eq!(frames[1], vec![3u8, 4]);
            }
            other => panic!("expected skin group, got {other:?}"),
        }

        assert_eq!(mdl.stverts.len(), 2);
        assert_eq!(mdl.triangles.len(), 1);

        match &mdl.frames[0] {
            Frame::Group {
                bboxmin,
                bboxmax,
                intervals,
                frames,
            } => {
                assert_eq!(*bboxmin, TriVertex { v: [1, 1, 1], lightnormalindex: 9 });
                assert_eq!(*bboxmax, TriVertex { v: [2, 2, 2], lightnormalindex: 9 });
                assert_eq!(intervals, &vec![0.5f32, 0.75]);
                assert_eq!(frames.len(), 2);
                assert_eq!(frames[0].name, "g0");
                assert_eq!(frames[1].name, "g1");
                assert_eq!(frames[0].verts.len(), 2);
                assert_eq!(frames[1].verts[1], TriVertex { v: [1, 1, 2], lightnormalindex: 0 });
            }
            other => panic!("expected frame group, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = build_canonical();
        buf[0] = b'X'; // corrupt ident
        match Mdl::parse(&buf) {
            Err(QError::BadMagic { context, .. }) => assert_eq!(context, "mdl ident"),
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_version() {
        let mut buf = build_canonical();
        // version is at offset 4..8
        buf[4..8].copy_from_slice(&5i32.to_le_bytes());
        assert!(matches!(Mdl::parse(&buf), Err(QError::Invalid(_))));
    }

    #[test]
    fn rejects_negative_numverts() {
        let mut buf = build_canonical();
        // numverts is the 4th int after the 12 floats (scale, scale_origin,
        // boundingradius, eyeposition) and skins counts. Easier: rebuild with
        // patched field. Layout offset of numverts:
        // ident(4)+version(4)+scale(12)+scale_origin(12)+boundingradius(4)
        // +eyeposition(12)+numskins(4)+skinwidth(4)+skinheight(4) = 60.
        buf[60..64].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(matches!(Mdl::parse(&buf), Err(QError::Invalid(_))));
    }

    #[test]
    fn truncated_buffer_is_an_error() {
        let buf = build_canonical();
        // Drop the last byte so the final frame vertex can't be read.
        let truncated = &buf[..buf.len() - 1];
        assert!(Mdl::parse(truncated).is_err());
    }

    #[test]
    fn empty_buffer_is_an_error() {
        assert!(Mdl::parse(&[]).is_err());
    }

    // --- animation selection (frame_pose / skin_image) -------------------

    #[test]
    fn select_interval_picks_by_cumulative_time() {
        // Cumulative intervals [0.1, 0.2, 0.3]: sub 0 for t<0.1, sub 1 for
        // 0.1<=t<0.2, sub 2 for 0.2<=t<0.3, wrapping at 0.3.
        let iv = [0.1f32, 0.2, 0.3];
        assert_eq!(select_interval(&iv, 3, 0.05), Some(0));
        assert_eq!(select_interval(&iv, 3, 0.15), Some(1));
        assert_eq!(select_interval(&iv, 3, 0.25), Some(2));
        // Wraps: 0.35 -> 0.05 -> sub 0.
        assert_eq!(select_interval(&iv, 3, 0.35), Some(0));
    }

    #[test]
    fn select_interval_tolerates_bad_table() {
        // Empty group -> None; non-positive/NaN fullinterval -> sub 0.
        assert_eq!(select_interval(&[], 0, 1.0), None);
        assert_eq!(select_interval(&[0.0f32], 1, 1.0), Some(0));
        assert_eq!(select_interval(&[f32::NAN], 1, 1.0), Some(0));
        assert_eq!(select_interval(&[0.5f32], 1, f32::INFINITY), Some(0));
    }

    /// A model with one ALIAS_GROUP frame of two sub-poses and two distinct
    /// vertex sets, plus a one-skin model — to exercise group animation.
    fn build_grouped() -> Vec<u8> {
        // numskins=1, numverts=1, numtris=1, numframes=1 (a group of 2),
        // skinwidth=1, skinheight=1 -> skinsize=1.
        let mut buf = Vec::new();
        let (sw, sh, nv, nt) = (1i32, 1i32, 1i32, 1i32);
        buf.extend_from_slice(IDPOLYHEADER);
        push_i32(&mut buf, ALIAS_VERSION);
        push_vec3(&mut buf, [1.0, 1.0, 1.0]);
        push_vec3(&mut buf, [0.0, 0.0, 0.0]);
        push_f32(&mut buf, 0.0);
        push_vec3(&mut buf, [0.0, 0.0, 0.0]);
        push_i32(&mut buf, 1); // numskins
        push_i32(&mut buf, sw);
        push_i32(&mut buf, sh);
        push_i32(&mut buf, nv);
        push_i32(&mut buf, nt);
        push_i32(&mut buf, 1); // numframes
        push_i32(&mut buf, 0); // synctype
        push_i32(&mut buf, 0); // flags
        push_f32(&mut buf, 1.0); // size
        // skin (single, 1 byte)
        push_i32(&mut buf, ALIAS_SKIN_SINGLE);
        buf.push(7);
        // stvert (1)
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 0);
        // triangle (1)
        push_i32(&mut buf, 1);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 0);
        push_i32(&mut buf, 0);
        // frame group: type=1, count=2, group bbox, cumulative intervals 0.1/0.2,
        // 2 sub-poses (one vert each) with distinct v[0].
        push_i32(&mut buf, ALIAS_GROUP);
        push_i32(&mut buf, 2);
        push_trivertex(&mut buf, [0, 0, 0], 0);
        push_trivertex(&mut buf, [9, 9, 9], 0);
        push_f32(&mut buf, 0.1);
        push_f32(&mut buf, 0.2);
        for v0 in [10u8, 20] {
            push_trivertex(&mut buf, [0, 0, 0], 0); // sub bboxmin
            push_trivertex(&mut buf, [9, 9, 9], 0); // sub bboxmax
            push_name16(&mut buf, "sub");
            push_trivertex(&mut buf, [v0, 0, 0], 0);
        }
        buf
    }

    #[test]
    fn frame_pose_animates_group_by_time() {
        let mdl = Mdl::parse(&build_grouped()).expect("parse grouped");
        // t in [0,0.1) -> sub 0 (v=10); t in [0.1,0.2) -> sub 1 (v=20).
        let a = mdl.frame_pose(0, 0.05).expect("pose");
        assert_eq!(a[0].v[0], 10);
        let b = mdl.frame_pose(0, 0.15).expect("pose");
        assert_eq!(b[0].v[0], 20);
        // Out-of-range frame resets to 0 (NOT clamped to last), so still group 0.
        let c = mdl.frame_pose(99, 0.15).expect("pose");
        assert_eq!(c[0].v[0], 20);
        let d = mdl.frame_pose(-3, 0.05).expect("pose");
        assert_eq!(d[0].v[0], 10);
    }

    #[test]
    fn skin_image_resolves_and_resets_out_of_range() {
        let mdl = Mdl::parse(&build_grouped()).expect("parse grouped");
        // Single skin -> its one byte regardless of time.
        assert_eq!(mdl.skin_image(0, 0.0), Some(&[7u8][..]));
        // Out-of-range skinnum resets to 0 (not an error).
        assert_eq!(mdl.skin_image(5, 0.0), Some(&[7u8][..]));
        assert_eq!(mdl.skin_image(-1, 0.0), Some(&[7u8][..]));
    }
}
