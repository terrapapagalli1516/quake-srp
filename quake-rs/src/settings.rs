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
//! **The two actions.** Options > "Reset to Classic" and "Reset to slop"
//! ([`Settings::reset`]) set everything to that preset on this machine:
//! every slop option, the key bindings, id's own Options (Screen size,
//! Brightness, the volumes, the mouse, Always Run, the look toggles), the
//! video mode, the name and colours — as a first session in that preset
//! has them. Applying a preset ([`Settings::apply_preset`]: the console's
//! `preset`, the address's `?classic` and `?slop`) is the gentler one: it
//! sets every slop option and leaves the rest as the player has it — but
//! for the wheel's weapon cycle, a slop option of a *binding*
//! ([`Bindings::with_wheel`]), and Screen size, above.
//!
//! **The controls are shared.** Both presets have the same ones: WASD and
//! the gamepad ([`Bindings::with_wasd`], [`Bindings::with_gamepad`] —
//! [`Preset::bindings`] applies both to *either* preset), mouse look,
//! Space-swims-up, Alt+Enter, the touch controls and Always Run (the module
//! docs of [`crate::cvar`] say which are slop options). Classic with the
//! original controls is hard to use these days: that was the user's call
//! (2026-10-03); id's own 1996 controls are one explicit step away, never a
//! preset: [`Settings::id`], and the console's `idcontrols`.
//!
//! **Where the settings stand** ([`Settings::standing`]): every slop option
//! (and the wheel) against the preset applied last — the preset, or "yours
//! differ in N rows". The console says it (`preset`, `version`); the menus
//! do not, nor mark a setting that differs from a preset (the user's call,
//! 2026-10-04: no indicator of a change from the default).

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
    /// The most threads a touch screen starts the renderer on. The browser
    /// offers every core it sees, but a phone's are of two or three kinds
    /// (four or five fast ones on any current phone), the page's own thread,
    /// the compositor, the GPU process and the sound need some of them each
    /// frame, and every busy core is heat, which a phone pays back in clock.
    /// Measured on an Android phone (one fast core, four middle, three small;
    /// 8 offered), warmed up: four threads against eight showed
    /// 70-74 frames a second against 67 at 2640x1080 with a third of the late
    /// ones, and at 1320x540 took the frame's 99th percentile from 20.7 ms to
    /// 9.5 (a band's thread put off its core); three were worse, five and six
    /// no better (web/PLATFORM.md, "On an Android phone").
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

