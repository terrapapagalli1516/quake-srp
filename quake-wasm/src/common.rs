//! The file system — common.c's `COM_InitFilesystem` on this platform: the
//! base directory (`-basedir`, default `.`), the game directory `id1` under
//! it and id's search path through it ([`quake_rs::common`]: `pak0.pak`,
//! the player's `pak1.pak` if they added it, the loose files), and
//! `COM_WriteFile`/`COM_LoadFile` for the game's own files (the saves,
//! `config.cfg`), which live in the game directory as in id's. All of it is
//! `std::fs`: in the browser the WASI host in `web/wasi.js` serves the calls
//! from the page's storage; natively they are the real disk.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use quake_rs::common::{check_progs, init_filesystem, Filesystem};
use quake_rs::pak::Pak;

/// `GAMENAME` (quakedef.h): the game directory under the base directory.
pub(crate) const GAMENAME: &str = quake_rs::common::GAMENAME;

/// The search path and what `COM_CheckRegistered` found, set once by
/// [`init`] (or, in tests, the shareware pak next to the crate on first
/// use).
static FILES: OnceLock<Option<Filesystem>> = OnceLock::new();

thread_local! {
    /// `com_gamedir`. A thread-local so each test gets its own directory,
    /// the way each test gets its own `App`.
    static GAMEDIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// `COM_InitFilesystem` and `COM_CheckRegistered`, then `PR_LoadProgs`'s
/// checks of the game's `progs.dat`: the search path opened as `quaketool`
/// opens a pak — each directory now, each file's bytes when it is read
/// ([`Pak::open`]), so no copy of an archive lives in the program. Returns
/// what id printed on the way (the packs, "Playing … version."); an `Err`
/// is the `Sys_Error` the game stops with.
pub(crate) fn init(basedir: &Path) -> Result<Vec<String>, String> {
    let fs = init_filesystem(basedir)?;
    check_progs(&fs.files)?;
    let log = fs.log.clone();
    GAMEDIR.with(|g| *g.borrow_mut() = Some(fs.gamedir.clone()));
    // A second init (tests only) keeps the first path: it is the same files.
    let _ = FILES.set(Some(fs));
    Ok(log)
}

/// The search path's head (the directories; entries read on demand), or
/// `None` before [`init`]. Cloning copies the first pack's directory only.
pub(crate) fn pak() -> Option<Pak> {
    files().map(|f| f.files.clone())
}

/// `static_registered`: the path holds id's `gfx/pop.lmp`.
pub(crate) fn registered() -> bool {
    files().is_some_and(|f| f.registered)
}

/// `COM_Path_f`'s lines (nothing before [`init`]).
pub(crate) fn path_lines() -> Vec<String> {
    files().map(|f| quake_rs::common::path_lines(&f.files)).unwrap_or_default()
}

fn files() -> Option<&'static Filesystem> {
    FILES.get_or_init(default_files).as_ref()
}

/// The tests read the shareware pak where the repo keeps it, alone on the
/// path (unregistered).
#[cfg(test)]
fn default_files() -> Option<Filesystem> {
    let pak = Pak::open(concat!(env!("CARGO_MANIFEST_DIR"), "/../quake-data/ID1/PAK0.PAK")).ok()?;
    Some(Filesystem { files: pak, gamedir: default_gamedir(), registered: false, modified: false, log: Vec::new() })
}

#[cfg(not(test))]
fn default_files() -> Option<Filesystem> {
    None
}

/// `com_gamedir`: where the saves and `config.cfg` go.
pub(crate) fn gamedir() -> PathBuf {
    GAMEDIR.with(|g| g.borrow_mut().get_or_insert_with(default_gamedir).clone())
}

/// The program's game directory before [`init`] runs: `./id1`.
#[cfg(not(test))]
fn default_gamedir() -> PathBuf {
    PathBuf::from(GAMENAME)
}

/// Each test thread gets an empty directory of its own under `target/`
/// (never `/tmp`), wiped on first use so a previous run's files cannot leak
/// in.
#[cfg(test)]
fn default_gamedir() -> PathBuf {
    let id = format!("{:?}", std::thread::current().id());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-gamedirs")
        .join(id.trim_start_matches("ThreadId(").trim_end_matches(')'));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test gamedir");
    dir
}

/// `COM_WriteFile`: `name` (relative to the game directory) with `data`.
pub(crate) fn write_file(name: &str, data: &[u8]) -> io::Result<()> {
    std::fs::write(gamedir().join(name), data)
}

/// `COM_LoadFile` for the game directory's own files (not the pak's).
pub(crate) fn read_file(name: &str) -> io::Result<Vec<u8>> {
    std::fs::read(gamedir().join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip_through_the_game_directory() {
        assert!(read_file("s7.sav").is_err(), "a fresh gamedir is empty");
        write_file("s7.sav", b"SAVEGAME").unwrap();
        assert_eq!(read_file("s7.sav").unwrap(), b"SAVEGAME");
        assert!(gamedir().join("s7.sav").exists());
    }

    #[test]
    fn the_pak_opens_by_directory_and_reads_on_demand() {
        let pak = pak().expect("the shareware pak");
        assert!(pak.find("maps/e1m1.bsp").is_some());
        let palette = pak.read_file("gfx/palette.lmp").unwrap().unwrap();
        assert_eq!(palette.len(), 768);
        assert!(!registered());
    }

    #[test]
    fn init_needs_a_game_directory_with_ids_pak0() {
        // An empty game directory: no pak0.pak, so no progs.dat on the path.
        let empty = gamedir().join("empty-base");
        std::fs::create_dir_all(empty.join(GAMENAME)).unwrap();
        let err = init(&empty).unwrap_err();
        assert_eq!(err, "PR_LoadProgs: couldn't load progs.dat");
    }
}
