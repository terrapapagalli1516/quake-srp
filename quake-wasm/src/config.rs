//! `config.cfg` — host.c's `Host_WriteConfiguration` and quake.rc's `exec
//! config.cfg`: the settings a session leaves for the next one, as console
//! lines in the game directory.
//!
//! The text is the engine's ([`quake_rs::settings::Settings::config_text`]):
//! id's `bind` lines and archived cvars, starting from a `preset` line, and
//! holding only what the player changed from that preset's values — so a
//! returning player keeps every choice they made and gets whatever else the
//! preset's values are now. id's writes the file when the game quits; a
//! page is never told it quits, so the port writes it as soon as the text
//! changes ([`write_if_changed`], once a frame).
//!
//! **Files from before the presets.** The page kept the video mode,
//! `viewsize` and the four Web extras, and the first `config.cfg` wrote all
//! six whatever their values (and the page moved its localStorage keys into
//! one the same way). Such a file names no preset (no `preset` line, nor
//! `profile`, its old name). Its lines that merely restate that page's
//! defaults ([`LEGACY_DEFAULTS`]) say nothing about the player — they would
//! switch the slop preset's uncapped frame rate and scaled 2-D layer back
//! off — so they are skipped, and the rest is a choice the player made and
//! runs over the default preset.

use crate::app::APP;
use crate::common;
use crate::host_cmd::execute_console_command;

/// The file's name in the game directory.
pub(crate) const CONFIG_CFG: &str = "config.cfg";

/// The lines a `config.cfg` from before the presets wrote for values the
/// page had never been asked to change: its defaults then. (`viewsize 100`
/// was then the default: it says the player never moved Screen size, so the
/// slop preset's own start, one step larger, applies. The same goes for the
/// localStorage the page moved into such a file.)
pub(crate) const LEGACY_DEFAULTS: [&str; 6] = [
    "_vid_resolution 960x600",
    "viewsize 100",
    "wasm_uncapped 0",
    "wasm_showfps 0",
    "wasm_exactpersp 0",
    "wasm_scaled2d 0",
];

/// The file's text for the live settings, or `None` before the App exists.
pub(crate) fn current_text() -> Option<String> {
    APP.with(|c| c.borrow().as_ref().map(|a| a.settings.config_text()))
}

/// `Host_WriteConfiguration` when the text differs from `last` (the file as
/// last written or read): write it and remember it. A failed write is tried
/// again on the next frame.
pub(crate) fn write_if_changed(last: &mut Option<String>) {
    let Some(now) = current_text() else { return };
    if last.as_ref() == Some(&now) {
        return;
    }
    if common::write_file(CONFIG_CFG, now.as_bytes()).is_ok() {
        *last = Some(now);
    }
}

/// The command lines of a `config.cfg`'s text to run: all of them, but for a
/// file from before the presets (no `preset` or `profile` line) the lines
/// that only restate that page's defaults ([`LEGACY_DEFAULTS`]).
pub(crate) fn lines_to_run(text: &str) -> Vec<&str> {
    // Each command line with its words as the console reads them (quotes
    // gone), to recognise the preset and the legacy defaults by.
    let words = |l: &str| quake_rs::cmd::Args::tokenize(l).all().join(" ");
    let lines: Vec<&str> = quake_rs::cmd::split_lines(text)
        .into_iter()
        .map(str::trim)
        .filter(|l| quake_rs::cmd::Args::tokenize(l).argc() > 0)
        .collect();
    let names_a_preset = |l: &&str| matches!(quake_rs::cmd::Args::tokenize(l).argv(0), "preset" | "profile");
    let legacy = !lines.iter().any(names_a_preset);
    lines.into_iter().filter(|l| !(legacy && LEGACY_DEFAULTS.contains(&words(l).as_str()))).collect()
}

