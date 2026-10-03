//! Video — the mode side of vid_win.c (`VID_SetMode`'s runtime resolution,
//! clamped to a safe envelope, the size of `vid.buffer`; the frame itself
//! goes to the page through [`crate::present`]) and the bits of screen.c
//! both client frames share (the `viewsize` cvar, `Draw_TileClear`'s
//! backtile).
//!
//! Two ways to show the picture, as the settings say ([`quake_rs::cvar`]):
//!
//! - **A video mode in a 4:3 box** (`vid_native 0`, Classic): the mode
//!   `_vid_resolution` (Options > Video Options), at most 1280x800, which the
//!   page shows in the largest 4:3 box the window fits, as a 1996 monitor
//!   showed WinQuake's 16:10 modes (so `vid.aspect` folds the stretch in).
//! - **Native** (`vid_native 1`, 2026): the picture fills the page's box for
//!   it (the `Window` record, in device pixels) at the box's own aspect, with
//!   square pixels, [`pixel_size`] device pixels to one of the picture's; the
//!   renderer's `hires` lets it past id's 1280x1024 and Hor+ (`fov_adapt`)
//!   widens the view instead of squashing it.
//!
//! The sky's clouds glide (`r_fluidsky`, [`render::SkyScroll::Fluid`]) or step
//! as id's do, in either, and so do the animated lights (`r_lerplightstyles`,
//! [`LerpLightStyles::Smooth`]).

use quake_rs::client::Vid;
use quake_rs::cvar::Cvars;
use quake_rs::render::{self, FovMode, MipCvars, SkyScroll, TorchFlicker, VideoCvars};
use quake_rs::server::LerpLightStyles;

use crate::app::{ensure_app, App, APP};

/// The default (boot) render resolution. A crisp `960x600` (preset index 4 — must
/// stay a member of [`render::RESOLUTION_PRESETS`] so the Video Options list
/// can mark it current): `_vid_resolution`'s default. The menu + HUD are drawn
/// at their own pixel size, as WinQuake draws them in every mode, unless the
/// scaled-2-D setting blows them up.
pub(crate) const DEFAULT_W: usize = 960;
pub(crate) const DEFAULT_H: usize = 600;
/// Sane bounds for a video mode ([`set_resolution`], `_vid_resolution`): the
/// framebuffer is clamped to this envelope and its total pixel count capped
/// so a runaway value cannot allocate gigabytes. `1280*800*4` bytes ≈ 4 MB is
/// the upper bound.
const MIN_W: i32 = 320;
const MAX_W: i32 = 1280;
const MIN_H: i32 = 200;
const MAX_H: i32 = 800;
const MAX_PIXELS: i32 = 1280 * 800;

/// The most pixels an Auto pixel size ([`pixel_size`]) renders on one
/// thread: a 1080p frame. The renderer costs about 2.8 ns a pixel natively
/// on one core (a little more in the browser), so a frame this size is about
/// 6 ms: 60-144 Hz displays keep up, and a 4K or 5K screen gets 2x2 or 3x3
/// pixels instead of a frame four to seven times the cost.
pub(crate) const AUTO_PIXEL_BUDGET: usize = 1920 * 1080;

/// [`AUTO_PIXEL_BUDGET`] for a renderer drawing on `threads` threads: times
/// the whole square root of their number, as the row bands do not scale
/// perfectly (demo1 at 1080p draws 3.5x as fast on 8 threads as on one):
/// 1-3 threads a 1080p frame, 4-8 two (a 1440p screen at 1x1), 9-15 three.
pub(crate) fn auto_pixel_budget(threads: usize) -> usize {
    AUTO_PIXEL_BUDGET * threads.max(1).isqrt()
}

/// The shorter side, in CSS pixels, of a box [`phone_sized`] takes for a
/// phone's: a phone held either way is 320-480 wide in CSS pixels; a tablet
/// or a laptop is 700 or more.
const PHONE_CSS_SIDE: f32 = 540.0;

/// A phone's screen: dense pixels (`devicePixelRatio` 2 or more) in a small
/// box (its shorter side at most [`PHONE_CSS_SIDE`] CSS pixels). Auto starts
/// such a screen at 2x2 ([`pixel_size`]): a phone's cores are several times
/// slower than a desktop's and slow down further as the phone warms, which
/// the thread count behind [`auto_pixel_budget`] cannot see (an Android
/// phone at 1x1, 2640x1080 on 8 threads, took 13 ms a frame dry and 19 ms
/// underwater against 60 Hz's 16.7, throttled at 40 C; at 2x2, 8 and 9 ms).
/// At 2x2 a game pixel is still under a CSS pixel there, finer than the eye
/// resolves at arm's length.
pub(crate) fn phone_sized((win_w, win_h): (u32, u32), dpr: f32) -> bool {
    dpr >= 2.0 && win_w.min(win_h) as f32 / dpr <= PHONE_CSS_SIDE
}

