//! The menus: main, single player, load/save, options, keys, video, help, quit.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/menu.c` — the `M_*_Draw` / `M_*_Key` pairs, `M_Print`,
//! `M_DrawSlider`, `bindnames`; the video list is `vid_win.c`'s `VID_MenuDraw`.

use crate::draw::{
    blit_qpic_at, draw_char_scaled, draw_string_scaled, fade_screen, screen_2d,
    MENU_VIRT_W,
};
use crate::keys::{default_bindings, keynum_to_string, K_ESCAPE};
use crate::render::Image;
use crate::screen::{center_string_top, VIEWSIZE_DEFAULT, VIEWSIZE_MAX, VIEWSIZE_MIN, VIEWSIZE_STEP};

// ---------------------------------------------------------------------------
// Main menu (a port of menu.c: M_Main_Draw/_Key, M_SinglePlayer_Draw/_Key)
// ---------------------------------------------------------------------------
//
// Quake boots INTO this menu (the id logo over the demo loop). It is drawn with
// `menu.c`'s coordinates, centred across the top of the screen as `M_DrawPic`
// centres it, on top of the finished game frame, with index-255 transparent
// blits. Navigation is keyboard-only: up/down move a
// 6-frame animated cursor, Enter selects, Escape backs out (or, on the main
// screen, closes the menu).
//
// Faithfulness: the coordinates here are lifted verbatim from `M_Main_Draw` /
// `M_SinglePlayer_Draw` — qplaque at (16,4), the centered title at y=4, the item
// list at (72,32), the cursor at (54, 32 + cursor*20), cursor frame
// `(int)(host_time*10) % 6`. The item *counts* (`MAIN_ITEMS = 5`,
// `SINGLEPLAYER_ITEMS = 3`) and the cursor wrap come straight from `M_Main_Key` /
// `M_SinglePlayer_Key`. Selecting Single Player -> New Game maps to the C's
// `map start` (here we start `e1m1`, the shareware first level).
//
// Safety: every pic is an `Option<Qpic>` in [`MenuPics`]; a missing pic is simply
// skipped (no panic). All blits clip at the framebuffer edges.

/// `MAIN_ITEMS` (menu.c): the main menu has 5 entries.
const MAIN_ITEMS: usize = 5;
/// `SINGLEPLAYER_ITEMS` (menu.c): the single-player menu has 3 entries.
const SINGLEPLAYER_ITEMS: usize = 3;
/// `OPTIONS_ITEMS` (menu.c): id's 13 rows plus this port's one. The row
/// indices 0..=12 match `M_AdjustSliders` / `M_Options_Key`:
/// 0 Customize controls, 1 Go to console, 2 Reset to defaults, 3 Screen size,
/// 4 Brightness, 5 Mouse Speed, 6 CD Music Volume, 7 Sound Volume, 8 Always Run,
/// 9 Invert Mouse, 10 Lookspring, 11 Lookstrafe, 12 Video Options. Row 13 is
/// the port's "Web extras" ([`ROW_EXTRAS`]), in the slot the C's own `_WIN32`
/// build gives its 14th row ("Use Mouse", y=136, `OPTIONS_ITEMS 14`).
const OPTIONS_ITEMS: usize = 14;

/// `OptionRow` — the stable index for each Options row (matches the C's
/// `options_cursor` cases in `M_AdjustSliders` / `M_Options_Key`).
const ROW_CONTROLS: usize = 0;
const ROW_CONSOLE: usize = 1;
const ROW_DEFAULTS: usize = 2;
const ROW_SCREENSIZE: usize = 3;
const ROW_BRIGHTNESS: usize = 4;
const ROW_MOUSESPEED: usize = 5;
const ROW_CDVOLUME: usize = 6;
const ROW_SNDVOLUME: usize = 7;
const ROW_ALWAYSRUN: usize = 8;
const ROW_INVERTMOUSE: usize = 9;
const ROW_LOOKSPRING: usize = 10;
const ROW_LOOKSTRAFE: usize = 11;
const ROW_VIDEO: usize = 12;
/// PORT ROW (not in id's Quake): "Web extras" opens [`MenuScreen::Extras`],
/// the one home of this port's opt-in departures ([`Extras`]).
const ROW_EXTRAS: usize = 13;

// ---------------------------------------------------------------------------
// Web extras: the port's opt-in departures from id's Quake
// ---------------------------------------------------------------------------

/// The port's opt-in departures from id's Quake (Options > Web extras, and
/// the `wasm_*` console commands). Every one defaults OFF: with all of them
/// off the port behaves as id's Quake (Always Run aside). They are not cvars
/// in default.cfg, so "Reset to defaults" leaves them alone; the page
/// persists them in localStorage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extras {
    /// `wasm_uncapped`: run a host frame on every display refresh instead of
    /// `Host_FilterTime`'s 72 fps cap (a 120/144 Hz display runs 120/144 fps).
    pub uncapped: bool,
    /// `wasm_showfps`: a frame-rate readout in conchars at the bottom right,
    /// above the status bar (QuakeWorld's `SCR_DrawFPS`).
    pub show_fps: bool,
    /// `wasm_exactpersp`: textured walls and liquids with exact perspective
    /// at every pixel instead of id's 16-pixel affine spans.
    pub exact_persp: bool,
    /// `wasm_scaled2d`: the 2-D layer (status bar, menus, console, text)
    /// blown up from a 320x200 screen to fill the frame, instead of id's 1:1
    /// pixels at every resolution ([`crate::draw::set_scaled_2d`]).
    pub scaled_2d: bool,
}

/// One Web extra (a row of [`WEB_EXTRAS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extra {
    Uncapped,
    ShowFps,
    ExactPersp,
    Scaled2d,
}

impl Extras {
    /// The bits the page stores (the `extras`/`set_extras` exports):
    /// 1 uncapped, 2 show FPS, 4 exact perspective, 8 scaled 2-D.
    pub fn bits(self) -> u32 {
        self.uncapped as u32
            | (self.show_fps as u32) << 1
            | (self.exact_persp as u32) << 2
            | (self.scaled_2d as u32) << 3
    }

    /// The inverse of [`Extras::bits`]; unknown bits are ignored.
    pub fn from_bits(bits: u32) -> Extras {
        Extras {
            uncapped: bits & 1 != 0,
            show_fps: bits & 2 != 0,
            exact_persp: bits & 4 != 0,
            scaled_2d: bits & 8 != 0,
        }
    }

    /// Whether `e` is on.
    pub fn get(self, e: Extra) -> bool {
        match e {
            Extra::Uncapped => self.uncapped,
            Extra::ShowFps => self.show_fps,
            Extra::ExactPersp => self.exact_persp,
            Extra::Scaled2d => self.scaled_2d,
        }
    }

    /// Switch `e` on or off.
    pub fn set(&mut self, e: Extra, on: bool) {
        match e {
            Extra::Uncapped => self.uncapped = on,
            Extra::ShowFps => self.show_fps = on,
            Extra::ExactPersp => self.exact_persp = on,
            Extra::Scaled2d => self.scaled_2d = on,
        }
    }
}

/// One Web extra, as the Options > Web extras page and the console know it.
#[derive(Debug, Clone, Copy)]
pub struct WebExtra {
    /// The value it switches.
    pub extra: Extra,
    /// Its console variable (`wasm_*`, a name no id command or cvar uses).
    pub cvar: &'static str,
    /// Its row label, right-justified to the Options label column like id's.
    pub label: &'static str,
    /// The two bronze help lines shown under the list while its row is
    /// highlighted (a third names the console variable).
    pub help: [&'static str; 2],
    /// Its one-line summary in the console's `help`.
    pub summary: &'static str,
}

/// THE table of the port's opt-in extras, in Extras-page order: the page's
/// rows and the console's `wasm_*` variables are both read from it, and
/// their values are the menu's [`Extras`].
pub const WEB_EXTRAS: [WebExtra; 4] = [
    WebExtra {
        extra: Extra::Uncapped,
        cvar: "wasm_uncapped",
        label: "    Uncapped framerate",
        help: ["One frame every display refresh,", "past Quake's 72 fps cap"],
        summary: "no 72 fps cap",
    },
    WebExtra {
        extra: Extra::ShowFps,
        cvar: "wasm_showfps",
        label: "              Show FPS",
        help: ["Frames per second, bottom right,", "as QuakeWorld's show_fps drew it"],
        summary: "frame rate",
    },
    WebExtra {
        extra: Extra::ExactPersp,
        cvar: "wasm_exactpersp",
        label: "     Exact perspective",
        help: ["Perspective exact at every pixel,", "not id's 16-pixel spans"],
        summary: "exact persp.",
    },
    WebExtra {
        extra: Extra::Scaled2d,
        cvar: "wasm_scaled2d",
        label: "      Scaled 2-D layer",
        help: ["Status bar, menus and text blown", "up from 320x200 to fill the screen"],
        summary: "scaled 2-D layer",
    },
];

/// `MULTIPLAYER_ITEMS` (menu.c): the multiplayer menu has 3 entries (Join /
/// New Game / Setup). Netcode is out of scope for this port, so like the C with
/// no net drivers, Enter on Join/New Game does nothing and the screen shows the
/// "No Communications Available" line (`M_MultiPlayer_Draw`).
const MULTIPLAYER_ITEMS: usize = 3;

/// `MAX_SAVEGAMES` (quakedef.h): the Load/Save menus list 12 slots.
pub const MAX_SAVEGAMES: usize = 12;
/// The text `M_ScanSaves` (menu.c) puts in every slot without an `sN.sav` file.
/// A slot whose host-set comment is empty shows exactly this.
pub const UNUSED_SLOT: &str = "--- UNUSED SLOT ---";

// --- key bindings (menu.c M_Keys_*, keys.h/keys.c) -----------------------------

/// `bindnames` (menu.c): the (command, label) rows `M_Keys_Draw` lists, verbatim.
pub const BINDNAMES: [(&str, &str); NUM_BINDNAMES] = [
    ("+attack", "attack"),
    ("impulse 10", "change weapon"),
    ("+jump", "jump / swim up"),
    ("+forward", "walk forward"),
    ("+back", "backpedal"),
    ("+left", "turn left"),
    ("+right", "turn right"),
    ("+speed", "run"),
    ("+moveleft", "step left"),
    ("+moveright", "step right"),
    ("+strafe", "sidestep"),
    ("+lookup", "look up"),
    ("+lookdown", "look down"),
    ("centerview", "center view"),
    ("+mlook", "mouse look"),
    ("+klook", "keyboard look"),
    ("+moveup", "swim up"),
    ("+movedown", "swim down"),
];
/// `NUMCOMMANDS` (menu.c): the bindnames row count.
pub const NUM_BINDNAMES: usize = 18;

/// Indices into [`BINDNAMES`] for the commands the host actually drives (the
/// rest are list-only: `+mlook` is permanent under pointer lock and `+klook`
/// has no effect without keyboard-look pitch — both still draw + rebind
/// faithfully).
pub const BIND_ATTACK: usize = 0;
pub const BIND_CHANGEWEAPON: usize = 1;
pub const BIND_JUMP: usize = 2;
pub const BIND_FORWARD: usize = 3;
pub const BIND_BACK: usize = 4;
pub const BIND_LEFT: usize = 5;
pub const BIND_RIGHT: usize = 6;
pub const BIND_SPEED: usize = 7;
pub const BIND_MOVELEFT: usize = 8;
pub const BIND_MOVERIGHT: usize = 9;
pub const BIND_STRAFE: usize = 10;
pub const BIND_LOOKUP: usize = 11;
pub const BIND_LOOKDOWN: usize = 12;
pub const BIND_CENTERVIEW: usize = 13;
/// `+mlook` / `+klook`: listed and bindable; mouse look is permanent under
/// pointer lock here and keyboard look is not modelled, so holding them does
/// nothing.
pub const BIND_MLOOK: usize = 14;
pub const BIND_KLOOK: usize = 15;
pub const BIND_MOVEUP: usize = 16;
pub const BIND_MOVEDOWN: usize = 17;
/// Commands `default.cfg` binds that `M_Keys_Draw` doesn't list: they sit past
/// the [`BINDNAMES`] rows, so Customize controls never shows them, but a
/// rebind over their key or `Reset to defaults` treats them like any other
/// binding. `bind + "sizeup"`, `bind = "sizeup"`, `bind - "sizedown"`.
pub const BIND_SIZEUP: usize = NUM_BINDNAMES;
pub const BIND_SIZEDOWN: usize = NUM_BINDNAMES + 1;
/// `bind TAB "+showscores"` (default.cfg): `sb_showscores` while held, so
/// `Sbar_Draw` shows the scorebar and `Sbar_SoloScoreboard`.
pub const BIND_SHOWSCORES: usize = NUM_BINDNAMES + 2;
/// `bind 0 "impulse 0"` .. `bind 8 "impulse 8"` (default.cfg): the command
/// `"impulse N"` is `BIND_IMPULSE_0 + N` for N in 0..=8 (`"impulse 10"` is the
/// listed [`BIND_CHANGEWEAPON`] row).
pub const BIND_IMPULSE_0: usize = NUM_BINDNAMES + 3;

/// The video modes the Video Options screen (`M_Video` -> `VID_MenuDraw`) lists,
/// as `(width, height)` render resolutions — this port's `modelist`. A
/// consistent 16:10 ladder (each step +160w/+100h) from the fast `320x200` up to
/// the host's `1280x800` clamp cap (`1_280*800` = the exact pixel budget). The
/// engine *boots* at the host's chosen default (see wasm `DEFAULT_W`/`DEFAULT_H`),
/// which must be one of these so the list can mark the current mode; higher modes
/// render the 3-D scene at the larger size and, as in WinQuake, draw the menu and
/// HUD at their own pixel size (the "scaled 2-D" extra,
/// [`crate::draw::set_scaled_2d`], blows them up instead). The Options "Screen size" row is id's
/// `viewsize` (see [`calc_refdef`](crate::screen::calc_refdef)), not the mode, exactly as in WinQuake.
pub const RESOLUTION_PRESETS: [(i32, i32); 7] = [
    (320, 200),
    (480, 300),
    (640, 400),
    (800, 500),
    (960, 600),
    (1120, 700),
    (1280, 800),
];

// --- analog cvar ranges (M_AdjustSliders) + their slider fraction mapping ------

/// `sensitivity` (Mouse Speed): 1..=11, step 0.5; slider r = (v-1)/10. Default 3.
const SENS_MIN: f32 = 1.0;
const SENS_MAX: f32 = 11.0;
const SENS_STEP: f32 = 0.5;
const SENS_DEFAULT: f32 = 3.0;

/// `volume` (Sound Volume): 0..=1, step 0.1; slider r = v. Default 0.7.
const VOLUME_MIN: f32 = 0.0;
const VOLUME_MAX: f32 = 1.0;
const VOLUME_STEP: f32 = 0.1;
const VOLUME_DEFAULT: f32 = 0.7;

/// `v_gamma` (Brightness): 0.5..=1, step 0.05 (RIGHT brightens: the C does
/// `v_gamma.value -= dir * 0.05`); slider r = (1 - v)/0.5. Default 1.0. LIVE:
/// the host runs the presented frame through [`build_gamma_table`](crate::render::build_gamma_table) (the C
/// applies `gammatable` at the hardware-palette boundary,
/// `V_UpdatePalette` -> `VID_ShiftPalette`); 1.0 is a byte-exact identity.
const GAMMA_MIN: f32 = 0.5;
const GAMMA_MAX: f32 = 1.0;
const GAMMA_STEP: f32 = 0.05;
const GAMMA_DEFAULT: f32 = 1.0;

/// `bgmvolume` (CD Music Volume): 0..=1, step 0.1; slider r = v. Default 1.0.
/// The slider is live (stores the cvar, exposed via [`Menu::bgm_volume`]).
/// DEVIATION (scope): there is no CD audio device in this port, so no track
/// ever plays at this volume — exactly like the C run without a CD, where the
/// cvar still adjusts (cd_null.c).
const BGM_MIN: f32 = 0.0;
const BGM_MAX: f32 = 1.0;
const BGM_STEP: f32 = 0.1;
const BGM_DEFAULT: f32 = 1.0;

/// `NUM_HELP_PAGES` (menu.c): the Help/Ordering screen pages through
/// `gfx/help0.lmp`..`help5.lmp`.
pub const NUM_HELP_PAGES: usize = 6;

/// The map New Game starts on. Matches the C `map start`: `start.bsp` is the
/// skill-select hub — the player walks into the Easy/Normal/Hard/Nightmare halls
/// (`trigger_setskill`) and an episode slipgate (`trigger_changelevel`) that
/// changelevels into `e1m1` (or e2m1/e3m1/e4m1). Changelevel is implemented, so
/// the full hub flow works.
pub const NEW_GAME_MAP: &str = "maps/start.bsp";

// --- conchars glyph indices used by the Options widgets (M_DrawSlider /
//     M_DrawCheckbox) and the cursor (M_Options_Draw). ----------------------------

/// `M_DrawSlider`: the slider trough is glyph 128 (left cap), 129 (the middle
/// segment, repeated [`SLIDER_RANGE`] times), then 130 (right cap); the knob is
/// glyph 131. The whole widget is drawn at virtual x=220.
const SLIDER_LEFT_CHAR: u8 = 128;
const SLIDER_MID_CHAR: u8 = 129;
const SLIDER_RIGHT_CHAR: u8 = 130;
const SLIDER_KNOB_CHAR: u8 = 131;
/// `SLIDER_RANGE` (menu.c): the slider trough is 10 middle segments wide.
const SLIDER_RANGE: usize = 10;

/// The Options cursor: glyph `12 + ((realtime*4)&1)` (12/13 flash) drawn at
/// virtual x=200 (`M_DrawCharacter(200, 32 + cursor*8, 12 + ...)`).
const OPTIONS_CURSOR_BASE: u8 = 12;
const OPTIONS_CURSOR_X: f32 = 200.0;

/// `(int)(realtime*rate) & 1` — the C's cursor-flash bit, shared by the menu
/// cursors (`rate` 4) and the console input cursor (`con_cursorspeed` 4). The C
/// truncates the double product toward zero; `realtime` only grows, so a
/// non-finite or non-positive clock is phase 0.
pub(crate) fn realtime_blink_bit(realtime: f64, rate: f64) -> u8 {
    if !realtime.is_finite() || realtime <= 0.0 {
        return 0;
    }
    ((realtime * rate) as i64 & 1) as u8
}

/// The flashing menu cursor's conchars cell: `12 + ((int)(realtime*4) & 1)`,
/// verbatim from every text menu that has one (`M_Options_Draw`,
/// `M_Load_Draw`/`M_Save_Draw`, `M_Keys_Draw`, vid_win.c `VID_MenuDraw`). Glyph
/// 12 is blank and 13 is the arrow, so the cursor is visible for a quarter
/// second out of every half second: a 4 Hz toggle on REAL time. (The menudot
/// spinner is the one menu animation on `host_time` — 10 Hz, see [`draw_menu`].)
pub fn menu_cursor_glyph(realtime: f64) -> u8 {
    OPTIONS_CURSOR_BASE + realtime_blink_bit(realtime, 4.0)
}

/// Which menu screen is showing. Mirrors the relevant `m_state` values from
/// menu.c (`m_main`, `m_singleplayer`, `m_load`, `m_save`, `m_multiplayer`,
/// `m_options`, `m_keys`, `m_video`, `m_help`, `m_quit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuScreen {
    /// The top-level menu (`m_main`): Single Player / Multiplayer / Options /
    /// Help / Quit.
    Main,
    /// The single-player submenu (`m_singleplayer`): New Game / Load / Save.
    SinglePlayer,
    /// The load-game slot list (`m_load`): [`MAX_SAVEGAMES`] rows showing the
    /// host-set slot comments (empty = [`UNUSED_SLOT`]). Enter on a loadable
    /// slot emits [`MenuAction::LoadSlot`]; on an unused slot it does nothing —
    /// the C's `M_Load_Key` returns when `!loadable[cursor]`.
    Load,
    /// The save-game slot list (`m_save`): Enter emits [`MenuAction::SaveSlot`]
    /// for the highlighted slot (`M_Save_Key`). Opening it is refused while no
    /// local game is running (`M_Menu_Save_f`'s `!sv.active` check, mapped to
    /// [`Menu::set_game_active`]).
    Save,
    /// The multiplayer submenu (`m_multiplayer`): Join / New Game / Setup over
    /// the `mp_menu` art. Netcode is out of scope, so — like the C with zero
    /// net drivers — Join/New Game don't respond and the screen shows
    /// "No Communications Available".
    Multiplayer,
    /// The options submenu (`m_options`): the full 13-row layout
    /// ([`OPTIONS_ITEMS`]). Sliders + checkboxes are adjusted with left/right.
    Options,
    /// The Customize-controls screen (`m_keys`): the [`BINDNAMES`] list with a
    /// cursor; Enter grabs the next key to rebind (`bind_grab`), Backspace/Del
    /// unbind (`M_Keys_Key`).
    Keys,
    /// The video-modes screen (`m_video`): this port's mode list is
    /// [`RESOLUTION_PRESETS`]; cursor + Enter applies a mode
    /// ([`MenuAction::ResolutionChanged`]), like `VID_MenuKey`'s K_ENTER
    /// `VID_SetMode` (vid_win.c).
    Video,
    /// The Help/Ordering screen (`m_help`): pages through
    /// `gfx/help0.lmp`..`help5.lmp` with left/right ([`NUM_HELP_PAGES`] pages).
    Help,
    /// The Quit confirmation prompt (`m_quit`): "Are you sure you want to quit?".
    Quit,
    /// PORT SCREEN (not in id's Quake): Options > Web extras, the port's
    /// opt-in departures ([`Extras`]) as on/off rows drawn in `M_Options_Draw`'s
    /// idiom. Left/right/Enter toggle (`M_AdjustSliders`' checkbox rows);
    /// Escape returns to Options on the Web extras row.
    Extras,
}

