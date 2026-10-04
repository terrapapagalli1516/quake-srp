//! The host session's settings — id's console variables ([`Cvars`]) and key
//! bindings ([`Bindings`]) as one value — and the port's two presets.
//!
//! **The slop options.** Every departure from id's WinQuake is a setting
//! ([`crate::cvar::CVARS`] marks them [`crate::cvar::Cvar::departure`]): the
//! port's "slop options", after its name (quake-srp, the slop rust port).
//! Two presets set them all at once. They are the project's, fixed: a
//! player applies one and then changes rows of his own settings.
//!
//! - **Classic** ([`Preset::Classic`]): every slop option off. The game is
//!   WinQuake: its frames (the goldens, the C oracle), its game state and
//!   its 72 fps timing (`oracle/classic_check.py` proves it).
//! - **Slop** ([`Preset::Slop`], the default): the best experience of an
//!   idealized software-rendered Quake in 2026 ([`Cvars::slop`]).
//!
//! **The machine picks the numbers.** A few slop options are numbers whose
//! best value depends on the machine: the renderer's threads and the pixel
//! size. A preset's values are built from a [`Machine`] — the facts the
//! host knows at start (a touch screen, the threads it offers), never read
//! again in a session — so a phone starts at 2x on four threads and a
//! desktop at 1x on every one. They are numbers, never "auto": the console
//! and the menus show what the game runs at, and `config.cfg` keeps a
//! number only when the player chose one that differs from his machine's.
//!
//! **Screen size follows the preset only while it is the preset's.** It is
//! id's own cvar (`viewsize`), not a slop option, but the two presets start
//! it apart: Classic at id's 100, slop one step larger at 110, so the HUD
//! takes less of a slop screen. [`Settings::apply_preset`] moves it to the
//! new preset's start if it still equals the old preset's (the player never
//! moved it), and keeps it otherwise, like id's other settings.
//!
//! **The controls are the player's**, not the engine's, so applying a preset
//! leaves them alone, like id's own settings (Brightness, the volumes, the
//! mouse): WASD and the gamepad ([`Bindings::with_wasd`],
//! [`Bindings::with_gamepad`] — [`Preset::bindings`] applies both to
//! *either* preset), mouse look, Space-swims-up, Alt+Enter and Always Run
//! (`freelook`, `cl_jumpswim`, `vid_altenter`, `cl_forwardspeed`/
//! `cl_backspeed` — on by default in [`Cvars::classic`] too, the module
//! docs of [`crate::cvar`] say why). Classic with the original controls is
//! hard to use these days: that was the user's call (2026-10-03); id's own
//! 1996 controls are one explicit step away, never a preset:
//! [`Settings::id`], and the console's `idcontrols`. The one exception is
//! the mouse wheel's weapon cycle ([`Bindings::with_wheel`]): the user
//! wanted that to stay a slop option, so [`Settings::apply_preset`] still
//! turns it on and off with the preset.
//!
//! A new *engine* departure takes a `departure` slot here, as smooth
//! monster movement (`r_lerpmove`), the slop mixer (`snd_modern`) and the
//! touch controls (`in_touch`) did; a new *control* takes one too, but
//! [`Settings::apply_preset`] must also learn to leave it alone (as it
//! already does for every `departure` field but the wheel's bindings).
//!
//! **Persistence, the id way.** [`Settings::config_text`] is
//! `Host_WriteConfiguration`'s `config.cfg`: `bind` lines and archived cvars,
//! exec'd at the next start as `quake.rc` does. It keeps what the player
//! changed and nothing else — a `preset` line, then only the bindings and
//! archived cvars that differ from that preset's values — so a returning
//! player gets whatever the preset's values are by then (a slop default
//! improved or added in a later version reaches them) while every setting
//! they changed stays as they left it. (id's file lists every value: id's
//! defaults never changed after release.)

use crate::cvar::{self, Cvars};
use crate::keys::Bindings;

