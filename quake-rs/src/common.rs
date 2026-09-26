//! The file system — common.c's search path, and its check of the
//! registered game.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/common.c` (`COM_InitFilesystem`, `COM_AddGameDirectory`,
//! `COM_LoadPackFile`'s `com_modified`, `COM_CheckRegistered` and its `pop[]`
//! table, `COM_Path_f`) and `pr_edict.c`'s `PR_LoadProgs` checks.
//!
//! id's engine reads every game file through one search path: the game
//! directory `id1` (its loose files), and in front of it `pak0.pak`,
//! `pak1.pak`, … as far as they go, the last one searched first. Shareware
//! Quake is `pak0.pak`; the registered game adds `pak1.pak` (episodes 2–4,
//! their monsters and sounds) and with it `gfx/pop.lmp`, a 16x16 picture
//! `COM_CheckRegistered` compares against a table in the executable. That
//! comparison is the `registered` cvar: the QuakeC's
//! `trigger_onlyregistered` opens the start map's episode gates on it, and
//! the end of episode 1 goes on to the next episode instead of the order
//! screen. [`init_filesystem`] builds the path ([`Pak::over`]) and makes the
//! check; the engine then reads every file through the path's head, which
//! reads like one pak.
//!
//! What is not ported: `-game`, `-rogue`/`-hipnotic` and `-path` (mods and
//! mission packs, out of the port's scope), `-cachedir` (a CD-ROM cache),
//! and `proghack`.
//!
//! One check is the port's own, [`check_progs`]: a `progs.dat` that calls
//! builtins id's engine never had (a builtin numbered past `pr_builtin[]`, or
//! numbered 0 and found by name — the extension builtins later engines,
//! among them the 2021 re-release's, resolve) is refused at startup, where
//! id's engine would have run it until the first such call (`PR_RunError
//! ("Bad builtin call number")`) or, for a builtin #0, run statement 0 in
//! its place and gone on without it. Either way it is not a game the port
//! can play, and the player is better told so at once.

use std::path::{Path, PathBuf};

use crate::pak::Pak;
use crate::progs::{Progs, PROG_VERSION};

/// `GAMENAME` (quakedef.h): the game directory under the base directory.
pub const GAMENAME: &str = "id1";

/// `PROGHEADER_CRC` (progdefs.h): the CRC of the system globals and fields
/// the engine was built against. `PR_LoadProgs` refuses a `progs.dat` made
/// against any other.
pub const PROGHEADER_CRC: i32 = 5927;

/// The most `pakN.pak` files a directory's search goes through: id's loop
/// has no bound but the first missing one.
const MAX_PAKS: usize = 100;

/// `pop[]` (common.c): "this graphic needs to be in the pak file to use
/// registered features". `gfx/pop.lmp` is these 128 shorts, big-endian.
pub const POP: [u16; 128] = [
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x6600, 0x0000, 0x0000, 0x0000, 0x6600, 0x0000, //
    0x0000, 0x0066, 0x0000, 0x0000, 0x0000, 0x0000, 0x0067, 0x0000, //
    0x0000, 0x6665, 0x0000, 0x0000, 0x0000, 0x0000, 0x0065, 0x6600, //
    0x0063, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6563, //
    0x0064, 0x6561, 0x0000, 0x0000, 0x0000, 0x0000, 0x0061, 0x6564, //
    0x0064, 0x6564, 0x0000, 0x6469, 0x6969, 0x6400, 0x0064, 0x6564, //
    0x0063, 0x6568, 0x6200, 0x0064, 0x6864, 0x0000, 0x6268, 0x6563, //
    0x0000, 0x6567, 0x6963, 0x0064, 0x6764, 0x0063, 0x6967, 0x6500, //
    0x0000, 0x6266, 0x6769, 0x6a68, 0x6768, 0x6a69, 0x6766, 0x6200, //
    0x0000, 0x0062, 0x6566, 0x6666, 0x6666, 0x6666, 0x6562, 0x0000, //
    0x0000, 0x0000, 0x0062, 0x6364, 0x6664, 0x6362, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x0000, 0x0062, 0x6662, 0x0000, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x0000, 0x0061, 0x6661, 0x0000, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x0000, 0x0000, 0x6500, 0x0000, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x0000, 0x0000, 0x6400, 0x0000, 0x0000, 0x0000, //
];