impl MenuScreen {
    /// The number of selectable items on this screen (the cursor wraps within it).
    /// Help/Quit have no cursor list (1 item — the screen itself) so up/down are
    /// inert there; Help pages with left/right, Quit answers Y/N.
    fn item_count(self) -> usize {
        match self {
            MenuScreen::Main => MAIN_ITEMS,
            MenuScreen::SinglePlayer => SINGLEPLAYER_ITEMS,
            MenuScreen::Load | MenuScreen::Save => MAX_SAVEGAMES,
            MenuScreen::Multiplayer => MULTIPLAYER_ITEMS,
            MenuScreen::Options => OPTIONS_ITEMS,
            MenuScreen::Keys => NUM_BINDNAMES,
            MenuScreen::Video => RESOLUTION_PRESETS.len(),
            MenuScreen::Extras => WEB_EXTRAS.len(),
            MenuScreen::Help | MenuScreen::Quit => 1,
        }
    }
}

/// One queued `S_LocalSound` from the menu (menu.c). The host drains these via
/// [`Menu::take_sounds`] and plays each like the C's `S_LocalSound` — a
/// view-entity sound at full volume, centred, no distance falloff
/// (`S_StartSound(cl.viewentity, -1, sfx, vec3_origin, 1, 1)`, snd_dma.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSound {
    /// `misc/menu1.wav` — cursor movement (and the keys-menu bind grab,
    /// `M_Keys_Key`; and every `VID_MenuKey` press).
    Menu1,
    /// `misc/menu2.wav` — `m_entersound`: entering a screen / Enter select.
    /// (The C latches the flag and plays it on the next `M_Draw` so pic caching
    /// can't stutter the sample; with pre-decoded Web Audio buffers that delay
    /// is unnecessary, so this port queues it at the trigger — EXCEPT where the
    /// menu closes before the next draw, where the C's latch never fires and we
    /// queue nothing.)
    Menu2,
    /// `misc/menu3.wav` — `M_AdjustSliders` (any Options left/right/Enter-adjust).
    Menu3,
}

impl MenuSound {
    /// The sample path relative to `sound/` (the form QuakeC sample names take;
    /// `S_LocalSound` passes exactly these strings).
    pub fn sample(self) -> &'static str {
        match self {
            MenuSound::Menu1 => "misc/menu1.wav",
            MenuSound::Menu2 => "misc/menu2.wav",
            MenuSound::Menu3 => "misc/menu3.wav",
        }
    }
}

/// The most local-sounds the menu queues between host drains: a bound on
/// [`Menu::take_sounds`]'s backlog so spamming menu keys without a running
/// `step` loop can't grow the queue without bound.
const MENU_SOUND_CAP: usize = 16;

/// What pressing Enter (or the menu closing) asks the host to do. The wasm/tool
/// front-end turns these into engine actions (e.g. [`MenuAction::NewGame`]
/// rebuilds the walk on [`NEW_GAME_MAP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do (the selection only changed the screen, or — like the C —
    /// the item doesn't respond: Load on an unused slot, Multiplayer Join with
    /// no net drivers).
    None,
    /// Enter on a loadable Load slot (`M_Load_Key` K_ENTER: `load sN`). The
    /// menu already closed (`m_state = m_none; key_dest = key_game`); the host
    /// dispatches the actual load.
    LoadSlot(usize),
    /// Enter on a Save slot (`M_Save_Key` K_ENTER: `save sN`). The menu already
    /// closed; the host dispatches the actual save.
    SaveSlot(usize),
    /// Start a fresh single-player game on [`NEW_GAME_MAP`] and close the menu.
    NewGame,
    /// Backed out of a submenu to the main screen (Escape on a submenu).
    Back,
    /// The menu just closed (Escape on the main screen, or confirmed Quit).
    Closed,
    /// Options "Go to console": the host should close the menu and open the
    /// drop-down console (`m_state = m_none; Con_ToggleConsole_f()`).
    OpenConsole,
    /// Options "Reset to defaults": the host should reset the option cvars
    /// (`exec default.cfg`). [`Menu::select`] already reset the in-menu values
    /// (default.cfg's viewsize, gamma, volume, sensitivity and bindings); the
    /// host reads them live each frame. The video mode is not in default.cfg.
    ResetDefaults,
    /// Enter on a Video Options mode line (`VID_MenuKey` K_ENTER -> `VID_SetMode`):
    /// the host must reallocate its framebuffer to [`Menu::resolution`].
    ResolutionChanged,
}

/// The keyboard-driven main-menu engine: the visible flag, the current screen,
/// and the cursor index within it. A port of menu.c's `m_state` + the
/// `m_*_cursor` globals, scoped to an instance rather than file-statics.
///
/// The host calls [`open`](Menu::open)/[`close`](Menu::close)/[`toggle`](Menu::toggle)
/// to show/hide it, [`move_cursor`](Menu::move_cursor) on up/down, and
/// [`select`](Menu::select)/[`cancel`](Menu::cancel) on Enter/Escape; the returned
/// [`MenuAction`] tells the host what to do. [`draw_menu`] renders the current
/// state.
#[derive(Debug, Clone)]
pub struct Menu {
    /// Whether the menu is showing (drawn + capturing input). Quake's `key_dest ==
    /// key_menu`.
    pub visible: bool,
    /// The screen currently displayed.
    screen: MenuScreen,
    /// The highlighted item index on the current screen (`0..item_count`).
    cursor: usize,
    /// Index into [`RESOLUTION_PRESETS`] of the live video mode (`vid_modenum`):
    /// what the Video Options list marks as current and opens its cursor on.
    /// The host keeps it synced to the real framebuffer
    /// ([`sync_resolution`](Menu::sync_resolution)); Enter on the Video list sets it.
    res_preset: usize,
    /// `viewsize` cvar (`scr_viewsize`), [`VIEWSIZE_MIN`]..=[`VIEWSIZE_MAX`]:
    /// the host frames the 3-D view with it ([`calc_refdef`](crate::screen::calc_refdef)).
    viewsize: f32,
    /// `sensitivity` cvar (Mouse Speed), [`SENS_MIN`]..=[`SENS_MAX`].
    sensitivity: f32,
    /// `volume` cvar (Sound Volume), [`VOLUME_MIN`]..=[`VOLUME_MAX`]. The host maps
    /// it to a 0.0..=1.0 master gain.
    volume: f32,
    /// `v_gamma` cvar (Brightness), [`GAMMA_MIN`]..=[`GAMMA_MAX`]. LIVE: the
    /// host runs the presented frame through [`build_gamma_table`](crate::render::build_gamma_table) with this
    /// (a byte-exact identity at the default 1.0).
    gamma: f32,
    /// `bgmvolume` cvar (CD Music Volume), [`BGM_MIN`]..=[`BGM_MAX`]. Live cvar;
    /// no CD audio exists to play at it (see the [`BGM_DEFAULT`] DEVIATION note).
    bgm_volume: f32,
    /// `cl_forwardspeed > 200` (Always Run). LIVE: the host swaps
    /// cl_forwardspeed/cl_backspeed 200 <-> 400 on it (M_AdjustSliders case 8).
    /// DEVIATION: defaults ON in this port (id's default.cfg leaves
    /// cl_forwardspeed at 200, i.e. off) — nobody wants to walk.
    always_run: bool,
    /// `m_pitch < 0` (Invert Mouse). LIVE: the host flips the mouse-pitch sign
    /// (in_win.c IN_MouseMove: `cl.viewangles[PITCH] += m_pitch.value * mouse_y`).
    invert_mouse: bool,
    /// `lookspring` cvar. LIVE: the C re-centres pitch when `+mlook` releases
    /// (`IN_MLookUp` -> `V_StartPitchDrift`, cl_input.c); this port's mouse-look
    /// is permanent while the pointer is locked, so the host maps the mlook
    /// RELEASE onto pointer-unlock (leaving pointer lock re-centres pitch).
    lookspring: bool,
    /// `lookstrafe` cvar. LIVE: while mouse-looking (always, under pointer
    /// lock), mouse X becomes strafe instead of yaw (in_win.c IN_MouseMove).
    lookstrafe: bool,
    /// The current Help page (`help_page`, `0..NUM_HELP_PAGES`).
    help_page: usize,
    /// Which screen the Quit prompt was raised from, restored on "No"
    /// (`m_quit_prevstate` / `wasInMenus`).
    quit_prev: MenuScreen,
    /// `wasInMenus`: the prompt rose over a menu (drawn under it, and "No"
    /// returns to it) rather than over the game.
    quit_in_menus: bool,
    /// `msgNumber`: which of the eight [`QUIT_MESSAGES`] the prompt shows,
    /// `rand()&7` each time it opens.
    quit_msg: usize,
    /// The state of the C library `rand()` [`Menu::open_quit`] draws from.
    quit_rand: u32,
    /// The Keys screen is waiting for the next key to bind (`bind_grab`,
    /// `M_Keys_Key`). The host routes raw keys to [`Menu::bind_key`] while set.
    bind_grab: bool,
    /// `keybindings[256]` (keys.c), as keynum -> [`BINDNAMES`] index. The menu
    /// owns the table; the host queries [`Menu::action_for_key`] to drive input.
    bindings: [Option<u8>; 256],
    /// Queued `S_LocalSound`s (menu1/menu2/menu3), drained by
    /// [`Menu::take_sounds`]. Capped at [`MENU_SOUND_CAP`].
    sounds: Vec<MenuSound>,
    /// The Load/Save slot comments (`m_filenames` from `M_ScanSaves`), host-set
    /// via [`Menu::set_save_comments`]. An empty string = unused slot (draws
    /// [`UNUSED_SLOT`], not loadable). All empty until a savegame engine fills
    /// them.
    save_comments: [String; MAX_SAVEGAMES],
    /// Whether a local single-player game is running (`sv.active &&
    /// !cl.intermission && svs.maxclients == 1`, the `M_Menu_Save_f` gate). The
    /// host keeps it current via [`Menu::set_game_active`]; while false the
    /// Save screen refuses to open.
    game_active: bool,
    /// `sv.active`: a local server is running (the live walk, intermission
    /// included). Host-set via [`Menu::set_server_active`]; New Game asks
    /// first while it is.
    server_active: bool,
    /// SCR_ModalMessage("Are you sure you want to\nstart a new game?\n") is up
    /// (`M_SinglePlayer_Key`'s New Game with `sv.active`): only Y (yes), N and
    /// Escape (no) answer it; the menu is not drawn, only the faded screen and
    /// the question.
    new_game_confirm: bool,
    /// The port's opt-in departures (Options > Web extras), all off by
    /// default. Like the Options cvars they survive navigation resets; unlike
    /// them no default.cfg line resets them.
    extras: Extras,
}

impl Default for Menu {
    fn default() -> Self {
        Menu::new()
    }
}

impl Menu {
    /// A closed menu sitting on the main screen with the cursor on the first item,
    /// with the Options cvars at their id defaults — except Always Run, which
    /// this port defaults ON (see the field's DEVIATION note).
    pub fn new() -> Menu {
        Menu {
            visible: false,
            screen: MenuScreen::Main,
            cursor: 0,
            res_preset: 0,
            viewsize: VIEWSIZE_DEFAULT,
            sensitivity: SENS_DEFAULT,
            volume: VOLUME_DEFAULT,
            gamma: GAMMA_DEFAULT,
            bgm_volume: BGM_DEFAULT,
            always_run: true,
            invert_mouse: false,
            lookspring: false,
            lookstrafe: false,
            help_page: 0,
            quit_prev: MenuScreen::Main,
            quit_in_menus: true,
            quit_msg: 0,
            quit_rand: 1,
            bind_grab: false,
            bindings: default_bindings(),
            sounds: Vec::new(),
            save_comments: Default::default(),
            game_active: false,
            server_active: false,
            new_game_confirm: false,
            extras: Extras::default(),
        }
    }

    /// Queue one `S_LocalSound` for the host to drain (bounded; a host that
    /// never drains can't leak).
    fn snd(&mut self, s: MenuSound) {
        if self.sounds.len() < MENU_SOUND_CAP {
            self.sounds.push(s);
        }
    }

    /// Drain the queued menu local-sounds (menu1/menu2/menu3), in fire order.
    /// The host plays each per `S_LocalSound` semantics (full volume, centred,
    /// no attenuation — see [`MenuSound`]).
    pub fn take_sounds(&mut self) -> Vec<MenuSound> {
        std::mem::take(&mut self.sounds)
    }

    /// Tell the menu whether a local single-player game is running — the
    /// `M_Menu_Save_f` gate (`!sv.active || cl.intermission || svs.maxclients
    /// != 1` all refuse). The host refreshes this every frame; Save refuses to
    /// open while false.
    pub fn set_game_active(&mut self, active: bool) {
        self.game_active = active;
    }

    /// Tell the menu whether a local server runs (`sv.active`): New Game then
    /// asks "Are you sure?" first (`M_SinglePlayer_Key`). Refreshed every frame.
    pub fn set_server_active(&mut self, active: bool) {
        self.server_active = active;
    }

    /// The New Game "Are you sure?" modal is up (see [`Menu::quit_yes`]).
    pub fn new_game_confirm(&self) -> bool {
        self.new_game_confirm
    }

    /// Set the 12 Load/Save slot comments (`M_ScanSaves`' `m_filenames`): the
    /// host's savegame engine fills these from the `sN.sav` headers; an empty
    /// string marks the slot unused ([`UNUSED_SLOT`] is drawn, Enter on Load
    /// refuses it).
    pub fn set_save_comments(&mut self, comments: [String; MAX_SAVEGAMES]) {
        self.save_comments = comments;
    }

    /// Set one slot's comment (the host refreshes slots individually as the
    /// page reads each stored savegame out of localStorage). Out-of-range is
    /// ignored; an empty string marks the slot unused.
    pub fn set_save_comment(&mut self, i: usize, comment: String) {
        if let Some(c) = self.save_comments.get_mut(i) {
            *c = comment;
        }
    }

    /// The comment for save slot `i` (empty = unused). Out-of-range is empty.
    pub fn save_comment(&self, i: usize) -> &str {
        self.save_comments.get(i).map(String::as_str).unwrap_or("")
    }

    /// Whether Load may act on slot `i` (`loadable[i]` in `M_ScanSaves`): a
    /// non-empty host-set comment means a real save exists there.
    pub fn slot_loadable(&self, i: usize) -> bool {
        !self.save_comment(i).is_empty()
    }

    /// The screen currently displayed.
    pub fn screen(&self) -> MenuScreen {
        self.screen
    }

    /// The highlighted item index on the current screen.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The current Help page index (`0..NUM_HELP_PAGES`).
    pub fn help_page(&self) -> usize {
        self.help_page
    }

    /// Open the menu on the main screen (`M_Menu_Main_f`): show it and reset to the
    /// top-level screen with the cursor on the first item. Plays the enter sound
    /// (`m_entersound = true` in the C).
    pub fn open(&mut self) {
        self.visible = true;
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        self.bind_grab = false;
        self.snd(MenuSound::Menu2);
    }

    /// Close the menu (`key_dest = key_game`). Leaves the screen/cursor as they
    /// were so a later `open` resets them.
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Reset the menu's NAVIGATION to boot state — closed, on the Main screen,
    /// cursor on the first item, no Help page / Quit return / bind grab, queued
    /// sounds dropped — while KEEPING every user choice: the Options cvars
    /// (Screen size, gamma, sensitivity, volume, CD volume, Always Run, Invert
    /// Mouse, lookspring, lookstrafe), the Web extras and the whole key-bindings table. In
    /// WinQuake a map start / New Game only restarts the server: cvars and
    /// `keybindings[]` live in host state (persisted by
    /// `Host_WriteConfiguration`) and are never reset by `map start`
    /// (`M_SinglePlayer_Key`). The host calls this at every re-boot site that
    /// used to rebuild the Menu wholesale, so "rebind keys, set Always Run,
    /// then New Game" keeps the player's setup. The host-mirrored externals —
    /// Load/Save slot comments (`set_save_comments`) and the game-active gate
    /// (refreshed every `step`) — survive too: they reflect engine state, not
    /// navigation.
    pub fn reset_nav(&mut self) {
        self.visible = false;
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        self.help_page = 0;
        self.quit_prev = MenuScreen::Main;
        self.bind_grab = false;
        self.new_game_confirm = false;
        self.sounds.clear();
    }

    /// Open the menu directly on the Help/Ordering screen (`M_Menu_Help_f`,
    /// menu.c): what the `help` console command — and therefore the shareware
    /// `svc_sellscreen` at episode end — runs. Resets to the first page
    /// (`help_page = 0`) like the C, and plays the enter sound (`m_entersound`).
    pub fn open_help(&mut self) {
        self.visible = true;
        self.screen = MenuScreen::Help;
        self.help_page = 0;
        self.cursor = 0;
        self.bind_grab = false;
        self.snd(MenuSound::Menu2);
    }

    /// Toggle the menu (`M_ToggleMenu_f`): if hidden, open on the main screen; if
    /// showing a submenu, go back to main; if already on the main screen, close.
    /// Returns the resulting [`MenuAction`] (`Closed` when it closed, else `None`).
    /// Opening / backing to main plays `m_entersound` (menu2); closing queues
    /// nothing audible (the C latches the flag but `M_Draw` never runs to fire
    /// it, and the next open re-latches it anyway).
    pub fn toggle(&mut self) -> MenuAction {
        if !self.visible {
            self.open();
            MenuAction::None
        } else if self.screen != MenuScreen::Main {
            // M_ToggleMenu_f -> M_Menu_Main_f (m_entersound = true).
            self.screen = MenuScreen::Main;
            self.cursor = 0;
            self.bind_grab = false;
            self.snd(MenuSound::Menu2);
            MenuAction::Back
        } else {
            self.close();
            MenuAction::Closed
        }
    }

