//! The host session's settings — id's console variables ([`Cvars`]) and key
//! bindings ([`Bindings`]) as one value — and the port's two profiles.
//!
//! **The profile is the engine.** Every departure from id's WinQuake is a
//! setting ([`crate::cvar::CVARS`] marks them [`crate::cvar::Cvar::departure`]),
//! and switching profile resets them all at once:
//!
//! - **Classic** ([`Profile::Classic`]): every departure off. The game is
//!   WinQuake: its frames (the goldens, the C oracle), its game state and
//!   its 72 fps timing (`oracle/classic_check.py` proves it).
//! - **2026** ([`Profile::Modern`], the default): the best experience of an
//!   idealized software-rendered Quake in 2026 ([`Cvars::modern`]).
//!
//! **The controls are the player's**, not the engine's, so a profile switch
//! leaves them alone, like id's own settings (Screen size, Brightness, the
//! volumes, the mouse): WASD and the gamepad ([`Bindings::with_wasd`],
//! [`Bindings::with_gamepad`] — [`Profile::bindings`] applies both to
//! *either* profile), mouse look, Space-swims-up, Alt+Enter and Always Run
//! (`freelook`, `cl_jumpswim`, `vid_altenter`, `cl_forwardspeed`/
//! `cl_backspeed` — on by default in [`Cvars::classic`] too, the module
//! docs of [`crate::cvar`] say why). Classic with the original controls is
//! hard to use these days: that was the user's call (2026-10-03); id's own
//! 1996 controls are one explicit step away, never a profile switch:
//! [`Settings::id`], and the console's `idcontrols`. The one exception is
//! the mouse wheel's weapon cycle ([`Bindings::with_wheel`]): the user
//! wanted that to stay a 2026-only departure, so [`Settings::set_profile`]
//! still turns it on and off with the profile.
//!
//! A new *engine* departure takes a `departure` slot here, as smooth
//! monster movement (`r_lerpmove`), the 2026 mixer (`snd_modern`) and the
//! touch controls (`in_touch`) did; a new *control* takes one too, but
//! [`Settings::set_profile`] must also learn to leave it alone (as it
//! already does for every `departure` field but the wheel's bindings).
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
    /// id's WinQuake engine: every departure off. (The controls are the
    /// player's, the same as 2026's: [`Settings::id`] is id's own.)
    Classic,
    /// The 2026 profile: the port's departures on, the mouse wheel cycling weapons.
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
    /// layout in *both* profiles (the controls are shared) — and the wheel
    /// in 2026 only, the one control that stays a departure.
    pub fn bindings(self) -> Bindings {
        let shared = Bindings::default_cfg().with_wasd().with_gamepad();
        match self {
            Profile::Classic => shared,
            Profile::Modern => shared.with_wheel(),
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

    /// `profile`'s engine with id's own 1996 controls (`default.cfg`'s
    /// bindings, [`Cvars::with_id_controls`]): no WASD, no mouse look, no
    /// gamepad, Space does not swim, no Alt+Enter, Always Run off — kept
    /// explicitly rather than inherited from whatever the shared default
    /// becomes. What the oracle harness pins against (`quaketool
    /// play`/`sound` call `Settings::id(Profile::Classic)`, never
    /// `Settings::new`, so `classic_check`/`oracle_move`/`rotate_check`/
    /// `sound_walk` keep comparing id's controls), and the console's
    /// `idcontrols` for a live session.
    pub fn id(profile: Profile) -> Settings {
        Settings { profile, cvars: profile.cvars().with_id_controls(), binds: Bindings::default_cfg() }
    }

    /// Switch to `profile`: every *engine* departure to its default
    /// (`departure` cvars — the controls are not among them, see the
    /// module docs). The wheel is the controls' one exception, a 2026-only
    /// departure of its *binding*, so it alone follows the profile; every
    /// other binding (WASD, the gamepad, any rebind) and id's own settings
    /// are kept.
    pub fn set_profile(&mut self, profile: Profile) {
        let defaults = profile.cvars();
        for c in cvar::CVARS.iter().filter(|c| c.departure) {
            c.set(&mut self.cvars, &c.get(&defaults));
        }
        self.binds = match profile {
            Profile::Classic => std::mem::take(&mut self.binds).without_wheel(),
            Profile::Modern => std::mem::take(&mut self.binds).with_wheel(),
        };
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
    use crate::keys::{BIND_ATTACK, BIND_FORWARD, BIND_LOOKUP, K_MWHEELUP};

    #[test]
    fn the_2026_profile_is_the_default_and_both_share_the_controls() {
        let s = Settings::default();
        assert_eq!(s.profile, Profile::Modern);
        assert!(s.cvars.uncapped && s.cvars.native && s.cvars.always_run());
        assert_eq!(s.cvars.crosshair, crate::render::Crosshair::Cross);
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD));

        // Classic: the engine off, but the controls (the module docs'
        // "the user's call") are the shared 2026 ones by default too.
        let classic = Settings::new(Profile::Classic);
        assert_eq!(classic.cvars, Cvars::classic());
        assert!(!classic.cvars.uncapped && !classic.cvars.native, "the engine: id's");
        assert!(classic.cvars.freelook && classic.cvars.jumpswim && classic.cvars.alt_enter && classic.cvars.always_run());
        assert_eq!(classic.binds.command(b'w'), Some(BIND_FORWARD), "WASD by default in Classic too");
        assert_eq!(classic.binds.get(K_MWHEELUP), None, "except the wheel: still a 2026-only departure");

        // id's own 1996 controls are the one explicit step away.
        let id = Settings::id(Profile::Classic);
        assert_eq!(id.binds, Bindings::default_cfg(), "id's bindings, not the shared WASD/gamepad ones");
        assert_eq!(id.binds.command(b'a'), Some(BIND_LOOKUP));
        assert!(!id.cvars.freelook && !id.cvars.jumpswim && !id.cvars.alt_enter && !id.cvars.always_run());
        assert_eq!(id.cvars.uncapped, classic.cvars.uncapped, "the engine is Classic's regardless");
    }

    #[test]
    fn switching_profile_resets_the_engine_and_keeps_the_controls_and_ids_settings() {
        let mut s = Settings::default(); // 2026
        s.cvars.viewsize = 80.0; // id's own setting
        s.cvars.gamma = 0.8; // id's own setting
        s.cvars.show_fps = true; // an engine departure
        s.cvars.pixel_size = 3; // an engine departure
        s.cvars.freelook = false; // a control, turned off by hand
        s.binds.bind(b'j', BIND_ATTACK); // a rebind, not the wheel

        s.set_profile(Profile::Classic);
        let mut want = Cvars::classic();
        want.viewsize = 80.0;
        want.gamma = 0.8;
        want.freelook = false; // kept, not reset to Classic's (on) default
        assert_eq!(s.cvars, want, "the engine off; id's own settings and the control kept as changed");
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind survives the switch");
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD), "WASD is untouched");
        assert_eq!(s.binds.get(K_MWHEELUP), None, "the wheel turns off with the profile");

        s.set_profile(Profile::Modern);
        assert!(s.cvars.uncapped && !s.cvars.show_fps && s.cvars.pixel_size == 0, "the engine: back to 2026's");
        assert_eq!((s.cvars.viewsize, s.cvars.gamma), (80.0, 0.8));
        assert!(!s.cvars.freelook, "set_profile never touched the control");
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind still survives");
        assert!(s.binds.get(K_MWHEELUP).is_some(), "the wheel is back");
    }

    #[test]
    fn config_cfg_keeps_the_profile_and_what_differs_from_it() {
        let s = Settings::default();
        assert_eq!(s.config_text(), "// generated by quake, do not modify\nprofile \"2026\"\n", "nothing changed");
        let mut s = Settings::new(Profile::Classic);
        s.cvars.viewsize = 110.0;
        s.cvars.show_fps = true;
        s.binds.bind(b'q', BIND_FORWARD); // 'q' is unbound by default; 'w' already is +forward
        s.binds.set(b'z', None);
        assert_eq!(
            s.config_text(),
            "// generated by quake, do not modify\nprofile \"classic\"\nbind \"q\" \"+forward\"\nunbind \"z\"\n\
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