/// What the host knows of the machine at start, and the presets' numbers
/// built from it ([`Preset::cvars`]). Read once, when the session starts:
/// a window resized or a phone turned does not change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    /// The primary pointer is coarse (`(pointer: coarse)`, the page's own
    /// test for its touch controls): a phone or a tablet.
    pub touch: bool,
    /// The threads the host offers the renderer (`-hwthreads`, at least 1).
    pub threads: usize,
}

impl Default for Machine {
    /// A plain machine: no touch screen, one thread.
    fn default() -> Self {
        Machine { touch: false, threads: 1 }
    }
}

impl Machine {
    /// The most threads a touch screen starts the renderer on. A phone's
    /// cores are several times slower than a desktop's and slow further as
    /// it warms; on an Android phone (8 threads offered), three
    /// draw worse than four and five or six no better (`fleet/opt-phone`'s
    /// measurements, 2026-10-03).
    pub const TOUCH_THREADS: usize = 4;

    /// The pixel size a touch screen starts at: on an Android phone at 1x (2640x1080)
    /// a frame took 13 ms dry and 19 ms underwater against 60 Hz's 16.7,
    /// throttled at 40 C; at 2x, 8 and 9 ms. A game pixel at 2x is still
    /// under a CSS pixel there, finer than the eye resolves at arm's length.
    pub const TOUCH_PIXEL_SIZE: u8 = 2;

    /// The renderer's threads to start at (`r_threads`): every thread
    /// offered, or at most [`Machine::TOUCH_THREADS`] on a touch screen.
    pub fn render_threads(self) -> usize {
        let all = self.threads.max(1);
        if self.touch { all.min(Machine::TOUCH_THREADS) } else { all }
    }

    /// The pixel size to start at (`vid_pixelsize`): 1, or
    /// [`Machine::TOUCH_PIXEL_SIZE`] on a touch screen.
    pub fn pixel_size(self) -> u8 {
        if self.touch { Machine::TOUCH_PIXEL_SIZE } else { 1 }
    }
}

/// The two presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// id's WinQuake engine: every slop option off. (The controls are the
    /// player's, the same as slop's: [`Settings::id`] is id's own.)
    Classic,
    /// The port's: its slop options on, the mouse wheel cycling weapons.
    #[default]
    Slop,
}

impl Preset {
    /// Its name, as the console, the menu and `config.cfg` print it.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Classic => "classic",
            Preset::Slop => "slop",
        }
    }

    /// The preset a console argument names, in any case: `slop` or
    /// `classic`, or a word an older version used for one (`2026`,
    /// `modern`, `1`; `id`, `0`), so its `config.cfg` and the address's
    /// `?2026` still name it.
    pub fn parse(s: &str) -> Option<Preset> {
        match s.trim().to_ascii_lowercase().as_str() {
            "classic" | "id" | "0" => Some(Preset::Classic),
            "slop" | "2026" | "modern" | "1" => Some(Preset::Slop),
            _ => None,
        }
    }

    /// The other one.
    pub fn toggled(self) -> Preset {
        match self {
            Preset::Classic => Preset::Slop,
            Preset::Slop => Preset::Classic,
        }
    }

    /// The preset's cvars on `machine`: [`Cvars::classic`] or
    /// [`Cvars::slop`], with the numbers the machine picks — the renderer's
    /// threads and the pixel size, the same in both presets (Classic's
    /// picture is a video mode, so its pixel size waits for `vid_native`).
    pub fn cvars(self, machine: Machine) -> Cvars {
        let values = match self {
            Preset::Classic => Cvars::classic(),
            Preset::Slop => Cvars::slop(),
        };
        Cvars { threads: machine.render_threads(), pixel_size: machine.pixel_size(), ..values }
    }

    /// The preset's Screen size (`viewsize`): `default.cfg`'s 100 in
    /// Classic, one step larger in slop. What a new session starts at, what
    /// Options > "Reset to defaults" sets, and what the tools that draw the
    /// slop frame use ([`Cvars::slop`]).
    pub fn viewsize(self) -> f32 {
        self.cvars(Machine::default()).viewsize
    }

    /// The preset's bindings: `default.cfg`, with WASD and the gamepad
    /// layout in *both* presets (the controls are shared) — and the wheel
    /// in slop only, the one control that stays a slop option.
    pub fn bindings(self) -> Bindings {
        let shared = Bindings::default_cfg().with_wasd().with_gamepad();
        match self {
            Preset::Classic => shared,
            Preset::Slop => shared.with_wheel(),
        }
    }
}

