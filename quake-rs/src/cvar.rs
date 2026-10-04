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
//! Two kinds of field. **id's cvars**, with id's defaults (one exception:
//! `viewsize` starts one step larger in [`Cvars::slop`], so the HUD takes
//! less of a slop screen; it stays id's own cvar, and
//! [`crate::settings::Settings::apply_preset`] moves it with the preset only
//! while the player has not moved it). And **the slop options**, the port's
//! departures from id's game, each marked [`Cvar::departure`]: a preset
//! ([`crate::settings::Preset`]) sets them all, off in [`Cvars::classic`]
//! and on in [`Cvars::slop`] — `crosshair`, the renderer and stepping
//! options, the slop mixer, the bigger edict pool.
//!
//! A slop option can be a *control* rather than the engine: mouse look
//! (`freelook`), the gamepad (`joystick`, in_win.c's advanced configuration
//! as the slop pad layout, and the port's `joy_*`), Space-swims-up
//! (`cl_jumpswim`), Alt+Enter (`vid_altenter`) and the touch controls
//! (`in_touch`) depart from id's own defaults but not from each other's:
//! both presets have them on, so applying either changes one only where the
//! player changed it. Always Run (`cl_forwardspeed`/`cl_backspeed`) is
//! on in both too, but it is id's own Options row, so no preset touches it.
//! id's 1996 controls are one explicit step away, never a preset:
//! [`Cvars::with_id_controls`], the console's `idcontrols`.

use crate::client::host::FrameCap;
use crate::client::in_win::JoyCvars;
use crate::client::lerpmodels::LerpModels;
use crate::client::lerpmove::LerpMove;
use crate::render::{Crosshair, PerspSpan, SkyScroll, TorchFlicker};
use crate::snd::SoundMode;
use crate::screen::{SbarLayout, VIEWSIZE_DEFAULT, VIEWSIZE_MAX, VIEWSIZE_MIN, VIEWSIZE_MODERN, VIEWSIZE_STEP};
use crate::server::LerpLightStyles;
use crate::vm::{MAX_EDICTS, MAX_EDICTS_LIMIT};

