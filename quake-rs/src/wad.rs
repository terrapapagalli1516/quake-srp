//! WAD2 archive loader.
//!
//! Ported from Quake (GPLv2) — `WinQuake/wad.c` and `WinQuake/wad.h`
//! (Copyright (C) 1996-1997 Id Software, Inc.).
//!
//! A WAD2 file is a flat archive of named "lumps". Quake ships `gfx.wad`, which
//! holds the console character font (`conchars`) and small 2-D pictures
//! (`qpic_t`), mostly the status bar's. (The palette is `gfx/palette.lmp`, a pak
//! file.) The on-disk layout is:
//!
//! ```text
//! wadinfo_t  (12 bytes)   identification[4] "WAD2", numlumps, infotableofs
//! ...lump payloads...
//! lumpinfo_t[numlumps]    (32 bytes each) starting at infotableofs
//! ```
//!
//! The C engine cast the raw mmap'd file buffer directly onto `wadinfo_t` /
//! `lumpinfo_t` and byte-swapped a few fields in place. Here every field is
//! decoded explicitly through a [`Reader`], so malformed or truncated archives
//! return a [`QError`] instead of reading out of bounds.

use crate::error::{QError, Result};
use crate::read::Reader;

// Compression types (`compression` field of a lump).
/// No compression (the only kind Quake actually uses).
pub const CMP_NONE: u8 = 0;
/// LZSS compression (defined but unused by the shipping engine).
pub const CMP_LZSS: u8 = 1;

// Lump content types (`type` field of a lump).
/// No / unknown type.
pub const TYP_NONE: u8 = 0;
/// A label lump (directory marker, e.g. `*_START` / `*_END`).
pub const TYP_LABEL: u8 = 1;
/// Base of the "lumpy" range: `TYP_LUMPY + grab_command_number`.
pub const TYP_LUMPY: u8 = 64;
/// A 256-entry RGB palette (768 bytes). Same value as [`TYP_LUMPY`].
pub const TYP_PALETTE: u8 = 64;
/// A Quake texture.
pub const TYP_QTEX: u8 = 65;
/// A 2-D picture (`qpic_t`): two `i32` dimensions followed by 8-bit pixels.
pub const TYP_QPIC: u8 = 66;
/// A sound lump.
pub const TYP_SOUND: u8 = 67;
/// A mip texture.
pub const TYP_MIPTEX: u8 = 68;

/// On-disk size of `wadinfo_t`: `char identification[4]; int numlumps, infotableofs;`.
pub const WADINFO_SIZE: usize = 12;
/// On-disk size of `lumpinfo_t`:
/// `int filepos, disksize, size; char type, compression, pad1, pad2; char name[16];`.
pub const LUMPINFO_SIZE: usize = 32;
/// Width of the fixed `name[16]` field, and the length [`Wad2::cleanup_name`]
/// produces internally before trimming (matches `W_CleanupName`).
pub const NAME_LEN: usize = 16;

/// One directory entry (`lumpinfo_t`).
///
/// The C struct also stored `pad1`/`pad2` (always zero, alignment padding). We
/// decode and discard them; they carry no information.
#[derive(Debug, Clone)]
pub struct LumpInfo {
    /// Absolute byte offset of this lump's payload within the WAD file.
    pub filepos: i32,
    /// Size of the payload as stored on disk (compressed size, if compressed).
    pub disksize: i32,
    /// Uncompressed size of the payload.
    pub size: i32,
    /// Content type (one of the `TYP_*` constants).
    pub typ: u8,
    /// Compression type (one of the `CMP_*` constants).
    pub compression: u8,
    /// Lump name, already run through [`Wad2::cleanup_name`] (lowercased,
    /// trimmed at the first NUL of the 16-char field).
    pub name: String,
}

/// A 2-D picture (`qpic_t`): an 8-bit, palette-indexed bitmap.
///
/// On disk this is two little-endian `i32`s (`SwapPic` byte-swaps them) followed
/// by exactly `width * height` bytes of pixel data.
#[derive(Debug, Clone)]
pub struct Qpic {
    /// Picture width in pixels.
    pub width: i32,
    /// Picture height in pixels.
    pub height: i32,
    /// `width * height` palette indices, row-major.
    pub data: Vec<u8>,
}