/// Clamp a requested `(w, h)` render resolution into the supported envelope:
/// width `MIN_W..=MAX_W`, height `MIN_H..=MAX_H`, and the total pixel count capped
/// at [`MAX_PIXELS`] (shrinking the height first if `w*h` would exceed it). Always
/// returns a valid, non-zero size — never panics on absurd input.
pub(crate) fn clamp_resolution(w: i32, h: i32) -> (usize, usize) {
    let mut cw = w.clamp(MIN_W, MAX_W);
    let mut ch = h.clamp(MIN_H, MAX_H);
    // Cap the pixel budget so a wide AND tall request can't blow the cap even
    // though each dimension is individually in range. Trim the height to fit,
    // never below its minimum.
    if cw.saturating_mul(ch) > MAX_PIXELS {
        ch = (MAX_PIXELS / cw.max(1)).clamp(MIN_H, MAX_H);
        // If even MIN_H * cw overflows the cap (it can't with these constants,
        // but stay safe), trim the width too.
        if cw.saturating_mul(ch) > MAX_PIXELS {
            cw = (MAX_PIXELS / ch.max(1)).clamp(MIN_W, MAX_W);
        }
    }
    (cw as usize, ch as usize)
}

/// How many device pixels make one of the picture's in native mode: the
/// setting's 1..=4, or for Auto (0) the smallest that keeps a `win_w x win_h`
/// box's frame within [`auto_pixel_budget`] for the renderer's `threads` (4
/// at most), from 2 on a [`phone_sized`] screen.
pub(crate) fn pixel_size(cvars: &Cvars, (win_w, win_h): (u32, u32), dpr: f32, threads: usize) -> u32 {
    let max = u32::from(quake_rs::cvar::PIXEL_SIZE_MAX);
    let budget = auto_pixel_budget(threads);
    let first = if phone_sized((win_w, win_h), dpr) { 2 } else { 1 };
    match u32::from(cvars.pixel_size) {
        0 => (first..=max).find(|p| (win_w / p) as usize * (win_h / p) as usize <= budget).unwrap_or(max),
        n => n.min(max),
    }
}

/// The picture's size for these settings: native, the page's box divided by
/// the pixel size (at least 320x200, at most the renderer's hires limit);
/// otherwise the video mode, clamped.
pub(crate) fn picture_size(cvars: &Cvars, window: Option<(u32, u32)>, dpr: f32, threads: usize) -> (usize, usize) {
    match window.filter(|_| cvars.native) {
        Some(win) => {
            let p = pixel_size(cvars, win, dpr, threads);
            let (w, h) = ((win.0 / p) as usize, (win.1 / p) as usize);
            let (mw, mh) = (render::HIRES_MAXWIDTH, render::HIRES_MAXHEIGHT);
            (w.clamp(MIN_W as usize, mw), h.clamp(MIN_H as usize, mh))
        }
        None => {
            let (w, h) = cvars.vid_resolution;
            clamp_resolution(i32::from(w), i32::from(h))
        }
    }
}

/// The most threads Auto (`r_threads 0`) draws on for a [`phone_sized`]
/// screen. A phone's cores are of several kinds, and the slower ones do not
/// help a frame that waits for its last band: on an Android phone (one fast
/// core, four middle, three small; its browser offers 8) four threads drew
/// every measured frame as fast as eight, or faster — `timedemo demo1` 367
/// fps against 323 at 1320x540 and 155 against 148 at 2640x1080, cool; in
/// play at 2640x1080 once warm the same median frame with fewer late ones
/// (p95 16.9 ms against 20.3, 58 frames a second shown against 56) — and six
/// were no better than four (web/PLATFORM.md, "On an Android phone"). `r_threads
/// N` still draws on N.
pub(crate) const PHONE_AUTO_THREADS: usize = 4;

/// The threads the renderer draws with: `r_threads` against what the host
/// offers — of which Auto takes at most [`PHONE_AUTO_THREADS`] on a phone's
/// screen.
pub(crate) fn render_threads(a: &App) -> usize {
    let phone = a.window.is_some_and(|win| phone_sized(win, a.dpr));
    let offered = if phone { a.hw_threads.min(PHONE_AUTO_THREADS) } else { a.hw_threads };
    a.settings.cvars.threads.resolve(offered)
}

/// Whether the picture is shown native (the page fills its box, square
/// pixels) rather than as a mode in a 4:3 box.
pub(crate) fn native(a: &App) -> bool {
    a.settings.cvars.native && a.window.is_some()
}