/// The largest of the port's pixel sizes, [`Cvars::pixel_size`]: 1 to 4
/// device pixels a side to one of the picture's.
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
    /// `bgmvolume` (snd_dma.c): CD Music Volume, the level of the player's
    /// own CD tracks ([`crate::cd_audio`]). With none added it is a value
    /// nothing plays at, as in a C build with `cd_null.c`.
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
    /// `crosshair` (view.c): what `V_RenderView` draws at the view's centre
    /// ([`Crosshair`]: 0 none, 1 the slop cross, 2 id's `+`). A departure:
    /// the cross in the slop preset.
    pub crosshair: Crosshair,
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
    /// `host_maxfps` (QuakeSpasm's name): the most frames a second
    /// ([`FrameCap`]). id's 72 is `Host_FilterTime`'s gate (Classic); none, a
    /// host frame on every display refresh, the game stepped as id's 72 Hz
    /// frames ([`crate::stepping::Stepping::Uncapped`]); a touch screen's
    /// slop preset starts at 60 ([`crate::settings::Machine::frame_cap`]).
    /// The retired `wasm_uncapped` still sets and reads it ([`RETIRED`]).
    pub max_fps: FrameCap,
    /// `wasm_showfps`: QuakeWorld's frame-rate readout.
    pub show_fps: bool,
    /// `r_perspspan`: how often the walls and liquids find their texel
    /// exactly ([`PerspSpan`]): every 16 pixels and affine between, id's
    /// `D_DrawSpans16` (Classic); 64 or 32, longer, about 1996's look on a
    /// 1080p or a phone's frame; 8, id's portable C `D_DrawSpans8` (slop);
    /// 4; or 1, exact at every pixel. The retired `wasm_exactpersp` still
    /// sets and reads it ([`RETIRED`]).
    pub persp_span: PerspSpan,
    /// `wasm_scaled2d`: the 2-D layer (status bar, menus, console) at the
    /// largest whole multiple of id's 320x200 that fits, where id draws it
    /// 1:1 ([`crate::draw::screen_2d`]).
    pub scaled_2d: bool,
    /// `scr_sbaroverlay`: the world goes on under the 3-D view, beside the
    /// status bar ([`SbarLayout::Overlay`]), where id tiles the bar's sides
    /// with `backtile`; the view itself is id's, pixel for pixel.
    pub sbar_layout: SbarLayout,
    /// `vid_native`: the picture fills the window at the window's own aspect,
    /// rendered at its device pixels divided by [`Cvars::pixel_size`], with
    /// square pixels and views past id's 1280x1024 (the renderer's `hires`);
    /// off, the video mode [`Cvars::vid_resolution`] in a 4:3 box, as a 1996
    /// monitor showed it.
    pub native: bool,
    /// `vid_pixelsize`: with [`Cvars::native`], how many device pixels a
    /// side make one of the picture's, 1..=[`PIXEL_SIZE_MAX`]: whole pixels,
    /// never smoothed. The presets start it at the machine's number
    /// ([`crate::settings::Machine::pixel_size`]); the host takes the next
    /// size up when a frame this size would not fit its memory.
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
    /// `vid_altenter`: Alt+Enter toggles fullscreen — the page's, taken
    /// before the game sees either key, whatever has the keyboard (the game,
    /// the menu, the console); QuakeSpasm's `VID_Toggle` chord. Off, the
    /// chord is the game's, as in WinQuake, where `default.cfg` binds ALT
    /// `+strafe` and ENTER `+jump`. Its old name, `vid_fkey` (when the key
    /// was F), still sets it ([`OLD_NAMES`]).
    pub alt_enter: bool,
    /// `r_lerpmove` (QuakeSpasm's name): monsters glide between their steps
    /// ([`LerpMove::Smooth`]) instead of being drawn where each 0.1 s step put
    /// them, as id's client does.
    pub lerpmove: LerpMove,
    /// `r_lerpmodels` (QuakeSpasm's name): an alias model's animation blends
    /// between its frames ([`LerpModels::Smooth`], `client::lerpmodels`)
    /// instead of snapping to each one, as id's client does.
    pub lerpmodels: LerpModels,
    /// `r_fluidsky`: the sky's cloud layer scrolls by its exact offset
    /// ([`SkyScroll::Fluid`], `render::sky`) instead of `R_MakeSky`'s whole
    /// texels, eight jumps a second.
    pub sky: SkyScroll,
    /// `snd_modern`: which of id's mixers plays ([`SoundMode`]): the slop one
    /// (its faults fixed, `snd::Fixes::ALL`, at the device's rate) instead
    /// of id's as written at 11025 Hz.
    pub sound: SoundMode,
    /// `r_threads`: how many threads draw the 3-D view, at least 1. The
    /// presets start it at the machine's number
    /// ([`crate::settings::Machine::render_threads`]). The pixels are the
    /// same for any count, so it is no departure.
    pub threads: usize,
    /// `sv_max_edicts`: the `ED_Alloc` ceiling ([`crate::vm::MAX_EDICTS`] in
    /// Classic, where id's own 600 is also the port's; higher in slop). A
    /// departure, but an unusual one: it never changes anything *drawn* —
    /// id's `MAX_EDICTS` is an engine limit, not game design — only whether
    /// a map that needs more than 600 edicts can be played at all. No map of
    /// id1 or the two mission packs does (Rogue's `r2m6` peaks at 546, as in
    /// id's C); it is room for bigger maps. Clamped to
    /// [`crate::vm::MAX_EDICTS`]..=[`crate::vm::MAX_EDICTS_LIMIT`].
    pub max_edicts: u32,
    /// `in_touch`: on a touch screen, the page's touch controls for play —
    /// a stick, look by dragging, fire, jump and next weapon (quake-wasm's
    /// `web/touch.js`). id's Quake has none; without them a phone can only
    /// open the menu, which stays tappable either way — so they are on in
    /// both presets (input, not the engine): the Classic preset must not
    /// leave a phone unplayable.
    pub touch: bool,
    /// `in_touchaccel`: how much a fast drag turns further than a slow one
    /// of the same length (0: none, the view turns with the finger).
    /// Nothing reads it but the touch controls, so it is no departure.
    pub touch_accel: f32,
    /// The joystick's: in_win.c's `joystick` and `joy*`, and the port's
    /// `joy_*` (slop's pad layout, stick shaping, menu keys, rumble).
    pub joy: JoyCvars,
    /// `r_lerplightstyles` (DarkPlaces' name): an animated light's brightness
    /// glides between its pattern's letters ([`LerpLightStyles::Smooth`],
    /// `server::lightstyle_scales_at`) instead of snapping ten times a second,
    /// as id's `R_AnimateLight` does.
    pub lightstyles: LerpLightStyles,
    /// `r_torchflicker`: how much the steady torches (style 0, as LIGHT.EXE
    /// baked them) flicker about their light, as if the mapper had given each
    /// a flicker style — 0 off, 1 that style's swing, at most 2
    /// ([`TorchFlicker`], `render::torch`).
    pub torches: TorchFlicker,
}

impl Default for Cvars {
    fn default() -> Self {
        Cvars::classic()
    }
}