/// quake.rc's `exec config.cfg` (`Cmd_Exec_f`): the file's lines through the
/// console ([`lines_to_run`]). A missing file is not an error (the first
/// session). Returns the file's text as it stands now, so the first frame
/// does not write back what was just read.
pub(crate) fn exec_config() -> Option<String> {
    let bytes = common::read_file(CONFIG_CFG).ok()?;
    for line in lines_to_run(&String::from_utf8_lossy(&bytes)) {
        execute_console_command(line);
    }
    current_text()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, ensure_app, APP};
    use quake_rs::client::host::FrameCap;
    use quake_rs::settings::{Machine, Preset, Settings};

    fn settings() -> Settings {
        APP.with(|c| c.borrow().as_ref().unwrap().settings.clone())
    }

    #[test]
    fn the_settings_survive_a_session_through_config_cfg() {
        assert_eq!(boot(), 1);
        ensure_app(|a| a.settings.apply_preset(Preset::Slop));
        crate::host_cmd::execute_console_command("viewsize 80; wasm_showfps 1; vid_pixelsize 3; crosshair 2");
        crate::host_cmd::execute_console_command("bind j \"+jump\"; unbind c; bind k \"echo hi; god\"");
        let mut last = None;
        write_if_changed(&mut last);
        let text = String::from_utf8(common::read_file(CONFIG_CFG).unwrap()).unwrap();
        assert!(text.starts_with("// generated by quake, do not modify\npreset \"slop\"\n"), "{text}");
        for line in ["bind \"j\" \"+jump\"", "unbind \"c\"", "bind \"k\" \"echo hi; god\"", "viewsize \"80\"",
                     "wasm_showfps \"1\"", "vid_pixelsize \"3\"", "crosshair \"2\""] {
            assert!(text.contains(&format!("{line}\n")), "{line} in {text}");
        }
        assert!(!text.contains("wasm_uncapped"), "a slop value is not written: {text}");
        let before = settings();

        // A new session: the defaults, then the file.
        APP.with(|c| *c.borrow_mut() = None);
        assert_eq!(boot(), 1);
        assert_ne!(settings(), before);
        let read = exec_config().expect("the file is there");
        assert_eq!(Some(read), last, "exec restores what was written");
        assert_eq!(settings(), before, "every setting and binding as it was");
    }

    #[test]
    fn a_classic_player_keeps_classic_and_nothing_is_written_until_a_change() {
        assert_eq!(boot(), 1);
        ensure_app(|a| a.settings = Settings::default());
        let mut last = current_text();
        write_if_changed(&mut last);
        assert!(common::read_file(CONFIG_CFG).is_err(), "unchanged: no file");
        crate::host_cmd::execute_console_command("preset classic");
        write_if_changed(&mut last);
        let text = String::from_utf8(common::read_file(CONFIG_CFG).unwrap()).unwrap();
        assert_eq!(text, "// generated by quake, do not modify\npreset \"classic\"\n");
        APP.with(|c| *c.borrow_mut() = None);
        assert_eq!(boot(), 1);
        ensure_app(|a| a.settings = Settings::default());
        exec_config();
        assert_eq!(settings(), Settings::new(Preset::Classic, Machine::default()), "Classic, whatever the start");
    }

    #[test]
    fn a_config_from_before_the_presets_keeps_only_what_the_player_chose() {
        // The first config.cfg: all six lines, one of them a choice.
        let old = "// generated by quake, do not modify\n_vid_resolution 960x600\nviewsize 100\n\
                   wasm_uncapped 0\nwasm_showfps 1\nwasm_exactpersp 0\nwasm_scaled2d 0\n";
        assert_eq!(lines_to_run(old), ["wasm_showfps 1"]);
        // A file with a preset line runs whole, under either name.
        assert_eq!(lines_to_run("preset \"slop\"\nviewsize \"100\"\n"), ["preset \"slop\"", "viewsize \"100\""]);
        assert_eq!(lines_to_run("profile \"2026\"\nviewsize \"100\"\n"), ["profile \"2026\"", "viewsize \"100\""]);
        assert_eq!(boot(), 1);
        ensure_app(|a| a.settings = Settings::default());
        common::write_file(CONFIG_CFG, old.as_bytes()).unwrap();
        exec_config();
        let s = settings();
        assert!(s.cvars.show_fps, "the choice is kept");
        assert!(s.cvars.max_fps == FrameCap::NONE && s.cvars.scaled_2d, "the slop values are not switched off");
        assert_eq!(s.preset, Preset::Slop);
        assert_eq!(s.cvars.viewsize, 110.0, "its `viewsize 100` only restated the old default: slop's start applies");
    }

    /// A file written before the presets were renamed (`profile "2026"`,
    /// `profile "classic"`) names the same preset, and the next write, for a
    /// change, uses the new words.
    #[test]
    fn a_config_from_before_the_rename_names_the_same_preset() {
        let head = "// generated by quake, do not modify\n";
        for (old, preset) in [("2026", Preset::Slop), ("classic", Preset::Classic)] {
            APP.with(|c| *c.borrow_mut() = None);
            assert_eq!(boot(), 1);
            ensure_app(|a| a.settings = Settings::default());
            common::write_file(CONFIG_CFG, format!("{head}profile \"{old}\"\nwasm_showfps \"1\"\n").as_bytes()).unwrap();
            let mut last = exec_config();
            assert_eq!((settings().preset, settings().cvars.show_fps), (preset, true), "profile \"{old}\"");
            crate::host_cmd::execute_console_command("viewsize 90");
            write_if_changed(&mut last);
            let text = String::from_utf8(common::read_file(CONFIG_CFG).unwrap()).unwrap();
            assert_eq!(text, format!("{head}preset \"{}\"\nviewsize \"90\"\nwasm_showfps \"1\"\n", preset.name()));
        }
    }

    /// Screen size across sessions: the default is not written, so a player
    /// who never moved it gets the preset's start whatever that is by then
    /// (slop's 110 now, id's 100 in Classic), and a size the player chose
    /// stays, in either preset (the 100 a slop player picked included).
    #[test]
    fn the_screen_size_in_config_cfg_is_the_presets_unless_the_player_chose_one() {
        let session = |file: &str| {
            APP.with(|c| *c.borrow_mut() = None);
            assert_eq!(boot(), 1);
            ensure_app(|a| a.settings = Settings::default());
            common::write_file(CONFIG_CFG, file.as_bytes()).unwrap();
            exec_config();
            settings().cvars.viewsize
        };
        let head = "// generated by quake, do not modify\n";
        assert_eq!(session(&format!("{head}preset \"slop\"\n")), 110.0, "never moved, slop");
        assert_eq!(session(&format!("{head}preset \"classic\"\n")), 100.0, "never moved, Classic");
        assert_eq!(session(&format!("{head}preset \"slop\"\nviewsize \"90\"\n")), 90.0, "moved, slop");
        assert_eq!(session(&format!("{head}preset \"classic\"\nviewsize \"90\"\n")), 90.0, "moved, Classic");
        assert_eq!(session(&format!("{head}preset \"slop\"\nviewsize \"100\"\n")), 100.0, "a slop player's 100 is a choice");
        assert_eq!(session(&format!("{head}preset \"classic\"\nviewsize \"110\"\n")), 110.0, "a Classic player's 110 is a choice");
        // What is written: slop's 110 is not, a chosen 100 is.
        ensure_app(|a| a.settings = Settings::default());
        assert_eq!(current_text().unwrap(), format!("{head}preset \"slop\"\n"));
        crate::host_cmd::execute_console_command("viewsize 100");
        assert_eq!(current_text().unwrap(), format!("{head}preset \"slop\"\nviewsize \"100\"\n"));
    }

    /// A cvar renamed since the file was saved (`quake_rs::cvar`'s old
    /// names): its line still sets the setting — the slop player who
    /// switched F's fullscreen off keeps the fullscreen key off now that it
    /// is Alt+Enter — and the file, next written for a change, has the new
    /// name. (Until then it stays as it was: nothing changed.)
    #[test]
    fn a_config_with_a_renamed_cvar_keeps_the_choice_under_the_new_name() {
        assert_eq!(boot(), 1);
        ensure_app(|a| a.settings = Settings::default());
        let saved = "// generated by quake, do not modify\npreset \"slop\"\nvid_fkey \"0\"\n";
        common::write_file(CONFIG_CFG, saved.as_bytes()).unwrap();
        let mut last = exec_config();
        assert!(!settings().cvars.alt_enter, "vid_fkey 0 switched the fullscreen key off");
        write_if_changed(&mut last);
        assert_eq!(common::read_file(CONFIG_CFG).unwrap(), saved.as_bytes(), "no change, no write");
        crate::host_cmd::execute_console_command("viewsize 90");
        write_if_changed(&mut last);
        let text = String::from_utf8(common::read_file(CONFIG_CFG).unwrap()).unwrap();
        assert_eq!(text, "// generated by quake, do not modify\npreset \"slop\"\nviewsize \"90\"\nvid_altenter \"0\"\n");
    }

    #[test]
    fn a_missing_config_is_the_first_session() {
        assert_eq!(boot(), 1);
        assert_eq!(exec_config(), None);
    }
}
