//! Console variables — cvar.c's list of `cvar_t`, as one typed value.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cvar.c` (`Cvar_FindVar`, `Cvar_Set`, `Cvar_Command`,
//! `Cvar_CompleteVariable`, `Cvar_WriteVariables`) and the registrations of
//! the cvars below in `view.c`, `screen.c`, `snd_dma.c`, `cl_main.c`,
//! `d_init.c`, `net_main.c`.
//!
//! id keeps each cvar as a named string with its float beside it, registered
//! by the file that reads it and found by name. The port keeps them as the
//! fields of [`Cvars`]: the host session owns one and hands it by reference
//! to whatever reads a setting, so every setting is a typed field in one
//! place. [`CVARS`] is the console's view of them — each field's name, its
//! `archive` flag (`config.cfg` keeps it) and how its value reads and sets —
//! which `Cvar_Command`'s `"viewsize" is "100"`, Tab completion and
//! `Cvar_WriteVariables` go through.
//!
//! Two kinds of field. **id's cvars**, with id's defaults. And **the port's
//! departures** from id's game, each marked [`Cvar::departure`], every one off
//! in [`Cvars::classic`]; [`Cvars::modern`] is the 2026 profile
//! ([`crate::settings::Profile`]). Three of id's own cvars are departures
//! too, where the 2026 profile gives them another default: `cl_forwardspeed`
//! and `cl_backspeed` (Always Run) and `crosshair`.

use crate::screen::{VIEWSIZE_DEFAULT, VIEWSIZE_MAX, VIEWSIZE_MIN, VIEWSIZE_STEP};

/// The port's pixel sizes for [`Cvars::pixel_size`]: 0 is Auto, 1..=4 a
/// fixed size.
pub const PIXEL_SIZE_MAX: u8 = 4;

/// The longest player name: `Host_Name_f` cuts it to 15 characters
/// (`newName[15] = 0`), as Setup's `char setup_myname[16]` holds.
pub const NAME_MAX: usize = 15;

/// Every cvar the port has, by field. [`Default`] is [`Cvars::classic`].
#[derive(Debug, Clone, PartialEq)]
pub struct Cvars {
    // --- id's cvars (default.cfg's four, then the registrations' defaults) ---
    /// `viewsize` (screen.c `scr_viewsize`): the 3-D view's size, 30..=120.
    pub viewsize: f32,
    /// `gamma` (view.c `v_gamma`): Brightness, 0.5..=1 on the slider.
    pub gamma: f32,
    /// `volume` (snd_dma.c): Sound Volume, 0..=1.
    pub volume: f32,
    /// `bgmvolume` (snd_dma.c): CD Music Volume. There is no CD audio, as a
    /// C build with `cd_null.c`: the slider moves a value nothing plays at.
    pub bgmvolume: f32,
    /// `sensitivity` (in_win.c): Mouse Speed, 1..=11 on the slider.
    pub sensitivity: f32,
    /// `cl_forwardspeed` (cl_main.c): the walking forward speed. Options >
    /// Always Run swaps it and [`Cvars::cl_backspeed`] between 200 and 400.
    pub cl_forwardspeed: f32,
    /// `cl_backspeed`.
    pub cl_backspeed: f32,
    /// `m_pitch` (cl_main.c, 0.022): its sign is Invert Mouse. Its size is
    /// not used: the port's mouse scale is calibrated for browser counts
    /// (quake-wasm `input.rs`).
    pub m_pitch: f32,
    /// `lookspring` (cl_main.c): the view re-levels when mouse look ends.
    pub lookspring: bool,
    /// `lookstrafe` (cl_main.c): mouse X strafes while mouse-looking.
    pub lookstrafe: bool,
    /// `crosshair` (view.c): `V_RenderView` draws a `+` at the view's centre.
    /// A departure: on in the 2026 profile.
    pub crosshair: bool,
    /// `_cl_name` (cl_main.c): the player's name.
    pub cl_name: String,
    /// `_cl_color` (cl_main.c): shirt * 16 + pants.
    pub cl_color: i32,
    /// `hostname` (net_main.c).
    pub hostname: String,
    /// `d_mipscale` (d_init.c): scales the mip-level thresholds.
    pub d_mipscale: f32,
    /// `d_mipcap` (d_init.c): the finest mip level allowed.
    pub d_mipcap: f32,
    // --- the port's ---
    /// `_vid_resolution`: the video mode (id archives a mode number,
    /// `_vid_default_mode_win`), shown in a 4:3 box; the Video Options list.
    /// Not used while [`Cvars::native`] is on.
    pub vid_resolution: (u16, u16),
    /// `wasm_uncapped`: no 72 fps cap (`Host_FilterTime`); a host frame on
    /// every display refresh, the game stepped as id's 72 Hz frames
    /// ([`crate::stepping::Stepping::Uncapped`]).
    pub uncapped: bool,
    /// `wasm_showfps`: QuakeWorld's frame-rate readout.
    pub show_fps: bool,
    /// `wasm_exactpersp`: exact perspective at every pixel, not id's
    /// 16-pixel spans.
    pub exact_persp: bool,
    /// `wasm_scaled2d`: the 2-D layer (status bar, menus, console) at the
    /// largest whole multiple of id's 320x200 that fits, where id draws it
    /// 1:1 ([`crate::draw::screen_2d`]).
    pub scaled_2d: bool,
    /// `vid_native`: the picture fills the window at the window's own aspect,
    /// rendered at its device pixels divided by [`Cvars::pixel_size`], with
    /// square pixels and views past id's 1280x1024 (the renderer's `hires`);
    /// off, the video mode [`Cvars::vid_resolution`] in a 4:3 box, as a 1996
    /// monitor showed it.
    pub native: bool,
    /// `vid_pixelsize`: with [`Cvars::native`], how many device pixels make
    /// one of the picture's (0: Auto, the smallest that keeps a frame
    /// affordable; 1..=[`PIXEL_SIZE_MAX`]). Whole pixels either way, never
    /// smoothed.
    pub pixel_size: u8,
    /// `fov_adapt`: Hor+ — `fov` spans a 4:3 screen and a wider one sees more
    /// at the sides ([`crate::render::FovMode::HorPlus`]).
    pub fov_adapt: bool,
    /// `freelook`: mouse look while the pointer is locked, as if `+mlook`
    /// were held (id: off; `\` and MOUSE3 hold it).
    pub freelook: bool,
    /// `cl_jumpswim`: `+jump` also swims up (`upmove`) in water and flight,
    /// on top of QuakeC's own swim-up.
    pub jumpswim: bool,
    /// `vid_fkey`: `f` toggles fullscreen (the page's; `default.cfg` leaves
    /// `f` unbound).
    pub fkey: bool,
}

