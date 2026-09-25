//! PAK archive reader.
//!
//! Ported from Quake (GPLv2) — `WinQuake/common.c`, `COM_LoadPackFile`,
//! plus the on-disk `dpackheader_t` / `dpackfile_t` structs and
//! `MAX_FILES_IN_PACK`.
//!
//! A `.pak` file is a flat archive: a 12-byte header pointing at a directory
//! of fixed 64-byte entries, each naming a contiguous run of bytes elsewhere
//! in the file.
//!
//! On disk (little-endian):
//!
//! ```text
//! dpackheader_t (12 bytes):
//!   char id[4];      // "PACK"
//!   int  dirofs;     // byte offset of the directory
//!   int  dirlen;     // directory length in bytes
//!
//! dpackfile_t (64 bytes):
//!   char name[56];   // NUL-padded, stored lowercase ("progs/player.mdl")
//!   int  filepos;    // byte offset of this file's contents
//!   int  filelen;    // length of this file's contents
//! ```
//!
//! `numfiles = dirlen / 64`. The C engine `Sys_Error`s if that exceeds
//! `MAX_FILES_IN_PACK` (2048); here we return [`QError::Invalid`].
//!
//! The C reader cast the raw header/directory bytes straight into structs;
//! this port decodes every field explicitly through a [`Reader`], so a
//! truncated or malformed archive yields an error instead of undefined
//! behaviour.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{QError, Result};
use crate::read::Reader;

/// Size of the on-disk `dpackheader_t`: `id[4] + dirofs + dirlen`.
pub const HEADER_SIZE: usize = 12;

/// Size of the on-disk `dpackfile_t`: `name[56] + filepos + filelen`.
pub const DIRENTRY_SIZE: usize = 64;

/// Length of the NUL-padded `name` field within a `dpackfile_t`.
pub const NAME_SIZE: usize = 56;

/// `MAX_FILES_IN_PACK` from `common.c`. Loading more is an error.
pub const MAX_FILES_IN_PACK: usize = 2048;

/// File count of the stock shareware `pak0.pak` (`PAK0_COUNT` in `common.c`).
pub const PAK0_COUNT: usize = 339;

/// CRC-16/CCITT of the stock shareware `pak0.pak` directory (`PAK0_CRC`).
pub const PAK0_CRC: u16 = 32981;

/// One entry from a PAK directory (the in-memory `packfile_t`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PakEntry {
    /// File name as stored, NUL-trimmed (e.g. `"progs/player.mdl"`).
    pub name: String,
    /// Byte offset of the file's contents within the archive.
    pub filepos: i32,
    /// Length of the file's contents in bytes.
    pub filelen: i32,
}