/// The session's settings: the preset they were last set from, the machine
/// whose numbers it has, the cvars, the key bindings. The host owns one and
/// hands it by reference to the menu (which edits it), the input and the
/// frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The preset applied last (`config.cfg`'s base).
    pub preset: Preset,
    /// The machine the presets' numbers are built for.
    pub machine: Machine,
    /// The console variables.
    pub cvars: Cvars,
    /// keys.c's `keybindings`.
    pub binds: Bindings,
}

impl Default for Settings {
    /// The slop preset on a plain machine.
    fn default() -> Self {
        Settings::new(Preset::default(), Machine::default())
    }
}

impl Settings {
    /// `preset`'s values on `machine`.
    pub fn new(preset: Preset, machine: Machine) -> Settings {
        Settings { preset, machine, cvars: preset.cvars(machine), binds: preset.bindings() }
    }

    /// `preset`'s engine with id's own 1996 controls (`default.cfg`'s
    /// bindings, [`Cvars::with_id_controls`]): no WASD, no mouse look, no
    /// gamepad, Space does not swim, no Alt+Enter, Always Run off — kept
    /// explicitly rather than inherited from whatever the shared default
    /// becomes. What the oracle harness pins against (`quaketool
    /// play`/`sound` call `Settings::id(Preset::Classic)`, never
    /// `Settings::new`, so `classic_check`/`oracle_move`/`rotate_check`/
    /// `sound_walk` keep comparing id's controls), and the console's
    /// `idcontrols` for a live session. On a plain machine: the harness
    /// gives the renderer its own thread count.
    pub fn id(preset: Preset) -> Settings {
        let machine = Machine::default();
        Settings { preset, machine, cvars: preset.cvars(machine).with_id_controls(), binds: Bindings::default_cfg() }
    }

    /// Apply `preset`: every *engine* slop option to its value
    /// (`departure` cvars — the controls are not among them, see the module
    /// docs). The wheel is the controls' one exception, a slop option of
    /// its *binding*, so it alone follows the preset; every other binding
    /// (WASD, the gamepad, any rebind) and id's own settings are kept — but
    /// for Screen size, which takes the new preset's start if it still is
    /// the old preset's (never moved by the player), so a visitor who has
    /// only applied Classic gets id's inventory bar back, and a player who
    /// chose a size keeps it.
    pub fn apply_preset(&mut self, preset: Preset) {
        let values = preset.cvars(self.machine);
        if self.cvars.viewsize == self.preset.viewsize() {
            self.cvars.viewsize = values.viewsize;
        }
        for c in cvar::CVARS.iter().filter(|c| c.departure) {
            c.set(&mut self.cvars, &c.get(&values));
        }
        self.binds = match preset {
            Preset::Classic => std::mem::take(&mut self.binds).without_wheel(),
            Preset::Slop => std::mem::take(&mut self.binds).with_wheel(),
        };
        self.preset = preset;
    }

    /// Options > "Reset to defaults", `exec default.cfg`: `unbindall`, the
    /// preset's bindings, and `default.cfg`'s four cvars — `viewsize`
    /// ([`Preset::viewsize`]: id's 100 in Classic, slop's own start in
    /// slop), `gamma 1.0`, `volume 0.7`, `sensitivity 3`. Nothing else: the
    /// rest of the Options (Always Run, Invert Mouse, the look toggles, CD
    /// volume), the video mode and the slop options keep their values, as
    /// in WinQuake.
    pub fn reset_defaults(&mut self) {
        self.binds = self.preset.bindings();
        let c = &mut self.cvars;
        c.viewsize = self.preset.viewsize();
        c.gamma = 1.0;
        c.volume = 0.7;
        c.sensitivity = 3.0;
    }

