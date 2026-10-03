//! The host session's settings — id's console variables ([`Cvars`]) and key
//! bindings ([`Bindings`]) as one value — and the port's two profiles.
//!
//! Every departure from id's WinQuake is a setting ([`crate::cvar::CVARS`]
//! marks them), and a profile sets them all at once:
//!
//! - **Classic** ([`Profile::Classic`]): every departure off and id's
//!   `default.cfg` bindings. The game is WinQuake: its frames (the goldens,
//!   the C oracle), its game state and its 72 fps timing
//!   (`oracle/classic_check.py` proves it).
//! - **2026** ([`Profile::Modern`], the default): the best experience of an
//!   idealized software-rendered Quake in 2026 ([`Cvars::modern`]), with the
//!   WASD layout ([`Bindings::with_wasd`]) and a gamepad layout
//!   ([`Bindings::with_gamepad`]).
//!
//! Switching profile resets the departures and the bindings to the new
//! profile's and keeps id's own settings (Screen size, Brightness, the
//! volumes, the mouse). A new departure takes a slot here, as smooth monster
//! movement (`r_lerpmove`), the 2026 mixer (`snd_modern`), the touch
//! controls (`in_touch`) and the pad (`joy_*`) did: a departure field in
//! [`Cvars`], on in [`Cvars::modern`].
//!
//! **Persistence, the id way.** [`Settings::config_text`] is
//! `Host_WriteConfiguration`'s `config.cfg`: `bind` lines and archived cvars,
//! exec'd at the next start as `quake.rc` does. It keeps what the player
//! changed and nothing else — a `profile` line, then only the bindings and
//! archived cvars that differ from that profile's defaults — so a returning
//! player gets whatever the profile's defaults are by then (a 2026 default
//! improved or added in a later version reaches them) while every setting
//! they changed stays as they left it. (id's file lists every value: id's
//! defaults never changed after release.)

use crate::cvar::{self, Cvars};
use crate::keys::Bindings;
use crate::screen::VIEWSIZE_DEFAULT;

/// The two profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// id's WinQuake: every departure off, `default.cfg`'s bindings.
    Classic,
    /// The 2026 profile: the port's departures on, WASD.
    #[default]
    Modern,
}

impl Profile {
    /// Its name, as the console and the menu print it.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Classic => "classic",
            Profile::Modern => "2026",
        }
    }

    /// The profile a console argument names (`classic`, `2026`, any case).
    pub fn parse(s: &str) -> Option<Profile> {
        match s.trim().to_ascii_lowercase().as_str() {
            "classic" | "id" | "0" => Some(Profile::Classic),
            "2026" | "modern" | "1" => Some(Profile::Modern),
            _ => None,
        }
    }

    /// The other one.
    pub fn toggled(self) -> Profile {
        match self {
            Profile::Classic => Profile::Modern,
            Profile::Modern => Profile::Classic,
        }
    }

    /// The profile's cvars.
    pub fn cvars(self) -> Cvars {
        match self {
            Profile::Classic => Cvars::classic(),
            Profile::Modern => Cvars::modern(),
        }
    }

    /// The profile's bindings: `default.cfg`, with WASD and the gamepad
    /// layout in 2026.
    pub fn bindings(self) -> Bindings {
        match self {
            Profile::Classic => Bindings::default_cfg(),
            Profile::Modern => Bindings::default_cfg().with_wasd().with_gamepad(),
        }
    }
}

/// The session's settings: the profile they start from, the cvars, the key
/// bindings. The host owns one and hands it by reference to the menu (which
/// edits it), the input and the frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The profile the settings were last reset to (`config.cfg`'s base).
    pub profile: Profile,
    /// The console variables.
    pub cvars: Cvars,
    /// keys.c's `keybindings`.
    pub binds: Bindings,
}

impl Default for Settings {
    /// The 2026 profile's defaults.
    fn default() -> Self {
        Settings::new(Profile::default())
    }
}

impl Settings {
    /// `profile`'s defaults.
    pub fn new(profile: Profile) -> Settings {
        Settings { profile, cvars: profile.cvars(), binds: profile.bindings() }
    }

    /// Switch to `profile`: every departure and every binding to its
    /// defaults; id's own settings are kept.
    pub fn set_profile(&mut self, profile: Profile) {
        let defaults = profile.cvars();
        for c in cvar::CVARS.iter().filter(|c| c.departure) {
            c.set(&mut self.cvars, &c.get(&defaults));
        }
        self.binds = profile.bindings();
        self.profile = profile;
    }

