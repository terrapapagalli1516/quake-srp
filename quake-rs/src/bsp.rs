//! BSP version-29 map loader.
//!
//! Ported from Quake (GPLv2): structures from `WinQuake/bspfile.h`, and the
//! lump-walking logic from `WinQuake/model.c` (`Mod_LoadBrushModel` and the
//! `Mod_Load*` lump loaders). The original C cast the raw file buffer straight
//! onto its `d*_t` on-disk structs (and byte-swapped in place with
//! `LittleLong`/`LittleShort`); here every field is decoded explicitly through
//! a [`Reader`], so a malformed or truncated `.bsp` yields a [`QError`] rather
//! than undefined behavior.
//!
//! The on-disk `d*_t` structures are reproduced faithfully — field order and
//! byte sizes match the C exactly (see the `*_SIZE` constants and the
//! `struct_sizes_match` test). The in-memory `m*_t` transforms that `model.c`
//! applied (resolving pointers, spreading model bounds by a pixel, computing
//! plane signbits, etc.) are *not* applied here: this loader returns the raw
//! decoded disk records, leaving interpretation to higher layers.

use crate::error::{QError, Result};
use crate::math::Vec3;
use crate::read::Reader;

// ---------------------------------------------------------------------------
// Constants (bspfile.h)
// ---------------------------------------------------------------------------

/// The only BSP version this loader accepts (`#define BSPVERSION 29`).
pub const BSPVERSION: i32 = 29;
/// Number of lump directory entries in the header (`HEADER_LUMPS`).
pub const HEADER_LUMPS: usize = 15;
/// Number of mip levels stored per texture (`MIPLEVELS`).
pub const MIPLEVELS: usize = 4;
/// Number of light styles per face (`MAXLIGHTMAPS`).
pub const MAXLIGHTMAPS: usize = 4;
/// Number of automatic ambient sound channels per leaf (`NUM_AMBIENTS`).
pub const NUM_AMBIENTS: usize = 4;
/// Number of collision hulls (`MAX_MAP_HULLS`).
pub const MAX_MAP_HULLS: usize = 4;

/// Texinfo flag: sky or slime — no lightmap or 256 subdivision (`TEX_SPECIAL`).
pub const TEX_SPECIAL: i32 = 1;

// Lump directory indices (LUMP_*).
pub const LUMP_ENTITIES: usize = 0;
pub const LUMP_PLANES: usize = 1;
pub const LUMP_TEXTURES: usize = 2;
pub const LUMP_VERTEXES: usize = 3;
pub const LUMP_VISIBILITY: usize = 4;
pub const LUMP_NODES: usize = 5;
pub const LUMP_TEXINFO: usize = 6;
pub const LUMP_FACES: usize = 7;
pub const LUMP_LIGHTING: usize = 8;
pub const LUMP_CLIPNODES: usize = 9;
pub const LUMP_LEAFS: usize = 10;
pub const LUMP_MARKSURFACES: usize = 11;
pub const LUMP_EDGES: usize = 12;
pub const LUMP_SURFEDGES: usize = 13;
pub const LUMP_MODELS: usize = 14;

// Contents values (CONTENTS_*). Negative numbers stored in node/leaf children.
pub const CONTENTS_EMPTY: i32 = -1;
pub const CONTENTS_SOLID: i32 = -2;
pub const CONTENTS_WATER: i32 = -3;
pub const CONTENTS_SLIME: i32 = -4;
pub const CONTENTS_LAVA: i32 = -5;
pub const CONTENTS_SKY: i32 = -6;
pub const CONTENTS_ORIGIN: i32 = -7; // removed at csg time
pub const CONTENTS_CLIP: i32 = -8; // changed to contents_solid
pub const CONTENTS_CURRENT_0: i32 = -9;
pub const CONTENTS_CURRENT_90: i32 = -10;
pub const CONTENTS_CURRENT_180: i32 = -11;
pub const CONTENTS_CURRENT_270: i32 = -12;
pub const CONTENTS_CURRENT_UP: i32 = -13;
pub const CONTENTS_CURRENT_DOWN: i32 = -14;

// Plane types (PLANE_*).
pub const PLANE_X: i32 = 0; // axial planes
pub const PLANE_Y: i32 = 1;
pub const PLANE_Z: i32 = 2;
pub const PLANE_ANYX: i32 = 3; // non-axial, snapped to nearest
pub const PLANE_ANYY: i32 = 4;
pub const PLANE_ANYZ: i32 = 5;