impl Default for Cvars {
    fn default() -> Self {
        Cvars::classic()
    }
}

impl Cvars {
    /// id's defaults, every departure off: WinQuake.
    pub fn classic() -> Cvars {
        Cvars {
            viewsize: VIEWSIZE_DEFAULT,
            gamma: 1.0,
            volume: 0.7,
            bgmvolume: 1.0,
            sensitivity: 3.0,
            cl_forwardspeed: 200.0,
            cl_backspeed: 200.0,
            m_pitch: 0.022,
            lookspring: false,
            lookstrafe: false,
            crosshair: false,
            cl_name: "player".to_string(),
            cl_color: 0,
            hostname: "UNNAMED".to_string(),
            d_mipscale: 1.0,
            d_mipcap: 0.0,
            vid_resolution: (960, 600),
            uncapped: false,
            show_fps: false,
            exact_persp: false,
            scaled_2d: false,
            native: false,
            pixel_size: 0,
            fov_adapt: false,
            freelook: false,
            jumpswim: false,
            fkey: false,
        }
    }

    /// The 2026 profile's: an idealized software-rendered Quake on a 2026
    /// machine. A frame every display refresh, the window filled at native
    /// resolution in whole chunky pixels with a Hor+ field of view, the 2-D
    /// layer at id's proportions, the crosshair, Always Run, mouse look,
    /// Space to swim up and `f` for fullscreen. Show FPS and exact
    /// perspective stay off: the readout is clutter, and id's 16-pixel spans
    /// are part of the look.
    pub fn modern() -> Cvars {
        Cvars {
            cl_forwardspeed: 400.0,
            cl_backspeed: 400.0,
            crosshair: true,
            uncapped: true,
            scaled_2d: true,
            native: true,
            fov_adapt: true,
            freelook: true,
            jumpswim: true,
            fkey: true,
            ..Cvars::classic()
        }
    }

    /// Options > Always Run: `cl_forwardspeed > 200` (`M_Options_Draw`).
    pub fn always_run(&self) -> bool {
        self.cl_forwardspeed > 200.0
    }