    /// Move the cursor by `delta` (down = +1, up = -1), wrapping within the current
    /// screen's item count — exactly the `++/--` wrap in `M_Main_Key` /
    /// `M_Options_Key`. `delta` may be any magnitude; it wraps modulo the item
    /// count. On the Help screen up/down also page (the C maps `K_UPARROW`/
    /// `K_DOWNARROW` to page +/-); see [`page`](Menu::page).
    pub fn move_cursor(&mut self, delta: i32) {
        if self.new_game_confirm {
            return; // SCR_ModalMessage waits for y / n / Escape only.
        }
        if self.screen == MenuScreen::Help {
            // M_Help_Key: UP = next page (m_help_page++), DOWN = previous. The host
            // passes up = -1 / down = +1 (cursor convention), so negate to map up
            // onto +1 (next). Previously up went backwards. (Paging latches
            // m_entersound in the C; `page` queues the menu2.)
            self.page(-delta.signum());
            return;
        }
        if self.screen == MenuScreen::Quit {
            return; // M_Quit_Key: up/down fall through to `default: break`.
        }
        let n = self.screen.item_count();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        // Every M_*_Key cursor move plays misc/menu1.wav.
        self.snd(MenuSound::Menu1);
        let n_i = n as i32;
        // Wrap into 0..n even for large / negative deltas.
        let next = (self.cursor as i32 + delta).rem_euclid(n_i);
        self.cursor = next as usize;
    }