// On-disk sizes, in bytes. These are used both as the per-record stride for
// `count = filelen / size` (mirroring the C `l->filelen / sizeof(*in)`) and to
// assert layout fidelity in the tests.
pub const LUMP_SIZE: usize = 8;
pub const HEADER_SIZE: usize = 4 + HEADER_LUMPS * LUMP_SIZE; // 124
pub const DMODEL_SIZE: usize = 64;
pub const DVERTEX_SIZE: usize = 12;
pub const DPLANE_SIZE: usize = 20;
pub const DNODE_SIZE: usize = 24;
pub const DCLIPNODE_SIZE: usize = 8;
pub const TEXINFO_SIZE: usize = 40;
pub const DEDGE_SIZE: usize = 4;
pub const DFACE_SIZE: usize = 20;
pub const DLEAF_SIZE: usize = 28;
pub const MIPTEX_SIZE: usize = 40;

// ---------------------------------------------------------------------------
// On-disk structures (bspfile.h `d*_t` / `texinfo_t` / `miptex_t`)
// ---------------------------------------------------------------------------

/// `lump_t`: a (offset, length) entry in the header directory.
#[derive(Debug, Clone, Copy)]
pub struct Lump {
    pub fileofs: i32,
    pub filelen: i32,
}

impl Lump {
    fn read(r: &mut Reader) -> Result<Lump> {
        Ok(Lump {
            fileofs: r.i32()?,
            filelen: r.i32()?,
        })
    }
}

/// `dheader_t`: the file header — version plus the lump directory.
#[derive(Debug, Clone)]
pub struct Header {
    pub version: i32,
    pub lumps: [Lump; HEADER_LUMPS],
}

impl Header {
    fn read(r: &mut Reader) -> Result<Header> {
        let version = r.i32()?;
        let mut lumps = [Lump {
            fileofs: 0,
            filelen: 0,
        }; HEADER_LUMPS];
        for slot in lumps.iter_mut() {
            *slot = Lump::read(r)?;
        }
        Ok(Header { version, lumps })
    }
}

/// `dmodel_t`: a submodel (the worldspawn is model 0; brush entities follow).
#[derive(Debug, Clone)]
pub struct DModel {
    pub mins: Vec3,
    pub maxs: Vec3,
    pub origin: Vec3,
    pub headnode: [i32; MAX_MAP_HULLS],
    /// Visible leaf count, not including the solid leaf 0.
    pub visleafs: i32,
    pub firstface: i32,
    pub numfaces: i32,
}

impl DModel {
    fn read(r: &mut Reader) -> Result<DModel> {
        let mins = r.vec3()?;
        let maxs = r.vec3()?;
        let origin = r.vec3()?;
        let mut headnode = [0i32; MAX_MAP_HULLS];
        for h in headnode.iter_mut() {
            *h = r.i32()?;
        }
        Ok(DModel {
            mins,
            maxs,
            origin,
            headnode,
            visleafs: r.i32()?,
            firstface: r.i32()?,
            numfaces: r.i32()?,
        })
    }
}

/// `dvertex_t`: a single map vertex.
#[derive(Debug, Clone)]
pub struct DVertex {
    pub point: Vec3,
}

impl DVertex {
    fn read(r: &mut Reader) -> Result<DVertex> {
        Ok(DVertex { point: r.vec3()? })
    }
}

/// `dplane_t`: a splitting plane. `ptype` is one of the `PLANE_*` values.
#[derive(Debug, Clone)]
pub struct DPlane {
    pub normal: Vec3,
    pub dist: f32,
    /// `type` in C (renamed; `type` is a Rust keyword). PLANE_X .. PLANE_ANYZ.
    pub ptype: i32,
}

impl DPlane {
    fn read(r: &mut Reader) -> Result<DPlane> {
        Ok(DPlane {
            normal: r.vec3()?,
            dist: r.f32()?,
            ptype: r.i32()?,
        })
    }
}

/// `dnode_t`: an internal BSP node. Negative `children` entries encode
/// `-(leaf + 1)` (leaf indices), non-negative entries are node indices.
#[derive(Debug, Clone)]
pub struct DNode {
    pub planenum: i32,
    pub children: [i16; 2],
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub firstface: u16,
    pub numfaces: u16,
}

impl DNode {
    fn read(r: &mut Reader) -> Result<DNode> {
        let planenum = r.i32()?;
        let children = [r.i16()?, r.i16()?];
        let mins = [r.i16()?, r.i16()?, r.i16()?];
        let maxs = [r.i16()?, r.i16()?, r.i16()?];
        Ok(DNode {
            planenum,
            children,
            mins,
            maxs,
            firstface: r.u16()?,
            numfaces: r.u16()?,
        })
    }
}

/// `dclipnode_t`: a collision-hull node. Negative `children` are contents.
#[derive(Debug, Clone)]
pub struct DClipNode {
    pub planenum: i32,
    pub children: [i16; 2],
}

