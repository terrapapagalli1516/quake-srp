//! The file system — common.c's `COM_InitFilesystem`: the base directory
//! (`-basedir`, default `.`), the game directory `id1` under it, and
//! `pak0.pak` in that; `COM_WriteFile`/`COM_LoadFile` for the game's own
//! files (the saves, `config.cfg`), which live in the game directory as in
//! id's. All of it is `std::fs`: in the browser the WASI host in
//! `web/wasi.js` serves the calls from the page's storage; natively they are
//! the real disk.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use quake_rs::pak::Pak;

/// `GAMENAME` (quakedef.h): the game directory under the base directory.
pub(crate) const GAMENAME: &str = "id1";

/// The opened `pak0.pak`, set once by [`init`] (or, in tests, found next to
/// the crate on first use).
static PAK: OnceLock<Option<Pak>> = OnceLock::new();

thread_local! {
    /// `com_gamedir`. A thread-local so each test gets its own directory,
    /// the way each test gets its own `App`.
    static GAMEDIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// `COM_InitFilesystem` with the one search path the shareware game needs:
/// `<basedir>/id1`, and `pak0.pak` in it (`COM_AddGameDirectory`), opened as
/// `quaketool` opens it — the directory now, each file's bytes when they are
/// read ([`Pak::open`]), so no copy of the archive lives in the program.
pub(crate) fn init(basedir: &Path) -> Result<(), String> {
    let gamedir = basedir.join(GAMENAME);
    let pak_path = gamedir.join("pak0.pak");
    let pak = Pak::open(&pak_path).map_err(|e| format!("couldn't open {}: {e}", pak_path.display()))?;
    GAMEDIR.with(|g| *g.borrow_mut() = Some(gamedir));
    // A second init (tests only) keeps the first pak: it is the same file.
    let _ = PAK.set(Some(pak));
    Ok(())
}

/// A handle on `pak0.pak` (its directory; entries read on demand), or
/// `None` before [`init`]. Cloning copies the directory only.
pub(crate) fn pak() -> Option<Pak> {
    PAK.get_or_init(default_pak).clone()
}

/// The tests read the shareware pak where the repo keeps it.
#[cfg(test)]
fn default_pak() -> Option<Pak> {
    Pak::open(concat!(env!("CARGO_MANIFEST_DIR"), "/../quake-data/ID1/PAK0.PAK")).ok()
}

#[cfg(not(test))]
fn default_pak() -> Option<Pak> {
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
    }

    #[test]
    fn init_needs_pak0_in_the_game_directory() {
        let empty = gamedir();
        let err = init(&empty).unwrap_err();
        assert!(err.contains("pak0.pak"), "{err}");
    }
}