    /// Options > "Reset to defaults", `exec default.cfg`: `unbindall`, the
    /// profile's bindings, and `default.cfg`'s four cvars — `viewsize 100`,
    /// `gamma 1.0`, `volume 0.7`, `sensitivity 3`. Nothing else: the rest of
    /// the Options (Always Run, Invert Mouse, the look toggles, CD volume),
    /// the video mode and the departures keep their values, as in WinQuake.
    pub fn reset_defaults(&mut self) {
        self.binds = self.profile.bindings();
        let c = &mut self.cvars;
        c.viewsize = VIEWSIZE_DEFAULT;
        c.gamma = 1.0;
        c.volume = 0.7;
        c.sensitivity = 3.0;
    }

    /// `Host_WriteConfiguration`'s `config.cfg` (`Key_WriteBindings` +
    /// `Cvar_WriteVariables`): the profile, then the bindings and archived
    /// cvars that differ from its defaults (see the module docs for why only
    /// those).
    pub fn config_text(&self) -> String {
        let mut t = String::from("// generated by quake, do not modify\n");
        t.push_str(&format!("profile \"{}\"\n", self.profile.name()));
        self.binds.write_changes(&self.profile.bindings(), &mut t);
        cvar::write_changes(&self.cvars, &self.profile.cvars(), &mut t);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{BIND_ATTACK, BIND_FORWARD, BIND_LOOKUP};

    #[test]
    fn the_2026_profile_is_the_default_and_classic_is_ids() {
        let s = Settings::default();
        assert_eq!(s.profile, Profile::Modern);
        assert!(s.cvars.uncapped && s.cvars.native && s.cvars.always_run());
        assert_eq!(s.cvars.crosshair, crate::render::Crosshair::Cross);
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD));
        let id = Settings::new(Profile::Classic);
        assert_eq!(id.cvars, Cvars::classic());
        assert_eq!(id.binds, Bindings::default_cfg());
        assert_eq!(id.binds.command(b'a'), Some(BIND_LOOKUP));
    }

    #[test]
    fn switching_profile_resets_departures_and_keys_and_keeps_ids_settings() {
        let mut s = Settings::default();
        s.cvars.viewsize = 80.0;
        s.cvars.gamma = 0.8;
        s.cvars.show_fps = true;
        s.cvars.pixel_size = 3;
        s.binds.bind(b'j', BIND_ATTACK);
        s.set_profile(Profile::Classic);
        let mut want = Cvars::classic();
        want.viewsize = 80.0;
        want.gamma = 0.8;
        assert_eq!(s.cvars, want, "every departure off, id's settings kept");
        assert_eq!(s.binds, Bindings::default_cfg());
        s.set_profile(Profile::Modern);
        assert!(s.cvars.uncapped && !s.cvars.show_fps && s.cvars.pixel_size == 0);
        assert_eq!((s.cvars.viewsize, s.cvars.gamma), (80.0, 0.8));
    }

    #[test]
    fn config_cfg_keeps_the_profile_and_what_differs_from_it() {
        let s = Settings::default();
        assert_eq!(s.config_text(), "// generated by quake, do not modify\nprofile \"2026\"\n", "nothing changed");
        let mut s = Settings::new(Profile::Classic);
        s.cvars.viewsize = 110.0;
        s.cvars.show_fps = true;
        s.binds.bind(b'w', BIND_FORWARD);
        s.binds.set(b'z', None);
        assert_eq!(
            s.config_text(),
            "// generated by quake, do not modify\nprofile \"classic\"\nbind \"w\" \"+forward\"\nunbind \"z\"\n\
             viewsize \"110\"\nwasm_showfps \"1\"\n"
        );
    }

    #[test]
    fn reset_defaults_is_the_profiles_default_cfg() {
        let mut s = Settings::default();
        s.cvars.viewsize = 50.0;
        s.cvars.bgmvolume = 0.3;
        s.cvars.set_always_run(false);
        s.binds.unbind_all();
        s.reset_defaults();
        assert_eq!(s.cvars.viewsize, 100.0);
        assert_eq!(s.cvars.bgmvolume, 0.3, "not in default.cfg");
        assert!(!s.cvars.always_run(), "not in default.cfg");
        assert_eq!(s.binds, Profile::Modern.bindings());
        assert_eq!((Profile::parse("Classic"), Profile::parse("2026"), Profile::parse("x")), (Some(Profile::Classic), Some(Profile::Modern), None));
    }
}