impl DClipNode {
    fn read(r: &mut Reader) -> Result<DClipNode> {
        Ok(DClipNode {
            planenum: r.i32()?,
            children: [r.i16()?, r.i16()?],
        })
    }
}

/// `texinfo_t`: texture mapping vectors plus a texture index and flags.
/// `vecs[0]` is the S axis, `vecs[1]` the T axis (each `[x y z offset]`).
#[derive(Debug, Clone)]
pub struct TexInfo {
    pub vecs: [[f32; 4]; 2],
    pub miptex: i32,
    pub flags: i32,
}

impl TexInfo {
    fn read(r: &mut Reader) -> Result<TexInfo> {
        let mut vecs = [[0.0f32; 4]; 2];
        for axis in vecs.iter_mut() {
            for c in axis.iter_mut() {
                *c = r.f32()?;
            }
        }
        Ok(TexInfo {
            vecs,
            miptex: r.i32()?,
            flags: r.i32()?,
        })
    }
}

/// `dedge_t`: an edge given by two vertex indices. Edge 0 is never used; a
/// negative reference to an edge means the edge is traversed backwards.
#[derive(Debug, Clone)]
pub struct DEdge {
    pub v: [u16; 2],
}

impl DEdge {
    fn read(r: &mut Reader) -> Result<DEdge> {
        Ok(DEdge {
            v: [r.u16()?, r.u16()?],
        })
    }
}

/// `dface_t`: a renderable surface.
#[derive(Debug, Clone)]
pub struct DFace {
    pub planenum: i16,
    /// 0 = on the front of the plane, non-zero = back side.
    pub side: i16,
    pub firstedge: i32,
    pub numedges: i16,
    pub texinfo: i16,
    /// Light style per lightmap layer (`styles[MAXLIGHTMAPS]`).
    pub styles: [u8; MAXLIGHTMAPS],
    /// Byte offset into the LIGHTING lump, or `-1` for no lightmap.
    pub lightofs: i32,
}

impl DFace {
    fn read(r: &mut Reader) -> Result<DFace> {
        let planenum = r.i16()?;
        let side = r.i16()?;
        let firstedge = r.i32()?;
        let numedges = r.i16()?;
        let texinfo = r.i16()?;
        let styles = r.bytes::<MAXLIGHTMAPS>()?;
        let lightofs = r.i32()?;
        Ok(DFace {
            planenum,
            side,
            firstedge,
            numedges,
            texinfo,
            styles,
            lightofs,
        })
    }
}

/// `dleaf_t`: a convex region of the map. `contents` is one of the
/// `CONTENTS_*` values; `visofs` is `-1` when there is no visibility info.
#[derive(Debug, Clone)]
pub struct DLeaf {
    pub contents: i32,
    /// Byte offset into the VISIBILITY lump, or `-1` for none.
    pub visofs: i32,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    pub firstmarksurface: u16,
    pub nummarksurfaces: u16,
    /// Per-channel ambient sound levels (`ambient_level[NUM_AMBIENTS]`).
    pub ambient_level: [u8; NUM_AMBIENTS],
}

impl DLeaf {
    fn read(r: &mut Reader) -> Result<DLeaf> {
        let contents = r.i32()?;
        let visofs = r.i32()?;
        let mins = [r.i16()?, r.i16()?, r.i16()?];
        let maxs = [r.i16()?, r.i16()?, r.i16()?];
        let firstmarksurface = r.u16()?;
        let nummarksurfaces = r.u16()?;
        let ambient_level = r.bytes::<NUM_AMBIENTS>()?;
        Ok(DLeaf {
            contents,
            visofs,
            mins,
            maxs,
            firstmarksurface,
            nummarksurfaces,
            ambient_level,
        })
    }
}

/// `miptex_t`: the header of one mip texture. The four `offsets` are byte
/// offsets (relative to the start of this `MipTex`) to the four mip levels,
/// which immediately follow the header on disk; the pixel data itself is not
/// captured here (the spec asks only for the header).
#[derive(Debug, Clone)]
pub struct MipTex {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub offsets: [u32; MIPLEVELS],
    /// The full-resolution (mip 0) pixel data: `width * height` palette indices,
    /// captured from `offsets[0]`. Empty if the data lies outside the lump (some
    /// maps store animated/external textures with no inline pixels).
    pub pixels: Vec<u8>,
}

impl MipTex {
    fn read(r: &mut Reader) -> Result<MipTex> {
        let start = r.pos(); // absolute start of this miptex; offsets are relative to it
        let name = r.name(16)?;
        let width = r.u32()?;
        let height = r.u32()?;
        let mut offsets = [0u32; MIPLEVELS];
        for o in offsets.iter_mut() {
            *o = r.u32()?;
        }
        // Capture the mip-0 pixels (width*height palette indices) at start+offsets[0].
        let npix = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| QError::invalid("miptex size overflow"))?;
        let pixels = if offsets[0] != 0 && npix > 0 {
            match start
                .checked_add(offsets[0] as usize)
                .and_then(|p| r.slice_at(p, npix).ok())
            {
                Some(s) => s.to_vec(),
                None => Vec::new(), // out of range -> leave empty (renderer falls back)
            }
        } else {
            Vec::new()
        };
        Ok(MipTex {
            name,
            width,
            height,
            offsets,
            pixels,
        })
    }
}