/// Where a [`Pak`]'s bytes live. Private by contract.
#[derive(Debug, Clone)]
enum Source {
    /// Read on demand from a file on disk.
    File(PathBuf),
    /// Whole archive held in memory (tests and small paks).
    Memory(Vec<u8>),
    /// Whole archive borrowed from a `'static` image (the browser build's
    /// `include_bytes!` pak): never copied, so cloning the [`Pak`] handle
    /// copies only its directory.
    Static(&'static [u8]),
}

/// An opened PAK archive (the in-memory `pack_t`).
#[derive(Debug, Clone)]
pub struct Pak {
    source: Source,
    entries: Vec<PakEntry>,
    name: String,
    /// CRC-16/CCITT of the on-disk directory region, as `COM_LoadPackFile`
    /// computes to detect modified archives.
    dir_crc: u16,
}

impl Pak {
    /// Decode the header + directory of an in-memory PAK image, returning the
    /// entries together with the CRC-16/CCITT of the directory region — the
    /// integrity value `COM_LoadPackFile` checks against `PAK0_CRC`.
    ///
    /// Verifies the `"PACK"` magic, reads `dirofs`/`dirlen`, then decodes
    /// `dirlen / 64` directory entries. Mirrors `COM_LoadPackFile`.
    fn decode_directory(file_bytes: &[u8]) -> Result<(Vec<PakEntry>, u16)> {
        // --- dpackheader_t ---
        let mut r = Reader::new(file_bytes);
        let id: [u8; 4] = r.bytes::<4>()?;
        if &id != b"PACK" {
            return Err(QError::BadMagic {
                context: "pak header",
                found: id,
                expected: "PACK",
            });
        }
        let dirofs = r.i32()?;
        let dirlen = r.i32()?;

        if dirofs < 0 {
            return Err(QError::invalid(format!("pak dirofs is negative: {dirofs}")));
        }
        if dirlen < 0 {
            return Err(QError::invalid(format!("pak dirlen is negative: {dirlen}")));
        }
        let dirofs = dirofs as usize;
        let dirlen = dirlen as usize;

        // numpackfiles = header.dirlen / sizeof(dpackfile_t)
        let numfiles = dirlen / DIRENTRY_SIZE;
        if numfiles > MAX_FILES_IN_PACK {
            return Err(QError::invalid(format!(
                "pak has {numfiles} files (max {MAX_FILES_IN_PACK})"
            )));
        }

        // The C engine seeks to dirofs and reads exactly dirlen bytes; the
        // directory we decode is `numfiles` whole entries starting there.
        // Bound the directory region explicitly so a bad offset/length is an
        // error rather than a slice past the end.
        let dir = r.slice_at(dirofs, numfiles * DIRENTRY_SIZE)?;

        let mut entries = Vec::with_capacity(numfiles);
        let mut dr = Reader::new(dir);
        for _ in 0..numfiles {
            // dpackfile_t: name[56] (NUL-padded), filepos, filelen
            let name = dr.name(NAME_SIZE)?;
            let filepos = dr.i32()?;
            let filelen = dr.i32()?;
            entries.push(PakEntry {
                name,
                filepos,
                filelen,
            });
        }

        // CRC the directory region exactly as COM_LoadPackFile does.
        let dir_crc = crate::crc::crc_block(dir);
        Ok((entries, dir_crc))
    }

    /// Parse just the directory entries of an in-memory PAK image (the public,
    /// CRC-free convenience over [`decode_directory`](Self::decode_directory)).
    pub fn parse_directory(file_bytes: &[u8]) -> Result<Vec<PakEntry>> {
        Ok(Self::decode_directory(file_bytes)?.0)
    }

    /// Build a [`Pak`] from an in-memory archive image (tests + small paks).
    ///
    /// The whole buffer is retained so [`read_entry`](Self::read_entry) can
    /// slice it directly.
    pub fn from_bytes(name: String, bytes: Vec<u8>) -> Result<Pak> {
        let (entries, dir_crc) = Self::decode_directory(&bytes)?;
        Ok(Pak {
            source: Source::Memory(bytes),
            entries,
            name,
            dir_crc,
        })
    }

    /// Build a [`Pak`] over a `'static` archive image without copying it (the
    /// browser build embeds the shareware pak with `include_bytes!`). Entry
    /// reads slice the image; a clone of the returned handle shares it.
    pub fn from_static(name: String, bytes: &'static [u8]) -> Result<Pak> {
        let (entries, dir_crc) = Self::decode_directory(bytes)?;
        Ok(Pak {
            source: Source::Static(bytes),
            entries,
            name,
            dir_crc,
        })
    }

    /// Open a PAK file on disk: read the header, seek to the directory, and
    /// decode it, retaining the path for on-demand entry reads.
    ///
    /// Equivalent to `COM_LoadPackFile`, but a missing file or bad magic is a
    /// returned error rather than a silent `NULL` / `Sys_Error`.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Pak> {
        let path = path.as_ref();
        let name = path.to_string_lossy().into_owned();

        let mut file = File::open(path)?;

        // --- read & verify dpackheader_t ---
        let mut header = [0u8; HEADER_SIZE];
        file.read_exact(&mut header)?;
        let mut hr = Reader::new(&header);
        let id: [u8; 4] = hr.bytes::<4>()?;
        if &id != b"PACK" {
            return Err(QError::BadMagic {
                context: "pak header",
                found: id,
                expected: "PACK",
            });
        }
        let dirofs = hr.i32()?;
        let dirlen = hr.i32()?;