/// `gfx/pop.lmp` as the registered `pak1.pak` holds it: [`POP`], big-endian
/// (the synthetic registered pak of the tests and the browser check).
pub fn pop_lmp() -> Vec<u8> {
    POP.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// What `COM_InitFilesystem` and `COM_CheckRegistered` found.
#[derive(Debug, Clone)]
pub struct Filesystem {
    /// The head of the search path (`com_searchpaths`): every game file is
    /// read through it.
    pub files: Pak,
    /// `com_gamedir`: where the saves and `config.cfg` go.
    pub gamedir: PathBuf,
    /// `static_registered`, and the `registered` cvar.
    pub registered: bool,
    /// `com_modified`: a pack on the path is not id's shareware `pak0.pak`
    /// (the registered `pak1.pak` is one).
    pub modified: bool,
    /// What id printed on the console on the way (`Con_Printf`): a line per
    /// pack, then the version.
    pub log: Vec<String>,
}

/// `COM_InitFilesystem` for `basedir` (`-basedir`, `.` by default):
/// `COM_AddGameDirectory ("<basedir>/id1")`, then `COM_CheckRegistered`.
/// An `Err` is one of id's `Sys_Error`s, in id's words: the game does not
/// start.
pub fn init_filesystem(basedir: &Path) -> Result<Filesystem, String> {
    let gamedir = basedir.join(GAMENAME);
    let mut log = Vec::new();
    // COM_FindFile starts with static_registered = 1: COM_CheckRegistered's
    // own search may find a loose gfx/pop.lmp.
    let (files, modified) = add_game_directory(&gamedir, true, &mut log)?;
    let registered = check_registered(&files, modified)?;
    log.push(format!("Playing {} version.", if registered { "registered" } else { "shareware" }));
    // The directory element takes the check's answer for every later search.
    let (files, _) = add_game_directory(&gamedir, registered, &mut Vec::new())?;
    Ok(Filesystem { files, gamedir, registered, modified, log })
}

/// `COM_AddGameDirectory`: the directory itself on the path, then
/// `pak0.pak`, `pak1.pak`, … in front of it until one does not open, each
/// logged as `COM_LoadPackFile` logs it. Also returns `com_modified`: any
/// pack whose file count or directory CRC is not the shareware `pak0.pak`'s.
fn add_game_directory(dir: &Path, registered: bool, log: &mut Vec<String>) -> Result<(Pak, bool), String> {
    let mut path = Pak::directory(dir, registered);
    let mut modified = false;
    for i in 0..MAX_PAKS {
        let file = dir.join(format!("pak{i}.pak"));
        if !file.is_file() {
            break; // COM_LoadPackFile: can't open, NULL: the search ends
        }
        let pack = Pak::open(&file).map_err(|e| load_pack_error(&file, &e))?;
        modified |= pack.is_modified();
        log.push(format!("Added packfile {} ({} files)", file.display(), pack.entries().len()));
        path = pack.over(path);
    }
    Ok((path, modified))
}

/// `COM_LoadPackFile`'s `Sys_Error`s: not a pack, too many files (or, the
/// port's, a directory that is not all there).
fn load_pack_error(file: &Path, e: &crate::QError) -> String {
    match e {
        crate::QError::BadMagic { .. } => format!("{} is not a packfile", file.display()),
        other => format!("{} is not a packfile: {other}", file.display()),
    }
}

/// `COM_CheckRegistered`'s verdict on a search path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    /// No `gfx/pop.lmp`: "Playing shareware version."
    Shareware,
    /// `gfx/pop.lmp` is id's: "Playing registered version."
    Registered,
    /// A `gfx/pop.lmp` that is not id's: "Corrupted data file."
    Corrupted,
}