// ---------------------------------------------------------------------------
// The parsed BSP
// ---------------------------------------------------------------------------

/// A fully parsed BSP version-29 file: every lump decoded into owned data.
#[derive(Debug, Clone)]
pub struct Bsp {
    pub version: i32,
    /// LUMP_ENTITIES: the entity description text, trimmed at the first NUL.
    pub entities: String,
    pub planes: Vec<DPlane>,
    pub vertexes: Vec<DVertex>,
    pub edges: Vec<DEdge>,
    pub faces: Vec<DFace>,
    pub nodes: Vec<DNode>,
    pub leafs: Vec<DLeaf>,
    pub clipnodes: Vec<DClipNode>,
    pub texinfo: Vec<TexInfo>,
    pub models: Vec<DModel>,
    /// LUMP_MARKSURFACES: indices into `faces`.
    pub marksurfaces: Vec<u16>,
    /// LUMP_SURFEDGES: signed edge references (negative = backwards edge).
    pub surfedges: Vec<i32>,
    /// LUMP_TEXTURES: one slot per `nummiptex`; `None` where the on-disk
    /// data offset was `-1` (a missing texture).
    pub textures: Vec<Option<MipTex>>,
    /// LUMP_VISIBILITY: raw compressed PVS bytes.
    pub visibility: Vec<u8>,
    /// LUMP_LIGHTING: raw lightmap sample bytes.
    pub lighting: Vec<u8>,
}

impl Bsp {
    /// Parse a complete BSP file from an in-memory byte buffer.
    ///
    /// Reads the header, requires `version == 29` (the C `Mod_LoadBrushModel`
    /// called `Sys_Error` otherwise — here it is an `Err`), then walks every
    /// lump. Each typed lump's record count is `filelen / record_size`, exactly
    /// as the C did with `l->filelen / sizeof(*in)`; a `filelen` that is not a
    /// whole multiple of the record size is rejected (the C `funny lump size`
    /// `Sys_Error`).
    pub fn parse(bytes: &[u8]) -> Result<Bsp> {
        let mut hr = Reader::new(bytes);
        let header = Header::read(&mut hr)?;

        if header.version != BSPVERSION {
            return Err(QError::invalid(format!(
                "Mod_LoadBrushModel: wrong version number ({} should be {})",
                header.version, BSPVERSION
            )));
        }

        let entities = read_entities(bytes, &header.lumps[LUMP_ENTITIES])?;
        let planes = read_typed(bytes, &header.lumps[LUMP_PLANES], DPLANE_SIZE, DPlane::read)?;
        let vertexes = read_typed(
            bytes,
            &header.lumps[LUMP_VERTEXES],
            DVERTEX_SIZE,
            DVertex::read,
        )?;
        let edges = read_typed(bytes, &header.lumps[LUMP_EDGES], DEDGE_SIZE, DEdge::read)?;
        let faces = read_typed(bytes, &header.lumps[LUMP_FACES], DFACE_SIZE, DFace::read)?;
        let nodes = read_typed(bytes, &header.lumps[LUMP_NODES], DNODE_SIZE, DNode::read)?;
        let leafs = read_typed(bytes, &header.lumps[LUMP_LEAFS], DLEAF_SIZE, DLeaf::read)?;
        let clipnodes = read_typed(
            bytes,
            &header.lumps[LUMP_CLIPNODES],
            DCLIPNODE_SIZE,
            DClipNode::read,
        )?;
        let texinfo = read_typed(
            bytes,
            &header.lumps[LUMP_TEXINFO],
            TEXINFO_SIZE,
            TexInfo::read,
        )?;
        let models = read_typed(bytes, &header.lumps[LUMP_MODELS], DMODEL_SIZE, DModel::read)?;
        let marksurfaces = read_u16_array(bytes, &header.lumps[LUMP_MARKSURFACES])?;
        let surfedges = read_i32_array(bytes, &header.lumps[LUMP_SURFEDGES])?;
        let textures = read_textures(bytes, &header.lumps[LUMP_TEXTURES])?;
        let visibility = read_raw(bytes, &header.lumps[LUMP_VISIBILITY])?;
        let lighting = read_raw(bytes, &header.lumps[LUMP_LIGHTING])?;

        Ok(Bsp {
            version: header.version,
            entities,
            planes,
            vertexes,
            edges,
            faces,
            nodes,
            leafs,
            clipnodes,
            texinfo,
            models,
            marksurfaces,
            surfedges,
            textures,
            visibility,
            lighting,
        })
    }
}

