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
//!
//! **The search path.** id's engine reads every game file through
//! `com_searchpaths`, a list of `searchpath_t` elements — a pack, or a
//! directory of loose files — searched first to last (`COM_FindFile`). A
//! [`Pak`] is one such element with the rest of the list behind it
//! ([`Pak::over`]): [`Pak::read_file`] looks in this element, then down the
//! path. A lone archive is a path of one, so the engine, which only ever
//! reads files by name, takes the whole search path wherever it took the
//! shareware pak. [`crate::common`] builds id's path (`COM_AddGameDirectory`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// Not an archive: the game directory's loose files, a search path's
    /// directory element (`searchpath_t` with no `pack`). `subdirs` is
    /// `static_registered`: `COM_FindFile` never reads a shareware game's
    /// loose file whose name has a directory in it ("if not a registered
    /// version, don't ever go beyond base").
    Dir { path: PathBuf, subdirs: bool },
}

/// An opened PAK archive (the in-memory `pack_t`), or a directory of loose
/// files — one element of the search path — with the rest of the path
/// behind it.
#[derive(Debug, Clone)]
pub struct Pak {
    source: Source,
    entries: Vec<PakEntry>,
    /// The archive's path (or label), or the directory's path.
    name: String,
    /// CRC-16/CCITT of the on-disk directory region, as `COM_LoadPackFile`
    /// computes to detect modified archives.
    dir_crc: u16,
    /// The rest of the search path (`searchpath_t.next`), which
    /// [`Pak::read_file`] looks in when this element has no such file.
    /// Shared: a clone copies only this element's directory.
    next: Option<Arc<Pak>>,
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
            next: None,
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
            next: None,
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
            next: None,
        })
    }

    /// The archive's name (file path for [`open`](Self::open), or the caller's
    /// label for [`from_bytes`](Self::from_bytes)); a directory's path.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parsed directory entries of this archive (none for a directory;
    /// not the rest of the path's).
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

    /// Find an entry by exact name (`strcmp` semantics, as `COM_FindFile`)
    /// in this archive's own directory.
    pub fn find(&self, name: &str) -> Option<&PakEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// A search path's directory element: the loose files under `dir`
    /// (`COM_AddGameDirectory` puts the game directory itself on the path,
    /// behind its paks). `registered` is `static_registered`: a shareware
    /// game reads no loose file below the directory itself.
    pub fn directory(dir: &Path, registered: bool) -> Pak {
        Pak {
            source: Source::Dir { path: dir.to_path_buf(), subdirs: registered },
            entries: Vec::new(),
            name: dir.to_string_lossy().into_owned(),
            dir_crc: 0,
            next: None,
        }
    }

    /// Whether this element is a directory of loose files, not an archive.
    pub fn is_directory(&self) -> bool {
        matches!(self.source, Source::Dir { .. })
    }

    /// This element in front of the search path `rest` — `COM_AddGameDirectory`
    /// linking each pack it loads to the head of `com_searchpaths`, so the
    /// last added is searched first. Replaces whatever path this element had
    /// behind it.
    #[must_use]
    pub fn over(mut self, rest: Pak) -> Pak {
        self.next = Some(Arc::new(rest));
        self
    }

    /// The search path from this element on, in search order (`path`,
    /// `COM_Path_f`).
    pub fn path(&self) -> impl Iterator<Item = &Pak> {
        std::iter::successors(Some(self), |p| p.next.as_deref())
    }

    /// Read the contents of one directory entry of this archive.
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
            Source::Dir { .. } => {
                return Err(QError::invalid(format!("{} is a directory, not a pak", self.name)));
            }
        };
        // slice_at bounds-checks filepos + filelen against the buffer.
        let slice = Reader::new(image).slice_at(filepos, filelen)?;
        Ok(slice.to_vec())
    }

    /// Find a file by name on the search path and read its contents
    /// (`COM_FindFile` + `COM_LoadFile`): this element's, else the rest of
    /// the path's.
    ///
    /// Returns `Ok(None)` when no element has the name, as the C engine
    /// fell through every search path to "can't find".
    pub fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>> {
        for element in self.path() {
            if let Some(bytes) = element.read_own(name)? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    /// This element's copy of `name`, if it has one: an archive's entry, or
    /// a directory's loose file (`Sys_FileTime` finding it; a shareware game
    /// skips names below the directory).
    fn read_own(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match &self.source {
            Source::Dir { path, subdirs } => {
                if !subdirs && name.contains(['/', '\\']) {
                    return Ok(None);
                }
                // A file that cannot be read is one COM_FindFile did not find.
                Ok(std::fs::read(path.join(name)).ok())
            }
            _ => match self.find(name) {
                Some(e) => Ok(Some(self.read_entry(e)?)),
                None => Ok(None),
            },
        }
    }
}

/// A PACK image holding `files` (name, contents) in order: the 12-byte
/// header, the contents back to back, then the directory — the layout id's
/// `qfiles -pak` wrote. The tests' archives, and the synthetic registered
/// `pak1.pak` of the checks (no game data in the repo). Names longer than
/// the 55 characters a `name[56]` holds are cut.
pub fn write_pack(files: &[(&str, &[u8])]) -> Vec<u8> {
    let contents_len: usize = files.iter().map(|(_, d)| d.len()).sum();
    let dirofs = HEADER_SIZE + contents_len;
    let dirlen = files.len() * DIRENTRY_SIZE;
    let mut img = Vec::with_capacity(dirofs + dirlen);
    img.extend_from_slice(b"PACK");
    img.extend_from_slice(&(dirofs as i32).to_le_bytes());
    img.extend_from_slice(&(dirlen as i32).to_le_bytes());
    for (_, data) in files {
        img.extend_from_slice(data);
    }
    let mut filepos = HEADER_SIZE;
    for (name, data) in files {
        let mut field = [0u8; NAME_SIZE];
        let n = name.len().min(NAME_SIZE - 1);
        field[..n].copy_from_slice(&name.as_bytes()[..n]);
        img.extend_from_slice(&field);
        img.extend_from_slice(&(filepos as i32).to_le_bytes());
        img.extend_from_slice(&(data.len() as i32).to_le_bytes());
        filepos += data.len();
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic PACK image of `files` (see [`write_pack`]).
    fn build_pack(files: &[(&str, &[u8])]) -> Vec<u8> {
        write_pack(files)
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

    /// `COM_FindFile` down a path: `pak1` over `pak0` over the directory. The
    /// last pack added answers first, then the ones before it, then the loose
    /// files.
    #[test]
    fn a_path_reads_each_name_from_its_first_element() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-pak-path");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("maps")).unwrap();
        std::fs::write(dir.join("loose.cfg"), b"loose").unwrap();
        std::fs::write(dir.join("maps/custom.bsp"), b"custom").unwrap();
        std::fs::write(dir.join("progs.dat"), b"loose progs").unwrap();
        let pak0 = Pak::from_bytes("pak0.pak".into(), build_pack(&[("progs.dat", b"id progs"), ("maps/e1m1.bsp", b"e1m1")])).unwrap();
        let pak1 = Pak::from_bytes("pak1.pak".into(), build_pack(&[("maps/e1m1.bsp", b"patched"), ("maps/e2m1.bsp", b"e2m1")])).unwrap();
        let read = |p: &Pak, n: &str| p.read_file(n).unwrap().map(|b| String::from_utf8(b).unwrap());
        for registered in [false, true] {
            let path = pak1.clone().over(pak0.clone().over(Pak::directory(&dir, registered)));
            assert_eq!(read(&path, "maps/e1m1.bsp").as_deref(), Some("patched"), "pak1 before pak0");
            assert_eq!(read(&path, "maps/e2m1.bsp").as_deref(), Some("e2m1"));
            assert_eq!(read(&path, "progs.dat").as_deref(), Some("id progs"), "the packs before the loose files");
            assert_eq!(read(&path, "loose.cfg").as_deref(), Some("loose"), "a loose file at the top");
            assert_eq!(read(&path, "nowhere.dat"), None);
            // static_registered: a shareware game never goes below the directory.
            let custom = read(&path, "maps/custom.bsp");
            assert_eq!(custom.as_deref(), registered.then_some("custom"), "registered {registered}");
            let names: Vec<&str> = path.path().map(Pak::name).collect();
            assert_eq!(names, ["pak1.pak", "pak0.pak", dir.to_str().unwrap()]);
            assert!(path.path().last().unwrap().is_directory());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