/// Read `gfx/pop.lmp` off the path and compare its first 128 big-endian
/// shorts with [`POP`], as `COM_CheckRegistered` does (`Sys_FileRead` of
/// `sizeof(check)`: a short file leaves the rest of `check` as the stack
/// had it, so here it can only mismatch).
pub fn registration(files: &Pak) -> Registration {
    let Ok(Some(lmp)) = files.read_file("gfx/pop.lmp") else { return Registration::Shareware };
    let matches = lmp.len() >= 2 * POP.len()
        && lmp.chunks_exact(2).zip(POP).all(|(b, want)| u16::from_be_bytes([b[0], b[1]]) == want);
    if matches {
        Registration::Registered
    } else {
        Registration::Corrupted
    }
}

/// The `registered` cvar a search path gives the game: its
/// [`registration`] is [`Registration::Registered`]. (A corrupted
/// `pop.lmp` never gets this far: [`init_filesystem`] refuses it.)
pub fn is_registered(files: &Pak) -> bool {
    registration(files) == Registration::Registered
}

/// `COM_CheckRegistered`: registered or not, and its two `Sys_Error`s — a
/// `gfx/pop.lmp` that is not id's, and a modified game (`com_modified`)
/// without one: "You must have the registered version to use modified
/// games".
pub fn check_registered(files: &Pak, modified: bool) -> Result<bool, String> {
    match registration(files) {
        Registration::Registered => Ok(true),
        Registration::Corrupted => Err("Corrupted data file.".to_string()),
        Registration::Shareware if modified => {
            Err("You must have the registered version to use modified games".to_string())
        }
        Registration::Shareware => Ok(false),
    }
}

/// `COM_Path_f`: "Current search path:", then each element, a pack with its
/// file count.
pub fn path_lines(files: &Pak) -> Vec<String> {
    let mut out = vec!["Current search path:".to_string()];
    for element in files.path() {
        if element.is_directory() {
            out.push(element.name().to_string());
        } else {
            out.push(format!("{} ({} files)", element.name(), element.entries().len()));
        }
    }
    out
}

/// `PR_LoadProgs`'s refusals of the path's `progs.dat` — missing, the wrong
/// bytecode version, made against another `progdefs.h` — and the port's
/// own: builtins id's engine never had (the module's doc says why). An
/// `Err` is the message the game stops with.
pub fn check_progs(files: &Pak) -> Result<(), String> {
    let Ok(Some(bytes)) = files.read_file("progs.dat") else {
        return Err("PR_LoadProgs: couldn't load progs.dat".to_string());
    };
    let progs = Progs::parse(&bytes).map_err(|e| format!("progs.dat: {e}"))?;
    if progs.version != PROG_VERSION {
        return Err(format!("progs.dat has wrong version number ({} should be {PROG_VERSION})", progs.version));
    }
    if progs.crc != PROGHEADER_CRC {
        return Err("progs.dat system vars have been modified, progdefs.h is out of date".to_string());
    }
    let missing = foreign_builtins(&progs);
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "progs.dat calls {} builtin{} id's engine does not have ({}): it was made for another engine \
         (the 2021 re-release's is), not for id's 1996 Quake, which this port plays",
        missing.len(),
        if missing.len() == 1 { "" } else { "s" },
        missing.iter().take(4).cloned().collect::<Vec<_>>().join(", ") + if missing.len() > 4 { ", …" } else { "" },
    ))
}