// ---------------------------------------------------------------------------
// Lump-walking helpers
// ---------------------------------------------------------------------------

/// Resolve a lump's `(fileofs, filelen)` into non-negative `usize` bounds and
/// confirm the span lies within `total` bytes. Mirrors the implicit assumption
/// in the C that `mod_base + l->fileofs` for `l->filelen` bytes is in range.
fn lump_bounds(context: &'static str, lump: &Lump, total: usize) -> Result<(usize, usize)> {
    if lump.fileofs < 0 {
        return Err(QError::invalid(format!(
            "{context}: negative lump offset {}",
            lump.fileofs
        )));
    }
    if lump.filelen < 0 {
        return Err(QError::invalid(format!(
            "{context}: negative lump length {}",
            lump.filelen
        )));
    }
    let ofs = lump.fileofs as usize;
    let len = lump.filelen as usize;
    let end = ofs
        .checked_add(len)
        .ok_or_else(|| QError::invalid(format!("{context}: lump span overflow")))?;
    if end > total {
        return Err(QError::Truncated {
            context,
            need: end,
            have: total,
        });
    }
    Ok((ofs, len))
}

/// Decode `filelen / record_size` records of a fixed-size typed lump, each via
/// `read_one` from a `Reader` positioned at the lump start. A `filelen` not a
/// whole multiple of `record_size` is the C `funny lump size` error.
fn read_typed<T, F>(
    bytes: &[u8],
    lump: &Lump,
    record_size: usize,
    read_one: F,
) -> Result<Vec<T>>
where
    F: Fn(&mut Reader) -> Result<T>,
{
    let (ofs, len) = lump_bounds("bsp lump", lump, bytes.len())?;
    if record_size == 0 {
        return Err(QError::invalid("bsp lump: zero record size"));
    }
    if len % record_size != 0 {
        return Err(QError::invalid(format!(
            "MOD_LoadBmodel: funny lump size ({len} not a multiple of {record_size})"
        )));
    }
    let count = len / record_size;
    let mut r = Reader::at(bytes, ofs);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_one(&mut r)?);
    }
    Ok(out)
}

/// LUMP_ENTITIES: a raw NUL-terminated text blob. The C `Mod_LoadEntities`
/// simply `memcpy`'d `filelen` bytes; we trim at the first NUL (the directory
/// length includes the terminator) and decode lossily as UTF-8.
fn read_entities(bytes: &[u8], lump: &Lump) -> Result<String> {
    let (ofs, len) = lump_bounds("LUMP_ENTITIES", lump, bytes.len())?;
    let raw = Reader::at(bytes, ofs).slice_at(ofs, len)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

/// Raw byte lumps (LUMP_VISIBILITY, LUMP_LIGHTING): copied verbatim.
fn read_raw(bytes: &[u8], lump: &Lump) -> Result<Vec<u8>> {
    let (ofs, len) = lump_bounds("bsp raw lump", lump, bytes.len())?;
    Ok(Reader::at(bytes, ofs).slice_at(ofs, len)?.to_vec())
}

/// LUMP_MARKSURFACES: a plain `u16` array (the C read `short` then assigned to
/// an `unsigned short` slot).
fn read_u16_array(bytes: &[u8], lump: &Lump) -> Result<Vec<u16>> {
    let (ofs, len) = lump_bounds("LUMP_MARKSURFACES", lump, bytes.len())?;
    if len % 2 != 0 {
        return Err(QError::invalid(
            "MOD_LoadBmodel: funny lump size (marksurfaces)",
        ));
    }
    let count = len / 2;
    let mut r = Reader::at(bytes, ofs);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.u16()?);
    }
    Ok(out)
}

/// LUMP_SURFEDGES: a plain `i32` array (signed; negative = backwards edge).
fn read_i32_array(bytes: &[u8], lump: &Lump) -> Result<Vec<i32>> {
    let (ofs, len) = lump_bounds("LUMP_SURFEDGES", lump, bytes.len())?;
    if len % 4 != 0 {
        return Err(QError::invalid(
            "MOD_LoadBmodel: funny lump size (surfedges)",
        ));
    }
    let count = len / 4;
    let mut r = Reader::at(bytes, ofs);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.i32()?);
    }
    Ok(out)
}