/// Point Video Options at the live picture (`Menu::sync_resolution`): the
/// render size, [`native`] (not just `vid_native`'s cvar — a window must be
/// known too, or there's nothing to fill natively), and whether the
/// native-resolution rows belong in the list at all (the 2026 profile;
/// Classic's list is `RESOLUTION_PRESETS` alone). Every caller that used to
/// hand `Menu::sync_resolution` the render size alone goes through this now,
/// so the two new facts can never drift out of sync with it.
pub(crate) fn sync_menu_resolution(a: &mut App) {
    let native = native(a);
    let modern = a.settings.profile == quake_rs::settings::Profile::Modern;
    let (w, h) = (a.render_w as i32, a.render_h as i32);
    a.menu.sync_resolution(w, h, native, modern);
}

/// Once a frame, before the client frame: the framebuffer to the size the
/// settings ask for, and the 2-D layer's scale (a setting `draw` still keeps
/// per thread). The renderer's settings go with the frame, in [`vid`].
pub(crate) fn apply_settings(a: &mut App) {
    let (w, h) = picture_size(&a.settings.cvars, a.window, a.dpr, render_threads(a));
    a.set_render_size(w, h);
    quake_rs::draw::set_scaled_2d(a.settings.cvars.scaled_2d);
    sync_menu_resolution(a);
}

/// The checks' and the benchmark's shorthand for the picture (the
/// `set_video` call): `modern` is the 2026 profile's — native resolution at
/// one device pixel a pixel, Hor+, the fluid sky, the gliding light styles,
/// the flickering torches, exact perspective:
/// `quaketool --video modern` — whose size then follows the window
/// (`set_window`); `classic` a video mode in the 4:3 box with id's field of
/// view, sky and light styles. Returns 1 for a known name.
pub(crate) fn set_video(name: &str) -> i32 {
    let modern = match name.trim() {
        "classic" => false,
        "modern" => true,
        _ => return 0,
    };
    ensure_app(|a| {
        let c = &mut a.settings.cvars;
        (c.native, c.fov_adapt) = (modern, modern);
        c.persp_span = if modern { render::PerspSpan::Exact } else { render::PerspSpan::Spans16 };
        c.sky = if modern { SkyScroll::Fluid } else { SkyScroll::Classic };
        c.lightstyles = if modern { LerpLightStyles::Smooth } else { LerpLightStyles::Classic };
        c.torches = if modern { TorchFlicker::MODERN } else { TorchFlicker::OFF };
        if modern {
            c.pixel_size = 1;
        }
    });
    1
}

/// The current render width in pixels. Each `Frame` record carries it, and
/// the page resizes its canvas backing store and its presenter when it changes.
pub(crate) fn width() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_w as i32).unwrap_or(DEFAULT_W as i32))
}
/// The current render height in pixels (defaults to [`DEFAULT_H`] = 600).
pub(crate) fn height() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_h as i32).unwrap_or(DEFAULT_H as i32))
}

/// Set a video mode, as Enter on a Video Options line does: `_vid_resolution`
/// (clamped to the supported envelope, never a panic), shown in the 4:3 box —
/// native resolution off — and drawn at from the next frame on, so
/// `width()`/`height()` report the new size at once.
pub(crate) fn set_resolution(w: i32, h: i32) {
    let (cw, ch) = clamp_resolution(w, h);
    ensure_app(|a| {
        a.settings.cvars.vid_resolution = (cw as u16, ch as u16);
        a.settings.cvars.native = false;
        a.set_render_size(cw, ch);
        // Keep the Video Options "current mode" pointing at the new size too.
        sync_menu_resolution(a);
    });
}

/// The page's box for the picture, in device pixels (the `Window` record);
/// native mode renders into it from the next frame.
/// The `Window` record: the page's box in device pixels and its
/// `devicePixelRatio` (an older page sends none, read as 0: taken as 1).
pub(crate) fn set_window(w: u32, h: u32, dpr: f32) {
    ensure_app(|a| {
        a.window = (w > 0 && h > 0).then_some((w, h));
        a.dpr = if dpr.is_finite() && dpr >= 1.0 { dpr } else { 1.0 };
    });
}

/// The width:height ratio the page shows a video mode at, whatever its
/// backing store: the largest 4:3 box the window fits, as DOS and Windows
/// Quake's modes filled a 4:3 monitor. Every mode is 16:10, so its pixels are
/// shown 1.2x taller than wide. (Native, the pixels are square.)
pub(crate) const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

/// The screen the client frames draw ([`Vid`]): the picture's size, the
/// aspect it is shown at (a mode's 4:3 box, or square pixels native; with the
/// size it gives `vid.aspect`), and the perspective span.
pub(crate) fn vid(a: &App) -> Vid {
    let (w, h) = (a.render_w, a.render_h);
    let c = &a.settings.cvars;
    let native = native(a);
    let display_aspect = if native && h > 0 { w as f64 / h as f64 } else { DISPLAY_ASPECT };
    let fov_mode = if c.fov_adapt { FovMode::HorPlus } else { FovMode::Classic };
    Vid {
        width: w,
        height: h,
        display_aspect,
        persp_span: c.persp_span,
        video: VideoCvars { fov_mode, hires: native, sky: c.sky, lightstyles: c.lightstyles, torches: c.torches },
        mip: MipCvars { mipscale: c.d_mipscale, mipcap: c.d_mipcap },
    }
}