        if dirofs < 0 {
            return Err(QError::invalid(format!("pak dirofs is negative: {dirofs}")));
        }
        if dirlen < 0 {
            return Err(QError::invalid(format!("pak dirlen is negative: {dirlen}")));
        }
        let dirofs_u = dirofs as u64;
        let dirlen_usize = dirlen as usize;

        let numfiles = dirlen_usize / DIRENTRY_SIZE;
        if numfiles > MAX_FILES_IN_PACK {
            return Err(QError::invalid(format!(
                "pak has {numfiles} files (max {MAX_FILES_IN_PACK})"
            )));
        }

        // --- seek to dirofs and read the directory ---
        file.seek(SeekFrom::Start(dirofs_u))?;
        let mut dir = vec![0u8; numfiles * DIRENTRY_SIZE];
        file.read_exact(&mut dir)?;

        let mut entries = Vec::with_capacity(numfiles);
        let mut dr = Reader::new(&dir);
        for _ in 0..numfiles {
            let name_field = dr.name(NAME_SIZE)?;
            let filepos = dr.i32()?;
            let filelen = dr.i32()?;
            entries.push(PakEntry {
                name: name_field,
                filepos,
                filelen,
            });
        }

        let dir_crc = crate::crc::crc_block(&dir);
        Ok(Pak {
            source: Source::File(path.to_path_buf()),
            entries,
            name,
            dir_crc,
        })
    }

    /// The archive's name (file path for [`open`](Self::open), or the caller's
    /// label for [`from_bytes`](Self::from_bytes)).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parsed directory entries.
    pub fn entries(&self) -> &[PakEntry] {
        &self.entries
    }

    /// CRC-16/CCITT of the on-disk directory region (the `COM_LoadPackFile`
    /// integrity value).
    pub fn dir_crc(&self) -> u16 {
        self.dir_crc
    }

    /// Whether this archive differs from the stock shareware `pak0.pak` — the C
    /// `com_modified` flag: a mismatched file count or directory CRC.
    pub fn is_modified(&self) -> bool {
        self.entries.len() != PAK0_COUNT || self.dir_crc != PAK0_CRC
    }

