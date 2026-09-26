//! Shared fixtures for the crate's `#[cfg(test)]` modules: a synthetic pak
//! builder and the helpers that drive the live App the way the page does.

use quake_rs::pak::{Pak, DIRENTRY_SIZE, HEADER_SIZE, NAME_SIZE};
use quake_rs::render;

use crate::app::{Walk, APP};
use crate::console::{console_char, console_enter};

/// Build a synthetic PACK image holding the given (name, contents) files, so
/// a test can hand the client files of its own without depending on the
/// shareware pak's contents.
pub(crate) fn build_test_pak(files: &[(&str, &[u8])]) -> Pak {
    let mut contents = Vec::new();
    let mut positions = Vec::new();
    let mut cursor = HEADER_SIZE as i32;
    for (_, data) in files {
        positions.push((cursor, data.len() as i32));
        contents.extend_from_slice(data);
        cursor += data.len() as i32;
    }
    let dirofs = HEADER_SIZE + contents.len();
    let dirlen = files.len() * DIRENTRY_SIZE;

    let mut img = Vec::new();
    img.extend_from_slice(b"PACK");
    img.extend_from_slice(&(dirofs as i32).to_le_bytes());
    img.extend_from_slice(&(dirlen as i32).to_le_bytes());
    img.extend_from_slice(&contents);
    for (i, (name, _)) in files.iter().enumerate() {
        let mut name_field = [0u8; NAME_SIZE];
        let b = name.as_bytes();
        name_field[..b.len()].copy_from_slice(b);
        img.extend_from_slice(&name_field);
        img.extend_from_slice(&positions[i].0.to_le_bytes());
        img.extend_from_slice(&positions[i].1.to_le_bytes());
    }
    Pak::from_bytes("test".into(), img).expect("synthetic pak")
}

/// Forget the sound calls waiting for the mixer (`snd_dma`).
pub(crate) fn reset_queue() {
    crate::snd_dma::clear_pending();
}

/// Read a player-edict float field from the live walk (0.0 if no walk).
pub(crate) fn player_field(name: &str) -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|a| a.walk.as_ref())
            .map(|w| w.server.vm.ent_get_float(w.player, name))
            .unwrap_or(0.0)
    })
}

/// Type a whole line into the (open) console and submit it.
pub(crate) fn run_console_line(line: &str) {
    for ch in line.chars() {
        console_char(ch as u32);
    }
    console_enter();
}

/// IT_ROCKET_LAUNCHER (defs.qc).
pub(crate) const IT_RL: i32 = 32;

/// Borrow the live walk mutably (panics if no walk — these tests boot first).
pub(crate) fn walk_mut<R>(f: impl FnOnce(&mut Walk) -> R) -> R {
    APP.with(|c| f(c.borrow_mut().as_mut().unwrap().walk.as_mut().unwrap()))
}

/// Close the App-level menu: `boot()` opens it over the walk, and while it is
/// up (`key_dest != key_game`) the gameplay buttons IntermissionThink polls
/// are gated to 0 — the player must dismiss it, and so must these tests.
/// Switch the session to the 2026 profile (the tests start in Classic): its
/// departures and its WASD keys.
pub(crate) fn use_2026() {
    crate::app::ensure_app(|a| a.settings.set_profile(quake_rs::settings::Profile::Modern));
}

pub(crate) fn close_menu() {
    APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.close());
}

/// The menu screen currently showing (test-side peek at the App menu).
pub(crate) fn menu_screen() -> render::MenuScreen {
    APP.with(|c| c.borrow().as_ref().unwrap().menu.screen())
}

/// The "scaled 2-D" extra on for as long as the guard lives, then back off
/// (so a failing test cannot leak it into the next test on its thread).
pub(crate) struct Scaled2dGuard;

impl Scaled2dGuard {
    pub(crate) fn set(on: bool) -> Scaled2dGuard {
        quake_rs::draw::set_scaled_2d(on);
        Scaled2dGuard
    }
}

impl Drop for Scaled2dGuard {
    fn drop(&mut self) {
        quake_rs::draw::set_scaled_2d(false);
    }
}