/// The [`Vid`] of a `w x h` video mode in the 4:3 box, id's spans: what a
/// Classic frame draws.
#[cfg(test)]
pub(crate) fn mode_vid(w: usize, h: usize) -> Vid {
    Vid {
        width: w,
        height: h,
        display_aspect: DISPLAY_ASPECT,
        persp_span: render::PerspSpan::Spans16,
        video: VideoCvars::CLASSIC,
        mip: MipCvars::DEFAULT,
    }
}

/// The `viewsize` cvar (Options "Screen size", `sizeup`/`sizedown`), 30..=120.
/// Read-only, for the page/verification harness.
pub(crate) fn viewsize() -> f32 {
    APP.with(|c| {
        c.borrow().as_ref().map(|a| a.settings.cvars.viewsize).unwrap_or(render::VIEWSIZE_DEFAULT)
    })
}

/// Set the `viewsize` cvar, bounded to 30..=120 (the console's `viewsize n`).
pub(crate) fn set_viewsize(v: f32) {
    ensure_app(|a| a.settings.cvars.set_viewsize(v));
}

/// The scaled-2-D setting (`wasm_scaled2d`): `1` draws the status bar,
/// menus, console and text on id's 320x200 screen at the largest whole
/// multiple that fits, `0` at their own pixel size as WinQuake does in every
/// mode. Takes effect on the next frame.
pub(crate) fn set_scaled_2d(on: i32) {
    ensure_app(|a| a.settings.cvars.scaled_2d = on != 0);
    quake_rs::draw::set_scaled_2d(on != 0);
}