    /// Options > Always Run switched (`M_AdjustSliders` case 8): both speeds
    /// to 400, or back to 200.
    pub fn set_always_run(&mut self, on: bool) {
        let speed = if on { 400.0 } else { 200.0 };
        self.cl_forwardspeed = speed;
        self.cl_backspeed = speed;
    }

    /// Options > Invert Mouse: `m_pitch < 0`.
    pub fn invert_mouse(&self) -> bool {
        self.m_pitch < 0.0
    }

    /// Options > Invert Mouse switched: `m_pitch = -m_pitch`.
    pub fn set_invert_mouse(&mut self, on: bool) {
        if on != self.invert_mouse() {
            self.m_pitch = -self.m_pitch;
        }
    }

    /// `sizeup` (`SCR_SizeUp_f`): `viewsize` + 10, bounded.
    pub fn size_up(&mut self) {
        self.set_viewsize(self.viewsize + VIEWSIZE_STEP);
    }

    /// `sizedown` (`SCR_SizeDown_f`): `viewsize` - 10, bounded.
    pub fn size_down(&mut self) {
        self.set_viewsize(self.viewsize - VIEWSIZE_STEP);
    }

    /// `Host_Name_f`'s client half (`Cvar_Set ("_cl_name", newName)`): the
    /// name, cut to [`NAME_MAX`] characters.
    pub fn set_name(&mut self, name: &str) {
        self.cl_name = name.chars().take(NAME_MAX).collect();
    }

    /// `Host_Color_f`'s client half: each colour `& 15`, at most 13, then
    /// `_cl_color = top*16 + bottom`.
    pub fn set_color(&mut self, top: i32, bottom: i32) {
        let clamp = |c: i32| (c & 15).min(13);
        self.cl_color = clamp(top) * 16 + clamp(bottom);
    }

    /// `viewsize` set, bounded to 30..=120 as `SCR_CalcRefdef` bounds it on
    /// the next frame (a non-number reads as 0, `atof`: the minimum).
    pub fn set_viewsize(&mut self, v: f32) {
        let v = if v.is_finite() { v } else { 0.0 };
        self.viewsize = v.clamp(VIEWSIZE_MIN, VIEWSIZE_MAX);
    }
}

/// One console variable: [`CVARS`]' row for a field of [`Cvars`].
pub struct Cvar {
    /// The console name.
    pub name: &'static str,
    /// `config.cfg` keeps it (id's `archive`).
    pub archive: bool,
    /// A departure from id's game: a profile sets it ([`Cvars::classic`]
    /// has it off).
    pub departure: bool,
    /// One line for the console's list.
    pub help: &'static str,
    get: fn(&Cvars) -> String,
    set: fn(&mut Cvars, &str),
}

impl Cvar {
    /// The value as the console prints it (`var->string`).
    pub fn get(&self, c: &Cvars) -> String {
        (self.get)(c)
    }

    /// `Cvar_Set`: set it from a console argument.
    pub fn set(&self, c: &mut Cvars, value: &str) {
        (self.set)(c, value);
    }
}

impl std::fmt::Debug for Cvar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cvar").field("name", &self.name).field("archive", &self.archive).finish()
    }
}

/// `Q_atof` (common.c) for a cvar's value: the number at the start of `s`,
/// 0 for none.
pub fn atof(s: &str) -> f32 {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')))
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    s[..end].parse().unwrap_or(0.0)
}