/// The builtins `progs` declares that id's `pr_builtin[]` does not have: a
/// number past its end, or 0 (a builtin found by name) on any function but
/// the null function 0. Each as `name #n`.
fn foreign_builtins(progs: &Progs) -> Vec<String> {
    let known = crate::builtins::default_builtins().len();
    progs
        .functions
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(_, f)| {
            let number = match f.builtin() {
                Some(n) if n >= known => n,
                None if f.first_statement == 0 => 0,
                _ => return None,
            };
            Some(format!("{} #{number}", progs.string(f.s_name)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pak::write_pack;

    /// A game directory under `target/` (never /tmp) holding `paks`, as
    /// `pak0.pak`, `pak1.pak`, … .
    fn basedir(name: &str, paks: &[Vec<u8>]) -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-common").join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(GAMENAME)).unwrap();
        for (i, p) in paks.iter().enumerate() {
            std::fs::write(base.join(GAMENAME).join(format!("pak{i}.pak")), p).unwrap();
        }
        base
    }

    /// A progs.dat image with the given functions' `first_statement`s after
    /// the null function (one statement, a few strings), `crc` in its header.
    fn progs_image(firsts: &[i32], crc: i32) -> Vec<u8> {
        let strings = b"\0main\0ex_bot_movetopoint\0checkextension\0".to_vec();
        let names = [1i32, 6, 25];
        let statements = [0u8; 8]; // one OP_DONE
        let mut funcs = vec![[0i32; 9]]; // the null function
        for (i, &first) in firsts.iter().enumerate() {
            funcs.push([first, 0, 0, 0, names[i % 3], 0, 0, 0, 0]);
        }
        let globals = [0u8; 4 * 28];
        let header = 60;
        let ofs_st = header;
        let ofs_fn = ofs_st + statements.len();
        let ofs_str = ofs_fn + funcs.len() * 36;
        let ofs_gl = ofs_str + strings.len();
        let mut img = Vec::new();
        for v in [
            PROG_VERSION,
            crc,
            ofs_st as i32,
            1,
            0,
            0,
            0,
            0,
            ofs_fn as i32,
            funcs.len() as i32,
            ofs_str as i32,
            strings.len() as i32,
            ofs_gl as i32,
            28,
            0,
        ] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&statements);
        for f in &funcs {
            for v in &f[..7] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            img.extend_from_slice(&[0u8; 8]);
        }
        img.extend_from_slice(&strings);
        img.extend_from_slice(&globals);
        img
    }

    fn pak0() -> Vec<u8> {
        write_pack(&[("progs.dat", &progs_image(&[1, -1, -78], PROGHEADER_CRC)), ("maps/e1m1.bsp", b"e1m1")])
    }

    #[test]
    fn a_modified_shareware_game_is_refused_and_pop_lmp_registers_it() {
        // COM_LoadPackFile sets com_modified for any pack that is not id's pak0;
        // without gfx/pop.lmp that is Sys_Error.
        let base = basedir("modified", &[pak0()]);
        let fs = init_filesystem(&base);
        let err = fs.err().unwrap_or_default();
        assert_eq!(err, "You must have the registered version to use modified games");
        // pak1.pak with id's pop.lmp: the registered game, e2m1 on the path.
        let pak1 = write_pack(&[("gfx/pop.lmp", &pop_lmp()), ("maps/e2m1.bsp", b"e2m1")]);
        let base = basedir("registered", &[pak0(), pak1]);
        let fs = init_filesystem(&base).expect("registered");
        assert!(fs.registered && fs.modified);
        assert_eq!(fs.files.read_file("maps/e2m1.bsp").unwrap().as_deref(), Some(&b"e2m1"[..]));
        assert_eq!(fs.files.read_file("maps/e1m1.bsp").unwrap().as_deref(), Some(&b"e1m1"[..]));
        assert!(is_registered(&fs.files));
        let id1 = base.join(GAMENAME);
        assert_eq!(
            fs.log,
            [
                format!("Added packfile {} (2 files)", id1.join("pak0.pak").display()),
                format!("Added packfile {} (2 files)", id1.join("pak1.pak").display()),
                "Playing registered version.".to_string(),
            ]
        );
        assert_eq!(
            path_lines(&fs.files),
            [
                "Current search path:".to_string(),
                format!("{} (2 files)", id1.join("pak1.pak").display()),
                format!("{} (2 files)", id1.join("pak0.pak").display()),
                id1.display().to_string(),
            ]
        );
    }

    #[test]
    fn a_pop_lmp_that_is_not_ids_is_a_corrupted_data_file() {
        let mut pop = pop_lmp();
        pop[21] ^= 1;
        let base = basedir("corrupted", &[pak0(), write_pack(&[("gfx/pop.lmp", &pop)])]);
        assert_eq!(init_filesystem(&base).err().as_deref(), Some("Corrupted data file."));
        let short = basedir("short", &[pak0(), write_pack(&[("gfx/pop.lmp", &pop_lmp()[..200])])]);
        assert_eq!(init_filesystem(&short).err().as_deref(), Some("Corrupted data file."));
    }

    #[test]
    fn the_search_stops_at_the_first_missing_pak_and_refuses_a_non_pack() {
        // pak2 without pak1 is never loaded (COM_AddGameDirectory's loop ends).
        let base = basedir("gap", &[pak0()]);
        std::fs::write(base.join(GAMENAME).join("pak2.pak"), write_pack(&[("gfx/pop.lmp", &pop_lmp())])).unwrap();
        let refused = init_filesystem(&base).err();
        assert_eq!(refused.as_deref(), Some("You must have the registered version to use modified games"), "pak2 is not on the path");
        std::fs::write(base.join(GAMENAME).join("pak1.pak"), b"not a pack at all").unwrap();
        let err = init_filesystem(&base).unwrap_err();
        assert!(err.ends_with("pak1.pak is not a packfile"), "{err}");
    }

    #[test]
    fn a_loose_pop_lmp_registers_as_id_found_it() {
        // COM_CheckRegistered searches with static_registered still 1: a loose
        // gfx/pop.lmp counts, and then loose files in subdirectories do too.
        let base = basedir("loose", &[pak0()]);
        let id1 = base.join(GAMENAME);
        std::fs::create_dir_all(id1.join("gfx")).unwrap();
        std::fs::create_dir_all(id1.join("maps")).unwrap();
        std::fs::write(id1.join("gfx/pop.lmp"), pop_lmp()).unwrap();
        std::fs::write(id1.join("maps/mine.bsp"), b"mine").unwrap();
        let fs = init_filesystem(&base).expect("registered by the loose lump");
        assert!(fs.registered);
        assert_eq!(fs.files.read_file("maps/mine.bsp").unwrap().as_deref(), Some(&b"mine"[..]));
    }

    #[test]
    fn progs_with_builtins_id_never_had_are_refused() {
        let ok = write_pack(&[("progs.dat", &progs_image(&[1, -1, -78], PROGHEADER_CRC))]);
        let pak = Pak::from_bytes("pak0.pak".into(), ok).unwrap();
        assert_eq!(check_progs(&pak), Ok(()));
        // A builtin by name (#0) and one past pr_builtin[]'s 79.
        let foreign = write_pack(&[("progs.dat", &progs_image(&[1, 0, -99], PROGHEADER_CRC))]);
        let err = check_progs(&Pak::from_bytes("pak0.pak".into(), foreign).unwrap()).unwrap_err();
        assert!(err.starts_with("progs.dat calls 2 builtins id's engine does not have (ex_bot_movetopoint #0, checkextension #99)"), "{err}");
        let other_defs = write_pack(&[("progs.dat", &progs_image(&[1], 1234))]);
        assert_eq!(
            check_progs(&Pak::from_bytes("pak0.pak".into(), other_defs).unwrap()),
            Err("progs.dat system vars have been modified, progdefs.h is out of date".to_string())
        );
        let none = Pak::from_bytes("pak0.pak".into(), write_pack(&[])).unwrap();
        assert_eq!(check_progs(&none), Err("PR_LoadProgs: couldn't load progs.dat".to_string()));
    }

    /// id's own data: the shareware `pak0.pak` alone is id's shareware game,
    /// unmodified, and its progs.dat has id's CRC and only id's builtins.
    #[test]
    fn ids_shareware_pak_is_the_shareware_game() {
        let pak0 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(bytes) = std::fs::read(&pak0) else {
            eprintln!("skipped: no shareware pak at {}", pak0.display());
            return;
        };
        let base = basedir("id", &[bytes]);
        let fs = init_filesystem(&base).expect("id's shareware game starts");
        assert!(!fs.registered && !fs.modified);
        let file = base.join(GAMENAME).join("pak0.pak");
        assert_eq!(fs.log, [format!("Added packfile {} (339 files)", file.display()), "Playing shareware version.".into()]);
        assert_eq!(check_progs(&fs.files), Ok(()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