impl Qpic {
    /// Parse a raw `qpic_t` / `.lmp` picture out of `bytes`.
    ///
    /// The stock menu/HUD art (`gfx/qplaque.lmp`, `gfx/mainmenu.lmp`,
    /// `gfx/menudot1.lmp`, …) ships as standalone `.lmp` files inside the PAK,
    /// each laid out exactly like a `TYP_QPIC` WAD lump: two little-endian `i32`
    /// dimensions (`SwapPic` byte-swaps them) followed by `width * height` bytes
    /// of 8-bit palette indices. This is the same decode [`Wad2::qpic`] does, but
    /// on a borrowed buffer rather than a WAD directory entry.
    ///
    /// Every field is bounds-checked: negative dimensions, a `width * height`
    /// overflow, or a buffer shorter than `8 + width * height` all return a
    /// [`QError`] rather than reading out of bounds or panicking. (Trailing bytes
    /// past the pixel payload are ignored, exactly as the C `Draw_CachePic` cast
    /// only the header + the pixels it needed.)
    pub fn parse(bytes: &[u8]) -> Result<Qpic> {
        let mut r = Reader::new(bytes);
        let width = r.i32()?;
        let height = r.i32()?;
        if width < 0 || height < 0 {
            return Err(QError::invalid(format!("qpic: negative dimensions {width}x{height}")));
        }
        let pixels =
            (width as usize).checked_mul(height as usize).ok_or_else(|| QError::invalid("qpic: size overflow"))?;
        let data = r.take(pixels)?.to_vec();
        Ok(Qpic { width, height, data })
    }
}

/// A parsed WAD2 archive that owns its backing bytes.
///
/// Mirrors the C globals `wad_base` / `wad_numlumps` / `wad_lumps`, but scoped
/// to an instance rather than process-global state.
#[derive(Debug, Clone)]
pub struct Wad2 {
    /// The whole file, kept alive so [`lump_data`](Wad2::lump_data) can borrow it.
    base: Vec<u8>,
    /// The parsed directory.
    lumps: Vec<LumpInfo>,
}

impl Wad2 {
    /// Parse a WAD2 archive from an owned byte buffer.
    ///
    /// Verifies the `b"WAD2"` identification, reads `numlumps` /
    /// `infotableofs`, then decodes the directory table, running every name
    /// through [`cleanup_name`](Wad2::cleanup_name) (the C `W_CleanupName`).
    ///
    /// Faithfulness note: `W_LoadWadFile` byte-swapped `filepos`/`size` (and,
    /// for `TYP_QPIC` lumps, the picture's `width`/`height`) in place. Since the
    /// `Reader` always reads little-endian, the swap is implicit; the `qpic`
    /// dimensions are swapped lazily by [`qpic`](Wad2::qpic) on access. Unlike
    /// the C — which called `Sys_Error` on a bad id — we return a [`QError`].
    pub fn parse(bytes: Vec<u8>) -> Result<Wad2> {
        let lumps = {
            let mut r = Reader::new(&bytes);

            // wadinfo_t: identification[4], numlumps i32, infotableofs i32.
            let ident = r.bytes::<4>()?;
            if &ident != b"WAD2" {
                return Err(QError::BadMagic { context: "wad", found: ident, expected: "WAD2" });
            }
            let numlumps = r.i32()?;
            let infotableofs = r.i32()?;

            if numlumps < 0 {
                return Err(QError::invalid(format!("wad: negative numlumps {numlumps}")));
            }
            if infotableofs < 0 {
                return Err(QError::invalid(format!("wad: negative infotableofs {infotableofs}")));
            }
            let numlumps = numlumps as usize;
            let infotableofs = infotableofs as usize;

            // The whole directory must lie within the buffer.
            let table_bytes =
                numlumps.checked_mul(LUMPINFO_SIZE).ok_or_else(|| QError::invalid("wad: lump table size overflow"))?;
            let table_end =
                infotableofs.checked_add(table_bytes).ok_or_else(|| QError::invalid("wad: lump table end overflow"))?;
            if table_end > bytes.len() {
                return Err(QError::Truncated { context: "wad lump table", need: table_end, have: bytes.len() });
            }

            let mut lumps = Vec::with_capacity(numlumps);
            r.seek(infotableofs)?;
            for _ in 0..numlumps {
                // lumpinfo_t layout, in order:
                //   filepos i32, disksize i32, size i32,
                //   type i8, compression i8, pad1 i8, pad2 i8,
                //   name[16].
                let filepos = r.i32()?;
                let disksize = r.i32()?;
                let size = r.i32()?;
                let typ = r.u8()?;
                let compression = r.u8()?;
                let _pad1 = r.u8()?;
                let _pad2 = r.u8()?;
                let raw_name = r.bytes::<NAME_LEN>()?;
                let name = cleanup_name_bytes(&raw_name);

                lumps.push(LumpInfo { filepos, disksize, size, typ, compression, name });
            }
            lumps
        };

        Ok(Wad2 { base: bytes, lumps })
    }