    /// Find an entry by exact name (`strcmp` semantics, as `COM_FindFile`).
    pub fn find(&self, name: &str) -> Option<&PakEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Read the contents of one directory entry.
    ///
    /// For a memory-backed (or static) archive this slices the retained
    /// buffer; for a file-backed archive it opens the file, seeks to
    /// `filepos`, and reads `filelen` bytes.
    pub fn read_entry(&self, e: &PakEntry) -> Result<Vec<u8>> {
        if e.filepos < 0 {
            return Err(QError::invalid(format!(
                "pak entry {:?} has negative filepos {}",
                e.name, e.filepos
            )));
        }
        if e.filelen < 0 {
            return Err(QError::invalid(format!(
                "pak entry {:?} has negative filelen {}",
                e.name, e.filelen
            )));
        }
        let filepos = e.filepos as usize;
        let filelen = e.filelen as usize;

        let image: &[u8] = match &self.source {
            Source::Memory(bytes) => bytes,
            Source::Static(bytes) => bytes,
            Source::File(path) => {
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(e.filepos as u64))?;
                let mut out = vec![0u8; filelen];
                file.read_exact(&mut out)?;
                return Ok(out);
            }
        };
        // slice_at bounds-checks filepos + filelen against the buffer.
        let slice = Reader::new(image).slice_at(filepos, filelen)?;
        Ok(slice.to_vec())
    }

    /// Find a file by name and read its contents.
    ///
    /// Returns `Ok(None)` when the name is not present, mirroring the C
    /// engine's "not found in this pack, try the next search path" behaviour.
    pub fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.find(name) {
            Some(e) => Ok(Some(self.read_entry(e)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a NUL-padded `name[56]` field.
    fn name56(s: &str) -> [u8; NAME_SIZE] {
        let mut buf = [0u8; NAME_SIZE];
        let b = s.as_bytes();
        assert!(b.len() <= NAME_SIZE, "test name too long");
        buf[..b.len()].copy_from_slice(b);
        buf
    }

    /// Construct a synthetic PACK image with the given (name, contents) files.
    ///
    /// Layout: header (12) then file contents back-to-back, then the
    /// directory. Returns the whole image.
    fn build_pack(files: &[(&str, &[u8])]) -> Vec<u8> {
        // Contents start right after the 12-byte header.
        let mut contents = Vec::new();
        let mut positions = Vec::new();
        let mut cursor = HEADER_SIZE as i32;
        for (_, data) in files {
            positions.push((cursor, data.len() as i32));
            contents.extend_from_slice(data);
            cursor += data.len() as i32;
        }

        // Directory follows the contents.
        let dirofs = HEADER_SIZE + contents.len();
        let dirlen = files.len() * DIRENTRY_SIZE;

        let mut dir = Vec::new();
        for (i, (name, _)) in files.iter().enumerate() {
            let (filepos, filelen) = positions[i];
            dir.extend_from_slice(&name56(name));
            dir.extend_from_slice(&filepos.to_le_bytes());
            dir.extend_from_slice(&filelen.to_le_bytes());
        }
        assert_eq!(dir.len(), dirlen);

        let mut img = Vec::new();
        img.extend_from_slice(b"PACK");
        img.extend_from_slice(&(dirofs as i32).to_le_bytes());
        img.extend_from_slice(&(dirlen as i32).to_le_bytes());
        img.extend_from_slice(&contents);
        img.extend_from_slice(&dir);
        img
    }

    fn sample_files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("progs/player.mdl", b"IDPO-player-model-bytes" as &[u8]),
            ("maps/start.bsp", b"BSP29-start-map" as &[u8]),
        ]
    }

    #[test]
    fn parses_two_file_directory() {
        let files = sample_files();
        let img = build_pack(&files);
        let entries = Pak::parse_directory(&img).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "progs/player.mdl");
        assert_eq!(entries[1].name, "maps/start.bsp");
        // filepos/filelen line up with the synthetic layout.
        assert_eq!(entries[0].filepos, HEADER_SIZE as i32);
        assert_eq!(entries[0].filelen, files[0].1.len() as i32);
        assert_eq!(
            entries[1].filepos,
            HEADER_SIZE as i32 + files[0].1.len() as i32
        );
        assert_eq!(entries[1].filelen, files[1].1.len() as i32);
    }

    #[test]
    fn from_bytes_entries_and_find() {
        let files = sample_files();
        let img = build_pack(&files);
        let pak = Pak::from_bytes("test.pak".to_string(), img).unwrap();

        assert_eq!(pak.name(), "test.pak");
        assert_eq!(pak.entries().len(), 2);

        let e = pak.find("progs/player.mdl").expect("entry present");
        assert_eq!(e.name, "progs/player.mdl");
        assert_eq!(e.filelen, files[0].1.len() as i32);

        // Exact (strcmp) match only — wrong case or partial name misses.
        assert!(pak.find("PROGS/PLAYER.MDL").is_none());
        assert!(pak.find("progs/player").is_none());
        assert!(pak.find("missing.txt").is_none());
    }

    #[test]
    fn read_entry_and_read_file_return_exact_bytes() {
        let files = sample_files();
        let img = build_pack(&files);
        let pak = Pak::from_bytes("test.pak".to_string(), img).unwrap();

        for (name, data) in &files {
            let e = pak.find(name).expect("entry present");
            let by_entry = pak.read_entry(e).unwrap();
            assert_eq!(&by_entry, data);

            let by_name = pak.read_file(name).unwrap().expect("found");
            assert_eq!(&by_name, data);
        }

        // Missing file: Ok(None), not an error.
        assert!(pak.read_file("nope.dat").unwrap().is_none());
    }

    #[test]
    fn from_static_reads_like_from_bytes_and_clones_share_the_image() {
        let files = sample_files();
        let img: &'static [u8] = Box::leak(build_pack(&files).into_boxed_slice());
        let owned = Pak::from_bytes("t.pak".into(), img.to_vec()).unwrap();
        let pak = Pak::from_static("t.pak".into(), img).unwrap();
        assert_eq!(pak.entries(), owned.entries());
        assert_eq!(pak.dir_crc(), owned.dir_crc());
        for (name, data) in &files {
            assert_eq!(&pak.read_file(name).unwrap().expect("found"), data);
        }
        assert!(pak.read_file("nope.dat").unwrap().is_none());
        // A clone is a handle onto the same image, not a copy of it.
        let clone = pak.clone();
        match (&pak.source, &clone.source) {
            (Source::Static(a), Source::Static(b)) => {
                assert!(std::ptr::eq(*a, *b) && std::ptr::eq(*a, img));
            }
            other => panic!("expected two static sources, got {other:?}"),
        }
        // The same bounds checks as the owned image.
        let past_end = PakEntry {
            name: "x".into(),
            filepos: img.len() as i32 - 2,
            filelen: 4,
        };
        assert!(pak.read_entry(&past_end).is_err());
        assert!(Pak::from_static("bad".into(), b"NOPE").is_err());
    }

    #[test]
    fn bad_magic_is_error() {
        let files = sample_files();
        let mut img = build_pack(&files);
        // Corrupt the magic.
        img[0] = b'N';
        img[1] = b'O';
        img[2] = b'P';
        img[3] = b'E';
        match Pak::parse_directory(&img) {
            Err(QError::BadMagic {
                context,
                expected,
                found,
            }) => {
                assert_eq!(context, "pak header");
                assert_eq!(expected, "PACK");
                assert_eq!(&found, b"NOPE");
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
        assert!(Pak::from_bytes("x".into(), img).is_err());
    }

    #[test]
    fn truncated_header_is_error() {
        // Only the magic, no dirofs/dirlen.
        let img = b"PACK".to_vec();
        assert!(Pak::parse_directory(&img).is_err());
    }

    #[test]
    fn empty_pack_has_no_entries() {
        let img = build_pack(&[]);
        let pak = Pak::from_bytes("empty.pak".into(), img).unwrap();
        assert_eq!(pak.entries().len(), 0);
        assert!(pak.find("anything").is_none());
        assert!(pak.read_file("anything").unwrap().is_none());
    }

    #[test]
    fn name_field_is_nul_trimmed() {
        // A name shorter than 56 bytes is padded with NULs and trimmed back.
        let img = build_pack(&[("a", b"hi")]);
        let entries = Pak::parse_directory(&img).unwrap();
        assert_eq!(entries[0].name, "a");
    }

    #[test]
    fn bad_dir_offset_is_error() {
        // Header claims a directory but the offset runs off the end.
        let mut img = Vec::new();
        img.extend_from_slice(b"PACK");
        img.extend_from_slice(&(9999i32).to_le_bytes()); // dirofs past EOF
        img.extend_from_slice(&(DIRENTRY_SIZE as i32).to_le_bytes()); // 1 entry
        assert!(Pak::parse_directory(&img).is_err());
    }

    #[test]
    fn too_many_files_is_error() {
        // dirlen implying > MAX_FILES_IN_PACK entries, without supplying them.
        let mut img = Vec::new();
        img.extend_from_slice(b"PACK");
        img.extend_from_slice(&(12i32).to_le_bytes());
        let dirlen = ((MAX_FILES_IN_PACK + 1) * DIRENTRY_SIZE) as i32;
        img.extend_from_slice(&dirlen.to_le_bytes());
        match Pak::parse_directory(&img) {
            Err(QError::Invalid(_)) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn read_entry_negative_filepos_is_error() {
        let bogus = PakEntry {
            name: "x".into(),
            filepos: -1,
            filelen: 4,
        };
        let pak = Pak::from_bytes("t.pak".into(), build_pack(&[("a", b"data")])).unwrap();
        assert!(pak.read_entry(&bogus).is_err());
    }
}