    /// `Host_WriteConfiguration`'s `config.cfg` (`Key_WriteBindings` +
    /// `Cvar_WriteVariables`): the preset, then the bindings and archived
    /// cvars that differ from its values (see the module docs for why only
    /// those).
    pub fn config_text(&self) -> String {
        let mut t = String::from("// generated by quake, do not modify\n");
        t.push_str(&format!("preset \"{}\"\n", self.preset.name()));
        self.binds.write_changes(&self.preset.bindings(), &mut t);
        cvar::write_changes(&self.cvars, &self.preset.cvars(self.machine), &mut t);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{BIND_ATTACK, BIND_FORWARD, BIND_LOOKUP, K_MWHEELUP};

    #[test]
    fn the_slop_preset_is_the_default_and_both_share_the_controls() {
        let s = Settings::default();
        assert_eq!(s.preset, Preset::Slop);
        assert!(s.cvars.uncapped && s.cvars.native && s.cvars.always_run());
        assert_eq!(s.cvars.crosshair, crate::render::Crosshair::Cross);
        assert_eq!(s.cvars.viewsize, 110.0, "slop starts Screen size one step past id's 100");
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD));

        // Classic: the engine off, but the controls (the module docs'
        // "the user's call") are the shared slop ones by default too.
        let classic = Settings::new(Preset::Classic, Machine::default());
        assert_eq!(classic.cvars, Cvars::classic());
        assert_eq!(classic.cvars.viewsize, 100.0, "Classic: default.cfg's, with id's inventory bar");
        assert!(!classic.cvars.uncapped && !classic.cvars.native, "the engine: id's");
        assert!(classic.cvars.freelook && classic.cvars.jumpswim && classic.cvars.alt_enter && classic.cvars.always_run());
        assert_eq!(classic.binds.command(b'w'), Some(BIND_FORWARD), "WASD by default in Classic too");
        assert_eq!(classic.binds.get(K_MWHEELUP), None, "except the wheel: still a slop option");