    /// Clean a lump name the way the C `W_CleanupName` does: read up to 16
    /// characters, stop at the first NUL, lowercase ASCII `A..=Z`, and (in C)
    /// pad the rest of the 16-char field with NULs. We return the trimmed,
    /// lowercased string; the NUL padding is implicit. Bytes beyond index 15
    /// are ignored, exactly like the C loop bound.
    pub fn cleanup_name(name: &str) -> String {
        cleanup_name_bytes(name.as_bytes())
    }

    /// The parsed directory of lumps.
    pub fn lumps(&self) -> &[LumpInfo] {
        &self.lumps
    }

    /// Look up a lump by name (case-insensitive, matching `W_GetLumpinfo`).
    ///
    /// The name is run through [`cleanup_name`](Wad2::cleanup_name) and compared
    /// against each (already cleaned) stored name. The C version called
    /// `Sys_Error` when nothing matched; we return `None` so callers can
    /// recover, and surface "not found" as an error only where the caller needs
    /// the payload (see [`qpic`](Wad2::qpic)).
    pub fn lump(&self, name: &str) -> Option<&LumpInfo> {
        let clean = Self::cleanup_name(name);
        self.lumps.iter().find(|l| l.name == clean)
    }

    /// Look up a lump by index (`W_GetLumpNum`).
    ///
    /// Faithfulness note: the C bounds check was `num < 0 || num > numlumps`
    /// (an off-by-one that would read one entry past the end); we use a correct
    /// `n < len` so indexing stays in bounds, which is the only safe behavior.
    pub fn lump_num(&self, n: usize) -> Option<&LumpInfo> {
        self.lumps.get(n)
    }

    /// Borrow a lump's raw payload bytes: `base[filepos .. filepos + disksize]`.
    ///
    /// Returns [`QError`] if `filepos`/`disksize` are negative or the range runs
    /// past the end of the file.
    pub fn lump_data(&self, l: &LumpInfo) -> Result<&[u8]> {
        if l.filepos < 0 {
            return Err(QError::invalid(format!("wad: lump '{}' has negative filepos {}", l.name, l.filepos)));
        }
        if l.disksize < 0 {
            return Err(QError::invalid(format!("wad: lump '{}' has negative disksize {}", l.name, l.disksize)));
        }
        Reader::new(&self.base).slice_at(l.filepos as usize, l.disksize as usize)
    }

    /// Parse a `TYP_QPIC` lump by name into a [`Qpic`].
    ///
    /// Mirrors `W_GetLumpName` + `SwapPic`: locate the lump, read the two
    /// little-endian `i32` dimensions at `filepos` (the implicit `SwapPic`),
    /// then the following `width * height` bytes of pixel data. The C engine
    /// would `Sys_Error` if the lump were missing; we return a [`QError`].
    pub fn qpic(&self, name: &str) -> Result<Qpic> {
        let lump = self.lump(name).ok_or_else(|| QError::invalid(format!("wad: lump '{name}' not found")))?;

        if lump.typ != TYP_QPIC {
            return Err(QError::invalid(format!(
                "wad: lump '{}' is type {}, not TYP_QPIC ({})",
                lump.name, lump.typ, TYP_QPIC
            )));
        }
        if lump.filepos < 0 {
            return Err(QError::invalid(format!("wad: qpic '{}' has negative filepos {}", lump.name, lump.filepos)));
        }

        // qpic_t: width i32, height i32, data[width*height].
        let mut r = Reader::at(&self.base, lump.filepos as usize);
        let width = r.i32()?;
        let height = r.i32()?;
        if width < 0 || height < 0 {
            return Err(QError::invalid(format!("wad: qpic '{}' has negative dimensions {width}x{height}", lump.name)));
        }
        let pixels =
            (width as usize).checked_mul(height as usize).ok_or_else(|| QError::invalid("wad: qpic size overflow"))?;
        let data = r.take(pixels)?.to_vec();

        Ok(Qpic { width, height, data })
    }
}