/// `1` while the scaled-2-D setting is on ([`set_scaled_2d`]).
pub(crate) fn scaled_2d() -> i32 {
    APP.with(|c| c.borrow().as_ref().map_or(0, |a| a.settings.cvars.scaled_2d as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::boot;
    use crate::console::console_toggle;
    use crate::host::step;
    use crate::menu::{menu_cancel, menu_down, menu_left, menu_right, menu_select, menu_visible};
    use crate::test_util::*;
    use quake_rs::render::PerspSpan;

    /// Auto picks the smallest whole pixel that keeps the frame within a
    /// 1080p frame's pixels per whole square root of the renderer's threads;
    /// a fixed size is what it says; Classic ignores the window.
    #[test]
    fn native_pictures_are_whole_fractions_of_the_window() {
        let mut c = quake_rs::cvar::Cvars::modern();
        let size = |c: &Cvars, win, threads| (picture_size(c, Some(win), 1.0, threads), pixel_size(c, win, 1.0, threads));
        assert_eq!(size(&c, (1920, 1080), 1), ((1920, 1080), 1));
        assert_eq!(size(&c, (2560, 1440), 1), ((1280, 720), 2), "1440p on one thread: 2x2");
        assert_eq!(size(&c, (2560, 1440), 8), ((2560, 1440), 1), "and 1x1 on eight");
        assert_eq!(size(&c, (3840, 2160), 8), ((1920, 1080), 2));
        assert_eq!(size(&c, (7680, 4320), 1), ((1920, 1080), 4), "8K: at most 4x4");
        assert_eq!(size(&c, (1000, 640), 1), ((1000, 640), 1), "any aspect");
        assert_eq!(size(&c, (300, 150), 1), ((320, 200), 1), "at least 320x200");
        c.pixel_size = 3;
        assert_eq!(size(&c, (1920, 1080), 1), ((640, 360), 3));
        c.native = false;
        assert_eq!(picture_size(&c, Some((1920, 1080)), 1.0, 1), (960, 600), "the mode");
        assert_eq!((auto_pixel_budget(0), auto_pixel_budget(3), auto_pixel_budget(4)), (AUTO_PIXEL_BUDGET, AUTO_PIXEL_BUDGET, 2 * AUTO_PIXEL_BUDGET));
    }

    /// The renderer's sky follows `r_fluidsky`: off in Classic, on in 2026,
    /// and `set_video`'s `modern` is `quaketool --video modern`'s, the fluid
    /// sky with the rest.
    #[test]
    fn the_sky_follows_r_fluidsky() {
        let sky = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.sky);
        assert_eq!(boot(), 1);
        assert_eq!(sky(), SkyScroll::Classic, "the tests start in Classic");
        use_2026();
        assert_eq!(sky(), SkyScroll::Fluid);
        crate::host_cmd::execute_console_command("r_fluidsky 0");
        assert_eq!(sky(), SkyScroll::Classic);
        assert_eq!(set_video("modern"), 1);
        assert_eq!(sky(), SkyScroll::Fluid);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(sky(), SkyScroll::Classic);
    }

    /// The renderer's perspective follows `r_perspspan`: id's 16-pixel spans
    /// in Classic, exact at every pixel in 2026 (the user's call: at 1080p
    /// and above the spans' affine steps show), 8 or 4 when set; the old
    /// `wasm_exactpersp` sets its two ends; and `set_video`'s `modern` is
    /// `quaketool --video modern`'s, exact perspective with the rest.
    #[test]
    fn the_perspective_follows_r_perspspan() {
        let span = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).persp_span);
        assert_eq!(boot(), 1);
        assert_eq!(span(), PerspSpan::Spans16, "the tests start in Classic: id's spans");
        use_2026();
        assert_eq!(span(), PerspSpan::Exact, "2026: exact at every pixel");
        for (line, want) in [("r_perspspan 8", PerspSpan::Spans8), ("r_perspspan 4", PerspSpan::Spans4),
                             ("wasm_exactpersp 0", PerspSpan::Spans16), ("wasm_exactpersp 1", PerspSpan::Exact),
                             ("r_perspspan 16", PerspSpan::Spans16)] {
            crate::host_cmd::execute_console_command(line);
            assert_eq!(span(), want, "{line}");
        }
        assert_eq!(set_video("modern"), 1);
        assert_eq!(span(), PerspSpan::Exact);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(span(), PerspSpan::Spans16);
    }

    /// The light styles the client animates follow `r_lerplightstyles` the
    /// same way: off in Classic, on in 2026, and in `set_video`'s `modern`.
    #[test]
    fn the_light_styles_follow_r_lerplightstyles() {
        let lerp = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.lightstyles);
        assert_eq!(boot(), 1);
        assert_eq!(lerp(), LerpLightStyles::Classic, "the tests start in Classic");
        use_2026();
        assert_eq!(lerp(), LerpLightStyles::Smooth);
        crate::host_cmd::execute_console_command("r_lerplightstyles 0");
        assert_eq!(lerp(), LerpLightStyles::Classic);
        assert_eq!(set_video("modern"), 1);
        assert_eq!(lerp(), LerpLightStyles::Smooth);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(lerp(), LerpLightStyles::Classic);
    }

    /// The steady torches' flicker follows `r_torchflicker`, a strength: off
    /// in Classic, on in 2026 and `set_video`'s `modern`, any value between 0
    /// and 2 from the console (the user tunes it by eye), read back as set.
    #[test]
    fn the_torches_follow_r_torchflicker() {
        let torches = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.torches);
        assert_eq!(boot(), 1);
        assert_eq!(torches(), TorchFlicker::OFF, "the tests start in Classic");
        use_2026();
        assert_eq!(torches(), TorchFlicker::MODERN);
        crate::host_cmd::execute_console_command("r_torchflicker 0.35");
        assert_eq!(torches().value(), 0.35);
        let cvar = quake_rs::cvar::find("r_torchflicker").expect("the cvar");
        assert_eq!(APP.with(|c| cvar.get(&c.borrow().as_ref().unwrap().settings.cvars)), "0.35");
        crate::host_cmd::execute_console_command("r_torchflicker 7");
        assert_eq!(torches().value(), 2.0, "at most 2");
        crate::host_cmd::execute_console_command("r_torchflicker 0");
        assert!(torches().is_off());
        assert_eq!(set_video("modern"), 1);
        assert_eq!(torches(), TorchFlicker::MODERN);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(torches(), TorchFlicker::OFF);
    }

    /// A phone's small, dense screen starts Auto at 2x2 whatever its threads;
    /// a tablet, a laptop's dense screen and a desktop keep the budget's
    /// answer; a fixed pixel size is what it says everywhere.
    #[test]
    fn auto_starts_a_phone_at_two_pixels() {
        let mut c = quake_rs::cvar::Cvars::modern();
        // A phone-sized landscape viewport: 880x360 CSS at 3x, 8 threads.
        assert!(phone_sized((2640, 1080), 3.0));
        assert_eq!(pixel_size(&c, (2640, 1080), 3.0, 8), 2);
        assert_eq!(picture_size(&c, Some((2640, 1080)), 3.0, 8), (1320, 540));
        assert!(phone_sized((1080, 2160), 3.0), "and held upright");
        // Not phones: an iPad (1024x768 CSS at 2x), a MacBook (1440x900 at 2x),
        // a desktop at 1x, and a phone-sized box at 1x (a small desktop window).
        assert!(!phone_sized((2048, 1536), 2.0));
        assert!(!phone_sized((2880, 1800), 2.0));
        assert!(!phone_sized((1920, 1080), 1.0));
        assert!(!phone_sized((880, 360), 1.0));
        assert_eq!(pixel_size(&c, (1920, 1080), 1.0, 8), 1);
        c.pixel_size = 1;
        assert_eq!(pixel_size(&c, (2640, 1080), 3.0, 8), 1, "1x1 when asked");
        // An older page sends no ratio (0): a desktop's 1.
        set_window(2640, 1080, 0.0);
        assert_eq!(APP.with(|a| a.borrow().as_ref().map(|a| a.dpr)), Some(1.0));
    }

    /// Auto draws a phone's screen on four threads at most, any other screen
    /// on every thread the host offers; a count asked for is that count
    /// anywhere.
    #[test]
    fn auto_draws_a_phone_on_four_threads() {
        assert_eq!(boot(), 1);
        let threads = |hw: usize| {
            APP.with(|c| {
                let mut b = c.borrow_mut();
                let a = b.as_mut().unwrap();
                a.hw_threads = hw;
                render_threads(a)
            })
        };
        // A phone-sized landscape viewport, fullscreen: 880x360 CSS at 3x.
        set_window(2640, 1080, 3.0);
        assert_eq!((threads(8), threads(6), threads(2), threads(1)), (PHONE_AUTO_THREADS, 4, 2, 1));
        crate::host_cmd::execute_console_command("r_threads 8");
        assert_eq!(threads(8), 8, "asked for by number: that many");
        crate::host_cmd::execute_console_command("r_threads 0");
        set_window(1920, 1080, 1.0);
        assert_eq!(threads(8), 8, "a desktop: every thread offered");
        set_window(2048, 1536, 2.0);
        assert_eq!(threads(16), 16, "a tablet too");
        APP.with(|c| c.borrow_mut().as_mut().unwrap().window = None);
        assert_eq!(threads(6), 6, "no window known: every thread offered");
    }

    // -- dynamic render resolution (set_resolution + clamp + reallocation) ----

    #[test]
    fn set_viewsize_restores_the_cvar_bounded_like_the_console() {
        // The page's restore of the saved viewsize (config.cfg's archived
        // cvar): bounded to 30..120 like `viewsize n`, survives a re-boot.
        assert_eq!(viewsize(), render::VIEWSIZE_DEFAULT);
        set_viewsize(90.0);
        assert_eq!(viewsize(), 90.0);
        set_viewsize(500.0);
        assert_eq!(viewsize(), 120.0);
        set_viewsize(f32::NAN);
        assert_eq!(viewsize(), 30.0, "a non-number reads as 0 (atof), the minimum");
        set_viewsize(110.0);
        assert_eq!(boot(), 1);
        assert_eq!(viewsize(), 110.0, "boot keeps it");
    }

    #[test]
    fn clamp_resolution_clamps_into_envelope() {
        // In-range values pass through unchanged.
        assert_eq!(clamp_resolution(640, 400), (640, 400));
        assert_eq!(clamp_resolution(DEFAULT_W as i32, DEFAULT_H as i32), (DEFAULT_W, DEFAULT_H));
        // Below the minimum clamps up; above the maximum clamps down.
        assert_eq!(clamp_resolution(0, 0), (MIN_W as usize, MIN_H as usize));
        assert_eq!(clamp_resolution(-100, -100), (320, 200));
        assert_eq!(clamp_resolution(99999, 99999).0, MAX_W as usize);
        // The pixel-budget cap: a max-width AND max-height request is trimmed so
        // w*h never exceeds MAX_PIXELS, never panicking.
        let (cw, ch) = clamp_resolution(MAX_W, MAX_H);
        assert!(
            (cw as i32).saturating_mul(ch as i32) <= MAX_PIXELS,
            "clamped {cw}x{ch} must respect the pixel cap"
        );
        assert!(cw >= MIN_W as usize && ch >= MIN_H as usize, "still a valid non-zero size");
        // i32::MAX in both dims must not overflow or panic.
        let (mw, mh) = clamp_resolution(i32::MAX, i32::MAX);
        assert!(mw <= MAX_W as usize && mh <= MAX_H as usize);
        assert!((mw as i32).saturating_mul(mh as i32) <= MAX_PIXELS);
    }

    #[test]
    fn set_resolution_reallocates_and_reports_new_size() {
        // Default boot size.
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);

        // A valid in-range resolution is applied verbatim; width()/height()
        // follow, and the next frame renders at it.
        set_resolution(640, 400);
        assert_eq!(width(), 640);
        assert_eq!(height(), 400);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().expect("app exists after set_resolution");
            assert_eq!(a.render_w, 640);
            assert_eq!(a.render_h, 400);
        });

        // Out-of-range input is clamped, not panicked: a huge request lands within
        // the envelope, and the render size is the clamped one.
        set_resolution(100000, 100000);
        let (w, h) = (width(), height());
        assert!((MIN_W..=MAX_W).contains(&w) && (MIN_H..=MAX_H).contains(&h));
        assert!(w.saturating_mul(h) <= MAX_PIXELS);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!((a.render_w, a.render_h), (w as usize, h as usize));
        });

        // Back to the boot default.
        set_resolution(DEFAULT_W as i32, DEFAULT_H as i32);
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
    }

    #[test]
    fn boot_then_set_resolution_renders_larger_framebuffer() {
        // Boot the real walk (embedded pak). If the pak is unavailable in this
        // build the test would fail to boot; the workspace embeds a real PAK0.PAK.
        assert_eq!(boot(), 1, "boot the embedded e1m1 walk");
        // Boot keeps the boot default resolution.
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.present.rgba().len(), DEFAULT_W * DEFAULT_H * 4, "default fb is DEFAULT_W*DEFAULT_H*4");
        });

        // Pick the largest preset (1280x800, > the 960x600 default), then render:
        // the framebuffer is now 1280*800*4 and the scene rendered into all of it
        // (the fb is fully written by step).
        set_resolution(1280, 800);
        assert_eq!(width(), 1280);
        assert_eq!(height(), 800);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.present.rgba().len(), 1280 * 800 * 4, "step renders into the 1280*800 framebuffer");
            // Every alpha byte is 255 (step pushes opaque RGBA), proving the whole
            // larger buffer was painted, not just the smaller default region.
            assert!(a.present.rgba().chunks_exact(4).all(|px| px[3] == 255), "full fb painted opaque");
        });
    }

    #[test]
    fn screen_size_is_viewsize_and_video_options_sets_the_mode() {
        // WinQuake: Options "Screen size" is scr_viewsize; the video mode lives
        // in Options > Video Options (M_Video -> VID_MenuDraw/VID_MenuKey). The
        // port used to cycle render resolutions on the Screen size row.
        assert_eq!(boot(), 1);
        assert_eq!(menu_visible(), 1, "boot enters the menu");
        assert_eq!((width(), height()), (DEFAULT_W as i32, DEFAULT_H as i32));
        menu_down(); // -> 1 (Multiplayer)
        menu_down(); // -> 2 (Options)
        menu_select(); // enter Options (cursor on row 0 = Customize controls)
        menu_down(); // -> 1 (Go to console)
        menu_down(); // -> 2 (Reset to defaults)
        menu_down(); // -> 3 (Screen size)
        assert_eq!(viewsize(), 100.0, "viewsize defaults to 100");
        menu_right();
        assert_eq!(viewsize(), 110.0, "right steps viewsize +10");
        menu_left();
        menu_left();
        assert_eq!(viewsize(), 90.0, "left steps viewsize -10");
        menu_select(); // Enter = M_AdjustSliders(1)
        assert_eq!(viewsize(), 100.0);
        assert_eq!(
            (width(), height()),
            (DEFAULT_W as i32, DEFAULT_H as i32),
            "Screen size never resizes the framebuffer"
        );
        // Video Options (row 12): the cursor opens on the current mode (960x600,
        // preset 4); left/right/up/down move the line only, Enter applies it.
        for _ in 0..9 {
            menu_down();
        }
        menu_select();
        assert_eq!(menu_screen(), render::MenuScreen::Video);
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().menu.cursor(), 4));
        menu_down(); // -> 1120x700
        menu_right(); // -> 1280x800 (VID_MenuKey moves the line)
        menu_left(); // -> back to 1120x700
        assert_eq!((width(), height()), (DEFAULT_W as i32, DEFAULT_H as i32), "moving the line alone");
        menu_select();
        assert_eq!((width(), height()), (1120, 700), "Enter sets the highlighted mode");
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!((a.render_w, a.render_h), (1120, 700), "frames render at the new mode");
            assert_eq!(a.menu.resolution(), (1120, 700), "the list marks it current");
        });
        assert_eq!(viewsize(), 100.0, "the mode never touches viewsize");
        menu_cancel();
        assert_eq!(menu_screen(), render::MenuScreen::Options, "Esc returns to Options");
        set_resolution(DEFAULT_W as i32, DEFAULT_H as i32);
    }

    /// The finished RGBA framebuffer as RGB rows (gamma 1.0 = identity).
    fn fb_rgb() -> (usize, usize, Vec<[u8; 3]>) {
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let px = a.present.rgba().chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
            (a.render_w, a.render_h, px)
        })
    }

    /// Set viewsize through the console (the `viewsize <n>` cvar command) and
    /// render one frozen frame (dt = 0: the world, bob and animations hold).
    fn frame_at_viewsize(vs: u32) -> Vec<[u8; 3]> {
        console_toggle();
        run_console_line(&format!("viewsize {vs}"));
        console_toggle();
        assert_eq!(viewsize(), vs as f32);
        step(0.0);
        fb_rgb().2
    }

    #[test]
    fn viewsize_frames_the_view_above_the_status_bar_like_the_c() {
        // SCR_CalcRefdef/R_SetVrect end to end on the real e1m1 at 320x200.
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        step(0.05);
        step(0.05);
        let (w, h, _) = fb_rgb();
        assert_eq!((w, h), (320, 200));
        let f120 = frame_at_viewsize(120);
        let f110 = frame_at_viewsize(110);
        let f100 = frame_at_viewsize(100);
        let f50 = frame_at_viewsize(50);
        let row = |f: &Vec<[u8; 3]>, y: usize| f[y * 320..(y + 1) * 320].to_vec();
        // The projection is centred on the view rectangle with the same
        // xscale, so the 100 view (320x152, centre 76) is the full-screen 120
        // view (centre 100) shifted up 24 rows, and the 110 view (320x176,
        // centre 88) is it shifted up 12 — i.e. id's framing: the view ends at
        // the status bar instead of running under it. (Allow a few edge pixels
        // for float rounding in the rasteriser. The rows the gun can reach are
        // left out: V_CalcRefdef's viewsize fudge raises it 2 units at 100 and
        // 1 at 110 over 120, so it does NOT simply shift.)
        let matching = |f: &Vec<[u8; 3]>, rows: usize, shift: usize| {
            (0..rows)
                .flat_map(|y| (0..320).map(move |x| (x, y)))
                .filter(|&(x, y)| f[y * 320 + x] == f120[(y + shift) * 320 + x])
                .count() as f64
                / (rows * 320) as f64
        };
        let m100 = matching(&f100, 110, 24);
        let m110 = matching(&f110, 122, 12);
        assert!(m100 > 0.99, "viewsize 100 = the 120 view shifted up 24 rows ({m100:.4})");
        assert!(m110 > 0.99, "viewsize 110 = the 120 view shifted up 12 rows ({m110:.4})");
        // ...and NOT the 120 view unshifted (the old full-screen framing).
        assert!(matching(&f100, 152, 0) < 0.9, "the horizon moved");
        // sb_lines: 110 keeps the 24-row status strip of 100 and drops its
        // inventory strip; 120 has no status bar (its bottom rows are world).
        for y in 176..200 {
            assert_eq!(row(&f110, y), row(&f100, y), "row {y}: the same status strip");
        }
        assert_ne!(row(&f110, 160), row(&f100, 160), "no inventory strip at 110");
        // viewsize 50: a 160x100 view at (80, 26) inside the backtile border,
        // the full 48-line status bar below.
        let backtile = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let wk = a.walk.as_ref().unwrap();
            let t = wk.gfx_wad.as_ref().unwrap().qpic("backtile").expect("backtile in gfx.wad");
            t.data.iter().map(|&i| wk.palette[i as usize]).collect::<Vec<_>>()
        });
        for y in 0..152 {
            for x in 0..320 {
                let inside = (80..240).contains(&x) && (26..126).contains(&y);
                if !inside {
                    assert_eq!(f50[y * 320 + x], backtile[(y % 64) * 64 + x % 64], "border ({x},{y})");
                }
            }
        }
        let interior_tiles = (26..126)
            .flat_map(|y| (80..240).map(move |x| (x, y)))
            .filter(|&(x, y)| f50[y * 320 + x] == backtile[(y % 64) * 64 + x % 64])
            .count();
        // (Brown walls match brown tile texels now and then: ~10% here.)
        assert!(interior_tiles < 160 * 100 / 2, "the view, not the tile, fills the rectangle ({interior_tiles})");
        for y in 152..200 {
            assert_eq!(row(&f50, y), row(&f100, y), "row {y}: the same full status bar");
        }
        set_resolution(DEFAULT_W as i32, DEFAULT_H as i32);
    }

    #[test]
    fn chosen_resolution_persists_across_reboot() {
        // The reported bug: pick a resolution in Options, start the game, and it
        // snapped back to the default. The chosen size must now carry across a
        // re-boot (the 🚶 walk button / New Game), not reset to DEFAULT.
        assert_eq!(boot(), 1);
        // The engine boots at DEFAULT (960x600); pick a different, smaller preset.
        set_resolution(640, 400);
        assert_eq!((width(), height()), (640, 400), "menu/host set the resolution");

        // Re-boot the walk: the resolution MUST be preserved, not reset to DEFAULT.
        assert_eq!(boot(), 1);
        assert_eq!(
            (width(), height()),
            (640, 400),
            "re-boot preserves the chosen resolution (was the reported bug)"
        );
        // ...and the fresh menu's current video mode tracks the live framebuffer,
        // so opening Video Options marks the real mode (no label/fb desync).
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.render_w, 640);
            assert_eq!(a.render_h, 400);
            assert_eq!(
                a.menu.resolution(),
                (640, 400),
                "the Video Options current mode follows the preserved framebuffer"
            );
        });
    }
}