        // id's own 1996 controls are the one explicit step away.
        let id = Settings::id(Preset::Classic);
        assert_eq!(id.binds, Bindings::default_cfg(), "id's bindings, not the shared WASD/gamepad ones");
        assert_eq!(id.binds.command(b'a'), Some(BIND_LOOKUP));
        assert!(!id.cvars.freelook && !id.cvars.jumpswim && !id.cvars.alt_enter && !id.cvars.always_run());
        assert_eq!(id.cvars.uncapped, classic.cvars.uncapped, "the engine is Classic's regardless");
    }

    #[test]
    fn a_preset_resets_the_engine_and_keeps_the_controls_and_ids_settings() {
        let mut s = Settings::default(); // slop
        s.cvars.viewsize = 80.0; // id's own setting
        s.cvars.gamma = 0.8; // id's own setting
        s.cvars.show_fps = true; // an engine slop option
        s.cvars.pixel_size = 3; // an engine slop option
        s.cvars.freelook = false; // a control, turned off by hand
        s.binds.bind(b'j', BIND_ATTACK); // a rebind, not the wheel

        s.apply_preset(Preset::Classic);
        let mut want = Cvars::classic();
        want.viewsize = 80.0;
        want.gamma = 0.8;
        want.freelook = false; // kept, not reset to Classic's (on) default
        assert_eq!(s.cvars, want, "the engine off; id's own settings and the control kept as changed");
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind survives the preset");
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD), "WASD is untouched");
        assert_eq!(s.binds.get(K_MWHEELUP), None, "the wheel turns off with the preset");

        s.apply_preset(Preset::Slop);
        assert!(s.cvars.uncapped && !s.cvars.show_fps && s.cvars.pixel_size == 1, "the engine: back to slop's");
        assert_eq!((s.cvars.viewsize, s.cvars.gamma), (80.0, 0.8));
        assert!(!s.cvars.freelook, "apply_preset never touched the control");
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind still survives");
        assert!(s.binds.get(K_MWHEELUP).is_some(), "the wheel is back");
    }

    /// Screen size is id's own setting, but the presets start it apart (110
    /// and 100): a preset moves it only while it still is the old preset's
    /// start, in both directions.
    #[test]
    fn screen_size_follows_the_preset_only_while_the_player_has_not_moved_it() {
        // Never moved: the visitor who only applies presets gets each one's own.
        let mut s = Settings::default();
        assert_eq!(s.cvars.viewsize, 110.0);
        s.apply_preset(Preset::Classic);
        assert_eq!(s.cvars, Cvars::classic(), "Classic is id's, inventory bar included");
        assert_eq!(s.cvars.viewsize, 100.0);
        s.apply_preset(Preset::Slop);
        assert_eq!(s.cvars.viewsize, 110.0, "and back");
        s.apply_preset(Preset::Slop);
        assert_eq!(s.cvars.viewsize, 110.0, "the preset it is already on changes nothing");

        // Moved in slop (Screen size 80, or the Options slider's 90): kept both ways.
        for moved in [80.0, 90.0, 120.0] {
            let mut s = Settings::default();
            s.cvars.viewsize = moved;
            s.apply_preset(Preset::Classic);
            assert_eq!(s.cvars.viewsize, moved, "slop -> Classic keeps a chosen size");
            s.apply_preset(Preset::Slop);
            assert_eq!(s.cvars.viewsize, moved, "and back");
        }

        // Moved in Classic: kept going the other way, too.
        let mut s = Settings::new(Preset::Classic, Machine::default());
        s.cvars.viewsize = 70.0;
        s.apply_preset(Preset::Slop);
        assert_eq!(s.cvars.viewsize, 70.0, "Classic -> slop keeps a chosen size");
        s.apply_preset(Preset::Classic);
        assert_eq!(s.cvars.viewsize, 70.0);

        // The one case the value cannot tell from "never moved": a player
        // who chose the *other* preset's start (here 100 in slop) is, to the
        // next preset, a player who never moved it. An accepted gap; the
        // alternative is a flag that `config.cfg` would have to keep.
        let mut s = Settings::default();
        s.cvars.viewsize = 100.0;
        s.apply_preset(Preset::Classic);
        assert_eq!(s.cvars.viewsize, 100.0, "it is Classic's start anyway");
        s.apply_preset(Preset::Slop);
        assert_eq!(s.cvars.viewsize, 110.0, "taken for unmoved: slop's start again");
    }

    #[test]
    fn config_cfg_keeps_the_preset_and_what_differs_from_it() {
        let s = Settings::default();
        assert_eq!(s.config_text(), "// generated by quake, do not modify\npreset \"slop\"\n", "nothing changed");
        // Slop's own 110 is not written, so a player who never touched
        // Screen size gets whatever the preset starts at by then; 100 is a
        // choice.
        let mut t = Settings::default();
        t.cvars.viewsize = 100.0;
        assert_eq!(t.config_text(), "// generated by quake, do not modify\npreset \"slop\"\nviewsize \"100\"\n");
        let mut t = Settings::new(Preset::Classic, Machine::default());
        assert_eq!(t.config_text(), "// generated by quake, do not modify\npreset \"classic\"\n", "Classic's 100 is not written");
        t.cvars.viewsize = 110.0;
        assert_eq!(t.config_text(), "// generated by quake, do not modify\npreset \"classic\"\nviewsize \"110\"\n");
        let mut s = Settings::new(Preset::Classic, Machine::default());
        s.cvars.viewsize = 110.0;
        s.cvars.show_fps = true;
        s.binds.bind(b'q', BIND_FORWARD); // 'q' is unbound by default; 'w' already is +forward
        s.binds.set(b'z', None);
        assert_eq!(
            s.config_text(),
            "// generated by quake, do not modify\npreset \"classic\"\nbind \"q\" \"+forward\"\nunbind \"z\"\n\
             viewsize \"110\"\nwasm_showfps \"1\"\n"
        );
    }

    #[test]
    fn reset_defaults_is_the_presets_default_cfg() {
        let mut s = Settings::default();
        s.cvars.viewsize = 50.0;
        s.cvars.bgmvolume = 0.3;
        s.cvars.set_always_run(false);
        s.binds.unbind_all();
        s.reset_defaults();
        assert_eq!(s.cvars.viewsize, 110.0, "slop's own start");
        assert_eq!(s.cvars.bgmvolume, 0.3, "not in default.cfg");
        assert!(!s.cvars.always_run(), "not in default.cfg");
        assert_eq!(s.binds, Preset::Slop.bindings());
        // Classic: default.cfg's viewsize 100, as in WinQuake.
        let mut c = Settings::new(Preset::Classic, Machine::default());
        c.cvars.viewsize = 50.0;
        c.reset_defaults();
        assert_eq!(c.cvars.viewsize, 100.0);
        // ...and a reset leaves the preset as it was.
        assert_eq!((c.preset, s.preset), (Preset::Classic, Preset::Slop));
    }

    /// The numbers a preset takes from the machine: a touch screen starts at
    /// 2x on at most four threads, anything else at 1x on every thread, in
    /// either preset; and `config.cfg` writes a number only when it differs
    /// from this machine's, so the same 2x is a choice on a desktop and
    /// nothing at all on a phone.
    #[test]
    fn the_machine_picks_the_presets_numbers() {
        let phone = Machine { touch: true, threads: 8 };
        let desktop = Machine { touch: false, threads: 16 };
        for preset in [Preset::Slop, Preset::Classic] {
            let c = preset.cvars(phone);
            assert_eq!((c.pixel_size, c.threads), (2, 4), "{preset:?} on a phone");
            let c = preset.cvars(desktop);
            assert_eq!((c.pixel_size, c.threads), (1, 16), "{preset:?} on a desktop");
        }
        assert_eq!(Machine { touch: true, threads: 2 }.render_threads(), 2, "four at most, not four at least");
        assert_eq!(Machine { touch: false, threads: 0 }.render_threads(), 1, "always one");
        assert_eq!(Preset::Slop.cvars(Machine::default()), Cvars::slop(), "a plain machine: one thread, 1x");
        assert_eq!(Preset::Classic.cvars(Machine::default()), Cvars::classic());

        let head = "// generated by quake, do not modify\npreset \"slop\"\n";
        let on_phone = Settings::new(Preset::Slop, phone);
        assert_eq!(on_phone.config_text(), head, "the phone's own numbers are not written");
        let mut at_2x = Settings::new(Preset::Slop, desktop);
        at_2x.cvars.pixel_size = 2;
        assert_eq!(at_2x.config_text(), format!("{head}vid_pixelsize \"2\"\n"), "2x is a choice on a desktop");
        let mut at_1x = on_phone.clone();
        at_1x.cvars.pixel_size = 1;
        at_1x.cvars.threads = 8;
        assert_eq!(at_1x.config_text(), format!("{head}vid_pixelsize \"1\"\nr_threads \"8\"\n"), "and 1x on eight a choice on a phone");

        // Applying a preset keeps the machine's numbers where they are
        // slop options, and never touches the threads (no slop option).
        let mut s = Settings::new(Preset::Slop, phone);
        s.cvars.threads = 6;
        s.apply_preset(Preset::Classic);
        assert_eq!((s.cvars.pixel_size, s.cvars.threads, s.machine), (2, 6, phone));
    }

    /// The console's and `config.cfg`'s words for a preset: its name, and
    /// the words an older version wrote for the same one.
    #[test]
    fn a_preset_is_named_by_its_name_or_an_older_word() {
        for (word, preset) in [("slop", Preset::Slop), ("Classic", Preset::Classic), ("2026", Preset::Slop),
                               ("modern", Preset::Slop), ("id", Preset::Classic), ("1", Preset::Slop), ("0", Preset::Classic)] {
            assert_eq!(Preset::parse(word), Some(preset), "{word}");
        }
        assert_eq!(Preset::parse("x"), None);
        assert_eq!((Preset::Slop.name(), Preset::Classic.name()), ("slop", "classic"));
    }
}