/// Shared implementation of `W_CleanupName` operating on raw bytes.
///
/// Reads at most [`NAME_LEN`] bytes, stops at the first NUL, lowercases ASCII
/// `A..=Z`, and leaves everything else as-is. Non-ASCII bytes pass through and
/// are decoded losslessly as Latin-1-into-UTF-8 (matching the C, which treated
/// names as raw `char`s). The C function NUL-pads to 16 bytes; here that padding
/// is represented by the string simply ending.
fn cleanup_name_bytes(input: &[u8]) -> String {
    let mut out = String::with_capacity(NAME_LEN);
    for &c in input.iter().take(NAME_LEN) {
        if c == 0 {
            break;
        }
        let lowered = if c.is_ascii_uppercase() { c + (b'a' - b'A') } else { c };
        out.push(lowered as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Append a 32-byte `lumpinfo_t` entry to `dir`.
    fn push_lump(dir: &mut Vec<u8>, filepos: i32, disksize: i32, size: i32, typ: u8, compression: u8, name: &str) {
        dir.extend_from_slice(&filepos.to_le_bytes());
        dir.extend_from_slice(&disksize.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes());
        dir.push(typ);
        dir.push(compression);
        dir.push(0); // pad1
        dir.push(0); // pad2
        let mut field = [0u8; NAME_LEN];
        let nb = name.as_bytes();
        let n = nb.len().min(NAME_LEN);
        field[..n].copy_from_slice(&nb[..n]);
        dir.extend_from_slice(&field);
    }

    /// Build a synthetic WAD2 with a 768-byte palette lump and a 2x2 qpic lump.
    ///
    /// Returns `(bytes, palette_bytes, qpic_pixels)` for assertion.
    fn build_wad() -> (Vec<u8>, Vec<u8>, [u8; 4]) {
        // Payloads, laid out right after the 12-byte header.
        let palette: Vec<u8> = (0..768u32).map(|i| (i % 256) as u8).collect();
        let qpic_pixels: [u8; 4] = [10, 20, 30, 40];

        // qpic payload: width i32, height i32, then 2*2 pixels.
        let mut qpic_payload = Vec::new();
        qpic_payload.extend_from_slice(&2i32.to_le_bytes()); // width
        qpic_payload.extend_from_slice(&2i32.to_le_bytes()); // height
        qpic_payload.extend_from_slice(&qpic_pixels);

        let pal_pos = WADINFO_SIZE;
        let qpic_pos = pal_pos + palette.len();
        let infotableofs = qpic_pos + qpic_payload.len();

        let mut bytes = Vec::new();
        // wadinfo_t header.
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&2i32.to_le_bytes()); // numlumps
        bytes.extend_from_slice(&(infotableofs as i32).to_le_bytes());
        // Payloads.
        bytes.extend_from_slice(&palette);
        bytes.extend_from_slice(&qpic_payload);
        // Directory. Mixed-case names exercise W_CleanupName lowercasing.
        let mut dir = Vec::new();
        push_lump(
            &mut dir,
            pal_pos as i32,
            palette.len() as i32,
            palette.len() as i32,
            TYP_PALETTE,
            CMP_NONE,
            "PALETTE",
        );
        push_lump(
            &mut dir,
            qpic_pos as i32,
            qpic_payload.len() as i32,
            qpic_payload.len() as i32,
            TYP_QPIC,
            CMP_NONE,
            "Conchars",
        );
        bytes.extend_from_slice(&dir);

        debug_assert_eq!(bytes.len(), infotableofs + 2 * LUMPINFO_SIZE);
        (bytes, palette, qpic_pixels)
    }

    #[test]
    fn parses_header_and_directory() {
        let (bytes, palette, _) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();

        assert_eq!(wad.lumps().len(), 2);
        assert_eq!(wad.lumps()[0].name, "palette");
        assert_eq!(wad.lumps()[0].typ, TYP_PALETTE);
        assert_eq!(wad.lumps()[0].size, palette.len() as i32);
        assert_eq!(wad.lumps()[1].name, "conchars");
        assert_eq!(wad.lumps()[1].typ, TYP_QPIC);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let (bytes, _, _) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();

        // All of these clean to "palette".
        for q in ["palette", "PALETTE", "PaLeTtE"] {
            let l = wad.lump(q).expect("palette lump found");
            assert_eq!(l.name, "palette");
            assert_eq!(l.typ, TYP_PALETTE);
        }

        assert!(wad.lump("conchars").is_some());
        assert!(wad.lump("CONCHARS").is_some());
        assert!(wad.lump("does-not-exist").is_none());
    }

    #[test]
    fn lump_num_and_bounds() {
        let (bytes, _, _) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();

        assert_eq!(wad.lump_num(0).unwrap().name, "palette");
        assert_eq!(wad.lump_num(1).unwrap().name, "conchars");
        // The C off-by-one (`num > numlumps`) would have allowed index 2; we
        // correctly reject it.
        assert!(wad.lump_num(2).is_none());
    }

    #[test]
    fn lump_data_borrows_correct_range() {
        let (bytes, palette, _) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();

        let pal = wad.lump("palette").unwrap();
        let data = wad.lump_data(pal).unwrap();
        assert_eq!(data.len(), 768);
        assert_eq!(data.len(), palette.len());
        assert_eq!(data, &palette[..]);
    }

    #[test]
    fn qpic_dims_and_data() {
        let (bytes, _, qpic_pixels) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();

        let pic = wad.qpic("conchars").unwrap();
        assert_eq!(pic.width, 2);
        assert_eq!(pic.height, 2);
        assert_eq!(pic.data.len(), 4);
        assert_eq!(pic.data, qpic_pixels.to_vec());
    }

    #[test]
    fn qpic_on_non_qpic_lump_errors() {
        let (bytes, _, _) = build_wad();
        let wad = Wad2::parse(bytes).unwrap();
        assert!(wad.qpic("palette").is_err());
        assert!(wad.qpic("missing").is_err());
    }

    #[test]
    fn cleanup_name_matches_c_semantics() {
        // Lowercasing of A..=Z only.
        assert_eq!(Wad2::cleanup_name("ABZ_9"), "abz_9");
        // Stops at the first NUL, ignores anything after it.
        assert_eq!(Wad2::cleanup_name("ab\0cd"), "ab");
        // Truncated to the 16-char field.
        assert_eq!(Wad2::cleanup_name("ABCDEFGHIJKLMNOPQRST"), "abcdefghijklmnop");
        assert_eq!(Wad2::cleanup_name("ABCDEFGHIJKLMNOPQRST").len(), NAME_LEN);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = vec![b'W', b'A', b'D', b'1'];
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&12i32.to_le_bytes());
        match Wad2::parse(bytes) {
            Err(QError::BadMagic { expected, .. }) => assert_eq!(expected, "WAD2"),
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn truncated_header_is_rejected() {
        assert!(Wad2::parse(vec![b'W', b'A']).is_err());
        // Valid magic but no numlumps/infotableofs.
        assert!(Wad2::parse(b"WAD2".to_vec()).is_err());
    }

    #[test]
    fn out_of_range_table_is_rejected() {
        // Header claims 5 lumps at offset 12, but the file is only the header.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&5i32.to_le_bytes());
        bytes.extend_from_slice(&(WADINFO_SIZE as i32).to_le_bytes());
        assert!(Wad2::parse(bytes).is_err());
    }

    #[test]
    fn qpic_parse_reads_raw_lmp() {
        // A 3x2 raw QPIC: width i32, height i32, then 6 palette indices.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3i32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        let pixels = [1u8, 2, 3, 4, 5, 6];
        bytes.extend_from_slice(&pixels);
        // A few trailing bytes are ignored (the cast only needed the payload).
        bytes.extend_from_slice(&[0xAA, 0xBB]);

        let pic = Qpic::parse(&bytes).expect("parse raw lmp");
        assert_eq!(pic.width, 3);
        assert_eq!(pic.height, 2);
        assert_eq!(pic.data, pixels.to_vec());
    }

    #[test]
    fn qpic_parse_rejects_short_and_bad_buffers_without_panic() {
        // Too short for even the 8-byte header.
        assert!(Qpic::parse(&[]).is_err());
        assert!(Qpic::parse(&[0, 0, 0]).is_err());

        // Header says 4x4 = 16 pixels but only 2 bytes of data follow.
        let mut short = Vec::new();
        short.extend_from_slice(&4i32.to_le_bytes());
        short.extend_from_slice(&4i32.to_le_bytes());
        short.extend_from_slice(&[7u8, 8]);
        assert!(Qpic::parse(&short).is_err());

        // Negative dimensions are rejected.
        let mut neg = Vec::new();
        neg.extend_from_slice(&(-1i32).to_le_bytes());
        neg.extend_from_slice(&2i32.to_le_bytes());
        assert!(Qpic::parse(&neg).is_err());
    }

    #[test]
    fn negative_counts_are_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&(-1i32).to_le_bytes()); // numlumps
        bytes.extend_from_slice(&(WADINFO_SIZE as i32).to_le_bytes());
        assert!(Wad2::parse(bytes).is_err());
    }
}