impl Cvars {
    /// id's WinQuake: every *engine* departure off (rendering, stepping,
    /// timing — everything `oracle/classic_check.py` compares). The
    /// *controls* — Always Run, mouse look, the gamepad, Space swims up,
    /// Alt+Enter, the touch controls — are already the shared default here
    /// too (the module
    /// docs say why); id's own 1996 ones (arrows, no mouse look, no
    /// gamepad, Always Run off) are [`Cvars::with_id_controls`]. The numbers
    /// a machine picks (the renderer's threads, the pixel size) are a plain
    /// machine's here, one thread at 1x: the presets give them the host's
    /// ([`crate::settings::Preset::cvars`]).
    pub fn classic() -> Cvars {
        Cvars {
            viewsize: VIEWSIZE_DEFAULT,
            gamma: 1.0,
            volume: 0.7,
            bgmvolume: 1.0,
            sensitivity: 3.0,
            cl_forwardspeed: 400.0,
            cl_backspeed: 400.0,
            m_pitch: 0.022,
            lookspring: false,
            lookstrafe: false,
            crosshair: Crosshair::Off,
            cl_name: "player".to_string(),
            cl_color: 0,
            hostname: "UNNAMED".to_string(),
            d_mipscale: 1.0,
            d_mipcap: 0.0,
            vid_resolution: (960, 600),
            max_fps: FrameCap::ID,
            show_fps: false,
            persp_span: PerspSpan::Spans16,
            scaled_2d: false,
            sbar_layout: SbarLayout::Classic,
            native: false,
            pixel_size: 1,
            fov_adapt: false,
            freelook: true,
            jumpswim: true,
            alt_enter: true,
            lerpmove: LerpMove::Classic,
            lerpmodels: LerpModels::Classic,
            sky: SkyScroll::Classic,
            sound: SoundMode::Classic,
            threads: 1,
            max_edicts: MAX_EDICTS as u32,
            touch: true,
            touch_accel: 0.0,
            joy: JoyCvars::modern(),
            lightstyles: LerpLightStyles::Classic,
            torches: TorchFlicker::OFF,
        }
    }

    /// The slop preset's: an idealized software-rendered Quake on a 2026
    /// machine. A frame every display refresh, the window filled at native
    /// resolution in whole chunky pixels with a Hor+ field of view, the 2-D
    /// layer at id's proportions with the world on beside the status bar,
    /// the crosshair, monsters that glide between their steps and whose
    /// animation blends between frames, clouds that glide across the sky,
    /// flickering lights that glide between their brightnesses, and
    /// perspective found exactly every 8 pixels
    /// along the walls and liquids (id's own portable-C loop, `D_DrawSpans8`:
    /// the user's call, 2026-10-03, for every device). That one is not
    /// Classic's because of the resolution: id's 16-pixel affine spans were a
    /// pixel or so off at 320x200, but at 1080p and above they show as a
    /// wobble along a wall seen at a grazing angle; 8 is much nearer exact
    /// than 16 for +7-11% of the 3-D view's cost (AUDIT.md, "The slop options and
    /// the presets"), and every value up to exact (1) stays one setting
    /// away. Show FPS stays off: the readout is clutter. (Always Run, mouse look, the gamepad and Space-swims-up are
    /// [`Cvars::classic`]'s too now — they are controls, not engine.) The
    /// edict pool grows past id's 600 (`max_edicts`, QuakeSpasm's own
    /// default) — invisible on every map id or the mission packs shipped,
    /// room for bigger ones. Screen size (`viewsize`, id's own cvar) starts at
    /// [`VIEWSIZE_MODERN`], one step past id's 100: the inventory strip goes
    /// and the status bar stays, so the HUD takes less of a slop screen.
    pub fn slop() -> Cvars {
        Cvars {
            viewsize: VIEWSIZE_MODERN,
            crosshair: Crosshair::Cross,
            max_fps: FrameCap::NONE,
            persp_span: PerspSpan::Spans8,
            scaled_2d: true,
            sbar_layout: SbarLayout::Overlay,
            native: true,
            fov_adapt: true,
            lerpmove: LerpMove::Smooth,
            lerpmodels: LerpModels::Smooth,
            sky: SkyScroll::Fluid,
            sound: SoundMode::Modern,
            max_edicts: 8192,
            lightstyles: LerpLightStyles::Smooth,
            torches: TorchFlicker::MODERN,
            ..Cvars::classic()
        }
    }