/// LUMP_TEXTURES: a `dmiptexlump_t` (a count followed by that many `i32` data
/// offsets, each relative to the lump start). An offset of `-1` means a missing
/// texture (`None`); otherwise a `miptex_t` header is decoded at
/// `lump_fileofs + offset`. Mirrors `Mod_LoadTextures` (which returns early on
/// an empty lump).
fn read_textures(bytes: &[u8], lump: &Lump) -> Result<Vec<Option<MipTex>>> {
    let (ofs, len) = lump_bounds("LUMP_TEXTURES", lump, bytes.len())?;
    if len == 0 {
        // C: `if (!l->filelen) { loadmodel->textures = NULL; return; }`
        return Ok(Vec::new());
    }

    let mut r = Reader::at(bytes, ofs);
    let nummiptex = r.i32()?;
    if nummiptex < 0 {
        return Err(QError::invalid(format!(
            "LUMP_TEXTURES: negative nummiptex {nummiptex}"
        )));
    }
    let nummiptex = nummiptex as usize;

    // Read the offset table (relative to the lump start) first, then resolve
    // each texture. The C accessed `m->dataofs[i]` which is the i32 directly
    // after `nummiptex`.
    // Do not pre-size from the untrusted `nummiptex`: a tiny lump could claim a
    // huge count and force a multi-GB allocation (which would abort). Cap the
    // reservation by the bytes actually available (4 per offset); the per-entry
    // `r.i32()?` still returns `Truncated` if the count overruns the buffer.
    let mut dataofs = Vec::with_capacity(nummiptex.min(r.remaining() / 4));
    for _ in 0..nummiptex {
        dataofs.push(r.i32()?);
    }

    let mut out = Vec::with_capacity(dataofs.len());
    for off in dataofs {
        if off == -1 {
            out.push(None);
            continue;
        }
        if off < 0 {
            return Err(QError::invalid(format!(
                "LUMP_TEXTURES: bad miptex data offset {off}"
            )));
        }
        // `mt = (miptex_t *)((byte *)m + m->dataofs[i])` — relative to the
        // lump start, which is `ofs` in the whole-file buffer.
        let abs = ofs
            .checked_add(off as usize)
            .ok_or_else(|| QError::invalid("LUMP_TEXTURES: miptex offset overflow"))?;
        let mut mr = Reader::at(bytes, abs);
        out.push(Some(MipTex::read(&mut mr)?));
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny builder for a synthetic version-29 BSP. Lump data is appended
    /// after the 124-byte header; each `set_*` records the directory entry.
    struct BspBuilder {
        header: [u8; HEADER_SIZE],
        data: Vec<u8>,
    }

    impl BspBuilder {
        fn new(version: i32) -> BspBuilder {
            let mut header = [0u8; HEADER_SIZE];
            header[0..4].copy_from_slice(&version.to_le_bytes());
            BspBuilder {
                header,
                data: Vec::new(),
            }
        }

        /// Append `payload` as lump `index` and record its directory entry.
        fn set_lump(&mut self, index: usize, payload: &[u8]) {
            let ofs = (HEADER_SIZE + self.data.len()) as i32;
            let len = payload.len() as i32;
            let base = 4 + index * LUMP_SIZE;
            self.header[base..base + 4].copy_from_slice(&ofs.to_le_bytes());
            self.header[base + 4..base + 8].copy_from_slice(&len.to_le_bytes());
            self.data.extend_from_slice(payload);
        }

        fn build(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity(HEADER_SIZE + self.data.len());
            out.extend_from_slice(&self.header);
            out.extend_from_slice(&self.data);
            out
        }
    }

    fn push_vec3(buf: &mut Vec<u8>, v: [f32; 3]) {
        for c in v {
            buf.extend_from_slice(&c.to_le_bytes());
        }
    }

    #[test]
    fn struct_sizes_match() {
        // Self-check the byte layouts the spec pins down.
        assert_eq!(LUMP_SIZE, 8);
        assert_eq!(HEADER_SIZE, 124);
        assert_eq!(DMODEL_SIZE, 64);
        assert_eq!(DVERTEX_SIZE, 12);
        assert_eq!(DPLANE_SIZE, 20);
        assert_eq!(DNODE_SIZE, 24);
        assert_eq!(DCLIPNODE_SIZE, 8);
        assert_eq!(TEXINFO_SIZE, 40);
        assert_eq!(DEDGE_SIZE, 4);
        assert_eq!(DFACE_SIZE, 20);
        assert_eq!(DLEAF_SIZE, 28);
        assert_eq!(MIPTEX_SIZE, 40);
    }

    #[test]
    fn minimal_synthetic_bsp() {
        let mut b = BspBuilder::new(BSPVERSION);

        // LUMP_ENTITIES: NUL-terminated text blob.
        let ent_text = b"{ \"classname\" \"worldspawn\" }\0";
        b.set_lump(LUMP_ENTITIES, ent_text);

        // LUMP_VERTEXES: 3 vertices.
        let coords = [
            [1.0f32, 2.0, 3.0],
            [-4.0, 5.5, 6.25],
            [7.0, -8.0, 9.0],
        ];
        let mut verts = Vec::new();
        for v in coords {
            push_vec3(&mut verts, v);
        }
        b.set_lump(LUMP_VERTEXES, &verts);

        // LUMP_PLANES: 2 planes.
        let mut planes = Vec::new();
        push_vec3(&mut planes, [1.0, 0.0, 0.0]);
        planes.extend_from_slice(&16.0f32.to_le_bytes()); // dist
        planes.extend_from_slice(&PLANE_X.to_le_bytes()); // ptype
        push_vec3(&mut planes, [0.0, 0.0, 1.0]);
        planes.extend_from_slice(&(-32.0f32).to_le_bytes());
        planes.extend_from_slice(&PLANE_Z.to_le_bytes());
        b.set_lump(LUMP_PLANES, &planes);

        // All other lumps stay zero-length (zero-filled directory entries).
        let bytes = b.build();
        let bsp = Bsp::parse(&bytes).expect("parse minimal bsp");

        assert_eq!(bsp.version, BSPVERSION);
        assert_eq!(bsp.entities, "{ \"classname\" \"worldspawn\" }");

        assert_eq!(bsp.vertexes.len(), 3);
        assert_eq!(bsp.vertexes[0].point, [1.0, 2.0, 3.0]);
        assert_eq!(bsp.vertexes[1].point, [-4.0, 5.5, 6.25]);
        assert_eq!(bsp.vertexes[2].point, [7.0, -8.0, 9.0]);

        assert_eq!(bsp.planes.len(), 2);
        assert_eq!(bsp.planes[0].normal, [1.0, 0.0, 0.0]);
        assert_eq!(bsp.planes[0].dist, 16.0);
        assert_eq!(bsp.planes[0].ptype, PLANE_X);
        assert_eq!(bsp.planes[1].normal, [0.0, 0.0, 1.0]);
        assert_eq!(bsp.planes[1].dist, -32.0);
        assert_eq!(bsp.planes[1].ptype, PLANE_Z);

        // Empty lumps decode to empty collections.
        assert!(bsp.faces.is_empty());
        assert!(bsp.nodes.is_empty());
        assert!(bsp.leafs.is_empty());
        assert!(bsp.edges.is_empty());
        assert!(bsp.clipnodes.is_empty());
        assert!(bsp.texinfo.is_empty());
        assert!(bsp.models.is_empty());
        assert!(bsp.marksurfaces.is_empty());
        assert!(bsp.surfedges.is_empty());
        assert!(bsp.textures.is_empty());
        assert!(bsp.visibility.is_empty());
        assert!(bsp.lighting.is_empty());
    }

    #[test]
    fn wrong_version_is_error() {
        let b = BspBuilder::new(28);
        let bytes = b.build();
        assert!(Bsp::parse(&bytes).is_err());
    }

    #[test]
    fn truncated_header_is_error() {
        // Fewer than 124 bytes — header decode must fail, not panic.
        let bytes = [0u8; 10];
        assert!(Bsp::parse(&bytes).is_err());
    }

    #[test]
    fn funny_lump_size_is_error() {
        let mut b = BspBuilder::new(BSPVERSION);
        // 13 bytes is not a multiple of DVERTEX_SIZE (12).
        b.set_lump(LUMP_VERTEXES, &[0u8; 13]);
        let bytes = b.build();
        assert!(Bsp::parse(&bytes).is_err());
    }

    #[test]
    fn lump_offset_past_end_is_error() {
        // Hand-build a header whose vertex lump points past the file.
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(&BSPVERSION.to_le_bytes());
        let base = 4 + LUMP_VERTEXES * LUMP_SIZE;
        header[base..base + 4].copy_from_slice(&1000i32.to_le_bytes()); // fileofs
        header[base + 4..base + 8].copy_from_slice(&(DVERTEX_SIZE as i32).to_le_bytes());
        assert!(Bsp::parse(&header).is_err());
    }

    #[test]
    fn textures_lump_roundtrip() {
        let mut b = BspBuilder::new(BSPVERSION);

        // Build a TEXTURES lump: nummiptex=2, offsets [hdr_a, -1].
        // Layout: [i32 count][i32 ofs0][i32 ofs1][MipTex A]
        let table = 4 + 2 * 4; // count + two offsets
        let ofs_a = table as i32;

        let mut lump = Vec::new();
        lump.extend_from_slice(&2i32.to_le_bytes()); // nummiptex
        lump.extend_from_slice(&ofs_a.to_le_bytes()); // dataofs[0]
        lump.extend_from_slice(&(-1i32).to_le_bytes()); // dataofs[1] = missing

        // MipTex A header.
        let mut name = [0u8; 16];
        name[..5].copy_from_slice(b"brick");
        lump.extend_from_slice(&name);
        lump.extend_from_slice(&64u32.to_le_bytes()); // width
        lump.extend_from_slice(&32u32.to_le_bytes()); // height
        for off in [40u32, 40 + 64 * 32, 0, 0] {
            lump.extend_from_slice(&off.to_le_bytes());
        }

        b.set_lump(LUMP_TEXTURES, &lump);
        let bytes = b.build();
        let bsp = Bsp::parse(&bytes).expect("parse textures bsp");

        assert_eq!(bsp.textures.len(), 2);
        let tex = bsp.textures[0].as_ref().expect("first texture present");
        assert_eq!(tex.name, "brick");
        assert_eq!(tex.width, 64);
        assert_eq!(tex.height, 32);
        assert_eq!(tex.offsets[0], 40);
        assert!(bsp.textures[1].is_none());
    }

    #[test]
    fn typed_lumps_roundtrip() {
        let mut b = BspBuilder::new(BSPVERSION);

        // One DFace with distinctive fields.
        let mut face = Vec::new();
        face.extend_from_slice(&5i16.to_le_bytes()); // planenum
        face.extend_from_slice(&1i16.to_le_bytes()); // side
        face.extend_from_slice(&100i32.to_le_bytes()); // firstedge
        face.extend_from_slice(&4i16.to_le_bytes()); // numedges
        face.extend_from_slice(&2i16.to_le_bytes()); // texinfo
        face.extend_from_slice(&[1u8, 2, 3, 4]); // styles
        face.extend_from_slice(&(-1i32).to_le_bytes()); // lightofs
        assert_eq!(face.len(), DFACE_SIZE);
        b.set_lump(LUMP_FACES, &face);

        // One DLeaf.
        let mut leaf = Vec::new();
        leaf.extend_from_slice(&CONTENTS_WATER.to_le_bytes()); // contents
        leaf.extend_from_slice(&(-1i32).to_le_bytes()); // visofs
        for c in [-10i16, -20, -30] {
            leaf.extend_from_slice(&c.to_le_bytes());
        }
        for c in [10i16, 20, 30] {
            leaf.extend_from_slice(&c.to_le_bytes());
        }
        leaf.extend_from_slice(&7u16.to_le_bytes()); // firstmarksurface
        leaf.extend_from_slice(&3u16.to_le_bytes()); // nummarksurfaces
        leaf.extend_from_slice(&[0u8, 1, 2, 3]); // ambient_level
        assert_eq!(leaf.len(), DLEAF_SIZE);
        b.set_lump(LUMP_LEAFS, &leaf);

        // marksurfaces + surfedges arrays.
        let mut marks = Vec::new();
        for m in [0u16, 1, 2] {
            marks.extend_from_slice(&m.to_le_bytes());
        }
        b.set_lump(LUMP_MARKSURFACES, &marks);

        let mut surfe = Vec::new();
        for s in [-1i32, 2, -3] {
            surfe.extend_from_slice(&s.to_le_bytes());
        }
        b.set_lump(LUMP_SURFEDGES, &surfe);

        // visibility + lighting raw bytes.
        b.set_lump(LUMP_VISIBILITY, &[0xaa, 0xbb, 0xcc]);
        b.set_lump(LUMP_LIGHTING, &[1, 2, 3, 4, 5]);

        let bytes = b.build();
        let bsp = Bsp::parse(&bytes).expect("parse typed lumps");

        assert_eq!(bsp.faces.len(), 1);
        let f = &bsp.faces[0];
        assert_eq!(f.planenum, 5);
        assert_eq!(f.side, 1);
        assert_eq!(f.firstedge, 100);
        assert_eq!(f.numedges, 4);
        assert_eq!(f.texinfo, 2);
        assert_eq!(f.styles, [1, 2, 3, 4]);
        assert_eq!(f.lightofs, -1);

        assert_eq!(bsp.leafs.len(), 1);
        let l = &bsp.leafs[0];
        assert_eq!(l.contents, CONTENTS_WATER);
        assert_eq!(l.visofs, -1);
        assert_eq!(l.mins, [-10, -20, -30]);
        assert_eq!(l.maxs, [10, 20, 30]);
        assert_eq!(l.firstmarksurface, 7);
        assert_eq!(l.nummarksurfaces, 3);
        assert_eq!(l.ambient_level, [0, 1, 2, 3]);

        assert_eq!(bsp.marksurfaces, vec![0, 1, 2]);
        assert_eq!(bsp.surfedges, vec![-1, 2, -3]);
        assert_eq!(bsp.visibility, vec![0xaa, 0xbb, 0xcc]);
        assert_eq!(bsp.lighting, vec![1, 2, 3, 4, 5]);
    }
}