/// The two presets: the project's, fixed.
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

    /// Its name in a sentence: "slop", "Classic" (the menus' lines).
    pub fn title(self) -> &'static str {
        match self {
            Preset::Classic => "Classic",
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

    /// The preset's cvars on `machine`: [`Cvars::classic`] or
    /// [`Cvars::slop`], with the numbers the machine picks — the renderer's
    /// threads and the pixel size, the same in both presets (Classic's
    /// picture is a video mode, so its pixel size waits for `vid_native`).
    /// The frame-rate cap is the preset's on any machine: id's 72 in
    /// Classic, none in slop (a touch screen too, since 2026-10-04).
    pub fn cvars(self, machine: Machine) -> Cvars {
        let values = match self {
            Preset::Classic => Cvars::classic(),
            Preset::Slop => Cvars::slop(),
        };
        Cvars { threads: machine.render_threads(), pixel_size: machine.pixel_size(), ..values }
    }

    /// The preset's Screen size (`viewsize`): `default.cfg`'s 100 in
    /// Classic, one step larger in slop. What a new session starts at, what
    /// Options > "Reset to slop" sets, and what the tools that draw the slop
    /// frame use ([`Cvars::slop`]).
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

    /// Apply `preset`: every slop option to its value on this machine (the
    /// `departure` cvars, and the wheel's binding), keeping the player's
    /// key bindings and id's own settings — but for Screen size, which
    /// takes the new preset's start if it still is the old preset's (never
    /// moved by the player), so a visitor who has only applied Classic gets
    /// id's inventory bar back, and a player who chose a size keeps it.
    /// What Options > "Reset to Classic" does, and the console's `preset`.
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

    /// Options > "Reset to slop" and "Reset to Classic": everything to
    /// `preset` on this machine — every slop option, the key bindings, id's
    /// own Options, the video mode, the name and colours — as a first
    /// session in that preset has them. Nothing is deleted: the next
    /// `config.cfg` written is the preset's one line, and the player's other
    /// files (the saves, his own paks and CD tracks) are the host's,
    /// untouched.
    pub fn reset(&mut self, preset: Preset) {
        *self = Settings::new(preset, self.machine);
    }

    /// `Cvar_Set` for the session: `value` into `c` — but 0 for a number the
    /// machine picks ([`cvar::Cvar::machine_picked`]: `vid_pixelsize`,
    /// `r_threads`) is this machine's number, as the presets give it, so a
    /// file or a player that says 0 gets what this machine starts at (the
    /// console then prints the number) and `config.cfg` writes nothing for it.
    /// (Read as 1, a 0 an older `config.cfg` kept from the days of Auto
    /// pinned a phone to 1x and a desktop to one thread.)
    pub fn set_cvar(&mut self, c: &cvar::Cvar, value: &str) {
        if c.machine_picked() && cvar::atof(value) == 0.0 {
            c.set(&mut self.cvars, &c.get(&self.preset.cvars(self.machine)));
        } else {
            c.set(&mut self.cvars, value);
        }
    }

    /// Where the settings stand against the preset applied last: the slop
    /// options (by console name; `bind` for the wheel's binding) whose
    /// value differs from the preset's on this machine. Screen size is
    /// id's own, and the presets start it apart, so it is not counted.
    pub fn standing(&self) -> Standing {
        let values = self.preset.cvars(self.machine);
        let mut changed: Vec<&'static str> = cvar::CVARS
            .iter()
            .filter(|c| c.departure && c.get(&self.cvars) != c.get(&values))
            .map(|c| c.name)
            .collect();
        if self.binds.wheel() != self.preset.bindings().wheel() {
            changed.push("bind");
        }
        Standing { preset: self.preset, changed }
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

/// Where the settings stand against the preset applied last
/// ([`Settings::standing`]): what the menus and the console say of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// The preset applied last.
    pub preset: Preset,
    /// The slop options that differ from it, by console name (`bind` for
    /// the wheel's binding), in [`crate::cvar::CVARS`]' order.
    pub changed: Vec<&'static str>,
}

impl Standing {
    /// Whether nothing differs: the settings are the preset.
    pub fn is_preset(&self) -> bool {
        self.changed.is_empty()
    }

    /// The console's line for it (`preset`): "Your settings are the slop
    /// preset", or "Yours differ from Classic in:", the names
    /// ([`Standing::changed`]) following — each under a 320-wide console's
    /// 38 columns.
    pub fn line(&self) -> String {
        if self.is_preset() {
            format!("Your settings are the {} preset", self.preset.title())
        } else {
            format!("Yours differ from {} in:", self.preset.title())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::host::FrameCap;
    use crate::keys::{BIND_ATTACK, BIND_FORWARD, BIND_LOOKUP, K_MWHEELUP};

    #[test]
    fn the_slop_preset_is_the_default_and_both_share_the_controls() {
        let s = Settings::default();
        assert_eq!(s.preset, Preset::Slop);
        assert!(s.cvars.max_fps == FrameCap::NONE && s.cvars.native && s.cvars.always_run());
        assert_eq!(s.cvars.crosshair, crate::render::Crosshair::Cross);
        assert_eq!(s.cvars.viewsize, 110.0, "slop starts Screen size one step past id's 100");
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD));

        // Classic: the engine off, but the controls (the module docs'
        // "the user's call") are the shared slop ones by default too.
        let classic = Settings::new(Preset::Classic, Machine::default());
        assert_eq!(classic.cvars, Cvars::classic());
        assert_eq!(classic.cvars.viewsize, 100.0, "Classic: default.cfg's, with id's inventory bar");
        assert!(classic.cvars.max_fps == FrameCap::ID && !classic.cvars.native, "the engine: id's");
        assert!(
            classic.cvars.freelook && classic.cvars.jumpswim && classic.cvars.alt_enter && classic.cvars.always_run()
        );
        assert_eq!(classic.binds.command(b'w'), Some(BIND_FORWARD), "WASD by default in Classic too");
        assert_eq!(classic.binds.get(K_MWHEELUP), None, "except the wheel: still a slop option");

        // id's own 1996 controls are the one explicit step away.
        let id = Settings::id(Preset::Classic);
        assert_eq!(id.binds, Bindings::default_cfg(), "id's bindings, not the shared WASD/gamepad ones");
        assert_eq!(id.binds.command(b'a'), Some(BIND_LOOKUP));
        assert!(!id.cvars.freelook && !id.cvars.jumpswim && !id.cvars.alt_enter && !id.cvars.always_run());
        assert_eq!(id.cvars.max_fps, classic.cvars.max_fps, "the engine is Classic's regardless");
    }

    /// Reset to Classic (applying a preset): every slop option to the
    /// preset's, the controls on the Slop Options pages included; the
    /// player's keys and id's own Options kept, the wheel's binding the
    /// preset's.
    #[test]
    fn a_preset_sets_every_slop_option_and_keeps_the_keys_and_ids_settings() {
        let mut s = Settings::default(); // slop
        s.cvars.viewsize = 80.0; // id's own setting
        s.cvars.gamma = 0.8; // id's own setting
        s.cvars.set_always_run(false); // id's own Options row
        s.cvars.show_fps = true; // a slop option
        s.cvars.pixel_size = 3; // a slop option
        s.cvars.freelook = false; // a slop option that is a control
        s.cvars.joy.deadzone = 0.5; // the pad's layout, on the console
        s.binds.bind(b'j', BIND_ATTACK); // a rebind, not the wheel

        s.apply_preset(Preset::Classic);
        let mut want = Cvars::classic();
        want.viewsize = 80.0;
        want.gamma = 0.8;
        want.set_always_run(false);
        assert_eq!(s.cvars, want, "every slop option Classic's; id's own settings kept as changed");
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind survives the preset");
        assert_eq!(s.binds.command(b'w'), Some(BIND_FORWARD), "WASD is untouched");
        assert_eq!(s.binds.get(K_MWHEELUP), None, "the wheel turns off with the preset");

        s.apply_preset(Preset::Slop);
        assert!(
            s.cvars.max_fps == FrameCap::NONE && !s.cvars.show_fps && s.cvars.pixel_size == 1 && s.cvars.freelook,
            "slop's"
        );
        assert_eq!((s.cvars.viewsize, s.cvars.gamma, s.cvars.always_run()), (80.0, 0.8, false));
        assert_eq!(s.binds.command(b'j'), Some(BIND_ATTACK), "the rebind still survives");
        assert!(s.binds.get(K_MWHEELUP).is_some(), "the wheel is back");
    }

    /// Reset to slop and Reset to Classic: everything, keys and id's
    /// Options too (Screen size, Brightness, the name), the video mode,
    /// every slop option, as a first session in that preset on this machine
    /// has them; `config.cfg` is the preset's one line again.
    #[test]
    fn a_reset_is_its_preset_whole() {
        let phone = Machine { touch: true, threads: 8 };
        for (from, to) in
            [(Preset::Classic, Preset::Slop), (Preset::Slop, Preset::Classic), (Preset::Slop, Preset::Slop)]
        {
            let mut s = Settings::new(from, phone);
            s.cvars.viewsize = 50.0;
            s.cvars.gamma = 0.6;
            s.cvars.set_name("ranger");
            s.cvars.vid_resolution = (640, 400);
            s.cvars.threads = 2;
            s.cvars.show_fps = true;
            s.binds.unbind_all();
            s.reset(to);
            assert_eq!(s, Settings::new(to, phone), "{from:?} -> {to:?}");
            assert_eq!(s.config_text(), format!("// generated by quake, do not modify\npreset \"{}\"\n", to.name()));
        }
    }

    /// The standing: the preset while every slop option is its, else the
    /// slop options that differ, by name — none for Screen size or id's
    /// Options. The console's `preset` says it; no menu does.
    #[test]
    fn the_standing_names_the_slop_options_that_differ_from_the_preset() {
        let mut s = Settings::default();
        let st = s.standing();
        assert!(st.is_preset());
        assert_eq!(st.line(), "Your settings are the slop preset");
        s.cvars.show_fps = true;
        let st = s.standing();
        assert_eq!((st.changed.as_slice(), st.line().as_str()), (&["wasm_showfps"][..], "Yours differ from slop in:"));
        s.cvars.show_fps = false;
        assert!(s.standing().is_preset(), "and back: the preset again");

        // Not slop options: Screen size, id's Options, the threads.
        s.cvars.viewsize = 80.0;
        s.cvars.gamma = 0.7;
        s.cvars.set_always_run(false);
        s.cvars.threads = 3;
        assert!(s.standing().is_preset());

        // Every one by its console name, the wheel as `bind`.
        s.cvars.native = false;
        s.cvars.pixel_size = 3;
        s.cvars.joy.enabled = false;
        s.cvars.joy.deadzone = 0.1;
        s.binds = std::mem::take(&mut s.binds).without_wheel();
        s.cvars.max_edicts = 600;
        let st = s.standing();
        assert_eq!(st.changed, ["joystick", "vid_native", "vid_pixelsize", "sv_max_edicts", "joy_deadzone", "bind"]);

        // Classic's words.
        let mut c = Settings::new(Preset::Classic, Machine::default());
        assert_eq!(c.standing().line(), "Your settings are the Classic preset");
        c.cvars.show_fps = true;
        assert_eq!(c.standing().line(), "Yours differ from Classic in:");
        assert!(
            Settings::new(Preset::Classic, Machine::default()).standing().line().len() <= 38,
            "a 320-wide console's line"
        );
    }

    /// A Classic `config.cfg` lists no slop option: its `preset "classic"`
    /// line turns them all off, so one a later version adds is off too.
    #[test]
    fn a_classic_file_with_no_line_for_a_slop_option_keeps_it_off() {
        let mut s = Settings::default(); // what a session starts with, before config.cfg
        s.apply_preset(Preset::Classic); // the file's first line
        assert_eq!(s, Settings::new(Preset::Classic, Machine::default()), "every slop option, the new ones too");
        assert!(s.standing().is_preset());
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
        assert_eq!(
            t.config_text(),
            "// generated by quake, do not modify\npreset \"classic\"\n",
            "Classic's 100 is not written"
        );
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

    /// The numbers a preset takes from the machine: a touch screen starts at
    /// 2x on at most four threads and slop's 60 fps cap, anything else at
    /// 1x on every thread and no cap; Classic's cap is id's 72 on either.
    /// `config.cfg` writes a number only when it differs from this
    /// machine's, so the same 2x is a choice on a desktop and nothing at all
    /// on a phone.
    /// 0 for a number the machine picks, on the console or in a file, is
    /// this machine's number: a phone's 2x and four threads, a desktop's 1x
    /// and every thread — printed as the number, and not written to
    /// `config.cfg` (a 0 an old file kept no longer pins 1x or one thread).
    #[test]
    fn zero_is_the_machines_number() {
        let pixel = cvar::find("vid_pixelsize").expect("the cvar");
        let threads = cvar::find("r_threads").expect("the cvar");
        for (machine, want) in
            [(Machine { touch: true, threads: 8 }, ("2", "4")), (Machine { touch: false, threads: 16 }, ("1", "16"))]
        {
            let mut s = Settings::new(Preset::Slop, machine);
            s.set_cvar(pixel, "3");
            s.set_cvar(threads, "2");
            assert_eq!(
                (pixel.get(&s.cvars), threads.get(&s.cvars)),
                ("3".to_string(), "2".to_string()),
                "a number is that number"
            );
            s.set_cvar(pixel, "0");
            s.set_cvar(threads, "0");
            assert_eq!((pixel.get(&s.cvars).as_str(), threads.get(&s.cvars).as_str()), want, "{machine:?}");
            assert_eq!(s.config_text(), "// generated by quake, do not modify\npreset \"slop\"\n", "nothing written");
        }
        let mut s = Settings::new(Preset::Slop, Machine { touch: false, threads: 16 });
        s.set_cvar(cvar::find("viewsize").expect("the cvar"), "0");
        assert_eq!(s.cvars.viewsize, crate::screen::VIEWSIZE_MIN, "any other cvar's 0 is its own");
    }

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
        assert_eq!(
            (Preset::Slop.cvars(phone).max_fps, Preset::Slop.cvars(desktop).max_fps),
            (FrameCap::NONE, FrameCap::NONE),
            "no cap, a phone too"
        );
        assert_eq!(
            (Preset::Classic.cvars(phone).max_fps, Preset::Classic.cvars(desktop).max_fps),
            (FrameCap::ID, FrameCap::ID)
        );
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
        at_1x.cvars.max_fps = FrameCap::new(60);
        assert_eq!(
            at_1x.config_text(),
            format!("{head}host_maxfps \"60\"\nvid_pixelsize \"1\"\nr_threads \"8\"\n"),
            "and 1x on eight, capped at 60, choices on a phone"
        );

        // Applying a preset sets the machine's numbers where they are slop
        // options, and never touches the threads (no slop option).
        let mut s = Settings::new(Preset::Slop, phone);
        s.cvars.threads = 6;
        s.apply_preset(Preset::Classic);
        assert_eq!((s.cvars.pixel_size, s.cvars.threads, s.cvars.max_fps, s.machine), (2, 6, FrameCap::ID, phone));
    }

    /// The console's and `config.cfg`'s words for a preset: its name, and
    /// the words an older version wrote for the same one.
    #[test]
    fn a_preset_is_named_by_its_name_or_an_older_word() {
        for (word, preset) in [
            ("slop", Preset::Slop),
            ("Classic", Preset::Classic),
            ("2026", Preset::Slop),
            ("modern", Preset::Slop),
            ("id", Preset::Classic),
            ("1", Preset::Slop),
            ("0", Preset::Classic),
        ] {
            assert_eq!(Preset::parse(word), Some(preset), "{word}");
        }
        assert_eq!(Preset::parse("x"), None);
        assert_eq!((Preset::Slop.name(), Preset::Classic.name()), ("slop", "classic"));
    }
}