    /// Activate the highlighted item (Enter / `K_ENTER`).
    ///
    /// * Main > Single Player / Multiplayer / Options / Help: switch screen,
    ///   cursor reset ([`MenuAction::None`]).
    /// * Main > Quit: raise the Quit confirm prompt ([`MenuAction::None`]).
    /// * SinglePlayer > New Game: [`MenuAction::NewGame`] and close the menu.
    /// * SinglePlayer > Load / Save: open the slot lists (`M_Menu_Load_f` /
    ///   `M_Menu_Save_f`; Save refuses while no game runs).
    /// * Load > slot: [`MenuAction::LoadSlot`] + close when loadable, else
    ///   nothing (`M_Load_Key`'s `!loadable` return).
    /// * Save > slot: [`MenuAction::SaveSlot`] + close (`M_Save_Key`).
    /// * Multiplayer > Join/New Game: no net drivers, nothing (like the C);
    ///   Setup: not ported ([`MenuAction::None`]).
    /// * Options > Customize controls: the Keys screen; Video Options: the
    ///   video-mode list.
    /// * Options > Go to console: [`MenuAction::OpenConsole`].
    /// * Options > Reset to defaults: reset the in-menu cvars, return
    ///   [`MenuAction::ResetDefaults`].
    /// * Options analog/checkbox rows: Enter nudges them right (the C falls through
    ///   to `M_AdjustSliders(1)`).
    /// * Keys > row: start the bind grab (`bind_grab`), unbinding first when the
    ///   row already shows two keys (`M_Keys_Key` K_ENTER).
    /// * Video > row: apply the highlighted preset ([`MenuAction::ResolutionChanged`]).
    /// * Options > Web extras (port row): the Extras screen; Extras > row:
    ///   toggle that extra (menu2 + menu3, like an Options checkbox).
    /// * Quit > Enter == "Yes": close the menu ([`MenuAction::Closed`]).
    /// * Help: Enter is inert ([`MenuAction::None`]).
    pub fn select(&mut self) -> MenuAction {
        if self.new_game_confirm {
            return MenuAction::None; // SCR_ModalMessage ignores Enter.
        }
        match self.screen {
            MenuScreen::Main => {
                // M_Main_Key K_ENTER: m_entersound = true for every item.
                self.snd(MenuSound::Menu2);
                match self.cursor {
                    0 => {
                        // M_Menu_SinglePlayer_f
                        self.screen = MenuScreen::SinglePlayer;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    1 => {
                        // M_Menu_MultiPlayer_f
                        self.screen = MenuScreen::Multiplayer;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    2 => {
                        // M_Menu_Options_f
                        self.screen = MenuScreen::Options;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    3 => {
                        // M_Menu_Help_f
                        self.screen = MenuScreen::Help;
                        self.cursor = 0;
                        self.help_page = 0;
                        MenuAction::None
                    }
                    4 => {
                        // M_Menu_Quit_f: pop the confirm prompt (does NOT quit yet).
                        self.open_quit();
                        MenuAction::None
                    }
                    _ => MenuAction::None,
                }
            }
            MenuScreen::SinglePlayer => match self.cursor {
                0 => {
                    // New Game: `if (sv.active) if (!SCR_ModalMessage("Are
                    // you sure you want to\nstart a new game?\n")) break;` —
                    // with a game running, ask first (quit_yes / cancel
                    // answer). m_entersound is latched either way.
                    if self.server_active {
                        self.snd(MenuSound::Menu2);
                        self.new_game_confirm = true;
                        return MenuAction::None;
                    }
                    // The C runs `map start`; we start the hub and close.
                    // (M_SinglePlayer_Key latches m_entersound, but the menu
                    // closes before M_Draw can fire it — silent.)
                    self.close();
                    self.screen = MenuScreen::Main;
                    self.cursor = 0;
                    MenuAction::NewGame
                }
                1 => {
                    // M_Menu_Load_f (M_ScanSaves already ran host-side: the
                    // slot comments are whatever set_save_comments put there).
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Load;
                    self.cursor = 0;
                    MenuAction::None
                }
                2 => {
                    // M_Menu_Save_f: refuse without an active local game
                    // (!sv.active / cl.intermission / maxclients != 1 — the
                    // host folds those into game_active). The C latches
                    // m_entersound BEFORE the early return and the menu keeps
                    // drawing, so the menu2 still plays either way.
                    self.snd(MenuSound::Menu2);
                    if self.game_active {
                        self.screen = MenuScreen::Save;
                        self.cursor = 0;
                    }
                    MenuAction::None
                }
                _ => MenuAction::None,
            },
            MenuScreen::Load => {
                // M_Load_Key K_ENTER: menu2 first, then return unless loadable.
                self.snd(MenuSound::Menu2);
                if !self.slot_loadable(self.cursor) {
                    return MenuAction::None;
                }
                // m_state = m_none; key_dest = key_game; Cbuf "load sN".
                let slot = self.cursor;
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::LoadSlot(slot)
            }
            MenuScreen::Save => {
                // M_Save_Key K_ENTER (no sound in the C): m_state = m_none;
                // key_dest = key_game; Cbuf "save sN".
                let slot = self.cursor;
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::SaveSlot(slot)
            }
            MenuScreen::Multiplayer => {
                // M_MultiPlayer_Key K_ENTER: m_entersound = true; items 0/1
                // only open the net menu when a driver is available (none here,
                // like a C build with no network) and item 2 (Setup) is not
                // ported — so every item responds with the sound alone.
                self.snd(MenuSound::Menu2);
                MenuAction::None
            }
            MenuScreen::Options => match self.cursor {
                ROW_CONTROLS => {
                    // M_Menu_Keys_f
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Keys;
                    self.cursor = 0;
                    self.bind_grab = false;
                    MenuAction::None
                }
                ROW_VIDEO => {
                    // M_Menu_Video_f: open the mode list with the cursor on the
                    // current mode (vid_win.c keeps vid_line on the live mode).
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Video;
                    self.cursor = self.res_preset.min(RESOLUTION_PRESETS.len() - 1);
                    MenuAction::None
                }
                ROW_EXTRAS => {
                    // PORT ROW: open the Web extras screen, entered like
                    // M_Menu_Video_f (m_entersound) with the cursor on top.
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Extras;
                    self.cursor = 0;
                    MenuAction::None
                }
                ROW_CONSOLE => {
                    // m_state = m_none; Con_ToggleConsole_f(). The latched
                    // m_entersound never fires (the menu closed) — silent.
                    self.close();
                    MenuAction::OpenConsole
                }
                ROW_DEFAULTS => {
                    // Cbuf_AddText("exec default.cfg"): reset every option cvar
                    // (m_entersound plays — the menu stays up).
                    self.snd(MenuSound::Menu2);
                    self.reset_defaults();
                    MenuAction::ResetDefaults
                }
                // Every other row: Enter latches m_entersound AND falls through
                // to M_AdjustSliders(1) (its own menu3) — the C audibly plays
                // BOTH. (Screen size is viewsize: the host reads it each frame.)
                _ => {
                    self.snd(MenuSound::Menu2);
                    self.adjust(1);
                    MenuAction::None
                }
            },
            MenuScreen::Keys => {
                // M_Keys_Key K_ENTER: menu2; unbind first when the row already
                // shows two keys, then grab the next key.
                self.snd(MenuSound::Menu2);
                let keys = self.find_keys_for_command(self.cursor);
                if keys[1].is_some() {
                    self.unbind_command(self.cursor);
                }
                self.bind_grab = true;
                MenuAction::None
            }
            MenuScreen::Video => {
                // VID_MenuKey K_ENTER: menu1 (NOT menu2) + VID_SetMode on the
                // highlighted mode line.
                self.snd(MenuSound::Menu1);
                self.res_preset = self.cursor.min(RESOLUTION_PRESETS.len() - 1);
                MenuAction::ResolutionChanged
            }
            MenuScreen::Extras => {
                // As an Options checkbox row: Enter latches m_entersound and
                // falls through to the toggle (its own menu3).
                self.snd(MenuSound::Menu2);
                self.adjust(1);
                MenuAction::None
            }
            MenuScreen::Help => MenuAction::None,
            MenuScreen::Quit => {
                // Enter == "Yes": Host_Quit_f. Here that closes the menu (quit to
                // the attract loop). (The C's M_Quit_Key ignores Enter — only
                // y/Y quits — but this port has always accepted Enter as Yes.)
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::Closed
            }
        }
    }

    /// Back out (Escape / `K_ESCAPE`). A hidden menu is a no-op
    /// ([`MenuAction::None`]). Otherwise:
    /// * a Keys bind-grab in progress is cancelled (`M_Keys_Key`, the grab
    ///   branch's `K_ESCAPE`) — the screen stays;
    /// * SinglePlayer/Multiplayer/Options/Help return to Main; Load/Save return
    ///   to SinglePlayer; Keys/Video return to Options (each `M_Menu_*_f` plays
    ///   `m_entersound`), Extras to Options on its own row — all
    ///   [`MenuAction::Back`];
    /// * the Quit prompt answers "No" → restores the previous screen
    ///   ([`MenuAction::Back`]);
    /// * the Main screen closes the menu ([`MenuAction::Closed`]).
    pub fn cancel(&mut self) -> MenuAction {
        if !self.visible {
            return MenuAction::None;
        }
        if self.new_game_confirm {
            // SCR_ModalMessage returns false on Escape: New Game `break`s,
            // leaving the Single Player menu up.
            self.new_game_confirm = false;
            return MenuAction::None;
        }
        if self.bind_grab {
            // M_Keys_Key while defining a key: menu1; Escape just ends the grab.
            self.snd(MenuSound::Menu1);
            self.bind_grab = false;
            return MenuAction::None;
        }
        match self.screen {
            MenuScreen::SinglePlayer
            | MenuScreen::Multiplayer
            | MenuScreen::Options
            | MenuScreen::Help => {
                // M_*_Key K_ESCAPE -> M_Menu_Main_f (m_entersound = true).
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Load | MenuScreen::Save => {
                // M_Load_Key / M_Save_Key K_ESCAPE -> M_Menu_SinglePlayer_f.
                self.screen = MenuScreen::SinglePlayer;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Keys => {
                // M_Keys_Key K_ESCAPE -> M_Menu_Options_f (m_entersound).
                self.screen = MenuScreen::Options;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Video => {
                // VID_MenuKey K_ESCAPE: menu1, then M_Menu_Options_f (menu2).
                self.snd(MenuSound::Menu1);
                self.screen = MenuScreen::Options;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Extras => {
                // M_Menu_Options_f (menu2), back on the row that opened it —
                // the C's options_cursor is a static that keeps its place.
                self.screen = MenuScreen::Options;
                self.cursor = ROW_EXTRAS;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Quit => {
                // M_Quit_Key 'n'/Escape: restore the screen the prompt rose from
                // (wasInMenus -> m_entersound = true), or the game.
                self.quit_back()
            }
            MenuScreen::Main => {
                // M_Main_Key K_ESCAPE -> key_dest = key_game
                self.close();
                MenuAction::Closed
            }
        }
    }

    /// Raise the Quit confirmation prompt (`M_Menu_Quit_f`): remember the screen we
    /// came from (so "No" restores it) and switch to [`MenuScreen::Quit`]. Visible
    /// either way (the prompt is reachable from the game via `menu_cancel`-open too).
    pub fn open_quit(&mut self) {
        if self.screen == MenuScreen::Quit {
            return;
        }
        // M_Menu_Quit_f: wasInMenus = (key_dest == key_menu), and
        // msgNumber = rand()&7 (the C library's LCG, seeded 1 like an
        // un-srand()ed rand: 214013 / 2531011, bits 16..30).
        self.quit_in_menus = self.visible;
        self.quit_rand = self.quit_rand.wrapping_mul(214_013).wrapping_add(2_531_011);
        self.quit_msg = ((self.quit_rand >> 16) & 0x7fff) as usize & 7;
        self.quit_prev = self.screen;
        self.visible = true;
        self.screen = MenuScreen::Quit;
    }

    /// Show quit message `n` (`msgNumber`, 0..8) — for a caller that must
    /// match another run's random pick (the 2-D oracle harness).
    pub fn set_quit_message(&mut self, n: usize) {
        self.quit_msg = n & 7;
    }

    /// Answer the Quit prompt "Yes" (the literal `Y` key) — quit: close the menu.
    /// A no-op off the Quit screen. Returns [`MenuAction::Closed`] when it quit,
    /// else [`MenuAction::None`].
    pub fn quit_yes(&mut self) -> MenuAction {
        if self.new_game_confirm {
            // "y" answers the New Game modal: key_dest = key_game, disconnect,
            // `map start`.
            self.new_game_confirm = false;
            self.close();
            self.screen = MenuScreen::Main;
            self.cursor = 0;
            return MenuAction::NewGame;
        }
        if self.screen != MenuScreen::Quit {
            return MenuAction::None;
        }
        self.close();
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        MenuAction::Closed
    }

    /// Answer the Quit prompt "No" (the literal `N` key) — back out, same as
    /// [`cancel`](Menu::cancel) on the Quit screen. A no-op off the Quit screen.
    pub fn quit_no(&mut self) -> MenuAction {
        if self.new_game_confirm {
            self.new_game_confirm = false; // "n": no new game
            return MenuAction::None;
        }
        if self.screen != MenuScreen::Quit {
            return MenuAction::None;
        }
        self.quit_back()
    }

    /// M_Quit_Key 'n' / Escape: back to the menu the prompt rose over
    /// (`wasInMenus`: m_entersound), else back to the game.
    fn quit_back(&mut self) -> MenuAction {
        if !self.quit_in_menus {
            self.close();
            self.screen = MenuScreen::Main;
            return MenuAction::Closed;
        }
        self.screen = self.quit_prev;
        self.snd(MenuSound::Menu2);
        MenuAction::Back
    }

    /// Page the Help screen by `dir` (right/up = +1 next, left/down = -1 previous),
    /// wrapping over [`NUM_HELP_PAGES`] (`M_Help_Key`, which latches
    /// `m_entersound` — menu2 — on every page turn). A no-op off the Help screen.
    pub fn page(&mut self, dir: i32) {
        if self.screen != MenuScreen::Help {
            return;
        }
        self.snd(MenuSound::Menu2);
        self.help_page = help_page_wrap(self.help_page as i32 + dir.signum());
    }

    /// Adjust the highlighted Options row by `delta` (left = -1, right = +1),
    /// porting `M_AdjustSliders` — plus the screens whose `M_*_Key` maps
    /// left/right onto cursor movement (`M_Load_Key`/`M_Save_Key`/`M_Keys_Key`
    /// pair LEFT with UP and RIGHT with DOWN; `VID_MenuKey` steps the mode line)
    /// and Help paging. Nothing here changes the video mode: that is Enter on
    /// the Video Options list ([`MenuAction::ResolutionChanged`]).
    pub fn adjust(&mut self, delta: i32) {
        let step = delta.signum();
        if step == 0 {
            return;
        }
        // M_Help_Key: RIGHT = next page, LEFT = previous page (the C handles
        // left/right on Help identically to up/down).
        if self.screen == MenuScreen::Help {
            self.page(step);
            return;
        }
        // M_Load_Key / M_Save_Key / M_Keys_Key: LEFT pairs with UP and RIGHT
        // with DOWN (cursor movement, menu1 inside move_cursor). VID_MenuKey
        // also moves the mode line on left/right (single-column here).
        if matches!(
            self.screen,
            MenuScreen::Load | MenuScreen::Save | MenuScreen::Keys | MenuScreen::Video
        ) {
            self.move_cursor(step);
            return;
        }
        // The Extras rows are checkboxes: menu3, then flip regardless of the
        // direction, like M_AdjustSliders' checkbox cases.
        if self.screen == MenuScreen::Extras {
            self.snd(MenuSound::Menu3);
            if let Some(e) = WEB_EXTRAS.get(self.cursor).map(|w| w.extra) {
                let on = self.extras.get(e);
                self.extras.set(e, !on);
            }
            return;
        }
        if self.screen != MenuScreen::Options {
            return;
        }
        // M_AdjustSliders plays misc/menu3.wav unconditionally — even when the
        // cursor sits on an action row the switch below ignores.
        self.snd(MenuSound::Menu3);
        let d = step as f32;
        match self.cursor {
            ROW_SCREENSIZE => {
                // scr_viewsize.value += dir * 10, clamped 30..=120.
                self.viewsize =
                    (self.viewsize + d * VIEWSIZE_STEP).clamp(VIEWSIZE_MIN, VIEWSIZE_MAX);
            }
            ROW_BRIGHTNESS => {
                // v_gamma.value -= dir * 0.05 (LEFT brightens), clamp 0.5..=1.
                self.gamma = (self.gamma - d * GAMMA_STEP).clamp(GAMMA_MIN, GAMMA_MAX);
            }
            ROW_MOUSESPEED => {
                self.sensitivity =
                    (self.sensitivity + d * SENS_STEP).clamp(SENS_MIN, SENS_MAX);
            }
            ROW_CDVOLUME => {
                self.bgm_volume = (self.bgm_volume + d * BGM_STEP).clamp(BGM_MIN, BGM_MAX);
            }
            ROW_SNDVOLUME => {
                self.volume = (self.volume + d * VOLUME_STEP).clamp(VOLUME_MIN, VOLUME_MAX);
            }
            // Checkboxes ignore the direction and simply toggle (matches the C,
            // which flips the bool regardless of `dir`).
            ROW_ALWAYSRUN => self.always_run = !self.always_run,
            ROW_INVERTMOUSE => self.invert_mouse = !self.invert_mouse,
            ROW_LOOKSPRING => self.lookspring = !self.lookspring,
            ROW_LOOKSTRAFE => self.lookstrafe = !self.lookstrafe,
            // Action rows (Customize / Console / Defaults / Video): not adjustable.
            _ => {}
        }
    }

    /// "Reset to defaults" = `exec default.cfg`, and exactly what that file
    /// sets: `unbindall` + its `bind` lines (the key table), and the four
    /// "default cvars" at its end — `viewsize 100`, `gamma 1.0`, `volume 0.7`,
    /// `sensitivity 3`. Nothing else: CD Music Volume, Always Run
    /// (`cl_forwardspeed`), Invert Mouse (`m_pitch`), Lookspring and
    /// Lookstrafe keep their values, as in WinQuake, and so do the video mode and
    /// the port's Web extras.
    pub fn reset_defaults(&mut self) {
        self.viewsize = VIEWSIZE_DEFAULT;
        self.gamma = GAMMA_DEFAULT;
        self.volume = VOLUME_DEFAULT;
        self.sensitivity = SENS_DEFAULT;
        self.bindings = default_bindings();
    }

    /// The current video mode `(width, height)` ([`RESOLUTION_PRESETS`] entry
    /// `res_preset`; `320x200` until the host syncs it). Enter on the Video
    /// Options list changes it and the host resizes its framebuffer to it.
    pub fn resolution(&self) -> (i32, i32) {
        RESOLUTION_PRESETS
            .get(self.res_preset)
            .copied()
            .unwrap_or(RESOLUTION_PRESETS[0])
    }

    /// Point the Video Options "current mode" at the preset matching `(w, h)`, if
    /// one exists (otherwise leave it). The host calls this with its *actual* render
    /// size so the displayed value always tracks reality — the framebuffer is the
    /// single source of truth, and the label can never desync from it (e.g. after
    /// a boot / New Game / `map` that changed the render size independently).
    pub fn sync_resolution(&mut self, w: i32, h: i32) {
        if let Some(i) = RESOLUTION_PRESETS.iter().position(|&(pw, ph)| pw == w && ph == h) {
            self.res_preset = i;
        }
    }

    /// The `viewsize` cvar (`scr_viewsize`, 30..=120, default 100): the host
    /// sizes the 3-D view and the status bar from it via [`calc_refdef`](crate::screen::calc_refdef).
    pub fn viewsize(&self) -> f32 {
        self.viewsize
    }

    /// Set the `viewsize` cvar (the console's `viewsize <n>`), bounded to
    /// 30..=120 as SCR_CalcRefdef bounds it (and writes back) on the next frame.
    /// A non-number reads as 0 (`atof`), i.e. the minimum.
    pub fn set_viewsize(&mut self, v: f32) {
        let v = if v.is_finite() { v } else { 0.0 };
        self.viewsize = v.clamp(VIEWSIZE_MIN, VIEWSIZE_MAX);
    }

    /// `sizeup` (SCR_SizeUp_f): `viewsize += 10` (bounded as above). Bound to
    /// `+` and `=` in default.cfg.
    pub fn size_up(&mut self) {
        self.set_viewsize(self.viewsize + VIEWSIZE_STEP);
    }

    /// `sizedown` (SCR_SizeDown_f): `viewsize -= 10` (bounded as above). Bound
    /// to `-` in default.cfg.
    pub fn size_down(&mut self) {
        self.set_viewsize(self.viewsize - VIEWSIZE_STEP);
    }

    /// The Options "Mouse Speed" as a sensitivity multiplier the host applies to
    /// its baseline look sensitivity. id's `sensitivity` defaults to 3, so we
    /// normalise by [`SENS_DEFAULT`]: the out-of-the-box feel is unchanged (1.0x),
    /// and the 1..=11 range maps to a `0.33..=3.67` multiplier.
    pub fn mouse_sensitivity(&self) -> f32 {
        self.sensitivity / SENS_DEFAULT
    }

    /// The Options "Sound Volume" as a `0.0..=1.0` master gain (the `volume` cvar
    /// directly). Default 0.7.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// The raw `sensitivity` cvar value (1..=11), for display/tests.
    pub fn sensitivity(&self) -> f32 {
        self.sensitivity
    }

    /// The `v_gamma` cvar (Brightness, 0.5..=1). The host runs the presented
    /// frame through [`build_gamma_table`](crate::render::build_gamma_table) with this (identity at 1.0).
    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    /// The `bgmvolume` cvar (CD Music Volume, 0..=1). Live cvar; no CD audio
    /// exists to play at it (see the [`BGM_DEFAULT`] DEVIATION note).
    pub fn bgm_volume(&self) -> f32 {
        self.bgm_volume
    }

    /// Whether the "Always Run" checkbox is on (`cl_forwardspeed > 200`): the
    /// host swaps cl_forwardspeed/cl_backspeed 200 <-> 400 on it.
    pub fn always_run(&self) -> bool {
        self.always_run
    }

    /// Whether the "Invert Mouse" checkbox is on (`m_pitch < 0`): the host
    /// flips the mouse-pitch sign.
    pub fn invert_mouse(&self) -> bool {
        self.invert_mouse
    }

    /// Whether the "Lookspring" checkbox is on: pitch re-centres when mouse-look
    /// disengages (pointer unlock in this port — see the field note).
    pub fn lookspring(&self) -> bool {
        self.lookspring
    }

    /// Whether the "Lookstrafe" checkbox is on: mouse X strafes instead of
    /// turning while mouse-looking.
    pub fn lookstrafe(&self) -> bool {
        self.lookstrafe
    }

    /// The port's opt-in extras (Options > Web extras), all off by default.
    pub fn extras(&self) -> Extras {
        self.extras
    }

    /// Replace the extras wholesale (the page restoring its saved choice).
    /// Exact perspective stays off in a build without it.
    pub fn set_extras(&mut self, extras: Extras) {
        self.extras = Extras::from_bits(extras.bits());
    }

    /// Switch one extra (its `wasm_*` console command).
    pub fn set_extra(&mut self, e: Extra, on: bool) {
        self.extras.set(e, on);
    }

    // --- key bindings (M_Keys_*, keys.c) -----------------------------------

    /// Whether the Keys screen is waiting for the next key to bind
    /// (`bind_grab`). While set the host routes RAW keys to
    /// [`bind_key`](Menu::bind_key) instead of menu navigation.
    pub fn bind_grabbing(&self) -> bool {
        self.bind_grab
    }

    /// Deliver the grabbed key (`M_Keys_Key`, the `bind_grab` branch): plays
    /// menu1; Escape cancels and the console key (backtick) is refused; any
    /// other key binds to the highlighted command. Either way the grab ends.
    /// A no-op when not grabbing.
    pub fn bind_key(&mut self, keynum: u8) {
        if !self.bind_grab {
            return;
        }
        self.snd(MenuSound::Menu1);
        if keynum != K_ESCAPE && keynum != b'`' {
            let cmd = self.cursor.min(NUM_BINDNAMES - 1);
            self.bindings[keynum as usize] = Some(cmd as u8);
        }
        self.bind_grab = false;
    }

    /// Backspace/Del on the Keys screen (`M_Keys_Key` K_BACKSPACE/K_DEL): plays
    /// menu2 and unbinds every key bound to the highlighted command. A no-op on
    /// any other screen (and while grabbing — the C's grab branch consumes the
    /// key as a BINDING first; the host routes it to [`bind_key`](Menu::bind_key)).
    pub fn keys_backspace(&mut self) {
        if self.screen != MenuScreen::Keys || self.bind_grab {
            return;
        }
        self.snd(MenuSound::Menu2);
        self.unbind_command(self.cursor.min(NUM_BINDNAMES - 1));
    }

    /// The [`BINDNAMES`] command index bound to `keynum`, if any — the host's
    /// per-keypress lookup (the inverse of the C consulting `keybindings[key]`
    /// in `Key_Event`).
    pub fn action_for_key(&self, keynum: u8) -> Option<usize> {
        self.bindings[keynum as usize].map(|c| c as usize)
    }

    /// `M_FindKeysForCommand` (menu.c): the first two keys bound to `cmd`, in
    /// keynum order (the C scans 0..256 ascending).
    pub fn find_keys_for_command(&self, cmd: usize) -> [Option<u8>; 2] {
        let mut out = [None; 2];
        let mut n = 0;
        for (k, b) in self.bindings.iter().enumerate() {
            if *b == Some(cmd as u8) {
                out[n] = Some(k as u8);
                n += 1;
                if n == 2 {
                    break;
                }
            }
        }
        out
    }

    /// `M_UnbindCommand` (menu.c): clear every key bound to `cmd`.
    pub fn unbind_command(&mut self, cmd: usize) {
        for b in self.bindings.iter_mut() {
            if *b == Some(cmd as u8) {
                *b = None;
            }
        }
    }
}

/// The slider knob's virtual-x offset, in pixels, from the trough's drawing
/// origin `x` (`M_DrawSlider`): the knob (glyph 131) sits at
/// `(SLIDER_RANGE-1)*8 * range`, with `range` clamped to `[0,1]`. So fraction 0
/// puts the knob on the left segment and fraction 1 on the rightmost of the
/// [`SLIDER_RANGE`] middle segments. A non-finite fraction is treated as 0.
fn slider_knob_offset(range: f32) -> f32 {
    let r = if range.is_finite() {
        range.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (SLIDER_RANGE - 1) as f32 * 8.0 * r
}

/// The checkbox label text (`M_DrawCheckbox`): "on" / "off".
fn checkbox_text(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// Wrap a Help page index into `0..NUM_HELP_PAGES` (`M_Help_Key`: past the last
/// page wraps to 0, below 0 wraps to the last). Accepts any `i32`.
fn help_page_wrap(p: i32) -> usize {
    (p.rem_euclid(NUM_HELP_PAGES as i32)) as usize
}

/// The menu's pre-loaded picture bundle: the plaque, both titles, both item-list
/// graphics, and the 6-frame animated cursor. Each is an `Option` so a pak
/// missing any one degrades gracefully — [`draw_menu`] skips a `None` pic rather
/// than panicking.
///
/// Built once at boot from the PAK's `.lmp` files via [`crate::wad::Qpic::parse`].
#[derive(Debug, Clone, Default)]
pub struct MenuPics {
    /// `gfx/qplaque.lmp` — the decorative left plaque (drawn at (16,4)).
    pub qplaque: Option<crate::wad::Qpic>,
    /// `gfx/ttl_main.lmp` — the "MAIN" title (centered at y=4 on the main screen).
    pub ttl_main: Option<crate::wad::Qpic>,
    /// `gfx/mainmenu.lmp` — the 5-item main menu list graphic (drawn at (72,32)).
    pub mainmenu: Option<crate::wad::Qpic>,
    /// `gfx/ttl_sgl.lmp` — the single-player title (centered at y=4).
    pub ttl_sgl: Option<crate::wad::Qpic>,
    /// `gfx/sp_menu.lmp` — the 3-item single-player list graphic (drawn at (72,32)).
    pub sp_menu: Option<crate::wad::Qpic>,
    /// `gfx/p_option.lmp` — the "OPTIONS" title plaque (centered at y=4 on the
    /// options screen, like the other titles).
    pub p_option: Option<crate::wad::Qpic>,
    /// `gfx/p_load.lmp` — the "LOAD GAME" title (`M_Load_Draw`).
    pub p_load: Option<crate::wad::Qpic>,
    /// `gfx/p_save.lmp` — the "SAVE GAME" title (`M_Save_Draw`).
    pub p_save: Option<crate::wad::Qpic>,
    /// `gfx/p_multi.lmp` — the MULTIPLAYER title (`M_MultiPlayer_Draw`).
    pub p_multi: Option<crate::wad::Qpic>,
    /// `gfx/mp_menu.lmp` — the 3-item multiplayer list graphic (drawn at (72,32)).
    pub mp_menu: Option<crate::wad::Qpic>,
    /// `gfx/ttl_cstm.lmp` — the CUSTOMIZE CONTROLS title (`M_Keys_Draw`).
    pub ttl_cstm: Option<crate::wad::Qpic>,
    /// `gfx/vidmodes.lmp` — the VIDEO MODES title (vid_win.c `VID_MenuDraw`).
    pub vidmodes: Option<crate::wad::Qpic>,
    /// `gfx/menudot1.lmp`..`menudot6.lmp` — the 6-frame animated cursor.
    pub menudot: [Option<crate::wad::Qpic>; 6],
    /// `gfx/help0.lmp`..`help5.lmp` — the 6 full-screen Help/Ordering pages
    /// (`M_Help_Draw` blits the current one at (0,0)).
    pub help: [Option<crate::wad::Qpic>; NUM_HELP_PAGES],
    /// The `M_DrawTextBox` border pieces, in [`TEXTBOX_PICS`] order.
    pub textbox: [Option<crate::wad::Qpic>; 10],
}

/// The pak pics `M_DrawTextBox` builds a box from, in [`MenuPics::textbox`]
/// order: the left column (top, middle, bottom), the 16-wide middle columns
/// (top, middle, the alternate middle of the second row, bottom), the right
/// column (top, middle, bottom).
pub const TEXTBOX_PICS: [&str; 10] = [
    "gfx/box_tl.lmp",
    "gfx/box_ml.lmp",
    "gfx/box_bl.lmp",
    "gfx/box_tm.lmp",
    "gfx/box_mm.lmp",
    "gfx/box_mm2.lmp",
    "gfx/box_bm.lmp",
    "gfx/box_tr.lmp",
    "gfx/box_mr.lmp",
    "gfx/box_br.lmp",
];

/// `quitMessage` (menu.c, the non-Windows builds): four 24-column lines each.
const QUIT_MESSAGES: [[&str; 4]; 8] = [
    ["  Are you gonna quit    ", "  this game just like   ", "   everything else?     ", "                        "],
    [" Milord, methinks that  ", "   thou art a lowly     ", " quitter. Is this true? ", "                        "],
    [" Do I need to bust your ", "  face open for trying  ", "        to quit?        ", "                        "],
    [" Man, I oughta smack you", "   for trying to quit!  ", "     Press Y to get     ", "      smacked out.      "],
    [" Press Y to quit like a ", "   big loser in life.   ", "  Press N to stay proud ", "    and successful!     "],
    ["   If you press Y to    ", "  quit, I will summon   ", "  Satan all over your   ", "      hard drive!       "],
    ["  Um, Asmodeus dislikes ", " his children trying to ", " quit. Press Y to return", "   to your Tinkertoys.  "],
    ["  If you quit now, I'll ", "  throw a blanket-party ", "   for you next time!   ", "                        "],
];

/// `M_Print` (menu.c): menu text in the conchars' second, bronze half — each
/// character is drawn as cell `c + 128` — at virtual `(vx, vy)`, 8 px apart.
/// (`M_PrintWhite` is plain [`draw_string_scaled`].) The menus print their
/// labels, values and hints this way; white marks only the odd highlight (the
/// current video mode, "No Communications Available").
#[allow(clippy::too_many_arguments)]
fn m_print(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    text: &str,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    for (i, b) in text.bytes().enumerate() {
        let x = vx + 8.0 * i as f32;
        draw_char_scaled(image, conchars, x, vy, b.wrapping_add(128), scale, ox, oy, palette);
    }
}

/// Draw a slider widget (`M_DrawSlider`) with its trough origin at virtual
/// `(x, y)`: glyph 128 (left cap) at `x-8`, [`SLIDER_RANGE`] copies of glyph 129
/// (middle) starting at `x`, glyph 130 (right cap) just past them, and the knob
/// (glyph 131) at `x + slider_knob_offset(range)`. `range` is the cvar's [0,1]
/// fraction (clamped inside [`slider_knob_offset`]).
#[allow(clippy::too_many_arguments)]
fn draw_slider(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    x: f32,
    y: f32,
    range: f32,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    draw_char_scaled(image, conchars, x - 8.0, y, SLIDER_LEFT_CHAR, scale, ox, oy, palette);
    for i in 0..SLIDER_RANGE {
        let cx = x + i as f32 * 8.0;
        draw_char_scaled(image, conchars, cx, y, SLIDER_MID_CHAR, scale, ox, oy, palette);
    }
    let right_x = x + SLIDER_RANGE as f32 * 8.0;
    draw_char_scaled(image, conchars, right_x, y, SLIDER_RIGHT_CHAR, scale, ox, oy, palette);
    let knob_x = x + slider_knob_offset(range);
    draw_char_scaled(image, conchars, knob_x, y, SLIDER_KNOB_CHAR, scale, ox, oy, palette);
}

/// Draw the main menu (or single-player submenu) over `image`, a port of
/// `M_Main_Draw` / `M_SinglePlayer_Draw`.
///
/// The layout is menu.c's 320-wide one, centred across the top of the
/// [`screen_2d`] screen as `M_DrawPic`'s `(vid.width - 320)>>1` centres it —
/// at the framebuffer's own pixel size, or blown up with the "scaled 2-D"
/// extra.
///
/// Two clocks, exactly like the C: `host_time` (the clamped-frametime host
/// clock) drives the animated menudot spinner, `(int)(host_time*10) % 6`
/// (`M_Main_Draw` and friends); `realtime` (the unclamped wall clock) drives
/// every flashing conchars cursor, `12 + ((int)(realtime*4) & 1)` — see
/// [`menu_cursor_glyph`].
///
/// Each pic is fetched from `pics` and skipped if absent (`None`) — a pak missing
/// the menu art still renders the rest without panicking. `conchars` draws the
/// text screens (Options, Keys, Load/Save, Quit); the Main and Single Player
/// items come from the `mainmenu`/`sp_menu` graphics, exactly as in Quake.
pub fn draw_menu(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    host_time: f32,
    realtime: f64,
    palette: &[[u8; 3]; 256],
) {
    draw_menu_inner(image, menu, pics, conchars, host_time, realtime, palette, true);
}

/// [`draw_menu`], with `fade` false for `M_Draw`'s `m_recursiveDraw` (the
/// screen the Quit prompt rose over, drawn under it without a second fade).
#[allow(clippy::too_many_arguments)]
fn draw_menu_inner(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    host_time: f32,
    realtime: f64,
    palette: &[[u8; 3]; 256],
    fade: bool,
) {
    if !menu.visible || image.w == 0 || image.h == 0 {
        return;
    }
    // M_DrawPic / M_DrawCharacter: `x + ((vid.width - 320)>>1)`, y as given —
    // the 320-wide menu centred across the top of the 2-D screen.
    let sc = screen_2d(image.w, image.h);
    let scale = sc.scale;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = ((sc.w - MENU_VIRT_W as i32) >> 1) as f32 * scale;
    let oy = 0.0;

    // M_Draw: the game/demo underneath fades first (Draw_FadeScreen). (The
    // C's other branch, the console background under a forced-up console,
    // can't occur: this port's menu and console never share the screen.)
    if fade {
        fade_screen(image, palette);
    }

    // SCR_ModalMessage's screen (scr_drawdialog: Sbar, Draw_FadeScreen,
    // SCR_DrawNotifyString) — the menu itself is not drawn.
    if menu.new_game_confirm {
        if let Some(cc) = conchars {
            draw_notify_string(image, cc, NEW_GAME_CONFIRM, scale, palette);
        }
        return;
    }

    // The animated cursor frame: (int)(host_time*10) % 6. Guard a non-finite /
    // negative clock so the index stays 0..6.
    let frame = if host_time.is_finite() && host_time > 0.0 {
        ((host_time * 10.0) as usize) % 6
    } else {
        0
    };
    // The flashing conchars cursor (Options / Load / Save / Keys / Video) runs
    // on REAL time at 4 Hz, independent of the menudot's host_time spinner.
    let cursor = menu_cursor_glyph(realtime);

    // The Help screen is a full-screen pic at (0,0); the Quit prompt is a small
    // text box; Load/Save/Keys/Video are a centered title + text rows with no
    // qplaque (M_Load_Draw etc. draw only the title pic). Dispatch them all
    // before drawing the plaque.
    match menu.screen {
        MenuScreen::Help => {
            draw_help_screen(image, menu, pics, scale, ox, oy, palette);
            return;
        }
        MenuScreen::Quit => {
            // M_Quit_Draw: wasInMenus -> the menu it rose over, m_recursiveDraw.
            if menu.quit_in_menus && menu.quit_prev != MenuScreen::Quit {
                let mut under = menu.clone();
                under.screen = menu.quit_prev;
                draw_menu_inner(image, &under, pics, conchars, host_time, realtime, palette, false);
            }
            draw_quit_screen(image, menu, pics, conchars, scale, ox, oy, palette);
            return;
        }
        MenuScreen::Load | MenuScreen::Save => {
            draw_load_save_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        MenuScreen::Keys => {
            draw_keys_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        MenuScreen::Video => {
            draw_video_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        _ => {}
    }

    // The plaque is shared by the Main / SinglePlayer / Multiplayer / Options
    // screens (M_DrawTransPic (16,4)), and the port's Extras page of Options.
    if let Some(p) = &pics.qplaque {
        blit_qpic_at(image, p, 16.0, 4.0, scale, ox, oy, palette);
    }

    // The Options screen is laid out from text rows (it has no single list pic);
    // the Main / SinglePlayer screens use their pre-baked list graphic. Branch the
    // whole body so each screen draws its own title + rows.
    if menu.screen == MenuScreen::Options {
        draw_options_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
        return;
    }
    // The port's Web extras page: a page of Options (same plaque + title).
    if menu.screen == MenuScreen::Extras {
        draw_extras_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
        return;
    }

    // M_MultiPlayer_Draw: the C's exact layout, plus the line a netless build
    // shows.
    if menu.screen == MenuScreen::Multiplayer {
        draw_multiplayer_screen(image, menu, pics, conchars, scale, ox, oy, frame, palette);
        return;
    }

    // The centered title + the item-list graphic differ per screen.
    let (title, list) = match menu.screen {
        MenuScreen::Main => (&pics.ttl_main, &pics.mainmenu),
        MenuScreen::SinglePlayer => (&pics.ttl_sgl, &pics.sp_menu),
        // Every other screen is handled above (early return); the catch-all keeps
        // the match exhaustive without a second layout here.
        _ => (&pics.p_option, &None),
    };
    if let Some(t) = title {
        // M_DrawPic ((320 - p->width)/2, 4, p).
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(l) = list {
        // M_DrawTransPic (72, 32, ...).
        blit_qpic_at(image, l, 72.0, 32.0, scale, ox, oy, palette);
    }

    // The animated cursor at (54, 32 + cursor*20).
    if let Some(dot) = pics.menudot.get(frame).and_then(|d| d.as_ref()) {
        let cy = 32.0 + menu.cursor as f32 * 20.0;
        blit_qpic_at(image, dot, 54.0, cy, scale, ox, oy, palette);
    }
}

/// The Options rows' vertical origin and step (`M_Options_Draw`: first label at
/// y=32, 8 px per row; the flashing cursor at `32 + cursor*8`).
const OPTIONS_ROW_Y0: f32 = 32.0;
const OPTIONS_ROW_STEP: f32 = 8.0;
/// The right-justified label column origin (`M_Print(16, ...)`). The labels are
/// pre-padded to a fixed width so their right edges line up at ~x=184, exactly as
/// id ships them (e.g. `"    Customize controls"`).
const OPTIONS_LABEL_X: f32 = 16.0;
/// Sliders + checkboxes draw at virtual x=220 (`M_DrawSlider(220,…)` /
/// `M_DrawCheckbox(220,…)`).
const OPTIONS_WIDGET_X: f32 = 220.0;

/// The Options labels, pre-padded to right-justify at x≈184 — the first 13
/// copied verbatim from `M_Options_Draw` so the column lines up with the
/// widgets at x=220; the 14th is the port's row, padded the same way.
const OPTIONS_LABELS: [&str; OPTIONS_ITEMS] = [
    "    Customize controls",
    "         Go to console",
    "     Reset to defaults",
    "           Screen size",
    "            Brightness",
    "           Mouse Speed",
    "       CD Music Volume",
    "          Sound Volume",
    "            Always Run",
    "          Invert Mouse",
    "            Lookspring",
    "            Lookstrafe",
    "         Video Options",
    "            Web extras",
];

/// Draw the Options submenu, a faithful port of `M_Options_Draw`: the `p_option`
/// title plaque centered at the top, the 13 right-justified labels at x=16 (8 px
/// apart from y=32), a [`draw_slider`] at x=220 for the analog rows (Screen size /
/// Brightness / Mouse Speed / CD Music Volume / Sound Volume), a [`checkbox_text`]
/// at x=220 for the boolean rows (Always Run / Invert Mouse / Lookspring /
/// Lookstrafe), and the flashing cursor glyph (12/13) at x=200 on the focused row.
///
/// A missing `conchars` leaves the labels/widgets blank but still draws the title;
/// nothing here panics. `cursor_glyph` is the flashing cursor's conchars cell
/// this frame ([`menu_cursor_glyph`]: 12/13 on real time at 4 Hz).
#[allow(clippy::too_many_arguments)]
fn draw_options_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    // The "OPTIONS" title plaque, centered like the other screens' titles.
    if let Some(t) = &pics.p_option {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }

    if let Some(cc) = conchars {
        // The labels.
        for (i, label) in OPTIONS_LABELS.iter().enumerate() {
            let ry = OPTIONS_ROW_Y0 + i as f32 * OPTIONS_ROW_STEP;
            m_print(image, cc, OPTIONS_LABEL_X, ry, label, scale, ox, oy, palette);
        }

        // The analog widgets (M_DrawSlider) on the slider rows, each with its
        // cvar's [0,1] fraction.
        let slider_row = |row: usize| OPTIONS_ROW_Y0 + row as f32 * OPTIONS_ROW_STEP;
        // Screen size: r = (scr_viewsize - 30) / (120 - 30).
        let size_frac = (menu.viewsize() - VIEWSIZE_MIN) / (VIEWSIZE_MAX - VIEWSIZE_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_SCREENSIZE), size_frac, scale, ox, oy, palette);
        // Brightness: r = (1 - gamma)/0.5.
        let bright_frac = (1.0 - menu.gamma()) / (GAMMA_MAX - GAMMA_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_BRIGHTNESS), bright_frac, scale, ox, oy, palette);
        // Mouse Speed: r = (sensitivity - 1)/10.
        let mouse_frac = (menu.sensitivity() - SENS_MIN) / (SENS_MAX - SENS_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_MOUSESPEED), mouse_frac, scale, ox, oy, palette);
        // CD Music Volume: r = bgmvolume.
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_CDVOLUME), menu.bgm_volume(), scale, ox, oy, palette);
        // Sound Volume: r = volume.
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_SNDVOLUME), menu.volume(), scale, ox, oy, palette);

        // The checkbox rows (M_DrawCheckbox -> "on"/"off").
        let checks = [
            (ROW_ALWAYSRUN, menu.always_run()),
            (ROW_INVERTMOUSE, menu.invert_mouse()),
            (ROW_LOOKSPRING, menu.lookspring()),
            (ROW_LOOKSTRAFE, menu.lookstrafe()),
        ];
        for (row, on) in checks {
            let ry = OPTIONS_ROW_Y0 + row as f32 * OPTIONS_ROW_STEP;
            // M_DrawCheckbox: M_Print (x, y, "on" / "off").
            m_print(image, cc, OPTIONS_WIDGET_X, ry, checkbox_text(on), scale, ox, oy, palette);
        }

        // The flashing cursor: M_DrawCharacter(200, 32 + cursor*8, 12 + (blink)).
        let cy = OPTIONS_ROW_Y0 + menu.cursor as f32 * OPTIONS_ROW_STEP;
        draw_char_scaled(image, cc, OPTIONS_CURSOR_X, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// The Extras screen's layout, in `M_Keys_Draw`'s shape: a white header line
/// at y=32, then the rows from y=48, 8 px apart, and the highlighted row's
/// help lines from y=[`EXTRAS_HELP_Y`].
const EXTRAS_HEADER_Y: f32 = 32.0;
const EXTRAS_ROW_Y0: f32 = 48.0;
const EXTRAS_HELP_Y: f32 = 88.0;
/// The Extras header (`M_PrintWhite`, centred): what these rows are.
const EXTRAS_HEADER: &str = "Web extras: not in id's Quake";

/// Draw the port's Web extras screen in `M_Options_Draw`'s idiom: qplaque
/// (drawn by the caller) and the `p_option` title (it is a page of Options),
/// the [`EXTRAS_HEADER`] in white, then each extra as an Options checkbox row
/// — the right-justified `M_Print` label at x=16, `M_DrawCheckbox`'s "on" /
/// "off" at x=220, the 4 Hz flashing cursor at x=200 — and, under the list,
/// the highlighted row's three bronze help lines, centred.
#[allow(clippy::too_many_arguments)]
fn draw_extras_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.p_option {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    let Some(cc) = conchars else { return };
    let centred = |s: &str| ((MENU_VIRT_W as i32 - s.len() as i32 * 8) / 2) as f32;
    draw_string_scaled(
        image, cc, centred(EXTRAS_HEADER), EXTRAS_HEADER_Y, EXTRAS_HEADER, scale, ox, oy, palette,
    );
    for (i, row) in WEB_EXTRAS.iter().enumerate() {
        let y = EXTRAS_ROW_Y0 + i as f32 * OPTIONS_ROW_STEP;
        m_print(image, cc, OPTIONS_LABEL_X, y, row.label, scale, ox, oy, palette);
        let on = checkbox_text(menu.extras.get(row.extra));
        m_print(image, cc, OPTIONS_WIDGET_X, y, on, scale, ox, oy, palette);
    }
    let cy = EXTRAS_ROW_Y0 + menu.cursor as f32 * OPTIONS_ROW_STEP;
    draw_char_scaled(image, cc, OPTIONS_CURSOR_X, cy, cursor_glyph, scale, ox, oy, palette);
    if let Some(row) = WEB_EXTRAS.get(menu.cursor) {
        for (i, line) in extras_help_lines(row).iter().enumerate() {
            let y = EXTRAS_HELP_Y + i as f32 * 8.0;
            m_print(image, cc, centred(line), y, line, scale, ox, oy, palette);
        }
    }
}

/// The three help lines under the Extras list for `row`: its two, then its
/// console variable.
fn extras_help_lines(row: &WebExtra) -> [String; 3] {
    [row.help[0].to_string(), row.help[1].to_string(), format!("console: {} 0/1", row.cvar)]
}

/// Draw the Load or Save slot list, a port of `M_Load_Draw` / `M_Save_Draw`:
/// the `p_load`/`p_save` title centered at y=4 (no qplaque on these screens),
/// [`MAX_SAVEGAMES`] rows of `M_Print(16, 32 + 8*i, m_filenames[i])` — each row
/// is the host-set slot comment, or [`UNUSED_SLOT`] when empty, exactly what
/// `M_ScanSaves` leaves for a missing `sN.sav` — and the flashing cursor at
/// `M_DrawCharacter(8, 32 + cursor*8, 12 + blink)`.
#[allow(clippy::too_many_arguments)]
fn draw_load_save_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    let title = if menu.screen == MenuScreen::Save {
        &pics.p_save
    } else {
        &pics.p_load
    };
    if let Some(t) = title {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(cc) = conchars {
        for i in 0..MAX_SAVEGAMES {
            let ry = 32.0 + i as f32 * 8.0;
            let text = menu.save_comment(i);
            let row = if text.is_empty() { UNUSED_SLOT } else { text };
            m_print(image, cc, 16.0, ry, row, scale, ox, oy, palette);
        }
        let cy = 32.0 + menu.cursor as f32 * 8.0;
        draw_char_scaled(image, cc, 8.0, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// Draw the multiplayer submenu, a port of `M_MultiPlayer_Draw`: qplaque at
/// (16,4) (drawn by the caller), the `p_multi` title centered, the `mp_menu`
/// 3-item list at (72,32), the animated menudot cursor at (54, 32 + cursor*20)
/// — and, since no net driver exists (netcode is out of scope), the C's exact
/// "No Communications Available" line at y=148.
#[allow(clippy::too_many_arguments)]
fn draw_multiplayer_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    frame: usize,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.p_multi {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(l) = &pics.mp_menu {
        blit_qpic_at(image, l, 72.0, 32.0, scale, ox, oy, palette);
    }
    if let Some(dot) = pics.menudot.get(frame).and_then(|d| d.as_ref()) {
        let cy = 32.0 + menu.cursor as f32 * 20.0;
        blit_qpic_at(image, dot, 54.0, cy, scale, ox, oy, palette);
    }
    if let Some(cc) = conchars {
        // M_PrintWhite ((320/2) - ((27*8)/2), 148, "No Communications Available").
        let line = "No Communications Available";
        let cx = MENU_VIRT_W * 0.5 - (line.len() as f32 * 8.0) * 0.5;
        draw_string_scaled(image, cc, cx, 148.0, line, scale, ox, oy, palette);
    }
}

/// Draw the Customize-controls screen, a port of `M_Keys_Draw`: the `ttl_cstm`
/// title centered at y=4, the instruction line at y=32 ("Press a key..." while
/// grabbing, else "Enter to change..."), one row per [`BINDNAMES`] entry from
/// y=48 (label at x=16, bound key name(s) at x=140 — "???" when unbound, "or"
/// between two), and the cursor at x=130 — `=` while grabbing, else the
/// flashing 12/13 glyph.
#[allow(clippy::too_many_arguments)]
fn draw_keys_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.ttl_cstm {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    let Some(cc) = conchars else { return };
    // Every string on this screen is M_Print (bronze).
    if menu.bind_grabbing() {
        m_print(
            image, cc, 12.0, 32.0, "Press a key or button for this action", scale, ox, oy,
            palette,
        );
    } else {
        m_print(
            image, cc, 18.0, 32.0, "Enter to change, backspace to clear", scale, ox, oy, palette,
        );
    }
    for (i, (_, label)) in BINDNAMES.iter().enumerate() {
        let y = 48.0 + 8.0 * i as f32;
        m_print(image, cc, 16.0, y, label, scale, ox, oy, palette);
        let keys = menu.find_keys_for_command(i);
        match keys[0] {
            None => m_print(image, cc, 140.0, y, "???", scale, ox, oy, palette),
            Some(k0) => {
                let name = keynum_to_string(k0);
                m_print(image, cc, 140.0, y, &name, scale, ox, oy, palette);
                if let Some(k1) = keys[1] {
                    // M_Print (140 + x + 8, y, "or"); M_Print (140 + x + 32, ...).
                    let x = name.len() as f32 * 8.0;
                    m_print(image, cc, 140.0 + x + 8.0, y, "or", scale, ox, oy, palette);
                    m_print(
                        image, cc, 140.0 + x + 32.0, y, &keynum_to_string(k1), scale, ox, oy,
                        palette,
                    );
                }
            }
        }
    }
    let cy = 48.0 + menu.cursor as f32 * 8.0;
    if menu.bind_grabbing() {
        // M_DrawCharacter (130, 48 + keys_cursor*8, '=').
        draw_char_scaled(image, cc, 130.0, cy, b'=', scale, ox, oy, palette);
    } else {
        // M_DrawCharacter (130, 48 + keys_cursor*8, 12+((int)(realtime*4)&1)).
        draw_char_scaled(image, cc, 130.0, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// Draw the video-modes screen — this port's `VID_MenuDraw` (vid_win.c): the
/// `vidmodes` title centered at y=4, one row per [`RESOLUTION_PRESETS`] entry
/// from y=36 (`WIDTHxHEIGHT`, bronze; the current mode white, as the C marks
/// it), the flashing cursor on the highlighted row, and hint lines.
/// Single column — the C's 3-wide grid exists to fit 15+ DOS modes; 7 presets
/// fit one column.
#[allow(clippy::too_many_arguments)]
fn draw_video_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.vidmodes {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    let Some(cc) = conchars else { return };
    // VID_MenuDraw prints every mode with M_Print (bronze) except the current
    // one, which it prints with M_PrintWhite.
    let current = menu.resolution();
    for (i, &(w, h)) in RESOLUTION_PRESETS.iter().enumerate() {
        let y = 36.0 + 8.0 * i as f32;
        let row = format!("{w}x{h}");
        if (w, h) == current {
            draw_string_scaled(image, cc, 16.0, y, &row, scale, ox, oy, palette);
        } else {
            m_print(image, cc, 16.0, y, &row, scale, ox, oy, palette);
        }
    }
    let cy = 36.0 + menu.cursor as f32 * 8.0;
    draw_char_scaled(image, cc, 8.0, cy, cursor_glyph, scale, ox, oy, palette);
    // The C's bottom hints ("Press enter to set mode" / "Esc to exit"), at this
    // single column's foot.
    let hints_y = 36.0 + RESOLUTION_PRESETS.len() as f32 * 8.0 + 16.0;
    m_print(image, cc, 9.0 * 8.0, hints_y, "Press Enter to set mode", scale, ox, oy, palette);
    m_print(image, cc, 15.0 * 8.0, hints_y + 16.0, "Esc to exit", scale, ox, oy, palette);
}

/// Draw the Help/Ordering screen (`M_Help_Draw`): blit the current page pic
/// (`gfx/help{page}.lmp`) full-screen at virtual (0,0). A missing page pic draws
/// nothing (graceful degrade); no panic.
fn draw_help_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if let Some(p) = pics.help.get(menu.help_page()).and_then(|p| p.as_ref()) {
        blit_qpic_at(image, p, 0.0, 0.0, scale, ox, oy, palette);
    }
}

/// `M_SinglePlayer_Key`'s New Game question (SCR_ModalMessage).
const NEW_GAME_CONFIRM: &str = "Are you sure you want to\nstart a new game?\n";

/// `SCR_DrawNotifyString` (screen.c): each line (up to 40 columns) centred on
/// the screen, from `y = vid.height*0.35` (x87's row 69 on a 200-line screen,
/// [`center_string_top`]), in plain (white) conchars.
fn draw_notify_string(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    text: &str,
    scale: f32,
    palette: &[[u8; 3]; 256],
) {
    // vid.width / vid.height of the 2-D screen: the text is placed on it, not
    // on the menu's centred 320 columns.
    let sc = screen_2d(image.w, image.h);
    let mut y = center_string_top(sc.h) as f32;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop(); // the loop stops at the terminating NUL after the last \n
    }
    for line in lines {
        let line = &line[..line.len().min(40)];
        let x = ((sc.w - line.len() as i32 * 8) / 2) as f32;
        draw_string_scaled(image, conchars, x, y, line, scale, 0.0, 0.0, palette);
        y += 8.0;
    }
}

/// `M_DrawTextBox (x, y, width, lines)` (menu.c): a box of 8x8 border pics
/// around `width` columns and `lines` rows of text, its top-left at `(x, y)` —
/// the left column, then the middle 16 pixels at a time (`width -= 2`; the
/// second row's middle piece is `box_mm2`), then the right column, each
/// piece through `M_DrawTransPic`. Missing pieces are skipped.
#[allow(clippy::too_many_arguments)]
fn draw_text_box(
    image: &mut Image,
    pics: &MenuPics,
    x: i32,
    y: i32,
    width: i32,
    lines: i32,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    let mut put = |i: usize, cx: i32, cy: i32| {
        if let Some(p) = &pics.textbox[i] {
            blit_qpic_at(image, p, cx as f32, cy as f32, scale, ox, oy, palette);
        }
    };
    // left side
    let (mut cx, mut cy) = (x, y);
    put(0, cx, cy);
    for _ in 0..lines {
        cy += 8;
        put(1, cx, cy);
    }
    put(2, cx, cy + 8);
    // middle
    cx += 8;
    let mut w = width;
    while w > 0 {
        cy = y;
        put(3, cx, cy);
        for n in 0..lines {
            cy += 8;
            put(if n >= 1 { 5 } else { 4 }, cx, cy);
        }
        put(6, cx, cy + 8);
        w -= 2;
        cx += 16;
    }
    // right side
    cy = y;
    put(7, cx, cy);
    for _ in 0..lines {
        cy += 8;
        put(8, cx, cy);
    }
    put(9, cx, cy + 8);
}

/// `M_Quit_Draw` (menu.c, the non-Windows builds — DOS Quake's; WinQuake on
/// Windows shows a credits box instead): `M_DrawTextBox (56, 76, 24, 4)` and
/// the four lines of quit message `msgNumber` at (64, 84..108) in `M_Print`'s
/// bronze. The menu it rose over is drawn first by [`draw_menu_inner`].
#[allow(clippy::too_many_arguments)]
fn draw_quit_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    draw_text_box(image, pics, 56, 76, 24, 4, scale, ox, oy, palette);
    if let Some(cc) = conchars {
        for (i, line) in QUIT_MESSAGES[menu.quit_msg & 7].iter().enumerate() {
            m_print(image, cc, 64.0, 84.0 + 8.0 * i as f32, line, scale, ox, oy, palette);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{K_CTRL, K_MOUSE1, K_SHIFT, K_SPACE, K_UPARROW};
    use crate::render::fixtures::{ramp_palette, solid_pic};
    use crate::wad::Qpic;

    /// A test conchars atlas where every glyph texel is the lit index 3 (except
    /// the byte-0 cell, which stays the transparent index 0), so any drawn
    /// label/value paints index-3 pixels.
    fn test_conchars() -> Qpic {
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        Qpic { width: 128, height: 128, data }
    }

    // -- main menu (Menu engine + draw_menu + draw_string) ------------------

    #[test]
    fn menu_move_cursor_wraps_within_each_screen() {
        let mut m = Menu::new();
        m.open(); // Main: 5 items.
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
        // Down past the end wraps to 0.
        for expect in [1, 2, 3, 4, 0, 1] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        // Up below 0 wraps to the last item (4).
        m.move_cursor(-1);
        assert_eq!(m.cursor(), 0);
        m.move_cursor(-1);
        assert_eq!(m.cursor(), 4);

        // On the single-player screen the wrap is modulo 3.
        m.cursor = 0;
        let action = m.select(); // Main>Single Player
        assert_eq!(action, MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        for expect in [1, 2, 0, 1] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        // A large delta still wraps correctly.
        m.cursor = 0;
        m.move_cursor(7); // 7 % 3 = 1
        assert_eq!(m.cursor(), 1);
        m.move_cursor(-7); // back to 0
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn menu_select_and_cancel_transitions() {
        let mut m = Menu::new();
        m.open();

        // Main > Single Player goes to the submenu, no host action.
        m.cursor = 0;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        assert!(m.visible);

        // SinglePlayer > New Game returns NewGame and closes the menu.
        m.cursor = 0;
        assert_eq!(m.select(), MenuAction::NewGame);
        assert!(!m.visible);

        // Re-open: Escape on a submenu goes Back to Main (still visible).
        m.open();
        m.select(); // -> SinglePlayer
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);

        // Escape on Main closes the menu.
        assert_eq!(m.cancel(), MenuAction::Closed);
        assert!(!m.visible);

        // Cancel on a hidden menu is a no-op.
        assert_eq!(m.cancel(), MenuAction::None);

        // Quit (item 4 on Main) raises the confirm prompt (does NOT close yet).
        m.open();
        m.cursor = 4;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Quit, "Quit raises the confirm prompt");
        assert!(m.visible);
        // Escape ("No") backs out to the screen the prompt rose from (Main here).
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // Re-raise it and answer "Yes" via select (Enter): closes the menu.
        m.cursor = 4;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Quit);
        assert_eq!(m.select(), MenuAction::Closed, "Enter on the Quit prompt quits");
        assert!(!m.visible);

        // Main item Multiplayer (item 1) opens the multiplayer screen
        // (M_Menu_MultiPlayer_f); Enter there does nothing (no net drivers,
        // like the C), and Escape returns to Main.
        m.open();
        m.cursor = 1;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Multiplayer, "item 1 enters Multiplayer");
        assert!(m.visible);
        assert_eq!(m.select(), MenuAction::None, "Join responds with no action (no net)");
        assert_eq!(m.screen(), MenuScreen::Multiplayer);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);

        // Help (item 3) now opens the Help screen.
        m.cursor = 3;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Help, "item 3 enters Help");
        assert_eq!(m.help_page(), 0, "Help opens on page 0");
        assert!(m.visible);
        // Escape backs out of Help to Main.
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);

        // Main item 2 (Options) switches to the Options screen.
        m.open();
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options, "item 2 enters Options");
        assert!(m.visible);
    }

    #[test]
    fn menu_toggle_open_back_close() {
        let mut m = Menu::new();
        // Hidden -> open on Main.
        assert_eq!(m.toggle(), MenuAction::None);
        assert!(m.visible);
        assert_eq!(m.screen(), MenuScreen::Main);
        // On a submenu, toggle backs out to Main.
        m.select(); // Main>SinglePlayer
        assert_eq!(m.toggle(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // On Main, toggle closes.
        assert_eq!(m.toggle(), MenuAction::Closed);
        assert!(!m.visible);
    }

    #[test]
    fn draw_menu_skips_missing_pics_without_panic() {
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [9, 9, 9]);
        let mut faded = Image::new(320, 200, [9, 9, 9]);
        fade_screen(&mut faded, &pal);
        let mut m = Menu::new();
        m.open();
        // All pics absent: only M_Draw's Draw_FadeScreen shows, and no panic.
        let pics = MenuPics::default();
        draw_menu(&mut img, &m, &pics, None, 0.3, 0.0, &pal);
        assert_eq!(img.rgb, faded.rgb, "an all-empty MenuPics draws only the fade");

        // A hidden menu never draws (not even the fade).
        m.close();
        let before = img.rgb.clone();
        let solid = solid_pic(64, 16, 7);
        let pics2 = MenuPics { mainmenu: Some(solid), ..Default::default() };
        draw_menu(&mut img, &m, &pics2, None, 0.3, 0.0, &pal);
        assert_eq!(img.rgb, before, "a hidden menu must not draw");
    }

    #[test]
    fn options_labels_are_m_print_bronze_and_the_current_video_mode_white() {
        // M_Print draws cell c + 128 (the conchars' bronze half); M_PrintWhite
        // the plain cell. A conchars whose bronze 'S' (211) is index 5 and
        // plain 'S' (83) index 6 tells them apart on the Options "Screen size"
        // label and on the Video list.
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        let mut fill = |cell: usize, idx: u8| {
            let (cx, cy) = ((cell % 16) * 8, (cell / 16) * 8);
            for y in 0..8 {
                for x in 0..8 {
                    data[(cy + y) * 128 + cx + x] = idx;
                }
            }
        };
        for c in 32..127usize {
            fill(c, 6); // white half
            fill(c + 128, 5); // bronze half
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
        // "           Screen size" at (16, 56): the 'S' is the 12th character.
        let s_px = (56 + 3) * 320 + 16 + 11 * 8 + 3;
        assert_eq!(img.rgb[s_px], pal[5], "Options labels are M_Print (bronze)");
        // Video Options: the current mode white, the others bronze.
        m.sync_resolution(640, 400);
        m.cursor = ROW_VIDEO;
        m.select();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
        let row_px = |row: usize| (36 + row * 8 + 3) * 320 + 16 + 3;
        assert_eq!(img.rgb[row_px(2)], pal[6], "640x400 (current) is M_PrintWhite");
        assert_eq!(img.rgb[row_px(0)], pal[5], "320x200 is M_Print");
        assert_eq!(img.rgb[row_px(6)], pal[5], "1280x800 is M_Print");
    }

    #[test]
    fn draw_menu_draws_present_pics_over_background() {
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [9, 9, 9]);
        let mut m = Menu::new();
        m.open();
        // A present mainmenu graphic (opaque index 7 -> a non-background colour)
        // at (72,32) must change pixels there.
        let pics = MenuPics {
            mainmenu: Some(solid_pic(120, 80, 7)),
            ..Default::default()
        };
        draw_menu(&mut img, &m, &pics, None, 0.0, 0.0, &pal);
        // At scale 1 on the 320x200 frame, virtual (72,32) maps to pixel (72,32).
        let idx = 32 * img.w + 72;
        assert_eq!(img.rgb[idx], pal[7], "the mainmenu pic must paint at (72,32)");
        assert_ne!(img.rgb[idx], [9, 9, 9], "the pixel must differ from the background");
        // A corner well outside the pic stays background.
        assert_eq!(img.rgb[0], [9, 9, 9]);
    }

    #[test]
    fn draw_menu_cursor_frame_animates_with_time() {
        let pal = ramp_palette();
        let mut m = Menu::new();
        m.open();
        // Distinct colours per cursor frame so we can detect which frame drew.
        let mut menudot: [Option<crate::wad::Qpic>; 6] = Default::default();
        for (i, slot) in menudot.iter_mut().enumerate() {
            *slot = Some(solid_pic(20, 20, 10 + i as u8));
        }
        let pics = MenuPics { menudot, ..Default::default() };

        // The cursor sits at (54, 32). frame = (time*10) % 6.
        let cursor_idx = 32 * 320 + 54;
        let mut img0 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img0, &m, &pics, None, 0.0, 0.0, &pal); // frame 0 -> index 10
        assert_eq!(img0.rgb[cursor_idx], pal[10]);

        let mut img1 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img1, &m, &pics, None, 0.35, 0.0, &pal); // (3.5)->3 -> index 13
        assert_eq!(img1.rgb[cursor_idx], pal[13]);
        // The spinner runs on host_time ONLY: realtime moving on (the flashing
        // cursors' clock) leaves the menudot frame alone.
        let mut img2 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img2, &m, &pics, None, 0.35, 7.3, &pal);
        assert_eq!(img2.rgb[cursor_idx], pal[13], "menudot ignores realtime");
    }

    #[test]
    fn menu_cursor_flashes_at_4hz_on_realtime() {
        // M_Options_Draw & co: 12 + ((int)(realtime*4) & 1). Glyph 12 (blank)
        // for the first quarter second, 13 (the arrow) for the next, and so on
        // — 4 toggles per second, NOT the menudot's 10 Hz frame parity (the old
        // bug: the cursor followed (int)(host_time*10) % 6 & 1, 2.5x too fast).
        assert_eq!(menu_cursor_glyph(0.0), 12);
        assert_eq!(menu_cursor_glyph(0.10), 12, "0.10 s: the 10 Hz parity would say 13");
        assert_eq!(menu_cursor_glyph(0.24), 12);
        assert_eq!(menu_cursor_glyph(0.25), 13);
        assert_eq!(menu_cursor_glyph(0.49), 13);
        assert_eq!(menu_cursor_glyph(0.50), 12);
        assert_eq!(menu_cursor_glyph(0.75), 13);
        // Count the toggles over one second sampled at 1 ms: exactly 4 edges
        // (at 0.25/0.5/0.75/1.0), whatever the frame rate.
        let mut edges = 0;
        let mut prev = menu_cursor_glyph(0.0);
        for ms in 1..=1000 {
            let g = menu_cursor_glyph(ms as f64 / 1000.0);
            if g != prev {
                edges += 1;
            }
            prev = g;
        }
        assert_eq!(edges, 4, "the menu cursor toggles 4 times per real second");
        // A garbage clock is phase 0, never a panic.
        assert_eq!(menu_cursor_glyph(f64::NAN), 12);
        assert_eq!(menu_cursor_glyph(-3.0), 12);
    }

    #[test]
    fn draw_menu_options_cursor_follows_realtime_not_host_time() {
        // End to end through draw_menu: a conchars whose cell 13 is lit and
        // cell 12 is blank (like id's), cursor on the Options top row at
        // (200, 32). host_time is held where the OLD parity code would have
        // shown the arrow (frame 1 = 0.1 s); only realtime decides.
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + 13 * 8 + x] = 3; // cell 13 = (13, 0)
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options, cursor row 0
        let px = 32 * 320 + 200;
        let mut off = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut off, &m, &MenuPics::default(), Some(&conchars), 0.1, 0.1, &pal);
        assert_eq!(off.rgb[px], [0, 0, 0], "realtime 0.1 s: cursor phase blank");
        let mut on = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut on, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.3, &pal);
        assert_eq!(on.rgb[px], pal[3], "realtime 0.3 s: the arrow shows");
    }

    // -- options menu (MenuScreen::Options + adjust + draw) -----------------

    #[test]
    fn menu_options_enter_from_main_and_back() {
        let mut m = Menu::new();
        m.open();
        // Main > Options (cursor 2) switches to the Options screen, no host action.
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options);
        assert_eq!(m.cursor(), 0, "entering Options resets the cursor to the top row");
        assert!(m.visible);
        // Up wraps from row 0 to the last row (13), down wraps 13 -> 0: the cursor
        // covers all OPTIONS_ITEMS (14) rows.
        m.move_cursor(-1);
        assert_eq!(m.cursor(), OPTIONS_ITEMS - 1, "up from row 0 wraps to the last row");
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "down from the last row wraps to 0");
        // Walk down through every row once.
        for expect in 1..OPTIONS_ITEMS {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "past the last row wraps to 0");
        // Escape backs out of Options to Main (still visible).
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
    }

    #[test]
    fn menu_screen_size_row_steps_viewsize_by_10_clamped_30_to_120() {
        // M_AdjustSliders case 3: scr_viewsize += dir*10, clamped 30..=120 —
        // the Screen size row is viewsize, NOT the video mode (the old port
        // cycled render resolutions here; WinQuake keeps those in M_Video).
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_SCREENSIZE;
        assert_eq!(m.screen(), MenuScreen::Options);
        assert_eq!(m.viewsize(), 100.0, "default.cfg: viewsize 100");
        let mode = m.resolution();
        m.adjust(1);
        assert_eq!(m.viewsize(), 110.0);
        m.adjust(1);
        assert_eq!(m.viewsize(), 120.0);
        m.adjust(1);
        assert_eq!(m.viewsize(), 120.0, "clamped at 120 (no wrap)");
        for expect in [110.0, 100.0, 90.0, 80.0, 70.0, 60.0, 50.0, 40.0, 30.0, 30.0] {
            m.adjust(-1);
            assert_eq!(m.viewsize(), expect);
        }
        assert_eq!(m.resolution(), mode, "Screen size never touches the video mode");
        // Enter falls through to M_AdjustSliders(1) (menu2 + menu3), no host action.
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.viewsize(), 40.0);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2, MenuSound::Menu3]);
        // A zero delta is a no-op; adjust only acts on the Options screen.
        m.adjust(0);
        assert_eq!(m.viewsize(), 40.0);
        m.cancel(); // -> Main
        m.adjust(1);
        assert_eq!(m.viewsize(), 40.0, "adjust is a no-op off the Options screen");
        // Reset to defaults: default.cfg's `viewsize 100`.
        m.reset_defaults();
        assert_eq!(m.viewsize(), 100.0);
    }

    #[test]
    fn options_screen_size_slider_tracks_viewsize() {
        // M_Options_Draw: r = (scr_viewsize - 30) / (120 - 30); the knob (glyph
        // 131) sits at 220 + 72*r on the Screen-size row (y = 56).
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[(8 * 8 + y) * 128 + 3 * 8 + x] = 3; // cell 131 = (3, 8)
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        let knob_x = |m: &Menu| {
            let mut img = Image::new(320, 200, [0, 0, 0]);
            draw_menu(&mut img, m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
            (0..320).find(|&x| img.rgb[56 * 320 + x] == pal[3]).expect("knob drawn")
        };
        assert_eq!(knob_x(&m), 276, "viewsize 100: r = 70/90 -> 220 + 56");
        m.set_viewsize(30.0);
        assert_eq!(knob_x(&m), 220, "viewsize 30: the left end");
        m.set_viewsize(120.0);
        assert_eq!(knob_x(&m), 292, "viewsize 120: the right end");
    }

    #[test]
    fn sizeup_sizedown_and_the_viewsize_cvar_bound_like_scr_calcrefdef() {
        let mut m = Menu::new();
        m.size_up();
        assert_eq!(m.viewsize(), 110.0);
        m.size_up();
        m.size_up();
        assert_eq!(m.viewsize(), 120.0, "sizeup stops at 120");
        for _ in 0..20 {
            m.size_down();
        }
        assert_eq!(m.viewsize(), 30.0, "sizedown stops at 30");
        // The console can set any value in range (not just multiples of 10);
        // out-of-range and garbage clamp like SCR_CalcRefdef's bound.
        m.set_viewsize(55.0);
        assert_eq!(m.viewsize(), 55.0);
        m.size_up();
        assert_eq!(m.viewsize(), 65.0);
        m.set_viewsize(7.0);
        assert_eq!(m.viewsize(), 30.0);
        m.set_viewsize(1e9);
        assert_eq!(m.viewsize(), 120.0);
        m.set_viewsize(f32::NAN);
        assert_eq!(m.viewsize(), 30.0, "atof garbage = 0 -> the minimum");
        // default.cfg binds + and = to sizeup and - to sizedown, as ordinary
        // (rebindable) bindings that Customize controls doesn't list.
        let m = Menu::new();
        assert_eq!(m.action_for_key(b'+'), Some(BIND_SIZEUP));
        assert_eq!(m.action_for_key(b'='), Some(BIND_SIZEUP));
        assert_eq!(m.action_for_key(b'-'), Some(BIND_SIZEDOWN));
        // Customize controls (the BINDNAMES rows) never lists them.
        let listed = (0..NUM_BINDNAMES).flat_map(|c| m.find_keys_for_command(c));
        for k in listed.flatten() {
            assert!(![b'+', b'=', b'-'].contains(&k), "key {k} is not a Keys-screen row");
        }
    }

    #[test]
    fn menu_adjust_clamps_mouse_and_volume() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Mouse Speed row: default sensitivity 3 -> 1.0x multiplier.
        m.cursor = ROW_MOUSESPEED;
        assert!((m.sensitivity() - SENS_DEFAULT).abs() < 1e-6);
        assert!((m.mouse_sensitivity() - 1.0).abs() < 1e-6, "default mouse is 1.0x");
        // Decreasing clamps at SENS_MIN (1), never below.
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.sensitivity() - SENS_MIN).abs() < 1e-6);
        // Increasing clamps at SENS_MAX (11).
        for _ in 0..60 {
            m.adjust(1);
        }
        assert!((m.sensitivity() - SENS_MAX).abs() < 1e-6);
        assert!(m.mouse_sensitivity() > 1.0, "max sensitivity is more than default");

        // Sound Volume row: default 0.7.
        m.cursor = ROW_SNDVOLUME;
        assert!((m.volume() - VOLUME_DEFAULT).abs() < 1e-6, "default volume is 0.7");
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.volume() - VOLUME_MIN).abs() < 1e-6, "min volume is silent");
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.volume() - VOLUME_MAX).abs() < 1e-6, "max volume is full gain");
    }

    #[test]
    fn draw_menu_options_screen_draws_without_panic() {
        let pal = ramp_palette();
        // A conchars atlas where every glyph texel is the lit index 3 (except the
        // byte-0 cell), so any drawn label/value paints index-3 pixels.
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };

        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(m.screen(), MenuScreen::Options);

        // Only the title pic is present (the cursor is a conchars glyph now, drawn
        // at x=200, not a menudot).
        let pics = MenuPics {
            p_option: Some(solid_pic(120, 24, 5)),
            ..Default::default()
        };

        let bg = [9u8, 9, 9];
        let mut img = Image::new(320, 200, bg);
        let before = img.rgb.clone();
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        // The Options screen must change pixels over the known background.
        assert_ne!(img.rgb, before, "the Options screen must draw something");
        // The title plaque (index 5) paints centered near the top: at virtual
        // (100, 4) with a 120-wide pic centered ((320-120)/2 = 100).
        let title_idx = 4 * img.w + 100;
        assert_eq!(img.rgb[title_idx], pal[5], "the OPTIONS title must paint at the top");
        // The flashing cursor (conchars glyph 12/13, all-lit in this atlas -> index
        // 3) sits at virtual (200, 32) on the top row.
        let cursor_idx = 32 * img.w + 200;
        assert_eq!(img.rgb[cursor_idx], pal[3], "the cursor glyph must paint at x=200 on the top row");
        // A label glyph (index 3) paints on the first row at the label column x=16
        // (the "Customize controls" row is right-justified, so its first non-space
        // glyph lands a few cells in; check at x=48 which is inside the text).
        let label_idx = 32 * img.w + 48;
        assert_eq!(img.rgb[label_idx], pal[3], "the first Options label must paint");
        // A slider on the Screen-size row (row 3, y=32+3*8=56): glyphs at x>=212
        // (the left cap is at 220-8=212). Check the left cap pixel.
        let slider_idx = 56 * img.w + 212;
        assert_eq!(img.rgb[slider_idx], pal[3], "the Screen-size slider must paint at y=56");

        // The SAME screen also renders correctly at a LARGER framebuffer (640x400,
        // scale 2): it must not panic and must draw the title + cursor scaled.
        let mut big = Image::new(640, 400, bg);
        let big_before = big.rgb.clone();
        draw_menu(&mut big, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_ne!(big.rgb, big_before, "the Options screen draws at 640x400 too");
        // At scale 2 the cursor's virtual (200,32) maps to pixel (400,64).
        let big_cursor_idx = 64 * big.w + 400;
        assert_eq!(big.rgb[big_cursor_idx], pal[3], "cursor scales to (400,64) at 640x400");

        // Missing conchars leaves labels/widgets/cursor blank but still draws the
        // title; no panic.
        let mut img2 = Image::new(320, 200, bg);
        draw_menu(&mut img2, &m, &pics, None, 0.0, 0.0, &pal);
        assert_eq!(img2.rgb[title_idx], pal[5], "title still draws without conchars");
        assert_eq!(img2.rgb[cursor_idx], bg, "cursor needs conchars (blank without it)");
    }

    // -- new Options widgets / Help / Quit pure helpers ----------------------

    #[test]
    fn slider_knob_offset_maps_fraction_to_position() {
        // The knob travels from 0 (fraction 0) to (SLIDER_RANGE-1)*8 (fraction 1).
        let span = (SLIDER_RANGE - 1) as f32 * 8.0; // 9*8 = 72.
        assert_eq!(slider_knob_offset(0.0), 0.0, "fraction 0 -> left of the trough");
        assert_eq!(slider_knob_offset(1.0), span, "fraction 1 -> right end of the segments");
        assert!((slider_knob_offset(0.5) - span * 0.5).abs() < 1e-6, "midpoint is halfway");
        // Out-of-range fractions clamp to [0,1]; non-finite is treated as 0.
        assert_eq!(slider_knob_offset(-3.0), 0.0, "below 0 clamps to the left");
        assert_eq!(slider_knob_offset(7.0), span, "above 1 clamps to the right");
        assert_eq!(slider_knob_offset(f32::NAN), 0.0, "NaN is treated as 0");
        assert_eq!(slider_knob_offset(f32::INFINITY), 0.0, "infinity is treated as 0");
        // Monotone increasing across the range.
        let mut prev = -1.0;
        for i in 0..=10 {
            let v = slider_knob_offset(i as f32 / 10.0);
            assert!(v >= prev, "knob position must be non-decreasing in the fraction");
            prev = v;
        }
    }

    #[test]
    fn checkbox_text_is_on_or_off() {
        assert_eq!(checkbox_text(true), "on");
        assert_eq!(checkbox_text(false), "off");
    }

    #[test]
    fn help_page_wrap_clamps_into_range() {
        // In-range stays put.
        for p in 0..NUM_HELP_PAGES {
            assert_eq!(help_page_wrap(p as i32), p);
        }
        // Past the last page wraps to 0; below 0 wraps to the last page.
        assert_eq!(help_page_wrap(NUM_HELP_PAGES as i32), 0, "one past the end wraps to 0");
        assert_eq!(help_page_wrap(-1), NUM_HELP_PAGES - 1, "below 0 wraps to the last page");
        // Large magnitudes wrap modulo the page count, never panic / out-of-range.
        assert_eq!(help_page_wrap(13), (13 % NUM_HELP_PAGES as i32) as usize);
        assert!(help_page_wrap(i32::MAX) < NUM_HELP_PAGES);
        assert!(help_page_wrap(i32::MIN) < NUM_HELP_PAGES);
    }

    #[test]
    fn options_cursor_wraps_over_all_fourteen_rows() {
        // The cursor must visit every one of the 14 OPTIONS_ITEMS rows (id's
        // 13 + Web extras) and wrap.
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(MenuScreen::Options.item_count(), 14);
        assert_eq!(m.cursor(), 0);
        let mut seen = [false; OPTIONS_ITEMS];
        for _ in 0..OPTIONS_ITEMS {
            seen[m.cursor()] = true;
            m.move_cursor(1);
        }
        assert!(seen.iter().all(|&v| v), "every Options row must be reachable");
        assert_eq!(m.cursor(), 0, "a full lap returns to row 0");
        // A big positive delta wraps modulo 14.
        m.cursor = 0;
        m.move_cursor(43); // 43 % 14 = 1
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn options_sliders_and_checkboxes_adjust_per_row() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Brightness (gamma) row: matching the C `v_gamma -= dir*0.05`, RIGHT
        // brightens (gamma DOWN toward 0.5), LEFT dims (gamma UP toward 1.0).
        m.cursor = ROW_BRIGHTNESS;
        assert!((m.gamma() - GAMMA_DEFAULT).abs() < 1e-6);
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.gamma() - GAMMA_MIN).abs() < 1e-6, "right clamps gamma at 0.5 (brightest)");
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.gamma() - GAMMA_MAX).abs() < 1e-6, "left clamps gamma at 1.0 (dimmest)");

        // CD Music Volume row: 0..=1.
        m.cursor = ROW_CDVOLUME;
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.bgm_volume() - BGM_MIN).abs() < 1e-6);
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.bgm_volume() - BGM_MAX).abs() < 1e-6);

        // Checkboxes toggle regardless of direction (matches the C). Always Run
        // starts ON (this port's default); the rest start off.
        for (row, getter, initial) in [
            (ROW_ALWAYSRUN, Menu::always_run as fn(&Menu) -> bool, true),
            (ROW_INVERTMOUSE, Menu::invert_mouse, false),
            (ROW_LOOKSPRING, Menu::lookspring, false),
            (ROW_LOOKSTRAFE, Menu::lookstrafe, false),
        ] {
            m.cursor = row;
            assert_eq!(getter(&m), initial, "checkbox row {row} starts at its default");
            m.adjust(1);
            assert_eq!(getter(&m), !initial, "right toggles it");
            m.adjust(-1);
            assert_eq!(getter(&m), initial, "left toggles it back");
        }
    }

    #[test]
    fn options_enter_actions_console_defaults_and_stubs() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Go to console: closes the menu, returns OpenConsole.
        m.cursor = ROW_CONSOLE;
        assert_eq!(m.select(), MenuAction::OpenConsole);
        assert!(!m.visible, "Go to console closes the menu");

        // Reset to defaults: exec default.cfg restores what that file sets
        // (viewsize/gamma/volume/sensitivity + the binds) and nothing else.
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_MOUSESPEED;
        m.adjust(1);
        m.adjust(1);
        m.cursor = ROW_BRIGHTNESS;
        m.adjust(1);
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1);
        m.cursor = ROW_CDVOLUME;
        m.adjust(-1);
        m.cursor = ROW_ALWAYSRUN;
        m.adjust(1); // toggles OFF (Always Run defaults on in this port)
        m.cursor = ROW_INVERTMOUSE;
        m.adjust(1);
        m.cursor = ROW_LOOKSPRING;
        m.adjust(1);
        m.cursor = ROW_LOOKSTRAFE;
        m.adjust(1);
        assert!(m.sensitivity() != SENS_DEFAULT && !m.always_run());
        m.cursor = ROW_DEFAULTS;
        assert_eq!(m.select(), MenuAction::ResetDefaults);
        assert!((m.sensitivity() - SENS_DEFAULT).abs() < 1e-6, "sensitivity 3");
        assert!((m.gamma() - GAMMA_DEFAULT).abs() < 1e-6, "gamma 1.0");
        assert!((m.volume() - VOLUME_DEFAULT).abs() < 1e-6, "volume 0.7");
        // default.cfg never touches these: they keep the player's values.
        assert!((m.bgm_volume() - 0.9).abs() < 1e-6, "bgmvolume kept");
        assert!(!m.always_run(), "cl_forwardspeed kept (Always Run stays off)");
        assert!(m.invert_mouse() && m.lookspring() && m.lookstrafe(), "m_pitch/lookspring/lookstrafe kept");

        // Customize controls opens the Keys screen (M_Menu_Keys_f); Escape
        // returns to Options (M_Keys_Key K_ESCAPE -> M_Menu_Options_f).
        m.cursor = ROW_CONTROLS;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Keys, "Customize controls enters Keys");
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "Esc on Keys returns to Options");

        // Video Options opens the mode list (M_Menu_Video_f) with the cursor on
        // the current preset; Escape returns to Options (VID_MenuKey K_ESCAPE).
        m.cursor = ROW_VIDEO;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Video, "Video Options enters the mode list");
        assert_eq!(m.cursor(), m.res_preset, "video cursor starts on the current mode");
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "Esc on Video returns to Options");

        // Enter on an analog row nudges it right (the C falls through to
        // M_AdjustSliders(1)).
        m.cursor = ROW_SNDVOLUME;
        let before = m.volume();
        m.select();
        assert!(m.volume() > before, "Enter on Sound Volume nudges it up");
    }

    // -- the port's Web extras ----------------------------------------------

    #[test]
    fn web_extras_default_off_toggle_like_checkboxes_and_back_out_to_their_row() {
        let mut m = Menu::new();
        assert_eq!(m.extras(), Extras::default(), "every extra defaults off");
        assert_eq!(m.extras().bits(), 0);
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_EXTRAS;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Extras, "Web extras opens its screen");
        assert_eq!(m.cursor(), 0);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2], "entered with m_entersound");

        // Checkbox rows: left and right both flip (menu3 each); Enter flips
        // with menu2 + menu3, like an Options checkbox row.
        let rows = &WEB_EXTRAS;
        for (i, e) in rows.iter().map(|r| r.extra).enumerate() {
            m.cursor = i;
            assert!(!m.extras().get(e));
            m.adjust(1);
            assert!(m.extras().get(e), "right turns {e:?} on");
            m.adjust(1);
            assert!(!m.extras().get(e), "the direction is ignored: right again flips it off");
            m.adjust(-1);
            assert!(m.extras().get(e), "left flips it too");
            assert_eq!(m.take_sounds(), vec![MenuSound::Menu3; 3]);
            m.select();
            assert!(!m.extras().get(e), "Enter flips it");
            assert_eq!(m.take_sounds(), vec![MenuSound::Menu2, MenuSound::Menu3]);
        }
        assert_eq!(m.extras(), Extras::default());

        // The cursor wraps over this build's rows (menu1 per move).
        m.cursor = 0;
        m.move_cursor(-1);
        assert_eq!(m.cursor(), rows.len() - 1, "up from the top wraps to the last row");
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu1; 2]);

        // Escape: back to Options on the Web extras row (options_cursor keeps
        // its place in the C), with m_entersound.
        m.cursor = 1;
        m.adjust(1); // Show FPS on
        m.take_sounds();
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!((m.screen(), m.cursor()), (MenuScreen::Options, ROW_EXTRAS));
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);

        // They are not default.cfg cvars: Reset to defaults keeps them, and
        // so does a navigation reset (re-boot / New Game).
        m.cursor = ROW_DEFAULTS;
        assert_eq!(m.select(), MenuAction::ResetDefaults);
        assert!(m.extras().show_fps, "Reset to defaults leaves the extras alone");
        m.reset_nav();
        assert!(m.extras().show_fps, "reset_nav keeps them");

        // The console commands' setter and the page's restore.
        m.set_extra(Extra::Uncapped, true);
        assert_eq!(m.extras().bits(), 0b011);
        m.set_extras(Extras::default());
        assert_eq!(m.extras(), Extras::default());
    }

    #[test]
    fn web_extras_bits_round_trip() {
        for bits in 0..16u32 {
            let e = Extras::from_bits(bits);
            assert_eq!(e.bits(), bits, "bits {bits:04b}");
            let rows = [Extra::Uncapped, Extra::ShowFps, Extra::ExactPersp, Extra::Scaled2d];
            for (i, x) in rows.into_iter().enumerate() {
                assert_eq!(e.get(x), bits & (1 << i) != 0, "bit {i} is {x:?}");
            }
        }
        assert_eq!(Extras::from_bits(0xffff_fff0), Extras::default(), "unknown bits are ignored");
        assert_eq!(MenuScreen::Extras.item_count(), 4);
    }

    #[test]
    fn web_extras_table_lists_each_extra_once_in_the_page_idiom() {
        let extras: Vec<Extra> = WEB_EXTRAS.iter().map(|w| w.extra).collect();
        assert_eq!(extras, [Extra::Uncapped, Extra::ShowFps, Extra::ExactPersp, Extra::Scaled2d], "bit order");
        for w in &WEB_EXTRAS {
            assert!(w.cvar.starts_with("wasm_"), "{}: not an id name", w.cvar);
            assert_eq!(w.label.len(), OPTIONS_LABELS[ROW_VIDEO].len(), "{}: label column", w.cvar);
            for line in extras_help_lines(w) {
                assert!(line.len() <= 38, "{line:?} fits the 320-wide page");
            }
        }
    }

    #[test]
    fn web_extras_screen_draws_in_the_options_idiom() {
        // Bronze (M_Print, c+128) is index 5, white (M_PrintWhite) index 6;
        // glyph 12 blank, 13 the cursor (index 7), as in id's conchars.
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        let mut fill = |cell: usize, idx: u8| {
            let (cx, cy) = ((cell % 16) * 8, (cell / 16) * 8);
            for y in 0..8 {
                for x in 0..8 {
                    data[(cy + y) * 128 + cx + x] = idx;
                }
            }
        };
        for c in 33..127usize {
            fill(c, 6);
            fill(c + 128, 5);
        }
        fill(13, 7);
        let cc = crate::wad::Qpic { width: 128, height: 128, data };
        let pics = MenuPics {
            qplaque: Some(solid_pic(32, 144, 9)),
            p_option: Some(solid_pic(120, 24, 8)),
            ..Default::default()
        };
        let px = |img: &Image, x: usize, y: usize| img.rgb[(y + 3) * 320 + x + 3];

        // Options: the port's row is the 14th, at y=136 (the C's _WIN32 row),
        // right-justified with id's labels ("Web extras" ends at x=184).
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &pics, Some(&cc), 0.0, 0.0, &pal);
        assert_eq!(OPTIONS_LABELS[ROW_EXTRAS].len(), OPTIONS_LABELS[ROW_VIDEO].len());
        assert_eq!(px(&img, 16 + 12 * 8, 136), pal[5], "'W' of Web extras, bronze, y=136");
        assert_eq!(px(&img, 16 + 21 * 8, 136), pal[5], "its 's' in the last label column");

        // The Extras screen: plaque + OPTIONS title, a white header at y=32,
        // the rows from y=48 (bronze labels, "off" at x=220), the cursor at
        // x=200 while the 4 Hz blink shows it, the help lines under the list.
        m.cursor = ROW_EXTRAS;
        m.select();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &pics, Some(&cc), 0.0, 0.3, &pal);
        assert_eq!(img.rgb[4 * 320 + 16], pal[9], "qplaque at (16,4)");
        assert_eq!(img.rgb[4 * 320 + 100], pal[8], "the OPTIONS title centred at y=4");
        let hx = (320 - EXTRAS_HEADER.len() * 8) / 2;
        assert_eq!(px(&img, hx, 32), pal[6], "the header is M_PrintWhite");
        for (i, label) in WEB_EXTRAS.iter().map(|r| r.label).enumerate() {
            let y = 48 + i * 8;
            let first = label.bytes().position(|b| b != b' ').unwrap();
            assert_eq!(px(&img, 16 + first * 8, y), pal[5], "row {i} label bronze");
            assert_eq!(px(&img, 220, y), pal[5], "row {i} checkbox 'off' at x=220");
        }
        assert_eq!(px(&img, 200, 48), pal[7], "the cursor on row 0 at x=200 (realtime 0.3: on)");
        let help = WEB_EXTRAS[0].help;
        let hx0 = (320 - help[0].len() * 8) / 2;
        assert_eq!(px(&img, hx0, 88), pal[5], "row 0's help, bronze, from y=88");
        // realtime 0.1: the blink is off (glyph 12, blank).
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &pics, Some(&cc), 0.0, 0.1, &pal);
        assert_eq!(px(&img, 200, 48), pal[0], "the cursor blinks");
        // "on" replaces "off" once toggled; the help follows the cursor.
        m.adjust(1);
        m.move_cursor(1);
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &pics, Some(&cc), 0.0, 0.3, &pal);
        assert_eq!(px(&img, 220 + 16, 48), pal[0], "\"on\" is two characters");
        assert_eq!(px(&img, 220 + 16, 56), pal[5], "\"off\" is three");
        let help1 = WEB_EXTRAS[1].help;
        let hx1 = (320 - help1[0].len() * 8) / 2;
        assert_eq!(px(&img, hx1, 88), pal[5], "row 1's help once the cursor moves");
        assert_eq!(px(&img, 200, 56), pal[7], "the cursor on row 1");
        // Without conchars only the pics draw; nothing panics.
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &pics, None, 0.0, 0.3, &pal);
        assert_eq!(img.rgb[4 * 320 + 100], pal[8]);
    }

    #[test]
    fn help_screen_pages_and_backs_out() {
        let mut m = Menu::new();
        m.open();
        // Main > Help.
        m.cursor = 3;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Help);
        assert_eq!(m.help_page(), 0);
        // Right/up advance the page (page(+1) = next), wrapping at NUM_HELP_PAGES.
        for expect in 1..NUM_HELP_PAGES {
            m.page(1);
            assert_eq!(m.help_page(), expect);
        }
        m.page(1);
        assert_eq!(m.help_page(), 0, "past the last page wraps to 0");
        // Left/down go back (page(-1) = prev), wrapping below 0.
        m.page(-1);
        assert_eq!(m.help_page(), NUM_HELP_PAGES - 1, "below 0 wraps to the last page");
        // Up/down on the Help screen also page (M_Help_Key): the host passes
        // up = move_cursor(-1) / down = move_cursor(+1), and per the C UP advances
        // (m_help_page++) while DOWN goes back (m_help_page--).
        m.help_page = 0;
        m.move_cursor(1); // down -> previous page (wraps below 0)
        assert_eq!(
            m.help_page(),
            NUM_HELP_PAGES - 1,
            "down pages backward on Help (wraps to the last page)"
        );
        m.move_cursor(-1); // up -> next page (wraps back to 0)
        assert_eq!(m.help_page(), 0, "up pages forward on Help");
        // page() is a no-op off the Help screen.
        m.cancel(); // -> Main
        assert_eq!(m.screen(), MenuScreen::Main);
        m.page(1);
        assert_eq!(m.help_page(), 0, "page() does nothing off the Help screen");
    }

    #[test]
    fn menu_sounds_follow_the_c_triggers() {
        let mut m = Menu::new();
        // Opening latches m_entersound -> menu2.
        m.open();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Cursor moves play menu1 per press (M_Main_Key K_UP/DOWNARROW).
        m.move_cursor(1);
        m.move_cursor(-1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu1, MenuSound::Menu1]);
        // Entering a submenu plays menu2 (M_Main_Key K_ENTER latches it).
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Left/right adjust plays menu3 (M_AdjustSliders' unconditional
        // S_LocalSound) — even when the cursor sits on an action row.
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu3]);
        m.cursor = ROW_CONTROLS;
        m.adjust(1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu3], "menu3 plays on action rows too");
        // Enter on a slider row: m_entersound (menu2) AND M_AdjustSliders' menu3.
        m.cursor = ROW_BRIGHTNESS;
        m.select();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2, MenuSound::Menu3]);
        // Escape back to Main: M_Menu_Main_f latches menu2.
        m.cancel();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Going to the console CLOSES the menu — the C's latched entersound
        // never fires (M_Draw stops running): silent.
        m.cursor = 2;
        m.select(); // -> Options (menu2)
        m.take_sounds();
        m.cursor = ROW_CONSOLE;
        assert_eq!(m.select(), MenuAction::OpenConsole);
        assert_eq!(m.take_sounds(), vec![], "closing into the console is silent");
        // The sample names match S_LocalSound's literals.
        assert_eq!(MenuSound::Menu1.sample(), "misc/menu1.wav");
        assert_eq!(MenuSound::Menu2.sample(), "misc/menu2.wav");
        assert_eq!(MenuSound::Menu3.sample(), "misc/menu3.wav");
        // The queue is bounded even when the host never drains.
        m.open();
        for _ in 0..100 {
            m.move_cursor(1);
        }
        assert!(m.take_sounds().len() <= MENU_SOUND_CAP);
    }

    #[test]
    fn load_save_screens_slots_gate_and_actions() {
        let mut m = Menu::new();
        m.open();
        m.select(); // -> SinglePlayer

        // Item 2 = Save: REFUSED while no game is running (M_Menu_Save_f's
        // `if (!sv.active) return`). The entersound was latched before the
        // early return, so menu2 still plays.
        m.cursor = 2;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer, "Save refuses without a game");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);

        // Item 1 = Load (M_Menu_Load_f) opens with all slots unused.
        m.cursor = 1;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Load);
        for i in 0..MAX_SAVEGAMES {
            assert!(!m.slot_loadable(i), "slot {i} must start unused");
            assert_eq!(m.save_comment(i), "");
        }
        // 12 slots; the cursor wraps over them, and left/right pair with
        // up/down (M_Load_Key K_LEFTARROW == K_UPARROW).
        for expect in [1, 2, 3] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        m.adjust(-1);
        assert_eq!(m.cursor(), 2);
        m.cursor = MAX_SAVEGAMES - 1;
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "load cursor wraps over MAX_SAVEGAMES");
        // Enter on an unused slot: menu2 plays but nothing happens — the C's
        // `if (!loadable[load_cursor]) return`.
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Load, "unused slot doesn't leave the screen");
        assert!(m.visible);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2], "Load Enter still plays menu2");

        // Host fills slot 2 (a savegame engine ran M_ScanSaves): it becomes
        // loadable and Enter emits LoadSlot(2) + closes the menu.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[2] = "e1m1: Slipgate Complex".to_string();
        m.set_save_comments(comments);
        assert!(m.slot_loadable(2));
        assert!(!m.slot_loadable(3));
        m.cursor = 2;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::LoadSlot(2));
        assert!(!m.visible, "a real load closes the menu (m_state = m_none)");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);

        // Escape on Load returns to SinglePlayer (M_Load_Key K_ESCAPE).
        m.open();
        m.select(); // -> SinglePlayer (cursor 0)
        m.cursor = 1;
        m.select(); // -> Load
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);

        // With a game running, Save opens; Enter emits SaveSlot for the
        // highlighted slot, closes the menu, and is SILENT (M_Save_Key K_ENTER
        // plays nothing).
        m.set_game_active(true);
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Save);
        m.cursor = 5;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::SaveSlot(5));
        assert!(!m.visible, "Save Enter closes the menu like the C");
        assert_eq!(m.take_sounds(), vec![], "Save Enter is silent in the C");

        // save_comment is total (out-of-range = empty).
        assert_eq!(m.save_comment(MAX_SAVEGAMES + 3), "");
    }

    #[test]
    fn video_screen_lists_and_applies_presets() {
        let mut m = Menu::new();
        m.sync_resolution(RESOLUTION_PRESETS[2].0, RESOLUTION_PRESETS[2].1);
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_VIDEO;
        m.select(); // -> Video
        assert_eq!(m.screen(), MenuScreen::Video);
        assert_eq!(m.cursor(), 2, "cursor opens on the current mode");
        // Move to another mode and apply it: VID_MenuKey K_ENTER -> VID_SetMode.
        m.move_cursor(1);
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::ResolutionChanged);
        assert_eq!(m.resolution(), RESOLUTION_PRESETS[3]);
        assert_eq!(
            m.take_sounds(),
            vec![MenuSound::Menu1],
            "VID_MenuKey K_ENTER plays menu1 (not menu2)"
        );
        assert_eq!(m.screen(), MenuScreen::Video, "the mode list stays up after applying");
        // The cursor wraps over the preset list; left/right also step it.
        m.cursor = RESOLUTION_PRESETS.len() - 1;
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0);
        let mode = m.resolution();
        m.adjust(1);
        assert_eq!(m.cursor(), 1, "video left/right move the line");
        assert_eq!(m.resolution(), mode, "...but only Enter sets the mode");
    }

    #[test]
    fn keys_screen_lists_rebinds_and_unbinds() {
        let mut m = Menu::new();
        // The defaults include id's default.cfg keys and the port's WASD layout.
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(K_UPARROW), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(K_MOUSE1), Some(BIND_ATTACK));
        assert_eq!(m.action_for_key(K_CTRL), Some(BIND_ATTACK));
        assert_eq!(m.action_for_key(K_SPACE), Some(BIND_JUMP));
        assert_eq!(m.action_for_key(b'/'), Some(BIND_CHANGEWEAPON));
        assert_eq!(m.action_for_key(b'c'), Some(BIND_MOVEDOWN));
        assert_eq!(m.action_for_key(K_SHIFT), Some(BIND_SPEED));
        // find_keys_for_command returns up to two keys in keynum order
        // (M_FindKeysForCommand scans 0..256 ascending: 'w' = 119 < 128).
        assert_eq!(m.find_keys_for_command(BIND_FORWARD), [Some(b'w'), Some(K_UPARROW)]);

        // Navigate Main > Options > Customize controls.
        m.open();
        m.cursor = 2;
        m.select();
        m.cursor = ROW_CONTROLS;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Keys);
        assert!(!m.bind_grabbing());

        // Enter on "+attack" (row 0, already two keys: CTRL + MOUSE1): the C
        // unbinds first, then grabs.
        m.cursor = BIND_ATTACK;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert!(m.bind_grabbing(), "Enter starts the bind grab");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        assert_eq!(
            m.find_keys_for_command(BIND_ATTACK),
            [None, None],
            "two-key rows unbind before grabbing"
        );
        // Deliver the grabbed key: 'x' binds to +attack, menu1 plays.
        m.bind_key(b'x');
        assert!(!m.bind_grabbing());
        assert_eq!(m.action_for_key(b'x'), Some(BIND_ATTACK));
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu1]);

        // Escape during a grab cancels without binding.
        m.select();
        assert!(m.bind_grabbing());
        m.bind_key(K_ESCAPE);
        assert!(!m.bind_grabbing());
        assert_eq!(m.action_for_key(K_ESCAPE), None, "Escape never binds");
        // The console key is refused too (the C's `k != '`'` check).
        m.select();
        m.bind_key(b'`');
        assert_eq!(m.action_for_key(b'`'), None, "backtick never binds");

        // cancel() during a grab also just ends the grab (screen stays).
        m.select();
        assert!(m.bind_grabbing());
        assert_eq!(m.cancel(), MenuAction::None);
        assert!(!m.bind_grabbing());
        assert_eq!(m.screen(), MenuScreen::Keys, "Esc in grab stays on Keys");

        // Backspace unbinds the highlighted command (menu2).
        m.cursor = BIND_FORWARD;
        m.take_sounds();
        m.keys_backspace();
        assert_eq!(m.find_keys_for_command(BIND_FORWARD), [None, None]);
        assert_eq!(m.action_for_key(b'w'), None);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // ...and left/right move the keys cursor like up/down (M_Keys_Key).
        m.adjust(1);
        assert_eq!(m.cursor(), BIND_FORWARD + 1);

        // Reset to defaults re-execs default.cfg: the bindings come back.
        m.reset_defaults();
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(b'x'), None, "custom binds reset too");
    }

    /// CENSUS L14: with a game running (`sv.active`), New Game asks
    /// SCR_ModalMessage("Are you sure you want to\nstart a new game?\n"):
    /// only y (yes), n or Escape (no) answer; no leaves the Single Player menu.
    #[test]
    fn new_game_asks_first_while_a_game_runs() {
        let mut m = Menu::new();
        m.open();
        m.select(); // Main > Single Player
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        m.set_server_active(true);
        assert_eq!(m.select(), MenuAction::None, "asks instead of starting");
        assert!(m.new_game_confirm());
        assert_eq!(m.select(), MenuAction::None, "Enter doesn't answer");
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "arrows don't move behind the modal");
        assert_eq!(m.cancel(), MenuAction::None, "Escape = no");
        assert!(!m.new_game_confirm() && m.visible);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer, "no: back on Single Player");
        m.select();
        assert_eq!(m.quit_no(), MenuAction::None, "n = no");
        assert!(!m.new_game_confirm());
        m.select();
        assert_eq!(m.quit_yes(), MenuAction::NewGame, "y starts it");
        assert!(!m.visible && !m.new_game_confirm());
        // No game running (the attract loop): straight in, as before.
        let mut m = Menu::new();
        m.open();
        m.select();
        assert_eq!(m.select(), MenuAction::NewGame);
    }

    /// The host's re-boot sites (boot / boot_demo / boot_attract / New Game /
    /// `map`) reset the menu with [`Menu::reset_nav`]: navigation goes back to
    /// boot state but EVERY user choice survives — WinQuake's `map start`
    /// (M_SinglePlayer "New Game") never resets cvars or `keybindings[]` (they
    /// are host state, persisted by Host_WriteConfiguration). This locks the
    /// "rebind keys, set Always Run, then New Game" flow as a contract.
    #[test]
    fn reset_nav_keeps_user_choices_and_resets_navigation() {
        let mut m = Menu::new();
        m.open();
        // Change every class of user choice through the real menu paths.
        m.cursor = 2;
        m.select(); // Main > Options
        m.cursor = ROW_SCREENSIZE;
        m.adjust(-1); // viewsize 100 -> 90
        m.cursor = ROW_BRIGHTNESS;
        m.adjust(1); // v_gamma 1.0 -> 0.95 (RIGHT brightens: -= 0.05)
        m.cursor = ROW_MOUSESPEED;
        m.adjust(1); // sensitivity 3 -> 3.5
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1); // volume 0.7 -> 0.6
        m.cursor = ROW_CDVOLUME;
        m.adjust(-1); // bgmvolume 1.0 -> 0.9
        for row in [ROW_ALWAYSRUN, ROW_INVERTMOUSE, ROW_LOOKSPRING, ROW_LOOKSTRAFE] {
            m.cursor = row;
            m.adjust(1); // toggles flip regardless of direction (Always Run: on -> OFF)
        }
        // Rebind through the real grab path: Options > Customize controls,
        // Enter on "change weapon" (one key bound, '/' — no unbind-first), 'j'.
        m.cursor = ROW_CONTROLS;
        m.select(); // -> Keys
        m.cursor = BIND_CHANGEWEAPON;
        m.select(); // starts the grab
        m.bind_key(b'j');
        assert_eq!(m.action_for_key(b'j'), Some(BIND_CHANGEWEAPON));
        // Host-mirrored externals: slot comments + the Save gate.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[3] = "e1m1 quick".to_string();
        m.set_save_comments(comments);
        m.set_game_active(true);

        // The re-boot reset.
        m.reset_nav();

        // Navigation is back at boot state...
        assert!(!m.visible, "reset_nav leaves the menu closed");
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
        assert_eq!(m.help_page(), 0);
        assert!(!m.bind_grabbing(), "a pending bind grab is cancelled");
        assert!(m.take_sounds().is_empty(), "queued menu sounds are dropped");
        // ...but EVERY user choice survives.
        assert_eq!(m.viewsize(), 90.0, "Screen size (viewsize) survives");
        assert!((m.gamma() - 0.95).abs() < 1e-6, "Brightness survives");
        assert!((m.sensitivity() - 3.5).abs() < 1e-6, "Mouse speed survives");
        assert!((m.volume() - 0.6).abs() < 1e-6, "Sound volume survives");
        assert!((m.bgm_volume() - 0.9).abs() < 1e-6, "CD volume survives");
        assert!(!m.always_run(), "Always Run (toggled off its on-default) survives");
        assert!(m.invert_mouse(), "Invert Mouse survives");
        assert!(m.lookspring(), "Lookspring survives");
        assert!(m.lookstrafe(), "Lookstrafe survives");
        assert_eq!(m.action_for_key(b'j'), Some(BIND_CHANGEWEAPON), "rebinds survive");
        assert_eq!(m.action_for_key(K_SPACE), Some(BIND_JUMP), "seeded binds survive");
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD), "seeded binds survive");
        assert_eq!(m.save_comment(3), "e1m1 quick", "host-set slot comments survive");
        assert!(m.slot_loadable(3));
        assert!(m.game_active, "the Save gate is host state, not navigation");

        // And the menu still opens normally afterwards.
        m.open();
        assert!(m.visible);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn draw_new_menu_screens_without_pics_dont_panic() {
        // Every new screen draws with NO pics and no conchars (worst case), and
        // with conchars only (the text paths) — nothing may panic, and the text
        // screens must put ink on the frame.
        let pal = ramp_palette();
        let pics = MenuPics::default();
        let conchars = test_conchars();
        let mut m = Menu::new();
        m.open();
        for (screen, cursor) in [
            (MenuScreen::Multiplayer, 0),
            (MenuScreen::Load, 3),
            (MenuScreen::Save, 11),
            (MenuScreen::Keys, 5),
            (MenuScreen::Video, 2),
        ] {
            m.screen = screen;
            m.cursor = cursor;
            let mut img = Image::new(320, 200, [9, 9, 9]);
            draw_menu(&mut img, &m, &pics, None, 0.4, 0.0, &pal); // no pics, no font
            let mut img2 = Image::new(320, 200, [9, 9, 9]);
            draw_menu(&mut img2, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
            let inked = img2.rgb.iter().any(|&p| p != [9, 9, 9]);
            assert!(inked, "{screen:?} must draw its text rows with conchars present");
        }
        // A host-set slot comment replaces the UNUSED text without panicking,
        // and the bind-grab prompt variant draws too.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[0] = "a comment longer than the unused-slot text fits fine".into();
        m.set_save_comments(comments);
        m.screen = MenuScreen::Load;
        let mut img = Image::new(320, 200, [9, 9, 9]);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
        m.screen = MenuScreen::Keys;
        m.bind_grab = true;
        let mut img = Image::new(320, 200, [9, 9, 9]);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
    }

    #[test]
    fn quit_confirm_yes_no_flow() {
        let mut m = Menu::new();
        m.open();
        // Raise from Main via select.
        m.cursor = 4;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Quit);
        // No (escape) restores Main.
        assert_eq!(m.quit_no(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // Raise again, Yes closes the menu.
        m.cursor = 4;
        m.select();
        assert_eq!(m.quit_yes(), MenuAction::Closed);
        assert!(!m.visible);

        // The prompt remembers a NON-Main origin (open_quit from Options -> No
        // restores Options).
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.open_quit();
        assert_eq!(m.screen(), MenuScreen::Quit);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "No restores the prompt's origin screen");

        // quit_yes / quit_no are no-ops off the Quit screen.
        assert_eq!(m.quit_yes(), MenuAction::None);
        assert_eq!(m.quit_no(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options, "no-op leaves the screen unchanged");
    }

    #[test]
    fn draw_help_and_quit_screens_without_panic() {
        let pal = ramp_palette();
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let bg = [9u8, 9, 9];

        // Help: a present page pic (index 6) at (0,0) must paint the top-left.
        let mut help: [Option<crate::wad::Qpic>; NUM_HELP_PAGES] = Default::default();
        help[2] = Some(solid_pic(320, 200, 6));
        let pics = MenuPics { help, ..Default::default() };
        let mut m = Menu::new();
        m.open();
        m.cursor = 3;
        m.select(); // -> Help, page 0
        m.help_page = 2; // the page that has art
        let mut img = Image::new(320, 200, bg);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_eq!(img.rgb[0], pal[6], "the help page pic must paint at (0,0)");
        // A missing page (page 0 here is None) draws nothing and never panics.
        m.help_page = 0;
        let mut img0 = Image::new(320, 200, bg);
        draw_menu(&mut img0, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_eq!(img0.rgb[0], bg, "a missing help page leaves the frame untouched");

        // Quit (M_Quit_Draw): M_DrawTextBox (56, 76, 24, 4) from the box_*
        // pics and quit message msgNumber at (64, 84..108) in M_Print's bronze.
        m.open();
        m.cursor = 4;
        m.select(); // -> Quit
        assert_eq!(m.screen(), MenuScreen::Quit);
        m.set_quit_message(4);
        let mut pics = MenuPics::default();
        for (i, slot) in pics.textbox.iter_mut().enumerate() {
            let w = if (3..=6).contains(&i) { 16 } else { 8 };
            *slot = Some(solid_pic(w, 8, 20 + i as u8));
        }
        let mut imgq = Image::new(320, 200, bg);
        draw_menu(&mut imgq, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        let at = |x: usize, y: usize| imgq.rgb[y * 320 + x];
        assert_eq!(at(56, 76), pal[20], "box_tl at (56, 76)");
        assert_eq!(at(56, 84), pal[21], "box_ml below it");
        assert_eq!(at(64, 76), pal[23], "box_tm from x 64");
        let mut bare = Image::new(320, 200, bg);
        draw_menu(&mut bare, &m, &pics, None, 0.0, 0.0, &pal);
        assert_eq!(bare.rgb[84 * 320 + 64], pal[24], "box_mm on the first text row");
        assert_eq!(bare.rgb[92 * 320 + 64], pal[25], "box_mm2 from the second on");
        assert_eq!(bare.rgb[108 * 320 + 64], pal[25], "box_mm2 on the fourth");
        assert_eq!(at(64 + 12 * 16, 76), pal[27], "box_tr after 12 middle pieces");
        assert_eq!(at(56, 116), pal[22], "box_bl under 4 rows");
        // The message over the box: every conchars cell but 0 is lit here,
        // and M_Print draws c + 128 — so the line's first cell paints.
        assert_eq!(at(64, 84), pal[3], "the quit message at (64, 84)");
        // The main menu it rose over is under it, faded once (1 in 4 kept).
        assert_eq!(at(1, 0), pal[0], "faded");
        // Without conchars the box still paints (no panic).
        let mut imgq2 = Image::new(320, 200, bg);
        draw_menu(&mut imgq2, &m, &pics, None, 0.0, 0.0, &pal);
        assert_eq!(imgq2.rgb[76 * 320 + 56], pal[20], "the Quit box paints without conchars");
        // "No" goes back to the menu it rose over.
        assert_eq!(m.quit_no(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
    }
}