    /// `self` with every *control* at id's own 1996 `default.cfg`: Always
    /// Run off (`cl_forwardspeed`/`cl_backspeed` 200), no mouse look
    /// (`freelook` off), Space does not swim (`jumpswim` off), no
    /// Alt+Enter, and id's joystick ([`JoyCvars::classic`]: `joystick` off,
    /// no advanced axis layout, id's thresholds, no dead zone, curve, menu
    /// keys or rumble). Every *engine* field — whatever preset `self` came
    /// from — is untouched. The explicit, one console command (`idcontrols`)
    /// back to id's controls, and what the oracle harness pins against
    /// (`quaketool play`/`sound`: [`crate::settings::Settings::id`]) so it
    /// keeps comparing id's controls whatever the shared default becomes.
    pub fn with_id_controls(mut self) -> Cvars {
        self.cl_forwardspeed = 200.0;
        self.cl_backspeed = 200.0;
        self.freelook = false;
        self.jumpswim = false;
        self.alt_enter = false;
        self.joy = JoyCvars::classic();
        self
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
    /// A slop option, a departure from id's game: a preset sets it
    /// ([`Cvars::classic`] has it off, or at the value both presets share).
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
    Cvar { name: "joywwhack2", archive: false, departure: false, help: "WingMan Warrior spinner curve",
        get: |c| number_string(c.joy.wwhack2), set: |c, v| c.joy.wwhack2 = atof(v) },
    Cvar { name: "joywwhack1", archive: false, departure: false, help: "WingMan Warrior U axis fix",
        get: |c| number_string(c.joy.wwhack1), set: |c, v| c.joy.wwhack1 = atof(v) },
    Cvar { name: "joyyawsensitivity", archive: true, departure: true, help: "joystick turn scale (sign: way)",
        get: |c| number_string(c.joy.yaw_sensitivity), set: |c, v| c.joy.yaw_sensitivity = atof(v) },
    Cvar { name: "joypitchsensitivity", archive: true, departure: true, help: "joystick look scale",
        get: |c| number_string(c.joy.pitch_sensitivity), set: |c, v| c.joy.pitch_sensitivity = atof(v) },
    Cvar { name: "joysidesensitivity", archive: true, departure: true, help: "joystick strafe scale",
        get: |c| number_string(c.joy.side_sensitivity), set: |c, v| c.joy.side_sensitivity = atof(v) },
    Cvar { name: "joyforwardsensitivity", archive: true, departure: true, help: "joystick walk scale",
        get: |c| number_string(c.joy.forward_sensitivity), set: |c, v| c.joy.forward_sensitivity = atof(v) },
    Cvar { name: "joyyawthreshold", archive: true, departure: true, help: "joystick turn dead zone",
        get: |c| number_string(c.joy.yaw_threshold), set: |c, v| c.joy.yaw_threshold = atof(v) },
    Cvar { name: "joypitchthreshold", archive: true, departure: true, help: "joystick look dead zone",
        get: |c| number_string(c.joy.pitch_threshold), set: |c, v| c.joy.pitch_threshold = atof(v) },
    Cvar { name: "joysidethreshold", archive: true, departure: true, help: "joystick strafe dead zone",
        get: |c| number_string(c.joy.side_threshold), set: |c, v| c.joy.side_threshold = atof(v) },
    Cvar { name: "joyforwardthreshold", archive: true, departure: true, help: "joystick walk dead zone",
        get: |c| number_string(c.joy.forward_threshold), set: |c, v| c.joy.forward_threshold = atof(v) },
    Cvar { name: "joyadvaxisv", archive: true, departure: true, help: "axis V: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[5]), set: |c, v| c.joy.advaxis[5] = atof(v) },
    Cvar { name: "joyadvaxisu", archive: true, departure: true, help: "axis U: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[4]), set: |c, v| c.joy.advaxis[4] = atof(v) },
    Cvar { name: "joyadvaxisr", archive: true, departure: true, help: "axis R: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[3]), set: |c, v| c.joy.advaxis[3] = atof(v) },
    Cvar { name: "joyadvaxisz", archive: true, departure: true, help: "axis Z: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[2]), set: |c, v| c.joy.advaxis[2] = atof(v) },
    Cvar { name: "joyadvaxisy", archive: true, departure: true, help: "axis Y: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[1]), set: |c, v| c.joy.advaxis[1] = atof(v) },
    Cvar { name: "joyadvaxisx", archive: true, departure: true, help: "axis X: 1 fwd 2 look 3 side 4 turn",
        get: |c| number_string(c.joy.advaxis[0]), set: |c, v| c.joy.advaxis[0] = atof(v) },
    Cvar { name: "joyadvanced", archive: true, departure: true, help: "axis maps from joyadvaxis*",
        get: |c| flag(c.joy.advanced), set: |c, v| c.joy.advanced = on(v) },
    Cvar { name: "joyname", archive: false, departure: false, help: "the controller's name",
        get: |c| c.joy.name.clone(), set: |c, v| c.joy.name = v.to_string() },
    Cvar { name: "joystick", archive: true, departure: true, help: "use the joystick / gamepad",
        get: |c| flag(c.joy.enabled), set: |c, v| c.joy.enabled = on(v) },
    Cvar { name: "_cl_color", archive: true, departure: false, help: "shirt*16 + pants colour",
        get: |c| c.cl_color.to_string(), set: |c, v| c.cl_color = atof(v) as i32 },
    Cvar { name: "_cl_name", archive: true, departure: false, help: "the player's name",
        get: |c| c.cl_name.clone(), set: |c, v| c.cl_name = v.to_string() },
    Cvar { name: "cl_forwardspeed", archive: true, departure: false, help: "walk speed (400: Always Run)",
        get: |c| number_string(c.cl_forwardspeed), set: |c, v| c.cl_forwardspeed = atof(v) },
    Cvar { name: "cl_backspeed", archive: true, departure: false, help: "backpedal speed",
        get: |c| number_string(c.cl_backspeed), set: |c, v| c.cl_backspeed = atof(v) },
    Cvar { name: "lookspring", archive: true, departure: false, help: "re-level the view after mouse look",
        get: |c| flag(c.lookspring), set: |c, v| c.lookspring = on(v) },
    Cvar { name: "lookstrafe", archive: true, departure: false, help: "mouse X strafes in mouse look",
        get: |c| flag(c.lookstrafe), set: |c, v| c.lookstrafe = on(v) },
    Cvar { name: "sensitivity", archive: true, departure: false, help: "mouse speed",
        get: |c| number_string(c.sensitivity), set: |c, v| c.sensitivity = atof(v) },
    Cvar { name: "m_pitch", archive: true, departure: false, help: "negative: Invert Mouse",
        get: |c| number_string(c.m_pitch), set: |c, v| c.m_pitch = atof(v) },
    Cvar { name: "crosshair", archive: true, departure: true, help: "0 off, 1 a thin cross, 2 id's +",
        get: |c| c.crosshair.cvar().to_string(), set: |c, v| c.crosshair = Crosshair::from_cvar(atof(v)) },
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
    Cvar { name: "host_maxfps", archive: true, departure: true, help: "frames a second at most, 0 none",
        get: |c| c.max_fps.cvar().to_string(), set: |c, v| c.max_fps = FrameCap::from_cvar(atof(v)) },
    Cvar { name: "wasm_showfps", archive: true, departure: true, help: "frame rate readout",
        get: |c| flag(c.show_fps), set: |c, v| c.show_fps = on(v) },
    Cvar { name: "r_perspspan", archive: true, departure: true, help: "exact every 64,32,16 (id),8,4,1 px",
        get: |c| c.persp_span.pixels().to_string(), set: |c, v| c.persp_span = PerspSpan::from_pixels(atof(v)) },
    Cvar { name: "wasm_scaled2d", archive: true, departure: true, help: "2-D layer at id's proportions",
        get: |c| flag(c.scaled_2d), set: |c, v| c.scaled_2d = on(v) },
    Cvar { name: "scr_sbaroverlay", archive: true, departure: true, help: "the world beside the status bar",
        get: |c| flag(c.sbar_layout == SbarLayout::Overlay),
        set: |c, v| c.sbar_layout = if on(v) { SbarLayout::Overlay } else { SbarLayout::Classic } },
    Cvar { name: "vid_native", archive: true, departure: true, help: "fill the window, native pixels",
        get: |c| flag(c.native), set: |c, v| c.native = on(v) },
    Cvar { name: "vid_pixelsize", archive: true, departure: true, help: "1..4 screen pixels a pixel",
        get: |c| c.pixel_size.to_string(),
        set: |c, v| c.pixel_size = atof(v).clamp(1.0, f32::from(PIXEL_SIZE_MAX)) as u8 },
    Cvar { name: "fov_adapt", archive: true, departure: true, help: "wider screens see more (Hor+)",
        get: |c| flag(c.fov_adapt), set: |c, v| c.fov_adapt = on(v) },
    Cvar { name: "freelook", archive: true, departure: true, help: "mouse look without +mlook",
        get: |c| flag(c.freelook), set: |c, v| c.freelook = on(v) },
    Cvar { name: "cl_jumpswim", archive: true, departure: true, help: "+jump also swims up",
        get: |c| flag(c.jumpswim), set: |c, v| c.jumpswim = on(v) },
    Cvar { name: "vid_altenter", archive: true, departure: true, help: "Alt+Enter toggles fullscreen",
        get: |c| flag(c.alt_enter), set: |c, v| c.alt_enter = on(v) },
    Cvar { name: "r_lerpmove", archive: true, departure: true, help: "monsters glide between steps",
        get: |c| flag(c.lerpmove == LerpMove::Smooth),
        set: |c, v| c.lerpmove = if on(v) { LerpMove::Smooth } else { LerpMove::Classic } },
    Cvar { name: "r_lerpmodels", archive: true, departure: true, help: "animation frames blend together",
        get: |c| flag(c.lerpmodels == LerpModels::Smooth),
        set: |c, v| c.lerpmodels = if on(v) { LerpModels::Smooth } else { LerpModels::Classic } },
    Cvar { name: "r_fluidsky", archive: true, departure: true, help: "sky clouds glide, not texel steps",
        get: |c| flag(c.sky == SkyScroll::Fluid),
        set: |c, v| c.sky = if on(v) { SkyScroll::Fluid } else { SkyScroll::Classic } },
    Cvar { name: "snd_modern", archive: true, departure: true, help: "slop mixer: device rate, fixes",
        get: |c| flag(c.sound == SoundMode::Modern),
        set: |c, v| c.sound = if on(v) { SoundMode::Modern } else { SoundMode::Classic } },
    Cvar { name: "r_threads", archive: true, departure: false, help: "threads that draw the 3-D view",
        get: |c| c.threads.to_string(), set: |c, v| c.threads = atof(v).max(1.0) as usize },
    Cvar { name: "sv_max_edicts", archive: true, departure: true, help: "edict pool past id's 600, for big maps",
        get: |c| c.max_edicts.to_string(),
        set: |c, v| c.max_edicts = atof(v).clamp(MAX_EDICTS as f32, MAX_EDICTS_LIMIT as f32) as u32 },
    Cvar { name: "in_touch", archive: true, departure: true, help: "touch controls on a touch screen",
        get: |c| flag(c.touch), set: |c, v| c.touch = on(v) },
    Cvar { name: "in_touchaccel", archive: true, departure: false, help: "touch look acceleration, 0 none",
        get: |c| number_string(c.touch_accel), set: |c, v| c.touch_accel = atof(v).clamp(0.0, 4.0) },
    Cvar { name: "joy_deadzone", archive: true, departure: true, help: "round stick dead zone, 0 off",
        get: |c| number_string(c.joy.deadzone), set: |c, v| c.joy.deadzone = atof(v) },
    Cvar { name: "joy_exponent", archive: true, departure: true, help: "look stick curve, 1 straight",
        get: |c| number_string(c.joy.exponent), set: |c, v| c.joy.exponent = atof(v) },
    Cvar { name: "joy_menukeys", archive: true, departure: true, help: "pad A/B/D-pad work the menus",
        get: |c| flag(c.joy.menu_keys), set: |c, v| c.joy.menu_keys = on(v) },
    Cvar { name: "joy_rumble", archive: true, departure: true, help: "pad rumble strength, 0 off",
        get: |c| number_string(c.joy.rumble), set: |c, v| c.joy.rumble = atof(v) },
    Cvar { name: "r_lerplightstyles", archive: true, departure: true, help: "flickering lights glide, not snap",
        get: |c| flag(c.lightstyles == LerpLightStyles::Smooth),
        set: |c, v| c.lightstyles = if on(v) { LerpLightStyles::Smooth } else { LerpLightStyles::Classic } },
    Cvar { name: "r_torchflicker", archive: true, departure: true, help: "torch flicker: 0 off, 1, 2 is noisy",
        get: |c| number_string(c.torches.value()), set: |c, v| c.torches = TorchFlicker::from_value(atof(v)) },
];

/// A renamed cvar's old name, and the name it has now. A `config.cfg` saved
/// before the rename still sets the setting: the file is exec'd through the
/// console, whose [`find`] reads an old name as the new one; the next save
/// writes the new name ([`write_changes`] knows only [`CVARS`]). Tab
/// completion and the lists offer only the new names.
const OLD_NAMES: &[(&str, &str)] = &[
    // The page's fullscreen key was F until 2026-10-02: a letter can't work
    // whatever has the keyboard (the console types it, a bind takes it).
    ("vid_fkey", "vid_altenter"),
];

/// A cvar replaced by one that does more, kept as a view onto it: a
/// `config.cfg` saved before the change still sets the setting, and the
/// console still reads and sets it by the old name; but [`write_changes`],
/// completion, the lists and the presets know only [`CVARS`], so the next
/// save writes the new cvar alone. (A cvar merely renamed, its values as
/// they were, is [`OLD_NAMES`]'.)
const RETIRED: &[Cvar] = &[
    // The on/off of exact perspective until 2026-10-03, now the span's two
    // ends: on is `r_perspspan 1`, off id's 16, and it reads 1 only while
    // the span is 1. A saved `wasm_exactpersp "1"` (written by a Classic
    // player who switched it on) draws exact perspective, as it did; a saved
    // "0" (a slop player who switched it off) draws id's 16-pixel spans.
    Cvar { name: "wasm_exactpersp", archive: false, departure: true, help: "old: 1 is r_perspspan 1, 0 is 16",
        get: |c| flag(c.persp_span == PerspSpan::Exact),
        set: |c, v| c.persp_span = if on(v) { PerspSpan::Exact } else { PerspSpan::Spans16 } },
    // The on/off of id's 72 fps cap until 2026-10-03, now `host_maxfps`: on
    // is none (0), off id's 72, and it reads 1 only while there is no cap.
    // A saved `wasm_uncapped "0"` (a slop player who switched it off) runs
    // id's gate, as it did; a saved "1" (a Classic player) runs uncapped.
    Cvar { name: "wasm_uncapped", archive: false, departure: true, help: "old: 1 is host_maxfps 0, 0 is 72",
        get: |c| flag(c.max_fps == FrameCap::NONE),
        set: |c, v| c.max_fps = if on(v) { FrameCap::NONE } else { FrameCap::ID } },
];

/// `Cvar_FindVar`: the cvar called `name` (any case, as the port's console
/// matches names), or called that before it was renamed ([`OLD_NAMES`]), or
/// a retired one ([`RETIRED`]).
pub fn find(name: &str) -> Option<&'static Cvar> {
    let name = OLD_NAMES.iter().find(|(old, _)| old.eq_ignore_ascii_case(name)).map_or(name, |&(_, new)| new);
    CVARS.iter().chain(RETIRED).find(|c| c.name.eq_ignore_ascii_case(name))
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
        let slop = Cvars::slop();
        let mut c = Cvars::classic();
        for v in CVARS {
            v.set(&mut c, &v.get(&slop));
        }
        assert_eq!(c, slop, "setting each cvar to the slop value's string gives the slop cvars");
        let names: std::collections::HashSet<_> = CVARS.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), CVARS.len(), "no name twice");
    }

    #[test]
    fn the_profiles_differ_only_in_departures_and_the_screen_size() {
        let (id, slop) = (Cvars::classic(), Cvars::slop());
        for c in CVARS {
            if c.get(&id) != c.get(&slop) {
                // Screen size is id's own cvar, started one step larger in
                // slop: not a departure (a preset keeps the
                // player's own value, `Settings::apply_preset`).
                assert!(c.departure || c.name == "viewsize", "{} differs between the presets, so it is a departure", c.name);
            }
        }
        assert_eq!((id.viewsize, slop.viewsize), (VIEWSIZE_DEFAULT, VIEWSIZE_DEFAULT + VIEWSIZE_STEP));
        assert_eq!(slop.viewsize, VIEWSIZE_MODERN);
        assert!(!find("viewsize").unwrap().departure, "id's own cvar");
        for c in CVARS.iter().filter(|c| c.departure) {
            assert!(c.archive, "{}: a departure is kept in config.cfg", c.name);
        }
        assert!(id.always_run() && slop.always_run(), "Always Run is a shared control, on by default in both");
    }

    /// The controls (module docs): departures from id, but not from each
    /// other — [`Cvars::classic`] and [`Cvars::slop`] agree on them. The
    /// ones on the Controls page and the pad's layout are slop options
    /// (a preset puts back a player's change); Always Run is id's own
    /// Options row and no slop option. [`Cvars::with_id_controls`] is the
    /// one way back to id's own.
    #[test]
    fn the_controls_are_the_same_in_both_presets() {
        let (id, slop) = (Cvars::classic(), Cvars::slop());
        for name in [
            "cl_forwardspeed", "cl_backspeed", "freelook", "cl_jumpswim", "vid_altenter", "joystick", "joy_rumble",
            "joyadvanced", "joy_deadzone", "in_touch",
        ] {
            let c = find(name).unwrap();
            assert_eq!(c.get(&id), c.get(&slop), "{name}: the same in both presets");
            assert_eq!(c.departure, !name.starts_with("cl_") || name == "cl_jumpswim", "{name}: a slop option, but Always Run");
        }
        assert_eq!(id.joy, slop.joy, "the whole gamepad layout, not just `joystick`");
        assert_eq!(id.joy, JoyCvars::modern(), "Cvars::classic already has the slop pad");

        // with_id_controls touches only the controls: everything else stays
        // whatever preset it came from.
        let old = slop.clone().with_id_controls();
        assert!(!old.freelook && !old.jumpswim && !old.alt_enter && !old.always_run() && old.joy == JoyCvars::classic());
        assert_eq!((old.max_fps, old.native, old.crosshair), (slop.max_fps, slop.native, slop.crosshair), "the engine is untouched");
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
        assert!(c.always_run(), "on by default: it's a shared control now, not id's 200");
        c.set_always_run(false);
        assert_eq!((c.cl_forwardspeed, c.cl_backspeed), (200.0, 200.0));
        let mut out = String::new();
        write_changes(&c, &Cvars::classic(), &mut out);
        assert_eq!(out, "cl_forwardspeed \"200\"\ncl_backspeed \"200\"\nm_pitch \"-0.022\"\n");
    }

    /// The perspective span is slop's 8 (id's portable C), Classic's id's 16:
    /// the departure a player changes in slop is the one `config.cfg` then
    /// writes — exact (1) included, which is a choice now. Its values are the
    /// six spans; any other number is the longest span not longer than it,
    /// and below 1 (0, a word) id's 16. (Show FPS, the other old "extra", is
    /// the one slop leaves off.)
    #[test]
    fn the_perspective_span_is_8_in_slop_and_ids_16_in_classic() {
        let (id, slop) = (Cvars::classic(), Cvars::slop());
        let c = find("r_perspspan").expect("the cvar");
        assert!(c.departure && c.archive);
        assert_eq!((c.get(&id), c.get(&slop)), ("16".into(), "8".into()));
        assert_eq!(slop.persp_span, PerspSpan::Spans8, "id's own D_DrawSpans8");
        let fps = find("wasm_showfps").expect("the cvar");
        assert_eq!((fps.get(&id), fps.get(&slop)), ("0".into(), "0".into()));
        let mut spans = Cvars::slop();
        for (set, now) in [("8", "8"), ("4", "4"), ("16", "16"), ("1", "1"), ("32", "32"), ("64", "64"), ("12", "8"),
                           ("40", "32"), ("100", "64"), ("5", "4"), ("2", "1"), ("0", "16"), ("junk", "16"), ("-4", "16")] {
            c.set(&mut spans, set);
            assert_eq!(c.get(&spans), now, "r_perspspan {set}");
        }
        c.set(&mut spans, "4");
        let mut out = String::new();
        write_changes(&spans, &Cvars::slop(), &mut out);
        assert_eq!(out, "r_perspspan \"4\"\n");
        c.set(&mut spans, "1");
        out.clear();
        write_changes(&spans, &Cvars::slop(), &mut out);
        assert_eq!(out, "r_perspspan \"1\"\n", "exact is a choice in slop now: written");
        c.set(&mut spans, "8");
        out.clear();
        write_changes(&spans, &Cvars::slop(), &mut out);
        assert_eq!(out, "", "slop's own 8 is not written: a player who never touched it gets the preset's");
        assert_eq!(complete("r_persp"), Some("r_perspspan"));
    }

    /// `host_maxfps` is 2026-10-03's frame-rate cap; `wasm_uncapped`, the
    /// on/off before it, still sets it from a saved config (1 none, 0 id's
    /// 72), reads 1 only with no cap, and nothing writes or lists it.
    #[test]
    fn wasm_uncapped_is_the_caps_two_ends() {
        let (id, slop) = (Cvars::classic(), Cvars::slop());
        let cap = find("host_maxfps").expect("the cvar");
        assert!(cap.departure && cap.archive);
        assert_eq!((cap.get(&id), cap.get(&slop)), ("72".into(), "0".into()));
        let old = find("WASM_UNCAPPED").expect("an old config still finds it");
        let mut c = Cvars::slop();
        old.set(&mut c, "0");
        assert_eq!((c.max_fps, old.get(&c).as_str()), (FrameCap::ID, "0"), "a slop player's saved 0: id's 72");
        let mut out = String::new();
        write_changes(&c, &Cvars::slop(), &mut out);
        assert_eq!(out, "host_maxfps \"72\"\n", "the next save writes the cap");
        old.set(&mut c, "1");
        assert_eq!((c.max_fps, old.get(&c).as_str()), (FrameCap::NONE, "1"));
        cap.set(&mut c, "60");
        assert_eq!(old.get(&c), "0", "60 is a cap");
        assert!(CVARS.iter().all(|v| v.name != "wasm_uncapped") && complete("wasm_u").is_none());
    }

    /// `wasm_exactpersp`, the on/off before the span: a saved config's line
    /// still sets it (1 exact, 0 id's 16), it reads 1 only while the span is
    /// 1, and nothing writes, completes or lists it any more.
    #[test]
    fn wasm_exactpersp_is_the_spans_two_ends() {
        let old = find("WASM_EXACTPERSP").expect("an old config still finds it");
        assert_eq!(old.name, "wasm_exactpersp");
        let mut c = Cvars::classic();
        old.set(&mut c, "1");
        assert_eq!((c.persp_span, old.get(&c).as_str()), (PerspSpan::Exact, "1"), "a Classic player's saved 1: exact, as before");
        let mut out = String::new();
        write_changes(&c, &Cvars::classic(), &mut out);
        assert_eq!(out, "r_perspspan \"1\"\n", "the next save writes the span");
        let mut c = Cvars::slop();
        assert_eq!(old.get(&c), "0", "slop's own 8 is not exact");
        old.set(&mut c, "0");
        assert_eq!((c.persp_span, old.get(&c).as_str()), (PerspSpan::Spans16, "0"), "a slop player's saved 0: id's spans, as before");
        old.set(&mut c, "1");
        assert_eq!((c.persp_span, old.get(&c).as_str()), (PerspSpan::Exact, "1"), "a saved 1: exact");
        for (span, reads) in [(PerspSpan::Spans64, "0"), (PerspSpan::Spans32, "0"), (PerspSpan::Spans8, "0"), (PerspSpan::Spans4, "0"), (PerspSpan::Exact, "1")] {
            c.persp_span = span;
            assert_eq!(old.get(&c), reads, "{span:?}");
        }
        assert_eq!(complete("wasm_ex"), None, "completion offers only the names in use");
        assert!(CVARS.iter().all(|v| v.name != "wasm_exactpersp"), "not listed, not written, not a preset's");
    }

    #[test]
    fn an_old_name_sets_the_renamed_cvar_and_only_the_new_one_is_written() {
        let v = find("VID_FKEY").expect("a config.cfg from before the rename still finds it");
        assert_eq!(v.name, "vid_altenter");
        let mut c = Cvars::slop();
        v.set(&mut c, "0");
        assert!(!c.alt_enter);
        let mut out = String::new();
        write_changes(&c, &Cvars::slop(), &mut out);
        assert_eq!(out, "vid_altenter \"0\"\n", "the next save writes the new name");
        assert_eq!(complete("vid_f"), None, "completion offers only the names in use");
        for (old, new) in OLD_NAMES {
            assert!(CVARS.iter().all(|c| !c.name.eq_ignore_ascii_case(old)), "{old} is not a name in use");
            assert!(CVARS.iter().any(|c| c.name == *new), "{old} leads to a cvar");
        }
    }
}