/// A number as the console prints a cvar: `%f` with the trailing zeros (and
/// a bare trailing point) trimmed — `100`, `0.7`, `-0.022`. (id prints the
/// string that set it last, whatever its form; the port keeps no strings.)
pub fn number_string(v: f32) -> String {
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn flag(on: bool) -> String {
    if on { "1" } else { "0" }.to_string()
}

fn on(v: &str) -> bool {
    atof(v) != 0.0
}

/// A `WxH` video mode.
fn parse_mode(v: &str) -> Option<(u16, u16)> {
    let (w, h) = v.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The console's cvars, in `cvar_vars` order for completion (id's list
/// finds the one registered last first; the port's own come after id's).
pub const CVARS: &[Cvar] = &[
    Cvar { name: "_cl_color", archive: true, departure: false, help: "shirt*16 + pants colour",
        get: |c| c.cl_color.to_string(), set: |c, v| c.cl_color = atof(v) as i32 },
    Cvar { name: "_cl_name", archive: true, departure: false, help: "the player's name",
        get: |c| c.cl_name.clone(), set: |c, v| c.cl_name = v.to_string() },
    Cvar { name: "cl_forwardspeed", archive: true, departure: true, help: "walk speed (400: Always Run)",
        get: |c| number_string(c.cl_forwardspeed), set: |c, v| c.cl_forwardspeed = atof(v) },
    Cvar { name: "cl_backspeed", archive: true, departure: true, help: "backpedal speed",
        get: |c| number_string(c.cl_backspeed), set: |c, v| c.cl_backspeed = atof(v) },
    Cvar { name: "lookspring", archive: true, departure: false, help: "re-level the view after mouse look",
        get: |c| flag(c.lookspring), set: |c, v| c.lookspring = on(v) },
    Cvar { name: "lookstrafe", archive: true, departure: false, help: "mouse X strafes in mouse look",
        get: |c| flag(c.lookstrafe), set: |c, v| c.lookstrafe = on(v) },
    Cvar { name: "sensitivity", archive: true, departure: false, help: "mouse speed",
        get: |c| number_string(c.sensitivity), set: |c, v| c.sensitivity = atof(v) },
    Cvar { name: "m_pitch", archive: true, departure: false, help: "negative: Invert Mouse",
        get: |c| number_string(c.m_pitch), set: |c, v| c.m_pitch = atof(v) },
    Cvar { name: "crosshair", archive: true, departure: true, help: "a + at the view's centre",
        get: |c| flag(c.crosshair), set: |c, v| c.crosshair = on(v) },
    Cvar { name: "gamma", archive: true, departure: false, help: "brightness (1 is none)",
        get: |c| number_string(c.gamma), set: |c, v| c.gamma = atof(v) },
    Cvar { name: "viewsize", archive: true, departure: false, help: "screen size, 30..120",
        get: |c| number_string(c.viewsize), set: |c, v| c.set_viewsize(atof(v)) },
    Cvar { name: "volume", archive: true, departure: false, help: "sound volume, 0..1",
        get: |c| number_string(c.volume), set: |c, v| c.volume = atof(v) },
    Cvar { name: "bgmvolume", archive: true, departure: false, help: "CD music volume (no CD)",
        get: |c| number_string(c.bgmvolume), set: |c, v| c.bgmvolume = atof(v) },
    Cvar { name: "d_mipscale", archive: false, departure: false, help: "mip level distance scale",
        get: |c| number_string(c.d_mipscale), set: |c, v| c.d_mipscale = atof(v) },
    Cvar { name: "d_mipcap", archive: false, departure: false, help: "finest mip level allowed",
        get: |c| number_string(c.d_mipcap), set: |c, v| c.d_mipcap = atof(v) },
    Cvar { name: "hostname", archive: false, departure: false, help: "the server's name",
        get: |c| c.hostname.clone(), set: |c, v| c.hostname = v.to_string() },
    Cvar { name: "_vid_resolution", archive: true, departure: false, help: "video mode WxH (4:3 box)",
        get: |c| format!("{}x{}", c.vid_resolution.0, c.vid_resolution.1),
        set: |c, v| if let Some(m) = parse_mode(v) { c.vid_resolution = m } },
    Cvar { name: "wasm_uncapped", archive: true, departure: true, help: "no 72 fps cap",
        get: |c| flag(c.uncapped), set: |c, v| c.uncapped = on(v) },
    Cvar { name: "wasm_showfps", archive: true, departure: true, help: "frame rate readout",
        get: |c| flag(c.show_fps), set: |c, v| c.show_fps = on(v) },
    Cvar { name: "wasm_exactpersp", archive: true, departure: true, help: "exact perspective per pixel",
        get: |c| flag(c.exact_persp), set: |c, v| c.exact_persp = on(v) },
    Cvar { name: "wasm_scaled2d", archive: true, departure: true, help: "2-D layer at id's proportions",
        get: |c| flag(c.scaled_2d), set: |c, v| c.scaled_2d = on(v) },
    Cvar { name: "vid_native", archive: true, departure: true, help: "fill the window, native pixels",
        get: |c| flag(c.native), set: |c, v| c.native = on(v) },
    Cvar { name: "vid_pixelsize", archive: true, departure: true, help: "0 auto, 1..4 pixels a pixel",
        get: |c| c.pixel_size.to_string(),
        set: |c, v| c.pixel_size = atof(v).clamp(0.0, f32::from(PIXEL_SIZE_MAX)) as u8 },
    Cvar { name: "fov_adapt", archive: true, departure: true, help: "wider screens see more (Hor+)",
        get: |c| flag(c.fov_adapt), set: |c, v| c.fov_adapt = on(v) },
    Cvar { name: "freelook", archive: true, departure: true, help: "mouse look without +mlook",
        get: |c| flag(c.freelook), set: |c, v| c.freelook = on(v) },
    Cvar { name: "cl_jumpswim", archive: true, departure: true, help: "+jump also swims up",
        get: |c| flag(c.jumpswim), set: |c, v| c.jumpswim = on(v) },
    Cvar { name: "vid_fkey", archive: true, departure: true, help: "F toggles fullscreen",
        get: |c| flag(c.fkey), set: |c, v| c.fkey = on(v) },
];

/// `Cvar_FindVar`: the cvar called `name` (any case, as the port's console
/// matches names).
pub fn find(name: &str) -> Option<&'static Cvar> {
    CVARS.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// `Cvar_CompleteVariable`: the first cvar whose name starts with `partial`
/// (case matters, `Q_strncmp`); nothing for an empty string.
pub fn complete(partial: &str) -> Option<&'static str> {
    if partial.is_empty() {
        return None;
    }
    CVARS.iter().map(|c| c.name).find(|n| n.starts_with(partial))
}

/// `Cvar_WriteVariables`, as the lines that turn `base` into `now`: `name
/// "value"` for each archived cvar whose value differs.
pub fn write_changes(now: &Cvars, base: &Cvars, out: &mut String) {
    for c in CVARS.iter().filter(|c| c.archive) {
        let v = c.get(now);
        if v != c.get(base) {
            out.push_str(&format!("{} \"{v}\"\n", c.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_round_trips_through_its_console_name() {
        let modern = Cvars::modern();
        let mut c = Cvars::classic();
        for v in CVARS {
            v.set(&mut c, &v.get(&modern));
        }
        assert_eq!(c, modern, "setting each cvar to the 2026 value's string gives the 2026 cvars");
        let names: std::collections::HashSet<_> = CVARS.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), CVARS.len(), "no name twice");
    }

    #[test]
    fn the_profiles_differ_only_in_departures() {
        let (id, modern) = (Cvars::classic(), Cvars::modern());
        for c in CVARS {
            if c.get(&id) != c.get(&modern) {
                assert!(c.departure, "{} differs between the profiles, so it is a departure", c.name);
            }
        }
        for c in CVARS.iter().filter(|c| c.departure) {
            assert!(c.archive, "{}: a departure is kept in config.cfg", c.name);
        }
        assert!(!id.always_run() && modern.always_run());
    }

    #[test]
    fn cvar_command_parses_like_q_atof() {
        assert_eq!((atof("100"), atof("0.7"), atof("-0.022"), atof("junk"), atof(" 55xyz")), (100.0, 0.7, -0.022, 0.0, 55.0));
        assert_eq!((number_string(100.0), number_string(0.7), number_string(-0.022)), ("100".into(), "0.7".into(), "-0.022".into()));
        let mut c = Cvars::classic();
        find("VIEWSIZE").unwrap().set(&mut c, "500");
        assert_eq!(c.viewsize, 120.0, "viewsize is bounded as SCR_CalcRefdef bounds it");
        find("vid_pixelsize").unwrap().set(&mut c, "9");
        assert_eq!(c.pixel_size, PIXEL_SIZE_MAX);
        find("_vid_resolution").unwrap().set(&mut c, "640x400");
        assert_eq!(c.vid_resolution, (640, 400));
        find("_vid_resolution").unwrap().set(&mut c, "nonsense");
        assert_eq!(c.vid_resolution, (640, 400), "a bad mode is ignored");
        assert_eq!(complete("vid_n"), Some("vid_native"));
        assert_eq!(complete(""), None);
    }

    #[test]
    fn invert_mouse_and_always_run_are_ids_cvars() {
        let mut c = Cvars::classic();
        c.set_invert_mouse(true);
        assert_eq!(c.m_pitch, -0.022);
        c.set_invert_mouse(true);
        assert_eq!(c.m_pitch, -0.022, "already inverted");
        c.set_always_run(true);
        assert_eq!((c.cl_forwardspeed, c.cl_backspeed), (400.0, 400.0));
        let mut out = String::new();
        write_changes(&c, &Cvars::classic(), &mut out);
        assert_eq!(out, "cl_forwardspeed \"400\"\ncl_backspeed \"400\"\nm_pitch \"-0.022\"\n");
    }
}
